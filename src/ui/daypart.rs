//! The hour of the day as explore reads it, matching oud.so: a line of copy
//! and the tags "play something" draws from. It follows the listener's own
//! clock, so the screen agrees with the light outside their window.

use chrono::{Local, Timelike};

pub struct Daypart {
    pub line: &'static str,
    pub source: &'static str,
    pub tags: &'static [&'static str],
}

// Start hours, ascending. The last entry runs until midnight.
const SCHEDULE: &[(u32, Daypart)] = &[
    (
        0,
        Daypart {
            line: "late. the quiet frequencies.",
            source: "late night",
            tags: &["ambient", "chillout", "lounge"],
        },
    ),
    (
        5,
        Daypart {
            line: "first light. something gentle.",
            source: "first light",
            tags: &["classical", "jazz", "folk"],
        },
    ),
    (
        8,
        Daypart {
            line: "morning. voices and coffee.",
            source: "morning",
            tags: &["news", "classical", "pop"],
        },
    ),
    (
        12,
        Daypart { line: "afternoon. keep it moving.", source: "afternoon", tags: &["pop", "rock", "oldies"] },
    ),
    (17, Daypart { line: "evening. slow down.", source: "evening", tags: &["soul", "jazz", "blues"] }),
    (21, Daypart { line: "night. turn it up.", source: "night", tags: &["house", "dance", "techno"] }),
];

pub fn now() -> &'static Daypart {
    let hour = Local::now().hour();
    SCHEDULE.iter().rev().find(|(from, _)| hour >= *from).map(|(_, part)| part).unwrap_or(&SCHEDULE[0].1)
}
