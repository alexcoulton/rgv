#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn write_support_files(suite_root: &Path) -> Result<()> {
    write_text(
        &suite_root.join("metadata/samples.tsv"),
        "sample\tcondition\tbatch\tfamily_id\tfather\tmother\taffected\n\
         tumour\tcase\tB\tFAM1\tnormal-father\tnormal-mother\tyes\n\
         normal\tcontrol\tA\tFAM1\t0\t0\tno\n\
         normal-father\tcontrol\tA\tFAM1\t0\t0\tno\n\
         normal-mother\tcontrol\tA\tFAM1\t0\t0\tno\n\
         child\tcase\tC\tPED1\tfather\tmother\tyes\n\
         father\tcontrol\tC\tPED1\t0\t0\tno\n\
         mother\tcontrol\tC\tPED1\t0\t0\tno\n\
         single-cell\tcase\tSC\t.\t0\t0\tunknown\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/cytobands.txt"),
        "chrVariants\t0\t5000\tp11\tgneg\n\
         chrVariants\t5000\t10000\tp12\tgpos50\n\
         chrVariants\t10000\t11000\tcen\tacen\n\
         chrVariants\t11000\t20000\tq11\tgpos75\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/default-annotations.bed"),
        "chrVariants\t8500\t9200\tEXPECTED_SEARCH_FEATURE\t900\t+\n\
         chrVariants\t9400\t12600\tEXPECTED_LARGE_INDEL_REGION\t800\t-\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/annotations.gff3"),
        "##gff-version 3\n\
         chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tID=EXPECTED_GENE;Name=Expected gene\n\
         chrVariants\tRGV\tmRNA\t14501\t17500\t.\t+\t.\tID=EXPECTED_TX;Parent=EXPECTED_GENE\n\
         chrVariants\tRGV\texon\t14501\t15000\t.\t+\t.\tParent=EXPECTED_TX\n\
         chrVariants\tRGV\texon\t17001\t17500\t.\t+\t.\tParent=EXPECTED_TX\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/rna-transcripts.gff3"),
        "##gff-version 3\n\
         chrManual\tRGV\tgene\t19951\t20750\t.\t+\t.\tID=RNA_GENE;Name=RNA fixture gene\n\
         chrManual\tRGV\tmRNA\t19951\t20750\t.\t+\t.\tID=RNA_SHARED_TX;Parent=RNA_GENE\n\
         chrManual\tRGV\texon\t19951\t20040\t.\t+\t.\tParent=RNA_SHARED_TX\n\
         chrManual\tRGV\texon\t20141\t20250\t.\t+\t.\tParent=RNA_SHARED_TX\n\
         chrManual\tRGV\tmRNA\t20201\t20500\t.\t+\t.\tID=RNA_A_SPECIFIC_TX;Parent=RNA_GENE\n\
         chrManual\tRGV\texon\t20201\t20250\t.\t+\t.\tParent=RNA_A_SPECIFIC_TX\n\
         chrManual\tRGV\texon\t20371\t20500\t.\t+\t.\tParent=RNA_A_SPECIFIC_TX\n",
    )?;
    write_gzip(
        &suite_root.join("data/tracks/annotations.gff3.gz"),
        "##gff-version 3\n\
         chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tID=EXPECTED_GENE_GZ;Name=Expected compressed gene\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/annotations.gtf"),
        "chrVariants\tRGV\tgene\t14501\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; gene_name \"Expected GTF gene\";\n\
         chrVariants\tRGV\ttranscript\t14501\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\";\n\
         chrVariants\tRGV\texon\t14501\t15000\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\"; exon_number \"1\";\n\
         chrVariants\tRGV\texon\t17001\t17500\t.\t+\t.\tgene_id \"EXPECTED_GTF_GENE\"; transcript_id \"EXPECTED_GTF_TX\"; exon_number \"2\";\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/transcripts.refFlat"),
        "EXPECTED_REF_FLAT\tEXPECTED_REF_FLAT_TX\tchrVariants\t+\t14500\t17500\t14600\t17400\t2\t14500,17000,\t15000,17500,\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/signal-fixed.wig"),
        "fixedStep chrom=chrVariants start=14501 step=100 span=100\n1.0\n2.0\n4.0\n8.0\n",
    )?;
    write_text(
        &suite_root.join("data/tracks/cohort.cbs"),
        "ID\tchrom\tloc.start\tloc.end\tnum.mark\tseg.mean\n\
         child\tchrVariants\t14501\t16000\t80\t0.75\n\
         father\tchrVariants\t16001\t17500\t75\t-0.50\n",
    )?;
    for extension in ["linear", "logistic", "assoc", "qassoc"] {
        write_text(
            &suite_root.join(format!("data/tracks/association.{extension}")),
            "CHR\tBP\tSNP\tP\tBETA\n\
             chrVariants\t15050\tEXPECTED_ASSOC_1\t1e-12\t0.8\n\
             chrVariants\t16600\tEXPECTED_ASSOC_2\t0.02\t-0.2\n",
        )?;
    }
    write_text(
        &suite_root.join("imports/regions.bed"),
        "track name=\"RGV manual regions\" itemRgb=On\n\
         chrVariants\t0\t1\tcontig-start\t0\t.\t0\t1\t45,92,175\n\
         chrVariants\t9400\t12600\tlarge-indels\t0\t.\t9400\t12600\t220,80,60\n\
         chrManual\t49900\t50600\tultima-evidence\t0\t.\t49900\t50600\t126,87,194\n\
         chrVariants\t19999\t20000\tcontig-end\t0\t.\t19999\t20000\t40,150,90\n",
    )?;
    write_text(
        &suite_root.join("imports/review-items.tsv"),
        "#RGV-review-queue-v1\n\
         contig\tstart\tend\tstatus\tlabel_json\tassignment_json\tnotes_json\n\
         chrVariants\t0\t1\tpending\t\"Review contig start\"\t\"tester-a\"\t\"Boundary coordinate must remain zero-based internally.\"\n\
         chrVariants\t9400\t12600\tin_progress\t\"Review large indels\"\tnull\t\"Expected 1000 bp insertion and 2000 bp deletion\"\n\
         chrManual\t49900\t50600\tpending\t\"Review Ultima\"\t\"tester-b\"\t\"Expected tp, mc, t0, missing, and malformed evidence\"\n\
         chrVariants\t19999\t20000\tcomplete\t\"Review contig end\"\tnull\t\"Last valid base of chrVariants.\"\n",
    )?;
    write_text(
        &suite_root.join("imports/representative-igv-session.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Session genome=\"manual\" locus=\"chrVariants:9401-12600\" version=\"8\">\n\
           <Resources>\n\
             <Resource path=\"../../variants.bam\" index=\"../../variants.bam.bai\"/>\n\
             <Resource path=\"../../variants.fa\"/>\n\
             <Resource path=\"../../demo-tracks/features.bed.gz\"/>\n\
           </Resources>\n\
           <Panel name=\"DataPanel\"><Track id=\"../../variants.bam\" name=\"Imported alignment\" color=\"0,0,150\" height=\"320\" displayMode=\"EXPANDED\"/></Panel>\n\
           <Panel name=\"IgnoredPanel\"><Track id=\"unsupported.hic\" name=\"Unsupported Hi-C\"/></Panel>\n\
         </Session>\n",
    )?;
    let igv_xml = fs::read_to_string(suite_root.join("imports/representative-igv-session.xml"))?;
    let imported = SessionDocument::import_igv_with_report(&igv_xml)?;
    write_json(
        &suite_root.join("expected/igv-import-report.json"),
        &serde_json::json!({
            "schema_version": 1,
            "locus": imported.document.locus,
            "alignment_track_count": imported.document.alignment_tracks.len(),
            "track_count": imported.document.tracks.len(),
            "imported": imported.report.imported,
            "approximated": imported.report.approximated,
            "ignored": imported.report.ignored,
            "omitted_notice_count": imported.report.omitted_notice_count
        }),
    )?;
    write_text(
        &suite_root.join("failures/changed-source.bed"),
        "chrVariants\t10000\t10100\tCURRENT_CHANGED_RECORD\n",
    )?;
    write_json(
        &suite_root.join("expected/changed-source-state.json"),
        &serde_json::json!({
            "schema_version": 1,
            "path": "../failures/changed-source.bed",
            "saved_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
            "expected_behavior": "report-changed-and-keep-healthy-sources"
        }),
    )?;
    write_text(
        &suite_root.join("failures/invalid-session.json"),
        "{\"schema_version\":99,\"locus\":{\"contig\":\"chr1\",\"start\":0,\"end\":1}}\n",
    )?;
    write_text(
        &suite_root.join("failures/truncated.bed"),
        "chrVariants\t9000\n",
    )?;

    let genome = GenomeConfiguration {
        schema_version: GENOME_CONFIGURATION_SCHEMA_VERSION,
        name: "RGV portable manual genome".to_owned(),
        assembly_identity: None,
        reference: local_source("../../variants.fa"),
        reference_index: None,
        cytobands: Some(local_source("../data/tracks/cytobands.txt")),
        contig_aliases: BTreeMap::from([("variants".to_owned(), "chrVariants".to_owned())]),
        default_annotations: vec![GenomeAnnotation {
            name: "Manual search annotations".to_owned(),
            kind: TrackKind::Feature,
            source: local_source("../data/tracks/default-annotations.bed"),
            format: None,
        }],
        asset_provenance: Vec::new(),
    };
    write_json(&suite_root.join("genomes/manual.rgvgenome"), &genome)
}

