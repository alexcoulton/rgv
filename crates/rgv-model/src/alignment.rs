use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use rgv_core::ContigId;
use thiserror::Error;

use crate::source_position::AlignmentSourcePosition;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentFlags(u16);

impl AlignmentFlags {
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_reverse(self) -> bool {
        self.0 & 0x10 != 0
    }

    pub const fn is_paired(self) -> bool {
        self.0 & 0x1 != 0
    }

    pub const fn is_proper_pair(self) -> bool {
        self.0 & 0x2 != 0
    }

    pub const fn is_unmapped(self) -> bool {
        self.0 & 0x4 != 0
    }

    pub const fn is_mate_unmapped(self) -> bool {
        self.0 & 0x8 != 0
    }

    pub const fn is_mate_reverse(self) -> bool {
        self.0 & 0x20 != 0
    }

    pub const fn is_first_in_template(self) -> bool {
        self.0 & 0x40 != 0
    }

    pub const fn is_last_in_template(self) -> bool {
        self.0 & 0x80 != 0
    }

    pub const fn is_secondary(self) -> bool {
        self.0 & 0x100 != 0
    }

    pub const fn is_supplementary(self) -> bool {
        self.0 & 0x800 != 0
    }

    pub const fn is_qc_failed(self) -> bool {
        self.0 & 0x200 != 0
    }

