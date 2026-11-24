//! How common a hovered read event is at its locus, for the read-stack tooltip.
//!
//! Figures come from the scene's exact per-position summaries wherever one exists, so they match
//! the coverage and Modifications lanes and their inspectors: allele and deletion counts from the
//! allele summary (`pileup_site_at`), insertion counts from the same bin the coverage
//! inspector uses, modification tallies from the modification summary at the site the call is
//! summarised at. The rest (inserted sequences, exact-span deletion matches, reads clipped at a
//! boundary) is counted among the loaded reads overlapping the locus, through the evidence
//! overlap index, so its cost follows the reads there, never the whole scene. It is gathered once
//! per hovered target (see `ReadHoverTooltipCache`), never per frame.

use crate::inspection::coverage_insertions::{inserted_bases_after, tally_inserted_sequences};
use crate::ui::read_hover::{
    AlleleFacts, DeletionFacts, InsertionFacts, LocusFacts, ModificationFacts,
    aligned_read_position, operation_origin,
};
use rgv_alignment_scene::{AlignmentComponent, AlignmentHoverTarget, AlignmentSceneSnapshot};
use rgv_core::GenomicInterval;
use rgv_hts::{AlignmentRecord, CigarOp, CigarOpKind, SequenceData};

/// Every loaded read overlapping `start..end`, as `(start, end, cigar, bases)`.
fn for_each_read_over(
    scene: &AlignmentSceneSnapshot,
    start: u64,
    end: u64,
    mut visit: impl FnMut(u64, u64, &[CigarOp], &[u8]),
) -> bool {
    let prepared = scene.prepared_interval();
    let (start, end) = (start.max(prepared.start()), end.min(prepared.end()));
    let Ok(interval) = GenomicInterval::new(prepared.contig(), start, end.max(start + 1)) else {
        return false;
    };
    let Ok(selection) = scene.reads().evidence().select(interval) else {
        return false;
    };
    for index in 0..selection.len() {
        let Some(record) = selection.record_view(index) else {
            continue;
        };
        let bases = match selection.sequence(index) {
            Some(rgv_model::SequenceView::Present { bases, .. }) => bases,
            _ => &[][..],
        };
        visit(record.start(), record.end(), record.cigar(), bases);
    }
    true
}

/// The inserted sequences of the loaded reads inserting on the boundary after `anchor`, most
/// common first, exactly as the coverage inspector lists them.
pub(crate) fn inserted_sequences_at(
    scene: &AlignmentSceneSnapshot,
    anchor: u64,
) -> Vec<(Vec<u8>, u32)> {
    let mut inserted = Vec::new();
    for_each_read_over(scene, anchor, anchor + 1, |start, _, cigar, bases| {
        inserted.extend(inserted_bases_after(start, cigar, bases, anchor));
    });
    tally_inserted_sequences(inserted)
}

/// The allele summary's counts at `position`, when the scene carries allele detail.
pub(crate) fn allele_facts(scene: &AlignmentSceneSnapshot, position: u64) -> Option<AlleleFacts> {
    let site = scene.pileup_site_at(position)?;
    if !site.evidence.allele_observations_available {
        return None;
    }
    let alleles = site.evidence.bin.alleles;
    Some(AlleleFacts {
        bases: [alleles.a, alleles.c, alleles.g, alleles.t, alleles.n],
        deletion: alleles.deletion,
        observations: alleles.base_observations(),
    })
}

/// Insertions on the boundary after `anchor`: the coverage inspector's counts and sequences.
pub(crate) fn insertion_facts(
    scene: &AlignmentSceneSnapshot,
    anchor: u64,
) -> Option<InsertionFacts> {
    let site = scene.pileup_site_at(anchor)?;
    if !site.evidence.allele_observations_available {
        return None;
    }
    let bin = site.evidence.bin;
    Some(InsertionFacts {
        with: bin.alleles.insertion,
        covering: rgv_render::insertion_covering_reads(&bin),
        sequences: inserted_sequences_at(scene, anchor),
    })
}

/// A deletion of `length` bases from `start`: loaded reads with exactly that deletion, loaded
/// reads spanning it, and the allele summary's deletions over its first base.
pub(crate) fn deletion_facts(
    scene: &AlignmentSceneSnapshot,
    start: u64,
    length: u64,
) -> Option<DeletionFacts> {
    let end = start.saturating_add(length);
    let (mut same, mut spanning) = (0_u32, 0_u32);
    let complete = for_each_read_over(scene, start, end, |read_start, read_end, cigar, _| {
        if read_start > start || read_end < end {
            return;
        }
        spanning += 1;
        let mut reference = read_start;
        for operation in cigar {
            match operation.kind {
                CigarOpKind::Deletion => {
                    if reference == start && u64::from(operation.len) == length {
                        same += 1;
                        return;
                    }
                    reference += u64::from(operation.len);
                }
                CigarOpKind::Match
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
                | CigarOpKind::ReferenceSkip => reference += u64::from(operation.len),
                _ => {}
            }
            if reference > start {
                return;
            }
        }
    });
    complete.then(|| DeletionFacts {
        same,
        spanning,
        any_over_first_base: allele_facts(scene, start).map(|alleles| alleles.deletion),
    })
}

