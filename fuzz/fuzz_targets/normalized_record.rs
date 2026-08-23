#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_core::ContigId;
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentRecord, AuxField, AuxValue, CigarOp, CigarOpKind,
    SequenceData,
};

fuzz_target!(|data: &[u8]| {
    let cigar = data
        .chunks_exact(5)
        .take(100_000)
        .map(|chunk| CigarOp {
            kind: match chunk[0] % 9 {
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
            len: u32::from_le_bytes([chunk[1], chunk[2], chunk[3], chunk[4]]),
        })
        .collect();
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 0,
        end: 1,
        flags: AlignmentFlags::from_bits(
            u16::from_le_bytes(data.get(..2).unwrap_or(&[0, 0]).try_into().unwrap()),
        ),
        mapping_quality: data.first().copied(),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: data.iter().copied().take(1_024).collect(),
        cigar,
        sequence: SequenceData::Missing,
        auxiliary_fields: vec![AuxField {
            tag: *b"ZZ",
            value: AuxValue::HexByteArray(String::from_utf8_lossy(data).into_owned()),
        }],
    };
    let mut batch = AlignmentBatch::default();
    if batch.push(&record).is_ok() {
        assert!(batch.validate());
        assert_eq!(batch.materialize_record(0).unwrap(), record);
    }
});
