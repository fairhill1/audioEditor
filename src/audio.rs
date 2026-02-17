use std::f64::consts::PI;
use std::fs::File;
use std::path::Path;

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{FixedSync, Fft, Indexing, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Clone)]
pub struct Clip {
    pub name: String,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
    /// Pre-computed mono mixdown for display
    pub mono: Vec<f32>,
    /// Pre-computed min/max summary at fixed bucket size for fast rendering
    pub summary: Vec<(f32, f32)>,
    /// Position of this clip on the timeline in seconds
    pub offset_secs: f64,
    /// Original source file path (for project save metadata)
    pub source_path: Option<String>,
}

/// Samples per summary bucket — tune for a good balance of detail vs speed
const SUMMARY_BUCKET: usize = 256;

impl Clip {
    pub fn duration_secs(&self) -> f64 {
        self.mono.len() as f64 / self.sample_rate as f64
    }

    pub fn build_mono(samples: &[f32], channels: u32) -> Vec<f32> {
        let ch = channels as usize;
        (0..samples.len() / ch)
            .map(|i| {
                let mut s = 0.0_f32;
                for c in 0..ch {
                    s += samples[i * ch + c];
                }
                s / ch as f32
            })
            .collect()
    }

    pub fn build_summary(mono: &[f32]) -> Vec<(f32, f32)> {
        mono.chunks(SUMMARY_BUCKET)
            .map(|chunk| {
                let mut min = f32::MAX;
                let mut max = f32::MIN;
                for &s in chunk {
                    min = min.min(s);
                    max = max.max(s);
                }
                (min, max)
            })
            .collect()
    }

    /// Split this clip at `secs` (relative to clip start), returning the right half.
    /// This clip is truncated to the left half.
    pub fn split_at(&mut self, secs: f64) -> Clip {
        let mono_idx = (secs * self.sample_rate as f64) as usize;
        let sample_idx = mono_idx * self.channels as usize;

        let right_samples = self.samples.split_off(sample_idx.min(self.samples.len()));
        let right_mono = self.mono.split_off(mono_idx.min(self.mono.len()));
        // Rebuild summaries for both halves
        self.summary = Self::build_summary(&self.mono);
        let right_summary = Self::build_summary(&right_mono);

        Clip {
            name: self.name.clone(),
            samples: right_samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
            mono: right_mono,
            summary: right_summary,
            offset_secs: self.offset_secs + secs,
            source_path: self.source_path.clone(),
        }
    }

    /// Get min/max for a range of mono samples, using the summary where possible
    pub fn min_max_range(&self, start: usize, end: usize) -> (f32, f32) {
        let mut min_val = 0.0_f32;
        let mut max_val = 0.0_f32;

        let bucket_start = (start + SUMMARY_BUCKET - 1) / SUMMARY_BUCKET;
        let bucket_end = end / SUMMARY_BUCKET;

        if bucket_start < bucket_end {
            // Scan leftover samples before first full bucket
            for &s in &self.mono[start..bucket_start * SUMMARY_BUCKET] {
                min_val = min_val.min(s);
                max_val = max_val.max(s);
            }
            // Use summary for full buckets
            for &(bmin, bmax) in &self.summary[bucket_start..bucket_end] {
                min_val = min_val.min(bmin);
                max_val = max_val.max(bmax);
            }
            // Scan leftover samples after last full bucket
            for &s in &self.mono[bucket_end * SUMMARY_BUCKET..end] {
                min_val = min_val.min(s);
                max_val = max_val.max(s);
            }
        } else {
            // Range is smaller than a bucket, scan directly
            for &s in &self.mono[start..end] {
                min_val = min_val.min(s);
                max_val = max_val.max(s);
            }
        }

        (min_val, max_val)
    }
}

#[derive(Clone)]
pub struct Track {
    pub clips: Vec<Clip>,
    pub muted: bool,
}

