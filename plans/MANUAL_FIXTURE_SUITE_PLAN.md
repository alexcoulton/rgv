# RGV Complete Manual Fixture Suite Implementation Plan

**Status:** Implemented and mechanically qualified; manual scenario results not yet run
**Date:** 23 August 2026
**Purpose:** Make every implemented, user-visible RGV Desktop capability straightforward to test by hand with deterministic, ready-to-open data.

## 1. Outcome

Build a complete manual fixture suite with these properties:

- A tester can generate, verify, discover, and open the entire suite from a clean checkout without finding their own data.
- Every advertised desktop capability in `docs/DESKTOP_FEATURE_ACCESS.md` and every supported format in `docs/FORMAT_SUPPORT.md` maps to at least one named fixture scenario.
- Each successful scenario has known truth: exact loci, values, counts, labels, tags, CIGAR events, genotypes, or saved state that should be observed.
- Each error scenario produces one intentional, documented failure while leaving unrelated tracks and panels usable.
- Specialist views open with the relevant mode already enabled. A tester should not have to reverse-engineer settings just to reveal the evidence being tested.
- Projects use portable relative paths and reopen from a relocated copy of the suite.
- One command verifies the fixture files through RGV's production readers and project/session loaders before manual testing begins.

The suite is a prerequisite for broad manual qualification. Completing it does not itself prove that RGV works well on arbitrary real data.

## 2. Scope and boundaries

### In scope

- Local and loopback-remote alignment, reference, variant, annotation, quantitative, interaction, copy-number, mutation, and association data.
- Short-read, paired-end, long-read, spliced, tagged single-cell, bisulfite, PacBio kinetics, SBX, and Ultima alignment evidence.
- Focused projects for navigation, display modes, interaction, inspection, export, review, recovery, and project portability.
- Valid, boundary, missing, malformed, truncated, stale, and unsupported inputs.
- Representative project/session imports, including IGV XML behavior that RGV claims to support.
- Repeatable automation batches for opening projects, waiting for settled state, and exporting evidence.
- Scenario cards for workflows that require human judgment, keyboard use, accessibility inspection, window management, or a deliberate service interruption.

### Out of scope

- Private customer or clinical data.
- Production credentials and external network services.
- Passing the separate real-world data matrix in `validation/MANUAL_QUALIFICATION.md`.
- Performance targets, soak testing, code signing, packaging, and cross-platform release qualification except where the suite supplies a common workload.
- Features explicitly deferred by the active product roadmap.

Synthetic fixtures test deterministic mechanics and semantics. Real Illumina, ONT, PacBio, single-cell, bisulfite, and large-cohort data must still be tested later because real files contain distributions and producer-specific edge cases that a generator will not anticipate.

### Current baseline

The repository already contains a strong starting point, which this plan extends rather than discards:

- `benchmark-data/mixed.bam` and its BAI cover the large mixed alignment corpus.
- `benchmark-data/variants.bam`, its BAI, and `variants.fa`/FAI cover base-level events and large indels.
- `benchmark-data/demo-tracks` contains 19 auxiliary sources with a checksum manifest.
- `rgv-demo-project.json` and `rgv-alignment-review-project.json` provide mixed-track and alignment-review handoff workspaces.
- `validation` contains native session compatibility examples, multi-track/multi-panel sessions, and automation/workspace smoke batches.
- Existing source tests already synthesize useful CRAM, remote, specialist-tag, and malformed cases that can be promoted into stable manual fixtures.

The principal gaps are specialist projects with their modes already selected, tagged single-cell and bisulfite data, checked-in reference-dependent CRAM, richer cohort/pedigree truth, genome/metadata/import artifacts, complete negative fixtures, and a user-operable remote failure lab. Existing demos also need exact scenario-by-scenario expected observations rather than only a successful non-empty query.

## 3. Definition of complete

A feature is not manually fixture-covered merely because a unit test exists, a parser recognizes its extension, or one generic demo project can load the file. It is covered only when all applicable items below exist:

1. A stable fixture ID and mapping to an advertised feature or supported format.
2. A ready-to-open project, import file, command, or controlled failure profile.
3. A small set of exact loci or actions that expose the behavior.
4. Expected semantic truth recorded independently of the renderer.
5. An automated preflight that exercises the production reader, query engine, project/session loader, and layout path needed by the scenario.
6. A concise manual scenario card with expected observations and recognizable failure symptoms.
7. Portable paths and a clean-checkout generation/reopen test.
8. A deterministic content hash and a declared size.

