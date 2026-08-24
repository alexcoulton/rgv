use super::super::*;

#[derive(Clone)]
pub(crate) struct FilterEditor {
    pub(crate) minimum_mapq: u8,
    pub(crate) include_secondary: bool,
    pub(crate) include_supplementary: bool,
    pub(crate) expression: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ActiveFilters {
    pub(crate) alignment: AlignmentFilter,
    pub(crate) include_secondary: bool,
    pub(crate) include_supplementary: bool,
}

/// Canonical alignment state used only while no alignment controller exists.
/// Once a source is open, the first controller owns the same state directly.
pub(crate) struct EmptyAlignmentState {
    pub(crate) filters: SessionFilters,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) active_filters: ActiveFilters,
    pub(crate) name: String,
    pub(crate) visible: bool,
    pub(crate) collapsed: bool,
    pub(crate) maximum_alignments: usize,
}

pub(crate) struct CachedAlignmentRows {
    pub(crate) query: Arc<AlignmentQuery>,
    pub(crate) layout: Arc<AlignmentRowLayout>,
    pub(crate) group_mode: AlignmentGroupMode,
    pub(crate) group_order: AlignmentOrderDirection,
    pub(crate) sort_mode: AlignmentSortMode,
    pub(crate) sort_order: AlignmentOrderDirection,
    pub(crate) sort_position: Option<u64>,
    pub(crate) promoted_read_name_hash: Option<u64>,
    pub(crate) max_rows: usize,
    pub(crate) view_as_pairs: bool,
    pub(crate) pair_length_bounds: TemplateLengthBounds,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentTrackLifecycle {
    Opening,
    Ready,
    Failed(String),
}

#[derive(Default)]
pub(crate) struct AlignmentPanelProjection {
    pub(crate) clock: GenerationClock,
    pub(crate) settled_generation: Option<u64>,
    pub(crate) unavailable: Option<String>,
    pub(crate) pending_query: bool,
    pub(crate) current_query: Option<Arc<AlignmentQuery>>,
    pub(crate) query_stats: Option<QueryStats>,
    pub(crate) alignment_rows: Option<CachedAlignmentRows>,
    pub(crate) cached_layout: Option<(LayoutTileKey, Arc<AlignmentLayout>)>,
}

pub(crate) struct AlignmentTrackController {
    pub(crate) config: SessionAlignmentTrack,
    pub(crate) engine: AlignmentEngine,
    pub(crate) lifecycle: AlignmentTrackLifecycle,
    pub(crate) dictionary: Option<ReferenceDictionary>,
    pub(crate) index_overview: Option<Arc<rgv_hts::AlignmentIndexOverview>>,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) active_filters: ActiveFilters,
    pub(crate) projections: HashMap<PanelId, AlignmentPanelProjection>,
    pub(crate) cache_stats: Option<CacheStats>,
    pub(crate) remote_io_stats: Option<RemoteIoStats>,
}

pub(crate) struct AlignmentSettingsDraft {
    pub(crate) filters: SessionFilters,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) maximum_alignments: usize,
    pub(crate) apply_filter: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentSettingsOutcome {
    pub(crate) configuration_changed: bool,
    pub(crate) query_changed: bool,
}

impl AlignmentTrackController {
    pub(crate) fn prepare_query_interval(
        &mut self,
        panel_id: PanelId,
        record_allocation: usize,
        contig_name: &str,
        viewport_interval: GenomicInterval,
    ) -> Option<GenomicInterval> {
        let projection = self.projections.entry(panel_id).or_default();
        if !projection.pending_query {
            return None;
        }
        match &self.lifecycle {
            AlignmentTrackLifecycle::Opening => return None,
            AlignmentTrackLifecycle::Failed(_) => {
                projection.pending_query = false;
                return None;
            }
            AlignmentTrackLifecycle::Ready => {}
        }
        if record_allocation == 0 {
            self.mark_unavailable(
                panel_id,
                "workspace record budget is exhausted by earlier visible tracks".to_owned(),
            );
            return None;
        }
        let Some(interval) = self
            .dictionary
            .as_ref()
            .and_then(|dictionary| interval_for_track(dictionary, contig_name, viewport_interval))
        else {
            self.mark_unavailable(
                panel_id,
                format!(
                    "{contig_name}:{}-{} is not available in this alignment",
                    viewport_interval.start().saturating_add(1),
                    viewport_interval.end()
                ),
            );
            return None;
        };
        if let Some(projection) = self.projections.get_mut(&panel_id) {
            projection.unavailable = None;
        }
        Some(interval)
    }

