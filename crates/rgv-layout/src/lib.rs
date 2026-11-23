//! CPU-side layout and immutable display-list primitives.

mod alignment_style;
mod display;
mod feature_presentation;
mod genome_context;
mod object_id;
mod reference_layout;
mod sashimi_layout;
mod track_layout;
mod track_world_layout;

pub use alignment_style::{
    MAX_HIDDEN_MODIFICATION_CODES, alignment_read_name_hash, modification_code_color,
    ultima_quality_color,
};
pub use display::{
    Direction, DisplayList, JunctionArcGeometrySample, RectPrimitive, RectShape, Rgba,
    TextPrimitive, breakend_bracket_dimensions, junction_arc_distance_to_point,
    junction_arc_geometry_at_point, junction_arc_stroke_width, read_arrow_head_width,
};
pub use feature_presentation::{
    FeaturePresentation, MAX_FEATURE_LABEL_CHARACTERS, representative_label,
};
pub use genome_context::{
    ContigAxis, CoordinateRulerLayout, CoordinateRulerOptions, IdeogramCentromere, IdeogramLayout,
    IdeogramSegment, RulerTick, format_genomic_position, layout_coordinate_ruler, layout_ideogram,
};
pub use object_id::{NO_OBJECT_ID, ObjectIdAllocator, ObjectIdRange, offset_object_id};
pub use reference_layout::{
    MINIMUM_BASE_GLYPH_PIXEL_WIDTH, layout_reference_slice, layout_reference_slice_oriented,
    layout_reference_translation, layout_reference_translation_in_contig,
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
    layout_sashimi_with_presentation,
};
pub use track_layout::{
    COPY_NUMBER_HEADER_HEIGHT, CopyNumberLayout, CopyNumberVisibleRow,
    MAX_COPY_NUMBER_VALUE_LABELS, MAX_FEATURE_LABELS, MAX_INTERACTION_LABELS, MAX_MUTATION_LABELS,
    MAX_QUANTITATIVE_WINDOWS, MAX_RAW_QUANTITATIVE_RECORDS, MAX_RENDERED_COPY_NUMBER_SEGMENTS,
    MAX_RENDERED_GWAS_POINTS, MAX_RENDERED_INTERACTIONS, MAX_RENDERED_MUTATIONS,
    MUTATION_HEADER_HEIGHT, MutationLayout, MutationVisibleRow, QuantitativeComposition,
    QuantitativeCompositionProvenance, QuantitativeRange, TrackLayoutOptions, TrackLayoutStats,
    compose_quantitative_records, copy_number_sample_order, copy_number_track_height,
    gwas_point_color, gwas_point_y, layout_copy_number, layout_features,
    layout_features_with_label_widths, layout_gwas, layout_interactions, layout_mutations,
    layout_presented_features, layout_quantitative, layout_variants, mutation_track_height,
    quantitative_data_range,
};
pub use track_world_layout::{
    MAXIMUM_WORLD_LABEL_CHARACTERS, PlacedTrackLabel, TrackLabelView, TrackSceneHit,
    TrackScenePoint, TrackSceneProbe, TrackSceneRowPosition, WORLD_FEATURE_ROW_PADDING_POINTS,
    WorldRowItem, cluster_closure_extent, hit_test_track_scene, hit_test_track_scene_at,
    lay_out_bin_tile, lay_out_feature_tile, lay_out_interaction_tile,
    lay_out_variant_tile, pack_seam_stable_rows, place_track_labels, world_feature_row_padding,
};
