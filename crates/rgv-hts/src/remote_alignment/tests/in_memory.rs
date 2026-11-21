//! SAM files and unindexed BAM files loaded into memory (PROD-10) answer exactly as the
//! equivalent coordinate-sorted, indexed BAM read by `HTSlib`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use rust_htslib::bam::{self, Format, Read as _, Record};

use super::{
    assert_stream_matches_query,
    differential::{
        assert_records_identical, fixture_records, interval, intervals, option_variants,
        write_alignment,
    },
    write_dense_bam,
};
use crate::{
    AlignmentError, AlignmentOpenOptions, AlignmentQuery, AlignmentSource, LocalAlignmentSource,
    LocalBamAlignmentSource, MAX_IN_MEMORY_ALIGNMENT_BYTES, ProductionAlignmentSource,
    QueryOptions,
};

/// The fixture records with every placement run reversed: far from coordinate order, while
/// records sharing a start keep their relative order (a sort has no other order to restore).
fn unsorted(records: &[Record]) -> Vec<Record> {
    let mut shuffled = records.to_vec();
    shuffled.sort_by_key(|record| std::cmp::Reverse((record.tid(), record.pos())));
    assert!(
        shuffled
            .windows(2)
            .any(|pair| (pair[0].tid(), pair[0].pos()) > (pair[1].tid(), pair[1].pos()))
    );
    shuffled
}

/// What `samtools view -b` makes of a SAM file: `HTSlib` parses the text and writes BAM.
fn convert_sam_to_indexed_bam(sam: &Path, bam: &Path) {
    let mut reader = bam::Reader::from_path(sam).unwrap();
    let header = bam::Header::from_template(reader.header());
    {
        let mut writer = bam::Writer::from_path(bam, &header, Format::Bam).unwrap();
        for result in reader.records() {
            writer.write(&result.unwrap()).unwrap();
        }
    }
    bam::index::build(bam, None, bam::index::Type::Bai, 1).unwrap();
}

/// Everything a query returns except the source positions, which name offsets in two
/// different BAM encodings of the same records.
fn assert_results_identical(expected: &AlignmentQuery, actual: &AlignmentQuery, context: &str) {
    assert_eq!(expected.stats, actual.stats, "statistics: {context}");
    assert_eq!(expected.header, actual.header, "header metadata: {context}");
    assert_eq!(expected.batch.len(), actual.batch.len(), "{context}");
    for index in 0..expected.batch.len() {
        assert_records_identical(
            &expected.batch.materialize_record(index).unwrap(),
            &actual.batch.materialize_record(index).unwrap(),
            context,
        );
    }
    assert_eq!(expected.pairs, actual.pairs, "pair summary: {context}");
    assert_eq!(expected.junctions, actual.junctions, "junctions: {context}");
    assert_eq!(expected.pileup, actual.pileup, "pileup: {context}");
}

fn assert_matches_oracle(
    oracle: &mut LocalAlignmentSource,
    loaded: &mut ProductionAlignmentSource,
    what: &str,
) {
    assert!(matches!(loaded, ProductionAlignmentSource::LocalBam(source) if source.is_in_memory()));
    assert_eq!(oracle.reference_dictionary(), loaded.reference_dictionary());
    assert_eq!(
        oracle.normalization_mapped_records(),
        loaded.normalization_mapped_records()
    );
    assert!(loaded.source_stamp().is_some());
    for (name, options) in option_variants() {
        for interval in intervals() {
            let context = format!("{what}, {name}, {interval:?}");
            let expected = oracle.query(interval, options.clone(), &|| true).unwrap();
            let actual = loaded.query(interval, options.clone(), &|| true).unwrap();
            assert_results_identical(&expected, &actual, &context);
        }
    }
    // The positioned stream of the in-memory source agrees with its own one-shot query.
    for interval in intervals() {
        assert_stream_matches_query(loaded, interval, &QueryOptions::default());
    }
}

#[test]
fn a_sam_file_answers_like_the_equivalent_indexed_bam() {
    let directory = tempfile::tempdir().unwrap();
    let records = fixture_records(false);
    let sam = directory.path().join("sorted.sam");
    write_alignment(&sam, Format::Sam, None, &records);
    assert!(fs::read(&sam).unwrap().starts_with(b"@HD"));
    let bam = directory.path().join("equivalent.bam");
    convert_sam_to_indexed_bam(&sam, &bam);
    let mut oracle = LocalAlignmentSource::open(&bam, &AlignmentOpenOptions::default()).unwrap();

    // `open` routes a `.sam` file to the in-memory loader by itself.
    let mut loaded =
        ProductionAlignmentSource::open(&sam, &AlignmentOpenOptions::default()).unwrap();
    assert_matches_oracle(&mut oracle, &mut loaded, "SAM");
    let whole = loaded
        .query(
            interval(0, 0, 100_000),
            option_variants()[1].1.clone(),
            &|| true,
        )
        .unwrap();
    assert!(whole.batch.len() > 15);

    // The same records in another order load to exactly the same source.
    let shuffled = directory.path().join("unsorted.sam");
    write_alignment(&shuffled, Format::Sam, None, &unsorted(&records));
    let mut from_unsorted =
        ProductionAlignmentSource::open(&shuffled, &AlignmentOpenOptions::default()).unwrap();
    assert_matches_oracle(&mut oracle, &mut from_unsorted, "unsorted SAM");
    for (name, options) in option_variants() {
        for interval in intervals() {
            let expected = loaded.query(interval, options.clone(), &|| true).unwrap();
            let actual = from_unsorted
                .query(interval, options.clone(), &|| true)
                .unwrap();
            assert!(
                expected == actual,
                "sorted and unsorted SAM: {name}, {interval:?}"
            );
        }
    }
    // Nothing was written next to the inputs.
    let mut files = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(
        files,
        [
            "equivalent.bam",
            "equivalent.bam.bai",
            "sorted.sam",
            "unsorted.sam"
        ]
    );
}

