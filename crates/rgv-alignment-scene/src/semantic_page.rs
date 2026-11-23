use std::{
    collections::{BTreeSet, HashMap},
    mem::size_of,
    num::NonZeroU32,
    ops::Range,
    sync::Arc,
};

use rgv_core::{
    AlignmentColorMode, GenomicInterval, ModifiedBaseColorMode, ReadBaseDisplayMode,
    SbxSimplexTailMode,
};
use rgv_model::{
    AlignedBaseClassification, AuxValue, CigarOpKind, ImplicitModificationMode, PairClassification,
    PairOrientation, ReferenceSlice, SbxInsertionState, SbxTailBounds, SequenceView,
    TemplateLengthBounds, UltimaEventUnavailable, UltimaIndelKind, UltimaQualitySource,
    aligned_position_for_sequencer_position, analyze_ultima_record, bisulfite_strand_is_reverse,
    classify_aligned_base, classify_bisulfite_observation, classify_sbx_insertion,
    decode_modified_bases_parts_checked, decode_smrt_kinetics, modified_bases_resolvable,
    pair_evidence_view, sbx_tail_bounds, sequencer_base_matches, smrt_kinetics_tag,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentReadLabelMode, AlignmentRowAtlas,
    AlignmentRowConnector, AlignmentSourceRevision, AlignmentStyleContract, AllocationClass,
    AllocationLease, AllocationLedger, CanonicalGenomicTile, CanonicalTileError,
    CapabilityRequirement, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, GeometryLod, LedgerError, PresentationContract, ReservationKind,
    RevisionFingerprint, RowAtlasRecordHit, RowAtlasRevision, TransientResourceClass,
    TransientResourceReservation,
    fingerprint::StableHasher,
    page_identity::{AlignmentPageIdentity, page_identity},
};

/// Measurement-selected canonical vertical page identity. The block height is explicit in every
/// key, so later profiling can change it without ambiguously reusing old geometry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalRowBlock {
    coordinate: u32,
    rows: NonZeroU32,
}

impl CanonicalRowBlock {
    #[must_use]
    pub const fn new(coordinate: u32, rows: NonZeroU32) -> Self {
        Self { coordinate, rows }
    }

    #[must_use]
    pub const fn coordinate(self) -> u32 {
        self.coordinate
    }

    #[must_use]
    pub const fn rows(self) -> NonZeroU32 {
        self.rows
    }

    pub fn range(self, total_rows: usize) -> Result<std::ops::Range<u32>, SemanticPageError> {
        let start = self
            .coordinate
            .checked_mul(self.rows.get())
            .ok_or(SemanticPageError::RowCoordinateOverflow)?;
        let total =
            u32::try_from(total_rows).map_err(|_| SemanticPageError::RowCoordinateOverflow)?;
        if total == 0 && self.coordinate == 0 {
            return Ok(0..0);
        }
        if start >= total {
            return Err(SemanticPageError::RowBlockOutsideAtlas { start, total });
        }
        Ok(start..start.saturating_add(self.rows.get()).min(total))
    }
}

/// Name of one immutable alignment geometry page. No exact camera coordinate, pixel position,
/// hover, selection, or vertical scroll state participates in this key.
///
/// The evidence and row-atlas revisions are those the page was *first built from*. A page the
/// store carries across an extension keeps its key, so scenes published from later roots can
/// hold pages whose keys name earlier revisions; that is what lets key-addressed derivatives
/// such as GPU pages survive an edge acquisition. The revision-free content identity the store
/// proves reuse with is [`crate::AlignmentPageIdentity`], from
/// [`AlignmentSemanticPage::identity`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentPageKey {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    row_atlas_revision: RowAtlasRevision,
    layout: ContractFingerprint,
    presentation: ContractFingerprint,
    genomic_tile: CanonicalGenomicTile,
    row_block: CanonicalRowBlock,
    family: AlignmentPageFamily,
    lod: GeometryLod,
    /// Digest of the reference bases a store-resolved page was built against; zero for a key
    /// made by [`Self::new`] and for every page family which reads no reference. One pair of
    /// roots can be presented against two reference slices, and this keeps those two pages
    /// from sharing a key.
    reference: RevisionFingerprint,
}

impl AlignmentPageKey {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        source_revision: AlignmentSourceRevision,
        evidence_revision: EvidenceRevision,
        row_atlas_revision: RowAtlasRevision,
        layout: ContractFingerprint,
        presentation: ContractFingerprint,
        genomic_tile: CanonicalGenomicTile,
        row_block: CanonicalRowBlock,
        family: AlignmentPageFamily,
        lod: GeometryLod,
    ) -> Self {
        Self {
            source_revision,
            evidence_revision,
            row_atlas_revision,
            layout,
            presentation,
            genomic_tile,
            row_block,
            family,
            lod,
            reference: RevisionFingerprint::new(0, 0),
        }
    }

    /// Digest of the reference bases the page was built against, or zero. See
    /// [`crate::AlignmentPageContentToken::reference`].
    #[must_use]
    pub const fn reference_digest(self) -> RevisionFingerprint {
        self.reference
    }

    pub(crate) const fn with_reference_digest(mut self, reference: RevisionFingerprint) -> Self {
        self.reference = reference;
        self
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn evidence_revision(self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn row_atlas_revision(self) -> RowAtlasRevision {
        self.row_atlas_revision
    }

    #[must_use]
    pub const fn layout(self) -> ContractFingerprint {
        self.layout
    }

    #[must_use]
    pub const fn presentation(self) -> ContractFingerprint {
        self.presentation
    }

    #[must_use]
    pub const fn genomic_tile(self) -> CanonicalGenomicTile {
        self.genomic_tile
    }

    #[must_use]
    pub const fn row_block(self) -> CanonicalRowBlock {
        self.row_block
    }

    #[must_use]
    pub const fn family(self) -> AlignmentPageFamily {
        self.family
    }

    #[must_use]
    pub const fn lod(self) -> GeometryLod {
        self.lod
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentOperation {
    Match,
    SequenceMatch,
    SequenceMismatch,
    Insertion,
    Deletion,
    ReferenceSkip,
    SoftClip,
    HardClip,
    Padding,
}

impl From<CigarOpKind> for AlignmentOperation {
    fn from(value: CigarOpKind) -> Self {
        match value {
            CigarOpKind::Match => Self::Match,
            CigarOpKind::SequenceMatch => Self::SequenceMatch,
            CigarOpKind::SequenceMismatch => Self::SequenceMismatch,
            CigarOpKind::Insertion => Self::Insertion,
            CigarOpKind::Deletion => Self::Deletion,
            CigarOpKind::ReferenceSkip => Self::ReferenceSkip,
            CigarOpKind::SoftClip => Self::SoftClip,
            CigarOpKind::HardClip => Self::HardClip,
            CigarOpKind::Padding => Self::Padding,
        }
    }
}

/// Typed semantic target shared by rendering, hover, click, inspection, and export.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentComponent {
    ReadSpan,
    AlignedSegment {
        cigar_index: u32,
        operation: AlignmentOperation,
    },
    Insertion {
        cigar_index: u32,
        read_start: u32,
        length: u32,
    },
    Deletion {
        cigar_index: u32,
    },
    ReferenceSkip {
        cigar_index: u32,
    },
    SoftClip {
        cigar_index: u32,
        read_start: u32,
        length: u32,
    },
    HardClip {
        cigar_index: u32,
        length: u32,
    },
    Padding {
        cigar_index: u32,
        length: u32,
    },
    DirectionTerminal,
    PairConnector {
        template: u64,
        /// Source position of the mate record. A connector joins two records of one source, so
        /// the mate's source revision is the primitive's own and is not repeated here; storing
        /// a whole [`AlignmentHandle`] made this the widest variant and every primitive 32
        /// bytes larger. Use [`AlignmentComponent::pair_connector_mate`] for the handle.
        mate: Option<rgv_model::AlignmentSourcePosition>,
    },
    Label {
        kind: AlignmentLabelKind,
        id: ContractFingerprint,
    },
    ModifiedBase {
        read_index: u32,
        canonical_base: u8,
        code: rgv_model::ModificationCode,
        /// For [`ModificationState::Modified`], the shown code's `ML` value; for
        /// [`ModificationState::Canonical`], the summed probability of every modification
        /// called at the base, or `None` for an implicitly unmodified base.
        probability: Option<u8>,
        state: ModificationState,
        /// Drawn in the code's type colour rather than the single modification colour.
        color_by_type: bool,
    },
    BisulfiteBase {
        read_index: u32,
        observation: BisulfiteObservationToken,
    },
    SmrtKinetics {
        read_index: u32,
        tag: [u8; 2],
        value: u64,
    },
    UltimaIndel {
        cigar_index: u32,
        kind: UltimaIndelToken,
        quality_bits: Option<u32>,
        quality_source: Option<UltimaQualitySourceToken>,
        unavailable: Option<UltimaUnavailableToken>,
    },
    SbxInsertion {
        cigar_index: u32,
        state: SbxInsertionToken,
    },
    SbxTailBase {
        read_index: u32,
    },
    SpecialistUnavailable {
        mode: SpecialistMode,
        reason: SpecialistUnavailableReason,
    },
    Base {
        cigar_index: u32,
        read_index: u32,
        base: u8,
        quality: Option<u8>,
        classification: AlignedBaseClassification,
    },
    /// A soft-clipped base drawn beyond the read's cut edge at base-level detail (its `SEQ`
    /// base, as stored). `obscured` when another read's aligned extent, or another read's ghost
    /// base, occupies the same cell of the row: the cell still draws beneath that read, but its
    /// letter is not shown.
    ClipGhostBase {
        cigar_index: u32,
        read_index: u32,
        base: u8,
        obscured: bool,
    },
}

impl AlignmentComponent {
    /// The mate record of a pair connector drawn for `own`, the handle of the primitive (or
    /// hit) that carries this component. `None` for any other component and for a connector
    /// whose mate is unknown.
    #[must_use]
    pub const fn pair_connector_mate(self, own: AlignmentHandle) -> Option<AlignmentHandle> {
        match self {
            Self::PairConnector {
                mate: Some(position),
                ..
            } => Some(AlignmentHandle::new(own.source_revision(), position)),
            _ => None,
        }
    }
}

/// Camera-independent shape carried by semantic geometry.
///
/// Direction belongs here rather than being inferred by one renderer. This keeps the CPU
/// reference projector, GPU renderer, picking, and exports on the same geometry contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentGeometryToken {
    Rectangle,
    EventLine,
    /// An I-beam (a narrow bar with short serifs across the top and bottom of the read) centred
    /// on the base boundary at the START edge of its one-base span (between the span's base and
    /// the base before it): insertions, padding. Its sizes are capped in points, so it never
    /// fills a base cell and cannot be mistaken for a substitution.
    PointMarker,
    /// As [`Self::PointMarker`], centred on the END edge of its one-base span. Used when the
    /// event lies at the read's right boundary (a trailing clip or insertion), whose position is
    /// one past the last aligned base and was therefore clamped onto that base.
    PointMarkerAtEnd,
    DirectionalTerminalLeft,
    DirectionalTerminalRight,
    /// The cut edge of a clipped read end, drawn just inside the read's aligned boundary on the
    /// start (or end) edge of its one-base span: a dark zigzag for a soft clip, a straight bar
    /// for a hard clip. It never fills a base cell. On a label (a clip length) the same token
    /// anchors the text OUTSIDE the read: before the span's start, or after its end.
    ClipEdge(ClipEdgeToken),
    /// One soft-clipped base drawn faintly beyond the cut edge at base-level detail. Ghost cells
    /// paint beneath all other read geometry of their row, so a neighbouring read covers them.
    ClipGhost,
    /// A rectangle drawn on a read (a base or specialist cell, or an aligned block piece) whose
    /// tip-side edge lies within [`ArrowheadCellToken::MAXIMUM_BASES_FROM_TIP`] whole bases of
    /// the read's directional tip, so the read's arrowhead may reach it. It is cut by the read's
    /// one diagonal exactly as the [`Self::DirectionalTerminalLeft`] / `Right` primitives that
    /// end on the tip are, with the tip that many bases beyond its own tip-side edge.
    InsideArrowhead(ArrowheadCellToken),
}

/// Where a [`AlignmentGeometryToken::InsideArrowhead`] rectangle lies relative to its read's
/// direction arrowhead: which way the read points and how many whole bases separate the
/// rectangle's tip-side edge from the read's tip. The head is at most
/// `ARROWHEAD_MAXIMUM_BASES` (ten) bases long at every zoom, so a rectangle ten or more bases
/// from the tip is never cut and carries no token.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArrowheadCellToken {
    points_right: bool,
    bases_from_tip: u8,
}

impl ArrowheadCellToken {
    /// The largest distance a token records: the head never reaches a base this far plus one.
    pub const MAXIMUM_BASES_FROM_TIP: u8 = 9;

    /// `None` when `bases_from_tip` exceeds [`Self::MAXIMUM_BASES_FROM_TIP`].
    #[must_use]
    pub const fn new(points_right: bool, bases_from_tip: u8) -> Option<Self> {
        if bases_from_tip > Self::MAXIMUM_BASES_FROM_TIP {
            return None;
        }
        Some(Self {
            points_right,
            bases_from_tip,
        })
    }

    /// The read's tip is its right end (a forward read); otherwise its left end.
    #[must_use]
    pub const fn points_right(self) -> bool {
        self.points_right
    }

    /// Whole bases from the rectangle's tip-side edge (its end edge when the read points right,
    /// its start edge otherwise) to the read's tip; zero when the rectangle ends on the tip.
    #[must_use]
    pub const fn bases_from_tip(self) -> u8 {
        self.bases_from_tip
    }
}

/// Which clipped boundary a [`AlignmentGeometryToken::ClipEdge`] marks and how it is drawn.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClipEdgeToken {
    /// The clip is on the read's right boundary: the end edge of the one-base span (the last
    /// aligned base). Otherwise it is on the left boundary, the span's start edge.
    pub at_end: bool,
    /// A hard clip: a straight bar. Otherwise a soft clip's sawtooth.
    pub hard: bool,
    /// The read's direction arrowhead is on the same boundary, so the cut edge is drawn just
    /// inside the arrowhead's base and the strand stays visible.
    pub behind_arrow: bool,
}

impl AlignmentGeometryToken {
    /// Point markers sit on a base boundary rather than covering their span.
    #[must_use]
    pub const fn is_point_marker(self) -> bool {
        matches!(self, Self::PointMarker | Self::PointMarkerAtEnd)
    }

    /// Marks drawn on a base boundary (point markers and clip cut edges) whose drawn extent may
    /// overhang their one-base span.
    #[must_use]
    pub const fn is_boundary_mark(self) -> bool {
        matches!(
            self,
            Self::PointMarker | Self::PointMarkerAtEnd | Self::ClipEdge(_)
        )
    }

    /// Paint order of a geometry alone: clip ghosts, then everything drawn on a read, then
    /// boundary marks. It cannot tell a read body from the bases drawn on it, which share the
    /// `Rectangle` geometry; order primitives with [`primitive_paint_layer`] instead.
    #[must_use]
    pub const fn paint_layer(self) -> u8 {
        match self {
            Self::ClipGhost => 0,
            Self::PointMarker | Self::PointMarkerAtEnd | Self::ClipEdge(_) => 2,
            Self::Rectangle
            | Self::EventLine
            | Self::DirectionalTerminalLeft
            | Self::DirectionalTerminalRight
            | Self::InsideArrowhead(_) => 1,
        }
    }

    /// Boundary geometry for a point event at `position`, whose valid aligned bases are
    /// `record_start..record_end`. An event at or past `record_end` is drawn on the read's right
    /// boundary: the end edge of the last aligned base.
    #[must_use]
    pub const fn point_marker_for(position: u64, record_end: u64) -> Self {
        if position >= record_end {
            Self::PointMarkerAtEnd
        } else {
            Self::PointMarker
        }
    }
}

/// Paint order within one row, shared by the semantic page (and so the GPU), the CPU projector
/// and picking:
///
/// 0. clip ghosts, beneath every read of the row;
/// 1. read bodies: the read span, aligned blocks, the direction terminal, deletion and skip
///    lines and pair connectors;
/// 2. everything drawn on a read body: base and mismatch cells, modification, bisulfite,
///    kinetics and other specialist cells;
/// 3. boundary marks (point markers, clip edges), which overhang neighbouring cells.
///
/// Bodies and the cells drawn on them share the `Rectangle` geometry, so the component decides.
/// Ordering them by position alone painted an aligned block over a mismatch cell that starts
/// at the same base (the first base after an insertion, deletion or splice), hiding its fill
/// while its letter, drawn separately, stayed visible.
#[must_use]
pub const fn primitive_paint_layer(
    component: AlignmentComponent,
    geometry: AlignmentGeometryToken,
) -> u8 {
    match geometry.paint_layer() {
        0 => 0,
        2 => 3,
        _ => match component {
            AlignmentComponent::ReadSpan
            | AlignmentComponent::AlignedSegment { .. }
            | AlignmentComponent::DirectionTerminal
            | AlignmentComponent::Deletion { .. }
            | AlignmentComponent::ReferenceSkip { .. }
            | AlignmentComponent::PairConnector { .. } => 1,
            _ => 2,
        },
    }
}

/// The within-page paint order of one primitive; see [`primitive_paint_layer`].
fn page_paint_key(
    primitive: &AlignmentSemanticPrimitive,
) -> (u32, u8, u64, u64, u32, AlignmentHandle, AlignmentComponent) {
    (
        primitive.row_offset,
        primitive_paint_layer(primitive.component, primitive.geometry),
        primitive.start_offset,
        primitive.end_offset,
        primitive.draw_order,
        primitive.handle,
        primitive.component,
    )
}

