#[allow(clippy::wildcard_imports)]
use super::*;

/// Canonical repack comparison is diagnostic validation, not part of the
/// authoritative row result. Sorting and repacking a million-record horizon a
/// second time on every extension can dominate the interaction by hundreds of
/// milliseconds, so large comparisons are left to qualification jobs.
pub(super) const MAXIMUM_LIVE_CANONICAL_METRIC_RECORDS: usize = 100_000;

pub fn assign_alignment_rows(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    previous: Option<&AlignmentRowLayout>,
) -> AlignmentRowLayout {
    assign_alignment_rows_internal(query, options, previous, true)
}

fn assign_alignment_rows_internal(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    previous: Option<&AlignmentRowLayout>,
    deterministic_ties: bool,
) -> AlignmentRowLayout {
    let pairs = pair_layout_metadata(query, options.view_as_pairs);
    let groups = grouped_indices_internal(
        query,
        options.group_mode,
        options.sort_mode,
        options.sort_order,
        effective_sort_position(&options),
        options.pair_length_bounds,
        options.promoted_read_name_hash,
        deterministic_ties,
    );
    let ordered_groups = ordered_group_entries(&groups, options.group_order);
    let group_count = groups.len().max(1);
    let max_rows = options.max_rows.max(1);
    // When the caller provides enough capacity for every retained alignment,
    // grouping must not manufacture a smaller per-group display cap. A group
    // can consume as many rows as it needs and later groups use the remaining
    // global capacity. Preserve the equal-share policy only for explicitly
    // capacity-limited layouts such as fixed-height secondary tracks.
    let rows_per_group = if max_rows >= query.batch.len() {
        max_rows
    } else {
        (max_rows / group_count).max(1)
    };
    let identities = alignment_row_identities(query, &pairs);
    let mut rows = vec![None; query.batch.len()];
    let mut group_starts = BTreeMap::new();
    let compatible_previous =
        previous.filter(|layout| layout.compatible_with(&options, query.batch.len()));

    if let Some(previous) = compatible_previous {
        assign_rows_with_history(
            &ordered_groups,
            &pairs,
            rows_per_group,
            max_rows,
            &identities,
            previous,
            &mut rows,
            &mut group_starts,
        );
    } else {
        assign_rows_from_scratch(
            &ordered_groups,
            &pairs,
            rows_per_group,
            max_rows,
            &mut rows,
            &mut group_starts,
        );
    }
    copy_pair_rows(&pairs, &mut rows);

    AlignmentRowLayout {
        rows: rows.into(),
        identities: identities.into(),
        physical_indices: None,
        logical_indices_by_physical: None,
        records_by_row: None,
        pair_representatives: options
            .view_as_pairs
            .then_some(pairs.representatives)
            .unwrap_or_default(),
        group_starts,
        group_mode: options.group_mode,
        group_order: options.group_order,
        sort_mode: options.sort_mode,
        sort_order: options.sort_order,
        sort_position: effective_sort_position(&options),
        promoted_read_name_hash: options.promoted_read_name_hash,
        max_rows,
        view_as_pairs: options.view_as_pairs,
    }
}

/// Reconciles one settled query against persistent row history.
///
/// Stable mode freezes every assignment already known to the active layout
/// island and places only genuinely new identities. Compact mode deliberately
/// ignores (and never mutates) stable history. `repack_stable` replaces the
/// active stable island with a fresh compact baseline.
pub fn reconcile_alignment_rows(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    history: &mut AlignmentRowHistory,
    stacking_mode: ReadStackingMode,
    repack_stable: bool,
) -> (AlignmentRowLayout, AlignmentRowReconciliationStats) {
    reconcile_alignment_rows_with_horizon(
        query,
        query,
        options,
        history,
        stacking_mode,
        repack_stable,
    )
}

/// Reconciles a visible query against a fixed-neighbourhood layout query.
///
/// In stable mode the layout query establishes or extends the persistent row
/// architecture, then the visible query is projected onto those rows. Compact
/// mode remains intentionally viewport-local and ignores the layout query.
pub fn reconcile_alignment_rows_with_horizon(
    query: &AlignmentQuery,
    layout_query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    history: &mut AlignmentRowHistory,
    stacking_mode: ReadStackingMode,
    repack_stable: bool,
) -> (AlignmentRowLayout, AlignmentRowReconciliationStats) {
    reconcile_alignment_rows_with_horizon_internal(
        query,
        layout_query,
        options,
        history,
        stacking_mode,
        repack_stable,
        &|| false,
    )
    .map(|(layout, stats, _)| (layout, stats))
    .expect("non-cancellable alignment reconciliation completes")
}

/// Reconciles rows on an owned candidate history and returns nothing once a
/// newer generation cancels the work. Because the candidate is consumed, a
/// cancelled sweep can never partially mutate the caller's committed history.
pub fn reconcile_alignment_rows_with_horizon_cancellable<F>(
    query: &AlignmentQuery,
    layout_query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    mut candidate_history: AlignmentRowHistory,
    stacking_mode: ReadStackingMode,
    repack_stable: bool,
    is_cancelled: F,
) -> Option<(
    AlignmentRowLayout,
    AlignmentRowReconciliationStats,
    AlignmentRowHistory,
)>
where
    F: Fn() -> bool,
{
    reconcile_alignment_rows_with_horizon_internal(
        query,
        layout_query,
        options,
        &mut candidate_history,
        stacking_mode,
        repack_stable,
        &is_cancelled,
    )
    .map(|(layout, stats, _)| (layout, stats, candidate_history))
}

pub fn reconcile_alignment_rows_with_horizon_scene_cancellable<F>(
    query: &AlignmentQuery,
    layout_query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    mut candidate_history: AlignmentRowHistory,
    stacking_mode: ReadStackingMode,
    repack_stable: bool,
    is_cancelled: F,
) -> Option<(
    AlignmentRowLayout,
    AlignmentRowReconciliationStats,
    AlignmentRowHistory,
    Option<AlignmentRowScene>,
)>
where
    F: Fn() -> bool,
{
    reconcile_alignment_rows_with_horizon_internal(
        query,
        layout_query,
        options,
        &mut candidate_history,
        stacking_mode,
        repack_stable,
        &is_cancelled,
    )
    .map(|(layout, stats, scene)| (layout, stats, candidate_history, scene))
}

