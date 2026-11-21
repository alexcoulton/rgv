//! Backend-neutral alignment record view and the derivations built on it.
//!
//! RGV ships two alignment decoders: `rust-htslib` (local BAM) and `noodles` (remote BAM, remote
//! and local CRAM). Everything that interprets a decoded record lives here once, written against
//! [`BackendRecord`]: normalisation, eligibility, pileup/depth/allele derivation with count-once
//! mate-overlap handling, the one-shot regional query fold, and the positioned-stream admission
//! rules. The I/O front ends in `local.rs` and `remote_alignment.rs` only produce records.

use std::{borrow::Cow, io, sync::Arc};

use noodles::sam::{
    self,
    alignment::record::{
        cigar::op::Kind,
        data::field::{Tag, Value, value::Array},
    },
};
use rgv_core::{ContigId, GenomicInterval};
use rust_htslib::bam::record::Aux;

use crate::{
    AlignmentBatch, AlignmentError, AlignmentFlags, AlignmentHeaderMetadata, AlignmentQuery,
    AlignmentRecord, AlignmentSourcePosition, AuxComparison, AuxField, AuxValue, CigarOp,
    CigarOpKind, IndividualReadStatus, PileupProvenance, PositionedAlignmentBlock,
    PositionedAlignmentRecord, QueryDetail, QueryOptions, QueryStats, SequenceData,
    alignment::FilteredRecordReason,
    bisulfite_strand_is_reverse,
    junction::{
        JunctionStrand, JunctionStrandInput, JunctionSummaryBuilder, RecordJunctionCollector,
    },
    modification_summary::ModificationSummaryBuilder,
    pair::PairSummaryBuilder,
    pileup::{
        MateAlignment, MateOverlapMask, OverlapCollapser, PileupBuilder, aligned_observation,
        base_observation_is_counted,
    },
    positioned_resume::{ChunkCompletion, ChunkEntry, PositionedChunkReceipt, ResumeLedger},
};

/// One decoded alignment record, as either backend exposes it.
///
/// Coordinates are zero-based. Accessors that a backend decodes lazily return `Result`.
pub(crate) trait BackendRecord {
    fn flags(&self) -> Result<AlignmentFlags, AlignmentError>;
    fn contig(&self) -> Result<Option<ContigId>, AlignmentError>;
    fn start(&self) -> Result<Option<u64>, AlignmentError>;
    /// `None` when the mapping quality is the SAM "unavailable" value 255.
    fn mapping_quality(&self) -> Result<Option<u8>, AlignmentError>;
    fn mate_contig(&self) -> Result<Option<ContigId>, AlignmentError>;
    fn mate_start(&self) -> Result<Option<u64>, AlignmentError>;
    fn template_length(&self) -> Result<i64, AlignmentError>;
    fn read_name(&self) -> &[u8];
    /// Replaces `cigar` with the record's real CIGAR (a `CG`-tag CIGAR already substituted).
    fn cigar_into(&self, cigar: &mut Vec<CigarOp>) -> Result<(), AlignmentError>;
    fn sequence_len(&self) -> usize;
    /// Decoded IUPAC bases, one byte per base.
    fn bases(&self) -> Vec<u8>;
    /// Phred qualities; all `0xff` or empty when the record stores none.
    fn qualities(&self) -> Result<Cow<'_, [u8]>, AlignmentError>;
    /// The first auxiliary field named `tag`.
    fn auxiliary(&self, tag: [u8; 2]) -> Result<Option<AuxValue>, AlignmentError>;
    /// Auxiliary fields in record order, restricted to the tags `retain` accepts. A `CG` field
    /// that only carried the real CIGAR of a long alignment is not an auxiliary field.
    fn auxiliary_fields(
        &self,
        retain: &dyn Fn([u8; 2]) -> bool,
    ) -> Result<Vec<AuxField>, AlignmentError>;
}

fn invalid_data(message: &'static str) -> AlignmentError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

// ---------------------------------------------------------------------------------------------
// rust-htslib
// ---------------------------------------------------------------------------------------------

impl BackendRecord for rust_htslib::bam::Record {
    fn flags(&self) -> Result<AlignmentFlags, AlignmentError> {
        Ok(AlignmentFlags::from_bits(Self::flags(self)))
    }

    fn contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        Ok(u32::try_from(self.tid()).ok().map(ContigId::new))
    }

    fn start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(u64::try_from(self.pos()).ok())
    }

    fn mapping_quality(&self) -> Result<Option<u8>, AlignmentError> {
        Ok((self.mapq() != u8::MAX).then_some(self.mapq()))
    }

    fn mate_contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        Ok(u32::try_from(self.mtid()).ok().map(ContigId::new))
    }

    fn mate_start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(u64::try_from(self.mpos()).ok())
    }

    fn template_length(&self) -> Result<i64, AlignmentError> {
        Ok(self.insert_size())
    }

    fn read_name(&self) -> &[u8] {
        self.qname()
    }

    fn cigar_into(&self, cigar: &mut Vec<CigarOp>) -> Result<(), AlignmentError> {
        cigar.clear();
        // HTSlib already moved a `CG` CIGAR into place when it read the record.
        for raw in self.raw_cigar() {
            let kind = match raw & 0xf {
                0 => CigarOpKind::Match,
                1 => CigarOpKind::Insertion,
                2 => CigarOpKind::Deletion,
                3 => CigarOpKind::ReferenceSkip,
                4 => CigarOpKind::SoftClip,
                5 => CigarOpKind::HardClip,
                6 => CigarOpKind::Padding,
                7 => CigarOpKind::SequenceMatch,
                8 => CigarOpKind::SequenceMismatch,
                _ => return Err(invalid_data("alignment has an unknown CIGAR operation")),
            };
            cigar.push(CigarOp {
                kind,
                len: raw >> 4,
            });
        }
        Ok(())
    }

    fn sequence_len(&self) -> usize {
        self.seq_len()
    }

    fn bases(&self) -> Vec<u8> {
        self.seq().as_bytes()
    }

    fn qualities(&self) -> Result<Cow<'_, [u8]>, AlignmentError> {
        Ok(Cow::Borrowed(self.qual()))
    }

    fn auxiliary(&self, tag: [u8; 2]) -> Result<Option<AuxValue>, AlignmentError> {
        for result in self.aux_iter() {
            let (field_tag, value) = result?;
            if field_tag == tag {
                return Ok(Some(normalize_htslib_aux(value)));
            }
        }
        Ok(None)
    }

    fn auxiliary_fields(
        &self,
        retain: &dyn Fn([u8; 2]) -> bool,
    ) -> Result<Vec<AuxField>, AlignmentError> {
        let mut fields = Vec::new();
        for result in self.aux_iter() {
            let (tag, value) = result?;
            let tag: [u8; 2] = tag.try_into().map_err(|_| AlignmentError::InvalidAuxTag)?;
            if retain(tag) {
                fields.push(AuxField {
                    tag,
                    value: normalize_htslib_aux(value),
                });
            }
        }
        Ok(fields)
    }
}

