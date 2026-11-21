//! `query_features` and `goto_gene`: annotation queries answered by the track engine.
//!
//! Both commands ask the track engine a question and wait for its answer in
//! [`AutomationWait::TrackReply`]. Neither borrows a panel's query state:
//!
//! * `query_features` submits an ordinary track query under a request identity that belongs to
//!   no panel ([`AUTOMATION_FEATURE_QUERY_PANEL`]) with its own cancellation clock, so the
//!   answer covers exactly the requested interval whatever the panels show, and no panel can
//!   accept it. [`Viewer::route_automation_track_event`] takes those events out of the stream
//!   before the panel bookkeeping sees them.
//! * `goto_gene` runs a feature search under a generation that the interactive search never
//!   uses, so its result can only complete this command and a stale result is ignored.
//!
//! Everything that decides an answer is a free function over plain data and is tested below.

use crate::Viewer;
use crate::automation::commands::evidence::bounded_automation_text;
use crate::automation::{AutomationExecution, AutomationWait, automation_failure};
use crate::workspace::navigation::{
    annotation_search_hit_primary_label, preferred_annotation_search_hit,
};
use rgv_automation::{
    AutomationErrorCode, AutomationFeature, AutomationFeatureQueryResult, AutomationGeneHit,
    AutomationGeneNavigationResult,
};
use rgv_core::{
    ContigAliases, ContigId, GenerationClock, GenomicInterval, PanelGeneration, PanelId,
    ReferenceDictionary, TrackId, TrackKind, resolve_contig_name,
};
use rgv_engine::{
    EngineQueueError, TrackEngineEvent, TrackSearchHit, TrackSearchResults, TrackSnapshot,
    TrackSnapshotData,
};
use rgv_hts::{FeatureRecord, Strand};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The request identity of automation feature queries. Panel IDs are small positive integers
/// handed out in order, and the two largest values are the engine's own mailbox keys for search
/// and adjacent-feature results, so no panel and no other event shares this identity.
pub(crate) const AUTOMATION_FEATURE_QUERY_PANEL: PanelId = PanelId::new(u64::MAX - 2);

/// How long the track engine may take to answer either command.
const AUTOMATION_TRACK_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest name, feature type or gene symbol returned for one feature.
const MAXIMUM_FEATURE_TEXT_BYTES: usize = 256;
/// Most candidates the interactive search asks for; `goto_gene` asks for the same number so
/// both apply the preference rule to the same candidates.
const GENE_SEARCH_CANDIDATES: usize = 50;
/// Most candidates named in an `ambiguous_match` message.
const MAXIMUM_LISTED_CANDIDATES: usize = 5;

/// The cancellation clock shared by every automation feature query. Each query advances it, so
/// each has a generation no earlier query had and starting one abandons the one before.
fn feature_query_clock() -> &'static GenerationClock {
    static CLOCK: OnceLock<GenerationClock> = OnceLock::new();
    CLOCK.get_or_init(GenerationClock::default)
}

/// What an automation command is waiting for the track engine to send.
#[derive(Clone)]
pub(crate) enum PendingTrackReply {
    Features(Arc<FeatureQueryWait>),
    Gene(Arc<GeneSearchWait>),
}

pub(crate) struct FeatureQueryWait {
    request: PanelGeneration,
    interval: GenomicInterval,
    contig: String,
    /// The tracks that were asked, in workspace order.
    track_order: Vec<TrackId>,
    offset: usize,
    limit: usize,
    reply: Mutex<FeatureQueryReply>,
}

#[derive(Default)]
struct FeatureQueryReply {
    lanes: Vec<TrackSnapshot>,
    errors: Vec<(Option<TrackId>, String)>,
    /// The tracks the engine reported as answered; set by its settlement event.
    completed: Option<Vec<TrackId>>,
}

pub(crate) struct GeneSearchWait {
    generation: u64,
    query: String,
    panel_id: PanelId,
    flank_bases: u64,
    results: Mutex<Option<TrackSearchResults>>,
}

impl FeatureQueryWait {
    /// Records `event` when it answers this query. Returns whether it did.
    fn accept(&self, event: TrackEngineEvent) -> bool {
        let mut reply = self.reply.lock().unwrap_or_else(PoisonError::into_inner);
        match event {
            TrackEngineEvent::LaneCompleted {
                request,
                interval,
                track,
            } if request == self.request && interval == self.interval => {
                reply.lanes.retain(|lane| lane.id != track.id);
                reply.lanes.push(track);
                true
            }
            TrackEngineEvent::QueryCompleted {
                request,
                interval,
                completed,
            } if request == self.request && interval == self.interval => {
                reply.completed = Some(completed);
                true
            }
            TrackEngineEvent::Error {
                request: Some(request),
                track,
                message,
            } if request == self.request => {
                reply.errors.push((track, message));
                true
            }
            _ => false,
        }
    }

    /// The command's outcome once the engine has settled the query, or `None` while lanes are
    /// still arriving.
    fn outcome(
        &self,
    ) -> Option<Result<AutomationFeatureQueryResult, (AutomationErrorCode, String)>> {
        let reply = self.reply.lock().unwrap_or_else(PoisonError::into_inner);
        let completed = reply.completed.as_ref()?;
        if !completed
            .iter()
            .all(|id| reply.lanes.iter().any(|lane| lane.id == *id))
        {
            return None;
        }
        if let Some(failure) = feature_query_failure(&reply.errors) {
            return Some(Err(failure));
        }
        let lanes = ordered_feature_lanes(&self.track_order, &reply.lanes);
        if lanes.is_empty() {
            return Some(Err((
                AutomationErrorCode::ResourceNotFound,
                "none of the requested feature tracks is open yet".to_owned(),
            )));
        }
        Some(Ok(feature_query_page(
            &self.contig,
            self.interval.start(),
            self.interval.end(),
            &lanes,
            self.offset,
            self.limit,
        )))
    }
}

