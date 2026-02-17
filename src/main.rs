mod audio;
mod playback;

use std::sync::Arc;

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

struct App {
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    config: Option<wgpu::SurfaceConfiguration>,
    pipeline: Option<wgpu::RenderPipeline>,
    tracks: Vec<audio::AudioTrack>,
    player: Option<playback::Player>,
    modifiers: ModifiersState,
    cursor_x: f64,
    cursor_y: f64,
    selected_track: Option<usize>,
    // Horizontal zoom/scroll state
    view_start: f64,    // left edge in seconds
    view_duration: f64, // visible time span in seconds
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
            view_start: 0.0,
            view_duration: 0.0, // 0 means "show everything" until tracks are loaded
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
            let color = TRACK_COLORS[idx % TRACK_COLORS.len()];
            let center_color = [color[0] * 0.5, color[1] * 0.5, color[2] * 0.5];

            let lane_top = 1.0 - idx as f32 * lane_height;
            let lane_bot = lane_top - lane_height;

            // Title bar background (brighter when selected)
            let title_top = lane_top;
            let title_bot = (lane_top - title_h).max(lane_bot);
            let title_color = if self.selected_track == Some(idx) {
                [0.25, 0.25, 0.32]
            } else {
                TITLE_BG_COLOR
            };
            push_quad(&mut vertices, -1.0, title_bot, 1.0, title_top, title_color);

            // Center line for this track (in waveform area, below title bar)
            let wave_top_here = title_bot;
            let wave_center_here = (wave_top_here + lane_bot) / 2.0;
            push_quad(&mut vertices, -1.0, wave_center_here - line_h, 1.0, wave_center_here + line_h, center_color);

            // Divider line between tracks
            if idx > 0 {
                push_quad(&mut vertices, -1.0, lane_top - line_h, 1.0, lane_top + line_h, DIVIDER_COLOR);
            }

            // Waveform — only draw samples visible in the current view window
            // Use the area below the title bar for waveform drawing
            let wave_top = title_bot;
            let wave_bot = lane_bot;
            let wave_center = (wave_top + wave_bot) / 2.0;
            let half_wave = (wave_top - wave_bot) / 2.0;

            let mono_len = track.mono.len();
            let sr = track.sample_rate as f64;
            let track_duration = track.duration_secs();

            // Clamp visible range to this track's actual duration
            let vis_start_sec = view_start.max(0.0);
            let vis_end_sec = view_end.min(track_duration);

            if vis_start_sec < vis_end_sec {
                let vis_start_sample = (vis_start_sec * sr) as usize;
                let vis_end_sample = ((vis_end_sec * sr) as usize).min(mono_len);
                let vis_sample_count = vis_end_sample - vis_start_sample;

                // How many pixel columns does this track's visible portion span?
                let vis_frac = (vis_end_sec - vis_start_sec) / view_duration;
                let track_cols = (width as f64 * vis_frac) as u32;
                let samples_per_col = (vis_sample_count as f64 / track_cols.max(1) as f64).max(1.0);

                // Where does the visible portion start in NDC x?
                let x_offset = ((vis_start_sec - view_start) / view_duration) as f32;

                for col in 0..track_cols {
                    let start = vis_start_sample + (col as f64 * samples_per_col) as usize;
                    let end = (vis_start_sample + (((col + 1) as f64) * samples_per_col) as usize).min(vis_end_sample);

                    if start >= end {
                        continue;
                    }

                    let (min_val, max_val) = track.min_max_range(start, end);

                    let x0 = (x_offset + col as f32 / width as f32) * 2.0 - 1.0;
                    let x1 = (x_offset + (col + 1) as f32 / width as f32) * 2.0 - 1.0;

                    let y_top = wave_center + max_val * half_wave;
                    let y_bot = wave_center + min_val * half_wave;

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

        vertices
    }

    fn rebuild_player(&mut self) {
        self.player = playback::Player::new(&self.tracks);
    }

    fn open_file(&mut self) {
        let file = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "mp3", "flac", "ogg", "m4a", "aac"])
            .pick_file();

