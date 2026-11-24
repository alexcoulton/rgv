use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
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
    AllocationRetirementToken, ClipEdgeToken, CpuAlignmentLabelFrame, CpuAlignmentProjector,
    CpuProjectionError, LedgerError, PinnedAllocationLease, ReservationKind, SceneRevision,
    TransientResourceClass, TransientResourceReservation,
};
use thiserror::Error;

use crate::paged_cache::{self, CachedFrameSlot, CachedGpuPage, PageTableError, PagedGpuCache};
use crate::palette::{AlignmentPalette, PaletteBinding, PaletteColor, PaletteToken};
use crate::{AlignmentLedgerPolicyView, GpuBudgetPressure, GpuReclaimGoal, GpuReclaimReport};

/// Two slots are sufficient for a strict present-next/drop-previous frame boundary: the
/// currently presented frame owns one slot while the next camera frame is captured into the
/// other. A third concurrently retained candidate is a coordinator bug/back-pressure condition,
/// not a reason to grow the shared GPU ledger during interaction.
const ALIGNMENT_CAMERA_FRAME_SLOT_COUNT: usize = 2;
/// Largest page-local offset, in bases, an instance may carry: the bound within which the
/// shaders' integer cancellation (`project_genomic_x`) cannot overflow. Pages are canonical
/// tiles a few kilobases wide, so real offsets are five orders of magnitude below it.
pub const MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET: u64 = 700_000_000;
/// Rows addressable inside one page (row block): twenty bits.
pub const MAXIMUM_ALIGNMENT_GPU_PAGE_ROWS: u32 = 1 << ROW_BITS;

const ROW_BITS: u32 = 20;
const ROW_MASK: u32 = (1 << ROW_BITS) - 1;
const SHAPE_SHIFT: u32 = ROW_BITS;
const SHAPE_MASK: u32 = 0xF;
const PALETTE_SHIFT: u32 = 24;
/// `start` and `length` are each at most [`MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET`], under 2^30, so
/// their top two bits are free. Together they carry the four-bit distance, in whole bases, from
/// an arrow-cut shape's tip-side edge to its read's tip (see [`AlignmentGpuInstance::new`]).
const SPAN_BITS: u32 = 30;
const SPAN_MASK: u32 = (1 << SPAN_BITS) - 1;
const TIP_BASES_LOW_MASK: u32 = 0b11;
const _: () = assert!(MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET < 1 << SPAN_BITS);
/// Largest distance from the tip an instance can carry (the semantic token stops at nine).
pub const MAXIMUM_ALIGNMENT_GPU_TIP_BASES: u8 = 15;

/// Persistent GPU representation of one non-text semantic primitive: sixteen bytes.
///
/// Offsets are whole bases from the page start, so the shader can cancel them against the
/// camera origin in integer arithmetic (the REND-7 contract); camera and page origins are
/// supplied independently. The colour is a palette index plus a parameter word, never a colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct AlignmentGpuInstance {
    /// Whole bases from the page start to the primitive's start (bits 0..30); bits 30..32 are
    /// the low two bits of the distance from the tip (direction-terminal shapes only).
    start: u32,
    /// Whole bases covered, never zero (bits 0..30); bits 30..32 are the high two bits of the
    /// distance from the tip.
    length: u32,
    /// Row offset inside the page (bits 0..20), shape (bits 20..24) and the eight-bit palette
    /// index of [`PaletteColor::encode`] (bits 24..32).
    row_shape_palette: u32,
    /// Colour parameter word of [`PaletteColor::encode`].
    color_parameter: u32,
}

impl AlignmentGpuInstance {
    /// Packs one instance, refusing any value its fields cannot hold exactly.
    ///
    /// `bases_from_tip` is meaningful for the direction-terminal shapes only: how many whole
    /// bases lie between the shape's tip-side edge and its read's tip (zero when it ends on the
    /// tip; at most [`MAXIMUM_ALIGNMENT_GPU_TIP_BASES`]). The shader then cuts it by the read's
    /// one arrowhead diagonal, measured from that tip.
    // The page-admission error type every packing path already returns.
    #[allow(clippy::result_large_err)]
    pub fn new(
        start_offset: u64,
        end_offset: u64,
        row_offset: u32,
        shape: AlignmentGpuShape,
        color: PaletteColor,
        bases_from_tip: u8,
    ) -> Result<Self, AlignmentGpuPageAdmissionError> {
        if start_offset >= end_offset {
            return Err(AlignmentGpuPageAdmissionError::InvalidSemanticSpan {
                start: start_offset,
                end: end_offset,
            });
        }
        if end_offset > MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET {
            return Err(AlignmentGpuPageAdmissionError::SemanticOffsetOutOfRange {
                start: start_offset,
                end: end_offset,
                limit: MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET,
            });
        }
        if row_offset >= MAXIMUM_ALIGNMENT_GPU_PAGE_ROWS {
            return Err(AlignmentGpuPageAdmissionError::RowOffsetOutOfRange {
                row_offset,
                limit: MAXIMUM_ALIGNMENT_GPU_PAGE_ROWS,
            });
        }
        if bases_from_tip > MAXIMUM_ALIGNMENT_GPU_TIP_BASES {
            return Err(AlignmentGpuPageAdmissionError::TipDistanceOutOfRange {
                bases_from_tip,
                limit: MAXIMUM_ALIGNMENT_GPU_TIP_BASES,
            });
        }
        let (palette_index, color_parameter) = color.encode();
        let tip = u32::from(bases_from_tip);
        Ok(Self {
            // Both fit in thirty bits: `end_offset` is at most 7e8.
            start: start_offset as u32 | ((tip & TIP_BASES_LOW_MASK) << SPAN_BITS),
            length: (end_offset - start_offset) as u32 | ((tip >> 2) << SPAN_BITS),
            row_shape_palette: row_offset
                | ((shape as u32) << SHAPE_SHIFT)
                | (u32::from(palette_index) << PALETTE_SHIFT),
            color_parameter,
        })
    }

    /// Whole bases from the page start to the primitive's start.
    #[must_use]
    pub const fn start_offset(&self) -> u64 {
        (self.start & SPAN_MASK) as u64
    }

    /// Whole bases from the page start to the primitive's end (exclusive).
    #[must_use]
    pub const fn end_offset(&self) -> u64 {
        (self.start & SPAN_MASK) as u64 + (self.length & SPAN_MASK) as u64
    }

    /// Whole bases from a direction-terminal shape's tip-side edge to its read's tip.
    #[must_use]
    pub const fn bases_from_tip(&self) -> u32 {
        (self.start >> SPAN_BITS) | ((self.length >> SPAN_BITS) << 2)
    }

    #[must_use]
    pub const fn row_offset(&self) -> u32 {
        self.row_shape_palette & ROW_MASK
    }

    /// The [`AlignmentGpuShape`] discriminant.
    #[must_use]
    pub const fn shape(&self) -> u32 {
        (self.row_shape_palette >> SHAPE_SHIFT) & SHAPE_MASK
    }

    /// The instance's colour before a palette is applied.
    #[must_use]
    pub const fn palette_color(&self) -> PaletteColor {
        PaletteColor::decode(
            (self.row_shape_palette >> PALETTE_SHIFT) as u8,
            self.color_parameter,
        )
    }
}

/// Camera-frame-local interaction state for one immutable alignment primitive. Geometry stays in
/// its canonical page buffer; this four-byte companion tells the interactive shader how to tint
/// that instance without copying its sixteen-byte geometry into a second global overlay buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
struct AlignmentGpuInteractionInstance {
    flags: u32,
}

impl AlignmentGpuInteractionInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![2 => Uint32];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

impl AlignmentGpuInstance {
    fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        // `[start, length]` and `[row | shape | palette, colour parameter]`.
        const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Uint32x2, 1 => Uint32x2];
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
    /// An insertion's I-beam (bar plus top and bottom serifs) centred on the start edge of its
    /// one-base span; see `rgv_alignment_scene::point_marker_extent`.
    PointMarker = 2,
    /// A shape cut by its read's direction arrowhead (pointing right, or left): the one-base
    /// terminal, a block or base cell ending on the tip, or a cell or block piece
    /// [`AlignmentGpuInstance::bases_from_tip`] bases short of it.
    DirectionTerminalRight = 3,
    DirectionTerminalLeft = 4,
    /// The same I-beam centred on the end edge of its one-base span (a read's right boundary).
    PointMarkerAtEnd = 5,
    /// Clip cut edges, `6 + at_end + 2 x hard + 4 x behind_arrow` (see [`ClipEdgeToken`] and
    /// `rgv_alignment_scene::clip_edge_extent`): a soft clip's sawtooth or a hard clip's bar,
    /// just inside the start (or end) edge of the one-base span, moved inside the arrowhead
    /// when the read's direction arrow shares that boundary.
    SoftClipEdgeStart = 6,
    SoftClipEdgeEnd = 7,
    HardClipEdgeStart = 8,
    HardClipEdgeEnd = 9,
    SoftClipEdgeStartBehindArrow = 10,
    SoftClipEdgeEndBehindArrow = 11,
    HardClipEdgeStartBehindArrow = 12,
    HardClipEdgeEndBehindArrow = 13,
    /// A soft-clipped ghost base cell: a rectangle drawn in the translucent ghost tint of its
    /// read's colour (`AlignmentPalette::clip_ghost_color`).
    ClipGhostCell = 14,
}

impl AlignmentGpuShape {
    const FIRST_CLIP_EDGE: u32 = Self::SoftClipEdgeStart as u32;
    const LAST_CLIP_EDGE: u32 = Self::HardClipEdgeEndBehindArrow as u32;

    /// The shape drawing `edge`.
    #[must_use]
    pub const fn clip_edge(edge: ClipEdgeToken) -> Self {
        match (edge.behind_arrow, edge.hard, edge.at_end) {
            (false, false, false) => Self::SoftClipEdgeStart,
            (false, false, true) => Self::SoftClipEdgeEnd,
            (false, true, false) => Self::HardClipEdgeStart,
            (false, true, true) => Self::HardClipEdgeEnd,
            (true, false, false) => Self::SoftClipEdgeStartBehindArrow,
            (true, false, true) => Self::SoftClipEdgeEndBehindArrow,
            (true, true, false) => Self::HardClipEdgeStartBehindArrow,
            (true, true, true) => Self::HardClipEdgeEndBehindArrow,
        }
    }

