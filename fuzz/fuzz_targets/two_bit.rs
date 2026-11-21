#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::FastaReference;

/// Sequences fetched per input, and bases fetched per sequence, whatever the header declares.
const MAXIMUM_FETCHED_SEQUENCES: usize = 4;
const MAXIMUM_FETCHED_BASES: u64 = 4_096;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };
    let path = directory.path().join("genome.2bit");
    if std::fs::write(&path, data).is_err() {
        return;
    }
    // Header, sequence index and every sequence record's length are parsed on open.
    let Ok(reference) = FastaReference::open(&path) else {
        return;
    };
    let lengths = reference
        .reference_dictionary()
        .iter()
        .map(|contig| contig.length())
        .take(MAXIMUM_FETCHED_SEQUENCES)
        .collect::<Vec<_>>();
    for (index, length) in lengths.into_iter().enumerate() {
        // The start and the end of each sequence: N-block and mask-block tables, then packed
        // bases, with the declared length bounded here rather than trusted.
        for (start, end) in [
            (0, length.clamp(1, MAXIMUM_FETCHED_BASES)),
            (length.saturating_sub(MAXIMUM_FETCHED_BASES), length.max(1)),
        ] {
            if let Ok(interval) = GenomicInterval::new(ContigId::new(index as u32), start, end) {
                let _ = reference.fetch(interval);
            }
        }
    }
});
