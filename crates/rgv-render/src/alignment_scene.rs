use std::{
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};
use rgv_alignment_scene::{
    AlignmentBodyColorToken, AlignmentCamera, AlignmentComponent, AlignmentGeometryToken,
    AlignmentInteractionState, AlignmentPageKey, AlignmentPanelLaneFrame,
    AlignmentPresentationScene, AlignmentSemanticPage, AlignmentSemanticPrimitive,
    AlignmentStyleToken, AllocationClass, AllocationLease, AllocationLedger,
    CpuAlignmentLabelFrame, CpuAlignmentProjector, CpuProjectionError, LedgerError,
    PinnedAllocationLease, ReservationKind, SceneRevision, TransientResourceClass,
};
use thiserror::Error;

const SHADER: &str = include_str!("alignment_scene.wgsl");

/// Persistent GPU representation of one non-text semantic primitive. Coordinates remain a
/// double-single page-local value; camera and page origins are supplied independently.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentGpuInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    pub row_offset: u32,
    pub shape: u32,
    pub color: [f32; 4],
}

impl AlignmentGpuInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32,
            3 => Uint32,
            4 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AlignmentGpuShape {
    Rectangle = 0,
    EventLine = 1,
    PointMarker = 2,
    DirectionTerminalRight = 3,
    DirectionTerminalLeft = 4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackedAlignmentPage {
    pub instances: Vec<AlignmentGpuInstance>,
    pub semantic_indices: Vec<u32>,
    pub text_primitive_count: usize,
}

/// Headless projection of one packed GPU instance, used by CPU/GPU equivalence fixtures without
/// requiring framebuffer readback. The live shader applies the same shape and clipping rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentGpuProjectedBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[must_use]
pub fn project_alignment_gpu_instance(
    instance: AlignmentGpuInstance,
    page_start: u64,
    page_row_start: u32,
    camera: AlignmentCamera,
) -> Option<AlignmentGpuProjectedBounds> {
    let start_offset = f64::from(instance.start_offset[0]) + f64::from(instance.start_offset[1]);
    let end_offset = f64::from(instance.end_offset[0]) + f64::from(instance.end_offset[1]);
    let mut x = camera.clip().x
        + (page_start as f64 + start_offset - camera.exact_start()) / camera.bases_per_pixel();
    let mut right = camera.clip().x
        + (page_start as f64 + end_offset - camera.exact_start()) / camera.bases_per_pixel();
    let row = f64::from(page_row_start.saturating_add(instance.row_offset));
    let row_top = camera.clip().y
        + (row - camera.row_scroll()) * camera.row_height()
        + (camera.row_height() - camera.read_height()) * 0.5;
    let (mut y, mut height) = (row_top, camera.read_height());
    if instance.shape == AlignmentGpuShape::EventLine as u32 {
        y = row_top + camera.read_height() * 0.45;
        height = (camera.read_height() * 0.10).max(1.0);
    } else if instance.shape == AlignmentGpuShape::PointMarker as u32 {
        let original_width = right - x;
        let marker_width = original_width.max(1.5);
        x -= (marker_width - original_width) * 0.5;
        right = x + marker_width;
    } else if instance.shape == AlignmentGpuShape::DirectionTerminalRight as u32
        || instance.shape == AlignmentGpuShape::DirectionTerminalLeft as u32
    {
        right = right.max(x + 1.0);
    }
    let clip = camera.clip();
    let clipped_right = right.min(clip.x + clip.width);
    let clipped_bottom = (y + height).min(clip.y + clip.height);
    x = x.max(clip.x);
    y = y.max(clip.y);
    let width = clipped_right - x;
    height = clipped_bottom - y;
    (width > 0.0 && height > 0.0).then_some(AlignmentGpuProjectedBounds {
        x,
        y,
        width,
        height,
    })
}

impl PackedAlignmentPage {
    pub fn from_semantic(
        page: &AlignmentSemanticPage,
    ) -> Result<Self, AlignmentGpuPageAdmissionError> {
        pack_page(page, None)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct AlignmentGpuCameraUniform {
    screen_size: [f32; 2],
    clip_origin: [f32; 2],
    points_per_base: [f32; 2],
    page_start_from_camera: [f32; 2],
    row_scroll: f32,
    row_height: f32,
    read_height: f32,
    page_row_start: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentGpuScissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct AlignmentGpuSceneRequest<'a> {
    pub scene: &'a Arc<AlignmentPresentationScene>,
    pub camera_sequence: u64,
    pub camera: AlignmentCamera,
    pub screen_size_points: [f32; 2],
    pub scissor: AlignmentGpuScissor,
    pub interaction: &'a AlignmentInteractionState,
}

impl<'a> AlignmentGpuSceneRequest<'a> {
    /// Captures rendering from the same immutable panel-lane frame used by picking. The live
    /// camera comes from the UI capture; the worker publication contributes only semantic pages.
    #[must_use]
    pub fn from_panel_lane(
        lane: &'a AlignmentPanelLaneFrame,
        camera_sequence: u64,
        screen_size_points: [f32; 2],
        scissor: AlignmentGpuScissor,
        interaction: &'a AlignmentInteractionState,
    ) -> Option<Self> {
        Some(Self {
            scene: lane.snapshot.as_ref()?.reads(),
            camera_sequence,
            camera: lane.camera,
            screen_size_points,
            scissor,
            interaction,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentGpuPrepareStats {
    pub scenes: usize,
    pub mandatory_pages: usize,
    pub base_instances: usize,
    pub overlay_instances: usize,
    pub text_labels: usize,
    pub camera_uniform_bytes: usize,
    pub geometry_uploaded_bytes: usize,
    pub page_cache_hits: usize,
    pub page_cache_misses: usize,
    pub resident_pages: usize,
    pub resident_gpu_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentGpuPageAdmission {
    Admitted {
        instances: usize,
        uploaded_bytes: usize,
    },
    AlreadyResident,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentGpuBatchAdmission {
    pub requested_pages: usize,
    pub admitted_pages: usize,
    pub already_resident_pages: usize,
    pub instances: usize,
    pub uploaded_bytes: usize,
}

/// Progress from one bounded speculative admission step. Mandatory visible-page admission remains
/// a separate atomic operation; this type reports only prepared pages admitted outside that frame
/// boundary. Repeated calls deterministically advance through missing page keys without uploading
/// an already-resident page again.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentGpuPrefetchProgress {
    pub prepared_pages: usize,
    pub missing_pages_before: usize,
    pub admission: AlignmentGpuBatchAdmission,
    pub remaining_missing_pages: usize,
    pub missing_pages_remain: bool,
}

impl AlignmentGpuPrefetchProgress {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        !self.missing_pages_remain
    }
}

/// Pins a complete set of prepared semantic-page GPU allocations. The token is created only after
/// every unique page in the requested scenes is resident; dropping it makes those allocations
/// eligible for the renderer's normal LRU eviction again.
#[derive(Debug)]
#[must_use = "dropping the residency lease immediately makes its pages evictable"]
pub struct AlignmentGpuResidencyLease {
    device_generation: u64,
    page_keys: Arc<[AlignmentPageKey]>,
    _pins: Vec<PinnedAllocationLease>,
}

impl AlignmentGpuResidencyLease {
    #[must_use]
    pub const fn device_generation(&self) -> u64 {
        self.device_generation
    }

    #[must_use]
    pub fn page_keys(&self) -> &[AlignmentPageKey] {
        &self.page_keys
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.page_keys.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.page_keys.is_empty()
    }
}

struct PendingGpuPageAdmission {
    key: AlignmentPageKey,
    instance_count: u32,
    buffer_capacity: usize,
    packed: PackedAlignmentPage,
}

#[derive(Debug)]
struct GpuAlignmentPage {
    key: AlignmentPageKey,
    buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    allocation: AllocationLease,
    last_used_revision: u64,
}

#[derive(Clone, Debug)]
struct PreparedAlignmentDraw {
    page: AlignmentPageKey,
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
    overlay_range: Range<u32>,
}

#[derive(Clone, Debug)]
struct PendingAlignmentDraw {
    page: AlignmentPageKey,
    camera: AlignmentCamera,
    screen_size: [f32; 2],
    scissor: AlignmentGpuScissor,
    page_start: u64,
    page_row_start: u32,
    overlay_range: Range<u32>,
    base_instance_count: u32,
}

/// Immutable lease for one fully prepared multi-scene GPU submission. All mandatory page
/// allocations and renderer-owned frame buffers remain pinned through command encoding.
#[derive(Debug)]
pub struct AlignmentGpuFrame {
    revision: u64,
    device_generation: u64,
    pub scene_revisions: Arc<[SceneRevision]>,
    pub camera_sequences: Arc<[u64]>,
    label_frames: Arc<[CpuAlignmentLabelFrame]>,
    draws: Arc<[PreparedAlignmentDraw]>,
    buffers: Arc<GpuAlignmentFrameBuffers>,
    _allocation: AllocationLease,
    _pins: Vec<PinnedAllocationLease>,
}

impl AlignmentGpuFrame {
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }

    /// Camera-dependent text accepted by the bounded label projector, in request order. Geometry
    /// remains in persistent GPU pages and is not duplicated here.
    #[must_use]
    pub fn label_frames(&self) -> &[CpuAlignmentLabelFrame] {
        &self.label_frames
    }
}

/// Reusable immutable-for-the-duration-of-a-frame camera/overlay slot. The renderer keeps one
/// cache lease, while every prepared frame keeps an `Arc` and pins the charged GPU capacity. A
/// slot is writable only when the renderer is its sole owner, so preparing a replacement frame
/// can never overwrite buffers still referenced by an in-flight submission.
#[derive(Debug)]
struct GpuAlignmentFrameBuffers {
    camera_buffer: Option<wgpu::Buffer>,
    camera_bind_group: Option<wgpu::BindGroup>,
    camera_allocation: Option<AllocationLease>,
    camera_capacity: usize,
    overlay_buffer: Option<wgpu::Buffer>,
    overlay_allocation: Option<AllocationLease>,
    overlay_capacity: usize,
}

/// Dedicated persistent renderer for immutable alignment semantic pages. Base geometry is
/// uploaded once per page key. Pan/scroll/compatible zoom update only camera uniforms and a small
/// stable-handle overlay buffer.
pub struct AlignmentGpuRenderer {
    pipeline: wgpu::RenderPipeline,
    camera_layout: wgpu::BindGroupLayout,
    camera_stride: usize,
    frame_buffers: Vec<Arc<GpuAlignmentFrameBuffers>>,
    pages: Vec<GpuAlignmentPage>,
    page_table_allocation: Option<AllocationLease>,
    page_table_capacity: usize,
    revision: u64,
    device_generation: u64,
    pending_geometry_uploaded_bytes: usize,
}

impl AlignmentGpuRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RGV alignment semantic page shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let camera_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("RGV alignment semantic camera layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(
                            size_of::<AlignmentGpuCameraUniform>() as u64
                        ),
                    },
                    count: None,
                }],
            });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV alignment semantic pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV alignment semantic pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[AlignmentGpuInstance::vertex_buffer_layout()],
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
        Self {
            pipeline,
            camera_layout,
            camera_stride: aligned_uniform_stride(device),
            frame_buffers: Vec::new(),
            pages: Vec::new(),
            page_table_allocation: None,
            page_table_capacity: 0,
            revision: 0,
            device_generation: 1,
            pending_geometry_uploaded_bytes: 0,
        }
    }

    pub fn admit_page(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &Arc<AlignmentSemanticPage>,
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuPageAdmission, AlignmentGpuPageAdmissionError> {
        let result = self.admit_pages(
            device,
            queue,
            std::slice::from_ref(page),
            ledger,
            publication_revision,
        )?;
        if result.admitted_pages == 0 {
            Ok(AlignmentGpuPageAdmission::AlreadyResident)
        } else {
            Ok(AlignmentGpuPageAdmission::Admitted {
                instances: result.instances,
                uploaded_bytes: result.uploaded_bytes,
            })
        }
    }

    /// Transactionally admits a complete mandatory semantic-page set. Every key, device limit,
    /// retained reservation, and upload reservation is checked before the renderer page table is
    /// changed. Already-resident members are protected from admission-time eviction, so success
    /// means the whole requested set is resident together; failure cannot leave a partial new
    /// mandatory set published.
    pub fn admit_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[Arc<AlignmentSemanticPage>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuBatchAdmission, AlignmentGpuPageAdmissionError> {
        self.admit_pages_protecting(device, queue, pages, pages, ledger, publication_revision)
    }

    fn admit_pages_protecting(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[Arc<AlignmentSemanticPage>],
        protected: &[Arc<AlignmentSemanticPage>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuBatchAdmission, AlignmentGpuPageAdmissionError> {
        let mut missing_pages = 0_usize;
        let mut resident_pages = 0_usize;
        let mut maximum_instances = 0_usize;
        let mut retained_bytes = 0_usize;
        for (index, page) in pages.iter().enumerate() {
            if pages[..index]
                .iter()
                .any(|candidate| candidate.key() == page.key())
            {
                return Err(AlignmentGpuPageAdmissionError::DuplicatePageKey(page.key()));
            }
            if self.page_index(page.key()).is_some() {
                resident_pages = resident_pages.saturating_add(1);
                continue;
            }
            missing_pages = missing_pages.saturating_add(1);
            let instances = non_text_primitive_count(page);
            u32::try_from(instances)
                .map_err(|_| AlignmentGpuPageAdmissionError::TooManyPrimitives(instances))?;
            let buffer_capacity = gpu_buffer_capacity::<AlignmentGpuInstance>(instances)?;
            validate_device_buffer_size(device, buffer_capacity)?;
            maximum_instances = maximum_instances.saturating_add(instances);
            retained_bytes = retained_bytes.saturating_add(buffer_capacity);
        }

        if missing_pages == 0 {
            self.revision = self.revision.saturating_add(1);
            for page in pages {
                if let Some(index) = self.page_index(page.key()) {
                    self.pages[index].last_used_revision = self.revision;
                }
            }
            return Ok(AlignmentGpuBatchAdmission {
                requested_pages: pages.len(),
                admitted_pages: 0,
                already_resident_pages: resident_pages,
                instances: 0,
                uploaded_bytes: 0,
            });
        }

        self.evict_unpinned_pages_for_excluding(ledger, retained_bytes, protected)?;
        self.ensure_page_table_capacity(ledger, self.pages.len().saturating_add(missing_pages))?;
        let mut retained = ledger.reserve(
            AllocationClass::GpuPage,
            retained_bytes,
            ReservationKind::Normal,
        )?;
        let scratch_bytes = maximum_instances
            .saturating_mul(size_of::<AlignmentGpuInstance>())
            .saturating_mul(2)
            .saturating_add(missing_pages.saturating_mul(size_of::<PendingGpuPageAdmission>()));
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            scratch_bytes,
            ReservationKind::Normal,
        )?;
        let mut pending = Vec::with_capacity(missing_pages);
        for page in pages {
            if self.page_index(page.key()).is_some() {
                continue;
            }
            let instance_count = non_text_primitive_count(page);
            let packed = pack_page(page, None)?;
            debug_assert_eq!(packed.instances.len(), instance_count);
            pending.push(PendingGpuPageAdmission {
                key: page.key(),
                instance_count: u32::try_from(instance_count)
                    .expect("instance address space was validated before allocation"),
                buffer_capacity: gpu_buffer_capacity::<AlignmentGpuInstance>(instance_count)?,
                packed,
            });
        }

        self.revision = self.revision.saturating_add(1);
        let revision = self.revision;
        let mut admitted = Vec::with_capacity(missing_pages);
        let mut uploaded_bytes = 0_usize;
        for candidate in pending {
            let buffer = (candidate.buffer_capacity > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV immutable alignment GPU page"),
                    size: candidate.buffer_capacity as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            if let Some(buffer) = &buffer
                && !candidate.packed.instances.is_empty()
            {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&candidate.packed.instances));
            }
            let allocation = retained.register(candidate.buffer_capacity, publication_revision)?;
            uploaded_bytes = uploaded_bytes.saturating_add(
                candidate
                    .packed
                    .instances
                    .len()
                    .saturating_mul(size_of::<AlignmentGpuInstance>()),
            );
            admitted.push(GpuAlignmentPage {
                key: candidate.key,
                buffer,
                instance_count: candidate.instance_count,
                allocation,
                last_used_revision: revision,
            });
        }
        for page in pages {
            if let Some(index) = self.page_index(page.key()) {
                self.pages[index].last_used_revision = revision;
            }
        }
        self.pages.extend(admitted);
        self.pages.sort_unstable_by_key(|page| page.key);
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_add(uploaded_bytes);
        Ok(AlignmentGpuBatchAdmission {
            requested_pages: pages.len(),
            admitted_pages: missing_pages,
            already_resident_pages: resident_pages,
            instances: maximum_instances,
            uploaded_bytes,
        })
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
    ) -> Result<(AlignmentGpuFrame, AlignmentGpuPrepareStats), AlignmentGpuPrepareError> {
        // First pass is allocation-free. It proves the complete mandatory set is resident and
        // derives conservative capacities before any frame candidate memory or GPU buffer grows.
        let mut mandatory_pages = 0_usize;
        let mut maximum_overlays = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        for request in requests {
            validate_request(request)?;
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request
                .scene
                .pages()
                .iter()
                .filter(|page| page.interval().overlaps(interval))
                .filter(|page| ranges_overlap(page.row_range(), rows.clone()))
            {
                mandatory_pages = mandatory_pages.saturating_add(1);
                if self.page_index(page.key()).is_none() {
                    page_cache_misses = page_cache_misses.saturating_add(1);
                    continue;
                }
                page_cache_hits = page_cache_hits.saturating_add(1);
                if !request.interaction.is_empty() {
                    maximum_overlays = maximum_overlays.saturating_add(
                        page.primitives()
                            .iter()
                            .filter(|primitive| {
                                let token = request.interaction.token(primitive.handle);
                                !matches!(primitive.component, AlignmentComponent::Label { .. })
                                    && (token.hovered || token.selected || token.selected_chain)
                            })
                            .count(),
                    );
                }
            }
        }
        if page_cache_misses > 0 {
            return Err(AlignmentGpuPrepareError::MissingMandatoryPages {
                missing: page_cache_misses,
            });
        }
        u32::try_from(maximum_overlays)
            .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
        let uniform_bytes = mandatory_pages.saturating_mul(self.camera_stride);
        if mandatory_pages > 0 {
            u32::try_from((mandatory_pages - 1).saturating_mul(self.camera_stride))
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        let overlay_bytes = maximum_overlays.saturating_mul(size_of::<AlignmentGpuInstance>());
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            uniform_bytes
                .saturating_add(overlay_bytes.saturating_mul(2))
                .saturating_add(mandatory_pages.saturating_mul(size_of::<PreparedAlignmentDraw>()))
                .saturating_add(mandatory_pages.saturating_mul(size_of::<PendingAlignmentDraw>()))
                .saturating_add(
                    requests.len().saturating_mul(
                        size_of::<SceneRevision>().saturating_add(size_of::<u64>()),
                    ),
                ),
            ReservationKind::Transition,
        )?;
        let maximum_frame_bytes = alignment_gpu_frame_bytes(
            requests.len(),
            requests.len(),
            requests.len(),
            mandatory_pages,
            mandatory_pages.saturating_add(4),
        );
        let mut frame_reservation = ledger.reserve(
            AllocationClass::TransitionFrame,
            maximum_frame_bytes,
            ReservationKind::Transition,
        )?;
        let mut label_frames = Vec::with_capacity(requests.len());
        for request in requests {
            label_frames.push(CpuAlignmentProjector::project_labels_with_interaction(
                request.scene,
                request.camera_sequence,
                request.camera,
                request.interaction,
            )?);
        }
        let frame_buffers =
            self.acquire_frame_buffers(device, ledger, mandatory_pages, maximum_overlays)?;

        let mut page_requests = Vec::with_capacity(mandatory_pages);
        let mut overlay_instances = Vec::with_capacity(maximum_overlays);
        let mut scene_revisions = Vec::with_capacity(requests.len());
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for request in requests {
            scene_revisions.push(request.scene.revision());
            camera_sequences.push(request.camera_sequence);
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request
                .scene
                .pages()
                .iter()
                .filter(|page| page.interval().overlaps(interval))
                .filter(|page| ranges_overlap(page.row_range(), rows.clone()))
            {
                let resident = self
                    .page(page.key())
                    .expect("mandatory page presence was proven in the allocation-free pass");
                let overlay_start = u32::try_from(overlay_instances.len())
                    .expect("overlay address space was proven before allocation");
                if !request.interaction.is_empty() {
                    for primitive in page.primitives() {
                        let token = request.interaction.token(primitive.handle);
                        if !token.hovered && !token.selected && !token.selected_chain {
                            continue;
                        }
                        if let Some(instance) = pack_primitive(
                            *primitive,
                            Some(interaction_color(color_for(*primitive), token)),
                        )? {
                            overlay_instances.push(instance);
                        }
                    }
                }
                let overlay_end = u32::try_from(overlay_instances.len())
                    .expect("overlay address space was proven before allocation");
                page_requests.push(PendingAlignmentDraw {
                    page: page.key(),
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: page.interval().start(),
                    page_row_start: page.row_range().start,
                    overlay_range: overlay_start..overlay_end,
                    base_instance_count: resident.instance_count,
                });
            }
        }
        debug_assert_eq!(page_requests.len(), mandatory_pages);
        debug_assert_eq!(overlay_instances.len(), maximum_overlays);

        let mut uniforms = vec![0_u8; uniform_bytes];
        let mut draws = Vec::with_capacity(mandatory_pages);
        let next_revision = self.revision.saturating_add(1);
        let mut pins = Vec::with_capacity(mandatory_pages.saturating_add(2));
        for (index, request) in page_requests.iter().enumerate() {
            let uniform = camera_uniform(
                request.camera,
                request.screen_size,
                request.page_start,
                request.page_row_start,
            );
            let offset = index.saturating_mul(self.camera_stride);
            uniforms[offset..offset + size_of::<AlignmentGpuCameraUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
            let offset_u32 = u32::try_from(offset)
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
            let page = self
                .page_mut(request.page)
                .expect("mandatory page presence was checked before frame mutation");
            page.last_used_revision = next_revision;
            pins.push(page.allocation.pin()?);
            draws.push(PreparedAlignmentDraw {
                page: request.page,
                uniform_offset: offset_u32,
                scissor: request.scissor,
                overlay_range: request.overlay_range.clone(),
            });
        }
        if let Some(allocation) = &frame_buffers.camera_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &frame_buffers.overlay_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &self.page_table_allocation {
            pins.push(allocation.pin()?);
        }
        if !uniforms.is_empty() {
            queue.write_buffer(
                frame_buffers
                    .camera_buffer
                    .as_ref()
                    .expect("non-empty uniforms require a camera buffer"),
                0,
                &uniforms,
            );
        }
        if !overlay_instances.is_empty() {
            queue.write_buffer(
                frame_buffers
                    .overlay_buffer
                    .as_ref()
                    .expect("non-empty overlays require an overlay buffer"),
                0,
                bytemuck::cast_slice(&overlay_instances),
            );
        }
        self.revision = next_revision;
        let retained_frame_bytes = alignment_gpu_frame_bytes(
            scene_revisions.len(),
            camera_sequences.len(),
            label_frames.len(),
            draws.len(),
            pins.len().saturating_add(1),
        );
        let frame_allocation = frame_reservation.register(retained_frame_bytes, next_revision)?;
        pins.push(frame_allocation.pin()?);
        let frame = AlignmentGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: scene_revisions.into(),
            camera_sequences: camera_sequences.into(),
            label_frames: label_frames.into(),
            draws: draws.into(),
            buffers: frame_buffers,
            _allocation: frame_allocation,
            _pins: pins,
        };
        let snapshot = ledger.snapshot()?;
        let stats = AlignmentGpuPrepareStats {
            scenes: requests.len(),
            mandatory_pages,
            base_instances: page_requests.iter().fold(0_usize, |total, request| {
                total.saturating_add(request.base_instance_count as usize)
            }),
            overlay_instances: overlay_instances.len(),
            text_labels: frame
                .label_frames
                .iter()
                .map(|labels| labels.labels.len())
                .sum(),
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            page_cache_hits,
            page_cache_misses,
            resident_pages: self.pages.len(),
            resident_gpu_bytes: snapshot
                .records
                .iter()
                .filter(|record| record.class == AllocationClass::GpuPage)
                .fold(0_usize, |total, record| {
                    total.saturating_add(record.charged_capacity)
                }),
        };
        Ok((frame, stats))
    }

    /// Transactionally admits exactly the mandatory visible pages and then captures the GPU
    /// frame. This is the desktop frame-boundary operation: failure leaves the prior frame and
    /// semantic scene untouched, while a camera-only repeat uploads no base geometry.
    pub fn prepare_with_admission(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<
        (
            AlignmentGpuFrame,
            AlignmentGpuPrepareStats,
            AlignmentGpuBatchAdmission,
        ),
        AlignmentGpuFrameBuildError,
    > {
        let mut pages = requests
            .iter()
            .flat_map(|request| {
                let interval = request.camera.interval();
                let rows = request.camera.visible_rows();
                request
                    .scene
                    .pages()
                    .iter()
                    .filter(move |page| page.interval().overlaps(interval))
                    .filter(move |page| ranges_overlap(page.row_range(), rows.clone()))
                    .cloned()
            })
            .collect::<Vec<_>>();
        pages.sort_unstable_by_key(|page| page.key());
        pages.dedup_by_key(|page| page.key());
        let admission = self.admit_pages(device, queue, &pages, ledger, publication_revision)?;
        let (frame, stats) = self.prepare(device, queue, requests, ledger)?;
        Ok((frame, stats, admission))
    }

    /// Admits every canonical semantic page already prepared by the requested scenes, including
    /// the horizontal and vertical margins outside the live camera. This is speculative GPU
    /// residency only: callers first build and retain the mandatory visible frame, so failure
    /// here cannot blank or invalidate current pixels.
    pub fn prefetch_prepared_scene_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuBatchAdmission, AlignmentGpuPageAdmissionError> {
        let pages = prepared_scene_pages(requests);
        let admission = self.admit_pages(device, queue, &pages, ledger, publication_revision)?;
        // Prefetch has its own explicit admission telemetry. Do not attribute these uploads to
        // the next camera-only frame, which would falsely report unchanged geometry upload while
        // the user scrolls through the already-resident margin.
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        Ok(admission)
    }

    /// Admits at most `maximum_pages` currently missing pages from the complete prepared scene
    /// set. Missing pages are ordered by their canonical content key, so a caller can advance a
    /// large speculative warm-up in bounded, deterministic transactions. Mandatory visible-page
    /// admission remains atomic in `prepare_with_admission` and is not weakened by this method.
    pub fn prefetch_prepared_scene_pages_bounded(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        publication_revision: u64,
        maximum_pages: NonZeroUsize,
    ) -> Result<AlignmentGpuPrefetchProgress, AlignmentGpuPageAdmissionError> {
        let pages = prepared_scene_pages(requests);
        let prepared_pages = pages.len();
        let missing_pages_before = pages
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
            .count();
        let batch = pages
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
            .take(maximum_pages.get())
            .cloned()
            .collect::<Vec<_>>();
        let admission = self.admit_pages_protecting(
            device,
            queue,
            &batch,
            &pages,
            ledger,
            publication_revision,
        )?;
        // Bounded prefetch has explicit per-step telemetry just like the unbounded entry point.
        // Suppress these speculative uploads from the next live camera frame's accounting.
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        let remaining_missing_pages = pages
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
            .count();
        Ok(AlignmentGpuPrefetchProgress {
            prepared_pages,
            missing_pages_before,
            admission,
            remaining_missing_pages,
            missing_pages_remain: remaining_missing_pages > 0,
        })
    }

    /// Acquires one all-or-nothing pin set for every unique prepared page in `requests`. This does
    /// not admit pages: callers finish bounded prefetch first, then use the typed missing-page
    /// error to distinguish an incomplete warm-up from a ledger failure while pinning.
    pub fn pin_prepared_scene_pages(
        &self,
        requests: &[AlignmentGpuSceneRequest<'_>],
    ) -> Result<AlignmentGpuResidencyLease, AlignmentGpuResidencyError> {
        let pages = prepared_scene_pages(requests);
        let missing = pages
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
            .map(|page| page.key())
            .collect::<Vec<_>>();
        if let Some(first_missing) = missing.first().copied() {
            return Err(AlignmentGpuResidencyError::MissingPreparedPages {
                missing: missing.len(),
                first_missing,
            });
        }
        let mut page_keys = Vec::with_capacity(pages.len());
        let mut pins = Vec::with_capacity(pages.len());
        for page in pages {
            let resident = self
                .page(page.key())
                .expect("complete prepared-page residency was proven before pinning");
            page_keys.push(page.key());
            pins.push(resident.allocation.pin()?);
        }
        Ok(AlignmentGpuResidencyLease {
            device_generation: self.device_generation,
            page_keys: page_keys.into(),
            _pins: pins,
        })
    }

    pub fn render<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentGpuFrame,
    ) -> Result<(), AlignmentGpuRenderError> {
        if frame.device_generation != self.device_generation {
            return Err(AlignmentGpuRenderError::DeviceGenerationMismatch {
                frame: frame.device_generation,
                renderer: self.device_generation,
            });
        }
        if frame.draws.is_empty() {
            return Ok(());
        }
        let bind_group = frame
            .buffers
            .camera_bind_group
            .as_ref()
            .ok_or(AlignmentGpuRenderError::MissingCameraBuffer)?;
        pass.set_pipeline(&self.pipeline);
        for draw in frame.draws.iter() {
            let page = self
                .page(draw.page)
                .ok_or(AlignmentGpuRenderError::MissingPinnedPage(draw.page))?;
            pass.set_scissor_rect(
                draw.scissor.x,
                draw.scissor.y,
                draw.scissor.width,
                draw.scissor.height,
            );
            pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
            if let Some(buffer) = &page.buffer
                && page.instance_count > 0
            {
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..6, 0..page.instance_count);
            }
            if !draw.overlay_range.is_empty() {
                let overlay = frame
                    .buffers
                    .overlay_buffer
                    .as_ref()
                    .ok_or(AlignmentGpuRenderError::MissingOverlayBuffer)?;
                pass.set_vertex_buffer(0, overlay.slice(..));
                pass.draw(0..6, draw.overlay_range.clone());
            }
        }
        Ok(())
    }

    /// Releases every device-derived page and frame buffer. Semantic scenes, picking, selection,
    /// and evidence remain owned by `rgv-alignment-scene` and are intentionally untouched.
    pub fn discard_device_resources(&mut self) {
        self.pages = Vec::new();
        self.page_table_allocation = None;
        self.page_table_capacity = 0;
        self.frame_buffers.clear();
        self.device_generation = self.device_generation.saturating_add(1).max(1);
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_index(&self, key: AlignmentPageKey) -> Option<usize> {
        self.pages
            .binary_search_by_key(&key, |resident| resident.key)
            .ok()
    }

    fn page(&self, key: AlignmentPageKey) -> Option<&GpuAlignmentPage> {
        self.page_index(key).map(|index| &self.pages[index])
    }

    fn page_mut(&mut self, key: AlignmentPageKey) -> Option<&mut GpuAlignmentPage> {
        self.page_index(key).map(|index| &mut self.pages[index])
    }

    fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
    ) -> Result<(), AlignmentGpuPageAdmissionError> {
        if required <= self.page_table_capacity {
            return Ok(());
        }
        let requested_capacity = next_capacity(required)?;
        let maximum_bytes = requested_capacity
            .checked_mul(2)
            .and_then(|capacity| capacity.checked_mul(size_of::<GpuAlignmentPage>()))
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        let mut reservation = ledger.reserve(
            AllocationClass::GpuPage,
            maximum_bytes,
            ReservationKind::Transition,
        )?;
        let mut candidate = Vec::with_capacity(requested_capacity);
        let actual_capacity = candidate.capacity();
        if actual_capacity < required {
            return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                required,
                actual: actual_capacity,
            });
        }
        let actual_bytes = actual_capacity
            .checked_mul(size_of::<GpuAlignmentPage>())
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        if actual_bytes > maximum_bytes {
            return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                required: maximum_bytes,
                actual: actual_bytes,
            });
        }
        let allocation = reservation.register(actual_bytes, self.revision)?;
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = actual_capacity;
        self.page_table_allocation = Some(allocation);
        Ok(())
    }

    fn acquire_frame_buffers(
        &mut self,
        device: &wgpu::Device,
        ledger: &AllocationLedger,
        uniforms: usize,
        overlays: usize,
    ) -> Result<Arc<GpuAlignmentFrameBuffers>, AlignmentGpuPrepareError> {
        if let Some(slot) = self
            .frame_buffers
            .iter()
            .filter(|slot| Arc::strong_count(slot) == 1)
            .filter(|slot| slot.camera_capacity >= uniforms && slot.overlay_capacity >= overlays)
            .min_by_key(|slot| (slot.camera_capacity, slot.overlay_capacity))
        {
            return Ok(Arc::clone(slot));
        }

        let camera_capacity = (uniforms > 0)
            .then(|| next_capacity(uniforms))
            .transpose()?
            .unwrap_or(0);
        let overlay_capacity = (overlays > 0)
            .then(|| next_capacity(overlays))
            .transpose()?
            .unwrap_or(0);
        let camera_bytes = camera_capacity.saturating_mul(self.camera_stride);
        let overlay_bytes = overlay_capacity.saturating_mul(size_of::<AlignmentGpuInstance>());
        validate_device_buffer_size(device, camera_bytes)?;
        validate_device_buffer_size(device, overlay_bytes)?;

        let mut camera_reservation = (camera_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    camera_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
        let mut overlay_reservation = (overlay_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    overlay_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
        let camera_candidate = (camera_bytes > 0).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV alignment frame camera ring"),
                size: camera_bytes as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = create_camera_bind_group(device, &self.camera_layout, &buffer);
            (buffer, bind_group)
        });
        let overlay_candidate = (overlay_bytes > 0).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV alignment stable-handle overlay buffer"),
                size: overlay_bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            buffer
        });
        let camera_allocation = camera_reservation
            .as_mut()
            .map(|reservation| reservation.register(camera_bytes, self.revision))
            .transpose()?;
        let overlay_allocation = overlay_reservation
            .as_mut()
            .map(|reservation| reservation.register(overlay_bytes, self.revision))
            .transpose()?;
        let (camera_buffer, camera_bind_group) = camera_candidate
            .map_or((None, None), |(buffer, bind_group)| {
                (Some(buffer), Some(bind_group))
            });
        let slot = Arc::new(GpuAlignmentFrameBuffers {
            camera_buffer,
            camera_bind_group,
            camera_allocation,
            camera_capacity,
            overlay_buffer: overlay_candidate,
            overlay_allocation,
            overlay_capacity,
        });
        self.frame_buffers.push(Arc::clone(&slot));
        Ok(slot)
    }

    fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        protected: &[Arc<AlignmentSemanticPage>],
    ) -> Result<(), LedgerError> {
        loop {
            let snapshot = ledger.snapshot()?;
            if snapshot
                .charged
                .saturating_add(snapshot.reserved)
                .saturating_add(required)
                <= snapshot.budget
            {
                return Ok(());
            }
            let candidate = self
                .pages
                .iter()
                .enumerate()
                .filter(|(_, page)| {
                    !protected
                        .iter()
                        .any(|protected| protected.key() == page.key)
                })
                .filter_map(|(index, page)| {
                    page.allocation
                        .snapshot()
                        .ok()
                        .filter(|allocation| allocation.pins == 0)
                        .map(|_| (index, page.key, page.last_used_revision))
                })
                .min_by_key(|(_, key, revision)| (*revision, *key))
                .map(|(index, _, _)| index);
            let Some(candidate) = candidate else {
                return Ok(());
            };
            self.pages.remove(candidate);
        }
    }
}

