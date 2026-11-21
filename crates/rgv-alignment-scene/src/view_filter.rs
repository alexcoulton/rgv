use std::sync::Arc;

use rgv_core::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentCigarEventPredicate,
    AlignmentFlagPredicate, ContigId, GenomicInterval,
};
use rgv_model::{
    AlignedBaseClassification, AlignmentFlags, AlignmentRecordView, AuxValue, CigarOpKind,
    PackedRecordError, ReferenceSlice, SequenceView, classify_aligned_base,
};
use thiserror::Error;

use crate::{
    AlignmentHandle, ContractFingerprint, EvidenceRoot, EvidenceSelectionError, ReferenceRevision,
    fingerprint::StableHasher,
};

/// A viewport-independent scope compiled against the alignment source dictionary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentFilterScope {
    Anywhere,
    Position {
        contig: ContigId,
        position: u64,
    },
    Interval {
        interval: GenomicInterval,
    },
    /// A persisted contig name which the current source cannot resolve. It is retained in the
    /// contract fingerprint and predictably matches no records rather than being rebound.
    UnresolvedContig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlignmentFilterPredicate {
    MappingQuality {
        minimum: Option<u8>,
        maximum: Option<u8>,
    },
    Flag {
        flag: AlignmentFlagPredicate,
        expected: bool,
    },
    TemplateLength {
        minimum: Option<u64>,
        maximum: Option<u64>,
    },
    MateContig {
        contig: Option<ContigId>,
    },
    Auxiliary {
        tag: [u8; 2],
        comparison: AlignmentAuxComparison,
        value: Option<AlignmentAuxFilterValue>,
    },
    CigarEvent {
        event: AlignmentCigarEventPredicate,
        scope: AlignmentFilterScope,
        minimum_length: Option<u32>,
        maximum_length: Option<u32>,
    },
    AlignedLength {
        minimum: Option<u64>,
        maximum: Option<u64>,
    },
    Substitution {
        scope: AlignmentFilterScope,
        observed_base: Option<u8>,
        minimum_count: Option<u32>,
        maximum_count: Option<u32>,
    },
}

impl AlignmentFilterPredicate {
    #[must_use]
    pub const fn requires_reference(&self) -> bool {
        matches!(self, Self::Substitution { .. })
    }
}

/// Immutable, versioned logical-AND selection contract. It contains no camera position: position
/// and interval predicates are explicitly anchored when the user creates them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentViewFilterContract {
    version: u16,
    predicates: Arc<[AlignmentFilterPredicate]>,
    fingerprint: ContractFingerprint,
}

impl Default for AlignmentViewFilterContract {
    fn default() -> Self {
        Self::new(1, [])
    }
}

impl AlignmentViewFilterContract {
    #[must_use]
    pub fn new(
        version: u16,
        predicates: impl IntoIterator<Item = AlignmentFilterPredicate>,
    ) -> Self {
        let predicates = predicates.into_iter().collect::<Vec<_>>();
        let fingerprint = filter_fingerprint(version, &predicates);
        Self {
            version,
            predicates: predicates.into(),
            fingerprint,
        }
    }

    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    #[must_use]
    pub fn predicates(&self) -> &[AlignmentFilterPredicate] {
        &self.predicates
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }

    #[must_use]
    pub fn requires_reference(&self) -> bool {
        self.predicates
            .iter()
            .any(AlignmentFilterPredicate::requires_reference)
    }

    #[must_use]
    pub const fn fingerprint(&self) -> ContractFingerprint {
        self.fingerprint
    }

