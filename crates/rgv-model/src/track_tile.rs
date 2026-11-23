//! The public track contract, data side: tile-aligned requests, world-coordinate tiles with an
//! open set of payload kinds, and the world-space scene a tile is laid out into.
//!
//! A track source answers `query_tile(request, cancel)` (the trait lives in `rgv-engine`). The
//! request names one tile of a fixed power-of-two pyramid, so a result depends on the tile and
//! the settings only, never on the exact viewport, and can be cached and shared between panels.
//! The engine lays a tile out once, in genomic coordinates, into a [`TrackTileScene`]; the
//! renderer pages scenes onto the GPU and pans them with a camera uniform.
//!
//! # Adding a payload kind
//!
//! Nothing outside the code that produces and lays out the new kind has to change:
//!
//! 1. the source returns [`TrackTilePayload::Custom`] with its own [`TrackRendererId`] and a
//!    type-erased payload;
//! 2. a layout function for that id is registered with the engine's layout registry; it
//!    downcasts the payload and returns a [`TrackTileScene`] built from the shared instance
//!    vocabulary ([`TrackSceneShape`], [`TrackColor`]).
//!
//! The engine, the cache, the renderer and the hit test consume only the scene and
//! [`TrackTilePayload::renderer_id`] / [`TrackTilePayload::resident_bytes`], which are the only
//! two matches over the payload set and both live in this file.

use std::{
    any::Any,
    borrow::Cow,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use rgv_core::{CopyNumberTrackSettings, InteractionTrackSettings, VariantTrackSettings};

use crate::tracks::{FeatureRecord, Strand, VariantRecord};

/// Bases spanned by a level-0 tile; a level-`n` tile spans `TRACK_TILE_BASE_SPAN << n`.
pub const TRACK_TILE_BASE_SPAN: u64 = 1_024;
/// Points a tile covers when it is drawn at its level's coarsest resolution. A tile is drawn at
/// between this many and twice this many points across.
pub const TRACK_TILE_NOMINAL_POINTS: u32 = 1_024;
/// Coarsest pyramid level. A level-19 tile spans 536,870,912 bases, which keeps every
/// tile-local offset inside the renderer's exact integer projection range (7e8 bases).
pub const MAXIMUM_TRACK_TILE_LEVEL: u8 = 19;
/// Regular bins of a quantitative tile: one per nominal point.
pub const TRACK_TILE_BINS: usize = 1_024;

/// Default density above which a feature tile is delivered as a coverage summary instead of
/// individual features: mean features per nominal point of the tile (so more than 512 features
/// in a 1,024-point tile). Sources read the effective value from
/// [`TrackTileSettings::feature_summary_features_per_point`].
pub const DEFAULT_FEATURE_SUMMARY_FEATURES_PER_POINT: f32 = 0.5;
/// Default density above which a variant tile is delivered as a count summary: mean variants
/// per nominal point (more than 1,024 variants in a tile).
pub const DEFAULT_VARIANT_SUMMARY_VARIANTS_PER_POINT: f32 = 1.0;
/// Default of [`TrackTileSettings::maximum_feature_rows`]: the rows the legacy feature layout
/// packs before it marks an overflow (`rgv_layout::TrackLayoutOptions::default().maximum_rows`).
pub const DEFAULT_TRACK_TILE_FEATURE_ROWS: u32 = 12;

/// One tile of the track pyramid: `index` counts tiles of `1024 << level` bases from zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackTileKey {
    pub level: u8,
    pub index: u64,
}

impl TrackTileKey {
    /// Level whose bins are at least one point wide at `bases_per_point`: the smallest `n`
    /// with `2^n >= bases_per_point`, clamped to the pyramid.
    #[must_use]
    pub fn level_for_bases_per_point(bases_per_point: f64) -> u8 {
        if !bases_per_point.is_finite() || bases_per_point <= 1.0 {
            return 0;
        }
        let mut level = 0_u8;
        while level < MAXIMUM_TRACK_TILE_LEVEL && f64::from(1_u32 << level) < bases_per_point {
            level += 1;
        }
        level
    }

    #[must_use]
    pub const fn span_at(level: u8) -> u64 {
        let level = if level > MAXIMUM_TRACK_TILE_LEVEL {
            MAXIMUM_TRACK_TILE_LEVEL
        } else {
            level
        };
        TRACK_TILE_BASE_SPAN << level
    }

    #[must_use]
    pub const fn span(self) -> u64 {
        Self::span_at(self.level)
    }

    /// Zero-based inclusive start.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.index.saturating_mul(self.span())
    }

    /// Zero-based exclusive end.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.start().saturating_add(self.span())
    }

    /// Bases per point this level is laid out for; also the width of one quantitative bin.
    #[must_use]
    pub const fn nominal_bases_per_point(self) -> u64 {
        self.span() / TRACK_TILE_NOMINAL_POINTS as u64
    }

    #[must_use]
    pub const fn containing(level: u8, position: u64) -> Self {
        Self {
            level,
            index: position / Self::span_at(level),
        }
    }

    /// Tiles of `level` that intersect `[start, end)`, left to right. Empty for an empty
    /// interval.
    pub fn covering(level: u8, start: u64, end: u64) -> impl Iterator<Item = Self> {
        let span = Self::span_at(level);
        let first = start / span;
        let last = if end > start {
            (end - 1) / span + 1
        } else {
            first
        };
        (first..last).map(move |index| Self { level, index })
    }
}

/// Cooperative cancellation handed to a source for one operation. Cheap to clone and to poll;
/// a source should poll it between records or blocks and return [`TrackTileError::Cancelled`].
#[derive(Clone)]
pub struct TrackCancellation {
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl TrackCancellation {
    /// `cancelled` returns true once the operation's result is no longer wanted.
    pub fn new(cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            cancelled: Arc::new(cancelled),
        }
    }

    /// Never cancelled.
    #[must_use]
    pub fn never() -> Self {
        Self::new(|| false)
    }