impl Track {
    pub fn duration_secs(&self) -> f64 {
        self.clips
            .iter()
            .map(|c| c.offset_secs + c.duration_secs())
            .fold(0.0_f64, f64::max)
    }
}

fn resample(samples: &[f32], channels: u32, from_rate: u32, to_rate: u32, on_progress: &dyn Fn(f32)) -> Vec<f32> {
    let ch = channels as usize;
    let in_frames = samples.len() / ch;

    let mut resampler =
        Fft::<f32>::new(from_rate as usize, to_rate as usize, 1024, 1, ch, FixedSync::Input)
            .expect("Failed to create resampler");

    let out_frames_max = (in_frames as f64 * to_rate as f64 / from_rate as f64).ceil() as usize
        + resampler.output_frames_max();
    let mut output = vec![0.0_f32; out_frames_max * ch];

    let input_adapter = InterleavedSlice::new(samples, ch, in_frames).unwrap();
    let mut output_adapter =
        InterleavedSlice::new_mut(&mut output, ch, out_frames_max).unwrap();

    let mut indexing = Indexing {
        input_offset: 0,
        output_offset: 0,
        partial_len: None,
        active_channels_mask: None,
    };

    let mut remaining = in_frames;
    while remaining > 0 {
        let needed = resampler.input_frames_next();
        if remaining < needed {
            indexing.partial_len = Some(remaining);
        }
        let (consumed, produced) = resampler
            .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
            .expect("Resample failed");
        indexing.input_offset += consumed;
        indexing.output_offset += produced;
        remaining = remaining.saturating_sub(consumed);
        if consumed == 0 {
            break;
        }
        on_progress(1.0 - remaining as f32 / in_frames as f32);
    }

    output.truncate(indexing.output_offset * ch);
    output
}

pub fn load_file(path: &Path, project_rate: u32, on_progress: &dyn Fn(f32)) -> Result<Clip, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or("no audio track found")?
        .clone();

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("no sample rate")?;
    let channels = track
        .codec_params
        .channels
        .ok_or("no channel info")?
        .count() as u32;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(e) => return Err(e.into()),
        };

        if packet.track_id() != track.id {
            continue;
        }

        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let duration = decoded.capacity();

        let mut buf = SampleBuffer::<f32>::new(duration as u64, spec);
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
    }

    let (samples, sample_rate) = if sample_rate != project_rate {
        (resample(&samples, channels, sample_rate, project_rate, on_progress), project_rate)
    } else {
        (samples, sample_rate)
    };

    let mono = Clip::build_mono(&samples, channels);
    let summary = Clip::build_summary(&mono);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string();

    Ok(Clip {
        name,
        samples,
        sample_rate,
        channels,
        mono,
        summary,
        offset_secs: 0.0,
        source_path: Some(path.to_string_lossy().into_owned()),
    })
}

pub fn generate_click_track(bpm: f64, duration_secs: f64, sample_rate: u32) -> Clip {
    let interval_samples = (sample_rate as f64 * 60.0 / bpm) as usize;
    let total_samples = (sample_rate as f64 * duration_secs) as usize;
    let click_len = (sample_rate as f64 * 0.015) as usize; // 15ms click

    let mut samples = vec![0.0_f32; total_samples];

    let mut pos = 0;
    while pos < total_samples {
        for i in 0..click_len.min(total_samples - pos) {
            let t = i as f64 / sample_rate as f64;
            let envelope = (-t * 300.0).exp();
            let freq = if pos == 0 || pos % (interval_samples * 4) < interval_samples {
                1500.0 // accented beat
            } else {
                1000.0
            };
            samples[pos + i] = (2.0 * PI * freq * t).sin() as f32 * envelope as f32 * 0.8;
        }
        pos += interval_samples;
    }

    let mono = samples.clone();
    let summary = Clip::build_summary(&mono);

    Clip {
        name: format!("Click — {bpm} BPM"),
        samples,
        sample_rate,
        channels: 1,
        mono,
        summary,
        offset_secs: 0.0,
        source_path: None,
    }
}
