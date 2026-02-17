use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;

use crate::audio::AudioTrack;

pub struct PlaybackState {
    pub playing: AtomicBool,
    pub frame_pos: AtomicU64,
    pub total_frames: u64,
}

pub struct Player {
    pub state: Arc<PlaybackState>,
    _stream: Stream,
}

impl Player {
    pub fn new(tracks: &[AudioTrack]) -> Option<Self> {
        if tracks.is_empty() {
            return None;
        }

        // Find the common sample rate (use first track's rate) and max length in frames
        let sample_rate = tracks[0].sample_rate;
        let max_frames = tracks
            .iter()
            .map(|t| t.samples.len() / t.channels as usize)
            .max()
            .unwrap_or(0) as u64;

        // Pre-mix all tracks down to stereo interleaved f32
        let total_samples = max_frames as usize * 2;
        let mut mixed = vec![0.0_f32; total_samples];

        for track in tracks {
            let ch = track.channels as usize;
            let frames = track.samples.len() / ch;
            for f in 0..frames {
                let left;
                let right;
                if ch == 1 {
                    left = track.samples[f];
                    right = left;
                } else {
                    left = track.samples[f * ch];
                    right = track.samples[f * ch + 1];
                }
                mixed[f * 2] += left;
                mixed[f * 2 + 1] += right;
            }
        }

        let state = Arc::new(PlaybackState {
            playing: AtomicBool::new(false),
            frame_pos: AtomicU64::new(0),
            total_frames: max_frames,
        });

        let host = cpal::default_host();
        let device = host.default_output_device()?;

        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let mixed = Arc::new(mixed);
        let cb_state = Arc::clone(&state);
        let cb_mixed = Arc::clone(&mixed);

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !cb_state.playing.load(Ordering::Relaxed) {
                        data.fill(0.0);
                        return;
                    }

                    let mut pos = cb_state.frame_pos.load(Ordering::Relaxed) as usize;
                    let total = cb_state.total_frames as usize;

                    for chunk in data.chunks_mut(2) {
                        if pos >= total {
                            cb_state.playing.store(false, Ordering::Relaxed);
                            chunk.fill(0.0);
                        } else {
                            chunk[0] = cb_mixed[pos * 2];
                            chunk[1] = cb_mixed[pos * 2 + 1];
                            pos += 1;
                        }
                    }

                    cb_state.frame_pos.store(pos as u64, Ordering::Relaxed);
                },
                |err| eprintln!("Audio stream error: {err}"),
                None,
            )
            .ok()?;

        stream.play().ok()?;

        Some(Player {
            state,
            _stream: stream,
        })
    }

    pub fn seek_frac(&self, frac: f64) {
        let frame = (frac.clamp(0.0, 1.0) * self.state.total_frames as f64) as u64;
        self.state.frame_pos.store(frame, Ordering::Relaxed);
    }

    pub fn toggle(&self) {
        let was_playing = self.state.playing.load(Ordering::Relaxed);
        if was_playing {
            self.state.playing.store(false, Ordering::Relaxed);
        } else {
            // If at end, restart from beginning
            if self.state.frame_pos.load(Ordering::Relaxed) >= self.state.total_frames {
                self.state.frame_pos.store(0, Ordering::Relaxed);
            }
            self.state.playing.store(true, Ordering::Relaxed);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state.playing.load(Ordering::Relaxed)
    }

    pub fn position_frac(&self) -> f64 {
        let pos = self.state.frame_pos.load(Ordering::Relaxed);
        if self.state.total_frames == 0 {
            0.0
        } else {
            pos as f64 / self.state.total_frames as f64
        }
    }
}
