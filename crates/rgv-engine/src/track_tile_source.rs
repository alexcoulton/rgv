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
//!   span and keeps the file's own extremes ([`BigWigSource::query_with_extremes`]).
//! * Feature sources return a coverage summary instead of features once the tile holds more
//!   than `settings.feature_summary_features_per_point` features per nominal point
//!   (default [`rgv_model::DEFAULT_FEATURE_SUMMARY_FEATURES_PER_POINT`]).
//! * Variant sources return per-bin counts above
//!   `settings.variant_summary_variants_per_point`
//!   (default [`rgv_model::DEFAULT_VARIANT_SUMMARY_VARIANTS_PER_POINT`]).

use std::{collections::HashMap, sync::Arc};

use rgv_core::CopyNumberValueMode;
use rgv_hts::{BigWigSource, RemoteCancellation, RemoteTransferBudget};
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
    TrackColorToken, TrackDigest, TrackRecordId, TrackRendererId, TrackRowStability, TrackTile,
    TrackTileError, TrackTileKey, TrackTileLod, TrackTilePayload, TrackTileProvenance,
    TrackTileRequest, TrackTileScene, VariantQuery, VariantType,
};

use crate::{
    TrackSnapshotData, TrackSourceDiagnostics,
    track_engine::REMOTE_TRACK_OPERATION_TRANSFER_BUDGET,
    track_source::{SearchRequest, TrackQuerySettings, TrackSource},
};

/// Most times a feature tile widens its read to reach the end of the overlap clusters that
/// touch it.
pub const MAXIMUM_CLUSTER_CLOSURE_ROUNDS: usize = 8;
/// Furthest a feature tile reads beyond either of its edges for row stability, in tile spans.
pub const MAXIMUM_CLUSTER_CLOSURE_TILES: u64 = 64;

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
}

impl LegacyTrackTileSource {
    pub(crate) fn new(source: Box<dyn TrackSource>) -> Self {
        Self { source }
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
        .map(|record| {
            let alternates = record.alternates.join(&b","[..]);
            TileVariant {
                id: TrackRecordId::from_parts(
                    record.start,
                    record.end,
                    &[
                        record.id.as_deref().unwrap_or_default().as_bytes(),
                        &record.reference,
                        &alternates,
                    ],
                ),
                start: record.start,
                end: record.end,
                kind: variant_kind(&record.variant_types),
                name: record.id.as_deref().map(Arc::from),
                quality: record.quality,
            }
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
        let result = self.query_tile_installed(request, cancel);
        // A later search or feature step must not inherit this request's cancellation.
        self.source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        result
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
        let data = self.query(request, key.start(), key.end(), cancel)?;
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
        let key = request.tile;
        let (keep_going, budget) = remote_control(request.transfer_budget, cancel);
        self.source.set_remote_cancellation(keep_going);
        self.source.set_remote_transfer_budget(budget);
        let result = self
            .source
            .query_with_extremes(&request.contig, key.start(), key.end());
        self.source.set_remote_cancellation(None);
        let query = result.map_err(|error| source_error(error.to_string(), cancel))?;
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
}
