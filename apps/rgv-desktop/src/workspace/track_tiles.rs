//! Desktop side of the experimental track tile pipeline (ARCH-5).
//!
//! Opt-in and off by default: with `DesktopSettings::track_pipeline == Legacy` nothing in this
//! module runs and every auxiliary track is queried, laid out, drawn, hit-tested and inspected
//! by the legacy per-viewport path. With `Tiles`, [`track_uses_tile_pipeline`] decides per
//! track; a routed track is
//!
//! - registered in a lazily created [`TrackTileEngine`] through the reader the legacy
//!   [`rgv_engine::TrackEngine`] already has open (it stays registered there, so annotation
//!   search, next/previous feature and automation feature queries are still served by legacy);
//! - never queried by the legacy viewport query, and skipped by the legacy lane draw;
//! - drawn from cached world-space tiles by a [`TrackTileRenderer`] inside the lane rectangle
//!   the legacy frame model gives it, so gutter, heights, collapse and scroll are unchanged;
//! - hit-tested, hovered, selected and inspected from the tile scenes.
//!
//! Every other track keeps the legacy path. The decisions are pure functions so they can be
//! tested without a window.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use rgv_alignment_scene::AllocationLedger;
use rgv_core::{
    FeatureTrackSettings, GenomicSourceFormat, PanelId, TrackConfig, TrackId, TrackKind,
    TrackPipeline, TranscriptDisplay, Viewport, classify_genomic_source,
};
use rgv_engine::{
    EngineQueueError, LaidOutTrackTile, TrackTileEngine, TrackTileEngineConfig,
    TrackTileEngineEvent, TrackTileView, TrackTileViewSnapshot,
};
use rgv_hts::FeatureRecord;
use rgv_layout::{
    Rgba, TextPrimitive, TrackLabelView, TrackLayoutOptions, hit_test_track_scene_at,
    place_track_labels,
};
use rgv_model::{
    TrackRecord, TrackRecordId, TrackRecordLookup, TrackRendererId, TrackTileKey, TrackTilePayload,
    TrackTileSettings,
};
use rgv_render::{
    TrackBinStyle, TrackLaneDraw, TrackLanePage, TrackLaneProjection, TrackPalette, TrackTileFrame,
    TrackTilePageKey, TrackTileRenderer,
};

use crate::inspection::model::{
    FeatureInspectorSnapshot, FeatureSelection, GeneTranscriptsSummary,
};
use crate::sources::remote_range_config;
use crate::{Viewer, ui};

/// `FeatureSelection::record_index` of a selection that lives in the tile pipeline: the record
/// is identified by its [`TrackRecordId`] in [`TrackTilePipelineState`], not by an index into
/// a legacy snapshot.
pub(crate) const TRACK_TILE_SELECTION_RECORD_INDEX: usize = usize::MAX;
/// Most feature labels one lane draws; the legacy feature layout's cap (`MAX_FEATURE_LABELS`).
const MAXIMUM_TRACK_TILE_LABELS: usize = 512;
/// Rows below which the legacy feature layout draws no labels, and the band it reserves.
const LABEL_MINIMUM_ROW_PITCH: f32 = 16.0;
const LABEL_BAND_HEIGHT: f32 = 10.0;
const LABEL_CENTRE_OFFSET: f32 = 5.5;
const LABEL_FONT_SIZE: f32 = 9.0;

/// THE routing decision: whether a track is read and drawn by the tile pipeline.
///
/// `true` only under [`TrackPipeline::Tiles`] and only for plain feature tracks: BED, bigBed,
/// GFF3 and GTF annotations that are not a built-in landmark track, carry no explicit schema
/// (bigGenePred, bigNarrowPeak) and use the default feature display settings (for GFF3 and GTF,
/// with every transcript shown). Every other kind, format or setting keeps the legacy
/// pipeline.
///
/// The first visible gene-model track is drawn by the pinned-gene path, not as an auxiliary
/// lane; [`tile_pipeline_track_ids`] excludes it on top of this decision.
pub(crate) fn track_uses_tile_pipeline(
    setting: TrackPipeline,
    track_kind: TrackKind,
    track: &TrackConfig,
) -> bool {
    if setting != TrackPipeline::Tiles {
        return false;
    }
    // The file format is taken from the source name. A name that does not say which format it
    // is stays on the legacy path, which has its own fallback.
    let format =
        classify_genomic_source(&track.source.redacted_label()).map(|source| source.format);
    let plain = track.built_in.is_none() && track.format.is_none();
    match track_kind {
        TrackKind::Feature => {
            plain
                && track.feature == FeatureTrackSettings::default()
                && matches!(
                    format,
                    Some(GenomicSourceFormat::Bed | GenomicSourceFormat::BigBed)
                )
        }
        // The tile pipeline draws every transcript: a gene-model track is routed only when it
        // shows all transcripts, unfiltered. One per gene stays on the legacy path.
        TrackKind::GeneModel => {
            plain
                && track.feature
                    == FeatureTrackSettings {
                        transcripts: TranscriptDisplay::All,
                        ..FeatureTrackSettings::default()
                    }
                && matches!(
                    format,
                    Some(GenomicSourceFormat::Gff3 | GenomicSourceFormat::Gtf)
                )
        }
        // Not routed yet: each of these still needs its draw, scale, selection and inspector
        // reproduced before it can leave the legacy path.
        TrackKind::Alignment
        | TrackKind::Reference
        | TrackKind::Variant
        | TrackKind::Mutation
        | TrackKind::Gwas
        | TrackKind::Quantitative
        | TrackKind::CopyNumber
        | TrackKind::Interaction => false,
    }
}

/// The tracks the tile pipeline owns: every track [`track_uses_tile_pipeline`] accepts except
/// the pinned gene-model track, which the legacy pinned-gene path draws.
pub(crate) fn tile_pipeline_track_ids(
    setting: TrackPipeline,
    tracks: &[TrackConfig],
    pinned_gene: Option<TrackId>,
) -> BTreeSet<TrackId> {
    tracks
        .iter()
        .filter(|track| Some(track.id) != pinned_gene)
        .filter(|track| track_uses_tile_pipeline(setting, track.kind, track))
        .map(|track| track.id)
        .collect()
}

/// The tracks a legacy viewport query reads: all of `ids` except the routed ones, in order.
/// With nothing routed (the default) the list is returned untouched.
pub(crate) fn legacy_query_track_ids(
    ids: Vec<TrackId>,
    routed: &BTreeSet<TrackId>,
) -> Vec<TrackId> {
    if routed.is_empty() {
        return ids;
    }
    ids.into_iter().filter(|id| !routed.contains(id)).collect()
}

/// What one panel asks of one routed track: the panel's contig, exact visible interval and
/// resolution, with the track's tile settings.
pub(crate) fn track_tile_view(
    panel: PanelId,
    track: TrackId,
    contig: &str,
    viewport: Viewport,
    contig_length: u64,
    settings: TrackTileSettings,
) -> TrackTileView {
    let start = viewport.exact_start();
    TrackTileView {
        panel: panel.get(),
        track,
        contig: contig.to_owned(),
        start,
        end: start + viewport.span(),
        bases_per_point: viewport.bases_per_pixel(),
        contig_length: (contig_length > 0).then_some(contig_length),
        settings,
    }
}

/// Everything about a view that changes which tiles the engine serves. A pan inside the same
/// tiles produces an equal key, so the engine is told nothing; the camera itself is applied at
/// draw time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrackTileViewKey {
    pub(crate) contig: String,
    pub(crate) level: u8,
    pub(crate) first_tile: u64,
    pub(crate) last_tile: u64,
    pub(crate) contig_length: Option<u64>,
    pub(crate) settings_digest: u64,
}

pub(crate) fn track_tile_view_key(view: &TrackTileView) -> TrackTileViewKey {
    let level = TrackTileKey::level_for_bases_per_point(view.bases_per_point);
    let limit = view.contig_length.unwrap_or(u64::MAX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (start, end) = (
        (view.start.max(0.0).floor() as u64).min(limit),
        (view.end.max(0.0).ceil() as u64).min(limit),
    );
    let mut tiles = TrackTileKey::covering(level, start, end);
    let first_tile = tiles.next().map_or(0, |tile| tile.index);
    let last_tile = tiles.last().map_or(first_tile, |tile| tile.index);
    TrackTileViewKey {
        contig: view.contig.clone(),
        level,
        first_tile,
        last_tile,
        contig_length: view.contig_length,
        settings_digest: view.settings.digest(),
    }
}

/// What a routed lane shows in its gutter row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TrackTileLaneStatus {
    /// A visible tile is still being read, or its page is not on the GPU yet.
    Loading,
    /// Every visible tile is drawn. The view's row cap left out `hidden_rows` packed rows
    /// holding `hidden_features` distinct features ([`TrackTileViewSnapshot::overflow`]).
    Ready {
        hidden_rows: u32,
        hidden_features: u32,
    },
    /// The track could not be opened, a visible tile failed, or a page does not fit the GPU.
    Failed(String),
}

/// The status of a lane from what the engine and the renderer report for it.
pub(crate) fn track_tile_lane_status(
    open_failure: Option<&str>,
    gpu_failure: Option<&str>,
    gpu_page_missing: bool,
    snapshot: Option<&TrackTileViewSnapshot>,
) -> TrackTileLaneStatus {
    if let Some(message) = open_failure {
        return TrackTileLaneStatus::Failed(message.to_owned());
    }
    let Some(snapshot) = snapshot else {
        return TrackTileLaneStatus::Loading;
    };
    if let Some((_, message)) = snapshot.failed.first() {
        return TrackTileLaneStatus::Failed(message.clone());
    }
    if let Some(message) = gpu_failure {
        return TrackTileLaneStatus::Failed(message.to_owned());
    }
    if !snapshot.is_settled() || gpu_page_missing {
        return TrackTileLaneStatus::Loading;
    }
    let overflow = snapshot.overflow();
    TrackTileLaneStatus::Ready {
        hidden_rows: overflow.hidden_rows,
        hidden_features: overflow.hidden_features,
    }
}

