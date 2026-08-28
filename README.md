# RGV

RGV is an experimental native desktop genome viewer in Rust. It uses a custom `wgpu` genomic
canvas, `egui` application chrome, and a bounded off-thread `rust-htslib` alignment engine.

The current native prototype supports:

- A native macOS, Linux, and Windows application shell.
- An IGV-like light desktop layout with application menus, compact locus navigation, assembly and
  contig selectors, locus history, chromosome context, adaptive coordinate ruler, fixed track-label
  gutter, contextual track controls, integrated scrollbar, and compact status bar.
- Cursor-centred zooming, drag panning, and clipped vertical scrolling across stacked tracks.
- Indexed local BAM/CRAM queries with a shared four-thread decompression pool.
- Standalone indexed FASTA genomes with FASTA-backed contig navigation, an independent pinned
  reference track, and bounded off-thread sequence fetching; the same FASTA also configures CRAM.
- All CIGAR operations, secondary/supplementary reads, arbitrary auxiliary fields, and missing
  sequence/quality states in the normalized alignment model.
- Full-query coverage with deterministic, bounded 50-base-window sampling at deep loci so nearby
  navigation does not reshuffle reads already in view.
- CPU read packing and instanced GPU rendering for coverage and CIGAR geometry.
- Generation-based cancellation so stale queries never replace a newer viewport.
- Reference bases, quality-aware mismatches, mate links, `MM`/`ML` overlays, picking, and a read
  inspector.
- Persisted CG/CHG/CHH/HCG/GCH/WCG bisulfite modes with both-strand reference interpretation,
  first-of-pair body colours, protected/converted marks at read and base detail, bounded rendering,
  a copyable legend, and per-read conversion summaries.
- Persisted PacBio SMRT kinetic modes for subread/CLR `ip` and `pw` arrays and CCS
  `fi`/`ri`/`fp`/`rp` arrays. Forward/reverse molecule-strand selection follows alignment
  orientation, per-base colouring uses a documented bounded scale, and missing or malformed arrays
  leave reads visible with explicit gutter and inspector status.
- A persisted Ultima flow-indel mode for signed `tp` direction arrays, `t0` deletion qualities,
  base qualities, and read-group `mc` limits. Insertions and deletions retain their ordinary shape
  while a bounded, position-and-colour confidence mark shows Q0–Q42; the inspector exposes exact
  quality, direction, source, homopolymer base, and unavailable reasons.
- A persisted, explicit SBX evidence mode. Insertions are purple, dark grey, or light grey for
  concordant, partly discordant, or fully discordant base-quality evidence; configurable simplex
  tails can be highlighted, hidden, or left unchanged. Thresholds, fallback state, omitted marks,
  and per-read evidence summaries remain visible rather than inferring the sequencing technology.
- MAPQ/flag/tag filtering plus strand, MAPQ, `RG`, `HP`, and arbitrary SAM-tag colour/group modes.
- Local BED/BED12, GFF3, GTF, genePred/refFlat, narrowPeak/broadPeak, PSL, GWAS/PLINK
  association summaries, indexed
  bigGenePred/bigNarrowPeak, indexed VCF/BCF, BigWig, TDF, bedGraph, WIG, generic BigBed,
  BEDPE/interact, multi-sample SEG/CBS copy-number, and cancer Mutation Annotation Format tracks.
- Bounded background annotation indexing and locus-bar autocomplete for feature names, gene names,
  aliases, IDs, and transcript IDs, with exact matches preferred and ambiguous choices shown.
- HTTP/S BAM, reference-dependent CRAM, FASTA, VCF/BCF, BigWig, TDF, and BigBed access. BAM, CRAM,
  CRAI, and FASTA/FAI use the observable pure-Rust range reader with a bounded disk cache, request
  coalescing, validator checks, safe redirects, retries, cooperative cancellation, and redacted
  authentication objects.
- Track ordering, visibility, and height controls; native file dialogs and drag-and-drop.
- Versioned JSON sessions, partial IGV XML-session import, and deterministic PNG export.
- A 512 MiB bounded cache hierarchy, stable directional prefetch, incremental GPU uploads, and
  observable cache/upload metrics.
- One authoritative columnar alignment batch with packed name, CIGAR, sequence, quality, and
  auxiliary-tag arenas; tags are decoded selectively and full records only on inspection.
- Persistent recent files/loci, configurable cache preferences, keyboard command search, and
  diagnostics that remain disabled unless explicitly enabled.