fn reconcile_alignment_rows_with_horizon_internal(
    query: &AlignmentQuery,
    layout_query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    history: &mut AlignmentRowHistory,
    stacking_mode: ReadStackingMode,
    repack_stable: bool,
    is_cancelled: &impl Fn() -> bool,
) -> Option<(
    AlignmentRowLayout,
    AlignmentRowReconciliationStats,
    Option<AlignmentRowScene>,
)> {
    let reconciliation_started = std::time::Instant::now();
    if is_cancelled() {
        return None;
    }
    if stacking_mode == ReadStackingMode::Compact {
        let allocation_started = std::time::Instant::now();
        let layout = assign_alignment_rows_internal(query, options, None, true);
        if is_cancelled() {
            return None;
        }
        let row_allocation_micros = elapsed_micros(allocation_started);
        let metrics_started = std::time::Instant::now();
        let canonical_metrics_deferred = query.batch.len() > MAXIMUM_LIVE_CANONICAL_METRIC_RECORDS;
        let organization = if canonical_metrics_deferred {
            alignment_row_organization_metrics_from_layout(&layout)
        } else {
            alignment_row_organization_metrics(query, options, &layout)
        };
        let canonical_metrics_micros = elapsed_micros(metrics_started);
        let stats = AlignmentRowReconciliationStats {
            revision: history.revision(),
            architecture_version: organization.architecture_version,
            reconciliation_micros: elapsed_micros(reconciliation_started),
            row_allocation_micros,
            canonical_metrics_micros,
            canonical_metrics_deferred,
            rows_appended: representative_row_count(&layout),
            added: representative_row_count(&layout),
            contexts: history.context_count(),
            islands: history.island_count(),
            assignments: history.assignment_count(),
            history_bytes: history.estimated_bytes(),
            compact: true,
            evictions: history.eviction_count(),
            transition_reason: ReadStackingTransitionReason::CompactMode,
            policy_fingerprint: alignment_row_policy_fingerprint(&options),
            projection_fingerprint: layout.assignment_fingerprint(),
            stable_rows: layout.used_row_count(),
            canonical_rows: organization.canonical_rows,
            row_inflation: organization.row_inflation,
            canonical_displacement: organization.canonical_displacement,
            top_first_violations: organization.top_first_violations,
            query_occupied_row_span: organization.occupied_row_span,
            query_empty_rows: organization.empty_rows,
            query_empty_ratio_ppm: organization.empty_ratio_ppm,
            horizon_start: query.interval.start(),
            horizon_end: query.interval.end(),
            horizon_records: query.batch.len(),
            horizon_bytes: query.estimated_bytes(),
            horizon_truncated: !query.individual_reads_complete(),
            ..AlignmentRowReconciliationStats::default()
        };
        let scene = AlignmentRowScene::new(query, &layout, &options);
        return (!is_cancelled()).then_some((layout, stats, scene));
    }

    history.revision = history.revision.saturating_add(1);
    let revision = history.revision;
    let policy = AlignmentRowPolicy::from_options(&options);
    let existing_assignment_count = history.assignment_count();
    let existing_context = history.contexts.iter().position(|context| {
        context.contig == layout_query.interval.contig() && context.policy == policy
    });
    let created_context = existing_context.is_none();
    let context_index = existing_context.unwrap_or_else(|| {
        history.contexts.push(AlignmentRowHistoryContext {
            contig: layout_query.interval.contig(),
            policy: policy.clone(),
            islands: Vec::new(),
        });
        history.contexts.len() - 1
    });

    let (
        mut layout,
        retained,
        restored,
        mut added,
        mut reservations,
        repacked,
        connected_history,
        horizon_extension,
        mut work,
    ) = {
        let context = &mut history.contexts[context_index];
        let mut connected = context
            .islands
            .iter()
            .enumerate()
            .filter_map(|(index, island)| {
                intervals_connect(island.interval, layout_query.interval).then_some(index)
            })
            .collect::<Vec<_>>();

        if repack_stable {
            for index in connected.iter().rev().copied() {
                context.islands.remove(index);
            }
            connected.clear();
        }

        let connected_interval = connected
            .iter()
            .copied()
            .map(|index| context.islands[index].interval)
            .reduce(interval_union);
        let horizon_extension = connected_interval
            .is_some_and(|covered| !interval_contains(covered, layout_query.interval));

        if connected.is_empty() {
            let allocation_started = std::time::Instant::now();
            let mut layout = assign_alignment_rows_internal(layout_query, options, None, true);
            if is_cancelled() {
                return None;
            }
            let row_allocation_micros = elapsed_micros(allocation_started);
            let commit_started = std::time::Instant::now();
            let added = seed_history_island(
                context,
                layout_query,
                &layout,
                options,
                revision,
                is_cancelled,
            )?;
            let history_commit_micros = elapsed_micros(commit_started);
            layout.max_rows = layout.used_row_count().max(1);
            (
                layout,
                0,
                0,
                added,
                0,
                repack_stable,
                false,
                false,
                HistoryReconciliationWork {
                    row_allocation_micros,
                    history_commit_micros,
                    rows_appended: added,
                    ..HistoryReconciliationWork::default()
                },
            )
        } else {
            let island_index = merge_connected_islands(
                context,
                &connected,
                layout_query.interval,
                !layout_query.individual_reads_complete(),
                horizon_extension,
                revision,
            );
            let (layout, retained, restored, added, reservations, work) = reconcile_history_island(
                &mut context.islands[island_index],
                layout_query,
                options,
                revision,
                is_cancelled,
            )?;
            (
                layout,
                retained,
                restored,
                added,
                reservations,
                false,
                true,
                horizon_extension,
                work,
            )
        }
    };

    let scene = AlignmentRowScene::new(layout_query, &layout, &options);

    if !std::ptr::eq(query, layout_query) {
        let context = &mut history.contexts[context_index];
        let island_index = context
            .islands
            .iter()
            .position(|island| interval_contains(island.interval, query.interval))
            .expect("the visible query must be contained by its layout horizon");
        let (visible_layout, _, _, visible_added, visible_reservations, visible_work) =
            reconcile_history_island(
                &mut context.islands[island_index],
                query,
                options,
                revision,
                is_cancelled,
            )?;
        layout = visible_layout;
        added = added.saturating_add(visible_added);
        reservations = visible_reservations;
        work.add_assign(visible_work);
    }

    let (horizon_interval, horizon_generation, horizon_extensions, horizon_truncated) = history
        .contexts
        .get(context_index)
        .and_then(|context| {
            context
                .islands
                .iter()
                .find(|island| interval_contains(island.interval, layout_query.interval))
        })
        .map(|island| {
            (
                island.interval,
                island.horizon_generation,
                island.horizon_extensions,
                island.horizon_truncated,
            )
        })
        .expect("the active layout horizon must remain in row history");
    enforce_history_budget(history, revision);
    if is_cancelled() {
        return None;
    }
    let transition_reason = if repacked {
        ReadStackingTransitionReason::ExplicitRebuild
    } else if horizon_extension {
        ReadStackingTransitionReason::HorizonExtension
    } else if connected_history {
        ReadStackingTransitionReason::Navigation
    } else if created_context && existing_assignment_count > 0 {
        ReadStackingTransitionReason::PolicyContext
    } else {
        ReadStackingTransitionReason::InitialCanonical
    };
    let metrics_started = std::time::Instant::now();
    let canonical_metrics_deferred =
        layout_query.batch.len() > MAXIMUM_LIVE_CANONICAL_METRIC_RECORDS;
    let organization = if canonical_metrics_deferred {
        alignment_row_organization_metrics_from_layout(&layout)
    } else {
        alignment_row_organization_metrics(query, options, &layout)
    };
    if is_cancelled() {
        return None;
    }
    let canonical_metrics_micros = elapsed_micros(metrics_started);

    let stats = AlignmentRowReconciliationStats {
        revision,
        architecture_version: organization.architecture_version,
        reconciliation_micros: elapsed_micros(reconciliation_started),
        incumbent_projection_micros: work.incumbent_projection_micros,
        history_index_micros: work.history_index_micros,
        row_allocation_micros: work.row_allocation_micros,
        history_commit_micros: work.history_commit_micros,
        canonical_metrics_micros,
        canonical_metrics_deferred,
        scene_projection_micros: 0,
        scene_hit: false,
        scene_physical_hit: false,
        scene_extended: false,
        exact_camera_hit: false,
        compatibility_probes: work.compatibility_probes,
        rows_reused: work.rows_reused,
        rows_appended: work.rows_appended,
        retained,
        restored,
        added,
        moved: 0,
        reservations,
        contexts: history.context_count(),
        islands: history.island_count(),
        assignments: history.assignment_count(),
        history_bytes: history.estimated_bytes(),
        compact: false,
        repacked,
        evictions: history.eviction_count(),
        transition_reason,
        policy_fingerprint: alignment_row_policy_fingerprint(&options),
        projection_fingerprint: layout.assignment_fingerprint(),
        stable_rows: layout.row_capacity(),
        canonical_rows: organization.canonical_rows,
        row_inflation: organization.row_inflation,
        canonical_displacement: organization.canonical_displacement,
        top_first_violations: organization.top_first_violations,
        query_occupied_row_span: organization.occupied_row_span,
        query_empty_rows: organization.empty_rows,
        query_empty_ratio_ppm: organization.empty_ratio_ppm,
        horizon_start: horizon_interval.start(),
        horizon_end: horizon_interval.end(),
        horizon_generation,
        horizon_extensions,
        horizon_records: layout_query.batch.len(),
        horizon_bytes: layout_query.estimated_bytes(),
        horizon_truncated,
    };
    Some((layout, stats, scene))
}

pub fn alignment_row_organization_metrics(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    layout: &AlignmentRowLayout,
) -> AlignmentRowOrganizationMetrics {
    let canonical = assign_alignment_rows_internal(query, options, None, true);
    let canonical_rows_by_identity = canonical
        .identity_rows()
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .collect::<HashMap<_, _>>();
    let (canonical_displacement, top_first_violations) = layout
        .identity_rows()
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .fold((0_usize, 0_usize), |(displaced, lower), (identity, row)| {
            let canonical_row = canonical_rows_by_identity.get(&identity).copied();
            (
                displaced + usize::from(canonical_row != Some(row)),
                lower + usize::from(canonical_row.is_some_and(|canonical| row > canonical)),
            )
        });
    let occupied = (0..layout.len())
        .filter(|index| layout.representative(*index) == *index)
        .filter_map(|index| layout.row(index))
        .collect::<BTreeSet<_>>();
    let occupied_row_span = occupied
        .first()
        .zip(occupied.last())
        .map_or(0, |(first, last)| {
            last.saturating_sub(*first).saturating_add(1)
        });
    let empty_rows = occupied_row_span.saturating_sub(occupied.len());
    let empty_ratio_ppm = if occupied_row_span == 0 {
        0
    } else {
        u32::try_from(
            empty_rows
                .saturating_mul(1_000_000)
                .checked_div(occupied_row_span)
                .unwrap_or_default(),
        )
        .unwrap_or(u32::MAX)
    };
    AlignmentRowOrganizationMetrics {
        architecture_version: ALIGNMENT_ROW_ARCHITECTURE_VERSION,
        canonical_rows: canonical.used_row_count(),
        stable_rows: layout.used_row_count(),
        row_inflation: layout
            .used_row_count()
            .saturating_sub(canonical.used_row_count()),
        canonical_displacement,
        top_first_violations,
        occupied_row_span,
        empty_rows,
        empty_ratio_ppm,
    }
}