    /// Cancelled once `flag` is set.
    #[must_use]
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self::new(move || flag.load(Ordering::Acquire))
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        (self.cancelled)()
    }

    /// `Err(Cancelled)` once cancelled; for use with `?` inside source loops.
    pub fn check(&self) -> Result<(), TrackTileError> {
        if self.is_cancelled() {
            Err(TrackTileError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for TrackCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrackCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TrackTileError {
    /// The request was abandoned; nothing is published or cached.
    #[error("the track tile request was cancelled")]
    Cancelled,
    #[error("{0}")]
    Source(String),
    /// No layout function is registered for the payload's renderer id.
    #[error("no track layout is registered for renderer {0}")]
    UnknownRenderer(String),
}

/// Most a source may transfer from a remote origin for one tile. Local sources ignore it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrackTransferBudget {
    pub maximum_requests: u64,
    pub maximum_response_bytes: u64,
}

/// Per-track settings that change what a tile contains. A source reads only what applies to
/// its kind; `custom` carries plugin settings as ordered key/value pairs.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackTileSettings {
    pub variant: Option<Arc<VariantTrackSettings>>,
    pub interaction: Option<Arc<InteractionTrackSettings>>,
    pub copy_number: Option<Arc<CopyNumberTrackSettings>>,
    /// See [`DEFAULT_FEATURE_SUMMARY_FEATURES_PER_POINT`]. Non-finite or non-positive values
    /// disable the summary and always deliver individual features.
    pub feature_summary_features_per_point: f32,
    /// See [`DEFAULT_VARIANT_SUMMARY_VARIANTS_PER_POINT`].
    pub variant_summary_variants_per_point: f32,
    /// Deliver the per-sample genotype matrix with variant tiles when the source has samples.
    pub genotype_matrix: bool,
    pub custom: Vec<(String, String)>,
    /// Most packed feature rows a tile's scene draws; see [`TrackTileScene::apply_row_cap`].
    /// Rows from this one on are left out of the scene's instances and labels and counted in
    /// [`TrackTileScene::hidden_rows`] and [`TrackTileScene::hidden_features`]. Zero and
    /// `u32::MAX` mean no cap. Default [`DEFAULT_TRACK_TILE_FEATURE_ROWS`].
    pub maximum_feature_rows: u32,
}

impl Default for TrackTileSettings {
    fn default() -> Self {
        Self {
            variant: None,
            interaction: None,
            copy_number: None,
            feature_summary_features_per_point: DEFAULT_FEATURE_SUMMARY_FEATURES_PER_POINT,
            variant_summary_variants_per_point: DEFAULT_VARIANT_SUMMARY_VARIANTS_PER_POINT,
            genotype_matrix: true,
            custom: Vec::new(),
            maximum_feature_rows: DEFAULT_TRACK_TILE_FEATURE_ROWS,
        }
    }
}

impl TrackTileSettings {
    /// These settings with another feature row cap; see [`Self::maximum_feature_rows`].
    #[must_use]
    pub fn with_maximum_feature_rows(mut self, rows: u32) -> Self {
        self.maximum_feature_rows = rows;
        self
    }

    /// Stable digest of everything that can change a tile; part of the tile cache key.
    #[must_use]
    pub fn digest(&self) -> u64 {
        let mut digest = TrackDigest::new();
        digest.write(format!("{self:?}").as_bytes());
        digest.finish()
    }
}

/// One tile request. `bases_per_point` is the resolution hint: the engine passes the tile
/// level's nominal value (a power of two), so equal requests produce equal tiles.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackTileRequest {
    /// Contig name as the source spells it.
    pub contig: String,
    pub tile: TrackTileKey,
    pub bases_per_point: f64,
    pub settings: TrackTileSettings,
    pub transfer_budget: Option<TrackTransferBudget>,
}

impl TrackTileRequest {
    /// A request for `tile` at its level's nominal resolution.
    #[must_use]
    pub fn new(contig: impl Into<String>, tile: TrackTileKey, settings: TrackTileSettings) -> Self {
        Self {
            contig: contig.into(),
            tile,
            #[allow(clippy::cast_precision_loss)]
            bases_per_point: tile.nominal_bases_per_point() as f64,
            settings,
            transfer_budget: None,
        }
    }

    /// Tile-aligned, zero-based, half-open interval to answer.
    #[must_use]
    pub const fn interval(&self) -> (u64, u64) {
        (self.tile.start(), self.tile.end())
    }

    /// Points the tile covers at the requested resolution.
    #[must_use]
    pub fn tile_points(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let span = self.tile.span() as f64;
        span / self.bases_per_point.max(f64::MIN_POSITIVE)
    }
}

/// Identity of one record that survives re-queries, tile seams and level changes: a digest of
/// the record's own coordinates and names, never an index into a result.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackRecordId(pub u64);

impl TrackRecordId {
    #[must_use]
    pub fn for_feature(record: &FeatureRecord) -> Self {
        let mut digest = TrackDigest::new();
        digest.write_u64(record.start);
        digest.write_u64(record.end);
        digest.write(&[match record.strand {
            Strand::Forward => 1,
            Strand::Reverse => 2,
            Strand::Unstranded => 0,
        }]);
        digest.write_text(record.name.as_deref());
        digest.write_text(record.feature_type.as_deref());
        let mut named = false;
        for (key, value) in &record.attributes {
            if key.eq_ignore_ascii_case("ID") || key.eq_ignore_ascii_case("transcript_id") {
                digest.write_text(Some(value));
                named = true;
            }
        }
        // A record named by `ID` or `transcript_id` is that name over that extent, however
        // many of its blocks were read: a transcript assembled from a window of an indexed
        // GTF or GFF3 file that holds only some of its exons is still the same transcript.
        if !named {
            digest.write_u64(record.blocks.len() as u64);
        }
        Self(digest.finish())
    }

    /// Identity of a variant: its interval, identifier and alleles. This is the identity
    /// variant tiles give their records.
    #[must_use]
    pub fn for_variant(record: &VariantRecord) -> Self {
        let alternates = record.alternates.join(&b","[..]);
        Self::from_parts(
            record.start,
            record.end,
            &[
                record.id.as_deref().unwrap_or_default().as_bytes(),
                &record.reference,
                &alternates,
            ],
        )
    }

    /// Identity from coordinates and any distinguishing words (allele strings, sample index…).
    #[must_use]
    pub fn from_parts(start: u64, end: u64, parts: &[&[u8]]) -> Self {
        let mut digest = TrackDigest::new();
        digest.write_u64(start);
        digest.write_u64(end);
        for part in parts {
            digest.write_u64(part.len() as u64);
            digest.write(part);
        }
        Self(digest.finish())
    }
}

/// A complete source record behind a tile's [`TrackRecordId`]: the record types the legacy
/// queries return and the inspector consumes. Tiles carry only what is drawn; this is what
/// `TrackTileEngine::lookup_record` resolves an identity to.
// A feature is the common case and is resolved one at a time behind an `Arc`; boxing it
// would cost an allocation per lookup for no saving.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TrackRecord {
    Feature(FeatureRecord),
    Variant(Box<VariantRecord>),
}

