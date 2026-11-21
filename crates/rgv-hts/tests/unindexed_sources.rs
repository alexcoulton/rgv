//! A file that has no index, the wrong compression or a broken header must end in a result or
//! an error, never in a crash inside `HTSlib`. Each case is its own test so a signal is
//! attributable: `cargo test -p rgv-hts --test unindexed_sources <name> -- --test-threads=1`.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource as _, FastaReference, FeatureFormat,
    LocalAlignmentSource, LocalVariantSource, ProductionAlignmentSource, TextFeatureSource,
    TrackError, VariantQuery,
};
use rust_htslib::{
    bam,
    bcf::{self, Read as _},
};

const CONTIG_LINES: &str = "##contig=<ID=chr1,length=100000>\n##contig=<ID=chr2,length=100000>\n";

fn vcf_text(contig_lines: bool, samples: &[&str], records: &[&str]) -> String {
    let mut text = String::from("##fileformat=VCFv4.2\n");
    if contig_lines {
        text.push_str(CONTIG_LINES);
    }
    text.push_str("##FILTER=<ID=q10,Description=\"Quality below 10\">\n");
    text.push_str("##INFO=<ID=DP,Number=1,Type=Integer,Description=\"Depth\">\n");
    text.push_str("##INFO=<ID=END,Number=1,Type=Integer,Description=\"End\">\n");
    text.push_str("##INFO=<ID=SVTYPE,Number=1,Type=String,Description=\"Type\">\n");
    text.push_str("##ALT=<ID=DEL,Description=\"Deletion\">\n");
    text.push_str("##FORMAT=<ID=GT,Number=1,Type=String,Description=\"Genotype\">\n");
    text.push_str("#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO");
    if !samples.is_empty() {
        text.push_str("\tFORMAT");
        for sample in samples {
            text.push('\t');
            text.push_str(sample);
        }
    }
    text.push('\n');
    for record in records {
        text.push_str(record);
        text.push('\n');
    }
    text
}

/// Four sorted site-only records: three on `chr1` (the last a long deletion), one on `chr2`.
const SITES: [&str; 4] = [
    "chr1\t100\trs1\tA\tT\t50\tPASS\tDP=10",
    "chr1\t200\trs2;alias2\tAC\tA\t5\tq10\tDP=3",
    "chr1\t5000\tsv1\tN\t<DEL>\t.\t.\tSVTYPE=DEL;END=9000",
    "chr2\t50\trs3\tG\tC\t.\t.\t.",
];

fn write(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// Rewrites a plain VCF as BGZF-compressed VCF or as BCF through `HTSlib`.
fn transcode(source: &Path, target: &Path, format: bcf::Format) {
    let mut reader = bcf::Reader::from_path(source).unwrap();
    let header = bcf::Header::from_template(reader.header());
    let mut writer = bcf::Writer::from_path(target, &header, false, format).unwrap();
    for record in reader.records() {
        let mut record = record.unwrap();
        writer.translate(&mut record);
        writer.write(&record).unwrap();
    }
}

fn starts(query: &VariantQuery) -> Vec<u64> {
    query.records.iter().map(|record| record.start).collect()
}

/// Everything the track engine asks of a variant source, in the order it asks.
fn exercise(path: &Path) -> Result<(), TrackError> {
    let mut source = LocalVariantSource::open(path)?;
    source.query("chr1", 0, 100_000, 1000)?;
    source.query("chr2", 0, 100_000, 1000)?;
    let _ = source.query("chrAbsent", 0, 100_000, 1000);
    source.search_ids("rs", 10)?;
    source.query("chr1", 150, 250, 1000)?;
    Ok(())
}

/// Asserts the four [`SITES`] are served completely and correctly from `path`.
fn assert_sites_served(path: &Path) {
    let mut source = LocalVariantSource::open(path).unwrap();
    let whole = source.query("chr1", 0, 100_000, 1000).unwrap();
    assert_eq!(starts(&whole), [99, 199, 4999]);
    assert_eq!(whole.records[0].id.as_deref(), Some("rs1"));
    assert_eq!(whole.records[0].reference, b"A");
    assert_eq!(whole.records[0].alternates, [b"T".to_vec()]);
    assert_eq!(whole.records[1].filters, ["q10"]);
    assert_eq!(whole.records[2].end, 9000);
    // Half-open `[start, end)` against each record's `[start, end)`.
    assert_eq!(starts(&source.query("chr1", 99, 100, 10).unwrap()), [99]);
    assert!(
        source
            .query("chr1", 100, 199, 10)
            .unwrap()
            .records
            .is_empty()
    );
    assert_eq!(starts(&source.query("chr1", 200, 201, 10).unwrap()), [199]);
    assert!(
        source
            .query("chr1", 201, 4999, 10)
            .unwrap()
            .records
            .is_empty()
    );
    // The deletion spans the window without starting in it.
    assert_eq!(
        starts(&source.query("chr1", 8000, 8001, 10).unwrap()),
        [4999]
    );
    assert!(
        source
            .query("chr1", 9000, 9001, 10)
            .unwrap()
            .records
            .is_empty()
    );
    assert_eq!(starts(&source.query("chr2", 0, 1000, 10).unwrap()), [49]);
    let truncated = source.query("chr1", 0, 100_000, 2).unwrap();
    assert_eq!(starts(&truncated), [99, 199]);
    assert!(truncated.provenance.records_truncated);
    let hits = source.search_ids("rs", 10).unwrap();
    assert_eq!(
        hits.hits
            .iter()
            .map(|hit| (hit.id.as_str(), hit.contig.as_str(), hit.start))
            .collect::<Vec<_>>(),
        [
            ("rs1", "chr1", 99),
            ("rs2", "chr1", 199),
            ("rs3", "chr2", 49)
        ]
    );
    assert_eq!(source.search_ids("ALIAS", 10).unwrap().hits[0].start, 199);
}

#[test]
fn plain_unindexed_vcf_is_served_from_memory() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    assert_sites_served(&path);
}