fn alignment_gpu_frame_bytes(
    scene_revisions: usize,
    camera_sequences: usize,
    label_frames: usize,
    draws: usize,
    pins: usize,
) -> usize {
    const ARC_SLICE_HEADER_WORDS: usize = 2;
    size_of::<AlignmentGpuFrame>()
        .saturating_add(
            scene_revisions
                .saturating_mul(size_of::<SceneRevision>())
                .saturating_add(ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>())),
        )
        .saturating_add(
            camera_sequences
                .saturating_mul(size_of::<u64>())
                .saturating_add(ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>())),
        )
        .saturating_add(
            label_frames
                .saturating_mul(size_of::<CpuAlignmentLabelFrame>())
                .saturating_add(ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>())),
        )
        .saturating_add(
            draws
                .saturating_mul(size_of::<PreparedAlignmentDraw>())
                .saturating_add(ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>())),
        )
        .saturating_add(pins.saturating_mul(size_of::<PinnedAllocationLease>()))
}

fn pack_page(
    page: &AlignmentSemanticPage,
    interaction: Option<&AlignmentInteractionState>,
) -> Result<PackedAlignmentPage, AlignmentGpuPageAdmissionError> {
    let maximum = page.primitives().len();
    let mut instances = Vec::with_capacity(maximum);
    let mut semantic_indices = Vec::with_capacity(maximum);
    let mut text_primitive_count = 0_usize;
    for (index, primitive) in page.primitives().iter().copied().enumerate() {
        if matches!(primitive.component, AlignmentComponent::Label { .. }) {
            text_primitive_count = text_primitive_count.saturating_add(1);
            continue;
        }
        let color = interaction
            .map(|state| interaction_color(color_for(primitive), state.token(primitive.handle)));
        if let Some(instance) = pack_primitive(primitive, color)? {
            instances.push(instance);
            semantic_indices.push(
                u32::try_from(index)
                    .map_err(|_| AlignmentGpuPageAdmissionError::TooManyPrimitives(maximum))?,
            );
        }
    }
    instances.shrink_to_fit();
    semantic_indices.shrink_to_fit();
    Ok(PackedAlignmentPage {
        instances,
        semantic_indices,
        text_primitive_count,
    })
}

