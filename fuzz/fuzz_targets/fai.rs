#![no_main]

use libfuzzer_sys::fuzz_target;
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::FastaReference;

/// The FASTA every fuzzed index is paired with; the index, not the sequence, is the input.
const FASTA: &[u8] =
    b">chr1 first\nACGTACGTACGTACGTACGT\nACGTACGTAC\n>chr2\nNNNNNNNNNN\nacgtacgt\n";
/// Contigs fetched per input, and bases fetched per contig, whatever the index declares.
const MAXIMUM_FETCHED_CONTIGS: usize = 4;
const MAXIMUM_FETCHED_BASES: u64 = 4_096;

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    // RGV's own `.fai` parser (the remote FASTA path).
    let _ = rgv_hts::fuzzing::parse_fasta_index(data);

    // The same bytes as the index of a local FASTA: open, then fetch what the index claims.
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };
    let path = directory.path().join("reference.fa");
    if std::fs::write(&path, FASTA).is_err()
        || std::fs::write(directory.path().join("reference.fa.fai"), data).is_err()
    {
        return;
    }
    let Ok(reference) = FastaReference::open(&path) else {
        return;
    };
    let lengths = reference
        .reference_dictionary()
        .iter()
        .map(|contig| contig.length())
        .take(MAXIMUM_FETCHED_CONTIGS)
        .collect::<Vec<_>>();
    for (index, length) in lengths.into_iter().enumerate() {
        let end = length.clamp(1, MAXIMUM_FETCHED_BASES);
        if let Ok(interval) = GenomicInterval::new(ContigId::new(index as u32), 0, end) {
            let _ = reference.fetch(interval);
        }
        // The last bases of the declared length: where a forged length reads past the file.
        if let Ok(interval) = GenomicInterval::new(
            ContigId::new(index as u32),
            length.saturating_sub(MAXIMUM_FETCHED_BASES),
            length.max(1),
        ) {
            let _ = reference.fetch(interval);
        }
    }
});