#[test]
fn plain_unindexed_vcf_without_contig_lines_is_served() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        directory.path(),
        "no-contigs.vcf",
        vcf_text(false, &[], &SITES).as_bytes(),
    );
    assert_sites_served(&path);
}

#[test]
fn plain_unindexed_vcf_answers_an_absent_contig_with_no_records() {
    let directory = tempfile::tempdir().unwrap();
    for contig_lines in [true, false] {
        let path = write(
            directory.path(),
            "absent.vcf",
            vcf_text(contig_lines, &[], &SITES).as_bytes(),
        );
        let mut source = LocalVariantSource::open(&path).unwrap();
        // Before and after another query, and after the ID index was built.
        assert!(
            source
                .query("chrAbsent", 0, 1000, 10)
                .unwrap()
                .records
                .is_empty()
        );
        source.query("chr1", 0, 1000, 10).unwrap();
        source.search_ids("rs1", 10).unwrap();
        assert!(
            source
                .query("chrAbsent", 0, 1000, 10)
                .unwrap()
                .records
                .is_empty()
        );
        assert!(source.query("", 0, 1000, 10).unwrap().records.is_empty());
    }
}

#[test]
fn plain_unindexed_vcf_with_an_empty_body_is_served() {
    let directory = tempfile::tempdir().unwrap();
    for contig_lines in [true, false] {
        let path = write(
            directory.path(),
            "empty-body.vcf",
            vcf_text(contig_lines, &["S1"], &[]).as_bytes(),
        );
        let mut source = LocalVariantSource::open(&path).unwrap();
        let query = source.query("chr1", 0, 1000, 10).unwrap();
        assert!(query.records.is_empty());
        assert_eq!(query.header.samples, ["S1"]);
        assert!(source.search_ids("rs", 10).unwrap().hits.is_empty());
    }
}

#[test]
fn plain_unindexed_multi_sample_vcf_carries_genotypes() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        directory.path(),
        "cohort.vcf",
        vcf_text(
            true,
            &["S1", "S2", "S3"],
            &[
                "chr1\t100\trs1\tA\tT\t50\tPASS\tDP=10\tGT\t0/1\t1|1\t./.",
                "chr1\t200\trs2\tC\tG,T\t50\tPASS\tDP=10\tGT\t1/2\t0/0\t0/1",
            ],
        )
        .as_bytes(),
    );
    let mut source = LocalVariantSource::open(&path).unwrap();
    let query = source.query("chr1", 0, 1000, 10).unwrap();
    assert_eq!(query.header.samples, ["S1", "S2", "S3"]);
    assert_eq!(starts(&query), [99, 199]);
    assert_eq!(query.records[0].genotypes.len(), 3);
    assert_eq!(query.records[1].genotypes.len(), 3);
    assert_eq!(query.provenance.genotype_cells_loaded, 6);
    // Stored records are decoded again by every query; the second answer is the first.
    assert_eq!(source.query("chr1", 0, 1000, 10).unwrap(), query);
}

