#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn cohort_variant_header() -> BcfHeader {
    let mut header = BcfHeader::new();
    header.push_record(b"##contig=<ID=chrVariants,length=20000>");
    header.push_record(br#"##FILTER=<ID=q10,Description="Quality below ten">"#);
    header
        .push_record(br#"##INFO=<ID=END,Number=1,Type=Integer,Description="Structural endpoint">"#);
    header.push_record(
        br#"##INFO=<ID=SVTYPE,Number=1,Type=String,Description="Structural variant type">"#,
    );
    header.push_record(br#"##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency">"#);
    header.push_record(
        br#"##INFO=<ID=SOMATIC,Number=0,Type=Flag,Description="Somatic fixture record">"#,
    );
    header.push_record(
        br#"##INFO=<ID=AC,Number=A,Type=Integer,Description="Alternate allele counts">"#,
    );
    header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
    header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">"#);
    header.push_record(br#"##FORMAT=<ID=AD,Number=R,Type=Integer,Description="Allele depths">"#);
    header.push_record(br#"##FORMAT=<ID=GQ,Number=1,Type=Integer,Description="Genotype quality">"#);
    header.push_record(
        br#"##FORMAT=<ID=PL,Number=G,Type=Integer,Description="Phred genotype likelihoods">"#,
    );
    for sample in ["child", "father", "mother"] {
        header.push_sample(sample.as_bytes());
    }
    for index in 0..125 {
        header.push_sample(format!("cohort-{index:03}").as_bytes());
    }
    header
}

pub(crate) fn write_cohort_variants(vcf_path: &Path, bcf_path: &Path) -> Result<()> {
    write_cohort_variant_format(vcf_path, BcfFormat::Vcf)?;
    bcf_index::build(vcf_path, None, 1, bcf_index::Type::Tbx)?;
    write_cohort_variant_format(bcf_path, BcfFormat::Bcf)?;
    bcf_index::build(bcf_path, None, 1, bcf_index::Type::Csi(14))?;
    Ok(())
}

pub(crate) fn write_cohort_variant_format(path: &Path, format: BcfFormat) -> Result<()> {
    let header = cohort_variant_header();
    let mut writer = BcfWriter::from_path(path, &header, false, format)?;
    let definitions: &[(&str, i64, &[&[u8]], f32, Option<i32>, Option<&str>)] = &[
        ("EXPECTED_SNV", 15_049, &[b"A", b"T"], 60.0, None, None),
        (
            "EXPECTED_MULTI_ALLELIC",
            15_149,
            &[b"A", b"C", b"G"],
            55.0,
            None,
            None,
        ),
        ("EXPECTED_MNV", 15_199, &[b"AC", b"GT"], 52.0, None, None),
        (
            "EXPECTED_INSERTION",
            15_249,
            &[b"A", b"ATGC"],
            50.0,
            None,
            None,
        ),
        (
            "EXPECTED_DELETION",
            15_349,
            &[b"ATGC", b"A"],
            45.0,
            None,
            None,
        ),
        (
            "EXPECTED_INVERSION",
            15_449,
            &[b"N", b"<INV>"],
            40.0,
            Some(15_900),
            Some("INV"),
        ),
        (
            "EXPECTED_SYMBOLIC_DELETION",
            15_999,
            &[b"N", b"<DEL>"],
            9.0,
            Some(16_500),
            Some("DEL"),
        ),
        (
            "EXPECTED_BREAKEND",
            16_599,
            &[b"N", b"N]chrVariants:18001]"],
            48.0,
            None,
            Some("BND"),
        ),
    ];
    for (record_index, (id, position, alleles, quality, end, sv_type)) in
        definitions.iter().enumerate()
    {
        let mut record = writer.empty_record();
        record.set_rid(Some(0));
        record.set_pos(*position);
        record.set_id(id.as_bytes())?;
        record.set_alleles(alleles)?;
        record.set_qual(*quality);
        if *quality < 10.0 {
            record.set_filters(&[b"q10".as_slice()])?;
        }
        if let Some(end) = end {
            record.push_info_integer(b"END", &[*end])?;
        }
        if let Some(sv_type) = sv_type {
            record.push_info_string(b"SVTYPE", &[sv_type.as_bytes()])?;
        }
        let alternate_count = alleles.len().saturating_sub(1);
        let frequencies = (0..alternate_count)
            .map(|offset| 0.05_f32 + (offset as f32 * 0.10))
            .collect::<Vec<_>>();
        record.push_info_float(b"AF", &frequencies)?;
        let counts = (0..alternate_count)
            .map(|offset| i32::try_from(2 + offset + record_index).unwrap_or(i32::MAX))
            .collect::<Vec<_>>();
        record.push_info_integer(b"AC", &counts)?;
        if record_index % 2 == 0 {
            record.push_info_flag(b"SOMATIC")?;
        }
        let mut genotypes = Vec::with_capacity(128);
        let mut depths = Vec::with_capacity(128);
        let mut allele_depths = Vec::new();
        let mut genotype_qualities = Vec::with_capacity(128);
        let mut likelihoods = Vec::with_capacity(128 * 3);
        for sample_index in 0..128_usize {
            let alternate =
                i32::try_from(1 + (sample_index + record_index) % alternate_count.max(1))?;
            if record_index == definitions.len() - 1 && sample_index == 127 {
                genotypes.push(vec![
                    GenotypeAllele::Unphased(0),
                    GenotypeAllele::Unphased(i32::try_from(alternate_count + 7)?),
                ]);
            } else {
                match sample_index % 7 {
                    0 => genotypes.push(vec![
                        GenotypeAllele::Phased(0),
                        GenotypeAllele::Phased(alternate),
                    ]),
                    1 => genotypes.push(vec![
                        GenotypeAllele::Unphased(alternate),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                    2 => genotypes.push(vec![
                        GenotypeAllele::UnphasedMissing,
                        GenotypeAllele::UnphasedMissing,
                    ]),
                    3 => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::UnphasedMissing,
                    ]),
                    4 => genotypes.push(vec![GenotypeAllele::Unphased(alternate)]),
                    5 => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::Unphased(alternate),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                    _ => genotypes.push(vec![
                        GenotypeAllele::Unphased(0),
                        GenotypeAllele::Unphased(alternate),
                    ]),
                }
            }
            let depth = i32::try_from(20 + sample_index % 41)?;
            depths.push(depth);
            genotype_qualities.push(i32::try_from(10 + sample_index % 90)?);
            for allele_index in 0..=alternate_count {
                allele_depths.push(if allele_index == 0 {
                    depth / 2
                } else {
                    depth / i32::try_from(alternate_count + 1)?
                });
            }
            let ploidy = genotypes.last().map_or(2, Vec::len);
            let allele_count = alternate_count + 1;
            let likelihood_count = genotype_likelihood_count(allele_count, ploidy);
            let maximum_likelihood_count = genotype_likelihood_count(allele_count, 3);
            likelihoods.extend((0..likelihood_count).map(|index| {
                i32::try_from(index.saturating_mul(10).saturating_add(sample_index % 7))
                    .unwrap_or(i32::MAX)
            }));
            likelihoods.extend(std::iter::repeat_n(
                i32::MIN + 1,
                maximum_likelihood_count.saturating_sub(likelihood_count),
            ));
        }
        record.push_genotype_structured(&genotypes, 3)?;
        record.push_format_integer(b"DP", &depths)?;
        record.push_format_integer(b"AD", &allele_depths)?;
        record.push_format_integer(b"GQ", &genotype_qualities)?;
        record.push_format_integer(b"PL", &likelihoods)?;
        writer.write(&record)?;
    }
    Ok(())
}

pub(crate) fn genotype_likelihood_count(allele_count: usize, ploidy: usize) -> usize {
    match ploidy {
        0 => 0,
        1 => allele_count,
        2 => allele_count.saturating_mul(allele_count + 1) / 2,
        3 => {
            allele_count
                .saturating_mul(allele_count + 1)
                .saturating_mul(allele_count + 2)
                / 6
        }
        _ => 0,
    }
}

pub(crate) fn basic_record(
    name: &str,
    position: i64,
    sequence: &[u8],
    reverse: bool,
) -> Result<Record> {
    basic_record_with_quality(name, position, sequence, reverse, 35)
}

pub(crate) fn basic_record_with_quality(
    name: &str,
    position: i64,
    sequence: &[u8],
    reverse: bool,
    quality: u8,
) -> Result<Record> {
    let mut record = Record::new();
    let cigar = CigarString(vec![Cigar::Equal(u32::try_from(sequence.len())?)]);
    let qualities = vec![quality; sequence.len()];
    record.set(name.as_bytes(), Some(&cigar), sequence, &qualities);
    record.set_tid(0);
    record.set_pos(position);
    record.set_mapq(60);
    if reverse {
        record.set_reverse();
    }
    Ok(record)
}

pub(crate) fn repeating_bases(length: usize, seed: u8) -> Vec<u8> {
    const BASES: &[u8] = b"ACGTCAGCTCGATCGGCA";
    (0..length)
        .map(|offset| BASES[(offset + usize::from(seed)) % BASES.len()])
        .collect()
}
