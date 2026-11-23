//! Name search indexes names once per gene and per transcript, never silently truncates, and
//! matches Ensembl/RefSeq identifiers with or without their version.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::tracks::feature_search::{
    SearchLevel, StreamingFeatureSearch, feature_search_terms, natural_contig_order, search_level,
    unversioned_identifier,
};

/// One GENCODE-style gene: a gene row, one transcript and its exon, CDS, UTR and codon rows.
fn gencode_gene(contig: &str, start: u64, name: &str, gene: &str, exons: u64) -> Vec<String> {
    let gene_id = format!("ENSG{gene}.18");
    let transcript_id = format!("ENST{gene}.7");
    let end = start + exons * 1_000 - 1;
    let attributes = |extra: &str| {
        format!("gene_id \"{gene_id}\"; gene_type \"protein_coding\"; gene_name \"{name}\";{extra}")
    };
    let transcript = attributes(&format!(
        " transcript_id \"{transcript_id}\"; transcript_name \"{name}-201\";"
    ));
    let mut lines = vec![
        format!(
            "{contig}\tHAVANA\tgene\t{start}\t{end}\t.\t+\t.\t{}",
            attributes("")
        ),
        format!("{contig}\tHAVANA\ttranscript\t{start}\t{end}\t.\t+\t.\t{transcript}"),
    ];
    for exon in 0..exons {
        let exon_start = start + exon * 1_000;
        let exon_end = exon_start + 499;
        let exon_attributes = format!(
            "{transcript} exon_number {}; exon_id \"ENSE{gene}{exon:03}.1\";",
            exon + 1
        );
        for feature in ["exon", "CDS"] {
            lines.push(format!(
                "{contig}\tHAVANA\t{feature}\t{exon_start}\t{exon_end}\t.\t+\t0\t{exon_attributes}"
            ));
        }
    }
    for (feature, at) in [
        ("UTR", start),
        ("start_codon", start),
        ("stop_codon", end - 2),
    ] {
        lines.push(format!(
            "{contig}\tHAVANA\t{feature}\t{at}\t{}\t.\t+\t0\t{transcript}",
            at + 2
        ));
    }
    lines
}

/// Genes on chr1, chr10, chr2 and chr3, written in that (lexical) order.
fn synthetic_gtf() -> String {
    [
        gencode_gene("chr1", 1_001, "ALPHA1", "00000000001", 4),
        gencode_gene("chr10", 5_001, "BETA10", "00000000010", 3),
        gencode_gene("chr2", 20_001, "GAMMA2", "00000000002", 5),
        gencode_gene("chr3", 52_001, "PBRM1", "00000163939", 6),
    ]
    .concat()
    .join("\n")
}

fn load(text: &str, format: FeatureFormat, cap: usize) -> TextFeatureSource {
    TextFeatureSource::from_reader_with_search_cap(
        std::io::Cursor::new(text.to_owned()),
        format,
        cap,
    )
    .unwrap()
}

fn exact_hits<'a>(results: &'a FeatureSearchResults, query: &str) -> Vec<&'a FeatureSearchHit> {
    let query = query.to_lowercase();
    results
        .hits
        .iter()
        .filter(|hit| hit.matched_term == query)
        .collect()
}