pub(crate) fn write_gzip(path: &Path, contents: &str) -> Result<()> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(contents.as_bytes())?;
    fs::write(path, encoder.finish()?)
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn write_specialist_reference(path: &Path) -> Result<()> {
    let sequence = manual_reference_sequence();
    write_indexed_fasta(path, &sequence)
}

pub(crate) fn write_wrong_reference(path: &Path) -> Result<()> {
    let sequence = vec![b'T'; MANUAL_CONTIG_LENGTH];
    write_indexed_fasta(path, &sequence)
}

pub(crate) fn write_indexed_fasta(path: &Path, sequence: &[u8]) -> Result<()> {
    let mut fasta = format!(">{MANUAL_CONTIG}\n");
    for chunk in sequence.chunks(80) {
        fasta.push_str(std::str::from_utf8(chunk)?);
        fasta.push('\n');
    }
    write_text(path, &fasta)?;
    let offset = MANUAL_CONTIG.len() + 2;
    write_text(
        &PathBuf::from(format!("{}.fai", path.display())),
        &format!("{MANUAL_CONTIG}\t{MANUAL_CONTIG_LENGTH}\t{offset}\t80\t81\n"),
    )
}

#[derive(Clone, Copy)]
pub(crate) enum RnaStrandTag {
    Positive,
    Negative,
    Missing,
    Invalid,
}

