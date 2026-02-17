use std::sync::Arc;

use glyphon::Resolution;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::app::{App, DragState, DRAG_THRESHOLD_PX};
use crate::{audio, modal, undo};

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Audio Editor");
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        self.init_wgpu(window);

        #[cfg(target_os = "macos")]
        crate::setup_macos_edit_menu();
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.poll_loading();
        if self.loading.is_some() {
            self.window.as_ref().unwrap().request_redraw();
        } else if let Some(player) = &self.player {
            if player.is_playing() {
                self.window.as_ref().unwrap().request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Modal input handling — intercepts all keyboard input when modal is open
        if self.modal.as_ref().is_some_and(|m| m.visible) {
            match &event {
                WindowEvent::KeyboardInput { event: key_event, .. }
                    if key_event.state == ElementState::Pressed =>
                {
                    match key_event.physical_key {
                        PhysicalKey::Code(KeyCode::Escape) => {
                            self.modal.as_mut().unwrap().cancel();
                            self.modal = None;
                            self.window.as_ref().unwrap().request_redraw();
                        }
                        PhysicalKey::Code(KeyCode::Backspace) => {
                            self.modal.as_mut().unwrap().handle_backspace();
                            self.window.as_ref().unwrap().request_redraw();
                        }
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => {
                            // Fake a newline char to trigger confirm
                            if let Some(result) = self.modal.as_mut().unwrap().handle_char('\n') {
                                self.modal = None;
                                self.handle_modal_result(result);
                            }
                            self.window.as_ref().unwrap().request_redraw();
                        }
                        _ => {
                            if let Some(ref text) = key_event.text {
                                for c in text.chars() {
                                    if let Some(result) = self.modal.as_mut().unwrap().handle_char(c) {
                                        self.modal = None;
                                        self.handle_modal_result(result);
                                        break;
                                    }
                                }
                                self.window.as_ref().unwrap().request_redraw();
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }
            // Still handle essential events while modal is open
            match event {
                WindowEvent::ModifiersChanged(mods) => {
                    self.modifiers = mods.state();
                }
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(new_size) => {
                    if let (Some(surface), Some(device), Some(config)) =
                        (&self.surface, &self.device, &mut self.config)
                    {
                        config.width = new_size.width.max(1);
                        config.height = new_size.height.max(1);
                        surface.configure(device, config);
                    }
                    self.window.as_ref().unwrap().request_redraw();
                }
                WindowEvent::RedrawRequested => self.render(),
                _ => {}
            }
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            // Cmd+G: Generate click track
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyG)
                    && self.modifiers.super_key() =>
            {
                self.modal = Some(modal::Modal::new("BPM", modal::ModalKind::ClickTrackBpm));
                self.window.as_ref().unwrap().request_redraw();
            }
            // Cmd+O: Open project
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyO)
                    && self.modifiers.super_key() =>
            {
                self.open_project();
            }
            // Cmd+I: Import audio file
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyI)
                    && self.modifiers.super_key() =>
            {
                self.open_file();
            }
            // Cmd+S: Save project
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyS)
                    && self.modifiers.super_key()
                    && !self.modifiers.shift_key() =>
            {
                self.save_project();
            }
            // Cmd+Shift+S: Save project as
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyS)
                    && self.modifiers.super_key()
                    && self.modifiers.shift_key() =>
            {
                self.save_project_as();
            }
            // Cmd+Shift+Z: Redo
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyZ)
                    && self.modifiers.super_key()
                    && self.modifiers.shift_key() =>
            {
                self.perform_redo();
            }
            // Cmd+Z: Undo
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyZ)
                    && self.modifiers.super_key()
                    && !self.modifiers.shift_key() =>
            {
                self.perform_undo();
            }
            // Cmd+C: Copy selected clip (or selection region)
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyC)
                    && self.modifiers.super_key() =>
            {
                if let (Some((s0, s1)), Some(track_idx), Some(clip_idx)) =
                    (self.selection, self.selected_track, self.selected_clip)
                {
                    // Copy selection region from selected clip
                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        let clip = &self.tracks[track_idx].clips[clip_idx];
                        let rel_start = (s0 - clip.offset_secs).max(0.0);
                        let rel_end = (s1 - clip.offset_secs).min(clip.duration_secs());
                        if rel_end > rel_start {
                            let mut sliced = clip.slice(rel_start, rel_end);
                            sliced.offset_secs = 0.0;
                            self.clipboard = Some(sliced);
                        }
                    }
                } else if let (Some(track_idx), Some(clip_idx)) = (self.selected_track, self.selected_clip) {
                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        self.clipboard = Some(self.tracks[track_idx].clips[clip_idx].clone());
                    }
                }
            }
            // Cmd+X: Cut selected clip (or selection region)
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyX)
                    && self.modifiers.super_key() =>
            {
                if let (Some((s0, s1)), Some(track_idx), Some(clip_idx)) =
                    (self.selection, self.selected_track, self.selected_clip)
                {
                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        let clip = &self.tracks[track_idx].clips[clip_idx];
                        let rel_start = (s0 - clip.offset_secs).max(0.0);
                        let rel_end = (s1 - clip.offset_secs).min(clip.duration_secs());
                        if rel_end > rel_start {
                            let prev_clipboard = self.clipboard.clone();
                            let prev_sel_clip = self.selected_clip;
                            let original_clip = self.tracks[track_idx].clips[clip_idx].clone();

                            // Copy the selection to clipboard
                            let mut sliced = clip.slice(rel_start, rel_end);
                            sliced.offset_secs = 0.0;

                            // Remove the region — replace original clip with left + right pieces
                            let (left, right) = clip.remove_region(rel_start, rel_end);
                            self.tracks[track_idx].clips.remove(clip_idx);
                            let mut insert_at = clip_idx;
                            if let Some(l) = left {
                                self.tracks[track_idx].clips.insert(insert_at, l);
                                insert_at += 1;
                            }
                            if let Some(r) = right {
                                self.tracks[track_idx].clips.insert(insert_at, r);
                            }

                            // Store original clip for undo — use CutClip with the whole original
                            self.undo_manager.push(undo::UndoAction::CutClip {
                                track_idx, clip_idx, clip: original_clip,
                                prev_clipboard, prev_sel_clip,
                            });
                            self.clipboard = Some(sliced);
                            self.selection = None;

                            if self.tracks[track_idx].clips.is_empty() {
                                self.selected_clip = None;
                            } else {
                                self.selected_clip = Some(clip_idx.min(self.tracks[track_idx].clips.len() - 1));
                            }
                            self.rebuild_player();
                            self.window.as_ref().unwrap().request_redraw();
                        }
                    }
                } else if let (Some(track_idx), Some(clip_idx)) = (self.selected_track, self.selected_clip) {
                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        let prev_clipboard = self.clipboard.clone();
                        let prev_sel_clip = self.selected_clip;
                        let clip = self.tracks[track_idx].clips.remove(clip_idx);
                        self.undo_manager.push(undo::UndoAction::CutClip {
                            track_idx, clip_idx, clip: clip.clone(),
                            prev_clipboard, prev_sel_clip,
                        });
                        self.clipboard = Some(clip);
                        if self.tracks[track_idx].clips.is_empty() {
                            self.selected_clip = None;
                        } else {
                            self.selected_clip = Some(clip_idx.min(self.tracks[track_idx].clips.len() - 1));
                        }
                        self.rebuild_player();
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            // Cmd+V: Paste clip at playhead on selected track
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyV)
                    && self.modifiers.super_key() =>
            {
                if let (Some(clip), Some(track_idx)) = (&self.clipboard, self.selected_track) {
                    if track_idx < self.tracks.len() {
                        let mut new_clip = clip.clone();
                        new_clip.offset_secs = self.playhead_secs();
                        let clip_dur = new_clip.duration_secs();

                        // Check for overlap with existing clips
                        let overlaps = self.tracks[track_idx].clips.iter().any(|c| {
                            let c_start = c.offset_secs;
                            let c_end = c_start + c.duration_secs();
                            let n_start = new_clip.offset_secs;
                            let n_end = n_start + clip_dur;
                            n_start < c_end && n_end > c_start
                        });

                        if !overlaps {
                            let prev_sel_clip = self.selected_clip;
                            let new_idx = self.tracks[track_idx].clips.len();
                            self.tracks[track_idx].clips.push(new_clip);
                            self.undo_manager.push(undo::UndoAction::PasteClip {
                                track_idx, clip_idx: new_idx, prev_sel_clip,
                            });
                            self.selected_clip = Some(new_idx);
                            self.rebuild_player();
                            self.window.as_ref().unwrap().request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyT)
                    && self.modifiers.super_key() =>
            {
                // Insert new empty track below the selected track (or at the end)
                let prev_sel_track = self.selected_track;
                let prev_sel_clip = self.selected_clip;
                let insert_at = self.selected_track.map_or(self.tracks.len(), |i| i + 1);
                self.tracks.insert(insert_at, audio::Track { clips: vec![], muted: false });
                self.undo_manager.push(undo::UndoAction::CreateTrack {
                    track_idx: insert_at, prev_sel_track, prev_sel_clip,
                });
                self.selected_track = Some(insert_at);
                self.selected_clip = None;
                self.rebuild_player();
                self.update_title();
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyS)
                    && !self.modifiers.super_key() =>
            {
                // Split selected clip at the playhead
                if let (Some(track_idx), Some(clip_idx)) = (self.selected_track, self.selected_clip) {
                    let playhead = self.playhead_secs();
                    let clip = &self.tracks[track_idx].clips[clip_idx];
                    let clip_start = clip.offset_secs;
                    let clip_end = clip_start + clip.duration_secs();
                    // Only split if playhead is strictly inside the clip
                    if playhead > clip_start && playhead < clip_end {
                        let original_clip = self.tracks[track_idx].clips[clip_idx].clone();
                        let prev_sel_clip = self.selected_clip;
                        let split_at = playhead - clip_start;
                        let right = self.tracks[track_idx].clips[clip_idx].split_at(split_at);
                        self.tracks[track_idx].clips.insert(clip_idx + 1, right);
                        self.undo_manager.push(undo::UndoAction::SplitClip {
                            track_idx, clip_idx, original_clip, prev_sel_clip,
                        });
                        // Select the right half
                        self.selected_clip = Some(clip_idx + 1);
                        self.rebuild_player();
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Backspace)
                    && !self.tracks.is_empty() =>
            {
                if let (Some(track_idx), Some(clip_idx)) = (self.selected_track, self.selected_clip) {
                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        let prev_sel_track = self.selected_track;
                        let prev_sel_clip = self.selected_clip;
                        let clip = self.tracks[track_idx].clips.remove(clip_idx);
                        // Remove track if it's now empty
                        let track_was_removed = self.tracks[track_idx].clips.is_empty();
                        if track_was_removed {
                            self.tracks.remove(track_idx);
                            if self.tracks.is_empty() {
                                self.selected_track = None;
                                self.selected_clip = None;
                            } else {
                                self.selected_track = Some(track_idx.min(self.tracks.len() - 1));
                                self.selected_clip = None;
                            }
                        } else {
                            // Select previous clip or first clip
                            self.selected_clip = Some(clip_idx.min(self.tracks[track_idx].clips.len() - 1));
                        }
                        self.undo_manager.push(undo::UndoAction::DeleteClip {
                            track_idx, clip_idx, clip, track_was_removed,
                            prev_sel_track, prev_sel_clip,
                        });
                        // Clamp view to new duration without resetting zoom
                        let max_dur = self.max_duration();
                        if self.view_duration > max_dur {
                            self.view_duration = max_dur;
                        }
                        self.view_start = self.view_start.clamp(0.0, (max_dur - self.view_duration).max(0.0));
                        self.rebuild_player();
                        self.update_title();
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            // Escape: clear selection
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Escape) =>
            {
                if self.selection.is_some() {
                    self.selection = None;
                    self.selecting = false;
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Space) =>
            {
                if let Some(player) = &self.player {
                    let max_dur = self.max_duration();
                    if player.is_playing() {
                        player.toggle();
                    } else {
                        // If there's a selection, seek to start and set end
                        if let Some((s0, s1)) = self.selection {
                            player.seek_to_secs(s0, max_dur);
                            player.set_end_secs(s1, max_dur);
                        } else {
                            player.set_end_secs(0.0, max_dur);
                        }
                        player.toggle();
                    }
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            // M: Toggle mute on selected track
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyM)
                    && !self.modifiers.super_key() =>
            {
                if let Some(track_idx) = self.selected_track {
                    if track_idx < self.tracks.len() {
                        self.tracks[track_idx].muted = !self.tracks[track_idx].muted;
                        self.rebuild_player();
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && matches!(
                        event.physical_key,
                        PhysicalKey::Code(KeyCode::ArrowLeft) | PhysicalKey::Code(KeyCode::ArrowRight)
                    ) =>
            {
                if let Some(player) = &self.player {
                    let max_dur = self.max_duration();
                    if max_dur > 0.0 {
                        let view = self.effective_view_duration();
                        let step = if self.modifiers.shift_key() { view * 0.01 } else { view * 0.05 };
                        let dir = if event.physical_key == PhysicalKey::Code(KeyCode::ArrowLeft) { -1.0 } else { 1.0 };
                        let cur_secs = player.position_frac() * max_dur;
                        let new_secs = (cur_secs + dir * step).clamp(0.0, max_dur);
                        player.seek_frac(new_secs / max_dur);
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_x = position.x;
                self.cursor_y = position.y;

                // Update selection end while dragging out a selection
                if self.selecting {
                    let secs = self.px_to_secs(position.x);
                    if let Some(sel) = &mut self.selection {
                        sel.1 = secs;
                    }
                    self.window.as_ref().unwrap().request_redraw();
                }

                // Handle clip dragging
                if let Some(drag) = &mut self.dragging {
                    if !drag.active {
                        let dx = position.x - drag.start_x;
                        let dy = position.y - drag.start_y;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist >= DRAG_THRESHOLD_PX {
                            drag.active = true;
                        }
                    }
                }

                // Update cursor icon
                if self.dragging.as_ref().is_some_and(|d| d.active) {
                    self.window.as_ref().unwrap().set_cursor(CursorIcon::Grabbing);
                } else if self.hit_test_title_bar(position.x, position.y).is_some() {
                    self.window.as_ref().unwrap().set_cursor(CursorIcon::Grab);
                } else {
                    self.window.as_ref().unwrap().set_cursor(CursorIcon::Text);
                }

                if self.dragging.as_ref().is_some_and(|d| d.active) {
                    let config = self.config.as_ref().unwrap();
                    let view_duration = self.effective_view_duration();
                    let num_tracks = self.tracks.len();

                    let dx_px = position.x - self.dragging.as_ref().unwrap().start_x;
                    let dx_secs = dx_px / config.width as f64 * view_duration;
                    let desired_offset = (self.dragging.as_ref().unwrap().start_offset + dx_secs).max(0.0);

                    // Determine target track from cursor Y
                    let lane_height = config.height as f64 / num_tracks as f64;
                    let target_track = ((position.y / lane_height) as usize).min(num_tracks - 1);

                    let current_track = self.dragging.as_ref().unwrap().current_track_idx;
                    let clip_idx = self.dragging.as_ref().unwrap().clip_idx;

                    // Move clip between tracks if needed
                    let clip_idx = if target_track != current_track
                        && current_track < self.tracks.len()
                        && clip_idx < self.tracks[current_track].clips.len()
                    {
                        let clip = self.tracks[current_track].clips.remove(clip_idx);
                        self.tracks[target_track].clips.push(clip);
                        let new_idx = self.tracks[target_track].clips.len() - 1;
                        let drag = self.dragging.as_mut().unwrap();
                        drag.current_track_idx = target_track;
                        drag.clip_idx = new_idx;
                        self.selected_track = Some(target_track);
                        self.selected_clip = Some(new_idx);
                        new_idx
                    } else {
                        clip_idx
                    };

                    let track_idx = self.dragging.as_ref().unwrap().current_track_idx;

                    if track_idx < self.tracks.len() && clip_idx < self.tracks[track_idx].clips.len() {
                        let clip_dur = self.tracks[track_idx].clips[clip_idx].duration_secs();

                        // Snap to nearby clip edges
                        let snapped = self.snap_offset(track_idx, clip_idx, desired_offset, clip_dur);

                        // Prevent overlap
                        let final_offset = self.clamp_no_overlap(track_idx, clip_idx, snapped, clip_dur);

                        self.tracks[track_idx].clips[clip_idx].offset_secs = final_offset;
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let Some((track_idx, clip_idx)) = self.hit_test_title_bar(self.cursor_x, self.cursor_y) {
                    // Title bar click → prepare clip drag
                    self.selection = None;
                    self.selecting = false;
                    self.selected_track = Some(track_idx);
                    self.selected_clip = Some(clip_idx);

                    let clip = &self.tracks[track_idx].clips[clip_idx];
                    self.dragging = Some(DragState {
                        clip_idx,
                        start_offset: clip.offset_secs,
                        start_x: self.cursor_x,
                        start_y: self.cursor_y,
                        source_track_idx: track_idx,
                        current_track_idx: track_idx,
                        active: false,
                        source_clip_idx: clip_idx,
                        prev_selected_track: self.selected_track,
                        prev_selected_clip: self.selected_clip,
                    });
                } else {
                    // Click on waveform body or empty area → start selection
                    let click_secs = self.px_to_secs(self.cursor_x);
                    self.selection = Some((click_secs, click_secs));
                    self.selecting = true;
                    self.dragging = None;

                    // Select track from Y position
                    if !self.tracks.is_empty() {
                        if let Some(config) = &self.config {
                            let track_idx = (self.cursor_y / config.height as f64 * self.tracks.len() as f64) as usize;
                            self.selected_track = Some(track_idx.min(self.tracks.len() - 1));
                        }
                    }
                    // Select clip if clicking on one (but not via title bar)
                    if let Some((track_idx, clip_idx)) = self.hit_test_clip(self.cursor_x, self.cursor_y) {
                        self.selected_track = Some(track_idx);
                        self.selected_clip = Some(clip_idx);
                    } else {
                        self.selected_clip = None;
                    }

                    // Seek playhead to click position
                    if let Some(player) = &self.player {
                        let max_dur = self.max_duration();
                        if max_dur > 0.0 {
                            player.seek_frac(click_secs / max_dur);
                        }
                    }
                }
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                // Finalize selection
                if self.selecting {
                    self.selecting = false;
                    // Use pixel distance to decide if it was a real drag or just a click
                    if let (Some((a, b)), Some(config)) = (self.selection, self.config.as_ref()) {
                        let px_a = (a - self.view_start) / self.effective_view_duration() * config.width as f64;
                        let px_b = (b - self.view_start) / self.effective_view_duration() * config.width as f64;
                        if (px_b - px_a).abs() < DRAG_THRESHOLD_PX {
                            self.selection = None;
                        } else {
                            let (start, end) = if a <= b { (a, b) } else { (b, a) };
                            self.selection = Some((start, end));
                        }
                    }
                    self.window.as_ref().unwrap().request_redraw();
                }

                if let Some(drag) = self.dragging.take() {
                    if drag.active {
                        let mut sel_track = drag.current_track_idx;
                        let sel_clip = drag.clip_idx;
                        let dest_offset = self.tracks[sel_track].clips[sel_clip].offset_secs;
                        // Remove the source track only if the drag emptied it
                        let src = drag.source_track_idx;
                        let src_track_was_removed = src < self.tracks.len() && self.tracks[src].clips.is_empty();
                        if src_track_was_removed {
                            self.tracks.remove(src);
                            if src < sel_track {
                                sel_track -= 1;
                            }
                        }
                        // Only push undo if something actually changed
                        let moved = drag.source_track_idx != sel_track
                            || drag.start_offset != dest_offset
                            || src_track_was_removed;
                        if moved {
                            self.undo_manager.push(undo::UndoAction::MoveClip {
                                src_track: drag.source_track_idx,
                                src_clip_idx: drag.source_clip_idx,
                                src_offset: drag.start_offset,
                                dest_track: sel_track,
                                dest_clip_idx: sel_clip,
                                dest_offset,
                                src_track_was_removed,
                                prev_sel_track: drag.prev_selected_track,
                                prev_sel_clip: drag.prev_selected_clip,
                            });
                        }
                        if self.tracks.is_empty() {
                            self.selected_track = None;
                            self.selected_clip = None;
                        } else {
                            self.selected_track = Some(sel_track);
                            self.selected_clip = Some(sel_clip);
                        }
                        self.rebuild_player();
                        self.update_title();
                    }
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            // Horizontal scroll: two-finger trackpad swipe / shift+wheel
            WindowEvent::MouseWheel { delta, .. } if !self.modifiers.super_key() => {
                if !self.tracks.is_empty() {
                    let max_dur = self.max_duration();
                    let view_dur = self.effective_view_duration();

                    let (dx, dy) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => (x as f64 * 40.0, y as f64 * 40.0),
                    };

                    // Horizontal scroll amount as fraction of view
                    let scroll = if dx.abs() > 0.0 { -dx } else if self.modifiers.shift_key() { -dy } else { 0.0 };
                    if scroll != 0.0 {
                        let shift = scroll / self.config.as_ref().map_or(1.0, |c| c.width as f64) * view_dur;
                        self.view_start = (self.view_start + shift).clamp(0.0, (max_dur - view_dur).max(0.0));
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            // Cmd+scroll to zoom
            WindowEvent::MouseWheel { delta, .. } if self.modifiers.super_key() => {
                if !self.tracks.is_empty() {
                    let max_dur = self.max_duration();
                    let view_dur = self.effective_view_duration();

                    let dy = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y,
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64 * 40.0,
                    };

                    let zoom_factor = 1.0 + dy * 0.005;
                    let new_dur = (view_dur / zoom_factor).clamp(0.01, max_dur);

                    // Zoom toward cursor
                    let cursor_frac = self.cursor_x / self.config.as_ref().map_or(1.0, |c| c.width as f64);
                    let time_at_cursor = self.view_start + cursor_frac * view_dur;
                    self.view_duration = new_dur;
                    self.view_start = (time_at_cursor - cursor_frac * new_dur).clamp(0.0, (max_dur - new_dur).max(0.0));
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            // Pinch-to-zoom (macOS trackpad)
            WindowEvent::PinchGesture { delta, .. } => {
                if !self.tracks.is_empty() {
                    let max_dur = self.max_duration();
                    let view_dur = self.effective_view_duration();

                    let zoom_factor = 1.0 + delta;
                    let new_dur = (view_dur / zoom_factor).clamp(0.01, max_dur);

                    let cursor_frac = self.cursor_x / self.config.as_ref().map_or(1.0, |c| c.width as f64);
                    let time_at_cursor = self.view_start + cursor_frac * view_dur;
                    self.view_duration = new_dur;
                    self.view_start = (time_at_cursor - cursor_frac * new_dur).clamp(0.0, (max_dur - new_dur).max(0.0));
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            WindowEvent::Resized(new_size) => {
                if let (Some(surface), Some(device), Some(config)) =
                    (&self.surface, &self.device, &mut self.config)
                {
                    config.width = new_size.width.max(1);
                    config.height = new_size.height.max(1);
                    surface.configure(device, config);
                }
                if let (Some(viewport), Some(queue), Some(config)) =
                    (&mut self.viewport, &self.queue, &self.config)
                {
                    viewport.update(
                        queue,
                        Resolution {
                            width: config.width,
                            height: config.height,
                        },
                    );
                }
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.render();
            }
            _ => {}
        }
    }
}
