//! Deferred changes to regions of interest, the review queue and sample metadata.

use super::*;

/// Whether any request this frame reads or edits the region and review lists. They are only
/// copied into the frame when this is true or one of their windows is open.
pub(crate) fn workspace_list_edit_requested(requests: &[bool]) -> bool {
    requests.iter().any(|requested| *requested)
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_region_and_review_changes(&mut self, st: &mut ShellFrame) {
        if !st.lists.lists_loaded {
            let requested = workspace_list_edit_requested(&[
                st.lists.add_current_region,
                st.lists.add_current_review,
                st.lists.add_selected_feature_to_review,
                st.lists.region_to_delete.is_some(),
                st.lists.review_to_delete.is_some(),
                st.lists.region_to_relocate.is_some(),
                st.lists.review_to_relocate.is_some(),
                st.lists.previous_review_requested,
                st.lists.next_review_requested,
            ]);
            if !requested {
                // Neither list window was open and nothing asked to change the lists: they
                // were never copied into this frame, so there is nothing to commit.
                self.review_status_filter = st.lists.review_status_filter;
                return;
            }
            st.lists.regions.clone_from(&self.regions_of_interest);
            st.lists.review_items.clone_from(&self.review_items);
            st.lists.lists_loaded = true;
        }
        if st.lists.add_current_region {
            if st.lists.regions.len() >= MAXIMUM_REGIONS_OF_INTEREST {
                self.data_status = format!(
                    "Cannot add another region: the {MAXIMUM_REGIONS_OF_INTEREST}-region workspace cap was reached"
                );
            } else if let Some(id) =
                next_available_item_id(st.lists.regions.iter().map(|region| region.id))
            {
                st.lists.regions.push(RegionOfInterest {
                    id,
                    name: format!("Region {id}"),
                    locus: SessionLocus {
                        contig: st.snapshot.contig_name.clone(),
                        start: st.snapshot.interval.start(),
                        end: st.snapshot.interval.end(),
                    },
                    color: [45, 92, 175, 48],
                });
                st.lists.regions_open = true;
            } else {
                "Cannot add another region: no stable identifier is available"
                    .clone_into(&mut self.data_status);
            }
        }
        if st.lists.add_current_review {
            if st.lists.review_items.len() >= MAXIMUM_REVIEW_ITEMS {
                self.data_status = format!(
                    "Cannot add another review item: the {MAXIMUM_REVIEW_ITEMS}-item workspace cap was reached"
                );
            } else if let Some(id) =
                next_available_item_id(st.lists.review_items.iter().map(|item| item.id))
            {
                st.lists.review_items.push(ReviewItem {
                    id,
                    label: st.snapshot.status_locus.clone(),
                    locus: SessionLocus {
                        contig: st.snapshot.contig_name.clone(),
                        start: st.snapshot.interval.start(),
                        end: st.snapshot.interval.end(),
                    },
                    status: ReviewStatus::Pending,
                    notes: String::new(),
                    assignment: None,
                });
                st.lists.review_open = true;
            } else {
                "Cannot add another review item: no stable identifier is available"
                    .clone_into(&mut self.data_status);
            }
        }
        if st.lists.add_selected_feature_to_review
            && let Some(inspector) = &st.snapshot.inspectors.feature_inspector
        {
            if st.lists.review_items.len() >= MAXIMUM_REVIEW_ITEMS {
                self.data_status = format!(
                    "Cannot add another review item: the {MAXIMUM_REVIEW_ITEMS}-item workspace cap was reached"
                );
            } else if let Some(id) =
                next_available_item_id(st.lists.review_items.iter().map(|item| item.id))
            {
                st.lists.review_items.push(ReviewItem {
                    id,
                    label: inspector
                        .record
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{} feature", inspector.track_name)),
                    locus: SessionLocus {
                        contig: inspector.record.contig.clone(),
                        start: inspector.record.start,
                        end: inspector.record.end,
                    },
                    status: ReviewStatus::Pending,
                    notes: String::new(),
                    assignment: None,
                });
            }
        }
        if let Some(id) = st.lists.region_to_delete {
            st.lists.regions.retain(|region| region.id != id);
        }
        if let Some(id) = st.lists.review_to_delete {
            st.lists.review_items.retain(|item| item.id != id);
        }
        if let Some(id) = st.lists.region_to_relocate
            && let Some(region) = st.lists.regions.iter_mut().find(|region| region.id == id)
        {
            region.locus = SessionLocus {
                contig: st.snapshot.contig_name.clone(),
                start: st.snapshot.interval.start(),
                end: st.snapshot.interval.end(),
            };
        }
        if let Some(id) = st.lists.review_to_relocate
            && let Some(item) = st.lists.review_items.iter_mut().find(|item| item.id == id)
        {
            item.locus = SessionLocus {
                contig: st.snapshot.contig_name.clone(),
                start: st.snapshot.interval.start(),
                end: st.snapshot.interval.end(),
            };
        }
        normalize_regions_and_review_items(&mut st.lists.regions, &mut st.lists.review_items);
        if st.lists.previous_review_requested || st.lists.next_review_requested {
            st.lists.managed_locus_requested = adjacent_review_locus(
                &st.lists.review_items,
                st.lists.review_status_filter,
                &st.snapshot.contig_name,
                st.snapshot.interval.start(),
                st.snapshot.interval.end(),
                st.lists.next_review_requested,
            )
            .map(|locus| session_locus_text(&locus));
        }
        let regions_changed = st.lists.regions != self.regions_of_interest;
        let review_changed = st.lists.review_items != self.review_items;
        self.regions_of_interest = std::mem::take(&mut st.lists.regions);
        self.review_items = std::mem::take(&mut st.lists.review_items);
        self.review_status_filter = st.lists.review_status_filter;
        if regions_changed {
            self.invalidate_navigation_contract("region overlays changed");
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
        } else if review_changed {
            self.mark_display_dirty("explicit display invalidation");
        }
    }

    /// Commits the sample-metadata filter and selection; returns whether the selection changed.
    pub(super) fn apply_metadata_selection(&mut self, st: &mut ShellFrame) -> bool {
        if st.lists.metadata_remove_requested {
            self.sample_metadata = None;
            st.lists.metadata_filter_value = None;
            st.lists.metadata_selected_tracks.clear();
            "Removed sample metadata; stable track/sample assignments were retained"
                .clone_into(&mut self.data_status);
        }
        if st.lists.metadata_select_requested
            && let (Some(table), Some(value)) = (
                &st.lists.sample_metadata,
                st.lists.metadata_filter_value.as_deref(),
            )
        {
            st.lists.metadata_selected_tracks = metadata_matching_track_ids(
                &st.lists.metadata_track_rows,
                table,
                st.lists.metadata_filter_column,
                value,
            );
            self.data_status = format!(
                "Selected {} tracks whose {} is {value:?}",
                st.lists.metadata_selected_tracks.len(),
                table.columns[st.lists.metadata_filter_column]
            );
        }
        let live_track_ids = st
            .lists
            .metadata_track_rows
            .iter()
            .map(|(id, _, _, _)| *id)
            .collect::<BTreeSet<_>>();
        st.lists
            .metadata_selected_tracks
            .retain(|id| live_track_ids.contains(id));
        let metadata_selection_changed =
            st.lists.metadata_selected_tracks != self.metadata_selected_tracks;
        self.metadata_filter_column = st.lists.metadata_filter_column;
        self.metadata_filter_value = st.lists.metadata_filter_value.take();
        self.metadata_selected_tracks = std::mem::take(&mut st.lists.metadata_selected_tracks);
        if metadata_selection_changed {
            let selected_alignment_tracks = self
                .alignment_tracks
                .iter()
                .filter(|track| self.metadata_selected_tracks.contains(&track.config.id))
                .map(|track| track.config.id)
                .collect::<Vec<_>>();
            let selected_auxiliary_tracks = self
                .tracks
                .tracks()
                .iter()
                .filter(|track| self.metadata_selected_tracks.contains(&track.id))
                .map(|track| track.id)
                .collect::<BTreeSet<_>>();
            let selected_nodes = self
                .track_presentation
                .pane_root_nodes(ui::TrackPane::Annotation)
                .into_iter()
                .filter(|node| match node {
                    PresentationNodeId::Track { track_id, .. } => {
                        selected_alignment_tracks.contains(track_id)
                            || selected_auxiliary_tracks.contains(track_id)
                    }
                    PresentationNodeId::Reference | PresentationNodeId::Group { .. } => false,
                })
                .collect::<Vec<_>>();
            let panel = self.panels.active_mut();
            panel.selected_alignment_tracks = selected_alignment_tracks;
            panel.selected_read_tracks.clear();
            panel.read_selection_anchor = None;
            panel.selected_presentation_nodes = selected_nodes;
            panel.presentation_selection_anchor = panel.selected_presentation_nodes.last().copied();
            panel.selected_track =
                panel
                    .selected_presentation_nodes
                    .last()
                    .and_then(|node| match node {
                        PresentationNodeId::Track { track_id, .. } => {
                            if selected_auxiliary_tracks.contains(track_id) {
                                Some(TrackFrameKey::Auxiliary(*track_id))
                            } else {
                                Some(TrackFrameKey::Alignment(*track_id))
                            }
                        }
                        PresentationNodeId::Reference | PresentationNodeId::Group { .. } => None,
                    });
            panel.alignment_selection_anchor = panel.selected_alignment_tracks.last().copied();
        }
        metadata_selection_changed
    }

    /// Imports/exports of metadata, regions and the review queue, then managed-locus navigation.
    pub(super) fn apply_workspace_list_transfers(&mut self, st: &mut ShellFrame) {
        if st.lists.metadata_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Sample attributes", &["tsv", "txt"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::SampleMetadata);
        }
        if st.lists.region_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("BED regions", &["bed"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::Regions);
        }
        if st.lists.region_export_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-regions.bed")
                .add_filter("BED regions", &["bed"])
                .save_file()
        {
            self.start_export(path, region_bed_export(&self.regions_of_interest));
        }
        if st.lists.review_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("RGV review queue", &["tsv"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::Review);
        }
        if st.lists.review_export_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-review-queue.tsv")
                .add_filter("RGV review queue", &["tsv"])
                .save_file()
        {
            self.start_export(path, review_tsv_export(&self.review_items));
        }
        if let Some(locus) = st.lists.managed_locus_requested.take()
            && let Err(error) = self.navigate_to_locus(&locus)
        {
            self.data_status = error.to_string();
        }
    }
}
