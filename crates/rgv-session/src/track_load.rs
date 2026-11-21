//! Loading annotation tracks: registering a source in the track engine (starting the engine
//! on first use), adding it to the track list, and reconciling the arrangement with the
//! sources that exist.

use crate::panels::Panel;
use crate::session::Session;
use crate::sources::spawn_track_engine;
use anyhow::Result;
use rgv_core::{TrackChannel, TrackConfig, TrackId, TrackKind};
use rgv_engine::{TrackSpec, TrackSpecKind};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

impl<P: Panel> Session<P> {
    /// Opens `path` as the annotation track `config` (of engine kind `kind`). The track
    /// engine is started on first use; `query_contig_aliases` maps workspace contig names to
    /// the source's own spelling. The track joins the list and the arrangement, and the status
    /// line says it is opening; its lanes fill in as the engine answers.
    ///
    /// The host reconciles its own interaction state with the new arrangement afterwards.
    pub fn add_track_source(
        &mut self,
        path: &Path,
        kind: TrackSpecKind,
        config: TrackConfig,
        query_contig_aliases: BTreeMap<String, String>,
    ) -> Result<TrackId> {
        let id = config.id;
        let specification = TrackSpec {
            id,
            name: config.name.clone(),
            path: path.to_path_buf(),
            kind,
        };
        if let Some(engine) = &self.track_engine {
            if query_contig_aliases.is_empty() {
                engine.try_add(specification)?;
            } else {
                engine.try_add_with_query_contig_aliases(specification, query_contig_aliases)?;
            }
        } else {
            // An empty alias map is the no-alias case.
            self.track_engine = Some(spawn_track_engine(vec![(
                specification,
                query_contig_aliases,
            )]));
        }
        let source_label = config.source.redacted_label();
        self.tracks.push(config);
        self.invalidate_navigation_contract("auxiliary track added");
        self.reconcile_track_sources();
        self.synthetic = false;
        self.data_status = format!("Opening track {source_label}…");
        Ok(id)
    }

    /// Reconciles the arrangement with the sources that exist. Presentation choices are never
    /// read back from the track configurations: existing node order, visibility, preferred
    /// sizes and groups remain authoritative, and the configurations' own presentation flags
    /// are reset to their neutral values.
    pub fn reconcile_track_sources(&mut self) {
        let alignment_defaults = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.config.id,
                    track.config.annotation_height,
                    track
                        .config
                        .read_group_height
                        .or(Some(track.config.height.clamp(40.0, 1_200.0))),
                )
            })
            .collect::<Vec<_>>();
        let auxiliary_defaults = self
            .tracks
            .tracks()
            .iter()
            .map(|track| (track.id, Some(track.height.clamp(26.0, 1_200.0))))
            .collect::<Vec<_>>();
        self.track_presentation
            .reconcile_sources(&alignment_defaults, &auxiliary_defaults);
        let overlay_ids = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Quantitative)
            .filter_map(|track| track.quantitative.overlay_group)
            .fold(BTreeMap::<u32, usize>::new(), |mut counts, id| {
                *counts.entry(id).or_default() += 1;
                counts
            })
            .into_iter()
            .filter_map(|(id, count)| (count >= 2).then_some(id))
            .collect::<BTreeSet<_>>();
        self.track_presentation
            .reconcile_composites(TrackChannel::Annotation, &overlay_ids);
        self.track_presentation
            .reconcile_composites(TrackChannel::Reads, &BTreeSet::new());
        for track in &mut self.alignment_tracks {
            track.config.visible = true;
            track.config.collapsed = false;
            track.config.height = 360.0;
            track.config.annotation_height = None;
            track.config.read_group_height = None;
            track.config.filters.show_reads = true;
        }
        for track in self.tracks.tracks_mut() {
            track.visible = true;
            track.collapsed = false;
            track.height = 120.0;
        }
    }
}

// ----- Alignment tracks --------------------------------------------------------------------

use crate::alignment::track::{
    AlignmentTrackController, connect_reference_slice_sinks, filter_state_from_session,
    spawn_alignment_track_controller,
};
use crate::sources::source_path;
use rgv_core::{PanelId, SessionAlignmentTrack};
use rgv_engine::AlignmentSceneEngine;
use std::num::NonZeroUsize;
use std::path::PathBuf;

