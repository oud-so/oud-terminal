//! What the terminal remembers between runs: volume, the visual, the last
//! station and what was last heard about new releases, in one small JSON file in the platform's config directory.
//! Bookmarks and lists live on the account, not here.
//!
//! The session token sits beside it in a file of its own that only the user
//! can read, so the settings file can be looked at or shared without it.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::api::Station;
use crate::update::Release;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub volume: f32,
    /// Whether the visual beside the column is shown.
    pub visual: bool,
    /// A fixed visual by name; empty means auto.
    pub visual_mode: String,
    /// The station last tuned to, so space can bring it back.
    pub last: Option<Station>,
    /// What the API last said about new releases (see `update`).
    pub release: Release,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            volume: 0.8,
            visual: true,
            visual_mode: String::new(),
            last: None,
            release: Release::default(),
        }
    }
}

fn dir() -> Option<PathBuf> {
    ProjectDirs::from("so", "oud", "oud").map(|d| d.config_dir().to_path_buf())
}

/// Write `bytes` to `path` through a temporary file, so a crash mid-write
/// never leaves a torn file. The file is readable by the user alone.
fn write_atomic(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(bytes)?;
    fs::rename(tmp, path)
}

impl Store {
    fn path() -> Option<PathBuf> {
        dir().map(|d| d.join("state.json"))
    }

    /// Load saved state. A missing or unreadable file starts fresh.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else { return Ok(()) };
        write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

/// The signed-in session, kept between runs.
pub mod session {
    use super::*;

    fn path() -> Option<PathBuf> {
        dir().map(|d| d.join("session"))
    }

    pub fn load() -> Option<String> {
        let token = fs::read_to_string(path()?).ok()?;
        Some(token.trim().to_string()).filter(|t| !t.is_empty())
    }

    pub fn save(token: &str) -> std::io::Result<()> {
        let Some(path) = path() else { return Ok(()) };
        write_atomic(&path, token.as_bytes())
    }

    pub fn clear() {
        if let Some(path) = path() {
            let _ = fs::remove_file(path);
        }
    }
}
