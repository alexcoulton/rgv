//! Remote tabix- or CSI-indexed text annotations must answer exactly as the local `HTSlib`
//! tabix path does, while every byte goes through the observable range reader.

use std::{
    collections::HashMap,
    io::Write as _,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use noodles::{
    bgzf,
    core::Position,
    csi::{
        self,
        binning_index::{
            Indexer,
            index::{header::Builder as TabixHeaderBuilder, reference_sequence::bin::Chunk},
        },
    },
};
use tiny_http::{Header as HttpHeader, Response, Server, StatusCode};

use super::*;
use crate::{HttpRangeConfig, RemoteAuthentication, RemoteCancellation, RemoteTransferBudget};

const SECRET: &str = "Bearer annotation-secret";

#[derive(Clone, Debug)]
struct LoggedRequest {
    path: String,
    range: Option<(usize, usize)>,
    authorized: bool,
}

/// Serves byte ranges of named objects and records every request.
struct RangeServer {
    base: String,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<LoggedRequest>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RangeServer {
    fn start(objects: HashMap<String, Vec<u8>>, require_authorization: bool) -> Self {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_log = Arc::clone(&log);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                let path = request
                    .url()
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let authorized = request.headers().iter().any(|header| {
                    header.field.equiv("Authorization") && header.value.as_str() == SECRET
                });
                let data = objects.get(&path);
                let range = data.and_then(|data| {
                    let header = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))?;
                    let (start, end) = header
                        .value
                        .as_str()
                        .strip_prefix("bytes=")?
                        .split_once('-')?;
                    let end = end.parse::<usize>().ok()?.min(data.len().checked_sub(1)?);
                    Some((start.parse::<usize>().ok()?, end))
                });
                worker_log.lock().unwrap().push(LoggedRequest {
                    path,
                    range,
                    authorized,
                });
                if require_authorization && !authorized {
                    request.respond(Response::empty(StatusCode(401))).unwrap();
                    continue;
                }
                let (Some(data), Some((start, end))) = (data, range) else {
                    request.respond(Response::empty(StatusCode(404))).unwrap();
                    continue;
                };
                let response = Response::from_data(data[start..=end].to_vec())
                    .with_status_code(StatusCode(206))
                    .with_header(
                        HttpHeader::from_bytes(
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", data.len()),
                        )
                        .unwrap(),
                    )
                    .with_header(HttpHeader::from_bytes("ETag", "\"annotations-v1\"").unwrap());
                request.respond(response).unwrap();
            }
        });
        Self {
            base: format!("http://{address}"),
            stop,
            log,
            worker: Some(worker),
        }
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }

    fn log(&self) -> Vec<LoggedRequest> {
        self.log.lock().unwrap().clone()
    }

    fn bytes_served(&self, name: &str) -> usize {
        self.log()
            .iter()
            .filter(|request| request.path == format!("/{name}"))
            .filter_map(|request| request.range)
            .map(|(start, end)| end - start + 1)
            .sum()
    }
}

impl Drop for RangeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn config() -> HttpRangeConfig {
    HttpRangeConfig {
        chunk_size: NonZeroUsize::new(4_096).unwrap(),
        maximum_cache_bytes: NonZeroUsize::new(16 * 4_096).unwrap(),
        maximum_retries: 0,
        ..HttpRangeConfig::default()
    }
}

fn authorized_config() -> HttpRangeConfig {
    HttpRangeConfig {
        authentication: RemoteAuthentication::Bearer("annotation-secret".to_owned()),
        ..config()
    }
}

/// Which columns of a line hold its sequence name and one-based inclusive interval.
#[derive(Clone, Copy)]
enum Layout {
    Bed,
    Gff,
}