The suite is complete when every in-scope feature and format has one of the following explicit dispositions in the coverage catalog:

- `ready`: a complete success scenario exists;
- `failure-ready`: the capability is an error/recovery behavior and has a controlled failure scenario;
- `operational-ready`: it needs no unique biological data but has a documented workflow using a named common fixture;
- `real-data-only`: permitted only when the roadmap explicitly requires a non-redistributable or non-synthetic input, with the reason recorded;
- `deferred`: permitted only when the product roadmap also defers the feature.

There must be no blank, implicit, or `covered by tests` dispositions.

## 4. Suite invariants

- Generation is deterministic. Running it twice with the same generator version produces the same declared files and SHA-256 hashes.
- Generation is idempotent and fails on stale undeclared output instead of silently retaining it.
- Large existing fixtures are referenced rather than duplicated.
- Generated records carry obvious names such as `EXPECTED_SNV_AT_1000250` so inspection remains understandable.
- Coordinates are recorded both as file-native coordinates and as the one-based inclusive locus displayed to users.
- Verification checks exact expected facts, not merely non-empty query results.
- The verifier uses production RGV sources wherever possible. A separate lightweight decoder may check that the generator wrote the intended truth, but it may not replace production-reader verification.
- Projects contain relative paths. Remote projects derive their base URL from a generated runtime endpoint file rather than committing a machine-specific port.
- No fixture contains secrets, personal data, absolute developer paths, or time-dependent signed URLs.
- Error scenarios operate on disposable generated copies, never on the canonical valid fixture.
- A visual screenshot may help a scenario card, but pixel equality is not the sole semantic oracle.
- The added checked-in corpus should remain small enough for ordinary source control. The initial target is no more than 50 MiB beyond the current benchmark corpus unless a larger addition is reviewed and justified.

## 5. Proposed repository layout

```text
benchmark-data/manual-suite/
  manifest.json
  data/
    alignments/
    references/
    variants/
    tracks/
  genomes/
  metadata/
  imports/
  projects/
  expected/
  failures/

validation/manual-suite/
  README.md
  coverage-catalog.json
  scenarios/
  batches/
  server-profiles/

validation/runner/src/bin/
  rgv-manual-fixtures.rs
  rgv-manual-fixture-check.rs
  rgv-fixture-server.rs
```

`benchmark-data/manual-suite` contains portable data artifacts and projects. `validation/manual-suite` contains human instructions, feature mappings, automation inputs, and server behavior definitions. The validation runner owns generation and verification so fixture creation remains Rust-native and checked by the workspace.

## 6. Manifest and coverage catalog

### 6.1 Fixture manifest

Every generated or checked-in artifact must be declared with:

- stable fixture ID;
- relative path;
- media/format type;
- generator schema version;
- byte length and SHA-256;
- source fixture IDs on which it depends;
- reference assembly and contig dictionary where relevant;
- exact truth record reference;
- projects and scenarios that consume it;
- whether it is valid, intentionally malformed, or runtime-generated;
- redistributability and privacy classification.

### 6.2 Capability coverage catalog

The catalog is the authoritative join between product claims and fixtures. Each row contains:

- stable feature ID;
- wording and source section from `DESKTOP_FEATURE_ACCESS.md` or `FORMAT_SUPPORT.md`;
- disposition;
- fixture, project, scenario, and automation batch IDs;
- expected-truth IDs;
- preflight status;
- manual qualification status;
- real-data follow-up, if any.

The verifier must fail when an advertised capability or format is absent from this catalog, when a referenced fixture does not exist, or when a `ready` row lacks expected truth and a scenario card. A reviewed catalog update is therefore required whenever feature documentation changes.

## 7. Fixture project catalog

The suite should use focused projects instead of one enormous workspace. A project may reuse the same underlying file while selecting different loci or track settings.

### MF-01: Core alignments and navigation

Purpose: exercise the common BAM experience for short, paired, long, supplementary, deep, sparse, and structurally unusual reads.

Include:

