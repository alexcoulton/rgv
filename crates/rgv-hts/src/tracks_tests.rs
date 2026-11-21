use std::{
    collections::HashMap,
    fmt::Write as _,
    io::Cursor,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use bigtools::{BigBedWrite, beddata::BedParserStreamingIterator};
use rust_htslib::bcf::{Format, Header, Writer, index};
use tiny_http::{Header as HttpHeader, Method, Response, Server, StatusCode};

use super::*;

fn cohort_filter_fixture(
    samples: usize,
    records: usize,
    late_match: bool,
) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cohort.bcf");
    let mut header = Header::new();
    header.push_record(br"##contig=<ID=chr1,length=1000000>");
    header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
    header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Depth">"#);
    for index in 0..samples {
        header.push_sample(format!("sample{index}").as_bytes());
    }
    {
        let mut writer = Writer::from_path(&path, &header, false, Format::Bcf).unwrap();
        for position in 0..records {
            let mut record = writer.empty_record();
            record.set_rid(Some(0));
            record.set_pos(i64::try_from(position).unwrap());
            record.set_alleles(&[b"A", b"C"]).unwrap();
            let alleles = (0..samples)
                .flat_map(|index| {
                    [
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::Unphased(i32::from(!late_match || index == samples - 1)),
                    ]
                })
                .collect::<Vec<_>>();
            record.push_genotypes(&alleles).unwrap();
            let depths = (0..samples)
                .map(|index| if index == samples - 1 { 42 } else { 0 })
                .collect::<Vec<_>>();
            record.push_format_integer(b"DP", &depths).unwrap();
            writer.write(&record).unwrap();
        }
    }
    index::build(&path, None, 1, index::Type::Csi(14)).unwrap();
    (directory, path)
}

#[test]
fn variant_eligibility_does_not_depend_on_viewport_or_retained_genotype_budget() {
    let (_directory, path) = cohort_filter_fixture(256, 1001, false);
    let mut source = LocalVariantSource::open(path).unwrap();
    let settings = VariantTrackSettings {
        genotype: VariantGenotypeFilter::Heterozygous,
        ..VariantTrackSettings::default()
    };
    let broad = source
        .query_with_settings("chr1", 0, 1001, 2000, &settings)
        .unwrap();
    let narrow = source
        .query_with_settings("chr1", 977, 1001, 2000, &settings)
        .unwrap();
    assert_eq!(broad.records.len(), 1001);
    assert_eq!(broad.provenance.records_filtered, 0);
    assert_eq!(
        broad.provenance.genotype_cells_loaded,
        MAX_VARIANT_GENOTYPE_CELLS_PER_QUERY
    );
    assert!(broad.provenance.genotype_cells_omitted > 0);
    assert_eq!(narrow.records.len(), 24);
    assert!(
        narrow
            .records
            .iter()
            .all(|record| broad.records.iter().any(|b| b.start == record.start))
    );
}

#[test]
fn variant_predicates_find_samples_beyond_the_display_sample_cap() {
    let (_directory, path) = cohort_filter_fixture(MAX_GENOTYPE_SAMPLES_PER_RECORD + 1, 1, true);
    let mut source = LocalVariantSource::open(path).unwrap();
    let mut settings = VariantTrackSettings {
        genotype: VariantGenotypeFilter::Heterozygous,
        field_predicates: vec![VariantFieldPredicate {
            scope: VariantFieldScope::Format,
            key: "DP".into(),
            comparison: VariantFieldComparison::GreaterThan(40.0),
            missing: VariantMissingValueRule::Exclude,
        }],
        ..VariantTrackSettings::default()
    };
    let query = source
        .query_with_settings("chr1", 0, 1, 10, &settings)
        .unwrap();
    assert_eq!(query.records.len(), 1);
    assert_eq!(
        query.records[0].genotypes.len(),
        MAX_GENOTYPE_SAMPLES_PER_RECORD
    );
    settings.sample_subset_enabled = true;
    settings.sample_subset = vec!["sample0".into()];
    assert!(
        source
            .query_with_settings("chr1", 0, 1, 10, &settings)
            .unwrap()
            .records
            .is_empty()
    );
    settings.sample_subset = vec![format!("sample{}", MAX_GENOTYPE_SAMPLES_PER_RECORD)];
    assert_eq!(
        source
            .query_with_settings("chr1", 0, 1, 10, &settings)
            .unwrap()
            .records
            .len(),
        1
    );
}

const BIG_GENE_PRED_AUTOSQL: &str = r#"table bigGenePred
"bigGenePred gene models"
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

struct RemoteVariantFixture {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RemoteVariantFixture {
    fn start() -> Self {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let objects = HashMap::from([
            (
                "/variants.bcf".to_owned(),
                include_bytes!("../../../benchmark-data/demo-tracks/variants.bcf").to_vec(),
            ),
            (
                "/variants.bcf.csi".to_owned(),
                include_bytes!("../../../benchmark-data/demo-tracks/variants.bcf.csi").to_vec(),
            ),
        ]);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                let path = request.url().split('?').next().unwrap_or_default();
                let Some(data) = objects.get(path) else {
                    request.respond(Response::empty(StatusCode(404))).unwrap();
                    continue;
                };
                let response = if request.method() == &Method::Head {
                    Response::from_data(data.clone())
                } else if let Some((start, end)) = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| parse_http_range(header.value.as_str(), data.len()))
                {
                    Response::from_data(data[start..=end].to_vec())
                        .with_status_code(StatusCode(206))
                        .with_header(http_header(
                            "Content-Range",
                            &format!("bytes {start}-{end}/{}", data.len()),
                        ))
                } else {
                    Response::from_data(data.clone())
                }
                .with_header(http_header("Accept-Ranges", "bytes"));
                request.respond(response).unwrap();
            }
        });
        Self {
            url: format!("http://{address}/variants.bcf"),
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for RemoteVariantFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn http_header(name: &str, value: &str) -> HttpHeader {
    HttpHeader::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn parse_http_range(value: &str, length: usize) -> Option<(usize, usize)> {
    let value = value.strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = if end.is_empty() {
        length.checked_sub(1)?
    } else {
        end.parse::<usize>().ok()?.min(length.checked_sub(1)?)
    };
    (start <= end).then_some((start, end))
}

fn write_test_bigbed(path: &Path, autosql: &str, contents: &str) {
    let mut chromosome_sizes = HashMap::new();
    chromosome_sizes.insert("chr1".to_owned(), 1_000_000);
    chromosome_sizes.insert("chr2".to_owned(), 1_000_000);
    let mut writer = BigBedWrite::create_file(path, chromosome_sizes).unwrap();
    writer.autosql = Some(autosql.to_owned());
    writer.options.compress = false;
    let data = BedParserStreamingIterator::from_bed_file(Cursor::new(contents), false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    writer.write(data, runtime).unwrap();
}

#[test]
#[allow(clippy::float_cmp)]
fn bigbed_zoom_summaries_give_coverage_without_reading_features() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dense.bb");
    // 40,000 features of 20 bases every 10 bases on the first 400 kb of chr1 (depth 2 nearly
    // everywhere), and one lone feature far away.
    let mut contents = String::new();
    for index in 0..40_000_u32 {
        let start = index * 10;
        writeln!(contents, "chr1\t{start}\t{}\tf{index}", start + 20).unwrap();
    }
    contents.push_str("chr1\t900000\t900100\tlone\n");
    write_test_bigbed(
        &path,
        "table bed\n\"Dense\"\n(\nstring chrom;\nuint chromStart;\nuint chromEnd;\nstring name;\n)\n",
        &contents,
    );
    let mut source = BigBedSource::open(&path).unwrap();
    let reductions = source.zoom_reductions();
    assert!(!reductions.is_empty(), "the writer produced zoom levels");
    assert!(reductions.is_sorted());

    // A bounded detail read says "dense" after `limit` records, never the hard error.
    let (records, more) = source.query_limited("chr1", 0, 1_000_000, 1_000).unwrap();
    assert_eq!((records.len(), more), (1_000, true));
    let (records, more) = source
        .query_limited("chr1", 890_000, 1_000_000, 1_000)
        .unwrap();
    assert_eq!((records.len(), more), (1, false));
    assert_eq!(records[0].name.as_deref(), Some("lone"));

    // The summary of the whole contig at the coarsest level that is no coarser than asked.
    let asked = u64::from(*reductions.last().unwrap());
    let query = source
        .query_coverage_summary("chr1", 0, 1_000_000, asked)
        .unwrap()
        .unwrap();
    assert_eq!(
        query.provenance.summary_reduction_bases,
        Some(*reductions.last().unwrap())
    );
    assert!(!query.records.is_empty());
    assert!(!query.provenance.records_truncated);
    // Coverage is conserved: 40,000 * 20 + 100 feature bases in total.
    let covered = query
        .records
        .iter()
        .map(|record| f64::from(record.mean) * (record.end - record.start) as f64)
        .sum::<f64>();
    assert!((covered - 800_100.0).abs() < 800.0, "{covered}");
    for record in &query.records {
        assert!(record.minimum <= record.mean && record.mean <= record.maximum);
        assert!(record.maximum <= 2.0);
    }
    assert!(query.records.iter().any(|record| record.maximum == 2.0));
    // Nothing between the dense block and the lone feature.
    let gap = source
        .query_coverage_summary("chr1", 500_000, 800_000, asked)
        .unwrap()
        .unwrap();
    assert!(gap.records.iter().all(|record| record.mean == 0.0) || gap.records.is_empty());
    // Asking for finer than the file has reads its finest level and says which.
    let finest = source
        .query_coverage_summary("chr1", 0, 100_000, 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        finest.provenance.summary_reduction_bases,
        Some(reductions[0])
    );
}

#[test]
fn bedgraph_queries_are_zero_based_half_open_and_overlap_aware() {
    let source = TextQuantitativeSource::from_reader(
        std::io::Cursor::new(
            "track type=bedGraph\nchr1\t10\t20\t2.5\nchr1\t15\t40\t-1\nchr2\t0\t5\tNaN\n",
        ),
        QuantitativeFormat::BedGraph,
    )
    .unwrap();
    let query = source.query("chr1", 19, 21);
    assert_eq!(
        query.records,
        vec![
            QuantitativeRecord {
                start: 10,
                end: 20,
                value: 2.5,
            },
            QuantitativeRecord {
                start: 15,
                end: 40,
                value: -1.0,
            },
        ]
    );
    assert_eq!(query.provenance.records_examined, 2);
    assert!(!query.provenance.records_truncated);
    let non_finite = source.query("chr2", 0, 10);
    assert_eq!(non_finite.provenance.non_finite_records, 1);
}

#[test]
fn wig_fixed_and_variable_steps_convert_one_based_coordinates_exactly() {
    let source = TextQuantitativeSource::from_reader(
            std::io::Cursor::new(
                "fixedStep chrom=chr1 start=11 step=5 span=3\n1.5\n-2\nvariableStep chrom=chr1 span=2\n21 4\n31 8\n",
            ),
            QuantitativeFormat::Wig,
        )
        .unwrap();
    assert_eq!(
        source.query("chr1", 0, 100).records,
        vec![
            QuantitativeRecord {
                start: 10,
                end: 13,
                value: 1.5,
            },
            QuantitativeRecord {
                start: 15,
                end: 18,
                value: -2.0,
            },
            QuantitativeRecord {
                start: 20,
                end: 22,
                value: 4.0,
            },
            QuantitativeRecord {
                start: 30,
                end: 32,
                value: 8.0,
            },
        ]
    );
}

#[test]
fn wig_rejects_data_without_a_declaration_and_zero_coordinates() {
    assert!(
            TextQuantitativeSource::from_reader(
                std::io::Cursor::new("1.0\n"),
                QuantitativeFormat::Wig,
            )
            .is_err()
        );
    assert!(
        TextQuantitativeSource::from_reader(
            std::io::Cursor::new("variableStep chrom=chr1\n0 1.0\n"),
            QuantitativeFormat::Wig,
        )
        .is_err()
    );
}

#[test]
fn seg_preserves_samples_markers_and_one_based_inclusive_coordinates() {
    let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
        "ID\tchrom\tloc.start\tloc.end\tnum.mark\tseg.mean\n\
             tumour\tchr1\t11\t20\t7\t-0.5\n\
             normal\tchr1\t21\t30\t9\t2.0\n\
             ignored\tchr1\t31\t40\t3\tnot-a-number\n",
    ))
    .unwrap();
    let query = source.query("chr1", 0, 100, &CopyNumberTrackSettings::default());
    assert_eq!(query.samples, vec!["tumour", "normal"]);
    assert_eq!(query.segments.len(), 2);
    assert_eq!((query.segments[0].start, query.segments[0].end), (10, 20));
    assert_eq!(query.segments[0].marker_count, Some(7));
    assert_eq!(
        query.inferred_value_semantics,
        CopyNumberValueSemantics::LogRatio
    );
    assert_eq!(query.provenance.source_rows_ignored, 1);
}

