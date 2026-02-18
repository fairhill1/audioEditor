use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics, Resolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::app::{push_quad, App, Vertex};

impl App {
    pub(crate) fn init_wgpu(&mut self, window: Arc<Window>) {
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
            let dim = if track.muted { 0.35 } else { 1.0 };
            let base_color = {
                let c = TRACK_COLORS[idx % TRACK_COLORS.len()];
                [c[0] * dim, c[1] * dim, c[2] * dim]
            };
            let bright_color = {
                let c = SELECTED_CLIP_COLORS[idx % SELECTED_CLIP_COLORS.len()];
                [c[0] * dim, c[1] * dim, c[2] * dim]
            };

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
            let label_bg = if track.muted {
                [0.28, 0.15, 0.15]
            } else if self.selected_track == Some(idx) {
                [0.22, 0.22, 0.28]
            } else {
                [0.15, 0.15, 0.18]
            };
            push_quad(&mut vertices, -1.0, lane_bot, label_x1_ndc, lane_top, label_bg);

            // Selection highlight (behind waveforms, only on selected track)
            if self.selected_track == Some(idx) {
                if let Some((sel_start, sel_end)) = self.selection {
                    let (s0, s1) = if sel_start <= sel_end { (sel_start, sel_end) } else { (sel_end, sel_start) };
                    let x0_frac = ((s0 - view_start) / view_duration) as f32;
                    let x1_frac = ((s1 - view_start) / view_duration) as f32;
                    if x1_frac > 0.0 && x0_frac < 1.0 {
                        let x0 = x0_frac.max(0.0) * 2.0 - 1.0;
                        let x1 = x1_frac.min(1.0) * 2.0 - 1.0;
                        push_quad(&mut vertices, x0, wave_bot, x1, wave_top, [0.15, 0.20, 0.35]);
                    }
                }
            }

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
                // Highlight title bar for clips in the selection group or multi-selection
                let in_selection_group = self.selected_track == Some(idx)
                    && (self.selection.is_some_and(|(s0, s1)| {
                        clip_start_sec < s1 && clip_end_sec > s0
                    }) || self.multi_selected_clips.contains(&clip_idx));
                let title_color = if is_selected {
                    [0.25, 0.25, 0.32]
                } else if in_selection_group {
                    [0.22, 0.22, 0.30]
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

    pub(crate) fn render(&mut self) {
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
                    default_color: if self.tracks[idx].muted {
                        GlyphonColor::rgb(200, 100, 100)
                    } else {
                        GlyphonColor::rgb(160, 160, 170)
                    },
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
