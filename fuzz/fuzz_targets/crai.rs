#![no_main]

use std::io::Write as _;

use flate2::{Compression, write::GzEncoder};
use libfuzzer_sys::fuzz_target;

/// CRAM object length the index offsets are validated against.
const SOURCE_LENGTH: u64 = 1 << 40;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    // The input as a complete `.crai`: exercises gzip framing and its rejection.
    let _ = rgv_hts::fuzzing::parse_cram_index(data, SOURCE_LENGTH);
    // A tiny object makes the "points outside the CRAM" checks reachable too.
    let _ = rgv_hts::fuzzing::parse_cram_index(data, 4_096);

    // The input as the index text, gzip-framed here, so mutations land on the line parser
    // instead of being stopped by a checksum.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    if encoder.write_all(data).is_err() {
        return;
    }
    let Ok(compressed) = encoder.finish() else {
        return;
    };
    let _ = rgv_hts::fuzzing::parse_cram_index(&compressed, SOURCE_LENGTH);
    let _ = rgv_hts::fuzzing::parse_cram_index(&compressed, 4_096);
});
