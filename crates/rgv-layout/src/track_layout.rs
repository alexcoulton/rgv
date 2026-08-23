use std::collections::{BTreeMap, BTreeSet};

use rgv_core::{
    CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode, FeatureDisplayMode,
    GwasColorMode, GwasTrackSettings, InteractionDisplayMode, InteractionTrackSettings,
    MutationColorMode, MutationTrackSettings, QuantitativeArithmetic, QuantitativeDisplayMode,
    QuantitativeMissingMode, QuantitativeScaleMode, QuantitativeScaleTransform,
    QuantitativeTrackSettings, QuantitativeWindowFunction, VariantColorMode, VariantTrackSettings,
    Viewport,
};
use rgv_hts::{
    CopyNumberQuery, CopyNumberValueSemantics, FeatureRecord, GwasRecord, InteractionRecord,
    MutationQuery, QuantitativeRecord, Strand, VariantRecord,
};

use crate::{DisplayList, RectPrimitive, Rgba, TextPrimitive};

pub const MAX_QUANTITATIVE_WINDOWS: usize = 4_096;
pub const MAX_RAW_QUANTITATIVE_RECORDS: usize = 16_384;
pub const MAX_RENDERED_INTERACTIONS: usize = 2_048;
pub const MAX_INTERACTION_LABELS: usize = 512;
pub const MAX_RENDERED_COPY_NUMBER_SEGMENTS: usize = 32_768;
pub const MAX_COPY_NUMBER_VALUE_LABELS: usize = 2_048;
pub const COPY_NUMBER_HEADER_HEIGHT: f32 = 24.0;
pub const MUTATION_HEADER_HEIGHT: f32 = 24.0;
pub const MAX_RENDERED_MUTATIONS: usize = 32_768;
pub const MAX_MUTATION_LABELS: usize = 2_048;
pub const MAX_RENDERED_GWAS_POINTS: usize = 250_000;
const INTERACTION_ARC_SEGMENTS: usize = 24;

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
    let mut display = DisplayList::default();
    let row_count = match options.feature_display_mode {
        FeatureDisplayMode::Collapsed => 1,
        FeatureDisplayMode::Squished => options.maximum_rows.saturating_mul(2).max(1),
        FeatureDisplayMode::Expanded => options.maximum_rows.max(1),
    };
    let mut row_ends = vec![0_u64; row_count];
    let row_height = options.height / row_ends.len() as f32;
    let mut rendered = 0;
    let mut hidden = 0;
    for (index, feature) in features.iter().enumerate() {
        if feature.end <= viewport.interval().start() || feature.start >= viewport.interval().end()
        {
            continue;
        }
        let row = if options.feature_display_mode == FeatureDisplayMode::Collapsed {
            0
        } else {
            let Some(row) = row_ends.iter().position(|end| *end <= feature.start) else {
                hidden += 1;
                continue;
            };
            row
        };
        row_ends[row] = row_ends[row].max(feature.end);
        let y = options.top + row as f32 * row_height + 2.0;
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
        add_feature(
            &mut display,
            feature,
            viewport,
            y,
            row_height - 4.0,
            object_id,
            options.selected_record == Some(index),
        );
        rendered += 1;
    }
    let primitive_count = display.rectangles.len();
    (
        display,
        TrackLayoutStats {
            input_records: features.len(),
            rendered_records: rendered,
            hidden_records: hidden,
            primitive_count,
            ..TrackLayoutStats::default()
        },
    )
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
        let x = viewport.pixel_at_genomic(variant.start) as f32;
        let right = viewport.pixel_at_genomic(variant.end) as f32;
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
        let color = match settings.color_mode {
            VariantColorMode::Type => variant_color(
                variant
                    .variant_types
                    .first()
                    .unwrap_or(&rgv_hts::VariantType::Complex),
            ),
            VariantColorMode::Genotype => genotype_color(variant),
        };
        if options.selected_record == Some(index) {
            display.rectangles.push(RectPrimitive {
                x: x - 6.0,
                y: options.top,
                width: 12.0,
                height: 12.0,
                color: Rgba::new(0.10, 0.28, 0.62, 0.24),
                object_id,
            });
        }
        display.rectangles.push(RectPrimitive {
            x: x - 1.0,
            y: options.top + 5.0,
            width: 2.0,
            height: (options.height - 12.0).max(2.0),
            color: with_alpha(color, 0.72),
            object_id,
        });
        display.rectangles.push(RectPrimitive {
            x: x - 4.0,
            y: options.top + 1.0,
            width: 8.0,
            height: 8.0,
            color,
            object_id,
        });
        display.rectangles.push(RectPrimitive {
            x,
            y: baseline,
            width: (right - x).max(2.0),
            height: 3.0,
            color,
            object_id,
        });
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
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
        if options.selected_record == Some(index) {
            let halo = point_size + 8.0;
            display.rectangles.push(RectPrimitive {
                x: x - halo * 0.5,
                y: y - halo * 0.5,
                width: halo,
                height: halo,
                color: Rgba::new(0.10, 0.28, 0.62, 0.20),
                object_id,
            });
        }
        display.rectangles.push(RectPrimitive {
            x: x - point_size * 0.5,
            y: y - point_size * 0.5,
            width: point_size,
            height: point_size,
            color: gwas_point_color(&record.contig, settings),
            object_id,
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
    let eligible = interactions
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            settings
                .minimum_score
                .is_none_or(|minimum| record.score.is_some_and(|score| score >= minimum))
        })
        .take(MAX_RENDERED_INTERACTIONS)
        .collect::<Vec<_>>();
    let automatic_maximum = eligible
        .iter()
        .map(|(_, record)| interaction_magnitude(record))
        .fold(0.0_f32, f32::max)
        .max(1.0);
    let scale_maximum = settings
        .fixed_maximum_score
        .unwrap_or(automatic_maximum)
        .max(f32::EPSILON);
    let baseline = options.top + options.height - 5.0;
    let mut rendered = 0_usize;
    let mut labels = 0_usize;
    for (index, record) in eligible {
        let source_here = record.source.contig == contig;
        let target_here = record.target.contig == contig;
        if !source_here && !target_here {
            continue;
        }
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
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
            let left_x = viewport.pixel_at_genomic(left) as f32;
            let right_x = viewport.pixel_at_genomic(right) as f32;
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
                        });
                    }
                }
            }
            for endpoint in [&record.source, &record.target] {
                add_interaction_endpoint(
                    &mut display,
                    endpoint.start,
                    endpoint.end,
                    viewport,
                    baseline,
                    color,
                    object_id,
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
            });
            add_interaction_endpoint(
                &mut display,
                local.start,
                local.end,
                viewport,
                baseline,
                color,
                object_id,
            );
            if settings.show_labels && labels < MAX_INTERACTION_LABELS {
                let partner_name = partner.name.as_deref().unwrap_or("partner");
                display.texts.push(TextPrimitive {
                    x: bounded_label_x(x, viewport.pixel_width() as f32, 54.0),
                    y: options.top + 7.0,
                    text: bounded_interaction_label(&format!(
                        "{} → {}:{} ({partner_name})",
                        local.name.as_deref().unwrap_or("endpoint"),
                        partner.contig,
                        partner.start.saturating_add(1),
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

fn add_interaction_arc(
    display: &mut DisplayList,
    left: f32,
    right: f32,
    baseline: f32,
    height: f32,
    color: Rgba,
    object_id: u32,
) {
    let width = (right - left).max(1.0);
    let mut previous = (left, baseline);
    for segment in 1..=INTERACTION_ARC_SEGMENTS {
        let fraction = segment as f32 / INTERACTION_ARC_SEGMENTS as f32;
        let point = (
            left + width * fraction,
            baseline - height * 4.0 * fraction * (1.0 - fraction),
        );
        display.rectangles.push(RectPrimitive {
            x: previous.0.min(point.0),
            y: previous.1.min(point.1),
            width: (point.0 - previous.0).abs().max(1.0),
            height: (point.1 - previous.1).abs().max(1.5),
            color,
            object_id,
        });
        previous = point;
    }
}

fn add_interaction_endpoint(
    display: &mut DisplayList,
    start: u64,
    end: u64,
    viewport: Viewport,
    baseline: f32,
    color: Rgba,
    object_id: u32,
) {
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
    });
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
    let windows = quantitative_windows(records, viewport, settings.window_function);
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
                aggregated: settings.window_function != QuantitativeWindowFunction::Raw,
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
        });
    }
    let mut rendered = 0_usize;
    let mut missing = 0_usize;
    let mut previous: Option<(u64, f32)> = None;
    for window in &windows {
        let x = viewport.pixel_at_genomic(window.start) as f32;
        let right = viewport.pixel_at_genomic(window.end) as f32;
        let width = (right - x).max(1.0);
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(window.identity).unwrap_or(u32::MAX));
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
                }),
                QuantitativeMissingMode::Marker => display.rectangles.push(RectPrimitive {
                    x,
                    y: options.top,
                    width,
                    height: options.height,
                    color: Rgba::new(0.72, 0.72, 0.72, 0.18),
                    object_id,
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
            aggregated: settings.window_function != QuantitativeWindowFunction::Raw,
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
        &quantitative_windows(records, viewport, settings.window_function),
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
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(record_index).unwrap_or(u32::MAX));
        if options.selected_record == Some(record_index) {
            display.rectangles.push(RectPrimitive {
                x: x - 2.0,
                y: y - 2.0,
                width: width + 4.0,
                height: height + 4.0,
                color: Rgba::new(0.05, 0.12, 0.24, 0.25),
                object_id: 0,
            });
        }
        display.rectangles.push(RectPrimitive {
            x,
            y,
            width,
            height,
            color,
            object_id,
        });
        if width > 3.0 && height > 3.0 {
            display.rectangles.push(RectPrimitive {
                x: x + 1.0,
                y: y + 1.0,
                width: (width - 2.0).max(1.0),
                height: (height - 2.0).max(1.0),
                color: Rgba::new(1.0, 1.0, 1.0, 0.90),
                object_id,
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

fn mutation_color(record: &rgv_hts::MutationRecord, mode: MutationColorMode) -> Rgba {
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
        let object_id = options
            .object_id_base
            .saturating_add(u32::try_from(segment_index).unwrap_or(u32::MAX));
        display.rectangles.push(RectPrimitive {
            x,
            y,
            width,
            height: (row_height - 2.0).max(1.0),
            color: Rgba::new(color.red, color.green, color.blue, color.alpha * intensity),
            object_id,
        });
        if options.selected_record == Some(segment_index) {
            display.rectangles.push(RectPrimitive {
                x,
                y,
                width,
                height: (row_height - 2.0).max(1.0),
                color: Rgba::new(0.05, 0.12, 0.24, 0.22),
                object_id: 0,
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

fn quantitative_windows(
    records: &[QuantitativeRecord],
    viewport: Viewport,
    function: QuantitativeWindowFunction,
) -> Vec<QuantitativeWindow> {
    let interval = viewport.interval();
    if function == QuantitativeWindowFunction::Raw {
        return records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.end > interval.start() && record.start < interval.end())
            .take(MAX_RAW_QUANTITATIVE_RECORDS)
            .map(|(identity, record)| QuantitativeWindow {
                start: record.start.max(interval.start()),
                end: record.end.min(interval.end()),
                value: record.value.is_finite().then_some(record.value),
                identity,
            })
            .collect();
    }

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
    accumulators
        .into_iter()
        .enumerate()
        .map(|(identity, accumulator)| {
            let start = interval.start()
                + u64::try_from((span_128 * identity as u128) / bin_count as u128).unwrap_or(span);
            let end = interval.start()
                + u64::try_from((span_128 * (identity + 1) as u128) / bin_count as u128)
                    .unwrap_or(span);
            let value = accumulator.present.then(|| match function {
                QuantitativeWindowFunction::Minimum => accumulator.minimum,
                QuantitativeWindowFunction::Mean => {
                    (accumulator.weighted_sum / accumulator.weight.max(1) as f64) as f32
                }
                QuantitativeWindowFunction::Maximum => accumulator.maximum,
                QuantitativeWindowFunction::Raw => unreachable!(),
            });
            QuantitativeWindow {
                start,
                end,
                value,
                identity,
            }
        })
        .collect()
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
    });
    let default_block = [(feature.start, feature.end)];
    let blocks = if feature.blocks.is_empty() {
        default_block.as_slice()
    } else {
        &feature.blocks
    };
    let transcript_model = feature.feature_type.as_deref() == Some("transcript");
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
        });
        if let Some((coding_start, coding_end)) = feature.coding_interval {
            let coding_start = start.max(coding_start);
            let coding_end = end.min(coding_end);
            if coding_start < coding_end {
                let (coding_x, coding_width) = clipped_geometry(coding_start, coding_end, viewport);
                if coding_width > 0.0 {
                    display.rectangles.push(RectPrimitive {
                        x: coding_x,
                        y,
                        width: coding_width.max(1.0),
                        height,
                        color: base_color,
                        object_id,
                    });
                }
            }
        }
    }
    if selected {
        add_feature_selection_outline(display, x, y, width, height, object_id);
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
        },
        RectPrimitive {
            x,
            y: y + (height - 1.5).max(0.0),
            width,
            height: 1.5,
            color,
            object_id,
        },
        RectPrimitive {
            x,
            y,
            width: 1.5,
            height,
            color,
            object_id,
        },
        RectPrimitive {
            x: x + (width - 1.5).max(0.0),
            y,
            width: 1.5,
            height,
            color,
            object_id,
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

fn variant_color(variant_type: &rgv_hts::VariantType) -> Rgba {
    match variant_type {
        rgv_hts::VariantType::Snv => Rgba::new(0.20, 0.67, 0.42, 0.96),
        rgv_hts::VariantType::Mnv => Rgba::new(0.15, 0.62, 0.68, 0.96),
        rgv_hts::VariantType::Insertion | rgv_hts::VariantType::StructuralInsertion => {
            Rgba::new(0.62, 0.34, 0.88, 0.96)
        }
        rgv_hts::VariantType::Deletion | rgv_hts::VariantType::StructuralDeletion => {
            Rgba::new(0.90, 0.25, 0.22, 0.96)
        }
        rgv_hts::VariantType::Inversion => Rgba::new(0.22, 0.46, 0.84, 0.96),
        rgv_hts::VariantType::Duplication => Rgba::new(0.92, 0.52, 0.16, 0.96),
        rgv_hts::VariantType::CopyNumber => Rgba::new(0.78, 0.62, 0.12, 0.96),
        rgv_hts::VariantType::Breakend => Rgba::new(0.84, 0.20, 0.63, 0.96),
        rgv_hts::VariantType::SpanningDeletion => Rgba::new(0.40, 0.42, 0.46, 0.96),
        rgv_hts::VariantType::Complex | rgv_hts::VariantType::Symbolic(_) => {
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
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;

    fn viewport() -> Viewport {
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
            1_000.0,
        )
        .unwrap()
    }

    #[test]
    fn bed_blocks_are_rendered_as_connected_exons() {
        let feature = FeatureRecord {
            contig: "chr1".to_owned(),
            start: 110,
            end: 190,
            name: Some("tx".to_owned()),
            feature_type: None,
            score: None,
            strand: Strand::Forward,
            blocks: vec![(110, 130), (170, 190)],
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        };
        let (display, stats) =
            layout_features(&[feature], viewport(), TrackLayoutOptions::default());
        assert_eq!(stats.rendered_records, 1);
        assert_eq!(display.rectangles.len(), 3);
    }

    #[test]
    fn gene_models_render_utrs_thinner_than_coding_exons() {
        let feature = FeatureRecord {
            contig: "chr1".to_owned(),
            start: 110,
            end: 190,
            name: Some("GENE1".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: Strand::Forward,
            blocks: vec![(110, 140), (160, 190)],
            coding_interval: Some((125, 175)),
            peak: None,
            attributes: Vec::new(),
        };
        let (display, _) = layout_features(&[feature], viewport(), TrackLayoutOptions::default());
        assert_eq!(display.rectangles.len(), 5);
        let exon_height = display.rectangles[1].height;
        let coding_height = display.rectangles[2].height;
        assert!(coding_height > exon_height);
    }

    #[test]
    fn narrow_peaks_render_the_called_summit() {
        let feature = FeatureRecord {
            contig: "chr1".to_owned(),
            start: 110,
            end: 190,
            name: Some("peak".to_owned()),
            feature_type: Some("narrowPeak".to_owned()),
            score: Some(500.0),
            strand: Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: Some(rgv_hts::PeakMetrics {
                signal_value: 12.0,
                p_value: Some(5.0),
                q_value: None,
                summit: Some(150),
            }),
            attributes: Vec::new(),
        };
        let (display, _) = layout_features(&[feature], viewport(), TrackLayoutOptions::default());
        assert_eq!(display.rectangles.len(), 2);
        assert!((display.rectangles[1].width - 2.0).abs() < f32::EPSILON);
        assert!((display.rectangles[1].x - 499.0).abs() < f32::EPSILON);
    }

    #[test]
    fn feature_display_modes_are_collapsed_squished_and_expanded() {
        let features = (0..3)
            .map(|index| FeatureRecord {
                contig: "chr1".to_owned(),
                start: 110 + index,
                end: 180 + index,
                name: Some(format!("feature-{index}")),
                feature_type: None,
                score: None,
                strand: Strand::Unstranded,
                blocks: Vec::new(),
                coding_interval: None,
                peak: None,
                attributes: Vec::new(),
            })
            .collect::<Vec<_>>();
        let expanded = layout_features(
            &features,
            viewport(),
            TrackLayoutOptions {
                maximum_rows: 1,
                feature_display_mode: FeatureDisplayMode::Expanded,
                ..TrackLayoutOptions::default()
            },
        );
        assert_eq!(expanded.1.rendered_records, 1);
        assert_eq!(expanded.1.hidden_records, 2);

        let squished = layout_features(
            &features,
            viewport(),
            TrackLayoutOptions {
                maximum_rows: 1,
                feature_display_mode: FeatureDisplayMode::Squished,
                ..TrackLayoutOptions::default()
            },
        );
        assert_eq!(squished.1.rendered_records, 2);
        assert_eq!(squished.1.hidden_records, 1);

        let collapsed = layout_features(
            &features,
            viewport(),
            TrackLayoutOptions {
                maximum_rows: 1,
                feature_display_mode: FeatureDisplayMode::Collapsed,
                ..TrackLayoutOptions::default()
            },
        );
        assert_eq!(collapsed.1.rendered_records, 3);
        assert_eq!(collapsed.1.hidden_records, 0);
        let block_rows = collapsed
            .0
            .rectangles
            .iter()
            .skip(1)
            .step_by(2)
            .map(|rectangle| rectangle.y)
            .collect::<Vec<_>>();
        assert_eq!(block_rows.len(), 3);
        assert!(
            block_rows
                .windows(2)
                .all(|pair| (pair[0] - pair[1]).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn quantitative_layout_supports_signed_values() {
        let records = [
            QuantitativeRecord {
                start: 100,
                end: 150,
                value: 2.0,
            },
            QuantitativeRecord {
                start: 150,
                end: 200,
                value: -1.0,
            },
        ];
        let settings = QuantitativeTrackSettings {
            window_function: QuantitativeWindowFunction::Raw,
            show_range_labels: false,
            ..QuantitativeTrackSettings::default()
        };
        let (display, stats) = layout_quantitative(
            &records,
            viewport(),
            TrackLayoutOptions::default(),
            &settings,
            None,
        );
        assert_eq!(stats.rendered_records, 2);
        assert_eq!(stats.quantitative_minimum, Some(-1.0));
        assert_eq!(stats.quantitative_maximum, Some(2.0));
        assert_eq!(display.rectangles.len(), 3); // baseline and two bars
        assert!(display.rectangles[1].y < display.rectangles[2].y);
    }

    #[test]
    fn quantitative_mean_is_weighted_by_genomic_overlap() {
        let viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 100, 104).unwrap(),
            1.0,
        )
        .unwrap();
        let records = [
            QuantitativeRecord {
                start: 100,
                end: 101,
                value: 2.0,
            },
            QuantitativeRecord {
                start: 101,
                end: 104,
                value: 4.0,
            },
        ];
        let settings = QuantitativeTrackSettings::default();
        assert_eq!(
            quantitative_data_range(&records, viewport, &settings),
            Some(QuantitativeRange {
                minimum: 0.0,
                maximum: 3.5,
            })
        );
    }

    #[test]
    fn quantitative_missing_windows_are_explicit_and_bounded() {
        let records = [QuantitativeRecord {
            start: 100,
            end: 150,
            value: 2.0,
        }];
        let settings = QuantitativeTrackSettings {
            missing_mode: QuantitativeMissingMode::Marker,
            show_range_labels: false,
            ..QuantitativeTrackSettings::default()
        };
        let (display, stats) = layout_quantitative(
            &records,
            viewport(),
            TrackLayoutOptions::default(),
            &settings,
            None,
        );
        assert_eq!(stats.rendered_records, 50);
        assert_eq!(stats.missing_windows, 50);
        assert!(stats.aggregated);
        assert_eq!(display.rectangles.len(), 101); // baseline, values, and missing markers
    }

    #[test]
    fn quantitative_fixed_and_shared_ranges_override_automatic_range() {
        let records = [QuantitativeRecord {
            start: 100,
            end: 200,
            value: 2.0,
        }];
        let fixed = QuantitativeTrackSettings {
            scale_mode: QuantitativeScaleMode::Fixed,
            fixed_minimum: -10.0,
            fixed_maximum: 20.0,
            ..QuantitativeTrackSettings::default()
        };
        let (_, fixed_stats) = layout_quantitative(
            &records,
            viewport(),
            TrackLayoutOptions::default(),
            &fixed,
            None,
        );
        assert_eq!(fixed_stats.quantitative_minimum, Some(-10.0));
        assert_eq!(fixed_stats.quantitative_maximum, Some(20.0));

        let shared = QuantitativeTrackSettings {
            scale_mode: QuantitativeScaleMode::Shared,
            ..QuantitativeTrackSettings::default()
        };
        let (_, shared_stats) = layout_quantitative(
            &records,
            viewport(),
            TrackLayoutOptions::default(),
            &shared,
            Some(QuantitativeRange {
                minimum: -5.0,
                maximum: 8.0,
            }),
        );
        assert_eq!(shared_stats.quantitative_minimum, Some(-5.0));
        assert_eq!(shared_stats.quantitative_maximum, Some(8.0));
    }

    #[test]
    fn raw_quantitative_layout_has_a_deterministic_record_cap() {
        let records = (0..MAX_RAW_QUANTITATIVE_RECORDS + 10)
            .map(|index| QuantitativeRecord {
                start: 100 + u64::try_from(index % 100).unwrap(),
                end: 101 + u64::try_from(index % 100).unwrap(),
                value: 1.0,
            })
            .collect::<Vec<_>>();
        let settings = QuantitativeTrackSettings {
            window_function: QuantitativeWindowFunction::Raw,
            show_range_labels: false,
            ..QuantitativeTrackSettings::default()
        };
        let (_, stats) = layout_quantitative(
            &records,
            viewport(),
            TrackLayoutOptions::default(),
            &settings,
            None,
        );
        assert_eq!(stats.rendered_records, MAX_RAW_QUANTITATIVE_RECORDS);
        assert_eq!(stats.hidden_records, 10);
    }

    #[test]
    fn quantitative_arithmetic_segments_exact_overlaps_and_reports_missing_values() {
        let left = [QuantitativeRecord {
            start: 0,
            end: 10,
            value: 4.0,
        }];
        let right = [QuantitativeRecord {
            start: 2,
            end: 8,
            value: 2.0,
        }];
        let composition = compose_quantitative_records(
            &left,
            &right,
            QuantitativeArithmetic::Subtract,
            0,
            10,
            100,
        );
        assert_eq!(
            composition.records,
            vec![QuantitativeRecord {
                start: 2,
                end: 8,
                value: 2.0,
            }]
        );
        assert_eq!(composition.provenance.missing_operand_segments, 2);
        assert!(!composition.provenance.records_truncated);
    }

    #[test]
    fn quantitative_arithmetic_uses_narrowest_overlap_and_marks_division_by_zero() {
        let left = [
            QuantitativeRecord {
                start: 0,
                end: 10,
                value: 1.0,
            },
            QuantitativeRecord {
                start: 2,
                end: 4,
                value: 5.0,
            },
        ];
        let right = [
            QuantitativeRecord {
                start: 0,
                end: 5,
                value: 1.0,
            },
            QuantitativeRecord {
                start: 5,
                end: 10,
                value: 0.0,
            },
        ];
        let added =
            compose_quantitative_records(&left, &right, QuantitativeArithmetic::Add, 0, 10, 100);
        assert_eq!(
            added.records,
            vec![
                QuantitativeRecord {
                    start: 0,
                    end: 2,
                    value: 2.0,
                },
                QuantitativeRecord {
                    start: 2,
                    end: 4,
                    value: 6.0,
                },
                QuantitativeRecord {
                    start: 4,
                    end: 5,
                    value: 2.0,
                },
                QuantitativeRecord {
                    start: 5,
                    end: 10,
                    value: 1.0,
                },
            ]
        );
        let divided =
            compose_quantitative_records(&left, &right, QuantitativeArithmetic::Divide, 0, 10, 100);
        assert_eq!(divided.provenance.divide_by_zero_segments, 1);
        assert_eq!(divided.records.last().unwrap().end, 5);
    }

    #[test]
    fn copy_number_order_is_deterministic_and_places_missing_samples_last() {
        let values = vec![Some(1.0), Some(3.0), None, Some(3.0)];
        assert_eq!(
            copy_number_sample_order(4, &values, CopyNumberSortMode::Amplification),
            vec![1, 3, 0, 2]
        );
        assert_eq!(
            copy_number_sample_order(4, &values, CopyNumberSortMode::Deletion),
            vec![0, 1, 3, 2]
        );
        assert_eq!(
            copy_number_sample_order(4, &values, CopyNumberSortMode::FileOrder),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn copy_number_layout_only_materializes_visible_sample_rows() {
        let query = CopyNumberQuery {
            samples: (0..5).map(|index| format!("sample-{index}")).collect(),
            segments: (0..5)
                .map(|sample_index| rgv_hts::CopyNumberSegment {
                    sample_index,
                    start: 100,
                    end: 200,
                    marker_count: Some(10),
                    value: 1.0 + sample_index as f32,
                })
                .collect(),
            sample_sort_values: vec![None; 5],
            inferred_value_semantics: CopyNumberValueSemantics::Absolute,
            provenance: rgv_hts::CopyNumberQueryProvenance::default(),
        };
        let settings = CopyNumberTrackSettings {
            row_height: 10.0,
            ..CopyNumberTrackSettings::default()
        };
        let layout = layout_copy_number(
            &query,
            viewport(),
            TrackLayoutOptions {
                top: 0.0,
                height: copy_number_track_height(5, 10.0),
                ..TrackLayoutOptions::default()
            },
            &settings,
            &QuantitativeTrackSettings::default(),
            None,
            44.0,
            64.0,
        );
        assert_eq!(
            layout
                .visible_rows
                .iter()
                .map(|row| row.sample_index)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(layout.stats.rendered_records, 2);
        assert_eq!(layout.stats.hidden_records, 3);
        assert!((layout.baseline - 2.0).abs() < f32::EPSILON);
        assert_eq!(layout.value_semantics, CopyNumberValueSemantics::Absolute);
        assert!((copy_number_track_height(5, 10.0) - 74.0).abs() < f32::EPSILON);
    }

    #[test]
    fn mutation_layout_virtualizes_samples_and_marks_repeated_calls() {
        let mutation = |sample_index, gene: &str| rgv_hts::MutationRecord {
            sample_index,
            start: 120,
            end: 121,
            source_start: 121,
            source_end: 121,
            insertion_boundary: false,
            gene: gene.to_owned(),
            classification: "Missense_Mutation".to_owned(),
            variant_type: "SNP".to_owned(),
            reference: "C".to_owned(),
            tumor_allele_one: "C".to_owned(),
            tumor_allele_two: "T".to_owned(),
            annotations: Vec::new(),
            annotation_values_omitted: 0,
        };
        let query = rgv_hts::MutationQuery {
            headers: Vec::new(),
            samples: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            records: vec![
                mutation(0, "TP53"),
                mutation(1, "KRAS"),
                mutation(1, "KRAS"),
            ],
            provenance: rgv_hts::MutationQueryProvenance::default(),
        };
        let layout = layout_mutations(
            &query,
            viewport(),
            TrackLayoutOptions {
                top: 0.0,
                height: mutation_track_height(3, 16.0),
                ..TrackLayoutOptions::default()
            },
            &MutationTrackSettings::default(),
            40.0,
            56.0,
        );

        assert_eq!(layout.visible_rows.len(), 1);
        assert_eq!(layout.visible_rows[0].sample_index, 1);
        assert_eq!(layout.stats.rendered_records, 2);
        assert!(layout.display_list.rectangles.len() >= 6);
        assert_eq!(layout.display_list.texts.len(), 2);
        assert!((mutation_track_height(3, 16.0) - 72.0).abs() < f32::EPSILON);
    }

    #[test]
    fn interaction_layout_draws_intrachromosomal_arcs_and_cross_contig_markers() {
        let endpoint = |contig: &str, start, end, name: &str| rgv_hts::InteractionEndpoint {
            contig: contig.to_owned(),
            start,
            end,
            name: Some(name.to_owned()),
            strand: Strand::Unstranded,
        };
        let records = [
            InteractionRecord {
                source: endpoint("chr1", 110, 120, "a"),
                target: endpoint("chr1", 180, 190, "b"),
                name: Some("intra".to_owned()),
                score: Some(100.0),
                value: None,
                experiment: None,
                color: Some([10, 20, 30, 255]),
                directional: false,
            },
            InteractionRecord {
                source: endpoint("chr1", 130, 140, "local"),
                target: endpoint("chr2", 500, 510, "remote"),
                name: Some("cross".to_owned()),
                score: Some(50.0),
                value: None,
                experiment: None,
                color: None,
                directional: true,
            },
        ];
        let (display, stats) = layout_interactions(
            &records,
            "chr1",
            viewport(),
            TrackLayoutOptions::default(),
            &InteractionTrackSettings::default(),
        );
        assert_eq!(stats.rendered_records, 2);
        assert!(display.rectangles.len() >= INTERACTION_ARC_SEGMENTS + 4);
        assert_eq!(display.texts.len(), 2);
        assert!(display.texts[1].text.contains("chr2:501"));
    }

    #[test]
    fn interaction_layout_obeys_filter_fixed_scale_and_hard_record_cap() {
        let records = (0..MAX_RENDERED_INTERACTIONS + 10)
            .map(|index| InteractionRecord {
                source: rgv_hts::InteractionEndpoint {
                    contig: "chr1".to_owned(),
                    start: 100 + u64::try_from(index % 20).unwrap(),
                    end: 101 + u64::try_from(index % 20).unwrap(),
                    name: None,
                    strand: Strand::Unstranded,
                },
                target: rgv_hts::InteractionEndpoint {
                    contig: "chr1".to_owned(),
                    start: 180,
                    end: 181,
                    name: None,
                    strand: Strand::Unstranded,
                },
                name: None,
                score: Some(if index == 0 { 1.0 } else { 10.0 }),
                value: None,
                experiment: None,
                color: None,
                directional: false,
            })
            .collect::<Vec<_>>();
        let settings = InteractionTrackSettings {
            minimum_score: Some(5.0),
            fixed_maximum_score: Some(20.0),
            show_labels: false,
            ..InteractionTrackSettings::default()
        };
        let (_, stats) = layout_interactions(
            &records,
            "chr1",
            viewport(),
            TrackLayoutOptions::default(),
            &settings,
        );
        assert_eq!(stats.rendered_records, MAX_RENDERED_INTERACTIONS);
        assert_eq!(stats.hidden_records, 10);
    }

    #[test]
    fn genotype_colour_uses_selected_cohort_calls_instead_of_variant_type() {
        let record = VariantRecord {
            contig: "chr1".to_owned(),
            start: 120,
            end: 121,
            id: None,
            reference: b"A".to_vec(),
            alternates: vec![b"T".to_vec()],
            quality: Some(50.0),
            filters: Vec::new(),
            info: Vec::new(),
            genotypes: vec![rgv_hts::VariantGenotype {
                sample_index: 0,
                alleles: vec![
                    rgv_hts::VariantGenotypeAllele {
                        index: Some(0),
                        phased: false,
                    },
                    rgv_hts::VariantGenotypeAllele {
                        index: Some(1),
                        phased: false,
                    },
                ],
                fields: Vec::new(),
            }],
            variant_types: vec![rgv_hts::VariantType::Snv],
            breakends: Vec::new(),
        };
        assert_eq!(genotype_color(&record), Rgba::new(0.94, 0.54, 0.12, 0.96));
        let settings = VariantTrackSettings {
            color_mode: VariantColorMode::Genotype,
            ..VariantTrackSettings::default()
        };
        let (display, stats) = layout_variants(
            &[record],
            viewport(),
            TrackLayoutOptions::default(),
            &settings,
        );
        assert_eq!(stats.rendered_records, 1);
        assert_eq!(
            display.rectangles[1].color,
            Rgba::new(0.94, 0.54, 0.12, 0.96)
        );
    }

    #[test]
    fn gwas_layout_transforms_p_values_and_preserves_selectable_point_identity() {
        let records = vec![
            rgv_hts::GwasRecord {
                contig: "chr1".to_owned(),
                position: 125,
                identifier: "rs-low".to_owned(),
                p_value: 0.1,
                minus_log10_p: 1.0,
                fields: Vec::new(),
            },
            rgv_hts::GwasRecord {
                contig: "chr1".to_owned(),
                position: 175,
                identifier: "rs-high".to_owned(),
                p_value: 1e-30,
                minus_log10_p: 30.0,
                fields: Vec::new(),
            },
        ];
        let settings = GwasTrackSettings {
            color_mode: GwasColorMode::Uniform,
            uniform_color: [10, 20, 30, 255],
            maximum_score: 25.0,
            point_size: 5.0,
            ..GwasTrackSettings::default()
        };
        let options = TrackLayoutOptions {
            selected_record: Some(1),
            ..TrackLayoutOptions::default()
        };
        let (display, stats) = layout_gwas(&records, viewport(), options, &settings);
        assert_eq!(stats.rendered_records, 2);
        assert_eq!(stats.quantitative_maximum, Some(25.0));
        // Axis, first point, selection halo, selected point.
        assert_eq!(display.rectangles.len(), 4);
        assert!((display.rectangles[1].width - 5.0).abs() < f32::EPSILON);
        assert_eq!(
            display.rectangles[3].color,
            rgba_from_bytes([10, 20, 30, 255])
        );
        assert!(display.rectangles[3].y < display.rectangles[1].y);
        assert_eq!(display.rectangles[3].object_id, options.object_id_base + 1);
    }

    #[test]
    fn gwas_chromosome_and_alternating_colours_are_stable() {
        let chromosome = GwasTrackSettings::default();
        assert_ne!(
            gwas_point_color("chr1", &chromosome),
            gwas_point_color("chr2", &chromosome)
        );
        let alternating = GwasTrackSettings {
            color_mode: GwasColorMode::Alternating,
            alternating_color_one: [1, 2, 3, 255],
            alternating_color_two: [4, 5, 6, 255],
            ..GwasTrackSettings::default()
        };
        assert_eq!(
            gwas_point_color("1", &alternating),
            rgba_from_bytes([1, 2, 3, 255])
        );
        assert_eq!(
            gwas_point_color("chr2", &alternating),
            rgba_from_bytes([4, 5, 6, 255])
        );
        assert!(
            (gwas_point_y(f32::INFINITY, 10.0, 100.0, 25.0)
                - gwas_point_y(25.0, 10.0, 100.0, 25.0))
            .abs()
                < f32::EPSILON
        );
    }
}
