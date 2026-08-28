use rgv_core::ContigId;
use thiserror::Error;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

pub(crate) fn alignment_header_metadata_from_sam(bytes: &[u8]) -> AlignmentHeaderMetadata {
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
pub struct AlignmentBatch {
    pub contigs: Vec<ContigId>,
    pub starts: Vec<u64>,
    pub ends: Vec<u64>,
    pub flags: Vec<AlignmentFlags>,
    pub mapping_qualities: Vec<Option<u8>>,
    pub mate_contigs: Vec<Option<ContigId>>,
    pub mate_starts: Vec<Option<u64>>,
    pub template_lengths: Vec<i64>,
    pub read_name_offsets: Vec<u32>,
    pub read_names: Vec<u8>,
    pub cigar_offsets: Vec<u32>,
    pub cigar_ops: Vec<CigarOp>,
    sequence_states: Vec<PackedSequenceState>,
    sequence_offsets: Vec<u32>,
    sequences: Vec<u8>,
    quality_offsets: Vec<u32>,
    qualities: Vec<u8>,
    auxiliary_offsets: Vec<u32>,
    auxiliary_data: Vec<u8>,
}

impl Default for AlignmentBatch {
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
        }
    }
}

impl AlignmentBatch {
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    pub fn validate(&self) -> bool {
        let len = self.len();
        self.contigs.len() == len
            && self.ends.len() == len
            && self.flags.len() == len
            && self.mapping_qualities.len() == len
            && self.mate_contigs.len() == len
            && self.mate_starts.len() == len
            && self.template_lengths.len() == len
            && self.sequence_states.len() == len
            && valid_offsets(&self.read_name_offsets, len, self.read_names.len())
            && valid_offsets(&self.cigar_offsets, len, self.cigar_ops.len())
            && valid_offsets(&self.sequence_offsets, len, self.sequences.len())
            && valid_offsets(&self.quality_offsets, len, self.qualities.len())
            && valid_offsets(&self.auxiliary_offsets, len, self.auxiliary_data.len())
            && (0..len).all(|index| self.sequence(index).is_some())
            && (0..len).all(|index| self.auxiliary_fields(index).is_ok())
    }

    pub fn push(&mut self, record: &AlignmentRecord) -> Result<(), BatchOverflow> {
        let read_name_end = arena_end(self.read_names.len(), record.read_name.len())?;
        let cigar_end = arena_end(self.cigar_ops.len(), record.cigar.len())?;
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
        let sequence_end = arena_end(self.sequences.len(), bases.len())?;
        let quality_end = arena_end(self.qualities.len(), qualities.map_or(0, <[u8]>::len))?;
        let auxiliary_bytes =
            record
                .auxiliary_fields
                .iter()
                .try_fold(0_usize, |total, field| {
                    total
                        .checked_add(encoded_auxiliary_len(field)?)
                        .ok_or(BatchOverflow)
                })?;
        let auxiliary_end = arena_end(self.auxiliary_data.len(), auxiliary_bytes)?;
        for field in &record.auxiliary_fields {
            encode_auxiliary_field(&mut self.auxiliary_data, field)?;
        }

        self.contigs.push(record.contig);
        self.starts.push(record.start);
        self.ends.push(record.end);
        self.flags.push(record.flags);
        self.mapping_qualities.push(record.mapping_quality);
        self.mate_contigs.push(record.mate_contig);
        self.mate_starts.push(record.mate_start);
        self.template_lengths.push(record.template_length);
        self.read_names.extend_from_slice(&record.read_name);
        self.read_name_offsets.push(read_name_end);
        self.cigar_ops.extend_from_slice(&record.cigar);
        self.cigar_offsets.push(cigar_end);
        self.sequence_states.push(sequence_state);
        self.sequences.extend_from_slice(bases);
        self.sequence_offsets.push(sequence_end);
        if let Some(qualities) = qualities {
            self.qualities.extend_from_slice(qualities);
        }
        self.quality_offsets.push(quality_end);
        self.auxiliary_offsets.push(auxiliary_end);
        Ok(())
    }

    pub fn read_name(&self, index: usize) -> Option<&[u8]> {
        arena_slice(&self.read_name_offsets, &self.read_names, index)
    }

    pub fn cigar(&self, index: usize) -> Option<&[CigarOp]> {
        arena_slice(&self.cigar_offsets, &self.cigar_ops, index)
    }

    pub fn sequence(&self, index: usize) -> Option<SequenceView<'_>> {
        let state = *self.sequence_states.get(index)?;
        let bases = arena_slice(&self.sequence_offsets, &self.sequences, index)?;
        let qualities = arena_slice(&self.quality_offsets, &self.qualities, index)?;
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

    pub fn has_auxiliary_tag(&self, index: usize, tag: [u8; 2]) -> bool {
        let Some(data) = arena_slice(&self.auxiliary_offsets, &self.auxiliary_data, index) else {
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

    pub fn auxiliary_field(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<AuxField>, PackedRecordError> {
        let data = arena_slice(&self.auxiliary_offsets, &self.auxiliary_data, index)
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
        let data = arena_slice(&self.auxiliary_offsets, &self.auxiliary_data, index)
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
        let data = arena_slice(&self.auxiliary_offsets, &self.auxiliary_data, index)?;
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

    pub fn materialize_record(&self, index: usize) -> Result<AlignmentRecord, PackedRecordError> {
        let contig = *self
            .contigs
            .get(index)
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
            start: self.starts[index],
            end: self.ends[index],
            flags: self.flags[index],
            mapping_quality: self.mapping_qualities[index],
            mate_contig: self.mate_contigs[index],
            mate_start: self.mate_starts[index],
            template_length: self.template_lengths[index],
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

    pub fn estimated_bytes(&self) -> usize {
        self.contigs.len() * std::mem::size_of::<ContigId>()
            + self.starts.len() * std::mem::size_of::<u64>()
            + self.ends.len() * std::mem::size_of::<u64>()
            + self.flags.len() * std::mem::size_of::<AlignmentFlags>()
            + self.mapping_qualities.len() * std::mem::size_of::<Option<u8>>()
            + self.mate_contigs.len() * std::mem::size_of::<Option<ContigId>>()
            + self.mate_starts.len() * std::mem::size_of::<Option<u64>>()
            + self.template_lengths.len() * std::mem::size_of::<i64>()
            + self.read_name_offsets.len() * std::mem::size_of::<u32>()
            + self.read_names.len()
            + self.cigar_offsets.len() * std::mem::size_of::<u32>()
            + self.cigar_ops.len() * std::mem::size_of::<CigarOp>()
            + self.sequence_states.len() * std::mem::size_of::<PackedSequenceState>()
            + self.sequence_offsets.len() * std::mem::size_of::<u32>()
            + self.sequences.len()
            + self.quality_offsets.len() * std::mem::size_of::<u32>()
            + self.qualities.len()
            + self.auxiliary_offsets.len() * std::mem::size_of::<u32>()
            + self.auxiliary_data.len()
    }
}

fn valid_offsets(offsets: &[u32], records: usize, arena_len: usize) -> bool {
    offsets.len() == records + 1
        && offsets.first() == Some(&0)
        && offsets.windows(2).all(|pair| pair[0] <= pair[1])
        && offsets.last().copied().map(|value| value as usize) == Some(arena_len)
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
