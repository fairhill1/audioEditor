use std::collections::HashMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};

use crate::audio;

#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    pub rate: u32,
    pub tracks: Vec<TrackEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct TrackEntry {
    pub clips: Vec<ClipEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct ClipEntry {
    pub name: String,
    /// Path to WAV file relative to the project directory
    pub file: String,
    /// Original source file path (informational)
    pub source: Option<String>,
    pub offset: f64,
    pub sample_rate: u32,
    pub channels: u32,
}

fn audio_dir_name(ron_path: &Path) -> String {
    let stem = ron_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    format!("{stem}_audio")
}

pub fn save_project(
    ron_path: &Path,
    tracks: &[audio::Track],
    project_rate: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = ron_path.parent().unwrap_or(ron_path);
    let audio_dir = audio_dir_name(ron_path);
    fs::create_dir_all(dir.join(&audio_dir))?;

    // Deduplicate clips by content hash — identical samples share one WAV file
    let mut written: HashMap<u64, String> = HashMap::new();
    let mut wav_counter = 0u32;
    let mut track_entries = Vec::new();

    for track in tracks {
        let mut clip_entries = Vec::new();

        for clip in &track.clips {
            let hash = hash_samples(&clip.samples);
            let rel_path = if let Some(existing) = written.get(&hash) {
                existing.clone()
            } else {
                let filename = format!("{}.wav", wav_counter);
                wav_counter += 1;
                let rel_path = format!("{audio_dir}/{filename}");
                write_wav(&dir.join(&rel_path), &clip.samples, clip.sample_rate, clip.channels)?;
                written.insert(hash, rel_path.clone());
                rel_path
            };

            clip_entries.push(ClipEntry {
                name: clip.name.clone(),
                file: rel_path,
                source: clip.source_path.clone(),
                offset: clip.offset_secs,
                sample_rate: clip.sample_rate,
                channels: clip.channels,
            });
        }

        track_entries.push(TrackEntry { clips: clip_entries });
    }

    let project = ProjectFile {
        rate: project_rate,
        tracks: track_entries,
    };

    let ron_str = ron::ser::to_string_pretty(&project, ron::ser::PrettyConfig::default())?;
    fs::write(ron_path, ron_str)?;

    Ok(())
}

pub fn load_project(ron_path: &Path) -> Result<(Vec<audio::Track>, u32), Box<dyn std::error::Error>> {
    let dir = ron_path.parent().unwrap_or(ron_path);
    let ron_str = fs::read_to_string(ron_path)?;
    let project: ProjectFile = ron::from_str(&ron_str)?;

    let mut tracks = Vec::new();

    for track_entry in &project.tracks {
        let mut clips = Vec::new();

        for clip_entry in &track_entry.clips {
            let wav_path = dir.join(&clip_entry.file);
            let samples = read_wav(&wav_path)?;

            let mono = audio::Clip::build_mono(&samples, clip_entry.channels);
            let summary = audio::Clip::build_summary(&mono);

            clips.push(audio::Clip {
                name: clip_entry.name.clone(),
                samples,
                sample_rate: clip_entry.sample_rate,
                channels: clip_entry.channels,
                mono,
                summary,
                offset_secs: clip_entry.offset,
                source_path: clip_entry.source.clone(),
            });
        }

        tracks.push(audio::Track { clips });
    }

    Ok((tracks, project.rate))
}

fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: u32) -> Result<(), Box<dyn std::error::Error>> {
    let spec = WavSpec {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };

    let mut writer = WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

fn read_wav(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let samples: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>()?;
    Ok(samples)
}

fn hash_samples(samples: &[f32]) -> u64 {
    let mut hasher = DefaultHasher::new();
    let bytes: &[u8] = bytemuck::cast_slice(samples);
    bytes.hash(&mut hasher);
    hasher.finish()
}