fn non_text_primitive_count(page: &AlignmentSemanticPage) -> usize {
    page.primitives()
        .iter()
        .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
        .count()
}

fn pack_primitive(
    primitive: AlignmentSemanticPrimitive,
    color: Option<[f32; 4]>,
) -> Result<Option<AlignmentGpuInstance>, AlignmentGpuPageAdmissionError> {
    if matches!(primitive.component, AlignmentComponent::Label { .. }) {
        return Ok(None);
    }
    if primitive.start_offset >= primitive.end_offset {
        return Err(AlignmentGpuPageAdmissionError::InvalidSemanticSpan {
            start: primitive.start_offset,
            end: primitive.end_offset,
        });
    }
    Ok(Some(AlignmentGpuInstance {
        start_offset: split_f64(primitive.start_offset as f64),
        end_offset: split_f64(primitive.end_offset as f64),
        row_offset: primitive.row_offset,
        shape: shape_for(primitive.geometry) as u32,
        color: color.unwrap_or_else(|| color_for(primitive)),
    }))
}

const fn shape_for(geometry: AlignmentGeometryToken) -> AlignmentGpuShape {
    match geometry {
        AlignmentGeometryToken::Rectangle => AlignmentGpuShape::Rectangle,
        AlignmentGeometryToken::EventLine => AlignmentGpuShape::EventLine,
        AlignmentGeometryToken::PointMarker => AlignmentGpuShape::PointMarker,
        AlignmentGeometryToken::DirectionalTerminalLeft => AlignmentGpuShape::DirectionTerminalLeft,
        AlignmentGeometryToken::DirectionalTerminalRight => {
            AlignmentGpuShape::DirectionTerminalRight
        }
    }
}

