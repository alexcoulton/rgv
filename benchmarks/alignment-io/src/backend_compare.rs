//! Head-to-head decode throughput of the two production alignment decoders on a local BAM
//! (HTS-15): `rust-htslib` against `noodles`, single-threaded and with each library's
//! multithreaded BGZF inflater, for a layout-level and a full-detail traversal.
//!
//! Unlike the `run` digest, the full-detail level materialises what RGV's `QueryDetail::Full`
//! normalisation reads: every base, every quality, every CIGAR operation and every auxiliary
//! value. Both backends must produce the same digest before a timing is accepted.
//!
//! A second section times RGV's own path: `rgv_hts::ProductionAlignmentSource` opened on the
//! local BAM with each `AlignmentBackendPreference`, running the one-shot regional query and
//! draining a positioned stream. That includes normalisation, eligibility, packing and pileup
//! (the same code for both backends), so it shows what a backend switch changes end to end.
//! Both backends must return equal results before a timing is accepted.

use std::{fs, num::NonZero, path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use noodles::{bam as noodles_bam, bgzf, core::Region, sam};
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::{
    AlignmentBackendPreference, AlignmentOpenOptions, AlignmentSource as _,
    PositionedAlignmentBlock, PositionedAlignmentStreamCheckpoint, ProductionAlignmentSource,
    QueryDetail, QueryOptions,
};
use rust_htslib::bam::{self as hts_bam, Read as _};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Detail {
    /// Flags, placement, name and CIGAR: what a layout-only query touches.
    Layout,
    /// Layout plus every base, quality and auxiliary value.
    Full,
}

#[derive(Clone, Copy, Debug)]
struct Workload {
    name: &'static str,
    shape: &'static str,
    region: Option<&'static str>,
}

const WORKLOADS: &[Workload] = &[
    Workload {
        name: "deep-hotspot",
        shape: "short reads, 150 bp, one deep locus",
        region: Some("chr1:900001-920000"),
    },
    Workload {
        name: "broad-chr1",
        shape: "short reads, 150 bp, 2 Mb",
        region: Some("chr1:1-2000000"),
    },
    Workload {
        name: "long-modified-split",
        shape: "long reads, 11.5 kb with MM/ML/SA",
        region: Some("chr2:1-2000000"),
    },
    Workload {
        name: "sequential-all",
        shape: "whole file, no index",
        region: None,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backend {
    Htslib { threads: usize },
    Noodles { workers: usize },
}

impl Backend {
    const fn label(self) -> &'static str {
        match self {
            Self::Htslib { threads: 1 } => "rust-htslib-1t",
            Self::Htslib { .. } => "rust-htslib-4t",
            Self::Noodles { workers: 1 } => "noodles-1t",
            Self::Noodles { .. } => "noodles-mt4",
        }
    }

    const fn threads(self) -> usize {
        match self {
            Self::Htslib { threads } => threads,
            Self::Noodles { workers } => workers,
        }
    }
}

const BACKENDS: &[Backend] = &[
    Backend::Htslib { threads: 1 },
    Backend::Htslib { threads: 4 },
    Backend::Noodles { workers: 1 },
    Backend::Noodles { workers: 4 },
];

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct Digest {
    records: u64,
    flag_sum: u64,
    start_sum: u64,
    name_hash: u64,
    cigar_ops: u64,
    reference_bases: u64,
    base_count: u64,
    base_sum: u64,
    quality_sum: u64,
    auxiliary_fields: u64,
    auxiliary_value_sum: u64,
}

#[derive(Debug, Serialize)]
struct Measurement {
    workload: &'static str,
    shape: &'static str,
    detail: Detail,
    backend: &'static str,
    threads: usize,
    /// Wall time of the decode loop only (open, header and index load excluded).
    decode_ms: Vec<f64>,
    decode_median_ms: f64,
    /// Fastest decode loop: the least disturbed sample on a machine doing other work.
    decode_min_ms: f64,
    /// Wall time including open, header and index load.
    total_median_ms: f64,
    records: u64,
    records_per_second: f64,
    megabases_per_second: f64,
}

/// One timing of RGV's production path (`ProductionAlignmentSource`).
#[derive(Debug, Serialize)]
struct ProductionMeasurement {
    workload: &'static str,
    detail: Detail,
    /// `query`: the one-shot regional query. `stream`: a positioned stream drained to its end.
    mode: &'static str,
    backend: &'static str,
    /// Decompression threads the source was granted.
    threads: u32,
    ms: Vec<f64>,
    median_ms: f64,
    min_ms: f64,
    records: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    input: String,
    input_bytes: u64,
    iterations: usize,
    machine: String,
    measurements: Vec<Measurement>,
    production: Vec<ProductionMeasurement>,
}

pub fn run(input: &Path, iterations: usize, output: &Path) -> Result<()> {
    if iterations == 0 {
        bail!("iterations must be greater than zero");
    }
    if !input.exists() {
        bail!("{} does not exist; run `generate` first", input.display());
    }
    let mut measurements = Vec::new();
    for workload in WORKLOADS {
        for detail in [Detail::Layout, Detail::Full] {
            let (expected, _, _) = decode(input, *workload, detail, BACKENDS[0])?;
            for &backend in BACKENDS {
                let mut decode_ms = Vec::with_capacity(iterations);
                let mut total_ms = Vec::with_capacity(iterations);
                for _ in 0..iterations {
                    let (digest, decode, total) = decode(input, *workload, detail, backend)?;
                    if digest != expected {
                        bail!(
                            "digest mismatch for {} / {detail:?} / {}: expected={expected:?}, actual={digest:?}",
                            workload.name,
                            backend.label()
                        );
                    }
                    decode_ms.push(decode);
                    total_ms.push(total);
                }
                decode_ms.sort_by(f64::total_cmp);
                total_ms.sort_by(f64::total_cmp);
                let decode_median_ms = decode_ms[decode_ms.len() / 2];
                let seconds = decode_median_ms / 1_000.0;
                let measurement = Measurement {
                    workload: workload.name,
                    shape: workload.shape,
                    detail,
                    backend: backend.label(),
                    threads: backend.threads(),
                    decode_median_ms,
                    decode_min_ms: decode_ms[0],
                    total_median_ms: total_ms[total_ms.len() / 2],
                    records: expected.records,
                    records_per_second: expected.records as f64 / seconds,
                    megabases_per_second: expected.base_count as f64 / seconds / 1e6,
                    decode_ms,
                };
                println!(
                    "{:20} {:6} {:15} decode {:9.2} ms (min {:9.2})  total {:9.2} ms  {:11.0} records/s",
                    measurement.workload,
                    format!("{detail:?}").to_lowercase(),
                    measurement.backend,
                    measurement.decode_median_ms,
                    measurement.decode_min_ms,
                    measurement.total_median_ms,
                    measurement.records_per_second,
                );
                measurements.push(measurement);
            }
        }
    }
    let production = run_production(input, iterations)?;
    let report = Report {
        input: input.display().to_string(),
        input_bytes: fs::metadata(input)?.len(),
        iterations,
        machine: format!(
            "{} {}, {} logical CPUs",
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::thread::available_parallelism().map_or(0, NonZero::get)
        ),
        measurements,
        production,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(())
}

/// One pass: the digest, the decode-loop milliseconds and the total milliseconds.
fn decode(
    input: &Path,
    workload: Workload,
    detail: Detail,
    backend: Backend,
) -> Result<(Digest, f64, f64)> {
    let opened = Instant::now();
    let (digest, decode_ms) = match backend {
        Backend::Htslib { threads } => decode_htslib(input, workload, detail, threads)?,
        Backend::Noodles { workers: 1 } => {
            let reader = bgzf::io::Reader::new(fs::File::open(input)?);
            decode_noodles(reader, input, workload, detail)?
        }
        Backend::Noodles { workers } => {
            let workers = NonZero::new(workers).context("worker count must be nonzero")?;
            let reader =
                bgzf::io::MultithreadedReader::with_worker_count(workers, fs::File::open(input)?);
            decode_noodles(reader, input, workload, detail)?
        }
    };
    Ok((digest, decode_ms, opened.elapsed().as_secs_f64() * 1_000.0))
}

fn decode_htslib(
    input: &Path,
    workload: Workload,
    detail: Detail,
    threads: usize,
) -> Result<(Digest, f64)> {
    let mut digest = Digest::default();
    let mut record = hts_bam::Record::new();
    if let Some(region) = workload.region {
        let (contig, start, end) = parse_region(region)?;
        let mut reader = hts_bam::IndexedReader::from_path(input)?;
        if threads > 1 {
            reader.set_threads(threads)?;
        }
        let started = Instant::now();
        reader.fetch((contig, start, end))?;
        while let Some(result) = reader.read(&mut record) {
            result?;
            digest_htslib(&record, detail, &mut digest)?;
        }
        Ok((digest, started.elapsed().as_secs_f64() * 1_000.0))
    } else {
        let mut reader = hts_bam::Reader::from_path(input)?;
        if threads > 1 {
            reader.set_threads(threads)?;
        }
        let started = Instant::now();
        while let Some(result) = reader.read(&mut record) {
            result?;
            digest_htslib(&record, detail, &mut digest)?;
        }
        Ok((digest, started.elapsed().as_secs_f64() * 1_000.0))
    }
}

fn decode_noodles<R>(
    bgzf_reader: R,
    input: &Path,
    workload: Workload,
    detail: Detail,
) -> Result<(Digest, f64)>
where
    R: bgzf::io::BufRead + bgzf::io::Seek,
{
    let mut reader = noodles_bam::io::Reader::from(bgzf_reader);
    let header = reader.read_header()?;
    let mut digest = Digest::default();
    let mut record = noodles_bam::Record::default();
    if let Some(region) = workload.region {
        let index = noodles_bam::bai::fs::read(format!("{}.bai", input.display()))?;
        let region: Region = region.parse()?;
        let started = Instant::now();
        let mut query = reader.query(&header, &index, &region)?;
        while query.read_record(&mut record)? != 0 {
            digest_noodles(&record, &header, detail, &mut digest)?;
        }
        Ok((digest, started.elapsed().as_secs_f64() * 1_000.0))
    } else {
        let started = Instant::now();
        while reader.read_record(&mut record)? != 0 {
            digest_noodles(&record, &header, detail, &mut digest)?;
        }
        Ok((digest, started.elapsed().as_secs_f64() * 1_000.0))
    }
}

fn digest_htslib(record: &hts_bam::Record, detail: Detail, digest: &mut Digest) -> Result<()> {
    use rust_htslib::bam::record::Aux;

    digest.records += 1;
    digest.flag_sum += u64::from(record.flags());
    digest.start_sum = digest.start_sum.wrapping_add(record.pos() as u64);
    digest.name_hash = fnv1a(digest.name_hash, record.qname());
    for raw in record.raw_cigar() {
        digest.cigar_ops += 1;
        if matches!(raw & 0xf, 0 | 2 | 3 | 7 | 8) {
            digest.reference_bases += u64::from(raw >> 4);
        }
    }
    if detail == Detail::Full {
        let bases = record.seq().as_bytes();
        digest.base_count += bases.len() as u64;
        digest.base_sum += bases.iter().map(|base| u64::from(*base)).sum::<u64>();
        // Absent qualities are 0xff-filled in BAM; noodles reports them as empty.
        digest.quality_sum += record
            .qual()
            .iter()
            .filter(|quality| **quality != u8::MAX)
            .map(|quality| u64::from(*quality))
            .sum::<u64>();
        for result in record.aux_iter() {
            let (_, value) = result?;
            digest.auxiliary_fields += 1;
            digest.auxiliary_value_sum = digest.auxiliary_value_sum.wrapping_add(match value {
                Aux::Char(value) | Aux::U8(value) => u64::from(value),
                Aux::I8(value) => i64::from(value) as u64,
                Aux::I16(value) => i64::from(value) as u64,
                Aux::U16(value) => u64::from(value),
                Aux::I32(value) => i64::from(value) as u64,
                Aux::U32(value) => u64::from(value),
                Aux::Float(value) => u64::from(value.to_bits()),
                Aux::Double(value) => value.to_bits(),
                Aux::String(value) | Aux::HexByteArray(value) => byte_sum(value.as_bytes()),
                Aux::ArrayI8(values) => values.iter().map(|v| i64::from(v) as u64).sum(),
                Aux::ArrayU8(values) => values.iter().map(u64::from).sum(),
                Aux::ArrayI16(values) => values.iter().map(|v| i64::from(v) as u64).sum(),
                Aux::ArrayU16(values) => values.iter().map(u64::from).sum(),
                Aux::ArrayI32(values) => values.iter().map(|v| i64::from(v) as u64).sum(),
                Aux::ArrayU32(values) => values.iter().map(u64::from).sum(),
                Aux::ArrayFloat(values) => values.iter().map(|v| u64::from(v.to_bits())).sum(),
            });
        }
    }
    Ok(())
}

fn digest_noodles(
    record: &noodles_bam::Record,
    _header: &sam::Header,
    detail: Detail,
    digest: &mut Digest,
) -> Result<()> {
    use sam::alignment::record::data::field::{Value, value::Array};

    digest.records += 1;
    digest.flag_sum += u64::from(record.flags().bits());
    let start = record
        .alignment_start()
        .transpose()?
        .map_or(u64::MAX, |position| position.get() as u64 - 1);
    digest.start_sum = digest.start_sum.wrapping_add(start);
    if let Some(name) = record.name() {
        digest.name_hash = fnv1a(digest.name_hash, name.as_ref());
    }
    let cigar = record.cigar();
    for result in cigar.iter() {
        let op = result?;
        digest.cigar_ops += 1;
        if op.kind().consumes_reference() {
            digest.reference_bases += op.len() as u64;
        }
    }
    if detail == Detail::Full {
        // noodles lists the `CG` field of a long alignment as data; HTSlib consumes it.
        let cigar_bytes = cigar.as_bytes();
        let data = record.data();
        let cigar_from_cg = !cigar_bytes.is_empty()
            && data
                .as_bytes()
                .as_ptr_range()
                .contains(&cigar_bytes.as_ptr());
        let sequence = record.sequence();
        digest.base_count += sequence.len() as u64;
        digest.base_sum += sequence.iter().map(u64::from).sum::<u64>();
        digest.quality_sum += record
            .quality_scores()
            .as_ref()
            .iter()
            .map(|q| u64::from(*q))
            .sum::<u64>();
        for result in data.iter() {
            let (tag, value) = result?;
            if cigar_from_cg && tag.as_ref() == b"CG" {
                continue;
            }
            digest.auxiliary_fields += 1;
            let sum = match value {
                Value::Character(value) | Value::UInt8(value) => u64::from(value),
                Value::Int8(value) => i64::from(value) as u64,
                Value::Int16(value) => i64::from(value) as u64,
                Value::UInt16(value) => u64::from(value),
                Value::Int32(value) => i64::from(value) as u64,
                Value::UInt32(value) => u64::from(value),
                Value::Float(value) => u64::from(value.to_bits()),
                Value::String(value) | Value::Hex(value) => byte_sum(value),
                Value::Array(array) => match array {
                    Array::Int8(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::UInt8(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::Int16(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::UInt16(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::Int32(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::UInt32(values) => sum_results(values.iter().map(|v| v.map(i64::from)))?,
                    Array::Float(values) => sum_results(
                        values
                            .iter()
                            .map(|v| v.map(|value| i64::from(value.to_bits()))),
                    )?,
                },
            };
            digest.auxiliary_value_sum = digest.auxiliary_value_sum.wrapping_add(sum);
        }
    }
    Ok(())
}

fn sum_results(values: impl Iterator<Item = std::io::Result<i64>>) -> Result<u64> {
    let mut sum = 0_u64;
    for value in values {
        sum = sum.wrapping_add(value? as u64);
    }
    Ok(sum)
}

fn byte_sum(bytes: &[u8]) -> u64 {
    bytes.iter().map(|byte| u64::from(*byte)).sum()
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
    Ok((contig, start.parse::<i64>()? - 1, end.parse()?))
}

const PRODUCTION_BACKENDS: &[(&str, AlignmentBackendPreference, u32)] = &[
    (
        "production-htslib-1t",
        AlignmentBackendPreference::Htslib,
        1,
    ),
    (
        "production-noodles-1t",
        AlignmentBackendPreference::Noodles,
        1,
    ),
    (
        "production-htslib-4t",
        AlignmentBackendPreference::Htslib,
        4,
    ),
    (
        "production-noodles-4t",
        AlignmentBackendPreference::Noodles,
        4,
    ),
];

fn production_options(detail: Detail) -> QueryOptions {
    let limit = NonZero::new(2_000_000).expect("nonzero");
    match detail {
        Detail::Layout => QueryOptions {
            max_records: limit,
            ..QueryOptions::default()
        },
        // What a base-level view asks for: every field, allele and modification pileups.
        Detail::Full => QueryOptions {
            detail: QueryDetail::Full,
            collect_alleles: true,
            collect_modifications: true,
            max_records: limit,
            ..QueryOptions::default()
        },
    }
}

fn production_interval(
    source: &ProductionAlignmentSource,
    region: &str,
) -> Result<GenomicInterval> {
    let (contig, start, end) = parse_region(region)?;
    let index = source
        .reference_dictionary()
        .iter()
        .position(|candidate| candidate.name() == contig)
        .with_context(|| format!("{contig} is not in the header"))?;
    GenomicInterval::new(
        ContigId::new(u32::try_from(index)?),
        u64::try_from(start)?,
        u64::try_from(end)?,
    )
    .context("invalid benchmark interval")
}

fn drain_stream(
    source: &mut ProductionAlignmentSource,
    interval: GenomicInterval,
    options: &QueryOptions,
) -> Result<Vec<PositionedAlignmentBlock>> {
    source.begin_positioned_stream_with_admission(interval, interval, options.clone())?;
    let mut blocks = Vec::new();
    loop {
        match source.next_positioned_stream_checkpoint(&|| true)? {
            PositionedAlignmentStreamCheckpoint::Block(block) => blocks.push(block),
            PositionedAlignmentStreamCheckpoint::Complete(_) => return Ok(blocks),
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => bail!("stream was cancelled"),
        }
    }
}

/// Times `ProductionAlignmentSource` with each local BAM backend. A source is opened once per
/// backend (open time excluded) and asked the same query `iterations` times.
fn run_production(input: &Path, iterations: usize) -> Result<Vec<ProductionMeasurement>> {
    let mut measurements = Vec::new();
    for workload in WORKLOADS {
        let Some(region) = workload.region else {
            continue;
        };
        for detail in [Detail::Layout, Detail::Full] {
            let options = production_options(detail);
            let mut expected = None;
            for &(label, backend, threads) in PRODUCTION_BACKENDS {
                let mut source = ProductionAlignmentSource::open(
                    input,
                    &AlignmentOpenOptions {
                        local_bam_backend: backend,
                        decompression_threads: NonZero::new(threads).expect("nonzero"),
                        ..AlignmentOpenOptions::default()
                    },
                )?;
                let granted = match &source {
                    ProductionAlignmentSource::Htslib(source) => source.decompression_threads(),
                    ProductionAlignmentSource::LocalBam(source) => source.decompression_threads(),
                    _ => bail!("{} did not open as a local BAM", input.display()),
                };
                let interval = production_interval(&source, region)?;
                let mut query_ms = Vec::with_capacity(iterations);
                let mut stream_ms = Vec::with_capacity(iterations);
                let mut records = 0;
                for _ in 0..iterations {
                    let started = Instant::now();
                    let query = source.query(interval, options.clone(), &|| true)?;
                    query_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                    let started = Instant::now();
                    let blocks = drain_stream(&mut source, interval, &options)?;
                    stream_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                    records = query.batch.len();
                    match &expected {
                        None => expected = Some((query, blocks)),
                        Some((expected_query, expected_blocks)) => {
                            if *expected_query != query {
                                bail!("{label}: query differs for {} / {detail:?}", workload.name);
                            }
                            if *expected_blocks != blocks {
                                bail!("{label}: stream differs for {} / {detail:?}", workload.name);
                            }
                        }
                    }
                }
                for (mode, mut ms) in [("query", query_ms), ("stream", stream_ms)] {
                    ms.sort_by(f64::total_cmp);
                    let measurement = ProductionMeasurement {
                        workload: workload.name,
                        detail,
                        mode,
                        backend: label,
                        threads: granted,
                        median_ms: ms[ms.len() / 2],
                        min_ms: ms[0],
                        ms,
                        records,
                    };
                    println!(
                        "{:20} {:6} {:22} {:6} {:9.2} ms (min {:9.2})  {} records, {} threads",
                        measurement.workload,
                        format!("{detail:?}").to_lowercase(),
                        measurement.backend,
                        measurement.mode,
                        measurement.median_ms,
                        measurement.min_ms,
                        measurement.records,
                        measurement.threads,
                    );
                    measurements.push(measurement);
                }
            }
        }
    }
    Ok(measurements)
}
