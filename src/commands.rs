use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::{App, PendingLoad};
use crate::{audio, modal, playback, project, undo};

impl App {
    pub(crate) fn rebuild_player(&mut self) {
        let prev = self.player.as_ref().map(|p| (p.is_playing(), p.position_frac()));
        self.player = playback::Player::new(&self.tracks);
        if let (Some((was_playing, frac)), Some(player)) = (prev, &self.player) {
            player.seek_frac(frac);
            if was_playing {
                player.state.playing.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn perform_undo(&mut self) {
        let action = match self.undo_manager.pop_undo() {
            Some(a) => a,
            None => return,
        };
        let reverse = self.apply_undo_action(action);
        self.undo_manager.push_redo(reverse);
        self.rebuild_player();
        self.update_title();
        self.window.as_ref().unwrap().request_redraw();
    }

    pub(crate) fn perform_redo(&mut self) {
        let action = match self.undo_manager.pop_redo() {
            Some(a) => a,
            None => return,
        };
        let reverse = self.apply_undo_action(action);
        self.undo_manager.push_undo(reverse);
        self.rebuild_player();
        self.update_title();
        self.window.as_ref().unwrap().request_redraw();
    }

    /// Apply an undo action and return the reverse action to push onto the opposite stack.
    fn apply_undo_action(&mut self, action: undo::UndoAction) -> undo::UndoAction {
        match action {
            undo::UndoAction::DeleteClip {
                track_idx, clip_idx, clip, track_was_removed,
                prev_sel_track, prev_sel_clip,
            } => {
                // Undo delete: re-insert the clip (and track if it was removed)
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                if track_was_removed {
                    self.tracks.insert(track_idx, audio::Track { clips: vec![], muted: false });
                }
                self.tracks[track_idx].clips.insert(clip_idx, clip.clone());
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                // Reverse: delete it again
                undo::UndoAction::DeleteClip {
                    track_idx, clip_idx, clip,
                    track_was_removed,
                    prev_sel_track: cur_sel_track,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::SplitClip {
                track_idx, clip_idx, original_clip, prev_sel_clip,
            } => {
                // Undo split: remove the two halves and restore original
                let cur_sel_clip = self.selected_clip;
                // The split produced clips at clip_idx and clip_idx+1
                // Save current two halves so redo can re-split
                self.tracks[track_idx].clips.remove(clip_idx);
                self.tracks[track_idx].clips.remove(clip_idx); // was clip_idx+1, now clip_idx
                self.tracks[track_idx].clips.insert(clip_idx, original_clip.clone());
                self.selected_clip = prev_sel_clip;
                // Reverse: split again (store the original we just restored)
                // On redo, we re-split the original clip the same way
                let merged = self.tracks[track_idx].clips[clip_idx].clone();
                undo::UndoAction::SplitClip {
                    track_idx, clip_idx,
                    original_clip: merged,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::CutClip {
                track_idx, clip_idx, clip, prev_clipboard, prev_sel_clip,
            } => {
                // Undo cut: re-insert the clip, restore previous clipboard
                let cur_sel_clip = self.selected_clip;
                let cur_clipboard = std::mem::take(&mut self.clipboard);
                self.tracks[track_idx].clips.insert(clip_idx, clip.clone());
                self.clipboard = prev_clipboard;
                self.selected_track = Some(track_idx);
                self.selected_clip = prev_sel_clip;
                // Reverse: cut it again
                undo::UndoAction::CutClip {
                    track_idx, clip_idx, clip,
                    prev_clipboard: cur_clipboard,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::CreateTrack {
                track_idx, prev_sel_track, prev_sel_clip,
            } => {
                // Undo create: remove the track
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                self.tracks.remove(track_idx);
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                // Reverse: create it again
                undo::UndoAction::CreateTrack {
                    track_idx,
                    prev_sel_track: cur_sel_track,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::MoveClip {
                src_track, src_clip_idx, src_offset,
                dest_track, dest_clip_idx, dest_offset,
                src_track_was_removed,
                prev_sel_track, prev_sel_clip,
            } => {
                // Undo move: move clip back from dest to src
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;

                // If the source track was removed during the original move, re-create it
                let actual_dest_track = if src_track_was_removed && dest_track >= src_track {
                    dest_track // dest_track index was already adjusted
                } else {
                    dest_track
                };

                if actual_dest_track < self.tracks.len()
                    && dest_clip_idx < self.tracks[actual_dest_track].clips.len()
                {
                    let mut clip = self.tracks[actual_dest_track].clips.remove(dest_clip_idx);
                    clip.offset_secs = src_offset;

                    if src_track_was_removed {
                        // Re-insert the source track
                        self.tracks.insert(src_track, audio::Track { clips: vec![clip], muted: false });
                    } else {
                        let insert_at = src_clip_idx.min(self.tracks[src_track].clips.len());
                        self.tracks[src_track].clips.insert(insert_at, clip);
                    }

                    // Clean up empty dest track if the move emptied it
                    let check_dest = if src_track_was_removed && actual_dest_track >= src_track {
                        actual_dest_track + 1
                    } else {
                        actual_dest_track
                    };
                    let reverse_src_removed = if check_dest < self.tracks.len()
                        && self.tracks[check_dest].clips.is_empty()
                    {
                        self.tracks.remove(check_dest);
                        true
                    } else {
                        false
                    };

                    self.selected_track = prev_sel_track;
                    self.selected_clip = prev_sel_clip;

                    // Reverse: move it forward again
                    let new_dest_clip_idx = if src_track_was_removed {
                        0
                    } else {
                        src_clip_idx.min(self.tracks[src_track].clips.len().saturating_sub(1))
                    };
                    undo::UndoAction::MoveClip {
                        src_track: if src_track_was_removed { src_track } else { src_track },
                        src_clip_idx: new_dest_clip_idx,
                        src_offset,
                        dest_track: actual_dest_track,
                        dest_clip_idx,
                        dest_offset,
                        src_track_was_removed: reverse_src_removed,
                        prev_sel_track: cur_sel_track,
                        prev_sel_clip: cur_sel_clip,
                    }
                } else {
                    // Can't apply, return a no-op by recreating the same action
                    undo::UndoAction::MoveClip {
                        src_track, src_clip_idx, src_offset,
                        dest_track, dest_clip_idx, dest_offset,
                        src_track_was_removed,
                        prev_sel_track: cur_sel_track,
                        prev_sel_clip: cur_sel_clip,
                    }
                }
            }
            undo::UndoAction::ImportClip {
                track_idx, clip_idx, created_new_track,
                prev_sel_track, prev_sel_clip,
            } => {
                // Undo import: remove the clip (and track if created)
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                let clip = self.tracks[track_idx].clips.remove(clip_idx);
                if created_new_track && self.tracks[track_idx].clips.is_empty() {
                    self.tracks.remove(track_idx);
                }
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                // Reverse: re-import (re-insert clip)
                // We use DeleteClip to represent "insert this clip back"
                undo::UndoAction::DeleteClip {
                    track_idx,
                    clip_idx,
                    clip,
                    track_was_removed: created_new_track,
                    prev_sel_track: cur_sel_track,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::GenerateClickTrack {
                track_idx, prev_sel_track, prev_sel_clip,
            } => {
                // Undo generate: remove the track
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                let clip = self.tracks[track_idx].clips.remove(0);
                self.tracks.remove(track_idx);
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                // Reverse: re-add the click track
                // Use DeleteClip with track_was_removed to re-create track + clip
                undo::UndoAction::DeleteClip {
                    track_idx,
                    clip_idx: 0,
                    clip,
                    track_was_removed: true,
                    prev_sel_track: cur_sel_track,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::LoadProject {
                prev_tracks, prev_project_path, prev_project_rate,
                prev_sel_track, prev_sel_clip,
            } => {
                let cur_tracks = std::mem::replace(&mut self.tracks, prev_tracks);
                let cur_path = std::mem::replace(&mut self.project_path, prev_project_path);
                let cur_rate = std::mem::replace(&mut self.project_rate, prev_project_rate);
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                self.view_start = 0.0;
                self.view_duration = self.max_duration();
                undo::UndoAction::LoadProject {
                    prev_tracks: cur_tracks,
                    prev_project_path: cur_path,
                    prev_project_rate: cur_rate,
                    prev_sel_track: cur_sel_track,
                    prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::ToggleMute { track_idx } => {
                self.tracks[track_idx].muted = !self.tracks[track_idx].muted;
                undo::UndoAction::ToggleMute { track_idx }
            }
            undo::UndoAction::PasteClips {
                track_idx, prev_clips, prev_sel_clip,
            } => {
                let cur_sel_clip = self.selected_clip;
                let cur_clips = std::mem::replace(&mut self.tracks[track_idx].clips, prev_clips);
                self.selected_clip = prev_sel_clip;
                undo::UndoAction::PasteClips {
                    track_idx, prev_clips: cur_clips, prev_sel_clip: cur_sel_clip,
                }
            }
            undo::UndoAction::CutRegion {
                track_idx, prev_clips, prev_clipboard, prev_sel_clip, prev_selection,
            } => {
                let cur_sel_clip = self.selected_clip;
                let cur_selection = self.selection;
                let cur_clips = std::mem::replace(&mut self.tracks[track_idx].clips, prev_clips);
                let cur_clipboard = std::mem::replace(&mut self.clipboard, prev_clipboard);
                self.selected_track = Some(track_idx);
                self.selected_clip = prev_sel_clip;
                self.selection = prev_selection;
                undo::UndoAction::CutRegion {
                    track_idx, prev_clips: cur_clips, prev_clipboard: cur_clipboard,
                    prev_sel_clip: cur_sel_clip, prev_selection: cur_selection,
                }
            }
            undo::UndoAction::DeleteRegionMulti {
                track_idx, prev_clips, prev_sel_clip, prev_selection,
            } => {
                let cur_sel_clip = self.selected_clip;
                let cur_selection = self.selection;
                let cur_clips = std::mem::replace(&mut self.tracks[track_idx].clips, prev_clips);
                self.selected_track = Some(track_idx);
                self.selected_clip = prev_sel_clip;
                self.selection = prev_selection;
                undo::UndoAction::DeleteRegionMulti {
                    track_idx, prev_clips: cur_clips,
                    prev_sel_clip: cur_sel_clip, prev_selection: cur_selection,
                }
            }
            undo::UndoAction::MoveClips {
                track_idx, moves, prev_sel_track, prev_sel_clip,
            } => {
                let cur_sel_track = self.selected_track;
                let cur_sel_clip = self.selected_clip;
                // Reverse: swap before/after offsets
                let reverse_moves: Vec<(usize, f64, f64)> = moves.iter()
                    .map(|&(idx, before, after)| {
                        if idx < self.tracks[track_idx].clips.len() {
                            self.tracks[track_idx].clips[idx].offset_secs = before;
                        }
                        (idx, after, before)
                    })
                    .collect();
                self.selected_track = prev_sel_track;
                self.selected_clip = prev_sel_clip;
                undo::UndoAction::MoveClips {
                    track_idx, moves: reverse_moves,
                    prev_sel_track: cur_sel_track, prev_sel_clip: cur_sel_clip,
                }
            }
        }
    }

    /// Start loading an audio file from a path (after file dialog completed).
    pub(crate) fn import_file_from_path(&mut self, path: std::path::PathBuf) {
        if self.loading.is_some() {
            return;
        }
        let result: Arc<Mutex<Option<Result<audio::Clip, String>>>> =
            Arc::new(Mutex::new(None));
        let progress = Arc::new(AtomicU8::new(0));

        let result_clone = result.clone();
        let progress_clone = progress.clone();
        let project_rate = self.project_rate;

        std::thread::spawn(move || {
            let on_progress = move |frac: f32| {
                progress_clone.store((frac * 100.0) as u8, Ordering::Relaxed);
            };
            let res = audio::load_file(&path, project_rate, &on_progress)
                .map_err(|e| e.to_string());
            *result_clone.lock().unwrap() = Some(res);
        });

        let clip_offset_secs = self.playhead_secs();

        self.loading = Some(PendingLoad { result, progress, target_track: None, clip_offset_secs });
        self.window.as_ref().unwrap().set_title("Loading…");
    }

    /// Save project to existing path (no dialog). Returns false if no path is set.
    pub(crate) fn save_project_to_existing_path(&mut self) -> bool {
        let path = if let Some(ref path) = self.project_path {
            path.clone()
        } else {
            return false;
        };
        match project::save_project(&path, &self.tracks, self.project_rate) {
            Ok(()) => {
                self.undo_manager.mark_saved();
                self.update_title();
            }
            Err(e) => eprintln!("Failed to save project: {e}"),
        }
        true
    }

    /// Save project to the given path (after save dialog completed).
    pub(crate) fn save_project_to_path(&mut self, path: std::path::PathBuf) {
        self.project_path = Some(path.clone());
        match project::save_project(&path, &self.tracks, self.project_rate) {
            Ok(()) => {
                self.undo_manager.mark_saved();
                self.update_title();
            }
            Err(e) => eprintln!("Failed to save project: {e}"),
        }
    }

    /// Open a project from the given path (after open dialog completed).
    pub(crate) fn open_project_from_path(&mut self, file: std::path::PathBuf) {
        match project::load_project(&file) {
            Ok((tracks, rate)) => {
                let prev_tracks = std::mem::replace(&mut self.tracks, tracks);
                let prev_project_path = std::mem::replace(&mut self.project_path, Some(file));
                let prev_project_rate = std::mem::replace(&mut self.project_rate, rate);
                let prev_sel_track = self.selected_track;
                let prev_sel_clip = self.selected_clip;
                self.undo_manager.push(undo::UndoAction::LoadProject {
                    prev_tracks, prev_project_path, prev_project_rate,
                    prev_sel_track, prev_sel_clip,
                });
                self.selected_track = None;
                self.selected_clip = None;
                self.view_start = 0.0;
                self.view_duration = self.max_duration();
                self.rebuild_player();
                self.undo_manager.mark_saved();
                self.update_title();
                self.window.as_ref().unwrap().request_redraw();
            }
            Err(e) => eprintln!("Failed to load project: {e}"),
        }
    }

    pub(crate) fn poll_loading(&mut self) {
        let done = if let Some(pending) = &self.loading {
            let lock = pending.result.lock().unwrap();
            if let Some(res) = &*lock {
                match res {
                    Ok(_) => true,
                    Err(e) => {
                        eprintln!("Failed to load audio: {e}");
                        true
                    }
                }
            } else {
                // Still loading — update title with progress
                let pct = pending.progress.load(Ordering::Relaxed);
                self.window.as_ref().unwrap().set_title(&format!("Resampling… {pct}%"));
                false
            }
        } else {
            return;
        };

        if done {
            let pending = self.loading.take().unwrap();
            let res = pending.result.lock().unwrap().take().unwrap();
            if let Ok(mut clip) = res {
                clip.offset_secs = pending.clip_offset_secs;

                let prev_sel_track = self.selected_track;
                let prev_sel_clip = self.selected_clip;

                if let Some(track_idx) = pending.target_track {
                    // Add clip to existing track
                    if track_idx < self.tracks.len() {
                        let clip_idx = self.tracks[track_idx].clips.len();
                        self.tracks[track_idx].clips.push(clip);
                        self.undo_manager.push(undo::UndoAction::ImportClip {
                            track_idx, clip_idx, created_new_track: false,
                            prev_sel_track, prev_sel_clip,
                        });
                        self.selected_clip = Some(clip_idx);
                        self.selected_track = Some(track_idx);
                    }
                } else {
                    // Create a new track with this clip
                    self.tracks.push(audio::Track {
                        clips: vec![clip],
                        muted: false,
                    });
                    let track_idx = self.tracks.len() - 1;
                    self.undo_manager.push(undo::UndoAction::ImportClip {
                        track_idx, clip_idx: 0, created_new_track: true,
                        prev_sel_track, prev_sel_clip,
                    });
                    self.selected_track = Some(track_idx);
                    self.selected_clip = Some(0);
                }

                // Only reset zoom if this is the first clip in the project
                if self.tracks.iter().map(|t| t.clips.len()).sum::<usize>() == 1 {
                    self.view_duration = self.max_duration();
                    self.view_start = 0.0;
                }
                self.rebuild_player();
            }
            self.update_title();
            self.window.as_ref().unwrap().request_redraw();
        }
    }

    pub(crate) fn update_title(&self) {
        let project_name = self.project_path.as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled");
        let dirty = if self.undo_manager.is_dirty() { " *" } else { "" };
        let title = if self.tracks.is_empty() {
            format!("Audio Editor — {project_name}{dirty}")
        } else {
            let rate_khz = self.project_rate as f64 / 1000.0;
            format!("Audio Editor — {project_name}{dirty} — {} track(s) — {rate_khz:.1}kHz", self.tracks.len())
        };
        self.window.as_ref().unwrap().set_title(&title);
    }

    /// Export the project as a stereo WAV file by mixing down all non-muted tracks.
    pub(crate) fn export_wav_to_path(&mut self, path: std::path::PathBuf) {
        let channels = 2_u32;
        let sample_rate = self.project_rate;
        let max_dur = self.max_duration();
        if max_dur <= 0.0 {
            return;
        }
        let total_frames = (max_dur * sample_rate as f64).ceil() as usize;
        let mut mix = vec![0.0_f32; total_frames * channels as usize];

        for track in &self.tracks {
            if track.muted {
                continue;
            }
            for clip in &track.clips {
                let frame_offset = (clip.offset_secs * sample_rate as f64) as usize;
                let clip_frames = clip.mono.len(); // mono.len() == number of frames
                let clip_ch = clip.channels as usize;

                for f in 0..clip_frames {
                    let out_f = frame_offset + f;
                    if out_f >= total_frames {
                        break;
                    }
                    if clip_ch == 1 {
                        // Mono clip: copy to both channels
                        let s = clip.samples[f];
                        mix[out_f * 2] += s;
                        mix[out_f * 2 + 1] += s;
                    } else {
                        // Multi-channel: take first two channels (or duplicate if mono)
                        let si = f * clip_ch;
                        mix[out_f * 2] += clip.samples[si];
                        mix[out_f * 2 + 1] += clip.samples.get(si + 1).copied().unwrap_or(clip.samples[si]);
                    }
                }
            }
        }

        let spec = hound::WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        match hound::WavWriter::create(&path, spec) {
            Ok(mut writer) => {
                for &s in &mix {
                    if writer.write_sample(s).is_err() {
                        eprintln!("Failed to write sample during export");
                        return;
                    }
                }
                if let Err(e) = writer.finalize() {
                    eprintln!("Failed to finalize WAV export: {e}");
                } else {
                    eprintln!("Exported to {}", path.display());
                }
            }
            Err(e) => eprintln!("Failed to create WAV file: {e}"),
        }
    }

    pub(crate) fn handle_modal_result(&mut self, result: modal::ModalResult) {
        match result {
            modal::ModalResult::ClickTrackBpm(bpm) => {
                let prev_sel_track = self.selected_track;
                let prev_sel_clip = self.selected_clip;
                let dur = if self.max_duration() > 0.0 { self.max_duration() } else { 30.0 };
                let clip = audio::generate_click_track(bpm, dur, self.project_rate);
                self.tracks.push(audio::Track {
                    clips: vec![clip],
                    muted: false,
                });
                let track_idx = self.tracks.len() - 1;
                self.undo_manager.push(undo::UndoAction::GenerateClickTrack {
                    track_idx, prev_sel_track, prev_sel_clip,
                });
                self.view_duration = self.max_duration();
                self.view_start = 0.0;
                self.rebuild_player();
                self.update_title();
            }
        }
    }
}
