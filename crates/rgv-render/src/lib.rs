//! GPU rendering infrastructure for genomic display lists.

mod alignment_scene;
mod alignment_summary;
mod context_scene;

/// Constant-size policy view accepted by replacement planners. Desktop code supplies
/// `LedgerProgress`; explicit inventory tests may supply `LedgerSnapshot`. Planning never needs
/// to enumerate allocation records merely to capture the budget-policy revision.
pub trait AlignmentLedgerPolicyView {
    fn policy_revision(&self) -> u64;
}

impl AlignmentLedgerPolicyView for rgv_alignment_scene::LedgerProgress {
    fn policy_revision(&self) -> u64 {
        self.policy_revision
    }
}

impl AlignmentLedgerPolicyView for rgv_alignment_scene::LedgerSnapshot {
    fn policy_revision(&self) -> u64 {
        self.policy_revision
    }
}

pub use alignment_scene::{
    AlignmentGpuBatchAdmission, AlignmentGpuCameraCaptureError, AlignmentGpuContentAuthority,
    AlignmentGpuContentReplacementReason, AlignmentGpuFrame, AlignmentGpuFrameBuildError,
    AlignmentGpuInstance, AlignmentGpuPageAdmission, AlignmentGpuPageAdmissionError,
    AlignmentGpuPrefetchCursor, AlignmentGpuPrefetchCursorError, AlignmentGpuPrefetchProgress,
    AlignmentGpuPrepareError, AlignmentGpuPrepareStats, AlignmentGpuProjectedBounds,
    AlignmentGpuPublishableReplacement, AlignmentGpuReadyReplacement, AlignmentGpuRenderError,
    AlignmentGpuRenderer, AlignmentGpuRendererOperationCounters, AlignmentGpuReplacementError,
    AlignmentGpuReplacementPlan, AlignmentGpuResidencyError, AlignmentGpuResidencyLease,
    AlignmentGpuRetirement, AlignmentGpuSceneRequest, AlignmentGpuScissor, AlignmentGpuShape,
    AlignmentGpuStagedReplacement, PackedAlignmentPage, project_alignment_gpu_instance,
};
pub use alignment_summary::{
    AlignmentSummaryAlleleToken, AlignmentSummaryDepthRangeIndex, AlignmentSummaryFrameOverlays,
    AlignmentSummaryGpuAlleleInstance, AlignmentSummaryGpuBatchAdmission,
    AlignmentSummaryGpuCameraCaptureError, AlignmentSummaryGpuContentAuthority,
    AlignmentSummaryGpuContentReplacementReason, AlignmentSummaryGpuDepthInstance,
    AlignmentSummaryGpuFrame, AlignmentSummaryGpuFrameBuildError, AlignmentSummaryGpuJunctionHit,
    AlignmentSummaryGpuJunctionInstance, AlignmentSummaryGpuJunctionLabel,
    AlignmentSummaryGpuLaneOptions, AlignmentSummaryGpuModificationInstance,
    AlignmentSummaryGpuPageAdmission, AlignmentSummaryGpuPageAdmissionError,
    AlignmentSummaryGpuPageError, AlignmentSummaryGpuPageKey, AlignmentSummaryGpuPageRequest,
    AlignmentSummaryGpuPrefetchCursor, AlignmentSummaryGpuPrefetchCursorError,
    AlignmentSummaryGpuPrefetchProgress, AlignmentSummaryGpuPrepareError,
    AlignmentSummaryGpuPrepareStats, AlignmentSummaryGpuProjectedAllele,
    AlignmentSummaryGpuProjectedBisulfite, AlignmentSummaryGpuProjectedBounds,
    AlignmentSummaryGpuProjectedJunction, AlignmentSummaryGpuProjectedModification,
    AlignmentSummaryGpuPublishableReplacement, AlignmentSummaryGpuReadyReplacement,
    AlignmentSummaryGpuRenderError, AlignmentSummaryGpuRenderer,
    AlignmentSummaryGpuRendererOperationCounters, AlignmentSummaryGpuReplacementError,
    AlignmentSummaryGpuReplacementPlan, AlignmentSummaryGpuResidencyError,
    AlignmentSummaryGpuResidencyLease, AlignmentSummaryGpuRetirement,
    AlignmentSummaryGpuSceneRequest, AlignmentSummaryGpuStagedReplacement,
    AlignmentSummaryJunctionComponentToken, AlignmentSummaryJunctionEntry,
    AlignmentSummaryJunctionInteraction, AlignmentSummaryJunctionPageMetadata,
    AlignmentSummaryModificationPageMetadata, PackedAlignmentSummaryPage,
    project_alignment_summary_allele_instance, project_alignment_summary_bisulfite_instance,
    project_alignment_summary_depth_instance, project_alignment_summary_junction_instance,
    project_alignment_summary_modification_instance,
};
pub use context_scene::{
    ContextSceneGpuCamera, ContextSceneGpuCameraCaptureError, ContextSceneGpuCoherenceKey,
    ContextSceneGpuFrame, ContextSceneGpuLaneKey, ContextSceneGpuLaneRequest,
    ContextSceneGpuOperationCounters, ContextSceneGpuPrepareReport, ContextSceneGpuPrepareStats,
    ContextSceneGpuRejection, ContextSceneGpuRejectionReason, ContextSceneGpuRenderer,
    ContextSceneGpuRetainedLaneRequest, ContextSceneGpuScissor, project_context_scene_rectangle,
};

