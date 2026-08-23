use std::{fs, num::NonZero, path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use noodles::{bam as noodles_bam, core::Region};
use rsomics_bamio::raw::RecordReader as RawRecordReader;
use rust_htslib::bam::{self as hts_bam, Read as _};
use serde::Serialize;

use crate::remote::{HttpRangeReader, RangeServer};

#[derive(Clone, Copy, Debug)]
struct Workload {
    name: &'static str,
    region: Option<&'static str>,
    detail: bool,
}

const WORKLOADS: &[Workload] = &[
    Workload {
        name: "sequential-all",
        region: None,
        detail: true,
    },
    Workload {
        name: "broad-chr1",
        region: Some("chr1:1-2000000"),
        detail: false,
    },
    Workload {
        name: "deep-hotspot",
        region: Some("chr1:900001-920000"),
        detail: true,
    },
    Workload {
        name: "spliced",
        region: Some("chr1:2100001-2301000"),
        detail: true,
    },
    Workload {
        name: "long-modified-split",
        region: Some("chr2:1-2000000"),
        detail: true,
    },
    Workload {
        name: "long-cigar",
        region: Some("chr2:2500001-2550000"),
        detail: true,
    },
];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct Digest {
    records: u64,
    sequence_bases: u64,
    cigar_ops: u64,
    reference_bases: u64,
    selected_tag_hits: u64,
    name_hash: u64,
    flag_sum: u64,
}

#[derive(Debug, Serialize)]
struct Measurement {
    backend: &'static str,
    threads: usize,
    workload: &'static str,
    iterations_ms: Vec<f64>,
    median_ms: f64,
    records: u64,
    records_per_second: f64,
    remote_requests: Option<Vec<u64>>,
    remote_bytes: Option<Vec<u64>>,
    digest: Digest,
}

#[derive(Debug, Serialize)]
struct Report {
    input: String,
    input_bytes: u64,
    iterations: usize,
    remote_latency_ms: u64,
    measurements: Vec<Measurement>,
}

pub fn run(input: &Path, iterations: usize, remote_latency_ms: u64, output: &Path) -> Result<()> {
    if iterations == 0 {
        bail!("iterations must be greater than zero");
    }
    if !input.exists() {
        bail!("{} does not exist; run `generate` first", input.display());
    }

    let mut measurements = Vec::new();
    for workload in WORKLOADS {
        let expected = run_noodles(input, *workload)?;
        let htslib = run_htslib(input, *workload)?;
        if expected != htslib {
            bail!(
                "digest mismatch for {}: noodles={expected:?}, rust-htslib={htslib:?}",
                workload.name
            );
        }
        let rsomics = run_rsomics(input, *workload)?;
        if expected != rsomics {
            bail!(
                "digest mismatch for {}: noodles={expected:?}, rsomics-bamio={rsomics:?}",
                workload.name
            );
        }

        let backends: &[&str] = if workload.region.is_none() {
            &[
                "noodles",
                "rust-htslib",
                "rust-htslib-4t",
                "rsomics-bamio",
                "rsomics-bamio-4t",
            ]
        } else {
            &["noodles", "rust-htslib", "rust-htslib-4t", "rsomics-bamio"]
        };
        for &backend in backends {
            let mut samples = Vec::with_capacity(iterations);
            let mut digest = Digest::default();
            for _ in 0..iterations {
                let started = Instant::now();
                digest = match backend {
                    "noodles" => run_noodles(input, *workload)?,
                    "rust-htslib" => run_htslib(input, *workload)?,
                    "rust-htslib-4t" => run_htslib_with_threads(input, *workload, 4)?,
                    "rsomics-bamio" => run_rsomics(input, *workload)?,
                    "rsomics-bamio-4t" => run_rsomics_with_workers(input, *workload, 4)?,
                    _ => unreachable!(),
                };
                if digest != expected {
                    bail!(
                        "digest changed during {} / {backend}: expected={expected:?}, actual={digest:?}",
                        workload.name
                    );
                }
                samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            }
            samples.sort_by(f64::total_cmp);
            let median_ms = samples[samples.len() / 2];
            let records_per_second = if median_ms > 0.0 {
                digest.records as f64 / (median_ms / 1_000.0)
            } else {
                0.0
            };
            println!(
                "{backend:14} {:20} {:10.3} ms {:12.0} records/s (n={})",
                workload.name, median_ms, records_per_second, digest.records
            );
            measurements.push(Measurement {
                backend,
                threads: if backend.ends_with("-4t") { 4 } else { 1 },
                workload: workload.name,
                iterations_ms: samples,
                median_ms,
                records: digest.records,
                records_per_second,
                remote_requests: None,
                remote_bytes: None,
                digest,
            });
        }
    }

    let remote_workload = WORKLOADS
        .iter()
        .find(|workload| workload.name == "deep-hotspot")
        .copied()
        .expect("deep-hotspot workload exists");
    let expected = run_noodles(input, remote_workload)?;
    let server = RangeServer::start(input, remote_latency_ms)?;
    for backend in ["noodles-http", "rust-htslib-http"] {
        let verified = match backend {
            "noodles-http" => run_noodles_remote(server.url(), remote_workload)?,
            "rust-htslib-http" => run_htslib_remote(server.url(), remote_workload)?,
            _ => unreachable!(),
        };
        if verified != expected {
            bail!(
                "remote digest mismatch for {backend}: expected={expected:?}, actual={verified:?}"
            );
        }

        let mut samples = Vec::with_capacity(iterations);
        let mut request_counts = Vec::with_capacity(iterations);
        let mut byte_counts = Vec::with_capacity(iterations);
        let mut digest = Digest::default();
        for _ in 0..iterations {
            server.reset_counters();
            let started = Instant::now();
            digest = match backend {
                "noodles-http" => run_noodles_remote(server.url(), remote_workload)?,
                "rust-htslib-http" => run_htslib_remote(server.url(), remote_workload)?,
                _ => unreachable!(),
            };
            if digest != expected {
                bail!(
                    "digest changed during remote run for {backend}: expected={expected:?}, actual={digest:?}"
                );
            }
            samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            let (requests, bytes) = server.counters();
            request_counts.push(requests);
            byte_counts.push(bytes);
        }
        samples.sort_by(f64::total_cmp);
        let median_ms = samples[samples.len() / 2];
        let records_per_second = digest.records as f64 / (median_ms / 1_000.0);
        println!(
            "{backend:14} {:20} {:10.3} ms {:12.0} records/s (n={}, requests~{}, bytes~{})",
            "remote-deep-hotspot",
            median_ms,
            records_per_second,
            digest.records,
            median_u64(&request_counts),
            median_u64(&byte_counts),
        );
        measurements.push(Measurement {
            backend,
            threads: 1,
            workload: "remote-deep-hotspot",
            iterations_ms: samples,
            median_ms,
            records: digest.records,
            records_per_second,
            remote_requests: Some(request_counts),
            remote_bytes: Some(byte_counts),
            digest,
        });
    }

    let report = Report {
        input: input.display().to_string(),
        input_bytes: fs::metadata(input)?.len(),
        iterations,
        remote_latency_ms,
        measurements,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(())
}

fn run_noodles_remote(url: &str, workload: Workload) -> Result<Digest> {
    let index_bytes = reqwest::blocking::get(format!("{url}.bai"))?
        .error_for_status()?
        .bytes()?;
    let index = noodles_bam::bai::io::Reader::new(index_bytes.as_ref()).read_index()?;
    let source = HttpRangeReader::new(url)?;
    let mut reader = noodles_bam::io::indexed_reader::Builder::default()
        .set_index(index)
        .build_from_reader(source)?;
    let header = reader.read_header()?;
    let region: Region = workload
        .region
        .context("remote workload requires a region")?
        .parse()?;
    let mut digest = Digest::default();
    let query = reader.query(&header, &region)?;
    for result in query.records() {
        digest_noodles(&result?, workload.detail, &mut digest)?;
    }
    Ok(digest)
}

fn run_htslib_remote(url: &str, workload: Workload) -> Result<Digest> {
    let region = workload
        .region
        .context("remote workload requires a region")?;
    let (contig, start, end) = parse_region(region)?;
    let url = url::Url::parse(url)?;
    let mut reader = hts_bam::IndexedReader::from_url(&url)?;
    reader.fetch((contig, start, end))?;
    let mut record = hts_bam::Record::new();
    let mut digest = Digest::default();
    while let Some(result) = reader.read(&mut record) {
        result?;
        digest_htslib(&record, workload.detail, &mut digest)?;
    }
    Ok(digest)
}

fn run_noodles(input: &Path, workload: Workload) -> Result<Digest> {
    let mut reader = noodles_bam::io::indexed_reader::Builder::default()
        .build_from_path(input)
        .with_context(|| format!("noodles failed to open {}", input.display()))?;
    let header = reader.read_header()?;
    let mut digest = Digest::default();

    if let Some(region) = workload.region {
        let region: Region = region.parse()?;
        let query = reader.query(&header, &region)?;
        for result in query.records() {
            digest_noodles(&result?, workload.detail, &mut digest)?;
        }
    } else {
        for result in reader.records() {
            digest_noodles(&result?, workload.detail, &mut digest)?;
        }
    }

    Ok(digest)
}

fn run_htslib(input: &Path, workload: Workload) -> Result<Digest> {
    run_htslib_with_threads(input, workload, 1)
}

fn run_htslib_with_threads(input: &Path, workload: Workload, threads: usize) -> Result<Digest> {
    let mut digest = Digest::default();
    let mut record = hts_bam::Record::new();

    if let Some(region) = workload.region {
        let (contig, start, end) = parse_region(region)?;
        let mut reader = hts_bam::IndexedReader::from_path(input)?;
        if threads > 1 {
            reader.set_threads(threads)?;
        }
        reader.fetch((contig, start, end))?;
        while let Some(result) = reader.read(&mut record) {
            result?;
            digest_htslib(&record, workload.detail, &mut digest)?;
        }
    } else {
        let mut reader = hts_bam::Reader::from_path(input)?;
        if threads > 1 {
            reader.set_threads(threads)?;
        }
        while let Some(result) = reader.read(&mut record) {
            result?;
            digest_htslib(&record, workload.detail, &mut digest)?;
        }
    }

    Ok(digest)
}

fn run_rsomics(input: &Path, workload: Workload) -> Result<Digest> {
    run_rsomics_with_workers(input, workload, 1)
}

fn run_rsomics_with_workers(
    input: &Path,
    workload: Workload,
    worker_count: usize,
) -> Result<Digest> {
    let mut digest = Digest::default();

    if let Some(region) = workload.region {
        let mut reader = rsomics_bamio::open_indexed_alignment(input, None)?;
        let header = reader.read_header()?;
        let region: noodles110::core::Region = region.parse()?;
        let query = reader.query(&header, &region)?;
        for result in query {
            let record = result?;
            digest_rsomics_record(record.as_ref(), workload.detail, &mut digest)?;
        }
    } else {
        let workers = NonZero::new(worker_count).context("worker count must be nonzero")?;
        let mut reader = rsomics_bamio::open_with_workers(input, workers)?;
        let _header = reader.read_header()?;
        let mut records = RawRecordReader::new(reader.get_mut());
        let mut cigar_buf = Vec::new();
        while let Some(record) = records.next()? {
            digest_rsomics_raw(&record, workload.detail, &mut cigar_buf, &mut digest)?;
        }
    }

    Ok(digest)
}

fn digest_noodles(record: &noodles_bam::Record, detail: bool, digest: &mut Digest) -> Result<()> {
    digest.records += 1;
    digest.flag_sum += u64::from(record.flags().bits());
    if let Some(name) = record.name() {
        digest.name_hash = fnv1a(digest.name_hash, name.as_ref());
    }

    if detail {
        digest.sequence_bases += record.sequence().len() as u64;
        for result in record.cigar().iter() {
            let op = result?;
            digest.cigar_ops += 1;
            if op.kind().consumes_reference() {
                digest.reference_bases += op.len() as u64;
            }
        }
        for result in record.data().iter() {
            let (tag, _) = result?;
            if selected_tag(tag.as_ref()) {
                digest.selected_tag_hits += 1;
            }
        }
    }
    Ok(())
}

fn digest_htslib(record: &hts_bam::Record, detail: bool, digest: &mut Digest) -> Result<()> {
    use rust_htslib::bam::record::Cigar;

    digest.records += 1;
    digest.flag_sum += u64::from(record.flags());
    digest.name_hash = fnv1a(digest.name_hash, record.qname());

    if detail {
        digest.sequence_bases += record.seq_len() as u64;
        for op in record.cigar().iter() {
            digest.cigar_ops += 1;
            if matches!(
                op,
                Cigar::Match(_)
                    | Cigar::Del(_)
                    | Cigar::RefSkip(_)
                    | Cigar::Equal(_)
                    | Cigar::Diff(_)
            ) {
                digest.reference_bases += u64::from(op.len());
            }
        }
        for result in record.aux_iter() {
            let (tag, _) = result?;
            if selected_tag(tag) {
                digest.selected_tag_hits += 1;
            }
        }
    }
    Ok(())
}

fn digest_rsomics_record(
    record: &dyn noodles110::sam::alignment::Record,
    detail: bool,
    digest: &mut Digest,
) -> Result<()> {
    digest.records += 1;
    digest.flag_sum += u64::from(record.flags()?.bits());
    if let Some(name) = record.name() {
        digest.name_hash = fnv1a(digest.name_hash, name.as_ref());
    }

    if detail {
        digest.sequence_bases += record.sequence().len() as u64;
        for result in record.cigar().iter() {
            let op = result?;
            digest.cigar_ops += 1;
            if op.kind().consumes_reference() {
                digest.reference_bases += op.len() as u64;
            }
        }
        for result in record.data().iter() {
            let (tag, _) = result?;
            if selected_tag(tag.as_ref()) {
                digest.selected_tag_hits += 1;
            }
        }
    }
    Ok(())
}

fn digest_rsomics_raw(
    record: &rsomics_bamio::raw::RecordRef<'_>,
    detail: bool,
    cigar_buf: &mut Vec<(u8, u32)>,
    digest: &mut Digest,
) -> Result<()> {
    digest.records += 1;
    digest.flag_sum += u64::from(record.flags());
    digest.name_hash = fnv1a(digest.name_hash, record.name());

    if detail {
        digest.sequence_bases += record.sequence_len() as u64;
        record.decode_cigar_into(cigar_buf)?;
        for &(kind, len) in cigar_buf.iter() {
            digest.cigar_ops += 1;
            if matches!(kind, 0 | 2 | 3 | 7 | 8) {
                digest.reference_bases += u64::from(len);
            }
        }
        for tag in SELECTED_TAGS {
            if record.aux_type(*tag).is_some() {
                digest.selected_tag_hits += 1;
            }
        }
    }
    Ok(())
}

const SELECTED_TAGS: &[[u8; 2]] = &[*b"RG", *b"NM", *b"MM", *b"ML", *b"HP", *b"SA"];

fn selected_tag(tag: &[u8]) -> bool {
    SELECTED_TAGS.iter().any(|candidate| candidate == tag)
}

fn fnv1a(current: u64, bytes: &[u8]) -> u64 {
    let mut hash = if current == 0 {
        0xcbf2_9ce4_8422_2325
    } else {
        current
    };
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn parse_region(region: &str) -> Result<(&str, i64, i64)> {
    let (contig, coordinates) = region.split_once(':').context("region is missing ':'")?;
    let (start, end) = coordinates
        .split_once('-')
        .context("region is missing '-'")?;
    let start: i64 = start.parse()?;
    let end: i64 = end.parse()?;
    Ok((contig, start - 1, end))
}

fn median_u64(values: &[u64]) -> u64 {
    let mut values = values.to_vec();
    values.sort_unstable();
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_display_region_to_zero_based_half_open_coordinates() -> Result<()> {
        assert_eq!(parse_region("chr1:1-200")?, ("chr1", 0, 200));
        assert!(parse_region("chr1").is_err());
        Ok(())
    }

    #[test]
    fn selected_tag_filter_is_explicit() {
        assert!(selected_tag(b"MM"));
        assert!(selected_tag(b"SA"));
        assert!(!selected_tag(b"AS"));
    }
}
