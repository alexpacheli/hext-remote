//! Video renderer.
//!
//! Decoded I420 planes are uploaded as three R8 textures and converted to
//! RGB in a fragment shader, letterboxed to the window. Frames are presented
//! as soon as they are uploaded (mailbox/immediate when available).

use std::sync::Arc;

use hext_codec::YuvFrame;
use winit::window::Window;

const SHADER: &str = include_str!("yuv.wgsl");
const BUTTON_SHADER: &str = include_str!("button.wgsl");

/// The full screen button, in logical pixels, and how far from the top.
const BUTTON_WIDTH: f64 = 56.0;
const BUTTON_HEIGHT: f64 = 28.0;
const BUTTON_TOP: f64 = 6.0;
/// Each numbered button of the display bar, and the gap to the full screen
/// button, in logical pixels.
const BAR_CELL: f64 = 32.0;
const BAR_GAP: f64 = 8.0;
/// The handle that stands for the buttons while they are folded: a thin
/// upright strip on the left edge, halfway down, in logical pixels. Its
/// height is eight times its width (the shader draws it so). The top edge
/// is the host's: its session bar folds there, anywhere along it.
const HANDLE_WIDTH: f64 = 8.0;
const HANDLE_HEIGHT: f64 = 64.0;
const HANDLE_LEFT: f64 = 2.0;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no compatible GPU adapter: {0}")]
    Adapter(String),
    #[error("GPU device: {0}")]
    Device(String),
    #[error("window surface: {0}")]
    Surface(String),
}

/// Where the video is drawn inside the window, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl VideoRect {
    /// Maps a window position to `0.0..=1.0` video coordinates, or `None`
    /// when the position is on the letterbox bars.
    pub fn normalize(&self, px: f64, py: f64) -> Option<(f32, f32)> {
        let nx = (px as f32 - self.x) / self.width;
        let ny = (py as f32 - self.y) / self.height;
        ((0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny)).then_some((nx, ny))
    }

    /// Whether a window position is inside, with `margin` pixels to spare.
    pub fn contains(&self, px: f64, py: f64, margin: f32) -> bool {
        let (px, py) = (px as f32, py as f32);
        px >= self.x - margin
            && px <= self.x + self.width + margin
            && py >= self.y - margin
            && py <= self.y + self.height + margin
    }
}

/// Where the full screen button goes, at the top center of a window of
/// `window` physical pixels and that display scale.
pub fn button_rect(window: (u32, u32), scale: f64) -> VideoRect {
    let (width, height) = (BUTTON_WIDTH * scale, BUTTON_HEIGHT * scale);
    VideoRect {
        x: ((f64::from(window.0) - width) / 2.0).floor() as f32,
        y: (BUTTON_TOP * scale).floor() as f32,
        width: width as f32,
        height: height as f32,
    }
}

/// Where the handle goes, on the left edge of a window of `window` physical
/// pixels and that display scale, halfway down.
pub fn handle_rect(window: (u32, u32), scale: f64) -> VideoRect {
    let (width, height) = (HANDLE_WIDTH * scale, HANDLE_HEIGHT * scale);
    VideoRect {
        x: (HANDLE_LEFT * scale).floor() as f32,
        y: ((f64::from(window.1) - height) / 2.0).max(0.0).floor() as f32,
        width: width as f32,
        height: height as f32,
    }
}

/// Where the chat button goes, left of what starts at `left` on the row of
/// the full screen `button`; `None` when the window has no room for it.
pub fn chat_button_rect(left: f32, button: VideoRect, scale: f64) -> Option<VideoRect> {
    let x = left - (BAR_GAP * scale) as f32 - button.width;
    (x >= 0.0).then_some(VideoRect { x, ..button })
}

/// What pressing the full screen button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    Enter,
    Leave,
}

/// The numbered buttons of the host's displays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBar {
    pub rect: VideoRect,
    pub count: u32,
    /// The one shown, from 0.
    pub current: u32,
}

impl DisplayBar {
    /// The bar for `count` displays, left of the full screen `button`;
    /// `None` when the window has no room for it.
    pub fn new(button: VideoRect, count: u32, current: u32, scale: f64) -> Option<Self> {
        let width = (BAR_CELL * scale) as f32 * count as f32;
        let x = button.x - (BAR_GAP * scale) as f32 - width;
        (x >= 0.0).then_some(Self {
            rect: VideoRect { x, width, ..button },
            count,
            current,
        })
    }