/// Label and detail of the lane's gutter row; `None` when the lane is complete.
pub(crate) fn track_tile_lane_label(
    status: &TrackTileLaneStatus,
) -> Option<(String, Option<String>)> {
    match status {
        TrackTileLaneStatus::Loading => Some(("Loading…".to_owned(), None)),
        TrackTileLaneStatus::Ready {
            hidden_rows: 0,
            hidden_features: 0,
        } => None,
        TrackTileLaneStatus::Ready {
            hidden_rows,
            hidden_features,
        } => Some((
            "Rows hidden".to_owned(),
            Some(format!(
                "{hidden_rows} more row(s) holding {hidden_features} feature(s); enlarge the track"
            )),
        )),
        TrackTileLaneStatus::Failed(message) => {
            Some(("Unavailable".to_owned(), Some(message.clone())))
        }
    }
}

/// Where a routed lane is drawn and how its rows are measured. The rectangle is the legacy
/// frame row clipped to the annotation pane; the row metrics are the legacy feature layout's:
/// the frame height divided into [`TrackLayoutOptions::default`]`.maximum_rows` rows, a
/// ten-point label band above each row once rows are sixteen points tall, two points of
/// padding above and below the feature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackTileLaneGeometry {
    /// Visible lane rectangle in logical points: x, y, width, height.
    pub(crate) rect: [f32; 4],
    /// Top and height of the whole (unclipped) frame row.
    pub(crate) natural_top: f32,
    pub(crate) natural_height: f32,
    pub(crate) row_pitch: f32,
    pub(crate) row_height: f32,
    /// Top of row 0's content relative to `rect`'s top; negative when the lane's top is
    /// scrolled above the pane.
    pub(crate) row_offset: f32,
    /// Whether rows are tall enough to carry labels.
    pub(crate) labels: bool,
    /// Rows that fit the frame.
    pub(crate) visible_rows: u32,
}

pub(crate) fn track_tile_lane_geometry(
    x: f32,
    width: f32,
    natural_top: f32,
    natural_bottom: f32,
    clip_top: f32,
    clip_bottom: f32,
) -> Option<TrackTileLaneGeometry> {
    let natural_height = natural_bottom - natural_top;
    let top = natural_top.max(clip_top);
    let bottom = natural_bottom.min(clip_bottom);
    if !(width > 0.0 && natural_height > 0.0 && bottom > top && x.is_finite()) {
        return None;
    }
    let rows = TrackLayoutOptions::default().maximum_rows.max(1);
    #[allow(clippy::cast_precision_loss)]
    let row_pitch = natural_height / rows as f32;
    let labels = row_pitch >= LABEL_MINIMUM_ROW_PITCH;
    let label_band = if labels { LABEL_BAND_HEIGHT } else { 0.0 };
    Some(TrackTileLaneGeometry {
        rect: [x, top, width, bottom - top],
        natural_top,
        natural_height,
        row_pitch,
        row_height: (row_pitch - label_band - 4.0).max(2.0),
        row_offset: natural_top - top + label_band + 2.0,
        labels,
        visible_rows: u32::try_from(rows).unwrap_or(u32::MAX),
    })
}

impl TrackTileLaneGeometry {
    /// The value range that makes a value-mapped scene (a density summary) project into the
    /// clipped rectangle exactly as it would into the whole frame row.
    pub(crate) fn clipped_value_range(&self, range: (f32, f32)) -> (f32, f32) {
        let (low, high) = range;
        let span = high - low;
        let natural_bottom = self.natural_top + self.natural_height;
        let bottom = self.rect[1] + self.rect[3];
        (
            low + span * (natural_bottom - bottom) / self.natural_height,
            low + span * (natural_bottom - self.rect[1]) / self.natural_height,
        )
    }

    fn projection(
        &self,
        camera_start: f64,
        points_per_base: f64,
        value_range: (f32, f32),
    ) -> TrackLaneProjection {
        TrackLaneProjection {
            rect: self.rect,
            camera_start,
            points_per_base,
            row_pitch: self.row_pitch,
            row_height: self.row_height,
            row_offset: self.row_offset,
            value_range: self.clipped_value_range(value_range),
            bin_style: TrackBinStyle::Bars,
        }
    }
}

/// The routed lane's frame row in a panel's annotation pane layout, or `None` while the lane
/// is hidden, collapsed or scrolled out of view.
pub(crate) fn track_tile_lane_in_layout(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    x: f32,
    width: f32,
) -> Option<TrackTileLaneGeometry> {
    let item = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Auxiliary(track_id)
            && item.frame.channel == ui::TrackPane::Annotation
    })?;
    if !item.effective_content_visible || item.frame.collapsed || !item.frame.visible {
        return None;
    }
    track_tile_lane_geometry(
        x,
        width,
        item.frame.top - layout.scroll_points,
        item.frame.bottom - layout.scroll_points,
        item.clip_rect.top(),
        item.clip_rect.bottom(),
    )
}

/// The value range of a lane's density-summary scenes: the union of their ranges, always
/// including zero (bars grow from the zero baseline) and never empty.
pub(crate) fn track_tile_value_range<'tile>(
    tiles: impl IntoIterator<Item = &'tile LaidOutTrackTile>,
) -> (f32, f32) {
    let (mut low, mut high) = (0.0_f32, 0.0_f32);
    for tile in tiles {
        if let Some((minimum, maximum)) = tile.scene.value_range {
            if minimum.is_finite() {
                low = low.min(minimum);
            }
            if maximum.is_finite() {
                high = high.max(maximum);
            }
        }
    }
    if high - low <= f32::EPSILON {
        high = low + 1.0;
    }
    (low, high)
}

/// The tiles a lane draws, bottom-most first: the stand-in tiles of other levels that cover
/// only still-missing tiles of the view, then the view's own ready tiles. A stand-in that
/// overlaps a ready tile is not drawn, because its rows were packed at another level and
/// would show through the ready tile as a second, differently stacked copy.
pub(crate) fn track_tile_draw_order(
    snapshot: &TrackTileViewSnapshot,
) -> Vec<Arc<LaidOutTrackTile>> {
    let overlaps_ready = |candidate: &LaidOutTrackTile| {
        let (start, end) = (candidate.tile.key.start(), candidate.tile.key.end());
        snapshot
            .tiles
            .iter()
            .any(|ready| ready.tile.key.start() < end && ready.tile.key.end() > start)
    };
    snapshot
        .fallback
        .iter()
        .filter(|candidate| !overlaps_ready(candidate))
        .chain(snapshot.tiles.iter())
        .cloned()
        .collect()
}

/// GPU page identity of a tile. Two panels may show the same tile index of different contigs,
/// so the key carries the contig ([`TrackTilePageKey::for_contig`]).
fn track_tile_page_key(track: TrackId, tile: &LaidOutTrackTile) -> TrackTilePageKey {
    TrackTilePageKey::for_contig(
        track.get(),
        tile.contig(),
        tile.tile.key,
        tile.settings_digest,
    )
}

/// The feature a tile carries for `id`, as a stand-in record while the complete one is looked
/// up ([`TrackTileEngine::lookup_record`]). A tile carries the label, interval, strand and
/// score; it does not carry the feature type, the attribute list, the coding interval, peak
/// metrics or the complete block list (blocks are clipped to the tile).
pub(crate) fn track_tile_feature_record(
    tile: &LaidOutTrackTile,
    contig: &str,
    id: TrackRecordId,
) -> Option<FeatureRecord> {
    let TrackTilePayload::Features(features) = &tile.tile.payload else {
        return None;
    };
    let feature = features.features.iter().find(|feature| feature.id == id)?;
    Some(FeatureRecord {
        contig: contig.to_owned(),
        start: feature.start,
        end: feature.end,
        name: feature.name.as_deref().map(str::to_owned),
        feature_type: None,
        score: feature.score,
        strand: feature.strand,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    })
}

/// One routed lane of the current frame.
pub(crate) struct TrackTileLane {
    pub(crate) panel_id: PanelId,
    pub(crate) track_id: TrackId,
    pub(crate) contig: String,
    pub(crate) geometry: TrackTileLaneGeometry,
    pub(crate) camera_start: f64,
    pub(crate) points_per_base: f64,
    /// Bottom-most first; see [`track_tile_draw_order`].
    pub(crate) tiles: Vec<Arc<LaidOutTrackTile>>,
    pub(crate) value_range: (f32, f32),
    pub(crate) labels: Vec<TextPrimitive>,
}

impl TrackTileLane {
    fn projection(&self) -> TrackLaneProjection {
        self.geometry
            .projection(self.camera_start, self.points_per_base, self.value_range)
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, width, height] = self.geometry.rect;
        x >= left && x < left + width && y >= top && y < top + height
    }
}

/// A record under a point of a lane.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackTileHit {
    pub(crate) panel_id: PanelId,
    pub(crate) track_id: TrackId,
    pub(crate) id: TrackRecordId,
    pub(crate) record: FeatureRecord,
}

