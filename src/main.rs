mod audio;
mod modal;
mod playback;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 3],
}

fn push_quad(vertices: &mut Vec<Vertex>, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 3]) {
    vertices.extend_from_slice(&[
        Vertex { position: [x0, y1], color },
        Vertex { position: [x1, y1], color },
        Vertex { position: [x0, y0], color },
        Vertex { position: [x0, y0], color },
        Vertex { position: [x1, y1], color },
        Vertex { position: [x1, y0], color },
    ]);
}

struct PendingLoad {
    result: Arc<Mutex<Option<Result<audio::Clip, String>>>>,
    progress: Arc<AtomicU8>,
    /// Which track to add the clip to, or None to create a new track
    target_track: Option<usize>,
    /// Timeline position for the new clip
    clip_offset_secs: f64,
}

struct DragState {
    clip_idx: usize,
    start_offset: f64,
    start_x: f64,
    start_y: f64,
    source_track_idx: usize,  // track the clip originally lived on
    current_track_idx: usize,
    active: bool, // becomes true once cursor moves past threshold
}

const DRAG_THRESHOLD_PX: f64 = 4.0;

struct App {
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    config: Option<wgpu::SurfaceConfiguration>,
    pipeline: Option<wgpu::RenderPipeline>,
    tracks: Vec<audio::Track>,
    player: Option<playback::Player>,
    modifiers: ModifiersState,
    cursor_x: f64,
    cursor_y: f64,
    selected_track: Option<usize>,
    selected_clip: Option<usize>,
    // Horizontal zoom/scroll state
    view_start: f64,    // left edge in seconds
    view_duration: f64, // visible time span in seconds
    modal: Option<modal::Modal>,
    modal_input_width_px: f32,
    project_rate: u32,
    loading: Option<PendingLoad>,
    dragging: Option<DragState>,
    // Text rendering (glyphon)
    font_system: Option<FontSystem>,
    swash_cache: Option<SwashCache>,
    glyphon_cache: Option<Cache>,
    text_atlas: Option<TextAtlas>,
    text_renderer: Option<TextRenderer>,
    viewport: Option<Viewport>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            pipeline: None,
            tracks: Vec::new(),
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
            font_system: None,
            swash_cache: None,
            glyphon_cache: None,
            text_atlas: None,
            text_renderer: None,
            viewport: None,
        }
    }

    fn init_wgpu(&mut self, window: Arc<Window>) {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("Failed to find a suitable GPU adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("device"),
                ..Default::default()
            },
        ))
        .expect("Failed to create device");

        let size = window.inner_size();
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("Surface not supported by adapter");
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("waveform_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("waveform.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("waveform_pipeline_layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waveform_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Glyphon text rendering setup
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let glyphon_cache = Cache::new(&device);
        let mut text_atlas = TextAtlas::new(&device, &queue, &glyphon_cache, config.format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );
        let mut viewport = Viewport::new(&device, &glyphon_cache);
        viewport.update(
            &queue,
            Resolution {
                width: config.width,
                height: config.height,
            },
        );

        self.window = Some(window);
        self.surface = Some(surface);
        self.device = Some(device);
        self.queue = Some(queue);
        self.config = Some(config);
        self.pipeline = Some(pipeline);
        self.font_system = Some(font_system);
        self.swash_cache = Some(swash_cache);
        self.glyphon_cache = Some(glyphon_cache);
        self.text_atlas = Some(text_atlas);
        self.text_renderer = Some(text_renderer);
        self.viewport = Some(viewport);
    }

    /// Height of the scrollbar in logical pixels
    const SCROLLBAR_LP: f32 = 8.0;
    /// Height of the title bar in logical pixels
    const TITLE_BAR_LP: f32 = 28.0;
    /// Width of the track label column in logical pixels
    const TRACK_LABEL_LP: f32 = 36.0;
    /// Font size in logical pixels
    const FONT_SIZE_LP: f32 = 14.0;
    /// Line height in logical pixels
    const LINE_HEIGHT_LP: f32 = 20.0;

    fn scale_factor(&self) -> f32 {
        self.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32)
    }

    fn max_duration(&self) -> f64 {
        self.tracks.iter().map(|t| t.duration_secs()).fold(0.0_f64, f64::max)
    }

    fn effective_view_duration(&self) -> f64 {
        if self.view_duration > 0.0 { self.view_duration } else { self.max_duration() }
    }

    /// Get the current playhead position in seconds
    fn playhead_secs(&self) -> f64 {
        if let Some(player) = &self.player {
            player.position_frac() * self.max_duration()
        } else {
            0.0
        }
    }

    /// Hit-test: find which (track_idx, clip_idx) is at pixel position (px, py)
    fn hit_test_clip(&self, px: f64, py: f64) -> Option<(usize, usize)> {
        if self.tracks.is_empty() {
            return None;
        }
        let config = self.config.as_ref()?;
        let num_tracks = self.tracks.len();
        let lane_height_px = config.height as f64 / num_tracks as f64;
        let track_idx = (py / lane_height_px) as usize;
        if track_idx >= num_tracks {
            return None;
        }

        let view_start = self.view_start;
        let view_duration = self.effective_view_duration();
        let cursor_secs = view_start + (px / config.width as f64) * view_duration;

        let track = &self.tracks[track_idx];
        for (clip_idx, clip) in track.clips.iter().enumerate() {
            let clip_start = clip.offset_secs;
            let clip_end = clip.offset_secs + clip.duration_secs();
            if cursor_secs >= clip_start && cursor_secs <= clip_end {
                return Some((track_idx, clip_idx));
            }
        }
        None
    }

    /// Snap a clip's offset to nearby clip edges in the same track.
    /// Returns the snapped offset, or the original if no snap applies.
    fn snap_offset(&self, track_idx: usize, skip_clip: usize, desired: f64, clip_dur: f64) -> f64 {
        let config = match self.config.as_ref() {
            Some(c) => c,
            None => return desired,
        };
        let view_duration = self.effective_view_duration();
        let snap_secs = 10.0 / config.width as f64 * view_duration;

        let clip_start = desired;
        let clip_end = desired + clip_dur;

        let mut best_offset = desired;
        let mut best_dist = f64::MAX;

        let track = &self.tracks[track_idx];
        for (i, other) in track.clips.iter().enumerate() {
            if i == skip_clip {
                continue;
            }
            let other_start = other.offset_secs;
            let other_end = other.offset_secs + other.duration_secs();

            // Dragged clip start snaps to other clip end
            let d = (clip_start - other_end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_end;
            }

            // Dragged clip end snaps to other clip start
            let d = (clip_end - other_start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_start - clip_dur;
            }

            // Dragged clip start snaps to other clip start
            let d = (clip_start - other_start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_start;
            }

            // Dragged clip end snaps to other clip end
            let d = (clip_end - other_end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_end - clip_dur;
            }
        }

        best_offset
    }

    /// Clamp a clip's offset so it doesn't overlap any other clip in the track.
    /// Returns the nearest valid offset.
    fn clamp_no_overlap(&self, track_idx: usize, skip_clip: usize, desired: f64, clip_dur: f64) -> f64 {
        let track = &self.tracks[track_idx];

        // Collect all other clips' intervals, sorted by start time
        let mut intervals: Vec<(f64, f64)> = track
            .clips
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip_clip)
            .map(|(_, c)| (c.offset_secs, c.offset_secs + c.duration_secs()))
            .collect();
        intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        if intervals.is_empty() {
            return desired.max(0.0);
        }

        // Build list of gaps where the clip could fit
        // Gap before first clip
        let mut candidates: Vec<f64> = Vec::new();

        // Before first interval
        if clip_dur <= intervals[0].0 {
            // Clip fits before the first clip
            let clamped = desired.clamp(0.0, intervals[0].0 - clip_dur);
            candidates.push(clamped);
        } else if 0.0 + clip_dur <= intervals[0].0 + 1e-9 {
            candidates.push(0.0);
        }

        // Gaps between intervals
        for i in 0..intervals.len() - 1 {
            let gap_start = intervals[i].1;
            let gap_end = intervals[i + 1].0;
            let gap_size = gap_end - gap_start;
            if gap_size >= clip_dur - 1e-9 {
                let clamped = desired.clamp(gap_start, gap_end - clip_dur);
                candidates.push(clamped);
            }
        }

        // After last interval
        let last_end = intervals.last().unwrap().1;
        let clamped = desired.max(last_end);
        candidates.push(clamped);

        // Also consider placing at time 0 if there's room
        if intervals[0].0 >= clip_dur && !candidates.iter().any(|&c| c < 1e-9) {
            candidates.push(0.0);
        }

        // Pick the candidate nearest to desired
        candidates
            .into_iter()
            .min_by(|a, b| {
                let da = (*a - desired).abs();
                let db = (*b - desired).abs();
                da.partial_cmp(&db).unwrap()
            })
            .unwrap_or(desired.max(0.0))
    }

    fn build_waveform_vertices(&self, width: u32, height: u32) -> Vec<Vertex> {
        if self.tracks.is_empty() {
            return Vec::new();
        }

        const TRACK_COLORS: [[f32; 3]; 6] = [
            [0.3, 0.7, 0.4],  // green
            [0.3, 0.5, 0.8],  // blue
            [0.8, 0.4, 0.3],  // red
            [0.7, 0.5, 0.8],  // purple
            [0.8, 0.7, 0.3],  // yellow
            [0.3, 0.7, 0.7],  // cyan
        ];
        const SELECTED_CLIP_COLORS: [[f32; 3]; 6] = [
            [0.4, 0.9, 0.5],
            [0.4, 0.65, 1.0],
            [1.0, 0.5, 0.4],
            [0.9, 0.65, 1.0],
            [1.0, 0.9, 0.4],
            [0.4, 0.9, 0.9],
        ];
        const DIVIDER_COLOR: [f32; 3] = [0.25, 0.25, 0.28];
        const TITLE_BG_COLOR: [f32; 3] = [0.18, 0.18, 0.21];

        let num_tracks = self.tracks.len();
        let lane_height = 2.0 / num_tracks as f32;
        let line_h = 1.0 / height as f32;
        let title_bar_physical = Self::TITLE_BAR_LP * self.scale_factor();
        let title_h = title_bar_physical / height as f32 * 2.0; // NDC height of title bar

        let view_start = self.view_start;
        let view_duration = self.effective_view_duration();
        let view_end = view_start + view_duration;

        let mut vertices = Vec::new();

        for (idx, track) in self.tracks.iter().enumerate() {
            let base_color = TRACK_COLORS[idx % TRACK_COLORS.len()];
            let bright_color = SELECTED_CLIP_COLORS[idx % SELECTED_CLIP_COLORS.len()];

            let lane_top = 1.0 - idx as f32 * lane_height;
            let lane_bot = lane_top - lane_height;

            // Title bar / center line geometry
            let title_top = lane_top;
            let title_bot = (lane_top - title_h).max(lane_bot);

            // Center line for this track (in waveform area, below title bar)
            let wave_top = title_bot;
            let wave_bot = lane_bot;
            let wave_center = (wave_top + wave_bot) / 2.0;
            let half_wave = (wave_top - wave_bot) / 2.0;
            let center_color = [base_color[0] * 0.5, base_color[1] * 0.5, base_color[2] * 0.5];
            push_quad(&mut vertices, -1.0, wave_center - line_h, 1.0, wave_center + line_h, center_color);

            // Divider line between tracks
            if idx > 0 {
                push_quad(&mut vertices, -1.0, lane_top - line_h, 1.0, lane_top + line_h, DIVIDER_COLOR);
            }

            // Track label background (far left column)
            let label_w_physical = Self::TRACK_LABEL_LP * self.scale_factor();
            let label_x1_ndc = -1.0 + label_w_physical / width as f32 * 2.0;
            let label_bg = if self.selected_track == Some(idx) {
                [0.22, 0.22, 0.28]
            } else {
                [0.15, 0.15, 0.18]
            };
            push_quad(&mut vertices, -1.0, lane_bot, label_x1_ndc, lane_top, label_bg);

            // Draw each clip in this track
            for (clip_idx, clip) in track.clips.iter().enumerate() {
                let is_selected = self.selected_track == Some(idx) && self.selected_clip == Some(clip_idx);
                let color = if is_selected { bright_color } else { base_color };

                let clip_start_sec = clip.offset_secs;
                let clip_end_sec = clip.offset_secs + clip.duration_secs();

                // Clamp to visible window
                let vis_start_sec = clip_start_sec.max(view_start);
                let vis_end_sec = clip_end_sec.min(view_end);

                if vis_start_sec >= vis_end_sec {
                    continue;
                }

                // Clip title bar background (spans only this clip's width)
                let clip_x0_ndc = ((vis_start_sec - view_start) / view_duration) as f32 * 2.0 - 1.0;
                let clip_x1_ndc = ((vis_end_sec - view_start) / view_duration) as f32 * 2.0 - 1.0;
                let title_color = if is_selected {
                    [0.25, 0.25, 0.32]
                } else {
                    TITLE_BG_COLOR
                };
                push_quad(&mut vertices, clip_x0_ndc, title_bot, clip_x1_ndc, title_top, title_color);

                let mono_len = clip.mono.len();
                let sr = clip.sample_rate as f64;

                // The portion of the clip that's visible (relative to clip start)
                let clip_vis_start = vis_start_sec - clip_start_sec;
                let clip_vis_end = vis_end_sec - clip_start_sec;

                let vis_start_sample = (clip_vis_start * sr) as usize;
                let vis_end_sample = ((clip_vis_end * sr) as usize).min(mono_len);

                if vis_start_sample >= vis_end_sample {
                    continue;
                }

                let vis_sample_count = vis_end_sample - vis_start_sample;

                // How many pixel columns does this clip's visible portion span?
                let vis_frac = (vis_end_sec - vis_start_sec) / view_duration;
                let clip_cols = (width as f64 * vis_frac) as u32;
                if clip_cols == 0 {
                    continue;
                }
                let samples_per_col = (vis_sample_count as f64 / clip_cols as f64).max(1.0);

                // Where does the visible portion start in NDC x?
                let x_offset = ((vis_start_sec - view_start) / view_duration) as f32;

                for col in 0..clip_cols {
                    let start = vis_start_sample + (col as f64 * samples_per_col) as usize;
                    let end = (vis_start_sample + (((col + 1) as f64) * samples_per_col) as usize).min(vis_end_sample);

                    if start >= end {
                        continue;
                    }

                    let (min_val, max_val) = clip.min_max_range(start, end);

                    let x0 = (x_offset + col as f32 / width as f32) * 2.0 - 1.0;
                    let x1 = (x_offset + (col + 1) as f32 / width as f32) * 2.0 - 1.0;

                    let y_top = (wave_center + max_val * half_wave).min(wave_top);
                    let y_bot = (wave_center + min_val * half_wave).max(wave_bot);

                    push_quad(&mut vertices, x0, y_bot, x1, y_top, color);
                }
            }
        }

        // Scrollbar at bottom
        let max_dur = self.max_duration();
        if max_dur > 0.0 {
            let scrollbar_h = Self::SCROLLBAR_LP * self.scale_factor();
            let bar_ndc_h = scrollbar_h / height as f32 * 2.0;

            // Track background (full width, dark)
            let track_bg = [0.15, 0.15, 0.18];
            let bar_top = -1.0 + bar_ndc_h;
            let bar_bot = -1.0_f32;
            push_quad(&mut vertices, -1.0, bar_bot, 1.0, bar_top, track_bg);

            // Thumb (shows visible portion)
            let thumb_left = (view_start / max_dur) as f32;
            let thumb_right = ((view_start + view_duration) / max_dur) as f32;
            let thumb_x0 = thumb_left * 2.0 - 1.0;
            let thumb_x1 = thumb_right * 2.0 - 1.0;
            let thumb_color = [0.4, 0.4, 0.45];
            push_quad(&mut vertices, thumb_x0, bar_bot, thumb_x1, bar_top, thumb_color);
        }

        // Playhead
        if let Some(player) = &self.player {
            let max_dur = self.max_duration();
            let playhead_secs = player.position_frac() * max_dur;
            let ndc_frac = ((playhead_secs - view_start) / view_duration) as f32;
            if ndc_frac >= 0.0 && ndc_frac <= 1.0 {
                let x = ndc_frac * 2.0 - 1.0;
                let hw = 1.0 / width as f32;
                push_quad(&mut vertices, x - hw, -1.0, x + hw, 1.0, [1.0, 1.0, 1.0]);
            }
        }

        // Modal overlay
        if self.modal.is_some() {
            // Modal box (centered, fixed NDC size)
            let box_w = 0.5;
            let box_h = 0.25;
            push_quad(&mut vertices, -box_w, -box_h, box_w, box_h, [0.18, 0.18, 0.22]);

            // Border
            let bw = 2.0 / width as f32;
            let bh = 2.0 / height as f32;
            let border_color = [0.4, 0.4, 0.5];
            push_quad(&mut vertices, -box_w, box_h, box_w, box_h + bh, border_color); // top
            push_quad(&mut vertices, -box_w, -box_h - bh, box_w, -box_h, border_color); // bottom
            push_quad(&mut vertices, -box_w - bw, -box_h, -box_w, box_h, border_color); // left
            push_quad(&mut vertices, box_w, -box_h, box_w + bw, box_h, border_color); // right

            // Input field background
            let field_w = 0.35;
            let field_h = 0.06;
            push_quad(&mut vertices, -field_w, -0.05, field_w, -0.05 + field_h, [0.12, 0.12, 0.15]);

            // Cursor in input field — position derived from glyphon layout
            let text_offset_ndc = self.modal_input_width_px / width as f32 * 2.0;
            let pad_ndc = 8.0 * self.scale_factor() * 0.5 / width as f32 * 2.0;
            let cursor_x = -field_w + pad_ndc + text_offset_ndc;
            if cursor_x < field_w - pad_ndc {
                push_quad(&mut vertices, cursor_x, -0.04, cursor_x + bw, -0.05 + field_h - 0.01, [0.7, 0.7, 0.8]);
            }
        }

        vertices
    }

    fn rebuild_player(&mut self) {
        self.player = playback::Player::new(&self.tracks);
    }

    fn open_file(&mut self) {
        if self.loading.is_some() {
            return; // already loading
        }

        let file = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "mp3", "flac", "ogg", "m4a", "aac"])
            .pick_file();

        if let Some(path) = file {
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
    }

    fn poll_loading(&mut self) {
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

                if let Some(track_idx) = pending.target_track {
                    // Add clip to existing track
                    if track_idx < self.tracks.len() {
                        self.selected_clip = Some(self.tracks[track_idx].clips.len());
                        self.tracks[track_idx].clips.push(clip);
                        self.selected_track = Some(track_idx);
                    }
                } else {
                    // Create a new track with this clip
                    self.tracks.push(audio::Track {
                        clips: vec![clip],
                    });
                    self.selected_track = Some(self.tracks.len() - 1);
                    self.selected_clip = Some(0);
                }

                self.view_duration = self.max_duration();
                self.view_start = 0.0;
                self.rebuild_player();
            }
            self.update_title();
            self.window.as_ref().unwrap().request_redraw();
        }
    }

    fn update_title(&self) {
        let title = if self.tracks.is_empty() {
            "Audio Editor".to_string()
        } else {
            let rate_khz = self.project_rate as f64 / 1000.0;
            format!("Audio Editor — {} track(s) — {rate_khz:.1}kHz", self.tracks.len())
        };
        self.window.as_ref().unwrap().set_title(&title);
    }

    fn handle_modal_result(&mut self, result: modal::ModalResult) {
        match result {
            modal::ModalResult::ClickTrackBpm(bpm) => {
                let dur = if self.max_duration() > 0.0 { self.max_duration() } else { 30.0 };
                let clip = audio::generate_click_track(bpm, dur, self.project_rate);
                self.tracks.push(audio::Track {
                    clips: vec![clip],
                });
                self.view_duration = self.max_duration();
                self.view_start = 0.0;
                self.rebuild_player();
                self.update_title();
            }
        }
    }

    fn render(&mut self) {
        let surface = self.surface.as_ref().unwrap();
        let device = self.device.as_ref().unwrap();
        let queue = self.queue.as_ref().unwrap();
        let config = self.config.as_ref().unwrap();
        let width = config.width;
        let height = config.height;

        // Prepare text buffers for track titles
        let scale = self.scale_factor();
        let title_bar_phys = Self::TITLE_BAR_LP * scale;
        let font_size_phys = Self::FONT_SIZE_LP * scale;
        let line_height_phys = Self::LINE_HEIGHT_LP * scale;
        let padding_phys = 8.0 * scale;

        let mut text_buffers: Vec<Buffer> = Vec::new();
        // (track_idx, clip_left_px, clip_width_px) for each clip text buffer
        let mut clip_text_positions: Vec<(usize, f32, f32)> = Vec::new();

        let view_start = self.view_start;
        let view_duration = self.effective_view_duration();
        let view_end = view_start + view_duration;

        let font_system = self.font_system.as_mut().unwrap();

        // Per-clip title text buffers
        if !self.tracks.is_empty() {
            for (track_idx, track) in self.tracks.iter().enumerate() {
                for clip in &track.clips {
                    let clip_start = clip.offset_secs;
                    let clip_end = clip.offset_secs + clip.duration_secs();
                    let vis_start = clip_start.max(view_start);
                    let vis_end = clip_end.min(view_end);
                    if vis_start >= vis_end {
                        continue;
                    }
                    let clip_left_px = ((vis_start - view_start) / view_duration) as f32 * width as f32;
                    let clip_right_px = ((vis_end - view_start) / view_duration) as f32 * width as f32;
                    let clip_width_px = clip_right_px - clip_left_px;

                    let mut buffer = Buffer::new(font_system, Metrics::new(font_size_phys, line_height_phys));
                    buffer.set_size(font_system, Some((clip_width_px - padding_phys * 2.0).max(0.0)), Some(title_bar_phys));
                    buffer.set_text(font_system, &clip.name, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
                    buffer.shape_until_scroll(font_system, false);
                    text_buffers.push(buffer);
                    clip_text_positions.push((track_idx, clip_left_px, clip_width_px));
                }
            }
        }

        // Track label text buffers ("T1", "T2", etc.)
        let track_label_start_idx = text_buffers.len();
        let label_w_phys = Self::TRACK_LABEL_LP * scale;
        if !self.tracks.is_empty() {
            for idx in 0..self.tracks.len() {
                let label = format!("T{}", idx + 1);
                let mut buffer = Buffer::new(font_system, Metrics::new(font_size_phys, line_height_phys));
                buffer.set_size(font_system, Some(label_w_phys), Some(line_height_phys * 2.0));
                buffer.set_text(font_system, &label, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
                buffer.shape_until_scroll(font_system, false);
                text_buffers.push(buffer);
            }
        }

        // Modal text buffers (title + input)
        let modal_title_idx;
        let modal_input_idx;
        if let Some(modal) = &self.modal {
            modal_title_idx = Some(text_buffers.len());
            let modal_font_size = Self::FONT_SIZE_LP * scale * 1.1;
            let modal_line_h = Self::LINE_HEIGHT_LP * scale * 1.1;
            let mut title_buf = Buffer::new(font_system, Metrics::new(modal_font_size, modal_line_h));
            title_buf.set_size(font_system, Some(width as f32 * 0.5), Some(modal_line_h * 2.0));
            title_buf.set_text(font_system, &modal.title, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
            title_buf.shape_until_scroll(font_system, false);
            text_buffers.push(title_buf);

            modal_input_idx = Some(text_buffers.len());
            let display = if modal.input.is_empty() { " " } else { &modal.input };
            let mut input_buf = Buffer::new(font_system, Metrics::new(font_size_phys, line_height_phys));
            input_buf.set_size(font_system, Some(width as f32 * 0.35), Some(line_height_phys * 2.0));
            input_buf.set_text(font_system, display, &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
            input_buf.shape_until_scroll(font_system, false);
            self.modal_input_width_px = input_buf
                .layout_runs()
                .next()
                .map(|run| run.line_w)
                .unwrap_or(0.0);
            text_buffers.push(input_buf);
        } else {
            modal_title_idx = None;
            modal_input_idx = None;
        }

        // Build all text areas
        {
            let text_atlas = self.text_atlas.as_mut().unwrap();
            let text_renderer = self.text_renderer.as_mut().unwrap();
            let viewport = self.viewport.as_mut().unwrap();
            let swash_cache = self.swash_cache.as_mut().unwrap();
            let font_system = self.font_system.as_mut().unwrap();

            viewport.update(queue, Resolution { width, height });

            let num_tracks = self.tracks.len();
            let lane_height_px = if num_tracks > 0 { height as f32 / num_tracks as f32 } else { 0.0 };

            let mut text_areas: Vec<TextArea> = Vec::new();

            // Clip titles
            for (buf_idx, &(track_idx, clip_left_px, clip_width_px)) in clip_text_positions.iter().enumerate() {
                let lane_top = track_idx as f32 * lane_height_px;
                let vert_pad = (title_bar_phys - line_height_phys) / 2.0;
                text_areas.push(TextArea {
                    buffer: &text_buffers[buf_idx],
                    left: clip_left_px + padding_phys,
                    top: lane_top + vert_pad,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: clip_left_px as i32,
                        top: lane_top as i32,
                        right: (clip_left_px + clip_width_px) as i32,
                        bottom: (lane_top + title_bar_phys) as i32,
                    },
                    default_color: GlyphonColor::rgb(220, 220, 220),
                    custom_glyphs: &[],
                });
            }

            // Track label text areas
            for idx in 0..num_tracks {
                let buf_idx = track_label_start_idx + idx;
                let lane_top_px = idx as f32 * lane_height_px;
                let vert_center = lane_top_px + lane_height_px / 2.0 - line_height_phys / 2.0;
                text_areas.push(TextArea {
                    buffer: &text_buffers[buf_idx],
                    left: padding_phys * 0.5,
                    top: vert_center,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: lane_top_px as i32,
                        right: label_w_phys as i32,
                        bottom: (lane_top_px + lane_height_px) as i32,
                    },
                    default_color: GlyphonColor::rgb(160, 160, 170),
                    custom_glyphs: &[],
                });
            }

            // Modal text areas
            if let Some(ti) = modal_title_idx {
                // Modal box is centered: NDC -0.5..0.5 horizontally, -0.25..0.25 vertically
                // Convert NDC to pixel coords: px = (ndc + 1) / 2 * dimension
                let box_left_px = ((-0.5 + 1.0) / 2.0) * width as f32;
                let _box_top_px = ((-0.25 + 1.0) / 2.0) * height as f32;
                // Flip Y: NDC top (0.25) → pixel top
                let box_pixel_top = ((1.0 - 0.25) / 2.0) * height as f32;
                let modal_line_h = Self::LINE_HEIGHT_LP * scale * 1.1;

                text_areas.push(TextArea {
                    buffer: &text_buffers[ti],
                    left: box_left_px + padding_phys,
                    top: box_pixel_top + padding_phys,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: box_left_px as i32,
                        top: box_pixel_top as i32,
                        right: (box_left_px + width as f32 * 0.5) as i32,
                        bottom: (box_pixel_top + modal_line_h * 2.0) as i32,
                    },
                    default_color: GlyphonColor::rgb(220, 220, 220),
                    custom_glyphs: &[],
                });

                if let Some(ii) = modal_input_idx {
                    // Input field is at NDC y=-0.05..0.01, which in pixels is:
                    let field_top_px = ((1.0 - (-0.05 + 0.06)) / 2.0) * height as f32;
                    let field_left_px = ((-0.35 + 1.0) / 2.0) * width as f32;

                    text_areas.push(TextArea {
                        buffer: &text_buffers[ii],
                        left: field_left_px + padding_phys * 0.5,
                        top: field_top_px + padding_phys * 0.25,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: field_left_px as i32,
                            top: field_top_px as i32,
                            right: (field_left_px + width as f32 * 0.35) as i32,
                            bottom: (field_top_px + line_height_phys * 2.0) as i32,
                        },
                        default_color: GlyphonColor::rgb(200, 200, 210),
                        custom_glyphs: &[],
                    });
                }
            }

            text_renderer
                .prepare(device, queue, font_system, text_atlas, viewport, text_areas, swash_cache)
                .expect("Failed to prepare text");
        }

        let frame = surface
            .get_current_texture()
            .expect("Failed to get surface texture");
        let view = frame.texture.create_view(&Default::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_encoder"),
        });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.12,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            let vertices = self.build_waveform_vertices(width, height);
            if !vertices.is_empty() {
                let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("waveform_vertices"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                render_pass.set_pipeline(self.pipeline.as_ref().unwrap());
                render_pass.set_vertex_buffer(0, vbuf.slice(..));
                render_pass.draw(0..vertices.len() as u32, 0..1);
            }

            // Render text (track titles + modal text) on top
            let text_atlas = self.text_atlas.as_ref().unwrap();
            let text_renderer = self.text_renderer.as_ref().unwrap();
            let viewport = self.viewport.as_ref().unwrap();
            text_renderer
                .render(text_atlas, viewport, &mut render_pass)
                .expect("Failed to render text");
        }

        queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        self.text_atlas.as_mut().unwrap().trim();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("Audio Editor");
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        self.init_wgpu(window);
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
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyO)
                    && self.modifiers.super_key() =>
            {
                self.open_file();
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyT)
                    && self.modifiers.super_key() =>
            {
                // Insert new empty track below the selected track (or at the end)
                let insert_at = self.selected_track.map_or(self.tracks.len(), |i| i + 1);
                self.tracks.insert(insert_at, audio::Track { clips: vec![] });
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
                        let split_at = playhead - clip_start;
                        let right = self.tracks[track_idx].clips[clip_idx].split_at(split_at);
                        self.tracks[track_idx].clips.insert(clip_idx + 1, right);
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
                        self.tracks[track_idx].clips.remove(clip_idx);
                        // Remove track if it's now empty
                        if self.tracks[track_idx].clips.is_empty() {
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
                        self.view_start = 0.0;
                        self.view_duration = self.max_duration();
                        self.rebuild_player();
                        self.update_title();
                        self.window.as_ref().unwrap().request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Space) =>
            {
                if let Some(player) = &self.player {
                    player.toggle();
                    self.window.as_ref().unwrap().request_redraw();
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
                        let step = if self.modifiers.shift_key() { 0.1 } else { 1.0 };
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
                // Hit-test for clip selection and potential drag
                if let Some((track_idx, clip_idx)) = self.hit_test_clip(self.cursor_x, self.cursor_y) {
                    self.selected_track = Some(track_idx);
                    self.selected_clip = Some(clip_idx);

                    // Prepare drag (not active until cursor moves past threshold)
                    let clip = &self.tracks[track_idx].clips[clip_idx];
                    self.dragging = Some(DragState {
                        clip_idx,
                        start_offset: clip.offset_secs,
                        start_x: self.cursor_x,
                        start_y: self.cursor_y,
                        source_track_idx: track_idx,
                        current_track_idx: track_idx,
                        active: false,
                    });
                } else if !self.tracks.is_empty() {
                    // Click on empty area — select track but deselect clip
                    if let Some(config) = &self.config {
                        let track_idx = (self.cursor_y / config.height as f64 * self.tracks.len() as f64) as usize;
                        self.selected_track = Some(track_idx.min(self.tracks.len() - 1));
                        self.selected_clip = None;
                    }
                }

                // Always seek on click
                if let (Some(player), Some(config)) = (&self.player, &self.config) {
                    let cursor_frac = self.cursor_x / config.width as f64;
                    let view_dur = self.effective_view_duration();
                    let click_secs = self.view_start + cursor_frac * view_dur;
                    let max_dur = self.max_duration();
                    if max_dur > 0.0 {
                        player.seek_frac(click_secs / max_dur);
                    }
                }
                self.window.as_ref().unwrap().request_redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let Some(drag) = self.dragging.take() {
                    if drag.active {
                        let mut sel_track = drag.current_track_idx;
                        let sel_clip = drag.clip_idx;
                        // Remove the source track only if the drag emptied it
                        let src = drag.source_track_idx;
                        if src < self.tracks.len() && self.tracks[src].clips.is_empty() {
                            self.tracks.remove(src);
                            if src < sel_track {
                                sel_track -= 1;
                            }
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

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}