impl TrackRecord {
    /// The identity tiles give this record.
    #[must_use]
    pub fn id(&self) -> TrackRecordId {
        match self {
            Self::Feature(record) => TrackRecordId::for_feature(record),
            Self::Variant(record) => TrackRecordId::for_variant(record),
        }
    }

    #[must_use]
    pub const fn as_feature(&self) -> Option<&FeatureRecord> {
        match self {
            Self::Feature(record) => Some(record),
            Self::Variant(_) => None,
        }
    }

    #[must_use]
    pub fn as_variant(&self) -> Option<&VariantRecord> {
        match self {
            Self::Variant(record) => Some(record),
            Self::Feature(_) => None,
        }
    }

    /// Zero-based half-open interval of the record.
    #[must_use]
    pub fn interval(&self) -> (u64, u64) {
        match self {
            Self::Feature(record) => (record.start, record.end),
            Self::Variant(record) => (record.start, record.end),
        }
    }

    /// Approximate heap bytes retained, for byte budgets.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        let text = |text: &Option<String>| text.as_ref().map_or(0, String::capacity);
        match self {
            Self::Feature(record) => {
                size_of::<Self>()
                    + record.contig.capacity()
                    + text(&record.name)
                    + text(&record.feature_type)
                    + record.blocks.capacity() * size_of::<(u64, u64)>()
                    + record
                        .attributes
                        .iter()
                        .map(|(key, value)| {
                            size_of::<(String, String)>() + key.capacity() + value.capacity()
                        })
                        .sum::<usize>()
            }
            // Variant records are not retained with tiles; a flat estimate is enough for the
            // small resolved-record cache.
            Self::Variant(record) => {
                size_of::<Self>()
                    + size_of::<VariantRecord>()
                    + record.contig.capacity()
                    + text(&record.id)
                    + record.reference.capacity()
                    + record.alternates.iter().map(Vec::capacity).sum::<usize>()
            }
        }
    }
}

/// What a source is asked when a record identity is resolved to its complete record: the
/// record's own interval (known from the tile that showed it) bounds the read.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRecordQuery {
    /// Contig name as the source spells it.
    pub contig: String,
    pub id: TrackRecordId,
    /// The record's interval, zero-based half-open.
    pub start: u64,
    pub end: u64,
    /// Settings of the view the record was shown in.
    pub settings: TrackTileSettings,
}

/// State of a record lookup (`TrackTileEngine::lookup_record`).
#[derive(Clone, Debug, PartialEq)]
pub enum TrackRecordLookup {
    /// The complete record.
    Ready(Arc<TrackRecord>),
    /// A bounded re-read of the source is scheduled or running; ask again (the UI is woken
    /// when it finishes).
    Pending,
    /// No cached tile shows the identity, or the source no longer has a record with it.
    NotFound,
    /// The re-read failed.
    Failed(String),
}

impl TrackRecordLookup {
    /// The record, when the lookup is ready.
    #[must_use]
    pub fn record(&self) -> Option<&TrackRecord> {
        match self {
            Self::Ready(record) => Some(record),
            Self::Pending | Self::NotFound | Self::Failed(_) => None,
        }
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// 64-bit FNV-1a; stable across runs and platforms, which `std`'s hashers are not.
#[derive(Clone, Copy, Debug)]
pub struct TrackDigest(u64);

impl TrackDigest {
    #[must_use]
    pub const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    pub fn write_text(&mut self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.write_u64(text.len() as u64 + 1);
                self.write(text.as_bytes());
            }
            None => self.write_u64(0),
        }
    }

    #[must_use]
    pub const fn finish(self) -> u64 {
        self.0
    }
}

impl Default for TrackDigest {
    fn default() -> Self {
        Self::new()
    }
}

/// Slot of the track palette. The renderer resolves a token against the active theme, so tiles
/// and GPU pages never hold theme colours.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackColorToken(u8);

impl TrackColorToken {
    /// Slot 0 is the inline-colour escape and never a palette colour.
    pub const INLINE: Self = Self(0);
    pub const FEATURE_FORWARD: Self = Self(1);
    pub const FEATURE_REVERSE: Self = Self(2);
    pub const FEATURE_UNSTRANDED: Self = Self(3);
    /// Intron lines and other connectors.
    pub const FEATURE_CONNECTOR: Self = Self(4);
    pub const BIN_MEAN: Self = Self(5);
    pub const BIN_ENVELOPE: Self = Self(6);
    /// Feature or variant density summaries.
    pub const DENSITY: Self = Self(7);
    pub const VARIANT_SNV: Self = Self(8);
    pub const VARIANT_INSERTION: Self = Self(9);
    pub const VARIANT_DELETION: Self = Self(10);
    pub const VARIANT_STRUCTURAL: Self = Self(11);
    pub const VARIANT_OTHER: Self = Self(12);
    pub const GENOTYPE_HOM_REF: Self = Self(13);
    pub const GENOTYPE_HET: Self = Self(14);
    pub const GENOTYPE_HOM_ALT: Self = Self(15);
    pub const GENOTYPE_NO_CALL: Self = Self(16);
    pub const INTERACTION_ARC: Self = Self(17);
    pub const INTERACTION_ANCHOR: Self = Self(18);
    /// Low end of the score ramp; the next slot is its high end.
    pub const SCORE_LOW: Self = Self(19);
    pub const SCORE_HIGH: Self = Self(20);
    /// Copy-number ramp: loss, neutral, gain. A ramp runs from a slot to the next one.
    pub const COPY_LOSS: Self = Self(21);
    pub const COPY_NEUTRAL: Self = Self(22);
    pub const COPY_GAIN: Self = Self(23);
    pub const ASSOCIATION: Self = Self(24);
    pub const MUTATION: Self = Self(25);
    /// One past the last assigned slot.
    pub const END: Self = Self(26);

    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Token for a slot index; `None` outside the assigned range.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        if index < Self::END.0 {
            Some(Self(index))
        } else {
            None
        }
    }
}

/// Entries of the track palette uniform.
pub const TRACK_PALETTE_ENTRIES: usize = 64;
/// Bit of an instance's token byte that selects a ramp from the slot to the next one.
pub const TRACK_COLOR_RAMP_FLAG: u8 = 0x80;

