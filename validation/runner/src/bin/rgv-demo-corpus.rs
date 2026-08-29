use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bigtools::{BigBedWrite, BigWigWrite, beddata::BedParserStreamingIterator};
use clap::Parser;
use flate2::{Compression, write::GzEncoder, write::ZlibEncoder};
use rgv_core::{
    GenerationClock, GenomicInterval, PanelGeneration, PanelId, ReviewItem, ReviewStatus,
    SessionAlignmentTrack, SessionDocument, SessionLocus, SessionViewPanel, SmrtKineticsMode,
    TrackConfig, TrackFormat, TrackId, TrackKind, TrackSource,
};
use rgv_engine::{TrackEngine, TrackEngineEvent, TrackSnapshotData, TrackSpec, TrackSpecKind};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentQuery, AlignmentSource, FastaReference, LocalAlignmentSource,
    QueryDetail, QueryOptions, SequenceView, smrt_kinetics_series, summarize_sbx_record,
};
use rust_htslib::bcf::{Format, Header, Writer, index, record::GenotypeAllele};
use serde::Serialize;
use sha2::{Digest, Sha256};

const CONTIG: &str = "chrVariants";
const CONTIG_LENGTH: u32 = 20_000;
const ALIGNMENT_REVIEW_PROJECT: &str = "rgv-alignment-review-project.json";

#[derive(Clone, Copy)]
struct AlignmentReviewLocus {
    label: &'static str,
    notes: &'static str,
    contig: &'static str,
    start: u64,
    end: u64,
}

const ALIGNMENT_REVIEW_LOCI: &[AlignmentReviewLocus] = &[
    AlignmentReviewLocus {
        label: "Paired short reads",
        notes: "Proper paired templates; use View as pairs, grouping, sorting, mate navigation, and overlap counting.",
        contig: "chr1",
        start: 0,
        end: 200_000,
    },
    AlignmentReviewLocus {
        label: "Deep coverage",
        notes: "150,000 records cross the locus; use this to review sampling-independent coverage and responsive navigation.",
        contig: "chr1",
        start: 900_000,
        end: 920_000,
    },
    AlignmentReviewLocus {
        label: "RNA splice junctions",
        notes: "10,000 distinct generated junctions with tagged, inferred, and invalid strand evidence.",
        contig: "chr1",
        start: 2_100_000,
        end: 2_301_000,
    },
    AlignmentReviewLocus {
        label: "Structural read pairs",
        notes: "Long, short, same-strand, outward-facing, and inter-contig pair evidence.",
        contig: "chr1",
        start: 2_590_000,
        end: 2_650_000,
    },
    AlignmentReviewLocus {
        label: "Long and specialist reads",
        notes: "Long reads, supplementary chains, modified bases, SMRT kinetics, and SBX evidence.",
        contig: "chr2",
        start: 0,
        end: 2_000_000,
    },
    AlignmentReviewLocus {
        label: "Pathological long CIGAR",
        notes: "One generated read with 70,000 CIGAR operations; inspect and navigate it without truncation or a crash.",
        contig: "chr2",
        start: 2_500_000,
        end: 2_550_000,
    },
];

const ALIGNMENT_PANEL_REVIEW_INDEXES: &[usize] = &[0, 2, 4, 5];

#[derive(Clone, Copy)]
struct DemoTrack {
    name: &'static str,
    file: &'static str,
    kind: TrackKind,
    format: Option<TrackFormat>,
    engine_kind: TrackSpecKind,
}

