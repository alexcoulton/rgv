use std::{mem::size_of, sync::Arc};

use rgv_core::{ContigId, GenomicInterval};
use thiserror::Error;

use crate::{
    AllocationClass, AllocationLease, AllocationLedger, AnnotationRevision, LedgerError,
    PinnedAllocationLease, ReservationKind, TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum JunctionAnnotationStatus {
    Annotated,
    Novel,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JunctionAnnotationInput {
    pub contig: ContigId,
    pub start: u64,
    pub end: u64,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JunctionAnnotationEvent {
    contig: ContigId,
    start: u64,
    end: u64,
    labels: Arc<[Arc<str>]>,
}

impl JunctionAnnotationEvent {
    #[must_use]
    pub const fn contig(&self) -> ContigId {
        self.contig
    }

    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }

    #[must_use]
    pub fn labels(&self) -> &[Arc<str>] {
        &self.labels
    }
}

/// Immutable exon-junction authority for one annotation revision. Absence from a fully covered
/// interval means Novel; absence outside certified coverage remains Unknown.
#[derive(Debug)]
pub struct JunctionAnnotationIndex {
    revision: AnnotationRevision,
    source_tracks: usize,
    complete_coverage: Arc<[GenomicInterval]>,
    events: Arc<[JunctionAnnotationEvent]>,
    allocation: AllocationLease,
}

impl JunctionAnnotationIndex {
    pub fn build(
        revision: AnnotationRevision,
        source_tracks: usize,
        mut complete_coverage: Vec<GenomicInterval>,
        mut inputs: Vec<JunctionAnnotationInput>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, JunctionAnnotationIndexError> {
        if source_tracks == 0 && !inputs.is_empty() {
            return Err(JunctionAnnotationIndexError::EventsWithoutSourceTracks);
        }
        if let Some(input) = inputs.iter().find(|input| input.start >= input.end) {
            return Err(JunctionAnnotationIndexError::InvalidEvent {
                contig: input.contig,
                start: input.start,
                end: input.end,
            });
        }
        let input_label_bytes = inputs
            .iter()
            .flat_map(|input| input.labels.iter())
            .map(String::len)
            .sum::<usize>();
        let maximum_bytes = annotation_bytes(
            inputs.len(),
            complete_coverage.len(),
            inputs.iter().map(|input| input.labels.len()).sum(),
            input_label_bytes,
        );
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::AnnotationBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::AnnotationIndex,
            maximum_bytes,
            reservation_kind,
        )?;

        complete_coverage
            .sort_unstable_by_key(|interval| (interval.contig(), interval.start(), interval.end()));
        let mut canonical_coverage = Vec::<GenomicInterval>::with_capacity(complete_coverage.len());
        for interval in complete_coverage {
            if source_tracks == 0 {
                break;
            }
            if let Some(previous) = canonical_coverage.last_mut()
                && previous.contig() == interval.contig()
                && previous.end() >= interval.start()
            {
                *previous = GenomicInterval::new(
                    previous.contig(),
                    previous.start(),
                    previous.end().max(interval.end()),
                )?;
            } else {
                canonical_coverage.push(interval);
            }
        }

        inputs.sort_unstable_by_key(|input| (input.contig, input.start, input.end));
        let mut merged = Vec::<JunctionAnnotationInput>::with_capacity(inputs.len());
        for mut input in inputs {
            input.labels.sort_unstable();
            input.labels.dedup();
            if let Some(previous) = merged.last_mut()
                && (previous.contig, previous.start, previous.end)
                    == (input.contig, input.start, input.end)
            {
                previous.labels.append(&mut input.labels);
                previous.labels.sort_unstable();
                previous.labels.dedup();
            } else {
                merged.push(input);
            }
        }
        let mut events = Vec::with_capacity(merged.len());
        for input in merged {
            events.push(JunctionAnnotationEvent {
                contig: input.contig,
                start: input.start,
                end: input.end,
                labels: input
                    .labels
                    .into_iter()
                    .map(Arc::<str>::from)
                    .collect::<Vec<_>>()
                    .into(),
            });
        }
        events.shrink_to_fit();
        canonical_coverage.shrink_to_fit();
        let retained_label_count = events.iter().map(|event| event.labels.len()).sum();
        let retained_label_bytes = events
            .iter()
            .flat_map(|event| event.labels.iter())
            .map(|label| label.len())
            .sum();
        let retained_bytes = annotation_bytes(
            events.len(),
            canonical_coverage.len(),
            retained_label_count,
            retained_label_bytes,
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            revision,
            source_tracks,
            complete_coverage: canonical_coverage.into(),
            events: events.into(),
            allocation,
        }))
    }

    #[must_use]
    pub const fn revision(&self) -> AnnotationRevision {
        self.revision
    }

    #[must_use]
    pub const fn source_tracks(&self) -> usize {
        self.source_tracks
    }

    #[must_use]
    pub fn complete_coverage(&self) -> &[GenomicInterval] {
        &self.complete_coverage
    }

    #[must_use]
    pub fn events(&self) -> &[JunctionAnnotationEvent] {
        &self.events
    }

    #[must_use]
    pub fn event(
        &self,
        contig: ContigId,
        start: u64,
        end: u64,
    ) -> Option<&JunctionAnnotationEvent> {
        self.events
            .binary_search_by_key(&(contig, start, end), |event| {
                (event.contig, event.start, event.end)
            })
            .ok()
            .and_then(|index| self.events.get(index))
    }

    #[must_use]
    pub fn status(&self, contig: ContigId, start: u64, end: u64) -> JunctionAnnotationStatus {
        if self.event(contig, start, end).is_some() {
            return JunctionAnnotationStatus::Annotated;
        }
        let complete = GenomicInterval::new(contig, start, end)
            .ok()
            .is_some_and(|target| {
                self.source_tracks > 0
                    && self
                        .complete_coverage
                        .iter()
                        .any(|coverage| coverage.contains_interval(target))
            });
        if complete {
            JunctionAnnotationStatus::Novel
        } else {
            JunctionAnnotationStatus::Unknown
        }
    }

    #[must_use]
    pub fn labels(&self, contig: ContigId, start: u64, end: u64) -> &[Arc<str>] {
        self.event(contig, start, end)
            .map_or(&[], JunctionAnnotationEvent::labels)
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    pub fn pin(&self) -> Result<JunctionAnnotationPins, LedgerError> {
        Ok(JunctionAnnotationPins {
            _allocation: self.allocation.pin()?,
        })
    }

    pub(crate) fn touch(
        &self,
        ledger: &AllocationLedger,
        revision: u64,
    ) -> Result<(), LedgerError> {
        ledger.touch(self.allocation.id(), revision)
    }
}

#[derive(Debug)]
pub struct JunctionAnnotationPins {
    _allocation: PinnedAllocationLease,
}

impl JunctionAnnotationPins {
    pub(crate) const fn allocation(&self) -> &PinnedAllocationLease {
        &self._allocation
    }
}

const fn annotation_bytes(
    events: usize,
    coverage: usize,
    labels: usize,
    label_bytes: usize,
) -> usize {
    size_of::<JunctionAnnotationIndex>()
        .saturating_add(events.saturating_mul(size_of::<JunctionAnnotationEvent>()))
        .saturating_add(coverage.saturating_mul(size_of::<GenomicInterval>()))
        .saturating_add(labels.saturating_mul(size_of::<Arc<str>>()))
        .saturating_add(label_bytes)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JunctionAnnotationIndexError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Interval(#[from] rgv_core::IntervalError),
    #[error("annotation events require at least one source track")]
    EventsWithoutSourceTracks,
    #[error("invalid annotation junction {contig:?}:{start}-{end}")]
    InvalidEvent {
        contig: ContigId,
        start: u64,
        end: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RevisionFingerprint;

    #[test]
    fn annotation_status_distinguishes_annotated_novel_and_unknown_without_caps() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        let contig = ContigId::new(0);
        let index = JunctionAnnotationIndex::build(
            AnnotationRevision::new(RevisionFingerprint::new(1, 2)),
            2,
            vec![GenomicInterval::new(contig, 100, 1_000).unwrap()],
            vec![
                JunctionAnnotationInput {
                    contig,
                    start: 200,
                    end: 300,
                    labels: vec!["TX2".to_owned(), "TX1".to_owned()],
                },
                JunctionAnnotationInput {
                    contig,
                    start: 200,
                    end: 300,
                    labels: vec!["TX1".to_owned(), "GENE".to_owned()],
                },
            ],
            &ledger,
            ReservationKind::Normal,
            1,
        )
        .unwrap();

        assert_eq!(index.events().len(), 1);
        assert_eq!(
            index.status(contig, 200, 300),
            JunctionAnnotationStatus::Annotated
        );
        assert_eq!(
            index.status(contig, 400, 500),
            JunctionAnnotationStatus::Novel
        );
        assert_eq!(
            index.status(contig, 50, 75),
            JunctionAnnotationStatus::Unknown
        );
        assert_eq!(
            index
                .labels(contig, 200, 300)
                .iter()
                .map(AsRef::<str>::as_ref)
                .collect::<Vec<_>>(),
            vec!["GENE", "TX1", "TX2"]
        );
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert_eq!(
            snapshot.records.last().unwrap().class,
            AllocationClass::AnnotationIndex
        );
    }
}
