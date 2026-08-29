//! GPU rendering infrastructure for genomic display lists.

use ab_glyph::{Font, FontRef, ScaleFont, point};
use bytemuck::{Pod, Zeroable};
use std::{collections::HashMap, path::Path};

use image::{ImageError, RgbaImage};
use rgv_layout::{
    Direction, DisplayList, RectPrimitive, RectShape, TextPrimitive, breakend_bracket_dimensions,
    junction_arc_distance_to_point, junction_arc_stroke_width, read_arrow_head_width,
};
use wgpu::util::DeviceExt;

const RECTANGLE_SHADER: &str = include_str!("rectangle.wgsl");
const NAVIGATION_PREVIEW_SHADER: &str = include_str!("navigation_preview.wgsl");
const MAX_UPLOAD_BYTES_PER_FRAME: usize = 16 * 1024 * 1024;
pub const MAX_NAVIGATION_PREVIEW_BYTES: u64 = 128 * 1024 * 1024;

fn navigation_preview_texture_bytes(width: u32, height: u32) -> Option<u64> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    (width > 0 && height > 0 && bytes <= MAX_NAVIGATION_PREVIEW_BYTES).then_some(bytes)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct RectangleInstance {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub color: [f32; 4],
    pub object_id: u32,
    padding: [u32; 3],
}

impl From<RectPrimitive> for RectangleInstance {
    fn from(value: RectPrimitive) -> Self {
        Self {
            origin: [value.x, value.y],
            size: [value.width, value.height],
            color: [
                value.color.red,
                value.color.green,
                value.color.blue,
                value.color.alpha,
            ],
            object_id: value.object_id,
            padding: [value.shape as u32, 0, 0],
        }
    }
}