#[test]
fn seg_track_coords_zero_is_honoured() {
    let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
        "#track type=seg coords=0\nSample\tChromosome\tStart\tEnd\tValue\ns1\tchr1\t0\t10\t2\n",
    ))
    .unwrap();
    let query = source.query("chr1", 0, 1, &CopyNumberTrackSettings::default());
    assert_eq!((query.segments[0].start, query.segments[0].end), (0, 10));
    assert_eq!(
        query.inferred_value_semantics,
        CopyNumberValueSemantics::Absolute
    );
}

#[test]
fn seg_sort_values_are_base_weighted_at_the_persisted_locus() {
    let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
        "sample\tchrom\tstart\tend\tmarkers\tvalue\n\
             a\tchr1\t1\t10\t2\t1\n\
             a\tchr1\t11\t30\t4\t3\n\
             b\tchr1\t1\t30\t6\t2\n",
    ))
    .unwrap();
    let query = source.query(
        "chr1",
        0,
        5,
        &CopyNumberTrackSettings {
            sort_mode: CopyNumberSortMode::Amplification,
            sort_locus: Some(rgv_core::CopyNumberSortLocus {
                contig: "chr1".to_owned(),
                start: 0,
                end: 30,
            }),
            ..CopyNumberTrackSettings::default()
        },
    );
    assert_eq!(query.segments.len(), 2);
    assert!((query.sample_sort_values[0].unwrap() - (70.0 / 30.0)).abs() < 1.0e-6);
    assert_eq!(query.sample_sort_values[1], Some(2.0));
    assert_eq!(query.provenance.sort_records_examined, 3);
}

#[test]
fn mutation_maf_preserves_samples_coordinates_and_ordered_annotations() {
    let source = TextMutationSource::from_reader(std::io::Cursor::new(
            "#version 2.4\n\
             Hugo_Symbol\tEntrez_Gene_Id\tCenter\tNCBI_Build\tChromosome\tStart_Position\tEnd_Position\tStrand\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tdbSNP_RS\tValidation_Status\tTumor_Sample_Barcode\tHGVSp_Short\n\
             TP53\t7157\tGDC\tGRCh38\tchr17\t101\t101\t+\tMissense_Mutation\tSNP\tC\tC\tT\tnovel\tUnknown\tTUMOUR-A\tp.R1W\n\
             EGFR\t1956\tGDC\tGRCh38\tchr17\t201\t202\t+\tFrame_Shift_Del\tDEL\tAC\t-\t-\tnovel\tUnknown\tTUMOUR-B\tp.E2fs\n\
             ALK\t238\tGDC\tGRCh38\tchr17\t301\t300\t+\tFrame_Shift_Ins\tINS\t-\t-\tA\tnovel\tUnknown\tTUMOUR-A\tp.A3fs\n\
             BAD\t0\tGDC\tGRCh38\tchr17\tnot-a-position\t300\t+\tSilent\tSNP\tA\tA\tG\tnovel\tUnknown\tTUMOUR-A\tp.=\n",
        ))
        .unwrap();

    let query = source.query("chr17", 100, 202);

    assert_eq!(query.samples, vec!["TUMOUR-A", "TUMOUR-B"]);
    assert_eq!(query.records.len(), 2);
    assert_eq!((query.records[0].start, query.records[0].end), (100, 101));
    assert_eq!(query.records[0].gene, "TP53");
    assert_eq!(query.records[0].classification, "Missense_Mutation");
    assert_eq!(query.records[0].variant_type, "SNP");
    assert_eq!(query.records[0].tumor_allele_two, "T");
    assert_eq!(
        query.headers[usize::from(query.records[0].annotations[0].column_index)],
        "Hugo_Symbol"
    );
    assert_eq!(query.provenance.source_rows_ignored, 1);
    assert_eq!(source.query("chr17", 101, 200).records.len(), 0);
    let insertion = source.query("chr17", 300, 301);
    assert_eq!(insertion.records.len(), 1);
    assert_eq!(
        (insertion.records[0].start, insertion.records[0].end),
        (300, 301)
    );
    assert_eq!(
        (
            insertion.records[0].source_start,
            insertion.records[0].source_end,
            insertion.records[0].insertion_boundary,
        ),
        (301, 300, true)
    );
}

#[test]
fn mutation_maf_accepts_uuid_sample_identity_and_requires_named_columns() {
    let header = "Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_UUID\n";
    let source = TextMutationSource::from_reader(std::io::Cursor::new(format!(
        "{header}KRAS\t12\t25398285\t25398285\tMissense_Mutation\tSNP\tC\tC\tA\tcase-uuid\n"
    )))
    .unwrap();
    assert_eq!(
        source.query("12", 25_398_284, 25_398_285).samples,
        ["case-uuid"]
    );
    assert!(
        TextMutationSource::from_reader(std::io::Cursor::new(
            "Chromosome\tStart_Position\tEnd_Position\nchr1\t1\t1\n"
        ))
        .is_err()
    );
}

#[test]
fn mutation_maf_rejects_an_unbounded_source_line() {
    let oversized = format!("{}\n", "x".repeat(MAX_MUTATION_LINE_BYTES));
    assert!(matches!(
        TextMutationSource::from_reader(std::io::Cursor::new(oversized)),
        Err(TrackError::ResourceLimit(_))
    ));
}

#[test]
fn mutation_maf_opens_gzip_sources() {
    use std::io::Write as _;

    let file = tempfile::Builder::new()
        .suffix(".maf.gz")
        .tempfile()
        .unwrap();
    let mut encoder =
        flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
    encoder
            .write_all(
                b"Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_Barcode\nTP53\tchr17\t101\t101\tMissense_Mutation\tSNP\tC\tC\tT\tTUMOUR-A\n",
            )
            .unwrap();
    encoder.finish().unwrap();

    let source = TextMutationSource::open(file.path()).unwrap();

    assert_eq!(source.query("chr17", 100, 101).records.len(), 1);
}

#[test]
fn bedpe_query_indexes_both_endpoints_and_deduplicates_intrachromosomal_spans() {
    let source = TextInteractionSource::from_reader(
        std::io::Cursor::new(
            "chr1\t10\t20\tchr1\t90\t100\tintra\t25\t+\t-\nchr1\t30\t40\tchr2\t50\t60\tcross\t5\n",
        ),
        InteractionFormat::BedPe,
    )
    .unwrap();
    let query = source.query(
        "chr1",
        15,
        95,
        &InteractionTrackSettings {
            minimum_score: Some(10.0),
            ..InteractionTrackSettings::default()
        },
    );
    assert_eq!(query.records.len(), 1);
    assert_eq!(query.records[0].name.as_deref(), Some("intra"));
    assert_eq!(query.records[0].source.strand, Strand::Forward);
    assert_eq!(query.records[0].target.strand, Strand::Reverse);
    assert!(!query.records[0].directional);
    assert!(query.provenance.duplicate_entries_ignored >= 1);
    assert_eq!(query.provenance.records_filtered, 1);

    let partner = source.query("chr2", 55, 56, &InteractionTrackSettings::default());
    assert_eq!(partner.records[0].name.as_deref(), Some("cross"));
}

#[test]
fn interact_parser_preserves_bed5_plus_13_semantics() {
    let source = TextInteractionSource::from_reader(
            std::io::Cursor::new(
                "chr1\t10\t100\tsnp/gene/liver\t700\t0.625\tliver\t#7A67EE\tchr1\t10\t11\trs1\t.\tchr1\t99\t100\tGENE1\t+\n",
            ),
            InteractionFormat::Interact,
        )
        .unwrap();
    let query = source.query("chr1", 50, 51, &InteractionTrackSettings::default());
    let record = &query.records[0];
    assert_eq!(record.source.start, 10);
    assert_eq!(record.target.end, 100);
    assert_eq!(record.source.name.as_deref(), Some("rs1"));
    assert_eq!(record.target.name.as_deref(), Some("GENE1"));
    assert_eq!(record.value, Some(0.625));
    assert_eq!(record.experiment.as_deref(), Some("liver"));
    assert_eq!(record.color, Some([0x7a, 0x67, 0xee, 230]));
    assert!(record.directional);
}

