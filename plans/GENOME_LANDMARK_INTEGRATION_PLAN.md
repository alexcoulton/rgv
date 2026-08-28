# Built-in genome landmark integration plan

Status: implemented locally; distribution review and four-build manual GUI qualification remain

Date: 28 August 2026

## Implementation outcome

The runtime, curation pipeline, and local resources described by this plan are implemented. The
work is intentionally not marked distribution-ready: every bundle notice still carries the
`local_qualification_only_pending_release_review` gate, and only the available GRCh38 RNA-seq
environment has received end-to-end live GUI/MCP qualification.

Implemented deliverables:

- A bounded, schema-versioned `GenomeLandmarkCatalogue` in `rgv-core`, including safe relative-path
  handling, canonical-path confinement, byte-size and SHA-256 verification, role/format validation,
  duplicate-claim rejection, and a catalogue-driven assembly resolver.
- Exact dictionary, compatible dictionary, explicit identity, sequence-MD5, unique-length
  suggestion, ambiguity, and incompatibility evidence paths shared by FASTA and BAM/CRAM handling.
- Automatic ordinary `Genes` tracks with representative exon/CDS/UTR geometry, Squished packing,
  a 48-point default, duplicate prevention, normalized aliases/cytobands, normalized search, and
  explicit all-transcript actions.
- Portable version-pinned built-in track identity, hide-versus-remove semantics, persisted
  suppression, explicit restoration, and strict catalogue rebinding for both startup and in-app
  project opening. Missing, changed, or corrupt pins are suppressed instead of trusting a saved
  developer path or silently upgrading.
- Pixel-aware overview/regional/detail rendering, deterministic label bounds, selected-label
  retention, and feature picking from the exact rendered primitive geometry.
- Gene Properties and source provenance, Genomes-menu controls, diagnostics, typed automation/MCP
  landmark state, and exact bundle/checksum reporting.
- An offline Python curator and staging tool. The application does not ship or run the converters.
  Compact and complete package staging are separate, hash-verified operations and refuse the
  pending distribution state unless explicitly qualified for local-only staging.

The current local catalogue occupies approximately 86 MiB including retained curation evidence;
normalized runtime assets occupy approximately 48 MiB. The five representative resources are:

| Bundle | Version | Representative genes | Representative asset SHA-256 |
| --- | --- | ---: | --- |
| `hg38` | `1.0-rgv1` | 19,062 | `8f4ef6013761d128d071e0aff8e3043d686dbb49ff73ea9aa97f2f7d291711bd` |
| `hg19` | `1.0-rgv1` | 19,254 | `44049a32566111194118a68631d2099524bd2245cf0f2b77ae097762f486a4bd` |
| `mm10` | `1.0-rgv1` | 38,779 | `e0296fc2c34ebf318e7d182ceb7b1d993b0b3070b8562345a9b60c2e6eb09269` |
| `mm39` | `1.0-rgv1` | 26,683 | `eb6755c4337f2a9e0faf55b84c58022243c713711df66faf4dd8c97b9b260883` |
| `hs1` | `1.0-rgv1` | 28,738 | `632f3f38c3da14dad9f28a6b56e751f04dab5b76e4de2fd5ed51cd7e3864087f` |

Qualification completed on 28 August 2026:

- Two consecutive five-bundle builds produced byte-identical normalized outputs.
- All nine Python curator/staging tests passed.
- `cargo fmt --all -- --check`, `cargo check -p rgv-desktop --all-targets`, and
  `cargo test --workspace` passed. The workspace run included the complete 326-test desktop suite;
  only deliberately network-dependent public-server tests were ignored.
- The live GRCh38 RNA-seq session resolved the exact 292-contig ordered dictionary, automatically
  loaded the pinned `hg38` Genes track, aligned exon landmarks with RNA coverage and splice arcs,
  returned normalized gene search results, exposed exact ACTB provenance in Properties/MCP, and
  demonstrated whole-chromosome, regional, gene-scale, selection, hide/show, removal, and saved
  suppression behaviour.

Remaining gates:

1. A redistribution/licensing review must approve the generated notices and upstream terms before
   either compact or detailed resources are shipped.
2. The manual GUI matrix in section 14 remains to be run for `hg19`, `mm10`, `mm39`, and `hs1` with
   matching reference/alignment fixtures. Full reference sequences were explicitly outside this
   plan's download scope, so those runs have not been fabricated from catalogue metadata.
