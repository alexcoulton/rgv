use crate::inspection::model::BoundedSequenceCopy;
use crate::inspection::read::{bounded_oriented_sequence_copy, bounded_sequence_copy};
use crate::ui::TrackFrameKey;
use crate::workspace::navigation::{mate_locus, mate_viewport};
use crate::workspace::panels::PendingReadSelection;
use crate::workspace::tracks::replace_panel_track_selection;
use crate::{Viewer, ui};
use anyhow::{Context as _, Result};
use rgv_core::{MAXIMUM_VIEW_PANELS, PanelId, ReferenceDictionary, TrackId, Viewport};
use rgv_hts::AlignmentRecord;

impl Viewer {
    fn selected_mate_record(&self) -> Option<(TrackId, AlignmentRecord, &ReferenceDictionary)> {
        let panel = self.session.panels.active();
        let track_id = panel
            .selected_alignment_hit
            .map(|hit| hit.track)
            .or(panel.selected_alignment_track)?;
        let record =
            self.alignment_scene_record_for_hit(panel.id, panel.selected_alignment_hit?)?;
        let dictionary = self.alignment_dictionary_for_track(track_id)?;
        Some((track_id, record, dictionary))
    }

    pub(crate) fn visible_consensus(&self) -> Option<BoundedSequenceCopy> {
        let track_id = self.primary_alignment_track_id();
        if let Some(presented) = self
            .presented_alignment_lane(self.session.panels.active().id, track_id)
            .filter(|presented| presented.lane.summary_current())
            && let Some(snapshot) = presented.lane.snapshot.as_ref()
        {
            let visible = snapshot
                .prepared_interval()
                .intersection(self.session.panels.active().viewport.interval())?;
            let reference = self
                .session
                .panels
                .active()
                .reference_slice
                .as_deref()
                .filter(|_| {
                    snapshot.reads().presentation().reference
                        == self.session.panels.active().reference_slice_revision
                });
            let mut bases = Vec::with_capacity(usize::try_from(visible.len()).ok()?);
            for position in visible.start()..visible.end() {
                let support = snapshot
                    .summaries()
                    .allele_site_evidence_at(position)?
                    .bin
                    .quality_weighted;
                let candidates = [
                    (b'A', support.a),
                    (b'C', support.c),
                    (b'G', support.g),
                    (b'T', support.t),
                    (b'N', support.n),
                ];
                let total = support.total();
                let base = if total == 0 {
                    reference
                        .and_then(|reference| reference.base_at(position))
                        .map_or(b'N', |base| base.to_ascii_uppercase())
                } else {
                    let (base, maximum) = candidates
                        .into_iter()
                        .max_by_key(|(_, value)| *value)
                        .unwrap();
                    if maximum.saturating_mul(2) > total {
                        base
                    } else {
                        b'N'
                    }
                };
                bases.push(base);
            }
            return Some(bounded_sequence_copy(&bases));
        }
        None
    }

    pub(crate) fn visible_reference_sequence(&self) -> Option<BoundedSequenceCopy> {
        let reference = self
            .session
            .panels
            .active()
            .reference_slice
            .as_deref()
            .filter(|_| {
                self.session.panels.active().reference_slice_revision
                    == self.session.reference_revision
            })?;
        let visible = self
            .session
            .panels
            .active()
            .viewport
            .interval()
            .intersection(reference.interval)?;
        let start = usize::try_from(visible.start() - reference.interval.start()).ok()?;
        let end = usize::try_from(visible.end() - reference.interval.start()).ok()?;
        Some(bounded_oriented_sequence_copy(
            reference.bases.get(start..end)?,
            self.reference_reverse_complement,
        ))
    }

    pub(crate) fn selected_mate_target(&self) -> Option<(PendingReadSelection, String)> {
        let (track_id, record, dictionary) = self.selected_mate_record()?;
        let locus = mate_locus(
            &record,
            dictionary,
            self.session.panels.active().viewport.interval().len(),
        )?;
        Some((PendingReadSelection::for_mate(track_id, &record)?, locus))
    }

    fn selected_mate_viewport_target(
        &self,
    ) -> Option<(PendingReadSelection, Viewport, String, u64)> {
        let (track_id, record, dictionary) = self.selected_mate_record()?;
        let (viewport, contig_name, contig_length) =
            mate_viewport(&record, dictionary, self.session.panels.active().viewport)?;
        Some((
            PendingReadSelection::for_mate(track_id, &record)?,
            viewport,
            contig_name,
            contig_length,
        ))
    }

