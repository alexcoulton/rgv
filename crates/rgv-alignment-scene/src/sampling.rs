use std::{collections::HashMap, num::NonZeroU32};

use crate::{AlignmentHandle, fingerprint::StableHasher};

/// Which eligible reads receive rows. Sampling is a presentation decision only: evidence,
/// coverage and every exact summary always use the complete eligible population.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadSampling {
    /// Every eligible read receives a row.
    #[default]
    Complete,
    /// At most `maximum_reads` reads whose alignment starts fall in one `window_bases` window
    /// receive rows. The retained reads are those with the smallest stable template priority, so
    /// the choice does not depend on decode order and mates share one priority.
    ///
    /// Sampling is template-consistent: once one alignment of a template has been retained or
    /// declined, every other alignment of that template follows the same decision. A window may
    /// therefore hold a few reads beyond `maximum_reads` (mates of templates retained in an
    /// earlier window), but a pair is never shown with one mate silently missing.
    Downsampled {
        window_bases: NonZeroU32,
        maximum_reads: NonZeroU32,
    },
}

impl ReadSampling {
    /// IGV Desktop's documented default: 100 reads per 50-base window.
    pub const DEFAULT_DOWNSAMPLED: Self = Self::Downsampled {
        window_bases: NonZeroU32::new(50).expect("window is non-zero"),
        maximum_reads: NonZeroU32::new(100).expect("read limit is non-zero"),
    };

    #[must_use]
    pub const fn is_downsampled(self) -> bool {
        matches!(self, Self::Downsampled { .. })
    }

    pub(crate) fn write_fingerprint(self, hash: &mut StableHasher) {
        match self {
            Self::Complete => hash.write(&[0]),
            Self::Downsampled {
                window_bases,
                maximum_reads,
            } => {
                hash.write(&[1]);
                hash.write(&window_bases.get().to_le_bytes());
                hash.write(&maximum_reads.get().to_le_bytes());
            }
        }
    }
}