3. Installer integration and resource publication remain separate release work; local staging is
   implemented, but no resources have been uploaded.

## 1. Objective

Make exon-aware gene landmarks appear automatically when RGV confidently identifies a supported
reference assembly. The automatic track must contain one representative transcript per gene with
complete exon, intron, CDS, UTR, strand, identifier, symbol, and provenance information. A separate
all-transcript resource must remain available on demand without making the default view dense or
expensive.

The initial supported catalogue is:

| Bundle | Assembly | Automatic landmark source | Optional detailed source |
| --- | --- | --- | --- |
| `hg38` | GRCh38 | MANE representative models | RefSeq All |
| `hg19` | GRCh37 | RefSeq Select models | RefSeq All |
| `mm10` | GRCm38 | One selected RefSeq transcript per gene | RefSeq All |
| `mm39` | GRCm39 | One selected curated RefSeq transcript per gene | RefSeq Curated |
| `hs1` | T2T CHM13 v2.0 | One selected curated RefSeq transcript per gene | RefSeq All |

This plan covers local development resources and runtime integration. Uploading, remote catalogue
delivery, background updating, and downloading complete reference sequences are not required for
this slice.

## 2. Baseline recorded before implementation

This section records the starting state used to derive the plan. The implementation outcome above
supersedes it as the current status.

### Implemented foundations

- RGV already has explicit assembly identity and confidence in `crates/rgv-core/src/genome.rs`.
- Alignment headers can currently suggest GRCh38 from length and MD5 fingerprints.
- Portable `.rgvgenome` files can provide aliases, cytobands, provenance, and default annotations.
- `bigGenePred` and extended `genePred` preserve transcript spans, exon blocks, CDS, strand, gene
  symbol, transcript accession, and related attributes.
- Gene models already use a compact 48-point default height.
- Annotation querying, search, adjacent-feature navigation, selection, inspection, session
  persistence, automation snapshots, and MCP workspace reporting already operate on gene-model
  tracks.
- Genome annotations can now retain an explicit binary schema such as `big_gene_pred` through
  startup, dynamic genome loading, and session construction.

### Local curated resources

The local development catalogue is under:

```text
benchmark-data/large-public-alignments/landmarks/igv/
```

It contains approximately 73 MiB across `hg38`, `hg19`, `mm10`, `mm39`, and `hs1`, with IGV genome
definitions, chromosome sizes, aliases, cytobands, representative or curated gene assets, detailed
transcript assets, search material, per-asset hashes, and manifests. The Python curator is
`tools/landmarks/build_bundles.py`, invoked by `scripts/build-landmark-bundles.sh`.

### Important gaps

- The local catalogue is not read by RGV.
- The current manifests are curation records, not a stable application resource schema.
- Resource licensing and attribution notices are not yet complete enough for distribution.
- Search assets use a mixture of IGV Trix and RGV-generated TSV conventions.
- Cytobands use compressed text for four builds and BigBed for `hs1`; RGV currently expects local
  UCSC-format text.
- `hs1` still needs a strict one-representative-transcript-per-gene derivative.
- Assembly identity matching is hard-coded for one GRCh38 fingerprint set rather than driven by a
  catalogue.
- Sessions currently persist resolved paths rather than a stable built-in bundle/asset identity.

## 3. Product decisions and default behaviour

These decisions are part of the acceptance contract, not suggestions to defer to implementation.

1. **Genes with exons are automatic.** A supported, confidently identified assembly receives one
   visible representative gene track without requiring the user to open a GTF or BigBed.
2. **One representative transcript per gene is the default biological model.** The default asset
   contains full exon/CDS/UTR geometry; it is not a gene-span-only index.
3. **All transcripts are opt-in.** Zooming in must not silently replace the representative resource
   with RefSeq All. The user explicitly chooses **Show all transcripts**.
4. **Geometry is adaptive.** Wide views suppress exon detail that cannot occupy meaningful pixels;
   close views reveal the exon blocks already present in the same representative record.
5. **Row packing and biological detail are separate.** Existing Collapsed, Squished, and Expanded
   settings continue to control packing. Zoom-dependent geometry must not rewrite the saved packing
   preference.
6. **The default packing mode is Squished** for built-in landmarks, with a 48-point track height.
7. **The track is called `Genes`.** Provider, release, assembly, resource version, and licence are
   shown in Properties and provenance, not forced into the everyday track title.
