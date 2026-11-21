#![no_main]

use std::{io::Write as _, path::Path};

use libfuzzer_sys::fuzz_target;
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource as _, LocalAlignmentSource, QueryDetail, QueryOptions,
};
use rust_htslib::{bam, bgzf};

/// Contigs queried per input; enough to reach records without letting a forged dictionary
/// dominate the run.
const MAXIMUM_QUERIED_CONTIGS: usize = 4;

/// Indexes `path` and, if HTSlib accepts it, opens it and decodes every record of the first few
/// contigs at full detail (sequence, qualities, auxiliary tags, modified bases).
///
/// `LocalAlignmentSource::open` refuses a BAM without an index, so a harness which writes only
/// the BAM never gets past that refusal.
fn index_open_and_query(path: &Path) {
    if bam::index::build(&path.to_path_buf(), None, bam::index::Type::Bai, 1).is_err() {
        return;
    }
    let Ok(mut source) = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()) else {
        return;
    };
    let contigs = source
        .reference_dictionary()
        .iter()
        .map(|contig| contig.length())
        .take(MAXIMUM_QUERIED_CONTIGS)
        .collect::<Vec<_>>();
    for (index, length) in contigs.into_iter().enumerate() {
        let Ok(interval) = GenomicInterval::new(ContigId::new(index as u32), 0, length.max(1))
        else {
            continue;
        };
        let _ = source.query(
            interval,
            QueryOptions {
                detail: QueryDetail::Full,
                collect_alleles: true,
                collect_modifications: true,
                ..QueryOptions::default()
            },
            &|| true,
        );
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 2 * 1024 * 1024 {
        return;
    }
    let Ok(directory) = tempfile::tempdir() else {
        return;
    };

    // The input as a complete file: exercises BGZF framing and its rejection.
    let raw = directory.path().join("raw.bam");
    if std::fs::write(&raw, data).is_ok() {
        index_open_and_query(&raw);
    }

    // The input as an uncompressed BAM payload, BGZF-framed here. Mutations then land on the
    // BAM header and record layer instead of being stopped by a gzip checksum. Seeds for this
    // form live in `fuzz/seeds/alignment_file`.
    let framed = directory.path().join("framed.bam");
    let written = bgzf::Writer::from_path(&framed)
        .is_ok_and(|mut writer| writer.write_all(data).is_ok() && writer.flush().is_ok());
    if written {
        index_open_and_query(&framed);
    }
});
