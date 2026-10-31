//! A quiet level meter: the last few thousand samples, folded into a handful
//! of log-spaced frequency bands that rise quickly and settle slowly.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Levels below the floor read as silence; the ceiling reads as full.
const FLOOR_DB: f32 = -66.0;
const CEILING_DB: f32 = -12.0;
/// How far a band may fall per frame, as a fraction of full scale.
const FALL: f32 = 0.06;

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    levels: Vec<f32>,
}

impl Spectrum {
    pub fn new(size: usize) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(size);
        // Hann window: keeps strong low notes from smearing across the bands.
        let window =
            (0..size).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos()).collect();
        Self { fft, window, scratch: vec![Complex::default(); size], levels: Vec::new() }
    }

    /// Fold `samples` (mono, oldest first) into `bands` levels in 0..=1.
    pub fn update(&mut self, samples: &[f32], rate: u32, bands: usize) -> &[f32] {
        if self.levels.len() != bands {
            self.levels = vec![0.0; bands];
        }
        let size = self.window.len();
        if samples.len() < size || bands == 0 {
            self.decay();
            return &self.levels;
        }
        let recent = &samples[samples.len() - size..];
        for ((slot, s), w) in self.scratch.iter_mut().zip(recent).zip(&self.window) {
            *slot = Complex::new(s * w, 0.0);
        }
        self.fft.process(&mut self.scratch);

        let bin_hz = rate as f32 / size as f32;
        let half = size / 2;
        let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / bands as f32);
        for (b, level) in self.levels.iter_mut().enumerate() {
            let lo = ((LOW_HZ * ratio.powi(b as i32)) / bin_hz) as usize;
            let hi = (((LOW_HZ * ratio.powi(b as i32 + 1)) / bin_hz) as usize).max(lo + 1).min(half);
            let peak = self.scratch[lo.min(half - 1)..hi].iter().map(|c| c.norm()).fold(0.0, f32::max);
            // Normalise by window gain, then map decibels onto 0..=1.
            let db = 20.0 * (peak / (size as f32 / 4.0)).max(1e-9).log10();
            let target = ((db - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0).powf(1.6);
            *level = if target > *level { target } else { (*level - FALL).max(target) };
        }
        &self.levels
    }

    fn decay(&mut self) {
        for level in &mut self.levels {
            *level = (*level - FALL).max(0.0);
        }
    }
}
