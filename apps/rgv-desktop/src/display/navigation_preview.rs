use super::super::*;
use super::alignment;

const HORIZON_PREVIEW_BUILD_BUDGET: Duration = Duration::from_millis(12);

pub(crate) fn complete_horizon_preview_eligible(
    query: &AlignmentQuery,
    target: GenomicInterval,
    query_contract_revision: u64,
    current_contract_revision: u64,
    expected_complete_read_limit: usize,
) -> bool {
    query_contract_revision == current_contract_revision
        && query.individual_reads_complete()
        && !query.stats.cancelled
        && query.interval.contains_interval(target)
        && query.stats.complete_read_limit == expected_complete_read_limit
}

#[derive(Clone)]
struct HorizonPaneInput {
    query: Arc<AlignmentQuery>,
    row_history: AlignmentRowHistory,
    config: SessionAlignmentTrack,
    row_revision: u64,
    sort_position: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    pair_length_bounds: TemplateLengthBounds,
    row_scale_class: u8,
    scroll: f32,
    clip_top: f32,
    clip_bottom: f32,
    reads_top: f32,
}

impl Viewer {
    /// Attempts the optional accurate read-stack preview. The retained texture
    /// remains the immediate fallback and still supplies coverage/annotation
    /// pixels; this layer replaces only complete read-pane geometry.
    pub(crate) fn try_build_complete_horizon_preview(
        &mut self,
        panel_id: PanelId,
        target: NavigationQueryTarget,
    ) {
        // Unified scene lanes retain and reproject their semantic pages directly. Query-local
        // read geometry is neither a preview nor a fallback for a supported source.
        if self
            .alignment_tracks
            .iter()
            .any(|track| track.scene_engine.is_some())
        {
            return;
        }
        self.navigation_horizon_preview_stats.eligibility_checks = self
            .navigation_horizon_preview_stats
            .eligibility_checks
            .saturating_add(1);
        if self.horizon_preview_budget_exceeded(panel_id) {
            self.navigation_horizon_preview_stats.budget_fallbacks = self
                .navigation_horizon_preview_stats
                .budget_fallbacks
                .saturating_add(1);
            return;
        }
        let started = Instant::now();
        let Some(display) = self.build_complete_horizon_preview(panel_id, target) else {
            self.navigation_horizon_preview_stats.misses = self
                .navigation_horizon_preview_stats
                .misses
                .saturating_add(1);
            return;
        };
        let elapsed = started.elapsed();
        self.navigation_horizon_preview_stats.last_build_micros =
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.navigation_horizon_preview_stats.last_rectangle_count = display.rectangles.len();
        if elapsed > HORIZON_PREVIEW_BUILD_BUDGET {
            self.navigation_horizon_preview_stats.budget_fallbacks = self
                .navigation_horizon_preview_stats
                .budget_fallbacks
                .saturating_add(1);
            return;
        }

        let Some(panel) = self.panels.get_mut(panel_id) else {
            return;
        };
        if panel.generation_clock.current().get() != target.generation
            || NavigationTargetFingerprint::from_viewport(panel.viewport) != target.fingerprint
            || !panel.navigation_presentation.prefer_complete_horizon()
        {
            return;
        }
        panel.navigation_provisional_display = Some(display);
        self.navigation_horizon_preview_stats.hits =
            self.navigation_horizon_preview_stats.hits.saturating_add(1);
        self.display_dirty = true;
    }