    /// The clip edge a packed shape discriminant draws, if it is one.
    #[must_use]
    pub const fn clip_edge_token(shape: u32) -> Option<ClipEdgeToken> {
        if shape < Self::FIRST_CLIP_EDGE || shape > Self::LAST_CLIP_EDGE {
            return None;
        }
        let bits = shape - Self::FIRST_CLIP_EDGE;
        Some(ClipEdgeToken {
            at_end: bits & 1 != 0,
            hard: bits & 2 != 0,
            behind_arrow: bits & 4 != 0,
        })
    }
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
    let start_offset = instance.start_offset() as f64;
    let end_offset = instance.end_offset() as f64;
    let mut x = camera.clip().x
        + (page_start as f64 + start_offset - camera.exact_start()) / camera.bases_per_pixel();
    let mut right = camera.clip().x
        + (page_start as f64 + end_offset - camera.exact_start()) / camera.bases_per_pixel();
    let row = f64::from(page_row_start.saturating_add(instance.row_offset()));
    let row_top = camera.clip().y
        + (row - camera.row_scroll()) * camera.row_height()
        + (camera.row_height() - camera.read_height()) * 0.5;
    // Minimum-size policy shared with `alignment_scene.wgsl` and the CPU projector: at least one
    // point (>= one physical pixel) in both axes for every shape.
    let (mut y, mut height) = (row_top, camera.read_height().max(1.0));
    if instance.shape() == AlignmentGpuShape::EventLine as u32 {
        y = row_top + camera.read_height() * 0.45;
        height = (camera.read_height() * 0.10).max(1.0);
    }
    let shape = instance.shape();
    // Point markers and clip edges are hairlines, then hidden, as the camera zooms out (see
    // `rgv_alignment_scene::boundary_mark_detail`); a hidden mark emits no quad.
    let points_per_base = 1.0 / camera.bases_per_pixel();
    if shape == AlignmentGpuShape::PointMarker as u32
        || shape == AlignmentGpuShape::PointMarkerAtEnd as u32
    {
        let (left, width) = rgv_alignment_scene::point_marker_extent(
            x,
            right,
            shape == AlignmentGpuShape::PointMarkerAtEnd as u32,
            points_per_base,
        )?;
        x = left;
        right = left + width;
    } else if let Some(edge) = AlignmentGpuShape::clip_edge_token(shape) {
        let (left, width) =
            rgv_alignment_scene::clip_edge_extent(x, right, height, edge, points_per_base)?;
        x = left;
        right = left + width;
    } else if shape == AlignmentGpuShape::DirectionTerminalRight as u32
        || shape == AlignmentGpuShape::DirectionTerminalLeft as u32
    {
        // The tip-side edge stays put (on the read's end, or `bases_from_tip` bases short of
        // it); the one-point minimum grows away from the tip.
        let (left, width) = rgv_alignment_scene::directional_extent(
            x,
            right,
            shape == AlignmentGpuShape::DirectionTerminalRight as u32,
        );
        x = left;
        right = left + width;
    } else if shape == AlignmentGpuShape::ClipGhostCell as u32
        && !rgv_alignment_scene::clip_ghost_visible(points_per_base)
    {
        // Ghost cells appear only together with their letters; zoomed out they emit no quad.
        return None;
    } else {
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
        pack_page(page)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct AlignmentGpuCameraUniform {
    screen_size: [f32; 2],
    clip_origin: [f32; 2],
    points_per_base: [f32; 2],
    /// Whole bases from the camera start to the page start, rounded down.
    page_start_whole: i32,
    /// The remainder, in `[0, 1)`.
    page_start_fraction: f32,
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
    /// Instances submitted for drawing, after culling each page to the camera's visible rows
    /// and genomic window.
    pub base_instances: usize,
    /// Instances of the frame's pages before culling: what `base_instances` was when every
    /// page was drawn whole.
    pub unculled_instances: usize,
    /// Instanced draw calls the frame issues (each a contiguous run of one page's instances).
    pub draw_ranges: usize,
    /// CPU time spent projecting the frame's text labels.
    pub label_projection_micros: u64,
    /// CPU time spent preparing the frame's draws apart from text: culling, interaction flags,
    /// camera uniforms and their uploads.
    pub instance_preparation_micros: u64,
    /// Primitive interaction flags which are non-zero. The field retains its historical name for
    /// diagnostic/API compatibility; no duplicate geometry overlay is constructed.
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
    /// The semantic page is consumed entirely by the CPU label projector and therefore has no
    /// GPU residency requirement.
    NoGpuGeometry,
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

/// Pins a complete set of prepared drawable-page GPU allocations. The token is created only after
/// every unique GPU page in the requested scenes is resident; dropping it makes those allocations
/// eligible for the renderer's normal LRU eviction again. CPU-only label/empty pages require no
/// lease.
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
    buffer: wgpu::Buffer,
    instance_count: u32,
    allocation: AllocationLease,
    last_used_revision: u64,
}

impl CachedGpuPage for GpuAlignmentPage {
    type Key = AlignmentPageKey;

    fn key(&self) -> AlignmentPageKey {
        self.key
    }

    fn last_used_revision(&self) -> u64 {
        self.last_used_revision
    }

    fn set_last_used_revision(&mut self, revision: u64) {
        self.last_used_revision = revision;
    }

    fn allocation(&self) -> Option<&AllocationLease> {
        Some(&self.allocation)
    }
}

#[derive(Clone, Debug)]
struct PreparedAlignmentDraw {
    page: AlignmentPageKey,
    uniform_offset: u32,
    scissor: AlignmentGpuScissor,
    interaction_buffer_index: Option<u32>,
    /// The page's instances this camera can see, ascending and disjoint (see
    /// [`culled_draw_ranges`]); drawn in this order, they keep the page's paint order.
    ranges: Arc<[AlignmentGpuDrawRange]>,
}

/// One instanced draw of a contiguous run of a page's instances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlignmentGpuDrawRange {
    /// First instance in the page's immutable buffer.
    first: u32,
    count: u32,
    /// Position of the first instance's flags in the draw's interaction buffer: equal to
    /// `first` for a page-sized buffer, the running total for a compacted one.
    interaction_first: u32,
}

/// Off-screen instances between two visible runs of a page are drawn rather than split into
/// another draw when there are at most this many of them: a draw call costs more than a few
/// quads the scissor discards.
const ALIGNMENT_DRAW_RANGE_MERGE_GAP: u32 = 32;

/// The ranges of `page`'s instances that can reach `camera`'s clip, merged across gaps of at
/// most [`ALIGNMENT_DRAW_RANGE_MERGE_GAP`] primitives, as semantic primitive ranges and the
/// matching instance ranges. With `culling` off, every instance.
///
/// The rows are the camera's visible rows widened by the rows a drawn read can spill into (a
/// read is at least one point tall however short its row), and the genomic window is
/// [`AlignmentCamera::projection_window`]: every instance outside both projects to nothing in
/// the clip, so drawing the rest, in page order, leaves every pixel as drawing all of them
/// would. The ranges keep page order, so overlapping instances still paint in the same order.
fn culled_draw_ranges(
    page: &AlignmentSemanticPage,
    camera: AlignmentCamera,
    instance_count: u32,
    culling: bool,
) -> Vec<(std::ops::Range<u32>, std::ops::Range<u32>)> {
    if !culling {
        let primitives = u32::try_from(page.primitives().len()).unwrap_or(u32::MAX);
        return vec![(0..primitives, 0..instance_count)];
    }
    let visible = camera.visible_rows();
    let spill = (camera.read_height().max(1.0) / camera.row_height()).ceil() as u32 + 1;
    let rows = visible.start.saturating_sub(spill)..visible.end.saturating_add(spill);
    let mut semantic = Vec::new();
    page.window_primitive_ranges(rows, camera.projection_window(), &mut semantic);
    let mut merged: Vec<std::ops::Range<u32>> = Vec::with_capacity(semantic.len());
    for range in semantic {
        match merged.last_mut() {
            Some(last)
                if range.start.saturating_sub(last.end) <= ALIGNMENT_DRAW_RANGE_MERGE_GAP =>
            {
                last.end = range.end;
            }
            _ => merged.push(range),
        }
    }
    merged
        .into_iter()
        .filter_map(|range| {
            let instances = page.drawable_index(range.start).min(instance_count)
                ..page.drawable_index(range.end).min(instance_count);
            (instances.start < instances.end).then_some((range, instances))
        })
        .collect()
}

#[derive(Clone, Debug)]
struct PendingAlignmentDraw {
    page: AlignmentPageKey,
    camera: AlignmentCamera,
    screen_size: [f32; 2],
    scissor: AlignmentGpuScissor,
    page_start: u64,
    page_row_start: u32,
    interaction_buffer_index: Option<u32>,
    base_instance_count: u32,
    /// Instances this draw submits after culling.
    drawn_instance_count: u32,
    ranges: Arc<[AlignmentGpuDrawRange]>,
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
/// this token from the exact ordered immutable scene identities and complete drawable-page
/// manifest. CPU-only label/empty pages remain in the semantic scene but never enter this GPU
/// authority. Camera, row scroll, scissor, hover, and selection are deliberately absent: those
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

/// How a foreground replacement treats visible pages that are not yet resident.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentGpuVisibleAdmission {
    /// Every visible page is admitted or the replacement is refused. This is the strict
    /// behaviour of the `Complete` memory policy.
    #[default]
    Complete,
    /// Draw what fits. Missing visible pages are admitted in priority order (centre of the
    /// viewport outward, then by row block) while their buffers fit in this many bytes; the rest
    /// are deferred and the frame is marked partially resident. Pages that are already resident
    /// cost nothing and are always drawn, so nothing visible is ever given up to admit a less
    /// central page.
    WithinPageBytes(usize),
}

/// Immutable lease for one fully prepared multi-scene GPU submission. All mandatory page
/// allocations and renderer-owned frame buffers remain pinned through command encoding.
#[derive(Debug)]
pub struct AlignmentGpuFrame {
    /// Visible pages this frame does not draw because they were not resident; sorted by key.
    deferred_pages: Arc<[AlignmentPageKey]>,
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
    /// Ledger admission of the draws' culled instance ranges, held as long as they are.
    _draw_range_resources: TransientResourceReservation,
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

    /// Visible pages this frame does not draw because the GPU budget could not hold them when it
    /// was prepared, sorted by key. Empty for a complete frame.
    #[must_use]
    pub fn deferred_page_keys(&self) -> &[AlignmentPageKey] {
        &self.deferred_pages
    }