/// Writes `lines` as a bgzip-compressed file with a CSI index carrying the tabix header, as
/// `tabix --csi -p bed|gff` does. A new BGZF block starts every `lines_per_block` lines, so a
/// small region lies in few blocks and some features span a block boundary.
///
/// As in an index built by `HTSlib`, a record's chunk starts where the previous indexed
/// record ended, so a comment line in the middle of the data belongs to the chunk of the
/// record after it.
fn write_indexed(path: &Path, lines: &[String], layout: Layout, lines_per_block: usize) {
    let mut names = Vec::<String>::new();
    let mut writer = bgzf::io::Writer::new(std::fs::File::create(path).unwrap());
    let mut indexer = Indexer::default();
    let mut previous_end = None;
    for (number, line) in lines.iter().enumerate() {
        if number > 0 && number % lines_per_block == 0 {
            writer.flush().unwrap();
        }
        let line_start = writer.virtual_position();
        writeln!(writer, "{line}").unwrap();
        let end = writer.virtual_position();
        if line.starts_with('#') {
            continue;
        }
        let start = previous_end.unwrap_or(line_start);
        previous_end = Some(end);
        let fields = line.split('\t').collect::<Vec<_>>();
        let (first, last) = match layout {
            Layout::Bed => (
                fields[1].parse::<usize>().unwrap() + 1,
                fields[2].parse::<usize>().unwrap(),
            ),
            Layout::Gff => (fields[3].parse().unwrap(), fields[4].parse().unwrap()),
        };
        let reference = names
            .iter()
            .position(|name| name == fields[0])
            .unwrap_or_else(|| {
                names.push(fields[0].to_owned());
                names.len() - 1
            });
        indexer
            .add_record(
                Some((
                    reference,
                    Position::new(first).unwrap(),
                    Position::new(last.max(first)).unwrap(),
                    true,
                )),
                Chunk::new(start, end),
            )
            .unwrap();
    }
    writer.finish().unwrap();
    let header = match layout {
        Layout::Bed => TabixHeaderBuilder::bed(),
        Layout::Gff => TabixHeaderBuilder::gff(),
    }
    .set_reference_sequence_names(names.iter().map(|name| name.as_bytes().into()).collect())
    .build();
    let index: csi::Index = indexer.set_header(header).build(names.len());
    csi::fs::write(format!("{}.csi", path.display()), &index).unwrap();
}

/// A BED file of many features on two contigs, including long ones that overlap regions far
/// from where they start and zero-score neighbours sharing a start.
fn bed_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for contig in ["chr1", "chr2"] {
        for index in 0..6_000_u64 {
            let start = index * 500;
            let length = if index % 97 == 0 {
                40_000
            } else {
                120 + index % 300
            };
            lines.push(format!(
                "{contig}\t{start}\t{}\tfeature_{contig}_{index}\t{}\t{}",
                start + length,
                index % 1_000,
                if index % 2 == 0 { '+' } else { '-' }
            ));
            if index % 211 == 0 {
                lines.push(format!(
                    "{contig}\t{start}\t{}\ttwin_{contig}_{index}\t0\t+",
                    start + 60
                ));
            }
        }
    }
    lines
}

const BED_REGIONS: &[(&str, u64, u64)] = &[
    ("chr1", 0, 1),
    ("chr1", 0, 500),
    ("chr1", 499, 501),
    ("chr1", 120_000, 121_000),
    ("chr1", 1_000_000, 1_250_000),
    ("chr1", 2_999_000, 3_100_000),
    ("chr1", 5_000_000, 6_000_000),
    ("chr2", 0, 3_000_000),
    ("chr2", 48_400, 48_600),
    ("chr2", 2_990_000, 2_990_001),
    ("chr3", 0, 1_000),
];

fn assert_sources_agree(
    local: &TextFeatureSource,
    remote: &TextFeatureSource,
    regions: &[(&str, u64, u64)],
) {
    let mut local_contigs = local.contigs().collect::<Vec<_>>();
    let mut remote_contigs = remote.contigs().collect::<Vec<_>>();
    local_contigs.sort_unstable();
    remote_contigs.sort_unstable();
    assert_eq!(local_contigs, remote_contigs);
    for &(contig, start, end) in regions {
        let expected = local.query(contig, start, end);
        let actual = remote.query(contig, start, end);
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(expected, actual, "{contig}:{start}-{end}");
            }
            (Err(_), Err(_)) => {}
            (expected, actual) => panic!(
                "{contig}:{start}-{end}: local {:?}, remote {:?}",
                expected.map(|records| records.len()),
                actual.map(|records| records.len())
            ),
        }
        for position in [start, end, u64::midpoint(start, end)] {
            for forward in [true, false] {
                assert_eq!(
                    local.adjacent_feature(contig, position, forward),
                    remote.adjacent_feature(contig, position, forward),
                    "adjacent to {contig}:{position}, forward {forward}"
                );
            }
        }
    }
}

struct BedFixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    server: RangeServer,
}