impl RectangleInstance {
    pub fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x4,
            3 => Uint32,
            4 => Uint32x3
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

fn rectangle_instance_bytes(instances: &[RectangleInstance]) -> usize {
    std::mem::size_of_val(instances)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ScreenUniform {
    size: [f32; 2],
    selected_object_id: u32,
    hovered_object_id: u32,
}

/// Instanced renderer for the dominant primitive in an alignment view.
pub struct RectangleRenderer {
    pipeline: wgpu::RenderPipeline,
    screen_buffer: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,
    neutral_screen_buffer: wgpu::Buffer,
    neutral_screen_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    cached_instances: Vec<RectangleInstance>,
    scratch_instances: Vec<RectangleInstance>,
    cached_slabs: HashMap<u64, CachedRectangleSlab>,
    active_slab_key: Option<u64>,
    slab_revision: u64,
    last_prepare_stats: PrepareStats,
}

struct CachedRectangleSlab {
    buffer: wgpu::Buffer,
    instance_count: u32,
    allocated_bytes: usize,
    last_used_revision: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrepareStats {
    pub total_instances: usize,
    pub uploaded_instances: usize,
    pub uploaded_bytes: usize,
    pub buffer_reallocated: bool,
    pub cpu_capacity_grew: bool,
    pub truncated: bool,
    pub slab_cache_hit: bool,
    pub slab_entries: usize,
    pub slab_resident_bytes: usize,
}

impl RectangleRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RGV rectangle shader"),
            source: wgpu::ShaderSource::Wgsl(RECTANGLE_SHADER.into()),
        });
        let screen_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("RGV screen uniform"),
            contents: bytemuck::bytes_of(&ScreenUniform {
                size: [1.0, 1.0],
                selected_object_id: 0,
                hovered_object_id: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV screen layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: screen_uniform_visibility(),
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let screen_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV screen bind group"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buffer.as_entire_binding(),
            }],
        });
        let neutral_screen_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("RGV neutral rectangle screen uniform"),
            contents: bytemuck::bytes_of(&ScreenUniform {
                size: [1.0, 1.0],
                selected_object_id: 0,
                hovered_object_id: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let neutral_screen_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV neutral rectangle screen bind group"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: neutral_screen_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV rectangle pipeline layout"),
            bind_group_layouts: &[Some(&screen_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV rectangle pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[RectangleInstance::vertex_buffer_layout()],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let instance_capacity = 1;
        let instance_buffer = create_instance_buffer(device, instance_capacity);

        Self {
            pipeline,
            screen_buffer,
            screen_bind_group,
            neutral_screen_buffer,
            neutral_screen_bind_group,
            instance_buffer,
            instance_capacity,
            instance_count: 0,
            cached_instances: Vec::new(),
            scratch_instances: Vec::new(),
            cached_slabs: HashMap::new(),
            active_slab_key: None,
            slab_revision: 0,
            last_prepare_stats: PrepareStats::default(),
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_size_points: [f32; 2],
        display_list: &DisplayList,
        selected_object_id: Option<u32>,
        hovered_object_id: Option<u32>,
    ) {
        self.prepare_with_slab_key(
            device,
            queue,
            screen_size_points,
            display_list,
            selected_object_id,
            hovered_object_id,
            None,
            0,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_with_slab_key(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_size_points: [f32; 2],
        display_list: &DisplayList,
        selected_object_id: Option<u32>,
        hovered_object_id: Option<u32>,
        slab_key: Option<u64>,
        maximum_slab_bytes: usize,
    ) {
        let screen = ScreenUniform {
            size: [
                screen_size_points[0].max(1.0),
                screen_size_points[1].max(1.0),
            ],
            selected_object_id: selected_object_id.unwrap_or_default(),
            hovered_object_id: hovered_object_id.unwrap_or_default(),
        };
        queue.write_buffer(&self.screen_buffer, 0, bytemuck::bytes_of(&screen));
        queue.write_buffer(
            &self.neutral_screen_buffer,
            0,
            bytemuck::bytes_of(&ScreenUniform {
                size: screen.size,
                selected_object_id: 0,
                hovered_object_id: 0,
            }),
        );

        self.slab_revision = self.slab_revision.saturating_add(1);
        if let Some(key) = slab_key
            && let Some(slab) = self.cached_slabs.get_mut(&key)
        {
            slab.last_used_revision = self.slab_revision;
            self.active_slab_key = Some(key);
            self.instance_count = slab.instance_count;
            self.trim_cached_slabs(maximum_slab_bytes);
            self.last_prepare_stats = PrepareStats {
                total_instances: self.instance_count as usize,
                slab_cache_hit: true,
                slab_entries: self.cached_slabs.len(),
                slab_resident_bytes: self.slab_resident_bytes(),
                ..PrepareStats::default()
            };
            return;
        }

        let maximum_instances =
            MAX_UPLOAD_BYTES_PER_FRAME / std::mem::size_of::<RectangleInstance>();
        self.scratch_instances.clear();
        let previous_cpu_capacity = self.scratch_instances.capacity();
        self.scratch_instances.extend(
            display_list
                .rectangles
                .iter()
                .take(maximum_instances)
                .copied()
                .map(RectangleInstance::from),
        );
        let instances = &self.scratch_instances;
        if self.cached_instances.capacity() < instances.capacity() {
            self.cached_instances.reserve(
                instances
                    .capacity()
                    .saturating_sub(self.cached_instances.len()),
            );
        }
        let mut stats = PrepareStats {
            total_instances: instances.len(),
            truncated: display_list.rectangles.len() > instances.len(),
            cpu_capacity_grew: instances.capacity() > previous_cpu_capacity,
            ..PrepareStats::default()
        };
        let allocated_bytes = instances
            .len()
            .max(1)
            .next_power_of_two()
            .saturating_mul(std::mem::size_of::<RectangleInstance>());
        if let Some(key) = slab_key.filter(|_| allocated_bytes <= maximum_slab_bytes.max(1)) {
            let capacity = instances.len().max(1).next_power_of_two();
            let uploaded_instances = instances.len();
            let uploaded_bytes = rectangle_instance_bytes(instances);
            let buffer = create_instance_buffer(device, capacity);
            if !instances.is_empty() {
                queue.write_buffer(&buffer, 0, bytemuck::cast_slice(instances));
            }
            self.instance_count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
            self.cached_slabs.insert(
                key,
                CachedRectangleSlab {
                    buffer,
                    instance_count: self.instance_count,
                    allocated_bytes,
                    last_used_revision: self.slab_revision,
                },
            );
            self.active_slab_key = Some(key);
            stats.uploaded_instances = uploaded_instances;
            stats.uploaded_bytes = uploaded_bytes;
            stats.buffer_reallocated = true;
            self.trim_cached_slabs(maximum_slab_bytes);
            stats.slab_entries = self.cached_slabs.len();
            stats.slab_resident_bytes = self.slab_resident_bytes();
            self.last_prepare_stats = stats;
            return;
        }
        self.active_slab_key = None;
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
            stats.buffer_reallocated = true;
        }
        let dirty = if stats.buffer_reallocated {
            (!instances.is_empty()).then_some(0..instances.len())
        } else {
            changed_instance_range(&self.cached_instances, instances)
        };
        if let Some(range) = dirty {
            let offset = range.start * std::mem::size_of::<RectangleInstance>();
            let changed = &instances[range.clone()];
            queue.write_buffer(
                &self.instance_buffer,
                offset as wgpu::BufferAddress,
                bytemuck::cast_slice(changed),
            );
            stats.uploaded_instances = changed.len();
            stats.uploaded_bytes = rectangle_instance_bytes(changed);
        }
        self.instance_count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        std::mem::swap(&mut self.cached_instances, &mut self.scratch_instances);
        self.last_prepare_stats = stats;
    }

    pub fn render<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let (instance_buffer, instance_count) = self.active_buffer_and_count();
        if instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.screen_bind_group, &[]);
        pass.set_vertex_buffer(0, instance_buffer.slice(..));
        pass.draw(0..6, 0..instance_count);
    }

    pub fn render_unhighlighted<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let (instance_buffer, instance_count) = self.active_buffer_and_count();
        if instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.neutral_screen_bind_group, &[]);
        pass.set_vertex_buffer(0, instance_buffer.slice(..));
        pass.draw(0..6, 0..instance_count);
    }

    pub const fn instance_count(&self) -> u32 {
        self.instance_count
    }

    pub const fn last_prepare_stats(&self) -> PrepareStats {
        self.last_prepare_stats
    }

    /// Applies a live GPU slab ceiling. Inactive least-recent slabs are
    /// released immediately; the active slab remains pinned until another
    /// authoritative frame replaces it.
    pub fn set_maximum_slab_bytes(&mut self, maximum_slab_bytes: usize) {
        self.trim_cached_slabs(maximum_slab_bytes.max(1));
        self.last_prepare_stats.slab_entries = self.cached_slabs.len();
        self.last_prepare_stats.slab_resident_bytes = self.slab_resident_bytes();
    }

    fn active_buffer_and_count(&self) -> (&wgpu::Buffer, u32) {
        self.active_slab_key
            .and_then(|key| self.cached_slabs.get(&key))
            .map_or((&self.instance_buffer, self.instance_count), |slab| {
                (&slab.buffer, slab.instance_count)
            })
    }

    fn slab_resident_bytes(&self) -> usize {
        self.cached_slabs.values().fold(0_usize, |total, slab| {
            total.saturating_add(slab.allocated_bytes)
        })
    }

    fn trim_cached_slabs(&mut self, maximum_slab_bytes: usize) {
        while self.slab_resident_bytes() > maximum_slab_bytes {
            let Some(key) = self
                .cached_slabs
                .iter()
                .filter(|(key, _)| Some(**key) != self.active_slab_key)
                .min_by_key(|(_, slab)| slab.last_used_revision)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.cached_slabs.remove(&key);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct NavigationPreviewInstance {
    pub destination_origin: [f32; 2],
    pub destination_size: [f32; 2],
    pub source_origin: [f32; 2],
    pub source_size: [f32; 2],
    /// Source-local x = destination-local x * scale + offset. The third
    /// component is 1 for a valid source and 0 for a neutral fallback.
    pub sample_transform: [f32; 4],
}

impl NavigationPreviewInstance {
    pub fn new(
        destination_origin: [f32; 2],
        destination_size: [f32; 2],
        source_origin: [f32; 2],
        source_size: [f32; 2],
        source_interval: Option<(f64, f64)>,
        target_interval: (f64, f64),
    ) -> Self {
        let transform = source_interval.and_then(|(source_start, source_end)| {
            let source_length = source_end - source_start;
            let target_length = target_interval.1 - target_interval.0;
            (source_start.is_finite()
                && source_end.is_finite()
                && target_interval.0.is_finite()
                && target_interval.1.is_finite()
                && source_length > 0.0
                && target_length > 0.0)
                .then(|| {
                    [
                        (target_length / source_length) as f32,
                        ((target_interval.0 - source_start) / source_length) as f32,
                        1.0,
                        0.0,
                    ]
                })
        });
        Self {
            destination_origin,
            destination_size,
            source_origin,
            source_size,
            sample_transform: transform.unwrap_or([1.0, 0.0, 0.0, 0.0]),
        }
    }

    pub fn source_x_for_destination_fraction(self, fraction: f32) -> Option<f32> {
        (self.sample_transform[2] >= 0.5)
            .then(|| fraction.mul_add(self.sample_transform[0], self.sample_transform[1]))
    }

    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x2,
            3 => Float32x2,
            4 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

/// Converts an integer capture rectangle into filter-safe normalized UVs.
/// Sampling texel centres prevents linear filtering from blending pixels from
/// adjacent panels or window chrome at a sub-rectangle boundary.
pub fn navigation_preview_source_uv(
    origin: [u32; 2],
    size: [u32; 2],
    texture_size: [u32; 2],
) -> ([f32; 2], [f32; 2]) {
    let texture_width = texture_size[0].max(1) as f32;
    let texture_height = texture_size[1].max(1) as f32;
    (
        [
            (origin[0] as f32 + 0.5) / texture_width,
            (origin[1] as f32 + 0.5) / texture_height,
        ],
        [
            size[0].saturating_sub(1) as f32 / texture_width,
            size[1].saturating_sub(1) as f32 / texture_height,
        ],
    )
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct NavigationPreviewScreen {
    size: [f32; 2],
    padding: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NavigationPreviewStats {
    pub allocated_bytes: u64,
    pub texture_captures: u64,
    pub texture_composites: u64,
    pub texture_invalidations: u64,
    pub texture_fallbacks: u64,
    pub width: u32,
    pub height: u32,
}

struct NavigationPreviewTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub struct NavigationPreviewRenderer {
    preview_pipeline: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    screen_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    texture: Option<NavigationPreviewTexture>,
    fallback_texture: NavigationPreviewTexture,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    needs_clear: bool,
    stats: NavigationPreviewStats,
}

impl NavigationPreviewRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RGV navigation preview shader"),
            source: wgpu::ShaderSource::Wgsl(NAVIGATION_PREVIEW_SHADER.into()),
        });
        let screen_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("RGV navigation preview screen uniform"),
            contents: bytemuck::bytes_of(&NavigationPreviewScreen {
                size: [1.0, 1.0],
                padding: [0.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("RGV navigation preview sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV navigation preview bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let preview_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV navigation preview pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let preview_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV navigation preview pipeline"),
            layout: Some(&preview_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("preview_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[NavigationPreviewInstance::vertex_buffer_layout()],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("preview_fragment"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let background_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV navigation preview background layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let background_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV navigation preview background pipeline"),
            layout: Some(&background_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("background_vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("background_fragment"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let instance_capacity = 1;
        let instance_buffer = create_navigation_preview_instance_buffer(device, instance_capacity);
        let fallback_gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGV navigation preview fallback texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let fallback_view =
            fallback_gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let fallback_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV navigation preview fallback bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: screen_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&fallback_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let fallback_texture = NavigationPreviewTexture {
            _texture: fallback_gpu_texture,
            view: fallback_view,
            bind_group: fallback_bind_group,
        };
        Self {
            preview_pipeline,
            background_pipeline,
            bind_group_layout,
            sampler,
            screen_buffer,
            instance_buffer,
            instance_capacity,
            instance_count: 0,
            texture: None,
            fallback_texture,
            format,
            size: [0, 0],
            needs_clear: false,
            stats: NavigationPreviewStats::default(),
        }
    }

    pub fn ensure_texture(&mut self, device: &wgpu::Device, width: u32, height: u32) -> bool {
        let Some(requested_bytes) = navigation_preview_texture_bytes(width, height) else {
            self.invalidate_texture();
            self.stats.texture_fallbacks = self.stats.texture_fallbacks.saturating_add(1);
            return false;
        };
        if self.texture.is_some() && self.size == [width, height] {
            return true;
        }
        if self.texture.is_some() {
            self.stats.texture_invalidations = self.stats.texture_invalidations.saturating_add(1);
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGV committed genomic canvas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV navigation preview bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.screen_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.texture = Some(NavigationPreviewTexture {
            _texture: texture,
            view,
            bind_group,
        });
        self.size = [width, height];
        self.needs_clear = true;
        self.stats.allocated_bytes = requested_bytes;
        self.stats.width = width;
        self.stats.height = height;
        true
    }

    /// Releases the retained evidentiary source immediately. The 1x1 neutral
    /// fallback remains resident so checking frames can still be rendered
    /// without allocating a pathological full-size texture.
    pub fn invalidate_texture(&mut self) {
        if self.texture.take().is_some() {
            self.stats.texture_invalidations = self.stats.texture_invalidations.saturating_add(1);
        }
        self.size = [0, 0];
        self.needs_clear = false;
        self.stats.allocated_bytes = 0;
        self.stats.width = 0;
        self.stats.height = 0;
    }

    pub fn capture_committed_regions(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        rectangle_renderer: &RectangleRenderer,
        regions: &[[u32; 4]],
    ) {
        let Some(texture) = self.texture.as_ref() else {
            return;
        };
        let load = if self.needs_clear {
            wgpu::LoadOp::Clear(wgpu::Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            })
        } else {
            wgpu::LoadOp::Load
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("RGV committed genomic canvas capture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        for [x, y, width, height] in regions.iter().copied() {
            if width == 0 || height == 0 {
                continue;
            }
            pass.set_scissor_rect(x, y, width, height);
            pass.set_pipeline(&self.background_pipeline);
            pass.draw(0..3, 0..1);
            rectangle_renderer.render_unhighlighted(&mut pass);
            self.stats.texture_captures = self.stats.texture_captures.saturating_add(1);
        }
        drop(pass);
        self.needs_clear = false;
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_size: [u32; 2],
        instances: &[NavigationPreviewInstance],
        composite_targets: u64,
    ) {
        queue.write_buffer(
            &self.screen_buffer,
            0,
            bytemuck::bytes_of(&NavigationPreviewScreen {
                size: [screen_size[0].max(1) as f32, screen_size[1].max(1) as f32],
                padding: [0.0; 2],
            }),
        );
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instance_buffer =
                create_navigation_preview_instance_buffer(device, self.instance_capacity);
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
        self.instance_count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        self.stats.texture_composites = self
            .stats
            .texture_composites
            .saturating_add(u64::from(self.instance_count).saturating_mul(composite_targets));
    }

    pub fn render<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.instance_count == 0 {
            return;
        }
        let texture = self.texture.as_ref().unwrap_or(&self.fallback_texture);
        pass.set_pipeline(&self.preview_pipeline);
        pass.set_bind_group(0, &texture.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.instance_count);
    }

    pub const fn texture_available(&self) -> bool {
        self.texture.is_some()
    }

    pub const fn stats(&self) -> NavigationPreviewStats {
        self.stats
    }
}

fn create_navigation_preview_instance_buffer(
    device: &wgpu::Device,
    capacity: usize,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("RGV navigation preview instances"),
        size: (capacity * std::mem::size_of::<NavigationPreviewInstance>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn screen_uniform_visibility() -> wgpu::ShaderStages {
    wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT
}

fn changed_instance_range(
    previous: &[RectangleInstance],
    current: &[RectangleInstance],
) -> Option<std::ops::Range<usize>> {
    let shared_length = previous.len().min(current.len());
    let first = (0..shared_length)
        .find(|index| previous[*index] != current[*index])
        .unwrap_or(shared_length);
    if previous.len() != current.len() {
        return (first < current.len()).then_some(first..current.len());
    }
    if first == shared_length {
        return None;
    }
    let mut previous_end = previous.len();
    let mut current_end = current.len();
    while previous_end > first
        && current_end > first
        && previous[previous_end - 1] == current[current_end - 1]
    {
        previous_end -= 1;
        current_end -= 1;
    }
    (first < current_end).then_some(first..current_end)
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("RGV rectangle instances"),
        size: (capacity * std::mem::size_of::<RectangleInstance>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub fn export_png(
    display_list: &DisplayList,
    width: u32,
    height: u32,
    path: impl AsRef<Path>,
) -> Result<(), ImageError> {
    let mut image = RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    for rectangle in &display_list.rectangles {
        rasterize_rectangle(&mut image, *rectangle);
    }
    for text in &display_list.texts {
        rasterize_text(&mut image, text);
    }
    image.save_with_format(path, image::ImageFormat::Png)
}

fn rasterize_text(image: &mut RgbaImage, text: &TextPrimitive) {
    let font = FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR)
        .expect("egui's embedded Hack font is valid");
    let scaled = font.as_scaled(text.font_size);
    let mut width = 0.0;
    let mut previous = None;
    for character in text.text.chars() {
        let glyph = scaled.glyph_id(character);
        if let Some(previous) = previous {
            width += scaled.kern(previous, glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(glyph);
    }

    let mut cursor_x = text.x - width * 0.5;
    let baseline_y = text.y + (scaled.ascent() + scaled.descent()) * 0.5;
    previous = None;
    for character in text.text.chars() {
        let glyph_id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            cursor_x += scaled.kern(previous, glyph_id);
        }
        let glyph = glyph_id.with_scale_and_position(text.font_size, point(cursor_x, baseline_y));
        cursor_x += scaled.h_advance(glyph_id);
        previous = Some(glyph_id);
        let Some(outlined) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outlined.px_bounds();
        outlined.draw(|x, y, coverage| {
            let pixel_x = bounds.min.x.floor() as i64 + i64::from(x);
            let pixel_y = bounds.min.y.floor() as i64 + i64::from(y);
            if pixel_x < 0
                || pixel_y < 0
                || pixel_x >= i64::from(image.width())
                || pixel_y >= i64::from(image.height())
            {
                return;
            }
            let destination = image.get_pixel_mut(pixel_x as u32, pixel_y as u32);
            blend_pixel(destination, text.color, coverage);
        });
    }
}

fn blend_pixel(destination: &mut image::Rgba<u8>, color: rgv_layout::Rgba, coverage: f32) {
    let source_alpha = color.alpha.clamp(0.0, 1.0) * coverage.clamp(0.0, 1.0);
    let source = [
        color.red.clamp(0.0, 1.0),
        color.green.clamp(0.0, 1.0),
        color.blue.clamp(0.0, 1.0),
    ];
    for channel in 0..3 {
        let destination_value = f32::from(destination[channel]) / 255.0;
        destination[channel] =
            ((source[channel] * source_alpha + destination_value * (1.0 - source_alpha)) * 255.0)
                .round() as u8;
    }
    destination[3] = 255;
}

fn rasterize_rectangle(image: &mut RgbaImage, rectangle: RectPrimitive) {
    let left = rectangle.x.floor().max(0.0) as u32;
    let top = rectangle.y.floor().max(0.0) as u32;
    let right = (rectangle.x + rectangle.width)
        .ceil()
        .clamp(0.0, image.width() as f32) as u32;
    let bottom = (rectangle.y + rectangle.height)
        .ceil()
        .clamp(0.0, image.height() as f32) as u32;
    for y in top..bottom {
        for x in left..right {
            if !rectangle_contains_pixel(rectangle, x, y) {
                continue;
            }
            let destination = image.get_pixel_mut(x, y);
            blend_pixel(destination, rectangle.color, 1.0);
        }
    }
}

fn rectangle_contains_pixel(rectangle: RectPrimitive, x: u32, y: u32) -> bool {
    let local_x = x as f32 + 0.5 - rectangle.x;
    let local_y = y as f32 + 0.5 - rectangle.y;
    if rectangle.shape.is_junction_arc() {
        let stroke = junction_arc_stroke_width(rectangle.height)
            + rectangle.shape.junction_arc_interaction_expansion() * 2.0;
        return junction_arc_distance_to_point(rectangle, x as f32 + 0.5, y as f32 + 0.5)
            .is_some_and(|distance| distance <= stroke * 0.5);
    }
    if matches!(
        rectangle.shape,
        RectShape::BreakendJoinBefore | RectShape::BreakendJoinAfter
    ) {
        let (bar_width, stem_half_height) =
            breakend_bracket_dimensions(rectangle.width, rectangle.height);
        let in_bar = match rectangle.shape {
            RectShape::BreakendJoinBefore => local_x <= bar_width,
            RectShape::BreakendJoinAfter => local_x >= rectangle.width - bar_width,
            _ => unreachable!(),
        };
        let in_stem = (local_y - rectangle.height * 0.5).abs() <= stem_half_height;
        return in_bar || in_stem;
    }
    let Some(direction) = rectangle.shape.direction() else {
        return true;
    };
    let terminal = matches!(
        rectangle.shape,
        RectShape::DirectionalTerminalLeft | RectShape::DirectionalTerminalRight
    );
    let head_width = if terminal {
        read_arrow_head_width(rectangle.width, rectangle.height)
    } else {
        rectangle.width
    };
    let distance_from_tip = match direction {
        Direction::Left => local_x,
        Direction::Right => rectangle.width - local_x,
    };
    if distance_from_tip >= head_width {
        return true;
    }
    let half_height = rectangle.height * 0.5;
    let allowed_half_height = half_height * (distance_from_tip / head_width).clamp(0.0, 1.0);
    (local_y - half_height).abs() <= allowed_half_height
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval, UltimaDisplayOptions, Viewport};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentQuery, AlignmentRecord, AuxField, AuxValue,
        CigarOp, CigarOpKind, QueryStats, ReferenceSlice, SequenceData,
    };
    use rgv_layout::{
        AlignmentColorMode, AlignmentLayoutOptions, RectPrimitive, Rgba, layout_alignment_query,
    };

    use super::*;

    #[test]
    fn navigation_preview_transform_preserves_genomic_mapping() {
        let preview = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some((100.0, 200.0)),
            (50.0, 250.0),
        );
        assert_eq!(preview.source_x_for_destination_fraction(0.0), Some(-0.5));
        assert_eq!(preview.source_x_for_destination_fraction(0.25), Some(0.0));
        assert_eq!(preview.source_x_for_destination_fraction(0.5), Some(0.5));
        assert_eq!(preview.source_x_for_destination_fraction(0.75), Some(1.0));
        assert_eq!(preview.source_x_for_destination_fraction(1.0), Some(1.5));
    }

    #[test]
    fn navigation_preview_transform_samples_a_zoomed_in_subset() {
        let preview = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some((50.0, 250.0)),
            (100.0, 200.0),
        );
        assert_eq!(preview.source_x_for_destination_fraction(0.0), Some(0.25));
        assert_eq!(preview.source_x_for_destination_fraction(0.5), Some(0.5));
        assert_eq!(preview.source_x_for_destination_fraction(1.0), Some(0.75));
    }

    #[test]
    fn navigation_preview_transform_preserves_fractional_base_camera_state() {
        let preview = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some((100.25, 200.25)),
            (125.25, 175.25),
        );
        assert_eq!(preview.source_x_for_destination_fraction(0.0), Some(0.25));
        assert_eq!(preview.source_x_for_destination_fraction(0.5), Some(0.5));
        assert_eq!(preview.source_x_for_destination_fraction(1.0), Some(0.75));
    }

    #[test]
    fn navigation_preview_transform_handles_single_base_and_contig_edge_views() {
        let single_base = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some((0.0, 1.0)),
            (0.0, 1.0),
        );
        assert_eq!(
            single_base.source_x_for_destination_fraction(0.0),
            Some(0.0)
        );
        assert_eq!(
            single_base.source_x_for_destination_fraction(1.0),
            Some(1.0)
        );

        let zoomed_out_at_edge = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some((0.0, 100.0)),
            (0.0, 200.0),
        );
        assert_eq!(
            zoomed_out_at_edge.source_x_for_destination_fraction(0.0),
            Some(0.0)
        );
        assert_eq!(
            zoomed_out_at_edge.source_x_for_destination_fraction(0.5),
            Some(1.0)
        );
    }

    #[test]
    fn repeated_mixed_targets_always_map_from_the_original_committed_source() {
        let source = (100.25, 200.25);
        let genomic_anchor = 150.25;
        for target in [
            (75.25, 225.25),
            (125.25, 175.25),
            (50.25, 250.25),
            (140.125, 190.125),
        ] {
            let preview = NavigationPreviewInstance::new(
                [0.0, 0.0],
                [1_000.0, 600.0],
                [0.0, 0.0],
                [1.0, 1.0],
                Some(source),
                target,
            );
            let target_fraction = ((genomic_anchor - target.0) / (target.1 - target.0)) as f32;
            let expected_source_fraction =
                ((genomic_anchor - source.0) / (source.1 - source.0)) as f32;
            let mapped = preview
                .source_x_for_destination_fraction(target_fraction)
                .unwrap();
            assert!((mapped - expected_source_fraction).abs() < 1.0e-6);
        }
    }

    #[test]
    fn navigation_preview_without_a_source_uses_neutral_fallback() {
        let preview = NavigationPreviewInstance::new(
            [0.0, 0.0],
            [1_000.0, 600.0],
            [0.0, 0.0],
            [1.0, 1.0],
            None,
            (100.0, 200.0),
        );
        assert_eq!(preview.source_x_for_destination_fraction(0.5), None);
    }

    #[test]
    fn navigation_preview_texture_budget_is_hard_and_overflow_safe() {
        assert_eq!(navigation_preview_texture_bytes(0, 1_000), None);
        assert_eq!(
            navigation_preview_texture_bytes(1_920, 1_080),
            Some(8_294_400)
        );
        assert_eq!(
            navigation_preview_texture_bytes(8_192, 4_096),
            Some(MAX_NAVIGATION_PREVIEW_BYTES)
        );
        assert_eq!(navigation_preview_texture_bytes(8_193, 4_096), None);
        assert_eq!(navigation_preview_texture_bytes(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn navigation_preview_subrect_uvs_sample_only_capture_texel_centres() {
        let (origin, size) = navigation_preview_source_uv([100, 50], [200, 100], [1_000, 500]);
        assert!((origin[0] - 0.1005).abs() < f32::EPSILON);
        assert!((origin[1] - 0.101).abs() < f32::EPSILON);
        assert!((origin[0] + size[0] - 0.2995).abs() < f32::EPSILON);
        assert!((origin[1] + size[1] - 0.299).abs() < f32::EPSILON);

        let (single_origin, single_size) = navigation_preview_source_uv([7, 9], [1, 1], [20, 20]);
        assert_eq!(single_size, [0.0, 0.0]);
        assert_eq!(single_origin, [0.375, 0.475]);
    }

    #[test]
    fn primitive_conversion_preserves_geometry_and_pick_id() {
        let primitive = RectPrimitive {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            color: Rgba::new(0.1, 0.2, 0.3, 0.4),
            object_id: 99,
            shape: Default::default(),
        };
        let instance = RectangleInstance::from(primitive);
        assert!((instance.origin[0] - 1.0).abs() < f32::EPSILON);
        assert!((instance.origin[1] - 2.0).abs() < f32::EPSILON);
        assert!((instance.size[0] - 3.0).abs() < f32::EPSILON);
        assert!((instance.size[1] - 4.0).abs() < f32::EPSILON);
        assert_eq!(instance.object_id, 99);
        assert_eq!(instance.padding[0], 0);
    }

    #[test]
    fn arrow_primitive_conversion_preserves_pick_id_and_encodes_direction() {
        let primitive = RectPrimitive {
            x: 1.0,
            y: 2.0,
            width: 12.0,
            height: 6.0,
            color: Rgba::new(0.1, 0.2, 0.3, 0.4),
            object_id: 99,
            shape: RectShape::DirectionalTerminalRight,
        };
        let instance = RectangleInstance::from(primitive);
        assert_eq!(instance.object_id, 99);
        assert_eq!(instance.padding[0], 2);
    }

    #[test]
    fn instance_size_is_aligned_for_vertex_uploads() {
        assert_eq!(std::mem::size_of::<RectangleInstance>(), 48);
        let instances = [RectangleInstance::zeroed(); 3];
        assert_eq!(rectangle_instance_bytes(&instances), 144);
    }

    #[test]
    fn screen_uniform_is_visible_to_every_shader_stage_that_reads_it() {
        let visibility = screen_uniform_visibility();
        assert!(visibility.contains(wgpu::ShaderStages::VERTEX));
        assert!(visibility.contains(wgpu::ShaderStages::FRAGMENT));
        assert!(RECTANGLE_SHADER.contains("screen.size"));
        assert!(RECTANGLE_SHADER.contains("screen.selected_object_id"));
        assert!(RECTANGLE_SHADER.contains("screen.hovered_object_id"));
    }

    #[test]
    fn dirty_range_ignores_unchanged_prefix_and_suffix() {
        let instance = RectangleInstance::from(RectPrimitive {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            color: Rgba::new(0.1, 0.2, 0.3, 0.4),
            object_id: 1,
            shape: Default::default(),
        });
        let mut changed = instance;
        changed.object_id = 2;
        assert_eq!(
            changed_instance_range(
                &[instance, instance, instance],
                &[instance, changed, instance]
            ),
            Some(1..2)
        );
        assert_eq!(changed_instance_range(&[instance], &[instance]), None);
    }

    #[test]
    fn png_export_produces_a_decodable_image() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("view.png");
        let display = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 1.0,
                y: 1.0,
                width: 3.0,
                height: 2.0,
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                object_id: 0,
                shape: Default::default(),
            }],
            texts: Vec::new(),
        };
        export_png(&display, 8, 8, &path).unwrap();
        let decoded = image::open(path).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (8, 8));
        assert_eq!(decoded.get_pixel(2, 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn png_export_clips_directional_read_ends_without_moving_the_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("arrow.png");
        let display = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 1.0,
                y: 1.0,
                width: 12.0,
                height: 6.0,
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                object_id: 1,
                shape: RectShape::DirectionalTerminalRight,
            }],
            texts: Vec::new(),
        };
        export_png(&display, 16, 8, &path).unwrap();
        let decoded = image::open(path).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(11, 1).0, [255, 255, 255, 255]);
        assert_eq!(decoded.get_pixel(11, 3).0, [255, 0, 0, 255]);
        assert_eq!(decoded.get_pixel(2, 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn narrow_directional_reads_keep_a_rectangular_shoulder() {
        let rectangle = RectPrimitive {
            x: 1.0,
            y: 1.0,
            width: 12.0,
            height: 30.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 1,
            shape: RectShape::DirectionalTerminalRight,
        };
        assert!(rectangle_contains_pixel(rectangle, 2, 1));
        assert!(!rectangle_contains_pixel(rectangle, 11, 1));
        assert!(rectangle_contains_pixel(rectangle, 11, 15));
    }

    #[test]
    fn chevrons_are_triangular_without_inheriting_a_read_body() {
        let chevron = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: 6.0,
            height: 6.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 1,
            shape: RectShape::ChevronRight,
        };
        assert!(rectangle_contains_pixel(chevron, 0, 0));
        assert!(!rectangle_contains_pixel(chevron, 5, 0));
        assert!(rectangle_contains_pixel(chevron, 4, 2));
    }

    #[test]
    fn breakend_brackets_have_a_join_side_bar_and_neutral_stem() {
        let before = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 8.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 1,
            shape: RectShape::BreakendJoinBefore,
        };
        let after = RectPrimitive {
            shape: RectShape::BreakendJoinAfter,
            ..before
        };
        assert!(rectangle_contains_pixel(before, 0, 0));
        assert!(!rectangle_contains_pixel(before, 7, 0));
        assert!(rectangle_contains_pixel(before, 7, 3));
        assert!(!rectangle_contains_pixel(after, 0, 0));
        assert!(rectangle_contains_pixel(after, 7, 0));
        assert!(rectangle_contains_pixel(after, 0, 3));
    }

    #[test]
    fn junction_arcs_meet_the_baseline_and_curve_to_the_opposite_edge() {
        let up = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 0,
            shape: RectShape::JunctionArcUp,
        };
        let down = RectPrimitive {
            shape: RectShape::JunctionArcDown,
            ..up
        };
        assert!(rectangle_contains_pixel(up, 0, 8));
        assert!(rectangle_contains_pixel(up, 9, 0));
        assert!(!rectangle_contains_pixel(up, 9, 8));
        assert!(rectangle_contains_pixel(down, 0, 1));
        assert!(rectangle_contains_pixel(down, 9, 9));
        assert!(!rectangle_contains_pixel(down, 9, 1));
    }

    #[test]
    fn sashimi_hover_and_selection_expand_only_the_arc_stroke() {
        let base = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
            color: Rgba::new(0.84, 0.30, 0.38, 0.82),
            object_id: 17,
            shape: RectShape::JunctionArcUp,
        };
        let hover = RectPrimitive {
            shape: base.shape.as_junction_arc_hover(),
            ..base
        };
        let selected = RectPrimitive {
            shape: base.shape.as_junction_arc_selected(),
            ..base
        };

        assert!(!rectangle_contains_pixel(base, 9, 2));
        assert!(rectangle_contains_pixel(hover, 9, 2));
        assert!(rectangle_contains_pixel(selected, 9, 2));
        assert!(!rectangle_contains_pixel(hover, 9, 3));
        assert!(rectangle_contains_pixel(selected, 9, 3));

        let ordinary = RectPrimitive {
            shape: RectShape::Rectangle,
            ..base
        };
        assert!(rectangle_contains_pixel(ordinary, 9, 3));
        assert_eq!(ordinary.object_id, base.object_id);
        assert_eq!(ordinary.shape.junction_arc_interaction_expansion(), 0.0);
    }

    #[test]
    fn sashimi_interaction_shapes_preserve_curve_direction() {
        assert_eq!(
            RectShape::JunctionArcUp.as_junction_arc_hover(),
            RectShape::JunctionArcHoverUp
        );
        assert_eq!(
            RectShape::JunctionArcDown.as_junction_arc_hover(),
            RectShape::JunctionArcHoverDown
        );
        assert_eq!(
            RectShape::JunctionArcUp.as_junction_arc_selected(),
            RectShape::JunctionArcSelectedUp
        );
        assert_eq!(
            RectShape::JunctionArcDown.as_junction_arc_selected(),
            RectShape::JunctionArcSelectedDown
        );
    }

    #[test]
    fn synthetic_screenshot_matches_golden_pixel_hash() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.png");
        let second_path = directory.path().join("second.png");
        let display = DisplayList::synthetic_coverage_and_reads_at(320.0, 24, 1_000, 8.0);
        export_png(&display, 320, 240, &first_path).unwrap();
        export_png(&display, 320, 240, &second_path).unwrap();
        assert_eq!(
            std::fs::read(&first_path).unwrap(),
            std::fs::read(&second_path).unwrap(),
            "identical display lists must produce byte-identical PNG files"
        );

        let pixels = image::open(first_path).unwrap().to_rgba8().into_raw();
        let hash = pixels.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!(
            hash, 1_550_560_487_537_907_815,
            "update only after reviewing an intentional raster change"
        );
    }

    #[test]
    fn alignment_event_screenshot_matches_reviewed_pixel_hash() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 200).unwrap();
        let viewport = Viewport::new(interval, 400.0).unwrap();
        let record = AlignmentRecord {
            contig,
            start: 20,
            end: 150,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"events".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 30,
                },
                CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 20,
                },
                CigarOp {
                    kind: CigarOpKind::Deletion,
                    len: 30,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 20,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 30,
                },
            ],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        let query = AlignmentQuery {
            interval,
            header: std::sync::Arc::default(),
            batch,
            pileup: rgv_hts::PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats::default(),
        };
        let options = AlignmentLayoutOptions {
            reads_top: 20.0,
            color_mode: AlignmentColorMode::None,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alignment-events.png");
        export_png(&layout.display_list, 400, 80, &path).unwrap();
        if let Some(review_path) = std::env::var_os("RGV_VISUAL_REVIEW_PATH") {
            std::fs::copy(&path, review_path).unwrap();
        }
        let pixels = image::open(path).unwrap().to_rgba8().into_raw();
        let hash = pixels.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!(
            hash, 14_537_395_103_464_253_144,
            "update only after reviewing an intentional alignment-event raster change"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn ultima_flow_indel_screenshot_matches_reviewed_pixel_hash() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 20).unwrap();
        let viewport = Viewport::new(interval, 400.0).unwrap();
        let records = [
            AlignmentRecord {
                contig,
                start: 0,
                end: 6,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: b"ultima-insertion-q40".to_vec(),
                cigar: vec![
                    CigarOp {
                        kind: CigarOpKind::SequenceMatch,
                        len: 3,
                    },
                    CigarOp {
                        kind: CigarOpKind::Insertion,
                        len: 2,
                    },
                    CigarOp {
                        kind: CigarOpKind::SequenceMatch,
                        len: 3,
                    },
                ],
                sequence: SequenceData::Present {
                    bases: b"ACGTTTAC".to_vec(),
                    qualities: Some(vec![30, 30, 30, 30, 40, 30, 30, 30]),
                },
                auxiliary_fields: vec![AuxField {
                    tag: *b"tp",
                    value: AuxValue::SignedArray(vec![0, 0, 0, 0, -2, 0, 0, 0]),
                }],
            },
            AlignmentRecord {
                contig,
                start: 4,
                end: 12,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: b"ultima-deletion-q0".to_vec(),
                cigar: vec![
                    CigarOp {
                        kind: CigarOpKind::SequenceMatch,
                        len: 3,
                    },
                    CigarOp {
                        kind: CigarOpKind::Deletion,
                        len: 2,
                    },
                    CigarOp {
                        kind: CigarOpKind::SequenceMatch,
                        len: 3,
                    },
                ],
                sequence: SequenceData::Present {
                    bases: b"CGTAAA".to_vec(),
                    qualities: Some(vec![30, 30, 30, 30, 38, 30]),
                },
                auxiliary_fields: vec![AuxField {
                    tag: *b"tp",
                    value: AuxValue::SignedArray(vec![0, 0, 0, 0, 2, 0]),
                }],
            },
        ];
        let mut batch = AlignmentBatch::default();
        for record in &records {
            batch.push(record).unwrap();
        }
        let mut reference = b"ACGTACGTACGTACGTACGT".to_vec();
        reference[4..12].copy_from_slice(b"CGTAAAAA");
        let query = AlignmentQuery {
            interval,
            header: std::sync::Arc::default(),
            batch,
            pileup: rgv_hts::PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: Some(ReferenceSlice {
                interval,
                bases: reference,
            }),
            stats: QueryStats {
                records_seen: 2,
                records_eligible: 2,
                records_emitted: 2,
                ..QueryStats::default()
            },
        };
        let options = AlignmentLayoutOptions {
            reads_top: 10.0,
            row_height: 12.0,
            read_height: 9.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modified_bases: false,
            show_modification_frequency: false,
            color_mode: AlignmentColorMode::Ultima(UltimaDisplayOptions {
                maximum_homopolymer_length_override: Some(3),
                ..UltimaDisplayOptions::default()
            }),
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        assert_eq!(layout.stats.ultima_unavailable_records, 0);
        assert_eq!(layout.stats.ultima_unavailable_events, 0);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ultima-flow-indels.png");
        export_png(&layout.display_list, 400, 60, &path).unwrap();
        if let Some(review_path) = std::env::var_os("RGV_ULTIMA_VISUAL_REVIEW_PATH") {
            std::fs::copy(&path, review_path).unwrap();
        }
        let pixels = image::open(path).unwrap().to_rgba8().into_raw();
        let hash = pixels.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!(
            hash, 3_876_064_690_168_404_792,
            "update only after reviewing the Ultima insertion/deletion confidence raster"
        );
    }

    #[test]
    fn alignment_technology_matrix_matches_reviewed_pixel_hash() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 400).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let mut batch = AlignmentBatch::default();
        let records = [
            (10, 60, 0, None, vec![(CigarOpKind::Match, 50)]),
            (10, 360, 0, None, vec![(CigarOpKind::Match, 350)]),
            (
                20,
                180,
                0,
                None,
                vec![
                    (CigarOpKind::Match, 30),
                    (CigarOpKind::ReferenceSkip, 100),
                    (CigarOpKind::Match, 30),
                ],
            ),
            (
                50,
                150,
                0x800,
                None,
                vec![(CigarOpKind::SoftClip, 20), (CigarOpKind::Match, 100)],
            ),
            (100, 150, 0x1, Some(200), vec![(CigarOpKind::Match, 50)]),
        ];
        for (index, (start, end, flags, mate_start, cigar)) in records.into_iter().enumerate() {
            push_fixture_record(
                &mut batch,
                contig,
                index,
                (start, end, flags, mate_start, cigar),
            );
        }
        for index in 0..20 {
            batch
                .push(&AlignmentRecord {
                    contig,
                    start: 220,
                    end: 270,
                    flags: AlignmentFlags::from_bits(if index == 0 { 0x10 } else { 0 }),
                    mapping_quality: Some(45),
                    mate_contig: None,
                    mate_start: None,
                    template_length: 0,
                    read_name: format!("deep-{index}").into_bytes(),
                    cigar: vec![CigarOp {
                        kind: CigarOpKind::Match,
                        len: 50,
                    }],
                    sequence: SequenceData::NotLoaded,
                    auxiliary_fields: Vec::new(),
                })
                .unwrap();
        }
        let query = AlignmentQuery {
            interval,
            header: std::sync::Arc::default(),
            batch,
            pileup: rgv_hts::PileupSummary::from_depths((0..400).map(|position| {
                if (220..270).contains(&position) {
                    20
                } else {
                    1
                }
            })),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 25,
                records_eligible: 25,
                records_emitted: 25,
                ..QueryStats::default()
            },
        };
        let options = AlignmentLayoutOptions {
            coverage_top: 8.0,
            coverage_height: 48.0,
            reads_top: 68.0,
            color_mode: AlignmentColorMode::None,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alignment-matrix.png");
        export_png(&layout.display_list, 800, 240, &path).unwrap();
        if let Some(review_path) = std::env::var_os("RGV_MATRIX_VISUAL_REVIEW_PATH") {
            std::fs::copy(&path, review_path).unwrap();
        }
        let pixels = image::open(path).unwrap().to_rgba8().into_raw();
        let hash = pixels.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!(
            hash, 2_874_170_321_630_723_772,
            "update only after reviewing the short-, long-, spliced-, supplementary-, paired-, and deep-read fixture"
        );
    }

    type FixtureRecord = (u64, u64, u16, Option<u64>, Vec<(CigarOpKind, u32)>);

    fn push_fixture_record(
        batch: &mut AlignmentBatch,
        contig: ContigId,
        index: usize,
        fixture: FixtureRecord,
    ) {
        let (start, end, flags, mate_start, cigar) = fixture;
        batch
            .push(&AlignmentRecord {
                contig,
                start,
                end,
                flags: AlignmentFlags::from_bits(flags),
                mapping_quality: Some(60),
                mate_contig: mate_start.map(|_| contig),
                mate_start,
                template_length: 0,
                read_name: format!("fixture-{index}").into_bytes(),
                cigar: cigar
                    .into_iter()
                    .map(|(kind, len)| CigarOp { kind, len })
                    .collect(),
                sequence: SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .unwrap();
    }
}