The current execution roadmap is in
[plans/RGV_IGV_PARITY_ROADMAP.md](plans/RGV_IGV_PARITY_ROADMAP.md); the complete planning index is
[plans/README.md](plans/README.md).
See [the user guide](docs/USER_GUIDE.md), [format support matrix](docs/FORMAT_SUPPORT.md),
[session compatibility policy](docs/SESSION_COMPATIBILITY.md), [portable genome configuration](docs/GENOME_CONFIGURATION.md), and
[validation guide](docs/VALIDATION.md) for operational details. To attach an agent to a visible,
explicitly controlled window, see [the MCP setup and tool guide](docs/MCP.md). The complete deterministic manual
fixture index is [validation/manual-suite/README.md](validation/manual-suite/README.md), with the
full testing procedure in
[validation/manual-suite/TEST_INSTRUCTIONS.md](validation/manual-suite/TEST_INSTRUCTIONS.md).
Historical review material is grouped under [`audits/`](audits/), and resolved defect write-ups
under [`issues/`](issues/).

## Run the desktop viewer

Run the synthetic GPU workload:

```sh
cargo run -p rgv-desktop
```

Open an indexed BAM at an IGV-style one-based locus:

```sh
cargo run --release -p rgv-desktop -- \
  --bam sample.bam \
  --locus chr1:1000001-1100000
```

Open an indexed FASTA without an alignment:

```sh
cargo run --release -p rgv-desktop -- \
  --reference reference.fa \
  --genome-name hg38 \
  --locus chr6:1000001-1010000
```

The FASTA requires an adjacent `.fai`. It can also be opened with **Genomes → Open FASTA** or by
dropping it onto the window. At base detail its sequence is fetched independently of BAM/CRAM and
the reference track remains pinned above the vertically scrollable tracks.

A portable genome bundle can additionally define exact contig aliases, cytobands, and default
annotations:

```sh
cargo run --release -p rgv-desktop -- \
  --genome-config genomes/hg38/genome.rgvgenome \
  --locus 6:1000001-1010000
```

The desktop **File → Open URL** dialog opens remote BAM/CRAM, FASTA, indexed VCF/BCF, BigWig, TDF,
BigBed, bigGenePred, and bigNarrowPeak sources, including custom alignment/reference indexes and an
optional CRAM reference. The same `--bam` argument accepts an HTTP/S BAM or CRAM URL. Remote BAM tries both conventional
`.bai` and `.csi` forms; remote CRAM tries both conventional `.crai` forms.
`--remote-index-url` supplies a nonconventional BAI, CSI, or CRAI URL, while
`--remote-reference-index-url` does the same for a remote FASTA's FAI. Set
`RGV_REMOTE_BEARER_TOKEN` in the launch environment when remote BAM/CRAM/FASTA objects require
bearer authentication; credentials are never stored. Indexed auxiliary URLs may use signed source
URLs, while remote VCF/BCF requires its conventional adjacent index. Add any number of auxiliary
tracks:

```sh
cargo run --release -p rgv-desktop -- \
  --bam sample.bam \
  --reference reference.fa \
  --genome-name hg38 \
  --cytobands hg38.cytoband.tsv \
  --bed targets.bed \
  --gff genes.gff3 \
  --gene-pred transcripts.genePred \
  --ref-flat refseq.refFlat \
  --narrow-peak binding.narrowPeak \
  --broad-peak broad-binding.broadPeak \
  --psl target-alignments.psl \
  --gwas study.assoc.gz \
  --big-gene-pred transcripts.bb \
  --big-narrow-peak binding.bb \
  --vcf calls.vcf.gz \
  --bigwig coverage.bw \
  --tdf archived-signal.tdf \
  --bedgraph signal.bedgraph \
  --bedpe loops.bedpe \
  --seg cohort.seg \
  --maf somatic.maf \
  --locus chr1:1000001-1100000
```

Open a saved RGV session or import an IGV XML session:

```sh
cargo run --release -p rgv-desktop -- --session analysis.rgv.json
cargo run --release -p rgv-desktop -- --import-igv analysis.xml
```

For CRAM, supply its reference FASTA:

```sh
cargo run --release -p rgv-desktop -- \
  --bam sample.cram \
  --reference reference.fa \
  --locus chr1:1000001-1100000
```

