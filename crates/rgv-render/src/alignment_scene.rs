use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use bytemuck::{Pod, Zeroable};
use rgv_alignment_scene::{
    AlignmentBodyColorToken, AlignmentCamera, AlignmentComponent, AlignmentGeometryToken,
    AlignmentInteractionState, AlignmentPageKey, AlignmentPanelLaneFrame,
    AlignmentPresentationScene, AlignmentSemanticPage, AlignmentSemanticPrimitive,
    AlignmentStyleToken, AllocationBundleActual, AllocationBundleDemand, AllocationBundleError,
    AllocationBundleLease, AllocationBundleReservation, AllocationBundleSlot, AllocationClass,
    AllocationId, AllocationLease, AllocationLedger, AllocationRetirementClaimSet,
    AllocationRetirementToken, CpuAlignmentLabelFrame, CpuAlignmentProjector, CpuProjectionError,
    LedgerError, LedgerProgress, PinnedAllocationLease, ReservationKind, SceneRevision,
    TransientResourceClass,
};
use thiserror::Error;

use crate::{AlignmentLedgerPolicyView, GpuBudgetPressure, GpuReclaimGoal, GpuReclaimReport};

const SHADER: &str = include_str!("alignment_scene.wgsl");

/// Two slots are sufficient for a strict present-next/drop-previous frame boundary: the
/// currently presented frame owns one slot while the next camera frame is captured into the
/// other. A third concurrently retained candidate is a coordinator bug/back-pressure condition,
/// not a reason to grow the shared GPU ledger during interaction.
const ALIGNMENT_CAMERA_FRAME_SLOT_COUNT: usize = 2;
/// Hover, selected read, and the other member of a paired/template selection chain. A larger
/// application interaction set remains valid but requires a content replacement to certify a
/// larger overlay slot rather than pessimistically charging every primitive in a large horizon.
const ORDINARY_INTERACTION_HANDLES: usize = 3;

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
            camera: lane.read_camera?,
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
    pub label_anchors_examined: usize,
    /// New ledger pins needed for visible immutable pages. Consecutive contained frames should
    /// reuse the prior frame's leases and report zero here.
    pub page_pin_acquisitions: usize,
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
    /// Physical capacity represented by optional pins retained by this cursor.
    pub retained_bytes: usize,
    /// At least one admitted page remained deliberately unpinned because the aggregate optional
    /// pin ceiling had been reached.
    pub retention_limited: bool,
}

/// One content-authority-bound speculative warm-up. The cursor retains only immutable scenes and
/// the replacement-minted key authority. Every UI-frame advance resolves, inspects, and admits at
/// most the requested quantum; there is no complete-scene setup scan. Pages already visited are
/// pinned so later quanta cannot evict earlier work.
#[must_use]
pub struct AlignmentGpuPrefetchCursor {
    device_generation: u64,
    page_keys: Arc<[AlignmentPageKey]>,
    scenes: Vec<Arc<AlignmentPresentationScene>>,
    next_page: usize,
    pins: Vec<PinnedAllocationLease>,
    retained_bytes: usize,
    maximum_total_pinned_bytes: usize,
    retention_limited: bool,
}

impl AlignmentGpuPrefetchCursor {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.next_page == self.page_keys.len()
    }

    #[must_use]
    pub fn remaining_pages(&self) -> usize {
        self.page_keys.len().saturating_sub(self.next_page)
    }

    #[must_use]
    pub const fn retention_limited(&self) -> bool {
        self.retention_limited
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

/// Constant-size renderer instrumentation used to prove that a camera-only capture does not
/// rebuild the complete prepared-content manifest. Visible mandatory-page work is intentionally
/// not counted here: it scales with the presentation being drawn, not the full prepared scene.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentGpuRendererOperationCounters {
    pub content_manifest_builds: u64,
    pub content_manifest_pages_enumerated: u64,
    pub prefetch_authority_keys_inspected: u64,
    pub reclaim_authority_keys_inspected: u64,
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
    allocation: Option<AllocationLease>,
    last_used_revision: u64,
}

#[derive(Clone, Copy, Debug)]
struct AlignmentPageReclaimCandidate {
    key: AlignmentPageKey,
    charged_capacity: usize,
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

#[derive(Debug)]
struct AlignmentGpuResidentPageLease {
    key: AlignmentPageKey,
    allocation: AllocationId,
    pin: PinnedAllocationLease,
}

#[derive(Debug, Default)]
struct AlignmentGpuContentLeaseCache {
    pages: Mutex<BTreeMap<AlignmentPageKey, Weak<AlignmentGpuResidentPageLease>>>,
}

impl AlignmentGpuContentLeaseCache {
    fn acquire(
        &self,
        key: AlignmentPageKey,
        allocation: &AllocationLease,
    ) -> Result<(Arc<AlignmentGpuResidentPageLease>, bool), LedgerError> {
        let mut pages = self.pages.lock().map_err(|_| LedgerError::Poisoned)?;
        if let Some(existing) = pages.get(&key).and_then(Weak::upgrade)
            && existing.allocation == allocation.id()
        {
            return Ok((existing, false));
        }
        let lease = Arc::new(AlignmentGpuResidentPageLease {
            key,
            allocation: allocation.id(),
            pin: allocation.pin()?,
        });
        pages.insert(key, Arc::downgrade(&lease));
        Ok((lease, true))
    }
}

/// Camera-independent content authority for a read presentation. Replacement publication mints
/// this token from the exact ordered immutable scene identities and complete prepared-page
/// manifest. Camera, row scroll, scissor, hover, and selection are deliberately absent: those
/// values can produce a new presentation frame without replacing immutable GPU content.
///
/// Camera capture validates the constant-size-per-lane scene identity. The manifest remains the
/// replacement/retirement authority, but is deliberately not rebuilt during pan or row scroll.
#[derive(Clone, Debug)]
pub struct AlignmentGpuContentAuthority {
    device_generation: u64,
    scene_revisions: Arc<[SceneRevision]>,
    scene_allocations: Arc<[AllocationId]>,
    page_manifest: Arc<[AlignmentPageKey]>,
    leases: Arc<AlignmentGpuContentLeaseCache>,
}

impl AlignmentGpuContentAuthority {
    fn new(
        device_generation: u64,
        scene_revisions: Arc<[SceneRevision]>,
        scene_allocations: Arc<[AllocationId]>,
        page_manifest: Arc<[AlignmentPageKey]>,
    ) -> Self {
        Self {
            device_generation,
            scene_revisions,
            scene_allocations,
            page_manifest,
            leases: Arc::new(AlignmentGpuContentLeaseCache::default()),
        }
    }

    #[must_use]
    pub const fn device_generation(&self) -> u64 {
        self.device_generation
    }

    #[must_use]
    pub fn scene_revisions(&self) -> &[SceneRevision] {
        &self.scene_revisions
    }

    #[must_use]
    pub fn page_manifest(&self) -> &[AlignmentPageKey] {
        &self.page_manifest
    }
}

impl PartialEq for AlignmentGpuContentAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.device_generation == other.device_generation
            && self.scene_revisions == other.scene_revisions
            && self.scene_allocations == other.scene_allocations
            && self.page_manifest == other.page_manifest
    }
}

impl Eq for AlignmentGpuContentAuthority {}

/// Immutable lease for one fully prepared multi-scene GPU submission. All mandatory page
/// allocations and renderer-owned frame buffers remain pinned through command encoding.
#[derive(Debug)]
pub struct AlignmentGpuFrame {
    revision: u64,
    device_generation: u64,
    pub scene_revisions: Arc<[SceneRevision]>,
    pub camera_sequences: Arc<[u64]>,
    content_authority: AlignmentGpuContentAuthority,
    label_frames: Arc<[CpuAlignmentLabelFrame]>,
    draws: Arc<[PreparedAlignmentDraw]>,
    buffers: Arc<GpuAlignmentFrameBuffers>,
    _page_leases: Vec<Arc<AlignmentGpuResidentPageLease>>,
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

    /// Returns the camera-independent authority which may be retained while this presentation
    /// frame is replaced by camera/scroll/interaction-only captures.
    #[must_use]
    pub const fn content_authority(&self) -> &AlignmentGpuContentAuthority {
        &self.content_authority
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
    /// Retained CPU-frame capacity charged with this physical camera slot. The renderer owns the
    /// lease and each in-flight frame pins it, exactly like the GPU buffers above.
    frame_allocation: AllocationLease,
    frame_capacity: usize,
    capture_ring: bool,
}

#[derive(Clone, Copy, Debug)]
struct PlannedAlignmentFrameSlot {
    camera_slot: Option<AllocationBundleSlot>,
    overlay_slot: Option<AllocationBundleSlot>,
    frame_slot: AllocationBundleSlot,
}

struct PlannedAlignmentPage {
    page: Arc<AlignmentSemanticPage>,
    slot: AllocationBundleSlot,
    instance_count: u32,
    maximum_bytes: usize,
}

/// Allocation-free retained-capacity inventory for one foreground read-frame replacement.
/// Bundle slots are deterministic from the caller-supplied first slot and can therefore be
/// composed with another renderer's plan before either renderer stages device objects.
#[must_use]
pub struct AlignmentGpuReplacementPlan {
    policy_revision: u64,
    first_bundle_slot: AllocationBundleSlot,
    next_bundle_slot: AllocationBundleSlot,
    renderer_revision: u64,
    device_generation: u64,
    scene_revisions: Arc<[SceneRevision]>,
    scene_allocations: Arc<[AllocationId]>,
    mandatory_pages: Vec<Arc<AlignmentSemanticPage>>,
    prepared_page_keys: Arc<[AlignmentPageKey]>,
    missing_pages: Vec<PlannedAlignmentPage>,
    demands: Vec<AllocationBundleDemand>,
    page_table_slot: Option<AllocationBundleSlot>,
    page_table_requested_capacity: usize,
    page_table_maximum_bytes: usize,
    camera_capacity: usize,
    camera_bytes: usize,
    overlay_capacity: usize,
    overlay_bytes: usize,
    frame_slots: [PlannedAlignmentFrameSlot; ALIGNMENT_CAMERA_FRAME_SLOT_COUNT],
    maximum_frame_bytes: usize,
    request_count: usize,
    mandatory_draws: usize,
    maximum_overlays: usize,
    page_cache_hits: usize,
    page_cache_misses: usize,
}

impl AlignmentGpuReplacementPlan {
    #[must_use]
    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    #[must_use]
    pub fn demands(&self) -> &[AllocationBundleDemand] {
        &self.demands
    }

    #[must_use]
    pub const fn first_bundle_slot(&self) -> AllocationBundleSlot {
        self.first_bundle_slot
    }

    #[must_use]
    pub const fn next_bundle_slot(&self) -> AllocationBundleSlot {
        self.next_bundle_slot
    }

    #[must_use]
    pub fn prepared_page_keys(&self) -> &Arc<[AlignmentPageKey]> {
        &self.prepared_page_keys
    }
}

struct RetiredAlignmentFrameBuffer {
    index: usize,
    buffers: Arc<GpuAlignmentFrameBuffers>,
}

/// Exact old read-presentation ownership detached before the shared ledger evaluates a
/// replacement bundle. The token is intentionally non-Clone: its opaque claim set is valid only
/// while this object continues to own every referenced lease and pin.
#[must_use = "restore this retirement after refusal or drop it before bundle settlement"]
pub struct AlignmentGpuRetirement {
    claim_set: AllocationRetirementClaimSet,
    prior_frame: Option<AlignmentGpuFrame>,
    retired_pages: Vec<GpuAlignmentPage>,
    retired_page_table: Option<(AllocationLease, usize)>,
    retired_frame_buffers: Vec<RetiredAlignmentFrameBuffer>,
    renderer_revision: u64,
    device_generation: u64,
}

impl AllocationRetirementToken for AlignmentGpuRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

impl AlignmentGpuRetirement {
    #[must_use]
    pub fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

struct StagedAlignmentPage {
    key: AlignmentPageKey,
    slot: Option<AllocationBundleSlot>,
    buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    charged_capacity: usize,
}

struct StagedAlignmentFrameBuffers {
    camera_buffer: Option<wgpu::Buffer>,
    camera_bind_group: Option<wgpu::BindGroup>,
    camera_slot: Option<AllocationBundleSlot>,
    camera_capacity: usize,
    overlay_buffer: Option<wgpu::Buffer>,
    overlay_slot: Option<AllocationBundleSlot>,
    overlay_capacity: usize,
    frame_slot: AllocationBundleSlot,
}

/// Fully built read candidate whose objects are still private and carry no retained leases. Its
/// actuals may be committed to the shared bundle; publication becomes possible only after the
/// old retirement token is dropped and the commit returns settled leases.
#[must_use = "a staged replacement must be published or dropped"]
pub struct AlignmentGpuStagedReplacement {
    plan: AlignmentGpuReplacementPlan,
    pages: Vec<StagedAlignmentPage>,
    page_table: Option<Vec<GpuAlignmentPage>>,
    buffers: Vec<StagedAlignmentFrameBuffers>,
    scene_revisions: Vec<SceneRevision>,
    scene_allocations: Vec<AllocationId>,
    camera_sequences: Vec<u64>,
    label_frames: Vec<CpuAlignmentLabelFrame>,
    draws: Vec<PreparedAlignmentDraw>,
    actuals: Vec<AllocationBundleActual>,
    admission: AlignmentGpuBatchAdmission,
    base_instances: usize,
    text_labels: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
}

impl AlignmentGpuStagedReplacement {
    #[must_use]
    pub fn actuals(&self) -> &[AllocationBundleActual] {
        &self.actuals
    }
}

/// A staged read candidate whose renderer/cache invariants and existing allocation pins were
/// checked before bundle commit. Coordinators preflight every renderer before retiring either old
/// presentation.
#[must_use = "bind the settled bundle leases before publication"]
pub struct AlignmentGpuReadyReplacement {
    staged: AlignmentGpuStagedReplacement,
    existing_pins: Vec<PinnedAllocationLease>,
    resident_gpu_bytes: usize,
}

impl AlignmentGpuReadyReplacement {
    #[must_use]
    pub fn actuals(&self) -> &[AllocationBundleActual] {
        self.staged.actuals()
    }
}

/// A fully validated read candidate owning all settled allocation leases and pins. Applying it to
/// an unchanged renderer is an infallible move-only publication step.
#[must_use = "publish this prepared read replacement"]
pub struct AlignmentGpuPublishableReplacement {
    plan: AlignmentGpuReplacementPlan,
    pages: Vec<GpuAlignmentPage>,
    page_table: Option<Vec<GpuAlignmentPage>>,
    page_table_allocation: Option<AllocationLease>,
    frame: AlignmentGpuFrame,
    frame_buffers: Vec<Arc<GpuAlignmentFrameBuffers>>,
    admission: AlignmentGpuBatchAdmission,
    base_instances: usize,
    text_labels: usize,
    label_anchors_examined: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
    resident_gpu_bytes: usize,
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
    /// Online LRU authority. Keeping this index current when a page is admitted, touched, or
    /// retired removes the former failure-time full page scan and sort from the render thread.
    reclaim_index: BTreeSet<(u64, AlignmentPageKey)>,
    page_table_allocation: Option<AllocationLease>,
    page_table_capacity: usize,
    revision: u64,
    device_generation: u64,
    pending_geometry_uploaded_bytes: usize,
    content_manifest_builds: AtomicU64,
    content_manifest_pages_enumerated: AtomicU64,
    prefetch_authority_keys_inspected: AtomicU64,
    reclaim_authority_keys_inspected: AtomicU64,
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
            reclaim_index: BTreeSet::new(),
            page_table_allocation: None,
            page_table_capacity: 0,
            revision: 0,
            device_generation: 1,
            pending_geometry_uploaded_bytes: 0,
            content_manifest_builds: AtomicU64::new(0),
            content_manifest_pages_enumerated: AtomicU64::new(0),
            prefetch_authority_keys_inspected: AtomicU64::new(0),
            reclaim_authority_keys_inspected: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn operation_counters(&self) -> AlignmentGpuRendererOperationCounters {
        AlignmentGpuRendererOperationCounters {
            content_manifest_builds: self.content_manifest_builds.load(Ordering::Relaxed),
            content_manifest_pages_enumerated: self
                .content_manifest_pages_enumerated
                .load(Ordering::Relaxed),
            prefetch_authority_keys_inspected: self
                .prefetch_authority_keys_inspected
                .load(Ordering::Relaxed),
            reclaim_authority_keys_inspected: self
                .reclaim_authority_keys_inspected
                .load(Ordering::Relaxed),
        }
    }

    fn record_content_manifest_build(&self, pages: usize) {
        self.content_manifest_builds.fetch_add(1, Ordering::Relaxed);
        self.content_manifest_pages_enumerated
            .fetch_add(pages as u64, Ordering::Relaxed);
    }

