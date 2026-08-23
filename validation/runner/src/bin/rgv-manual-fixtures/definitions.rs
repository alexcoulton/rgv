#[derive(Clone, Copy)]
pub(crate) struct ProjectDefinition {
    pub(crate) id: &'static str,
    pub(crate) filename: &'static str,
    pub(crate) title: &'static str,
    pub(crate) purpose: &'static str,
    pub(crate) initial_locus: &'static str,
    pub(crate) scenario_id: &'static str,
}

pub(crate) const PROJECTS: &[ProjectDefinition] = &[
    ProjectDefinition {
        id: "mf-01-alignment-core",
        filename: "mf-01-alignment-core.rgv.json",
        title: "Core alignments and navigation",
        purpose: "paired, deep, structural, long-read, modification, grouping, sorting, and navigation review",
        initial_locus: "chr1:2590001-2650000",
        scenario_id: "scenario-mf-01",
    },
    ProjectDefinition {
        id: "mf-02-alignment-events",
        filename: "mf-02-alignment-events.rgv.json",
        title: "Alignment events and reference bases",
        purpose: "exact base, coverage, CIGAR, small-indel, and large-indel truth",
        initial_locus: "chrVariants:9401-12600",
        scenario_id: "scenario-mf-02",
    },
    ProjectDefinition {
        id: "mf-03-rna-sashimi",
        filename: "mf-03-rna-sashimi.rgv.json",
        title: "RNA and Sashimi",
        purpose: "spliced-read, strand-evidence, junction-count, scale, and export review",
        initial_locus: "chr1:2100001-2301000",
        scenario_id: "scenario-mf-03",
    },
    ProjectDefinition {
        id: "mf-04-bisulfite",
        filename: "mf-04-bisulfite.rgv.json",
        title: "Bisulfite contexts",
        purpose: "forward/reverse converted and protected cytosines in preselected contexts",
        initial_locus: "chrManual:901-1200",
        scenario_id: "scenario-mf-04",
    },
    ProjectDefinition {
        id: "mf-05-smrt-kinetics",
        filename: "mf-05-smrt-kinetics.rgv.json",
        title: "PacBio SMRT kinetics",
        purpose: "subread and CCS kinetics with the physical evidence mode preselected",
        initial_locus: "chr2:1-2000000",
        scenario_id: "scenario-mf-05",
    },
    ProjectDefinition {
        id: "mf-06-sbx",
        filename: "mf-06-sbx.rgv.json",
        title: "SBX insertion and tail evidence",
        purpose: "concordant/discordant insertions and simplex tail presentation",
        initial_locus: "chr2:1-2000000",
        scenario_id: "scenario-mf-06",
    },
    ProjectDefinition {
        id: "mf-07-ultima",
        filename: "mf-07-ultima.rgv.json",
        title: "Ultima flow-indel evidence",
        purpose: "directional and minimum Ultima evidence with a matching reference",
        initial_locus: "chrManual:49901-50600",
        scenario_id: "scenario-mf-07",
    },
    ProjectDefinition {
        id: "mf-08-single-cell",
        filename: "mf-08-single-cell.rgv.json",
        title: "Tagged single-cell alignments",
        purpose: "RG/CB/CR/UB/UR grouping, sorting, filtering, metadata, and persistence",
        initial_locus: "chrManual:4901-5400",
        scenario_id: "scenario-mf-08",
    },
    ProjectDefinition {
        id: "mf-09-cohort-variants",
        filename: "mf-09-cohort-variants.rgv.json",
        title: "Cohort variants and genotypes",
        purpose: "typed SNV/MNV/indel/SV/BND evidence, genotypes, samples, filters, and export",
        initial_locus: "chrVariants:14901-18100",
        scenario_id: "scenario-mf-09",
    },
    ProjectDefinition {
        id: "mf-10-mixed-tracks",
        filename: "mf-10-mixed-tracks.rgv.json",
        title: "Mixed auxiliary tracks",
        purpose: "all supported auxiliary formats, typed settings, overlays, arithmetic, and interaction",
        initial_locus: "chrVariants:8501-16000",
        scenario_id: "scenario-mf-10",
    },
    ProjectDefinition {
        id: "mf-11-genome-review",
        filename: "mf-11-genome-review.rgv.json",
        title: "Genome, search, translation, ROI, and review",
        purpose: "portable genome semantics, aliases, six-frame translation, ROI, and review workflow",
        initial_locus: "chrVariants:8501-12500",
        scenario_id: "scenario-mf-11",
    },
    ProjectDefinition {
        id: "mf-12-workspaces",
        filename: "mf-12-workspaces.rgv.json",
        title: "Multi-track and multi-panel workspace",
        purpose: "track identity, bulk actions, linked/independent panels, failure isolation, and relocation",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-14-reference-cram",
        filename: "mf-14-reference-cram.rgv.json",
        title: "Reference-dependent CRAM",
        purpose: "local CRAM decoding with matching and deliberately wrong reference behavior",
        initial_locus: "chrManual:901-5400",
        scenario_id: "scenario-mf-14",
    },
    ProjectDefinition {
        id: "mf-12-four-track",
        filename: "mf-12-four-track.rgv.json",
        title: "Four-track alignment workspace",
        purpose: "compact multi-sample identity, bulk-operation, and persistence review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-12-eight-track",
        filename: "mf-12-eight-track.rgv.json",
        title: "Eight-track alignment workspace",
        purpose: "larger multi-sample scrolling, ordering, metadata, and persistence review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-12-failure-isolation",
        filename: "mf-12-failure-isolation.rgv.json",
        title: "Workspace source failure isolation",
        purpose: "one deliberately missing alignment source alongside healthy tracks and panels",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-12",
    },
    ProjectDefinition {
        id: "mf-13-recovery",
        filename: "mf-13-recovery.rgv.json",
        title: "Sessions, imports, and recovery",
        purpose: "native/IGV compatibility, autosave discovery, partial workspace, and recovery review",
        initial_locus: "chr1:900001-920000",
        scenario_id: "scenario-mf-13",
    },
];