/// The feature under `(x, y)` (logical points) of `lane`. Only the tile whose columns contain
/// the point is tested, as only that tile is drawn there; the view's own tiles win over
/// stand-ins. A density summary has no records and is never hit.
pub(crate) fn track_tile_hit_in_lane(lane: &TrackTileLane, x: f32, y: f32) -> Option<TrackTileHit> {
    if !lane.contains(x, y) {
        return None;
    }
    let projection = lane.projection();
    // The probe measures the fraction down a row against the row's content height, the unit
    // scenes state extents in, and has no row in the gap between two rows.
    let point = projection.scene_probe(x, y);
    point.row?;
    let tolerance = projection.hit_tolerance_bases();
    lane.tiles.iter().rev().find_map(|tile| {
        if tile.scene.renderer != TrackRendererId::FEATURES {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let (start, end) = (tile.tile.key.start() as f64, tile.tile.key.end() as f64);
        if point.position < start || point.position >= end {
            return None;
        }
        let id = hit_test_track_scene_at(&tile.scene, point, tolerance)?.id?;
        Some(TrackTileHit {
            panel_id: lane.panel_id,
            track_id: lane.track_id,
            id,
            record: track_tile_feature_record(tile, &lane.contig, id)?,
        })
    })
}

/// A feature selected in a routed lane. It is valid while the panel's `selected_feature` is
/// the tile selection of the same track, so every legacy path that clears or replaces the
/// panel's feature selection also ends this one. The record is kept, so the inspector survives
/// tile refreshes, level changes and the feature leaving the view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackTileSelection {
    pub(crate) track_id: TrackId,
    pub(crate) id: TrackRecordId,
    /// Workspace contig the record was selected on.
    pub(crate) contig: String,
    pub(crate) record: FeatureRecord,
    /// Whether `record` is final: the complete source record, or the tile's stand-in after
    /// the lookup found nothing more. While `false` the engine is asked again each poll.
    pub(crate) complete: bool,
}

/// What to do with a selection given the outcome of its record lookup: the record to show
/// from now on, or `None` while the lookup is pending. A complete record keeps the
/// workspace's spelling of the contig, as the stand-in does; a lookup that found nothing or
/// failed leaves the stand-in in place.
pub(crate) fn resolve_track_tile_record(
    stand_in: &FeatureRecord,
    lookup: &TrackRecordLookup,
) -> Option<FeatureRecord> {
    match lookup {
        TrackRecordLookup::Pending => None,
        TrackRecordLookup::Ready(record) => match record.as_ref() {
            TrackRecord::Feature(record) => {
                let mut record = record.clone();
                record.contig.clone_from(&stand_in.contig);
                Some(record)
            }
            // A feature lane names no other kind of record; keep what the tile showed.
            _ => Some(stand_in.clone()),
        },
        TrackRecordLookup::NotFound | TrackRecordLookup::Failed(_) => Some(stand_in.clone()),
    }
}

/// Everything the desktop holds for the tile pipeline. Empty (and never touched) while the
/// setting is `Legacy`.
#[derive(Default)]
pub(crate) struct TrackTilePipelineState {
    engine: Option<TrackTileEngine>,
    renderer: Option<TrackTileRenderer>,
    /// Tracks registered in the tile engine and withheld from the legacy query and draw.
    routed: BTreeSet<TrackId>,
    open_failures: BTreeMap<TrackId, String>,
    /// The view last stated to the engine per `(panel, track)`.
    views: BTreeMap<(PanelId, TrackId), TrackTileViewKey>,
    /// The lane status the display was built from.
    presented: BTreeMap<(PanelId, TrackId), TrackTileLaneStatus>,
    selections: BTreeMap<PanelId, TrackTileSelection>,
    hover: Option<(PanelId, TrackId, TrackRecordId)>,
    lanes: Vec<TrackTileLane>,
    /// Pages the last frame could not admit, with the reason, and pages it could not draw.
    gpu_failures: BTreeMap<(PanelId, TrackId), String>,
    gpu_missing: BTreeSet<(PanelId, TrackId)>,
    reported_failures: BTreeSet<(TrackId, String)>,
    frame_prepared: bool,
}

impl TrackTilePipelineState {
    /// Whether `track` is drawn by the tile pipeline. Always `false` under `Legacy`.
    pub(crate) fn is_routed(&self, track: TrackId) -> bool {
        self.routed.contains(&track)
    }

    pub(crate) const fn routed(&self) -> &BTreeSet<TrackId> {
        &self.routed
    }

    fn is_idle(&self) -> bool {
        self.engine.is_none() && self.routed.is_empty() && self.renderer.is_none()
    }

    /// Feature labels of the current frame's lanes, for the genomic text path.
    pub(crate) fn label_texts(&self) -> impl Iterator<Item = &TextPrimitive> {
        self.lanes.iter().flat_map(|lane| lane.labels.iter())
    }

    /// Forgets everything derived from the legacy engine's readers. Called when that engine
    /// is replaced: the shared readers belonged to it.
    pub(crate) fn reset(&mut self) {
        let renderer = self.renderer.take().map(|mut renderer| {
            renderer.clear();
            renderer
        });
        *self = Self {
            renderer,
            ..Self::default()
        };
    }

    /// Drops the GPU renderer of a lost device; the next frame builds one for the new device.
    pub(crate) fn device_lost(&mut self) {
        self.renderer = None;
        self.frame_prepared = false;
    }

    pub(crate) fn set_palette(&mut self, queue: &wgpu::Queue, palette: &TrackPalette) {
        if let Some(renderer) = &mut self.renderer {
            renderer.set_palette(queue, palette);
        }
    }

    /// Draws the prepared frame, if this frame has one.
    pub(crate) fn render<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) -> bool {
        match &self.renderer {
            Some(renderer) if self.frame_prepared => {
                renderer.render(pass);
                true
            }
            _ => false,
        }
    }

    fn selection(
        &self,
        panel: PanelId,
        selected: Option<FeatureSelection>,
    ) -> Option<&TrackTileSelection> {
        let selected = selected?;
        self.selections.get(&panel).filter(|selection| {
            selected.record_index == TRACK_TILE_SELECTION_RECORD_INDEX
                && selected.track_id == selection.track_id
                && self.routed.contains(&selection.track_id)
        })
    }
}

impl Viewer {
    fn track_tile_palette(&self) -> TrackPalette {
        TrackPalette::for_theme(crate::theme::theme_id(self.session.settings.theme))
    }

    /// Hands the active theme's palette to the tile renderer (one 1 kB upload).
    pub(crate) fn apply_theme_to_track_tiles(&mut self) {
        let palette = self.track_tile_palette();
        self.track_tiles.set_palette(&self.queue, &palette);
    }

    /// Applies a change of `DesktopSettings::track_pipeline` at once.
    pub(crate) fn apply_track_pipeline_setting(&mut self) {
        self.synchronize_track_tiles();
        self.mark_display_dirty("track pipeline setting changed");
        self.window.request_redraw();
    }

    /// Reconciles the tile pipeline with the workspace: which tracks it owns, the engine's
    /// events, the view each panel states for each visible routed lane, and the lane status
    /// the display is built from. Runs every poll and before every frame; with the setting off
    /// and nothing routed it returns at once.
    pub(crate) fn synchronize_track_tiles(&mut self) {
        let setting = self.session.settings.track_pipeline;
        if setting == TrackPipeline::Legacy && self.track_tiles.is_idle() {
            return;
        }
        let desired = if self.session.track_engine.is_some() && !self.session.synthetic {
            let pinned = crate::display::primary_pinned_gene_id(
                &self.effective_auxiliary_presentation_configs(),
            );
            tile_pipeline_track_ids(setting, self.session.tracks.tracks(), pinned)
        } else {
            BTreeSet::new()
        };
        let removed = self
            .track_tiles
            .routed
            .difference(&desired)
            .copied()
            .collect::<Vec<_>>();
        let added = desired
            .difference(&self.track_tiles.routed)
            .copied()
            .collect::<Vec<_>>();
        let mut routing_changed = false;
        if !removed.is_empty() {
            self.unroute_track_tiles(&removed);
            routing_changed = true;
        }
        if !added.is_empty() {
            routing_changed |= self.route_track_tiles(added);
        }
        if routing_changed {
            // Legacy results computed for the old membership are stale, and tracks that came
            // back to legacy need a query; this is what a change of query membership does.
            self.invalidate_and_request_all_auxiliary_track_queries();
            self.mark_display_dirty("track tile routing changed");
        }
        if setting == TrackPipeline::Legacy && self.track_tiles.routed.is_empty() {
            // Switched off: release the helper threads and every GPU page.
            self.track_tiles = TrackTilePipelineState::default();
            self.window.request_redraw();
            return;
        }
        self.poll_track_tile_events();
        self.synchronize_track_tile_views();
        self.refresh_track_tile_selection_records();
    }

    /// Registers `ids` in the tile engine through the legacy engine's open readers and clears
    /// the legacy per-panel state of those tracks. Returns whether they were routed; a full
    /// legacy queue leaves them on the legacy path until the next poll.
    fn route_track_tiles(&mut self, ids: Vec<TrackId>) -> bool {
        let Some(legacy) = &self.session.track_engine else {
            return false;
        };
        let engine = self.track_tiles.engine.get_or_insert_with(|| {
            TrackTileEngine::spawn(TrackTileEngineConfig {
                remote: remote_range_config(),
                ..TrackTileEngineConfig::default()
            })
        });
        match legacy.try_share_tile_sources(ids.clone(), engine) {
            Ok(()) => {}
            Err(EngineQueueError::Full) => {
                self.window.request_redraw();
                return false;
            }
            Err(error @ EngineQueueError::Disconnected) => {
                if self
                    .track_tiles
                    .reported_failures
                    .insert((ids[0], error.to_string()))
                {
                    self.report_error(format!("Track tile pipeline: {error}"));
                }
                return false;
            }
        }
        let routed = ids.iter().copied().collect::<HashSet<_>>();
        for panel in self.session.panels.iter_mut() {
            panel
                .track_snapshots
                .retain(|snapshot| !routed.contains(&snapshot.id));
            panel
                .track_snapshot_intervals
                .retain(|track_id, _| !routed.contains(track_id));
            panel
                .track_snapshot_generations
                .retain(|track_id, _| !routed.contains(track_id));
            panel
                .track_query_error_generations
                .retain(|track_id, _| !routed.contains(track_id));
            panel.context_scene.remove_auxiliary(&routed);
            // An index into the snapshot that has just been dropped names nothing any more.
            panel.selected_feature = panel
                .selected_feature
                .filter(|selection| !routed.contains(&selection.track_id));
        }
        self.track_tiles.routed.extend(ids);
        true
    }

