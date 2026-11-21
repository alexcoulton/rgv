#![no_main]

use std::io::Write as _;

use flate2::{Compression, write::GzEncoder};
use libfuzzer_sys::fuzz_target;

/// Magic of an uncompressed tabix index.
const TABIX_MAGIC: &[u8] = b"TBI\x01";

fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    // The input as the uncompressed index: every mutation lands on the tabix parser written
    // for the remote VCF and annotation paths (counts, names, bins, chunks, linear index).
    let _ = rgv_hts::fuzzing::parse_inflated_tabix_index(data);
    // The same with the magic supplied, so short inputs get past the first check.
    let mut prefixed = TABIX_MAGIC.to_vec();
    prefixed.extend_from_slice(data);
    let _ = rgv_hts::fuzzing::parse_inflated_tabix_index(&prefixed);

    // The input as a stored index: framing and its rejection, then the format dispatch
    // (tabix or CSI, VCF or BCF) with the input gzip-framed here.
    let _ = rgv_hts::fuzzing::parse_variant_index(data);
    for content in [data, prefixed.as_slice()] {
        if let Some(compressed) = gzip(content) {
            let _ = rgv_hts::fuzzing::parse_variant_index(&compressed);
        }
    }
});
