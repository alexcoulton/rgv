use std::{
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource, HttpRangeConfig, HttpRangeReader,
    ProductionAlignmentSource, QueryDetail, QueryOptions, RemoteAuthentication,
};
use serde::Serialize;
use tiny_http::{Header, Response, Server, StatusCode};

#[derive(Debug, Parser)]
#[command(about = "Deterministic RGV HTTP range benchmark with simulated WAN conditions")]
struct Arguments {
    #[arg(long, default_value_t = 20)]
    latency_ms: u64,
    #[arg(long, default_value_t = true)]
    fail_first_data_request: bool,
    #[arg(long, default_value_t = 600 * 1024)]
    read_bytes: usize,
    #[arg(long, default_value_t = 3)]
    maximum_requests: u64,
    #[arg(long, default_value_t = 3 * 256 * 1024 + 1)]
    maximum_response_bytes: u64,
    #[arg(long, default_value = "benchmark-data/mixed.bam")]
    bam: PathBuf,
    #[arg(long, default_value_t = 12)]
    maximum_alignment_requests: u64,
    #[arg(long, default_value_t = 15 * 1024 * 1024)]
    maximum_alignment_response_bytes: u64,
    #[arg(long, default_value_t = 1_000.0)]
    maximum_alignment_wall_ms: f64,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Report {
    latency_ms: u64,
    injected_failure: bool,
    range_elapsed_ms: f64,
    total_elapsed_ms: f64,
    requests: u64,
    retries: u64,
    response_bytes: u64,
    cache_hits: u64,
    cache_misses: u64,
    evictions: u64,
    cancellations: u64,
    second_read_added_requests: u64,
    alignment: AlignmentReport,
}

#[derive(Debug, Serialize)]
struct AlignmentReport {
    locus: &'static str,
    elapsed_ms: f64,
    requests: u64,
    retries: u64,
    response_bytes: u64,
    cache_hits: u64,
    cache_misses: u64,
    evictions: u64,
    cancellations: u64,
    records_seen: usize,
    records_retained: usize,
    warm_query_added_requests: u64,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    anyhow::ensure!(arguments.read_bytes > 0, "read size must be positive");
    let fixture = RangeFixture::start(
        (0_u8..=255).cycle().take(4 * 1024 * 1024).collect(),
        Duration::from_millis(arguments.latency_ms),
        arguments.fail_first_data_request,
    )?;
    let total_started = Instant::now();
    let range_started = Instant::now();
    let mut reader = HttpRangeReader::open(
        &fixture.url,
        HttpRangeConfig {
            chunk_size: NonZeroUsize::new(256 * 1024).unwrap(),
            maximum_cache_bytes: NonZeroUsize::new(2 * 1024 * 1024).unwrap(),
            minimum_coalesced_chunks: NonZeroUsize::MIN,
            maximum_coalesced_chunks: NonZeroUsize::new(4).unwrap(),
            authentication: RemoteAuthentication::None,
            request_timeout: Duration::from_secs(5),
            maximum_retries: usize::from(arguments.fail_first_data_request),
        },
    )?;
    let mut bytes = vec![0; arguments.read_bytes];
    reader.read_exact(&mut bytes)?;
    let first_stats = reader.stats();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut bytes[..arguments.read_bytes.min(128 * 1024)])?;
    let final_stats = reader.stats();
    let range_elapsed_ms = range_started.elapsed().as_secs_f64() * 1_000.0;
    let alignment = run_alignment_gate(&arguments)?;
    let report = Report {
        latency_ms: arguments.latency_ms,
        injected_failure: arguments.fail_first_data_request,
        range_elapsed_ms,
        total_elapsed_ms: total_started.elapsed().as_secs_f64() * 1_000.0,
        requests: final_stats.requests,
        retries: final_stats.retries,
        response_bytes: final_stats.response_bytes,
        cache_hits: final_stats.cache_hits,
        cache_misses: final_stats.cache_misses,
        evictions: final_stats.evictions,
        cancellations: final_stats.cancellations,
        second_read_added_requests: final_stats.requests.saturating_sub(first_stats.requests),
        alignment,
    };
    anyhow::ensure!(
        report.requests <= arguments.maximum_requests,
        "{} requests exceeded the {} request budget",
        report.requests,
        arguments.maximum_requests
    );
    anyhow::ensure!(
        report.response_bytes <= arguments.maximum_response_bytes,
        "{} response bytes exceeded the {} byte budget",
        report.response_bytes,
        arguments.maximum_response_bytes
    );
    anyhow::ensure!(
        report.second_read_added_requests == 0,
        "warm reread missed cache"
    );
    anyhow::ensure!(
        report.retries == u64::from(arguments.fail_first_data_request),
        "retry count did not match the injected failure"
    );
    anyhow::ensure!(
        report.alignment.requests <= arguments.maximum_alignment_requests,
        "{} remote alignment requests exceeded the {} request budget",
        report.alignment.requests,
        arguments.maximum_alignment_requests
    );
    anyhow::ensure!(
        report.alignment.response_bytes <= arguments.maximum_alignment_response_bytes,
        "{} remote alignment response bytes exceeded the {} byte budget",
        report.alignment.response_bytes,
        arguments.maximum_alignment_response_bytes
    );
    anyhow::ensure!(
        report.alignment.elapsed_ms <= arguments.maximum_alignment_wall_ms,
        "{:.3} ms remote alignment query exceeded the {:.3} ms wall-time budget",
        report.alignment.elapsed_ms,
        arguments.maximum_alignment_wall_ms
    );
    anyhow::ensure!(
        report.alignment.warm_query_added_requests == 0,
        "warm remote alignment query missed the range cache"
    );
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = arguments.output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, format!("{json}\n"))?;
        println!("wrote {}", path.display());
    } else {
        println!("{json}");
    }
    Ok(())
}