/// A source that failed makes the whole answer unreliable, so the command fails with the first
/// failure; a per-query record limit is reported as `too_dense`.
fn feature_query_failure(
    errors: &[(Option<TrackId>, String)],
) -> Option<(AutomationErrorCode, String)> {
    let (track, message) = errors.first()?;
    let code = if message.contains("record limit") {
        AutomationErrorCode::TooDense
    } else {
        AutomationErrorCode::Io
    };
    let message = match track {
        Some(track) => format!("feature track {} could not answer: {message}", track.get()),
        None => format!("the feature query failed: {message}"),
    };
    Some((code, message))
}

/// The feature lanes among `snapshots`, in workspace track order.
fn ordered_feature_lanes<'a>(
    track_order: &[TrackId],
    snapshots: &'a [TrackSnapshot],
) -> Vec<(TrackId, &'a [FeatureRecord])> {
    track_order
        .iter()
        .filter_map(|id| {
            snapshots.iter().find_map(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) if snapshot.id == *id => {
                    Some((*id, records.as_slice()))
                }
                _ => None,
            })
        })
        .collect()
}

const fn strand_symbol(strand: Strand) -> &'static str {
    match strand {
        Strand::Forward => "+",
        Strand::Reverse => "-",
        Strand::Unstranded => ".",
    }
}

fn feature_label(record: &FeatureRecord) -> &str {
    record.display_label().unwrap_or_default()
}

/// One page of the features overlapping `[start, end)`, ordered by track (the order of
/// `lanes`), then start, end and name. The order depends only on the records, so a later page
/// of the same query continues exactly where this one stopped.
pub(crate) fn feature_query_page(
    contig: &str,
    start: u64,
    end: u64,
    lanes: &[(TrackId, &[FeatureRecord])],
    offset: usize,
    limit: usize,
) -> AutomationFeatureQueryResult {
    let mut matching = Vec::new();
    for (track_id, records) in lanes {
        let first = matching.len();
        matching.extend(
            records
                .iter()
                // A zero-length record still occupies its anchor base.
                .filter(|record| {
                    record.start < end && record.end.max(record.start.saturating_add(1)) > start
                })
                .map(|record| (*track_id, record)),
        );
        // Stable, so records that compare equal keep their source order.
        matching[first..].sort_by(|(_, left), (_, right)| {
            (left.start, left.end, feature_label(left)).cmp(&(
                right.start,
                right.end,
                feature_label(right),
            ))
        });
    }
    let total_matching = matching.len();
    let features = matching
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(track_id, record)| AutomationFeature {
            track_id: track_id.get(),
            name: bounded_automation_text(feature_label(record), MAXIMUM_FEATURE_TEXT_BYTES),
            feature_type: bounded_automation_text(
                record.feature_type.as_deref().unwrap_or("feature"),
                MAXIMUM_FEATURE_TEXT_BYTES,
            ),
            contig: record.contig.clone(),
            start: record.start,
            end: record.end,
            strand: strand_symbol(record.strand).to_owned(),
            gene_name: record
                .presentation_identity()
                .display_symbol
                .map(|symbol| bounded_automation_text(symbol, MAXIMUM_FEATURE_TEXT_BYTES)),
        })
        .collect::<Vec<_>>();
    AutomationFeatureQueryResult {
        contig: contig.to_owned(),
        start,
        end,
        track_ids: lanes.iter().map(|(id, _)| id.get()).collect(),
        truncated: offset.saturating_add(features.len()) < total_matching,
        features,
        total_matching,
        offset,
    }
}

/// The feature and gene-model tracks a query reads, in workspace order: every one of them, or
/// the requested subset. A requested track that does not exist is `resource_not_found`; one of
/// another kind cannot hold features and is `invalid_command`.
pub(crate) fn select_feature_tracks(
    tracks: &[(TrackId, TrackKind)],
    requested: Option<&[TrackId]>,
) -> Result<Vec<TrackId>, (AutomationErrorCode, String)> {
    let holds_features =
        |kind: TrackKind| matches!(kind, TrackKind::Feature | TrackKind::GeneModel);
    if let Some(requested) = requested {
        for id in requested {
            match tracks.iter().find(|(track, _)| track == id) {
                None => {
                    return Err((
                        AutomationErrorCode::ResourceNotFound,
                        format!("track {} was not found", id.get()),
                    ));
                }
                Some((_, kind)) if !holds_features(*kind) => {
                    return Err((
                        AutomationErrorCode::InvalidCommand,
                        format!("track {} is not a feature or gene-model track", id.get()),
                    ));
                }
                Some(_) => {}
            }
        }
    }
    let selected = tracks
        .iter()
        .filter(|(id, kind)| {
            holds_features(*kind) && requested.is_none_or(|requested| requested.contains(id))
        })
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err((
            AutomationErrorCode::ResourceNotFound,
            "no feature or gene-model track is loaded".to_owned(),
        ));
    }
    Ok(selected)
}

