//! The public track contract, behaviour side: [`TrackTileSource`], the layout registry that
//! turns any payload kind into a world-space scene, and adapters that give every existing
//! auxiliary track format the new contract.
//!
//! # Level of detail
//!
//! A request names a tile of a power-of-two pyramid and carries the level's resolution as
//! `bases_per_point`. The adapters use it as follows.
//!
//! * Quantitative sources (bigWig, TDF, bedGraph, WIG) return [`TRACK_TILE_BINS`] regular bins
//!   of minimum / mean / maximum. bigWig reads the zoom level its reader picks for the tile's
//!   span and keeps the file's own extremes ([`BigWigSource::query_with_extremes`]). TDF does
//!   the same where the archive has them ([`TdfSource::query_channel_with_extremes`]): a TDF
//!   zoom level is one dataset per window function, and most archives were written with the
//!   mean only. For such an archive a bin's minimum and maximum are the extremes of the
//!   *means* that touch it, as before; an archive written with `min` and `max` datasets
//!   keeps a peak narrower than a bin.
//! * Feature sources return a coverage summary instead of features once the tile holds more
//!   than `settings.feature_summary_features_per_point` features per nominal point
//!   (default [`rgv_model::DEFAULT_FEATURE_SUMMARY_FEATURES_PER_POINT`]).
//! * bigBed decides that without reading the whole tile. It reads at most the number of
//!   features that makes the tile dense ([`BigBedSource::query_limited`]); if there are more,
//!   the summary comes from the file's own zoom level no coarser than a bin
//!   ([`BigBedSource::query_coverage_summary`]) and no further feature is read. A dense tile
//!   therefore costs a bounded read however many features it holds, and an interval beyond
//!   the per-query record limit is a summary instead of an error. Bins of such a summary
//!   hold the shallowest, mean and deepest feature overlap. A bigBed without zoom levels is
//!   read like any other feature source.
//! * Variant sources return per-bin counts above
//!   `settings.variant_summary_variants_per_point`
//!   (default [`rgv_model::DEFAULT_VARIANT_SUMMARY_VARIANTS_PER_POINT`]).

use std::{collections::HashMap, sync::Arc};

use rgv_core::CopyNumberValueMode;
use rgv_hts::{
    BigBedSource, BigWigSource, QuantitativeSourceMetadata, RemoteCancellation,
    RemoteTransferBudget, TdfSource,
};
use rgv_layout::{
    WorldRowItem, cluster_closure_extent, lay_out_bin_tile, lay_out_feature_tile,
    lay_out_interaction_tile, lay_out_variant_tile, pack_seam_stable_rows,
    world_feature_row_padding,
};
use rgv_model::{
    BinSemantics, BinTile, CopyNumberQuery, CopyNumberValueSemantics, FeatureRecord,
    FeatureRowAxis, FeatureTile, GenotypeCall, GenotypeMatrix, GwasQuery, InteractionQuery,
    InteractionTile, MutationQuery, Strand, TRACK_TILE_BINS, TileBin, TileFeature,
    TileFeatureBlock, TileInteraction, TileVariant, TileVariantKind, TrackCancellation, TrackColor,
    TrackColorToken, TrackDigest, TrackRecord, TrackRecordId, TrackRecordQuery, TrackRendererId,
    TrackRowStability, TrackTile, TrackTileError, TrackTileKey, TrackTileLod, TrackTilePayload,
    TrackTileProvenance, TrackTileRequest, TrackTileScene, VariantQuery, VariantType,
};

use crate::{
    TrackSnapshotData, TrackSourceDiagnostics,
    track_engine::REMOTE_TRACK_OPERATION_TRANSFER_BUDGET,
    track_source::{NativeTile, SearchRequest, TrackQuerySettings, TrackSource},
};

/// Most times a feature tile widens its read to reach the end of the overlap clusters that
/// touch it.
pub const MAXIMUM_CLUSTER_CLOSURE_ROUNDS: usize = 8;
/// Furthest a feature tile reads beyond either of its edges for row stability, in tile spans.
pub const MAXIMUM_CLUSTER_CLOSURE_TILES: u64 = 64;
/// Most bytes of complete records a built-in feature source hands over with one tile
/// ([`TrackTileSource::take_retained_records`]). Records beyond it are resolved by a re-read
/// ([`TrackTileSource::lookup_record`]).
pub const MAXIMUM_RETAINED_RECORD_BYTES_PER_TILE: usize = 256 << 10;

/// A name search handed to a source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackTileSearch {
    /// The query exactly as typed.
    pub query: String,
    pub limit: usize,
    pub include_variant_ids: bool,
}

/// Name-search results from one source: matched term and the feature it belongs to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackTileSearchHits {
    pub hits: Vec<(String, FeatureRecord)>,
    pub index_truncated: bool,
    pub scan_truncated: bool,
}

/// A track data source: built-in file formats and plugins implement exactly this.
///
/// A source owns its reader. The engine calls one method at a time per source, from a helper
/// thread, and may call different sources concurrently. Every method receives the
/// cancellation of its operation; a source that reads remote data or scans many records
/// should poll it and return [`TrackTileError::Cancelled`].
pub trait TrackTileSource: Send {
    /// Everything this track shows in `request.interval()` of `request.contig`, at the
    /// resolution `request.bases_per_point`. The result must depend only on the request, so
    /// the engine can cache it by tile.
    fn query_tile(
        &mut self,
        request: &TrackTileRequest,
        cancel: &TrackCancellation,
    ) -> Result<TrackTile, TrackTileError>;

    /// The nearest feature after (or before) `position`; `None` for kinds without discrete
    /// navigable features.
    fn adjacent_feature(
        &mut self,
        _contig: &str,
        _position: u64,
        _forward: bool,
        _cancel: &TrackCancellation,
    ) -> Result<Option<FeatureRecord>, TrackTileError> {
        Ok(None)
    }

    /// Features whose name matches; `None` for kinds that are not searchable.
    fn search(
        &mut self,
        _search: &TrackTileSearch,
        _cancel: &TrackCancellation,
    ) -> Result<Option<TrackTileSearchHits>, TrackTileError> {
        Ok(None)
    }

    /// Input the source had to skip; `None` for kinds that do not track it.
    fn diagnostics(&self) -> Option<TrackSourceDiagnostics> {
        None
    }

    /// The complete record with identity `query.id`, found by reading `query`'s interval (the
    /// record's own) and nothing wider. `None` when the source has no such record there, and
    /// for kinds whose tiles carry everything there is to show.
    fn lookup_record(
        &mut self,
        _query: &TrackRecordQuery,
        _cancel: &TrackCancellation,
    ) -> Result<Option<TrackRecord>, TrackTileError> {
        Ok(None)
    }

    /// Complete records of the tile the last successful [`Self::query_tile`] returned, for
    /// the engine to keep with the tile so a later [`Self::lookup_record`] needs no read. The
    /// engine calls it once after each tile; a source that keeps nothing returns nothing.
    /// What is returned is charged to the track's tile cache budget.
    fn take_retained_records(&mut self) -> Vec<TrackRecord> {
        Vec::new()
    }
}

/// Lays one tile out in world space.
pub type TrackTileLayoutFn =
    Arc<dyn Fn(&TrackTile) -> Result<TrackTileScene, TrackTileError> + Send + Sync>;

/// Layout functions by renderer id. This lookup is how a payload kind reaches its layout: no
/// code between the source and the layout function inspects the payload.
#[derive(Clone)]
pub struct TrackTileLayoutRegistry {
    layouts: HashMap<TrackRendererId, TrackTileLayoutFn>,
}

impl TrackTileLayoutRegistry {
    /// A registry without any layout, not even the built-in ones.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            layouts: HashMap::new(),
        }
    }

    /// The built-in feature, bin, variant and interaction layouts.
    #[must_use]
    pub fn builtin() -> Self {
        let mut registry = Self::empty();
        registry.register(TrackRendererId::FEATURES, lay_out_feature_tile);
        registry.register(TrackRendererId::BINS, lay_out_bin_tile);
        registry.register(TrackRendererId::VARIANTS, lay_out_variant_tile);
        registry.register(TrackRendererId::INTERACTIONS, lay_out_interaction_tile);
        registry
    }

    /// Registers (or replaces) the layout of `renderer`.
    pub fn register(
        &mut self,
        renderer: TrackRendererId,
        layout: impl Fn(&TrackTile) -> Result<TrackTileScene, TrackTileError> + Send + Sync + 'static,
    ) {
        self.layouts.insert(renderer, Arc::new(layout));
    }

    pub fn lay_out(&self, tile: &TrackTile) -> Result<TrackTileScene, TrackTileError> {
        let renderer = tile.payload.renderer_id();
        let layout = self
            .layouts
            .get(&renderer)
            .ok_or_else(|| TrackTileError::UnknownRenderer(renderer.to_string()))?;
        layout(tile)
    }
}

