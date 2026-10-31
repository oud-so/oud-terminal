//! Native audio engine: HTTP stream → ICY splitter → demux/decode →
//! resample → ring buffer → sound card. No external player is involved.

mod hls;
mod icy;
mod output;
mod resample;

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ringbuf::traits::Producer;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;

use crate::api::{Api, Station, StreamOption};
pub use output::Deck;
use resample::Resampler;

/// Everything the engine reports. Each event carries the tune it belongs
/// to so a late message from a station you already left is easy to ignore.
#[derive(Debug, Clone)]
pub enum Event {
    Connecting { tune: u64 },
    OnAir { tune: u64, format: String },
    Title { tune: u64, title: String },
    Reconnecting { tune: u64 },
    Failed { tune: u64, reason: String },
}

pub struct Player {
    deck: Arc<Deck>,
    api: Api,
    events: Sender<Event>,
}

impl Player {
    pub fn new(api: Api, volume: f32, events: Sender<Event>) -> Result<Self> {
        Ok(Self { deck: output::open(volume)?, api, events })
    }

    pub fn deck(&self) -> &Arc<Deck> {
        &self.deck
    }

    /// Tune to `station`, abandoning whatever was playing. Returns the new tune id.
    pub fn play(&self, station: &Station) -> u64 {
        let tune = self.deck.retune();
        let session = Session {
            tune,
            station: station.clone(),
            deck: self.deck.clone(),
            api: self.api.clone(),
            events: self.events.clone(),
        };
        let _ = thread::Builder::new().name("oud-tune".into()).spawn(move || session.run());
        tune
    }

    /// Silence the deck. Any running session notices and winds down.
    pub fn stop(&self) {
        self.deck.retune();
    }
}

/// How often a stream that was playing may drop before we move on.
const RECONNECTS: u32 = 3;
/// Codecs the decoder cannot handle; streams using them are skipped up front.
const UNPLAYABLE: &[&str] = &["opus", "ogg opus", "wma"];

enum Ended {
    /// We were tuned away; stop quietly.
    TunedAway,
    /// The server closed the stream.
    Drained,
}

struct Session {
    tune: u64,
    station: Station,
    deck: Arc<Deck>,
    api: Api,
    events: Sender<Event>,
}

impl Session {
    fn current(&self) -> bool {
        self.deck.tune() == self.tune
    }

    fn emit(&self, event: Event) {
        if self.current() {
            let _ = self.events.send(event);
        }
    }

    fn run(self) {
        self.emit(Event::Connecting { tune: self.tune });
        let reason = match self.tune_in() {
            Ok(()) => return,
            Err(e) => e.to_string(),
        };
        self.emit(Event::Failed { tune: self.tune, reason });
    }

    fn tune_in(&self) -> Result<()> {
        let streams: Vec<StreamOption> = self
            .api
            .streams(&self.station.id)?
            .into_iter()
            .filter(|s| !is_unplayable(s.codec.as_deref()))
            .collect();
        if streams.is_empty() {
            bail!("off air: no stream we can play");
        }
        let client = Api::stream_client()?;

        for stream in &streams {
            let mut drops = 0;
            let mut heard = false;
            loop {
                if !self.current() {
                    return Ok(());
                }
                match self.play_stream(&client, stream, &mut heard) {
                    Ok(Ended::TunedAway) => return Ok(()),
                    Ok(Ended::Drained) | Err(_) if heard && drops < RECONNECTS => {
                        drops += 1;
                        self.emit(Event::Reconnecting { tune: self.tune });
                        if !self.wait(Duration::from_secs(1 << drops)) {
                            return Ok(());
                        }
                    }
                    Ok(Ended::Drained) => break,
                    Err(e) => {
                        if !heard {
                            let reason = failure_reason(&e);
                            self.api.report_failure(&self.station.id, &stream.radio_id, &reason);
                        }
                        break;
                    }
                }
            }
        }
        bail!("couldn't tune in")
    }

    /// Sleep for `d`, returning early (false) if we are tuned away.
    fn wait(&self, d: Duration) -> bool {
        let step = Duration::from_millis(50);
        let mut left = d;
        while !left.is_zero() {
            if !self.current() {
                return false;
            }
            thread::sleep(step.min(left));
            left = left.saturating_sub(step);
        }
        self.current()
    }

