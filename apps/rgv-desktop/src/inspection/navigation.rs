use super::super::*;
use crate::display::alignment;

impl Viewer {
    pub(crate) fn visible_consensus(&self) -> Option<BoundedSequenceCopy> {
        let query = self.primary_alignment_query()?;
        let visible = query
            .interval
            .intersection(self.panels.active().viewport.interval())?;
        let bases = query
            .pileup
            .consensus(query.interval, visible, query.reference.as_ref())?;
        Some(bounded_sequence_copy(&bases))
    }

    pub(crate) fn visible_reference_sequence(&self) -> Option<BoundedSequenceCopy> {
        let reference = self
            .panels
            .active()
            .reference_slice
            .as_deref()
            .filter(|_| self.panels.active().reference_slice_revision == self.reference_revision)
            .or_else(|| {
                self.primary_alignment_query()
                    .and_then(|query| query.reference.as_ref())
            })?;
        let visible = self
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
        let track_id = self.panels.active().selected_alignment_track?;
        let index = self.panels.active().selected_record?;
        let record = self
            .complete_read_query_for_track(track_id)?
            .materialize_record(index)
            .ok()?;
        let locus = mate_locus(
            &record,
            self.alignment_dictionary_for_track(track_id)?,
            self.panels.active().viewport.interval().len(),
        )?;
        Some((PendingReadSelection::for_mate(track_id, &record)?, locus))
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
        self.display_dirty = true;
        Ok(id)
    }

    pub(crate) fn open_genomic_position_in_new_panel(
        &mut self,
        contig_name: &str,
        position: u64,
    ) -> Result<PanelId> {
        let dictionary = self
            .primary_alignment_dictionary()
            .unwrap_or(&self.assembly.dictionary);
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
            .panels
            .active()
            .selected_alignment_track
            .context("supplementary-chain track is unavailable")?;
        let mut chain = self
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
        self.display_dirty = true;
        Ok(id)
    }

    pub(crate) fn open_all_chain_segments_in_panels(&mut self) -> Result<(usize, usize)> {
        let origin = self.panels.active().id;
        let track_id = self
            .panels
            .active()
            .selected_alignment_track
            .context("supplementary-chain track is unavailable")?;
        let chain = self
            .panels
            .active()
            .selected_chain
            .clone()
            .context("supplementary-chain selection is unavailable")?;
        let segment_indices = (0..chain.segments.len())
            .filter(|index| *index != chain.active_segment)
            .collect::<Vec<_>>();
        let requested = segment_indices.len();
        let available = MAXIMUM_VIEW_PANELS.saturating_sub(self.panels.len());
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
        self.display_dirty = true;
        Ok((opened, requested))
    }

    pub(crate) fn resolve_pending_read_selection(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        query: &AlignmentQuery,
    ) {
        let Some(pending_selection) = self
            .panels
            .get(panel_id)
            .and_then(|panel| panel.pending_read_selection.clone())
        else {
            return;
        };
        if pending_selection.track_id != track_id {
            return;
        }
        let complete = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| {
                let projection = track.projection(panel_id)?;
                let layout_query = projection.layout_query.as_deref().unwrap_or(query);
                Some(
                    alignment::individual_read_refusal(
                        query,
                        layout_query,
                        track.config.filters.read_stacking_mode,
                    )
                    .is_none(),
                )
            })
            .unwrap_or(false);
        if !complete {
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.selected_record = None;
                panel.selected_read_event = None;
                panel.selected_chain = None;
            }
            return;
        }
        let Some(index) = pending_selection.matching_index(query) else {
            // The selected read may be outside this query's interval. Its old
            // numeric index is unsafe against the new batch, but retain the
            // stable pending identity so it can reappear after navigation.
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.selected_record = None;
                panel.selected_read_event = None;
            }
            return;
        };
        let contig_name = self
            .panels
            .get(panel_id)
            .map(|panel| panel.contig_name.clone())
            .unwrap_or_default();
        let selected_record = query.materialize_record(index).ok();
        let selected_chain = selected_record
            .as_ref()
            .and_then(|record| selected_supplementary_chain(record, &contig_name));
        let (retained_selection, reveal) =
            pending_selection.retained_after_resolution(selected_record.as_ref());
        if let Some(panel) = self.panels.get_mut(panel_id) {
            if reveal {
                panel.pending_read_reveal = Some((track_id, index));
            }
            // Record indexes belong to one query batch. Keep the exact read
            // identity as the panel's durable selection so every later zoom,
            // pan, or workspace refresh can bind it again.
            panel.pending_read_selection = Some(retained_selection);
            panel.selected_record = Some(index);
            panel.selected_alignment_track = Some(track_id);
            replace_panel_track_selection(
                panel,
                TrackFrameKey::Alignment(track_id),
                ui::TrackPane::Reads,
            );
            panel.selected_chain = selected_chain;
        }
    }
}
