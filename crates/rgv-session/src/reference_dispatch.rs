//! Asking the reference engine for each panel's bases, and handing fetched slices to the
//! alignment scene lanes that wait for them.
//!
//! A panel asks for the buffered visible interval (for base marks) or, when its alignment lanes
//! need FASTA authority for a wider horizon, for one bounded union of those horizons. A slice
//! already resident is installed into every live lane it satisfies without new I/O.

use crate::alignment::settlement::{
    AlignmentReferenceLanePlan, AlignmentReferenceLaneRequirement,
    alignment_scene_event_token_is_current, plan_alignment_reference_lanes,
};
use crate::alignment::track::{
    AlignmentReferenceCompatibility, AlignmentTrackLifecycle, alignment_reference_compatibility,
    compatible_reference_contig_name,
};
use crate::panels::Panel;
use crate::session::Session;
use rgv_alignment_scene::ReferenceRevision;
use rgv_core::{
    GenomicInterval, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId, ReferenceDictionary, Viewport,
};
use rgv_engine::{
    AlignmentReferenceAuthority, AlignmentReferenceRequirement, EngineQueueError,
    MAX_REFERENCE_QUERY_BASES, ReferenceQueryIdentity,
};
use rgv_hts::ReferenceSlice;
use std::sync::Arc;

/// The visible interval widened by the browsing buffer on each side: 100% means one visible
/// viewport on each side, as alignment scenes use. The optional cap bounds the interval for
/// sources such as FASTA while keeping the visible part.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn buffered_context_interval(
    viewport: Viewport,
    contig_length: u64,
    buffer_percent: u16,
    maximum_bases: Option<u64>,
) -> GenomicInterval {
    let visible = viewport.interval();
    let side = ((viewport.span() * f64::from(buffer_percent) / 100.0).ceil() as u64).max(1);
    let start = visible.start().saturating_sub(side);
    let end = visible.end().saturating_add(side).min(contig_length.max(1));
    let mut interval = GenomicInterval::new(
        visible.contig(),
        start.min(contig_length.saturating_sub(1)),
        end.max(start.saturating_add(1)).min(contig_length.max(1)),
    )
    .expect("a buffered viewport remains non-empty");
    if let Some(maximum) = maximum_bases.filter(|maximum| *maximum > 0)
        && interval.len() > maximum
        && maximum >= visible.len()
    {
        let spare = maximum - visible.len();
        let before = spare / 2;
        let after = spare - before;
        let mut bounded_start = visible.start().saturating_sub(before);
        let mut bounded_end = visible.end().saturating_add(after).min(contig_length);
        if bounded_end - bounded_start < maximum {
            bounded_start = bounded_end.saturating_sub(maximum);
            bounded_end = bounded_start.saturating_add(maximum).min(contig_length);
        }
        interval = GenomicInterval::new(visible.contig(), bounded_start, bounded_end)
            .expect("a capped buffered viewport remains non-empty");
    }
    interval
}

impl<P: Panel> Session<P> {
    /// The dictionary of the first alignment source, once it has opened.
    pub fn primary_alignment_dictionary(&self) -> Option<&ReferenceDictionary> {
        self.alignment_tracks
            .first()
            .and_then(|track| track.dictionary.as_ref())
    }

    /// The open FASTA's revision, when its dictionary is compatible with `alignment`.
    pub fn compatible_alignment_reference_revision(
        &self,
        alignment: &ReferenceDictionary,
    ) -> Option<ReferenceRevision> {
        self.reference_revision.filter(|_| {
            self.reference_dictionary.as_ref().is_some_and(|reference| {
                alignment_reference_compatibility(alignment, reference, &self.contig_aliases)
                    == AlignmentReferenceCompatibility::Compatible
            })
        })
    }

    /// Resolves the panel's alignment spelling to the actual FASTA contig name. A loaded but
    /// incompatible FASTA is an explicit unavailable capability, never a query destined to fail
    /// later inside the reference worker.
    pub fn compatible_reference_contig_for_panel(
        &self,
        panel_id: PanelId,
    ) -> Result<Option<String>, String> {
        let (Some(_revision), Some(reference), Some(panel)) = (
            self.reference_revision,
            self.reference_dictionary.as_ref(),
            self.panels.get(panel_id),
        ) else {
            return Ok(None);
        };
        let panel = panel.state();
        let Some(alignment) = self.primary_alignment_dictionary() else {
            return Ok(Some(panel.contig_name.clone()));
        };
        compatible_reference_contig_name(
            alignment,
            reference,
            &self.contig_aliases,
            &panel.contig_name,
        )
        .map(Some)
    }