fn color_for(primitive: AlignmentSemanticPrimitive) -> [f32; 4] {
    match primitive.component {
        AlignmentComponent::Base { base, .. } => base_color(base),
        AlignmentComponent::ModifiedBase { state, .. } => match state {
            rgv_alignment_scene::ModificationState::Modified => [0.58, 0.24, 0.78, 1.0],
            rgv_alignment_scene::ModificationState::Canonical => [0.42, 0.42, 0.48, 1.0],
        },
        AlignmentComponent::BisulfiteBase { observation, .. } => match observation {
            rgv_alignment_scene::BisulfiteObservationToken::Protected => [0.86, 0.46, 0.12, 1.0],
            rgv_alignment_scene::BisulfiteObservationToken::Converted => [0.18, 0.62, 0.78, 1.0],
        },
        AlignmentComponent::Insertion { .. }
        | AlignmentComponent::SoftClip { .. }
        | AlignmentComponent::HardClip { .. }
        | AlignmentComponent::Padding { .. }
        | AlignmentComponent::UltimaIndel { .. }
        | AlignmentComponent::SbxInsertion { .. } => [0.55, 0.20, 0.70, 1.0],
        AlignmentComponent::Deletion { .. } => [0.18, 0.18, 0.20, 1.0],
        AlignmentComponent::ReferenceSkip { .. } | AlignmentComponent::PairConnector { .. } => {
            [0.38, 0.40, 0.46, 1.0]
        }
        AlignmentComponent::DirectionTerminal => style_color(primitive.style),
        AlignmentComponent::SmrtKinetics { .. } => [0.10, 0.63, 0.56, 1.0],
        AlignmentComponent::SbxTailBase { .. } => [0.86, 0.52, 0.12, 1.0],
        AlignmentComponent::SpecialistUnavailable { .. } => [0.52, 0.52, 0.54, 0.8],
        AlignmentComponent::Label { .. }
        | AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. } => style_color(primitive.style),
    }
}

