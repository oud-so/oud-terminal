//! Live HLS as a continuous byte stream.
//!
//! Many broadcasters only publish HTTP Live Streaming: a playlist that is
//! rewritten every few seconds, pointing at short audio segments. The decoder
//! wants one unbroken stream, so [`HlsReader`] follows the playlist, fetches
//! each new segment in order and serves their audio back to back. Segments
//! come either as packed audio (ADTS AAC / MP3, often with an ID3 header) or
//! as MPEG transport streams, whose audio we lift out with [`ts::Demuxer`].

use std::collections::VecDeque;
use std::io::{self, Read};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::Url;
use reqwest::blocking::Client;

/// Segments to start behind the live edge, so playback has a cushion.
const START_BEHIND: usize = 3;

pub struct HlsReader<C> {
    client: Client,
    playlist: Url,
    /// Sequence number of the next segment we have not queued yet.
    next_seq: Option<u64>,
    queue: VecDeque<Url>,
    ended: bool,
    refresh_every: Duration,
    refreshed_at: Option<Instant>,
    chunk: io::Cursor<Vec<u8>>,
    demuxer: ts::Demuxer,
    is_current: C,
}

/// What a variant playlist says about its codec, for picking a decoder.
pub struct Opened<C> {
    pub reader: HlsReader<C>,
    pub extension: &'static str,
}

impl<C: Fn() -> bool> HlsReader<C> {
    /// Resolve a master playlist (if given one) and prime the segment queue.
    pub fn open(client: Client, url: &str, is_current: C) -> Result<Opened<C>> {
        let mut playlist = Url::parse(url).context("bad playlist url")?;
        let mut text = fetch_text(&client, &playlist)?;
        let mut extension = "aac";

        if text.contains("#EXT-X-STREAM-INF") {
            let variant = pick_variant(&text).context("playlist has no playable variant")?;
            if variant.codecs.contains("mp4a.40.34") || variant.codecs.contains("mp3") {
                extension = "mp3";
            }
            playlist = playlist.join(&variant.uri).context("bad variant url")?;
            text = fetch_text(&client, &playlist)?;
        }

        let mut reader = Self {
            client,
            playlist,
            next_seq: None,
            queue: VecDeque::new(),
            ended: false,
            refresh_every: Duration::from_secs(2),
            refreshed_at: Some(Instant::now()),
            chunk: io::Cursor::new(Vec::new()),
            demuxer: ts::Demuxer::default(),
            is_current,
        };
        reader.absorb(&text)?;
        if reader.queue.is_empty() && reader.ended {
            bail!("empty playlist");
        }
        if let Some(first) = reader.queue.front()
            && first.path().ends_with(".mp3")
        {
            extension = "mp3";
        }
        Ok(Opened { reader, extension })
    }

    /// Merge a freshly fetched media playlist into the queue.
    fn absorb(&mut self, text: &str) -> Result<()> {
        let media = parse_media(text);
        if media.segments.is_empty() && !media.ended {
            bail!("not a media playlist");
        }
        self.ended = media.ended;
        if let Some(target) = media.target_duration {
            // Poll at half the segment length: new segments show up promptly
            // without hammering the server.
            self.refresh_every = Duration::from_secs_f32((target / 2.0).clamp(1.0, 10.0));
        }

        let first_seq = media.sequence;
        let skip = match self.next_seq {
            None => media.segments.len().saturating_sub(START_BEHIND),
            Some(next) if next < first_seq => 0, // fell behind the window; resume at its start
            Some(next) => (next - first_seq) as usize,
        };
        for uri in media.segments.iter().skip(skip) {
            self.queue.push_back(self.playlist.join(uri).context("bad segment url")?);
        }
        self.next_seq = Some(first_seq + media.segments.len() as u64);
        Ok(())
    }

