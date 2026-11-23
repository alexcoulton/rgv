use std::collections::{BTreeMap, BTreeSet};

use rgv_core::{
    CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode, FeatureDisplayMode,
    GwasColorMode, GwasTrackSettings, InteractionDisplayMode, InteractionTrackSettings,
    MutationColorMode, MutationTrackSettings, QuantitativeArithmetic, QuantitativeDisplayMode,
    QuantitativeMissingMode, QuantitativeScaleMode, QuantitativeScaleTransform,
    QuantitativeTrackSettings, QuantitativeWindowFunction, VariantColorMode, VariantTrackSettings,
    Viewport,
};
use rgv_model::{
    CopyNumberQuery, CopyNumberValueSemantics, FeatureRecord, GwasRecord, InteractionRecord,
    MutationQuery, QuantitativeRecord, Strand, VariantRecord,
};

use crate::{DisplayList, FeaturePresentation, RectPrimitive, RectShape, Rgba, TextPrimitive};

pub const MAX_QUANTITATIVE_WINDOWS: usize = 4_096;
pub const MAX_RAW_QUANTITATIVE_RECORDS: usize = 16_384;
pub const MAX_RENDERED_INTERACTIONS: usize = 2_048;
pub const MAX_INTERACTION_LABELS: usize = 512;
pub const MAX_FEATURE_LABELS: usize = 512;
pub const MAX_FEATURE_DIRECTION_MARKERS: usize = 16_384;
pub const MAX_RENDERED_COPY_NUMBER_SEGMENTS: usize = 32_768;
pub const MAX_COPY_NUMBER_VALUE_LABELS: usize = 2_048;
pub const COPY_NUMBER_HEADER_HEIGHT: f32 = 24.0;
pub const MUTATION_HEADER_HEIGHT: f32 = 24.0;
pub const MAX_RENDERED_MUTATIONS: usize = 32_768;
pub const MAX_MUTATION_LABELS: usize = 2_048;
pub const MAX_RENDERED_GWAS_POINTS: usize = 250_000;
/// How far beyond either viewport edge, in viewport widths, an interaction
/// arc endpoint may be placed before it is clamped.
const INTERACTION_ARC_OFFSCREEN_MARGIN_VIEWPORTS: f64 = 4.0;
const MINIMUM_INTERACTION_ARC_HEIGHT: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLayoutOptions {
    pub top: f32,
    pub height: f32,
    pub maximum_rows: usize,
    pub object_id_base: u32,
    pub selected_record: Option<usize>,
    pub feature_display_mode: FeatureDisplayMode,
}

