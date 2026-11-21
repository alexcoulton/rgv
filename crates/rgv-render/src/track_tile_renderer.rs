//! The paged world-space renderer for track content.
//!
//! One pipeline draws every auxiliary track kind. A page is the instance data of one laid-out
//! tile ([`rgv_model::TrackTileScene`]): 28-byte instances in tile-local genomic offsets,
//! uploaded once and never rewritten while the user pans or zooms within the tile's level. A
//! frame binds, per lane and page, a small uniform holding the camera relative to the tile
//! start; the vertex stage projects with the same integer-cancellation contract as the
//! alignment pipelines (`project_genomic_x`).
//!
//! * **No silent caps.** A page holds every instance of its scene or is not admitted at all;
//!   [`TrackTileRenderer::admit_page`] returns the budget error and
//!   [`TrackTileRenderer::prepare`] reports the pages it could not draw. Density is bounded
//!   upstream by level of detail (sources summarise dense tiles), never by dropping shapes.
//! * **Exact accounting.** Every page buffer is charged to the shared allocation ledger at its
//!   exact size through the shared [`PagedGpuCache`], which also owns LRU eviction.
//! * **Seams.** Shapes are not clipped to their tile; every draw is scissored to the pixel
//!   columns of its tile, computed once per frame in f64, so neighbouring tiles partition the
//!   lane exactly and a shape crossing a seam is one continuous projection.
//! * **Colours** are palette tokens resolved in the vertex stage; a theme change uploads 1 kB.

use std::sync::atomic::AtomicU64;

use bytemuck::{Pod, Zeroable};
use rgv_alignment_scene::{
    AllocationClass, AllocationLease, AllocationLedger, LedgerError, ReservationKind,
};
use rgv_layout::TrackScenePoint;
use rgv_model::{
    TRACK_PALETTE_ENTRIES, TRACK_SCENE_FLAG_BIN_MEAN, TRACK_SCENE_FLAG_POINT, TrackColor,
    TrackColorToken, TrackSceneInstance, TrackSceneShape, TrackSceneVertical, TrackTileKey,
    TrackTileScene,
};
use thiserror::Error;

use crate::{
    paged_cache::{CachedGpuPage, PageTableError, PagedGpuCache},
    palette::ThemeId,
    shaders::{self, split_camera_origin},
};

/// Bytes of one instance on the GPU.
pub const TRACK_TILE_INSTANCE_BYTES: usize = size_of::<TrackSceneInstance>();
/// Bytes of the per-draw lane uniform.
pub const TRACK_LANE_UNIFORM_BYTES: usize = size_of::<LaneUniform>();

const CLAMP_MARGIN: f64 = 64.0;
const POINT_HALF_SIZE: f64 = 4.0;
const CHEVRON_PERIOD: f64 = 12.0;
const SELECTED_TINT: ([f64; 3], f64) = ([0.16, 0.42, 0.92], 0.58);
const HOVERED_TINT: ([f64; 3], f64) = ([0.28, 0.52, 0.94], 0.26);
/// Record word that matches no record.
const NO_HIGHLIGHT: u32 = u32::MAX;

/// Vertex attributes of a [`TrackSceneInstance`], in field order.
const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Sint32,
    1 => Uint32,
    2 => Float32x2,
    3 => Uint32,
    4 => Uint32,
    5 => Uint32,
];

/// Theme colours of the track tokens.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct TrackPalette {
    colors: [[f32; 4]; TRACK_PALETTE_ENTRIES],
}

const fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> [f32; 4] {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        alpha,
    ]
}

impl TrackPalette {
    #[must_use]
    pub fn light() -> Self {
        let mut palette = Self {
            colors: [[0.5, 0.5, 0.5, 1.0]; TRACK_PALETTE_ENTRIES],
        };
        let mut set =
            |token: TrackColorToken, color| palette.colors[token.index() as usize] = color;
        set(TrackColorToken::FEATURE_FORWARD, rgba(40, 130, 200, 1.0));
        set(TrackColorToken::FEATURE_REVERSE, rgba(205, 80, 95, 1.0));
        set(
            TrackColorToken::FEATURE_UNSTRANDED,
            rgba(120, 105, 190, 1.0),
        );
        set(TrackColorToken::FEATURE_CONNECTOR, rgba(90, 96, 110, 1.0));
        set(TrackColorToken::BIN_MEAN, rgba(50, 110, 170, 1.0));
        set(TrackColorToken::BIN_ENVELOPE, rgba(50, 110, 170, 0.28));
        set(TrackColorToken::DENSITY, rgba(110, 118, 132, 1.0));
        set(TrackColorToken::VARIANT_SNV, rgba(30, 120, 200, 1.0));
        set(TrackColorToken::VARIANT_INSERTION, rgba(130, 70, 180, 1.0));
        set(TrackColorToken::VARIANT_DELETION, rgba(200, 60, 50, 1.0));
        set(TrackColorToken::VARIANT_STRUCTURAL, rgba(220, 140, 20, 1.0));
        set(TrackColorToken::VARIANT_OTHER, rgba(110, 110, 120, 1.0));
        set(TrackColorToken::GENOTYPE_HOM_REF, rgba(205, 210, 218, 1.0));
        set(TrackColorToken::GENOTYPE_HET, rgba(60, 120, 215, 1.0));
        set(TrackColorToken::GENOTYPE_HOM_ALT, rgba(20, 190, 215, 1.0));
        set(TrackColorToken::GENOTYPE_NO_CALL, rgba(250, 250, 250, 1.0));
        set(TrackColorToken::INTERACTION_ARC, rgba(170, 80, 150, 0.85));
        set(TrackColorToken::INTERACTION_ANCHOR, rgba(120, 60, 110, 1.0));
        set(TrackColorToken::SCORE_LOW, rgba(215, 222, 235, 1.0));
        set(TrackColorToken::SCORE_HIGH, rgba(20, 60, 140, 1.0));
        set(TrackColorToken::COPY_LOSS, rgba(30, 80, 200, 1.0));
        set(TrackColorToken::COPY_NEUTRAL, rgba(240, 240, 240, 1.0));
        set(TrackColorToken::COPY_GAIN, rgba(205, 40, 40, 1.0));
        set(TrackColorToken::ASSOCIATION, rgba(60, 100, 170, 1.0));
        set(TrackColorToken::MUTATION, rgba(70, 150, 90, 1.0));
        palette
    }