/// Stable, well-mixed sampling priority for one template name. Lower values are retained first.
#[must_use]
pub(crate) fn sampling_priority(read_name: &[u8]) -> u64 {
    let mut value = rgv_hts::template_id(read_name).get();
    // SplitMix64 finaliser: FNV alone leaves trailing name characters weakly mixed.
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SamplingCandidate {
    pub handle: AlignmentHandle,
    pub start: u64,
    /// Template priority; every alignment of one template carries the same value.
    pub priority: u64,
    /// A decision already made for this candidate's template outside this call (its mate holds,
    /// or was declined, a row in an incumbent atlas). It overrides the window capacity.
    pub decided: Option<bool>,
}

/// Returns one retain flag per candidate. `occupied` reports reads already holding rows in a
/// window (incumbent rows are never withdrawn by a later extension).
///
/// Windows are visited left to right. Within a window, candidates whose template was already
/// decided (by `decided`, or in an earlier window) follow that decision first; the remaining
/// capacity then goes to undecided templates in priority order, and each such decision is
/// remembered for the template's other alignments.
pub(crate) fn retained_by_sampling(
    sampling: ReadSampling,
    candidates: &[SamplingCandidate],
    occupied: &HashMap<u64, u32>,
) -> Vec<bool> {
    let ReadSampling::Downsampled {
        window_bases,
        maximum_reads,
    } = sampling
    else {
        return vec![true; candidates.len()];
    };
    let window = u64::from(window_bases.get());
    let mut order = (0..candidates.len()).collect::<Vec<_>>();
    order.sort_unstable_by_key(|index| {
        let candidate = candidates[*index];
        (
            candidate.start / window,
            candidate.priority,
            candidate.handle,
        )
    });
    // Template decisions, keyed by the shared template priority.
    let mut decisions = candidates
        .iter()
        .filter_map(|candidate| candidate.decided.map(|kept| (candidate.priority, kept)))
        .collect::<HashMap<u64, bool>>();
    let mut retained = vec![false; candidates.len()];
    for members in order.chunk_by(|left, right| {
        candidates[*left].start / window == candidates[*right].start / window
    }) {
        let candidate_window = candidates[members[0]].start / window;
        let mut remaining = maximum_reads
            .get()
            .saturating_sub(occupied.get(&candidate_window).copied().unwrap_or(0));
        // Templates decided elsewhere claim their place before any new template competes.
        for index in members.iter().copied() {
            if decisions.get(&candidates[index].priority) == Some(&true) {
                retained[index] = true;
                remaining = remaining.saturating_sub(1);
            }
        }
        for index in members.iter().copied() {
            let priority = candidates[index].priority;
            match decisions.get(&priority) {
                // Retained above, or a second alignment of a template retained in this window.
                Some(true) => retained[index] = true,
                Some(false) => {}
                None => {
                    let kept = remaining > 0;
                    remaining = remaining.saturating_sub(u32::from(kept));
                    decisions.insert(priority, kept);
                    retained[index] = kept;
                }
            }
        }
    }
    retained
}

#[must_use]
pub(crate) fn sampling_window(sampling: ReadSampling, start: u64) -> Option<u64> {
    match sampling {
        ReadSampling::Complete => None,
        ReadSampling::Downsampled { window_bases, .. } => {
            Some(start / u64::from(window_bases.get()))
        }
    }
}

/// Alignment starts belonging to one sampling window.
#[must_use]
pub(crate) fn sampling_window_starts(
    sampling: ReadSampling,
    window: u64,
) -> Option<std::ops::Range<u64>> {
    match sampling {
        ReadSampling::Complete => None,
        ReadSampling::Downsampled { window_bases, .. } => {
            let width = u64::from(window_bases.get());
            let start = window.checked_mul(width)?;
            Some(start..start.saturating_add(width))
        }
    }
}

#[cfg(test)]
mod tests {
    use rgv_hts::AlignmentSourcePosition;

    use super::*;
    use crate::{AlignmentSourceRevision, RevisionFingerprint, SourceFingerprint};

    fn candidate(offset: u64, start: u64, name: &str) -> SamplingCandidate {
        SamplingCandidate {
            handle: AlignmentHandle::new(
                AlignmentSourceRevision::new(
                    SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                    RevisionFingerprint::new(3, 4),
                ),
                AlignmentSourcePosition::bam_virtual_offset(offset),
            ),
            start,
            priority: sampling_priority(name.as_bytes()),
            decided: None,
        }
    }

    fn sampling(window: u32, maximum: u32) -> ReadSampling {
        ReadSampling::Downsampled {
            window_bases: NonZeroU32::new(window).unwrap(),
            maximum_reads: NonZeroU32::new(maximum).unwrap(),
        }
    }

    #[test]
    fn complete_retains_everything() {
        let candidates = (0..10)
            .map(|index| candidate(index, 5, &format!("r{index}")))
            .collect::<Vec<_>>();
        assert!(
            retained_by_sampling(ReadSampling::Complete, &candidates, &HashMap::new())
                .into_iter()
                .all(|kept| kept)
        );
    }

    #[test]
    fn each_window_is_capped_independently_of_input_order() {
        let mut candidates = (0..40)
            .map(|index| candidate(index, index % 2 * 50 + index % 7, &format!("read-{index}")))
            .collect::<Vec<_>>();
        let forward = retained_by_sampling(sampling(50, 3), &candidates, &HashMap::new());
        let kept = |flags: &[bool], candidates: &[SamplingCandidate]| {
            let mut handles = candidates
                .iter()
                .zip(flags)
                .filter_map(|(candidate, kept)| kept.then_some(candidate.handle))
                .collect::<Vec<_>>();
            handles.sort_unstable();
            handles
        };
        let expected = kept(&forward, &candidates);
        assert_eq!(expected.len(), 6);
        candidates.reverse();
        let reversed = retained_by_sampling(sampling(50, 3), &candidates, &HashMap::new());
        assert_eq!(kept(&reversed, &candidates), expected);
    }

    #[test]
    fn incumbent_rows_consume_window_capacity() {
        let candidates = (0..5)
            .map(|index| candidate(index, 120, &format!("n{index}")))
            .collect::<Vec<_>>();
        let occupied = HashMap::from([(2_u64, 2_u32)]);
        let retained = retained_by_sampling(sampling(50, 3), &candidates, &occupied);
        assert_eq!(retained.into_iter().filter(|kept| *kept).count(), 1);
    }

    #[test]
    fn mates_are_retained_or_declined_together_across_windows() {
        // Window 0 is crowded (cap 2), window 3 is nearly empty. Every template has its left
        // mate in window 0 and its right mate in window 3.
        let templates = (0..12)
            .map(|index| format!("pair-{index}"))
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        for (index, name) in templates.iter().enumerate() {
            let index = index as u64;
            candidates.push(candidate(index * 2, 10 + index, name));
            candidates.push(candidate(index * 2 + 1, 160 + index, name));
        }
        let retained = retained_by_sampling(sampling(50, 2), &candidates, &HashMap::new());
        for pair in retained.chunks(2) {
            assert_eq!(pair[0], pair[1], "mates must share one sampling decision");
        }
        // The crowded window decides: exactly its cap of templates survive, with both mates.
        assert_eq!(retained.iter().filter(|kept| **kept).count(), 4);
    }

    #[test]
    fn an_incumbent_template_decision_overrides_window_capacity() {
        let mut candidates = (0..6)
            .map(|index| candidate(index, 120 + index, &format!("n{index}")))
            .collect::<Vec<_>>();
        // The window is already full, yet the mate of a displayed read must join it, and the
        // mate of a declined read must stay out even though it has the best priority left.
        let occupied = HashMap::from([(2_u64, 3_u32)]);
        candidates[4].decided = Some(true);
        let best = (0..6)
            .filter(|index| *index != 4)
            .min_by_key(|index| candidates[*index].priority)
            .unwrap();
        candidates[best].decided = Some(false);
        let retained = retained_by_sampling(sampling(50, 3), &candidates, &occupied);
        assert_eq!(retained, (0..6).map(|index| index == 4).collect::<Vec<_>>());

        let roomy = retained_by_sampling(sampling(50, 4), &candidates, &HashMap::new());
        assert!(roomy[4]);
        assert!(!roomy[best]);
        assert_eq!(roomy.iter().filter(|kept| **kept).count(), 4);
    }

    #[test]
    fn mates_share_one_priority() {
        assert_eq!(
            sampling_priority(b"template"),
            sampling_priority(b"template")
        );
        assert_ne!(
            sampling_priority(b"template1"),
            sampling_priority(b"template2")
        );
    }
}