#[test]
fn plain_unindexed_vcf_with_crlf_line_endings_is_served() {
    let directory = tempfile::tempdir().unwrap();
    let text = vcf_text(true, &[], &SITES).replace('\n', "\r\n");
    let path = write(directory.path(), "crlf.vcf", text.as_bytes());
    assert_sites_served(&path);
}

#[test]
fn gzip_compressed_vcf_that_is_not_bgzf_is_served() {
    let directory = tempfile::tempdir().unwrap();
    let compressed = gzip(vcf_text(true, &[], &SITES).as_bytes());
    for name in ["gzip.vcf", "gzip.vcf.gz"] {
        let path = write(directory.path(), name, &compressed);
        assert_sites_served(&path);
    }
}

#[test]
fn id_search_before_any_query_is_served() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        directory.path(),
        "search-first.vcf",
        vcf_text(false, &[], &SITES).as_bytes(),
    );
    let mut source = LocalVariantSource::open(&path).unwrap();
    assert_eq!(source.search_ids("sv1", 10).unwrap().hits[0].start, 4999);
    assert!(source.search_ids("", 10).unwrap().hits.is_empty());
    assert_eq!(starts(&source.query("chr2", 0, 100, 10).unwrap()), [49]);
}

#[test]
fn unsorted_plain_vcf_is_sorted_in_memory() {
    let directory = tempfile::tempdir().unwrap();
    let shuffled = [SITES[3], SITES[2], SITES[0], SITES[1]];
    for contig_lines in [true, false] {
        let path = write(
            directory.path(),
            "shuffled.vcf",
            vcf_text(contig_lines, &[], &shuffled).as_bytes(),
        );
        assert_sites_served(&path);
    }
}

#[test]
fn bgzf_vcf_without_an_index_is_served_from_memory() {
    let directory = tempfile::tempdir().unwrap();
    let plain = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    let path = directory.path().join("unindexed.vcf.gz");
    transcode(&plain, &path, bcf::Format::Vcf);
    assert_sites_served(&path);
}

#[test]
fn bcf_without_an_index_is_served_from_memory() {
    let directory = tempfile::tempdir().unwrap();
    let plain = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    let path = directory.path().join("unindexed.bcf");
    transcode(&plain, &path, bcf::Format::Bcf);
    assert_sites_served(&path);
}

#[test]
fn indexed_bgzf_vcf_and_bcf_are_still_served_through_the_index() {
    let directory = tempfile::tempdir().unwrap();
    let plain = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    let vcf = directory.path().join("indexed.vcf.gz");
    transcode(&plain, &vcf, bcf::Format::Vcf);
    bcf::index::build(&vcf, None, 1, bcf::index::Type::Tbx).unwrap();
    assert_sites_served(&vcf);
    let csi_vcf = directory.path().join("csi.vcf.gz");
    transcode(&plain, &csi_vcf, bcf::Format::Vcf);
    bcf::index::build(&csi_vcf, None, 1, bcf::index::Type::Csi(14)).unwrap();
    assert_sites_served(&csi_vcf);
    let bcf_path = directory.path().join("indexed.bcf");
    transcode(&plain, &bcf_path, bcf::Format::Bcf);
    bcf::index::build(&bcf_path, None, 1, bcf::index::Type::Csi(14)).unwrap();
    assert_sites_served(&bcf_path);
}

#[test]
fn indexed_vcf_with_an_empty_body_is_served() {
    let directory = tempfile::tempdir().unwrap();
    for (contig_lines, name) in [(true, "declared"), (false, "bare")] {
        let plain = write(
            directory.path(),
            "plain.vcf",
            vcf_text(contig_lines, &[], &[]).as_bytes(),
        );
        for (extension, format, kind) in [
            ("vcf.gz", bcf::Format::Vcf, bcf::index::Type::Tbx),
            ("bcf", bcf::Format::Bcf, bcf::index::Type::Csi(14)),
        ] {
            let path = directory.path().join(format!("{name}.{extension}"));
            transcode(&plain, &path, format);
            bcf::index::build(&path, None, 1, kind).unwrap();
            let mut source = LocalVariantSource::open(&path).unwrap();
            for contig in ["chr1", "chrAbsent"] {
                if let Ok(query) = source.query(contig, 0, 1000, 10) {
                    assert!(query.records.is_empty());
                }
            }
            assert!(source.search_ids("rs", 10).unwrap().hits.is_empty());
        }
    }
}

