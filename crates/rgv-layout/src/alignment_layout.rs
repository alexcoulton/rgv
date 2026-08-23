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

pub fn assign_alignment_rows(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    previous: Option<&AlignmentRowLayout>,
) -> AlignmentRowLayout {
    let pairs = pair_layout_metadata(query, options.view_as_pairs);
    let groups = grouped_indices(
        query,
        options.group_mode,
        options.sort_mode,
        options.sort_order,
        effective_sort_position(&options),
        options.pair_length_bounds,
        options.promoted_read_name_hash,
    );
    let ordered_groups = ordered_group_indices(&groups, options.group_order);
    let group_count = groups.len().max(1);
    let max_rows = options.max_rows.max(1);
    let rows_per_group = (max_rows / group_count).max(1);
    let identities = alignment_row_identities(query, &pairs);
    let mut rows = vec![None; query.batch.len()];
    let compatible_previous = previous.filter(|layout| layout.compatible_with(&options));

    if let Some(previous) = compatible_previous {
        assign_rows_with_history(
            &ordered_groups,
            &pairs,
            rows_per_group,
            max_rows,
            &identities,
            previous,
            &mut rows,
        );
    } else {
        assign_rows_from_scratch(&ordered_groups, &pairs, rows_per_group, max_rows, &mut rows);
    }
    copy_pair_rows(&pairs, &mut rows);

    AlignmentRowLayout {
        rows,
        identities,
        pair_representatives: pairs.representatives,
        group_mode: options.group_mode,
        group_order: options.group_order,
        sort_mode: options.sort_mode,
        sort_order: options.sort_order,
        sort_position: effective_sort_position(&options),
        promoted_read_name_hash: options.promoted_read_name_hash,
        max_rows,
        view_as_pairs: options.view_as_pairs,
    }
}

#[derive(Clone, Debug)]
struct PairLayoutMetadata {
    enabled: bool,
    representatives: Vec<usize>,
    starts: Vec<u64>,
    ends: Vec<u64>,
}

fn pair_layout_metadata(query: &AlignmentQuery, view_as_pairs: bool) -> PairLayoutMetadata {
    let batch = &query.batch;
    let mut representatives = (0..batch.len()).collect::<Vec<_>>();
    let mut starts = batch.starts.clone();
    let mut ends = batch.ends.clone();
    if !view_as_pairs {
        return PairLayoutMetadata {
            enabled: false,
            representatives,
            starts,
            ends,
        };
    }

    let mut first_by_name = HashMap::<Vec<u8>, usize>::new();
    for (index, representative_slot) in representatives.iter_mut().enumerate() {
        let flags = batch.flags[index];
        let name = batch.read_name(index).unwrap_or_default();
        if !flags.is_paired() || flags.is_secondary() || flags.is_supplementary() || name.is_empty()
        {
            continue;
        }
        let representative = *first_by_name.entry(name.to_vec()).or_insert(index);
        *representative_slot = representative;
        starts[representative] = starts[representative].min(batch.starts[index]);
        ends[representative] = ends[representative].max(batch.ends[index]);
        if !flags.is_mate_unmapped()
            && batch.mate_contigs[index] == Some(batch.contigs[index])
            && let Some(mate_start) = batch.mate_starts[index]
        {
            starts[representative] = starts[representative].min(mate_start);
            ends[representative] = ends[representative].max(mate_start.saturating_add(1));
        }
    }
    for index in 0..batch.len() {
        let representative = representatives[index];
        starts[index] = starts[representative];
        ends[index] = ends[representative];
    }
    PairLayoutMetadata {
        enabled: true,
        representatives,
        starts,
        ends,
    }
}

fn copy_pair_rows(pairs: &PairLayoutMetadata, rows: &mut [Option<usize>]) {
    for index in 0..rows.len() {
        let representative = pairs.representatives[index];
        if representative != index {
            rows[index] = rows[representative];
        }
    }
}

fn assign_rows_from_scratch(
    groups: &[&[usize]],
    pairs: &PairLayoutMetadata,
    rows_per_group: usize,
    max_rows: usize,
    rows: &mut [Option<usize>],
) {
    for (group_index, indices) in groups.iter().enumerate() {
        let mut occupied = BinaryHeap::<Reverse<(u64, usize)>>::new();
        let mut allocated = 0_usize;
        for &index in *indices {
            if pairs.representatives[index] != index {
                continue;
            }
            let local_row = if occupied
                .peek()
                .is_some_and(|Reverse((end, _))| *end <= pairs.starts[index])
            {
                let Reverse((_, row)) = occupied.pop().expect("peeked row must exist");
                row
            } else if allocated < rows_per_group {
                let row = allocated;
                allocated += 1;
                row
            } else {
                continue;
            };
            let absolute_row = group_index
                .saturating_mul(rows_per_group)
                .saturating_add(local_row);
            if absolute_row >= max_rows {
                continue;
            }
            occupied.push(Reverse((pairs.ends[index], local_row)));
            rows[index] = Some(absolute_row);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn assign_rows_with_history(
    groups: &[&[usize]],
    pairs: &PairLayoutMetadata,
    rows_per_group: usize,
    max_rows: usize,
    identities: &[AlignmentRowIdentity],
    previous: &AlignmentRowLayout,
    rows: &mut [Option<usize>],
) {
    let previous_rows = previous
        .identities
        .iter()
        .copied()
        .zip(previous.rows.iter().copied())
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .collect::<HashMap<_, _>>();
    let mut occupied = vec![Vec::<(u64, u64, usize)>::new(); max_rows];

    for (group_index, indices) in groups.iter().enumerate() {
        let row_start = group_index.saturating_mul(rows_per_group);
        let row_end = row_start.saturating_add(rows_per_group).min(max_rows);
        for &index in *indices {
            if pairs.representatives[index] != index {
                continue;
            }
            let Some(&preferred) = previous_rows.get(&identities[index]) else {
                continue;
            };
            if (row_start..row_end).contains(&preferred) {
                occupied[preferred].push((pairs.starts[index], pairs.ends[index], index));
            }
        }
    }

    for (row, intervals) in occupied.iter_mut().enumerate() {
        intervals.sort_unstable();
        let mut retained = Vec::with_capacity(intervals.len());
        let mut previous_end = 0_u64;
        for &(start, end, index) in intervals.iter() {
            if retained.is_empty() || start >= previous_end {
                retained.push((start, end, index));
                previous_end = end;
                rows[index] = Some(row);
            }
        }
        *intervals = retained;
    }

    for (group_index, indices) in groups.iter().enumerate() {
        let row_start = group_index.saturating_mul(rows_per_group);
        let row_end = row_start.saturating_add(rows_per_group).min(max_rows);
        for &index in *indices {
            if pairs.representatives[index] != index {
                continue;
            }
            if rows[index].is_some() {
                continue;
            }
            let start = pairs.starts[index];
            let end = pairs.ends[index];
            let Some(row) =
                (row_start..row_end).find(|&row| interval_fits(&occupied[row], start, end))
            else {
                continue;
            };
            let insert_at =
                occupied[row].partition_point(|&(existing_start, _, _)| existing_start <= start);
            occupied[row].insert(insert_at, (start, end, index));
            rows[index] = Some(row);
        }
    }
}

fn interval_fits(intervals: &[(u64, u64, usize)], start: u64, end: u64) -> bool {
    let next = intervals.partition_point(|&(existing_start, _, _)| existing_start < start);
    let clears_previous = next == 0 || intervals[next - 1].1 <= start;
    let clears_next = next == intervals.len() || end <= intervals[next].0;
    clears_previous && clears_next
}

fn alignment_row_identities(
    query: &AlignmentQuery,
    pairs: &PairLayoutMetadata,
) -> Vec<AlignmentRowIdentity> {
    let batch = &query.batch;
    let mut occurrences = HashMap::<(u64, u64), u32>::new();
    (0..batch.len())
        .map(|index| {
            let representative = pairs.representatives[index];
            let flags = batch.flags[index];
            if pairs.enabled
                && flags.is_paired()
                && !flags.is_secondary()
                && !flags.is_supplementary()
                && !batch.read_name(index).unwrap_or_default().is_empty()
            {
                let name = batch.read_name(representative).unwrap_or_default();
                return AlignmentRowIdentity {
                    primary: fnv_mix(0xcbf2_9ce4_8422_2325_u64, name),
                    secondary: fnv_mix(0x6c62_272e_07bb_0142_u64, name),
                    occurrence: 0,
                };
            }
            let mut primary = 0xcbf2_9ce4_8422_2325_u64;
            let mut secondary = 0x6c62_272e_07bb_0142_u64;
            for bytes in [
                batch.contigs[index].get().to_le_bytes().as_slice(),
                batch.starts[index].to_le_bytes().as_slice(),
                batch.ends[index].to_le_bytes().as_slice(),
                batch.flags[index].bits().to_le_bytes().as_slice(),
                batch.mate_contigs[index]
                    .map_or(u32::MAX, rgv_core::ContigId::get)
                    .to_le_bytes()
                    .as_slice(),
                batch.mate_starts[index]
                    .unwrap_or(u64::MAX)
                    .to_le_bytes()
                    .as_slice(),
                batch.template_lengths[index].to_le_bytes().as_slice(),
                batch.read_name(index).unwrap_or_default(),
            ] {
                primary = fnv_mix(primary, bytes);
                secondary = fnv_mix(secondary, bytes);
            }
            let occurrence = occurrences.entry((primary, secondary)).or_default();
            let identity = AlignmentRowIdentity {
                primary,
                secondary,
                occurrence: *occurrence,
            };
            *occurrence = occurrence.saturating_add(1);
            identity
        })
        .collect()
}

fn fnv_mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn add_coverage(
    display_list: &mut DisplayList,
    pileup: &PileupSummary,
    reference: Option<&ReferenceSlice>,
    query_interval: rgv_core::GenomicInterval,
    viewport: Viewport,
    width: f32,
    options: &AlignmentLayoutOptions,
) -> u32 {
    if pileup.is_empty() {
        return 0;
    }
    let visible_depths = pileup.bins.iter().enumerate().filter(|(index, _)| {
        coverage_bin_interval(query_interval, pileup.len(), *index)
            .is_some_and(|interval| interval.overlaps(viewport.interval()))
    });
    let maximum = visible_depths
        .map(|(_, bin)| bin.depth)
        .max()
        .unwrap_or_default();
    if maximum == 0 || !options.show_coverage {
        return maximum;
    }
    let scale_maximum = options.coverage_scale_maximum.unwrap_or(maximum).max(1);
    for (index, bin) in pileup.bins.iter().copied().enumerate() {
        if bin.depth == 0 {
            continue;
        }
        let Some(bin_interval) = coverage_bin_interval(query_interval, pileup.len(), index) else {
            continue;
        };
        let Some(visible) = bin_interval.intersection(viewport.interval()) else {
            continue;
        };
        let x = viewport.pixel_at_genomic(visible.start()) as f32;
        let right = viewport.pixel_at_genomic(visible.end()) as f32;
        let fraction = (bin.depth as f32 / scale_maximum as f32).min(1.0);
        let height = (fraction.sqrt() * options.coverage_height).max(1.0);
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.coverage_top + options.coverage_height - height,
            width: (right - x).max(1.0).min(width),
            height,
            color: Rgba::new(0.48, 0.48, 0.48, 0.90),
            object_id: 0,
        });
        if bin_interval.len() == 1
            && let Some(reference_base) =
                reference.and_then(|reference| reference.base_at(bin_interval.start()))
        {
            add_allele_coverage(
                display_list,
                bin,
                reference_base,
                x,
                options.coverage_top + options.coverage_height,
                (right - x).max(1.0).min(width),
                height,
                options.minimum_allele_fraction,
                options.quality_weighted_alleles,
            );
        }
    }
    maximum
}

fn add_coverage_navigation_markers(
    display_list: &mut DisplayList,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
) {
    if options.show_coverage_center_line {
        display_list.rectangles.push(RectPrimitive {
            x: viewport.pixel_width() as f32 * 0.5,
            y: options.coverage_top,
            width: 1.0,
            height: options.coverage_height,
            color: Rgba::new(0.18, 0.18, 0.18, 0.24),
            object_id: 0,
        });
    }
    if let Some(position) = options
        .selected_pileup_position
        .filter(|position| viewport.interval().contains(*position))
    {
        let x = viewport.pixel_at_genomic(position) as f32;
        let base_width = (1.0 / viewport.bases_per_pixel()) as f32;
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.coverage_top,
            width: base_width.clamp(2.0, 12.0),
            height: options.coverage_height,
            color: Rgba::new(0.08, 0.08, 0.08, 0.32),
            object_id: 0,
        });
    }
}

#[allow(clippy::too_many_lines)]
fn add_junctions(
    display_list: &mut DisplayList,
    summary: &JunctionSummary,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
) -> (usize, usize, f64, bool) {
    if !options.show_junctions || summary.junctions.is_empty() {
        return (
            0,
            0,
            0.0,
            options.junction_count_mode == SashimiCountMode::Raw
                || summary.provenance.normalization_mapped_records.is_some(),
        );
    }
    let interval = viewport.interval();
    let candidate_count = summary
        .junctions
        .iter()
        .filter(|junction| junction.start < interval.end() && junction.end > interval.start())
        .count();
    let mut visible = summary
        .junctions
        .iter()
        .enumerate()
        .filter(|(_, junction)| {
            junction.start < interval.end()
                && junction.end > interval.start()
                && junction.depth >= options.minimum_junction_depth
        })
        .collect::<Vec<_>>();
    let visible_count = visible.len();
    if visible_count > MAX_RENDERED_JUNCTIONS {
        visible.sort_by_key(|(index, junction)| {
            (
                Reverse(junction.depth),
                junction.start,
                junction.end,
                *index,
            )
        });
        visible.truncate(MAX_RENDERED_JUNCTIONS);
        visible.sort_by_key(|(index, junction)| (junction.start, junction.end, *index));
    } else {
        visible.sort_by_key(|(index, junction)| (junction.start, junction.end, *index));
    }
    let normalization_available = options.junction_count_mode == SashimiCountMode::Raw
        || summary.provenance.normalization_mapped_records.is_some();
    let automatic_maximum = visible
        .iter()
        .map(|(_, junction)| junction_count_value(summary, junction, options.junction_count_mode))
        .fold(0.0_f64, f64::max);
    let scale_maximum = options
        .junction_scale_maximum
        .filter(|maximum| maximum.is_finite() && *maximum > 0.0)
        .unwrap_or(automatic_maximum.max(f64::EPSILON));
    let expanded_rows = if options.junction_display_mode == JunctionDisplayMode::Expanded {
        expanded_junction_rows(&visible)
    } else {
        vec![0; visible.len()]
    };
    let row_count = expanded_rows.iter().copied().max().unwrap_or(0) + 1;
    let width = viewport.pixel_width() as f32;
    let mut rendered_labels = 0_usize;
    for ((_, junction), row) in visible.iter().zip(expanded_rows) {
        let left = (viewport.pixel_at_genomic(junction.start) as f32).clamp(0.0, width);
        let right = (viewport.pixel_at_genomic(junction.end) as f32).clamp(0.0, width);
        if right <= left {
            continue;
        }
        let value = junction_count_value(summary, junction, options.junction_count_mode);
        let fraction = (value / scale_maximum).clamp(0.0, 1.0).sqrt() as f32;
        let (baseline, arc_height) =
            if options.junction_display_mode == JunctionDisplayMode::Expanded {
                let band_height = options.junction_height / row_count.max(1) as f32;
                let baseline = options.junction_top + (row + 1) as f32 * band_height;
                let arc_height = (band_height * (0.25 + 0.70 * fraction)).clamp(2.0, band_height);
                (baseline, arc_height)
            } else {
                let baseline = options.junction_top + options.junction_height;
                let arc_height =
                    (options.junction_height * fraction).clamp(3.0, options.junction_height);
                (baseline, arc_height)
            };
        let arc_top = baseline - arc_height;
        let selected = options.selected_junction == [junction.start, junction.end];
        let color = if selected {
            Rgba::new(0.08, 0.08, 0.10, 1.0)
        } else {
            junction_color(junction)
        };
        let thickness = if selected { 2.5 } else { 1.0 + fraction };
        for rectangle in [
            RectPrimitive {
                x: left,
                y: arc_top,
                width: thickness,
                height: arc_height,
                color,
                object_id: 0,
            },
            RectPrimitive {
                x: left,
                y: arc_top,
                width: (right - left).max(thickness),
                height: thickness,
                color,
                object_id: 0,
            },
            RectPrimitive {
                x: (right - thickness).max(left),
                y: arc_top,
                width: thickness,
                height: arc_height,
                color,
                object_id: 0,
            },
        ] {
            display_list.rectangles.push(rectangle);
        }
        if let Some(annotations) = annotations {
            let status = annotations.status(junction.start, junction.end);
            let marker_color = match status {
                JunctionAnnotationStatus::Annotated => Rgba::new(0.16, 0.58, 0.28, 1.0),
                JunctionAnnotationStatus::Novel => Rgba::new(0.92, 0.48, 0.10, 1.0),
                JunctionAnnotationStatus::Unknown => Rgba::new(0.46, 0.48, 0.52, 0.92),
            };
            display_list.rectangles.push(RectPrimitive {
                x: ((left + right) * 0.5 - 2.5).clamp(0.0, (width - 5.0).max(0.0)),
                y: (arc_top - 2.5).max(options.junction_top),
                width: 5.0,
                height: 5.0,
                color: marker_color,
                object_id: 0,
            });
        }
        if options.show_junction_labels
            && rendered_labels < MAX_RENDERED_JUNCTION_LABELS
            && right - left >= 30.0
        {
            display_list.texts.push(TextPrimitive {
                x: (left + right) * 0.5,
                y: (arc_top - 5.5).max(options.junction_top + 5.5),
                text: junction_count_label(summary, junction, options.junction_count_mode),
                font_size: 10.0,
                color: Rgba::new(0.10, 0.10, 0.12, 0.94),
            });
            rendered_labels = rendered_labels.saturating_add(1);
        }
    }
    (
        visible.len(),
        candidate_count.saturating_sub(visible.len()),
        automatic_maximum,
        normalization_available,
    )
}

/// Returns a junction count in the declared Sashimi units. If whole-file index metadata is not
/// available for CPM, the raw count is returned so evidence remains visible; callers must surface
/// the accompanying availability state rather than labelling it CPM.
pub fn junction_count_value(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> f64 {
    match mode {
        SashimiCountMode::Raw => f64::from(junction.depth),
        SashimiCountMode::CountsPerMillion => summary
            .provenance
            .normalization_mapped_records
            .filter(|total| *total > 0)
            .map_or_else(
                || f64::from(junction.depth),
                |total| f64::from(junction.depth) * 1_000_000.0 / total as f64,
            ),
    }
}

fn junction_count_label(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> String {
    match (mode, summary.provenance.normalization_mapped_records) {
        (SashimiCountMode::CountsPerMillion, Some(total)) if total > 0 => {
            let value = junction_count_value(summary, junction, mode);
            if value < 10.0 {
                format!("{value:.1} CPM")
            } else {
                format!("{value:.0} CPM")
            }
        }
        (SashimiCountMode::CountsPerMillion, _) => format!("{} raw", junction.depth),
        (SashimiCountMode::Raw, _) => junction.depth.to_string(),
    }
}

fn expanded_junction_rows(visible: &[(usize, &JunctionEvidence)]) -> Vec<usize> {
    let mut row_ends = Vec::<u64>::new();
    visible
        .iter()
        .map(|(_, junction)| {
            if let Some((row, end)) = row_ends
                .iter_mut()
                .enumerate()
                .find(|(_, end)| **end <= junction.start)
            {
                *end = junction.end;
                row
            } else if row_ends.len() < MAX_EXPANDED_JUNCTION_ROWS {
                row_ends.push(junction.end);
                row_ends.len() - 1
            } else {
                let row = row_ends
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, end)| **end)
                    .map_or(0, |(row, _)| row);
                row_ends[row] = row_ends[row].max(junction.end);
                row
            }
        })
        .collect()
}

