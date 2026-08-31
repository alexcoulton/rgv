//! CPU-side layout and immutable display-list primitives.

mod alignment_style;
mod display;
mod genome_context;
mod reference_layout;
mod sashimi_layout;
mod track_layout;

pub use alignment_style::{
    MAX_HIDDEN_MODIFICATION_CODES, alignment_read_name_hash, modification_code_color,
    ultima_quality_color,
};
pub use display::{
    Direction, DisplayList, JunctionArcGeometrySample, RectPrimitive, RectShape, Rgba,
    TextPrimitive, breakend_bracket_dimensions, junction_arc_distance_to_point,
    junction_arc_geometry_at_point, junction_arc_stroke_width, read_arrow_head_width,
};
pub use genome_context::{
    ContigAxis, CoordinateRulerLayout, CoordinateRulerOptions, IdeogramCentromere, IdeogramLayout,
    IdeogramSegment, RulerTick, format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};
pub use reference_layout::{
    MINIMUM_BASE_GLYPH_PIXEL_WIDTH, layout_reference_slice, layout_reference_slice_oriented,
    layout_reference_translation,
};
pub use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    BisulfiteContext, SbxDisplayOptions, SbxSimplexTailMode, SmrtKineticsMode,
    UltimaDisplayOptions, UltimaQualityMode,
};
pub use sashimi_layout::{
    JunctionAnnotationLookup, JunctionAnnotationStatus, JunctionArcComponent,
    MAX_ANNOTATED_JUNCTIONS, MAX_RENDERED_JUNCTION_LABELS, MAX_RENDERED_JUNCTIONS, SashimiLayout,
    SashimiLayoutOptions, SashimiLayoutStats, junction_count_value, layout_sashimi,
};
pub use track_layout::{
    COPY_NUMBER_HEADER_HEIGHT, CopyNumberLayout, CopyNumberVisibleRow,
    MAX_COPY_NUMBER_VALUE_LABELS, MAX_INTERACTION_LABELS, MAX_MUTATION_LABELS,
    MAX_QUANTITATIVE_WINDOWS, MAX_RAW_QUANTITATIVE_RECORDS, MAX_RENDERED_COPY_NUMBER_SEGMENTS,
    MAX_RENDERED_GWAS_POINTS, MAX_RENDERED_INTERACTIONS, MAX_RENDERED_MUTATIONS,
    MUTATION_HEADER_HEIGHT, MutationLayout, MutationVisibleRow, QuantitativeComposition,
    QuantitativeCompositionProvenance, QuantitativeRange, TrackLayoutOptions, TrackLayoutStats,
    compose_quantitative_records, copy_number_sample_order, copy_number_track_height,
    gwas_point_color, gwas_point_y, layout_copy_number, layout_features,
    layout_features_with_label_widths, layout_gwas, layout_interactions, layout_mutations,
    layout_quantitative, layout_variants, mutation_track_height, quantitative_data_range,
};