const DEMO_TRACKS: &[DemoTrack] = &[
    DemoTrack {
        name: "Features (gzip BED)",
        file: "features.bed.gz",
        kind: TrackKind::Feature,
        format: None,
        engine_kind: TrackSpecKind::Bed,
    },
    DemoTrack {
        name: "Genes (gzip GTF)",
        file: "genes.gtf.gz",
        kind: TrackKind::GeneModel,
        format: None,
        engine_kind: TrackSpecKind::Gtf,
    },
    DemoTrack {
        name: "Transcripts",
        file: "transcripts.genePred",
        kind: TrackKind::GeneModel,
        format: None,
        engine_kind: TrackSpecKind::GenePred,
    },
    DemoTrack {
        name: "Narrow peaks",
        file: "peaks.narrowPeak",
        kind: TrackKind::Feature,
        format: None,
        engine_kind: TrackSpecKind::NarrowPeak,
    },
    DemoTrack {
        name: "Broad peaks",
        file: "regions.broadPeak",
        kind: TrackKind::Feature,
        format: None,
        engine_kind: TrackSpecKind::BroadPeak,
    },
    DemoTrack {
        name: "PSL target alignments",
        file: "target-alignments.psl",
        kind: TrackKind::Feature,
        format: None,
        engine_kind: TrackSpecKind::Psl,
    },
    DemoTrack {
        name: "Association study",
        file: "association.gwas",
        kind: TrackKind::Gwas,
        format: None,
        engine_kind: TrackSpecKind::Gwas,
    },
    DemoTrack {
        name: "Variants and genotypes",
        file: "variants.bcf",
        kind: TrackKind::Variant,
        format: None,
        engine_kind: TrackSpecKind::VcfOrBcf,
    },
    DemoTrack {
        name: "Signal BigWig",
        file: "signal.bigWig",
        kind: TrackKind::Quantitative,
        format: None,
        engine_kind: TrackSpecKind::BigWig,
    },
    DemoTrack {
        name: "Archived TDF",
        file: "archive.tdf",
        kind: TrackKind::Quantitative,
        format: None,
        engine_kind: TrackSpecKind::Tdf,
    },
    DemoTrack {
        name: "Signal bedGraph",
        file: "signal.bedGraph",
        kind: TrackKind::Quantitative,
        format: None,
        engine_kind: TrackSpecKind::BedGraph,
    },
    DemoTrack {
        name: "Signal WIG",
        file: "signal.wig",
        kind: TrackKind::Quantitative,
        format: None,
        engine_kind: TrackSpecKind::Wig,
    },
    DemoTrack {
        name: "BEDPE loops",
        file: "loops.bedpe",
        kind: TrackKind::Interaction,
        format: None,
        engine_kind: TrackSpecKind::BedPe,
    },
    DemoTrack {
        name: "UCSC interactions",
        file: "interactions.interact",
        kind: TrackKind::Interaction,
        format: None,
        engine_kind: TrackSpecKind::Interact,
    },
    DemoTrack {
        name: "Copy number",
        file: "cohort.seg",
        kind: TrackKind::CopyNumber,
        format: None,
        engine_kind: TrackSpecKind::Seg,
    },
    DemoTrack {
        name: "Somatic mutations",
        file: "mutations.maf",
        kind: TrackKind::Mutation,
        format: None,
        engine_kind: TrackSpecKind::MutationMaf,
    },
    DemoTrack {
        name: "Generic BigBed",
        file: "features.bigBed",
        kind: TrackKind::Feature,
        format: None,
        engine_kind: TrackSpecKind::BigBed,
    },
    DemoTrack {
        name: "Indexed transcripts",
        file: "genes.bigGenePred",
        kind: TrackKind::GeneModel,
        format: Some(TrackFormat::BigGenePred),
        engine_kind: TrackSpecKind::BigGenePred,
    },
    DemoTrack {
        name: "Indexed peaks",
        file: "peaks.bigNarrowPeak",
        kind: TrackKind::Feature,
        format: Some(TrackFormat::BigNarrowPeak),
        engine_kind: TrackSpecKind::BigNarrowPeak,
    },
];

#[derive(Debug, Parser)]
#[command(about = "Generate the deterministic mixed-format RGV manual-test workspace")]
struct Arguments {
    #[arg(long, default_value = "benchmark-data/demo-tracks")]
    output: PathBuf,
}

#[derive(Serialize)]
struct DemoManifest {
    schema_version: u32,
    locus: String,
    project: String,
    files: Vec<DemoFile>,
}

#[derive(Serialize)]
struct DemoFile {
    path: String,
    sha256: String,
    purpose: String,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    generate(&arguments.output)
}

fn generate(output: &Path) -> Result<()> {
    fs::create_dir_all(output).with_context(|| format!("failed to create {}", output.display()))?;
    write_text_fixtures(output)?;
    write_bcf(&output.join("variants.bcf"))?;
    write_bigwig(
        &output.join("signal.bedGraph"),
        &output.join("signal.bigWig"),
    )?;
    write_bigbed(
        &output.join("features.bigBed"),
        None,
        "chrVariants\t8700\t9000\tindexed-feature\t500\t+\n\
         chrVariants\t12100\t12500\tindexed-feature-2\t700\t-\n",
    )?;
    write_bigbed(
        &output.join("genes.bigGenePred"),
        Some(BIG_GENE_PRED_AUTOSQL),
        "chrVariants\t9000\t10200\tDEMO-BIG-T1\t900\t+\t9100\t10100\t0\t3\t200,250,200,\t0,450,1000,\tDEMO-BIG-T1-alt\tcmpl\tcmpl\t0,1,2,\tcoding\tDEMO-BIG\tDEMO-BIG-alt\tprotein_coding\n",
    )?;
    write_bigbed(
        &output.join("peaks.bigNarrowPeak"),
        Some(BIG_NARROW_PEAK_AUTOSQL),
        "chrVariants\t10600\t11100\tindexed-peak\t780\t.\t18.0\t7.0\t5.0\t220\n",
    )?;
    write_text_bytes(&output.join("archive.tdf"), &build_tdf())?;

    let project_path = output.join("rgv-demo-project.json");
    let alignment_project_path = output.join(ALIGNMENT_REVIEW_PROJECT);
    write_project(&project_path)?;
    write_alignment_review_project(&alignment_project_path)?;
    write_manifest(output, &project_path)?;
    verify_generated_workspace(output, &project_path, &alignment_project_path)?;
    println!(
        "wrote and verified deterministic demo workspaces {} and {}",
        project_path.display(),
        alignment_project_path.display()
    );
    Ok(())
}

