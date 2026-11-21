//! Remote bgzipped VCF must equal the local `HTSlib` path record for record, while every byte
//! goes through the observable range reader.

use std::{
    collections::HashMap,
    fmt::Write as _,
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

use rust_htslib::bcf::index;
use tiny_http::{Header as HttpHeader, Response, Server, StatusCode};

use super::*;
use crate::RemoteAuthentication;

const SECRET: &str = "Bearer variant-secret";

/// One request as the fixture server saw it.
#[derive(Clone, Debug)]
struct LoggedRequest {
    path: String,
    range: Option<(usize, usize)>,
    authorized: bool,
}

/// Serves byte ranges of several named objects and records every request.
struct VariantServer {
    base: String,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<LoggedRequest>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl VariantServer {
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
                    .with_header(HttpHeader::from_bytes("ETag", "\"variants-v1\"").unwrap());
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

    /// Bytes of `name` served so far.
    fn bytes_served(&self, name: &str) -> usize {
        self.log()
            .iter()
            .filter(|request| request.path == format!("/{name}"))
            .filter_map(|request| request.range)
            .map(|(start, end)| end - start + 1)
            .sum()
    }
}

impl Drop for VariantServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

/// Records whose region membership or decoding is easy to get wrong, as `(POS, line)`.
const SPECIAL_RECORDS: &[(u32, &str)] = &[
    // Multi-allelic, with a second and third record at the same position.
    (
        2_500_000,
        "chr1\t2500000\tmulti;alias-multi\tA\tG,T,AC\t99\tPASS\tDP=80;AF=0.1,0.2,0.05\tGT:DP:AD\t1/2:40:5,15,20,0\t0/3:38:20,0,0,18\t2|1:41:1,20,20,0",
    ),
    (
        2_500_000,
        "chr1\t2500000\tsame-pos-del\tACGT\tA\t50\tPASS\tDP=70\tGT:DP:AD\t0/1:30:15,15\t0/0:30:30,0\t0/1:30:14,16",
    ),
    (
        2_500_000,
        "chr1\t2500000\tsame-pos-star\tA\t*,C\t.\t.\tDP=12\tGT\t1/2\t./.\t0/2",
    ),
    // Symbolic alleles whose INFO/END reaches far beyond POS.
    (
        2_600_000,
        "chr1\t2600000\tsv-del\tN\t<DEL>\t.\tPASS\tSVTYPE=DEL;END=2650000;SVLEN=-50000;STRANDS=+-:12\tGT:DP\t0/1:20\t0/0:21\t0/1:19",
    ),
    (
        2_610_000,
        "chr1\t2610000\tsv-dup\tN\t<DUP>\t30\tlowqual\tSOMATIC;SVTYPE=DUP;END=2900000\tGT\t0/1\t0/1\t1/1",
    ),
    (
        2_620_000,
        "chr1\t2620000\tsv-inv\tN\t<INV>\t30\tPASS\tEND=2621000;SVTYPE=INV\tGT\t0/0\t0/1\t0/1",
    ),
    (
        2_630_000,
        "chr1\t2630000\tbnd-a\tG\tG]chr2:1500]\t60\tPASS\tSVTYPE=BND\tGT\t0/1\t0/0\t0/0",
    ),
    // A long plain deletion: REF itself spans the next query boundary.
    (
        2_699_990,
        "chr1\t2699990\tlong-ref\tACGTACGTACGTACGTACGTACGTACGTACGT\tA\t70\tPASS\tDP=33\tGT:DP:AD\t0/1:33:16,17\t0/0:30:30,0\t1/1:29:0,29",
    ),
    (
        2_700_100,
        "chr1\t2700100\t.\tC\tCTT\t45\tPASS\tDP=21;AF=0.5\tGT:DP:AD\t0/1:21:10,11\t0/1:20:10,10\t0/0:22:22,0",
    ),
    (
        4_999_999,
        "chr1\t4999999\tlast\tT\tC\t10\tPASS\tDP=5\tGT\t0/1\t0/1\t0/1",
    ),
];

/// A three-sample VCF large enough to span many BGZF blocks, with the record shapes whose
/// region membership or decoding is easy to get wrong.
fn cohort_vcf_text() -> String {
    cohort_text(true)
}

fn cohort_text(include_undeclared_contig: bool) -> String {
    let mut text = String::from(
        "##fileformat=VCFv4.3\n\
         ##FILTER=<ID=PASS,Description=\"All filters passed\">\n\
         ##FILTER=<ID=lowqual,Description=\"Low quality\">\n\
         ##contig=<ID=chr1,length=5000000>\n\
         ##contig=<ID=chr2,length=5000000>\n\
         ##INFO=<ID=DP,Number=1,Type=Integer,Description=\"Depth\">\n\
         ##INFO=<ID=AF,Number=A,Type=Float,Description=\"Allele frequency\">\n\
         ##INFO=<ID=END,Number=1,Type=Integer,Description=\"End\">\n\
         ##INFO=<ID=SVTYPE,Number=1,Type=String,Description=\"SV type\">\n\
         ##INFO=<ID=SVLEN,Number=.,Type=Integer,Description=\"SV length\">\n\
         ##INFO=<ID=STRANDS,Number=.,Type=String,Description=\"Strands\">\n\
         ##INFO=<ID=SOMATIC,Number=0,Type=Flag,Description=\"Somatic\">\n\
         ##ALT=<ID=DEL,Description=\"Deletion\">\n\
         ##ALT=<ID=DUP,Description=\"Duplication\">\n\
         ##ALT=<ID=INV,Description=\"Inversion\">\n\
         ##FORMAT=<ID=GT,Number=1,Type=String,Description=\"Genotype\">\n\
         ##FORMAT=<ID=DP,Number=1,Type=Integer,Description=\"Depth\">\n\
         ##FORMAT=<ID=AD,Number=R,Type=Integer,Description=\"Allele depth\">\n\
         #CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tmother\tfather\tchild\n",
    );
    let mut lines: Vec<(u32, String)> = Vec::new();
    // Dense SNVs: chr1:1,000-1,800,000 every 150 bp, leaving 1,800,000-2,400,000 empty.
    for index in 0..12_000_u32 {
        let position = 1_000 + index * 150;
        let genotypes = match index % 4 {
            0 => "0/1:30:15,15\t0/0:28:28,0\t1/1:31:0,31",
            1 => "0|1:22:11,11\t1|0:25:12,13\t./.:.:.",
            2 => "0/0:40:40,0\t0/1:35:20,15\t0/1:33:17,16",
            _ => "1/1:18:0,18\t0/1:.:.\t0|0:27:27,0",
        };
        let filter = if index % 9 == 0 { "lowqual" } else { "PASS" };
        lines.push((
            position,
            format!(
                "chr1\t{position}\trs{index}\tA\tG\t{}\t{filter}\tDP={};AF=0.{:02}\tGT:DP:AD\t{genotypes}",
                20 + index % 50,
                60 + index % 40,
                1 + index % 98,
            ),
        ));
    }
    lines.extend(
        SPECIAL_RECORDS
            .iter()
            .map(|(position, line)| (*position, (*line).to_owned())),
    );
    lines.sort_by_key(|(position, _)| *position);
    for (_, line) in lines {
        text.push_str(&line);
        text.push('\n');
    }
    for index in 0..40_u32 {
        writeln!(
            text,
            "chr2\t{}\tchr2-{index}\tC\tT\t33\tPASS\tDP=9\tGT:DP:AD\t0/1:9:4,5\t0/0:9:9,0\t0/1:9:5,4",
            1_000 + index * 25
        )
        .unwrap();
    }
    // A contig the header never declares: `HTSlib` learns it from the index.
    for index in 0..if include_undeclared_contig { 5_u32 } else { 0 } {
        writeln!(
            text,
            "chrUndeclared\t{}\tundeclared-{index}\tG\tA\t12\tPASS\tDP=3\tGT\t0/1\t0/0\t1/1",
            500 + index * 10
        )
        .unwrap();
    }
    text
}

/// Writes `text` as a BGZF file with the requested index next to it and returns the data and
/// index paths.
fn write_indexed_vcf(directory: &Path, text: &str, index_type: index::Type) -> (PathBuf, PathBuf) {
    let path = directory.join("cohort.vcf.gz");
    let mut writer = noodles::bgzf::io::Writer::new(std::fs::File::create(&path).unwrap());
    writer.write_all(text.as_bytes()).unwrap();
    writer.finish().unwrap();
    let suffix = match index_type {
        index::Type::Tbx => "tbi",
        index::Type::Csi(_) => "csi",
    };
    index::build(&path, None, 1, index_type).unwrap();
    let index_path = directory.join(format!("cohort.vcf.gz.{suffix}"));
    assert!(index_path.exists());
    (path, index_path)
}

/// Converts the cohort to an indexed BCF with `HTSlib` and returns the data and index paths.
fn write_indexed_bcf(directory: &Path) -> (PathBuf, PathBuf) {
    use rust_htslib::bcf::{Format, Header, Read as _, Reader, Writer};

    let text_path = directory.join("cohort.vcf");
    std::fs::write(&text_path, cohort_text(false)).unwrap();
    let path = directory.join("cohort.bcf");
    {
        let mut reader = Reader::from_path(&text_path).unwrap();
        let header = Header::from_template(reader.header());
        let mut writer = Writer::from_path(&path, &header, false, Format::Bcf).unwrap();
        for record in reader.records() {
            let mut record = record.unwrap();
            writer.translate(&mut record);
            writer.write(&record).unwrap();
        }
    }
    index::build(&path, None, 1, index::Type::Csi(14)).unwrap();
    let index_path = directory.join("cohort.bcf.csi");
    assert!(index_path.exists());
    (path, index_path)
}

struct CohortFixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    data_length: usize,
    server: VariantServer,
}

impl CohortFixture {
    fn start(index_type: index::Type, require_authorization: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (path, index_path) =
            write_indexed_vcf(directory.path(), &cohort_vcf_text(), index_type);
        let data = std::fs::read(&path).unwrap();
        let data_length = data.len();
        let index_name = index_path.file_name().unwrap().to_str().unwrap().to_owned();
        let server = VariantServer::start(
            HashMap::from([
                ("/cohort.vcf.gz".to_owned(), data),
                (
                    format!("/{index_name}"),
                    std::fs::read(&index_path).unwrap(),
                ),
            ]),
            require_authorization,
        );
        Self {
            _directory: directory,
            path,
            data_length,
            server,
        }
    }

    fn url(&self) -> String {
        self.server.url("cohort.vcf.gz")
    }
}

/// Small blocks and a small cache: distinct regions need distinct requests.
fn small_block_config() -> HttpRangeConfig {
    HttpRangeConfig {
        authentication: RemoteAuthentication::Bearer("variant-secret".to_owned()),
        chunk_size: NonZeroUsize::new(4_096).unwrap(),
        maximum_cache_bytes: NonZeroUsize::new(16 * 4_096).unwrap(),
        maximum_retries: 0,
        ..HttpRangeConfig::default()
    }
}

const REGIONS: &[(&str, u64, u64)] = &[
    // A handful of dense SNVs inside one BGZF block.
    ("chr1", 10_000, 12_000),
    // Hundreds of kilobases: many BGZF blocks.
    ("chr1", 200_000, 1_400_000),
    // No records at all.
    ("chr1", 1_900_000, 2_300_000),
    // Multi-allelic and same-position records.
    ("chr1", 2_499_990, 2_500_010),
    ("chr1", 2_499_999, 2_500_000),
    ("chr1", 2_500_000, 2_500_001),
    // Inside symbolic alleles only reached through INFO/END.
    ("chr1", 2_640_000, 2_645_000),
    ("chr1", 2_649_999, 2_650_000),
    ("chr1", 2_650_000, 2_650_001),
    ("chr1", 2_899_999, 2_900_050),
    // A REF allele spanning the start bound, and the first base after it.
    ("chr1", 2_700_000, 2_700_200),
    ("chr1", 2_700_020, 2_700_021),
    ("chr1", 2_700_021, 2_700_099),
    // Breakend, contig end, whole contig, degenerate and far-out-of-range intervals.
    ("chr1", 2_629_000, 2_631_000),
    ("chr1", 4_999_000, 5_000_000),
    ("chr1", 0, 5_000_000),
    ("chr1", 2_640_000, 2_640_000),
    ("chr1", 2_640_000, 100),
    ("chr1", 0, 0),
    ("chr1", 600_000_000, 700_000_000),
    ("chr1", 0, u64::MAX),
    ("chr2", 0, 5_000_000),
    ("chr2", 1_100, 1_300),
    ("chrUndeclared", 0, 1_000),
];

fn assert_remote_equals_local(index_type: index::Type) {
    let fixture = CohortFixture::start(index_type, false);
    let local = LocalVariantSource::open(&fixture.path).unwrap();
    let remote =
        LocalVariantSource::open_with_remote_config(fixture.url(), HttpRangeConfig::default())
            .unwrap();
    assert_sources_agree(local, remote, true);
}

fn assert_sources_agree(
    mut local: LocalVariantSource,
    mut remote: LocalVariantSource,
    has_undeclared_contig: bool,
) {
    assert!(remote.remote_io_stats().is_some());
    let filtered = VariantTrackSettings {
        genotype: VariantGenotypeFilter::Heterozygous,
        pass_only: true,
        sample_subset_enabled: true,
        sample_subset: vec!["child".to_owned(), "mother".to_owned()],
        ..VariantTrackSettings::default()
    };
    let mut total = 0_usize;
    for &(contig, start, end) in REGIONS {
        if contig == "chrUndeclared" && !has_undeclared_contig {
            continue;
        }
        for (settings, limit) in [
            (&VariantTrackSettings::default(), 100_000_usize),
            (&VariantTrackSettings::default(), 3),
            (&filtered, 100_000),
            (&filtered, 7),
        ] {
            let expected = local
                .query_with_settings(contig, start, end, limit, settings)
                .unwrap();
            let actual = remote
                .query_with_settings(contig, start, end, limit, settings)
                .unwrap();
            assert_eq!(
                actual, expected,
                "{contig}:{start}-{end} limit {limit} differs between remote and local"
            );
            total += expected.records.len();
        }
    }
    assert!(total > 10_000, "the regions must exercise real records");

    // The fixture regions really do cover the shapes they claim to.
    let empty = remote.query("chr1", 1_900_000, 2_300_000, 10).unwrap();
    assert!(empty.records.is_empty());
    let multi = remote.query("chr1", 2_499_999, 2_500_000, 10).unwrap();
    assert_eq!(multi.records.len(), 3);
    assert_eq!(multi.records[0].alternates.len(), 3);
    assert_eq!(multi.records[0].genotypes.len(), 3);
    let symbolic = remote.query("chr1", 2_640_000, 2_645_000, 10).unwrap();
    assert_eq!(
        symbolic
            .records
            .iter()
            .map(|record| record.variant_types[0].clone())
            .collect::<Vec<_>>(),
        [VariantType::StructuralDeletion, VariantType::Duplication]
    );
    assert_eq!(symbolic.records[0].end, 2_650_000);

    // Unknown contigs fail on both paths.
    assert!(local.query("chrMissing", 0, 10, 10).is_err());
    assert!(remote.query("chrMissing", 0, 10, 10).is_err());

    // The ID index is the same too.
    for prefix in [
        "rs1199",
        "multi",
        "alias",
        "sv-",
        "undeclared",
        "chr2-3",
        "",
    ] {
        assert_eq!(
            remote.search_ids(prefix, 50).unwrap(),
            local.search_ids(prefix, 50).unwrap(),
            "ID search for {prefix:?}"
        );
    }
    assert!(!remote.search_ids("rs1", 5).unwrap().index_truncated);
}

#[test]
fn remote_tabix_vcf_equals_local_htslib_for_every_region() {
    assert_remote_equals_local(index::Type::Tbx);
}

#[test]
fn remote_csi_vcf_equals_local_htslib_for_every_region() {
    assert_remote_equals_local(index::Type::Csi(14));
}

#[test]
fn remote_bcf_equals_local_htslib_for_every_region() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index_path) = write_indexed_bcf(directory.path());
    let server = VariantServer::start(
        HashMap::from([
            ("/cohort.bcf".to_owned(), std::fs::read(&path).unwrap()),
            (
                "/cohort.bcf.csi".to_owned(),
                std::fs::read(&index_path).unwrap(),
            ),
        ]),
        true,
    );
    assert!(LocalVariantSource::open(server.url("cohort.bcf")).is_err());
    let remote =
        LocalVariantSource::open_with_remote_config(server.url("cohort.bcf"), small_block_config())
            .unwrap();
    assert_sources_agree(LocalVariantSource::open(&path).unwrap(), remote, false);
    let log = server.log();
    assert!(log.iter().skip(1).all(|request| request.authorized));
    // Nothing was fetched by a second transport: every request is a logged range request.
    assert!(log.iter().all(|request| request.range.is_some()));
}