fn alignment_row_organization_metrics_from_layout(
    layout: &AlignmentRowLayout,
) -> AlignmentRowOrganizationMetrics {
    let occupied = (0..layout.len())
        .filter(|index| layout.representative(*index) == *index)
        .filter_map(|index| layout.row(index))
        .collect::<BTreeSet<_>>();
    let occupied_row_span = occupied
        .first()
        .zip(occupied.last())
        .map_or(0, |(first, last)| {
            last.saturating_sub(*first).saturating_add(1)
        });
    let empty_rows = occupied_row_span.saturating_sub(occupied.len());
    let empty_ratio_ppm = if occupied_row_span == 0 {
        0
    } else {
        u32::try_from(
            empty_rows
                .saturating_mul(1_000_000)
                .checked_div(occupied_row_span)
                .unwrap_or_default(),
        )
        .unwrap_or(u32::MAX)
    };
    let stable_rows = layout.used_row_count();
    AlignmentRowOrganizationMetrics {
        architecture_version: ALIGNMENT_ROW_ARCHITECTURE_VERSION,
        // Deferred values are explicitly marked on reconciliation telemetry.
        // Use the committed organization as the neutral comparison baseline.
        canonical_rows: stable_rows,
        stable_rows,
        row_inflation: 0,
        canonical_displacement: 0,
        top_first_violations: 0,
        occupied_row_span,
        empty_rows,
        empty_ratio_ppm,
    }
}

fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Default)]
struct HistoryReconciliationWork {
    incumbent_projection_micros: u64,
    history_index_micros: u64,
    row_allocation_micros: u64,
    history_commit_micros: u64,
    compatibility_probes: usize,
    rows_reused: usize,
    rows_appended: usize,
}

impl HistoryReconciliationWork {
    fn add_assign(&mut self, other: Self) {
        self.incumbent_projection_micros = self
            .incumbent_projection_micros
            .saturating_add(other.incumbent_projection_micros);
        self.history_index_micros = self
            .history_index_micros
            .saturating_add(other.history_index_micros);
        self.row_allocation_micros = self
            .row_allocation_micros
            .saturating_add(other.row_allocation_micros);
        self.history_commit_micros = self
            .history_commit_micros
            .saturating_add(other.history_commit_micros);
        self.compatibility_probes = self
            .compatibility_probes
            .saturating_add(other.compatibility_probes);
        self.rows_reused = self.rows_reused.saturating_add(other.rows_reused);
        self.rows_appended = self.rows_appended.saturating_add(other.rows_appended);
    }
}

pub fn alignment_row_policy_fingerprint(options: &AlignmentLayoutOptions) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ALIGNMENT_ROW_ARCHITECTURE_VERSION.hash(&mut hasher);
    AlignmentRowPolicy::from_options(options).hash(&mut hasher);
    hasher.finish()
}

fn enforce_history_budget(history: &mut AlignmentRowHistory, protected_revision: u64) {
    while history.estimated_bytes() > DEFAULT_ALIGNMENT_ROW_HISTORY_BUDGET_BYTES {
        let candidate = history
            .contexts
            .iter()
            .enumerate()
            .flat_map(|(context_index, context)| {
                context
                    .islands
                    .iter()
                    .enumerate()
                    .map(move |(island_index, island)| {
                        (context_index, island_index, island.last_used_revision)
                    })
            })
            .filter(|(_, _, last_used)| *last_used != protected_revision)
            .filter(|(context_index, island_index, _)| {
                !history.contexts[*context_index].islands[*island_index].navigation_pinned
            })
            .min_by_key(|(_, _, last_used)| *last_used);
        let Some((context_index, island_index, _)) = candidate else {
            break;
        };
        history.contexts[context_index].islands.remove(island_index);
        history.evictions = history.evictions.saturating_add(1);
    }
    history
        .contexts
        .retain(|context| !context.islands.is_empty());
}

fn representative_row_count(layout: &AlignmentRowLayout) -> usize {
    (0..layout.len())
        .filter(|index| layout.representative(*index) == *index && layout.row(*index).is_some())
        .count()
}

fn intervals_connect(left: rgv_core::GenomicInterval, right: rgv_core::GenomicInterval) -> bool {
    left.contig() == right.contig() && left.start() <= right.end() && right.start() <= left.end()
}

fn interval_union(
    left: rgv_core::GenomicInterval,
    right: rgv_core::GenomicInterval,
) -> rgv_core::GenomicInterval {
    debug_assert_eq!(left.contig(), right.contig());
    rgv_core::GenomicInterval::new(
        left.contig(),
        left.start().min(right.start()),
        left.end().max(right.end()),
    )
    .expect("the union of valid same-contig intervals is valid")
}

fn interval_contains(outer: rgv_core::GenomicInterval, inner: rgv_core::GenomicInterval) -> bool {
    outer.contig() == inner.contig() && outer.start() <= inner.start() && outer.end() >= inner.end()
}

fn row_inputs(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
) -> (
    PairLayoutMetadata,
    Vec<AlignmentRowIdentity>,
    BTreeMap<AlignmentGroupKey, Vec<usize>>,
    Vec<AlignmentGroupKey>,
) {
    let pairs = pair_layout_metadata(query, options.view_as_pairs);
    let identities = alignment_row_identities(query, &pairs);
    let groups = grouped_indices_internal(
        query,
        options.group_mode,
        options.sort_mode,
        options.sort_order,
        effective_sort_position(&options),
        options.pair_length_bounds,
        options.promoted_read_name_hash,
        true,
    );
    let mut group_by_index = vec![AlignmentGroupKey::All; query.batch.len()];
    for (group, indices) in &groups {
        for &index in indices {
            group_by_index[index] = group.clone();
        }
    }
    (pairs, identities, groups, group_by_index)
}

fn seed_history_island(
    context: &mut AlignmentRowHistoryContext,
    query: &AlignmentQuery,
    layout: &AlignmentRowLayout,
    options: AlignmentLayoutOptions,
    revision: u64,
    is_cancelled: &impl Fn() -> bool,
) -> Option<usize> {
    let (pairs, identities, _, group_by_index) = row_inputs(query, options);
    let mut assignments = HashMap::new();
    let mut row_groups = BTreeMap::<usize, BTreeSet<AlignmentGroupKey>>::new();
    let mut added = 0;
    for index in 0..query.batch.len() {
        if cancellation_checkpoint(index, is_cancelled) {
            return None;
        }
        if pairs.representatives[index] != index {
            continue;
        }
        let Some(row) = layout.row(index) else {
            continue;
        };
        let group = group_by_index[index].clone();
        row_groups.entry(row).or_default().insert(group.clone());
        assignments.insert(
            identities[index],
            HistoricalRowAssignment {
                group,
                row,
                start: pairs.starts[index],
                end: pairs.ends[index],
                last_seen_revision: revision,
            },
        );
        added += 1;
    }
    context.islands.push(AlignmentRowIsland {
        interval: query.interval,
        horizon_generation: 1,
        horizon_extensions: 0,
        horizon_truncated: !query.individual_reads_complete(),
        assignments,
        row_groups,
        last_used_revision: revision,
        navigation_pinned: false,
    });
    Some(added)
}

fn merge_connected_islands(
    context: &mut AlignmentRowHistoryContext,
    connected: &[usize],
    query_interval: rgv_core::GenomicInterval,
    query_truncated: bool,
    horizon_extension: bool,
    revision: u64,
) -> usize {
    let first = connected[0];
    let mut merged_interval = query_interval;
    let mut assignments = HashMap::new();
    let mut row_groups = BTreeMap::<usize, BTreeSet<AlignmentGroupKey>>::new();
    let mut navigation_pinned = false;
    let mut horizon_generation = 1_u64;
    let mut horizon_extensions = 0_usize;
    let mut horizon_truncated = query_truncated;
    for index in connected.iter().rev().copied() {
        let island = context.islands.remove(index);
        navigation_pinned |= island.navigation_pinned;
        horizon_generation = horizon_generation.max(island.horizon_generation);
        horizon_extensions = horizon_extensions.saturating_add(island.horizon_extensions);
        horizon_truncated |= island.horizon_truncated;
        merged_interval = interval_union(merged_interval, island.interval);
        for (identity, assignment) in island.assignments {
            assignments
                .entry(identity)
                .and_modify(|existing: &mut HistoricalRowAssignment| {
                    if assignment.last_seen_revision > existing.last_seen_revision {
                        *existing = assignment.clone();
                    }
                })
                .or_insert(assignment);
        }
        for (row, groups) in island.row_groups {
            row_groups.entry(row).or_default().extend(groups);
        }
    }
    let insertion = first.min(context.islands.len());
    context.islands.insert(
        insertion,
        AlignmentRowIsland {
            interval: merged_interval,
            horizon_generation: horizon_generation.saturating_add(u64::from(horizon_extension)),
            horizon_extensions: horizon_extensions.saturating_add(usize::from(horizon_extension)),
            horizon_truncated,
            assignments,
            row_groups,
            last_used_revision: revision,
            navigation_pinned,
        },
    );
    insertion
}