impl Default for TrackTileLayoutRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

fn remote_control(
    request_budget: Option<rgv_model::TrackTransferBudget>,
    cancel: &TrackCancellation,
) -> (Option<RemoteCancellation>, Option<RemoteTransferBudget>) {
    let cancel = cancel.clone();
    (
        Some(Arc::new(move || !cancel.is_cancelled())),
        Some(
            request_budget.map_or(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET, |budget| {
                RemoteTransferBudget {
                    maximum_requests: budget.maximum_requests,
                    maximum_response_bytes: budget.maximum_response_bytes,
                }
            }),
        ),
    )
}

/// A source error, unless the operation was cancelled meanwhile: an aborted remote read
/// surfaces as an I/O error and must not be reported as one.
fn source_error(message: String, cancel: &TrackCancellation) -> TrackTileError {
    if cancel.is_cancelled() {
        TrackTileError::Cancelled
    } else {
        TrackTileError::Source(message)
    }
}

/// Adapter giving any legacy [`TrackSource`] the tile contract.
pub(crate) struct LegacyTrackTileSource {
    source: Box<dyn TrackSource>,
    /// Complete records of the feature tile built last, until the engine takes them.
    retained: Vec<TrackRecord>,
}

impl LegacyTrackTileSource {
    pub(crate) fn new(source: Box<dyn TrackSource>) -> Self {
        Self {
            source,
            retained: Vec::new(),
        }
    }

    fn query(
        &mut self,
        request: &TrackTileRequest,
        start: u64,
        end: u64,
        cancel: &TrackCancellation,
    ) -> Result<TrackSnapshotData, TrackTileError> {
        cancel.check()?;
        let settings = TrackQuerySettings {
            variant: request.settings.variant.as_deref(),
            interaction: request.settings.interaction.as_deref(),
            copy_number: request.settings.copy_number.as_deref(),
        };
        self.source
            .query(&request.contig, start, end, settings)
            .map_err(|message| source_error(message, cancel))
    }

    /// Features of the tile with seam-stable rows: the read is widened to the end of every
    /// overlap cluster touching the tile, within the closure bounds.
    fn feature_tile(
        &mut self,
        request: &TrackTileRequest,
        mut records: Vec<FeatureRecord>,
        cancel: &TrackCancellation,
    ) -> Result<(FeatureTile, TrackRowStability), TrackTileError> {
        let key = request.tile;
        let padding = world_feature_row_padding(key.level);
        let reach = key.span().saturating_mul(MAXIMUM_CLUSTER_CLOSURE_TILES);
        let (floor, ceiling) = (
            key.start().saturating_sub(reach),
            key.end().saturating_add(reach),
        );
        let items = |records: &[FeatureRecord]| {
            records
                .iter()
                .map(|record| (record.start, record.end, TrackRecordId::for_feature(record)))
                .collect::<Vec<WorldRowItem>>()
        };
        let (mut start, mut end) = (key.start(), key.end());
        let mut stability = TrackRowStability::Bounded;
        for _ in 0..MAXIMUM_CLUSTER_CLOSURE_ROUNDS {
            let Some((low, high)) = cluster_closure_extent(&items(&records), start, end, padding)
            else {
                stability = TrackRowStability::SeamStable;
                break;
            };
            let (low, high) = (low.max(floor), high.min(ceiling));
            if low >= start && high <= end {
                // The cluster runs past the read-ahead bound.
                break;
            }
            (start, end) = (low.min(start), high.max(end));
            let TrackSnapshotData::Features(wider) = self.query(request, start, end, cancel)?
            else {
                break;
            };
            records = wider;
        }
        if stability == TrackRowStability::Bounded
            && cluster_closure_extent(&items(&records), start, end, padding).is_none()
        {
            stability = TrackRowStability::SeamStable;
        }
        let packed = items(&records);
        let rows = pack_seam_stable_rows(&packed, padding);
        let features = records
            .iter()
            .zip(packed)
            .zip(rows)
            .filter(|((record, _), _)| record.end > key.start() && record.start < key.end())
            .map(|((record, (_, _, id)), row)| tile_feature(record, id, Some(row), key))
            .collect();
        // The complete records of the tile, within the retention bound, go with the tile.
        let mut retained_bytes = 0_usize;
        self.retained = records
            .into_iter()
            .filter(|record| record.end > key.start() && record.start < key.end())
            .map(TrackRecord::Feature)
            .take_while(|record| {
                retained_bytes = retained_bytes.saturating_add(record.resident_bytes());
                retained_bytes <= MAXIMUM_RETAINED_RECORD_BYTES_PER_TILE
            })
            .collect();
        Ok((
            FeatureTile {
                features,
                rows: FeatureRowAxis::Packed,
            },
            stability,
        ))
    }
}

fn tile_feature(
    record: &FeatureRecord,
    id: TrackRecordId,
    row_hint: Option<u32>,
    key: TrackTileKey,
) -> TileFeature {
    let mut blocks = Vec::new();
    for (start, end) in &record.blocks {
        if *end <= key.start() || *start >= key.end() {
            continue;
        }
        let mut push = |start: u64, end: u64, thick: bool| {
            if start < end {
                blocks.push(TileFeatureBlock { start, end, thick });
            }
        };
        match record.coding_interval {
            Some((coding_start, coding_end)) => {
                push(*start, (*end).min(coding_start), false);
                push((*start).max(coding_start), (*end).min(coding_end), true);
                push((*start).max(coding_end), *end, false);
            }
            None => push(*start, *end, true),
        }
    }
    // Same rule as the pixel layout: typed records are labelled at gene or transcript level.
    let labelable = record.feature_type.as_deref().is_none_or(|feature_type| {
        ["gene", "transcript", "mrna", "primary_transcript"]
            .iter()
            .any(|candidate| feature_type.eq_ignore_ascii_case(candidate))
    });
    TileFeature {
        id,
        start: record.start,
        end: record.end,
        strand: record.strand,
        // A structured record whose blocks all lie outside the tile still needs its connector.
        blocks: if record.blocks.is_empty() {
            Vec::new()
        } else if blocks.is_empty() {
            vec![TileFeatureBlock {
                start: record.start,
                end: record.start,
                thick: false,
            }]
        } else {
            blocks
        },
        name: record.display_label().map(Arc::from),
        score: record.score,
        color: None,
        row_hint,
        labelable,
    }
}

/// Whether `count` records in a tile exceed `threshold` records per nominal point.
fn exceeds_density(count: usize, request: &TrackTileRequest, threshold: f32) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let density = count as f64 / request.tile_points().max(1.0);
    threshold.is_finite() && threshold > 0.0 && density > f64::from(threshold)
}

/// Coverage summary of `intervals` over the tile: per bin, the features covering all of it
/// (minimum), the mean depth, and the features touching it (maximum).
fn interval_density_bins(
    key: TrackTileKey,
    intervals: impl Iterator<Item = (u64, u64)>,
) -> BinTile {
    let width = key.nominal_bases_per_point().max(1);
    let mut touching = vec![0_i64; TRACK_TILE_BINS + 1];
    let mut covering = vec![0_i64; TRACK_TILE_BINS + 1];
    let mut bases = vec![0_u64; TRACK_TILE_BINS];
    for (start, end) in intervals {
        let (start, end) = (start.max(key.start()), end.max(start + 1).min(key.end()));
        if start >= end {
            continue;
        }
        let (local_start, local_end) = (start - key.start(), end - key.start());
        #[allow(clippy::cast_possible_truncation)]
        let (first, last) = (
            (local_start / width) as usize,
            ((local_end - 1) / width) as usize,
        );
        touching[first] += 1;
        touching[last + 1] -= 1;
        #[allow(clippy::cast_possible_truncation)]
        let (full_first, full_end) = (
            local_start.div_ceil(width) as usize,
            (local_end / width) as usize,
        );
        if full_first < full_end {
            covering[full_first] += 1;
            covering[full_end] -= 1;
        }
        // Partial cover of the edge bins; fully covered bins are counted through `covering`.
        if first < full_first || first >= full_end {
            let bin_end = (first as u64 + 1) * width;
            bases[first] += local_end.min(bin_end) - local_start;
        }
        if last != first && last >= full_end {
            bases[last] += local_end - last as u64 * width;
        }
    }
    let (mut touch, mut cover) = (0_i64, 0_i64);
    let bins = (0..TRACK_TILE_BINS)
        .map(|bin| {
            touch += touching[bin];
            cover += covering[bin];
            #[allow(clippy::cast_precision_loss)]
            TileBin {
                minimum: cover as f32,
                mean: cover as f32 + bases[bin] as f32 / width as f32,
                maximum: touch as f32,
            }
        })
        .collect();
    BinTile {
        bin_width: width,
        bins,
        semantics: BinSemantics::FeatureDensity,
    }
}