#[test]
fn indexed_vcf_queries_beyond_the_index_range_do_not_abort() {
    let directory = tempfile::tempdir().unwrap();
    let plain = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    for (extension, format, kind) in [
        ("vcf.gz", bcf::Format::Vcf, bcf::index::Type::Tbx),
        ("bcf", bcf::Format::Bcf, bcf::index::Type::Csi(14)),
    ] {
        let path = directory.path().join(format!("range.{extension}"));
        transcode(&plain, &path, format);
        bcf::index::build(&path, None, 1, kind).unwrap();
        let mut source = LocalVariantSource::open(&path).unwrap();
        for (start, end) in [
            (1 << 29, (1 << 29) + 10),
            (1 << 31, (1 << 31) + 10),
            (1 << 44, (1 << 44) + 10),
            (u64::MAX - 1, u64::MAX),
            (0, u64::MAX),
            (10, 10),
            (10, 5),
        ] {
            if let Ok(query) = source.query("chr1", start, end, 10) {
                assert!(query.records.len() <= 3, "{start}-{end}");
            }
        }
        // The source still works afterwards.
        assert_eq!(
            starts(&source.query("chr1", 0, 1000, 10).unwrap()),
            [99, 199]
        );
    }
}

#[test]
fn plain_vcf_queries_at_extreme_coordinates_are_empty() {
    let directory = tempfile::tempdir().unwrap();
    let path = write(
        directory.path(),
        "extreme.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    let mut source = LocalVariantSource::open(&path).unwrap();
    assert!(
        source
            .query("chr1", u64::MAX - 1, u64::MAX, 10)
            .unwrap()
            .records
            .is_empty()
    );
    assert!(source.query("chr1", 10, 10, 10).unwrap().records.is_empty());
    assert!(source.query("chr1", 500, 5, 10).unwrap().records.is_empty());
    assert_eq!(
        source.query("chr1", 0, u64::MAX, 10).unwrap().records.len(),
        3
    );
}

#[test]
fn broken_or_stale_indexes_are_clean_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    let plain = write(
        directory.path(),
        "plain.vcf",
        vcf_text(true, &[], &SITES).as_bytes(),
    );
    for (extension, format, kind, index_extension) in [
        ("vcf.gz", bcf::Format::Vcf, bcf::index::Type::Tbx, "tbi"),
        ("vcf.gz", bcf::Format::Vcf, bcf::index::Type::Csi(14), "csi"),
        ("bcf", bcf::Format::Bcf, bcf::index::Type::Csi(14), "csi"),
    ] {
        let path = directory.path().join(format!("broken.{extension}"));
        let index_path = directory
            .path()
            .join(format!("broken.{extension}.{index_extension}"));
        transcode(&plain, &path, format);
        bcf::index::build(&path, None, 1, kind).unwrap();
        let index = std::fs::read(&index_path).unwrap();
        let replacements: [Vec<u8>; 5] = [
            Vec::new(),
            b"not an index".to_vec(),
            index[..index.len() / 2].to_vec(),
            index[..index.len().min(30)].to_vec(),
            gzip(b"TBI\x01"),
        ];
        for replacement in replacements {
            std::fs::write(&index_path, &replacement).unwrap();
            // Either outcome is acceptable; a broken index must not take the process down.
            let _ = exercise(&path);
        }
        std::fs::remove_file(&index_path).unwrap();
    }
    // A BCF-style CSI (no tabix header) beside a bgzipped VCF, and a tabix index beside a BCF.
    let vcf = directory.path().join("swapped.vcf.gz");
    let bcf_path = directory.path().join("swapped.bcf");
    transcode(&plain, &vcf, bcf::Format::Vcf);
    transcode(&plain, &bcf_path, bcf::Format::Bcf);
    bcf::index::build(&vcf, None, 1, bcf::index::Type::Tbx).unwrap();
    bcf::index::build(&bcf_path, None, 1, bcf::index::Type::Csi(14)).unwrap();
    std::fs::rename(
        directory.path().join("swapped.bcf.csi"),
        directory.path().join("swapped.vcf.gz.csi"),
    )
    .unwrap();
    std::fs::rename(
        directory.path().join("swapped.vcf.gz.tbi"),
        directory.path().join("swapped.bcf.csi"),
    )
    .unwrap();
    let _ = exercise(&vcf);
    let _ = exercise(&bcf_path);
}