    pub(crate) fn open_selected_mate_in_new_panel(&mut self) -> Result<PanelId> {
        let (pending_selection, locus) = self
            .selected_mate_target()
            .context("the selected read has no mapped mate location")?;
        let track_id = pending_selection.track_id;
        // Adding a panel resizes and refreshes every viewport. Capture the
        // source identity before any of those transitions can invalidate its
        // query-local record index.
        self.retain_active_read_selection_for_query_refresh();
        let id = self.add_view_panel(&locus)?;
        let panel = self
            .session
            .panels
            .get_mut(id)
            .context("new mate panel is unavailable")?;
        panel.pending_read_selection = Some(pending_selection);
        panel.selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            panel,
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Reads,
        );
        self.mark_display_dirty("explicit display invalidation");
        Ok(id)
    }

    /// Replaces the active panel's locus with the selected read's mate locus while retaining the
    /// current genomic span. The mate is resolved by stable record identity only after the
    /// destination alignment scene commits, at which point it is selected and revealed in the
    /// read stack.
    pub(crate) fn go_to_selected_mate_in_current_panel(&mut self) -> Result<PanelId> {
        let (pending_selection, viewport, contig_name, contig_length) = self
            .selected_mate_viewport_target()
            .context("the selected read has no mapped mate location")?;
        let track_id = pending_selection.track_id;
        let panel_id = self.session.panels.active().id;

        self.retain_active_read_selection_for_query_refresh();
        self.navigate_to_viewport(viewport, contig_name, contig_length);

        let panel = self
            .session
            .panels
            .get_mut(panel_id)
            .context("the mate destination panel is unavailable")?;
        panel.pending_read_selection = Some(pending_selection);
        panel.pending_alignment_reveal = None;
        panel.selected_alignment_track = Some(track_id);
        panel.selected_alignment_hit = None;
        panel.selected_chain = None;
        replace_panel_track_selection(
            panel,
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Reads,
        );
        self.synchronize_alignment_scene_interactions(panel_id);
        self.mark_display_dirty("explicit display invalidation");
        Ok(panel_id)
    }

    pub(crate) fn open_genomic_position_in_new_panel(
        &mut self,
        contig_name: &str,
        position: u64,
    ) -> Result<PanelId> {
        let dictionary = self
            .primary_alignment_dictionary()
            .unwrap_or(&self.session.assembly.dictionary);
        let contig_id = dictionary
            .id_by_name(contig_name)
            .with_context(|| format!("the active genome has no contig named {contig_name}"))?;
        let contig = dictionary
            .get(contig_id)
            .context("the active genome dictionary is inconsistent")?;
        anyhow::ensure!(
            position < contig.length(),
            "requested evidence position is outside its contig"
        );
        let span = self
            .session
            .panels
            .active()
            .viewport
            .interval()
            .len()
            .max(1)
            .min(contig.length());
        let start = position
            .saturating_sub(span / 2)
            .min(contig.length().saturating_sub(span));
        let locus = format!("{contig_name}:{}-{}", start + 1, start + span);
        self.add_view_panel(&locus)
    }

    pub(crate) fn open_chain_segment_in_new_panel(&mut self, index: usize) -> Result<PanelId> {
        let track_id = self
            .session
            .panels
            .active()
            .selected_alignment_track
            .context("supplementary-chain track is unavailable")?;
        let mut chain = self
            .session
            .panels
            .active()
            .selected_chain
            .clone()
            .context("supplementary-chain selection is unavailable")?;
        let segment = chain
            .segments
            .get(index)
            .context("supplementary-chain segment is unavailable")?;
        let locus = format!(
            "{}:{}-{}",
            segment.reference_name,
            segment.start.saturating_add(1),
            segment.end
        );
        chain.active_segment = index;
        let read_name = chain.read_name.clone();
        let id = self.add_view_panel(&locus)?;
        let panel = self
            .session
            .panels
            .get_mut(id)
            .context("new supplementary panel is unavailable")?;
        panel.pending_read_selection = PendingReadSelection::by_name(track_id, read_name);
        panel.selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            panel,
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Reads,
        );
        panel.selected_chain = Some(chain);
        self.mark_display_dirty("explicit display invalidation");
        Ok(id)
    }

    pub(crate) fn open_all_chain_segments_in_panels(&mut self) -> Result<(usize, usize)> {
        let origin = self.session.panels.active().id;
        let track_id = self
            .session
            .panels
            .active()
            .selected_alignment_track
            .context("supplementary-chain track is unavailable")?;
        let chain = self
            .session
            .panels
            .active()
            .selected_chain
            .clone()
            .context("supplementary-chain selection is unavailable")?;
        let segment_indices = (0..chain.segments.len())
            .filter(|index| *index != chain.active_segment)
            .collect::<Vec<_>>();
        let requested = segment_indices.len();
        let available = MAXIMUM_VIEW_PANELS.saturating_sub(self.session.panels.len());
        let mut opened = 0;
        for index in segment_indices.into_iter().take(available) {
            let segment = &chain.segments[index];
            let locus = format!(
                "{}:{}-{}",
                segment.reference_name,
                segment.start.saturating_add(1),
                segment.end
            );
            let id = self.add_view_panel(&locus)?;
            let mut destination_chain = chain.clone();
            destination_chain.active_segment = index;
            let panel = self
                .session
                .panels
                .get_mut(id)
                .context("new supplementary panel is unavailable")?;
            panel.pending_read_selection =
                PendingReadSelection::by_name(track_id, chain.read_name.clone());
            panel.selected_alignment_track = Some(track_id);
            replace_panel_track_selection(
                panel,
                TrackFrameKey::Alignment(track_id),
                ui::TrackPane::Reads,
            );
            panel.selected_chain = Some(destination_chain);
            opened += 1;
        }
        self.activate_view_panel(origin);
        self.mark_display_dirty("explicit display invalidation");
        Ok((opened, requested))
    }
}
