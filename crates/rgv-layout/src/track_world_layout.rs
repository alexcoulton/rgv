//! World-space layout of track tiles.
//!
//! A tile is laid out once, in genomic coordinates relative to its own start, on the engine
//! side. Nothing here knows the viewport: panning and zooming within a level reuse the scene
//! unchanged, and the only per-frame work is [`place_track_labels`], which decides label
//! visibility from the camera.
//!
//! # Seam-stable rows
//!
//! [`pack_seam_stable_rows`] is first-fit packing over features sorted by
//! `(start, end, identity)`. A feature's row depends only on its *overlap cluster*: the
//! maximal run of features chained by (padded) overlap. A cluster starts where every row is
//! free, so packing it gives the same rows whatever precedes it in the input. Two tiles that
//! each read the whole cluster of a feature spanning their seam therefore give that feature the
//! same row. Sources read the closure of the clusters touching a tile
//! ([`cluster_closure_extent`]) and pass the result as `TileFeature::row_hint`.

use std::{collections::BTreeMap, sync::Arc};

use rgv_model::{
    BinSemantics, FeatureRowAxis, GenotypeCall, Strand, TRACK_SCENE_FLAG_BIN_MEAN,
    TRACK_SCENE_FLAG_POINT, TRACK_SCENE_NO_RECORD, TileVariantKind, TrackColor, TrackColorToken,
    TrackRecordId, TrackRendererId, TrackSceneInstance, TrackSceneLabel, TrackSceneRecord,
    TrackSceneShape, TrackSceneVertical, TrackTile, TrackTileError, TrackTileKey,
    TrackTilePayload, TrackTileScene,
};

/// Points kept free between two features packed on one row.
pub const WORLD_FEATURE_ROW_PADDING_POINTS: u64 = 2;
/// Longest label, in characters.
pub const MAXIMUM_WORLD_LABEL_CHARACTERS: usize = 32;
/// Lane fraction at which interaction anchors start and arcs end.
const INTERACTION_BASELINE: f32 = 0.92;

/// Bases kept free between features on one row at `level`: two nominal points.
#[must_use]
pub const fn world_feature_row_padding(level: u8) -> u64 {
    TrackTileKey::span_at(level) / 1_024 * WORLD_FEATURE_ROW_PADDING_POINTS
}

/// One interval to pack: start, end (exclusive) and the identity that breaks ties.
pub type WorldRowItem = (u64, u64, TrackRecordId);

/// First-fit rows for `items`, returned in input order. Deterministic: ties are broken by
/// identity, never by input position, so any input holding the same complete clusters yields
/// the same row for each member.
#[must_use]
pub fn pack_seam_stable_rows(items: &[WorldRowItem], padding: u64) -> Vec<u32> {
    let mut order = (0..items.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| (items[*index], *index));
    let mut row_ends = Vec::<u64>::new();
    let mut rows = vec![0_u32; items.len()];
    for index in order {
        let (start, end, _) = items[index];
        let padded_end = end.max(start.saturating_add(1)).saturating_add(padding);
        let row = row_ends.iter().position(|row_end| *row_end <= start);
        let row = if let Some(row) = row {
            row_ends[row] = padded_end;
            row
        } else {
            row_ends.push(padded_end);
            row_ends.len() - 1
        };
        rows[index] = u32::try_from(row).unwrap_or(u32::MAX);
    }
    rows
}

/// The interval a source must have read for the rows of everything in `[start, end)` to be
/// seam-stable: the padded extent of `items`, which must be every feature intersecting the
/// padded `[start, end)`. Returns `None` when `[start, end)` already contains it (closed).
#[must_use]
pub fn cluster_closure_extent(
    items: &[WorldRowItem],
    start: u64,
    end: u64,
    padding: u64,
) -> Option<(u64, u64)> {
    let low = items
        .iter()
        .map(|item| item.0.saturating_sub(padding))
        .min()
        .unwrap_or(start)
        .min(start);
    let high = items
        .iter()
        .map(|item| item.1.saturating_add(padding))
        .max()
        .unwrap_or(end)
        .max(end);
    (low < start || high > end).then_some((low, high))
}

fn wrong_payload(expected: &TrackRendererId) -> TrackTileError {
    TrackTileError::Source(format!("the {expected} layout was given another payload kind"))
}

fn row_u16(row: u32) -> u16 {
    u16::try_from(row).unwrap_or(u16::MAX)
}

fn strand_color(strand: Strand) -> TrackColor {
    TrackColor::Token(match strand {
        Strand::Forward => TrackColorToken::FEATURE_FORWARD,
        Strand::Reverse => TrackColorToken::FEATURE_REVERSE,
        Strand::Unstranded => TrackColorToken::FEATURE_UNSTRANDED,
    })
}

fn truncated_label(text: &str) -> Arc<str> {
    if text.chars().count() <= MAXIMUM_WORLD_LABEL_CHARACTERS {
        Arc::from(text)
    } else {
        text.chars()
            .take(MAXIMUM_WORLD_LABEL_CHARACTERS)
            .collect::<String>()
            .into()
    }
}