#[test]
fn files_that_are_not_variant_files_are_clean_errors() {
    let directory = tempfile::tempdir().unwrap();
    let valid = vcf_text(
        true,
        &["S1"],
        &["chr1\t100\trs1\tA\tT\t50\tPASS\tDP=10\tGT\t0/1"],
    );
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty.vcf", Vec::new()),
        ("garbage.vcf", b"this is not a variant file\n".to_vec()),
        ("binary.vcf", (0..=255_u8).cycle().take(4096).collect()),
        ("bed.vcf", b"chr1\t10\t20\n".to_vec()),
        ("format-only.vcf", b"##fileformat=VCFv4.2\n".to_vec()),
        ("no-newline.vcf", b"##fileformat=VCFv4.2".to_vec()),
        (
            "no-column-line.vcf",
            b"##fileformat=VCFv4.2\n##contig=<ID=chr1>\nchr1\t100\t.\tA\tT\t.\t.\t.\n".to_vec(),
        ),
        (
            "short-column-line.vcf",
            b"##fileformat=VCFv4.2\n#CHROM\tPOS\n".to_vec(),
        ),
        (
            "malformed-meta.vcf",
            b"##fileformat=VCFv4.2\n##INFO=<ID=DP,Number=\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n".to_vec(),
        ),
        ("truncated-header.vcf", valid.as_bytes()[..60].to_vec()),
        ("gzip-garbage.vcf", gzip(b"this is not a variant file\n")),
        ("gzip-empty.vcf.gz", gzip(b"")),
        ("truncated-gzip.vcf.gz", {
            let compressed = gzip(valid.as_bytes());
            compressed[..compressed.len() / 2].to_vec()
        }),
        ("magic-only.bcf", b"BCF\x02\x02".to_vec()),
        ("bad-length.bcf", b"BCF\x02\x02\xff\xff\xff\x7f##fileformat".to_vec()),
        ("gzip-bcf-magic.bcf", gzip(b"BCF\x02\x02\x10\x00\x00\x00")),
    ];
    for (name, bytes) in cases {
        let path = write(directory.path(), name, &bytes);
        // No panic and no signal; whether it opens is asserted for the unambiguous cases.
        let outcome = exercise(&path);
        if matches!(
            name,
            "empty.vcf"
                | "garbage.vcf"
                | "binary.vcf"
                | "bed.vcf"
                | "gzip-garbage.vcf"
                | "magic-only.bcf"
                | "gzip-empty.vcf.gz"
        ) {
            assert!(outcome.is_err(), "{name} was accepted");
        }
    }
    assert!(LocalVariantSource::open(directory.path().join("missing.vcf")).is_err());
    assert!(LocalVariantSource::open(directory.path()).is_err());
}

#[test]
fn malformed_and_undeclared_records_are_clean_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    let cases: [(&str, &[&str]); 7] = [
        // INFO, FILTER and FORMAT keys the header never declared.
        (
            "undeclared",
            &["chr1\t100\trs1\tA\tT\t50\tnope\tXX=1;YY\tZZ\t7"],
        ),
        ("few-columns", &["chr1\t100\trs1\tA"]),
        ("bad-position", &["chr1\tabc\trs1\tA\tT\t.\t.\t."]),
        ("negative-position", &["chr1\t-5\trs1\tA\tT\t.\t.\t."]),
        ("zero-position", &["chr1\t0\trs1\tA\tT\t.\t.\t."]),
        (
            "huge-position",
            &["chr1\t99999999999999999999\trs1\tA\tT\t.\t.\t."],
        ),
        (
            "sample-count",
            &["chr1\t100\trs1\tA\tT\t50\tPASS\tDP=1\tGT\t0/1\t0/1\t0/1\t1/1"],
        ),
    ];
    for (name, records) in cases {
        for contig_lines in [true, false] {
            let path = write(
                directory.path(),
                &format!("{name}.vcf"),
                vcf_text(contig_lines, &["S1"], records).as_bytes(),
            );
            let _ = exercise(&path);
        }
    }
    // A record cut off mid-line, with and without a final newline.
    let text = vcf_text(true, &[], &SITES);
    let path = write(
        directory.path(),
        "cut.vcf",
        &text.as_bytes()[..text.len() - 9],
    );
    let _ = exercise(&path);
    let path = write(
        directory.path(),
        "no-final-newline.vcf",
        text.trim_end().as_bytes(),
    );
    assert_sites_served(&path);
}

