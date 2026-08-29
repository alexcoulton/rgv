use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet},
    hash::{BuildHasherDefault, Hash, Hasher},
    ops::Index,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    BisulfiteContext, CoverageScaleTransform, ReadStackingMode, ReadStackingTransitionReason,
    SbxDisplayOptions, SbxSimplexTailMode, SmrtKineticsMode, UltimaDisplayOptions,
    UltimaQualityMode,
};
use rgv_core::{
    DetailLevel, GeneticCode, GenomicInterval, MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
    ModifiedBaseColorMode, ReadBaseDisplayMode, ReferenceTranslationFrame, Viewport,
};
use rgv_core::{JunctionDisplayMode, SashimiCountMode};
use rgv_hts::{
    AlignedBaseClassification, AlignmentFlags, AlignmentQuery, AlignmentStorageRecordId, AuxValue,
    BisulfiteObservation, CigarOp, CigarOpKind, ImplicitModificationMode, JunctionEvidence,
    JunctionSummary, ModificationBin, ModificationCode, ModificationSummary, PairClassification,
    PairOrientation, PileupBin, PileupSummary, ReferenceSlice, SbxInsertionState, SbxTailBounds,
    SequenceView, SmrtKineticsSeries, TemplateLengthBounds, UltimaEvidenceError,
    UltimaRecordAnalysis, UltimaTagOrientation, aligned_position_for_sequencer_position,
    analyze_ultima_record_with_tag_orientation, bisulfite_context_matches,
    bisulfite_strand_is_reverse, classify_aligned_base, classify_bisulfite_observation,
    classify_sbx_insertion, decode_modified_bases_parts, decode_smrt_kinetics, pair_evidence_at,
    sbx_tail_bounds, sequencer_base_matches, smrt_kinetics_tag,
};

use crate::{
    DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive, junction_arc_stroke_width,
    read_detail_object_id, record_object_id,
};