/// The interval a feature query reads: the contig as the workspace names it and `[start, end)`
/// clamped to its length. Without a dictionary the contig is taken as given and nothing is
/// clamped, exactly as a typed locus is in a workspace that has no reference.
pub(crate) fn resolve_feature_interval(
    dictionary: Option<&ReferenceDictionary>,
    aliases: &ContigAliases,
    contig: &str,
    start: u64,
    end: u64,
) -> Result<(GenomicInterval, String), (AutomationErrorCode, String)> {
    let invalid = |message: String| (AutomationErrorCode::InvalidCommand, message);
    let Some(dictionary) = dictionary else {
        let interval = GenomicInterval::new(ContigId::new(0), start, end)
            .map_err(|error| invalid(error.to_string()))?;
        return Ok((interval, contig.trim().to_owned()));
    };
    let resolved = resolve_contig_name(dictionary, aliases, contig)
        .ok()
        .flatten()
        .and_then(|id| dictionary.get(id));
    let Some(resolved) = resolved else {
        return Err((
            AutomationErrorCode::ResourceNotFound,
            format!(
                "contig {} is not in the reference dictionary",
                contig.trim()
            ),
        ));
    };
    if start >= resolved.length() {
        return Err(invalid(format!(
            "start {start} is beyond the end of contig {} ({} bases)",
            resolved.name(),
            resolved.length()
        )));
    }
    let interval = GenomicInterval::new(resolved.id(), start, end.min(resolved.length()))
        .map_err(|error| invalid(error.to_string()))?;
    Ok((interval, resolved.name().to_owned()))
}

/// What a gene search decided.
#[derive(Debug, PartialEq)]
pub(crate) enum GeneMatch<'a> {
    /// The one hit the interactive search would navigate to.
    Preferred(&'a TrackSearchHit),
    NotFound,
    /// Several candidates and no single exact match.
    Ambiguous,
}

/// Applies the interactive search's preference rule: the only exact match, or the only hit.
pub(crate) fn gene_match<'a>(results: &'a TrackSearchResults, query: &str) -> GeneMatch<'a> {
    match preferred_annotation_search_hit(results, query) {
        Some(hit) => GeneMatch::Preferred(hit),
        None if results.hits.is_empty() => GeneMatch::NotFound,
        None => GeneMatch::Ambiguous,
    }
}

/// The zero-based half-open interval to show for a hit: the feature padded by `flank_bases` on
/// each side, kept inside the contig when its length is known. Never empty.
pub(crate) fn padded_hit_interval(
    start: u64,
    end: u64,
    flank_bases: u64,
    contig_length: Option<u64>,
) -> (u64, u64) {
    let mut padded_end = end.max(start.saturating_add(1)).saturating_add(flank_bases);
    if let Some(length) = contig_length.filter(|length| *length > 0) {
        padded_end = padded_end.min(length);
    }
    let padded_start = start
        .saturating_sub(flank_bases)
        .min(padded_end.saturating_sub(1));
    (padded_start, padded_end)
}

/// A one-based inclusive locus string for a zero-based half-open interval.
fn one_based_locus(contig: &str, start: u64, end: u64) -> String {
    format!("{contig}:{}-{end}", start.saturating_add(1))
}