fn indexed_fixture(directory: &Path) -> (PathBuf, Vec<Record>) {
    let records = fixture_records(false);
    let path = directory.join("indexed.bam");
    write_alignment(&path, Format::Bam, None, &records);
    bam::index::build(&path, None, bam::index::Type::Bai, 1).unwrap();
    (path, records)
}

#[test]
fn an_unindexed_or_unsorted_bam_answers_like_the_sorted_indexed_bam() {
    let directory = tempfile::tempdir().unwrap();
    let (indexed, records) = indexed_fixture(directory.path());
    let mut oracle =
        LocalAlignmentSource::open(&indexed, &AlignmentOpenOptions::default()).unwrap();

    // Sorted but without an index.
    let unindexed = directory.path().join("unindexed.bam");
    fs::copy(&indexed, &unindexed).unwrap();
    // The ordinary open path still refuses a BAM without an index, with either backend.
    assert!(ProductionAlignmentSource::open(&unindexed, &AlignmentOpenOptions::default()).is_err());
    let mut loaded =
        ProductionAlignmentSource::open_in_memory(&unindexed, &AlignmentOpenOptions::default())
            .unwrap();
    assert_matches_oracle(&mut oracle, &mut loaded, "unindexed BAM");

    // Not coordinate-sorted at all.
    let shuffled = directory.path().join("unsorted.bam");
    write_alignment(&shuffled, Format::Bam, None, &unsorted(&records));
    let mut loaded =
        ProductionAlignmentSource::open_in_memory(&shuffled, &AlignmentOpenOptions::default())
            .unwrap();
    assert_matches_oracle(&mut oracle, &mut loaded, "unsorted BAM");

    // A file changed after loading is refused rather than answered from the stale copy.
    fs::write(&shuffled, b"@HD\tVN:1.6\n").unwrap();
    assert!(matches!(
        loaded.query(interval(0, 0, 10), QueryOptions::default(), &|| true),
        Err(AlignmentError::SourceChanged)
    ));
}

#[test]
fn a_file_above_the_cap_is_refused_with_instructions_to_sort_and_index() {
    assert_eq!(MAX_IN_MEMORY_ALIGNMENT_BYTES, 256 * 1024 * 1024);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dense.bam");
    write_dense_bam(&path, 20_000);
    fs::remove_file(format!("{}.bai", path.display())).unwrap();
    let file_bytes = fs::metadata(&path).unwrap().len();
    let options = AlignmentOpenOptions::default();
    let refused = |cap: u64| {
        let Err(AlignmentError::Io(error)) =
            LocalBamAlignmentSource::open_in_memory_with_cap(&path, &options, cap)
        else {
            panic!("a cap of {cap} bytes was not enforced");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
        let message = error.to_string();
        assert!(message.contains("samtools sort"), "{message}");
        assert!(message.contains("samtools index"), "{message}");
        assert!(message.contains("dense.bam"), "{message}");
        assert!(
            !message.contains(&*directory.path().to_string_lossy()),
            "{message}"
        );
    };
    // The file itself is larger than the cap.
    refused(file_bytes - 1);
    // The file fits, its decoded records do not: a BAM inflates to several times its size.
    refused(file_bytes);
    // With room for the records it loads.
    let source =
        LocalBamAlignmentSource::open_in_memory_with_cap(&path, &options, file_bytes * 64).unwrap();
    assert!(source.is_in_memory());

    // The same for SAM text.
    let sam = directory.path().join("small.sam");
    write_alignment(&sam, Format::Sam, None, &fixture_records(false));
    let sam_bytes = fs::metadata(&sam).unwrap().len();
    assert!(matches!(
        LocalBamAlignmentSource::open_in_memory_with_cap(&sam, &options, sam_bytes - 1),
        Err(AlignmentError::Io(error)) if error.kind() == std::io::ErrorKind::FileTooLarge
    ));
    // (The BAM encoding of the fixture's 70,001-operation CIGAR is larger than its text.)
    assert!(
        LocalBamAlignmentSource::open_in_memory_with_cap(&sam, &options, sam_bytes * 4).is_ok()
    );
}

#[test]
fn a_cram_or_a_url_is_not_loaded_into_memory() {
    let directory = tempfile::tempdir().unwrap();
    let (cram, _) = super::differential::write_cram_fixture(directory.path());
    let renamed = directory.path().join("really-cram.sam");
    fs::copy(&cram, &renamed).unwrap();
    let Err(error) = ProductionAlignmentSource::open(&renamed, &AlignmentOpenOptions::default())
    else {
        panic!("a CRAM was loaded as SAM");
    };
    assert!(error.to_string().contains("CRAM"), "{error}");
    assert!(
        ProductionAlignmentSource::open_in_memory(
            "s3://bucket/reads.bam",
            &AlignmentOpenOptions::default()
        )
        .is_err()
    );
}