    /// Plans every retained allocation needed by one foreground replacement without reserving
    /// ledger capacity or creating GPU objects. The returned slots begin at `first_bundle_slot`
    /// so a coordinator can concatenate read and summary demands into one atomic bundle.
    pub fn plan_replacement(
        &self,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &impl AlignmentLedgerPolicyView,
        first_bundle_slot: AllocationBundleSlot,
    ) -> Result<AlignmentGpuReplacementPlan, AlignmentGpuReplacementError> {
        let mandatory_pages = mandatory_alignment_pages(requests)?;
        let prepared_page_keys = prepared_scene_page_key_authority(requests);
        if requests.len() > 1 {
            self.record_content_manifest_build(prepared_page_keys.len());
        }
        let scene_revisions = requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>()
            .into();
        let scene_allocations = requests
            .iter()
            .map(|request| request.scene.metadata_allocation().id())
            .collect::<Vec<_>>()
            .into();
        let mut mandatory_draws = 0_usize;
        let mut maximum_overlays = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        for request in requests {
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request.scene.visible_pages(interval, rows) {
                mandatory_draws = mandatory_draws.saturating_add(1);
                if self.page_index(page.key()).is_some() {
                    page_cache_hits = page_cache_hits.saturating_add(1);
                } else {
                    page_cache_misses = page_cache_misses.saturating_add(1);
                }
                if !request.interaction.is_empty() {
                    maximum_overlays = maximum_overlays.saturating_add(
                        page.primitives()
                            .iter()
                            .filter(|primitive| {
                                let token = request.interaction.token(primitive.handle);
                                !matches!(primitive.component, AlignmentComponent::Label { .. })
                                    && interaction_token_requires_overlay(token)
                            })
                            .count(),
                    );
                }
            }
        }
        u32::try_from(maximum_overlays)
            .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
        if mandatory_draws > 0 {
            u32::try_from((mandatory_draws - 1).saturating_mul(self.camera_stride))
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
        }

        let mut next_slot = first_bundle_slot.get();
        let mut demands = Vec::new();
        let mut missing_pages = Vec::new();
        for page in mandatory_pages
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
        {
            let instances = non_text_primitive_count(page);
            let instance_count = u32::try_from(instances)
                .map_err(|_| AlignmentGpuPageAdmissionError::TooManyPrimitives(instances))?;
            let maximum_bytes = gpu_buffer_capacity::<AlignmentGpuInstance>(instances)?;
            let slot = take_bundle_slot(&mut next_slot)?;
            demands.push(AllocationBundleDemand::new(
                slot,
                AllocationClass::GpuPage,
                maximum_bytes,
            ));
            missing_pages.push(PlannedAlignmentPage {
                page: Arc::clone(page),
                slot,
                instance_count,
                maximum_bytes,
            });
        }

        let required_page_capacity = self.pages.len().saturating_add(missing_pages.len());
        let (page_table_slot, page_table_requested_capacity, page_table_maximum_bytes) =
            if required_page_capacity > self.page_table_capacity {
                let requested = next_capacity(required_page_capacity)?;
                let maximum = requested
                    .checked_mul(2)
                    .and_then(alignment_page_table_retained_bytes)
                    .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
                let slot = take_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    maximum,
                ));
                (Some(slot), requested, maximum)
            } else {
                (None, 0, 0)
            };