impl<P: Panel> Session<P> {
    /// Starts one controller (scene engine, filters, per-panel projections) per configuration,
    /// all charged to the session's scene ledger. Nothing is installed: on any failure the
    /// controllers already started are retired and the error returned.
    pub fn prepare_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
        reference_path: Option<PathBuf>,
        remote_reference_index_url: Option<String>,
        panel_ids: &[PanelId],
    ) -> Result<Vec<AlignmentTrackController>> {
        for config in &configs {
            filter_state_from_session(&config.filters)?;
        }
        let cache_bytes = NonZeroUsize::new(self.alignment_scene_budget.budget_bytes.max(1))
            .expect("scene budget is non-zero");
        let mut controllers = Vec::with_capacity(configs.len());
        for config in configs {
            match spawn_alignment_track_controller(
                config,
                cache_bytes,
                self.alignment_scene_ledger.clone(),
                reference_path.clone(),
                remote_reference_index_url.clone(),
                panel_ids,
            ) {
                Ok(controller) => controllers.push(controller),
                Err(error) => {
                    for controller in controllers {
                        self.retire_alignment_track_controller(controller);
                    }
                    return Err(error);
                }
            }
        }
        Ok(controllers)
    }

    /// [`Self::prepare_alignment_track_controllers`] for the session's current reference and
    /// panels, refused while too many replaced sources are still closing.
    pub fn prepare_alignment_track_controllers_for_workspace(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
    ) -> Result<Vec<AlignmentTrackController>> {
        self.poll_alignment_retirement_tasks();
        anyhow::ensure!(
            self.alignment_retirement_tasks.len() <= rgv_core::MAXIMUM_ALIGNMENT_TRACKS,
            "Previous alignment sources are still closing; retry shortly"
        );
        let panel_ids = self
            .panels
            .iter()
            .map(|panel| panel.state().id)
            .collect::<Vec<_>>();
        let reference_path = self.reference_source.as_ref().map(source_path);
        let remote_reference_index_url = self.remote_reference_index_url.clone();
        self.prepare_alignment_track_controllers(
            configs,
            reference_path,
            remote_reference_index_url,
            &panel_ids,
        )
    }

    /// Makes `controllers` the session's alignment tracks. Their scene engines are connected
    /// to the reference engine's slices; the replaced controllers are retired off the calling
    /// thread. The host resets whatever it derived from the old controllers and reconciles the
    /// arrangement.
    pub fn swap_alignment_track_controllers(&mut self, controllers: Vec<AlignmentTrackController>) {
        connect_reference_slice_sinks(self.reference_engine.as_ref(), &controllers);
        let replaced = std::mem::replace(&mut self.alignment_tracks, controllers);
        for controller in replaced {
            self.retire_alignment_track_controller(controller);
        }
    }

    /// Stops a controller's scene and coverage workers without blocking: each is told to stop
    /// and a reaper thread, kept in the retirement lists, waits for it. Every projection's
    /// generation advances first, so a late answer is never accepted.
    pub fn retire_alignment_track_controller(&mut self, controller: AlignmentTrackController) {
        for projection in controller.projections.values() {
            projection.advance_scene_generation();
        }
        let AlignmentTrackController {
            scene_engine,
            coverage_engine,
            ..
        } = controller;
        if let Some(task) = scene_engine.and_then(AlignmentSceneEngine::retire) {
            self.alignment_retirement_tasks.push(task);
        }
        // The coverage worker ends with its track; stopping it never blocks the caller.
        if let Some(task) =
            coverage_engine.and_then(crate::alignment::coverage::retire_alignment_coverage_engine)
        {
            self.alignment_coverage_retirement_tasks.push(task);
        }
    }

    /// Joins the alignment reapers that have finished.
    pub fn poll_alignment_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.alignment_retirement_tasks.len() {
            if !self.alignment_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            let task = self.alignment_retirement_tasks.remove(index);
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked");
            }
        }
        // Coverage workers are counted apart from scene workers: they stop within one
        // cancellation poll and must not consume the scene-retirement admission bound.
        let mut index = 0;
        while index < self.alignment_coverage_retirement_tasks.len() {
            if !self.alignment_coverage_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            let task = self.alignment_coverage_retirement_tasks.remove(index);
            if task.join().is_err() {
                tracing::warn!("alignment coverage reaper panicked");
            }
        }
    }

    /// Final shutdown: waits for every alignment reaper.
    pub fn finish_alignment_retirement_tasks(&mut self) {
        for task in self.alignment_retirement_tasks.drain(..) {
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked during shutdown");
            }
        }
        for task in self.alignment_coverage_retirement_tasks.drain(..) {
            if task.join().is_err() {
                tracing::warn!("alignment coverage reaper panicked during shutdown");
            }
        }
    }
}

// ----- Track ids and TDF channels ----------------------------------------------------------

use crate::sources::build_tdf_channel_expansion;
use rgv_core::TrackFormat;

impl<P: Panel> Session<P> {
    fn largest_live_track_id(&self) -> u64 {
        self.tracks
            .tracks()
            .iter()
            .map(|track| track.id.get())
            .chain(
                self.alignment_tracks
                    .iter()
                    .map(|track| track.config.id.get()),
            )
            .max()
            .unwrap_or_default()
    }