fn reconcile_history_island(
    island: &mut AlignmentRowIsland,
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    revision: u64,
    is_cancelled: &impl Fn() -> bool,
) -> Option<(
    AlignmentRowLayout,
    usize,
    usize,
    usize,
    usize,
    HistoryReconciliationWork,
)> {
    island.last_used_revision = revision;
    let preparation_started = std::time::Instant::now();
    let (pairs, identities, groups, group_by_index) = row_inputs(query, options);
    if is_cancelled() {
        return None;
    }
    let mut rows = vec![None; query.batch.len()];
    let mut retained = 0;
    let mut restored = 0;
    let mut current_identities = BTreeSet::new();
    let mut work = HistoryReconciliationWork::default();

    // A refused/truncated canonical horizon can legitimately establish an
    // empty island before a Complete foreground arrives. Seeding that first
    // real batch through the historical insertion path used to scan every
    // previously appended row for every read: a dense n-read clique therefore
    // performed roughly n²/2 compatibility checks. There are no incumbents to
    // preserve in an empty island, so use the canonical scratch sweep directly
    // and establish history from its result in one pass.
    if island.assignments.is_empty() {
        let ordered_groups = ordered_group_entries(&groups, options.group_order);
        let group_count = groups.len().max(1);
        let max_rows = options.max_rows.max(1);
        let rows_per_group = if max_rows >= query.batch.len() {
            max_rows
        } else {
            (max_rows / group_count).max(1)
        };
        let mut group_starts = BTreeMap::new();
        assign_rows_from_scratch(
            &ordered_groups,
            &pairs,
            rows_per_group,
            max_rows,
            &mut rows,
            &mut group_starts,
        );
        copy_pair_rows(&pairs, &mut rows);
        // Identity construction, grouping, canonical ordering, and row choice
        // are all part of allocation for a first real batch. Keep them in one
        // stage measurement rather than reporting only the final heap sweep.
        work.row_allocation_micros = elapsed_micros(preparation_started);

        let commit_started = std::time::Instant::now();
        let mut added = 0;
        for index in 0..query.batch.len() {
            if cancellation_checkpoint(index, is_cancelled) {
                return None;
            }
            if pairs.representatives[index] != index {
                continue;
            }
            let Some(row) = rows[index] else {
                continue;
            };
            let identity = identities[index];
            let group = group_by_index[index].clone();
            island
                .row_groups
                .entry(row)
                .or_default()
                .insert(group.clone());
            island.assignments.insert(
                identity,
                HistoricalRowAssignment {
                    group,
                    row,
                    start: pairs.starts[index],
                    end: pairs.ends[index],
                    last_seen_revision: revision,
                },
            );
            current_identities.insert(identity);
            added += 1;
        }
        work.history_commit_micros = elapsed_micros(commit_started);
        work.rows_appended = added;
        let effective_max_rows = island
            .row_groups
            .last_key_value()
            .map_or(1, |(&row, _)| row.saturating_add(1));
        return Some((
            AlignmentRowLayout {
                rows: rows.into(),
                identities: identities.into(),
                physical_indices: None,
                logical_indices_by_physical: None,
                records_by_row: None,
                pair_representatives: options
                    .view_as_pairs
                    .then_some(pairs.representatives)
                    .unwrap_or_default(),
                group_starts,
                group_mode: options.group_mode,
                group_order: options.group_order,
                sort_mode: options.sort_mode,
                sort_order: options.sort_order,
                sort_position: effective_sort_position(&options),
                promoted_read_name_hash: options.promoted_read_name_hash,
                max_rows: effective_max_rows,
                view_as_pairs: options.view_as_pairs,
            },
            0,
            0,
            added,
            0,
            work,
        ));
    }

    // Refresh known spans before building row occupancy. Pair view can learn a
    // wider template span when the mate enters the query; the identity and row
    // remain stable while the reservation conservatively expands.
    let projection_started = std::time::Instant::now();
    for index in 0..query.batch.len() {
        if cancellation_checkpoint(index, is_cancelled) {
            return None;
        }
        if pairs.representatives[index] != index {
            continue;
        }
        let identity = identities[index];
        current_identities.insert(identity);
        if let Some(assignment) = island.assignments.get_mut(&identity) {
            // The canonical layout horizon owns the grouping decision. Some
            // pair classifications are viewport-relative, so a smaller rich
            // data query may calculate a different local label without being
            // allowed to move the read to another canonical group or row.
            if assignment.last_seen_revision.saturating_add(1) == revision {
                retained += 1;
            } else {
                restored += 1;
            }
            assignment.start = assignment.start.min(pairs.starts[index]);
            assignment.end = assignment.end.max(pairs.ends[index]);
            assignment.last_seen_revision = revision;
            rows[index] = Some(assignment.row);
        }
    }
    work.incumbent_projection_micros = elapsed_micros(projection_started);

    let index_started = std::time::Instant::now();
    let mut occupied = BTreeMap::<usize, Vec<(u64, u64, AlignmentRowIdentity)>>::new();
    for (index, (&identity, assignment)) in island.assignments.iter().enumerate() {
        if cancellation_checkpoint(index, is_cancelled) {
            return None;
        }
        occupied.entry(assignment.row).or_default().push((
            assignment.start,
            assignment.end,
            identity,
        ));
    }
    for intervals in occupied.values_mut() {
        intervals.sort_unstable_by_key(|&(start, end, identity)| {
            (
                start,
                end,
                identity.primary,
                identity.secondary,
                identity.occurrence,
            )
        });
        debug_assert!(intervals.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    }
    work.history_index_micros = elapsed_micros(index_started);

    let ordered_groups = ordered_group_entries(&groups, options.group_order);
    let mut added = 0;
    let allocation_started = std::time::Instant::now();
    for &(group, indices) in &ordered_groups {
        if is_cancelled() {
            return None;
        }
        let mut new_representatives = indices
            .iter()
            .copied()
            .filter(|&index| pairs.representatives[index] == index && rows[index].is_none())
            .collect::<Vec<_>>();
        if new_representatives.is_empty() {
            continue;
        }
        if !new_representatives
            .windows(2)
            .all(|pair| pairs.starts[pair[0]] <= pairs.starts[pair[1]])
        {
            // Stable incumbents cannot be reordered to reproduce a fresh
            // viewport-local sort. Place only the new identities by genomic
            // start so the gap sweep remains indexed; stable sorting retains
            // the selected/group sort priority for equal-start reads, which
            // is the case in which that priority determines vertical order.
            new_representatives.sort_by_key(|&index| (pairs.starts[index], pairs.ends[index]));
        }
        added += assign_monotonic_history_rows(
            group,
            &new_representatives,
            &pairs,
            &identities,
            &group_by_index,
            island,
            &mut occupied,
            &mut rows,
            revision,
            &mut work,
            is_cancelled,
        )?;
    }
    work.row_allocation_micros = work
        .row_allocation_micros
        .saturating_add(elapsed_micros(allocation_started));
    copy_pair_rows(&pairs, &mut rows);

    let mut group_starts = BTreeMap::new();
    for assignment in island.assignments.values() {
        group_starts
            .entry(assignment.group.clone())
            .and_modify(|first: &mut usize| *first = (*first).min(assignment.row))
            .or_insert(assignment.row);
    }
    let effective_max_rows = island
        .row_groups
        .last_key_value()
        .map_or(1, |(&row, _)| row.saturating_add(1));
    let reservations = island
        .assignments
        .len()
        .saturating_sub(current_identities.len());
    let layout = AlignmentRowLayout {
        rows: rows.into(),
        identities: identities.into(),
        physical_indices: None,
        logical_indices_by_physical: None,
        records_by_row: None,
        pair_representatives: options
            .view_as_pairs
            .then_some(pairs.representatives)
            .unwrap_or_default(),
        group_starts,
        group_mode: options.group_mode,
        group_order: options.group_order,
        sort_mode: options.sort_mode,
        sort_order: options.sort_order,
        sort_position: effective_sort_position(&options),
        promoted_read_name_hash: options.promoted_read_name_hash,
        max_rows: effective_max_rows,
        view_as_pairs: options.view_as_pairs,
    };
    Some((layout, retained, restored, added, reservations, work))
}

const ALIGNMENT_ROW_CANCELLATION_CHUNK: usize = 2_048;

fn cancellation_checkpoint(index: usize, is_cancelled: &impl Fn() -> bool) -> bool {
    index.is_multiple_of(ALIGNMENT_ROW_CANCELLATION_CHUNK) && is_cancelled()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RowGapEvent {
    start: u64,
    row: usize,
    end: u64,
}

/// Segment tree that returns the visually highest row whose currently active
/// free gap ends at or after a requested alignment end.
///
/// Gap starts are handled by the event sweep in
/// `assign_monotonic_history_rows`; each leaf therefore needs only the active
/// gap end. Interior maximums let an incompatible subtree be rejected without
/// inspecting any of its rows.
struct FirstCompatibleRowTree {
    leaf_count: usize,
    maximum_ends: Vec<u64>,
}

impl FirstCompatibleRowTree {
    fn with_capacity(row_capacity: usize) -> Self {
        let leaf_count = row_capacity.max(1).next_power_of_two();
        Self {
            leaf_count,
            maximum_ends: vec![0; leaf_count.saturating_mul(2)],
        }
    }

    fn set(&mut self, row: usize, end: u64) {
        debug_assert!(row < self.leaf_count);
        let mut node = self.leaf_count + row;
        self.maximum_ends[node] = end;
        while node > 1 {
            node /= 2;
            self.maximum_ends[node] =
                self.maximum_ends[node * 2].max(self.maximum_ends[node * 2 + 1]);
        }
    }

    fn end(&self, row: usize) -> u64 {
        self.maximum_ends[self.leaf_count + row]
    }

    fn first_ending_at_or_after(
        &self,
        required_end: u64,
        compatibility_probes: &mut usize,
    ) -> Option<usize> {
        *compatibility_probes = compatibility_probes.saturating_add(1);
        if self.maximum_ends[1] < required_end {
            return None;
        }
        let mut node = 1;
        while node < self.leaf_count {
            *compatibility_probes = compatibility_probes.saturating_add(1);
            let left = node * 2;
            node = if self.maximum_ends[left] >= required_end {
                left
            } else {
                left + 1
            };
        }
        Some(node - self.leaf_count)
    }
}

#[allow(clippy::too_many_arguments)]
fn assign_monotonic_history_rows(
    group: &AlignmentGroupKey,
    indices: &[usize],
    pairs: &PairLayoutMetadata,
    identities: &[AlignmentRowIdentity],
    group_by_index: &[AlignmentGroupKey],
    island: &mut AlignmentRowIsland,
    occupied: &mut BTreeMap<usize, Vec<(u64, u64, AlignmentRowIdentity)>>,
    rows: &mut [Option<usize>],
    revision: u64,
    work: &mut HistoryReconciliationWork,
    is_cancelled: &impl Fn() -> bool,
) -> Option<usize> {
    let index_started = std::time::Instant::now();
    let owned_rows = island
        .row_groups
        .iter()
        .filter(|(_, owners)| owners.contains(group))
        .map(|(&row, _)| row)
        .collect::<Vec<_>>();
    let mut next_row = island
        .row_groups
        .last_key_value()
        .map_or(0, |(&row, _)| row.saturating_add(1));
    let capacity = next_row.saturating_add(indices.len()).max(1);
    let mut gaps = BinaryHeap::<Reverse<RowGapEvent>>::new();
    for row in owned_rows {
        add_row_gap_events(row, occupied.get(&row).map(Vec::as_slice), &mut gaps);
    }
    let mut compatible = FirstCompatibleRowTree::with_capacity(capacity);
    let mut pending = BTreeMap::<usize, Vec<(u64, u64, AlignmentRowIdentity)>>::new();
    work.history_index_micros = work
        .history_index_micros
        .saturating_add(elapsed_micros(index_started));

    for (position, &index) in indices.iter().enumerate() {
        if cancellation_checkpoint(position, is_cancelled) {
            return None;
        }
        let start = pairs.starts[index];
        let end = pairs.ends[index];
        while gaps
            .peek()
            .is_some_and(|Reverse(event)| event.start <= start)
        {
            let Reverse(event) = gaps
                .pop()
                .expect("an eligible row-gap event was observed at the heap head");
            compatible.set(event.row, event.end);
        }

        let (row, gap_end) = if let Some(row) =
            compatible.first_ending_at_or_after(end, &mut work.compatibility_probes)
        {
            let gap_end = compatible.end(row);
            compatible.set(row, 0);
            work.rows_reused = work.rows_reused.saturating_add(1);
            (row, gap_end)
        } else {
            let row = next_row;
            next_row = next_row.saturating_add(1);
            island
                .row_groups
                .entry(row)
                .or_default()
                .insert(group.clone());
            work.rows_appended = work.rows_appended.saturating_add(1);
            (row, u64::MAX)
        };
        if end < gap_end {
            gaps.push(Reverse(RowGapEvent {
                start: end,
                row,
                end: gap_end,
            }));
        }

        let identity = identities[index];
        pending.entry(row).or_default().push((start, end, identity));
        island.assignments.insert(
            identity,
            HistoricalRowAssignment {
                group: group_by_index[index].clone(),
                row,
                start,
                end,
                last_seen_revision: revision,
            },
        );
        rows[index] = Some(row);
    }

    let commit_started = std::time::Instant::now();
    for (index, (row, additions)) in pending.into_iter().enumerate() {
        if cancellation_checkpoint(index, is_cancelled) {
            return None;
        }
        let historical = occupied.remove(&row).unwrap_or_default();
        occupied.insert(row, merge_history_intervals(&historical, &additions));
    }
    work.history_commit_micros = work
        .history_commit_micros
        .saturating_add(elapsed_micros(commit_started));
    Some(indices.len())
}

fn add_row_gap_events(
    row: usize,
    intervals: Option<&[(u64, u64, AlignmentRowIdentity)]>,
    gaps: &mut BinaryHeap<Reverse<RowGapEvent>>,
) {
    add_span_gap_events(
        row,
        intervals
            .into_iter()
            .flatten()
            .map(|&(start, end, _)| (start, end)),
        gaps,
    );
}

fn add_span_gap_events(
    row: usize,
    intervals: impl IntoIterator<Item = (u64, u64)>,
    gaps: &mut BinaryHeap<Reverse<RowGapEvent>>,
) {
    let mut previous_end = 0;
    let mut populated = false;
    for (start, end) in intervals {
        populated = true;
        if previous_end < start {
            gaps.push(Reverse(RowGapEvent {
                start: previous_end,
                row,
                end: start,
            }));
        }
        previous_end = previous_end.max(end);
    }
    if !populated {
        gaps.push(Reverse(RowGapEvent {
            start: 0,
            row,
            end: u64::MAX,
        }));
        return;
    }
    if previous_end < u64::MAX {
        gaps.push(Reverse(RowGapEvent {
            start: previous_end,
            row,
            end: u64::MAX,
        }));
    }
}

fn merge_history_intervals(
    historical: &[(u64, u64, AlignmentRowIdentity)],
    additions: &[(u64, u64, AlignmentRowIdentity)],
) -> Vec<(u64, u64, AlignmentRowIdentity)> {
    debug_assert!(historical.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    debug_assert!(additions.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    let mut merged = Vec::with_capacity(historical.len().saturating_add(additions.len()));
    let mut old = 0;
    let mut new = 0;
    while old < historical.len() || new < additions.len() {
        let take_old = new == additions.len()
            || (old < historical.len() && historical[old].0 <= additions[new].0);
        if take_old {
            merged.push(historical[old]);
            old += 1;
        } else {
            merged.push(additions[new]);
            new += 1;
        }
    }
    debug_assert!(merged.windows(2).all(|pair| pair[0].1 <= pair[1].0));
    merged
}

#[derive(Clone, Debug)]
pub(crate) struct PairLayoutMetadata {
    enabled: bool,
    representatives: Vec<usize>,
    starts: Vec<u64>,
    ends: Vec<u64>,
}

pub(crate) fn pair_layout_metadata(
    query: &AlignmentQuery,
    view_as_pairs: bool,
) -> PairLayoutMetadata {
    let batch = &query.batch;
    let mut representatives = (0..batch.len()).collect::<Vec<_>>();
    let mut starts = batch.starts().collect::<Vec<_>>();
    let mut ends = batch.ends().collect::<Vec<_>>();
    if !view_as_pairs {
        return PairLayoutMetadata {
            enabled: false,
            representatives,
            starts,
            ends,
        };
    }

    let mut first_by_name = HashMap::<Vec<u8>, usize>::new();
    for (index, representative_slot) in representatives.iter_mut().enumerate() {
        let flags = batch.flags(index).expect("validated flags");
        let name = batch.read_name(index).unwrap_or_default();
        if !flags.is_paired() || flags.is_secondary() || flags.is_supplementary() || name.is_empty()
        {
            continue;
        }
        let representative = *first_by_name.entry(name.to_vec()).or_insert(index);
        *representative_slot = representative;
        starts[representative] =
            starts[representative].min(batch.start(index).expect("validated start"));
        ends[representative] = ends[representative].max(batch.end(index).expect("validated end"));
        if !flags.is_mate_unmapped()
            && batch.mate_contig(index).flatten() == batch.contig(index)
            && let Some(mate_start) = batch.mate_start(index).flatten()
        {
            starts[representative] = starts[representative].min(mate_start);
            ends[representative] = ends[representative].max(mate_start.saturating_add(1));
        }
        // TLEN describes the outer template span and is available even when
        // only one mate is present in the current query. Reserving it at first
        // sight prevents a later-arriving mate from expanding a historical
        // span into a row that has meanwhile been given to another template.
        let template_length = batch
            .template_length(index)
            .expect("validated template length");
        if template_length > 0 {
            ends[representative] = ends[representative].max(
                batch
                    .start(index)
                    .expect("validated start")
                    .saturating_add(template_length.unsigned_abs()),
            );
        } else if template_length < 0 {
            starts[representative] = starts[representative].min(
                batch
                    .end(index)
                    .expect("validated end")
                    .saturating_sub(template_length.unsigned_abs()),
            );
        }
    }
    for index in 0..batch.len() {
        let representative = representatives[index];
        starts[index] = starts[representative];
        ends[index] = ends[representative];
    }
    PairLayoutMetadata {
        enabled: true,
        representatives,
        starts,
        ends,
    }
}

pub(crate) fn copy_pair_rows(pairs: &PairLayoutMetadata, rows: &mut [Option<usize>]) {
    for index in 0..rows.len() {
        let representative = pairs.representatives[index];
        if representative != index {
            rows[index] = rows[representative];
        }
    }
}

pub(crate) fn assign_rows_from_scratch(
    groups: &[(&AlignmentGroupKey, &[usize])],
    pairs: &PairLayoutMetadata,
    rows_per_group: usize,
    max_rows: usize,
    rows: &mut [Option<usize>],
    group_starts: &mut BTreeMap<AlignmentGroupKey, usize>,
) {
    let mut next_group_row = 0_usize;
    for &(group_key, indices) in groups {
        if next_group_row >= max_rows {
            break;
        }
        group_starts.insert(group_key.clone(), next_group_row);
        let group_capacity = rows_per_group.min(max_rows - next_group_row);
        let representatives = indices
            .iter()
            .copied()
            .filter(|&index| pairs.representatives[index] == index)
            .collect::<Vec<_>>();
        let starts_are_monotonic = representatives
            .windows(2)
            .all(|pair| pairs.starts[pair[0]] <= pairs.starts[pair[1]]);
        if !starts_are_monotonic {
            let mut coordinate_order = representatives.clone();
            // Interval partitioning is optimal and near-linear when processed
            // by start. The former arbitrary-sort fallback scanned every row
            // and became quadratic at dense loci. Preserve the user's sort as
            // a row-order priority after packing: a dense clique therefore
            // retains the exact requested order, while reusable rows remain a
            // compact deterministic interval coloring.
            coordinate_order.sort_by_key(|&index| (pairs.starts[index], pairs.ends[index]));
            let allocated = assign_rows_coordinate_sweep(
                &coordinate_order,
                pairs,
                next_group_row,
                group_capacity,
                rows,
            );
            reorder_scratch_rows_by_priority(&representatives, next_group_row, allocated, rows);
            next_group_row = next_group_row.saturating_add(allocated);
            continue;
        }

        let allocated = assign_rows_coordinate_sweep(
            &representatives,
            pairs,
            next_group_row,
            group_capacity,
            rows,
        );
        next_group_row = next_group_row.saturating_add(allocated);
    }
}

fn assign_rows_coordinate_sweep(
    representatives: &[usize],
    pairs: &PairLayoutMetadata,
    first_row: usize,
    capacity: usize,
    rows: &mut [Option<usize>],
) -> usize {
    // `active_by_end` establishes when a row becomes reusable;
    // `available_rows` deliberately chooses the visually highest reusable
    // row. The previous one-heap implementation chose whichever row ended
    // earliest, which was height-optimal but rotated free lanes and made a
    // canonical stable baseline look vertically scrambled.
    let mut active_by_end = BinaryHeap::<Reverse<(u64, usize)>>::new();
    let mut available_rows = BinaryHeap::<Reverse<usize>>::new();
    let mut allocated = 0_usize;
    for &index in representatives {
        while active_by_end
            .peek()
            .is_some_and(|Reverse((end, _))| *end <= pairs.starts[index])
        {
            let Reverse((_, row)) = active_by_end
                .pop()
                .expect("a reusable row was observed at the heap head");
            available_rows.push(Reverse(row));
        }
        let local_row = if let Some(Reverse(row)) = available_rows.pop() {
            row
        } else if allocated < capacity {
            let row = allocated;
            allocated += 1;
            row
        } else {
            continue;
        };
        let absolute_row = first_row.saturating_add(local_row);
        active_by_end.push(Reverse((pairs.ends[index], local_row)));
        rows[index] = Some(absolute_row);
    }
    allocated
}

fn reorder_scratch_rows_by_priority(
    representatives: &[usize],
    first_row: usize,
    allocated: usize,
    rows: &mut [Option<usize>],
) {
    let mut priority = vec![usize::MAX; allocated];
    for (rank, &index) in representatives.iter().enumerate() {
        let Some(local_row) = rows[index].and_then(|row| row.checked_sub(first_row)) else {
            continue;
        };
        priority[local_row] = priority[local_row].min(rank);
    }
    let mut ordered_rows = (0..allocated).collect::<Vec<_>>();
    ordered_rows.sort_unstable_by_key(|&row| (priority[row], row));
    let mut remap = vec![0; allocated];
    for (next, previous) in ordered_rows.into_iter().enumerate() {
        remap[previous] = next;
    }
    for &index in representatives {
        let Some(local_row) = rows[index].and_then(|row| row.checked_sub(first_row)) else {
            continue;
        };
        rows[index] = Some(first_row.saturating_add(remap[local_row]));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn assign_rows_with_history(
    groups: &[(&AlignmentGroupKey, &[usize])],
    pairs: &PairLayoutMetadata,
    rows_per_group: usize,
    max_rows: usize,
    identities: &[AlignmentRowIdentity],
    previous: &AlignmentRowLayout,
    rows: &mut [Option<usize>],
    group_starts: &mut BTreeMap<AlignmentGroupKey, usize>,
) {
    let previous_rows = previous
        .identity_rows()
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .collect::<HashMap<_, _>>();
    let mut next_group_row = 0_usize;
    for &(group_key, indices) in groups {
        if next_group_row >= max_rows {
            break;
        }
        let group_capacity = rows_per_group.min(max_rows - next_group_row);
        let previous_group_start = previous.group_starts.get(group_key).copied();
        group_starts.insert(group_key.clone(), next_group_row);
        let mut occupied = vec![Vec::<(u64, u64, usize)>::new(); group_capacity];

        for &index in indices {
            if pairs.representatives[index] != index {
                continue;
            }
            let Some(&preferred) = previous_rows.get(&identities[index]) else {
                continue;
            };
            let Some(previous_group_start) = previous_group_start else {
                continue;
            };
            let Some(local_row) = preferred.checked_sub(previous_group_start) else {
                continue;
            };
            if local_row < group_capacity {
                occupied[local_row].push((pairs.starts[index], pairs.ends[index], index));
            }
        }

        for (local_row, intervals) in occupied.iter_mut().enumerate() {
            intervals.sort_unstable();
            let mut retained = Vec::with_capacity(intervals.len());
            let mut previous_end = 0_u64;
            for &(start, end, index) in intervals.iter() {
                if retained.is_empty() || start >= previous_end {
                    retained.push((start, end, index));
                    previous_end = end;
                    rows[index] = Some(next_group_row + local_row);
                }
            }
            *intervals = retained;
        }

        let mut gaps = BinaryHeap::<Reverse<RowGapEvent>>::new();
        for (local_row, intervals) in occupied.iter().enumerate() {
            add_span_gap_events(
                local_row,
                intervals.iter().map(|&(start, end, _)| (start, end)),
                &mut gaps,
            );
        }
        let mut compatible = FirstCompatibleRowTree::with_capacity(group_capacity);
        let mut newcomers = indices
            .iter()
            .copied()
            .filter(|&index| pairs.representatives[index] == index && rows[index].is_none())
            .collect::<Vec<_>>();
        newcomers.sort_by_key(|&index| (pairs.starts[index], pairs.ends[index]));
        let mut ignored_probes = 0;
        for index in newcomers {
            let start = pairs.starts[index];
            let end = pairs.ends[index];
            while gaps
                .peek()
                .is_some_and(|Reverse(event)| event.start <= start)
            {
                let Reverse(event) = gaps
                    .pop()
                    .expect("an eligible previous-layout gap was observed");
                compatible.set(event.row, event.end);
            }
            let Some(local_row) = compatible.first_ending_at_or_after(end, &mut ignored_probes)
            else {
                continue;
            };
            let gap_end = compatible.end(local_row);
            compatible.set(local_row, 0);
            if end < gap_end {
                gaps.push(Reverse(RowGapEvent {
                    start: end,
                    row: local_row,
                    end: gap_end,
                }));
            }
            occupied[local_row].push((start, end, index));
            rows[index] = Some(next_group_row + local_row);
        }

        let used_rows = occupied
            .iter()
            .rposition(|intervals| !intervals.is_empty())
            .map_or(0, |last| last + 1);
        next_group_row = next_group_row.saturating_add(used_rows);
    }
}

pub(crate) fn alignment_row_identities(
    query: &AlignmentQuery,
    pairs: &PairLayoutMetadata,
) -> Vec<AlignmentRowIdentity> {
    alignment_row_identities_for_mode(query, pairs.enabled)
}

/// Builds the stable identities needed to address an existing row scene.
///
/// Unlike fresh row assignment, scene projection does not need template spans or a
/// read-name-to-representative map. Both primary mates deliberately share the hash of
/// their own (identical) read name, which is the same identity produced after pair
/// representatives have been resolved. Keeping this path independent from
/// `pair_layout_metadata` avoids allocating and hashing a `Vec<u8>` key for every
/// record merely to look up rows that already exist.
pub(crate) fn alignment_row_identities_for_mode(
    query: &AlignmentQuery,
    view_as_pairs: bool,
) -> Vec<AlignmentRowIdentity> {
    let batch = &query.batch;
    let mut occurrences = HashMap::<(u64, u64), u32>::new();
    (0..batch.len())
        .map(|index| {
            let flags = batch.flags(index).expect("validated flags");
            if view_as_pairs
                && flags.is_paired()
                && !flags.is_secondary()
                && !flags.is_supplementary()
                && !batch.read_name(index).unwrap_or_default().is_empty()
            {
                let name = batch.read_name(index).unwrap_or_default();
                return AlignmentRowIdentity {
                    primary: fnv_mix(0xcbf2_9ce4_8422_2325_u64, name),
                    secondary: fnv_mix(0x6c62_272e_07bb_0142_u64, name),
                    occurrence: 0,
                };
            }
            let (primary, secondary) = stable_record_tie_breaker(batch, index);
            let occurrence = occurrences.entry((primary, secondary)).or_default();
            let identity = AlignmentRowIdentity {
                primary,
                secondary,
                occurrence: *occurrence,
            };
            *occurrence = occurrence.saturating_add(1);
            identity
        })
        .collect()
}

pub(crate) fn fnv_mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AlignmentGroupKey {
    All,
    Forward,
    Reverse,
    Tagged(String),
    Missing,
}

#[allow(clippy::too_many_lines)]
#[cfg(test)]
pub(crate) fn grouped_indices(
    query: &AlignmentQuery,
    group_mode: AlignmentGroupMode,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    pair_length_bounds: TemplateLengthBounds,
    promoted_read_name_hash: Option<u64>,
) -> BTreeMap<AlignmentGroupKey, Vec<usize>> {
    grouped_indices_internal(
        query,
        group_mode,
        sort_mode,
        sort_order,
        sort_position,
        pair_length_bounds,
        promoted_read_name_hash,
        false,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn grouped_indices_internal(
    query: &AlignmentQuery,
    group_mode: AlignmentGroupMode,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    pair_length_bounds: TemplateLengthBounds,
    promoted_read_name_hash: Option<u64>,
    deterministic_ties: bool,
) -> BTreeMap<AlignmentGroupKey, Vec<usize>> {
    let batch = &query.batch;
    let pair_evidence = (0..batch.len())
        .map(|index| pair_evidence_at(batch, index, query.interval, pair_length_bounds))
        .collect::<Vec<_>>();
    let sort_labels = match sort_mode {
        AlignmentSortMode::ReadGroup => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, *b"RG"))
                .collect::<Vec<_>>(),
        ),
        AlignmentSortMode::Haplotype => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, *b"HP"))
                .collect::<Vec<_>>(),
        ),
        AlignmentSortMode::Tag(tag) => Some(
            (0..batch.len())
                .map(|index| record_tag_label(query, index, tag))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    let base_sort_keys = (sort_mode == AlignmentSortMode::Base).then(|| {
        (0..batch.len())
            .map(|index| alignment_base_sort_key(query, index, sort_position))
            .collect::<Vec<_>>()
    });
    let stable_ties = (0..batch.len())
        .map(|index| {
            if deterministic_ties {
                stable_record_tie_breaker(batch, index)
            } else {
                (u64::try_from(index).unwrap_or(u64::MAX), 0)
            }
        })
        .collect::<Vec<_>>();
    let mut groups = BTreeMap::<AlignmentGroupKey, Vec<usize>>::new();
    for index in 0..batch.len() {
        groups
            .entry(group_key(query, index, group_mode, pair_length_bounds))
            .or_default()
            .push(index);
    }
    for indices in groups.values_mut() {
        indices.sort_by(|left, right| match sort_mode {
            AlignmentSortMode::Start => (
                batch.start(*left).expect("validated start"),
                batch.end(*left).expect("validated end"),
                stable_ties[*left],
            )
                .cmp(&(
                    batch.start(*right).expect("validated start"),
                    batch.end(*right).expect("validated end"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::Strand => (
                batch.flags(*left).expect("validated flags").is_reverse(),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    batch.flags(*right).expect("validated flags").is_reverse(),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::FirstOfPairStrand => (
                first_of_pair_reverse(batch.flags(*left).expect("validated flags")).is_none(),
                first_of_pair_reverse(batch.flags(*left).expect("validated flags"))
                    .unwrap_or_default(),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    first_of_pair_reverse(batch.flags(*right).expect("validated flags")).is_none(),
                    first_of_pair_reverse(batch.flags(*right).expect("validated flags"))
                        .unwrap_or_default(),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::MappingQuality => (
                Reverse(batch.mapping_quality(*left).flatten().unwrap_or_default()),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(batch.mapping_quality(*right).flatten().unwrap_or_default()),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::ReadName => batch
                .read_name(*left)
                .cmp(&batch.read_name(*right))
                .then_with(|| {
                    batch
                        .start(*left)
                        .expect("validated start")
                        .cmp(&batch.start(*right).expect("validated start"))
                })
                .then_with(|| stable_ties[*left].cmp(&stable_ties[*right])),
            AlignmentSortMode::ReadGroup
            | AlignmentSortMode::Haplotype
            | AlignmentSortMode::Tag(_) => {
                let labels = sort_labels
                    .as_ref()
                    .expect("tag-derived sort labels exist for this mode");
                (
                    labels[*left].is_none(),
                    labels[*left].as_deref(),
                    batch.start(*left).expect("validated start"),
                    stable_ties[*left],
                )
                    .cmp(&(
                        labels[*right].is_none(),
                        labels[*right].as_deref(),
                        batch.start(*right).expect("validated start"),
                        stable_ties[*right],
                    ))
            }
            AlignmentSortMode::AlignedLength => (
                Reverse(aligned_length(batch.cigar(*left).unwrap_or_default())),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(aligned_length(batch.cigar(*right).unwrap_or_default())),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::LeftClip => (
                Reverse(left_clip(batch.cigar(*left).unwrap_or_default())),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(left_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::RightClip => (
                Reverse(right_clip(batch.cigar(*left).unwrap_or_default())),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(right_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::TemplateLength => (
                Reverse(
                    batch
                        .template_length(*left)
                        .expect("validated template length")
                        .unsigned_abs(),
                ),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(
                        batch
                            .template_length(*right)
                            .expect("validated template length")
                            .unsigned_abs(),
                    ),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::PairOrientation => (
                pair_evidence[*left].map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::MateContig => (
                batch.mate_contig(*left).flatten(),
                batch.mate_start(*left).flatten(),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    batch.mate_contig(*right).flatten(),
                    batch.mate_start(*right).flatten(),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::PairStatus => (
                pair_evidence[*left]
                    .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::ReadOrder => (
                pair_evidence[*left].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
            AlignmentSortMode::Base => (
                base_sort_keys.as_ref().expect("base sort keys exist")[*left],
                batch.start(*left).expect("validated start"),
                stable_ties[*left],
            )
                .cmp(&(
                    base_sort_keys.as_ref().expect("base sort keys exist")[*right],
                    batch.start(*right).expect("validated start"),
                    stable_ties[*right],
                )),
        });
        if sort_order == AlignmentOrderDirection::Reverse {
            indices.reverse();
        }
        if let Some(promoted) = promoted_read_name_hash {
            indices.sort_by_key(|index| {
                batch
                    .read_name(*index)
                    .is_none_or(|name| alignment_read_name_hash(name) != promoted)
            });
        }
    }
    groups
}

fn stable_record_tie_breaker(batch: &rgv_hts::AlignmentBatch, index: usize) -> (u64, u64) {
    let stable = batch.stable_id(index).expect("validated stable record id");
    (stable.primary(), stable.secondary())
}

pub(crate) fn first_of_pair_reverse(flags: AlignmentFlags) -> Option<bool> {
    if flags.is_first_in_template() {
        Some(flags.is_reverse())
    } else if flags.is_last_in_template() {
        Some(flags.is_mate_reverse())
    } else {
        None
    }
}

pub(crate) fn effective_sort_position(options: &AlignmentLayoutOptions) -> Option<u64> {
    (options.sort_mode == AlignmentSortMode::Base)
        .then_some(options.sort_position)
        .flatten()
}

pub(crate) fn ordered_group_entries(
    groups: &BTreeMap<AlignmentGroupKey, Vec<usize>>,
    order: AlignmentOrderDirection,
) -> Vec<(&AlignmentGroupKey, &[usize])> {
    match order {
        AlignmentOrderDirection::Natural => groups
            .iter()
            .map(|(key, indices)| (key, indices.as_slice()))
            .collect(),
        AlignmentOrderDirection::Reverse => groups
            .iter()
            .rev()
            .map(|(key, indices)| (key, indices.as_slice()))
            .collect(),
    }
}

/// Sorts the visible reference observation before deletions, skips, unavailable sequence, and
/// records that do not cover the selected position. The base byte provides a deterministic order
/// for IUPAC and other non-ACGT observations without inventing biological equivalence.
pub(crate) fn alignment_base_sort_key(
    query: &AlignmentQuery,
    index: usize,
    sort_position: Option<u64>,
) -> (u8, u8) {
    let Some(target) = sort_position else {
        return (7, 0);
    };
    if target < query.batch.start(index).expect("validated start")
        || target >= query.batch.end(index).expect("validated end")
    {
        return (7, 0);
    }
    let Some(cigar) = query.batch.cigar(index) else {
        return (6, 0);
    };
    let mut reference_position = query.batch.start(index).expect("validated start");
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    let offset = usize::try_from(target.saturating_sub(reference_position))
                        .unwrap_or(usize::MAX);
                    let aligned_position = read_position.saturating_add(offset);
                    let base = query.batch.sequence(index).and_then(|sequence| {
                        let SequenceView::Present { bases, qualities } = sequence else {
                            return None;
                        };
                        aligned_base(bases, qualities, aligned_position)
                            .map(|(base, _)| base.to_ascii_uppercase())
                    });
                    return base.map_or((6, 0), |base| match base {
                        b'A' => (0, base),
                        b'C' => (1, base),
                        b'G' => (2, base),
                        b'T' | b'U' => (3, base),
                        b'N' => (4, base),
                        other => (5, other),
                    });
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_end;
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    return (5, b'-');
                }
                reference_position = reference_end;
            }
            CigarOpKind::ReferenceSkip => {
                let reference_end = reference_position.saturating_add(u64::from(operation.len));
                if (reference_position..reference_end).contains(&target) {
                    return (5, b'~');
                }
                reference_position = reference_end;
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    (7, 0)
}

pub(crate) fn group_key(
    query: &AlignmentQuery,
    index: usize,
    mode: AlignmentGroupMode,
    pair_length_bounds: TemplateLengthBounds,
) -> AlignmentGroupKey {
    match mode {
        AlignmentGroupMode::None => AlignmentGroupKey::All,
        AlignmentGroupMode::Strand => {
            if query
                .batch
                .flags(index)
                .expect("validated flags")
                .is_reverse()
            {
                AlignmentGroupKey::Reverse
            } else {
                AlignmentGroupKey::Forward
            }
        }
        AlignmentGroupMode::ReadGroup => record_tag_label(query, index, *b"RG")
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
        AlignmentGroupMode::Haplotype => record_tag_label(query, index, *b"HP")
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
        AlignmentGroupMode::Chimeric => AlignmentGroupKey::Tagged(
            if query.batch.has_auxiliary_tag(index, *b"SA") {
                "Chimeric"
            } else {
                "Not chimeric"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::Supplementary => AlignmentGroupKey::Tagged(
            if query
                .batch
                .flags(index)
                .expect("validated flags")
                .is_supplementary()
            {
                "Supplementary"
            } else {
                "Primary/secondary"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::LeftClip => AlignmentGroupKey::Tagged(
            if left_clip(query.batch.cigar(index).unwrap_or_default()) > 0 {
                "Left clipped"
            } else {
                "Not left clipped"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::RightClip => AlignmentGroupKey::Tagged(
            if right_clip(query.batch.cigar(index).unwrap_or_default()) > 0 {
                "Right clipped"
            } else {
                "Not right clipped"
            }
            .to_owned(),
        ),
        AlignmentGroupMode::PairOrientation => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds).map_or_else(
                || "Unavailable".to_owned(),
                |pair| format!("{:?}", pair.orientation),
            ),
        ),
        AlignmentGroupMode::UnexpectedPair => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds)
                .map_or("Unavailable", |pair| match pair.classification {
                    PairClassification::Proper | PairClassification::Concordant => "Expected",
                    PairClassification::Unpaired => "Unpaired",
                    _ => "Unexpected",
                })
                .to_owned(),
        ),
        AlignmentGroupMode::MateContig => {
            AlignmentGroupKey::Tagged(query.batch.mate_contig(index).flatten().map_or_else(
                || "Missing".to_owned(),
                |contig| format!("tid {}", contig.get()),
            ))
        }
        AlignmentGroupMode::ReadOrder => AlignmentGroupKey::Tagged(
            pair_evidence_at(&query.batch, index, query.interval, pair_length_bounds).map_or_else(
                || "Unavailable".to_owned(),
                |pair| format!("{:?}", pair.read_order),
            ),
        ),
        AlignmentGroupMode::FirstOfPairStrand => {
            let flags = query.batch.flags(index).expect("validated flags");
            AlignmentGroupKey::Tagged(
                if flags.is_first_in_template() {
                    if flags.is_reverse() {
                        "First reverse"
                    } else {
                        "First forward"
                    }
                } else if flags.is_last_in_template() {
                    if flags.is_mate_reverse() {
                        "First reverse"
                    } else {
                        "First forward"
                    }
                } else {
                    "Unknown"
                }
                .to_owned(),
            )
        }
        AlignmentGroupMode::Tag(tag) => record_tag_label(query, index, tag)
            .map_or(AlignmentGroupKey::Missing, AlignmentGroupKey::Tagged),
    }
}

pub(crate) const fn orientation_rank(orientation: PairOrientation) -> u8 {
    match orientation {
        PairOrientation::Inward => 0,
        PairOrientation::Outward => 1,
        PairOrientation::SameStrand => 2,
        PairOrientation::Unknown => 3,
    }
}

pub(crate) const fn classification_rank(classification: PairClassification) -> u8 {
    match classification {
        PairClassification::Proper => 0,
        PairClassification::Concordant => 1,
        PairClassification::DiscordantTemplateLengthShort => 2,
        PairClassification::DiscordantTemplateLengthLong => 3,
        PairClassification::DiscordantOrientation => 4,
        PairClassification::InterContig => 5,
        PairClassification::MateUnmapped => 6,
        PairClassification::MateMissing => 7,
        PairClassification::ReadUnmapped => 8,
        PairClassification::Unpaired => 9,
    }
}

pub(crate) const fn read_order_rank(order: rgv_hts::ReadOrder) -> u8 {
    match order {
        rgv_hts::ReadOrder::First => 0,
        rgv_hts::ReadOrder::Last => 1,
        rgv_hts::ReadOrder::Unknown => 2,
        rgv_hts::ReadOrder::Ambiguous => 3,
    }
}

pub fn alignment_read_name_hash(name: &[u8]) -> u64 {
    fnv_mix(0xcbf2_9ce4_8422_2325_u64, name)
}

pub(crate) fn aligned_length(cigar: &[CigarOp]) -> u64 {
    cigar.iter().fold(0_u64, |length, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Insertion
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            length.saturating_add(u64::from(operation.len))
        } else {
            length
        }
    })
}

pub(crate) fn left_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .first()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}

pub(crate) fn right_clip(cigar: &[CigarOp]) -> u32 {
    cigar
        .last()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::SoftClip | CigarOpKind::HardClip
            )
        })
        .map_or(0, |operation| operation.len)
}
