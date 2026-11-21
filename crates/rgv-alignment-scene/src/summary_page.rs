use std::{mem::size_of, sync::Arc};

use rgv_core::{BisulfiteContext, ContigId, GenomicInterval};
use rgv_model::{
    JunctionEvidence, ModificationBin, ModificationSeriesKey, PairSummary, PileupBin,
    ReferenceSlice, bisulfite_context_matches,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    AnnotationRevision, CanonicalGenomicTile, ContractFingerprint, ExactSummaryQueryError,
    ExactSummaryRoot, JunctionAnnotationIndex, JunctionAnnotationStatus, LedgerError,
    ModificationSummaryContract, ReferenceRevision, ReservationKind, TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SummaryPageFamily {
    Depth,
    Alleles,
    Junctions,
    Modifications,
    Pairs,
    FilterProvenance,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct FilterPageScope {
    contig: ContigId,
    start: u64,
    end: u64,
}

impl From<GenomicInterval> for FilterPageScope {
    fn from(interval: GenomicInterval) -> Self {
        Self {
            contig: interval.contig(),
            start: interval.start(),
            end: interval.end(),
        }
    }
}

/// Camera-independent content identity for one exact derived-lane semantic page.
///
/// Evidence-root and summary-root publication revisions are deliberately absent. An immutable
/// exact tile for the same source, record contract, family contract, and dependencies has the
/// same biological contents when a wider compatible root is published. Keeping publication
/// revisions here would force the presentation cache and GPU page table to rebuild unchanged
/// tiles on every prepared-horizon extension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SummaryPageKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    family_contract: ContractFingerprint,
    /// Pair statistics can contain a fact canonically owned by an earlier tile. This stable
    /// dependency span changes only when the contributing exact-tile set changes.
    dependency_first_tile: u64,
    dependency_last_tile: u64,
    junction_normalization_mapped_records: Option<u64>,
    filter_coverage: Option<ContractFingerprint>,
    filter_scope: Option<FilterPageScope>,
    reference_revision: Option<ReferenceRevision>,
    annotation_revision: Option<AnnotationRevision>,
    tile: CanonicalGenomicTile,
    family: SummaryPageFamily,
}

impl SummaryPageKey {
    #[must_use]
    pub fn new(
        root: &ExactSummaryRoot,
        tile: CanonicalGenomicTile,
        family: SummaryPageFamily,
    ) -> Self {
        Self::new_with_annotation(root, tile, family, None)
    }

    #[must_use]
    pub fn new_with_annotation(
        root: &ExactSummaryRoot,
        tile: CanonicalGenomicTile,
        family: SummaryPageFamily,
        annotation: Option<&JunctionAnnotationIndex>,
    ) -> Self {
        Self::new_with_dependencies(root, tile, family, None, annotation)
    }

    #[must_use]
    pub fn new_with_dependencies(
        root: &ExactSummaryRoot,
        tile: CanonicalGenomicTile,
        family: SummaryPageFamily,
        reference_revision: Option<ReferenceRevision>,
        annotation: Option<&JunctionAnnotationIndex>,
    ) -> Self {
        let (mut dependency_first_tile, dependency_last_tile) =
            summary_page_dependency_span(root, tile, family);
        if family == SummaryPageFamily::Junctions && !root.entering_junctions_for(tile).is_empty() {
            // The first prepared tile also carries junctions which start left of it. A root
            // prepared further left owns those in an earlier tile instead, so the two pages of
            // this tile differ and must not share a cache identity.
            dependency_first_tile = dependency_first_tile.min(tile.coordinate().saturating_sub(1));
        }
        let junction_normalization_mapped_records = (family == SummaryPageFamily::Junctions)
            .then(|| {
                root.junction_tiles()
                    .iter()
                    .find(|summary| summary.tile() == tile)
                    .and_then(|summary| summary.summary().provenance.normalization_mapped_records)
            })
            .flatten();
        let filter = (family == SummaryPageFamily::FilterProvenance)
            .then(|| {
                root.filter_tiles()
                    .iter()
                    .find(|summary| summary.tile() == tile)
            })
            .flatten();
        Self {
            source_revision: root.source_revision(),
            record_contract: root.record_contract(),
            family_contract: summary_family_contract(root, family),
            dependency_first_tile,
            dependency_last_tile,
            junction_normalization_mapped_records,
            filter_coverage: filter.map(|summary| summary.coverage_fingerprint()),
            filter_scope: filter.map(|summary| summary.provenance_scope().into()),
            reference_revision: (family == SummaryPageFamily::Alleles)
                .then_some(reference_revision)
                .flatten(),
            annotation_revision: (family == SummaryPageFamily::Junctions)
                .then(|| annotation.map(JunctionAnnotationIndex::revision))
                .flatten(),
            tile,
            family,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn reference_revision(self) -> Option<ReferenceRevision> {
        self.reference_revision
    }

    #[must_use]
    pub const fn annotation_revision(self) -> Option<AnnotationRevision> {
        self.annotation_revision
    }

    #[must_use]
    pub const fn junction_normalization_mapped_records(self) -> Option<u64> {
        self.junction_normalization_mapped_records
    }

    #[must_use]
    pub const fn tile(self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn family(self) -> SummaryPageFamily {
        self.family
    }
}

fn summary_family_contract(
    root: &ExactSummaryRoot,
    family: SummaryPageFamily,
) -> ContractFingerprint {
    match family {
        SummaryPageFamily::Depth => root.contract().depth().fingerprint(),
        SummaryPageFamily::Alleles => root.contract().alleles().fingerprint(),
        SummaryPageFamily::Junctions => root.contract().junctions().fingerprint(),
        SummaryPageFamily::Modifications => root
            .contract()
            .modifications()
            .unwrap_or(ModificationSummaryContract)
            .fingerprint(),
        SummaryPageFamily::Pairs => root.contract().pairs().fingerprint(),
        SummaryPageFamily::FilterProvenance => root.contract().filters().fingerprint(),
    }
}

fn summary_page_dependency_span(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    family: SummaryPageFamily,
) -> (u64, u64) {
    let Some(interval) = root
        .depth_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
        .map(|summary| summary.depth().interval())
    else {
        return (tile.coordinate(), tile.coordinate());
    };
    let contributor_coordinates = match family {
        SummaryPageFamily::Pairs => root
            .pair_tiles()
            .iter()
            .filter(|summary| summary.tile().contig() == interval.contig())
            .filter(|summary| {
                summary
                    .tile()
                    .coordinate()
                    .checked_mul(summary.tile().bases().get())
                    .is_some_and(|start| start < interval.end())
                    && summary
                        .maximum_record_end()
                        .is_some_and(|end| end > interval.start())
            })
            .map(|summary| summary.tile().coordinate())
            .collect::<Vec<_>>(),
        SummaryPageFamily::Depth
        | SummaryPageFamily::Alleles
        | SummaryPageFamily::Junctions
        | SummaryPageFamily::Modifications
        | SummaryPageFamily::FilterProvenance => Vec::new(),
    };
    contributor_coordinates.into_iter().fold(
        (tile.coordinate(), tile.coordinate()),
        |(first, last), coordinate| (first.min(coordinate), last.max(coordinate)),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummarySemanticComponent {
    Depth {
        depth: u32,
        forward_depth: u32,
        reverse_depth: u32,
        mapping_quality_sum: u64,
        mapping_quality_observations: u32,
    },
    Alleles {
        bin: PileupBin,
        /// The immutable FASTA base whose revision is part of this allele page's key. `None`
        /// means the scene deliberately requested reference-independent allele facts; consumers
        /// must not label those observations as substitutions.
        reference_base: Option<u8>,
    },
    Junction(AnnotatedJunctionSemantic),
    Modification {
        series: ModificationSeriesKey,
        bin: ModificationBin,
    },
    PairCandidate {
        handle: crate::AlignmentHandle,
        score: u64,
        template_length: u64,
    },
    PairStatistics(PairSummary),
    FilterProvenance(crate::CompleteCoverage),
}

/// Exact, reference-revision-backed bisulfite context at one allele site.
///
/// The masks are computed while the immutable FASTA authority is present, using the same matcher
/// as the legacy layout. Semantic pages therefore retain enough context for any captured
/// bisulfite policy without retaining a mutable or oversized reference slice.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SummaryAlleleReferenceContext {
    genomic_bases: [Option<u8>; 5],
    forward: u8,
    reverse: u8,
}

impl SummaryAlleleReferenceContext {
    #[must_use]
    pub fn matches(self, reverse: bool, context: BisulfiteContext) -> bool {
        let mask = if reverse { self.reverse } else { self.forward };
        mask & bisulfite_context_bit(context) != 0
    }

    /// Stable bitset for every exact bisulfite context supported at this reference position.
    #[must_use]
    pub const fn context_mask(self, reverse: bool) -> u8 {
        if reverse { self.reverse } else { self.forward }
    }

    /// Exact uppercase FASTA base at a genomic offset in `-2..=2`; `None` is possible only past
    /// a true contig boundary because page construction validates the complete bounded halo.
    #[must_use]
    pub fn genomic_base(self, offset: i8) -> Option<u8> {
        let index = usize::try_from(i16::from(offset) + 2).ok()?;
        self.genomic_bases.get(index).copied().flatten()
    }

    pub(crate) fn capture(reference: &ReferenceSlice, position: u64) -> Self {
        let mut captured = Self {
            genomic_bases: [-2_i64, -1, 0, 1, 2].map(|offset| {
                position
                    .checked_add_signed(offset)
                    .and_then(|position| reference.base_at(position))
                    .map(|base| base.to_ascii_uppercase())
            }),
            ..Self::default()
        };
        for context in [
            BisulfiteContext::Cg,
            BisulfiteContext::Chg,
            BisulfiteContext::Chh,
            BisulfiteContext::Hcg,
            BisulfiteContext::Gch,
            BisulfiteContext::Wcg,
        ] {
            let bit = bisulfite_context_bit(context);
            if bisulfite_context_matches(reference, position, false, context) {
                captured.forward |= bit;
            }
            if bisulfite_context_matches(reference, position, true, context) {
                captured.reverse |= bit;
            }
        }
        captured
    }
}

const fn bisulfite_context_bit(context: BisulfiteContext) -> u8 {
    1 << match context {
        BisulfiteContext::Cg => 0,
        BisulfiteContext::Chg => 1,
        BisulfiteContext::Chh => 2,
        BisulfiteContext::Hcg => 3,
        BisulfiteContext::Gch => 4,
        BisulfiteContext::Wcg => 5,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnotatedJunctionSemantic {
    pub evidence: JunctionEvidence,
    /// Stable expanded-layout row assigned across the complete immutable prepared horizon.
    /// Collapsed presentation ignores this value.
    pub expanded_row: u32,
    /// `None` means no annotation authority was requested for this scene. `Some(Unknown)` means
    /// an authority exists but does not certify this junction's interval.
    pub annotation: Option<JunctionAnnotationStatus>,
}

/// Canonically tile-owned raw summary fact. Offsets are relative to the owner interval; a
/// junction's end offset can extend beyond that tile. Display transforms, thresholds, scale
/// maxima, labels, and camera pixels are projection state and never alter this exact authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SummarySemanticPrimitive {
    pub start_offset: u64,
    pub end_offset: u64,
    pub component: SummarySemanticComponent,
}

#[derive(Debug)]
pub struct SummarySemanticPage {
    key: SummaryPageKey,
    /// Canonical ownership interval used to decode tile-local primitive offsets.
    interval: GenomicInterval,
    /// Coarse page index covering the owner tile and every primitive's complete genomic span.
    /// A junction may extend beyond `interval`, but it remains stored only in its start tile.
    indexed_interval: GenomicInterval,
    primitives: Arc<[SummarySemanticPrimitive]>,
    allele_reference_contexts: Option<Arc<[SummaryAlleleReferenceContext]>>,
    allocation: AllocationLease,
}

impl SummarySemanticPage {
    pub fn build(
        key: SummaryPageKey,
        root: &Arc<ExactSummaryRoot>,
        annotation: Option<&JunctionAnnotationIndex>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SummarySemanticPageError> {
        Self::build_with_dependencies(
            key,
            root,
            None,
            annotation,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_with_dependencies(
        key: SummaryPageKey,
        root: &Arc<ExactSummaryRoot>,
        reference: Option<(ReferenceRevision, &ReferenceSlice, u64)>,
        annotation: Option<&JunctionAnnotationIndex>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SummarySemanticPageError> {
        validate_key(key, root, reference, annotation)?;
        let interval = root
            .depth_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map(|tile| tile.depth().interval())
            .ok_or(SummarySemanticPageError::MissingCanonicalTile)?;
        if let Some((_, reference, contig_length)) = reference
            && key.family == SummaryPageFamily::Alleles
        {
            let required = allele_reference_context_interval(interval, contig_length)?;
            if !reference.interval.contains_interval(required) {
                return Err(SummarySemanticPageError::ReferenceCoverageMismatch {
                    required,
                    available: reference.interval,
                });
            }
            if reference.base_at(required.start()).is_none()
                || reference
                    .base_at(required.end().saturating_sub(1))
                    .is_none()
            {
                return Err(SummarySemanticPageError::ReferenceBaseCoverageMismatch {
                    required,
                    available: reference.interval,
                    available_bases: reference.bases.len(),
                });
            }
        }
        let maximum_primitives = maximum_primitives(root, key, interval)?;
        let maximum_reference_contexts =
            usize::from(key.family == SummaryPageFamily::Alleles && reference.is_some())
                .saturating_mul(maximum_primitives);
        let maximum_bytes = page_bytes(maximum_primitives, maximum_reference_contexts);
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::SummaryPageBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::SemanticPage,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut primitives = Vec::with_capacity(maximum_primitives);
        match key.family {
            SummaryPageFamily::Depth => emit_depth(root, key.tile, interval, &mut primitives),
            SummaryPageFamily::Alleles => emit_alleles(
                root,
                key.tile,
                interval,
                reference.map(|(_, reference, _)| reference),
                &mut primitives,
            ),
            SummaryPageFamily::Junctions => {
                emit_junctions(root, key.tile, interval, annotation, &mut primitives)?;
            }
            SummaryPageFamily::Modifications => {
                emit_modifications(root, key.tile, interval, &mut primitives);
            }
            SummaryPageFamily::Pairs => emit_pairs(root, key.tile, interval, &mut primitives)?,
            SummaryPageFamily::FilterProvenance => {
                emit_filter_provenance(root, key.tile, interval, &mut primitives)?;
            }
        }
        primitives.sort_by_key(|primitive| (primitive.start_offset, primitive.end_offset));
        let allele_reference_contexts = (key.family == SummaryPageFamily::Alleles)
            .then(|| {
                reference.map(|(_, reference, _)| {
                    primitives
                        .iter()
                        .map(|primitive| {
                            SummaryAlleleReferenceContext::capture(
                                reference,
                                interval.start().saturating_add(primitive.start_offset),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .flatten();
        let indexed_end = primitives.iter().fold(interval.end(), |end, primitive| {
            end.max(interval.start().saturating_add(primitive.end_offset))
        });
        let indexed_interval =
            GenomicInterval::new(interval.contig(), interval.start(), indexed_end)
                .expect("a page index always contains its non-empty canonical owner tile");
        primitives.shrink_to_fit();
        let allele_reference_contexts = allele_reference_contexts.map(|mut contexts| {
            contexts.shrink_to_fit();
            Arc::<[SummaryAlleleReferenceContext]>::from(contexts)
        });
        let retained_bytes = page_bytes(
            primitives.len(),
            allele_reference_contexts
                .as_ref()
                .map_or(0, |contexts| contexts.len()),
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            key,
            interval,
            indexed_interval,
            primitives: primitives.into(),
            allele_reference_contexts,
            allocation,
        }))
    }

    #[must_use]
    pub const fn key(&self) -> SummaryPageKey {
        self.key
    }

    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    /// Returns the page-table query extent. For junction pages this can be wider than the
    /// canonical owner interval so a partial camera can find a junction through its sole page.
    #[must_use]
    pub const fn indexed_interval(&self) -> GenomicInterval {
        self.indexed_interval
    }

    #[must_use]
    pub fn primitives(&self) -> &[SummarySemanticPrimitive] {
        &self.primitives
    }

    /// Contexts are parallel to `primitives()` and exist only for a reference-backed allele page.
    #[must_use]
    pub fn allele_reference_contexts(&self) -> Option<&[SummaryAlleleReferenceContext]> {
        self.allele_reference_contexts.as_deref()
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }
}

fn validate_key(
    key: SummaryPageKey,
    root: &ExactSummaryRoot,
    reference: Option<(ReferenceRevision, &ReferenceSlice, u64)>,
    annotation: Option<&JunctionAnnotationIndex>,
) -> Result<(), SummarySemanticPageError> {
    if key
        != SummaryPageKey::new_with_dependencies(
            root,
            key.tile,
            key.family,
            reference.map(|(revision, _, _)| revision),
            annotation,
        )
    {
        return Err(SummarySemanticPageError::AuthorityMismatch);
    }
    Ok(())
}

fn allele_reference_context_interval(
    interval: GenomicInterval,
    contig_length: u64,
) -> Result<GenomicInterval, SummarySemanticPageError> {
    if interval.end() > contig_length {
        return Err(SummarySemanticPageError::ReferenceContigLengthMismatch {
            interval,
            contig_length,
        });
    }
    GenomicInterval::new(
        interval.contig(),
        interval.start().saturating_sub(2),
        interval.end().saturating_add(2).min(contig_length),
    )
    .map_err(
        |_| SummarySemanticPageError::ReferenceContigLengthMismatch {
            interval,
            contig_length,
        },
    )
}

fn maximum_primitives(
    root: &ExactSummaryRoot,
    key: SummaryPageKey,
    interval: GenomicInterval,
) -> Result<usize, SummarySemanticPageError> {
    Ok(match key.family {
        SummaryPageFamily::Depth => root
            .depth_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(0, |tile| tile.depth().runs().len()),
        SummaryPageFamily::Alleles => {
            usize::try_from(interval.len()).map_err(|_| SummarySemanticPageError::TileTooWide)?
        }
        SummaryPageFamily::Junctions => root
            .junction_tiles()
            .iter()
            .find(|summary| summary.tile() == key.tile)
            .map_or(0, |summary| summary.summary().junctions.len())
            .saturating_add(root.entering_junctions_for(key.tile).len()),
        SummaryPageFamily::Modifications => root
            .modification_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(0, |tile| {
                tile.summary()
                    .series
                    .len()
                    .saturating_mul(tile.summary().bin_count())
            }),
        SummaryPageFamily::Pairs => root
            .pair_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(1, |tile| tile.candidates().len().saturating_add(1)),
        SummaryPageFamily::FilterProvenance => root
            .filter_tiles()
            .iter()
            .find(|summary| summary.tile() == key.tile)
            .map_or(0, |summary| summary.provenances().len()),
    })
}

fn emit_depth(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .depth_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    output.extend(
        summary
            .depth()
            .runs()
            .iter()
            .map(|run| SummarySemanticPrimitive {
                start_offset: run.start - interval.start(),
                end_offset: run.end - interval.start(),
                component: SummarySemanticComponent::Depth {
                    depth: run.depth,
                    forward_depth: run.forward_depth,
                    reverse_depth: run.reverse_depth,
                    mapping_quality_sum: run.mapping_quality_sum,
                    mapping_quality_observations: run.mapping_quality_observations,
                },
            }),
    );
}

fn emit_alleles(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    reference: Option<&ReferenceSlice>,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .allele_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    output.extend(
        summary
            .sites()
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, bin)| {
                let observations = bin
                    .alleles
                    .reference_observations()
                    .saturating_add(bin.alleles.insertion);
                (observations > 0).then(|| SummarySemanticPrimitive {
                    start_offset: index as u64,
                    end_offset: (index as u64).saturating_add(1).min(interval.len()),
                    component: SummarySemanticComponent::Alleles {
                        bin,
                        reference_base: reference
                            .and_then(|reference| {
                                reference.base_at(interval.start().saturating_add(index as u64))
                            })
                            .map(|base| base.to_ascii_uppercase()),
                    },
                })
            }),
    );
}

fn emit_junctions(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    annotation: Option<&JunctionAnnotationIndex>,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    let summary = root
        .junction_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
        .ok_or(SummarySemanticPageError::MissingCanonicalTile)?;
    // Junctions entering the first prepared tile from the left have no owner page in this root.
    // Their page-relative span is clipped to the tile start (offsets are unsigned); `evidence`
    // keeps the true biological start for projection, labels and inspection.
    let entering = root.entering_junctions_for(tile).iter().copied();
    let owned = summary.summary().junctions.iter().copied();
    output.extend(entering.chain(owned).map(|junction| {
        debug_assert!(interval.contains(junction.start) || junction.start < interval.start());
        SummarySemanticPrimitive {
            start_offset: junction.start.saturating_sub(interval.start()),
            // Preserve the complete biological extent. Later tiles index this owner page rather
            // than storing clipped copies of the same junction.
            end_offset: junction.end.saturating_sub(interval.start()),
            component: SummarySemanticComponent::Junction(AnnotatedJunctionSemantic {
                evidence: junction,
                expanded_row: root
                    .expanded_junction_row(junction.start, junction.end)
                    .expect("every exact root junction has an expanded-row assignment"),
                annotation: annotation
                    .map(|index| index.status(interval.contig(), junction.start, junction.end)),
            }),
        }
    }));
    Ok(())
}

fn emit_modifications(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .modification_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    for series in &summary.summary().series {
        for (index, bin) in series.bins.iter().copied().enumerate() {
            if bin.canonical_observations == 0 && bin.modified_calls == 0 {
                continue;
            }
            output.push(SummarySemanticPrimitive {
                start_offset: index as u64,
                end_offset: (index as u64).saturating_add(1).min(interval.len()),
                component: SummarySemanticComponent::Modification {
                    series: series.key,
                    bin,
                },
            });
        }
    }
}

fn emit_pairs(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    let Some(summary) = root
        .pair_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return Ok(());
    };
    output.extend(summary.candidates().iter().map(|candidate| {
        SummarySemanticPrimitive {
            start_offset: candidate.record_start.saturating_sub(interval.start()),
            end_offset: candidate
                .record_end
                .saturating_sub(interval.start())
                .min(interval.len()),
            component: SummarySemanticComponent::PairCandidate {
                handle: candidate.handle,
                score: candidate.score,
                template_length: candidate.template_length,
            },
        }
    }));
    output.push(SummarySemanticPrimitive {
        start_offset: 0,
        end_offset: interval.len(),
        component: SummarySemanticComponent::PairStatistics(root.pair_summary(interval)?),
    });
    Ok(())
}

fn emit_filter_provenance(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    let provenances = root
        .filter_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
        .ok_or(SummarySemanticPageError::MissingCanonicalTile)?
        .provenances();
    for provenance in provenances.iter().copied() {
        output.push(SummarySemanticPrimitive {
            start_offset: 0,
            end_offset: interval.len(),
            component: SummarySemanticComponent::FilterProvenance(provenance),
        });
    }
    Ok(())
}

/// Exact page keys required to project an interval. Ordinary families are owned by overlapping
/// canonical tiles. Junction pages are additionally indexed by complete junction extent, so an
/// owner tile immediately before a partial prepared interval is retained exactly once.
pub(crate) fn required_summary_page_keys(
    root: &ExactSummaryRoot,
    interval: GenomicInterval,
    reference_revision: Option<ReferenceRevision>,
    annotation: Option<&JunctionAnnotationIndex>,
) -> Vec<SummaryPageKey> {
    let families = required_summary_families(root);
    let mut keys = Vec::new();
    for tile in root
        .depth_tiles()
        .iter()
        .filter(|tile| tile.depth().interval().overlaps(interval))
        .map(|tile| tile.tile())
    {
        for family in families.iter().copied() {
            keys.push(SummaryPageKey::new_with_dependencies(
                root,
                tile,
                family,
                reference_revision,
                annotation,
            ));
        }
    }
    for tile in root.junction_tiles().iter().filter(|tile| {
        tile.summary()
            .junctions
            .iter()
            .chain(root.entering_junctions_for(tile.tile()))
            .any(|junction| junction.start < interval.end() && junction.end > interval.start())
    }) {
        keys.push(SummaryPageKey::new_with_dependencies(
            root,
            tile.tile(),
            SummaryPageFamily::Junctions,
            reference_revision,
            annotation,
        ));
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn required_summary_families(root: &ExactSummaryRoot) -> Vec<SummaryPageFamily> {
    let mut families = vec![
        SummaryPageFamily::Depth,
        SummaryPageFamily::Junctions,
        SummaryPageFamily::Pairs,
        SummaryPageFamily::FilterProvenance,
    ];
    if root.contract().allele_detail != crate::AlleleSummaryDetail::None {
        families.push(SummaryPageFamily::Alleles);
    }
    if root.contract().collect_modifications {
        families.push(SummaryPageFamily::Modifications);
    }
    families
}

const fn page_bytes(primitives: usize, allele_reference_contexts: usize) -> usize {
    size_of::<SummarySemanticPage>()
        .saturating_add(primitives.saturating_mul(size_of::<SummarySemanticPrimitive>()))
        .saturating_add(
            allele_reference_contexts.saturating_mul(size_of::<SummaryAlleleReferenceContext>()),
        )
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SummarySemanticPageError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Query(#[from] ExactSummaryQueryError),
    #[error("summary page key does not match its exact summary authority")]
    AuthorityMismatch,
    #[error("summary page canonical tile is absent from the exact summary root")]
    MissingCanonicalTile,
    #[error(
        "summary allele page requires reference coverage {required:?}, but only {available:?} is available"
    )]
    ReferenceCoverageMismatch {
        required: GenomicInterval,
        available: GenomicInterval,
    },
    #[error(
        "summary allele page requires reference bases for {required:?}, but slice {available:?} contains only {available_bases} bases"
    )]
    ReferenceBaseCoverageMismatch {
        required: GenomicInterval,
        available: GenomicInterval,
        available_bases: usize,
    },
    #[error(
        "summary allele page interval {interval:?} is outside reference contig length {contig_length}"
    )]
    ReferenceContigLengthMismatch {
        interval: GenomicInterval,
        contig_length: u64,
    },
    #[error("summary page canonical tile is too wide for this platform")]
    TileTooWide,
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId,
        OverlappingMatePolicy, ReadStackingMode,
    };
    use rgv_model::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, SequenceData,
        TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AlignmentPageKey, AlignmentPresentationScene, AlignmentPresentationStore,
        AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSceneSnapshotError,
        AlignmentSemanticPage, AlignmentSourceRevision, AlleleSummaryDetail,
        AllocationBundleActual, AllocationBundleDemand, AllocationBundleError,
        AllocationBundleSlot, AllocationBundleSpec, AllocationClass, AllocationLease,
        AllocationRetirementClaimSet, AllocationRetirementToken, CanonicalRowBlock,
        CapabilityRequirement, CompleteCoverage, EvidenceAcquisition, EvidenceChunk,
        EvidenceRevision, EvidenceRoot, EvidenceRootBuilder, ExactAlleleSummaryTile,
        ExactDepthSummaryTile, ExactFilterSummaryTile, ExactJunctionSummaryTile,
        ExactModificationSummaryTile, ExactPairSummaryTile, FilteredRecordProvenance, GeometryLod,
        LayoutContract, PayloadContract, PayloadSidecar, PresentationContract, RevisionFingerprint,
        RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint, SummaryContract,
        SummaryRevision,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(77);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn exact_authority(ledger: &AllocationLedger) -> (Arc<EvidenceRoot>, Arc<ExactSummaryRoot>) {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 23,
            flags: AlignmentFlags::from_bits(0x63),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(50),
            template_length: 60,
            read_name: b"spliced-pair".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
            ],
            sequence: SequenceData::Present {
                bases: b"ACGTCACGTC".to_vec(),
                qualities: Some(vec![35; 10]),
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"XS",
                    value: AuxValue::Character(b'+'),
                },
                AuxField {
                    tag: *b"MM",
                    value: AuxValue::String("C+m,0;".to_owned()),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(vec![220]),
                },
                AuxField {
                    tag: *b"HP",
                    value: AuxValue::Unsigned(1),
                },
            ],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((5 << 16) | 1),
            )
            .unwrap();
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = crate::CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 5,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let payload_allocation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(payload_bytes, 1)
            .unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, 1)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(6, 7)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(2).unwrap();
        let tile = CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap());
        let contract = SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::Counts,
            collect_modifications: true,
        };
        let depth = ExactDepthSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.depth(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let alleles = ExactAlleleSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.alleles(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let junctions = ExactJunctionSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.junctions(),
            Some(1_000),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let pairs = ExactPairSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.pairs(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let filters = ExactFilterSummaryTile::build(
            &evidence,
            tile,
            100,
            interval,
            contract.filters(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let modifications = ExactModificationSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.modifications().unwrap(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let root = ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(8, 9)),
            evidence.revision(),
            evidence.source_revision(),
            evidence.record_contract(),
            contract,
            interval,
            vec![depth],
            vec![alleles],
            vec![junctions],
            vec![pairs],
            vec![filters],
            vec![modifications],
            ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();
        (evidence, root)
    }

    fn exact_root(ledger: &AllocationLedger) -> Arc<ExactSummaryRoot> {
        exact_authority(ledger).1
    }

    fn exclusively_owned_scene_snapshot(ledger: &AllocationLedger) -> Arc<AlignmentSceneSnapshot> {
        let (evidence, summaries) = exact_authority(ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(90, 91)),
            &evidence,
            interval,
            layout,
            ledger,
            ReservationKind::Normal,
            90,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let tile = summaries.depth_tiles()[0].tile();
        let row_block = CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap());
        let read_page = AlignmentSemanticPage::build(
            AlignmentPageKey::new(
                evidence.source_revision(),
                evidence.revision(),
                atlas.revision(),
                layout.fingerprint(),
                presentation.page_fingerprint(crate::AlignmentPageFamily::ReadGeometry),
                tile,
                row_block,
                crate::AlignmentPageFamily::ReadGeometry,
                GeometryLod::Span,
            ),
            &evidence,
            &atlas,
            presentation,
            100,
            None,
            ledger,
            ReservationKind::Normal,
            91,
        )
        .unwrap();
        let revision = SceneRevision::new(RevisionFingerprint::new(92, 93));
        let reads = AlignmentPresentationScene::new(
            revision,
            Arc::clone(&evidence),
            Arc::clone(&atlas),
            presentation,
            interval,
            0..1,
            vec![read_page],
            ledger,
            ReservationKind::Normal,
            92,
        )
        .unwrap();
        let pages = [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, family)| {
            SummarySemanticPage::build(
                SummaryPageKey::new(&summaries, tile, family),
                &summaries,
                None,
                ledger,
                ReservationKind::Normal,
                100 + index as u64,
            )
            .unwrap()
        })
        .collect();
        AlignmentSceneSnapshot::new(
            revision,
            reads,
            summaries,
            pages,
            ledger,
            ReservationKind::Normal,
            110,
        )
        .unwrap()
    }

    struct OwnedSnapshotAndGpuRetirement {
        claims: AllocationRetirementClaimSet,
        _snapshot: Arc<AlignmentSceneSnapshot>,
        _gpu: AllocationLease,
    }

    impl AllocationRetirementToken for OwnedSnapshotAndGpuRetirement {
        fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
            &self.claims
        }
    }

    #[test]
    fn semantic_and_gpu_epochs_must_supply_one_exact_atomic_retirement_credit() {
        let ledger = AllocationLedger::new(64 << 20, 64 << 20);
        let snapshot = exclusively_owned_scene_snapshot(&ledger);
        let old_gpu_bytes = 4 << 10;
        let old_gpu = ledger
            .reserve(
                AllocationClass::GpuPage,
                old_gpu_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(old_gpu_bytes, 120)
            .unwrap();
        let before = ledger.snapshot().unwrap();
        let (semantic_claims, stats) = crate::build_alignment_semantic_retirement_claim_set(
            &ledger,
            std::iter::once(&snapshot),
        )
        .unwrap();
        assert_eq!(
            semantic_claims.len().saturating_add(1),
            before.records.len()
        );
        assert_eq!(stats.exclusively_owned_evidence_roots, 1);
        assert_eq!(stats.exclusively_owned_row_atlases, 1);
        assert_eq!(stats.exclusively_owned_summary_roots, 1);
        let gpu_claims = {
            let mut builder = ledger.retirement_claim_builder();
            builder.include_lease(&old_gpu).unwrap();
            builder.finish().unwrap()
        };
        let combined_claims = ledger
            .combine_retirement_claim_sets([&semantic_claims, &gpu_claims])
            .unwrap();

        let candidate_bytes = 4 << 10;
        ledger.set_budget(candidate_bytes).unwrap();
        let progress = ledger.progress().unwrap();
        let slot = AllocationBundleSlot::new(0);
        let demands = vec![AllocationBundleDemand::new(
            slot,
            AllocationClass::GpuPage,
            candidate_bytes,
        )];
        assert!(matches!(
            ledger.reserve_replacement_bundle(AllocationBundleSpec::new(
                progress.policy_revision,
                demands.clone(),
                semantic_claims,
            )),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));
        assert!(matches!(
            ledger.reserve_replacement_bundle(AllocationBundleSpec::new(
                progress.policy_revision,
                demands.clone(),
                gpu_claims,
            )),
            Err(AllocationBundleError::SteadyBudgetExceeded { .. })
        ));

        let reservation = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                progress.policy_revision,
                demands,
                combined_claims.clone(),
            ))
            .unwrap();
        let commit = reservation
            .commit(vec![AllocationBundleActual::new(
                slot,
                candidate_bytes,
                121,
            )])
            .unwrap();
        let mut retirement = Some(OwnedSnapshotAndGpuRetirement {
            claims: combined_claims,
            _snapshot: snapshot,
            _gpu: old_gpu,
        });
        let candidate = commit.retire_and_settle(&mut retirement).unwrap();
        assert_eq!(candidate.len(), 1);
        assert!(retirement.is_none());
        assert_eq!(ledger.snapshot().unwrap().charged, candidate_bytes);
        drop(candidate);
        assert_eq!(ledger.snapshot().unwrap().charged, 0);
    }

    const CROSS_TILE_CONTIG_LENGTH: u64 = 300;
    const CROSS_TILE_NORMALIZATION: u64 = 1_000;

    fn cross_tile_interval(tile_count: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), 0, tile_count.saturating_mul(100)).unwrap()
    }

    fn cross_tile_tile(coordinate: u64) -> CanonicalGenomicTile {
        CanonicalGenomicTile::new(ContigId::new(0), coordinate, NonZeroU64::new(100).unwrap())
    }

    fn cross_tile_contract() -> SummaryContract {
        SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::None,
            collect_modifications: false,
        }
    }

    fn cross_tile_evidence(ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let interval = cross_tile_interval(3);
        let record = AlignmentRecord {
            contig: interval.contig(),
            start: 80,
            end: 230,
            flags: AlignmentFlags::from_bits(0x10),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"cross-tile-splice".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 10,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 120,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 20,
                },
            ],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: vec![AuxField {
                tag: *b"XS",
                value: AuxValue::Character(b'+'),
            }],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((71 << 16) | 1),
            )
            .unwrap();
        let retained_tags = [*b"XS"];
        let payload_contract = PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            retained_tags,
            None,
        );
        let payload = Arc::new(
            AlignmentPayloadBatch::from_batch(&decoded, false, false, Some(&retained_tags))
                .unwrap(),
        );
        let core = crate::CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 71,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let payload_allocation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(payload_bytes, 70)
            .unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, 70)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, 70)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let acquisition = EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                RECORD_CONTRACT,
                interval,
                FilteredRecordProvenance {
                    records_seen: 1,
                    eligible_records: 1,
                    ..FilteredRecordProvenance::default()
                },
            )
            .unwrap(),
            [chunk],
        )
        .unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(70, 71)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(72).unwrap()
    }

    fn cross_tile_root_reusing(
        evidence: &Arc<EvidenceRoot>,
        tile_count: u64,
        revision_seed: u64,
        reused: Option<&ExactSummaryRoot>,
        ledger: &AllocationLedger,
    ) -> Arc<ExactSummaryRoot> {
        cross_tile_root_over(evidence, 0..tile_count, revision_seed, reused, ledger)
    }

    fn cross_tile_root_over(
        evidence: &Arc<EvidenceRoot>,
        tiles: std::ops::Range<u64>,
        revision_seed: u64,
        reused: Option<&ExactSummaryRoot>,
        ledger: &AllocationLedger,
    ) -> Arc<ExactSummaryRoot> {
        let contract = cross_tile_contract();
        assert!(reused.is_none_or(|root| root.contract() == contract));
        let prepared =
            GenomicInterval::new(ContigId::new(0), tiles.start * 100, tiles.end * 100).unwrap();
        let mut depth = Vec::new();
        let mut junctions = Vec::new();
        let mut pairs = Vec::new();
        let mut filters = Vec::new();
        for coordinate in tiles {
            let tile = cross_tile_tile(coordinate);
            let publication_revision = revision_seed.saturating_add(coordinate);
            depth.push(
                reused
                    .and_then(|root| {
                        root.depth_tiles()
                            .iter()
                            .find(|summary| summary.tile() == tile)
                            .cloned()
                    })
                    .unwrap_or_else(|| {
                        ExactDepthSummaryTile::build(
                            evidence,
                            tile,
                            CROSS_TILE_CONTIG_LENGTH,
                            contract.depth(),
                            ledger,
                            ReservationKind::Normal,
                            publication_revision,
                        )
                        .unwrap()
                    }),
            );
            junctions.push(
                reused
                    .and_then(|root| {
                        root.junction_tiles()
                            .iter()
                            .find(|summary| summary.tile() == tile)
                            .cloned()
                    })
                    .unwrap_or_else(|| {
                        ExactJunctionSummaryTile::build(
                            evidence,
                            tile,
                            CROSS_TILE_CONTIG_LENGTH,
                            contract.junctions(),
                            Some(CROSS_TILE_NORMALIZATION),
                            ledger,
                            ReservationKind::Normal,
                            publication_revision,
                        )
                        .unwrap()
                    }),
            );
            pairs.push(
                reused
                    .and_then(|root| {
                        root.pair_tiles()
                            .iter()
                            .find(|summary| summary.tile() == tile)
                            .cloned()
                    })
                    .unwrap_or_else(|| {
                        ExactPairSummaryTile::build(
                            evidence,
                            tile,
                            CROSS_TILE_CONTIG_LENGTH,
                            contract.pairs(),
                            ledger,
                            ReservationKind::Normal,
                            publication_revision,
                        )
                        .unwrap()
                    }),
            );
            // Filter certificates name the complete prepared scope, so extension deliberately
            // rebuilds this family while reusing the tile-owned biological summaries above.
            filters.push(
                ExactFilterSummaryTile::build(
                    evidence,
                    tile,
                    CROSS_TILE_CONTIG_LENGTH,
                    prepared,
                    contract.filters(),
                    ledger,
                    ReservationKind::Normal,
                    publication_revision,
                )
                .unwrap(),
            );
        }
        ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(revision_seed, revision_seed + 1)),
            evidence.revision(),
            evidence.source_revision(),
            evidence.record_contract(),
            contract,
            prepared,
            depth,
            Vec::new(),
            junctions,
            pairs,
            filters,
            Vec::new(),
            ledger,
            ReservationKind::Normal,
            revision_seed + 10,
        )
        .unwrap()
    }

    fn cross_tile_root(
        evidence: &Arc<EvidenceRoot>,
        tile_count: u64,
        revision_seed: u64,
        ledger: &AllocationLedger,
    ) -> Arc<ExactSummaryRoot> {
        cross_tile_root_reusing(evidence, tile_count, revision_seed, None, ledger)
    }

    fn extend_cross_tile_root(
        evidence: &Arc<EvidenceRoot>,
        partial: &Arc<ExactSummaryRoot>,
        ledger: &AllocationLedger,
    ) -> Arc<ExactSummaryRoot> {
        cross_tile_root_reusing(evidence, 3, 92, Some(partial), ledger)
    }

    fn cross_tile_atlas(
        evidence: &Arc<EvidenceRoot>,
        ledger: &AllocationLedger,
    ) -> Arc<AlignmentRowAtlas> {
        let layout = LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        };
        AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(97, 98)),
            evidence,
            cross_tile_interval(3),
            layout,
            ledger,
            ReservationKind::Normal,
            97,
        )
        .unwrap()
    }

    fn cross_tile_snapshot(
        evidence: &Arc<EvidenceRoot>,
        summaries: &Arc<ExactSummaryRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        prepared_interval: GenomicInterval,
        ledger: &AllocationLedger,
    ) -> Arc<AlignmentSceneSnapshot> {
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let candidate = store
            .prepare(
                evidence,
                summaries,
                atlas,
                presentation,
                prepared_interval,
                0..u32::try_from(atlas.row_count()).unwrap(),
                CROSS_TILE_CONTIG_LENGTH,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        Arc::clone(candidate.snapshot())
    }

    #[test]
    fn exact_pileup_site_keeps_depth_when_allele_detail_is_not_requested() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        let summaries = cross_tile_root(&evidence, 3, 111, &ledger);
        let site = summaries.pileup_site_evidence_at(82).unwrap();
        assert_eq!(site.bin.depth, 1);
        assert_eq!(site.bin.alleles.base_observations(), 0);
        assert!(!site.allele_observations_available);
        assert_eq!(site.eligible_records, 1);
    }

    fn cross_tile_junction_frame(
        snapshot: &Arc<AlignmentSceneSnapshot>,
        interval: GenomicInterval,
        count_mode: rgv_core::SashimiCountMode,
        show_labels: bool,
    ) -> crate::CpuSummaryFrame {
        let camera = crate::CpuSummaryCamera::new(
            interval,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 600.0,
                height: 80.0,
            },
        )
        .unwrap();
        crate::CpuSummaryProjector::project(
            snapshot,
            7,
            camera,
            &crate::CpuSummaryProjectionOptions::Junctions(crate::CpuJunctionProjectionOptions {
                count_mode,
                show_labels,
                ..Default::default()
            }),
        )
        .unwrap()
    }

    fn cross_tile_junction_arcs(
        frame: &crate::CpuSummaryFrame,
    ) -> Vec<&crate::CpuSummaryProjectedPrimitive> {
        frame
            .primitives
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    crate::CpuSummaryProjectedComponent::Junction { evidence, .. }
                        if (evidence.start, evidence.end) == (90, 210)
                )
            })
            .collect()
    }

    #[test]
    fn spanning_junction_has_one_semantic_owner_and_one_whole_interval_arc() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        let summaries = cross_tile_root(&evidence, 3, 100, &ledger);
        let atlas = cross_tile_atlas(&evidence, &ledger);
        let snapshot = cross_tile_snapshot(
            &evidence,
            &summaries,
            &atlas,
            cross_tile_interval(3),
            &ledger,
        );
        let junction_pages = snapshot
            .summary_pages()
            .iter()
            .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
            .collect::<Vec<_>>();
        assert_eq!(junction_pages.len(), 3);
        assert_eq!(
            junction_pages
                .iter()
                .map(|page| page.primitives().len())
                .sum::<usize>(),
            1
        );
        let owner = junction_pages
            .iter()
            .find(|page| !page.primitives().is_empty())
            .unwrap();
        assert_eq!(owner.key().tile(), cross_tile_tile(0));
        assert_eq!(owner.interval(), cross_tile_interval(1));
        assert_eq!(
            owner.indexed_interval(),
            GenomicInterval::new(ContigId::new(0), 0, 210).unwrap()
        );
        assert_eq!(
            (
                owner.primitives()[0].start_offset,
                owner.primitives()[0].end_offset
            ),
            (90, 210)
        );

        let frame = cross_tile_junction_frame(
            &snapshot,
            cross_tile_interval(3),
            rgv_core::SashimiCountMode::Raw,
            false,
        );
        assert_eq!(cross_tile_junction_arcs(&frame).len(), 1);
    }

    #[test]
    fn junction_starting_left_of_the_prepared_interval_still_produces_its_arc() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        // Prepared from tile 1: the tile owning the junction's start (90) is not in this root,
        // but the read supporting it is resident in tile 1.
        let summaries = cross_tile_root_over(&evidence, 1..3, 160, None, &ledger);
        let prepared = GenomicInterval::new(ContigId::new(0), 100, 300).unwrap();
        assert_eq!(summaries.prepared_interval(), prepared);
        assert_eq!(
            summaries
                .junctions(prepared)
                .unwrap()
                .iter()
                .map(|junction| (junction.start, junction.end, junction.depth))
                .collect::<Vec<_>>(),
            vec![(90, 210, 1)]
        );
        assert_eq!(summaries.junction_count(prepared).unwrap(), 1);
        assert_eq!(summaries.expanded_junction_row(90, 210), Some(0));

        let atlas = cross_tile_atlas(&evidence, &ledger);
        let snapshot = cross_tile_snapshot(&evidence, &summaries, &atlas, prepared, &ledger);
        let junction_pages = snapshot
            .summary_pages()
            .iter()
            .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
            .collect::<Vec<_>>();
        assert_eq!(junction_pages.len(), 2);
        let carrier = junction_pages
            .iter()
            .find(|page| !page.primitives().is_empty())
            .unwrap();
        assert_eq!(carrier.key().tile(), cross_tile_tile(1));
        assert_eq!(
            junction_pages
                .iter()
                .map(|page| page.primitives().len())
                .sum::<usize>(),
            1
        );
        // The page span is clipped to its tile; the evidence keeps the biological extent.
        let primitive = carrier.primitives()[0];
        assert_eq!((primitive.start_offset, primitive.end_offset), (0, 110));
        let SummarySemanticComponent::Junction(junction) = primitive.component else {
            panic!("junction page holds a junction");
        };
        assert_eq!((junction.evidence.start, junction.evidence.end), (90, 210));
        let frame =
            cross_tile_junction_frame(&snapshot, prepared, rgv_core::SashimiCountMode::Raw, false);
        assert_eq!(cross_tile_junction_arcs(&frame).len(), 1);

        // A root which also prepared the owner tile draws the junction from there only, and the
        // two versions of tile 1's page never share a cache identity.
        let full = cross_tile_root(&evidence, 3, 180, &ledger);
        assert!(full.entering_junctions().is_empty());
        assert_eq!(full.junction_count(cross_tile_interval(3)).unwrap(), 1);
        let family = SummaryPageFamily::Junctions;
        assert_ne!(
            SummaryPageKey::new(&summaries, cross_tile_tile(1), family),
            SummaryPageKey::new(&full, cross_tile_tile(1), family)
        );
        assert_eq!(
            SummaryPageKey::new(&summaries, cross_tile_tile(2), family),
            SummaryPageKey::new(&full, cross_tile_tile(2), family)
        );
    }

    #[test]
    fn partial_camera_discovers_spanning_junction_through_owner_page_index() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        let summaries = cross_tile_root(&evidence, 3, 120, &ledger);
        let atlas = cross_tile_atlas(&evidence, &ledger);
        let partial = GenomicInterval::new(ContigId::new(0), 150, 160).unwrap();
        let snapshot = cross_tile_snapshot(&evidence, &summaries, &atlas, partial, &ledger);
        let owner = snapshot
            .summary_pages()
            .iter()
            .find(|page| {
                page.key().family() == SummaryPageFamily::Junctions
                    && page.key().tile() == cross_tile_tile(0)
            })
            .expect("the page table retains a non-local junction owner");
        assert!(!owner.interval().overlaps(partial));
        assert!(owner.indexed_interval().overlaps(partial));
        assert!(
            snapshot
                .summary_pages_for(SummaryPageFamily::Junctions, partial)
                .iter()
                .any(|page| page.key().tile() == cross_tile_tile(0))
        );

        let frame =
            cross_tile_junction_frame(&snapshot, partial, rgv_core::SashimiCountMode::Raw, false);
        assert_eq!(frame.covered_interval, Some(partial));
        assert_eq!(cross_tile_junction_arcs(&frame).len(), 1);
    }

    #[test]
    fn incrementally_extended_and_clean_roots_have_identical_junction_pages_and_projection() {
        let ledger = AllocationLedger::new(128 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        let partial = cross_tile_root(&evidence, 2, 140, &ledger);
        let incremental = extend_cross_tile_root(&evidence, &partial, &ledger);
        let clean = cross_tile_root(&evidence, 3, 160, &ledger);
        for index in 0..2 {
            assert!(Arc::ptr_eq(
                &partial.junction_tiles()[index],
                &incremental.junction_tiles()[index]
            ));
        }
        assert_eq!(
            incremental.junctions(cross_tile_interval(3)).unwrap(),
            clean.junctions(cross_tile_interval(3)).unwrap()
        );

        let atlas = cross_tile_atlas(&evidence, &ledger);
        let incremental_snapshot = cross_tile_snapshot(
            &evidence,
            &incremental,
            &atlas,
            cross_tile_interval(3),
            &ledger,
        );
        let clean_snapshot =
            cross_tile_snapshot(&evidence, &clean, &atlas, cross_tile_interval(3), &ledger);
        let page_signature = |snapshot: &AlignmentSceneSnapshot| {
            snapshot
                .summary_pages()
                .iter()
                .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
                .map(|page| {
                    (
                        page.key(),
                        page.interval(),
                        page.indexed_interval(),
                        page.primitives().to_vec(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            page_signature(&incremental_snapshot),
            page_signature(&clean_snapshot)
        );

        let incremental_frame = cross_tile_junction_frame(
            &incremental_snapshot,
            cross_tile_interval(3),
            rgv_core::SashimiCountMode::Raw,
            false,
        );
        let clean_frame = cross_tile_junction_frame(
            &clean_snapshot,
            cross_tile_interval(3),
            rgv_core::SashimiCountMode::Raw,
            false,
        );
        assert_eq!(incremental_frame.scale_maximum, clean_frame.scale_maximum);
        assert_eq!(
            incremental_frame.normalization_available,
            clean_frame.normalization_available
        );
        assert_eq!(incremental_frame.primitives, clean_frame.primitives);
    }

    #[test]
    fn single_owner_preserves_raw_and_cpm_values_scales_and_labels() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let evidence = cross_tile_evidence(&ledger);
        let summaries = cross_tile_root(&evidence, 3, 180, &ledger);
        let atlas = cross_tile_atlas(&evidence, &ledger);
        let snapshot = cross_tile_snapshot(
            &evidence,
            &summaries,
            &atlas,
            cross_tile_interval(3),
            &ledger,
        );
        let raw = cross_tile_junction_frame(
            &snapshot,
            cross_tile_interval(3),
            rgv_core::SashimiCountMode::Raw,
            true,
        );
        let cpm = cross_tile_junction_frame(
            &snapshot,
            cross_tile_interval(3),
            rgv_core::SashimiCountMode::CountsPerMillion,
            true,
        );
        let raw_arcs = cross_tile_junction_arcs(&raw);
        let cpm_arcs = cross_tile_junction_arcs(&cpm);
        assert_eq!((raw_arcs.len(), cpm_arcs.len()), (1, 1));
        let value = |primitive: &crate::CpuSummaryProjectedPrimitive| {
            let crate::CpuSummaryProjectedComponent::Junction { value_bits, .. } =
                primitive.component
            else {
                unreachable!()
            };
            f64::from_bits(value_bits)
        };
        assert_eq!(value(raw_arcs[0]), 1.0);
        assert_eq!(value(cpm_arcs[0]), 1_000.0);
        assert_eq!(raw.scale_maximum, Some(1.0));
        assert_eq!(cpm.scale_maximum, Some(1_000.0));
        assert!(raw.normalization_available);
        assert!(cpm.normalization_available);
        let label = |frame: &crate::CpuSummaryFrame| {
            frame.primitives.iter().find_map(|primitive| {
                matches!(
                    primitive.component,
                    crate::CpuSummaryProjectedComponent::JunctionLabel { .. }
                )
                .then(|| primitive.text.clone())
                .flatten()
            })
        };
        assert_eq!(label(&raw).as_deref(), Some("1"));
        assert_eq!(label(&cpm).as_deref(), Some("1000 CPM"));
    }

    fn republished_exact_root(
        root: &ExactSummaryRoot,
        revision: SummaryRevision,
        ledger: &AllocationLedger,
    ) -> Arc<ExactSummaryRoot> {
        ExactSummaryRoot::build(
            revision,
            root.evidence_revision(),
            root.source_revision(),
            root.record_contract(),
            root.contract(),
            root.prepared_interval(),
            root.depth_tiles().to_vec(),
            root.allele_tiles().to_vec(),
            root.junction_tiles().to_vec(),
            root.pair_tiles().to_vec(),
            root.filter_tiles().to_vec(),
            root.modification_tiles().to_vec(),
            ledger,
            ReservationKind::Normal,
            5,
        )
        .unwrap()
    }

    #[test]
    fn semantic_page_identity_survives_a_compatible_root_republication() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let root = exact_root(&ledger);
        let republished = republished_exact_root(
            &root,
            SummaryRevision::new(RevisionFingerprint::new(80, 81)),
            &ledger,
        );
        let tile = root.depth_tiles()[0].tile();
        for family in [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ] {
            assert_eq!(
                SummaryPageKey::new(&root, tile, family),
                SummaryPageKey::new(&republished, tile, family),
                "{family:?} was incorrectly keyed by root publication"
            );
        }

        let original_page = SummarySemanticPage::build(
            SummaryPageKey::new(&root, tile, SummaryPageFamily::Depth),
            &root,
            None,
            &ledger,
            ReservationKind::Normal,
            6,
        )
        .unwrap();
        assert!(validate_key(original_page.key(), &republished, None, None).is_ok());
    }

    #[test]
    fn unrelated_summary_settings_do_not_rekey_depth_pages() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, root) = exact_authority(&ledger);
        let tile = root.depth_tiles()[0].tile();
        let contract = SummaryContract {
            minimum_junction_flank: 2,
            ..root.contract()
        };
        let junction = ExactJunctionSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.junctions(),
            Some(1_000),
            &ledger,
            ReservationKind::Normal,
            8,
        )
        .unwrap();
        let changed = ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(82, 83)),
            root.evidence_revision(),
            root.source_revision(),
            root.record_contract(),
            contract,
            root.prepared_interval(),
            root.depth_tiles().to_vec(),
            root.allele_tiles().to_vec(),
            vec![junction],
            root.pair_tiles().to_vec(),
            root.filter_tiles().to_vec(),
            root.modification_tiles().to_vec(),
            &ledger,
            ReservationKind::Normal,
            9,
        )
        .unwrap();

        assert_eq!(
            SummaryPageKey::new(&root, tile, SummaryPageFamily::Depth),
            SummaryPageKey::new(&changed, tile, SummaryPageFamily::Depth)
        );
        assert_ne!(
            SummaryPageKey::new(&root, tile, SummaryPageFamily::Junctions),
            SummaryPageKey::new(&changed, tile, SummaryPageFamily::Junctions)
        );
    }

    #[test]
    fn reference_revision_is_an_allele_only_page_dependency() {
        let ledger = AllocationLedger::new(32 << 20, 0);
        let root = exact_root(&ledger);
        let tile = root.depth_tiles()[0].tile();
        let first = ReferenceRevision::new(RevisionFingerprint::new(88, 89));
        let second = ReferenceRevision::new(RevisionFingerprint::new(90, 91));

        for family in [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ] {
            let first =
                SummaryPageKey::new_with_dependencies(&root, tile, family, Some(first), None);
            let second =
                SummaryPageKey::new_with_dependencies(&root, tile, family, Some(second), None);
            assert_eq!(
                first == second,
                family != SummaryPageFamily::Alleles,
                "unexpected reference dependency for {family:?}"
            );
        }
    }

    #[test]
    fn every_derived_lane_is_a_raw_camera_independent_semantic_page() {
        let ledger = AllocationLedger::new(32 << 20, 0);
        let root = exact_root(&ledger);
        let tile = root.depth_tiles()[0].tile();
        let before = ledger.snapshot().unwrap().charged;
        let families = [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ];
        let pages = families
            .into_iter()
            .enumerate()
            .map(|(index, family)| {
                SummarySemanticPage::build(
                    SummaryPageKey::new(&root, tile, family),
                    &root,
                    None,
                    &ledger,
                    ReservationKind::Normal,
                    10 + index as u64,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        assert!(pages.iter().all(|page| !page.primitives().is_empty()));
        assert!(pages[0].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Depth { depth: 1, .. }
        )));
        assert!(pages[2].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Junction(junction)
                if junction.evidence.start == 15 && junction.evidence.end == 18
        )));
        assert!(pages[3].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Modification { .. }
        )));
        assert!(pages[4].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::PairStatistics(_)
        )));
        let snapshot = ledger.snapshot().unwrap();
        assert!(snapshot.charged > before);
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::SummaryPageBuilder && usage.high_water > 0
        }));
    }

    #[test]
    fn allele_page_captures_exact_reference_authority_without_retaining_the_fasta_slice() {
        let ledger = AllocationLedger::new(32 << 20, 0);
        let root = exact_root(&ledger);
        let tile = root.depth_tiles()[0].tile();
        let interval = root.depth_tiles()[0].depth().interval();
        let contig_length = interval.end().saturating_add(2);
        let revision = ReferenceRevision::new(RevisionFingerprint::new(90, 91));
        let reference_interval = GenomicInterval::new(interval.contig(), 0, contig_length).unwrap();
        let mut bases = vec![b'A'; reference_interval.len() as usize];
        bases[10] = b'c';
        let reference = ReferenceSlice {
            interval: reference_interval,
            bases,
        };
        let key = SummaryPageKey::new_with_dependencies(
            &root,
            tile,
            SummaryPageFamily::Alleles,
            Some(revision),
            None,
        );
        let page = SummarySemanticPage::build_with_dependencies(
            key,
            &root,
            Some((revision, &reference, contig_length)),
            None,
            &ledger,
            ReservationKind::Normal,
            19,
        )
        .unwrap();

        assert_eq!(page.key().reference_revision(), Some(revision));
        assert!(page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Alleles {
                reference_base: Some(b'C'),
                ..
            }
        )));

        let mismatched = SummarySemanticPage::build_with_dependencies(
            key,
            &root,
            Some((
                ReferenceRevision::new(RevisionFingerprint::new(92, 93)),
                &reference,
                contig_length,
            )),
            None,
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap_err();
        assert_eq!(mismatched, SummarySemanticPageError::AuthorityMismatch);

        let short_interval = GenomicInterval::new(interval.contig(), 0, interval.end()).unwrap();
        let short = ReferenceSlice {
            interval: short_interval,
            bases: vec![b'A'; short_interval.len() as usize],
        };
        assert!(matches!(
            SummarySemanticPage::build_with_dependencies(
                key,
                &root,
                Some((revision, &short, contig_length)),
                None,
                &ledger,
                ReservationKind::Normal,
                21,
            )
            .unwrap_err(),
            SummarySemanticPageError::ReferenceCoverageMismatch { .. }
        ));

        let malformed = ReferenceSlice {
            interval: reference_interval,
            bases: vec![b'A'; interval.len() as usize],
        };
        assert!(matches!(
            SummarySemanticPage::build_with_dependencies(
                key,
                &root,
                Some((revision, &malformed, contig_length)),
                None,
                &ledger,
                ReservationKind::Normal,
                22,
            )
            .unwrap_err(),
            SummarySemanticPageError::ReferenceBaseCoverageMismatch { .. }
        ));
    }

    #[test]
    fn allele_reference_context_requires_two_exact_neighbors_and_matches_legacy_contexts() {
        let contig = ContigId::new(0);
        let page = GenomicInterval::new(contig, 100, 120).unwrap();
        assert_eq!(
            allele_reference_context_interval(page, 1_000).unwrap(),
            GenomicInterval::new(contig, 98, 122).unwrap()
        );
        assert_eq!(
            allele_reference_context_interval(GenomicInterval::new(contig, 0, 20).unwrap(), 20)
                .unwrap(),
            GenomicInterval::new(contig, 0, 20).unwrap()
        );

        let interval = GenomicInterval::new(contig, 98, 122).unwrap();
        let reference = ReferenceSlice {
            interval,
            bases: b"AAACGACAGACATAGCATAAAAAA".to_vec(),
        };
        for position in 100..120 {
            let captured = SummaryAlleleReferenceContext::capture(&reference, position);
            for context in [
                BisulfiteContext::Cg,
                BisulfiteContext::Chg,
                BisulfiteContext::Chh,
                BisulfiteContext::Hcg,
                BisulfiteContext::Gch,
                BisulfiteContext::Wcg,
            ] {
                for reverse in [false, true] {
                    assert_eq!(
                        captured.matches(reverse, context),
                        bisulfite_context_matches(&reference, position, reverse, context),
                        "position={position}, reverse={reverse}, context={context:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn stale_summary_key_cannot_publish_against_another_root() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let first = exact_root(&ledger);
        let second = exact_root(&ledger);
        let key = SummaryPageKey::new(
            &first,
            first.depth_tiles()[0].tile(),
            SummaryPageFamily::Depth,
        );
        // Both helpers intentionally describe compatible content, so they share a key. A family
        // contract mismatch remains stale even though publication revisions are not content.
        let stale = SummaryPageKey {
            family_contract: ContractFingerprint::new(99),
            ..key
        };
        assert_eq!(
            SummarySemanticPage::build(stale, &second, None, &ledger, ReservationKind::Normal, 20,)
                .unwrap_err(),
            SummarySemanticPageError::AuthorityMismatch
        );
    }

    #[test]
    fn read_and_derived_lanes_publish_as_one_pinned_scene_snapshot() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(20, 21)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let tile = summaries.depth_tiles()[0].tile();
        let row_block = CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap());
        let read_page = AlignmentSemanticPage::build(
            AlignmentPageKey::new(
                evidence.source_revision(),
                evidence.revision(),
                atlas.revision(),
                layout.fingerprint(),
                presentation.page_fingerprint(crate::AlignmentPageFamily::ReadGeometry),
                tile,
                row_block,
                crate::AlignmentPageFamily::ReadGeometry,
                GeometryLod::Span,
            ),
            &evidence,
            &atlas,
            presentation,
            100,
            None,
            &ledger,
            ReservationKind::Normal,
            21,
        )
        .unwrap();
        let revision = SceneRevision::new(RevisionFingerprint::new(22, 23));
        let reads = AlignmentPresentationScene::new(
            revision,
            Arc::clone(&evidence),
            Arc::clone(&atlas),
            presentation,
            interval,
            0..1,
            vec![read_page],
            &ledger,
            ReservationKind::Normal,
            22,
        )
        .unwrap();
        let families = [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ];
        let pages = families
            .into_iter()
            .enumerate()
            .map(|(index, family)| {
                SummarySemanticPage::build(
                    SummaryPageKey::new(&summaries, tile, family),
                    &summaries,
                    None,
                    &ledger,
                    ReservationKind::Normal,
                    30 + index as u64,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let missing = AlignmentSceneSnapshot::new(
            revision,
            Arc::clone(&reads),
            Arc::clone(&summaries),
            pages[..pages.len() - 1].to_vec(),
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            AlignmentSceneSnapshotError::MissingMandatorySummaryPage(_)
        ));

        let snapshot = AlignmentSceneSnapshot::new(
            revision,
            reads,
            Arc::clone(&summaries),
            pages,
            &ledger,
            ReservationKind::Normal,
            41,
        )
        .unwrap();
        assert!(snapshot.covers(interval, 0..1));
        assert_eq!(snapshot.summary_pages().len(), 6);
        assert_eq!(snapshot.reads().revision(), snapshot.revision());
        assert_eq!(
            snapshot.reads().evidence().revision(),
            snapshot.summaries().evidence_revision()
        );
        let (semantic_retirement, semantic_stats) =
            crate::build_alignment_semantic_retirement_claim_set(
                &ledger,
                std::iter::once(&snapshot),
            )
            .unwrap();
        // Root variables remain externally owned in this test, while the moved read/summary
        // semantic pages are exclusive to the snapshot. The maximal exact proof credits the two
        // scene metadata allocations plus all seven page allocations, but not shared roots.
        assert_eq!(semantic_retirement.len(), 9);
        assert_eq!(semantic_stats.exclusively_owned_snapshots, 1);
        assert_eq!(semantic_stats.exclusively_owned_read_scenes, 1);
        assert_eq!(semantic_stats.exclusively_owned_evidence_roots, 0);
        assert_eq!(semantic_stats.exclusively_owned_row_atlases, 0);
        assert_eq!(semantic_stats.exclusively_owned_summary_roots, 0);
        assert_eq!(semantic_stats.exclusively_owned_read_pages, 1);
        assert_eq!(semantic_stats.exclusively_owned_summary_pages, 6);
        assert_eq!(semantic_stats.claimed_allocations, 9);

        let external_snapshot_owner = Arc::clone(&snapshot);
        let (shared_retirement, shared_stats) =
            crate::build_alignment_semantic_retirement_claim_set(
                &ledger,
                std::iter::once(&snapshot),
            )
            .unwrap();
        assert!(shared_retirement.is_empty());
        assert_eq!(shared_stats.exclusively_owned_snapshots, 0);
        drop(external_snapshot_owner);
        assert!(snapshot.metadata_allocation().snapshot().unwrap().pins > 0);
        let allele_site = snapshot.allele_site_at(10).unwrap();
        assert_eq!(allele_site.evidence.owner_interval, interval);
        assert_eq!(allele_site.evidence.eligible_records, 1);
        assert_eq!(allele_site.evidence.bin.alleles.a, 1);
        assert_eq!(allele_site.reference_base, None);
        let pileup_site = snapshot.pileup_site_at(10).unwrap();
        assert_eq!(pileup_site.evidence.bin.depth, 1);
        assert!(pileup_site.evidence.allele_observations_available);
        assert!(snapshot.allele_site_at(interval.end()).is_none());
        assert!(snapshot.pileup_site_at(interval.end()).is_none());

        let summary_camera = crate::CpuSummaryCamera::new(
            interval,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 80.0,
            },
        )
        .unwrap();
        ledger.set_transient_budget(0).unwrap();
        assert!(matches!(
            crate::CpuSummaryProjector::project(
                &snapshot,
                0,
                summary_camera,
                &crate::CpuSummaryProjectionOptions::Depth(Default::default()),
            )
            .unwrap_err(),
            crate::CpuSummaryProjectionError::Ledger(LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::CpuSummaryProjection,
                ..
            })
        ));
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
        ledger.set_transient_budget(64 << 20).unwrap();
        let accounted_frame = crate::CpuSummaryProjector::project(
            &snapshot,
            0,
            summary_camera,
            &crate::CpuSummaryProjectionOptions::Depth(Default::default()),
        )
        .unwrap();
        assert!(
            ledger
                .snapshot()
                .unwrap()
                .transient_resources
                .iter()
                .any(|usage| {
                    usage.class == TransientResourceClass::CpuSummaryProjection && usage.current > 0
                })
        );
        let accounted_clone = accounted_frame.clone();
        assert!(Arc::ptr_eq(
            &accounted_frame.primitives,
            &accounted_clone.primitives
        ));
        drop(accounted_frame);
        assert!(ledger.snapshot().unwrap().transient > 0);
        drop(accounted_clone);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);

        let projection_options = [
            crate::CpuSummaryProjectionOptions::Depth(Default::default()),
            crate::CpuSummaryProjectionOptions::Alleles(crate::CpuAlleleProjectionOptions {
                minimum_fraction: 0.0,
                quality_weighted: true,
                ..Default::default()
            }),
            crate::CpuSummaryProjectionOptions::Junctions(crate::CpuJunctionProjectionOptions {
                display_mode: rgv_core::JunctionDisplayMode::Expanded,
                count_mode: rgv_core::SashimiCountMode::CountsPerMillion,
                ..Default::default()
            }),
            crate::CpuSummaryProjectionOptions::Modifications(Default::default()),
            crate::CpuSummaryProjectionOptions::Pairs,
            crate::CpuSummaryProjectionOptions::FilterProvenance,
        ];
        let charged_before_projection = ledger.snapshot().unwrap().charged;
        for (sequence, options) in projection_options.iter().enumerate() {
            let frame = crate::CpuSummaryProjector::project(
                &snapshot,
                sequence as u64,
                summary_camera,
                options,
            )
            .unwrap();
            assert_eq!(frame.family, options.family());
            assert_eq!(frame.covered_interval, Some(interval));
            if options.family() == SummaryPageFamily::Alleles {
                assert!(
                    frame.primitives.is_empty(),
                    "reference-independent allele facts must not be labelled substitutions"
                );
            } else {
                assert!(
                    !frame.primitives.is_empty(),
                    "{:?} projection unexpectedly emitted no primitives",
                    options.family()
                );
            }
        }
        assert_eq!(
            ledger.snapshot().unwrap().charged,
            charged_before_projection
        );

        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let candidate = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(store.prepared().is_none());
        assert_eq!(store.read_page_count(), 1);
        assert_eq!(store.summary_page_count(), 6);
        let current = store.stage(candidate);
        assert!(store.prepared_covers(interval, 0..1));

        let resident = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert_eq!(store.read_page_count(), 1);
        assert_eq!(store.summary_page_count(), 6);
        assert!(Arc::ptr_eq(store.prepared().unwrap(), &current));
        let replacement = store.stage(resident);
        assert!(!Arc::ptr_eq(&replacement, &current));
        assert_eq!(
            replacement.reads().pages()[0].key(),
            current.reads().pages()[0].key()
        );

        let republished_summaries = republished_exact_root(
            &summaries,
            SummaryRevision::new(RevisionFingerprint::new(84, 85)),
            &ledger,
        );
        let republished = store
            .prepare(
                &evidence,
                &republished_summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert_eq!(store.summary_page_count(), 6);
        for original in current.summary_pages() {
            let reused = republished
                .snapshot()
                .summary_pages()
                .iter()
                .find(|candidate| candidate.key() == original.key())
                .expect("republished scene contains every compatible summary family");
            assert!(
                Arc::ptr_eq(original, reused),
                "{:?} page was rebuilt solely because its root was republished",
                original.key().family()
            );
        }

        let reference_revision = ReferenceRevision::new(RevisionFingerprint::new(86, 87));
        let reference_presentation = PresentationContract {
            reference: Some(reference_revision),
            ..presentation
        };
        let minimum_reference_interval = GenomicInterval::new(interval.contig(), 0, 102).unwrap();
        let minimum_reference = ReferenceSlice {
            interval: minimum_reference_interval,
            bases: vec![b'A'; minimum_reference_interval.len() as usize],
        };
        let wider_reference_interval = GenomicInterval::new(interval.contig(), 0, 150).unwrap();
        let wider_reference = ReferenceSlice {
            interval: wider_reference_interval,
            bases: vec![b'A'; wider_reference_interval.len() as usize],
        };
        let mut reference_store = AlignmentPresentationStore::new(ledger.clone());
        let minimum = reference_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                reference_presentation,
                interval,
                0..1,
                200,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                Some(&minimum_reference),
                ReservationKind::Normal,
            )
            .unwrap();
        let minimum_alleles = minimum
            .snapshot()
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Alleles)
            .cloned()
            .unwrap();
        let wider = reference_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                reference_presentation,
                interval,
                0..1,
                200,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                Some(&wider_reference),
                ReservationKind::Normal,
            )
            .unwrap();
        let wider_alleles = wider
            .snapshot()
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Alleles)
            .unwrap();
        assert!(
            Arc::ptr_eq(&minimum_alleles, wider_alleles),
            "a wider slice of one immutable reference revision must reuse the allele page"
        );
        assert_eq!(reference_store.summary_page_count(), 6);

        let mut family_store = AlignmentPresentationStore::new(ledger.clone());
        let read_presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let first = family_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                read_presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let geometry = first
            .snapshot()
            .reads()
            .pages()
            .iter()
            .find(|page| page.key().family() == crate::AlignmentPageFamily::ReadGeometry)
            .cloned()
            .unwrap();
        assert_eq!(family_store.read_page_count(), 3);
        let labelled = PresentationContract {
            style: crate::AlignmentStyleContract {
                read_labels: crate::AlignmentReadLabelMode::ReadName,
                ..read_presentation.style
            },
            ..read_presentation
        };
        let second = family_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                labelled,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let reused_geometry = second
            .snapshot()
            .reads()
            .pages()
            .iter()
            .find(|page| page.key().family() == crate::AlignmentPageFamily::ReadGeometry)
            .unwrap();
        assert!(Arc::ptr_eq(&geometry, reused_geometry));
        assert_eq!(family_store.read_page_count(), 4);
    }

    #[test]
    fn presentation_pressure_evicts_inactive_pages_without_mutating_current() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(30, 31)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            30,
        )
        .unwrap();
        let span = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let candidate = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                span,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.stage(candidate);
        let current_pointer = Arc::as_ptr(&current);
        let baseline_read_pages = store.read_page_count();

        let detailed = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            ..span
        };
        let inactive = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                detailed,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(store.read_page_count() > baseline_read_pages);
        let ownership_pressure = store.evict_inactive_until(0).unwrap();
        assert!(ownership_pressure.evicted_read_pages > 0);
        assert!(ownership_pressure.remaining_overage > 0);
        assert_eq!(store.read_page_count(), baseline_read_pages);
        assert!(inactive.snapshot().covers(interval, 0..1));
        drop(inactive);

        let pinned = ledger.snapshot().unwrap().pinned;
        let pressure = store.reduce_budget(pinned).unwrap();
        assert_eq!(pressure.evicted_read_pages, 0);
        assert_eq!(pressure.remaining_overage, 0);
        assert_eq!(store.read_page_count(), baseline_read_pages);
        assert_eq!(Arc::as_ptr(store.prepared().unwrap()), current_pointer);
        assert!(store.prepared_covers(interval, 0..1));

        let pinned_pressure = store.reduce_budget(0).unwrap();
        assert_eq!(pinned_pressure.charged, pinned_pressure.pinned);
        assert_eq!(pinned_pressure.pinned_overage, pinned_pressure.pinned);
        assert_eq!(pinned_pressure.remaining_overage, pinned_pressure.charged);

        let distant = GenomicInterval::new(ContigId::new(0), 200, 300).unwrap();
        let discarded = store.discard_disjoint_cache(distant, 0..1);
        assert_eq!(discarded.read_pages, baseline_read_pages);
        assert_eq!(discarded.summary_pages, 6);
        assert_eq!(store.read_page_count(), 0);
        assert_eq!(store.summary_page_count(), 0);
        assert!(current.covers(interval, 0..1));
        assert_eq!(ledger.snapshot().unwrap().charged, pinned_pressure.charged);
    }

    #[test]
    fn junction_annotation_status_and_labels_publish_in_the_same_scene_epoch() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(40, 41)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap();
        let annotation_revision = crate::AnnotationRevision::new(RevisionFingerprint::new(50, 51));
        let annotation = crate::JunctionAnnotationIndex::build(
            annotation_revision,
            1,
            vec![interval],
            vec![crate::JunctionAnnotationInput {
                contig: interval.contig(),
                start: 15,
                end: 18,
                labels: vec!["GENE1 / TX1".to_owned()],
            }],
            &ledger,
            ReservationKind::Normal,
            50,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: Some(annotation_revision),
        };
        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let missing = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap_err();
        assert!(matches!(
            missing,
            crate::AlignmentPresentationStoreError::AnnotationAuthorityMismatch
        ));

        let candidate = store
            .prepare_with_annotation(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                Some(&annotation),
                ReservationKind::Normal,
            )
            .unwrap();
        let snapshot = store.stage(candidate);
        assert!(Arc::ptr_eq(snapshot.annotation().unwrap(), &annotation));
        assert_eq!(
            snapshot
                .annotation()
                .unwrap()
                .labels(interval.contig(), 15, 18)[0]
                .as_ref(),
            "GENE1 / TX1"
        );
        let junction_page = snapshot
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Junctions)
            .unwrap();
        assert_eq!(
            junction_page.key().annotation_revision(),
            Some(annotation_revision)
        );
        assert!(junction_page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Junction(AnnotatedJunctionSemantic {
                annotation: Some(crate::JunctionAnnotationStatus::Annotated),
                ..
            })
        )));

        let camera = crate::CpuSummaryCamera::new(
            interval,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 80.0,
            },
        )
        .unwrap();
        let frame = crate::CpuSummaryProjector::project(
            &snapshot,
            1,
            camera,
            &crate::CpuSummaryProjectionOptions::Junctions(Default::default()),
        )
        .unwrap();
        assert!(frame.primitives.iter().any(|primitive| matches!(
            primitive.component,
            crate::CpuSummaryProjectedComponent::JunctionAnnotation {
                status: crate::JunctionAnnotationStatus::Annotated,
                ..
            }
        )));
    }
}