#[test]
fn every_gene_is_found_by_name_and_by_versioned_and_unversioned_ids() {
    let source = load(
        &synthetic_gtf(),
        FeatureFormat::Gtf,
        MAX_FEATURE_SEARCH_ASSOCIATIONS,
    );
    for (contig, name, gene, span) in [
        ("chr1", "ALPHA1", "00000000001", (1_000, 5_000)),
        ("chr10", "BETA10", "00000000010", (5_000, 8_000)),
        ("chr2", "GAMMA2", "00000000002", (20_000, 25_000)),
        ("chr3", "PBRM1", "00000163939", (52_000, 58_000)),
    ] {
        for query in [
            name.to_owned(),
            format!("ENSG{gene}.18"),
            format!("ENSG{gene}"),
        ] {
            let results = source.search(&query, 10);
            let exact = exact_hits(&results, &query);
            // Exactly one exact match, so the locus box navigates without asking.
            assert_eq!(exact.len(), 1, "{query}: {:?}", results.hits);
            let record = &exact[0].record;
            assert_eq!(record.contig, contig, "{query}");
            assert_eq!(record.feature_type.as_deref(), Some("gene"), "{query}");
            // The whole gene, not one exon.
            assert_eq!((record.start, record.end), span, "{query}");
        }
        for query in [format!("ENST{gene}.7"), format!("ENST{gene}")] {
            let results = source.search(&query, 10);
            let exact = exact_hits(&results, &query);
            assert_eq!(exact.len(), 1, "{query}: {:?}", results.hits);
            assert_eq!(exact[0].record.feature_type.as_deref(), Some("transcript"));
        }
        let transcript_name = format!("{name}-201");
        assert_eq!(
            exact_hits(&source.search(&transcript_name, 10), &transcript_name).len(),
            1
        );
    }
    let coverage = source.search_index_coverage().unwrap();
    assert!(!coverage.truncated);
    // Per gene: name, versioned and unversioned gene ID; per transcript: its versioned and
    // unversioned ID and its name. Nothing from the 31 exon, CDS, UTR and codon rows.
    assert_eq!(coverage.associations, 4 * 6);
}