#[test]
fn bed12_blocks_use_zero_based_offsets() {
    let record = parse_bed("chr1\t100\t200\ttx\t42\t+\t100\t200\t0\t2\t20,30\t0,70").unwrap();
    assert_eq!(record.blocks, vec![(100, 120), (170, 200)]);
    assert_eq!(record.coding_interval, Some((100, 200)));
    assert_eq!(record.strand, Strand::Forward);
}

#[test]
fn gene_pred_preserves_transcript_cds_and_exons() {
    let record = parse_gene_pred(
        "tx1\tchr2\t-\t100\t400\t150\t350\t3\t100,200,320,\t180,260,400,",
        false,
    )
    .unwrap();
    assert_eq!((record.start, record.end), (100, 400));
    assert_eq!(record.name.as_deref(), Some("tx1"));
    assert_eq!(record.strand, Strand::Reverse);
    assert_eq!(record.blocks, [(100, 180), (200, 260), (320, 400)]);
    assert_eq!(record.coding_interval, Some((150, 350)));
    assert_eq!(record.feature_type.as_deref(), Some("transcript"));
    assert_eq!(
        attribute_value(&record.attributes, &["transcript_id"]).as_deref(),
        Some("tx1")
    );
}

#[test]
fn extended_gene_pred_and_ref_flat_keep_gene_and_transcript_names() {
    let extended = parse_gene_pred(
        "585\ttx2\tchr3\t+\t10\t80\t20\t70\t2\t10,50,\t30,80,\t900\tGENE2\tcmpl\tincmpl\t0,2,",
        false,
    )
    .unwrap();
    assert_eq!(extended.name.as_deref(), Some("GENE2"));
    assert_eq!(extended.display_label(), Some("GENE2"));
    assert_eq!(extended.score, Some(900.0));
    assert_eq!(
        attribute_value(&extended.attributes, &["gene_name"]).as_deref(),
        Some("GENE2")
    );

    let ref_flat = parse_gene_pred("GENE3\ttx3\tchr4\t+\t0\t100\t0\t0\t1\t0,\t100,", true).unwrap();
    assert_eq!(ref_flat.name.as_deref(), Some("GENE3"));
    assert_eq!(ref_flat.coding_interval, None);
    assert_eq!(
        attribute_value(&ref_flat.attributes, &["transcript_id"]).as_deref(),
        Some("tx3")
    );
}

#[test]
fn feature_presentation_identity_uses_symbols_without_changing_plain_bed_names() {
    let gene = parse_gff(
        "chr1\tsrc\tgene\t11\t50\t.\t+\t.\tID=ENSG1;gene_name=ACTB;gene_id=ENSG1",
        false,
    )
    .unwrap();
    assert_eq!(gene.presentation_identity().display_symbol, Some("ACTB"));
    assert_eq!(gene.presentation_identity().gene_id, Some("ENSG1"));
    assert_eq!(gene.display_label(), Some("ACTB"));

    let bed = parse_bed("chr1\t10\t50\tplain-feature").unwrap();
    assert_eq!(bed.presentation_identity().display_symbol, None);
    assert_eq!(
        bed.presentation_identity().record_name,
        Some("plain-feature")
    );
    assert_eq!(bed.display_label(), Some("plain-feature"));
}

#[test]
fn gene_pred_rejects_mismatched_or_overlapping_exons() {
    assert!(parse_gene_pred("tx\tchr1\t+\t0\t100\t10\t90\t2\t0,50,\t25,", false,).is_err());
    assert!(parse_gene_pred("tx\tchr1\t+\t0\t100\t10\t90\t2\t0,20,\t30,40,", false,).is_err());
}

#[test]
fn big_gene_pred_preserves_binary_schema_transcript_semantics() {
    let record = parse_bigbed_record(
            "chr1",
            &bigtools::BedEntry {
                start: 100,
                end: 220,
                rest: "tx1\t900\t+\t120\t200\t0\t3\t20,30,20,\t0,60,100,\ttx1-alt\tcmpl\tincmpl\t0,1,2,\tcoding\tGENE1\tGENE1-alt\tprotein_coding".to_owned(),
            },
            BigBedFeatureFormat::BigGenePred,
        )
        .unwrap();

    assert_eq!(record.name.as_deref(), Some("GENE1-alt"));
    assert_eq!(record.blocks, [(100, 120), (160, 190), (200, 220)]);
    assert_eq!(record.coding_interval, Some((120, 200)));
    assert_eq!(record.strand, Strand::Forward);
    assert_eq!(record.score, Some(900.0));
    assert_eq!(
        attribute_value(&record.attributes, &["transcript_id"]).as_deref(),
        Some("tx1")
    );
    assert_eq!(
        attribute_value(&record.attributes, &["gene_type"]).as_deref(),
        Some("protein_coding")
    );
    assert!(feature_search_terms(&record).contains(&"tx1-alt".to_owned()));
}

#[test]
fn big_gene_pred_prefers_the_human_readable_gene_symbol() {
    let record = parse_big_gene_pred_record(
        "chr1",
        100,
        220,
        "ENST000001	900	+	120	200	0	3	20,30,20,	0,60,100,	ENST000001	cmpl	cmpl	0,1,2,	coding	ENSG000001	GENE1	protein_coding",
    )
    .unwrap();

    assert_eq!(record.name.as_deref(), Some("GENE1"));
    assert_eq!(
        attribute_value(&record.attributes, &["gene_id"]).as_deref(),
        Some("ENSG000001")
    );
    assert_eq!(record.display_label(), Some("GENE1"));
}

#[test]
fn big_gene_pred_accepts_and_preserves_extension_columns() {
    let record = parse_big_gene_pred_record(
        "chr1",
        100,
        220,
        "tx1\t900\t+\t120\t200\t8,81,156\t3\t20,30,20,\t0,60,100,\ttx1-alt\tcmpl\tincmpl\t0,1,2,\tcoding\tGENE1\tGENE1-alt\tprotein_coding\tMANE Select\tNM_001",
    )
    .unwrap();

    assert!(
        record
            .attributes
            .contains(&("extension_field_1".to_owned(), "MANE Select".to_owned()))
    );
    assert!(
        record
            .attributes
            .contains(&("extension_field_2".to_owned(), "NM_001".to_owned()))
    );
    assert!(
        record
            .attributes
            .contains(&("item_rgb".to_owned(), "8,81,156".to_owned()))
    );
}