/// The likelier state of a base with modification calls.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModificationState {
    /// The cell's code is the likeliest modification, at least as likely as none.
    Modified,
    /// No modification is the likelier state (or the base is implicitly unmodified).
    Canonical,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BisulfiteObservationToken {
    Protected,
    Converted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaIndelToken {
    Insertion,
    Deletion,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaQualitySourceToken {
    TpDirection,
    T0,
    MinimumHomopolymer,
    MaximumHomopolymerLimit,
    TpReadMaximumFallback,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UltimaUnavailableToken {
    NotBracketedByAlignedBases,
    AmbiguousHomopolymer,
    NonHomopolymer,
    ReferenceUnavailable,
    T0Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SbxInsertionToken {
    Concordant,
    PartlyDiscordant,
    FullyDiscordant,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpecialistMode {
    ModifiedBases,
    Bisulfite,
    SmrtKinetics,
    Ultima,
    Sbx,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpecialistUnavailableReason {
    MissingRecordData,
    MalformedRecordData,
    MissingReference,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentLabelKind {
    ReadName,
    AuxiliaryTag([u8; 2]),
    /// An insertion's label (`+T`, `+AG` or `+12`; see `insertion_label_text`) for an insertion
    /// at least `minimum_event_label_size` long: shown, as its length, from
    /// `EVENT_LABEL_MINIMUM_POINTS_PER_BASE` where it has room.
    InsertionLength,
    /// The same label for a shorter insertion: shown only at base-letter detail.
    ShortInsertion,
    DeletionLength,
    SkipLength,
    ClipLength,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSemanticLabel {
    id: ContractFingerprint,
    text: Arc<str>,
}

impl AlignmentSemanticLabel {
    #[must_use]
    pub const fn id(&self) -> ContractFingerprint {
        self.id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentStyleToken {
    pub flags: u16,
    pub mapping_quality: Option<u8>,
    pub operation: Option<AlignmentOperation>,
    pub body: AlignmentBodyColorToken,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentBodyColorToken {
    Neutral,
    Strand { reverse: bool },
    MappingQuality(Option<u8>),
    Category(Option<ContractFingerprint>),
    TemplateLength { absolute: u64, status: u8 },
    PairOrientation(u8),
    PairStatus(u8),
    FirstOfPairStrand(Option<bool>),
    Specialist { mode: u8, reverse: bool },
}

/// Page-local semantic primitive. Coordinates are integer offsets from the genomic tile and row
/// block, never absolute chromosome `f32` values or pane pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSemanticPrimitive {
    pub start_offset: u64,
    pub end_offset: u64,
    pub row_offset: u32,
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub geometry: AlignmentGeometryToken,
    pub style: AlignmentStyleToken,
    pub draw_order: u32,
}

impl AlignmentSemanticPrimitive {
    #[must_use]
    pub const fn contains_offset(self, offset: u64) -> bool {
        self.start_offset <= offset && offset < self.end_offset
    }

    /// The mate record when this primitive is a pair connector with a known mate.
    #[must_use]
    pub const fn pair_connector_mate(self) -> Option<AlignmentHandle> {
        self.component.pair_connector_mate(self.handle)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPageHit {
    pub handle: AlignmentHandle,
    pub component: AlignmentComponent,
    pub draw_order: u32,
}

/// Immutable CPU semantic geometry page. GPU pages and the software projector derive from this
/// object; GPU residency can never alter its evidence or picking identity.
#[derive(Debug)]
pub struct AlignmentSemanticPage {
    key: AlignmentPageKey,
    /// Revision-free digest of every input this page was built from. A later evidence or
    /// row-atlas publication may reuse this page exactly when it reproduces this identity.
    identity: AlignmentPageIdentity,
    interval: GenomicInterval,
    row_start: u32,
    row_end: u32,
    primitives: Arc<[AlignmentSemanticPrimitive]>,
    row_offsets: Arc<[u32]>,
    /// Immutable page-local anchor directory. The labels remain canonical semantic primitives;
    /// camera capture follows these indices instead of filtering every page primitive again.
    label_primitive_indices: Arc<[u32]>,
    labels: Arc<[AlignmentSemanticLabel]>,
    allocation: AllocationLease,
}

#[derive(Clone, Debug)]
struct PreparedRecordSemantics {
    body: AlignmentBodyColorToken,
    read_label: Option<(AlignmentLabelKind, String)>,
    overlays: Vec<PreparedOverlay>,
    hidden_sbx_tail: Option<SbxTailBounds>,
}

#[derive(Clone, Copy, Debug)]
struct PreparedOverlay {
    start: u64,
    end: u64,
    component: AlignmentComponent,
    /// A point event at the read's right boundary, clamped onto the last aligned base; its
    /// marker is drawn on that base's end edge.
    end_anchored: bool,
}

#[derive(Clone, Debug)]
struct PendingLabel {
    id: ContractFingerprint,
    text: String,
}

/// Everything a page build reads from the row atlas, gathered once so that the content
/// identity and the build provably describe the same inputs.
#[derive(Debug)]
pub(crate) struct AlignmentPageInputs {
    key: AlignmentPageKey,
    identity: AlignmentPageIdentity,
    interval: GenomicInterval,
    rows: std::ops::Range<u32>,
    atlas_records: Vec<RowAtlasRecordHit>,
    pair_connectors: Vec<AlignmentRowConnector>,
    _scratch: TransientResourceReservation,
}

impl AlignmentPageInputs {
    pub(crate) const fn key(&self) -> AlignmentPageKey {
        self.key
    }

    pub(crate) const fn identity(&self) -> AlignmentPageIdentity {
        self.identity
    }

    /// Names the page to be built by the reference bases it reads, so that the same roots
    /// presented against a different reference slice cannot produce a second page under one
    /// key. Only the page store does this; a directly built page keeps the caller's key.
    pub(crate) fn keyed_by_reference(mut self) -> Self {
        self.key = self
            .key
            .with_reference_digest(self.identity.content().reference());
        self
    }
}

impl AlignmentSemanticPage {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        key: AlignmentPageKey,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        contig_length: u64,
        reference: Option<&ReferenceSlice>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SemanticPageError> {
        let inputs = Self::collect_inputs(
            key,
            evidence,
            atlas,
            presentation,
            contig_length,
            reference,
            ledger,
            reservation_kind,
        )?;
        Self::build_from_inputs(
            inputs,
            evidence,
            atlas,
            presentation,
            reference,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    /// Validates `key` against the roots and gathers the row-atlas inputs and content identity
    /// of the page it names, without selecting evidence or walking any CIGAR. Succeeding here
    /// means every handle the atlas assigns to the page resolves in `evidence`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn collect_inputs(
        key: AlignmentPageKey,
        evidence: &EvidenceRoot,
        atlas: &AlignmentRowAtlas,
        presentation: PresentationContract,
        contig_length: u64,
        reference: Option<&ReferenceSlice>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
    ) -> Result<AlignmentPageInputs, SemanticPageError> {
        validate_dependencies(key, evidence, atlas, presentation, reference)?;
        let interval = key.genomic_tile.interval(contig_length)?;
        if !evidence.coverage().covers(interval) {
            return Err(SemanticPageError::UncertifiedTile(interval));
        }
        let rows = key.row_block.range(atlas.row_count())?;
        let atlas_records = atlas.records_for_rows(rows.clone(), interval);
        let connector_count = if key.family == AlignmentPageFamily::ReadGeometry {
            atlas.connector_count_for_rows(rows.clone(), interval)
        } else {
            0
        };
        let scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            semantic_page_base_scratch_bytes(0, atlas_records.len(), connector_count),
            reservation_kind,
        )?;
        let pair_connectors = if key.family == AlignmentPageFamily::ReadGeometry {
            atlas.connectors_for_rows(rows.clone(), interval)
        } else {
            Vec::new()
        };
        let identity = page_identity(
            key,
            evidence,
            atlas,
            presentation.style,
            contig_length,
            interval,
            rows.clone(),
            &atlas_records,
            &pair_connectors,
            reference,
        )
        .map_err(|missing| SemanticPageError::MissingEvidenceHandle(missing.0))?;
        Ok(AlignmentPageInputs {
            key,
            identity,
            interval,
            rows,
            atlas_records,
            pair_connectors,
            _scratch: scratch,
        })
    }

    /// Reference-agnostic identity of the page `key` names, for residency questions asked
    /// without a reference slice. It matches a resident page built from the same records and
    /// rows whatever reference bases that page was built with.
    pub(crate) fn probe_identity(
        key: AlignmentPageKey,
        evidence: &EvidenceRoot,
        atlas: &AlignmentRowAtlas,
        presentation: PresentationContract,
        contig_length: u64,
    ) -> Result<AlignmentPageIdentity, SemanticPageError> {
        validate_root_dependencies(key, evidence, atlas, presentation)?;
        let interval = key.genomic_tile.interval(contig_length)?;
        if !evidence.coverage().covers(interval) {
            return Err(SemanticPageError::UncertifiedTile(interval));
        }
        let rows = key.row_block.range(atlas.row_count())?;
        let atlas_records = atlas.records_for_rows(rows.clone(), interval);
        let pair_connectors = if key.family == AlignmentPageFamily::ReadGeometry {
            atlas.connectors_for_rows(rows.clone(), interval)
        } else {
            Vec::new()
        };
        page_identity(
            key,
            evidence,
            atlas,
            presentation.style,
            contig_length,
            interval,
            rows,
            &atlas_records,
            &pair_connectors,
            None,
        )
        .map(AlignmentPageIdentity::without_reference)
        .map_err(|missing| SemanticPageError::MissingEvidenceHandle(missing.0))
    }

    /// Builds the page from inputs gathered by [`Self::collect_inputs`] for the same roots.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) fn build_from_inputs(
        inputs: AlignmentPageInputs,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        reference: Option<&ReferenceSlice>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SemanticPageError> {
        let AlignmentPageInputs {
            key,
            identity,
            interval,
            rows,
            atlas_records,
            pair_connectors,
            _scratch: _inputs_scratch,
        } = inputs;
        let selection = evidence.select_handles_with_reservation_kind(
            interval,
            atlas_records.iter().map(|hit| hit.record.handle()),
            reservation_kind,
        )?;
        let _base_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            semantic_page_base_scratch_bytes(selection.len(), 0, 0),
            reservation_kind,
        )?;
        let mut by_handle = HashMap::with_capacity(selection.len());
        for (index, record) in selection.records().iter().enumerate() {
            by_handle.insert(record.handle, index);
        }

        let dynamic_scratch_bytes = semantic_page_dynamic_scratch_bytes(
            &selection,
            &atlas_records,
            &by_handle,
            key,
            presentation,
            interval,
        )?;
        let _dynamic_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            dynamic_scratch_bytes,
            reservation_kind,
        )?;

        // First pass computes an exact output upper bound without creating retained geometry.
        let mut maximum_primitives = pair_connectors.len();
        let mut maximum_labels = 0_usize;
        let mut maximum_label_bytes = 0_usize;
        let mut prepared =
            HashMap::<AlignmentHandle, PreparedRecordSemantics>::with_capacity(atlas_records.len());
        for hit in &atlas_records {
            let selection_index = *by_handle.get(&hit.record.handle()).ok_or(
                SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
            )?;
            let record = selection
                .record_view(selection_index)
                .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
            let body = if key.family == AlignmentPageFamily::ReadGeometry {
                body_color_token(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.color_mode,
                    // Only the pair classification and orientation are read, and neither
                    // depends on this interval (it decides mate visibility alone). Passing the
                    // tile keeps the atlas's prepared extent out of the page's inputs, so an
                    // extension cannot change a page it did not touch.
                    interval,
                    atlas.layout_contract().pair_length_bounds,
                )?
            } else {
                AlignmentBodyColorToken::Neutral
            };
            let read_label = if key.family == AlignmentPageFamily::Labels
                && matches!(key.lod, GeometryLod::Read | GeometryLod::Base)
            {
                read_label_for_record(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.read_labels,
                )?
            } else {
                None
            };
            let overlays = if key.family == AlignmentPageFamily::SpecialistOverlay
                && matches!(key.lod, GeometryLod::Read | GeometryLod::Base)
            {
                prepare_specialist_overlays(
                    &selection,
                    selection_index,
                    record,
                    presentation.style,
                    key.lod,
                    reference,
                    interval,
                )?
            } else {
                Vec::new()
            };
            let hidden_sbx_tail = if key.family == AlignmentPageFamily::ReadGeometry
                && matches!(
                    presentation.style.color_mode,
                    AlignmentColorMode::Sbx(options)
                        if options.simplex_tail_mode == SbxSimplexTailMode::Hide
                ) {
                hidden_sbx_tail_bounds(
                    &selection,
                    selection_index,
                    record,
                    presentation.style.color_mode,
                )
            } else {
                None
            };
            if let Some((_, label)) = &read_label {
                maximum_labels = maximum_labels.saturating_add(1);
                maximum_label_bytes = maximum_label_bytes.saturating_add(label.len());
            }
            // Only operations which can intersect this tile are counted. Counting the whole
            // CIGAR reserved (and allocated) every aligned base of a long read for every page
            // it touched.
            let bound = record_primitive_bound(
                record,
                interval,
                key.lod,
                presentation.style.minimum_event_label_size,
            );
            let event_labels = if key.family == AlignmentPageFamily::Labels {
                bound.event_labels
            } else {
                0
            };
            maximum_labels = maximum_labels.saturating_add(event_labels);
            // A length is at most ten digits plus a hard clip's `H`.
            maximum_label_bytes =
                maximum_label_bytes.saturating_add(event_labels.saturating_mul(11));
            maximum_primitives = maximum_primitives
                .saturating_add(overlays.len())
                .saturating_add(event_labels)
                .saturating_add(usize::from(read_label.is_some()))
                .saturating_add(if key.family == AlignmentPageFamily::ReadGeometry {
                    bound.geometry
                } else {
                    0
                });
            prepared.insert(
                hit.record.handle(),
                PreparedRecordSemantics {
                    body,
                    read_label,
                    overlays,
                    hidden_sbx_tail,
                },
            );
        }
        let row_offset_count = usize::try_from(rows.end.saturating_sub(rows.start))
            .map_err(|_| SemanticPageError::RowCoordinateOverflow)?
            .saturating_add(1);
        let maximum_bytes = semantic_page_bytes(
            maximum_primitives,
            row_offset_count,
            maximum_labels,
            maximum_labels,
            maximum_label_bytes,
        );
        let _output_scratch = ledger.reserve_transient(
            TransientResourceClass::SemanticPageBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::SemanticPage,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut primitives = Vec::with_capacity(maximum_primitives);
        let mut labels = Vec::<PendingLabel>::with_capacity(maximum_labels);
        let tile_start = interval.start();
        // Base-detail read geometry draws soft-clip ghost cells, whose letters are suppressed
        // where another read of the row covers them; that needs every record's extent.
        let clip_ghost_page =
            key.family == AlignmentPageFamily::ReadGeometry && key.lod == GeometryLod::Base;
        let mut row_extents = Vec::<(u32, u64, u64)>::with_capacity(if clip_ghost_page {
            atlas_records.len()
        } else {
            0
        });
        for hit in atlas_records {
            let selection_index = *by_handle.get(&hit.record.handle()).ok_or(
                SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
            )?;
            let record = selection
                .record_view(selection_index)
                .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
            let row_offset = hit.record.row().saturating_sub(rows.start);
            let prepared = prepared.remove(&hit.record.handle()).ok_or(
                SemanticPageError::MissingPreparedSemantics(hit.record.handle()),
            )?;
            if clip_ghost_page
                && let Some((start, end)) =
                    visible_reference_bounds(record, prepared.hidden_sbx_tail)
                && start.max(interval.start()) < end.min(interval.end())
            {
                row_extents.push((
                    row_offset,
                    start.max(interval.start()) - tile_start,
                    end.min(interval.end()) - tile_start,
                ));
            }
            emit_record_primitives(
                &mut primitives,
                &mut labels,
                hit.record.handle(),
                row_offset,
                record,
                selection.sequence(selection_index),
                prepared.body,
                key.lod,
                interval,
                tile_start,
                reference,
                presentation.style,
                prepared.read_label.as_ref(),
                key.family,
                prepared.hidden_sbx_tail,
            )?;
            let overlay_style = AlignmentStyleToken {
                flags: record.flags().bits(),
                mapping_quality: record.mapping_quality(),
                operation: None,
                body: prepared.body,
            };
            let first_record_overlay = primitives.len();
            for (draw_order, overlay) in prepared.overlays.into_iter().enumerate() {
                let first_overlay_primitive = primitives.len();
                push_span(
                    &mut primitives,
                    hit.record.handle(),
                    row_offset,
                    overlay.start,
                    overlay.end,
                    interval,
                    tile_start,
                    overlay.component,
                    overlay_style,
                    20_000_u32.saturating_add(u32::try_from(draw_order).unwrap_or(u32::MAX)),
                );
                if overlay.end_anchored {
                    for primitive in &mut primitives[first_overlay_primitive..] {
                        if primitive.geometry.is_point_marker() {
                            primitive.geometry = AlignmentGeometryToken::PointMarkerAtEnd;
                        }
                    }
                }
            }
            // Specialist cells near the read's tip are cut by its arrowhead like the base cells
            // of the read geometry page: the same true terminal, hidden SBX tail included.
            if primitives.len() > first_record_overlay
                && presentation.style.show_direction_terminals
            {
                let hidden_tail = if matches!(
                    presentation.style.color_mode,
                    AlignmentColorMode::Sbx(options)
                        if options.simplex_tail_mode == SbxSimplexTailMode::Hide
                ) {
                    hidden_sbx_tail_bounds(
                        &selection,
                        selection_index,
                        record,
                        presentation.style.color_mode,
                    )
                } else {
                    None
                };
                if let Some((visible_start, visible_end)) =
                    visible_reference_bounds(record, hidden_tail)
                {
                    let reverse = record.flags().is_reverse();
                    mark_arrowhead_cells(
                        &mut primitives[first_record_overlay..],
                        hit.record.handle(),
                        row_offset,
                        if reverse {
                            visible_start
                        } else {
                            visible_end.saturating_sub(1)
                        },
                        reverse,
                        tile_start,
                    );
                }
            }
        }
        for connector in pair_connectors {
            push_span(
                &mut primitives,
                connector.left(),
                connector.row().saturating_sub(rows.start),
                connector.start(),
                connector.end(),
                interval,
                tile_start,
                AlignmentComponent::PairConnector {
                    template: connector.template(),
                    mate: connector.right().map(|mate| {
                        // One atlas lays out one source: both ends share its revision.
                        debug_assert_eq!(
                            mate.source_revision(),
                            connector.left().source_revision()
                        );
                        mate.source_position()
                    }),
                },
                AlignmentStyleToken {
                    flags: 0,
                    mapping_quality: None,
                    operation: None,
                    body: AlignmentBodyColorToken::Neutral,
                },
                0,
            );
        }
        // Reservation invariant: the tile-clipped first pass must never undercount what the
        // second pass emits, otherwise the page outgrows what the ledger was asked to admit.
        debug_assert!(
            primitives.len() <= maximum_primitives,
            "semantic page emitted {} primitives but reserved {maximum_primitives}",
            primitives.len(),
        );
        debug_assert!(
            labels.len() <= maximum_labels,
            "semantic page emitted {} labels but reserved {maximum_labels}",
            labels.len(),
        );
        if clip_ghost_page {
            mark_obscured_clip_ghosts(&mut primitives, row_extents);
        }
        // Within a row, soft-clip ghost cells come first, beneath every read of the row, so a
        // neighbouring read covers them; boundary marks (point markers and clip cut edges) come
        // last: they sit on a base boundary and overhang the neighbouring cells, so they must
        // paint over the read body and bases on both sides (the GPU draws a page's instances in
        // this order). See `AlignmentGeometryToken::paint_layer`.
        primitives.sort_unstable_by_key(page_paint_key);
        let mut row_offsets = vec![0_u32; row_offset_count];
        let mut cursor = 0_usize;
        for row in 0..row_offset_count.saturating_sub(1) {
            row_offsets[row] =
                u32::try_from(cursor).map_err(|_| SemanticPageError::PrimitiveCountOverflow)?;
            while primitives
                .get(cursor)
                .is_some_and(|primitive| primitive.row_offset as usize == row)
            {
                cursor = cursor.saturating_add(1);
            }
        }
        if let Some(end) = row_offsets.last_mut() {
            *end = u32::try_from(primitives.len())
                .map_err(|_| SemanticPageError::PrimitiveCountOverflow)?;
        }
        let mut label_primitive_indices = primitives
            .iter()
            .enumerate()
            .filter_map(|(index, primitive)| {
                matches!(primitive.component, AlignmentComponent::Label { .. })
                    .then(|| u32::try_from(index))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SemanticPageError::PrimitiveCountOverflow)?;
        primitives.shrink_to_fit();
        row_offsets.shrink_to_fit();
        label_primitive_indices.shrink_to_fit();
        let labels = finalize_labels(labels)?;
        let retained_label_bytes = labels.iter().map(|label| label.text.len()).sum::<usize>();
        let retained_bytes = semantic_page_bytes(
            primitives.len(),
            row_offsets.len(),
            label_primitive_indices.len(),
            labels.len(),
            retained_label_bytes,
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            key,
            identity,
            interval,
            row_start: rows.start,
            row_end: rows.end,
            primitives: primitives.into(),
            row_offsets: row_offsets.into(),
            label_primitive_indices: label_primitive_indices.into(),
            labels: labels.into(),
            allocation,
        }))
    }

    /// The key this page was first built under. Its evidence and row-atlas revisions are the
    /// page's birth revisions: a scene published from later roots keeps this key for every page
    /// it reuses, which is what lets key-addressed derivatives (GPU pages) survive an extension.
    #[must_use]
    pub const fn key(&self) -> AlignmentPageKey {
        self.key
    }

    /// Revision-free content identity. Equal identities mean structurally equal pages.
    #[must_use]
    pub const fn identity(&self) -> AlignmentPageIdentity {
        self.identity
    }

    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub const fn row_range(&self) -> std::ops::Range<u32> {
        self.row_start..self.row_end
    }

    #[must_use]
    pub fn primitives(&self) -> &[AlignmentSemanticPrimitive] {
        &self.primitives
    }

    /// Number of primitives with GPU geometry. Text labels are projected by the CPU and have an
    /// immutable index of their own, so this is an O(1) classification rather than a scan of a
    /// potentially very deep label page.
    #[must_use]
    pub fn drawable_primitive_count(&self) -> usize {
        self.primitives
            .len()
            .saturating_sub(self.label_primitive_indices.len())
    }

    /// Whether this semantic page requires a GPU vertex buffer.
    #[must_use]
    pub fn has_drawable_primitives(&self) -> bool {
        self.drawable_primitive_count() > 0
    }

    /// Camera-visible text anchors prepared once with the immutable semantic page.
    pub fn label_anchors(&self) -> impl ExactSizeIterator<Item = &AlignmentSemanticPrimitive> {
        self.label_primitive_indices.iter().map(|index| {
            &self.primitives
                [usize::try_from(*index).expect("u32 semantic primitive indices always fit usize")]
        })
    }

    #[must_use]
    pub fn label_anchor_count(&self) -> usize {
        self.label_primitive_indices.len()
    }

    #[must_use]
    pub fn labels(&self) -> &[AlignmentSemanticLabel] {
        &self.labels
    }

    #[must_use]
    pub fn label(&self, id: ContractFingerprint) -> Option<&str> {
        self.labels
            .binary_search_by_key(&id, AlignmentSemanticLabel::id)
            .ok()
            .and_then(|index| self.labels.get(index))
            .map(AlignmentSemanticLabel::text)
    }

    #[must_use]
    pub fn label_arc(&self, id: ContractFingerprint) -> Option<Arc<str>> {
        self.labels
            .binary_search_by_key(&id, AlignmentSemanticLabel::id)
            .ok()
            .and_then(|index| self.labels.get(index))
            .map(|label| Arc::clone(&label.text))
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    #[must_use]
    pub fn pick(&self, row: u32, genomic_position: u64) -> Option<SemanticPageHit> {
        self.primitive_candidates(row, genomic_position)
            .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
            .max_by_key(|primitive| (primitive.draw_order, primitive.component))
            .map(|primitive| SemanticPageHit {
                handle: primitive.handle,
                component: primitive.component,
                draw_order: primitive.draw_order,
            })
    }

    /// Returns all typed components at a stable row/genomic position. The CPU picker refines
    /// these candidates against their projected component shapes without GPU readback.
    ///
    /// This iterator borrows the immutable page and performs no allocation. Interactive pointer
    /// motion must not allocate a temporary vector for every page under the cursor.
    pub fn primitive_candidates(
        &self,
        row: u32,
        genomic_position: u64,
    ) -> impl Iterator<Item = AlignmentSemanticPrimitive> + '_ {
        let candidates = if self.interval.contains(genomic_position) {
            self.row_primitives(row)
        } else {
            &[][..]
        };
        let offset = genomic_position.saturating_sub(self.interval.start());
        candidates
            .iter()
            .filter(move |primitive| primitive.contains_offset(offset))
            .copied()
    }

    /// [`Self::primitive_candidates`] plus every boundary mark (point marker or clip edge) of
    /// `row` whose one-base span intersects `marker_search`. A point marker is drawn centred on
    /// a base boundary, and a clip edge may sit behind an arrowhead, so either can overhang into
    /// neighbouring base cells (and, when zoomed out, possibly further) and be under the pointer
    /// while its own span is not. Like `primitive_candidates`, this borrows the page and
    /// allocates nothing.
    pub fn pick_candidates(
        &self,
        row: u32,
        genomic_position: u64,
        marker_search: Range<u64>,
    ) -> impl Iterator<Item = AlignmentSemanticPrimitive> + '_ {
        let page_start = self.interval.start();
        self.row_primitives(row)
            .iter()
            .filter(move |primitive| {
                let start = page_start.saturating_add(primitive.start_offset);
                let end = page_start.saturating_add(primitive.end_offset);
                (start <= genomic_position && genomic_position < end)
                    || (primitive.geometry.is_boundary_mark()
                        && start < marker_search.end
                        && marker_search.start < end)
            })
            .copied()
    }

    fn row_primitives(&self, row: u32) -> &[AlignmentSemanticPrimitive] {
        if row < self.row_start || row >= self.row_end {
            return &[];
        }
        usize::try_from(row - self.row_start)
            .ok()
            .and_then(|local_row| {
                let start = self.row_offsets.get(local_row).copied()?;
                let end = self.row_offsets.get(local_row.saturating_add(1)).copied()?;
                self.primitives.get(start as usize..end as usize)
            })
            .unwrap_or_default()
    }

    /// Compatibility collector for non-interactive callers and tests that need an owned list.
    #[must_use]
    pub fn primitives_at(
        &self,
        row: u32,
        genomic_position: u64,
    ) -> Vec<AlignmentSemanticPrimitive> {
        self.primitive_candidates(row, genomic_position).collect()
    }
}

fn body_color_token(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    mode: AlignmentColorMode,
    visible_interval: GenomicInterval,
    pair_bounds: TemplateLengthBounds,
) -> Result<AlignmentBodyColorToken, SemanticPageError> {
    let pair = || pair_evidence_view(record, visible_interval, pair_bounds);
    Ok(match mode {
        AlignmentColorMode::None => AlignmentBodyColorToken::Neutral,
        AlignmentColorMode::Strand => AlignmentBodyColorToken::Strand {
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::MappingQuality => {
            AlignmentBodyColorToken::MappingQuality(record.mapping_quality())
        }
        AlignmentColorMode::ReadGroup => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, *b"RG")?)
        }
        AlignmentColorMode::Haplotype => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, *b"HP")?)
        }
        AlignmentColorMode::TemplateLength => AlignmentBodyColorToken::TemplateLength {
            absolute: record.template_length().unsigned_abs(),
            status: classification_rank(pair().classification),
        },
        AlignmentColorMode::PairOrientation => {
            AlignmentBodyColorToken::PairOrientation(orientation_rank(pair().orientation))
        }
        AlignmentColorMode::UnexpectedPair => {
            AlignmentBodyColorToken::PairStatus(classification_rank(pair().classification))
        }
        AlignmentColorMode::MateContig => {
            let category = record.mate_contig().map(|contig| {
                let mut hash = StableHasher::default();
                hash.write(b"mate-contig\0");
                hash.write(&contig.get().to_le_bytes());
                hash.finish()
            });
            AlignmentBodyColorToken::Category(category)
        }
        AlignmentColorMode::FirstOfPairStrand => {
            AlignmentBodyColorToken::FirstOfPairStrand(first_of_pair_reverse(record.flags()))
        }
        AlignmentColorMode::Bisulfite(_) => AlignmentBodyColorToken::Specialist {
            mode: 0,
            reverse: bisulfite_strand_is_reverse(record.flags()),
        },
        AlignmentColorMode::SmrtKinetics(_) => AlignmentBodyColorToken::Specialist {
            mode: 1,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Ultima(_) => AlignmentBodyColorToken::Specialist {
            mode: 2,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Sbx(_) => AlignmentBodyColorToken::Specialist {
            mode: 3,
            reverse: record.flags().is_reverse(),
        },
        AlignmentColorMode::Tag(tag) => {
            AlignmentBodyColorToken::Category(auxiliary_category(selection, index, tag)?)
        }
    })
}

fn auxiliary_category(
    selection: &crate::EvidenceSelection,
    index: usize,
    tag: [u8; 2],
) -> Result<Option<ContractFingerprint>, SemanticPageError> {
    let Some(field) = selection.auxiliary_field(index, tag)? else {
        return Ok(None);
    };
    let mut hash = StableHasher::default();
    hash.write(b"alignment-category\0");
    hash.write(&tag);
    let supported = match field.value {
        AuxValue::Character(value) => {
            hash.write(&[0, value]);
            true
        }
        AuxValue::Signed(value) => {
            hash.write(&[1]);
            hash.write(&value.to_le_bytes());
            true
        }
        AuxValue::Unsigned(value) => {
            hash.write(&[2]);
            hash.write(&value.to_le_bytes());
            true
        }
        AuxValue::Float(value) => {
            hash.write(&[3]);
            hash.write(&value.to_bits().to_le_bytes());
            true
        }
        AuxValue::Double(value) => {
            hash.write(&[4]);
            hash.write(&value.to_bits().to_le_bytes());
            true
        }
        AuxValue::String(value) => {
            hash.write(&[5]);
            hash.write(value.as_bytes());
            true
        }
        AuxValue::HexByteArray(value) => {
            hash.write(&[6]);
            hash.write(value.as_bytes());
            true
        }
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => false,
    };
    Ok(supported.then(|| hash.finish()))
}

fn first_of_pair_reverse(flags: rgv_model::AlignmentFlags) -> Option<bool> {
    if flags.is_first_in_template() {
        Some(flags.is_reverse())
    } else if flags.is_last_in_template() {
        Some(flags.is_mate_reverse())
    } else {
        None
    }
}

const fn orientation_rank(value: PairOrientation) -> u8 {
    match value {
        PairOrientation::Inward => 0,
        PairOrientation::Outward => 1,
        PairOrientation::SameStrand => 2,
        PairOrientation::Unknown => 3,
    }
}

const fn classification_rank(value: PairClassification) -> u8 {
    match value {
        PairClassification::Proper => 0,
        PairClassification::Concordant => 1,
        PairClassification::DiscordantTemplateLengthShort => 2,
        PairClassification::DiscordantTemplateLengthLong => 3,
        PairClassification::DiscordantOrientation => 4,
        PairClassification::InterContig => 5,
        PairClassification::MateUnmapped => 6,
        PairClassification::MateMissing => 7,
        PairClassification::ReadUnmapped => 8,
        PairClassification::Unpaired => 9,
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_specialist_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    style: AlignmentStyleContract,
    lod: GeometryLod,
    reference: Option<&ReferenceSlice>,
    tile: GenomicInterval,
) -> Result<Vec<PreparedOverlay>, SemanticPageError> {
    let mut output = Vec::new();
    if style.show_modified_bases {
        // Mismatch cells are drawn at base detail only, and only when bases are shown at all;
        // there a mismatch takes priority over a modification on the same base.
        let mismatches = style
            .modification_overlay_reads_reference(lod)
            .then_some(MismatchPriority { reference });
        prepare_modified_base_overlays(
            selection,
            index,
            record,
            style,
            mismatches,
            tile,
            &mut output,
        )?;
    }
    match style.color_mode {
        AlignmentColorMode::Bisulfite(context) => {
            prepare_bisulfite_overlays(
                selection,
                index,
                record,
                reference,
                context,
                tile,
                &mut output,
            );
        }
        AlignmentColorMode::SmrtKinetics(mode) => {
            prepare_smrt_overlays(selection, index, record, mode, tile, &mut output)?;
        }
        AlignmentColorMode::Ultima(options) => {
            prepare_ultima_overlays(
                selection,
                index,
                record,
                reference,
                options,
                tile,
                &mut output,
            )?;
        }
        AlignmentColorMode::Sbx(options) => {
            prepare_sbx_overlays(selection, index, record, options, tile, &mut output);
        }
        _ => {}
    }
    output.sort_unstable_by_key(|overlay| (overlay.start, overlay.end, overlay.component));
    Ok(output)
}

/// The reference against which modification cells give way to mismatch cells. `reference` is
/// `None` while no FASTA slice is available: then only `X` operations are mismatches, exactly as
/// the read geometry classifies them.
#[derive(Clone, Copy)]
struct MismatchPriority<'a> {
    reference: Option<&'a ReferenceSlice>,
}

impl MismatchPriority<'_> {
    /// Whether the read geometry draws the base `base` aligned at `position` by an operation of
    /// `kind` as a mismatch cell.
    fn is_mismatch(self, kind: CigarOpKind, base: u8, position: u64) -> bool {
        !matches!(
            classify_aligned_base(
                kind,
                Some(base),
                self.reference
                    .and_then(|reference| reference.base_at(position))
            ),
            AlignedBaseClassification::Match | AlignedBaseClassification::Unknown
        )
    }
}

/// Every call of one record at one base: what decides the cell drawn there.
#[derive(Clone, Copy)]
struct BaseModificationCalls {
    canonical_base: u8,
    /// Summed probability (of 255) of every modification called at the base; a call without
    /// `ML` counts as certain.
    total: u32,
    /// The likeliest call of a shown code: its code, `ML` value and effective probability.
    best_shown: Option<(rgv_model::ModificationCode, Option<u8>, u8)>,
    /// The effective probability of the likeliest call of a hidden code.
    best_hidden: u8,
}

/// The modification cells of one record, one per called base, showing its likelier state as IGV
/// does: the likeliest shown modification when it is at least as likely as no modification,
/// else "unmodified" (blue). A record with no usable calls (no `MM` tag at all, or an `MM` tag
/// that cannot be placed on its stored sequence, as on a hard-clipped supplementary alignment
/// without a matching `MN`, or one that cannot be parsed) gets no overlay at all and is drawn
/// as an ordinary read; the read inspector says why it has no calls.
#[allow(clippy::too_many_lines)]
fn prepare_modified_base_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    style: AlignmentStyleContract,
    mismatches: Option<MismatchPriority<'_>>,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return Ok(());
    };
    let mm_upper = selection.auxiliary_field(index, *b"MM")?;
    let mm_lower = selection.auxiliary_field(index, *b"Mm")?;
    let Some(mm) = mm_upper.as_ref().or(mm_lower.as_ref()) else {
        return Ok(());
    };
    let ml_upper = selection.auxiliary_field(index, *b"ML")?;
    let ml_lower = selection.auxiliary_field(index, *b"Ml")?;
    let ml = ml_upper.as_ref().or(ml_lower.as_ref());
    // `MM` counts bases of the sequence it was written against. A hard-clipped record (or one
    // whose `MN` disagrees with the stored sequence) would place every call on the wrong base,
    // so it has no usable calls: the same decision the exact modification summary makes.
    let mn = selection.auxiliary_field(index, *b"MN")?;
    let mn = mn.as_ref().map(|field| &field.value);
    let hard_clipped = record
        .cigar()
        .iter()
        .any(|operation| operation.kind == CigarOpKind::HardClip);
    if !modified_bases_resolvable(bases.len(), mn, hard_clipped) {
        return Ok(());
    }
    let Ok(calls) = decode_modified_bases_parts_checked(
        bases,
        record.flags(),
        &mm.value,
        ml.map(|field| &field.value),
        mn,
        hard_clipped,
    ) else {
        return Ok(());
    };
    let color_by_type = style.modified_base_color_mode == ModifiedBaseColorMode::ByType;
    let minimum = style.minimum_modification_probability;
    let hidden = |code| style.hidden_modification_codes.contains(&Some(code));
    let mut called = std::collections::BTreeMap::<usize, BaseModificationCalls>::new();
    let mut canonical_groups = BTreeSet::<(u8, rgv_model::ModificationCode)>::new();
    for call in &calls {
        let effective = call.probability.unwrap_or(u8::MAX);
        let base = called
            .entry(call.sequencer_position)
            .or_insert(BaseModificationCalls {
                canonical_base: call.canonical_base,
                total: 0,
                best_shown: None,
                best_hidden: 0,
            });
        base.total = base.total.saturating_add(u32::from(effective));
        if hidden(call.code) {
            base.best_hidden = base.best_hidden.max(effective);
            continue;
        }
        if base.best_shown.is_none_or(|(_, _, best)| effective > best) {
            base.best_shown = Some((call.code, call.probability, effective));
        }
        if style.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified
            && call.implicit_mode == ImplicitModificationMode::Canonical
        {
            canonical_groups.insert((call.canonical_base, call.code));
        }
    }
    let mut push = |sequencer_position: usize,
                    canonical_base: u8,
                    code: rgv_model::ModificationCode,
                    probability: Option<u8>,
                    state: ModificationState|
     -> Result<(), SemanticPageError> {
        let Some(read_index) = aligned_position_for_sequencer_position(
            bases.len(),
            record.flags(),
            sequencer_position,
        ) else {
            return Ok(());
        };
        let Some((position, kind)) =
            reference_position_for_read_position(record.start(), record.cigar(), read_index)
        else {
            return Ok(());
        };
        if !tile.contains(position)
            || mismatches.is_some_and(|mismatches| {
                bases
                    .get(read_index)
                    .is_some_and(|base| mismatches.is_mismatch(kind, *base, position))
            })
        {
            return Ok(());
        }
        output.push(PreparedOverlay {
            end_anchored: false,
            start: position,
            end: position.saturating_add(1),
            component: AlignmentComponent::ModifiedBase {
                read_index: u32::try_from(read_index)
                    .map_err(|_| SemanticPageError::ReadCoordinateOverflow)?,
                canonical_base,
                code,
                probability,
                state,
                color_by_type,
            },
        });
        Ok(())
    };
    for (&sequencer_position, base) in &called {
        let Some((code, probability, best)) = base.best_shown else {
            continue;
        };
        let modified = u8::try_from(base.total.min(u32::from(u8::MAX))).unwrap_or(u8::MAX);
        let unmodified = u8::MAX - modified;
        if best >= unmodified && best >= base.best_hidden {
            if minimum == 0 || probability.is_some_and(|probability| probability >= minimum) {
                push(
                    sequencer_position,
                    base.canonical_base,
                    code,
                    probability,
                    ModificationState::Modified,
                )?;
            }
        } else if unmodified > best && unmodified >= base.best_hidden && unmodified >= minimum {
            push(
                sequencer_position,
                base.canonical_base,
                code,
                Some(modified),
                ModificationState::Canonical,
            )?;
        }
    }
    // Under `.` (implicitly unmodified) groups, every uncalled base of the canonical base is
    // unmodified; the modified-versus-unmodified mode draws those too.
    for (canonical_base, code) in canonical_groups {
        for sequencer_position in 0..bases.len() {
            if called.contains_key(&sequencer_position)
                || !sequencer_base_matches(
                    bases,
                    record.flags(),
                    sequencer_position,
                    canonical_base,
                )
            {
                continue;
            }
            push(
                sequencer_position,
                canonical_base,
                code,
                None,
                ModificationState::Canonical,
            )?;
        }
    }
    Ok(())
}

fn prepare_bisulfite_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    reference: Option<&ReferenceSlice>,
    context: rgv_core::BisulfiteContext,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) {
    let Some(reference) = reference else {
        push_unavailable(
            output,
            record,
            tile,
            SpecialistMode::Bisulfite,
            SpecialistUnavailableReason::MissingReference,
        );
        return;
    };
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return;
    };
    let reverse = bisulfite_strand_is_reverse(record.flags());
    for_each_aligned_base(record, |read_index, position, _| {
        let Some(base) = bases.get(read_index).copied() else {
            return;
        };
        let observation =
            classify_bisulfite_observation(reference, position, reverse, context, base);
        let observation = match observation {
            Some(rgv_model::BisulfiteObservation::Protected) => {
                Some(BisulfiteObservationToken::Protected)
            }
            Some(rgv_model::BisulfiteObservation::Converted) => {
                Some(BisulfiteObservationToken::Converted)
            }
            None => None,
        };
        if tile.contains(position)
            && let Some(observation) = observation
            && let Ok(read_index) = u32::try_from(read_index)
        {
            output.push(PreparedOverlay {
                end_anchored: false,
                start: position,
                end: position.saturating_add(1),
                component: AlignmentComponent::BisulfiteBase {
                    read_index,
                    observation,
                },
            });
        }
    });
}

fn prepare_smrt_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    mode: rgv_core::SmrtKineticsMode,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let Some(SequenceView::Present { bases, .. }) = selection.sequence(index) else {
        return Ok(());
    };
    let tag = smrt_kinetics_tag(mode, record.flags().is_reverse());
    let Some(field) = selection.auxiliary_field(index, tag)? else {
        return Ok(());
    };
    let series = match decode_smrt_kinetics(tag, &field.value, bases.len()) {
        Ok(series) => series,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::SmrtKinetics,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return Ok(());
        }
    };
    for_each_aligned_base(record, |read_index, position, _| {
        if !tile.contains(position) {
            return;
        }
        if let (Some(value), Ok(read_index)) = (
            series.value_at_aligned_read_position(record.flags(), read_index),
            u32::try_from(read_index),
        ) {
            output.push(PreparedOverlay {
                end_anchored: false,
                start: position,
                end: position.saturating_add(1),
                component: AlignmentComponent::SmrtKinetics {
                    read_index,
                    tag,
                    value,
                },
            });
        }
    });
    Ok(())
}