impl Default for TrackLayoutOptions {
    fn default() -> Self {
        Self {
            top: 64.0,
            height: 96.0,
            maximum_rows: 12,
            object_id_base: 1_000_000,
            selected_record: None,
            feature_display_mode: FeatureDisplayMode::Expanded,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackLayoutStats {
    pub input_records: usize,
    pub rendered_records: usize,
    pub hidden_records: usize,
    pub primitive_count: usize,
    pub quantitative_minimum: Option<f32>,
    pub quantitative_maximum: Option<f32>,
    pub missing_windows: usize,
    pub non_finite_records: usize,
    pub aggregated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantitativeRange {
    pub minimum: f32,
    pub maximum: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CopyNumberVisibleRow {
    pub sample_index: usize,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopyNumberLayout {
    pub display_list: DisplayList,
    pub stats: TrackLayoutStats,
    pub visible_rows: Vec<CopyNumberVisibleRow>,
    pub sample_order: Vec<usize>,
    pub range: Option<QuantitativeRange>,
    pub baseline: f32,
    pub value_semantics: CopyNumberValueSemantics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MutationVisibleRow {
    pub sample_index: usize,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MutationLayout {
    pub display_list: DisplayList,
    pub stats: TrackLayoutStats,
    pub visible_rows: Vec<MutationVisibleRow>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QuantitativeCompositionProvenance {
    pub left_records: usize,
    pub right_records: usize,
    pub output_records: usize,
    pub missing_operand_segments: usize,
    pub divide_by_zero_segments: usize,
    pub non_finite_results: usize,
    pub records_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuantitativeComposition {
    pub records: Vec<QuantitativeRecord>,
    pub provenance: QuantitativeCompositionProvenance,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum QuantitativeSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum QuantitativeEventKind {
    End,
    Start,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct QuantitativeEvent {
    position: u64,
    kind: QuantitativeEventKind,
    side: QuantitativeSide,
    key: (u64, u64, usize),
}

#[derive(Clone, Copy, Debug, Default)]
struct QuantitativeAccumulator {
    minimum: f32,
    maximum: f32,
    weighted_sum: f64,
    weight: u64,
    present: bool,
}

impl QuantitativeRange {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            minimum: self.minimum.min(other.minimum),
            maximum: self.maximum.max(other.maximum),
        }
    }
}

pub fn layout_features(
    features: &[FeatureRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
) -> (DisplayList, TrackLayoutStats) {
    layout_features_with_label_widths(features, viewport, options, None)
}

#[derive(Clone, Debug)]
struct PlacedFeature {
    record_index: usize,
    row: usize,
}

#[derive(Clone, Debug)]
struct FeatureLabelCandidate {
    row: usize,
    text: String,
    text_width: f32,
    center: f32,
    selected: bool,
}

pub fn layout_features_with_label_widths(
    features: &[FeatureRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
    measured_label_widths: Option<&[f32]>,
) -> (DisplayList, TrackLayoutStats) {
    layout_presented_features(features, viewport, options, measured_label_widths, None)
}

/// [`layout_features_with_label_widths`] for a gene-model track: records the `presentation`
/// hides take no row and are not drawn, and its labels replace the records' own. Measured label
/// widths, when given, are of the presented labels.
#[allow(clippy::too_many_lines)]
pub fn layout_presented_features(
    features: &[FeatureRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
    measured_label_widths: Option<&[f32]>,
    presentation: Option<&FeaturePresentation>,
) -> (DisplayList, TrackLayoutStats) {
    let mut display = DisplayList::default();
    let row_count = match options.feature_display_mode {
        FeatureDisplayMode::Collapsed => 1,
        FeatureDisplayMode::Squished => options.maximum_rows.saturating_mul(2).max(1),
        FeatureDisplayMode::Expanded => options.maximum_rows.max(1),
    };
    let row_height = options.height / row_count as f32;
    let (placed, overflow) = pack_feature_rows(
        features,
        viewport,
        row_count,
        options.feature_display_mode == FeatureDisplayMode::Collapsed,
        |index| presentation.is_some_and(|presentation| presentation.is_hidden(index)),
    );
    let hidden = overflow.len();

    let viewport_width = viewport.pixel_width() as f32;
    let mut candidates = Vec::new();
    for placement in &placed {
        let feature = &features[placement.record_index];
        let selected = options.selected_record == Some(placement.record_index);
        // Untyped records (BED, bigBed, peaks) are whole features and are
        // labelled by name. Typed records are labelled only at gene or
        // transcript level, so exons and CDS parts do not repeat the label.
        let labelable = feature.feature_type.as_deref().is_none_or(|feature_type| {
            ["gene", "transcript", "mrna", "primary_transcript"]
                .iter()
                .any(|candidate| feature_type.eq_ignore_ascii_case(candidate))
        });
        if !labelable || (row_height < 16.0 && !selected) {
            continue;
        }
        let text = FeaturePresentation::label_text(presentation, features, placement.record_index);
        if text.is_empty() {
            continue;
        }
        let text_width = measured_label_widths
            .and_then(|widths| widths.get(placement.record_index))
            .copied()
            .filter(|width| width.is_finite() && *width >= 0.0)
            .unwrap_or_else(|| text.chars().count() as f32 * 9.0 * 0.62);
        if text_width + 4.0 > viewport_width {
            continue;
        }
        let (x, width) = clipped_geometry(feature.start, feature.end, viewport);
        if !selected && width < 3.0 {
            continue;
        }
        let half_width = text_width * 0.5;
        let center = (x + width * 0.5).clamp(
            half_width + 2.0,
            (viewport_width - half_width - 2.0).max(half_width + 2.0),
        );
        candidates.push(FeatureLabelCandidate {
            row: placement.row,
            text,
            text_width,
            center,
            selected,
        });
    }

    let mut occupied = vec![Vec::<(f32, f32)>::new(); row_count];
    let mut accepted = Vec::new();
    let mut ordinary_labels = 0_usize;
    for selected_pass in [true, false] {
        for candidate in candidates
            .iter()
            .filter(|candidate| candidate.selected == selected_pass)
        {
            if !candidate.selected && ordinary_labels == MAX_FEATURE_LABELS {
                break;
            }
            let half_width = candidate.text_width * 0.5;
            let interval = (candidate.center - half_width, candidate.center + half_width);
            let collision = occupied[candidate.row]
                .iter()
                .any(|existing| interval.0 < existing.1 + 6.0 && interval.1 + 6.0 > existing.0);
            if collision && !candidate.selected {
                continue;
            }
            occupied[candidate.row].push(interval);
            accepted.push(candidate.clone());
            if !candidate.selected {
                ordinary_labels = ordinary_labels.saturating_add(1);
            }
        }
    }
    let row_has_label = occupied
        .iter()
        .map(|intervals| !intervals.is_empty())
        .collect::<Vec<_>>();

    let mut direction_marker_budget = MAX_FEATURE_DIRECTION_MARKERS;
    for placement in &placed {
        let index = placement.record_index;
        let feature = &features[index];
        let row = placement.row;
        let row_top = options.top + row as f32 * row_height;
        let selected = options.selected_record == Some(index);
        let label_band = if row_has_label[row] { 10.0 } else { 0.0 };
        let y = row_top + label_band + 2.0;
        let object_id = crate::offset_object_id(options.object_id_base, index);
        add_feature(
            &mut display,
            feature,
            viewport,
            y,
            (row_height - label_band - 4.0).max(2.0),
            object_id,
            selected,
            &mut direction_marker_budget,
        );
    }
    for label in accepted {
        display.texts.push(TextPrimitive {
            x: label.center,
            y: options.top + label.row as f32 * row_height + 5.5,
            text: label.text,
            font_size: 9.0,
            color: Rgba::new(0.16, 0.18, 0.22, 0.96),
        });
    }
    add_feature_overflow_indicator(&mut display, features, &overflow, viewport, options);
    let primitive_count = display.rectangles.len();
    (
        display,
        TrackLayoutStats {
            input_records: features.len(),
            rendered_records: placed.len(),
            hidden_records: hidden,
            primitive_count,
            ..TrackLayoutStats::default()
        },
    )
}

/// Minimum horizontal gap, in pixels, between two features sharing a row.
const FEATURE_ROW_PADDING_PIXELS: f64 = 2.0;
/// Height of the strip that marks where features did not fit in any row.
const FEATURE_OVERFLOW_INDICATOR_HEIGHT: f32 = 2.0;
const MAX_FEATURE_OVERFLOW_INDICATORS: usize = 1_024;

/// Assigns every viewport-intersecting feature a row, returning the placed
/// features in input order and the indices of those that fit in no row.
///
/// Rows are packed over *all* supplied records in genomic order, not just the
/// visible ones in input order: a feature's row then depends on the loaded
/// neighbourhood rather than on which features happen to intersect the
/// viewport, so features keep their rows while panning, and unsorted input
/// packs as tightly as sorted input. Features on one row are separated by at
/// least [`FEATURE_ROW_PADDING_PIXELS`] so abutting features stay distinct.
fn pack_feature_rows(
    features: &[FeatureRecord],
    viewport: Viewport,
    row_count: usize,
    collapsed: bool,
    hidden: impl Fn(usize) -> bool,
) -> (Vec<PlacedFeature>, Vec<usize>) {
    let interval = viewport.interval();
    let visible =
        |feature: &FeatureRecord| feature.end > interval.start() && feature.start < interval.end();
    if collapsed {
        let placed = features
            .iter()
            .enumerate()
            .filter(|(index, feature)| !hidden(*index) && visible(feature))
            .map(|(record_index, _)| PlacedFeature {
                record_index,
                row: 0,
            })
            .collect();
        return (placed, Vec::new());
    }
    let padding = (viewport.bases_per_pixel() * FEATURE_ROW_PADDING_PIXELS).ceil() as u64;
    let mut order = (0..features.len())
        .filter(|index| !hidden(*index))
        .collect::<Vec<_>>();
    order.sort_by_key(|index| (features[*index].start, features[*index].end, *index));
    // Exclusive end of the last feature on each row, including its padding.
    let mut row_ends = vec![0_u64; row_count];
    let mut placed = Vec::new();
    let mut overflow = Vec::new();
    for index in order {
        let feature = &features[index];
        let row = row_ends.iter().position(|end| *end <= feature.start);
        if let Some(row) = row {
            row_ends[row] = row_ends[row].max(feature.end.saturating_add(padding));
        }
        if !visible(feature) {
            continue;
        }
        match row {
            Some(row) => placed.push(PlacedFeature {
                record_index: index,
                row,
            }),
            None => overflow.push(index),
        }
    }
    // Draw order (and therefore overlap and marker budgets) follows the input.
    placed.sort_by_key(|placement| placement.record_index);
    overflow.sort_unstable();
    (placed, overflow)
}

/// Marks the extent of features that fit in no row with a strip along the
/// bottom edge of the track, so a dense region is visibly incomplete instead
/// of silently missing records. The strip is not pickable.
fn add_feature_overflow_indicator(
    display: &mut DisplayList,
    features: &[FeatureRecord],
    overflow: &[usize],
    viewport: Viewport,
    options: TrackLayoutOptions,
) {
    let mut spans = overflow
        .iter()
        .map(|index| clipped_geometry(features[*index].start, features[*index].end, viewport))
        .map(|(x, width)| (x, x + width.max(2.0)))
        .collect::<Vec<_>>();
    spans.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut merged = Vec::<(f32, f32)>::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 + 1.0 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    for (start, end) in merged.into_iter().take(MAX_FEATURE_OVERFLOW_INDICATORS) {
        display.rectangles.push(RectPrimitive {
            x: start,
            y: options.top + options.height - FEATURE_OVERFLOW_INDICATOR_HEIGHT,
            width: end - start,
            height: FEATURE_OVERFLOW_INDICATOR_HEIGHT,
            color: Rgba::new(0.80, 0.33, 0.20, 0.85),
            object_id: crate::NO_OBJECT_ID,
            shape: RectShape::Rectangle,
        });
    }
}

pub fn layout_variants(
    variants: &[VariantRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &VariantTrackSettings,
) -> (DisplayList, TrackLayoutStats) {
    let mut display = DisplayList::default();
    let baseline = options.top + options.height - 4.0;
    let mut rendered = 0;
    for (index, variant) in variants.iter().enumerate() {
        if variant.end <= viewport.interval().start() || variant.start >= viewport.interval().end()
        {
            continue;
        }
        // Geometry is clipped to the viewport before the cast to `f32`: a
        // long variant whose start lies far off-screen would otherwise lose
        // pixel precision or overflow the renderer's coordinate range.
        let start_visible = variant.start >= viewport.interval().start();
        let (x, span_width) = clipped_geometry(variant.start, variant.end, viewport);
        let right = x + span_width;
        let object_id = crate::offset_object_id(options.object_id_base, index);
        let color = match settings.color_mode {
            VariantColorMode::Type => variant_color(
                variant
                    .variant_types
                    .first()
                    .unwrap_or(&rgv_model::VariantType::Complex),
            ),
            VariantColorMode::Genotype => genotype_color(variant),
        };
        if options.selected_record == Some(index) {
            display.rectangles.push(RectPrimitive {
                x: x - 6.0,
                y: options.top,
                width: if start_visible {
                    12.0
                } else {
                    span_width + 6.0
                },
                height: 12.0,
                color: Rgba::new(0.10, 0.28, 0.62, 0.24),
                object_id,
                shape: Default::default(),
            });
        }
        // The stem and head mark the variant's start position; when that is
        // off-screen only the clipped span below remains.
        if start_visible {
            display.rectangles.push(RectPrimitive {
                x: x - 1.0,
                y: options.top + 5.0,
                width: 2.0,
                height: (options.height - 12.0).max(2.0),
                color: with_alpha(color, 0.72),
                object_id,
                shape: Default::default(),
            });
            display.rectangles.push(RectPrimitive {
                x: x - 4.0,
                y: options.top + 1.0,
                width: 8.0,
                height: 8.0,
                color,
                object_id,
                shape: variant
                    .breakends
                    .first()
                    .map(|partner| breakend_shape(partner.local_join_after_position))
                    .or_else(|| {
                        variant.breakpoint_orientations.first().map(|orientation| {
                            breakend_shape(orientation.local_join_after_position)
                        })
                    })
                    .unwrap_or(RectShape::Rectangle),
            });
        }
        if variant.breakends.is_empty() {
            display.rectangles.push(RectPrimitive {
                x,
                y: baseline,
                width: span_width.max(2.0),
                height: 3.0,
                color,
                object_id,
                shape: RectShape::Rectangle,
            });
            if let Some(orientation) = variant.breakpoint_orientations.first()
                && variant.end > viewport.interval().start()
                && variant.end <= viewport.interval().end()
            {
                display.rectangles.push(RectPrimitive {
                    x: right - 4.0,
                    y: baseline - 3.0,
                    width: 8.0,
                    height: 8.0,
                    color,
                    object_id,
                    shape: breakend_shape(orientation.partner_join_after_position),
                });
            }
        } else {
            add_breakend_partners(
                &mut display,
                variant,
                viewport,
                x,
                baseline,
                color,
                object_id,
            );
        }
        rendered += 1;
    }
    let primitive_count = display.rectangles.len();
    (
        display,
        TrackLayoutStats {
            input_records: variants.len(),
            rendered_records: rendered,
            hidden_records: variants.len().saturating_sub(rendered),
            primitive_count,
            ..TrackLayoutStats::default()
        },
    )
}

const fn breakend_shape(join_after_position: bool) -> RectShape {
    if join_after_position {
        RectShape::BreakendJoinAfter
    } else {
        RectShape::BreakendJoinBefore
    }
}

fn add_breakend_partners(
    display: &mut DisplayList,
    variant: &VariantRecord,
    viewport: Viewport,
    local_x: f32,
    baseline: f32,
    color: Rgba,
    object_id: u32,
) {
    const MAX_PARTNERS: usize = 64;
    const MAX_OFFSCREEN_PARTNER_LABELS: usize = 8;
    const OFFSCREEN_PARTNER_ROW_HEIGHT: f32 = 10.0;
    let viewport_width = viewport.pixel_width() as f32;
    let mut offscreen_partner_count = 0_usize;
    for partner in variant.breakends.iter().take(MAX_PARTNERS) {
        let partner_is_visible = partner.contig == variant.contig
            && partner.position >= viewport.interval().start()
            && partner.position < viewport.interval().end();
        if partner_is_visible {
            let partner_x = viewport.pixel_at_genomic(partner.position) as f32;
            let left = local_x.min(partner_x);
            let right = local_x.max(partner_x);
            display.rectangles.push(RectPrimitive {
                x: left,
                y: baseline,
                width: (right - left).max(2.0),
                height: 2.0,
                color: with_alpha(color, 0.72),
                object_id,
                shape: RectShape::Rectangle,
            });
            display.rectangles.push(RectPrimitive {
                x: partner_x - 4.0,
                y: baseline - 3.0,
                width: 8.0,
                height: 8.0,
                color,
                object_id,
                shape: breakend_shape(partner.partner_join_after_position),
            });
        } else {
            let marker_on_left = if partner.contig == variant.contig {
                partner.position < viewport.interval().start()
            } else {
                local_x > viewport_width * 0.5
            };
            let marker_width = 8.0_f32.min(viewport_width);
            let row = offscreen_partner_count.min(MAX_OFFSCREEN_PARTNER_LABELS - 1);
            let marker_y = baseline - 3.0 - row as f32 * OFFSCREEN_PARTNER_ROW_HEIGHT;
            let marker_x = if marker_on_left {
                0.0
            } else {
                viewport_width - marker_width
            };
            if partner.contig == variant.contig {
                let connector_left = local_x.min(marker_x + marker_width * 0.5);
                let connector_right = local_x.max(marker_x + marker_width * 0.5);
                display.rectangles.push(RectPrimitive {
                    x: connector_left,
                    y: marker_y + 3.0,
                    width: (connector_right - connector_left).max(2.0),
                    height: 2.0,
                    color: with_alpha(color, 0.72),
                    object_id,
                    shape: RectShape::Rectangle,
                });
            }
            display.rectangles.push(RectPrimitive {
                x: marker_x,
                y: marker_y,
                width: marker_width,
                height: 8.0,
                color,
                object_id,
                shape: breakend_shape(partner.partner_join_after_position),
            });

            if offscreen_partner_count >= MAX_OFFSCREEN_PARTNER_LABELS {
                offscreen_partner_count = offscreen_partner_count.saturating_add(1);
                continue;
            }
            let local_side = if partner.local_join_after_position {
                "after"
            } else {
                "before"
            };
            let partner_side = if partner.partner_join_after_position {
                "after"
            } else {
                "before"
            };
            let text = bounded_interaction_label(&format!(
                "ALT {} {local_side} <-> {}:{} {partner_side}",
                partner.alternate_index + 1,
                partner.contig,
                partner.position.saturating_add(1)
            ));
            let half_text_width = (text.chars().count() as f32 * 9.0 * 0.31)
                .min(viewport_width * 0.5)
                .max(0.0);
            display.texts.push(TextPrimitive {
                x: local_x.clamp(half_text_width, viewport_width - half_text_width),
                y: (baseline - 7.0 - row as f32 * OFFSCREEN_PARTNER_ROW_HEIGHT).max(7.0),
                text,
                font_size: 9.0,
                color: Rgba::new(0.18, 0.16, 0.22, 0.92),
            });
            offscreen_partner_count = offscreen_partner_count.saturating_add(1);
        }
    }
}

pub fn layout_gwas(
    records: &[GwasRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &GwasTrackSettings,
) -> (DisplayList, TrackLayoutStats) {
    let mut display = DisplayList::default();
    let plot_bottom = options.top + (options.height - 12.0).max(8.0);
    display.rectangles.push(RectPrimitive {
        x: 0.0,
        y: plot_bottom,
        width: viewport.pixel_width() as f32,
        height: 1.0,
        color: Rgba::new(0.72, 0.74, 0.77, 0.85),
        object_id: 0,
        shape: Default::default(),
    });
    display.texts.push(TextPrimitive {
        x: 18.0,
        y: options.top + 7.0,
        text: format!("{:.1}", settings.maximum_score),
        font_size: 9.0,
        color: Rgba::new(0.20, 0.22, 0.25, 0.90),
    });
    display.texts.push(TextPrimitive {
        x: 6.0,
        y: plot_bottom - 5.0,
        text: "0".to_owned(),
        font_size: 9.0,
        color: Rgba::new(0.20, 0.22, 0.25, 0.90),
    });
    let point_size = settings.point_size.clamp(1.0, 16.0);
    let mut rendered = 0_usize;
    for (index, record) in records.iter().enumerate() {
        if record.position < viewport.interval().start()
            || record.position >= viewport.interval().end()
        {
            continue;
        }
        if rendered == MAX_RENDERED_GWAS_POINTS {
            break;
        }
        let x = viewport.pixel_at_genomic(record.position) as f32;
        let y = gwas_point_y(
            record.minus_log10_p,
            options.top,
            options.height,
            settings.maximum_score,
        );
        let object_id = crate::offset_object_id(options.object_id_base, index);
        if options.selected_record == Some(index) {
            let halo = point_size + 8.0;
            display.rectangles.push(RectPrimitive {
                x: x - halo * 0.5,
                y: y - halo * 0.5,
                width: halo,
                height: halo,
                color: Rgba::new(0.10, 0.28, 0.62, 0.20),
                object_id,
                shape: Default::default(),
            });
        }
        display.rectangles.push(RectPrimitive {
            x: x - point_size * 0.5,
            y: y - point_size * 0.5,
            width: point_size,
            height: point_size,
            color: gwas_point_color(&record.contig, settings),
            object_id,
            shape: Default::default(),
        });
        rendered = rendered.saturating_add(1);
    }
    let primitive_count = display.rectangles.len();
    (
        display,
        TrackLayoutStats {
            input_records: records.len(),
            rendered_records: rendered,
            hidden_records: records.len().saturating_sub(rendered),
            primitive_count,
            quantitative_minimum: Some(0.0),
            quantitative_maximum: Some(settings.maximum_score),
            ..TrackLayoutStats::default()
        },
    )
}

#[must_use]
pub fn gwas_point_y(score: f32, top: f32, height: f32, maximum_score: f32) -> f32 {
    let plot_top = top + 8.0;
    let plot_bottom = top + (height - 12.0).max(8.0);
    let maximum = maximum_score.clamp(1.0, 1_000.0);
    let bounded = if score.is_infinite() && score.is_sign_positive() {
        maximum
    } else {
        score.clamp(0.0, maximum)
    };
    plot_bottom - (bounded / maximum) * (plot_bottom - plot_top).max(1.0)
}

#[must_use]
pub fn gwas_point_color(contig: &str, settings: &GwasTrackSettings) -> Rgba {
    let chromosome_index = gwas_chromosome_index(contig);
    let bytes = match settings.color_mode {
        GwasColorMode::Uniform => settings.uniform_color,
        GwasColorMode::Alternating => {
            if chromosome_index.is_multiple_of(2) {
                settings.alternating_color_one
            } else {
                settings.alternating_color_two
            }
        }
        GwasColorMode::Chromosome => {
            const PALETTE: [[u8; 4]; 12] = [
                [45, 92, 175, 230],
                [197, 78, 82, 230],
                [69, 145, 91, 230],
                [132, 91, 165, 230],
                [220, 135, 50, 230],
                [52, 142, 150, 230],
                [179, 91, 147, 230],
                [103, 111, 124, 230],
                [102, 153, 204, 230],
                [204, 102, 119, 230],
                [92, 172, 130, 230],
                [151, 120, 190, 230],
            ];
            PALETTE[chromosome_index % PALETTE.len()]
        }
    };
    rgba_from_bytes(bytes)
}

fn gwas_chromosome_index(contig: &str) -> usize {
    let name = contig
        .strip_prefix("chr")
        .or_else(|| contig.strip_prefix("CHR"))
        .unwrap_or(contig);
    if let Ok(number) = name.parse::<usize>() {
        return number.saturating_sub(1);
    }
    match name.to_ascii_uppercase().as_str() {
        "X" => 22,
        "Y" => 23,
        "M" | "MT" => 24,
        _ => name
            .as_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            }) as usize,
    }
}

#[allow(clippy::too_many_lines)]
pub fn layout_interactions(
    interactions: &[InteractionRecord],
    contig: &str,
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &InteractionTrackSettings,
) -> (DisplayList, TrackLayoutStats) {
    let mut display = DisplayList::default();
    let interval = viewport.interval();
    let passes_score = |record: &InteractionRecord| {
        settings
            .minimum_score
            .is_none_or(|minimum| record.score.is_some_and(|score| score >= minimum))
    };
    // Exactly the records the loop below draws. The render cap must count
    // these, not the input prefix: a query returns every contig's records in
    // file order, so capping first starved everything after the first
    // `MAX_RENDERED_INTERACTIONS` records regardless of what was on screen.
    let drawable = |record: &InteractionRecord| {
        let source_here = record.source.contig == contig;
        let target_here = record.target.contig == contig;
        if source_here && target_here {
            let source = endpoint_center(record.source.start, record.source.end);
            let target = endpoint_center(record.target.start, record.target.end);
            source.max(target) >= interval.start() && source.min(target) < interval.end()
        } else if source_here || target_here {
            let local = if source_here {
                &record.source
            } else {
                &record.target
            };
            local.end > interval.start() && local.start < interval.end()
        } else {
            false
        }
    };
    // The automatic scale stays a property of the loaded records so arc
    // heights do not rescale while panning.
    let automatic_maximum = interactions
        .iter()
        .filter(|record| passes_score(record))
        .map(interaction_magnitude)
        .fold(0.0_f32, f32::max)
        .max(1.0);
    let eligible = interactions
        .iter()
        .enumerate()
        .filter(|(_, record)| passes_score(record) && drawable(record))
        .take(MAX_RENDERED_INTERACTIONS)
        .collect::<Vec<_>>();
    let scale_maximum = settings
        .fixed_maximum_score
        .unwrap_or(automatic_maximum)
        .max(f32::EPSILON);
    let baseline = options.top + options.height - 5.0;
    let mut rendered = 0_usize;
    let mut labels = 0_usize;
    let eligible_count = eligible.len();
    for (index, record) in eligible {
        let source_here = record.source.contig == contig;
        let target_here = record.target.contig == contig;
        if !source_here && !target_here {
            continue;
        }
        let object_id = crate::offset_object_id(options.object_id_base, index);
        let color = record
            .color
            .map_or(Rgba::new(0.48, 0.30, 0.76, 0.86), rgba_from_bytes);
        let magnitude = (interaction_magnitude(record) / scale_maximum).clamp(0.05, 1.0);
        if options.selected_record == Some(index) {
            display.rectangles.push(RectPrimitive {
                x: 0.0,
                y: options.top,
                width: viewport.pixel_width() as f32,
                height: options.height,
                color: Rgba::new(0.10, 0.28, 0.62, 0.10),
                object_id: 0,
                shape: Default::default(),
            });
        }
        if source_here && target_here {
            let source = endpoint_center(record.source.start, record.source.end);
            let target = endpoint_center(record.target.start, record.target.end);
            let left = source.min(target);
            let right = source.max(target);
            if right < viewport.interval().start() || left >= viewport.interval().end() {
                continue;
            }
            // Endpoints far outside the viewport are pulled in to a bounded
            // margin before the cast to `f32`, so a megabase-scale arc viewed
            // at base resolution keeps finite, precise geometry.
            let pixel_width = viewport.pixel_width();
            let margin = pixel_width * INTERACTION_ARC_OFFSCREEN_MARGIN_VIEWPORTS;
            let bounded_x = |position: u64| {
                viewport
                    .pixel_at_genomic(position)
                    .clamp(-margin, pixel_width + margin) as f32
            };
            let left_x = bounded_x(left);
            let right_x = bounded_x(right);
            match settings.display_mode {
                InteractionDisplayMode::Arcs => add_interaction_arc(
                    &mut display,
                    left_x,
                    right_x,
                    baseline,
                    options.height * 0.82 * magnitude,
                    color,
                    object_id,
                ),
                InteractionDisplayMode::Blocks => {
                    let line_start = left_x.max(0.0);
                    let line_end = right_x.min(viewport.pixel_width() as f32);
                    if line_end > line_start {
                        display.rectangles.push(RectPrimitive {
                            x: line_start,
                            y: baseline - 1.0,
                            width: line_end - line_start,
                            height: 2.0,
                            color: Rgba::new(color.red, color.green, color.blue, color.alpha * 0.6),
                            object_id,
                            shape: Default::default(),
                        });
                    }
                }
            }
            let source_shape = if record.directional {
                RectShape::Rectangle
            } else {
                strand_terminal_shape(record.source.strand)
            };
            let target_shape = if record.directional {
                if target < source {
                    RectShape::DirectionalTerminalLeft
                } else {
                    RectShape::DirectionalTerminalRight
                }
            } else {
                strand_terminal_shape(record.target.strand)
            };
            add_interaction_endpoint(
                &mut display,
                record.source.start,
                record.source.end,
                viewport,
                baseline,
                color,
                object_id,
                source_shape,
            );
            if record.directional {
                add_directed_interaction_target(
                    &mut display,
                    &record.source,
                    &record.target,
                    viewport,
                    baseline,
                    color,
                    object_id,
                    target_shape,
                );
            } else {
                add_interaction_endpoint(
                    &mut display,
                    record.target.start,
                    record.target.end,
                    viewport,
                    baseline,
                    color,
                    object_id,
                    target_shape,
                );
            }
            if settings.show_labels
                && labels < MAX_INTERACTION_LABELS
                && let Some(label) = record.name.as_deref()
            {
                display.texts.push(TextPrimitive {
                    x: bounded_label_x(
                        (left_x + right_x) * 0.5,
                        viewport.pixel_width() as f32,
                        40.0,
                    ),
                    y: (baseline - options.height * 0.82 * magnitude - 7.0).max(options.top + 7.0),
                    text: bounded_interaction_label(label),
                    font_size: 10.5,
                    color: Rgba::new(0.18, 0.16, 0.22, 0.92),
                });
                labels = labels.saturating_add(1);
            }
        } else {
            let (local, partner) = if source_here {
                (&record.source, &record.target)
            } else {
                (&record.target, &record.source)
            };
            if local.end <= viewport.interval().start() || local.start >= viewport.interval().end()
            {
                continue;
            }
            let x = viewport.pixel_at_genomic(endpoint_center(local.start, local.end)) as f32;
            display.rectangles.push(RectPrimitive {
                x: x - 1.5,
                y: options.top + 8.0,
                width: 3.0,
                height: (baseline - options.top - 8.0).max(2.0),
                color,
                object_id,
                shape: Default::default(),
            });
            add_interaction_endpoint(
                &mut display,
                local.start,
                local.end,
                viewport,
                baseline,
                color,
                object_id,
                strand_terminal_shape(local.strand),
            );
            if settings.show_labels && labels < MAX_INTERACTION_LABELS {
                let partner_name = partner.name.as_deref().unwrap_or("partner");
                display.texts.push(TextPrimitive {
                    x: bounded_label_x(x, viewport.pixel_width() as f32, 54.0),
                    y: options.top + 7.0,
                    text: bounded_interaction_label(&interaction_cross_contig_label(
                        record,
                        partner_name,
                    )),
                    font_size: 10.5,
                    color: Rgba::new(0.18, 0.16, 0.22, 0.92),
                });
                labels = labels.saturating_add(1);
            }
        }
        rendered = rendered.saturating_add(1);
    }
    let primitive_count = display.rectangles.len();
    debug_assert_eq!(rendered, eligible_count);
    (
        display,
        TrackLayoutStats {
            input_records: interactions.len(),
            rendered_records: rendered,
            hidden_records: interactions.len().saturating_sub(rendered),
            primitive_count,
            ..TrackLayoutStats::default()
        },
    )
}

/// One smooth arc primitive whose endpoints meet `baseline`, replacing a
/// polyline of axis-aligned rectangles that rendered as stair-steps.
fn add_interaction_arc(
    display: &mut DisplayList,
    left: f32,
    right: f32,
    baseline: f32,
    height: f32,
    color: Rgba,
    object_id: u32,
) {
    let height = height.max(MINIMUM_INTERACTION_ARC_HEIGHT);
    display.rectangles.push(RectPrimitive {
        x: left,
        y: baseline - height,
        width: (right - left).max(1.0),
        height,
        color,
        object_id,
        shape: RectShape::JunctionArcUp,
    });
}

fn add_interaction_endpoint(
    display: &mut DisplayList,
    start: u64,
    end: u64,
    viewport: Viewport,
    baseline: f32,
    color: Rgba,
    object_id: u32,
    mut shape: RectShape,
) {
    if (matches!(shape.direction(), Some(crate::Direction::Left))
        && start < viewport.interval().start())
        || (matches!(shape.direction(), Some(crate::Direction::Right))
            && end > viewport.interval().end())
    {
        shape = RectShape::Rectangle;
    }
    let start = start.max(viewport.interval().start());
    let end = end.min(viewport.interval().end());
    if start >= end {
        return;
    }
    let x = viewport.pixel_at_genomic(start) as f32;
    let right = viewport.pixel_at_genomic(end) as f32;
    display.rectangles.push(RectPrimitive {
        x,
        y: baseline - 5.0,
        width: (right - x).max(3.0),
        height: 6.0,
        color,
        object_id,
        shape,
    });
}

#[allow(clippy::too_many_arguments)]
fn add_directed_interaction_target(
    display: &mut DisplayList,
    source: &rgv_model::InteractionEndpoint,
    target: &rgv_model::InteractionEndpoint,
    viewport: Viewport,
    baseline: f32,
    color: Rgba,
    object_id: u32,
    shape: RectShape,
) {
    let target_is_left =
        endpoint_center(target.start, target.end) < endpoint_center(source.start, source.end);
    let target_tip_is_clipped = if target_is_left {
        target.start < viewport.interval().start()
    } else {
        target.end > viewport.interval().end()
    };
    if target_tip_is_clipped {
        let marker_width = 6.0_f32.min(viewport.pixel_width() as f32);
        display.rectangles.push(RectPrimitive {
            x: if target_is_left {
                0.0
            } else {
                viewport.pixel_width() as f32 - marker_width
            },
            y: baseline - 5.0,
            width: marker_width,
            height: 6.0,
            color,
            object_id,
            shape: if target_is_left {
                RectShape::ChevronLeft
            } else {
                RectShape::ChevronRight
            },
        });
    }
    add_interaction_endpoint(
        display,
        target.start,
        target.end,
        viewport,
        baseline,
        color,
        object_id,
        shape,
    );
}

const fn strand_terminal_shape(strand: Strand) -> RectShape {
    match strand {
        Strand::Forward => RectShape::DirectionalTerminalRight,
        Strand::Reverse => RectShape::DirectionalTerminalLeft,
        Strand::Unstranded => RectShape::Rectangle,
    }
}

fn interaction_cross_contig_label(record: &InteractionRecord, partner_name: &str) -> String {
    let source_name = record.source.name.as_deref().unwrap_or("source");
    let target_name = record.target.name.as_deref().unwrap_or(partner_name);
    let separator = if record.directional { "->" } else { "<->" };
    format!(
        "{source_name} {}:{} {separator} {target_name} {}:{}",
        record.source.contig,
        record.source.start.saturating_add(1),
        record.target.contig,
        record.target.start.saturating_add(1),
    )
}

fn interaction_magnitude(record: &InteractionRecord) -> f32 {
    record
        .score
        .or_else(|| record.value.map(|value| value.abs() as f32))
        .filter(|value| value.is_finite())
        .unwrap_or(1.0)
        .abs()
}

fn endpoint_center(start: u64, end: u64) -> u64 {
    start.saturating_add(end.saturating_sub(start) / 2)
}

fn bounded_interaction_label(label: &str) -> String {
    const MAX_LABEL_CHARS: usize = 64;
    let mut characters = label.chars();
    let mut bounded = characters
        .by_ref()
        .take(MAX_LABEL_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn bounded_label_x(x: f32, width: f32, margin: f32) -> f32 {
    if width <= margin * 2.0 {
        width * 0.5
    } else {
        x.clamp(margin, width - margin)
    }
}

#[allow(clippy::too_many_lines)]
pub fn layout_quantitative(
    records: &[QuantitativeRecord],
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &QuantitativeTrackSettings,
    shared_range: Option<QuantitativeRange>,
) -> (DisplayList, TrackLayoutStats) {
    let (windows, aggregated) = quantitative_windows(records, viewport, settings.window_function);
    let non_finite_records = records
        .iter()
        .filter(|record| !record.value.is_finite())
        .count();
    let automatic_range = range_for_windows(&windows, settings.baseline);
    let range = match settings.scale_mode {
        QuantitativeScaleMode::Automatic => automatic_range,
        QuantitativeScaleMode::Fixed => Some(QuantitativeRange {
            minimum: settings.fixed_minimum,
            maximum: settings.fixed_maximum,
        }),
        QuantitativeScaleMode::Shared => shared_range.or(automatic_range),
    };
    let mut display = DisplayList::default();
    let Some(range) = range.filter(|range| {
        range.minimum.is_finite() && range.maximum.is_finite() && range.minimum < range.maximum
    }) else {
        return (
            display,
            TrackLayoutStats {
                input_records: records.len(),
                hidden_records: records.len(),
                non_finite_records,
                aggregated,
                ..TrackLayoutStats::default()
            },
        );
    };
    let positive = with_opacity(rgba_from_bytes(settings.positive_color), settings.opacity);
    let negative = with_opacity(rgba_from_bytes(settings.negative_color), settings.opacity);
    if let Some(selected) = options
        .selected_record
        .and_then(|index| records.get(index))
        .filter(|record| {
            record.end > viewport.interval().start() && record.start < viewport.interval().end()
        })
    {
        let start = selected.start.max(viewport.interval().start());
        let end = selected.end.min(viewport.interval().end());
        let x = viewport.pixel_at_genomic(start) as f32;
        let right = viewport.pixel_at_genomic(end) as f32;
        display.rectangles.push(RectPrimitive {
            x,
            y: options.top,
            width: (right - x).max(2.0),
            height: options.height,
            color: Rgba::new(0.10, 0.28, 0.62, 0.14),
            object_id: 0,
            shape: Default::default(),
        });
    }
    let baseline_y = quantitative_y(settings.baseline, range, settings, options);
    if baseline_y >= options.top && baseline_y <= options.top + options.height {
        display.rectangles.push(RectPrimitive {
            x: 0.0,
            y: baseline_y,
            width: viewport.pixel_width() as f32,
            height: 1.0,
            color: Rgba::new(0.42, 0.45, 0.50, 0.42),
            object_id: 0,
            shape: Default::default(),
        });
    }
    let mut rendered = 0_usize;
    let mut missing = 0_usize;
    let mut previous: Option<(u64, f32)> = None;
    for window in &windows {
        let x = viewport.pixel_at_genomic(window.start) as f32;
        let right = viewport.pixel_at_genomic(window.end) as f32;
        let width = (right - x).max(1.0);
        let object_id = crate::offset_object_id(options.object_id_base, window.identity);
        let Some(value) = window.value else {
            missing = missing.saturating_add(1);
            match settings.missing_mode {
                QuantitativeMissingMode::Gap => {}
                QuantitativeMissingMode::Baseline => display.rectangles.push(RectPrimitive {
                    x,
                    y: baseline_y,
                    width,
                    height: 1.0,
                    color: Rgba::new(0.55, 0.57, 0.60, 0.55),
                    object_id,
                    shape: Default::default(),
                }),
                QuantitativeMissingMode::Marker => display.rectangles.push(RectPrimitive {
                    x,
                    y: options.top,
                    width,
                    height: options.height,
                    color: Rgba::new(0.72, 0.72, 0.72, 0.18),
                    object_id,
                    shape: Default::default(),
                }),
            }
            previous = None;
            continue;
        };
        let y = quantitative_y(value, range, settings, options);
        let color = if value >= settings.baseline {
            positive
        } else {
            negative
        };
        match settings.display_mode {
            QuantitativeDisplayMode::Bar => {
                let top = y
                    .min(baseline_y)
                    .clamp(options.top, options.top + options.height);
                let bottom = y
                    .max(baseline_y)
                    .clamp(options.top, options.top + options.height);
                display.rectangles.push(RectPrimitive {
                    x,
                    y: top,
                    width,
                    height: (bottom - top).max(1.0),
                    color,
                    object_id,
                    shape: Default::default(),
                });
            }
            QuantitativeDisplayMode::Line => {
                display.rectangles.push(RectPrimitive {
                    x,
                    y: y - 0.75,
                    width,
                    height: 1.5,
                    color,
                    object_id,
                    shape: Default::default(),
                });
                if let Some((previous_end, previous_y)) = previous
                    && previous_end >= window.start
                {
                    display.rectangles.push(RectPrimitive {
                        x,
                        y: previous_y.min(y),
                        width: 1.5,
                        height: (previous_y - y).abs().max(1.5),
                        color,
                        object_id,
                        shape: Default::default(),
                    });
                }
            }
            QuantitativeDisplayMode::Point => display.rectangles.push(RectPrimitive {
                x: x + width * 0.5 - 2.0,
                y: y - 2.0,
                width: 4.0,
                height: 4.0,
                color,
                object_id,
                shape: Default::default(),
            }),
            QuantitativeDisplayMode::Heatmap => {
                let transformed_value = transform_quantitative(value, settings.scale_transform);
                let transformed_baseline =
                    transform_quantitative(settings.baseline, settings.scale_transform);
                let transformed_edge = if value >= settings.baseline {
                    transform_quantitative(range.maximum, settings.scale_transform)
                } else {
                    transform_quantitative(range.minimum, settings.scale_transform)
                };
                let intensity = ((transformed_value - transformed_baseline).abs()
                    / (transformed_edge - transformed_baseline)
                        .abs()
                        .max(f32::EPSILON))
                .clamp(0.08, 1.0);
                display.rectangles.push(RectPrimitive {
                    x,
                    y: options.top,
                    width,
                    height: options.height,
                    color: Rgba::new(color.red, color.green, color.blue, color.alpha * intensity),
                    object_id,
                    shape: Default::default(),
                });
            }
        }
        previous = Some((window.end, y));
        rendered = rendered.saturating_add(1);
    }
    if settings.show_range_labels {
        let label_x = (viewport.pixel_width() as f32 - 38.0).max(38.0);
        display.texts.push(TextPrimitive {
            x: label_x,
            y: options.top + 8.0,
            text: format_quantitative_value(range.maximum),
            font_size: 11.0,
            color: Rgba::new(0.16, 0.17, 0.19, 0.92),
        });
        display.texts.push(TextPrimitive {
            x: label_x,
            y: options.top + options.height - 8.0,
            text: format_quantitative_value(range.minimum),
            font_size: 11.0,
            color: Rgba::new(0.16, 0.17, 0.19, 0.92),
        });
    }
    let primitive_count = display.rectangles.len();
    (
        display,
        TrackLayoutStats {
            input_records: records.len(),
            rendered_records: rendered,
            hidden_records: records.len().saturating_sub(
                windows
                    .iter()
                    .filter(|window| window.value.is_some())
                    .count(),
            ),
            primitive_count,
            quantitative_minimum: Some(range.minimum),
            quantitative_maximum: Some(range.maximum),
            missing_windows: missing,
            non_finite_records,
            aggregated,
        },
    )
}

#[must_use]
pub fn quantitative_data_range(
    records: &[QuantitativeRecord],
    viewport: Viewport,
    settings: &QuantitativeTrackSettings,
) -> Option<QuantitativeRange> {
    range_for_windows(
        &quantitative_windows(records, viewport, settings.window_function).0,
        settings.baseline,
    )
}

#[must_use]
pub fn copy_number_track_height(sample_count: usize, row_height: f32) -> f32 {
    COPY_NUMBER_HEADER_HEIGHT + sample_count as f32 * row_height.clamp(6.0, 40.0)
}

#[must_use]
pub fn mutation_track_height(sample_count: usize, row_height: f32) -> f32 {
    MUTATION_HEADER_HEIGHT + sample_count as f32 * row_height.clamp(8.0, 40.0)
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn layout_mutations(
    query: &MutationQuery,
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &MutationTrackSettings,
    visible_top: f32,
    visible_bottom: f32,
) -> MutationLayout {
    let row_height = settings.row_height.clamp(8.0, 40.0);
    let rows_top = options.top + MUTATION_HEADER_HEIGHT;
    let first_row = (((visible_top - rows_top) / row_height).floor().max(0.0) as usize)
        .min(query.samples.len());
    let last_row = (((visible_bottom - rows_top) / row_height).ceil().max(0.0) as usize)
        .min(query.samples.len());
    let visible_rows = (first_row..last_row)
        .map(|sample_index| {
            let top = rows_top + sample_index as f32 * row_height;
            MutationVisibleRow {
                sample_index,
                top,
                bottom: top + row_height,
            }
        })
        .collect::<Vec<_>>();
    let mut multiplicity = BTreeMap::new();
    for record in &query.records {
        *multiplicity
            .entry((record.sample_index, record.start, record.end))
            .or_insert(0_usize) += 1;
    }
    let mut display = DisplayList::default();
    let mut rendered = 0_usize;
    let mut labels = 0_usize;
    for (record_index, record) in query.records.iter().enumerate() {
        if rendered == MAX_RENDERED_MUTATIONS {
            break;
        }
        if record.sample_index < first_row || record.sample_index >= last_row {
            continue;
        }
        let start = record.start.max(viewport.interval().start());
        let end = record.end.min(viewport.interval().end());
        if start >= end {
            continue;
        }
        let x = viewport.pixel_at_genomic(start) as f32;
        let right = viewport.pixel_at_genomic(end) as f32;
        let width = (right - x).max(5.0);
        let y = rows_top + record.sample_index as f32 * row_height + 2.0;
        let height = (row_height - 4.0).max(4.0);
        let color = mutation_color(record, settings.color_mode);
        let object_id = crate::offset_object_id(options.object_id_base, record_index);
        if options.selected_record == Some(record_index) {
            display.rectangles.push(RectPrimitive {
                x: x - 2.0,
                y: y - 2.0,
                width: width + 4.0,
                height: height + 4.0,
                color: Rgba::new(0.05, 0.12, 0.24, 0.25),
                object_id: 0,
                shape: Default::default(),
            });
        }
        display.rectangles.push(RectPrimitive {
            x,
            y,
            width,
            height,
            color,
            object_id,
            shape: Default::default(),
        });
        if width > 3.0 && height > 3.0 {
            display.rectangles.push(RectPrimitive {
                x: x + 1.0,
                y: y + 1.0,
                width: (width - 2.0).max(1.0),
                height: (height - 2.0).max(1.0),
                color: Rgba::new(1.0, 1.0, 1.0, 0.90),
                object_id,
                shape: Default::default(),
            });
        }
        if multiplicity
            .get(&(record.sample_index, record.start, record.end))
            .copied()
            .unwrap_or(1)
            > 1
        {
            display.rectangles.push(RectPrimitive {
                x,
                y: y + height * 0.5,
                width,
                height: 1.0,
                color,
                object_id,
                shape: Default::default(),
            });
        }
        if settings.show_gene_labels
            && viewport.bases_per_pixel() <= 10.0
            && !record.gene.is_empty()
            && labels < MAX_MUTATION_LABELS
        {
            display.texts.push(TextPrimitive {
                x: x + width + 3.0,
                y: y + height * 0.5,
                text: bounded_interaction_label(&record.gene),
                font_size: (row_height - 5.0).clamp(8.0, 12.0),
                color: Rgba::new(0.10, 0.10, 0.11, 0.96),
            });
            labels = labels.saturating_add(1);
        }
        rendered = rendered.saturating_add(1);
    }
    MutationLayout {
        stats: TrackLayoutStats {
            input_records: query.records.len(),
            rendered_records: rendered,
            hidden_records: query.records.len().saturating_sub(rendered),
            primitive_count: display.rectangles.len(),
            ..TrackLayoutStats::default()
        },
        display_list: display,
        visible_rows,
    }
}

fn mutation_color(record: &rgv_model::MutationRecord, mode: MutationColorMode) -> Rgba {
    let bytes = match mode {
        MutationColorMode::Uniform => [45, 48, 54, 255],
        MutationColorMode::VariantType => match record.variant_type.to_ascii_uppercase().as_str() {
            "SNP" => [55, 113, 180, 255],
            "DNP" | "TNP" | "ONP" => [112, 78, 170, 255],
            "INS" => [224, 132, 44, 255],
            "DEL" => [202, 65, 70, 255],
            _ => [102, 109, 120, 255],
        },
        MutationColorMode::Classification => {
            let classification = record.classification.to_ascii_lowercase();
            if classification.contains("nonsense") || classification.contains("frame_shift") {
                [202, 65, 70, 255]
            } else if classification.contains("missense") {
                [55, 113, 180, 255]
            } else if classification.contains("splice") {
                [125, 78, 168, 255]
            } else if classification.contains("in_frame") {
                [224, 132, 44, 255]
            } else if classification.contains("silent") {
                [72, 145, 91, 255]
            } else {
                [102, 109, 120, 255]
            }
        }
    };
    rgba_from_bytes(bytes)
}

#[must_use]
pub fn copy_number_sample_order(
    sample_count: usize,
    sort_values: &[Option<f32>],
    mode: CopyNumberSortMode,
) -> Vec<usize> {
    let mut order = (0..sample_count).collect::<Vec<_>>();
    if mode == CopyNumberSortMode::FileOrder {
        return order;
    }
    order.sort_by(|left_index, right_index| {
        match (
            sort_values.get(*left_index).copied().flatten(),
            sort_values.get(*right_index).copied().flatten(),
        ) {
            (Some(left_value), Some(right_value)) => {
                let ordering = match mode {
                    CopyNumberSortMode::Amplification => right_value.total_cmp(&left_value),
                    CopyNumberSortMode::Deletion => left_value.total_cmp(&right_value),
                    CopyNumberSortMode::FileOrder => unreachable!(),
                };
                ordering.then_with(|| left_index.cmp(right_index))
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left_index.cmp(right_index),
        }
    });
    order
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn layout_copy_number(
    query: &CopyNumberQuery,
    viewport: Viewport,
    options: TrackLayoutOptions,
    settings: &CopyNumberTrackSettings,
    quantitative: &QuantitativeTrackSettings,
    shared_range: Option<QuantitativeRange>,
    visible_top: f32,
    visible_bottom: f32,
) -> CopyNumberLayout {
    let value_semantics = match settings.value_mode {
        CopyNumberValueMode::Automatic => query.inferred_value_semantics,
        CopyNumberValueMode::LogRatio => CopyNumberValueSemantics::LogRatio,
        CopyNumberValueMode::Absolute => CopyNumberValueSemantics::Absolute,
    };
    let baseline = match value_semantics {
        CopyNumberValueSemantics::LogRatio => 0.0,
        CopyNumberValueSemantics::Absolute => 2.0,
    };
    let automatic_range = query
        .segments
        .iter()
        .map(|segment| segment.value)
        .filter(|value| value.is_finite())
        .fold(None, |range: Option<QuantitativeRange>, value| {
            Some(range.map_or(
                QuantitativeRange {
                    minimum: value.min(baseline),
                    maximum: value.max(baseline),
                },
                |range| QuantitativeRange {
                    minimum: range.minimum.min(value).min(baseline),
                    maximum: range.maximum.max(value).max(baseline),
                },
            ))
        })
        .map(expand_degenerate_range);
    let range = match quantitative.scale_mode {
        QuantitativeScaleMode::Automatic => automatic_range,
        QuantitativeScaleMode::Fixed => Some(QuantitativeRange {
            minimum: quantitative.fixed_minimum,
            maximum: quantitative.fixed_maximum,
        }),
        QuantitativeScaleMode::Shared => shared_range.or(automatic_range),
    }
    .filter(|range| {
        range.minimum.is_finite() && range.maximum.is_finite() && range.minimum < range.maximum
    });
    let sample_order = copy_number_sample_order(
        query.samples.len(),
        &query.sample_sort_values,
        settings.sort_mode,
    );
    let row_height = settings.row_height.clamp(6.0, 40.0);
    let rows_top = options.top + COPY_NUMBER_HEADER_HEIGHT;
    let first_row =
        (((visible_top - rows_top) / row_height).floor().max(0.0) as usize).min(sample_order.len());
    let last_row = (((visible_bottom - rows_top) / row_height).ceil().max(0.0) as usize)
        .min(sample_order.len());
    let mut visible_rows = Vec::with_capacity(last_row.saturating_sub(first_row));
    let mut sample_to_visible_row = vec![None; query.samples.len()];
    for (row_index, &sample_index) in sample_order
        .iter()
        .enumerate()
        .take(last_row)
        .skip(first_row)
    {
        let top = rows_top + row_index as f32 * row_height;
        visible_rows.push(CopyNumberVisibleRow {
            sample_index,
            top,
            bottom: top + row_height,
        });
        sample_to_visible_row[sample_index] = Some(row_index);
    }
    let mut display = DisplayList::default();
    let Some(range) = range else {
        return CopyNumberLayout {
            display_list: display,
            stats: TrackLayoutStats {
                input_records: query.segments.len(),
                hidden_records: query.segments.len(),
                ..TrackLayoutStats::default()
            },
            visible_rows,
            sample_order,
            range: None,
            baseline,
            value_semantics,
        };
    };
    let positive = with_opacity(
        rgba_from_bytes(quantitative.positive_color),
        quantitative.opacity,
    );
    let negative = with_opacity(
        rgba_from_bytes(quantitative.negative_color),
        quantitative.opacity,
    );
    let mut rendered = 0_usize;
    let mut labels = 0_usize;
    for (segment_index, segment) in query.segments.iter().enumerate() {
        let Some(row_index) = sample_to_visible_row
            .get(segment.sample_index)
            .copied()
            .flatten()
        else {
            continue;
        };
        if rendered == MAX_RENDERED_COPY_NUMBER_SEGMENTS {
            break;
        }
        let start = segment.start.max(viewport.interval().start());
        let end = segment.end.min(viewport.interval().end());
        if start >= end {
            continue;
        }
        let x = viewport.pixel_at_genomic(start) as f32;
        let right = viewport.pixel_at_genomic(end) as f32;
        let width = (right - x).max(1.0);
        let y = rows_top + row_index as f32 * row_height + 1.0;
        let color = if segment.value >= baseline {
            positive
        } else {
            negative
        };
        let edge = if segment.value >= baseline {
            range.maximum
        } else {
            range.minimum
        };
        let intensity = ((segment.value - baseline).abs()
            / (edge - baseline).abs().max(f32::EPSILON))
        .clamp(0.08, 1.0);
        let object_id = crate::offset_object_id(options.object_id_base, segment_index);
        display.rectangles.push(RectPrimitive {
            x,
            y,
            width,
            height: (row_height - 2.0).max(1.0),
            color: Rgba::new(color.red, color.green, color.blue, color.alpha * intensity),
            object_id,
            shape: Default::default(),
        });
        if options.selected_record == Some(segment_index) {
            display.rectangles.push(RectPrimitive {
                x,
                y,
                width,
                height: (row_height - 2.0).max(1.0),
                color: Rgba::new(0.05, 0.12, 0.24, 0.22),
                object_id: 0,
                shape: Default::default(),
            });
        }
        if settings.show_values
            && row_height >= 14.0
            && width >= 38.0
            && labels < MAX_COPY_NUMBER_VALUE_LABELS
        {
            display.texts.push(TextPrimitive {
                x: x + width * 0.5,
                y: y + (row_height - 2.0) * 0.5,
                text: format_quantitative_value(segment.value),
                font_size: (row_height - 4.0).clamp(8.0, 12.0),
                color: Rgba::new(0.08, 0.08, 0.08, 0.92),
            });
            labels = labels.saturating_add(1);
        }
        rendered = rendered.saturating_add(1);
    }
    if quantitative.show_range_labels {
        display.texts.push(TextPrimitive {
            x: (viewport.pixel_width() as f32 - 90.0).max(90.0),
            y: options.top + COPY_NUMBER_HEADER_HEIGHT * 0.5,
            text: format!(
                "{}  neutral {}  {}",
                format_quantitative_value(range.minimum),
                format_quantitative_value(baseline),
                format_quantitative_value(range.maximum)
            ),
            font_size: 10.0,
            color: Rgba::new(0.16, 0.17, 0.19, 0.92),
        });
    }
    CopyNumberLayout {
        stats: TrackLayoutStats {
            input_records: query.segments.len(),
            rendered_records: rendered,
            hidden_records: query.segments.len().saturating_sub(rendered),
            primitive_count: display.rectangles.len(),
            quantitative_minimum: Some(range.minimum),
            quantitative_maximum: Some(range.maximum),
            ..TrackLayoutStats::default()
        },
        display_list: display,
        visible_rows,
        sample_order,
        range: Some(range),
        baseline,
        value_semantics,
    }
}

fn expand_degenerate_range(range: QuantitativeRange) -> QuantitativeRange {
    if range.minimum < range.maximum {
        return range;
    }
    let padding = range.minimum.abs().max(1.0) * 0.05;
    QuantitativeRange {
        minimum: range.minimum - padding,
        maximum: range.maximum + padding,
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn compose_quantitative_records(
    left: &[QuantitativeRecord],
    right: &[QuantitativeRecord],
    operation: QuantitativeArithmetic,
    start: u64,
    end: u64,
    maximum_records: usize,
) -> QuantitativeComposition {
    let mut output = QuantitativeComposition {
        provenance: QuantitativeCompositionProvenance {
            left_records: left.len(),
            right_records: right.len(),
            ..QuantitativeCompositionProvenance::default()
        },
        ..QuantitativeComposition::default()
    };
    if operation == QuantitativeArithmetic::None || start >= end || maximum_records == 0 {
        return output;
    }
    let mut events = Vec::with_capacity(left.len().saturating_add(right.len()).saturating_mul(2));
    for (side, records) in [
        (QuantitativeSide::Left, left),
        (QuantitativeSide::Right, right),
    ] {
        for (index, record) in records.iter().enumerate().filter(|(_, record)| {
            record.value.is_finite() && record.start < end && record.end > start
        }) {
            let clipped_start = record.start.max(start);
            let clipped_end = record.end.min(end);
            if clipped_start >= clipped_end {
                continue;
            }
            let key = (record.end.saturating_sub(record.start), record.start, index);
            events.push(QuantitativeEvent {
                position: clipped_start,
                kind: QuantitativeEventKind::Start,
                side,
                key,
            });
            events.push(QuantitativeEvent {
                position: clipped_end,
                kind: QuantitativeEventKind::End,
                side,
                key,
            });
        }
    }
    events.sort_unstable();
    let mut active_left: BTreeSet<(u64, u64, usize)> = BTreeSet::new();
    let mut active_right: BTreeSet<(u64, u64, usize)> = BTreeSet::new();
    let mut previous = start;
    let mut event_index = 0_usize;
    while event_index < events.len() {
        let position = events[event_index].position;
        if previous < position {
            let left_value = active_left
                .first()
                .and_then(|(_, _, index)| left.get(*index))
                .map(|record| record.value);
            let right_value = active_right
                .first()
                .and_then(|(_, _, index)| right.get(*index))
                .map(|record| record.value);
            match left_value.zip(right_value) {
                Some((left_value, right_value)) => {
                    let value = match operation {
                        QuantitativeArithmetic::None => unreachable!(),
                        QuantitativeArithmetic::Add => Some(left_value + right_value),
                        QuantitativeArithmetic::Subtract => Some(left_value - right_value),
                        QuantitativeArithmetic::Multiply => Some(left_value * right_value),
                        QuantitativeArithmetic::Divide if right_value == 0.0 => {
                            output.provenance.divide_by_zero_segments =
                                output.provenance.divide_by_zero_segments.saturating_add(1);
                            None
                        }
                        QuantitativeArithmetic::Divide => Some(left_value / right_value),
                    };
                    if let Some(value) = value {
                        if !value.is_finite() {
                            output.provenance.non_finite_results =
                                output.provenance.non_finite_results.saturating_add(1);
                        } else if let Some(last) = output.records.last_mut()
                            && last.end == previous
                            && last.value.to_bits() == value.to_bits()
                        {
                            last.end = position;
                        } else if output.records.len() == maximum_records {
                            output.provenance.records_truncated = true;
                            break;
                        } else {
                            output.records.push(QuantitativeRecord {
                                start: previous,
                                end: position,
                                value,
                            });
                        }
                    }
                }
                None => {
                    output.provenance.missing_operand_segments =
                        output.provenance.missing_operand_segments.saturating_add(1);
                }
            }
        }
        while event_index < events.len() && events[event_index].position == position {
            let event = events[event_index];
            let active = match event.side {
                QuantitativeSide::Left => &mut active_left,
                QuantitativeSide::Right => &mut active_right,
            };
            match event.kind {
                QuantitativeEventKind::End => {
                    active.remove(&event.key);
                }
                QuantitativeEventKind::Start => {
                    active.insert(event.key);
                }
            }
            event_index = event_index.saturating_add(1);
        }
        previous = position;
    }
    output.provenance.output_records = output.records.len();
    output
}

#[derive(Clone, Copy, Debug)]
struct QuantitativeWindow {
    start: u64,
    end: u64,
    value: Option<f32>,
    identity: usize,
}

/// How one aggregation window reduces the records that overlap it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuantitativeReduction {
    Minimum,
    Mean,
    Maximum,
    /// Whichever of the minimum and maximum is further from zero: keeps the
    /// peaks and troughs a one-rectangle-per-record plot would have shown.
    Extreme,
}

/// One window per visible record, or `None` when that exceeds the raw budget.
fn raw_quantitative_windows(
    records: &[QuantitativeRecord],
    viewport: Viewport,
) -> Option<Vec<QuantitativeWindow>> {
    let interval = viewport.interval();
    let visible = || {
        records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.end > interval.start() && record.start < interval.end())
    };
    if visible().nth(MAX_RAW_QUANTITATIVE_RECORDS).is_some() {
        return None;
    }
    Some(
        visible()
            .map(|(identity, record)| QuantitativeWindow {
                start: record.start.max(interval.start()),
                end: record.end.min(interval.end()),
                value: record.value.is_finite().then_some(record.value),
                identity,
            })
            .collect(),
    )
}

/// Builds the plotted windows and reports whether they aggregate records.
///
/// Raw mode plots one window per record while that fits the render budget.
/// Beyond [`MAX_RAW_QUANTITATIVE_RECORDS`] visible records it used to plot
/// only the first records and leave the rest of the viewport blank; it now
/// falls back to extreme-preserving per-pixel windows across the whole
/// viewport (many records then share each pixel in any case).
fn quantitative_windows(
    records: &[QuantitativeRecord],
    viewport: Viewport,
    function: QuantitativeWindowFunction,
) -> (Vec<QuantitativeWindow>, bool) {
    let interval = viewport.interval();
    let reduction = match function {
        QuantitativeWindowFunction::Raw => {
            if let Some(windows) = raw_quantitative_windows(records, viewport) {
                return (windows, false);
            }
            QuantitativeReduction::Extreme
        }
        QuantitativeWindowFunction::Minimum => QuantitativeReduction::Minimum,
        QuantitativeWindowFunction::Mean => QuantitativeReduction::Mean,
        QuantitativeWindowFunction::Maximum => QuantitativeReduction::Maximum,
    };

    let bin_count = usize::try_from(interval.len())
        .unwrap_or(usize::MAX)
        .min(viewport.pixel_width().ceil().max(1.0) as usize)
        .clamp(1, MAX_QUANTITATIVE_WINDOWS);
    let span = interval.len();
    let span_128 = u128::from(span);
    let mut accumulators = vec![QuantitativeAccumulator::default(); bin_count];
    for record in records.iter().filter(|record| {
        record.value.is_finite() && record.end > interval.start() && record.start < interval.end()
    }) {
        let start = record.start.max(interval.start());
        let end = record.end.min(interval.end());
        if start >= end {
            continue;
        }
        let first =
            ((u128::from(start - interval.start()) * bin_count as u128) / span_128) as usize;
        let last =
            ((u128::from(end - 1 - interval.start()) * bin_count as u128) / span_128) as usize;
        let last = last.min(bin_count - 1);
        for (index, accumulator) in accumulators
            .iter_mut()
            .enumerate()
            .take(last + 1)
            .skip(first)
        {
            let bin_start = interval.start()
                + u64::try_from((span_128 * index as u128) / bin_count as u128).unwrap_or(span);
            let bin_end = interval.start()
                + u64::try_from((span_128 * (index + 1) as u128) / bin_count as u128)
                    .unwrap_or(span);
            let overlap = end.min(bin_end).saturating_sub(start.max(bin_start));
            if overlap == 0 {
                continue;
            }
            if accumulator.present {
                accumulator.minimum = accumulator.minimum.min(record.value);
                accumulator.maximum = accumulator.maximum.max(record.value);
            } else {
                accumulator.minimum = record.value;
                accumulator.maximum = record.value;
                accumulator.present = true;
            }
            accumulator.weighted_sum += f64::from(record.value) * overlap as f64;
            accumulator.weight = accumulator.weight.saturating_add(overlap);
        }
    }
    let windows = accumulators
        .into_iter()
        .enumerate()
        .map(|(identity, accumulator)| {
            let start = interval.start()
                + u64::try_from((span_128 * identity as u128) / bin_count as u128).unwrap_or(span);
            let end = interval.start()
                + u64::try_from((span_128 * (identity + 1) as u128) / bin_count as u128)
                    .unwrap_or(span);
            let value = accumulator.present.then(|| match reduction {
                QuantitativeReduction::Minimum => accumulator.minimum,
                QuantitativeReduction::Mean => {
                    (accumulator.weighted_sum / accumulator.weight.max(1) as f64) as f32
                }
                QuantitativeReduction::Maximum => accumulator.maximum,
                QuantitativeReduction::Extreme => {
                    if accumulator.minimum.abs() > accumulator.maximum.abs() {
                        accumulator.minimum
                    } else {
                        accumulator.maximum
                    }
                }
            });
            QuantitativeWindow {
                start,
                end,
                value,
                identity,
            }
        })
        .collect();
    (windows, true)
}

fn range_for_windows(windows: &[QuantitativeWindow], baseline: f32) -> Option<QuantitativeRange> {
    let mut minimum = baseline;
    let mut maximum = baseline;
    let mut present = false;
    for value in windows.iter().filter_map(|window| window.value) {
        minimum = minimum.min(value);
        maximum = maximum.max(value);
        present = true;
    }
    if !present {
        return None;
    }
    if maximum - minimum <= f32::EPSILON {
        let padding = maximum.abs().mul_add(0.05, 1.0e-6).max(1.0e-6);
        minimum -= padding;
        maximum += padding;
    }
    Some(QuantitativeRange { minimum, maximum })
}

fn quantitative_y(
    value: f32,
    range: QuantitativeRange,
    settings: &QuantitativeTrackSettings,
    options: TrackLayoutOptions,
) -> f32 {
    let minimum = transform_quantitative(range.minimum, settings.scale_transform);
    let maximum = transform_quantitative(range.maximum, settings.scale_transform);
    let value = transform_quantitative(
        value.clamp(range.minimum, range.maximum),
        settings.scale_transform,
    );
    options.top + (maximum - value) / (maximum - minimum).max(f32::EPSILON) * options.height
}

fn transform_quantitative(value: f32, transform: QuantitativeScaleTransform) -> f32 {
    match transform {
        QuantitativeScaleTransform::Linear => value,
        QuantitativeScaleTransform::SignedLog10 => value.signum() * (1.0 + value.abs()).log10(),
    }
}

fn rgba_from_bytes(color: [u8; 4]) -> Rgba {
    Rgba::new(
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
        f32::from(color[3]) / 255.0,
    )
}

fn with_opacity(color: Rgba, opacity: f32) -> Rgba {
    Rgba::new(
        color.red,
        color.green,
        color.blue,
        color.alpha * opacity.clamp(0.0, 1.0),
    )
}

fn format_quantitative_value(value: f32) -> String {
    let absolute = value.abs();
    if absolute >= 1_000_000.0 || (absolute > 0.0 && absolute < 0.001) {
        format!("{value:.2e}")
    } else if absolute >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn add_feature(
    display: &mut DisplayList,
    feature: &FeatureRecord,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    selected: bool,
    direction_marker_budget: &mut usize,
) {
    let base_color = match feature.strand {
        Strand::Forward => Rgba::new(0.25, 0.70, 0.94, 0.88),
        Strand::Reverse => Rgba::new(0.94, 0.38, 0.44, 0.88),
        Strand::Unstranded => Rgba::new(0.64, 0.58, 0.90, 0.86),
    };
    let (x, width) = clipped_geometry(feature.start, feature.end, viewport);
    if let Some(peak) = feature.peak {
        let score_opacity = feature.score.map_or(0.72, |score| {
            0.35 + score.clamp(0.0, 1000.0) / 1000.0 * 0.55
        });
        let color = with_alpha(base_color, score_opacity);
        display.rectangles.push(RectPrimitive {
            x,
            y: y + height * 0.18,
            width: width.max(1.0),
            height: (height * 0.64).max(2.0),
            color,
            object_id,
            shape: feature_terminal_shape(feature, feature.start, feature.end, viewport),
        });
        if let Some(summit) = peak.summit
            && summit >= viewport.interval().start()
            && summit < viewport.interval().end()
        {
            let summit_x = viewport.pixel_at_genomic(summit) as f32;
            display.rectangles.push(RectPrimitive {
                x: summit_x - 1.0,
                y,
                width: 2.0,
                height: height.max(2.0),
                color: with_alpha(base_color, 1.0),
                object_id,
                shape: Default::default(),
            });
        }
        if selected {
            add_feature_selection_outline(display, x, y, width, height, object_id);
        }
        return;
    }
    display.rectangles.push(RectPrimitive {
        x,
        y: y + height * 0.45,
        width: width.max(1.0),
        height: 2.0,
        color: with_alpha(base_color, 0.65),
        object_id,
        shape: Default::default(),
    });
    let default_block = [(feature.start, feature.end)];
    let blocks = if feature.blocks.is_empty() {
        default_block.as_slice()
    } else {
        &feature.blocks
    };
    let transcript_model = feature.feature_type.as_deref() == Some("transcript");
    let terminal_geometry = feature_uses_terminal_geometry(feature);
    let block_detail_visible = !transcript_model
        || blocks.iter().any(|&(start, end)| {
            let (_, width) = clipped_geometry(start, end, viewport);
            width >= 1.5
        });
    if !block_detail_visible {
        // At overview scale exon blocks would all expand from subpixel spans
        // into misleading one-pixel bars. The transcript connector above is
        // the stable gene landmark; one bounded direction glyph preserves
        // strand without emitting a primitive per exon.
        add_component_feature_direction_marker(
            display,
            feature,
            viewport,
            y + height * 0.5,
            base_color,
            object_id,
            direction_marker_budget,
        );
        if selected {
            add_feature_selection_outline(display, x, y, width, height, object_id);
        }
        return;
    }
    for &(start, end) in blocks {
        let (x, width) = clipped_geometry(start, end, viewport);
        if width <= 0.0 {
            continue;
        }
        let (block_y, block_height) = if transcript_model {
            (y + height * 0.3, (height * 0.4).max(2.0))
        } else {
            (y, height)
        };
        display.rectangles.push(RectPrimitive {
            x,
            y: block_y,
            width: width.max(1.0),
            height: block_height,
            color: base_color,
            object_id,
            shape: if terminal_geometry {
                feature_terminal_shape(feature, start, end, viewport)
            } else {
                RectShape::Rectangle
            },
        });
        if let Some((coding_start, coding_end)) = feature.coding_interval {
            let coding_start = start.max(coding_start);
            let coding_end = end.min(coding_end);
            if coding_start < coding_end {
                let (coding_x, coding_width) = clipped_geometry(coding_start, coding_end, viewport);
                if coding_width >= 1.5 {
                    display.rectangles.push(RectPrimitive {
                        x: coding_x,
                        y,
                        width: coding_width.max(1.0),
                        height,
                        color: base_color,
                        object_id,
                        shape: Default::default(),
                    });
                }
            }
        }
    }
    add_feature_connector_chevrons(
        display,
        feature,
        blocks,
        viewport,
        y + height * 0.45,
        base_color,
        object_id,
        direction_marker_budget,
    );
    if !terminal_geometry {
        add_component_feature_direction_marker(
            display,
            feature,
            viewport,
            y + height * 0.5,
            base_color,
            object_id,
            direction_marker_budget,
        );
    }
    if selected {
        add_feature_selection_outline(display, x, y, width, height, object_id);
    }
}

fn feature_uses_terminal_geometry(feature: &FeatureRecord) -> bool {
    !feature.blocks.is_empty()
        || feature.feature_type.is_none()
        || matches!(
            feature.feature_type.as_deref(),
            Some("gene" | "transcript" | "mRNA")
        )
}

#[allow(clippy::too_many_arguments)]
fn add_component_feature_direction_marker(
    display: &mut DisplayList,
    feature: &FeatureRecord,
    viewport: Viewport,
    centre_y: f32,
    color: Rgba,
    object_id: u32,
    budget: &mut usize,
) {
    let shape = match feature.strand {
        Strand::Forward => RectShape::ChevronRight,
        Strand::Reverse => RectShape::ChevronLeft,
        Strand::Unstranded => return,
    };
    const MARKER_SIZE: f32 = 5.0;
    let (x, width) = clipped_geometry(feature.start, feature.end, viewport);
    if width < MARKER_SIZE || *budget == 0 {
        return;
    }
    display.rectangles.push(RectPrimitive {
        x: x + width * 0.5 - MARKER_SIZE * 0.5,
        y: centre_y - MARKER_SIZE * 0.5,
        width: MARKER_SIZE,
        height: MARKER_SIZE,
        color,
        object_id,
        shape,
    });
    *budget = budget.saturating_sub(1);
}

fn feature_terminal_shape(
    feature: &FeatureRecord,
    block_start: u64,
    block_end: u64,
    viewport: Viewport,
) -> RectShape {
    match feature.strand {
        Strand::Forward
            if block_end == feature.end
                && feature.end > viewport.interval().start()
                && feature.end <= viewport.interval().end() =>
        {
            RectShape::DirectionalTerminalRight
        }
        Strand::Reverse
            if block_start == feature.start
                && feature.start >= viewport.interval().start()
                && feature.start < viewport.interval().end() =>
        {
            RectShape::DirectionalTerminalLeft
        }
        Strand::Forward | Strand::Reverse | Strand::Unstranded => RectShape::Rectangle,
    }
}

#[allow(clippy::too_many_arguments)]
fn add_feature_connector_chevrons(
    display: &mut DisplayList,
    feature: &FeatureRecord,
    blocks: &[(u64, u64)],
    viewport: Viewport,
    centre_y: f32,
    color: Rgba,
    object_id: u32,
    budget: &mut usize,
) {
    let shape = match feature.strand {
        Strand::Forward => RectShape::ChevronRight,
        Strand::Reverse => RectShape::ChevronLeft,
        Strand::Unstranded => return,
    };
    if blocks.len() < 2 || *budget == 0 {
        return;
    }
    const SPACING: f32 = 18.0;
    const MARKER_WIDTH: f32 = 5.0;
    const MARKER_HEIGHT: f32 = 5.0;
    for pair in blocks.windows(2) {
        let gap_start = pair[0].1;
        let gap_end = pair[1].0;
        if gap_end <= gap_start {
            continue;
        }
        let (x, width) = clipped_geometry(gap_start, gap_end, viewport);
        if width < MARKER_WIDTH || *budget == 0 {
            continue;
        }
        let count = ((width / SPACING).floor() as usize).max(1).min(*budget);
        let step = width / (count + 1) as f32;
        for marker in 1..=count {
            let centre_x = x + step * marker as f32;
            display.rectangles.push(RectPrimitive {
                x: centre_x - MARKER_WIDTH * 0.5,
                y: centre_y - MARKER_HEIGHT * 0.5,
                width: MARKER_WIDTH,
                height: MARKER_HEIGHT,
                color,
                object_id,
                shape,
            });
        }
        *budget = budget.saturating_sub(count);
    }
}

fn add_feature_selection_outline(
    display: &mut DisplayList,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    object_id: u32,
) {
    let color = Rgba::new(0.10, 0.10, 0.12, 1.0);
    let width = width.max(1.0);
    for rectangle in [
        RectPrimitive {
            x,
            y,
            width,
            height: 1.5,
            color,
            object_id,
            shape: Default::default(),
        },
        RectPrimitive {
            x,
            y: y + (height - 1.5).max(0.0),
            width,
            height: 1.5,
            color,
            object_id,
            shape: Default::default(),
        },
        RectPrimitive {
            x,
            y,
            width: 1.5,
            height,
            color,
            object_id,
            shape: Default::default(),
        },
        RectPrimitive {
            x: x + (width - 1.5).max(0.0),
            y,
            width: 1.5,
            height,
            color,
            object_id,
            shape: Default::default(),
        },
    ] {
        display.rectangles.push(rectangle);
    }
}

fn clipped_geometry(start: u64, end: u64, viewport: Viewport) -> (f32, f32) {
    let start = start.max(viewport.interval().start());
    let end = end.min(viewport.interval().end());
    if start >= end {
        return (0.0, 0.0);
    }
    let x = viewport.pixel_at_genomic(start) as f32;
    let right = viewport.pixel_at_genomic(end) as f32;
    (x, right - x)
}

fn variant_color(variant_type: &rgv_model::VariantType) -> Rgba {
    match variant_type {
        rgv_model::VariantType::Snv => Rgba::new(0.20, 0.67, 0.42, 0.96),
        rgv_model::VariantType::Mnv => Rgba::new(0.15, 0.62, 0.68, 0.96),
        rgv_model::VariantType::Insertion | rgv_model::VariantType::StructuralInsertion => {
            Rgba::new(0.62, 0.34, 0.88, 0.96)
        }
        rgv_model::VariantType::Deletion | rgv_model::VariantType::StructuralDeletion => {
            Rgba::new(0.90, 0.25, 0.22, 0.96)
        }
        rgv_model::VariantType::Inversion => Rgba::new(0.22, 0.46, 0.84, 0.96),
        rgv_model::VariantType::Duplication => Rgba::new(0.92, 0.52, 0.16, 0.96),
        rgv_model::VariantType::CopyNumber => Rgba::new(0.78, 0.62, 0.12, 0.96),
        rgv_model::VariantType::Breakend => Rgba::new(0.84, 0.20, 0.63, 0.96),
        rgv_model::VariantType::SpanningDeletion => Rgba::new(0.40, 0.42, 0.46, 0.96),
        rgv_model::VariantType::Complex | rgv_model::VariantType::Symbolic(_) => {
            Rgba::new(0.48, 0.38, 0.72, 0.96)
        }
    }
}

fn genotype_color(variant: &VariantRecord) -> Rgba {
    let mut saw_called = false;
    let mut saw_reference = false;
    let mut saw_heterozygous = false;
    let mut saw_homozygous_alternate = false;
    let mut saw_other_non_reference = false;
    for genotype in &variant.genotypes {
        let indices = genotype
            .alleles
            .iter()
            .map(|allele| allele.index)
            .collect::<Option<Vec<_>>>();
        let Some(indices) = indices.filter(|indices| !indices.is_empty()) else {
            continue;
        };
        saw_called = true;
        let first = indices[0];
        if indices.iter().all(|index| *index == 0) {
            saw_reference = true;
        } else if indices.iter().any(|index| *index != first) {
            saw_heterozygous = true;
        } else if first > 0 {
            saw_homozygous_alternate = true;
        } else {
            saw_other_non_reference = true;
        }
    }
    if saw_homozygous_alternate {
        Rgba::new(0.83, 0.20, 0.22, 0.96)
    } else if saw_heterozygous {
        Rgba::new(0.94, 0.54, 0.12, 0.96)
    } else if saw_other_non_reference {
        Rgba::new(0.62, 0.30, 0.78, 0.96)
    } else if saw_reference {
        Rgba::new(0.22, 0.48, 0.82, 0.96)
    } else if saw_called {
        Rgba::new(0.18, 0.62, 0.60, 0.96)
    } else {
        Rgba::new(0.48, 0.50, 0.54, 0.90)
    }
}

const fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba::new(color.red, color.green, color.blue, alpha)
}

#[cfg(test)]
#[path = "track_layout_tests.rs"]
mod tests;