impl BedFixture {
    fn start(require_authorization: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("features.bed.gz");
        write_indexed(&path, &bed_lines(), Layout::Bed, 64);
        let server = RangeServer::start(
            HashMap::from([
                ("/features.bed.gz".to_owned(), std::fs::read(&path).unwrap()),
                (
                    "/features.bed.gz.csi".to_owned(),
                    std::fs::read(format!("{}.csi", path.display())).unwrap(),
                ),
            ]),
            require_authorization,
        );
        Self {
            _directory: directory,
            path,
            server,
        }
    }
}

#[test]
fn remote_indexed_bed_matches_the_local_tabix_path_and_reads_only_what_a_region_needs() {
    let fixture = BedFixture::start(true);
    let local = TextFeatureSource::open(&fixture.path, FeatureFormat::Bed).unwrap();
    assert!(local.is_indexed());
    assert!(local.remote_io_stats().is_none());
    let working_directory = std::env::current_dir().unwrap();
    let before = std::fs::read_dir(&working_directory).unwrap().count();

    let remote = TextFeatureSource::open_with_remote_config(
        fixture.server.url("features.bed.gz"),
        FeatureFormat::Bed,
        &authorized_config(),
    )
    .unwrap();
    assert!(remote.is_indexed());
    // Opening transferred the index and the first block, not the file.
    let file_bytes = std::fs::metadata(&fixture.path).unwrap().len() as usize;
    assert!(file_bytes > 100_000, "{file_bytes}");
    assert!(fixture.server.bytes_served("features.bed.gz") <= 8_192);

    // One small region costs a few blocks.
    let records = remote.query("chr2", 48_400, 48_600).unwrap();
    assert_eq!(
        records,
        local.query("chr2", 48_400, 48_600).unwrap(),
        "small region"
    );
    assert!(!records.is_empty());
    let served = fixture.server.bytes_served("features.bed.gz");
    assert!(
        served < file_bytes / 4,
        "{served} of {file_bytes} bytes were transferred for one small region"
    );

    assert_sources_agree(&local, &remote, BED_REGIONS);

    // Every request carried the credentials and asked for a byte range.
    let log = fixture.server.log();
    assert!(log.iter().all(|request| request.authorized));
    assert!(
        log.iter()
            .filter(|request| request.path == "/features.bed.gz")
            .all(|request| request.range.is_some())
    );
    let stats = remote.remote_io_stats().unwrap();
    assert!(stats.requests > 0 && stats.response_bytes > 0);

    // Name search streams the file through the same reader and agrees with the local index.
    for term in [
        "feature_chr1_97",
        "twin_chr2_211",
        "feature_chr2_59",
        "absent",
    ] {
        assert_eq!(
            local.search(term, 20),
            remote.search(term, 20),
            "search for {term}"
        );
    }
    assert!(!remote.search("feature_chr1_97", 20).hits.is_empty());

    assert_eq!(
        std::fs::read_dir(&working_directory).unwrap().count(),
        before,
        "something was written into {}",
        working_directory.display()
    );
}

#[test]
fn remote_indexed_annotations_honour_authentication_cancellation_and_budgets() {
    let fixture = BedFixture::start(true);
    // Without credentials the open fails; it does not fall back to anything.
    let Err(error) = TextFeatureSource::open_with_remote_config(
        fixture.server.url("features.bed.gz"),
        FeatureFormat::Bed,
        &config(),
    ) else {
        panic!("an unauthenticated open succeeded");
    };
    assert!(error.to_string().contains("401"), "{error}");
    assert!(
        fixture
            .server
            .log()
            .iter()
            .all(|request| !request.authorized)
    );

    let mut remote = TextFeatureSource::open_with_remote_config(
        fixture.server.url("features.bed.gz"),
        FeatureFormat::Bed,
        &authorized_config(),
    )
    .unwrap();
    let local = TextFeatureSource::open(&fixture.path, FeatureFormat::Bed).unwrap();

    let cancellation: RemoteCancellation = Arc::new(|| false);
    remote.set_remote_cancellation(Some(cancellation));
    let before = fixture.server.log().len();
    let cancelled = remote.query("chr1", 1_000_000, 1_250_000).unwrap_err();
    assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
    assert_eq!(fixture.server.log().len(), before);
    assert_eq!(remote.remote_io_stats().unwrap().cancellations, 1);
    remote.set_remote_cancellation(None);

    remote.set_remote_transfer_budget(Some(RemoteTransferBudget {
        maximum_requests: 0,
        maximum_response_bytes: 0,
    }));
    let over_budget = remote.query("chr1", 2_000_000, 2_250_000).unwrap_err();
    assert!(over_budget.to_string().contains("budget"), "{over_budget}");
    assert_eq!(fixture.server.log().len(), before);
    remote.set_remote_transfer_budget(None);

    assert_eq!(
        remote.query("chr1", 1_000_000, 1_250_000).unwrap(),
        local.query("chr1", 1_000_000, 1_250_000).unwrap()
    );
}

