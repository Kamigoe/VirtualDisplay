//! wgpu presentation. The decoder thread uploads planes straight from FFmpeg's buffers via
//! [`Uploader`]; the main thread draws whatever was uploaded last via [`Renderer`].

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use winit::window::Window;

use crate::decoder::{Layout, Picture};

struct VideoTextures {
    layout: Layout,
    width: u32,
    height: u32,
    full_range: bool,
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
}

#[derive(Default)]
struct Shared {
    video: Option<VideoTextures>,
    /// Sender capture time of the most recently uploaded picture.
    capture_us: u64,
    /// Bumped on every upload so the renderer can tell a new picture from a redraw.
    serial: u64,
}

struct Common {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    sampler_chroma: wgpu::Sampler,
    params: wgpu::Buffer,
    /// 1x1 texture bound to unused slots (tex_v for NV12).
    dummy: wgpu::TextureView,
    shared: Mutex<Shared>,
}

pub struct Uploader(Arc<Common>);

pub struct Renderer {
    common: Arc<Common>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    srgb_out: bool,
    presented_serial: u64,
}

/// Mirrors `Params` in shader.wgsl (32 bytes).
fn params_bytes(scale: [f32; 2], layout: Layout, full_range: bool, srgb_out: bool) -> [u8; 32] {
    let words: [u32; 8] = [
        scale[0].to_bits(),
        scale[1].to_bits(),
        (layout != Layout::Nv12) as u32,
        full_range as u32,
        srgb_out as u32,
        0,
        0,
        0,
    ];
    let mut out = [0u8; 32];
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
    }
    out
}

impl Renderer {
    pub fn new(window: Arc<Window>, vsync: bool) -> Result<(Renderer, Uploader)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance
            .create_surface(window.clone())
            .context("creating surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no suitable GPU adapter")?;
        let info = adapter.get_info();
        println!("gpu: {} ({:?})", info.name, info.backend);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("vdview"),
                ..Default::default()
            }))?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("surface not supported by adapter"))?;
        let caps = surface.get_capabilities(&adapter);
        // Prefer a plain 8-bit non-sRGB swapchain: the decoded values are already gamma-encoded.
        // Float formats (Rgba16Float, offered first on Wayland/RADV) may be composited as linear.
        let preferred = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm];
        if let Some(f) = preferred.into_iter().find(|f| caps.formats.contains(f)) {
            config.format = f;
        }
        let srgb_out = config.format.is_srgb();
        config.present_mode = if vsync {
            wgpu::PresentMode::Fifo
        } else {
            [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
                .into_iter()
                .find(|m| caps.present_modes.contains(m))
                .unwrap_or(wgpu::PresentMode::Fifo)
        };
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        println!(
            "present: {:?}, format {:?}",
            config.present_mode, config.format
        );

        let (common, pipeline) = Common::build(device, queue, config.format);
        let renderer = Renderer {
            common: common.clone(),
            surface,
            config,
            pipeline,
            srgb_out,
            presented_serial: 0,
        };
        Ok((renderer, Uploader(common)))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.common.device, &self.config);
    }

    /// Draws the latest picture. Returns its sender capture time if it had not been presented before.
    pub fn render(&mut self) -> Option<u64> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.common.device, &self.config);
                return None;
            }
            _ => return None,
        };
        let view = frame.texture.create_view(&Default::default());
        let c = &self.common;
        let mut encoder = c.device.create_command_encoder(&Default::default());
        let drawn = c.draw(
            &mut encoder,
            &view,
            &self.pipeline,
            (self.config.width, self.config.height),
            self.srgb_out,
        );
        let fresh = match drawn {
            Some((serial, capture_us)) if serial != self.presented_serial => {
                self.presented_serial = serial;
                Some(capture_us)
            }
            _ => None,
        };
        c.queue.submit([encoder.finish()]);
        c.queue.present(frame);
        fresh
    }
}