    #[must_use]
    pub fn dark() -> Self {
        let mut palette = Self::light();
        let mut set =
            |token: TrackColorToken, color| palette.colors[token.index() as usize] = color;
        set(TrackColorToken::FEATURE_FORWARD, rgba(95, 175, 240, 1.0));
        set(TrackColorToken::FEATURE_REVERSE, rgba(240, 120, 130, 1.0));
        set(
            TrackColorToken::FEATURE_UNSTRANDED,
            rgba(165, 150, 230, 1.0),
        );
        set(TrackColorToken::FEATURE_CONNECTOR, rgba(160, 166, 180, 1.0));
        set(TrackColorToken::BIN_MEAN, rgba(110, 170, 230, 1.0));
        set(TrackColorToken::BIN_ENVELOPE, rgba(110, 170, 230, 0.30));
        set(TrackColorToken::DENSITY, rgba(150, 158, 172, 1.0));
        set(TrackColorToken::GENOTYPE_HOM_REF, rgba(70, 74, 84, 1.0));
        set(TrackColorToken::GENOTYPE_NO_CALL, rgba(30, 32, 38, 1.0));
        set(TrackColorToken::SCORE_LOW, rgba(50, 58, 75, 1.0));
        set(TrackColorToken::SCORE_HIGH, rgba(150, 195, 255, 1.0));
        set(TrackColorToken::COPY_NEUTRAL, rgba(48, 50, 58, 1.0));
        palette
    }

    /// The palette of `theme`. The colour-blind themes swap the strand, variant and genotype
    /// tokens for Okabe-Ito colours.
    #[must_use]
    pub fn for_theme(theme: ThemeId) -> Self {
        let mut palette = if theme.is_dark() {
            Self::dark()
        } else {
            Self::light()
        };
        if matches!(theme, ThemeId::LightColorBlind | ThemeId::DarkColorBlind) {
            let mut set =
                |token: TrackColorToken, color| palette.colors[token.index() as usize] = color;
            set(TrackColorToken::FEATURE_FORWARD, rgba(0, 114, 178, 1.0));
            set(TrackColorToken::FEATURE_REVERSE, rgba(213, 94, 0, 1.0));
            set(TrackColorToken::VARIANT_SNV, rgba(0, 114, 178, 1.0));
            set(TrackColorToken::VARIANT_INSERTION, rgba(204, 121, 167, 1.0));
            set(TrackColorToken::VARIANT_DELETION, rgba(213, 94, 0, 1.0));
            set(TrackColorToken::VARIANT_STRUCTURAL, rgba(230, 159, 0, 1.0));
            set(TrackColorToken::GENOTYPE_HET, rgba(0, 114, 178, 1.0));
            set(TrackColorToken::GENOTYPE_HOM_ALT, rgba(86, 180, 233, 1.0));
            set(TrackColorToken::COPY_LOSS, rgba(0, 114, 178, 1.0));
            set(TrackColorToken::COPY_GAIN, rgba(213, 94, 0, 1.0));
        }
        palette
    }

    #[must_use]
    pub const fn color(&self, token: TrackColorToken) -> [f32; 4] {
        self.colors[token.index() as usize % TRACK_PALETTE_ENTRIES]
    }

    pub fn set_color(&mut self, token: TrackColorToken, color: [f32; 4]) {
        self.colors[token.index() as usize % TRACK_PALETTE_ENTRIES] = color;
    }

    /// The colour the shader produces for `color`.
    #[must_use]
    pub fn resolve(&self, color: TrackColor) -> [f32; 4] {
        match color {
            TrackColor::Inline(rgba) => rgba.map(|channel| f32::from(channel) / 255.0),
            TrackColor::Token(token) => self.color(token),
            TrackColor::Ramp { low, position } => {
                let from = self.color(low);
                let to = self.colors[(low.index() as usize + 1) % TRACK_PALETTE_ENTRIES];
                std::array::from_fn(|channel| {
                    from[channel] + (to[channel] - from[channel]) * position
                })
            }
        }
    }
}

impl Default for TrackPalette {
    fn default() -> Self {
        Self::light()
    }
}

/// How the mean of a bin is drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TrackBinStyle {
    /// A bar from the zero baseline to the mean.
    #[default]
    Bars,
    /// A one-point step line at the mean.
    Line,
}

/// Where and how one track lane is drawn: its rectangle, camera and row metrics. This is the
/// whole per-frame state of a lane; the pages it draws are immutable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLaneProjection {
    /// Lane rectangle in logical points: x, y, width, height.
    pub rect: [f32; 4],
    /// Exact genomic position at the lane's left edge.
    pub camera_start: f64,
    pub points_per_base: f64,
    /// Distance between row tops, in points.
    pub row_pitch: f32,
    /// Height of a row's content, in points.
    pub row_height: f32,
    /// Top of row 0 relative to the lane top: label band, padding and vertical scroll.
    pub row_offset: f32,
    /// Data values at the lane's bottom and top edges, for `Value` instances.
    pub value_range: (f32, f32),
    pub bin_style: TrackBinStyle,
}