#[test]
fn big_gene_pred_rejects_malformed_blocks_frames_status_and_bounds() {
    let parse =
        |rest: &str| parse_big_gene_pred_record("chr1", 100, 220, rest).map(|record| record.blocks);
    assert!(parse("tx\t0\t+\t100\t200\t0\t2\t50,50,\t0,25,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
    assert!(parse("tx\t0\t+\t100\t200\t0\t2\t20,20,\t0,50,\ttx\tbad\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
    assert!(parse("tx\t0\t+\t100\t200\t0\t2\t20,20,\t0,50,\ttx\tcmpl\tcmpl\t0,\tcoding\tGENE\t.\tprotein_coding").is_err());
    assert!(parse("tx\t0\t+\t90\t200\t0\t2\t20,20,\t0,50,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
}

#[test]
fn narrow_peak_preserves_statistics_and_absolute_summit() {
    let record = parse_narrow_peak("chr1\t100\t200\tp1\t750\t.\t18.25\t4.5\t-1\t37").unwrap();
    assert_eq!((record.start, record.end), (100, 200));
    assert_eq!(record.score, Some(750.0));
    assert_eq!(
        record.peak,
        Some(PeakMetrics {
            signal_value: 18.25,
            p_value: Some(4.5),
            q_value: None,
            summit: Some(137),
        })
    );
    assert_eq!(record.feature_type.as_deref(), Some("narrowPeak"));
}

#[test]
fn narrow_peak_rejects_out_of_interval_summits_and_invalid_missing_values() {
    assert!(parse_narrow_peak("chr1\t100\t200\tp1\t0\t.\t1\t-1\t-1\t100").is_err());
    assert!(parse_narrow_peak("chr1\t100\t200\tp1\t0\t.\t1\t-2\t-1\t-1").is_err());
}

#[test]
fn broad_peak_preserves_statistics_without_inventing_a_summit() {
    let record =
        parse_broad_peak("chr1\t100\t250\tregion-a\t116\t.\t4.89716\t3.70716\t-1").unwrap();
    assert_eq!((record.start, record.end), (100, 250));
    assert_eq!(record.feature_type.as_deref(), Some("broadPeak"));
    assert_eq!(record.score, Some(116.0));
    assert_eq!(record.peak.unwrap().summit, None);
    assert_eq!(record.peak.unwrap().q_value, None);
    assert!(parse_broad_peak("chr1\t100\t250\tbad\t1001\t.\t1\t-1\t-1").is_err());
    assert!(parse_broad_peak("chr1\t100\t250\tbad\t1\tx\t1\t-1\t-1").is_err());
}

#[test]
fn psl_preserves_target_blocks_identity_counts_and_query_name() {
    let record = parse_psl(
            "70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,",
        )
        .unwrap();
    assert_eq!((record.start, record.end), (100, 190));
    assert_eq!(record.contig, "chr1");
    assert_eq!(record.name.as_deref(), Some("query-1"));
    assert_eq!(record.feature_type.as_deref(), Some("PSL alignment"));
    assert_eq!(record.blocks, [(100, 130), (140, 190)]);
    assert_eq!(record.strand, Strand::Forward);
    assert_eq!(record.score, Some(937.5));
    assert_eq!(
        attribute_value(&record.attributes, &["mismatches"]).as_deref(),
        Some("5")
    );
}

#[test]
fn psl_maps_reverse_query_and_target_block_coordinates() {
    let query_reverse = parse_psl(
            "38\t0\t0\t0\t1\t14\t1\t32\t-\tquery-minus\t61\t4\t56\tchr1\t1000\t110\t180\t2\t20,18,\t5,39,\t110,162,",
        )
        .unwrap();
    assert_eq!(query_reverse.strand, Strand::Reverse);
    assert_eq!(query_reverse.blocks, [(110, 130), (162, 180)]);

    let target_reverse = parse_psl(
            "59\t9\t0\t0\t1\t823\t1\t96\t+-\tFS_CONTIG_48080_1\t1955\t171\t1062\tchr22\t47748585\t13073589\t13073753\t2\t48,20,\t171,1042,\t34674832,34674976,",
        )
        .unwrap();
    assert_eq!(target_reverse.strand, Strand::Reverse);
    assert_eq!(
        target_reverse.blocks,
        [(13_073_589, 13_073_609), (13_073_705, 13_073_753)]
    );
}

#[test]
fn psl_source_skips_standard_header_and_rejects_malformed_block_arrays() {
    let source = TextFeatureSource::from_reader(
            std::io::Cursor::new(
                "psLayout version 3\n\nmatch mis- rep. N's Q gap count Q gap bases T gap count T gap bases strand Q name Q size Q start Q end T name T size T start T end block count blockSizes qStarts tStarts\n---------------------------------------------------------------------------------------------------------------------------------------------------------------\n70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,\n",
            ),
            FeatureFormat::Psl,
        )
        .unwrap();
    assert_eq!(source.query("chr1", 120, 121).unwrap().len(), 1);
    assert_eq!(source.search("query-1", 10).hits.len(), 1);
    assert!(
            parse_psl("70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t80\tchr1\t1000\t100\t190\t2\t30,\t0,40,\t100,150,")
                .is_err()
        );
    assert!(
            parse_psl("110\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t200\t0\t123\tchr1\t1000\t100\t230\t2\t60,60,\t0,63,\t100,120,")
                .is_err()
        );
    assert!(
        parse_psl(
            "30\t0\t0\t0\t0\t0\t0\t0\t+\tprotein\t30\t0\t30\tchr1\t1000\t100\t190\t1\t30,\t0,\t100,"
        )
        .unwrap_err()
        .contains("translated protein PSL")
    );
}

#[test]
fn big_narrow_peak_preserves_binary_statistics_and_source() {
    let record = parse_bigbed_record(
        "chr2",
        &bigtools::BedEntry {
            start: 500,
            end: 600,
            rest: "peak-a\t750\t.\t18.25\t4.5\t-1\t37".to_owned(),
        },
        BigBedFeatureFormat::BigNarrowPeak,
    )
    .unwrap();
    assert_eq!(record.feature_type.as_deref(), Some("bigNarrowPeak"));
    assert_eq!(record.peak.unwrap().summit, Some(537));
    assert_eq!(
        attribute_value(&record.attributes, &["source_format"]).as_deref(),
        Some("bigNarrowPeak")
    );
}

#[test]
fn generated_bigbed_truth_queries_typed_models_and_supports_bounded_search() {
    let directory = tempfile::tempdir().unwrap();
    let gene_path = directory.path().join("genes.bb");
    write_test_bigbed(
        &gene_path,
        BIG_GENE_PRED_AUTOSQL,
        "chr1\t100\t220\ttx1\t900\t+\t120\t200\t0\t3\t20,30,20,\t0,60,100,\ttx1-alt\tcmpl\tincmpl\t0,1,2,\tcoding\tGENE1\tGENE1-alt\tprotein_coding\n\
             chr2\t300\t380\ttx2\t500\t-\t300\t300\t0\t2\t20,20,\t0,60,\ttx2-alt\tnone\tnone\t-1,-1,\tnoncoding\tGENE2\t.\tlncRNA\n",
    );
    let peak_path = directory.path().join("peaks.bb");
    write_test_bigbed(
        &peak_path,
        BIG_NARROW_PEAK_AUTOSQL,
        "chr1\t150\t250\tpeak-a\t750\t.\t18.25\t4.5\t-1\t37\n\
             chr2\t500\t600\tpeak-b\t200\t-\t2.5\t-1\t3.25\t-1\n",
    );

    let mut genes =
        BigBedSource::open_with_format(&gene_path, BigBedFeatureFormat::BigGenePred).unwrap();
    let queried = genes.query("chr1", 110, 210).unwrap();
    assert_eq!(queried.len(), 1);
    assert_eq!(queried[0].name.as_deref(), Some("GENE1-alt"));
    assert_eq!(queried[0].blocks, [(100, 120), (160, 190), (200, 220)]);
    let search = genes.search("tx1-alt", 10).unwrap();
    assert_eq!(search.hits.len(), 1);
    assert_eq!(search.hits[0].record.name.as_deref(), Some("GENE1-alt"));
    assert!(!search.scan_truncated);
    assert_eq!(
        genes
            .adjacent_feature("chr1", 0, true)
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("GENE1-alt")
    );
    assert_eq!(
        genes
            .adjacent_feature("chr1", 221, false)
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("GENE1-alt")
    );

    let mut peaks =
        BigBedSource::open_with_format(&peak_path, BigBedFeatureFormat::BigNarrowPeak).unwrap();
    let queried = peaks.query("chr1", 0, 1_000).unwrap();
    assert_eq!(queried[0].peak.unwrap().summit, Some(187));
    assert_eq!(peaks.search("peak-b", 10).unwrap().hits.len(), 1);

    assert!(matches!(
        BigBedSource::open_with_format(&gene_path, BigBedFeatureFormat::BigNarrowPeak),
        Err(TrackError::InvalidSchema(_))
    ));
    assert!(matches!(
        ensure_bigbed_query_capacity(MAX_FEATURE_RECORDS_PER_QUERY),
        Err(TrackError::ResourceLimit(_))
    ));
}

#[test]
fn malformed_binary_gene_record_fails_with_its_indexed_location() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("malformed.bb");
    write_test_bigbed(
        &path,
        BIG_GENE_PRED_AUTOSQL,
        "chr1\t100\t220\ttx\t0\t+\t100\t200\t0\t2\t50,50,\t0,25,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding\n",
    );
    let mut source =
        BigBedSource::open_with_format(&path, BigBedFeatureFormat::BigGenePred).unwrap();
    assert!(matches!(
        source.query("chr1", 0, 1_000),
        Err(TrackError::MalformedRecord { ref contig, start: 100, .. }) if contig == "chr1"
    ));
}

#[test]
fn text_feature_sources_reject_unbounded_lines() {
    let oversized = format!("{}\n", "x".repeat(MAX_FEATURE_LINE_BYTES));
    assert!(matches!(
        TextFeatureSource::from_reader(std::io::Cursor::new(oversized), FeatureFormat::GenePred),
        Err(TrackError::ResourceLimit(_))
    ));
}

#[test]
fn feature_search_indexes_names_aliases_and_transcripts_deterministically() {
    let source = TextFeatureSource::from_reader(
            std::io::Cursor::new(
                "chr2\tsrc\tgene\t101\t200\t.\t+\t.\tID=g2;Name=BRCA2;Alias=FANCD1\n\
                 chr1\tsrc\ttranscript\t11\t50\t.\t+\t.\tID=tx1;gene_name=BRCA1;transcript_id=BRCA1-201\n",
            ),
            FeatureFormat::Gff3,
        )
        .unwrap();

    let prefix = source.search("brca", 10);
    assert_eq!(prefix.hits.len(), 2);
    assert_eq!(prefix.hits[0].record.contig, "chr1");
    assert_eq!(prefix.hits[1].record.contig, "chr2");
    assert!(!prefix.index_truncated);
    assert!(!prefix.scan_truncated);

    let alias = source.search("fancd1", 10);
    assert_eq!(alias.hits[0].record.name.as_deref(), Some("BRCA2"));
    let transcript = source.search("brca1-201", 10);
    assert_eq!(transcript.hits[0].record.start, 10);
    assert_eq!(
        source
            .adjacent_feature("chr1", 0, true)
            .and_then(|record| record.name),
        Some("BRCA1".to_owned())
    );
    assert_eq!(
        source
            .adjacent_feature("chr2", 500, false)
            .and_then(|record| record.name),
        Some("BRCA2".to_owned())
    );
}

#[test]
fn compressed_gene_pred_sources_open_and_query() {
    use std::io::Write as _;

    let file = tempfile::Builder::new()
        .suffix(".genePred.gz")
        .tempfile()
        .unwrap();
    let mut encoder =
        flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
    encoder
        .write_all(b"tx1\tchr1\t+\t0\t100\t10\t90\t1\t0,\t100,\n")
        .unwrap();
    encoder.finish().unwrap();

    let source = TextFeatureSource::open(file.path(), FeatureFormat::GenePred).unwrap();

    assert_eq!(source.query("chr1", 0, 1).unwrap().len(), 1);
}

#[test]
fn compressed_psl_sources_open_query_and_search() {
    use std::io::Write as _;

    let file = tempfile::Builder::new()
        .suffix(".psl.gz")
        .tempfile()
        .unwrap();
    let mut encoder =
        flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
    encoder
            .write_all(b"70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,\n")
            .unwrap();
    encoder.finish().unwrap();

    let source = TextFeatureSource::open(file.path(), FeatureFormat::Psl).unwrap();

    assert_eq!(source.query("chr1", 120, 121).unwrap().len(), 1);
    assert_eq!(source.search("query-1", 10).hits.len(), 1);
}

#[test]
fn gff_coordinates_become_zero_based_half_open() {
    let record = parse_gff(
        "chr1\tsrc\tgene\t101\t200\t.\t-\t.\tID=g1;Name=GENE1",
        false,
    )
    .unwrap();
    assert_eq!((record.start, record.end), (100, 200));
    assert_eq!(record.name.as_deref(), Some("GENE1"));
    assert_eq!(record.strand, Strand::Reverse);
}

#[test]
fn gtf_names_are_decoded() {
    let record = parse_gff(
        "chr1\tsrc\texon\t1\t20\t1.5\t+\t.\tgene_id \"g1\"; gene_name \"ABC\";",
        true,
    )
    .unwrap();
    assert_eq!(record.name.as_deref(), Some("ABC"));
    assert_eq!(record.score, Some(1.5));
}

/// Writes `text` bgzip-compressed with a tabix index built by `HTSlib` (VCF preset: sequence
/// in column 1, position in column 2, `#` comment lines).
fn write_tabix_indexed_text(path: &Path, text: &[u8]) {
    use std::io::Write as _;
    let mut writer = rust_htslib::bgzf::Writer::from_path(path).unwrap();
    writer.write_all(text).unwrap();
    drop(writer);
    index::build(path, None, 1, index::Type::Tbx).unwrap();
}

#[test]
fn non_utf8_lines_of_an_indexed_annotation_are_errors_not_panics() {
    let directory = tempfile::tempdir().unwrap();
    let header = b"##fileformat=VCFv4.2\n##contig=<ID=chr1,length=1000000>\n".to_vec();
    let columns = b"#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n";

    // A comment line that is not UTF-8: `rust-htslib` panics while opening the file.
    let comment = directory.path().join("comment.gff.gz");
    let mut text = header.clone();
    text.extend_from_slice(b"##note=caf\xe9 \xff\xfe\n");
    text.extend_from_slice(columns);
    text.extend_from_slice(b"chr1\t100\ta\tA\tT\t.\t.\t.\n");
    write_tabix_indexed_text(&comment, &text);
    let Err(error) = TextFeatureSource::open(&comment, FeatureFormat::Gff3) else {
        panic!("a non-UTF-8 comment line must be refused");
    };
    assert!(error.to_string().contains("not valid UTF-8"), "{error}");

    // A data line that is not UTF-8: `rust-htslib` panics while reading the region.
    let data = directory.path().join("data.gff.gz");
    let mut text = header;
    text.extend_from_slice(columns);
    text.extend_from_slice(b"chr1\t100\t\xff\xfe\tA\tT\t.\t.\t.\n");
    text.extend_from_slice(b"chr1\t900000\tb\tA\tT\t.\t.\t.\n");
    write_tabix_indexed_text(&data, &text);
    let source = TextFeatureSource::open(&data, FeatureFormat::Gff3).unwrap();
    assert!(source.is_indexed());
    let Err(error) = source.query("chr1", 0, 1_000) else {
        panic!("a non-UTF-8 data line must be an error");
    };
    assert!(error.to_string().contains("not UTF-8"), "{error}");
    // The reader survives: a later query is answered (its lines do not parse as GFF and are
    // counted as skipped, which is the point of asking again).
    assert!(source.query("chr1", 899_000, 901_000).unwrap().is_empty());
    assert!(source.query("chr1", 0, 1_000).is_err());
}

#[test]
fn a_non_utf8_name_htslib_adds_to_the_variant_header_is_an_error_not_a_panic() {
    // Found by the mutation sweep over the `vcf_in_memory` fuzz entry point. The header is
    // valid UTF-8; the contig of the record is not declared, so HTSlib adds it to the header
    // as it stands, and `rust-htslib` unwraps the conversion of every header value.
    let directory = tempfile::tempdir().unwrap();
    for (name, line) in [
        ("contig.vcf", &b"\xff\t100\t.\tA\tT\t.\t.\t.\n"[..]),
        ("filter.vcf", &b"chr1\t100\t.\tA\tT\t.\tq\xff\t.\n"[..]),
    ] {
        let path = directory.path().join(name);
        let mut text =
            b"##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n".to_vec();
        text.extend_from_slice(line);
        std::fs::write(&path, text).unwrap();
        let Err(error) = LocalVariantSource::open(&path) else {
            panic!("{name}: a non-UTF-8 name must be refused");
        };
        assert!(
            error.to_string().contains("is not valid UTF-8"),
            "{name}: {error}"
        );
    }
}

#[test]
fn tabix_indexed_annotations_are_read_on_demand() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/indexed.gtf.gz");
    let source = TextFeatureSource::open(&path, FeatureFormat::Gtf).unwrap();
    assert!(source.is_indexed());
    let mut contigs = source.contigs().collect::<Vec<_>>();
    contigs.sort_unstable();
    assert_eq!(contigs, ["chr1", "chr2"]);

    // One-based inclusive GTF rows become zero-based half-open records.
    let first = source.query("chr1", 0, 6_000).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|record| (record.start, record.end, record.feature_type.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (1_000, 1_200, Some("exon")),
            (1_000, 5_000, Some("gene")),
            (1_000, 5_000, Some("transcript")),
            (4_800, 5_000, Some("exon")),
        ]
    );
    // Overlap is exact: a query touching only the gap between the exons returns the spans.
    assert_eq!(source.query("chr1", 2_000, 2_001).unwrap().len(), 2);
    assert!(source.query("chr1", 5_000, 6_000).unwrap().is_empty());
    assert!(source.query("chr3", 0, 1_000).unwrap().is_empty());
    assert_eq!(
        source.query("chr1", 1_000_000, 1_001_000).unwrap().len(),
        10
    );

    let next = source.adjacent_feature("chr1", 5_000, true).unwrap();
    assert_eq!((next.start, next.end), (200_000, 200_500));
    let previous = source.adjacent_feature("chr1", 200_000, false).unwrap();
    assert_eq!((previous.start, previous.end), (4_800, 5_000));
    assert!(source.adjacent_feature("chr1", 1_000, false).is_none());

    // Name search keeps the widest feature carrying each term: the gene, not an exon.
    let hits = source.search("alpha", 10);
    assert!(!hits.index_truncated);
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(
        (hits.hits[0].record.start, hits.hits[0].record.end),
        (1_000, 5_000)
    );
    // 111 terms share this prefix; results stay within the search-result cap.
    assert_eq!(source.search("bulk29", 1_000).hits.len(), 100);
    assert_eq!(source.search("bulk299", 1_000).hits.len(), 11);
    assert!(source.search("no-such-gene", 10).hits.is_empty());
}

#[test]
fn nested_features_do_not_break_overlap_queries() {
    let source = TextFeatureSource {
        malformed_lines: 0,
        first_malformed_line: None,
        features: HashMap::from([(
            "chr1".to_owned(),
            FeatureIndex {
                records: vec![
                    FeatureRecord {
                        contig: "chr1".to_owned(),
                        start: 0,
                        end: 1_000,
                        name: Some("outer".to_owned()),
                        feature_type: None,
                        score: None,
                        strand: Strand::Unstranded,
                        blocks: Vec::new(),
                        coding_interval: None,
                        peak: None,
                        attributes: Vec::new(),
                    },
                    FeatureRecord {
                        contig: "chr1".to_owned(),
                        start: 10,
                        end: 20,
                        name: Some("inner".to_owned()),
                        feature_type: None,
                        score: None,
                        strand: Strand::Unstranded,
                        blocks: Vec::new(),
                        coding_interval: None,
                        peak: None,
                        attributes: Vec::new(),
                    },
                ],
                prefix_max_end: vec![1_000, 1_000],
            },
        )]),
        search_terms: BTreeMap::default(),
        search_index_truncated: false,
        indexed: None,
    };
    let records = source.query("chr1", 500, 600).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].name.as_deref(), Some("outer"));
}

#[test]
fn interval_index_matches_brute_force_queries() {
    let mut state = 0xa076_1d64_78bd_642f_u64;
    let mut index = FeatureIndex::default();
    for record_number in 0..5_000_u64 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let start = state % 1_000_000;
        let length = 1 + state.rotate_left(29) % 50_000;
        index.records.push(FeatureRecord {
            contig: "chr1".to_owned(),
            start,
            end: start + length,
            name: Some(record_number.to_string()),
            feature_type: None,
            score: None,
            strand: Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        });
    }
    index.finish();
    for query_number in 0..1_000_u64 {
        let start = query_number * 997 % 1_000_000;
        let end = start + 10_000;
        let expected = index
            .records
            .iter()
            .filter(|record| record.start < end && record.end > start)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(index.query(start, end).unwrap(), expected);
    }
}