#[test]
fn remote_tabix_indexed_gtf_matches_the_local_path() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/indexed.gtf.gz");
    let server = RangeServer::start(
        HashMap::from([
            (
                "/data/genes.gtf.gz".to_owned(),
                std::fs::read(&path).unwrap(),
            ),
            (
                "/data/genes.gtf.gz.tbi".to_owned(),
                std::fs::read(format!("{}.tbi", path.display())).unwrap(),
            ),
            (
                "/indexes/genes.tbi".to_owned(),
                std::fs::read(format!("{}.tbi", path.display())).unwrap(),
            ),
            (
                "/bare/genes.gtf.gz".to_owned(),
                std::fs::read(&path).unwrap(),
            ),
        ]),
        false,
    );
    let local = TextFeatureSource::open(&path, FeatureFormat::Gtf).unwrap();
    let regions = [
        ("chr1", 0, 6_000),
        ("chr1", 2_000, 2_001),
        ("chr1", 5_000, 6_000),
        ("chr1", 1_000_000, 1_001_000),
        ("chr1", 0, 250_000_000),
        ("chr2", 0, 250_000_000),
        ("chr3", 0, 1_000),
    ];

    // The conventional `.tbi` beside the file (the `.csi` probe answers 404 first).
    let remote = TextFeatureSource::open_with_remote_config(
        server.url("data/genes.gtf.gz"),
        FeatureFormat::Gtf,
        &config(),
    )
    .unwrap();
    assert!(remote.is_indexed());
    assert_sources_agree(&local, &remote, &regions);
    for term in ["abc", "gene", "ENST", "tx"] {
        assert_eq!(local.search(term, 50), remote.search(term, 50), "{term}");
    }

    // An explicit index URL for a file whose index lives elsewhere.
    let explicit = TextFeatureSource::open_with_remote_index_and_config(
        server.url("bare/genes.gtf.gz"),
        FeatureFormat::Gtf,
        Some(&server.url("indexes/genes.tbi")),
        &config(),
    )
    .unwrap();
    assert!(explicit.is_indexed());
    assert_sources_agree(&local, &explicit, &regions);
    // An explicit index that does not exist is an error, not a silent full download.
    assert!(
        TextFeatureSource::open_with_remote_index_and_config(
            server.url("bare/genes.gtf.gz"),
            FeatureFormat::Gtf,
            Some(&server.url("indexes/absent.tbi")),
            &config(),
        )
        .is_err()
    );
}

#[test]
fn a_remote_annotation_without_an_index_is_still_downloaded_whole() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("plain.bed.gz");
    let lines = bed_lines().into_iter().take(400).collect::<Vec<_>>();
    write_indexed(&path, &lines, Layout::Bed, 64);
    let text = lines.join("\n") + "\n";
    let server = RangeServer::start(
        HashMap::from([
            ("/plain.bed.gz".to_owned(), std::fs::read(&path).unwrap()),
            ("/plain.bed".to_owned(), text.clone().into_bytes()),
        ]),
        false,
    );
    let local = TextFeatureSource::from_reader(text.as_bytes(), FeatureFormat::Bed).unwrap();
    let regions = [("chr1", 0, 10_000), ("chr1", 100_000, 250_000)];

    // Compressed, no index on the server: both index names are probed, then the file is read
    // whole.
    let compressed = TextFeatureSource::open_with_remote_config(
        server.url("plain.bed.gz"),
        FeatureFormat::Bed,
        &config(),
    )
    .unwrap();
    assert!(!compressed.is_indexed());
    assert!(compressed.remote_io_stats().is_none());
    let probed = server
        .log()
        .iter()
        .map(|request| request.path.clone())
        .collect::<Vec<_>>();
    assert!(
        probed.contains(&"/plain.bed.gz.csi".to_owned()),
        "{probed:?}"
    );
    assert!(
        probed.contains(&"/plain.bed.gz.tbi".to_owned()),
        "{probed:?}"
    );
    for (contig, start, end) in regions {
        assert_eq!(
            compressed.query(contig, start, end).unwrap(),
            local.query(contig, start, end).unwrap()
        );
    }

    // Uncompressed text cannot be tabix-indexed: no index is asked for.
    let before = server.log().len();
    let plain = TextFeatureSource::open_with_remote_config(
        server.url("plain.bed"),
        FeatureFormat::Bed,
        &config(),
    )
    .unwrap();
    assert!(!plain.is_indexed());
    assert!(
        server.log()[before..]
            .iter()
            .all(|request| request.path == "/plain.bed")
    );
    for (contig, start, end) in regions {
        assert_eq!(
            plain.query(contig, start, end).unwrap(),
            local.query(contig, start, end).unwrap()
        );
    }
}