    pub(crate) fn mark_unavailable(&mut self, panel_id: PanelId, message: String) {
        let projection = self.projections.entry(panel_id).or_default();
        projection.pending_query = false;
        projection.current_query = None;
        projection.alignment_rows = None;
        projection.cached_layout = None;
        projection.unavailable = Some(message);
        projection.settled_generation = Some(projection.clock.current().get());
    }

    pub(crate) fn projection(&self, panel_id: PanelId) -> Option<&AlignmentPanelProjection> {
        self.projections.get(&panel_id)
    }

    pub(crate) fn projection_mut(&mut self, panel_id: PanelId) -> &mut AlignmentPanelProjection {
        self.projections.entry(panel_id).or_default()
    }
}

impl Viewer {
    pub(crate) fn primary_alignment_filters(&self) -> &SessionFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.filters, |track| &track.config.filters)
    }

    pub(crate) fn primary_alignment_filters_mut(&mut self) -> &mut SessionFilters {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.filters, |track| {
                &mut track.config.filters
            })
    }

    pub(crate) fn primary_filter_editor(&self) -> &FilterEditor {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.filter_editor, |track| {
                &track.filter_editor
            })
    }

    pub(crate) fn primary_filter_editor_mut(&mut self) -> &mut FilterEditor {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.filter_editor, |track| {
                &mut track.filter_editor
            })
    }

    pub(crate) fn primary_active_filters(&self) -> &ActiveFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.active_filters, |track| {
                &track.active_filters
            })
    }

    pub(crate) fn primary_active_filters_mut(&mut self) -> &mut ActiveFilters {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.active_filters, |track| {
                &mut track.active_filters
            })
    }

    pub(crate) fn primary_alignment_name(&self) -> &str {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.name, |track| &track.config.name)
    }

    pub(crate) fn primary_alignment_visible(&self) -> bool {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.visible, |track| track.config.visible)
    }

    pub(crate) fn primary_alignment_collapsed(&self) -> bool {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.collapsed, |track| {
                track.config.collapsed
            })
    }

    pub(crate) fn primary_alignment_limit(&self) -> usize {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.maximum_alignments, |track| {
                track.config.maximum_alignments
            })
    }

    pub(crate) fn primary_alignment_source(&self) -> Option<&TrackSource> {
        self.alignment_tracks
            .first()
            .map(|track| &track.config.source)
    }

    pub(crate) fn primary_hidden_modification_codes(&self) -> Vec<ModificationCode> {
        self.primary_alignment_filters()
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value))
            .take(MAX_HIDDEN_MODIFICATION_CODES)
            .collect()
    }

    pub(crate) fn set_primary_alignment_name(&mut self, name: String) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.name = name;
        } else {
            self.empty_alignment.name = name;
        }
    }

    pub(crate) fn set_primary_alignment_visible(&mut self, visible: bool) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.visible = visible;
        } else {
            self.empty_alignment.visible = visible;
        }
    }

    pub(crate) fn set_primary_alignment_collapsed(&mut self, collapsed: bool) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.collapsed = collapsed;
        } else {
            self.empty_alignment.collapsed = collapsed;
        }
    }

    pub(crate) fn set_primary_alignment_limit(&mut self, maximum_alignments: usize) {
        let maximum_alignments = maximum_alignments
            .min(self.workspace_alignment_limit)
            .max(1);
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.maximum_alignments = maximum_alignments;
        } else {
            self.empty_alignment.maximum_alignments = maximum_alignments;
        }
    }
}