- paired reads with inward, outward, same-strand, small-insert, and large-insert orientations;
- unmapped mates, mate on another contig, secondary and supplementary records;
- long reads spanning the viewport and reads with long CIGAR strings;
- duplicate, QC-fail, low-map-quality, missing-quality, and read-group cases;
- dense and sparse loci with deterministic stacking pressure;
- RG and general-purpose numeric/string tags for grouping, sorting, coloring, filtering, and inspection;
- BAMs indexed by BAI and CSI so both local index paths are exercised;
- ordinary modified-base `MM`/`ML` evidence with multiple codes, strands, probabilities, missing values, and malformed values.

Preconfigure separate tracks or panels for packed, squished, expanded, grouped, sorted, paired, density, strand-color, pair-orientation, insert-size, and tag-color views. Add review items for mate navigation, supplementary navigation, modification inspection, copy details, read inspection, horizontal zoom, vertical zoom, and stable scrolling.

### MF-02: Alignment events and reference bases

Purpose: make every supported base-level alignment event visible against a matching FASTA.

Include exact loci for:

- matches, SNVs, MNVs, ambiguous bases, and base-quality boundaries;
- `M`, `=`, and `X` match/mismatch representations;
- insertions of 1, 2, 10, 100, and at least 1,000 bases;
- deletions of 1, 2, 10, 100, and at least 1,000 bases;
- soft clips, hard clips, reference skips, padding, and adjacent mixed events;
- reverse-strand reads and overlapping pairs;
- coverage counts, mismatch fractions, deletion counts, and insertion markers with known totals.

The truth file must state the reference sequence, CIGAR, read sequence, displayed event label, consensus, and expected coverage values at each locus. This project is the main fixture for base-letter legibility, stable event coloring, picking, tooltips, and zoom transitions.

### MF-03: RNA and Sashimi

Purpose: test spliced alignments, junction aggregation, transcript context, and Sashimi controls.

Include at least two RNA alignment tracks containing:

- shared and sample-specific junctions;
- annotated and unannotated splice junctions;
- positive, negative, inferred, missing, and malformed strand tags;
- alternative donor and acceptor events;
- retained intron evidence and multi-exon reads;
- known junction-support counts over a range suitable for filtering and scaling.

Open with transcript annotation and separate Sashimi configurations. Cover link geometry, strand modes, minimum count, shared versus independent scale, panel movement, selection, and vector export.

### MF-04: Bisulfite evidence

Purpose: expose supported methylation contexts with a matching reference rather than relying on ordinary mismatch coloring.

Include forward and reverse reads with known converted/protected bases in CG, CHG, CHH, HCG, GCH, and WCG contexts, plus paired orientation cases, low-quality calls, missing evidence, and malformed values. Use separate preconfigured tracks or projects when modes cannot coexist on one track.

Expected truth must distinguish reference context, read strand, observed base, methylation classification, and displayed count/color.

### MF-05: PacBio kinetics

Purpose: test SMRT subread and CCS kinetic modes.

Include:

- subread `ip` and `pw` tags;
- CCS `fi`, `ri`, `fp`, and `rp` tags;
- forward and reverse records;
- varying kinetic values and quality thresholds;
- absent, wrong-length, wrong-type, and malformed tags.

Projects must open with the relevant SMRT mode already selected and have exact expected per-base kinetic values.

### MF-06: SBX evidence

Purpose: test SBX insertion and tail semantics.

Include concordant, partial, and full insertions; left and right tails; reverse reads; all-tail reads; hidden/highlighted tails; quality boundaries; and missing or malformed evidence. Expected truth must name each visible insertion/tail component and its state under every preconfigured mode.

### MF-07: Ultima evidence

Purpose: test directional and minimum-evidence Ultima display paths.

Include known `tp`, `t0`, and `mc` values, forward and reverse reads, insertions, deletions, quality boundaries, unavailable evidence, and malformed tags. Use a matching reference and preconfigured tracks for each supported mode.

### MF-08: Tagged single-cell alignments

Purpose: make cell and molecule tag workflows testable rather than merely accepting arbitrary BAM tags.

Include multiple samples, read groups, cells, raw/corrected barcodes, raw/corrected UMIs, repeated molecules, distinct molecules at the same locus, missing tags, malformed tags, and enough records to make grouping/filtering obvious. Use `RG`, `CB`, `CR`, `UB`, and `UR` consistently and link sample metadata.

Cover group, sort, color, filter, inspector, copy, bulk track operations, and save/reopen of those settings.