    fn horizon_preview_budget_exceeded(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.panels.get(panel_id) else {
            return false;
        };
        let Some(cache) = panel.scroll_display_cache.as_ref() else {
            return false;
        };
        let mut records = 0_usize;
        for read_panel in &cache.read_panels {
            let Some(projection) = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == read_panel.track_id)
                .and_then(|track| track.projection(panel_id))
            else {
                continue;
            };
            let Some(query) = projection.layout_query.as_ref() else {
                continue;
            };
            records = records.saturating_add(query.batch.len());
            if records > MAX_PROVISIONAL_HORIZON_RECORDS
                || query.estimated_bytes() > MAX_PROVISIONAL_HORIZON_BYTES
                || projection.row_history.assignment_count() > MAX_PROVISIONAL_HORIZON_RECORDS
            {
                return true;
            }
        }
        false
    }

    fn build_complete_horizon_preview(
        &self,
        panel_id: PanelId,
        target: NavigationQueryTarget,
    ) -> Option<DisplayList> {
        let panel_rect = self.panel_column_rect(panel_id)?;
        let panel = self.panels.get(panel_id)?;
        if panel.navigation_presentation.source.is_none()
            || panel.viewport.detail_level() == DetailLevel::Overview
            || panel.generation_clock.current().get() != target.generation
            || NavigationTargetFingerprint::from_viewport(panel.viewport) != target.fingerprint
        {
            return None;
        }
        let cache = panel.scroll_display_cache.as_ref()?;
        if cache.read_panels.is_empty() {
            return None;
        }

        let requests = self
            .alignment_tracks
            .iter()
            .map(|track| {
                if matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_)) {
                    0
                } else {
                    track.config.maximum_alignments
                }
            })
            .collect::<Vec<_>>();
        let panel_budget = (self.workspace_alignment_limit / self.panels.len().max(1)).max(1);
        let allocations = allocate_workspace_records(panel_budget, &requests);
        let allocations = self
            .alignment_tracks
            .iter()
            .zip(allocations)
            .map(|(track, allocation)| (track.config.id, allocation))
            .collect::<HashMap<_, _>>();

        let mut total_records = 0_usize;
        let panes = cache
            .read_panels
            .iter()
            .map(|read_panel| {
                let track = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == read_panel.track_id)?;
                let projection = track.projection(panel_id)?;
                let query = projection.layout_query.clone()?;
                let cached_rows = projection.alignment_rows.as_ref()?;
                let expected_limit = allocations.get(&track.config.id).copied()?;
                let target_sort_position = alignment_sort_position(
                    track.config.filters.alignment_sort_mode,
                    track.config.filters.alignment_sort_anchor,
                    panel.viewport,
                );
                let target_row_scale_class =
                    alignment::alignment_row_scale_class(panel.viewport.bases_per_pixel());
                if !complete_horizon_preview_eligible(
                    &query,
                    panel.viewport.interval(),
                    projection.layout_query_contract_revision,
                    self.navigation_contract_revision,
                    expected_limit,
                ) || alignment::individual_read_refusal(
                    &query,
                    &query,
                    track.config.filters.read_stacking_mode,
                )
                .is_some()
                    || cached_rows.group_mode != track.config.filters.alignment_group_mode
                    || cached_rows.group_order != track.config.filters.alignment_group_order
                    || cached_rows.sort_mode != track.config.filters.alignment_sort_mode
                    || cached_rows.sort_order != track.config.filters.alignment_sort_order
                    || cached_rows.sort_position != target_sort_position
                    || cached_rows.row_scale_class != target_row_scale_class
                    || cached_rows.view_as_pairs != track.config.filters.view_as_pairs
                    || cached_rows.stacking_mode != track.config.filters.read_stacking_mode
                {
                    return None;
                }
                total_records = total_records.saturating_add(query.batch.len());
                if total_records > MAX_PROVISIONAL_HORIZON_RECORDS {
                    return None;
                }
                let reads_top = panel
                    .track_frames
                    .iter()
                    .find(|frame| {
                        frame.channel == ui::TrackPane::Reads
                            && frame.key == TrackFrameKey::Alignment(track.config.id)
                    })
                    .and_then(|frame| {
                        frame.subtracks.iter().find(|subtrack| {
                            subtrack.alignment_subtrack == Some(AlignmentSubtrack::Reads)
                        })
                    })
                    .map_or(read_panel.clip_top + ui::TRACK_HEADER_HEIGHT, |subtrack| {
                        subtrack.top
                    });
                Some(HorizonPaneInput {
                    query,
                    row_history: projection.row_history.clone(),
                    config: track.config.clone(),
                    row_revision: cached_rows.reconciliation.revision,
                    sort_position: cached_rows.sort_position,
                    promoted_read_name_hash: cached_rows.promoted_read_name_hash,
                    pair_length_bounds: cached_rows.pair_length_bounds,
                    row_scale_class: cached_rows.row_scale_class,
                    scroll: panel
                        .read_panel_scroll_state(read_panel.track_id)
                        .effective_scroll_points(),
                    clip_top: read_panel.clip_top,
                    clip_bottom: read_panel.clip_bottom,
                    reads_top,
                })
            })
            .collect::<Option<Vec<_>>>()?;

        let mut display = DisplayList::default();
        for pane in panes {
            let background = RectPrimitive {
                x: panel_rect.left(),
                y: pane.clip_top,
                width: panel_rect.width(),
                height: (pane.clip_bottom - pane.clip_top).max(0.0),
                color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                object_id: 0,
                shape: Default::default(),
            };
            if background.width > 0.0 && background.height > 0.0 {
                display.rectangles.push(background);
            }

            let filters = &pane.config.filters;
            let density = filters.alignment_density;
            let row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            let mut options = AlignmentLayoutOptions {
                reads_top: pane.reads_top,
                row_height,
                read_height: (5.0 * density).clamp(1.5, (row_height - 1.0).max(1.5)),
                max_rows: pane.query.batch.len().max(1),
                row_scale_class: pane.row_scale_class,
                row_layout_revision: pane.row_revision,
                color_mode: filters.alignment_color_mode,
                group_mode: filters.alignment_group_mode,
                group_order: filters.alignment_group_order,
                sort_mode: filters.alignment_sort_mode,
                sort_order: filters.alignment_sort_order,
                sort_position: pane.sort_position,
                show_reference: false,
                show_coverage: false,
                show_junctions: false,
                show_reads: true,
                show_modification_frequency: false,
                read_base_display_mode: filters.read_base_display_mode,
                shade_read_bases_by_quality: filters.shade_read_bases_by_quality,
                minimum_indel_size: filters.minimum_indel_size,
                minimum_event_label_size: filters.minimum_event_label_size,
                show_modified_bases: filters.show_modified_bases,
                minimum_modification_probability: filters.minimum_modification_probability,
                modified_base_color_mode: filters.modified_base_color_mode,
                view_as_pairs: filters.view_as_pairs,
                pair_length_bounds: pane.pair_length_bounds,
                promoted_read_name_hash: pane.promoted_read_name_hash,
                ..AlignmentLayoutOptions::default()
            };
            for (slot, code) in options.hidden_modification_codes.iter_mut().zip(
                filters
                    .hidden_modification_codes
                    .iter()
                    .filter_map(|value| parse_modification_code(value)),
            ) {
                *slot = Some(code);
            }

            // Reconcile against a cloned history. The provisional path must not
            // extend, evict, repack, or otherwise mutate committed row state.
            let mut preview_history = pane.row_history;
            let (rows, _) = reconcile_alignment_rows_with_horizon(
                &pane.query,
                &pane.query,
                options,
                &mut preview_history,
                filters.read_stacking_mode,
                false,
            );
            let layout =
                layout_alignment_query_with_rows(&pane.query, panel.viewport, options, &rows);
            let mut pane_display = layout.display_list;
            pane_display.texts.clear();
            remove_navigation_preview_semantics(&mut pane_display);
            let mut visible_pane = DisplayList::default();
            pane_display.append_scrolled_and_clipped_vertical(
                pane.scroll,
                pane.clip_top,
                pane.clip_bottom,
                &mut visible_pane,
            );
            visible_pane.translate_and_clip_horizontal(
                panel_rect.left(),
                panel_rect.left(),
                panel_rect.right(),
            );
            display.rectangles.extend(visible_pane.rectangles);
        }
        Some(display)
    }
}

pub(crate) fn remove_navigation_preview_semantics(display: &mut DisplayList) {
    display.texts.clear();
    for rectangle in &mut display.rectangles {
        rectangle.object_id = 0;
    }
}