8. **The track is hideable and removable.** Removal is persisted as an explicit suppression so the
   automatic track does not return on every project load.
9. **No ambiguous auto-loading.** Contig prefixes such as `chr` are never sufficient evidence for
   an assembly. An uncertain match produces a suggestion, not an automatic annotation.
10. **No duplicate automatic track.** A restored built-in track, an equivalent configured default,
    or an explicit user gene track prevents insertion of a second automatic copy.
11. **Cytobands and aliases are automatic supporting context** after a bundle match, but remain
    independently reportable in genome diagnostics.
12. **Reference sequence remains separate.** Landmark bundles identify coordinate systems and
    annotations but do not pretend to provide bases when no matching FASTA is loaded.

## 4. Required resource normalization

Complete this work entirely in the offline Python curation pipeline. No converter or curator is
distributed in the RGV application.

### 4.1 Representative tracks

- Validate whether every `hg38` MANE and `hg19` RefSeq Select gene has exactly one selected model.
  Where a source contains MANE Plus Clinical or other additional models, apply a documented stable
  rule and emit one automatic model per gene while preserving the complete source as detail.
- Retain the current deterministic mouse selection priority: `NM_`, `NR_`, `XM_`, `XR_`, coding
  length, spliced length, genomic span, then stable accession.
- Read the `hs1` curated BigGenePred in Python, group by stable gene identity, apply the same
  deterministic representative policy, and emit a compact exon-aware `genePred.gz` or
  `bigGenePred` asset.
- Validate all transcript, CDS, and exon coordinates against the bundle chromosome dictionary.
- Exclude off-reference fix/alternate records from automatic landmarks while retaining untouched
  detailed sources and recording exclusion counts.

### 4.2 Unified search index

- Define one versioned `rgv_gene_names_v1` format rather than teaching runtime code both Trix and
  ad hoc TSV variants.
- Include normalized term, display symbol, stable gene ID when available, representative transcript
  ID, canonical contig, start, end, strand, and optional synonyms.
- Sort deterministically by normalized term, contig order, start, end, and stable ID.
- Keep the index small enough to load once for the active assembly; no database server is required.
- Search must support exact and prefix matches without scanning a BigBed or materializing the
  detailed transcript track.

### 4.3 Cytobands and aliases

- Normalize every cytoband asset to decompressed UCSC text with one schema and validate coordinates
  against chromosome sizes.
- Convert the `hs1` cytoband BigBed offline; do not add a second cytoband reader solely for one
  curated asset.
- Normalize alias files to an RGV-owned two-column `alias<TAB>canonical` form.
- Reject aliases whose canonical target is absent, aliases with multiple targets, and aliases that
  collide with a different canonical contig.

### 4.4 Identity and integrity

- Record a SHA-256 over the ordered canonical `contig<TAB>length` dictionary.
- Record available primary-contig sequence MD5 fingerprints separately from the dictionary hash.
- Assign a stable bundle and release ID, for example `hg38-mane-1.0-rgv1`.
- Hash every packaged asset and fail curation when an existing immutable release changes.
- Make two consecutive builds byte-for-byte deterministic.

### 4.5 Licensing and attribution

- Add a bundle-level `NOTICE.md` plus structured per-asset licence fields.
- Preserve source URL, upstream project, upstream version/date, derivation, SHA-256, citation, licence
  URL, required notice, and local output role.
- Include the IGV MIT notice for copied configuration metadata.
- Include UCSC, NCBI/RefSeq, MANE, and GENCODE notices as applicable.
- Exclude restricted clinical tracks, liftOver chains, or any source whose redistribution status is
  unresolved.
- Treat legal review of the finished notices as a distribution gate even though local integration
  may proceed first.

## 5. Runtime catalogue schema

Introduce a small, read-only, schema-versioned catalogue in `rgv-core`. It must describe resources,
not own track rendering or queries.

Conceptually:

```text
GenomeLandmarkCatalogue
└── bundles[]
    ├── id + version + display name
    ├── assembly identity
    │   ├── stable IDs and compatible aliases
    │   ├── primary contig count
    │   ├── dictionary SHA-256
    │   └── optional contig MD5 fingerprints
    ├── representative gene asset
    ├── detailed transcript asset
    ├── gene-name search asset
    ├── normalized cytobands
    ├── normalized contig aliases
    └── provenance/licensing assets
```