fn style_color(style: AlignmentStyleToken) -> [f32; 4] {
    match style.body {
        AlignmentBodyColorToken::Neutral => [0.38, 0.48, 0.64, 1.0],
        AlignmentBodyColorToken::Strand { reverse: false } => [0.30, 0.50, 0.78, 1.0],
        AlignmentBodyColorToken::Strand { reverse: true } => [0.78, 0.34, 0.38, 1.0],
        AlignmentBodyColorToken::MappingQuality(quality) => {
            let fraction = f32::from(quality.unwrap_or(0)) / 60.0;
            [
                0.25 + fraction * 0.35,
                0.30 + fraction * 0.40,
                0.40 + fraction * 0.45,
                1.0,
            ]
        }
        AlignmentBodyColorToken::Category(category) => category
            .map_or([0.48, 0.48, 0.52, 1.0], |category| {
                hash_color(category.get())
            }),
        AlignmentBodyColorToken::TemplateLength { status, .. }
        | AlignmentBodyColorToken::PairOrientation(status)
        | AlignmentBodyColorToken::PairStatus(status) => hash_color(u64::from(status)),
        AlignmentBodyColorToken::FirstOfPairStrand(reverse) => match reverse {
            Some(false) => [0.28, 0.56, 0.78, 1.0],
            Some(true) => [0.80, 0.38, 0.42, 1.0],
            None => [0.50, 0.50, 0.54, 1.0],
        },
        AlignmentBodyColorToken::Specialist { mode, reverse } => {
            let mut color = hash_color(u64::from(mode));
            if reverse {
                color[0] = (color[0] + 0.18).min(1.0);
            }
            color
        }
    }
}

fn interaction_color(
    mut color: [f32; 4],
    token: rgv_alignment_scene::AlignmentInteractionToken,
) -> [f32; 4] {
    let (target, amount) = if token.selected {
        ([0.16, 0.42, 0.92], 0.58)
    } else if token.hovered {
        ([0.28, 0.52, 0.94], 0.32)
    } else if token.selected_chain {
        ([0.90, 0.62, 0.16], 0.38)
    } else {
        return color;
    };
    for channel in 0..3 {
        color[channel] += (target[channel] - color[channel]) * amount;
    }
    color[3] = 1.0;
    color
}

fn base_color(base: u8) -> [f32; 4] {
    match base.to_ascii_uppercase() {
        b'A' => [0.24, 0.66, 0.34, 1.0],
        b'C' => [0.24, 0.46, 0.82, 1.0],
        b'G' => [0.90, 0.64, 0.16, 1.0],
        b'T' => [0.82, 0.28, 0.30, 1.0],
        _ => [0.48, 0.48, 0.52, 1.0],
    }
}

fn hash_color(value: u64) -> [f32; 4] {
    let hue = (value % 360) as f32 / 360.0;
    let sector = hue * 6.0;
    let fraction = sector.fract();
    let (r, g, b) = match sector as u32 % 6 {
        0 => (1.0, fraction, 0.0),
        1 => (1.0 - fraction, 1.0, 0.0),
        2 => (0.0, 1.0, fraction),
        3 => (0.0, 1.0 - fraction, 1.0),
        4 => (fraction, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - fraction),
    };
    [0.25 + r * 0.50, 0.25 + g * 0.50, 0.25 + b * 0.50, 1.0]
}

fn camera_uniform(
    camera: AlignmentCamera,
    screen_size: [f32; 2],
    page_start: u64,
    page_row_start: u32,
) -> AlignmentGpuCameraUniform {
    AlignmentGpuCameraUniform {
        screen_size: [screen_size[0].max(1.0), screen_size[1].max(1.0)],
        clip_origin: [camera.clip().x as f32, camera.clip().y as f32],
        points_per_base: split_f64(1.0 / camera.bases_per_pixel()),
        page_start_from_camera: split_f64(page_start as f64 - camera.exact_start()),
        row_scroll: camera.row_scroll() as f32,
        row_height: camera.row_height() as f32,
        read_height: camera.read_height() as f32,
        page_row_start,
    }
}

fn validate_request(
    request: &AlignmentGpuSceneRequest<'_>,
) -> Result<(), AlignmentGpuPrepareError> {
    if request.screen_size_points[0] <= 0.0
        || request.screen_size_points[1] <= 0.0
        || !request.screen_size_points[0].is_finite()
        || !request.screen_size_points[1].is_finite()
        || request.scissor.width == 0
        || request.scissor.height == 0
    {
        return Err(AlignmentGpuPrepareError::InvalidFrameGeometry);
    }
    Ok(())
}

fn split_f64(value: f64) -> [f32; 2] {
    let high = value as f32;
    [high, (value - f64::from(high)) as f32]
}

fn aligned_uniform_stride(device: &wgpu::Device) -> usize {
    let alignment = device.limits().min_uniform_buffer_offset_alignment.max(1) as usize;
    size_of::<AlignmentGpuCameraUniform>()
        .div_ceil(alignment)
        .saturating_mul(alignment)
}

fn create_camera_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("RGV alignment camera bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size_of::<AlignmentGpuCameraUniform>() as u64),
            }),
        }],
    })
}

fn gpu_buffer_capacity<T>(count: usize) -> Result<usize, AlignmentGpuPageAdmissionError> {
    if count == 0 {
        return Ok(0);
    }
    next_capacity(count)?
        .checked_mul(size_of::<T>())
        .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)
}