pub(crate) fn write_rna_bams(directory: &Path) -> Result<()> {
    write_rna_bam(&directory.join("rna-sample-a.bam"), 'A')?;
    write_rna_bam(&directory.join("rna-sample-b.bam"), 'B')
}

pub(crate) fn write_rna_bam(path: &Path, sample: char) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    let read_group = format!("rna-{sample}");
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", &read_group)
            .push_tag(b"SM", format!("RNA sample {sample}"))
            .push_tag(b"PL", "ILLUMINA"),
    );

    let mut records = Vec::new();
    let shared_count = if sample == 'A' { 5 } else { 4 };
    for index in 0..shared_count {
        let tag = match index {
            0 => RnaStrandTag::Positive,
            1 => RnaStrandTag::Negative,
            2 => RnaStrandTag::Missing,
            _ => RnaStrandTag::Invalid,
        };
        records.push(rna_record(
            &format!("EXPECTED_RNA_{sample}_SHARED_{index}"),
            20_000,
            vec![Cigar::Equal(40), Cigar::RefSkip(100), Cigar::Equal(40)],
            index % 2 == 1,
            tag,
            &read_group,
        )?);
    }
    if sample == 'A' {
        for index in 0..3 {
            records.push(rna_record(
                &format!("EXPECTED_RNA_A_SPECIFIC_{index}"),
                20_200,
                vec![Cigar::Equal(50), Cigar::RefSkip(120), Cigar::Equal(50)],
                false,
                RnaStrandTag::Positive,
                &read_group,
            )?);
        }
    } else {
        for index in 0..2 {
            records.push(rna_record(
                &format!("EXPECTED_RNA_B_SPECIFIC_{index}"),
                20_400,
                vec![Cigar::Equal(35), Cigar::RefSkip(200), Cigar::Equal(35)],
                true,
                RnaStrandTag::Negative,
                &read_group,
            )?);
        }
        records.push(rna_record(
            "EXPECTED_RNA_B_ALTERNATIVE_DONOR",
            20_000,
            vec![Cigar::Equal(50), Cigar::RefSkip(90), Cigar::Equal(40)],
            false,
            RnaStrandTag::Positive,
            &read_group,
        )?);
        records.push(rna_record(
            "EXPECTED_RNA_B_ALTERNATIVE_ACCEPTOR",
            20_000,
            vec![Cigar::Equal(40), Cigar::RefSkip(110), Cigar::Equal(40)],
            false,
            RnaStrandTag::Positive,
            &read_group,
        )?);
    }
    records.push(rna_record(
        &format!("EXPECTED_RNA_{sample}_MULTI_EXON"),
        20_500,
        vec![
            Cigar::Equal(30),
            Cigar::RefSkip(80),
            Cigar::Equal(30),
            Cigar::RefSkip(90),
            Cigar::Equal(30),
        ],
        sample == 'B',
        RnaStrandTag::Missing,
        &read_group,
    )?);
    records.push(rna_record(
        &format!("EXPECTED_RNA_{sample}_RETAINED_INTRON"),
        20_000,
        vec![Cigar::Equal(180)],
        false,
        RnaStrandTag::Positive,
        &read_group,
    )?);
    records.sort_by_key(Record::pos);
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    ensure_fixture_alignments_are_mapped(path)?;
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