    fn play_stream(
        &self,
        client: &reqwest::blocking::Client,
        stream: &StreamOption,
        heard: &mut bool,
    ) -> Result<Ended> {
        let mut hint = Hint::new();
        let source: Box<dyn MediaSource> = if stream.hls {
            let (deck, tune) = (self.deck.clone(), self.tune);
            let opened = hls::HlsReader::open(client.clone(), &stream.url, move || deck.tune() == tune)?;
            hint.with_extension(opened.extension);
            Box::new(ReadOnlySource::new(opened.reader))
        } else {
            let res = client.get(&stream.url).header("Icy-MetaData", "1").send().context("connect")?;
            if !res.status().is_success() {
                bail!("http {}", res.status().as_u16());
            }
            let header =
                |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
            if let Some(mime) = header("content-type") {
                hint.mime_type(mime.split(';').next().unwrap_or(&mime).trim());
            }
            if let Some(ext) = extension_for(stream.codec.as_deref()) {
                hint.with_extension(ext);
            }
            let interval = header("icy-metaint").and_then(|v| v.trim().parse().ok());

            let (events, tune, deck) = (self.events.clone(), self.tune, self.deck.clone());
            let reader = icy::IcyReader::new(res, interval, move |title| {
                if deck.tune() == tune {
                    let _ = events.send(Event::Title { tune, title });
                }
            });
            Box::new(ReadOnlySource::new(reader))
        };
        let mss = MediaSourceStream::new(source, Default::default());

        let mut format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .context("unrecognised stream")?;
        let track = format.default_track(TrackType::Audio).context("no audio in stream")?;
        let track_id = track.id;
        let params =
            track.codec_params.as_ref().and_then(|p| p.audio()).context("no audio in stream")?.clone();
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .context("unsupported codec")?;

        let label = format_label(stream);
        let mut resampler: Option<(u32, Resampler)> = None;
        let mut decoded: Vec<f32> = Vec::new();
        let mut stereo: Vec<f32> = Vec::new();
        let mut converted: Vec<f32> = Vec::new();
        let mut bad_packets = 0;

        loop {
            if !self.current() {
                return Ok(Ended::TunedAway);
            }
            let packet = match format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(Ended::Drained),
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Ok(Ended::Drained);
                }
                Err(e) => return Err(e).context("stream read"),
            };
            if packet.track_id != track_id {
                continue;
            }
            let buf = match decoder.decode(&packet) {
                Ok(buf) => buf,
                // Radio streams start mid-frame and hiccup; skip, but not forever.
                Err(SymphoniaError::DecodeError(_)) if bad_packets < 64 => {
                    bad_packets += 1;
                    continue;
                }
                Err(e) => return Err(e).context("decode"),
            };
            bad_packets = 0;

            let rate = buf.spec().rate();
            let channels = buf.spec().channels().count().max(1);
            buf.copy_to_vec_interleaved(&mut decoded);
            to_stereo(&decoded, channels, &mut stereo);

            let resampler = match &mut resampler {
                Some((r, rs)) if *r == rate => rs,
                slot => &mut slot.insert((rate, Resampler::new(rate, self.deck.rate))).1,
            };
            converted.clear();
            resampler.process(&stereo, &mut converted);

            if !*heard {
                *heard = true;
                self.emit(Event::OnAir { tune: self.tune, format: label.clone() });
            }
            if !self.feed(&converted) {
                return Ok(Ended::TunedAway);
            }
        }
    }

    /// Push samples into the deck, waiting for room. False if tuned away.
    fn feed(&self, mut samples: &[f32]) -> bool {
        while !samples.is_empty() {
            if !self.current() {
                return false;
            }
            let pushed = match self.deck.producer.lock() {
                Ok(mut p) => p.push_slice(samples),
                Err(_) => return false,
            };
            samples = &samples[pushed..];
            if pushed == 0 {
                thread::sleep(Duration::from_millis(15));
            }
        }
        true
    }
}

fn to_stereo(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    for frame in interleaved.chunks_exact(channels) {
        let (l, r) = if channels == 1 { (frame[0], frame[0]) } else { (frame[0], frame[1]) };
        out.push(l);
        out.push(r);
    }
}

fn is_unplayable(codec: Option<&str>) -> bool {
    codec.is_some_and(|c| UNPLAYABLE.contains(&c.trim().to_ascii_lowercase().as_str()))
}

fn extension_for(codec: Option<&str>) -> Option<&'static str> {
    match codec?.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mpeg" => Some("mp3"),
        "aac" | "aac+" | "aacp" | "he-aac" => Some("aac"),
        "ogg" | "vorbis" => Some("ogg"),
        "flac" => Some("flac"),
        _ => None,
    }
}

fn format_label(stream: &StreamOption) -> String {
    let mut codec = stream.codec.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    if codec == "unknown" {
        codec.clear();
    }
    match (codec.is_empty(), stream.bitrate.filter(|b| *b > 0)) {
        (false, Some(b)) => format!("{codec} {b}k"),
        (false, None) => codec,
        (true, Some(b)) => format!("{b}k"),
        (true, None) => "live".into(),
    }
}

/// Short machine-readable reason sent with a failure report.
fn failure_reason(e: &anyhow::Error) -> String {
    let top = e.to_string();
    let reason = if top.starts_with("http ") { top.replace(' ', "_") } else { top };
    reason.chars().take(60).collect()
}