#[test]
fn variant_classification_and_breakend_parsing_are_typed() {
    assert_eq!(classify_variant(b"A", b"T"), VariantType::Snv);
    assert_eq!(classify_variant(b"AC", b"GT"), VariantType::Mnv);
    assert_eq!(classify_variant(b"A", b"AT"), VariantType::Insertion);
    assert_eq!(classify_variant(b"AT", b"A"), VariantType::Deletion);
    assert_eq!(classify_variant(b"N", b"<INV>"), VariantType::Inversion);
    assert_eq!(classify_variant(b"N", b"*"), VariantType::SpanningDeletion);

    let partner = parse_breakend_partner(2, b"A]chr2:501]").unwrap();
    assert_eq!(partner.alternate_index, 2);
    assert_eq!(partner.contig, "chr2");
    assert_eq!(partner.position, 500);
    assert!(partner.local_join_after_position);
    assert!(partner.partner_join_after_position);
    let leading = parse_breakend_partner(0, b"[chr3:7[G").unwrap();
    assert!(!leading.local_join_after_position);
    assert!(!leading.partner_join_after_position);

    for (alternate, local_after, partner_after) in [
        (b"[chr4:9[A".as_slice(), false, false),
        (b"]chr4:9]A".as_slice(), false, true),
        (b"A[chr4:9[".as_slice(), true, false),
        (b"A]chr4:9]".as_slice(), true, true),
    ] {
        let parsed = parse_breakend_partner(0, alternate).unwrap();
        assert_eq!(parsed.local_join_after_position, local_after);
        assert_eq!(parsed.partner_join_after_position, partner_after);
    }
}

#[test]
fn symbolic_sv_strands_are_parsed_as_bounded_breakpoint_orientations() {
    let fields = vec![VariantField {
        key: "STRANDS".to_owned(),
        value: VariantValue::Strings(vec!["+-:12,--:3,invalid".to_owned()]),
        truncated: false,
    }];
    assert_eq!(
        parse_breakpoint_orientations(&fields),
        vec![
            BreakpointOrientation {
                local_join_after_position: true,
                partner_join_after_position: false,
                support: Some(12),
            },
            BreakpointOrientation {
                local_join_after_position: false,
                partner_join_after_position: false,
                support: Some(3),
            },
        ]
    );
}

#[test]
fn large_cohort_subset_selection_is_bounded_and_preserves_header_indices() {
    let header = VariantHeader {
        samples: (0..25_000).map(|index| format!("sample-{index}")).collect(),
        ..VariantHeader::default()
    };
    let settings = VariantTrackSettings {
        sample_subset_enabled: true,
        sample_subset: (0..10_000)
            .map(|index| format!("sample-{}", index * 2))
            .collect(),
        ..VariantTrackSettings::default()
    };
    assert!(settings.is_valid());
    let selected = selected_sample_indices(&header, &settings);
    assert_eq!(selected.len(), 10_000);
    assert_eq!(selected[0], 0);
    assert_eq!(selected[9_999], 19_998);
    assert_eq!(
        selected_sample_indices(&header, &VariantTrackSettings::default()).len(),
        25_000
    );
}