pub(crate) fn alignment_cache_share(total_bytes: usize, track_count: usize) -> NonZeroUsize {
    NonZeroUsize::new((total_bytes.max(1) / track_count.max(1)).max(1)).unwrap()
}

pub(crate) fn alignment_panel_projections(
    panel_ids: &[PanelId],
) -> HashMap<PanelId, AlignmentPanelProjection> {
    panel_ids
        .iter()
        .copied()
        .map(|id| (id, AlignmentPanelProjection::default()))
        .collect()
}

pub(crate) fn alignment_query_options(
    track: &AlignmentTrackController,
    detail_level: DetailLevel,
    record_allocation: usize,
    coverage_bins: usize,
) -> QueryOptions {
    let filters = &track.config.filters;
    QueryOptions {
        detail: if matches!(detail_level, DetailLevel::Reads | DetailLevel::Bases) {
            QueryDetail::Full
        } else {
            QueryDetail::Layout
        },
        max_records: NonZeroUsize::new(record_allocation).unwrap(),
        coverage_bins: NonZeroUsize::new(coverage_bins).unwrap(),
        collect_alleles: detail_level == DetailLevel::Bases,
        collect_modifications: filters.show_modification_frequency
            && detail_level != DetailLevel::Overview,
        overlapping_mate_policy: filters.overlapping_mate_policy,
        minimum_base_quality: filters.minimum_base_quality,
        minimum_junction_flank: filters.minimum_junction_flank,
        include_secondary: track.active_filters.include_secondary,
        include_supplementary: track.active_filters.include_supplementary,
        include_reference: alignment_query_needs_reference(
            filters.alignment_color_mode,
            detail_level,
        ),
        filter: track.active_filters.alignment.clone(),
        ..QueryOptions::default()
    }
}

pub(crate) fn alignment_query_needs_reference(
    color_mode: AlignmentColorMode,
    detail_level: DetailLevel,
) -> bool {
    detail_level == DetailLevel::Bases
        || (detail_level == DetailLevel::Reads && alignment_color_mode_needs_reference(color_mode))
}

pub(crate) const fn alignment_color_mode_needs_reference(color_mode: AlignmentColorMode) -> bool {
    matches!(
        color_mode,
        AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
    )
}

pub(crate) fn allocate_workspace_records(total_records: usize, requests: &[usize]) -> Vec<usize> {
    let mut allocations = vec![0; requests.len()];
    let mut remaining = total_records;
    let mut active = requests
        .iter()
        .enumerate()
        .filter_map(|(index, request)| (*request > 0).then_some(index))
        .collect::<Vec<_>>();

    while remaining > 0 && !active.is_empty() {
        let share = (remaining / active.len()).max(1);
        for &index in &active {
            if remaining == 0 {
                break;
            }
            let outstanding = requests[index].saturating_sub(allocations[index]);
            let grant = outstanding.min(share).min(remaining);
            allocations[index] = allocations[index].saturating_add(grant);
            remaining -= grant;
        }
        active.retain(|&index| allocations[index] < requests[index]);
    }
    allocations
}

pub(crate) fn move_ordered_item<T>(
    items: &mut Vec<T>,
    old_index: usize,
    target_index: usize,
) -> bool {
    if old_index >= items.len() {
        return false;
    }
    let new_index = target_index.min(items.len().saturating_sub(1));
    if old_index == new_index {
        return false;
    }
    let item = items.remove(old_index);
    items.insert(new_index, item);
    true
}

pub(crate) fn updated_alignment_selection(
    selected: &[TrackId],
    id: TrackId,
    additive: bool,
) -> Vec<TrackId> {
    if !additive {
        return vec![id];
    }
    let mut next = selected.to_vec();
    if let Some(index) = next.iter().position(|candidate| *candidate == id) {
        next.remove(index);
    } else {
        next.push(id);
    }
    next
}

