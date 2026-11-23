//! GTF and GFF3 lines are drawn as assembled transcript models, like genePred records.

#[allow(clippy::wildcard_imports)]
use super::super::*;

fn load(text: &str, format: FeatureFormat) -> TextFeatureSource {
    TextFeatureSource::from_reader(text.as_bytes(), format).unwrap()
}

fn all(source: &TextFeatureSource) -> Vec<FeatureRecord> {
    let mut contigs = source.contigs().map(str::to_owned).collect::<Vec<_>>();
    contigs.sort_unstable();
    contigs
        .iter()
        .flat_map(|contig| source.query(contig, 0, u64::from(u32::MAX)).unwrap())
        .collect()
}

fn named<'a>(records: &'a [FeatureRecord], key: &str, value: &str) -> &'a FeatureRecord {
    records
        .iter()
        .find(|record| {
            record
                .attributes
                .iter()
                .any(|(name, found)| name == key && found == value)
        })
        .unwrap_or_else(|| panic!("no record with {key}={value}: {records:#?}"))
}

fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// A GENCODE-style minus-strand gene with a coding and a non-coding transcript. GTF keeps the
/// stop codon outside the CDS; on the minus strand it lies below the CDS.
const GTF: &str = "\
chr1\tHAVANA\tgene\t1001\t9000\t.\t-\t.\tgene_id \"G1.5\"; gene_type \"protein_coding\"; gene_name \"GENE1\";\n\
chr1\tHAVANA\ttranscript\t1001\t9000\t7\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_type \"protein_coding\"; gene_name \"GENE1\"; transcript_type \"protein_coding\"; transcript_name \"GENE1-201\"; tag \"basic\"; tag \"CCDS\";\n\
chr1\tHAVANA\texon\t8001\t9000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 1; exon_id \"E1\";\n\
chr1\tHAVANA\tCDS\t8001\t8500\t.\t-\t0\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 1;\n\
chr1\tHAVANA\tstart_codon\t8498\t8500\t.\t-\t0\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 1;\n\
chr1\tHAVANA\texon\t4001\t5000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 2; exon_id \"E2\";\n\
chr1\tHAVANA\tCDS\t4001\t5000\t.\t-\t1\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 2;\n\
chr1\tHAVANA\texon\t1001\t2000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 3; exon_id \"E3\";\n\
chr1\tHAVANA\tCDS\t1501\t2000\t.\t-\t2\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 3;\n\
chr1\tHAVANA\tstop_codon\t1498\t1500\t.\t-\t0\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 3;\n\
chr1\tHAVANA\tUTR\t8501\t9000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 1;\n\
chr1\tHAVANA\tUTR\t1001\t1497\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T1.2\"; gene_name \"GENE1\"; exon_number 3;\n\
chr1\tHAVANA\ttranscript\t3001\t9000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T2.1\"; gene_type \"protein_coding\"; gene_name \"GENE1\"; transcript_type \"retained_intron\"; transcript_name \"GENE1-202\";\n\
chr1\tHAVANA\texon\t8001\t9000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T2.1\"; gene_name \"GENE1\"; exon_number 1;\n\
chr1\tHAVANA\texon\t3001\t5000\t.\t-\t.\tgene_id \"G1.5\"; transcript_id \"T2.1\"; gene_name \"GENE1\"; exon_number 2;\n\
chr1\tHAVANA\tgene\t20001\t21000\t.\t+\t.\tgene_id \"G2.1\"; gene_type \"lncRNA\"; gene_name \"LONELY\";\n\
chr1\tRepeatMasker\trepeat\t30001\t30300\t.\t+\t.\trepeat_id \"R1\";\n";

