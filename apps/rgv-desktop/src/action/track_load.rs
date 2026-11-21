//! The desktop half of `Action::LoadTracks`: each source goes through the one open path its
//! kind has always used, and the loaded tracks become one undo step.

use crate::Viewer;
use crate::action::{Action, ActionOutcome, TrackLoad, TrackLoadKind, TrackRestore};
use crate::sources::{source_path, track_source};
use anyhow::Result;
use rgv_core::TrackId;
use std::collections::BTreeSet;
use std::path::Path;

/// What a load did: its outcome, the inverse, and the action a redo performs instead of loading
/// the sources again.
pub(super) type LoadPerformed = (ActionOutcome, Vec<Action>, Option<Action>);

impl Viewer {
    /// Loads sources through the action path, so the load is undoable. The entry point every
    /// open dialog, drop, recent-file choice and `load_track` shares.
    pub(crate) fn load_tracks(&mut self, loads: Vec<TrackLoad>) -> Result<ActionOutcome> {
        self.apply_action(Action::LoadTracks { loads })
    }

    /// Opens one local or remote path as the kind its extension names.
    pub(crate) fn load_track_path(&mut self, path: &Path, kind: TrackLoadKind) -> Result<()> {
        self.load_tracks(vec![TrackLoad {
            source: track_source(path),
            kind,
            name: None,
        }])
        .map(|_| ())
    }

    fn open_track_ids(&self) -> BTreeSet<TrackId> {
        self.session
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .chain(self.session.tracks.tracks().iter().map(|track| track.id))
            .collect()
    }

    /// The implementation behind `Action::LoadTracks`; only the action path calls it.
    pub(super) fn perform_track_loads(&mut self, loads: &[TrackLoad]) -> Result<LoadPerformed> {
        let mut loaded = Vec::new();
        let mut reference_opened = false;
        for load in loads {
            let before = self.open_track_ids();
            let path = source_path(&load.source);
            match &load.kind {
                TrackLoadKind::Reference { remote_index_url } => {
                    self.open_reference_source_with_remote_index(&path, remote_index_url.clone());
                    reference_opened |= self.session.reference_engine.is_some();
                }
                TrackLoadKind::Alignment {
                    remote_index_url: None,
                } => self.open_alignment_source(&path),
                TrackLoadKind::Alignment {
                    remote_index_url: Some(index),
                } => self.open_alignment_source_with_remote_index(&path, Some(index.clone())),
                TrackLoadKind::Annotation => self.add_track_source(&path)?,
            }
            let added = self
                .open_track_ids()
                .difference(&before)
                .copied()
                .collect::<Vec<_>>();
            if let Some(name) = &load.name {
                for id in &added {
                    if let Some(track) = self
                        .session
                        .alignment_tracks
                        .iter_mut()
                        .find(|track| track.config.id == *id)
                    {
                        track.config.name.clone_from(name);
                    } else if let Some(track) = self
                        .session
                        .tracks
                        .tracks_mut()
                        .iter_mut()
                        .find(|track| track.id == *id)
                    {
                        track.name.clone_from(name);
                    }
                }
            }
            loaded.extend(added);
        }
        if loaded.is_empty() {
            return Ok((
                ActionOutcome {
                    changed: reference_opened,
                    ..ActionOutcome::default()
                },
                Vec::new(),
                None,
            ));
        }
        // Redo reopens the tracks from their configuration as loaded, with the same ids and
        // places, rather than opening the sources as new tracks.
        let selected = loaded.iter().copied().collect::<BTreeSet<_>>();
        let restore = TrackRestore {
            alignment: self
                .session
                .alignment_tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.config.id))
                .map(|(index, track)| (index, track.config.clone()))
                .collect(),
            auxiliary: self
                .session
                .tracks
                .tracks()
                .iter()
                .enumerate()
                .filter(|(_, track)| selected.contains(&track.id))
                .map(|(index, track)| (index, track.clone()))
                .collect(),
            presentation: Box::new(self.session.track_presentation.clone()),
            suppressed_builtin_gene_bundles: self.session.suppressed_builtin_gene_bundles.clone(),
        };
        Ok((
            ActionOutcome {
                changed: true,
                loaded_tracks: loaded.clone(),
                ..ActionOutcome::default()
            },
            vec![Action::RemoveTracks { track_ids: loaded }],
            Some(Action::RestoreTracks(restore)),
        ))
    }
}
