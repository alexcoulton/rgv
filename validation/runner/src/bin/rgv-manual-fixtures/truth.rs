#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn write_truth_documents(suite_root: &Path) -> Result<()> {
    let truths = [
        truth(
            "truth-mf-01",
            &["dep-mixed-bam"],
            "mixed-synthetic",
            vec![truth_locus(
                "structural-pairs",
                "chr1:2590001-2650000",
                "chr1",
                2_590_000,
                2_650_000,
                &[
                    "Long, short, same-strand, outward-facing, inter-contig, and unmapped-mate templates are present.",
                    "Named sparse controls cover duplicate, QC-fail, MAPQ 0, missing qualities, secondary, RG, and NM evidence.",
                    "Deep coverage and long/supplementary loci are reachable through the review queue.",
                    "Named long-read controls cover valid multiple-code/strand/probability MM/ML, missing tags, malformed MM, mismatched ML length, and wrong MM type.",
                ],
            )],
        ),
        truth(
            "truth-mf-02",
            &["dep-variants-bam", "dep-variants-fasta"],
            "variants-synthetic",
            vec![truth_locus(
                "large-indels",
                "chrVariants:9401-12600",
                "chrVariants",
                9_400,
                12_600,
                &[
                    "A 1,000-base insertion and a 2,000-base deletion start from the shared 9,500 zero-based alignment position.",
                    "The 25-record base-level locus contains exact-match controls, SNVs, a three-base MNV, four Ns, Q5 and reverse-strand controls, and a compound event.",
                    "Named insertions have lengths 1, 2, 3, 6, 10, 100, and 1,000; named deletions have lengths 1, 2, 10, 12, 100, 500, 1,000, and 2,000.",
                    "Named records exercise M, =, X, I, D, S, H, N, and P CIGAR operations, including adjacent skip/padding/insertion events.",
                    "At displayed base chrVariants:10290 the bounded pileup bin has depth 7, forward/reverse depth 6/1, and two deletion observations.",
                ],
            )],
        ),
        truth(
            "truth-mf-03",
            &[
                "manual-rna-a-bam",
                "manual-rna-b-bam",
                "manual-rna-annotation",
            ],
            "manual-synthetic",
            vec![truth_locus(
                "rna-junctions",
                "chrManual:19901-21100",
                MANUAL_CONTIG,
                19_900,
                21_100,
                &[
                    "Sample A contains five observations of the shared 20040-20140 junction and three of the annotated A-specific 20250-20370 junction.",
                    "Sample B contains four observations of the shared junction, two of the unannotated B-specific 20435-20635 junction, and alternative donor and acceptor events.",
                    "Both samples contain a two-junction multi-exon read and an unspliced retained-intron record.",
                    "Tagged positive, tagged negative, inferred missing, and invalid XS strand evidence are all present.",
                ],
            )],
        ),
        truth(
            "truth-mf-04",
            &["manual-specialist-bam", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "bisulfite",
                "chrManual:901-1200",
                MANUAL_CONTIG,
                900,
                1_200,
                &[
                    "Eight records cover forward/reverse converted/protected evidence, a low-quality call, missing evidence, and a two-read inward proper-pair orientation control.",
                    "Every CG, CHG, CHH, HCG, GCH, and WCG display mode has both protected and converted observations against the matching reference.",
                ],
            )],
        ),
        truth(
            "truth-mf-05",
            &["manual-smrt-bam"],
            "manual-synthetic",
            vec![truth_locus(
                "smrt",
                "chrManual:29901-30600",
                MANUAL_CONTIG,
                29_900,
                30_600,
                &[
                    "Forward and reverse records expose exact 100-value ip, pw, fi, ri, fp, and rp arrays in all six preconfigured modes.",
                    "Named controls expose missing tags, a three-value ip array, a string ip value, and a negative signed ip value without breaking valid reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-06",
            &["manual-sbx-bam"],
            "manual-synthetic",
            vec![truth_locus(
                "sbx",
                "chrManual:39901-40600",
                MANUAL_CONTIG,
                39_900,
                40_600,
                &[
                    "Named records expose concordant, partly discordant, and fully discordant ten-base insertions plus a reverse-strand control.",
                    "A named boundary record carries insertion qualities Q9, Q10, and Q30 around the two classification thresholds.",
                    "A named all-tail record has no retained center and a named missing-quality record reports unavailable evidence without hiding healthy reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-07",
            &["manual-ultima-bam", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "ultima",
                "chrManual:49901-50600",
                MANUAL_CONTIG,
                49_900,
                50_600,
                &[
                    "Forward Q40 and reverse Q0 insertion records expose exact signed tp direction arrays in directional and minimum modes.",
                    "A read-group mc=1 homopolymer deletion resolves to Q0 and a single-base non-homopolymer deletion resolves from t0 to Q35.",
                    "Named missing, wrong-length, and wrong-type tp controls remain local errors without hiding the valid reads.",
                ],
            )],
        ),
        truth(
            "truth-mf-08",
            &["manual-specialist-bam", "manual-sample-metadata"],
            "manual-synthetic",
            vec![truth_locus(
                "single-cell",
                "chrManual:4901-5400",
                MANUAL_CONTIG,
                4_900,
                5_400,
                &[
                    "CELL_A-1, CELL_B-1, and CELL_C-1 records have corrected/raw cell and UMI tags.",
                    "One named record deliberately lacks cell and UMI tags.",
                    "One named record deliberately carries a numeric malformed CB tag.",
                ],
            )],
        ),
        truth(
            "truth-mf-09",
            &["manual-cohort-vcf", "manual-cohort-bcf"],
            "variants-synthetic",
            vec![truth_locus(
                "cohort-variants",
                "chrVariants:14901-18100",
                "chrVariants",
                14_900,
                18_100,
                &[
                    "The VCF and BCF contain SNV, MNV, multi-allelic, insertion, deletion, inversion, symbolic deletion, and breakend records.",
                    "All 128 samples retain phased, unphased, haploid, polyploid, missing, and partial no-call GT states plus DP, AD, GQ, and PL fields.",
                    "The final sample on EXPECTED_BREAKEND has one out-of-range allele index as a readable malformed-genotype boundary case.",
                ],
            )],
        ),
        truth(
            "truth-mf-10",
            &["dep-demo-project"],
            "variants-synthetic",
            vec![truth_locus(
                "mixed-tracks",
                "chrVariants:8501-16000",
                "chrVariants",
                8_500,
                16_000,
                &[
                    "All 19 declared auxiliary sources return non-empty production-engine evidence.",
                    "The TDF exposes two independent named channels.",
                ],
            )],
        ),
        truth(
            "truth-mf-11",
            &["manual-genome-config", "dep-variants-fasta"],
            "variants-synthetic",
            vec![truth_locus(
                "genome-review",
                "chrVariants:8501-12500",
                "chrVariants",
                8_500,
                12_500,
                &[
                    "Alias variants resolves to chrVariants.",
                    "All six translation frames, two ROIs, and two review items restore.",
                    "The chrVariants:8501-8560 reference is exact ACGT repetition, yielding stable forward/reverse codons including ACG, CGT, GTA, and TAC.",
                    "EXPECTED_SEARCH_FEATURE resolves exactly to chrVariants:8501-9200, and the import files contain valid coloured boundary ROIs and versioned review rows.",
                ],
            )],
        ),
        truth(
            "truth-mf-12",
            &["dep-mixed-bam"],
            "mixed-synthetic",
            vec![truth_locus(
                "workspace-deep",
                "chr1:900001-920000",
                "chr1",
                900_000,
                920_000,
                &[
                    "Four stable alignment tracks reopen in two ordered panels; separate eight-track and one-failure workspaces retain distinct identities.",
                    "The self-contained reference/BAM project reopens and queries after relocation to a different directory.",
                ],
            )],
        ),
        truth(
            "truth-mf-13",
            &[
                "manual-igv-import",
                "manual-igv-expected",
                "manual-recovery-autosave",
                "manual-recovery-state",
                "failure-changed-source",
                "manual-changed-source-state",
            ],
            "mixed-synthetic",
            vec![truth_locus(
                "session-recovery",
                "chr1:900001-920000",
                "chr1",
                900_000,
                920_000,
                &[
                    "The IGV import report exactly classifies imported, approximated, and ignored items.",
                    "A controlled unclean runtime state exposes exactly one valid autosave and an explicit recover-or-continue choice.",
                    "The changed-source fingerprint differs deliberately, while the partial workspace names exactly one missing BAM.",
                    "Native versions 1, 2, and 3 reopen and the deliberately future schema is rejected.",
                ],
            )],
        ),
        truth(
            "truth-mf-14",
            &["manual-reference-cram", "manual-reference-fasta"],
            "manual-synthetic",
            vec![truth_locus(
                "reference-dependent-cram",
                "chrManual:901-5400",
                MANUAL_CONTIG,
                900,
                5_400,
                &[
                    "The CRAM returns the same sixteen records (fifteen names, including one paired template) as the source BAM when opened with the matching reference.",
                    "A reference with a different MD5 is rejected rather than displaying fabricated sequence.",
                ],
            )],
        ),
    ];
    for truth in truths {
        write_json(
            &suite_root.join(format!("expected/{}.json", truth.id)),
            &truth,
        )?;
    }
    Ok(())
}

pub(crate) fn truth(
    id: &str,
    fixture_ids: &[&str],
    assembly: &str,
    loci: Vec<TruthLocus>,
) -> TruthDocument {
    TruthDocument {
        schema_version: MANUAL_SUITE_SCHEMA_VERSION,
        id: id.to_owned(),
        fixture_ids: fixture_ids
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        assembly: assembly.to_owned(),
        loci,
    }
}

pub(crate) fn truth_locus(
    id: &str,
    displayed_locus: &str,
    contig: &str,
    start: u64,
    end: u64,
    facts: &[&str],
) -> TruthLocus {
    TruthLocus {
        id: id.to_owned(),
        displayed_locus: displayed_locus.to_owned(),
        contig: contig.to_owned(),
        start_zero_based: start,
        end_zero_based_exclusive: end,
        expected_facts: facts.iter().map(|value| (*value).to_owned()).collect(),
    }
}
