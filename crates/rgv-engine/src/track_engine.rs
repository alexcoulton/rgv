use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use rgv_core::{
    CopyNumberTrackSettings, GenerationClock, GenomicInterval, InteractionTrackSettings,
    PanelGeneration, TrackId, VariantTrackSettings,
};
use rgv_hts::{
    BigBedFeatureFormat, BigBedSource, BigWigSource, CopyNumberQuery, FeatureFormat, FeatureRecord,
    GwasQuery, HttpRangeConfig, InteractionFormat, InteractionQuery, LocalVariantSource,
    MAX_FEATURE_SEARCH_RESULTS, MutationQuery, QuantitativeFormat, QuantitativeQuery,
    RemoteCancellation, RemoteTransferBudget, TdfSource, TextCopyNumberSource, TextFeatureSource,
    TextGwasSource, TextInteractionSource, TextMutationSource, TextQuantitativeSource,
    VariantQuery,
};

use crate::{
    EngineQueueError,
    mailbox::{
        CoalescingReceiver, CoalescingSender, bounded_coalescing_to_ui, panic_message, reap_worker,
        run_thread_root,
    },
    track_source::{
        ClosedTrackSource, SearchRequest, SharedTrackSource, TdfChannel, TrackQuerySettings,
        TrackSource,
    },
    track_tile_engine::TrackTileSourceRegistrar,
};

/// Most tracks queried at the same time for one viewport request.
const MAXIMUM_PARALLEL_TRACK_QUERIES: usize = 8;

/// Most one track operation (a viewport query, a name search, a feature step) may transfer
/// from a remote source before it fails with a clear error instead of downloading without
/// limit. Each operation starts with a fresh allowance. Local sources ignore it.
pub(crate) const REMOTE_TRACK_OPERATION_TRANSFER_BUDGET: RemoteTransferBudget =
    RemoteTransferBudget {
        maximum_requests: 4_000,
        maximum_response_bytes: 512 << 20,
    };

/// Input lines a track source skipped because they could not be parsed, and a name-search
/// index that does not cover the whole source.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackSourceDiagnostics {
    /// Lines skipped so far. A fully loaded source counts its whole file at open; an indexed
    /// source counts the lines met by the queries run so far.
    pub malformed_lines: usize,
    /// One-based line number and parse error of the first skipped line, when known.
    pub first_malformed_line: Option<(usize, String)>,
    /// The source's name-search index, when it stopped before the end of the source. An
    /// indexed source builds its index on the first search, so this appears after it.
    pub truncated_search_index: Option<rgv_model::tracks::FeatureSearchIndexCoverage>,
}

/// Latest diagnostics of every open track that reports any, shared with the engine handle.
type SharedTrackDiagnostics = Arc<Mutex<BTreeMap<TrackId, TrackSourceDiagnostics>>>;

fn record_track_diagnostics(diagnostics: &SharedTrackDiagnostics, track: &OpenTrack) {
    let Some(report) = track.source.diagnostics() else {
        return;
    };
    let mut diagnostics = diagnostics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if report.malformed_lines == 0 && report.truncated_search_index.is_none() {
        diagnostics.remove(&track.id);
    } else {
        diagnostics.insert(track.id, report);
    }
}

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
    ShareTileSources {
        ids: Vec<TrackId>,
        registrar: TrackTileSourceRegistrar,
    },
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

/// One opened auxiliary track. The file format is known only to its [`TrackSource`].
struct OpenTrack {
    id: TrackId,
    name: String,
    source: Box<dyn TrackSource>,
}

impl OpenTrack {
    fn new(id: TrackId, name: String, source: impl TrackSource + 'static) -> Self {
        Self {
            id,
            name,
            source: Box::new(source),
        }
    }

    const fn id(&self) -> TrackId {
        self.id
    }
}

pub struct TrackEngine {
    input: Option<Sender<TrackEngineRequest>>,
    output: CoalescingReceiver<TrackEngineEvent>,
    worker: Option<JoinHandle<()>>,
    /// Newest search generation the queue accepted. The actor abandons any search older than
    /// this, before it starts and between tracks, instead of finishing an answer nobody wants.
    latest_search: Arc<AtomicU64>,
    diagnostics: SharedTrackDiagnostics,
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
        Self::spawn_with_remote_config(specifications, capacity, HttpRangeConfig::default())
    }

    /// Like [`Self::spawn_with_query_contig_aliases`], reading every remote bigWig, bigBed, TDF
    /// and text annotation with `remote` (authentication, timeouts, retries and cache geometry)
    /// instead of the anonymous defaults. Tracks added later use the same configuration.
    pub fn spawn_with_remote_config(
        specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
        capacity: usize,
        remote: HttpRangeConfig,
    ) -> Self {
        let diagnostics = SharedTrackDiagnostics::default();
        let worker_diagnostics = Arc::clone(&diagnostics);
        let (input_tx, input_rx) = bounded(capacity.max(1));
        let (output_tx, output_rx) = bounded_coalescing_to_ui(capacity.max(1));
        let latest_search = Arc::new(AtomicU64::new(0));
        let worker_latest_search = Arc::clone(&latest_search);
        let worker = thread::Builder::new()
            .name("rgv-track-actor".to_owned())
            .spawn(move || {
                // Query helpers publish lane results through the same sender.
                let output = Arc::new(output_tx);
                run_thread_root(
                    || {
                        worker_loop(
                            specifications,
                            &input_rx,
                            &output,
                            &worker_latest_search,
                            &remote,
                            &worker_diagnostics,
                        );
                    },
                    |message| {
                        tracing::error!(message, "track actor panicked");
                        send_event(
                            &output,
                            TrackEngineEvent::Error {
                                request: None,
                                track: None,
                                message: format!(
                                    "the track engine stopped after an internal error: {message}"
                                ),
                            },
                        );
                    },
                );
            })
            .expect("failed to spawn RGV track actor");
        Self {
            input: Some(input_tx),
            output: output_rx,
            worker: Some(worker),
            latest_search,
            diagnostics,
        }
    }

    /// Open tracks that have skipped unparseable input, with how much and where it began.
    /// Ordered by track; a track with nothing skipped is absent. Cheap enough to call whenever
    /// a track event is handled.
    #[must_use]
    pub fn source_diagnostics(&self) -> Vec<(TrackId, TrackSourceDiagnostics)> {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, report)| (*id, report.clone()))
            .collect()
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

    /// Serves `ids` in `tile_engine` from the readers this engine already has open, instead
    /// of the tile engine opening each file a second time ([`crate::TrackTileEngine::add_track`]
    /// does that). Use it for a track that is in this engine and is to be drawn through the
    /// tile engine: both engines then read through one reader, one operation at a time.
    ///
    /// The tracks are registered in `tile_engine` at once, so views may be requested
    /// immediately; `TrackOpened` follows on the tile engine's event queue when the source has
    /// been handed over, or `TrackOpenFailed` for an id this engine has not (or not
    /// successfully) opened. Requests are handled in order, so sharing a track right after
    /// [`Self::try_add`] waits for that open. The track's query contig aliases are carried
    /// over. Nothing changes for this engine's own queries, searches and feature steps,
    /// except that they wait while the tile engine reads the same file, and vice versa.
    /// Removing the track from either engine leaves the other's handle working; the file is
    /// closed when both have let go.
    pub fn try_share_tile_sources(
        &self,
        ids: Vec<TrackId>,
        tile_engine: &crate::TrackTileEngine,
    ) -> Result<(), EngineQueueError> {
        let registrar = tile_engine.registrar();
        registrar.expect(&ids);
        let awaited = ids.clone();
        let result = self
            .input
            .as_ref()
            .ok_or(EngineQueueError::Disconnected)
            .and_then(|input| {
                input
                    .try_send(TrackEngineRequest::ShareTileSources {
                        ids,
                        registrar: registrar.clone(),
                    })
                    .map_err(map_send_error)
            });
        if result.is_err() {
            registrar.withdraw(&awaited);
        }
        result
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
            .map_err(map_send_error)?;
        // Published only once the request is queued, and only ever raised: a search the queue
        // refused cancels nothing, and a late store cannot cancel the search it belongs to.
        self.latest_search.fetch_max(generation, Ordering::AcqRel);
        Ok(())
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
    /// Signals the actor exactly as `retire` does and returns without joining it: an engine
    /// dropped on a path which forgot to retire must not block that thread on in-flight I/O.
    /// A reaper thread waits for the actor instead.
    fn drop(&mut self) {
        self.input.take();
        self.output.close();
        if let Some(worker) = self.worker.take() {
            drop(reap_worker(worker, "rgv-track-reaper", "track actor"));
        }
    }
}