    fn refresh(&mut self) -> io::Result<()> {
        if let Some(at) = self.refreshed_at {
            let due = at + self.refresh_every;
            while Instant::now() < due {
                if !(self.is_current)() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        self.refreshed_at = Some(Instant::now());
        let text = fetch_text(&self.client, &self.playlist).map_err(io::Error::other)?;
        self.absorb(&text).map_err(io::Error::other)
    }

    fn load_next_segment(&mut self) -> io::Result<bool> {
        while self.queue.is_empty() {
            if self.ended || !(self.is_current)() {
                return Ok(false);
            }
            self.refresh()?;
        }
        let url = self.queue.pop_front().expect("queue is non-empty");
        let bytes = self
            .client
            .get(url.clone())
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .map_err(io::Error::other)?;

        let audio = if ts::looks_like_ts(&bytes) {
            self.demuxer.audio_from(&bytes)
        } else {
            strip_id3(&bytes).to_vec()
        };
        self.chunk = io::Cursor::new(audio);
        Ok(true)
    }
}

impl<C: Fn() -> bool> Read for HlsReader<C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.chunk.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            if !self.load_next_segment()? {
                return Ok(0);
            }
        }
    }
}

fn fetch_text(client: &Client, url: &Url) -> Result<String> {
    let res = client.get(url.clone()).send().context("connect")?;
    if !res.status().is_success() {
        bail!("http {}", res.status().as_u16());
    }
    res.text().context("reading playlist")
}

struct Variant {
    uri: String,
    bandwidth: u64,
    codecs: String,
}

/// Choose the richest variant our decoder handles. HE-AAC (`mp4a.40.5`,
/// `mp4a.40.29`) is avoided when plain AAC-LC is on offer.
fn pick_variant(master: &str) -> Option<Variant> {
    let mut variants = Vec::new();
    let mut lines = master.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") else { continue };
        let Some(uri) = lines.by_ref().find(|l| !l.is_empty() && !l.starts_with('#')) else { break };
        variants.push(Variant {
            uri: uri.to_string(),
            bandwidth: attribute(attrs, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0),
            codecs: attribute(attrs, "CODECS").unwrap_or_default().to_ascii_lowercase(),
        });
    }
    let he_aac = |v: &Variant| v.codecs.contains("mp4a.40.5") || v.codecs.contains("mp4a.40.29");
    let lc_exists = variants.iter().any(|v| !he_aac(v));
    variants.into_iter().filter(|v| !(lc_exists && he_aac(v))).max_by_key(|v| v.bandwidth)
}

/// Read `KEY=value` or `KEY="value"` out of an attribute list.
fn attribute(attrs: &str, key: &str) -> Option<String> {
    let start = attrs.find(&format!("{key}="))? + key.len() + 1;
    let rest = &attrs[start..];
    let value = match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"')?],
        None => rest.split(',').next()?,
    };
    Some(value.to_string())
}

struct MediaPlaylist {
    sequence: u64,
    target_duration: Option<f32>,
    segments: Vec<String>,
    ended: bool,
}

fn parse_media(text: &str) -> MediaPlaylist {
    let mut playlist =
        MediaPlaylist { sequence: 0, target_duration: None, segments: Vec::new(), ended: false };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            playlist.sequence = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            playlist.target_duration = v.trim().parse().ok();
        } else if line.starts_with("#EXT-X-ENDLIST") {
            playlist.ended = true;
        } else if !line.starts_with('#') {
            playlist.segments.push(line.to_string());
        }
    }
    playlist
}

/// Packed-audio segments open with an ID3v2 tag carrying a timestamp.
/// Mid-stream, the decoder would read it as garbage, so drop it.
fn strip_id3(bytes: &[u8]) -> &[u8] {
    let mut rest = bytes;
    while rest.len() >= 10 && &rest[..3] == b"ID3" {
        let size = rest[6..10].iter().fold(0usize, |acc, b| (acc << 7) | (*b as usize & 0x7f));
        let footer = if rest[5] & 0x10 != 0 { 10 } else { 0 };
        rest = rest.get(10 + size + footer..).unwrap_or(&[]);
    }
    rest
}

/// Just enough MPEG-TS to pull one audio elementary stream out of a segment.
mod ts {
    const PACKET: usize = 188;
    const SYNC: u8 = 0x47;

    pub fn looks_like_ts(bytes: &[u8]) -> bool {
        bytes.len() >= PACKET * 2 && bytes[0] == SYNC && bytes[PACKET] == SYNC
    }

