//! An abstract ASCII visualizer for the space beside the column.
//!
//! Each frame samples a field over the panel: moving waves, rain, curtains,
//! orbs and the like. In auto mode it wanders through all of them, slowly
//! crossfading from one to the next; a fixed pattern can be chosen instead.
//! The music steers it: bass swells the brightness and sends rings out from
//! the middle, mids speed the motion up, treble throws sparks. Every tune
//! reshuffles the pattern's proportions, so no two stations look alike.

/// From empty to dense. Lower steps are drawn dim, the top steps bold.
pub const RAMP: [char; 10] = [' ', '.', '·', ':', '-', '=', '+', '*', '#', '@'];

/// Seconds a pattern holds before the next one fades in, and how long the fade takes.
const HOLD: f32 = 18.0;
const FADE: f32 = 4.0;
/// Below this the field is left empty while nothing plays.
const IDLE_CREST: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    Plasma,
    Tunnel,
    Ripples,
    Spiral,
    Weave,
    Rain,
    Aurora,
    Kaleido,
    Orbs,
    Dunes,
    Warp,
    Lattice,
}

pub const PATTERNS: [Pattern; 12] = [
    Pattern::Plasma,
    Pattern::Tunnel,
    Pattern::Ripples,
    Pattern::Spiral,
    Pattern::Weave,
    Pattern::Rain,
    Pattern::Aurora,
    Pattern::Kaleido,
    Pattern::Orbs,
    Pattern::Dunes,
    Pattern::Warp,
    Pattern::Lattice,
];

/// A repeatable pseudo-random value in 0..1 for `n`.
fn hash(n: f32) -> f32 {
    ((n * 12.9898).sin() * 43_758.547).fract().abs()
}

/// Map a 0..1 value onto the -1..1 range patterns return.
fn signed(v: f32) -> f32 {
    v.clamp(0.0, 1.0) * 2.0 - 1.0
}

impl Pattern {
    pub fn name(self) -> &'static str {
        match self {
            Pattern::Plasma => "plasma",
            Pattern::Tunnel => "tunnel",
            Pattern::Ripples => "ripples",
            Pattern::Spiral => "spiral",
            Pattern::Weave => "weave",
            Pattern::Rain => "rain",
            Pattern::Aurora => "aurora",
            Pattern::Kaleido => "kaleido",
            Pattern::Orbs => "orbs",
            Pattern::Dunes => "dunes",
            Pattern::Warp => "warp",
            Pattern::Lattice => "lattice",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        PATTERNS.into_iter().find(|p| p.name() == name)
    }