    /// Identity of one exact predicate evaluation authority. Intrinsic filters are independent of
    /// FASTA state; reference-derived filters include the immutable reference revision so cached
    /// rows or summaries can never be reused after the reference authority changes.
    #[must_use]
    pub fn evaluation_fingerprint(
        &self,
        reference: Option<ReferenceRevision>,
    ) -> ContractFingerprint {
        if !self.requires_reference() {
            return self.fingerprint;
        }
        let mut hash = StableHasher::default();
        hash.write(b"alignment-view-filter-evaluation-v1\0");
        hash.write(&self.fingerprint.get().to_le_bytes());
        if let Some(revision) = reference.map(ReferenceRevision::fingerprint) {
            hash.write(&[1]);
            hash.write(&revision.high().to_le_bytes());
            hash.write(&revision.low().to_le_bytes());
        } else {
            hash.write(&[0]);
        }
        hash.finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentFilterEvaluation {
    matching_handles: Vec<AlignmentHandle>,
    predicate_matches: Vec<usize>,
    eligible_records: usize,
}

impl AlignmentFilterEvaluation {
    #[must_use]
    pub fn matching_handles(&self) -> &[AlignmentHandle] {
        &self.matching_handles
    }

    #[must_use]
    pub fn into_matching_handles(self) -> Vec<AlignmentHandle> {
        self.matching_handles
    }

    #[must_use]
    pub fn predicate_matches(&self) -> &[usize] {
        &self.predicate_matches
    }

    #[must_use]
    pub const fn eligible_records(&self) -> usize {
        self.eligible_records
    }

    pub(crate) fn into_parts(self) -> (Vec<AlignmentHandle>, Vec<usize>, usize) {
        (
            self.matching_handles,
            self.predicate_matches,
            self.eligible_records,
        )
    }
}

pub fn evaluate_alignment_filter(
    evidence: &Arc<EvidenceRoot>,
    target: GenomicInterval,
    contract: &AlignmentViewFilterContract,
    reference: Option<&ReferenceSlice>,
) -> Result<AlignmentFilterEvaluation, AlignmentFilterEvaluationError> {
    if contract.requires_reference() && reference.is_none() {
        return Err(AlignmentFilterEvaluationError::MissingReference);
    }
    let selection = evidence.select(target)?;
    let mut matching_handles = Vec::with_capacity(selection.len());
    let mut predicate_matches = vec![0_usize; contract.predicates().len()];
    for index in 0..selection.len() {
        let record = selection
            .record_view(index)
            .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
        let mut all_match = true;
        for (predicate_index, predicate) in contract.predicates().iter().enumerate() {
            let matched = record_matches(&selection, index, record, predicate, reference)?;
            if matched {
                predicate_matches[predicate_index] =
                    predicate_matches[predicate_index].saturating_add(1);
            } else {
                all_match = false;
            }
        }
        if all_match {
            matching_handles.push(selection.records()[index].handle);
        }
    }
    Ok(AlignmentFilterEvaluation {
        matching_handles,
        predicate_matches,
        eligible_records: selection.len(),
    })
}

/// Returns the contiguous reference span required to evaluate every reference-backed predicate
/// without guessing. An `Anywhere` substitution predicate expands beyond the acquisition horizon
/// to include the full aligned span of every eligible overlapping record; anchored predicates
/// contribute only their fixed position or interval.
pub fn alignment_filter_reference_horizon(
    evidence: &Arc<EvidenceRoot>,
    target: GenomicInterval,
    contract: &AlignmentViewFilterContract,
    contig_length: u64,
) -> Result<GenomicInterval, AlignmentFilterEvaluationError> {
    let mut start = target.start();
    let mut end = target.end();
    let mut requires_record_spans = false;
    for predicate in contract.predicates() {
        let AlignmentFilterPredicate::Substitution { scope, .. } = predicate else {
            continue;
        };
        match *scope {
            AlignmentFilterScope::Anywhere => requires_record_spans = true,
            AlignmentFilterScope::Position { contig, position }
                if contig == target.contig() && position < contig_length =>
            {
                start = start.min(position);
                end = end.max(position.saturating_add(1).min(contig_length));
            }
            AlignmentFilterScope::Interval { interval }
                if interval.contig() == target.contig() && interval.start() < contig_length =>
            {
                start = start.min(interval.start());
                end = end.max(interval.end().min(contig_length));
            }
            AlignmentFilterScope::Position { .. }
            | AlignmentFilterScope::Interval { .. }
            | AlignmentFilterScope::UnresolvedContig => {}
        }
    }
    if requires_record_spans {
        let selection = evidence.select_eligible(target)?;
        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            if record.contig() == target.contig() {
                start = start.min(record.start());
                end = end.max(record.end().min(contig_length));
            }
        }
    }
    Ok(GenomicInterval::new(target.contig(), start, end)
        .expect("the non-empty source horizon remains part of the reference requirement"))
}

fn record_matches(
    selection: &crate::EvidenceSelection,
    index: usize,
    record: AlignmentRecordView<'_>,
    predicate: &AlignmentFilterPredicate,
    reference: Option<&ReferenceSlice>,
) -> Result<bool, AlignmentFilterEvaluationError> {
    Ok(match predicate {
        AlignmentFilterPredicate::MappingQuality { minimum, maximum } => {
            record.mapping_quality().is_some_and(|value| {
                in_range(
                    u64::from(value),
                    minimum.map(u64::from),
                    maximum.map(u64::from),
                )
            })
        }
        AlignmentFilterPredicate::Flag { flag, expected } => {
            flag_value(record.flags(), *flag) == *expected
        }
        AlignmentFilterPredicate::TemplateLength { minimum, maximum } => {
            in_range(record.template_length().unsigned_abs(), *minimum, *maximum)
        }
        AlignmentFilterPredicate::MateContig { contig } => {
            contig.is_some_and(|contig| record.mate_contig() == Some(contig))
        }
        AlignmentFilterPredicate::Auxiliary {
            tag,
            comparison,
            value,
        } => auxiliary_matches(
            selection
                .auxiliary_field(index, *tag)?
                .map(|field| field.value),
            *comparison,
            value.as_ref(),
        ),
        AlignmentFilterPredicate::CigarEvent {
            event,
            scope,
            minimum_length,
            maximum_length,
        } => cigar_event_matches(record, *event, *scope, *minimum_length, *maximum_length),
        AlignmentFilterPredicate::AlignedLength { minimum, maximum } => {
            let length = record
                .cigar()
                .iter()
                .filter(|operation| {
                    matches!(
                        operation.kind,
                        CigarOpKind::Match
                            | CigarOpKind::SequenceMatch
                            | CigarOpKind::SequenceMismatch
                    )
                })
                .fold(0_u64, |total, operation| {
                    total.saturating_add(u64::from(operation.len))
                });
            in_range(length, *minimum, *maximum)
        }
        AlignmentFilterPredicate::Substitution {
            scope,
            observed_base,
            minimum_count,
            maximum_count,
        } => substitution_matches(
            selection.sequence(index),
            record,
            *scope,
            *observed_base,
            *minimum_count,
            *maximum_count,
            reference.ok_or(AlignmentFilterEvaluationError::MissingReference)?,
        )?,
    })
}

fn flag_value(flags: AlignmentFlags, flag: AlignmentFlagPredicate) -> bool {
    match flag {
        AlignmentFlagPredicate::Duplicate => flags.is_duplicate(),
        AlignmentFlagPredicate::Secondary => flags.is_secondary(),
        AlignmentFlagPredicate::Supplementary => flags.is_supplementary(),
        AlignmentFlagPredicate::FailedQualityControl => flags.is_qc_failed(),
        AlignmentFlagPredicate::Paired => flags.is_paired(),
        AlignmentFlagPredicate::ProperPair => flags.is_proper_pair(),
        AlignmentFlagPredicate::MateMapped => flags.is_paired() && !flags.is_mate_unmapped(),
        AlignmentFlagPredicate::ReverseStrand => flags.is_reverse(),
    }
}

fn in_range<T: Ord>(value: T, minimum: Option<T>, maximum: Option<T>) -> bool {
    minimum.is_none_or(|minimum| value >= minimum) && maximum.is_none_or(|maximum| value <= maximum)
}

fn auxiliary_matches(
    actual: Option<AuxValue>,
    comparison: AlignmentAuxComparison,
    expected: Option<&AlignmentAuxFilterValue>,
) -> bool {
    if comparison == AlignmentAuxComparison::Exists {
        return actual.is_some();
    }
    let Some((actual, expected)) = actual.zip(expected) else {
        return false;
    };
    let ordering = match (&actual, expected) {
        (AuxValue::Signed(actual), AlignmentAuxFilterValue::Integer(expected)) => {
            actual.partial_cmp(expected)
        }
        (AuxValue::Unsigned(actual), AlignmentAuxFilterValue::Integer(expected)) => {
            u64::try_from(*expected)
                .ok()
                .and_then(|expected| actual.partial_cmp(&expected))
        }
        (AuxValue::Character(actual), AlignmentAuxFilterValue::Integer(expected)) => {
            i64::from(actual.to_owned()).partial_cmp(expected)
        }
        (AuxValue::Float(actual), AlignmentAuxFilterValue::FloatBits(expected)) => {
            f64::from(actual.to_owned()).partial_cmp(&f64::from_bits(expected.to_owned()))
        }
        (AuxValue::Double(actual), AlignmentAuxFilterValue::FloatBits(expected)) => {
            actual.partial_cmp(&f64::from_bits(*expected))
        }
        (AuxValue::String(actual), AlignmentAuxFilterValue::String(expected))
        | (AuxValue::HexByteArray(actual), AlignmentAuxFilterValue::String(expected)) => {
            actual.partial_cmp(expected)
        }
        _ => None,
    };
    comparison_from_ordering(comparison, ordering)
}

fn comparison_from_ordering(
    comparison: AlignmentAuxComparison,
    ordering: Option<std::cmp::Ordering>,
) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match comparison {
        AlignmentAuxComparison::Exists => true,
        AlignmentAuxComparison::Equal => ordering == Some(Equal),
        AlignmentAuxComparison::NotEqual => ordering.is_some_and(|value| value != Equal),
        AlignmentAuxComparison::Greater => ordering == Some(Greater),
        AlignmentAuxComparison::GreaterOrEqual => matches!(ordering, Some(Equal | Greater)),
        AlignmentAuxComparison::Less => ordering == Some(Less),
        AlignmentAuxComparison::LessOrEqual => matches!(ordering, Some(Equal | Less)),
    }
}