#[test]
fn remote_bcf_honours_cancellation_budget_and_a_bounded_id_scan() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index_path) = write_indexed_bcf(directory.path());
    let data = std::fs::read(&path).unwrap();
    let data_length = data.len();
    let server = VariantServer::start(
        HashMap::from([
            ("/cohort.bcf".to_owned(), data),
            (
                "/cohort.bcf.csi".to_owned(),
                std::fs::read(&index_path).unwrap(),
            ),
        ]),
        true,
    );
    let mut remote =
        LocalVariantSource::open_with_remote_config(server.url("cohort.bcf"), small_block_config())
            .unwrap();
    let cancellation: RemoteCancellation = Arc::new(|| false);
    remote.set_remote_cancellation(Some(cancellation));
    let before = server.log().len();
    let cancelled = remote
        .query("chr1", 200_000, 1_400_000, 100_000)
        .unwrap_err();
    assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
    assert_eq!(server.log().len(), before);
    remote.set_remote_cancellation(None);

    remote.set_remote_transfer_budget(Some(RemoteTransferBudget {
        maximum_requests: 0,
        maximum_response_bytes: 0,
    }));
    let over_budget = remote
        .query("chr1", 200_000, 1_400_000, 100_000)
        .unwrap_err();
    assert!(over_budget.to_string().contains("budget"), "{over_budget}");
    assert_eq!(server.log().len(), before);
    remote.set_remote_transfer_budget(None);

    let bytes_before_scan = server.bytes_served("cohort.bcf");
    remote.set_remote_id_scan_byte_limit(20_000);
    let results = remote.search_ids("rs", 5).unwrap();
    assert!(results.index_truncated);
    assert_eq!(results.hits.len(), 5);
    let scanned = server.bytes_served("cohort.bcf") - bytes_before_scan;
    assert!(
        (20_000..data_length / 2).contains(&scanned),
        "scan transferred {scanned} of {data_length} bytes"
    );
}