/// One source record for [`rebin`]: start, end, minimum, mean, maximum.
type SummaryRecord = (u64, u64, f32, f32, f32);

/// Regular bins over the tile from arbitrary source records: base-weighted mean, and the
/// extremes of every record that touches the bin.
fn rebin(key: TrackTileKey, records: impl Iterator<Item = SummaryRecord>) -> BinTile {
    let width = key.nominal_bases_per_point().max(1);
    let mut sums = vec![(0.0_f64, 0_u64); TRACK_TILE_BINS];
    let mut bins = vec![
        TileBin {
            minimum: f32::INFINITY,
            mean: f32::NAN,
            maximum: f32::NEG_INFINITY,
        };
        TRACK_TILE_BINS
    ];
    for (start, end, minimum, mean, maximum) in records {
        let (start, end) = (start.max(key.start()), end.min(key.end()));
        if start >= end || !mean.is_finite() {
            continue;
        }
        let (local_start, local_end) = (start - key.start(), end - key.start());
        #[allow(clippy::cast_possible_truncation)]
        for bin in (local_start / width) as usize..=((local_end - 1) / width) as usize {
            let bin_start = bin as u64 * width;
            let overlap = local_end.min(bin_start + width) - local_start.max(bin_start);
            #[allow(clippy::cast_precision_loss)]
            {
                sums[bin].0 += f64::from(mean) * overlap as f64;
            }
            sums[bin].1 += overlap;
            bins[bin].minimum = bins[bin].minimum.min(minimum);
            bins[bin].maximum = bins[bin].maximum.max(maximum);
        }
    }
    for (bin, (sum, weight)) in bins.iter_mut().zip(sums) {
        if weight == 0 {
            *bin = TileBin::EMPTY;
        } else {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            {
                bin.mean = (sum / weight as f64) as f32;
            }
        }
    }
    BinTile {
        bin_width: width,
        bins,
        semantics: BinSemantics::Signal,
    }
}

fn variant_kind(types: &[VariantType]) -> TileVariantKind {
    match types.first() {
        Some(VariantType::Snv | VariantType::Mnv) => TileVariantKind::Snv,
        Some(VariantType::Insertion) => TileVariantKind::Insertion,
        Some(VariantType::Deletion) => TileVariantKind::Deletion,
        Some(
            VariantType::StructuralDeletion
            | VariantType::StructuralInsertion
            | VariantType::Inversion
            | VariantType::Duplication
            | VariantType::CopyNumber
            | VariantType::Breakend,
        ) => TileVariantKind::Structural,
        _ => TileVariantKind::Other,
    }
}

fn variant_payload(
    request: &TrackTileRequest,
    query: &VariantQuery,
) -> (TrackTilePayload, TrackTileLod) {
    let key = request.tile;
    if exceeds_density(
        query.records.len(),
        request,
        request.settings.variant_summary_variants_per_point,
    ) {
        let width = key.nominal_bases_per_point().max(1);
        let mut counts = vec![0_u32; TRACK_TILE_BINS];
        for record in &query.records {
            #[allow(clippy::cast_possible_truncation)]
            let bin = (record.start.clamp(key.start(), key.end() - 1) - key.start()) / width;
            counts[bin as usize] += 1;
        }
        #[allow(clippy::cast_precision_loss)]
        let bins = counts
            .into_iter()
            .map(|count| TileBin {
                minimum: count as f32,
                mean: count as f32,
                maximum: count as f32,
            })
            .collect();
        return (
            TrackTilePayload::Bins(BinTile {
                bin_width: width,
                bins,
                semantics: BinSemantics::VariantDensity,
            }),
            TrackTileLod::Summary,
        );
    }
    let variants = query
        .records
        .iter()
        .map(|record| TileVariant {
            id: TrackRecordId::for_variant(record),
            start: record.start,
            end: record.end,
            kind: variant_kind(&record.variant_types),
            name: record.id.as_deref().map(Arc::from),
            quality: record.quality,
        })
        .collect::<Vec<_>>();
    let genotypes = (request.settings.genotype_matrix
        && !query.header.samples.is_empty()
        && query
            .records
            .iter()
            .any(|record| !record.genotypes.is_empty()))
    .then(|| {
        let mut matrix = GenotypeMatrix::new(
            query.header.samples.iter().cloned().collect(),
            variants.len(),
        );
        for (index, record) in query.records.iter().enumerate() {
            for genotype in &record.genotypes {
                matrix.set(
                    index,
                    genotype.sample_index,
                    GenotypeCall::classify(genotype.alleles.iter().map(|allele| allele.index)),
                );
            }
        }
        matrix
    });
    (
        TrackTilePayload::Variants(rgv_model::VariantTile {
            variants,
            genotypes,
        }),
        TrackTileLod::Detail,
    )
}

fn interaction_payload(request: &TrackTileRequest, query: &InteractionQuery) -> TrackTilePayload {
    let interactions = query
        .records
        .iter()
        .map(|record| {
            // The anchor on the queried contig is the local one.
            let (local, other) = if record.source.contig != request.contig
                && record.target.contig == request.contig
            {
                (&record.target, &record.source)
            } else {
                (&record.source, &record.target)
            };
            TileInteraction {
                id: TrackRecordId::from_parts(
                    record.source.start,
                    record.target.end,
                    &[
                        record.source.contig.as_bytes(),
                        record.target.contig.as_bytes(),
                        &record.source.end.to_le_bytes(),
                        &record.target.start.to_le_bytes(),
                        record.name.as_deref().unwrap_or_default().as_bytes(),
                    ],
                ),
                source: (local.start, local.end),
                target: (other.contig == local.contig).then_some((other.start, other.end)),
                score: record.score,
                color: record.color.map(TrackColor::Inline),
                name: record.name.as_deref().map(Arc::from),
            }
        })
        .collect();
    TrackTilePayload::Interactions(InteractionTile { interactions })
}

fn copy_number_payload(request: &TrackTileRequest, query: &CopyNumberQuery) -> TrackTilePayload {
    let absolute = match request
        .settings
        .copy_number
        .as_ref()
        .map_or(CopyNumberValueMode::Automatic, |settings| {
            settings.value_mode
        }) {
        CopyNumberValueMode::Automatic => {
            query.inferred_value_semantics == CopyNumberValueSemantics::Absolute
        }
        CopyNumberValueMode::LogRatio => false,
        CopyNumberValueMode::Absolute => true,
    };
    let features = query
        .segments
        .iter()
        .map(|segment| {
            // Position on the loss..neutral..gain axis, -1 to 1.
            let signed = if absolute {
                (segment.value - 2.0) / 2.0
            } else {
                segment.value / 1.5
            }
            .clamp(-1.0, 1.0);
            let color = if signed < 0.0 {
                TrackColor::Ramp {
                    low: TrackColorToken::COPY_LOSS,
                    position: 1.0 + signed,
                }
            } else {
                TrackColor::Ramp {
                    low: TrackColorToken::COPY_NEUTRAL,
                    position: signed,
                }
            };
            TileFeature {
                id: TrackRecordId::from_parts(
                    segment.start,
                    segment.end,
                    &[&(segment.sample_index as u64).to_le_bytes()],
                ),
                start: segment.start,
                end: segment.end,
                strand: Strand::Unstranded,
                blocks: Vec::new(),
                name: None,
                score: Some(segment.value),
                color: Some(color),
                row_hint: Some(u32::try_from(segment.sample_index).unwrap_or(u32::MAX)),
                labelable: false,
            }
        })
        .collect();
    TrackTilePayload::Features(FeatureTile {
        features,
        rows: FeatureRowAxis::Fixed {
            labels: query.samples.iter().cloned().collect(),
        },
    })
}