#[derive(Clone, Copy)]
pub(crate) struct ScenarioDefinition {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) project_id: Option<&'static str>,
    pub(crate) expected: &'static [&'static str],
}

pub(crate) const SCENARIOS: &[ScenarioDefinition] = &[
    scenario(
        "scenario-mf-01",
        "Core alignments and navigation",
        Some("mf-01-alignment-core"),
        &[
            "Structural pair classes are visible and inspectable.",
            "Horizontal navigation does not move the read stack vertically.",
            "Reference and coverage stay fixed while the read stack scrolls.",
        ],
    ),
    scenario(
        "scenario-mf-02",
        "Alignment events and reference bases",
        Some("mf-02-alignment-events"),
        &[
            "Small and large insertions/deletions remain anchored at their named positions.",
            "Reference and read letters remain legible and use stable event colours.",
            "I1000 denotes a 1,000-base insertion between reference bases.",
        ],
    ),
    scenario(
        "scenario-mf-03",
        "RNA and Sashimi",
        Some("mf-03-rna-sashimi"),
        &[
            "Sample A and B show exact shared, sample-specific, alternative, and multi-exon junction counts.",
            "Annotated and unannotated junctions plus valid, inferred, and invalid strand evidence remain distinct.",
        ],
    ),
    scenario(
        "scenario-mf-04",
        "Bisulfite contexts",
        Some("mf-04-bisulfite"),
        &[
            "The initial track is already in CG bisulfite mode.",
            "Converted and protected reads are distinguishable on both strands.",
        ],
    ),
    scenario(
        "scenario-mf-05",
        "PacBio SMRT kinetics",
        Some("mf-05-smrt-kinetics"),
        &[
            "The initial track is already in subread IPD mode.",
            "Kinetic values remain attached to bases through zoom and reverse orientation.",
        ],
    ),
    scenario(
        "scenario-mf-06",
        "SBX evidence",
        Some("mf-06-sbx"),
        &[
            "The initial track is already in SBX mode.",
            "Insertion categories and highlighted tails remain distinguishable.",
        ],
    ),
    scenario(
        "scenario-mf-07",
        "Ultima evidence",
        Some("mf-07-ultima"),
        &[
            "The initial track is already in directional Ultima mode.",
            "Valid forward/reverse tp, read-group mc, and t0 evidence plus three explicit error controls remain distinguishable.",
        ],
    ),
    scenario(
        "scenario-mf-08",
        "Tagged single-cell alignments",
        Some("mf-08-single-cell"),
        &[
            "Tracks are grouped by corrected cell barcode at startup.",
            "CB/CR/UB/UR tags and linked sample metadata are visible in inspection.",
        ],
    ),
    scenario(
        "scenario-mf-09",
        "Cohort variants and genotypes",
        Some("mf-09-cohort-variants"),
        &[
            "SNV, multi-allelic, insertion, deletion, inversion, symbolic deletion, and BND records are present.",
            "All 128 samples retain phased, unphased, missing, partial no-call, and DP fields.",
        ],
    ),
    scenario(
        "scenario-mf-10",
        "Mixed auxiliary tracks",
        Some("mf-10-mixed-tracks"),
        &[
            "Every supported generated auxiliary source queries non-empty evidence.",
            "Both TDF channels open independently and retain channel provenance.",
        ],
    ),
    scenario(
        "scenario-mf-11",
        "Genome and review",
        Some("mf-11-genome-review"),
        &[
            "Aliases, six translation frames, ROI entries, and review items restore.",
            "The portable genome bundle resolves its relative reference, cytobands, and annotation.",
        ],
    ),
    scenario(
        "scenario-mf-12",
        "Workspaces",
        Some("mf-12-workspaces"),
        &[
            "Four alignment tracks retain stable IDs across two panels.",
            "The intentionally missing source fails locally while healthy tracks remain usable.",
        ],
    ),
    scenario(
        "scenario-mf-13",
        "Sessions, IGV import, and recovery",
        Some("mf-13-recovery"),
        &[
            "Native schema versions migrate or reject exactly as declared.",
            "IGV import reports exact, approximated, and ignored items explicitly.",
        ],
    ),
    scenario(
        "scenario-mf-14",
        "Remote sources and failure isolation",
        None,
        &[
            "Healthy range requests are bounded and semantically match local sources.",
            "Auth, outage, corruption, validator, and retry profiles fail or recover deterministically.",
        ],
    ),
    scenario(
        "scenario-mf-15",
        "Automation, export, and accessibility",
        None,
        &[
            "Automation reports each command with stable identity and status.",
            "Keyboard focus and accessible names expose every common workflow.",
        ],
    ),
];

pub(crate) const fn scenario(
    id: &'static str,
    title: &'static str,
    project_id: Option<&'static str>,
    expected: &'static [&'static str],
) -> ScenarioDefinition {
    ScenarioDefinition {
        id,
        title,
        project_id,
        expected,
    }
}