fn prepare_ultima_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    reference: Option<&ReferenceSlice>,
    options: rgv_core::UltimaDisplayOptions,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) -> Result<(), SemanticPageError> {
    let materialized = selection.materialize_retained_record(index)?;
    let analysis = match analyze_ultima_record(
        &materialized,
        reference,
        options.quality_mode,
        options.maximum_homopolymer_length_override,
    ) {
        Ok(analysis) => analysis,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::Ultima,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return Ok(());
        }
    };
    for event in analysis.events {
        let end = match event.kind {
            UltimaIndelKind::Insertion => event.reference_start.saturating_add(1),
            UltimaIndelKind::Deletion => event
                .reference_start
                .saturating_add(u64::try_from(event.length).unwrap_or(u64::MAX)),
        };
        if event.reference_start >= tile.end() || end <= tile.start() {
            continue;
        }
        output.push(PreparedOverlay {
            end_anchored: false,
            start: event.reference_start,
            end,
            component: AlignmentComponent::UltimaIndel {
                cigar_index: u32::try_from(event.operation_index)
                    .map_err(|_| SemanticPageError::CigarIndexOverflow)?,
                kind: match event.kind {
                    UltimaIndelKind::Insertion => UltimaIndelToken::Insertion,
                    UltimaIndelKind::Deletion => UltimaIndelToken::Deletion,
                },
                quality_bits: event.quality.map(f32::to_bits),
                quality_source: event.quality_source.map(map_ultima_quality_source),
                unavailable: event.unavailable.map(map_ultima_unavailable),
            },
        });
    }
    Ok(())
}