    /// Remembers the program layout across segments; it rarely changes.
    #[derive(Default)]
    pub struct Demuxer {
        pmt_pid: Option<u16>,
        audio_pid: Option<u16>,
    }

    impl Demuxer {
        pub fn audio_from(&mut self, bytes: &[u8]) -> Vec<u8> {
            let mut audio = Vec::with_capacity(bytes.len());
            for packet in bytes.as_chunks::<PACKET>().0.iter().filter(|p| p[0] == SYNC) {
                let pid = u16::from_be_bytes([packet[1] & 0x1f, packet[2]]);
                let unit_start = packet[1] & 0x40 != 0;
                let Some(payload) = payload(packet) else { continue };

                if pid == 0 && unit_start {
                    self.pmt_pid = first_program_pmt(payload);
                } else if Some(pid) == self.pmt_pid && unit_start {
                    self.audio_pid = audio_stream_pid(payload).or(self.audio_pid);
                } else if Some(pid) == self.audio_pid {
                    let data = if unit_start { skip_pes_header(payload) } else { Some(payload) };
                    audio.extend_from_slice(data.unwrap_or(&[]));
                }
            }
            audio
        }
    }

    fn payload(packet: &[u8]) -> Option<&[u8]> {
        let control = (packet[3] >> 4) & 0b11;
        let start = match control {
            0b01 => 4,
            0b11 => 5 + packet[4] as usize,
            _ => return None,
        };
        packet.get(start..)
    }

    /// Body of a PSI section, after the pointer field and 8-byte header.
    fn section(payload: &[u8]) -> Option<&[u8]> {
        let table = payload.get(1 + *payload.first()? as usize..)?;
        let length = (u16::from_be_bytes([*table.get(1)? & 0x0f, *table.get(2)?])) as usize;
        // Drop the 5 bytes after the length field and the trailing CRC.
        table.get(8..3 + length.checked_sub(4)?)
    }

    fn first_program_pmt(payload: &[u8]) -> Option<u16> {
        section(payload)?
            .as_chunks::<4>()
            .0
            .iter()
            .find(|e| u16::from_be_bytes([e[0], e[1]]) != 0)
            .map(|e| u16::from_be_bytes([e[2] & 0x1f, e[3]]))
    }

    fn audio_stream_pid(payload: &[u8]) -> Option<u16> {
        let body = section(payload)?;
        let info_len = (u16::from_be_bytes([*body.get(2)? & 0x0f, *body.get(3)?])) as usize;
        let mut streams = body.get(4 + info_len..)?;
        while streams.len() >= 5 {
            let kind = streams[0];
            let pid = u16::from_be_bytes([streams[1] & 0x1f, streams[2]]);
            let es_len = (u16::from_be_bytes([streams[3] & 0x0f, streams[4]])) as usize;
            // 0x0f: AAC in ADTS. 0x03 / 0x04: MPEG-1/2 audio.
            if matches!(kind, 0x0f | 0x03 | 0x04) {
                return Some(pid);
            }
            streams = streams.get(5 + es_len..)?;
        }
        None
    }

    fn skip_pes_header(payload: &[u8]) -> Option<&[u8]> {
        if payload.get(..3)? != [0, 0, 1] {
            return None;
        }
        payload.get(9 + *payload.get(8)? as usize..)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_aac_lc_over_he_aac() {
        let master = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=320000,CODECS=\"mp4a.40.5\"\nhigh.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nmid.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\nlow.m3u8\n";
        assert_eq!(pick_variant(master).unwrap().uri, "mid.m3u8");
    }

    #[test]
    fn parses_media_playlist() {
        let p = parse_media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:42\n#EXTINF:10,\na.aac\n#EXTINF:10,\nb.aac\n",
        );
        assert_eq!((p.sequence, p.segments.len(), p.ended), (42, 2, false));
        assert_eq!(p.target_duration, Some(10.0));
    }

    #[test]
    fn strips_leading_id3() {
        let mut seg = b"ID3\x04\x00\x00\x00\x00\x00\x03abc".to_vec();
        seg.extend(b"\xff\xf1audio");
        assert_eq!(strip_id3(&seg), b"\xff\xf1audio");
    }
}
