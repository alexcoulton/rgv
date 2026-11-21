//! Decode-time coverage: builds coverage-pyramid tiles from a region's record stream without
//! retaining any read.
//!
//! Every backend feeds the records its index returns for the requested tiles into one
//! [`CoverageStream`]. Per record it reads the flags, mapping quality (and any auxiliary field
//! the filter names), placement, mate placement, name and CIGAR; sequence, qualities and other
//! auxiliary fields are never decoded or copied. Eligibility is the shared
//! [`crate::backend_record::eligibility`] rule of the read-level paths, and depth is counted by
//! [`rgv_model::add_counted_depth_segments`], the rule of the exact depth summary.

use std::ops::Range;

use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy, ReferenceDictionary};
use rgv_model::{
    CoverageAccumulation, CoverageAccumulator, CoverageAccumulatorTelemetry, CoverageEligibility,
    CoveragePyramidError, CoveragePyramidParameters, CoverageTile, pileup::MateAlignment,
};

use crate::{
    AlignmentError, CigarOp, QueryOptions,
    backend_record::{BackendRecord, Eligibility, cigar_reference_length, eligibility},
};

/// Records between two polls of the cancellation probe.
const CANCELLATION_CHUNK: u64 = 256;

/// Tiles of one pyramid level to build from one contiguous run of a contig.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageTileRequest {
    pub parameters: CoveragePyramidParameters,
    pub contig: ContigId,
    pub level: u8,
    /// Consecutive tile indices of `level`.
    pub tiles: Range<u64>,
    pub eligibility: CoverageEligibility,
}

/// Tiles built by one request, with the provenance needed to judge them.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageTileProduction {
    pub tiles: Vec<CoverageTile>,
    /// Records the index returned.
    pub records_seen: u64,
    /// Records that passed eligibility and were counted.
    pub records_eligible: u64,
    pub collapsed_mate_bases: u64,
    /// False when the count-once window overflowed: the tiles are not the count-once depth
    /// the request asked for and must not be cached or presented as that. What they are
    /// instead is said by `mate_overlap_not_collapsed`.
    pub overlap_collapse_complete: bool,
    /// True when the tiles count every mate independently although the request asked for
    /// overlapping mates to be counted once: the count-once window overflowed (more than
    /// [`rgv_model::pileup::MAX_PENDING_PAIR_OVERLAPS`] earlier mates were waiting for theirs
    /// at once, as over an ultra-deep amplicon), so the tiles were built again without it.
    /// They are then exact for [`OverlappingMatePolicy::CountBoth`]: where two mates of a
    /// template overlap, depth is one higher than the count-once depth. Present them, and
    /// say so. Always false for a request that asked for both mates to be counted.
    pub mate_overlap_not_collapsed: bool,
    pub telemetry: CoverageAccumulatorTelemetry,
}

/// Result of a coverage request that did not fail.
#[derive(Clone, Debug, PartialEq)]
pub enum CoverageTileOutcome {
    Complete(CoverageTileProduction),
    /// The cancellation probe fired; nothing is returned.
    Cancelled,
}

impl CoverageTileRequest {
    /// The same tiles with every mate counted independently: the request that degrades this
    /// one honestly when its count-once window overflows.
    #[must_use]
    pub fn counting_mates_independently(&self) -> Self {
        let mut request = self.clone();
        request.eligibility.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
        request
    }
}

/// Runs `produce` for `request` and, when the count-once window overflowed, once more with
/// every mate counted independently, flagging the result
/// ([`CoverageTileProduction::mate_overlap_not_collapsed`]).
///
/// The first pass stays within the window's fixed bound and the second keeps no window at
/// all, so working memory does not grow with depth; the cost is a second decode of the
/// affected tiles only. A cancelled or failed second pass is a cancelled or failed request.
pub(crate) fn with_mate_overlap_fallback(
    request: &CoverageTileRequest,
    mut produce: impl FnMut(&CoverageTileRequest) -> Result<CoverageTileOutcome, AlignmentError>,
) -> Result<CoverageTileOutcome, AlignmentError> {
    let outcome = produce(request)?;
    let overflowed = matches!(
        &outcome,
        CoverageTileOutcome::Complete(production) if !production.overlap_collapse_complete
    );
    if !overflowed
        || request.eligibility.overlapping_mate_policy == OverlappingMatePolicy::CountBoth
    {
        return Ok(outcome);
    }
    Ok(match produce(&request.counting_mates_independently())? {
        CoverageTileOutcome::Complete(mut production) => {
            production.overlap_collapse_complete = false;
            production.mate_overlap_not_collapsed = true;
            CoverageTileOutcome::Complete(production)
        }
        CoverageTileOutcome::Cancelled => CoverageTileOutcome::Cancelled,
    })
}

