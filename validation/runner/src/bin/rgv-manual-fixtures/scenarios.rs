#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn write_scenarios(
    root: &Path,
    suite_root: &Path,
    validation_root: &Path,
) -> Result<()> {
    let relative_suite = suite_root.strip_prefix(root)?.to_string_lossy();
    for scenario in SCENARIOS {
        let launch = scenario.project_id.map_or_else(
            || operational_launch(scenario.id).to_owned(),
            |project_id| {
                let project = PROJECTS
                    .iter()
                    .find(|project| project.id == project_id)
                    .expect("scenario project IDs are static and valid");
                format!(
                    "cargo run --release -p rgv-desktop -- --session {relative_suite}/projects/{}",
                    project.filename
                )
            },
        );
        let ordinal = scenario.id.trim_start_matches("scenario-mf-");
        let project_line = scenario.project_id.unwrap_or("runtime-generated");
        let mut text = format!(
            "# {}: {}\n\n- **Status:** Generated and machine-verified; manual result not run\n- **Scenario ID:** `{}`\n- **Feature group:** `MF-{}`\n- **Project:** `{}`\n- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-{}.json`\n\n## Why this exists\n\n{}\n\n## Launch\n\n```bash\n{}\n```\n\n## Expected truth\n\n",
            scenario.id,
            scenario.title,
            scenario.id,
            ordinal,
            project_line,
            ordinal,
            scenario.title,
            launch
        );
        for expected in scenario.expected {
            let _ = writeln!(text, "- {expected}");
        }
        text.push_str("\n## Manual actions\n\n");
        for (index, action) in scenario_actions(scenario.id).iter().enumerate() {
            let _ = writeln!(text, "{}. {action}", index + 1);
        }
        text.push_str("\n## Common failure signatures\n\n");
        for failure in scenario_failures(scenario.id) {
            let _ = writeln!(text, "- {failure}");
        }
        text.push_str("\n## Result\n\n```text\ncommit:\nplatform/GPU/display scaling:\nresult: not run\nnotes:\nscreenshot or issue:\n```\n");
        write_text(
            &validation_root.join(format!("scenarios/{}.md", scenario.id)),
            &text,
        )?;
    }
    Ok(())
}

pub(crate) fn write_automation_batches(validation_root: &Path) -> Result<()> {
    write_json(
        &validation_root.join("batches/mf15-complete-exports.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/mixed.bam" } }, "kind": "Alignment", "name": "Automation alignments" },
                { "command": "goto", "loci": ["chr1:100001-120000"] },
                { "command": "add_panel", "locus": "chr1:110001-130000" },
                { "command": "set_option", "scope": { "scope": "workspace" }, "key": "show_junctions", "value": true },
                { "command": "group", "track_ids": [1], "mode": "strand" },
                { "command": "sort", "track_ids": [1], "mode": "mapping_quality", "direction": "natural" },
                { "command": "filter", "track_ids": [1], "expression": "NM>=0" },
                { "command": "wait_until_settled", "timeout_millis": 30000 },
                { "command": "select", "selection": { "selection": "position", "panel_id": 1, "track_id": 1, "position": 115_000 } },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/alignments.tsv", "kind": "alignments" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/pileup.tsv", "kind": "pileup" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/selected-evidence.tsv", "kind": "selected_evidence" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/modifications.tsv", "kind": "modifications" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/junctions.tsv", "kind": "junctions" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/track-names.tsv", "kind": "track_names" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/workspace.png", "scope": "visible_workspace", "format": "png" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/full-panel.svg", "scope": "full_active_panel", "format": "svg" },
                { "command": "save", "path": "../../../benchmark-results/manual-suite/automation-project.rgv.json" },
                { "command": "exit" }
            ]
        }),
    )?;
    write_json(
        &validation_root.join("batches/mf15-variant-track-exports.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "set_genome", "source": { "Local": { "path": "../../../benchmark-data/variants.fa" } }, "name": "Manual variants genome" },
                { "command": "goto", "loci": ["chrVariants:14901-18100"] },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/data/variants/cohort.bcf" } }, "kind": "Variant", "name": "Automation variants" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/demo-tracks/signal.bedGraph" } }, "kind": "Quantitative", "name": "Automation signal" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/data/tracks/default-annotations.bed" } }, "kind": "Feature", "name": "Automation regions" },
                { "command": "wait_until_settled", "timeout_millis": 30000 },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/variants.tsv", "kind": "variants" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/quantitative.tsv", "kind": "quantitative" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/regions.tsv", "kind": "regions" },
                { "command": "export", "path": "../../../benchmark-results/manual-suite/variant-track-names.tsv", "kind": "track_names" },
                { "command": "snapshot", "path": "../../../benchmark-results/manual-suite/variant-workspace.svg", "scope": "visible_workspace", "format": "svg" },
                { "command": "save", "path": "../../../benchmark-results/manual-suite/variant-project.rgv.json" },
                { "command": "exit" }
            ]
        }),
    )?;
    write_json(
        &validation_root.join("batches/mf15-controlled-failure.json"),
        &serde_json::json!({
            "schema_version": 1,
            "commands": [
                { "command": "new_workspace" },
                { "command": "load_track", "source": { "Local": { "path": "../../../benchmark-data/manual-suite/failures/does-not-exist.bam" } }, "kind": "Alignment", "name": "EXPECTED missing source" },
                { "command": "goto", "loci": ["chr1:900001-920000"] },
                { "command": "wait_until_settled", "timeout_millis": 100 },
                { "command": "exit" }
            ]
        }),
    )
}