fn bgzf(bytes: &[u8]) -> Vec<u8> {
    let mut writer = noodles::bgzf::io::Writer::new(Vec::new());
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}

/// A record whose FILTER, INFO and FORMAT keys (and, without contig lines, contig) the header
/// never declared: `HTSlib` adds them to the header it parses with as it meets them.
const UNDECLARED: [&str; 2] = [
    "chr1\t100\trs1\tA\tT\t50\tnope;q10;other\tXX=1;YY;DP=4\tGT:ZZ\t0/1:7",
    "chr3\t100\trs9\tA\tT\t50\tlater\tDP=1\tGT\t0/1",
];

#[test]
fn undeclared_filters_and_fields_are_served_from_every_local_backend() {
    let directory = tempfile::tempdir().unwrap();
    for contig_lines in [true, false] {
        let text = vcf_text(contig_lines, &["S1"], &UNDECLARED);
        let plain = write(directory.path(), "undeclared.vcf", text.as_bytes());
        let indexed = write(
            directory.path(),
            "undeclared.vcf.gz",
            &bgzf(text.as_bytes()),
        );
        bcf::index::build(&indexed, None, 1, bcf::index::Type::Tbx).unwrap();
        for (path, names_known) in [(&plain, true), (&indexed, false)] {
            let mut source = LocalVariantSource::open(path).unwrap();
            // Twice: the second query meets ids the first one made `HTSlib` invent.
            for _ in 0..2 {
                let query = source.query("chr1", 0, 1000, 10).unwrap();
                assert_eq!(starts(&query), [99]);
                let filters = &query.records[0].filters;
                assert_eq!(filters.len(), 3, "{filters:?}");
                assert!(filters.contains(&"q10".to_owned()), "{filters:?}");
                if names_known {
                    assert_eq!(filters, &["nope", "q10", "other"]);
                }
                let later = source.query("chr3", 0, 1000, 10).unwrap();
                assert_eq!(starts(&later), [99]);
                assert_eq!(later.records[0].filters.len(), 1);
            }
            let hits = source.search_ids("rs", 10).unwrap().hits;
            assert_eq!(
                hits.iter()
                    .map(|hit| (hit.id.as_str(), hit.contig.as_str()))
                    .collect::<Vec<_>>(),
                [("rs1", "chr1"), ("rs9", "chr3")]
            );
        }
    }
}

#[test]
fn a_header_that_is_not_utf8_is_a_clean_error() {
    let directory = tempfile::tempdir().unwrap();
    let mut bytes = vcf_text(true, &[], &SITES).into_bytes();
    let description = bytes
        .windows(5)
        .position(|window| window == b"Depth")
        .unwrap();
    bytes[description] = 0xe9;
    let plain = write(directory.path(), "latin1.vcf", &bytes);
    assert!(LocalVariantSource::open(&plain).is_err());
    let indexed = write(directory.path(), "latin1.vcf.gz", &bgzf(&bytes));
    bcf::index::build(&indexed, None, 1, bcf::index::Type::Tbx).unwrap();
    assert!(LocalVariantSource::open(&indexed).is_err());
}

// --- The other `HTSlib`-backed readers: the same misuse must not crash them either. ---

const BED: &str = "chr1\t10\t20\ta\nchr1\t30\t40\tb\nchr2\t5\t9\tc\n";

fn feature_outcome(path: &Path) -> Result<usize, TrackError> {
    let source = TextFeatureSource::open(path, FeatureFormat::Bed)?;
    let _ = source.query("chrAbsent", 0, 100);
    Ok(source.query("chr1", 0, 100)?.len())
}