impl TrackLaneProjection {
    /// The scene coordinates under a point of the lane, for
    /// [`rgv_layout::hit_test_track_scene`].
    #[must_use]
    pub fn scene_point(&self, x: f32, y: f32) -> TrackScenePoint {
        TrackScenePoint {
            position: self.camera_start
                + f64::from(x - self.rect[0]) / self.points_per_base.max(f64::MIN_POSITIVE),
            row: f64::from(y - self.rect[1] - self.row_offset)
                / f64::from(self.row_pitch.max(f32::MIN_POSITIVE)),
            lane_fraction: f64::from(y - self.rect[1])
                / f64::from(self.rect[3].max(f32::MIN_POSITIVE)),
        }
    }

    /// Half the one-point minimum width, in bases: the hit-test tolerance at this zoom.
    #[must_use]
    pub fn hit_tolerance_bases(&self) -> f64 {
        0.5 / self.points_per_base.max(f64::MIN_POSITIVE)
    }

    /// Horizontal position of a genomic coordinate, in logical points.
    #[must_use]
    pub fn x_of(&self, position: f64) -> f64 {
        f64::from(self.rect[0]) + (position - self.camera_start) * self.points_per_base
    }

    /// Physical-pixel scissor `[x, y, width, height]` of a tile in this lane: the lane
    /// rectangle restricted to the tile's columns, clamped to the target. Both tile edges are
    /// rounded the same way, so the scissors of adjacent tiles share no pixel and leave none.
    #[must_use]
    pub fn tile_scissor(
        &self,
        tile: TrackTileKey,
        scale_factor: f64,
        target_size_pixels: [u32; 2],
    ) -> Option<[u32; 4]> {
        let pixel = |point: f64, limit: u32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                (point * scale_factor).round().clamp(0.0, f64::from(limit)) as u32
            }
        };
        let lane_left = f64::from(self.rect[0]);
        let lane_right = lane_left + f64::from(self.rect[2]);
        #[allow(clippy::cast_precision_loss)]
        let (tile_left, tile_right) =
            (self.x_of(tile.start() as f64), self.x_of(tile.end() as f64));
        let left = pixel(tile_left.max(lane_left), target_size_pixels[0]);
        let right = pixel(tile_right.min(lane_right), target_size_pixels[0]);
        let top = pixel(f64::from(self.rect[1]), target_size_pixels[1]);
        let bottom = pixel(
            f64::from(self.rect[1]) + f64::from(self.rect[3]),
            target_size_pixels[1],
        );
        (right > left && bottom > top && tile_right > tile_left)
            .then(|| [left, top, right - left, bottom - top])
    }

    fn uniform(
        &self,
        screen_size: [f32; 2],
        tile_start: u64,
        selected: Option<u32>,
        hovered: Option<u32>,
    ) -> LaneUniform {
        let (origin_whole, origin_fraction) = split_camera_origin(tile_start, self.camera_start);
        #[allow(clippy::cast_possible_truncation)]
        let high = self.points_per_base as f32;
        #[allow(clippy::cast_possible_truncation)]
        let low = (self.points_per_base - f64::from(high)) as f32;
        LaneUniform {
            screen_size,
            lane_origin: [self.rect[0], self.rect[1]],
            lane_size: [self.rect[2], self.rect[3]],
            row_pitch: self.row_pitch,
            row_height: self.row_height,
            points_per_base: [high, if low.is_finite() { low } else { 0.0 }],
            origin_fraction,
            row_offset: self.row_offset,
            value_range: [self.value_range.0, self.value_range.1],
            origin_whole,
            selected: selected.unwrap_or(NO_HIGHLIGHT),
            hovered: hovered.unwrap_or(NO_HIGHLIGHT),
            bin_style: u32::from(self.bin_style == TrackBinStyle::Line),
            padding: [0; 2],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct LaneUniform {
    screen_size: [f32; 2],
    lane_origin: [f32; 2],
    lane_size: [f32; 2],
    row_pitch: f32,
    row_height: f32,
    points_per_base: [f32; 2],
    origin_fraction: f32,
    row_offset: f32,
    value_range: [f32; 2],
    origin_whole: i32,
    selected: u32,
    hovered: u32,
    bin_style: u32,
    padding: [u32; 2],
}

/// One instance as the vertex stage places it, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedTrackInstance {
    /// True left, top, right, bottom of the shape (after the one-point minimum).
    pub bounds: [f64; 4],
    /// The quad actually rasterised: `bounds` clamped to the lane plus slack, and padded for
    /// arc strokes. Fragments exist only inside it.
    pub quad: [f64; 4],
    pub shape: TrackSceneShape,
    pub color: TrackColor,
    pub record: Option<u32>,
}

/// CPU projection oracle: exactly what the vertex stage of `track_tile.wgsl` computes, in f64.
/// Used by the readback tests, by export and wherever geometry must be known without the GPU.
#[must_use]
pub fn project_track_instance(
    instance: &TrackSceneInstance,
    tile_start: u64,
    lane: &TrackLaneProjection,
) -> ProjectedTrackInstance {
    #[allow(clippy::cast_precision_loss)]
    let start = tile_start as f64 + f64::from(instance.start);
    let mut left = lane.x_of(start);
    let mut right = lane.x_of(start + f64::from(instance.length));
    let lane_top = f64::from(lane.rect[1]);
    let lane_height = f64::from(lane.rect[3]);
    let (y0, y1) = (f64::from(instance.y0), f64::from(instance.y1));
    let (mut top, mut bottom) = match instance.vertical() {
        TrackSceneVertical::Value => {
            let (low, high) = (f64::from(lane.value_range.0), f64::from(lane.value_range.1));
            let y_of = |value: f64| {
                lane_top
                    + lane_height
                        * (1.0 - ((value - low) / (high - low).max(1.0e-20)).clamp(0.0, 1.0))
            };
            let to = y_of(y1);
            let from = if instance.flags() & TRACK_SCENE_FLAG_BIN_MEAN != 0
                && lane.bin_style == TrackBinStyle::Line
            {
                to
            } else {
                y_of(y0)
            };
            (from.min(to), from.max(to))
        }
        TrackSceneVertical::Lane => (lane_top + y0 * lane_height, lane_top + y1 * lane_height),
        TrackSceneVertical::Row => {
            let row_top = lane_top
                + f64::from(lane.row_offset)
                + f64::from(instance.row()) * f64::from(lane.row_pitch);
            (
                row_top + y0 * f64::from(lane.row_height),
                row_top + y1 * f64::from(lane.row_height),
            )
        }
    };
    if instance.flags() & TRACK_SCENE_FLAG_POINT != 0 {
        let centre = ((left + right) * 0.5, (top + bottom) * 0.5);
        left = centre.0 - POINT_HALF_SIZE;
        right = centre.0 + POINT_HALF_SIZE;
        top = centre.1 - POINT_HALF_SIZE;
        bottom = centre.1 + POINT_HALF_SIZE;
    } else {
        if right - left < 1.0 {
            let centre = (left + right) * 0.5;
            left = centre - 0.5;
            right = centre + 0.5;
        }
        if bottom - top < 1.0 {
            let centre = (top + bottom) * 0.5;
            top = centre - 0.5;
            bottom = centre + 0.5;
        }
    }
    let shape = instance.shape().unwrap_or(TrackSceneShape::Box);
    let low_x = f64::from(lane.rect[0]) - CLAMP_MARGIN;
    let high_x = f64::from(lane.rect[0]) + f64::from(lane.rect[2]) + CLAMP_MARGIN;
    let stroke = if shape == TrackSceneShape::Arc {
        1.5
    } else {
        0.0
    };
    ProjectedTrackInstance {
        bounds: [left, top, right, bottom],
        quad: [
            left.clamp(low_x, high_x),
            top - stroke,
            right.clamp(low_x, high_x),
            bottom + stroke,
        ],
        shape,
        color: instance.color(),
        record: instance.record_index(),
    }
}

/// CPU mirror of the fragment stage's shape rules: the alpha multiplier of the fragment at
/// `point` for a shape with true `bounds`, or `None` where the fragment is discarded.
#[must_use]
pub fn track_fragment_coverage(
    shape: TrackSceneShape,
    point: [f64; 2],
    bounds: [f64; 4],
) -> Option<f64> {
    let size = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    let local = [point[0] - bounds[0], point[1] - bounds[1]];
    let half_height = size[1] * 0.5;
    match shape {
        TrackSceneShape::Box => Some(1.0),
        TrackSceneShape::ArrowRight | TrackSceneShape::ArrowLeft => {
            let head = (size[1] * 0.55).min(size[0] * 0.5);
            let from_tip = if shape == TrackSceneShape::ArrowLeft {
                local[0]
            } else {
                bounds[2] - point[0]
            };
            if from_tip < head {
                let allowed = half_height * (from_tip / head.max(1.0e-6)).clamp(0.0, 1.0);
                if (local[1] - half_height).abs() > allowed {
                    return None;
                }
            }
            Some(1.0)
        }
        TrackSceneShape::Line
        | TrackSceneShape::LineChevronRight
        | TrackSceneShape::LineChevronLeft => {
            let from_centre = (local[1] - half_height).abs();
            let mut inside = from_centre <= 0.5;
            if shape != TrackSceneShape::Line && from_centre <= 3.0 {
                let phase = local[0] / CHEVRON_PERIOD;
                let mut along = (phase - phase.floor()) * CHEVRON_PERIOD - CHEVRON_PERIOD * 0.5;
                if shape == TrackSceneShape::LineChevronLeft {
                    along = -along;
                }
                inside = inside || (along + from_centre - 3.0).abs() <= 0.6;
            }
            inside.then_some(1.0)
        }
        TrackSceneShape::Arc => {
            let fraction = (local[0] / size[0].max(1.0e-6)).clamp(0.0, 1.0);
            let offset = fraction - 0.5;
            let curve = size[1] * 4.0 * offset * offset;
            let slope = size[1] * 8.0 * offset / size[0].max(1.0);
            let distance = (local[1] - curve).abs() / (1.0 + slope * slope).sqrt();
            let t = ((distance - 0.25) / 1.0).clamp(0.0, 1.0);
            let coverage = 1.0 - t * t * (3.0 - 2.0 * t);
            (coverage > 0.0).then_some(coverage)
        }
        TrackSceneShape::Diamond => {
            let half = [(size[0] * 0.5).max(1.0e-6), (size[1] * 0.5).max(1.0e-6)];
            let reach = (local[0] - half[0]).abs() / half[0] + (local[1] - half[1]).abs() / half[1];
            (reach <= 1.0).then_some(1.0)
        }
    }
}

/// The colour the fragment stage outputs for a shape of `base` colour and `coverage`, given
/// whether its record is selected or hovered.
#[must_use]
pub fn track_fragment_color(
    base: [f32; 4],
    coverage: f64,
    selected: bool,
    hovered: bool,
) -> [f64; 4] {
    let base = base.map(f64::from);
    let tint = |(target, amount): ([f64; 3], f64)| {
        [
            base[0] + (target[0] - base[0]) * amount,
            base[1] + (target[1] - base[1]) * amount,
            base[2] + (target[2] - base[2]) * amount,
            1.0,
        ]
    };
    if selected {
        tint(SELECTED_TINT)
    } else if hovered {
        tint(HOVERED_TINT)
    } else {
        [base[0], base[1], base[2], base[3] * coverage]
    }
}

/// Identity of a tile's GPU page: the track, the tile and the digest of the settings the tile
/// was built with.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackTilePageKey {
    pub track: u64,
    pub tile: TrackTileKey,
    pub settings_digest: u64,
}