impl Common {
    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> (Arc<Common>, wgpu::RenderPipeline) {
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video"),
            bind_group_layouts: &[Some(&bind_layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sampler_chroma = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("chroma"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummy = create_plane(&device, 1, 1, wgpu::TextureFormat::R8Unorm)
            .create_view(&Default::default());

        let common = Arc::new(Common {
            device,
            queue,
            bind_layout,
            sampler,
            sampler_chroma,
            params,
            dummy,
            shared: Mutex::new(Shared::default()),
        });
        (common, pipeline)
    }

    /// Records a pass drawing the latest picture aspect-fitted into `view`.
    /// Returns the upload serial and capture time of the picture drawn, if any.
    fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        (tw, th): (u32, u32),
        srgb_out: bool,
    ) -> Option<(u64, u64)> {
        let shared = self.shared.lock().unwrap();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("video"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let v = shared.video.as_ref()?;
        let (va, sa) = (v.width as f32 / v.height as f32, tw as f32 / th as f32);
        let scale = if va > sa {
            [1.0, sa / va]
        } else {
            [va / sa, 1.0]
        };
        self.queue.write_buffer(
            &self.params,
            0,
            &params_bytes(scale, v.layout, v.full_range, srgb_out),
        );
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &v.bind_group, &[]);
        pass.draw(0..6, 0..1);
        Some((shared.serial, shared.capture_us))
    }
}

impl Uploader {
    /// Called on the decoder thread; copies the planes into GPU textures.
    pub fn upload(&self, pic: &Picture, capture_us: u64) {
        let c = &self.0;
        let mut shared = c.shared.lock().unwrap();
        let stale = shared.video.as_ref().is_none_or(|v| {
            v.layout != pic.layout
                || v.width != pic.width
                || v.height != pic.height
                || v.full_range != pic.full_range
        });
        if stale {
            shared.video = Some(self.create(pic));
        }
        let video = shared.video.as_ref().unwrap();
        for (plane, tex) in pic.planes.iter().zip(&video.textures) {
            c.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                plane.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(plane.stride),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: plane.width,
                    height: plane.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        shared.capture_us = capture_us;
        shared.serial += 1;
    }

    fn create(&self, pic: &Picture) -> VideoTextures {
        let c = &self.0;
        let textures: Vec<wgpu::Texture> = pic
            .planes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let format = if pic.layout == Layout::Nv12 && i == 1 {
                    wgpu::TextureFormat::Rg8Unorm
                } else {
                    wgpu::TextureFormat::R8Unorm
                };
                create_plane(&c.device, p.width, p.height, format)
            })
            .collect();
        let views: Vec<wgpu::TextureView> = textures
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let v_view = views.get(2).unwrap_or(&c.dummy);
        let bind_group = c.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video"),
            layout: &c.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(v_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&c.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: c.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&c.sampler_chroma),
                },
            ],
        });
        println!(
            "video: {}x{} {:?}{}",
            pic.width,
            pic.height,
            pic.layout,
            if pic.full_range { " full-range" } else { "" }
        );
        VideoTextures {
            layout: pic.layout,
            width: pic.width,
            height: pic.height,
            full_range: pic.full_range,
            textures,
            bind_group,
        }
    }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_plane(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("plane"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Renders one picture offscreen and returns tightly packed RGBA8 pixels. Used by `--snapshot`
/// to check decoding and colour conversion on machines without a usable display.
pub fn snapshot(pic: &Picture) -> Result<Vec<u8>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .context("no GPU adapter")?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default()))?;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (common, pipeline) = Common::build(device, queue, format);
    Uploader(common.clone()).upload(pic, 0);

    let (w, h) = (pic.width, pic.height);
    let target = common.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let padded =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = common.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = common.device.create_command_encoder(&Default::default());
    common.draw(
        &mut encoder,
        &target.create_view(&Default::default()),
        &pipeline,
        (w, h),
        false,
    );
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    common.queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |r| r.expect("map readback buffer"));
    common.device.poll(wgpu::PollType::wait_indefinitely())?;
    let mapped = readback.get_mapped_range(..)?;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in mapped.chunks(padded as usize) {
        out.extend_from_slice(&row[..(w * 4) as usize]);
    }
    Ok(out)
}