    /// The display whose button is at this window position.
    pub fn display_at(&self, px: f64, py: f64) -> Option<u32> {
        if !self.rect.contains(px, py, 0.0) {
            return None;
        }
        let across = (px as f32 - self.rect.x) / self.rect.width * self.count as f32;
        Some((across as u32).min(self.count - 1))
    }
}

/// Fits a `video` sized picture into a `window`, preserving aspect ratio.
pub fn letterbox(window: (u32, u32), video: (u32, u32)) -> VideoRect {
    let (ww, wh) = (window.0 as f32, window.1 as f32);
    let (vw, vh) = (video.0.max(1) as f32, video.1.max(1) as f32);
    let scale = (ww / vw).min(wh / vh);
    let (width, height) = (vw * scale, vh * scale);
    VideoRect {
        x: ((ww - width) / 2.0).floor(),
        y: ((wh - height) / 2.0).floor(),
        width,
        height,
    }
}

struct Planes {
    width: u32,
    height: u32,
    y: wgpu::Texture,
    u: wgpu::Texture,
    v: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// The graphics API to try first, unless `WGPU_BACKEND` says otherwise.
/// DirectX 12 on Windows: with Vulkan, on a computer with two GPUs, a window
/// going full screen on the other GPU's monitor kept the size it had.
fn preferred_backends() -> Option<wgpu::Backends> {
    (cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none()).then_some(wgpu::Backends::DX12)
}

/// A surface on `window` and the GPU to draw it with, on `backends` (`None`:
/// any, even a software one; with a choice, the others may do better).
fn open_gpu(window: &Arc<Window>, backends: Option<wgpu::Backends>) -> Result<Gpu, RenderError> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    if let Some(backends) = backends {
        descriptor.backends = backends;
    }
    let instance = wgpu::Instance::new(descriptor);
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| RenderError::Surface(e.to_string()))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|e| RenderError::Adapter(e.to_string()))?;
    let info = adapter.get_info();
    if backends.is_some() && info.device_type == wgpu::DeviceType::Cpu {
        return Err(RenderError::Adapter(format!("{} é software", info.name)));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("hext"),
        ..Default::default()
    }))
    .map_err(|e| RenderError::Device(e.to_string()))?;
    tracing::info!("GPU: {:?} ({:?})", info.name, info.backend);
    Ok(Gpu {
        surface,
        adapter,
        device,
        queue,
    })
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    planes: Option<Planes>,
    button_enter: wgpu::RenderPipeline,
    button_leave: wgpu::RenderPipeline,
    /// The button on screen, if any.
    button: Option<(ButtonAction, VideoRect)>,
    bar_pipeline: wgpu::RenderPipeline,
    /// What the bar's shader reads: how many displays, and the one shown.
    bar_displays: wgpu::Buffer,
    bar_bind_group: wgpu::BindGroup,
    /// The display bar on screen, if any.
    bar: Option<DisplayBar>,
    chat_pipeline: wgpu::RenderPipeline,
    /// The chat button on screen, if any.
    chat_button: Option<VideoRect>,
    handle_pipeline: wgpu::RenderPipeline,
    /// The handle on screen, if any.
    handle: Option<VideoRect>,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Result<Self, RenderError> {
        let size = window.inner_size();
        let Gpu {
            surface,
            adapter,
            device,
            queue,
        } = match preferred_backends() {
            Some(backends) => open_gpu(&window, Some(backends)).or_else(|e| {
                tracing::warn!("{backends:?}: {e}");
                open_gpu(&window, None)
            })?,
            None => open_gpu(&window, None)?,
        };

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| RenderError::Surface("surface not supported by adapter".into()))?;
        // The video is already gamma-encoded: render through a non-sRGB view so
        // the GPU does not encode it a second time.
        let view_format = config.format.remove_srgb_suffix();
        if view_format != config.format {
            config.view_formats.push(view_format);
        }
        let modes = surface.get_capabilities(&adapter).present_modes;
        config.present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|m| modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv planes"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(view_format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let button_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("button"),
            source: wgpu::ShaderSource::Wgsl(BUTTON_SHADER.into()),
        });
        let button_pipeline = |entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("button"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &button_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &button_shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: view_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let button_enter = button_pipeline("fs_enter");
        let button_leave = button_pipeline("fs_leave");
        let bar_pipeline = button_pipeline("fs_bar");
        let chat_pipeline = button_pipeline("fs_chat");
        let handle_pipeline = button_pipeline("fs_handle");
        let bar_displays = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("display bar"),
            size: 8,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bar_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display bar"),
            layout: &bar_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: bar_displays.as_entire_binding(),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            view_format,
            pipeline,
            layout,
            sampler,
            planes: None,
            button_enter,
            button_leave,
            button: None,
            bar_pipeline,
            bar_displays,
            bar_bind_group,
            bar: None,
            chat_pipeline,
            chat_button: None,
            handle_pipeline,
            handle: None,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Shows the full screen button, or hides it, from the next render.
    pub fn set_button(&mut self, button: Option<(ButtonAction, VideoRect)>) {
        self.button = button;
    }

    /// Shows the display bar, or hides it, from the next render.
    pub fn set_display_bar(&mut self, bar: Option<DisplayBar>) {
        self.bar = bar;
        if let Some(bar) = bar {
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&bar.count.to_le_bytes());
            bytes[4..].copy_from_slice(&bar.current.to_le_bytes());
            self.queue.write_buffer(&self.bar_displays, 0, &bytes);
        }
    }

    /// Shows the chat button, or hides it, from the next render.
    pub fn set_chat_button(&mut self, rect: Option<VideoRect>) {
        self.chat_button = rect;
    }

    /// Shows the handle, or hides it, from the next render.
    pub fn set_handle(&mut self, rect: Option<VideoRect>) {
        self.handle = rect;
    }

    /// Where the current video sits in the window, once a frame was uploaded.
    pub fn video_rect(&self) -> Option<VideoRect> {
        let planes = self.planes.as_ref()?;
        Some(letterbox(
            (self.config.width, self.config.height),
            (planes.width, planes.height),
        ))
    }

    pub fn upload(&mut self, frame: &YuvFrame) {
        if !matches!(&self.planes, Some(p) if (p.width, p.height) == (frame.width, frame.height)) {
            self.planes = Some(self.create_planes(frame.width, frame.height));
        }
        let planes = self.planes.as_ref().expect("planes created above");
        let (w, h) = (frame.width, frame.height);
        self.write_plane(&planes.y, &frame.y, w, h);
        self.write_plane(&planes.u, &frame.u, w / 2, h / 2);
        self.write_plane(&planes.v, &frame.v, w / 2, h / 2);
    }

    pub fn render(&mut self) -> Result<(), RenderError> {
        let mut texture = self.surface.get_current_texture();
        if matches!(
            texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
        ) {
            // Once more, set up again: a still screen sends no next frame.
            self.surface.configure(&self.device, &self.config);
            texture = self.surface.get_current_texture();
        }
        let texture = match texture {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            // Minimized or busy: skip this frame.
            _ => return Ok(()),
        };
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let (Some(planes), Some(rect)) = (&self.planes, self.video_rect()) {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &planes.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            let (width, height) = (self.config.width as f32, self.config.height as f32);
            // (A window too small for it goes without.)
            if let Some((action, rect)) = self.button
                && rect.x >= 0.0
                && rect.x + rect.width <= width
                && rect.y + rect.height <= height
            {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(match action {
                    ButtonAction::Enter => &self.button_enter,
                    ButtonAction::Leave => &self.button_leave,
                });
                pass.draw(0..3, 0..1);
            }
            if let Some(DisplayBar { rect, .. }) = self.bar
                && rect.x >= 0.0
                && rect.x + rect.width <= width
                && rect.y + rect.height <= height
            {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(&self.bar_pipeline);
                pass.set_bind_group(0, &self.bar_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            if let Some(rect) = self.chat_button
                && rect.x >= 0.0
                && rect.x + rect.width <= width
                && rect.y + rect.height <= height
            {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(&self.chat_pipeline);
                pass.draw(0..3, 0..1);
            }
            if let Some(rect) = self.handle
                && rect.x >= 0.0
                && rect.x + rect.width <= width
                && rect.y + rect.height <= height
            {
                pass.set_viewport(rect.x, rect.y, rect.width, rect.height, 0.0, 1.0);
                pass.set_pipeline(&self.handle_pipeline);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(texture);
        Ok(())
    }

    fn create_planes(&self, width: u32, height: u32) -> Planes {
        let plane = |label, w, h| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = plane("y", width, height);
        let u = plane("u", width / 2, height / 2);
        let v = plane("v", width / 2, height / 2);
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (yv, uv, vv) = (view(&y), view(&u), view(&v));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv planes"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&yv),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&vv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Planes {
            width,
            height,
            y,
            u,
            v,
            bind_group,
        }
    }

    fn write_plane(&self, texture: &wgpu::Texture, data: &[u8], width: u32, height: u32) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_sits_at_the_top_center() {
        let r = button_rect((1000, 800), 2.0);
        assert_eq!((r.x, r.y, r.width, r.height), (444.0, 12.0, 112.0, 56.0));
        assert!(r.contains(500.0, 40.0, 0.0));
        assert!(!r.contains(500.0, 80.0, 0.0));
        assert!(r.contains(500.0, 80.0, 56.0));
    }

    #[test]
    fn the_handle_sits_on_the_left_edge_halfway_down() {
        let h = handle_rect((1000, 800), 2.0);
        assert_eq!((h.x, h.y, h.width, h.height), (4.0, 336.0, 16.0, 128.0));
        assert_eq!(h.height, h.width * 8.0, "the shader draws it so");
    }

    #[test]
    fn the_shaders_are_valid() {
        use naga::valid::{Capabilities, ValidationFlags, Validator};
        for source in [SHADER, BUTTON_SHADER] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            Validator::new(ValidationFlags::all(), Capabilities::default())
                .validate(&module)
                .unwrap();
        }
    }

    #[test]
    fn the_display_bar_sits_left_of_the_button() {
        let button = button_rect((1000, 800), 2.0);
        let bar = DisplayBar::new(button, 3, 1, 2.0).unwrap();
        assert_eq!((bar.rect.y, bar.rect.height), (button.y, button.height));
        assert_eq!(bar.rect.x + bar.rect.width + 16.0, button.x);
        assert_eq!(bar.rect.width, 192.0);
    }

    #[test]
    fn each_part_of_the_display_bar_is_a_display() {
        let button = button_rect((1000, 800), 1.0);
        let bar = DisplayBar::new(button, 3, 0, 1.0).unwrap();
        let y = f64::from(bar.rect.y) + 2.0;
        let x = f64::from(bar.rect.x);
        assert_eq!(bar.display_at(x + 1.0, y), Some(0));
        assert_eq!(bar.display_at(x + 40.0, y), Some(1));
        assert_eq!(bar.display_at(x + 95.0, y), Some(2));
        assert_eq!(bar.display_at(x + 96.0, y), Some(2), "the last pixel");
        assert_eq!(bar.display_at(x - 1.0, y), None);
        assert_eq!(bar.display_at(x + 1.0, 500.0), None);
    }

    #[test]
    fn the_chat_button_sits_left_of_what_is_there() {
        let button = button_rect((1000, 800), 1.0);
        let chat = chat_button_rect(button.x, button, 1.0).unwrap();
        assert_eq!(chat.x + chat.width + 8.0, button.x);
        assert_eq!((chat.y, chat.height), (button.y, button.height));
        assert!(chat_button_rect(10.0, button, 1.0).is_none());
    }

    #[test]
    fn a_window_too_small_goes_without_the_display_bar() {
        let button = button_rect((200, 100), 1.0);
        assert!(DisplayBar::new(button, 9, 0, 1.0).is_none());
    }

    #[test]
    fn letterbox_wide_window() {
        let r = letterbox((2000, 1000), (1920, 1080));
        assert_eq!(r.height, 1000.0);
        assert!((r.width - 1777.78).abs() < 0.1);
        assert_eq!(r.y, 0.0);
        assert_eq!(r.x, 111.0);
    }

    #[test]
    fn normalize_inside_and_on_bars() {
        let r = letterbox((2000, 1000), (1920, 1080));
        let (nx, ny) = r.normalize(1000.0, 500.0).unwrap();
        assert!((nx - 0.5).abs() < 0.01 && (ny - 0.5).abs() < 0.01);
        assert_eq!(r.normalize(10.0, 500.0), None);
    }
}
