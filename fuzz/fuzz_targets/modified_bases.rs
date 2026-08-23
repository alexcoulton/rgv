#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_core::ContigId;
use rgv_hts::{
    AlignmentFlags, AlignmentRecord, AuxField, AuxValue, SequenceData, decode_modified_bases,
};

fuzz_target!(|data: &[u8]| {
    let split = data.len() / 2;
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 0,
        end: 1,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: None,
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: Vec::new(),
        cigar: Vec::new(),
        sequence: SequenceData::Present {
            bases: data[..split].to_vec(),
            qualities: None,
        },
        auxiliary_fields: vec![
            AuxField {
                tag: *b"MM",
                value: AuxValue::String(String::from_utf8_lossy(&data[split..]).into_owned()),
            },
            AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(data.iter().map(|value| u64::from(*value)).collect()),
            },
        ],
    };
    let _ = decode_modified_bases(&record);
});
