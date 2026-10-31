//! The sound card side of the engine.
//!
//! One output stream is opened for the life of the app and fed from a
//! lock-free ring of interleaved stereo `f32`. Tuning to another station never
//! reopens the device; it bumps [`Deck::tune`], which tells the callback to
//! drop whatever audio is still queued from the previous station.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

/// Seconds of audio the ring can hold.
const RING_SECONDS: f32 = 4.0;
/// Seconds to gather before sound starts, so brief network stalls go unheard.
const PREROLL_SECONDS: f32 = 0.6;
/// Mono samples kept for the spectrum display.
pub const SCOPE_LEN: usize = 4096;

/// State shared between the decoder, the audio callback and the UI.
pub struct Deck {
    pub rate: u32,
    volume: AtomicU32,
    /// Incremented on every station change; queued audio from older tunes is dropped.
    tune: AtomicU64,
    /// Samples currently queued, for the buffer gauge.
    queued: AtomicUsize,
    /// Set while the callback is waiting for preroll (start-up or after a stall).
    filling: AtomicBool,
    pub producer: Mutex<HeapProd<f32>>,
    scope: Mutex<Scope>,
}

struct Scope {
    samples: Vec<f32>,
    write: usize,
}

impl Deck {
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn tune(&self) -> u64 {
        self.tune.load(Ordering::Acquire)
    }

    pub fn retune(&self) -> u64 {
        self.tune.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn is_filling(&self) -> bool {
        self.filling.load(Ordering::Relaxed)
    }

    /// Queued audio as a fraction of the preroll target (1.0 = ready).
    pub fn fill_level(&self) -> f32 {
        let target = (self.rate as f32 * 2.0 * PREROLL_SECONDS).max(1.0);
        (self.queued.load(Ordering::Relaxed) as f32 / target).min(1.0)
    }

    /// Copy the most recent mono samples, oldest first, into `out`.
    pub fn snapshot(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(scope) = self.scope.lock() {
            out.extend_from_slice(&scope.samples[scope.write..]);
            out.extend_from_slice(&scope.samples[..scope.write]);
        }
    }
}

/// Open the default output device. The returned deck stays valid for as
/// long as the process runs; the device stream lives on its own thread.
pub fn open(initial_volume: f32) -> Result<Arc<Deck>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("oud-output".into())
        .spawn(move || {
            match start_stream(initial_volume) {
                Ok((deck, stream)) => {
                    let _ = ready_tx.send(Ok(deck));
                    // Keep the stream alive; it stops when the process exits.
                    let _stream = stream;
                    loop {
                        thread::park();
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .context("spawning output thread")?;
    ready_rx.recv().map_err(|_| anyhow!("output thread exited"))?
}

fn start_stream(initial_volume: f32) -> Result<(Arc<Deck>, cpal::Stream)> {
    let host = cpal::default_host();
    let device = host.default_output_device().context("no audio output device found")?;
    let supported = device.default_output_config().context("reading output device config")?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let rate = config.sample_rate;
    let capacity = (rate as f32 * 2.0 * RING_SECONDS) as usize;
    let (producer, consumer) = HeapRb::<f32>::new(capacity).split();

    let deck = Arc::new(Deck {
        rate,
        volume: AtomicU32::new(initial_volume.clamp(0.0, 1.0).to_bits()),
        tune: AtomicU64::new(0),
        queued: AtomicUsize::new(0),
        filling: AtomicBool::new(true),
        producer: Mutex::new(producer),
        scope: Mutex::new(Scope { samples: vec![0.0; SCOPE_LEN], write: 0 }),
    });

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, consumer, deck.clone()),
        SampleFormat::F64 => build::<f64>(&device, config, consumer, deck.clone()),
        SampleFormat::I16 => build::<i16>(&device, config, consumer, deck.clone()),
        SampleFormat::I32 => build::<i32>(&device, config, consumer, deck.clone()),
        SampleFormat::U16 => build::<u16>(&device, config, consumer, deck.clone()),
        SampleFormat::U8 => build::<u8>(&device, config, consumer, deck.clone()),
        other => Err(anyhow!("unsupported output sample format {other}")),
    }?;
    stream.play().context("starting audio output")?;
    Ok((deck, stream))
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: HeapCons<f32>,
    deck: Arc<Deck>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let preroll = (config.sample_rate as f32 * 2.0 * PREROLL_SECONDS) as usize;
    let mut heard_tune = deck.tune();
    let mut stereo: Vec<f32> = Vec::new();

    let callback = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / channels;
        stereo.resize(frames * 2, 0.0);

        let tune = deck.tune();
        if tune != heard_tune {
            heard_tune = tune;
            consumer.clear();
            deck.filling.store(true, Ordering::Relaxed);
        }

        let queued = consumer.occupied_len();
        if deck.is_filling() && queued >= preroll {
            deck.filling.store(false, Ordering::Relaxed);
        }

        let got = if deck.is_filling() { 0 } else { consumer.pop_slice(&mut stereo) };
        stereo[got..].fill(0.0);
        // Ran dry mid-play: wait for a fresh preroll instead of stuttering.
        if !deck.is_filling() && got < stereo.len() {
            deck.filling.store(true, Ordering::Relaxed);
        }
        deck.queued.store(consumer.occupied_len(), Ordering::Relaxed);

        if let Ok(mut scope) = deck.scope.try_lock() {
            for pair in stereo.as_chunks::<2>().0 {
                let w = scope.write;
                scope.samples[w] = 0.5 * (pair[0] + pair[1]);
                scope.write = (w + 1) % SCOPE_LEN;
            }
        }

        // Perceptual volume: a squared curve feels even across the slider.
        let gain = deck.volume().powi(2);
        for (frame, pair) in data.chunks_mut(channels).zip(stereo.as_chunks::<2>().0) {
            for (ch, slot) in frame.iter_mut().enumerate() {
                let s = match (channels, ch) {
                    (1, _) => 0.5 * (pair[0] + pair[1]),
                    (_, 0) | (_, 1) => pair[ch],
                    _ => 0.0,
                };
                *slot = T::from_sample(s * gain);
            }
        }
    };

    device.build_output_stream(config, callback, |_| {}, None).context("opening audio output stream")
}