pub const MAX_HIDDEN_MODIFICATION_CODES: usize = 8;
pub const MAX_RENDERED_JUNCTIONS: usize = 4_096;
pub const MAX_RENDERED_JUNCTION_LABELS: usize = 512;
pub const MAX_RENDERED_BISULFITE_MARKS: usize = 50_000;
pub const MAX_RENDERED_SMRT_KINETICS_MARKS: usize = 50_000;
pub const MAX_RENDERED_SBX_TAIL_MARKS: usize = 50_000;
pub const MAX_RENDERED_ULTIMA_QUALITY_MARKS: usize = 50_000;
pub const MAX_RENDERED_REGIONAL_INSERTIONS: usize = 50_000;
pub const MINIMUM_BASE_GLYPH_PIXEL_WIDTH: f32 = 7.0;
const MAX_EXPANDED_JUNCTION_ROWS: usize = 12;
pub const MAX_ANNOTATED_JUNCTIONS: usize = 100_000;
/// Version two replaces earliest-ending lane rotation with a deterministic
/// topmost-compatible canonical baseline.
pub const ALIGNMENT_ROW_ARCHITECTURE_VERSION: u16 = 3;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IndividualReadRefusalScope {
    Foreground,
    LayoutHorizon,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IndividualReadRefusal {
    pub eligible_records: usize,
    pub limit: usize,
    pub scope: IndividualReadRefusalScope,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JunctionAnnotationStatus {
    Annotated,
    Novel,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JunctionAnnotationLookup {
    annotated: BTreeSet<(u64, u64)>,
    pub source_tracks: usize,
    pub complete_for_interval: bool,
    pub truncated: bool,
}

impl JunctionAnnotationLookup {
    pub fn new(
        junctions: impl IntoIterator<Item = (u64, u64)>,
        source_tracks: usize,
        complete_for_interval: bool,
    ) -> Self {
        let mut annotated = BTreeSet::new();
        let mut truncated = false;
        for junction in junctions {
            if annotated.len() >= MAX_ANNOTATED_JUNCTIONS && !annotated.contains(&junction) {
                truncated = true;
                continue;
            }
            annotated.insert(junction);
        }
        Self {
            annotated,
            source_tracks,
            complete_for_interval: complete_for_interval && !truncated && source_tracks > 0,
            truncated,
        }
    }

    pub fn status(&self, start: u64, end: u64) -> JunctionAnnotationStatus {
        if self.annotated.contains(&(start, end)) {
            JunctionAnnotationStatus::Annotated
        } else if self.complete_for_interval {
            JunctionAnnotationStatus::Novel
        } else {
            JunctionAnnotationStatus::Unknown
        }
    }

    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in self
            .annotated
            .iter()
            .flat_map(|(start, end)| start.to_le_bytes().into_iter().chain(end.to_le_bytes()))
            .chain(self.source_tracks.to_le_bytes())
            .chain([
                u8::from(self.complete_for_interval),
                u8::from(self.truncated),
            ])
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentLayoutOptions {
    pub reference_top: f32,
    pub coverage_top: f32,
    pub coverage_height: f32,
    pub reads_top: f32,
    pub row_height: f32,
    pub read_height: f32,
    pub max_rows: usize,
    /// Quantized horizontal presentation scale. Stable row history and resident row scenes may
    /// be reused only inside this class; decoded reads remain shareable across every class.
    pub row_scale_class: u8,
    /// Half-open stable-row range for which expensive read and picking
    /// primitives should be materialised. Row assignment always covers the
    /// complete admitted query; `None` retains the headless/all-rows default.
    pub materialized_row_range: Option<(usize, usize)>,
    /// Revision of the query-to-stable-row projection consumed by this render.
    /// It is paint-cache metadata and does not participate in row policy.
    pub row_layout_revision: u64,
    pub color_mode: AlignmentColorMode,
    pub group_mode: AlignmentGroupMode,
    pub group_order: AlignmentOrderDirection,
    pub sort_mode: AlignmentSortMode,
    pub sort_order: AlignmentOrderDirection,
    /// Zero-based reference position used by [`AlignmentSortMode::Base`].
    pub sort_position: Option<u64>,
    pub minimum_allele_fraction: f32,
    pub quality_weighted_alleles: bool,
    pub show_reference: bool,
    pub show_coverage: bool,
    pub show_junctions: bool,
    pub show_reads: bool,
    /// Complete-or-refuse state supplied by the query/horizon owner. When
    /// present, aggregate evidence remains visible but no read primitive or
    /// picking target is emitted.
    pub read_refusal: Option<IndividualReadRefusal>,
    pub coverage_scale_maximum: Option<u32>,
    pub coverage_scale_transform: CoverageScaleTransform,
    pub show_coverage_center_line: bool,
    pub junction_top: f32,
    pub junction_height: f32,
    pub minimum_junction_depth: u32,
    pub junction_display_mode: JunctionDisplayMode,
    /// Fixed or workspace-shared maximum in the selected count units. `None` is per-track auto.
    pub junction_scale_maximum: Option<f64>,
    pub junction_count_mode: SashimiCountMode,
    pub show_junction_labels: bool,
    /// Identity of the derived annotation overlay supplied alongside the query.
    pub junction_annotation_fingerprint: u64,
    pub selected_pileup_position: Option<u64>,
    pub read_base_display_mode: ReadBaseDisplayMode,
    pub shade_read_bases_by_quality: bool,
    pub minimum_indel_size: u32,
    pub minimum_event_label_size: u32,
    pub show_modified_bases: bool,
    pub show_modification_frequency: bool,
    pub modification_top: f32,
    pub modification_height: f32,
    pub selected_modification_position: Option<u64>,
    pub minimum_modification_probability: u8,
    pub modified_base_color_mode: ModifiedBaseColorMode,
    pub hidden_modification_codes: [Option<ModificationCode>; MAX_HIDDEN_MODIFICATION_CODES],
    pub selected_chain_read_name_hash: Option<u64>,
    /// Non-persisted selected read/template promoted ahead of the configured sort order.
    pub promoted_read_name_hash: Option<u64>,
    pub view_as_pairs: bool,
    pub pair_length_bounds: TemplateLengthBounds,
}

impl Default for AlignmentLayoutOptions {
    fn default() -> Self {
        Self {
            reference_top: 64.0,
            coverage_top: 92.0,
            coverage_height: 60.0,
            reads_top: 194.0,
            row_height: 7.0,
            read_height: 5.0,
            max_rows: 4_096,
            row_scale_class: 0,
            materialized_row_range: None,
            row_layout_revision: 0,
            color_mode: AlignmentColorMode::Strand,
            group_mode: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sort_mode: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_position: None,
            minimum_allele_fraction: 0.20,
            quality_weighted_alleles: true,
            show_reference: true,
            show_coverage: true,
            show_junctions: true,
            show_reads: true,
            read_refusal: None,
            coverage_scale_maximum: None,
            coverage_scale_transform: CoverageScaleTransform::Linear,
            show_coverage_center_line: false,
            junction_top: 158.0,
            junction_height: 40.0,
            minimum_junction_depth: 1,
            junction_display_mode: JunctionDisplayMode::Collapsed,
            junction_scale_maximum: None,
            junction_count_mode: SashimiCountMode::Raw,
            show_junction_labels: true,
            junction_annotation_fingerprint: 0,
            selected_pileup_position: None,
            read_base_display_mode: ReadBaseDisplayMode::Mismatches,
            shade_read_bases_by_quality: true,
            minimum_indel_size: 1,
            minimum_event_label_size: 1,
            show_modified_bases: true,
            show_modification_frequency: true,
            modification_top: 158.0,
            modification_height: 28.0,
            selected_modification_position: None,
            minimum_modification_probability: 0,
            modified_base_color_mode: ModifiedBaseColorMode::ByType,
            hidden_modification_codes: [None; MAX_HIDDEN_MODIFICATION_CODES],
            selected_chain_read_name_hash: None,
            promoted_read_name_hash: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
        }
    }
}

impl AlignmentLayoutOptions {
    pub fn materializes_row(&self, row: usize) -> bool {
        self.materialized_row_range
            .is_none_or(|(start, end)| start <= row && row < end)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AlignmentLayoutStats {
    pub input_alignments: usize,
    pub rendered_alignments: usize,
    pub hidden_alignments: usize,
    /// Complete admitted alignments whose stable rows are outside the current
    /// vertical materialisation window.
    pub virtualized_alignments: usize,
    pub primitive_count: usize,
    pub maximum_coverage: u32,
    pub rendered_junctions: usize,
    pub hidden_junctions: usize,
    pub maximum_junction_depth: u32,
    pub maximum_junction_value: f64,
    pub junction_normalization_available: bool,
    pub bisulfite_marks_omitted: usize,
    pub smrt_kinetics_marks_omitted: usize,
    pub smrt_kinetics_unavailable_records: usize,
    pub smrt_kinetics_malformed_records: usize,
    pub sbx_tail_marks_omitted: usize,
    pub sbx_unavailable_records: usize,
    pub sbx_unavailable_insertions: usize,
    pub ultima_quality_marks_omitted: usize,
    pub ultima_unavailable_records: usize,
    pub ultima_malformed_records: usize,
    pub ultima_unavailable_events: usize,
    pub regional_insertions_rendered: usize,
    pub regional_insertions_collapsed: usize,
    pub regional_insertions_omitted: usize,
    /// Query-wide stable packing height, including rows that can be empty in
    /// the current horizontal slice.
    pub packed_row_count: usize,
    /// Distinct stable rows occupied by an alignment overlapping the viewport.
    pub visible_occupied_rows: usize,
    /// First-to-last occupied stable-row span in the viewport.
    pub visible_row_span: usize,
    /// Maximum number of alignment spans concurrent in the viewport before
    /// grouping/sorting constraints are applied.
    pub natural_stack_height: usize,
    pub materialized_row_start: usize,
    pub materialized_row_end: usize,
}

/// Strand-specific visual component of one biological splice junction.
///
/// The biological selection remains identified by `(start, end)`. This value
/// preserves which independently rendered line the pointer acquired.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JunctionArcComponent {
    Positive,
    Negative,
    Unknown,
}

/// Exact semantic geometry emitted beside one visible Sashimi arc.
///
/// `primitive` is the same value appended to the display list. Consumers must
/// carry it through composition rather than rebuilding curve geometry from the
/// raw junction query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionArcTarget {
    pub junction_index: usize,
    pub start: u64,
    pub end: u64,
    pub component: JunctionArcComponent,
    pub primitive: RectPrimitive,
    pub stroke_width: f32,
    pub draw_order: u32,
}

#[derive(Clone, Copy, Debug)]
struct BisulfiteRenderBudget {
    remaining: usize,
    omitted: usize,
}

impl BisulfiteRenderBudget {
    const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            omitted: 0,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.omitted = self.omitted.saturating_add(1);
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SmrtKineticsRenderBudget {
    remaining: usize,
    omitted: usize,
}

impl SmrtKineticsRenderBudget {
    const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            omitted: 0,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.omitted = self.omitted.saturating_add(1);
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ReadRenderStats {
    rendered: usize,
    hidden: usize,
    virtualized: usize,
    bisulfite_marks_omitted: usize,
    smrt_kinetics_marks_omitted: usize,
    smrt_kinetics_unavailable_records: usize,
    smrt_kinetics_malformed_records: usize,
    sbx_tail_marks_omitted: usize,
    sbx_unavailable_records: usize,
    sbx_unavailable_insertions: usize,
    ultima_quality_marks_omitted: usize,
    ultima_unavailable_records: usize,
    ultima_malformed_records: usize,
    ultima_unavailable_events: usize,
    regional_insertions_rendered: usize,
    regional_insertions_collapsed: usize,
    regional_insertions_omitted: usize,
}

#[derive(Clone, Copy, Debug)]
struct UltimaRenderBudget {
    remaining: usize,
    omitted: usize,
}

impl UltimaRenderBudget {
    const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            omitted: 0,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.omitted = self.omitted.saturating_add(1);
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SbxRenderBudget {
    remaining: usize,
    omitted: usize,
    unavailable_insertions: usize,
}

impl SbxRenderBudget {
    const fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            omitted: 0,
            unavailable_insertions: 0,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.omitted = self.omitted.saturating_add(1);
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SbxRenderState {
    options: SbxDisplayOptions,
    tails: SbxTailBounds,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlignmentLayout {
    pub display_list: DisplayList,
    /// Every and only rendered Sashimi component, in deterministic draw order.
    pub junction_arc_targets: Vec<JunctionArcTarget>,
    /// Rectangle prefix belonging to the frozen reference and coverage lanes.
    pub pinned_rectangle_count: usize,
    /// Text prefix belonging to the frozen reference and coverage lanes.
    pub pinned_text_count: usize,
    pub stats: AlignmentLayoutStats,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentRowIdentity {
    primary: u64,
    secondary: u64,
    occurrence: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AlignmentRowPolicy {
    row_scale_class: u8,
    group_mode: AlignmentGroupMode,
    group_order: AlignmentOrderDirection,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    view_as_pairs: bool,
    pair_length_bounds: Option<TemplateLengthBounds>,
}

impl AlignmentRowPolicy {
    fn from_options(options: &AlignmentLayoutOptions) -> Self {
        let pair_length_bounds = (options.group_mode == AlignmentGroupMode::UnexpectedPair
            || options.sort_mode == AlignmentSortMode::PairStatus)
            .then_some(options.pair_length_bounds);
        Self {
            row_scale_class: options.row_scale_class,
            group_mode: options.group_mode,
            group_order: options.group_order,
            sort_mode: options.sort_mode,
            sort_order: options.sort_order,
            sort_position: effective_sort_position(options),
            promoted_read_name_hash: options.promoted_read_name_hash,
            view_as_pairs: options.view_as_pairs,
            pair_length_bounds,
        }
    }
}

#[derive(Clone, Debug)]
struct HistoricalRowAssignment {
    group: AlignmentGroupKey,
    row: usize,
    start: u64,
    end: u64,
    last_seen_revision: u64,
}

#[derive(Clone, Debug)]
struct AlignmentRowIsland {
    interval: rgv_core::GenomicInterval,
    horizon_generation: u64,
    horizon_extensions: usize,
    horizon_truncated: bool,
    assignments: HashMap<AlignmentRowIdentity, HistoricalRowAssignment>,
    row_groups: BTreeMap<usize, BTreeSet<AlignmentGroupKey>>,
    last_used_revision: u64,
    navigation_pinned: bool,
}

#[derive(Clone, Debug)]
struct AlignmentRowHistoryContext {
    contig: rgv_core::ContigId,
    policy: AlignmentRowPolicy,
    islands: Vec<AlignmentRowIsland>,
}

/// Persistent, query-independent read-row state for one panel/alignment track.
///
/// Contexts are separated by contig and explicit row policy. Within a context,
/// overlapping query intervals extend a layout island. Alignments absent from
/// the current query remain compact reservations so a later return can restore
/// their original rows.
#[derive(Clone, Debug, Default)]
pub struct AlignmentRowHistory {
    contexts: Vec<AlignmentRowHistoryContext>,
    revision: u64,
    evictions: usize,
}

impl AlignmentRowHistory {
    pub fn clear(&mut self) {
        self.contexts.clear();
        self.revision = self.revision.saturating_add(1);
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }

    pub fn island_count(&self) -> usize {
        self.contexts
            .iter()
            .map(|context| context.islands.len())
            .sum()
    }

    pub fn assignment_count(&self) -> usize {
        self.contexts
            .iter()
            .flat_map(|context| &context.islands)
            .map(|island| island.assignments.len())
            .sum()
    }

    pub fn eviction_count(&self) -> usize {
        self.evictions
    }

    /// Protects islands reachable through the owning panel's bounded
    /// navigation history. Pins are refreshed by the desktop before each
    /// reconciliation and are never applied to record data, only compact row
    /// assignments.
    pub fn pin_navigation_intervals(&mut self, intervals: &[rgv_core::GenomicInterval]) {
        for island in self
            .contexts
            .iter_mut()
            .flat_map(|context| &mut context.islands)
        {
            island.navigation_pinned = intervals.iter().any(|interval| {
                island.interval.contig() == interval.contig()
                    && island.interval.start() < interval.end()
                    && interval.start() < island.interval.end()
            });
        }
    }

    pub fn estimated_bytes(&self) -> usize {
        self.assignment_count()
            .saturating_mul(std::mem::size_of::<(
                AlignmentRowIdentity,
                HistoricalRowAssignment,
            )>())
            .saturating_add(
                self.island_count()
                    .saturating_mul(std::mem::size_of::<AlignmentRowIsland>()),
            )
    }
}

pub const DEFAULT_ALIGNMENT_ROW_HISTORY_BUDGET_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentRowReconciliationStats {
    pub revision: u64,
    pub architecture_version: u16,
    /// Wall-clock time spent reconciling this settled query, reported for
    /// diagnostics only and never used as layout state.
    pub reconciliation_micros: u64,
    /// Time spent projecting identities already present in stable history.
    pub incumbent_projection_micros: u64,
    /// Time spent building row occupancy and compatibility indexes.
    pub history_index_micros: u64,
    /// Time spent choosing rows for newly discovered representatives.
    pub row_allocation_micros: u64,
    /// Time spent merging and committing new compact history intervals.
    pub history_commit_micros: u64,
    /// Time spent comparing the stable layout with a fresh canonical layout.
    pub canonical_metrics_micros: u64,
    /// True when expensive clean-repack comparison was deliberately deferred
    /// for a large live horizon. Correct row assignment is unaffected.
    pub canonical_metrics_deferred: bool,
    pub scene_projection_micros: u64,
    pub scene_hit: bool,
    pub scene_physical_hit: bool,
    pub scene_extended: bool,
    pub exact_camera_hit: bool,
    /// Compatibility-index nodes or fallback rows inspected while assigning
    /// new representatives. This is a complexity regression oracle.
    pub compatibility_probes: usize,
    pub rows_reused: usize,
    pub rows_appended: usize,
    pub retained: usize,
    pub restored: usize,
    pub added: usize,
    pub moved: usize,
    pub reservations: usize,
    pub contexts: usize,
    pub islands: usize,
    pub assignments: usize,
    pub history_bytes: usize,
    pub compact: bool,
    pub repacked: bool,
    pub evictions: usize,
    pub transition_reason: ReadStackingTransitionReason,
    pub policy_fingerprint: u64,
    pub projection_fingerprint: u64,
    pub stable_rows: usize,
    pub canonical_rows: usize,
    pub row_inflation: usize,
    pub canonical_displacement: usize,
    pub top_first_violations: usize,
    pub query_occupied_row_span: usize,
    pub query_empty_rows: usize,
    pub query_empty_ratio_ppm: u32,
    pub horizon_start: u64,
    pub horizon_end: u64,
    pub horizon_generation: u64,
    pub horizon_extensions: usize,
    pub horizon_records: usize,
    pub horizon_bytes: usize,
    pub horizon_truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentRowOrganizationMetrics {
    pub architecture_version: u16,
    pub canonical_rows: usize,
    pub stable_rows: usize,
    pub row_inflation: usize,
    pub canonical_displacement: usize,
    pub top_first_violations: usize,
    pub occupied_row_span: usize,
    pub empty_rows: usize,
    pub empty_ratio_ppm: u32,
}

/// Viewport-independent row assignments for one bounded alignment query.
///
/// Assignments are calculated before viewport clipping so horizontal navigation
/// cannot cause visible reads to be repacked vertically.
#[derive(Debug)]
struct AlignmentRowValues<T> {
    chunks: Arc<[Arc<[T]>]>,
    chunk_ends: Arc<[usize]>,
    len: usize,
}

impl<T> AlignmentRowValues<T> {
    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        let chunk = self.chunk_ends.partition_point(|end| *end <= index);
        let chunk_start = chunk
            .checked_sub(1)
            .map_or(0, |previous| self.chunk_ends[previous]);
        self.chunks
            .get(chunk)
            .and_then(|values| values.get(index.saturating_sub(chunk_start)))
    }

    fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    fn append(&self, values: Vec<T>) -> Self {
        if values.is_empty() {
            return self.clone();
        }
        let mut chunks = self.chunks.to_vec();
        chunks.push(values.into());
        let mut chunk_ends = self.chunk_ends.to_vec();
        let len = self
            .len
            .saturating_add(chunks.last().map_or(0, |chunk| chunk.len()));
        chunk_ends.push(len);
        Self {
            chunks: chunks.into(),
            chunk_ends: chunk_ends.into(),
            len,
        }
    }

    fn add_resident_bytes(&self, residency: &mut AlignmentRowSceneResidency) {
        residency.add_arc_slice(&self.chunks);
        for chunk in self.chunks.iter() {
            residency.add_arc_slice(chunk);
        }
        residency.add_arc_slice(&self.chunk_ends);
    }
}

impl<T> Clone for AlignmentRowValues<T> {
    fn clone(&self) -> Self {
        Self {
            chunks: Arc::clone(&self.chunks),
            chunk_ends: Arc::clone(&self.chunk_ends),
            len: self.len,
        }
    }
}

impl<T: PartialEq> PartialEq for AlignmentRowValues<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for AlignmentRowValues<T> {}

impl<T> From<Vec<T>> for AlignmentRowValues<T> {
    fn from(values: Vec<T>) -> Self {
        let len = values.len();
        if values.is_empty() {
            return Self {
                chunks: Arc::from([]),
                chunk_ends: Arc::from([]),
                len: 0,
            };
        }
        Self {
            chunks: Arc::from([values.into()]),
            chunk_ends: Arc::from([len]),
            len,
        }
    }
}

impl<T> Index<usize> for AlignmentRowValues<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("alignment row index out of bounds")
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentRowLayout {
    rows: AlignmentRowValues<Option<usize>>,
    identities: AlignmentRowValues<AlignmentRowIdentity>,
    physical_indices: Option<Arc<[u32]>>,
    logical_indices_by_physical: Option<Arc<[u32]>>,
    records_by_row: Option<Arc<[Arc<[u32]>]>>,
    pair_representatives: Vec<usize>,
    group_starts: BTreeMap<AlignmentGroupKey, usize>,
    group_mode: AlignmentGroupMode,
    group_order: AlignmentOrderDirection,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    max_rows: usize,
    view_as_pairs: bool,
}

#[derive(Clone, Copy, Debug)]
struct AlignmentRowSceneRecord {
    layout_index: u32,
    identity: AlignmentRowIdentity,
    representative: AlignmentStorageRecordId,
    representative_identity: AlignmentRowIdentity,
}

#[derive(Clone, Copy, Debug)]
struct AlignmentRowSceneOrderedRecord {
    start: u64,
    primary: u64,
    secondary: u64,
    layout_index: u32,
}

#[derive(Clone, Copy, Debug)]
struct AlignmentRowSceneEdgeRecord {
    source_index: usize,
    layout_index: usize,
    storage: AlignmentStorageRecordId,
    identity: AlignmentRowIdentity,
    start: u64,
    end: u64,
}

/// The two halves of a stable alignment identity are already independently
/// mixed fingerprints. Re-hashing them with the standard collision-resistant
/// string hasher costs noticeably more than the lookup itself at ACTB scale.
/// This local hasher avalanches the two machine words while retaining the
/// 128-bit key comparison performed by `HashMap`.
#[derive(Default)]
struct AlignmentIdentityHasher(u64);

impl Hasher for AlignmentIdentityHasher {
    fn finish(&self) -> u64 {
        let mut value = self.0;
        value ^= value >> 30;
        value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value ^= value >> 27;
        value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self
                .0
                .wrapping_mul(0x0000_0100_0000_01b3)
                .wrapping_add(u64::from(*byte));
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = self
            .0
            .rotate_left(27)
            .wrapping_add(value.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    }
}

type AlignmentIdentityMap<V> = HashMap<(u64, u64), V, BuildHasherDefault<AlignmentIdentityHasher>>;

fn alignment_identity_map_with_capacity<V>(capacity: usize) -> AlignmentIdentityMap<V> {
    AlignmentIdentityMap::with_capacity_and_hasher(capacity, BuildHasherDefault::default())
}

#[derive(Clone, Debug)]
enum AlignmentRowSceneStorageRecords {
    DirectDense(Arc<[u32]>),
    DirectSparse(Arc<HashMap<u32, u32>>),
    Dense(Arc<[Option<AlignmentRowSceneRecord>]>),
    Sparse(Arc<HashMap<u32, AlignmentRowSceneRecord>>),
}

impl AlignmentRowSceneStorageRecords {
    fn layout_index(&self, record: u32) -> Option<u32> {
        match self {
            Self::DirectDense(records) => records
                .get(record as usize)
                .copied()
                .filter(|index| *index != u32::MAX),
            Self::DirectSparse(records) => records.get(&record).copied(),
            Self::Dense(records) => records
                .get(record as usize)
                .copied()
                .flatten()
                .map(|record| record.layout_index),
            Self::Sparse(records) => records.get(&record).map(|record| record.layout_index),
        }
    }

    fn get(&self, record: u32) -> Option<AlignmentRowSceneRecord> {
        match self {
            Self::DirectDense(_) | Self::DirectSparse(_) => None,
            Self::Dense(records) => records.get(record as usize).copied().flatten(),
            Self::Sparse(records) => records.get(&record).copied(),
        }
    }

    fn logical_len(&self) -> usize {
        match self {
            Self::DirectDense(records) => records.len(),
            Self::DirectSparse(records) => records
                .keys()
                .copied()
                .max()
                .map_or(0, |record| record as usize + 1),
            Self::Dense(records) => records.len(),
            Self::Sparse(records) => records
                .keys()
                .copied()
                .max()
                .map_or(0, |record| record as usize + 1),
        }
    }

    fn add_resident_bytes(&self, residency: &mut AlignmentRowSceneResidency) {
        match self {
            Self::DirectDense(records) => {
                residency.add_arc_slice(records);
            }
            Self::DirectSparse(records) => {
                if residency.add_arc_sized(records) {
                    residency.bytes = residency.bytes.saturating_add(
                        records
                            .capacity()
                            .saturating_mul(std::mem::size_of::<(u32, u32)>()),
                    );
                }
            }
            Self::Dense(records) => {
                residency.add_arc_slice(records);
            }
            Self::Sparse(records) => {
                if residency.add_arc_sized(records) {
                    residency.bytes = residency.bytes.saturating_add(
                        records
                            .capacity()
                            .saturating_mul(std::mem::size_of::<(u32, AlignmentRowSceneRecord)>()),
                    );
                }
            }
        }
    }
}

#[derive(Default)]
struct AlignmentRowSceneResidency {
    scenes: HashSet<usize>,
    allocations: HashSet<usize>,
    bytes: usize,
}

impl AlignmentRowSceneResidency {
    fn add_scene(&mut self, scene: &AlignmentRowScene) {
        let identity = std::ptr::from_ref(scene) as usize;
        if !self.scenes.insert(identity) {
            return;
        }
        self.bytes = self
            .bytes
            .saturating_add(std::mem::size_of::<AlignmentRowScene>());
        scene.rows.add_resident_bytes(self);
        scene.identities.add_resident_bytes(self);

        if self.add_arc_sized(&scene.records_by_storage) {
            if let Ok(records_by_storage) = scene.records_by_storage.read() {
                self.bytes =
                    self.bytes
                        .saturating_add(records_by_storage.capacity().saturating_mul(
                            std::mem::size_of::<(usize, Arc<[AlignmentRowSceneStorageRecords]>)>(),
                        ));
                for layers in records_by_storage.values() {
                    if self.add_arc_slice(layers) {
                        for layer in layers.iter() {
                            layer.add_resident_bytes(self);
                        }
                    }
                }
            }
        }

        if self.add_arc_slice(&scene.records_by_identity) {
            for layer in scene.records_by_identity.iter() {
                if self.add_arc_sized(layer) {
                    self.bytes = self.bytes.saturating_add(layer.capacity().saturating_mul(
                        std::mem::size_of::<((u64, u64), Vec<AlignmentRowSceneRecord>)>(),
                    ));
                    self.bytes = self.bytes.saturating_add(layer.values().fold(
                        0_usize,
                        |total, records| {
                            total.saturating_add(
                                records
                                    .capacity()
                                    .saturating_mul(std::mem::size_of::<AlignmentRowSceneRecord>()),
                            )
                        },
                    ));
                }
            }
        }

        if self.add_arc_slice(&scene.records_by_order) {
            for layer in scene.records_by_order.iter() {
                self.add_arc_slice(layer);
            }
        }
        if self.add_arc_slice(&scene.records_by_row) {
            for row in scene.records_by_row.iter() {
                self.add_arc_slice(row);
            }
        }
        self.add_arc_slice(&scene.row_min_starts);
        self.add_arc_slice(&scene.row_max_ends);
        self.bytes = self.bytes.saturating_add(
            scene
                .group_starts
                .len()
                .saturating_mul(std::mem::size_of::<(AlignmentGroupKey, usize)>()),
        );
    }

    fn add_arc_sized<T>(&mut self, value: &Arc<T>) -> bool {
        let identity = Arc::as_ptr(value) as usize;
        if !self.allocations.insert(identity) {
            return false;
        }
        self.bytes = self.bytes.saturating_add(std::mem::size_of::<T>());
        true
    }

    fn add_arc_slice<T>(&mut self, value: &Arc<[T]>) -> bool {
        let identity = Arc::as_ptr(value) as *const () as usize;
        if !self.allocations.insert(identity) {
            return false;
        }
        self.bytes = self
            .bytes
            .saturating_add(value.len().saturating_mul(std::mem::size_of::<T>()));
        true
    }
}

/// Persistent row assignments addressed by immutable packed-record identity.
///
/// A contained viewport can project these assignments in one linear pass without rerunning
/// grouping, sorting, collision detection, or history reconciliation.
#[derive(Clone, Debug)]
pub struct AlignmentRowScene {
    interval: GenomicInterval,
    row_scale_class: u8,
    rows: AlignmentRowValues<Option<usize>>,
    identities: AlignmentRowValues<AlignmentRowIdentity>,
    records_by_storage: Arc<RwLock<HashMap<usize, Arc<[AlignmentRowSceneStorageRecords]>>>>,
    /// Immutable identity layers let an edge extension share the incumbent
    /// million-record index and append only its new slab identities.
    records_by_identity: Arc<[Arc<AlignmentIdentityMap<Vec<AlignmentRowSceneRecord>>>]>,
    /// Source-order layers permit independently decoded foreground batches to
    /// be mapped by sequential merge rather than one random hash-table lookup
    /// per read. Edge slabs append a new immutable layer.
    records_by_order: Arc<[Arc<[AlignmentRowSceneOrderedRecord]>]>,
    records_by_row: Arc<[Arc<[u32]>]>,
    row_min_starts: Arc<[u64]>,
    row_max_ends: Arc<[u64]>,
    group_starts: BTreeMap<AlignmentGroupKey, usize>,
    group_mode: AlignmentGroupMode,
    group_order: AlignmentOrderDirection,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    max_rows: usize,
    view_as_pairs: bool,
    pair_length_bounds: TemplateLengthBounds,
    residency_revision: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentRowSceneProjection {
    Physical,
    StableIdentity,
    EdgeExtension,
    ExactCamera,
}

impl AlignmentRowScene {
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    /// Prepares a larger settled resident horizon using the scene's own row
    /// policy, then registers that resident storage for direct projection.
    /// This is intended for speculative worker execution after browsing-buffer
    /// acquisition, before a camera gesture needs the new area.
    pub fn prepare_resident_extension(&self, query: &AlignmentQuery) -> Option<Self> {
        let options = AlignmentLayoutOptions {
            row_scale_class: self.row_scale_class,
            group_mode: self.group_mode,
            group_order: self.group_order,
            sort_mode: self.sort_mode,
            sort_order: self.sort_order,
            sort_position: self.sort_position,
            promoted_read_name_hash: self.promoted_read_name_hash,
            max_rows: self.max_rows,
            view_as_pairs: self.view_as_pairs,
            pair_length_bounds: self.pair_length_bounds,
            ..AlignmentLayoutOptions::default()
        };
        let extended = self.extend_edges(query, &options)?;
        extended.project(query, &options)?;
        Some(extended)
    }

    pub fn new(
        query: &AlignmentQuery,
        layout: &AlignmentRowLayout,
        options: &AlignmentLayoutOptions,
    ) -> Option<Self> {
        if query.batch.len() != layout.len() {
            return None;
        }
        let mut records_by_storage = HashMap::<usize, Vec<Option<AlignmentRowSceneRecord>>>::new();
        let mut records_by_identity = alignment_identity_map_with_capacity(query.batch.len());
        let mut records_by_order = Vec::with_capacity(query.batch.len());
        let row_count = layout.used_row_count();
        let mut row_min_starts = vec![u64::MAX; row_count];
        let mut row_max_ends = vec![0_u64; row_count];
        let mut records_by_row = vec![Vec::<u32>::new(); row_count];
        for index in 0..query.batch.len() {
            let identity = query.batch.storage_record_id(index)?;
            let representative_index = layout.representative(index);
            let representative = query.batch.storage_record_id(representative_index)?;
            let row_identity = layout.identity(index)?;
            let record = AlignmentRowSceneRecord {
                layout_index: u32::try_from(layout.physical_index(index)?).ok()?,
                identity: row_identity,
                representative,
                representative_identity: layout.identity(representative_index)?,
            };
            let records = records_by_storage.entry(identity.storage()).or_default();
            let physical = identity.record() as usize;
            if records.len() <= physical {
                records.resize(physical.saturating_add(1), None);
            }
            records[physical] = Some(record);
            records_by_identity
                .entry((row_identity.primary, row_identity.secondary))
                .or_insert_with(Vec::new)
                .push(record);
            records_by_order.push(AlignmentRowSceneOrderedRecord {
                start: query.batch.start(index)?,
                primary: row_identity.primary,
                secondary: row_identity.secondary,
                layout_index: record.layout_index,
            });
            if let Some(row) = layout.row(index) {
                records_by_row[row].push(record.layout_index);
                row_min_starts[row] =
                    row_min_starts[row].min(query.batch.start(index).expect("validated start"));
                row_max_ends[row] =
                    row_max_ends[row].max(query.batch.end(index).expect("validated end"));
            }
        }
        Some(Self {
            interval: query.interval,
            row_scale_class: options.row_scale_class,
            rows: layout.rows.clone(),
            identities: layout.identities.clone(),
            records_by_storage: Arc::new(RwLock::new(
                records_by_storage
                    .into_iter()
                    .map(|(storage, records)| {
                        (
                            storage,
                            Arc::from([AlignmentRowSceneStorageRecords::Dense(records.into())]),
                        )
                    })
                    .collect(),
            )),
            records_by_identity: Arc::from([Arc::new(records_by_identity)]),
            records_by_order: Arc::from([records_by_order.into()]),
            records_by_row: records_by_row
                .into_iter()
                .map(Arc::<[u32]>::from)
                .collect::<Vec<_>>()
                .into(),
            row_min_starts: row_min_starts.into(),
            row_max_ends: row_max_ends.into(),
            group_starts: layout.group_starts.clone(),
            group_mode: layout.group_mode,
            group_order: layout.group_order,
            sort_mode: layout.sort_mode,
            sort_order: layout.sort_order,
            sort_position: layout.sort_position,
            promoted_read_name_hash: layout.promoted_read_name_hash,
            max_rows: layout.max_rows,
            view_as_pairs: layout.view_as_pairs,
            pair_length_bounds: options.pair_length_bounds,
            residency_revision: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn project(
        &self,
        query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<AlignmentRowLayout> {
        self.project_with_outcome(query, options)
            .map(|(layout, _)| layout)
    }

    pub fn project_with_outcome(
        &self,
        query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<(AlignmentRowLayout, AlignmentRowSceneProjection)> {
        if !self.compatible_with(options) {
            return None;
        }
        self.project_physical_unchecked(query)
            .map(|layout| (layout, AlignmentRowSceneProjection::Physical))
            .or_else(|| {
                self.project_by_order(query, options)
                    .map(|layout| (layout, AlignmentRowSceneProjection::StableIdentity))
            })
            .or_else(|| {
                self.project_by_identity(query, options)
                    .map(|layout| (layout, AlignmentRowSceneProjection::StableIdentity))
            })
    }

    pub fn project_physical(
        &self,
        query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<AlignmentRowLayout> {
        self.compatible_with(options)
            .then(|| self.project_physical_unchecked(query))
            .flatten()
    }

    /// Extends a compatible stable/default row scene using only records from
    /// newly decoded left or right slabs. Incumbent row arrays and immutable
    /// storage indexes stay shared; old records are never regrouped or sorted.
    /// More complex grouping, pairing, and user sort policies retain the
    /// production full-reconciliation fallback.
    pub fn extend_edges(
        &self,
        layout_query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<Self> {
        if !self.compatible_with(options)
            || self.view_as_pairs
            || self.group_mode != AlignmentGroupMode::None
            || self.group_order != AlignmentOrderDirection::Natural
            || self.sort_mode != AlignmentSortMode::Start
            || self.sort_order != AlignmentOrderDirection::Natural
            || self.sort_position.is_some()
            || self.promoted_read_name_hash.is_some()
            || layout_query.interval.contig() != self.interval.contig()
            || layout_query.interval.start() > self.interval.start()
            || layout_query.interval.end() < self.interval.end()
            || layout_query.interval == self.interval
            || !layout_query.individual_reads_complete()
        {
            return None;
        }

        let incumbent_storage = self.records_by_storage.read().ok()?;
        // A production resident extension is a composite of incumbent and
        // newly decoded immutable slabs. In that case an absent physical
        // record is necessarily novel, so consulting the million-entry stable
        // identity map for every edge record is both redundant and expensive.
        // Independently decoded queries share no storage identity and retain
        // the source-independent fallback below.
        let shares_incumbent_storage = layout_query
            .batch
            .storage_ids()
            .into_iter()
            .any(|storage| incumbent_storage.contains_key(&storage));
        let incumbent_record_count = self.rows.len();
        let mut appended_rows = Vec::new();
        let mut appended_identities = Vec::new();
        let mut missing_occurrences = AlignmentIdentityMap::<u32>::default();
        let mut incumbent_occurrences = AlignmentIdentityMap::<usize>::default();
        let mut storage_additions = HashMap::<usize, Vec<(usize, AlignmentRowSceneRecord)>>::new();
        let mut left = Vec::<AlignmentRowSceneEdgeRecord>::new();
        let mut right = Vec::<AlignmentRowSceneEdgeRecord>::new();
        // Complete edge acquisition cannot introduce a genuinely new record
        // whose start is wholly inside the incumbent interval: that record
        // was already enumerable by the old indexed query. Inspect only the
        // coordinate prefixes/suffixes and leave independently decoded middle
        // storage to the ordinary visible-query projection.
        let left_end = layout_query
            .batch
            .partition_point_start(|start| start < self.interval.start());
        let right_start = layout_query
            .batch
            .partition_point_start(|start| start < self.interval.end());
        for index in (0..left_end).chain(right_start..layout_query.batch.len()) {
            let storage = layout_query.batch.storage_record_id(index)?;
            if storage_contains_record(&incumbent_storage, storage) {
                continue;
            }
            let key = scene_identity_key(layout_query, index, false)?;
            if let Some(candidates) = (!shares_incumbent_storage)
                .then(|| {
                    self.records_by_identity
                        .iter()
                        .rev()
                        .find_map(|layer| layer.get(&key))
                })
                .flatten()
            {
                let occurrence = if candidates.len() == 1 {
                    0
                } else {
                    let occurrence = incumbent_occurrences.entry(key).or_default();
                    let current = *occurrence;
                    *occurrence = occurrence.saturating_add(1);
                    current
                };
                if let Some(record) = candidates.get(occurrence).copied() {
                    storage_additions
                        .entry(storage.storage())
                        .or_default()
                        .push((storage.record() as usize, record));
                    continue;
                }
            }
            let start = layout_query.batch.start(index)?;
            let end = layout_query.batch.end(index)?;
            let side = if start < self.interval.start() && end <= self.interval.end() {
                &mut left
            } else if end > self.interval.end() && start >= self.interval.start() {
                &mut right
            } else {
                // A genuinely novel record wholly inside the incumbent scene
                // indicates a changed/incomplete contract, not an edge slab.
                return None;
            };
            let occurrence = missing_occurrences.entry(key).or_default();
            let identity = AlignmentRowIdentity {
                primary: key.0,
                secondary: key.1,
                occurrence: *occurrence,
            };
            *occurrence = occurrence.saturating_add(1);
            let layout_index = incumbent_record_count.saturating_add(appended_rows.len());
            appended_rows.push(None);
            appended_identities.push(identity);
            side.push(AlignmentRowSceneEdgeRecord {
                source_index: index,
                layout_index,
                storage,
                identity,
                start,
                end,
            });
        }
        let maximum_rows = self.max_rows.max(options.max_rows.max(1));
        let mut row_min_starts = self.row_min_starts.to_vec();
        let mut row_max_ends = self.row_max_ends.to_vec();
        assign_right_edge_rows(
            &mut right,
            &mut appended_rows,
            incumbent_record_count,
            &mut row_min_starts,
            &mut row_max_ends,
            maximum_rows,
        );
        if !assign_left_edge_rows(
            &mut left,
            &mut appended_rows,
            incumbent_record_count,
            &mut row_min_starts,
            &mut row_max_ends,
            maximum_rows,
        ) {
            return None;
        }

        let mut records_by_storage = incumbent_storage.clone();
        drop(incumbent_storage);
        let mut identity_layer = AlignmentIdentityMap::<Vec<AlignmentRowSceneRecord>>::default();
        let mut ordered_layer = Vec::with_capacity(left.len().saturating_add(right.len()));
        for edge in left.iter().chain(&right).copied() {
            let record = AlignmentRowSceneRecord {
                layout_index: u32::try_from(edge.layout_index).ok()?,
                identity: edge.identity,
                representative: edge.storage,
                representative_identity: edge.identity,
            };
            storage_additions
                .entry(edge.storage.storage())
                .or_default()
                .push((edge.storage.record() as usize, record));
            identity_layer
                .entry((edge.identity.primary, edge.identity.secondary))
                .or_default()
                .push(record);
            ordered_layer.push((
                edge.source_index,
                AlignmentRowSceneOrderedRecord {
                    start: edge.start,
                    primary: edge.identity.primary,
                    secondary: edge.identity.secondary,
                    layout_index: record.layout_index,
                },
            ));
        }
        for (storage, additions) in storage_additions {
            let mut layers = records_by_storage
                .get(&storage)
                .map_or_else(Vec::new, |layers| layers.to_vec());
            layers.push(alignment_row_storage_records(additions));
            records_by_storage.insert(storage, layers.into());
        }
        let mut identity_layers = self.records_by_identity.to_vec();
        identity_layers.push(Arc::new(identity_layer));
        ordered_layer.sort_unstable_by_key(|(source_index, _)| *source_index);
        let mut order_layers = self.records_by_order.to_vec();
        if !ordered_layer.is_empty() {
            order_layers.push(
                ordered_layer
                    .into_iter()
                    .map(|(_, record)| record)
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
        let extended_rows = self.rows.append(appended_rows);
        let mut records_by_row = self.records_by_row.to_vec();
        if records_by_row.len() < row_min_starts.len() {
            records_by_row.resize_with(row_min_starts.len(), || Arc::from([]));
        }
        let mut additions_by_row = HashMap::<usize, Vec<u32>>::new();
        for edge in left.iter().chain(&right) {
            if let Some(row) = extended_rows[edge.layout_index] {
                additions_by_row
                    .entry(row)
                    .or_default()
                    .push(edge.layout_index as u32);
            }
        }
        for (row, additions) in additions_by_row {
            let mut records = records_by_row[row].to_vec();
            records.extend(additions);
            records.sort_unstable();
            records_by_row[row] = records.into();
        }
        let row_capacity = row_min_starts.len().max(1);
        Some(Self {
            interval: layout_query.interval,
            row_scale_class: self.row_scale_class,
            rows: extended_rows,
            identities: self.identities.append(appended_identities),
            records_by_storage: Arc::new(RwLock::new(records_by_storage)),
            records_by_identity: identity_layers.into(),
            records_by_order: order_layers.into(),
            records_by_row: records_by_row.into(),
            row_min_starts: row_min_starts.into(),
            row_max_ends: row_max_ends.into(),
            group_starts: self.group_starts.clone(),
            group_mode: self.group_mode,
            group_order: self.group_order,
            sort_mode: self.sort_mode,
            sort_order: self.sort_order,
            sort_position: self.sort_position,
            promoted_read_name_hash: self.promoted_read_name_hash,
            max_rows: row_capacity,
            view_as_pairs: false,
            pair_length_bounds: self.pair_length_bounds,
            residency_revision: Arc::new(AtomicU64::new(1)),
        })
    }

    fn project_physical_unchecked(&self, query: &AlignmentQuery) -> Option<AlignmentRowLayout> {
        let records_by_storage = self.records_by_storage.read().ok()?;
        if !self.view_as_pairs {
            let mut physical_indices = Vec::with_capacity(query.batch.len());
            let mut cached_storage = None::<usize>;
            let mut cached_layers = None::<&[AlignmentRowSceneStorageRecords]>;
            for index in 0..query.batch.len() {
                let identity = query.batch.storage_record_id(index)?;
                if cached_storage != Some(identity.storage()) {
                    cached_storage = Some(identity.storage());
                    cached_layers = Some(records_by_storage.get(&identity.storage())?.as_ref());
                }
                let scene_record = cached_layers?
                    .iter()
                    .rev()
                    .find_map(|records| records.layout_index(identity.record()))?;
                physical_indices.push(scene_record);
            }
            return Some(AlignmentRowLayout {
                rows: self.rows.clone(),
                identities: self.identities.clone(),
                logical_indices_by_physical: Some(
                    logical_indices_by_physical(self.rows.len(), &physical_indices).into(),
                ),
                physical_indices: Some(physical_indices.into()),
                records_by_row: Some(Arc::clone(&self.records_by_row)),
                pair_representatives: Vec::new(),
                group_starts: self.group_starts.clone(),
                group_mode: self.group_mode,
                group_order: self.group_order,
                sort_mode: self.sort_mode,
                sort_order: self.sort_order,
                sort_position: self.sort_position,
                promoted_read_name_hash: self.promoted_read_name_hash,
                max_rows: self.max_rows,
                view_as_pairs: false,
            });
        }
        let mut logical_by_storage = HashMap::<usize, Vec<usize>>::new();
        for (storage, layers) in records_by_storage.iter() {
            for records in layers.iter() {
                let logical = logical_by_storage.entry(*storage).or_default();
                if logical.len() < records.logical_len() {
                    logical.resize(records.logical_len(), usize::MAX);
                }
            }
        }
        let mut physical_indices = Vec::with_capacity(query.batch.len());
        let mut scene_records = Vec::with_capacity(query.batch.len());
        let mut cached_storage = None::<usize>;
        let mut cached_layers = None::<&[AlignmentRowSceneStorageRecords]>;
        for index in 0..query.batch.len() {
            let identity = query.batch.storage_record_id(index)?;
            if cached_storage != Some(identity.storage()) {
                cached_storage = Some(identity.storage());
                cached_layers = Some(records_by_storage.get(&identity.storage())?.as_ref());
            }
            let scene_record = cached_layers?
                .iter()
                .rev()
                .find_map(|records| records.get(identity.record()))?;
            let logical = logical_by_storage.get_mut(&identity.storage())?;
            *logical.get_mut(identity.record() as usize)? = index;
            physical_indices.push(scene_record.layout_index);
            scene_records.push(scene_record);
        }
        let pair_representatives = scene_records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                logical_by_storage
                    .get(&record.representative.storage())
                    .and_then(|logical| logical.get(record.representative.record() as usize))
                    .copied()
                    .filter(|representative| *representative != usize::MAX)
                    .unwrap_or(index)
            })
            .collect();
        Some(AlignmentRowLayout {
            rows: self.rows.clone(),
            identities: self.identities.clone(),
            logical_indices_by_physical: Some(
                logical_indices_by_physical(self.rows.len(), &physical_indices).into(),
            ),
            physical_indices: Some(physical_indices.into()),
            records_by_row: Some(Arc::clone(&self.records_by_row)),
            pair_representatives,
            group_starts: self.group_starts.clone(),
            group_mode: self.group_mode,
            group_order: self.group_order,
            sort_mode: self.sort_mode,
            sort_order: self.sort_order,
            sort_position: self.sort_position,
            promoted_read_name_hash: self.promoted_read_name_hash,
            max_rows: self.max_rows,
            view_as_pairs: self.view_as_pairs,
        })
    }

    fn project_by_identity(
        &self,
        query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<AlignmentRowLayout> {
        let mut duplicate_occurrences = AlignmentIdentityMap::<usize>::default();
        let mut physical_indices = Vec::with_capacity(query.batch.len());
        let mut scene_records = Vec::with_capacity(query.batch.len());
        for index in 0..query.batch.len() {
            let key = scene_identity_key(query, index, options.view_as_pairs)?;
            let candidates = self
                .records_by_identity
                .iter()
                .rev()
                .find_map(|layer| layer.get(&key))?;
            let occurrence = if candidates.len() == 1 {
                0
            } else {
                let occurrence = duplicate_occurrences.entry(key).or_default();
                let current = *occurrence;
                *occurrence = occurrence.saturating_add(1);
                current
            };
            let record = *candidates.get(occurrence)?;
            physical_indices.push(record.layout_index);
            scene_records.push(record);
        }
        self.register_storage_projection(query, &scene_records)?;
        let pair_representatives = if options.view_as_pairs {
            let logical_by_identity = scene_records
                .iter()
                .enumerate()
                .map(|(index, record)| (record.identity, index))
                .collect::<HashMap<_, _>>();
            scene_records
                .iter()
                .enumerate()
                .map(|(index, record)| {
                    logical_by_identity
                        .get(&record.representative_identity)
                        .copied()
                        .unwrap_or(index)
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(AlignmentRowLayout {
            rows: self.rows.clone(),
            identities: self.identities.clone(),
            logical_indices_by_physical: Some(
                logical_indices_by_physical(self.rows.len(), &physical_indices).into(),
            ),
            physical_indices: Some(physical_indices.into()),
            records_by_row: Some(Arc::clone(&self.records_by_row)),
            pair_representatives,
            group_starts: self.group_starts.clone(),
            group_mode: self.group_mode,
            group_order: self.group_order,
            sort_mode: self.sort_mode,
            sort_order: self.sort_order,
            sort_position: self.sort_position,
            promoted_read_name_hash: self.promoted_read_name_hash,
            max_rows: self.max_rows,
            view_as_pairs: self.view_as_pairs,
        })
    }

    /// Maps an independently decoded, coordinate-sorted batch to immutable
    /// row identities by merging each source-order slab sequentially. A query
    /// is a subsequence of each resident layer, so this replaces hundreds of
    /// thousands of cache-unfriendly global hash lookups with linear scans.
    /// Unusual reordered inputs fall through to the authoritative identity
    /// index.
    fn project_by_order(
        &self,
        query: &AlignmentQuery,
        options: &AlignmentLayoutOptions,
    ) -> Option<AlignmentRowLayout> {
        if self.view_as_pairs || options.view_as_pairs || self.records_by_order.len() < 2 {
            return None;
        }
        let mut layout_indices = vec![u32::MAX; query.batch.len()];
        for layer in self.records_by_order.iter() {
            let mut query_index = 0_usize;
            let mut layer_index = 0_usize;
            while query_index < query.batch.len() && layer_index < layer.len() {
                let query_start = query.batch.start(query_index)?;
                let layer_start = layer[layer_index].start;
                if query_start < layer_start {
                    query_index = query
                        .batch
                        .partition_point_start(|start| start <= query_start);
                    continue;
                }
                if layer_start < query_start {
                    layer_index = layer[layer_index..]
                        .partition_point(|record| record.start <= layer_start)
                        .saturating_add(layer_index);
                    continue;
                }

                let query_group_end = query
                    .batch
                    .partition_point_start(|start| start <= query_start);
                let layer_group_end = layer[layer_index..]
                    .partition_point(|record| record.start <= layer_start)
                    .saturating_add(layer_index);
                let mut candidate = layer_index;
                for index in query_index..query_group_end {
                    if candidate >= layer_group_end {
                        break;
                    }
                    let key = scene_identity_key(query, index, false)?;
                    let record = layer[candidate];
                    if key == (record.primary, record.secondary) {
                        if layout_indices[index] != u32::MAX
                            && layout_indices[index] != record.layout_index
                        {
                            return None;
                        }
                        layout_indices[index] = record.layout_index;
                        candidate = candidate.saturating_add(1);
                    }
                }
                query_index = query_group_end;
                layer_index = layer_group_end;
            }
        }
        if layout_indices.iter().any(|index| *index == u32::MAX) {
            return None;
        }
        self.register_direct_storage_projection(query, &layout_indices)?;
        Some(AlignmentRowLayout {
            rows: self.rows.clone(),
            identities: self.identities.clone(),
            logical_indices_by_physical: Some(
                logical_indices_by_physical(self.rows.len(), &layout_indices).into(),
            ),
            physical_indices: Some(layout_indices.into()),
            records_by_row: Some(Arc::clone(&self.records_by_row)),
            pair_representatives: Vec::new(),
            group_starts: self.group_starts.clone(),
            group_mode: self.group_mode,
            group_order: self.group_order,
            sort_mode: self.sort_mode,
            sort_order: self.sort_order,
            sort_position: self.sort_position,
            promoted_read_name_hash: self.promoted_read_name_hash,
            max_rows: self.max_rows,
            view_as_pairs: false,
        })
    }

    pub fn estimated_bytes(&self) -> usize {
        Self::estimated_resident_bytes(std::iter::once(self))
    }

    pub fn residency_revision(&self) -> u64 {
        self.residency_revision.load(Ordering::Relaxed)
    }

    /// Counts live allocations once across persistent scene revisions. Edge extensions share
    /// their immutable row, identity, storage, order, and inverted-row chunks with the incumbent;
    /// charging every revision independently would evict decoded browsing buffers that still fit.
    pub fn estimated_resident_bytes<'a>(scenes: impl IntoIterator<Item = &'a Self>) -> usize {
        let mut residency = AlignmentRowSceneResidency::default();
        for scene in scenes {
            residency.add_scene(scene);
        }
        residency.bytes
    }

    fn register_storage_projection(
        &self,
        query: &AlignmentQuery,
        records: &[AlignmentRowSceneRecord],
    ) -> Option<()> {
        if query.batch.len() != records.len() {
            return None;
        }
        let mut additions = HashMap::<usize, Vec<(usize, AlignmentRowSceneRecord)>>::new();
        for (index, record) in records.iter().copied().enumerate() {
            let identity = query.batch.storage_record_id(index)?;
            additions
                .entry(identity.storage())
                .or_default()
                .push((identity.record() as usize, record));
        }
        let mut records_by_storage = self.records_by_storage.write().ok()?;
        for (storage, additions) in additions {
            let mut layers = records_by_storage
                .get(&storage)
                .map_or_else(Vec::new, |layers| layers.to_vec());
            layers.push(alignment_row_storage_records(additions));
            records_by_storage.insert(storage, layers.into());
        }
        self.residency_revision.fetch_add(1, Ordering::Relaxed);
        Some(())
    }

    fn register_direct_storage_projection(
        &self,
        query: &AlignmentQuery,
        layout_indices: &[u32],
    ) -> Option<()> {
        if query.batch.len() != layout_indices.len() {
            return None;
        }
        let storage_ids = query.batch.storage_ids();
        let mut records_by_storage = self.records_by_storage.write().ok()?;
        if let [storage] = storage_ids.as_slice() {
            let mut direct = Vec::<u32>::new();
            for (index, layout_index) in layout_indices.iter().copied().enumerate() {
                let identity = query.batch.storage_record_id(index)?;
                if identity.storage() != *storage {
                    return None;
                }
                let physical = identity.record() as usize;
                if direct.len() <= physical {
                    direct.resize(physical.saturating_add(1), u32::MAX);
                }
                direct[physical] = layout_index;
            }
            let mut layers = records_by_storage
                .get(storage)
                .map_or_else(Vec::new, |layers| layers.to_vec());
            layers.push(AlignmentRowSceneStorageRecords::DirectDense(direct.into()));
            records_by_storage.insert(*storage, layers.into());
            self.residency_revision.fetch_add(1, Ordering::Relaxed);
            return Some(());
        }

        let mut additions = HashMap::<usize, Vec<(usize, u32)>>::new();
        for (index, layout_index) in layout_indices.iter().copied().enumerate() {
            let identity = query.batch.storage_record_id(index)?;
            additions
                .entry(identity.storage())
                .or_default()
                .push((identity.record() as usize, layout_index));
        }
        for (storage, additions) in additions {
            let mut layers = records_by_storage
                .get(&storage)
                .map_or_else(Vec::new, |layers| layers.to_vec());
            layers.push(alignment_row_direct_storage_records(additions));
            records_by_storage.insert(storage, layers.into());
        }
        self.residency_revision.fetch_add(1, Ordering::Relaxed);
        Some(())
    }

    fn compatible_with(&self, options: &AlignmentLayoutOptions) -> bool {
        self.row_scale_class == options.row_scale_class
            && self.group_mode == options.group_mode
            && self.group_order == options.group_order
            && self.sort_mode == options.sort_mode
            && self.sort_order == options.sort_order
            && self.sort_position == effective_sort_position(options)
            && self.promoted_read_name_hash == options.promoted_read_name_hash
            && self.view_as_pairs == options.view_as_pairs
            && ((self.group_mode != AlignmentGroupMode::UnexpectedPair
                && self.sort_mode != AlignmentSortMode::PairStatus)
                || self.pair_length_bounds == options.pair_length_bounds)
            && (self.max_rows == options.max_rows.max(1)
                || (self.max_rows >= 1 && options.max_rows >= self.max_rows))
    }
}

fn alignment_row_storage_records(
    additions: Vec<(usize, AlignmentRowSceneRecord)>,
) -> AlignmentRowSceneStorageRecords {
    let required = additions
        .iter()
        .map(|(physical, _)| physical.saturating_add(1))
        .max()
        .unwrap_or_default();
    if additions.len().saturating_mul(4) >= required {
        let mut records = vec![None; required];
        for (physical, record) in additions {
            records[physical] = Some(record);
        }
        AlignmentRowSceneStorageRecords::Dense(records.into())
    } else {
        AlignmentRowSceneStorageRecords::Sparse(Arc::new(
            additions
                .into_iter()
                .map(|(physical, record)| (physical as u32, record))
                .collect(),
        ))
    }
}

fn alignment_row_direct_storage_records(
    additions: Vec<(usize, u32)>,
) -> AlignmentRowSceneStorageRecords {
    let required = additions
        .iter()
        .map(|(physical, _)| physical.saturating_add(1))
        .max()
        .unwrap_or_default();
    if additions.len().saturating_mul(4) >= required {
        let mut records = vec![u32::MAX; required];
        for (physical, layout_index) in additions {
            records[physical] = layout_index;
        }
        AlignmentRowSceneStorageRecords::DirectDense(records.into())
    } else {
        AlignmentRowSceneStorageRecords::DirectSparse(Arc::new(
            additions
                .into_iter()
                .map(|(physical, layout_index)| (physical as u32, layout_index))
                .collect(),
        ))
    }
}

fn storage_contains_record(
    records_by_storage: &HashMap<usize, Arc<[AlignmentRowSceneStorageRecords]>>,
    identity: AlignmentStorageRecordId,
) -> bool {
    records_by_storage
        .get(&identity.storage())
        .is_some_and(|layers| {
            layers
                .iter()
                .rev()
                .any(|records| records.layout_index(identity.record()).is_some())
        })
}

fn logical_indices_by_physical(record_count: usize, physical_indices: &[u32]) -> Vec<u32> {
    let mut logical = vec![u32::MAX; record_count];
    for (logical_index, physical_index) in physical_indices.iter().copied().enumerate() {
        if let Some(slot) = logical.get_mut(physical_index as usize) {
            *slot = u32::try_from(logical_index).unwrap_or(u32::MAX);
        }
    }
    logical
}

fn assign_right_edge_rows(
    records: &mut [AlignmentRowSceneEdgeRecord],
    rows: &mut [Option<usize>],
    first_layout_index: usize,
    row_min_starts: &mut Vec<u64>,
    row_max_ends: &mut Vec<u64>,
    maximum_rows: usize,
) {
    records.sort_unstable_by_key(|record| (record.start, record.end, record.identity));
    let mut active_by_end = row_max_ends
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, end)| *end > 0)
        .map(|(row, end)| Reverse((end, row)))
        .collect::<BinaryHeap<_>>();
    let mut available_rows = BinaryHeap::<Reverse<usize>>::new();
    for record in records {
        while active_by_end
            .peek()
            .is_some_and(|Reverse((end, _))| *end <= record.start)
        {
            let Reverse((_, row)) = active_by_end
                .pop()
                .expect("a compatible right-edge row was observed");
            available_rows.push(Reverse(row));
        }
        let row = if let Some(Reverse(row)) = available_rows.pop() {
            row
        } else if row_max_ends.len() < maximum_rows {
            row_min_starts.push(u64::MAX);
            row_max_ends.push(0);
            row_max_ends.len() - 1
        } else {
            continue;
        };
        rows[record.layout_index.saturating_sub(first_layout_index)] = Some(row);
        row_min_starts[row] = row_min_starts[row].min(record.start);
        row_max_ends[row] = row_max_ends[row].max(record.end);
        active_by_end.push(Reverse((row_max_ends[row], row)));
    }
}

fn assign_left_edge_rows(
    records: &mut [AlignmentRowSceneEdgeRecord],
    rows: &mut [Option<usize>],
    first_layout_index: usize,
    row_min_starts: &mut Vec<u64>,
    row_max_ends: &mut Vec<u64>,
    maximum_rows: usize,
) -> bool {
    // A left extension can equal a clean canonical start sweep without moving
    // incumbent rows only when its chosen rows finish before those rows' first
    // incumbent records. Build that canonical prefix in forward order, then
    // reject the fast path if the old boundary would require recolouring. The
    // caller will use the authoritative history reconciliation in that rare
    // case rather than silently committing navigation-order-dependent rows.
    records.sort_unstable_by_key(|record| (record.start, record.end, record.identity));
    let incumbent_min_starts = row_min_starts.clone();
    let mut candidate_rows = rows.to_vec();
    let mut candidate_min_starts = row_min_starts.clone();
    let mut candidate_max_ends = row_max_ends.clone();
    let mut active_by_end = BinaryHeap::<Reverse<(u64, usize)>>::new();
    let mut available_rows = BinaryHeap::<Reverse<usize>>::new();
    let mut allocated = 0_usize;
    for record in records {
        while active_by_end
            .peek()
            .is_some_and(|Reverse((end, _))| *end <= record.start)
        {
            let Reverse((_, row)) = active_by_end
                .pop()
                .expect("a reusable canonical left-edge row was observed");
            available_rows.push(Reverse(row));
        }
        let row = if let Some(Reverse(row)) = available_rows.pop() {
            row
        } else if allocated < maximum_rows {
            let row = allocated;
            allocated = allocated.saturating_add(1);
            row
        } else {
            continue;
        };
        if incumbent_min_starts
            .get(row)
            .is_some_and(|incumbent_start| *incumbent_start < record.end)
        {
            return false;
        }
        if candidate_min_starts.len() <= row {
            candidate_min_starts.resize(row.saturating_add(1), u64::MAX);
            candidate_max_ends.resize(row.saturating_add(1), 0);
        }
        candidate_rows[record.layout_index.saturating_sub(first_layout_index)] = Some(row);
        candidate_min_starts[row] = candidate_min_starts[row].min(record.start);
        candidate_max_ends[row] = candidate_max_ends[row].max(record.end);
        active_by_end.push(Reverse((record.end, row)));
    }
    rows.copy_from_slice(&candidate_rows);
    *row_min_starts = candidate_min_starts;
    *row_max_ends = candidate_max_ends;
    true
}

fn scene_identity_key(
    query: &AlignmentQuery,
    index: usize,
    view_as_pairs: bool,
) -> Option<(u64, u64)> {
    let flags = query.batch.flags(index)?;
    let name = query.batch.read_name(index).unwrap_or_default();
    if view_as_pairs
        && flags.is_paired()
        && !flags.is_secondary()
        && !flags.is_supplementary()
        && !name.is_empty()
    {
        return Some((
            rows::fnv_mix(0xcbf2_9ce4_8422_2325_u64, name),
            rows::fnv_mix(0x6c62_272e_07bb_0142_u64, name),
        ));
    }
    let stable = query.batch.stable_id(index)?;
    Some((stable.primary(), stable.secondary()))
}

impl AlignmentRowLayout {
    pub fn len(&self) -> usize {
        self.physical_indices
            .as_ref()
            .map_or(self.rows.len(), |indices| indices.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn row(&self, index: usize) -> Option<usize> {
        self.rows
            .get(self.physical_index(index)?)
            .copied()
            .flatten()
    }

    pub fn identity(&self, index: usize) -> Option<AlignmentRowIdentity> {
        self.identities.get(self.physical_index(index)?).copied()
    }

    pub fn row_for_identity(&self, identity: AlignmentRowIdentity) -> Option<usize> {
        (0..self.len())
            .find(|index| self.identity(*index) == Some(identity))
            .and_then(|index| self.row(index))
    }

    pub fn used_row_count(&self) -> usize {
        (0..self.len())
            .filter_map(|index| self.row(index))
            .max()
            .map_or(0, |row| row.saturating_add(1))
    }

    /// Stable row-space extent, including rows currently occupied only by
    /// historical reservations.
    pub fn row_capacity(&self) -> usize {
        self.max_rows
    }

    /// Query-local record indices for the requested materialized row range,
    /// selected through the persistent scene's inverted row index. `None`
    /// means this layout came from a compatibility path without a scene index.
    pub fn materialized_record_indices(
        &self,
        row_range: Option<(usize, usize)>,
    ) -> Option<Vec<usize>> {
        let records_by_row = self.records_by_row.as_ref()?;
        let logical = self.logical_indices_by_physical.as_ref()?;
        let (start, end) = row_range.unwrap_or((0, records_by_row.len()));
        let mut indices = records_by_row
            .get(start.min(records_by_row.len())..end.min(records_by_row.len()))?
            .iter()
            .flat_map(|records| records.iter().copied())
            .filter_map(|physical| {
                logical
                    .get(physical as usize)
                    .copied()
                    .filter(|index| *index != u32::MAX)
                    .map(|index| index as usize)
            })
            .collect::<Vec<_>>();
        indices.sort_unstable();
        Some(indices)
    }

    /// Contiguous row ranges with approximately equal query-local record
    /// populations. Equal-height slices are badly imbalanced at dense RNA
    /// loci because top rows contain many more non-overlapping records.
    pub fn balanced_materialized_row_ranges(
        &self,
        row_range: (usize, usize),
        maximum_partitions: usize,
    ) -> Option<Vec<(usize, usize)>> {
        let records_by_row = self.records_by_row.as_ref()?;
        let logical = self.logical_indices_by_physical.as_ref()?;
        let start = row_range.0.min(records_by_row.len());
        let end = row_range.1.min(records_by_row.len()).max(start);
        let counts = records_by_row[start..end]
            .iter()
            .map(|records| {
                records
                    .iter()
                    .filter(|physical| {
                        logical
                            .get(**physical as usize)
                            .is_some_and(|index| *index != u32::MAX)
                    })
                    .count()
            })
            .collect::<Vec<_>>();
        let total = counts.iter().sum::<usize>();
        let partition_count = maximum_partitions.min(counts.len()).min(total.max(1));
        if partition_count == 0 {
            return Some(Vec::new());
        }
        let mut ranges = Vec::with_capacity(partition_count);
        let mut first = start;
        let mut cumulative = 0_usize;
        for (offset, count) in counts.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count);
            let row_end = start.saturating_add(offset).saturating_add(1);
            let next_target =
                total.saturating_mul(ranges.len().saturating_add(1)) / partition_count;
            if ranges.len().saturating_add(1) < partition_count
                && cumulative >= next_target
                && row_end > first
            {
                ranges.push((first, row_end));
                first = row_end;
            }
        }
        if first < end || ranges.is_empty() {
            ranges.push((first, end));
        }
        Some(ranges)
    }

    /// Packed and query-occupied row metrics from the scene's inverted index.
    /// This avoids full record sweeps merely to populate diagnostics.
    pub fn indexed_row_metrics(&self) -> Option<(usize, usize, usize)> {
        let records_by_row = self.records_by_row.as_ref()?;
        let logical = self.logical_indices_by_physical.as_ref()?;
        let mut occupied = 0_usize;
        let mut first = None;
        let mut last = None;
        for (row, records) in records_by_row.iter().enumerate() {
            if records.iter().any(|physical| {
                logical
                    .get(*physical as usize)
                    .is_some_and(|index| *index != u32::MAX)
            }) {
                occupied = occupied.saturating_add(1);
                first.get_or_insert(row);
                last = Some(row);
            }
        }
        let span = first.zip(last).map_or(0, |(first, last)| {
            last.saturating_sub(first).saturating_add(1)
        });
        Some((records_by_row.len(), occupied, span))
    }

    /// Order-independent digest of representative identity-to-row mappings.
    /// It is intended for qualification and diagnostics, not persistence.
    pub fn assignment_fingerprint(&self) -> u64 {
        let mut assignments = self
            .identity_rows()
            .enumerate()
            .filter(|(index, _)| self.representative(*index) == *index)
            .filter_map(|(_, (identity, row))| row.map(|row| (identity, row)))
            .collect::<Vec<_>>();
        assignments.sort_unstable();
        assignments
            .into_iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, (identity, row)| {
                let hash = rows::fnv_mix(hash, &identity.primary.to_le_bytes());
                let hash = rows::fnv_mix(hash, &identity.secondary.to_le_bytes());
                let hash = rows::fnv_mix(hash, &identity.occurrence.to_le_bytes());
                rows::fnv_mix(hash, &row.to_le_bytes())
            })
    }

    fn compatible_with(&self, options: &AlignmentLayoutOptions, record_count: usize) -> bool {
        let next_max_rows = options.max_rows.max(1);
        let capacity_is_compatible = self.max_rows == next_max_rows
            || (self.max_rows >= self.len().max(1) && next_max_rows >= record_count.max(1));
        self.group_mode == options.group_mode
            && self.group_order == options.group_order
            && self.sort_mode == options.sort_mode
            && self.sort_order == options.sort_order
            && self.sort_position == effective_sort_position(options)
            && self.promoted_read_name_hash == options.promoted_read_name_hash
            && capacity_is_compatible
            && self.view_as_pairs == options.view_as_pairs
    }

    fn representative(&self, index: usize) -> usize {
        if !self.view_as_pairs {
            return index;
        }
        self.pair_representatives
            .get(index)
            .copied()
            .unwrap_or(index)
    }

    fn physical_index(&self, index: usize) -> Option<usize> {
        self.physical_indices.as_ref().map_or_else(
            || (index < self.rows.len()).then_some(index),
            |indices| indices.get(index).map(|value| *value as usize),
        )
    }

    fn identity_rows(
        &self,
    ) -> impl ExactSizeIterator<Item = (AlignmentRowIdentity, Option<usize>)> + '_ {
        (0..self.len()).map(|index| {
            (
                self.identity(index).expect("validated row identity"),
                self.row(index),
            )
        })
    }

    #[cfg(test)]
    fn materialized_rows(&self) -> Vec<Option<usize>> {
        (0..self.len()).map(|index| self.row(index)).collect()
    }
}

impl PartialEq for AlignmentRowLayout {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self.group_starts == other.group_starts
            && self.group_mode == other.group_mode
            && self.group_order == other.group_order
            && self.sort_mode == other.sort_mode
            && self.sort_order == other.sort_order
            && self.sort_position == other.sort_position
            && self.promoted_read_name_hash == other.promoted_read_name_hash
            && self.max_rows == other.max_rows
            && self.view_as_pairs == other.view_as_pairs
            && (0..self.len()).all(|index| {
                self.row(index) == other.row(index)
                    && self.identity(index) == other.identity(index)
                    && self.representative(index) == other.representative(index)
            })
    }
}

pub fn layout_alignment_query(
    query: &AlignmentQuery,
    viewport: Viewport,
    options: AlignmentLayoutOptions,
) -> AlignmentLayout {
    let rows = assign_alignment_rows(query, options, None);
    layout_alignment_query_with_rows(query, viewport, options, &rows)
}

pub fn layout_sashimi(
    summary: &JunctionSummary,
    viewport: Viewport,
    options: AlignmentLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
) -> AlignmentLayout {
    let mut display_list = DisplayList::default();
    let mut junction_arc_targets = Vec::new();
    let (
        rendered_junctions,
        hidden_junctions,
        maximum_junction_value,
        junction_normalization_available,
    ) = add_junctions(
        &mut display_list,
        &mut junction_arc_targets,
        summary,
        viewport,
        &options,
        annotations,
    );
    AlignmentLayout {
        junction_arc_targets,
        pinned_rectangle_count: display_list.rectangles.len(),
        pinned_text_count: display_list.texts.len(),
        stats: AlignmentLayoutStats {
            rendered_junctions,
            hidden_junctions,
            maximum_junction_depth: summary.maximum_depth(),
            maximum_junction_value,
            junction_normalization_available,
            primitive_count: display_list.rectangles.len(),
            ..AlignmentLayoutStats::default()
        },
        display_list,
    }
}

pub fn layout_alignment_query_with_rows(
    query: &AlignmentQuery,
    viewport: Viewport,
    options: AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
) -> AlignmentLayout {
    layout_alignment_query_with_rows_and_annotations(query, viewport, options, rows, None)
}

#[allow(clippy::too_many_lines)]
pub fn layout_alignment_query_with_rows_and_annotations(
    query: &AlignmentQuery,
    viewport: Viewport,
    options: AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
    annotations: Option<&JunctionAnnotationLookup>,
) -> AlignmentLayout {
    debug_assert_eq!(query.batch.len(), rows.len());
    let mut display_list = DisplayList::default();
    let mut junction_arc_targets = Vec::new();
    if options.show_reference
        && viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
        && let Some(reference) = &query.reference
    {
        add_reference_bases(
            &mut display_list,
            reference,
            viewport,
            options.reference_top,
            false,
        );
    }
    let maximum_coverage = add_coverage(
        &mut display_list,
        &query.pileup,
        query.reference.as_ref(),
        query.interval,
        viewport,
        viewport.pixel_width() as f32,
        &options,
    );
    if options.show_coverage {
        add_coverage_navigation_markers(&mut display_list, viewport, &options);
    }
    let (
        rendered_junctions,
        hidden_junctions,
        maximum_junction_value,
        junction_normalization_available,
    ) = add_junctions(
        &mut display_list,
        &mut junction_arc_targets,
        &query.junctions,
        viewport,
        &options,
        annotations,
    );
    add_modification_frequency(
        &mut display_list,
        &query.pileup.modifications,
        query.interval,
        viewport,
        &options,
    );
    // Reference and coverage are emitted first so the desktop can keep this
    // prefix frozen while applying vertical scrolling only to the read stack.
    let pinned_rectangle_count = display_list.rectangles.len();
    let pinned_text_count = display_list.texts.len();
    let read_stats = if !options.show_reads {
        ReadRenderStats {
            hidden: query.batch.len(),
            ..ReadRenderStats::default()
        }
    } else if let Some(refusal) = options.read_refusal {
        add_complete_read_refusal_placeholder(&mut display_list, viewport, &options, refusal);
        ReadRenderStats {
            hidden: refusal.eligible_records,
            ..ReadRenderStats::default()
        }
    } else if matches!(
        viewport.detail_level(),
        DetailLevel::Reads | DetailLevel::Bases
    ) {
        add_reads(&mut display_list, query, viewport, &options, rows)
    } else if viewport.detail_level() == DetailLevel::Regional {
        add_spans(&mut display_list, query, viewport, &options, rows)
    } else {
        add_overview_reads_placeholder(&mut display_list, query, viewport, &options);
        ReadRenderStats {
            hidden: query.batch.len(),
            ..ReadRenderStats::default()
        }
    };
    let primitive_count = display_list.rectangles.len();
    let (packed_row_count, visible_occupied_rows, visible_row_span) =
        rows.indexed_row_metrics().unwrap_or_else(|| {
            let packed_row_count = (0..rows.len())
                .filter_map(|index| rows.row(index))
                .max()
                .map_or(0, |row| row.saturating_add(1));
            let visible_rows = query
                .batch
                .starts()
                .zip(query.batch.ends())
                .enumerate()
                .filter_map(|(index, (start, end))| {
                    (start < viewport.interval().end() && end > viewport.interval().start())
                        .then(|| rows.row(index))
                        .flatten()
                })
                .collect::<BTreeSet<_>>();
            let visible_row_span = visible_rows
                .first()
                .zip(visible_rows.last())
                .map_or(0, |(first, last)| {
                    last.saturating_sub(*first).saturating_add(1)
                });
            (packed_row_count, visible_rows.len(), visible_row_span)
        });
    let natural_stack_height = if query.batch.len() > rows::MAXIMUM_LIVE_CANONICAL_METRIC_RECORDS {
        visible_row_span
    } else {
        natural_stack_height(query, viewport.interval())
    };
    AlignmentLayout {
        display_list,
        junction_arc_targets,
        pinned_rectangle_count,
        pinned_text_count,
        stats: AlignmentLayoutStats {
            input_alignments: query.stats.records_eligible,
            rendered_alignments: read_stats.rendered,
            hidden_alignments: read_stats.hidden.saturating_add(read_stats.virtualized),
            virtualized_alignments: read_stats.virtualized,
            primitive_count,
            maximum_coverage,
            rendered_junctions,
            hidden_junctions,
            maximum_junction_depth: query.junctions.maximum_depth(),
            maximum_junction_value,
            junction_normalization_available,
            bisulfite_marks_omitted: read_stats.bisulfite_marks_omitted,
            smrt_kinetics_marks_omitted: read_stats.smrt_kinetics_marks_omitted,
            smrt_kinetics_unavailable_records: read_stats.smrt_kinetics_unavailable_records,
            smrt_kinetics_malformed_records: read_stats.smrt_kinetics_malformed_records,
            sbx_tail_marks_omitted: read_stats.sbx_tail_marks_omitted,
            sbx_unavailable_records: read_stats.sbx_unavailable_records,
            sbx_unavailable_insertions: read_stats.sbx_unavailable_insertions,
            ultima_quality_marks_omitted: read_stats.ultima_quality_marks_omitted,
            ultima_unavailable_records: read_stats.ultima_unavailable_records,
            ultima_malformed_records: read_stats.ultima_malformed_records,
            ultima_unavailable_events: read_stats.ultima_unavailable_events,
            regional_insertions_rendered: read_stats.regional_insertions_rendered,
            regional_insertions_collapsed: read_stats.regional_insertions_collapsed,
            regional_insertions_omitted: read_stats.regional_insertions_omitted,
            packed_row_count,
            visible_occupied_rows,
            visible_row_span,
            natural_stack_height,
            materialized_row_start: options.materialized_row_range.map_or(0, |(start, _)| start),
            materialized_row_end: options
                .materialized_row_range
                .map_or(packed_row_count, |(_, end)| end.min(packed_row_count)),
        },
    }
}

fn add_complete_read_refusal_placeholder(
    display_list: &mut DisplayList,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    refusal: IndividualReadRefusal,
) {
    let top = options.reads_top + 5.0;
    let width = (viewport.pixel_width() as f32 - 24.0).max(1.0);
    display_list.rectangles.push(RectPrimitive {
        x: 12.0,
        y: top,
        width,
        height: 48.0,
        color: Rgba::new(0.96, 0.93, 0.87, 0.98),
        object_id: 0,
        shape: Default::default(),
    });
    display_list.rectangles.push(RectPrimitive {
        x: 12.0,
        y: top,
        width: 3.0_f32.min(width),
        height: 48.0,
        color: Rgba::new(0.76, 0.45, 0.12, 1.0),
        object_id: 0,
        shape: Default::default(),
    });
    let subject = match refusal.scope {
        IndividualReadRefusalScope::Foreground => "eligible reads overlap this interval",
        IndividualReadRefusalScope::LayoutHorizon => "reads occur in the stable layout horizon",
    };
    display_list.texts.push(TextPrimitive {
        x: viewport.pixel_width() as f32 * 0.5,
        y: top + 15.0,
        text: format!(
            "{} {subject}; complete-read limit is {}",
            refusal.eligible_records, refusal.limit
        ),
        font_size: 12.0,
        color: Rgba::new(0.30, 0.24, 0.17, 1.0),
    });
    display_list.texts.push(TextPrimitive {
        x: viewport.pixel_width() as f32 * 0.5,
        y: top + 34.0,
        text: "Zoom in to inspect complete reads, or raise the limit in track properties"
            .to_owned(),
        font_size: 11.0,
        color: Rgba::new(0.38, 0.31, 0.22, 1.0),
    });
}

fn natural_stack_height(query: &AlignmentQuery, interval: rgv_core::GenomicInterval) -> usize {
    let mut events = BTreeMap::<u64, i64>::new();
    for (start, end) in query.batch.starts().zip(query.batch.ends()) {
        let start = start.max(interval.start());
        let end = end.min(interval.end());
        if start >= end {
            continue;
        }
        *events.entry(start).or_default() += 1;
        *events.entry(end).or_default() -= 1;
    }
    let mut depth = 0_i64;
    let mut maximum = 0_i64;
    for delta in events.into_values() {
        depth = depth.saturating_add(delta);
        maximum = maximum.max(depth);
    }
    usize::try_from(maximum.max(0)).unwrap_or(usize::MAX)
}

fn add_overview_reads_placeholder(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
) {
    let top = options.reads_top + 5.0;
    let width = (viewport.pixel_width() as f32 - 24.0).max(1.0);
    display_list.rectangles.push(RectPrimitive {
        x: 12.0,
        y: top,
        width,
        height: 32.0,
        color: Rgba::new(0.92, 0.94, 0.97, 0.95),
        object_id: 0,
        shape: Default::default(),
    });
    display_list.rectangles.push(RectPrimitive {
        x: 12.0,
        y: top,
        width: 3.0_f32.min(width),
        height: 32.0,
        color: Rgba::new(0.25, 0.45, 0.72, 0.90),
        object_id: 0,
        shape: Default::default(),
    });
    let count = query.stats.records_eligible;
    let text = if query.stats.cancelled {
        if count == 0 {
            "Alignment query incomplete — zoom in or retry".to_owned()
        } else {
            format!("At least {count} alignments found — overview query incomplete")
        }
    } else if count == 0 {
        "No alignments in this region".to_owned()
    } else {
        format!(
            "{count} alignment{} present — zoom in to view individual reads",
            if count == 1 { "" } else { "s" }
        )
    };
    display_list.texts.push(TextPrimitive {
        x: viewport.pixel_width() as f32 * 0.5,
        y: top + 16.0,
        text,
        font_size: 12.0,
        color: Rgba::new(0.25, 0.29, 0.35, 1.0),
    });
}

mod rows;
#[cfg(test)]
use rows::grouped_indices;
use rows::{AlignmentGroupKey, effective_sort_position, first_of_pair_reverse};
pub use rows::{
    alignment_read_name_hash, alignment_row_organization_metrics, alignment_row_policy_fingerprint,
    assign_alignment_rows, reconcile_alignment_rows, reconcile_alignment_rows_with_horizon,
    reconcile_alignment_rows_with_horizon_cancellable,
    reconcile_alignment_rows_with_horizon_scene_cancellable,
};
mod coverage;
pub use coverage::junction_count_value;
use coverage::{
    add_coverage, add_coverage_navigation_markers, add_junctions, add_modification_frequency,
    add_spans,
};
mod reads;
use reads::{add_reads, for_each_aligned_reference_block};
mod cigar;
#[cfg(test)]
use cigar::add_modified_base_calls;
pub use cigar::modification_code_color;
use cigar::{
    add_chain_highlight, add_cigar, add_match_segment, add_mate_connector, add_reference_bases,
    reference_position_for_read_position,
};
mod reference;
pub use reference::{
    layout_reference_slice, layout_reference_slice_oriented, layout_reference_translation,
};
mod technologies;
#[cfg(test)]
use technologies::sbx_simplex_tail_color;
use technologies::{
    add_aligned_bases, add_bisulfite_marks, add_sbx_tail_marks, add_smrt_kinetics_marks,
    sbx_hides_read_position, sbx_insertion_color, sbx_retained_reference_span,
    sbx_visible_read_range,
};
mod color;
pub use color::ultima_quality_color;
use color::{
    add_base_glyph, add_event_label, add_marker, add_read_body, add_ultima_deletion_quality_mark,
    add_ultima_insertion_quality_mark, aligned_base, alignment_color, base_color,
    bisulfite_conversion_mismatch, bisulfite_observation, clipped_geometry, complement,
    fill_label_color, insertion_event_color, palette_color, read_base_label_color,
    record_tag_label, reference_base_label_color, smrt_kinetics_color, visible_positions,
    with_alpha,
};

#[cfg(test)]
#[path = "alignment_layout_tests.rs"]
mod tests;