/// Colour of one instance: a palette slot, a ramp between two adjacent slots, or (for colours
/// the data file itself specifies) an inline RGBA value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackColor {
    Token(TrackColorToken),
    /// `position` 0 is `low`, 1 is the slot after it.
    Ramp {
        low: TrackColorToken,
        position: f32,
    },
    Inline([u8; 4]),
}

impl TrackColor {
    /// Token byte and parameter word as stored in a [`TrackSceneInstance`].
    #[must_use]
    pub const fn encode(self) -> (u8, u32) {
        match self {
            Self::Token(token) => (token.0, 0),
            Self::Ramp { low, position } => (low.0 | TRACK_COLOR_RAMP_FLAG, position.to_bits()),
            Self::Inline(rgba) => (0, u32::from_le_bytes(rgba)),
        }
    }

    #[must_use]
    pub const fn decode(token: u8, parameter: u32) -> Self {
        let slot = token & !TRACK_COLOR_RAMP_FLAG;
        if slot == 0 {
            Self::Inline(parameter.to_le_bytes())
        } else if token & TRACK_COLOR_RAMP_FLAG != 0 {
            Self::Ramp {
                low: TrackColorToken(slot),
                position: f32::from_bits(parameter),
            }
        } else {
            Self::Token(TrackColorToken(slot))
        }
    }
}

/// Names the layout (and, for custom kinds, the producer) of a payload.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackRendererId(Cow<'static, str>);

impl TrackRendererId {
    pub const FEATURES: Self = Self(Cow::Borrowed("rgv.features"));
    pub const BINS: Self = Self(Cow::Borrowed("rgv.bins"));
    pub const VARIANTS: Self = Self(Cow::Borrowed("rgv.variants"));
    pub const INTERACTIONS: Self = Self(Cow::Borrowed("rgv.interactions"));

    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(Cow::Owned(id.into()))
    }

    #[must_use]
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TrackRendererId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether a tile holds individual records or a summary chosen by level of detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackTileLod {
    Detail,
    /// Records were too dense for the tile's resolution and were replaced by a summary.
    Summary,
}

/// How rows of a feature tile were assigned.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TrackRowStability {
    /// Not applicable (no packed rows).
    #[default]
    NotPacked,
    /// Every feature's whole overlap cluster was read, so its row is the same in every tile.
    SeamStable,
    /// The cluster was larger than the read-ahead bound; rows are deterministic for this tile
    /// but a feature may change row at a tile seam.
    Bounded,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackTileProvenance {
    pub records_examined: usize,
    /// The source stopped at a record cap; the tile is incomplete and says so.
    pub records_truncated: bool,
    /// Bases per source record when a precomputed zoom level was read.
    pub source_reduction_bases: Option<u32>,
    pub row_stability: TrackRowStability,
    /// Genotype cells the source did not load (its own per-query cap).
    pub genotype_cells_omitted: usize,
}

/// One world-coordinate tile.
#[derive(Clone, Debug)]
pub struct TrackTile {
    pub contig: String,
    pub key: TrackTileKey,
    pub lod: TrackTileLod,
    pub payload: TrackTilePayload,
    pub provenance: TrackTileProvenance,
}

impl TrackTile {
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.contig.capacity())
            .saturating_add(self.payload.resident_bytes())
    }
}

/// The open payload set. `Custom` is the extension point; see the module documentation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum TrackTilePayload {
    Features(FeatureTile),
    Bins(BinTile),
    Variants(VariantTile),
    Interactions(InteractionTile),
    Custom(CustomTile),
}

impl TrackTilePayload {
    #[must_use]
    pub fn renderer_id(&self) -> TrackRendererId {
        match self {
            Self::Features(_) => TrackRendererId::FEATURES,
            Self::Bins(_) => TrackRendererId::BINS,
            Self::Variants(_) => TrackRendererId::VARIANTS,
            Self::Interactions(_) => TrackRendererId::INTERACTIONS,
            Self::Custom(custom) => custom.renderer.clone(),
        }
    }

    /// Heap bytes retained by the payload, for the tile cache's byte budget.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        let text = |text: &Option<Arc<str>>| text.as_ref().map_or(0, |text| text.len());
        match self {
            Self::Features(tile) => tile
                .features
                .iter()
                .map(|feature| {
                    size_of::<TileFeature>()
                        + feature.blocks.capacity() * size_of::<TileFeatureBlock>()
                        + text(&feature.name)
                })
                .sum::<usize>()
                .saturating_add(match &tile.rows {
                    FeatureRowAxis::Fixed { labels } => labels.iter().map(String::len).sum(),
                    FeatureRowAxis::Packed | FeatureRowAxis::Scatter { .. } => 0,
                }),
            Self::Bins(tile) => tile.bins.capacity() * size_of::<TileBin>(),
            Self::Variants(tile) => tile
                .variants
                .iter()
                .map(|variant| size_of::<TileVariant>() + text(&variant.name))
                .sum::<usize>()
                .saturating_add(tile.genotypes.as_ref().map_or(0, |matrix| {
                    matrix.calls.capacity()
                        + matrix.loaded.capacity()
                        + matrix.samples.iter().map(String::len).sum::<usize>()
                })),
            Self::Interactions(tile) => tile
                .interactions
                .iter()
                .map(|interaction| size_of::<TileInteraction>() + text(&interaction.name))
                .sum(),
            Self::Custom(custom) => custom.resident_bytes,
        }
    }
}