/// Renderer-local progress from one cooperative shared-ledger reclaim pass. Retained byte
/// counts include only allocations owned by the renderer receiving the request; ledger charged
/// counts intentionally cover every owner sharing that ledger.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[must_use]
pub struct GpuReclaimReport {
    pub pressure: GpuBudgetPressure,
    pub baseline_occupied_bytes: usize,
    pub pages_before: usize,
    pub pages_after: usize,
    pub retained_bytes_before: usize,
    pub retained_bytes_after: usize,
    pub ledger_charged_before: usize,
    pub ledger_charged_after: usize,
    /// True when the shared ledger has released the failure-time deficit measured by the common
    /// reclaim goal after this renderer has exhausted its safe candidates.
    pub headroom_satisfied: bool,
}

/// Exact persistent retained-budget pressure reported by a failed GPU candidate. Failure-time
/// charged and reserved totals are retained because earlier reservations from the same atomic
/// candidate unwind before a coordinator can run. Re-evaluating only the post-unwind ledger can
/// otherwise repeat an aggregate reservation failure without reclaiming anything.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GpuBudgetPressureCause {
    /// Ordinary retained allocation failed against the configured steady budget.
    #[default]
    RetainedBudget,
    /// Candidate steady state exceeded the budget after exact retirement credit.
    SteadyReplacement,
    /// Old/new physical overlap exceeded the explicit transition allowance.
    TransitionOverlap,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GpuBudgetPressure {
    pub cause: GpuBudgetPressureCause,
    pub requested_bytes: usize,
    /// Effective occupied bytes relevant to this cause. For a steady replacement this has already
    /// subtracted the exact retirement credit, while `retirement_credit_bytes` preserves the
    /// original accounting fact for diagnostics.
    pub charged_bytes_at_failure: usize,
    pub reserved_bytes_at_failure: usize,
    pub retirement_credit_bytes: usize,
    pub limit_bytes: usize,
}

impl GpuBudgetPressure {
    #[must_use]
    pub const fn from_budget_failure(
        requested_bytes: usize,
        charged_bytes_at_failure: usize,
        reserved_bytes_at_failure: usize,
        limit_bytes: usize,
    ) -> Self {
        Self {
            cause: GpuBudgetPressureCause::RetainedBudget,
            requested_bytes,
            charged_bytes_at_failure,
            reserved_bytes_at_failure,
            retirement_credit_bytes: 0,
            limit_bytes,
        }
    }

    #[must_use]
    pub const fn from_steady_replacement_failure(
        requested_bytes: usize,
        occupied_bytes_at_failure: usize,
        retirement_credit_bytes: usize,
        limit_bytes: usize,
    ) -> Self {
        Self {
            cause: GpuBudgetPressureCause::SteadyReplacement,
            requested_bytes,
            charged_bytes_at_failure: occupied_bytes_at_failure
                .saturating_sub(retirement_credit_bytes),
            reserved_bytes_at_failure: 0,
            retirement_credit_bytes,
            limit_bytes,
        }
    }

    #[must_use]
    pub const fn from_transition_overlap_failure(
        requested_bytes: usize,
        occupied_bytes_at_failure: usize,
        limit_bytes: usize,
    ) -> Self {
        Self {
            cause: GpuBudgetPressureCause::TransitionOverlap,
            requested_bytes,
            charged_bytes_at_failure: occupied_bytes_at_failure,
            reserved_bytes_at_failure: 0,
            retirement_credit_bytes: 0,
            limit_bytes,
        }
    }

    /// Durable retained capacity that must disappear before the exact candidate is retried.
    /// Candidate-local reservations are deliberately included: they will be recreated by the
    /// retry and cannot count as reclaimed merely because they unwind with the error.
    #[must_use]
    pub const fn required_reclaim_bytes(self) -> usize {
        self.charged_bytes_at_failure
            .saturating_add(self.reserved_bytes_at_failure)
            .saturating_add(self.requested_bytes)
            .saturating_sub(self.limit_bytes)
    }
}

/// One composable reclaim target shared by every GPU cache owner participating in a retry. The
/// baseline is captured once after the failed candidate has unwound. Sequential renderer passes
/// use this same value, so the second pass releases only the remainder left by the first.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GpuReclaimGoal {
    pub pressure: GpuBudgetPressure,
    pub baseline_occupied_bytes: usize,
    /// Hysteresis beyond the exact fit point. This is derived from measured foreground demand by
    /// the coordinator and never changes whether the underlying candidate is intrinsically
    /// feasible.
    pub additional_headroom_bytes: usize,
}