/// The decode-time accumulator shared by every backend.
pub(crate) struct CoverageStream {
    accumulator: CoverageAccumulator,
    options: QueryOptions,
    cigar: Vec<CigarOp>,
    records_seen: u64,
    cancelled: bool,
}

impl CoverageStream {
    pub(crate) fn new(
        request: &CoverageTileRequest,
        dictionary: &ReferenceDictionary,
    ) -> Result<Self, AlignmentError> {
        let contig = dictionary
            .get(request.contig)
            .ok_or(AlignmentError::UnknownContig(request.contig.get()))?;
        let accumulator = CoverageAccumulator::new(
            request.parameters,
            request.contig,
            contig.length(),
            request.level,
            request.tiles.clone(),
            request.eligibility.overlapping_mate_policy,
        )
        .map_err(coverage_error)?;
        Ok(Self {
            accumulator,
            options: request.eligibility.to_query_options(),
            cigar: Vec::new(),
            records_seen: 0,
            cancelled: false,
        })
    }

    /// The region whose records the backend must feed, in coordinate order.
    pub(crate) fn interval(&self) -> Result<GenomicInterval, AlignmentError> {
        let (start, end) = self.accumulator.span();
        GenomicInterval::new(self.accumulator.contig(), start, end)
            .map_err(|_| AlignmentError::CoordinateOverflow)
    }

    /// Adds one record. Returns `false` once the request was cancelled.
    pub(crate) fn add<R>(
        &mut self,
        record: &R,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<bool, AlignmentError>
    where
        R: BackendRecord + ?Sized,
    {
        self.records_seen = self.records_seen.saturating_add(1);
        if self.records_seen.is_multiple_of(CANCELLATION_CHUNK) && !keep_going() {
            self.cancelled = true;
            return Ok(false);
        }
        let flags = record.flags()?;
        if eligibility(record, flags, &self.options)? != Eligibility::Eligible {
            return Ok(true);
        }
        let (Some(contig), Some(start)) = (record.contig()?, record.start()?) else {
            return Ok(true);
        };
        let mut cigar = std::mem::take(&mut self.cigar);
        let result = record.cigar_into(&mut cigar).and_then(|()| {
            self.accumulator
                .add_record(
                    MateAlignment {
                        read_name: record.read_name(),
                        contig,
                        start,
                        end: start
                            .saturating_add(cigar_reference_length(&cigar))
                            .max(start.saturating_add(1)),
                        flags,
                        mate_contig: record.mate_contig()?,
                        mate_start: record.mate_start()?,
                    },
                    &cigar,
                )
                .map_err(coverage_error)
        });
        self.cigar = cigar;
        result.map(|()| true)
    }

    pub(crate) fn finish(self) -> Result<CoverageTileOutcome, AlignmentError> {
        if self.cancelled {
            return Ok(CoverageTileOutcome::Cancelled);
        }
        let CoverageAccumulation {
            tiles,
            eligible_records,
            collapsed_mate_bases,
            overlap_collapse_complete,
            telemetry,
        } = self.accumulator.finish().map_err(coverage_error)?;
        Ok(CoverageTileOutcome::Complete(CoverageTileProduction {
            tiles,
            records_seen: self.records_seen,
            records_eligible: eligible_records,
            collapsed_mate_bases,
            overlap_collapse_complete,
            mate_overlap_not_collapsed: false,
            telemetry,
        }))
    }
}

fn coverage_error(error: CoveragePyramidError) -> AlignmentError {
    let kind = match error {
        CoveragePyramidError::RecordsOutOfOrder | CoveragePyramidError::WrongContig => {
            std::io::ErrorKind::InvalidData
        }
        _ => std::io::ErrorKind::InvalidInput,
    };
    AlignmentError::Io(std::io::Error::new(
        kind,
        format!("coverage tiles cannot be built: {error}"),
    ))
}

#[cfg(test)]
mod tests;