/// Lays out a [`TrackTilePayload::Features`] tile.
#[allow(clippy::too_many_lines)]
pub fn lay_out_feature_tile(tile: &TrackTile) -> Result<TrackTileScene, TrackTileError> {
    let TrackTilePayload::Features(features) = &tile.payload else {
        return Err(wrong_payload(&TrackRendererId::FEATURES));
    };
    let mut scene = TrackTileScene::empty(&tile.contig, tile.key, TrackRendererId::FEATURES);
    scene.lod = tile.lod;
    let fallback_rows = match features.rows {
        FeatureRowAxis::Packed
            if features
                .features
                .iter()
                .any(|feature| feature.row_hint.is_none()) =>
        {
            let items = features
                .features
                .iter()
                .map(|feature| (feature.start, feature.end, feature.id))
                .collect::<Vec<_>>();
            Some(pack_seam_stable_rows(
                &items,
                world_feature_row_padding(tile.key.level),
            ))
        }
        _ => None,
    };
    if let FeatureRowAxis::Fixed { labels } = &features.rows {
        scene.row_labels = Some(Arc::clone(labels));
        scene.row_count = u32::try_from(labels.len()).unwrap_or(u32::MAX);
    }
    if let FeatureRowAxis::Scatter { minimum, maximum } = features.rows {
        scene.include_value(minimum);
        scene.include_value(maximum);
    }
    let (tile_start, tile_end) = (tile.key.start(), tile.key.end());
    for (index, feature) in features.features.iter().enumerate() {
        let row = feature
            .row_hint
            .or_else(|| fallback_rows.as_ref().map(|rows| rows[index]))
            .unwrap_or(0);
        let record = u32::try_from(scene.records.len()).unwrap_or(TRACK_SCENE_NO_RECORD);
        scene.records.push(TrackSceneRecord {
            id: feature.id,
            start: feature.start,
            end: feature.end,
            row,
            name: feature.name.clone(),
        });
        let color = feature.color.unwrap_or_else(|| strand_color(feature.strand));
        let instance = |start, end, shape, vertical, flags, y, color| {
            TrackSceneInstance::new(
                tile.key,
                start,
                end,
                row_u16(row),
                shape,
                vertical,
                flags,
                y,
                color,
                record,
            )
        };
        match features.rows {
            FeatureRowAxis::Scatter { .. } => {
                let value = feature.score.unwrap_or(0.0);
                scene.include_value(value);
                scene.instances.push(instance(
                    feature.start,
                    feature.end,
                    TrackSceneShape::Diamond,
                    TrackSceneVertical::Value,
                    TRACK_SCENE_FLAG_POINT,
                    (value, value),
                    color,
                ));
                continue;
            }
            FeatureRowAxis::Fixed { .. } => {
                scene.instances.push(instance(
                    feature.start,
                    feature.end,
                    TrackSceneShape::Box,
                    TrackSceneVertical::Row,
                    0,
                    (0.06, 0.94),
                    color,
                ));
                continue;
            }
            FeatureRowAxis::Packed => {}
        }
        scene.row_count = scene.row_count.max(row.saturating_add(1));
        let terminal = match feature.strand {
            Strand::Forward => TrackSceneShape::ArrowRight,
            Strand::Reverse => TrackSceneShape::ArrowLeft,
            Strand::Unstranded => TrackSceneShape::Box,
        };
        if feature.blocks.is_empty() {
            scene.instances.push(instance(
                feature.start,
                feature.end,
                terminal,
                TrackSceneVertical::Row,
                0,
                (0.0, 1.0),
                color,
            ));
        } else {
            scene.instances.push(instance(
                feature.start,
                feature.end,
                match feature.strand {
                    Strand::Forward => TrackSceneShape::LineChevronRight,
                    Strand::Reverse => TrackSceneShape::LineChevronLeft,
                    Strand::Unstranded => TrackSceneShape::Line,
                },
                TrackSceneVertical::Row,
                0,
                (0.0, 1.0),
                TrackColor::Token(TrackColorToken::FEATURE_CONNECTOR),
            ));
            for block in &feature.blocks {
                if block.end <= tile_start || block.start >= tile_end {
                    continue;
                }
                // The block that ends the transcript in its reading direction carries the
                // strand arrow.
                let shape = match feature.strand {
                    Strand::Forward if block.end == feature.end => TrackSceneShape::ArrowRight,
                    Strand::Reverse if block.start == feature.start => TrackSceneShape::ArrowLeft,
                    _ => TrackSceneShape::Box,
                };
                scene.instances.push(instance(
                    block.start,
                    block.end,
                    shape,
                    TrackSceneVertical::Row,
                    0,
                    if block.thick { (0.0, 1.0) } else { (0.25, 0.75) },
                    color,
                ));
            }
        }
        if feature.labelable
            && let Some(name) = feature.name.as_deref().filter(|name| !name.is_empty())
        {
            scene.labels.push(TrackSceneLabel {
                record,
                start: feature.start,
                end: feature.end,
                row,
                text: truncated_label(name),
            });
        }
    }
    Ok(scene)
}

/// Lays out a [`TrackTilePayload::Bins`] tile: per run of equal bins, a min-max envelope and
/// the mean drawn from the zero baseline. Values stay data values; the lane's value range maps
/// them to points at draw time.
pub fn lay_out_bin_tile(tile: &TrackTile) -> Result<TrackTileScene, TrackTileError> {
    let TrackTilePayload::Bins(bins) = &tile.payload else {
        return Err(wrong_payload(&TrackRendererId::BINS));
    };
    let mut scene = TrackTileScene::empty(&tile.contig, tile.key, TrackRendererId::BINS);
    scene.lod = tile.lod;
    let mean_token = match bins.semantics {
        BinSemantics::Signal => TrackColorToken::BIN_MEAN,
        BinSemantics::FeatureDensity | BinSemantics::VariantDensity => TrackColorToken::DENSITY,
    };
    let width = bins.bin_width.max(1);
    let start_of = |bin: usize| tile.key.start().saturating_add(bin as u64 * width);
    let mut first = 0;
    while first < bins.bins.len() {
        let bin = bins.bins[first];
        let mut last = first + 1;
        // NaN never compares equal, so empty bins are skipped one at a time.
        while last < bins.bins.len() && bins.bins[last] == bin {
            last += 1;
        }
        if !bin.is_empty() {
            let record = u32::try_from(first).unwrap_or(TRACK_SCENE_NO_RECORD);
            let (start, end) = (start_of(first), start_of(last));
            scene.include_value(0.0);
            scene.include_value(bin.minimum);
            scene.include_value(bin.maximum);
            scene.include_value(bin.mean);
            if bin.minimum < bin.maximum {
                scene.instances.push(TrackSceneInstance::new(
                    tile.key,
                    start,
                    end,
                    0,
                    TrackSceneShape::Box,
                    TrackSceneVertical::Value,
                    0,
                    (bin.minimum, bin.maximum),
                    TrackColor::Token(TrackColorToken::BIN_ENVELOPE),
                    record,
                ));
            }
            scene.instances.push(TrackSceneInstance::new(
                tile.key,
                start,
                end,
                0,
                TrackSceneShape::Box,
                TrackSceneVertical::Value,
                TRACK_SCENE_FLAG_BIN_MEAN,
                (0.0, bin.mean),
                TrackColor::Token(mean_token),
                record,
            ));
        }
        first = last;
    }
    Ok(scene)
}

