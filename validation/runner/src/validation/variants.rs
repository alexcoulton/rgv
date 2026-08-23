#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn verify_variant_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let bcf = fixture_path(root, manifest, "manual-cohort-bcf")?;
    let vcf = fixture_path(root, manifest, "manual-cohort-vcf")?;
    let mut source = LocalVariantSource::open(&bcf)?;
    let query = source.query("chrVariants", 14_900, 18_100, 100)?;
    ensure!(
        query.header.samples.len() == 128
            && query.header.samples[..3] == ["child", "father", "mother"],
        "MF-09 sample count or pedigree-leading order changed"
    );
    ensure!(
        query.records.len() == 8,
        "MF-09 has {} variant records instead of 8",
        query.records.len()
    );
    let types = query
        .records
        .iter()
        .flat_map(|record| &record.variant_types)
        .collect::<Vec<_>>();
    for expected in [
        VariantType::Snv,
        VariantType::Mnv,
        VariantType::Insertion,
        VariantType::Deletion,
        VariantType::Inversion,
        VariantType::StructuralDeletion,
        VariantType::Breakend,
    ] {
        ensure!(
            types.contains(&&expected),
            "MF-09 lacks variant type {expected:?}"
        );
    }
    ensure!(
        query
            .records
            .iter()
            .any(|record| record.alternates.len() == 2),
        "MF-09 lacks its multi-allelic record"
    );
    ensure!(
        query
            .records
            .iter()
            .flat_map(|record| &record.genotypes)
            .any(|genotype| genotype.alleles.len() == 1)
            && query
                .records
                .iter()
                .flat_map(|record| &record.genotypes)
                .any(|genotype| genotype.alleles.len() == 3)
            && query.header.formats.iter().any(|field| field.id == "AD")
            && query.header.formats.iter().any(|field| field.id == "GQ")
            && query.header.formats.iter().any(|field| field.id == "PL"),
        "MF-09 lacks haploid/polyploid genotypes or AD/GQ/PL FORMAT truth"
    );
    let malformed = query
        .records
        .iter()
        .find(|record| record.id.as_deref() == Some("EXPECTED_BREAKEND"))
        .context("MF-09 lacks EXPECTED_BREAKEND")?;
    ensure!(
        malformed
            .genotypes
            .last()
            .is_some_and(|genotype| genotype.alleles.iter().any(|allele| {
                allele
                    .index
                    .is_some_and(|index| index as usize > malformed.alternates.len())
            })),
        "MF-09 lost its readable out-of-range malformed genotype boundary"
    );
    let mut vcf_source = LocalVariantSource::open(&vcf)?;
    let vcf_query = vcf_source.query("chrVariants", 14_900, 18_100, 100)?;
    ensure!(
        vcf_query.header == query.header && vcf_query.records == query.records,
        "MF-09 VCF and BCF semantic truth differ"
    );
    let project = project_document(root, manifest, "mf-09-cohort-variants")?;
    let pedigree_order = project
        .sample_metadata
        .as_ref()
        .context("MF-09 project has no sample metadata")?
        .pedigree_sample_order(&query.header.samples)
        .context("MF-09 metadata did not produce a pedigree order")?;
    ensure!(
        pedigree_order.starts_with(&[0, 1, 2]),
        "MF-09 pedigree order does not lead with child, father, and mother"
    );
    Ok(())
}