        // Complete evidence remains independently pageable. A frame slot only needs enough
        // uniforms for pages one compatible camera can expose, including tile/block boundary
        // crossings. This keeps camera capture stable without sizing every slot as if the entire
        // vertical scene were visible simultaneously.
        let (camera_envelope_draws, overlay_envelope_instances) = requests.iter().fold(
            (0_usize, 0_usize),
            |(draw_total, overlay_total), request| {
                let visible_rows = request.camera.visible_rows();
                // Content authority remains compatible for every camera inside the prepared
                // genomic horizon. Precharge that complete horizontal envelope so an ordinary
                // zoom-out never needs a second retained bundle. Vertical capacity remains
                // bounded by one physical viewport (including block-boundary crossing), not by
                // the complete prepared read stack.
                let camera_pages = request.scene.camera_page_envelope(
                    request.scene.prepared_interval().len(),
                    visible_rows.end.saturating_sub(visible_rows.start),
                );
                let ordinary_overlays = camera_pages
                    .saturating_mul(request.scene.maximum_handle_primitives_per_page())
                    .saturating_mul(ORDINARY_INTERACTION_HANDLES);
                // Unlike hover and selection, a predicate highlight can legitimately match every
                // read in the viewport. Provision for a completely highlighted camera envelope,
                // but never for duplicated geometry across the whole prepared scene.
                let highlight_overlays = (!request.interaction.highlighted().is_empty())
                    .then(|| {
                        camera_pages
                            .saturating_mul(request.scene.maximum_non_label_primitives_per_page())
                    })
                    .unwrap_or(0);
                let current_overlays = request
                    .scene
                    .interaction_overlay_instances(request.interaction);
                (
                    draw_total.saturating_add(camera_pages),
                    overlay_total.saturating_add(
                        ordinary_overlays
                            .max(highlight_overlays)
                            .max(current_overlays),
                    ),
                )
            },
        );
        if camera_envelope_draws > 0 {
            u32::try_from((camera_envelope_draws - 1).saturating_mul(self.camera_stride))
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        u32::try_from(overlay_envelope_instances)
            .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
        let camera_capacity = (camera_envelope_draws > 0)
            .then(|| next_capacity(camera_envelope_draws))
            .transpose()?
            .unwrap_or(0);
        let camera_bytes = camera_capacity.saturating_mul(self.camera_stride);
        let overlay_capacity = (overlay_envelope_instances > 0)
            .then(|| next_capacity(overlay_envelope_instances))
            .transpose()?
            .unwrap_or(0);
        let overlay_bytes = overlay_capacity.saturating_mul(size_of::<AlignmentGpuInstance>());
        let maximum_frame_bytes = alignment_gpu_frame_bytes(
            requests.len(),
            prepared_page_keys.len(),
            requests.len(),
            requests.len(),
            camera_envelope_draws,
            camera_envelope_draws.saturating_add(4),
        );
        let mut frame_slots = Vec::with_capacity(ALIGNMENT_CAMERA_FRAME_SLOT_COUNT);
        for _ in 0..ALIGNMENT_CAMERA_FRAME_SLOT_COUNT {
            let camera_slot = if camera_bytes > 0 {
                let slot = take_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    camera_bytes,
                ));
                Some(slot)
            } else {
                None
            };
            let overlay_slot = if overlay_bytes > 0 {
                let slot = take_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    overlay_bytes,
                ));
                Some(slot)
            } else {
                None
            };
            let frame_slot = take_bundle_slot(&mut next_slot)?;
            demands.push(AllocationBundleDemand::new(
                frame_slot,
                AllocationClass::TransitionFrame,
                maximum_frame_bytes,
            ));
            frame_slots.push(PlannedAlignmentFrameSlot {
                camera_slot,
                overlay_slot,
                frame_slot,
            });
        }
        let frame_slots: [PlannedAlignmentFrameSlot; ALIGNMENT_CAMERA_FRAME_SLOT_COUNT] =
            frame_slots
                .try_into()
                .expect("camera frame slot count is a compile-time constant");
        let next_bundle_slot = AllocationBundleSlot::new(next_slot);

        Ok(AlignmentGpuReplacementPlan {
            policy_revision: ledger.policy_revision(),
            first_bundle_slot,
            next_bundle_slot,
            renderer_revision: self.revision,
            device_generation: self.device_generation,
            scene_revisions,
            scene_allocations,
            mandatory_pages,
            prepared_page_keys,
            missing_pages,
            demands,
            page_table_slot,
            page_table_requested_capacity,
            page_table_maximum_bytes,
            camera_capacity,
            camera_bytes,
            overlay_capacity,
            overlay_bytes,
            frame_slots,
            maximum_frame_bytes,
            request_count: requests.len(),
            mandatory_draws,
            maximum_overlays,
            page_cache_hits,
            page_cache_misses,
        })
    }

    /// Preflights exact ownership, then consumes the caller's prior read frame and detaches only
    /// renderer-cache lease objects for which the resulting token owns every live ledger
    /// reference. Every fallible ledger check completes before the caller frame or renderer cache
    /// is mutated. Every resident page in the new candidate's complete prepared authority remains
    /// resident and deliberately receives no retirement credit, including prefetched pages that
    /// are outside the current visible camera.
    pub fn detach_replacement_retirement(
        &mut self,
        ledger: &AllocationLedger,
        plan: &AlignmentGpuReplacementPlan,
        prior_frame: &mut Option<AlignmentGpuFrame>,
    ) -> Result<AlignmentGpuRetirement, AlignmentGpuReplacementError> {
        self.validate_replacement_plan(plan)?;
        if prior_frame
            .as_ref()
            .is_some_and(|frame| frame.device_generation != self.device_generation)
        {
            return Err(AlignmentGpuReplacementError::PriorFrameDeviceGeneration);
        }
        let mut prior_pin_counts = BTreeMap::<AllocationId, usize>::new();
        if let Some(frame) = prior_frame.as_ref() {
            for pin in &frame._pins {
                *prior_pin_counts.entry(pin.id()).or_default() += 1;
            }
            for lease in &frame._page_leases {
                *prior_pin_counts.entry(lease.pin.id()).or_default() += 1;
            }
        }
        let prepared_keys = plan
            .prepared_page_keys
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut claimable = BTreeSet::<AllocationId>::new();

        let mut all_pages_retirable = true;
        for page in &self.pages {
            if prepared_keys.contains(&page.key)
                || page
                    .allocation
                    .as_ref()
                    .map(|allocation| exact_retirement_ownership(allocation, &prior_pin_counts))
                    .transpose()?
                    == Some(false)
            {
                all_pages_retirable = false;
                break;
            }
        }
        let can_retire_table = plan.page_table_slot.is_some()
            && all_pages_retirable
            && self
                .page_table_allocation
                .as_ref()
                .map(|allocation| exact_retirement_ownership(allocation, &prior_pin_counts))
                .transpose()?
                .unwrap_or(false);
        let mut retired_page_indices = Vec::new();
        if can_retire_table {
            for page in &self.pages {
                if let Some(allocation) = &page.allocation {
                    claimable.insert(allocation.id());
                }
            }
            claimable.insert(
                self.page_table_allocation
                    .as_ref()
                    .expect("retirable page table was proven present")
                    .id(),
            );
        } else {
            for (index, page) in self.pages.iter().enumerate() {
                if prepared_keys.contains(&page.key) {
                    continue;
                }
                let Some(allocation) = &page.allocation else {
                    continue;
                };
                if exact_retirement_ownership(allocation, &prior_pin_counts)? {
                    claimable.insert(allocation.id());
                    retired_page_indices.push(index);
                }
            }
        }

        let mut retired_frame_buffer_indices = Vec::new();
        for (index, buffers) in self.frame_buffers.iter().enumerate() {
            let frame_references = prior_frame
                .iter()
                .filter(|frame| Arc::ptr_eq(&frame.buffers, buffers))
                .count();
            if Arc::strong_count(buffers) != frame_references.saturating_add(1) {
                continue;
            }
            let allocations = [
                buffers.camera_allocation.as_ref(),
                buffers.overlay_allocation.as_ref(),
                Some(&buffers.frame_allocation),
            ];
            let mut exact = true;
            for allocation in allocations.iter().flatten() {
                if !exact_retirement_ownership(allocation, &prior_pin_counts)? {
                    exact = false;
                    break;
                }
            }
            if !exact {
                continue;
            }
            for allocation in allocations.into_iter().flatten() {
                claimable.insert(allocation.id());
            }
            retired_frame_buffer_indices.push(index);
        }

        let mut builder = ledger.retirement_claim_builder();
        if let Some(frame) = prior_frame.as_ref() {
            for pin in &frame._pins {
                if claimable.contains(&pin.id()) {
                    builder.include_pin(pin)?;
                }
            }
            for lease in &frame._page_leases {
                if claimable.contains(&lease.pin.id()) {
                    builder.include_pin(&lease.pin)?;
                }
            }
        }
        for (index, page) in self.pages.iter().enumerate() {
            if can_retire_table || retired_page_indices.binary_search(&index).is_ok() {
                if let Some(allocation) = &page.allocation {
                    builder.include_lease(allocation)?;
                }
            }
        }
        if can_retire_table {
            builder.include_lease(
                self.page_table_allocation
                    .as_ref()
                    .expect("retirable page table was proven present"),
            )?;
        }
        for index in &retired_frame_buffer_indices {
            let buffers = &self.frame_buffers[*index];
            if let Some(allocation) = &buffers.camera_allocation {
                builder.include_lease(allocation)?;
            }
            if let Some(allocation) = &buffers.overlay_allocation {
                builder.include_lease(allocation)?;
            }
            builder.include_lease(&buffers.frame_allocation)?;
        }
        let claim_set = builder.finish()?;

        // From this point onward every operation is an infallible ownership move. No error can
        // strand the caller without its prior frame or leave a partially detached renderer.
        let prior_frame = prior_frame.take();
        let retired_pages = if can_retire_table {
            std::mem::take(&mut self.pages)
        } else {
            let mut retired = Vec::with_capacity(retired_page_indices.len());
            for index in retired_page_indices.into_iter().rev() {
                retired.push(self.pages.remove(index));
            }
            retired.sort_unstable_by_key(|page| page.key);
            retired
        };
        let retired_page_table = if can_retire_table {
            let capacity = self.page_table_capacity;
            self.page_table_capacity = 0;
            Some((
                self.page_table_allocation
                    .take()
                    .expect("retirable page table was proven present"),
                capacity,
            ))
        } else {
            None
        };
        let mut retired_frame_buffers = Vec::with_capacity(retired_frame_buffer_indices.len());
        for index in retired_frame_buffer_indices.into_iter().rev() {
            retired_frame_buffers.push(RetiredAlignmentFrameBuffer {
                index,
                buffers: self.frame_buffers.remove(index),
            });
        }
        retired_frame_buffers.sort_unstable_by_key(|retired| retired.index);
        self.rebuild_reclaim_index();
        Ok(AlignmentGpuRetirement {
            claim_set,
            prior_frame,
            retired_pages,
            retired_page_table,
            retired_frame_buffers,
            renderer_revision: self.revision,
            device_generation: self.device_generation,
        })
    }

    /// Restores the exact cache ownership detached before a refused or failed bundle. Callers
    /// must first drop the bundle reservation so its ledger claim no longer covers this token.
    pub fn restore_replacement_retirement(
        &mut self,
        mut retirement: AlignmentGpuRetirement,
    ) -> Result<Option<AlignmentGpuFrame>, AlignmentGpuReplacementError> {
        if self.revision != retirement.renderer_revision
            || self.device_generation != retirement.device_generation
        {
            return Err(AlignmentGpuReplacementError::StaleRenderer {
                planned_revision: retirement.renderer_revision,
                current_revision: self.revision,
            });
        }
        if retirement.retired_page_table.is_some()
            && (self.page_table_allocation.is_some() || !self.pages.is_empty())
        {
            return Err(AlignmentGpuReplacementError::RetirementRestoreConflict);
        }
        if let Some((allocation, capacity)) = retirement.retired_page_table.take() {
            self.pages = std::mem::take(&mut retirement.retired_pages);
            if self.pages.capacity() != capacity {
                return Err(AlignmentGpuReplacementError::RetirementRestoreConflict);
            }
            self.page_table_allocation = Some(allocation);
            self.page_table_capacity = capacity;
        } else {
            self.pages.append(&mut retirement.retired_pages);
            self.pages.sort_unstable_by_key(|page| page.key);
        }
        self.rebuild_reclaim_index();
        for retired in retirement.retired_frame_buffers.drain(..) {
            self.frame_buffers
                .insert(retired.index.min(self.frame_buffers.len()), retired.buffers);
        }
        Ok(retirement.prior_frame.take())
    }

    fn validate_replacement_plan(
        &self,
        plan: &AlignmentGpuReplacementPlan,
    ) -> Result<(), AlignmentGpuReplacementError> {
        if plan.device_generation != self.device_generation {
            return Err(AlignmentGpuReplacementError::DeviceGeneration {
                planned: plan.device_generation,
                current: self.device_generation,
            });
        }
        if plan.renderer_revision != self.revision {
            return Err(AlignmentGpuReplacementError::StaleRenderer {
                planned_revision: plan.renderer_revision,
                current_revision: self.revision,
            });
        }
        Ok(())
    }

    /// Builds every candidate GPU/CPU object under a previously accepted shared bundle while
    /// leaving renderer-visible pages, table, ring cache, revision, and telemetry unchanged.
    /// This path intentionally performs no retained `AllocationLedger::reserve` call.
    pub fn stage_replacement(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        plan: AlignmentGpuReplacementPlan,
        bundle: &AllocationBundleReservation,
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuStagedReplacement, AlignmentGpuReplacementError> {
        self.validate_replacement_plan(&plan)?;
        if bundle.policy_revision() != plan.policy_revision {
            return Err(AlignmentGpuReplacementError::BundlePolicyRevision {
                planned: plan.policy_revision,
                reserved: bundle.policy_revision(),
            });
        }
        let mandatory_pages = mandatory_alignment_pages(requests)?;
        if requests.len() != plan.request_count
            || requests
                .iter()
                .map(|request| request.scene.revision())
                .ne(plan.scene_revisions.iter().copied())
            || requests
                .iter()
                .map(|request| request.scene.metadata_allocation().id())
                .ne(plan.scene_allocations.iter().copied())
            || mandatory_pages
                .iter()
                .map(|page| page.key())
                .ne(plan.mandatory_pages.iter().map(|page| page.key()))
        {
            return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
        }
        for page in &plan.missing_pages {
            if self.page_index(page.page.key()).is_some() {
                return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
            }
            validate_device_buffer_size(device, page.maximum_bytes)?;
        }
        validate_device_buffer_size(device, plan.camera_bytes)?;
        validate_device_buffer_size(device, plan.overlay_bytes)?;

        let missing_instances = plan.missing_pages.iter().fold(0_usize, |total, page| {
            total.saturating_add(page.instance_count as usize)
        });
        let uniform_bytes = plan.mandatory_draws.saturating_mul(self.camera_stride);
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            uniform_bytes
                .saturating_add(plan.overlay_bytes.saturating_mul(2))
                .saturating_add(missing_instances.saturating_mul(size_of::<AlignmentGpuInstance>()))
                .saturating_add(
                    plan.missing_pages
                        .len()
                        .saturating_mul(size_of::<PendingGpuPageAdmission>()),
                )
                .saturating_add(
                    plan.mandatory_draws
                        .saturating_mul(size_of::<PreparedAlignmentDraw>()),
                )
                .saturating_add(
                    plan.mandatory_draws
                        .saturating_mul(size_of::<PendingAlignmentDraw>()),
                ),
            ReservationKind::Transition,
        )?;

        let mut staged_pages = Vec::with_capacity(plan.missing_pages.len());
        let mut geometry_uploaded_bytes = 0_usize;
        for planned in &plan.missing_pages {
            let packed = pack_page(&planned.page, None)?;
            debug_assert_eq!(packed.instances.len(), planned.instance_count as usize);
            let buffer = (planned.maximum_bytes > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged immutable alignment GPU page"),
                    size: planned.maximum_bytes as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            if let Some(buffer) = &buffer {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&packed.instances));
            }
            geometry_uploaded_bytes = geometry_uploaded_bytes.saturating_add(
                packed
                    .instances
                    .len()
                    .saturating_mul(size_of::<AlignmentGpuInstance>()),
            );
            staged_pages.push(StagedAlignmentPage {
                key: planned.page.key(),
                slot: (planned.maximum_bytes > 0).then_some(planned.slot),
                buffer,
                instance_count: planned.instance_count,
                charged_capacity: planned.maximum_bytes,
            });
        }

        let page_table = if plan.page_table_slot.is_some() {
            let candidate = Vec::with_capacity(plan.page_table_requested_capacity);
            if candidate.capacity() < self.pages.len().saturating_add(plan.missing_pages.len()) {
                return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                    required: self.pages.len().saturating_add(plan.missing_pages.len()),
                    actual: candidate.capacity(),
                }
                .into());
            }
            let actual_bytes = alignment_page_table_retained_bytes(candidate.capacity())
                .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
            if actual_bytes > plan.page_table_maximum_bytes {
                return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                    required: plan.page_table_maximum_bytes,
                    actual: actual_bytes,
                }
                .into());
            }
            Some(candidate)
        } else {
            None
        };

        let mut staged_buffers = Vec::with_capacity(ALIGNMENT_CAMERA_FRAME_SLOT_COUNT);
        for slot in plan.frame_slots {
            let camera_buffer = (plan.camera_bytes > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged alignment camera frame slot"),
                    size: plan.camera_bytes as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            let camera_bind_group = camera_buffer
                .as_ref()
                .map(|buffer| create_camera_bind_group(device, &self.camera_layout, buffer));
            let overlay_buffer = (plan.overlay_bytes > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV staged alignment overlay frame slot"),
                    size: plan.overlay_bytes as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
            staged_buffers.push(StagedAlignmentFrameBuffers {
                camera_buffer,
                camera_bind_group,
                camera_slot: slot.camera_slot,
                camera_capacity: plan.camera_capacity,
                overlay_buffer,
                overlay_slot: slot.overlay_slot,
                overlay_capacity: plan.overlay_capacity,
                frame_slot: slot.frame_slot,
            });
        }

        let mut label_frames = Vec::with_capacity(requests.len());
        for request in requests {
            label_frames.push(CpuAlignmentProjector::project_labels_with_interaction(
                request.scene,
                request.camera_sequence,
                request.camera,
                request.interaction,
            )?);
        }
        let mut page_requests = Vec::with_capacity(plan.mandatory_draws);
        let mut overlay_instances = Vec::with_capacity(plan.maximum_overlays);
        let scene_revisions = plan.scene_revisions.to_vec();
        let scene_allocations = plan.scene_allocations.to_vec();
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for request in requests {
            camera_sequences.push(request.camera_sequence);
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request.scene.visible_pages(interval, rows) {
                let instance_count = self
                    .page(page.key())
                    .map(|resident| resident.instance_count)
                    .or_else(|| {
                        staged_pages
                            .iter()
                            .find(|candidate| candidate.key == page.key())
                            .map(|candidate| candidate.instance_count)
                    })
                    .ok_or(AlignmentGpuReplacementError::RequestChangedAfterPlan)?;
                let overlay_start = u32::try_from(overlay_instances.len())
                    .expect("overlay address space was proven by the replacement plan");
                if !request.interaction.is_empty() {
                    for primitive in page.primitives() {
                        let token = request.interaction.token(primitive.handle);
                        if !interaction_token_requires_overlay(token) {
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
                    .expect("overlay address space was proven by the replacement plan");
                page_requests.push(PendingAlignmentDraw {
                    page: page.key(),
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: page.interval().start(),
                    page_row_start: page.row_range().start,
                    overlay_range: overlay_start..overlay_end,
                    base_instance_count: instance_count,
                });
            }
        }
        if page_requests.len() != plan.mandatory_draws
            || overlay_instances.len() > plan.maximum_overlays
        {
            return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
        }

        let mut uniforms = vec![0_u8; uniform_bytes];
        let mut draws = Vec::with_capacity(plan.mandatory_draws);
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
            draws.push(PreparedAlignmentDraw {
                page: request.page,
                uniform_offset: u32::try_from(offset)
                    .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?,
                scissor: request.scissor,
                overlay_range: request.overlay_range.clone(),
            });
        }
        let initial_buffers = staged_buffers
            .first()
            .expect("replacement provisions at least one camera frame slot");
        if let Some(buffer) = &initial_buffers.camera_buffer
            && !uniforms.is_empty()
        {
            queue.write_buffer(buffer, 0, &uniforms);
        }
        if let Some(buffer) = &initial_buffers.overlay_buffer
            && !overlay_instances.is_empty()
        {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&overlay_instances));
        }

        let next_revision = plan.renderer_revision.saturating_add(1);
        let page_pin_count = page_requests
            .iter()
            .filter(|request| {
                self.page(request.page)
                    .is_some_and(|page| page.allocation.is_some())
                    || staged_pages
                        .iter()
                        .any(|page| page.key == request.page && page.charged_capacity > 0)
            })
            .count();
        let actual_pin_count = page_pin_count
            .saturating_add(usize::from(plan.frame_slots[0].camera_slot.is_some()))
            .saturating_add(usize::from(plan.frame_slots[0].overlay_slot.is_some()))
            .saturating_add(usize::from(
                plan.page_table_slot.is_some() || self.page_table_allocation.is_some(),
            ))
            .saturating_add(1);
        let retained_frame_bytes = alignment_gpu_frame_bytes(
            scene_revisions.len(),
            plan.prepared_page_keys.len(),
            camera_sequences.len(),
            label_frames.len(),
            draws.len(),
            actual_pin_count,
        );
        if retained_frame_bytes > plan.maximum_frame_bytes {
            return Err(AlignmentGpuReplacementError::FrameCapacityExceeded {
                actual: retained_frame_bytes,
                maximum: plan.maximum_frame_bytes,
            });
        }
        let mut actuals = staged_pages
            .iter()
            .zip(&plan.missing_pages)
            .map(|(page, planned)| {
                AllocationBundleActual::new(
                    planned.slot,
                    page.charged_capacity,
                    publication_revision,
                )
            })
            .collect::<Vec<_>>();
        if let (Some(slot), Some(table)) = (plan.page_table_slot, page_table.as_ref()) {
            actuals.push(AllocationBundleActual::new(
                slot,
                alignment_page_table_retained_bytes(table.capacity()).unwrap_or(usize::MAX),
                next_revision,
            ));
        }
        for slot in plan.frame_slots {
            if let Some(camera_slot) = slot.camera_slot {
                actuals.push(AllocationBundleActual::new(
                    camera_slot,
                    plan.camera_bytes,
                    next_revision,
                ));
            }
            if let Some(overlay_slot) = slot.overlay_slot {
                actuals.push(AllocationBundleActual::new(
                    overlay_slot,
                    plan.overlay_bytes,
                    next_revision,
                ));
            }
            // Charge the full reusable metadata envelope for each physical slot. Captures later
            // consume that capacity without opening a new retained ledger reservation.
            actuals.push(AllocationBundleActual::new(
                slot.frame_slot,
                plan.maximum_frame_bytes,
                next_revision,
            ));
        }
        actuals.sort_unstable_by_key(|actual| actual.slot);
        let admission = AlignmentGpuBatchAdmission {
            requested_pages: plan.mandatory_pages.len(),
            admitted_pages: plan.missing_pages.len(),
            already_resident_pages: plan
                .mandatory_pages
                .len()
                .saturating_sub(plan.missing_pages.len()),
            instances: missing_instances,
            uploaded_bytes: geometry_uploaded_bytes,
        };
        let base_instances = page_requests.iter().fold(0_usize, |total, request| {
            total.saturating_add(request.base_instance_count as usize)
        });
        let text_labels = label_frames.iter().map(|labels| labels.labels.len()).sum();
        Ok(AlignmentGpuStagedReplacement {
            pages: staged_pages,
            page_table,
            buffers: staged_buffers,
            scene_revisions,
            scene_allocations,
            camera_sequences,
            label_frames,
            draws,
            actuals,
            admission,
            base_instances,
            text_labels,
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes,
            plan,
        })
    }

    /// Checks every renderer/cache invariant and acquires pins for reused allocations before the
    /// bundle is committed. A coordinator preflights all renderers before retirement settlement.
    pub fn preflight_replacement_publication(
        &self,
        staged: AlignmentGpuStagedReplacement,
    ) -> Result<AlignmentGpuReadyReplacement, AlignmentGpuReplacementError> {
        self.validate_replacement_plan(&staged.plan)?;
        let staged_keys = staged
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<BTreeSet<_>>();
        if staged_keys.len() != staged.pages.len()
            || staged_keys.iter().any(|key| self.page(*key).is_some())
        {
            return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
        }
        let mut existing_pins = Vec::with_capacity(staged.draws.len().saturating_add(1));
        for draw in &staged.draws {
            if let Some(page) = self.page(draw.page) {
                if let Some(allocation) = &page.allocation {
                    existing_pins.push(allocation.pin()?);
                }
            } else if !staged_keys.contains(&draw.page) {
                return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
            }
        }
        if staged.page_table.is_none()
            && let Some(allocation) = &self.page_table_allocation
        {
            existing_pins.push(allocation.pin()?);
        }
        let replaced_table_bytes = if staged.page_table.is_some() {
            self.page_table_allocation
                .as_ref()
                .map(|allocation| allocation.snapshot().map(|record| record.charged_capacity))
                .transpose()?
                .unwrap_or(0)
        } else {
            0
        };
        let candidate_gpu_bytes = staged
            .actuals
            .iter()
            .filter(|actual| {
                !staged
                    .plan
                    .frame_slots
                    .iter()
                    .any(|slot| slot.frame_slot == actual.slot)
            })
            .fold(0_usize, |total, actual| {
                total.saturating_add(actual.charged_capacity)
            });
        let resident_gpu_bytes = self
            .resident_retained_bytes()?
            .saturating_sub(replaced_table_bytes)
            .saturating_add(candidate_gpu_bytes);
        Ok(AlignmentGpuReadyReplacement {
            staged,
            existing_pins,
            resident_gpu_bytes,
        })
    }

    /// Binds the exact leases minted by the already-validated bundle commit without changing
    /// renderer-visible state. Missing/mismatched slots or pin failure are invariant violations:
    /// every recoverable check ran in preflight before retirement, so this post-settlement suffix
    /// is constructionally infallible.
    pub fn bind_replacement_leases(
        &self,
        ready: AlignmentGpuReadyReplacement,
        settled_leases: &mut Vec<AllocationBundleLease>,
    ) -> AlignmentGpuPublishableReplacement {
        assert!(
            self.validate_replacement_plan(&ready.staged.plan).is_ok(),
            "alignment renderer changed after replacement preflight"
        );
        let AlignmentGpuReadyReplacement {
            staged,
            mut existing_pins,
            resident_gpu_bytes,
        } = ready;
        let mut allocations = take_replacement_leases(&staged.actuals, settled_leases)
            .expect("bundle settlement must return every validated alignment slot exactly once");
        let next_revision = staged.plan.renderer_revision.saturating_add(1);
        let mut pages = Vec::with_capacity(staged.pages.len());
        for page in staged.pages {
            let allocation = page.slot.map(|slot| {
                allocations
                    .remove(&slot)
                    .expect("settled alignment page slot was prevalidated")
            });
            pages.push(GpuAlignmentPage {
                key: page.key,
                buffer: page.buffer,
                instance_count: page.instance_count,
                allocation,
                last_used_revision: next_revision,
            });
        }
        let page_table_allocation = staged.plan.page_table_slot.map(|slot| {
            allocations
                .remove(&slot)
                .expect("settled alignment table slot was prevalidated")
        });
        let mut frame_buffers = Vec::with_capacity(staged.buffers.len());
        for buffers in staged.buffers {
            let camera_allocation = buffers.camera_slot.map(|slot| {
                allocations
                    .remove(&slot)
                    .expect("settled alignment camera slot was prevalidated")
            });
            let overlay_allocation = buffers.overlay_slot.map(|slot| {
                allocations
                    .remove(&slot)
                    .expect("settled alignment overlay slot was prevalidated")
            });
            let frame_allocation = allocations
                .remove(&buffers.frame_slot)
                .expect("settled alignment frame slot was prevalidated");
            frame_buffers.push(Arc::new(GpuAlignmentFrameBuffers {
                camera_buffer: buffers.camera_buffer,
                camera_bind_group: buffers.camera_bind_group,
                camera_allocation,
                camera_capacity: buffers.camera_capacity,
                overlay_buffer: buffers.overlay_buffer,
                overlay_allocation,
                overlay_capacity: buffers.overlay_capacity,
                frame_allocation,
                frame_capacity: staged.plan.maximum_frame_bytes,
                capture_ring: true,
            }));
        }
        assert!(
            allocations.is_empty(),
            "all settled alignment slots must bind to a staged object"
        );
        let initial_frame_buffers = Arc::clone(
            frame_buffers
                .first()
                .expect("replacement provisions at least one camera frame slot"),
        );
        existing_pins.reserve(4);
        for draw in &staged.draws {
            if self.page(draw.page).is_none()
                && let Some(allocation) = pages
                    .iter()
                    .find(|page| page.key == draw.page)
                    .and_then(|page| page.allocation.as_ref())
            {
                existing_pins.push(
                    allocation
                        .pin()
                        .expect("newly settled alignment page must remain pinnable"),
                );
            }
        }
        if let Some(allocation) = &initial_frame_buffers.camera_allocation {
            existing_pins.push(
                allocation
                    .pin()
                    .expect("newly settled alignment camera ring must remain pinnable"),
            );
        }
        if let Some(allocation) = &initial_frame_buffers.overlay_allocation {
            existing_pins.push(
                allocation
                    .pin()
                    .expect("newly settled alignment overlay ring must remain pinnable"),
            );
        }
        if let Some(allocation) = &page_table_allocation {
            existing_pins.push(
                allocation
                    .pin()
                    .expect("newly settled alignment table must remain pinnable"),
            );
        }
        existing_pins.push(
            initial_frame_buffers
                .frame_allocation
                .pin()
                .expect("newly settled alignment frame must remain pinnable"),
        );
        let content_authority = AlignmentGpuContentAuthority::new(
            self.device_generation,
            staged.scene_revisions.clone().into(),
            staged.scene_allocations.into(),
            Arc::clone(&staged.plan.prepared_page_keys),
        );
        let label_anchors_examined = staged
            .label_frames
            .iter()
            .map(|frame| frame.anchors_examined)
            .sum();
        let frame = AlignmentGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: staged.scene_revisions.into(),
            camera_sequences: staged.camera_sequences.into(),
            content_authority,
            label_frames: staged.label_frames.into(),
            draws: staged.draws.into(),
            buffers: initial_frame_buffers,
            _page_leases: Vec::new(),
            _pins: existing_pins,
        };
        AlignmentGpuPublishableReplacement {
            plan: staged.plan,
            pages,
            page_table: staged.page_table,
            page_table_allocation,
            frame,
            frame_buffers,
            admission: staged.admission,
            base_instances: staged.base_instances,
            text_labels: staged.text_labels,
            label_anchors_examined,
            camera_uniform_bytes: staged.camera_uniform_bytes,
            geometry_uploaded_bytes: staged.geometry_uploaded_bytes,
            resident_gpu_bytes,
        }
    }

    /// Applies a fully bound candidate through an infallible move-only cache mutation. The ready
    /// type is valid while the renderer remains at the revision/device generation preflighted
    /// before commit.
    pub fn publish_replacement(
        &mut self,
        mut publishable: AlignmentGpuPublishableReplacement,
    ) -> (
        AlignmentGpuFrame,
        AlignmentGpuPrepareStats,
        AlignmentGpuBatchAdmission,
    ) {
        assert_eq!(
            self.revision, publishable.plan.renderer_revision,
            "alignment renderer changed after replacement preflight"
        );
        assert_eq!(
            self.device_generation, publishable.plan.device_generation,
            "alignment device generation changed after replacement preflight"
        );
        let next_revision = publishable.plan.renderer_revision.saturating_add(1);
        for page in &publishable.plan.mandatory_pages {
            if let Some(index) = self.page_index(page.key()) {
                self.pages[index].last_used_revision = next_revision;
            }
        }
        if let Some(mut table) = publishable.page_table.take() {
            table.extend(std::mem::take(&mut self.pages));
            table.append(&mut publishable.pages);
            table.sort_unstable_by_key(|page| page.key);
            self.pages = table;
            self.page_table_capacity = self.pages.capacity();
            self.page_table_allocation = publishable.page_table_allocation.take();
        } else {
            self.pages.append(&mut publishable.pages);
            self.pages.sort_unstable_by_key(|page| page.key);
        }
        self.rebuild_reclaim_index();
        self.frame_buffers.append(&mut publishable.frame_buffers);
        self.revision = next_revision;
        let geometry_uploaded_bytes = std::mem::take(&mut self.pending_geometry_uploaded_bytes)
            .saturating_add(publishable.geometry_uploaded_bytes);
        let stats = AlignmentGpuPrepareStats {
            scenes: publishable.plan.request_count,
            mandatory_pages: publishable.plan.mandatory_draws,
            base_instances: publishable.base_instances,
            overlay_instances: publishable.plan.maximum_overlays,
            text_labels: publishable.text_labels,
            label_anchors_examined: publishable.label_anchors_examined,
            page_pin_acquisitions: publishable.plan.mandatory_draws,
            camera_uniform_bytes: publishable.camera_uniform_bytes,
            geometry_uploaded_bytes,
            page_cache_hits: publishable.plan.page_cache_hits,
            page_cache_misses: publishable.plan.page_cache_misses,
            resident_pages: self.pages.len(),
            resident_gpu_bytes: publishable.resident_gpu_bytes,
        };
        (publishable.frame, stats, publishable.admission)
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
        self.admit_pages_protecting(
            device,
            queue,
            pages,
            pages,
            ledger,
            publication_revision,
            ReservationKind::Normal,
        )
    }

    /// Admits a complete visible candidate while the prior frame remains pinned. Transition
    /// reserve is available only for this atomic old-to-new foreground swap; speculative
    /// prefetch continues to use the normal retained budget.
    pub fn admit_mandatory_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[Arc<AlignmentSemanticPage>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuBatchAdmission, AlignmentGpuPageAdmissionError> {
        self.admit_pages_protecting(
            device,
            queue,
            pages,
            pages,
            ledger,
            publication_revision,
            ReservationKind::Transition,
        )
    }

    fn admit_pages_protecting(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[Arc<AlignmentSemanticPage>],
        protected: &[Arc<AlignmentSemanticPage>],
        ledger: &AllocationLedger,
        publication_revision: u64,
        reservation_kind: ReservationKind,
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
            let revision = self.revision;
            for page in pages {
                if let Some(index) = self.page_index(page.key()) {
                    self.touch_page_at(index, revision);
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

        let snapshot = ledger.progress()?;
        let reservation_limit = match reservation_kind {
            ReservationKind::Normal => snapshot.budget,
            ReservationKind::Transition => {
                snapshot.budget.saturating_add(snapshot.transition_reserve)
            }
        };
        self.evict_unpinned_pages_for_excluding(
            ledger,
            retained_bytes,
            reservation_limit,
            protected,
        )?;
        let mut retained =
            ledger.reserve(AllocationClass::GpuPage, retained_bytes, reservation_kind)?;
        // Hold the page bytes while growing the table so the complete page+table transition is
        // proven before either allocation becomes resident.
        self.ensure_page_table_capacity(
            ledger,
            self.pages.len().saturating_add(missing_pages),
            reservation_kind,
        )?;
        let scratch_bytes = maximum_instances
            .saturating_mul(size_of::<AlignmentGpuInstance>())
            .saturating_mul(2)
            .saturating_add(missing_pages.saturating_mul(size_of::<PendingGpuPageAdmission>()));
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            scratch_bytes,
            reservation_kind,
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
                allocation: Some(allocation),
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
        self.rebuild_reclaim_index();
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
        for request in requests {
            validate_request(request)?;
        }
        let page_manifest = prepared_scene_pages(requests)
            .into_iter()
            .map(|page| page.key())
            .collect::<Vec<_>>();
        self.record_content_manifest_build(page_manifest.len());
        let authority = AlignmentGpuContentAuthority::new(
            self.device_generation,
            requests
                .iter()
                .map(|request| request.scene.revision())
                .collect::<Vec<_>>()
                .into(),
            requests
                .iter()
                .map(|request| request.scene.metadata_allocation().id())
                .collect::<Vec<_>>()
                .into(),
            page_manifest.into(),
        );
        self.capture_resident_camera_frame(device, queue, requests, ledger, &authority, true)
    }

    /// Builds only a camera-dependent frame over a previously minted immutable content
    /// authority. This function cannot enumerate the prepared scene, admit/retire pages, create a
    /// content identity, or reserve a replacement bundle. The `allow_slot_growth` flag is true
    /// only for the initial content publication; production camera capture always supplies false.
    fn capture_resident_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        content_authority: &AlignmentGpuContentAuthority,
        allow_slot_growth: bool,
    ) -> Result<(AlignmentGpuFrame, AlignmentGpuPrepareStats), AlignmentGpuPrepareError> {
        // First pass is allocation-free. It proves the complete mandatory set is resident and
        // derives conservative capacities before any frame candidate memory or GPU buffer grows.
        let visible_pages = requests
            .iter()
            .map(|request| {
                validate_request(request)?;
                Ok(request
                    .scene
                    .visible_pages(request.camera.interval(), request.camera.visible_rows()))
            })
            .collect::<Result<Vec<_>, AlignmentGpuPrepareError>>()?;
        let mut mandatory_pages = 0_usize;
        let mut maximum_overlays = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        for (request, pages) in requests.iter().zip(&visible_pages) {
            for page in pages {
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
                                    && interaction_token_requires_overlay(token)
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
        let scratch_bytes = uniform_bytes
            .saturating_add(overlay_bytes.saturating_mul(2))
            .saturating_add(mandatory_pages.saturating_mul(size_of::<PreparedAlignmentDraw>()))
            .saturating_add(mandatory_pages.saturating_mul(size_of::<PendingAlignmentDraw>()))
            .saturating_add(
                requests
                    .len()
                    .saturating_mul(size_of::<SceneRevision>().saturating_add(size_of::<u64>())),
            );
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            scratch_bytes,
            ReservationKind::Transition,
        )?;
        let prepared_page_count = content_authority.page_manifest.len();
        let maximum_frame_bytes = alignment_gpu_frame_bytes(
            requests.len(),
            prepared_page_count,
            requests.len(),
            requests.len(),
            mandatory_pages,
            mandatory_pages.saturating_add(4),
        );
        let mut label_frames = Vec::with_capacity(requests.len());
        for request in requests {
            label_frames.push(CpuAlignmentProjector::project_labels_with_interaction(
                request.scene,
                request.camera_sequence,
                request.camera,
                request.interaction,
            )?);
        }
        let frame_buffers = if allow_slot_growth {
            self.acquire_frame_buffers(
                device,
                ledger,
                mandatory_pages,
                maximum_overlays,
                maximum_frame_bytes,
            )?
        } else {
            self.acquire_existing_frame_buffers(
                mandatory_pages,
                maximum_overlays,
                maximum_frame_bytes,
            )?
        };

        let mut page_requests = Vec::with_capacity(mandatory_pages);
        let mut overlay_instances = Vec::with_capacity(maximum_overlays);
        let mut scene_revisions = Vec::with_capacity(requests.len());
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for (request, pages) in requests.iter().zip(&visible_pages) {
            scene_revisions.push(request.scene.revision());
            camera_sequences.push(request.camera_sequence);
            for page in pages {
                let resident = self
                    .page(page.key())
                    .expect("mandatory page presence was proven in the allocation-free pass");
                let overlay_start = u32::try_from(overlay_instances.len())
                    .expect("overlay address space was proven before allocation");
                if !request.interaction.is_empty() {
                    for primitive in page.primitives() {
                        let token = request.interaction.token(primitive.handle);
                        if !interaction_token_requires_overlay(token) {
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
        let mut page_leases =
            BTreeMap::<AlignmentPageKey, Arc<AlignmentGpuResidentPageLease>>::new();
        let mut page_pin_acquisitions = 0_usize;
        let mut pins = Vec::with_capacity(4);
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
            let page_index = self
                .page_index(request.page)
                .expect("mandatory page presence was checked before frame mutation");
            self.touch_page_at(page_index, next_revision);
            let page = &self.pages[page_index];
            if let Some(allocation) = &page.allocation
                && !page_leases.contains_key(&request.page)
            {
                let (lease, acquired) =
                    content_authority.leases.acquire(request.page, allocation)?;
                debug_assert_eq!(lease.key, request.page);
                page_pin_acquisitions = page_pin_acquisitions.saturating_add(usize::from(acquired));
                page_leases.insert(request.page, lease);
            }
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
            prepared_page_count,
            camera_sequences.len(),
            label_frames.len(),
            draws.len(),
            pins.len()
                .saturating_add(page_leases.len())
                .saturating_add(1),
        );
        if retained_frame_bytes > frame_buffers.frame_capacity {
            return Err(AlignmentGpuPrepareError::FrameSlotCapacity {
                required: retained_frame_bytes,
                available: frame_buffers.frame_capacity,
            });
        }
        pins.push(frame_buffers.frame_allocation.pin()?);
        let frame = AlignmentGpuFrame {
            revision: next_revision,
            device_generation: self.device_generation,
            scene_revisions: scene_revisions.into(),
            camera_sequences: camera_sequences.into(),
            content_authority: content_authority.clone(),
            label_frames: label_frames.into(),
            draws: draws.into(),
            buffers: frame_buffers,
            _page_leases: page_leases.into_values().collect(),
            _pins: pins,
        };
        let resident_gpu_bytes = self.resident_gpu_bytes()?;
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
            label_anchors_examined: frame
                .label_frames
                .iter()
                .map(|labels| labels.anchors_examined)
                .sum(),
            page_pin_acquisitions,
            camera_uniform_bytes: uniforms.len(),
            geometry_uploaded_bytes: std::mem::take(&mut self.pending_geometry_uploaded_bytes),
            page_cache_hits,
            page_cache_misses,
            resident_pages: self.pages.len(),
            resident_gpu_bytes,
        };
        Ok((frame, stats))
    }

    /// Captures a new camera/scroll/interaction presentation over unchanged resident content.
    ///
    /// This is the no-bundle foreground path. It validates the replacement frame's immutable
    /// authority before touching presentation state, then reuses the bounded camera/overlay ring
    /// used by [`Self::prepare`]. It never admits or retires a semantic page, rebuilds the page
    /// table, reserves a replacement bundle, or uploads base geometry. A typed
    /// [`AlignmentGpuCameraCaptureError::ContentReplacementRequired`] tells the coordinator to use
    /// the existing replacement transaction instead.
    pub fn capture_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        authority: &AlignmentGpuContentAuthority,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
    ) -> Result<(AlignmentGpuFrame, AlignmentGpuPrepareStats), AlignmentGpuCameraCaptureError> {
        if authority.device_generation != self.device_generation {
            return Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::DeviceGeneration {
                    authority: authority.device_generation,
                    renderer: self.device_generation,
                },
            });
        }

        let requested_scene_revisions = requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        if requested_scene_revisions.as_slice() != authority.scene_revisions.as_ref() {
            return Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::SceneContent {
                    authority_scenes: authority.scene_revisions.len(),
                    requested_scenes: requested_scene_revisions.len(),
                },
            });
        }

        let requested_scene_allocations = requests
            .iter()
            .map(|request| request.scene.metadata_allocation().id())
            .collect::<Vec<_>>();
        if requested_scene_allocations.as_slice() != authority.scene_allocations.as_ref() {
            return Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::SceneContent {
                    authority_scenes: authority.scene_allocations.len(),
                    requested_scenes: requested_scene_allocations.len(),
                },
            });
        }

        let mandatory = mandatory_alignment_pages(requests)?;
        let mut missing = mandatory
            .iter()
            .filter(|page| self.page_index(page.key()).is_none())
            .map(|page| page.key());
        if let Some(first_missing) = missing.next() {
            let missing_pages = 1_usize.saturating_add(missing.count());
            return Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::MissingResidentPages {
                    missing: missing_pages,
                    first_missing,
                },
            });
        }

        // CPU packing/projection scratch remains transient-budget accounted. The important
        // single-flight invariant is that this path never creates a retained GPU/frame
        // reservation; transient reservations are independent of an active replacement bundle.
        let result =
            self.capture_resident_camera_frame(device, queue, requests, ledger, authority, false);
        let (mut frame, stats) = match result {
            Ok(prepared) => prepared,
            Err(AlignmentGpuPrepareError::FrameSlotBusy { slots }) => {
                return Err(AlignmentGpuCameraCaptureError::FrameSlotBusy { slots });
            }
            Err(AlignmentGpuPrepareError::FrameSlotCapacity {
                required,
                available,
            }) => {
                return Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                    reason: AlignmentGpuContentReplacementReason::FrameEnvelope {
                        required,
                        available,
                    },
                });
            }
            Err(error) => return Err(error.into()),
        };
        debug_assert_eq!(&frame.content_authority, authority);
        // Retain the exact replacement-minted token instead of constructing a logically equal
        // authority for every camera capture. The presentation remains independently replaceable.
        frame.content_authority = authority.clone();
        Ok((frame, stats))
    }

    /// Returns the complete sorted page-key set needed by the current foreground cameras. Shared
    /// cache coordination uses this allocation-free inventory to protect the other renderer's
    /// upcoming mandatory pages before either candidate is prepared.
    pub fn mandatory_page_keys(
        requests: &[AlignmentGpuSceneRequest<'_>],
    ) -> Result<Vec<AlignmentPageKey>, AlignmentGpuPrepareError> {
        Ok(mandatory_alignment_pages(requests)?
            .into_iter()
            .map(|page| page.key())
            .collect())
    }

    /// Returns the stable full prepared-scene key set used as speculative-generation authority.
    /// Unlike mandatory keys, this does not change during camera-only movement inside a prepared
    /// horizon.
    #[must_use]
    pub fn prepared_page_keys(requests: &[AlignmentGpuSceneRequest<'_>]) -> Vec<AlignmentPageKey> {
        prepared_scene_page_keys(requests)
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
        let pages = mandatory_alignment_pages(requests)?;
        let admission =
            self.admit_mandatory_pages(device, queue, &pages, ledger, publication_revision)?;
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
            ReservationKind::Normal,
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
            retained_bytes: 0,
            retention_limited: false,
        })
    }

    /// Starts a resumable prefetch cursor in O(scene lanes). The replacement-minted keys remain
    /// the authority; semantic pages are resolved lazily in bounded quanta by
    /// [`Self::advance_prepared_scene_prefetch_cursor`].
    pub fn begin_prepared_scene_prefetch_cursor(
        &self,
        requests: &[AlignmentGpuSceneRequest<'_>],
        page_keys: Arc<[AlignmentPageKey]>,
        maximum_total_pinned_bytes: usize,
    ) -> Result<AlignmentGpuPrefetchCursor, AlignmentGpuPrefetchCursorError> {
        let scenes = requests
            .iter()
            .map(|request| Arc::clone(request.scene))
            .collect();
        Ok(AlignmentGpuPrefetchCursor {
            device_generation: self.device_generation,
            page_keys,
            scenes,
            next_page: 0,
            pins: Vec::new(),
            retained_bytes: 0,
            maximum_total_pinned_bytes,
            retention_limited: false,
        })
    }

    /// Advances one cursor by at most `maximum_pages` missing prepared pages. Prior cursor pages
    /// remain pinned, so admission never rescans or re-protects the complete authority.
    pub fn advance_prepared_scene_prefetch_cursor(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cursor: &mut AlignmentGpuPrefetchCursor,
        ledger: &AllocationLedger,
        publication_revision: u64,
        maximum_pages: NonZeroUsize,
    ) -> Result<AlignmentGpuPrefetchProgress, AlignmentGpuPrefetchCursorError> {
        if cursor.device_generation != self.device_generation {
            return Err(AlignmentGpuPrefetchCursorError::DeviceGeneration {
                cursor: cursor.device_generation,
                renderer: self.device_generation,
            });
        }
        let authority_pages_before = cursor.remaining_pages();
        let end = cursor
            .next_page
            .saturating_add(maximum_pages.get())
            .min(cursor.page_keys.len());
        let inspected = end.saturating_sub(cursor.next_page);
        self.prefetch_authority_keys_inspected
            .fetch_add(inspected as u64, Ordering::Relaxed);
        let mut batch = Vec::with_capacity(inspected);
        let mut resident_pins = Vec::new();
        let mut resident_retained_bytes = 0_usize;
        for key in cursor.page_keys[cursor.next_page..end].iter().copied() {
            let page = cursor
                .scenes
                .iter()
                .find_map(|scene| scene.page_for_key(key))
                .ok_or(AlignmentGpuPrefetchCursorError::PreparedAuthorityChanged)?;
            if let Some(resident) = self.page(key) {
                if let Some(allocation) = &resident.allocation {
                    let capacity = allocation.snapshot()?.charged_capacity;
                    if let Some(pin) =
                        allocation.pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
                    {
                        resident_pins.push(pin);
                        resident_retained_bytes = resident_retained_bytes.saturating_add(capacity);
                    } else {
                        cursor.retention_limited = true;
                    }
                }
            } else {
                batch.push(Arc::clone(page));
            }
        }
        let admission = self.admit_pages(device, queue, &batch, ledger, publication_revision)?;
        self.pending_geometry_uploaded_bytes = self
            .pending_geometry_uploaded_bytes
            .saturating_sub(admission.uploaded_bytes);
        for page in &batch {
            let resident = self
                .page(page.key())
                .expect("successful cursor admission makes every batch page resident");
            if let Some(allocation) = &resident.allocation {
                let capacity = allocation.snapshot()?.charged_capacity;
                if let Some(pin) =
                    allocation.pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
                {
                    cursor.pins.push(pin);
                    cursor.retained_bytes = cursor.retained_bytes.saturating_add(capacity);
                } else {
                    cursor.retention_limited = true;
                }
            }
        }
        cursor.pins.extend(resident_pins);
        cursor.retained_bytes = cursor
            .retained_bytes
            .saturating_add(resident_retained_bytes);
        cursor.next_page = end;
        let remaining_authority_pages = cursor.remaining_pages();
        Ok(AlignmentGpuPrefetchProgress {
            prepared_pages: cursor.page_keys.len(),
            // Cursor progress deliberately treats unvisited authority keys as conservatively
            // missing. Determining the exact future cache-hit count would reintroduce the
            // complete-scene scan this cursor exists to remove.
            missing_pages_before: authority_pages_before,
            admission,
            remaining_missing_pages: remaining_authority_pages,
            missing_pages_remain: remaining_authority_pages > 0,
            retained_bytes: cursor.retained_bytes,
            retention_limited: cursor.retention_limited,
        })
    }

    /// Converts a completed cursor into the all-page residency lease without another scene scan.
    pub fn finish_prepared_scene_prefetch_cursor(
        &self,
        cursor: AlignmentGpuPrefetchCursor,
    ) -> Result<AlignmentGpuResidencyLease, AlignmentGpuPrefetchCursorError> {
        if cursor.device_generation != self.device_generation {
            return Err(AlignmentGpuPrefetchCursorError::DeviceGeneration {
                cursor: cursor.device_generation,
                renderer: self.device_generation,
            });
        }
        if !cursor.is_complete() {
            return Err(AlignmentGpuPrefetchCursorError::Incomplete {
                remaining: cursor.remaining_pages(),
            });
        }
        if cursor.retention_limited {
            return Err(AlignmentGpuPrefetchCursorError::ResidencyBudgetLimited {
                retained_bytes: cursor.retained_bytes,
                maximum_total_pinned_bytes: cursor.maximum_total_pinned_bytes,
            });
        }
        Ok(AlignmentGpuResidencyLease {
            device_generation: cursor.device_generation,
            page_keys: cursor.page_keys,
            _pins: cursor.pins,
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
            if let Some(allocation) = &resident.allocation {
                pins.push(allocation.pin()?);
            }
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
        self.reclaim_index.clear();
        self.page_table_allocation = None;
        self.page_table_capacity = 0;
        self.frame_buffers.clear();
        self.device_generation = self.device_generation.saturating_add(1).max(1);
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.pages.len()
    }

    /// Cooperatively releases renderer-local retained capacity until the exact failed shared-
    /// ledger request fits its original limit or no safe candidate remains. This preserves
    /// transition-reserve semantics when the current charged total is legitimately above the
    /// base budget. Idle frame-ring slots are dropped first, followed by an unpinned sparse page
    /// table and then unpinned pages from the online deterministic
    /// `(last_used_revision, page_key)` index. Planning stops once the exact goal is covered; it
    /// never rebuilds or sorts a full failure-time page inventory.
    /// `protected_pages` are never evicted by this pass; active frames and residency leases are
    /// independently protected by ledger pins.
    ///
    /// The protected set is call-scoped. A coordinator should include every current-but-unpinned
    /// mandatory page whose loss would make the next frame non-speculative.
    pub fn reclaim_for_goal(
        &mut self,
        ledger: &AllocationLedger,
        goal: GpuReclaimGoal,
        protected_pages: &[AlignmentPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        let ledger_before = ledger.progress()?;
        let pages_before = self.pages.len();
        let retained_bytes_before = self.resident_retained_bytes()?;

        if goal.is_feasible() && !shared_ledger_satisfies_goal(&ledger_before, goal) {
            self.reclaim_all_idle_frame_buffers();
            self.reclaim_sparse_page_table(ledger)?;
            loop {
                let snapshot = ledger.progress()?;
                if shared_ledger_satisfies_goal(&snapshot, goal) {
                    break;
                }
                let occupied = snapshot.charged.saturating_add(snapshot.reserved);
                let candidates = self
                    .indexed_unpinned_lru_candidates_until(protected_pages, |released| {
                        goal.is_satisfied_at(occupied.saturating_sub(released))
                    })?;
                if candidates.is_empty() {
                    break;
                }
                let checkpoint = self
                    .removals_until_sparse_page_table_checkpoint()?
                    .unwrap_or(candidates.len());
                let remove = checkpoint.min(candidates.len());
                if remove == 0 {
                    break;
                }
                self.remove_reclaim_candidates(&candidates[..remove]);
                self.reclaim_sparse_page_table(ledger)?;
            }
        }

        let retained_bytes_after = self.resident_retained_bytes()?;
        let ledger_after = ledger.progress()?;
        Ok(GpuReclaimReport {
            pressure: goal.pressure,
            baseline_occupied_bytes: goal.baseline_occupied_bytes,
            pages_before,
            pages_after: self.pages.len(),
            retained_bytes_before,
            retained_bytes_after,
            ledger_charged_before: ledger_before.charged,
            ledger_charged_after: ledger_after.charged,
            headroom_satisfied: shared_ledger_satisfies_goal(&ledger_after, goal),
        })
    }

    /// Creates a reclaim goal from the post-unwind ledger and applies it to this renderer. Shared
    /// coordinators should instead create one [`GpuReclaimGoal`] and pass it unchanged to every
    /// participating renderer via [`Self::reclaim_for_goal`].
    pub fn reclaim_for_budget_pressure(
        &mut self,
        ledger: &AllocationLedger,
        pressure: GpuBudgetPressure,
        protected_pages: &[AlignmentPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        let snapshot = ledger.progress()?;
        let goal =
            GpuReclaimGoal::new(pressure, snapshot.charged.saturating_add(snapshot.reserved));
        self.reclaim_for_goal(ledger, goal, protected_pages)
    }

    /// Convenience wrapper for callers whose failed request used the ledger's base retained
    /// budget rather than transition reserve.
    pub fn reclaim_for_headroom(
        &mut self,
        ledger: &AllocationLedger,
        required_headroom_bytes: usize,
        protected_pages: &[AlignmentPageKey],
    ) -> Result<GpuReclaimReport, LedgerError> {
        let snapshot = ledger.progress()?;
        let pressure = GpuBudgetPressure::from_budget_failure(
            required_headroom_bytes,
            snapshot.charged,
            snapshot.reserved,
            snapshot.budget,
        );
        self.reclaim_for_budget_pressure(ledger, pressure, protected_pages)
    }

    fn resident_retained_bytes(&self) -> Result<usize, LedgerError> {
        let mut total = self.resident_gpu_bytes()?;
        for slot in &self.frame_buffers {
            total = total.saturating_add(slot.frame_allocation.snapshot()?.charged_capacity);
        }
        Ok(total)
    }

    fn resident_gpu_bytes(&self) -> Result<usize, LedgerError> {
        let mut total = 0_usize;
        for page in &self.pages {
            if let Some(allocation) = &page.allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
        }
        if let Some(allocation) = &self.page_table_allocation {
            total = total.saturating_add(allocation.snapshot()?.charged_capacity);
        }
        for slot in &self.frame_buffers {
            if let Some(allocation) = &slot.camera_allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
            if let Some(allocation) = &slot.overlay_allocation {
                total = total.saturating_add(allocation.snapshot()?.charged_capacity);
            }
        }
        Ok(total)
    }

    fn page_index(&self, key: AlignmentPageKey) -> Option<usize> {
        self.pages
            .binary_search_by_key(&key, |resident| resident.key)
            .ok()
    }

    fn page(&self, key: AlignmentPageKey) -> Option<&GpuAlignmentPage> {
        self.page_index(key).map(|index| &self.pages[index])
    }

    fn rebuild_reclaim_index(&mut self) {
        self.reclaim_index.clear();
        self.reclaim_index.extend(
            self.pages
                .iter()
                .map(|page| (page.last_used_revision, page.key)),
        );
        debug_assert_eq!(self.reclaim_index.len(), self.pages.len());
    }

    fn touch_page_at(&mut self, index: usize, revision: u64) {
        let page = &mut self.pages[index];
        self.reclaim_index
            .remove(&(page.last_used_revision, page.key));
        page.last_used_revision = revision;
        assert!(self.reclaim_index.insert((revision, page.key)));
    }

    fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        reservation_kind: ReservationKind,
    ) -> Result<(), AlignmentGpuPageAdmissionError> {
        if required <= self.page_table_capacity {
            return Ok(());
        }
        let requested_capacity = next_capacity(required)?;
        let maximum_bytes = requested_capacity
            .checked_mul(2)
            .and_then(alignment_page_table_retained_bytes)
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        let mut reservation =
            ledger.reserve(AllocationClass::GpuPage, maximum_bytes, reservation_kind)?;
        let mut candidate = Vec::with_capacity(requested_capacity);
        let actual_capacity = candidate.capacity();
        if actual_capacity < required {
            return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                required,
                actual: actual_capacity,
            });
        }
        let actual_bytes = alignment_page_table_retained_bytes(actual_capacity)
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
        frame_bytes: usize,
    ) -> Result<Arc<GpuAlignmentFrameBuffers>, AlignmentGpuPrepareError> {
        if let Some(index) = self
            .frame_buffers
            .iter()
            .enumerate()
            .filter(|(_, slot)| Arc::strong_count(slot) == 1)
            .filter(|(_, slot)| {
                slot.camera_capacity >= uniforms
                    && slot.overlay_capacity >= overlays
                    && slot.frame_capacity >= frame_bytes
            })
            .min_by_key(|(_, slot)| (slot.camera_capacity, slot.overlay_capacity))
            .map(|(index, _)| index)
        {
            let slot = Arc::clone(&self.frame_buffers[index]);
            self.frame_buffers.retain(|candidate| {
                Arc::strong_count(candidate) > 1 || Arc::ptr_eq(candidate, &slot)
            });
            return Ok(slot);
        }

        self.reclaim_all_idle_frame_buffers();

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
        let mut frame_reservation = ledger.reserve(
            AllocationClass::TransitionFrame,
            frame_bytes,
            ReservationKind::Transition,
        )?;
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
        let frame_allocation = frame_reservation.register(frame_bytes, self.revision)?;
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
            frame_allocation,
            frame_capacity: frame_bytes,
            capture_ring: false,
        });
        self.frame_buffers.push(Arc::clone(&slot));
        Ok(slot)
    }

    /// Selects an already provisioned physical frame slot without touching the allocation
    /// ledger. A compatible content authority guarantees sufficient capacity; the only expected
    /// failure is that both bounded slots are still referenced by in-flight frames.
    fn acquire_existing_frame_buffers(
        &self,
        uniforms: usize,
        overlays: usize,
        frame_bytes: usize,
    ) -> Result<Arc<GpuAlignmentFrameBuffers>, AlignmentGpuPrepareError> {
        let mut adequate_slots = 0_usize;
        let mut available = 0_usize;
        for slot in &self.frame_buffers {
            available = available.max(slot.frame_capacity);
            if slot.camera_capacity < uniforms
                || slot.overlay_capacity < overlays
                || slot.frame_capacity < frame_bytes
            {
                continue;
            }
            adequate_slots = adequate_slots.saturating_add(1);
            if Arc::strong_count(slot) == 1 {
                return Ok(Arc::clone(slot));
            }
        }
        if adequate_slots >= ALIGNMENT_CAMERA_FRAME_SLOT_COUNT {
            Err(AlignmentGpuPrepareError::FrameSlotBusy {
                slots: adequate_slots,
            })
        } else {
            Err(AlignmentGpuPrepareError::FrameSlotCapacity {
                required: frame_bytes,
                available,
            })
        }
    }

    fn reclaim_idle_frame_buffers(&mut self) {
        self.frame_buffers
            .retain(|slot| slot.capture_ring || Arc::strong_count(slot) > 1);
    }

    fn reclaim_all_idle_frame_buffers(&mut self) {
        self.frame_buffers
            .retain(|slot| Arc::strong_count(slot) > 1);
    }

    fn reclaim_sparse_page_table(&mut self, ledger: &AllocationLedger) -> Result<(), LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(());
        };
        let snapshot = allocation.snapshot()?;
        if snapshot.pins > 0 {
            return Ok(());
        }
        if self.pages.is_empty() {
            self.pages = Vec::new();
            self.page_table_allocation = None;
            self.page_table_capacity = 0;
            return Ok(());
        }
        let Some(requested_capacity) = self.pages.len().checked_next_power_of_two() else {
            return Ok(());
        };
        if requested_capacity.saturating_mul(4) > self.page_table_capacity {
            return Ok(());
        }
        let maximum_bytes =
            alignment_page_table_retained_bytes(requested_capacity.saturating_mul(2))
                .unwrap_or(usize::MAX);
        let Ok(mut reservation) = ledger.reserve(
            AllocationClass::GpuPage,
            maximum_bytes,
            ReservationKind::Transition,
        ) else {
            return Ok(());
        };
        let mut candidate = Vec::with_capacity(requested_capacity);
        if candidate.capacity() < self.pages.len() {
            return Ok(());
        }
        let actual_bytes =
            alignment_page_table_retained_bytes(candidate.capacity()).unwrap_or(usize::MAX);
        let Ok(allocation) = reservation.register(actual_bytes, self.revision) else {
            return Ok(());
        };
        candidate.extend(std::mem::take(&mut self.pages));
        self.pages = candidate;
        self.page_table_capacity = self.pages.capacity();
        self.page_table_allocation = Some(allocation);
        Ok(())
    }

    fn indexed_unpinned_lru_candidates_until(
        &self,
        protected_pages: &[AlignmentPageKey],
        mut sufficient: impl FnMut(usize) -> bool,
    ) -> Result<Vec<AlignmentPageReclaimCandidate>, LedgerError> {
        let mut protected = protected_pages.to_vec();
        protected.sort_unstable();
        protected.dedup();
        let mut candidates = Vec::new();
        let mut reclaimable_bytes = 0_usize;
        for (_last_used_revision, key) in &self.reclaim_index {
            self.reclaim_authority_keys_inspected
                .fetch_add(1, Ordering::Relaxed);
            if protected.binary_search(key).is_ok() {
                continue;
            }
            let page = self
                .page(*key)
                .expect("online reclaim authority contains only resident pages");
            let allocation = page
                .allocation
                .as_ref()
                .map(AllocationLease::snapshot)
                .transpose()?;
            if allocation
                .as_ref()
                .is_none_or(|allocation| allocation.pins == 0)
            {
                candidates.push(AlignmentPageReclaimCandidate {
                    key: page.key,
                    charged_capacity: allocation
                        .map_or(0, |allocation| allocation.charged_capacity),
                });
                reclaimable_bytes = reclaimable_bytes.saturating_add(
                    candidates
                        .last()
                        .map_or(0, |candidate| candidate.charged_capacity),
                );
                if sufficient(reclaimable_bytes) {
                    break;
                }
            }
        }
        Ok(candidates)
    }

    fn removals_until_sparse_page_table_checkpoint(&self) -> Result<Option<usize>, LedgerError> {
        let Some(allocation) = self.page_table_allocation.as_ref() else {
            return Ok(None);
        };
        if allocation.snapshot()?.pins > 0 || self.pages.is_empty() {
            return Ok(None);
        }
        let quarter_capacity = self.page_table_capacity / 4;
        if quarter_capacity == 0 {
            return Ok(Some(self.pages.len()));
        }
        let maximum_sparse_len = 1_usize
            .checked_shl(usize::BITS - quarter_capacity.leading_zeros() - 1)
            .unwrap_or(0);
        if self.pages.len() <= maximum_sparse_len {
            // The immediately preceding shrink attempt was already eligible but could not
            // reserve its replacement. Retry after one more page releases capacity; combining
            // more removals would be able to overshoot a goal that the table release satisfies.
            return Ok(Some(1));
        }
        Ok(Some(self.pages.len().saturating_sub(maximum_sparse_len)))
    }

    fn remove_reclaim_candidates(&mut self, candidates: &[AlignmentPageReclaimCandidate]) {
        let mut keys = candidates
            .iter()
            .map(|candidate| candidate.key)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut next_key = 0;
        self.pages.retain(|page| {
            while keys
                .get(next_key)
                .is_some_and(|candidate| *candidate < page.key)
            {
                next_key = next_key.saturating_add(1);
            }
            if keys.get(next_key).is_some_and(|key| *key == page.key) {
                next_key = next_key.saturating_add(1);
                false
            } else {
                true
            }
        });
        self.rebuild_reclaim_index();
    }

    fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        limit: usize,
        protected: &[Arc<AlignmentSemanticPage>],
    ) -> Result<(), LedgerError> {
        self.reclaim_idle_frame_buffers();
        self.reclaim_sparse_page_table(ledger)?;
        let protected = protected.iter().map(|page| page.key()).collect::<Vec<_>>();
        loop {
            let snapshot = ledger.progress()?;
            if snapshot
                .charged
                .saturating_add(snapshot.reserved)
                .saturating_add(required)
                <= limit
            {
                return Ok(());
            }
            let occupied = snapshot.charged.saturating_add(snapshot.reserved);
            let candidates = self
                .indexed_unpinned_lru_candidates_until(&protected, |released| {
                    occupied.saturating_sub(released).saturating_add(required) <= limit
                })?;
            if candidates.is_empty() {
                return Ok(());
            }
            let checkpoint = self
                .removals_until_sparse_page_table_checkpoint()?
                .unwrap_or(candidates.len());
            let remove = checkpoint.min(candidates.len());
            if remove == 0 {
                return Ok(());
            }
            self.remove_reclaim_candidates(&candidates[..remove]);
            self.reclaim_sparse_page_table(ledger)?;
        }
    }
}