fn next_capacity(count: usize) -> Result<usize, AlignmentGpuPageAdmissionError> {
    count
        .max(1)
        .checked_next_power_of_two()
        .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)
}

fn validate_device_buffer_size(
    device: &wgpu::Device,
    bytes: usize,
) -> Result<(), AlignmentGpuPageAdmissionError> {
    if bytes == 0 {
        return Ok(());
    }
    let limit = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
    if bytes > limit {
        return Err(AlignmentGpuPageAdmissionError::DeviceBufferLimit {
            required: bytes,
            limit,
        });
    }
    Ok(())
}

fn ranges_overlap(left: Range<u32>, right: Range<u32>) -> bool {
    left.start < right.end && left.end > right.start
}

fn prepared_scene_pages(
    requests: &[AlignmentGpuSceneRequest<'_>],
) -> Vec<Arc<AlignmentSemanticPage>> {
    let mut pages = requests
        .iter()
        .flat_map(|request| request.scene.pages().iter().cloned())
        .collect::<Vec<_>>();
    pages.sort_unstable_by_key(|page| page.key());
    pages.dedup_by_key(|page| page.key());
    pages
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentGpuPageAdmissionError {
    #[error("alignment GPU batch contains duplicate semantic page {0:?}")]
    DuplicatePageKey(AlignmentPageKey),
    #[error("alignment GPU page has {0} primitives, exceeding the u32 draw address space")]
    TooManyPrimitives(usize),
    #[error("alignment GPU page capacity overflowed usize")]
    CapacityOverflow,
    #[error("alignment semantic primitive has invalid span {start}..{end}")]
    InvalidSemanticSpan { start: u64, end: u64 },
    #[error(
        "alignment GPU page table capacity is invalid: required {required}, allocated {actual}"
    )]
    PageTableCapacity { required: usize, actual: usize },
    #[error("alignment GPU buffer requires {required} bytes but the device limit is {limit}")]
    DeviceBufferLimit { required: usize, limit: usize },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentGpuResidencyError {
    #[error(
        "{missing} prepared alignment GPU pages are not resident; first missing page is {first_missing:?}"
    )]
    MissingPreparedPages {
        missing: usize,
        first_missing: AlignmentPageKey,
    },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[derive(Debug, Error)]
