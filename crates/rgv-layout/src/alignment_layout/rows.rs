#[allow(clippy::wildcard_imports)]
use super::*;

pub fn assign_alignment_rows(
    query: &AlignmentQuery,
    options: AlignmentLayoutOptions,
    previous: Option<&AlignmentRowLayout>,
) -> AlignmentRowLayout {
    let pairs = pair_layout_metadata(query, options.view_as_pairs);
    let groups = grouped_indices(
        query,
        options.group_mode,
        options.sort_mode,
        options.sort_order,
        effective_sort_position(&options),
        options.pair_length_bounds,
        options.promoted_read_name_hash,
    );
    let ordered_groups = ordered_group_entries(&groups, options.group_order);
    let group_count = groups.len().max(1);
    let max_rows = options.max_rows.max(1);
    let rows_per_group = (max_rows / group_count).max(1);
    let identities = alignment_row_identities(query, &pairs);
    let mut rows = vec![None; query.batch.len()];
    let mut group_starts = BTreeMap::new();
    let compatible_previous = previous.filter(|layout| layout.compatible_with(&options));

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
        let mut occupied = BinaryHeap::<Reverse<(u64, usize)>>::new();
        let mut allocated = 0_usize;
        for &index in indices {
            if pairs.representatives[index] != index {
                continue;
            }
            let local_row = if occupied
                .peek()
                .is_some_and(|Reverse((end, _))| *end <= pairs.starts[index])
            {
                let Reverse((_, row)) = occupied.pop().expect("peeked row must exist");
                row
            } else if allocated < group_capacity {
                let row = allocated;
                allocated += 1;
                row
            } else {
                continue;
            };
            let absolute_row = next_group_row.saturating_add(local_row);
            occupied.push(Reverse((pairs.ends[index], local_row)));
            rows[index] = Some(absolute_row);
        }
        next_group_row = next_group_row.saturating_add(allocated);
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
            let mut primary = 0xcbf2_9ce4_8422_2325_u64;
            let mut secondary = 0x6c62_272e_07bb_0142_u64;
            for bytes in [
                batch.contigs[index].get().to_le_bytes().as_slice(),
                batch.starts[index].to_le_bytes().as_slice(),
                batch.ends[index].to_le_bytes().as_slice(),
                batch.flags[index].bits().to_le_bytes().as_slice(),
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
pub(crate) fn grouped_indices(
    query: &AlignmentQuery,
    group_mode: AlignmentGroupMode,
    sort_mode: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_position: Option<u64>,
    pair_length_bounds: TemplateLengthBounds,
    promoted_read_name_hash: Option<u64>,
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
    let mut groups = BTreeMap::<AlignmentGroupKey, Vec<usize>>::new();
    for index in 0..batch.len() {
        groups
            .entry(group_key(query, index, group_mode, pair_length_bounds))
            .or_default()
            .push(index);
    }
    for indices in groups.values_mut() {
        indices.sort_by(|left, right| match sort_mode {
            AlignmentSortMode::Start => (batch.starts[*left], batch.ends[*left], *left).cmp(&(
                batch.starts[*right],
                batch.ends[*right],
                *right,
            )),
            AlignmentSortMode::Strand => {
                (batch.flags[*left].is_reverse(), batch.starts[*left], *left).cmp(&(
                    batch.flags[*right].is_reverse(),
                    batch.starts[*right],
                    *right,
                ))
            }
            AlignmentSortMode::FirstOfPairStrand => (
                first_of_pair_reverse(batch.flags[*left]).is_none(),
                first_of_pair_reverse(batch.flags[*left]).unwrap_or_default(),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    first_of_pair_reverse(batch.flags[*right]).is_none(),
                    first_of_pair_reverse(batch.flags[*right]).unwrap_or_default(),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::MappingQuality => (
                Reverse(batch.mapping_qualities[*left].unwrap_or_default()),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(batch.mapping_qualities[*right].unwrap_or_default()),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::ReadName => batch
                .read_name(*left)
                .cmp(&batch.read_name(*right))
                .then_with(|| batch.starts[*left].cmp(&batch.starts[*right])),
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
                    *left,
                )
                    .cmp(&(
                        labels[*right].is_none(),
                        labels[*right].as_deref(),
                        batch.starts[*right],
                        *right,
                    ))
            }
            AlignmentSortMode::AlignedLength => (
                Reverse(aligned_length(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(aligned_length(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::LeftClip => (
                Reverse(left_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(left_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::RightClip => (
                Reverse(right_clip(batch.cigar(*left).unwrap_or_default())),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(right_clip(batch.cigar(*right).unwrap_or_default())),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::TemplateLength => (
                Reverse(batch.template_lengths[*left].unsigned_abs()),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    Reverse(batch.template_lengths[*right].unsigned_abs()),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::PairOrientation => (
                pair_evidence[*left].map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| orientation_rank(pair.orientation)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::MateContig => (
                batch.mate_contigs[*left],
                batch.mate_starts[*left],
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    batch.mate_contigs[*right],
                    batch.mate_starts[*right],
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::PairStatus => (
                pair_evidence[*left]
                    .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right]
                        .map_or(u8::MAX, |pair| classification_rank(pair.classification)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::ReadOrder => (
                pair_evidence[*left].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    pair_evidence[*right].map_or(u8::MAX, |pair| read_order_rank(pair.read_order)),
                    batch.starts[*right],
                    *right,
                )),
            AlignmentSortMode::Base => (
                base_sort_keys.as_ref().expect("base sort keys exist")[*left],
                batch.starts[*left],
                *left,
            )
                .cmp(&(
                    base_sort_keys.as_ref().expect("base sort keys exist")[*right],
                    batch.starts[*right],
                    *right,
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
                        oriented_base(query.batch.flags[index], bases, qualities, aligned_position)
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