fn mutation_payload(query: &MutationQuery) -> TrackTilePayload {
    let features = query
        .records
        .iter()
        .map(|record| {
            // One stable colour per classification, as the pixel layout does by name.
            let mut digest = TrackDigest::new();
            digest.write(record.classification.as_bytes());
            let hash = digest.finish().to_le_bytes();
            TileFeature {
                id: TrackRecordId::from_parts(
                    record.start,
                    record.end,
                    &[
                        &(record.sample_index as u64).to_le_bytes(),
                        record.gene.as_bytes(),
                        record.tumor_allele_two.as_bytes(),
                    ],
                ),
                start: record.start,
                end: record.end,
                strand: Strand::Unstranded,
                blocks: Vec::new(),
                name: Some(Arc::from(record.gene.as_str())),
                score: None,
                color: Some(if record.classification.is_empty() {
                    TrackColor::Token(TrackColorToken::MUTATION)
                } else {
                    TrackColor::Inline([
                        64 | (hash[0] >> 1),
                        64 | (hash[1] >> 1),
                        64 | (hash[2] >> 1),
                        255,
                    ])
                }),
                row_hint: Some(u32::try_from(record.sample_index).unwrap_or(u32::MAX)),
                labelable: false,
            }
        })
        .collect();
    TrackTilePayload::Features(FeatureTile {
        features,
        rows: FeatureRowAxis::Fixed {
            labels: query.samples.iter().cloned().collect(),
        },
    })
}

fn gwas_payload(request: &TrackTileRequest, query: &GwasQuery) -> (TrackTilePayload, TrackTileLod) {
    if exceeds_density(
        query.records.len(),
        request,
        request.settings.feature_summary_features_per_point,
    ) {
        // A Manhattan plot keeps its peaks: the bin maximum is the strongest association.
        return (
            TrackTilePayload::Bins(rebin(
                request.tile,
                query.records.iter().map(|record| {
                    let value = record.minus_log10_p;
                    (record.position, record.position + 1, value, value, value)
                }),
            )),
            TrackTileLod::Summary,
        );
    }
    let maximum = query
        .records
        .iter()
        .map(|record| record.minus_log10_p)
        .filter(|value| value.is_finite())
        .fold(0.0_f32, f32::max);
    let features = query
        .records
        .iter()
        .map(|record| TileFeature {
            id: TrackRecordId::from_parts(
                record.position,
                record.position + 1,
                &[record.identifier.as_bytes()],
            ),
            start: record.position,
            end: record.position + 1,
            strand: Strand::Unstranded,
            blocks: Vec::new(),
            name: Some(Arc::from(record.identifier.as_str())),
            score: record
                .minus_log10_p
                .is_finite()
                .then_some(record.minus_log10_p),
            color: Some(TrackColor::Token(TrackColorToken::ASSOCIATION)),
            row_hint: None,
            labelable: false,
        })
        .collect();
    (
        TrackTilePayload::Features(FeatureTile {
            features,
            rows: FeatureRowAxis::Scatter {
                minimum: 0.0,
                maximum,
            },
        }),
        TrackTileLod::Detail,
    )
}

impl TrackTileSource for LegacyTrackTileSource {
    fn query_tile(
        &mut self,
        request: &TrackTileRequest,
        cancel: &TrackCancellation,
    ) -> Result<TrackTile, TrackTileError> {
        let (keep_going, budget) = remote_control(request.transfer_budget, cancel);
        self.source.set_remote_control(keep_going, budget);
        self.retained.clear();
        let result = self.query_tile_installed(request, cancel);
        if result.is_err() {
            self.retained.clear();
        }
        // A later search or feature step must not inherit this request's cancellation.
        self.source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        result
    }

    fn lookup_record(
        &mut self,
        query: &TrackRecordQuery,
        cancel: &TrackCancellation,
    ) -> Result<Option<TrackRecord>, TrackTileError> {
        cancel.check()?;
        let (keep_going, budget) = remote_control(None, cancel);
        self.source.set_remote_control(keep_going, budget);
        let settings = TrackQuerySettings {
            variant: query.settings.variant.as_deref(),
            interaction: query.settings.interaction.as_deref(),
            copy_number: query.settings.copy_number.as_deref(),
        };
        // The record's own interval, and at least one base of it for a zero-length record.
        let result = self
            .source
            .query(
                &query.contig,
                query.start,
                query.end.max(query.start.saturating_add(1)),
                settings,
            )
            .map_err(|message| source_error(message, cancel));
        self.source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        let data = result?;
        cancel.check()?;
        Ok(match data {
            TrackSnapshotData::Features(records) => records
                .into_iter()
                .find(|record| {
                    record.start == query.start
                        && record.end == query.end
                        && TrackRecordId::for_feature(record) == query.id
                })
                .map(TrackRecord::Feature),
            TrackSnapshotData::Variants(variants) => variants
                .records
                .iter()
                .find(|record| {
                    record.start == query.start
                        && record.end == query.end
                        && TrackRecordId::for_variant(record) == query.id
                })
                .map(|record| TrackRecord::Variant(Box::new(record.clone()))),
            _ => None,
        })
    }

    fn take_retained_records(&mut self) -> Vec<TrackRecord> {
        std::mem::take(&mut self.retained)
    }

    fn adjacent_feature(
        &mut self,
        contig: &str,
        position: u64,
        forward: bool,
        cancel: &TrackCancellation,
    ) -> Result<Option<FeatureRecord>, TrackTileError> {
        cancel.check()?;
        let (keep_going, budget) = remote_control(None, cancel);
        self.source.set_remote_control(keep_going, budget);
        let result = self
            .source
            .adjacent_feature(contig, position, forward)
            .map_err(|message| source_error(message, cancel));
        self.source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        result
    }

    fn search(
        &mut self,
        search: &TrackTileSearch,
        cancel: &TrackCancellation,
    ) -> Result<Option<TrackTileSearchHits>, TrackTileError> {
        cancel.check()?;
        let (keep_going, budget) = remote_control(None, cancel);
        self.source.set_remote_control(keep_going, budget);
        let normalized = search.query.trim().to_lowercase();
        let result = self
            .source
            .search(SearchRequest {
                query: &search.query,
                normalized: &normalized,
                limit: search.limit,
                include_variant_ids: search.include_variant_ids,
            })
            .map(|results| {
                results.map(|results| TrackTileSearchHits {
                    hits: results.hits,
                    index_truncated: results.index_truncated,
                    scan_truncated: results.scan_truncated,
                })
            })
            .map_err(|message| source_error(message, cancel));
        self.source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        result
    }

    fn diagnostics(&self) -> Option<TrackSourceDiagnostics> {
        self.source.diagnostics()
    }
}