### MF-09: Cohort variants and genotypes

Purpose: exercise VCF, bgzip/tabix VCF, BCF/CSI, variant rendering, genotype tables, pedigree ordering, and exports.

Include:

- SNVs, MNVs, multi-allelic records, insertions, deletions, inversions, symbolic SVs, and breakends;
- phased, unphased, haploid, polyploid, no-call, partial no-call, and malformed genotypes;
- INFO flags and scalar/list values, FILTER states, FORMAT fields, allele depths, quality, and likelihoods;
- a small pedigree with affected/unaffected parents and child;
- a generated large sample list for scrolling, filtering, ordering, and performance sanity, without creating a huge biological file;
- the same core records in compressed VCF and BCF so their displayed truth can be compared.

Expected truth must cover allele display, genomic span, genotype classification, sample order, inheritance observation, selection, inspector fields, next/previous feature navigation, and tabular/vector export.

### MF-10: Mixed auxiliary tracks

Purpose: cover every non-alignment format and cross-track interaction using and extending `benchmark-data/demo-tracks`.

Include at least one meaningful, indexed/queryable example of:

- BED, BED12, GFF3, GTF, genePred, refFlat, BigBed, and bigGenePred;
- narrowPeak, broadPeak, bigNarrowPeak, and PSL;
- bedGraph, WIG, BigWig, and a multi-channel TDF;
- BEDPE and interact;
- SEG, CBS, and mutation MAF;
- GWAS;
- VCF and BCF where comparison with the variant project is useful.

Include plain and gzip text variants where both are supported, and cover the documented `.linear`, `.logistic`, `.assoc`, `.qassoc`, and `.gwas` association-header families. The projects must demonstrate feature labels, strand and exon structure, inspectors, feature search, next/previous navigation, interaction endpoint navigation, copy-number and mutation sort/group behavior, shared and independent signal scaling, arithmetic tracks, overlay tracks, TDF channel choice, and SVG/tabular export. Each format needs at least one exact expected record at a named locus; loading alone is insufficient.

### MF-11: Genome, search, translation, ROI, and review

Purpose: test genome-level behavior independently of alignment scrolling.

Include:

- a portable `.rgvgenome` bundle;
- contig aliases, lengths, cytobands, default annotations, and a matching reference;
- forward and reverse reference display;
- known six-frame translation products, including start/stop and partial codons;
- searchable genes, variants, coordinates, aliases, and ambiguous/no-result queries;
- importable ROI and review-item files with labels, colors, notes, and boundary coordinates.

Cover locus history, multiple loci, whole-contig navigation, search result selection, next/previous feature, ROI editing, review queue progression, and import/export round trips.

### MF-12: Multi-track and multi-panel workspaces

Purpose: test the application as a workspace, not just as an individual track renderer.

Provide four-track and eight-track alignment projects and at least one mixed project with two or more panels. Include distinct samples and metadata so track identity remains obvious.

Cover add/remove/reorder, visibility, collapse, bulk operations, metadata grouping, panel-specific loci, linked and independent navigation, reference persistence, one failing source among healthy sources, project save/reopen, relative-path relocation, and exported track manifests.

### MF-13: Sessions, imports, and recovery

Purpose: exercise compatibility, approximation, rejection, autosave, and recovery deliberately.

Include:

- the existing valid native session schema versions and a deliberately future version;
- a representative IGV XML session with items classified in advance as imported exactly, imported approximately, ignored with warning, or rejected;
- moved projects, missing sources, changed sources, partial workspaces, and invalid settings;
- a controlled unclean-shutdown/autosave state created in a disposable runtime directory.

Expected truth must state the exact imported panels, tracks, loci, settings, warnings, and recovery choices. RGV must never silently claim exact compatibility when it approximates or ignores an IGV setting.

### MF-14: Remote sources and failure isolation

Purpose: make HTTP range access, authentication, indexing, caching, retry, and error isolation deterministic without an external service.

Provide both local and remote projects for reference-dependent CRAM. Serve BAM/BAI or CSI, CRAM/CRAI, FASTA/FAI, VCF/TBI, BCF/CSI, BigWig, BigBed, bigGenePred, bigNarrowPeak, TDF, and representative text tracks through the loopback fixture server. Exercise adjacent indexes, custom index URLs, redirects, bearer authentication, signed query preservation, validators, retries, interruption, and recovery.