/// How the rows of a feature tile are defined.
#[derive(Clone, Debug, PartialEq)]
pub enum FeatureRowAxis {
    /// Rows are packed so overlapping features do not collide; `TileFeature::row_hint` holds
    /// the seam-stable row when the source computed one.
    Packed,
    /// One named row per sample or series; `row_hint` is the row.
    Fixed { labels: Arc<[String]> },
    /// No rows: features are points placed vertically by `score` within the value range.
    Scatter { minimum: f32, maximum: f32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileFeatureBlock {
    pub start: u64,
    pub end: u64,
    /// Thick (coding) block; thin blocks are drawn at half height.
    pub thick: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileFeature {
    pub id: TrackRecordId,
    /// Full genomic extent, not clipped to the tile.
    pub start: u64,
    pub end: u64,
    pub strand: Strand,
    /// Blocks intersecting the tile, in genomic order. Empty for an unstructured feature.
    pub blocks: Vec<TileFeatureBlock>,
    pub name: Option<Arc<str>>,
    pub score: Option<f32>,
    /// `None` colours by strand.
    pub color: Option<TrackColor>,
    pub row_hint: Option<u32>,
    /// Whether a label may be drawn for this feature (sub-features of a transcript are not
    /// labelled).
    pub labelable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeatureTile {
    pub features: Vec<TileFeature>,
    pub rows: FeatureRowAxis,
}

/// What the values of a bin tile mean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinSemantics {
    /// A quantitative signal.
    Signal,
    /// Features overlapping each bin (level-of-detail summary of a feature tile).
    FeatureDensity,
    /// Variants starting in each bin (level-of-detail summary of a variant tile).
    VariantDensity,
}

/// Range of the values behind one bin. A bin with no data has a NaN `mean`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileBin {
    pub minimum: f32,
    pub mean: f32,
    pub maximum: f32,
}

impl TileBin {
    pub const EMPTY: Self = Self {
        minimum: f32::NAN,
        mean: f32::NAN,
        maximum: f32::NAN,
    };

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.mean.is_nan()
    }
}

/// Regular bins starting at the tile start, each `bin_width` bases.
#[derive(Clone, Debug, PartialEq)]
pub struct BinTile {
    pub bin_width: u64,
    pub bins: Vec<TileBin>,
    pub semantics: BinSemantics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileVariantKind {
    Snv,
    Insertion,
    Deletion,
    Structural,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileVariant {
    pub id: TrackRecordId,
    pub start: u64,
    pub end: u64,
    pub kind: TileVariantKind,
    pub name: Option<Arc<str>>,
    pub quality: Option<f32>,
}

/// Diploid-style genotype class of one sample at one variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GenotypeCall {
    HomRef = 0,
    Het = 1,
    HomAlt = 2,
    NoCall = 3,
}

impl GenotypeCall {
    /// Class of a genotype from its allele indices (`None` is a missing allele). Any missing
    /// allele or an empty genotype is a no-call; all-reference is hom-ref; a single distinct
    /// non-reference allele in every position is hom-alt; everything else is het.
    #[must_use]
    pub fn classify(alleles: impl IntoIterator<Item = Option<u32>>) -> Self {
        let mut first = None;
        let mut mixed = false;
        for allele in alleles {
            let Some(allele) = allele else {
                return Self::NoCall;
            };
            match first {
                None => first = Some(allele),
                Some(first) if first != allele => mixed = true,
                Some(_) => {}
            }
        }
        match (first, mixed) {
            (None, _) => Self::NoCall,
            (Some(_), true) => Self::Het,
            (Some(0), false) => Self::HomRef,
            (Some(_), false) => Self::HomAlt,
        }
    }
}

/// Compact per-sample genotype classes: two bits per cell, variant-major.
#[derive(Clone, Debug, PartialEq)]
pub struct GenotypeMatrix {
    pub samples: Arc<[String]>,
    variant_count: usize,
    /// `variant_count * samples.len()` two-bit cells, four per byte.
    calls: Vec<u8>,
    /// One flag per variant: whether the source delivered genotype cells for it. Cells of a
    /// variant that was not loaded are not drawn (they are unknown, not no-calls).
    loaded: Vec<bool>,
}

impl GenotypeMatrix {
    /// Every cell starts as a no-call and every variant as not loaded.
    #[must_use]
    pub fn new(samples: Arc<[String]>, variant_count: usize) -> Self {
        let cells = variant_count.saturating_mul(samples.len());
        Self {
            samples,
            variant_count,
            calls: vec![0xFF; cells.div_ceil(4)],
            loaded: vec![false; variant_count],
        }
    }

    #[must_use]
    pub const fn variant_count(&self) -> usize {
        self.variant_count
    }

    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    pub fn set(&mut self, variant: usize, sample: usize, call: GenotypeCall) {
        if variant >= self.variant_count || sample >= self.samples.len() {
            return;
        }
        self.loaded[variant] = true;
        let slot = variant * self.samples.len() + sample;
        let shift = (slot % 4) * 2;
        let byte = &mut self.calls[slot / 4];
        *byte = (*byte & !(0b11 << shift)) | ((call as u8) << shift);
    }

    #[must_use]
    pub fn call(&self, variant: usize, sample: usize) -> Option<GenotypeCall> {
        if variant >= self.variant_count || sample >= self.samples.len() || !self.loaded[variant] {
            return None;
        }
        let slot = variant * self.samples.len() + sample;
        Some(match (self.calls[slot / 4] >> ((slot % 4) * 2)) & 0b11 {
            0 => GenotypeCall::HomRef,
            1 => GenotypeCall::Het,
            2 => GenotypeCall::HomAlt,
            _ => GenotypeCall::NoCall,
        })
    }

