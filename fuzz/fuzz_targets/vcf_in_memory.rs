#![no_main]

use std::io::Write as _;

use flate2::{Compression, write::GzEncoder};
use libfuzzer_sys::fuzz_target;
use rgv_hts::LocalVariantSource;

/// Contigs queried per input and records decoded per query.
const MAXIMUM_QUERIED_CONTIGS: usize = 4;
const MAXIMUM_RECORDS: usize = 256;
const QUERY_END: u64 = 1 << 28;

/// Opens `path`, which has no index, so the whole file is loaded into memory, then queries it
/// at full detail and scans it for the ID index.
fn load_and_query(path: &std::path::Path, contigs: &[&[u8]]) {
    let Ok(mut source) = LocalVariantSource::open(path) else {
        return;
    };
    for contig in contigs {
        if let Ok(contig) = std::str::from_utf8(contig) {
            let _ = source.query(contig, 0, QUERY_END, MAXIMUM_RECORDS);
            // A narrow window: the overlap selection, not only "everything".
            let _ = source.query(contig, 100, 101, MAXIMUM_RECORDS);
        }
    }
    let _ = source.search_ids("rs", 16);
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    let mut contigs = Vec::<&[u8]>::new();
    for line in data.split(|byte| *byte == b'\n') {
        if line.first().is_some_and(|byte| *byte != b'#')
            && let Some(contig) = line.split(|byte| *byte == b'\t').next()
            && !contigs.contains(&contig)
            && contigs.len() < MAXIMUM_QUERIED_CONTIGS
        {
            contigs.push(contig);
        }
    }
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };
    // The input as an uncompressed VCF (or BCF, if it happens to be one): never indexed, so
    // the in-memory loader reads it whole.
    let plain = directory.path().join("input.vcf");
    if std::fs::write(&plain, data).is_ok() {
        load_and_query(&plain, &contigs);
    }
    // The same bytes gzip-compressed (not BGZF): the loader's other compression path.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    if encoder.write_all(data).is_err() {
        return;
    }
    let Ok(compressed) = encoder.finish() else {
        return;
    };
    let gzip = directory.path().join("input.vcf.gz");
    if std::fs::write(&gzip, compressed).is_ok() {
        load_and_query(&gzip, &contigs);
    }
});