pub(crate) fn rna_record(
    name: &str,
    position: i64,
    cigar: Vec<Cigar>,
    reverse: bool,
    strand_tag: RnaStrandTag,
    read_group: &str,
) -> Result<Record> {
    let sequence_length = cigar
        .iter()
        .map(|operation| match operation {
            Cigar::Match(length)
            | Cigar::Ins(length)
            | Cigar::SoftClip(length)
            | Cigar::Equal(length)
            | Cigar::Diff(length) => usize::try_from(*length).unwrap_or_default(),
            _ => 0,
        })
        .sum();
    let sequence = repeating_bases(sequence_length, name.as_bytes()[0]);
    let qualities = vec![35; sequence_length];
    let mut record = Record::new();
    record.set(
        name.as_bytes(),
        Some(&CigarString(cigar)),
        &sequence,
        &qualities,
    );
    record.set_tid(0);
    record.set_pos(position);
    record.set_mapq(60);
    // `Record::new` starts life as an unmapped record. These fixtures describe
    // real alignments, so replace the default flags instead of only OR-ing in
    // the reverse-strand bit.
    record.set_flags(if reverse { 0x10 } else { 0 });
    record.push_aux(b"RG", Aux::String(read_group))?;
    match strand_tag {
        RnaStrandTag::Positive => record.push_aux(b"XS", Aux::Char(b'+'))?,
        RnaStrandTag::Negative => record.push_aux(b"XS", Aux::Char(b'-'))?,
        RnaStrandTag::Missing => {}
        RnaStrandTag::Invalid => record.push_aux(b"XS", Aux::Char(b'?'))?,
    }
    Ok(record)
}

#[derive(Clone, Copy)]
pub(crate) enum SmrtFixtureCase {
    Valid,
    Missing,
    WrongLength,
    WrongType,
    Negative,
}

