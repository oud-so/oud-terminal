//! Streaming stereo sample-rate conversion.
//!
//! Radio streams arrive at whatever rate the broadcaster chose (usually 44.1
//! or 48 kHz) while the output device runs at its own fixed rate. Changing the
//! device rate would affect every other app, so we convert instead, using
//! 4-point cubic Hermite interpolation: cheap, and free of the dull top end
//! that linear interpolation leaves behind.

pub struct Resampler {
    /// Input frames consumed per output frame.
    step: f64,
    /// Position of the next output frame, relative to `history[1]`.
    phase: f64,
    /// The last four input frames seen, oldest first.
    history: [[f32; 2]; 4],
    passthrough: bool,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            phase: 0.0,
            history: [[0.0; 2]; 4],
            passthrough: from_rate == to_rate,
        }
    }

    /// Convert interleaved stereo `input`, appending interleaved stereo to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        for frame in input.as_chunks::<2>().0 {
            self.history.rotate_left(1);
            self.history[3] = [frame[0], frame[1]];
            // Emit every output frame that falls between history[1] and history[2].
            while self.phase < 1.0 {
                let t = self.phase as f32;
                for ch in 0..2 {
                    let [p0, p1, p2, p3] = self.history.map(|f| f[ch]);
                    out.push(hermite(p0, p1, p2, p3, t));
                }
                self.phase += self.step;
            }
            self.phase -= 1.0;
        }
    }
}

fn hermite(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (p2 - p0);
    let c2 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c3 = 0.5 * (p3 - p0) + 1.5 * (p1 - p2);
    ((c3 * t + c2) * t + c1) * t + p1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_length_tracks_rate_ratio() {
        let input = vec![0.25f32; 44_100 * 2];
        let mut out = Vec::new();
        Resampler::new(44_100, 48_000).process(&input, &mut out);
        let frames = out.len() / 2;
        assert!((47_990..=48_010).contains(&frames), "got {frames} frames");
    }

    #[test]
    fn constant_signal_stays_constant() {
        let input = vec![0.5f32; 2_000];
        let mut out = Vec::new();
        Resampler::new(32_000, 48_000).process(&input, &mut out);
        // Skip the warm-up frames that still blend in the zeroed history.
        assert!(out[16..].iter().all(|s| (s - 0.5).abs() < 1e-5));
    }
}