fn write_text_fixtures(output: &Path) -> Result<()> {
    write_gzip(
        &output.join("features.bed.gz"),
        "track name=RGV_demo_features itemRgb=On\n\
         chrVariants\t8800\t9600\treview-region\t900\t+\t9000\t9450\t42,112,190\t3\t150,180,120\t0,320,680\n\
         chrVariants\t10100\t10600\tenhancer\t500\t.\n",
    )?;
    write_gzip(
        &output.join("genes.gtf.gz"),
        "chrVariants\tRGV\ttranscript\t9001\t10100\t.\t+\t.\tgene_id \"DEMO1\"; transcript_id \"DEMO1-T1\"; gene_name \"Demo gene 1\";\n\
         chrVariants\tRGV\texon\t9001\t9300\t.\t+\t.\tgene_id \"DEMO1\"; transcript_id \"DEMO1-T1\"; gene_name \"Demo gene 1\";\n\
         chrVariants\tRGV\tCDS\t9051\t9300\t.\t+\t0\tgene_id \"DEMO1\"; transcript_id \"DEMO1-T1\"; gene_name \"Demo gene 1\";\n\
         chrVariants\tRGV\texon\t9701\t10100\t.\t+\t.\tgene_id \"DEMO1\"; transcript_id \"DEMO1-T1\"; gene_name \"Demo gene 1\";\n\
         chrVariants\tRGV\tCDS\t9701\t10050\t.\t+\t2\tgene_id \"DEMO1\"; transcript_id \"DEMO1-T1\"; gene_name \"Demo gene 1\";\n",
    )?;
    write_text(
        &output.join("transcripts.genePred"),
        "DEMO2-T1\tchrVariants\t-\t10500\t12000\t10600\t11800\t3\t10500,11000,11700,\t10800,11300,12000,\n",
    )?;
    write_text(
        &output.join("peaks.narrowPeak"),
        "chrVariants\t9200\t9500\tpeak-with-summit\t850\t.\t24.5\t8.0\t6.0\t140\n",
    )?;
    write_text(
        &output.join("regions.broadPeak"),
        "chrVariants\t10800\t11400\tbroad-region\t620\t.\t12.5\t5.0\t3.0\n",
    )?;
    write_text(
        &output.join("target-alignments.psl"),
        "60\t5\t5\t0\t1\t10\t1\t20\t+\tquery-demo\t100\t0\t80\tchrVariants\t20000\t9800\t9890\t2\t30,40,\t0,40,\t9800,9850,\n",
    )?;
    write_text(
        &output.join("association.gwas"),
        "CHR\tBP\tSNP\tP\tBETA\n\
         chrVariants\t9100\trs-demo-1\t1e-8\t0.42\n\
         chrVariants\t10050\trs-demo-2\t0\t-0.31\n\
         chrVariants\t11950\trs-demo-3\t0.004\t0.12\n",
    )?;
    write_text(
        &output.join("signal.bedGraph"),
        "track type=bedGraph name=RGV_demo_signal\n\
         chrVariants\t8500\t9300\t2.5\n\
         chrVariants\t9300\t10000\t8.0\n\
         chrVariants\t10000\t11000\t-3.5\n\
         chrVariants\t11000\t12500\t5.0\n",
    )?;
    write_text(
        &output.join("signal.wig"),
        "track type=wiggle_0 name=RGV_demo_wig\n\
         variableStep chrom=chrVariants span=100\n\
         8801 1.0\n9301 4.0\n9801 2.0\n10801 6.0\n",
    )?;
    write_text(
        &output.join("loops.bedpe"),
        "chrVariants\t9000\t9100\tchrVariants\t11200\t11300\tloop-A\t750\t+\t-\n\
         chrVariants\t9900\t10000\tchrVariants\t12300\t12400\tloop-B\t500\t-\t+\n",
    )?;
    write_text(
        &output.join("interactions.interact"),
        "chrVariants\t9200\t11800\tvariant/gene/demo\t700\t0.625\tdemo\t#7A67EE\tchrVariants\t9200\t9201\trs-demo-1\t.\tchrVariants\t11799\t11800\tDEMO2\t+\n",
    )?;
    write_text(
        &output.join("cohort.seg"),
        "Sample\tChromosome\tStart\tEnd\tNum_Probes\tSegment_Mean\n\
         tumour\tchrVariants\t8501\t10000\t120\t0.85\n\
         tumour\tchrVariants\t10001\t12500\t180\t-0.65\n\
         normal\tchrVariants\t8501\t12500\t300\t0.02\n",
    )?;
    write_text(
        &output.join("mutations.maf"),
        "#version 2.4\n\
         Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_Barcode\n\
         DEMO1\tchrVariants\t9951\t9951\tMissense_Mutation\tSNP\tA\tA\tT\ttumour\n\
         DEMO2\tchrVariants\t11201\t11203\tFrame_Shift_Del\tDEL\tATC\t-\t-\ttumour\n",
    )?;
    Ok(())
}

