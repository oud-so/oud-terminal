//! Whether a newer oud is out. The API is asked at most once a day, in the
//! background, and its answer is kept in the state file so the note stays
//! up between runs and offline. Nothing is downloaded or replaced here:
//! whoever installed oud upgrades it the same way, which for Homebrew is
//! `brew upgrade oud`.
//!
//! `OUD_NO_UPDATE_CHECK=1` turns the asking off.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const DAY_SECS: u64 = 24 * 60 * 60;

/// The API's answer, as last heard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    /// The newest release, if known.
    pub latest: Option<String>,
    /// The oldest release the API still works with, if it says.
    pub minimum: Option<String>,
    /// When this was asked, in seconds since the epoch.
    pub checked: u64,
}

/// What to tell the listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// A newer release is out.
    Available(String),
    /// This copy is older than the API still works with.
    Required(String),
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// `a` is a later version than `b`; false when either can't be read.
fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

impl Release {
    /// Time to ask again: never asked, a day has passed, or the clock went back.
    pub fn due(&self) -> bool {
        if std::env::var_os("OUD_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty() && v != "0") {
            return false;
        }
        let now = now();
        now < self.checked || now - self.checked >= DAY_SECS
    }

    pub fn heard(latest: Option<String>, minimum: Option<String>) -> Self {
        Self { latest, minimum, checked: now() }
    }

    /// What this build should say about it, if anything.
    pub fn update(&self) -> Option<Update> {
        let latest = self.latest.as_deref().filter(|l| newer(l, CURRENT));
        match (&self.minimum, latest) {
            (Some(min), _) if newer(min, CURRENT) => {
                Some(Update::Required(latest.unwrap_or(min).to_string()))
            }
            (_, Some(latest)) => Some(Update::Available(latest.to_string())),
            _ => None,
        }
    }
}

/// How this copy is upgraded, when it can be told: Homebrew's own path.
pub fn upgrade_command() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let path = exe.to_string_lossy();
    (path.contains("/Cellar/") || path.contains("/homebrew/") || path.contains("/linuxbrew/"))
        .then_some("brew upgrade oud")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(latest: Option<&str>, minimum: Option<&str>) -> Release {
        Release { latest: latest.map(Into::into), minimum: minimum.map(Into::into), checked: 0 }
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.99.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1", "0.0.1"));
        assert!(!newer("latest", "0.1.0"));
    }

    #[test]
    fn says_nothing_when_current_or_unknown() {
        assert_eq!(release(None, None).update(), None);
        assert_eq!(release(Some(CURRENT), None).update(), None);
        assert_eq!(release(Some("0.0.1"), Some("0.0.1")).update(), None);
    }

    #[test]
    fn tells_of_a_newer_release() {
        assert_eq!(release(Some("999.0.0"), None).update(), Some(Update::Available("999.0.0".into())));
    }

    #[test]
    fn requires_an_upgrade_below_the_minimum() {
        assert_eq!(
            release(Some("999.1.0"), Some("999.0.0")).update(),
            Some(Update::Required("999.1.0".into()))
        );
        assert_eq!(release(None, Some("999.0.0")).update(), Some(Update::Required("999.0.0".into())));
    }

    #[test]
    fn asks_again_after_a_day() {
        assert!(release(None, None).due() || std::env::var_os("OUD_NO_UPDATE_CHECK").is_some());
        let fresh = Release::heard(None, None);
        assert!(!fresh.due());
    }
}
