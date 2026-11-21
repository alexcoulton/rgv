#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    // A coverage sidecar tile file (`.rgvcov`) as the cache reads it: once as given, where
    // the checksum rejects almost everything, and once resealed with the identity its header
    // names, where the header and every bin are validated. A tile that decodes must encode
    // back to the same bytes (asserted inside).
    let _ = rgv_hts::fuzzing::decode_coverage_sidecar_tile(data);
});