    /// Hands `ids` back to the legacy path.
    fn unroute_track_tiles(&mut self, ids: &[TrackId]) {
        let state = &mut self.track_tiles;
        if let Some(engine) = &state.engine {
            engine.remove_tracks(ids);
        }
        for id in ids {
            state.routed.remove(id);
            state.open_failures.remove(id);
            if let Some(renderer) = &mut state.renderer {
                renderer.retire_track(id.get());
            }
        }
        state.views.retain(|(_, track), _| !ids.contains(track));
        state.presented.retain(|(_, track), _| !ids.contains(track));
        state
            .gpu_failures
            .retain(|(_, track), _| !ids.contains(track));
        state.gpu_missing.retain(|(_, track)| !ids.contains(track));
        state
            .reported_failures
            .retain(|(track, _)| !ids.contains(track));
        state.lanes.retain(|lane| !ids.contains(&lane.track_id));
        state.frame_prepared = false;
        if state
            .hover
            .is_some_and(|(_, track, _)| ids.contains(&track))
        {
            state.hover = None;
        }
        state
            .selections
            .retain(|_, selection| !ids.contains(&selection.track_id));
        for panel in self.session.panels.iter_mut() {
            panel.selected_feature = panel.selected_feature.filter(|selection| {
                !(selection.record_index == TRACK_TILE_SELECTION_RECORD_INDEX
                    && ids.contains(&selection.track_id))
            });
        }
    }

    /// Logs a tile pipeline failure of `track` once per distinct message.
    fn report_track_tile_failure(&mut self, track: TrackId, message: &str) {
        if self
            .track_tiles
            .reported_failures
            .insert((track, message.to_owned()))
        {
            tracing::warn!(track_id = track.get(), error = %message, "track tile pipeline");
            self.log_track_error(Some(track), message);
        }
    }