    /// Whether some visible page is missing from this frame. A partially resident frame is a
    /// truthful picture of the pages it has, but it is not the complete presentation.
    #[must_use]
    pub fn is_partially_resident(&self) -> bool {
        !self.deferred_pages.is_empty()
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

/// Reusable immutable-for-the-duration-of-a-frame camera/interaction slot. The renderer keeps one
/// cache lease, while every prepared frame keeps an `Arc` and pins the charged GPU capacity. A
/// slot is writable only when the renderer is its sole owner, so preparing a replacement frame
/// can never overwrite buffers still referenced by an in-flight submission.
#[derive(Debug)]
struct GpuAlignmentFrameBuffers {
    camera_buffer: Option<wgpu::Buffer>,
    camera_bind_group: Option<wgpu::BindGroup>,
    camera_allocation: Option<AllocationLease>,
    camera_capacity: usize,
    /// One compact interaction buffer per possible visible canonical page. Physical buffers stay
    /// page-sized, so no predicate can create a monolithic allocation above the device limit.
    interaction_buffers: Vec<wgpu::Buffer>,
    interaction_allocation: Option<AllocationLease>,
    interaction_capacity: usize,
    /// Retained CPU-frame capacity charged with this physical camera slot. The renderer owns the
    /// lease and each in-flight frame pins it, exactly like the GPU buffers above.
    frame_allocation: AllocationLease,
    frame_capacity: usize,
    capture_ring: bool,
}

impl CachedFrameSlot for GpuAlignmentFrameBuffers {
    fn frame_allocation(&self) -> &AllocationLease {
        &self.frame_allocation
    }

    fn capture_ring(&self) -> bool {
        self.capture_ring
    }

    fn gpu_allocations(&self) -> [Option<&AllocationLease>; 2] {
        [
            self.camera_allocation.as_ref(),
            self.interaction_allocation.as_ref(),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct PlannedAlignmentFrameSlot {
    camera_slot: Option<AllocationBundleSlot>,
    interaction_slot: Option<AllocationBundleSlot>,
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
    /// Visible pages the frame draws: every resident one plus the admitted missing ones.
    mandatory_pages: Vec<Arc<AlignmentSemanticPage>>,
    /// Visible pages left out by [`AlignmentGpuVisibleAdmission::WithinPageBytes`]; sorted.
    deferred_pages: Arc<[AlignmentPageKey]>,
    prepared_page_keys: Arc<[AlignmentPageKey]>,
    missing_pages: Vec<PlannedAlignmentPage>,
    demands: Vec<AllocationBundleDemand>,
    page_table_slot: Option<AllocationBundleSlot>,
    page_table_requested_capacity: usize,
    page_table_maximum_bytes: usize,
    camera_capacity: usize,
    camera_bytes: usize,
    interaction_capacity: usize,
    interaction_buffer_bytes: usize,
    interaction_buffer_count: usize,
    interaction_total_bytes: usize,
    frame_slots: [PlannedAlignmentFrameSlot; ALIGNMENT_CAMERA_FRAME_SLOT_COUNT],
    maximum_frame_bytes: usize,
    request_count: usize,
    mandatory_draws: usize,
    maximum_interactions: usize,
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

    /// Bytes of page buffers this plan will create (the admitted missing pages).
    #[must_use]
    pub fn missing_page_bytes(&self) -> usize {
        self.missing_pages
            .iter()
            .fold(0, |total, page| total.saturating_add(page.maximum_bytes))
    }

    /// Keys of the missing pages this plan admits, in page-key order.
    #[must_use]
    pub fn admitted_page_keys(&self) -> Vec<AlignmentPageKey> {
        self.missing_pages
            .iter()
            .map(|page| page.page.key())
            .collect()
    }

    /// Visible pages this plan leaves out, sorted by key. Empty for a complete plan.
    #[must_use]
    pub fn deferred_page_keys(&self) -> &[AlignmentPageKey] {
        &self.deferred_pages
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
    slot: AllocationBundleSlot,
    buffer: wgpu::Buffer,
    instance_count: u32,
    charged_capacity: usize,
}

struct StagedAlignmentFrameBuffers {
    camera_buffer: Option<wgpu::Buffer>,
    camera_bind_group: Option<wgpu::BindGroup>,
    camera_slot: Option<AllocationBundleSlot>,
    camera_capacity: usize,
    interaction_buffers: Vec<wgpu::Buffer>,
    interaction_slot: Option<AllocationBundleSlot>,
    interaction_capacity: usize,
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
    unculled_instances: usize,
    draw_ranges: usize,
    draw_range_resources: TransientResourceReservation,
    timings: AlignmentGpuFrameTimings,
    text_labels: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
}

/// CPU time a frame's preparation took, by phase; see [`AlignmentGpuPrepareStats`].
#[derive(Clone, Copy, Debug, Default)]
struct AlignmentGpuFrameTimings {
    label_projection_micros: u64,
    instance_preparation_micros: u64,
}

fn duration_micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
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
    unculled_instances: usize,
    draw_ranges: usize,
    timings: AlignmentGpuFrameTimings,
    text_labels: usize,
    label_anchors_examined: usize,
    camera_uniform_bytes: usize,
    geometry_uploaded_bytes: usize,
    resident_gpu_bytes: usize,
}

/// Dedicated persistent renderer for immutable alignment semantic pages. Base geometry is
/// uploaded once per page key. Pan/scroll/compatible zoom update only camera uniforms and a small
/// bounded set of page-local stable-handle interaction buffers.
pub struct AlignmentGpuRenderer {
    pipeline: wgpu::RenderPipeline,
    interaction_pipeline: wgpu::RenderPipeline,
    camera_layout: wgpu::BindGroupLayout,
    camera_stride: usize,
    /// Semantic palette uniform (bind group 1). Outside the page ledger: it is one fixed 2 KiB
    /// buffer per renderer, like the pipelines.
    palette: PaletteBinding,
    frame_buffers: Vec<Arc<GpuAlignmentFrameBuffers>>,
    /// Sorted device-page table, its ledger charge and the online LRU reclaim authority.
    cache: PagedGpuCache<GpuAlignmentPage>,
    revision: u64,
    device_generation: u64,
    pending_geometry_uploaded_bytes: usize,
    content_manifest_builds: AtomicU64,
    content_manifest_pages_enumerated: AtomicU64,
    prefetch_authority_keys_inspected: AtomicU64,
    reclaim_authority_keys_inspected: AtomicU64,
    /// Whether frames draw only the instances their camera can see (the default) rather than
    /// every instance of every visible page. Both draw identical pixels.
    instance_culling: bool,
}

impl AlignmentGpuRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self::with_palette(device, target_format, &AlignmentPalette::light())
    }

    /// The palette instances are currently resolved against.
    #[must_use]
    pub const fn palette(&self) -> &AlignmentPalette {
        self.palette.palette()
    }

    /// Recolours every resident and future page. This writes the 2 KiB palette uniform and
    /// nothing else: page buffers hold palette indices, so none is re-uploaded. Returns whether
    /// the palette changed.
    pub fn set_palette(&mut self, queue: &wgpu::Queue, palette: &AlignmentPalette) -> bool {
        self.palette.set(queue, palette)
    }

    /// Number of palette uniform uploads since this renderer was created.
    #[must_use]
    pub const fn palette_uploads(&self) -> u64 {
        self.palette.uploads()
    }

    #[must_use]
    pub fn with_palette(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        palette: &AlignmentPalette,
    ) -> Self {
        let shader = crate::shaders::ALIGNMENT_SCENE.create_module(device);
        let palette_layout = PaletteBinding::layout(device);
        let palette = PaletteBinding::new(device, &palette_layout, palette);
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
            bind_group_layouts: &[Some(&camera_layout), Some(&palette_layout)],
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
        let interaction_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV interactive alignment semantic pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("interaction_vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    AlignmentGpuInstance::vertex_buffer_layout(),
                    AlignmentGpuInteractionInstance::vertex_buffer_layout(),
                ],
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
            interaction_pipeline,
            camera_layout,
            camera_stride: aligned_uniform_stride(device),
            palette,
            frame_buffers: Vec::new(),
            cache: PagedGpuCache::default(),
            revision: 0,
            device_generation: 1,
            pending_geometry_uploaded_bytes: 0,
            content_manifest_builds: AtomicU64::new(0),
            content_manifest_pages_enumerated: AtomicU64::new(0),
            prefetch_authority_keys_inspected: AtomicU64::new(0),
            reclaim_authority_keys_inspected: AtomicU64::new(0),
            instance_culling: true,
        }
    }

    /// Chooses whether frames prepared from now on draw only the instances their camera can
    /// see (`true`, the default) or every instance of every visible page. The pictures are
    /// identical; the switch exists to prove it and to measure the difference.
    pub fn set_instance_culling(&mut self, enabled: bool) {
        self.instance_culling = enabled;
    }

    #[must_use]
    pub const fn instance_culling(&self) -> bool {
        self.instance_culling
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
        self.plan_replacement_within(
            requests,
            ledger,
            first_bundle_slot,
            AlignmentGpuVisibleAdmission::Complete,
        )
    }

    /// [`Self::plan_replacement`] with an explicit rule for visible pages that are not yet
    /// resident. Under [`AlignmentGpuVisibleAdmission::WithinPageBytes`] the plan admits the
    /// highest-priority missing pages that fit, defers the rest, and the frame it publishes
    /// reports them through [`AlignmentGpuFrame::deferred_page_keys`]. Planning the same requests
    /// again once capacity has been released admits more, until nothing is deferred.
    // Same error type as `plan_replacement`, which this generalises.
    #[allow(clippy::too_many_lines, clippy::result_large_err)]
    pub fn plan_replacement_within(
        &self,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &impl AlignmentLedgerPolicyView,
        first_bundle_slot: AllocationBundleSlot,
        admission: AlignmentGpuVisibleAdmission,
    ) -> Result<AlignmentGpuReplacementPlan, AlignmentGpuReplacementError> {
        let mut mandatory_pages = mandatory_alignment_pages(requests)?;
        let deferred_pages: Arc<[AlignmentPageKey]> = match admission {
            AlignmentGpuVisibleAdmission::Complete => Arc::from([]),
            AlignmentGpuVisibleAdmission::WithinPageBytes(page_bytes) => {
                let mut deferred = Vec::new();
                let mut admitted_bytes = 0_usize;
                let mut exhausted = false;
                for key in visible_page_admission_order(requests) {
                    if self.page_index(key).is_some() {
                        continue;
                    }
                    let page = mandatory_pages
                        .iter()
                        .find(|page| page.key() == key)
                        .expect("the admission order enumerates the mandatory pages");
                    let bytes = gpu_buffer_capacity::<AlignmentGpuInstance>(
                        non_text_primitive_count(page),
                    )?;
                    // A strict prefix of the priority order: once one page does not fit, no
                    // less central page is admitted in its place.
                    if exhausted || admitted_bytes.saturating_add(bytes) > page_bytes {
                        exhausted = true;
                        deferred.push(key);
                    } else {
                        admitted_bytes = admitted_bytes.saturating_add(bytes);
                    }
                }
                deferred.sort_unstable();
                deferred.into()
            }
        };
        mandatory_pages.retain(|page| !is_deferred(&deferred_pages, page.key()));
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
        let mut maximum_interactions = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        for request in requests {
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request
                .scene
                .visible_pages(interval, rows)
                .into_iter()
                .filter(|page| alignment_page_has_gpu_geometry(page))
                .filter(|page| !is_deferred(&deferred_pages, page.key()))
            {
                mandatory_draws = mandatory_draws.saturating_add(1);
                if self.page_index(page.key()).is_some() {
                    page_cache_hits = page_cache_hits.saturating_add(1);
                } else {
                    page_cache_misses = page_cache_misses.saturating_add(1);
                }
                if !request.interaction.is_empty() {
                    maximum_interactions = maximum_interactions.saturating_add(
                        page.primitives()
                            .iter()
                            .filter(|primitive| {
                                let token = request.interaction.token(primitive.handle);
                                !matches!(primitive.component, AlignmentComponent::Label { .. })
                                    && interaction_token_is_active(token)
                            })
                            .count(),
                    );
                }
            }
        }
        u32::try_from(maximum_interactions)
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

        let required_page_capacity = self.cache.pages.len().saturating_add(missing_pages.len());
        let (page_table_slot, page_table_requested_capacity, page_table_maximum_bytes) =
            if required_page_capacity > self.cache.page_table_capacity {
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
        let (camera_envelope_draws, maximum_page_interaction_instances) =
            requests
                .iter()
                .fold((0_usize, 0_usize), |(draw_total, page_maximum), request| {
                    let visible_rows = request.camera.visible_rows();
                    // Capacity follows the largest number of canonical pages that one camera at this
                    // scale can expose anywhere inside the prepared horizon. Passing the horizon's
                    // own length here would multiply a viewport requirement by every prefetched tile
                    // and recreate the former unbounded predicate-highlight buffer.
                    let camera_pages = request.scene.camera_page_envelope(
                        request.camera.interval().len(),
                        visible_rows.end.saturating_sub(visible_rows.start),
                    );
                    (
                        draw_total.saturating_add(camera_pages),
                        page_maximum.max(request.scene.maximum_non_label_primitives_per_page()),
                    )
                });
        if camera_envelope_draws > 0 {
            u32::try_from((camera_envelope_draws - 1).saturating_mul(self.camera_stride))
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        u32::try_from(maximum_page_interaction_instances)
            .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
        let camera_capacity = (camera_envelope_draws > 0)
            .then(|| next_capacity(camera_envelope_draws))
            .transpose()?
            .unwrap_or(0);
        let camera_bytes = camera_capacity.saturating_mul(self.camera_stride);
        let interaction_capacity = (maximum_page_interaction_instances > 0)
            .then(|| next_capacity(maximum_page_interaction_instances))
            .transpose()?
            .unwrap_or(0);
        let interaction_buffer_bytes = interaction_capacity
            .checked_mul(size_of::<AlignmentGpuInteractionInstance>())
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        let interaction_buffer_count =
            usize::from(interaction_buffer_bytes > 0).saturating_mul(camera_envelope_draws);
        let interaction_total_bytes = interaction_buffer_bytes
            .checked_mul(interaction_buffer_count)
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
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
            let interaction_slot = if interaction_total_bytes > 0 {
                let slot = take_bundle_slot(&mut next_slot)?;
                demands.push(AllocationBundleDemand::new(
                    slot,
                    AllocationClass::GpuPage,
                    interaction_total_bytes,
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
                interaction_slot,
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
            deferred_pages,
            prepared_page_keys,
            missing_pages,
            demands,
            page_table_slot,
            page_table_requested_capacity,
            page_table_maximum_bytes,
            camera_capacity,
            camera_bytes,
            interaction_capacity,
            interaction_buffer_bytes,
            interaction_buffer_count,
            interaction_total_bytes,
            frame_slots,
            maximum_frame_bytes,
            request_count: requests.len(),
            mandatory_draws,
            maximum_interactions,
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
        for page in &self.cache.pages {
            if prepared_keys.contains(&page.key)
                || !exact_retirement_ownership(&page.allocation, &prior_pin_counts)?
            {
                all_pages_retirable = false;
                break;
            }
        }
        let can_retire_table = plan.page_table_slot.is_some()
            && all_pages_retirable
            && self
                .cache
                .page_table_allocation
                .as_ref()
                .map(|allocation| exact_retirement_ownership(allocation, &prior_pin_counts))
                .transpose()?
                .unwrap_or(false);
        let mut retired_page_indices = Vec::new();
        if can_retire_table {
            for page in &self.cache.pages {
                claimable.insert(page.allocation.id());
            }
            claimable.insert(
                self.cache
                    .page_table_allocation
                    .as_ref()
                    .expect("retirable page table was proven present")
                    .id(),
            );
        } else {
            for (index, page) in self.cache.pages.iter().enumerate() {
                if prepared_keys.contains(&page.key) {
                    continue;
                }
                if exact_retirement_ownership(&page.allocation, &prior_pin_counts)? {
                    claimable.insert(page.allocation.id());
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
                buffers.interaction_allocation.as_ref(),
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
        for (index, page) in self.cache.pages.iter().enumerate() {
            if can_retire_table || retired_page_indices.binary_search(&index).is_ok() {
                builder.include_lease(&page.allocation)?;
            }
        }
        if can_retire_table {
            builder.include_lease(
                self.cache
                    .page_table_allocation
                    .as_ref()
                    .expect("retirable page table was proven present"),
            )?;
        }
        for index in &retired_frame_buffer_indices {
            let buffers = &self.frame_buffers[*index];
            if let Some(allocation) = &buffers.camera_allocation {
                builder.include_lease(allocation)?;
            }
            if let Some(allocation) = &buffers.interaction_allocation {
                builder.include_lease(allocation)?;
            }
            builder.include_lease(&buffers.frame_allocation)?;
        }
        let claim_set = builder.finish()?;

        // From this point onward every operation is an infallible ownership move. No error can
        // strand the caller without its prior frame or leave a partially detached renderer.
        let prior_frame = prior_frame.take();
        let retired_pages = if can_retire_table {
            std::mem::take(&mut self.cache.pages)
        } else {
            let mut retired = Vec::with_capacity(retired_page_indices.len());
            for index in retired_page_indices.into_iter().rev() {
                retired.push(self.cache.pages.remove(index));
            }
            retired.sort_unstable_by_key(|page| page.key);
            retired
        };
        let retired_page_table = if can_retire_table {
            let capacity = self.cache.page_table_capacity;
            self.cache.page_table_capacity = 0;
            Some((
                self.cache
                    .page_table_allocation
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
            && (self.cache.page_table_allocation.is_some() || !self.cache.pages.is_empty())
        {
            return Err(AlignmentGpuReplacementError::RetirementRestoreConflict);
        }
        if let Some((allocation, capacity)) = retirement.retired_page_table.take() {
            self.cache.pages = std::mem::take(&mut retirement.retired_pages);
            if self.cache.pages.capacity() != capacity {
                return Err(AlignmentGpuReplacementError::RetirementRestoreConflict);
            }
            self.cache.page_table_allocation = Some(allocation);
            self.cache.page_table_capacity = capacity;
        } else {
            self.cache.pages.append(&mut retirement.retired_pages);
            self.cache.pages.sort_unstable_by_key(|page| page.key);
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
        let mut mandatory_pages = mandatory_alignment_pages(requests)?;
        mandatory_pages.retain(|page| !is_deferred(&plan.deferred_pages, page.key()));
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
        // Interaction state is physically page-local. Validate one page-sized buffer rather than
        // rejecting the aggregate capacity retained by the complete camera ring.
        validate_device_buffer_size(device, plan.interaction_buffer_bytes)?;

        let missing_instances = plan.missing_pages.iter().fold(0_usize, |total, page| {
            total.saturating_add(page.instance_count as usize)
        });
        let uniform_bytes = plan.mandatory_draws.saturating_mul(self.camera_stride);
        let interaction_payload_instances = requests
            .iter()
            .filter(|request| !request.interaction.is_empty())
            .flat_map(|request| {
                request
                    .scene
                    .visible_pages(request.camera.interval(), request.camera.visible_rows())
            })
            .filter(|page| alignment_page_has_gpu_geometry(page))
            .filter(|page| !is_deferred(&plan.deferred_pages, page.key()))
            .fold(0_usize, |total, page| {
                total.saturating_add(non_text_primitive_count(page))
            });
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::GpuUpload,
            uniform_bytes
                .saturating_add(
                    interaction_payload_instances
                        .saturating_mul(size_of::<AlignmentGpuInteractionInstance>()),
                )
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
            let packed = pack_page(&planned.page)?;
            debug_assert_eq!(packed.instances.len(), planned.instance_count as usize);
            debug_assert!(planned.maximum_bytes > 0);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV staged immutable alignment GPU page"),
                size: planned.maximum_bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&packed.instances));
            geometry_uploaded_bytes = geometry_uploaded_bytes.saturating_add(
                packed
                    .instances
                    .len()
                    .saturating_mul(size_of::<AlignmentGpuInstance>()),
            );
            staged_pages.push(StagedAlignmentPage {
                key: planned.page.key(),
                slot: planned.slot,
                buffer,
                instance_count: planned.instance_count,
                charged_capacity: planned.maximum_bytes,
            });
        }

        let page_table = if plan.page_table_slot.is_some() {
            let candidate = Vec::with_capacity(plan.page_table_requested_capacity);
            if candidate.capacity()
                < self
                    .cache
                    .pages
                    .len()
                    .saturating_add(plan.missing_pages.len())
            {
                return Err(AlignmentGpuPageAdmissionError::PageTableCapacity {
                    required: self
                        .cache
                        .pages
                        .len()
                        .saturating_add(plan.missing_pages.len()),
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
            let interaction_buffers = (0..plan.interaction_buffer_count)
                .map(|_| {
                    device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("RGV staged page-local alignment interaction slot"),
                        size: plan.interaction_buffer_bytes as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    })
                })
                .collect();
            staged_buffers.push(StagedAlignmentFrameBuffers {
                camera_buffer,
                camera_bind_group,
                camera_slot: slot.camera_slot,
                camera_capacity: plan.camera_capacity,
                interaction_buffers,
                interaction_slot: slot.interaction_slot,
                interaction_capacity: plan.interaction_capacity,
                frame_slot: slot.frame_slot,
            });
        }

        let labels_started = std::time::Instant::now();
        let mut label_frames = Vec::with_capacity(requests.len());
        for request in requests {
            label_frames.push(CpuAlignmentProjector::project_labels_with_interaction(
                request.scene,
                request.camera_sequence,
                request.camera,
                request.interaction,
            )?);
        }
        let label_projection = labels_started.elapsed();
        let instances_started = std::time::Instant::now();
        let mut draw_range_count = 0_usize;
        let mut page_requests = Vec::with_capacity(plan.mandatory_draws);
        let mut interaction_pages =
            Vec::<Vec<AlignmentGpuInteractionInstance>>::with_capacity(plan.mandatory_draws);
        let mut interaction_instances = 0_usize;
        let scene_revisions = plan.scene_revisions.to_vec();
        let scene_allocations = plan.scene_allocations.to_vec();
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for request in requests {
            camera_sequences.push(request.camera_sequence);
            let rows = request.camera.visible_rows();
            let interval = request.camera.interval();
            for page in request
                .scene
                .visible_pages(interval, rows)
                .into_iter()
                .filter(|page| alignment_page_has_gpu_geometry(page))
                .filter(|page| !is_deferred(&plan.deferred_pages, page.key()))
            {
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
                let interaction_buffer_index = if request.interaction.is_empty() {
                    None
                } else {
                    let (flags, active) = interaction_page_instances(page, request.interaction);
                    if flags.len() != instance_count as usize
                        || flags.len() > plan.interaction_capacity
                    {
                        return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
                    }
                    interaction_instances = interaction_instances.saturating_add(active);
                    let index = u32::try_from(interaction_pages.len())
                        .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
                    interaction_pages.push(flags);
                    Some(index)
                };
                // A replacement uploads page-sized interaction buffers, so each range reads its
                // flags at its own instance index.
                let ranges =
                    culled_draw_ranges(page, request.camera, instance_count, self.instance_culling)
                        .into_iter()
                        .map(|(_, instances)| AlignmentGpuDrawRange {
                            first: instances.start,
                            count: instances.end - instances.start,
                            interaction_first: instances.start,
                        })
                        .collect::<Arc<[_]>>();
                draw_range_count = draw_range_count.saturating_add(ranges.len());
                page_requests.push(PendingAlignmentDraw {
                    page: page.key(),
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: page.interval().start(),
                    page_row_start: page.row_range().start,
                    interaction_buffer_index,
                    base_instance_count: instance_count,
                    drawn_instance_count: ranges.iter().map(|range| range.count).sum(),
                    ranges,
                });
            }
        }
        if page_requests.len() != plan.mandatory_draws
            || interaction_instances != plan.maximum_interactions
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
                interaction_buffer_index: request.interaction_buffer_index,
                ranges: Arc::clone(&request.ranges),
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
        for (index, instances) in interaction_pages.iter().enumerate() {
            let buffer = initial_buffers
                .interaction_buffers
                .get(index)
                .ok_or(AlignmentGpuReplacementError::RequestChangedAfterPlan)?;
            if !instances.is_empty() {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
            }
        }

        let next_revision = plan.renderer_revision.saturating_add(1);
        let page_pin_count = page_requests
            .iter()
            .filter(|request| {
                self.page(request.page).is_some()
                    || staged_pages.iter().any(|page| page.key == request.page)
            })
            .count();
        let actual_pin_count = page_pin_count
            .saturating_add(usize::from(plan.frame_slots[0].camera_slot.is_some()))
            .saturating_add(usize::from(plan.frame_slots[0].interaction_slot.is_some()))
            .saturating_add(usize::from(
                plan.page_table_slot.is_some() || self.cache.page_table_allocation.is_some(),
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
            if let Some(interaction_slot) = slot.interaction_slot {
                actuals.push(AllocationBundleActual::new(
                    interaction_slot,
                    plan.interaction_total_bytes,
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
            total.saturating_add(request.drawn_instance_count as usize)
        });
        let unculled_instances = page_requests.iter().fold(0_usize, |total, request| {
            total.saturating_add(request.base_instance_count as usize)
        });
        let text_labels = label_frames.iter().map(|labels| labels.labels.len()).sum();
        // The draw ranges live as long as the frame; their bytes are admitted like the label
        // frames' text.
        let draw_range_resources = ledger.reserve_transient(
            TransientResourceClass::CpuProjection,
            draw_range_count.saturating_mul(size_of::<AlignmentGpuDrawRange>()),
            ReservationKind::Transition,
        )?;
        let timings = AlignmentGpuFrameTimings {
            label_projection_micros: duration_micros(label_projection),
            instance_preparation_micros: duration_micros(instances_started.elapsed()),
        };
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
            unculled_instances,
            draw_ranges: draw_range_count,
            draw_range_resources,
            timings,
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
                existing_pins.push(page.allocation.pin()?);
            } else if !staged_keys.contains(&draw.page) {
                return Err(AlignmentGpuReplacementError::RequestChangedAfterPlan);
            }
        }
        if staged.page_table.is_none()
            && let Some(allocation) = &self.cache.page_table_allocation
        {
            existing_pins.push(allocation.pin()?);
        }
        let replaced_table_bytes = if staged.page_table.is_some() {
            self.cache
                .page_table_allocation
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
            let allocation = allocations
                .remove(&page.slot)
                .expect("settled alignment page slot was prevalidated");
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
            let interaction_allocation = buffers.interaction_slot.map(|slot| {
                allocations
                    .remove(&slot)
                    .expect("settled alignment interaction slot was prevalidated")
            });
            let frame_allocation = allocations
                .remove(&buffers.frame_slot)
                .expect("settled alignment frame slot was prevalidated");
            frame_buffers.push(Arc::new(GpuAlignmentFrameBuffers {
                camera_buffer: buffers.camera_buffer,
                camera_bind_group: buffers.camera_bind_group,
                camera_allocation,
                camera_capacity: buffers.camera_capacity,
                interaction_buffers: buffers.interaction_buffers,
                interaction_allocation,
                interaction_capacity: buffers.interaction_capacity,
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
                    .map(|page| &page.allocation)
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
        if let Some(allocation) = &initial_frame_buffers.interaction_allocation {
            existing_pins.push(
                allocation
                    .pin()
                    .expect("newly settled alignment interaction ring must remain pinnable"),
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
            deferred_pages: Arc::clone(&staged.plan.deferred_pages),
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
            _draw_range_resources: staged.draw_range_resources,
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
            unculled_instances: staged.unculled_instances,
            draw_ranges: staged.draw_ranges,
            timings: staged.timings,
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
                self.cache.pages[index].last_used_revision = next_revision;
            }
        }
        if let Some(mut table) = publishable.page_table.take() {
            table.extend(std::mem::take(&mut self.cache.pages));
            table.append(&mut publishable.pages);
            table.sort_unstable_by_key(|page| page.key);
            self.cache.pages = table;
            self.cache.page_table_capacity = self.cache.pages.capacity();
            self.cache.page_table_allocation = publishable.page_table_allocation.take();
        } else {
            self.cache.pages.append(&mut publishable.pages);
            self.cache.pages.sort_unstable_by_key(|page| page.key);
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
            unculled_instances: publishable.unculled_instances,
            draw_ranges: publishable.draw_ranges,
            label_projection_micros: publishable.timings.label_projection_micros,
            instance_preparation_micros: publishable.timings.instance_preparation_micros,
            overlay_instances: publishable.plan.maximum_interactions,
            text_labels: publishable.text_labels,
            label_anchors_examined: publishable.label_anchors_examined,
            page_pin_acquisitions: publishable.plan.mandatory_draws,
            camera_uniform_bytes: publishable.camera_uniform_bytes,
            geometry_uploaded_bytes,
            page_cache_hits: publishable.plan.page_cache_hits,
            page_cache_misses: publishable.plan.page_cache_misses,
            resident_pages: self.cache.pages.len(),
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
        if !alignment_page_has_gpu_geometry(page) {
            return Ok(AlignmentGpuPageAdmission::NoGpuGeometry);
        }
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

    /// Transactionally admits the drawable members of a complete mandatory semantic-page set.
    /// Label-only pages remain owned by the CPU scene and intentionally create no GPU residency.
    /// Every drawable key, device limit, retained reservation, and upload reservation is checked
    /// before the renderer page table is changed. Already-resident members are protected from
    /// admission-time eviction, so success means the whole requested GPU set is resident together;
    /// failure cannot leave a partial new mandatory set published.
    pub fn admit_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pages: &[Arc<AlignmentSemanticPage>],
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<AlignmentGpuBatchAdmission, AlignmentGpuPageAdmissionError> {
        let pages = pages
            .iter()
            .filter(|page| alignment_page_has_gpu_geometry(page))
            .cloned()
            .collect::<Vec<_>>();
        self.admit_pages_protecting(
            device,
            queue,
            &pages,
            &pages,
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
        let pages = pages
            .iter()
            .filter(|page| alignment_page_has_gpu_geometry(page))
            .cloned()
            .collect::<Vec<_>>();
        self.admit_pages_protecting(
            device,
            queue,
            &pages,
            &pages,
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
        let mut seen = HashSet::with_capacity(pages.len());
        for page in pages {
            if !seen.insert(page.key()) {
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
            self.cache.pages.len().saturating_add(missing_pages),
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
            let packed = pack_page(page)?;
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
            debug_assert!(candidate.buffer_capacity > 0);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV immutable alignment GPU page"),
                size: candidate.buffer_capacity as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(
                &buffer,
                0,
                bytemuck::cast_slice(&candidate.packed.instances),
            );
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
                self.cache.pages[index].last_used_revision = revision;
            }
        }
        self.cache.pages.extend(admitted);
        self.cache.pages.sort_unstable_by_key(|page| page.key);
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
        self.capture_resident_camera_frame(device, queue, requests, ledger, &authority, true, &[])
    }

    /// Builds only a camera-dependent frame over a previously minted immutable content
    /// authority. This function cannot enumerate the prepared scene, admit/retire pages, create a
    /// content identity, or reserve a replacement bundle. The `allow_slot_growth` flag is true
    /// only for the initial content publication; production camera capture always supplies false.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn capture_resident_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        content_authority: &AlignmentGpuContentAuthority,
        allow_slot_growth: bool,
        tolerated_missing: &[AlignmentPageKey],
    ) -> Result<(AlignmentGpuFrame, AlignmentGpuPrepareStats), AlignmentGpuPrepareError> {
        // First pass is allocation-free. It proves the complete mandatory set is resident and
        // derives conservative capacities before any frame candidate memory or GPU buffer grows.
        // A visible page the caller already knows to be deferred (a partially resident
        // presentation) is left out of the frame instead of failing the capture.
        let mut deferred_pages = Vec::new();
        let visible_pages = requests
            .iter()
            .map(|request| {
                validate_request(request)?;
                Ok(request
                    .scene
                    .visible_pages(request.camera.interval(), request.camera.visible_rows()))
                .map(|pages| {
                    pages
                        .into_iter()
                        .filter(|page| alignment_page_has_gpu_geometry(page))
                        .filter(|page| {
                            let deferred = self.page_index(page.key()).is_none()
                                && is_deferred(tolerated_missing, page.key());
                            if deferred {
                                deferred_pages.push(page.key());
                            }
                            !deferred
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Result<Vec<_>, AlignmentGpuPrepareError>>()?;
        deferred_pages.sort_unstable();
        deferred_pages.dedup();
        let mut mandatory_pages = 0_usize;
        let mut maximum_page_instances = 0_usize;
        let mut page_cache_hits = 0_usize;
        let mut page_cache_misses = 0_usize;
        // Active interaction instances are counted by the single pass which builds the per-page
        // flag buffers below, rather than by a separate walk of every visible primitive here.
        for page in visible_pages.iter().flatten() {
            mandatory_pages = mandatory_pages.saturating_add(1);
            if self.page_index(page.key()).is_none() {
                page_cache_misses = page_cache_misses.saturating_add(1);
                continue;
            }
            page_cache_hits = page_cache_hits.saturating_add(1);
            maximum_page_instances = maximum_page_instances.max(non_text_primitive_count(page));
        }
        if page_cache_misses > 0 {
            return Err(AlignmentGpuPrepareError::MissingMandatoryPages {
                missing: page_cache_misses,
            });
        }
        let uniform_bytes = mandatory_pages.saturating_mul(self.camera_stride);
        if mandatory_pages > 0 {
            u32::try_from((mandatory_pages - 1).saturating_mul(self.camera_stride))
                .map_err(|_| AlignmentGpuPrepareError::DynamicUniformAddressSpace)?;
        }
        let interaction_payload_bytes = visible_pages
            .iter()
            .zip(requests)
            .filter(|(_, request)| !request.interaction.is_empty())
            .flat_map(|(pages, _)| pages.iter())
            .fold(0_usize, |total, page| {
                total.saturating_add(non_text_primitive_count(page))
            })
            .saturating_mul(size_of::<AlignmentGpuInteractionInstance>());
        let scratch_bytes = uniform_bytes
            .saturating_add(interaction_payload_bytes)
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
        let labels_started = std::time::Instant::now();
        let mut label_frames = Vec::with_capacity(requests.len());
        for request in requests {
            label_frames.push(CpuAlignmentProjector::project_labels_with_interaction(
                request.scene,
                request.camera_sequence,
                request.camera,
                request.interaction,
            )?);
        }
        let label_projection = labels_started.elapsed();
        let instances_started = std::time::Instant::now();
        let (interaction_buffer_count, interaction_buffer_capacity) = if allow_slot_growth {
            let envelope_pages = requests.iter().fold(0_usize, |total, request| {
                let rows = request.camera.visible_rows();
                total.saturating_add(request.scene.camera_page_envelope(
                    request.camera.interval().len(),
                    rows.end.saturating_sub(rows.start),
                ))
            });
            let envelope_page_instances = requests.iter().fold(0_usize, |maximum, request| {
                maximum.max(request.scene.maximum_non_label_primitives_per_page())
            });
            (envelope_pages, envelope_page_instances)
        } else {
            (mandatory_pages, maximum_page_instances)
        };
        let frame_buffers = if allow_slot_growth {
            self.acquire_frame_buffers(
                device,
                ledger,
                mandatory_pages,
                interaction_buffer_count,
                interaction_buffer_capacity,
                maximum_frame_bytes,
            )?
        } else {
            self.acquire_existing_frame_buffers(
                mandatory_pages,
                interaction_buffer_count,
                interaction_buffer_capacity,
                maximum_frame_bytes,
            )?
        };

        let mut page_requests = Vec::with_capacity(mandatory_pages);
        let mut interaction_pages = Vec::<Vec<AlignmentGpuInteractionInstance>>::new();
        let mut interaction_instances = 0_usize;
        let mut draw_range_count = 0_usize;
        let mut scene_revisions = Vec::with_capacity(requests.len());
        let mut camera_sequences = Vec::with_capacity(requests.len());
        for (request, pages) in requests.iter().zip(&visible_pages) {
            scene_revisions.push(request.scene.revision());
            camera_sequences.push(request.camera_sequence);
            for page in pages {
                let resident = self
                    .page(page.key())
                    .expect("mandatory page presence was proven in the allocation-free pass");
                if non_text_primitive_count(page) != resident.instance_count as usize {
                    return Err(AlignmentGpuPrepareError::InvalidFrameGeometry);
                }
                let culled = culled_draw_ranges(
                    page,
                    request.camera,
                    resident.instance_count,
                    self.instance_culling,
                );
                // Interaction flags are written for the drawn instances only, compacted in draw
                // order: each range reads its flags from where its run starts in the buffer.
                let mut interaction_first = 0_u32;
                let ranges = culled
                    .iter()
                    .map(|(_, instances)| {
                        let count = instances.end - instances.start;
                        let range = AlignmentGpuDrawRange {
                            first: instances.start,
                            count,
                            interaction_first,
                        };
                        interaction_first = interaction_first.saturating_add(count);
                        range
                    })
                    .collect::<Arc<[_]>>();
                let interaction_buffer_index = if request.interaction.is_empty() {
                    None
                } else {
                    let (flags, active) =
                        interaction_range_instances(page, &culled, request.interaction);
                    if flags.len() != interaction_first as usize {
                        return Err(AlignmentGpuPrepareError::InvalidFrameGeometry);
                    }
                    interaction_instances = interaction_instances.saturating_add(active);
                    let index = u32::try_from(interaction_pages.len())
                        .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;
                    interaction_pages.push(flags);
                    Some(index)
                };
                draw_range_count = draw_range_count.saturating_add(ranges.len());
                page_requests.push(PendingAlignmentDraw {
                    page: page.key(),
                    camera: request.camera,
                    screen_size: request.screen_size_points,
                    scissor: request.scissor,
                    page_start: page.interval().start(),
                    page_row_start: page.row_range().start,
                    interaction_buffer_index,
                    base_instance_count: resident.instance_count,
                    drawn_instance_count: interaction_first,
                    ranges,
                });
            }
        }
        debug_assert_eq!(page_requests.len(), mandatory_pages);
        u32::try_from(interaction_instances)
            .map_err(|_| AlignmentGpuPrepareError::TooManyOverlayPrimitives)?;

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
            let page = &self.cache.pages[page_index];
            if !page_leases.contains_key(&request.page) {
                let (lease, acquired) = content_authority
                    .leases
                    .acquire(request.page, &page.allocation)?;
                debug_assert_eq!(lease.key, request.page);
                page_pin_acquisitions = page_pin_acquisitions.saturating_add(usize::from(acquired));
                page_leases.insert(request.page, lease);
            }
            draws.push(PreparedAlignmentDraw {
                page: request.page,
                uniform_offset: offset_u32,
                scissor: request.scissor,
                interaction_buffer_index: request.interaction_buffer_index,
                ranges: Arc::clone(&request.ranges),
            });
        }
        if let Some(allocation) = &frame_buffers.camera_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &frame_buffers.interaction_allocation {
            pins.push(allocation.pin()?);
        }
        if let Some(allocation) = &self.cache.page_table_allocation {
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
        for (index, instances) in interaction_pages.iter().enumerate() {
            let buffer = frame_buffers.interaction_buffers.get(index).ok_or(
                AlignmentGpuPrepareError::FrameSlotCapacity {
                    required: interaction_pages.len(),
                    available: frame_buffers.interaction_buffers.len(),
                },
            )?;
            if !instances.is_empty() {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(instances));
            }
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
        // The draw ranges live as long as the frame; their bytes are admitted like the label
        // frames' text.
        let draw_range_resources = ledger.reserve_transient(
            TransientResourceClass::CpuProjection,
            draw_range_count.saturating_mul(size_of::<AlignmentGpuDrawRange>()),
            ReservationKind::Transition,
        )?;
        let instance_preparation = instances_started.elapsed();
        let frame = AlignmentGpuFrame {
            deferred_pages: deferred_pages.into(),
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
            _draw_range_resources: draw_range_resources,
        };
        let resident_gpu_bytes = self.resident_gpu_bytes()?;
        let stats = AlignmentGpuPrepareStats {
            scenes: requests.len(),
            mandatory_pages,
            base_instances: page_requests.iter().fold(0_usize, |total, request| {
                total.saturating_add(request.drawn_instance_count as usize)
            }),
            unculled_instances: page_requests.iter().fold(0_usize, |total, request| {
                total.saturating_add(request.base_instance_count as usize)
            }),
            draw_ranges: draw_range_count,
            label_projection_micros: duration_micros(label_projection),
            instance_preparation_micros: duration_micros(instance_preparation),
            overlay_instances: interaction_instances,
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
            resident_pages: self.cache.pages.len(),
            resident_gpu_bytes,
        };
        Ok((frame, stats))
    }

    /// Captures a new camera/scroll/interaction presentation over unchanged resident content.
    ///
    /// This is the no-bundle foreground path. It validates the replacement frame's immutable
    /// authority before touching presentation state, then reuses the bounded camera/interaction ring
    /// used by [`Self::prepare`]. It never admits or retires a semantic page, rebuilds the page
    /// table, reserves a replacement bundle, or uploads base geometry. A typed
    /// [`AlignmentGpuCameraCaptureError::ContentReplacementRequired`] tells the coordinator to use
    /// the existing replacement transaction instead.
    #[allow(clippy::result_large_err)]
    pub fn capture_camera_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        authority: &AlignmentGpuContentAuthority,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
    ) -> Result<(AlignmentGpuFrame, AlignmentGpuPrepareStats), AlignmentGpuCameraCaptureError> {
        self.capture_camera_frame_tolerating(device, queue, authority, requests, ledger, &[])
    }

    /// [`Self::capture_camera_frame`] over a partially resident presentation.
    ///
    /// `tolerated_missing` (sorted by key) names visible pages the caller already knows are
    /// deferred, normally [`AlignmentGpuFrame::deferred_page_keys`] of the frame being replaced.
    /// A missing visible page on that list is left out of the captured frame, which then reports
    /// it as deferred in turn; any other missing page still asks for a content replacement. A
    /// hover or selection change over a partial picture therefore stays a camera-only capture.
    // Same error type as `capture_camera_frame`, which this generalises.
    #[allow(clippy::result_large_err)]
    pub fn capture_camera_frame_tolerating(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        authority: &AlignmentGpuContentAuthority,
        requests: &[AlignmentGpuSceneRequest<'_>],
        ledger: &AllocationLedger,
        tolerated_missing: &[AlignmentPageKey],
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
            .filter(|page| !is_deferred(tolerated_missing, page.key()))
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
        let result = self.capture_resident_camera_frame(
            device,
            queue,
            requests,
            ledger,
            authority,
            false,
            tolerated_missing,
        );
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

    /// Returns the stable full prepared-scene drawable key set used as speculative-generation
    /// authority. Unlike mandatory keys, this does not change during camera-only movement inside
    /// a prepared horizon. CPU-only label/empty pages are intentionally absent.
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

    /// Admits every drawable canonical semantic page already prepared by the requested scenes,
    /// including the horizontal and vertical margins outside the live camera. CPU-only
    /// label/empty pages require no GPU residency. Callers first build and retain the mandatory
    /// visible frame, so failure here cannot blank or invalidate current pixels.
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
                let capacity = resident.allocation.snapshot()?.charged_capacity;
                if let Some(pin) = resident
                    .allocation
                    .pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
                {
                    resident_pins.push(pin);
                    resident_retained_bytes = resident_retained_bytes.saturating_add(capacity);
                } else {
                    cursor.retention_limited = true;
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
            let capacity = resident.allocation.snapshot()?.charged_capacity;
            if let Some(pin) = resident
                .allocation
                .pin_if_total_at_most(cursor.maximum_total_pinned_bytes)?
            {
                cursor.pins.push(pin);
                cursor.retained_bytes = cursor.retained_bytes.saturating_add(capacity);
            } else {
                cursor.retention_limited = true;
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
        self.render_within(pass, frame, [u32::MAX; 2])
    }

    /// Replays `frame` like [`Self::render`], but first intersects every captured scissor with a
    /// render target of `target_size` physical pixels. A frame is captured against one target
    /// size and may be replayed after a resize or into a differently sized capture texture; a
    /// scissor outside the attachment is a wgpu validation error, so draws whose scissor falls
    /// wholly outside the target are skipped and the rest are clipped.
    pub fn render_within<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        frame: &'pass AlignmentGpuFrame,
        target_size: [u32; 2],
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
        pass.set_bind_group(1, self.palette.bind_group(), &[]);
        for draw in frame.draws.iter() {
            let page = self
                .page(draw.page)
                .ok_or(AlignmentGpuRenderError::MissingPinnedPage(draw.page))?;
            let Some([x, y, width, height]) = crate::clamp_scissor_to_target(
                [
                    draw.scissor.x,
                    draw.scissor.y,
                    draw.scissor.width,
                    draw.scissor.height,
                ],
                target_size,
            ) else {
                continue;
            };
            if draw.ranges.is_empty() {
                continue;
            }
            pass.set_scissor_rect(x, y, width, height);
            pass.set_bind_group(0, bind_group, &[draw.uniform_offset]);
            debug_assert!(page.instance_count > 0);
            let interaction = draw
                .interaction_buffer_index
                .map(|index| {
                    frame
                        .buffers
                        .interaction_buffers
                        .get(index as usize)
                        .ok_or(AlignmentGpuRenderError::MissingOverlayBuffer)
                })
                .transpose()?;
            pass.set_pipeline(if interaction.is_some() {
                &self.interaction_pipeline
            } else {
                &self.pipeline
            });
            // Each range binds its run of the page (and of the interaction flags) at offset
            // zero, so instance `i` of the draw is the run's `i`th instance in both buffers.
            for range in draw.ranges.iter() {
                if range.first.saturating_add(range.count) > page.instance_count {
                    return Err(AlignmentGpuRenderError::InvalidDrawRange(draw.page));
                }
                let instance_stride = size_of::<AlignmentGpuInstance>() as u64;
                pass.set_vertex_buffer(
                    0,
                    page.buffer.slice(
                        u64::from(range.first) * instance_stride
                            ..u64::from(range.first + range.count) * instance_stride,
                    ),
                );
                if let Some(interaction) = interaction {
                    let flag_stride = size_of::<AlignmentGpuInteractionInstance>() as u64;
                    pass.set_vertex_buffer(
                        1,
                        interaction.slice(
                            u64::from(range.interaction_first) * flag_stride
                                ..u64::from(range.interaction_first + range.count) * flag_stride,
                        ),
                    );
                }
                pass.draw(0..6, 0..range.count);
            }
        }
        Ok(())
    }

    /// Rebuilds this renderer for a replacement GPU device, for example after the previous device
    /// was lost. Every pipeline is recreated against `device` and `target_format`, every
    /// device-derived page and frame buffer is dropped (releasing its ledger charge), and the
    /// device generation advances so frames, leases and plans captured against the old device
    /// are rejected instead of being replayed. Semantic scenes are untouched and are simply
    /// admitted again.
    pub fn rebuild_for_device(
        &mut self,
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
    ) {
        self.discard_device_resources();
        let mut rebuilt = Self::with_palette(device, target_format, self.palette.palette());
        rebuilt.device_generation = self.device_generation;
        rebuilt.revision = self.revision;
        for (rebuilt, previous) in [
            (
                &rebuilt.content_manifest_builds,
                &self.content_manifest_builds,
            ),
            (
                &rebuilt.content_manifest_pages_enumerated,
                &self.content_manifest_pages_enumerated,
            ),
            (
                &rebuilt.prefetch_authority_keys_inspected,
                &self.prefetch_authority_keys_inspected,
            ),
            (
                &rebuilt.reclaim_authority_keys_inspected,
                &self.reclaim_authority_keys_inspected,
            ),
        ] {
            rebuilt.store(previous.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        *self = rebuilt;
    }

    /// Releases every device-derived page and frame buffer. Semantic scenes, picking, selection,
    /// and evidence remain owned by `rgv-alignment-scene` and are intentionally untouched.
    pub fn discard_device_resources(&mut self) {
        self.cache.clear();
        self.frame_buffers.clear();
        self.pending_geometry_uploaded_bytes = 0;
        self.device_generation = self.device_generation.saturating_add(1).max(1);
    }

    #[must_use]
    pub fn resident_page_count(&self) -> usize {
        self.cache.pages.len()
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
        paged_cache::reclaim_for_goal(
            &mut self.cache,
            &mut self.frame_buffers,
            ledger,
            goal,
            protected_pages,
            self.revision,
            &self.reclaim_authority_keys_inspected,
        )
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
        paged_cache::resident_retained_bytes(&self.cache, &self.frame_buffers)
    }

    fn resident_gpu_bytes(&self) -> Result<usize, LedgerError> {
        paged_cache::resident_gpu_bytes(&self.cache, &self.frame_buffers)
    }

    fn page_index(&self, key: AlignmentPageKey) -> Option<usize> {
        self.cache.page_index(key)
    }

    fn page(&self, key: AlignmentPageKey) -> Option<&GpuAlignmentPage> {
        self.cache.page(key)
    }

    fn rebuild_reclaim_index(&mut self) {
        self.cache.rebuild_reclaim_index();
    }

    fn touch_page_at(&mut self, index: usize, revision: u64) {
        self.cache.touch_page_at(index, revision);
    }

    fn ensure_page_table_capacity(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        reservation_kind: ReservationKind,
    ) -> Result<(), AlignmentGpuPageAdmissionError> {
        Ok(self.cache.ensure_page_table_capacity(
            ledger,
            required,
            reservation_kind,
            self.revision,
        )?)
    }

    fn acquire_frame_buffers(
        &mut self,
        device: &wgpu::Device,
        ledger: &AllocationLedger,
        uniforms: usize,
        interaction_buffer_count: usize,
        interaction_page_instances: usize,
        frame_bytes: usize,
    ) -> Result<Arc<GpuAlignmentFrameBuffers>, AlignmentGpuPrepareError> {
        if let Some(index) = self
            .frame_buffers
            .iter()
            .enumerate()
            .filter(|(_, slot)| Arc::strong_count(slot) == 1)
            .filter(|(_, slot)| {
                slot.camera_capacity >= uniforms
                    && slot.interaction_buffers.len() >= interaction_buffer_count
                    && slot.interaction_capacity >= interaction_page_instances
                    && slot.frame_capacity >= frame_bytes
            })
            .min_by_key(|(_, slot)| {
                (
                    slot.camera_capacity,
                    slot.interaction_buffers.len(),
                    slot.interaction_capacity,
                )
            })
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
        let interaction_capacity = (interaction_page_instances > 0)
            .then(|| next_capacity(interaction_page_instances))
            .transpose()?
            .unwrap_or(0);
        let camera_bytes = camera_capacity.saturating_mul(self.camera_stride);
        let interaction_buffer_bytes = interaction_capacity
            .checked_mul(size_of::<AlignmentGpuInteractionInstance>())
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        let interaction_buffer_count =
            usize::from(interaction_buffer_bytes > 0).saturating_mul(interaction_buffer_count);
        let interaction_total_bytes = interaction_buffer_bytes
            .checked_mul(interaction_buffer_count)
            .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)?;
        validate_device_buffer_size(device, camera_bytes)?;
        validate_device_buffer_size(device, interaction_buffer_bytes)?;

        let mut camera_reservation = (camera_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    camera_bytes,
                    ReservationKind::Transition,
                )
            })
            .transpose()?;
        let mut interaction_reservation = (interaction_total_bytes > 0)
            .then(|| {
                ledger.reserve(
                    AllocationClass::GpuPage,
                    interaction_total_bytes,
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
        let interaction_buffers = (0..interaction_buffer_count)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV page-local alignment interaction buffer"),
                    size: interaction_buffer_bytes as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let camera_allocation = camera_reservation
            .as_mut()
            .map(|reservation| reservation.register(camera_bytes, self.revision))
            .transpose()?;
        let interaction_allocation = interaction_reservation
            .as_mut()
            .map(|reservation| reservation.register(interaction_total_bytes, self.revision))
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
            interaction_buffers,
            interaction_allocation,
            interaction_capacity,
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
        interaction_buffer_count: usize,
        interaction_page_instances: usize,
        frame_bytes: usize,
    ) -> Result<Arc<GpuAlignmentFrameBuffers>, AlignmentGpuPrepareError> {
        let mut adequate_slots = 0_usize;
        let mut available = 0_usize;
        for slot in &self.frame_buffers {
            available = available.max(slot.frame_capacity);
            if slot.camera_capacity < uniforms
                || slot.interaction_buffers.len() < interaction_buffer_count
                || slot.interaction_capacity < interaction_page_instances
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
        paged_cache::reclaim_idle_frame_slots(&mut self.frame_buffers);
    }

    fn reclaim_all_idle_frame_buffers(&mut self) {
        paged_cache::reclaim_all_idle_frame_slots(&mut self.frame_buffers);
    }

    fn evict_unpinned_pages_for_excluding(
        &mut self,
        ledger: &AllocationLedger,
        required: usize,
        limit: usize,
        protected: &[Arc<AlignmentSemanticPage>],
    ) -> Result<(), LedgerError> {
        self.reclaim_idle_frame_buffers();
        let protected = protected.iter().map(|page| page.key()).collect::<Vec<_>>();
        self.cache.evict_unpinned_pages_for_excluding(
            ledger,
            required,
            limit,
            &protected,
            self.revision,
            &self.reclaim_authority_keys_inspected,
        )
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
        if let Some(instance) = pack_primitive(primitive)? {
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
    page.drawable_primitive_count()
}

fn pack_primitive(
    primitive: AlignmentSemanticPrimitive,
) -> Result<Option<AlignmentGpuInstance>, AlignmentGpuPageAdmissionError> {
    if matches!(primitive.component, AlignmentComponent::Label { .. }) {
        return Ok(None);
    }
    let bases_from_tip = match primitive.geometry {
        AlignmentGeometryToken::InsideArrowhead(token) => token.bases_from_tip(),
        _ => 0,
    };
    AlignmentGpuInstance::new(
        primitive.start_offset,
        primitive.end_offset,
        primitive.row_offset,
        shape_for(primitive.geometry),
        color_for(primitive),
        bases_from_tip,
    )
    .map(Some)
}

const fn shape_for(geometry: AlignmentGeometryToken) -> AlignmentGpuShape {
    match geometry {
        AlignmentGeometryToken::Rectangle => AlignmentGpuShape::Rectangle,
        AlignmentGeometryToken::EventLine => AlignmentGpuShape::EventLine,
        AlignmentGeometryToken::PointMarker => AlignmentGpuShape::PointMarker,
        AlignmentGeometryToken::PointMarkerAtEnd => AlignmentGpuShape::PointMarkerAtEnd,
        AlignmentGeometryToken::DirectionalTerminalLeft => AlignmentGpuShape::DirectionTerminalLeft,
        AlignmentGeometryToken::DirectionalTerminalRight => {
            AlignmentGpuShape::DirectionTerminalRight
        }
        AlignmentGeometryToken::ClipEdge(edge) => AlignmentGpuShape::clip_edge(edge),
        AlignmentGeometryToken::ClipGhost => AlignmentGpuShape::ClipGhostCell,
        // Drawn as the terminal shapes are, with its distance from the tip.
        AlignmentGeometryToken::InsideArrowhead(token) => {
            if token.points_right() {
                AlignmentGpuShape::DirectionTerminalRight
            } else {
                AlignmentGpuShape::DirectionTerminalLeft
            }
        }
    }
}

fn color_for(primitive: AlignmentSemanticPrimitive) -> PaletteColor {
    semantic_alignment_color(primitive.component, primitive.style)
}

/// The semantic colour of one read component. Closed categories are palette tokens, continuous
/// quantities (mapping quality, modification-call confidence) ramps between two slots; only
/// hashed open-ended categories carry an inline colour.
pub(crate) fn semantic_alignment_color(
    component: AlignmentComponent,
    style: AlignmentStyleToken,
) -> PaletteColor {
    use PaletteToken as T;
    PaletteColor::Token(match component {
        AlignmentComponent::Base { base, .. } => T::base(base),
        AlignmentComponent::ModifiedBase {
            code,
            probability,
            state,
            color_by_type,
            ..
        } => {
            return crate::modification_style::modification_cell_color(
                code,
                probability,
                state,
                color_by_type,
            );
        }
        AlignmentComponent::BisulfiteBase { observation, .. } => match observation {
            rgv_alignment_scene::BisulfiteObservationToken::Protected => T::BISULFITE_PROTECTED,
            rgv_alignment_scene::BisulfiteObservationToken::Converted => T::BISULFITE_CONVERTED,
        },
        AlignmentComponent::Insertion { .. }
        | AlignmentComponent::Padding { .. }
        | AlignmentComponent::UltimaIndel { .. }
        | AlignmentComponent::SbxInsertion { .. } => T::INSERTION,
        AlignmentComponent::SoftClip { .. } | AlignmentComponent::HardClip { .. } => T::CLIP_EDGE,
        AlignmentComponent::Deletion { .. } => T::DELETION,
        AlignmentComponent::ReferenceSkip { .. } | AlignmentComponent::PairConnector { .. } => {
            T::SKIP
        }
        AlignmentComponent::SmrtKinetics { .. } => T::SMRT_KINETICS,
        AlignmentComponent::SbxTailBase { .. } => T::SBX_TAIL,
        AlignmentComponent::SpecialistUnavailable { .. } => T::SPECIALIST_UNAVAILABLE,
        // A ghost base carries its read's colour; the ghost shape (14) tints it.
        AlignmentComponent::DirectionTerminal
        | AlignmentComponent::Label { .. }
        | AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::ClipGhostBase { .. } => return style_color(style),
    })
}

fn style_color(style: AlignmentStyleToken) -> PaletteColor {
    use PaletteToken as T;
    PaletteColor::Token(match style.body {
        // Keep ordinary reads visually quiet. Interaction is applied independently below, so
        // hover and selection still receive RGV's blue emphasis without making every unselected
        // record look active.
        AlignmentBodyColorToken::Neutral => T::READ_NEUTRAL,
        AlignmentBodyColorToken::Strand { reverse: false } => T::STRAND_FORWARD,
        AlignmentBodyColorToken::Strand { reverse: true } => T::STRAND_REVERSE,
        AlignmentBodyColorToken::MappingQuality(quality) => {
            return PaletteColor::Ramp {
                low: T::MAPPING_QUALITY_LOW,
                position: f32::from(quality.unwrap_or(0)) / 60.0,
            };
        }
        // Open-ended: the hash decides the hue, so the colour travels inline.
        AlignmentBodyColorToken::Category(Some(category)) => {
            return PaletteColor::inline(crate::palette::categorical_hash_color(category.get()));
        }
        AlignmentBodyColorToken::Category(None) => T::UNCATEGORIZED,
        // Closed categories use explicit tables: hashing their small ranks put every member
        // within a few degrees of red.
        AlignmentBodyColorToken::PairOrientation(rank) => T::pair_orientation(rank),
        AlignmentBodyColorToken::TemplateLength { status, .. }
        | AlignmentBodyColorToken::PairStatus(status) => T::pair_classification(status),
        AlignmentBodyColorToken::FirstOfPairStrand(reverse) => match reverse {
            Some(false) => T::FIRST_OF_PAIR_FORWARD,
            Some(true) => T::FIRST_OF_PAIR_REVERSE,
            None => T::FIRST_OF_PAIR_UNKNOWN,
        },
        AlignmentBodyColorToken::Specialist { mode, reverse } => T::specialist_body(mode, reverse),
    })
}

const fn interaction_token_is_active(
    token: rgv_alignment_scene::AlignmentInteractionToken,
) -> bool {
    token.hovered || token.selected || token.selected_chain || token.highlighted
}

fn interaction_flags(token: rgv_alignment_scene::AlignmentInteractionToken) -> u32 {
    u32::from(token.hovered)
        | (u32::from(token.selected) << 1)
        | (u32::from(token.selected_chain) << 2)
        | (u32::from(token.highlighted) << 3)
}

/// Builds one page's interaction flags and counts its active (non-zero) instances in the same
/// walk.
fn interaction_page_instances(
    page: &AlignmentSemanticPage,
    interaction: &AlignmentInteractionState,
) -> (Vec<AlignmentGpuInteractionInstance>, usize) {
    let mut active = 0_usize;
    let mut instances = Vec::with_capacity(non_text_primitive_count(page));
    for primitive in page
        .primitives()
        .iter()
        .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
    {
        let flags = interaction_flags(interaction.token(primitive.handle));
        active += usize::from(flags != 0);
        instances.push(AlignmentGpuInteractionInstance { flags });
    }
    (instances, active)
}

/// Interaction flags for the instances of `ranges` (pairs of semantic primitive range and
/// instance range from [`culled_draw_ranges`]), concatenated in draw order, and the number of
/// active ones. Only the drawn instances are visited.
fn interaction_range_instances(
    page: &AlignmentSemanticPage,
    ranges: &[(std::ops::Range<u32>, std::ops::Range<u32>)],
    interaction: &AlignmentInteractionState,
) -> (Vec<AlignmentGpuInteractionInstance>, usize) {
    let mut active = 0_usize;
    let mut instances = Vec::with_capacity(
        ranges
            .iter()
            .map(|(_, instances)| (instances.end - instances.start) as usize)
            .sum(),
    );
    for (primitives, _) in ranges {
        for primitive in page.primitives()[primitives.start as usize..primitives.end as usize]
            .iter()
            .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
        {
            let flags = interaction_flags(interaction.token(primitive.handle));
            active += usize::from(flags != 0);
            instances.push(AlignmentGpuInteractionInstance { flags });
        }
    }
    (instances, active)
}

fn camera_uniform(
    camera: AlignmentCamera,
    screen_size: [f32; 2],
    page_start: u64,
    page_row_start: u32,
) -> AlignmentGpuCameraUniform {
    let (page_start_whole, page_start_fraction) =
        crate::shaders::split_camera_origin(page_start, camera.exact_start());
    AlignmentGpuCameraUniform {
        screen_size: [screen_size[0].max(1.0), screen_size[1].max(1.0)],
        clip_origin: [camera.clip().x as f32, camera.clip().y as f32],
        points_per_base: split_f64(1.0 / camera.bases_per_pixel()),
        page_start_whole,
        page_start_fraction,
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

/// Byte size of an immutable page buffer holding `count` instances of `T`.
///
/// Page buffers are written once and never grow, so they are allocated at their exact size,
/// rounded up only to wgpu's copy alignment. (Rounding the instance count to a power of two, as
/// the growable slot tables do, wasted up to half of every page's GPU memory.) The ledger is
/// charged this same value, so accounted bytes equal allocated bytes.
fn gpu_buffer_capacity<T>(count: usize) -> Result<usize, AlignmentGpuPageAdmissionError> {
    exact_immutable_buffer_bytes(count, size_of::<T>())
        .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)
}

/// `count * stride` rounded up to `wgpu::COPY_BUFFER_ALIGNMENT`, or `None` on overflow.
pub(crate) fn exact_immutable_buffer_bytes(count: usize, stride: usize) -> Option<usize> {
    let alignment = wgpu::COPY_BUFFER_ALIGNMENT as usize;
    count
        .checked_mul(stride)?
        .checked_next_multiple_of(alignment)
}

fn next_capacity(count: usize) -> Result<usize, AlignmentGpuPageAdmissionError> {
    paged_cache::next_power_of_two_capacity(count)
        .ok_or(AlignmentGpuPageAdmissionError::CapacityOverflow)
}

/// Retained charge of a GPU page table of `capacity` entries; see
/// [`paged_cache::page_table_retained_bytes`].
fn alignment_page_table_retained_bytes(capacity: usize) -> Option<usize> {
    paged_cache::page_table_retained_bytes::<GpuAlignmentPage>(capacity)
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

fn prepared_scene_pages(
    requests: &[AlignmentGpuSceneRequest<'_>],
) -> Vec<Arc<AlignmentSemanticPage>> {
    let mut pages = requests
        .iter()
        .flat_map(|request| request.scene.pages().iter().cloned())
        .filter(|page| alignment_page_has_gpu_geometry(page))
        .collect::<Vec<_>>();
    pages.sort_unstable_by_key(|page| page.key());
    pages.dedup_by_key(|page| page.key());
    pages
}

fn prepared_scene_page_keys(requests: &[AlignmentGpuSceneRequest<'_>]) -> Vec<AlignmentPageKey> {
    let mut keys = requests
        .iter()
        .flat_map(|request| request.scene.drawable_page_keys().iter().copied())
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
        [request] => Arc::clone(request.scene.drawable_page_keys()),
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
                .filter(|page| alignment_page_has_gpu_geometry(page))
                .cloned()
        })
        .collect::<Vec<_>>();
    pages.sort_unstable_by_key(|page| page.key());
    pages.dedup_by_key(|page| page.key());
    Ok(pages)
}

/// Semantic label pages remain required for CPU projection, picking, inspection, and export, but
/// contain no vertex instances. Keeping this boundary explicit prevents zero-byte semantic pages
/// from acquiring fake GPU residency or appearing in a frame draw manifest that cache reclaim can
/// later invalidate.
/// Whether `key` is in the sorted `deferred` list.
fn is_deferred(deferred: &[AlignmentPageKey], key: AlignmentPageKey) -> bool {
    !deferred.is_empty() && deferred.binary_search(&key).is_ok()
}

/// The visible drawable pages of `requests`, most important first: by genomic distance from the
/// centre of the viewport to the page (zero for the page under the centre), then by row block
/// from the top of the visible rows down, then by page key so the order is total. A page visible
/// through several requests takes its best rank.
pub(crate) fn visible_page_admission_order(
    requests: &[AlignmentGpuSceneRequest<'_>],
) -> Vec<AlignmentPageKey> {
    let mut ranked = BTreeMap::<AlignmentPageKey, (u64, u32)>::new();
    for request in requests {
        let interval = request.camera.interval();
        let centre = interval.start() + interval.len() / 2;
        let rows = request.camera.visible_rows();
        for page in request
            .scene
            .visible_pages(interval, rows.clone())
            .into_iter()
            .filter(|page| alignment_page_has_gpu_geometry(page))
        {
            let page_interval = page.interval();
            let distance = if centre < page_interval.start() {
                page_interval.start() - centre
            } else {
                // The page end is exclusive: the last base it holds is `end - 1`.
                centre.saturating_sub(page_interval.end().saturating_sub(1))
            };
            let row_distance = page.row_range().start.saturating_sub(rows.start);
            let rank = (distance, row_distance);
            ranked
                .entry(page.key())
                .and_modify(|best| *best = (*best).min(rank))
                .or_insert(rank);
        }
    }
    let mut order = ranked.into_iter().collect::<Vec<_>>();
    order.sort_unstable_by_key(|(key, rank)| (*rank, *key));
    order.into_iter().map(|(key, _)| key).collect()
}

fn alignment_page_has_gpu_geometry(page: &AlignmentSemanticPage) -> bool {
    page.has_drawable_primitives()
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
        "alignment semantic primitive spans page offsets {start}..{end}, beyond the {limit}-base \
         range a GPU page can address"
    )]
    SemanticOffsetOutOfRange { start: u64, end: u64, limit: u64 },
    #[error(
        "alignment semantic primitive is on page row {row_offset}, beyond the {limit} rows a GPU \
         page can address"
    )]
    RowOffsetOutOfRange { row_offset: u32, limit: u32 },
    #[error(
        "alignment semantic primitive is {bases_from_tip} bases from its read's tip, beyond the \
         {limit} a GPU instance can carry"
    )]
    TipDistanceOutOfRange { bases_from_tip: u8, limit: u8 },
    #[error(
        "alignment GPU page table capacity is invalid: required {required}, allocated {actual}"
    )]
    PageTableCapacity { required: usize, actual: usize },
    #[error("alignment GPU buffer requires {required} bytes but the device limit is {limit}")]
    DeviceBufferLimit { required: usize, limit: usize },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

impl From<PageTableError> for AlignmentGpuPageAdmissionError {
    fn from(error: PageTableError) -> Self {
        match error {
            PageTableError::CapacityOverflow => Self::CapacityOverflow,
            PageTableError::Capacity { required, actual } => {
                Self::PageTableCapacity { required, actual }
            }
            PageTableError::Ledger(error) => Self::from(error),
        }
    }
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
    #[error("alignment interaction count exceeds the u32 draw address space")]
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
    #[error("GPU alignment frame has no interaction buffer")]
    MissingOverlayBuffer,
    #[error("a pinned GPU alignment page disappeared before render: {0:?}")]
    MissingPinnedPage(AlignmentPageKey),
    #[error("a GPU alignment draw range lies outside its page: {0:?}")]
    InvalidDrawRange(AlignmentPageKey),
}

#[cfg(test)]
pub(crate) mod culling_tests;
#[cfg(test)]
pub(crate) mod tests;