pub(crate) fn scenario_actions(id: &str) -> &'static [&'static str] {
    match id {
        "scenario-mf-01" => &[
            "Confirm paired, structural, deep, long-read, supplementary, modified-base, and pathological-CIGAR review loci are present.",
            "At each locus pan and zoom with the top navigator, then vertically scroll without moving reference or coverage tracks.",
            "Switch grouping, sorting, pairing, color, density, and base-display settings and inspect named reads.",
            "Use mate and supplementary-chain navigation, then return through locus history and the review queue.",
            "Save, reopen, and export the selected read/evidence; confirm track identity and settings persist.",
        ],
        "scenario-mf-02" => &[
            "Inspect each named match, SNV, small-indel, 1,000-base insertion, and 2,000-base deletion record.",
            "Zoom across base-letter, event-label, and aggregate thresholds without changing event identity or colour.",
            "Select insertion and deletion markers and compare positions, CIGAR, sequence, qualities, and pileup with truth-mf-02.",
            "Pan slightly while zoomed in and confirm reads do not jump away or restack vertically.",
            "Export the selected evidence and reopen the project to confirm the exact locus and base-display settings.",
        ],
        "scenario-mf-03" => &[
            "Compare RNA sample A and B at the shared and sample-specific junctions.",
            "Confirm annotated A-specific and unannotated B-specific links plus alternative donor/acceptor and multi-exon reads.",
            "Switch strand handling and inspect positive, negative, inferred-missing, and invalid XS evidence.",
            "Compare fixed and per-track automatic Sashimi scaling while panning and zooming both panels.",
            "Filter junction counts, select links, and export vector evidence without changing the exact support counts.",
        ],
        "scenario-mf-04" => &[
            "Inspect all six preconfigured CG, CHG, CHH, HCG, GCH, and WCG tracks.",
            "For every context confirm protected and converted observations against the matching reference.",
            "Compare forward and reverse reads, then inspect the low-quality and missing-evidence controls.",
            "Toggle pairing, grouping, and zoom thresholds without falling back to ordinary mismatch colouring.",
            "Save and reopen; confirm every track retains its distinct bisulfite context.",
        ],
        "scenario-mf-05" => &[
            "Open each of the six subread/CCS IPD and pulse-width modes.",
            "Inspect forward and reverse records and compare exact per-base values with truth-mf-05.",
            "Check missing, wrong-length, wrong-type, and malformed evidence is reported locally rather than crashing the track.",
            "Pan and zoom through thresholds and confirm kinetic colour scales do not reinterpret the same value.",
            "Save/reopen all modes and export one selected kinetic series.",
        ],
        "scenario-mf-06" => &[
            "Inspect concordant, partial, full, reverse, and all-tail SBX records.",
            "Compare Highlight, Hide, and Off simplex-tail tracks at the same reads.",
            "Check left/right tails, insertion components, and quality boundaries against truth-mf-06.",
            "Inspect missing or malformed evidence and confirm neighboring healthy reads remain visible.",
            "Save/reopen and export a selected SBX read in every tail mode.",
        ],
        "scenario-mf-07" => &[
            "Inspect the four valid and three invalid named Ultima records against the matching reference.",
            "Compare directional and minimum-evidence tracks for forward Q40/reverse Q0 tp, read-group mc Q0, and t0 Q35 calls.",
            "Check missing, wrong-length, and wrong-type tp records are reported without hiding valid evidence.",
            "Pan and zoom through base/event thresholds without changing the decoded event.",
            "Save/reopen and export the same selected event from both quality modes.",
        ],
        "scenario-mf-08" => &[
            "Group, sort, colour, and filter by RG, CB, CR, UB, and UR.",
            "Confirm repeated and distinct molecules for CELL_A-1, CELL_B-1, and CELL_C-1 remain obvious.",
            "Inspect and copy corrected/raw cell and UMI tags from named reads.",
            "Check the missing-tag and numeric malformed-CB records remain local, explicit failures.",
            "Apply bulk actions, save, reopen, and confirm tag expressions and metadata links persist.",
        ],
        "scenario-mf-09" => &[
            "Compare the same eight records in bgzip/tabix VCF and BCF/CSI tracks.",
            "Inspect SNV, MNV, multi-allelic, indel, inversion, symbolic deletion, and breakend spans.",
            "Filter and scroll 128 samples; verify pedigree order and phased, unphased, haploid, polyploid, and no-call genotypes.",
            "Inspect INFO, FILTER, GT, DP, AD, GQ, and PL fields and use next/previous variant navigation.",
            "Export selected records/genotypes and confirm the two source formats remain semantically identical.",
        ],
        "scenario-mf-10" => &[
            "Open every declared auxiliary track and inspect its named exact record.",
            "Exercise search, labels, exon/strand structure, inspectors, and next/previous feature navigation.",
            "Navigate both endpoints of BEDPE/interact records and compare SEG/CBS/MAF/GWAS behavior.",
            "Compare independent/shared signal scales, overlay groups, arithmetic output, and both TDF channels.",
            "Export SVG and tables, save/reopen, and confirm every typed format setting persists.",
        ],
        "scenario-mf-11" => &[
            "Open the portable genome and confirm reference, lengths, cytobands, aliases, and default annotation.",
            "Search coordinates, aliases, genes, and variants, including ambiguous and no-result cases.",
            "Compare forward/reverse reference and all six translation frames at known start/stop/partial codons.",
            "Import, edit, navigate, and export regions of interest and review items at their boundary coordinates.",
            "Use locus history, multiple loci, whole-contig, and previous/next navigation, then save/reopen.",
        ],
        "scenario-mf-12" => &[
            "Open the four-track, eight-track, two-panel, and failure-isolation projects in turn.",
            "Add, remove, reorder, hide, collapse, resize, and bulk-edit tracks while preserving stable identities.",
            "Group by metadata and compare linked versus independent panel navigation and reference persistence.",
            "Open failure isolation and confirm exactly one missing source does not blank the three healthy tracks.",
            "Save/reopen, relocate the portable project bundle, and compare the exported track manifest.",
        ],
        "scenario-mf-13" => &[
            "Open native schema versions 1, 2, and 3, then confirm the future version is rejected explicitly.",
            "Import the representative IGV XML and compare every imported, approximated, and ignored notice with the expected report.",
            "Open missing-source, changed-source, partial-workspace, and invalid-setting cases and confirm failures remain isolated.",
            "Materialize the controlled runtime state in a disposable settings directory and verify one autosave recovery choice appears.",
            "Recover the autosave and also test continue-without-recovery; use Save project as… to preserve only the chosen result.",
        ],
        "scenario-mf-14" => &[
            "Start the loopback server, materialize runtime projects, and compare remote sources with their local equivalents.",
            "Exercise HEAD, ranges, adjacent/custom indexes, redirects, signed queries, validators, and bearer authorization.",
            "Switch through fail-first, outage, slow, no-range, short-body, changed-ETag, corrupt, and invalid-content-type profiles.",
            "Test missing/corrupt indexes, wrong CRAM reference, cancellation, retry, and recovery while healthy panels remain usable.",
            "Stop the server and confirm runtime credentials were absent from projects, logs, reports, and workspace files.",
        ],
        "scenario-mf-15" => &[
            "Run every supplied automation batch and inspect stable command IDs, settled-state waits, results, timeouts, and errors.",
            "Exercise locus, panel, track, selection, and settings operations through authenticated local control.",
            "Generate screenshots and every claimed structured, vector, semantic, and tabular export and validate each output.",
            "Complete the common workflow using only the keyboard; check focus order, shortcuts, names, high zoom, high DPI, and reduced motion.",
            "Save/reopen and inspect logs/reports to confirm runtime credentials and sensitive query values are redacted.",
        ],
        _ => &[],
    }
}

