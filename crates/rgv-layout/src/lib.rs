//! CPU-side layout and immutable display-list primitives.

mod alignment_layout;
mod display;
mod genome_context;
mod track_layout;

pub use alignment_layout::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentLayout, AlignmentLayoutOptions,
    AlignmentLayoutStats, AlignmentOrderDirection, AlignmentRowLayout, AlignmentSortMode,
    BisulfiteContext, JunctionAnnotationLookup, JunctionAnnotationStatus, MAX_ANNOTATED_JUNCTIONS,
    MAX_HIDDEN_MODIFICATION_CODES, MAX_RENDERED_BISULFITE_MARKS, MAX_RENDERED_JUNCTION_LABELS,
    MAX_RENDERED_SBX_TAIL_MARKS, MAX_RENDERED_SMRT_KINETICS_MARKS,
    MAX_RENDERED_ULTIMA_QUALITY_MARKS, SbxDisplayOptions, SbxSimplexTailMode, SmrtKineticsMode,
    UltimaDisplayOptions, UltimaQualityMode, alignment_read_name_hash, assign_alignment_rows,
    junction_count_value, layout_alignment_query, layout_alignment_query_with_rows,
    layout_alignment_query_with_rows_and_annotations, layout_reference_slice,
    layout_reference_slice_oriented, layout_reference_translation, layout_sashimi,
    modification_code_color, ultima_quality_color,
};
pub use display::{
    DisplayList, READ_DETAIL_OBJECT_BIT, RectPrimitive, Rgba, TextPrimitive, read_detail_object_id,
    record_object_id,
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