Requirements:

- Asset paths are relative to a resource root and may not escape it.
- Every asset declares its semantic role, exact decoder format, byte size, and SHA-256.
- Catalogue validation is bounded and rejects duplicate bundle IDs, duplicate assembly claims,
  incompatible formats, unsafe paths, invalid hashes, and missing required roles.
- Runtime uses a configurable resource root. Development points to the current ignored landmark
  directory; packaged builds resolve the installed application resource directory.
- Failure of one optional or nonmatching bundle does not prevent RGV from starting, but a selected
  bundle with a corrupt required asset produces a visible typed error.
- The checked-in catalogue schema has unit tests and fixture manifests; large data remains outside
  Git until packaging is deliberately enabled.

## 6. Assembly resolution

Replace the GRCh38-only recognition branch with a catalogue-driven resolver shared by standalone
references, BAM/CRAM headers, genome configurations, and restored sessions.

Resolution priority:

1. A valid explicit assembly identity in a loaded genome configuration or session.
2. Exact ordered dictionary hash from a loaded FASTA/FAI.
3. Sufficient length and MD5 fingerprint agreement from BAM/CRAM `@SQ` records.
4. A unique complete primary-contig length match, reported as a suggestion unless policy explicitly
   permits promotion.
5. Otherwise unknown.

Rules:

- Explicit identity must still be checked for incompatibility with the loaded dictionary.
- Fingerprint mismatches veto a candidate rather than being ignored.
- Partial dictionaries may match only through strong fingerprints and must remain labelled as a
  subset.
- If two bundles remain possible, neither is selected automatically.
- The resolver returns evidence, confidence, matched/expected counts, and rejection reasons for
  diagnostics and MCP—not only a bundle ID.
- Add fixtures for hg38, hg19, mm10, mm39, hs1, partial GRCh38, prefix-only ambiguity, conflicting
  MD5, and mixed dictionaries.

## 7. Automatic track lifecycle

Use the existing `TrackConfig`, track engine, presentation reducer, query generations, and panel
projections. Do not create a parallel built-in annotation renderer.

### Insertion

- After assembly resolution settles, resolve and verify the representative asset.
- Construct a normal `TrackConfig` with `TrackKind::GeneModel`, the exact format, Squished packing,
  48-point height, visible state, and built-in provenance.
- Add it through the same configured-source path used by `.rgvgenome` defaults.
- Load normalized aliases and cytobands in the same transaction so a frame never claims complete
  genome context before its supporting resources are ready.
- Refresh all panels through existing generation-aware query paths.

### Identity and persistence

- Give a built-in asset a stable logical identity containing bundle ID, bundle version, asset role,
  and expected SHA-256.
- Persist that logical identity in addition to the resolved local path. On restore, re-resolve the
  installed resource and verify its hash rather than trusting a developer-machine absolute path.
- Persist automatic-track suppression per bundle in the session document.
- Preserve explicit user changes to visibility, height, packing, ordering, grouping, and colour.
- A saved project remains pinned to its bundle version. A newer installed release must not silently
  change its gene models.

### Removal and replacement

- Removing an automatic Genes track sets suppression and uses the normal auxiliary removal path.
- **Restore default genes** clears suppression and re-adds the pinned or current compatible bundle.
- Opening a `.rgvgenome` with its own default gene annotation must replace or suppress the automatic
  track according to stable source identity, never merely by comparing display names.
- Changing to a different assembly removes the incompatible built-in track, aliases, search index,
  and cytobands atomically before installing the new context.

## 8. Adaptive exon-aware rendering

The same representative records must support every zoom. Do not request or switch to the detailed
transcript asset as a side effect of magnification.

Define geometry using bases per pixel and measured label capacity:

- **Overview:** draw a thin gene span and direction indication; suppress exon boxes and most labels
  when individual blocks are subpixel.
- **Regional:** draw packed exon blocks, intron connectors, strand direction, and collision-bounded
  gene symbols.
- **Detailed:** distinguish UTR and CDS geometry, preserve individual exon boundaries, and show the
  selected gene label even when normal labels collide.
- **Selection:** expose the complete representative record and exon count in Properties at every
  zoom, even when overview geometry is simplified.

Requirements:

- Thresholds derive from projected pixel widths, not hard-coded chromosome-window sizes.
- Label selection is deterministic and stable during small pans.
- Wide queries remain bounded; a dense chromosome does not create one label or primitive per gene
  when they cannot be displayed.
- Picking uses the exact geometry and row assignment used by rendering.
- Row packing settings retain their current meaning and session representation.
- Accessibility exposes the gene symbol, locus, strand, and model type without relying on colour.

## 9. Search and navigation integration

- Load only the active bundle's normalized name index.
- Merge catalogue search results with existing open-track and variant-ID results using stable source
  identity and deterministic ordering.
- Exact gene-symbol matches take priority, but ambiguous symbols never navigate silently.
- Search results identify the bundle and assembly when multiple contexts are possible.
- Navigation selects the representative Genes track and uses its normal selection/picking state.
- Search remains available when the Genes track is hidden but not when it has been explicitly
  removed and suppressed, unless product review chooses a separate global-genome-search contract.
- Adjacent-feature navigation operates on representative genes by default.
- Search index failure must not disable rendering; rendering-source failure must not return stale
  search hits.

## 10. Optional all-transcript workflow

Add explicit actions:

- **Show all transcripts** from the Genes track menu and settings.
- **Show all transcripts for this gene** from search results and Properties.
- **Return to representative genes** from the detailed track.

Behaviour:

- The full detailed asset is a separate ordinary GeneModel track with clear source/release naming.
- It is off by default and never added solely because the user zoomed in.
- Whole-asset loading must not block the UI. Prefer indexed BigGenePred for distributed detailed
  assets; if a local compressed genePred source remains, load it asynchronously with explicit
  progress and bounds.
- A gene-scoped action should query only the selected locus where the format supports indexed
  access.
- Representative and detailed tracks have distinct stable IDs and session state.
- Removing the detail track does not suppress or alter automatic representative genes.

## 11. UI, provenance, diagnostics, and MCP

### Genomes menu

- Show the matched assembly, bundle release, evidence/confidence, and subset status.
- Add **Restore default genes** and **Open landmark resource folder** for development builds.
- Show a direct explanation when no bundle matches or a required asset fails integrity checks.

### Track settings and Properties

- Default title: `Genes`.
- Source section: assembly, annotation provider, release, selection policy, asset checksum, and
  citation/licence link.
- Track menu: packing, visibility, height, **Show all transcripts**, and removal.
- Gene Properties: symbol, stable gene ID, representative transcript, locus, strand, exon count,
  CDS/UTR status, biotype/type, selection rationale where recorded, and source provenance.

### Diagnostics and automation

- Report catalogue schema version, resource root, matched bundle/version, integrity state, assembly
  evidence, automatic-track state, suppression state, gene asset format, and search readiness.
- Extend typed automation/MCP snapshots without exposing credentials or arbitrary local paths.
- MCP must distinguish `configured`, `matched`, `loaded`, `visible`, `suppressed`, and `failed`.
- Screenshot qualification records the exact bundle ID and SHA-256 used.

## 12. Packaging policy

For local development, continue using the ignored shared resource directory. For application
packaging, separate assets into:

1. **Required compact context:** representative genes, normalized search, aliases, cytobands,
   catalogue, notices, and manifests.
2. **Optional detail:** all-transcript resources.

Measure both packages before deciding whether optional detail ships in the base installer. The
default experience may not depend on network access. Every packaged file must be reproducible from
the Python curator, hash-verified, and covered by the distribution notice.

Do not package the upstream IGV JSON files merely because the curator used them. Once the normalized
RGV manifest contains the required provenance and reference URLs, retain IGV JSON as curation
evidence unless runtime genuinely consumes it.

## 13. Implementation sequence

### Phase A — finish and qualify resources

1. Add pinned Python dependencies through `uv` only if BigBed decoding is required.
2. Produce strict representative assets for all five assemblies, including `hs1`.
3. Normalize search, aliases, and cytobands.
4. Add dictionary hashes, stable bundle versions, notices, and per-asset licences.
5. Add deterministic Python tests and run two-build checksum comparison.
6. Freeze the first local resource release.

### Phase B — catalogue and assembly resolver

1. Add bounded catalogue models and validation to `rgv-core`.
2. Add resource-root resolution and hash verification to desktop startup.
3. Replace hard-coded recognition with catalogue-driven evidence matching.
4. Expose typed resolution results to diagnostics and automation.