pub(crate) fn scenario_failures(id: &str) -> &'static [&'static str] {
    match id {
        "scenario-mf-14" => &[
            "A failed remote source blanks or destabilizes healthy panels.",
            "Credentials or signed query values appear in a saved project, report, or log.",
        ],
        "scenario-mf-13" => &[
            "Compatibility is silently approximated or a future schema is accepted.",
            "Recovery overwrites the original project or offers an invalid autosave.",
        ],
        "scenario-mf-15" => &[
            "An operation reports settled before the requested state is visible.",
            "Keyboard focus becomes lost, hidden, unnamed, or trapped.",
        ],
        _ => &[
            "Named truth changes, disappears, or moves after pan/zoom/save/reopen.",
            "A malformed record crashes the app or hides unrelated healthy evidence.",
        ],
    }
}

pub(crate) fn operational_launch(id: &str) -> &'static str {
    match id {
        "scenario-mf-13" => {
            "cargo run --release -p rgv-desktop -- --session validation/multi-panel-four-track-session-v3.json"
        }
        "scenario-mf-14" => {
            "cargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy"
        }
        "scenario-mf-15" => {
            "cargo run --release -p rgv-desktop -- --batch validation/automation-smoke.json --automation-report benchmark-results/manual-suite-automation.json"
        }
        _ => "cargo run --release -p rgv-manual-fixtures -- list",
    }
}