fn cigar_event_matches(
    record: AlignmentRecordView<'_>,
    requested: AlignmentCigarEventPredicate,
    scope: AlignmentFilterScope,
    minimum_length: Option<u32>,
    maximum_length: Option<u32>,
) -> bool {
    if scope == AlignmentFilterScope::UnresolvedContig {
        return false;
    }
    let mut reference_position = record.start();
    record.cigar().iter().any(|operation| {
        let start = reference_position;
        let consumes_reference = matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
                | CigarOpKind::Deletion
                | CigarOpKind::ReferenceSkip
        );
        if consumes_reference {
            reference_position = reference_position.saturating_add(u64::from(operation.len));
        }
        let event_matches = match requested {
            AlignmentCigarEventPredicate::Insertion => operation.kind == CigarOpKind::Insertion,
            AlignmentCigarEventPredicate::Deletion => operation.kind == CigarOpKind::Deletion,
            AlignmentCigarEventPredicate::Indel => {
                matches!(
                    operation.kind,
                    CigarOpKind::Insertion | CigarOpKind::Deletion
                )
            }
            AlignmentCigarEventPredicate::SoftClip => operation.kind == CigarOpKind::SoftClip,
            AlignmentCigarEventPredicate::HardClip => operation.kind == CigarOpKind::HardClip,
            AlignmentCigarEventPredicate::ReferenceSkip => {
                operation.kind == CigarOpKind::ReferenceSkip
            }
        };
        event_matches
            && in_range(operation.len, minimum_length, maximum_length)
            && event_scope_matches(
                record.contig(),
                scope,
                start,
                reference_position,
                operation.kind,
            )
    })
}