impl LegacyTrackTileSource {
    fn query_tile_installed(
        &mut self,
        request: &TrackTileRequest,
        cancel: &TrackCancellation,
    ) -> Result<TrackTile, TrackTileError> {
        let key = request.tile;
        cancel.check()?;
        // A source with summaries of its own answers from them.
        let data = match self
            .source
            .query_native_tile(request)
            .map_err(|message| source_error(message, cancel))?
        {
            NativeTile::Tile(tile) => {
                cancel.check()?;
                return Ok(*tile);
            }
            NativeTile::Features(records) => TrackSnapshotData::Features(records),
            NativeTile::Generic => self.query(request, key.start(), key.end(), cancel)?,
        };
        let mut provenance = TrackTileProvenance::default();
        // The one match over the legacy snapshot enum: it stays inside this adapter.
        let (payload, lod) = match data {
            TrackSnapshotData::Features(records) => {
                provenance.records_examined = records.len();
                provenance.records_truncated =
                    records.len() >= rgv_model::MAX_FEATURE_RECORDS_PER_QUERY;
                if exceeds_density(
                    records.len(),
                    request,
                    request.settings.feature_summary_features_per_point,
                ) {
                    (
                        TrackTilePayload::Bins(interval_density_bins(
                            key,
                            records.iter().map(|record| (record.start, record.end)),
                        )),
                        TrackTileLod::Summary,
                    )
                } else {
                    let (tile, stability) = self.feature_tile(request, records, cancel)?;
                    provenance.row_stability = stability;
                    (TrackTilePayload::Features(tile), TrackTileLod::Detail)
                }
            }
            TrackSnapshotData::Variants(query) => {
                provenance.records_examined = query.provenance.records_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                provenance.genotype_cells_omitted = query.provenance.genotype_cells_omitted;
                variant_payload(request, &query)
            }
            TrackSnapshotData::Quantitative(query) => {
                provenance.records_examined = query.provenance.records_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                provenance.source_reduction_bases = query.provenance.summary_reduction_bases;
                (
                    TrackTilePayload::Bins(rebin(
                        key,
                        query.records.iter().map(|record| {
                            (
                                record.start,
                                record.end,
                                record.value,
                                record.value,
                                record.value,
                            )
                        }),
                    )),
                    TrackTileLod::Detail,
                )
            }
            TrackSnapshotData::CopyNumber(query) => {
                provenance.records_examined = query.provenance.records_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                (copy_number_payload(request, &query), TrackTileLod::Detail)
            }
            TrackSnapshotData::Mutations(query) => {
                provenance.records_examined = query.provenance.records_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                (mutation_payload(&query), TrackTileLod::Detail)
            }
            TrackSnapshotData::Interactions(query) => {
                provenance.records_examined = query.provenance.endpoint_entries_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                (interaction_payload(request, &query), TrackTileLod::Detail)
            }
            TrackSnapshotData::Gwas(query) => {
                provenance.records_examined = query.provenance.records_examined;
                provenance.records_truncated = query.provenance.records_truncated;
                gwas_payload(request, &query)
            }
        };
        cancel.check()?;
        Ok(TrackTile {
            contig: request.contig.clone(),
            key,
            lod,
            payload,
            provenance,
        })
    }
}

/// bigWig with the file's own zoom-level extremes, so a peak narrower than a bin survives in
/// the bin's maximum.
pub(crate) struct BigWigTileSource {
    source: BigWigSource,
}

impl BigWigTileSource {
    pub(crate) fn new(source: BigWigSource) -> Self {
        Self { source }
    }
}

impl TrackTileSource for BigWigTileSource {
    fn query_tile(
        &mut self,
        request: &TrackTileRequest,
        cancel: &TrackCancellation,
    ) -> Result<TrackTile, TrackTileError> {
        cancel.check()?;
        let (keep_going, budget) = remote_control(request.transfer_budget, cancel);
        self.source.set_remote_cancellation(keep_going);
        self.source.set_remote_transfer_budget(budget);
        let result = bigwig_native_tile(&mut self.source, request);
        self.source.set_remote_cancellation(None);
        result.map_err(|message| source_error(message, cancel))
    }
}

/// A bigWig tile with the file's own zoom-level extremes.
pub(crate) fn bigwig_native_tile(
    source: &mut BigWigSource,
    request: &TrackTileRequest,
) -> Result<TrackTile, String> {
    let key = request.tile;
    let query = source
        .query_with_extremes(&request.contig, key.start(), key.end())
        .map_err(|error| error.to_string())?;
    Ok(TrackTile {
        contig: request.contig.clone(),
        key,
        lod: if query.provenance.summary_reduction_bases.is_some() {
            TrackTileLod::Summary
        } else {
            TrackTileLod::Detail
        },
        payload: TrackTilePayload::Bins(rebin(
            key,
            query.records.iter().map(|record| {
                (
                    record.start,
                    record.end,
                    record.minimum,
                    record.mean,
                    record.maximum,
                )
            }),
        )),
        provenance: TrackTileProvenance {
            records_examined: query.provenance.records_examined,
            records_truncated: query.provenance.records_truncated,
            source_reduction_bases: query.provenance.summary_reduction_bases,
            ..TrackTileProvenance::default()
        },
    })
}

/// A TDF tile with the archive's `min` and `max` datasets where it has them; see the module
/// documentation for what a mean-only archive yields.
pub(crate) fn tdf_native_tile(
    source: &mut TdfSource,
    track_index: usize,
    request: &TrackTileRequest,
) -> Result<TrackTile, String> {
    let key = request.tile;
    let query = source
        .query_channel_with_extremes(track_index, &request.contig, key.start(), key.end())
        .map_err(|error| error.to_string())?;
    let from_zoom_level = matches!(
        &query.source_metadata,
        Some(QuantitativeSourceMetadata::Tdf(metadata)) if metadata.zoom.is_some()
    );
    Ok(TrackTile {
        contig: request.contig.clone(),
        key,
        lod: if from_zoom_level {
            TrackTileLod::Summary
        } else {
            TrackTileLod::Detail
        },
        payload: TrackTilePayload::Bins(rebin(
            key,
            query.records.iter().map(|record| {
                (
                    record.start,
                    record.end,
                    record.minimum,
                    record.mean,
                    record.maximum,
                )
            }),
        )),
        provenance: TrackTileProvenance {
            records_examined: query.provenance.records_examined,
            records_truncated: query.provenance.records_truncated,
            source_reduction_bases: query.provenance.summary_reduction_bases,
            ..TrackTileProvenance::default()
        },
    })
}

/// Most features a tile may hold before it is summarised: the largest count that
/// [`exceeds_density`] does not reject. `None` when the threshold is disabled.
fn detail_feature_limit(request: &TrackTileRequest) -> Option<usize> {
    let threshold = request.settings.feature_summary_features_per_point;
    if !(threshold.is_finite() && threshold > 0.0) {
        return None;
    }
    let limit = (f64::from(threshold) * request.tile_points().max(1.0)).floor();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(if limit >= usize::MAX as f64 {
        usize::MAX
    } else {
        limit as usize
    })
}

/// Feature-density bins from a source's own coverage summaries: per bin the shallowest
/// overlap (zero unless summaries cover the whole bin), the mean overlap over the bin, and
/// the deepest overlap of any summary touching it.
fn summary_density_bins(
    key: TrackTileKey,
    records: impl Iterator<Item = SummaryRecord>,
) -> BinTile {
    let width = key.nominal_bases_per_point().max(1);
    let mut sums = vec![(0.0_f64, 0_u64); TRACK_TILE_BINS];
    let mut bins = vec![
        TileBin {
            minimum: f32::INFINITY,
            mean: 0.0,
            maximum: 0.0,
        };
        TRACK_TILE_BINS
    ];
    for (start, end, minimum, mean, maximum) in records {
        let (start, end) = (start.max(key.start()), end.min(key.end()));
        if start >= end || !mean.is_finite() {
            continue;
        }
        let (local_start, local_end) = (start - key.start(), end - key.start());
        #[allow(clippy::cast_possible_truncation)]
        for bin in (local_start / width) as usize..=((local_end - 1) / width) as usize {
            let bin_start = bin as u64 * width;
            let overlap = local_end.min(bin_start + width) - local_start.max(bin_start);
            #[allow(clippy::cast_precision_loss)]
            {
                sums[bin].0 += f64::from(mean) * overlap as f64;
            }
            sums[bin].1 += overlap;
            bins[bin].minimum = bins[bin].minimum.min(minimum);
            bins[bin].maximum = bins[bin].maximum.max(maximum);
        }
    }
    for (bin, (sum, covered)) in bins.iter_mut().zip(sums) {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        {
            bin.mean = (sum / width as f64) as f32;
        }
        if covered < width || !bin.minimum.is_finite() {
            bin.minimum = 0.0;
        }
        bin.minimum = bin.minimum.min(bin.mean);
        bin.maximum = bin.maximum.max(bin.mean);
    }
    BinTile {
        bin_width: width,
        bins,
        semantics: BinSemantics::FeatureDensity,
    }
}