fn normalize_htslib_aux(value: Aux<'_>) -> AuxValue {
    match value {
        Aux::Char(value) => AuxValue::Character(value),
        Aux::I8(value) => AuxValue::Signed(i64::from(value)),
        Aux::U8(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::I16(value) => AuxValue::Signed(i64::from(value)),
        Aux::U16(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::I32(value) => AuxValue::Signed(i64::from(value)),
        Aux::U32(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::Float(value) => AuxValue::Float(value),
        Aux::Double(value) => AuxValue::Double(value),
        Aux::String(value) => AuxValue::String(value.to_owned()),
        Aux::HexByteArray(value) => AuxValue::HexByteArray(value.to_owned()),
        Aux::ArrayI8(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU8(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayI16(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU16(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayI32(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU32(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayFloat(values) => AuxValue::FloatArray(values.iter().collect()),
    }
}

// ---------------------------------------------------------------------------------------------
// noodles
// ---------------------------------------------------------------------------------------------

/// A noodles record (BAM record, lazy CRAM record or `RecordBuf`) with the header that resolves
/// its reference sequence names.
pub(crate) struct NoodlesRecord<'a, R: ?Sized> {
    record: &'a R,
    header: &'a sam::Header,
    /// A data field that is container bookkeeping rather than an auxiliary field of the
    /// alignment, which `HTSlib` consumes while decoding and never exposes.
    hidden_tag: Option<[u8; 2]>,
}

impl<'a, R> NoodlesRecord<'a, R>
where
    R: sam::alignment::Record + ?Sized,
{
    /// A record decoded from CRAM. `HTSlib` writes a private `cF:C` field recording which of
    /// `MD`/`NM` it dropped, and removes it again when decoding.
    pub(crate) const fn cram(record: &'a R, header: &'a sam::Header) -> Self {
        Self {
            record,
            header,
            hidden_tag: Some(*b"cF"),
        }
    }
}

impl<'a> NoodlesRecord<'a, noodles::bam::Record> {
    /// A BAM record, which may carry the CIGAR of a long alignment in its `CG` field: noodles
    /// substitutes that field for the placeholder CIGAR but still lists it as data.
    pub(crate) fn bam(record: &'a noodles::bam::Record, header: &'a sam::Header) -> Self {
        Self {
            record,
            header,
            hidden_tag: bam_cigar_is_cg_placeholder(record).then_some(*b"CG"),
        }
    }
}

/// A noodles record type that knows which [`BackendRecord`] view describes it.
pub(crate) trait AsBackendRecord: sam::alignment::Record + Sized {
    type View<'a>: BackendRecord
    where
        Self: 'a;

    fn backend_record<'a>(&'a self, header: &'a sam::Header) -> Self::View<'a>;
}

impl AsBackendRecord for noodles::bam::Record {
    type View<'a> = NoodlesBamRecord<'a>;

    fn backend_record<'a>(&'a self, header: &'a sam::Header) -> NoodlesBamRecord<'a> {
        NoodlesBamRecord(NoodlesRecord::bam(self, header))
    }
}

/// Owned records come only from the CRAM decoder.
impl AsBackendRecord for sam::alignment::RecordBuf {
    type View<'a> = NoodlesRecord<'a, Self>;

    fn backend_record<'a>(&'a self, header: &'a sam::Header) -> NoodlesRecord<'a, Self> {
        NoodlesRecord::cram(self, header)
    }
}

/// A noodles BAM record. It answers exactly as [`NoodlesRecord`] does, but reads the CIGAR,
/// bases and qualities from the record's bytes instead of through the boxed per-element
/// iterators of the generic alignment record trait, which dominate full-detail normalisation.
pub(crate) struct NoodlesBamRecord<'a>(NoodlesRecord<'a, noodles::bam::Record>);

impl BackendRecord for NoodlesBamRecord<'_> {
    fn flags(&self) -> Result<AlignmentFlags, AlignmentError> {
        self.0.flags()
    }

    fn contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        self.0.contig()
    }

    fn start(&self) -> Result<Option<u64>, AlignmentError> {
        self.0.start()
    }

    fn mapping_quality(&self) -> Result<Option<u8>, AlignmentError> {
        self.0.mapping_quality()
    }

    fn mate_contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        self.0.mate_contig()
    }

    fn mate_start(&self) -> Result<Option<u64>, AlignmentError> {
        self.0.mate_start()
    }

    fn template_length(&self) -> Result<i64, AlignmentError> {
        self.0.template_length()
    }

    fn read_name(&self) -> &[u8] {
        self.0.read_name()
    }

    fn cigar_into(&self, cigar: &mut Vec<CigarOp>) -> Result<(), AlignmentError> {
        cigar.clear();
        // The `CG` payload when the stored CIGAR is the long-alignment placeholder.
        let raw = self.0.record.cigar().as_bytes();
        cigar.reserve(raw.len() / 4);
        for chunk in raw.chunks_exact(4) {
            let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let kind = match raw & 0xf {
                0 => CigarOpKind::Match,
                1 => CigarOpKind::Insertion,
                2 => CigarOpKind::Deletion,
                3 => CigarOpKind::ReferenceSkip,
                4 => CigarOpKind::SoftClip,
                5 => CigarOpKind::HardClip,
                6 => CigarOpKind::Padding,
                7 => CigarOpKind::SequenceMatch,
                8 => CigarOpKind::SequenceMismatch,
                _ => return Err(invalid_data("alignment has an unknown CIGAR operation")),
            };
            cigar.push(CigarOp {
                kind,
                len: raw >> 4,
            });
        }
        Ok(())
    }

    fn sequence_len(&self) -> usize {
        self.0.record.sequence().len()
    }

    fn bases(&self) -> Vec<u8> {
        const BASES: [u8; 16] = *b"=ACMGRSVTWYHKDBN";
        let sequence = self.0.record.sequence();
        let length = sequence.len();
        let mut bases = Vec::with_capacity(length.saturating_add(1));
        for packed in sequence.as_bytes().iter().take(length.div_ceil(2)) {
            bases.push(BASES[usize::from(packed >> 4)]);
            bases.push(BASES[usize::from(packed & 0xf)]);
        }
        bases.truncate(length);
        bases
    }

    fn qualities(&self) -> Result<Cow<'_, [u8]>, AlignmentError> {
        Ok(Cow::Borrowed(self.0.record.quality_scores().as_bytes()))
    }

    fn auxiliary(&self, tag: [u8; 2]) -> Result<Option<AuxValue>, AlignmentError> {
        self.0.auxiliary(tag)
    }

    fn auxiliary_fields(
        &self,
        retain: &dyn Fn([u8; 2]) -> bool,
    ) -> Result<Vec<AuxField>, AlignmentError> {
        self.0.auxiliary_fields(retain)
    }
}

/// Whether a BAM record stores the `<read length>S<reference length>N` placeholder CIGAR and a
/// `CG:B,I` field holding the real one (SAM specification section 4.2.2). `HTSlib` replaces the
/// placeholder and removes the field when it reads such a record; noodles returns the field's
/// payload as the CIGAR, which is how the substitution is recognised here.
fn bam_cigar_is_cg_placeholder(record: &noodles::bam::Record) -> bool {
    let cigar = record.cigar().as_bytes();
    let data = record.data().as_bytes();
    !cigar.is_empty() && data.as_ptr_range().contains(&cigar.as_ptr())
}

impl<R> BackendRecord for NoodlesRecord<'_, R>
where
    R: sam::alignment::Record + ?Sized,
{
    fn flags(&self) -> Result<AlignmentFlags, AlignmentError> {
        Ok(AlignmentFlags::from_bits(self.record.flags()?.bits()))
    }

    fn contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        self.record
            .reference_sequence_id(self.header)
            .transpose()?
            .map(|id| {
                u32::try_from(id)
                    .map(ContigId::new)
                    .map_err(|_| AlignmentError::CoordinateOverflow)
            })
            .transpose()
    }

    fn start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(self
            .record
            .alignment_start()
            .transpose()?
            .map(|position| position.get() as u64 - 1))
    }

    fn mapping_quality(&self) -> Result<Option<u8>, AlignmentError> {
        Ok(self
            .record
            .mapping_quality()
            .transpose()?
            .map(|value| value.get()))
    }

    fn mate_contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        self.record
            .mate_reference_sequence_id(self.header)
            .transpose()?
            .map(|id| {
                u32::try_from(id)
                    .map(ContigId::new)
                    .map_err(|_| AlignmentError::CoordinateOverflow)
            })
            .transpose()
    }

    fn mate_start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(self
            .record
            .mate_alignment_start()
            .transpose()?
            .map(|position| position.get() as u64 - 1))
    }

    fn template_length(&self) -> Result<i64, AlignmentError> {
        Ok(i64::from(self.record.template_length()?))
    }

    fn read_name(&self) -> &[u8] {
        self.record.name().map_or(&[][..], AsRef::<[u8]>::as_ref)
    }

    fn cigar_into(&self, cigar: &mut Vec<CigarOp>) -> Result<(), AlignmentError> {
        cigar.clear();
        for result in self.record.cigar().iter() {
            let operation = result?;
            cigar.push(CigarOp {
                kind: match operation.kind() {
                    Kind::Match => CigarOpKind::Match,
                    Kind::Insertion => CigarOpKind::Insertion,
                    Kind::Deletion => CigarOpKind::Deletion,
                    Kind::Skip => CigarOpKind::ReferenceSkip,
                    Kind::SoftClip => CigarOpKind::SoftClip,
                    Kind::HardClip => CigarOpKind::HardClip,
                    Kind::Pad => CigarOpKind::Padding,
                    Kind::SequenceMatch => CigarOpKind::SequenceMatch,
                    Kind::SequenceMismatch => CigarOpKind::SequenceMismatch,
                },
                len: u32::try_from(operation.len())
                    .map_err(|_| AlignmentError::CoordinateOverflow)?,
            });
        }
        Ok(())
    }

    fn sequence_len(&self) -> usize {
        self.record.sequence().len()
    }

    fn bases(&self) -> Vec<u8> {
        self.record.sequence().iter().collect()
    }

    fn qualities(&self) -> Result<Cow<'_, [u8]>, AlignmentError> {
        Ok(Cow::Owned(
            self.record
                .quality_scores()
                .iter()
                .collect::<io::Result<Vec<_>>>()?,
        ))
    }

    fn auxiliary(&self, tag: [u8; 2]) -> Result<Option<AuxValue>, AlignmentError> {
        if self.hidden_tag == Some(tag) {
            return Ok(None);
        }
        self.record
            .data()
            .get(&Tag::new(tag[0], tag[1]))
            .transpose()?
            .map(normalize_noodles_aux)
            .transpose()
    }

    fn auxiliary_fields(
        &self,
        retain: &dyn Fn([u8; 2]) -> bool,
    ) -> Result<Vec<AuxField>, AlignmentError> {
        let mut fields = Vec::new();
        for result in self.record.data().iter() {
            let (tag, value) = result?;
            let tag: [u8; 2] = *tag.as_ref();
            if self.hidden_tag == Some(tag) {
                continue;
            }
            if retain(tag) {
                fields.push(AuxField {
                    tag,
                    value: normalize_noodles_aux(value)?,
                });
            }
        }
        Ok(fields)
    }
}