pub(crate) fn write_smrt_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "smrt")
            .push_tag(b"SM", "smrt")
            .push_tag(b"PL", "PACBIO"),
    );
    let mut records = Vec::new();
    for (offset, (name, reverse, fixture_case)) in [
        ("EXPECTED_SMRT_VALID_FORWARD", false, SmrtFixtureCase::Valid),
        ("EXPECTED_SMRT_VALID_REVERSE", true, SmrtFixtureCase::Valid),
        (
            "EXPECTED_SMRT_MISSING_TAGS",
            false,
            SmrtFixtureCase::Missing,
        ),
        (
            "EXPECTED_SMRT_WRONG_LENGTH_IP",
            false,
            SmrtFixtureCase::WrongLength,
        ),
        (
            "EXPECTED_SMRT_WRONG_TYPE_IP",
            false,
            SmrtFixtureCase::WrongType,
        ),
        (
            "EXPECTED_SMRT_NEGATIVE_IP",
            false,
            SmrtFixtureCase::Negative,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut record = basic_record(
            name,
            30_000 + i64::try_from(offset)? * 10,
            &repeating_bases(100, u8::try_from(offset)?),
            reverse,
        )?;
        record.push_aux(b"RG", Aux::String("smrt"))?;
        push_smrt_fixture_tags(&mut record, fixture_case)?;
        records.push(record);
    }
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    ensure_fixture_alignments_are_mapped(path)?;
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

pub(crate) fn push_smrt_fixture_tags(
    record: &mut Record,
    fixture_case: SmrtFixtureCase,
) -> Result<()> {
    if matches!(fixture_case, SmrtFixtureCase::Missing) {
        return Ok(());
    }
    let values = (0..100_u16)
        .map(|index| index.saturating_mul(75))
        .collect::<Vec<_>>();
    for tag in [b"ip", b"pw", b"fi", b"ri", b"fp", b"rp"] {
        if tag == b"ip" {
            match fixture_case {
                SmrtFixtureCase::WrongLength => {
                    record.push_aux(tag, Aux::ArrayU16((&values[..3]).into()))?;
                    continue;
                }
                SmrtFixtureCase::WrongType => {
                    record.push_aux(tag, Aux::String("not-an-array"))?;
                    continue;
                }
                SmrtFixtureCase::Negative => {
                    let mut signed = values
                        .iter()
                        .map(|value| i16::try_from(*value).unwrap_or(i16::MAX))
                        .collect::<Vec<_>>();
                    signed[5] = -1;
                    record.push_aux(tag, Aux::ArrayI16((&signed[..]).into()))?;
                    continue;
                }
                SmrtFixtureCase::Valid | SmrtFixtureCase::Missing => {}
            }
        }
        record.push_aux(tag, Aux::ArrayU16((&values[..]).into()))?;
    }
    Ok(())
}

pub(crate) fn write_sbx_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    let mut records = Vec::new();
    for (offset, (name, reverse, insertion_low_bases, all_tail, missing_qualities, boundary)) in [
        (
            "EXPECTED_SBX_CONCORDANT",
            false,
            0_usize,
            false,
            false,
            false,
        ),
        ("EXPECTED_SBX_PARTIAL", false, 1, false, false, false),
        ("EXPECTED_SBX_FULL", false, 10, false, false, false),
        ("EXPECTED_SBX_REVERSE", true, 0, false, false, false),
        ("EXPECTED_SBX_ALL_TAIL", false, 10, true, false, false),
        (
            "EXPECTED_SBX_MISSING_QUALITIES",
            false,
            0,
            false,
            true,
            false,
        ),
        (
            "EXPECTED_SBX_QUALITY_BOUNDARY",
            false,
            0,
            false,
            false,
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let cigar = CigarString(vec![
            Cigar::SoftClip(20),
            Cigar::Equal(50),
            Cigar::Ins(10),
            Cigar::Equal(50),
            Cigar::SoftClip(20),
        ]);
        let sequence = repeating_bases(150, u8::try_from(offset)?);
        let mut qualities = vec![35_u8; 150];
        qualities[..20].fill(20);
        qualities[130..].fill(20);
        qualities[70..70 + insertion_low_bases].fill(5);
        if all_tail {
            qualities.fill(5);
        }
        if missing_qualities {
            qualities.fill(u8::MAX);
        }
        if boundary {
            qualities[70..73].copy_from_slice(&[9, 10, 30]);
        }
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(40_000 + i64::try_from(offset)? * 10);
        record.set_mapq(60);
        record.set_flags(if reverse { 0x10 } else { 0 });
        records.push(record);
    }
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    ensure_fixture_alignments_are_mapped(path)?;
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

pub(crate) fn write_ultima_bam(path: &Path) -> Result<()> {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "ultima")
            .push_tag(b"SM", "ultima")
            .push_tag(b"PL", "ULTIMA")
            .push_tag(b"mc", 1)
            .push_tag(b"tp", "reference"),
    );

    let cases = [
        ("EXPECTED_ULTIMA_FORWARD_Q40", false, Some("valid")),
        ("EXPECTED_ULTIMA_REVERSE_Q0", true, Some("valid")),
        ("EXPECTED_ULTIMA_MISSING_TP", false, None),
        ("EXPECTED_ULTIMA_WRONG_LENGTH_TP", false, Some("short")),
        ("EXPECTED_ULTIMA_WRONG_TYPE_TP", false, Some("string")),
    ];
    let cigar = CigarString(vec![Cigar::Equal(3), Cigar::Ins(2), Cigar::Equal(3)]);
    let forward_sequence = b"ACGAAATC";
    let mut records = Vec::new();
    for (offset, (name, reverse, tp_case)) in cases.into_iter().enumerate() {
        let mut sequence = forward_sequence.to_vec();
        let mut qualities = vec![30_u8; sequence.len()];
        qualities[4] = if offset == 1 { 0 } else { 40 };
        let mut tp = vec![0_i32; sequence.len()];
        tp[4] = -2;
        if reverse {
            sequence = reverse_complement(&sequence);
            qualities.reverse();
            tp.reverse();
        }
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(50_000 + i64::try_from(offset)? * 10);
        record.set_mapq(60);
        record.push_aux(b"RG", Aux::String("ultima"))?;
        record.set_flags(if reverse { 0x10 } else { 0 });
        match tp_case {
            Some("valid") => record.push_aux(b"tp", Aux::ArrayI32((&tp[..]).into()))?,
            Some("short") => record.push_aux(b"tp", Aux::ArrayI32((&tp[..3]).into()))?,
            Some("string") => record.push_aux(b"tp", Aux::String("not-an-array"))?,
            None => {}
            Some(other) => anyhow::bail!("unknown Ultima fixture case {other}"),
        }
        records.push(record);
    }

    let reference = manual_reference_sequence();
    for (name, start, t0_quality) in [
        ("EXPECTED_ULTIMA_DELETION_MC_Q0", 50_109_usize, None),
        ("EXPECTED_ULTIMA_DELETION_T0_Q35", 50_130, Some(35_u8)),
    ] {
        let sequence = [
            &reference[start..start + 3],
            &reference[start + 4..start + 7],
        ]
        .concat();
        let qualities = vec![30_u8; sequence.len()];
        let cigar = CigarString(vec![Cigar::Equal(3), Cigar::Del(1), Cigar::Equal(3)]);
        let mut record = Record::new();
        record.set(name.as_bytes(), Some(&cigar), &sequence, &qualities);
        record.set_tid(0);
        record.set_pos(i64::try_from(start)?);
        record.set_mapq(60);
        record.set_flags(0);
        record.push_aux(b"RG", Aux::String("ultima"))?;
        let tp = vec![0_i32; sequence.len()];
        record.push_aux(b"tp", Aux::ArrayI32((&tp[..]).into()))?;
        if let Some(quality) = t0_quality {
            let mut values = vec![20_u8; sequence.len()];
            values[2] = quality;
            values[3] = quality;
            let encoded = values
                .iter()
                .map(|value| char::from(value.saturating_add(33)))
                .collect::<String>();
            record.push_aux(b"t0", Aux::String(&encoded))?;
        }
        records.push(record);
    }
    records.sort_by_key(Record::pos);
    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    ensure_fixture_alignments_are_mapped(path)?;
    let index = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&index), bam::index::Type::Bai, 1)?;
    Ok(())
}

