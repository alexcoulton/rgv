# Developing RGV

Building RGV from source, the command-line options for development, the ready-made test
workspaces, the benchmarks and the quality gates. For using RGV, see the [README](../README.md)
and the [user guide](USER_GUIDE.md).

## Build and run from source

RGV is a Rust workspace. Install Rust with [rustup](https://rustup.rs); the toolchain version is
pinned in `rust-toolchain.toml` and installed automatically. On Linux, also install the
development packages for fontconfig, xkbcommon and Wayland (on Debian or Ubuntu:
`sudo apt-get install libfontconfig1-dev libxkbcommon-dev libwayland-dev`).

Windows builds use the MinGW target (`x86_64-pc-windows-gnu`) because HTSlib does not build with
MSVC; see [`.github/actions/windows-mingw`](../.github/actions/windows-mingw/action.yml) for the
toolchain and [`vendor/README.md`](../vendor/README.md) for the patched HTSlib crates.

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

## Ready-made manual-review workspaces

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

See [the manual qualification guide](../validation/MANUAL_QUALIFICATION.md) for the short starting
sequence and the later representative-data checklist.

## Alignment benchmark

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

## Interaction benchmark

No matched RGV-versus-IGV comparison has been run; see the status section at the top of
[benchmarks/IGV_COMPARISON.md](../benchmarks/IGV_COMPARISON.md). The benchmark below is headless and
is a regression gate, not a comparison with IGV.

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

The toolchain is pinned in `rust-toolchain.toml`; `./test.rgv.sh` runs the formatting, Clippy and
test steps in one go. `.github/workflows/ci.yml` builds and tests on macOS, Linux and Windows, and
runs formatting and Clippy separately. The test suite passes on all three platforms; the
formatting and Clippy jobs do not yet pass, because of existing formatting differences and
pedantic Clippy warnings.

A plain `cargo build` or `cargo test` at the repository root skips `rgv-alignment-bench`, the
only crate that pulls the benchmark-only alignment backends; pass `--workspace` or
`-p rgv-alignment-bench` to include it.

## Releases

How release packages are built and published is in [RELEASING.md](RELEASING.md).

## Further documentation

- [Format support matrix](FORMAT_SUPPORT.md)
- [Session compatibility policy](SESSION_COMPATIBILITY.md)
- [Portable genome configuration](GENOME_CONFIGURATION.md) and [landmark bundles](LANDMARK_BUNDLES.md)
- [Validation guide](VALIDATION.md), the [manual fixture index](../validation/manual-suite/README.md)
  and its [testing procedure](../validation/manual-suite/TEST_INSTRUCTIONS.md)
- [Controlling RGV from an MCP agent](MCP.md)
- [Interaction-stress testing](INTERACTION_STRESS.md)
- [Architecture decisions](architecture/)
- Historical review material in [`audits/`](../audits/) and resolved defect write-ups in
  [`issues/`](../issues/)