Failure cases must include missing index, corrupt index, wrong reference, no range support, truncated response, corrupt body, transient server errors, permanent outage, unauthorized response, changed ETag/object, invalid content type, and a slow response that remains cancellable. One broken source must not blank or destabilize healthy panels.

### MF-15: Automation, export, and accessibility workflows

Purpose: provide a common, known workspace for operational features that do not require unique biological input.

Add automation batches and scenario cards for:

- local control and bearer-token authorization;
- locus, panel, track, and settings operations;
- settled-state waits and timeout/error results;
- screenshots and every structured/vector/tabular export claimed by the desktop;
- keyboard-only navigation, focus order, shortcuts, high zoom, high-DPI text, screen-reader naming, and reduced-motion behavior where supported.

The automation server must use runtime-generated credentials and redact them from logs and saved projects.

## 8. Controlled loopback fixture server

Implement `rgv-fixture-server` as a small validation binary, not as an embedded desktop-only test hook. It should:

- bind to loopback on an ephemeral port;
- serve only the declared manual-suite root;
- implement `HEAD`, byte ranges, conditional requests, redirects, and configurable response headers;
- generate a runtime endpoint document containing the base URL and short-lived test credential;
- preserve and log path/query information after redacting credentials;
- support deterministic profiles such as `healthy`, `auth-required`, `redirect`, `fail-first-n`, `outage`, `slow`, `no-range`, `short-body`, `changed-etag`, and `corrupt`;
- permit state changes through an explicit local control command or profile file so a scenario can fail and recover during one desktop session;
- terminate cleanly and leave bounded logs;
- reject access outside the fixture root.

This server proves deterministic client behavior. It does not replace later testing against a real WAN, proxy, object store, or institution-specific authentication setup.

## 9. Expected truth model

Each biological fixture receives a machine-readable truth file. The common structure should contain:

- assembly and contig definitions;
- named loci and displayed one-based coordinates;
- source records and their stable names;
- expected query membership and ordering;
- expected layout primitives or semantic events;
- expected aggregate counts and ranges;
- expected tags, genotypes, annotations, and inspector values;
- expected warnings and intentional errors;
- allowed variation, only where rendering legitimately depends on viewport size or platform text metrics.

Truth should be expressed semantically. For example, assert that a 1,000-base insertion marker exists between two named reference positions with label `I1000`; do not assert only that a particular rectangle is purple at fixed pixels.

## 10. Manual scenario cards

Every project or operational scenario receives a short Markdown card containing:

1. Fixture and feature IDs.
2. A copyable release-mode launch command.
3. The reason for the scenario.
4. Preconditions and any loopback-server profile.
5. The initial panel, locus, track, and preselected display mode.
6. Five to fifteen numbered actions.
7. An exact expected result after each meaningful action.
8. Common failure signatures and what evidence to capture.
9. A result block for platform, commit, pass/fail, notes, screenshot, and issue link.

`validation/manual-suite/README.md` becomes the tester's index. It provides a short recommended pass for routine development and a full pass for release qualification, both using the same fixture IDs.

## 11. Commands and developer workflow

The intended interface is:

```bash
# Generate or refresh declared fixture artifacts.
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate

# Verify hashes, exact truth, projects, sessions, sources, and coverage mappings.
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- verify

# List focused projects and copyable launch commands.
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list

# Start the controlled remote-source lab when a scenario requires it.
cargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- \
  --suite benchmark-data/manual-suite --profile healthy
```

The project list should display the exact `cargo run --release -p rgv-desktop -- ...` command for each scenario. A tester may use `cargo run`; a separate `cargo build` is not required because Cargo builds a changed binary automatically. Release mode remains the default for manual rendering checks so debug-build overhead is not mistaken for product behavior.

## 12. Implementation phases

### Phase 0: Freeze the coverage contract

- Assign stable IDs to all advertised desktop features and supported formats.
- Create `coverage-catalog.json` and map current fixtures, projects, batches, and sessions.
- Record every gap as a planned fixture or an explicitly justified non-fixture disposition.
- Add a checker that detects undocumented features/formats and broken catalog references.

Gate: every in-scope product claim has a non-blank catalog row, and every existing fixture has a declared owner and purpose.

### Phase 1: Build generator and verifier infrastructure