#[derive(Debug)]
struct TrackTilePage {
    key: TrackTilePageKey,
    /// `None` for a tile without instances, which is resident without a buffer.
    buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    allocation: Option<AllocationLease>,
    last_used_revision: u64,
}

impl CachedGpuPage for TrackTilePage {
    type Key = TrackTilePageKey;

    fn key(&self) -> Self::Key {
        self.key
    }

    fn last_used_revision(&self) -> u64 {
        self.last_used_revision
    }

    fn set_last_used_revision(&mut self, revision: u64) {
        self.last_used_revision = revision;
    }

    fn allocation(&self) -> Option<&AllocationLease> {
        self.allocation.as_ref()
    }
}

#[derive(Debug, Error)]
pub enum TrackTileRenderError {
    /// The page does not fit the GPU budget even after evicting every page the frame does not
    /// need. Nothing was uploaded; the caller shows the tile as unavailable (or requests a
    /// coarser level) instead of a partial tile.
    #[error("track tile page of {required} bytes does not fit the GPU budget of {limit} bytes")]
    Budget { required: usize, limit: usize },
    #[error("track tile page of {instances} instances exceeds the device limits")]
    DeviceLimit { instances: usize },
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("track tile page table: {0}")]
    PageTable(String),
}

impl From<PageTableError> for TrackTileRenderError {
    fn from(error: PageTableError) -> Self {
        match error {
            PageTableError::Ledger(error) => Self::Ledger(error),
            other => Self::PageTable(other.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackTilePageAdmission {
    /// The page was already resident; nothing was uploaded.
    pub already_resident: bool,
    /// Instance bytes of the page: exactly what was uploaded and charged.
    pub bytes: usize,
    pub instances: usize,
}

/// One page drawn in a lane, with the tile-local record indices to highlight
/// ([`TrackTileScene::record_index_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackLanePage {
    pub key: TrackTilePageKey,
    pub selected: Option<u32>,
    pub hovered: Option<u32>,
}

/// One lane of a frame: its projection and the pages it draws, bottom-most first.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackLaneDraw {
    pub projection: TrackLaneProjection,
    pub pages: Vec<TrackLanePage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackTileFrame {
    /// Target size in logical points.
    pub screen_size: [f32; 2],
    /// Physical pixels per logical point.
    pub scale_factor: f64,
    pub target_size_pixels: [u32; 2],
    pub lanes: Vec<TrackLaneDraw>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackTilePrepareReport {
    pub draws: usize,
    pub instances: u64,
    /// Bytes written to the lane uniform buffer this frame.
    pub uniform_bytes: usize,
    /// Pages a lane asked for that are not resident; they were not drawn and are reported
    /// here, never skipped silently.
    pub missing_pages: Vec<TrackTilePageKey>,
}

struct PreparedDraw {
    page: usize,
    uniform_offset: u32,
    scissor: [u32; 4],
}

pub struct TrackTileRenderer {
    pipeline: wgpu::RenderPipeline,
    lane_layout: wgpu::BindGroupLayout,
    lane_buffer: Option<(wgpu::Buffer, wgpu::BindGroup, usize)>,
    lane_stride: usize,
    palette: TrackPalette,
    palette_buffer: wgpu::Buffer,
    palette_bind_group: wgpu::BindGroup,
    cache: PagedGpuCache<TrackTilePage>,
    ledger: AllocationLedger,
    revision: u64,
    keys_inspected: AtomicU64,
    draws: Vec<PreparedDraw>,
    target_size_pixels: [u32; 2],
    maximum_buffer_bytes: u64,
}

impl TrackTileRenderer {
    /// `ledger` is the allocation ledger pages are charged to; pass the application's shared
    /// ledger so track pages compete with alignment pages under one budget.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        ledger: AllocationLedger,
    ) -> Self {
        let shader = shaders::TRACK_TILE.create_module(device);
        let lane_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV track lane layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: std::num::NonZeroU64::new(TRACK_LANE_UNIFORM_BYTES as u64),
                },
                count: None,
            }],
        });
        let palette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV track palette layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(size_of::<TrackPalette>() as u64),
                },
                count: None,
            }],
        });
        let palette = TrackPalette::default();
        let palette_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RGV track palette"),
            size: size_of::<TrackPalette>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        palette_buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(bytemuck::bytes_of(&palette));
        palette_buffer.unmap();
        let palette_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV track palette bind group"),
            layout: &palette_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RGV track tile pipeline layout"),
            bind_group_layouts: &[Some(&lane_layout), Some(&palette_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RGV track tile pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: TRACK_TILE_INSTANCE_BYTES as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRIBUTES,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let alignment = device.limits().min_uniform_buffer_offset_alignment.max(1) as usize;
        Self {
            pipeline,
            lane_layout,
            lane_buffer: None,
            lane_stride: TRACK_LANE_UNIFORM_BYTES.div_ceil(alignment) * alignment,
            palette,
            palette_buffer,
            palette_bind_group,
            cache: PagedGpuCache::default(),
            ledger,
            revision: 1,
            keys_inspected: AtomicU64::new(0),
            draws: Vec::new(),
            target_size_pixels: [0; 2],
            maximum_buffer_bytes: device.limits().max_buffer_size,
        }
    }

    /// Switches theme: one 1 kB upload, no page is touched.
    pub fn set_palette(&mut self, queue: &wgpu::Queue, palette: &TrackPalette) -> bool {
        if self.palette == *palette {
            return false;
        }
        self.palette = *palette;
        queue.write_buffer(&self.palette_buffer, 0, bytemuck::bytes_of(palette));
        true
    }

    #[must_use]
    pub const fn palette(&self) -> &TrackPalette {
        &self.palette
    }

    #[must_use]
    pub fn is_resident(&self, key: TrackTilePageKey) -> bool {
        self.cache.page_index(key).is_some()
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.cache.pages.len()
    }

    /// Ledger charge of every page buffer and the page table.
    pub fn resident_page_bytes(&self) -> Result<usize, LedgerError> {
        self.cache.charged_bytes()
    }

    /// Ledger charge of the page table alone.
    #[cfg(test)]
    pub(crate) fn page_table_bytes(&self) -> usize {
        self.cache
            .page_table_allocation
            .as_ref()
            .and_then(|allocation| allocation.snapshot().ok())
            .map_or(0, |snapshot| snapshot.charged_capacity)
    }

    /// Bytes of the lane uniform buffer (not ledger-charged: it is a few kilobytes).
    #[must_use]
    pub fn lane_buffer_bytes(&self) -> usize {
        self.lane_buffer
            .as_ref()
            .map_or(0, |(_, _, capacity)| capacity * self.lane_stride)
    }

    /// Uploads the page of `scene` unless it is already resident. Every instance of the scene
    /// is uploaded, or none: when the page does not fit the ledger budget, least-recently-used
    /// pages not listed in `protected` are evicted, and if it still does not fit the budget
    /// error is returned.
    pub fn admit_page(
        &mut self,
        device: &wgpu::Device,
        key: TrackTilePageKey,
        scene: &TrackTileScene,
        protected: &[TrackTilePageKey],
    ) -> Result<TrackTilePageAdmission, TrackTileRenderError> {
        self.revision += 1;
        let bytes = scene.instance_bytes();
        let instances = scene.instances.len();
        if let Some(index) = self.cache.page_index(key) {
            self.cache.touch_page_at(index, self.revision);
            return Ok(TrackTilePageAdmission {
                already_resident: true,
                bytes,
                instances,
            });
        }
        let instance_count = u32::try_from(instances)
            .ok()
            .filter(|_| bytes as u64 <= self.maximum_buffer_bytes)
            .ok_or(TrackTileRenderError::DeviceLimit { instances })?;
        self.with_eviction(protected, bytes, |renderer| {
            renderer
                .cache
                .ensure_page_table_capacity(
                    &renderer.ledger,
                    renderer.cache.pages.len() + 1,
                    ReservationKind::Normal,
                    renderer.revision,
                )
                .map_err(TrackTileRenderError::from)
        })?;
        let allocation = if bytes == 0 {
            None
        } else {
            Some(self.with_eviction(protected, bytes, |renderer| {
                Ok(renderer
                    .ledger
                    .reserve(AllocationClass::GpuPage, bytes, ReservationKind::Normal)?
                    .register(bytes, renderer.revision)?)
            })?)
        };
        let buffer = (bytes > 0).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV track tile page"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX,
                mapped_at_creation: true,
            });
            let words = scene
                .instances
                .iter()
                .map(TrackSceneInstance::to_words)
                .collect::<Vec<_>>();
            buffer
                .slice(..)
                .get_mapped_range_mut()
                .copy_from_slice(bytemuck::cast_slice(&words));
            buffer.unmap();
            buffer
        });
        let page = TrackTilePage {
            key,
            buffer,
            instance_count,
            allocation,
            last_used_revision: self.revision,
        };
        let index = self
            .cache
            .pages
            .binary_search_by_key(&key, CachedGpuPage::key)
            .unwrap_or_else(|index| index);
        self.cache.pages.insert(index, page);
        self.cache.reclaim_index.insert((self.revision, key));
        Ok(TrackTilePageAdmission {
            already_resident: false,
            bytes,
            instances,
        })
    }

    /// Runs a ledger operation; on a budget failure evicts unprotected LRU pages for it and
    /// tries once more.
    fn with_eviction<T>(
        &mut self,
        protected: &[TrackTilePageKey],
        required: usize,
        mut operation: impl FnMut(&mut Self) -> Result<T, TrackTileRenderError>,
    ) -> Result<T, TrackTileRenderError> {
        match operation(self) {
            Err(TrackTileRenderError::Ledger(LedgerError::BudgetExceeded {
                requested,
                limit,
                ..
            })) => {
                // A page that cannot fit an empty budget must not cost the resident pages.
                if requested > limit {
                    return Err(TrackTileRenderError::Budget { required, limit });
                }
                self.cache.evict_unpinned_pages_for_excluding(
                    &self.ledger,
                    requested,
                    limit,
                    protected,
                    self.revision,
                    &self.keys_inspected,
                )?;
                operation(self).map_err(|error| match error {
                    TrackTileRenderError::Ledger(LedgerError::BudgetExceeded { limit, .. }) => {
                        TrackTileRenderError::Budget { required, limit }
                    }
                    other => other,
                })
            }
            other => other,
        }
    }

    /// Drops every page of `track` (a removed track, or one whose file changed).
    pub fn retire_track(&mut self, track: u64) {
        self.draws.clear();
        self.cache.pages.retain(|page| page.key.track != track);
        self.cache.rebuild_reclaim_index();
    }

    /// Drops every page and the page table, releasing their ledger charges.
    pub fn clear(&mut self) {
        self.draws.clear();
        self.cache.clear();
    }

    /// Builds the frame: one small uniform per (lane, page) and the draw list. No page data is
    /// written. Pages that are not resident are listed in the report.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &TrackTileFrame,
    ) -> TrackTilePrepareReport {
        self.revision += 1;
        self.draws.clear();
        self.target_size_pixels = frame.target_size_pixels;
        let mut report = TrackTilePrepareReport::default();
        let mut uniforms = Vec::<u8>::new();
        for lane in &frame.lanes {
            for page in &lane.pages {
                let Some(index) = self.cache.page_index(page.key) else {
                    report.missing_pages.push(page.key);
                    continue;
                };
                self.cache.touch_page_at(index, self.revision);
                let instance_count = self.cache.pages[index].instance_count;
                let Some(scissor) = lane.projection.tile_scissor(
                    page.key.tile,
                    frame.scale_factor,
                    frame.target_size_pixels,
                ) else {
                    continue;
                };
                if instance_count == 0 {
                    continue;
                }
                let Ok(uniform_offset) = u32::try_from(uniforms.len()) else {
                    report.missing_pages.push(page.key);
                    continue;
                };
                uniforms.extend_from_slice(bytemuck::bytes_of(&lane.projection.uniform(
                    frame.screen_size,
                    page.key.tile.start(),
                    page.selected,
                    page.hovered,
                )));
                uniforms.resize(uniform_offset as usize + self.lane_stride, 0);
                self.draws.push(PreparedDraw {
                    page: index,
                    uniform_offset,
                    scissor,
                });
                report.instances += u64::from(instance_count);
            }
        }
        report.draws = self.draws.len();
        report.uniform_bytes = uniforms.len();
        if !self.draws.is_empty() {
            let required = self.draws.len();
            if self
                .lane_buffer
                .as_ref()
                .is_none_or(|(_, _, capacity)| *capacity < required)
            {
                let capacity = required.next_power_of_two().max(16);
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("RGV track lane uniforms"),
                    size: (capacity * self.lane_stride) as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("RGV track lane bind group"),
                    layout: &self.lane_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: 0,
                            size: std::num::NonZeroU64::new(TRACK_LANE_UNIFORM_BYTES as u64),
                        }),
                    }],
                });
                self.lane_buffer = Some((buffer, bind_group, capacity));
            }
            if let Some((buffer, _, _)) = &self.lane_buffer {
                queue.write_buffer(buffer, 0, &uniforms);
            }
        }
        report
    }

    /// Draws the prepared frame: one instanced draw per (lane, page), each scissored to its
    /// tile's columns of its lane. The scissor is reset to the whole target afterwards.
    pub fn render<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let Some((_, lane_bind_group, _)) = &self.lane_buffer else {
            return;
        };
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, &self.palette_bind_group, &[]);
        for draw in &self.draws {
            let page = &self.cache.pages[draw.page];
            let Some(buffer) = &page.buffer else {
                continue;
            };
            let [x, y, width, height] = draw.scissor;
            pass.set_scissor_rect(x, y, width, height);
            pass.set_bind_group(0, lane_bind_group, &[draw.uniform_offset]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..6, 0..page.instance_count);
        }
        if self.target_size_pixels[0] > 0 && self.target_size_pixels[1] > 0 {
            pass.set_scissor_rect(0, 0, self.target_size_pixels[0], self.target_size_pixels[1]);
        }
    }
}