pub(crate) fn write_validation_index(validation_root: &Path) -> Result<()> {
    let mut text = String::from(
        "# RGV manual fixture suite\n\nThe suite is fixture-ready: its hashes, portable projects, exact semantic truth, production readers, relocation, coverage catalog, and controlled failure inputs pass mechanically. Manual scenario results remain `not run` until a tester records them; synthetic readiness does not replace real-data or platform qualification.\n\n## Generate and verify\n\n```bash\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- verify --require-complete\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list\n```\n\n`cargo run` builds changed binaries automatically; a separate `cargo build` is optional. Use release mode for visual testing. The expected CRAM MD5 diagnostic during verification is the deliberate wrong-reference rejection.\n\n## Short pass\n\nRun MF-01, MF-02, MF-03, MF-09, MF-10, MF-12, and one MF-14 outage/recovery profile. This covers ordinary alignments, base events, RNA, cohorts, formats, workspaces, and isolation without attempting every specialist mode.\n\n## Remote lab\n\n```bash\ncargo build --release -p rgv-validation-runner --bins\ntarget/release/rgv-fixture-server-check --suite benchmark-data/manual-suite\ncargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy --write-endpoint /tmp/rgv-fixture-endpoint.json\ncargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- materialize-remote --endpoint /tmp/rgv-fixture-endpoint.json --output /tmp/rgv-remote-projects\n```\n\nThe endpoint document contains runtime-only credentials and must remain outside the repository.\n\n## Full pass\n\n",
    );
    for scenario in SCENARIOS {
        let _ = writeln!(
            text,
            "- [{}: {}](scenarios/{}.md)",
            scenario.id, scenario.title, scenario.id
        );
    }
    text.push_str(
        "\nSynthetic truth proves deterministic mechanics; it does not replace the representative private-technology matrix in `../MANUAL_QUALIFICATION.md`.\n",
    );
    write_text(&validation_root.join("README.md"), &text)
}

pub(crate) fn write_server_profiles(validation_root: &Path) -> Result<()> {
    for (name, status, behavior) in [
        (
            "healthy",
            206,
            "HEAD and bounded GET byte ranges succeed with stable validators",
        ),
        (
            "auth-required",
            401,
            "requests without the runtime bearer token are rejected",
        ),
        (
            "redirect",
            307,
            "the first request redirects while preserving signed query keys",
        ),
        (
            "fail-first-n",
            503,
            "the first configured number of data requests fail, then recover",
        ),
        (
            "outage",
            503,
            "all data requests fail until the profile is changed",
        ),
        (
            "slow",
            206,
            "responses are delayed by the configured bounded duration",
        ),
        (
            "no-range",
            200,
            "Range is deliberately ignored and Accept-Ranges is omitted",
        ),
        (
            "short-body",
            206,
            "the response body is shorter than its requested range",
        ),
        (
            "changed-etag",
            206,
            "the validator changes after the first data request",
        ),
        (
            "corrupt",
            206,
            "one deterministic byte in the response body is changed",
        ),
        (
            "invalid-content-type",
            206,
            "the bytes are valid but the response declares an incompatible media type",
        ),
    ] {
        write_json(
            &validation_root.join(format!("server-profiles/{name}.json")),
            &serde_json::json!({
                "schema_version": 1,
                "profile": name,
                "representative_status": status,
                "expected_behavior": behavior,
                "external_network_required": false
            }),
        )?;
    }
    write_json(
        &validation_root.join("server-profiles/remote-source-map.json"),
        &serde_json::json!({
            "schema_version": 1,
            "bam": { "source": "dep-mixed-bam", "index": "dep-mixed-bai" },
            "cram": { "source": "manual-reference-cram", "index": "manual-reference-crai", "reference": "manual-reference-fasta", "reference_index": "manual-reference-fai" },
            "reference": { "source": "dep-variants-fasta", "index": "dep-variants-fai" },
            "variants": { "source": "manual-cohort-bcf", "index": "manual-cohort-csi" },
            "bigwig": "dep-demo-signal.bigWig",
            "bigbed": "dep-demo-features.bigBed",
            "big_gene_pred": "dep-demo-genes.bigGenePred",
            "big_narrow_peak": "dep-demo-peaks.bigNarrowPeak",
            "tdf": "dep-demo-archive.tdf"
        }),
    )
}