#[test]
fn sub_feature_rows_no_longer_consume_the_association_cap() {
    let text = (0..40)
        .flat_map(|gene| {
            let contig = format!("chr{}", gene % 4 + 1);
            gencode_gene(
                &contig,
                1_001 + gene * 100_000,
                &format!("GENE{gene}"),
                &format!("{gene:011}"),
                20,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cap = 300;
    // Before, every row added each of its names: far beyond the cap.
    let all = load(&text, FeatureFormat::Gtf, usize::MAX);
    let per_row_terms = all
        .features
        .values()
        .flat_map(|index| &index.records)
        .map(|record| feature_search_terms(record).len())
        .sum::<usize>();
    assert!(per_row_terms > 10 * cap, "{per_row_terms}");
    // Now 40 genes cost six associations each, well inside it.
    let source = load(&text, FeatureFormat::Gtf, cap);
    let coverage = source.search_index_coverage().unwrap();
    assert_eq!(
        coverage,
        FeatureSearchIndexCoverage {
            associations: 240,
            truncated: false,
            complete_through: None,
        }
    );
    assert_eq!(exact_hits(&source.search("gene39", 10), "gene39").len(), 1);
}

#[test]
fn a_truncated_index_reports_the_last_complete_contig_in_chromosome_order() {
    // chr1, chr2 and chr3 fit (six associations each); chr10 comes after them, not after chr1.
    let source = load(&synthetic_gtf(), FeatureFormat::Gtf, 20);
    let coverage = source.search_index_coverage().unwrap();
    assert!(coverage.truncated);
    assert_eq!(coverage.associations, 20);
    assert_eq!(coverage.complete_through.as_deref(), Some("chr3"));
    let results = source.search("pbrm1", 10);
    assert!(results.index_truncated);
    assert_eq!(exact_hits(&results, "pbrm1").len(), 1);
    // Even an empty query reports that the index is partial.
    assert!(source.search("", 10).index_truncated);

    // Stopping inside the first contig names none.
    let first = load(&synthetic_gtf(), FeatureFormat::Gtf, 2)
        .search_index_coverage()
        .unwrap();
    assert!(first.truncated);
    assert_eq!(first.complete_through, None);
}

#[test]
fn files_of_only_exon_rows_index_each_name_once_on_the_gene_span() {
    let exon = |start: u64, gene: &str, name: &str, transcript: &str| {
        format!(
            "chr5\tsrc\texon\t{start}\t{}\t.\t-\t.\tgene_id \"{gene}\"; gene_name \"{name}\"; \
             transcript_id \"{transcript}\";",
            start + 99
        )
    };
    let text = [
        exon(1_001, "G1", "TP53", "T1.2"),
        exon(3_001, "G1", "TP53", "T1.2"),
        exon(2_001, "G1", "TP53", "T2"),
        exon(9_001, "G1", "TP53", "T2"),
        // Another gene sharing the symbol far away keeps its own span.
        exon(900_001, "G9", "TP53", "T9"),
    ]
    .join("\n");
    let source = load(&text, FeatureFormat::Gtf, MAX_FEATURE_SEARCH_ASSOCIATIONS);
    let symbol = source.search("tp53", 10);
    let spans = exact_hits(&symbol, "tp53")
        .iter()
        .map(|hit| (hit.record.start, hit.record.end))
        .collect::<Vec<_>>();
    assert_eq!(spans, [(1_000, 9_100), (900_000, 900_100)]);
    assert!(
        symbol
            .hits
            .iter()
            .all(|hit| hit.record.feature_type.as_deref() == Some("gene"))
    );
    let gene = source.search("g1", 10);
    assert_eq!(exact_hits(&gene, "g1").len(), 1);
    assert_eq!(
        (gene.hits[0].record.start, gene.hits[0].record.end),
        (1_000, 9_100)
    );
    let transcript = source.search("t1", 10);
    let transcript = exact_hits(&transcript, "t1");
    assert_eq!(transcript.len(), 1);
    assert_eq!(
        transcript[0].record.feature_type.as_deref(),
        Some("transcript")
    );
    assert_eq!(
        (transcript[0].record.start, transcript[0].record.end),
        (1_000, 3_100)
    );
    // tp53 twice (two genes), g1, g9, t1.2, t1, t2, t9.
    assert_eq!(source.search_index_coverage().unwrap().associations, 8);
}

#[test]
fn gff3_sub_feature_identifiers_are_not_indexed() {
    let text = "chr1\tsrc\tgene\t101\t900\t.\t+\t.\tID=gene:G1;Name=FOO;gene_id=G1\n\
                chr1\tsrc\tmRNA\t101\t900\t.\t+\t.\tID=transcript:T1;Parent=gene:G1;Name=FOO-201;transcript_id=T1\n\
                chr1\tsrc\texon\t101\t200\t.\t+\t.\tParent=transcript:T1;Name=ENSE0001;exon_id=ENSE0001\n\
                chr1\tsrc\texon\t801\t900\t.\t+\t.\tID=exon:T1:2;Parent=transcript:T1;Name=ENSE0002\n\
                chr1\tsrc\tCDS\t151\t850\t.\t+\t0\tID=CDS:T1;Parent=transcript:T1\n\
                chr2\tsrc\texon\t11\t20\t.\t+\t.\tName=LONE\n\
                chr2\tsrc\texon\t31\t40\t.\t+\t.\tName=LONE\n";
    let source = load(text, FeatureFormat::Gff3, MAX_FEATURE_SEARCH_ASSOCIATIONS);
    assert!(source.search("ense", 10).hits.is_empty());
    assert!(source.search("exon:", 10).hits.is_empty());
    assert!(source.search("cds:", 10).hits.is_empty());
    let foo = source.search("foo", 10);
    assert_eq!(exact_hits(&foo, "foo").len(), 1);
    assert_eq!(foo.hits[0].record.feature_type.as_deref(), Some("gene"));
    // An exon without a parent or identity is found by its own name, once.
    let lone = source.search("lone", 10);
    assert_eq!(lone.hits.len(), 1);
    assert_eq!(
        (lone.hits[0].record.start, lone.hits[0].record.end),
        (10, 40)
    );
    // gene: gene:g1, foo, g1; mRNA: transcript:t1, foo-201, t1; LONE once.
    assert_eq!(source.search_index_coverage().unwrap().associations, 7);
}

#[test]
fn the_streamed_index_follows_the_same_rules_and_reports_truncation() {
    let records = synthetic_gtf()
        .lines()
        .map(|line| crate::tracks::parse_gff(line, true).unwrap())
        .collect::<Vec<_>>();
    let mut streamed = StreamingFeatureSearch::default();
    for record in &records {
        assert!(streamed.add(record, MAX_FEATURE_SEARCH_ASSOCIATIONS));
    }
    assert!(!streamed.coverage().truncated);
    assert_eq!(streamed.coverage().associations, 4 * 6);
    let loaded = load(
        &synthetic_gtf(),
        FeatureFormat::Gtf,
        MAX_FEATURE_SEARCH_ASSOCIATIONS,
    );
    for query in [
        "pbrm1",
        "ensg00000163939",
        "ensg00000163939.18",
        "enst00000163939",
        "beta10-201",
        "ens",
        "g",
    ] {
        let located = |results: FeatureSearchResults| {
            results
                .hits
                .into_iter()
                .map(|hit| {
                    (
                        hit.matched_term,
                        hit.record.contig,
                        hit.record.start,
                        hit.record.end,
                        hit.record.feature_type,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            located(streamed.search(query, 20)),
            located(loaded.search(query, 20)),
            "{query}"
        );
    }

    // File order here is chr1, chr10, chr2, chr3: the cap falls inside chr2.
    let mut bounded = StreamingFeatureSearch::default();
    let mut stopped = false;
    for record in &records {
        if !bounded.add(record, 14) {
            stopped = true;
            break;
        }
    }
    assert!(stopped);
    assert!(bounded.coverage().truncated);
    assert_eq!(
        bounded.coverage().complete_through.as_deref(),
        Some("chr10")
    );
    assert!(bounded.search("alpha1", 10).index_truncated);
}

#[test]
fn rows_are_ranked_gene_then_transcript_then_sub_feature() {
    let record = |feature_type: Option<&str>| FeatureRecord {
        contig: "chr1".to_owned(),
        start: 0,
        end: 1,
        name: None,
        feature_type: feature_type.map(str::to_owned),
        score: None,
        strand: Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    for (feature_type, level) in [
        (Some("gene"), SearchLevel::Gene),
        (Some("ncRNA_gene"), SearchLevel::Gene),
        (Some("pseudogene"), SearchLevel::Gene),
        (Some("transcript"), SearchLevel::Other),
        (Some("mRNA"), SearchLevel::Other),
        (None, SearchLevel::Other),
        (Some("exon"), SearchLevel::SubFeature),
        (Some("CDS"), SearchLevel::SubFeature),
        (Some("UTR"), SearchLevel::SubFeature),
        (Some("five_prime_UTR"), SearchLevel::SubFeature),
        (Some("start_codon"), SearchLevel::SubFeature),
        (Some("stop_codon"), SearchLevel::SubFeature),
        (Some("Selenocysteine"), SearchLevel::SubFeature),
    ] {
        assert_eq!(
            search_level(&record(feature_type)),
            level,
            "{feature_type:?}"
        );
    }
}

#[test]
fn only_identifier_shaped_terms_lose_their_version() {
    for (term, unversioned) in [
        ("ensg00000163939.18", Some("ensg00000163939")),
        ("enst00000296302.11", Some("enst00000296302")),
        ("nm_000546.6", Some("nm_000546")),
        ("ensg00000163939", None),
        ("pbrm1", None),
        ("pbrm1-201", None),
        ("1.5", None),
        ("abc.1", None),
        ("ensg0001.x", None),
        ("ensg00000182378.13_par_y", None),
    ] {
        assert_eq!(unversioned_identifier(term), unversioned, "{term}");
    }
}

#[test]
fn contigs_are_indexed_in_chromosome_order() {
    let mut contigs = [
        "chr10",
        "chrX",
        "chr2",
        "chrM",
        "chr1",
        "chrUn_gl000220",
        "chr22",
        "chrY",
        "chr3",
    ];
    contigs.sort_by(|left, right| natural_contig_order(left, right));
    assert_eq!(
        contigs,
        [
            "chr1",
            "chr2",
            "chr3",
            "chr10",
            "chr22",
            "chrX",
            "chrY",
            "chrM",
            "chrUn_gl000220"
        ]
    );
}