fn junction_color(junction: &JunctionEvidence) -> Rgba {
    match (
        junction.positive_depth > 0,
        junction.negative_depth > 0,
        junction.unknown_depth > 0,
    ) {
        (true, false, false) => Rgba::new(0.84, 0.30, 0.38, 0.88),
        (false, true, false) => Rgba::new(0.25, 0.45, 0.82, 0.88),
        (false, false, true) => Rgba::new(0.42, 0.45, 0.50, 0.82),
        _ => Rgba::new(0.58, 0.34, 0.70, 0.88),
    }
}

fn add_modification_frequency(
    display_list: &mut DisplayList,
    summary: &ModificationSummary,
    query_interval: rgv_core::GenomicInterval,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
) {
    if !options.show_modification_frequency || summary.bin_count() == 0 {
        return;
    }
    for index in 0..summary.bin_count() {
        let Some(interval) = summary.bin_interval(query_interval, index) else {
            continue;
        };
        let Some(visible) = interval.intersection(viewport.interval()) else {
            continue;
        };
        let dominant = summary
            .series
            .iter()
            .filter(|series| {
                !options
                    .hidden_modification_codes
                    .contains(&Some(series.key.code))
            })
            .filter_map(|series| {
                let bin = *series.bins.get(index)?;
                (bin.modified_calls > 0).then_some((series.key.code, bin))
            })
            .max_by(|left, right| {
                modification_bin_score(left.1).total_cmp(&modification_bin_score(right.1))
            });
        let Some((code, bin)) = dominant else {
            continue;
        };
        let x = viewport.pixel_at_genomic(visible.start()) as f32;
        let right = viewport.pixel_at_genomic(visible.end()) as f32;
        let width = (right - x).max(1.0);
        let fraction = modification_bin_score(bin).clamp(0.0, 1.0) as f32;
        if options.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified
            && bin.canonical_observations > 0
        {
            display_list.rectangles.push(RectPrimitive {
                x,
                y: options.modification_top,
                width,
                height: options.modification_height,
                color: Rgba::new(0.14, 0.44, 0.82, 0.20),
                object_id: 0,
            });
        }
        let color = match options.modified_base_color_mode {
            ModifiedBaseColorMode::ByType => modification_code_color(code),
            ModifiedBaseColorMode::Mono | ModifiedBaseColorMode::ModifiedVsUnmodified => {
                Rgba::new(0.90, 0.24, 0.92, 0.92)
            }
        };
        let height = (fraction * options.modification_height).max(1.0);
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.modification_top + options.modification_height - height,
            width,
            height,
            color,
            object_id: 0,
        });
    }
    if let Some(position) = options
        .selected_modification_position
        .filter(|position| viewport.interval().contains(*position))
    {
        display_list.rectangles.push(RectPrimitive {
            x: viewport.pixel_at_genomic(position) as f32,
            y: options.modification_top,
            width: ((1.0 / viewport.bases_per_pixel()) as f32).clamp(2.0, 12.0),
            height: options.modification_height,
            color: Rgba::new(0.08, 0.08, 0.08, 0.34),
            object_id: 0,
        });
    }
}

fn modification_bin_score(bin: ModificationBin) -> f64 {
    bin.raw_fraction()
        .or_else(|| {
            bin.mean_probability()
                .map(|probability| probability / f64::from(u8::MAX))
        })
        .unwrap_or(1.0)
}

#[allow(clippy::too_many_arguments)]
fn add_allele_coverage(
    display_list: &mut DisplayList,
    bin: PileupBin,
    reference_base: u8,
    x: f32,
    bottom: f32,
    width: f32,
    total_height: f32,
    minimum_fraction: f32,
    quality_weighted: bool,
) {
    let reference_base = reference_base.to_ascii_uppercase();
    let denominator = if quality_weighted {
        bin.quality_weighted.total() as f64
    } else {
        f64::from(bin.alleles.base_observations())
    };
    if denominator <= 0.0 {
        return;
    }
    let alternate_weight = [b'A', b'C', b'G', b'T', b'N']
        .into_iter()
        .filter(|base| *base != reference_base)
        .map(|base| {
            if quality_weighted {
                bin.quality_weighted.for_base(base) as f64
            } else {
                f64::from(bin.alleles.for_base(base))
            }
        })
        .sum::<f64>();
    if ((alternate_weight / denominator) as f32) < minimum_fraction.clamp(0.0, 1.0) {
        return;
    }

    let mut cursor = bottom;
    for base in [b'A', b'C', b'G', b'T', b'N'] {
        if base == reference_base {
            continue;
        }
        let support = if quality_weighted {
            bin.quality_weighted.for_base(base) as f64
        } else {
            f64::from(bin.alleles.for_base(base))
        };
        if support <= 0.0 {
            continue;
        }
        let height = (support / denominator) as f32 * total_height;
        cursor -= height;
        display_list.rectangles.push(RectPrimitive {
            x,
            y: cursor,
            width,
            height: height.max(1.0).min(total_height),
            color: base_color(base),
            object_id: 0,
        });
    }
}

fn coverage_bin_interval(
    query_interval: rgv_core::GenomicInterval,
    bins: usize,
    index: usize,
) -> Option<rgv_core::GenomicInterval> {
    if bins == 0 || index >= bins {
        return None;
    }
    let query_length = u128::from(query_interval.len());
    let bins = bins as u128;
    let start_offset = (index as u128 * query_length) / bins;
    let end_offset = ((index as u128 + 1) * query_length).div_ceil(bins);
    let start = query_interval
        .start()
        .saturating_add(u64::try_from(start_offset).ok()?);
    let end = query_interval
        .start()
        .saturating_add(u64::try_from(end_offset).ok()?)
        .min(query_interval.end());
    rgv_core::GenomicInterval::new(query_interval.contig(), start, end).ok()
}

fn add_spans(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
) -> (usize, usize) {
    let batch = &query.batch;
    let mut rendered = 0;
    let mut hidden = 0;
    for index in 0..batch.len() {
        let (x, width) = clipped_geometry(batch.starts[index], batch.ends[index], viewport);
        if width <= 0.0 {
            continue;
        }
        let Some(row) = rows.rows[index] else {
            hidden += 1;
            continue;
        };
        let representative = rows.representative(index);
        add_read_body(
            display_list,
            x,
            options.reads_top + row as f32 * options.row_height,
            width.max(1.0),
            options.read_height,
            alignment_color(
                query,
                index,
                options.color_mode,
                options.pair_length_bounds,
                0.78,
            ),
            u32::try_from(representative + 1).unwrap_or(u32::MAX),
        );
        if options
            .selected_chain_read_name_hash
            .is_some_and(|selected| {
                batch
                    .read_name(index)
                    .is_some_and(|name| alignment_read_name_hash(name) == selected)
            })
        {
            add_chain_highlight(
                display_list,
                batch.starts[index],
                batch.ends[index],
                viewport,
                options.reads_top + row as f32 * options.row_height,
                options.read_height,
            );
        }
        rendered += 1;
    }
    (rendered, hidden)
}

#[allow(clippy::too_many_lines)]
fn add_reads(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
) -> ReadRenderStats {
    let batch = &query.batch;
    let mut stats = ReadRenderStats::default();
    let mut event_label_budget = 2_000_usize;
    let mut modification_budget = 50_000_usize;
    let mut bisulfite_budget = BisulfiteRenderBudget::new(MAX_RENDERED_BISULFITE_MARKS);
    let mut smrt_kinetics_budget = SmrtKineticsRenderBudget::new(MAX_RENDERED_SMRT_KINETICS_MARKS);
    let mut sbx_budget = SbxRenderBudget::new(MAX_RENDERED_SBX_TAIL_MARKS);
    let mut ultima_budget = UltimaRenderBudget::new(MAX_RENDERED_ULTIMA_QUALITY_MARKS);
    for index in 0..batch.len() {
        if batch.ends[index] <= viewport.interval().start()
            || batch.starts[index] >= viewport.interval().end()
        {
            continue;
        }
        let Some(row) = rows.rows[index] else {
            stats.hidden += 1;
            continue;
        };
        let smrt_mode = match options.color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => Some(mode),
            _ => None,
        };
        let smrt_tag =
            smrt_mode.map(|mode| smrt_kinetics_tag(mode, batch.flags[index].is_reverse()));
        let smrt_field = smrt_tag.and_then(|tag| match batch.auxiliary_field(index, tag) {
            Ok(Some(field)) => Some(field),
            Ok(None) => {
                stats.smrt_kinetics_unavailable_records += 1;
                None
            }
            Err(_) => {
                stats.smrt_kinetics_malformed_records += 1;
                None
            }
        });
        let smrt_series = match (smrt_tag, smrt_field.as_ref(), batch.sequence(index)) {
            (Some(tag), Some(field), Some(SequenceView::Present { bases, .. })) => {
                if let Ok(series) = decode_smrt_kinetics(tag, &field.value, bases.len()) {
                    Some(series)
                } else {
                    stats.smrt_kinetics_malformed_records += 1;
                    None
                }
            }
            (Some(_), Some(_), _) => {
                stats.smrt_kinetics_unavailable_records += 1;
                None
            }
            _ => None,
        };
        let sbx_state = match (options.color_mode, batch.sequence(index)) {
            (
                AlignmentColorMode::Sbx(sbx_options),
                Some(SequenceView::Present {
                    bases,
                    qualities: Some(qualities),
                }),
            ) => {
                if let Ok(tails) = sbx_tail_bounds(
                    batch.flags[index],
                    bases,
                    qualities,
                    sbx_options.simplex_tail_quality_maximum,
                ) {
                    Some(SbxRenderState {
                        options: sbx_options,
                        tails,
                    })
                } else {
                    stats.sbx_unavailable_records += 1;
                    None
                }
            }
            (AlignmentColorMode::Sbx(_), _) => {
                stats.sbx_unavailable_records += 1;
                None
            }
            _ => None,
        };
        let ultima_analysis = match options.color_mode {
            AlignmentColorMode::Ultima(ultima_options) => {
                let maximum_homopolymer_length = ultima_options
                    .maximum_homopolymer_length_override
                    .or_else(|| {
                        query
                            .read_group(index)
                            .and_then(|group| group.maximum_homopolymer_length)
                    });
                if let Ok(record) = query.materialize_record(index) {
                    match analyze_ultima_record(
                        &record,
                        query.reference.as_ref(),
                        ultima_options.quality_mode,
                        maximum_homopolymer_length,
                    ) {
                        Ok(analysis) => {
                            stats.ultima_unavailable_events = stats
                                .ultima_unavailable_events
                                .saturating_add(analysis.summary.unavailable_events);
                            Some(analysis)
                        }
                        Err(error) if ultima_record_evidence_is_unavailable(&error) => {
                            stats.ultima_unavailable_records += 1;
                            None
                        }
                        Err(_) => {
                            stats.ultima_malformed_records += 1;
                            None
                        }
                    }
                } else {
                    stats.ultima_malformed_records += 1;
                    None
                }
            }
            _ => None,
        };
        let y = options.reads_top + row as f32 * options.row_height;
        let representative = rows.representative(index);
        let object_id = u32::try_from(representative + 1).unwrap_or(u32::MAX);
        if representative == index {
            add_mate_connector(
                display_list,
                query,
                index,
                viewport,
                y,
                options.read_height,
                object_id,
            );
        }
        add_cigar(
            display_list,
            query,
            index,
            viewport,
            y,
            options.read_height,
            object_id,
            options,
            &mut event_label_budget,
            &mut modification_budget,
            &mut bisulfite_budget,
            smrt_series,
            &mut smrt_kinetics_budget,
            sbx_state,
            &mut sbx_budget,
            ultima_analysis.as_ref(),
            &mut ultima_budget,
        );
        if options
            .selected_chain_read_name_hash
            .is_some_and(|selected| {
                query
                    .batch
                    .read_name(index)
                    .is_some_and(|name| alignment_read_name_hash(name) == selected)
            })
        {
            let highlighted_span = sbx_state
                .filter(|state| state.options.simplex_tail_mode == SbxSimplexTailMode::Hide)
                .and_then(|state| {
                    sbx_retained_reference_span(
                        batch.starts[index],
                        batch.cigar(index)?,
                        state.tails,
                    )
                })
                .or_else(|| {
                    sbx_state
                        .is_none_or(|state| {
                            state.options.simplex_tail_mode != SbxSimplexTailMode::Hide
                        })
                        .then_some((batch.starts[index], batch.ends[index]))
                });
            if let Some((start, end)) = highlighted_span {
                add_chain_highlight(display_list, start, end, viewport, y, options.read_height);
            }
        }
        stats.rendered += 1;
    }
    stats.bisulfite_marks_omitted = bisulfite_budget.omitted;
    stats.smrt_kinetics_marks_omitted = smrt_kinetics_budget.omitted;
    stats.sbx_tail_marks_omitted = sbx_budget.omitted;
    stats.sbx_unavailable_insertions = sbx_budget.unavailable_insertions;
    stats.ultima_quality_marks_omitted = ultima_budget.omitted;
    stats
}