fn prepare_sbx_overlays(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    options: rgv_core::SbxDisplayOptions,
    tile: GenomicInterval,
    output: &mut Vec<PreparedOverlay>,
) {
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = selection.sequence(index)
    else {
        return;
    };
    let tails = match sbx_tail_bounds(
        record.flags(),
        bases,
        qualities,
        options.simplex_tail_quality_maximum,
    ) {
        Ok(tails) => tails,
        Err(_) => {
            push_unavailable(
                output,
                record,
                tile,
                SpecialistMode::Sbx,
                SpecialistUnavailableReason::MalformedRecordData,
            );
            return;
        }
    };
    if options.simplex_tail_mode != SbxSimplexTailMode::Off {
        for_each_aligned_base(record, |read_index, position, _| {
            if tails.is_tail(read_index)
                && tile.contains(position)
                && let Ok(read_index) = u32::try_from(read_index)
            {
                output.push(PreparedOverlay {
                    end_anchored: false,
                    start: position,
                    end: position.saturating_add(1),
                    component: AlignmentComponent::SbxTailBase { read_index },
                });
            }
        });
    }
    let mut reference_position = record.start();
    let mut read_position = 0_usize;
    for (cigar_index, operation) in record.cigar().iter().enumerate() {
        match operation.kind {
            CigarOpKind::Insertion => {
                if tile.contains(reference_position) {
                    let state = classify_sbx_insertion(
                        record.flags(),
                        bases,
                        qualities,
                        read_position,
                        operation.len as usize,
                        options.discordant_quality_maximum,
                    )
                    .map_or(
                        SbxInsertionToken::Unavailable,
                        |state| match state {
                            SbxInsertionState::Concordant => SbxInsertionToken::Concordant,
                            SbxInsertionState::PartlyDiscordant => {
                                SbxInsertionToken::PartlyDiscordant
                            }
                            SbxInsertionState::FullyDiscordant => {
                                SbxInsertionToken::FullyDiscordant
                            }
                        },
                    );
                    if let Ok(cigar_index) = u32::try_from(cigar_index) {
                        output.push(PreparedOverlay {
                            end_anchored: reference_position >= record.end(),
                            start: reference_position.min(record.end().saturating_sub(1)),
                            end: reference_position
                                .min(record.end().saturating_sub(1))
                                .saturating_add(1),
                            component: AlignmentComponent::SbxInsertion { cigar_index, state },
                        });
                    }
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
}

fn hidden_sbx_tail_bounds(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    color_mode: AlignmentColorMode,
) -> Option<SbxTailBounds> {
    let AlignmentColorMode::Sbx(options) = color_mode else {
        return None;
    };
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = selection.sequence(index)
    else {
        return None;
    };
    sbx_tail_bounds(
        record.flags(),
        bases,
        qualities,
        options.simplex_tail_quality_maximum,
    )
    .ok()
}

fn push_unavailable(
    output: &mut Vec<PreparedOverlay>,
    record: rgv_model::AlignmentRecordView<'_>,
    tile: GenomicInterval,
    mode: SpecialistMode,
    reason: SpecialistUnavailableReason,
) {
    let start = record.start().max(tile.start());
    let end = record.end().min(tile.end());
    if start < end {
        output.push(PreparedOverlay {
            end_anchored: false,
            start,
            end,
            component: AlignmentComponent::SpecialistUnavailable { mode, reason },
        });
    }
}

fn for_each_aligned_base(
    record: rgv_model::AlignmentRecordView<'_>,
    mut visit: impl FnMut(usize, u64, usize),
) {
    let mut reference_position = record.start();
    let mut read_position = 0_usize;
    for (cigar_index, operation) in record.cigar().iter().enumerate() {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                for offset in 0..operation.len as usize {
                    visit(
                        read_position.saturating_add(offset),
                        reference_position.saturating_add(offset as u64),
                        cigar_index,
                    );
                }
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
}

/// The reference position of the aligned read base `target` and the CIGAR operation aligning
/// it, or `None` for a base no `M`, `=` or `X` operation aligns.
fn reference_position_for_read_position(
    alignment_start: u64,
    cigar: &[rgv_model::CigarOp],
    target: usize,
) -> Option<(u64, CigarOpKind)> {
    let mut reference_position = alignment_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let read_end = read_position.saturating_add(operation.len as usize);
                if (read_position..read_end).contains(&target) {
                    return Some((
                        reference_position.saturating_add((target - read_position) as u64),
                        operation.kind,
                    ));
                }
                read_position = read_end;
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    None
}

const fn map_ultima_quality_source(value: UltimaQualitySource) -> UltimaQualitySourceToken {
    match value {
        UltimaQualitySource::TpDirection => UltimaQualitySourceToken::TpDirection,
        UltimaQualitySource::TpReadMaximumFallback => {
            UltimaQualitySourceToken::TpReadMaximumFallback
        }
        UltimaQualitySource::MinimumHomopolymer => UltimaQualitySourceToken::MinimumHomopolymer,
        UltimaQualitySource::T0 => UltimaQualitySourceToken::T0,
        UltimaQualitySource::MaximumHomopolymerLimit => {
            UltimaQualitySourceToken::MaximumHomopolymerLimit
        }
    }
}

const fn map_ultima_unavailable(value: UltimaEventUnavailable) -> UltimaUnavailableToken {
    match value {
        UltimaEventUnavailable::NotBracketedByAlignedBases => {
            UltimaUnavailableToken::NotBracketedByAlignedBases
        }
        UltimaEventUnavailable::ReferenceUnavailable => {
            UltimaUnavailableToken::ReferenceUnavailable
        }
        UltimaEventUnavailable::AmbiguousHomopolymer => {
            UltimaUnavailableToken::AmbiguousHomopolymer
        }
        UltimaEventUnavailable::NonHomopolymer => UltimaUnavailableToken::NonHomopolymer,
        UltimaEventUnavailable::T0Unavailable => UltimaUnavailableToken::T0Unavailable,
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn emit_record_primitives(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    record: rgv_model::AlignmentRecordView<'_>,
    sequence: Option<SequenceView<'_>>,
    body: AlignmentBodyColorToken,
    lod: GeometryLod,
    tile: GenomicInterval,
    tile_start: u64,
    reference: Option<&ReferenceSlice>,
    style_contract: AlignmentStyleContract,
    read_label: Option<&(AlignmentLabelKind, String)>,
    family: AlignmentPageFamily,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Result<(), SemanticPageError> {
    let emit_geometry = family == AlignmentPageFamily::ReadGeometry;
    let emit_labels = family == AlignmentPageFamily::Labels;
    // Each handle is emitted exactly once per page, so this record's primitives are precisely
    // those appended from here on.
    let first_record_primitive = output.len();
    let style = AlignmentStyleToken {
        flags: record.flags().bits(),
        mapping_quality: record.mapping_quality(),
        operation: None,
        body,
    };
    let mut draw_order = 0_u32;
    if lod == GeometryLod::Overview {
        if emit_geometry
            && let Some((visible_start, visible_end)) =
                visible_reference_bounds(record, hidden_sbx_tail)
        {
            push_span(
                output,
                handle,
                row_offset,
                visible_start,
                visible_end,
                tile,
                tile_start,
                AlignmentComponent::ReadSpan,
                style,
                draw_order,
            );
        }
        return Ok(());
    }

    if emit_labels && let Some((kind, text)) = read_label {
        push_label_span(
            output,
            labels,
            handle,
            row_offset,
            record.start(),
            record.end(),
            tile,
            tile_start,
            *kind,
            text,
            style,
            10_000,
        )?;
    }

    // The read's visible extent decides where its single direction arrowhead goes (the true
    // terminal: the right end of a forward read, the left end of a reverse one), and so whether
    // a clip's cut edge shares its boundary with that arrowhead.
    let visible_bounds = if emit_geometry {
        visible_reference_bounds(record, hidden_sbx_tail)
    } else {
        None
    };
    let arrow_bounds = visible_bounds.filter(|_| style_contract.show_direction_terminals);
    let reverse = record.flags().is_reverse();
    let arrow_at_start = reverse && arrow_bounds.is_some_and(|(start, _)| start == record.start());
    let arrow_at_end = !reverse && arrow_bounds.is_some_and(|(_, end)| end == record.end());
    // Soft-clipped bases are drawn as faint ghost cells beyond the cut edge only at base-level
    // detail, when read bases are shown at all and the record carries its sequence.
    let clip_ghosts_drawn = lod == GeometryLod::Base
        && style_contract.base_display != ReadBaseDisplayMode::Hidden
        && matches!(sequence, Some(SequenceView::Present { .. }));

    let mut reference_position = record.start();
    let mut read_position = 0_u32;
    let cigar = record.cigar();
    if emit_geometry && cigar.is_empty() && hidden_sbx_tail.is_none() {
        push_span(
            output,
            handle,
            row_offset,
            record.start(),
            record.end(),
            tile,
            tile_start,
            AlignmentComponent::ReadSpan,
            style,
            draw_order,
        );
    }
    for (index, operation) in cigar.iter().copied().enumerate() {
        let cigar_index =
            u32::try_from(index).map_err(|_| SemanticPageError::CigarIndexOverflow)?;
        let operation_style = AlignmentStyleToken {
            operation: Some(operation.kind.into()),
            ..style
        };
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry
                    && let Some((visible_read_start, visible_read_end)) =
                        visible_read_range(read_position, operation.len, hidden_sbx_tail)
                {
                    let visible_start = reference_position
                        .saturating_add(u64::from(visible_read_start - read_position));
                    let visible_end = reference_position
                        .saturating_add(u64::from(visible_read_end - read_position));
                    push_span(
                        output,
                        handle,
                        row_offset,
                        visible_start,
                        visible_end,
                        tile,
                        tile_start,
                        AlignmentComponent::AlignedSegment {
                            cigar_index,
                            operation: operation.kind.into(),
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_geometry && lod == GeometryLod::Base {
                    emit_bases(
                        output,
                        handle,
                        row_offset,
                        cigar_index,
                        operation.kind,
                        reference_position,
                        read_position,
                        operation.len,
                        sequence,
                        tile,
                        tile_start,
                        reference,
                        style_contract.base_display,
                        hidden_sbx_tail,
                        operation_style,
                        &mut draw_order,
                    )?;
                }
                reference_position = end;
                read_position = read_position.saturating_add(operation.len);
            }
            CigarOpKind::Deletion => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry && operation.len >= style_contract.minimum_indel_size {
                    push_span(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentComponent::Deletion { cigar_index },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_span(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentLabelKind::DeletionLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if emit_geometry {
                    push_span(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentComponent::ReferenceSkip { cigar_index },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    push_label_span(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        end,
                        tile,
                        tile_start,
                        AlignmentLabelKind::SkipLength,
                        &operation.len.to_string(),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::Insertion => {
                if emit_geometry
                    && operation.len >= style_contract.minimum_indel_size
                    && visible_read_range(read_position, operation.len, hidden_sbx_tail).is_some()
                {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::Insertion {
                            cigar_index,
                            read_start: read_position,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
                // Every insertion drawn is labelled; one shorter than the minimum event label
                // size only at base-letter detail (see `place_insertion_label`). Its bases come
                // from the record's own SEQ.
                if emit_labels && operation.len >= style_contract.minimum_indel_size {
                    let inserted = match sequence {
                        Some(SequenceView::Present { bases, .. }) => {
                            let start = read_position as usize;
                            bases.get(start..start.saturating_add(operation.len as usize))
                        }
                        _ => None,
                    };
                    push_label_point(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        if operation.len >= style_contract.minimum_event_label_size {
                            AlignmentLabelKind::InsertionLength
                        } else {
                            AlignmentLabelKind::ShortInsertion
                        },
                        &crate::insertion_label_text(inserted, operation.len),
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                read_position = read_position.saturating_add(operation.len);
            }
            CigarOpKind::SoftClip | CigarOpKind::HardClip => {
                let hard = operation.kind == CigarOpKind::HardClip;
                // A clip lies on the read's left boundary (its position is the first aligned
                // base) or right boundary (one past the last aligned base).
                let at_end = reference_position >= record.end();
                let edge = ClipEdgeToken {
                    at_end,
                    hard,
                    behind_arrow: if at_end { arrow_at_end } else { arrow_at_start },
                };
                let component = if hard {
                    AlignmentComponent::HardClip {
                        cigar_index,
                        length: operation.len,
                    }
                } else {
                    AlignmentComponent::SoftClip {
                        cigar_index,
                        read_start: read_position,
                        length: operation.len,
                    }
                };
                let ghosts = !hard && clip_ghosts_drawn;
                if emit_geometry
                    && (hard
                        || visible_read_range(read_position, operation.len, hidden_sbx_tail)
                            .is_some())
                {
                    push_clip_edge(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        component,
                        edge,
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                    if ghosts {
                        emit_clip_ghosts(
                            output,
                            handle,
                            row_offset,
                            cigar_index,
                            reference_position,
                            at_end,
                            read_position,
                            operation.len,
                            sequence,
                            tile,
                            tile_start,
                            hidden_sbx_tail,
                            operation_style,
                        )?;
                    }
                }
                if emit_labels && operation.len >= style_contract.minimum_event_label_size {
                    let text = if hard {
                        format!("{}H", operation.len)
                    } else {
                        operation.len.to_string()
                    };
                    push_clip_length_label(
                        output,
                        labels,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        if ghosts { operation.len } else { 0 },
                        tile,
                        tile_start,
                        &text,
                        edge,
                        operation_style,
                        draw_order,
                    )?;
                    draw_order = draw_order.saturating_add(1);
                }
                if !hard {
                    read_position = read_position.saturating_add(operation.len);
                }
            }
            CigarOpKind::Padding => {
                if emit_geometry {
                    push_point(
                        output,
                        handle,
                        row_offset,
                        reference_position,
                        record.start(),
                        record.end(),
                        tile,
                        tile_start,
                        AlignmentComponent::Padding {
                            cigar_index,
                            length: operation.len,
                        },
                        operation_style,
                        draw_order,
                    );
                    draw_order = draw_order.saturating_add(1);
                }
            }
        }
    }
    if let Some((visible_start, visible_end)) = arrow_bounds {
        let terminal = if reverse {
            visible_start
        } else {
            visible_end.saturating_sub(1)
        };
        push_point(
            output,
            handle,
            row_offset,
            terminal,
            record.start(),
            record.end(),
            tile,
            tile_start,
            AlignmentComponent::DirectionTerminal,
            style,
            draw_order,
        );
        mark_directional_read_geometry(
            &mut output[first_record_primitive..],
            handle,
            row_offset,
            terminal,
            reverse,
            tile,
            tile_start,
        );
    }
    Ok(())
}

/// Shapes the read's single arrowhead: every body primitive of this record that covers the
/// read's true terminal base (`terminal`: the last visible aligned base of a forward read, the
/// first of a reverse one). Only that end of the read carries the arrow; the inner ends of the
/// blocks of a spliced read stay square, so the read still reads as one molecule.
///
/// `output` must be the primitives of this one record. Scanning the whole page here made page
/// construction quadratic in the number of reads.
///
/// Every other rectangle of the record whose tip-side edge is within the longest possible head
/// (a mismatch cell a few bases in, a block piece ending at an insertion or a tile boundary near
/// the tip) is marked by [`mark_arrowhead_cells`], so the arrow cuts it too.
fn mark_directional_read_geometry(
    output: &mut [AlignmentSemanticPrimitive],
    handle: AlignmentHandle,
    row_offset: u32,
    terminal: u64,
    reverse: bool,
    tile: GenomicInterval,
    tile_start: u64,
) {
    if tile.contains(terminal) {
        let geometry = if reverse {
            AlignmentGeometryToken::DirectionalTerminalLeft
        } else {
            AlignmentGeometryToken::DirectionalTerminalRight
        };
        let offset = terminal - tile_start;
        for primitive in output.iter_mut().filter(|primitive| {
            primitive.handle == handle
                && primitive.row_offset == row_offset
                && primitive.contains_offset(offset)
                && matches!(
                    primitive.component,
                    AlignmentComponent::ReadSpan
                        | AlignmentComponent::AlignedSegment { .. }
                        | AlignmentComponent::Base { .. }
                        | AlignmentComponent::DirectionTerminal
                )
        }) {
            primitive.geometry = geometry;
        }
    }
    mark_arrowhead_cells(output, handle, row_offset, terminal, reverse, tile_start);
}

/// Marks every plain rectangle of one record (`output` holds only its primitives) whose
/// tip-side edge is at most [`ArrowheadCellToken::MAXIMUM_BASES_FROM_TIP`] bases from the read's
/// tip as [`AlignmentGeometryToken::InsideArrowhead`]: base, mismatch, modification, bisulfite,
/// kinetics, SBX and other specialist cells, and aligned block pieces that end short of the tip.
/// `terminal` is the read's true terminal base (see [`mark_directional_read_geometry`]); the
/// tip is that base's end edge when the read points right, its start edge otherwise. Rectangles
/// reaching past the tip (a cell of a hidden SBX tail) are beyond the arrow and stay as they
/// are, as do boundary marks, event lines and labels.
fn mark_arrowhead_cells(
    output: &mut [AlignmentSemanticPrimitive],
    handle: AlignmentHandle,
    row_offset: u32,
    terminal: u64,
    reverse: bool,
    tile_start: u64,
) {
    let tip = if reverse {
        terminal
    } else {
        terminal.saturating_add(1)
    };
    for primitive in output.iter_mut().filter(|primitive| {
        primitive.handle == handle
            && primitive.row_offset == row_offset
            && primitive.geometry == AlignmentGeometryToken::Rectangle
            && !matches!(primitive.component, AlignmentComponent::Label { .. })
    }) {
        let bases_from_tip = if reverse {
            tile_start
                .saturating_add(primitive.start_offset)
                .checked_sub(tip)
        } else {
            tip.checked_sub(tile_start.saturating_add(primitive.end_offset))
        };
        if let Some(token) = bases_from_tip
            .and_then(|bases| u8::try_from(bases).ok())
            .and_then(|bases| ArrowheadCellToken::new(!reverse, bases))
        {
            primitive.geometry = AlignmentGeometryToken::InsideArrowhead(token);
        }
    }
}

/// Reference interval `[start, end)` of the ghost cells of a soft clip of `length` bases on the
/// read boundary `boundary`: just before it for a leading clip, from it onwards for a trailing
/// one (`at_end`). Not clipped to any tile.
fn clip_ghost_span(boundary: u64, at_end: bool, length: u32) -> (u64, u64) {
    if at_end {
        (boundary, boundary.saturating_add(u64::from(length)))
    } else {
        (boundary.saturating_sub(u64::from(length)), boundary)
    }
}

/// Emits one ghost cell per soft-clipped base, extending outward from the read boundary
/// `boundary` (one cell per reference position, clipped to the tile and so to the contig). The
/// base is the record's `SEQ` base at that read index, exactly as aligned bases read it.
#[allow(clippy::too_many_arguments)]
fn emit_clip_ghosts(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    cigar_index: u32,
    boundary: u64,
    at_end: bool,
    read_start: u32,
    length: u32,
    sequence: Option<SequenceView<'_>>,
    tile: GenomicInterval,
    tile_start: u64,
    hidden_sbx_tail: Option<SbxTailBounds>,
    style: AlignmentStyleToken,
) -> Result<(), SemanticPageError> {
    let Some(SequenceView::Present { bases, .. }) = sequence else {
        return Ok(());
    };
    let (span_start, span_end) = clip_ghost_span(boundary, at_end, length);
    for position in span_start.max(tile.start())..span_end.min(tile.end()) {
        // Index of this base within the clip: a leading clip's base `i` lies `length - i` cells
        // before the boundary, a trailing clip's base `i` lies `i` cells after it. (A leading
        // span clamped at the contig start simply omits the clip's first bases.)
        let offset = if at_end {
            position - boundary
        } else {
            position + u64::from(length) - boundary
        };
        let read_index = u32::try_from(offset)
            .ok()
            .and_then(|offset| read_start.checked_add(offset))
            .ok_or(SemanticPageError::ReadCoordinateOverflow)?;
        let index =
            usize::try_from(read_index).map_err(|_| SemanticPageError::ReadCoordinateOverflow)?;
        if hidden_sbx_tail.is_some_and(|tail| tail.is_tail(index)) {
            continue;
        }
        let base = *bases
            .get(index)
            .ok_or(SemanticPageError::InvalidSequenceGeometry)?;
        output.push(AlignmentSemanticPrimitive {
            start_offset: position - tile_start,
            end_offset: position - tile_start + 1,
            row_offset,
            handle,
            component: AlignmentComponent::ClipGhostBase {
                cigar_index,
                read_index,
                base,
                obscured: false,
            },
            geometry: AlignmentGeometryToken::ClipGhost,
            style,
            draw_order: 0,
        });
    }
    Ok(())
}

/// Marks every ghost cell whose letter must not be drawn: its cell lies inside another read's
/// aligned extent in the same row (that read paints over the cell), or another read's ghost
/// cell shares it (two overlapping letters would be illegible). `extents` holds the
/// tile-clipped `(row offset, start offset, end offset)` of every record of the page; a ghost
/// never lies in its own read's extent, so no record needs excluding.
fn mark_obscured_clip_ghosts(
    primitives: &mut [AlignmentSemanticPrimitive],
    mut extents: Vec<(u32, u64, u64)>,
) {
    extents.sort_unstable();
    let mut covered = Vec::<(u32, u64, u64)>::with_capacity(extents.len());
    for (row, start, end) in extents {
        match covered.last_mut() {
            Some(last) if last.0 == row && start <= last.2 => last.2 = last.2.max(end),
            _ => covered.push((row, start, end)),
        }
    }
    let mut ghosts = primitives
        .iter()
        .enumerate()
        .filter(|(_, primitive)| {
            matches!(
                primitive.component,
                AlignmentComponent::ClipGhostBase { .. }
            )
        })
        .map(|(index, primitive)| (primitive.row_offset, primitive.start_offset, index))
        .collect::<Vec<_>>();
    ghosts.sort_unstable();
    for cell in ghosts.chunk_by(|left, right| left.0 == right.0 && left.1 == right.1) {
        let (row, offset, _) = cell[0];
        let inside_another_read = {
            let next = covered.partition_point(|&(r, start, _)| (r, start) <= (row, offset));
            next > 0 && covered[next - 1].0 == row && offset < covered[next - 1].2
        };
        if cell.len() > 1 || inside_another_read {
            for &(_, _, index) in cell {
                if let AlignmentComponent::ClipGhostBase { obscured, .. } =
                    &mut primitives[index].component
                {
                    *obscured = true;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_bases(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    cigar_index: u32,
    cigar_kind: CigarOpKind,
    reference_start: u64,
    read_start: u32,
    length: u32,
    sequence: Option<SequenceView<'_>>,
    tile: GenomicInterval,
    tile_start: u64,
    reference: Option<&ReferenceSlice>,
    base_display: ReadBaseDisplayMode,
    hidden_sbx_tail: Option<SbxTailBounds>,
    style: AlignmentStyleToken,
    draw_order: &mut u32,
) -> Result<(), SemanticPageError> {
    let Some(sequence) = sequence else {
        return Err(SemanticPageError::SequencePayloadUnavailable);
    };
    let SequenceView::Present { bases, qualities } = sequence else {
        return match sequence {
            SequenceView::Missing => Ok(()),
            SequenceView::NotLoaded => Err(SemanticPageError::SequencePayloadUnavailable),
            SequenceView::Present { .. } => unreachable!(),
        };
    };
    for offset in 0..length {
        let position = reference_start.saturating_add(u64::from(offset));
        if !tile.contains(position) {
            continue;
        }
        let read_index = read_start
            .checked_add(offset)
            .ok_or(SemanticPageError::ReadCoordinateOverflow)?;
        let index =
            usize::try_from(read_index).map_err(|_| SemanticPageError::ReadCoordinateOverflow)?;
        if hidden_sbx_tail.is_some_and(|tail| tail.is_tail(index)) {
            continue;
        }
        let base = *bases
            .get(index)
            .ok_or(SemanticPageError::InvalidSequenceGeometry)?;
        let quality = qualities.and_then(|qualities| qualities.get(index).copied());
        let classification = classify_aligned_base(
            cigar_kind,
            Some(base),
            reference.and_then(|reference| reference.base_at(position)),
        );
        let visible = match base_display {
            ReadBaseDisplayMode::All => true,
            ReadBaseDisplayMode::Hidden => false,
            ReadBaseDisplayMode::Mismatches => !matches!(
                classification,
                AlignedBaseClassification::Match | AlignedBaseClassification::Unknown
            ),
        };
        if !visible {
            continue;
        }
        output.push(AlignmentSemanticPrimitive {
            start_offset: position - tile_start,
            end_offset: position - tile_start + 1,
            row_offset,
            handle,
            component: AlignmentComponent::Base {
                cigar_index,
                read_index,
                base,
                quality,
                classification,
            },
            geometry: AlignmentGeometryToken::Rectangle,
            style,
            draw_order: *draw_order,
        });
        *draw_order = draw_order.saturating_add(1);
    }
    Ok(())
}

fn visible_read_range(
    start: u32,
    length: u32,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Option<(u32, u32)> {
    let end = start.checked_add(length)?;
    let Some(tail) = hidden_sbx_tail else {
        return (start < end).then_some((start, end));
    };
    let retained_start = u32::try_from(tail.retained_start).unwrap_or(u32::MAX);
    let retained_end = u32::try_from(tail.retained_end).unwrap_or(u32::MAX);
    let visible_start = start.max(retained_start);
    let visible_end = end.min(retained_end);
    (visible_start < visible_end).then_some((visible_start, visible_end))
}

fn visible_reference_bounds(
    record: rgv_model::AlignmentRecordView<'_>,
    hidden_sbx_tail: Option<SbxTailBounds>,
) -> Option<(u64, u64)> {
    let Some(tail) = hidden_sbx_tail else {
        return (record.start() < record.end()).then_some((record.start(), record.end()));
    };
    let mut minimum = None::<u64>;
    let mut maximum = None::<u64>;
    for_each_aligned_base(record, |read_index, position, _| {
        if tail.is_tail(read_index) {
            return;
        }
        minimum = Some(minimum.map_or(position, |value| value.min(position)));
        maximum = Some(maximum.map_or(position, |value| value.max(position)));
    });
    minimum.zip(maximum.map(|position| position.saturating_add(1)))
}

fn read_label_for_record(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: rgv_model::AlignmentRecordView<'_>,
    mode: AlignmentReadLabelMode,
) -> Result<Option<(AlignmentLabelKind, String)>, SemanticPageError> {
    Ok(match mode {
        AlignmentReadLabelMode::Hidden => None,
        AlignmentReadLabelMode::ReadName => (!record.read_name().is_empty()).then(|| {
            (
                AlignmentLabelKind::ReadName,
                String::from_utf8_lossy(record.read_name()).into_owned(),
            )
        }),
        AlignmentReadLabelMode::Tag(tag) => selection
            .auxiliary_field(index, tag)?
            .and_then(|field| auxiliary_value_label(&field.value))
            .map(|label| (AlignmentLabelKind::AuxiliaryTag(tag), label)),
    })
}

fn auxiliary_value_label(value: &AuxValue) -> Option<String> {
    Some(match value {
        AuxValue::Character(value) => char::from(*value).to_string(),
        AuxValue::Signed(value) => value.to_string(),
        AuxValue::Unsigned(value) => value.to_string(),
        AuxValue::Float(value) => value.to_string(),
        AuxValue::Double(value) => value.to_string(),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => value.clone(),
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => {
            return None;
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn push_label_span(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    start: u64,
    end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    kind: AlignmentLabelKind,
    text: &str,
    style: AlignmentStyleToken,
    draw_order: u32,
) -> Result<(), SemanticPageError> {
    push_label_with_geometry(
        output,
        labels,
        handle,
        row_offset,
        start,
        end,
        tile,
        tile_start,
        kind,
        text,
        style,
        draw_order,
        AlignmentGeometryToken::Rectangle,
    )
}

/// `geometry` is the label's anchor: `Rectangle` centres the text on its span, while a point
/// marker token centres it on the same base boundary as the event's marker.
#[allow(clippy::too_many_arguments)]
fn push_label_with_geometry(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    start: u64,
    end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    kind: AlignmentLabelKind,
    text: &str,
    style: AlignmentStyleToken,
    draw_order: u32,
    geometry: AlignmentGeometryToken,
) -> Result<(), SemanticPageError> {
    let semantic_start = start;
    let semantic_end = end;
    let start = start.max(tile.start());
    let end = end.min(tile.end());
    if start >= end || text.is_empty() {
        return Ok(());
    }
    let id = label_id(kind, text, semantic_start, semantic_end);
    labels.push(PendingLabel {
        id,
        text: text.to_owned(),
    });
    output.push(AlignmentSemanticPrimitive {
        start_offset: start - tile_start,
        end_offset: end - tile_start,
        row_offset,
        handle,
        component: AlignmentComponent::Label { kind, id },
        geometry,
        style,
        draw_order,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_label_point(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    kind: AlignmentLabelKind,
    text: &str,
    style: AlignmentStyleToken,
    draw_order: u32,
) -> Result<(), SemanticPageError> {
    // An insertion's label is anchored on the boundary its I-beam is drawn on. Other point
    // labels keep their base-cell anchor.
    let geometry = if matches!(
        kind,
        AlignmentLabelKind::InsertionLength | AlignmentLabelKind::ShortInsertion
    ) {
        AlignmentGeometryToken::point_marker_for(position, record_end)
    } else {
        AlignmentGeometryToken::Rectangle
    };
    let position = position.clamp(record_start, record_end.saturating_sub(1));
    push_label_with_geometry(
        output,
        labels,
        handle,
        row_offset,
        position,
        position.saturating_add(1),
        tile,
        tile_start,
        kind,
        text,
        style,
        draw_order,
        geometry,
    )
}

fn label_id(
    kind: AlignmentLabelKind,
    text: &str,
    semantic_start: u64,
    semantic_end: u64,
) -> ContractFingerprint {
    let mut hash = StableHasher::default();
    hash.write(b"alignment-semantic-label-v2\0");
    match kind {
        AlignmentLabelKind::ReadName => hash.write(&[0]),
        AlignmentLabelKind::AuxiliaryTag(tag) => {
            hash.write(&[1]);
            hash.write(&tag);
        }
        AlignmentLabelKind::InsertionLength => hash.write(&[2]),
        AlignmentLabelKind::DeletionLength => hash.write(&[3]),
        AlignmentLabelKind::SkipLength => hash.write(&[4]),
        AlignmentLabelKind::ClipLength => hash.write(&[5]),
        AlignmentLabelKind::ShortInsertion => hash.write(&[6]),
    }
    hash.write(text.as_bytes());
    hash.write(&semantic_start.to_le_bytes());
    hash.write(&semantic_end.to_le_bytes());
    hash.finish()
}

fn finalize_labels(
    mut pending: Vec<PendingLabel>,
) -> Result<Vec<AlignmentSemanticLabel>, SemanticPageError> {
    pending.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.text.cmp(&right.text))
    });
    let mut labels = Vec::<AlignmentSemanticLabel>::with_capacity(pending.len());
    for label in pending {
        if let Some(previous) = labels.last()
            && previous.id == label.id
        {
            if previous.text.as_ref() != label.text {
                return Err(SemanticPageError::LabelFingerprintCollision(label.id));
            }
            continue;
        }
        labels.push(AlignmentSemanticLabel {
            id: label.id,
            text: Arc::from(label.text),
        });
    }
    labels.shrink_to_fit();
    Ok(labels)
}

#[allow(clippy::too_many_arguments)]
fn push_span(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    start: u64,
    end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    component: AlignmentComponent,
    style: AlignmentStyleToken,
    draw_order: u32,
) {
    let start = start.max(tile.start());
    let end = end.min(tile.end());
    if start >= end {
        return;
    }
    output.push(AlignmentSemanticPrimitive {
        start_offset: start - tile_start,
        end_offset: end - tile_start,
        row_offset,
        handle,
        component,
        geometry: geometry_for(component, style),
        style,
        draw_order,
    });
}

#[allow(clippy::too_many_arguments)]
fn push_point(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    component: AlignmentComponent,
    style: AlignmentStyleToken,
    draw_order: u32,
) {
    let mut geometry = geometry_for(component, style);
    if geometry.is_point_marker() {
        geometry = AlignmentGeometryToken::point_marker_for(position, record_end);
    }
    let position = position.clamp(record_start, record_end.saturating_sub(1));
    if !tile.contains(position) {
        return;
    }
    output.push(AlignmentSemanticPrimitive {
        start_offset: position - tile_start,
        end_offset: position - tile_start + 1,
        row_offset,
        handle,
        component,
        geometry,
        style,
        draw_order,
    });
}

/// A clip's cut edge on the one-base span of the aligned base at the clipped boundary: the
/// first aligned base for a leading clip, the last for a trailing one (`edge.at_end`, whose
/// position, one past that base, is clamped onto it).
#[allow(clippy::too_many_arguments)]
fn push_clip_edge(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    tile: GenomicInterval,
    tile_start: u64,
    component: AlignmentComponent,
    edge: ClipEdgeToken,
    style: AlignmentStyleToken,
    draw_order: u32,
) {
    let position = position.min(record_end.saturating_sub(1)).max(record_start);
    if !tile.contains(position) {
        return;
    }
    output.push(AlignmentSemanticPrimitive {
        start_offset: position - tile_start,
        end_offset: position - tile_start + 1,
        row_offset,
        handle,
        component,
        geometry: AlignmentGeometryToken::ClipEdge(edge),
        style,
        draw_order,
    });
}

/// A clip's length, anchored OUTSIDE the read: its span runs from the aligned base at the
/// clipped boundary over the `ghost_cells` ghost bases drawn beyond it, and its
/// [`AlignmentGeometryToken::ClipEdge`] token tells the label projector to place the text
/// before the span's start (leading clip) or after its end (trailing clip). Each page only
/// carries the fragment inside its tile, and only pages holding the record (and so drawing its
/// ghosts) carry one, so the merged span ends exactly where the drawn ghosts do.
#[allow(clippy::too_many_arguments)]
fn push_clip_length_label(
    output: &mut Vec<AlignmentSemanticPrimitive>,
    labels: &mut Vec<PendingLabel>,
    handle: AlignmentHandle,
    row_offset: u32,
    position: u64,
    record_start: u64,
    record_end: u64,
    ghost_cells: u32,
    tile: GenomicInterval,
    tile_start: u64,
    text: &str,
    edge: ClipEdgeToken,
    style: AlignmentStyleToken,
    draw_order: u32,
) -> Result<(), SemanticPageError> {
    let boundary_base = position.min(record_end.saturating_sub(1)).max(record_start);
    let (start, end) = if edge.at_end {
        (
            boundary_base,
            boundary_base
                .saturating_add(1)
                .saturating_add(u64::from(ghost_cells)),
        )
    } else {
        (
            boundary_base.saturating_sub(u64::from(ghost_cells)),
            boundary_base.saturating_add(1),
        )
    };
    push_label_with_geometry(
        output,
        labels,
        handle,
        row_offset,
        start,
        end,
        tile,
        tile_start,
        AlignmentLabelKind::ClipLength,
        text,
        style,
        draw_order,
        AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            behind_arrow: false,
            ..edge
        }),
    )
}

fn geometry_for(
    component: AlignmentComponent,
    style: AlignmentStyleToken,
) -> AlignmentGeometryToken {
    match component {
        AlignmentComponent::Deletion { .. }
        | AlignmentComponent::ReferenceSkip { .. }
        | AlignmentComponent::PairConnector { .. } => AlignmentGeometryToken::EventLine,
        // An Ultima deletion covers the deleted reference bases, so it keeps a span shape; only
        // events between two reference bases are boundary markers.
        AlignmentComponent::UltimaIndel {
            kind: UltimaIndelToken::Deletion,
            ..
        } => AlignmentGeometryToken::Rectangle,
        // Clip edges are emitted by `push_clip_edge`, which also decides the boundary and the
        // arrow composition; this is only the token's default form.
        AlignmentComponent::SoftClip { .. } => AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: false,
        }),
        AlignmentComponent::HardClip { .. } => AlignmentGeometryToken::ClipEdge(ClipEdgeToken {
            at_end: false,
            hard: true,
            behind_arrow: false,
        }),
        AlignmentComponent::ClipGhostBase { .. } => AlignmentGeometryToken::ClipGhost,
        AlignmentComponent::Insertion { .. }
        | AlignmentComponent::Padding { .. }
        | AlignmentComponent::UltimaIndel { .. }
        | AlignmentComponent::SbxInsertion { .. } => AlignmentGeometryToken::PointMarker,
        AlignmentComponent::DirectionTerminal => {
            if rgv_model::AlignmentFlags::from_bits(style.flags).is_reverse() {
                AlignmentGeometryToken::DirectionalTerminalLeft
            } else {
                AlignmentGeometryToken::DirectionalTerminalRight
            }
        }
        AlignmentComponent::Label { .. }
        | AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::ModifiedBase { .. }
        | AlignmentComponent::BisulfiteBase { .. }
        | AlignmentComponent::SmrtKinetics { .. }
        | AlignmentComponent::SbxTailBase { .. }
        | AlignmentComponent::SpecialistUnavailable { .. }
        | AlignmentComponent::Base { .. } => AlignmentGeometryToken::Rectangle,
    }
}

fn validate_dependencies(
    key: AlignmentPageKey,
    evidence: &EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    presentation: PresentationContract,
    reference: Option<&ReferenceSlice>,
) -> Result<(), SemanticPageError> {
    validate_root_dependencies(key, evidence, atlas, presentation)?;
    let page_uses_reference =
        crate::page_identity::page_reads_reference(key.family, key.lod, presentation.style);
    if page_uses_reference && presentation.reference.is_some() && reference.is_none() {
        return Err(SemanticPageError::ReferenceUnavailable);
    }
    Ok(())
}

fn validate_root_dependencies(
    key: AlignmentPageKey,
    evidence: &EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    presentation: PresentationContract,
) -> Result<(), SemanticPageError> {
    if key.source_revision != evidence.source_revision()
        || key.source_revision != atlas.source_revision()
    {
        return Err(SemanticPageError::SourceRevisionMismatch);
    }
    if key.evidence_revision != evidence.revision() {
        return Err(SemanticPageError::EvidenceRevisionMismatch);
    }
    if atlas.record_contract() != evidence.record_contract() {
        return Err(SemanticPageError::RecordContractMismatch);
    }
    if key.row_atlas_revision != atlas.revision() {
        return Err(SemanticPageError::RowAtlasRevisionMismatch);
    }
    if key.layout != atlas.layout_contract().fingerprint() {
        return Err(SemanticPageError::LayoutContractMismatch);
    }
    if key.presentation != presentation.page_fingerprint(key.family)
        || key.lod != presentation.geometry_lod
    {
        return Err(SemanticPageError::PresentationContractMismatch);
    }
    validate_style_capabilities(evidence, presentation, key.family)?;
    Ok(())
}

fn validate_style_capabilities(
    evidence: &EvidenceRoot,
    presentation: PresentationContract,
    family: AlignmentPageFamily,
) -> Result<(), SemanticPageError> {
    let style = presentation.style;
    let hides_sbx_tail = matches!(
        style.color_mode,
        AlignmentColorMode::Sbx(options)
            if options.simplex_tail_mode == SbxSimplexTailMode::Hide
    );
    let sequence_required = (family == AlignmentPageFamily::ReadGeometry
        && (presentation.geometry_lod == GeometryLod::Base || hides_sbx_tail))
        || (family == AlignmentPageFamily::SpecialistOverlay
            && (style.show_modified_bases
                || matches!(
                    style.color_mode,
                    AlignmentColorMode::Bisulfite(_)
                        | AlignmentColorMode::SmrtKinetics(_)
                        | AlignmentColorMode::Ultima(_)
                        | AlignmentColorMode::Sbx(_)
                )));
    if sequence_required && evidence.payload().sequence() != CapabilityRequirement::Required {
        return Err(SemanticPageError::StyleSequencePayloadRequired);
    }
    let quality_required = (family == AlignmentPageFamily::ReadGeometry
        && ((presentation.geometry_lod == GeometryLod::Base && style.shade_bases_by_quality)
            || hides_sbx_tail))
        || (family == AlignmentPageFamily::SpecialistOverlay
            && matches!(
                style.color_mode,
                AlignmentColorMode::Ultima(_) | AlignmentColorMode::Sbx(_)
            ));
    if quality_required && evidence.payload().qualities() != CapabilityRequirement::Required {
        return Err(SemanticPageError::StyleQualityPayloadRequired);
    }
    // Reference-backed specialist modes deliberately remain buildable without a FASTA slice.
    // Their page emits `SpecialistUnavailable::MissingReference` while independent modified-base
    // overlays and the core read geometry remain truthful. A later immutable scene upgrades the
    // same family when the requested reference revision becomes available.

    let mut tags = Vec::<[u8; 2]>::new();
    if family == AlignmentPageFamily::ReadGeometry {
        match style.color_mode {
            AlignmentColorMode::ReadGroup => tags.push(*b"RG"),
            AlignmentColorMode::Haplotype => tags.push(*b"HP"),
            AlignmentColorMode::Tag(tag) => tags.push(tag),
            _ => {}
        }
    }
    if family == AlignmentPageFamily::SpecialistOverlay {
        match style.color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => {
                tags.push(smrt_kinetics_tag(mode, false));
                tags.push(smrt_kinetics_tag(mode, true));
            }
            AlignmentColorMode::Ultima(_) => tags.extend([*b"tp", *b"t0"]),
            _ => {}
        }
    }
    if family == AlignmentPageFamily::Labels
        && let AlignmentReadLabelMode::Tag(tag) = style.read_labels
    {
        tags.push(tag);
    }
    if family == AlignmentPageFamily::SpecialistOverlay && style.show_modified_bases {
        // `MN` decides whether `MM` still describes the stored sequence; without it retained,
        // a hard-clipped read carrying a valid `MN` would be reported unavailable.
        tags.extend([*b"MM", *b"Mm", *b"ML", *b"Ml", *b"MN"]);
    }
    tags.sort_unstable();
    tags.dedup();
    if !evidence.payload().all_auxiliary_tags() {
        for tag in tags {
            if evidence
                .payload()
                .auxiliary_tags()
                .binary_search(&tag)
                .is_err()
            {
                return Err(SemanticPageError::StyleAuxiliaryPayloadRequired(tag));
            }
        }
    }
    Ok(())
}

fn semantic_page_base_scratch_bytes(
    selected_records: usize,
    atlas_records: usize,
    connectors: usize,
) -> usize {
    hash_table_scratch_bytes(selected_records, size_of::<(AlignmentHandle, usize)>())
        .saturating_add(atlas_records.saturating_mul(size_of::<RowAtlasRecordHit>()))
        .saturating_add(connectors.saturating_mul(size_of::<AlignmentRowConnector>()))
}

fn semantic_page_dynamic_scratch_bytes(
    selection: &crate::EvidenceSelection,
    atlas_records: &[RowAtlasRecordHit],
    by_handle: &HashMap<AlignmentHandle, usize>,
    key: AlignmentPageKey,
    presentation: PresentationContract,
    tile: GenomicInterval,
) -> Result<usize, SemanticPageError> {
    let mut bytes = hash_table_scratch_bytes(
        atlas_records.len(),
        size_of::<(AlignmentHandle, PreparedRecordSemantics)>(),
    );
    let mut overlay_slots = 0_usize;
    let mut modification_call_slots = 0_usize;
    let mut pending_label_slots = 0_usize;
    let mut pending_label_bytes = 0_usize;
    let detailed = matches!(key.lod, GeometryLod::Read | GeometryLod::Base);
    let clip_ghost_page =
        key.family == AlignmentPageFamily::ReadGeometry && key.lod == GeometryLod::Base;
    let mut clip_ghost_slots = 0_usize;

    for hit in atlas_records {
        let index = *by_handle.get(&hit.record.handle()).ok_or(
            SemanticPageError::MissingEvidenceHandle(hit.record.handle()),
        )?;
        let record = selection
            .record_view(index)
            .ok_or(SemanticPageError::InvalidEvidenceRecord)?;
        let sequence_len = match selection.sequence(index) {
            Some(SequenceView::Present { bases, .. }) => bases.len(),
            Some(SequenceView::Missing | SequenceView::NotLoaded) | None => 0,
        };
        let (aligned_bases, tile_aligned_bases) = aligned_base_counts(record, tile);
        let auxiliary_bytes = selection.auxiliary_encoded_len(index).unwrap_or_default();
        // Decoded tag values, kinetics arrays, and modification calls are temporary owned values.
        bytes = bytes.saturating_add(auxiliary_bytes.saturating_mul(8));
        if clip_ghost_page {
            clip_ghost_slots = clip_ghost_slots.saturating_add(clip_ghost_bound(record, tile));
        }

        if key.family == AlignmentPageFamily::Labels && detailed {
            let read_label_bytes = match presentation.style.read_labels {
                AlignmentReadLabelMode::Hidden => 0,
                // Invalid UTF-8 replacement characters may expand one input byte to three.
                AlignmentReadLabelMode::ReadName => record.read_name().len().saturating_mul(3),
                AlignmentReadLabelMode::Tag(tag) => selection
                    .auxiliary_value_payload_len(index, tag)?
                    .unwrap_or_default()
                    .saturating_mul(3)
                    .saturating_add(32),
            };
            if read_label_bytes > 0 {
                pending_label_slots = pending_label_slots.saturating_add(1);
                pending_label_bytes = pending_label_bytes.saturating_add(read_label_bytes);
            }
            let events = record
                .cigar()
                .iter()
                .filter(|operation| {
                    // Every insertion is labelled (see `emit_record_primitives`).
                    (operation.len >= presentation.style.minimum_event_label_size
                        || operation.kind == CigarOpKind::Insertion)
                        && matches!(
                            operation.kind,
                            CigarOpKind::Insertion
                                | CigarOpKind::Deletion
                                | CigarOpKind::ReferenceSkip
                                | CigarOpKind::SoftClip
                                | CigarOpKind::HardClip
                        )
                })
                .count();
            pending_label_slots = pending_label_slots.saturating_add(events);
            pending_label_bytes = pending_label_bytes.saturating_add(events.saturating_mul(11));
        }

        if key.family == AlignmentPageFamily::SpecialistOverlay && detailed {
            if presentation.style.show_modified_bases && sequence_len > 0 {
                let mm_upper = selection.auxiliary_field(index, *b"MM")?;
                let mm_lower = selection.auxiliary_field(index, *b"Mm")?;
                if let Some(mm) = mm_upper.as_ref().or(mm_lower.as_ref()) {
                    // The MM text states how many calls it makes, so count them rather than
                    // assuming every tag byte could call every sequence position: that product
                    // reserved gigabytes for one long read carrying a dense MM tag.
                    let bound = modified_base_call_bound(&mm.value);
                    modification_call_slots = modification_call_slots.saturating_add(bound.calls);
                    // An overlay needs an aligned base inside this tile, and each modification
                    // series marks one read position at most once.
                    let tile_series_slots = bound.series.saturating_mul(tile_aligned_bases);
                    let explicit = bound.calls.min(tile_series_slots);
                    let canonical = if presentation.style.modified_base_color_mode
                        == ModifiedBaseColorMode::ModifiedVsUnmodified
                    {
                        tile_series_slots
                    } else {
                        0
                    };
                    // One further slot covers the "unavailable" marker of a malformed or
                    // unresolvable tag.
                    overlay_slots = overlay_slots
                        .saturating_add(explicit)
                        .saturating_add(canonical)
                        .saturating_add(1);
                }
            }
            let specialist_slots = match presentation.style.color_mode {
                // These overlays are emitted per aligned base inside the tile only.
                AlignmentColorMode::Bisulfite(_)
                | AlignmentColorMode::SmrtKinetics(_)
                | AlignmentColorMode::Sbx(_) => tile_aligned_bases
                    .saturating_add(record.cigar().len())
                    .saturating_add(1),
                AlignmentColorMode::Ultima(_) => aligned_bases
                    .saturating_add(record.cigar().len().saturating_mul(2))
                    .saturating_add(1),
                _ => 0,
            };
            overlay_slots = overlay_slots.saturating_add(specialist_slots);
        }
    }

    bytes = bytes
        .saturating_add(
            overlay_slots
                .saturating_mul(2)
                .saturating_mul(size_of::<PreparedOverlay>()),
        )
        .saturating_add(overlay_slots.saturating_mul(64))
        // Modified-base decoding temporarily retains every decoded call of the read (a growing
        // vector, hence twice) and at most one canonical-position set entry per call.
        .saturating_add(
            modification_call_slots.saturating_mul(
                size_of::<rgv_model::ModifiedBaseCall>()
                    .saturating_mul(2)
                    .saturating_add(64),
            ),
        )
        .saturating_add(
            pending_label_slots
                .saturating_mul(2)
                .saturating_mul(size_of::<PendingLabel>()),
        )
        .saturating_add(pending_label_bytes);
    if clip_ghost_page {
        // `mark_obscured_clip_ghosts`: every record's extent and their per-row union, and one
        // sort entry per ghost cell.
        bytes = bytes
            .saturating_add(
                atlas_records
                    .len()
                    .saturating_mul(2)
                    .saturating_mul(size_of::<(u32, u64, u64)>()),
            )
            .saturating_add(clip_ghost_slots.saturating_mul(size_of::<(u32, u64, usize)>()));
    }
    Ok(bytes)
}

/// Ghost cells the record's soft clips can place inside `tile` (see `emit_clip_ghosts`).
fn clip_ghost_bound(record: rgv_model::AlignmentRecordView<'_>, tile: GenomicInterval) -> usize {
    let mut cells = 0_usize;
    let mut reference_position = record.start();
    for operation in record.cigar() {
        match operation.kind {
            CigarOpKind::Match
            | CigarOpKind::SequenceMatch
            | CigarOpKind::SequenceMismatch
            | CigarOpKind::Deletion
            | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::SoftClip => {
                let (start, end) = clip_ghost_span(
                    reference_position,
                    reference_position >= record.end(),
                    operation.len,
                );
                cells = cells.saturating_add(clipped_len(start, end, tile));
            }
            CigarOpKind::Insertion | CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    cells
}

/// Upper bounds stated by one `MM` value: decoded calls and distinct modification series.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ModifiedBaseCallBound {
    calls: usize,
    series: usize,
}

/// Counts what `decode_modified_bases_parts` can yield without locating any call. Each
/// `;`-separated group yields one call per listed delta and modification code; a group the
/// decoder would reject only makes this bound more generous.
fn modified_base_call_bound(mm: &AuxValue) -> ModifiedBaseCallBound {
    let AuxValue::String(text) = mm else {
        return ModifiedBaseCallBound::default();
    };
    let mut bound = ModifiedBaseCallBound::default();
    for group in text.split(';').filter(|group| !group.is_empty()) {
        let Some((header, deltas)) = group.split_once(',') else {
            continue;
        };
        let codes = header.as_bytes().get(2..).map_or(1, |codes| {
            let codes = match codes.last() {
                Some(b'?' | b'.') => &codes[..codes.len() - 1],
                _ => codes,
            };
            if codes.iter().all(u8::is_ascii_digit) {
                // A numeric ChEBI identifier is one code however many digits it has.
                1
            } else {
                codes.len().max(1)
            }
        });
        let positions = deltas
            .bytes()
            .filter(|byte| *byte == b',')
            .count()
            .saturating_add(1);
        bound.calls = bound.calls.saturating_add(codes.saturating_mul(positions));
        bound.series = bound.series.saturating_add(codes);
    }
    bound
}

/// Aligned (`M`/`=`/`X`) bases of the whole record and of its part inside `tile`.
fn aligned_base_counts(
    record: rgv_model::AlignmentRecordView<'_>,
    tile: GenomicInterval,
) -> (usize, usize) {
    let mut total = 0_usize;
    let mut inside = 0_usize;
    let mut reference_position = record.start();
    for operation in record.cigar() {
        let end = reference_position.saturating_add(u64::from(operation.len));
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                total = total.saturating_add(operation.len as usize);
                inside = inside.saturating_add(clipped_len(reference_position, end, tile));
                reference_position = end;
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => reference_position = end,
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    (total, inside)
}

/// Length of `[start, end)` inside `tile`.
fn clipped_len(start: u64, end: u64, tile: GenomicInterval) -> usize {
    usize::try_from(end.min(tile.end()).saturating_sub(start.max(tile.start())))
        .unwrap_or(usize::MAX)
}

/// Upper bound on what [`emit_record_primitives`] appends for one record on one page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RecordPrimitiveBound {
    /// Read-geometry primitives at the requested level of detail.
    geometry: usize,
    /// CIGAR event labels at or above the minimum labelled size.
    event_labels: usize,
}

/// One CIGAR walk which counts only the operations able to intersect `tile`, mirroring the
/// clipping `push_span`, `push_point` and `emit_bases` apply. Display filters (minimum indel
/// size, hidden tails, base display mode) can only emit fewer primitives than counted here.
fn record_primitive_bound(
    record: rgv_model::AlignmentRecordView<'_>,
    tile: GenomicInterval,
    lod: GeometryLod,
    minimum_event_label_size: u32,
) -> RecordPrimitiveBound {
    if lod == GeometryLod::Overview {
        return RecordPrimitiveBound {
            geometry: 1,
            event_labels: 0,
        };
    }
    let record_start = record.start();
    let last_position = record.end().saturating_sub(1).max(record_start);
    let point_visible =
        |position: u64| tile.contains(position.max(record_start).min(last_position));
    let cigar = record.cigar();
    // The direction terminal, plus the whole-read span of a record without a CIGAR.
    let mut bound = RecordPrimitiveBound {
        geometry: 1 + usize::from(cigar.is_empty()),
        event_labels: 0,
    };
    let mut reference_position = record_start;
    for operation in cigar {
        let end = reference_position.saturating_add(u64::from(operation.len));
        // Every insertion is labelled (see `emit_record_primitives`).
        let labelled = usize::from(
            operation.len >= minimum_event_label_size || operation.kind == CigarOpKind::Insertion,
        );
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let inside = clipped_len(reference_position, end, tile);
                if inside > 0 {
                    bound.geometry = bound.geometry.saturating_add(1);
                    if lod == GeometryLod::Base {
                        bound.geometry = bound.geometry.saturating_add(inside);
                    }
                }
                reference_position = end;
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                if clipped_len(reference_position, end, tile) > 0 {
                    bound.geometry = bound.geometry.saturating_add(1);
                    bound.event_labels = bound.event_labels.saturating_add(labelled);
                }
                reference_position = end;
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip | CigarOpKind::HardClip => {
                if point_visible(reference_position) {
                    bound.geometry = bound.geometry.saturating_add(1);
                    bound.event_labels = bound.event_labels.saturating_add(labelled);
                }
                if lod == GeometryLod::Base && operation.kind == CigarOpKind::SoftClip {
                    // Ghost cells extend outward from the boundary, past the read's own extent,
                    // so they are counted wherever they meet the tile.
                    let (start, end) = clip_ghost_span(
                        reference_position,
                        reference_position >= record.end(),
                        operation.len,
                    );
                    bound.geometry = bound.geometry.saturating_add(clipped_len(start, end, tile));
                }
            }
            CigarOpKind::Padding => {
                if point_visible(reference_position) {
                    bound.geometry = bound.geometry.saturating_add(1);
                }
            }
        }
    }
    bound
}

fn hash_table_scratch_bytes(entries: usize, entry_bytes: usize) -> usize {
    if entries == 0 {
        return 0;
    }
    entries
        .saturating_mul(2)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .saturating_mul(entry_bytes.saturating_add(1))
}

const fn semantic_page_bytes(
    primitives: usize,
    row_offsets: usize,
    label_primitive_indices: usize,
    labels: usize,
    label_bytes: usize,
) -> usize {
    size_of::<AlignmentSemanticPage>()
        .saturating_add(primitives.saturating_mul(size_of::<AlignmentSemanticPrimitive>()))
        .saturating_add(row_offsets.saturating_mul(size_of::<u32>()))
        .saturating_add(label_primitive_indices.saturating_mul(size_of::<u32>()))
        .saturating_add(labels.saturating_mul(size_of::<AlignmentSemanticLabel>()))
        .saturating_add(label_bytes)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SemanticPageError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    EvidenceSelection(#[from] EvidenceSelectionError),
    #[error("semantic page source revision does not match evidence and rows")]
    SourceRevisionMismatch,
    #[error("semantic page evidence revision is stale or incompatible")]
    EvidenceRevisionMismatch,
    #[error("semantic page row atlas uses a different physical-record contract")]
    RecordContractMismatch,
    #[error("semantic page row-atlas revision is stale or incompatible")]
    RowAtlasRevisionMismatch,
    #[error("semantic page layout contract does not match its row atlas")]
    LayoutContractMismatch,
    #[error("semantic page presentation contract does not match its key")]
    PresentationContractMismatch,
    #[error("semantic page tile {0:?} is not completely certified")]
    UncertifiedTile(GenomicInterval),
    #[error("semantic page row coordinate overflowed")]
    RowCoordinateOverflow,
    #[error("row block beginning at {start} is outside an atlas with {total} rows")]
    RowBlockOutsideAtlas { start: u32, total: u32 },
    #[error("row atlas referenced handle {0:?} absent from tile evidence")]
    MissingEvidenceHandle(AlignmentHandle),
    #[error("semantic page evidence contains an invalid record reference")]
    InvalidEvidenceRecord,
    #[error("semantic page primitive count cannot be represented")]
    PrimitiveCountOverflow,
    #[error("CIGAR operation index cannot be represented")]
    CigarIndexOverflow,
    #[error("read coordinate cannot be represented")]
    ReadCoordinateOverflow,
    #[error("base-detail geometry requires sequence payload")]
    SequencePayloadUnavailable,
    #[error("sequence payload does not satisfy CIGAR geometry")]
    InvalidSequenceGeometry,
    #[error("the presentation contract requires a reference slice")]
    ReferenceUnavailable,
    #[error("alignment style requires retained sequence payload")]
    StyleSequencePayloadRequired,
    #[error("alignment style requires retained base-quality payload")]
    StyleQualityPayloadRequired,
    #[error("alignment style requires a loaded interpretation reference")]
    StyleReferenceRequired,
    #[error("alignment style requires retained auxiliary tag {0:?}")]
    StyleAuxiliaryPayloadRequired([u8; 2]),
    #[error("two distinct semantic labels produced fingerprint {0:?}")]
    LabelFingerprintCollision(ContractFingerprint),
    #[error("semantic-page preparation for handle {0:?} was lost")]
    MissingPreparedSemantics(AlignmentHandle),
    #[error(transparent)]
    PackedRecord(#[from] rgv_model::PackedRecordError),
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, num::NonZeroU64, sync::Arc};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, BisulfiteContext, ContigId,
        ReadBaseDisplayMode, ReadStackingMode, SbxDisplayOptions, SmrtKineticsMode,
        UltimaDisplayOptions,
    };
    use rgv_model::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, SequenceData, TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AllocationClass, CapabilityRequirement, CompleteCoverage, EvidenceAcquisition,
        EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance, PayloadContract,
        PayloadSidecar, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(17);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn layout_contract() -> crate::LayoutContract {
        crate::LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        }
    }

    fn record(
        name: &[u8],
        start: u64,
        end: u64,
        cigar: &[(CigarOpKind, u32)],
        sequence: &[u8],
    ) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: cigar
                .iter()
                .map(|(kind, len)| CigarOp {
                    kind: *kind,
                    len: *len,
                })
                .collect(),
            sequence: if sequence.is_empty() {
                SequenceData::Missing
            } else {
                let mut qualities = vec![35; sequence.len()];
                if name == b"events" {
                    qualities[..3].fill(5);
                    *qualities.last_mut().unwrap() = 5;
                }
                SequenceData::Present {
                    bases: sequence.to_vec(),
                    qualities: Some(qualities),
                }
            },
            auxiliary_fields: (name == b"events")
                .then(|| {
                    vec![
                        AuxField {
                            tag: *b"RG",
                            value: AuxValue::String("group-a".to_owned()),
                        },
                        AuxField {
                            tag: *b"HP",
                            value: AuxValue::Unsigned(1),
                        },
                        AuxField {
                            tag: *b"ZZ",
                            value: AuxValue::String("label-value".to_owned()),
                        },
                        AuxField {
                            tag: *b"MM",
                            value: AuxValue::String("A+m,2;".to_owned()),
                        },
                        AuxField {
                            tag: *b"ML",
                            value: AuxValue::UnsignedArray(vec![220]),
                        },
                        // This fixture read is hard-clipped; `MN` states that `MM` was written
                        // for the stored 11-base sequence, which the decoder now requires.
                        AuxField {
                            tag: *b"MN",
                            value: AuxValue::Unsigned(11),
                        },
                        AuxField {
                            tag: *b"ip",
                            value: AuxValue::UnsignedArray((1..=11).collect()),
                        },
                        AuxField {
                            tag: *b"tp",
                            value: AuxValue::SignedArray(vec![10; 11]),
                        },
                        AuxField {
                            tag: *b"t0",
                            value: AuxValue::String("IIIIIIIIIII".to_owned()),
                        },
                    ]
                })
                .unwrap_or_default(),
        }
    }

    fn evidence_and_atlas(
        ledger: &AllocationLedger,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let records = vec![
            record(
                b"events",
                20,
                30,
                &[
                    (CigarOpKind::SoftClip, 2),
                    (CigarOpKind::Match, 5),
                    (CigarOpKind::Insertion, 2),
                    (CigarOpKind::Deletion, 3),
                    (CigarOpKind::SequenceMismatch, 2),
                    (CigarOpKind::Padding, 1),
                    (CigarOpKind::HardClip, 1),
                ],
                b"AAACCCCCCCC",
            ),
            record(
                b"spliced",
                90,
                210,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::ReferenceSkip, 100),
                    (CigarOpKind::Match, 10),
                ],
                b"AAAAAAAAAAAAAAAAAAAA",
            ),
            record(b"cigarless", 250, 260, &[], b""),
        ];
        evidence_and_atlas_for_records(ledger, &records, layout_contract())
    }

    fn evidence_and_atlas_for_records(
        ledger: &AllocationLedger,
        records: &[AlignmentRecord],
        layout: crate::LayoutContract,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let mut decoded = AlignmentBatch::default();
        for (ordinal, record) in records.iter().enumerate() {
            decoded
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (7 << 16) | u64::try_from(ordinal + 1).unwrap(),
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
        let core = crate::CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let mut reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let payload_allocation = reservation.register(payload_bytes, 1).unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let mut reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core_allocation = reservation.register(footprint.core_bytes, 1).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage_interval = GenomicInterval::new(ContigId::new(0), 0, 300).unwrap();
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            coverage_interval,
            FilteredRecordProvenance {
                records_seen: records.len() as u64,
                eligible_records: records.len() as u64,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(5, 6)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(2).unwrap();
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(7, 8)),
            &evidence,
            coverage_interval,
            layout,
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        (evidence, atlas)
    }

    fn paired_evidence_and_atlas(
        ledger: &AllocationLedger,
    ) -> (Arc<EvidenceRoot>, Arc<AlignmentRowAtlas>) {
        let aligned = [(CigarOpKind::Match, 10)];
        let mut first = record(b"pair", 50, 60, &aligned, b"AAAAAAAAAA");
        first.flags = AlignmentFlags::from_bits(0x63);
        first.mate_contig = Some(ContigId::new(0));
        first.mate_start = Some(150);
        first.template_length = 110;
        let mut last = record(b"pair", 150, 160, &aligned, b"CCCCCCCCCC");
        last.flags = AlignmentFlags::from_bits(0x93);
        last.mate_contig = Some(ContigId::new(0));
        last.mate_start = Some(50);
        last.template_length = -110;
        let primary = record(b"split", 20, 30, &aligned, b"GGGGGGGGGG");
        let mut supplementary = record(b"split", 110, 120, &aligned, b"TTTTTTTTTT");
        supplementary.flags = AlignmentFlags::from_bits(0x800);
        let mut secondary = record(b"split", 180, 190, &aligned, b"ACGTACGTAC");
        secondary.flags = AlignmentFlags::from_bits(0x100);
        let layout = crate::LayoutContract {
            view_as_pairs: true,
            ..layout_contract()
        };
        evidence_and_atlas_for_records(
            ledger,
            &[first, last, primary, supplementary, secondary],
            layout,
        )
    }

    fn page(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        tile_coordinate: u64,
        lod: GeometryLod,
        reference: Option<&ReferenceSlice>,
    ) -> Arc<AlignmentSemanticPage> {
        page_with_style(
            ledger,
            evidence,
            atlas,
            tile_coordinate,
            lod,
            reference,
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::ReadGeometry,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn page_with_style(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        tile_coordinate: u64,
        lod: GeometryLod,
        reference: Option<&ReferenceSlice>,
        style: crate::AlignmentStyleContract,
        family: AlignmentPageFamily,
    ) -> Arc<AlignmentSemanticPage> {
        let presentation = PresentationContract {
            geometry_lod: lod,
            style,
            reference: reference
                .map(|_| crate::ReferenceRevision::new(RevisionFingerprint::new(10, 11))),
            annotation: None,
        };
        let key = AlignmentPageKey::new(
            evidence.source_revision(),
            evidence.revision(),
            atlas.revision(),
            atlas.layout_contract().fingerprint(),
            presentation.page_fingerprint(family),
            CanonicalGenomicTile::new(
                ContigId::new(0),
                tile_coordinate,
                NonZeroU64::new(100).unwrap(),
            ),
            CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap()),
            family,
            lod,
        );
        AlignmentSemanticPage::build(
            key,
            evidence,
            atlas,
            presentation,
            300,
            reference,
            ledger,
            ReservationKind::Normal,
            4 + tile_coordinate,
        )
        .unwrap()
    }

    fn pages_for_presentation(
        ledger: &AllocationLedger,
        evidence: &Arc<EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
    ) -> Vec<Arc<AlignmentSemanticPage>> {
        (0..3)
            .flat_map(|tile| {
                presentation
                    .required_page_families()
                    .into_iter()
                    .map(move |family| {
                        page_with_style(
                            ledger,
                            evidence,
                            atlas,
                            tile,
                            presentation.geometry_lod,
                            None,
                            presentation.style,
                            family,
                        )
                    })
            })
            .collect()
    }

    #[test]
    fn specialist_modes_and_read_labels_are_scene_native() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let mut reference_bases = vec![b'A'; 300];
        // The first aligned C in `events` is at reference position 21. Make it a CG site.
        reference_bases[21] = b'C';
        reference_bases[22] = b'G';
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: reference_bases,
        };

        let default_page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::SpecialistOverlay,
        );
        assert!(default_page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            AlignmentComponent::ModifiedBase {
                probability: Some(220),
                state: ModificationState::Modified,
                ..
            }
        )));

        let styles = [
            (
                AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
                SpecialistMode::Bisulfite,
            ),
            (
                AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
                SpecialistMode::SmrtKinetics,
            ),
            (
                AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
                SpecialistMode::Ultima,
            ),
            (
                AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
                SpecialistMode::Sbx,
            ),
        ];
        for (color_mode, expected_mode) in styles {
            let style = crate::AlignmentStyleContract {
                color_mode,
                ..crate::AlignmentStyleContract::default()
            };
            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Base,
                Some(&reference),
                style,
                AlignmentPageFamily::SpecialistOverlay,
            );
            assert!(page.primitives().iter().any(|primitive| {
                match (expected_mode, primitive.component) {
                    (SpecialistMode::Bisulfite, AlignmentComponent::BisulfiteBase { .. })
                    | (SpecialistMode::SmrtKinetics, AlignmentComponent::SmrtKinetics { .. })
                    | (SpecialistMode::Ultima, AlignmentComponent::UltimaIndel { .. })
                    | (SpecialistMode::Sbx, AlignmentComponent::SbxInsertion { .. }) => true,
                    (_, AlignmentComponent::SpecialistUnavailable { mode, .. }) => {
                        mode == expected_mode
                    }
                    _ => false,
                }
            }));
        }

        let style = crate::AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::ReadName,
            ..crate::AlignmentStyleContract::default()
        };
        let page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Read,
            None,
            style,
            AlignmentPageFamily::Labels,
        );
        assert!(page.labels().iter().any(|label| label.text() == "events"));
        assert!(page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            AlignmentComponent::Label {
                kind: AlignmentLabelKind::ReadName,
                ..
            }
        )));

        let hidden_tail_style = crate::AlignmentStyleContract {
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: SbxSimplexTailMode::Hide,
                ..SbxDisplayOptions::default()
            }),
            ..crate::AlignmentStyleContract::default()
        };
        let hidden_tail = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
            hidden_tail_style,
            AlignmentPageFamily::ReadGeometry,
        );
        assert!(hidden_tail.primitives_at(0, 20).is_empty());
        assert!(
            hidden_tail
                .primitives_at(0, 21)
                .iter()
                .any(|primitive| matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                ))
        );
        assert!(
            hidden_tail
                .primitives_at(0, 29)
                .iter()
                .all(|primitive| !matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                        | AlignmentComponent::Base { .. }
                        | AlignmentComponent::DirectionTerminal
                ))
        );
    }

    #[test]
    fn long_and_spliced_geometry_splits_at_tiles_without_identity_or_length_loss() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let pages = (0..3)
            .map(|tile| page(&ledger, &evidence, &atlas, tile, GeometryLod::Read, None))
            .collect::<Vec<_>>();
        let aligned_bases = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                )
            })
            .map(|primitive| primitive.end_offset - primitive.start_offset)
            .sum::<u64>();
        let skipped_bases = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::ReferenceSkip { .. }
                )
            })
            .map(|primitive| primitive.end_offset - primitive.start_offset)
            .sum::<u64>();
        let spliced_handles = pages
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::ReferenceSkip { .. }
                )
            })
            .map(|primitive| primitive.handle)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(aligned_bases, 27);
        assert_eq!(skipped_bases, 100);
        assert_eq!(spliced_handles.len(), 1);
        assert!(matches!(
            pages[1].pick(0, 150).unwrap().component,
            AlignmentComponent::ReferenceSkip { .. }
        ));
    }

    #[test]
    fn every_read_geometry_and_event_label_kind_is_scene_native() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let geometry = (0..3)
            .map(|tile| {
                page(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Base,
                    Some(&reference),
                )
            })
            .collect::<Vec<_>>();
        let components = geometry
            .iter()
            .flat_map(|page| {
                page.primitives()
                    .iter()
                    .map(|primitive| primitive.component)
            })
            .collect::<Vec<_>>();
        for present in [
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::AlignedSegment { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Base { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Insertion { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Deletion { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::ReferenceSkip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::SoftClip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::HardClip { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::Padding { .. })),
            components
                .iter()
                .any(|component| matches!(component, AlignmentComponent::DirectionTerminal)),
        ] {
            assert!(present);
        }

        let label_style = crate::AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::Hidden,
            minimum_event_label_size: 1,
            ..crate::AlignmentStyleContract::default()
        };
        let labels = (0..3)
            .flat_map(|tile| {
                page_with_style(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Read,
                    None,
                    label_style,
                    AlignmentPageFamily::Labels,
                )
                .primitives()
                .iter()
                .filter_map(|primitive| match primitive.component {
                    AlignmentComponent::Label { kind, .. } => Some(kind),
                    _ => None,
                })
                .collect::<Vec<_>>()
            })
            .collect::<std::collections::HashSet<_>>();
        for kind in [
            AlignmentLabelKind::InsertionLength,
            AlignmentLabelKind::DeletionLength,
            AlignmentLabelKind::SkipLength,
            AlignmentLabelKind::ClipLength,
        ] {
            assert!(labels.contains(&kind), "missing semantic label {kind:?}");
        }
    }

    #[test]
    fn span_lod_preserves_cigar_landmarks_and_both_read_directions() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let cigar = [
            (CigarOpKind::Match, 5),
            (CigarOpKind::Insertion, 2),
            (CigarOpKind::Match, 5),
        ];
        let forward = record(b"forward", 20, 30, &cigar, b"AAAAAAAAAAAA");
        let mut reverse = record(b"reverse", 40, 50, &cigar, b"CCCCCCCCCCCC");
        reverse.flags = AlignmentFlags::from_bits(0x10);
        let (evidence, atlas) =
            evidence_and_atlas_for_records(&ledger, &[forward, reverse], layout_contract());
        let page = page(&ledger, &evidence, &atlas, 0, GeometryLod::Span, None);

        assert!(
            page.primitives().iter().any(|primitive| matches!(
                primitive.component,
                AlignmentComponent::Insertion { .. }
            ))
        );
        assert!(page.primitives().iter().any(|primitive| {
            primitive.geometry == AlignmentGeometryToken::DirectionalTerminalRight
        }));
        assert!(page.primitives().iter().any(|primitive| {
            primitive.geometry == AlignmentGeometryToken::DirectionalTerminalLeft
        }));
        assert!(
            page.primitives().iter().all(|primitive| {
                !matches!(primitive.component, AlignmentComponent::Base { .. })
            })
        );
    }

    #[test]
    fn every_standard_read_colour_mode_produces_a_typed_body_token() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let modes = [
            AlignmentColorMode::None,
            AlignmentColorMode::Strand,
            AlignmentColorMode::MappingQuality,
            AlignmentColorMode::ReadGroup,
            AlignmentColorMode::Haplotype,
            AlignmentColorMode::TemplateLength,
            AlignmentColorMode::PairOrientation,
            AlignmentColorMode::UnexpectedPair,
            AlignmentColorMode::MateContig,
            AlignmentColorMode::FirstOfPairStrand,
            AlignmentColorMode::Tag(*b"ZZ"),
        ];
        for mode in modes {
            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Read,
                None,
                crate::AlignmentStyleContract {
                    color_mode: mode,
                    ..crate::AlignmentStyleContract::default()
                },
                AlignmentPageFamily::ReadGeometry,
            );
            let token = page
                .primitives()
                .iter()
                .find(|primitive| {
                    primitive.start_offset == 20
                        && matches!(
                            primitive.component,
                            AlignmentComponent::AlignedSegment { .. }
                        )
                })
                .unwrap()
                .style
                .body;
            let valid = match (mode, token) {
                (AlignmentColorMode::None, AlignmentBodyColorToken::Neutral)
                | (AlignmentColorMode::Strand, AlignmentBodyColorToken::Strand { .. })
                | (
                    AlignmentColorMode::MappingQuality,
                    AlignmentBodyColorToken::MappingQuality(_),
                )
                | (
                    AlignmentColorMode::ReadGroup
                    | AlignmentColorMode::Haplotype
                    | AlignmentColorMode::MateContig
                    | AlignmentColorMode::Tag(_),
                    AlignmentBodyColorToken::Category(_),
                )
                | (
                    AlignmentColorMode::TemplateLength,
                    AlignmentBodyColorToken::TemplateLength { .. },
                )
                | (
                    AlignmentColorMode::PairOrientation,
                    AlignmentBodyColorToken::PairOrientation(_),
                )
                | (AlignmentColorMode::UnexpectedPair, AlignmentBodyColorToken::PairStatus(_))
                | (
                    AlignmentColorMode::FirstOfPairStrand,
                    AlignmentBodyColorToken::FirstOfPairStrand(_),
                ) => true,
                _ => false,
            };
            assert!(valid, "wrong body token {token:?} for {mode:?}");
        }
    }

    #[test]
    fn base_page_and_picker_share_the_same_typed_component_identity() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let page = page(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Base,
            Some(&reference),
        );
        let substitutions = page
            .primitives()
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::Base {
                        classification: AlignedBaseClassification::Substitution
                            | AlignedBaseClassification::DeclaredSubstitution,
                        ..
                    }
                )
            })
            .count();

        assert!(substitutions >= 1);
        let hit = page.pick(0, 29).unwrap();
        assert!(matches!(
            hit.component,
            AlignmentComponent::DirectionTerminal
                | AlignmentComponent::HardClip { .. }
                | AlignmentComponent::Base { .. }
        ));
        let snapshot = page.allocation().snapshot().unwrap();
        assert_eq!(snapshot.class, AllocationClass::SemanticPage);
        assert_eq!(
            snapshot.charged_capacity,
            semantic_page_bytes(
                page.primitives().len(),
                page.row_offsets.len(),
                page.label_primitive_indices.len(),
                page.labels().len(),
                page.labels().iter().map(|label| label.text().len()).sum(),
            )
        );
    }

    #[test]
    fn semantic_page_build_scratch_is_separately_bounded_and_released() {
        let ledger = AllocationLedger::new_with_transient_budget(8 << 20, 0, 2 << 20);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            GeometryLod::Read,
            None,
            crate::AlignmentStyleContract::default(),
            AlignmentPageFamily::SpecialistOverlay,
        );
        assert!(!page.primitives().is_empty());
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_high_water > 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::EvidenceSelection && usage.high_water > 0
        }));
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::SemanticPageBuilder && usage.high_water > 0
        }));
    }

    /// A read aligned across all three fixture tiles with every base a called cytosine.
    fn dense_modified_read() -> AlignmentRecord {
        let length = 280_usize;
        let mut record = record(
            b"dense",
            10,
            290,
            &[(CigarOpKind::Match, 280)],
            &vec![b'C'; length],
        );
        record.auxiliary_fields = vec![
            AuxField {
                tag: *b"MM",
                value: AuxValue::String(format!("C+m{};", ",0".repeat(length))),
            },
            AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(vec![200; length]),
            },
        ];
        record
    }

    #[test]
    fn modified_base_scratch_is_bounded_by_calls_not_sequence_length_times_tag_bytes() {
        // `sequence_len x MM_tag_bytes` overlay slots came to ~25 MiB for this one 280-base
        // read; the calls it actually makes need a few tens of kilobytes.
        let ledger = AllocationLedger::new_with_transient_budget(8 << 20, 0, 1 << 20);
        let (evidence, atlas) =
            evidence_and_atlas_for_records(&ledger, &[dense_modified_read()], layout_contract());
        for color_mode in [
            ModifiedBaseColorMode::ByType,
            ModifiedBaseColorMode::ModifiedVsUnmodified,
        ] {
            let style = crate::AlignmentStyleContract {
                modified_base_color_mode: color_mode,
                ..crate::AlignmentStyleContract::default()
            };
            for (tile, expected) in [(0, 90), (1, 100), (2, 90)] {
                let page = page_with_style(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Read,
                    None,
                    style,
                    AlignmentPageFamily::SpecialistOverlay,
                );
                let modified = page
                    .primitives()
                    .iter()
                    .filter(|primitive| {
                        matches!(primitive.component, AlignmentComponent::ModifiedBase { .. })
                    })
                    .count();
                assert_eq!(modified, expected, "tile {tile}, {color_mode:?}");
            }
        }
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_high_water < (1 << 20));
    }

    /// A six-base read `ACGCAC` at `start` with one `C+m` call and optional `MN`.
    fn modified_read(
        name: &[u8],
        start: u64,
        cigar: &[(CigarOpKind, u32)],
        flags: u16,
        mm: &str,
        ml: Vec<u64>,
        mn: Option<u64>,
    ) -> AlignmentRecord {
        let aligned = cigar
            .iter()
            .filter(|(kind, _)| *kind == CigarOpKind::Match)
            .map(|(_, len)| u64::from(*len))
            .sum::<u64>();
        let mut record = record(name, start, start + aligned, cigar, b"ACGCAC");
        record.flags = AlignmentFlags::from_bits(flags);
        record.auxiliary_fields = vec![
            AuxField {
                tag: *b"MM",
                value: AuxValue::String(mm.to_owned()),
            },
            AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(ml),
            },
        ];
        if let Some(length) = mn {
            record.auxiliary_fields.push(AuxField {
                tag: *b"MN",
                value: AuxValue::Unsigned(length),
            });
        }
        record
    }

    /// What one record's modified-base evidence came to, for the overlay page and for the exact
    /// modification summary alike.
    #[derive(Debug, Eq, PartialEq)]
    enum ModificationOutcome {
        /// `(reference position, read index, probability)` of each modified call.
        Calls(Vec<(u64, u32, Option<u8>)>),
        Unavailable,
        Malformed,
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn overlays_and_the_modification_summary_decide_alike_for_every_record() {
        const SUPPLEMENTARY: u16 = 0x800;
        const REVERSE: u16 = 0x10;
        let hard_clipped = [
            (CigarOpKind::HardClip, 3),
            (CigarOpKind::Match, 6),
            (CigarOpKind::HardClip, 2),
        ];
        let soft_clipped = [(CigarOpKind::SoftClip, 2), (CigarOpKind::Match, 4)];
        // Stored SEQ is ACGCAC; its cytosines are at read indices 1, 3 and 5.
        let cases = [
            // `C+m,1` skips one C: read index 3. 3H6M2H aligns index 3 at 40 + 3 = 43. `MN` = 6
            // confirms `MM` was written for the stored (hard-clipped) sequence.
            (
                modified_read(
                    b"hard-clipped-supplementary-valid-mn",
                    40,
                    &hard_clipped,
                    SUPPLEMENTARY,
                    "C+m,1;",
                    vec![200],
                    Some(6),
                ),
                ModificationOutcome::Calls(vec![(43, 3, Some(200))]),
            ),
            // Reverse strand: `MM` counts on the original read GTGCGT, whose first C is
            // sequencer index 3, i.e. stored index 6 - 1 - 3 = 2, at 60 + 2 = 62.
            (
                modified_read(
                    b"hard-clipped-reverse-supplementary-valid-mn",
                    60,
                    &hard_clipped,
                    SUPPLEMENTARY | REVERSE,
                    "C+m,0;",
                    vec![150],
                    Some(6),
                ),
                ModificationOutcome::Calls(vec![(62, 2, Some(150))]),
            ),
            // Without `MN` nothing says the tag was rewritten for the clipped sequence.
            (
                modified_read(
                    b"hard-clipped-no-mn",
                    40,
                    &hard_clipped,
                    SUPPLEMENTARY,
                    "C+m,1;",
                    vec![200],
                    None,
                ),
                ModificationOutcome::Unavailable,
            ),
            // `MN` = 11 is the unclipped length: the tag describes bases that are not stored.
            (
                modified_read(
                    b"hard-clipped-stale-mn",
                    40,
                    &hard_clipped,
                    SUPPLEMENTARY,
                    "C+m,1;",
                    vec![200],
                    Some(11),
                ),
                ModificationOutcome::Unavailable,
            ),
            // 2S4M: read index 3 is the second aligned base, at 40 + 1 = 41.
            (
                modified_read(
                    b"soft-clipped-no-mn",
                    40,
                    &soft_clipped,
                    0,
                    "C+m,1;",
                    vec![90],
                    None,
                ),
                ModificationOutcome::Calls(vec![(41, 3, Some(90))]),
            ),
            (
                modified_read(
                    b"soft-clipped-stale-mn",
                    40,
                    &soft_clipped,
                    0,
                    "C+m,1;",
                    vec![90],
                    Some(9),
                ),
                ModificationOutcome::Unavailable,
            ),
            // One call but two probabilities.
            (
                modified_read(
                    b"probability-count-mismatch",
                    40,
                    &soft_clipped,
                    0,
                    "C+m,1;",
                    vec![90, 91],
                    Some(6),
                ),
                ModificationOutcome::Malformed,
            ),
        ];
        for (record, expected) in cases {
            let name = String::from_utf8_lossy(&record.read_name).into_owned();
            let ledger = AllocationLedger::new(8 << 20, 0);
            let (evidence, atlas) =
                evidence_and_atlas_for_records(&ledger, &[record], layout_contract());

            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Base,
                None,
                crate::AlignmentStyleContract::default(),
                AlignmentPageFamily::SpecialistOverlay,
            );
            let mut calls = Vec::new();
            for primitive in page.primitives() {
                match primitive.component {
                    // A call below one half is drawn as "unmodified" (its likelier state); its
                    // probability is still the record's single call's.
                    AlignmentComponent::ModifiedBase {
                        read_index,
                        canonical_base: b'C',
                        probability,
                        ..
                    } => {
                        assert_eq!(primitive.end_offset, primitive.start_offset + 1);
                        calls.push((primitive.start_offset, read_index, probability));
                    }
                    other => panic!("{name}: unexpected overlay {other:?}"),
                }
            }
            // A record whose calls cannot be placed or parsed is drawn as an ordinary read: no
            // overlay at all (formerly a whole-read "unavailable" overlay). The summary still
            // counts it as unavailable or malformed, never as unmodified.
            let overlay = ModificationOutcome::Calls(calls);
            let expected_overlay = match &expected {
                ModificationOutcome::Calls(calls) => ModificationOutcome::Calls(calls.clone()),
                ModificationOutcome::Unavailable | ModificationOutcome::Malformed => {
                    ModificationOutcome::Calls(Vec::new())
                }
            };

            let tile = crate::ExactModificationSummaryTile::build(
                &evidence,
                CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap()),
                300,
                crate::ModificationSummaryContract,
                &ledger,
                ReservationKind::Normal,
                9,
            )
            .unwrap();
            let summary = tile.summary();
            assert_eq!(
                summary.provenance.records_with_modification_tags, 1,
                "{name}"
            );
            let summarized = match (
                summary.provenance.unavailable_records,
                summary.provenance.malformed_records,
            ) {
                (0, 0) => ModificationOutcome::Calls(
                    summary
                        .series
                        .iter()
                        .flat_map(|series| series.bins.iter().enumerate())
                        .filter(|(_, bin)| bin.modified_calls > 0)
                        .map(|(position, bin)| {
                            assert_eq!(bin.modified_calls, 1, "{name}");
                            // The summary keeps no read index; take it from the expectation
                            // so position and probability are what is compared.
                            let read_index = match &expected {
                                ModificationOutcome::Calls(expected) => expected
                                    .iter()
                                    .find(|(start, _, _)| *start == position as u64)
                                    .map_or(u32::MAX, |(_, read_index, _)| *read_index),
                                _ => u32::MAX,
                            };
                            (
                                position as u64,
                                read_index,
                                (bin.probability_observations == 1)
                                    .then(|| u8::try_from(bin.probability_sum).unwrap()),
                            )
                        })
                        .collect(),
                ),
                (1, 0) => ModificationOutcome::Unavailable,
                (0, 1) => ModificationOutcome::Malformed,
                other => panic!("{name}: unexpected provenance {other:?}"),
            };

            assert_eq!(overlay, expected_overlay, "{name}: overlay");
            assert_eq!(summarized, expected, "{name}: summary");
        }
    }

    /// The modification cells of one record's overlay page, as `(reference position, state,
    /// code, probability)`.
    fn modification_cells(
        record: AlignmentRecord,
        lod: GeometryLod,
        reference: Option<&ReferenceSlice>,
        style: crate::AlignmentStyleContract,
    ) -> Vec<(
        u64,
        ModificationState,
        rgv_model::ModificationCode,
        Option<u8>,
    )> {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) =
            evidence_and_atlas_for_records(&ledger, &[record], layout_contract());
        let page = page_with_style(
            &ledger,
            &evidence,
            &atlas,
            0,
            lod,
            reference,
            style,
            AlignmentPageFamily::SpecialistOverlay,
        );
        page.primitives()
            .iter()
            .map(|primitive| match primitive.component {
                AlignmentComponent::ModifiedBase {
                    code,
                    probability,
                    state,
                    ..
                } => (primitive.start_offset, state, code, probability),
                other => panic!("unexpected overlay {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_record_without_usable_modification_calls_is_an_ordinary_read() {
        const SUPPLEMENTARY: u16 = 0x800;
        // A supplementary alignment with no MM/ML at all, and one whose MM was copied from
        // its primary onto the hard-clipped sequence (no MN), as aligners commonly do.
        let mut tagless = record(
            b"tagless-supplementary",
            40,
            46,
            &[(CigarOpKind::Match, 6)],
            b"ACGCAC",
        );
        tagless.flags = AlignmentFlags::from_bits(SUPPLEMENTARY);
        let copied = modified_read(
            b"copied-mm-supplementary",
            40,
            &[
                (CigarOpKind::HardClip, 3),
                (CigarOpKind::Match, 6),
                (CigarOpKind::HardClip, 2),
            ],
            SUPPLEMENTARY,
            "C+m,1;",
            vec![250],
            None,
        );
        for record in [tagless, copied] {
            let name = String::from_utf8_lossy(&record.read_name).into_owned();
            for mode in [
                ModifiedBaseColorMode::ByType,
                ModifiedBaseColorMode::Mono,
                ModifiedBaseColorMode::ModifiedVsUnmodified,
            ] {
                let style = crate::AlignmentStyleContract {
                    color_mode: AlignmentColorMode::None,
                    modified_base_color_mode: mode,
                    ..crate::AlignmentStyleContract::default()
                };
                let ledger = AllocationLedger::new(8 << 20, 0);
                let (evidence, atlas) = evidence_and_atlas_for_records(
                    &ledger,
                    std::slice::from_ref(&record),
                    layout_contract(),
                );
                for (family, lod) in [
                    (AlignmentPageFamily::SpecialistOverlay, GeometryLod::Read),
                    (AlignmentPageFamily::SpecialistOverlay, GeometryLod::Base),
                    (AlignmentPageFamily::ReadGeometry, GeometryLod::Read),
                    (AlignmentPageFamily::ReadGeometry, GeometryLod::Base),
                ] {
                    let page =
                        page_with_style(&ledger, &evidence, &atlas, 0, lod, None, style, family);
                    for primitive in page.primitives() {
                        assert!(
                            !matches!(
                                primitive.component,
                                AlignmentComponent::SpecialistUnavailable { .. }
                                    | AlignmentComponent::ModifiedBase { .. }
                            ),
                            "{name} {mode:?} {family:?} {lod:?}: {:?}",
                            primitive.component
                        );
                        assert_eq!(
                            primitive.style.body,
                            AlignmentBodyColorToken::Neutral,
                            "{name}: the body keeps the active colour mode"
                        );
                    }
                    if family == AlignmentPageFamily::ReadGeometry {
                        assert!(
                            page.primitives().iter().any(|primitive| matches!(
                                primitive.component,
                                AlignmentComponent::ReadSpan
                                    | AlignmentComponent::AlignedSegment { .. }
                            )),
                            "{name}: the read body is drawn"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_cell_shows_the_likelier_state_of_its_base() {
        const M: rgv_model::ModificationCode = rgv_model::ModificationCode::Letter(b'm');
        const H: rgv_model::ModificationCode = rgv_model::ModificationCode::Letter(b'h');
        let style = crate::AlignmentStyleContract::default();
        // ACGCAC at 40: cytosines at 41, 43 and 45. `C+m?,0,0,0` calls all three.
        let read = |mm: &str, ml: Vec<u64>| {
            modified_read(b"calls", 40, &[(CigarOpKind::Match, 6)], 0, mm, ml, None)
        };
        assert_eq!(
            modification_cells(
                read("C+m?,0,0,0;", vec![250, 128, 20]),
                GeometryLod::Read,
                None,
                style
            ),
            vec![
                (41, ModificationState::Modified, M, Some(250)),
                (43, ModificationState::Modified, M, Some(128)),
                // 20/255 modified: unmodified is likelier. The cell keeps the summed
                // modification probability.
                (45, ModificationState::Canonical, M, Some(20)),
            ]
        );
        // Two codes at one base: the likeliest is drawn when it beats "no modification"; two
        // modest calls together still make the base more likely modified than not.
        assert_eq!(
            modification_cells(
                read("C+h?,0,0;C+m?,0,0;", vec![30, 100, 200, 100]),
                GeometryLod::Read,
                None,
                style
            ),
            vec![
                (41, ModificationState::Modified, M, Some(200)),
                (43, ModificationState::Modified, H, Some(100)),
            ]
        );
        // A hidden code that is the likeliest state hides the cell; it still counts against
        // "unmodified".
        let hidden = crate::AlignmentStyleContract {
            hidden_modification_codes: {
                let mut codes = [None; crate::MAXIMUM_HIDDEN_MODIFICATION_CODES];
                codes[0] = Some(H);
                codes
            },
            ..style
        };
        assert_eq!(
            modification_cells(
                read("C+h?,0;C+m?,0;", vec![200, 40]),
                GeometryLod::Read,
                None,
                hidden
            ),
            Vec::new()
        );
        // The minimum probability applies to the shown state, modified or not.
        let minimum = crate::AlignmentStyleContract {
            minimum_modification_probability: 200,
            ..style
        };
        assert_eq!(
            modification_cells(
                read("C+m?,0,0,0;", vec![250, 128, 20]),
                GeometryLod::Read,
                None,
                minimum
            ),
            vec![
                (41, ModificationState::Modified, M, Some(250)),
                (45, ModificationState::Canonical, M, Some(20)),
            ]
        );
        // Every modification cell is coloured by type in the by-type mode only.
        for (mode, by_type) in [
            (ModifiedBaseColorMode::ByType, true),
            (ModifiedBaseColorMode::Mono, false),
            (ModifiedBaseColorMode::ModifiedVsUnmodified, false),
        ] {
            let ledger = AllocationLedger::new(8 << 20, 0);
            let (evidence, atlas) = evidence_and_atlas_for_records(
                &ledger,
                &[read("C+m?,0;", vec![250])],
                layout_contract(),
            );
            let page = page_with_style(
                &ledger,
                &evidence,
                &atlas,
                0,
                GeometryLod::Read,
                None,
                crate::AlignmentStyleContract {
                    modified_base_color_mode: mode,
                    ..style
                },
                AlignmentPageFamily::SpecialistOverlay,
            );
            assert!(page.primitives().iter().all(|primitive| matches!(
                primitive.component,
                AlignmentComponent::ModifiedBase { color_by_type, .. } if color_by_type == by_type
            )));
        }
    }

    #[test]
    fn a_mismatch_takes_priority_over_a_modification_on_the_same_base() {
        const M: rgv_model::ModificationCode = rgv_model::ModificationCode::Letter(b'm');
        // ACGCAC at 40 with all three cytosines called; the reference has T at 43, so the read's
        // C there is a mismatch.
        let read = modified_read(
            b"mismatch",
            40,
            &[(CigarOpKind::Match, 6)],
            0,
            "C+m?,0,0,0;",
            vec![250, 250, 250],
            None,
        );
        let mut bases = vec![b'A'; 300];
        bases[40..46].copy_from_slice(b"ACGTAC");
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases,
        };
        let style = crate::AlignmentStyleContract::default();
        let modified = |positions: &[u64]| {
            positions
                .iter()
                .map(|position| (*position, ModificationState::Modified, M, Some(250)))
                .collect::<Vec<_>>()
        };
        // At base detail the mismatch cell is drawn and the modification cell is not.
        assert_eq!(
            modification_cells(read.clone(), GeometryLod::Base, Some(&reference), style),
            modified(&[41, 45])
        );
        // Without a reference only `X` operations are mismatches: nothing gives way here.
        assert_eq!(
            modification_cells(read.clone(), GeometryLod::Base, None, style),
            modified(&[41, 43, 45])
        );
        // With bases hidden, or zoomed out where no mismatch cell is drawn, the modification
        // is drawn.
        let hidden_bases = crate::AlignmentStyleContract {
            base_display: ReadBaseDisplayMode::Hidden,
            ..style
        };
        assert_eq!(
            modification_cells(
                read.clone(),
                GeometryLod::Base,
                Some(&reference),
                hidden_bases
            ),
            modified(&[41, 43, 45])
        );
        assert_eq!(
            modification_cells(read, GeometryLod::Read, Some(&reference), style),
            modified(&[41, 43, 45])
        );
        // The overlay page reads the reference exactly when the rule needs it.
        assert!(style.modification_overlay_reads_reference(GeometryLod::Base));
        assert!(!style.modification_overlay_reads_reference(GeometryLod::Read));
        assert!(!hidden_bases.modification_overlay_reads_reference(GeometryLod::Base));
        assert_ne!(
            PresentationContract {
                geometry_lod: GeometryLod::Base,
                style,
                reference: None,
                annotation: None,
            }
            .page_fingerprint(AlignmentPageFamily::SpecialistOverlay),
            PresentationContract {
                geometry_lod: GeometryLod::Base,
                style: hidden_bases,
                reference: None,
                annotation: None,
            }
            .page_fingerprint(AlignmentPageFamily::SpecialistOverlay),
        );
    }

    #[test]
    fn modified_base_call_bound_counts_calls_and_series_from_the_tag_text() {
        let bound = modified_base_call_bound(&AuxValue::String(
            "C+mh,0,1,2;A+a?,5;C+76792.,1,1;;".to_owned(),
        ));
        assert_eq!(
            bound,
            ModifiedBaseCallBound {
                calls: 2 * 3 + 1 + 2,
                series: 2 + 1 + 1,
            }
        );
        assert_eq!(
            modified_base_call_bound(&AuxValue::Unsigned(3)),
            ModifiedBaseCallBound::default()
        );
        // The bound is never below what the decoder yields.
        let sequence = b"CCACCACCCCAAACCC";
        let mm = AuxValue::String("C+mh,0,1,2;A+a?,3;".to_owned());
        let decoded = rgv_model::decode_modified_bases_parts(
            sequence,
            AlignmentFlags::from_bits(0),
            &mm,
            None,
        )
        .unwrap();
        let bound = modified_base_call_bound(&mm);
        assert_eq!(decoded.len(), 7);
        assert_eq!(bound.calls, 7);
        assert_eq!(bound.series, 3);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn primitive_reservation_is_clipped_to_the_tile_and_never_below_what_is_emitted() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let mut records = vec![
            dense_modified_read(),
            record(
                b"events",
                20,
                30,
                &[
                    (CigarOpKind::SoftClip, 2),
                    (CigarOpKind::Match, 5),
                    (CigarOpKind::Insertion, 2),
                    (CigarOpKind::Deletion, 3),
                    (CigarOpKind::SequenceMismatch, 2),
                    (CigarOpKind::Padding, 1),
                    (CigarOpKind::HardClip, 1),
                ],
                b"AAACCCCCCCC",
            ),
            record(
                b"spliced",
                90,
                210,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::ReferenceSkip, 100),
                    (CigarOpKind::Match, 10),
                ],
                b"AAAAAAAAAAAAAAAAAAAA",
            ),
            // Clips and an insertion exactly on a tile seam, and a deletion crossing one.
            record(
                b"seams",
                100,
                205,
                &[
                    (CigarOpKind::HardClip, 3),
                    (CigarOpKind::SoftClip, 4),
                    (CigarOpKind::Match, 95),
                    (CigarOpKind::Deletion, 10),
                    (CigarOpKind::Insertion, 2),
                    (CigarOpKind::SoftClip, 5),
                ],
                &[b'G'; 106],
            ),
            record(b"cigarless", 250, 260, &[], b""),
        ];
        records[1].flags = AlignmentFlags::from_bits(0x10);
        let (evidence, _) = evidence_and_atlas_for_records(&ledger, &records, layout_contract());
        let style = crate::AlignmentStyleContract {
            base_display: ReadBaseDisplayMode::All,
            ..crate::AlignmentStyleContract::default()
        };
        let label = (AlignmentLabelKind::ReadName, "name".to_owned());
        let mut dense_base_bounds = Vec::new();
        for tile_bases in [7_u64, 100, 128, 300] {
            for tile_start in (0..300).step_by(usize::try_from(tile_bases).unwrap()) {
                let tile = GenomicInterval::new(
                    ContigId::new(0),
                    tile_start,
                    (tile_start + tile_bases).min(300),
                )
                .unwrap();
                let selection = evidence.select(tile).unwrap();
                for index in 0..selection.len() {
                    let view = selection.record_view(index).unwrap();
                    for lod in [
                        GeometryLod::Overview,
                        GeometryLod::Span,
                        GeometryLod::Read,
                        GeometryLod::Base,
                    ] {
                        let bound = record_primitive_bound(view, tile, lod, 1);
                        for family in [
                            AlignmentPageFamily::ReadGeometry,
                            AlignmentPageFamily::Labels,
                        ] {
                            let mut primitives = Vec::new();
                            let mut labels = Vec::new();
                            emit_record_primitives(
                                &mut primitives,
                                &mut labels,
                                selection.records()[index].handle,
                                0,
                                view,
                                selection.sequence(index),
                                AlignmentBodyColorToken::Neutral,
                                lod,
                                tile,
                                tile.start(),
                                None,
                                style,
                                Some(&label),
                                family,
                                None,
                            )
                            .unwrap();
                            let reserved = if family == AlignmentPageFamily::ReadGeometry {
                                bound.geometry
                            } else {
                                // One read label besides the CIGAR event labels.
                                bound.event_labels + 1
                            };
                            assert!(
                                primitives.len() <= reserved,
                                "{:?} {lod:?} {family:?} in {tile:?}: emitted {} > reserved {reserved}",
                                String::from_utf8_lossy(view.read_name()),
                                primitives.len(),
                            );
                            if family == AlignmentPageFamily::ReadGeometry
                                && lod == GeometryLod::Base
                            {
                                // With every base displayed the clipped count is exact apart
                                // from a direction terminal lying in another tile.
                                assert!(reserved <= primitives.len() + 1);
                                if view.read_name() == b"dense" && tile_bases == 100 {
                                    dense_base_bounds.push(reserved);
                                }
                            }
                        }
                    }
                }
            }
        }
        // Segment + direction terminal + the bases inside each tile; the whole-CIGAR count
        // reserved all 280 bases for every one of the three pages.
        assert_eq!(dense_base_bounds, vec![92, 102, 92]);
    }

    #[test]
    fn empty_row_atlas_still_produces_an_explicit_empty_page() {
        assert_eq!(
            CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap())
                .range(0)
                .unwrap(),
            0..0
        );
    }

    #[test]
    fn presentation_scene_publishes_only_with_a_complete_mandatory_page_set() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(12, 13)),
            Arc::clone(&evidence),
            Arc::clone(&atlas),
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages.clone(),
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();

        assert!(scene.covers(
            GenomicInterval::new(ContigId::new(0), 50, 250).unwrap(),
            0..1
        ));
        assert_eq!(scene.pages().len(), 9);
        assert_eq!(
            scene.metadata_allocation().snapshot().unwrap().class,
            AllocationClass::PresentationScene
        );
        assert!(ledger.snapshot().unwrap().pinned > 0);

        let error = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(14, 15)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages
                .iter()
                .filter(|page| {
                    !(page.key().family() == AlignmentPageFamily::ReadGeometry
                        && page.key().genomic_tile().coordinate() == 1)
                })
                .cloned()
                .collect(),
            &ledger,
            ReservationKind::Normal,
            21,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            crate::AlignmentPresentationSceneError::MissingMandatoryBlock {
                first_missing_base: 100,
                ..
            }
        ));
    }

    #[test]
    fn cpu_projection_and_picking_are_camera_only_views_of_one_scene_snapshot() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(20, 21)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            30,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 50, 250).unwrap(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 100.0,
            },
        )
        .unwrap();
        let before = ledger.snapshot().unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 7, camera).unwrap();
        let before_pick = ledger.snapshot().unwrap();
        let hit = crate::CpuAlignmentProjector::pick(
            &scene,
            rgv_core::TrackId::new(42),
            &frame,
            500.0,
            5.0,
        )
        .unwrap()
        .unwrap();
        let after_pick = ledger.snapshot().unwrap();
        let after = ledger.snapshot().unwrap();

        assert_eq!(frame.scene_revision, scene.revision());
        assert_eq!(frame.camera_sequence, 7);
        assert_eq!(frame.coverage.covered_interval, Some(camera.interval()));
        assert!(matches!(
            hit.component,
            AlignmentComponent::ReferenceSkip { .. }
        ));
        assert!(frame.primitives.iter().any(|primitive| {
            primitive.handle == hit.handle && primitive.component == hit.component
        }));
        let interaction = crate::AlignmentInteractionState::new(
            Some(hit.handle),
            Some(hit.handle),
            scene.supplementary_chain_handles(hit.handle),
        );
        let interactive =
            crate::CpuAlignmentProjector::project_with_interaction(&scene, 9, camera, &interaction)
                .unwrap();
        assert!(interactive.primitives.iter().any(|primitive| {
            primitive.handle == hit.handle
                && primitive.interaction.hovered
                && primitive.interaction.selected
                && primitive.interaction.selected_chain
        }));
        assert!(scene.contains_handle(hit.handle));
        assert!(
            scene
                .edge_handle(crate::AlignmentTraversalDirection::Next)
                .is_some()
        );
        assert!(
            scene
                .edge_handle(crate::AlignmentTraversalDirection::Previous)
                .is_some()
        );
        let hit_record = scene.inspect_retained(hit.handle).unwrap().unwrap();
        assert!(
            scene
                .handles_for_read_name(&hit_record.read_name)
                .contains(&hit.handle)
        );
        assert!(
            scene
                .adjacent_handle(hit.handle, crate::AlignmentTraversalDirection::Next)
                .is_some()
        );
        assert!(scene.mate_handles(hit.handle).is_empty());
        assert_eq!(before_pick, after_pick, "pointer picking must not allocate");
        assert_eq!(before.charged, after.charged);
        assert_eq!(before.records.len(), after.records.len());
        let exported = scene
            .export_full(GenomicInterval::new(ContigId::new(0), 0, 300).unwrap())
            .unwrap();
        assert_eq!(exported.len(), 3);
        let mut streamed = Vec::new();
        scene
            .for_each_full(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |record| {
                    streamed.push(record);
                    true
                },
            )
            .unwrap();
        assert_eq!(streamed, exported);
        let mut stopped_after_one = 0;
        scene
            .for_each_full(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |_| {
                    stopped_after_one += 1;
                    false
                },
            )
            .unwrap();
        assert_eq!(stopped_after_one, 1);
        let mut identities = Vec::new();
        scene
            .for_each_identity(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                |handle, read_name, flags| {
                    identities.push((handle, read_name.to_vec(), flags));
                },
            )
            .unwrap();
        assert_eq!(identities.len(), exported.len());
        assert_eq!(
            scene.modification_codes(10, 10).unwrap(),
            vec![rgv_model::ModificationCode::Letter(b'm')]
        );
        assert_eq!(
            scene.inspect_retained(hit.handle).unwrap().unwrap(),
            exported
                .iter()
                .find(|record| record.handle == hit.handle)
                .unwrap()
                .record
        );
        assert_eq!(
            exported
                .iter()
                .map(|record| record.handle)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );

        let mut stale_frame = frame.clone();
        stale_frame.scene_revision = crate::SceneRevision::new(RevisionFingerprint::new(99, 100));
        assert!(matches!(
            crate::CpuAlignmentProjector::pick(
                &scene,
                rgv_core::TrackId::new(42),
                &stale_frame,
                500.0,
                5.0,
            ),
            Err(crate::CpuInteractionError::StaleSceneFrame { .. })
        ));

        let edge_camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 250, 350).unwrap(),
            0.0,
            10.0,
            8.0,
            camera.clip(),
        )
        .unwrap();
        let edge = crate::CpuAlignmentProjector::project(&scene, 8, edge_camera).unwrap();
        assert_eq!(
            edge.coverage.covered_interval,
            Some(GenomicInterval::new(ContigId::new(0), 250, 300).unwrap())
        );
        assert!(!edge.primitives.is_empty());
    }

    #[test]
    fn pair_chain_picking_inspection_and_export_share_one_stable_handle_authority() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let (evidence, atlas) = paired_evidence_and_atlas(&ledger);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 300).unwrap();
        let selection = evidence.select(interval).unwrap();
        let mut pair_first = None;
        let mut pair_last = None;
        let mut primary = None;
        let mut supplementary = None;
        let mut secondary = None;
        for (index, reference) in selection.records().iter().enumerate() {
            let record = selection.record_view(index).unwrap();
            match (
                record.read_name(),
                record.flags().is_first_in_template(),
                record.flags().is_last_in_template(),
                record.flags().is_supplementary(),
                record.flags().is_secondary(),
            ) {
                (b"pair", true, false, false, false) => pair_first = Some(reference.handle),
                (b"pair", false, true, false, false) => pair_last = Some(reference.handle),
                (b"split", false, false, false, false) => primary = Some(reference.handle),
                (b"split", false, false, true, false) => supplementary = Some(reference.handle),
                (b"split", false, false, false, true) => secondary = Some(reference.handle),
                _ => {}
            }
        }
        let pair_first = pair_first.unwrap();
        let pair_last = pair_last.unwrap();
        let primary = primary.unwrap();
        let supplementary = supplementary.unwrap();
        let secondary = secondary.unwrap();
        drop(selection);

        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let connectors = pages
            .iter()
            .flat_map(|page| {
                page.primitives().iter().filter_map(move |primitive| {
                    if !matches!(
                        primitive.component,
                        AlignmentComponent::PairConnector { .. }
                    ) {
                        return None;
                    }
                    let mate = primitive.pair_connector_mate();
                    (primitive.handle == pair_first && mate == Some(pair_last)).then_some((
                        page.interval().start() + primitive.start_offset,
                        page.interval().start() + primitive.end_offset,
                    ))
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(connectors, vec![(60, 100), (100, 150)]);

        let row_count = u32::try_from(atlas.row_count()).unwrap();
        let pair_row = atlas.record(pair_first).unwrap().record.row();
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(40, 41)),
            Arc::clone(&evidence),
            atlas,
            presentation,
            interval,
            0..row_count,
            pages,
            &ledger,
            ReservationKind::Normal,
            50,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            interval,
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: f64::from(row_count.max(1)) * 10.0,
            },
        )
        .unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 1, camera).unwrap();
        let hit = crate::CpuAlignmentProjector::pick(
            &scene,
            rgv_core::TrackId::new(9),
            &frame,
            105.0 / 300.0 * 900.0,
            f64::from(pair_row) * 10.0 + 5.0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.handle, pair_first);
        assert!(matches!(
            hit.component,
            AlignmentComponent::PairConnector { mate: Some(_), .. }
        ));
        assert_eq!(
            hit.component.pair_connector_mate(hit.handle),
            Some(pair_last)
        );
        assert_eq!(scene.mate_handles(pair_first), vec![pair_last]);
        assert_eq!(scene.mate_handles(pair_last), vec![pair_first]);

        let chain = scene.supplementary_chain_handles(primary);
        assert_eq!(chain.len(), 2);
        assert!(chain.contains(&primary));
        assert!(chain.contains(&supplementary));
        assert!(!chain.contains(&secondary));

        let exported = scene.export_full(interval).unwrap();
        assert_eq!(exported.len(), 5);
        assert_eq!(
            exported
                .iter()
                .map(|record| record.handle)
                .collect::<Vec<_>>(),
            evidence
                .select(interval)
                .unwrap()
                .records()
                .iter()
                .map(|record| record.handle)
                .collect::<Vec<_>>()
        );
        for record in exported {
            assert_eq!(
                scene.inspect_retained(record.handle).unwrap().unwrap(),
                record.record
            );
        }
    }

    #[test]
    fn cpu_labels_reconcile_tile_fragments_and_screen_collisions_without_rekeying_pages() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::ReadName,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let mut page_keys = pages.iter().map(|page| page.key()).collect::<Vec<_>>();
        page_keys.sort_unstable();
        let spliced_fragments = pages
            .iter()
            .flat_map(|page| {
                page.primitives().iter().filter_map(move |primitive| {
                    let AlignmentComponent::Label {
                        kind: AlignmentLabelKind::ReadName,
                        id,
                    } = primitive.component
                    else {
                        return None;
                    };
                    (page.label(id) == Some("spliced")).then_some(id)
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(spliced_fragments.len(), 3);
        assert!(
            spliced_fragments
                .iter()
                .all(|id| *id == spliced_fragments[0])
        );

        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(30, 31)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap();
        let clip = crate::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 900.0,
            height: 20.0,
        };
        let wide_camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0.0,
            10.0,
            8.0,
            clip,
        )
        .unwrap();
        let wide = crate::CpuAlignmentProjector::project(&scene, 1, wide_camera).unwrap();
        let wide_labels = crate::CpuAlignmentProjector::project_labels(&scene, 1, wide_camera)
            .expect("immutable label anchors project independently of read geometry");
        let visible_anchor_count = scene
            .visible_pages(wide_camera.interval(), wide_camera.visible_rows())
            .into_iter()
            .filter(|page| page.key().family() == AlignmentPageFamily::Labels)
            .map(|page| page.label_anchor_count())
            .sum::<usize>();
        assert_eq!(wide_labels.anchors_examined, visible_anchor_count);
        assert_eq!(
            wide.primitives
                .iter()
                .filter(|primitive| {
                    primitive.shape == crate::CpuPrimitiveShape::TextLabel
                        && primitive.text.as_deref() == Some("spliced")
                })
                .count(),
            1
        );
        assert_eq!(
            wide_labels
                .labels
                .iter()
                .filter(|primitive| primitive.text.as_deref() == Some("spliced"))
                .count(),
            1
        );

        let narrow_camera = crate::AlignmentCamera::new(
            wide_camera.interval(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                width: 50.0,
                ..clip
            },
        )
        .unwrap();
        let narrow = crate::CpuAlignmentProjector::project(&scene, 2, narrow_camera).unwrap();
        assert!(narrow.primitives.iter().all(|primitive| {
            primitive.shape != crate::CpuPrimitiveShape::TextLabel
                || primitive.text.as_deref() != Some("spliced")
        }));
        let mut projected_page_keys = scene
            .pages()
            .iter()
            .map(|page| page.key())
            .collect::<Vec<_>>();
        projected_page_keys.sort_unstable();
        assert_eq!(projected_page_keys, page_keys);
    }

    #[test]
    fn base_cells_precede_glyphs_and_default_height_can_show_letters() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let style = crate::AlignmentStyleContract {
            base_display: ReadBaseDisplayMode::All,
            ..crate::AlignmentStyleContract::default()
        };
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Base,
            style,
            reference: Some(crate::ReferenceRevision::new(RevisionFingerprint::new(
                10, 11,
            ))),
            annotation: None,
        };
        let mut pages = Vec::new();
        for tile in 0..3 {
            for family in presentation.required_page_families() {
                pages.push(page_with_style(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Base,
                    Some(&reference),
                    style,
                    family,
                ));
            }
        }
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(51, 52)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            53,
        )
        .unwrap();
        let camera = |width: f64, row_height: f64| {
            crate::AlignmentCamera::new(
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
                0.0,
                row_height,
                row_height,
                crate::CpuClipRect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height: row_height,
                },
            )
            .unwrap()
        };
        let colour_only_camera = camera(100.0, 5.0);
        let colour_only =
            crate::CpuAlignmentProjector::project(&scene, 0, colour_only_camera).unwrap();
        assert!(colour_only.primitives.iter().any(|primitive| {
            matches!(
                primitive.component,
                AlignmentComponent::Base {
                    classification: AlignedBaseClassification::Substitution
                        | AlignedBaseClassification::DeclaredSubstitution,
                    ..
                }
            ) && primitive.shape == crate::CpuPrimitiveShape::BaseCell
        }));
        let colour_only_labels =
            crate::CpuAlignmentProjector::project_labels(&scene, 0, colour_only_camera).unwrap();
        assert!(colour_only_labels.base_anchors_examined > 0);
        assert_eq!(colour_only_labels.base_labels_projected, 0);

        let legible =
            crate::CpuAlignmentProjector::project_labels(&scene, 1, camera(700.0, 5.0)).unwrap();
        assert!(legible.base_anchors_examined > 0);
        assert!(legible.base_labels_projected > 0);
        assert!(legible.labels.iter().any(|label| {
            matches!(label.component, AlignmentComponent::Base { .. })
                && label
                    .text
                    .as_deref()
                    .is_some_and(|text| matches!(text, "A" | "C" | "G" | "T" | "N"))
        }));
        let too_narrow =
            crate::CpuAlignmentProjector::project_labels(&scene, 2, camera(699.0, 5.0)).unwrap();
        assert_eq!(
            too_narrow.base_anchors_examined,
            legible.base_anchors_examined
        );
        assert_eq!(too_narrow.base_labels_projected, 0);
        assert!(
            too_narrow
                .labels
                .iter()
                .all(|label| !matches!(label.component, AlignmentComponent::Base { .. }))
        );
        let too_short =
            crate::CpuAlignmentProjector::project_labels(&scene, 3, camera(700.0, 4.9)).unwrap();
        assert_eq!(
            too_short.base_anchors_examined,
            legible.base_anchors_examined
        );
        assert_eq!(too_short.base_labels_projected, 0);
        assert!(
            too_short
                .labels
                .iter()
                .all(|label| !matches!(label.component, AlignmentComponent::Base { .. }))
        );
    }

    #[test]
    fn cpu_projection_reserves_before_work_and_remains_accounted_while_a_frame_is_live() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let pages = pages_for_presentation(&ledger, &evidence, &atlas, presentation);
        let scene = crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(50, 51)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0..1,
            pages,
            &ledger,
            ReservationKind::Normal,
            60,
        )
        .unwrap();
        let camera = crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            0.0,
            10.0,
            8.0,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 20.0,
            },
        )
        .unwrap();
        let charged = ledger.snapshot().unwrap().charged;
        ledger.set_transient_budget(0).unwrap();
        assert!(matches!(
            crate::CpuAlignmentProjector::project(&scene, 1, camera).unwrap_err(),
            crate::CpuProjectionError::Ledger(LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::CpuProjection,
                ..
            })
        ));
        let refused = ledger.snapshot().unwrap();
        assert_eq!(refused.charged, charged);
        assert_eq!(refused.transient, 0);

        ledger.set_transient_budget(4 << 20).unwrap();
        let frame = crate::CpuAlignmentProjector::project(&scene, 2, camera).unwrap();
        let live = ledger.snapshot().unwrap();
        assert!(live.transient > 0);
        assert!(live.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::CpuProjection && usage.current > 0
        }));
        let cloned = frame.clone();
        assert!(Arc::ptr_eq(&frame.primitives, &cloned.primitives));
        drop(frame);
        assert!(ledger.snapshot().unwrap().transient > 0);
        drop(cloned);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
    }

    /// Handle of the `ordinal`-th record given to `evidence_and_atlas_for_records`.
    fn handle_of(ordinal: usize) -> AlignmentHandle {
        AlignmentHandle::new(
            source_revision(),
            AlignmentSourcePosition::bam_virtual_offset(
                (7 << 16) | u64::try_from(ordinal + 1).unwrap(),
            ),
        )
    }

    /// Every page (three 100-base tiles, every required family) of `records` at `lod`, with an
    /// all-`A` reference at base detail.
    fn scene_for_records(
        ledger: &AllocationLedger,
        records: &[AlignmentRecord],
        lod: GeometryLod,
        style: crate::AlignmentStyleContract,
    ) -> Arc<crate::AlignmentPresentationScene> {
        let (evidence, atlas) = evidence_and_atlas_for_records(ledger, records, layout_contract());
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let reference = (lod == GeometryLod::Base).then_some(&reference);
        let presentation = PresentationContract {
            geometry_lod: lod,
            style,
            reference: reference
                .map(|_| crate::ReferenceRevision::new(RevisionFingerprint::new(10, 11))),
            annotation: None,
        };
        let mut pages = Vec::new();
        for tile in 0..3 {
            for family in presentation.required_page_families() {
                pages.push(page_with_style(
                    ledger, &evidence, &atlas, tile, lod, reference, style, family,
                ));
            }
        }
        let rows = 0..u32::try_from(atlas.row_count()).unwrap();
        crate::AlignmentPresentationScene::new(
            crate::SceneRevision::new(RevisionFingerprint::new(61, 62)),
            evidence,
            atlas,
            presentation,
            GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            rows,
            pages,
            ledger,
            ReservationKind::Normal,
            63,
        )
        .unwrap()
    }

    /// A camera over `start..end` drawn `width` points wide from pane x = 10, reads 11 points
    /// tall in 14-point rows.
    fn clip_camera(start: u64, end: u64, width: f64) -> crate::AlignmentCamera {
        crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), start, end).unwrap(),
            0.0,
            14.0,
            11.0,
            crate::CpuClipRect {
                x: 10.0,
                y: 0.0,
                width,
                height: 140.0,
            },
        )
        .unwrap()
    }

    fn pane_x(camera: crate::AlignmentCamera, position: u64) -> f64 {
        10.0 + (position as f64 - camera.exact_start()) / camera.bases_per_pixel()
    }

    fn with_flags(mut record: AlignmentRecord, flags: u16) -> AlignmentRecord {
        record.flags = AlignmentFlags::from_bits(flags);
        record
    }

    /// The reported defect: a spliced read drew an arrowhead at the end of every exon block,
    /// so one molecule looked like several reads. Only the read's true terminal is directional:
    /// the right end of a forward read's last block, the left end of a reverse read's first.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn spliced_reads_carry_their_direction_arrow_only_at_the_true_terminal() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let sequence = |length: usize| vec![b'A'; length];
        let records = [
            // Reverse 44M112N57M: blocks [50, 94) and [206, 263).
            with_flags(
                record(
                    b"reverse-spliced",
                    50,
                    263,
                    &[
                        (CigarOpKind::Match, 44),
                        (CigarOpKind::ReferenceSkip, 112),
                        (CigarOpKind::Match, 57),
                    ],
                    &sequence(101),
                ),
                0x10,
            ),
            // Forward 30M100N20M60N10M: blocks [20, 50), [150, 170), [230, 240).
            record(
                b"forward-spliced",
                20,
                240,
                &[
                    (CigarOpKind::Match, 30),
                    (CigarOpKind::ReferenceSkip, 100),
                    (CigarOpKind::Match, 20),
                    (CigarOpKind::ReferenceSkip, 60),
                    (CigarOpKind::Match, 10),
                ],
                &sequence(60),
            ),
            // Unspliced controls, one per strand, the forward one with a deletion.
            record(
                b"forward",
                110,
                130,
                &[
                    (CigarOpKind::Match, 8),
                    (CigarOpKind::Deletion, 2),
                    (CigarOpKind::Match, 10),
                ],
                &sequence(18),
            ),
            with_flags(
                record(
                    b"reverse",
                    270,
                    290,
                    &[(CigarOpKind::Match, 20)],
                    &sequence(20),
                ),
                0x10,
            ),
        ];
        // (record, terminal base, directional geometry)
        let expected = [
            (0, 50, AlignmentGeometryToken::DirectionalTerminalLeft),
            (1, 239, AlignmentGeometryToken::DirectionalTerminalRight),
            (2, 129, AlignmentGeometryToken::DirectionalTerminalRight),
            (3, 270, AlignmentGeometryToken::DirectionalTerminalLeft),
        ];
        for lod in [GeometryLod::Span, GeometryLod::Read, GeometryLod::Base] {
            let scene = scene_for_records(
                &ledger,
                &records,
                lod,
                crate::AlignmentStyleContract {
                    base_display: ReadBaseDisplayMode::All,
                    ..crate::AlignmentStyleContract::default()
                },
            );
            for (ordinal, terminal, geometry) in expected {
                let handle = handle_of(ordinal);
                let mut terminals = 0;
                let mut directional_segments = 0;
                let mut square_segments = 0;
                for page in scene.pages() {
                    if page.key().family() != AlignmentPageFamily::ReadGeometry {
                        continue;
                    }
                    let start = page.interval().start();
                    for primitive in page.primitives().iter().filter(|p| p.handle == handle) {
                        let directional = matches!(
                            primitive.geometry,
                            AlignmentGeometryToken::DirectionalTerminalLeft
                                | AlignmentGeometryToken::DirectionalTerminalRight
                        );
                        let covers_terminal = start + primitive.start_offset <= terminal
                            && terminal < start + primitive.end_offset;
                        if directional {
                            assert_eq!(primitive.geometry, geometry, "{lod:?} {primitive:?}");
                            assert!(
                                covers_terminal,
                                "{lod:?} arrow off the terminal: {primitive:?}"
                            );
                        }
                        match primitive.component {
                            AlignmentComponent::DirectionTerminal => {
                                assert!(directional && covers_terminal);
                                terminals += 1;
                            }
                            AlignmentComponent::AlignedSegment { .. } if directional => {
                                directional_segments += 1;
                            }
                            AlignmentComponent::AlignedSegment { .. } => {
                                assert_eq!(primitive.geometry, AlignmentGeometryToken::Rectangle);
                                square_segments += 1;
                            }
                            _ => {}
                        }
                    }
                }
                // One arrowhead per read, wherever its tiles split it.
                assert_eq!(terminals, 1, "{lod:?} record {ordinal}");
                assert_eq!(directional_segments, 1, "{lod:?} record {ordinal}");
                // Every other aligned block keeps square ends: the reverse read's [206, 263),
                // the forward read's [20, 50) and [150, 170), and the block before the
                // unspliced forward read's deletion.
                let square = [1, 2, 1, 0][ordinal];
                assert_eq!(square_segments, square, "{lod:?} record {ordinal}");
            }
        }
    }

    /// The reported defect: zoomed out from base detail, a read grew a second, smaller point
    /// past its arrowhead, because the aligned block, the one-base direction terminal and the
    /// base cell at the tip each sized their head from their own width. All of them are now cut
    /// by the read's one diagonal (min(0.55 x read height, 10 bases) long, from the read's tip;
    /// none under half a point), so at every zoom the union of the read's primitives is exactly
    /// one arrow (or one square end): covered height grows monotonically from the tip, nothing
    /// of the read is inside the cut-away corners, and picking there does not select the read.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn tip_primitives_share_one_diagonal_so_the_read_has_one_clean_arrowhead_at_every_zoom() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let mut forward_bases = vec![b'A'; 60];
        // A mismatch on the forward read's terminal base, so a coloured cell sits at the tip.
        forward_bases[59] = b'C';
        let mut reverse_bases = vec![b'A'; 60];
        reverse_bases[0] = b'G';
        let records = [
            record(
                b"forward",
                100,
                160,
                &[(CigarOpKind::Match, 60)],
                &forward_bases,
            ),
            with_flags(
                record(
                    b"reverse",
                    150,
                    210,
                    &[(CigarOpKind::Match, 60)],
                    &reverse_bases,
                ),
                0x10,
            ),
        ];
        // Mismatch cells only (the default), so the terminal base's cell is the only one at the
        // tip. (Cells a few bases short of it are covered by
        // `cells_short_of_a_read_tip_are_cut_by_its_arrowhead_at_every_zoom`.)
        let scene = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                base_display: ReadBaseDisplayMode::Mismatches,
                ..crate::AlignmentStyleContract::default()
            },
        );
        // (record, tip position, points right)
        for (ordinal, tip_position, points_right) in [(0, 160, true), (1, 150, false)] {
            // Down to 0.04 points per base, where the head (ten bases) is under half a point
            // and the read ends square.
            for points_per_base in [0.04_f64, 0.06, 0.2, 0.5, 2.0, 10.0, 40.0] {
                // About forty points either side of the tip, but at most 100 bases.
                let span = ((40.0 / points_per_base).ceil() as u64).min(100);
                let width = 2.0 * span as f64 * points_per_base;
                let camera = clip_camera(tip_position - span, tip_position + span, width);
                let frame = crate::CpuAlignmentProjector::project(&scene, 0, camera).unwrap();
                let context = format!("record {ordinal} at {points_per_base} points per base");
                let read = frame
                    .primitives
                    .iter()
                    .filter(|primitive| {
                        primitive.handle == handle_of(ordinal)
                            && primitive.shape != crate::CpuPrimitiveShape::TextLabel
                    })
                    .collect::<Vec<_>>();
                let tip = read
                    .iter()
                    .filter(|primitive| primitive.arrow.is_some())
                    .collect::<Vec<_>>();
                // The block, the one-base terminal and the base cell all carry the arrow ...
                for wanted in ["AlignedSegment", "DirectionTerminal", "Base"] {
                    assert!(
                        tip.iter()
                            .any(|primitive| format!("{:?}", primitive.component)
                                .starts_with(wanted)),
                        "{context}: no {wanted} at the tip"
                    );
                }
                // ... and the same arrow: the read's tip, centre line and height.
                let arrow = tip[0].arrow.unwrap();
                assert!(tip.iter().all(|primitive| primitive.arrow == Some(arrow)));
                assert_eq!(arrow.points_right, points_right, "{context}");
                let tip_x = pane_x(camera, tip_position);
                assert!((arrow.tip_x - tip_x).abs() < 1.0e-9, "{context}: {arrow:?}");
                assert!((arrow.height - 11.0).abs() < 1.0e-9);
                for primitive in &tip {
                    // Every tip primitive ends on the read's tip.
                    let edge = if points_right {
                        primitive.x + primitive.width
                    } else {
                        primitive.x
                    };
                    assert!((edge - tip_x).abs() < 1.0e-9, "{context}: {primitive:?}");
                }

                // Written out independently: the head is 0.55 x 11 points long, but at most
                // ten bases, and none under half a point; the read is covered where
                // |y - centre| <= 5.5 x min(d / head, 1).
                let head = (11.0_f64 * 0.55).min(points_per_base * 10.0);
                let head = if head < 0.5 { 0.0 } else { head };
                assert!((arrow.length() - head).abs() < 1.0e-9, "{context}");
                let (centre, half) = (arrow.centre_y, 5.5);
                // From a point outside the tip to the end of the sampled read (its 60 bases, or
                // the pane, or twenty points, whichever is nearest).
                let reach = (60.0 * points_per_base)
                    .min(span as f64 * points_per_base)
                    .min(20.0)
                    - 0.01;
                let mut columns = Vec::new();
                // Off-grid steps, so no sample sits exactly on an outline.
                for column in 0..400 {
                    let distance = -1.0 + f64::from(column) * (reach + 1.0) / 400.0 + 0.0011;
                    let x = if points_right {
                        tip_x - distance
                    } else {
                        tip_x + distance
                    };
                    let mut covered_height = 0;
                    for row in 0..300 {
                        let y = centre - half - 1.0 + f64::from(row) * 0.0437 + 0.0007;
                        let covered = read.iter().any(|primitive| primitive.contains(x, y));
                        let expected = distance > 0.0
                            && (y - centre).abs() < half
                            && (head == 0.0
                                || (y - centre).abs() <= half * (distance / head).min(1.0));
                        assert_eq!(
                            covered, expected,
                            "{context}: ({x}, {y}) is {distance} from the tip"
                        );
                        covered_height += usize::from(covered);
                    }
                    columns.push((distance, covered_height));
                }
                // One monotonic diagonal: moving into the read, the covered height never drops.
                columns.sort_by(|a, b| a.0.total_cmp(&b.0));
                for pair in columns.windows(2) {
                    assert!(pair[0].1 <= pair[1].1, "{context}: step at {pair:?}");
                }
                if head > 0.0 {
                    assert!(columns.iter().any(|(distance, height)| *distance > 0.0
                        && *distance < head
                        && *height > 0
                        && *height < 250));
                } else {
                    // A square end: every sampled column inside the read is full height.
                    let full = columns.iter().map(|(_, height)| *height).max().unwrap();
                    assert!(
                        columns
                            .iter()
                            .all(|(distance, height)| *distance <= 0.0 || *height == full),
                        "{context}: {columns:?}"
                    );
                }

                // Hit-testing: inside the cut-away corner nothing is picked; on the centre line
                // just inside the tip the read is.
                let track = rgv_core::TrackId::new(3);
                if head > 0.0 {
                    let corner_x = if points_right {
                        tip_x - head * 0.4
                    } else {
                        tip_x + head * 0.4
                    };
                    let corner = crate::CpuAlignmentProjector::pick(
                        &scene,
                        track,
                        &frame,
                        corner_x,
                        centre - half + 0.5,
                    )
                    .unwrap();
                    assert!(
                        corner.is_none_or(|hit| hit.handle != handle_of(ordinal)),
                        "{context}: picked {corner:?} in the cut-away corner"
                    );
                }
                let inside_x = if points_right {
                    tip_x - 0.2
                } else {
                    tip_x + 0.2
                };
                let inside =
                    crate::CpuAlignmentProjector::pick(&scene, track, &frame, inside_x, centre)
                        .unwrap()
                        .expect("the arrow tip picks its read");
                assert_eq!(inside.handle, handle_of(ordinal), "{context}");
            }
        }
    }

    /// The reported defect: zoomed out (about 3.7 points per base), mismatch cells a few bases
    /// short of a read's tip were drawn full height inside its arrowhead, poking out of the
    /// cut-away corners, because only primitives ending on the tip knew where it was. Every
    /// rectangle whose tip-side edge is within nine bases of the tip now records that distance
    /// and is cut by the read's one diagonal: mismatch cells 1, 3 and 9 bases in, a block piece
    /// after an insertion near the tip and a block piece ending on a tile boundary near it. At
    /// every zoom nothing of the read lies outside its arrow outline, cells beyond the head are
    /// whole, a cell 12 bases in is a plain rectangle, and picking in a cut corner misses.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn cells_short_of_a_read_tip_are_cut_by_its_arrowhead_at_every_zoom() {
        // (record, tip, points right, mismatch positions and their distance from the tip)
        type TipCase = (usize, u64, bool, &'static [(u64, u64)]);
        let ledger = AllocationLedger::new(16 << 20, 0);
        let mut forward_bases = vec![b'A'; 60];
        // Forward [100, 160), tip 160: mismatches 1, 3, 9 and 12 bases from the tip.
        for index in [58, 56, 50, 47] {
            forward_bases[index] = b'C';
        }
        // Reverse `3M 2I 55M` over [120, 180), tip 120: mismatches at 121, 123, 129 and 132
        // (read indices skip the two inserted bases after the first three).
        let mut reverse_bases = vec![b'A'; 60];
        for index in [1, 5, 11, 14] {
            reverse_bases[index] = b'G';
        }
        // Forward [190, 205), tip 205 on the next tile: mismatches 2 (on the tip's tile) and 7
        // (on the previous tile, whose block piece ends 5 bases short of the tip).
        let mut crossing_bases = vec![b'A'; 15];
        crossing_bases[12] = b'T';
        crossing_bases[7] = b'T';
        let records = [
            record(
                b"forward",
                100,
                160,
                &[(CigarOpKind::Match, 60)],
                &forward_bases,
            ),
            with_flags(
                record(
                    b"reverse",
                    120,
                    180,
                    &[
                        (CigarOpKind::Match, 3),
                        (CigarOpKind::Insertion, 2),
                        (CigarOpKind::Match, 55),
                    ],
                    &reverse_bases,
                ),
                0x10,
            ),
            record(
                b"crossing",
                190,
                205,
                &[(CigarOpKind::Match, 15)],
                &crossing_bases,
            ),
        ];
        let scene = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                base_display: ReadBaseDisplayMode::Mismatches,
                ..crate::AlignmentStyleContract::default()
            },
        );
        let reads: [TipCase; 3] = [
            (0, 160, true, &[(158, 1), (156, 3), (150, 9), (147, 12)]),
            (1, 120, false, &[(121, 1), (123, 3), (129, 9), (132, 12)]),
            (2, 205, true, &[(202, 2), (197, 7)]),
        ];

        // The semantic geometry: every mismatch within nine bases records its distance; the
        // one twelve bases in is a plain rectangle; so are the block pieces short of the tip.
        for (ordinal, tip, points_right, mismatches) in reads {
            let mut seen = 0;
            for page in scene.pages() {
                for primitive in page
                    .primitives()
                    .iter()
                    .filter(|primitive| primitive.handle == handle_of(ordinal))
                {
                    let start = page.interval().start() + primitive.start_offset;
                    let end = page.interval().start() + primitive.end_offset;
                    let expected = |bases: u64| {
                        if bases <= 9 {
                            AlignmentGeometryToken::InsideArrowhead(
                                ArrowheadCellToken::new(points_right, bases as u8).unwrap(),
                            )
                        } else {
                            AlignmentGeometryToken::Rectangle
                        }
                    };
                    match primitive.component {
                        AlignmentComponent::Base { .. } => {
                            let bases = mismatches
                                .iter()
                                .find(|(position, _)| *position == start)
                                .expect("only the fixture's mismatches are drawn")
                                .1;
                            assert_eq!(primitive.geometry, expected(bases), "{ordinal} at {start}");
                            seen += 1;
                        }
                        AlignmentComponent::AlignedSegment { .. }
                            if (points_right && end < tip) || (!points_right && start > tip) =>
                        {
                            let bases = if points_right { tip - end } else { start - tip };
                            assert_eq!(primitive.geometry, expected(bases), "{ordinal} block");
                            seen += 1;
                        }
                        _ => {}
                    }
                }
            }
            // The mismatches, plus the block after the insertion or before the tile boundary.
            let pieces = usize::from(ordinal != 0);
            assert_eq!(seen, mismatches.len() + pieces, "record {ordinal}");
        }

        let track = rgv_core::TrackId::new(3);
        for (ordinal, tip_position, points_right, mismatches) in reads {
            for points_per_base in [0.6_f64, 2.0, 3.7, 4.0, 10.0] {
                // At least forty points, and every mismatch, either side of the tip.
                let span = ((40.0 / points_per_base).ceil() as u64).max(15);
                let width = 2.0 * span as f64 * points_per_base;
                let camera = clip_camera(tip_position - span, tip_position + span, width);
                let frame = crate::CpuAlignmentProjector::project(&scene, 0, camera).unwrap();
                let context = format!("record {ordinal} at {points_per_base} points per base");
                let read = frame
                    .primitives
                    .iter()
                    .filter(|primitive| {
                        primitive.handle == handle_of(ordinal)
                            && primitive.shape != crate::CpuPrimitiveShape::TextLabel
                    })
                    .collect::<Vec<_>>();
                // One arrow for the whole read, whichever primitive carries it.
                let arrow = read
                    .iter()
                    .find_map(|primitive| primitive.arrow)
                    .expect("the read has an arrow");
                assert!(
                    read.iter()
                        .all(|primitive| primitive.arrow.is_none_or(|found| found == arrow)),
                    "{context}"
                );
                let tip_x = pane_x(camera, tip_position);
                assert!((arrow.tip_x - tip_x).abs() < 1.0e-9, "{context}");
                // Written out independently: min(0.55 x 11, 10 bases) long (0.5 points at
                // least), covering |y - centre| <= 5.5 x min(d / head, 1).
                let head = (11.0_f64 * 0.55).min(10.0 * points_per_base);
                assert!(head >= 0.5);
                let (centre, half) = (arrow.centre_y, 5.5);
                let into_read = |x: f64| if points_right { tip_x - x } else { x - tip_x };
                let inside_outline = |x: f64, y: f64| {
                    let distance = into_read(x);
                    distance > 0.0
                        && (y - centre).abs() < half
                        && (y - centre).abs() <= half * (distance / head).min(1.0)
                };
                // The whole read near its tip is exactly the arrow outline: no cell or block
                // piece pokes out of a cut-away corner. (The insertion's marker is a boundary
                // mark drawn over the read, not part of its outline.)
                for column in 0..600 {
                    let x = tip_x - 1.5 * head + f64::from(column) * 0.0503 + 0.0011;
                    for row in 0..300 {
                        let y = centre - half - 1.0 + f64::from(row) * 0.0437 + 0.0007;
                        let covered = read.iter().any(|primitive| {
                            !primitive.geometry.is_boundary_mark() && primitive.contains(x, y)
                        });
                        assert_eq!(
                            covered,
                            inside_outline(x, y),
                            "{context}: ({x}, {y}) is {} from the tip",
                            into_read(x)
                        );
                    }
                }
                let cells = read
                    .iter()
                    .filter(|primitive| {
                        matches!(primitive.component, AlignmentComponent::Base { .. })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(cells.len(), mismatches.len(), "{context}");
                for cell in cells {
                    let bases = match cell.geometry {
                        AlignmentGeometryToken::InsideArrowhead(token) => {
                            assert_eq!(token.points_right(), points_right);
                            assert_eq!(cell.arrow, Some(arrow), "{context}");
                            u64::from(token.bases_from_tip())
                        }
                        geometry => {
                            assert_eq!(geometry, AlignmentGeometryToken::Rectangle);
                            assert_eq!(cell.arrow, None, "{context}");
                            12
                        }
                    };
                    assert!(mismatches.iter().any(|(_, distance)| *distance == bases));
                    // An arrow-cut cell keeps its tip-side edge `bases` whole bases from the tip
                    // (its one-point minimum grows away from the tip); a plain cell keeps its
                    // start edge, as every rectangle does.
                    let (edge, expected) = if cell.arrow.is_none() {
                        let start = if points_right {
                            tip_position - bases - 1
                        } else {
                            tip_position + bases
                        };
                        (cell.x, pane_x(camera, start))
                    } else if points_right {
                        (cell.x + cell.width, tip_x - bases as f64 * points_per_base)
                    } else {
                        (cell.x, tip_x + bases as f64 * points_per_base)
                    };
                    assert!((edge - expected).abs() < 1.0e-9, "{context}: {cell:?}");
                    // Every covered point of the cell is inside the outline, and the cell is
                    // whole beyond the head.
                    for step_x in 0..40 {
                        let x = cell.x + cell.width * (f64::from(step_x) + 0.5) / 40.0;
                        for step_y in 0..60 {
                            let y = cell.y + cell.height * (f64::from(step_y) + 0.5) / 60.0;
                            let covered = cell.contains(x, y);
                            if covered {
                                assert!(inside_outline(x, y), "{context}: {cell:?} at ({x}, {y})");
                            }
                            if into_read(x) >= head {
                                assert!(covered, "{context}: {cell:?} cut at ({x}, {y})");
                            }
                        }
                    }
                    // Within the head, a cut corner above the cell picks nothing of the read,
                    // while the centre line picks the cell itself.
                    let cell_centre = cell.x + cell.width * 0.5;
                    let allowed = half * (into_read(cell_centre) / head).min(1.0);
                    if allowed + 0.3 < half {
                        let corner = crate::CpuAlignmentProjector::pick(
                            &scene,
                            track,
                            &frame,
                            cell_centre,
                            centre - allowed - 0.3,
                        )
                        .unwrap();
                        assert!(
                            corner.is_none_or(|hit| hit.handle != handle_of(ordinal)),
                            "{context}: picked {corner:?} in the cut-away corner"
                        );
                    }
                    if cell.width >= 0.5 {
                        let hit = crate::CpuAlignmentProjector::pick(
                            &scene,
                            track,
                            &frame,
                            cell_centre,
                            centre,
                        )
                        .unwrap()
                        .expect("the cell's centre picks it");
                        assert_eq!(hit.component, cell.component, "{context}");
                    }
                }
            }
        }
    }

    /// Clips are cut edges on the read's own boundary, never purple point markers: a soft
    /// clip's sawtooth or a hard clip's bar, just inside the boundary, moved behind the
    /// arrowhead when the clip is at the read's terminal. The CPU projector places them as the
    /// shared extent rule says at every zoom.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn clips_are_cut_edges_inside_the_boundary_and_compose_with_the_arrowhead() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let records = [
            // Forward: leading soft clip (no arrow there), trailing soft clip at the arrow.
            record(
                b"forward",
                100,
                120,
                &[
                    (CigarOpKind::SoftClip, 5),
                    (CigarOpKind::Match, 20),
                    (CigarOpKind::SoftClip, 3),
                ],
                &[b'A'; 28],
            ),
            // Reverse: leading hard clip at the arrow, trailing hard clip without one.
            with_flags(
                record(
                    b"reverse",
                    150,
                    170,
                    &[
                        (CigarOpKind::HardClip, 4),
                        (CigarOpKind::Match, 20),
                        (CigarOpKind::HardClip, 6),
                    ],
                    &[b'A'; 20],
                ),
                0x10,
            ),
        ];
        let scene = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Read,
            crate::AlignmentStyleContract::default(),
        );
        let edge = |at_end, hard, behind_arrow| ClipEdgeToken {
            at_end,
            hard,
            behind_arrow,
        };
        // (record, clip boundary, token, component is soft)
        let clips = [
            (0, 100, edge(false, false, false), true),
            (0, 120, edge(true, false, true), true),
            (1, 150, edge(false, true, true), false),
            (1, 170, edge(true, true, false), false),
        ];
        for page in scene.pages() {
            for primitive in page.primitives() {
                if matches!(
                    primitive.component,
                    AlignmentComponent::SoftClip { .. } | AlignmentComponent::HardClip { .. }
                ) {
                    assert!(!primitive.geometry.is_point_marker(), "{primitive:?}");
                    assert!(matches!(
                        primitive.geometry,
                        AlignmentGeometryToken::ClipEdge(_)
                    ));
                }
            }
        }
        // 0.05, 0.2, 0.5, 4, 12 and 40 points per base.
        for (start, end, width) in [
            (0, 300, 15.0),
            (0, 300, 60.0),
            (0, 300, 150.0),
            (90, 180, 360.0),
            (95, 175, 960.0),
            (95, 180, 3_400.0),
        ] {
            let camera = clip_camera(start, end, width);
            let points_per_base = 1.0 / camera.bases_per_pixel();
            let frame = crate::CpuAlignmentProjector::project(&scene, 0, camera).unwrap();
            for (ordinal, boundary, token, soft) in clips {
                let projected = frame
                    .primitives
                    .iter()
                    .filter(|primitive| {
                        primitive.handle == handle_of(ordinal)
                            && primitive.geometry == AlignmentGeometryToken::ClipEdge(token)
                            // (The clip's length label shares the token as its anchor.)
                            && primitive.shape != crate::CpuPrimitiveShape::TextLabel
                    })
                    .collect::<Vec<_>>();
                if points_per_base < 0.1 {
                    // Neither drawn nor picked so far out.
                    assert!(projected.is_empty(), "{points_per_base} {token:?}");
                    continue;
                }
                assert_eq!(projected.len(), 1, "{points_per_base} {token:?}");
                let projected = projected[0];
                assert_eq!(
                    projected.shape,
                    if points_per_base < 1.0 {
                        crate::CpuPrimitiveShape::BoundaryHairline
                    } else {
                        crate::CpuPrimitiveShape::ClipEdge
                    }
                );
                assert_eq!(
                    matches!(projected.component, AlignmentComponent::SoftClip { .. }),
                    soft
                );
                // Written out independently of `clip_edge_extent`: a one-point hairline below
                // one point per base.
                let expected_width = if points_per_base < 1.0 {
                    1.0
                } else if token.hard {
                    2.0
                } else {
                    (points_per_base * 0.5).clamp(2.0, 6.0)
                };
                // The head length depends on the read height and the zoom alone.
                let arrowhead = if token.behind_arrow {
                    (11.0_f64 * 0.55).min(points_per_base * 10.0)
                } else {
                    0.0
                };
                let boundary_x = pane_x(camera, boundary);
                // The arrow tip is the read's boundary: below one point per base the terminal
                // base cell keeps its one-point minimum by growing away from the tip.
                let tip = boundary_x;
                let expected_left = if token.at_end && token.behind_arrow {
                    tip - arrowhead - expected_width
                } else if token.at_end {
                    boundary_x - expected_width
                } else {
                    boundary_x + arrowhead
                };
                assert!(
                    (projected.x - expected_left).abs() < 1.0e-9
                        && (projected.width - expected_width).abs() < 1.0e-9,
                    "{points_per_base} {token:?}: {projected:?}"
                );
                assert!((projected.height - 11.0).abs() < 1.0e-9);
                // Inside the read, and once a base is wider than the edge it never fills one.
                if points_per_base >= 4.0 {
                    if token.at_end {
                        assert!(projected.x + projected.width <= boundary_x + 1.0e-9);
                    } else {
                        assert!(projected.x >= boundary_x - 1.0e-9);
                    }
                    assert!(projected.width < points_per_base);
                }
                if token.behind_arrow {
                    // The direction terminal of the same read, on the same boundary base: the
                    // cut edge starts exactly where its arrowhead's base is.
                    let terminal = frame
                        .primitives
                        .iter()
                        .find(|primitive| {
                            primitive.handle == handle_of(ordinal)
                                && primitive.component == AlignmentComponent::DirectionTerminal
                        })
                        .unwrap();
                    let head = terminal.arrow.unwrap().length();
                    assert!((head - arrowhead).abs() < 1.0e-9);
                    if token.at_end {
                        let terminal_tip = terminal.x + terminal.width;
                        assert!((terminal_tip - tip).abs() < 1.0e-9);
                        assert!(
                            (projected.x + projected.width - (terminal_tip - head)).abs() < 1.0e-9
                        );
                    } else {
                        assert!((terminal.x - boundary_x).abs() < 1.0e-9);
                        assert!((projected.x - (terminal.x + head)).abs() < 1.0e-9);
                    }
                }
            }
        }
    }

    /// The soft clip's sawtooth zigzags between the boundary and the inner side; a hard clip's
    /// bar is solid.
    #[test]
    fn soft_clip_edges_are_a_zigzag_and_hard_clip_edges_a_solid_bar() {
        let soft = ClipEdgeToken {
            at_end: false,
            hard: false,
            behind_arrow: false,
        };
        let (width, height) = (6.0, 11.0);
        // Three teeth of 11 / 3 points: the stroke centre is on the boundary at y = 0 and at
        // the inner side half a tooth later.
        let period: f64 = 11.0 / 3.0;
        let painted = |y: f64, edge: ClipEdgeToken| {
            (0..600)
                .map(|step| (f64::from(step) + 0.5) / 100.0)
                .filter(|x| crate::clip_edge_covers(*x, y, width, height, edge))
                .collect::<Vec<_>>()
        };
        let at_vertex = painted(0.01, soft);
        let at_inner = painted(period * 0.5, soft);
        assert!(at_vertex.iter().all(|x| *x < 3.0), "{at_vertex:?}");
        assert!(at_inner.iter().all(|x| *x > 3.0), "{at_inner:?}");
        assert!(!at_vertex.is_empty() && !at_inner.is_empty());
        // The trailing edge is the mirror image.
        let trailing = ClipEdgeToken {
            at_end: true,
            ..soft
        };
        assert!(painted(0.01, trailing).iter().all(|x| *x > 3.0));
        // Every scanline has its stroke, and no scanline is solid.
        for row in 0..110 {
            let y = (f64::from(row) + 0.5) / 10.0;
            let columns = painted(y, soft).len();
            assert!(columns > 0 && columns < 600, "{y}: {columns}");
        }
        let hard = ClipEdgeToken { hard: true, ..soft };
        assert_eq!(painted(period * 0.5, hard).len(), 600);
    }

    fn ghosts_of(
        scene: &crate::AlignmentPresentationScene,
        handle: AlignmentHandle,
    ) -> Vec<(u64, u32, u8, bool, u32)> {
        let mut ghosts = scene
            .pages()
            .iter()
            .flat_map(|page| {
                let start = page.interval().start();
                let row_start = page.row_range().start;
                page.primitives()
                    .iter()
                    .filter_map(move |primitive| match primitive.component {
                        AlignmentComponent::ClipGhostBase {
                            read_index,
                            base,
                            obscured,
                            ..
                        } if primitive.handle == handle => {
                            assert_eq!(primitive.geometry, AlignmentGeometryToken::ClipGhost);
                            assert_eq!(primitive.end_offset, primitive.start_offset + 1);
                            Some((
                                start + primitive.start_offset,
                                read_index,
                                base,
                                obscured,
                                row_start + primitive.row_offset,
                            ))
                        }
                        _ => None,
                    })
            })
            .collect::<Vec<_>>();
        ghosts.sort_unstable();
        ghosts
    }

    /// Soft-clipped bases are drawn as ghosts beyond the cut edge only at base detail, one per
    /// reference position outward from the boundary, outside the aligned extent, carrying the
    /// record's own SEQ bases (as stored, also for a reverse read), and labelled in lowercase.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn soft_clip_ghosts_come_from_seq_only_at_base_detail_and_lie_outside_the_read() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let records = [
            // Forward 3S10M4S at 110: SEQ TGC | AAAAAAAAAA | GTCA. (Ghosts are drawn by the
            // pages holding the read, so these all lie inside the [100, 200) tile.)
            record(
                b"forward",
                110,
                120,
                &[
                    (CigarOpKind::SoftClip, 3),
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::SoftClip, 4),
                ],
                b"TGCAAAAAAAAAAGTCA",
            ),
            // Reverse 2S8M at 150: SEQ (reference orientation) CG | AAAAAAAA.
            with_flags(
                record(
                    b"reverse",
                    150,
                    158,
                    &[(CigarOpKind::SoftClip, 2), (CigarOpKind::Match, 8)],
                    b"CGAAAAAAAA",
                ),
                0x10,
            ),
            // A leading clip longer than the distance to the contig start: clamped there.
            record(
                b"contig-start",
                2,
                12,
                &[(CigarOpKind::SoftClip, 5), (CigarOpKind::Match, 10)],
                b"ACGTCAAAAAAAAAA",
            ),
        ];
        let style = crate::AlignmentStyleContract::default();
        // Outside base detail no page of any family carries a ghost cell or a ghost letter,
        // so nothing pale is drawn where the letters could not be.
        for lod in [GeometryLod::Overview, GeometryLod::Span, GeometryLod::Read] {
            let scene = scene_for_records(&ledger, &records, lod, style);
            for ordinal in 0..records.len() {
                assert!(ghosts_of(&scene, handle_of(ordinal)).is_empty(), "{lod:?}");
            }
            for page in scene.pages() {
                assert!(
                    page.primitives().iter().all(|primitive| {
                        primitive.geometry != AlignmentGeometryToken::ClipGhost
                            && !matches!(
                                primitive.component,
                                AlignmentComponent::ClipGhostBase { .. }
                            )
                    }),
                    "{lod:?} {:?}",
                    page.key()
                );
            }
        }
        let hidden = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                base_display: ReadBaseDisplayMode::Hidden,
                ..style
            },
        );
        assert!(ghosts_of(&hidden, handle_of(0)).is_empty());

        let scene = scene_for_records(&ledger, &records, GeometryLod::Base, style);
        let simple = |ghosts: Vec<(u64, u32, u8, bool, u32)>| {
            ghosts
                .into_iter()
                .map(|(position, read_index, base, obscured, _)| {
                    assert!(!obscured);
                    (position, read_index, base)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            simple(ghosts_of(&scene, handle_of(0))),
            [
                (107, 0, b'T'),
                (108, 1, b'G'),
                (109, 2, b'C'),
                (120, 13, b'G'),
                (121, 14, b'T'),
                (122, 15, b'C'),
                (123, 16, b'A'),
            ]
        );
        // Not complemented or reversed: SEQ is already in reference orientation.
        assert_eq!(
            simple(ghosts_of(&scene, handle_of(1))),
            [(148, 0, b'C'), (149, 1, b'G')]
        );
        // Five clipped bases but only two reference positions before the read: the outermost
        // three are not drawn.
        assert_eq!(
            simple(ghosts_of(&scene, handle_of(2))),
            [(0, 3, b'T'), (1, 4, b'C')]
        );

        // Letters: lowercase, on the ghost cells, once the cells are legible (12 points/base).
        let camera = clip_camera(100, 160, 720.0);
        let labels = crate::CpuAlignmentProjector::project_labels(&scene, 0, camera).unwrap();
        let mut ghost_letters = labels
            .labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::ClipGhostBase { .. }))
            .map(|label| {
                (
                    ((label.x - 10.0) / 12.0).round() as u64 + 100,
                    label.text.as_deref().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        ghost_letters.sort();
        assert_eq!(
            ghost_letters,
            [
                (107, "t"),
                (108, "g"),
                (109, "c"),
                (120, "g"),
                (121, "t"),
                (122, "c"),
                (123, "a"),
                (148, "c"),
                (149, "g"),
            ]
            .map(|(position, text)| (position, text.to_owned()))
        );
        // The CPU frame draws them as translucent ghost cells, ahead of everything else in
        // their row.
        let frame = crate::CpuAlignmentProjector::project(&scene, 0, camera).unwrap();
        let ghost_cells = frame
            .primitives
            .iter()
            .filter(|primitive| primitive.shape == crate::CpuPrimitiveShape::ClipGhostCell)
            .count();
        assert_eq!(ghost_cells, 9);
    }

    /// Ghost cells paint beneath every other read of their row (so a neighbour covers them),
    /// and their letters are suppressed wherever another read's aligned extent, or another
    /// read's ghost, occupies the cell.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn soft_clip_ghosts_draw_beneath_neighbouring_reads_and_lose_their_letters_there() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let records = [
            // A: [100, 110) with five trailing clipped bases, ghosts at 110..115.
            record(
                b"a",
                100,
                110,
                &[(CigarOpKind::Match, 10), (CigarOpKind::SoftClip, 5)],
                b"AAAAAAAAAACGTCG",
            ),
            // B: [113, 123), packed into A's row (three bases after A, the minimum spacing of
            // reads in a row); it covers A's ghosts at 113..115.
            record(b"b", 113, 123, &[(CigarOpKind::Match, 10)], b"AAAAAAAAAA"),
            // C: [200, 210) with five trailing clipped bases (ghosts 210..215) and D: [217, 227)
            // with four leading ones (ghosts 213..217): the two reads' ghosts share 213 and 214.
            record(
                b"c",
                200,
                210,
                &[(CigarOpKind::Match, 10), (CigarOpKind::SoftClip, 5)],
                b"AAAAAAAAAACGTCG",
            ),
            record(
                b"d",
                217,
                227,
                &[(CigarOpKind::SoftClip, 4), (CigarOpKind::Match, 10)],
                b"TTTTAAAAAAAAAA",
            ),
        ];
        let scene = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract::default(),
        );
        let a = ghosts_of(&scene, handle_of(0));
        let obscured = |ghosts: &[(u64, u32, u8, bool, u32)]| {
            ghosts
                .iter()
                .map(|(position, _, _, obscured, _)| (*position, *obscured))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            obscured(&a),
            [
                (110, false),
                (111, false),
                (112, false),
                (113, true),
                (114, true)
            ]
        );
        let row = a[0].4;
        assert!(a.iter().all(|ghost| ghost.4 == row));
        // B shares A's row, and within that row (in page draw order, which the GPU follows)
        // every ghost precedes B's body.
        let page = scene
            .pages()
            .iter()
            .find(|page| {
                page.key().family() == AlignmentPageFamily::ReadGeometry
                    && page.interval().contains(113)
            })
            .unwrap();
        let local_row = row - page.row_range().start;
        let row_primitives = page
            .primitives()
            .iter()
            .filter(|primitive| primitive.row_offset == local_row)
            .collect::<Vec<_>>();
        let b_body = row_primitives
            .iter()
            .position(|primitive| {
                primitive.handle == handle_of(1)
                    && matches!(
                        primitive.component,
                        AlignmentComponent::AlignedSegment { .. }
                    )
            })
            .expect("B is in A's row");
        let last_ghost = row_primitives
            .iter()
            .rposition(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::ClipGhostBase { .. }
                )
            })
            .unwrap();
        assert!(last_ghost < b_body);
        assert!(
            row_primitives[..=last_ghost]
                .iter()
                .all(|primitive| primitive.geometry == AlignmentGeometryToken::ClipGhost)
        );
        // The CPU projector paints in the same order.
        let camera = clip_camera(95, 230, 1_620.0);
        let frame = crate::CpuAlignmentProjector::project(&scene, 0, camera).unwrap();
        let in_row = frame
            .primitives
            .iter()
            .filter(|primitive| primitive.row == row)
            .collect::<Vec<_>>();
        let first_body = in_row
            .iter()
            .position(|primitive| primitive.shape != crate::CpuPrimitiveShape::ClipGhostCell)
            .unwrap();
        assert!(
            in_row[first_body..]
                .iter()
                .all(|primitive| primitive.shape != crate::CpuPrimitiveShape::ClipGhostCell)
        );
        // Picking a covered ghost cell selects the read on top of it.
        let x = pane_x(camera, 113) + 6.0;
        let y = (f64::from(row) + 0.5) * 14.0;
        let hit =
            crate::CpuAlignmentProjector::pick(&scene, rgv_core::TrackId::new(1), &frame, x, y)
                .unwrap()
                .unwrap();
        assert_eq!(hit.handle, handle_of(1));

        // C and D: shared cells lose both letters, unshared ones keep theirs.
        assert_eq!(
            obscured(&ghosts_of(&scene, handle_of(2))),
            [
                (210, false),
                (211, false),
                (212, false),
                (213, true),
                (214, true)
            ]
        );
        assert_eq!(
            obscured(&ghosts_of(&scene, handle_of(3))),
            [(213, true), (214, true), (215, false), (216, false)]
        );
        let labels = crate::CpuAlignmentProjector::project_labels(&scene, 0, camera).unwrap();
        let mut lettered = labels
            .labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::ClipGhostBase { .. }))
            .map(|label| (label.x - 10.0) / 12.0 + 95.0)
            .map(|position| position.round() as u64)
            .collect::<Vec<_>>();
        lettered.sort_unstable();
        assert_eq!(lettered, [110, 111, 112, 210, 211, 212, 215, 216]);
    }

    fn event_labels(
        scene: &Arc<crate::AlignmentPresentationScene>,
        camera: crate::AlignmentCamera,
        kind: AlignmentLabelKind,
    ) -> Vec<crate::CpuProjectedPrimitive> {
        crate::CpuAlignmentProjector::project_labels(scene, 0, camera)
            .unwrap()
            .labels
            .iter()
            .filter(|label| {
                matches!(label.component, AlignmentComponent::Label { kind: found, .. } if found == kind)
            })
            .cloned()
            .collect()
    }

    /// A clip's length is written outside the read, just beyond its cut edge and beyond any
    /// ghost bases drawn there, never over the read body; a hard clip's carries an `H`.
    #[test]
    fn clip_length_labels_sit_outside_the_read_beyond_any_ghosts() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let records = [record(
            b"clipped",
            120,
            140,
            &[
                (CigarOpKind::SoftClip, 5),
                (CigarOpKind::Match, 20),
                (CigarOpKind::HardClip, 3),
            ],
            &[b'A'; 25],
        )];
        // 20 points per base.
        let camera = clip_camera(100, 160, 1_200.0);
        for (lod, ghost_cells) in [(GeometryLod::Read, 0), (GeometryLod::Base, 5)] {
            let scene = scene_for_records(
                &ledger,
                &records,
                lod,
                crate::AlignmentStyleContract::default(),
            );
            let labels = event_labels(&scene, camera, AlignmentLabelKind::ClipLength);
            assert_eq!(labels.len(), 2, "{lod:?} {labels:?}");
            let soft = labels
                .iter()
                .find(|label| label.text.as_deref() == Some("5"))
                .unwrap();
            let hard = labels
                .iter()
                .find(|label| label.text.as_deref() == Some("3H"))
                .unwrap();
            // Leading: the text ends where the drawn ghosts (or, without them, the read) begin.
            let read_left = pane_x(camera, 120);
            let read_right = pane_x(camera, 140);
            assert!(
                (soft.x + soft.width - pane_x(camera, 120 - ghost_cells)).abs() < 1.0e-9,
                "{lod:?} {soft:?}"
            );
            assert!(soft.x + soft.width <= read_left + 1.0e-9);
            // Trailing hard clip: no ghosts, the text starts on the read's right boundary.
            assert!((hard.x - read_right).abs() < 1.0e-9, "{lod:?} {hard:?}");
        }
    }

    /// A clip length never lands on another read. With ghost bases hidden at this zoom it sits
    /// right at the read's clipped end (not past invisible ghosts); with ghosts drawn it sits past
    /// them, unless that place is on the next read packed into the same row, where it is dropped.
    #[test]
    fn clip_length_labels_follow_the_drawn_ghosts_and_never_sit_on_another_read() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let clipped = record(
            b"clipped",
            120,
            140,
            &[(CigarOpKind::Match, 20), (CigarOpKind::SoftClip, 11)],
            &[b'A'; 31],
        );
        // 24 bases starting six bases after the clipped read ends: the same row (spacing 3),
        // and on top of where its eleven ghost bases would be.
        let neighbour = record(b"neighbour", 146, 170, &[(CigarOpKind::Match, 24)], &[b'A'; 24]);
        let style = crate::AlignmentStyleContract::default();
        let alone = scene_for_records(&ledger, &[clipped.clone()], GeometryLod::Base, style);
        let packed = scene_for_records(&ledger, &[clipped, neighbour], GeometryLod::Base, style);

        // 6.5 points per base: base LOD builds the ghosts but they are not drawn (< 7).
        let hidden_ghosts = clip_camera(100, 180, 520.0);
        assert!(!crate::clip_ghost_visible(6.5));
        for scene in [&alone, &packed] {
            let labels = event_labels(scene, hidden_ghosts, AlignmentLabelKind::ClipLength);
            assert_eq!(labels.len(), 1, "{labels:?}");
            assert!(
                (labels[0].x - pane_x(hidden_ghosts, 140)).abs() < 1.0e-9,
                "the count starts at the clipped end, not past hidden ghosts: {:?}",
                labels[0]
            );
        }

        // 12 points per base: ghosts drawn, so the count goes past them (base 151)...
        let drawn_ghosts = clip_camera(100, 160, 720.0);
        let labels = event_labels(&alone, drawn_ghosts, AlignmentLabelKind::ClipLength);
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert!((labels[0].x - pane_x(drawn_ghosts, 151)).abs() < 1.0e-9, "{:?}", labels[0]);
        // ...which is on the neighbouring read, so there the count is dropped.
        let labels = event_labels(&packed, drawn_ghosts, AlignmentLabelKind::ClipLength);
        assert!(labels.is_empty(), "a clip count was drawn on another read: {labels:?}");
    }

    /// Event lengths only appear where they have room. Span events (deletions, skips) need the
    /// text to fit inside their on-screen extent; clips need at least two points per base and
    /// the text to fit in the width their bases would take. An insertion's label (here `+2`, at
    /// least `minimum_event_label_size` long) is a chip beside its I-beam from two points per
    /// base wherever the chip has room (it used to need its bases' width too).
    #[test]
    fn event_length_labels_are_hidden_until_the_text_fits() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let records = [record(
            b"events",
            40,
            175,
            &[
                (CigarOpKind::SoftClip, 5),
                (CigarOpKind::Match, 10),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 10),
                (CigarOpKind::Deletion, 3),
                (CigarOpKind::Match, 10),
                (CigarOpKind::ReferenceSkip, 100),
                (CigarOpKind::Match, 12),
            ],
            &[b'A'; 49],
        )];
        let scene = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Read,
            crate::AlignmentStyleContract::default(),
        );
        // Text is 0.56 x 11 points per glyph plus 2 x 2 points of padding: one digit is 10.16
        // points, three are 22.48.
        let visible = |points_per_base: f64| {
            let width = 300.0 * points_per_base;
            let camera = clip_camera(0, 300, width);
            [
                AlignmentLabelKind::ClipLength,
                AlignmentLabelKind::InsertionLength,
                AlignmentLabelKind::DeletionLength,
                AlignmentLabelKind::SkipLength,
            ]
            .map(|kind| !event_labels(&scene, camera, kind).is_empty())
        };
        // Coarse: the 100-base skip is 5 points long, too short for "100"; nothing shows.
        assert_eq!(visible(0.05), [false, false, false, false]);
        // 0.25 points per base: the skip is 25 points long and fits its "100".
        assert_eq!(visible(0.25), [false, false, false, true]);
        // Below two points per base no point event is labelled, even a long one.
        assert_eq!(visible(1.9), [false, false, false, true]);
        // 2.1: "5" fits in five clipped bases (10.5 points) and the insertion's chip has room;
        // "3" does not fit in the 6.3-point deletion.
        assert_eq!(visible(2.1), [true, true, false, true]);
        // 4: the deletion is 12 points long.
        assert_eq!(visible(4.0), [true, true, true, true]);
        // 6: everything fits.
        assert_eq!(visible(6.0), [true, true, true, true]);
    }

    /// `(text, left, width, row, y, height)` of every insertion label the label projector shows.
    fn insertion_labels(
        scene: &Arc<crate::AlignmentPresentationScene>,
        camera: crate::AlignmentCamera,
    ) -> Vec<(String, f64, f64, u32, f64, f64)> {
        let mut labels = crate::CpuAlignmentProjector::project_labels(scene, 0, camera)
            .unwrap()
            .labels
            .iter()
            .filter(|label| crate::is_insertion_label(label.component))
            .map(|label| {
                (
                    label.text.as_deref().unwrap_or_default().to_owned(),
                    label.x,
                    label.width,
                    label.row,
                    label.y,
                    label.height,
                )
            })
            .collect::<Vec<_>>();
        labels.sort_by(|left, right| left.0.cmp(&right.0));
        labels
    }

    /// A camera over `start..end`, `width` points wide, with rows 14 points apart and reads
    /// `read_height` points tall.
    fn read_height_camera(
        start: u64,
        end: u64,
        width: f64,
        read_height: f64,
    ) -> crate::AlignmentCamera {
        crate::AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), start, end).unwrap(),
            0.0,
            14.0,
            read_height,
            crate::CpuClipRect {
                x: 10.0,
                y: 0.0,
                width,
                height: 140.0,
            },
        )
        .unwrap()
    }

    /// The user's case, `79M1I21M`, in miniature: an insertion shorter than the minimum event
    /// label size (the session default, 10) is labelled with its base (`+T`, from the read's
    /// SEQ) once base letters are drawn; a 12-base one with its length (`+12`), which also shows
    /// zoomed out to two points per base. The label is a chip the read's height tall, one point
    /// after the I-beam (whose bar and serifs reach four points either side at 20 points per
    /// base). Reads under eight points tall carry none.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn insertion_labels_follow_the_zoom_and_minimum_size_rules() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        let mut one = vec![b'A'; 21];
        one[10] = b't';
        let records = [
            record(
                b"one",
                100,
                120,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::Insertion, 1),
                    (CigarOpKind::Match, 10),
                ],
                &one,
            ),
            record(
                b"long",
                100,
                120,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::Insertion, 12),
                    (CigarOpKind::Match, 10),
                ],
                &[b'C'; 32],
            ),
            record(
                b"three",
                100,
                120,
                &[
                    (CigarOpKind::Match, 10),
                    (CigarOpKind::Insertion, 3),
                    (CigarOpKind::Match, 10),
                ],
                b"AAAAAAAAAAACGAAAAAAAAAA",
            ),
        ];
        let style = crate::AlignmentStyleContract {
            minimum_event_label_size: 10,
            ..crate::AlignmentStyleContract::default()
        };
        let scene = scene_for_records(&ledger, &records, GeometryLod::Base, style);
        let texts = |camera| {
            insertion_labels(&scene, camera)
                .into_iter()
                .map(|label| label.0)
                .collect::<Vec<_>>()
        };

        // 20 points per base: base letters are drawn, so every insertion is labelled.
        let detailed = clip_camera(90, 130, 800.0);
        let labels = insertion_labels(&scene, detailed);
        assert_eq!(
            labels
                .iter()
                .map(|label| label.0.as_str())
                .collect::<Vec<_>>(),
            ["+12", "+ACG", "+T"]
        );
        let boundary = pane_x(detailed, 110);
        for (text, left, width, row, y, height) in &labels {
            assert!(
                (left - (boundary + 4.0 + 1.0)).abs() < 1.0e-9,
                "{text}: {left}"
            );
            let glyphs = text.chars().count() as f64;
            assert!(
                (width - (glyphs * 11.0 * 0.56 + 4.0)).abs() < 1.0e-9,
                "{text}"
            );
            // Exactly over its read: the row's top plus the 1.5-point margin, 11 points tall.
            assert!(
                (y - (f64::from(*row) * 14.0 + 1.5)).abs() < 1.0e-9,
                "{text}"
            );
            assert!((height - 11.0).abs() < 1.0e-9, "{text}");
        }
        // Just under the base-letter width (6.9) and down to two points per base only the
        // insertion at least ten bases long is labelled, and only by its length.
        assert_eq!(texts(clip_camera(0, 200, 1_380.0)), ["+12"]);
        assert_eq!(texts(clip_camera(0, 200, 400.0)), ["+12"]);
        // Below two points per base nothing is.
        assert!(texts(clip_camera(0, 200, 380.0)).is_empty());
        // Seven-point reads cannot hold the chip, even at 20 points per base.
        assert!(insertion_labels(&scene, read_height_camera(90, 130, 800.0, 7.0)).is_empty());
        assert_eq!(
            insertion_labels(&scene, read_height_camera(90, 130, 800.0, 8.0)).len(),
            3
        );

        // With the minimum at one, a short insertion is labelled zoomed out too, by its length.
        let every = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                minimum_event_label_size: 1,
                ..crate::AlignmentStyleContract::default()
            },
        );
        let zoomed_out = insertion_labels(&every, clip_camera(0, 200, 800.0));
        assert_eq!(
            zoomed_out
                .iter()
                .map(|label| label.0.as_str())
                .collect::<Vec<_>>(),
            ["+1", "+12", "+3"]
        );

        // An insertion hidden by the minimum indel size has no I-beam, so no label either.
        let fewer = scene_for_records(
            &ledger,
            &records,
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                minimum_indel_size: 2,
                ..style
            },
        );
        assert_eq!(
            insertion_labels(&fewer, detailed)
                .into_iter()
                .map(|label| label.0)
                .collect::<Vec<_>>(),
            ["+12", "+ACG"]
        );
    }

    /// A chip goes after its I-beam, or before it when after would cover another read packed
    /// into the same row, and is dropped when both sides would; it never covers another chip
    /// (the second of two insertions a base apart loses its label) and the base letters it
    /// covers are not drawn over it.
    #[test]
    fn insertion_labels_avoid_other_reads_other_labels_and_hide_covered_letters() {
        let ledger = AllocationLedger::new(16 << 20, 0);
        // At 7 points per base the I-beam reaches 3.5 points either side, and `+ACGT` is
        // 5 x 11 x 0.56 + 4 = 34.8 points wide: 5.6 bases after the gap.
        let short = record(
            b"short",
            110,
            112,
            &[
                (CigarOpKind::Match, 1),
                (CigarOpKind::Insertion, 4),
                (CigarOpKind::Match, 1),
            ],
            b"AACGTA",
        );
        let before = record(b"before", 100, 107, &[(CigarOpKind::Match, 7)], &[b'A'; 7]);
        let after = record(b"after", 115, 130, &[(CigarOpKind::Match, 15)], &[b'A'; 15]);
        let style = crate::AlignmentStyleContract::default();
        let camera = clip_camera(90, 150, 420.0);
        let boundary = pane_x(camera, 111);
        let chip = |records: &[AlignmentRecord]| {
            let scene = scene_for_records(&ledger, records, GeometryLod::Base, style);
            let labels = insertion_labels(&scene, camera);
            assert!(labels.len() <= 1, "{labels:?}");
            labels.first().map(|label| (label.1, label.1 + label.2))
        };
        let (left, _) =
            chip(std::slice::from_ref(&short)).expect("alone, the chip goes after the I-beam");
        assert!((left - (boundary + 3.5 + 1.0)).abs() < 1.0e-9, "{left}");
        let (_, right) = chip(&[short.clone(), after.clone()])
            .expect("with a read after it, the chip goes before the I-beam");
        assert!((right - (boundary - 3.5 - 1.0)).abs() < 1.0e-9, "{right}");
        assert_eq!(chip(&[before, short, after]), None);

        // Two insertions a base apart: the second chip would cover the first.
        let twice = record(
            b"twice",
            100,
            120,
            &[
                (CigarOpKind::Match, 5),
                (CigarOpKind::Insertion, 1),
                (CigarOpKind::Match, 1),
                (CigarOpKind::Insertion, 1),
                (CigarOpKind::Match, 13),
            ],
            b"AAAAATAGAAAAAAAAAAAAA",
        );
        let scene = scene_for_records(&ledger, &[twice], GeometryLod::Base, style);
        let labels = insertion_labels(&scene, camera);
        assert_eq!(labels.len(), 1, "{labels:?}");
        assert_eq!(labels[0].0, "+T");

        // Every base lettered: the chip after the insertion at 110 covers the letter of base
        // 110 (centred 10 points in at 20 points per base), not those of 109 or 111.
        let one = record(
            b"one",
            100,
            120,
            &[
                (CigarOpKind::Match, 10),
                (CigarOpKind::Insertion, 1),
                (CigarOpKind::Match, 10),
            ],
            &[b'A'; 21],
        );
        let scene = scene_for_records(
            &ledger,
            &[one],
            GeometryLod::Base,
            crate::AlignmentStyleContract {
                base_display: ReadBaseDisplayMode::All,
                ..style
            },
        );
        let camera = clip_camera(90, 130, 800.0);
        let lettered = crate::CpuAlignmentProjector::project_labels(&scene, 0, camera)
            .unwrap()
            .labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::Base { .. }))
            .map(|label| ((label.x - 10.0) / 20.0 + 90.0).round() as u64)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(insertion_labels(&scene, camera).len(), 1);
        assert!(
            lettered.contains(&109) && lettered.contains(&111),
            "{lettered:?}"
        );
        assert!(!lettered.contains(&110), "{lettered:?}");
    }

    /// SCENE-9. Pins the in-memory size of one semantic primitive and of its parts, so a field
    /// that widens every primitive is a deliberate decision. The primitive was 184 bytes while
    /// `PairConnector` carried a whole mate handle (which made the component 64 bytes); the
    /// connector now stores only the mate's source position. What remains is dominated by the
    /// 56-byte `AlignmentHandle`; see the design note on that type.
    #[test]
    fn semantic_primitive_size_is_pinned() {
        assert_eq!(size_of::<AlignmentHandle>(), 56);
        assert_eq!(size_of::<rgv_model::AlignmentSourcePosition>(), 24);
        assert_eq!(size_of::<AlignmentStyleToken>(), 32);
        assert_eq!(size_of::<AlignmentComponent>(), 32);
        assert_eq!(size_of::<AlignmentSemanticPrimitive>(), 152);
        // The page ledger charge is derived from the same `size_of`, so it shrank with it.
        assert_eq!(size_of::<SemanticPageHit>(), 96);
    }

    #[test]
    fn a_mismatch_cell_paints_over_an_aligned_block_that_starts_at_the_same_base() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let (evidence, atlas) = evidence_and_atlas(&ledger);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
            bases: vec![b'A'; 300],
        };
        let pages = (0..3)
            .map(|tile| {
                page(
                    &ledger,
                    &evidence,
                    &atlas,
                    tile,
                    GeometryLod::Base,
                    Some(&reference),
                )
            })
            .collect::<Vec<_>>();
        let primitives = pages
            .iter()
            .flat_map(|page| page.primitives().iter().copied())
            .collect::<Vec<_>>();

        // The case that hid the fill: a block and a one-base cell starting at the same base.
        let segment = primitives
            .iter()
            .copied()
            .find(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::AlignedSegment { .. }
                ) && primitive.end_offset > primitive.start_offset + 1
            })
            .expect("the fixture has a multi-base aligned block");
        let mut base = primitives
            .iter()
            .copied()
            .find(|primitive| matches!(primitive.component, AlignmentComponent::Base { .. }))
            .expect("the fixture has base cells at base LOD");
        base.row_offset = segment.row_offset;
        base.start_offset = segment.start_offset;
        base.end_offset = segment.start_offset + 1;
        let mut ordered = vec![base, segment];
        ordered.sort_unstable_by_key(page_paint_key);
        assert!(
            matches!(
                ordered[0].component,
                AlignmentComponent::AlignedSegment { .. }
            ) && matches!(ordered[1].component, AlignmentComponent::Base { .. }),
            "the base cell must paint after (over) the block it sits on"
        );

        // On every real page, within each row, nothing drawn on a read body paints before a
        // body, and boundary marks paint last.
        for page in &pages {
            let mut last_layer_in_row = std::collections::HashMap::<u32, u8>::new();
            for primitive in page.primitives() {
                let layer = primitive_paint_layer(primitive.component, primitive.geometry);
                let previous = last_layer_in_row.insert(primitive.row_offset, layer);
                assert!(
                    previous.is_none_or(|previous| previous <= layer),
                    "row {} paints layer {layer} after layer {:?}",
                    primitive.row_offset,
                    previous
                );
            }
        }
    }
}