#[test]
fn fuzz_entry_points_accept_real_indexes_and_lines() {
    let directory = tempfile::tempdir().unwrap();
    for index_type in [index::Type::Tbx, index::Type::Csi(14)] {
        let (_, index_path) = write_indexed_vcf(directory.path(), &cohort_vcf_text(), index_type);
        assert!(crate::fuzzing::parse_variant_index(
            &std::fs::read(index_path).unwrap()
        ));
    }
    assert!(!crate::fuzzing::parse_variant_index(b"not an index"));
    assert_eq!(
        crate::fuzzing::variant_line_interval(b"chr1\t100\t.\tACG\tA\t.\t.\tEND=250"),
        Some((99, 250))
    );
    assert!(crate::fuzzing::parse_fasta_index(b"chr1\t30\t12\t20\t21\n"));
    assert!(!crate::fuzzing::parse_fasta_index(b"chr1\t30\n"));
    assert!(!crate::fuzzing::parse_cram_index(b"not gzip", 1_000));
}

#[test]
fn remote_vcf_fixture_spans_many_bgzf_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = write_indexed_vcf(directory.path(), &cohort_vcf_text(), index::Type::Tbx);
    let mut reader = noodles::bgzf::io::Reader::new(std::fs::File::open(path).unwrap());
    let mut blocks = std::collections::BTreeSet::new();
    let mut line = Vec::new();
    loop {
        blocks.insert(reader.virtual_position().compressed());
        line.clear();
        if std::io::BufRead::read_until(&mut reader, b'\n', &mut line).unwrap() == 0 {
            break;
        }
    }
    assert!(blocks.len() >= 8, "only {} BGZF blocks", blocks.len());
}