Drag to pan and use the mouse wheel or trackpad to zoom around the pointer. Scroll over the track
gutter, use the right-edge scrollbar, or hold Shift while scrolling to move vertically. Right-click
a track label for settings and press Alt+Left/Right for locus history. Use `Escape` to exit.
Application menus open alignments and tracks, save sessions, export PNG images, and clear the
decoded-alignment cache. `Ctrl/Cmd+K` opens command search, `Ctrl/Cmd+D` toggles diagnostics, and
`Ctrl/Cmd+,` opens settings. `Ctrl/Cmd+W` asks for confirmation; press it again to exit.

### Ready-made manual-review workspaces

Generate, mechanically qualify, and list the complete focused manual suite with:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- \
  verify --require-complete
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list
```

This supplies 17 ready-to-open projects for MF-01 through MF-14, 15 plain-language scenario cards,
three MF-15 automation batches, exact truth, deliberate failures, portable relocation, and an
ephemeral loopback remote-source lab. `cargo run` builds changed binaries automatically; a separate
`cargo build` is optional. Fixture readiness does not mean that the manual cards or real-data and
platform qualification have passed.

After building the release executable, open the deterministic manual-test project directly:

```sh
target/release/rgv-desktop --session benchmark-data/demo-tracks/rgv-demo-project.json
```

The project combines the checked-in variant BAM and FASTA with 19 annotation, variant,
quantitative, interaction, copy-number, mutation, and indexed track examples at
`chrVariants:8501-16000`. Its checksum manifest is
`benchmark-data/demo-tracks/manifest.json`.

Open the companion alignment project separately:

```sh
target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-alignment-review-project.json
```

Its four panels cover paired reads, RNA junctions, long/supplementary/modified/specialist reads,
and the 70,000-operation CIGAR truth. Its six-item Review queue also links the deep-coverage and
structural-pair loci. Regenerate and mechanically verify both projects through RGV's production
readers with:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-demo-corpus
```

See [the manual qualification guide](validation/MANUAL_QUALIFICATION.md) for the short starting
sequence and the later representative-data checklist.

## Run the alignment benchmark

```sh
cargo run --release -p rgv-alignment-bench -- generate
cargo run --release -p rgv-alignment-bench -- run --iterations 5
```

The deterministic BAM, indexes, variant truth, and reference under `benchmark-data/` are checked in
so tests and the manual-review projects work immediately. Regenerate them with the commands above
when changing the declared truth corpus.

For visual testing of alignment events, generate the compact variant truth set:

```sh
cargo run --release -p rgv-alignment-bench -- generate-variants
target/release/rgv-desktop \
  --bam benchmark-data/variants.bam \
  --reference benchmark-data/variants.fa \
  --locus chrVariants:9941-10080
```

Every read is named for its intentional event. The base-scale locus above contains SNVs and 1/6 bp
insertions plus 1/12 bp deletions. Use `chrVariants:9401-12600` to review 100/1,000 bp insertions
and 500/2,000 bp deletions. For Ultima truth, select **Ultima flow indels** and open
`chrVariants:13991-15120`; its three reads cover directional `tp` insertion Q40, read-group `mc`
deletion Q0, and single-base `t0` deletion Q35. The generator writes an indexed BAM and indexed
FASTA.

## Run the interaction benchmark

The interaction benchmark measures indexed query, coverage/sampling, read packing, and display-list
construction as one pipeline:

```sh
cargo run --release -p rgv-interaction-bench -- \
  benchmark-data/mixed.bam \
  --locus chr1:900001-920000 \
  --iterations 10 \
  --maximum-p95-ms 16.7 \
  --maximum-first-useful-ms 250 \
  --output benchmark-results/interaction-latest.json
```

The deterministic WAN range gate injects latency and one transient failure while enforcing exact
request and response-byte budgets:

```sh
cargo run --release -p rgv-remote-range-bench -- \
  --output benchmark-results/remote-range-latest.json
```

## Quality gates

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

The same checks run on macOS, Linux, and Windows in CI.

## Current limitations

- Text BED/GFF/GTF, genePred/refFlat, and narrowPeak inputs are currently loaded into bounded
  in-memory indexes. BigBed, bigGenePred, and bigNarrowPeak stay indexed and on-demand; explicit
  name search without a BigBed extra index performs a bounded background scan.
- Release signing requires the platform credentials described in
  [docs/RELEASING.md](docs/RELEASING.md).

## License

RGV is available under either the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option. Release packages also carry the locked
third-party license inventory and full license/attribution texts used by their dependencies.