fn write_text(path: &Path, contents: &str) -> Result<()> {
    write_text_bytes(path, contents.as_bytes())
}

fn write_text_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn write_gzip(path: &Path, contents: &str) -> Result<()> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(contents.as_bytes())?;
    write_text_bytes(path, &encoder.finish()?)
}

fn write_bcf(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(format!("##contig=<ID={CONTIG},length={CONTIG_LENGTH}>").as_bytes());
    header.push_record(br#"##FILTER=<ID=q10,Description="Quality below ten">"#);
    header
        .push_record(br#"##INFO=<ID=END,Number=1,Type=Integer,Description="Structural endpoint">"#);
    header.push_record(br#"##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency">"#);
    header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
    header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">"#);
    header.push_sample(b"tumour");
    header.push_sample(b"normal");
    {
        let mut writer = Writer::from_path(path, &header, false, Format::Bcf)?;
        for (position, id, alleles, quality, end, af) in [
            (
                9_949_i64,
                "demo-snv",
                vec![b"A".as_slice(), b"T".as_slice()],
                60.0,
                None,
                0.35,
            ),
            (
                10_149,
                "demo-mnv",
                vec![b"AC".as_slice(), b"GT".as_slice()],
                45.0,
                None,
                0.20,
            ),
            (
                10_349,
                "demo-ins",
                vec![b"A".as_slice(), b"ATGC".as_slice()],
                50.0,
                None,
                0.18,
            ),
            (
                10_649,
                "demo-del",
                vec![b"ATGC".as_slice(), b"A".as_slice()],
                38.0,
                None,
                0.12,
            ),
            (
                11_049,
                "demo-sv",
                vec![b"N".as_slice(), b"<DEL>".as_slice()],
                9.5,
                Some(11_800),
                0.25,
            ),
            (
                12_049,
                "demo-bnd",
                vec![b"N".as_slice(), b"N]chrVariants:14001]".as_slice()],
                52.0,
                None,
                0.08,
            ),
        ] {
            let mut record = writer.empty_record();
            record.set_rid(Some(0));
            record.set_pos(position);
            record.set_id(id.as_bytes())?;
            record.set_alleles(&alleles)?;
            record.set_qual(quality);
            if let Some(end) = end {
                record.push_info_integer(b"END", &[end])?;
                record.set_filters(&[b"q10".as_slice()])?;
            }
            record.push_info_float(b"AF", &[af])?;
            record.push_genotypes(&[
                GenotypeAllele::Unphased(0),
                GenotypeAllele::Unphased(1),
                GenotypeAllele::Phased(0),
                GenotypeAllele::Phased(0),
            ])?;
            record.push_format_integer(b"DP", &[48, 42])?;
            writer.write(&record)?;
        }
    }
    index::build(path, None, 1, index::Type::Csi(14))?;
    Ok(())
}

fn chromosome_sizes() -> HashMap<String, u32> {
    HashMap::from([(CONTIG.to_owned(), CONTIG_LENGTH)])
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .context("failed to build fixture-writer runtime")
}

fn write_bigwig(input: &Path, output: &Path) -> Result<()> {
    let writer = BigWigWrite::create_file(output, chromosome_sizes())?;
    let contents = fs::read_to_string(input)?;
    let values = contents
        .lines()
        .filter(|line| !line.starts_with("track") && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let data = BedParserStreamingIterator::from_bedgraph_file(Cursor::new(values), false);
    writer
        .write(data, runtime()?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn write_bigbed(path: &Path, autosql: Option<&str>, contents: &str) -> Result<()> {
    let mut writer = BigBedWrite::create_file(path, chromosome_sizes())?;
    writer.autosql = autosql.map(str::to_owned);
    writer.options.compress = false;
    let data =
        BedParserStreamingIterator::from_bed_file(Cursor::new(contents.as_bytes().to_vec()), false);
    writer
        .write(data, runtime()?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn put_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
}

fn build_tdf() -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&1_u32.to_le_bytes());
    put_string(&mut header, "mean");
    put_string(&mut header, "OTHER");
    put_string(&mut header, "track type=wiggle_0");
    header.extend_from_slice(&2_u32.to_le_bytes());
    put_string(&mut header, "archive sample one");
    put_string(&mut header, "archive sample two");
    put_string(&mut header, "rgv-demo");
    header.extend_from_slice(&1_u32.to_le_bytes());

    let mut tile = Vec::new();
    put_string(&mut tile, "fixedStep");
    tile.extend_from_slice(&4_u32.to_le_bytes());
    tile.extend_from_slice(&9_000_i32.to_le_bytes());
    tile.extend_from_slice(&500_f32.to_le_bytes());
    for value in [1.0_f32, 3.0, 5.0, 7.0, 2.0, 4.0, 6.0, 8.0] {
        tile.extend_from_slice(&value.to_le_bytes());
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tile).expect("writing a Vec cannot fail");
    let tile = encoder.finish().expect("finishing a Vec cannot fail");

    let tile_position = (24 + header.len()) as u64;
    let mut dataset = Vec::new();
    dataset.extend_from_slice(&0_u32.to_le_bytes());
    put_string(&mut dataset, "FLOAT");
    dataset.extend_from_slice(&20_000_f32.to_le_bytes());
    dataset.extend_from_slice(&1_u32.to_le_bytes());
    dataset.extend_from_slice(&i64::try_from(tile_position).unwrap().to_le_bytes());
    dataset.extend_from_slice(&(tile.len() as u32).to_le_bytes());
    let dataset_position = tile_position + tile.len() as u64;

    let mut index_bytes = Vec::new();
    index_bytes.extend_from_slice(&1_u32.to_le_bytes());
    put_string(&mut index_bytes, "/chrVariants/z0/mean");
    index_bytes.extend_from_slice(&dataset_position.to_le_bytes());
    index_bytes.extend_from_slice(&(dataset.len() as u32).to_le_bytes());
    index_bytes.extend_from_slice(&0_u32.to_le_bytes());
    let index_position = dataset_position + dataset.len() as u64;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"TDF4");
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&index_position.to_le_bytes());
    bytes.extend_from_slice(&(index_bytes.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&tile);
    bytes.extend_from_slice(&dataset);
    bytes.extend_from_slice(&index_bytes);
    bytes
}

fn local_source(path: impl Into<String>) -> TrackSource {
    TrackSource::Local { path: path.into() }
}

fn local_path(source: &TrackSource) -> Result<&Path> {
    let TrackSource::Local { path } = source else {
        anyhow::bail!("demo project unexpectedly contains a remote source");
    };
    Ok(Path::new(path))
}

fn verify_generated_workspace(
    output: &Path,
    project_path: &Path,
    alignment_project_path: &Path,
) -> Result<()> {
    let document = SessionDocument::read(project_path)
        .with_context(|| format!("failed to reopen {}", project_path.display()))?;
    anyhow::ensure!(
        document.tracks.len() == DEMO_TRACKS.len(),
        "demo project restored {} auxiliary tracks instead of {}",
        document.tracks.len(),
        DEMO_TRACKS.len()
    );
    verify_primary_sources(&document)?;

    let engine = TrackEngine::spawn(demo_track_specs(output), 64);
    verify_tracks_opened(&engine)?;
    verify_tracks_query(&engine)?;
    verify_alignment_review_project(alignment_project_path)
}

fn verify_primary_sources(document: &SessionDocument) -> Result<()> {
    anyhow::ensure!(
        document.alignment_tracks.len() == 1,
        "demo project must restore one alignment track"
    );

    let alignment_path = local_path(&document.alignment_tracks[0].source)?;
    let alignment = LocalAlignmentSource::open(alignment_path, &AlignmentOpenOptions::default())
        .with_context(|| format!("failed to open demo alignment {}", alignment_path.display()))?;
    anyhow::ensure!(
        alignment
            .reference_dictionary()
            .id_by_name(CONTIG)
            .is_some(),
        "demo alignment does not contain {CONTIG}"
    );

    let reference_path = local_path(
        document
            .reference
            .as_ref()
            .context("demo project has no reference source")?,
    )?;
    let reference = FastaReference::open(reference_path)
        .with_context(|| format!("failed to open demo reference {}", reference_path.display()))?;
    anyhow::ensure!(
        reference
            .reference_dictionary()
            .id_by_name(CONTIG)
            .is_some(),
        "demo reference does not contain {CONTIG}"
    );
    Ok(())
}

fn demo_track_specs(output: &Path) -> Vec<TrackSpec> {
    DEMO_TRACKS
        .iter()
        .enumerate()
        .map(|(offset, demo)| TrackSpec {
            id: TrackId::new(u64::try_from(offset).unwrap() + 2),
            name: demo.name.to_owned(),
            path: output.join(demo.file),
            kind: demo.engine_kind,
        })
        .collect()
}

fn verify_tracks_opened(engine: &TrackEngine) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = None;
    let mut errors = Vec::new();
    let mut tdf_channels = None;
    while Instant::now() < deadline && ready.is_none() {
        match engine
            .try_receive()
            .context("demo track engine disconnected")?
        {
            Some(TrackEngineEvent::Ready { opened, failed }) => ready = Some((opened, failed)),
            Some(TrackEngineEvent::TdfChannelsDiscovered { track_names, .. }) => {
                tdf_channels = Some(track_names);
            }
            Some(TrackEngineEvent::Error { message, .. }) => errors.push(message),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let (opened, failed) = ready.context("timed out opening generated demo tracks")?;
    anyhow::ensure!(
        errors.is_empty(),
        "generated demo tracks failed to open: {}",
        errors.join("; ")
    );
    anyhow::ensure!(
        opened == DEMO_TRACKS.len() && failed == 0,
        "opened {opened} generated tracks with {failed} failures; expected {} tracks",
        DEMO_TRACKS.len()
    );
    anyhow::ensure!(
        tdf_channels.as_deref().is_some_and(|names| {
            names.len() == 2 && names[0] == "archive sample one" && names[1] == "archive sample two"
        }),
        "generated TDF did not expose both named channels"
    );
    Ok(())
}

fn verify_tracks_query(engine: &TrackEngine) -> Result<()> {
    let clock = GenerationClock::default();
    let request = PanelGeneration::new(PanelId::new(1), clock.current());
    engine
        .try_query(
            request,
            clock,
            CONTIG.to_owned(),
            u64::from(8_500_u32),
            u64::from(16_000_u32),
        )
        .context("failed to queue generated demo query")?;
    let snapshots = receive_track_snapshots(engine)?;
    anyhow::ensure!(
        snapshots.len() == DEMO_TRACKS.len(),
        "generated demo query returned {} snapshots instead of {}",
        snapshots.len(),
        DEMO_TRACKS.len()
    );
    for snapshot in snapshots {
        let has_data = match snapshot.data {
            TrackSnapshotData::Features(records) => !records.is_empty(),
            TrackSnapshotData::Variants(query) => !query.records.is_empty(),
            TrackSnapshotData::Quantitative(query) => !query.records.is_empty(),
            TrackSnapshotData::CopyNumber(query) => !query.segments.is_empty(),
            TrackSnapshotData::Mutations(query) => !query.records.is_empty(),
            TrackSnapshotData::Interactions(query) => !query.records.is_empty(),
            TrackSnapshotData::Gwas(query) => !query.records.is_empty(),
        };
        anyhow::ensure!(
            has_data,
            "generated track {} queried no data",
            snapshot.name
        );
    }
    Ok(())
}

fn receive_track_snapshots(engine: &TrackEngine) -> Result<Vec<rgv_engine::TrackSnapshot>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out querying generated demo tracks"
        );
        match engine
            .try_receive()
            .context("demo track engine disconnected")?
        {
            Some(TrackEngineEvent::QueryCompleted { tracks, .. }) => return Ok(tracks),
            Some(TrackEngineEvent::Error { message, .. }) => {
                anyhow::bail!("generated demo track query failed: {message}");
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

fn write_project(path: &Path) -> Result<()> {
    let mut document = SessionDocument::new(SessionLocus {
        contig: CONTIG.to_owned(),
        start: 8_500,
        end: 16_000,
    });
    document.genome_name = Some("RGV mixed-format demo".to_owned());
    document.reference = Some(local_source("../variants.fa"));
    document.alignment_tracks.push(SessionAlignmentTrack::new(
        TrackId::new(1),
        "Variant and specialist alignment truth".to_owned(),
        local_source("../variants.bam"),
    ));
    for (offset, demo) in DEMO_TRACKS.iter().enumerate() {
        let mut track = TrackConfig::new(
            TrackId::new(u64::try_from(offset).unwrap() + 2),
            demo.name.to_owned(),
            demo.kind,
            local_source(demo.file),
        );
        track.format = demo.format;
        document.tracks.push(track);
    }
    write_text(path, &document.to_json()?)
}

fn review_session_locus(review: AlignmentReviewLocus) -> SessionLocus {
    SessionLocus {
        contig: review.contig.to_owned(),
        start: review.start,
        end: review.end,
    }
}

fn write_alignment_review_project(path: &Path) -> Result<()> {
    let first = review_session_locus(ALIGNMENT_REVIEW_LOCI[ALIGNMENT_PANEL_REVIEW_INDEXES[0]]);
    let mut document = SessionDocument::new(first.clone());
    document.genome_name = Some("RGV synthetic alignment truth".to_owned());
    document.panels = ALIGNMENT_PANEL_REVIEW_INDEXES
        .iter()
        .enumerate()
        .map(|(offset, review_index)| {
            SessionViewPanel::new(
                PanelId::new(u64::try_from(offset).unwrap() + 1),
                review_session_locus(ALIGNMENT_REVIEW_LOCI[*review_index]),
            )
        })
        .collect();
    document.active_panel = PanelId::new(1);
    document.locus = first;

    let mut alignment = SessionAlignmentTrack::new(
        TrackId::new(1),
        "Short, RNA, long-read, and specialist alignment truth".to_owned(),
        local_source("../mixed.bam"),
    );
    alignment.height = 420.0;
    document.alignment_tracks.push(alignment);
    document.review_items = ALIGNMENT_REVIEW_LOCI
        .iter()
        .enumerate()
        .map(|(offset, review)| ReviewItem {
            id: u64::try_from(offset).unwrap() + 1,
            label: review.label.to_owned(),
            locus: review_session_locus(*review),
            status: ReviewStatus::Pending,
            notes: review.notes.to_owned(),
            assignment: None,
        })
        .collect();
    write_text(path, &document.to_json()?)
}

fn query_alignment_review_locus(
    source: &mut LocalAlignmentSource,
    review: AlignmentReviewLocus,
) -> Result<AlignmentQuery> {
    let contig = source
        .reference_dictionary()
        .id_by_name(review.contig)
        .with_context(|| format!("alignment review source has no {} contig", review.contig))?;
    let interval = GenomicInterval::new(contig, review.start, review.end)?;
    source
        .query(
            interval,
            QueryOptions {
                detail: QueryDetail::Full,
                collect_modifications: true,
                ..QueryOptions::default()
            },
            &|| true,
        )
        .with_context(|| format!("failed to query alignment review locus {}", review.label))
}

fn verify_alignment_review_project(path: &Path) -> Result<()> {
    let document = SessionDocument::read(path)
        .with_context(|| format!("failed to reopen {}", path.display()))?;
    verify_alignment_review_document(&document)?;
    let alignment_path = local_path(&document.alignment_tracks[0].source)?;
    let mut source = LocalAlignmentSource::open(alignment_path, &AlignmentOpenOptions::default())
        .with_context(|| {
        format!(
            "failed to open alignment review source {}",
            alignment_path.display()
        )
    })?;
    verify_alignment_review_semantics(&mut source)
}

fn verify_alignment_review_document(document: &SessionDocument) -> Result<()> {
    anyhow::ensure!(
        document.panels.len() == ALIGNMENT_PANEL_REVIEW_INDEXES.len(),
        "alignment review project restored {} panels instead of {}",
        document.panels.len(),
        ALIGNMENT_PANEL_REVIEW_INDEXES.len()
    );
    anyhow::ensure!(
        document.review_items.len() == ALIGNMENT_REVIEW_LOCI.len(),
        "alignment review project restored {} review items instead of {}",
        document.review_items.len(),
        ALIGNMENT_REVIEW_LOCI.len()
    );
    anyhow::ensure!(
        document.alignment_tracks.len() == 1 && document.tracks.is_empty(),
        "alignment review project must contain one alignment and no auxiliary tracks"
    );
    for (panel, review_index) in document.panels.iter().zip(ALIGNMENT_PANEL_REVIEW_INDEXES) {
        anyhow::ensure!(
            panel.locus == review_session_locus(ALIGNMENT_REVIEW_LOCI[*review_index]),
            "alignment review panel {} has the wrong locus",
            panel.id.get()
        );
    }
    for (item, review) in document.review_items.iter().zip(ALIGNMENT_REVIEW_LOCI) {
        anyhow::ensure!(
            item.label == review.label && item.locus == review_session_locus(*review),
            "alignment review item {} does not match its declared truth locus",
            item.id
        );
    }
    Ok(())
}

fn verify_alignment_review_semantics(source: &mut LocalAlignmentSource) -> Result<()> {
    let paired = query_alignment_review_locus(source, ALIGNMENT_REVIEW_LOCI[0])?;
    anyhow::ensure!(
        paired.pairs.eligible_proper_templates > 0
            && paired
                .batch
                .records()
                .any(|record| record.flags().is_paired()),
        "paired-read review panel contains no proper paired templates"
    );

    let rna = query_alignment_review_locus(source, ALIGNMENT_REVIEW_LOCI[2])?;
    anyhow::ensure!(
        rna.junctions.junctions.len() == 10_000
            && rna.junctions.provenance.spliced_records == 10_000,
        "RNA review panel does not retain the 10,000-junction truth"
    );

    let long = query_alignment_review_locus(source, ALIGNMENT_REVIEW_LOCI[4])?;
    anyhow::ensure!(
        long.batch
            .records()
            .any(|record| record.flags().is_supplementary()),
        "long-read review panel contains no supplementary alignment"
    );
    anyhow::ensure!(
        (0..long.batch.len()).any(|index| long.batch.has_auxiliary_tag(index, *b"MM")),
        "long-read review panel contains no modified-base tags"
    );
    anyhow::ensure!(
        (0..long.batch.len()).any(|index| {
            matches!(
                long.batch.sequence(index),
                Some(SequenceView::Present { bases, .. }) if bases.len() >= 10_000
            )
        }),
        "long-read review panel contains no read of at least 10,000 bases"
    );
    anyhow::ensure!(
        (0..long.batch.len()).any(|index| {
            long.materialize_record(index).is_ok_and(|record| {
                smrt_kinetics_series(&record, SmrtKineticsMode::SubreadIpd)
                    .is_ok_and(|series| !series.is_empty())
            })
        }),
        "long-read review panel contains no decodable SMRT kinetics"
    );
    anyhow::ensure!(
        (0..long.batch.len()).any(|index| {
            long.materialize_record(index).is_ok_and(|record| {
                summarize_sbx_record(&record, 10, 30).is_ok_and(|summary| {
                    summary
                        .concordant_insertions
                        .saturating_add(summary.partly_discordant_insertions)
                        .saturating_add(summary.fully_discordant_insertions)
                        > 0
                })
            })
        }),
        "long-read review panel contains no decodable SBX insertion evidence"
    );

    let long_cigar = query_alignment_review_locus(source, ALIGNMENT_REVIEW_LOCI[5])?;
    anyhow::ensure!(
        (0..long_cigar.batch.len()).any(|index| {
            long_cigar
                .batch
                .cigar(index)
                .is_some_and(|cigar| cigar.len() >= 70_000)
        }),
        "long-CIGAR review panel does not retain the 70,000-operation record"
    );
    Ok(())
}

fn write_manifest(output: &Path, project: &Path) -> Result<()> {
    let purposes = HashMap::from([
        ("archive.tdf", "multi-channel archived quantitative signal"),
        (
            "association.gwas",
            "GWAS Manhattan points and identifier search",
        ),
        ("cohort.seg", "multi-sample copy-number matrix"),
        ("features.bed.gz", "compressed BED12 annotation"),
        ("features.bigBed", "generic indexed annotation"),
        ("genes.bigGenePred", "indexed typed transcript annotation"),
        ("genes.gtf.gz", "compressed gene and CDS annotation"),
        ("interactions.interact", "directional UCSC interaction"),
        ("loops.bedpe", "paired-endpoint interaction arcs"),
        ("mutations.maf", "somatic mutation samples"),
        ("peaks.bigNarrowPeak", "indexed typed peak with summit"),
        ("peaks.narrowPeak", "text peak with summit"),
        ("regions.broadPeak", "text broad peak without summit"),
        (
            "rgv-demo-project.json",
            "ready-to-open mixed-format RGV project",
        ),
        (
            ALIGNMENT_REVIEW_PROJECT,
            "ready-to-open multi-panel alignment truth project",
        ),
        ("signal.bedGraph", "signed quantitative intervals"),
        ("signal.bigWig", "indexed quantitative intervals"),
        ("signal.wig", "variable-step WIG signal"),
        (
            "target-alignments.psl",
            "nucleotide target alignment blocks",
        ),
        ("transcripts.genePred", "text transcript and CDS model"),
        (
            "variants.bcf",
            "typed variants, structural evidence, and genotypes",
        ),
        ("variants.bcf.csi", "CSI index for the demo BCF"),
    ]);
    let mut files = purposes
        .into_iter()
        .map(|(name, purpose)| {
            let path = output.join(name);
            let bytes =
                fs::read(&path).with_context(|| format!("failed to hash {}", path.display()))?;
            Ok(DemoFile {
                path: name.to_owned(),
                sha256: sha256_bytes(&bytes),
                purpose: purpose.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = DemoManifest {
        schema_version: 1,
        locus: "chrVariants:8501-16000".to_owned(),
        project: project
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        files,
    };
    write_text(
        &output.join("manifest.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

const BIG_GENE_PRED_AUTOSQL: &str = r#"table bigGenePred
"bigGenePred transcripts"
(
string chrom;
uint chromStart;
uint chromEnd;
string name;
uint score;
char[1] strand;
uint thickStart;
uint thickEnd;
uint reserved;
int blockCount;
int[blockCount] blockSizes;
int[blockCount] chromStarts;
string name2;
string cdsStartStat;
string cdsEndStat;
int[blockCount] exonFrames;
string type;
string geneName;
string geneName2;
string geneType;
)
"#;

const BIG_NARROW_PEAK_AUTOSQL: &str = r#"table bigNarrowPeak
"bigNarrowPeak peaks"
(
string chrom;
uint chromStart;
uint chromEnd;
string name;
uint score;
char[1] strand;
float signalValue;
float pValue;
float qValue;
int peak;
)
"#;
