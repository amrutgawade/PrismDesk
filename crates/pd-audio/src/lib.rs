//! PrismDesk audio — play the device's raw PCM audio via WASAPI (cpal).
//!
//! scrcpy is run with `audio_codec=raw`, so the audio socket carries 48 kHz
//! stereo s16le PCM (no decode needed). We feed it to the default Windows output
//! endpoint, nearest-resampling to the device mix rate. Video-priority: audio
//! adapts to its own render clock and never gates video.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const SRC_RATE: f64 = 48000.0; // scrcpy raw audio: 48 kHz stereo

/// Thread-safe handle used by the network thread to push PCM and to mute.
#[derive(Clone)]
pub struct AudioSink {
    buf: Arc<Mutex<VecDeque<f32>>>,
    muted: Arc<AtomicBool>,
}

impl AudioSink {
    /// Push one raw s16le stereo PCM chunk (as delivered on the audio socket).
    pub fn push_pcm_s16(&self, bytes: &[u8]) {
        let mut b = self.buf.lock().unwrap();
        for s in bytes.chunks_exact(2) {
            b.push_back(i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0);
        }
        // Bound to ~1 s so a paused consumer can't grow latency.
        let cap = (SRC_RATE as usize) * 2;
        while b.len() > cap {
            b.pop_front();
        }
    }

    /// Toggle mute; returns the new muted state.
    pub fn toggle_mute(&self) -> bool {
        let m = !self.muted.load(Ordering::Relaxed);
        self.muted.store(m, Ordering::Relaxed);
        m
    }
}

/// Owns the cpal output stream (kept alive for the session). Not Send — create
/// and hold it on the main thread; use `sink()` for the producer thread.
pub struct Player {
    _stream: cpal::Stream,
    sink: AudioSink,
    info: String,
}

impl Player {
    pub fn new() -> Result<Player, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no output device")?;
        let cfg = device.default_output_config().map_err(|e| e.to_string())?;
        let out_rate = cfg.sample_rate().0 as f64;
        let ch = cfg.channels() as usize;
        let fmt = cfg.sample_format();
        let info = format!("{} Hz, {} ch, {:?}", out_rate as u32, ch, fmt);

        let buf = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let muted = Arc::new(AtomicBool::new(false));
        let sink = AudioSink { buf: buf.clone(), muted: muted.clone() };

        let step = SRC_RATE / out_rate; // source frames advanced per output frame
        let mut acc = 0.0f64;
        let mut priming = true; // fill a small buffer before playing (anti-click)
        let target = 9600usize; // ~100 ms of stereo @48k to absorb network jitter
        let config: cpal::StreamConfig = cfg.into();
        let err_fn = |e| eprintln!("[audio] {e}");

        if fmt != cpal::SampleFormat::F32 {
            return Err(format!("unsupported output format {fmt:?}"));
        }
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut b = buf.lock().unwrap();
                    let m = muted.load(Ordering::Relaxed);
                    if priming && b.len() >= target {
                        priming = false;
                    }
                    for frame in data.chunks_mut(ch) {
                        let play = !priming && !m && b.len() >= 2;
                        let (l, r) = if play { (b[0], b[1]) } else { (0.0, 0.0) };
                        for (ci, s) in frame.iter_mut().enumerate() {
                            *s = if ci == 0 { l } else if ci == 1 { r } else { 0.0 };
                        }
                        if !priming {
                            acc += step;
                            while acc >= 1.0 && b.len() >= 2 {
                                b.pop_front();
                                b.pop_front();
                                acc -= 1.0;
                            }
                            if b.len() < 2 {
                                priming = true; // underran -> re-prime, avoids click storms
                            }
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        Ok(Player { _stream: stream, sink, info })
    }

    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }

    pub fn info(&self) -> &str {
        &self.info
    }
}