fn normalize_noodles_aux(value: Value<'_>) -> Result<AuxValue, AlignmentError> {
    Ok(match value {
        Value::Character(value) => AuxValue::Character(value),
        Value::Int8(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt8(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Int16(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt16(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Int32(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt32(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Float(value) => AuxValue::Float(value),
        Value::String(value) => AuxValue::String(String::from_utf8_lossy(value).into_owned()),
        Value::Hex(value) => AuxValue::HexByteArray(String::from_utf8_lossy(value).into_owned()),
        Value::Array(array) => normalize_noodles_array(array)?,
    })
}

fn normalize_noodles_array(array: Array<'_>) -> Result<AuxValue, AlignmentError> {
    fn signed<T: Into<i64>>(
        values: &dyn sam::alignment::record::data::field::value::array::Values<'_, T>,
    ) -> io::Result<AuxValue> {
        values
            .iter()
            .map(|value| value.map(Into::into))
            .collect::<io::Result<_>>()
            .map(AuxValue::SignedArray)
    }
    fn unsigned<T: Into<u64>>(
        values: &dyn sam::alignment::record::data::field::value::array::Values<'_, T>,
    ) -> io::Result<AuxValue> {
        values
            .iter()
            .map(|value| value.map(Into::into))
            .collect::<io::Result<_>>()
            .map(AuxValue::UnsignedArray)
    }
    Ok(match array {
        Array::Int8(values) => signed(&*values)?,
        Array::UInt8(values) => unsigned(&*values)?,
        Array::Int16(values) => signed(&*values)?,
        Array::UInt16(values) => unsigned(&*values)?,
        Array::Int32(values) => signed(&*values)?,
        Array::UInt32(values) => unsigned(&*values)?,
        Array::Float(values) => AuxValue::FloatArray(values.iter().collect::<io::Result<_>>()?),
    })
}

// ---------------------------------------------------------------------------------------------
// Derivations shared by both backends
// ---------------------------------------------------------------------------------------------

/// Reference bases a CIGAR consumes.
pub(crate) fn cigar_reference_length(cigar: &[CigarOp]) -> u64 {
    cigar
        .iter()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::Match
                    | CigarOpKind::Deletion
                    | CigarOpKind::ReferenceSkip
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
            )
        })
        .fold(0_u64, |length, operation| {
            length.saturating_add(u64::from(operation.len))
        })
}

/// Zero-based half-open reference span, `None` for a record without a placement or one that
/// consumes no reference base.
fn reference_span(start: Option<u64>, cigar: &[CigarOp]) -> Option<(u64, u64)> {
    let start = start?;
    let end = start.saturating_add(cigar_reference_length(cigar));
    (end > start).then_some((start, end))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Eligibility {
    Eligible,
    Unmapped,
    Secondary,
    Supplementary,
    UserFilter { duplicate: bool, qc_failed: bool },
}

/// Classifies a record against the visibility options and the user filter, in the fixed order
/// unmapped, secondary, supplementary, user filter.
pub(crate) fn eligibility<R>(
    record: &R,
    flags: AlignmentFlags,
    options: &QueryOptions,
) -> Result<Eligibility, AlignmentError>
where
    R: BackendRecord + ?Sized,
{
    if flags.is_unmapped() {
        return Ok(Eligibility::Unmapped);
    }
    if !options.include_secondary && flags.is_secondary() {
        return Ok(Eligibility::Secondary);
    }
    if !options.include_supplementary && flags.is_supplementary() {
        return Ok(Eligibility::Supplementary);
    }
    let filtered = Eligibility::UserFilter {
        duplicate: flags.is_duplicate(),
        qc_failed: flags.is_qc_failed(),
    };
    let filter = &options.filter;
    let bits = flags.bits();
    if bits & filter.required_flags != filter.required_flags || bits & filter.excluded_flags != 0 {
        return Ok(filtered);
    }
    if let Some(minimum) = filter.minimum_mapping_quality
        && record
            .mapping_quality()?
            .is_none_or(|value| value < minimum)
    {
        return Ok(filtered);
    }
    for predicate in &filter.auxiliary {
        let value = record.auxiliary(predicate.tag)?;
        let matches = if predicate.comparison == AuxComparison::Exists {
            value.is_some()
        } else {
            value
                .as_ref()
                .is_some_and(|value| predicate.matches_value(value))
        };
        if !matches {
            return Ok(filtered);
        }
    }
    Ok(Eligibility::Eligible)
}

fn note_ineligible(
    stats: &mut QueryStats,
    eligibility: Eligibility,
) -> Option<FilteredRecordReason> {
    match eligibility {
        Eligibility::Eligible => None,
        Eligibility::Unmapped => {
            stats.records_filtered_unmapped = stats.records_filtered_unmapped.saturating_add(1);
            Some(FilteredRecordReason::Unmapped)
        }
        Eligibility::Secondary => {
            stats.records_filtered_secondary = stats.records_filtered_secondary.saturating_add(1);
            Some(FilteredRecordReason::Secondary)
        }
        Eligibility::Supplementary => {
            stats.records_filtered_supplementary =
                stats.records_filtered_supplementary.saturating_add(1);
            Some(FilteredRecordReason::Supplementary)
        }
        Eligibility::UserFilter {
            duplicate,
            qc_failed,
        } => {
            stats.records_filtered_user = stats.records_filtered_user.saturating_add(1);
            stats.records_filtered_duplicate = stats
                .records_filtered_duplicate
                .saturating_add(usize::from(duplicate));
            stats.records_filtered_qc_failed = stats
                .records_filtered_qc_failed
                .saturating_add(usize::from(qc_failed));
            Some(FilteredRecordReason::User {
                duplicate,
                qc_failed,
            })
        }
    }
}

/// Builds RGV's normalised record from a backend record and its already decoded CIGAR.
pub(crate) fn normalize_record<R>(
    record: &R,
    cigar: &[CigarOp],
    detail: QueryDetail,
    retained_auxiliary_tags: &[[u8; 2]],
) -> Result<AlignmentRecord, AlignmentError>
where
    R: BackendRecord + ?Sized,
{
    let contig = record.contig()?.ok_or(AlignmentError::UnmappedRecord)?;
    let start = record.start()?.ok_or(AlignmentError::UnmappedRecord)?;
    let sequence = if detail == QueryDetail::Layout {
        SequenceData::NotLoaded
    } else if record.sequence_len() == 0 {
        SequenceData::Missing
    } else {
        let qualities = record.qualities()?;
        SequenceData::Present {
            bases: record.bases(),
            qualities: (!qualities.iter().all(|quality| *quality == u8::MAX))
                .then(|| qualities.into_owned()),
        }
    };
    let auxiliary_fields = if detail == QueryDetail::Full {
        record.auxiliary_fields(&|_| true)?
    } else if retained_auxiliary_tags.is_empty() {
        Vec::new()
    } else {
        record.auxiliary_fields(&|tag| retained_auxiliary_tags.contains(&tag))?
    };
    Ok(AlignmentRecord {
        contig,
        start,
        end: start
            .saturating_add(cigar_reference_length(cigar))
            .max(start.saturating_add(1)),
        flags: record.flags()?,
        mapping_quality: record.mapping_quality()?,
        mate_contig: record.mate_contig()?,
        mate_start: record.mate_start()?,
        template_length: record.template_length()?,
        read_name: record.read_name().to_vec(),
        cigar: cigar.to_vec(),
        sequence,
        auxiliary_fields,
    })
}

/// Splice-strand evidence carried by an `XS` field.
pub(crate) fn junction_strand_input(value: Option<&AuxValue>) -> JunctionStrandInput {
    match value {
        Some(AuxValue::Character(b'+')) => JunctionStrandInput::Tagged(JunctionStrand::Positive),
        Some(AuxValue::String(value)) if value == "+" => {
            JunctionStrandInput::Tagged(JunctionStrand::Positive)
        }
        Some(AuxValue::Character(b'-')) => JunctionStrandInput::Tagged(JunctionStrand::Negative),
        Some(AuxValue::String(value)) if value == "-" => {
            JunctionStrandInput::Tagged(JunctionStrand::Negative)
        }
        Some(_) => JunctionStrandInput::Invalid,
        None => JunctionStrandInput::Missing,
    }
}

/// The placed alignment a pileup walks: everything depth, allele, indel and junction
/// derivation reads from one record.
pub(crate) struct PileupAlignment<'a> {
    pub start: u64,
    pub cigar: &'a [CigarOp],
    pub flags: AlignmentFlags,
    pub mapping_quality: Option<u8>,
    /// Bases and qualities when allele observations are collected.
    pub observations: Option<(&'a [u8], &'a [u8])>,
    pub junction_strand: JunctionStrandInput,
}

/// Adds one alignment's depth, allele, indel and junction evidence, leaving out what
/// `overlap_mask` says its mate already contributed. Returns the collapsed base count.
pub(crate) fn add_alignment_pileup(
    alignment: &PileupAlignment<'_>,
    pileup: &mut PileupBuilder,
    junctions: &mut JunctionSummaryBuilder,
    overlap_mask: Option<&MateOverlapMask>,
) -> u64 {
    let mut reference_position = alignment.start;
    let mut read_position = 0_usize;
    let reverse = alignment.flags.is_reverse();
    let bisulfite_reverse = bisulfite_strand_is_reverse(alignment.flags);
    let mut collapsed_bases = 0_u64;
    let mut aligned_segments = Vec::new();
    let mut record_junctions = RecordJunctionCollector::default();
    for operation in alignment.cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                record_junctions.add_aligned(operation.len);
                let end = reference_position.saturating_add(u64::from(operation.len));
                let mut add_segment = |start, end| aligned_segments.push((start, end));
                collapsed_bases =
                    collapsed_bases.saturating_add(if let Some(mask) = overlap_mask {
                        mask.add_unmasked_depth_range(reference_position, end, add_segment)
                    } else {
                        add_segment(reference_position, end);
                        0
                    });
                if let Some((sequence, qualities)) = alignment.observations {
                    for offset in 0..length {
                        let position = reference_position.saturating_add(offset as u64);
                        if overlap_mask.is_some_and(|mask| mask.masks_base(position)) {
                            continue;
                        }
                        let Some((base, quality)) = aligned_observation(
                            sequence,
                            qualities,
                            read_position.saturating_add(offset),
                        ) else {
                            continue;
                        };
                        pileup.add_base_with_bisulfite_strand(
                            position,
                            base,
                            quality,
                            reverse,
                            bisulfite_reverse,
                        );
                    }
                }
                read_position = read_position.saturating_add(length);
                reference_position = end;
            }
            CigarOpKind::Deletion => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                let mut add_deletion = |start, end| pileup.add_deletion(start, end, reverse);
                collapsed_bases =
                    collapsed_bases.saturating_add(if let Some(mask) = overlap_mask {
                        mask.add_unmasked_deletion_range(reference_position, end, add_deletion)
                    } else {
                        add_deletion(reference_position, end);
                        0
                    });
                reference_position = end;
            }
            CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                record_junctions.add_skip(reference_position, end);
                reference_position = end;
            }
            CigarOpKind::Insertion => {
                if !overlap_mask.is_some_and(|mask| mask.masks_indel(reference_position)) {
                    pileup.add_insertion(reference_position, reverse);
                }
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    pileup.add_aligned_record(aligned_segments, reverse, alignment.mapping_quality);
    let (record_junctions, dropped_record_junctions) = record_junctions.finish();
    junctions.add_record_junctions(
        &record_junctions,
        dropped_record_junctions,
        reverse,
        alignment.junction_strand,
    );
    collapsed_bases
}

/// Folds the records of one indexed region into an [`AlignmentQuery`]: filter accounting,
/// the all-or-nothing individual-read limit, pair/junction/pileup/modification summaries and
/// count-once mate-overlap handling. A front end feeds it every record its index returns.
pub(crate) struct RegionQueryAccumulator<'o> {
    interval: GenomicInterval,
    options: &'o QueryOptions,
    stats: QueryStats,
    retained: Vec<(AlignmentRecord, Option<AlignmentSourcePosition>)>,
    filtered_records: AlignmentBatch,
    too_dense: bool,
    pileup: PileupBuilder,
    modifications: Option<ModificationSummaryBuilder>,
    junctions: JunctionSummaryBuilder,
    pairs: PairSummaryBuilder,
    overlap_collapser: OverlapCollapser,
    cigar: Vec<CigarOp>,
}