#[test]
fn remote_vcf_sends_authentication_on_every_request_and_fetches_only_part_of_the_file() {
    let fixture = CohortFixture::start(index::Type::Tbx, true);
    // The anonymous default is refused by the server.
    assert!(LocalVariantSource::open(fixture.url()).is_err());
    let anonymous_requests = fixture.server.log().len();
    assert!(anonymous_requests > 0);

    let mut remote =
        LocalVariantSource::open_with_remote_config(fixture.url(), small_block_config()).unwrap();
    let mut local = LocalVariantSource::open(&fixture.path).unwrap();
    assert_eq!(
        remote.query("chr1", 2_499_990, 2_700_200, 1_000).unwrap(),
        local.query("chr1", 2_499_990, 2_700_200, 1_000).unwrap()
    );
    let log = fixture.server.log();
    let authenticated = &log[anonymous_requests..];
    assert!(authenticated.len() >= 3);
    assert!(authenticated.iter().all(|request| request.authorized));
    assert!(
        authenticated
            .iter()
            .any(|request| request.path == "/cohort.vcf.gz.tbi")
    );
    // The header and one small region cost a fraction of the object, and the reader's own
    // accounting agrees with what the server sent.
    let served = fixture.server.bytes_served("cohort.vcf.gz");
    assert!(
        served < fixture.data_length / 3,
        "served {served} of {} bytes",
        fixture.data_length
    );
    let stats = remote.remote_io_stats().unwrap();
    let served_everything: usize = authenticated
        .iter()
        .filter_map(|request| request.range)
        .map(|(start, end)| end - start + 1)
        .sum();
    assert_eq!(stats.response_bytes, served_everything as u64);
    // The probe for an adjacent `.csi` was answered 404 and never became a reader.
    let missing_csi = authenticated
        .iter()
        .filter(|request| request.path == "/cohort.vcf.gz.csi")
        .count();
    assert_eq!(missing_csi, 1);
    assert_eq!(stats.requests, (authenticated.len() - missing_csi) as u64);
}