#[test]
fn indexed_remote_bcf_opens_and_queries_through_http_ranges() {
    let fixture = RemoteVariantFixture::start();
    let mut source = LocalVariantSource::open(&fixture.url).unwrap();
    let query = source.query("chrVariants", 8_500, 16_000, 100).unwrap();

    assert_eq!(query.header.samples, ["tumour", "normal"]);
    assert_eq!(query.records.len(), 6);
    assert_eq!(query.records[0].id.as_deref(), Some("demo-snv"));
    assert!(
        query
            .records
            .iter()
            .any(|record| record.variant_types.contains(&VariantType::Breakend))
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn indexed_bcf_query_preserves_typed_headers_info_filters_and_genotypes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("typed.bcf");
    let mut header = Header::new();
    header.push_record(br"##contig=<ID=chr1,length=1000000>");
    header.push_record(br"##contig=<ID=chr2,length=1000000>");
    header.push_record(br#"##FILTER=<ID=q10,Description="Quality below ten">"#);
    header
        .push_record(br#"##INFO=<ID=END,Number=1,Type=Integer,Description="Structural endpoint">"#);
    header.push_record(br#"##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency">"#);
    header.push_record(br#"##INFO=<ID=SOMATIC,Number=0,Type=Flag,Description="Somatic event">"#);
    header.push_record(br#"##INFO=<ID=NOTE,Number=1,Type=String,Description="Review note">"#);
    header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
    header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">"#);
    header.push_sample(b"tumour");
    header.push_sample(b"normal");
    {
        let mut writer = Writer::from_path(&path, &header, false, Format::Bcf).unwrap();
        let mut record = writer.empty_record();
        record.set_rid(Some(0));
        record.set_pos(99);
        record.set_id(b"sv1").unwrap();
        record.set_alleles(&[b"N", b"<DEL>"]).unwrap();
        record.set_qual(9.5);
        record.set_filters(&[b"q10".as_slice()]).unwrap();
        record.push_info_integer(b"END", &[300]).unwrap();
        record.push_info_float(b"AF", &[0.25]).unwrap();
        record.push_info_flag(b"SOMATIC").unwrap();
        record.push_info_string(b"NOTE", &[b"review"]).unwrap();
        record
            .push_genotypes(&[
                GenotypeAllele::Unphased(0),
                GenotypeAllele::Unphased(1),
                GenotypeAllele::Unphased(1),
                GenotypeAllele::Phased(1),
            ])
            .unwrap();
        record.push_format_integer(b"DP", &[42, 31]).unwrap();
        writer.write(&record).unwrap();

        for (position, alleles) in [
            (399, vec![b"A".as_slice(), b"T".as_slice()]),
            (499, vec![b"AC".as_slice(), b"GT".as_slice()]),
            (599, vec![b"A".as_slice(), b"AT".as_slice()]),
            (699, vec![b"AT".as_slice(), b"A".as_slice()]),
            (799, vec![b"N".as_slice(), b"N]chr2:501]".as_slice()]),
            (
                899,
                vec![b"A".as_slice(), b"C".as_slice(), b"<INV>".as_slice()],
            ),
        ] {
            let mut record = writer.empty_record();
            record.set_rid(Some(0));
            record.set_pos(position);
            record.set_alleles(&alleles).unwrap();
            record
                .push_genotypes(&[
                    GenotypeAllele::UnphasedMissing,
                    GenotypeAllele::UnphasedMissing,
                ])
                .unwrap();
            writer.write(&record).unwrap();
        }
    }
    index::build(&path, None, 1, index::Type::Csi(14)).unwrap();

    let mut source = LocalVariantSource::open(&path).unwrap();
    let id_results = source.search_ids("SV", 10).unwrap();
    assert_eq!(id_results.hits.len(), 1);
    assert_eq!(id_results.hits[0].matched_id, "sv1");
    assert_eq!(id_results.hits[0].id, "sv1");
    assert_eq!(id_results.hits[0].contig, "chr1");
    assert_eq!(id_results.hits[0].start, 99);
    let query = source.query("chr1", 0, 1_000, 10).unwrap();
    assert_eq!(query.header.samples, vec!["tumour", "normal"]);
    assert_eq!(query.header.info[0].id, "END");
    assert_eq!(query.header.info[0].description, "Structural endpoint");
    assert_eq!(query.records.len(), 7);
    let record = &query.records[0];
    assert_eq!((record.start, record.end), (99, 300));
    assert_eq!(record.filters, vec!["q10"]);
    assert_eq!(record.variant_types, vec![VariantType::StructuralDeletion]);
    assert!(
        record
            .info
            .iter()
            .any(|field| { field.key == "SOMATIC" && field.value == VariantValue::Flag(true) })
    );
    assert_eq!(record.genotypes.len(), 2);
    assert_eq!(
        record.genotypes[0].alleles,
        vec![
            VariantGenotypeAllele {
                index: Some(0),
                phased: false,
            },
            VariantGenotypeAllele {
                index: Some(1),
                phased: false,
            },
        ]
    );
    assert!(record.genotypes[1].alleles[1].phased);
    assert_eq!(
        record.genotypes[0].fields[0].value,
        VariantValue::Integers(vec![Some(42)])
    );
    assert_eq!(query.provenance.genotype_cells_loaded, 14);
    assert_eq!(query.provenance.genotype_cells_omitted, 0);
    assert_eq!(query.records[1].variant_types, vec![VariantType::Snv]);
    assert_eq!(query.records[2].variant_types, vec![VariantType::Mnv]);
    assert_eq!(query.records[3].variant_types, vec![VariantType::Insertion]);
    assert_eq!(query.records[4].variant_types, vec![VariantType::Deletion]);
    assert_eq!(query.records[5].variant_types, vec![VariantType::Breakend]);
    assert_eq!(query.records[5].breakends[0].contig, "chr2");
    assert_eq!(query.records[5].breakends[0].position, 500);
    assert_eq!(
        query.records[6].variant_types,
        vec![VariantType::Snv, VariantType::Inversion]
    );

    let filtered = source
        .query_with_settings(
            "chr1",
            0,
            1_000,
            10,
            &VariantTrackSettings {
                minimum_quality: Some(9.0),
                allowed_filters: vec!["q10".to_owned()],
                variant_types: vec![VariantTypeGroup::StructuralDeletion],
                minimum_allele_frequency: Some(0.20),
                maximum_allele_frequency: Some(0.30),
                genotype: VariantGenotypeFilter::Heterozygous,
                field_predicates: vec![
                    VariantFieldPredicate {
                        scope: VariantFieldScope::Info,
                        key: "NOTE".to_owned(),
                        comparison: VariantFieldComparison::Contains("view".to_owned()),
                        missing: VariantMissingValueRule::Exclude,
                    },
                    VariantFieldPredicate {
                        scope: VariantFieldScope::Format,
                        key: "DP".to_owned(),
                        comparison: VariantFieldComparison::GreaterOrEqual(40.0),
                        missing: VariantMissingValueRule::Exclude,
                    },
                    VariantFieldPredicate {
                        scope: VariantFieldScope::Format,
                        key: "GT".to_owned(),
                        comparison: VariantFieldComparison::Equals("0/1".to_owned()),
                        missing: VariantMissingValueRule::Exclude,
                    },
                ],
                sample_subset_enabled: true,
                sample_subset: vec!["tumour".to_owned()],
                ..VariantTrackSettings::default()
            },
        )
        .unwrap();
    assert_eq!(filtered.records.len(), 1);
    assert_eq!(filtered.records[0].id.as_deref(), Some("sv1"));
    assert_eq!(filtered.records[0].genotypes.len(), 1);
    assert_eq!(filtered.records[0].genotypes[0].sample_index, 0);
    assert_eq!(filtered.provenance.records_examined, 7);
    assert_eq!(filtered.provenance.records_filtered, 6);
    assert_eq!(filtered.provenance.selected_samples, 1);
    assert_eq!(filtered.provenance.samples_omitted_by_subset, 1);
    assert_eq!(filtered.provenance.genotype_cells_loaded, 1);

    let missing_af_is_explicit = source
        .query_with_settings(
            "chr1",
            0,
            1_000,
            10,
            &VariantTrackSettings {
                variant_types: vec![VariantTypeGroup::Snv],
                minimum_allele_frequency: Some(0.20),
                allele_frequency_missing: VariantMissingValueRule::Include,
                ..VariantTrackSettings::default()
            },
        )
        .unwrap();
    assert_eq!(
        missing_af_is_explicit
            .records
            .iter()
            .map(|record| record.start)
            .collect::<Vec<_>>(),
        vec![399, 899]
    );

    let pass_deletion = source
        .query_with_settings(
            "chr1",
            0,
            1_000,
            10,
            &VariantTrackSettings {
                pass_only: true,
                variant_types: vec![VariantTypeGroup::Deletion],
                ..VariantTrackSettings::default()
            },
        )
        .unwrap();
    assert_eq!(
        pass_deletion
            .records
            .iter()
            .map(|record| record.start)
            .collect::<Vec<_>>(),
        vec![699]
    );
}

#[test]
fn quantitative_query_cap_and_non_finite_provenance_are_exact() {
    let mut query = QuantitativeQuery::default();
    assert!(retain_quantitative_record(
        &mut query,
        QuantitativeRecord {
            start: 0,
            end: 1,
            value: 2.0,
        },
        2,
    ));
    assert!(retain_quantitative_record(
        &mut query,
        QuantitativeRecord {
            start: 1,
            end: 2,
            value: f32::NAN,
        },
        2,
    ));
    assert!(!retain_quantitative_record(
        &mut query,
        QuantitativeRecord {
            start: 2,
            end: 3,
            value: 4.0,
        },
        2,
    ));
    assert_eq!(
        query.provenance,
        QuantitativeQueryProvenance {
            records_examined: 3,
            records_retained: 2,
            non_finite_records: 1,
            records_truncated: true,
            summary_reduction_bases: None,
        }
    );
}

#[test]
fn bigwig_zoom_summaries_expose_the_minimum_and_maximum_a_mean_hides() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("peak.bw");
    let mut chromosome_sizes = HashMap::new();
    chromosome_sizes.insert("chr1".to_owned(), 4_000_000);
    // A flat signal of 2 with one 100-base peak of 5,000 and one 100-base dip to 0.
    let mut bedgraph = String::new();
    for index in 0..40_000_u32 {
        let value = match index {
            12_345 => 5_000,
            30_001 => 0,
            _ => 2,
        };
        writeln!(
            bedgraph,
            "chr1\t{}\t{}\t{value}",
            index * 100,
            (index + 1) * 100
        )
        .unwrap();
    }
    let writer = bigtools::BigWigWrite::create_file(&path, chromosome_sizes).unwrap();
    let data = BedParserStreamingIterator::from_bedgraph_file(Cursor::new(bedgraph), false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    writer.write(data, runtime).unwrap();
    let mut source = BigWigSource::open(&path).unwrap();

    // Whole contig: a zoom level is read, and the records are exactly those of `query`.
    let means = source.query("chr1", 0, 4_000_000).unwrap();
    let wide = source.query_with_extremes("chr1", 0, 4_000_000).unwrap();
    let reduction = wide
        .provenance
        .summary_reduction_bases
        .expect("a whole-contig query reads a precomputed zoom level");
    assert!(reduction > 100);
    assert_eq!(wide.provenance, means.provenance);
    assert_eq!(
        wide.records
            .iter()
            .map(|record| (record.start, record.end, record.mean))
            .collect::<Vec<_>>(),
        means
            .records
            .iter()
            .map(|record| (record.start, record.end, record.value))
            .collect::<Vec<_>>()
    );
    assert!(
        wide.records
            .iter()
            .all(|record| record.minimum <= record.mean && record.mean <= record.maximum)
    );
    // The peak is diluted in the mean of its bin but intact in the maximum; likewise the dip.
    // The peak is diluted in the mean of the bin (or two) holding it but intact in the maximum;
    // likewise the dip in the minimum.
    let overlapping = |start: u64, end: u64| {
        wide.records
            .iter()
            .filter(|record| record.start < end && start < record.end)
            .collect::<Vec<_>>()
    };
    let peak = overlapping(1_234_500, 1_234_600);
    assert!(!peak.is_empty());
    for record in &peak {
        assert_eq!((record.minimum, record.maximum), (2.0, 5_000.0));
        assert!(record.mean < 2_500.0, "mean {} hides the peak", record.mean);
    }
    let dip = overlapping(3_000_100, 3_000_200);
    assert!(!dip.is_empty());
    for record in &dip {
        assert_eq!((record.minimum, record.maximum), (0.0, 2.0));
        assert!(record.mean > 0.0 && record.mean < 2.0);
    }
    let flat = wide
        .records
        .iter()
        .filter(|record| (record.maximum - record.minimum).abs() < f32::EPSILON)
        .count();
    assert_eq!(flat, wide.records.len() - peak.len() - dip.len());

    // Raw values carry no range.
    let narrow = source
        .query_with_extremes("chr1", 1_234_400, 1_234_700)
        .unwrap();
    assert_eq!(narrow.provenance.summary_reduction_bases, None);
    assert_eq!(
        narrow
            .records
            .iter()
            .map(|record| (record.start, record.minimum, record.mean, record.maximum))
            .collect::<Vec<_>>(),
        vec![
            (1_234_400, 2.0, 2.0, 2.0),
            (1_234_500, 5_000.0, 5_000.0, 5_000.0),
            (1_234_600, 2.0, 2.0, 2.0),
        ]
    );
}

#[test]
fn bigwig_wide_queries_use_zoom_summaries_and_narrow_queries_stay_exact() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dense.bw");
    let mut chromosome_sizes = HashMap::new();
    chromosome_sizes.insert("chr1".to_owned(), 4_000_000);
    // 40,000 adjacent 100-base intervals alternating between 1 and 3: the mean is exactly 2.
    let mut bedgraph = String::new();
    for index in 0..40_000_u32 {
        let value = if index % 2 == 0 { 1 } else { 3 };
        writeln!(
            bedgraph,
            "chr1\t{}\t{}\t{value}",
            index * 100,
            (index + 1) * 100
        )
        .unwrap();
    }
    let writer = bigtools::BigWigWrite::create_file(&path, chromosome_sizes).unwrap();
    let data = BedParserStreamingIterator::from_bedgraph_file(Cursor::new(bedgraph), false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    writer.write(data, runtime).unwrap();

    let mut source = BigWigSource::open(&path).unwrap();

    let narrow = source.query("chr1", 1_000, 1_400).unwrap();
    assert_eq!(narrow.provenance.summary_reduction_bases, None);
    assert_eq!(
        narrow
            .records
            .iter()
            .map(|record| (record.start, record.end, record.value))
            .collect::<Vec<_>>(),
        vec![
            (1_000, 1_100, 1.0),
            (1_100, 1_200, 3.0),
            (1_200, 1_300, 1.0),
            (1_300, 1_400, 3.0),
        ]
    );

    let wide = source.query("chr1", 0, 4_000_000).unwrap();
    let reduction = wide
        .provenance
        .summary_reduction_bases
        .expect("a whole-contig query reads a precomputed zoom level");
    assert!(
        reduction > 100,
        "reduction {reduction} should aggregate raw intervals"
    );
    assert!(!wide.provenance.records_truncated);
    assert!(wide.records.len() < 40_000);
    assert!(wide.records.len() >= 1_000);
    assert!(wide.records.last().unwrap().end > 3_900_000);
    let (weighted, bases) = wide.records.iter().fold((0.0_f64, 0_u64), |state, record| {
        let length = record.end - record.start;
        (
            state.0 + f64::from(record.value) * length as f64,
            state.1 + length,
        )
    });
    assert!((weighted / bases as f64 - 2.0).abs() < 0.01);
    assert!(
        wide.records
            .iter()
            .all(|record| (1.0..=3.0).contains(&record.value))
    );
}

#[test]
fn gwas_header_aliases_coordinates_values_and_missing_rows_are_exact() {
    let source = TextGwasSource::from_reader(Cursor::new(
        b"# generated by the association pipeline\n\
              #CHROM POS ID BETA P EXTRA\n\
              chr1 101 rs-one 0.25 1e-8 retained\n\
              chr1 151 rs-missing -0.5 NA ignored\n\
              chr2 201 rs-zero 1.0 0 underflow\n",
    ))
    .unwrap();
    let query = source.query("chr1", 100, 200);
    assert_eq!(query.records.len(), 1);
    let record = &query.records[0];
    assert_eq!(record.contig, "chr1");
    assert_eq!(record.position, 100);
    assert_eq!(record.identifier, "rs-one");
    assert!((record.p_value - 1e-8).abs() < f64::EPSILON);
    assert!((record.minus_log10_p - 8.0).abs() < 1e-5);
    assert_eq!(record.fields[4], ("P".to_owned(), "1e-8".to_owned()));
    assert_eq!(query.provenance.source_rows_ignored, 1);
    assert!(!query.provenance.records_truncated);
    let zero = source.query("chr2", 0, 1_000);
    assert_eq!(zero.records[0].position, 200);
    assert!(zero.records[0].minus_log10_p.is_infinite());
}

#[test]
fn gwas_schema_and_malformed_rows_fail_explicitly() {
    let missing = TextGwasSource::from_reader(Cursor::new(b"CHR BP P\nchr1 1 0.5\n"));
    assert!(matches!(
        missing,
        Err(TrackError::MalformedLine { line: 1, .. })
    ));

    for contents in [
        "CHR BP SNP P\nchr1 0 rs1 0.5\n",
        "CHR BP SNP P\nchr1 1 rs1 1.5\n",
        "CHR BP SNP P EXTRA\nchr1 1 rs1 0.5\n",
    ] {
        assert!(matches!(
            TextGwasSource::from_reader(Cursor::new(contents.as_bytes())),
            Err(TrackError::MalformedLine { line: 2, .. })
        ));
    }
    assert!(matches!(
        TextGwasSource::from_reader(Cursor::new(b"## comment only\n")),
        Err(TrackError::InvalidSchema(_))
    ));
}

#[test]
fn gwas_query_cap_keeps_the_strongest_point_in_each_positional_chunk() {
    let records = [(0, 1.0), (1, 3.0), (2, 2.0), (3, 4.0), (4, 5.0)]
        .into_iter()
        .map(|(position, score)| GwasRecord {
            contig: "chr1".to_owned(),
            position,
            identifier: format!("rs{position}"),
            p_value: 10_f64.powf(-f64::from(score)),
            minus_log10_p: score,
            fields: Vec::new(),
        })
        .collect::<Vec<_>>();
    let retained = bounded_gwas_records(&records, 2);
    assert_eq!(
        retained
            .iter()
            .map(|record| record.position)
            .collect::<Vec<_>>(),
        vec![1, 4]
    );
}

#[test]
fn gzip_gwas_opens_through_the_production_path() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("associations.gwas.gz");
    let file = File::create(&path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder
        .write_all(b"chromosome position markername p.value note\nchr7 42 rs7 0.001 exact\n")
        .unwrap();
    encoder.finish().unwrap();
    let source = TextGwasSource::open(path).unwrap();
    let query = source.query("chr7", 0, 100);
    assert_eq!(query.records.len(), 1);
    assert_eq!(query.records[0].identifier, "rs7");
    assert!((query.records[0].minus_log10_p - 3.0).abs() < 1e-5);
}

#[test]
fn remote_text_annotations_are_downloaded_and_read_like_local_files() {
    let data = b"chr1\t100\t200\tfirst\nchr1\t300\t450\tsecond\nchr2\t10\t20\tthird\n".to_vec();
    let server = Server::http("127.0.0.1:0").unwrap();
    let address = server.server_addr().to_ip().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let body = data.clone();
    let worker = thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                continue;
            };
            let range = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Range"))
                .and_then(|header| {
                    let value = header.value.as_str().strip_prefix("bytes=")?;
                    let (start, end) = value.split_once('-')?;
                    let start = start.parse::<usize>().ok()?;
                    let end = end
                        .parse::<usize>()
                        .map_or(body.len() - 1, |end| end.min(body.len() - 1));
                    (start <= end).then_some((start, end))
                })
                .unwrap_or((0, body.len() - 1));
            let response = Response::from_data(body[range.0..=range.1].to_vec())
                .with_status_code(StatusCode(206))
                .with_header(
                    HttpHeader::from_bytes(
                        "Content-Range",
                        format!("bytes {}-{}/{}", range.0, range.1, body.len()),
                    )
                    .unwrap(),
                )
                .with_header(HttpHeader::from_bytes("ETag", "\"remote-bed\"").unwrap());
            request.respond(response).unwrap();
        }
    });

    let url = format!("http://{address}/peaks/regions.bed?token=ignored");
    let source = TextFeatureSource::open(&url, FeatureFormat::Bed);
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    let source = source.unwrap();
    let mut contigs = source.contigs().collect::<Vec<_>>();
    contigs.sort_unstable();
    assert_eq!(contigs, ["chr1", "chr2"]);
    let features = source.query("chr1", 150, 320).unwrap();
    assert_eq!(features.len(), 2);
}

#[test]
fn bed_accepts_space_separated_columns_and_empty_thick_intervals() {
    let spaced = parse_bed("chr1 100   200 tx 42 -").unwrap();
    assert_eq!((spaced.start, spaced.end), (100, 200));
    assert_eq!(spaced.name.as_deref(), Some("tx"));
    assert_eq!(spaced.strand, Strand::Reverse);
    // Tabs win when present, so a name may contain spaces.
    let tabbed = parse_bed("chr1\t100\t200\tmy feature").unwrap();
    assert_eq!(tabbed.name.as_deref(), Some("my feature"));

    // thickStart == thickEnd means "no thick part" wherever it is written.
    for thick in ["0\t0", "200\t200", "100\t100", "150\t150"] {
        let record = parse_bed(&format!("chr1\t100\t200\ttx\t0\t+\t{thick}")).unwrap();
        assert_eq!(record.coding_interval, None, "thick {thick}");
    }
    let record = parse_bed("chr1\t100\t200\ttx\t0\t+\t120\t180").unwrap();
    assert_eq!(record.coding_interval, Some((120, 180)));
    // A non-empty thick interval outside the feature is still wrong.
    assert!(parse_bed("chr1\t100\t200\ttx\t0\t+\t50\t180").is_err());
}

#[test]
fn gff3_attributes_are_percent_decoded_after_splitting() {
    let record = parse_gff(
        "chr1\tsrc\tgene\t11\t50\t.\t+\t.\tID=g%3B1;Name=A%2CB%3DC%20d;Note=50%25 done%;bad=%zz",
        false,
    )
    .unwrap();
    let value = |key: &str| attribute_value(&record.attributes, &[key]);
    // The escaped `;`, `,`, `=` and space are data, not separators.
    assert_eq!(value("ID").as_deref(), Some("g;1"));
    assert_eq!(value("Name").as_deref(), Some("A,B=C d"));
    assert_eq!(record.name.as_deref(), Some("A,B=C d"));
    // Truncated and non-hex escapes are kept verbatim.
    assert_eq!(value("Note").as_deref(), Some("50% done%"));
    assert_eq!(value("bad").as_deref(), Some("%zz"));
    assert_eq!(record.attributes.len(), 4);

    // GTF has no percent-escaping.
    let gtf = parse_gff("chr1\tsrc\tgene\t11\t50\t.\t+\t.\tgene_id \"g%3B1\";", true).unwrap();
    assert_eq!(
        attribute_value(&gtf.attributes, &["gene_id"]).as_deref(),
        Some("g%3B1")
    );
}

#[test]
fn gff3_reading_stops_at_the_embedded_fasta_section() {
    let source = TextFeatureSource::from_reader(
        Cursor::new(
            "##gff-version 3\n\
             chr1\tsrc\tgene\t11\t50\t.\t+\t.\tID=g1\n\
             ##FASTA\n\
             >chr1\n\
             ACGTACGTACGT\n\
             chr1\tnot\ta\tfeature\n",
        ),
        FeatureFormat::Gff3,
    )
    .unwrap();
    assert_eq!(source.query("chr1", 0, 1_000).unwrap().len(), 1);
    assert_eq!(source.malformed_line_count(), 0);

    // The directive is optional: a bare FASTA header also ends the annotation.
    let bare = TextFeatureSource::from_reader(
        Cursor::new("chr1\tsrc\tgene\t11\t50\t.\t+\t.\tID=g1\n>chr1\nACGT\n"),
        FeatureFormat::Gff3,
    )
    .unwrap();
    assert_eq!(bare.query("chr1", 0, 1_000).unwrap().len(), 1);
    assert_eq!(bare.malformed_line_count(), 0);
}

#[test]
fn one_malformed_feature_line_is_skipped_and_counted() {
    let source = TextFeatureSource::from_reader(
        Cursor::new(
            "chr1\t10\t20\tfirst\n\
             chr1\tnot-a-number\t30\tbroken\n\
             chr1\t40\t50\tsecond\n\
             chr1\t70\t60\tbackwards\n",
        ),
        FeatureFormat::Bed,
    )
    .unwrap();
    let names = source
        .query("chr1", 0, 1_000)
        .unwrap()
        .into_iter()
        .map(|record| record.name.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(source.malformed_line_count(), 2);
    let (line, message) = source.first_malformed_line().unwrap();
    assert_eq!(line, 2);
    assert!(message.contains("BED start"));

    // A file with no parseable line at all is the wrong format and still fails loudly.
    assert!(matches!(
        TextFeatureSource::from_reader(
            Cursor::new("this is not\na feature file\n"),
            FeatureFormat::Gff3,
        ),
        Err(TrackError::MalformedLine { line: 1, .. })
    ));
}

/// Serves byte ranges of `data` only to requests carrying `Authorization: Bearer track-secret`.
struct AuthenticatedRangeServer {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl AuthenticatedRangeServer {
    fn start(data: Vec<u8>, name: &str) -> Self {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                    continue;
                };
                if !request.headers().iter().any(|header| {
                    header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer track-secret"
                }) {
                    request.respond(Response::empty(StatusCode(401))).unwrap();
                    continue;
                }
                let (start, end) = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| {
                        let (start, end) = header
                            .value
                            .as_str()
                            .strip_prefix("bytes=")?
                            .split_once('-')?;
                        let end = end.parse::<usize>().ok()?.min(data.len() - 1);
                        Some((start.parse::<usize>().ok()?, end))
                    })
                    .unwrap();
                let response = Response::from_data(data[start..=end].to_vec())
                    .with_status_code(StatusCode(206))
                    .with_header(
                        HttpHeader::from_bytes(
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", data.len()),
                        )
                        .unwrap(),
                    )
                    .with_header(HttpHeader::from_bytes("ETag", "\"authenticated\"").unwrap());
                request.respond(response).unwrap();
            }
        });
        Self {
            url: format!("http://{address}/{name}"),
            stop,
            worker: Some(worker),
        }
    }

    fn config() -> HttpRangeConfig {
        HttpRangeConfig {
            authentication: crate::RemoteAuthentication::Bearer("track-secret".to_owned()),
            // One small block is cached at a time, so distinct regions need distinct requests.
            chunk_size: std::num::NonZeroUsize::new(4_096).unwrap(),
            maximum_cache_bytes: std::num::NonZeroUsize::new(4_096).unwrap(),
            maximum_retries: 0,
            ..HttpRangeConfig::default()
        }
    }
}