fn take_bundle_slot(next: &mut u32) -> Result<AllocationBundleSlot, AlignmentGpuReplacementError> {
    let slot = AllocationBundleSlot::new(*next);
    *next = next
        .checked_add(1)
        .ok_or(AlignmentGpuReplacementError::BundleSlotOverflow)?;
    Ok(slot)
}

fn exact_retirement_ownership(
    allocation: &AllocationLease,
    prior_pin_counts: &BTreeMap<AllocationId, usize>,
) -> Result<bool, LedgerError> {
    let snapshot = allocation.snapshot()?;
    let pins = prior_pin_counts.get(&allocation.id()).copied().unwrap_or(0);
    Ok(snapshot.references == pins.saturating_add(1) && snapshot.pins == pins)
}

fn take_replacement_leases(
    actuals: &[AllocationBundleActual],
    settled: &mut Vec<AllocationBundleLease>,
) -> Result<BTreeMap<AllocationBundleSlot, AllocationLease>, AlignmentGpuReplacementError> {
    let mut positions = BTreeMap::new();
    for actual in actuals.iter().filter(|actual| actual.charged_capacity > 0) {
        if positions.contains_key(&actual.slot) {
            return Err(AlignmentGpuReplacementError::DuplicateActualSlot(
                actual.slot,
            ));
        }
        let index = settled
            .iter()
            .position(|lease| lease.slot == actual.slot)
            .ok_or(AlignmentGpuReplacementError::MissingSettledLease(
                actual.slot,
            ))?;
        let snapshot = settled[index].allocation.snapshot()?;
        if snapshot.charged_capacity != actual.charged_capacity {
            return Err(AlignmentGpuReplacementError::SettledLeaseCapacity {
                slot: actual.slot,
                expected: actual.charged_capacity,
                actual: snapshot.charged_capacity,
            });
        }
        positions.insert(actual.slot, index);
    }

    let mut removals = positions
        .iter()
        .map(|(slot, index)| (*slot, *index))
        .collect::<Vec<_>>();
    removals.sort_unstable_by_key(|(_, index)| std::cmp::Reverse(*index));
    let mut allocations = BTreeMap::new();
    for (slot, index) in removals {
        allocations.insert(slot, settled.remove(index).allocation);
    }
    Ok(allocations)
}