#[test]
fn text_tracks_without_a_usable_tabix_index_are_clean_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    // Plain, gzip and BGZF text without an index is read whole.
    let plain = write(directory.path(), "plain.bed", BED.as_bytes());
    assert_eq!(feature_outcome(&plain).unwrap(), 2);
    let gzipped = write(directory.path(), "gzip.bed.gz", &gzip(BED.as_bytes()));
    assert_eq!(feature_outcome(&gzipped).unwrap(), 2);
    let block = write(directory.path(), "bgzf.bed.gz", &bgzf(BED.as_bytes()));
    assert_eq!(feature_outcome(&block).unwrap(), 2);
    // An index that is empty, garbage, truncated, or sits beside a file that is not BGZF.
    for index in [
        &b""[..],
        b"not an index",
        &gzip(b"TBI\x01"),
        &bgzf(b"TBI\x01\x01\x00"),
    ] {
        for extension in ["tbi", "csi"] {
            let index_path = directory.path().join(format!("bgzf.bed.gz.{extension}"));
            std::fs::write(&index_path, index).unwrap();
            let _ = feature_outcome(&block);
            std::fs::remove_file(&index_path).unwrap();
        }
    }
    write(directory.path(), "gzip.bed.gz.tbi", b"not an index");
    let _ = feature_outcome(&gzipped);
    // A valid index beside a file that is gzip, or not compressed at all.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let index = std::fs::read(fixtures.join("indexed.gtf.gz.tbi")).unwrap();
    let mut text = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::MultiGzDecoder::new(
            std::fs::File::open(fixtures.join("indexed.gtf.gz")).unwrap(),
        ),
        &mut text,
    )
    .unwrap();
    let contig =
        String::from_utf8_lossy(text.split(|byte| *byte == b'\t').next().unwrap()).into_owned();
    for (name, bytes) in [
        ("regzip.gtf.gz", gzip(&text)),
        ("plain.gtf.gz", text.clone()),
    ] {
        let path = write(directory.path(), name, &bytes);
        write(directory.path(), &format!("{name}.tbi"), &index);
        if let Ok(source) = TextFeatureSource::open(&path, FeatureFormat::Gtf) {
            let _ = source.query(&contig, 0, 1_000_000_000);
            let _ = source.query("chrAbsent", 0, 100);
        }
    }
    // A directory, and a missing file, where an indexed file is expected.
    let folder = directory.path().join("folder.bed.gz");
    std::fs::create_dir(&folder).unwrap();
    write(directory.path(), "folder.bed.gz.tbi", b"not an index");
    assert!(feature_outcome(&folder).is_err());
    write(directory.path(), "missing.bed.gz.tbi", b"not an index");
    assert!(feature_outcome(&directory.path().join("missing.bed.gz")).is_err());
}

#[cfg(unix)]
#[test]
fn an_unreadable_indexed_text_track_is_a_clean_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("indexed.gtf.gz");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::copy(fixtures.join("indexed.gtf.gz"), &path).unwrap();
    std::fs::copy(
        fixtures.join("indexed.gtf.gz.tbi"),
        directory.path().join("indexed.gtf.gz.tbi"),
    )
    .unwrap();
    assert!(TextFeatureSource::open(&path, FeatureFormat::Gtf).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        // Running as a user the permission bits do not bind: nothing to test.
        return;
    }
    assert!(TextFeatureSource::open(&path, FeatureFormat::Gtf).is_err());
}

fn fasta_outcome(path: &Path) -> Result<Vec<u8>, rgv_hts::AlignmentError> {
    let reference = FastaReference::open(path)?;
    let dictionary = reference.reference_dictionary();
    let Some(contig) = dictionary.iter().next() else {
        return Ok(Vec::new());
    };
    let interval = rgv_core::GenomicInterval::new(contig.id(), 0, contig.length().min(4))
        .expect("interval within the contig");
    reference.fetch(interval)
}

#[test]
fn fasta_without_a_fai_is_indexed_or_a_clean_error() {
    let directory = tempfile::tempdir().unwrap();
    // `HTSlib` builds the missing `.fai` beside a readable FASTA.
    let fasta = write(
        directory.path(),
        "ok.fa",
        b">chr1 first\nACGTACGT\nACGT\n>chr2\nTTTT\n",
    );
    assert_eq!(fasta_outcome(&fasta).unwrap(), b"ACGT");
    // Nothing it can index, or an index it cannot use.
    let cases: [(&str, &[u8]); 6] = [
        ("empty.fa", b""),
        ("garbage.fa", b"this is not a FASTA file\n"),
        ("binary.fa", &[0, 159, 146, 150, 0, 1, 2, 3]),
        ("ragged.fa", b">chr1\nACGTACGT\nAC\nACGTACGT\n"),
        ("duplicate.fa", b">chr1\nACGT\n>chr1\nACGT\n"),
        ("gzip.fa.gz", &gzip(b">chr1\nACGT\n")),
    ];
    for (name, bytes) in cases {
        let path = write(directory.path(), name, bytes);
        let outcome = fasta_outcome(&path);
        if matches!(
            name,
            "garbage.fa" | "binary.fa" | "ragged.fa" | "gzip.fa.gz"
        ) {
            assert!(outcome.is_err(), "{name} was accepted");
        }
    }
    // BGZF-compressed FASTA without `.fai` and `.gzi`.
    let block = write(directory.path(), "bgzf.fa.gz", &bgzf(b">chr1\nACGTACGT\n"));
    let _ = fasta_outcome(&block);
    for index in [
        &b""[..],
        b"not an index\n",
        b"chr1\t8\n",
        b"chr1\t999999\t6\t8\t9\n",
    ] {
        let path = write(directory.path(), "indexed.fa", b">chr1\nACGTACGT\n");
        write(directory.path(), "indexed.fa.fai", index);
        let _ = fasta_outcome(&path);
    }
    assert!(FastaReference::open(directory.path().join("missing.fa")).is_err());
    assert!(FastaReference::open(directory.path()).is_err());
}