/// Lays out a [`TrackTilePayload::Variants`] tile: the variants on row 0 and, when the tile
/// carries a genotype matrix, sample `s` on row `s + 1` with one cell per loaded variant.
pub fn lay_out_variant_tile(tile: &TrackTile) -> Result<TrackTileScene, TrackTileError> {
    let TrackTilePayload::Variants(variants) = &tile.payload else {
        return Err(wrong_payload(&TrackRendererId::VARIANTS));
    };
    let mut scene = TrackTileScene::empty(&tile.contig, tile.key, TrackRendererId::VARIANTS);
    scene.lod = tile.lod;
    scene.row_count = 1;
    for (index, variant) in variants.variants.iter().enumerate() {
        let record = u32::try_from(index).unwrap_or(TRACK_SCENE_NO_RECORD);
        scene.records.push(TrackSceneRecord {
            id: variant.id,
            start: variant.start,
            end: variant.end,
            row: 0,
            name: variant.name.clone(),
        });
        scene.instances.push(TrackSceneInstance::new(
            tile.key,
            variant.start,
            variant.end,
            0,
            TrackSceneShape::Box,
            TrackSceneVertical::Row,
            0,
            (0.1, 0.9),
            TrackColor::Token(match variant.kind {
                TileVariantKind::Snv => TrackColorToken::VARIANT_SNV,
                TileVariantKind::Insertion => TrackColorToken::VARIANT_INSERTION,
                TileVariantKind::Deletion => TrackColorToken::VARIANT_DELETION,
                TileVariantKind::Structural => TrackColorToken::VARIANT_STRUCTURAL,
                TileVariantKind::Other => TrackColorToken::VARIANT_OTHER,
            }),
            record,
        ));
    }
    if let Some(matrix) = &variants.genotypes {
        let samples = matrix.sample_count().min(usize::from(u16::MAX) - 1);
        scene.row_count = u32::try_from(samples + 1).unwrap_or(u32::MAX);
        scene.row_labels = Some(
            std::iter::once(String::new())
                .chain(matrix.samples.iter().take(samples).cloned())
                .collect(),
        );
        for (index, variant) in variants.variants.iter().enumerate() {
            if !matrix.is_loaded(index) {
                continue;
            }
            let record = u32::try_from(index).unwrap_or(TRACK_SCENE_NO_RECORD);
            for sample in 0..samples {
                let Some(call) = matrix.call(index, sample) else {
                    continue;
                };
                scene.instances.push(TrackSceneInstance::new(
                    tile.key,
                    variant.start,
                    variant.end,
                    row_u16(u32::try_from(sample + 1).unwrap_or(u32::MAX)),
                    TrackSceneShape::Box,
                    TrackSceneVertical::Row,
                    0,
                    (0.0, 1.0),
                    TrackColor::Token(match call {
                        GenotypeCall::HomRef => TrackColorToken::GENOTYPE_HOM_REF,
                        GenotypeCall::Het => TrackColorToken::GENOTYPE_HET,
                        GenotypeCall::HomAlt => TrackColorToken::GENOTYPE_HOM_ALT,
                        GenotypeCall::NoCall => TrackColorToken::GENOTYPE_NO_CALL,
                    }),
                    record,
                ));
            }
        }
    }
    Ok(scene)
}

/// Lays out a [`TrackTilePayload::Interactions`] tile: an arc between the anchor midpoints
/// (taller for longer spans, relative to the tile level) and a box on each anchor.
pub fn lay_out_interaction_tile(tile: &TrackTile) -> Result<TrackTileScene, TrackTileError> {
    let TrackTilePayload::Interactions(interactions) = &tile.payload else {
        return Err(wrong_payload(&TrackRendererId::INTERACTIONS));
    };
    let mut scene = TrackTileScene::empty(&tile.contig, tile.key, TrackRendererId::INTERACTIONS);
    scene.lod = tile.lod;
    #[allow(clippy::cast_precision_loss)]
    let full_height_span = tile.key.span() as f32 * 2.0;
    for (index, interaction) in interactions.interactions.iter().enumerate() {
        let record = u32::try_from(index).unwrap_or(TRACK_SCENE_NO_RECORD);
        let (start, end) = interaction.target.map_or(interaction.source, |target| {
            (
                interaction.source.0.min(target.0),
                interaction.source.1.max(target.1),
            )
        });
        scene.records.push(TrackSceneRecord {
            id: interaction.id,
            start,
            end,
            row: 0,
            name: interaction.name.clone(),
        });
        let color = interaction
            .color
            .unwrap_or(TrackColor::Token(TrackColorToken::INTERACTION_ARC));
        let anchor = |(start, end): (u64, u64)| {
            TrackSceneInstance::new(
                tile.key,
                start,
                end,
                0,
                TrackSceneShape::Box,
                TrackSceneVertical::Lane,
                0,
                (INTERACTION_BASELINE, 1.0),
                interaction
                    .color
                    .unwrap_or(TrackColor::Token(TrackColorToken::INTERACTION_ANCHOR)),
                record,
            )
        };
        if let Some(target) = interaction.target {
            let middle = |(start, end): (u64, u64)| start + (end - start.min(end)) / 2;
            let (left, right) = (
                middle(interaction.source).min(middle(target)),
                middle(interaction.source).max(middle(target)),
            );
            #[allow(clippy::cast_precision_loss)]
            let height = ((right - left) as f32 / full_height_span).clamp(0.12, 1.0);
            scene.instances.push(TrackSceneInstance::new(
                tile.key,
                left,
                right,
                0,
                TrackSceneShape::Arc,
                TrackSceneVertical::Lane,
                0,
                (INTERACTION_BASELINE * (1.0 - height), INTERACTION_BASELINE),
                color,
                record,
            ));
            scene.instances.push(anchor(target));
        }
        scene.instances.push(anchor(interaction.source));
    }
    Ok(scene)
}