impl Drop for AuthenticatedRangeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn remote_bigwig_honours_authentication_cancellation_and_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("remote.bw");
    let mut chromosome_sizes = HashMap::new();
    chromosome_sizes.insert("chr1".to_owned(), 4_000_000);
    let mut bedgraph = String::new();
    for index in 0..20_000_u32 {
        writeln!(
            bedgraph,
            "chr1\t{}\t{}\t{}",
            index * 100,
            (index + 1) * 100,
            index % 7
        )
        .unwrap();
    }
    let writer = bigtools::BigWigWrite::create_file(&path, chromosome_sizes).unwrap();
    let data = BedParserStreamingIterator::from_bedgraph_file(Cursor::new(bedgraph), false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    writer.write(data, runtime).unwrap();
    let mut local = BigWigSource::open(&path).unwrap();
    assert_eq!(local.remote_io_stats(), None);
    let expected = local.query("chr1", 1_000, 1_400).unwrap().records;
    assert_eq!(expected.len(), 4);

    let server = AuthenticatedRangeServer::start(std::fs::read(&path).unwrap(), "remote.bw");
    // The anonymous default is refused by the server.
    assert!(BigWigSource::open(&server.url).is_err());
    let mut source =
        BigWigSource::open_with_remote_config(&server.url, AuthenticatedRangeServer::config())
            .unwrap();
    assert_eq!(
        source.query("chr1", 1_000, 1_400).unwrap().records,
        expected
    );
    let opened = source.remote_io_stats().unwrap();
    assert!(opened.requests >= 2);

    let cancellation: crate::RemoteCancellation = Arc::new(|| false);
    source.set_remote_cancellation(Some(cancellation));
    assert!(source.query("chr1", 1_900_000, 1_900_400).is_err());
    assert_eq!(source.remote_io_stats().unwrap().cancellations, 1);
    assert_eq!(source.remote_io_stats().unwrap().requests, opened.requests);

    source.set_remote_cancellation(None);
    source.set_remote_transfer_budget(Some(RemoteTransferBudget {
        maximum_requests: 0,
        maximum_response_bytes: 0,
    }));
    assert!(source.query("chr1", 1_900_000, 1_900_400).is_err());
    assert_eq!(source.remote_io_stats().unwrap().requests, opened.requests);

    source.set_remote_transfer_budget(None);
    assert_eq!(
        source.query("chr1", 1_900_000, 1_900_400).unwrap().records,
        local.query("chr1", 1_900_000, 1_900_400).unwrap().records
    );
}

