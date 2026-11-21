#![no_main]

use std::io::Write as _;

use libfuzzer_sys::fuzz_target;
use rgv_hts::LocalVariantSource;
use rust_htslib::{bcf, bgzf};

/// Contigs queried per input and records decoded per query.
const MAXIMUM_QUERIED_CONTIGS: usize = 4;
const MAXIMUM_RECORDS: usize = 256;
/// Query end: inside every index's coordinate range, so the query itself is always valid.
const QUERY_END: u64 = 1 << 28;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    // RGV's own line scanner (the region filter of the remote VCF path) on every line.
    let mut contigs = Vec::<&[u8]>::new();
    for line in data.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let _ = rgv_hts::fuzzing::variant_line_interval(line);
        if line.first().is_some_and(|byte| *byte != b'#')
            && let Some(contig) = line.split(|byte| *byte == b'\t').next()
            && !contigs.contains(&contig)
            && contigs.len() < MAXIMUM_QUERIED_CONTIGS
        {
            contigs.push(contig);
        }
    }
    // The input as an index: exercises BGZF framing and the tabix/CSI parser's rejection.
    let _ = rgv_hts::fuzzing::parse_variant_index(data);

    // The input as VCF text, bgzip-framed and tabix-indexed here, then opened, queried at full
    // detail (INFO, FORMAT and genotype decoding) and scanned for the ID index.
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };
    let path = directory.path().join("input.vcf.gz");
    let written = bgzf::Writer::from_path(&path)
        .is_ok_and(|mut writer| writer.write_all(data).is_ok() && writer.flush().is_ok());
    if !written || bcf::index::build(&path, None, 1, bcf::index::Type::Tbx).is_err() {
        return;
    }
    // Whatever HTSlib indexed, RGV's tabix parser reads.
    if let Ok(index) = std::fs::read(directory.path().join("input.vcf.gz.tbi")) {
        let _ = rgv_hts::fuzzing::parse_variant_index(&index);
    }
    let Ok(mut source) = LocalVariantSource::open(&path) else {
        return;
    };
    for contig in contigs {
        if let Ok(contig) = std::str::from_utf8(contig) {
            let _ = source.query(contig, 0, QUERY_END, MAXIMUM_RECORDS);
        }
    }
    let _ = source.search_ids("rs", 16);
});