fn event_scope_matches(
    record_contig: ContigId,
    scope: AlignmentFilterScope,
    start: u64,
    end: u64,
    kind: CigarOpKind,
) -> bool {
    let point_event = matches!(
        kind,
        CigarOpKind::Insertion | CigarOpKind::SoftClip | CigarOpKind::HardClip
    );
    match scope {
        AlignmentFilterScope::Anywhere => true,
        AlignmentFilterScope::Position { contig, position } => {
            contig == record_contig
                && if point_event {
                    position == start
                } else {
                    position >= start && position < end
                }
        }
        AlignmentFilterScope::Interval { interval } => {
            interval.contig() == record_contig
                && if point_event {
                    interval.contains(start)
                } else {
                    start < interval.end() && end > interval.start()
                }
        }
        AlignmentFilterScope::UnresolvedContig => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn substitution_matches(
    sequence: Option<SequenceView<'_>>,
    record: AlignmentRecordView<'_>,
    scope: AlignmentFilterScope,
    observed_base: Option<u8>,
    minimum_count: Option<u32>,
    maximum_count: Option<u32>,
    reference: &ReferenceSlice,
) -> Result<bool, AlignmentFilterEvaluationError> {
    if scope == AlignmentFilterScope::UnresolvedContig {
        return Ok(false);
    }
    let bases = match sequence {
        Some(SequenceView::Present { bases, .. }) => bases,
        Some(SequenceView::NotLoaded) | None => {
            return Err(AlignmentFilterEvaluationError::MissingSequence);
        }
        Some(SequenceView::Missing) => {
            return Err(AlignmentFilterEvaluationError::RecordSequenceMissing);
        }
    };
    let mut reference_position = record.start();
    let mut read_position = 0_usize;
    let mut count = 0_u32;
    for operation in record.cigar() {
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                for offset in 0..operation.len as usize {
                    let position = reference_position.saturating_add(offset as u64);
                    if !base_scope_matches(record.contig(), scope, position) {
                        continue;
                    }
                    let observed = bases.get(read_position.saturating_add(offset)).copied();
                    let reference_base = reference
                        .base_at(position)
                        .ok_or(AlignmentFilterEvaluationError::IncompleteReference { position })?;
                    if classify_aligned_base(operation.kind, observed, Some(reference_base))
                        == AlignedBaseClassification::Substitution
                        && observed_base.is_none_or(|requested| {
                            observed
                                .is_some_and(|observed| observed.eq_ignore_ascii_case(&requested))
                        })
                    {
                        count = count.saturating_add(1);
                    }
                }
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    let minimum = minimum_count.or(Some(1));
    Ok(in_range(count, minimum, maximum_count))
}

fn base_scope_matches(contig: ContigId, scope: AlignmentFilterScope, position: u64) -> bool {
    match scope {
        AlignmentFilterScope::Anywhere => true,
        AlignmentFilterScope::Position {
            contig: requested,
            position: requested_position,
        } => contig == requested && position == requested_position,
        AlignmentFilterScope::Interval { interval } => {
            interval.contig() == contig && interval.contains(position)
        }
        AlignmentFilterScope::UnresolvedContig => false,
    }
}

fn filter_fingerprint(
    version: u16,
    predicates: &[AlignmentFilterPredicate],
) -> ContractFingerprint {
    if predicates.is_empty() {
        return ContractFingerprint::new(0);
    }
    let mut hash = StableHasher::default();
    hash.write(b"alignment-view-filter\0");
    hash.write(&version.to_le_bytes());
    for predicate in predicates {
        write_predicate(&mut hash, predicate);
        hash.write(&[0xff]);
    }
    hash.finish()
}

fn write_predicate(hash: &mut StableHasher, predicate: &AlignmentFilterPredicate) {
    match predicate {
        AlignmentFilterPredicate::MappingQuality { minimum, maximum } => {
            hash.write(&[0, minimum.unwrap_or(u8::MIN), maximum.unwrap_or(u8::MAX)]);
        }
        AlignmentFilterPredicate::Flag { flag, expected } => {
            hash.write(&[1, *flag as u8, u8::from(*expected)]);
        }
        AlignmentFilterPredicate::TemplateLength { minimum, maximum } => {
            hash.write(&[2]);
            write_optional_u64(hash, *minimum);
            write_optional_u64(hash, *maximum);
        }
        AlignmentFilterPredicate::MateContig { contig } => {
            hash.write(&[3]);
            write_optional_u64(hash, contig.map(|value| u64::from(value.get())));
        }
        AlignmentFilterPredicate::Auxiliary {
            tag,
            comparison,
            value,
        } => {
            hash.write(&[4, tag[0], tag[1], *comparison as u8]);
            match value {
                None => hash.write(&[0]),
                Some(AlignmentAuxFilterValue::Integer(value)) => {
                    hash.write(&[1]);
                    hash.write(&value.to_le_bytes());
                }
                Some(AlignmentAuxFilterValue::FloatBits(value)) => {
                    hash.write(&[2]);
                    hash.write(&value.to_le_bytes());
                }
                Some(AlignmentAuxFilterValue::String(value)) => {
                    hash.write(&[3]);
                    hash.write(value.as_bytes());
                }
            }
        }
        AlignmentFilterPredicate::CigarEvent {
            event,
            scope,
            minimum_length,
            maximum_length,
        } => {
            hash.write(&[5, *event as u8]);
            write_scope(hash, *scope);
            write_optional_u32(hash, *minimum_length);
            write_optional_u32(hash, *maximum_length);
        }
        AlignmentFilterPredicate::AlignedLength { minimum, maximum } => {
            hash.write(&[6]);
            write_optional_u64(hash, *minimum);
            write_optional_u64(hash, *maximum);
        }
        AlignmentFilterPredicate::Substitution {
            scope,
            observed_base,
            minimum_count,
            maximum_count,
        } => {
            hash.write(&[7, u8::from(observed_base.is_some())]);
            hash.write(&[observed_base.unwrap_or_default()]);
            write_scope(hash, *scope);
            write_optional_u32(hash, *minimum_count);
            write_optional_u32(hash, *maximum_count);
        }
    }
}

fn write_optional_u32(hash: &mut StableHasher, value: Option<u32>) {
    hash.write(&[u8::from(value.is_some())]);
    hash.write(&value.unwrap_or_default().to_le_bytes());
}

fn write_optional_u64(hash: &mut StableHasher, value: Option<u64>) {
    hash.write(&[u8::from(value.is_some())]);
    hash.write(&value.unwrap_or_default().to_le_bytes());
}

fn write_scope(hash: &mut StableHasher, scope: AlignmentFilterScope) {
    match scope {
        AlignmentFilterScope::Anywhere => hash.write(&[0]),
        AlignmentFilterScope::Position { contig, position } => {
            hash.write(&[1]);
            hash.write(&contig.get().to_le_bytes());
            hash.write(&position.to_le_bytes());
        }
        AlignmentFilterScope::Interval { interval } => {
            hash.write(&[2]);
            hash.write(&interval.contig().get().to_le_bytes());
            hash.write(&interval.start().to_le_bytes());
            hash.write(&interval.end().to_le_bytes());
        }
        AlignmentFilterScope::UnresolvedContig => hash.write(&[3]),
    }
}

#[derive(Debug, Error)]
pub enum AlignmentFilterEvaluationError {
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    PackedRecord(#[from] PackedRecordError),
    #[error("alignment filter requires retained sequence payload")]
    MissingSequence,
    #[error("alignment filter cannot classify a source record whose sequence is missing")]
    RecordSequenceMissing,
    #[error("alignment filter requires a matching reference")]
    MissingReference,
    #[error("alignment filter reference does not cover genomic position {position}")]
    IncompleteReference { position: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_model::{AlignmentBatch, AlignmentRecord, CigarOp, SequenceData};

    fn packed_record(cigar: Vec<CigarOp>, bases: &[u8]) -> AlignmentBatch {
        let reference_bases = cigar.iter().fold(0_u64, |total, operation| {
            if matches!(
                operation.kind,
                CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
                    | CigarOpKind::Deletion
                    | CigarOpKind::ReferenceSkip
            ) {
                total.saturating_add(u64::from(operation.len))
            } else {
                total
            }
        });
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 100 + reference_bases,
            flags: AlignmentFlags::from_bits(0x451),
            mapping_quality: Some(42),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(500),
            template_length: -400,
            read_name: b"filter-fixture".to_vec(),
            cigar,
            sequence: SequenceData::Present {
                bases: bases.to_vec(),
                qualities: Some(vec![30; bases.len()]),
            },
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        batch
    }

    #[test]
    fn empty_filter_has_canonical_zero_fingerprint() {
        let filter = AlignmentViewFilterContract::default();
        assert!(filter.is_empty());
        assert_eq!(filter.fingerprint(), ContractFingerprint::new(0));
    }

    #[test]
    fn anchored_predicates_have_distinct_fingerprints() {
        let first = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::CigarEvent {
                event: AlignmentCigarEventPredicate::Insertion,
                scope: AlignmentFilterScope::Position {
                    contig: ContigId::new(0),
                    position: 10,
                },
                minimum_length: None,
                maximum_length: None,
            }],
        );
        let second = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::CigarEvent {
                event: AlignmentCigarEventPredicate::Insertion,
                scope: AlignmentFilterScope::Position {
                    contig: ContigId::new(0),
                    position: 11,
                },
                minimum_length: None,
                maximum_length: None,
            }],
        );
        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn only_reference_derived_evaluations_include_the_reference_revision() {
        let intrinsic = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::MappingQuality {
                minimum: Some(30),
                maximum: None,
            }],
        );
        let reference_backed = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::Substitution {
                scope: AlignmentFilterScope::Anywhere,
                observed_base: None,
                minimum_count: Some(1),
                maximum_count: None,
            }],
        );
        let first = ReferenceRevision::new(crate::RevisionFingerprint::new(10, 11));
        let second = ReferenceRevision::new(crate::RevisionFingerprint::new(10, 12));
        assert_eq!(
            intrinsic.evaluation_fingerprint(Some(first)),
            intrinsic.evaluation_fingerprint(Some(second))
        );
        assert_ne!(
            reference_backed.evaluation_fingerprint(Some(first)),
            reference_backed.evaluation_fingerprint(Some(second))
        );
        assert_ne!(
            reference_backed.evaluation_fingerprint(None),
            reference_backed.evaluation_fingerprint(Some(first))
        );
    }

    #[test]
    fn optional_substitution_bounds_have_unambiguous_fingerprints() {
        let predicate = |minimum_count| {
            AlignmentViewFilterContract::new(
                1,
                [AlignmentFilterPredicate::Substitution {
                    scope: AlignmentFilterScope::Anywhere,
                    observed_base: None,
                    minimum_count,
                    maximum_count: None,
                }],
            )
        };
        assert_ne!(
            predicate(None).fingerprint(),
            predicate(Some(0)).fingerprint()
        );
    }

    #[test]
    fn cigar_event_scopes_obey_point_and_spanning_half_open_boundaries() {
        let batch = packed_record(
            vec![
                CigarOp {
                    kind: CigarOpKind::SoftClip,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 10,
                },
                CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::Deletion,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 4,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
            ],
            b"AACCCCCCCCCCGGTTTAAAAA",
        );
        let record = batch.record_view(0).unwrap();
        let at = |position| AlignmentFilterScope::Position {
            contig: ContigId::new(0),
            position,
        };
        assert!(cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::SoftClip,
            at(100),
            None,
            None
        ));
        assert!(cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::Insertion,
            at(110),
            Some(2),
            Some(2)
        ));
        assert!(!cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::Insertion,
            at(109),
            None,
            None
        ));
        assert!(cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::Deletion,
            at(114),
            None,
            None
        ));
        assert!(!cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::Deletion,
            at(115),
            None,
            None
        ));
        assert!(cigar_event_matches(
            record,
            AlignmentCigarEventPredicate::ReferenceSkip,
            at(117),
            Some(4),
            None
        ));
    }

    #[test]
    fn substitution_evaluation_is_exact_for_base_scope_identity_and_missing_reference() {
        let batch = packed_record(
            vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            b"ACGT",
        );
        let record = batch.record_view(0).unwrap();
        let sequence = SequenceView::Present {
            bases: b"ACGT",
            qualities: None,
        };
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 100, 104).unwrap(),
            bases: b"ACAT".to_vec(),
        };
        assert!(
            substitution_matches(
                Some(sequence),
                record,
                AlignmentFilterScope::Anywhere,
                Some(b'G'),
                Some(1),
                Some(1),
                &reference,
            )
            .unwrap()
        );
        assert!(
            !substitution_matches(
                Some(sequence),
                record,
                AlignmentFilterScope::Position {
                    contig: ContigId::new(0),
                    position: 101,
                },
                None,
                Some(1),
                None,
                &reference,
            )
            .unwrap()
        );
        assert!(
            substitution_matches(
                Some(sequence),
                record,
                AlignmentFilterScope::Position {
                    contig: ContigId::new(0),
                    position: 102,
                },
                None,
                Some(1),
                None,
                &reference,
            )
            .unwrap()
        );
        let incomplete = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 100, 102).unwrap(),
            bases: b"AC".to_vec(),
        };
        assert!(matches!(
            substitution_matches(
                Some(sequence),
                record,
                AlignmentFilterScope::Position {
                    contig: ContigId::new(0),
                    position: 102,
                },
                None,
                Some(1),
                None,
                &incomplete,
            ),
            Err(AlignmentFilterEvaluationError::IncompleteReference { position: 102 })
        ));
    }

    #[test]
    fn flag_and_auxiliary_predicates_do_not_coerce_missing_or_incompatible_values() {
        let flags = AlignmentFlags::from_bits(0x451);
        assert!(flag_value(flags, AlignmentFlagPredicate::Duplicate));
        assert!(flag_value(flags, AlignmentFlagPredicate::ReverseStrand));
        assert!(!flag_value(flags, AlignmentFlagPredicate::Secondary));
        assert!(!flag_value(
            AlignmentFlags::from_bits(0),
            AlignmentFlagPredicate::MateMapped
        ));
        assert!(auxiliary_matches(
            Some(AuxValue::Signed(7)),
            AlignmentAuxComparison::GreaterOrEqual,
            Some(&AlignmentAuxFilterValue::Integer(7))
        ));
        assert!(!auxiliary_matches(
            None,
            AlignmentAuxComparison::NotEqual,
            Some(&AlignmentAuxFilterValue::Integer(7))
        ));
        assert!(!auxiliary_matches(
            Some(AuxValue::String("7".to_owned())),
            AlignmentAuxComparison::Equal,
            Some(&AlignmentAuxFilterValue::Integer(7))
        ));
    }
}
