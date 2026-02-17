use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct AudioTrack {
    pub name: String,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u32,
    /// Pre-computed mono mixdown for display
    pub mono: Vec<f32>,
    /// Pre-computed min/max summary at fixed bucket size for fast rendering
    pub summary: Vec<(f32, f32)>,
}

/// Samples per summary bucket — tune for a good balance of detail vs speed
const SUMMARY_BUCKET: usize = 256;

impl AudioTrack {
    pub fn duration_secs(&self) -> f64 {
        self.mono.len() as f64 / self.sample_rate as f64
    }

    fn build_mono(samples: &[f32], channels: u32) -> Vec<f32> {
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

    fn build_summary(mono: &[f32]) -> Vec<(f32, f32)> {
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

pub fn load_file(path: &Path) -> Result<AudioTrack, Box<dyn std::error::Error>> {
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

    let mono = AudioTrack::build_mono(&samples, channels);
    let summary = AudioTrack::build_summary(&mono);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string();

    Ok(AudioTrack {
        name,
        samples,
        sample_rate,
        channels,
        mono,
        summary,
    })
}