fn alignment_gpu_frame_bytes(
    scene_revisions: usize,
    page_manifest: usize,
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
            scene_revisions
                .saturating_mul(size_of::<AllocationId>())
                .saturating_add(ARC_SLICE_HEADER_WORDS.saturating_mul(size_of::<usize>())),
        )
        .saturating_add(
            page_manifest
                .saturating_mul(size_of::<AlignmentPageKey>())
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
        // Keep ordinary reads visually quiet. Interaction is applied independently below, so
        // hover and selection still receive RGV's blue emphasis without making every unselected
        // record look active.
        AlignmentBodyColorToken::Neutral => [0.73, 0.73, 0.73, 1.0],
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

const fn interaction_token_requires_overlay(
    token: rgv_alignment_scene::AlignmentInteractionToken,
) -> bool {
    token.hovered || token.selected || token.selected_chain || token.highlighted
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
    } else if token.highlighted {
        ([0.96, 0.70, 0.12], 0.46)
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

/// Conservative retained charge for the sorted GPU page table and its online B-tree reclaim
/// authority. Four pointer words per key cover node links, occupancy slack, and allocator
/// metadata without depending on the standard library's private B-tree node layout.
fn alignment_page_table_retained_bytes(capacity: usize) -> Option<usize> {
    let page_table = capacity.checked_mul(size_of::<GpuAlignmentPage>())?;
    let reclaim_entry = size_of::<(u64, AlignmentPageKey)>()
        .checked_add(4_usize.checked_mul(size_of::<usize>())?)?;
    page_table.checked_add(capacity.checked_mul(reclaim_entry)?)
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

fn shared_ledger_satisfies_goal(snapshot: &LedgerProgress, goal: GpuReclaimGoal) -> bool {
    goal.is_satisfied_at(snapshot.charged.saturating_add(snapshot.reserved))
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

fn prepared_scene_page_keys(requests: &[AlignmentGpuSceneRequest<'_>]) -> Vec<AlignmentPageKey> {
    let mut keys = requests
        .iter()
        .flat_map(|request| request.scene.page_keys().iter().copied())
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn prepared_scene_page_key_authority(
    requests: &[AlignmentGpuSceneRequest<'_>],
) -> Arc<[AlignmentPageKey]> {
    match requests {
        [] => Arc::from([]),
        [request] => Arc::clone(request.scene.page_keys()),
        _ => prepared_scene_page_keys(requests).into(),
    }
}

fn mandatory_alignment_pages(
    requests: &[AlignmentGpuSceneRequest<'_>],
) -> Result<Vec<Arc<AlignmentSemanticPage>>, AlignmentGpuPrepareError> {
    for request in requests {
        validate_request(request)?;
    }
    let mut pages = requests
        .iter()
        .flat_map(|request| {
            let interval = request.camera.interval();
            let rows = request.camera.visible_rows();
            request
                .scene
                .visible_pages(interval, rows)
                .into_iter()
                .cloned()
        })
        .collect::<Vec<_>>();
    pages.sort_unstable_by_key(|page| page.key());
    pages.dedup_by_key(|page| page.key());
    Ok(pages)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentGpuContentReplacementReason {
    #[error(
        "content authority belongs to device generation {authority}, renderer is generation {renderer}"
    )]
    DeviceGeneration { authority: u64, renderer: u64 },
    #[error(
        "ordered scene content changed (authority has {authority_scenes} scenes, request has {requested_scenes})"
    )]
    SceneContent {
        authority_scenes: usize,
        requested_scenes: usize,
    },
    #[error(
        "prepared-page manifest changed (authority has {authority_pages} pages, request has {requested_pages})"
    )]
    PageManifest {
        authority_pages: usize,
        requested_pages: usize,
    },
    #[error(
        "{missing} pages required by the camera are not resident; first missing page is {first_missing:?}"
    )]
    MissingResidentPages {
        missing: usize,
        first_missing: AlignmentPageKey,
    },
    #[error(
        "camera frame needs {required} retained bytes but the content replacement provisioned {available}"
    )]
    FrameEnvelope { required: usize, available: usize },
}

