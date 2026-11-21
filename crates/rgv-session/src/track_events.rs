//! Routing track-engine answers into the panels that asked for them.
//!
//! A lane result is accepted only by the panel generation and buffered interval that requested
//! it; a refreshed lane keeps each auxiliary selection on its record. Events that are not
//! answers to a panel query (engine readiness, track membership, searches, TDF catalogues) are
//! handed back to the host unchanged.

use crate::notifications::NotificationSeverity;
use crate::panels::Panel;
use crate::selection::{AuxiliarySelections, reresolve_auxiliary_selections};
use crate::session::Session;
use rgv_core::PanelId;
use rgv_engine::TrackEngineEvent;

/// What routing one track-engine event did, and what the host still owes for it.
#[derive(Debug)]
pub enum TrackEventRoute {
    /// A panel's lane state changed: its display is stale.
    PanelChanged { panel_id: PanelId },
    /// The answer was for a superseded generation or interval and changed nothing.
    Discarded,
    /// A failure was logged (and shown on the status line when it concerns the focused panel
    /// or no panel). `display_stale` when an accepted panel's lane now shows the failure.
    ErrorReported { display_stale: bool },
    /// Not an answer to a panel query: the host handles it.
    Unrouted(TrackEngineEvent),
}

impl<P: Panel> Session<P> {
    /// Applies one track-engine event to the panels. Counts answers for superseded panel
    /// generations in `navigation_stale_result_rejections`.
    #[allow(clippy::too_many_lines)]
    pub fn route_track_event(&mut self, event: TrackEngineEvent) -> TrackEventRoute {
        let stale_query_result = match &event {
            TrackEngineEvent::QueryCompleted { request, .. }
            | TrackEngineEvent::LaneCompleted { request, .. }
            | TrackEngineEvent::Error {
                request: Some(request),
                ..
            } => self
                .panels
                .get(request.panel_id())
                .is_none_or(|panel| !panel.state().accepts(*request)),
            _ => false,
        };
        if stale_query_result {
            self.navigation_stale_result_rejections =
                self.navigation_stale_result_rejections.saturating_add(1);
        }
        match event {
            TrackEngineEvent::LaneCompleted {
                request,
                interval,
                track,
            } => {
                let Some(panel) = self.panels.get_mut(request.panel_id()) else {
                    return TrackEventRoute::Discarded;
                };
                let panel = panel.state_mut();
                if !(panel.accepts(request) && panel.track_request_interval == Some(interval)) {
                    return TrackEventRoute::Discarded;
                }
                let track_id = track.id;
                panel.track_query_error_generations.remove(&track.id);
                panel.track_snapshot_intervals.insert(track.id, interval);
                panel
                    .track_snapshot_generations
                    .insert(track.id, request.generation().get());
                // Selections index into the lane's result. Re-resolve them against the
                // refreshed result by record identity instead of clearing them.
                let position = panel
                    .track_snapshots
                    .iter()
                    .position(|snapshot| snapshot.id == track_id);
                let (previous, position) = if let Some(position) = position {
                    (
                        Some(std::mem::replace(
                            &mut panel.track_snapshots[position],
                            track,
                        )),
                        position,
                    )
                } else {
                    panel.track_snapshots.push(track);
                    (None, panel.track_snapshots.len() - 1)
                };
                let selections = reresolve_auxiliary_selections(
                    AuxiliarySelections {
                        feature: panel.selected_feature,
                        variant: panel.selected_variant,
                        gwas: panel.selected_gwas,
                        quantitative: panel.selected_quantitative,
                        copy_number: panel.selected_copy_number,
                        mutation: panel.selected_mutation,
                        interaction: panel.selected_interaction,
                    },
                    previous.as_ref(),
                    &panel.track_snapshots[position],
                );
                panel.selected_feature = selections.feature;
                panel.selected_variant = selections.variant;
                panel.selected_gwas = selections.gwas;
                panel.selected_quantitative = selections.quantitative;
                panel.selected_copy_number = selections.copy_number;
                panel.selected_mutation = selections.mutation;
                panel.selected_interaction = selections.interaction;
                TrackEventRoute::PanelChanged {
                    panel_id: request.panel_id(),
                }
            }
            TrackEngineEvent::QueryCompleted {
                request,
                interval,
                completed: _,
            } => {
                let Some(panel) = self.panels.get_mut(request.panel_id()) else {
                    return TrackEventRoute::Discarded;
                };
                let panel = panel.state_mut();
                if !(panel.accepts(request) && panel.track_request_interval == Some(interval)) {
                    return TrackEventRoute::Discarded;
                }
                panel.track_request_interval = None;
                panel.track_settled_generation = Some(request.generation().get());
                TrackEventRoute::PanelChanged {
                    panel_id: request.panel_id(),
                }
            }
            TrackEngineEvent::Error {
                request: Some(request),
                track,
                message,
            } => {
                let is_focused = request.panel_id() == self.panels.active().state().id;
                let Some(panel) = self
                    .panels
                    .get_mut(request.panel_id())
                    .filter(|panel| panel.state().accepts(request))
                else {
                    return TrackEventRoute::Discarded;
                };
                if let Some(track_id) = track {
                    panel
                        .state_mut()
                        .track_query_error_generations
                        .insert(track_id, request.generation().get());
                }
                // Logged for every panel; only the focused panel's failure also takes the
                // status line.
                self.notifications
                    .push_reported(NotificationSeverity::Error, track, &message);
                if is_focused {
                    self.data_status = message;
                }
                TrackEventRoute::ErrorReported {
                    display_stale: true,
                }
            }
            TrackEngineEvent::Error {
                request: None,
                track,
                message,
            } => {
                self.notifications
                    .push_reported(NotificationSeverity::Error, track, &message);
                self.data_status = message;
                TrackEventRoute::ErrorReported {
                    display_stale: false,
                }
            }
            other => TrackEventRoute::Unrouted(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::PanelState;
    use crate::selection::FeatureSelection;
    use rgv_core::{
        ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, ReferenceDictionary, TrackId,
        Viewport,
    };
    use rgv_engine::{TrackSnapshot, TrackSnapshotData};
    use rgv_hts::{FeatureRecord, Strand};
    use std::path::PathBuf;

    fn session() -> Session {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let panel = PanelState::new(
            PanelId::new(1),
            Viewport::new(interval, 600.0).unwrap(),
            None,
            "chr1".to_owned(),
            1_000_000,
        );
        Session::new(
            panel,
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            PathBuf::from("settings.json"),
        )
    }

    fn feature(start: u64, name: &str) -> FeatureRecord {
        FeatureRecord {
            contig: "chr1".to_owned(),
            start,
            end: start + 10,
            name: Some(name.to_owned()),
            feature_type: None,
            score: None,
            strand: Strand::Forward,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        }
    }

    fn lane(records: Vec<FeatureRecord>) -> TrackSnapshot {
        TrackSnapshot {
            id: TrackId::new(5),
            name: "peaks".to_owned(),
            data: TrackSnapshotData::Features(records),
        }
    }

    #[test]
    fn a_lane_result_is_accepted_only_by_the_generation_and_interval_that_asked() {
        let mut session = session();
        let interval = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
        let stale = session.panels.active().request();
        session.panels.active().generation_clock.advance();
        let current = session.panels.active().request();
        session.panels.active_mut().track_request_interval = Some(interval);

        let route = session.route_track_event(TrackEngineEvent::LaneCompleted {
            request: stale,
            interval,
            track: lane(vec![feature(10, "a")]),
        });
        assert!(matches!(route, TrackEventRoute::Discarded));
        assert_eq!(session.navigation_stale_result_rejections, 1);
        assert!(session.panels.active().track_snapshots.is_empty());

        let route = session.route_track_event(TrackEngineEvent::LaneCompleted {
            request: current,
            interval,
            track: lane(vec![feature(10, "a"), feature(50, "b")]),
        });
        assert!(
            matches!(route, TrackEventRoute::PanelChanged { panel_id } if panel_id == PanelId::new(1))
        );
        assert_eq!(session.navigation_stale_result_rejections, 1);
        session.panels.active_mut().selected_feature = Some(FeatureSelection {
            track_id: TrackId::new(5),
            record_index: 1,
        });

        // A refresh that lists the selected record at another index keeps it selected.
        session.route_track_event(TrackEngineEvent::LaneCompleted {
            request: current,
            interval,
            track: lane(vec![feature(5, "new"), feature(10, "a"), feature(50, "b")]),
        });
        assert_eq!(
            session.panels.active().selected_feature,
            Some(FeatureSelection {
                track_id: TrackId::new(5),
                record_index: 2,
            })
        );
        assert_eq!(
            session.panels.active().track_snapshot_generations[&TrackId::new(5)],
            current.generation().get()
        );

        let route = session.route_track_event(TrackEngineEvent::QueryCompleted {
            request: current,
            interval,
            completed: vec![TrackId::new(5)],
        });
        assert!(matches!(route, TrackEventRoute::PanelChanged { .. }));
        assert_eq!(session.panels.active().track_request_interval, None);
        assert_eq!(
            session.panels.active().track_settled_generation,
            Some(current.generation().get())
        );
    }

    #[test]
    fn failures_are_logged_and_only_the_focused_panel_takes_the_status_line() {
        let mut session = session();
        let request = session.panels.active().request();
        let route = session.route_track_event(TrackEngineEvent::Error {
            request: Some(request),
            track: Some(TrackId::new(5)),
            message: "peaks.bed: unreadable".to_owned(),
        });
        assert!(matches!(
            route,
            TrackEventRoute::ErrorReported {
                display_stale: true
            }
        ));
        assert_eq!(session.data_status, "peaks.bed: unreadable");
        assert_eq!(
            session.panels.active().track_query_error_generations[&TrackId::new(5)],
            request.generation().get()
        );
        let route = session.route_track_event(TrackEngineEvent::Error {
            request: None,
            track: None,
            message: "engine stopped".to_owned(),
        });
        assert!(matches!(
            route,
            TrackEventRoute::ErrorReported {
                display_stale: false
            }
        ));
        assert_eq!(session.data_status, "engine stopped");
        assert!(matches!(
            session.route_track_event(TrackEngineEvent::TrackAdded {
                id: TrackId::new(5)
            }),
            TrackEventRoute::Unrouted(TrackEngineEvent::TrackAdded { .. })
        ));
    }
}