- Add deterministic file generation, manifest writing, SHA-256 calculation, and stale-output detection.
- Add schema-versioned expected-truth files.
- Add portable project-path construction and relocation verification.
- Add `generate`, `verify`, and `list` commands with clear diagnostics.
- Reuse the current synthetic generator logic where appropriate rather than maintaining two competing fixture systems.

Gate: two clean generations have identical declared tree hashes; tampering, stale output, missing output, absolute paths, and undeclared files are detected.

### Phase 2: Complete core alignment fixtures

- Implement MF-01 and MF-02.
- Reuse `mixed.bam` and `variants.bam` when their content already satisfies exact truth; generate focused additions only for missing events.
- Add preconfigured projects, expected query/layout truth, scenario cards, and settled-state export batches.

Gate: every alignment event, stacking/navigation action, coverage value, and base-level inspector claim in these projects is verified through production code before the projects are marked `ready`.

### Phase 3: Complete RNA and specialist alignment modes

- Implement MF-03 through MF-08: RNA, bisulfite, SMRT, SBX, Ultima, and tagged single-cell.
- Generate a matching reference for every reference-dependent mode.
- Place incompatible display modes on separate tracks or in separate projects.
- Cover valid, missing, and malformed evidence without allowing one bad record to break the track.

Gate: each specialist project opens directly in the advertised mode, exposes named evidence at its initial locus, and passes exact semantic verification and project reopen.

### Phase 4: Complete variants, formats, genomes, and metadata

- Implement MF-09 through MF-11.
- Extend the current demo tracks where semantic coverage is missing, especially multi-channel TDF, refFlat, GFF3, and arithmetic/overlay inputs.
- Add cohort metadata, pedigree, aliases, cytobands, portable genome bundle, ROI/review imports, and exact search/translation truth.

Gate: every format in `FORMAT_SUPPORT.md` is queried through its production source and has at least one exact expected observation in a ready project.

### Phase 5: Complete workspaces, compatibility, and recovery

- Implement MF-12 and MF-13.
- Extend the existing four-track and multi-panel sessions into ready manual projects.
- Add representative IGV XML import truth, relative-path relocation, missing/changed source behavior, autosave, and controlled recovery state.

Gate: save/reopen/relocate round trips preserve declared state; version rejection and approximated imports produce the exact documented diagnostics; a failing source remains isolated.

### Phase 6: Complete remote and operational scenarios

- Implement MF-14 and MF-15 plus the loopback fixture server.
- Add remote projects that are materialized from the runtime endpoint document.
- Add deterministic authentication, retry, outage, cancellation, validator, corruption, and recovery cases.
- Add export, local-control, keyboard, accessibility, high-DPI, and focus scenario cards using common projects.

Gate: every remote/failure profile is repeatable, credentials are absent from committed artifacts and logs, and all operational workflows have a named common fixture and exact success/error result.

### Phase 7: Close the suite and prepare manual handoff

- Run all mechanical gates from a clean checkout.
- Generate the short-pass and full-pass indexes.
- Add fixture IDs and exact launch commands to `validation/MANUAL_QUALIFICATION.md`.
- Update the beta execution plan and parity roadmap so they accurately distinguish fixture readiness from manual qualification completion.
- Commit the complete fixture suite and its generated manifest together.

Gate: the coverage catalog contains no unresolved in-scope rows and the full suite can be generated, verified, relocated, opened, and sampled without developer-specific knowledge.

## 13. Mechanical verification gates

Before broad manual testing, all of the following must pass:

- `cargo fmt --all -- --check`;
- workspace tests, checks, and existing synthetic-corpus validation;
- release builds of the desktop and validation binaries;
- generation twice with identical manifest and tree hash;
- verification of every declared byte length and SHA-256;
- rejection of missing, stale, mutated, and undeclared fixture output;
- zero unresolved sources in valid projects after relocation;
- exact queries through BAM/CRAM, reference, variant, annotation, signal, interaction, copy-number, mutation, and association production sources;
- exact project/session/import state after save and reopen;
- layout and picking checks for specialist modes at their named loci;
- successful settled-state batch and declared exports for every ready project;
- typed, expected failures for every invalid case, with healthy tracks still responsive;
- all advertised features and supported formats mapped in the coverage catalog;
- credential, personal-data, and absolute-path scan;
- corpus-size report within the reviewed budget;
- path and line-ending checks suitable for macOS, Linux, and Windows checkouts;
- at least one native GPU smoke of the fixture index on the current development platform.