fn run_alignment_gate(arguments: &Arguments) -> Result<AlignmentReport> {
    let index = PathBuf::from(format!("{}.bai", arguments.bam.display()));
    anyhow::ensure!(
        arguments.bam.exists() && index.exists(),
        "{} and its BAI are required; run the alignment benchmark generator first",
        arguments.bam.display()
    );
    let fixture = BamFixture::start(
        std::fs::read(&arguments.bam)?,
        std::fs::read(index)?,
        Duration::from_millis(arguments.latency_ms),
    )?;
    let options = AlignmentOpenOptions {
        remote_index_url: Some(fixture.index_url.clone()),
        remote_range: HttpRangeConfig {
            minimum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
            maximum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
            request_timeout: Duration::from_secs(10),
            ..HttpRangeConfig::default()
        },
        ..AlignmentOpenOptions::default()
    };
    let mut source = ProductionAlignmentSource::open(&fixture.bam_url, &options)?;
    let interval = GenomicInterval::new(ContigId::new(0), 900_000, 920_000)?;
    let query_options = QueryOptions {
        detail: QueryDetail::Full,
        ..QueryOptions::default()
    };
    let started = Instant::now();
    let query = source.query(interval, query_options.clone(), &|| true)?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    anyhow::ensure!(query.batch.validate(), "remote alignment batch is invalid");
    anyhow::ensure!(
        query.stats.records_seen >= 150_000,
        "remote deep-locus query returned too few records"
    );
    let first_stats = source
        .remote_io_stats()
        .context("production source did not select the observable remote BAM backend")?;
    let warm = source.query(interval, query_options, &|| true)?;
    anyhow::ensure!(
        warm.stats.records_seen == query.stats.records_seen
            && warm.stats.records_emitted == query.stats.records_emitted,
        "warm remote alignment query changed its record result"
    );
    let final_stats = source
        .remote_io_stats()
        .context("remote BAM statistics disappeared")?;
    Ok(AlignmentReport {
        locus: "chr1:900001-920000",
        elapsed_ms,
        requests: first_stats.requests,
        retries: first_stats.retries,
        response_bytes: first_stats.response_bytes,
        cache_hits: final_stats.cache_hits,
        cache_misses: final_stats.cache_misses,
        evictions: final_stats.evictions,
        cancellations: final_stats.cancellations,
        records_seen: query.stats.records_seen,
        records_retained: query.stats.records_emitted,
        warm_query_added_requests: final_stats.requests.saturating_sub(first_stats.requests),
    })
}

struct RangeFixture {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RangeFixture {
    fn start(data: Vec<u8>, latency: Duration, fail_first_data_request: bool) -> Result<Self> {
        let server =
            Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let address = server
            .server_addr()
            .to_ip()
            .context("loopback benchmark did not receive an IP address")?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut request_count = 0_u64;
            let mut rejected_data_request = false;
            while !worker_stop.load(Ordering::Relaxed) {
                let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                    continue;
                };
                request_count = request_count.saturating_add(1);
                if !latency.is_zero() {
                    thread::sleep(latency);
                }
                if fail_first_data_request && request_count > 1 && !rejected_data_request {
                    rejected_data_request = true;
                    request.respond(Response::empty(StatusCode(503))).unwrap();
                    continue;
                }
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_range(header.value.as_str(), data.len()))
                    .expect("benchmark request must contain a valid range");
                let response = Response::from_data(data[range.0..=range.1].to_vec())
                    .with_status_code(StatusCode(206))
                    .with_header(header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                    ))
                    .with_header(header("ETag", "\"rgv-wan-fixture-v1\""));
                request.respond(response).unwrap();
            }
        });
        Ok(Self {
            url: format!("http://{address}/fixture.bin"),
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for RangeFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("range fixture worker panicked");
        }
    }
}

struct BamFixture {
    bam_url: String,
    index_url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl BamFixture {
    fn start(bam: Vec<u8>, index: Vec<u8>, latency: Duration) -> Result<Self> {
        let server =
            Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let address = server
            .server_addr()
            .to_ip()
            .context("alignment benchmark did not receive an IP address")?;
        let bam = Arc::new(bam);
        let index = Arc::new(index);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                    continue;
                };
                if !latency.is_zero() {
                    thread::sleep(latency);
                }
                let data = match request.url() {
                    "/fixture.bam" => Arc::clone(&bam),
                    "/explicit-index.bai" => Arc::clone(&index),
                    _ => {
                        request.respond(Response::empty(StatusCode(404))).unwrap();
                        continue;
                    }
                };
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_range(header.value.as_str(), data.len()))
                    .expect("alignment benchmark request must contain a valid range");
                let response = Response::from_data(data[range.0..=range.1].to_vec())
                    .with_status_code(StatusCode(206))
                    .with_header(header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                    ))
                    .with_header(header("ETag", "\"rgv-alignment-fixture-v1\""));
                request.respond(response).unwrap();
            }
        });
        Ok(Self {
            bam_url: format!("http://{address}/fixture.bam"),
            index_url: format!("http://{address}/explicit-index.bai"),
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for BamFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("alignment fixture worker panicked");
        }
    }
}

fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse::<usize>().ok()?.min(length.saturating_sub(1));
    (start <= end).then_some((start, end))
}

fn header(name: &str, value: impl AsRef<str>) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_ref().as_bytes()).unwrap()
}