/// GFF3 with a `###` separator and an unparseable feature line in the middle of the data:
/// whatever the local tabix path makes of a region containing them, the remote path makes
/// the same of it.
#[test]
fn remote_indexed_gff_treats_interior_comment_lines_as_the_local_path_does() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("genes.gff3.gz");
    let mut lines = vec!["##gff-version 3".to_owned()];
    for index in 0..300_u64 {
        let start = 1 + index * 1_000;
        lines.push(format!(
            "chr1\ttest\tgene\t{start}\t{}\t.\t+\t.\tID=gene{index};Name=G{index}",
            start + 799
        ));
        if index == 150 {
            lines.push("###".to_owned());
        }
    }
    write_indexed(&path, &lines, Layout::Gff, 32);
    let server = RangeServer::start(
        HashMap::from([
            ("/genes.gff3.gz".to_owned(), std::fs::read(&path).unwrap()),
            (
                "/genes.gff3.gz.csi".to_owned(),
                std::fs::read(format!("{}.csi", path.display())).unwrap(),
            ),
        ]),
        false,
    );
    let local = TextFeatureSource::open(&path, FeatureFormat::Gff3).unwrap();
    let remote = TextFeatureSource::open_with_remote_config(
        server.url("genes.gff3.gz"),
        FeatureFormat::Gff3,
        &config(),
    )
    .unwrap();
    assert!(local.is_indexed() && remote.is_indexed());
    let regions = [
        ("chr1", 0, 5_000),
        ("chr1", 10_000, 20_500),
        ("chr1", 149_000, 153_000),
        ("chr1", 150_000, 150_001),
        ("chr1", 0, 300_000),
        ("chr1", 299_000, 310_000),
    ];
    assert!(!local.query("chr1", 0, 5_000).unwrap().is_empty());
    assert_sources_agree(&local, &remote, &regions);
    // What that is today: `HTSlib` abandons a region whose chunk holds the `###` line (it
    // cannot read coordinates from it), and so does the remote path; other regions are fine.
    assert!(local.query("chr1", 149_000, 153_000).is_err());
    assert!(remote.query("chr1", 149_000, 153_000).is_err());
    assert_eq!(remote.query("chr1", 10_000, 20_500).unwrap().len(), 11);
}

/// A 300 kb minus-strand transcript with an unrelated gene inside one of its introns, as GTF
/// lines sorted for tabix: the lines of `tests/fixtures/long_transcript.gtf.gz`.
fn long_transcript_gtf() -> Vec<String> {
    let line = |kind: &str, start: u64, end: u64, attributes: &str| {
        format!("chr1\tsrc\t{kind}\t{start}\t{end}\t.\t-\t.\t{attributes}")
    };
    let long = "gene_id \"G1\"; transcript_id \"T1\"; gene_name \"LONG\";";
    let nested = "gene_id \"G2\"; transcript_id \"T2\"; gene_name \"NESTED\";";
    let mut lines = vec![
        line(
            "gene",
            100_001,
            400_000,
            "gene_id \"G1\"; gene_name \"LONG\";",
        ),
        line("transcript", 100_001, 400_000, long),
        line("exon", 100_001, 101_000, long),
        line("UTR", 100_001, 100_500, long),
        line("stop_codon", 100_501, 100_503, long),
        line("CDS", 100_504, 101_000, long),
        line("exon", 200_001, 201_000, long),
        line("CDS", 200_001, 201_000, long),
        line(
            "gene",
            250_001,
            251_000,
            "gene_id \"G2\"; gene_name \"NESTED\";",
        ),
        line("transcript", 250_001, 251_000, nested),
        line("exon", 250_001, 251_000, nested),
        line("exon", 300_001, 301_000, long),
        line("CDS", 300_001, 300_700, long),
        line("start_codon", 300_698, 300_700, long),
        line("UTR", 300_701, 301_000, long),
        line("exon", 399_001, 400_000, long),
        line("UTR", 399_001, 400_000, long),
    ];
    lines.sort_by_key(|line| line.split('\t').nth(3).unwrap().parse::<u64>().unwrap());
    lines
}