pub(crate) fn write_specialist_bam(path: &Path) -> Result<()> {
    // MD5 of the deterministic 100,000-base `manual_reference_sequence`. CRAM requires the
    // sequence digest in SQ so it remains genuinely reference-dependent rather than embedding it.
    const REFERENCE_MD5: &str = "1bd65b2b27c9e6c1cb4ad93cbb50516d";
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", MANUAL_CONTIG)
            .push_tag(b"LN", MANUAL_CONTIG_LENGTH)
            .push_tag(b"M5", REFERENCE_MD5)
            .push_tag(b"UR", "manual.fa"),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "bisulfite")
            .push_tag(b"SM", "bisulfite")
            .push_tag(b"PL", "ILLUMINA"),
    );
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "single-cell")
            .push_tag(b"SM", "single-cell")
            .push_tag(b"PL", "ILLUMINA"),
    );
    let mut records = Vec::new();
    let reference = manual_reference_sequence();
    for (offset, (name, reverse, converted)) in [
        ("EXPECTED_CG_PROTECTED_FORWARD", false, false),
        ("EXPECTED_CG_CONVERTED_FORWARD", false, true),
        ("EXPECTED_CG_PROTECTED_REVERSE", true, false),
        ("EXPECTED_CG_CONVERTED_REVERSE", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let start = 1_000 + offset * 4;
        // BAM SEQ is stored in reference-aligned orientation. Reverse-strand
        // conversion therefore appears as G-to-A, not as an independently
        // reverse-complemented sequence.
        let mut sequence = reference[start..start + 160].to_vec();
        if converted {
            let source = if reverse { b'G' } else { b'C' };
            let converted = if reverse { b'A' } else { b'T' };
            for base in &mut sequence {
                if *base == source {
                    *base = converted;
                }
            }
        }
        let mut record = specialist_record(name, i64::try_from(start)?, &sequence, reverse, 35)?;
        record.push_aux(b"RG", Aux::String("bisulfite"))?;
        records.push(record);
    }
    let mut low_quality = specialist_record(
        "EXPECTED_BISULFITE_LOW_QUALITY",
        1_020,
        &reference[1_020..1_180],
        false,
        5,
    )?;
    low_quality.push_aux(b"RG", Aux::String("bisulfite"))?;
    records.push(low_quality);
    let mut missing = specialist_record(
        "EXPECTED_BISULFITE_MISSING_EVIDENCE",
        1_024,
        &[b'N'; 160],
        false,
        35,
    )?;
    missing.push_aux(b"RG", Aux::String("bisulfite"))?;
    records.push(missing);
    let pair_name = "EXPECTED_BISULFITE_PAIRED_ORIENTATION";
    let left_start = 1_040_usize;
    let right_start = 1_120_usize;
    let mut left = specialist_record(
        pair_name,
        i64::try_from(left_start)?,
        &reference[left_start..left_start + 160],
        false,
        35,
    )?;
    left.set_flags(0x63);
    left.set_mtid(0);
    left.set_mpos(i64::try_from(right_start)?);
    left.set_insert_size(240);
    left.push_aux(b"RG", Aux::String("bisulfite"))?;
    // Both ends inherit the first end's conversion strand. Although the right
    // alignment carries the reverse flag, its aligned bisulfite evidence is
    // still forward-strand C-to-T.
    let mut right_sequence = reference[right_start..right_start + 160].to_vec();
    for base in &mut right_sequence {
        if *base == b'C' {
            *base = b'T';
        }
    }
    let mut right = specialist_record(
        pair_name,
        i64::try_from(right_start)?,
        &right_sequence,
        true,
        35,
    )?;
    right.set_flags(0x93);
    right.set_mtid(0);
    right.set_mpos(i64::try_from(left_start)?);
    right.set_insert_size(-240);
    right.push_aux(b"RG", Aux::String("bisulfite"))?;
    records.extend([left, right]);
    for (offset, (cell, raw_cell, umi, raw_umi)) in [
        ("CELL_A-1", "RAW_CELL_A", "UMI0001", "RAWUMI01"),
        ("CELL_A-1", "RAW_CELL_A", "UMI0001", "RAWUMI01"),
        ("CELL_A-1", "RAW_CELL_A", "UMI0002", "RAWUMI02"),
        ("CELL_B-1", "RAW_CELL_B", "UMI0001", "RAWUMI11"),
        ("CELL_B-1", "RAW_CELL_B", "UMI0003", "RAWUMI13"),
        ("CELL_C-1", "RAW_CELL_C", "UMI0009", "RAWUMI29"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("EXPECTED_{cell}_{umi}_{offset}");
        let mut record = basic_record(
            &name,
            5_000 + i64::try_from(offset)? * 5,
            &repeating_bases(100, offset as u8),
            offset % 2 == 1,
        )?;
        record.push_aux(b"RG", Aux::String("single-cell"))?;
        record.push_aux(b"CB", Aux::String(cell))?;
        record.push_aux(b"CR", Aux::String(raw_cell))?;
        record.push_aux(b"UB", Aux::String(umi))?;
        record.push_aux(b"UR", Aux::String(raw_umi))?;
        record.push_aux(b"HP", Aux::I32(i32::try_from(offset % 2 + 1)?))?;
        records.push(record);
    }
    let mut missing = basic_record(
        "EXPECTED_MISSING_CELL_TAGS",
        5_040,
        &repeating_bases(100, 9),
        false,
    )?;
    missing.push_aux(b"RG", Aux::String("single-cell"))?;
    records.push(missing);
    let mut malformed = basic_record(
        "EXPECTED_MALFORMED_CELL_TAG",
        5_045,
        &repeating_bases(100, 10),
        true,
    )?;
    malformed.push_aux(b"RG", Aux::String("single-cell"))?;
    malformed.push_aux(b"CB", Aux::I32(42))?;
    malformed.push_aux(b"CR", Aux::String("RAW_MALFORMED"))?;
    malformed.push_aux(b"UB", Aux::String("UMI_BAD"))?;
    malformed.push_aux(b"UR", Aux::String("RAW_UMI_BAD"))?;
    records.push(malformed);
    records.sort_by_key(Record::pos);

    let mut writer = Writer::from_path(path, &header, Format::Bam)?;
    for record in &records {
        writer.write(record)?;
    }
    drop(writer);
    ensure_fixture_alignments_are_mapped(path)?;
    let bai = PathBuf::from(format!("{}.bai", path.display()));
    bam::index::build(path, Some(&bai), bam::index::Type::Bai, 1)?;
    let csi = PathBuf::from(format!("{}.csi", path.display()));
    bam::index::build(path, Some(&csi), bam::index::Type::Csi(14), 1)?;
    Ok(())
}

fn specialist_record(
    name: &str,
    position: i64,
    sequence: &[u8],
    reverse: bool,
    quality: u8,
) -> Result<Record> {
    let mut record = Record::new();
    // These records intentionally contain conversion differences, so `M` is
    // the valid CIGAR operation; `=` would falsely assert every base matches.
    let cigar = CigarString(vec![Cigar::Match(u32::try_from(sequence.len())?)]);
    let qualities = vec![quality; sequence.len()];
    record.set(name.as_bytes(), Some(&cigar), sequence, &qualities);
    record.set_tid(0);
    record.set_pos(position);
    record.set_mapq(60);
    record.set_flags(if reverse { 0x10 } else { 0 });
    Ok(record)
}

fn ensure_fixture_alignments_are_mapped(path: &Path) -> Result<()> {
    let mut reader = Reader::from_path(path)?;
    let mut count = 0_usize;
    for result in reader.records() {
        let record = result?;
        count += 1;
        ensure!(
            !record.is_unmapped(),
            "manual fixture {} contains an unintentionally unmapped record: {}",
            path.display(),
            String::from_utf8_lossy(record.qname())
        );
    }
    ensure!(
        count > 0,
        "manual alignment fixture {} contains no records",
        path.display()
    );
    Ok(())
}

pub(crate) fn reverse_complement(sequence: &[u8]) -> Vec<u8> {
    sequence
        .iter()
        .rev()
        .map(|base| match base.to_ascii_uppercase() {
            b'A' => b'T',
            b'C' => b'G',
            b'G' => b'C',
            b'T' | b'U' => b'A',
            other => other,
        })
        .collect()
}

pub(crate) fn manual_reference_sequence() -> Vec<u8> {
    const MOTIF: &[u8] = b"ACGTCAGCTCGATCGGCH";
    (0..MANUAL_CONTIG_LENGTH)
        .map(|index| {
            let base = MOTIF[index % MOTIF.len()];
            if base == b'H' { b'A' } else { base }
        })
        .collect()
}

pub(crate) fn write_specialist_cram(
    bam_path: &Path,
    reference_path: &Path,
    cram_path: &Path,
) -> Result<()> {
    let mut reader = Reader::from_path(bam_path)?;
    let header = Header::from_template(reader.header());
    let mut writer = Writer::from_path(cram_path, &header, Format::Cram)?;
    writer.set_reference(reference_path)?;
    for result in reader.records() {
        writer.write(&result?)?;
    }
    drop(writer);
    let crai = PathBuf::from(format!("{}.crai", cram_path.display()));
    bam::index::build(cram_path, Some(&crai), bam::index::Type::Bai, 1)?;
    Ok(())
}