pub enum AlignmentGpuPrepareError {
    #[error("alignment GPU frame geometry is invalid")]
    InvalidFrameGeometry,
    #[error("{missing} mandatory alignment GPU pages are not resident")]
    MissingMandatoryPages { missing: usize },
    #[error("alignment overlay count exceeds the u32 draw address space")]
    TooManyOverlayPrimitives,
    #[error("alignment dynamic-uniform offset exceeds the u32 address space")]
    DynamicUniformAddressSpace,
    #[error(transparent)]
    Admission(#[from] AlignmentGpuPageAdmissionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    LabelProjection(#[from] CpuProjectionError),
}

#[derive(Debug, Error)]
pub enum AlignmentGpuFrameBuildError {
    #[error(transparent)]
    Admission(#[from] AlignmentGpuPageAdmissionError),
    #[error(transparent)]
    Prepare(#[from] AlignmentGpuPrepareError),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentGpuRenderError {
    #[error(
        "GPU alignment frame belongs to device generation {frame}; renderer is at generation {renderer}"
    )]
    DeviceGenerationMismatch { frame: u64, renderer: u64 },
    #[error("GPU alignment frame has no camera buffer")]
    MissingCameraBuffer,
    #[error("GPU alignment frame has no overlay buffer")]
    MissingOverlayBuffer,
    #[error("a pinned GPU alignment page disappeared before render: {0:?}")]
    MissingPinnedPage(AlignmentPageKey),
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64},
        sync::Arc,
    };

    use rgv_alignment_scene::{
        AlignmentHandle, AlignmentOperation, AlignmentPageKey, AlignmentPresentationScene,
        AlignmentReadLabelMode, AlignmentRowAtlas, AlignmentSemanticPage, AlignmentSourceRevision,
        AllocationClass, AllocationLedger, CanonicalGenomicTile, CanonicalRecordChunk,
        CanonicalRowBlock, CapabilityRequirement, CompleteCoverage, ContractFingerprint,
        EvidenceAcquisition, EvidenceChunk, EvidenceRevision, EvidenceRootBuilder,
        FilteredRecordProvenance, GeometryLod, LayoutContract, PayloadContract, PayloadSidecar,
        PresentationContract, ReservationKind, RevisionFingerprint, RowAtlasRevision,
        SceneRevision, SourceChunkId, SourceFingerprint,
    };
    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
        ReadStackingMode,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, CigarOp, CigarOpKind, SequenceData, TemplateLengthBounds,
    };

    use super::*;

    fn handle() -> AlignmentHandle {
        AlignmentHandle::new(
            AlignmentSourceRevision::new(
                SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                RevisionFingerprint::new(3, 4),
            ),
            AlignmentSourcePosition::bam_virtual_offset(5),
        )
    }

    fn primitive(component: AlignmentComponent) -> AlignmentSemanticPrimitive {
        let geometry = match component {
            AlignmentComponent::Deletion { .. }
            | AlignmentComponent::ReferenceSkip { .. }
            | AlignmentComponent::PairConnector { .. } => AlignmentGeometryToken::EventLine,
            AlignmentComponent::Insertion { .. }
            | AlignmentComponent::SoftClip { .. }
            | AlignmentComponent::HardClip { .. }
            | AlignmentComponent::Padding { .. }
            | AlignmentComponent::UltimaIndel { .. }
            | AlignmentComponent::SbxInsertion { .. } => AlignmentGeometryToken::PointMarker,
            AlignmentComponent::DirectionTerminal => {
                AlignmentGeometryToken::DirectionalTerminalRight
            }
            _ => AlignmentGeometryToken::Rectangle,
        };
        AlignmentSemanticPrimitive {
            start_offset: 16_777_217,
            end_offset: 16_777_219,
            row_offset: 7,
            handle: handle(),
            component,
            geometry,
            style: AlignmentStyleToken {
                flags: 0,
                mapping_quality: Some(60),
                operation: Some(AlignmentOperation::SequenceMatch),
                body: AlignmentBodyColorToken::Category(Some(ContractFingerprint::new(99))),
            },
            draw_order: 3,
        }
    }

    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("RGV alignment scene test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .ok()
    }

    fn render_test_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &AlignmentGpuRenderer,
        frame: &AlignmentGpuFrame,
    ) -> Result<(), AlignmentGpuRenderError> {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGV alignment scene test target"),
            size: wgpu::Extent3d {
                width: 256,
                height: 128,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("RGV alignment scene test encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGV alignment scene test pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass, frame)?;
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }

    fn test_scene(ledger: &AllocationLedger) -> Arc<AlignmentPresentationScene> {
        test_scene_with_page_shape(
            ledger,
            NonZeroU64::new(100).unwrap(),
            1,
            NonZeroU32::new(16).unwrap(),
        )
    }

    fn test_scene_with_tile_bases(
        ledger: &AllocationLedger,
        tile_bases: NonZeroU64,
    ) -> Arc<AlignmentPresentationScene> {
        test_scene_with_page_shape(ledger, tile_bases, 1, NonZeroU32::new(16).unwrap())
    }

    fn test_scene_with_page_shape(
        ledger: &AllocationLedger,
        tile_bases: NonZeroU64,
        overlapping_records: usize,
        row_block_rows: NonZeroU32,
    ) -> Arc<AlignmentPresentationScene> {
        let source_revision = AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(11, 12)),
            RevisionFingerprint::new(13, 14),
        );
        let record_contract = ContractFingerprint::new(15);
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 20,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"gpu".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 10,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGTACGTAC".to_vec(),
                qualities: Some(vec![35; 10]),
            },
            auxiliary_fields: Vec::new(),
        };
        let mut decoded = AlignmentBatch::default();
        for ordinal in 0..overlapping_records {
            decoded
                .push_with_source_position(
                    &record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (1 << 16) | u64::try_from(ordinal).unwrap(),
                    ),
                )
                .unwrap();
        }
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = CanonicalRecordChunk::new(
            source_revision,
            SourceChunkId::BamBlock {
                compressed_offset: 1,
            },
            Arc::new(decoded.copy_core().unwrap()),
            record_contract,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let mut payload_reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let payload_allocation = payload_reservation.register(payload_bytes, 1).unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let mut core_reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core_allocation = core_reservation.register(footprint.core_bytes, 1).unwrap();
        let mut overlap_reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = overlap_reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let coverage = CompleteCoverage::new(
            source_revision,
            record_contract,
            interval,
            FilteredRecordProvenance {
                records_seen: u64::try_from(overlapping_records).unwrap(),
                eligible_records: u64::try_from(overlapping_records).unwrap(),
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut evidence_builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(16, 17)),
            source_revision,
            record_contract,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        evidence_builder.insert_acquisition(&acquisition).unwrap();
        let evidence = evidence_builder.build(2).unwrap();
        let layout = LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(18, 19)),
            &evidence,
            interval,
            layout,
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: rgv_alignment_scene::AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::ReadName,
                ..rgv_alignment_scene::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let rows = 0..u32::try_from(atlas.row_count()).unwrap();
        let mut pages = Vec::new();
        for tile in CanonicalGenomicTile::covering(interval, tile_bases, 100).unwrap() {
            let row_blocks = rows
                .end
                .saturating_add(row_block_rows.get().saturating_sub(1))
                / row_block_rows.get();
            for row_block in 0..row_blocks {
                for family in presentation.required_page_families() {
                    let key = AlignmentPageKey::new(
                        source_revision,
                        evidence.revision(),
                        atlas.revision(),
                        layout.fingerprint(),
                        presentation.page_fingerprint(family),
                        tile,
                        CanonicalRowBlock::new(row_block, row_block_rows),
                        family,
                        GeometryLod::Read,
                    );
                    let page = AlignmentSemanticPage::build(
                        key,
                        &evidence,
                        &atlas,
                        presentation,
                        100,
                        None,
                        ledger,
                        ReservationKind::Normal,
                        4,
                    )
                    .unwrap();
                    pages.push(page);
                }
            }
        }
        AlignmentPresentationScene::new(
            SceneRevision::new(RevisionFingerprint::new(20, 21)),
            evidence,
            atlas,
            presentation,
            interval,
            rows,
            pages,
            ledger,
            ReservationKind::Normal,
            5,
        )
        .unwrap()
    }

    #[test]
    fn page_local_coordinates_survive_beyond_single_f32_integer_precision() {
        let instance = pack_primitive(primitive(AlignmentComponent::ReadSpan), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            f64::from(instance.start_offset[0]) + f64::from(instance.start_offset[1]),
            16_777_217.0
        );
        assert_eq!(
            f64::from(instance.end_offset[0]) + f64::from(instance.end_offset[1]),
            16_777_219.0
        );
    }

    #[test]
    fn semantic_component_families_map_to_stable_gpu_shapes_without_labels() {
        assert_eq!(
            pack_primitive(
                primitive(AlignmentComponent::Deletion { cigar_index: 1 }),
                None
            )
            .unwrap()
            .unwrap()
            .shape,
            AlignmentGpuShape::EventLine as u32
        );
        assert_eq!(
            pack_primitive(
                primitive(AlignmentComponent::Insertion {
                    cigar_index: 1,
                    read_start: 2,
                    length: 3,
                }),
                None
            )
            .unwrap()
            .unwrap()
            .shape,
            AlignmentGpuShape::PointMarker as u32
        );
        assert_eq!(
            pack_primitive(primitive(AlignmentComponent::DirectionTerminal), None)
                .unwrap()
                .unwrap()
                .shape,
            AlignmentGpuShape::DirectionTerminalRight as u32
        );
        assert!(
            pack_primitive(
                primitive(AlignmentComponent::Label {
                    kind: rgv_alignment_scene::AlignmentLabelKind::ReadName,
                    id: ContractFingerprint::new(7),
                }),
                None
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn stable_handle_interaction_changes_only_overlay_color_not_base_geometry() {
        let semantic = primitive(AlignmentComponent::ReadSpan);
        let base = pack_primitive(semantic, None).unwrap().unwrap();
        let interaction = AlignmentInteractionState::new(Some(handle()), Some(handle()), []);
        let overlay = pack_primitive(
            semantic,
            Some(interaction_color(
                color_for(semantic),
                interaction.token(handle()),
            )),
        )
        .unwrap()
        .unwrap();
        assert_eq!(base.start_offset, overlay.start_offset);
        assert_eq!(base.end_offset, overlay.end_offset);
        assert_eq!(base.row_offset, overlay.row_offset);
        assert_eq!(base.shape, overlay.shape);
        assert_ne!(base.color, overlay.color);
    }

    #[test]
    fn camera_uniform_is_host_shareable_and_double_single() {
        assert_eq!(size_of::<AlignmentGpuCameraUniform>(), 48);
        let split = split_f64(5_525_127.3);
        assert!(((f64::from(split[0]) + f64::from(split[1])) - 5_525_127.3).abs() < 1.0e-7);
    }

    #[test]
    fn packed_gpu_geometry_matches_the_cpu_reference_projector() {
        let ledger = AllocationLedger::new(16 << 20, 2 << 20);
        let scene = test_scene(&ledger);
        let camera = AlignmentCamera::new(
            scene.prepared_interval(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 17.0,
                y: 23.0,
                width: 503.0,
                height: 137.0,
            },
        )
        .unwrap();
        let cpu = rgv_alignment_scene::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();

        for page in scene.pages() {
            let packed = PackedAlignmentPage::from_semantic(page).unwrap();
            for (instance, semantic_index) in packed
                .instances
                .iter()
                .copied()
                .zip(packed.semantic_indices.iter().copied())
            {
                let semantic = page.primitives()[semantic_index as usize];
                let Some(gpu) = project_alignment_gpu_instance(
                    instance,
                    page.interval().start(),
                    page.row_range().start,
                    camera,
                ) else {
                    continue;
                };
                let row = page.row_range().start + semantic.row_offset;
                let matching = cpu.primitives.iter().find(|candidate| {
                    candidate.handle == semantic.handle
                        && candidate.component == semantic.component
                        && candidate.row == row
                        && (candidate.x - gpu.x).abs() < 1.0e-6
                        && (candidate.y - gpu.y).abs() < 1.0e-6
                        && (candidate.width - gpu.width).abs() < 1.0e-6
                        && (candidate.height - gpu.height).abs() < 1.0e-6
                });
                assert!(
                    matching.is_some(),
                    "GPU instance disagrees with CPU component {:?}",
                    semantic.component
                );
            }
        }
    }

    #[test]
    fn dedicated_alignment_pipeline_is_valid_on_an_available_headless_adapter() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };

        let renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(renderer.resident_page_count(), 0);
    }

    #[test]
    fn mandatory_batch_failure_never_publishes_a_partial_gpu_page_set() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene(&ledger);
        let baseline = ledger.snapshot().unwrap().charged;
        ledger.set_budget(baseline.saturating_add(1)).unwrap();
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let result = renderer.admit_pages(&device, &queue, scene.pages(), &ledger, 10);
        assert!(matches!(
            result,
            Err(AlignmentGpuPageAdmissionError::Ledger(
                LedgerError::BudgetExceeded { .. }
            ))
        ));
        assert_eq!(renderer.resident_page_count(), 0);

        ledger.set_budget(16 << 20).unwrap();
        let admitted = renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 11)
            .unwrap();
        assert_eq!(admitted.requested_pages, scene.pages().len());
        assert_eq!(admitted.admitted_pages, scene.pages().len());
        assert_eq!(renderer.resident_page_count(), scene.pages().len());
    }

    #[test]
    fn frame_boundary_admission_uploads_once_then_pan_changes_only_camera_state() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene(&ledger);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
        let interaction = AlignmentInteractionState::default();
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        };
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (first, first_stats, first_admission) = renderer
            .prepare_with_admission(&device, &queue, &[request], &ledger, 10)
            .unwrap();
        assert!(first_admission.admitted_pages > 0);
        assert!(first_stats.geometry_uploaded_bytes > 0);
        drop(first);

        let viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
        let panned = AlignmentCamera::from_viewport(viewport, 0.0, 12.0, 9.0, clip).unwrap();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: panned,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (_second, second_stats, second_admission) = renderer
            .prepare_with_admission(&device, &queue, &[request], &ledger, 11)
            .unwrap();
        assert_eq!(second_admission.admitted_pages, 0);
        assert_eq!(second_admission.already_resident_pages, scene.pages().len());
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(second_stats.page_cache_misses, 0);
    }

    #[test]
    fn prepared_margin_prefetch_makes_the_next_tile_a_camera_only_frame() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(50).unwrap());
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let interaction = AlignmentInteractionState::default();
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        };
        let first_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 50).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let first_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: first_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (first, _, mandatory) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                std::slice::from_ref(&first_request),
                &ledger,
                10,
            )
            .unwrap();
        assert!(mandatory.admitted_pages > 0);
        let prefetch = renderer
            .prefetch_prepared_scene_pages(
                &device,
                &queue,
                std::slice::from_ref(&first_request),
                &ledger,
                11,
            )
            .unwrap();
        assert!(prefetch.admitted_pages > 0);
        assert_eq!(renderer.resident_page_count(), scene.pages().len());
        drop(first);

        let second_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 50, 100).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let second_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: second_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (_second, second_stats, second_admission) = renderer
            .prepare_with_admission(&device, &queue, &[second_request], &ledger, 12)
            .unwrap();
        assert_eq!(second_admission.admitted_pages, 0);
        assert!(second_admission.already_resident_pages > 0);
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    }

    #[test]
    fn prepared_vertical_pages_are_uploaded_before_the_row_camera_reaches_them() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene_with_page_shape(
            &ledger,
            NonZeroU64::new(100).unwrap(),
            4,
            NonZeroU32::new(1).unwrap(),
        );
        assert_eq!(scene.atlas().row_count(), 4);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 5.0,
        };
        let interaction = AlignmentInteractionState::default();
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 5,
        };
        let first_camera =
            AlignmentCamera::new(scene.prepared_interval(), 0.0, 10.0, 4.0, clip).unwrap();
        assert_eq!(first_camera.visible_rows(), 0..2);
        let first_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: first_camera,
            screen_size_points: [256.0, 5.0],
            scissor,
            interaction: &interaction,
        };
        let (first, _, mandatory) = renderer
            .prepare_with_admission(
                &device,
                &queue,
                std::slice::from_ref(&first_request),
                &ledger,
                10,
            )
            .unwrap();
        assert!(mandatory.admitted_pages > 0);
        let prefetch = renderer
            .prefetch_prepared_scene_pages(
                &device,
                &queue,
                std::slice::from_ref(&first_request),
                &ledger,
                11,
            )
            .unwrap();
        assert!(prefetch.admitted_pages > 0);
        assert_eq!(renderer.resident_page_count(), scene.pages().len());
        drop(first);

        let second_camera =
            AlignmentCamera::new(scene.prepared_interval(), 2.0, 10.0, 4.0, clip).unwrap();
        assert_eq!(second_camera.visible_rows(), 2..4);
        let second_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: second_camera,
            screen_size_points: [256.0, 5.0],
            scissor,
            interaction: &interaction,
        };
        let (_second, second_stats, second_admission) = renderer
            .prepare_with_admission(&device, &queue, &[second_request], &ledger, 12)
            .unwrap();
        assert_eq!(second_admission.admitted_pages, 0);
        assert!(second_admission.already_resident_pages > 0);
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    }

    #[test]
    fn bounded_prepared_scene_prefetch_advances_deterministically_without_duplicate_uploads() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(50).unwrap());
        assert!(scene.pages().len() > 2);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let camera = AlignmentCamera::new(
            scene.prepared_interval(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 128.0,
            },
        )
        .unwrap();
        let interaction = AlignmentInteractionState::default();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 128,
            },
            interaction: &interaction,
        };
        let requests = [request];
        let maximum_pages = NonZeroUsize::new(2).unwrap();
        let first = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &requests,
                &ledger,
                10,
                maximum_pages,
            )
            .unwrap();
        assert_eq!(first.prepared_pages, scene.pages().len());
        assert_eq!(first.missing_pages_before, scene.pages().len());
        assert_eq!(first.admission.requested_pages, 2);
        assert_eq!(first.admission.admitted_pages, 2);
        assert_eq!(first.remaining_missing_pages, scene.pages().len() - 2);
        assert!(first.missing_pages_remain);
        let mut expected_keys = scene
            .pages()
            .iter()
            .map(|page| page.key())
            .collect::<Vec<_>>();
        expected_keys.sort_unstable();
        expected_keys.dedup();
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.key)
                .collect::<Vec<_>>(),
            expected_keys[..2]
        );

        let mut total_admitted = first.admission.admitted_pages;
        let mut total_uploaded = first.admission.uploaded_bytes;
        let mut revision = 11_u64;
        while renderer.resident_page_count() < scene.pages().len() {
            let progress = renderer
                .prefetch_prepared_scene_pages_bounded(
                    &device,
                    &queue,
                    &requests,
                    &ledger,
                    revision,
                    maximum_pages,
                )
                .unwrap();
            assert!(progress.admission.admitted_pages <= maximum_pages.get());
            assert_eq!(progress.admission.already_resident_pages, 0);
            total_admitted = total_admitted.saturating_add(progress.admission.admitted_pages);
            total_uploaded = total_uploaded.saturating_add(progress.admission.uploaded_bytes);
            revision = revision.saturating_add(1);
        }
        assert_eq!(total_admitted, expected_keys.len());
        assert!(total_uploaded > 0);

        let complete = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &requests,
                &ledger,
                revision,
                maximum_pages,
            )
            .unwrap();
        assert!(complete.is_complete());
        assert_eq!(complete.missing_pages_before, 0);
        assert_eq!(complete.admission.requested_pages, 0);
        assert_eq!(complete.admission.admitted_pages, 0);
        assert_eq!(complete.admission.uploaded_bytes, 0);
        assert_eq!(renderer.resident_page_count(), expected_keys.len());
    }

    #[test]
    fn completed_scene_residency_lease_requires_every_page_and_pins_until_drop() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(50).unwrap());
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let camera = AlignmentCamera::new(
            scene.prepared_interval(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 128.0,
            },
        )
        .unwrap();
        let interaction = AlignmentInteractionState::default();
        let requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 128,
            },
            interaction: &interaction,
        }];

        assert!(matches!(
            renderer.pin_prepared_scene_pages(&requests),
            Err(AlignmentGpuResidencyError::MissingPreparedPages {
                missing,
                ..
            }) if missing == scene.pages().len()
        ));
        renderer
            .prefetch_prepared_scene_pages(&device, &queue, &requests, &ledger, 10)
            .unwrap();
        let lease = renderer.pin_prepared_scene_pages(&requests).unwrap();
        assert_eq!(lease.device_generation(), renderer.device_generation);
        assert_eq!(lease.page_count(), scene.pages().len());
        assert!(!lease.is_empty());
        assert!(lease.page_keys().windows(2).all(|pair| pair[0] < pair[1]));

        let resident = renderer.resident_page_count();
        renderer
            .evict_unpinned_pages_for_excluding(&ledger, usize::MAX / 2, &[])
            .unwrap();
        assert_eq!(renderer.resident_page_count(), resident);
        drop(lease);
        renderer
            .evict_unpinned_pages_for_excluding(&ledger, usize::MAX / 2, &[])
            .unwrap();
        assert_eq!(renderer.resident_page_count(), 0);
    }

    #[test]
    fn preparing_a_replacement_does_not_overwrite_an_in_flight_frame() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene(&ledger);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
            .unwrap();
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
        let interaction = AlignmentInteractionState::default();
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        };
        let first_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (first, _) = renderer
            .prepare(&device, &queue, &[first_request], &ledger)
            .unwrap();
        let panned_viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
        let panned = AlignmentCamera::from_viewport(panned_viewport, 0.0, 12.0, 9.0, clip).unwrap();
        let second_request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: panned,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        };
        let (second, stats) = renderer
            .prepare(&device, &queue, &[second_request], &ledger)
            .unwrap();
        assert_eq!(stats.geometry_uploaded_bytes, 0);
        let cpu = CpuAlignmentProjector::project_with_interaction(&scene, 2, panned, &interaction)
            .unwrap();
        let cpu_labels = cpu
            .primitives
            .iter()
            .filter(|primitive| {
                primitive.shape == rgv_alignment_scene::CpuPrimitiveShape::TextLabel
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(!cpu_labels.is_empty());
        assert_eq!(second.label_frames().len(), 1);
        assert_eq!(
            second.label_frames()[0].labels.as_ref(),
            cpu_labels.as_slice()
        );
        assert_eq!(stats.text_labels, cpu_labels.len());
        assert_ne!(first.revision(), second.revision());
        render_test_frame(&device, &queue, &renderer, &first).unwrap();
        render_test_frame(&device, &queue, &renderer, &second).unwrap();

        let snapshot = ledger.snapshot().unwrap();
        assert!(
            snapshot.records.iter().any(|record| {
                record.class == AllocationClass::TransitionFrame && record.pins > 0
            })
        );
        assert!(renderer.frame_buffers.len() >= 2);
    }

    #[test]
    fn active_frame_pins_prevent_gpu_page_eviction_until_submission_releases() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene(&ledger);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
            .unwrap();
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
        let interaction = AlignmentInteractionState::default();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 128,
            },
            interaction: &interaction,
        };
        let (frame, _) = renderer
            .prepare(&device, &queue, &[request], &ledger)
            .unwrap();
        let resident = renderer.resident_page_count();
        renderer
            .evict_unpinned_pages_for_excluding(&ledger, usize::MAX / 2, &[])
            .unwrap();
        assert_eq!(renderer.resident_page_count(), resident);
        drop(frame);
        renderer
            .evict_unpinned_pages_for_excluding(&ledger, usize::MAX / 2, &[])
            .unwrap();
        assert_eq!(renderer.resident_page_count(), 0);
    }

    #[test]
    fn admitted_geometry_is_reused_across_camera_frames_and_device_discard_preserves_scene() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
        let scene = test_scene(&ledger);
        let semantic_handles = scene
            .pages()
            .iter()
            .flat_map(|page| page.primitives().iter().map(|primitive| primitive.handle))
            .collect::<Vec<_>>();
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut expected_uploaded = 0_usize;
        for page in scene.pages() {
            match renderer
                .admit_page(&device, &queue, page, &ledger, 10)
                .unwrap()
            {
                AlignmentGpuPageAdmission::Admitted { uploaded_bytes, .. } => {
                    expected_uploaded = expected_uploaded.saturating_add(uploaded_bytes);
                }
                AlignmentGpuPageAdmission::AlreadyResident => {
                    panic!("fixture page must be admitted exactly once")
                }
            }
        }
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
        let interaction = AlignmentInteractionState::default();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [256.0, 128.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 128,
            },
            interaction: &interaction,
        };
        let (first, first_stats) = renderer
            .prepare(&device, &queue, &[request], &ledger)
            .unwrap();
        assert_eq!(first_stats.geometry_uploaded_bytes, expected_uploaded);
        drop(first);

        let viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
        let panned = AlignmentCamera::from_viewport(viewport, 0.0, 12.0, 9.0, clip).unwrap();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: panned,
            screen_size_points: [256.0, 128.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 256,
                height: 128,
            },
            interaction: &interaction,
        };
        let (second, second_stats) = renderer
            .prepare(&device, &queue, &[request], &ledger)
            .unwrap();
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(second_stats.page_cache_misses, 0);
        assert_eq!(second.scene_revisions.as_ref(), &[scene.revision()]);

        render_test_frame(&device, &queue, &renderer, &second).unwrap();
        renderer.discard_device_resources();
        assert_eq!(renderer.resident_page_count(), 0);
        assert!(matches!(
            render_test_frame(&device, &queue, &renderer, &second),
            Err(AlignmentGpuRenderError::DeviceGenerationMismatch { .. })
        ));
        assert!(
            semantic_handles
                .iter()
                .all(|handle| scene.contains_handle(*handle))
        );
        drop(second);
        assert_eq!(
            ledger
                .snapshot()
                .unwrap()
                .records
                .iter()
                .filter(|record| record.class == AllocationClass::GpuPage)
                .count(),
            0
        );
    }
}