    pub const fn is_duplicate(self) -> bool {
        self.0 & 0x400 != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CigarOpKind {
    Match,
    Insertion,
    Deletion,
    ReferenceSkip,
    SoftClip,
    HardClip,
    Padding,
    SequenceMatch,
    SequenceMismatch,
}

/// The result of reconciling an aligned read base with its CIGAR assertion
/// and, when available, the loaded reference base.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignedBaseClassification {
    Match,
    Substitution,
    /// CIGAR `X` declares a substitution, but one of the bases required to
    /// validate that assertion is unavailable.
    DeclaredSubstitution,
    /// The loaded bases contradict an explicit CIGAR `=` or `X` assertion.
    EncodingConflict,
    Unknown,
}

pub fn classify_aligned_base(
    cigar_kind: CigarOpKind,
    observed: Option<u8>,
    reference: Option<u8>,
) -> AlignedBaseClassification {
    use AlignedBaseClassification::{
        DeclaredSubstitution, EncodingConflict, Match, Substitution, Unknown,
    };

    let bases_agree = observed
        .zip(reference)
        .map(|(observed, reference)| observed.eq_ignore_ascii_case(&reference));
    match (cigar_kind, bases_agree) {
        (CigarOpKind::SequenceMismatch, Some(false)) => Substitution,
        (CigarOpKind::SequenceMismatch, Some(true)) => EncodingConflict,
        (CigarOpKind::SequenceMismatch, None) => DeclaredSubstitution,
        (CigarOpKind::SequenceMatch, Some(true) | None) => Match,
        (CigarOpKind::SequenceMatch, Some(false)) => EncodingConflict,
        (CigarOpKind::Match, Some(true)) => Match,
        (CigarOpKind::Match, Some(false)) => Substitution,
        (CigarOpKind::Match, None) | (_, _) => Unknown,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CigarOp {
    pub kind: CigarOpKind,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SequenceData {
    NotLoaded,
    Missing,
    /// Bases and qualities in SAM/BAM alignment order. For an alignment with
    /// flag 0x10, SAM/BAM has already reverse-complemented SEQ and reversed
    /// QUAL, so CIGAR query positions index these arrays directly.
    Present {
        bases: Vec<u8>,
        qualities: Option<Vec<u8>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceView<'a> {
    NotLoaded,
    Missing,
    /// Borrowed sequence in the same SAM/BAM alignment order as
    /// [`SequenceData::Present`].
    Present {
        bases: &'a [u8],
        qualities: Option<&'a [u8]>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuxValue {
    Character(u8),
    Signed(i64),
    Unsigned(u64),
    Float(f32),
    Double(f64),
    String(String),
    HexByteArray(String),
    SignedArray(Vec<i64>),
    UnsignedArray(Vec<u64>),
    FloatArray(Vec<f32>),
}

pub const MAX_ALIGNMENT_READ_GROUPS: usize = 16_384;
pub const MAX_ALIGNMENT_READ_GROUP_ID_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UltimaTagOrientation {
    Reference,
    Synthesis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentReadGroup {
    pub id: Vec<u8>,
    pub platform: Option<String>,
    pub maximum_homopolymer_length: Option<u16>,
    pub ultima_tag_orientation: Option<UltimaTagOrientation>,
}

impl AlignmentReadGroup {
    pub fn is_ultima(&self) -> bool {
        self.platform
            .as_deref()
            .is_some_and(|platform| platform.eq_ignore_ascii_case("ULTIMA"))
            || self.maximum_homopolymer_length.is_some()
            || self.ultima_tag_orientation.is_some()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlignmentHeaderMetadata {
    pub read_groups: Vec<AlignmentReadGroup>,
    pub read_groups_omitted: usize,
    pub reference_sequences: Vec<AlignmentReferenceSequence>,
    pub reference_sequences_omitted: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentReferenceSequence {
    pub name: String,
    pub length: u64,
    pub md5: Option<String>,
    pub uri: Option<String>,
}

impl AlignmentHeaderMetadata {
    pub fn read_group(&self, id: &[u8]) -> Option<&AlignmentReadGroup> {
        self.read_groups
            .iter()
            .find(|read_group| read_group.id == id)
    }
}

#[doc(hidden)]
pub fn alignment_header_metadata_from_sam(bytes: &[u8]) -> AlignmentHeaderMetadata {
    let mut metadata = AlignmentHeaderMetadata::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.starts_with(b"@SQ\t") {
            let mut name = None;
            let mut length = None;
            let mut md5 = None;
            let mut uri = None;
            for field in line.split(|byte| *byte == b'\t').skip(1) {
                let Some(separator) = field.iter().position(|byte| *byte == b':') else {
                    continue;
                };
                let (tag, value) = (&field[..separator], &field[separator + 1..]);
                match tag {
                    b"SN" if !value.is_empty() && value.len() <= 256 => {
                        name = std::str::from_utf8(value).ok().map(ToOwned::to_owned);
                    }
                    b"LN" => {
                        length = std::str::from_utf8(value)
                            .ok()
                            .and_then(|value| value.parse::<u64>().ok());
                    }
                    b"M5" if value.len() == 32 => {
                        md5 = std::str::from_utf8(value).ok().map(ToOwned::to_owned);
                    }
                    b"UR" if value.len() <= 4_096 => {
                        uri = std::str::from_utf8(value).ok().map(ToOwned::to_owned);
                    }
                    _ => {}
                }
            }
            if let (Some(name), Some(length)) = (name, length) {
                if metadata.reference_sequences.len() < 16_384 {
                    metadata
                        .reference_sequences
                        .push(AlignmentReferenceSequence {
                            name,
                            length,
                            md5,
                            uri,
                        });
                } else {
                    metadata.reference_sequences_omitted =
                        metadata.reference_sequences_omitted.saturating_add(1);
                }
            }
            continue;
        }
        if !line.starts_with(b"@RG\t") {
            continue;
        }
        let mut id = None;
        let mut platform = None;
        let mut maximum_homopolymer_length = None;
        let mut ultima_tag_orientation = None;
        for field in line.split(|byte| *byte == b'\t').skip(1) {
            let Some(separator) = field.iter().position(|byte| *byte == b':') else {
                continue;
            };
            let (tag, value) = (&field[..separator], &field[separator + 1..]);
            match tag {
                b"ID" if !value.is_empty() && value.len() <= MAX_ALIGNMENT_READ_GROUP_ID_BYTES => {
                    id = Some(value.to_vec());
                }
                b"PL" if value.len() <= 64 => {
                    platform = std::str::from_utf8(value).ok().map(ToOwned::to_owned);
                }
                b"mc" => {
                    maximum_homopolymer_length = std::str::from_utf8(value)
                        .ok()
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|value| *value > 0);
                }
                b"tp" => {
                    ultima_tag_orientation = match value {
                        b"reference" => Some(UltimaTagOrientation::Reference),
                        b"synthesis" => Some(UltimaTagOrientation::Synthesis),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        let Some(id) = id else {
            continue;
        };
        if metadata.read_groups.len() == MAX_ALIGNMENT_READ_GROUPS {
            metadata.read_groups_omitted = metadata.read_groups_omitted.saturating_add(1);
            continue;
        }
        if metadata.read_group(&id).is_none() {
            metadata.read_groups.push(AlignmentReadGroup {
                id,
                platform,
                maximum_homopolymer_length,
                ultima_tag_orientation,
            });
        }
    }
    metadata
}

pub fn resolve_assembly_from_header(
    catalogue: &rgv_core::GenomeLandmarkCatalogue,
    metadata: &AlignmentHeaderMetadata,
) -> rgv_core::AssemblyResolution {
    let dictionary = rgv_core::ReferenceDictionary::from_contigs(
        metadata
            .reference_sequences
            .iter()
            .map(|sequence| (sequence.name.clone(), sequence.length)),
    );
    let sequence_md5 = metadata
        .reference_sequences
        .iter()
        .filter_map(|sequence| {
            sequence
                .md5
                .as_ref()
                .map(|md5| (sequence.name.clone(), md5.clone()))
        })
        .collect();
    catalogue.resolve(&rgv_core::AssemblyResolutionInput {
        explicit_stable_id: None,
        dictionary: &dictionary,
        sequence_md5: &sequence_md5,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuxField {
    pub tag: [u8; 2],
    pub value: AuxValue,
}

/// Lossless normalized alignment materialized only at API boundaries such as inspection.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentRecord {
    pub contig: ContigId,
    pub start: u64,
    pub end: u64,
    pub flags: AlignmentFlags,
    pub mapping_quality: Option<u8>,
    pub mate_contig: Option<ContigId>,
    pub mate_start: Option<u64>,
    pub template_length: i64,
    pub read_name: Vec<u8>,
    pub cigar: Vec<CigarOp>,
    pub sequence: SequenceData,
    pub auxiliary_fields: Vec<AuxField>,
}

/// A zero-allocation view over one record in a packed [`AlignmentBatch`].
///
/// Resident-window projection uses this form so selecting a smaller viewport
/// does not first recreate an allocation graph for every retained read.
#[derive(Clone, Copy, Debug)]
pub struct AlignmentRecordView<'a> {
    batch: &'a AlignmentBatch,
    index: usize,
}

impl<'a> AlignmentRecordView<'a> {
    pub fn source_position(self) -> Option<AlignmentSourcePosition> {
        self.batch.source_position(self.index)
    }

    pub fn contig(self) -> ContigId {
        self.batch
            .contig(self.index)
            .expect("a validated packed record has a contig")
    }

    pub fn start(self) -> u64 {
        self.batch
            .start(self.index)
            .expect("a validated packed record has a start")
    }

    pub fn end(self) -> u64 {
        self.batch
            .end(self.index)
            .expect("a validated packed record has an end")
    }

    pub fn flags(self) -> AlignmentFlags {
        self.batch
            .flags(self.index)
            .expect("a validated packed record has flags")
    }

    pub fn mapping_quality(self) -> Option<u8> {
        self.batch.mapping_quality(self.index).flatten()
    }

    pub fn mate_contig(self) -> Option<ContigId> {
        self.batch.mate_contig(self.index).flatten()
    }

    pub fn mate_start(self) -> Option<u64> {
        self.batch.mate_start(self.index).flatten()
    }

    pub fn template_length(self) -> i64 {
        self.batch
            .template_length(self.index)
            .expect("a validated packed record has a template length")
    }

    pub fn read_name(self) -> &'a [u8] {
        self.batch
            .read_name(self.index)
            .expect("a validated packed record has a read name")
    }

    pub fn cigar(self) -> &'a [CigarOp] {
        self.batch
            .cigar(self.index)
            .expect("a validated packed record has a CIGAR")
    }

    pub fn sequence(self) -> SequenceView<'a> {
        self.batch
            .sequence(self.index)
            .expect("a validated packed record has a sequence state")
    }

    pub fn has_auxiliary_tag(self, tag: [u8; 2]) -> bool {
        self.batch.has_auxiliary_tag(self.index, tag)
    }

    pub fn auxiliary_field(self, tag: [u8; 2]) -> Result<Option<AuxField>, PackedRecordError> {
        self.batch.auxiliary_field(self.index, tag)
    }

    pub fn materialize(self) -> Result<AlignmentRecord, PackedRecordError> {
        self.batch.materialize_record(self.index)
    }

    fn auxiliary_bytes(self) -> Option<&'a [u8]> {
        self.batch.auxiliary_bytes(self.index)
    }

    fn primary_order(self) -> (u64, u64, &'a [u8]) {
        (self.start(), self.end(), self.read_name())
    }

    fn stable_fingerprint(self) -> (u64, u64) {
        self.batch.stable_fingerprint(self.index)
    }

    fn exactly_matches(self, other: Self) -> bool {
        self.batch
            .records_exactly_match(self.index, other.batch, other.index)
    }
}

const ALIGNMENT_OVERLAP_INDEX_BLOCK_RECORDS: usize = 256;

/// A compact block-max index for selecting every record that overlaps a
/// contained target, including reads that start far before it because of a
/// long alignment or RNA splice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlignmentOverlapIndex {
    maximum_end_by_block: Vec<u64>,
}

impl AlignmentOverlapIndex {
    pub fn new(batch: &AlignmentBatch) -> Self {
        let maximum_end_by_block = (0..batch.len())
            .step_by(ALIGNMENT_OVERLAP_INDEX_BLOCK_RECORDS)
            .map(|first| {
                let last = first
                    .saturating_add(ALIGNMENT_OVERLAP_INDEX_BLOCK_RECORDS)
                    .min(batch.len());
                (first..last)
                    .filter_map(|index| batch.end(index))
                    .max()
                    .unwrap_or_default()
            })
            .collect();
        Self {
            maximum_end_by_block,
        }
    }

    pub fn select(
        &self,
        batch: &AlignmentBatch,
        interval: rgv_core::GenomicInterval,
    ) -> Vec<usize> {
        let mut selected = Vec::with_capacity(self.count(batch, interval));
        self.visit(batch, interval, |index| selected.push(index));
        selected
    }

    /// Counts overlapping physical records without allocating a temporary selection. Scene
    /// builders use this as an exact upper bound before reserving transient selection capacity.
    pub fn count(&self, batch: &AlignmentBatch, interval: rgv_core::GenomicInterval) -> usize {
        let mut count = 0_usize;
        self.visit(batch, interval, |_| count = count.saturating_add(1));
        count
    }

    fn visit(
        &self,
        batch: &AlignmentBatch,
        interval: rgv_core::GenomicInterval,
        mut visit: impl FnMut(usize),
    ) {
        let upper = batch.partition_point_start(|start| start < interval.end());
        for (block, maximum_end) in self.maximum_end_by_block.iter().copied().enumerate() {
            let first = block.saturating_mul(ALIGNMENT_OVERLAP_INDEX_BLOCK_RECORDS);
            if first >= upper {
                break;
            }
            if maximum_end <= interval.start() {
                continue;
            }
            let last = first
                .saturating_add(ALIGNMENT_OVERLAP_INDEX_BLOCK_RECORDS)
                .min(upper);
            for index in first..last {
                if batch.contig(index) == Some(interval.contig())
                    && batch
                        .start(index)
                        .is_some_and(|start| start < interval.end())
                    && batch.end(index).is_some_and(|end| end > interval.start())
                {
                    visit(index);
                }
            }
        }
    }

    pub fn estimated_bytes(&self) -> usize {
        self.maximum_end_by_block
            .capacity()
            .saturating_mul(std::mem::size_of::<u64>())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[doc(hidden)]
pub enum FilteredRecordReason {
    Unmapped,
    Secondary,
    Supplementary,
    User { duplicate: bool, qc_failed: bool },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FilteredRecordSpan {
    start: u64,
    end: u64,
    reason: FilteredRecordReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackedSequenceState {
    NotLoaded,
    Missing,
    PresentWithoutQualities,
    PresentWithQualities,
}

/// A column-oriented alignment batch with packed arenas for every variable-width field.
///
/// Auxiliary values use a private length-delimited representation. Consumers can test for a tag
/// without decoding it, decode one selected tag for colouring, or materialize one complete record
/// for the inspector. This avoids a second heap allocation graph for every displayed alignment.
#[derive(Clone, Debug, PartialEq)]
struct AlignmentBatchStorage {
    contigs: Vec<ContigId>,
    starts: Vec<u64>,
    ends: Vec<u64>,
    flags: Vec<AlignmentFlags>,
    mapping_qualities: Vec<Option<u8>>,
    mate_contigs: Vec<Option<ContigId>>,
    mate_starts: Vec<Option<u64>>,
    template_lengths: Vec<i64>,
    read_name_offsets: Vec<u32>,
    read_names: Vec<u8>,
    cigar_offsets: Vec<u32>,
    cigar_ops: Vec<CigarOp>,
    sequence_states: Vec<PackedSequenceState>,
    sequence_offsets: Vec<u32>,
    sequences: Vec<u8>,
    quality_offsets: Vec<u32>,
    qualities: Vec<u8>,
    auxiliary_offsets: Vec<u32>,
    auxiliary_data: Vec<u8>,
    stable_ids: Vec<AlignmentStableId>,
    source_positions: Vec<Option<AlignmentSourcePosition>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompositeRecordIndex {
    storage: u32,
    record: u32,
}

#[derive(Clone, Debug)]
struct CompositeAlignmentStorage {
    storages: Arc<[Arc<AlignmentBatchStorage>]>,
    records: Arc<[CompositeRecordIndex]>,
}

impl Default for AlignmentBatchStorage {
    fn default() -> Self {
        Self {
            contigs: Vec::new(),
            starts: Vec::new(),
            ends: Vec::new(),
            flags: Vec::new(),
            mapping_qualities: Vec::new(),
            mate_contigs: Vec::new(),
            mate_starts: Vec::new(),
            template_lengths: Vec::new(),
            read_name_offsets: vec![0],
            read_names: Vec::new(),
            cigar_offsets: vec![0],
            cigar_ops: Vec::new(),
            sequence_states: Vec::new(),
            sequence_offsets: vec![0],
            sequences: Vec::new(),
            quality_offsets: vec![0],
            qualities: Vec::new(),
            auxiliary_offsets: vec![0],
            auxiliary_data: Vec::new(),
            stable_ids: Vec::new(),
            source_positions: Vec::new(),
        }
    }
}

/// Immutable packed alignment storage with an optional zero-copy logical projection.
///
/// Source queries own one packed storage allocation. A resident viewport keeps an Arc to that
/// allocation plus compact physical indices, so deriving a contained target does not duplicate
/// fixed columns or variable-width read arenas.
#[derive(Clone, Debug)]
pub struct AlignmentBatch {
    storage: Arc<AlignmentBatchStorage>,
    physical_indices: Option<Arc<[u32]>>,
    composite: Option<Arc<CompositeAlignmentStorage>>,
    /// Compact spatial provenance for records rejected by the active query
    /// contract. It lets a contained resident projection rebuild exact filter
    /// counters without retaining those records' sequence or auxiliary data.
    filtered_record_spans: Vec<FilteredRecordSpan>,
    filtered_record_spans_complete: bool,
}

/// Packed optional record payload keyed in the same physical order as an alignment core batch.
///
/// This deliberately contains no coordinates, names, flags, mates, or CIGAR operations. A scene
/// can therefore evict sequence/quality/tag capability without retaining a duplicate core record
/// allocation.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentPayloadBatch {
    sequence_capability_retained: bool,
    quality_capability_retained: bool,
    all_auxiliary_tags_retained: bool,
    retained_auxiliary_tags: Arc<[[u8; 2]]>,
    source_positions: Vec<Option<AlignmentSourcePosition>>,
    sequence_states: Vec<PackedSequenceState>,
    sequence_offsets: Vec<u32>,
    sequences: Vec<u8>,
    quality_offsets: Vec<u32>,
    qualities: Vec<u8>,
    auxiliary_offsets: Vec<u32>,
    auxiliary_data: Vec<u8>,
}

impl Default for AlignmentPayloadBatch {
    fn default() -> Self {
        Self {
            sequence_capability_retained: false,
            quality_capability_retained: false,
            all_auxiliary_tags_retained: false,
            retained_auxiliary_tags: Arc::from([]),
            source_positions: Vec::new(),
            sequence_states: Vec::new(),
            sequence_offsets: vec![0],
            sequences: Vec::new(),
            quality_offsets: vec![0],
            qualities: Vec::new(),
            auxiliary_offsets: vec![0],
            auxiliary_data: Vec::new(),
        }
    }
}

/// Process-local identity for one immutable packed record allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentStorageRecordId {
    storage: usize,
    record: u32,
}

/// Source-independent identity for one immutable alignment record.
///
/// The identity deliberately excludes sequence, qualities, and arbitrary auxiliary tags so a
/// layout-only horizon and a full-detail foreground query produce the same value. Exact duplicate
/// records are disambiguated by their deterministic occurrence at the layout layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentStableId {
    primary: u64,
    secondary: u64,
}

impl AlignmentStableId {
    pub const fn primary(self) -> u64 {
        self.primary
    }

    pub const fn secondary(self) -> u64 {
        self.secondary
    }
}

impl AlignmentStorageRecordId {
    pub const fn storage(self) -> usize {
        self.storage
    }

    pub const fn record(self) -> u32 {
        self.record
    }
}

impl PartialEq for AlignmentBatch {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self.filtered_record_spans == other.filtered_record_spans
            && self.filtered_record_spans_complete == other.filtered_record_spans_complete
            && (0..self.len()).all(|index| self.records_exactly_match(index, other, index))
    }
}

impl Default for AlignmentBatch {
    fn default() -> Self {
        Self {
            storage: Arc::new(AlignmentBatchStorage::default()),
            physical_indices: None,
            composite: None,
            filtered_record_spans: Vec::new(),
            filtered_record_spans_complete: true,
        }
    }
}

impl AlignmentBatch {
    pub fn len(&self) -> usize {
        self.composite.as_ref().map_or_else(
            || {
                self.physical_indices
                    .as_ref()
                    .map_or(self.storage.starts.len(), |indices| indices.len())
            },
            |composite| composite.records.len(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_projection(&self) -> bool {
        self.physical_indices.is_some() || self.composite.is_some()
    }

    pub fn projection_indices_bytes(&self) -> usize {
        self.composite.as_ref().map_or_else(
            || {
                self.physical_indices.as_ref().map_or(0, |indices| {
                    indices.len().saturating_mul(std::mem::size_of::<u32>())
                })
            },
            |composite| {
                composite
                    .records
                    .len()
                    .saturating_mul(std::mem::size_of::<CompositeRecordIndex>())
            },
        )
    }

    /// Whether this batch retains every packed storage allocation referenced by `other`.
    pub fn retains_all_storage_from(&self, other: &Self) -> bool {
        let retained = self.backing_storages();
        other.backing_storages().iter().all(|required| {
            retained
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate, required))
        })
    }

    pub fn storage_record_id(&self, index: usize) -> Option<AlignmentStorageRecordId> {
        if let Some(composite) = &self.composite {
            let location = composite.records.get(index)?;
            let storage = composite.storages.get(location.storage as usize)?;
            return Some(AlignmentStorageRecordId {
                storage: Arc::as_ptr(storage) as usize,
                record: location.record,
            });
        }
        Some(AlignmentStorageRecordId {
            storage: Arc::as_ptr(&self.storage) as usize,
            record: u32::try_from(self.physical_index(index)?).ok()?,
        })
    }

    /// Process-local identities of the immutable packed allocations retained
    /// by this batch. Resident consumers use this small set to distinguish a
    /// zero-copy slab extension from an independently decoded equivalent
    /// query without walking every logical record first.
    pub fn storage_ids(&self) -> Vec<usize> {
        self.backing_storages()
            .iter()
            .map(|storage| Arc::as_ptr(storage) as usize)
            .collect()
    }

    pub fn stable_id(&self, index: usize) -> Option<AlignmentStableId> {
        let (storage, physical) = self.record_location(index)?;
        storage.stable_ids.get(physical).copied()
    }

    /// Returns the stable physical source position captured while decoding this record.
    ///
    /// Hand-built records used by tests and compatibility callers can be unpositioned. The
    /// alignment-scene ingestion boundary requires this to be `Some` for production evidence.
    pub fn source_position(&self, index: usize) -> Option<AlignmentSourcePosition> {
        let (storage, physical) = self.record_location(index)?;
        storage.source_positions.get(physical).copied().flatten()
    }

    /// Creates a zero-copy logical projection over this batch.
    pub fn project(&self, indices: &[usize]) -> Result<Self, BatchOverflow> {
        if let Some(composite) = &self.composite {
            let records = indices
                .iter()
                .map(|index| composite.records.get(*index).copied().ok_or(BatchOverflow))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Self {
                storage: Arc::clone(&self.storage),
                physical_indices: None,
                composite: Some(Arc::new(CompositeAlignmentStorage {
                    storages: Arc::clone(&composite.storages),
                    records: records.into(),
                })),
                filtered_record_spans: Vec::new(),
                filtered_record_spans_complete: self.filtered_record_spans_complete,
            });
        }
        let mut physical = Vec::with_capacity(indices.len());
        for index in indices {
            let physical_index = self.physical_index(*index).ok_or(BatchOverflow)?;
            physical.push(u32::try_from(physical_index).map_err(|_| BatchOverflow)?);
        }
        Ok(Self {
            storage: Arc::clone(&self.storage),
            physical_indices: Some(physical.into()),
            composite: None,
            filtered_record_spans: Vec::new(),
            filtered_record_spans_complete: self.filtered_record_spans_complete,
        })
    }

    fn physical_index(&self, index: usize) -> Option<usize> {
        if self.composite.is_some() {
            return None;
        }
        self.physical_indices.as_ref().map_or_else(
            || (index < self.storage.starts.len()).then_some(index),
            |indices| indices.get(index).map(|value| *value as usize),
        )
    }

    fn storage_mut(&mut self) -> Result<&mut AlignmentBatchStorage, BatchOverflow> {
        if self.physical_indices.is_some() || self.composite.is_some() {
            return Err(BatchOverflow);
        }
        Ok(Arc::make_mut(&mut self.storage))
    }

    fn record_location(&self, index: usize) -> Option<(&AlignmentBatchStorage, usize)> {
        if let Some(composite) = &self.composite {
            let location = composite.records.get(index)?;
            let storage = composite.storages.get(location.storage as usize)?;
            let record = location.record as usize;
            return (record < storage.starts.len()).then_some((storage, record));
        }
        let physical = self.physical_index(index)?;
        Some((&self.storage, physical))
    }

    pub fn contig(&self, index: usize) -> Option<ContigId> {
        let (storage, physical) = self.record_location(index)?;
        storage.contigs.get(physical).copied()
    }

    pub fn start(&self, index: usize) -> Option<u64> {
        let (storage, physical) = self.record_location(index)?;
        storage.starts.get(physical).copied()
    }

    pub fn end(&self, index: usize) -> Option<u64> {
        let (storage, physical) = self.record_location(index)?;
        storage.ends.get(physical).copied()
    }

    pub fn flags(&self, index: usize) -> Option<AlignmentFlags> {
        let (storage, physical) = self.record_location(index)?;
        storage.flags.get(physical).copied()
    }

    pub fn mapping_quality(&self, index: usize) -> Option<Option<u8>> {
        let (storage, physical) = self.record_location(index)?;
        storage.mapping_qualities.get(physical).copied()
    }

    pub fn mate_contig(&self, index: usize) -> Option<Option<ContigId>> {
        let (storage, physical) = self.record_location(index)?;
        storage.mate_contigs.get(physical).copied()
    }

    pub fn mate_start(&self, index: usize) -> Option<Option<u64>> {
        let (storage, physical) = self.record_location(index)?;
        storage.mate_starts.get(physical).copied()
    }

    pub fn template_length(&self, index: usize) -> Option<i64> {
        let (storage, physical) = self.record_location(index)?;
        storage.template_lengths.get(physical).copied()
    }

    pub fn starts(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        (0..self.len()).map(|index| self.start(index).expect("validated start"))
    }

    pub fn ends(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        (0..self.len()).map(|index| self.end(index).expect("validated end"))
    }

    /// First logical record whose coordinate-sorted start does not satisfy
    /// `predicate`.
    pub fn partition_point_start(&self, mut predicate: impl FnMut(u64) -> bool) -> usize {
        let (mut left, mut right) = (0_usize, self.len());
        while left < right {
            let middle = left + (right - left) / 2;
            if self.start(middle).is_some_and(&mut predicate) {
                left = middle.saturating_add(1);
            } else {
                right = middle;
            }
        }
        left
    }

    pub fn validate(&self) -> bool {
        let len = self.len();
        let storage_len = self.storage.starts.len();
        valid_alignment_storage(&self.storage)
            && self
                .physical_indices
                .as_ref()
                .is_none_or(|indices| indices.iter().all(|index| (*index as usize) < storage_len))
            && self.composite.as_ref().is_none_or(|composite| {
                composite
                    .storages
                    .iter()
                    .all(|storage| valid_alignment_storage(storage))
                    && composite.records.iter().all(|location| {
                        composite
                            .storages
                            .get(location.storage as usize)
                            .is_some_and(|storage| {
                                (location.record as usize) < storage.starts.len()
                            })
                    })
            })
            && (0..len).all(|index| self.sequence(index).is_some())
            && (0..len).all(|index| self.auxiliary_fields(index).is_ok())
            && self
                .filtered_record_spans
                .windows(2)
                .all(|pair| pair[0].start <= pair[1].start)
    }

    pub fn push(&mut self, record: &AlignmentRecord) -> Result<(), BatchOverflow> {
        self.push_with_optional_source_position(record, None)
    }

    /// Adds a decoded record together with its stable physical source position.
    pub fn push_with_source_position(
        &mut self,
        record: &AlignmentRecord,
        source_position: AlignmentSourcePosition,
    ) -> Result<(), BatchOverflow> {
        self.push_with_optional_source_position(record, Some(source_position))
    }

    fn push_with_optional_source_position(
        &mut self,
        record: &AlignmentRecord,
        source_position: Option<AlignmentSourcePosition>,
    ) -> Result<(), BatchOverflow> {
        let storage = self.storage_mut()?;
        let read_name_end = arena_end(storage.read_names.len(), record.read_name.len())?;
        let cigar_end = arena_end(storage.cigar_ops.len(), record.cigar.len())?;
        let (sequence_state, bases, qualities) = match &record.sequence {
            SequenceData::NotLoaded => (PackedSequenceState::NotLoaded, &[][..], None),
            SequenceData::Missing => (PackedSequenceState::Missing, &[][..], None),
            SequenceData::Present { bases, qualities } => (
                if qualities.is_some() {
                    PackedSequenceState::PresentWithQualities
                } else {
                    PackedSequenceState::PresentWithoutQualities
                },
                bases.as_slice(),
                qualities.as_deref(),
            ),
        };
        let sequence_end = arena_end(storage.sequences.len(), bases.len())?;
        let quality_end = arena_end(storage.qualities.len(), qualities.map_or(0, <[u8]>::len))?;
        let auxiliary_bytes =
            record
                .auxiliary_fields
                .iter()
                .try_fold(0_usize, |total, field| {
                    total
                        .checked_add(encoded_auxiliary_len(field)?)
                        .ok_or(BatchOverflow)
                })?;
        let auxiliary_end = arena_end(storage.auxiliary_data.len(), auxiliary_bytes)?;
        for field in &record.auxiliary_fields {
            encode_auxiliary_field(&mut storage.auxiliary_data, field)?;
        }

        storage.contigs.push(record.contig);
        storage.starts.push(record.start);
        storage.ends.push(record.end);
        storage.flags.push(record.flags);
        storage.mapping_qualities.push(record.mapping_quality);
        storage.mate_contigs.push(record.mate_contig);
        storage.mate_starts.push(record.mate_start);
        storage.template_lengths.push(record.template_length);
        storage.read_names.extend_from_slice(&record.read_name);
        storage.read_name_offsets.push(read_name_end);
        storage.cigar_ops.extend_from_slice(&record.cigar);
        storage.cigar_offsets.push(cigar_end);
        storage.sequence_states.push(sequence_state);
        storage.sequences.extend_from_slice(bases);
        storage.sequence_offsets.push(sequence_end);
        if let Some(qualities) = qualities {
            storage.qualities.extend_from_slice(qualities);
        }
        storage.quality_offsets.push(quality_end);
        storage.auxiliary_offsets.push(auxiliary_end);
        let physical = storage.starts.len().checked_sub(1).ok_or(BatchOverflow)?;
        storage
            .stable_ids
            .push(alignment_storage_stable_id(storage, physical)?);
        storage.source_positions.push(source_position);
        Ok(())
    }

    /// Copies one packed record into this batch without materializing any of
    /// its variable-width fields.
    pub fn push_view(&mut self, record: AlignmentRecordView<'_>) -> Result<(), BatchOverflow> {
        let read_name = record.read_name();
        let cigar = record.cigar();
        let (sequence_state, bases, qualities) = match record.sequence() {
            SequenceView::NotLoaded => (PackedSequenceState::NotLoaded, &[][..], None),
            SequenceView::Missing => (PackedSequenceState::Missing, &[][..], None),
            SequenceView::Present { bases, qualities } => (
                if qualities.is_some() {
                    PackedSequenceState::PresentWithQualities
                } else {
                    PackedSequenceState::PresentWithoutQualities
                },
                bases,
                qualities,
            ),
        };
        let auxiliary = record.auxiliary_bytes().ok_or(BatchOverflow)?;
        let storage = self.storage_mut()?;
        let read_name_end = arena_end(storage.read_names.len(), read_name.len())?;
        let cigar_end = arena_end(storage.cigar_ops.len(), cigar.len())?;
        let sequence_end = arena_end(storage.sequences.len(), bases.len())?;
        let quality_end = arena_end(storage.qualities.len(), qualities.map_or(0, <[u8]>::len))?;
        let auxiliary_end = arena_end(storage.auxiliary_data.len(), auxiliary.len())?;

        storage.contigs.push(record.contig());
        storage.starts.push(record.start());
        storage.ends.push(record.end());
        storage.flags.push(record.flags());
        storage.mapping_qualities.push(record.mapping_quality());
        storage.mate_contigs.push(record.mate_contig());
        storage.mate_starts.push(record.mate_start());
        storage.template_lengths.push(record.template_length());
        storage.read_names.extend_from_slice(read_name);
        storage.read_name_offsets.push(read_name_end);
        storage.cigar_ops.extend_from_slice(cigar);
        storage.cigar_offsets.push(cigar_end);
        storage.sequence_states.push(sequence_state);
        storage.sequences.extend_from_slice(bases);
        storage.sequence_offsets.push(sequence_end);
        if let Some(qualities) = qualities {
            storage.qualities.extend_from_slice(qualities);
        }
        storage.quality_offsets.push(quality_end);
        storage.auxiliary_data.extend_from_slice(auxiliary);
        storage.auxiliary_offsets.push(auxiliary_end);
        let physical = storage.starts.len().checked_sub(1).ok_or(BatchOverflow)?;
        storage
            .stable_ids
            .push(alignment_storage_stable_id(storage, physical)?);
        storage.source_positions.push(record.source_position());
        Ok(())
    }

    /// Copies only immutable core columns, deliberately replacing all optional payload with the
    /// `NotLoaded` capability state. Stable IDs and physical source positions are preserved.
    pub fn copy_core(&self) -> Result<Self, BatchOverflow> {
        let mut core = Self::default();
        for record in self.records() {
            core.push_core_view(record)?;
        }
        core.filtered_record_spans = self.filtered_record_spans.clone();
        core.filtered_record_spans_complete = self.filtered_record_spans_complete;
        Ok(core)
    }

    /// True when no sequence, qualities, or auxiliary bytes are retained in this batch.
    pub fn is_core_only(&self) -> bool {
        (0..self.len()).all(|index| matches!(self.sequence(index), Some(SequenceView::NotLoaded)))
            && (0..self.len()).all(|index| {
                self.record_location(index).map(|(storage, physical)| {
                    arena_record_len(&storage.auxiliary_offsets, physical)
                }) == Some(0)
            })
    }

    fn push_core_view(&mut self, record: AlignmentRecordView<'_>) -> Result<(), BatchOverflow> {
        let read_name = record.read_name();
        let cigar = record.cigar();
        let storage = self.storage_mut()?;
        let read_name_end = arena_end(storage.read_names.len(), read_name.len())?;
        let cigar_end = arena_end(storage.cigar_ops.len(), cigar.len())?;
        let sequence_end = u32::try_from(storage.sequences.len()).map_err(|_| BatchOverflow)?;
        let quality_end = u32::try_from(storage.qualities.len()).map_err(|_| BatchOverflow)?;
        let auxiliary_end =
            u32::try_from(storage.auxiliary_data.len()).map_err(|_| BatchOverflow)?;

        storage.contigs.push(record.contig());
        storage.starts.push(record.start());
        storage.ends.push(record.end());
        storage.flags.push(record.flags());
        storage.mapping_qualities.push(record.mapping_quality());
        storage.mate_contigs.push(record.mate_contig());
        storage.mate_starts.push(record.mate_start());
        storage.template_lengths.push(record.template_length());
        storage.read_names.extend_from_slice(read_name);
        storage.read_name_offsets.push(read_name_end);
        storage.cigar_ops.extend_from_slice(cigar);
        storage.cigar_offsets.push(cigar_end);
        storage.sequence_states.push(PackedSequenceState::NotLoaded);
        storage.sequence_offsets.push(sequence_end);
        storage.quality_offsets.push(quality_end);
        storage.auxiliary_offsets.push(auxiliary_end);
        let physical = storage.starts.len().checked_sub(1).ok_or(BatchOverflow)?;
        storage
            .stable_ids
            .push(alignment_storage_stable_id(storage, physical)?);
        storage.source_positions.push(record.source_position());
        Ok(())
    }

    pub fn record_view(&self, index: usize) -> Option<AlignmentRecordView<'_>> {
        (index < self.len()).then_some(AlignmentRecordView { batch: self, index })
    }

    pub fn records(&self) -> impl ExactSizeIterator<Item = AlignmentRecordView<'_>> {
        (0..self.len()).map(|index| AlignmentRecordView { batch: self, index })
    }

    #[doc(hidden)]
    pub fn push_filtered_record(&mut self, start: u64, end: u64, reason: FilteredRecordReason) {
        if start < end {
            self.filtered_record_spans
                .push(FilteredRecordSpan { start, end, reason });
        }
    }

    #[doc(hidden)]
    pub fn mark_filtered_record_spans_incomplete(&mut self) {
        self.filtered_record_spans_complete = false;
    }

    pub fn filtered_record_spans_complete(&self) -> bool {
        self.filtered_record_spans_complete
    }

    #[doc(hidden)]
    pub fn sort_filtered_records(&mut self) {
        self.filtered_record_spans
            .sort_unstable_by_key(|record| record.start);
    }

    #[doc(hidden)]
    pub fn absorb_filtered_records(&mut self, mut source: Self) {
        self.filtered_record_spans
            .append(&mut source.filtered_record_spans);
        self.filtered_record_spans_complete &= source.filtered_record_spans_complete;
        self.sort_filtered_records();
    }

    pub fn filtered_record_count(&self) -> usize {
        self.filtered_record_spans.len()
    }

    /// Merges two complete start-sorted query batches and removes only the
    /// duplicate copies introduced by indexed overlap at the query boundary.
    /// Genuine byte-identical duplicate records are retained at the maximum
    /// multiplicity present in either input.
    pub fn merge_complete(&self, other: &Self) -> Result<Self, BatchOverflow> {
        if !self.filtered_record_spans_complete || !other.filtered_record_spans_complete {
            return Err(BatchOverflow);
        }
        let mut left_by_fingerprint = HashMap::<(u64, u64), Vec<usize>>::new();
        for record in self.records() {
            left_by_fingerprint
                .entry(record.stable_fingerprint())
                .or_default()
                .push(record.index);
        }
        let left_by_source_position = self
            .records()
            .map(|record| {
                record
                    .source_position()
                    .map(|position| (position, record.index))
            })
            .collect::<Option<HashMap<_, _>>>();
        let other_is_fully_positioned = other
            .records()
            .all(|record| record.source_position().is_some());
        let use_source_positions = left_by_source_position.is_some() && other_is_fully_positioned;
        let mut matched_left = HashSet::new();
        let mut storages = Vec::<Arc<AlignmentBatchStorage>>::new();
        for storage in self
            .backing_storages()
            .into_iter()
            .chain(other.backing_storages())
        {
            if !storages
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate, &storage))
            {
                storages.push(storage);
            }
        }
        let storage_slots = storages
            .iter()
            .enumerate()
            .map(|(index, storage)| {
                Ok((
                    Arc::as_ptr(storage) as usize,
                    u32::try_from(index).map_err(|_| BatchOverflow)?,
                ))
            })
            .collect::<Result<HashMap<_, _>, BatchOverflow>>()?;
        let mut merged_records = Vec::with_capacity(self.len().saturating_add(other.len()));
        let (mut left, mut right) = (0_usize, 0_usize);
        while left < self.len() || right < other.len() {
            let take_left = match (self.record_view(left), other.record_view(right)) {
                (Some(left), Some(right)) => left.primary_order() <= right.primary_order(),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_left {
                merged_records.push(self.composite_record_index(left, &storage_slots)?);
                left = left.saturating_add(1);
                continue;
            }
            let record = other.record_view(right).ok_or(BatchOverflow)?;
            let duplicate = if use_source_positions {
                let position = record.source_position().ok_or(BatchOverflow)?;
                let candidate = left_by_source_position
                    .as_ref()
                    .and_then(|positions| positions.get(&position))
                    .copied();
                if let Some(candidate) = candidate
                    && !self
                        .record_view(candidate)
                        .is_some_and(|left| left.exactly_matches(record))
                {
                    return Err(BatchOverflow);
                }
                candidate
            } else {
                left_by_fingerprint
                    .get(&record.stable_fingerprint())
                    .and_then(|candidates| {
                        candidates.iter().copied().find(|candidate| {
                            !matched_left.contains(candidate)
                                && self
                                    .record_view(*candidate)
                                    .is_some_and(|left| left.exactly_matches(record))
                        })
                    })
            };
            if let Some(duplicate) = duplicate {
                matched_left.insert(duplicate);
            } else {
                merged_records.push(other.composite_record_index(right, &storage_slots)?);
            }
            right = right.saturating_add(1);
        }
        let mut filtered_record_spans = self.filtered_record_spans.clone();
        let mut unmatched_left = self.filtered_record_spans.iter().copied().fold(
            HashMap::<FilteredRecordSpan, usize>::new(),
            |mut counts, record| {
                *counts.entry(record).or_default() += 1;
                counts
            },
        );
        for record in other.filtered_record_spans.iter().copied() {
            let retained_overlap = unmatched_left.get_mut(&record).is_some_and(|count| {
                if *count == 0 {
                    false
                } else {
                    *count -= 1;
                    true
                }
            });
            if !retained_overlap {
                filtered_record_spans.push(record);
            }
        }
        filtered_record_spans.sort_unstable_by_key(|record| record.start);
        let storage = storages.first().cloned().unwrap_or_default();
        Ok(Self {
            storage,
            physical_indices: None,
            composite: Some(Arc::new(CompositeAlignmentStorage {
                storages: storages.into(),
                records: merged_records.into(),
            })),
            filtered_record_spans,
            filtered_record_spans_complete: true,
        })
    }

    fn backing_storages(&self) -> Vec<Arc<AlignmentBatchStorage>> {
        self.composite.as_ref().map_or_else(
            || vec![Arc::clone(&self.storage)],
            |composite| composite.storages.iter().cloned().collect(),
        )
    }

    fn composite_record_index(
        &self,
        index: usize,
        storage_slots: &HashMap<usize, u32>,
    ) -> Result<CompositeRecordIndex, BatchOverflow> {
        let (storage, record) = if let Some(composite) = &self.composite {
            let location = composite.records.get(index).ok_or(BatchOverflow)?;
            (
                composite
                    .storages
                    .get(location.storage as usize)
                    .ok_or(BatchOverflow)?,
                location.record,
            )
        } else {
            (
                &self.storage,
                u32::try_from(self.physical_index(index).ok_or(BatchOverflow)?)
                    .map_err(|_| BatchOverflow)?,
            )
        };
        let storage = storage_slots
            .get(&(Arc::as_ptr(storage) as usize))
            .copied()
            .ok_or(BatchOverflow)?;
        Ok(CompositeRecordIndex { storage, record })
    }

    fn stable_fingerprint(&self, index: usize) -> (u64, u64) {
        let mut left = 0xcbf2_9ce4_8422_2325_u64;
        let mut right = 0x6c62_272e_07bb_0142_u64;
        let mut mix = |bytes: &[u8]| {
            for byte in bytes {
                left ^= u64::from(*byte);
                left = left.wrapping_mul(0x0000_0100_0000_01b3);
                right ^= u64::from(*byte);
                right = right.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(&self
            .contig(index)
            .expect("validated contig")
            .get()
            .to_le_bytes());
        mix(&self.start(index).expect("validated start").to_le_bytes());
        mix(&self.end(index).expect("validated end").to_le_bytes());
        mix(&self
            .flags(index)
            .expect("validated flags")
            .bits()
            .to_le_bytes());
        mix(&[self.mapping_quality(index).flatten().unwrap_or(u8::MAX)]);
        mix(&self
            .mate_contig(index)
            .flatten()
            .map_or(u32::MAX, ContigId::get)
            .to_le_bytes());
        mix(&self
            .mate_start(index)
            .flatten()
            .unwrap_or(u64::MAX)
            .to_le_bytes());
        mix(&self
            .template_length(index)
            .expect("validated template length")
            .to_le_bytes());
        mix(self.read_name(index).unwrap_or_default());
        for operation in self.cigar(index).unwrap_or_default() {
            mix(&[operation.kind as u8]);
            mix(&operation.len.to_le_bytes());
        }
        match self.sequence(index) {
            Some(SequenceView::NotLoaded) => mix(&[0]),
            Some(SequenceView::Missing) => mix(&[1]),
            Some(SequenceView::Present { bases, qualities }) => {
                mix(&[2]);
                mix(bases);
                mix(qualities.unwrap_or_default());
            }
            None => mix(&[u8::MAX]),
        }
        if let Some(auxiliary) = self.auxiliary_bytes(index) {
            mix(auxiliary);
        }
        (left, right)
    }

    fn records_exactly_match(&self, left: usize, other: &Self, right: usize) -> bool {
        self.contig(left) == other.contig(right)
            && self.start(left) == other.start(right)
            && self.end(left) == other.end(right)
            && self.flags(left) == other.flags(right)
            && self.mapping_quality(left) == other.mapping_quality(right)
            && self.mate_contig(left) == other.mate_contig(right)
            && self.mate_start(left) == other.mate_start(right)
            && self.template_length(left) == other.template_length(right)
            && self.read_name(left) == other.read_name(right)
            && self.cigar(left) == other.cigar(right)
            && self.sequence(left) == other.sequence(right)
            && self.auxiliary_bytes(left) == other.auxiliary_bytes(right)
    }

    #[doc(hidden)]
    pub fn copy_filtered_records_within(
        &self,
        interval: rgv_core::GenomicInterval,
        target: &mut Self,
    ) -> crate::QueryStats {
        let mut stats = crate::QueryStats::default();
        for record in self
            .filtered_record_spans
            .iter()
            .take_while(|record| record.start < interval.end())
            .filter(|record| record.end > interval.start())
        {
            stats.records_seen = stats.records_seen.saturating_add(1);
            match record.reason {
                FilteredRecordReason::Unmapped => {
                    stats.records_filtered_unmapped =
                        stats.records_filtered_unmapped.saturating_add(1);
                }
                FilteredRecordReason::Secondary => {
                    stats.records_filtered_secondary =
                        stats.records_filtered_secondary.saturating_add(1);
                }
                FilteredRecordReason::Supplementary => {
                    stats.records_filtered_supplementary =
                        stats.records_filtered_supplementary.saturating_add(1);
                }
                FilteredRecordReason::User {
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
                }
            }
            target.filtered_record_spans.push(*record);
        }
        target.filtered_record_spans_complete = self.filtered_record_spans_complete;
        stats
    }

    pub fn read_name(&self, index: usize) -> Option<&[u8]> {
        let (storage, physical) = self.record_location(index)?;
        arena_slice(&storage.read_name_offsets, &storage.read_names, physical)
    }

    pub fn cigar(&self, index: usize) -> Option<&[CigarOp]> {
        let (storage, physical) = self.record_location(index)?;
        arena_slice(&storage.cigar_offsets, &storage.cigar_ops, physical)
    }

    pub fn sequence(&self, index: usize) -> Option<SequenceView<'_>> {
        let (storage, physical) = self.record_location(index)?;
        let state = *storage.sequence_states.get(physical)?;
        let bases = arena_slice(&storage.sequence_offsets, &storage.sequences, physical)?;
        let qualities = arena_slice(&storage.quality_offsets, &storage.qualities, physical)?;
        match state {
            PackedSequenceState::NotLoaded if bases.is_empty() && qualities.is_empty() => {
                Some(SequenceView::NotLoaded)
            }
            PackedSequenceState::Missing if bases.is_empty() && qualities.is_empty() => {
                Some(SequenceView::Missing)
            }
            PackedSequenceState::PresentWithoutQualities if qualities.is_empty() => {
                Some(SequenceView::Present {
                    bases,
                    qualities: None,
                })
            }
            PackedSequenceState::PresentWithQualities if bases.len() == qualities.len() => {
                Some(SequenceView::Present {
                    bases,
                    qualities: Some(qualities),
                })
            }
            _ => None,
        }
    }

    /// Sequence capability is present for every physical record. Records for which the source
    /// itself has no sequence remain explicitly `Missing` and still satisfy it.
    #[must_use]
    pub fn has_complete_sequence_capability(&self) -> bool {
        (0..self.len()).all(|index| !matches!(self.sequence(index), Some(SequenceView::NotLoaded)))
    }

    /// Every present sequence retains qualities; records with no source sequence remain Missing.
    #[must_use]
    pub fn has_complete_quality_capability(&self) -> bool {
        (0..self.len()).all(|index| {
            matches!(
                self.sequence(index),
                Some(SequenceView::Missing)
                    | Some(SequenceView::Present {
                        qualities: Some(_),
                        ..
                    })
            )
        })
    }

    pub fn has_auxiliary_tag(&self, index: usize, tag: [u8; 2]) -> bool {
        let Some(data) = self.auxiliary_bytes(index) else {
            return false;
        };
        let mut cursor = 0;
        while let Ok(Some((field_tag, _, _))) = next_auxiliary(data, &mut cursor) {
            if field_tag == tag {
                return true;
            }
        }
        false
    }

    #[must_use]
    pub fn auxiliary_encoded_len(&self, index: usize) -> Option<usize> {
        self.auxiliary_bytes(index).map(<[u8]>::len)
    }

    pub fn auxiliary_value_payload_len(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<usize>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut cursor = 0;
        while let Some((field_tag, _, payload)) = next_auxiliary(data, &mut cursor)? {
            if field_tag == tag {
                return Ok(Some(payload.len()));
            }
        }
        Ok(None)
    }

    pub fn auxiliary_field(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<AuxField>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut cursor = 0;
        while let Some((field_tag, kind, payload)) = next_auxiliary(data, &mut cursor)? {
            if field_tag == tag {
                return Ok(Some(AuxField {
                    tag: field_tag,
                    value: decode_auxiliary_value(kind, payload)?,
                }));
            }
        }
        Ok(None)
    }

    pub fn auxiliary_fields(&self, index: usize) -> Result<Vec<AuxField>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut fields = Vec::new();
        let mut cursor = 0;
        while let Some((tag, kind, payload)) = next_auxiliary(data, &mut cursor)? {
            fields.push(AuxField {
                tag,
                value: decode_auxiliary_value(kind, payload)?,
            });
        }
        Ok(fields)
    }

    /// Stable digest of the packed auxiliary payload for identity
    /// disambiguation without materialising the record or exposing the
    /// private arena encoding.
    pub fn auxiliary_fingerprint(&self, index: usize) -> Option<(u64, u64)> {
        let data = self.auxiliary_bytes(index)?;
        let mix = |mut hash: u64| {
            for &byte in data {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash
        };
        Some((
            mix(0xcbf2_9ce4_8422_2325_u64),
            mix(0x6c62_272e_07bb_0142_u64),
        ))
    }

    fn auxiliary_bytes(&self, index: usize) -> Option<&[u8]> {
        let (storage, physical) = self.record_location(index)?;
        arena_slice(
            &storage.auxiliary_offsets,
            &storage.auxiliary_data,
            physical,
        )
    }

    pub fn materialize_record(&self, index: usize) -> Result<AlignmentRecord, PackedRecordError> {
        let contig = self
            .contig(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let sequence = match self
            .sequence(index)
            .ok_or(PackedRecordError::InvalidSequence(index))?
        {
            SequenceView::NotLoaded => SequenceData::NotLoaded,
            SequenceView::Missing => SequenceData::Missing,
            SequenceView::Present { bases, qualities } => SequenceData::Present {
                bases: bases.to_vec(),
                qualities: qualities.map(<[u8]>::to_vec),
            },
        };
        Ok(AlignmentRecord {
            contig,
            start: self
                .start(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            end: self
                .end(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            flags: self
                .flags(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            mapping_quality: self
                .mapping_quality(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            mate_contig: self
                .mate_contig(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            mate_start: self
                .mate_start(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            template_length: self
                .template_length(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?,
            read_name: self
                .read_name(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?
                .to_vec(),
            cigar: self
                .cigar(index)
                .ok_or(PackedRecordError::InvalidIndex(index))?
                .to_vec(),
            sequence,
            auxiliary_fields: self.auxiliary_fields(index)?,
        })
    }

    /// Memory retained by this batch, including packed storage shared by a projection.
    ///
    /// Cache admission must use this conservative weight unless its coordinator explicitly
    /// deduplicates Arc-backed storage identities across entries.
    pub fn estimated_bytes(&self) -> usize {
        self.storage_estimated_bytes()
            .saturating_add(self.projection_indices_bytes())
            .saturating_add(self.filtered_provenance_estimated_bytes())
    }

    /// Bytes newly allocated for this batch rather than retained through shared storage.
    pub fn incremental_estimated_bytes(&self) -> usize {
        let storage = (!self.is_projection())
            .then(|| self.storage_estimated_bytes())
            .unwrap_or_default();
        storage
            .saturating_add(self.projection_indices_bytes())
            .saturating_add(self.filtered_provenance_estimated_bytes())
    }

    /// Per-view bytes when packed storage is already charged to a resident scene.
    pub fn shared_view_estimated_bytes(&self) -> usize {
        self.projection_indices_bytes()
            .saturating_add(self.filtered_provenance_estimated_bytes())
    }

    /// Packed bytes attributable to the logical records in this batch.
    ///
    /// A zero-copy projection retains a potentially much larger immutable
    /// arena, so `estimated_bytes` remains the correct conservative cache
    /// ownership weight. Density forecasting for a *new* decoded interval,
    /// however, must use the selected records' fixed columns and variable
    /// slices rather than charging every unselected backing record.
    pub fn logical_decoded_estimated_bytes(&self) -> usize {
        const FIXED_RECORD_BYTES: usize = std::mem::size_of::<ContigId>()
            + std::mem::size_of::<u64>()
            + std::mem::size_of::<u64>()
            + std::mem::size_of::<AlignmentFlags>()
            + std::mem::size_of::<Option<u8>>()
            + std::mem::size_of::<Option<ContigId>>()
            + std::mem::size_of::<Option<u64>>()
            + std::mem::size_of::<i64>()
            + std::mem::size_of::<PackedSequenceState>()
            + std::mem::size_of::<AlignmentStableId>()
            + std::mem::size_of::<Option<AlignmentSourcePosition>>()
            + 5 * std::mem::size_of::<u32>();

        let records = (0..self.len()).fold(0_usize, |total, index| {
            let Some((storage, physical)) = self.record_location(index) else {
                return total;
            };
            let variable = arena_record_len(&storage.read_name_offsets, physical)
                .saturating_add(
                    arena_record_len(&storage.cigar_offsets, physical)
                        .saturating_mul(std::mem::size_of::<CigarOp>()),
                )
                .saturating_add(arena_record_len(&storage.sequence_offsets, physical))
                .saturating_add(arena_record_len(&storage.quality_offsets, physical))
                .saturating_add(arena_record_len(&storage.auxiliary_offsets, physical));
            total
                .saturating_add(FIXED_RECORD_BYTES)
                .saturating_add(variable)
        });
        records
            // Each packed arena owns one terminal offset even for an empty
            // batch. Amortize that constant exactly once per logical view.
            .saturating_add(5 * std::mem::size_of::<u32>())
            .saturating_add(self.filtered_provenance_estimated_bytes())
    }

    fn storage_estimated_bytes(&self) -> usize {
        self.composite.as_ref().map_or_else(
            || alignment_storage_estimated_bytes(&self.storage),
            |composite| {
                composite.storages.iter().fold(0_usize, |total, storage| {
                    total.saturating_add(alignment_storage_estimated_bytes(storage))
                })
            },
        )
    }

    fn filtered_provenance_estimated_bytes(&self) -> usize {
        self.filtered_record_spans.len() * std::mem::size_of::<FilteredRecordSpan>()
    }
}

impl AlignmentPayloadBatch {
    pub fn from_batch(
        batch: &AlignmentBatch,
        retain_sequence: bool,
        retain_qualities: bool,
        retained_auxiliary_tags: Option<&[[u8; 2]]>,
    ) -> Result<Self, AlignmentPayloadError> {
        if retain_qualities && !retain_sequence {
            return Err(AlignmentPayloadError::QualitiesRequireSequence);
        }
        let mut payload = Self::default();
        payload.sequence_capability_retained = retain_sequence;
        payload.quality_capability_retained = retain_qualities;
        payload.all_auxiliary_tags_retained = retained_auxiliary_tags.is_none();
        payload.retained_auxiliary_tags = retained_auxiliary_tags
            .map(|tags| {
                let mut tags = tags.to_vec();
                tags.sort_unstable();
                tags.dedup();
                Arc::<[[u8; 2]]>::from(tags)
            })
            .unwrap_or_else(|| Arc::from([]));
        payload.source_positions.reserve(batch.len());
        payload.sequence_states.reserve(batch.len());
        payload.sequence_offsets.reserve(batch.len());
        payload.quality_offsets.reserve(batch.len());
        payload.auxiliary_offsets.reserve(batch.len());
        for record in batch.records() {
            payload.source_positions.push(record.source_position());
            let (state, bases, qualities) = if retain_sequence {
                match record.sequence() {
                    SequenceView::NotLoaded => (PackedSequenceState::NotLoaded, &[][..], None),
                    SequenceView::Missing => (PackedSequenceState::Missing, &[][..], None),
                    SequenceView::Present { bases, qualities } => {
                        let qualities = retain_qualities.then_some(qualities).flatten();
                        (
                            if qualities.is_some() {
                                PackedSequenceState::PresentWithQualities
                            } else {
                                PackedSequenceState::PresentWithoutQualities
                            },
                            bases,
                            qualities,
                        )
                    }
                }
            } else {
                (PackedSequenceState::NotLoaded, &[][..], None)
            };
            let sequence_end = arena_end(payload.sequences.len(), bases.len())?;
            let quality_end = arena_end(payload.qualities.len(), qualities.map_or(0, <[u8]>::len))?;
            payload.sequence_states.push(state);
            payload.sequences.extend_from_slice(bases);
            payload.sequence_offsets.push(sequence_end);
            if let Some(qualities) = qualities {
                payload.qualities.extend_from_slice(qualities);
            }
            payload.quality_offsets.push(quality_end);

            let auxiliary = record
                .auxiliary_bytes()
                .ok_or(AlignmentPayloadError::InvalidRecord)?;
            match retained_auxiliary_tags {
                None => payload.auxiliary_data.extend_from_slice(auxiliary),
                Some(tags) => {
                    let mut cursor = 0;
                    while cursor < auxiliary.len() {
                        let field_start = cursor;
                        let Some((tag, _, _)) = next_auxiliary(auxiliary, &mut cursor)? else {
                            break;
                        };
                        if tags.contains(&tag) {
                            payload
                                .auxiliary_data
                                .extend_from_slice(&auxiliary[field_start..cursor]);
                        }
                    }
                }
            }
            payload
                .auxiliary_offsets
                .push(u32::try_from(payload.auxiliary_data.len())?);
        }
        debug_assert!(payload.validate());
        Ok(payload)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sequence_states.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sequence_states.is_empty()
    }

    #[must_use]
    pub fn source_position(&self, index: usize) -> Option<AlignmentSourcePosition> {
        self.source_positions.get(index).copied().flatten()
    }

    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<SequenceView<'_>> {
        match self.sequence_states.get(index)? {
            PackedSequenceState::NotLoaded => Some(SequenceView::NotLoaded),
            PackedSequenceState::Missing => Some(SequenceView::Missing),
            PackedSequenceState::PresentWithoutQualities => Some(SequenceView::Present {
                bases: arena_slice(&self.sequence_offsets, &self.sequences, index)?,
                qualities: None,
            }),
            PackedSequenceState::PresentWithQualities => {
                let bases = arena_slice(&self.sequence_offsets, &self.sequences, index)?;
                let qualities = arena_slice(&self.quality_offsets, &self.qualities, index)?;
                (bases.len() == qualities.len()).then_some(SequenceView::Present {
                    bases,
                    qualities: Some(qualities),
                })
            }
        }
    }

    /// The sidecar was decoded with sequence capability for every physical record. Records for
    /// which the source itself has no sequence remain explicitly `Missing` and still satisfy it.
    #[must_use]
    pub fn has_complete_sequence_capability(&self) -> bool {
        self.sequence_capability_retained
            && (0..self.len())
                .all(|index| !matches!(self.sequence(index), Some(SequenceView::NotLoaded)))
    }

    /// Every present sequence retains qualities; records with no source sequence remain Missing.
    #[must_use]
    pub fn has_complete_quality_capability(&self) -> bool {
        self.sequence_capability_retained && self.quality_capability_retained
    }

    /// Whether every auxiliary field was decoded, including source records which contain none.
    #[must_use]
    pub const fn has_all_auxiliary_tag_capability(&self) -> bool {
        self.all_auxiliary_tags_retained
    }

    /// Whether a requested tag was considered during decode. Absence from an individual record
    /// remains a truthful source value rather than evidence that the capability was omitted.
    #[must_use]
    pub fn has_auxiliary_tag_capability(&self, tag: [u8; 2]) -> bool {
        self.all_auxiliary_tags_retained || self.retained_auxiliary_tags.binary_search(&tag).is_ok()
    }

    pub fn has_auxiliary_tag(&self, index: usize, tag: [u8; 2]) -> bool {
        let Some(data) = self.auxiliary_bytes(index) else {
            return false;
        };
        let mut cursor = 0;
        while let Ok(Some((field_tag, _, _))) = next_auxiliary(data, &mut cursor) {
            if field_tag == tag {
                return true;
            }
        }
        false
    }

    #[must_use]
    pub fn auxiliary_encoded_len(&self, index: usize) -> Option<usize> {
        self.auxiliary_bytes(index).map(<[u8]>::len)
    }

    pub fn auxiliary_value_payload_len(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<usize>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut cursor = 0;
        while let Some((field_tag, _, payload)) = next_auxiliary(data, &mut cursor)? {
            if field_tag == tag {
                return Ok(Some(payload.len()));
            }
        }
        Ok(None)
    }

    pub fn auxiliary_field(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<AuxField>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut cursor = 0;
        while let Some((field_tag, kind, value)) = next_auxiliary(data, &mut cursor)? {
            if field_tag == tag {
                return Ok(Some(AuxField {
                    tag,
                    value: decode_auxiliary_value(kind, value)?,
                }));
            }
        }
        Ok(None)
    }

    pub fn auxiliary_fields(&self, index: usize) -> Result<Vec<AuxField>, PackedRecordError> {
        let data = self
            .auxiliary_bytes(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let mut fields = Vec::new();
        let mut cursor = 0;
        while let Some((tag, kind, value)) = next_auxiliary(data, &mut cursor)? {
            fields.push(AuxField {
                tag,
                value: decode_auxiliary_value(kind, value)?,
            });
        }
        Ok(fields)
    }

    #[must_use]
    pub fn validate(&self) -> bool {
        let len = self.len();
        self.source_positions.len() == len
            && valid_offsets(&self.sequence_offsets, len, self.sequences.len())
            && valid_offsets(&self.quality_offsets, len, self.qualities.len())
            && valid_offsets(&self.auxiliary_offsets, len, self.auxiliary_data.len())
            && (0..len).all(|index| self.sequence(index).is_some())
            && (0..len).all(|index| {
                let Some(data) = self.auxiliary_bytes(index) else {
                    return false;
                };
                let mut cursor = 0;
                loop {
                    match next_auxiliary(data, &mut cursor) {
                        Ok(Some(_)) => {}
                        Ok(None) => break true,
                        Err(_) => break false,
                    }
                }
            })
    }

    /// Capacity charged when this sidecar is retained independently from core records.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.source_positions
            .capacity()
            .saturating_mul(std::mem::size_of::<Option<AlignmentSourcePosition>>())
            .saturating_add(
                self.sequence_states
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PackedSequenceState>()),
            )
            .saturating_add(
                self.sequence_offsets
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(self.sequences.capacity())
            .saturating_add(
                self.quality_offsets
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(self.qualities.capacity())
            .saturating_add(
                self.auxiliary_offsets
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(self.auxiliary_data.capacity())
            .saturating_add(
                self.retained_auxiliary_tags
                    .len()
                    .saturating_mul(std::mem::size_of::<[u8; 2]>()),
            )
    }

    fn auxiliary_bytes(&self, index: usize) -> Option<&[u8]> {
        arena_slice(&self.auxiliary_offsets, &self.auxiliary_data, index)
    }
}

fn valid_offsets(offsets: &[u32], records: usize, arena_len: usize) -> bool {
    offsets.len() == records + 1
        && offsets.first() == Some(&0)
        && offsets.windows(2).all(|pair| pair[0] <= pair[1])
        && offsets.last().copied().map(|value| value as usize) == Some(arena_len)
}

fn valid_alignment_storage(storage: &AlignmentBatchStorage) -> bool {
    let len = storage.starts.len();
    storage.contigs.len() == len
        && storage.ends.len() == len
        && storage.flags.len() == len
        && storage.mapping_qualities.len() == len
        && storage.mate_contigs.len() == len
        && storage.mate_starts.len() == len
        && storage.template_lengths.len() == len
        && storage.stable_ids.len() == len
        && storage.source_positions.len() == len
        && storage.sequence_states.len() == len
        && valid_offsets(&storage.read_name_offsets, len, storage.read_names.len())
        && valid_offsets(&storage.cigar_offsets, len, storage.cigar_ops.len())
        && valid_offsets(&storage.sequence_offsets, len, storage.sequences.len())
        && valid_offsets(&storage.quality_offsets, len, storage.qualities.len())
        && valid_offsets(
            &storage.auxiliary_offsets,
            len,
            storage.auxiliary_data.len(),
        )
}

fn alignment_storage_estimated_bytes(storage: &AlignmentBatchStorage) -> usize {
    storage.contigs.len() * std::mem::size_of::<ContigId>()
        + storage.starts.len() * std::mem::size_of::<u64>()
        + storage.ends.len() * std::mem::size_of::<u64>()
        + storage.flags.len() * std::mem::size_of::<AlignmentFlags>()
        + storage.mapping_qualities.len() * std::mem::size_of::<Option<u8>>()
        + storage.mate_contigs.len() * std::mem::size_of::<Option<ContigId>>()
        + storage.mate_starts.len() * std::mem::size_of::<Option<u64>>()
        + storage.template_lengths.len() * std::mem::size_of::<i64>()
        + storage.read_name_offsets.len() * std::mem::size_of::<u32>()
        + storage.read_names.len()
        + storage.cigar_offsets.len() * std::mem::size_of::<u32>()
        + storage.cigar_ops.len() * std::mem::size_of::<CigarOp>()
        + storage.sequence_states.len() * std::mem::size_of::<PackedSequenceState>()
        + storage.sequence_offsets.len() * std::mem::size_of::<u32>()
        + storage.sequences.len()
        + storage.quality_offsets.len() * std::mem::size_of::<u32>()
        + storage.qualities.len()
        + storage.auxiliary_offsets.len() * std::mem::size_of::<u32>()
        + storage.auxiliary_data.len()
        + storage.stable_ids.len() * std::mem::size_of::<AlignmentStableId>()
        + storage.source_positions.len() * std::mem::size_of::<Option<AlignmentSourcePosition>>()
}

fn arena_record_len(offsets: &[u32], index: usize) -> usize {
    offsets
        .get(index)
        .zip(offsets.get(index.saturating_add(1)))
        .map_or(0, |(start, end)| end.saturating_sub(*start) as usize)
}

fn alignment_storage_stable_id(
    storage: &AlignmentBatchStorage,
    index: usize,
) -> Result<AlignmentStableId, BatchOverflow> {
    let mut primary = 0xcbf2_9ce4_8422_2325_u64;
    let mut secondary = 0x6c62_272e_07bb_0142_u64;
    let contig = storage.contigs.get(index).ok_or(BatchOverflow)?.get();
    let start = *storage.starts.get(index).ok_or(BatchOverflow)?;
    let end = *storage.ends.get(index).ok_or(BatchOverflow)?;
    let flags = storage.flags.get(index).ok_or(BatchOverflow)?.bits();
    let mapping_quality = storage
        .mapping_qualities
        .get(index)
        .ok_or(BatchOverflow)?
        .unwrap_or(u8::MAX);
    let mate_contig = storage
        .mate_contigs
        .get(index)
        .ok_or(BatchOverflow)?
        .map_or(u32::MAX, ContigId::get);
    let mate_start = storage
        .mate_starts
        .get(index)
        .ok_or(BatchOverflow)?
        .unwrap_or(u64::MAX);
    let template_length = *storage.template_lengths.get(index).ok_or(BatchOverflow)?;
    let read_name =
        arena_slice(&storage.read_name_offsets, &storage.read_names, index).ok_or(BatchOverflow)?;
    for bytes in [
        contig.to_le_bytes().as_slice(),
        start.to_le_bytes().as_slice(),
        end.to_le_bytes().as_slice(),
        flags.to_le_bytes().as_slice(),
        mapping_quality.to_le_bytes().as_slice(),
        mate_contig.to_le_bytes().as_slice(),
        mate_start.to_le_bytes().as_slice(),
        template_length.to_le_bytes().as_slice(),
        read_name,
    ] {
        primary = stable_id_fnv_mix(primary, bytes);
        secondary = stable_id_fnv_mix(secondary, bytes);
    }
    let cigar =
        arena_slice(&storage.cigar_offsets, &storage.cigar_ops, index).ok_or(BatchOverflow)?;
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
        primary = stable_id_fnv_mix(primary, &[kind]);
        primary = stable_id_fnv_mix(primary, &operation.len.to_le_bytes());
        secondary = stable_id_fnv_mix(secondary, &[kind]);
        secondary = stable_id_fnv_mix(secondary, &operation.len.to_le_bytes());
    }
    Ok(AlignmentStableId { primary, secondary })
}

fn stable_id_fnv_mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn arena_end(current: usize, added: usize) -> Result<u32, BatchOverflow> {
    current
        .checked_add(added)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(BatchOverflow)
}

fn arena_slice<'a, T>(offsets: &[u32], arena: &'a [T], index: usize) -> Option<&'a [T]> {
    let start = usize::try_from(*offsets.get(index)?).ok()?;
    let end = usize::try_from(*offsets.get(index + 1)?).ok()?;
    arena.get(start..end)
}

const AUX_CHARACTER: u8 = 0;
const AUX_SIGNED: u8 = 1;
const AUX_UNSIGNED: u8 = 2;
const AUX_FLOAT: u8 = 3;
const AUX_DOUBLE: u8 = 4;
const AUX_STRING: u8 = 5;
const AUX_HEX: u8 = 6;
const AUX_SIGNED_ARRAY: u8 = 7;
const AUX_UNSIGNED_ARRAY: u8 = 8;
const AUX_FLOAT_ARRAY: u8 = 9;
const AUX_HEADER_BYTES: usize = 7;
type PackedAuxiliary<'a> = ([u8; 2], u8, &'a [u8]);

fn encoded_auxiliary_len(field: &AuxField) -> Result<usize, BatchOverflow> {
    let payload_bytes = match &field.value {
        AuxValue::Character(_) => 1,
        AuxValue::Signed(_) | AuxValue::Unsigned(_) | AuxValue::Double(_) => 8,
        AuxValue::Float(_) => 4,
        AuxValue::String(value) | AuxValue::HexByteArray(value) => value.len(),
        AuxValue::SignedArray(values) => values.len().checked_mul(8).ok_or(BatchOverflow)?,
        AuxValue::UnsignedArray(values) => values.len().checked_mul(8).ok_or(BatchOverflow)?,
        AuxValue::FloatArray(values) => values.len().checked_mul(4).ok_or(BatchOverflow)?,
    };
    AUX_HEADER_BYTES
        .checked_add(payload_bytes)
        .ok_or(BatchOverflow)
}

fn encode_auxiliary_field(data: &mut Vec<u8>, field: &AuxField) -> Result<(), BatchOverflow> {
    let (kind, payload_bytes) = match &field.value {
        AuxValue::Character(_) => (AUX_CHARACTER, 1),
        AuxValue::Signed(_) => (AUX_SIGNED, 8),
        AuxValue::Unsigned(_) => (AUX_UNSIGNED, 8),
        AuxValue::Float(_) => (AUX_FLOAT, 4),
        AuxValue::Double(_) => (AUX_DOUBLE, 8),
        AuxValue::String(value) => (AUX_STRING, value.len()),
        AuxValue::HexByteArray(value) => (AUX_HEX, value.len()),
        AuxValue::SignedArray(values) => (
            AUX_SIGNED_ARRAY,
            values.len().checked_mul(8).ok_or(BatchOverflow)?,
        ),
        AuxValue::UnsignedArray(values) => (
            AUX_UNSIGNED_ARRAY,
            values.len().checked_mul(8).ok_or(BatchOverflow)?,
        ),
        AuxValue::FloatArray(values) => (
            AUX_FLOAT_ARRAY,
            values.len().checked_mul(4).ok_or(BatchOverflow)?,
        ),
    };
    let payload_len = u32::try_from(payload_bytes).map_err(|_| BatchOverflow)?;
    let _ = data
        .len()
        .checked_add(AUX_HEADER_BYTES)
        .and_then(|value| value.checked_add(payload_bytes))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(BatchOverflow)?;
    data.extend_from_slice(&field.tag);
    data.push(kind);
    data.extend_from_slice(&payload_len.to_le_bytes());
    match &field.value {
        AuxValue::Character(value) => data.push(*value),
        AuxValue::Signed(value) => data.extend_from_slice(&value.to_le_bytes()),
        AuxValue::Unsigned(value) => data.extend_from_slice(&value.to_le_bytes()),
        AuxValue::Float(value) => data.extend_from_slice(&value.to_le_bytes()),
        AuxValue::Double(value) => data.extend_from_slice(&value.to_le_bytes()),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => {
            data.extend_from_slice(value.as_bytes());
        }
        AuxValue::SignedArray(values) => {
            for value in values {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        AuxValue::UnsignedArray(values) => {
            for value in values {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        AuxValue::FloatArray(values) => {
            for value in values {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(())
}

fn next_auxiliary<'a>(
    data: &'a [u8],
    cursor: &mut usize,
) -> Result<Option<PackedAuxiliary<'a>>, PackedRecordError> {
    if *cursor == data.len() {
        return Ok(None);
    }
    let header_end = (*cursor).saturating_add(AUX_HEADER_BYTES);
    let header = data
        .get(*cursor..header_end)
        .ok_or(PackedRecordError::MalformedAuxiliaryData)?;
    let tag = [header[0], header[1]];
    let kind = header[2];
    let payload_len = u32::from_le_bytes(header[3..7].try_into().unwrap()) as usize;
    let payload_start = header_end;
    let payload_end = payload_start
        .checked_add(payload_len)
        .ok_or(PackedRecordError::MalformedAuxiliaryData)?;
    let payload = data
        .get(payload_start..payload_end)
        .ok_or(PackedRecordError::MalformedAuxiliaryData)?;
    *cursor = payload_end;
    Ok(Some((tag, kind, payload)))
}

fn decode_auxiliary_value(kind: u8, payload: &[u8]) -> Result<AuxValue, PackedRecordError> {
    match kind {
        AUX_CHARACTER => Ok(AuxValue::Character(
            *payload
                .first()
                .filter(|_| payload.len() == 1)
                .ok_or(PackedRecordError::MalformedAuxiliaryData)?,
        )),
        AUX_SIGNED => Ok(AuxValue::Signed(i64::from_le_bytes(exact_array(payload)?))),
        AUX_UNSIGNED => Ok(AuxValue::Unsigned(u64::from_le_bytes(exact_array(
            payload,
        )?))),
        AUX_FLOAT => Ok(AuxValue::Float(f32::from_le_bytes(exact_array(payload)?))),
        AUX_DOUBLE => Ok(AuxValue::Double(f64::from_le_bytes(exact_array(payload)?))),
        AUX_STRING => Ok(AuxValue::String(
            String::from_utf8(payload.to_vec())
                .map_err(|_| PackedRecordError::MalformedAuxiliaryData)?,
        )),
        AUX_HEX => Ok(AuxValue::HexByteArray(
            String::from_utf8(payload.to_vec())
                .map_err(|_| PackedRecordError::MalformedAuxiliaryData)?,
        )),
        AUX_SIGNED_ARRAY => decode_array(payload, i64::from_le_bytes).map(AuxValue::SignedArray),
        AUX_UNSIGNED_ARRAY => {
            decode_array(payload, u64::from_le_bytes).map(AuxValue::UnsignedArray)
        }
        AUX_FLOAT_ARRAY => decode_array(payload, f32::from_le_bytes).map(AuxValue::FloatArray),
        _ => Err(PackedRecordError::MalformedAuxiliaryData),
    }
}

fn exact_array<const N: usize>(payload: &[u8]) -> Result<[u8; N], PackedRecordError> {
    payload
        .try_into()
        .map_err(|_| PackedRecordError::MalformedAuxiliaryData)
}

fn decode_array<const N: usize, T>(
    payload: &[u8],
    decode: impl Fn([u8; N]) -> T,
) -> Result<Vec<T>, PackedRecordError> {
    if !payload.len().is_multiple_of(N) {
        return Err(PackedRecordError::MalformedAuxiliaryData);
    }
    payload
        .chunks_exact(N)
        .map(|chunk| exact_array(chunk).map(&decode))
        .collect()
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AlignmentPayloadError {
    #[error("alignment payload capacity exceeded its packed 32-bit offsets")]
    Overflow,
    #[error("qualities cannot be retained without retaining sequence")]
    QualitiesRequireSequence,
    #[error("alignment payload source record is internally inconsistent")]
    InvalidRecord,
    #[error(transparent)]
    Packed(#[from] PackedRecordError),
}

impl From<BatchOverflow> for AlignmentPayloadError {
    fn from(_: BatchOverflow) -> Self {
        Self::Overflow
    }
}

impl From<std::num::TryFromIntError> for AlignmentPayloadError {
    fn from(_: std::num::TryFromIntError) -> Self {
        Self::Overflow
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchOverflow;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PackedRecordError {
    #[error("alignment index {0} is outside the packed batch")]
    InvalidIndex(usize),
    #[error("alignment {0} has inconsistent packed sequence or quality data")]
    InvalidSequence(usize),
    #[error("packed auxiliary data is malformed")]
    MalformedAuxiliaryData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_base_classification_reconciles_explicit_cigar_assertions() {
        use AlignedBaseClassification::{
            DeclaredSubstitution, EncodingConflict, Match, Substitution, Unknown,
        };

        for (kind, observed, reference, expected) in [
            (
                CigarOpKind::SequenceMismatch,
                Some(b'G'),
                Some(b'C'),
                Substitution,
            ),
            (
                CigarOpKind::SequenceMismatch,
                Some(b'c'),
                Some(b'C'),
                EncodingConflict,
            ),
            (
                CigarOpKind::SequenceMismatch,
                Some(b'C'),
                None,
                DeclaredSubstitution,
            ),
            (CigarOpKind::SequenceMatch, Some(b'C'), Some(b'c'), Match),
            (
                CigarOpKind::SequenceMatch,
                Some(b'G'),
                Some(b'C'),
                EncodingConflict,
            ),
            (CigarOpKind::SequenceMatch, Some(b'C'), None, Match),
            (CigarOpKind::Match, Some(b'C'), Some(b'C'), Match),
            (CigarOpKind::Match, Some(b'G'), Some(b'C'), Substitution),
            (CigarOpKind::Match, Some(b'C'), None, Unknown),
            (CigarOpKind::Insertion, Some(b'G'), Some(b'C'), Unknown),
        ] {
            assert_eq!(classify_aligned_base(kind, observed, reference), expected);
        }
    }

    #[test]
    fn alignment_header_preserves_bounded_ultima_read_group_metadata() {
        let metadata = alignment_header_metadata_from_sam(
            b"@HD\tVN:1.6\n@RG\tID:ultima-a\tSM:sample\tPL:ULTIMA\tmc:12\ttp:synthesis\n@RG\tID:other\tPL:ILLUMINA\n",
        );
        assert_eq!(metadata.read_groups.len(), 2);
        assert_eq!(metadata.read_groups_omitted, 0);
        let ultima = metadata.read_group(b"ultima-a").unwrap();
        assert_eq!(ultima.platform.as_deref(), Some("ULTIMA"));
        assert_eq!(ultima.maximum_homopolymer_length, Some(12));
        assert_eq!(
            ultima.ultima_tag_orientation,
            Some(UltimaTagOrientation::Synthesis)
        );
        assert!(ultima.is_ultima());
        assert!(!metadata.read_group(b"other").unwrap().is_ultima());
    }

    fn complete_record() -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(2),
            start: 10,
            end: 20,
            flags: AlignmentFlags::from_bits(0x11),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(2)),
            mate_start: Some(42),
            template_length: -32,
            read_name: b"read-1".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 10,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGTACGTAA".to_vec(),
                qualities: Some(vec![30; 10]),
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"RG",
                    value: AuxValue::String("sample-a".to_owned()),
                },
                AuxField {
                    tag: *b"NM",
                    value: AuxValue::Unsigned(3),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(vec![1, 127, 255]),
                },
            ],
        }
    }

    #[test]
    fn flags_retain_distinct_alignment_states() {
        let flags = AlignmentFlags::from_bits(0x10 | 0x100 | 0x800);
        assert!(flags.is_reverse());
        assert!(flags.is_secondary());
        assert!(flags.is_supplementary());
    }

    #[test]
    fn empty_batch_is_valid() {
        assert!(AlignmentBatch::default().validate());
    }

    #[test]
    fn complete_record_round_trips_through_packed_arenas() {
        let record = complete_record();
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        assert!(batch.validate());
        assert_eq!(batch.read_name(0), Some(b"read-1".as_slice()));
        assert_eq!(batch.cigar(0), Some(record.cigar.as_slice()));
        assert_eq!(
            batch.sequence(0),
            Some(SequenceView::Present {
                bases: b"ACGTACGTAA",
                qualities: Some(&[30; 10]),
            })
        );
        assert!(batch.has_auxiliary_tag(0, *b"RG"));
        assert!(!batch.has_auxiliary_tag(0, *b"HP"));
        assert_eq!(
            batch.auxiliary_field(0, *b"NM").unwrap(),
            Some(AuxField {
                tag: *b"NM",
                value: AuxValue::Unsigned(3),
            })
        );
        assert_eq!(batch.materialize_record(0).unwrap(), record);
    }

    #[test]
    fn core_and_optional_payload_are_independent_allocations_with_the_same_identity() {
        let mut batch = AlignmentBatch::default();
        batch
            .push_with_source_position(
                &complete_record(),
                AlignmentSourcePosition::bam_virtual_offset(123),
            )
            .unwrap();

        let core = batch.copy_core().unwrap();
        assert!(core.is_core_only());
        assert_eq!(core.stable_id(0), batch.stable_id(0));
        assert_eq!(core.source_position(0), batch.source_position(0));
        assert_eq!(core.sequence(0), Some(SequenceView::NotLoaded));
        assert!(!core.has_auxiliary_tag(0, *b"RG"));

        let full = AlignmentPayloadBatch::from_batch(&batch, true, true, None).unwrap();
        assert!(full.validate());
        assert!(full.has_complete_sequence_capability());
        assert!(full.has_complete_quality_capability());
        assert!(full.has_all_auxiliary_tag_capability());
        assert_eq!(full.source_position(0), batch.source_position(0));
        assert_eq!(full.sequence(0), batch.sequence(0));
        assert_eq!(
            full.auxiliary_field(0, *b"NM").unwrap(),
            batch.auxiliary_field(0, *b"NM").unwrap()
        );
        assert!(full.estimated_bytes() > 0);

        let selective =
            AlignmentPayloadBatch::from_batch(&batch, true, false, Some(&[*b"RG"])).unwrap();
        assert!(selective.has_complete_sequence_capability());
        assert!(!selective.has_complete_quality_capability());
        assert!(!selective.has_all_auxiliary_tag_capability());
        assert!(selective.has_auxiliary_tag_capability(*b"RG"));
        assert!(!selective.has_auxiliary_tag_capability(*b"NM"));
        assert_eq!(
            selective.sequence(0),
            Some(SequenceView::Present {
                bases: b"ACGTACGTAA",
                qualities: None,
            })
        );
        assert!(selective.has_auxiliary_tag(0, *b"RG"));
        assert!(!selective.has_auxiliary_tag(0, *b"NM"));
        assert_eq!(
            AlignmentPayloadBatch::from_batch(&batch, false, true, None).unwrap_err(),
            AlignmentPayloadError::QualitiesRequireSequence
        );
    }

    #[test]
    fn retained_quality_capability_preserves_truthful_source_missingness() {
        let mut record = complete_record();
        record.sequence = SequenceData::Present {
            bases: b"ACGTACGTAA".to_vec(),
            qualities: None,
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();

        let payload = AlignmentPayloadBatch::from_batch(&batch, true, true, None).unwrap();

        assert!(payload.has_complete_quality_capability());
        assert_eq!(
            payload.sequence(0),
            Some(SequenceView::Present {
                bases: b"ACGTACGTAA",
                qualities: None,
            })
        );
    }

    #[test]
    fn projection_shares_packed_arenas_and_preserves_logical_record_order() {
        let mut first = complete_record();
        first.read_name = b"first".to_vec();
        first.start = 10;
        first.end = 20;
        let mut second = complete_record();
        second.read_name = b"second-with-a-longer-name".to_vec();
        second.start = 30;
        second.end = 40;
        let mut third = complete_record();
        third.read_name = b"third".to_vec();
        third.start = 50;
        third.end = 60;

        let mut resident = AlignmentBatch::default();
        resident.push(&first).unwrap();
        resident.push(&second).unwrap();
        resident.push(&third).unwrap();

        let mut projection = resident.project(&[2, 0]).unwrap();

        assert!(projection.is_projection());
        assert!(projection.validate());
        assert_eq!(projection.projection_indices_bytes(), 2 * size_of::<u32>());
        assert_eq!(
            projection.incremental_estimated_bytes(),
            2 * size_of::<u32>()
        );
        assert_eq!(
            projection.estimated_bytes(),
            resident.estimated_bytes() + 2 * size_of::<u32>()
        );
        assert_eq!(projection.starts().collect::<Vec<_>>(), vec![50, 10]);
        assert_eq!(projection.read_name(0), Some(b"third".as_slice()));
        assert_eq!(projection.read_name(1), Some(b"first".as_slice()));
        assert_eq!(projection.materialize_record(0).unwrap(), third);
        assert_eq!(projection.materialize_record(1).unwrap(), first);
        let mut independently_decoded = AlignmentBatch::default();
        independently_decoded.push(&third).unwrap();
        independently_decoded.push(&first).unwrap();
        assert_eq!(
            projection.logical_decoded_estimated_bytes(),
            independently_decoded.logical_decoded_estimated_bytes()
        );
        assert!(projection.logical_decoded_estimated_bytes() < resident.estimated_bytes());
        assert!(projection.push(&second).is_err());
        assert_eq!(projection.len(), 2);
        assert_eq!(resident.len(), 3);
    }

    #[test]
    fn complete_batch_merge_deduplicates_boundary_copies_not_genuine_duplicates() {
        let duplicate = complete_record();
        let mut left_tail = complete_record();
        left_tail.start = 30;
        left_tail.end = 40;
        left_tail.read_name = b"left-tail".to_vec();
        let mut right_unique = complete_record();
        right_unique.start = 15;
        right_unique.end = 25;
        right_unique.read_name = b"right-unique".to_vec();

        let mut left = AlignmentBatch::default();
        left.push(&duplicate).unwrap();
        left.push(&duplicate).unwrap();
        left.push(&left_tail).unwrap();
        let mut right = AlignmentBatch::default();
        right.push(&duplicate).unwrap();
        right.push(&duplicate).unwrap();
        right.push(&right_unique).unwrap();

        let merged = left.merge_complete(&right).unwrap();
        assert!(merged.validate());
        assert!(merged.is_projection());
        assert_eq!(merged.len(), 4);
        assert_eq!(
            merged.incremental_estimated_bytes(),
            4 * size_of::<CompositeRecordIndex>()
        );
        assert_eq!(
            merged.estimated_bytes(),
            left.estimated_bytes()
                + right.estimated_bytes()
                + 4 * size_of::<CompositeRecordIndex>()
        );
        assert_eq!(
            merged
                .records()
                .filter(|record| record.read_name() == b"read-1")
                .count(),
            2
        );
        assert_eq!(merged.starts().collect::<Vec<_>>(), vec![10, 10, 15, 30]);
        let projected = merged.project(&[3, 0]).unwrap();
        assert!(projected.validate());
        assert_eq!(projected.starts().collect::<Vec<_>>(), vec![30, 10]);
        assert_eq!(
            projected.incremental_estimated_bytes(),
            2 * size_of::<CompositeRecordIndex>()
        );
        assert_eq!(projected.materialize_record(0).unwrap(), left_tail);
    }

    #[test]
    fn positioned_merge_uses_physical_identity_not_duplicate_multiplicity() {
        let duplicate = complete_record();
        let first = AlignmentSourcePosition::bam_virtual_offset(100);
        let second = AlignmentSourcePosition::bam_virtual_offset(200);
        let third = AlignmentSourcePosition::bam_virtual_offset(300);

        let mut left = AlignmentBatch::default();
        left.push_with_source_position(&duplicate, first).unwrap();
        left.push_with_source_position(&duplicate, second).unwrap();
        let mut right = AlignmentBatch::default();
        right.push_with_source_position(&duplicate, second).unwrap();
        right.push_with_source_position(&duplicate, third).unwrap();

        let merged = left.merge_complete(&right).unwrap();
        let positions = (0..merged.len())
            .map(|index| merged.source_position(index).unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(merged.len(), 3);
        assert_eq!(positions, HashSet::from([first, second, third]));
    }

    #[test]
    fn complete_batch_merge_preserves_exact_filtered_provenance_without_boundary_copies() {
        let duplicate = FilteredRecordReason::User {
            duplicate: true,
            qc_failed: false,
        };
        let mut left = AlignmentBatch::default();
        left.push_filtered_record(10, 20, duplicate);
        left.push_filtered_record(10, 20, duplicate);
        left.push_filtered_record(30, 40, FilteredRecordReason::Secondary);
        left.sort_filtered_records();
        let mut right = AlignmentBatch::default();
        right.push_filtered_record(10, 20, duplicate);
        right.push_filtered_record(10, 20, duplicate);
        right.push_filtered_record(50, 60, FilteredRecordReason::Supplementary);
        right.sort_filtered_records();

        let merged = left.merge_complete(&right).unwrap();

        assert!(merged.filtered_record_spans_complete());
        assert_eq!(merged.filtered_record_count(), 4);
        let mut target = AlignmentBatch::default();
        let stats = merged.copy_filtered_records_within(
            rgv_core::GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
            &mut target,
        );
        assert_eq!(stats.records_filtered_user, 2);
        assert_eq!(stats.records_filtered_secondary, 1);
        assert_eq!(stats.records_filtered_supplementary, 1);
    }

    #[test]
    fn overlap_index_keeps_a_long_record_crossing_earlier_blocks() {
        let mut batch = AlignmentBatch::default();
        let mut long = complete_record();
        long.start = 0;
        long.end = 1_000;
        long.read_name = b"long-crossing-read".to_vec();
        batch.push(&long).unwrap();
        for position in 1_u64..600 {
            let mut short = complete_record();
            short.start = position;
            short.end = position.saturating_add(1);
            short.read_name = format!("short-{position}").into_bytes();
            batch.push(&short).unwrap();
        }
        assert!(batch.validate());

        let interval = rgv_core::GenomicInterval::new(ContigId::new(2), 900, 901).unwrap();
        let index = AlignmentOverlapIndex::new(&batch);
        let selected = index.select(&batch, interval);
        assert_eq!(selected, vec![0]);
        assert_eq!(index.count(&batch, interval), selected.len());
    }

    #[test]
    fn sequence_states_remain_distinct() {
        let mut not_loaded = complete_record();
        not_loaded.sequence = SequenceData::NotLoaded;
        let mut missing = complete_record();
        missing.sequence = SequenceData::Missing;
        let mut no_quality = complete_record();
        no_quality.sequence = SequenceData::Present {
            bases: b"AC".to_vec(),
            qualities: None,
        };
        let mut batch = AlignmentBatch::default();
        batch.push(&not_loaded).unwrap();
        batch.push(&missing).unwrap();
        batch.push(&no_quality).unwrap();
        assert_eq!(batch.sequence(0), Some(SequenceView::NotLoaded));
        assert_eq!(batch.sequence(1), Some(SequenceView::Missing));
        assert_eq!(
            batch.sequence(2),
            Some(SequenceView::Present {
                bases: b"AC",
                qualities: None,
            })
        );
        assert!(batch.validate());
    }

    #[test]
    fn all_auxiliary_value_kinds_round_trip() {
        let mut record = complete_record();
        record.auxiliary_fields = vec![
            AuxField {
                tag: *b"A1",
                value: AuxValue::Character(b'Z'),
            },
            AuxField {
                tag: *b"A2",
                value: AuxValue::Signed(-7),
            },
            AuxField {
                tag: *b"A3",
                value: AuxValue::Unsigned(7),
            },
            AuxField {
                tag: *b"A4",
                value: AuxValue::Float(1.25),
            },
            AuxField {
                tag: *b"A5",
                value: AuxValue::Double(2.5),
            },
            AuxField {
                tag: *b"A6",
                value: AuxValue::String("value".to_owned()),
            },
            AuxField {
                tag: *b"A7",
                value: AuxValue::HexByteArray("CAFE".to_owned()),
            },
            AuxField {
                tag: *b"A8",
                value: AuxValue::SignedArray(vec![-1, 2]),
            },
            AuxField {
                tag: *b"A9",
                value: AuxValue::UnsignedArray(vec![1, 2]),
            },
            AuxField {
                tag: *b"B0",
                value: AuxValue::FloatArray(vec![1.0, 2.0]),
            },
        ];
        let mut batch = AlignmentBatch::default();
        batch.push(&record).unwrap();
        assert_eq!(batch.auxiliary_fields(0).unwrap(), record.auxiliary_fields);
        assert_eq!(batch.materialize_record(0).unwrap(), record);
    }

    #[test]
    fn columnar_packing_round_trips_variable_records() {
        let mut state = 0xd1b5_4a32_d192_ed03_u64;
        let mut expected = Vec::new();
        let mut batch = AlignmentBatch::default();
        for index in 0..2_048_u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let name_length = (state as usize % 47) + 1;
            let cigar_length = (state.rotate_left(19) as usize % 23) + 1;
            let read_name = (0..name_length)
                .map(|offset| b'A' + ((index as usize + offset) % 26) as u8)
                .collect::<Vec<_>>();
            let cigar = (0..cigar_length)
                .map(|offset| CigarOp {
                    kind: match offset % 9 {
                        0 => CigarOpKind::Match,
                        1 => CigarOpKind::Insertion,
                        2 => CigarOpKind::Deletion,
                        3 => CigarOpKind::ReferenceSkip,
                        4 => CigarOpKind::SoftClip,
                        5 => CigarOpKind::HardClip,
                        6 => CigarOpKind::Padding,
                        7 => CigarOpKind::SequenceMatch,
                        _ => CigarOpKind::SequenceMismatch,
                    },
                    len: 1 + (state.rotate_left(offset as u32) as u32 % 100),
                })
                .collect::<Vec<_>>();
            let record = AlignmentRecord {
                contig: ContigId::new((index % 3) as u32),
                start: index * 10,
                end: index * 10 + 100,
                flags: AlignmentFlags::from_bits(index as u16),
                mapping_quality: u8::try_from(index % 61).ok(),
                mate_contig: Some(ContigId::new((index % 3) as u32)),
                mate_start: Some(index * 10 + 200),
                template_length: i64::try_from(index).unwrap() - 1_024,
                read_name,
                cigar,
                sequence: SequenceData::Present {
                    bases: vec![b'A'; name_length],
                    qualities: (index % 2 == 0).then(|| vec![30; name_length]),
                },
                auxiliary_fields: vec![AuxField {
                    tag: *b"HP",
                    value: AuxValue::Unsigned(index % 3),
                }],
            };
            batch.push(&record).unwrap();
            expected.push(record);
        }
        assert!(batch.validate());
        assert_eq!(batch.len(), expected.len());
        let unpacked_bytes = expected.iter().fold(0_usize, |total, record| {
            let sequence_bytes = match &record.sequence {
                SequenceData::Present { bases, qualities } => {
                    bases.len() + qualities.as_ref().map_or(0, Vec::len)
                }
                SequenceData::NotLoaded | SequenceData::Missing => 0,
            };
            total
                + std::mem::size_of::<AlignmentRecord>()
                + record.read_name.len()
                + record.cigar.len() * std::mem::size_of::<CigarOp>()
                + sequence_bytes
                + record.auxiliary_fields.len() * std::mem::size_of::<AuxField>()
        });
        assert!(batch.estimated_bytes() < unpacked_bytes);
        for (index, record) in expected.iter().enumerate() {
            assert_eq!(batch.materialize_record(index).unwrap(), *record);
        }
        assert!(batch.materialize_record(expected.len()).is_err());
    }

    #[test]
    fn catalogue_assembly_resolution_requires_length_and_md5_fingerprints() {
        let header = b"@SQ\tSN:chr6\tLN:170805979\tM5:5691468a67c7e7a7b5f2a3a683792c29\n@SQ\tSN:chr20\tLN:64444167\tM5:b18e6c531b0bd70e949a7fc20859cb01\n@SQ\tSN:chrM\tLN:16569\tM5:c68f52674c9fb33aef52dcf399755519\n";
        let catalogue = rgv_core::GenomeLandmarkCatalogue {
            schema_version: rgv_core::LANDMARK_CATALOGUE_SCHEMA_VERSION,
            bundles: vec![rgv_core::GenomeLandmarkBundle {
                id: "hg38-test".to_owned(),
                version: "1".to_owned(),
                display_name: "GRCh38 test".to_owned(),
                provider: "test".to_owned(),
                release: "test".to_owned(),
                selection_policy: "test".to_owned(),
                assembly: rgv_core::LandmarkAssemblyIdentity {
                    stable_id: "GRCh38.no_alt_analysis_set".to_owned(),
                    display_name: "GRCh38".to_owned(),
                    compatible_ids: vec!["hg38".to_owned()],
                    primary_contig_count: 3,
                    dictionary_sha256: "0".repeat(64),
                    compatible_dictionary_sha256: Vec::new(),
                    contigs: vec![
                        ("chr6", 170_805_979, "5691468a67c7e7a7b5f2a3a683792c29"),
                        ("chr20", 64_444_167, "b18e6c531b0bd70e949a7fc20859cb01"),
                        ("chrM", 16_569, "c68f52674c9fb33aef52dcf399755519"),
                    ]
                    .into_iter()
                    .map(|(name, length, md5)| rgv_core::LandmarkContigFingerprint {
                        name: name.to_owned(),
                        length,
                        md5: Some(md5.to_owned()),
                        primary: true,
                    })
                    .collect(),
                },
                assets: Vec::new(),
            }],
        };
        let metadata = alignment_header_metadata_from_sam(header);
        let resolution = resolve_assembly_from_header(&catalogue, &metadata);
        assert_eq!(
            resolution.status,
            rgv_core::AssemblyResolutionStatus::Matched
        );
        assert_eq!(resolution.matched_contigs, 3);

        let mismatched = header
            .windows(1)
            .enumerate()
            .map(|(index, byte)| {
                if index == header.len() - 2 {
                    b'0'
                } else {
                    byte[0]
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(
            resolve_assembly_from_header(
                &catalogue,
                &alignment_header_metadata_from_sam(&mismatched),
            )
            .status,
            rgv_core::AssemblyResolutionStatus::Matched,
        );
    }
}