#[test]
fn remote_bigbed_and_text_annotations_use_the_supplied_remote_config() {
    let directory = tempfile::tempdir().unwrap();
    let bed = "chr1\t100\t200\tfirst\nchr1\t300\t450\tsecond\n";
    let path = directory.path().join("remote.bb");
    let mut chromosome_sizes = HashMap::new();
    chromosome_sizes.insert("chr1".to_owned(), 10_000);
    let writer = BigBedWrite::create_file(&path, chromosome_sizes).unwrap();
    let data = BedParserStreamingIterator::from_bed_file(Cursor::new(bed.to_owned()), false);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    writer.write(data, runtime).unwrap();

    let server = AuthenticatedRangeServer::start(std::fs::read(&path).unwrap(), "remote.bb");
    assert!(BigBedSource::open(&server.url).is_err());
    let mut source = BigBedSource::open_with_format_and_remote_config(
        &server.url,
        BigBedFeatureFormat::Bed,
        AuthenticatedRangeServer::config(),
    )
    .unwrap();
    let names = |records: Vec<FeatureRecord>| {
        records
            .into_iter()
            .map(|record| record.name.unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(source.query("chr1", 0, 1_000).unwrap()),
        ["first", "second"]
    );
    assert!(source.remote_io_stats().unwrap().requests >= 2);
    let cancellation: crate::RemoteCancellation = Arc::new(|| false);
    source.set_remote_cancellation(Some(cancellation));
    assert!(source.query("chr1", 0, 1_000).is_err());

    let text = AuthenticatedRangeServer::start(bed.as_bytes().to_vec(), "authenticated-hts10.bed");
    assert!(TextFeatureSource::open(&text.url, FeatureFormat::Bed).is_err());
    let source = TextFeatureSource::open_with_remote_config(
        &text.url,
        FeatureFormat::Bed,
        &AuthenticatedRangeServer::config(),
    )
    .unwrap();
    assert_eq!(
        names(source.query("chr1", 0, 1_000).unwrap()),
        ["first", "second"]
    );
}