fn worker_loop(
    specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
    input: &Receiver<TrackEngineRequest>,
    output: &Arc<CoalescingSender<TrackEngineEvent>>,
    latest_search: &AtomicU64,
    remote: &HttpRangeConfig,
    diagnostics: &SharedTrackDiagnostics,
) {
    let mut query_pool = None;
    let mut tracks = Vec::new();
    let mut opener = TrackOpener {
        tdf_sources: HashMap::new(),
        remote,
        diagnostics,
    };
    let mut query_contig_aliases = HashMap::new();
    let mut failed = 0;
    for (specification, aliases) in specifications {
        if output.is_closed() {
            return;
        }
        match open_and_push_track(&specification, &mut tracks, &mut opener, output) {
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
            } => match open_and_push_track(&specification, &mut tracks, &mut opener, output) {
                Ok(id) => {
                    if !aliases.is_empty() {
                        query_contig_aliases.insert(id, aliases);
                    }
                    send_event(output, TrackEngineEvent::TrackAdded { id });
                }
                Err(message) => send_track_open_error(output, &specification, &message),
            },
            TrackEngineRequest::AddMany(specifications) => {
                let added = open_and_push_tracks(specifications, &mut tracks, &mut opener, output);
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
                    &mut opener.tdf_sources,
                    &mut query_contig_aliases,
                    ids,
                );
                if !removed.is_empty() {
                    let mut diagnostics = diagnostics
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for id in &removed {
                        diagnostics.remove(id);
                    }
                    drop(diagnostics);
                    send_event(output, TrackEngineEvent::TracksRemoved { ids: removed });
                }
            }
            TrackEngineRequest::ShareTileSources { ids, registrar } => {
                share_tile_sources(&mut tracks, &query_contig_aliases, ids, &registrar);
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
                &mut query_pool,
                &Arc::new(TrackQueryContext {
                    output: Arc::clone(output),
                    diagnostics: Arc::clone(diagnostics),
                    clock,
                    request,
                    contig,
                    interval,
                    variant_settings,
                    interaction_settings,
                    copy_number_settings,
                }),
                track_ids.as_deref(),
            ),
            TrackEngineRequest::Search {
                generation,
                query,
                limit,
                include_variant_ids,
            } => {
                let superseded = || latest_search.load(Ordering::Acquire) > generation;
                let results =
                    search_tracks(&mut tracks, query, limit, include_variant_ids, &superseded);
                // The first search builds an indexed source's name index, which may be partial.
                for track in &tracks {
                    record_track_diagnostics(diagnostics, track);
                }
                if let Some(results) = results {
                    send_event(
                        output,
                        TrackEngineEvent::SearchCompleted {
                            generation,
                            results,
                        },
                    );
                }
            }
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

/// What opening a track needs beyond its specification.
struct TrackOpener<'a> {
    tdf_sources: TdfSourceCache,
    remote: &'a HttpRangeConfig,
    diagnostics: &'a SharedTrackDiagnostics,
}

fn open_and_push_track(
    specification: &TrackSpec,
    tracks: &mut Vec<OpenTrack>,
    opener: &mut TrackOpener<'_>,
    output: &CoalescingSender<TrackEngineEvent>,
) -> Result<TrackId, String> {
    let track = open_track_with(specification, &mut opener.tdf_sources, opener.remote)?;
    record_track_diagnostics(opener.diagnostics, &track);
    send_tdf_discovery(output, specification, &track);
    let id = specification.id;
    tracks.push(track);
    Ok(id)
}