    #[must_use]
    pub fn is_loaded(&self, variant: usize) -> bool {
        self.loaded.get(variant).copied().unwrap_or(false)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantTile {
    /// Sorted by start.
    pub variants: Vec<TileVariant>,
    /// Row `v` of the matrix belongs to `variants[v]`.
    pub genotypes: Option<GenotypeMatrix>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TileInteraction {
    pub id: TrackRecordId,
    /// Anchor on this tile's contig.
    pub source: (u64, u64),
    /// Other anchor; `None` when it lies on another contig.
    pub target: Option<(u64, u64)>,
    pub score: Option<f32>,
    pub color: Option<TrackColor>,
    pub name: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractionTile {
    pub interactions: Vec<TileInteraction>,
}

/// A payload kind this crate does not know. `renderer` selects the registered layout function,
/// which downcasts `payload`.
#[derive(Clone)]
pub struct CustomTile {
    pub renderer: TrackRendererId,
    pub payload: Arc<dyn Any + Send + Sync>,
    /// Bytes the payload retains, for the tile cache's budget.
    pub resident_bytes: usize,
}

impl fmt::Debug for CustomTile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CustomTile")
            .field("renderer", &self.renderer)
            .field("resident_bytes", &self.resident_bytes)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------------------------
// World-space scene
// ---------------------------------------------------------------------------------------------

/// Shape of one scene instance. The discriminants are the values the track shader switches on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TrackSceneShape {
    Box = 0,
    /// Box whose right end is a point (forward strand).
    ArrowRight = 1,
    /// Box whose left end is a point (reverse strand).
    ArrowLeft = 2,
    /// One-point line through the vertical centre (intron).
    Line = 3,
    /// As `Line`, with chevrons pointing right or left along it.
    LineChevronRight = 4,
    LineChevronLeft = 5,
    /// Parabolic arc stroke joining the instance's two ends, apex at `y0`, feet at `y1`.
    Arc = 6,
    /// Diamond centred in the instance (point features).
    Diamond = 7,
}

impl TrackSceneShape {
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::Box,
            1 => Self::ArrowRight,
            2 => Self::ArrowLeft,
            3 => Self::Line,
            4 => Self::LineChevronRight,
            5 => Self::LineChevronLeft,
            6 => Self::Arc,
            7 => Self::Diamond,
            _ => return None,
        })
    }
}

/// What an instance's `y0`/`y1` mean. Stored in the two low bits of the flags byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TrackSceneVertical {
    /// Fractions of the instance's row (0 top, 1 bottom).
    Row = 0,
    /// Data values, mapped through the lane's value range at draw time (so autoscaling never
    /// re-lays-out a tile). `y0 == y1` draws a one-point line at that value.
    Value = 1,
    /// Fractions of the whole lane (0 top, 1 bottom).
    Lane = 2,
}

/// Flag bit: the instance is the mean of a bin, which the "line" bin style collapses to a
/// one-point line at draw time.
pub const TRACK_SCENE_FLAG_BIN_MEAN: u8 = 0b0000_0100;
/// Flag bit: draw at fixed point size centred on the instance (point features), instead of
/// stretching with the zoom.
pub const TRACK_SCENE_FLAG_POINT: u8 = 0b0000_1000;

/// Record word value of an instance that belongs to no record.
pub const TRACK_SCENE_NO_RECORD: u32 = 0x00FF_FFFF;
/// Largest magnitude of a tile-local base offset: the largest multiple of 4,096 below the
/// 7e8-base bound of the shader's integer projection, so that both words the shader splits an
/// offset into stay inside that bound.
pub const MAXIMUM_TRACK_SCENE_OFFSET: i64 = 699_998_208;

/// One world-space shape: 28 bytes, uploaded to the GPU unchanged.
///
/// Horizontal position is a whole number of bases relative to the tile start (negative for a
/// shape that begins left of the tile), so a tile is laid out once and stays valid for every
/// camera. A shape is *not* clipped to its tile: each tile that the shape intersects holds the
/// whole shape, and the renderer scissors every tile to its own pixel columns, so adjacent
/// tiles draw disjoint parts of one identical projection.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct TrackSceneInstance {
    /// Start, in bases from the tile start.
    pub start: i32,
    /// Length in bases; zero-length shapes are drawn at the one-point minimum width.
    pub length: u32,
    pub y0: f32,
    pub y1: f32,
    /// `row | shape << 16 | flags << 24`.
    pub layout: u32,
    /// Colour parameter: inline RGBA or the f32 bits of a ramp position.
    pub parameter: u32,
    /// `record index | colour token << 24`. The index refers to [`TrackTileScene::records`]
    /// (or is a bin index for bin scenes) and is [`TRACK_SCENE_NO_RECORD`] for decorations.
    pub record: u32,
}

impl TrackSceneInstance {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        tile: TrackTileKey,
        start: u64,
        end: u64,
        row: u16,
        shape: TrackSceneShape,
        vertical: TrackSceneVertical,
        flags: u8,
        (y0, y1): (f32, f32),
        color: TrackColor,
        record: u32,
    ) -> Self {
        let clamp = |position: u64| {
            #[allow(clippy::cast_possible_wrap)]
            let delta = i128::from(position) - i128::from(tile.start());
            delta.clamp(
                -i128::from(MAXIMUM_TRACK_SCENE_OFFSET),
                i128::from(MAXIMUM_TRACK_SCENE_OFFSET),
            ) as i64
        };
        let local_start = clamp(start);
        let local_end = clamp(end.max(start));
        let (token, parameter) = color.encode();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self {
            start: local_start as i32,
            length: (local_end - local_start) as u32,
            y0,
            y1,
            layout: u32::from(row)
                | (u32::from(shape as u8) << 16)
                | (u32::from((flags & !0b11) | vertical as u8) << 24),
            parameter,
            record: (record & TRACK_SCENE_NO_RECORD) | (u32::from(token) << 24),
        }
    }

    #[must_use]
    pub const fn row(&self) -> u16 {
        (self.layout & 0xFFFF) as u16
    }

    #[must_use]
    pub const fn shape(&self) -> Option<TrackSceneShape> {
        TrackSceneShape::from_index(((self.layout >> 16) & 0xFF) as u8)
    }

    #[must_use]
    pub const fn flags(&self) -> u8 {
        (self.layout >> 24) as u8
    }

    #[must_use]
    pub const fn vertical(&self) -> TrackSceneVertical {
        match self.flags() & 0b11 {
            1 => TrackSceneVertical::Value,
            2 => TrackSceneVertical::Lane,
            _ => TrackSceneVertical::Row,
        }
    }

    /// Record (or bin) index; `None` for a decoration.
    #[must_use]
    pub const fn record_index(&self) -> Option<u32> {
        let index = self.record & TRACK_SCENE_NO_RECORD;
        if index == TRACK_SCENE_NO_RECORD {
            None
        } else {
            Some(index)
        }
    }

    #[must_use]
    pub const fn color(&self) -> TrackColor {
        TrackColor::decode((self.record >> 24) as u8, self.parameter)
    }

    /// The seven little-endian words uploaded to the GPU.
    #[must_use]
    pub const fn to_words(&self) -> [u32; 7] {
        #[allow(clippy::cast_sign_loss)]
        [
            self.start as u32,
            self.length,
            self.y0.to_bits(),
            self.y1.to_bits(),
            self.layout,
            self.parameter,
            self.record,
        ]
    }
}

/// One record of a scene, for hit-testing, selection and inspection.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSceneRecord {
    pub id: TrackRecordId,
    pub start: u64,
    pub end: u64,
    pub row: u32,
    pub name: Option<Arc<str>>,
}

/// A label anchored in genomic space. Whether and where it is drawn is decided at draw time
/// from the camera (`rgv_layout::place_track_labels`); nothing about it is in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSceneLabel {
    /// Index into [`TrackTileScene::records`].
    pub record: u32,
    /// Genomic extent the label is centred on.
    pub start: u64,
    pub end: u64,
    pub row: u32,
    pub text: Arc<str>,
}

