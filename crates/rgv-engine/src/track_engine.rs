use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use rgv_core::{
    CopyNumberTrackSettings, GenerationClock, GenomicInterval, InteractionTrackSettings,
    PanelGeneration, TrackId, VariantTrackSettings,
};
use rgv_hts::{
    BigBedFeatureFormat, BigBedSource, BigWigSource, CopyNumberQuery, FeatureFormat, FeatureRecord,
    GwasQuery, InteractionFormat, InteractionQuery, LocalVariantSource, MAX_FEATURE_SEARCH_RESULTS,
    MutationQuery, QuantitativeFormat, QuantitativeQuery, Strand, TdfSource, TextCopyNumberSource,
    TextFeatureSource, TextGwasSource, TextInteractionSource, TextMutationSource,
    TextQuantitativeSource, VariantQuery,
};

use crate::{
    EngineQueueError,
    mailbox::{CoalescingReceiver, CoalescingSender, bounded_coalescing},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackSpecKind {
    Bed,
    Gff3,
    Gtf,
    GenePred,
    RefFlat,
    NarrowPeak,
    BroadPeak,
    Psl,
    Gwas,
    Tdf,
    TdfChannel { index: usize },
    VcfOrBcf,
    BigWig,
    BedGraph,
    Wig,
    BedPe,
    Interact,
    Seg,
    MutationMaf,
    BigBed,
    BigGenePred,
    BigNarrowPeak,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackSpec {
    pub id: TrackId,
    pub name: String,
    pub path: PathBuf,
    pub kind: TrackSpecKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackSnapshotData {
    Features(Vec<FeatureRecord>),
    Variants(Arc<VariantQuery>),
    Quantitative(Arc<QuantitativeQuery>),
    CopyNumber(Arc<CopyNumberQuery>),
    Mutations(Arc<MutationQuery>),
    Interactions(Arc<InteractionQuery>),
    Gwas(Arc<GwasQuery>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSnapshot {
    pub id: TrackId,
    pub name: String,
    pub data: TrackSnapshotData,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSearchHit {
    pub track_id: TrackId,
    pub track_name: String,
    pub matched_term: String,
    pub record: FeatureRecord,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackSearchResults {
    pub query: String,
    pub hits: Vec<TrackSearchHit>,
    pub index_truncated: bool,
    pub scan_truncated: bool,
    pub variant_ids_included: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackEngineEvent {
    Ready {
        opened: usize,
        failed: usize,
    },
    TrackAdded {
        id: TrackId,
    },
    TracksAdded {
        ids: Vec<TrackId>,
    },
    TracksRemoved {
        ids: Vec<TrackId>,
    },
    TdfChannelsDiscovered {
        id: TrackId,
        track_names: Vec<String>,
        selected_index: usize,
    },
    QueryCompleted {
        request: PanelGeneration,
        /// Exact buffered horizon used to build every snapshot in this publication.
        interval: GenomicInterval,
        /// Successfully published lane identities. Lane payloads arrive independently before
        /// this settlement marker and are never combined into a panel-wide data authority.
        completed: Vec<TrackId>,
    },
    LaneCompleted {
        request: PanelGeneration,
        interval: GenomicInterval,
        track: TrackSnapshot,
    },
    SearchCompleted {
        generation: u64,
        results: TrackSearchResults,
    },
    AdjacentFeatureCompleted {
        generation: u64,
        hit: Option<Box<TrackSearchHit>>,
    },
    Error {
        request: Option<PanelGeneration>,
        track: Option<TrackId>,
        message: String,
    },
}

enum TrackEngineRequest {
    Add {
        specification: TrackSpec,
        query_contig_aliases: BTreeMap<String, String>,
    },
    AddMany(Vec<TrackSpec>),
    SetQueryContigAliases {
        id: TrackId,
        query_contig_aliases: BTreeMap<String, String>,
    },
    Remove(Vec<TrackId>),
    Query {
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
        track_ids: Option<Vec<TrackId>>,
        variant_settings: Vec<(TrackId, VariantTrackSettings)>,
        interaction_settings: Vec<(TrackId, InteractionTrackSettings)>,
        copy_number_settings: Vec<(TrackId, CopyNumberTrackSettings)>,
    },
    Search {
        generation: u64,
        query: String,
        limit: usize,
        include_variant_ids: bool,
    },
    AdjacentFeature {
        generation: u64,
        contig: String,
        position: u64,
        forward: bool,
    },
}

enum OpenTrack {
    Text {
        id: TrackId,
        name: String,
        source: TextFeatureSource,
    },
    Variants {
        id: TrackId,
        name: String,
        source: LocalVariantSource,
    },
    BigWig {
        id: TrackId,
        name: String,
        source: BigWigSource,
    },
    TextQuantitative {
        id: TrackId,
        name: String,
        source: TextQuantitativeSource,
    },
    Interactions {
        id: TrackId,
        name: String,
        source: TextInteractionSource,
    },
    CopyNumber {
        id: TrackId,
        name: String,
        source: TextCopyNumberSource,
    },
    Mutations {
        id: TrackId,
        name: String,
        source: TextMutationSource,
    },
    Gwas {
        id: TrackId,
        name: String,
        source: TextGwasSource,
    },
    Tdf {
        id: TrackId,
        name: String,
        source: Rc<RefCell<TdfSource>>,
        track_index: usize,
    },
    BigBed {
        id: TrackId,
        name: String,
        source: BigBedSource,
    },
}

impl OpenTrack {
    fn id(&self) -> TrackId {
        match self {
            Self::Text { id, .. }
            | Self::Variants { id, .. }
            | Self::BigWig { id, .. }
            | Self::TextQuantitative { id, .. }
            | Self::Interactions { id, .. }
            | Self::CopyNumber { id, .. }
            | Self::Mutations { id, .. }
            | Self::Gwas { id, .. }
            | Self::Tdf { id, .. }
            | Self::BigBed { id, .. } => *id,
        }
    }
}

pub struct TrackEngine {
    input: Option<Sender<TrackEngineRequest>>,
    output: CoalescingReceiver<TrackEngineEvent>,
    worker: Option<JoinHandle<()>>,
}

impl TrackEngine {
    pub fn spawn(specifications: Vec<TrackSpec>, capacity: usize) -> Self {
        Self::spawn_with_query_contig_aliases(
            specifications
                .into_iter()
                .map(|specification| (specification, BTreeMap::new()))
                .collect(),
            capacity,
        )
    }

    pub fn spawn_with_query_contig_aliases(
        specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
        capacity: usize,
    ) -> Self {
        let (input_tx, input_rx) = bounded(capacity.max(1));
        let (output_tx, output_rx) = bounded_coalescing(capacity.max(1));
        let worker = thread::Builder::new()
            .name("rgv-track-actor".to_owned())
            .spawn(move || worker_loop(specifications, &input_rx, &output_tx))
            .expect("failed to spawn RGV track actor");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
        }
    }

    pub fn try_query(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
    ) -> Result<(), EngineQueueError> {
        self.try_query_with_settings(
            request,
            clock,
            contig,
            interval,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    pub fn try_query_with_variant_settings(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
        variant_settings: Vec<(TrackId, VariantTrackSettings)>,
    ) -> Result<(), EngineQueueError> {
        self.try_query_with_settings(
            request,
            clock,
            contig,
            interval,
            variant_settings,
            Vec::new(),
            Vec::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_query_with_settings(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
        variant_settings: Vec<(TrackId, VariantTrackSettings)>,
        interaction_settings: Vec<(TrackId, InteractionTrackSettings)>,
        copy_number_settings: Vec<(TrackId, CopyNumberTrackSettings)>,
    ) -> Result<(), EngineQueueError> {
        self.try_query_internal(
            request,
            clock,
            contig,
            interval,
            None,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_query_selected_with_settings(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
        track_ids: Vec<TrackId>,
        variant_settings: Vec<(TrackId, VariantTrackSettings)>,
        interaction_settings: Vec<(TrackId, InteractionTrackSettings)>,
        copy_number_settings: Vec<(TrackId, CopyNumberTrackSettings)>,
    ) -> Result<(), EngineQueueError> {
        self.try_query_internal(
            request,
            clock,
            contig,
            interval,
            Some(track_ids),
            variant_settings,
            interaction_settings,
            copy_number_settings,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn try_query_internal(
        &self,
        request: PanelGeneration,
        clock: GenerationClock,
        contig: String,
        interval: GenomicInterval,
        track_ids: Option<Vec<TrackId>>,
        variant_settings: Vec<(TrackId, VariantTrackSettings)>,
        interaction_settings: Vec<(TrackId, InteractionTrackSettings)>,
        copy_number_settings: Vec<(TrackId, CopyNumberTrackSettings)>,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::Query {
                request,
                clock,
                contig,
                interval,
                track_ids,
                variant_settings,
                interaction_settings,
                copy_number_settings,
            })
            .map_err(map_send_error)
    }

    pub fn try_add(&self, specification: TrackSpec) -> Result<(), EngineQueueError> {
        self.try_add_with_query_contig_aliases(specification, BTreeMap::new())
    }

    /// Adds a track whose source uses different contig spellings from the
    /// workspace dictionary. Keys are workspace contigs and values are exact
    /// names understood by the source (for example `1 -> chr1`).
    pub fn try_add_with_query_contig_aliases(
        &self,
        specification: TrackSpec,
        query_contig_aliases: BTreeMap<String, String>,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::Add {
                specification,
                query_contig_aliases,
            })
            .map_err(map_send_error)
    }

    pub fn try_add_many(&self, specifications: Vec<TrackSpec>) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::AddMany(specifications))
            .map_err(map_send_error)
    }

    pub fn try_set_query_contig_aliases(
        &self,
        id: TrackId,
        query_contig_aliases: BTreeMap<String, String>,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::SetQueryContigAliases {
                id,
                query_contig_aliases,
            })
            .map_err(map_send_error)
    }

    pub fn try_remove_many(&self, ids: Vec<TrackId>) -> Result<(), EngineQueueError> {
        if ids.is_empty() {
            return Ok(());
        }
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::Remove(ids))
            .map_err(map_send_error)
    }

    pub fn try_search(
        &self,
        generation: u64,
        query: String,
        limit: usize,
        include_variant_ids: bool,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::Search {
                generation,
                query,
                limit: limit.min(MAX_FEATURE_SEARCH_RESULTS),
                include_variant_ids,
            })
            .map_err(map_send_error)
    }

    pub fn try_adjacent_feature(
        &self,
        generation: u64,
        contig: String,
        position: u64,
        forward: bool,
    ) -> Result<(), EngineQueueError> {
        let Some(input) = &self.input else {
            return Err(EngineQueueError::Disconnected);
        };
        input
            .try_send(TrackEngineRequest::AdjacentFeature {
                generation,
                contig,
                position,
                forward,
            })
            .map_err(map_send_error)
    }

    pub fn try_receive(&self) -> Result<Option<TrackEngineEvent>, EngineQueueError> {
        self.output
            .try_receive()
            .map_err(|_| EngineQueueError::Disconnected)
    }

    /// Close ownership immediately, leaving the caller to poll and join the
    /// worker. Queued obsolete opens and queries are abandoned on closure.
    #[must_use]
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.input.take();
        self.output.close();
        self.worker.take()
    }
}

impl Drop for TrackEngine {
    fn drop(&mut self) {
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("track actor panicked while shutting down");
        }
    }
}

fn worker_loop(
    specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
    input: &Receiver<TrackEngineRequest>,
    output: &CoalescingSender<TrackEngineEvent>,
) {
    let mut tracks = Vec::new();
    let mut tdf_sources = HashMap::new();
    let mut query_contig_aliases = HashMap::new();
    let mut failed = 0;
    for (specification, aliases) in specifications {
        if output.is_closed() {
            return;
        }
        match open_and_push_track(&specification, &mut tracks, &mut tdf_sources, output) {
            Ok(id) => {
                if !aliases.is_empty() {
                    query_contig_aliases.insert(id, aliases);
                }
            }
            Err(message) => {
                failed += 1;
                send_track_open_error(output, &specification, &message);
            }
        }
    }
    send_event(
        output,
        TrackEngineEvent::Ready {
            opened: tracks.len(),
            failed,
        },
    );
    while let Ok(request) = input.recv() {
        if output.is_closed() {
            break;
        }
        match request {
            TrackEngineRequest::Add {
                specification,
                query_contig_aliases: aliases,
            } => match open_and_push_track(&specification, &mut tracks, &mut tdf_sources, output) {
                Ok(id) => {
                    if !aliases.is_empty() {
                        query_contig_aliases.insert(id, aliases);
                    }
                    send_event(output, TrackEngineEvent::TrackAdded { id });
                }
                Err(message) => send_track_open_error(output, &specification, &message),
            },
            TrackEngineRequest::AddMany(specifications) => {
                let added =
                    open_and_push_tracks(specifications, &mut tracks, &mut tdf_sources, output);
                if !added.is_empty() {
                    send_event(output, TrackEngineEvent::TracksAdded { ids: added });
                }
            }
            TrackEngineRequest::SetQueryContigAliases {
                id,
                query_contig_aliases: aliases,
            } => {
                if tracks.iter().any(|track| track.id() == id) {
                    if aliases.is_empty() {
                        query_contig_aliases.remove(&id);
                    } else {
                        query_contig_aliases.insert(id, aliases);
                    }
                }
            }
            TrackEngineRequest::Remove(ids) => {
                let removed = remove_tracks(
                    &mut tracks,
                    &mut tdf_sources,
                    &mut query_contig_aliases,
                    ids,
                );
                if !removed.is_empty() {
                    send_event(output, TrackEngineEvent::TracksRemoved { ids: removed });
                }
            }
            TrackEngineRequest::Query {
                request,
                clock,
                contig,
                interval,
                track_ids,
                variant_settings,
                interaction_settings,
                copy_number_settings,
            } => query_tracks(
                &mut tracks,
                &query_contig_aliases,
                output,
                &clock,
                request,
                &contig,
                interval,
                track_ids.as_deref(),
                &variant_settings,
                &interaction_settings,
                &copy_number_settings,
            ),
            TrackEngineRequest::Search {
                generation,
                query,
                limit,
                include_variant_ids,
            } => send_event(
                output,
                TrackEngineEvent::SearchCompleted {
                    generation,
                    results: search_tracks(&mut tracks, query, limit, include_variant_ids),
                },
            ),
            TrackEngineRequest::AdjacentFeature {
                generation,
                contig,
                position,
                forward,
            } => send_event(
                output,
                TrackEngineEvent::AdjacentFeatureCompleted {
                    generation,
                    hit: adjacent_feature(
                        &mut tracks,
                        &query_contig_aliases,
                        &contig,
                        position,
                        forward,
                    )
                    .map(Box::new),
                },
            ),
        }
    }
}

fn open_and_push_track(
    specification: &TrackSpec,
    tracks: &mut Vec<OpenTrack>,
    tdf_sources: &mut TdfSourceCache,
    output: &CoalescingSender<TrackEngineEvent>,
) -> Result<TrackId, String> {
    let track = open_track_cached(specification, tdf_sources)?;
    send_tdf_discovery(output, specification, &track);
    let id = specification.id;
    tracks.push(track);
    Ok(id)
}

fn open_and_push_tracks(
    specifications: Vec<TrackSpec>,
    tracks: &mut Vec<OpenTrack>,
    tdf_sources: &mut TdfSourceCache,
    output: &CoalescingSender<TrackEngineEvent>,
) -> Vec<TrackId> {
    let mut added = Vec::new();
    for specification in specifications {
        if output.is_closed() {
            break;
        }
        match open_and_push_track(&specification, tracks, tdf_sources, output) {
            Ok(id) => added.push(id),
            Err(message) => send_track_open_error(output, &specification, &message),
        }
    }
    added
}

fn send_track_open_error(
    output: &CoalescingSender<TrackEngineEvent>,
    specification: &TrackSpec,
    message: &str,
) {
    send_event(
        output,
        TrackEngineEvent::Error {
            request: None,
            track: Some(specification.id),
            message: format!("{}: {message}", specification.name),
        },
    );
}

fn remove_tracks(
    tracks: &mut Vec<OpenTrack>,
    tdf_sources: &mut TdfSourceCache,
    query_contig_aliases: &mut HashMap<TrackId, BTreeMap<String, String>>,
    ids: Vec<TrackId>,
) -> Vec<TrackId> {
    let requested = ids.into_iter().collect::<HashSet<_>>();
    let mut removed = tracks
        .iter()
        .map(OpenTrack::id)
        .filter(|id| requested.contains(id))
        .collect::<Vec<_>>();
    removed.sort_unstable();
    removed.dedup();
    if removed.is_empty() {
        return removed;
    }
    let removed_set = removed.iter().copied().collect::<HashSet<_>>();
    tracks.retain(|track| !removed_set.contains(&track.id()));
    query_contig_aliases.retain(|id, _| !removed_set.contains(id));
    tdf_sources.retain(|_, source| Rc::strong_count(source) > 1);
    removed
}

fn send_tdf_discovery(
    output: &CoalescingSender<TrackEngineEvent>,
    specification: &TrackSpec,
    track: &OpenTrack,
) {
    if specification.kind != TrackSpecKind::Tdf {
        return;
    }
    let OpenTrack::Tdf {
        source,
        track_index,
        ..
    } = track
    else {
        return;
    };
    send_event(
        output,
        TrackEngineEvent::TdfChannelsDiscovered {
            id: specification.id,
            track_names: source.borrow().metadata().track_names.clone(),
            selected_index: *track_index,
        },
    );
}

fn adjacent_feature(
    tracks: &mut [OpenTrack],
    query_contig_aliases: &HashMap<TrackId, BTreeMap<String, String>>,
    contig: &str,
    position: u64,
    forward: bool,
) -> Option<TrackSearchHit> {
    let mut hits = tracks
        .iter_mut()
        .filter_map(|track| {
            let query_contig = track_query_contig(query_contig_aliases, track.id(), contig);
            let (id, name, record) = match track {
                OpenTrack::Text { id, name, source } => {
                    (*id, name.clone(), source.adjacent_feature(query_contig, position, forward)?)
                }
                OpenTrack::BigBed { id, name, source } => match source
                    .adjacent_feature(query_contig, position, forward)
                {
                    Ok(Some(record)) => (*id, name.clone(), record),
                    Ok(None) => return None,
                    Err(error) => {
                        tracing::warn!(%error, track = %name, "BigBed feature navigation failed");
                        return None;
                    }
                },
                OpenTrack::Gwas { id, name, source } => {
                    let association =
                        source.adjacent_association(query_contig, position, forward)?;
                    (*id, name.clone(), gwas_feature_record(association))
                }
                OpenTrack::Variants { .. }
                | OpenTrack::BigWig { .. }
                | OpenTrack::Tdf { .. }
                | OpenTrack::TextQuantitative { .. }
                | OpenTrack::Interactions { .. }
                | OpenTrack::CopyNumber { .. }
                | OpenTrack::Mutations { .. } => return None,
            };
            Some(TrackSearchHit {
                track_id: id,
                track_name: name,
                matched_term: record.name.as_deref().unwrap_or_default().to_lowercase(),
                record,
            })
        })
        .collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        let ordering = left
            .record
            .start
            .cmp(&right.record.start)
            .then_with(|| left.record.end.cmp(&right.record.end))
            .then_with(|| left.track_id.cmp(&right.track_id));
        if forward {
            ordering
        } else {
            ordering.reverse()
        }
    });
    hits.into_iter().next()
}

#[allow(clippy::too_many_lines)]
fn search_tracks(
    tracks: &mut [OpenTrack],
    query: String,
    limit: usize,
    include_variant_ids: bool,
) -> TrackSearchResults {
    let normalized_query = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut index_truncated = false;
    let mut scan_truncated = false;
    for track in tracks {
        match track {
            OpenTrack::Text { id, name, source } => {
                let results = source.search(&normalized_query, limit);
                index_truncated |= results.index_truncated;
                scan_truncated |= results.scan_truncated;
                hits.extend(results.hits.into_iter().map(|hit| TrackSearchHit {
                    track_id: *id,
                    track_name: name.clone(),
                    matched_term: hit.matched_term,
                    record: hit.record,
                }));
            }
            OpenTrack::Variants { id, name, source } if include_variant_ids => {
                match source.search_ids(&query, limit) {
                    Ok(results) => {
                        index_truncated |= results.index_truncated;
                        scan_truncated |= results.scan_truncated;
                        hits.extend(results.hits.into_iter().map(|hit| TrackSearchHit {
                            track_id: *id,
                            track_name: name.clone(),
                            matched_term: hit.matched_id,
                            record: FeatureRecord {
                                contig: hit.contig,
                                start: hit.start,
                                end: hit.end,
                                name: Some(hit.id),
                                feature_type: Some("variant".to_owned()),
                                score: None,
                                strand: Strand::Unstranded,
                                blocks: Vec::new(),
                                coding_interval: None,
                                peak: None,
                                attributes: Vec::new(),
                            },
                        }));
                    }
                    Err(error) => tracing::warn!(%error, track = %name, "variant ID index failed"),
                }
            }
            OpenTrack::Variants { .. }
            | OpenTrack::BigWig { .. }
            | OpenTrack::TextQuantitative { .. }
            | OpenTrack::Interactions { .. }
            | OpenTrack::CopyNumber { .. }
            | OpenTrack::Mutations { .. }
            | OpenTrack::Tdf { .. } => {}
            OpenTrack::Gwas { id, name, source } => {
                let results = source.search(&query, limit);
                index_truncated |= results.index_truncated;
                scan_truncated |= results.scan_truncated;
                hits.extend(results.hits.into_iter().map(|hit| TrackSearchHit {
                    track_id: *id,
                    track_name: name.clone(),
                    matched_term: hit.matched_term,
                    record: gwas_feature_record(hit.record),
                }));
            }
            OpenTrack::BigBed { id, name, source } => match source.search(&query, limit) {
                Ok(results) => {
                    index_truncated |= results.index_truncated;
                    scan_truncated |= results.scan_truncated;
                    hits.extend(results.hits.into_iter().map(|hit| TrackSearchHit {
                        track_id: *id,
                        track_name: name.clone(),
                        matched_term: hit.matched_term,
                        record: hit.record,
                    }));
                }
                Err(error) => tracing::warn!(%error, track = %name, "BigBed feature search failed"),
            },
        }
    }
    hits.sort_by(|left, right| {
        let left_exact = left.matched_term == normalized_query;
        let right_exact = right.matched_term == normalized_query;
        right_exact
            .cmp(&left_exact)
            .then_with(|| left.matched_term.cmp(&right.matched_term))
            .then_with(|| left.record.contig.cmp(&right.record.contig))
            .then_with(|| left.record.start.cmp(&right.record.start))
            .then_with(|| left.record.end.cmp(&right.record.end))
            .then_with(|| left.track_id.cmp(&right.track_id))
    });
    hits.dedup_by(|left, right| {
        left.track_id == right.track_id
            && left.record.contig == right.record.contig
            && left.record.start == right.record.start
            && left.record.end == right.record.end
            && left.record.name == right.record.name
    });
    hits.truncate(limit.min(MAX_FEATURE_SEARCH_RESULTS));
    TrackSearchResults {
        query,
        hits,
        index_truncated,
        scan_truncated,
        variant_ids_included: include_variant_ids,
    }
}

fn gwas_feature_record(record: rgv_hts::GwasRecord) -> FeatureRecord {
    FeatureRecord {
        contig: record.contig,
        start: record.position,
        end: record.position.saturating_add(1),
        name: Some(record.identifier),
        feature_type: Some("GWAS association".to_owned()),
        score: record
            .minus_log10_p
            .is_finite()
            .then_some(record.minus_log10_p),
        strand: Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: record.fields,
    }
}

#[allow(clippy::too_many_arguments)]
fn query_tracks(
    tracks: &mut [OpenTrack],
    query_contig_aliases: &HashMap<TrackId, BTreeMap<String, String>>,
    output: &CoalescingSender<TrackEngineEvent>,
    clock: &GenerationClock,
    request: PanelGeneration,
    contig: &str,
    interval: GenomicInterval,
    track_ids: Option<&[TrackId]>,
    variant_settings: &[(TrackId, VariantTrackSettings)],
    interaction_settings: &[(TrackId, InteractionTrackSettings)],
    copy_number_settings: &[(TrackId, CopyNumberTrackSettings)],
) {
    if !clock.is_current(request.generation()) {
        return;
    }
    let selected = track_ids.map(|ids| ids.iter().copied().collect::<HashSet<_>>());
    let mut completed = Vec::with_capacity(selected.as_ref().map_or(tracks.len(), HashSet::len));
    for track in tracks {
        if output.is_closed() || !clock.is_current(request.generation()) {
            break;
        }
        if selected
            .as_ref()
            .is_some_and(|selected| !selected.contains(&track.id()))
        {
            continue;
        }
        let query_contig = track_query_contig(query_contig_aliases, track.id(), contig);
        match query_track(
            track,
            query_contig,
            interval.start(),
            interval.end(),
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(snapshot) => {
                completed.push(snapshot.id);
                send_event(
                    output,
                    TrackEngineEvent::LaneCompleted {
                        request,
                        interval,
                        track: snapshot,
                    },
                );
            }
            Err((id, message)) => send_event(
                output,
                TrackEngineEvent::Error {
                    request: Some(request),
                    track: Some(id),
                    message,
                },
            ),
        }
    }
    if clock.is_current(request.generation()) {
        send_event(
            output,
            TrackEngineEvent::QueryCompleted {
                request,
                interval,
                completed,
            },
        );
    }
}

fn track_query_contig<'a>(
    aliases: &'a HashMap<TrackId, BTreeMap<String, String>>,
    track_id: TrackId,
    workspace_contig: &'a str,
) -> &'a str {
    aliases
        .get(&track_id)
        .and_then(|aliases| aliases.get(workspace_contig))
        .map_or(workspace_contig, String::as_str)
}

type TdfSourceCache = HashMap<PathBuf, Rc<RefCell<TdfSource>>>;

#[cfg(test)]
fn open_track(specification: &TrackSpec) -> Result<OpenTrack, String> {
    open_track_cached(specification, &mut HashMap::new())
}

fn open_track_cached(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
) -> Result<OpenTrack, String> {
    let id = specification.id;
    let name = specification.name.clone();
    match specification.kind {
        TrackSpecKind::Bed
        | TrackSpecKind::Gff3
        | TrackSpecKind::Gtf
        | TrackSpecKind::GenePred
        | TrackSpecKind::RefFlat
        | TrackSpecKind::NarrowPeak
        | TrackSpecKind::BroadPeak
        | TrackSpecKind::Psl => {
            let format = match specification.kind {
                TrackSpecKind::Bed => FeatureFormat::Bed,
                TrackSpecKind::Gff3 => FeatureFormat::Gff3,
                TrackSpecKind::Gtf => FeatureFormat::Gtf,
                TrackSpecKind::GenePred => FeatureFormat::GenePred,
                TrackSpecKind::RefFlat => FeatureFormat::RefFlat,
                TrackSpecKind::NarrowPeak => FeatureFormat::NarrowPeak,
                TrackSpecKind::BroadPeak => FeatureFormat::BroadPeak,
                TrackSpecKind::Psl => FeatureFormat::Psl,
                _ => unreachable!(),
            };
            TextFeatureSource::open(&specification.path, format)
                .map(|source| OpenTrack::Text { id, name, source })
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::VcfOrBcf => LocalVariantSource::open(&specification.path)
            .map(|source| OpenTrack::Variants { id, name, source })
            .map_err(|error| error.to_string()),
        TrackSpecKind::Gwas => TextGwasSource::open(&specification.path)
            .map(|source| OpenTrack::Gwas { id, name, source })
            .map_err(|error| error.to_string()),
        TrackSpecKind::BigWig => BigWigSource::open(&specification.path)
            .map(|source| OpenTrack::BigWig { id, name, source })
            .map_err(|error| error.to_string()),
        TrackSpecKind::Tdf | TrackSpecKind::TdfChannel { .. } => {
            open_tdf_track(specification, tdf_sources)
        }
        TrackSpecKind::BedGraph | TrackSpecKind::Wig => {
            let format = if specification.kind == TrackSpecKind::BedGraph {
                QuantitativeFormat::BedGraph
            } else {
                QuantitativeFormat::Wig
            };
            TextQuantitativeSource::open(&specification.path, format)
                .map(|source| OpenTrack::TextQuantitative { id, name, source })
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::BedPe | TrackSpecKind::Interact => {
            let format = if specification.kind == TrackSpecKind::BedPe {
                InteractionFormat::BedPe
            } else {
                InteractionFormat::Interact
            };
            TextInteractionSource::open(&specification.path, format)
                .map(|source| OpenTrack::Interactions { id, name, source })
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::Seg => TextCopyNumberSource::open(&specification.path)
            .map(|source| OpenTrack::CopyNumber { id, name, source })
            .map_err(|error| error.to_string()),
        TrackSpecKind::MutationMaf => TextMutationSource::open(&specification.path)
            .map(|source| OpenTrack::Mutations { id, name, source })
            .map_err(|error| error.to_string()),
        TrackSpecKind::BigBed | TrackSpecKind::BigGenePred | TrackSpecKind::BigNarrowPeak => {
            let format = match specification.kind {
                TrackSpecKind::BigBed => BigBedFeatureFormat::Bed,
                TrackSpecKind::BigGenePred => BigBedFeatureFormat::BigGenePred,
                TrackSpecKind::BigNarrowPeak => BigBedFeatureFormat::BigNarrowPeak,
                _ => unreachable!(),
            };
            BigBedSource::open_with_format(&specification.path, format)
                .map(|source| OpenTrack::BigBed { id, name, source })
                .map_err(|error| error.to_string())
        }
    }
}

fn open_tdf_track(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
) -> Result<OpenTrack, String> {
    let source = if let Some(source) = tdf_sources.get(&specification.path) {
        Rc::clone(source)
    } else {
        let source = Rc::new(RefCell::new(
            TdfSource::open(&specification.path).map_err(|error| error.to_string())?,
        ));
        tdf_sources.insert(specification.path.clone(), Rc::clone(&source));
        source
    };
    let track_index = match specification.kind {
        TrackSpecKind::Tdf => source
            .borrow()
            .track_index_by_name(&specification.name)
            .unwrap_or(0),
        TrackSpecKind::TdfChannel { index } => index,
        _ => unreachable!(),
    };
    let channel_count = source.borrow().metadata().track_names.len();
    if track_index >= channel_count {
        return Err(format!(
            "TDF channel index {track_index} is outside the {channel_count} embedded channels"
        ));
    }
    Ok(OpenTrack::Tdf {
        id: specification.id,
        name: specification.name.clone(),
        source,
        track_index,
    })
}

fn query_track(
    track: &mut OpenTrack,
    contig: &str,
    start: u64,
    end: u64,
    variant_settings: &[(TrackId, VariantTrackSettings)],
    interaction_settings: &[(TrackId, InteractionTrackSettings)],
    copy_number_settings: &[(TrackId, CopyNumberTrackSettings)],
) -> Result<TrackSnapshot, (TrackId, String)> {
    match track {
        OpenTrack::Text { id, name, source } => source
            .query(contig, start, end)
            .map(|records| TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::Features(records),
            })
            .map_err(|error| (*id, error.to_string())),
        OpenTrack::Variants { id, name, source } => {
            let default_settings = VariantTrackSettings::default();
            let settings = variant_settings
                .iter()
                .find(|(track_id, _)| track_id == id)
                .map_or(&default_settings, |(_, settings)| settings);
            source
                .query_with_settings(contig, start, end, 100_000, settings)
                .map(|records| TrackSnapshot {
                    id: *id,
                    name: name.clone(),
                    data: TrackSnapshotData::Variants(Arc::new(records)),
                })
                .map_err(|error| (*id, error.to_string()))
        }
        OpenTrack::BigWig { id, name, source } => source
            .query(contig, start, end)
            .map(|query| TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::Quantitative(Arc::new(query)),
            })
            .map_err(|error| (*id, error.to_string())),
        OpenTrack::TextQuantitative { id, name, source } => Ok(TrackSnapshot {
            id: *id,
            name: name.clone(),
            data: TrackSnapshotData::Quantitative(Arc::new(source.query(contig, start, end))),
        }),
        OpenTrack::Tdf {
            id,
            name,
            source,
            track_index,
        } => source
            .borrow_mut()
            .query_channel(*track_index, contig, start, end)
            .map(|query| TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::Quantitative(Arc::new(query)),
            })
            .map_err(|error| (*id, error.to_string())),
        OpenTrack::Interactions { id, name, source } => {
            let default_settings = InteractionTrackSettings::default();
            let settings = interaction_settings
                .iter()
                .find(|(track_id, _)| track_id == id)
                .map_or(&default_settings, |(_, settings)| settings);
            Ok(TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::Interactions(Arc::new(
                    source.query(contig, start, end, settings),
                )),
            })
        }
        OpenTrack::CopyNumber { id, name, source } => {
            let default_settings = CopyNumberTrackSettings::default();
            let settings = copy_number_settings
                .iter()
                .find(|(track_id, _)| track_id == id)
                .map_or(&default_settings, |(_, settings)| settings);
            Ok(TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::CopyNumber(Arc::new(
                    source.query(contig, start, end, settings),
                )),
            })
        }
        OpenTrack::Mutations { id, name, source } => Ok(TrackSnapshot {
            id: *id,
            name: name.clone(),
            data: TrackSnapshotData::Mutations(Arc::new(source.query(contig, start, end))),
        }),
        OpenTrack::Gwas { id, name, source } => Ok(TrackSnapshot {
            id: *id,
            name: name.clone(),
            data: TrackSnapshotData::Gwas(Arc::new(source.query(contig, start, end))),
        }),
        OpenTrack::BigBed { id, name, source } => source
            .query(contig, start, end)
            .map(|records| TrackSnapshot {
                id: *id,
                name: name.clone(),
                data: TrackSnapshotData::Features(records),
            })
            .map_err(|error| (*id, error.to_string())),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_send_error(error: TrySendError<TrackEngineRequest>) -> EngineQueueError {
    match error {
        TrySendError::Full(_) => EngineQueueError::Full,
        TrySendError::Disconnected(_) => EngineQueueError::Disconnected,
    }
}

fn send_event(output: &CoalescingSender<TrackEngineEvent>, event: TrackEngineEvent) {
    let result = match &event {
        TrackEngineEvent::QueryCompleted { request, .. } => {
            output.send_latest_for(request.panel_id().get(), event)
        }
        TrackEngineEvent::LaneCompleted { request, track, .. } => {
            output.send_latest_system_for(request.panel_id().get(), track.id.get(), event)
        }
        TrackEngineEvent::SearchCompleted { .. } => output.send_latest_for(u64::MAX, event),
        TrackEngineEvent::AdjacentFeatureCompleted { .. } => {
            output.send_latest_for(u64::MAX - 1, event)
        }
        _ => output.send_control(event),
    };
    if result.is_err() {
        tracing::debug!("track event receiver closed");
    }
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval, PanelId, ViewportGeneration};

    use super::*;

    fn tdf_fixture() -> Vec<u8> {
        fn string(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }

        let mut header = Vec::new();
        header.extend_from_slice(&1_u32.to_le_bytes());
        string(&mut header, "mean");
        string(&mut header, "OTHER");
        string(&mut header, "");
        header.extend_from_slice(&2_u32.to_le_bytes());
        string(&mut header, "engine signal");
        string(&mut header, "second signal");
        string(&mut header, "hg38");
        header.extend_from_slice(&0_u32.to_le_bytes());

        let mut tile = Vec::new();
        string(&mut tile, "fixedStep");
        tile.extend_from_slice(&2_u32.to_le_bytes());
        tile.extend_from_slice(&100_i32.to_le_bytes());
        tile.extend_from_slice(&10_f32.to_le_bytes());
        tile.extend_from_slice(&2.0_f32.to_le_bytes());
        tile.extend_from_slice(&4.0_f32.to_le_bytes());
        tile.extend_from_slice(&20.0_f32.to_le_bytes());
        tile.extend_from_slice(&40.0_f32.to_le_bytes());
        let tile_position = (24 + header.len()) as u64;

        let mut dataset = Vec::new();
        dataset.extend_from_slice(&0_u32.to_le_bytes());
        string(&mut dataset, "FLOAT");
        dataset.extend_from_slice(&1_000_f32.to_le_bytes());
        dataset.extend_from_slice(&1_u32.to_le_bytes());
        dataset.extend_from_slice(&i64::try_from(tile_position).unwrap().to_le_bytes());
        dataset.extend_from_slice(&(tile.len() as u32).to_le_bytes());
        let dataset_position = tile_position + tile.len() as u64;

        let mut index = Vec::new();
        index.extend_from_slice(&1_u32.to_le_bytes());
        string(&mut index, "/chr1/z0/mean");
        index.extend_from_slice(&dataset_position.to_le_bytes());
        index.extend_from_slice(&(dataset.len() as u32).to_le_bytes());
        index.extend_from_slice(&0_u32.to_le_bytes());
        let index_position = dataset_position + dataset.len() as u64;

        let mut bytes = b"TDF4".to_vec();
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&index_position.to_le_bytes());
        bytes.extend_from_slice(&(index.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend(header);
        bytes.extend(tile);
        bytes.extend(dataset);
        bytes.extend(index);
        bytes
    }

    fn receive_query(engine: &TrackEngine, timeout_message: &str) -> Vec<TrackSnapshot> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut tracks = Vec::new();
        while std::time::Instant::now() < deadline {
            match engine.try_receive().unwrap() {
                Some(TrackEngineEvent::LaneCompleted { track, .. }) => tracks.push(track),
                Some(TrackEngineEvent::QueryCompleted { completed, .. }) => {
                    tracks.retain(|track| completed.contains(&track.id));
                    tracks.sort_by_key(|track| track.id);
                    return tracks;
                }
                Some(_) | None => std::thread::yield_now(),
            }
        }
        panic!("{timeout_message}");
    }

    fn query_selected_tdf_channels(
        engine: &TrackEngine,
        panel_id: u64,
        track_ids: Vec<TrackId>,
    ) -> Vec<TrackSnapshot> {
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(panel_id), clock.current());
        engine
            .try_query_selected_with_settings(
                request,
                clock,
                "chr1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                track_ids,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        receive_query(engine, "selected TDF actor query timed out")
    }

    #[test]
    fn query_burst_retains_newest_snapshot_behind_ready_event() {
        let (output, receiver) = bounded_coalescing(1);
        send_event(
            &output,
            TrackEngineEvent::Ready {
                opened: 0,
                failed: 0,
            },
        );
        for generation in 1..=32 {
            send_event(
                &output,
                TrackEngineEvent::QueryCompleted {
                    request: PanelGeneration::new(
                        PanelId::new(1),
                        ViewportGeneration::new(generation),
                    ),
                    interval: GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                    completed: Vec::new(),
                },
            );
        }

        assert!(matches!(
            receiver.try_receive().unwrap(),
            Some(TrackEngineEvent::Ready { .. })
        ));
        assert_eq!(
            receiver.try_receive().unwrap(),
            Some(TrackEngineEvent::QueryCompleted {
                request: PanelGeneration::new(PanelId::new(1), ViewportGeneration::new(32),),
                interval: GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                completed: Vec::new(),
            })
        );
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn query_coalescing_retains_the_latest_snapshot_for_each_panel() {
        let (output, receiver) = bounded_coalescing(1);
        let request = |panel, generation| {
            PanelGeneration::new(PanelId::new(panel), ViewportGeneration::new(generation))
        };
        for request in [request(1, 1), request(2, 1), request(1, 2)] {
            send_event(
                &output,
                TrackEngineEvent::QueryCompleted {
                    request,
                    interval: GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                    completed: Vec::new(),
                },
            );
        }

        assert_eq!(
            receiver.try_receive().unwrap(),
            Some(TrackEngineEvent::QueryCompleted {
                request: request(2, 1),
                interval: GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                completed: Vec::new(),
            })
        );
        assert_eq!(
            receiver.try_receive().unwrap(),
            Some(TrackEngineEvent::QueryCompleted {
                request: request(1, 2),
                interval: GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
                completed: Vec::new(),
            })
        );
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn feature_search_merges_tracks_with_exact_matches_before_prefixes() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("genes-a.gff3");
        let second = directory.path().join("genes-b.gff3");
        std::fs::write(
            &first,
            "chr2\tsrc\tgene\t101\t200\t.\t+\t.\tID=g2;Name=BRCA2\n",
        )
        .unwrap();
        std::fs::write(
            &second,
            "chr1\tsrc\tgene\t11\t50\t.\t+\t.\tID=g1;Name=BRCA\n",
        )
        .unwrap();
        let mut tracks = vec![
            open_track(&TrackSpec {
                id: TrackId::new(2),
                name: "Genes B".to_owned(),
                path: first,
                kind: TrackSpecKind::Gff3,
            })
            .unwrap(),
            open_track(&TrackSpec {
                id: TrackId::new(1),
                name: "Genes A".to_owned(),
                path: second,
                kind: TrackSpecKind::Gff3,
            })
            .unwrap(),
        ];

        let results = search_tracks(&mut tracks, "BRCA".to_owned(), 10, false);

        assert_eq!(results.hits.len(), 2);
        assert_eq!(results.hits[0].record.name.as_deref(), Some("BRCA"));
        assert_eq!(results.hits[1].record.name.as_deref(), Some("BRCA2"));
        assert_eq!(
            adjacent_feature(&mut tracks, &HashMap::new(), "chr2", 0, true)
                .and_then(|hit| hit.record.name),
            Some("BRCA2".to_owned())
        );
    }

    #[test]
    fn mutation_maf_opens_and_queries_through_the_track_engine_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cohort.maf");
        std::fs::write(
            &path,
            "Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_Barcode\n\
             TP53\tchr17\t101\t101\tMissense_Mutation\tSNP\tC\tC\tT\tTUMOUR-A\n",
        )
        .unwrap();
        let mut track = open_track(&TrackSpec {
            id: TrackId::new(9),
            name: "somatic".to_owned(),
            path,
            kind: TrackSpecKind::MutationMaf,
        })
        .unwrap();

        let snapshot = query_track(&mut track, "chr17", 100, 101, &[], &[], &[]).unwrap();

        assert_eq!(snapshot.id, TrackId::new(9));
        let TrackSnapshotData::Mutations(query) = snapshot.data else {
            panic!("mutation source returned the wrong snapshot type");
        };
        assert_eq!(query.samples, ["TUMOUR-A"]);
        assert_eq!(query.records[0].gene, "TP53");
    }

    #[test]
    fn gene_pred_and_narrow_peak_query_through_the_track_engine_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let gene_path = directory.path().join("genes.genePred");
        std::fs::write(
            &gene_path,
            "tx1\tchr1\t+\t100\t400\t150\t350\t2\t100,300,\t200,400,\n",
        )
        .unwrap();
        let peak_path = directory.path().join("signal.narrowPeak");
        std::fs::write(&peak_path, "chr1\t120\t220\tpeak-1\t800\t.\t25\t6\t5\t40\n").unwrap();
        let mut gene_track = open_track(&TrackSpec {
            id: TrackId::new(10),
            name: "genes".to_owned(),
            path: gene_path,
            kind: TrackSpecKind::GenePred,
        })
        .unwrap();
        let mut peak_track = open_track(&TrackSpec {
            id: TrackId::new(11),
            name: "peaks".to_owned(),
            path: peak_path,
            kind: TrackSpecKind::NarrowPeak,
        })
        .unwrap();

        let gene = query_track(&mut gene_track, "chr1", 180, 181, &[], &[], &[]).unwrap();
        let peak = query_track(&mut peak_track, "chr1", 180, 181, &[], &[], &[]).unwrap();

        let TrackSnapshotData::Features(genes) = gene.data else {
            panic!("genePred source returned the wrong snapshot type");
        };
        let TrackSnapshotData::Features(peaks) = peak.data else {
            panic!("narrowPeak source returned the wrong snapshot type");
        };
        assert_eq!(genes[0].blocks, [(100, 200), (300, 400)]);
        assert_eq!(genes[0].coding_interval, Some((150, 350)));
        assert_eq!(peaks[0].peak.and_then(|peak| peak.summit), Some(160));
    }

    #[test]
    fn dynamically_added_landmark_track_queries_its_source_contig_alias() {
        let directory = tempfile::tempdir().unwrap();
        let gene_path = directory.path().join("genes.genePred");
        std::fs::write(
            &gene_path,
            "tx1\tchr1\t+\t100\t400\t150\t350\t2\t100,300,\t200,400,\n",
        )
        .unwrap();
        let track_id = TrackId::new(10);
        let engine = TrackEngine::spawn(Vec::new(), 8);
        engine
            .try_add_with_query_contig_aliases(
                TrackSpec {
                    id: track_id,
                    name: "Genes".to_owned(),
                    path: gene_path,
                    kind: TrackSpecKind::GenePred,
                },
                BTreeMap::from([("1".to_owned(), "chr1".to_owned())]),
            )
            .unwrap();
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        engine
            .try_query(
                request,
                clock,
                "1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 180, 181).unwrap(),
            )
            .unwrap();

        let snapshots = receive_query(&engine, "mapped landmark actor query timed out");
        assert_eq!(snapshots.len(), 1);
        let TrackSnapshotData::Features(genes) = &snapshots[0].data else {
            panic!("landmark genePred source returned the wrong snapshot type");
        };
        assert_eq!(genes.len(), 1);
        assert_eq!(genes[0].contig, "chr1");
    }

    #[test]
    fn restored_landmark_track_can_rebind_its_query_contig_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let gene_path = directory.path().join("genes.genePred");
        std::fs::write(
            &gene_path,
            "tx1\tchr1\t+\t100\t400\t150\t350\t2\t100,300,\t200,400,\n",
        )
        .unwrap();
        let track_id = TrackId::new(10);
        let engine = TrackEngine::spawn(
            vec![TrackSpec {
                id: track_id,
                name: "Genes".to_owned(),
                path: gene_path,
                kind: TrackSpecKind::GenePred,
            }],
            8,
        );
        engine
            .try_set_query_contig_aliases(
                track_id,
                BTreeMap::from([("1".to_owned(), "chr1".to_owned())]),
            )
            .unwrap();
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        engine
            .try_query(
                request,
                clock,
                "1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 180, 181).unwrap(),
            )
            .unwrap();

        let snapshots = receive_query(&engine, "rebound landmark actor query timed out");
        assert_eq!(snapshots.len(), 1);
        let TrackSnapshotData::Features(genes) = &snapshots[0].data else {
            panic!("landmark genePred source returned the wrong snapshot type");
        };
        assert_eq!(genes.len(), 1);
    }

    #[test]
    fn broad_peak_and_psl_query_search_and_navigate_through_the_engine_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let broad_path = directory.path().join("signal.broadPeak");
        std::fs::write(
            &broad_path,
            "chr1\t100\t250\tregion-a\t116\t.\t4.89716\t3.70716\t-1\n",
        )
        .unwrap();
        let psl_path = directory.path().join("hits.psl");
        std::fs::write(
            &psl_path,
            "70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,\n",
        )
        .unwrap();
        let broad = open_track(&TrackSpec {
            id: TrackId::new(12),
            name: "broad peaks".to_owned(),
            path: broad_path,
            kind: TrackSpecKind::BroadPeak,
        })
        .unwrap();
        let psl = open_track(&TrackSpec {
            id: TrackId::new(13),
            name: "PSL".to_owned(),
            path: psl_path,
            kind: TrackSpecKind::Psl,
        })
        .unwrap();
        let mut tracks = vec![broad, psl];

        let broad = query_track(&mut tracks[0], "chr1", 120, 121, &[], &[], &[]).unwrap();
        let psl = query_track(&mut tracks[1], "chr1", 120, 121, &[], &[], &[]).unwrap();
        let TrackSnapshotData::Features(broad) = broad.data else {
            panic!("broadPeak source returned the wrong snapshot type");
        };
        let TrackSnapshotData::Features(psl) = psl.data else {
            panic!("PSL source returned the wrong snapshot type");
        };
        assert_eq!(broad[0].peak.unwrap().summit, None);
        assert_eq!(psl[0].blocks, [(100, 130), (140, 190)]);

        let search = search_tracks(&mut tracks, "query-1".to_owned(), 10, false);
        assert_eq!(search.hits[0].track_id, TrackId::new(13));
        assert_eq!(
            adjacent_feature(&mut tracks, &HashMap::new(), "chr1", 0, true)
                .unwrap()
                .record
                .start,
            100
        );
    }

    #[test]
    fn gwas_queries_searches_and_navigates_through_the_engine_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("study.assoc");
        std::fs::write(
            &path,
            "CHR BP SNP P EFFECT\nchr6 101 rs-first 1e-9 0.4\nchr6 201 rs-second 0.05 -0.2\n",
        )
        .unwrap();
        let track = open_track(&TrackSpec {
            id: TrackId::new(14),
            name: "GWAS".to_owned(),
            path,
            kind: TrackSpecKind::Gwas,
        })
        .unwrap();
        let mut tracks = vec![track];
        let snapshot = query_track(&mut tracks[0], "chr6", 90, 120, &[], &[], &[]).unwrap();
        let TrackSnapshotData::Gwas(query) = snapshot.data else {
            panic!("GWAS source returned the wrong snapshot type");
        };
        assert_eq!(query.records[0].identifier, "rs-first");
        assert!((query.records[0].minus_log10_p - 9.0).abs() < 1e-5);

        let search = search_tracks(&mut tracks, "RS-FIR".to_owned(), 10, false);
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].record.name.as_deref(), Some("rs-first"));
        assert_eq!(search.hits[0].record.start, 100);
        let adjacent = adjacent_feature(&mut tracks, &HashMap::new(), "chr6", 101, true).unwrap();
        assert_eq!(adjacent.record.name.as_deref(), Some("rs-second"));
    }

    #[test]
    fn tdf_queries_through_the_engine_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.tdf");
        std::fs::write(&path, tdf_fixture()).unwrap();
        let mut track = open_track(&TrackSpec {
            id: TrackId::new(15),
            name: "engine signal".to_owned(),
            path,
            kind: TrackSpecKind::Tdf,
        })
        .unwrap();

        let snapshot = query_track(&mut track, "chr1", 100, 120, &[], &[], &[]).unwrap();
        let TrackSnapshotData::Quantitative(query) = snapshot.data else {
            panic!("TDF source returned the wrong snapshot type");
        };
        assert_eq!(query.records.len(), 2);
        assert!((query.records[1].value - 4.0).abs() < f32::EPSILON);
        let Some(rgv_hts::QuantitativeSourceMetadata::Tdf(metadata)) = &query.source_metadata
        else {
            panic!("TDF metadata was not routed through the engine");
        };
        assert_eq!(metadata.selected_track_name, "engine signal");
    }

    #[test]
    fn tdf_channels_share_one_source_and_discovery_preserves_every_channel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.tdf");
        std::fs::write(&path, tdf_fixture()).unwrap();
        let parent_specification = TrackSpec {
            id: TrackId::new(20),
            name: "archive.tdf".to_owned(),
            path: path.clone(),
            kind: TrackSpecKind::Tdf,
        };
        let channel_specification = TrackSpec {
            id: TrackId::new(21),
            name: "second signal".to_owned(),
            path,
            kind: TrackSpecKind::TdfChannel { index: 1 },
        };
        let mut cache = TdfSourceCache::new();
        let mut parent = open_track_cached(&parent_specification, &mut cache).unwrap();
        let mut channel = open_track_cached(&channel_specification, &mut cache).unwrap();
        let (
            OpenTrack::Tdf {
                source: parent_source,
                ..
            },
            OpenTrack::Tdf {
                source: channel_source,
                ..
            },
        ) = (&parent, &channel)
        else {
            panic!("expected two TDF tracks");
        };
        assert!(Rc::ptr_eq(parent_source, channel_source));

        let second = query_track(&mut channel, "chr1", 100, 120, &[], &[], &[]).unwrap();
        let TrackSnapshotData::Quantitative(second) = second.data else {
            panic!("TDF channel returned the wrong snapshot type");
        };
        assert!((second.records[1].value - 40.0).abs() < f32::EPSILON);

        let (output, receiver) = bounded_coalescing(4);
        send_tdf_discovery(&output, &parent_specification, &parent);
        assert_eq!(
            receiver.try_receive().unwrap(),
            Some(TrackEngineEvent::TdfChannelsDiscovered {
                id: TrackId::new(20),
                track_names: vec!["engine signal".to_owned(), "second signal".to_owned()],
                selected_index: 0,
            })
        );
        let first = query_track(&mut parent, "chr1", 100, 120, &[], &[], &[]).unwrap();
        let TrackSnapshotData::Quantitative(first) = first.data else {
            panic!("TDF parent returned the wrong snapshot type");
        };
        assert!((first.records[1].value - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn removal_releases_a_shared_tdf_source_only_after_its_last_channel() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.tdf");
        std::fs::write(&path, tdf_fixture()).unwrap();
        let mut cache = TdfSourceCache::new();
        let mut query_contig_aliases = HashMap::new();
        let mut tracks = vec![
            open_track_cached(
                &TrackSpec {
                    id: TrackId::new(20),
                    name: "engine signal".to_owned(),
                    path: path.clone(),
                    kind: TrackSpecKind::TdfChannel { index: 0 },
                },
                &mut cache,
            )
            .unwrap(),
            open_track_cached(
                &TrackSpec {
                    id: TrackId::new(21),
                    name: "second signal".to_owned(),
                    path,
                    kind: TrackSpecKind::TdfChannel { index: 1 },
                },
                &mut cache,
            )
            .unwrap(),
        ];
        assert_eq!(cache.len(), 1);

        assert_eq!(
            remove_tracks(
                &mut tracks,
                &mut cache,
                &mut query_contig_aliases,
                vec![TrackId::new(20), TrackId::new(20), TrackId::new(999)],
            ),
            [TrackId::new(20)]
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id(), TrackId::new(21));
        assert_eq!(cache.len(), 1);

        assert_eq!(
            remove_tracks(
                &mut tracks,
                &mut cache,
                &mut query_contig_aliases,
                vec![TrackId::new(21)],
            ),
            [TrackId::new(21)]
        );
        assert!(tracks.is_empty());
        assert!(cache.is_empty());
    }

    #[test]
    fn tdf_channel_expansion_runs_through_the_actor_queue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.tdf");
        std::fs::write(&path, tdf_fixture()).unwrap();
        let engine = TrackEngine::spawn(
            vec![TrackSpec {
                id: TrackId::new(30),
                name: "archive.tdf".to_owned(),
                path: path.clone(),
                kind: TrackSpecKind::Tdf,
            }],
            8,
        );
        let mut deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut discovered = false;
        let mut ready = false;
        while std::time::Instant::now() < deadline && !(discovered && ready) {
            match engine.try_receive().unwrap() {
                Some(TrackEngineEvent::TdfChannelsDiscovered { track_names, .. }) => {
                    discovered = track_names == ["engine signal", "second signal"];
                }
                Some(TrackEngineEvent::Ready {
                    opened: 1,
                    failed: 0,
                }) => ready = true,
                Some(_) | None => std::thread::yield_now(),
            }
        }
        assert!(discovered && ready);

        engine
            .try_add_many(vec![TrackSpec {
                id: TrackId::new(31),
                name: "second signal".to_owned(),
                path,
                kind: TrackSpecKind::TdfChannel { index: 1 },
            }])
            .unwrap();
        deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut added = false;
        while std::time::Instant::now() < deadline && !added {
            match engine.try_receive().unwrap() {
                Some(TrackEngineEvent::TracksAdded { ids }) => {
                    added = ids == [TrackId::new(31)];
                }
                Some(_) | None => std::thread::yield_now(),
            }
        }
        assert!(added);

        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        engine
            .try_query(
                request,
                clock,
                "chr1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
            )
            .unwrap();
        let snapshots = receive_query(&engine, "TDF actor query timed out");
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].id, TrackId::new(30));
        assert_eq!(snapshots[1].id, TrackId::new(31));

        let snapshots = query_selected_tdf_channels(
            &engine,
            2,
            vec![TrackId::new(31), TrackId::new(31), TrackId::new(999)],
        );
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, TrackId::new(31));

        assert!(query_selected_tdf_channels(&engine, 3, Vec::new()).is_empty());

        engine
            .try_remove_many(vec![TrackId::new(30), TrackId::new(30)])
            .unwrap();
        // The desktop queues the replacement query immediately. FIFO actor
        // ordering must make it observe the surviving set even before the UI
        // consumes the removal acknowledgement.
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        engine
            .try_query(
                request,
                clock,
                "chr1".to_owned(),
                GenomicInterval::new(ContigId::new(0), 100, 120).unwrap(),
            )
            .unwrap();
        deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut removed = false;
        let mut lane_snapshots = Vec::new();
        let mut snapshots = None;
        while std::time::Instant::now() < deadline && (!removed || snapshots.is_none()) {
            match engine.try_receive().unwrap() {
                Some(TrackEngineEvent::TracksRemoved { ids }) => {
                    removed = ids == [TrackId::new(30)];
                }
                Some(TrackEngineEvent::LaneCompleted { track, .. }) => {
                    lane_snapshots.push(track);
                }
                Some(TrackEngineEvent::QueryCompleted { completed, .. }) => {
                    lane_snapshots.retain(|track| completed.contains(&track.id));
                    snapshots = Some(std::mem::take(&mut lane_snapshots));
                }
                Some(_) | None => std::thread::yield_now(),
            }
        }
        assert!(removed);
        let snapshots = snapshots.expect("post-removal TDF actor query timed out");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, TrackId::new(31));
    }
}