    /// A track id that has not been used before in this run, even by a track since removed:
    /// worker events for a removed track may still be in flight.
    pub fn allocate_track_id(&mut self) -> TrackId {
        let largest_live = self.largest_live_track_id();
        self.track_ids.allocate(largest_live)
    }

    /// First id of a caller-numbered block; [`Self::reserve_track_ids_through`] records its end.
    pub fn peek_track_id(&self) -> Option<u64> {
        self.track_ids.peek(self.largest_live_track_id())
    }

    pub fn reserve_track_ids_through(&mut self, id: u64) {
        self.track_ids.reserve_through(id);
    }

    /// Replaces a TDF track that turned out to hold several embedded channels with one track
    /// per channel: the parent becomes the selected channel and the others are inserted after
    /// it, registered in the track engine under fresh ids. Returns whether the track list
    /// changed; the host reconciles its interaction state when it did.
    pub fn expand_tdf_channels(
        &mut self,
        parent_id: TrackId,
        track_names: &[String],
        selected_index: usize,
    ) -> Result<bool> {
        if track_names.is_empty() || selected_index >= track_names.len() {
            anyhow::bail!("TDF source returned an invalid embedded-channel catalogue");
        }
        let Some(parent_position) = self
            .tracks
            .tracks()
            .iter()
            .position(|track| track.id == parent_id)
        else {
            return Ok(false);
        };
        let parent = self.tracks.tracks()[parent_position].clone();
        if matches!(parent.format, Some(TrackFormat::TdfChannel { .. })) {
            return Ok(false);
        }
        let path = source_path(&parent.source);
        let source_label = parent.source.redacted_label();
        let next_id = self
            .peek_track_id()
            .ok_or_else(|| anyhow::anyhow!("TDF channel track ID space is exhausted"))?;
        let expansion =
            build_tdf_channel_expansion(&parent, &path, track_names, selected_index, next_id)?;
        if !expansion.specifications.is_empty() {
            let engine = self
                .track_engine
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("TDF track engine is unavailable"))?;
            engine.try_add_many(expansion.specifications)?;
        }
        let parent = &mut self.tracks.tracks_mut()[parent_position];
        parent.name = expansion.selected_name;
        parent.format = Some(TrackFormat::TdfChannel {
            index: selected_index,
        });
        if let Some(largest) = expansion.children.iter().map(|track| track.id.get()).max() {
            self.reserve_track_ids_through(largest);
        }
        self.tracks.insert_after(parent_id, expansion.children);
        if selected_index > 0 {
            self.tracks
                .move_track(parent_id, parent_position.saturating_add(selected_index));
        }
        self.reconcile_track_sources();
        self.data_status = format!(
            "Opened {} embedded TDF channels from {}",
            track_names.len(),
            source_label
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use crate::panels::PanelState;
    use crate::session::Session;
    use rgv_core::{
        ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, PanelId, PresentationNodeId,
        ReferenceDictionary, TrackChannel, TrackConfig, TrackKind, TrackSource, Viewport,
    };
    use rgv_engine::TrackSpecKind;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

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
        let mut session = Session::new(
            panel,
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            PathBuf::from("settings.json"),
        );
        session.synthetic = true;
        session
    }

    #[test]
    fn a_loaded_track_joins_the_list_and_the_arrangement_and_starts_the_engine() {
        let mut session = session();
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../benchmark-data/manual-suite/imports/regions.bed"
        ));
        let id = session.allocate_track_id();
        let mut config = TrackConfig::new(
            id,
            "regions".to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: path.display().to_string(),
            },
        );
        config.height = 3_000.0;
        let revision = session.navigation_contract_revision;
        assert_eq!(
            session
                .add_track_source(path, TrackSpecKind::Bed, config, BTreeMap::new())
                .unwrap(),
            id
        );
        assert!(session.track_engine.is_some());
        assert!(!session.synthetic);
        assert!(session.data_status.starts_with("Opening track"));
        assert!(session.navigation_contract_revision > revision);
        assert_eq!(session.tracks.tracks().len(), 1);
        // The arrangement took the track, with its preferred height clamped from the config;
        // the config's own presentation fields are reset to neutral values.
        assert!(
            session
                .track_presentation
                .pane_interactive_nodes(TrackChannel::Annotation)
                .contains(&PresentationNodeId::Track {
                    channel: TrackChannel::Annotation,
                    track_id: id,
                })
        );
        assert_eq!(session.tracks.tracks()[0].height, 120.0);
        // Ids are never reused within a run.
        assert!(session.allocate_track_id() > id);
        session.retire_track_engine();
        session.join_engine_retirement_tasks();
    }
}