/// The shader's token byte for a colour, for tests that compare the two token tables.
#[cfg(test)]
pub(crate) const fn ramp_flag() -> u8 {
    rgv_model::TRACK_COLOR_RAMP_FLAG
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::too_many_lines, clippy::format_push_string)]

    use super::*;

    fn lane() -> TrackLaneProjection {
        TrackLaneProjection {
            rect: [10.0, 100.0, 800.0, 60.0],
            camera_start: 1_000_000.25,
            points_per_base: 2.0,
            row_pitch: 20.0,
            row_height: 12.0,
            row_offset: 4.0,
            value_range: (0.0, 10.0),
            bin_style: TrackBinStyle::Bars,
        }
    }

    #[test]
    fn uniform_layout_matches_the_shader_struct() {
        assert_eq!(TRACK_TILE_INSTANCE_BYTES, 28);
        assert_eq!(TRACK_LANE_UNIFORM_BYTES, 80);
        assert_eq!(std::mem::offset_of!(LaneUniform, points_per_base), 32);
        assert_eq!(std::mem::offset_of!(LaneUniform, value_range), 48);
        assert_eq!(std::mem::offset_of!(LaneUniform, origin_whole), 56);
        assert_eq!(std::mem::offset_of!(LaneUniform, bin_style), 68);
        assert_eq!(size_of::<TrackPalette>(), 1_024);
        assert_eq!(ramp_flag(), 0x80);
        let source = shaders::TRACK_TILE.source();
        assert!(source.contains("array<vec4<f32>, 64>"));
        // Every shape the model defines has a rule in the shader, by discriminant.
        for (name, shape) in [
            ("SHAPE_ARROW_RIGHT", TrackSceneShape::ArrowRight),
            ("SHAPE_ARROW_LEFT", TrackSceneShape::ArrowLeft),
            ("SHAPE_LINE", TrackSceneShape::Line),
            (
                "SHAPE_LINE_CHEVRON_RIGHT",
                TrackSceneShape::LineChevronRight,
            ),
            ("SHAPE_LINE_CHEVRON_LEFT", TrackSceneShape::LineChevronLeft),
            ("SHAPE_ARC", TrackSceneShape::Arc),
            ("SHAPE_DIAMOND", TrackSceneShape::Diamond),
        ] {
            assert!(
                source.contains(&format!("const {name}: u32 = {}u;", shape as u8)),
                "{name}"
            );
        }
    }

    #[test]
    fn the_camera_is_split_relative_to_the_tile_start() {
        let lane = lane();
        let uniform = lane.uniform([1_000.0, 600.0], 999_424, None, Some(7));
        // 999,424 - 1,000,000.25 = -576.25 = -577 + 0.75.
        assert_eq!(
            (uniform.origin_whole, uniform.origin_fraction),
            (-577, 0.75)
        );
        assert_eq!((uniform.selected, uniform.hovered), (NO_HIGHLIGHT, 7));
        assert_eq!(uniform.points_per_base, [2.0, 0.0]);
    }

    #[test]
    fn projection_applies_rows_values_and_one_point_minimums() {
        let lane = lane();
        let tile = TrackTileKey {
            level: 0,
            index: 976,
        };
        assert_eq!(tile.start(), 999_424);
        let instance = |start, end, row, vertical, flags, y| {
            TrackSceneInstance::new(
                tile,
                start,
                end,
                row,
                TrackSceneShape::Box,
                vertical,
                flags,
                y,
                TrackColor::Token(TrackColorToken::FEATURE_FORWARD),
                0,
            )
        };
        let feature = project_track_instance(
            &instance(
                1_000_010,
                1_000_020,
                2,
                TrackSceneVertical::Row,
                0,
                (0.25, 0.75),
            ),
            tile.start(),
            &lane,
        );
        // x = 10 + (1,000,010 - 1,000,000.25) * 2; y = 100 + 4 + 2 * 20 + fraction * 12.
        assert_eq!(feature.bounds, [29.5, 147.0, 49.5, 153.0]);
        // A zero-length shape is one point wide, centred.
        let point = project_track_instance(
            &instance(
                1_000_010,
                1_000_010,
                0,
                TrackSceneVertical::Row,
                0,
                (0.0, 1.0),
            ),
            tile.start(),
            &lane,
        );
        assert_eq!((point.bounds[0], point.bounds[2]), (29.0, 30.0));
        // Values map through the lane range, top is high; a flat value is one point tall.
        let bar = project_track_instance(
            &instance(
                1_000_010,
                1_000_020,
                0,
                TrackSceneVertical::Value,
                0,
                (0.0, 5.0),
            ),
            tile.start(),
            &lane,
        );
        assert_eq!((bar.bounds[1], bar.bounds[3]), (130.0, 160.0));
        let mut line_lane = lane;
        line_lane.bin_style = TrackBinStyle::Line;
        let stepped = project_track_instance(
            &instance(
                1_000_010,
                1_000_020,
                0,
                TrackSceneVertical::Value,
                TRACK_SCENE_FLAG_BIN_MEAN,
                (0.0, 5.0),
            ),
            tile.start(),
            &line_lane,
        );
        assert_eq!((stepped.bounds[1], stepped.bounds[3]), (129.5, 130.5));
        // A shape reaching far outside the lane is rasterised clamped, with true bounds kept.
        let long = project_track_instance(
            &instance(0, 2_000_000, 0, TrackSceneVertical::Row, 0, (0.0, 1.0)),
            tile.start(),
            &lane,
        );
        assert_eq!((long.quad[0], long.quad[2]), (-54.0, 874.0));
        assert!(long.bounds[0] < -1_000_000.0 && long.bounds[2] > 1_000_000.0);
    }

    #[test]
    fn scene_points_invert_the_projection() {
        let lane = lane();
        let point = lane.scene_point(29.5, 147.0);
        assert!((point.position - 1_000_010.0).abs() < 1.0e-9);
        assert!((point.row - 2.15).abs() < 1.0e-6);
        assert!((point.lane_fraction - 47.0 / 60.0).abs() < 1.0e-6);
        assert!((lane.hit_tolerance_bases() - 0.25).abs() < 1.0e-12);
    }

    #[test]
    fn adjacent_tile_scissors_partition_the_lane() {
        let mut lane = lane();
        for (camera, points_per_base, scale) in [
            (1_000_000.25, 0.2, 1.0),
            (999_900.6, 0.37, 2.0),
            (1_000_300.1, 0.1913, 2.5),
        ] {
            lane.camera_start = camera;
            lane.points_per_base = points_per_base;
            let target = [(820.0 * scale) as u32, (600.0 * scale) as u32];
            let scissors = (960..=1_000)
                .filter_map(|index| {
                    lane.tile_scissor(TrackTileKey { level: 0, index }, scale, target)
                })
                .collect::<Vec<_>>();
            assert!(scissors.len() >= 2);
            for pair in scissors.windows(2) {
                assert_eq!(
                    pair[0][0] + pair[0][2],
                    pair[1][0],
                    "{camera} {points_per_base}"
                );
            }
            // Together they cover exactly the lane's columns.
            let first = scissors.first().unwrap();
            let last = scissors.last().unwrap();
            assert_eq!(first[0], (10.0 * scale).round() as u32);
            assert_eq!(last[0] + last[2], (810.0 * scale).round() as u32);
        }
    }

    #[test]
    fn every_theme_resolves_the_genotype_tokens_to_distinct_colours() {
        for theme in ThemeId::ALL {
            let palette = TrackPalette::for_theme(theme);
            let genotype = [
                TrackColorToken::GENOTYPE_HOM_REF,
                TrackColorToken::GENOTYPE_HET,
                TrackColorToken::GENOTYPE_HOM_ALT,
                TrackColorToken::GENOTYPE_NO_CALL,
            ]
            .map(|token| palette.color(token));
            for (index, left) in genotype.iter().enumerate() {
                for right in &genotype[index + 1..] {
                    let distance = left
                        .iter()
                        .zip(right)
                        .take(3)
                        .map(|(a, b)| (a - b) * (a - b))
                        .sum::<f32>()
                        .sqrt();
                    assert!(distance > 0.12, "{theme:?}: {left:?} vs {right:?}");
                }
            }
        }
        let palette = TrackPalette::light();
        let ramp = palette.resolve(TrackColor::Ramp {
            low: TrackColorToken::COPY_LOSS,
            position: 0.5,
        });
        let (low, high) = (
            palette.color(TrackColorToken::COPY_LOSS),
            palette.color(TrackColorToken::COPY_NEUTRAL),
        );
        assert!((ramp[0] - (low[0] + high[0]) * 0.5).abs() < 1.0e-6);
        assert_eq!(
            palette.resolve(TrackColor::Inline([255, 0, 51, 255])),
            [1.0, 0.0, 0.2, 1.0]
        );
    }
}