/// A tile laid out once in world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackTileScene {
    pub contig: String,
    pub key: TrackTileKey,
    pub renderer: TrackRendererId,
    pub lod: TrackTileLod,
    /// In draw order.
    pub instances: Vec<TrackSceneInstance>,
    pub records: Vec<TrackSceneRecord>,
    pub labels: Vec<TrackSceneLabel>,
    /// Rows the layout packed (largest row + 1), including rows a row cap has since hidden.
    pub row_count: u32,
    /// Names of fixed rows (samples), when the rows are not packed.
    pub row_labels: Option<Arc<[String]>>,
    /// Smallest and largest value of the `Value` instances; the lane's value range is the
    /// union over its visible tiles (or a user-fixed range).
    pub value_range: Option<(f32, f32)>,
    /// The row cap applied by [`Self::apply_row_cap`]; `None` when the scene is uncapped.
    pub row_cap: Option<u32>,
    /// Packed rows left out by the row cap: `row_count - row_cap` when positive.
    pub hidden_rows: u32,
    /// Records of this tile left out by the row cap (those on a hidden row). They remain in
    /// `records`, where `row >= row_cap` identifies them; a record spanning several tiles is
    /// counted in each, so sum over tiles by distinct [`TrackRecordId`].
    pub hidden_features: u32,
}

/// What a row cap hid in one scene, or in one lane ([`TrackTileScene::lane_overflow`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackSceneOverflow {
    pub hidden_rows: u32,
    pub hidden_features: u32,
}

impl TrackSceneOverflow {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.hidden_rows == 0 && self.hidden_features == 0
    }
}

impl TrackTileScene {
    #[must_use]
    pub fn empty(contig: impl Into<String>, key: TrackTileKey, renderer: TrackRendererId) -> Self {
        Self {
            contig: contig.into(),
            key,
            renderer,
            lod: TrackTileLod::Detail,
            instances: Vec::new(),
            records: Vec::new(),
            labels: Vec::new(),
            row_count: 0,
            row_labels: None,
            value_range: None,
            row_cap: None,
            hidden_rows: 0,
            hidden_features: 0,
        }
    }

    /// Hides the packed feature rows from `maximum_rows` on: their instances and labels are
    /// removed (their records stay, so record indices are unchanged) and what was hidden is
    /// counted in `hidden_rows` and `hidden_features`. `row_count` keeps the number of rows
    /// packed. Rows are seam-stable, so a feature hidden in one tile is hidden in every tile.
    ///
    /// Only scenes of packed feature rows are capped: scenes with fixed, named rows (samples),
    /// value-mapped scenes and other renderers are returned untouched, as are `maximum_rows`
    /// of zero or `u32::MAX` (no cap). Applying a cap twice is the same as applying the
    /// smaller one; a hidden row cannot be brought back, so lay the tile out again to raise it.
    pub fn apply_row_cap(&mut self, maximum_rows: u32) -> TrackSceneOverflow {
        if maximum_rows == 0
            || maximum_rows == u32::MAX
            || self.renderer != TrackRendererId::FEATURES
            || self.row_labels.is_some()
        {
            return self.overflow();
        }
        let cap = self
            .row_cap
            .map_or(maximum_rows, |cap| cap.min(maximum_rows));
        self.row_cap = Some(cap);
        if self.row_count > cap {
            let records = &self.records;
            self.instances.retain(|instance| {
                instance.vertical() != TrackSceneVertical::Row
                    || instance
                        .record_index()
                        .and_then(|index| records.get(index as usize))
                        .map_or(u32::from(instance.row()), |record| record.row)
                        < cap
            });
            self.labels.retain(|label| label.row < cap);
        }
        self.hidden_rows = self.row_count.saturating_sub(cap);
        self.hidden_features = u32::try_from(
            self.records
                .iter()
                .filter(|record| record.row >= cap)
                .count(),
        )
        .unwrap_or(u32::MAX);
        self.overflow()
    }

    /// What the row cap hid in this scene.
    #[must_use]
    pub const fn overflow(&self) -> TrackSceneOverflow {
        TrackSceneOverflow {
            hidden_rows: self.hidden_rows,
            hidden_features: self.hidden_features,
        }
    }

