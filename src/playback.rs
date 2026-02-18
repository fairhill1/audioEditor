use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;

use crate::audio::Track;

pub struct PlaybackState {
    pub playing: AtomicBool,
    pub frame_pos: AtomicU64,
    pub total_frames: u64,
    /// If non-zero, stop playback when frame_pos reaches this frame
    pub end_frame: AtomicU64,
}

pub struct Player {
    pub state: Arc<PlaybackState>,
    _stream: Stream,
}

impl Player {
    pub fn new(tracks: &[Track]) -> Option<Self> {
        if tracks.is_empty() {
            return None;
        }

        // Find the common sample rate (use first clip's rate) and max length in frames
        let sample_rate = tracks.iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.sample_rate)
            .next()?;

        let max_duration_secs = tracks.iter()
            .map(|t| t.duration_secs())
            .fold(0.0_f64, f64::max);
        let max_frames = (max_duration_secs * sample_rate as f64) as u64;

        if max_frames == 0 {
            return None;
        }

        // Pre-mix all tracks down to stereo interleaved f32
        let total_samples = max_frames as usize * 2;
        let mut mixed = vec![0.0_f32; total_samples];

        for track in tracks {
            if track.muted {
                continue;
            }
            for clip in &track.clips {
                let ch = clip.channels as usize;
                let frames = clip.samples.len() / ch;
                let offset_frames = (clip.offset_secs * sample_rate as f64) as usize;

                let gain = clip.gain * track.gain;
                for f in 0..frames {
                    let out_f = offset_frames + f;
                    if out_f >= max_frames as usize {
                        break;
                    }
                    let left;
                    let right;
                    if ch == 1 {
                        left = clip.samples[f] * gain;
                        right = left;
                    } else {
                        left = clip.samples[f * ch] * gain;
                        right = clip.samples[f * ch + 1] * gain;
                    }
                    mixed[out_f * 2] += left;
                    mixed[out_f * 2 + 1] += right;
                }
            }
        }

        let state = Arc::new(PlaybackState {
            playing: AtomicBool::new(false),
            frame_pos: AtomicU64::new(0),
            total_frames: max_frames,
            end_frame: AtomicU64::new(0),
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
                    let end = cb_state.end_frame.load(Ordering::Relaxed) as usize;

                    for chunk in data.chunks_mut(2) {
                        if pos >= total || (end > 0 && pos >= end) {
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

    pub fn seek_to_secs(&self, secs: f64, max_duration: f64) {
        if max_duration > 0.0 {
            self.seek_frac(secs / max_duration);
        }
    }

    /// Set the frame at which playback should auto-stop. Pass 0 to disable.
    pub fn set_end_secs(&self, secs: f64, max_duration: f64) {
        if max_duration > 0.0 && secs > 0.0 {
            let frame = (secs / max_duration * self.state.total_frames as f64) as u64;
            self.state.end_frame.store(frame, Ordering::Relaxed);
        } else {
            self.state.end_frame.store(0, Ordering::Relaxed);
        }
    }

    pub fn toggle(&self) {
        let was_playing = self.state.playing.load(Ordering::Relaxed);
        if was_playing {
            self.state.playing.store(false, Ordering::Relaxed);
            self.state.end_frame.store(0, Ordering::Relaxed);
        } else {
            // If at end, restart from beginning
            let end = self.state.end_frame.load(Ordering::Relaxed);
            let pos = self.state.frame_pos.load(Ordering::Relaxed);
            if pos >= self.state.total_frames || (end > 0 && pos >= end) {
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