const fn ultima_record_evidence_is_unavailable(error: &UltimaEvidenceError) -> bool {
    matches!(
        error,
        UltimaEvidenceError::SequenceUnavailable
            | UltimaEvidenceError::QualitiesUnavailable
            | UltimaEvidenceError::MissingTp
    )
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AlignmentGroupKey {
    All,
    Forward,
    Reverse,
    Tagged(String),
    Missing,
}

#[allow(clippy::too_many_lines)]
fn grouped_indices(
    query: &AlignmentQuery,
    group_mode: AlignmentGroupMode,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    pair_length_bounds: TemplateLengthBounds,
    promoted_read_name_hash: Option<u64>,
) -> BTreeMap<AlignmentGroupKey, Vec<usize>> {
    let batch = &query.batch;
    let pair_evidence = (0..batch.len())
        .map(|index| pair_evidence_at(batch, index, query.interval, pair_length_bounds))
        .collect::<Vec<_>>();
    let sort_labels = match sort_mode {
        AlignmentSortMode::ReadGroup => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, *b"RG"))
                .collect::<Vec<_>>(),
        ),
        AlignmentSortMode::Haplotype => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, *b"HP"))
                .collect::<Vec<_>>(),
        ),
        AlignmentSortMode::Tag(tag) => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, tag))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    let base_sort_keys = (sort_mode == AlignmentSortMode::Base).then(|| {
        (0..batch.len())
            .map(|index| alignment_base_sort_key(query, index, sort_position))
            .collect::<Vec<_>>()
    });
    let mut groups = BTreeMap::<AlignmentGroupKey, Vec<usize>>::new();
    for index in 0..batch.len() {
        groups
            .entry(group_key(query, index, group_mode, pair_length_bounds))
            .or_default()
            .push(index);
    }
    for indices in groups.values_mut() {
        indices.sort_by(|left, right| match sort_mode {
            AlignmentSortMode::Start => (batch.starts[*left], batch.ends[*left], *left).cmp(&(
                batch.starts[*right],
                batch.ends[*right],
                *right,
            )),
            AlignmentSortMode::Strand => {
                (batch.flags[*left].is_reverse(), batch.starts[*left], *left).cmp(&(
                    batch.flags[*right].is_reverse(),
                    batch.starts[*right],
                    *right,
                ))
            }
            AlignmentSortMode::FirstOfPairStrand => (
                first_of_pair_reverse(batch.flags[*left]).is_none(),
                first_of_pair_reverse(batch.flags[*left]).unwrap_or_default(),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    first_of_pair_reverse(batch.flags[*right]).is_none(),
                    first_of_pair_reverse(batch.flags[*right]).unwrap_or_default(),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::MappingQuality => (
                Reverse(batch.mapping_qualities[*left].unwrap_or_default()),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(batch.mapping_qualities[*right].unwrap_or_default()),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::ReadName => batch
                .read_name(*left)
                .cmp(&batch.read_name(*right))
                .then_with(|| batch.starts[*left].cmp(&batch.starts[*right])),
            AlignmentSortMode::ReadGroup
            | AlignmentSortMode::Haplotype
            | AlignmentSortMode::Tag(_) => {
                let labels = sort_labels
                    .as_ref()
                    .expect("tag-derived sort labels exist for this mode");
                (
                    labels[*left].is_none(),
                    labels[*left].as_deref(),
                    batch.starts[*left],
                    *left,
                )
                    .cmp(&(
                        labels[*right].is_none(),
                        labels[*right].as_deref(),
                        batch.starts[*right],
                        *right,
                    ))
            }
            AlignmentSortMode::AlignedLength => (
                Reverse(aligned_length(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(aligned_length(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::LeftClip => (
                Reverse(left_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(left_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::RightClip => (
                Reverse(right_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(right_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::TemplateLength => (
                Reverse(batch.template_lengths[*left].unsigned_abs()),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(batch.template_lengths[*right].unsigned_abs()),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::PairOrientation => (
                pair_evidence[*left].map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::MateContig => (
                batch.mate_contigs[*left],
                batch.mate_starts[*left],
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    batch.mate_contigs[*right],
                    batch.mate_starts[*right],
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::PairStatus => (
                pair_evidence[*left]
                    .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::ReadOrder => (
                pair_evidence[*left].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::Base => (
                base_sort_keys.as_ref().expect("base sort keys exist")[*left],
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    base_sort_keys.as_ref().expect("base sort keys exist")[*right],
                    batch.starts[*right],
                    *right,
                )),
        });
        if sort_order == AlignmentOrderDirection::Reverse {
            indices.reverse();
        }
        if let Some(promoted) = promoted_read_name_hash {
            indices.sort_by_key(|index| {
                batch
                    .read_name(*index)
                    .is_none_or(|name| alignment_read_name_hash(name) != promoted)
            });
        }
    }
    groups
}

fn first_of_pair_reverse(flags: AlignmentFlags) -> Option<bool> {
    if flags.is_first_in_template() {
        Some(flags.is_reverse())
    } else if flags.is_last_in_template() {
        Some(flags.is_mate_reverse())
    } else {
        None
    }
}

fn effective_sort_position(options: &AlignmentLayoutOptions) -> Option<u64> {
    (options.sort_mode == AlignmentSortMode::Base)
        .then_some(options.sort_position)
        .flatten()
}

fn ordered_group_indices(
    groups: &BTreeMap<AlignmentGroupKey, Vec<usize>>,
    order: AlignmentOrderDirection,
) -> Vec<&[usize]> {
    match order {
        AlignmentOrderDirection::Natural => groups.values().map(Vec::as_slice).collect(),
        AlignmentOrderDirection::Reverse => groups.values().rev().map(Vec::as_slice).collect(),
    }
}

/// Sorts the visible reference observation before deletions, skips, unavailable sequence, and
/// records that do not cover the selected position. The base byte provides a deterministic order
/// for IUPAC and other non-ACGT observations without inventing biological equivalence.
fn alignment_base_sort_key(
    query: &AlignmentQuery,
    index: usize,
    sort_position: Option<u64>,
) -> (u8, u8) {
    let Some(target) = sort_position else {
        return (7, 0);
    };
    if target < query.batch.starts[index] || target >= query.batch.ends[index] {
        return (7, 0);
    }
    let Some(cigar) = query.batch.cigar(index) else {
        return (6, 0);
    };
    let mut reference_position = query.batch.starts[index];
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    let offset = usize::try_from(target.saturating_sub(reference_position))
                        .unwrap_or(usize::MAX);
                    let aligned_position = read_position.saturating_add(offset);
                    let base = query.batch.sequence(index).and_then(|sequence| {
                        let SequenceView::Present { bases, qualities } = sequence else {
                            return None;
                        };
                        oriented_base(query.batch.flags[index], bases, qualities, aligned_position)
                            .map(|(base, _)| base.to_ascii_uppercase())
                    });
                    return base.map_or((6, 0), |base| match base {
                        b'A' => (0, base),
                        b'C' => (1, base),
                        b'G' => (2, base),
                        b'T' | b'U' => (3, base),
                        b'N' => (4, base),
                        other => (5, other),
                    });
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_end;
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    return (5, b'-');
                }
                reference_position = reference_end;
            }
            CigarOpKind::ReferenceSkip => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    return (5, b'~');
                }
                reference_position = reference_end;
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    (7, 0)
}

fn group_key(
    query: &AlignmentQuery,
    index: usize,
    mode: AlignmentGroupMode,
    pair_length_bounds: TemplateLengthBounds,
) -> AlignmentGroupKey {
    match mode {
        AlignmentGroupMode::None => AlignmentGroupKey::All,
        AlignmentGroupMode::Strand => {
            if query.batch.flags[index].is_reverse() {
                AlignmentGroupKey::Reverse
            } else {
                AlignmentGroupKey::Forward
            }
        }
        AlignmentGroupMode::ReadGroup => record_tag_label(query, index, *b"RG")
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
        AlignmentGroupMode::Haplotype => record_tag_label(query, index, *b"HP")
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
        AlignmentGroupMode::Chimeric => AlignmentGroupKey::Tagged(
            if query.batch.has_auxiliary_tag(index, *b"SA") {
                "Chimeric"
            } else {
                "Not chimeric"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::Supplementary => AlignmentGroupKey::Tagged(
            if query.batch.flags[index].is_supplementary() {
                "Supplementary"
            } else {
                "Primary/secondary"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::LeftClip => AlignmentGroupKey::Tagged(
            if left_clip(query.batch.cigar(index).unwrap_or_default()) > 0 {
                "Left clipped"
            } else {
                "Not left clipped"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::RightClip => AlignmentGroupKey::Tagged(
            if right_clip(query.batch.cigar(index).unwrap_or_default()) > 0 {
                "Right clipped"
            } else {
                "Not right clipped"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::PairOrientation => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds).map_or_else(
                || "Unavailable".to_owned(),
                |pair| format!("{:?}", pair.orientation),
            ),
        ),
        AlignmentGroupMode::UnexpectedPair => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds)
                .map_or("Unavailable", |pair| match pair.classification {
                    PairClassification::Proper | PairClassification::Concordant => "Expected",
                    PairClassification::Unpaired => "Unpaired",
                    _ => "Unexpected",
                })
                .to_owned(),
        ),
        AlignmentGroupMode::MateContig => {
            AlignmentGroupKey::Tagged(query.batch.mate_contigs[index].map_or_else(
                || "Missing".to_owned(),
                |contig| format!("tid {}", contig.get()),
            ))
        }
        AlignmentGroupMode::ReadOrder => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds).map_or_else(
                || "Unavailable".to_owned(),
                |pair| format!("{:?}", pair.read_order),
            ),
        ),
        AlignmentGroupMode::FirstOfPairStrand => {
            let flags = query.batch.flags[index];
            AlignmentGroupKey::Tagged(
                if flags.is_first_in_template() {
                    if flags.is_reverse() {
                        "First reverse"
                    } else {
                        "First forward"
                    }
                } else if flags.is_last_in_template() {
                    if flags.is_mate_reverse() {
                        "First reverse"
                    } else {
                        "First forward"
                    }
                } else {
                    "Unknown"
                }
                .to_owned(),
            )
        }
        AlignmentGroupMode::Tag(tag) => record_tag_label(query, index, tag)
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
    }
}

const fn orientation_rank(orientation: PairOrientation) -> u8 {
    match orientation {
        PairOrientation::Inward => 0,
        PairOrientation::Outward => 1,
        PairOrientation::SameStrand => 2,
        PairOrientation::Unknown => 3,
    }
}

const fn classification_rank(classification: PairClassification) -> u8 {
    match classification {
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

const fn read_order_rank(order: rgv_hts::ReadOrder) -> u8 {
    match order {
        rgv_hts::ReadOrder::First => 0,
        rgv_hts::ReadOrder::Last => 1,
        rgv_hts::ReadOrder::Unknown => 2,
        rgv_hts::ReadOrder::Ambiguous => 3,
    }
}

pub fn alignment_read_name_hash(name: &[u8]) -> u64 {
    fnv_mix(0xcbf2_9ce4_8422_2325_u64, name)
}

fn aligned_length(cigar: &[CigarOp]) -> u64 {
    cigar.iter().fold(0_u64, |length, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Insertion
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            length.saturating_add(u64::from(operation.len))
        } else {
            length
        }
    })
}

fn left_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .first()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}

fn right_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .last()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}

fn add_chain_highlight(
    display_list: &mut DisplayList,
    start: u64,
    end: u64,
    viewport: Viewport,
    y: f32,
    height: f32,
) {
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    let color = Rgba::new(0.05, 0.66, 0.78, 1.0);
    let stroke = 1.5_f32.min(height * 0.35).max(1.0);
    for rectangle in [
        RectPrimitive {
            x,
            y: y - stroke,
            width,
            height: stroke,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x,
            y: y + height,
            width,
            height: stroke,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x: x - stroke,
            y: y - stroke,
            width: stroke,
            height: height + stroke * 2.0,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x: x + width,
            y: y - stroke,
            width: stroke,
            height: height + stroke * 2.0,
            color,
            object_id: 0,
        },
    ] {
        display_list.rectangles.push(rectangle);
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
fn add_cigar(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    event_label_budget: &mut usize,
    modification_budget: &mut usize,
    bisulfite_budget: &mut BisulfiteRenderBudget,
    smrt_series: Option<SmrtKineticsSeries<'_>>,
    smrt_kinetics_budget: &mut SmrtKineticsRenderBudget,
    sbx_state: Option<SbxRenderState>,
    sbx_budget: &mut SbxRenderBudget,
    ultima_analysis: Option<&UltimaRecordAnalysis>,
    ultima_budget: &mut UltimaRenderBudget,
) {
    let batch = &query.batch;
    let Some(cigar) = batch.cigar(index) else {
        return;
    };
    let mut reference_position = batch.starts[index];
    let mut read_position = 0_usize;
    if cigar.is_empty() {
        let (x, width) = clipped_geometry(batch.starts[index], batch.ends[index], viewport);
        add_read_body(
            display_list,
            x,
            y,
            width.max(1.0),
            height,
            alignment_color(
                query,
                index,
                options.color_mode,
                options.pair_length_bounds,
                0.92,
            ),
            object_id,
        );
        return;
    }
    for (operation_index, operation) in cigar.iter().enumerate() {
        let length = u64::from(operation.len);
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let end = reference_position.saturating_add(length);
                if viewport.detail_level() == DetailLevel::Bases {
                    add_aligned_bases(
                        display_list,
                        query,
                        index,
                        operation.kind,
                        reference_position,
                        read_position,
                        operation.len as usize,
                        viewport,
                        y,
                        height,
                        object_id,
                        options.color_mode,
                        options.pair_length_bounds,
                        options.read_base_display_mode,
                        options.shade_read_bases_by_quality,
                        bisulfite_budget,
                        smrt_series,
                        smrt_kinetics_budget,
                        sbx_state,
                        sbx_budget,
                    );
                } else {
                    if let Some((visible_start, visible_end)) =
                        sbx_visible_read_range(sbx_state, read_position, operation.len as usize)
                    {
                        let visible_reference_start = reference_position
                            .saturating_add((visible_start - read_position) as u64);
                        let visible_reference_end =
                            reference_position.saturating_add((visible_end - read_position) as u64);
                        add_match_segment(
                            display_list,
                            query,
                            index,
                            operation.kind,
                            visible_reference_start,
                            visible_reference_end,
                            viewport,
                            y,
                            height,
                            object_id,
                            options.color_mode,
                            options.pair_length_bounds,
                        );
                    }
                    if let AlignmentColorMode::Bisulfite(context) = options.color_mode {
                        add_bisulfite_marks(
                            display_list,
                            query,
                            index,
                            context,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            options.shade_read_bases_by_quality,
                            bisulfite_budget,
                        );
                    }
                    if let Some(series) = smrt_series {
                        add_smrt_kinetics_marks(
                            display_list,
                            query,
                            index,
                            series,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            smrt_kinetics_budget,
                        );
                    }
                    if sbx_state.is_some_and(|state| {
                        state.options.simplex_tail_mode == SbxSimplexTailMode::Highlight
                    }) {
                        add_sbx_tail_marks(
                            display_list,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            sbx_state.expect("checked SBX state"),
                            sbx_budget,
                        );
                    }
                }
                reference_position = end;
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(length);
                let (x, width) = clipped_geometry(reference_position, end, viewport);
                let visible_event = operation.kind == CigarOpKind::ReferenceSkip
                    || operation.len >= options.minimum_indel_size;
                let visible_after_sbx_tail_hiding = sbx_state.is_none_or(|state| {
                    state.options.simplex_tail_mode != SbxSimplexTailMode::Hide
                        || (read_position > state.tails.retained_start
                            && read_position < state.tails.retained_end)
                });
                if visible_event && visible_after_sbx_tail_hiding && width > 0.0 {
                    display_list.rectangles.push(RectPrimitive {
                        x,
                        y: y + height * 0.48,
                        width: width.max(1.0),
                        height: 1.0,
                        color: if operation.kind == CigarOpKind::ReferenceSkip {
                            Rgba::new(0.26, 0.36, 0.52, 0.92)
                        } else {
                            Rgba::new(0.12, 0.12, 0.14, 0.96)
                        },
                        object_id: read_detail_object_id(object_id),
                    });
                    if width >= 18.0 && operation.len >= options.minimum_event_label_size {
                        let label = if operation.kind == CigarOpKind::ReferenceSkip {
                            format!("N{}", operation.len)
                        } else {
                            operation.len.to_string()
                        };
                        add_event_label(
                            display_list,
                            &label,
                            x + width * 0.5,
                            y + height * 0.5,
                            if operation.kind == CigarOpKind::ReferenceSkip {
                                Rgba::new(0.20, 0.31, 0.52, 1.0)
                            } else {
                                Rgba::new(0.10, 0.10, 0.12, 1.0)
                            },
                            object_id,
                            event_label_budget,
                        );
                    }
                    if operation.kind == CigarOpKind::Deletion
                        && let Some(quality) = ultima_analysis
                            .and_then(|analysis| analysis.event(operation_index))
                            .and_then(|event| event.quality)
                        && ultima_budget.take()
                    {
                        add_ultima_deletion_quality_mark(
                            display_list,
                            x,
                            width,
                            y,
                            height,
                            quality,
                            object_id,
                        );
                    }
                }
                reference_position = end;
            }
            CigarOpKind::Insertion => {
                let visible_range =
                    sbx_visible_read_range(sbx_state, read_position, operation.len as usize);
                let visible_length = visible_range
                    .map(|(start, end)| end.saturating_sub(start))
                    .unwrap_or_default();
                let insertion_color =
                    visible_range.map_or(Rgba::new(0.86, 0.32, 0.82, 0.96), |(start, end)| {
                        sbx_insertion_color(
                            query,
                            index,
                            matches!(options.color_mode, AlignmentColorMode::Sbx(_)),
                            sbx_state,
                            start,
                            end.saturating_sub(start),
                            sbx_budget,
                        )
                    });
                if visible_length >= options.minimum_indel_size as usize {
                    add_marker(
                        display_list,
                        viewport,
                        reference_position,
                        y,
                        height,
                        insertion_color,
                        object_id,
                    );
                    if let Some(quality) = ultima_analysis
                        .and_then(|analysis| analysis.event(operation_index))
                        .and_then(|event| event.quality)
                        && ultima_budget.take()
                    {
                        add_ultima_insertion_quality_mark(
                            display_list,
                            viewport,
                            reference_position,
                            y,
                            height,
                            quality,
                            object_id,
                        );
                    }
                }
                if visible_length >= options.minimum_indel_size as usize
                    && visible_length >= options.minimum_event_label_size as usize
                    && viewport.bases_per_pixel() <= 5.0
                {
                    let label = if visible_length == 1 {
                        "I".to_owned()
                    } else {
                        format!("I{visible_length}")
                    };
                    add_event_label(
                        display_list,
                        &label,
                        viewport.pixel_at_genomic(reference_position) as f32,
                        y + height * 0.5,
                        with_alpha(insertion_color, 1.0),
                        object_id,
                        event_label_budget,
                    );
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::SoftClip => {
                let visible =
                    sbx_visible_read_range(sbx_state, read_position, operation.len as usize)
                        .is_some();
                if visible {
                    add_marker(
                        display_list,
                        viewport,
                        reference_position,
                        y,
                        height,
                        Rgba::new(0.62, 0.40, 0.86, 0.92),
                        object_id,
                    );
                }
                if visible
                    && operation.len >= options.minimum_event_label_size
                    && viewport.bases_per_pixel() <= 2.0
                {
                    add_event_label(
                        display_list,
                        "S",
                        viewport.pixel_at_genomic(reference_position) as f32,
                        y + height * 0.5,
                        Rgba::new(0.48, 0.26, 0.72, 1.0),
                        object_id,
                        event_label_budget,
                    );
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::HardClip => add_marker(
                display_list,
                viewport,
                reference_position,
                y,
                height,
                Rgba::new(0.62, 0.40, 0.86, 0.92),
                object_id,
            ),
            CigarOpKind::Padding => add_marker(
                display_list,
                viewport,
                reference_position,
                y,
                height,
                Rgba::new(0.92, 0.76, 0.20, 0.90),
                object_id,
            ),
        }
    }
    if options.show_modified_bases {
        add_modified_base_calls(
            display_list,
            query,
            index,
            viewport,
            y,
            height,
            object_id,
            options,
            modification_budget,
            sbx_state,
        );
    }
}

fn add_mate_connector(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
) {
    let batch = &query.batch;
    let (Some(mate_contig), Some(mate_start)) =
        (batch.mate_contigs[index], batch.mate_starts[index])
    else {
        return;
    };
    if mate_contig != batch.contigs[index] {
        return;
    }
    let start = batch.ends[index].min(mate_start);
    let end = batch.ends[index].max(mate_start);
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    display_list.rectangles.push(RectPrimitive {
        x,
        y: y + height * 0.5,
        width,
        height: 1.0,
        color: Rgba::new(0.58, 0.64, 0.74, 0.62),
        object_id,
    });
}

#[allow(clippy::too_many_arguments)]
fn add_modified_base_calls(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    budget: &mut usize,
    sbx_state: Option<SbxRenderState>,
) {
    let batch = &query.batch;
    let Some(SequenceView::Present { bases, .. }) = batch.sequence(index) else {
        return;
    };
    let Some(cigar) = batch.cigar(index) else {
        return;
    };
    let mm = batch
        .auxiliary_field(index, *b"MM")
        .ok()
        .flatten()
        .or_else(|| batch.auxiliary_field(index, *b"Mm").ok().flatten());
    let Some(mm) = mm else {
        return;
    };
    let ml = batch
        .auxiliary_field(index, *b"ML")
        .ok()
        .flatten()
        .or_else(|| batch.auxiliary_field(index, *b"Ml").ok().flatten());
    let Ok(calls) =
        decode_modified_bases_parts(bases, &mm.value, ml.as_ref().map(|field| &field.value))
    else {
        return;
    };
    let sequence_length = bases.len();
    for call in &calls {
        if *budget == 0 {
            break;
        }
        if (options.minimum_modification_probability > 0
            && call
                .probability
                .is_none_or(|probability| probability < options.minimum_modification_probability))
            || options.hidden_modification_codes.contains(&Some(call.code))
        {
            continue;
        }
        let aligned_position = if batch.flags[index].is_reverse() {
            let Some(position) = sequence_length.checked_sub(call.read_position.saturating_add(1))
            else {
                continue;
            };
            position
        } else {
            call.read_position
        };
        if sbx_hides_read_position(sbx_state, aligned_position) {
            continue;
        }
        let Some(reference_position) =
            reference_position_for_read_position(batch.starts[index], cigar, aligned_position)
        else {
            continue;
        };
        if !viewport.interval().contains(reference_position) {
            continue;
        }
        let (x, base_width) = clipped_geometry(
            reference_position,
            reference_position.saturating_add(1),
            viewport,
        );
        let alpha = call
            .probability
            .map_or(0.78, |value| 0.35 + f32::from(value) / 255.0 * 0.65);
        let mut color = match options.modified_base_color_mode {
            ModifiedBaseColorMode::ByType => modification_code_color(call.code),
            ModifiedBaseColorMode::Mono | ModifiedBaseColorMode::ModifiedVsUnmodified => {
                Rgba::new(0.90, 0.24, 0.92, 1.0)
            }
        };
        color.alpha = alpha;
        display_list.rectangles.push(RectPrimitive {
            x: x + base_width * 0.2,
            y: y - 2.0,
            width: (base_width * 0.6).clamp(2.0, 8.0),
            height: (height * 0.22).clamp(2.0, 4.0),
            color,
            object_id: read_detail_object_id(object_id),
        });
        *budget -= 1;
    }
    if options.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified {
        add_unmodified_base_calls(
            display_list,
            &calls,
            bases,
            cigar,
            batch.starts[index],
            batch.flags[index],
            viewport,
            y,
            height,
            object_id,
            options,
            budget,
            sbx_state,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn add_unmodified_base_calls(
    display_list: &mut DisplayList,
    calls: &[rgv_hts::ModifiedBaseCall],
    bases: &[u8],
    cigar: &[CigarOp],
    alignment_start: u64,
    flags: AlignmentFlags,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    budget: &mut usize,
    sbx_state: Option<SbxRenderState>,
) {
    let mut groups = Vec::<(u8, ModificationCode, BTreeSet<usize>)>::new();
    for call in calls {
        if call.implicit_mode != ImplicitModificationMode::Canonical
            || options.hidden_modification_codes.contains(&Some(call.code))
        {
            continue;
        }
        if let Some((_, _, positions)) = groups
            .iter_mut()
            .find(|(base, code, _)| *base == call.canonical_base && *code == call.code)
        {
            positions.insert(call.read_position);
        } else {
            groups.push((
                call.canonical_base,
                call.code,
                BTreeSet::from([call.read_position]),
            ));
        }
    }
    for (canonical_base, _, called_positions) in groups {
        for (read_position, base) in bases.iter().copied().enumerate() {
            if *budget == 0 {
                return;
            }
            if base.to_ascii_uppercase() != canonical_base
                || called_positions.contains(&read_position)
            {
                continue;
            }
            let aligned_position = if flags.is_reverse() {
                let Some(position) = bases.len().checked_sub(read_position.saturating_add(1))
                else {
                    continue;
                };
                position
            } else {
                read_position
            };
            if sbx_hides_read_position(sbx_state, aligned_position) {
                continue;
            }
            let Some(reference_position) =
                reference_position_for_read_position(alignment_start, cigar, aligned_position)
            else {
                continue;
            };
            if !viewport.interval().contains(reference_position) {
                continue;
            }
            let (x, base_width) = clipped_geometry(
                reference_position,
                reference_position.saturating_add(1),
                viewport,
            );
            display_list.rectangles.push(RectPrimitive {
                x: x + base_width * 0.2,
                y: y - 2.0,
                width: (base_width * 0.6).clamp(2.0, 8.0),
                height: (height * 0.22).clamp(2.0, 4.0),
                color: Rgba::new(0.14, 0.44, 0.82, 0.72),
                object_id: read_detail_object_id(object_id),
            });
            *budget -= 1;
        }
    }
}

pub fn modification_code_color(code: ModificationCode) -> Rgba {
    match code {
        ModificationCode::Letter(b'm') => Rgba::new(0.86, 0.20, 0.58, 1.0),
        ModificationCode::Letter(b'h') => Rgba::new(0.12, 0.62, 0.70, 1.0),
        ModificationCode::Letter(b'a') => Rgba::new(0.92, 0.52, 0.10, 1.0),
        ModificationCode::Letter(b'f') => Rgba::new(0.48, 0.32, 0.80, 1.0),
        ModificationCode::Letter(letter) => palette_color(&char::from(letter).to_string(), 1.0),
        ModificationCode::Chebi(code) => palette_color(&format!("CHEBI:{code}"), 1.0),
    }
}

fn reference_position_for_read_position(
    alignment_start: u64,
    cigar: &[CigarOp],
    target_read_position: usize,
) -> Option<u64> {
    let mut reference_position = alignment_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if (read_position..read_position.saturating_add(length))
                    .contains(&target_read_position)
                {
                    return Some(
                        reference_position.saturating_add(
                            target_read_position.saturating_sub(read_position) as u64,
                        ),
                    );
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                if (read_position..read_position.saturating_add(length))
                    .contains(&target_read_position)
                {
                    return Some(reference_position);
                }
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn add_match_segment(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    kind: CigarOpKind,
    start: u64,
    end: u64,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    color_mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
) {
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    let explicit_mismatch = kind == CigarOpKind::SequenceMismatch
        && !matches!(color_mode, AlignmentColorMode::Bisulfite(_));
    let color = if explicit_mismatch {
        Rgba::new(0.96, 0.58, 0.18, 0.95)
    } else {
        alignment_color(query, index, color_mode, pair_length_bounds, 0.88)
    };
    let primitive_object_id = if explicit_mismatch {
        read_detail_object_id(object_id)
    } else {
        object_id
    };
    add_read_body(
        display_list,
        x,
        y,
        width.max(1.0),
        height,
        color,
        primitive_object_id,
    );
}

fn add_reference_bases(
    display_list: &mut DisplayList,
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    reverse_complement: bool,
) {
    for position in visible_positions(reference.interval, viewport) {
        let Some(mut base) = reference.base_at(position) else {
            continue;
        };
        if reverse_complement {
            base = complement(base);
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        if width <= 0.0 {
            continue;
        }
        display_list.rectangles.push(RectPrimitive {
            x,
            y: top,
            width: width.max(1.0),
            height: 20.0,
            color: with_alpha(base_color(base), 0.32),
            object_id: 0,
        });
        if width >= 7.0 {
            add_base_glyph(
                display_list,
                base,
                x,
                top + 3.0,
                width,
                14.0,
                reference_base_label_color(base),
            );
        }
    }
}

/// Lays out a standalone reference slice using the same base colours and glyphs
/// as the alignment reference lane.
#[must_use]
pub fn layout_reference_slice(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
) -> DisplayList {
    layout_reference_slice_oriented(reference, viewport, top, false)
}

#[must_use]
pub fn layout_reference_slice_oriented(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    reverse_complement: bool,
) -> DisplayList {
    let mut display_list = DisplayList::default();
    add_reference_bases(
        &mut display_list,
        reference,
        viewport,
        top,
        reverse_complement,
    );
    display_list
}

#[must_use]
pub fn layout_reference_translation(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    frame: ReferenceTranslationFrame,
    genetic_code: GeneticCode,
) -> DisplayList {
    let mut display = DisplayList::default();
    let visible_start = reference.interval.start().max(viewport.interval().start());
    let visible_end = reference.interval.end().min(viewport.interval().end());
    let phase = frame.phase();
    let mut start = visible_start.saturating_add((phase + 3 - visible_start % 3) % 3);
    while start.saturating_add(3) <= visible_end {
        let (Some(first), Some(second), Some(third)) = (
            reference.base_at(start),
            reference.base_at(start + 1),
            reference.base_at(start + 2),
        ) else {
            break;
        };
        let mut codon = [first, second, third];
        if frame.is_reverse() {
            codon.reverse();
            for base in &mut codon {
                *base = complement(*base);
            }
        }
        let amino_acid = translate_codon(&codon, genetic_code);
        let (x, width) = clipped_geometry(start, start + 3, viewport);
        display.rectangles.push(RectPrimitive {
            x,
            y: top,
            width: width.max(1.0),
            height: 18.0,
            color: if (start / 3).is_multiple_of(2) {
                Rgba::new(0.90, 0.92, 0.95, 1.0)
            } else {
                Rgba::new(0.96, 0.97, 0.98, 1.0)
            },
            object_id: 0,
        });
        if width >= 12.0 {
            display.texts.push(TextPrimitive {
                x: x + width * 0.5,
                y: top + 9.0,
                text: char::from(amino_acid).to_string(),
                font_size: 12.0,
                color: Rgba::new(0.08, 0.10, 0.14, 1.0),
            });
        }
        start = start.saturating_add(3);
    }
    display
}

fn translate_codon(codon: &[u8], genetic_code: GeneticCode) -> u8 {
    const STANDARD: &[u8; 64] = b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG";
    let index = |base: u8| match base.to_ascii_uppercase() {
        b'T' | b'U' => Some(0_usize),
        b'C' => Some(1),
        b'A' => Some(2),
        b'G' => Some(3),
        _ => None,
    };
    if codon.len() < 3 {
        return b'X';
    }
    let (Some(first), Some(second), Some(third)) =
        (index(codon[0]), index(codon[1]), index(codon[2]))
    else {
        return b'X';
    };
    let standard = STANDARD[first * 16 + second * 4 + third];
    if genetic_code == GeneticCode::VertebrateMitochondrial {
        match codon[..3]
            .try_into()
            .map(|codon: [u8; 3]| codon.map(|base| base.to_ascii_uppercase()))
        {
            Ok([b'A', b'T', b'A']) => b'M',
            Ok([b'T', b'G', b'A']) => b'W',
            Ok([b'A', b'G', b'A' | b'G']) => b'*',
            _ => standard,
        }
    } else {
        standard
    }
}

#[allow(clippy::too_many_arguments)]
fn add_bisulfite_marks(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    context: BisulfiteContext,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    shade_by_quality: bool,
    budget: &mut BisulfiteRenderBudget,
) {
    let Some(SequenceView::Present { bases, qualities }) = query.batch.sequence(index) else {
        return;
    };
    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let Some((base, quality)) = oriented_base(
            query.batch.flags[index],
            bases,
            qualities,
            read_start.saturating_add(offset),
        ) else {
            continue;
        };
        let Some(observation) = bisulfite_observation(query, index, context, position, base) else {
            continue;
        };
        if !budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        let alpha = if shade_by_quality {
            quality.map_or(0.78, |value| {
                (0.40 + f32::from(value.min(40)) / 40.0 * 0.58).clamp(0.40, 0.98)
            })
        } else {
            0.98
        };
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.25),
            height,
            color: with_alpha(bisulfite_color(observation), alpha),
            object_id: read_detail_object_id(object_id),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn add_smrt_kinetics_marks(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    series: SmrtKineticsSeries<'_>,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    budget: &mut SmrtKineticsRenderBudget,
) {
    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let Some(value) = series.value_at_aligned_read_position(
            query.batch.flags[index],
            read_start.saturating_add(offset),
        ) else {
            continue;
        };
        if !budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.25),
            height,
            color: smrt_kinetics_color(value),
            object_id: read_detail_object_id(object_id),
        });
    }
}

fn sbx_visible_read_range(
    state: Option<SbxRenderState>,
    read_start: usize,
    length: usize,
) -> Option<(usize, usize)> {
    let read_end = read_start.checked_add(length)?;
    let Some(state) = state else {
        return (read_start < read_end).then_some((read_start, read_end));
    };
    if state.options.simplex_tail_mode != SbxSimplexTailMode::Hide {
        return (read_start < read_end).then_some((read_start, read_end));
    }
    let visible_start = read_start.max(state.tails.retained_start);
    let visible_end = read_end.min(state.tails.retained_end);
    (visible_start < visible_end).then_some((visible_start, visible_end))
}

fn sbx_hides_read_position(state: Option<SbxRenderState>, read_position: usize) -> bool {
    state.is_some_and(|state| {
        state.options.simplex_tail_mode == SbxSimplexTailMode::Hide
            && state.tails.is_tail(read_position)
    })
}

fn sbx_retained_reference_span(
    alignment_start: u64,
    cigar: &[CigarOp],
    tails: SbxTailBounds,
) -> Option<(u64, u64)> {
    if tails.all_tail() {
        return None;
    }
    let start = reference_position_for_read_position(alignment_start, cigar, tails.retained_start)?;
    let final_read_position = tails.retained_end.checked_sub(1)?;
    let end = reference_position_for_read_position(alignment_start, cigar, final_read_position)?
        .saturating_add(1);
    (start < end).then_some((start, end))
}

#[allow(clippy::too_many_arguments)]
fn add_sbx_tail_marks(
    display_list: &mut DisplayList,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    state: SbxRenderState,
    budget: &mut SbxRenderBudget,
) {
    let read_end = read_start.saturating_add(length);
    for (tail_start, tail_end) in [
        (0, state.tails.retained_start),
        (state.tails.retained_end, state.tails.read_length),
    ] {
        let visible_start = read_start.max(tail_start);
        let visible_end = read_end.min(tail_end);
        if visible_start >= visible_end {
            continue;
        }
        let genomic_start = reference_start.saturating_add((visible_start - read_start) as u64);
        let genomic_end = reference_start.saturating_add((visible_end - read_start) as u64);
        let (x, width) = clipped_geometry(genomic_start, genomic_end, viewport);
        if width > 0.0 && budget.take() {
            display_list.rectangles.push(RectPrimitive {
                x,
                y,
                width: width.max(1.0),
                height,
                color: sbx_simplex_tail_color(),
                object_id: read_detail_object_id(object_id),
            });
        }
    }
}

fn sbx_insertion_color(
    query: &AlignmentQuery,
    index: usize,
    enabled: bool,
    state: Option<SbxRenderState>,
    insertion_start: usize,
    insertion_length: usize,
    budget: &mut SbxRenderBudget,
) -> Rgba {
    let concordant = Rgba::new(0.86, 0.32, 0.82, 0.96);
    if !enabled {
        return concordant;
    }
    let Some(state) = state else {
        budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
        return concordant;
    };
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = query.batch.sequence(index)
    else {
        budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
        return concordant;
    };
    match classify_sbx_insertion(
        query.batch.flags[index],
        bases,
        qualities,
        insertion_start,
        insertion_length,
        state.options.discordant_quality_maximum,
    ) {
        Ok(SbxInsertionState::Concordant) => concordant,
        Ok(SbxInsertionState::PartlyDiscordant) => Rgba::new(0.55, 0.55, 0.55, 0.98),
        Ok(SbxInsertionState::FullyDiscordant) => Rgba::new(0.71, 0.71, 0.71, 0.98),
        Err(_) => {
            budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
            concordant
        }
    }
}

const fn sbx_simplex_tail_color() -> Rgba {
    Rgba::new(0.59, 0.20, 0.78, 0.28)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn add_aligned_bases(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    cigar_kind: CigarOpKind,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    color_mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
    base_display_mode: ReadBaseDisplayMode,
    shade_by_quality: bool,
    bisulfite_budget: &mut BisulfiteRenderBudget,
    smrt_series: Option<SmrtKineticsSeries<'_>>,
    smrt_kinetics_budget: &mut SmrtKineticsRenderBudget,
    sbx_state: Option<SbxRenderState>,
    sbx_budget: &mut SbxRenderBudget,
) {
    let Some(sequence) = query.batch.sequence(index) else {
        add_match_segment(
            display_list,
            query,
            index,
            cigar_kind,
            reference_start,
            reference_start.saturating_add(length as u64),
            viewport,
            y,
            height,
            object_id,
            color_mode,
            pair_length_bounds,
        );
        return;
    };
    let SequenceView::Present { bases, qualities } = sequence else {
        add_match_segment(
            display_list,
            query,
            index,
            cigar_kind,
            reference_start,
            reference_start.saturating_add(length as u64),
            viewport,
            y,
            height,
            object_id,
            color_mode,
            pair_length_bounds,
        );
        return;
    };

    if let Some((visible_start, visible_end)) =
        sbx_visible_read_range(sbx_state, read_start, length)
    {
        let genomic_start = reference_start.saturating_add((visible_start - read_start) as u64);
        let genomic_end = reference_start.saturating_add((visible_end - read_start) as u64);
        let (segment_x, segment_width) = clipped_geometry(genomic_start, genomic_end, viewport);
        if segment_width > 0.0 {
            add_read_body(
                display_list,
                segment_x,
                y,
                segment_width.max(1.0),
                height,
                alignment_color(query, index, color_mode, pair_length_bounds, 0.88),
                object_id,
            );
        }
    }

    let highlights_sbx_tails = sbx_state
        .is_some_and(|state| state.options.simplex_tail_mode == SbxSimplexTailMode::Highlight);
    if base_display_mode == ReadBaseDisplayMode::Hidden
        && !matches!(
            color_mode,
            AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::SmrtKinetics(_)
        )
        && !highlights_sbx_tails
    {
        return;
    }

    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let aligned_read_position = read_start.saturating_add(offset);
        if sbx_hides_read_position(sbx_state, aligned_read_position) {
            continue;
        }
        let Some((base, quality)) = oriented_base(
            query.batch.flags[index],
            bases,
            qualities,
            aligned_read_position,
        ) else {
            continue;
        };
        let reference_base = query
            .reference
            .as_ref()
            .and_then(|value| value.base_at(position));
        let mismatch = match cigar_kind {
            CigarOpKind::SequenceMismatch => true,
            CigarOpKind::Match => {
                reference_base.is_some_and(|reference| !base.eq_ignore_ascii_case(&reference))
            }
            _ => false,
        };
        let bisulfite = match color_mode {
            AlignmentColorMode::Bisulfite(context) => {
                bisulfite_observation(query, index, context, position, base)
            }
            _ => None,
        };
        let smrt_kinetics = smrt_series.and_then(|series| {
            series.value_at_aligned_read_position(query.batch.flags[index], aligned_read_position)
        });
        let sbx_tail = highlights_sbx_tails
            && sbx_state.is_some_and(|state| state.tails.is_tail(aligned_read_position));
        if bisulfite.is_none()
            && smrt_kinetics.is_none()
            && !sbx_tail
            && (base_display_mode == ReadBaseDisplayMode::Hidden
                || (base_display_mode == ReadBaseDisplayMode::Mismatches && !mismatch))
        {
            continue;
        }
        if bisulfite.is_some() && !bisulfite_budget.take() {
            continue;
        }
        if smrt_kinetics.is_some() && !smrt_kinetics_budget.take() {
            continue;
        }
        if sbx_tail && !sbx_budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        let quality_alpha = if shade_by_quality {
            quality.map_or(0.78, |value| {
                (0.40 + f32::from(value.min(40)) / 40.0 * 0.58).clamp(0.40, 0.98)
            })
        } else {
            0.98
        };
        let color = if sbx_tail {
            sbx_simplex_tail_color()
        } else {
            smrt_kinetics.map_or_else(
                || {
                    with_alpha(
                        bisulfite.map_or_else(|| base_color(base), bisulfite_color),
                        quality_alpha,
                    )
                },
                smrt_kinetics_color,
            )
        };
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.0),
            height,
            color,
            object_id: read_detail_object_id(object_id),
        });
        if width >= 7.0 {
            add_base_glyph(
                display_list,
                base,
                x,
                y + 1.0,
                width,
                (height - 2.0).max(5.0),
                if bisulfite.is_some() || smrt_kinetics.is_some() || sbx_tail {
                    fill_label_color(color)
                } else {
                    read_base_label_color(base)
                },
            );
        }
    }
}

const fn bisulfite_color(observation: BisulfiteObservation) -> Rgba {
    match observation {
        BisulfiteObservation::Protected => Rgba::new(0.86, 0.12, 0.12, 1.0),
        BisulfiteObservation::Converted => Rgba::new(0.12, 0.34, 0.88, 1.0),
    }
}

fn smrt_kinetics_color(value: u64) -> Rgba {
    let value = value.min(6_000) as f32;
    if value <= 100.0 {
        let alpha = 0.08 + value / 100.0 * 0.90;
        Rgba::new(0.94, 0.08, 0.06, alpha)
    } else if value <= 600.0 {
        let fraction = (value - 100.0) / 500.0;
        Rgba::new(0.96, fraction * 0.94, 0.04, 0.98)
    } else {
        let fraction = (value - 600.0) / 5_400.0;
        Rgba::new(0.96 * (1.0 - fraction), 0.94, fraction * 0.94, 0.98)
    }
}

fn bisulfite_observation(
    query: &AlignmentQuery,
    index: usize,
    context: BisulfiteContext,
    position: u64,
    observed_base: u8,
) -> Option<BisulfiteObservation> {
    let reference = query.reference.as_ref()?;
    classify_bisulfite_observation(
        reference,
        position,
        bisulfite_strand_is_reverse(query.batch.flags[index]),
        context,
        observed_base,
    )
}

fn oriented_base(
    flags: AlignmentFlags,
    bases: &[u8],
    qualities: Option<&[u8]>,
    aligned_position: usize,
) -> Option<(u8, Option<u8>)> {
    if flags.is_reverse() {
        let index = bases
            .len()
            .checked_sub(aligned_position.saturating_add(1))?;
        Some((
            complement(*bases.get(index)?),
            qualities.and_then(|values| values.get(index)).copied(),
        ))
    } else {
        Some((
            *bases.get(aligned_position)?,
            qualities
                .and_then(|values| values.get(aligned_position))
                .copied(),
        ))
    }
}

fn visible_positions(
    interval: rgv_core::GenomicInterval,
    viewport: Viewport,
) -> std::ops::Range<u64> {
    let start = interval.start().max(viewport.interval().start());
    let end = interval.end().min(viewport.interval().end());
    start..end
}

fn complement(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' | b'U' => b'A',
        other => other,
    }
}

fn base_color(base: u8) -> Rgba {
    match base.to_ascii_uppercase() {
        b'A' => Rgba::new(0.24, 0.78, 0.38, 1.0),
        b'C' => Rgba::new(0.22, 0.52, 0.96, 1.0),
        b'G' => Rgba::new(0.96, 0.65, 0.18, 1.0),
        b'T' | b'U' => Rgba::new(0.94, 0.28, 0.28, 1.0),
        _ => Rgba::new(0.60, 0.64, 0.70, 1.0),
    }
}

fn reference_base_label_color(base: u8) -> Rgba {
    let color = base_color(base);
    Rgba::new(color.red * 0.5, color.green * 0.5, color.blue * 0.5, 1.0)
}

fn read_base_label_color(base: u8) -> Rgba {
    let color = base_color(base);
    fill_label_color(color)
}

fn fill_label_color(color: Rgba) -> Rgba {
    let luminance = color.red * 0.2126 + color.green * 0.7152 + color.blue * 0.0722;
    if luminance >= 0.55 {
        Rgba::new(0.06, 0.08, 0.10, 1.0)
    } else {
        Rgba::new(0.98, 0.99, 1.0, 1.0)
    }
}

const fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba::new(color.red, color.green, color.blue, alpha)
}

fn add_base_glyph(
    display_list: &mut DisplayList,
    base: u8,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Rgba,
) {
    display_list.texts.push(TextPrimitive {
        x: x + width * 0.5,
        y: y + height * 0.5,
        text: char::from(base.to_ascii_uppercase()).to_string(),
        font_size: (height * 0.90).clamp(10.0, 16.0),
        color,
    });
}

fn add_read_body(
    display_list: &mut DisplayList,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    fill: Rgba,
    object_id: u32,
) {
    display_list.rectangles.push(RectPrimitive {
        x,
        y,
        width,
        height,
        color: Rgba::new(0.60, 0.62, 0.65, 0.92),
        object_id,
    });
    let inset = 0.8;
    if width > inset * 2.0 && height > inset * 2.0 {
        display_list.rectangles.push(RectPrimitive {
            x: x + inset,
            y: y + inset,
            width: width - inset * 2.0,
            height: height - inset * 2.0,
            color: fill,
            object_id,
        });
    }
}

fn add_event_label(
    display_list: &mut DisplayList,
    label: &str,
    center_x: f32,
    center_y: f32,
    color: Rgba,
    _object_id: u32,
    budget: &mut usize,
) {
    if *budget == 0 || label.is_empty() {
        return;
    }
    *budget -= 1;
    let text = label.chars().take(12).collect::<String>();
    let width = text.chars().count() as f32 * 5.5;
    let left = center_x - width * 0.5;
    let top = center_y - 6.0;
    display_list.rectangles.push(RectPrimitive {
        x: left - 2.0,
        y: top,
        width: width + 4.0,
        height: 12.0,
        color: Rgba::new(1.0, 1.0, 1.0, 0.96),
        object_id: 0,
    });
    display_list.texts.push(TextPrimitive {
        x: center_x,
        y: center_y,
        text,
        font_size: 9.0,
        color,
    });
}

fn add_marker(
    display_list: &mut DisplayList,
    viewport: Viewport,
    reference_position: u64,
    y: f32,
    height: f32,
    color: Rgba,
    object_id: u32,
) {
    let x = viewport.pixel_at_genomic(reference_position) as f32;
    if x >= -2.0 && x <= viewport.pixel_width() as f32 + 2.0 {
        display_list.rectangles.push(RectPrimitive {
            x: x - 1.0,
            y: y - 1.0,
            width: 2.0,
            height: height + 2.0,
            color,
            object_id: read_detail_object_id(object_id),
        });
    }
}

fn add_ultima_insertion_quality_mark(
    display_list: &mut DisplayList,
    viewport: Viewport,
    reference_position: u64,
    y: f32,
    height: f32,
    quality: f32,
    object_id: u32,
) {
    let x = viewport.pixel_at_genomic(reference_position) as f32;
    if x < -3.0 || x > viewport.pixel_width() as f32 + 3.0 {
        return;
    }
    let stroke = 2.0_f32.min(height.max(1.0));
    let width = if height > 10.0 {
        6.0
    } else if height > 5.0 {
        4.0
    } else {
        2.0
    };
    let fraction = 1.0 - quality.clamp(0.0, 42.0) / 42.0;
    display_list.rectangles.push(RectPrimitive {
        x: x - width * 0.5,
        y: y + (height - stroke).max(0.0) * fraction,
        width,
        height: stroke,
        color: ultima_quality_color(quality),
        object_id: read_detail_object_id(object_id),
    });
}

#[allow(clippy::too_many_arguments)]
fn add_ultima_deletion_quality_mark(
    display_list: &mut DisplayList,
    x: f32,
    width: f32,
    y: f32,
    height: f32,
    quality: f32,
    object_id: u32,
) {
    let stroke = 2.0_f32.min(width.max(1.0));
    let fraction = quality.clamp(0.0, 42.0) / 42.0;
    display_list.rectangles.push(RectPrimitive {
        x: x + width * fraction - stroke * 0.5,
        y,
        width: stroke,
        height,
        color: ultima_quality_color(quality),
        object_id: read_detail_object_id(object_id),
    });
}

/// IGV's 44-entry Ultima jet scale, reproduced with the same integer arithmetic.
pub fn ultima_quality_color(quality: f32) -> Rgba {
    const SIZE: usize = 44;
    let index = quality.clamp(0.0, (SIZE - 1) as f32) as usize;
    let green = ultima_jet_green(index);
    let blue = if index >= SIZE * 7 / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE / 4) % SIZE)
    };
    let red = if index < SIZE / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE * 6 / 8) % SIZE)
    };
    Rgba::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        1.0,
    )
}

fn ultima_jet_green(index: usize) -> u8 {
    const SIZE: usize = 44;
    if !(SIZE / 8..SIZE * 7 / 8).contains(&index) {
        0
    } else if index < SIZE * 3 / 8 {
        u8::try_from(255 * (index - SIZE / 8) * 4 / SIZE).unwrap_or(u8::MAX)
    } else if index < SIZE * 5 / 8 {
        u8::MAX
    } else {
        u8::try_from(255 - 255 * (index - SIZE * 5 / 8) * 4 / SIZE).unwrap_or_default()
    }
}

fn clipped_geometry(start: u64, end: u64, viewport: Viewport) -> (f32, f32) {
    let interval = viewport.interval();
    let start = start.max(interval.start()).min(interval.end());
    let end = end.max(interval.start()).min(interval.end());
    if start >= end {
        return (0.0, 0.0);
    }
    let x = viewport.pixel_at_genomic(start) as f32;
    let right = viewport.pixel_at_genomic(end) as f32;
    (x, right - x)
}

#[allow(clippy::too_many_lines)]
fn alignment_color(
    query: &AlignmentQuery,
    index: usize,
    mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
    alpha: f32,
) -> Rgba {
    let batch = &query.batch;
    let mapq_fraction = batch.mapping_qualities[index]
        .map_or(0.45, |value| (f32::from(value) / 60.0).clamp(0.2, 1.0));
    match mode {
        AlignmentColorMode::None => {
            let grey = 0.82 + mapq_fraction * 0.12;
            Rgba::new(grey, grey, grey, alpha.max(0.92))
        }
        AlignmentColorMode::Strand => {
            if batch.flags[index].is_reverse() {
                Rgba::new(0.58, 0.36, 0.67, alpha)
            } else {
                Rgba::new(0.92, 0.55, 0.64, alpha)
            }
        }
        AlignmentColorMode::MappingQuality => Rgba::new(
            0.22 + mapq_fraction * 0.58,
            0.28 + mapq_fraction * 0.62,
            0.34 + mapq_fraction * 0.56,
            alpha,
        ),
        AlignmentColorMode::ReadGroup => record_tag_label(query, index, *b"RG")
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
        AlignmentColorMode::Haplotype => record_tag_label(query, index, *b"HP")
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
        AlignmentColorMode::TemplateLength => {
            let Some(pair) = pair_evidence_at(batch, index, query.interval, pair_length_bounds)
            else {
                return Rgba::new(0.56, 0.60, 0.68, alpha);
            };
            if matches!(
                pair.classification,
                PairClassification::DiscordantTemplateLengthShort
                    | PairClassification::DiscordantTemplateLengthLong
            ) {
                Rgba::new(0.90, 0.27, 0.22, alpha)
            } else if let (Some(minimum), Some(maximum)) =
                (pair_length_bounds.minimum, pair_length_bounds.maximum)
            {
                let span = maximum.saturating_sub(minimum).max(1) as f32;
                let fraction = (pair.absolute_template_length.saturating_sub(minimum) as f32
                    / span)
                    .clamp(0.0, 1.0);
                Rgba::new(
                    0.20 + fraction * 0.48,
                    0.68 - fraction * 0.22,
                    0.82 - fraction * 0.18,
                    alpha,
                )
            } else {
                Rgba::new(0.56, 0.60, 0.68, alpha)
            }
        }
        AlignmentColorMode::PairOrientation => {
            match pair_evidence_at(batch, index, query.interval, pair_length_bounds)
                .map(|pair| pair.orientation)
            {
                Some(PairOrientation::Inward) => Rgba::new(0.35, 0.68, 0.48, alpha),
                Some(PairOrientation::Outward) => Rgba::new(0.91, 0.45, 0.18, alpha),
                Some(PairOrientation::SameStrand) => Rgba::new(0.62, 0.36, 0.76, alpha),
                Some(PairOrientation::Unknown) | None => Rgba::new(0.56, 0.60, 0.68, alpha),
            }
        }
        AlignmentColorMode::UnexpectedPair => {
            match pair_evidence_at(batch, index, query.interval, pair_length_bounds)
                .map(|pair| pair.classification)
            {
                Some(PairClassification::Proper | PairClassification::Concordant) => {
                    Rgba::new(0.72, 0.76, 0.80, alpha)
                }
                Some(PairClassification::Unpaired) | None => Rgba::new(0.56, 0.60, 0.68, alpha),
                Some(_) => Rgba::new(0.91, 0.29, 0.24, alpha),
            }
        }
        AlignmentColorMode::MateContig => batch.mate_contigs[index]
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |contig| {
                palette_color(&format!("tid {}", contig.get()), alpha)
            }),
        AlignmentColorMode::FirstOfPairStrand => {
            let flags = batch.flags[index];
            let reverse = if flags.is_first_in_template() {
                Some(flags.is_reverse())
            } else if flags.is_last_in_template() {
                Some(flags.is_mate_reverse())
            } else {
                None
            };
            match reverse {
                Some(true) => Rgba::new(0.58, 0.36, 0.67, alpha),
                Some(false) => Rgba::new(0.92, 0.55, 0.64, alpha),
                None => Rgba::new(0.56, 0.60, 0.68, alpha),
            }
        }
        AlignmentColorMode::Bisulfite(_) => {
            let reverse = first_of_pair_reverse(batch.flags[index])
                .unwrap_or_else(|| batch.flags[index].is_reverse());
            if reverse {
                // A muted sage body keeps the protected/converted red and blue marks dominant.
                Rgba::new(0.68, 0.75, 0.62, alpha.max(0.92))
            } else {
                Rgba::new(0.82, 0.82, 0.82, alpha.max(0.92))
            }
        }
        AlignmentColorMode::SmrtKinetics(_) => {
            // Keep missing and low-value kinetic observations visibly attached to the read.
            Rgba::new(0.80, 0.82, 0.84, alpha.max(0.92))
        }
        AlignmentColorMode::Ultima(_) | AlignmentColorMode::Sbx(_) => {
            Rgba::new(0.82, 0.83, 0.85, alpha.max(0.92))
        }
        AlignmentColorMode::Tag(tag) => record_tag_label(query, index, tag)
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
    }
}

fn record_tag_label(query: &AlignmentQuery, index: usize, tag: [u8; 2]) -> Option<String> {
    let field = query.batch.auxiliary_field(index, tag).ok()??;
    let value = &field.value;
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

fn palette_color(label: &str, alpha: f32) -> Rgba {
    const PALETTE: [[f32; 3]; 8] = [
        [0.26, 0.70, 0.94],
        [0.94, 0.40, 0.42],
        [0.42, 0.82, 0.48],
        [0.80, 0.48, 0.94],
        [0.96, 0.68, 0.24],
        [0.24, 0.78, 0.76],
        [0.94, 0.42, 0.74],
        [0.66, 0.72, 0.30],
    ];
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in label.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let color = PALETTE[hash as usize % PALETTE.len()];
    Rgba::new(color[0], color[1], color[2], alpha)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{READ_DETAIL_OBJECT_BIT, record_object_id};
    use rgv_core::{ContigId, GenomicInterval};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentHeaderMetadata, AlignmentReadGroup,
        AlignmentRecord, AlleleSupport, AuxField, AuxValue, BaseModificationStrand, CigarOp,
        ModificationProvenance, ModificationSeries, ModificationSeriesKey, QueryStats,
        ReferenceSlice, SequenceData, bisulfite_context_matches,
    };

    use super::*;

    fn row_test_record(name: &[u8], start: u64, end: u64) -> AlignmentRecord {
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
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: u32::try_from(end - start).unwrap(),
            }],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    fn row_test_query(records: &[AlignmentRecord]) -> AlignmentQuery {
        let mut batch = AlignmentBatch::default();
        for record in records {
            batch.push(record).unwrap();
        }
        AlignmentQuery {
            interval: GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: records.len(),
                records_eligible: records.len(),
                records_emitted: records.len(),
                ..QueryStats::default()
            },
        }
    }

    fn reverse_complement(bases: &[u8]) -> Vec<u8> {
        bases.iter().rev().copied().map(complement).collect()
    }

    #[test]
    fn bisulfite_contexts_match_both_reference_strands_exactly() {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 20).unwrap();
        let reference = ReferenceSlice {
            interval,
            bases: b"ACGACAGACATAGCATAAAA".to_vec(),
        };

        assert!(bisulfite_context_matches(
            &reference,
            1,
            false,
            BisulfiteContext::Cg
        ));
        assert!(bisulfite_context_matches(
            &reference,
            4,
            false,
            BisulfiteContext::Chg
        ));
        assert!(bisulfite_context_matches(
            &reference,
            8,
            false,
            BisulfiteContext::Chh
        ));
        assert!(bisulfite_context_matches(
            &reference,
            1,
            false,
            BisulfiteContext::Hcg
        ));
        assert!(bisulfite_context_matches(
            &reference,
            13,
            false,
            BisulfiteContext::Gch
        ));
        assert!(bisulfite_context_matches(
            &reference,
            1,
            false,
            BisulfiteContext::Wcg
        ));
        // The G paired with the first forward-strand C is a reverse-strand CG cytosine.
        assert!(bisulfite_context_matches(
            &reference,
            2,
            true,
            BisulfiteContext::Cg
        ));
        assert!(!bisulfite_context_matches(
            &reference,
            4,
            false,
            BisulfiteContext::Cg
        ));
    }

    #[test]
    fn bisulfite_observations_distinguish_protected_and_converted_on_both_strands() {
        let reference_bases = b"ACGTCGAA";
        let mut forward = row_test_record(b"forward", 0, reference_bases.len() as u64);
        let mut forward_bases = reference_bases.to_vec();
        forward_bases[4] = b'T';
        forward.sequence = SequenceData::Present {
            bases: forward_bases,
            qualities: None,
        };
        let mut reverse = row_test_record(b"reverse", 0, reference_bases.len() as u64);
        reverse.flags = AlignmentFlags::from_bits(0x10);
        let mut reverse_display = reference_bases.to_vec();
        reverse_display[5] = b'A';
        reverse.sequence = SequenceData::Present {
            bases: reverse_complement(&reverse_display),
            qualities: None,
        };
        let mut query = row_test_query(&[forward, reverse]);
        query.reference = Some(ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64)
                .unwrap(),
            bases: reference_bases.to_vec(),
        });

        assert_eq!(
            bisulfite_observation(&query, 0, BisulfiteContext::Cg, 1, b'C'),
            Some(BisulfiteObservation::Protected)
        );
        assert_eq!(
            bisulfite_observation(&query, 0, BisulfiteContext::Cg, 4, b'T'),
            Some(BisulfiteObservation::Converted)
        );
        assert_eq!(
            bisulfite_observation(&query, 1, BisulfiteContext::Cg, 2, b'G'),
            Some(BisulfiteObservation::Protected)
        );
        assert_eq!(
            bisulfite_observation(&query, 1, BisulfiteContext::Cg, 5, b'A'),
            Some(BisulfiteObservation::Converted)
        );
        assert_eq!(
            bisulfite_observation(&query, 0, BisulfiteContext::Cg, 3, b'T'),
            None
        );
    }

    #[test]
    fn bisulfite_render_budget_records_every_omitted_mark() {
        let mut budget = BisulfiteRenderBudget::new(1);
        assert!(budget.take());
        assert!(!budget.take());
        assert!(!budget.take());
        assert_eq!(budget.omitted, 2);
    }

    #[test]
    fn bisulfite_marks_remain_visible_across_base_and_read_detail_levels() {
        let reference_bases = b"ACGTCGAA";
        let mut record = row_test_record(b"converted", 0, reference_bases.len() as u64);
        let mut observed = reference_bases.to_vec();
        observed[4] = b'T';
        record.sequence = SequenceData::Present {
            bases: observed,
            qualities: None,
        };
        let mut query = row_test_query(&[record]);
        query.reference = Some(ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64)
                .unwrap(),
            bases: reference_bases.to_vec(),
        });
        let options = AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        };
        let rows = assign_alignment_rows(&query, options, None);
        for viewport in [
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap(),
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
                500.0,
            )
            .unwrap(),
        ] {
            let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
            let protected = layout
                .display_list
                .rectangles
                .iter()
                .filter(|rectangle| {
                    (rectangle.color.red - 0.86).abs() < 1.0e-6
                        && (rectangle.color.green - 0.12).abs() < 1.0e-6
                })
                .count();
            let converted = layout
                .display_list
                .rectangles
                .iter()
                .filter(|rectangle| {
                    (rectangle.color.red - 0.12).abs() < 1.0e-6
                        && (rectangle.color.blue - 0.88).abs() < 1.0e-6
                })
                .count();
            assert_eq!((protected, converted), (1, 1));
            assert_eq!(layout.stats.bisulfite_marks_omitted, 0);
        }
    }

    #[test]
    fn smrt_kinetics_marks_remain_visible_across_base_and_read_detail_levels() {
        let mut record = row_test_record(b"smrt", 0, 4);
        record.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: None,
        };
        record.auxiliary_fields.push(AuxField {
            tag: *b"ip",
            value: AuxValue::UnsignedArray(vec![0, 100, 600, 6_000]),
        });
        let query = row_test_query(&[record]);
        let options = AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        };
        let rows = assign_alignment_rows(&query, options, None);
        for viewport in [
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap(),
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
                100.0,
            )
            .unwrap(),
        ] {
            let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
            for expected in [0_u64, 100, 600, 6_000].map(smrt_kinetics_color) {
                assert!(layout.display_list.rectangles.iter().any(|rectangle| {
                    (rectangle.color.red - expected.red).abs() < 1.0e-6
                        && (rectangle.color.green - expected.green).abs() < 1.0e-6
                        && (rectangle.color.blue - expected.blue).abs() < 1.0e-6
                        && (rectangle.color.alpha - expected.alpha).abs() < 1.0e-6
                }));
            }
            assert_eq!(layout.stats.smrt_kinetics_marks_omitted, 0);
            assert_eq!(layout.stats.smrt_kinetics_unavailable_records, 0);
            assert_eq!(layout.stats.smrt_kinetics_malformed_records, 0);
        }
    }

    #[test]
    fn smrt_kinetics_missing_and_malformed_records_remain_visible_and_reported() {
        let mut missing = row_test_record(b"missing", 0, 4);
        missing.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: None,
        };
        let mut malformed = row_test_record(b"malformed", 0, 4);
        malformed.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: None,
        };
        malformed.auxiliary_fields.push(AuxField {
            tag: *b"ip",
            value: AuxValue::UnsignedArray(vec![1, 2, 3]),
        });
        let query = row_test_query(&[missing, malformed]);
        let options = AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
            ..AlignmentLayoutOptions::default()
        };
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
        let layout = layout_alignment_query(&query, viewport, options);
        assert_eq!(layout.stats.rendered_alignments, 2);
        assert_eq!(layout.stats.smrt_kinetics_unavailable_records, 1);
        assert_eq!(layout.stats.smrt_kinetics_malformed_records, 1);
    }

    #[test]
    fn smrt_kinetics_palette_and_render_budget_are_bounded() {
        assert_eq!(smrt_kinetics_color(6_000), smrt_kinetics_color(u64::MAX));
        assert!(smrt_kinetics_color(100).red > smrt_kinetics_color(100).green);
        assert!(smrt_kinetics_color(600).green > 0.9);
        assert!(smrt_kinetics_color(6_000).blue > 0.9);

        let mut budget = SmrtKineticsRenderBudget::new(1);
        assert!(budget.take());
        assert!(!budget.take());
        assert!(!budget.take());
        assert_eq!(budget.omitted, 2);
    }

    fn ultima_insertion_record() -> AlignmentRecord {
        let mut record = row_test_record(b"ultima-insertion", 0, 6);
        record.cigar = vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 3,
            },
            CigarOp {
                kind: CigarOpKind::Insertion,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 3,
            },
        ];
        record.sequence = SequenceData::Present {
            bases: b"ACGAAATC".to_vec(),
            qualities: Some(vec![30, 30, 30, 30, 40, 30, 30, 30]),
        };
        record.auxiliary_fields.push(AuxField {
            tag: *b"tp",
            value: AuxValue::SignedArray(vec![0, 0, 0, 0, -2, 0, 0, 0]),
        });
        record
    }

    fn ultima_layout_options(options: UltimaDisplayOptions) -> AlignmentLayoutOptions {
        AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Ultima(options),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        }
    }

    #[test]
    fn ultima_insertion_quality_is_scale_stable_at_read_and_base_detail() {
        let query = row_test_query(&[ultima_insertion_record()]);
        let options = ultima_layout_options(UltimaDisplayOptions::default());
        let rows = assign_alignment_rows(&query, options, None);
        let expected = ultima_quality_color(40.0);
        for viewport in [
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
                200.0,
            )
            .unwrap(),
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
                100.0,
            )
            .unwrap(),
        ] {
            let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
            let mark = layout
                .display_list
                .rectangles
                .iter()
                .find(|rectangle| rectangle.color == expected)
                .expect("Ultima insertion quality mark");
            assert_eq!(
                mark.object_id & READ_DETAIL_OBJECT_BIT,
                READ_DETAIL_OBJECT_BIT
            );
            assert!(mark.width <= 6.0);
            assert!(mark.height <= 2.0);
            assert_eq!(layout.stats.ultima_quality_marks_omitted, 0);
            assert_eq!(layout.stats.ultima_unavailable_records, 0);
            assert_eq!(layout.stats.ultima_malformed_records, 0);
            assert_eq!(layout.stats.ultima_unavailable_events, 0);
        }
    }

    #[test]
    fn ultima_deletion_uses_read_group_maximum_homopolymer_length() {
        let mut record = row_test_record(b"ultima-deletion", 0, 8);
        record.cigar = vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 3,
            },
            CigarOp {
                kind: CigarOpKind::Deletion,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 3,
            },
        ];
        record.sequence = SequenceData::Present {
            bases: b"ACGAAA".to_vec(),
            qualities: Some(vec![30, 30, 30, 30, 38, 30]),
        };
        record.auxiliary_fields.extend([
            AuxField {
                tag: *b"tp",
                value: AuxValue::SignedArray(vec![0, 0, 0, 0, 2, 0]),
            },
            AuxField {
                tag: *b"RG",
                value: AuxValue::String("ultima".to_owned()),
            },
        ]);
        let mut query = row_test_query(&[record]);
        query.reference = Some(ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(),
            bases: b"ACGAAAAA".to_vec(),
        });
        query.header = Arc::new(AlignmentHeaderMetadata {
            read_groups: vec![AlignmentReadGroup {
                id: b"ultima".to_vec(),
                platform: Some("ULTIMA".to_owned()),
                maximum_homopolymer_length: Some(3),
            }],
            read_groups_omitted: 0,
        });
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 160.0).unwrap();
        let layout = layout_alignment_query(
            &query,
            viewport,
            ultima_layout_options(UltimaDisplayOptions::default()),
        );
        let mark = layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.color == ultima_quality_color(0.0))
            .expect("Q0 maximum-homopolymer deletion mark");
        assert!((mark.x - 60.0).abs() < 1.1);
        assert!((mark.height - AlignmentLayoutOptions::default().read_height).abs() < f32::EPSILON);
    }

    #[test]
    fn ultima_missing_and_malformed_evidence_leave_reads_visible() {
        let mut missing = ultima_insertion_record();
        missing.read_name = b"missing".to_vec();
        missing.auxiliary_fields.retain(|field| field.tag != *b"tp");
        let mut malformed = ultima_insertion_record();
        malformed.read_name = b"malformed".to_vec();
        malformed.auxiliary_fields[0].value = AuxValue::String("not-an-array".to_owned());
        let query = row_test_query(&[missing, malformed]);
        let viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
            200.0,
        )
        .unwrap();
        let layout = layout_alignment_query(
            &query,
            viewport,
            ultima_layout_options(UltimaDisplayOptions::default()),
        );
        assert_eq!(layout.stats.rendered_alignments, 2);
        assert_eq!(layout.stats.ultima_unavailable_records, 1);
        assert_eq!(layout.stats.ultima_malformed_records, 1);
        assert_eq!(layout.stats.ultima_quality_marks_omitted, 0);
    }

    #[test]
    fn ultima_palette_and_render_budget_match_the_bounded_jet_scale() {
        let rgb = |quality| {
            let color = ultima_quality_color(quality);
            [
                (color.red * 255.0).round() as u8,
                (color.green * 255.0).round() as u8,
                (color.blue * 255.0).round() as u8,
            ]
        };
        assert_eq!(rgb(0.0), [0, 0, 139]);
        assert_eq!(rgb(5.0), [0, 0, 255]);
        assert_eq!(rgb(16.0), [0, 255, 255]);
        assert_eq!(rgb(27.0), [255, 255, 0]);
        assert_eq!(rgb(38.0), [255, 0, 0]);
        assert_eq!(rgb(43.0), [140, 0, 0]);
        assert_eq!(ultima_quality_color(500.0), ultima_quality_color(43.0));

        let mut budget = UltimaRenderBudget::new(1);
        assert!(budget.take());
        assert!(!budget.take());
        assert!(!budget.take());
        assert_eq!(budget.omitted, 2);
    }

    fn sbx_insertion_record(name: &[u8], qualities: Vec<u8>) -> AlignmentRecord {
        let mut record = row_test_record(name, 0, 2);
        record.cigar = vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 1,
            },
            CigarOp {
                kind: CigarOpKind::Insertion,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 1,
            },
        ];
        record.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: Some(qualities),
        };
        record
    }

    #[test]
    fn sbx_insertion_colors_reach_the_display_list() {
        let query = row_test_query(&[
            sbx_insertion_record(b"full", vec![39, 5, 10, 39]),
            sbx_insertion_record(b"partial", vec![39, 39, 5, 39]),
            sbx_insertion_record(b"concordant", vec![39; 4]),
        ]);
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
        let layout = layout_alignment_query(
            &query,
            viewport,
            AlignmentLayoutOptions {
                reference_top: 0.0,
                coverage_top: 0.0,
                reads_top: 0.0,
                show_reference: false,
                show_coverage: false,
                show_junctions: false,
                show_modification_frequency: false,
                show_modified_bases: false,
                color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                    simplex_tail_mode: SbxSimplexTailMode::Off,
                    ..SbxDisplayOptions::default()
                }),
                read_base_display_mode: ReadBaseDisplayMode::Hidden,
                ..AlignmentLayoutOptions::default()
            },
        );
        for expected in [
            Rgba::new(0.71, 0.71, 0.71, 0.98),
            Rgba::new(0.55, 0.55, 0.55, 0.98),
            Rgba::new(0.86, 0.32, 0.82, 0.96),
        ] {
            assert!(
                layout
                    .display_list
                    .rectangles
                    .iter()
                    .any(|rectangle| rectangle.color == expected)
            );
        }
        assert_eq!(layout.stats.sbx_unavailable_records, 0);
        assert_eq!(layout.stats.sbx_unavailable_insertions, 0);
    }

    #[test]
    fn sbx_simplex_tails_highlight_at_read_and_base_detail() {
        let mut record = row_test_record(b"tails", 0, 8);
        record.sequence = SequenceData::Present {
            bases: b"ACGTACGT".to_vec(),
            qualities: Some(vec![20, 25, 39, 39, 39, 39, 18, 5]),
        };
        let query = row_test_query(&[record]);
        let options = AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        };
        for (viewport, expected_marks) in [
            (
                Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap(),
                4,
            ),
            (
                Viewport::new(
                    GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
                    100.0,
                )
                .unwrap(),
                2,
            ),
        ] {
            let layout = layout_alignment_query(&query, viewport, options);
            assert_eq!(
                layout
                    .display_list
                    .rectangles
                    .iter()
                    .filter(|rectangle| rectangle.color == sbx_simplex_tail_color())
                    .count(),
                expected_marks
            );
            assert_eq!(layout.stats.sbx_tail_marks_omitted, 0);
        }
    }

    #[test]
    fn sbx_hide_clips_tails_and_all_tail_reads_without_losing_record_status() {
        let mut partly_retained = row_test_record(b"partly-retained", 0, 8);
        partly_retained.sequence = SequenceData::Present {
            bases: b"ACGTACGT".to_vec(),
            qualities: Some(vec![20, 25, 39, 39, 39, 39, 18, 5]),
        };
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap();
        let options = AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: SbxSimplexTailMode::Hide,
                ..SbxDisplayOptions::default()
            }),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&row_test_query(&[partly_retained]), viewport, options);
        let outer_body = layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.object_id == 1)
            .unwrap();
        assert!((outer_body.x - 20.0).abs() < 1.0e-6);
        assert!((outer_body.width - 40.0).abs() < 1.0e-6);

        let mut all_tail = row_test_record(b"all-tail", 0, 8);
        all_tail.sequence = SequenceData::Present {
            bases: b"ACGTACGT".to_vec(),
            qualities: Some(vec![5; 8]),
        };
        let layout = layout_alignment_query(&row_test_query(&[all_tail]), viewport, options);
        assert_eq!(layout.stats.rendered_alignments, 1);
        assert!(layout.display_list.rectangles.is_empty());
    }

    #[test]
    fn sbx_hide_removes_reference_only_events_at_the_trimmed_left_boundary() {
        let mut record = row_test_record(b"left-tail-deletion", 0, 9);
        record.cigar = vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Deletion,
                len: 3,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            },
        ];
        record.sequence = SequenceData::Present {
            bases: b"ACGTAC".to_vec(),
            qualities: Some(vec![20, 20, 39, 39, 39, 39]),
        };
        let query = row_test_query(&[record]);
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 9).unwrap(), 90.0).unwrap();
        let layout = layout_alignment_query(
            &query,
            viewport,
            AlignmentLayoutOptions {
                reference_top: 0.0,
                coverage_top: 0.0,
                reads_top: 0.0,
                show_reference: false,
                show_coverage: false,
                show_junctions: false,
                show_modification_frequency: false,
                show_modified_bases: false,
                color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                    simplex_tail_mode: SbxSimplexTailMode::Hide,
                    ..SbxDisplayOptions::default()
                }),
                read_base_display_mode: ReadBaseDisplayMode::Hidden,
                ..AlignmentLayoutOptions::default()
            },
        );
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .all(|rectangle| { rectangle.color != Rgba::new(0.12, 0.12, 0.14, 0.96) })
        );
        let outer_body = layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.object_id == 1)
            .unwrap();
        assert!((outer_body.x - 50.0).abs() < 1.0e-6);
        assert!((outer_body.width - 40.0).abs() < 1.0e-6);
    }

    #[test]
    fn sbx_missing_qualities_leave_reads_visible_and_report_unclassified_insertions() {
        let mut record = sbx_insertion_record(b"missing-quality", vec![39; 4]);
        record.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: None,
        };
        let query = row_test_query(&[record]);
        let viewport =
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
        let layout = layout_alignment_query(
            &query,
            viewport,
            AlignmentLayoutOptions {
                reference_top: 0.0,
                coverage_top: 0.0,
                reads_top: 0.0,
                show_reference: false,
                show_coverage: false,
                show_junctions: false,
                show_modification_frequency: false,
                show_modified_bases: false,
                color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
                ..AlignmentLayoutOptions::default()
            },
        );
        assert_eq!(layout.stats.rendered_alignments, 1);
        assert!(!layout.display_list.rectangles.is_empty());
        assert_eq!(layout.stats.sbx_unavailable_records, 1);
        assert_eq!(layout.stats.sbx_unavailable_insertions, 1);
    }

    #[test]
    fn sbx_tail_render_budget_is_bounded_and_reports_omissions() {
        let mut budget = SbxRenderBudget::new(1);
        assert!(budget.take());
        assert!(!budget.take());
        assert!(!budget.take());
        assert_eq!(budget.omitted, 2);
    }

    #[test]
    fn stable_rows_survive_viewport_zoom_and_detail_transition() {
        let query = row_test_query(&[
            row_test_record(b"leading", 0, 70),
            row_test_record(b"retained", 50, 110),
        ]);
        let options = AlignmentLayoutOptions::default();
        let rows = assign_alignment_rows(&query, options, None);
        assert_eq!(rows.rows, vec![Some(0), Some(1)]);

        let broad = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
            500.0,
        )
        .unwrap();
        let narrow = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 71, 120).unwrap(),
            500.0,
        )
        .unwrap();
        assert_eq!(broad.detail_level(), DetailLevel::Reads);
        assert_eq!(narrow.detail_level(), DetailLevel::Bases);
        let broad_layout = layout_alignment_query_with_rows(&query, broad, options, &rows);
        let narrow_layout = layout_alignment_query_with_rows(&query, narrow, options, &rows);
        let read_y = |layout: &AlignmentLayout| {
            layout
                .display_list
                .rectangles
                .iter()
                .find(|primitive| primitive.object_id == 2)
                .map(|primitive| primitive.y)
                .unwrap()
        };
        assert!((read_y(&broad_layout) - read_y(&narrow_layout)).abs() < f32::EPSILON);
        assert!((read_y(&narrow_layout) - (options.reads_top + options.row_height)).abs() < 1.0e-6);
    }

    #[test]
    fn chimeric_grouping_and_clip_sorting_use_normalized_evidence() {
        let mut left = row_test_record(b"left", 10, 100);
        left.cigar = vec![
            CigarOp {
                kind: CigarOpKind::SoftClip,
                len: 10,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 90,
            },
        ];
        left.auxiliary_fields.push(AuxField {
            tag: *b"SA",
            value: AuxValue::String("chr2,101,+,100M,60,0;".to_owned()),
        });
        let mut more_clipped = row_test_record(b"more-clipped", 10, 100);
        more_clipped.cigar = vec![
            CigarOp {
                kind: CigarOpKind::SoftClip,
                len: 20,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 80,
            },
        ];
        more_clipped.flags = AlignmentFlags::from_bits(0x800);
        let query = row_test_query(&[left, more_clipped]);

        assert_eq!(
            grouped_indices(
                &query,
                AlignmentGroupMode::Chimeric,
                AlignmentSortMode::Start,
                AlignmentOrderDirection::Natural,
                None,
                TemplateLengthBounds::default(),
                None,
            )
            .len(),
            2
        );
        assert_eq!(
            grouped_indices(
                &query,
                AlignmentGroupMode::Supplementary,
                AlignmentSortMode::Start,
                AlignmentOrderDirection::Natural,
                None,
                TemplateLengthBounds::default(),
                None,
            )
            .len(),
            2
        );
        let rows = assign_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                sort_mode: AlignmentSortMode::LeftClip,
                ..AlignmentLayoutOptions::default()
            },
            None,
        );
        assert_eq!(rows.rows, vec![Some(1), Some(0)]);
    }

    #[test]
    fn group_and_sort_directions_are_independent_and_deterministic() {
        let forward = row_test_record(b"forward", 10, 30);
        let mut reverse = row_test_record(b"reverse", 10, 30);
        reverse.flags = AlignmentFlags::from_bits(0x10);
        let query = row_test_query(&[forward, reverse]);

        let natural = assign_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                group_mode: AlignmentGroupMode::Strand,
                max_rows: 4,
                ..AlignmentLayoutOptions::default()
            },
            None,
        );
        let reversed = assign_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                group_mode: AlignmentGroupMode::Strand,
                group_order: AlignmentOrderDirection::Reverse,
                max_rows: 4,
                ..AlignmentLayoutOptions::default()
            },
            Some(&natural),
        );

        assert!(natural.rows[0] < natural.rows[1]);
        assert!(reversed.rows[1] < reversed.rows[0]);

        let naturally_sorted = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::ReadName,
            AlignmentOrderDirection::Natural,
            None,
            TemplateLengthBounds::default(),
            None,
        );
        let reverse_sorted = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::ReadName,
            AlignmentOrderDirection::Reverse,
            None,
            TemplateLengthBounds::default(),
            None,
        );
        assert_eq!(naturally_sorted.values().next().unwrap(), &vec![0, 1]);
        assert_eq!(reverse_sorted.values().next().unwrap(), &vec![1, 0]);
    }

    #[test]
    fn selected_read_promotion_precedes_configured_sort_without_changing_identity() {
        let records = [
            row_test_record(b"alpha", 10, 30),
            row_test_record(b"selected", 10, 30),
            row_test_record(b"zeta", 10, 30),
        ];
        let query = row_test_query(&records);
        let promoted = alignment_read_name_hash(b"selected");
        let groups = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::ReadName,
            AlignmentOrderDirection::Reverse,
            None,
            TemplateLengthBounds::default(),
            Some(promoted),
        );
        assert_eq!(groups.values().next().unwrap(), &vec![1, 2, 0]);

        let ordinary = assign_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                sort_mode: AlignmentSortMode::ReadName,
                max_rows: 3,
                ..AlignmentLayoutOptions::default()
            },
            None,
        );
        let promoted_rows = assign_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                sort_mode: AlignmentSortMode::ReadName,
                promoted_read_name_hash: Some(promoted),
                max_rows: 3,
                ..AlignmentLayoutOptions::default()
            },
            Some(&ordinary),
        );
        assert_eq!(promoted_rows.rows[1], Some(0));
        assert_ne!(
            ordinary.promoted_read_name_hash,
            promoted_rows.promoted_read_name_hash
        );
        assert_eq!(ordinary.identities, promoted_rows.identities);
    }

    #[test]
    fn base_sort_uses_oriented_bases_and_explicit_gap_states() {
        let mut adenine = row_test_record(b"adenine", 10, 20);
        adenine.sequence = SequenceData::Present {
            bases: b"AAAAAAAAAA".to_vec(),
            qualities: None,
        };
        let mut reverse_cytosine = row_test_record(b"reverse-cytosine", 10, 20);
        reverse_cytosine.flags = AlignmentFlags::from_bits(0x10);
        reverse_cytosine.sequence = SequenceData::Present {
            // Reverse-orientation display complements stored G to C.
            bases: b"GGGGGGGGGG".to_vec(),
            qualities: None,
        };
        let mut deletion = row_test_record(b"deletion", 10, 20);
        deletion.cigar = vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 5,
            },
            CigarOp {
                kind: CigarOpKind::Deletion,
                len: 1,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            },
        ];
        deletion.sequence = SequenceData::Present {
            bases: b"AAAAAAAAA".to_vec(),
            qualities: None,
        };
        let unavailable = row_test_record(b"unavailable", 10, 20);
        let query = row_test_query(&[adenine, reverse_cytosine, deletion, unavailable]);

        let natural = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::Base,
            AlignmentOrderDirection::Natural,
            Some(15),
            TemplateLengthBounds::default(),
            None,
        );
        assert_eq!(natural.values().next().unwrap(), &vec![0, 1, 2, 3]);

        let reverse = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::Base,
            AlignmentOrderDirection::Reverse,
            Some(15),
            TemplateLengthBounds::default(),
            None,
        );
        assert_eq!(reverse.values().next().unwrap(), &vec![3, 2, 1, 0]);
    }

    #[test]
    fn strand_and_tag_sort_modes_use_normalized_record_fields() {
        let mut reverse_zeta = row_test_record(b"reverse-zeta", 10, 20);
        reverse_zeta.flags = AlignmentFlags::from_bits(0x10);
        reverse_zeta.auxiliary_fields.push(AuxField {
            tag: *b"RG",
            value: AuxValue::String("zeta".to_owned()),
        });
        let mut forward_alpha = row_test_record(b"forward-alpha", 10, 20);
        forward_alpha.auxiliary_fields.push(AuxField {
            tag: *b"RG",
            value: AuxValue::String("alpha".to_owned()),
        });
        let query = row_test_query(&[reverse_zeta, forward_alpha]);

        let strand = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::Strand,
            AlignmentOrderDirection::Natural,
            None,
            TemplateLengthBounds::default(),
            None,
        );
        assert_eq!(strand.values().next().unwrap(), &vec![1, 0]);

        for mode in [AlignmentSortMode::ReadGroup, AlignmentSortMode::Tag(*b"RG")] {
            let groups = grouped_indices(
                &query,
                AlignmentGroupMode::None,
                mode,
                AlignmentOrderDirection::Natural,
                None,
                TemplateLengthBounds::default(),
                None,
            );
            assert_eq!(groups.values().next().unwrap(), &vec![1, 0]);
        }
    }

    #[test]
    fn selected_chain_hash_adds_non_pickable_outlines_to_every_loaded_segment() {
        let records = [
            row_test_record(b"split", 10, 30),
            row_test_record(b"split", 40, 60),
            row_test_record(b"other", 70, 90),
        ];
        let query = row_test_query(&records);
        let options = AlignmentLayoutOptions {
            selected_chain_read_name_hash: Some(alignment_read_name_hash(b"split")),
            ..AlignmentLayoutOptions::default()
        };
        for (pixel_width, expected_level) in
            [(800.0, DetailLevel::Reads), (10.0, DetailLevel::Regional)]
        {
            let viewport = Viewport::new(query.interval, pixel_width).unwrap();
            assert_eq!(viewport.detail_level(), expected_level);
            let layout = layout_alignment_query(&query, viewport, options);
            let outlines = layout
                .display_list
                .rectangles
                .iter()
                .filter(|primitive| {
                    primitive.object_id == 0 && primitive.color == Rgba::new(0.05, 0.66, 0.78, 1.0)
                })
                .count();
            assert_eq!(outlines, 8);
        }
        assert_eq!(
            alignment_read_name_hash(b"split"),
            alignment_read_name_hash(b"split")
        );
        assert_ne!(
            alignment_read_name_hash(b"split"),
            alignment_read_name_hash(b"other")
        );
    }

    #[test]
    fn pair_view_packs_both_ends_as_one_row_and_one_pick_target() {
        let mut left = row_test_record(b"pair", 10, 20);
        left.flags = AlignmentFlags::from_bits(0x63);
        left.mate_contig = Some(left.contig);
        left.mate_start = Some(50);
        left.template_length = 50;
        let mut middle = row_test_record(b"other", 25, 35);
        middle.flags = AlignmentFlags::from_bits(0);
        let mut right = row_test_record(b"pair", 50, 60);
        right.flags = AlignmentFlags::from_bits(0x93);
        right.mate_contig = Some(right.contig);
        right.mate_start = Some(10);
        right.template_length = -50;
        let query = row_test_query(&[left, middle, right]);
        let options = AlignmentLayoutOptions {
            view_as_pairs: true,
            ..AlignmentLayoutOptions::default()
        };
        let rows = assign_alignment_rows(&query, options, None);
        assert_eq!(rows.rows[0], rows.rows[2]);
        assert_ne!(rows.rows[0], rows.rows[1]);

        let viewport = Viewport::new(query.interval, 800.0).unwrap();
        let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .all(|primitive| { crate::record_object_id(primitive.object_id) != 3 })
        );
        let connectors = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| {
                primitive.color == Rgba::new(0.58, 0.64, 0.74, 0.62) && primitive.object_id == 1
            })
            .count();
        assert_eq!(connectors, 1);
    }

    #[test]
    fn pair_style_modes_use_explicit_classification_and_template_length() {
        let mut expected = row_test_record(b"expected", 10, 20);
        expected.flags = AlignmentFlags::from_bits(0x63);
        expected.mate_contig = Some(expected.contig);
        expected.mate_start = Some(50);
        expected.template_length = 50;
        let unpaired = row_test_record(b"unpaired", 70, 80);
        let query = row_test_query(&[expected, unpaired]);
        let bounds = TemplateLengthBounds {
            minimum: Some(10),
            maximum: Some(40),
        };

        let groups = grouped_indices(
            &query,
            AlignmentGroupMode::UnexpectedPair,
            AlignmentSortMode::Start,
            AlignmentOrderDirection::Natural,
            None,
            bounds,
            None,
        );
        assert_eq!(groups.len(), 2);
        assert!(
            groups
                .keys()
                .any(|key| { key == &AlignmentGroupKey::Tagged("Unexpected".to_owned()) })
        );
        assert!(
            groups
                .keys()
                .any(|key| { key == &AlignmentGroupKey::Tagged("Unpaired".to_owned()) })
        );

        let sorted = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            AlignmentSortMode::TemplateLength,
            AlignmentOrderDirection::Natural,
            None,
            bounds,
            None,
        );
        assert_eq!(sorted.values().next().unwrap(), &vec![0, 1]);
        assert_eq!(
            alignment_color(&query, 0, AlignmentColorMode::UnexpectedPair, bounds, 1.0,),
            Rgba::new(0.91, 0.29, 0.24, 1.0)
        );
    }

    #[test]
    fn chromosome_scale_start_establishes_rows_before_reads_appear() {
        let records = [
            row_test_record(b"leading", 900_000, 900_370),
            row_test_record(b"retained", 900_300, 900_500),
        ];
        let mut batch = AlignmentBatch::default();
        for record in &records {
            batch.push(record).unwrap();
        }
        let query = AlignmentQuery {
            interval: GenomicInterval::new(ContigId::new(0), 0, 3_000_000).unwrap(),
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats::default(),
        };
        let options = AlignmentLayoutOptions::default();
        let rows = assign_alignment_rows(&query, options, None);
        assert_eq!(rows.rows, vec![Some(0), Some(1)]);

        let chromosome = Viewport::new(query.interval, 1_000.0).unwrap();
        assert_eq!(chromosome.detail_level(), DetailLevel::Overview);
        let overview = layout_alignment_query_with_rows(&query, chromosome, options, &rows);
        assert!(
            overview
                .display_list
                .rectangles
                .iter()
                .all(|primitive| primitive.object_id == 0)
        );

        let reads = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 900_250, 900_650).unwrap(),
            1_000.0,
        )
        .unwrap();
        let bases = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 900_320, 900_370).unwrap(),
            1_000.0,
        )
        .unwrap();
        assert_eq!(reads.detail_level(), DetailLevel::Reads);
        assert_eq!(bases.detail_level(), DetailLevel::Bases);
        let reads_layout = layout_alignment_query_with_rows(&query, reads, options, &rows);
        let bases_layout = layout_alignment_query_with_rows(&query, bases, options, &rows);
        let retained_y = |layout: &AlignmentLayout| {
            layout
                .display_list
                .rectangles
                .iter()
                .find(|primitive| primitive.object_id == 2)
                .map(|primitive| primitive.y)
                .unwrap()
        };
        assert!((retained_y(&reads_layout) - retained_y(&bases_layout)).abs() < f32::EPSILON);
    }

    #[test]
    fn refined_query_preserves_rows_for_shared_read_identities() {
        let broad_query = row_test_query(&[
            row_test_record(b"leading", 0, 70),
            row_test_record(b"retained", 50, 110),
        ]);
        let options = AlignmentLayoutOptions::default();
        let broad_rows = assign_alignment_rows(&broad_query, options, None);

        let refined_query = row_test_query(&[row_test_record(b"retained", 50, 110)]);
        let scratch_rows = assign_alignment_rows(&refined_query, options, None);
        let carried_rows = assign_alignment_rows(&refined_query, options, Some(&broad_rows));
        assert_eq!(scratch_rows.rows, vec![Some(0)]);
        assert_eq!(carried_rows.rows, vec![Some(1)]);
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn layout_distinguishes_cigar_operations() {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 125,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"complex".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 10,
                },
                CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 5,
                },
                CigarOp {
                    kind: CigarOpKind::SequenceMismatch,
                    len: 10,
                },
            ],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        let viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 95, 130).unwrap(),
            700.0,
        )
        .unwrap();
        let query = AlignmentQuery {
            interval: viewport.interval(),
            header: Arc::default(),
            batch,
            pileup: PileupSummary::from_depths(vec![1; 100]),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 1,
                records_eligible: 1,
                records_emitted: 1,
                ..QueryStats::default()
            },
        };
        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
        assert_eq!(layout.stats.rendered_alignments, 1);
        assert!(layout.display_list.rectangles.len() > query.pileup.len());
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .any(|primitive| primitive.object_id == 1)
        );
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
                && record_object_id(primitive.object_id) == 1
        }));
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            primitive.object_id == 1
                && (primitive.height - AlignmentLayoutOptions::default().read_height).abs()
                    < f32::EPSILON
        }));
        assert!(
            layout
                .display_list
                .texts
                .iter()
                .any(|text| text.text == "I3")
        );
        assert!(
            layout
                .display_list
                .texts
                .iter()
                .any(|text| text.text == "N5")
        );

        let filtered = layout_alignment_query(
            &query,
            viewport,
            AlignmentLayoutOptions {
                minimum_indel_size: 4,
                minimum_event_label_size: 4,
                ..AlignmentLayoutOptions::default()
            },
        );
        assert!(
            !filtered
                .display_list
                .texts
                .iter()
                .any(|text| text.text == "I3")
        );
        assert!(
            filtered
                .display_list
                .texts
                .iter()
                .any(|text| text.text == "N5")
        );
        assert!(!filtered.display_list.rectangles.iter().any(|primitive| {
            (primitive.color.red - 0.86).abs() < 0.01
                && (primitive.color.green - 0.32).abs() < 0.01
                && (primitive.color.blue - 0.82).abs() < 0.01
        }));
    }

    #[test]
    fn base_layout_compares_m_operations_with_reference() {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 104,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"mismatch".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGT".to_vec(),
                qualities: Some(vec![0, 40, 40, 40]),
            },
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let query = AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::from_depths(vec![1; 4]),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: Some(ReferenceSlice {
                interval,
                bases: b"ACAT".to_vec(),
            }),
            stats: QueryStats {
                records_seen: 1,
                records_eligible: 1,
                records_emitted: 1,
                ..QueryStats::default()
            },
        };

        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
        assert_eq!(layout.stats.rendered_alignments, 1);
        assert!(layout.pinned_rectangle_count > 0);
        assert!(
            layout.display_list.rectangles[..layout.pinned_rectangle_count]
                .iter()
                .all(|primitive| primitive.object_id == 0)
        );
        assert!(
            layout.display_list.rectangles[layout.pinned_rectangle_count..]
                .iter()
                .any(|primitive| record_object_id(primitive.object_id) == 1)
        );
        assert_eq!(layout.pinned_text_count, 4);
        assert!(layout.display_list.texts.len() > layout.pinned_text_count);
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            record_object_id(primitive.object_id) == 1
                && primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
                && primitive.color.red > 0.9
                && primitive.color.green > 0.5
        }));
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            primitive.object_id == 0 && (primitive.y - 64.0).abs() < f32::EPSILON
        }));
        let reference_label = layout
            .display_list
            .texts
            .iter()
            .min_by(|left, right| left.y.total_cmp(&right.y))
            .unwrap();
        assert!(reference_label.color.red < 0.9);
        assert!(reference_label.color.green < 0.9);
        assert!(reference_label.color.blue < 0.9);
        let mismatch_label = layout
            .display_list
            .texts
            .iter()
            .max_by(|left, right| left.y.total_cmp(&right.y))
            .unwrap();
        assert!(mismatch_label.color.red < 0.1);
        assert!(mismatch_label.color.green < 0.1);
        assert!(mismatch_label.color.blue < 0.15);
        assert!((mismatch_label.color.alpha - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn read_base_labels_contrast_with_each_nucleotide_fill() {
        for base in [b'A', b'G'] {
            let color = read_base_label_color(base);
            assert!(color.red < 0.1, "{base:?} should use dark text");
            assert!((color.alpha - 1.0).abs() < f32::EPSILON);
        }
        for base in [b'C', b'T'] {
            let color = read_base_label_color(base);
            assert!(color.red > 0.95, "{base:?} should use light text");
            assert!((color.alpha - 1.0).abs() < f32::EPSILON);
        }
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn exact_match_fill_is_stable_across_the_base_detail_boundary() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 100, 104).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let record = AlignmentRecord {
            contig,
            start: 100,
            end: 104,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"exact-match".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::SequenceMatch,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGT".to_vec(),
                qualities: Some(vec![0, 40, 40, 40]),
            },
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        let query = AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: Some(ReferenceSlice {
                interval,
                bases: b"ACGT".to_vec(),
            }),
            stats: QueryStats::default(),
        };

        let mut reads = DisplayList::default();
        add_match_segment(
            &mut reads,
            &query,
            0,
            CigarOpKind::SequenceMatch,
            100,
            104,
            viewport,
            20.0,
            5.0,
            1,
            AlignmentColorMode::None,
            TemplateLengthBounds::default(),
        );
        let mut bases = DisplayList::default();
        let mut bisulfite_budget = BisulfiteRenderBudget::new(MAX_RENDERED_BISULFITE_MARKS);
        let mut smrt_kinetics_budget =
            SmrtKineticsRenderBudget::new(MAX_RENDERED_SMRT_KINETICS_MARKS);
        let mut sbx_budget = SbxRenderBudget::new(MAX_RENDERED_SBX_TAIL_MARKS);
        add_aligned_bases(
            &mut bases,
            &query,
            0,
            CigarOpKind::SequenceMatch,
            100,
            0,
            4,
            viewport,
            20.0,
            5.0,
            1,
            AlignmentColorMode::None,
            TemplateLengthBounds::default(),
            ReadBaseDisplayMode::Mismatches,
            true,
            &mut bisulfite_budget,
            None,
            &mut smrt_kinetics_budget,
            None,
            &mut sbx_budget,
        );

        assert_eq!(reads.rectangles.len(), 2);
        assert_eq!(bases.rectangles.len(), 2);
        assert_eq!(reads.rectangles[1].color, bases.rectangles[1].color);

        let mut all_bases = DisplayList::default();
        add_aligned_bases(
            &mut all_bases,
            &query,
            0,
            CigarOpKind::SequenceMatch,
            100,
            0,
            4,
            viewport,
            20.0,
            5.0,
            1,
            AlignmentColorMode::None,
            TemplateLengthBounds::default(),
            ReadBaseDisplayMode::All,
            true,
            &mut bisulfite_budget,
            None,
            &mut smrt_kinetics_budget,
            None,
            &mut sbx_budget,
        );
        assert_eq!(all_bases.rectangles.len(), 6);
        assert_eq!(all_bases.texts.len(), 4);
        assert!((all_bases.rectangles[2].color.alpha - 0.40).abs() < 0.01);

        let mut unshaded = DisplayList::default();
        add_aligned_bases(
            &mut unshaded,
            &query,
            0,
            CigarOpKind::SequenceMatch,
            100,
            0,
            4,
            viewport,
            20.0,
            5.0,
            1,
            AlignmentColorMode::None,
            TemplateLengthBounds::default(),
            ReadBaseDisplayMode::All,
            false,
            &mut bisulfite_budget,
            None,
            &mut smrt_kinetics_budget,
            None,
            &mut sbx_budget,
        );
        assert!((unshaded.rectangles[2].color.alpha - 0.98).abs() < 0.01);

        let mut hidden = DisplayList::default();
        add_aligned_bases(
            &mut hidden,
            &query,
            0,
            CigarOpKind::SequenceMatch,
            100,
            0,
            4,
            viewport,
            20.0,
            5.0,
            1,
            AlignmentColorMode::None,
            TemplateLengthBounds::default(),
            ReadBaseDisplayMode::Hidden,
            true,
            &mut bisulfite_budget,
            None,
            &mut smrt_kinetics_budget,
            None,
            &mut sbx_budget,
        );
        assert_eq!(hidden.rectangles.len(), 2);
        assert!(hidden.texts.is_empty());
    }

    #[test]
    fn reverse_strand_bases_are_reverse_complemented_with_quality() {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: 4,
            flags: AlignmentFlags::from_bits(0x10),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: Vec::new(),
            cigar: Vec::new(),
            sequence: SequenceData::Present {
                bases: b"AACG".to_vec(),
                qualities: Some(vec![10, 20, 30, 40]),
            },
            auxiliary_fields: Vec::new(),
        };
        let SequenceData::Present { bases, qualities } = &record.sequence else {
            unreachable!();
        };
        assert_eq!(
            oriented_base(record.flags, bases, qualities.as_deref(), 0),
            Some((b'C', Some(40)))
        );
        assert_eq!(
            oriented_base(record.flags, bases, qualities.as_deref(), 3),
            Some((b'T', Some(10)))
        );
    }

    #[test]
    fn prefetched_coverage_is_clipped_and_scaled_to_visible_bins() {
        let contig = ContigId::new(0);
        let query_interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
        let viewport =
            Viewport::new(GenomicInterval::new(contig, 400, 600).unwrap(), 1_000.0).unwrap();
        let mut display_list = DisplayList::default();
        let maximum = add_coverage(
            &mut display_list,
            &PileupSummary::from_depths([1; 10]),
            None,
            query_interval,
            viewport,
            1_000.0,
            &AlignmentLayoutOptions::default(),
        );
        assert_eq!(maximum, 1);
        assert_eq!(display_list.rectangles.len(), 2);
        assert!(display_list.rectangles[0].x.abs() < f32::EPSILON);
        assert!((display_list.rectangles[1].x - 500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fixed_coverage_scale_is_applied_and_clipped_without_changing_reported_depth() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 1).unwrap();
        let viewport = Viewport::new(interval, 100.0).unwrap();
        let pileup = PileupSummary::from_depths([10]);
        let options = AlignmentLayoutOptions {
            coverage_height: 60.0,
            coverage_scale_maximum: Some(40),
            ..AlignmentLayoutOptions::default()
        };
        let mut display = DisplayList::default();
        let maximum = add_coverage(
            &mut display,
            &pileup,
            None,
            interval,
            viewport,
            100.0,
            &options,
        );
        assert_eq!(maximum, 10);
        assert_eq!(display.rectangles.len(), 1);
        assert!((display.rectangles[0].height - 30.0).abs() < 0.01);

        display.rectangles.clear();
        let maximum = add_coverage(
            &mut display,
            &PileupSummary::from_depths([80]),
            None,
            interval,
            viewport,
            100.0,
            &options,
        );
        assert_eq!(maximum, 80);
        assert!((display.rectangles[0].height - 60.0).abs() < 0.01);
    }

    #[test]
    fn hidden_alignment_subtracks_emit_no_primitives_but_keep_coverage_provenance() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 200).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let mut query = row_test_query(&[row_test_record(b"read", 10, 40)]);
        query.pileup = PileupSummary::from_depths([7]);
        query.reference = Some(ReferenceSlice {
            interval,
            bases: vec![b'A'; 200],
        });
        let options = AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_reads: false,
            show_modification_frequency: false,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        assert!(layout.display_list.rectangles.is_empty());
        assert!(layout.display_list.texts.is_empty());
        assert_eq!(layout.stats.maximum_coverage, 7);
        assert_eq!(layout.stats.rendered_alignments, 0);
        assert_eq!(layout.stats.hidden_alignments, 1);
    }

    #[test]
    fn junction_lane_renders_bounded_strand_aware_full_evidence() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 200).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let mut query = row_test_query(&[]);
        query.junctions = JunctionSummary {
            junctions: vec![
                JunctionEvidence {
                    start: 20,
                    end: 80,
                    depth: 4,
                    positive_depth: 4,
                    ..JunctionEvidence::default()
                },
                JunctionEvidence {
                    start: 100,
                    end: 180,
                    depth: 9,
                    negative_depth: 9,
                    ..JunctionEvidence::default()
                },
            ],
            provenance: rgv_hts::JunctionProvenance {
                eligible_records: 13,
                spliced_records: 13,
                all_eligible_records: true,
                ..rgv_hts::JunctionProvenance::default()
            },
        };
        let options = AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_junctions: true,
            show_reads: false,
            show_modification_frequency: false,
            junction_top: 10.0,
            junction_height: 30.0,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        assert_eq!(layout.display_list.rectangles.len(), 6);
        assert_eq!(layout.pinned_rectangle_count, 6);
        assert_eq!(layout.stats.rendered_junctions, 2);
        assert_eq!(layout.stats.hidden_junctions, 0);
        assert_eq!(layout.stats.maximum_junction_depth, 9);
        assert_eq!(
            layout.display_list.rectangles[0].color,
            Rgba::new(0.84, 0.30, 0.38, 0.88)
        );
        assert_eq!(
            layout.display_list.rectangles[3].color,
            Rgba::new(0.25, 0.45, 0.82, 0.88)
        );
        assert_eq!(query.junctions.provenance.eligible_records, 13);

        let filtered = layout_alignment_query(
            &query,
            viewport,
            AlignmentLayoutOptions {
                minimum_junction_depth: 5,
                selected_junction: [100, 180],
                ..options
            },
        );
        assert_eq!(filtered.display_list.rectangles.len(), 3);
        assert_eq!(filtered.stats.rendered_junctions, 1);
        assert_eq!(filtered.stats.hidden_junctions, 1);
        assert!(
            filtered
                .display_list
                .rectangles
                .iter()
                .all(|rectangle| rectangle.color == Rgba::new(0.08, 0.08, 0.10, 1.0))
        );
    }

    #[test]
    fn sashimi_layout_applies_exact_cpm_fixed_scale_and_expanded_rows() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 200).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let mut query = row_test_query(&[]);
        query.junctions = JunctionSummary {
            junctions: vec![
                JunctionEvidence {
                    start: 20,
                    end: 120,
                    depth: 4,
                    positive_depth: 4,
                    ..JunctionEvidence::default()
                },
                JunctionEvidence {
                    start: 40,
                    end: 160,
                    depth: 16,
                    negative_depth: 16,
                    ..JunctionEvidence::default()
                },
            ],
            provenance: rgv_hts::JunctionProvenance {
                normalization_mapped_records: Some(2_000_000),
                all_eligible_records: true,
                ..rgv_hts::JunctionProvenance::default()
            },
        };
        assert!(
            (junction_count_value(
                &query.junctions,
                &query.junctions.junctions[0],
                SashimiCountMode::CountsPerMillion
            ) - 2.0)
                .abs()
                < f64::EPSILON
        );

        let options = AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_reads: false,
            show_modification_frequency: false,
            junction_top: 10.0,
            junction_height: 80.0,
            junction_display_mode: JunctionDisplayMode::Expanded,
            junction_count_mode: SashimiCountMode::CountsPerMillion,
            junction_scale_maximum: Some(8.0),
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        assert_eq!(layout.stats.rendered_junctions, 2);
        assert!((layout.stats.maximum_junction_value - 8.0).abs() < f64::EPSILON);
        assert!(layout.stats.junction_normalization_available);
        assert!(
            (layout.display_list.rectangles[0].y - layout.display_list.rectangles[3].y).abs()
                > f32::EPSILON
        );
        assert_eq!(layout.display_list.texts.len(), 2);
        assert_eq!(layout.display_list.texts[0].text, "2.0 CPM");
        assert_eq!(layout.display_list.texts[1].text, "8.0 CPM");

        let annotations = JunctionAnnotationLookup::new([(20, 120)], 1, true);
        assert_eq!(
            annotations.status(20, 120),
            JunctionAnnotationStatus::Annotated
        );
        assert_eq!(annotations.status(40, 160), JunctionAnnotationStatus::Novel);
        let rows = assign_alignment_rows(&query, options, None);
        let annotated_layout = layout_alignment_query_with_rows_and_annotations(
            &query,
            viewport,
            options,
            &rows,
            Some(&annotations),
        );
        assert_eq!(annotated_layout.display_list.rectangles.len(), 8);
        assert_eq!(
            annotated_layout.display_list.rectangles[3].color,
            Rgba::new(0.16, 0.58, 0.28, 1.0)
        );
        assert_eq!(
            annotated_layout.display_list.rectangles[7].color,
            Rgba::new(0.92, 0.48, 0.10, 1.0)
        );

        let raw_fallback = JunctionSummary {
            provenance: rgv_hts::JunctionProvenance::default(),
            ..query.junctions.clone()
        };
        assert!(
            (junction_count_value(
                &raw_fallback,
                &raw_fallback.junctions[0],
                SashimiCountMode::CountsPerMillion,
            ) - 4.0)
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn high_density_sashimi_output_has_hard_arc_and_label_caps() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 20_000).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let summary = JunctionSummary {
            junctions: (0..5_000_u64)
                .map(|index| JunctionEvidence {
                    start: index,
                    end: 10_000 + index,
                    depth: u32::try_from(index + 1).unwrap(),
                    positive_depth: 1,
                    ..JunctionEvidence::default()
                })
                .collect(),
            ..JunctionSummary::default()
        };
        let layout = layout_sashimi(
            &summary,
            viewport,
            AlignmentLayoutOptions {
                show_junctions: true,
                junction_top: 0.0,
                junction_height: 100.0,
                ..AlignmentLayoutOptions::default()
            },
            None,
        );
        assert_eq!(layout.stats.rendered_junctions, MAX_RENDERED_JUNCTIONS);
        assert_eq!(
            layout.stats.hidden_junctions,
            5_000 - MAX_RENDERED_JUNCTIONS
        );
        assert_eq!(
            layout.display_list.rectangles.len(),
            MAX_RENDERED_JUNCTIONS * 3
        );
        assert_eq!(
            layout.display_list.texts.len(),
            MAX_RENDERED_JUNCTION_LABELS
        );
    }

    #[test]
    fn coverage_colours_only_reference_discordant_support_above_threshold() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 100, 101).unwrap();
        let viewport = Viewport::new(interval, 100.0).unwrap();
        let pileup = PileupSummary {
            bins: vec![PileupBin {
                depth: 10,
                alleles: AlleleSupport {
                    a: 8,
                    c: 2,
                    ..AlleleSupport::default()
                },
                ..PileupBin::default()
            }],
            ..PileupSummary::default()
        };
        let reference = ReferenceSlice {
            interval,
            bases: b"A".to_vec(),
        };
        let mut display = DisplayList::default();
        let options = AlignmentLayoutOptions {
            minimum_allele_fraction: 0.20,
            quality_weighted_alleles: false,
            ..AlignmentLayoutOptions::default()
        };
        add_coverage(
            &mut display,
            &pileup,
            Some(&reference),
            interval,
            viewport,
            100.0,
            &options,
        );
        assert_eq!(display.rectangles.len(), 2);
        assert_eq!(display.rectangles[1].color, base_color(b'C'));
        assert!((display.rectangles[1].height - options.coverage_height * 0.2).abs() < 0.01);

        display.rectangles.clear();
        add_coverage(
            &mut display,
            &pileup,
            Some(&reference),
            interval,
            viewport,
            100.0,
            &AlignmentLayoutOptions {
                minimum_allele_fraction: 0.21,
                ..options
            },
        );
        assert_eq!(display.rectangles.len(), 1);
    }

    #[test]
    fn standalone_reference_layout_uses_the_alignment_base_lane_semantics() {
        let interval = GenomicInterval::new(ContigId::new(0), 10, 14).unwrap();
        let viewport = Viewport::new(interval, 40.0).unwrap();
        let reference = ReferenceSlice {
            interval,
            bases: b"ACGT".to_vec(),
        };

        let display = layout_reference_slice(&reference, viewport, 25.0);

        assert_eq!(display.rectangles.len(), 4);
        assert_eq!(display.texts.len(), 4);
        assert!(
            display
                .rectangles
                .iter()
                .all(|base| (base.y - 25.0).abs() < f32::EPSILON)
        );
        assert_eq!(
            display
                .texts
                .iter()
                .map(|label| label.text.as_str())
                .collect::<Vec<_>>(),
            ["A", "C", "G", "T"]
        );
        let reverse = layout_reference_slice_oriented(&reference, viewport, 25.0, true);
        assert_eq!(
            reverse
                .texts
                .iter()
                .map(|label| label.text.as_str())
                .collect::<Vec<_>>(),
            ["T", "G", "C", "A"]
        );
    }

    #[test]
    fn reference_translation_supports_both_strands_and_explicit_genetic_codes() {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 3).unwrap();
        let viewport = Viewport::new(interval, 30.0).unwrap();
        let forward = ReferenceSlice {
            interval,
            bases: b"ATG".to_vec(),
        };
        let reverse = ReferenceSlice {
            interval,
            bases: b"CAT".to_vec(),
        };
        assert_eq!(
            layout_reference_translation(
                &forward,
                viewport,
                0.0,
                ReferenceTranslationFrame::Forward1,
                GeneticCode::Standard,
            )
            .texts[0]
                .text,
            "M"
        );
        assert_eq!(
            layout_reference_translation(
                &reverse,
                viewport,
                0.0,
                ReferenceTranslationFrame::Reverse1,
                GeneticCode::Standard,
            )
            .texts[0]
                .text,
            "M"
        );
        let mitochondrial = ReferenceSlice {
            interval,
            bases: b"TGA".to_vec(),
        };
        assert_eq!(
            layout_reference_translation(
                &mitochondrial,
                viewport,
                0.0,
                ReferenceTranslationFrame::Forward1,
                GeneticCode::VertebrateMitochondrial,
            )
            .texts[0]
                .text,
            "W"
        );
    }

    #[test]
    fn deep_overlapping_reads_extend_beyond_the_old_ninety_row_cap() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let mut batch = AlignmentBatch::default();
        for index in 0..120 {
            batch
                .push(&AlignmentRecord {
                    contig,
                    start: 100,
                    end: 200,
                    flags: AlignmentFlags::from_bits(0),
                    mapping_quality: Some(60),
                    mate_contig: None,
                    mate_start: None,
                    template_length: 0,
                    read_name: format!("read-{index}").into_bytes(),
                    cigar: vec![CigarOp {
                        kind: CigarOpKind::Match,
                        len: 100,
                    }],
                    sequence: SequenceData::NotLoaded,
                    auxiliary_fields: Vec::new(),
                })
                .unwrap();
        }
        let query = AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 120,
                records_eligible: 121,
                records_emitted: 120,
                truncated: true,
                ..QueryStats::default()
            },
        };
        let options = AlignmentLayoutOptions {
            max_rows: 128,
            ..AlignmentLayoutOptions::default()
        };

        let layout = layout_alignment_query(&query, viewport, options);

        assert_eq!(layout.stats.rendered_alignments, 120);
        assert_eq!(layout.stats.hidden_alignments, 0);
        assert!(layout.display_list.rectangles.iter().any(|rectangle| {
            rectangle.object_id == 0
                && (rectangle.height - 3.0).abs() < f32::EPSILON
                && rectangle.color.red < 0.1
        }));
        let bottom = layout
            .display_list
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(0.0_f32, f32::max);
        assert!(bottom > options.reads_top + 90.0 * options.row_height);
    }

    #[test]
    fn regional_spans_reuse_rows_after_preceding_alignments_end() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
        let viewport = Viewport::new(interval, 50.0).unwrap();
        assert_eq!(viewport.detail_level(), DetailLevel::Regional);
        let mut batch = AlignmentBatch::default();
        for (index, (start, end)) in [(100, 200), (110, 210), (220, 320), (230, 330)]
            .into_iter()
            .enumerate()
        {
            batch
                .push(&AlignmentRecord {
                    contig,
                    start,
                    end,
                    flags: AlignmentFlags::from_bits(0),
                    mapping_quality: Some(60),
                    mate_contig: None,
                    mate_start: None,
                    template_length: 0,
                    read_name: format!("regional-{index}").into_bytes(),
                    cigar: vec![CigarOp {
                        kind: CigarOpKind::Match,
                        len: u32::try_from(end - start).unwrap(),
                    }],
                    sequence: SequenceData::NotLoaded,
                    auxiliary_fields: Vec::new(),
                })
                .unwrap();
        }
        let query = AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 4,
                records_eligible: 4,
                records_emitted: 4,
                ..QueryStats::default()
            },
        };

        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());

        assert_eq!(layout.stats.rendered_alignments, 4);
        assert_eq!(layout.stats.hidden_alignments, 0);
        let rows = layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| {
                rectangle.object_id != 0
                    && (rectangle.height - AlignmentLayoutOptions::default().read_height).abs()
                        < f32::EPSILON
            })
            .map(|rectangle| rectangle.y.to_bits())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(rows.len(), 2);
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .filter(|rectangle| rectangle.object_id != 0)
                .all(|rectangle| rectangle.width >= 1.0)
        );
    }

    #[test]
    fn default_reads_have_a_neutral_outline_and_pale_fill() {
        let contig = ContigId::new(0);
        let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
        let viewport = Viewport::new(interval, 1_000.0).unwrap();
        let mut batch = AlignmentBatch::default();
        batch
            .push(&AlignmentRecord {
                contig,
                start: 100,
                end: 200,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: b"outlined".to_vec(),
                cigar: vec![CigarOp {
                    kind: CigarOpKind::Match,
                    len: 100,
                }],
                sequence: SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .unwrap();
        let query = AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats::default(),
        };
        let options = AlignmentLayoutOptions {
            color_mode: AlignmentColorMode::None,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        let body = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| primitive.object_id == 1)
            .collect::<Vec<_>>();
        assert_eq!(body.len(), 2);
        assert!(body[0].color.red < 0.7);
        assert!(body[1].color.red > 0.8);
        assert!(body[1].width < body[0].width);
        assert!(body[1].height < body[0].height);
    }

    #[test]
    fn event_label_generation_obeys_a_hard_budget() {
        let mut display = DisplayList::default();
        let mut budget = 3;
        for index in 0..10 {
            add_event_label(
                &mut display,
                "I12",
                index as f32 * 20.0,
                20.0,
                Rgba::new(0.5, 0.2, 0.8, 1.0),
                1,
                &mut budget,
            );
        }
        assert_eq!(budget, 0);
        assert_eq!(display.texts.len(), 3);
        assert_eq!(
            display
                .rectangles
                .iter()
                .filter(|primitive| primitive.object_id == 0)
                .count(),
            3
        );
    }

    #[test]
    fn maps_read_positions_through_insertions_and_deletions() {
        let cigar = [
            CigarOp {
                kind: CigarOpKind::Match,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Insertion,
                len: 2,
            },
            CigarOp {
                kind: CigarOpKind::Deletion,
                len: 3,
            },
            CigarOp {
                kind: CigarOpKind::Match,
                len: 2,
            },
        ];
        assert_eq!(
            reference_position_for_read_position(100, &cigar, 0),
            Some(100)
        );
        assert_eq!(
            reference_position_for_read_position(100, &cigar, 2),
            Some(102)
        );
        assert_eq!(
            reference_position_for_read_position(100, &cigar, 4),
            Some(105)
        );
    }

    fn modified_base_query(
        mm: &str,
        probabilities: Option<Vec<u64>>,
    ) -> (AlignmentQuery, Viewport) {
        let mut auxiliary_fields = vec![AuxField {
            tag: *b"MM",
            value: AuxValue::String(mm.to_owned()),
        }];
        if let Some(probabilities) = probabilities {
            auxiliary_fields.push(AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(probabilities),
            });
        }
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 104,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"modified".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACCC".to_vec(),
                qualities: Some(vec![40; 4]),
            },
            auxiliary_fields,
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        (
            AlignmentQuery {
                interval,
                header: Arc::default(),
                batch,
                pileup: PileupSummary::default(),
                junctions: rgv_hts::JunctionSummary::default(),
                pairs: rgv_hts::PairSummary::default(),
                reference: None,
                stats: QueryStats::default(),
            },
            viewport,
        )
    }

    fn modification_overlays(layout: &AlignmentLayout) -> Vec<&RectPrimitive> {
        layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| {
                record_object_id(primitive.object_id) == 1
                    && primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
                    && primitive.height <= 4.0
            })
            .collect()
    }

    #[test]
    fn modified_base_calls_use_stable_type_colour_and_probability_alpha() {
        let (query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
        let overlays = modification_overlays(&layout);
        assert_eq!(overlays.len(), 1);
        assert_eq!(
            overlays[0].color,
            modification_code_color(ModificationCode::Letter(b'm'))
        );

        assert_ne!(
            modification_code_color(ModificationCode::Letter(b'm')),
            modification_code_color(ModificationCode::Letter(b'h'))
        );
        assert_eq!(
            modification_code_color(ModificationCode::Chebi(76792)),
            modification_code_color(ModificationCode::Chebi(76792))
        );
    }

    #[test]
    fn modified_base_probability_and_visibility_filters_are_enforced() {
        let (query, viewport) = modified_base_query("C+m,1;", Some(vec![127]));
        let mut options = AlignmentLayoutOptions {
            minimum_modification_probability: 128,
            ..AlignmentLayoutOptions::default()
        };
        let filtered = layout_alignment_query(&query, viewport, options);
        assert!(modification_overlays(&filtered).is_empty());

        options.minimum_modification_probability = 0;
        options.hidden_modification_codes[0] = Some(ModificationCode::Letter(b'm'));
        let hidden = layout_alignment_query(&query, viewport, options);
        assert!(modification_overlays(&hidden).is_empty());

        options.hidden_modification_codes[0] = None;
        options.show_modified_bases = false;
        let disabled = layout_alignment_query(&query, viewport, options);
        assert!(modification_overlays(&disabled).is_empty());
    }

    #[test]
    fn missing_probability_is_visible_only_without_a_probability_threshold() {
        let (query, viewport) = modified_base_query("C+m,1;", None);
        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
        let overlays = modification_overlays(&layout);
        assert_eq!(overlays.len(), 1);
        assert!((overlays[0].color.alpha - 0.78).abs() < f32::EPSILON);

        let options = AlignmentLayoutOptions {
            minimum_modification_probability: 1,
            ..AlignmentLayoutOptions::default()
        };
        let filtered = layout_alignment_query(&query, viewport, options);
        assert!(modification_overlays(&filtered).is_empty());
    }

    #[test]
    fn modified_vs_unmodified_mode_marks_only_implicit_canonical_sites() {
        let (query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
        let options = AlignmentLayoutOptions {
            modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        let overlays = modification_overlays(&layout);
        assert_eq!(overlays.len(), 3);
        assert_eq!(
            overlays
                .iter()
                .filter(|primitive| primitive.color.red > 0.8)
                .count(),
            1
        );
        assert_eq!(
            overlays
                .iter()
                .filter(|primitive| primitive.color.blue > 0.8 && primitive.color.red < 0.3)
                .count(),
            2
        );

        let (unknown_query, unknown_viewport) = modified_base_query("C+m?,1;", Some(vec![255]));
        let unknown = layout_alignment_query(&unknown_query, unknown_viewport, options);
        assert_eq!(modification_overlays(&unknown).len(), 1);
    }

    #[test]
    fn modified_base_overlays_obey_the_shared_hard_budget() {
        let (query, viewport) = modified_base_query("C+m,0,0,0;", Some(vec![255, 255, 255]));
        let mut display = DisplayList::default();
        let mut budget = 2;
        add_modified_base_calls(
            &mut display,
            &query,
            0,
            viewport,
            20.0,
            5.0,
            1,
            &AlignmentLayoutOptions::default(),
            &mut budget,
            None,
        );
        assert_eq!(display.rectangles.len(), 2);
        assert_eq!(budget, 0);
    }

    #[test]
    fn aggregate_modification_lane_renders_fraction_color_and_selection() {
        let (mut query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
        query.pileup.modifications = ModificationSummary {
            bin_count: 4,
            series: vec![ModificationSeries {
                key: ModificationSeriesKey {
                    canonical_base: b'C',
                    code: ModificationCode::Letter(b'm'),
                    modification_strand: BaseModificationStrand::Forward,
                    implicit_mode: ImplicitModificationMode::Canonical,
                    haplotype: Some(1),
                },
                bins: vec![
                    ModificationBin::default(),
                    ModificationBin::default(),
                    ModificationBin {
                        modified_calls: 1,
                        canonical_observations: 2,
                        forward_modified_calls: 1,
                        probability_sum: 255,
                        probability_observations: 1,
                        ..ModificationBin::default()
                    },
                    ModificationBin::default(),
                ],
            }],
            provenance: ModificationProvenance {
                eligible_records: 2,
                records_with_modification_tags: 1,
                decoded_calls: 1,
                all_eligible_records: true,
                ..ModificationProvenance::default()
            },
        };
        let options = AlignmentLayoutOptions {
            selected_modification_position: Some(102),
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_alignment_query(&query, viewport, options);
        let lane = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| {
                primitive.y >= options.modification_top
                    && primitive.y < options.modification_top + options.modification_height
            })
            .collect::<Vec<_>>();
        assert_eq!(lane.len(), 2);
        assert_eq!(
            lane[0].color,
            modification_code_color(ModificationCode::Letter(b'm'))
        );
        assert!((lane[0].height - options.modification_height * 0.5).abs() < f32::EPSILON);
        assert!((lane[1].height - options.modification_height).abs() < f32::EPSILON);
    }

    #[test]
    fn aggregate_modification_lane_respects_visibility_and_two_color_mode() {
        let (mut query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
        query.pileup.modifications = ModificationSummary {
            bin_count: 1,
            series: vec![ModificationSeries {
                key: ModificationSeriesKey {
                    canonical_base: b'C',
                    code: ModificationCode::Letter(b'm'),
                    modification_strand: BaseModificationStrand::Forward,
                    implicit_mode: ImplicitModificationMode::Canonical,
                    haplotype: None,
                },
                bins: vec![ModificationBin {
                    modified_calls: 1,
                    canonical_observations: 2,
                    probability_sum: 255,
                    probability_observations: 1,
                    ..ModificationBin::default()
                }],
            }],
            provenance: ModificationProvenance::default(),
        };
        let options = AlignmentLayoutOptions {
            modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
            ..AlignmentLayoutOptions::default()
        };
        let mut display = DisplayList::default();
        add_modification_frequency(
            &mut display,
            &query.pileup.modifications,
            query.interval,
            viewport,
            &options,
        );
        assert_eq!(display.rectangles.len(), 2);
        assert!(display.rectangles[0].color.blue > display.rectangles[0].color.red);
        assert_eq!(
            display.rectangles[1].color,
            Rgba::new(0.90, 0.24, 0.92, 0.92)
        );

        let mut hidden = options;
        hidden.hidden_modification_codes[0] = Some(ModificationCode::Letter(b'm'));
        let mut display = DisplayList::default();
        add_modification_frequency(
            &mut display,
            &query.pileup.modifications,
            query.interval,
            viewport,
            &hidden,
        );
        assert!(display.rectangles.is_empty());

        hidden.hidden_modification_codes[0] = None;
        hidden.show_modification_frequency = false;
        add_modification_frequency(
            &mut display,
            &query.pileup.modifications,
            query.interval,
            viewport,
            &hidden,
        );
        assert!(display.rectangles.is_empty());
    }
}
