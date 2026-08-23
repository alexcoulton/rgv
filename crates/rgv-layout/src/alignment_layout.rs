use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap},
};

pub use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    BisulfiteContext, SbxDisplayOptions, SbxSimplexTailMode, SmrtKineticsMode,
    UltimaDisplayOptions, UltimaQualityMode,
};
use rgv_core::{
    DetailLevel, GeneticCode, ModifiedBaseColorMode, ReadBaseDisplayMode,
    ReferenceTranslationFrame, Viewport,
};
use rgv_core::{JunctionDisplayMode, SashimiCountMode};
use rgv_hts::{
    AlignmentFlags, AlignmentQuery, AuxValue, BisulfiteObservation, CigarOp, CigarOpKind,
    ImplicitModificationMode, JunctionEvidence, JunctionSummary, ModificationBin, ModificationCode,
    ModificationSummary, PairClassification, PairOrientation, PileupBin, PileupSummary,
    ReferenceSlice, SbxInsertionState, SbxTailBounds, SequenceView, SmrtKineticsSeries,
    TemplateLengthBounds, UltimaEvidenceError, UltimaRecordAnalysis, analyze_ultima_record,
    bisulfite_strand_is_reverse, classify_bisulfite_observation, classify_sbx_insertion,
    decode_modified_bases_parts, decode_smrt_kinetics, pair_evidence_at, sbx_tail_bounds,
    smrt_kinetics_tag,
};

use crate::{DisplayList, RectPrimitive, Rgba, TextPrimitive, read_detail_object_id};

pub const MAX_HIDDEN_MODIFICATION_CODES: usize = 8;
pub const MAX_RENDERED_JUNCTIONS: usize = 4_096;
pub const MAX_RENDERED_JUNCTION_LABELS: usize = 512;
pub const MAX_RENDERED_BISULFITE_MARKS: usize = 50_000;
pub const MAX_RENDERED_SMRT_KINETICS_MARKS: usize = 50_000;
pub const MAX_RENDERED_SBX_TAIL_MARKS: usize = 50_000;
pub const MAX_RENDERED_ULTIMA_QUALITY_MARKS: usize = 50_000;
const MAX_EXPANDED_JUNCTION_ROWS: usize = 12;
pub const MAX_ANNOTATED_JUNCTIONS: usize = 100_000;

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
    pub coverage_scale_maximum: Option<u32>,
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
    pub selected_junction: [u64; 2],
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
            coverage_scale_maximum: None,
            show_coverage_center_line: false,
            junction_top: 158.0,
            junction_height: 40.0,
            minimum_junction_depth: 1,
            junction_display_mode: JunctionDisplayMode::Collapsed,
            junction_scale_maximum: None,
            junction_count_mode: SashimiCountMode::Raw,
            show_junction_labels: true,
            junction_annotation_fingerprint: 0,
            selected_junction: [u64::MAX; 2],
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AlignmentLayoutStats {
    pub input_alignments: usize,
    pub rendered_alignments: usize,
    pub hidden_alignments: usize,
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
    /// Rectangle prefix belonging to the frozen reference and coverage lanes.
    pub pinned_rectangle_count: usize,
    /// Text prefix belonging to the frozen reference and coverage lanes.
    pub pinned_text_count: usize,
    pub stats: AlignmentLayoutStats,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct AlignmentRowIdentity {
    primary: u64,
    secondary: u64,
    occurrence: u32,
}

/// Viewport-independent row assignments for one bounded alignment query.
///
/// Assignments are calculated before viewport clipping so horizontal navigation
/// cannot cause visible reads to be repacked vertically.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentRowLayout {
    rows: Vec<Option<usize>>,
    identities: Vec<AlignmentRowIdentity>,
    pair_representatives: Vec<usize>,
    group_mode: AlignmentGroupMode,
    group_order: AlignmentOrderDirection,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    max_rows: usize,
    view_as_pairs: bool,
}

impl AlignmentRowLayout {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn compatible_with(&self, options: &AlignmentLayoutOptions) -> bool {
        self.group_mode == options.group_mode
            && self.group_order == options.group_order
            && self.sort_mode == options.sort_mode
            && self.sort_order == options.sort_order
            && self.sort_position == effective_sort_position(options)
            && self.promoted_read_name_hash == options.promoted_read_name_hash
            && self.max_rows == options.max_rows.max(1)
            && self.view_as_pairs == options.view_as_pairs
    }

    fn representative(&self, index: usize) -> usize {
        self.pair_representatives
            .get(index)
            .copied()
            .unwrap_or(index)
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
    let (
        rendered_junctions,
        hidden_junctions,
        maximum_junction_value,
        junction_normalization_available,
    ) = add_junctions(&mut display_list, summary, viewport, &options, annotations);
    AlignmentLayout {
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
    if options.show_reference
        && viewport.detail_level() == DetailLevel::Bases
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
    if options.show_reads
        && query.stats.truncated
        && viewport.detail_level() != DetailLevel::Overview
    {
        display_list.rectangles.push(RectPrimitive {
            x: 0.0,
            y: options.coverage_top + options.coverage_height + 2.0,
            width: viewport.pixel_width() as f32,
            height: 3.0,
            color: Rgba::new(0.08, 0.08, 0.08, 1.0),
            object_id: 0,
        });
    }
    // Reference and coverage are emitted first so the desktop can keep this
    // prefix frozen while applying vertical scrolling only to the read stack.
    let pinned_rectangle_count = display_list.rectangles.len();
    let pinned_text_count = display_list.texts.len();
    let read_stats = if !options.show_reads {
        ReadRenderStats {
            hidden: query.batch.len(),
            ..ReadRenderStats::default()
        }
    } else if matches!(
        viewport.detail_level(),
        DetailLevel::Reads | DetailLevel::Bases
    ) {
        add_reads(&mut display_list, query, viewport, &options, rows)
    } else if viewport.detail_level() == DetailLevel::Regional {
        let (rendered, hidden) = add_spans(&mut display_list, query, viewport, &options, rows);
        ReadRenderStats {
            rendered,
            hidden,
            ..ReadRenderStats::default()
        }
    } else {
        ReadRenderStats {
            hidden: query.batch.len(),
            ..ReadRenderStats::default()
        }
    };
    let primitive_count = display_list.rectangles.len();
    AlignmentLayout {
        display_list,
        pinned_rectangle_count,
        pinned_text_count,
        stats: AlignmentLayoutStats {
            input_alignments: query.stats.records_eligible,
            rendered_alignments: read_stats.rendered,
            hidden_alignments: read_stats.hidden,
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
        },
    }
}

mod rows;
#[cfg(test)]
use rows::{AlignmentGroupKey, grouped_indices};
pub use rows::{alignment_read_name_hash, assign_alignment_rows};
use rows::{effective_sort_position, first_of_pair_reverse};
mod coverage;
pub use coverage::junction_count_value;
use coverage::{
    add_coverage, add_coverage_navigation_markers, add_junctions, add_modification_frequency,
    add_spans,
};
mod reads;
use reads::add_reads;
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
    add_ultima_insertion_quality_mark, alignment_color, base_color, bisulfite_observation,
    clipped_geometry, complement, fill_label_color, oriented_base, palette_color,
    read_base_label_color, record_tag_label, reference_base_label_color, smrt_kinetics_color,
    visible_positions, with_alpha,
};

#[cfg(test)]
#[path = "alignment_layout_tests.rs"]
mod tests;