## 14. Manual handoff criteria

The suite is ready to hand to the user for broad manual testing only when:

- every coverage-catalog row is `ready`, `failure-ready`, `operational-ready`, or explicitly roadmap-deferred;
- every ready biological feature can be reached from a focused project with a single copyable command;
- specialist projects start with the relevant evidence and mode visible;
- expected truth and scenario instructions are written in plain language;
- the release desktop opens every valid project without unresolved local paths;
- a relocated copy of the suite reopens successfully;
- the loopback server can run all remote profiles without external network access;
- full mechanical verification passes from a clean checkout;
- the manual qualification document names remaining real-world datasets separately instead of implying that synthetic coverage replaces them.

Only then should the broad manual pass begin. Findings from that pass may require fixture corrections, product fixes, or additional real-world data, but the tester should no longer be blocked by missing example data or hidden setup.

## 15. Risks and controls

| Risk | Control |
|---|---|
| One giant project is slow and difficult to understand | Use small, purpose-built projects with a separate workspace stress project. |
| Large binaries bloat the repository | Reuse existing BAMs, generate focused small inputs, report sizes, and require review above the budget. |
| The generator and verifier agree on the same mistake | Record independently reasoned truth and verify through production sources as well as file-level decoders. |
| Specialist modes are technically present but undiscoverable | Save each project with the mode enabled and evidence centered at startup. |
| Synthetic data creates false confidence | Keep the later real-world technology/platform matrix as a separate mandatory qualification step. |
| Remote tests become flaky | Use deterministic loopback profiles; reserve real WAN testing for later qualification. |
| Invalid fixtures damage valid files | Materialize corrupt/truncated cases in a disposable generated area and verify canonical hashes afterward. |
| Projects work only on one developer's machine | Forbid absolute paths and require relocation verification. |
| Fixture binaries drift away from generator truth | Version manifests and fail regeneration/verification on any undeclared drift. |
| The manual suite becomes overwhelming | Provide short and full passes, a recommended order, and focused scenario cards. |
| Screenshots hide semantic errors | Base success on exact records, counts, tags, and state; use screenshots only as supporting evidence. |

## 16. Deliverables checklist

- [x] Stable feature and format coverage catalog.
- [x] Schema-versioned manifest and expected-truth model.
- [x] Deterministic Rust fixture generator.
- [x] Production-path fixture verifier.
- [x] Fixture/project listing command.
- [x] Core alignment and event projects.
- [x] RNA and Sashimi project.
- [x] Bisulfite project.
- [x] PacBio kinetics project.
- [x] SBX project.
- [x] Ultima project.
- [x] Tagged single-cell project.
- [x] Cohort variant, genotype, and pedigree project.
- [x] Complete mixed-format auxiliary-track projects.
- [x] Portable genome, search, translation, ROI, and review project.
- [x] Multi-track and multi-panel workspace projects.
- [x] Native/IGV import, compatibility, relocation, autosave, and recovery scenarios.
- [x] CRAM with matching and wrong-reference scenarios.
- [x] Controlled loopback fixture server and remote projects.
- [x] Malformed, truncated, missing, stale, unauthorized, outage, and recovery scenarios.
- [x] Automation/export/accessibility scenario cards and batches.
- [x] Short-pass and full-pass tester indexes.
- [x] Mechanical clean-checkout and relocation qualification.
- [x] Updated manual qualification, beta execution, and parity roadmap references.

## 17. Relationship to the main roadmap

This plan does not reopen the current feature boundary or divert the project into performance tuning. It supplies the missing manual-test infrastructure needed to assess the major features already implemented or still scheduled by `RGV_IGV_PARITY_ROADMAP.md`.

The correct sequence is:

1. Finish the major in-scope product features.
2. Build or update their fixtures as part of each feature's definition of done.
3. Close this suite's catalog and mechanical gates.
4. Run the broad manual pass with these fixtures.
5. Run the separate real-world data, platform, recovery, and performance qualification gates.

Fixture work should therefore happen alongside remaining feature implementation, with Phase 7 closure immediately before the user's broad manual test. It should not become a reason to delay useful product work for speculative micro-optimisation.
