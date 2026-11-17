//! GPU canvas renderer: a texture-array tile cache with LRU eviction, an
//! instanced tile pipeline (checkerboard, pixel grid) and a marching-ants
//! pipeline. Driven through an egui paint callback.

use crate::surface::{TILE, TileRef};
use egui_wgpu::wgpu;
use std::collections::HashMap;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewUniform {
    pub origin: [f32; 2],
    pub scale: f32,
    pub time: f32,
    pub viewport: [f32; 2],
    pub flags: u32,
    pub checker: f32,
    pub image: [f32; 2],
    pub _pad: [f32; 2],
    /// Transparency checkerboard colours (sRGB 0..1, alpha unused).
    pub check_a: [f32; 4],
    pub check_b: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    slot: u32,
}

pub type TileKey = (u64, u8, u32, u32);

pub struct FrameTile {
    pub key: TileKey,
    pub version: u64,
    pub data: TileRef,
    pub rect: [f32; 4],
    pub uv: [f32; 4],
}

pub struct CanvasFrame {
    pub view: ViewUniform,
    pub tiles: Vec<FrameTile>,
    /// (version, segments)
    pub ants: Option<(u64, Arc<Vec<[f32; 4]>>)>,
}

pub struct GpuResources {
    tile_pipeline: wgpu::RenderPipeline,
    ant_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    texture: wgpu::Texture,
    slots: u32,
    cache: HashMap<TileKey, (u32, u64, u64)>, // slot, version, last used frame
    free: Vec<u32>,
    frame: u64,
    instances: wgpu::Buffer,
    instance_cap: usize,
    instance_count: u32,
    ants: wgpu::Buffer,
    ants_cap: usize,
    ants_count: u32,
    ants_version: u64,
}

impl GpuResources {
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let limits = device.limits();
        let slots = limits.max_texture_array_layers.min(2048).max(64);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile cache"),
            size: wgpu::Extent3d { width: TILE as u32, height: TILE as u32, depth_or_array_layers: slots },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let mk_sampler = |f: wgpu::FilterMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: None,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: f,
                min_filter: f,
                ..Default::default()
            })
        };
        let nearest = mk_sampler(wgpu::FilterMode::Nearest);
        let linear = mk_sampler(wgpu::FilterMode::Linear);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&nearest) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&linear) },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("canvas"), source: wgpu::ShaderSource::Wgsl(include_str!("canvas.wgsl").into()) });
        let tile_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Uint32];
        let ant_attrs = wgpu::vertex_attr_array![0 => Float32x4];
        let mk = |vs: &str, fs: &str, stride: usize, attrs: &[wgpu::VertexAttribute]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout { array_stride: stride as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: attrs })],
                },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: target, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let tile_pipeline = mk("vs_tile", "fs_tile", std::mem::size_of::<TileInstance>(), &tile_attrs);
        let ant_pipeline = mk("vs_ant", "fs_ant", 16, &ant_attrs);
        let instance_cap = 4096;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile instances"),
            size: (instance_cap * std::mem::size_of::<TileInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ants_cap = 4096;
        let ants = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ants"),
            size: (ants_cap * 16) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // slot 0 = permanently transparent tile
        let free = (1..slots).rev().collect();
        GpuResources {
            tile_pipeline,
            ant_pipeline,
            bind_group,
            uniform,
            texture,
            slots,
            cache: HashMap::new(),
            free,
            frame: 0,
            instances,
            instance_cap,
            instance_count: 0,
            ants,
            ants_cap,
            ants_count: 0,
            ants_version: u64::MAX,
        }
    }

    fn alloc_slot(&mut self, protect_frame: u64) -> Option<u32> {
        if let Some(s) = self.free.pop() {
            return Some(s);
        }
        // evict least recently used tile not used this frame
        let victim = self.cache.iter().filter(|(_, v)| v.2 < protect_frame).min_by_key(|(_, v)| v.2).map(|(k, v)| (*k, v.0));
        let (k, s) = victim?;
        self.cache.remove(&k);
        Some(s)
    }

    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &CanvasFrame) {
        self.frame += 1;
        let now = self.frame;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame.view));
        let mut inst = Vec::with_capacity(frame.tiles.len());
        for t in &frame.tiles {
            let Some(data) = &t.data else {
                inst.push(TileInstance { rect: t.rect, uv: t.uv, slot: 0 });
                continue;
            };
            let slot = match self.cache.get_mut(&t.key) {
                Some(e) if e.1 == t.version => {
                    e.2 = now;
                    Some(e.0)
                }
                Some(e) => {
                    e.1 = t.version;
                    e.2 = now;
                    let s = e.0;
                    self.upload(queue, s, &data.px[..]);
                    Some(s)
                }
                None => match self.alloc_slot(now) {
                    Some(s) => {
                        self.upload(queue, s, &data.px[..]);
                        self.cache.insert(t.key, (s, t.version, now));
                        Some(s)
                    }
                    None => None, // cache full this frame (huge viewport); skip tile
                },
            };
            if let Some(slot) = slot {
                inst.push(TileInstance { rect: t.rect, uv: t.uv, slot });
            }
        }
        if inst.len() > self.instance_cap {
            self.instance_cap = inst.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tile instances"),
                size: (self.instance_cap * std::mem::size_of::<TileInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !inst.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&inst));
        }
        self.instance_count = inst.len() as u32;

        match &frame.ants {
            None => self.ants_count = 0,
            Some((ver, segs)) => {
                if *ver != self.ants_version || self.ants_count as usize != segs.len() {
                    if segs.len() > self.ants_cap {
                        self.ants_cap = segs.len().next_power_of_two();
                        self.ants = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("ants"),
                            size: (self.ants_cap * 16) as u64,
                            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                    }
                    if !segs.is_empty() {
                        queue.write_buffer(&self.ants, 0, bytemuck::cast_slice(&segs[..]));
                    }
                    self.ants_version = *ver;
                }
                self.ants_count = segs.len() as u32;
            }
        }
    }

    fn upload(&self, queue: &wgpu::Queue, slot: u32, px: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: slot }, aspect: wgpu::TextureAspect::All },
            px,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE as u32 * 4), rows_per_image: Some(TILE as u32) },
            wgpu::Extent3d { width: TILE as u32, height: TILE as u32, depth_or_array_layers: 1 },
        );
    }

    pub fn clear_slot0(&self, queue: &wgpu::Queue) {
        self.upload(queue, 0, &vec![0u8; TILE * TILE * 4]);
    }

    pub fn paint(&self, pass: &mut wgpu::RenderPass<'static>) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        if self.instance_count > 0 {
            pass.set_pipeline(&self.tile_pipeline);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..4, 0..self.instance_count);
        }
        if self.ants_count > 0 {
            pass.set_pipeline(&self.ant_pipeline);
            pass.set_vertex_buffer(0, self.ants.slice(..));
            pass.draw(0..4, 0..self.ants_count);
        }
    }

    pub fn slot_count(&self) -> u32 {
        self.slots
    }
}

pub struct CanvasCallback {
    pub frame: Arc<CanvasFrame>,
}

impl egui_wgpu::CallbackTrait for CanvasCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _sd: &egui_wgpu::ScreenDescriptor,
        _enc: &mut wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = res.get_mut::<GpuResources>() {
            r.prepare(device, queue, &self.frame);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, res: &egui_wgpu::CallbackResources) {
        if let Some(r) = res.get::<GpuResources>() {
            r.paint(pass);
        }
    }
}