        if let Some(path) = file {
            match audio::load_file(&path) {
                Ok(track) => {
                    self.tracks.push(track);
                    self.view_duration = self.max_duration();
                    self.view_start = 0.0;
                    self.rebuild_player();
                    self.update_title();
                    self.window.as_ref().unwrap().request_redraw();
                }
                Err(e) => {
                    eprintln!("Failed to load audio: {e}");
                }
            }
        }
    }

    fn update_title(&self) {
        let title = if self.tracks.is_empty() {
            "Audio Editor".to_string()
        } else {
            format!("Audio Editor — {} track(s)", self.tracks.len())
        };
        self.window.as_ref().unwrap().set_title(&title);
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
        if !self.tracks.is_empty() {
            let font_system = self.font_system.as_mut().unwrap();
            let num_tracks = self.tracks.len();
            let lane_height_px = height as f32 / num_tracks as f32;

            for track in &self.tracks {
                let mut buffer = Buffer::new(font_system, Metrics::new(font_size_phys, line_height_phys));
                buffer.set_size(font_system, Some(width as f32 - padding_phys * 2.0), Some(title_bar_phys));
                buffer.set_text(font_system, &track.name, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
                buffer.shape_until_scroll(font_system, false);
                text_buffers.push(buffer);
            }

            // Prepare glyphon
            let text_atlas = self.text_atlas.as_mut().unwrap();
            let text_renderer = self.text_renderer.as_mut().unwrap();
            let viewport = self.viewport.as_mut().unwrap();
            let swash_cache = self.swash_cache.as_mut().unwrap();
            let font_system = self.font_system.as_mut().unwrap();

            viewport.update(queue, Resolution { width, height });

            let text_areas: Vec<TextArea> = text_buffers
                .iter()
                .enumerate()
                .map(|(idx, buf)| {
                    let lane_top = idx as f32 * lane_height_px;
                    let vert_pad = (title_bar_phys - line_height_phys) / 2.0;
                    TextArea {
                        buffer: buf,
                        left: padding_phys,
                        top: lane_top + vert_pad,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: 0,
                            top: lane_top as i32,
                            right: width as i32,
                            bottom: (lane_top + title_bar_phys) as i32,
                        },
                        default_color: GlyphonColor::rgb(220, 220, 220),
                        custom_glyphs: &[],
                    }
                })
                .collect();

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

            if !self.tracks.is_empty() {
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

                // Render track title text on top
                let text_atlas = self.text_atlas.as_ref().unwrap();
                let text_renderer = self.text_renderer.as_ref().unwrap();
                let viewport = self.viewport.as_ref().unwrap();
                text_renderer
                    .render(text_atlas, viewport, &mut render_pass)
                    .expect("Failed to render text");
            }
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
        if let Some(player) = &self.player {
            if player.is_playing() {
                self.window.as_ref().unwrap().request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
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
                    && event.physical_key == PhysicalKey::Code(KeyCode::Backspace)
                    && !self.tracks.is_empty() =>
            {
                if let Some(idx) = self.selected_track {
                    self.tracks.remove(idx);
                    // Fix selection
                    if self.tracks.is_empty() {
                        self.selected_track = None;
                    } else {
                        self.selected_track = Some(idx.min(self.tracks.len() - 1));
                    }
                    // Reset view to fit remaining tracks
                    self.view_start = 0.0;
                    self.view_duration = self.max_duration();
                    self.rebuild_player();
                    self.update_title();
                    self.window.as_ref().unwrap().request_redraw();
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
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_x = position.x;
                self.cursor_y = position.y;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                // Select track based on click y position
                if !self.tracks.is_empty() {
                    if let Some(config) = &self.config {
                        let track_idx = (self.cursor_y / config.height as f64 * self.tracks.len() as f64) as usize;
                        self.selected_track = Some(track_idx.min(self.tracks.len() - 1));
                    }
                }
                // Seek
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