### Phase C — automatic Genes track

1. Add stable built-in resource identity and session suppression state.
2. Insert the normal GeneModel track after a confident match.
3. Apply aliases and cytobands transactionally.
4. Deduplicate configured, restored, and automatic sources.
5. Implement restore, removal, and assembly-change lifecycle.

### Phase D — adaptive display and search

1. Implement pixel-aware span/exon/CDS geometry LOD.
2. Add deterministic label density and exact picking tests.
3. Add normalized bundle search alongside existing annotation search.
4. Complete Properties, track settings, accessibility, and provenance presentation.

### Phase E — detailed transcripts and qualification

1. Add explicit all-transcript actions and lifecycle.
2. Ensure detailed source loading remains asynchronous and bounded.
3. Extend session, automation, MCP, recovery, and failure-isolation coverage.
4. Run manual and MCP visual qualification across all five assemblies and required zoom states.

## 14. Test matrix

### Python resource tests

- Source schemas, coordinate conversion, exon ordering, CDS containment, and contig compatibility.
- Deterministic representative selection under reordered input.
- Off-reference exclusion with recorded counts.
- Alias conflicts, cytoband bounds, search normalization, licence completeness, and path safety.
- Stable byte-for-byte outputs and manifest hashes.

### Core and HTS tests

- Catalogue parsing limits, unsafe paths, duplicate identities, and format/role compatibility.
- Full, partial, explicit, fingerprint, ambiguous, and conflicting assembly matches.
- BigGenePred and genePred exon/CDS semantics for representative assets.
- Stable built-in source identity and version-pinned session round trips.

### Desktop tests

- Automatic insertion only after a confident match.
- No duplicate insertion after startup, dynamic genome loading, or session restore.
- Removal suppression and explicit restoration.
- Assembly changes atomically remove incompatible context.
- Packing, height, visibility, order, and grouping persist.
- Search exactness, ambiguity, hidden/removed behaviour, and stale-result invalidation.
- Overview, regional, detailed, selected, and multi-panel rendering/picking.
- Corrupt optional search versus corrupt required landmark failure isolation.

### Manual qualification

For every bundle, capture and inspect:

1. Whole chromosome or comparably wide overview.
2. Multi-megabase regional view.
3. Gene-scale exon view.
4. Selected gene and Properties.
5. Exact gene-symbol search and an ambiguous search.
6. **Show all transcripts** and return to representative genes.
7. Hide, remove, save, restore, and restore-default behaviour.
8. Unknown and deliberately mismatched reference behaviour.

The existing GRCh38 RNA-seq session must additionally prove that automatic genes align with splice
junction evidence and the exact reference dictionary used by the BAM.

## 15. Acceptance criteria

The work is complete only when:

1. hg38, hg19, mm10, mm39, and hs1 have versioned, deterministic, redistribution-reviewed bundles.
2. Every automatic record represents one gene and retains valid exon/CDS/UTR geometry.
3. Loading or confidently recognizing a supported assembly adds one visible `Genes` track without
   user file selection.
4. Wide views show stable gene landmarks without excessive primitives, labels, rows, or queries.
5. Close views expose individual exons from the same representative model.
6. Full transcripts never load automatically and are discoverable through explicit actions.
7. Unknown or ambiguous assemblies never receive a silently guessed gene track.
8. Search works from the normalized bundle index without scanning the detailed annotation.
9. Session restore is portable, version-pinned, and respects automatic-track suppression.
10. Corrupt or missing resources fail visibly and do not damage unrelated BAM, variant, or
    quantitative tracks.
11. Properties, diagnostics, automation, MCP, and screenshots report the exact bundle and evidence.
12. All focused tests, the complete desktop suite, and the five-assembly manual qualification pass.

## 16. Recommended first implementation slice

Start with resource normalization plus one end-to-end `hg38` vertical slice:

1. Freeze `hg38-mane-1.0-rgv1` with normalized names, aliases, cytobands, dictionary identity, and
   notices.
2. Implement the catalogue schema and exact GRCh38 resolver.
3. Automatically add the existing BigGenePred as a normal 48-point Squished Genes track.
4. Add suppression, session persistence, search, and provenance.
5. Qualify whole-chromosome, regional, exon-scale, and RNA splice-junction views.

Once that slice is stable, adding the other four bundles should primarily be data qualification and
assembly fixtures rather than four new runtime implementations.