    /// What the row cap hid over `scenes` (the tiles of one lane): the deepest hidden row
    /// count and the number of distinct hidden records.
    #[must_use]
    pub fn lane_overflow<'scene>(
        scenes: impl IntoIterator<Item = &'scene Self>,
    ) -> TrackSceneOverflow {
        let mut hidden = std::collections::BTreeSet::new();
        let mut hidden_rows = 0;
        for scene in scenes {
            hidden_rows = hidden_rows.max(scene.hidden_rows);
            if let Some(cap) = scene.row_cap {
                hidden.extend(
                    scene
                        .records
                        .iter()
                        .filter(|record| record.row >= cap)
                        .map(|record| record.id),
                );
            }
        }
        TrackSceneOverflow {
            hidden_rows,
            hidden_features: u32::try_from(hidden.len()).unwrap_or(u32::MAX),
        }
    }

    /// Bytes of the instance data exactly as uploaded to the GPU.
    #[must_use]
    pub fn instance_bytes(&self) -> usize {
        self.instances.len() * size_of::<TrackSceneInstance>()
    }

    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        size_of::<Self>()
            + self.contig.capacity()
            + self.instances.capacity() * size_of::<TrackSceneInstance>()
            + self
                .records
                .iter()
                .map(|record| {
                    size_of::<TrackSceneRecord>()
                        + record.name.as_ref().map_or(0, |name| name.len())
                })
                .sum::<usize>()
            + self
                .labels
                .iter()
                .map(|label| size_of::<TrackSceneLabel>() + label.text.len())
                .sum::<usize>()
            + self
                .row_labels
                .as_ref()
                .map_or(0, |labels| labels.iter().map(String::len).sum())
    }

    /// Index of the record with identity `id` in this tile, for per-tile selection and hover
    /// highlighting.
    #[must_use]
    pub fn record_index_of(&self, id: TrackRecordId) -> Option<u32> {
        self.records
            .iter()
            .position(|record| record.id == id)
            .and_then(|index| u32::try_from(index).ok())
    }

    /// Grows `value_range` to include `value`.
    pub fn include_value(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        self.value_range = Some(match self.value_range {
            Some((low, high)) => (low.min(value), high.max(value)),
            None => (value, value),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::too_many_lines, clippy::format_push_string)]

    use super::*;

    #[test]
    fn tile_levels_keep_bins_at_least_one_point_wide() {
        assert_eq!(TrackTileKey::level_for_bases_per_point(0.05), 0);
        assert_eq!(TrackTileKey::level_for_bases_per_point(1.0), 0);
        assert_eq!(TrackTileKey::level_for_bases_per_point(1.01), 1);
        assert_eq!(TrackTileKey::level_for_bases_per_point(2.0), 1);
        assert_eq!(TrackTileKey::level_for_bases_per_point(1000.0), 10);
        assert_eq!(TrackTileKey::level_for_bases_per_point(1.0e12), 19);
        assert_eq!(TrackTileKey::level_for_bases_per_point(f64::NAN), 0);
        for level in 0..=MAXIMUM_TRACK_TILE_LEVEL {
            let key = TrackTileKey { level, index: 3 };
            assert_eq!(key.span(), 1_024 << level);
            assert_eq!(key.nominal_bases_per_point(), 1 << level);
            assert_eq!(key.start(), 3 * key.span());
            assert!(i64::try_from(key.span()).unwrap() <= MAXIMUM_TRACK_SCENE_OFFSET);
        }
    }

    #[test]
    fn covering_tiles_are_aligned_and_independent_of_the_exact_viewport() {
        let tiles = |start, end| TrackTileKey::covering(2, start, end).collect::<Vec<_>>();
        assert_eq!(tiles(0, 0), Vec::new());
        assert_eq!(tiles(0, 1), vec![TrackTileKey { level: 2, index: 0 }]);
        assert_eq!(tiles(4_095, 4_097).len(), 2);
        // Two viewports a few bases apart ask for the same tiles.
        assert_eq!(tiles(5_000, 9_000), tiles(5_003, 9_003));
        assert_eq!(
            tiles(4_096, 8_192),
            vec![TrackTileKey { level: 2, index: 1 }]
        );
        assert_eq!(TrackTileKey::containing(2, 8_191).index, 1);
    }

    #[test]
    fn settings_digest_changes_with_any_setting_and_is_stable() {
        let base = TrackTileSettings::default();
        let mut other = base.clone();
        assert_eq!(base.digest(), other.digest());
        other.feature_summary_features_per_point = 2.0;
        assert_ne!(base.digest(), other.digest());
        let mut custom = base.clone();
        custom.custom.push(("mode".to_owned(), "dense".to_owned()));
        assert_ne!(base.digest(), custom.digest());
    }

    #[test]
    fn instances_pack_into_28_bytes_and_round_trip() {
        assert_eq!(size_of::<TrackSceneInstance>(), 28);
        let tile = TrackTileKey {
            level: 3,
            index: 10,
        };
        let instance = TrackSceneInstance::new(
            tile,
            tile.start() - 100,
            tile.start() + 50,
            513,
            TrackSceneShape::ArrowLeft,
            TrackSceneVertical::Lane,
            TRACK_SCENE_FLAG_POINT,
            (0.25, 0.75),
            TrackColor::Ramp {
                low: TrackColorToken::SCORE_LOW,
                position: 0.5,
            },
            42,
        );
        assert_eq!((instance.start, instance.length), (-100, 150));
        assert_eq!(instance.row(), 513);
        assert_eq!(instance.shape(), Some(TrackSceneShape::ArrowLeft));
        assert_eq!(instance.vertical(), TrackSceneVertical::Lane);
        assert_ne!(instance.flags() & TRACK_SCENE_FLAG_POINT, 0);
        assert_eq!(instance.record_index(), Some(42));
        assert_eq!(
            instance.color(),
            TrackColor::Ramp {
                low: TrackColorToken::SCORE_LOW,
                position: 0.5
            }
        );
        let decoration = TrackSceneInstance::new(
            tile,
            0,
            u64::MAX,
            0,
            TrackSceneShape::Box,
            TrackSceneVertical::Row,
            0,
            (0.0, 1.0),
            TrackColor::Inline([1, 2, 3, 4]),
            TRACK_SCENE_NO_RECORD,
        );
        // Far-away ends saturate at the exact-projection bound instead of wrapping.
        assert_eq!(decoration.start, -81_920);
        assert_eq!(
            i64::from(decoration.start) + i64::from(decoration.length),
            MAXIMUM_TRACK_SCENE_OFFSET
        );
        assert_eq!(decoration.record_index(), None);
        assert_eq!(decoration.color(), TrackColor::Inline([1, 2, 3, 4]));
    }

    #[test]
    fn genotype_matrix_stores_two_bits_per_cell() {
        let samples: Arc<[String]> = (0..5).map(|index| format!("s{index}")).collect();
        let mut matrix = GenotypeMatrix::new(samples, 3);
        assert_eq!(matrix.calls.len(), 4);
        assert_eq!(matrix.call(0, 0), None);
        matrix.set(0, 0, GenotypeCall::HomRef);
        matrix.set(0, 4, GenotypeCall::HomAlt);
        matrix.set(2, 3, GenotypeCall::Het);
        assert_eq!(matrix.call(0, 0), Some(GenotypeCall::HomRef));
        assert_eq!(matrix.call(0, 1), Some(GenotypeCall::NoCall));
        assert_eq!(matrix.call(0, 4), Some(GenotypeCall::HomAlt));
        assert_eq!(matrix.call(2, 3), Some(GenotypeCall::Het));
        assert_eq!(matrix.call(1, 0), None);
        assert!(!matrix.is_loaded(1));
        assert_eq!(
            GenotypeCall::classify([Some(0), Some(0)]),
            GenotypeCall::HomRef
        );
        assert_eq!(
            GenotypeCall::classify([Some(0), Some(1)]),
            GenotypeCall::Het
        );
        assert_eq!(
            GenotypeCall::classify([Some(2), Some(2)]),
            GenotypeCall::HomAlt
        );
        assert_eq!(
            GenotypeCall::classify([Some(1), Some(2)]),
            GenotypeCall::Het
        );
        assert_eq!(
            GenotypeCall::classify([Some(1), None]),
            GenotypeCall::NoCall
        );
        assert_eq!(GenotypeCall::classify([]), GenotypeCall::NoCall);
        assert_eq!(GenotypeCall::classify([Some(1)]), GenotypeCall::HomAlt);
    }

    #[test]
    fn a_custom_payload_names_its_own_renderer() {
        let payload = TrackTilePayload::Custom(CustomTile {
            renderer: TrackRendererId::new("plugin.heatmap"),
            payload: Arc::new(vec![1_u8, 2, 3]),
            resident_bytes: 3,
        });
        assert_eq!(payload.renderer_id().as_str(), "plugin.heatmap");
        assert_eq!(payload.resident_bytes(), 3);
        assert_eq!(
            TrackTilePayload::Bins(BinTile {
                bin_width: 1,
                bins: Vec::new(),
                semantics: BinSemantics::Signal,
            })
            .renderer_id(),
            TrackRendererId::BINS
        );
    }
}