impl<'o> RegionQueryAccumulator<'o> {
    pub(crate) fn new(
        interval: GenomicInterval,
        options: &'o QueryOptions,
        normalization_mapped_records: Option<u64>,
    ) -> Self {
        Self {
            interval,
            options,
            stats: QueryStats {
                complete_read_limit: options.max_records.get(),
                ..QueryStats::default()
            },
            retained: Vec::new(),
            filtered_records: AlignmentBatch::default(),
            too_dense: false,
            pileup: PileupBuilder::new(
                interval,
                options.coverage_bins.get(),
                options.minimum_base_quality,
            ),
            modifications: options
                .collect_modifications
                .then(|| ModificationSummaryBuilder::new(interval, options.coverage_bins.get())),
            junctions: JunctionSummaryBuilder::new(
                interval,
                options.minimum_junction_flank,
                normalization_mapped_records,
            ),
            pairs: PairSummaryBuilder::new(),
            overlap_collapser: OverlapCollapser::new(options.overlapping_mate_policy),
            cigar: Vec::new(),
        }
    }

    pub(crate) const fn cancelled(&self) -> bool {
        self.stats.cancelled
    }

    /// Adds one record. Returns `false` once the query was cancelled; the record that observed
    /// the cancellation is counted as seen and nothing else.
    pub(crate) fn add<R>(
        &mut self,
        record: &R,
        source_position: Option<AlignmentSourcePosition>,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<bool, AlignmentError>
    where
        R: BackendRecord + ?Sized,
    {
        let options = self.options;
        self.stats.records_seen = self.stats.records_seen.saturating_add(1);
        if self
            .stats
            .records_seen
            .is_multiple_of(options.cancellation_chunk.get())
            && !keep_going()
        {
            self.stats.cancelled = true;
            return Ok(false);
        }
        let flags = record.flags()?;
        // One CIGAR decode per record: span, mate overlap, pileup and normalisation share it.
        let mut cigar = std::mem::take(&mut self.cigar);
        let result = self.add_decoded(record, flags, &mut cigar, source_position);
        self.cigar = cigar;
        result.map(|()| true)
    }

    #[allow(clippy::too_many_lines)]
    fn add_decoded<R>(
        &mut self,
        record: &R,
        flags: AlignmentFlags,
        cigar: &mut Vec<CigarOp>,
        source_position: Option<AlignmentSourcePosition>,
    ) -> Result<(), AlignmentError>
    where
        R: BackendRecord + ?Sized,
    {
        let options = self.options;
        record.cigar_into(cigar)?;
        let cigar = cigar.as_slice();
        let start = record.start()?;
        if let Some(reason) = note_ineligible(&mut self.stats, eligibility(record, flags, options)?)
        {
            if let Some((start, end)) = reference_span(start, cigar) {
                self.filtered_records
                    .push_filtered_record(start, end, reason);
            } else {
                self.filtered_records
                    .mark_filtered_record_spans_incomplete();
            }
            return Ok(());
        }
        let maximum_records = options.max_records.get();
        self.stats.records_eligible = self.stats.records_eligible.saturating_add(1);
        self.stats.complete_read_records_eligible =
            self.stats.complete_read_records_eligible.saturating_add(1);
        if self.stats.records_eligible > maximum_records && !self.too_dense {
            self.retained.clear();
            self.retained.shrink_to_fit();
            self.too_dense = true;
        }
        let contig = record.contig()?.unwrap_or(self.interval.contig());
        let mate_contig = record.mate_contig()?;
        let mate_start = record.mate_start()?;
        let read_name = record.read_name();
        self.pairs.add_fields(
            read_name,
            contig,
            flags,
            mate_contig,
            record.template_length()?,
        );
        let record_start = start.unwrap_or_default();
        let record_end = record_start.saturating_add(cigar_reference_length(cigar));
        // Bases are decoded only when alleles are collected; depth-only queries never touch
        // the packed sequence.
        let observations = options
            .collect_alleles
            .then(|| -> Result<_, AlignmentError> { Ok((record.bases(), record.qualities()?)) })
            .transpose()?;
        let observations = observations
            .as_ref()
            .map(|(bases, qualities)| (bases.as_slice(), qualities.as_ref()));
        let overlap_mask = self.overlap_collapser.mask_for_alignment(
            MateAlignment {
                read_name,
                contig,
                start: record_start,
                end: record_end,
                flags,
                mate_contig,
                mate_start,
            },
            cigar.iter().copied(),
            |read_position| {
                observations.is_none_or(|(bases, qualities)| {
                    base_observation_is_counted(
                        bases.len(),
                        qualities,
                        read_position,
                        options.minimum_base_quality,
                    )
                })
            },
        );
        if start.is_some() {
            let collapsed = add_alignment_pileup(
                &PileupAlignment {
                    start: record_start,
                    cigar,
                    flags,
                    mapping_quality: record.mapping_quality()?,
                    observations,
                    junction_strand: junction_strand_input(record.auxiliary(*b"XS")?.as_ref()),
                },
                &mut self.pileup,
                &mut self.junctions,
                overlap_mask.as_ref(),
            );
            self.overlap_collapser.note_collapsed(collapsed);
        }
        // Modification rendering, export, inspection and MCP discovery must all observe the
        // same complete record set, so the fully normalised record the summary paid for is the
        // one retained.
        let normalized_for_modifications = match &mut self.modifications {
            Some(builder) => {
                let normalized = normalize_record(record, cigar, QueryDetail::Full, &[])?;
                builder.add_record(&normalized);
                Some(normalized)
            }
            None => None,
        };
        if !self.too_dense {
            let normalized = match normalized_for_modifications {
                Some(normalized) => normalized,
                None => normalize_record(
                    record,
                    cigar,
                    options.detail,
                    &options.retained_auxiliary_tags,
                )?,
            };
            self.retained.push((normalized, source_position));
        }
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
        header: Arc<AlignmentHeaderMetadata>,
    ) -> Result<AlignmentQuery, AlignmentError> {
        self.retained.sort_by(|left, right| {
            (left.0.start, left.0.end, &left.0.read_name).cmp(&(
                right.0.start,
                right.0.end,
                &right.0.read_name,
            ))
        });
        let mut batch = AlignmentBatch::default();
        for (record, source_position) in &self.retained {
            if let Some(source_position) = source_position {
                batch
                    .push_with_source_position(record, *source_position)
                    .map_err(AlignmentError::BatchOverflow)?;
            } else {
                batch.push(record).map_err(AlignmentError::BatchOverflow)?;
            }
        }
        batch.absorb_filtered_records(self.filtered_records);
        let mut stats = self.stats;
        let maximum_records = self.options.max_records.get();
        stats.records_emitted = self.retained.len();
        stats.truncated = self.too_dense;
        stats.individual_reads = if self.too_dense {
            IndividualReadStatus::TooDense {
                limit: maximum_records,
            }
        } else {
            IndividualReadStatus::Complete
        };
        let mut pileup = self.pileup.finish(PileupProvenance {
            eligible_records: stats.records_eligible,
            rendered_records: stats.records_emitted,
            all_eligible_records: !stats.cancelled,
            allele_observations_available: self.options.collect_alleles,
            overlapping_mate_policy: self.overlap_collapser.policy(),
            overlap_collapse_complete: self.overlap_collapser.complete(),
            collapsed_mate_bases: self.overlap_collapser.collapsed_mate_bases(),
            ..PileupProvenance::default()
        });
        if let Some(modifications) = self.modifications {
            pileup.modifications = modifications.finish(stats.records_eligible, !stats.cancelled);
        }
        debug_assert!(batch.validate());
        Ok(AlignmentQuery {
            interval: self.interval,
            header,
            batch,
            pileup,
            junctions: self
                .junctions
                .finish(stats.records_eligible, !stats.cancelled),
            pairs: self.pairs.finish(!stats.cancelled),
            reference: None,
            stats,
        })
    }
}

/// Records of one physical owner unit being assembled by a positioned stream.
#[derive(Default)]
pub(crate) struct PositionedBlockBuilder {
    records: Vec<PositionedAlignmentRecord>,
    span: Option<(u64, u64)>,
    /// Placement and eligibility of every record seen, for the chunk's receipt.
    entries: Vec<ChunkEntry>,
    /// An eligible record was not retained (the Complete-read limit was crossed), or a record
    /// had no placement: the block is not the chunk's whole contribution.
    incomplete: bool,
}

impl PositionedBlockBuilder {
    /// The block will not hold everything the chunk contributes (the stream was cancelled
    /// inside it), so its receipt must not let a later stream skip the chunk.
    pub(crate) const fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    pub(crate) fn finish(
        &mut self,
        owner_offset: u64,
        interval: GenomicInterval,
    ) -> PositionedAlignmentBlock {
        let mut records = std::mem::take(&mut self.records);
        records.sort_by(|left, right| {
            (
                left.record.start,
                left.record.end,
                &left.record.read_name,
                left.source_position,
            )
                .cmp(&(
                    right.record.start,
                    right.record.end,
                    &right.record.read_name,
                    right.source_position,
                ))
        });
        PositionedAlignmentBlock {
            compressed_offset: owner_offset,
            records,
            genomic_span: self
                .span
                .take()
                .and_then(|(start, end)| GenomicInterval::new(interval.contig(), start, end).ok()),
        }
    }
}

/// Eligibility, Complete-read admission and retention rules of a positioned stream, applied
/// identically whichever decoder produced the record.
pub(crate) struct PositionedAdmission {
    pub(crate) interval: GenomicInterval,
    pub(crate) admission_interval: GenomicInterval,
    pub(crate) options: QueryOptions,
    pub(crate) stats: QueryStats,
    cigar: Vec<CigarOp>,
    resume: ResumeLedger,
}

impl PositionedAdmission {
    pub(crate) fn new(
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<Self, AlignmentError> {
        if !interval.contains_interval(admission_interval) {
            return Err(AlignmentError::AdmissionIntervalOutsideQuery {
                query: interval,
                admission: admission_interval,
            });
        }
        Ok(Self {
            interval,
            admission_interval,
            stats: QueryStats {
                complete_read_limit: options.max_records.get(),
                ..QueryStats::default()
            },
            resume: ResumeLedger::new(&options),
            options,
            cigar: Vec::new(),
        })
    }

    /// Installs the chunks this stream skips; see [`ResumeLedger::set_skipped`].
    pub(crate) fn set_skipped(
        &mut self,
        receipts: Vec<PositionedChunkReceipt>,
        usable: impl Fn(&PositionedChunkReceipt) -> bool,
    ) -> usize {
        self.resume
            .set_skipped(receipts, self.interval, &self.options, usable)
    }

    pub(crate) fn resume(&self) -> &ResumeLedger {
        &self.resume
    }

    /// For a decoder positioned at the virtual offset of the next record: accounts the skipped
    /// chunks lying there and returns the offset to continue from.
    pub(crate) fn skip_from_position(&mut self, position: u64) -> Option<u64> {
        let mut target = None;
        let mut position = position;
        while let Some(index) = self.resume.skipped_at(position) {
            let Some(end) = self.resume.account_run(
                index,
                self.interval,
                self.admission_interval,
                &mut self.stats,
                self.options.max_records.get(),
            ) else {
                break;
            };
            if end <= position {
                break;
            }
            position = end;
            target = Some(end);
        }
        target
    }

    /// For a decoder that just read a record owned by `owner`: when that chunk is skipped,
    /// accounts it (and the skipped chunks following it without a gap) and returns the offset
    /// to continue from. The record itself is already accounted and must be dropped.
    pub(crate) fn skip_from_owner(&mut self, owner: u64, record_end: u64) -> Option<u64> {
        let index = self.resume.skipped_owner(owner)?;
        // A record beyond what the receipt saw is not part of the skipped chunk. Coordinate
        // order rules it out; an unsorted file must not be able to stall the stream.
        if !self.resume.covers(index, record_end) {
            return None;
        }
        self.resume.account_run(
            index,
            self.interval,
            self.admission_interval,
            &mut self.stats,
            self.options.max_records.get(),
        )
    }

    /// Accounts every skipped chunk ordered before `(owner, landmark)`.
    pub(crate) fn account_skipped_before(&mut self, key: (u64, u64)) {
        self.resume.account_before(
            key,
            self.interval,
            self.admission_interval,
            &mut self.stats,
            self.options.max_records.get(),
        );
    }

    /// Whether the skipped chunk at the virtual offset `position` holds a record of the target.
    pub(crate) fn skipped_target_record_at(&self, position: u64) -> bool {
        self.resume
            .skipped_at(position)
            .is_some_and(|index| self.resume.starts_before(index, self.interval.end()))
    }

    /// Issues the receipt of the chunk `block` just finished.
    pub(crate) fn issue_receipt(
        &mut self,
        block: &mut PositionedBlockBuilder,
        completion: ChunkCompletion,
    ) -> PositionedChunkReceipt {
        let complete = !std::mem::take(&mut block.incomplete);
        self.resume.issue(
            completion,
            self.interval,
            std::mem::take(&mut block.entries),
            complete,
        )
    }

    /// Counts one record the index returned. Returns whether the caller should poll its
    /// cancellation probe before processing it.
    pub(crate) fn note_seen(&mut self) -> bool {
        self.stats.records_seen = self.stats.records_seen.saturating_add(1);
        self.stats
            .records_seen
            .is_multiple_of(self.options.cancellation_chunk.get())
    }

    /// Applies eligibility, admission and retention to one seen record of `block`, the chunk
    /// `owner` identifies.
    pub(crate) fn process<R>(
        &mut self,
        record: &R,
        source_position: AlignmentSourcePosition,
        owner: u64,
        block: &mut PositionedBlockBuilder,
    ) -> Result<(), AlignmentError>
    where
        R: BackendRecord + ?Sized,
    {
        let mut cigar = std::mem::take(&mut self.cigar);
        let result = self.process_decoded(record, &mut cigar, source_position, owner, block);
        self.cigar = cigar;
        result
    }

    fn process_decoded<R>(
        &mut self,
        record: &R,
        cigar: &mut Vec<CigarOp>,
        source_position: AlignmentSourcePosition,
        owner: u64,
        block: &mut PositionedBlockBuilder,
    ) -> Result<(), AlignmentError>
    where
        R: BackendRecord + ?Sized,
    {
        record.cigar_into(cigar)?;
        let cigar = cigar.as_slice();
        let start = record.start()?;
        let record_span = reference_span(start, cigar);
        if let Some((start, end)) = record_span {
            block.span = Some(block.span.map_or((start, end), |(minimum, maximum)| {
                (minimum.min(start), maximum.max(end))
            }));
        }
        let flags = record.flags()?;
        let eligibility = eligibility(record, flags, &self.options)?;
        if let Some(start) = start {
            block.entries.push(ChunkEntry::new(
                start,
                cigar_reference_length(cigar),
                eligibility,
            ));
            self.resume.note_seen_start(owner, start, self.interval);
        } else {
            block.incomplete = true;
        }
        if note_ineligible(&mut self.stats, eligibility).is_some() {
            return Ok(());
        }
        self.stats.records_eligible = self.stats.records_eligible.saturating_add(1);
        if record_span.is_some_and(|(start, end)| {
            start < self.admission_interval.end() && end > self.admission_interval.start()
        }) {
            self.stats.complete_read_records_eligible =
                self.stats.complete_read_records_eligible.saturating_add(1);
        }
        // Keep evaluating the exact filter population after the Complete admission limit is
        // crossed, but do not retain an unbounded rejected individual-read candidate. Blocks
        // already returned remain valid salvageable fragments for a later smaller target.
        if self.stats.complete_read_records_eligible > self.options.max_records.get() {
            block.incomplete = true;
            return Ok(());
        }
        let (detail, tags): (QueryDetail, &[[u8; 2]]) = if self.options.collect_modifications {
            (QueryDetail::Full, &[])
        } else {
            (self.options.detail, &self.options.retained_auxiliary_tags)
        };
        block.records.push(PositionedAlignmentRecord {
            record: normalize_record(record, cigar, detail, tags)?,
            source_position,
        });
        self.stats.records_emitted = self.stats.records_emitted.saturating_add(1);
        Ok(())
    }

    /// Final statistics of the transaction.
    pub(crate) fn finish(mut self) -> (GenomicInterval, QueryStats) {
        self.resume.account_remaining(
            self.interval,
            self.admission_interval,
            &mut self.stats,
            self.options.max_records.get(),
        );
        let limit = self.stats.complete_read_limit;
        if self.stats.complete_read_records_eligible > limit {
            self.stats.records_emitted = 0;
            self.stats.truncated = true;
            self.stats.individual_reads = IndividualReadStatus::TooDense { limit };
        } else {
            self.stats.truncated = false;
            self.stats.individual_reads = IndividualReadStatus::Complete;
        }
        (self.interval, self.stats)
    }
}