fn open_and_push_tracks(
    specifications: Vec<TrackSpec>,
    tracks: &mut Vec<OpenTrack>,
    opener: &mut TrackOpener<'_>,
    output: &CoalescingSender<TrackEngineEvent>,
) -> Vec<TrackId> {
    let mut added = Vec::new();
    for specification in specifications {
        if output.is_closed() {
            break;
        }
        match open_and_push_track(&specification, tracks, opener, output) {
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
    tdf_sources.retain(|_, source| Arc::strong_count(source) > 1);
    removed
}

/// Hands the tile engine a handle on the reader of each of `ids`. A track shared for the
/// first time has its reader moved behind a [`SharedTrackSource`]; this engine keeps reading
/// through its own handle.
fn share_tile_sources(
    tracks: &mut [OpenTrack],
    query_contig_aliases: &HashMap<TrackId, BTreeMap<String, String>>,
    ids: Vec<TrackId>,
    registrar: &TrackTileSourceRegistrar,
) {
    for id in ids {
        let Some(track) = tracks.iter_mut().find(|track| track.id == id) else {
            registrar.fail(id, "the track is not open in the track engine");
            continue;
        };
        let handle = track.source.share_handle().unwrap_or_else(|| {
            let source = std::mem::replace(&mut track.source, Box::new(ClosedTrackSource));
            let shared = SharedTrackSource::new(source);
            let handle = shared.handle();
            track.source = Box::new(shared);
            handle
        });
        registrar.supply(
            id,
            track.name.clone(),
            query_contig_aliases.get(&id).cloned().unwrap_or_default(),
            Box::new(crate::track_tile_source::LegacyTrackTileSource::new(
                Box::new(handle),
            )),
        );
    }
}

fn send_tdf_discovery(
    output: &CoalescingSender<TrackEngineEvent>,
    specification: &TrackSpec,
    track: &OpenTrack,
) {
    if specification.kind != TrackSpecKind::Tdf {
        return;
    }
    let Some((track_names, selected_index)) = track.source.channels() else {
        return;
    };
    send_event(
        output,
        TrackEngineEvent::TdfChannelsDiscovered {
            id: specification.id,
            track_names,
            selected_index,
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
            track
                .source
                .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
            let record = match track
                .source
                .adjacent_feature(query_contig, position, forward)
            {
                Ok(record) => record?,
                Err(error) => {
                    tracing::warn!(%error, track = %track.name, "feature navigation failed");
                    return None;
                }
            };
            let (id, name) = (track.id, track.name.clone());
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

/// Searches every track, or returns `None` as soon as `superseded` reports that a newer search
/// was queued. The check runs before the first track and again around every track, so an
/// abandoned query costs at most the one source scan already in progress.
#[allow(clippy::too_many_lines)]
fn search_tracks(
    tracks: &mut [OpenTrack],
    query: String,
    limit: usize,
    include_variant_ids: bool,
    superseded: &dyn Fn() -> bool,
) -> Option<TrackSearchResults> {
    if superseded() {
        return None;
    }
    let normalized_query = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut index_truncated = false;
    let mut scan_truncated = false;
    let request = SearchRequest {
        query: &query,
        normalized: &normalized_query,
        limit,
        include_variant_ids,
    };
    for track in tracks {
        if superseded() {
            return None;
        }
        track
            .source
            .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
        match track.source.search(request) {
            Ok(Some(results)) => {
                index_truncated |= results.index_truncated;
                scan_truncated |= results.scan_truncated;
                hits.extend(results.hits.into_iter().map(|(matched_term, record)| {
                    TrackSearchHit {
                        track_id: track.id,
                        track_name: track.name.clone(),
                        matched_term,
                        record,
                    }
                }));
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, track = %track.name, "feature search failed"),
        }
    }
    if superseded() {
        return None;
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
    Some(TrackSearchResults {
        query,
        hits,
        index_truncated,
        scan_truncated,
        variant_ids_included: include_variant_ids,
    })
}

/// Everything one viewport query shares across the tracks it reads.
struct TrackQueryContext {
    output: Arc<CoalescingSender<TrackEngineEvent>>,
    diagnostics: SharedTrackDiagnostics,
    clock: GenerationClock,
    request: PanelGeneration,
    contig: String,
    interval: GenomicInterval,
    variant_settings: Vec<(TrackId, VariantTrackSettings)>,
    interaction_settings: Vec<(TrackId, InteractionTrackSettings)>,
    copy_number_settings: Vec<(TrackId, CopyNumberTrackSettings)>,
}

impl TrackQueryContext {
    fn is_current(&self) -> bool {
        self.clock.is_current(self.request.generation())
    }

    /// A check a remote reader can hold: true while this request is still the panel's newest.
    /// It deliberately owns only the clock, never the event sender.
    fn remote_keep_going(&self) -> RemoteCancellation {
        let clock = self.clock.clone();
        let generation = self.request.generation();
        Arc::new(move || clock.is_current(generation))
    }
}

struct TrackQueryJob {
    index: usize,
    track: OpenTrack,
    query_contig: String,
    context: Arc<TrackQueryContext>,
}

struct TrackQueryJobResult {
    index: usize,
    track: OpenTrack,
    completed: Option<TrackId>,
}

/// Long-lived helper threads for querying independent tracks at the same time. They are started
/// once, on the first query that has more than one track to read, and reused for every later
/// query; the actor lends each helper one track and takes it back with the result.
struct TrackQueryPool {
    jobs: Option<Sender<TrackQueryJob>>,
    finished: Receiver<TrackQueryJobResult>,
    workers: Vec<JoinHandle<()>>,
}

impl TrackQueryPool {
    fn new(size: usize) -> Self {
        let (jobs, job_rx) = unbounded::<TrackQueryJob>();
        let (finished_tx, finished) = unbounded();
        let workers = (0..size.max(1))
            .filter_map(|ordinal| {
                let job_rx = job_rx.clone();
                let finished_tx = finished_tx.clone();
                thread::Builder::new()
                    .name(format!("rgv-track-query-{ordinal}"))
                    .spawn(move || track_query_helper(&job_rx, &finished_tx))
                    .inspect_err(
                        |error| tracing::warn!(%error, "could not start a track query helper"),
                    )
                    .ok()
            })
            .collect();
        Self {
            jobs: Some(jobs),
            finished,
            workers,
        }
    }

    fn submit(&self, job: TrackQueryJob) -> Result<(), Box<TrackQueryJob>> {
        match &self.jobs {
            Some(jobs) if !self.workers.is_empty() => {
                jobs.send(job).map_err(|error| Box::new(error.into_inner()))
            }
            _ => Err(Box::new(job)),
        }
    }
}

impl Drop for TrackQueryPool {
    fn drop(&mut self) {
        self.jobs.take();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                tracing::warn!("track query helper panicked while shutting down");
            }
        }
    }
}

/// Thread root of one query helper. A panic inside a source is contained per job: it becomes
/// that lane's error event, and the borrowed track always travels back to the actor.
fn track_query_helper(jobs: &Receiver<TrackQueryJob>, finished: &Sender<TrackQueryJobResult>) {
    while let Ok(job) = jobs.recv() {
        let TrackQueryJob {
            index,
            mut track,
            query_contig,
            context,
        } = job;
        let completed = query_one_contained(&mut track, &query_contig, &context);
        // Release the shared event sender before the actor can observe this job as finished.
        drop(context);
        if finished
            .send(TrackQueryJobResult {
                index,
                track,
                completed,
            })
            .is_err()
        {
            return;
        }
    }
}

fn query_one_contained(
    track: &mut OpenTrack,
    query_contig: &str,
    context: &TrackQueryContext,
) -> Option<TrackId> {
    let id = track.id();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        query_one(track, query_contig, context)
    })) {
        Ok(completed) => completed,
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            tracing::error!(message, track = %track.name, "track query panicked");
            send_event(
                &context.output,
                TrackEngineEvent::Error {
                    request: Some(context.request),
                    track: Some(id),
                    message: format!("{}: internal error: {message}", track.name),
                },
            );
            None
        }
    }
}

/// Queries one track for the request and publishes its lane. Both the start and the result are
/// checked against the generation clock: a lane that became stale while its source was being
/// read is dropped here rather than published for the UI to reject.
fn query_one(
    track: &mut OpenTrack,
    query_contig: &str,
    context: &TrackQueryContext,
) -> Option<TrackId> {
    if context.output.is_closed() || !context.is_current() {
        return None;
    }
    // A remote source stops transferring as soon as the view moves on, and may spend at most
    // one fresh allowance on this request.
    track.source.set_remote_control(
        Some(context.remote_keep_going()),
        Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET),
    );
    let result = query_track(
        track,
        query_contig,
        context.interval.start(),
        context.interval.end(),
        &context.variant_settings,
        &context.interaction_settings,
        &context.copy_number_settings,
    );
    // A search or feature step that follows must not be abandoned by this request's check.
    track
        .source
        .set_remote_control(None, Some(REMOTE_TRACK_OPERATION_TRANSFER_BUDGET));
    record_track_diagnostics(&context.diagnostics, track);
    if !context.is_current() {
        return None;
    }
    match result {
        Ok(snapshot) => {
            let id = snapshot.id;
            send_event(
                &context.output,
                TrackEngineEvent::LaneCompleted {
                    request: context.request,
                    interval: context.interval,
                    track: snapshot,
                },
            );
            Some(id)
        }
        Err((id, message)) => {
            send_event(
                &context.output,
                TrackEngineEvent::Error {
                    request: Some(context.request),
                    track: Some(id),
                    message,
                },
            );
            None
        }
    }
}