    /// The field at (x, y), both roughly in -1..1, as a value in -1..1.
    fn sample(self, x: f32, y: f32, t: f32, k: &[f32; 4]) -> f32 {
        let r = (x * x + y * y).sqrt();
        let a = y.atan2(x);
        match self {
            Pattern::Plasma => {
                ((x * k[0] * 4.0 + t).sin()
                    + (y * k[1] * 5.0 - t * 0.8).sin()
                    + ((x + y) * k[2] * 3.0 + t * 0.5).sin()
                    + (r * k[3] * 8.0 - t * 1.7).sin())
                    / 4.0
            }
            Pattern::Tunnel => {
                ((2.5 + k[0]) / (r + 0.25) + t * 2.0).sin() * 0.6
                    + (a * (3.0 + (k[1] * 4.0).floor()) + t).sin() * 0.4
            }
            Pattern::Ripples => {
                // Two drifting sources whose rings cross.
                let (sx, sy) = ((t * 0.31 * k[0]).sin() * 0.6, (t * 0.23 * k[1]).cos() * 0.5);
                let d1 = ((x - sx).powi(2) + (y - sy).powi(2)).sqrt();
                let d2 = ((x + sx).powi(2) + (y + sy).powi(2)).sqrt();
                ((d1 * (9.0 + k[2] * 6.0) - t * 3.0).sin() + (d2 * (9.0 + k[3] * 6.0) - t * 3.0).sin()) / 2.0
            }
            Pattern::Spiral => (a * (2.0 + (k[0] * 3.0).floor()) + r * (6.0 + k[1] * 6.0) - t * 2.2).sin(),
            Pattern::Weave => ((x * (6.0 + k[0] * 4.0) + (y * 3.0 + t).sin() * 1.5).sin()
                * (y * (6.0 + k[1] * 4.0) + (x * 3.0 - t * 0.7).cos() * 1.5).cos())
            .clamp(-1.0, 1.0),
            Pattern::Rain => {
                // Narrow lanes, each falling at its own speed with a bright head.
                let lane = (x * (30.0 + k[0] * 20.0)).floor();
                let speed = 0.6 + hash(lane) * 1.4;
                let drop = (y * 0.8 - t * speed * 0.5 + hash(lane + 7.0)).rem_euclid(1.0);
                signed((1.0 - drop).powf(6.0 + k[1] * 6.0) * (0.4 + hash(lane + 3.0)))
            }
            Pattern::Aurora => {
                // Curtains hanging from a line that sways across the panel.
                let crest = (x * (2.0 + k[0] * 2.0) + t * 0.6).sin() * 0.35 + (x * 5.0 - t * 0.3).sin() * 0.1;
                let curtain = (-(y - crest + 0.2).abs() * (3.0 + k[1] * 3.0)).exp();
                let folds = (x * (14.0 + k[2] * 10.0) + (y * 3.0 + t).sin() * 2.0).sin() * 0.5 + 0.5;
                signed(curtain * (0.35 + 0.65 * folds))
            }
            Pattern::Kaleido => {
                // Fold the angle into mirrored wedges, then draw rings across them.
                let wedges = 5.0 + (k[0] * 4.0).floor();
                let wedge = std::f32::consts::TAU / wedges;
                let folded = ((a + t * 0.2).rem_euclid(wedge) / wedge - 0.5).abs();
                ((r * (10.0 + k[1] * 6.0) - t * 1.5).sin() + (folded * (12.0 + k[2] * 8.0) + t).cos()) / 2.0
            }
            Pattern::Orbs => {
                // Soft blobs on slow orbits that swell where they meet.
                let field: f32 = (0..5)
                    .map(|i| {
                        let i = i as f32;
                        let ox = (t * (0.3 + hash(i) * 0.4) + i * 1.7).sin() * 0.7;
                        let oy = (t * (0.25 + hash(i + 9.0) * 0.4) + i * 2.3).cos() * 0.6;
                        (0.018 + k[0] * 0.012) / ((x - ox).powi(2) + (y - oy).powi(2) + 0.002)
                    })
                    .sum();
                signed((field - 0.6) * 0.5)
            }
            Pattern::Dunes => {
                // Stacked ridgelines rolling sideways, like a landscape scrolling past.
                let ridge = (x * (3.0 + k[0] * 3.0) + t * 0.8).sin() * 0.5 + (x * 7.0 - t * 0.4).sin() * 0.15;
                (y * (14.0 + k[1] * 8.0) + ridge * 6.0).sin() * (0.4 + (y + 1.0) * 0.3).min(1.0)
            }
            Pattern::Warp => {
                // Points streaming out of the centre, each on its own ray.
                let ray = (a / std::f32::consts::TAU * (48.0 + k[0] * 32.0)).floor();
                let speed = 0.4 + hash(ray) * 0.8;
                let head = (r * 1.2 - t * speed * 0.6 + hash(ray + 5.0)).rem_euclid(1.0);
                signed(head.powf(14.0) * (r * 1.5).min(1.0) * 1.4)
            }
            Pattern::Lattice => {
                // A grid that turns and breathes in and out.
                let turn = t * 0.25;
                let zoom = 6.0 + (t * 0.5).sin() * 3.0 + k[0] * 4.0;
                let (u, v) = (x * turn.cos() - y * turn.sin(), x * turn.sin() + y * turn.cos());
                ((u * zoom).sin() * (v * zoom).sin() * 3.0).clamp(-1.0, 1.0)
            }
        }
    }
}

/// Bass, mid and treble pulled out of spectrum bands, each 0..=1.
#[derive(Default, Clone, Copy)]
pub struct Energy {
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
}

impl Energy {
    pub fn from_bands(bands: &[f32]) -> Self {
        let n = bands.len();
        if n < 3 {
            return Self::default();
        }
        let avg = |s: &[f32]| s.iter().sum::<f32>() / s.len().max(1) as f32;
        Self {
            bass: avg(&bands[..n / 4]),
            mid: avg(&bands[n / 4..n * 2 / 3]),
            treble: avg(&bands[n * 2 / 3..]),
        }
    }
}

pub struct Visual {
    /// A fixed pattern, or `None` to wander through all of them.
    mode: Option<Pattern>,
    seed: u64,
    shape: [f32; 4],
    /// Time runs faster when the music is busy, so it is integrated per frame.
    clock: f32,
    last: Option<f32>,
    /// Bass followed slowly, to notice when a beat lands above it.
    bass_floor: f32,
    /// Seconds since the last beat, for the ring it sends out.
    since_beat: f32,
}

/// One drawn cell: the glyph and how bright to paint it (0 dim, 1 plain, 2 bold).
pub type Cell = (char, u8);

impl Visual {
    pub fn new() -> Self {
        let mut v = Self {
            mode: None,
            seed: 0,
            shape: [0.5; 4],
            clock: 0.0,
            last: None,
            bass_floor: 0.0,
            since_beat: 9.0,
        };
        v.reshuffle();
        v
    }

    /// Pick new proportions and a new starting pattern.
    pub fn reshuffle(&mut self) {
        self.seed = fastrand::u64(..);
        self.shape = [fastrand::f32(), fastrand::f32(), fastrand::f32(), fastrand::f32()];
        self.clock = fastrand::f32() * 100.0;
    }

    pub fn mode(&self) -> Option<Pattern> {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Option<Pattern>) {
        self.mode = mode;
    }

