//! Shoutcast/Icecast in-band metadata.
//!
//! When a client asks with `Icy-MetaData: 1`, the server interleaves a
//! metadata block after every `icy-metaint` bytes of audio: one length byte
//! (in units of 16) followed by that many bytes of `Key='value';` pairs.
//! [`IcyReader`] hands the decoder clean audio and reports titles on the side.

use std::io::{self, Read};

pub struct IcyReader<R, F> {
    inner: R,
    /// Audio bytes between metadata blocks; `None` when the server sends none.
    interval: Option<usize>,
    /// Audio bytes left before the next metadata block.
    until_meta: usize,
    on_title: F,
    last_title: Option<String>,
}

impl<R: Read, F: FnMut(String)> IcyReader<R, F> {
    pub fn new(inner: R, interval: Option<usize>, on_title: F) -> Self {
        let interval = interval.filter(|n| *n > 0);
        Self { inner, interval, until_meta: interval.unwrap_or(0), on_title, last_title: None }
    }

    fn read_meta_block(&mut self) -> io::Result<()> {
        let mut len = [0u8; 1];
        self.inner.read_exact(&mut len)?;
        let size = len[0] as usize * 16;
        if size == 0 {
            return Ok(());
        }
        let mut block = vec![0u8; size];
        self.inner.read_exact(&mut block)?;
        if let Some(title) = stream_title(&block)
            && self.last_title.as_deref() != Some(title.as_str())
        {
            self.last_title = Some(title.clone());
            (self.on_title)(title);
        }
        Ok(())
    }
}

impl<R: Read, F: FnMut(String)> Read for IcyReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(interval) = self.interval else {
            return self.inner.read(buf);
        };
        if self.until_meta == 0 {
            self.read_meta_block()?;
            self.until_meta = interval;
        }
        let want = buf.len().min(self.until_meta);
        let got = self.inner.read(&mut buf[..want])?;
        self.until_meta -= got;
        Ok(got)
    }
}

/// Pull `StreamTitle` out of a raw metadata block.
///
/// Titles routinely contain apostrophes ("Guns N' Roses"), so the value runs
/// to the last `';` rather than the first quote.
fn stream_title(block: &[u8]) -> Option<String> {
    let end = block.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let text = decode_text(&block[..end]);
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let stop = rest.find("';StreamUrl=").or_else(|| rest.rfind("';")).or_else(|| rest.rfind('\''))?;
    let title = rest[..stop].trim();
    // Many stations send separators or their own name as filler.
    if title.chars().all(|c| !c.is_alphanumeric()) {
        return None;
    }
    Some(title.to_string())
}

/// Metadata has no declared charset. Most modern servers send UTF-8; older
/// ones send Latin-1, which maps byte-for-byte onto the first 256 code points.
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(meta: &str) -> Vec<u8> {
        let mut bytes = meta.as_bytes().to_vec();
        bytes.resize(bytes.len().div_ceil(16) * 16, 0);
        let mut out = vec![(bytes.len() / 16) as u8];
        out.extend(bytes);
        out
    }

    #[test]
    fn strips_metadata_and_reports_titles() {
        let mut wire = b"abcd".to_vec();
        wire.extend(block("StreamTitle='Guns N' Roses - Patience';StreamUrl='';"));
        wire.extend(b"efgh");
        wire.push(0);
        wire.extend(b"ij");

        let mut titles = Vec::new();
        let mut audio = Vec::new();
        IcyReader::new(&wire[..], Some(4), |t| titles.push(t)).read_to_end(&mut audio).unwrap();

        assert_eq!(audio, b"abcdefghij");
        assert_eq!(titles, vec!["Guns N' Roses - Patience".to_string()]);
    }

    #[test]
    fn passes_through_without_interval() {
        let mut audio = Vec::new();
        IcyReader::new(&b"plain"[..], None, |_| panic!("no metadata expected"))
            .read_to_end(&mut audio)
            .unwrap();
        assert_eq!(audio, b"plain");
    }

    #[test]
    fn reads_latin1_and_ignores_filler() {
        assert_eq!(stream_title(b"StreamTitle='Caf\xe9 del Mar';"), Some("Café del Mar".into()));
        assert_eq!(stream_title(b"StreamTitle=' - ';"), None);
        assert_eq!(stream_title(b"StreamUrl='x';"), None);
    }
}