    /// Drains the tile engine's events: tiles that became ready need a redraw, failures go to
    /// the notification log with their track.
    fn poll_track_tile_events(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = None;
        if let Some(engine) = &self.track_tiles.engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        disconnected = Some(error.to_string());
                        break;
                    }
                }
            }
        }
        if let Some(message) = disconnected
            && let Some(track) = self.track_tiles.routed.first().copied()
        {
            self.report_track_tile_failure(track, &format!("Track tile engine: {message}"));
        }
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                TrackTileEngineEvent::TrackOpenFailed { track, message } => {
                    if self.track_tiles.routed.contains(&track) {
                        self.track_tiles
                            .open_failures
                            .insert(track, message.clone());
                        self.report_track_tile_failure(track, &message);
                    }
                }
                TrackTileEngineEvent::TileFailed { track, message, .. } => {
                    if self.track_tiles.routed.contains(&track) {
                        self.report_track_tile_failure(track, &message);
                    }
                }
                TrackTileEngineEvent::TrackOpened { track } => {
                    self.track_tiles.open_failures.remove(&track);
                }
                // The lane status reconciliation below reads the views themselves.
                TrackTileEngineEvent::TileReady { .. }
                | TrackTileEngineEvent::ViewSettled { .. }
                // Search and feature stepping are served by the legacy engine, where routed
                // tracks stay registered; the tile engine is never asked.
                | TrackTileEngineEvent::SearchCompleted { .. }
                | TrackTileEngineEvent::AdjacentFeatureCompleted { .. } => {}
            }
        }
        // Tiles are drawn from the engine's cache every frame: a redraw shows them.
        self.window.request_redraw();
    }

    /// States each panel's camera for each routed lane that is on screen, drops the views of
    /// lanes that are not (closed panels, removed, hidden, collapsed or scrolled-out tracks),
    /// and marks the display dirty when a lane's status changes.
    fn synchronize_track_tile_views(&mut self) {
        let mut wanted = BTreeMap::new();
        for panel in self.session.panels.iter() {
            let Some(column) = self.panel_column_rect(panel.id) else {
                continue;
            };
            for track in &self.track_tiles.routed {
                let Some(geometry) = track_tile_lane_in_layout(
                    &panel.pane_layout.annotation,
                    *track,
                    column.left(),
                    column.width(),
                ) else {
                    continue;
                };
                // The engine caps each scene at the rows the lane shows, so the view's
                // overflow is exactly what the lane cannot show.
                wanted.insert(
                    (panel.id, *track),
                    track_tile_view(
                        panel.id,
                        *track,
                        &panel.contig_name,
                        panel.viewport,
                        panel.contig_length,
                        TrackTileSettings::default()
                            .with_maximum_feature_rows(geometry.visible_rows),
                    ),
                );
            }
        }
        let state = &mut self.track_tiles;
        let Some(engine) = &state.engine else {
            return;
        };
        state.views.retain(|key, _| {
            let keep = wanted.contains_key(key);
            if !keep {
                engine.drop_view(key.0.get(), key.1);
            }
            keep
        });
        let mut changed = false;
        for (key, view) in wanted {
            let view_key = track_tile_view_key(&view);
            if state.views.get(&key) != Some(&view_key) && engine.request_view(view).is_some() {
                state.views.insert(key, view_key);
            }
            let snapshot = state
                .views
                .contains_key(&key)
                .then(|| engine.view(key.0.get(), key.1))
                .flatten();
            let status = track_tile_lane_status(
                state.open_failures.get(&key.1).map(String::as_str),
                state.gpu_failures.get(&key).map(String::as_str),
                state.gpu_missing.contains(&key),
                snapshot.as_ref(),
            );
            if state.presented.get(&key) != Some(&status) {
                state.presented.insert(key, status);
                changed = true;
            }
        }
        let before = state.presented.len();
        let views = &state.views;
        state.presented.retain(|key, _| views.contains_key(key));
        changed |= state.presented.len() != before;
        if changed {
            self.mark_display_dirty("track tile lane state changed");
            self.window.request_redraw();
        }
    }

    /// The gutter row a routed lane shows while it is loading, failed or has hidden rows.
    pub(crate) fn track_tile_lane_subtrack(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        top: f32,
        bottom: f32,
    ) -> Option<ui::TrackSubtrack> {
        let status = self.track_tiles.presented.get(&(panel_id, track_id))?;
        let (label, detail) = track_tile_lane_label(status)?;
        Some(ui::TrackSubtrack {
            alignment_subtrack: None,
            label,
            detail,
            top: top + 19.0,
            bottom: (top + 39.0).min(bottom),
            collapsed: false,
            composite_member: None,
        })
    }

    /// Captures this frame's routed lanes from the final pane layouts and the engine's cache:
    /// rectangle, camera, tiles to draw and feature labels. Hit-testing reads the same lanes.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn plan_track_tile_lanes(&mut self) {
        self.track_tiles.lanes.clear();
        if self.track_tiles.routed.is_empty() {
            return;
        }
        let Some(engine) = &self.track_tiles.engine else {
            return;
        };
        // Label widths come from egui's fonts, which exist once the first frame has run.
        let fonts_ready = self.egui_context.cumulative_frame_nr() > 0;
        let measure = |text: &str| {
            self.egui_context.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::monospace(LABEL_FONT_SIZE),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        };
        let mut lanes = Vec::new();
        for panel in self.session.panels.iter() {
            let Some(column) = self.panel_column_rect(panel.id) else {
                continue;
            };
            let bases_per_point = panel.viewport.bases_per_pixel();
            if !(bases_per_point.is_finite() && bases_per_point > 0.0) {
                continue;
            }
            for track_id in &self.track_tiles.routed {
                let key = (panel.id, *track_id);
                // A view stated for another contig (the poll before a jump) is not this
                // panel's data.
                if self
                    .track_tiles
                    .views
                    .get(&key)
                    .is_none_or(|view| view.contig != panel.contig_name)
                {
                    continue;
                }
                let Some(geometry) = track_tile_lane_in_layout(
                    &panel.pane_layout.annotation,
                    *track_id,
                    column.left(),
                    column.width(),
                ) else {
                    continue;
                };
                let Some(snapshot) = engine.view(panel.id.get(), *track_id) else {
                    continue;
                };
                let tiles = track_tile_draw_order(&snapshot);
                let camera_start = panel.viewport.exact_start();
                let points_per_base = 1.0 / bases_per_point;
                let mut labels = Vec::new();
                if geometry.labels && fonts_ready {
                    let placed = place_track_labels(
                        snapshot
                            .tiles
                            .iter()
                            .filter(|tile| tile.scene.renderer == TrackRendererId::FEATURES)
                            .map(|tile| &tile.scene),
                        TrackLabelView {
                            start: camera_start,
                            points_per_base,
                            width_points: f64::from(geometry.rect[2]),
                        },
                        &measure,
                        MAXIMUM_TRACK_TILE_LABELS,
                    );
                    let (clip_top, clip_bottom) =
                        (geometry.rect[1], geometry.rect[1] + geometry.rect[3]);
                    #[allow(clippy::cast_precision_loss)]
                    labels.extend(placed.into_iter().filter_map(|label| {
                        let y = geometry.natural_top
                            + label.row as f32 * geometry.row_pitch
                            + LABEL_CENTRE_OFFSET;
                        (y - LABEL_CENTRE_OFFSET >= clip_top
                            && y + LABEL_CENTRE_OFFSET <= clip_bottom
                            && label.row < geometry.visible_rows)
                            .then(|| TextPrimitive {
                                x: geometry.rect[0] + label.center,
                                y,
                                text: label.text.to_string(),
                                font_size: LABEL_FONT_SIZE,
                                color: Rgba::new(0.16, 0.18, 0.22, 0.96),
                            })
                    }));
                }
                lanes.push(TrackTileLane {
                    panel_id: panel.id,
                    track_id: *track_id,
                    contig: panel.contig_name.clone(),
                    geometry,
                    camera_start,
                    points_per_base,
                    value_range: track_tile_value_range(tiles.iter().map(Arc::as_ref)),
                    tiles,
                    labels,
                });
            }
        }
        self.track_tiles.lanes = lanes;
    }

    /// Builds this frame's tile draw: reconciles the views with the final camera, admits the
    /// pages of every visible tile and prepares the lane uniforms. Called once per submitted
    /// frame after the pane layout is final, like the coverage pyramid.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn prepare_track_tile_frame(&mut self, scale_factor: f32) {
        self.track_tiles.frame_prepared = false;
        if self.track_tiles.is_idle() {
            return;
        }
        self.synchronize_track_tiles();
        self.plan_track_tile_lanes();
        if self.track_tiles.lanes.is_empty() {
            return;
        }
        let palette = self.track_tile_palette();
        let budget = self.context_lane_gpu_budget_bytes.max(1);
        let state = &mut self.track_tiles;
        let renderer = state.renderer.get_or_insert_with(|| {
            // A private strict ledger, like the alignment GPU caches under the bounded policy:
            // tile pages evict their own least-recently-used pages and can neither refuse
            // alignment pages nor be refused by them.
            let mut renderer = TrackTileRenderer::new(
                &self.device,
                self.surface_config.format,
                AllocationLedger::new_with_transient_budget(budget, budget, budget),
            );
            renderer.set_palette(&self.queue, &palette);
            renderer
        });
        let selected_feature = |panel_id: PanelId| {
            self.session
                .panels
                .get(panel_id)
                .and_then(|panel| panel.selected_feature)
        };
        let protected = state
            .lanes
            .iter()
            .flat_map(|lane| {
                lane.tiles
                    .iter()
                    .map(|tile| track_tile_page_key(lane.track_id, tile))
            })
            .collect::<Vec<_>>();
        let mut gpu_failures = BTreeMap::new();
        let mut frame_lanes = Vec::new();
        for lane in &state.lanes {
            let selected = state
                .selections
                .get(&lane.panel_id)
                .filter(|selection| {
                    selection.track_id == lane.track_id
                        && selected_feature(lane.panel_id).is_some_and(|selected| {
                            selected.track_id == lane.track_id
                                && selected.record_index == TRACK_TILE_SELECTION_RECORD_INDEX
                        })
                })
                .map(|selection| selection.id);
            let hovered = state
                .hover
                .filter(|(panel, track, _)| *panel == lane.panel_id && *track == lane.track_id)
                .map(|(_, _, id)| id);
            let mut pages = Vec::with_capacity(lane.tiles.len());
            for tile in &lane.tiles {
                let key = track_tile_page_key(lane.track_id, tile);
                match renderer.admit_page(&self.device, key, &tile.scene, &protected) {
                    Ok(_) => pages.push(TrackLanePage {
                        key,
                        selected: selected.and_then(|id| tile.scene.record_index_of(id)),
                        hovered: hovered.and_then(|id| tile.scene.record_index_of(id)),
                    }),
                    Err(error) => {
                        gpu_failures
                            .entry((lane.panel_id, lane.track_id))
                            .or_insert_with(|| error.to_string());
                    }
                }
            }
            frame_lanes.push((
                (lane.panel_id, lane.track_id),
                TrackLaneDraw {
                    projection: lane.projection(),
                    pages,
                },
            ));
        }
        let scale = f64::from(scale_factor.max(f32::EPSILON));
        #[allow(clippy::cast_possible_truncation)]
        let frame = TrackTileFrame {
            screen_size: [
                (f64::from(self.surface_config.width) / scale) as f32,
                (f64::from(self.surface_config.height) / scale) as f32,
            ],
            scale_factor: scale,
            target_size_pixels: [self.surface_config.width, self.surface_config.height],
            lanes: frame_lanes.iter().map(|(_, lane)| lane.clone()).collect(),
        };
        let report = renderer.prepare(&self.device, &self.queue, &frame);
        let gpu_missing = frame_lanes
            .iter()
            .filter(|(_, lane)| {
                lane.pages
                    .iter()
                    .any(|page| report.missing_pages.contains(&page.key))
            })
            .map(|(key, _)| *key)
            .collect::<BTreeSet<_>>();
        state.frame_prepared = report.draws > 0;
        let gpu_state_changed =
            state.gpu_failures != gpu_failures || state.gpu_missing != gpu_missing;
        state.gpu_failures = gpu_failures;
        state.gpu_missing = gpu_missing;
        if gpu_state_changed {
            let failures = self
                .track_tiles
                .gpu_failures
                .iter()
                .map(|((_, track), message)| (*track, message.clone()))
                .collect::<Vec<_>>();
            for (track, message) in failures {
                self.report_track_tile_failure(track, &message);
            }
            // The next poll turns the new GPU state into the lane's status row.
            self.window.request_redraw();
        }
    }

    /// The feature of a routed lane under the cursor in the active panel.
    pub(crate) fn track_tile_feature_at_cursor(&self) -> Option<TrackTileHit> {
        if self.track_tiles.lanes.is_empty() {
            return None;
        }
        let scale = self.window.scale_factor();
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = (
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        let active = self.session.panels.active().id;
        self.track_tiles
            .lanes
            .iter()
            .filter(|lane| lane.panel_id == active)
            .find_map(|lane| track_tile_hit_in_lane(lane, x, y))
    }

    /// Follows the cursor with the hover highlight of routed lanes. Hover is frame state: it
    /// changes two words of a lane uniform, so it needs a redraw and nothing else.
    pub(crate) fn update_track_tile_hover(&mut self, hover_allowed: bool) {
        if self.track_tiles.is_idle() {
            return;
        }
        let hover = hover_allowed
            .then(|| self.track_tile_feature_at_cursor())
            .flatten()
            .map(|hit| (hit.panel_id, hit.track_id, hit.id));
        if self.track_tiles.hover != hover {
            self.track_tiles.hover = hover;
            self.window.request_redraw();
        }
    }

    /// Selects the routed feature under the cursor, if there is one. The panel's
    /// `selected_feature` becomes the tile selection of that track.
    pub(crate) fn select_track_tile_feature_at_cursor(&mut self) -> bool {
        let Some(hit) = self.track_tile_feature_at_cursor() else {
            return false;
        };
        self.session.panels.active_mut().selected_feature = Some(FeatureSelection {
            track_id: hit.track_id,
            record_index: TRACK_TILE_SELECTION_RECORD_INDEX,
        });
        let contig = self.session.panels.get(hit.panel_id).map_or_else(
            || hit.record.contig.clone(),
            |panel| panel.contig_name.clone(),
        );
        self.track_tiles.selections.insert(
            hit.panel_id,
            TrackTileSelection {
                track_id: hit.track_id,
                id: hit.id,
                contig,
                record: hit.record,
                complete: false,
            },
        );
        self.refresh_track_tile_selection_records();
        true
    }

    /// Asks the tile engine for the complete record of every selection still showing the
    /// tile's stand-in. Only selections their panel currently holds are asked; the others are
    /// kept as they are (a selection is valid only while its panel holds it, as before) and
    /// their lookup is abandoned unless a held selection of the same track still wants one. A
    /// track runs one lookup at a time, so only one selection per track is asked per poll; the
    /// others wait their turn rather than cancelling each other.
    pub(crate) fn refresh_track_tile_selection_records(&mut self) {
        let state = &mut self.track_tiles;
        if state.selections.is_empty() {
            return;
        }
        let Some(engine) = &state.engine else {
            return;
        };
        let panels = &self.session.panels;
        let routed = &state.routed;
        let held = |panel_id: PanelId, selection: &TrackTileSelection| {
            panels
                .get(panel_id)
                .and_then(|panel| panel.selected_feature)
                .is_some_and(|held| {
                    held.record_index == TRACK_TILE_SELECTION_RECORD_INDEX
                        && held.track_id == selection.track_id
                })
                && routed.contains(&selection.track_id)
        };
        let wanted = state
            .selections
            .iter()
            .filter(|(panel_id, selection)| !selection.complete && held(**panel_id, selection))
            .map(|(_, selection)| selection.track_id)
            .collect::<BTreeSet<_>>();
        let abandoned = state
            .selections
            .iter()
            .filter(|(panel_id, selection)| !selection.complete && !held(**panel_id, selection))
            .map(|(_, selection)| selection.track_id)
            .filter(|track| !wanted.contains(track))
            .collect::<BTreeSet<_>>();
        for track in abandoned {
            engine.cancel_record_lookup(track);
        }
        let mut asked = BTreeSet::new();
        let mut resolved = false;
        for (panel_id, selection) in &mut state.selections {
            if selection.complete
                || !wanted.contains(&selection.track_id)
                || !held(*panel_id, selection)
                || !asked.insert(selection.track_id)
            {
                continue;
            }
            let lookup = engine.lookup_record(selection.track_id, &selection.contig, selection.id);
            if let TrackRecordLookup::Failed(message) = &lookup {
                tracing::debug!(
                    track_id = selection.track_id.get(),
                    error = %message,
                    "track tile record lookup failed; the inspector shows the tile's fields"
                );
            }
            if let Some(record) = resolve_track_tile_record(&selection.record, &lookup) {
                selection.record = record;
                selection.complete = true;
                resolved = true;
            }
        }
        if resolved {
            self.window.request_redraw();
        }
    }

    /// The active panel's tile selection with its complete record, waiting up to `timeout`
    /// for the lookup: for automation, which cannot wait for a later poll. The stored
    /// selection is not changed; the next poll collects the same outcome.
    pub(crate) fn track_tile_feature_inspector_snapshot_blocking(
        &self,
        timeout: std::time::Duration,
    ) -> Option<FeatureInspectorSnapshot> {
        let mut snapshot = self.track_tile_feature_inspector_snapshot()?;
        let panel = self.session.panels.active();
        let selection = self
            .track_tiles
            .selection(panel.id, panel.selected_feature)?;
        if !selection.complete
            && let Some(engine) = &self.track_tiles.engine
        {
            let lookup = engine.lookup_record_blocking(
                selection.track_id,
                &selection.contig,
                selection.id,
                timeout,
            );
            if let Some(record) = resolve_track_tile_record(&selection.record, &lookup) {
                snapshot.record = record;
            }
        }
        Some(snapshot)
    }

    /// The inspector snapshot of the active panel's tile selection.
    pub(crate) fn track_tile_feature_inspector_snapshot(&self) -> Option<FeatureInspectorSnapshot> {
        let panel = self.session.panels.active();
        let selection = self
            .track_tiles
            .selection(panel.id, panel.selected_feature)?;
        let track_name = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == selection.track_id)?
            .name
            .clone();
        Some(FeatureInspectorSnapshot {
            track_name,
            record: selection.record.clone(),
            gene_transcripts: GeneTranscriptsSummary::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use rgv_core::{
        BuiltInTrackIdentity, FeatureDisplayMode, GenomicInterval, TrackFormat, TrackSource,
    };
    use rgv_layout::lay_out_feature_tile;
    use rgv_model::{
        FeatureRowAxis, FeatureTile, Strand, TileFeature, TrackTile, TrackTileLod,
        TrackTileProvenance,
    };

    use super::*;

    fn track(id: u64, kind: TrackKind, path: &str) -> TrackConfig {
        let mut config = TrackConfig::new(
            TrackId::new(id),
            format!("track {id}"),
            kind,
            TrackSource::Local {
                path: path.to_owned(),
            },
        );
        // Gene-model tracks are routed only while they show every transcript.
        if kind == TrackKind::GeneModel {
            config.feature.transcripts = TranscriptDisplay::All;
        }
        config
    }

    fn routed(kind: TrackKind, path: &str) -> bool {
        track_uses_tile_pipeline(TrackPipeline::Tiles, kind, &track(1, kind, path))
    }

    #[test]
    fn nothing_is_routed_while_the_setting_is_legacy() {
        for (kind, path) in [
            (TrackKind::Feature, "peaks.bed"),
            (TrackKind::Feature, "peaks.bb"),
            (TrackKind::GeneModel, "genes.gtf"),
            (TrackKind::GeneModel, "genes.gff3"),
            (TrackKind::Quantitative, "signal.bw"),
            (TrackKind::Variant, "calls.vcf.gz"),
        ] {
            assert!(!track_uses_tile_pipeline(
                TrackPipeline::Legacy,
                kind,
                &track(1, kind, path)
            ));
        }
        assert_eq!(TrackPipeline::default(), TrackPipeline::Legacy);
        assert!(
            tile_pipeline_track_ids(
                TrackPipeline::Legacy,
                &[track(1, TrackKind::Feature, "a.bed")],
                None
            )
            .is_empty()
        );
    }

    #[test]
    fn the_routing_table_covers_plain_feature_tracks_only() {
        // Routed: plain BED, bigBed, GFF3 and GTF annotations.
        assert!(routed(TrackKind::Feature, "peaks.bed"));
        assert!(routed(TrackKind::Feature, "peaks.bed.gz"));
        assert!(routed(TrackKind::Feature, "peaks.bb"));
        assert!(routed(TrackKind::Feature, "peaks.bigBed"));
        assert!(routed(TrackKind::GeneModel, "genes.gff3"));
        assert!(routed(TrackKind::GeneModel, "genes.gff3.gz"));
        assert!(routed(TrackKind::GeneModel, "genes.gtf"));
        assert!(routed(TrackKind::GeneModel, "genes.gtf.gz"));
        // Feature formats with their own schema or inspector content stay legacy.
        assert!(!routed(TrackKind::Feature, "peaks.narrowPeak"));
        assert!(!routed(TrackKind::Feature, "peaks.broadPeak"));
        assert!(!routed(TrackKind::Feature, "alignments.psl"));
        assert!(!routed(TrackKind::GeneModel, "genes.genePred"));
        assert!(!routed(TrackKind::GeneModel, "genes.refflat"));
        // A name that does not state its format stays legacy.
        assert!(!routed(TrackKind::Feature, "regions.txt"));
        assert!(!routed(TrackKind::GeneModel, "annotation.txt"));
        assert!(!routed(TrackKind::GeneModel, "genes.bb"));
        // Every other track kind stays legacy.
        for (kind, path) in [
            (TrackKind::Quantitative, "signal.bw"),
            (TrackKind::Quantitative, "signal.bedGraph"),
            (TrackKind::Quantitative, "signal.wig"),
            (TrackKind::Quantitative, "signal.tdf"),
            (TrackKind::Variant, "calls.vcf.gz"),
            (TrackKind::Variant, "calls.bcf"),
            (TrackKind::Interaction, "loops.bedpe"),
            (TrackKind::Interaction, "loops.interact"),
            (TrackKind::CopyNumber, "segments.seg"),
            (TrackKind::Mutation, "somatic.maf"),
            (TrackKind::Gwas, "study.gwas"),
            (TrackKind::Alignment, "reads.bam"),
            (TrackKind::Reference, "genome.fa"),
        ] {
            assert!(!routed(kind, path), "{kind:?} {path} must stay legacy");
        }
        // The kind passed in decides, not the file name alone.
        assert!(!track_uses_tile_pipeline(
            TrackPipeline::Tiles,
            TrackKind::Quantitative,
            &track(1, TrackKind::Feature, "peaks.bed")
        ));
    }

    #[test]
    fn non_default_settings_explicit_schemas_and_built_in_tracks_stay_legacy() {
        let plain = track(1, TrackKind::Feature, "peaks.bed");
        assert!(track_uses_tile_pipeline(
            TrackPipeline::Tiles,
            plain.kind,
            &plain
        ));
        for mode in [FeatureDisplayMode::Collapsed, FeatureDisplayMode::Squished] {
            let mut changed = plain.clone();
            changed.feature.display_mode = mode;
            assert!(!track_uses_tile_pipeline(
                TrackPipeline::Tiles,
                changed.kind,
                &changed
            ));
        }
        for format in [TrackFormat::BigNarrowPeak, TrackFormat::BigGenePred] {
            let mut explicit = track(1, TrackKind::Feature, "peaks.bb");
            explicit.format = Some(format);
            assert!(!track_uses_tile_pipeline(
                TrackPipeline::Tiles,
                explicit.kind,
                &explicit
            ));
        }
        // A gene-model track showing one transcript per gene (the default), filtered to
        // protein-coding genes or with an expanded gene is drawn by the legacy path.
        let genes = track(1, TrackKind::GeneModel, "genes.gtf");
        assert!(track_uses_tile_pipeline(
            TrackPipeline::Tiles,
            genes.kind,
            &genes
        ));
        let mut one_per_gene = genes.clone();
        one_per_gene.feature.transcripts = TranscriptDisplay::OnePerGene;
        let mut coding = genes.clone();
        coding.feature.protein_coding_only = true;
        let mut expanded = genes;
        expanded.feature.expanded_genes.insert("G1".to_owned());
        for changed in [one_per_gene, coding, expanded] {
            assert!(!track_uses_tile_pipeline(
                TrackPipeline::Tiles,
                changed.kind,
                &changed
            ));
        }
        let mut built_in = track(1, TrackKind::GeneModel, "genes.gff3");
        built_in.built_in = Some(BuiltInTrackIdentity {
            bundle_id: "bundle".to_owned(),
            bundle_version: "1".to_owned(),
            asset_role: rgv_core::LandmarkAssetRole::RepresentativeGenes,
            sha256: String::new(),
        });
        assert!(!track_uses_tile_pipeline(
            TrackPipeline::Tiles,
            built_in.kind,
            &built_in
        ));
        // Presentation state (height, visibility, collapse) does not change routing.
        let mut resized = plain;
        resized.height = 300.0;
        resized.collapsed = true;
        resized.visible = false;
        assert!(track_uses_tile_pipeline(
            TrackPipeline::Tiles,
            resized.kind,
            &resized
        ));
    }

    #[test]
    fn the_pinned_gene_track_is_never_routed() {
        let tracks = [
            track(1, TrackKind::GeneModel, "genes.gtf"),
            track(2, TrackKind::GeneModel, "other.gtf"),
            track(3, TrackKind::Feature, "peaks.bed"),
            track(4, TrackKind::Quantitative, "signal.bw"),
        ];
        assert_eq!(
            tile_pipeline_track_ids(TrackPipeline::Tiles, &tracks, Some(TrackId::new(1))),
            BTreeSet::from([TrackId::new(2), TrackId::new(3)])
        );
        assert_eq!(
            tile_pipeline_track_ids(TrackPipeline::Tiles, &tracks, None),
            BTreeSet::from([TrackId::new(1), TrackId::new(2), TrackId::new(3)])
        );
    }

    #[test]
    fn a_routed_track_is_never_in_the_legacy_query() {
        let ids = vec![
            TrackId::new(1),
            TrackId::new(2),
            TrackId::new(3),
            TrackId::new(4),
        ];
        let routed = BTreeSet::from([TrackId::new(2), TrackId::new(3)]);
        let legacy = legacy_query_track_ids(ids.clone(), &routed);
        assert_eq!(legacy, vec![TrackId::new(1), TrackId::new(4)]);
        assert!(legacy.iter().all(|id| !routed.contains(id)));
        // Nothing routed (the default): the legacy query is exactly what it was.
        assert_eq!(legacy_query_track_ids(ids.clone(), &BTreeSet::new()), ids);
        // Everything routed: the legacy query reads nothing.
        assert!(legacy_query_track_ids(ids.clone(), &ids.iter().copied().collect()).is_empty());
    }

    fn viewport(start: u64, end: u64, width: f64) -> Viewport {
        Viewport::new(
            GenomicInterval::new(rgv_core::ContigId::new(0), start, end).unwrap(),
            width,
        )
        .unwrap()
    }

    #[test]
    fn a_view_request_states_the_panels_contig_interval_and_resolution() {
        let view = track_tile_view(
            PanelId::new(7),
            TrackId::new(3),
            "chr2",
            viewport(10_000, 12_000, 1_000.0),
            50_000,
            TrackTileSettings::default(),
        );
        assert_eq!(view.panel, 7);
        assert_eq!(view.track, TrackId::new(3));
        assert_eq!(view.contig, "chr2");
        assert_eq!((view.start, view.end), (10_000.0, 12_000.0));
        assert_eq!(view.bases_per_point, 2.0);
        assert_eq!(view.contig_length, Some(50_000));
        assert_eq!(view.settings, TrackTileSettings::default());
        // An unknown contig length does not bound the request.
        let unbounded = track_tile_view(
            PanelId::new(7),
            TrackId::new(3),
            "chr2",
            viewport(0, 100, 100.0),
            0,
            TrackTileSettings::default(),
        );
        assert_eq!(unbounded.contig_length, None);
    }

    #[test]
    fn the_view_key_changes_only_when_the_requested_tiles_change() {
        let view = |start: u64, end: u64, contig: &str| {
            track_tile_view(
                PanelId::new(1),
                TrackId::new(1),
                contig,
                viewport(start, end, 1_000.0),
                1_000_000,
                TrackTileSettings::default(),
            )
        };
        let base = track_tile_view_key(&view(10_000, 11_000, "chr1"));
        assert_eq!(base.contig, "chr1");
        assert_eq!(base.level, TrackTileKey::level_for_bases_per_point(1.0));
        assert_eq!(base.settings_digest, TrackTileSettings::default().digest());
        let span = TrackTileKey::span_at(base.level);
        assert_eq!(base.first_tile, 10_000 / span);
        assert_eq!(base.last_tile, 10_999 / span);
        // A pan inside the same tiles asks for nothing new.
        let nudge = track_tile_view_key(&view(10_010, 11_010, "chr1"));
        assert_eq!(
            (nudge.first_tile, nudge.last_tile),
            (10_010 / span, 11_009 / span)
        );
        if (nudge.first_tile, nudge.last_tile) == (base.first_tile, base.last_tile) {
            assert_eq!(nudge, base);
        }
        // Another tile, another level, another contig or other settings: a new request.
        assert_ne!(track_tile_view_key(&view(500_000, 501_000, "chr1")), base);
        assert_ne!(track_tile_view_key(&view(10_000, 110_000, "chr1")), base);
        assert_ne!(track_tile_view_key(&view(10_000, 11_000, "chr2")), base);
        let mut other_settings = view(10_000, 11_000, "chr1");
        other_settings.settings.genotype_matrix = false;
        assert_ne!(track_tile_view_key(&other_settings), base);
    }

    fn feature(identity: u64, start: u64, end: u64, row: u32, name: &str) -> TileFeature {
        TileFeature {
            id: TrackRecordId(identity),
            start,
            end,
            strand: Strand::Forward,
            blocks: Vec::new(),
            name: Some(Arc::from(name)),
            score: Some(12.5),
            color: None,
            row_hint: Some(row),
            labelable: true,
        }
    }

    fn laid_out(key: TrackTileKey, features: Vec<TileFeature>) -> Arc<LaidOutTrackTile> {
        let tile = TrackTile {
            contig: "chr1".to_owned(),
            key,
            lod: TrackTileLod::Detail,
            payload: TrackTilePayload::Features(FeatureTile {
                features,
                rows: FeatureRowAxis::Packed,
            }),
            provenance: TrackTileProvenance::default(),
        };
        let scene = lay_out_feature_tile(&tile).unwrap();
        Arc::new(LaidOutTrackTile {
            tile,
            scene,
            settings_digest: 5,
        })
    }

    fn key(level: u8, index: u64) -> TrackTileKey {
        TrackTileKey { level, index }
    }

    /// A view of one tile laid out with `cap` rows, as the engine serves it.
    fn capped_view(features: Vec<TileFeature>, cap: u32) -> TrackTileViewSnapshot {
        let tile = laid_out(key(0, 0), features);
        let mut scene = tile.scene.clone();
        scene.apply_row_cap(cap);
        TrackTileViewSnapshot {
            tiles: vec![Arc::new(LaidOutTrackTile {
                tile: tile.tile.clone(),
                scene,
                settings_digest: tile.settings_digest,
            })],
            ..TrackTileViewSnapshot::default()
        }
    }

    #[test]
    fn lane_status_follows_the_view_and_the_gpu() {
        let stacked = || {
            vec![
                feature(1, 10, 20, 0, "a"),
                feature(2, 12, 30, 14, "b"),
                feature(3, 40, 50, 13, "c"),
            ]
        };
        let ready = capped_view(stacked(), 12);
        assert_eq!(
            track_tile_lane_status(None, None, false, None),
            TrackTileLaneStatus::Loading
        );
        // Rows 12 to 14 are past the cap: three rows holding two features.
        assert_eq!(
            track_tile_lane_status(None, None, false, Some(&ready)),
            TrackTileLaneStatus::Ready {
                hidden_rows: 3,
                hidden_features: 2
            }
        );
        assert_eq!(
            track_tile_lane_status(None, None, false, Some(&capped_view(stacked(), 20))),
            TrackTileLaneStatus::Ready {
                hidden_rows: 0,
                hidden_features: 0
            }
        );
        // A page that is not on the GPU yet reads as loading, never as complete.
        assert_eq!(
            track_tile_lane_status(None, None, true, Some(&ready)),
            TrackTileLaneStatus::Loading
        );
        let mut loading = ready.clone();
        loading.missing.push(key(0, 1));
        assert_eq!(
            track_tile_lane_status(None, None, false, Some(&loading)),
            TrackTileLaneStatus::Loading
        );
        let mut failed = loading;
        failed.failed.push((key(0, 2), "read failed".to_owned()));
        assert_eq!(
            track_tile_lane_status(None, None, false, Some(&failed)),
            TrackTileLaneStatus::Failed("read failed".to_owned())
        );
        assert_eq!(
            track_tile_lane_status(None, Some("page too large"), false, Some(&ready)),
            TrackTileLaneStatus::Failed("page too large".to_owned())
        );
        assert_eq!(
            track_tile_lane_status(Some("no such file"), None, false, Some(&ready)),
            TrackTileLaneStatus::Failed("no such file".to_owned())
        );
    }

    #[test]
    fn a_complete_lane_has_no_status_row_and_a_pending_one_never_reads_as_complete() {
        assert_eq!(
            track_tile_lane_label(&TrackTileLaneStatus::Ready {
                hidden_rows: 0,
                hidden_features: 0
            }),
            None
        );
        assert_eq!(
            track_tile_lane_label(&TrackTileLaneStatus::Loading),
            Some(("Loading…".to_owned(), None))
        );
        let (label, detail) = track_tile_lane_label(&TrackTileLaneStatus::Ready {
            hidden_rows: 4,
            hidden_features: 9,
        })
        .unwrap();
        assert_eq!(label, "Rows hidden");
        let detail = detail.unwrap();
        assert!(detail.starts_with("4 more row"));
        assert!(detail.contains("9 feature"));
        assert_eq!(
            track_tile_lane_label(&TrackTileLaneStatus::Failed("boom".to_owned())),
            Some(("Unavailable".to_owned(), Some("boom".to_owned())))
        );
    }

    #[test]
    fn lane_geometry_uses_the_legacy_frame_and_row_metrics() {
        // A 120-point frame has twelve ten-point rows and no label band, as legacy.
        let lane = track_tile_lane_geometry(200.0, 800.0, 100.0, 220.0, 50.0, 600.0).unwrap();
        assert_eq!(lane.rect, [200.0, 100.0, 800.0, 120.0]);
        assert_eq!(lane.row_pitch, 10.0);
        assert_eq!(lane.row_height, 6.0);
        assert_eq!(lane.row_offset, 2.0);
        assert!(!lane.labels);
        assert_eq!(lane.visible_rows, 12);
        // A 240-point frame has twenty-point rows with the ten-point label band.
        let tall = track_tile_lane_geometry(200.0, 800.0, 100.0, 340.0, 50.0, 600.0).unwrap();
        assert_eq!(tall.row_pitch, 20.0);
        assert_eq!(tall.row_height, 6.0);
        assert_eq!(tall.row_offset, 12.0);
        assert!(tall.labels);
        // Scrolled half out of the pane: the rectangle is clipped and the rows keep their
        // place in the frame.
        let clipped = track_tile_lane_geometry(200.0, 800.0, 20.0, 140.0, 50.0, 600.0).unwrap();
        assert_eq!(clipped.rect, [200.0, 50.0, 800.0, 90.0]);
        assert_eq!(clipped.row_offset, -28.0);
        assert_eq!((clipped.natural_top, clipped.natural_height), (20.0, 120.0));
        // Entirely outside the pane, or degenerate: no lane.
        assert!(track_tile_lane_geometry(200.0, 800.0, 700.0, 820.0, 50.0, 600.0).is_none());
        assert!(track_tile_lane_geometry(200.0, 0.0, 100.0, 220.0, 50.0, 600.0).is_none());
        assert!(track_tile_lane_geometry(200.0, 800.0, 100.0, 100.0, 50.0, 600.0).is_none());
    }

    #[test]
    fn a_clipped_lane_keeps_the_value_axis_of_the_whole_frame() {
        let whole = track_tile_lane_geometry(0.0, 100.0, 100.0, 200.0, 0.0, 600.0).unwrap();
        assert_eq!(whole.clipped_value_range((0.0, 10.0)), (0.0, 10.0));
        // The top quarter is above the pane: the visible rectangle shows values 0 to 7.5.
        let top_clipped = track_tile_lane_geometry(0.0, 100.0, 100.0, 200.0, 125.0, 600.0).unwrap();
        assert_eq!(top_clipped.clipped_value_range((0.0, 10.0)), (0.0, 7.5));
        // The bottom half is below the pane: the visible rectangle shows values 5 to 10.
        let bottom_clipped =
            track_tile_lane_geometry(0.0, 100.0, 100.0, 200.0, 0.0, 150.0).unwrap();
        assert_eq!(bottom_clipped.clipped_value_range((0.0, 10.0)), (5.0, 10.0));
    }

    #[test]
    fn the_value_range_always_includes_zero_and_is_never_empty() {
        assert_eq!(track_tile_value_range([]), (0.0, 1.0));
        let features = laid_out(key(0, 0), vec![feature(1, 10, 20, 0, "a")]);
        assert_eq!(track_tile_value_range([features.as_ref()]), (0.0, 1.0));
    }

    #[test]
    fn stand_in_tiles_are_drawn_only_where_the_view_has_nothing_ready() {
        let ready = laid_out(key(0, 2), vec![feature(1, 2_100, 2_200, 0, "a")]);
        // Level 1 tile 1 spans level 0 tiles 2 and 3; level 1 tile 2 spans tiles 4 and 5.
        let overlapping = laid_out(key(1, 1), vec![feature(1, 2_100, 2_200, 3, "a")]);
        let filling = laid_out(key(1, 2), vec![feature(2, 4_200, 4_300, 0, "b")]);
        let snapshot = TrackTileViewSnapshot {
            tiles: vec![Arc::clone(&ready)],
            missing: vec![key(0, 4)],
            fallback: vec![Arc::clone(&overlapping), Arc::clone(&filling)],
            ..TrackTileViewSnapshot::default()
        };
        let order = track_tile_draw_order(&snapshot);
        assert_eq!(order.len(), 2);
        assert!(Arc::ptr_eq(&order[0], &filling));
        assert!(Arc::ptr_eq(&order[1], &ready));
        // Nothing ready yet: every stand-in is drawn.
        let nothing_ready = TrackTileViewSnapshot {
            missing: vec![key(0, 2), key(0, 4)],
            fallback: vec![overlapping, filling],
            ..TrackTileViewSnapshot::default()
        };
        assert_eq!(track_tile_draw_order(&nothing_ready).len(), 2);
    }

    #[test]
    fn page_keys_distinguish_tracks_contigs_and_settings() {
        let tile = laid_out(key(0, 2), Vec::new());
        let base = track_tile_page_key(TrackId::new(1), &tile);
        assert_eq!(base.track, 1);
        assert_eq!(base.tile, key(0, 2));
        assert_eq!(base, track_tile_page_key(TrackId::new(1), &tile));
        assert_ne!(base, track_tile_page_key(TrackId::new(2), &tile));
        let mut other_contig = LaidOutTrackTile {
            tile: tile.tile.clone(),
            scene: tile.scene.clone(),
            settings_digest: tile.settings_digest,
        };
        other_contig.tile.contig = "chr2".to_owned();
        assert_ne!(base, track_tile_page_key(TrackId::new(1), &other_contig));
        other_contig.tile.contig = "chr1".to_owned();
        other_contig.settings_digest = 6;
        assert_ne!(base, track_tile_page_key(TrackId::new(1), &other_contig));
    }

    fn lane(tiles: Vec<Arc<LaidOutTrackTile>>, camera_start: f64) -> TrackTileLane {
        TrackTileLane {
            panel_id: PanelId::new(1),
            track_id: TrackId::new(9),
            contig: "1".to_owned(),
            // Twelve ten-point rows from y = 100; one point per base.
            geometry: track_tile_lane_geometry(200.0, 800.0, 100.0, 220.0, 50.0, 600.0).unwrap(),
            camera_start,
            points_per_base: 1.0,
            value_range: (0.0, 1.0),
            tiles,
            labels: Vec::new(),
        }
    }

    #[test]
    fn a_pointer_hits_the_feature_drawn_under_it() {
        let tile = laid_out(
            key(0, 0),
            vec![
                feature(11, 100, 200, 0, "first"),
                feature(22, 150, 400, 1, "second"),
            ],
        );
        let lane = lane(vec![tile], 0.0);
        // Row 0 content is y 102..108; base 120 is x = 320.
        let hit = track_tile_hit_in_lane(&lane, 320.0, 105.0).unwrap();
        assert_eq!(hit.id, TrackRecordId(11));
        assert_eq!(hit.panel_id, PanelId::new(1));
        assert_eq!(hit.track_id, TrackId::new(9));
        // The record is the panel's contig, with what the tile carries.
        assert_eq!(hit.record.contig, "1");
        assert_eq!((hit.record.start, hit.record.end), (100, 200));
        assert_eq!(hit.record.name.as_deref(), Some("first"));
        assert_eq!(hit.record.strand, Strand::Forward);
        assert_eq!(hit.record.score, Some(12.5));
        assert!(hit.record.feature_type.is_none() && hit.record.attributes.is_empty());
        // Row 1 content is y 112..118.
        assert_eq!(
            track_tile_hit_in_lane(&lane, 380.0, 115.0).unwrap().id,
            TrackRecordId(22)
        );
        // Between rows, right of every feature, and outside the lane: nothing.
        assert!(track_tile_hit_in_lane(&lane, 320.0, 110.0).is_none());
        assert!(track_tile_hit_in_lane(&lane, 900.0, 105.0).is_none());
        assert!(track_tile_hit_in_lane(&lane, 320.0, 90.0).is_none());
        assert!(track_tile_hit_in_lane(&lane, 100.0, 105.0).is_none());
    }

    #[test]
    fn hits_follow_the_camera_and_the_tile_that_is_drawn_at_the_pointer() {
        let span = TrackTileKey::span_at(0);
        // The same feature crosses the seam and is carried by both tiles.
        let crossing = |row| feature(33, span - 50, span + 50, row, "crossing");
        let left = laid_out(key(0, 0), vec![crossing(0)]);
        let right = laid_out(key(0, 1), vec![crossing(0)]);
        #[allow(clippy::cast_precision_loss)]
        let camera = (span - 100) as f64;
        let lane = lane(vec![left, right], camera);
        // x = 200 is the camera start; the seam is at x = 300.
        for x in [260.0, 299.0, 301.0, 340.0] {
            assert_eq!(
                track_tile_hit_in_lane(&lane, x, 105.0).map(|hit| hit.id),
                Some(TrackRecordId(33)),
                "x = {x}"
            );
        }
        assert!(track_tile_hit_in_lane(&lane, 240.0, 105.0).is_none());
        assert!(track_tile_hit_in_lane(&lane, 360.0, 105.0).is_none());
    }

    #[test]
    fn a_selection_survives_a_tile_refresh_by_record_identity() {
        // The refreshed tile lists the same feature at another index and row.
        let before = laid_out(
            key(0, 0),
            vec![feature(11, 100, 200, 0, "a"), feature(22, 300, 400, 0, "b")],
        );
        let after = laid_out(
            key(0, 0),
            vec![
                feature(5, 10, 50, 0, "new"),
                feature(22, 300, 400, 1, "b"),
                feature(11, 100, 200, 2, "a"),
            ],
        );
        let selected = TrackRecordId(22);
        assert_eq!(before.scene.record_index_of(selected), Some(1));
        assert_eq!(after.scene.record_index_of(selected), Some(1));
        assert_eq!(after.scene.record_index_of(TrackRecordId(11)), Some(2));
        // The inspector record is rebuilt from either tile, or kept when no tile carries it.
        assert_eq!(
            track_tile_feature_record(&before, "chr1", selected),
            track_tile_feature_record(&after, "chr1", selected)
        );
        assert!(track_tile_feature_record(&after, "chr1", TrackRecordId(99)).is_none());
    }

    #[test]
    fn a_looked_up_record_replaces_the_stand_in_and_keeps_the_workspace_contig() {
        let stand_in = track_tile_feature_record(
            &laid_out(key(0, 0), vec![feature(22, 300, 400, 0, "b")]),
            "chr1",
            TrackRecordId(22),
        )
        .unwrap();
        assert_eq!(
            resolve_track_tile_record(&stand_in, &TrackRecordLookup::Pending),
            None
        );
        for outcome in [
            TrackRecordLookup::NotFound,
            TrackRecordLookup::Failed("gone".to_owned()),
        ] {
            assert_eq!(
                resolve_track_tile_record(&stand_in, &outcome),
                Some(stand_in.clone())
            );
        }
        let mut complete = stand_in.clone();
        complete.contig = "1".to_owned();
        complete.feature_type = Some("exon".to_owned());
        complete.attributes = vec![("gene_id".to_owned(), "G1".to_owned())];
        let resolved = resolve_track_tile_record(
            &stand_in,
            &TrackRecordLookup::Ready(Arc::new(TrackRecord::Feature(complete))),
        )
        .unwrap();
        assert_eq!(resolved.contig, "chr1");
        assert_eq!(resolved.feature_type.as_deref(), Some("exon"));
        assert_eq!(resolved.attributes.len(), 1);
    }

    #[test]
    fn a_tile_selection_is_valid_only_while_the_panel_selects_that_routed_track() {
        let mut state = TrackTilePipelineState::default();
        state.routed.insert(TrackId::new(9));
        let panel = PanelId::new(1);
        state.selections.insert(
            panel,
            TrackTileSelection {
                track_id: TrackId::new(9),
                id: TrackRecordId(22),
                contig: "chr1".to_owned(),
                record: track_tile_feature_record(
                    &laid_out(key(0, 0), vec![feature(22, 300, 400, 0, "b")]),
                    "chr1",
                    TrackRecordId(22),
                )
                .unwrap(),
                complete: false,
            },
        );
        let tile_selection = |track: u64| {
            Some(FeatureSelection {
                track_id: TrackId::new(track),
                record_index: TRACK_TILE_SELECTION_RECORD_INDEX,
            })
        };
        assert!(state.selection(panel, tile_selection(9)).is_some());
        // Cleared, replaced by a legacy selection, or on another track or panel: not valid.
        assert!(state.selection(panel, None).is_none());
        assert!(
            state
                .selection(
                    panel,
                    Some(FeatureSelection {
                        track_id: TrackId::new(9),
                        record_index: 0,
                    })
                )
                .is_none()
        );
        assert!(state.selection(panel, tile_selection(4)).is_none());
        assert!(
            state
                .selection(PanelId::new(2), tile_selection(9))
                .is_none()
        );
        // Back on the legacy path: the selection ends.
        state.routed.clear();
        assert!(state.selection(panel, tile_selection(9)).is_none());
    }
}