fn ambiguous_gene_message(results: &TrackSearchResults, query: &str) -> String {
    let listed = results
        .hits
        .iter()
        .take(MAXIMUM_LISTED_CANDIDATES)
        .map(|hit| {
            format!(
                "{} ({}, {}, track {})",
                bounded_automation_text(annotation_search_hit_primary_label(hit), 64),
                one_based_locus(&hit.record.contig, hit.record.start, hit.record.end),
                bounded_automation_text(
                    hit.record.feature_type.as_deref().unwrap_or("feature"),
                    64
                ),
                hit.track_id.get()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let more = results.hits.len().saturating_sub(MAXIMUM_LISTED_CANDIDATES);
    let more = if more == 0 {
        String::new()
    } else {
        format!("; and {more} more")
    };
    format!(
        "{} annotation features match {}; none is a unique exact match: {listed}{more}",
        results.hits.len(),
        bounded_automation_text(query, 64)
    )
}

impl Viewer {
    /// The dictionary a locus typed without a panel is resolved in.
    fn automation_feature_dictionary(&self) -> Option<&ReferenceDictionary> {
        self.primary_alignment_dictionary()
            .or(self.reference_dictionary.as_ref())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_automation_feature_query(
        &mut self,
        command_index: usize,
        contig: &str,
        start: u64,
        end: u64,
        track_ids: Option<&[TrackId]>,
        offset: usize,
        limit: usize,
        now: Instant,
    ) -> AutomationExecution {
        let tracks = self
            .tracks
            .tracks()
            .iter()
            .map(|track| (track.id, track.kind))
            .collect::<Vec<_>>();
        let track_order = match select_feature_tracks(&tracks, track_ids) {
            Ok(selected) => selected,
            Err((code, message)) => return automation_failure(code, message),
        };
        let (interval, contig) = match resolve_feature_interval(
            self.automation_feature_dictionary(),
            &self.contig_aliases,
            contig,
            start,
            end,
        ) {
            Ok(resolved) => resolved,
            Err((code, message)) => return automation_failure(code, message),
        };
        let Some(engine) = &self.track_engine else {
            return automation_failure(
                AutomationErrorCode::ResourceNotFound,
                "no feature or gene-model track is loaded",
            );
        };
        let clock = feature_query_clock();
        let request = PanelGeneration::new(AUTOMATION_FEATURE_QUERY_PANEL, clock.advance());
        match engine.try_query_selected_with_settings(
            request,
            clock.clone(),
            contig.clone(),
            interval,
            track_order.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ) {
            Ok(()) => AutomationExecution::Wait(AutomationWait::TrackReply {
                command_index,
                deadline: now + AUTOMATION_TRACK_REPLY_TIMEOUT,
                pending: PendingTrackReply::Features(Arc::new(FeatureQueryWait {
                    request,
                    interval,
                    contig,
                    track_order,
                    offset,
                    limit,
                    reply: Mutex::default(),
                })),
            }),
            Err(EngineQueueError::Full) => automation_failure(
                AutomationErrorCode::InvalidState,
                "the track engine is busy; retry the feature query",
            ),
            Err(error @ EngineQueueError::Disconnected) => {
                automation_failure(AutomationErrorCode::InvalidState, error)
            }
        }
    }

    pub(super) fn start_automation_gene_search(
        &mut self,
        command_index: usize,
        query: &str,
        panel_id: Option<PanelId>,
        flank_bases: u64,
        now: Instant,
    ) -> AutomationExecution {
        // The focused panel is read once, here; the navigation is addressed to this ID even if
        // focus moves while the search runs.
        let panel_id = panel_id.unwrap_or_else(|| self.panels.active().id);
        if self.panels.get(panel_id).is_none() {
            return automation_failure(
                AutomationErrorCode::ResourceNotFound,
                format!("panel {} was not found", panel_id.get()),
            );
        }
        let query = query.trim();
        let Some(engine) = &self.track_engine else {
            return automation_failure(
                AutomationErrorCode::ResourceNotFound,
                "no searchable annotation tracks are open",
            );
        };
        // The engine answers only the newest search, so this one must be numbered after every
        // interactive search. It takes the next generation and the interactive counter moves
        // one past it: the interactive result handler, which accepts only the counter's current
        // value, can never take this result, and a later interactive search still supersedes
        // this one.
        let generation = self.annotation_search_generation.saturating_add(1);
        if let Err(error) =
            engine.try_search(generation, query.to_owned(), GENE_SEARCH_CANDIDATES, false)
        {
            return automation_failure(AutomationErrorCode::InvalidState, error);
        }
        self.annotation_search_generation = generation.saturating_add(1);
        AutomationExecution::Wait(AutomationWait::TrackReply {
            command_index,
            deadline: now + AUTOMATION_TRACK_REPLY_TIMEOUT,
            pending: PendingTrackReply::Gene(Arc::new(GeneSearchWait {
                generation,
                query: query.to_owned(),
                panel_id,
                flank_bases,
                results: Mutex::default(),
            })),
        })
    }

    /// Takes a track-engine event that answers an automation command out of the event stream.
    /// Returns the event when it is not one, for the ordinary panel and search handling.
    ///
    /// Every event carrying the automation feature-query identity is taken, including a late
    /// one from a query that has timed out: no panel owns it, and it must not be counted as a
    /// panel's stale result.
    pub(crate) fn route_automation_track_event(
        &mut self,
        event: TrackEngineEvent,
    ) -> Option<TrackEngineEvent> {
        let pending = self
            .automation
            .as_ref()
            .and_then(|runner| match &runner.waiting {
                Some(AutomationWait::TrackReply { pending, .. }) => Some(pending.clone()),
                _ => None,
            });
        match event {
            TrackEngineEvent::QueryCompleted { request, .. }
            | TrackEngineEvent::LaneCompleted { request, .. }
            | TrackEngineEvent::Error {
                request: Some(request),
                ..
            } if request.panel_id() == AUTOMATION_FEATURE_QUERY_PANEL => {
                if let Some(PendingTrackReply::Features(wait)) = pending {
                    wait.accept(event);
                }
                None
            }
            TrackEngineEvent::SearchCompleted {
                generation,
                mut results,
            } => match pending {
                Some(PendingTrackReply::Gene(wait)) if wait.generation == generation => {
                    // The same candidates the interactive search would show.
                    self.merge_landmark_search_results(&results.query.clone(), &mut results);
                    *wait.results.lock().unwrap_or_else(PoisonError::into_inner) = Some(results);
                    None
                }
                _ => Some(TrackEngineEvent::SearchCompleted {
                    generation,
                    results,
                }),
            },
            event => Some(event),
        }
    }

    /// Completes the waiting command once the engine has answered, or fails it at the deadline.
    pub(super) fn poll_automation_track_reply(
        &mut self,
        command_index: usize,
        deadline: Instant,
        pending: &PendingTrackReply,
        now: Instant,
    ) {
        let outcome = match pending {
            PendingTrackReply::Features(wait) => wait.outcome().map(|outcome| {
                outcome.and_then(|result| {
                    let message = format!(
                        "Read {} of {} features overlapping {}:{}-{} (zero-based, half-open)",
                        result.features.len(),
                        result.total_matching,
                        result.contig,
                        result.start,
                        result.end
                    );
                    serde_json::to_value(result)
                        .map(|data| (message, data))
                        .map_err(|error| (AutomationErrorCode::Internal, error.to_string()))
                })
            }),
            PendingTrackReply::Gene(wait) => wait
                .results
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .map(|results| self.finish_automation_gene_search(wait, &results)),
        };
        let outcome = match outcome {
            Some(outcome) => outcome,
            None if now >= deadline => {
                if matches!(pending, PendingTrackReply::Features(_)) {
                    // Abandon the engine's work; a late event is discarded by its generation.
                    feature_query_clock().advance();
                }
                Err((
                    AutomationErrorCode::Timeout,
                    "the annotation tracks did not answer before the automation timeout".to_owned(),
                ))
            }
            None => return,
        };
        self.finish_automation_command(command_index, outcome);
    }

    fn finish_automation_gene_search(
        &mut self,
        wait: &GeneSearchWait,
        results: &TrackSearchResults,
    ) -> Result<(String, serde_json::Value), (AutomationErrorCode, String)> {
        let hit = match gene_match(results, &wait.query) {
            GeneMatch::Preferred(hit) => hit,
            GeneMatch::NotFound => {
                return Err((
                    AutomationErrorCode::ResourceNotFound,
                    format!(
                        "no annotation feature matches {}",
                        bounded_automation_text(&wait.query, 64)
                    ),
                ));
            }
            GeneMatch::Ambiguous => {
                return Err((
                    AutomationErrorCode::AmbiguousMatch,
                    ambiguous_gene_message(results, &wait.query),
                ));
            }
        };
        let panel_id = wait.panel_id;
        if self.panels.get(panel_id).is_none() {
            return Err((
                AutomationErrorCode::ResourceNotFound,
                format!("panel {} was closed during the search", panel_id.get()),
            ));
        }
        let contig_length = self.automation_feature_dictionary().and_then(|dictionary| {
            resolve_contig_name(dictionary, &self.contig_aliases, &hit.record.contig)
                .ok()
                .flatten()
                .and_then(|id| dictionary.get(id))
                .map(rgv_core::Contig::length)
        });
        let (start, end) = padded_hit_interval(
            hit.record.start,
            hit.record.end,
            wait.flank_bases,
            contig_length,
        );
        let outcome = self
            .apply_action(crate::action::Action::GoToLocus {
                panel_id,
                locus: one_based_locus(&hit.record.contig, start, end),
            })
            .map_err(|error| (AutomationErrorCode::InvalidCommand, error.to_string()))?;
        self.bump_workspace_revision();
        let name = bounded_automation_text(
            annotation_search_hit_primary_label(hit),
            MAXIMUM_FEATURE_TEXT_BYTES,
        );
        let result = AutomationGeneNavigationResult {
            panel_id: panel_id.get(),
            contig: self.panels.panel(panel_id).contig_name.clone(),
            start: outcome.interval.map_or(start, GenomicInterval::start),
            end: outcome.interval.map_or(end, GenomicInterval::end),
            hit: AutomationGeneHit {
                track_id: hit.track_id.get(),
                name: name.clone(),
                feature_type: bounded_automation_text(
                    hit.record.feature_type.as_deref().unwrap_or("feature"),
                    MAXIMUM_FEATURE_TEXT_BYTES,
                ),
            },
            candidate_count: results.hits.len(),
        };
        let message = format!(
            "Navigated panel {} to {name} at {}",
            panel_id.get(),
            one_based_locus(&result.contig, result.start, result.end)
        );
        serde_json::to_value(result)
            .map(|data| (message, data))
            .map_err(|error| (AutomationErrorCode::Internal, error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::ViewportGeneration;
    use rgv_engine::{TrackEngine, TrackSpec, TrackSpecKind};

    fn feature(contig: &str, start: u64, end: u64, name: &str) -> FeatureRecord {
        FeatureRecord {
            contig: contig.to_owned(),
            start,
            end,
            name: (!name.is_empty()).then(|| name.to_owned()),
            feature_type: None,
            score: None,
            strand: Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        }
    }

    fn names(result: &AutomationFeatureQueryResult) -> Vec<(u64, &str)> {
        result
            .features
            .iter()
            .map(|feature| (feature.track_id, feature.name.as_str()))
            .collect()
    }

    #[test]
    fn features_are_filtered_to_the_half_open_interval_and_ordered_for_stable_paging() {
        let genes = [
            feature("chr1", 300, 400, "late"),
            feature("chr1", 100, 200, "beta"),
            feature("chr1", 100, 200, "alpha"),
            feature("chr1", 100, 150, "short"),
            // Ends exactly at the query start, and starts exactly at its end: neither overlaps.
            feature("chr1", 0, 100, "before"),
            feature("chr1", 500, 600, "after"),
            // A zero-length record occupies its anchor base.
            feature("chr1", 250, 250, "point"),
        ];
        let peaks = [feature("chr1", 120, 130, "peak")];
        let lanes: [(TrackId, &[FeatureRecord]); 2] =
            [(TrackId::new(9), &genes), (TrackId::new(2), &peaks)];

        let all = feature_query_page("chr1", 100, 500, &lanes, 0, 500);
        // Track order is the order given (workspace order), not track ID order.
        assert_eq!(
            names(&all),
            [
                (9, "short"),
                (9, "alpha"),
                (9, "beta"),
                (9, "point"),
                (9, "late"),
                (2, "peak"),
            ]
        );
        assert_eq!(all.total_matching, 6);
        assert!(!all.truncated);
        assert_eq!(all.track_ids, [9, 2]);
        assert_eq!((all.start, all.end), (100, 500));

        // Pages of the same query join into the unpaged answer, and only the last is complete.
        let mut joined = Vec::new();
        for offset in [0, 4] {
            let page = feature_query_page("chr1", 100, 500, &lanes, offset, 4);
            assert_eq!(page.total_matching, 6);
            assert_eq!(page.offset, offset);
            assert_eq!(page.truncated, offset == 0);
            joined.extend(page.features);
        }
        assert_eq!(joined, all.features);

        let beyond = feature_query_page("chr1", 100, 500, &lanes, 6, 4);
        assert!(beyond.features.is_empty());
        assert_eq!(beyond.total_matching, 6);
        assert!(!beyond.truncated);

        let empty = feature_query_page("chr1", 700, 800, &lanes, 0, 4);
        assert_eq!(empty.total_matching, 0);
        assert!(!empty.truncated);
    }

    #[test]
    fn feature_fields_carry_strand_type_gene_symbol_and_bounded_text() {
        let mut gene = feature("chr1", 10, 20, "ENST0001");
        gene.strand = Strand::Reverse;
        gene.feature_type = Some("transcript".to_owned());
        gene.attributes = vec![("gene_name".to_owned(), "TP53".to_owned())];
        let unnamed = feature("chr1", 30, 40, "");
        let mut long = feature("chr1", 50, 60, &"é".repeat(400));
        long.strand = Strand::Forward;
        let records = [gene, unnamed, long];
        let lanes: [(TrackId, &[FeatureRecord]); 1] = [(TrackId::new(1), &records)];

        let page = feature_query_page("chr1", 0, 100, &lanes, 0, 10);
        let first = &page.features[0];
        // The gene symbol is the display label; the record name stays reachable by search.
        assert_eq!(first.name, "TP53");
        assert_eq!(first.gene_name.as_deref(), Some("TP53"));
        assert_eq!(first.feature_type, "transcript");
        assert_eq!(first.strand, "-");
        assert_eq!((first.start, first.end), (10, 20));
        let second = &page.features[1];
        assert_eq!(second.name, "");
        assert_eq!(second.gene_name, None);
        assert_eq!(second.feature_type, "feature");
        assert_eq!(second.strand, ".");
        let third = &page.features[2];
        assert_eq!(third.strand, "+");
        assert!(third.name.len() <= MAXIMUM_FEATURE_TEXT_BYTES + '…'.len_utf8());
        assert!(third.name.ends_with('…'));
    }

    #[test]
    fn only_feature_and_gene_model_tracks_are_read_in_workspace_order() {
        let tracks = [
            (TrackId::new(5), TrackKind::GeneModel),
            (TrackId::new(1), TrackKind::Variant),
            (TrackId::new(3), TrackKind::Feature),
            (TrackId::new(4), TrackKind::Quantitative),
        ];
        assert_eq!(
            select_feature_tracks(&tracks, None).unwrap(),
            [TrackId::new(5), TrackId::new(3)]
        );
        // The requested order and duplicates do not change the answer's track order.
        assert_eq!(
            select_feature_tracks(
                &tracks,
                Some(&[TrackId::new(3), TrackId::new(5), TrackId::new(3)])
            )
            .unwrap(),
            [TrackId::new(5), TrackId::new(3)]
        );
        assert_eq!(
            select_feature_tracks(&tracks, Some(&[TrackId::new(3)])).unwrap(),
            [TrackId::new(3)]
        );
        assert_eq!(
            select_feature_tracks(&tracks, Some(&[TrackId::new(8)]))
                .unwrap_err()
                .0,
            AutomationErrorCode::ResourceNotFound
        );
        assert_eq!(
            select_feature_tracks(&tracks, Some(&[TrackId::new(3), TrackId::new(1)]))
                .unwrap_err()
                .0,
            AutomationErrorCode::InvalidCommand
        );
        assert_eq!(
            select_feature_tracks(&tracks[1..2], None).unwrap_err().0,
            AutomationErrorCode::ResourceNotFound
        );
        assert_eq!(
            select_feature_tracks(&[], None).unwrap_err().0,
            AutomationErrorCode::ResourceNotFound
        );
    }

    #[test]
    fn the_queried_interval_is_named_and_clamped_by_the_dictionary_when_there_is_one() {
        let dictionary = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 1_000),
            ("chr2".to_owned(), 500),
        ]);
        let aliases = ContigAliases::default();
        let (interval, contig) =
            resolve_feature_interval(Some(&dictionary), &aliases, "chr2", 100, 200).unwrap();
        assert_eq!(contig, "chr2");
        assert_eq!((interval.start(), interval.end()), (100, 200));
        assert_eq!(interval.contig(), dictionary.id_by_name("chr2").unwrap());

        // The `chr`-less spelling a locus accepts resolves to the workspace's own name.
        let (interval, contig) =
            resolve_feature_interval(Some(&dictionary), &aliases, "2", 400, 9_000).unwrap();
        assert_eq!(contig, "chr2");
        assert_eq!((interval.start(), interval.end()), (400, 500));

        assert_eq!(
            resolve_feature_interval(Some(&dictionary), &aliases, "chr9", 0, 10)
                .unwrap_err()
                .0,
            AutomationErrorCode::ResourceNotFound
        );
        assert_eq!(
            resolve_feature_interval(Some(&dictionary), &aliases, "chr2", 500, 600)
                .unwrap_err()
                .0,
            AutomationErrorCode::InvalidCommand
        );

        // Without a dictionary the contig is taken as given and nothing is clamped.
        let (interval, contig) =
            resolve_feature_interval(None, &aliases, "scaffold_7", 400, 9_000).unwrap();
        assert_eq!(contig, "scaffold_7");
        assert_eq!((interval.start(), interval.end()), (400, 9_000));
    }

    fn hit(track: u64, matched_term: &str, name: &str, start: u64, end: u64) -> TrackSearchHit {
        TrackSearchHit {
            track_id: TrackId::new(track),
            track_name: "genes".to_owned(),
            matched_term: matched_term.to_owned(),
            record: feature("chr17", start, end, name),
        }
    }

    fn results(hits: Vec<TrackSearchHit>) -> TrackSearchResults {
        TrackSearchResults {
            query: "brca1".to_owned(),
            hits,
            ..TrackSearchResults::default()
        }
    }

    #[test]
    fn a_gene_is_navigated_only_when_the_interactive_search_would_prefer_one_hit() {
        assert_eq!(
            gene_match(&results(Vec::new()), "BRCA1"),
            GeneMatch::NotFound
        );

        // The only hit is taken even when it is a prefix match.
        let only = results(vec![hit(3, "brca1-as1", "BRCA1-AS1", 10, 20)]);
        assert_eq!(
            gene_match(&only, "BRCA1"),
            GeneMatch::Preferred(&only.hits[0])
        );

        // One exact match wins over prefix matches; the query's case and padding are ignored.
        let exact = results(vec![
            hit(3, "brca1-as1", "BRCA1-AS1", 10, 20),
            hit(3, "brca1", "BRCA1", 100, 200),
        ]);
        assert_eq!(
            gene_match(&exact, "  Brca1 "),
            GeneMatch::Preferred(&exact.hits[1])
        );

        // Two exact matches (the gene in two tracks), or several prefixes, are not guessed.
        let twice = results(vec![
            hit(3, "brca1", "BRCA1", 100, 200),
            hit(4, "brca1", "BRCA1", 100, 200),
        ]);
        assert_eq!(gene_match(&twice, "BRCA1"), GeneMatch::Ambiguous);
        let prefixes = results(vec![
            hit(3, "brca1-as1", "BRCA1-AS1", 10, 20),
            hit(3, "brca1p1", "BRCA1P1", 30, 40),
        ]);
        assert_eq!(gene_match(&prefixes, "BRCA1"), GeneMatch::Ambiguous);

        let message = ambiguous_gene_message(&twice, "BRCA1");
        assert!(
            message.starts_with("2 annotation features match BRCA1"),
            "{message}"
        );
        // Candidates are listed as one-based inclusive loci, like every locus string.
        assert!(
            message.contains("BRCA1 (chr17:101-200, feature, track 3)"),
            "{message}"
        );
        assert!(message.contains("track 4"), "{message}");
        let many = results(
            (0..8)
                .map(|index| hit(3, "brca1x", "BRCA1X", index * 10, index * 10 + 5))
                .collect(),
        );
        assert!(ambiguous_gene_message(&many, "BRCA1").ends_with("; and 3 more"));
    }

    #[test]
    fn the_flank_pads_both_sides_and_is_clamped_to_the_contig() {
        assert_eq!(padded_hit_interval(100, 200, 0, Some(1_000)), (100, 200));
        assert_eq!(padded_hit_interval(100, 200, 50, Some(1_000)), (50, 250));
        // Clamped at the contig start and at its length.
        assert_eq!(padded_hit_interval(100, 200, 150, Some(1_000)), (0, 350));
        assert_eq!(padded_hit_interval(900, 990, 50, Some(1_000)), (850, 1_000));
        assert_eq!(padded_hit_interval(10, 20, 5_000, Some(1_000)), (0, 1_000));
        // An unknown length leaves the end to the locus resolver; nothing overflows.
        assert_eq!(padded_hit_interval(100, 200, 50, None), (50, 250));
        assert_eq!(
            padded_hit_interval(u64::MAX - 10, u64::MAX - 5, 100, None),
            (u64::MAX - 110, u64::MAX)
        );
        // A zero-length feature still yields a one-base interval, inside the contig.
        assert_eq!(padded_hit_interval(40, 40, 0, Some(1_000)), (40, 41));
        assert_eq!(
            padded_hit_interval(999, 1_500, 0, Some(1_000)),
            (999, 1_000)
        );
        assert_eq!(one_based_locus("chr1", 50, 250), "chr1:51-250");
    }

    fn feature_wait(generation: u64, track_order: &[u64]) -> FeatureQueryWait {
        FeatureQueryWait {
            request: PanelGeneration::new(
                AUTOMATION_FEATURE_QUERY_PANEL,
                ViewportGeneration::new(generation),
            ),
            interval: GenomicInterval::new(ContigId::new(0), 100, 500).unwrap(),
            contig: "chr1".to_owned(),
            track_order: track_order.iter().copied().map(TrackId::new).collect(),
            offset: 0,
            limit: 10,
            reply: Mutex::default(),
        }
    }

    fn lane(wait: &FeatureQueryWait, track: u64, records: Vec<FeatureRecord>) -> TrackEngineEvent {
        TrackEngineEvent::LaneCompleted {
            request: wait.request,
            interval: wait.interval,
            track: TrackSnapshot {
                id: TrackId::new(track),
                name: format!("track {track}"),
                data: TrackSnapshotData::Features(records),
            },
        }
    }

    fn settled(wait: &FeatureQueryWait, completed: &[u64]) -> TrackEngineEvent {
        TrackEngineEvent::QueryCompleted {
            request: wait.request,
            interval: wait.interval,
            completed: completed.iter().copied().map(TrackId::new).collect(),
        }
    }

    #[test]
    fn a_feature_query_completes_only_from_its_own_settled_lanes() {
        let wait = feature_wait(7, &[5, 3]);
        assert!(wait.outcome().is_none());

        // Another query's events (an earlier generation, or another interval) are not taken.
        let stale = feature_wait(6, &[5, 3]);
        assert!(!wait.accept(lane(&stale, 5, vec![feature("chr1", 100, 200, "stale")])));
        assert!(!wait.accept(settled(&stale, &[5])));
        assert!(!wait.accept(TrackEngineEvent::QueryCompleted {
            request: wait.request,
            interval: GenomicInterval::new(ContigId::new(0), 0, 50).unwrap(),
            completed: Vec::new(),
        }));
        assert!(!wait.accept(TrackEngineEvent::Ready {
            opened: 1,
            failed: 0
        }));
        assert!(wait.outcome().is_none());

        // Lanes arrive in any order; the answer waits for the settlement and every lane in it.
        assert!(wait.accept(lane(&wait, 3, vec![feature("chr1", 150, 160, "peak")])));
        assert!(wait.outcome().is_none());
        assert!(wait.accept(settled(&wait, &[5, 3])));
        assert!(wait.outcome().is_none());
        assert!(wait.accept(lane(&wait, 5, vec![feature("chr1", 100, 200, "gene")])));
        let result = wait.outcome().unwrap().unwrap();
        assert_eq!(names(&result), [(5, "gene"), (3, "peak")]);
        assert_eq!(result.track_ids, [5, 3]);
        assert_eq!(
            (result.contig.as_str(), result.start, result.end),
            ("chr1", 100, 500)
        );
    }

    #[test]
    fn a_failed_or_unopened_feature_source_fails_the_query_with_a_typed_error() {
        let dense = feature_wait(1, &[5, 3]);
        assert!(dense.accept(TrackEngineEvent::Error {
            request: Some(dense.request),
            track: Some(TrackId::new(5)),
            message: "feature query exceeds the 250000-record limit".to_owned(),
        }));
        assert!(dense.accept(lane(&dense, 3, Vec::new())));
        assert!(dense.accept(settled(&dense, &[3])));
        let (code, message) = dense.outcome().unwrap().unwrap_err();
        assert_eq!(code, AutomationErrorCode::TooDense);
        assert!(
            message.starts_with("feature track 5 could not answer"),
            "{message}"
        );

        let unreadable = feature_wait(2, &[5]);
        assert!(unreadable.accept(TrackEngineEvent::Error {
            request: Some(unreadable.request),
            track: Some(TrackId::new(5)),
            message: "unexpected end of file".to_owned(),
        }));
        assert!(unreadable.accept(settled(&unreadable, &[])));
        assert_eq!(
            unreadable.outcome().unwrap().unwrap_err().0,
            AutomationErrorCode::Io
        );

        // A track the engine has not opened answers nothing; that is not an empty result.
        let unopened = feature_wait(3, &[5]);
        assert!(unopened.accept(settled(&unopened, &[])));
        assert_eq!(
            unopened.outcome().unwrap().unwrap_err().0,
            AutomationErrorCode::ResourceNotFound
        );

        // A track that answered with no overlapping records is an empty, successful result.
        let empty = feature_wait(4, &[5]);
        assert!(empty.accept(lane(&empty, 5, Vec::new())));
        assert!(empty.accept(settled(&empty, &[5])));
        let result = empty.outcome().unwrap().unwrap();
        assert_eq!(
            (result.total_matching, result.track_ids.as_slice()),
            (0, &[5][..])
        );
    }

    /// The route the command depends on: the engine answers a query that belongs to no panel,
    /// for an interval chosen by the caller, restricted to the requested tracks.
    #[test]
    fn the_track_engine_answers_a_query_that_belongs_to_no_panel() {
        let directory = tempfile::tempdir().unwrap();
        let genes = directory.path().join("genes.bed");
        let peaks = directory.path().join("peaks.bed");
        std::fs::write(
            &genes,
            "chr1\t100\t200\tGENE_A\t0\t+\nchr1\t5000\t5100\tGENE_B\t0\t-\nchr2\t100\t200\tGENE_C\t0\t+\n",
        )
        .unwrap();
        std::fs::write(&peaks, "chr1\t150\t160\tPEAK_1\nchr1\t5050\t5060\tPEAK_2\n").unwrap();
        let spec = |id, path: &std::path::Path| TrackSpec {
            id: TrackId::new(id),
            name: format!("track {id}"),
            path: path.to_owned(),
            kind: TrackSpecKind::Bed,
        };
        let engine = TrackEngine::spawn(vec![spec(1, &genes), spec(2, &peaks)], 16);

        let clock = GenerationClock::default();
        let wait = FeatureQueryWait {
            request: PanelGeneration::new(AUTOMATION_FEATURE_QUERY_PANEL, clock.advance()),
            interval: GenomicInterval::new(ContigId::new(0), 4_000, 6_000).unwrap(),
            contig: "chr1".to_owned(),
            track_order: vec![TrackId::new(2), TrackId::new(1)],
            offset: 0,
            limit: 10,
            reply: Mutex::default(),
        };
        engine
            .try_query_selected_with_settings(
                wait.request,
                clock,
                wait.contig.clone(),
                wait.interval,
                wait.track_order.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let result = loop {
            while let Some(event) = engine.try_receive().unwrap() {
                wait.accept(event);
            }
            if let Some(outcome) = wait.outcome() {
                break outcome.unwrap();
            }
            assert!(Instant::now() < deadline, "the track engine did not answer");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(names(&result), [(2, "PEAK_2"), (1, "GENE_B")]);
        assert_eq!(result.features[1].strand, "-");
        assert_eq!(
            (result.features[1].start, result.features[1].end),
            (5_000, 5_100)
        );
        assert_eq!(result.total_matching, 2);
        assert!(!result.truncated);
    }
}