/// The fixture opened through its index and loaded whole, and the long transcript's model.
fn long_transcript_sources() -> (PathBuf, TextFeatureSource, TextFeatureSource, FeatureRecord) {
    // Indexed by `tabix -p gff` (an index built here would not carry HTSlib's linear offsets).
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/long_transcript.gtf.gz");
    let lines = long_transcript_gtf();
    let mut text = String::new();
    MultiGzDecoder::new(std::fs::File::open(&path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(
        text,
        lines.join("\n") + "\n",
        "the fixture holds these lines"
    );
    let indexed = TextFeatureSource::open(&path, FeatureFormat::Gtf).unwrap();
    assert!(indexed.is_indexed());
    let loaded = TextFeatureSource::from_reader(text.as_bytes(), FeatureFormat::Gtf).unwrap();
    let whole = loaded.query("chr1", 0, 1_000_000).unwrap();
    assert_eq!(whole.len(), 2, "{whole:#?}");
    let expected = whole
        .into_iter()
        .find(|record| record.name.as_deref() == Some("LONG"))
        .unwrap();
    assert_eq!((expected.start, expected.end), (100_000, 400_000));
    assert_eq!(
        expected.blocks,
        [
            (100_000, 101_000),
            (200_000, 201_000),
            (300_000, 301_000),
            (399_000, 400_000)
        ]
    );
    assert_eq!(expected.coding_interval, Some((100_500, 300_700)));
    (path, indexed, loaded, expected)
}

#[test]
fn a_window_of_an_indexed_gtf_returns_the_whole_transcript_with_a_stable_identity() {
    let (path, indexed, loaded, expected) = long_transcript_sources();
    let identity = rgv_model::track_tile::TrackRecordId::for_feature(&expected);

    // Windows that hold one exon, only intron, or only the nested gene return the same
    // complete model: the read is widened to the extent of the transcript lines it meets.
    for (start, end) in [
        (100_200, 100_300),
        (200_500, 200_600),
        (150_000, 150_010),
        (250_100, 250_200),
        (399_990, 400_000),
        (0, 1_000_000),
    ] {
        let records = indexed.query("chr1", start, end).unwrap();
        let long = records
            .iter()
            .filter(|record| record.name.as_deref() == Some("LONG"))
            .collect::<Vec<_>>();
        assert_eq!(long, [&expected], "{start}-{end}");
        assert_eq!(
            rgv_model::track_tile::TrackRecordId::for_feature(long[0]),
            identity
        );
        // Only what overlaps the window: the nested gene's model appears only where it is.
        let nested = records
            .iter()
            .any(|record| record.name.as_deref() == Some("NESTED"));
        assert_eq!(nested, start < 251_000 && end > 250_000, "{start}-{end}");
        assert_eq!(records, loaded.query("chr1", start, end).unwrap());
    }
    // Next and previous feature step over the models, not over their exons.
    let next = indexed.adjacent_feature("chr1", 100_001, true).unwrap();
    assert_eq!(next.name.as_deref(), Some("NESTED"));
    assert_eq!(
        next,
        loaded.adjacent_feature("chr1", 100_001, true).unwrap()
    );
    let previous = indexed.adjacent_feature("chr1", 260_000, false).unwrap();
    assert_eq!(previous.name.as_deref(), Some("NESTED"));
    assert!(indexed.adjacent_feature("chr1", 250_001, false).is_none());

    // A remote copy of the same file answers the same.
    let server = RangeServer::start(
        HashMap::from([
            ("/genes.gtf.gz".to_owned(), std::fs::read(&path).unwrap()),
            (
                "/genes.gtf.gz.tbi".to_owned(),
                std::fs::read(format!("{}.tbi", path.display())).unwrap(),
            ),
        ]),
        false,
    );
    let remote = TextFeatureSource::open_with_remote_config(
        server.url("genes.gtf.gz"),
        FeatureFormat::Gtf,
        &config(),
    )
    .unwrap();
    assert!(remote.is_indexed());
    assert_sources_agree(
        &indexed,
        &remote,
        &[
            ("chr1", 150_000, 150_010),
            ("chr1", 250_100, 250_200),
            ("chr1", 0, 1_000_000),
        ],
    );
}

#[test]
fn a_window_beyond_the_widening_limit_keeps_the_transcript_span_and_identity() {
    let (_, indexed, _, expected) = long_transcript_sources();
    let identity = rgv_model::track_tile::TrackRecordId::for_feature(&expected);
    // Without widening (a transcript reaching further than the widening limit), a window holds
    // only some of the exons; the model still has its transcript line's span and the same
    // identity.
    let backend = indexed.indexed.as_ref().unwrap();
    for (start, end, blocks) in [
        (100_200, 100_300, vec![(100_000, 101_000)]),
        (
            199_000,
            300_100,
            vec![(200_000, 201_000), (300_000, 301_000)],
        ),
    ] {
        let records = backend.query_within_reach("chr1", start, end, 0).unwrap();
        let long = records
            .iter()
            .find(|record| record.name.as_deref() == Some("LONG"))
            .unwrap_or_else(|| panic!("{start}-{end}: {records:#?}"));
        assert_eq!((long.start, long.end), (100_000, 400_000), "{start}-{end}");
        assert_eq!(long.feature_type.as_deref(), Some("transcript"));
        assert_eq!(long.blocks, blocks, "{start}-{end}");
        assert_eq!(
            rgv_model::track_tile::TrackRecordId::for_feature(long),
            identity,
            "{start}-{end}"
        );
    }
}

/// A windowed read of an indexed GTF without gene lines chooses each gene's representative
/// from all its transcripts, however little of the gene the window holds, exactly as the
/// whole-file load does.
#[test]
fn indexed_windows_choose_the_representative_from_every_transcript_of_the_gene() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("genes.gtf.gz");
    let line = |id: &str, kind: &str, start: u64, end: u64, extra: &str| {
        format!(
            "chr1\tsrc\t{kind}\t{start}\t{end}\t.\t+\t.\tgene_id \"G1\"; transcript_id \"{id}\"; gene_name \"GENE1\";{extra}"
        )
    };
    // T_MANE lies far left of the window used below; T_LONG bridges to it; T_RIGHT lies inside.
    let mut lines = vec![
        line(
            "T_MANE",
            "transcript",
            1_001,
            2_000,
            " tag \"MANE_Select\";",
        ),
        line("T_MANE", "exon", 1_001, 2_000, ""),
        line("T_LONG", "transcript", 1_501, 60_000, ""),
        line("T_LONG", "exon", 1_501, 1_800, ""),
        line("T_LONG", "exon", 59_001, 60_000, ""),
        line("T_RIGHT", "transcript", 59_501, 60_000, ""),
        line("T_RIGHT", "exon", 59_501, 60_000, ""),
    ];
    lines.sort_by_key(|line| line.split('\t').nth(3).unwrap().parse::<u64>().unwrap());
    write_indexed(&path, &lines, Layout::Gff, 2);
    let indexed = TextFeatureSource::open(&path, FeatureFormat::Gtf).unwrap();
    assert!(indexed.is_indexed());
    let loaded =
        TextFeatureSource::from_reader(lines.join("\n").as_bytes(), FeatureFormat::Gtf).unwrap();
    let choices = |records: Vec<FeatureRecord>| {
        records
            .iter()
            .map(|record| {
                let choice = record.transcript_choice().unwrap();
                (
                    record.transcript_identifier().unwrap().to_owned(),
                    choice.rank,
                    choice.count,
                    choice.representative.to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let window = choices(indexed.query("chr1", 59_600, 59_700).unwrap());
    assert_eq!(
        window,
        [
            ("T_LONG".to_owned(), 1, 3, "T_MANE".to_owned()),
            ("T_RIGHT".to_owned(), 2, 3, "T_MANE".to_owned()),
        ]
    );
    assert_eq!(
        window,
        choices(loaded.query("chr1", 59_600, 59_700).unwrap())
    );
    let left = choices(indexed.query("chr1", 1_100, 1_200).unwrap());
    assert_eq!(left, [("T_MANE".to_owned(), 0, 3, "T_MANE".to_owned())]);
    assert_eq!(left, choices(loaded.query("chr1", 1_100, 1_200).unwrap()));
}
