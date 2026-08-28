#[allow(clippy::wildcard_imports)]
use super::*;

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
        rows,
        identities,
        pair_representatives: pairs.representatives,
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
    let reconciliation_started = std::time::Instant::now();
    if stacking_mode == ReadStackingMode::Compact {
        let layout = assign_alignment_rows_internal(query, options, None, true);
        let organization = alignment_row_organization_metrics(query, options, &layout);
        let stats = AlignmentRowReconciliationStats {
            revision: history.revision(),
            architecture_version: organization.architecture_version,
            reconciliation_micros: elapsed_micros(reconciliation_started),
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
        return (layout, stats);
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
            let mut layout = assign_alignment_rows_internal(layout_query, options, None, true);
            let added = seed_history_island(context, layout_query, &layout, options, revision);
            layout.max_rows = layout.used_row_count().max(1);
            (layout, 0, 0, added, 0, repack_stable, false, false)
        } else {
            let island_index = merge_connected_islands(
                context,
                &connected,
                layout_query.interval,
                !layout_query.individual_reads_complete(),
                horizon_extension,
                revision,
            );
            let (layout, retained, restored, added, reservations) = reconcile_history_island(
                &mut context.islands[island_index],
                layout_query,
                options,
                revision,
            );
            (
                layout,
                retained,
                restored,
                added,
                reservations,
                false,
                true,
                horizon_extension,
            )
        }
    };

    if !std::ptr::eq(query, layout_query) {
        let context = &mut history.contexts[context_index];
        let island_index = context
            .islands
            .iter()
            .position(|island| interval_contains(island.interval, query.interval))
            .expect("the visible query must be contained by its layout horizon");
        let (visible_layout, _, _, visible_added, visible_reservations) =
            reconcile_history_island(&mut context.islands[island_index], query, options, revision);
        layout = visible_layout;
        added = added.saturating_add(visible_added);
        reservations = visible_reservations;
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
    let organization = alignment_row_organization_metrics(query, options, &layout);

    let stats = AlignmentRowReconciliationStats {
        revision,
        architecture_version: organization.architecture_version,
        reconciliation_micros: elapsed_micros(reconciliation_started),
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
    (layout, stats)
}

pub fn alignment_row_organization_metrics(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    layout: &AlignmentRowLayout,
) -> AlignmentRowOrganizationMetrics {
    let canonical = assign_alignment_rows_internal(query, options, None, true);
    let canonical_rows_by_identity = canonical
        .identities
        .iter()
        .copied()
        .zip(canonical.rows.iter().copied())
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .collect::<HashMap<_, _>>();
    let (canonical_displacement, top_first_violations) = layout
        .identities
        .iter()
        .copied()
        .zip(layout.rows.iter().copied())
        .filter_map(|(identity, row)| row.map(|row| (identity, row)))
        .fold((0_usize, 0_usize), |(displaced, lower), (identity, row)| {
            let canonical_row = canonical_rows_by_identity.get(&identity).copied();
            (
                displaced + usize::from(canonical_row != Some(row)),
                lower + usize::from(canonical_row.is_some_and(|canonical| row > canonical)),
            )
        });
    let occupied = layout
        .pair_representatives
        .iter()
        .enumerate()
        .filter(|(index, representative)| **representative == *index)
        .filter_map(|(index, _)| layout.rows[index])
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

fn elapsed_micros(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn alignment_row_policy_fingerprint(options: &AlignmentLayoutOptions) -> u64 {
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
    layout
        .pair_representatives
        .iter()
        .enumerate()
        .filter(|(index, representative)| {
            **representative == *index && layout.rows[*index].is_some()
        })
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
) -> usize {
    let (pairs, identities, _, group_by_index) = row_inputs(query, options);
    let mut assignments = HashMap::new();
    let mut row_groups = BTreeMap::<usize, BTreeSet<AlignmentGroupKey>>::new();
    let mut added = 0;
    for index in 0..query.batch.len() {
        if pairs.representatives[index] != index {
            continue;
        }
        let Some(row) = layout.rows[index] else {
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
    added
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
) -> (AlignmentRowLayout, usize, usize, usize, usize) {
    island.last_used_revision = revision;
    let (pairs, identities, groups, group_by_index) = row_inputs(query, options);
    let mut rows = vec![None; query.batch.len()];
    let mut retained = 0;
    let mut restored = 0;
    let mut current_identities = BTreeSet::new();

    // Refresh known spans before building row occupancy. Pair view can learn a
    // wider template span when the mate enters the query; the identity and row
    // remain stable while the reservation conservatively expands.
    for index in 0..query.batch.len() {
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

    let mut occupied = BTreeMap::<usize, Vec<(u64, u64, AlignmentRowIdentity)>>::new();
    for (&identity, assignment) in &island.assignments {
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

    let ordered_groups = ordered_group_entries(&groups, options.group_order);
    let mut added = 0;
    for &(group, indices) in &ordered_groups {
        for &index in indices {
            if pairs.representatives[index] != index || rows[index].is_some() {
                continue;
            }
            let identity = identities[index];
            let start = pairs.starts[index];
            let end = pairs.ends[index];
            let row = island
                .row_groups
                .iter()
                .filter(|(_, owners)| owners.contains(group))
                .map(|(&row, _)| row)
                .find(|row| {
                    occupied
                        .get(row)
                        .is_none_or(|intervals| history_interval_fits(intervals, start, end))
                })
                .unwrap_or_else(|| {
                    let row = island
                        .row_groups
                        .last_key_value()
                        .map_or(0, |(&last, _)| last.saturating_add(1));
                    island
                        .row_groups
                        .entry(row)
                        .or_default()
                        .insert(group.clone());
                    row
                });
            let intervals = occupied.entry(row).or_default();
            let insert_at =
                intervals.partition_point(|&(existing_start, _, _)| existing_start <= start);
            intervals.insert(insert_at, (start, end, identity));
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
            current_identities.insert(identity);
            added += 1;
        }
    }
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
        rows,
        identities,
        pair_representatives: pairs.representatives,
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
    (layout, retained, restored, added, reservations)
}

fn history_interval_fits(
    intervals: &[(u64, u64, AlignmentRowIdentity)],
    start: u64,
    end: u64,
) -> bool {
    let next = intervals.partition_point(|&(existing_start, _, _)| existing_start < start);
    let clears_previous = next == 0 || intervals[next - 1].1 <= start;
    let clears_next = next == intervals.len() || end <= intervals[next].0;
    clears_previous && clears_next
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
    let mut starts = batch.starts.clone();
    let mut ends = batch.ends.clone();
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
        let flags = batch.flags[index];
        let name = batch.read_name(index).unwrap_or_default();
        if !flags.is_paired() || flags.is_secondary() || flags.is_supplementary() || name.is_empty()
        {
            continue;
        }
        let representative = *first_by_name.entry(name.to_vec()).or_insert(index);
        *representative_slot = representative;
        starts[representative] = starts[representative].min(batch.starts[index]);
        ends[representative] = ends[representative].max(batch.ends[index]);
        if !flags.is_mate_unmapped()
            && batch.mate_contigs[index] == Some(batch.contigs[index])
            && let Some(mate_start) = batch.mate_starts[index]
        {
            starts[representative] = starts[representative].min(mate_start);
            ends[representative] = ends[representative].max(mate_start.saturating_add(1));
        }
        // TLEN describes the outer template span and is available even when
        // only one mate is present in the current query. Reserving it at first
        // sight prevents a later-arriving mate from expanding a historical
        // span into a row that has meanwhile been given to another template.
        let template_length = batch.template_lengths[index];
        if template_length > 0 {
            ends[representative] = ends[representative]
                .max(batch.starts[index].saturating_add(template_length.unsigned_abs()));
        } else if template_length < 0 {
            starts[representative] = starts[representative]
                .min(batch.ends[index].saturating_sub(template_length.unsigned_abs()));
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
            let allocated = assign_rows_top_first_general(
                &representatives,
                pairs,
                next_group_row,
                group_capacity,
                rows,
            );
            next_group_row = next_group_row.saturating_add(allocated);
            continue;
        }

        // `active_by_end` establishes when a row becomes reusable;
        // `available_rows` deliberately chooses the visually highest reusable
        // row. The previous one-heap implementation chose whichever row ended
        // earliest, which was height-optimal but rotated free lanes and made a
        // canonical stable baseline look vertically scrambled.
        let mut active_by_end = BinaryHeap::<Reverse<(u64, usize)>>::new();
        let mut available_rows = BinaryHeap::<Reverse<usize>>::new();
        let mut allocated = 0_usize;
        for index in representatives {
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
            } else if allocated < group_capacity {
                let row = allocated;
                allocated += 1;
                row
            } else {
                continue;
            };
            let absolute_row = next_group_row.saturating_add(local_row);
            active_by_end.push(Reverse((pairs.ends[index], local_row)));
            rows[index] = Some(absolute_row);
        }
        next_group_row = next_group_row.saturating_add(allocated);
    }
}

fn assign_rows_top_first_general(
    representatives: &[usize],
    pairs: &PairLayoutMetadata,
    first_row: usize,
    capacity: usize,
    rows: &mut [Option<usize>],
) -> usize {
    let mut occupied = Vec::<Vec<(u64, u64, usize)>>::new();
    for &index in representatives {
        let start = pairs.starts[index];
        let end = pairs.ends[index];
        let row = occupied
            .iter()
            .position(|intervals| interval_fits(intervals, start, end))
            .or_else(|| (occupied.len() < capacity).then_some(occupied.len()));
        let Some(row) = row else {
            continue;
        };
        if row == occupied.len() {
            occupied.push(Vec::new());
        }
        let intervals = &mut occupied[row];
        let insert_at =
            intervals.partition_point(|&(existing_start, _, _)| existing_start <= start);
        intervals.insert(insert_at, (start, end, index));
        rows[index] = Some(first_row.saturating_add(row));
    }
    occupied.len()
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
        .identities
        .iter()
        .copied()
        .zip(previous.rows.iter().copied())
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

        for &index in indices {
            if pairs.representatives[index] != index {
                continue;
            }
            if rows[index].is_some() {
                continue;
            }
            let start = pairs.starts[index];
            let end = pairs.ends[index];
            let Some(local_row) =
                (0..group_capacity).find(|&row| interval_fits(&occupied[row], start, end))
            else {
                continue;
            };
            let insert_at = occupied[local_row]
                .partition_point(|&(existing_start, _, _)| existing_start <= start);
            occupied[local_row].insert(insert_at, (start, end, index));
            rows[index] = Some(next_group_row + local_row);
        }

        let used_rows = occupied
            .iter()
            .rposition(|intervals| !intervals.is_empty())
            .map_or(0, |last| last + 1);
        next_group_row = next_group_row.saturating_add(used_rows);
    }
}

pub(crate) fn interval_fits(intervals: &[(u64, u64, usize)], start: u64, end: u64) -> bool {
    let next = intervals.partition_point(|&(existing_start, _, _)| existing_start < start);
    let clears_previous = next == 0 || intervals[next - 1].1 <= start;
    let clears_next = next == intervals.len() || end <= intervals[next].0;
    clears_previous && clears_next
}

pub(crate) fn alignment_row_identities(
    query: &AlignmentQuery,
    pairs: &PairLayoutMetadata,
) -> Vec<AlignmentRowIdentity> {
    let batch = &query.batch;
    let mut occurrences = HashMap::<(u64, u64), u32>::new();
    (0..batch.len())
        .map(|index| {
            let representative = pairs.representatives[index];
            let flags = batch.flags[index];
            if pairs.enabled
                && flags.is_paired()
                && !flags.is_secondary()
                && !flags.is_supplementary()
                && !batch.read_name(index).unwrap_or_default().is_empty()
            {
                let name = batch.read_name(representative).unwrap_or_default();
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
            AlignmentSortMode::Start => {
                (batch.starts[*left], batch.ends[*left], stable_ties[*left]).cmp(&(
                    batch.starts[*right],
                    batch.ends[*right],
                    stable_ties[*right],
                ))
            }
            AlignmentSortMode::Strand => (
                batch.flags[*left].is_reverse(),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    batch.flags[*right].is_reverse(),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::FirstOfPairStrand => (
                first_of_pair_reverse(batch.flags[*left]).is_none(),
                first_of_pair_reverse(batch.flags[*left]).unwrap_or_default(),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    first_of_pair_reverse(batch.flags[*right]).is_none(),
                    first_of_pair_reverse(batch.flags[*right]).unwrap_or_default(),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::MappingQuality => (
                Reverse(batch.mapping_qualities[*left].unwrap_or_default()),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(batch.mapping_qualities[*right].unwrap_or_default()),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::ReadName => batch
                .read_name(*left)
                .cmp(&batch.read_name(*right))
                .then_with(|| batch.starts[*left].cmp(&batch.starts[*right]))
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
                    batch.starts[*left],
                    stable_ties[*left],
                )
                    .cmp(&(
                        labels[*right].is_none(),
                        labels[*right].as_deref(),
                        batch.starts[*right],
                        stable_ties[*right],
                    ))
            }
            AlignmentSortMode::AlignedLength => (
                Reverse(aligned_length(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(aligned_length(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::LeftClip => (
                Reverse(left_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(left_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::RightClip => (
                Reverse(right_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(right_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::TemplateLength => (
                Reverse(batch.template_lengths[*left].unsigned_abs()),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    Reverse(batch.template_lengths[*right].unsigned_abs()),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::PairOrientation => (
                pair_evidence[*left].map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::MateContig => (
                batch.mate_contigs[*left],
                batch.mate_starts[*left],
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    batch.mate_contigs[*right],
                    batch.mate_starts[*right],
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::PairStatus => (
                pair_evidence[*left]
                    .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::ReadOrder => (
                pair_evidence[*left].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    pair_evidence[*right].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                    batch.starts[*right],
                    stable_ties[*right],
                )),
            AlignmentSortMode::Base => (
                base_sort_keys.as_ref().expect("base sort keys exist")[*left],
                batch.starts[*left],
                stable_ties[*left],
            )
                .cmp(&(
                    base_sort_keys.as_ref().expect("base sort keys exist")[*right],
                    batch.starts[*right],
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
    let mut primary = 0xcbf2_9ce4_8422_2325_u64;
    let mut secondary = 0x6c62_272e_07bb_0142_u64;
    for bytes in [
        batch.contigs[index].get().to_le_bytes().as_slice(),
        batch.starts[index].to_le_bytes().as_slice(),
        batch.ends[index].to_le_bytes().as_slice(),
        batch.flags[index].bits().to_le_bytes().as_slice(),
        batch.mapping_qualities[index]
            .unwrap_or(u8::MAX)
            .to_le_bytes()
            .as_slice(),
        batch.mate_contigs[index]
            .map_or(u32::MAX, rgv_core::ContigId::get)
            .to_le_bytes()
            .as_slice(),
        batch.mate_starts[index]
            .unwrap_or(u64::MAX)
            .to_le_bytes()
            .as_slice(),
        batch.template_lengths[index].to_le_bytes().as_slice(),
        batch.read_name(index).unwrap_or_default(),
    ] {
        primary = fnv_mix(primary, bytes);
        secondary = fnv_mix(secondary, bytes);
    }
    if let Some(cigar) = batch.cigar(index) {
        for operation in cigar {
            let kind = match operation.kind {
                CigarOpKind::Match => 0,
                CigarOpKind::Insertion => 1,
                CigarOpKind::Deletion => 2,
                CigarOpKind::ReferenceSkip => 3,
                CigarOpKind::SoftClip => 4,
                CigarOpKind::HardClip => 5,
                CigarOpKind::Padding => 6,
                CigarOpKind::SequenceMatch => 7,
                CigarOpKind::SequenceMismatch => 8,
            };
            primary = fnv_mix(primary, &[kind]);
            primary = fnv_mix(primary, &operation.len.to_le_bytes());
            secondary = fnv_mix(secondary, &[kind]);
            secondary = fnv_mix(secondary, &operation.len.to_le_bytes());
        }
    }
    // Stable row identity deliberately excludes arbitrary auxiliary payload.
    // Layout-horizon queries retain only policy-required tags, while the
    // visible query may contain every tag; coordinates, flags, MAPQ, mate
    // fields, read name, and CIGAR must therefore be sufficient and identical
    // across both detail levels. Exact duplicates retain their deterministic
    // occurrence within the same fixed horizon.
    (primary, secondary)
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
    if target < query.batch.starts[index] || target >= query.batch.ends[index] {
        return (7, 0);
    }
    let Some(cigar) = query.batch.cigar(index) else {
        return (6, 0);
    };
    let mut reference_position = query.batch.starts[index];
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
            if query.batch.flags[index].is_reverse() {
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
            if query.batch.flags[index].is_supplementary() {
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
            AlignmentGroupKey::Tagged(query.batch.mate_contigs[index].map_or_else(
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
            let flags = query.batch.flags[index];
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
