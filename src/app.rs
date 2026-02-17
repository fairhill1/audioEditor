use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, Mutex};

use glyphon::{Cache, FontSystem, SwashCache, TextAtlas, TextRenderer, Viewport};
use winit::keyboard::ModifiersState;
use winit::window::Window;

use crate::{audio, modal, playback, undo};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Vertex {
    pub(crate) position: [f32; 2],
    pub(crate) color: [f32; 3],
}

pub(crate) fn push_quad(vertices: &mut Vec<Vertex>, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 3]) {
    vertices.extend_from_slice(&[
        Vertex { position: [x0, y1], color },
        Vertex { position: [x1, y1], color },
        Vertex { position: [x0, y0], color },
        Vertex { position: [x0, y0], color },
        Vertex { position: [x1, y1], color },
        Vertex { position: [x1, y0], color },
    ]);
}

pub(crate) struct PendingLoad {
    pub(crate) result: Arc<Mutex<Option<Result<audio::Clip, String>>>>,
    pub(crate) progress: Arc<AtomicU8>,
    /// Which track to add the clip to, or None to create a new track
    pub(crate) target_track: Option<usize>,
    /// Timeline position for the new clip
    pub(crate) clip_offset_secs: f64,
}

pub(crate) struct DragState {
    pub(crate) clip_idx: usize,
    pub(crate) start_offset: f64,
    pub(crate) start_x: f64,
    pub(crate) start_y: f64,
    pub(crate) source_track_idx: usize,
    pub(crate) current_track_idx: usize,
    pub(crate) active: bool,
    pub(crate) source_clip_idx: usize,
    pub(crate) prev_selected_track: Option<usize>,
    pub(crate) prev_selected_clip: Option<usize>,
    /// Group drag: (clip_idx, original_offset) for all clips being dragged together.
    /// Empty means single-clip drag.
    pub(crate) group: Vec<(usize, f64)>,
}

pub(crate) const DRAG_THRESHOLD_PX: f64 = 4.0;
pub(crate) const SELECTION_EDGE_PX: f64 = 6.0;

#[derive(Clone, Copy)]
pub(crate) enum SelectionEdge {
    Left,
    Right,
}

/// Actions that open native dialogs. These are spawned on a background thread
/// to avoid winit's re-entrant event handling panic on macOS (the rfd modal
/// dialog pumps its own event loop, which can deliver events while winit's
/// handle_event is still on the stack).
pub(crate) enum DeferredAction {
    OpenFile,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    ExportWav,
}

/// A native file dialog running on a background thread.
pub(crate) struct PendingDialog {
    pub(crate) result: Arc<Mutex<Option<Option<PathBuf>>>>,
    pub(crate) action: DeferredAction,
}

pub(crate) struct App {
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) surface: Option<wgpu::Surface<'static>>,
    pub(crate) device: Option<wgpu::Device>,
    pub(crate) queue: Option<wgpu::Queue>,
    pub(crate) config: Option<wgpu::SurfaceConfiguration>,
    pub(crate) pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) tracks: Vec<audio::Track>,
    pub(crate) project_path: Option<PathBuf>,
    pub(crate) player: Option<playback::Player>,
    pub(crate) modifiers: ModifiersState,
    pub(crate) cursor_x: f64,
    pub(crate) cursor_y: f64,
    pub(crate) selected_track: Option<usize>,
    pub(crate) selected_clip: Option<usize>,
    // Horizontal zoom/scroll state
    pub(crate) view_start: f64,
    pub(crate) view_duration: f64,
    pub(crate) modal: Option<modal::Modal>,
    pub(crate) modal_input_width_px: f32,
    pub(crate) project_rate: u32,
    pub(crate) loading: Option<PendingLoad>,
    pub(crate) dragging: Option<DragState>,
    pub(crate) selection: Option<(f64, f64)>,
    pub(crate) selecting: bool,
    pub(crate) selecting_edge: Option<SelectionEdge>,
    pub(crate) clipboard: Vec<audio::Clip>,
    pub(crate) undo_manager: undo::UndoManager,
    pub(crate) deferred_action: Option<DeferredAction>,
    pub(crate) pending_dialog: Option<PendingDialog>,
    // Text rendering (glyphon)
    pub(crate) font_system: Option<FontSystem>,
    pub(crate) swash_cache: Option<SwashCache>,
    pub(crate) glyphon_cache: Option<Cache>,
    pub(crate) text_atlas: Option<TextAtlas>,
    pub(crate) text_renderer: Option<TextRenderer>,
    pub(crate) viewport: Option<Viewport>,
}

impl App {
    pub(crate) fn new() -> Self {
        Self {
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            pipeline: None,
            tracks: Vec::new(),
            project_path: None,
            player: None,
            modifiers: ModifiersState::empty(),
            cursor_x: 0.0,
            cursor_y: 0.0,
            selected_track: None,
            selected_clip: None,
            view_start: 0.0,
            view_duration: 0.0, // 0 means "show everything" until tracks are loaded
            modal: None,
            modal_input_width_px: 0.0,
            project_rate: 48_000,
            loading: None,
            dragging: None,
            selection: None,
            selecting: false,
            selecting_edge: None,
            clipboard: Vec::new(),
            undo_manager: undo::UndoManager::new(100),
            deferred_action: None,
            pending_dialog: None,
            font_system: None,
            swash_cache: None,
            glyphon_cache: None,
            text_atlas: None,
            text_renderer: None,
            viewport: None,
        }
    }

    /// Height of the scrollbar in logical pixels
    pub(crate) const SCROLLBAR_LP: f32 = 8.0;
    /// Height of the title bar in logical pixels
    pub(crate) const TITLE_BAR_LP: f32 = 28.0;
    /// Width of the track label column in logical pixels
    pub(crate) const TRACK_LABEL_LP: f32 = 36.0;
    /// Font size in logical pixels
    pub(crate) const FONT_SIZE_LP: f32 = 14.0;
    /// Line height in logical pixels
    pub(crate) const LINE_HEIGHT_LP: f32 = 20.0;

    pub(crate) fn scale_factor(&self) -> f32 {
        self.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32)
    }

    pub(crate) fn max_duration(&self) -> f64 {
        self.tracks.iter().map(|t| t.duration_secs()).fold(0.0_f64, f64::max)
    }

    pub(crate) fn effective_view_duration(&self) -> f64 {
        if self.view_duration > 0.0 { self.view_duration } else { self.max_duration() }
    }

    /// Get the current playhead position in seconds
    pub(crate) fn playhead_secs(&self) -> f64 {
        if let Some(player) = &self.player {
            player.position_frac() * self.max_duration()
        } else {
            0.0
        }
    }
}
