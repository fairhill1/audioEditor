use std::path::PathBuf;

use crate::audio;

pub enum UndoAction {
    DeleteClip {
        track_idx: usize,
        clip_idx: usize,
        clip: audio::Clip,
        track_was_removed: bool,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
    SplitClip {
        track_idx: usize,
        clip_idx: usize,
        original_clip: audio::Clip,
        prev_sel_clip: Option<usize>,
    },
    CutClip {
        track_idx: usize,
        clip_idx: usize,
        clip: audio::Clip,
        prev_clipboard: Option<audio::Clip>,
        prev_sel_clip: Option<usize>,
    },
    DeleteRegion {
        track_idx: usize,
        clip_idx: usize,
        original_clip: audio::Clip,
        num_pieces: usize,
        prev_sel_clip: Option<usize>,
        prev_selection: Option<(f64, f64)>,
    },
    PasteClip {
        track_idx: usize,
        clip_idx: usize,
        prev_sel_clip: Option<usize>,
    },
    CreateTrack {
        track_idx: usize,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
    MoveClip {
        src_track: usize,
        src_clip_idx: usize,
        src_offset: f64,
        dest_track: usize,
        dest_clip_idx: usize,
        dest_offset: f64,
        src_track_was_removed: bool,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
    ImportClip {
        track_idx: usize,
        clip_idx: usize,
        created_new_track: bool,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
    GenerateClickTrack {
        track_idx: usize,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
    LoadProject {
        prev_tracks: Vec<audio::Track>,
        prev_project_path: Option<PathBuf>,
        prev_project_rate: u32,
        prev_sel_track: Option<usize>,
        prev_sel_clip: Option<usize>,
    },
}

pub struct UndoManager {
    undo_stack: Vec<UndoAction>,
    redo_stack: Vec<UndoAction>,
    max_entries: usize,
    save_point: usize,
}

impl UndoManager {
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_entries,
            save_point: 0,
        }
    }

    pub fn push(&mut self, action: UndoAction) {
        self.redo_stack.clear();
        self.undo_stack.push(action);
        if self.undo_stack.len() > self.max_entries {
            self.undo_stack.remove(0);
        }
    }

    pub fn pop_undo(&mut self) -> Option<UndoAction> {
        self.undo_stack.pop()
    }

    pub fn push_redo(&mut self, action: UndoAction) {
        self.redo_stack.push(action);
    }

    pub fn pop_redo(&mut self) -> Option<UndoAction> {
        self.redo_stack.pop()
    }

    pub fn push_undo(&mut self, action: UndoAction) {
        self.undo_stack.push(action);
        if self.undo_stack.len() > self.max_entries {
            self.undo_stack.remove(0);
            // save_point was before the dropped entry — can never return to it
            self.save_point = self.save_point.saturating_sub(1);
        }
    }

    pub fn mark_saved(&mut self) {
        self.save_point = self.undo_stack.len();
    }

    pub fn is_dirty(&self) -> bool {
        self.undo_stack.len() != self.save_point
    }
}