pub(crate) fn interval_for_track(
    dictionary: &ReferenceDictionary,
    contig_name: &str,
    viewport_interval: GenomicInterval,
) -> Option<GenomicInterval> {
    let contig_id = dictionary.id_by_name(contig_name)?;
    let contig_length = dictionary.get(contig_id)?.length();
    let start = viewport_interval.start();
    if start >= contig_length {
        return None;
    }
    GenomicInterval::new(contig_id, start, viewport_interval.end().min(contig_length)).ok()
}

pub(crate) fn commit_alignment_settings_draft(
    config: &mut SessionAlignmentTrack,
    filter_editor: &mut FilterEditor,
    active_filters: &mut ActiveFilters,
    mut update: AlignmentSettingsDraft,
    workspace_limit: usize,
) -> AlignmentSettingsOutcome {
    let old_filters = config.filters.clone();
    let old_limit = config.maximum_alignments;
    let mut query_changed = old_filters.minimum_base_quality != update.filters.minimum_base_quality
        || old_filters.overlapping_mate_policy != update.filters.overlapping_mate_policy
        || old_filters.show_modification_frequency != update.filters.show_modification_frequency
        || old_filters.minimum_junction_flank != update.filters.minimum_junction_flank
        || alignment_color_mode_needs_reference(old_filters.alignment_color_mode)
            != alignment_color_mode_needs_reference(update.filters.alignment_color_mode);
    if update.apply_filter {
        let expressions = update
            .filter_editor
            .expression
            .split(';')
            .map(str::trim)
            .filter(|expression| !expression.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let parsed = expressions
            .iter()
            .map(|expression| expression.parse::<AuxPredicate>())
            .collect::<Result<Vec<_>, _>>();
        match parsed {
            Ok(auxiliary) => {
                let next_active = ActiveFilters {
                    alignment: AlignmentFilter {
                        minimum_mapping_quality: (update.filter_editor.minimum_mapq > 0)
                            .then_some(update.filter_editor.minimum_mapq),
                        auxiliary,
                        ..AlignmentFilter::default()
                    },
                    include_secondary: update.filter_editor.include_secondary,
                    include_supplementary: update.filter_editor.include_supplementary,
                };
                query_changed |= active_filters.alignment.fingerprint()
                    != next_active.alignment.fingerprint()
                    || active_filters.include_secondary != next_active.include_secondary
                    || active_filters.include_supplementary != next_active.include_supplementary;
                update.filters.minimum_mapping_quality =
                    next_active.alignment.minimum_mapping_quality;
                update.filters.include_secondary = next_active.include_secondary;
                update.filters.include_supplementary = next_active.include_supplementary;
                update.filters.auxiliary_predicates = expressions;
                *active_filters = next_active;
                update.filter_editor.error = None;
            }
            Err(error) => update.filter_editor.error = Some(error.to_string()),
        }
    }
    let filters_changed = old_filters != update.filters;
    let limit = update.maximum_alignments.min(workspace_limit).max(1);
    query_changed |= old_limit != limit;
    *filter_editor = update.filter_editor;
    config.filters = update.filters;
    config.maximum_alignments = limit;
    AlignmentSettingsOutcome {
        configuration_changed: filters_changed || old_limit != limit,
        query_changed,
    }
}

pub(crate) fn copy_compatible_alignment_appearance(
    target: &mut SessionFilters,
    source: &SessionFilters,
) {
    target.alignment_color_mode = source.alignment_color_mode;
    target.alignment_group_mode = source.alignment_group_mode;
    target.alignment_group_order = source.alignment_group_order;
    target.alignment_sort_mode = source.alignment_sort_mode;
    target.alignment_sort_order = source.alignment_sort_order;
    target.view_as_pairs = source.view_as_pairs;
    target.minimum_template_length = source.minimum_template_length;
    target.maximum_template_length = source.maximum_template_length;
    target.show_reference = source.show_reference;
    target.show_coverage = source.show_coverage;
    target.show_junctions = source.show_junctions;
    target.minimum_junction_depth = source.minimum_junction_depth;
    target.junction_display_mode = source.junction_display_mode;
    target.sashimi_scale_mode = source.sashimi_scale_mode;
    target.sashimi_fixed_scale_maximum = source.sashimi_fixed_scale_maximum;
    target.sashimi_count_mode = source.sashimi_count_mode;
    target.show_junction_labels = source.show_junction_labels;
    target.show_reads = source.show_reads;
    target.coverage_scale_maximum = source.coverage_scale_maximum;
    target.show_coverage_center_line = source.show_coverage_center_line;
    target.alignment_density = source.alignment_density;
    target.inspector_activation = source.inspector_activation;
    target.read_base_display_mode = source.read_base_display_mode;
    target.shade_read_bases_by_quality = source.shade_read_bases_by_quality;
    target.minimum_indel_size = source.minimum_indel_size;
    target.minimum_event_label_size = source.minimum_event_label_size;
    target.show_modified_bases = source.show_modified_bases;
    target.show_modification_frequency = source.show_modification_frequency;
    target.minimum_modification_probability = source.minimum_modification_probability;
    target.modified_base_color_mode = source.modified_base_color_mode;
    target
        .hidden_modification_codes
        .clone_from(&source.hidden_modification_codes);
}

pub(crate) fn spawn_alignment_track_controller(
    config: SessionAlignmentTrack,
    cache_bytes: NonZeroUsize,
    reference_path: Option<PathBuf>,
    remote_reference_index_url: Option<String>,
    panel_ids: &[PanelId],
) -> Result<AlignmentTrackController> {
    let (filter_editor, active_filters) = filter_state_from_session(&config.filters)?;
    let engine = AlignmentEngine::spawn_with_cache_budget(8, cache_bytes);
    engine.try_open(
        source_path(&config.source),
        alignment_open_options(
            reference_path,
            config.remote_index_url.clone(),
            remote_reference_index_url,
        ),
    )?;
    Ok(AlignmentTrackController {
        config,
        engine,
        lifecycle: AlignmentTrackLifecycle::Opening,
        dictionary: None,
        index_overview: None,
        filter_editor,
        active_filters,
        projections: alignment_panel_projections(panel_ids),
        cache_stats: None,
        remote_io_stats: None,
    })
}

pub(crate) fn initial_filters(
    session: Option<&SessionDocument>,
) -> Result<(FilterEditor, ActiveFilters)> {
    let filters = session
        .and_then(primary_alignment_track)
        .map_or_else(SessionFilters::default, |track| track.filters.clone());
    filter_state_from_session(&filters)
}

pub(crate) fn filter_state_from_session(
    filters: &SessionFilters,
) -> Result<(FilterEditor, ActiveFilters)> {
    let active = active_filters_from_session(filters)?;
    let expression = filters.auxiliary_predicates.join(";");
    Ok((
        FilterEditor {
            minimum_mapq: filters.minimum_mapping_quality.unwrap_or_default(),
            include_secondary: filters.include_secondary,
            include_supplementary: filters.include_supplementary,
            expression,
            error: None,
        },
        active,
    ))
}

pub(crate) fn active_filters_from_session(filters: &SessionFilters) -> Result<ActiveFilters> {
    let auxiliary = filters
        .auxiliary_predicates
        .iter()
        .map(|expression| expression.parse::<AuxPredicate>())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ActiveFilters {
        alignment: AlignmentFilter {
            minimum_mapping_quality: filters.minimum_mapping_quality,
            auxiliary,
            ..AlignmentFilter::default()
        },
        include_secondary: filters.include_secondary,
        include_supplementary: filters.include_supplementary,
    })
}