fn query_tracks(
    tracks: &mut Vec<OpenTrack>,
    query_contig_aliases: &HashMap<TrackId, BTreeMap<String, String>>,
    pool: &mut Option<TrackQueryPool>,
    context: &Arc<TrackQueryContext>,
    track_ids: Option<&[TrackId]>,
) {
    if !context.is_current() {
        return;
    }
    let selected = track_ids.map(|ids| ids.iter().copied().collect::<HashSet<_>>());
    let wanted = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| {
            selected
                .as_ref()
                .is_none_or(|selected| selected.contains(&track.id()))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let parallelism = MAXIMUM_PARALLEL_TRACK_QUERIES
        .min(thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get));
    let mut completed = Vec::with_capacity(wanted.len());
    if wanted.len().min(parallelism) <= 1 {
        for index in wanted {
            let track = &mut tracks[index];
            let query_contig =
                track_query_contig(query_contig_aliases, track.id(), &context.contig).to_owned();
            if let Some(id) = query_one_contained(track, &query_contig, context) {
                completed.push((index, id));
            }
        }
    } else {
        // Each source owns its reader, so tracks are independent: one slow file (a remote
        // BigWig, a large indexed annotation) does not delay the lanes behind it. The helpers
        // are persistent; no thread is created per query.
        let pool = pool.get_or_insert_with(|| TrackQueryPool::new(parallelism));
        let mut lent = std::mem::take(tracks)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut outstanding = 0_usize;
        for index in wanted {
            let track = lent[index]
                .take()
                .expect("each wanted track index is lent exactly once");
            let query_contig =
                track_query_contig(query_contig_aliases, track.id(), &context.contig).to_owned();
            match pool.submit(TrackQueryJob {
                index,
                track,
                query_contig,
                context: Arc::clone(context),
            }) {
                Ok(()) => outstanding += 1,
                Err(job) => {
                    // No helper could be started: read this track on the actor itself.
                    let TrackQueryJob {
                        index,
                        mut track,
                        query_contig,
                        ..
                    } = *job;
                    if let Some(id) = query_one_contained(&mut track, &query_contig, context) {
                        completed.push((index, id));
                    }
                    lent[index] = Some(track);
                }
            }
        }
        for _ in 0..outstanding {
            let Ok(result) = pool.finished.recv() else {
                tracing::error!("track query helpers stopped; unreturned tracks were closed");
                break;
            };
            if let Some(id) = result.completed {
                completed.push((result.index, id));
            }
            lent[result.index] = Some(result.track);
        }
        *tracks = lent.into_iter().flatten().collect();
    }
    completed.sort_unstable_by_key(|(index, _)| *index);
    let completed = completed.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
    if context.is_current() {
        send_event(
            &context.output,
            TrackEngineEvent::QueryCompleted {
                request: context.request,
                interval: context.interval,
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

pub(crate) type TdfSourceCache = HashMap<PathBuf, Arc<Mutex<TdfSource>>>;

/// Opens one track behind the public tile contract. bigWig gets the adapter that keeps the
/// file's zoom-level extremes; every other kind is opened exactly as [`open_track_with`] does
/// and wrapped in the generic adapter.
pub(crate) fn open_tile_source(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
    remote: &HttpRangeConfig,
) -> Result<Box<dyn crate::TrackTileSource>, String> {
    if specification.kind == TrackSpecKind::BigWig {
        return BigWigSource::open_with_remote_config(&specification.path, remote.clone())
            .map(|source| {
                Box::new(crate::track_tile_source::BigWigTileSource::new(source))
                    as Box<dyn crate::TrackTileSource>
            })
            .map_err(|error| error.to_string());
    }
    open_track_with(specification, tdf_sources, remote).map(|track| {
        Box::new(crate::track_tile_source::LegacyTrackTileSource::new(
            track.source,
        )) as Box<dyn crate::TrackTileSource>
    })
}

/// The feature records the legacy query returns for `[start, end)`: what the legacy
/// inspector is given, for tests that compare another path with it.
#[cfg(test)]
pub(crate) fn legacy_feature_records(
    specification: &TrackSpec,
    contig: &str,
    start: u64,
    end: u64,
) -> Vec<FeatureRecord> {
    let mut track = open_track(specification).expect("the fixture opens");
    match track
        .source
        .query(contig, start, end, TrackQuerySettings::default())
        .expect("the fixture is readable")
    {
        TrackSnapshotData::Features(records) => records,
        other => panic!("not a feature track: {other:?}"),
    }
}

#[cfg(test)]
fn open_track(specification: &TrackSpec) -> Result<OpenTrack, String> {
    open_track_cached(specification, &mut HashMap::new())
}

#[cfg(test)]
fn open_track_cached(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
) -> Result<OpenTrack, String> {
    open_track_with(specification, tdf_sources, &HttpRangeConfig::default())
}

/// Opens one track. `remote` configures every source RGV reads through its own HTTP client
/// (bigWig, bigBed, TDF and downloaded text annotations). VCF/BCF URLs are read by htslib and
/// the remaining text formats are local-only, so neither uses it.
fn open_track_with(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
    remote: &HttpRangeConfig,
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
            TextFeatureSource::open_with_remote_config(&specification.path, format, remote)
                .map(|source| OpenTrack::new(id, name, source))
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::VcfOrBcf => {
            LocalVariantSource::open_with_remote_config(&specification.path, remote.clone())
                .map(|source| OpenTrack::new(id, name, source))
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::Gwas => TextGwasSource::open(&specification.path)
            .map(|source| OpenTrack::new(id, name, source))
            .map_err(|error| error.to_string()),
        TrackSpecKind::BigWig => {
            BigWigSource::open_with_remote_config(&specification.path, remote.clone())
                .map(|source| OpenTrack::new(id, name, source))
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::Tdf | TrackSpecKind::TdfChannel { .. } => {
            open_tdf_track(specification, tdf_sources, remote)
        }
        TrackSpecKind::BedGraph | TrackSpecKind::Wig => {
            let format = if specification.kind == TrackSpecKind::BedGraph {
                QuantitativeFormat::BedGraph
            } else {
                QuantitativeFormat::Wig
            };
            TextQuantitativeSource::open(&specification.path, format)
                .map(|source| OpenTrack::new(id, name, source))
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::BedPe | TrackSpecKind::Interact => {
            let format = if specification.kind == TrackSpecKind::BedPe {
                InteractionFormat::BedPe
            } else {
                InteractionFormat::Interact
            };
            TextInteractionSource::open(&specification.path, format)
                .map(|source| OpenTrack::new(id, name, source))
                .map_err(|error| error.to_string())
        }
        TrackSpecKind::Seg => TextCopyNumberSource::open(&specification.path)
            .map(|source| OpenTrack::new(id, name, source))
            .map_err(|error| error.to_string()),
        TrackSpecKind::MutationMaf => TextMutationSource::open(&specification.path)
            .map(|source| OpenTrack::new(id, name, source))
            .map_err(|error| error.to_string()),
        TrackSpecKind::BigBed | TrackSpecKind::BigGenePred | TrackSpecKind::BigNarrowPeak => {
            let format = match specification.kind {
                TrackSpecKind::BigBed => BigBedFeatureFormat::Bed,
                TrackSpecKind::BigGenePred => BigBedFeatureFormat::BigGenePred,
                TrackSpecKind::BigNarrowPeak => BigBedFeatureFormat::BigNarrowPeak,
                _ => unreachable!(),
            };
            BigBedSource::open_with_format_and_remote_config(
                &specification.path,
                format,
                remote.clone(),
            )
            .map(|source| OpenTrack::new(id, name, source))
            .map_err(|error| error.to_string())
        }
    }
}

fn open_tdf_track(
    specification: &TrackSpec,
    tdf_sources: &mut TdfSourceCache,
    remote: &HttpRangeConfig,
) -> Result<OpenTrack, String> {
    let source = if let Some(source) = tdf_sources.get(&specification.path) {
        Arc::clone(source)
    } else {
        let source = Arc::new(Mutex::new(
            TdfSource::open_with_remote_config(&specification.path, None, remote.clone())
                .map_err(|error| error.to_string())?,
        ));
        tdf_sources.insert(specification.path.clone(), Arc::clone(&source));
        source
    };
    let (track_index, channel_count) = {
        let archive = source
            .lock()
            .map_err(|_| "the TDF reader failed in another query".to_owned())?;
        let track_index = match specification.kind {
            TrackSpecKind::Tdf => archive
                .track_index_by_name(&specification.name)
                .unwrap_or(0),
            TrackSpecKind::TdfChannel { index } => index,
            _ => unreachable!(),
        };
        (track_index, archive.metadata().track_names.len())
    };
    if track_index >= channel_count {
        return Err(format!(
            "TDF channel index {track_index} is outside the {channel_count} embedded channels"
        ));
    }
    Ok(OpenTrack::new(
        specification.id,
        specification.name.clone(),
        TdfChannel {
            source,
            track_index,
            keep_going: None,
            budget: None,
        },
    ))
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
    let id = track.id;
    let settings = TrackQuerySettings {
        variant: setting_for_track(variant_settings, id),
        interaction: setting_for_track(interaction_settings, id),
        copy_number: setting_for_track(copy_number_settings, id),
    };
    track
        .source
        .query(contig, start, end, settings)
        .map(|data| TrackSnapshot {
            id,
            name: track.name.clone(),
            data,
        })
        .map_err(|message| (id, message))
}

fn setting_for_track<T>(settings: &[(TrackId, T)], id: TrackId) -> Option<&T> {
    settings
        .iter()
        .find(|(track, _)| *track == id)
        .map(|(_, settings)| settings)
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
    use crate::mailbox::bounded_coalescing;

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

    fn test_query_context(
        output: CoalescingSender<TrackEngineEvent>,
        clock: &GenerationClock,
        request: PanelGeneration,
        interval: GenomicInterval,
    ) -> Arc<TrackQueryContext> {
        Arc::new(TrackQueryContext {
            output: Arc::new(output),
            diagnostics: SharedTrackDiagnostics::default(),
            clock: clock.clone(),
            request,
            contig: "chr1".to_owned(),
            interval,
            variant_settings: Vec::new(),
            interaction_settings: Vec::new(),
            copy_number_settings: Vec::new(),
        })
    }

    /// A source whose behaviour a test scripts: it can advance the generation clock while it is
    /// being read, panic, or record that it was searched.
    #[derive(Default)]
    struct ScriptedSource {
        advance_during_query: Option<GenerationClock>,
        panic_in_query: bool,
        searches: Option<Arc<AtomicU64>>,
        supersede_on_search: Option<Arc<AtomicU64>>,
    }

    impl TrackSource for ScriptedSource {
        fn query(
            &mut self,
            _contig: &str,
            _start: u64,
            _end: u64,
            _settings: TrackQuerySettings<'_>,
        ) -> Result<TrackSnapshotData, String> {
            assert!(!self.panic_in_query, "scripted source failure");
            if let Some(clock) = &self.advance_during_query {
                clock.advance();
            }
            Ok(TrackSnapshotData::Features(Vec::new()))
        }

        fn search(
            &mut self,
            _request: SearchRequest<'_>,
        ) -> Result<Option<crate::track_source::SourceSearch>, String> {
            if let Some(searches) = &self.searches {
                searches.fetch_add(1, Ordering::AcqRel);
            }
            if let Some(latest) = &self.supersede_on_search {
                latest.fetch_add(1, Ordering::AcqRel);
            }
            Ok(Some(crate::track_source::SourceSearch::default()))
        }
    }

    fn drain(receiver: &CoalescingReceiver<TrackEngineEvent>) -> Vec<TrackEngineEvent> {
        std::iter::from_fn(|| receiver.try_receive().unwrap()).collect()
    }

    #[test]
    fn a_lane_that_became_stale_while_its_source_was_read_is_not_published() {
        let (output, receiver) = bounded_coalescing(8);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
        let context = test_query_context(output, &clock, request, interval);
        let mut tracks = vec![OpenTrack::new(
            TrackId::new(1),
            "navigated away".to_owned(),
            ScriptedSource {
                advance_during_query: Some(clock.clone()),
                ..ScriptedSource::default()
            },
        )];

        query_tracks(&mut tracks, &HashMap::new(), &mut None, &context, None);

        assert_eq!(
            drain(&receiver),
            Vec::new(),
            "neither the stale lane nor its settlement marker may be published"
        );
    }

    #[test]
    fn a_panicking_source_becomes_that_lanes_error_and_spares_the_others() {
        let (output, receiver) = bounded_coalescing(8);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
        let context = test_query_context(output, &clock, request, interval);
        let mut tracks = vec![
            OpenTrack::new(
                TrackId::new(1),
                "broken".to_owned(),
                ScriptedSource {
                    panic_in_query: true,
                    ..ScriptedSource::default()
                },
            ),
            OpenTrack::new(
                TrackId::new(2),
                "healthy".to_owned(),
                ScriptedSource::default(),
            ),
        ];
        let mut pool = None;

        query_tracks(&mut tracks, &HashMap::new(), &mut pool, &context, None);

        assert_eq!(tracks.len(), 2, "the panicking track is still returned");
        let events = drain(&receiver);
        assert!(events.iter().any(|event| matches!(
            event,
            TrackEngineEvent::Error {
                request: Some(failed),
                track: Some(track),
                message,
            } if *failed == request
                && *track == TrackId::new(1)
                && message.contains("scripted source failure")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            TrackEngineEvent::LaneCompleted { track, .. } if track.id == TrackId::new(2)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            TrackEngineEvent::QueryCompleted { completed, .. }
                if completed == &[TrackId::new(2)]
        )));
    }

    #[test]
    fn a_superseded_search_stops_before_and_between_tracks() {
        let searches = Arc::new(AtomicU64::new(0));
        let latest = Arc::new(AtomicU64::new(1));
        let scripted = |supersede: bool| ScriptedSource {
            searches: Some(Arc::clone(&searches)),
            supersede_on_search: supersede.then(|| Arc::clone(&latest)),
            ..ScriptedSource::default()
        };
        let mut tracks = vec![
            OpenTrack::new(TrackId::new(1), "first".to_owned(), scripted(true)),
            OpenTrack::new(TrackId::new(2), "second".to_owned(), scripted(false)),
        ];
        let superseded = |generation: u64| {
            let latest = Arc::clone(&latest);
            move || latest.load(Ordering::Acquire) > generation
        };

        // A newer search is queued while the first track is being scanned: the second track is
        // never read and no result is produced.
        assert!(search_tracks(&mut tracks, "gene".to_owned(), 10, false, &superseded(1)).is_none());
        assert_eq!(searches.load(Ordering::Acquire), 1);

        // Already superseded on arrival: nothing is read at all.
        assert!(search_tracks(&mut tracks, "gene".to_owned(), 10, false, &superseded(1)).is_none());
        assert_eq!(searches.load(Ordering::Acquire), 1);

        // The current search runs to completion.
        tracks.remove(0);
        let current = latest.load(Ordering::Acquire);
        assert!(
            search_tracks(
                &mut tracks,
                "gene".to_owned(),
                10,
                false,
                &superseded(current)
            )
            .is_some()
        );
        assert_eq!(searches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn the_engine_answers_only_the_newest_queued_search() {
        let engine = TrackEngine::spawn(Vec::new(), 8);
        // Both are queued before the actor can have finished the first; whichever it reaches,
        // only generation 2 may ever be answered.
        engine.try_search(1, "a".to_owned(), 10, false).unwrap();
        engine.try_search(2, "b".to_owned(), 10, false).unwrap();
        let answered = loop {
            match engine
                .output
                .receive_timeout(std::time::Duration::from_secs(30))
                .unwrap()
            {
                Some(TrackEngineEvent::SearchCompleted { generation, .. }) => {
                    if generation == 2 {
                        break generation;
                    }
                    // Generation 1 may legitimately finish before generation 2 is queued.
                    assert_eq!(generation, 1);
                }
                Some(TrackEngineEvent::Ready { .. }) | None => {}
                Some(other) => panic!("unexpected track event {other:?}"),
            }
        };
        assert_eq!(answered, 2);
        assert_eq!(engine.latest_search.load(Ordering::Acquire), 2);
    }

    #[test]
    fn many_tracks_are_queried_concurrently_and_reported_in_track_order() {
        let directory = tempfile::tempdir().unwrap();
        let mut tracks = Vec::new();
        for index in 0..12_u64 {
            let path = directory.path().join(format!("features-{index}.bed"));
            std::fs::write(
                &path,
                format!("chr1\t{}\t{}\tfeature{index}\n", 100 + index, 200),
            )
            .unwrap();
            tracks.push(
                open_track(&TrackSpec {
                    id: TrackId::new(index + 1),
                    name: format!("track {index}"),
                    path,
                    kind: TrackSpecKind::Bed,
                })
                .unwrap(),
            );
        }
        let (output, receiver) = bounded_coalescing(64);
        let clock = GenerationClock::default();
        let request = PanelGeneration::new(PanelId::new(1), clock.current());
        let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
        let context = test_query_context(output, &clock, request, interval);
        let mut pool = None;

        for _ in 0..2 {
            // The second pass reuses the helpers the first one started.
            query_tracks(
                &mut tracks,
                &HashMap::new(),
                &mut pool,
                &context,
                Some(&(2..=11).map(TrackId::new).collect::<Vec<_>>()),
            );
            if let Some(pool) = &pool {
                assert!(pool.workers.len() <= MAXIMUM_PARALLEL_TRACK_QUERIES);
            }
            assert_eq!(
                tracks.iter().map(OpenTrack::id).collect::<Vec<_>>(),
                (1..=12).map(TrackId::new).collect::<Vec<_>>(),
                "every lent track is returned to its original position"
            );
        }

        let mut lanes = Vec::new();
        let mut completed = None;
        while let Some(event) = receiver.try_receive().unwrap() {
            match event {
                TrackEngineEvent::LaneCompleted { track, .. } => {
                    let TrackSnapshotData::Features(records) = &track.data else {
                        panic!("a BED track returned the wrong snapshot type");
                    };
                    assert_eq!(
                        records[0].name.as_deref(),
                        Some(format!("feature{}", track.id.get() - 1).as_str())
                    );
                    lanes.push(track.id);
                }
                TrackEngineEvent::QueryCompleted { completed: ids, .. } => completed = Some(ids),
                other => panic!("unexpected track event {other:?}"),
            }
        }
        lanes.sort_unstable();
        let expected = (2..=11).map(TrackId::new).collect::<Vec<_>>();
        assert_eq!(lanes, expected, "only the selected tracks are queried");
        assert_eq!(
            completed,
            Some(expected),
            "completion lists tracks in order"
        );
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

        let results = search_tracks(&mut tracks, "BRCA".to_owned(), 10, false, &|| false).unwrap();

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

        let search =
            search_tracks(&mut tracks, "query-1".to_owned(), 10, false, &|| false).unwrap();
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

        let search = search_tracks(&mut tracks, "RS-FIR".to_owned(), 10, false, &|| false).unwrap();
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
        // Both channels read through the one archive opened for the file.
        assert_eq!(cache.len(), 1);
        assert_eq!(Arc::strong_count(cache.values().next().unwrap()), 3);

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

    /// Serves byte ranges of `data` to requests carrying `Authorization: Bearer track-secret`.
    /// While a clock is parked in `advance_on_request`, the next authorised request takes it and
    /// advances it: the view "moves on" in the middle of a remote read.
    struct AuthorizedRangeServer {
        url: String,
        stop: Arc<std::sync::atomic::AtomicBool>,
        advance_on_request: Arc<Mutex<Option<GenerationClock>>>,
        worker: Option<JoinHandle<()>>,
    }

    impl AuthorizedRangeServer {
        fn start(data: Vec<u8>) -> Self {
            Self::start_objects(vec![("archive.tdf", data)])
        }

        /// Serves several named objects; `url` names the first.
        fn start_objects(objects: Vec<(&'static str, Vec<u8>)>) -> Self {
            let first = objects[0].0;
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let advance_on_request = Arc::new(Mutex::new(None::<GenerationClock>));
            let worker_stop = Arc::clone(&stop);
            let worker_advance = Arc::clone(&advance_on_request);
            let header = |name: &str, value: &str| {
                tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
            };
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Some(request) = server
                        .recv_timeout(std::time::Duration::from_millis(20))
                        .unwrap()
                    else {
                        continue;
                    };
                    let authorised = request.headers().iter().any(|header| {
                        header.field.equiv("Authorization")
                            && header.value.as_str() == "Bearer track-secret"
                    });
                    if !authorised {
                        request
                            .respond(tiny_http::Response::empty(tiny_http::StatusCode(401)))
                            .unwrap();
                        continue;
                    }
                    let name = request.url().rsplit('/').next().unwrap_or_default();
                    let Some(data) = objects
                        .iter()
                        .find(|(object, _)| *object == name)
                        .map(|(_, data)| data)
                    else {
                        request
                            .respond(tiny_http::Response::empty(tiny_http::StatusCode(404)))
                            .unwrap();
                        continue;
                    };
                    if let Some(clock) = worker_advance.lock().unwrap().take() {
                        clock.advance();
                    }
                    let (start, end) = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| {
                            let (start, end) = header
                                .value
                                .as_str()
                                .strip_prefix("bytes=")?
                                .split_once('-')?;
                            let last = data.len().checked_sub(1)?;
                            let start = start.parse::<usize>().ok()?;
                            let end = if end.is_empty() {
                                last
                            } else {
                                end.parse::<usize>().ok()?.min(last)
                            };
                            (start <= end).then_some((start, end))
                        })
                        .unwrap();
                    let response = tiny_http::Response::from_data(data[start..=end].to_vec())
                        .with_status_code(tiny_http::StatusCode(206))
                        .with_header(header(
                            "Content-Range",
                            &format!("bytes {start}-{end}/{}", data.len()),
                        ))
                        .with_header(header("ETag", "\"track-fixture\""));
                    request.respond(response).unwrap();
                }
            });
            Self {
                url: format!("http://{address}/{first}"),
                stop,
                advance_on_request,
                worker: Some(worker),
            }
        }
    }

    impl Drop for AuthorizedRangeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    #[test]
    fn remote_tracks_use_the_engine_remote_config_cancellation_and_budget() {
        let server = AuthorizedRangeServer::start(tdf_fixture());
        let specification = TrackSpec {
            id: TrackId::new(40),
            name: "engine signal".to_owned(),
            path: PathBuf::from(&server.url),
            kind: TrackSpecKind::Tdf,
        };
        // The anonymous default configuration is refused by the server.
        assert!(
            open_track_with(
                &specification,
                &mut TdfSourceCache::new(),
                &HttpRangeConfig::default()
            )
            .is_err()
        );
        let remote = HttpRangeConfig {
            authentication: rgv_hts::RemoteAuthentication::Bearer("track-secret".to_owned()),
            // One 64-byte block is cached at a time, so every query below must fetch.
            chunk_size: std::num::NonZeroUsize::new(64).unwrap(),
            maximum_cache_bytes: std::num::NonZeroUsize::new(64).unwrap(),
            maximum_retries: 0,
            ..HttpRangeConfig::default()
        };
        let mut cache = TdfSourceCache::new();
        let mut track = open_track_with(&specification, &mut cache, &remote).unwrap();
        let archive = Arc::clone(&cache[&specification.path]);
        let stats = || archive.lock().unwrap().remote_io_stats().unwrap();

        let clock = GenerationClock::default();
        let interval = GenomicInterval::new(ContigId::new(0), 100, 120).unwrap();
        let request =
            |clock: &GenerationClock| PanelGeneration::new(PanelId::new(1), clock.current());
        let (output, receiver) = bounded_coalescing(8);
        let context = test_query_context(output, &clock, request(&clock), interval);
        let lane_values =
            |receiver: &CoalescingReceiver<TrackEngineEvent>| match receiver.try_receive().unwrap()
            {
                Some(TrackEngineEvent::LaneCompleted { track, .. }) => {
                    let TrackSnapshotData::Quantitative(query) = track.data else {
                        panic!("TDF source returned the wrong snapshot type");
                    };
                    query
                        .records
                        .iter()
                        .map(|record| record.value)
                        .collect::<Vec<_>>()
                }
                other => panic!("expected a lane, got {other:?}"),
            };

        // The budget reaches the shared reader: an exhausted allowance fails the first read...
        track.source.set_remote_control(
            None,
            Some(RemoteTransferBudget {
                maximum_requests: 0,
                maximum_response_bytes: 0,
            }),
        );
        let (_, over_budget) =
            query_track(&mut track, "chr1", 100, 120, &[], &[], &[]).unwrap_err();
        assert!(over_budget.contains("budget"), "{over_budget}");
        // ...and every engine query starts with a fresh one. The fixture's first channel holds
        // 2.0 over 100..110 and 4.0 over 110..120.
        assert_eq!(
            query_one(&mut track, "chr1", &context),
            Some(TrackId::new(40))
        );
        assert_eq!(lane_values(&receiver), [2.0, 4.0]);
        assert_eq!(stats().cancellations, 0);

        // A second reader that has not fetched its data yet. The view moves on while the first
        // range of its query is in flight: the reader abandons the transfer instead of
        // finishing a lane nobody wants.
        let mut cache = TdfSourceCache::new();
        let mut track = open_track_with(&specification, &mut cache, &remote).unwrap();
        let archive = Arc::clone(&cache[&specification.path]);
        let stats = || archive.lock().unwrap().remote_io_stats().unwrap();
        *server.advance_on_request.lock().unwrap() = Some(clock.clone());
        assert_eq!(query_one(&mut track, "chr1", &context), None);
        assert!(server.advance_on_request.lock().unwrap().is_none());
        assert_eq!(receiver.try_receive().unwrap(), None);
        assert_eq!(stats().cancellations, 1);

        // The stale check does not outlive its request.
        let (output, receiver) = bounded_coalescing(8);
        let context = test_query_context(output, &clock, request(&clock), interval);
        assert_eq!(
            query_one(&mut track, "chr1", &context),
            Some(TrackId::new(40))
        );
        assert_eq!(lane_values(&receiver), [2.0, 4.0]);
        assert_eq!(stats().cancellations, 1);
    }

    #[test]
    fn remote_variant_tracks_use_the_engine_remote_config_cancellation_and_budget() {
        let fixture = |name: &str| {
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../benchmark-data/manual-suite/data/variants")
                    .join(name),
            )
            .unwrap()
        };
        let server = AuthorizedRangeServer::start_objects(vec![
            ("cohort.vcf.gz", fixture("cohort.vcf.gz")),
            ("cohort.vcf.gz.tbi", fixture("cohort.vcf.gz.tbi")),
        ]);
        let specification = TrackSpec {
            id: TrackId::new(41),
            name: "engine variants".to_owned(),
            path: PathBuf::from(&server.url),
            kind: TrackSpecKind::VcfOrBcf,
        };
        // The server refuses every request without the bearer token, so opening with the
        // anonymous default fails and opening at all proves the engine's configuration (and
        // its authentication header) reached the variant reader.
        assert!(
            open_track_with(
                &specification,
                &mut TdfSourceCache::new(),
                &HttpRangeConfig::default()
            )
            .is_err()
        );
        let remote = HttpRangeConfig {
            authentication: rgv_hts::RemoteAuthentication::Bearer("track-secret".to_owned()),
            // One 64-byte block is cached at a time, so every query below must fetch.
            chunk_size: std::num::NonZeroUsize::new(64).unwrap(),
            maximum_cache_bytes: std::num::NonZeroUsize::new(64).unwrap(),
            maximum_retries: 0,
            ..HttpRangeConfig::default()
        };
        let open = || open_track_with(&specification, &mut TdfSourceCache::new(), &remote).unwrap();
        let mut track = open();

        let clock = GenerationClock::default();
        let interval = GenomicInterval::new(ContigId::new(0), 15_000, 16_100).unwrap();
        let request =
            |clock: &GenerationClock| PanelGeneration::new(PanelId::new(1), clock.current());
        let (output, receiver) = bounded_coalescing(8);
        let context = test_query_context(output, &clock, request(&clock), interval);
        let lane_records =
            |receiver: &CoalescingReceiver<TrackEngineEvent>| match receiver.try_receive().unwrap()
            {
                Some(TrackEngineEvent::LaneCompleted { track, .. }) => {
                    let TrackSnapshotData::Variants(query) = track.data else {
                        panic!("variant source returned the wrong snapshot type");
                    };
                    query.records.len()
                }
                other => panic!("expected a lane, got {other:?}"),
            };

        // The budget reaches the remote reader: an exhausted allowance fails the read...
        track.source.set_remote_control(
            None,
            Some(RemoteTransferBudget {
                maximum_requests: 0,
                maximum_response_bytes: 0,
            }),
        );
        let (_, over_budget) =
            query_track(&mut track, "chrVariants", 15_000, 16_100, &[], &[], &[]).unwrap_err();
        assert!(over_budget.contains("budget"), "{over_budget}");
        // ...and every engine query starts with a fresh one.
        assert_eq!(
            query_one(&mut track, "chrVariants", &context),
            Some(TrackId::new(41))
        );
        assert!(lane_records(&receiver) > 0);

        // A reader which has not fetched its data yet. The view moves on while the first range
        // of its query is in flight: the transfer is abandoned and no lane is published.
        let mut track = open();
        *server.advance_on_request.lock().unwrap() = Some(clock.clone());
        assert_eq!(query_one(&mut track, "chrVariants", &context), None);
        assert!(server.advance_on_request.lock().unwrap().is_none());
        assert_eq!(receiver.try_receive().unwrap(), None);

        // The stale check does not outlive its request.
        let (output, receiver) = bounded_coalescing(8);
        let context = test_query_context(output, &clock, request(&clock), interval);
        assert_eq!(
            query_one(&mut track, "chrVariants", &context),
            Some(TrackId::new(41))
        );
        assert!(lane_records(&receiver) > 0);
    }

    #[test]
    fn skipped_feature_lines_are_reported_through_the_engine_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blemished.bed");
        // Lines 2 (start is not a number) and 4 (end before start) cannot be parsed.
        std::fs::write(
            &path,
            "chr1\t10\t20\tfirst\n\
             chr1\tnot-a-number\t30\tbroken\n\
             chr1\t40\t50\tsecond\n\
             chr1\t70\t60\tbackwards\n",
        )
        .unwrap();
        let clean = directory.path().join("clean.bed");
        std::fs::write(&clean, "chr1\t10\t20\tonly\n").unwrap();
        let spec = |id, path: &std::path::Path| TrackSpec {
            id: TrackId::new(id),
            name: format!("track-{id}"),
            path: path.to_path_buf(),
            kind: TrackSpecKind::Bed,
        };
        let engine = TrackEngine::spawn(vec![spec(50, &path), spec(51, &clean)], 4);
        let wait_for = |wanted: &dyn Fn(&TrackEngineEvent) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match engine.try_receive().unwrap() {
                    Some(event) if wanted(&event) => break,
                    Some(_) => {}
                    None => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "track actor timed out"
                        );
                        std::thread::yield_now();
                    }
                }
            }
        };
        wait_for(&|event| {
            matches!(
                event,
                TrackEngineEvent::Ready {
                    opened: 2,
                    failed: 0
                }
            )
        });

        let diagnostics = engine.source_diagnostics();
        assert_eq!(diagnostics.len(), 1, "the clean track reports nothing");
        let (id, report) = &diagnostics[0];
        assert_eq!(*id, TrackId::new(50));
        assert_eq!(report.malformed_lines, 2);
        let (line, message) = report.first_malformed_line.as_ref().unwrap();
        assert_eq!(*line, 2);
        assert!(message.contains("BED start"), "{message}");

        engine.try_remove_many(vec![TrackId::new(50)]).unwrap();
        wait_for(&|event| matches!(event, TrackEngineEvent::TracksRemoved { .. }));
        assert!(engine.source_diagnostics().is_empty());
    }

    #[test]
    fn sharing_moves_the_reader_behind_one_lock_and_never_opens_another() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genes.bed");
        std::fs::write(&path, "chr1\t100\t200\talpha\n").unwrap();
        let mut tracks = vec![
            open_track(&TrackSpec {
                id: TrackId::new(3),
                name: "genes".to_owned(),
                path,
                kind: TrackSpecKind::Bed,
            })
            .unwrap(),
        ];
        assert!(tracks[0].source.share_handle().is_none());
        let tiles = crate::TrackTileEngine::spawn(crate::TrackTileEngineConfig::default());
        let registrar = tiles.registrar();
        registrar.expect(&[TrackId::new(3)]);
        share_tile_sources(
            &mut tracks,
            &HashMap::new(),
            vec![TrackId::new(3)],
            &registrar,
        );
        // The legacy handle, the tile engine's handle, and the one taken here to count.
        let count = |tracks: &[OpenTrack]| {
            tracks[0]
                .source
                .share_handle()
                .expect("the track is shared")
                .handle_count()
        };
        assert_eq!(count(&tracks), 3);
        // The legacy engine still reads, through the same reader.
        let TrackSnapshotData::Features(features) = tracks[0]
            .source
            .query("chr1", 0, 1_000, TrackQuerySettings::default())
            .unwrap()
        else {
            panic!("expected features");
        };
        assert_eq!(features.len(), 1);
        // Sharing again (a second tile engine, or the same after a reset) adds a handle; the
        // reader is not wrapped twice.
        registrar.expect(&[TrackId::new(3)]);
        share_tile_sources(
            &mut tracks,
            &HashMap::new(),
            vec![TrackId::new(3)],
            &registrar,
        );
        // The first tile-engine handle was dropped with the registration it belonged to.
        assert_eq!(count(&tracks), 3);
        drop(tiles);
    }
}