#[test]
fn remote_vcf_cancellation_and_budget_stop_fetching() {
    let fixture = CohortFixture::start(index::Type::Tbx, true);
    let mut remote =
        LocalVariantSource::open_with_remote_config(fixture.url(), small_block_config()).unwrap();

    let cancellation: RemoteCancellation = Arc::new(|| false);
    remote.set_remote_cancellation(Some(cancellation));
    let before = fixture.server.log().len();
    let cancelled = remote
        .query("chr1", 200_000, 1_400_000, 100_000)
        .unwrap_err();
    assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
    assert_eq!(fixture.server.log().len(), before);
    assert_eq!(remote.remote_io_stats().unwrap().cancellations, 1);

    // A generation cancelled part-way through a large region stops within one request.
    let keep_going = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&keep_going);
    let log = Arc::clone(&fixture.server.log);
    let cancellation: RemoteCancellation = Arc::new(move || {
        if log.lock().unwrap().len() >= before + 2 {
            flag.store(false, Ordering::Relaxed);
        }
        flag.load(Ordering::Relaxed)
    });
    remote.set_remote_cancellation(Some(cancellation));
    let cancelled = remote
        .query("chr1", 200_000, 1_400_000, 100_000)
        .unwrap_err();
    assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
    assert!(!keep_going.load(Ordering::Relaxed));
    let issued = fixture.server.log().len() - before;
    assert!(
        (2..=3).contains(&issued),
        "{issued} requests after cancellation"
    );

    remote.set_remote_cancellation(None);
    remote.set_remote_transfer_budget(Some(RemoteTransferBudget {
        maximum_requests: 0,
        maximum_response_bytes: 0,
    }));
    let before = fixture.server.log().len();
    let over_budget = remote
        .query("chr1", 1_500_000, 1_700_000, 100_000)
        .unwrap_err();
    assert!(over_budget.to_string().contains("budget"), "{over_budget}");
    assert_eq!(fixture.server.log().len(), before);

    remote.set_remote_transfer_budget(None);
    let mut local = LocalVariantSource::open(&fixture.path).unwrap();
    assert_eq!(
        remote.query("chr1", 200_000, 1_400_000, 100_000).unwrap(),
        local.query("chr1", 200_000, 1_400_000, 100_000).unwrap()
    );
}