#[cfg(unix)]
#[test]
fn fasta_whose_index_cannot_be_written_is_a_clean_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let locked = directory.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    let fasta = write(&locked, "ok.fa", b">chr1\nACGTACGT\n");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::write(locked.join("probe"), b"").is_ok();
    let outcome = fasta_outcome(&fasta);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !writable {
        assert!(outcome.is_err());
    }
}

fn write_bam(path: &Path, format: bam::Format) {
    let mut header = bam::Header::new();
    let mut line = bam::header::HeaderRecord::new(b"SQ");
    line.push_tag(b"SN", "chr1");
    line.push_tag(b"LN", 1000);
    header.push_record(&line);
    let mut writer = bam::Writer::from_path(path, &header, format).unwrap();
    let mut record = bam::Record::new();
    record.set(
        b"read1",
        Some(&bam::record::CigarString(vec![bam::record::Cigar::Match(
            4,
        )])),
        b"ACGT",
        &[30, 30, 30, 30],
    );
    record.set_tid(0);
    record.set_pos(10);
    record.set_mapq(60);
    record.set_mtid(-1);
    record.set_mpos(-1);
    writer.write(&record).unwrap();
}

#[test]
fn alignments_without_an_index_are_clean_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    let options = AlignmentOpenOptions::default();
    let bam_path = directory.path().join("unindexed.bam");
    write_bam(&bam_path, bam::Format::Bam);
    assert!(LocalAlignmentSource::open(&bam_path, &options).is_err());
    assert!(ProductionAlignmentSource::open(&bam_path, &options).is_err());
    // The same file is served once it is loaded into memory.
    let in_memory = ProductionAlignmentSource::open_in_memory(&bam_path, &options).unwrap();
    assert_eq!(in_memory.reference_dictionary().iter().count(), 1);
    let sam_path = directory.path().join("plain.sam");
    write_bam(&sam_path, bam::Format::Sam);
    assert!(LocalAlignmentSource::open(&sam_path, &options).is_err());
    assert!(ProductionAlignmentSource::open(&sam_path, &options).is_ok());

    let bytes = std::fs::read(&bam_path).unwrap();
    let cases: [(&str, &[u8]); 6] = [
        ("empty.bam", b""),
        ("garbage.bam", b"this is not an alignment file\n"),
        ("truncated.bam", &bytes[..bytes.len() / 2]),
        (
            "vcf.bam",
            b"##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n",
        ),
        ("gzip.bam", &gzip(b"not BAM")),
        ("magic.cram", b"CRAM\x03\x00"),
    ];
    for (name, content) in cases {
        let path = write(directory.path(), name, content);
        assert!(
            LocalAlignmentSource::open(&path, &options).is_err(),
            "{name}"
        );
        let _ = ProductionAlignmentSource::open(&path, &options);
        let _ = ProductionAlignmentSource::open_in_memory(&path, &options);
    }
    // An index that is not one, beside a good file and named explicitly.
    for index in [
        &b""[..],
        b"not an index",
        b"BAI\x01",
        b"BAI\x01\x02\x00\x00\x00",
    ] {
        let index_path = write(directory.path(), "unindexed.bam.bai", index);
        let _ = LocalAlignmentSource::open(&bam_path, &options);
        let _ = ProductionAlignmentSource::open(&bam_path, &options);
        let explicit = AlignmentOpenOptions {
            index_path: Some(index_path.clone()),
            ..AlignmentOpenOptions::default()
        };
        let _ = LocalAlignmentSource::open(&bam_path, &explicit);
        std::fs::remove_file(&index_path).unwrap();
    }
    assert!(LocalAlignmentSource::open(directory.path().join("missing.bam"), &options).is_err());
    assert!(LocalAlignmentSource::open(directory.path(), &options).is_err());
}