#[test]
fn gtf_lines_become_one_record_per_transcript() {
    let records = all(&load(GTF, FeatureFormat::Gtf));
    // Two transcript models, the childless gene and the unrelated line; not the gene of the
    // drawn transcripts, nor any exon, CDS, UTR or codon line.
    assert_eq!(records.len(), 4, "{records:#?}");
    assert!(
        records
            .iter()
            .all(|record| record.attributes.iter().all(|(key, _)| key != "exon_id"))
    );

    let coding = named(&records, "transcript_id", "T1.2");
    assert_eq!(
        coding,
        &FeatureRecord {
            contig: "chr1".to_owned(),
            start: 1_000,
            end: 9_000,
            name: Some("GENE1".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: Some(7.0),
            strand: Strand::Reverse,
            blocks: vec![(1_000, 2_000), (4_000, 5_000), (8_000, 9_000)],
            // The CDS, extended by the stop codon below it.
            coding_interval: Some((1_497, 8_500)),
            peak: None,
            // The transcript line's attributes, unchanged, then its place in its gene: the
            // representative of two, ahead of T2.1 by its CCDS tag.
            attributes: pairs(&[
                ("gene_id", "G1.5"),
                ("transcript_id", "T1.2"),
                ("gene_type", "protein_coding"),
                ("gene_name", "GENE1"),
                ("transcript_type", "protein_coding"),
                ("transcript_name", "GENE1-201"),
                ("tag", "basic"),
                ("tag", "CCDS"),
                (TRANSCRIPT_CHOICE_ATTRIBUTE, "0\t2\tccds\tT1.2\tG1.5"),
            ]),
        }
    );
    assert_eq!(coding.display_label(), Some("GENE1"));

    let non_coding = named(&records, "transcript_id", "T2.1");
    assert_eq!((non_coding.start, non_coding.end), (3_000, 9_000));
    assert_eq!(non_coding.blocks, [(3_000, 5_000), (8_000, 9_000)]);
    assert_eq!(non_coding.coding_interval, None);
    assert_eq!(non_coding.name.as_deref(), Some("GENE1"));
    assert_eq!(non_coding.strand, Strand::Reverse);

    // A gene with no transcripts, and a line of no transcript, are drawn as they were.
    let lonely = named(&records, "gene_id", "G2.1");
    assert_eq!(
        lonely,
        &parse_gff(GTF.lines().nth(15).unwrap(), true).unwrap()
    );
    let repeat = named(&records, "repeat_id", "R1");
    assert_eq!(
        repeat,
        &parse_gff(GTF.lines().nth(16).unwrap(), true).unwrap()
    );
}

#[test]
fn transcripts_without_exon_lines_are_built_from_cds_and_utr_lines() {
    // GTF 2.2 as some tools write it: no transcript line and no exon lines.
    let text = "\
chr2\tsrc\tCDS\t201\t300\t.\t+\t0\tgene_id \"G\"; transcript_id \"T\"; gene_name \"CODING\";\n\
chr2\tsrc\tUTR\t101\t200\t.\t+\t.\tgene_id \"G\"; transcript_id \"T\"; gene_name \"CODING\";\n\
chr2\tsrc\tCDS\t501\t597\t.\t+\t1\tgene_id \"G\"; transcript_id \"T\"; gene_name \"CODING\";\n\
chr2\tsrc\tstop_codon\t598\t600\t.\t+\t0\tgene_id \"G\"; transcript_id \"T\"; gene_name \"CODING\";\n";
    let records = all(&load(text, FeatureFormat::Gtf));
    assert_eq!(records.len(), 1);
    let model = &records[0];
    assert_eq!((model.start, model.end), (100, 600));
    // The UTR and the CDS beside it are one exon; the stop codon closes the second.
    assert_eq!(model.blocks, [(100, 300), (500, 600)]);
    assert_eq!(model.coding_interval, Some((200, 600)));
    assert_eq!(model.name.as_deref(), Some("CODING"));
    assert_eq!(
        model.attributes,
        pairs(&[
            ("gene_id", "G"),
            ("transcript_id", "T"),
            ("gene_name", "CODING")
        ])
    );
}

/// An Ensembl-style hierarchy: gene → two mRNAs → exons and CDS, one exon shared by both
/// transcripts; a gene with no transcripts; a feature with no hierarchy.
const GFF3: &str = "\
##gff-version 3\n\
chr1\tsrc\tgene\t1001\t9000\t.\t+\t.\tID=gene:G1;Name=ALPHA;gene_id=G1;biotype=protein_coding\n\
chr1\tsrc\tmRNA\t1001\t9000\t.\t+\t.\tID=transcript:T1;Parent=gene:G1;Name=ALPHA-201;transcript_id=T1;tag=basic\n\
chr1\tsrc\tmRNA\t1001\t6000\t.\t+\t.\tID=transcript:T2;Parent=gene:G1;Name=ALPHA-202;transcript_id=T2\n\
chr1\tsrc\texon\t1001\t1500\t.\t+\t.\tID=exon:E1;Parent=transcript:T1,transcript:T2\n\
chr1\tsrc\tfive_prime_UTR\t1001\t1200\t.\t+\t.\tParent=transcript:T1\n\
chr1\tsrc\tCDS\t1201\t1500\t.\t+\t0\tID=CDS:P1;Parent=transcript:T1\n\
chr1\tsrc\texon\t3001\t3600\t.\t+\t.\tID=exon:E2;Parent=transcript:T1\n\
chr1\tsrc\tCDS\t3001\t3600\t.\t+\t1\tID=CDS:P1;Parent=transcript:T1\n\
chr1\tsrc\texon\t5001\t6000\t.\t+\t.\tID=exon:E3;Parent=transcript:T2\n\
chr1\tsrc\texon\t8001\t9000\t.\t+\t.\tID=exon:E4;Parent=transcript:T1\n\
chr1\tsrc\tCDS\t8001\t8400\t.\t+\t2\tID=CDS:P1;Parent=transcript:T1\n\
chr1\tsrc\tthree_prime_UTR\t8401\t9000\t.\t+\t.\tParent=transcript:T1\n\
chr1\tsrc\tgene\t20001\t21000\t5.5\t-\t.\tID=gene:G2;Name=BETA\n\
chr1\tsrc\trepeat_region\t30001\t30400\t.\t.\t.\tID=repeat1;Name=AluY\n";

#[test]
fn gff3_hierarchies_become_one_record_per_transcript() {
    let records = all(&load(GFF3, FeatureFormat::Gff3));
    assert_eq!(records.len(), 4, "{records:#?}");

    let first = named(&records, "ID", "transcript:T1");
    assert_eq!(
        first,
        &FeatureRecord {
            contig: "chr1".to_owned(),
            start: 1_000,
            end: 9_000,
            name: Some("ALPHA".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: Strand::Forward,
            blocks: vec![(1_000, 1_500), (3_000, 3_600), (8_000, 9_000)],
            coding_interval: Some((1_200, 8_400)),
            peak: None,
            // The mRNA line's attributes, with its gene's name and identifier first, then its
            // place in its gene: the representative of two, ahead of T2 by its basic tag.
            attributes: pairs(&[
                ("gene_name", "ALPHA"),
                ("gene_id", "G1"),
                ("ID", "transcript:T1"),
                ("Parent", "gene:G1"),
                ("Name", "ALPHA-201"),
                ("transcript_id", "T1"),
                ("tag", "basic"),
                (TRANSCRIPT_CHOICE_ATTRIBUTE, "0\t2\tbasic\tT1\tG1"),
            ]),
        }
    );
    assert_eq!(first.display_label(), Some("ALPHA"));

    // The exon with two parents is in both transcripts.
    let second = named(&records, "ID", "transcript:T2");
    assert_eq!((second.start, second.end), (1_000, 6_000));
    assert_eq!(second.blocks, [(1_000, 1_500), (5_000, 6_000)]);
    assert_eq!(second.coding_interval, None);
    assert_eq!(second.name.as_deref(), Some("ALPHA"));

    // The childless gene and the orphan feature are drawn as they were.
    let lines = GFF3.lines().collect::<Vec<_>>();
    assert_eq!(
        named(&records, "ID", "gene:G2"),
        &parse_gff(lines[13], false).unwrap()
    );
    assert_eq!(
        named(&records, "ID", "repeat1"),
        &parse_gff(lines[14], false).unwrap()
    );
}

#[test]
fn gene_names_still_find_the_whole_gene_and_transcript_names_the_model() {
    for (text, format, gene, transcript) in [
        (GTF, FeatureFormat::Gtf, "GENE1", "GENE1-202"),
        (GFF3, FeatureFormat::Gff3, "ALPHA", "ALPHA-202"),
    ] {
        let source = load(text, format);
        let hits = source.search(gene, 10).hits;
        let exact = hits
            .iter()
            .filter(|hit| hit.matched_term == gene.to_lowercase())
            .collect::<Vec<_>>();
        assert_eq!(exact.len(), 1, "{gene}: {hits:#?}");
        let record = &exact[0].record;
        assert_eq!(record.feature_type.as_deref(), Some("gene"), "{gene}");
        assert_eq!((record.start, record.end), (1_000, 9_000), "{gene}");

        let hits = source.search(transcript, 10).hits;
        assert_eq!(hits.len(), 1, "{transcript}: {hits:#?}");
        let record = &hits[0].record;
        // The drawn model, with its exons.
        assert_eq!(record.feature_type.as_deref(), Some("transcript"));
        assert_eq!(record.blocks.len(), 2, "{transcript}");
        assert!(all(&source).contains(record), "{transcript}");
    }
}