#[test]
fn remote_vcf_id_search_is_bounded_cancellable_and_retried() {
    let fixture = CohortFixture::start(index::Type::Tbx, true);
    let mut remote =
        LocalVariantSource::open_with_remote_config(fixture.url(), small_block_config()).unwrap();

    // Cancelled: an error, no partial index kept.
    let cancellation: RemoteCancellation = Arc::new(|| false);
    remote.set_remote_cancellation(Some(cancellation));
    assert!(
        remote
            .search_ids("rs1", 5)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    remote.set_remote_cancellation(None);

    // A caller budget smaller than the scan: an error, retried later.
    remote.set_remote_transfer_budget(Some(RemoteTransferBudget {
        maximum_requests: 2,
        maximum_response_bytes: u64::MAX,
    }));
    assert!(
        remote
            .search_ids("rs1", 5)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    remote.set_remote_transfer_budget(None);

    // The scan's own allowance: a truncated index after a bounded transfer.
    let before = fixture.server.bytes_served("cohort.vcf.gz");
    remote.set_remote_id_scan_byte_limit(40_000);
    let results = remote.search_ids("rs", 5).unwrap();
    assert!(results.index_truncated);
    assert_eq!(results.hits.len(), 5);
    let scanned = fixture.server.bytes_served("cohort.vcf.gz") - before;
    assert!(
        (40_000..fixture.data_length / 2).contains(&scanned),
        "scan transferred {scanned} of {} bytes",
        fixture.data_length
    );
    // Late records were never reached.
    assert!(remote.search_ids("last", 5).unwrap().hits.is_empty());
}

#[test]
fn remote_vcf_writes_nothing_into_the_working_directory() {
    let fixture = CohortFixture::start(index::Type::Tbx, false);
    let working_directory = std::env::current_dir().unwrap();
    let mut remote = LocalVariantSource::open(fixture.url()).unwrap();
    assert!(
        !remote
            .query("chr1", 0, 5_000_000, 50)
            .unwrap()
            .records
            .is_empty()
    );
    assert!(!remote.search_ids("rs1", 5).unwrap().hits.is_empty());
    for entry in std::fs::read_dir(&working_directory).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains("cohort.vcf") && !name.starts_with("rgv-remote-variants-"),
            "{name} was written into {}",
            working_directory.display()
        );
    }
}

#[test]
fn remote_vcf_accepts_an_explicit_index_url_and_reports_a_missing_index() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index_path) =
        write_indexed_vcf(directory.path(), &cohort_vcf_text(), index::Type::Tbx);
    let server = VariantServer::start(
        HashMap::from([
            (
                "/data/calls.vcf.gz".to_owned(),
                std::fs::read(&path).unwrap(),
            ),
            (
                "/indexes/calls.tbi".to_owned(),
                std::fs::read(&index_path).unwrap(),
            ),
        ]),
        false,
    );
    let missing = LocalVariantSource::open(server.url("data/calls.vcf.gz"))
        .err()
        .unwrap();
    assert!(
        missing.to_string().contains("no .csi or .tbi index"),
        "{missing}"
    );
    let mut remote = LocalVariantSource::open_with_remote_index_and_config(
        server.url("data/calls.vcf.gz"),
        Some(&server.url("indexes/calls.tbi")),
        HttpRangeConfig::default(),
    )
    .unwrap();
    let mut local = LocalVariantSource::open(&path).unwrap();
    assert_eq!(
        remote.query("chr2", 0, 5_000_000, 1_000).unwrap(),
        local.query("chr2", 0, 5_000_000, 1_000).unwrap()
    );
}