#[derive(Debug, Error)]
pub enum AlignmentGpuCameraCaptureError {
    #[error("alignment GPU content replacement is required: {reason}")]
    ContentReplacementRequired {
        reason: AlignmentGpuContentReplacementReason,
    },
    #[error("all {slots} reusable alignment camera frame slots are in flight")]
    FrameSlotBusy { slots: usize },
    #[error(transparent)]
    Prepare(#[from] AlignmentGpuPrepareError),
}

#[derive(Debug, Error)]
pub enum AlignmentGpuReplacementError {
    #[error("allocation bundle slot space is exhausted")]
    BundleSlotOverflow,
    #[error(
        "alignment renderer changed after replacement planning: planned revision {planned_revision}, current revision {current_revision}"
    )]
    StaleRenderer {
        planned_revision: u64,
        current_revision: u64,
    },
    #[error(
        "alignment renderer device generation changed after planning: planned {planned}, current {current}"
    )]
    DeviceGeneration { planned: u64, current: u64 },
    #[error("prior alignment frame belongs to a different device generation")]
    PriorFrameDeviceGeneration,
    #[error(
        "alignment replacement bundle policy differs from its plan: planned {planned}, reserved {reserved}"
    )]
    BundlePolicyRevision { planned: u64, reserved: u64 },
    #[error("alignment replacement requests changed after planning")]
    RequestChangedAfterPlan,
    #[error("staged alignment frame retained {actual} bytes, exceeding its {maximum}-byte plan")]
    FrameCapacityExceeded { actual: usize, maximum: usize },
    #[error("alignment retirement conflicts with current renderer cache state")]
    RetirementRestoreConflict,
    #[error("settled allocation bundle omitted alignment slot {0:?}")]
    MissingSettledLease(AllocationBundleSlot),
    #[error("alignment replacement contains duplicate actual slot {0:?}")]
    DuplicateActualSlot(AllocationBundleSlot),
    #[error("alignment replacement received an unexpected settled slot {0:?}")]
    UnexpectedSettledLease(AllocationBundleSlot),
    #[error("settled alignment slot {slot:?} charged {actual} bytes; staged actual was {expected}")]
    SettledLeaseCapacity {
        slot: AllocationBundleSlot,
        expected: usize,
        actual: usize,
    },
    #[error(transparent)]
    Admission(#[from] AlignmentGpuPageAdmissionError),
    #[error(transparent)]
    Prepare(#[from] AlignmentGpuPrepareError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Bundle(#[from] AllocationBundleError),
    #[error(transparent)]
    LabelProjection(#[from] CpuProjectionError),
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

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentGpuPrefetchCursorError {
    #[error("prepared alignment GPU authority changed before prefetch began")]
    PreparedAuthorityChanged,
    #[error(
        "alignment GPU prefetch cursor belongs to device generation {cursor}, renderer is generation {renderer}"
    )]
    DeviceGeneration { cursor: u64, renderer: u64 },
    #[error("alignment GPU prefetch cursor still has {remaining} pages to admit")]
    Incomplete { remaining: usize },
    #[error(
        "complete alignment GPU residency exceeds the optional pin ceiling after retaining {retained_bytes} bytes (aggregate pinned ceiling {maximum_total_pinned_bytes})"
    )]
    ResidencyBudgetLimited {
        retained_bytes: usize,
        maximum_total_pinned_bytes: usize,
    },
    #[error(transparent)]
    Admission(#[from] AlignmentGpuPageAdmissionError),
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
    #[error(
        "alignment camera frame requires {required} retained bytes but the reusable slot provides {available}"
    )]
    FrameSlotCapacity { required: usize, available: usize },
    #[error("all {slots} reusable alignment camera frame slots are in flight")]
    FrameSlotBusy { slots: usize },
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

impl AlignmentGpuFrameBuildError {
    /// Persistent retained-budget pressure whose failed reservation may succeed after
    /// cooperative cache reclaim. The reported limit preserves whether transition reserve was
    /// available and should be passed unchanged to `reclaim_for_budget_pressure`. Transient
    /// pressure and malformed/device-invalid candidates deliberately do not request eviction
    /// from other cache owners.
    #[must_use]
    pub const fn persistent_budget_pressure(&self) -> Option<GpuBudgetPressure> {
        match self {
            Self::Admission(AlignmentGpuPageAdmissionError::Ledger(
                LedgerError::BudgetExceeded {
                    requested,
                    charged,
                    reserved,
                    limit,
                },
            ))
            | Self::Prepare(AlignmentGpuPrepareError::Admission(
                AlignmentGpuPageAdmissionError::Ledger(LedgerError::BudgetExceeded {
                    requested,
                    charged,
                    reserved,
                    limit,
                }),
            ))
            | Self::Prepare(AlignmentGpuPrepareError::Ledger(LedgerError::BudgetExceeded {
                requested,
                charged,
                reserved,
                limit,
            })) => Some(GpuBudgetPressure::from_budget_failure(
                *requested, *charged, *reserved, *limit,
            )),
            _ => None,
        }
    }
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
        AllocationBundleSpec, AllocationClass, AllocationLedger, CanonicalGenomicTile,
        CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement, CompleteCoverage,
        ContractFingerprint, EvidenceAcquisition, EvidenceChunk, EvidenceRevision,
        EvidenceRootBuilder, FilteredRecordProvenance, GeometryLod, LayoutContract,
        PayloadContract, PayloadSidecar, PresentationContract, ReservationKind,
        RevisionFingerprint, RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint,
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