/// A bigBed tile: the features when there are few enough to show, read once, or the file's
/// own zoom-level coverage when the tile is dense. See the module documentation.
pub(crate) fn bigbed_native_tile(
    source: &mut BigBedSource,
    request: &TrackTileRequest,
) -> Result<NativeTile, String> {
    if source.zoom_reductions().is_empty() {
        return Ok(NativeTile::Generic);
    }
    let key = request.tile;
    let limit = detail_feature_limit(request)
        .unwrap_or(usize::MAX)
        .min(rgv_model::MAX_FEATURE_RECORDS_PER_QUERY - 1);
    let (records, more) = source
        .query_limited(&request.contig, key.start(), key.end(), limit)
        .map_err(|error| error.to_string())?;
    if !more {
        return Ok(NativeTile::Features(records));
    }
    let Some(summary) = source
        .query_coverage_summary(
            &request.contig,
            key.start(),
            key.end(),
            key.nominal_bases_per_point().max(1),
        )
        .map_err(|error| error.to_string())?
    else {
        return Ok(NativeTile::Generic);
    };
    Ok(NativeTile::Tile(Box::new(TrackTile {
        contig: request.contig.clone(),
        key,
        lod: TrackTileLod::Summary,
        payload: TrackTilePayload::Bins(summary_density_bins(
            key,
            summary.records.iter().map(|record| {
                (
                    record.start,
                    record.end,
                    record.minimum,
                    record.mean,
                    record.maximum,
                )
            }),
        )),
        provenance: TrackTileProvenance {
            // Features read before the tile proved dense, then summaries.
            records_examined: records
                .len()
                .saturating_add(summary.provenance.records_examined),
            records_truncated: summary.provenance.records_truncated,
            source_reduction_bases: summary.provenance.summary_reduction_bases,
            ..TrackTileProvenance::default()
        },
    })))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp, clippy::too_many_lines, clippy::format_push_string)]

    use std::path::Path;

    use rgv_core::TrackId;
    use rgv_model::{CustomTile, TrackSceneInstance, TrackSceneShape, TrackSceneVertical};

    use super::*;
    use crate::{TrackSpec, TrackSpecKind, track_engine::open_tile_source};

    fn open(path: &Path, kind: TrackSpecKind) -> Box<dyn TrackTileSource> {
        open_tile_source(
            &TrackSpec {
                id: TrackId::new(1),
                name: "track".to_owned(),
                path: path.to_owned(),
                kind,
            },
            &mut HashMap::new(),
            &rgv_hts::HttpRangeConfig::default(),
        )
        .unwrap()
    }

    fn request(level: u8, index: u64) -> TrackTileRequest {
        TrackTileRequest::new(
            "chr1",
            TrackTileKey { level, index },
            rgv_model::TrackTileSettings::default(),
        )
    }

    fn write(directory: &tempfile::TempDir, name: &str, text: &str) -> std::path::PathBuf {
        let path = directory.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn the_trait_is_object_safe_and_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<Box<dyn TrackTileSource>>();
    }

    #[test]
    fn bed_features_keep_their_row_across_a_tile_seam() {
        let directory = tempfile::tempdir().unwrap();
        // Overlapping chain across the seam at 1024, then an isolated cluster.
        let path = write(
            &directory,
            "genes.bed",
            "chr1\t900\t1010\ta\t0\t+\n\
             chr1\t950\t1100\tb\t0\t-\n\
             chr1\t1000\t1300\tc\t0\t+\n\
             chr1\t1090\t1200\td\t0\t+\n\
             chr1\t1500\t1600\te\t0\t+\n",
        );
        let mut source = open(&path, TrackSpecKind::Bed);
        let rows = |source: &mut Box<dyn TrackTileSource>, index| {
            let tile = source
                .query_tile(&request(0, index), &TrackCancellation::never())
                .unwrap();
            assert_eq!(tile.provenance.row_stability, TrackRowStability::SeamStable);
            let TrackTilePayload::Features(features) = tile.payload else {
                panic!("expected features");
            };
            features
                .features
                .iter()
                .map(|feature| {
                    (
                        feature.name.clone().unwrap().to_string(),
                        feature.row_hint.unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let left = rows(&mut source, 0);
        let right = rows(&mut source, 1);
        // The left tile sees a, b, c; the right tile b, c, d, e (a ends before the seam).
        assert_eq!(
            left,
            [
                ("a".to_owned(), 0),
                ("b".to_owned(), 1),
                ("c".to_owned(), 2)
            ]
        );
        assert_eq!(
            right,
            [
                ("b".to_owned(), 1),
                ("c".to_owned(), 2),
                ("d".to_owned(), 0),
                ("e".to_owned(), 0)
            ]
        );
    }

    #[test]
    fn dense_features_become_a_coverage_summary_at_the_stated_threshold() {
        let directory = tempfile::tempdir().unwrap();
        let mut text = String::new();
        // 600 features of 100 bases every 100 bases, two deep on the first 20 kb.
        for index in 0..600_u64 {
            let start = (index % 300) * 100;
            text.push_str(&format!("chr1\t{start}\t{}\tf{index}\n", start + 100));
        }
        let path = write(&directory, "dense.bed", &text);
        let mut source = open(&path, TrackSpecKind::Bed);
        // Level 5: 32,768 bases per tile, 600 features in 1,024 points = 0.59 per point.
        let summary = source
            .query_tile(&request(5, 0), &TrackCancellation::never())
            .unwrap();
        assert_eq!(summary.lod, TrackTileLod::Summary);
        let TrackTilePayload::Bins(bins) = &summary.payload else {
            panic!("expected a density summary");
        };
        assert_eq!(bins.semantics, BinSemantics::FeatureDensity);
        assert_eq!((bins.bin_width, bins.bins.len()), (32, 1_024));
        // Bin 1 covers bases 32..64: both features over 0..100 cover it entirely.
        assert_eq!(
            bins.bins[1],
            TileBin {
                minimum: 2.0,
                mean: 2.0,
                maximum: 2.0
            }
        );
        // Bin 3 covers 96..128 and straddles the boundary at 100: four features touch it.
        assert_eq!(
            bins.bins[3],
            TileBin {
                minimum: 0.0,
                mean: 2.0,
                maximum: 4.0
            }
        );
        // Beyond 30 kb there is nothing.
        assert_eq!(
            bins.bins[1_000],
            TileBin {
                minimum: 0.0,
                mean: 0.0,
                maximum: 0.0
            }
        );
        // Raising the threshold (the exposed setting) returns the features themselves.
        let mut detailed = request(5, 0);
        detailed.settings.feature_summary_features_per_point = 1.0;
        let detail = source
            .query_tile(&detailed, &TrackCancellation::never())
            .unwrap();
        assert_eq!(detail.lod, TrackTileLod::Detail);
        let TrackTilePayload::Features(features) = &detail.payload else {
            panic!("expected features");
        };
        assert_eq!(features.features.len(), 600);
        // One level down the same data is sparse enough for features: 1,024 points hold 164.
        let finer = source
            .query_tile(&request(4, 0), &TrackCancellation::never())
            .unwrap();
        assert_eq!(finer.lod, TrackTileLod::Detail);
    }

    #[test]
    fn bedgraph_is_rebinned_to_regular_minimum_mean_maximum_bins() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(
            &directory,
            "signal.bedgraph",
            "chr1\t0\t2\t1.0\nchr1\t2\t3\t9.0\nchr1\t3\t4\t2.0\nchr1\t8\t16\t5.0\n",
        );
        let mut source = open(&path, TrackSpecKind::BedGraph);
        // Level 2: four bases per bin.
        let tile = source
            .query_tile(&request(2, 0), &TrackCancellation::never())
            .unwrap();
        let TrackTilePayload::Bins(bins) = &tile.payload else {
            panic!("expected bins");
        };
        assert_eq!((bins.bin_width, bins.bins.len()), (4, 1_024));
        // The one-base spike survives in the maximum although the mean is 3.25.
        assert_eq!(
            bins.bins[0],
            TileBin {
                minimum: 1.0,
                mean: 3.25,
                maximum: 9.0
            }
        );
        assert!(bins.bins[1].is_empty());
        assert_eq!(
            bins.bins[2],
            TileBin {
                minimum: 5.0,
                mean: 5.0,
                maximum: 5.0
            }
        );
        assert_eq!(bins.bins[3], bins.bins[2]);
        assert!(bins.bins[4].is_empty());
    }

    #[test]
    fn vcf_tiles_carry_a_compact_genotype_matrix_and_summarise_when_dense() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/manual-suite/data/variants/cohort.vcf.gz");
        let mut source = open(&path, TrackSpecKind::VcfOrBcf);
        // Level 2, tile 3 is [12288, 16384): seven of the fixture's eight variants.
        let mut request = TrackTileRequest::new(
            "chrVariants",
            TrackTileKey { level: 2, index: 3 },
            rgv_model::TrackTileSettings::default(),
        );
        let tile = source
            .query_tile(&request, &TrackCancellation::never())
            .unwrap();
        assert_eq!(tile.lod, TrackTileLod::Detail);
        let TrackTilePayload::Variants(variants) = &tile.payload else {
            panic!("expected variants");
        };
        assert_eq!(
            variants
                .variants
                .iter()
                .map(|variant| (variant.start, variant.kind))
                .collect::<Vec<_>>(),
            [
                (15_049, TileVariantKind::Snv),
                (15_149, TileVariantKind::Snv),
                (15_199, TileVariantKind::Snv),
                (15_249, TileVariantKind::Insertion),
                (15_349, TileVariantKind::Deletion),
                (15_449, TileVariantKind::Structural),
                (15_999, TileVariantKind::Structural),
            ]
        );
        let matrix = variants.genotypes.as_ref().unwrap();
        assert_eq!(&matrix.samples[..3], ["child", "father", "mother"]);
        let calls = |variant| {
            (0..4)
                .map(|sample| matrix.call(variant, sample).unwrap())
                .collect::<Vec<_>>()
        };
        // child 0|1, father 1/1, mother ./., cohort-000 0/.
        let expected = [
            GenotypeCall::Het,
            GenotypeCall::HomAlt,
            GenotypeCall::NoCall,
            GenotypeCall::NoCall,
        ];
        assert_eq!(calls(0), expected);
        // Multi-allelic 0|2 is still a het.
        assert_eq!(calls(1), expected);
        // The matrix reaches the scene as palette-token rows: one cell per sample and variant.
        let scene = TrackTileLayoutRegistry::builtin().lay_out(&tile).unwrap();
        let samples = matrix.sample_count();
        assert_eq!(scene.row_count as usize, samples + 1);
        assert_eq!(scene.instances.len(), 7 + 7 * samples);
        assert_eq!(
            scene.instances[7 + 1].color(),
            TrackColor::Token(TrackColorToken::GENOTYPE_HOM_ALT)
        );

        // Lowering the exposed threshold below 7 variants per 1,024 points gives the summary.
        request.settings.variant_summary_variants_per_point = 0.005;
        let summary = source
            .query_tile(&request, &TrackCancellation::never())
            .unwrap();
        assert_eq!(summary.lod, TrackTileLod::Summary);
        let TrackTilePayload::Bins(bins) = &summary.payload else {
            panic!("expected a variant density summary");
        };
        assert_eq!(bins.semantics, BinSemantics::VariantDensity);
        assert_eq!(bins.bins.iter().map(|bin| bin.mean).sum::<f32>(), 7.0);
        assert_eq!(bins.bins[(15_049 - 12_288) / 4].mean, 1.0);
    }

    #[test]
    fn interactions_copy_number_mutations_and_gwas_all_have_tile_adapters() {
        let directory = tempfile::tempdir().unwrap();
        let registry = TrackTileLayoutRegistry::builtin();
        let never = TrackCancellation::never();

        let bedpe = write(
            &directory,
            "loops.bedpe",
            "chr1\t100\t120\tchr1\t700\t720\tloop\t5\nchr1\t300\t320\tchr2\t10\t20\ttrans\t5\n",
        );
        let tile = open(&bedpe, TrackSpecKind::BedPe)
            .query_tile(&request(0, 0), &never)
            .unwrap();
        let TrackTilePayload::Interactions(interactions) = &tile.payload else {
            panic!("expected interactions");
        };
        assert_eq!(interactions.interactions.len(), 2);
        assert_eq!(interactions.interactions[0].target, Some((700, 720)));
        assert_eq!(interactions.interactions[1].target, None);
        let scene = registry.lay_out(&tile).unwrap();
        assert_eq!(
            scene
                .instances
                .iter()
                .filter(|instance| instance.shape() == Some(TrackSceneShape::Arc))
                .count(),
            1
        );

        let seg = write(
            &directory,
            "cohort.seg",
            "ID\tchrom\tloc.start\tloc.end\tnum.mark\tseg.mean\nS1\tchr1\t1\t500\t10\t-1.0\nS2\tchr1\t1\t500\t10\t0.8\n",
        );
        let tile = open(&seg, TrackSpecKind::Seg)
            .query_tile(&request(0, 0), &never)
            .unwrap();
        let TrackTilePayload::Features(segments) = &tile.payload else {
            panic!("expected copy-number rows");
        };
        assert!(matches!(&segments.rows, FeatureRowAxis::Fixed { labels } if labels.len() == 2));
        assert!(matches!(
            segments.features[0].color,
            Some(TrackColor::Ramp {
                low: TrackColorToken::COPY_LOSS,
                ..
            })
        ));
        assert_eq!(registry.lay_out(&tile).unwrap().row_count, 2);

        let maf = write(
            &directory,
            "cohort.maf",
            "Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_Barcode\n\
             TP53\tchr1\t101\t101\tMissense_Mutation\tSNP\tC\tC\tT\tTUMOUR-A\n",
        );
        let tile = open(&maf, TrackSpecKind::MutationMaf)
            .query_tile(&request(0, 0), &never)
            .unwrap();
        let scene = registry.lay_out(&tile).unwrap();
        assert_eq!(scene.records[0].name.as_deref(), Some("TP53"));
        assert_eq!(scene.row_labels.as_ref().unwrap().len(), 1);

        let gwas = write(
            &directory,
            "study.gwas",
            "CHR\tBP\tSNP\tP\nchr1\t150\trs9\t1e-8\n",
        );
        let tile = open(&gwas, TrackSpecKind::Gwas)
            .query_tile(&request(0, 0), &never)
            .unwrap();
        let scene = registry.lay_out(&tile).unwrap();
        assert_eq!(scene.instances[0].vertical(), TrackSceneVertical::Value);
        assert_eq!(scene.instances[0].y0, 8.0);
        assert_eq!(scene.value_range, Some((0.0, 8.0)));
    }

    #[test]
    fn a_cancelled_request_reads_nothing_and_reports_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let path = write(&directory, "one.bed", "chr1\t1\t2\n");
        let mut source = open(&path, TrackSpecKind::Bed);
        let cancelled = TrackCancellation::new(|| true);
        assert_eq!(
            source.query_tile(&request(0, 0), &cancelled).unwrap_err(),
            TrackTileError::Cancelled
        );
    }

    /// A plugin: its own source, payload type and layout, with no change to any built-in code.
    struct HeatmapSource;
    struct HeatmapPayload {
        cells: Vec<(u64, u64, u8)>,
    }
    const HEATMAP: TrackRendererId = TrackRendererId::from_static("plugin.heatmap");

    impl TrackTileSource for HeatmapSource {
        fn query_tile(
            &mut self,
            request: &TrackTileRequest,
            _cancel: &TrackCancellation,
        ) -> Result<TrackTile, TrackTileError> {
            let start = request.tile.start();
            Ok(TrackTile {
                contig: request.contig.clone(),
                key: request.tile,
                lod: TrackTileLod::Detail,
                payload: TrackTilePayload::Custom(CustomTile {
                    renderer: HEATMAP,
                    payload: Arc::new(HeatmapPayload {
                        cells: vec![(start, start + 10, 200), (start + 10, start + 20, 40)],
                    }),
                    resident_bytes: 48,
                }),
                provenance: TrackTileProvenance::default(),
            })
        }
    }

    #[test]
    fn a_plugin_adds_a_payload_kind_by_registering_one_layout() {
        let mut registry = TrackTileLayoutRegistry::builtin();
        let tile = HeatmapSource
            .query_tile(&request(0, 3), &TrackCancellation::never())
            .unwrap();
        assert_eq!(
            registry.lay_out(&tile).unwrap_err(),
            TrackTileError::UnknownRenderer("plugin.heatmap".to_owned())
        );
        registry.register(HEATMAP, |tile| {
            let TrackTilePayload::Custom(custom) = &tile.payload else {
                return Err(TrackTileError::Source("not a heatmap".to_owned()));
            };
            let payload = custom
                .payload
                .downcast_ref::<HeatmapPayload>()
                .ok_or_else(|| TrackTileError::Source("not a heatmap".to_owned()))?;
            let mut scene = TrackTileScene::empty(&tile.contig, tile.key, HEATMAP);
            scene.row_count = 1;
            scene.instances = payload
                .cells
                .iter()
                .map(|(start, end, heat)| {
                    TrackSceneInstance::new(
                        tile.key,
                        *start,
                        *end,
                        0,
                        TrackSceneShape::Box,
                        TrackSceneVertical::Row,
                        0,
                        (0.0, 1.0),
                        TrackColor::Inline([*heat, 0, 0, 255]),
                        rgv_model::TRACK_SCENE_NO_RECORD,
                    )
                })
                .collect();
            Ok(scene)
        });
        let scene = registry.lay_out(&tile).unwrap();
        assert_eq!(scene.instances.len(), 2);
        assert_eq!(scene.instances[1].start, 10);
        assert_eq!(
            scene.instances[0].color(),
            TrackColor::Inline([200, 0, 0, 255])
        );
    }

    /// A version-4 TDF with one channel and one zoom level holding a dataset per entry of
    /// `functions`: three fixed-step values of ten bases each from position 100.
    fn tdf_with_functions(functions: &[(&str, [f32; 3])]) -> Vec<u8> {
        fn string(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        let mut header = Vec::new();
        header.extend_from_slice(&(functions.len() as u32).to_le_bytes());
        for (function, _) in functions {
            string(&mut header, function);
        }
        string(&mut header, "OTHER");
        string(&mut header, "");
        header.extend_from_slice(&1_u32.to_le_bytes());
        string(&mut header, "signal");
        string(&mut header, "hg38");
        header.extend_from_slice(&0_u32.to_le_bytes());
        let base = (24 + header.len()) as u64;
        let mut body = Vec::new();
        let mut entries = Vec::new();
        for (function, values) in functions {
            let tile_position = base + body.len() as u64;
            let mut tile = Vec::new();
            string(&mut tile, "fixedStep");
            tile.extend_from_slice(&3_u32.to_le_bytes());
            tile.extend_from_slice(&100_i32.to_le_bytes());
            tile.extend_from_slice(&10_f32.to_le_bytes());
            for value in values {
                tile.extend_from_slice(&value.to_le_bytes());
            }
            body.extend_from_slice(&tile);
            let dataset_position = base + body.len() as u64;
            let mut dataset = Vec::new();
            dataset.extend_from_slice(&0_u32.to_le_bytes());
            string(&mut dataset, "FLOAT");
            dataset.extend_from_slice(&1_000_f32.to_le_bytes());
            dataset.extend_from_slice(&1_u32.to_le_bytes());
            dataset.extend_from_slice(&i64::try_from(tile_position).unwrap().to_le_bytes());
            dataset.extend_from_slice(&(tile.len() as u32).to_le_bytes());
            body.extend_from_slice(&dataset);
            entries.push((
                format!("/chr1/z0/{function}"),
                dataset_position,
                dataset.len() as u32,
            ));
        }
        let mut index = Vec::new();
        index.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (name, position, size) in &entries {
            string(&mut index, name);
            index.extend_from_slice(&position.to_le_bytes());
            index.extend_from_slice(&size.to_le_bytes());
        }
        index.extend_from_slice(&0_u32.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TDF4");
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&(base + body.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(index.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&index);
        bytes
    }

    #[test]
    fn tdf_tiles_keep_the_archives_own_extremes_and_fall_back_to_the_mean_without_them() {
        let directory = tempfile::tempdir().unwrap();
        let bins_of = |name: &str, functions: &[(&str, [f32; 3])]| {
            let path = directory.path().join(name);
            std::fs::write(&path, tdf_with_functions(functions)).unwrap();
            let mut source = open(&path, TrackSpecKind::Tdf);
            // Level 0: one base per bin, so bins 100..130 are the three records.
            let tile = source
                .query_tile(&request(0, 0), &TrackCancellation::never())
                .unwrap();
            assert_eq!(tile.lod, TrackTileLod::Summary);
            assert_eq!(tile.provenance.records_examined, 3);
            let TrackTilePayload::Bins(bins) = tile.payload else {
                panic!("expected bins");
            };
            assert_eq!(bins.semantics, BinSemantics::Signal);
            assert!(bins.bins[99].mean.is_nan() && bins.bins[130].mean.is_nan());
            [bins.bins[100], bins.bins[115], bins.bins[129]]
        };
        // Written with min and max: a 250-high spike inside a bin whose mean is 5 survives.
        let with_extremes = bins_of(
            "extremes.tdf",
            &[
                ("mean", [2.0, 5.0, 3.0]),
                ("min", [0.0, 4.0, 1.0]),
                ("max", [9.0, 250.0, 3.5]),
            ],
        );
        assert_eq!(
            with_extremes,
            [
                TileBin {
                    minimum: 0.0,
                    mean: 2.0,
                    maximum: 9.0
                },
                TileBin {
                    minimum: 4.0,
                    mean: 5.0,
                    maximum: 250.0
                },
                TileBin {
                    minimum: 1.0,
                    mean: 3.0,
                    maximum: 3.5
                },
            ]
        );
        // Written with the mean only (what igvtools does by default): the mean is all there
        // is, and the tile says no more than that.
        let mean_only = bins_of("mean.tdf", &[("mean", [2.0, 5.0, 3.0])]);
        assert_eq!(
            mean_only,
            [2.0_f32, 5.0, 3.0].map(|value| TileBin {
                minimum: value,
                mean: value,
                maximum: value
            })
        );
    }

    #[test]
    fn a_dense_bigbed_tile_is_summarised_from_the_files_zoom_level() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/demo-tracks/features.bigBed");
        if !path.exists() {
            return;
        }
        // Two features on chrVariants, [8700, 9000) and [12100, 12500), and one zoom level
        // of 10,240 bases per summary.
        assert_eq!(
            rgv_hts::BigBedSource::open(&path)
                .unwrap()
                .zoom_reductions(),
            [10_240]
        );
        let mut source = open(&path, TrackSpecKind::BigBed);
        let request_at = |threshold: f32| {
            let mut request = TrackTileRequest::new(
                "chrVariants",
                TrackTileKey { level: 5, index: 0 },
                rgv_model::TrackTileSettings::default(),
            );
            request.settings.feature_summary_features_per_point = threshold;
            request
        };
        // At the default threshold two features in 1,024 points are detail, read once.
        let detail = source
            .query_tile(&request_at(0.5), &TrackCancellation::never())
            .unwrap();
        assert_eq!(detail.lod, TrackTileLod::Detail);
        let TrackTilePayload::Features(features) = &detail.payload else {
            panic!("expected features");
        };
        assert_eq!(
            features
                .features
                .iter()
                .map(|feature| (feature.start, feature.end))
                .collect::<Vec<_>>(),
            [(8_700, 9_000), (12_100, 12_500)]
        );
        assert_eq!(detail.provenance.source_reduction_bases, None);

        // A threshold of one feature per tile makes the same tile dense. One feature is read
        // to find that out; the summary then comes from the zoom level, and says so.
        let summary = source
            .query_tile(&request_at(0.001), &TrackCancellation::never())
            .unwrap();
        assert_eq!(summary.lod, TrackTileLod::Summary);
        assert_eq!(summary.provenance.source_reduction_bases, Some(10_240));
        assert!(!summary.provenance.records_truncated);
        let TrackTilePayload::Bins(bins) = &summary.payload else {
            panic!("expected a density summary");
        };
        assert_eq!(bins.semantics, BinSemantics::FeatureDensity);
        assert_eq!((bins.bin_width, bins.bins.len()), (32, 1_024));
        // The summary conserves coverage: 300 + 400 feature bases.
        let covered = bins
            .bins
            .iter()
            .map(|bin| f64::from(bin.mean) * 32.0)
            .sum::<f64>();
        assert!((covered - 700.0).abs() < 1.0, "{covered}");
        for bin in &bins.bins {
            assert!(
                bin.minimum <= bin.mean && bin.mean <= bin.maximum,
                "{bin:?}"
            );
            assert!(bin.maximum <= 1.0);
        }
        // Nothing before the first feature or far beyond the last.
        assert_eq!(bins.bins[0].maximum, 0.0);
        assert_eq!(bins.bins[1_000].maximum, 0.0);
        assert!(bins.bins.iter().any(|bin| bin.maximum == 1.0));

        // The same through the generic adapter over a shared reader (what the legacy engine
        // hands the tile engine): the native summary is reached through the lock too.
        let shared = crate::track_source::SharedTrackSource::new(Box::new(
            rgv_hts::BigBedSource::open(&path).unwrap(),
        ));
        let mut through_lock = LegacyTrackTileSource::new(Box::new(shared.handle()));
        let again = through_lock
            .query_tile(&request_at(0.001), &TrackCancellation::never())
            .unwrap();
        assert_eq!(again.lod, TrackTileLod::Summary);
        assert_eq!(again.provenance.source_reduction_bases, Some(10_240));
    }
}