/// Whether a read's soft clip is on its left end (before any aligned base).
fn soft_clip_on_left(cigar: &[CigarOp]) -> bool {
    cigar
        .iter()
        .find(|operation| operation.kind != CigarOpKind::HardClip)
        .is_some_and(|operation| operation.kind == CigarOpKind::SoftClip)
}

fn soft_clip_on_right(cigar: &[CigarOp]) -> bool {
    cigar
        .iter()
        .rev()
        .find(|operation| operation.kind != CigarOpKind::HardClip)
        .is_some_and(|operation| operation.kind == CigarOpKind::SoftClip)
}

/// Loaded reads soft-clipped on the same side at the same boundary as clip `cigar_index` of
/// `record`.
pub(crate) fn clipped_here(
    scene: &AlignmentSceneSnapshot,
    record: &AlignmentRecord,
    cigar_index: usize,
) -> Option<u32> {
    let left = record.cigar[..cigar_index].iter().all(|operation| {
        matches!(
            operation.kind,
            CigarOpKind::HardClip | CigarOpKind::SoftClip
        )
    });
    let boundary = if left { record.start } else { record.end };
    let mut count = 0_u32;
    let complete = for_each_read_over(
        scene,
        boundary.saturating_sub(1),
        boundary + 1,
        |start, end, cigar, _| {
            if (left && start == boundary && soft_clip_on_left(cigar))
                || (!left && end == boundary && soft_clip_on_right(cigar))
            {
                count += 1;
            }
        },
    );
    complete.then_some(count)
}

/// The reference position the modification summary records a call at: a cytosine call of a
/// reverse-strand read on the G of a `CpG` (the read's stored bases `CG`) is pooled on the
/// `CpG`'s C, exactly as `rgv_model::ModificationSummary` places it.
pub(crate) fn modification_summary_site(
    record: Option<&AlignmentRecord>,
    position: u64,
    read_index: u32,
    canonical_base: u8,
) -> u64 {
    let Some(record) = record else {
        return position;
    };
    let SequenceData::Present { bases, .. } = &record.sequence else {
        return position;
    };
    let index = read_index as usize;
    let cpg_g = canonical_base.eq_ignore_ascii_case(&b'C')
        && record.flags.is_reverse()
        && index > 0
        && bases
            .get(index)
            .is_some_and(|base| base.eq_ignore_ascii_case(&b'G'))
        && bases[index - 1].eq_ignore_ascii_case(&b'C');
    if cpg_g && position > 0 && aligned_read_position(record, position - 1) == Some(index - 1) {
        position - 1
    } else {
        position
    }
}

/// The modification summary's tally of `site`.
pub(crate) fn modification_tally(
    scene: &AlignmentSceneSnapshot,
    site: u64,
) -> Option<rgv_hts::ModificationSiteTally> {
    let (interval, summary) = scene.summaries().modification_summary_at(site)?;
    let index = summary.bin_index_at(interval, site)?;
    Some(summary.site_tally(index))
}

/// The locus facts the scene itself can give for `target` (junction and pair facts come from
/// the viewer's Sashimi presentation and the read inspector; see `Viewer::read_hover_facts`).
pub(crate) fn scene_locus_facts(
    scene: &AlignmentSceneSnapshot,
    target: &AlignmentHoverTarget,
    record: Option<&AlignmentRecord>,
) -> LocusFacts {
    let span = target.genomic_span();
    let origin =
        |cigar_index: u32| record.and_then(|record| operation_origin(record, cigar_index as usize));
    let mut facts = LocusFacts::default();
    match target.hit.component {
        AlignmentComponent::Base { .. } => facts.alleles = allele_facts(scene, span.start),
        AlignmentComponent::Insertion { cigar_index, .. } => {
            let boundary = origin(cigar_index).map_or(
                if target.primitive.geometry
                    == rgv_alignment_scene::AlignmentGeometryToken::PointMarkerAtEnd
                {
                    span.end
                } else {
                    span.start
                },
                |(reference, _, _)| reference,
            );
            if let Some(anchor) = boundary.checked_sub(1) {
                facts.insertion = insertion_facts(scene, anchor);
            }
        }
        AlignmentComponent::Deletion { cigar_index } => {
            if let Some((start, _, length)) = origin(cigar_index) {
                facts.deletion = deletion_facts(scene, start, u64::from(length));
            }
        }
        AlignmentComponent::ModifiedBase {
            read_index,
            canonical_base,
            code,
            ..
        } => {
            let site = modification_summary_site(record, span.start, read_index, canonical_base);
            facts.modification =
                modification_tally(scene, site).map(|tally| ModificationFacts { tally, code });
        }
        AlignmentComponent::SoftClip { cigar_index, .. }
        | AlignmentComponent::ClipGhostBase { cigar_index, .. } => {
            facts.clipped_here =
                record.and_then(|record| clipped_here(scene, record, cigar_index as usize));
        }
        _ => {}
    }
    facts
}

#[cfg(test)]
mod tests;