    fn publish_bundled_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut AlignmentGpuRenderer,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        mut prior_frame: Option<AlignmentGpuFrame>,
        first_slot: u32,
        publication_revision: u64,
    ) -> (
        AlignmentGpuFrame,
        AlignmentGpuPrepareStats,
        AlignmentGpuBatchAdmission,
    ) {
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(requests, &snapshot, AllocationBundleSlot::new(first_slot))
            .unwrap();
        let retirement = renderer
            .detach_replacement_retirement(ledger, &plan, &mut prior_frame)
            .unwrap();
        assert!(prior_frame.is_none());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        let bundle = ledger.reserve_replacement_bundle(spec).unwrap();
        let staged = renderer
            .stage_replacement(
                device,
                queue,
                requests,
                plan,
                &bundle,
                ledger,
                publication_revision,
            )
            .unwrap();
        let ready = renderer.preflight_replacement_publication(staged).unwrap();
        let actuals = ready.actuals().to_vec();
        let committed = bundle.commit(actuals).unwrap();
        let mut retirement = Some(retirement);
        let mut leases = committed.retire_and_settle(&mut retirement).unwrap();
        assert!(retirement.is_none());
        let publishable = renderer.bind_replacement_leases(ready, &mut leases);
        let published = renderer.publish_replacement(publishable);
        assert!(leases.is_empty());
        published
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
    fn neutral_hover_selection_and_highlight_have_distinct_read_colours() {
        let mut semantic = primitive(AlignmentComponent::ReadSpan);
        semantic.style.body = AlignmentBodyColorToken::Neutral;
        let neutral = style_color(semantic.style);
        assert_eq!(neutral, [0.73, 0.73, 0.73, 1.0]);
        let hovered = interaction_color(
            neutral,
            rgv_alignment_scene::AlignmentInteractionToken {
                hovered: true,
                ..rgv_alignment_scene::AlignmentInteractionToken::default()
            },
        );
        let selected = interaction_color(
            neutral,
            rgv_alignment_scene::AlignmentInteractionToken {
                selected: true,
                ..rgv_alignment_scene::AlignmentInteractionToken::default()
            },
        );
        let highlighted = interaction_color(
            neutral,
            rgv_alignment_scene::AlignmentInteractionToken {
                highlighted: true,
                ..rgv_alignment_scene::AlignmentInteractionToken::default()
            },
        );
        assert_ne!(neutral, hovered);
        assert_ne!(hovered, selected);
        assert_ne!(selected, highlighted);
        assert!(selected[2] > selected[0]);
        assert!(highlighted[0] > highlighted[2]);
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
        assert_eq!(renderer.reclaim_index.len(), renderer.pages.len());
        assert_eq!(
            renderer
                .page_table_allocation
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .charged_capacity,
            alignment_page_table_retained_bytes(renderer.page_table_capacity).unwrap()
        );
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
    fn prepared_scene_prefetch_cursor_resolves_once_then_performs_only_bounded_work() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let interaction = AlignmentInteractionState::default();
        let request = AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: AlignmentCamera::new(
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
            .unwrap(),
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
        let keys: Arc<[AlignmentPageKey]> =
            AlignmentGpuRenderer::prepared_page_keys(&requests).into();
        let before = renderer.operation_counters();
        let mut cursor = renderer
            .begin_prepared_scene_prefetch_cursor(
                &requests,
                Arc::clone(&keys),
                ledger.progress().unwrap().budget,
            )
            .unwrap();
        let after_begin = renderer.operation_counters();
        assert_eq!(after_begin, before);

        let mut revision = 20_u64;
        while !cursor.is_complete() {
            let before_step = renderer.operation_counters();
            let progress = renderer
                .advance_prepared_scene_prefetch_cursor(
                    &device,
                    &queue,
                    &mut cursor,
                    &ledger,
                    revision,
                    NonZeroUsize::new(2).unwrap(),
                )
                .unwrap();
            assert!(progress.admission.requested_pages <= 2);
            let after_step = renderer.operation_counters();
            assert_eq!(
                after_step.content_manifest_builds,
                before_step.content_manifest_builds
            );
            assert_eq!(
                after_step.content_manifest_pages_enumerated,
                before_step.content_manifest_pages_enumerated
            );
            assert!(
                after_step
                    .prefetch_authority_keys_inspected
                    .saturating_sub(before_step.prefetch_authority_keys_inspected)
                    <= 2
            );
            revision = revision.saturating_add(1);
        }
        let residency = renderer
            .finish_prepared_scene_prefetch_cursor(cursor)
            .unwrap();
        assert_eq!(residency.page_keys.as_ref(), keys.as_ref());
        assert_eq!(renderer.resident_page_count(), keys.len());
        drop(residency);

        let mut limited = renderer
            .begin_prepared_scene_prefetch_cursor(&requests, Arc::clone(&keys), 0)
            .unwrap();
        while !limited.is_complete() {
            let progress = renderer
                .advance_prepared_scene_prefetch_cursor(
                    &device,
                    &queue,
                    &mut limited,
                    &ledger,
                    revision,
                    NonZeroUsize::new(2).unwrap(),
                )
                .unwrap();
            assert!(progress.retention_limited);
        }
        assert!(matches!(
            renderer.finish_prepared_scene_prefetch_cursor(limited),
            Err(AlignmentGpuPrefetchCursorError::ResidencyBudgetLimited {
                maximum_total_pinned_bytes: 0,
                ..
            })
        ));
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
        let snapshot = ledger.snapshot().unwrap();
        let retained = renderer.resident_retained_bytes().unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            retained,
            snapshot.charged,
            snapshot.reserved,
            snapshot.charged.saturating_add(snapshot.reserved),
        );
        let pinned = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();
        assert_eq!(pinned.pressure, pressure);
        assert_eq!(pinned.reclaimed_pages(), 0);
        assert!(!pinned.headroom_satisfied);
        assert_eq!(renderer.resident_page_count(), resident);
        drop(lease);
        let released = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();
        assert_eq!(released.reclaimed_pages(), resident);
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
        assert_eq!(
            stats.label_anchors_examined,
            second.label_frames()[0].anchors_examined
        );
        assert!(stats.label_anchors_examined >= stats.text_labels);
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
    fn camera_scroll_and_hover_captures_preserve_read_content_authority_without_a_bundle() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_scene_with_page_shape(
            &ledger,
            NonZeroU64::new(20).unwrap(),
            8,
            NonZeroU32::new(2).unwrap(),
        );
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 24.0,
        };
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 24,
        };
        let first_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 40).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let neutral = AlignmentInteractionState::default();
        let first_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: first_camera,
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction: &neutral,
        }];
        let (first, _, _) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            None,
            700,
            800,
        );
        renderer
            .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 801)
            .unwrap();
        let authority = first.content_authority().clone();
        assert_eq!(authority.page_manifest().len(), scene.pages().len());
        drop(first);

        let page_allocations = renderer
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>();
        let page_table_allocation = renderer
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id);
        let hovered = scene
            .pages()
            .iter()
            .flat_map(|page| page.primitives())
            .next()
            .expect("the overlapping read fixture produces semantic primitives")
            .handle;
        let interactions = [
            AlignmentInteractionState::default(),
            AlignmentInteractionState::default(),
            AlignmentInteractionState::new(Some(hovered), None, []),
        ];
        let cameras = [
            AlignmentCamera::new(
                GenomicInterval::new(ContigId::new(0), 5, 45).unwrap(),
                0.0,
                12.0,
                9.0,
                clip,
            )
            .unwrap(),
            AlignmentCamera::new(
                GenomicInterval::new(ContigId::new(0), 10, 50).unwrap(),
                2.0,
                12.0,
                9.0,
                clip,
            )
            .unwrap(),
            AlignmentCamera::new(
                GenomicInterval::new(ContigId::new(0), 15, 55).unwrap(),
                1.0,
                12.0,
                9.0,
                clip,
            )
            .unwrap(),
        ];
        let operations_before = ledger.operation_counters().unwrap();
        let renderer_operations_before = renderer.operation_counters();
        let mut presented = None;
        let mut page_pin_acquisitions = Vec::new();
        for (index, (camera, interaction)) in cameras.into_iter().zip(&interactions).enumerate() {
            assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
            let requests = [AlignmentGpuSceneRequest {
                scene: &scene,
                camera_sequence: 2 + index as u64,
                camera,
                screen_size_points: [256.0, 24.0],
                scissor,
                interaction,
            }];
            let (frame, stats) = renderer
                .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
                .unwrap();
            assert_eq!(stats.geometry_uploaded_bytes, 0);
            assert_eq!(stats.page_cache_misses, 0);
            page_pin_acquisitions.push(stats.page_pin_acquisitions);
            assert_eq!(frame.content_authority(), &authority);
            assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
            assert_eq!(
                renderer
                    .pages
                    .iter()
                    .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                    .collect::<Vec<_>>(),
                page_allocations
            );
            assert_eq!(
                renderer
                    .page_table_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                page_table_allocation
            );
            let previous = presented.replace(frame);
            drop(previous);
        }
        assert!(page_pin_acquisitions[0] > 0);
        let repeated_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 99,
            camera: cameras[2],
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction: &interactions[2],
        }];
        let (repeated, repeated_stats) = renderer
            .capture_camera_frame(&device, &queue, &authority, &repeated_requests, &ledger)
            .unwrap();
        assert_eq!(
            repeated_stats.page_pin_acquisitions, 0,
            "a contained frame must reuse the prior presentation's page leases"
        );
        let previous = presented.replace(repeated);
        drop(previous);
        let operations_after = ledger.operation_counters().unwrap();
        let renderer_operations_after = renderer.operation_counters();
        assert_eq!(
            operations_after.full_inventory_snapshots, operations_before.full_inventory_snapshots,
            "camera, vertical-scroll, and hover capture must not inventory retained allocations"
        );
        assert_eq!(
            operations_after.allocation_records_enumerated,
            operations_before.allocation_records_enumerated
        );
        assert_eq!(
            renderer_operations_after, renderer_operations_before,
            "camera, vertical-scroll, and hover capture must not rebuild the full prepared-page manifest"
        );
        assert!(renderer.frame_buffers.len() <= 2);

        let changed_scene_set = [
            AlignmentGpuSceneRequest {
                scene: &scene,
                camera_sequence: 7,
                camera: first_camera,
                screen_size_points: [256.0, 24.0],
                scissor,
                interaction: &neutral,
            },
            AlignmentGpuSceneRequest {
                scene: &scene,
                camera_sequence: 8,
                camera: first_camera,
                screen_size_points: [256.0, 24.0],
                scissor,
                interaction: &neutral,
            },
        ];
        assert!(matches!(
            renderer
                .capture_camera_frame(&device, &queue, &authority, &changed_scene_set, &ledger,),
            Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::SceneContent { .. }
            })
        ));

        let changed_manifest_scene =
            test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
        assert_eq!(changed_manifest_scene.revision(), scene.revision());
        let changed_requests = [AlignmentGpuSceneRequest {
            scene: &changed_manifest_scene,
            camera_sequence: 9,
            camera: first_camera,
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction: &neutral,
        }];
        assert!(matches!(
            renderer.capture_camera_frame(&device, &queue, &authority, &changed_requests, &ledger,),
            Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::SceneContent { .. }
            })
        ));
        let (replacement, replacement_stats, _) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &changed_requests,
            &ledger,
            None,
            740,
            840,
        );
        assert!(replacement_stats.geometry_uploaded_bytes > 0);

        let replacement_authority = replacement.content_authority().clone();
        drop(replacement);
        renderer.discard_device_resources();
        assert!(matches!(
            renderer.capture_camera_frame(
                &device,
                &queue,
                &replacement_authority,
                &changed_requests,
                &ledger,
            ),
            Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentGpuContentReplacementReason::DeviceGeneration { .. }
            })
        ));
    }

    #[test]
    fn predicate_highlight_generates_a_real_gpu_overlay_frame() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
        let scene = test_scene_with_page_shape(
            &ledger,
            NonZeroU64::new(20).unwrap(),
            8,
            NonZeroU32::new(2).unwrap(),
        );
        let highlighted_handles = scene
            .pages()
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
            .map(|primitive| primitive.handle)
            .collect::<BTreeSet<_>>();
        assert!(highlighted_handles.len() > ORDINARY_INTERACTION_HANDLES);
        let interaction = AlignmentInteractionState::default()
            .with_highlighted(highlighted_handles.iter().copied());
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        };
        let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
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
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let (frame, stats, _) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &[request],
            &ledger,
            None,
            760,
            860,
        );

        assert!(stats.overlay_instances > 0);
        assert!(
            frame
                .draws
                .iter()
                .any(|draw| !draw.overlay_range.is_empty())
        );
        let visible_rows = camera.visible_rows();
        let full_highlight_envelope = scene
            .camera_page_envelope(
                scene.prepared_interval().len(),
                visible_rows.end.saturating_sub(visible_rows.start),
            )
            .saturating_mul(scene.maximum_non_label_primitives_per_page());
        assert!(
            renderer
                .frame_buffers
                .iter()
                .all(|slot| slot.overlay_capacity >= full_highlight_envelope)
        );
        render_test_frame(&device, &queue, &renderer, &frame).unwrap();
    }

    #[test]
    fn compatible_read_capture_uses_the_precharged_ring_while_an_unrelated_bundle_is_active() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
        let scene = test_scene_with_page_shape(
            &ledger,
            NonZeroU64::new(20).unwrap(),
            3,
            NonZeroU32::new(16).unwrap(),
        );
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let clip = rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 512.0,
            height: 256.0,
        };
        let first_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let neutral = AlignmentInteractionState::default();
        let scissor = AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 512,
            height: 256,
        };
        let first_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: first_camera,
            screen_size_points: [512.0, 256.0],
            scissor,
            interaction: &neutral,
        }];
        let (first, _, _) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            None,
            900,
            901,
        );
        renderer
            .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 902)
            .unwrap();
        let authority = first.content_authority().clone();
        assert_eq!(
            renderer.frame_buffers.len(),
            ALIGNMENT_CAMERA_FRAME_SLOT_COUNT
        );
        let slot_ids = renderer
            .frame_buffers
            .iter()
            .map(|slot| {
                (
                    slot.camera_allocation.as_ref().map(AllocationLease::id),
                    slot.overlay_allocation.as_ref().map(AllocationLease::id),
                    slot.frame_allocation.id(),
                )
            })
            .collect::<Vec<_>>();

        let handles = scene
            .pages()
            .iter()
            .flat_map(|page| page.primitives())
            .map(|primitive| primitive.handle)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(handles.len(), 3);
        let interaction = AlignmentInteractionState::new(
            handles.first().copied(),
            handles.get(1).copied(),
            handles.iter().copied(),
        );
        let wide_camera =
            AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
        let requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: wide_camera,
            screen_size_points: [512.0, 256.0],
            scissor,
            interaction: &interaction,
        }];

        let policy_revision = ledger.snapshot().unwrap().policy_revision;
        let unrelated = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            ))
            .unwrap();
        let before = ledger.snapshot().unwrap();
        let allocation_ids_before = before
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let (second, stats) = renderer
            .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
            .unwrap();
        assert!(stats.mandatory_pages > first.draw_count());
        assert!(stats.overlay_instances > 0);
        assert_eq!(stats.geometry_uploaded_bytes, 0);
        assert_ne!(
            first.buffers.frame_allocation.id(),
            second.buffers.frame_allocation.id()
        );
        assert!(matches!(
            renderer.capture_camera_frame(&device, &queue, &authority, &requests, &ledger),
            Err(AlignmentGpuCameraCaptureError::FrameSlotBusy { slots: 2 })
        ));
        let after = ledger.snapshot().unwrap();
        assert_eq!(
            after.active_replacement_bundle,
            before.active_replacement_bundle
        );
        assert_eq!(after.charged, before.charged);
        assert_eq!(after.reserved, before.reserved);
        assert_eq!(
            after
                .records
                .iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            allocation_ids_before
        );
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(|slot| (
                    slot.camera_allocation.as_ref().map(AllocationLease::id),
                    slot.overlay_allocation.as_ref().map(AllocationLease::id),
                    slot.frame_allocation.id(),
                ))
                .collect::<Vec<_>>(),
            slot_ids
        );
        drop(unrelated);
        drop(second);
        drop(first);
    }

    #[test]
    fn large_read_horizon_overlay_slots_scale_with_interaction_not_geometry() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(128 << 20, 16 << 20, 32 << 20);
        let scene = test_scene_with_page_shape(
            &ledger,
            NonZeroU64::new(20).unwrap(),
            2_048,
            NonZeroU32::new(256).unwrap(),
        );
        let renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 512.0,
                height: 256.0,
            },
        )
        .unwrap();
        let neutral = AlignmentInteractionState::default();
        let requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera,
            screen_size_points: [512.0, 256.0],
            scissor: AlignmentGpuScissor {
                x: 0,
                y: 0,
                width: 512,
                height: 256,
            },
            interaction: &neutral,
        }];
        let plan = renderer
            .plan_replacement(
                &requests,
                &ledger.snapshot().unwrap(),
                AllocationBundleSlot::new(1_000),
            )
            .unwrap();
        let full_non_label_geometry = scene
            .pages()
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
            .count();
        assert!(full_non_label_geometry > 2_000);
        let visible_rows = camera.visible_rows();
        let camera_envelope = scene.camera_page_envelope(
            scene.prepared_interval().len(),
            visible_rows.end.saturating_sub(visible_rows.start),
        );
        assert_eq!(plan.camera_capacity, camera_envelope.next_power_of_two());
        assert!(plan.camera_capacity < scene.pages().len());
        assert!(plan.overlay_capacity > 0);
        let ordinary_overlay_envelope = camera_envelope
            .saturating_mul(scene.maximum_handle_primitives_per_page())
            .saturating_mul(3);
        assert_eq!(
            plan.overlay_capacity,
            ordinary_overlay_envelope.next_power_of_two()
        );
        assert!(plan.overlay_capacity < full_non_label_geometry);
        assert_eq!(plan.frame_slots.len(), ALIGNMENT_CAMERA_FRAME_SLOT_COUNT);
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
        let snapshot = ledger.snapshot().unwrap();
        let retained = renderer.resident_retained_bytes().unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            retained,
            snapshot.charged,
            snapshot.reserved,
            snapshot.charged.saturating_add(snapshot.reserved),
        );
        let pinned = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();
        assert_eq!(pinned.reclaimed_pages(), 0);
        assert_eq!(renderer.resident_page_count(), resident);
        assert_eq!(renderer.frame_buffers.len(), 1);
        render_test_frame(&device, &queue, &renderer, &frame).unwrap();
        drop(frame);
        let released = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();
        assert_eq!(released.reclaimed_pages(), resident);
        assert_eq!(renderer.resident_page_count(), 0);
        assert!(renderer.frame_buffers.is_empty());
    }

    #[test]
    fn cooperative_reclaim_drops_idle_resources_and_reports_renderer_local_bytes() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene(&ledger);
        let unrelated_bytes = 4_096;
        let _unrelated = ledger
            .reserve(
                AllocationClass::GpuPage,
                unrelated_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(unrelated_bytes, 1)
            .unwrap();
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
        let request = |camera_sequence| AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence,
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
        let (first, _) = renderer
            .prepare(&device, &queue, &[request(1)], &ledger)
            .unwrap();
        let (second, _) = renderer
            .prepare(&device, &queue, &[request(2)], &ledger)
            .unwrap();
        assert_eq!(renderer.frame_buffers.len(), 2);
        drop(first);
        drop(second);

        let before = ledger.snapshot().unwrap();
        let retained_before = renderer.resident_retained_bytes().unwrap();
        assert!(retained_before > 0);
        let available = before
            .budget
            .saturating_sub(before.charged.saturating_add(before.reserved));
        let pressure = GpuBudgetPressure::from_budget_failure(
            available.saturating_add(retained_before),
            before.charged,
            before.reserved,
            before.budget,
        );
        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();

        assert_eq!(report.pressure, pressure);
        assert_eq!(report.pages_before, scene.pages().len());
        assert_eq!(report.pages_after, 0);
        assert_eq!(report.retained_bytes_before, retained_before);
        assert_eq!(report.retained_bytes_after, 0);
        assert_eq!(report.ledger_charged_before, before.charged);
        assert_eq!(report.released_retained_bytes(), retained_before);
        assert_eq!(report.released_ledger_bytes(), retained_before);
        assert!(report.headroom_satisfied);
        assert!(renderer.frame_buffers.is_empty());
        assert_eq!(renderer.page_table_capacity, 0);
        assert!(renderer.page_table_allocation.is_none());

        let global_gpu_bytes = ledger
            .snapshot()
            .unwrap()
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::GpuPage)
            .map(|record| record.charged_capacity)
            .sum::<usize>();
        assert_eq!(global_gpu_bytes, unrelated_bytes);
    }

    #[test]
    fn cooperative_reclaim_skips_protected_and_pinned_pages_in_deterministic_lru_order() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
        assert!(scene.pages().len() > 2);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
            .unwrap();
        for page in &mut renderer.pages {
            page.last_used_revision = 7;
        }
        renderer.rebuild_reclaim_index();
        let protected = renderer.pages[0].key;
        let pinned_key = renderer.pages[1].key;
        let pin = renderer
            .page(pinned_key)
            .unwrap()
            .allocation
            .as_ref()
            .unwrap()
            .pin()
            .unwrap();
        let eligible_order = renderer
            .pages
            .iter()
            .filter(|page| page.key != protected && page.key != pinned_key)
            .map(|page| page.key)
            .collect::<Vec<_>>();
        assert!(eligible_order.len() > 1);
        let eligible_bytes = renderer
            .pages
            .iter()
            .filter(|page| page.key != protected && page.key != pinned_key)
            .map(|page| {
                page.allocation
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .unwrap()
                    .charged_capacity
            })
            .sum::<usize>();
        let before = ledger.snapshot().unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            eligible_bytes,
            before.charged,
            before.reserved,
            before.charged.saturating_add(before.reserved),
        );
        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[protected])
            .unwrap();
        let remaining = renderer
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>();
        let removed = eligible_order
            .iter()
            .copied()
            .filter(|key| !remaining.contains(key))
            .collect::<Vec<_>>();

        assert!(removed.len() > 1);
        assert_eq!(removed.as_slice(), &eligible_order[..removed.len()]);
        assert_eq!(report.reclaimed_pages(), removed.len());
        assert!(remaining.contains(&protected));
        assert!(remaining.contains(&pinned_key));
        assert!(
            renderer
                .page(pinned_key)
                .unwrap()
                .allocation
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap()
                .pins
                > 0
        );
        drop(pin);
    }

    #[test]
    fn infeasible_reclaim_pressure_leaves_the_scene_cache_untouched() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
            .unwrap();
        let page_keys_before = renderer
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>();
        let table_before = renderer
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id);
        let table_capacity_before = renderer.page_table_capacity;
        let retained_before = renderer.resident_retained_bytes().unwrap();
        let ledger_before = ledger.snapshot().unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            ledger_before.budget.saturating_add(1),
            ledger_before.charged,
            ledger_before.reserved,
            ledger_before.budget,
        );

        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();

        assert!(
            !GpuReclaimGoal::new(
                pressure,
                ledger_before.charged.saturating_add(ledger_before.reserved)
            )
            .is_feasible()
        );
        assert_eq!(report.reclaimed_pages(), 0);
        assert!(!report.headroom_satisfied);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.key)
                .collect::<Vec<_>>(),
            page_keys_before
        );
        assert_eq!(
            renderer
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            table_before
        );
        assert_eq!(renderer.page_table_capacity, table_capacity_before);
        assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
        assert_eq!(ledger.snapshot().unwrap().charged, ledger_before.charged);
    }

    #[test]
    fn exact_transition_pressure_does_not_reclaim_when_the_original_limit_already_fits() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let transition_reserve = 20;
        let ledger =
            AllocationLedger::new_with_transient_budget(32 << 20, transition_reserve, 16 << 20);
        let scene = test_scene(&ledger);
        let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        renderer
            .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
            .unwrap();
        let before = ledger.snapshot().unwrap();
        let base_budget = before.charged.saturating_sub(15);
        ledger.set_budget(base_budget).unwrap();
        let pressure = GpuBudgetPressure::from_budget_failure(
            5,
            before.charged,
            before.reserved,
            base_budget.saturating_add(transition_reserve),
        );
        let pages_before = renderer.resident_page_count();
        let retained_before = renderer.resident_retained_bytes().unwrap();
        let report = renderer
            .reclaim_for_budget_pressure(&ledger, pressure, &[])
            .unwrap();

        assert!(report.headroom_satisfied);
        assert_eq!(report.reclaimed_pages(), 0);
        assert_eq!(report.released_retained_bytes(), 0);
        assert_eq!(renderer.resident_page_count(), pages_before);
        assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
        assert!(report.ledger_charged_before > base_budget);
        assert_eq!(report.ledger_charged_after, report.ledger_charged_before);
    }

    #[test]
    fn read_gpu_stats_exclude_unrelated_shared_ledger_gpu_allocations() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
        let scene = test_scene(&ledger);
        let unrelated_bytes = 4_096;
        let _unrelated = ledger
            .reserve(
                AllocationClass::GpuPage,
                unrelated_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(unrelated_bytes, 1)
            .unwrap();
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
        let (_frame, stats, _) = renderer
            .prepare_with_admission(&device, &queue, &[request], &ledger, 10)
            .unwrap();

        assert_eq!(
            stats.resident_gpu_bytes,
            renderer.resident_gpu_bytes().unwrap()
        );
        let global_gpu_bytes = ledger
            .snapshot()
            .unwrap()
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::GpuPage)
            .map(|record| record.charged_capacity)
            .sum::<usize>();
        assert_eq!(
            global_gpu_bytes,
            stats.resident_gpu_bytes.saturating_add(unrelated_bytes)
        );
    }

    #[test]
    fn first_bundled_frame_cannot_spend_transition_reserve_above_the_base_budget() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
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
        let initial = ledger.snapshot().unwrap();
        let preliminary = renderer
            .plan_replacement(&requests, &initial, AllocationBundleSlot::new(7))
            .unwrap();
        let candidate_bytes = preliminary
            .demands()
            .iter()
            .try_fold(0_usize, |total, demand| {
                total.checked_add(demand.maximum_bytes)
            })
            .unwrap();
        assert!(candidate_bytes > 0);
        let occupied = initial.charged.saturating_add(initial.reserved);
        ledger
            .set_limits(
                occupied.saturating_add(candidate_bytes).saturating_sub(1),
                candidate_bytes,
                initial.transient_budget,
            )
            .unwrap();

        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(&requests, &snapshot, AllocationBundleSlot::new(7))
            .unwrap();
        let mut prior = None;
        let retirement = renderer
            .detach_replacement_retirement(&ledger, &plan, &mut prior)
            .unwrap();
        assert!(retirement.retirement_claim_set().is_empty());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        assert!(matches!(
            ledger.reserve_replacement_bundle(spec),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));
        assert!(
            renderer
                .restore_replacement_retirement(retirement)
                .unwrap()
                .is_none()
        );
        assert_eq!(renderer.resident_page_count(), 0);
        assert!(renderer.frame_buffers.is_empty());
    }

    #[test]
    fn retirement_preflight_error_preserves_the_caller_frame_and_renderer_cache() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
        let foreign_ledger =
            AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
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
        let (frame, _, _) = renderer
            .prepare_with_admission(&device, &queue, &requests, &ledger, 72)
            .unwrap();
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(&requests, &snapshot, AllocationBundleSlot::new(13))
            .unwrap();
        let renderer_revision = renderer.revision;
        let page_ids = renderer
            .pages
            .iter()
            .map(|page| page.allocation.as_ref().map(AllocationLease::id))
            .collect::<Vec<_>>();
        let table = (
            renderer
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            renderer.page_table_capacity,
        );
        let buffer_identity = renderer
            .frame_buffers
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>();
        let frame_id = frame.buffers.frame_allocation.id();
        let mut prior = Some(frame);

        assert!(matches!(
            renderer.detach_replacement_retirement(&foreign_ledger, &plan, &mut prior),
            Err(AlignmentGpuReplacementError::Bundle(
                AllocationBundleError::ForeignRetirementReference { .. }
            ))
        ));
        assert_eq!(renderer.revision, renderer_revision);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| page.allocation.as_ref().map(AllocationLease::id))
                .collect::<Vec<_>>(),
            page_ids
        );
        assert_eq!(
            (
                renderer
                    .page_table_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                renderer.page_table_capacity,
            ),
            table
        );
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(Arc::as_ptr)
                .collect::<Vec<_>>(),
            buffer_identity
        );
        let prior = prior
            .as_ref()
            .expect("preflight refusal must not consume it");
        assert_eq!(prior.buffers.frame_allocation.id(), frame_id);
        render_test_frame(&device, &queue, &renderer, prior).unwrap();
    }

    #[test]
    fn over_base_bundled_replacement_refusal_restores_the_exact_prior_frame_and_cache() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
        let old_scene = test_scene(&ledger);
        let candidate_scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
        assert!(candidate_scene.pages().len() >= 4);
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
        let old_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 25).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let old_requests = [AlignmentGpuSceneRequest {
            scene: &old_scene,
            camera_sequence: 1,
            camera: old_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        }];
        let (prior, _, _) = renderer
            .prepare_with_admission(&device, &queue, &old_requests, &ledger, 70)
            .unwrap();
        let renderer_revision = renderer.revision;
        let pages_before = renderer
            .pages
            .iter()
            .map(|page| {
                (
                    page.key,
                    page.allocation.as_ref().map(AllocationLease::id),
                    page.instance_count,
                    page.last_used_revision,
                )
            })
            .collect::<Vec<_>>();
        let table_before = (
            renderer
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            renderer.page_table_capacity,
        );
        let frame_buffers_before = renderer
            .frame_buffers
            .iter()
            .map(|buffers| {
                (
                    Arc::as_ptr(buffers),
                    buffers.camera_allocation.as_ref().map(AllocationLease::id),
                    buffers.overlay_allocation.as_ref().map(AllocationLease::id),
                    buffers.camera_capacity,
                    buffers.overlay_capacity,
                )
            })
            .collect::<Vec<_>>();
        let prior_frame_allocation = prior.buffers.frame_allocation.id();
        let prior_buffer = Arc::as_ptr(&prior.buffers);

        let before = ledger.snapshot().unwrap();
        ledger.set_budget(before.charged).unwrap();
        let new_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 25, 100).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let new_requests = [AlignmentGpuSceneRequest {
            scene: &candidate_scene,
            camera_sequence: 2,
            camera: new_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        }];
        let snapshot = ledger.snapshot().unwrap();
        let plan = renderer
            .plan_replacement(&new_requests, &snapshot, AllocationBundleSlot::new(31))
            .unwrap();
        assert!(plan.missing_pages.len() > pages_before.len());
        assert!(plan.page_table_slot.is_some());
        let mut prior = Some(prior);
        let retirement = renderer
            .detach_replacement_retirement(&ledger, &plan, &mut prior)
            .unwrap();
        assert!(prior.is_none());
        assert_eq!(retirement.retired_pages.len(), pages_before.len());
        assert!(retirement.retired_page_table.is_some());
        assert_eq!(retirement.retired_frame_buffers.len(), 1);
        assert!(renderer.pages.is_empty());
        assert!(renderer.page_table_allocation.is_none());
        assert!(renderer.frame_buffers.is_empty());
        let spec = AllocationBundleSpec::new(
            plan.policy_revision(),
            plan.demands().to_vec(),
            retirement.retirement_claim_set().clone(),
        );
        assert!(matches!(
            ledger.reserve_replacement_bundle(spec),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));

        let restored = renderer
            .restore_replacement_retirement(retirement)
            .unwrap()
            .unwrap();
        assert_eq!(renderer.revision, renderer_revision);
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| {
                    (
                        page.key,
                        page.allocation.as_ref().map(AllocationLease::id),
                        page.instance_count,
                        page.last_used_revision,
                    )
                })
                .collect::<Vec<_>>(),
            pages_before
        );
        assert_eq!(
            (
                renderer
                    .page_table_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                renderer.page_table_capacity,
            ),
            table_before
        );
        assert_eq!(
            renderer
                .frame_buffers
                .iter()
                .map(|buffers| {
                    (
                        Arc::as_ptr(buffers),
                        buffers.camera_allocation.as_ref().map(AllocationLease::id),
                        buffers.overlay_allocation.as_ref().map(AllocationLease::id),
                        buffers.camera_capacity,
                        buffers.overlay_capacity,
                    )
                })
                .collect::<Vec<_>>(),
            frame_buffers_before
        );
        assert_eq!(
            restored.buffers.frame_allocation.id(),
            prior_frame_allocation
        );
        assert_eq!(Arc::as_ptr(&restored.buffers), prior_buffer);
        render_test_frame(&device, &queue, &renderer, &restored).unwrap();
    }

    #[test]
    fn bundled_constant_camera_reuses_prepared_geometry_without_uploading_it_again() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
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
        let request = |camera_sequence| AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence,
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
        let first_requests = [request(1)];
        let (first, first_stats, first_admission) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            None,
            80,
            90,
        );
        assert!(first_admission.admitted_pages > 0);
        assert!(first_stats.geometry_uploaded_bytes > 0);

        let second_requests = [request(1)];
        let (second, second_stats, second_admission) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &second_requests,
            &ledger,
            Some(first),
            100,
            91,
        );
        assert_eq!(second_admission.admitted_pages, 0);
        assert_eq!(second_admission.already_resident_pages, scene.pages().len());
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(second_stats.page_cache_misses, 0);
        render_test_frame(&device, &queue, &renderer, &second).unwrap();
    }

    #[test]
    fn bundled_replacements_preserve_multi_quantum_prepared_prefetch_without_repeat_uploads() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
        let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
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
            GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let first_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 1,
            camera: first_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        }];
        let prepared_keys = AlignmentGpuRenderer::prepared_page_keys(&first_requests);
        assert!(prepared_keys.len() >= 5);
        let (first, _, first_admission) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &first_requests,
            &ledger,
            None,
            180,
            300,
        );
        assert!(first_admission.admitted_pages < prepared_keys.len());

        let mut admitted_quanta = 0_usize;
        let mut previous_resident = renderer.resident_page_count();
        loop {
            let progress = renderer
                .prefetch_prepared_scene_pages_bounded(
                    &device,
                    &queue,
                    &first_requests,
                    &ledger,
                    301 + admitted_quanta as u64,
                    NonZeroUsize::new(1).unwrap(),
                )
                .unwrap();
            assert!(progress.admission.admitted_pages <= 1);
            let resident = renderer.resident_page_count();
            assert!(resident >= previous_resident);
            if progress.admission.admitted_pages > 0 {
                admitted_quanta = admitted_quanta.saturating_add(1);
                assert_eq!(resident, previous_resident.saturating_add(1));
            }
            previous_resident = resident;
            if progress.is_complete() {
                break;
            }
        }
        assert!(admitted_quanta > 2);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        let converged_page_ids = renderer
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>();

        let repeated_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2,
            camera: first_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        }];
        let (second, second_stats, second_admission) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &repeated_requests,
            &ledger,
            Some(first),
            220,
            320,
        );
        assert_eq!(second_admission.admitted_pages, 0);
        assert_eq!(second_stats.geometry_uploaded_bytes, 0);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                .collect::<Vec<_>>(),
            converged_page_ids
        );
        let no_repeat = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &repeated_requests,
                &ledger,
                321,
                NonZeroUsize::new(1).unwrap(),
            )
            .unwrap();
        assert!(no_repeat.is_complete());
        assert_eq!(no_repeat.admission.admitted_pages, 0);
        assert_eq!(no_repeat.admission.uploaded_bytes, 0);

        let compatible_camera = AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 20, 40).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap();
        let compatible_requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 3,
            camera: compatible_camera,
            screen_size_points: [256.0, 128.0],
            scissor,
            interaction: &interaction,
        }];
        let (third, third_stats, third_admission) = publish_bundled_frame(
            &device,
            &queue,
            &mut renderer,
            &compatible_requests,
            &ledger,
            Some(second),
            260,
            322,
        );
        assert_eq!(third_admission.admitted_pages, 0);
        assert_eq!(third_stats.geometry_uploaded_bytes, 0);
        assert_eq!(renderer.resident_page_count(), prepared_keys.len());
        assert_eq!(
            renderer
                .pages
                .iter()
                .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                .collect::<Vec<_>>(),
            converged_page_ids
        );
        render_test_frame(&device, &queue, &renderer, &third).unwrap();
    }

    #[test]
    fn only_persistent_budget_failures_report_retryable_gpu_pressure() {
        let budget = LedgerError::BudgetExceeded {
            requested: 4_096,
            charged: 8_192,
            reserved: 0,
            limit: 12_048,
        };
        let expected = GpuBudgetPressure::from_budget_failure(4_096, 8_192, 0, 12_048);
        let admission =
            AlignmentGpuFrameBuildError::Admission(AlignmentGpuPageAdmissionError::Ledger(budget));
        assert_eq!(admission.persistent_budget_pressure(), Some(expected));
        assert_eq!(expected.required_reclaim_bytes(), 240);

        let nested = AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Admission(
            AlignmentGpuPageAdmissionError::Ledger(budget),
        ));
        assert_eq!(nested.persistent_budget_pressure(), Some(expected));
        let prepare =
            AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Ledger(budget));
        assert_eq!(prepare.persistent_budget_pressure(), Some(expected));

        let transient = AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Ledger(
            LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::GpuUpload,
                requested: 4_096,
                transient: 8_192,
                limit: 10_000,
            },
        ));
        assert_eq!(transient.persistent_budget_pressure(), None);
        assert_eq!(
            AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::InvalidFrameGeometry)
                .persistent_budget_pressure(),
            None
        );
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
