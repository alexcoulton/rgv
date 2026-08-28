//! CPU-side layout and immutable display-list primitives.

mod alignment_layout;
mod display;
mod genome_context;
mod track_layout;

pub use alignment_layout::{
    ALIGNMENT_ROW_ARCHITECTURE_VERSION, AlignmentColorMode, AlignmentGroupMode, AlignmentLayout,
    AlignmentLayoutOptions, AlignmentLayoutStats, AlignmentOrderDirection, AlignmentRowHistory,
    AlignmentRowIdentity, AlignmentRowLayout, AlignmentRowOrganizationMetrics,
    AlignmentRowReconciliationStats, AlignmentSortMode, BisulfiteContext,
    DEFAULT_ALIGNMENT_ROW_HISTORY_BUDGET_BYTES, IndividualReadRefusal, IndividualReadRefusalScope,
    JunctionAnnotationLookup, JunctionAnnotationStatus, JunctionArcComponent, JunctionArcTarget,
    MAX_ANNOTATED_JUNCTIONS, MAX_HIDDEN_MODIFICATION_CODES, MAX_RENDERED_BISULFITE_MARKS,
    MAX_RENDERED_JUNCTION_LABELS, MAX_RENDERED_JUNCTIONS, MAX_RENDERED_SBX_TAIL_MARKS,
    MAX_RENDERED_SMRT_KINETICS_MARKS, MAX_RENDERED_ULTIMA_QUALITY_MARKS,
    MINIMUM_BASE_GLYPH_PIXEL_WIDTH, SbxDisplayOptions, SbxSimplexTailMode, SmrtKineticsMode,
    UltimaDisplayOptions, UltimaQualityMode, alignment_read_name_hash,
    alignment_row_organization_metrics, assign_alignment_rows, junction_count_value,
    layout_alignment_query, layout_alignment_query_with_rows,
    layout_alignment_query_with_rows_and_annotations, layout_reference_slice,
    layout_reference_slice_oriented, layout_reference_translation, layout_sashimi,
    modification_code_color, reconcile_alignment_rows, reconcile_alignment_rows_with_horizon,
    ultima_quality_color,
};
pub use display::{
    Direction, DisplayList, JunctionArcGeometrySample, READ_DETAIL_OBJECT_BIT, RectPrimitive,
    RectShape, Rgba, TextPrimitive, breakend_bracket_dimensions, junction_arc_distance_to_point,
    junction_arc_geometry_at_point, junction_arc_stroke_width, read_arrow_head_width,
    read_detail_object_id, record_object_id,
};
pub use genome_context::{
    CoordinateRulerLayout, CoordinateRulerOptions, IdeogramLayout, IdeogramSegment, RulerTick,
    format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};
pub use track_layout::{
    COPY_NUMBER_HEADER_HEIGHT, CopyNumberLayout, CopyNumberVisibleRow,
    MAX_COPY_NUMBER_VALUE_LABELS, MAX_INTERACTION_LABELS, MAX_MUTATION_LABELS,
    MAX_QUANTITATIVE_WINDOWS, MAX_RAW_QUANTITATIVE_RECORDS, MAX_RENDERED_COPY_NUMBER_SEGMENTS,
    MAX_RENDERED_GWAS_POINTS, MAX_RENDERED_INTERACTIONS, MAX_RENDERED_MUTATIONS,
    MUTATION_HEADER_HEIGHT, MutationLayout, MutationVisibleRow, QuantitativeComposition,
    QuantitativeCompositionProvenance, QuantitativeRange, TrackLayoutOptions, TrackLayoutStats,
    compose_quantitative_records, copy_number_sample_order, copy_number_track_height,
    gwas_point_color, gwas_point_y, layout_copy_number, layout_features, layout_gwas,
    layout_interactions, layout_mutations, layout_quantitative, layout_variants,
    mutation_track_height, quantitative_data_range,
};