    /// Marks the panel's reference as wanted when base marks are visible and a reference is
    /// open, and dispatches it. Returns whether a failure was reported (see
    /// [`Self::dispatch_reference_query`]).
    pub fn request_reference_query(&mut self, panel_id: PanelId) -> bool {
        let panel = self.panels.panel_mut(panel_id).state_mut();
        let base_marks_visible =
            panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        panel.pending_queries.reference = self.reference_engine.is_some() && base_marks_visible;
        self.dispatch_reference_query(panel_id)
    }

    /// Returns every live current lane which still requires the currently open FASTA revision.
    /// A terminal dependency failure settles the projection generation and therefore excludes
    /// it until the next navigation advances that generation and retries.
    pub fn current_alignment_reference_requirements(
        &self,
        panel_id: PanelId,
        revision: ReferenceRevision,
    ) -> Vec<AlignmentReferenceLaneRequirement> {
        if self.panels.get(panel_id).is_some_and(|panel| {
            let panel = panel.state();
            panel.reference_query_error_generation == Some(panel.generation_clock.current().get())
        }) {
            return Vec::new();
        }
        self.alignment_tracks
            .iter()
            .filter(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.scene_engine.is_some()
            })
            .filter_map(|track| {
                let projection = track.projection(panel_id)?;
                let owner = projection.scene_owner?;
                let frame = projection.scene_frame.as_ref()?;
                if projection.navigation.desired_camera() != Some(frame.camera)
                    || !alignment_scene_event_token_is_current(
                        projection.navigation.request_token(),
                        projection.scene_frame_request_token,
                    )
                {
                    return None;
                }
                match frame.reference_requirement {
                    AlignmentReferenceRequirement::Pending {
                        revision: required_revision,
                        horizon,
                    } if required_revision == revision => Some(AlignmentReferenceLaneRequirement {
                        owner,
                        revision,
                        horizon,
                    }),
                    AlignmentReferenceRequirement::NotRequired
                    | AlignmentReferenceRequirement::Pending { .. }
                    | AlignmentReferenceRequirement::Ready { .. } => None,
                }
            })
            .collect()
    }

    /// Queues an already-fetched immutable FASTA slice into every current scene owner whose
    /// pending requirement it satisfies. Success means every live current owner is either
    /// already satisfied or has accepted its exact authority into the bounded engine control
    /// queue.
    pub fn install_reference_authority_for_panel(
        &mut self,
        panel_id: PanelId,
        revision: ReferenceRevision,
        slice: &Arc<ReferenceSlice>,
    ) -> bool {
        let authority = AlignmentReferenceAuthority::new(revision, Arc::clone(slice));
        let requirements = self.current_alignment_reference_requirements(panel_id, revision);
        let mut queued = self
            .panels
            .get_mut(panel_id)
            .map(|panel| std::mem::take(&mut panel.state_mut().reference_installs_queued))
            .unwrap_or_default();
        queued.retain(|owner, queued_requirement| {
            requirements.iter().any(|requirement| {
                requirement.owner == *owner
                    && *queued_requirement == (requirement.revision, requirement.horizon)
            })
        });
        for requirement in &requirements {
            if queued.get(&requirement.owner) == Some(&(requirement.revision, requirement.horizon))
                || !slice.interval.contains_interval(requirement.horizon)
            {
                continue;
            }
            let Some(track_index) = self.alignment_tracks.iter().position(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.scene_owner == Some(requirement.owner))
            }) else {
                continue;
            };
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| {
                    engine.try_install_reference_authority(requirement.owner, authority.clone())
                });
            match result {
                Some(Ok(())) => {
                    queued.insert(
                        requirement.owner,
                        (requirement.revision, requirement.horizon),
                    );
                }
                Some(Err(EngineQueueError::Full)) | None => {}
                Some(Err(EngineQueueError::Disconnected)) => {
                    // This lane is terminal and will no longer participate in the panel's
                    // reference settlement. Do not leave the otherwise-successful panel
                    // retrying an engine which has already disconnected.
                    self.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        let live = self.current_alignment_reference_requirements(panel_id, revision);
        queued.retain(|owner, queued_requirement| {
            live.iter().any(|requirement| {
                requirement.owner == *owner
                    && *queued_requirement == (requirement.revision, requirement.horizon)
            })
        });
        let all_queued = matches!(
            plan_alignment_reference_lanes(&live, &queued),
            AlignmentReferenceLanePlan::AllQueued
        );
        if let Some(panel) = self.panels.get_mut(panel_id) {
            panel.state_mut().reference_installs_queued = queued;
        }
        all_queued
    }

    /// Sends the panel's pending reference request, if it has one: installs a resident slice
    /// into waiting lanes, settles the panel when the slice already covers what it needs, and
    /// otherwise queries the reference engine for the bounded interval its lanes need.
    /// Returns whether a failure was reported, so a windowed host can redraw its status line.
    #[allow(clippy::too_many_lines)]
    pub fn dispatch_reference_query(&mut self, panel_id: PanelId) -> bool {
        if !self
            .panels
            .panel(panel_id)
            .state()
            .pending_queries
            .reference
        {
            return false;
        }
        let Some(revision) = self.reference_revision else {
            self.panels
                .panel_mut(panel_id)
                .state_mut()
                .pending_queries
                .reference = false;
            return false;
        };
        let reference_contig_name = match self.compatible_reference_contig_for_panel(panel_id) {
            Ok(Some(name)) => name,
            Ok(None) => {
                self.panels
                    .panel_mut(panel_id)
                    .state_mut()
                    .pending_queries
                    .reference = false;
                return false;
            }
            Err(reason) => {
                let panel = self.panels.panel_mut(panel_id).state_mut();
                panel.pending_queries.reference = false;
                panel.reference_slice = None;
                panel.reference_slice_revision = None;
                panel.reference_slice_generation = None;
                panel.reference_request_revision = None;
                panel.reference_request_interval = None;
                panel.reference_installs_queued.clear();
                panel.reference_settled_generation = Some(panel.generation_clock.current().get());
                panel.reference_query_error_generation =
                    Some(panel.generation_clock.current().get());
                self.data_status = format!(
                    "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                );
                return false;
            }
        };
        let cached = self.panels.get(panel_id).and_then(|panel| {
            let panel = panel.state();
            (panel.reference_slice_revision == Some(revision))
                .then(|| panel.reference_slice.as_ref())
                .flatten()
                .cloned()
        });
        if let Some(slice) = &cached {
            self.install_reference_authority_for_panel(panel_id, revision, slice);
        }
        let requirements = self.current_alignment_reference_requirements(panel_id, revision);
        let lane_plan =
            self.panels
                .get(panel_id)
                .map_or(AlignmentReferenceLanePlan::AllQueued, |panel| {
                    plan_alignment_reference_lanes(
                        &requirements,
                        &panel.state().reference_installs_queued,
                    )
                });
        let contextual_interval = {
            let panel = self.panels.panel(panel_id).state();
            buffered_context_interval(
                panel.viewport,
                panel.contig_length,
                self.settings.read_browsing_buffer_percent,
                Some(MAX_REFERENCE_QUERY_BASES),
            )
        };
        let cached_covers_context = cached
            .as_ref()
            .is_some_and(|slice| slice.interval.contains_interval(contextual_interval));
        if matches!(lane_plan, AlignmentReferenceLanePlan::AllQueued) && cached_covers_context {
            let panel = self.panels.panel_mut(panel_id).state_mut();
            panel.pending_queries.reference = false;
            let generation = panel.generation_clock.current().get();
            panel.reference_slice_generation = Some(generation);
            panel.reference_settled_generation = Some(generation);
            panel.reference_query_error_generation = None;
            return false;
        }
        let (revision, interval) = match lane_plan {
            AlignmentReferenceLanePlan::Query { revision, interval } => (revision, interval),
            AlignmentReferenceLanePlan::AllQueued => (revision, contextual_interval),
        };
        if cached
            .as_ref()
            .is_some_and(|slice| slice.interval.contains_interval(interval))
        {
            // The bases are already resident, so an unqueued owner can only be waiting for
            // bounded control-queue capacity. Keep the dispatch flag set and retry without
            // duplicate I/O.
            self.panels
                .panel_mut(panel_id)
                .state_mut()
                .pending_queries
                .reference = true;
            return false;
        }
        let already_requested = self.panels.get(panel_id).is_some_and(|panel| {
            let panel = panel.state();
            panel.reference_request_revision == Some(revision)
                && panel.reference_request_interval == Some(interval)
        });
        if already_requested {
            self.panels
                .panel_mut(panel_id)
                .state_mut()
                .pending_queries
                .reference = false;
            return false;
        }
        let Some(engine) = &self.reference_engine else {
            self.panels
                .panel_mut(panel_id)
                .state_mut()
                .pending_queries
                .reference = false;
            return false;
        };
        let panel = self.panels.panel(panel_id).state();
        let query = ReferenceQueryIdentity::new(panel.request(), revision, interval);
        match engine.try_query(query, panel.generation_clock.clone(), reference_contig_name) {
            Ok(()) => {
                let panel = self.panels.panel_mut(panel_id).state_mut();
                panel.reference_request_revision = Some(revision);
                panel.reference_request_interval = Some(interval);
                panel.pending_queries.reference = false;
                false
            }
            Err(EngineQueueError::Full) => {
                self.set_busy_status("Reference queue busy; retrying the latest locus…");
                false
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels
                    .panel_mut(panel_id)
                    .state_mut()
                    .pending_queries
                    .reference = false;
                self.report_error(EngineQueueError::Disconnected.to_string());
                true
            }
        }
    }
}