impl GpuReclaimGoal {
    #[must_use]
    pub const fn new(pressure: GpuBudgetPressure, baseline_occupied_bytes: usize) -> Self {
        Self {
            pressure,
            baseline_occupied_bytes,
            additional_headroom_bytes: 0,
        }
    }

    #[must_use]
    pub const fn with_additional_headroom(mut self, bytes: usize) -> Self {
        self.additional_headroom_bytes = bytes;
        self
    }

    /// Adds hysteresis without demanding eviction below an exact committed/pinned occupancy
    /// floor. Low-water headroom is desirable, but it cannot turn an otherwise admissible
    /// foreground candidate into a terminal failure merely because the full reserve is
    /// unavailable.
    #[must_use]
    pub const fn with_additional_headroom_above_floor(
        mut self,
        bytes: usize,
        minimum_occupied_bytes: usize,
    ) -> Self {
        let maximum = self
            .baseline_occupied_bytes
            .saturating_sub(minimum_occupied_bytes)
            .saturating_sub(self.pressure.required_reclaim_bytes());
        self.additional_headroom_bytes = if bytes < maximum { bytes } else { maximum };
        self
    }

    #[must_use]
    pub const fn required_reclaim_bytes(self) -> usize {
        self.pressure
            .required_reclaim_bytes()
            .saturating_add(self.additional_headroom_bytes)
    }

    #[must_use]
    pub const fn released_bytes_at(self, occupied_bytes: usize) -> usize {
        self.baseline_occupied_bytes.saturating_sub(occupied_bytes)
    }

    #[must_use]
    pub const fn is_satisfied_at(self, occupied_bytes: usize) -> bool {
        self.released_bytes_at(occupied_bytes) >= self.required_reclaim_bytes()
    }

    /// Whether releasing every byte present at the post-unwind baseline could satisfy this goal.
    /// An intrinsically oversized candidate must fail without purging unrelated caches.
    #[must_use]
    pub const fn is_feasible(self) -> bool {
        self.pressure.requested_bytes <= self.pressure.limit_bytes
            && self.pressure.required_reclaim_bytes() <= self.baseline_occupied_bytes
    }
}

impl GpuReclaimReport {
    #[must_use]
    pub const fn reclaimed_pages(&self) -> usize {
        self.pages_before.saturating_sub(self.pages_after)
    }

    #[must_use]
    pub const fn released_retained_bytes(&self) -> usize {
        self.retained_bytes_before
            .saturating_sub(self.retained_bytes_after)
    }

    #[must_use]
    pub const fn released_ledger_bytes(&self) -> usize {
        self.ledger_charged_before
            .saturating_sub(self.ledger_charged_after)
    }
}

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
const MAX_UPLOAD_BYTES_PER_FRAME: usize = 16 * 1024 * 1024;

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
    operation_counters: RectangleRendererOperationCounters,
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

/// Monotonic, constant-size counters for proving that camera-only genomic movement did not
/// re-enter generic screen-space composition or upload. These counters intentionally avoid any
/// retained-slab inventory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RectangleRendererOperationCounters {
    pub preparations: u64,
    pub instances_examined: u64,
    pub slab_cache_hits: u64,
    pub uploaded_bytes: u64,
    pub buffer_reallocations: u64,
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
            operation_counters: RectangleRendererOperationCounters::default(),
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
            let stats = PrepareStats {
                total_instances: self.instance_count as usize,
                slab_cache_hit: true,
                slab_entries: self.cached_slabs.len(),
                slab_resident_bytes: self.slab_resident_bytes(),
                ..PrepareStats::default()
            };
            self.record_prepare_stats(stats);
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
            self.record_prepare_stats(stats);
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
        self.record_prepare_stats(stats);
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

    #[must_use]
    pub const fn operation_counters(&self) -> RectangleRendererOperationCounters {
        self.operation_counters
    }

    /// Retained capacity of the generic instance buffer, excluding optional keyed slabs.
    #[must_use]
    pub fn primary_resident_bytes(&self) -> usize {
        self.instance_capacity
            .saturating_mul(std::mem::size_of::<RectangleInstance>())
    }

    /// Application-owned generic GPU capacity included in the desktop aggregate cache scope.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.primary_resident_bytes()
            .saturating_add(self.slab_resident_bytes())
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

    fn record_prepare_stats(&mut self, stats: PrepareStats) {
        self.operation_counters.preparations =
            self.operation_counters.preparations.saturating_add(1);
        self.operation_counters.instances_examined = self
            .operation_counters
            .instances_examined
            .saturating_add(stats.total_instances as u64);
        self.operation_counters.slab_cache_hits = self
            .operation_counters
            .slab_cache_hits
            .saturating_add(u64::from(stats.slab_cache_hit));
        self.operation_counters.uploaded_bytes = self
            .operation_counters
            .uploaded_bytes
            .saturating_add(stats.uploaded_bytes as u64);
        self.operation_counters.buffer_reallocations = self
            .operation_counters
            .buffer_reallocations
            .saturating_add(u64::from(stats.buffer_reallocated));
        self.last_prepare_stats = stats;
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
    use rgv_layout::{RectPrimitive, Rgba};

    use super::*;

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
}