/// Where the pointer is, in the coordinates scenes are laid out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackScenePoint {
    /// Genomic position, in bases.
    pub position: f64,
    /// Row coordinate: row index plus the fraction down that row.
    pub row: f64,
    /// Fraction down the whole lane.
    pub lane_fraction: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackSceneHit {
    /// Index into [`TrackTileScene::instances`].
    pub instance: usize,
    /// Index into [`TrackTileScene::records`], or the bin index for a bin scene.
    pub record_index: u32,
    /// Stable identity of the record; `None` for a bin.
    pub id: Option<TrackRecordId>,
}

/// Where a pointer is vertically, relative to the rows of a lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSceneRowPosition {
    /// Row index.
    pub row: u32,
    /// Fraction down the row's **content height** (not its pitch), in `0.0..=1.0`: the unit
    /// `Row` instances state their vertical extent in ([`TrackSceneInstance`]'s `y0`/`y1`).
    pub content_fraction: f64,
}

/// A pointer position in unambiguous scene units, for [`hit_test_track_scene_at`].
///
/// Unlike [`TrackScenePoint`], whose `row` is a single number that cannot say whether the
/// fraction is of the row pitch or of the row height, the row position here is explicit and
/// is absent when the pointer is not over any row's content (above row 0, or in the gap
/// between two rows when the pitch exceeds the height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSceneProbe {
    /// Genomic position, in bases.
    pub position: f64,
    /// The row content under the pointer; `None` in a gap.
    pub row: Option<TrackSceneRowPosition>,
    /// Fraction down the whole lane.
    pub lane_fraction: f64,
}

/// Topmost record-bearing instance under `point`. `tolerance_bases` is half the renderer's
/// one-point minimum width expressed in bases (`0.5 / points_per_base`), so anything that is
/// drawn can be hit.
///
/// `point.row` is read as row index plus the fraction down that row's *content height*. A
/// caller that derives it by dividing by the row pitch hits the gap between rows; prefer
/// [`hit_test_track_scene_at`], whose argument cannot be misread.
#[must_use]
pub fn hit_test_track_scene(
    scene: &TrackTileScene,
    point: TrackScenePoint,
    tolerance_bases: f64,
) -> Option<TrackSceneHit> {
    let row = point.row.floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let row_position =
        (row >= 0.0 && row <= f64::from(u32::MAX)).then_some(TrackSceneRowPosition {
            row: row as u32,
            content_fraction: point.row - row,
        });
    hit_test_track_scene_at(
        scene,
        TrackSceneProbe {
            position: point.position,
            row: row_position,
            lane_fraction: point.lane_fraction,
        },
        tolerance_bases,
    )
}

/// Topmost record-bearing instance under `probe`; see [`hit_test_track_scene`] for the
/// tolerance. A `Row` instance is hit only when the probe is over the content of its row and
/// inside the instance's vertical extent there (connector lines take the whole content
/// height); a probe in a gap between rows hits no `Row` instance.
#[must_use]
pub fn hit_test_track_scene_at(
    scene: &TrackTileScene,
    probe: TrackSceneProbe,
    tolerance_bases: f64,
) -> Option<TrackSceneHit> {
    #[allow(clippy::cast_precision_loss)]
    let tile_start = scene.key.start() as f64;
    scene
        .instances
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, instance)| {
            let record_index = instance.record_index()?;
            let start = tile_start + f64::from(instance.start);
            let end = start + f64::from(instance.length);
            let tolerance = if instance.flags() & TRACK_SCENE_FLAG_POINT == 0 {
                tolerance_bases
            } else {
                tolerance_bases * 8.0
            };
            if probe.position < start - tolerance || probe.position >= end + tolerance {
                return None;
            }
            let (y0, y1) = (f64::from(instance.y0), f64::from(instance.y1));
            let inside = match instance.vertical() {
                TrackSceneVertical::Row => probe.row.is_some_and(|position| {
                    position.row == u32::from(instance.row())
                        && (matches!(
                            instance.shape(),
                            Some(
                                TrackSceneShape::Line
                                    | TrackSceneShape::LineChevronLeft
                                    | TrackSceneShape::LineChevronRight
                            )
                        ) || (position.content_fraction >= y0
                            && position.content_fraction <= y1))
                }),
                // The value axis is the lane's to define; a bin is hit by its column.
                TrackSceneVertical::Value => true,
                TrackSceneVertical::Lane => {
                    if instance.shape() == Some(TrackSceneShape::Arc) {
                        let t = ((probe.position - start) / (end - start).max(1.0)).clamp(0.0, 1.0);
                        let curve = y1 + (y0 - y1) * (1.0 - 4.0 * (t - 0.5) * (t - 0.5));
                        (probe.lane_fraction - curve).abs() <= 0.06
                    } else {
                        probe.lane_fraction >= y0 && probe.lane_fraction <= y1
                    }
                }
            };
            inside.then(|| TrackSceneHit {
                instance: index,
                record_index,
                id: scene.records.get(record_index as usize).map(|record| record.id),
            })
        })
}

/// The camera, as far as labels are concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLabelView {
    /// Exact genomic position of the lane's left edge.
    pub start: f64,
    pub points_per_base: f64,
    pub width_points: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedTrackLabel {
    pub id: TrackRecordId,
    pub row: u32,
    /// Horizontal centre, in points from the lane's left edge.
    pub center: f32,
    pub width: f32,
    pub text: Arc<str>,
}