    /// Step through auto, then each pattern in turn, then back to auto.
    pub fn cycle(&mut self, step: isize) {
        let slots = PATTERNS.len() as isize + 1; // slot 0 is auto
        let at = self.mode.and_then(|m| PATTERNS.iter().position(|p| *p == m)).map_or(0, |i| i as isize + 1);
        let next = (at + step).rem_euclid(slots);
        self.mode = if next == 0 { None } else { Some(PATTERNS[next as usize - 1]) };
    }

    /// What the caption says: the pattern, marked when chosen automatically.
    pub fn label(&self) -> String {
        match self.mode {
            Some(p) => p.name().to_string(),
            None => format!("auto · {}", self.patterns().0.name()),
        }
    }

    /// The pattern on screen, the one fading in, and how far the fade is (0..1).
    fn patterns(&self) -> (Pattern, Pattern, f32) {
        if let Some(p) = self.mode {
            return (p, p, 0.0);
        }
        let span = HOLD + FADE;
        let step = (self.clock / span) as u64 + self.seed;
        let into = (self.clock % span - HOLD).max(0.0) / FADE;
        let pick = |i: u64| PATTERNS[(i % PATTERNS.len() as u64) as usize];
        (pick(step), pick(step + 1), into)
    }

    /// Advance to wall-clock time `now` (seconds) and draw `width` × `height` cells.
    pub fn frame(
        &mut self,
        now: f32,
        width: usize,
        height: usize,
        energy: Energy,
        live: bool,
    ) -> Vec<Vec<Cell>> {
        let dt = self.last.map_or(0.0, |l| (now - l).clamp(0.0, 0.2));
        self.last = Some(now);

        // Silence drifts slowly; music pushes the clock along.
        let pace = if live { 0.5 + energy.mid * 1.8 } else { 0.18 };
        self.clock += dt * pace;
        self.bass_floor += (energy.bass - self.bass_floor) * (dt * 1.5).min(1.0);
        self.since_beat += dt;
        if live && energy.bass > self.bass_floor * 1.25 + 0.05 && self.since_beat > 0.25 {
            self.since_beat = 0.0;
        }

        let (now_pattern, next_pattern, fade) = self.patterns();
        let t = self.clock * 3.0;
        let gain = if live { 0.35 + energy.bass * 0.9 } else { 0.38 };
        let ring = self.since_beat * 1.6;
        let sparks = if live { energy.treble * 0.035 } else { 0.0 };

        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let scale = cx.max(cy * 2.0).max(1.0);
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|col| {
                        // Terminal cells are about twice as tall as wide.
                        let x = (col as f32 - cx) / scale;
                        let y = (row as f32 - cy) * 2.0 / scale;
                        let a = now_pattern.sample(x, y, t, &self.shape);
                        let b = next_pattern.sample(x, y, t, &self.shape);
                        let mut v = (a + (b - a) * smooth(fade)) * 0.5 + 0.5;
                        if !live {
                            // At rest only the crests show, faint and sparse.
                            v = ((v - IDLE_CREST) / (1.0 - IDLE_CREST)).max(0.0);
                        }
                        v *= gain;
                        if live && ring < 1.6 {
                            let r = (x * x + y * y).sqrt();
                            v += (-(r - ring).abs() * 9.0).exp() * (1.0 - ring / 1.6) * 0.8;
                        }
                        if sparks > 0.0 && fastrand::f32() < sparks {
                            return ('*', 2);
                        }
                        glyph(v.clamp(0.0, 1.0))
                    })
                    .collect()
            })
            .collect()
    }
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn glyph(v: f32) -> Cell {
    let i = ((v * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1);
    let tone = match i {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    };
    (RAMP[i], tone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_the_requested_size() {
        let mut v = Visual::new();
        let frame = v.frame(0.0, 30, 12, Energy::default(), false);
        assert_eq!(frame.len(), 12);
        assert!(frame.iter().all(|row| row.len() == 30));
    }

    #[test]
    fn every_pattern_stays_in_range() {
        let k = [0.1, 0.5, 0.9, 0.3];
        for p in PATTERNS {
            for i in 0..400 {
                let (x, y, t) = ((i % 20) as f32 / 10.0 - 1.0, (i / 20) as f32 / 10.0 - 1.0, i as f32 * 0.37);
                let v = p.sample(x, y, t, &k);
                assert!(v.is_finite() && (-1.0..=1.0).contains(&v), "{} gave {v}", p.name());
            }
        }
    }

    #[test]
    fn cycling_visits_every_pattern_then_auto() {
        let mut v = Visual::new();
        let mut seen = Vec::new();
        for _ in 0..PATTERNS.len() {
            v.cycle(1);
            seen.push(v.mode().unwrap().name());
        }
        assert_eq!(seen.len(), PATTERNS.len());
        v.cycle(1);
        assert!(v.mode().is_none());
        v.cycle(-1);
        assert_eq!(v.mode(), Some(PATTERNS[PATTERNS.len() - 1]));
    }

    #[test]
    fn silence_stays_faint() {
        let mut v = Visual::new();
        let frame = v.frame(1.0, 40, 16, Energy::default(), false);
        assert!(frame.iter().flatten().all(|(_, tone)| *tone < 2));
    }
}