/// Decides which labels of the visible scenes are drawn for `view`. A label is anchored on its
/// feature's genomic extent; it is shown when the visible part of the feature is at least
/// three points wide and it does not collide with a label already accepted on its row. A
/// feature present in several tiles is labelled once. Candidates are visited in
/// `(row, start, identity)` order, so the outcome does not depend on tile arrival order.
#[must_use]
pub fn place_track_labels<'scene>(
    scenes: impl IntoIterator<Item = &'scene TrackTileScene>,
    view: TrackLabelView,
    measure: &dyn Fn(&str) -> f32,
    maximum_labels: usize,
) -> Vec<PlacedTrackLabel> {
    let mut candidates = BTreeMap::new();
    for scene in scenes {
        for label in &scene.labels {
            let Some(record) = scene.records.get(label.record as usize) else {
                continue;
            };
            candidates
                .entry((label.row, label.start, record.id))
                .or_insert(label);
        }
    }
    let mut placed = Vec::<PlacedTrackLabel>::new();
    let mut row_right_edge = (u32::MAX, f64::NEG_INFINITY);
    for ((row, _, id), label) in candidates {
        if placed.len() >= maximum_labels {
            break;
        }
        #[allow(clippy::cast_precision_loss)]
        let left = ((label.start as f64 - view.start) * view.points_per_base).max(0.0);
        #[allow(clippy::cast_precision_loss)]
        let right = ((label.end as f64 - view.start) * view.points_per_base).min(view.width_points);
        if right - left < 3.0 {
            continue;
        }
        let width = f64::from(measure(&label.text));
        if !width.is_finite() || width + 4.0 > view.width_points {
            continue;
        }
        let half = width * 0.5;
        let center = ((left + right) * 0.5).clamp(
            half + 2.0,
            (view.width_points - half - 2.0).max(half + 2.0),
        );
        if row_right_edge.0 != row {
            row_right_edge = (row, f64::NEG_INFINITY);
        }
        if center - half < row_right_edge.1 + 6.0 {
            continue;
        }
        row_right_edge.1 = center + half;
        #[allow(clippy::cast_possible_truncation)]
        placed.push(PlacedTrackLabel {
            id,
            row,
            center: center as f32,
            width: width as f32,
            text: Arc::clone(&label.text),
        });
    }
    placed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::too_many_lines, clippy::format_push_string)]

    use rgv_model::{
        BinTile, FeatureTile, GenotypeMatrix, InteractionTile, TileBin, TileFeature,
        TileFeatureBlock, TileInteraction, TileVariant, TrackTileLod, TrackTileProvenance,
        VariantTile,
    };

    use super::*;

    fn id(value: u64) -> TrackRecordId {
        TrackRecordId(value)
    }

    fn tile(key: TrackTileKey, payload: TrackTilePayload) -> TrackTile {
        TrackTile {
            contig: "chr1".to_owned(),
            key,
            lod: TrackTileLod::Detail,
            payload,
            provenance: TrackTileProvenance::default(),
        }
    }

    fn feature(identity: u64, start: u64, end: u64, row_hint: Option<u32>) -> TileFeature {
        TileFeature {
            id: id(identity),
            start,
            end,
            strand: Strand::Forward,
            blocks: Vec::new(),
            name: Some(format!("f{identity}").into()),
            score: None,
            color: None,
            row_hint,
            labelable: true,
        }
    }

    /// Deterministic pseudo-random features: clusters of overlapping intervals separated by
    /// gaps, the shape of a gene annotation.
    fn synthetic_features(seed: u64, count: usize) -> Vec<WorldRowItem> {
        let mut state = seed;
        let mut next = |modulus: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % modulus
        };
        let mut position = 0_u64;
        (0..count)
            .map(|index| {
                position += if next(6) == 0 { 2_000 + next(4_000) } else { next(300) };
                (position, position + 50 + next(1_500), id(index as u64))
            })
            .collect()
    }

    /// What a source does for one tile: read the closure of the clusters touching it, pack.
    fn rows_seen_by_tile(
        all: &[WorldRowItem],
        key: TrackTileKey,
        padding: u64,
    ) -> BTreeMap<TrackRecordId, u32> {
        let query = |start: u64, end: u64| {
            all.iter()
                .copied()
                .filter(|item| item.1 > start && item.0 < end)
                .collect::<Vec<_>>()
        };
        let (mut start, mut end) = (key.start(), key.end());
        let mut items = query(start, end);
        while let Some((low, high)) = cluster_closure_extent(&items, start, end, padding) {
            (start, end) = (low, high);
            items = query(start, end);
        }
        let rows = pack_seam_stable_rows(&items, padding);
        items
            .iter()
            .zip(rows)
            .filter(|(item, _)| item.1 > key.start() && item.0 < key.end())
            .map(|(item, row)| (item.2, row))
            .collect()
    }

    #[test]
    fn rows_never_overlap_and_respect_padding() {
        let items = synthetic_features(7, 400);
        let rows = pack_seam_stable_rows(&items, 16);
        for (left, left_row) in items.iter().zip(&rows) {
            for (right, right_row) in items.iter().zip(&rows) {
                if left.2 != right.2 && left_row == right_row && left.0 <= right.0 {
                    assert!(
                        left.1 + 16 <= right.0,
                        "{left:?} and {right:?} share row {left_row}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_feature_gets_the_same_row_in_every_tile_it_spans() {
        for (seed, level) in [(1_u64, 0_u8), (2, 1), (3, 2), (4, 3)] {
            let all = synthetic_features(seed, 600);
            let padding = world_feature_row_padding(level);
            let extent = all.iter().map(|item| item.1).max().unwrap();
            let global = pack_seam_stable_rows(&all, padding);
            let mut seam_features = 0;
            let mut previous: Option<BTreeMap<TrackRecordId, u32>> = None;
            for key in TrackTileKey::covering(level, 0, extent) {
                let rows = rows_seen_by_tile(&all, key, padding);
                // Every tile agrees with one global packing of the whole contig...
                for (identity, row) in &rows {
                    assert_eq!(global[identity.0 as usize], *row, "seed {seed} tile {key:?}");
                }
                // ...and so with its neighbour, for every feature crossing the seam.
                if let Some(previous) = &previous {
                    for (identity, row) in &rows {
                        if let Some(previous_row) = previous.get(identity) {
                            assert_eq!(previous_row, row);
                            seam_features += 1;
                        }
                    }
                }
                previous = Some(rows);
            }
            assert!(seam_features > 20, "seed {seed}: only {seam_features} seam features");
        }
    }

    #[test]
    fn packing_ignores_input_order() {
        let items = synthetic_features(11, 200);
        let rows = pack_seam_stable_rows(&items, 4);
        let mut reversed = items.clone();
        reversed.reverse();
        let mut reversed_rows = pack_seam_stable_rows(&reversed, 4);
        reversed_rows.reverse();
        assert_eq!(rows, reversed_rows);
    }

    #[test]
    fn feature_scene_keeps_whole_shapes_relative_to_the_tile_and_draws_blocks() {
        let key = TrackTileKey { level: 0, index: 2 };
        let mut transcript = feature(1, 1_900, 2_600, Some(3));
        transcript.blocks = vec![
            TileFeatureBlock { start: 1_900, end: 2_000, thick: false },
            TileFeatureBlock { start: 2_100, end: 2_200, thick: true },
            TileFeatureBlock { start: 2_500, end: 2_600, thick: true },
        ];
        let plain = feature(2, 2_300, 2_400, Some(0));
        let scene = lay_out_feature_tile(&tile(
            key,
            TrackTilePayload::Features(FeatureTile {
                features: vec![transcript, plain],
                rows: FeatureRowAxis::Packed,
            }),
        ))
        .unwrap();
        assert_eq!(scene.row_count, 4);
        assert_eq!(scene.records.len(), 2);
        assert_eq!(scene.labels.len(), 2);
        // Connector spans the whole transcript, starting left of the tile.
        let connector = scene.instances[0];
        assert_eq!(connector.shape(), Some(TrackSceneShape::LineChevronRight));
        assert_eq!((connector.start, connector.length), (-148, 700));
        // The first block ends at the tile start and is left to the neighbouring tile.
        let blocks = &scene.instances[1..3];
        assert_eq!((blocks[0].start, blocks[0].length), (52, 100));
        assert_eq!(blocks[0].shape(), Some(TrackSceneShape::Box));
        assert_eq!((blocks[0].y0, blocks[0].y1), (0.0, 1.0));
        assert_eq!(blocks[1].shape(), Some(TrackSceneShape::ArrowRight));
        assert_eq!(blocks[1].row(), 3);
        assert_eq!(scene.instances[3].shape(), Some(TrackSceneShape::ArrowRight));
        assert_eq!(scene.instances.len(), 4);
    }

    #[test]
    fn bins_merge_equal_runs_and_keep_values_in_data_space() {
        let key = TrackTileKey { level: 1, index: 0 };
        let flat = TileBin { minimum: 2.0, mean: 2.0, maximum: 2.0 };
        let peak = TileBin { minimum: 0.5, mean: 3.0, maximum: 40.0 };
        let scene = lay_out_bin_tile(&tile(
            key,
            TrackTilePayload::Bins(BinTile {
                bin_width: 2,
                bins: vec![flat, flat, flat, TileBin::EMPTY, peak],
                semantics: BinSemantics::Signal,
            }),
        ))
        .unwrap();
        assert_eq!(scene.value_range, Some((0.0, 40.0)));
        assert_eq!(scene.instances.len(), 3);
        let run = scene.instances[0];
        assert_eq!((run.start, run.length), (0, 6));
        assert_eq!(run.vertical(), TrackSceneVertical::Value);
        assert_eq!((run.y0, run.y1), (0.0, 2.0));
        let envelope = scene.instances[1];
        assert_eq!((envelope.start, envelope.length), (8, 2));
        assert_eq!((envelope.y0, envelope.y1), (0.5, 40.0));
        assert_eq!(envelope.color(), TrackColor::Token(TrackColorToken::BIN_ENVELOPE));
        assert_eq!(scene.instances[2].record_index(), Some(4));
        let hit = hit_test_track_scene(
            &scene,
            TrackScenePoint { position: 9.0, row: 0.0, lane_fraction: 0.5 },
            0.0,
        )
        .unwrap();
        assert_eq!((hit.record_index, hit.id), (4, None));
    }

    #[test]
    fn variant_scene_lays_out_the_genotype_matrix_as_palette_rows() {
        let key = TrackTileKey { level: 0, index: 0 };
        let samples: Arc<[String]> = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()].into();
        let mut matrix = GenotypeMatrix::new(samples, 2);
        matrix.set(0, 0, GenotypeCall::HomRef);
        matrix.set(0, 1, GenotypeCall::Het);
        matrix.set(0, 2, GenotypeCall::HomAlt);
        let variant = |identity, start| TileVariant {
            id: id(identity),
            start,
            end: start + 1,
            kind: TileVariantKind::Snv,
            name: None,
            quality: None,
        };
        let scene = lay_out_variant_tile(&tile(
            key,
            TrackTilePayload::Variants(VariantTile {
                variants: vec![variant(1, 100), variant(2, 200)],
                genotypes: Some(matrix),
            }),
        ))
        .unwrap();
        assert_eq!(scene.row_count, 4);
        assert_eq!(scene.row_labels.as_ref().unwrap().len(), 4);
        // Two variant marks, then three cells for the loaded variant only.
        assert_eq!(scene.instances.len(), 5);
        let cells = scene.instances[2..]
            .iter()
            .map(|cell| (cell.row(), cell.start, cell.color()))
            .collect::<Vec<_>>();
        assert_eq!(
            cells,
            vec![
                (1, 100, TrackColor::Token(TrackColorToken::GENOTYPE_HOM_REF)),
                (2, 100, TrackColor::Token(TrackColorToken::GENOTYPE_HET)),
                (3, 100, TrackColor::Token(TrackColorToken::GENOTYPE_HOM_ALT)),
            ]
        );
        // A cell is hit by row and position and reports the variant's identity.
        let hit = hit_test_track_scene(
            &scene,
            TrackScenePoint { position: 100.5, row: 2.5, lane_fraction: 0.6 },
            0.0,
        )
        .unwrap();
        assert_eq!(hit.id, Some(id(1)));
        assert!(
            hit_test_track_scene(
                &scene,
                TrackScenePoint { position: 200.5, row: 2.5, lane_fraction: 0.6 },
                0.0,
            )
            .is_none()
        );
    }

    #[test]
    fn interaction_arcs_keep_their_full_extent_and_are_hit_along_the_curve() {
        let key = TrackTileKey { level: 0, index: 1 };
        let scene = lay_out_interaction_tile(&tile(
            key,
            TrackTilePayload::Interactions(InteractionTile {
                interactions: vec![TileInteraction {
                    id: id(9),
                    source: (900, 920),
                    target: Some((1_500, 1_520)),
                    score: None,
                    color: None,
                    name: None,
                }],
            }),
        ))
        .unwrap();
        let arc = scene.instances[0];
        assert_eq!(arc.shape(), Some(TrackSceneShape::Arc));
        assert_eq!((arc.start, arc.length), (-114, 600));
        assert_eq!(scene.instances.len(), 3);
        let apex = TrackScenePoint {
            position: 1_210.0,
            row: 0.0,
            lane_fraction: f64::from(arc.y0),
        };
        assert_eq!(hit_test_track_scene(&scene, apex, 0.5).unwrap().id, Some(id(9)));
        let below_apex = TrackScenePoint { lane_fraction: 0.8, ..apex };
        assert!(hit_test_track_scene(&scene, below_apex, 0.5).is_none());
    }

    #[test]
    fn hit_testing_returns_the_stable_identity_at_one_point_tolerance() {
        let key = TrackTileKey { level: 0, index: 0 };
        let scene = lay_out_feature_tile(&tile(
            key,
            TrackTilePayload::Features(FeatureTile {
                features: vec![feature(5, 100, 101, Some(0)), feature(6, 100, 400, Some(1))],
                rows: FeatureRowAxis::Packed,
            }),
        ))
        .unwrap();
        let at = |position, row| TrackScenePoint { position, row, lane_fraction: 0.0 };
        assert_eq!(hit_test_track_scene(&scene, at(100.5, 0.5), 0.0).unwrap().id, Some(id(5)));
        // 40 bases per point: the one-base feature is drawn one point (40 bases) wide.
        assert!(hit_test_track_scene(&scene, at(115.0, 0.5), 0.0).is_none());
        assert_eq!(hit_test_track_scene(&scene, at(115.0, 0.5), 20.0).unwrap().id, Some(id(5)));
        assert_eq!(hit_test_track_scene(&scene, at(250.0, 1.2), 0.0).unwrap().id, Some(id(6)));
        assert!(hit_test_track_scene(&scene, at(250.0, 2.2), 0.0).is_none());
    }

    #[test]
    fn a_row_cap_hides_rows_beyond_it_and_reports_exactly_what_it_hid() {
        // The default cap is the legacy feature layout's row count.
        assert_eq!(
            rgv_model::DEFAULT_TRACK_TILE_FEATURE_ROWS as usize,
            crate::TrackLayoutOptions::default().maximum_rows
        );
        assert_eq!(
            rgv_model::TrackTileSettings::default().maximum_feature_rows,
            rgv_model::DEFAULT_TRACK_TILE_FEATURE_ROWS
        );
        let key = TrackTileKey { level: 0, index: 0 };
        // Twenty features stacked on twenty rows, each with a block structure and a label.
        let features = (0..20_u32)
            .map(|row| {
                let mut stacked = feature(u64::from(row), 100, 600, Some(row));
                stacked.blocks = vec![
                    TileFeatureBlock { start: 100, end: 200, thick: true },
                    TileFeatureBlock { start: 500, end: 600, thick: false },
                ];
                stacked
            })
            .collect::<Vec<_>>();
        let lay_out = || {
            lay_out_feature_tile(&tile(
                key,
                TrackTilePayload::Features(FeatureTile {
                    features: features.clone(),
                    rows: FeatureRowAxis::Packed,
                }),
            ))
            .unwrap()
        };
        let whole = lay_out();
        assert_eq!((whole.row_count, whole.row_cap), (20, None));
        assert!(whole.overflow().is_empty());
        assert_eq!(whole.instances.len(), 60);

        for (cap, hidden) in [(12_u32, 8_u32), (1, 19), (19, 1), (20, 0), (64, 0)] {
            let mut scene = lay_out();
            let overflow = scene.apply_row_cap(cap);
            assert_eq!(
                (overflow.hidden_rows, overflow.hidden_features),
                (hidden, hidden),
                "cap {cap}"
            );
            assert_eq!(scene.overflow(), overflow);
            assert_eq!(scene.row_cap, Some(cap));
            // The packed row count and the records are kept; only what is drawn shrinks.
            assert_eq!((scene.row_count, scene.records.len()), (20, 20));
            let shown = (20 - hidden) as usize;
            assert_eq!(scene.instances.len(), shown * 3);
            assert_eq!(scene.labels.len(), shown);
            assert!(scene.instances.iter().all(|instance| u32::from(instance.row()) < cap));
            assert!(scene.labels.iter().all(|label| label.row < cap));
            // Every surviving instance still names its own record, and a hidden record is
            // no longer hit where it used to be drawn.
            for instance in &scene.instances {
                let record = &scene.records[instance.record_index().unwrap() as usize];
                assert_eq!(record.row, u32::from(instance.row()));
            }
            let probe = |row| TrackSceneProbe {
                position: 150.0,
                row: Some(TrackSceneRowPosition { row, content_fraction: 0.5 }),
                lane_fraction: 0.5,
            };
            assert_eq!(
                hit_test_track_scene_at(&scene, probe(19), 0.0).is_some(),
                cap > 19
            );
            assert_eq!(hit_test_track_scene_at(&scene, probe(0), 0.0).unwrap().id, Some(id(0)));
        }

        // No cap: zero and `u32::MAX` leave the scene exactly as laid out.
        for cap in [0, u32::MAX] {
            let mut scene = lay_out();
            assert!(scene.apply_row_cap(cap).is_empty());
            assert_eq!(scene, whole);
        }
        // A second, smaller cap tightens; a larger one cannot bring rows back.
        let mut scene = lay_out();
        scene.apply_row_cap(12);
        assert_eq!(scene.apply_row_cap(16).hidden_rows, 8);
        assert_eq!(scene.apply_row_cap(4).hidden_rows, 16);
        assert_eq!(scene.instances.len(), 12);

        // Over the tiles of a lane a feature present in two tiles is one hidden feature.
        let mut left = lay_out();
        let mut right = lay_out();
        left.apply_row_cap(12);
        right.apply_row_cap(12);
        let lane = TrackTileScene::lane_overflow([&left, &right]);
        assert_eq!((lane.hidden_rows, lane.hidden_features), (8, 8));

        // Fixed (sample) rows and value-mapped scenes are never capped.
        let labels: Arc<[String]> = (0..20).map(|row| format!("s{row}")).collect();
        let mut fixed = lay_out_feature_tile(&tile(
            key,
            TrackTilePayload::Features(FeatureTile {
                features: features.clone(),
                rows: FeatureRowAxis::Fixed { labels },
            }),
        ))
        .unwrap();
        let before = fixed.clone();
        assert!(fixed.apply_row_cap(12).is_empty());
        assert_eq!(fixed, before);
    }

    #[test]
    fn a_probe_states_its_row_in_content_units_and_a_gap_hits_nothing() {
        let key = TrackTileKey { level: 0, index: 0 };
        let mut structured = feature(7, 100, 400, Some(1));
        structured.blocks = vec![
            TileFeatureBlock { start: 100, end: 150, thick: false },
            TileFeatureBlock { start: 300, end: 400, thick: true },
        ];
        let scene = lay_out_feature_tile(&tile(
            key,
            TrackTilePayload::Features(FeatureTile {
                features: vec![feature(5, 100, 400, Some(0)), structured],
                rows: FeatureRowAxis::Packed,
            }),
        ))
        .unwrap();
        let probe = |position, row: Option<(u32, f64)>| TrackSceneProbe {
            position,
            row: row.map(|(row, content_fraction)| TrackSceneRowPosition { row, content_fraction }),
            lane_fraction: 0.5,
        };
        let hit = |position, row| hit_test_track_scene_at(&scene, probe(position, row), 0.0);
        // Anywhere down a row's content, including both edges, is that row's feature.
        for fraction in [0.0, 0.5, 1.0] {
            assert_eq!(hit(250.0, Some((0, fraction))).unwrap().id, Some(id(5)));
        }
        // A gap between rows (no row position) hits no row instance, whatever the column.
        assert!(hit(250.0, None).is_none());
        assert!(hit(120.0, None).is_none());
        // A thin block covers the middle half of the content; its connector the whole of it.
        assert_eq!(hit(120.0, Some((1, 0.5))).unwrap().id, Some(id(7)));
        assert_eq!(hit(120.0, Some((1, 0.1))).unwrap().id, Some(id(7)));
        assert_eq!(hit(200.0, Some((1, 0.9))).unwrap().id, Some(id(7)));
        // Another row is another row.
        assert!(hit(250.0, Some((2, 0.5))).is_none());
        // The legacy entry point is the same test with `row = index + content fraction`.
        let legacy = TrackScenePoint { position: 250.0, row: 1.5, lane_fraction: 0.5 };
        assert_eq!(
            hit_test_track_scene(&scene, legacy, 0.0),
            hit(250.0, Some((1, 0.5)))
        );
    }

    #[test]
    fn labels_are_placed_from_the_camera_and_deduplicated_across_tiles() {
        let spanning = feature(1, 900, 1_200, Some(0));
        let neighbour = feature(2, 1_210, 1_400, Some(0));
        let tiny = feature(3, 1_600, 1_601, Some(1));
        let scene_for = |index, features: Vec<TileFeature>| {
            lay_out_feature_tile(&tile(
                TrackTileKey { level: 0, index },
                TrackTilePayload::Features(FeatureTile { features, rows: FeatureRowAxis::Packed }),
            ))
            .unwrap()
        };
        let left = scene_for(0, vec![spanning.clone()]);
        let right = scene_for(1, vec![spanning, neighbour, tiny]);
        let measure = |text: &str| text.len() as f32 * 6.0;
        let view = TrackLabelView { start: 800.0, points_per_base: 1.0, width_points: 1_000.0 };
        let placed = place_track_labels([&left, &right], view, &measure, 100);
        // The spanning feature once, its neighbour, and not the one-point feature.
        assert_eq!(
            placed.iter().map(|label| (label.id, label.center)).collect::<Vec<_>>(),
            vec![(id(1), 250.0), (id(2), 505.0)]
        );
        // Tile order is irrelevant.
        assert_eq!(placed, place_track_labels([&right, &left], view, &measure, 100));
        // Zoomed out, the neighbour's label would collide and is dropped; no layout re-ran.
        let far = TrackLabelView { start: 800.0, points_per_base: 0.05, width_points: 1_000.0 };
        assert_eq!(place_track_labels([&left, &right], far, &measure, 100).len(), 1);
    }
}
