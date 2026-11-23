# Curated gene landmark bundles

RGV keeps lightweight gene-context resources locally and will distribute the finished resources,
not the curation program. The offline curation workflow is a standard-library Python script. It
mirrors the annotation choices in IGV's official genome definitions and records the source URL,
size, and SHA-256 digest for every downloaded or derived file.

The initial catalogue contains the five current human and mouse builds discussed for RGV. Every
build draws and searches **every gene of IGV's default gene track** — the track IGV itself shows and
whose names it finds — as one representative transcript per gene symbol. A build with a curated
"select" set prefers that set's transcript as the representative:

| Build | Assembly | Gene list (drawn and searchable) | Preferred representative | Genes | Drawn / search size | Detailed source |
| --- | --- | --- | --- | ---: | --- | --- |
| `hg38` | GRCh38 | IGV RefSeq All | MANE Select (IGV `mane.1.0.bb`) | 42,764 | 3.2 MB / 1.1 MB | IGV RefSeq All |
| `hg19` | GRCh37 | IGV RefSeq All | RefSeq Select (IGV `ncbiRefSeqSelect.bb`) | 29,692 | 2.7 MB / 0.7 MB | IGV RefSeq All |
| `mm10` | GRCm38 | IGV RefSeq All | — | 38,779 | 3.0 MB / 0.8 MB | IGV RefSeq All |
| `mm39` | GRCm39 | IGV RefSeq Curated | — | 26,683 | 2.5 MB / 0.6 MB | IGV RefSeq Curated |
| `hs1` | T2T CHM13 v2.0 | IGV RefSeq All BigGenePred | — | 41,566 | 3.2 MB / 0.9 MB | IGV RefSeq All BigGenePred |

Each directory also contains IGV's genome definition, chromosome sizes, aliases and cytobands when
the preset supplies them. IGV Trix search files are retained where available. The bundles do not
include multi-gigabyte FASTA or 2bit sequences; their manifests preserve the matching reference URL.

Representative selection keys genes by their RefSeq gene symbol, so the drawn track and the search
index hold exactly one record for every symbol of the default track on the bundle's contigs. Within a
gene, a preferred transcript (same accession and contig; another version of it ranks next) comes
first, then `NM_`, `NR_`, `XM_` and `XR_` accessions, followed by coding length, spliced length,
genomic span, and accession. In `hg38` the MANE Select record's RefSeq twin (`ncbiId`) is the
representative, so MANE genes keep their MANE model wherever RefSeq All still carries that
accession; genes MANE does not cover (PBRM1 in MANE 1.0,
for example) are drawn and found through their RefSeq representative. Exon arrays and all
coordinates are validated against IGV's chromosome-size file. The unfiltered source table is kept
beside the compact derived track.

Every build also cross-validates the representative gene model and symbol-search assets before its
manifest is written: transcript, symbol, contig, coordinates, and strand must agree one-for-one, so
every search hit lands on a gene the Genes track draws. Every search row must retain a non-empty
stable gene ID: in `hg38` it is the Ensembl gene (`ENSG…`) of the gene's MANE Select record —
matched by accession, or by symbol when MANE's RefSeq accession is no longer in RefSeq All — and
otherwise, in every build, the RefSeq gene symbol, which the selection makes unique within the
bundle. `hg38` search rows also carry the MANE symbol (when RefSeq has since renamed the gene, as
`ARNTL` → `BMAL1`) and the MANE Ensembl transcript as synonyms, so those names still find the gene.
`--validate-existing` additionally fails if any gene of the detailed track is not searchable.

## Build or refresh the local catalogue

```sh
scripts/build-landmark-bundles.sh
```

Build only selected assemblies with repeatable `--build` arguments:

```sh
scripts/build-landmark-bundles.sh --build hg19 --build mm39
```

Re-curate from the source files already downloaded, without any network access (a missing source
is an error, never a download):

```sh
scripts/build-landmark-bundles.sh --offline --build hg38
```

Add `--output-root DIR` to work on a catalogue stored elsewhere (the folder holding `catalog.json`).

A rebuild that changes a bundle's assets must also bump its `bundle_version` in `build_bundles.py`;
the curator refuses to change an existing release in place. Saved sessions pin the version and
checksum of a built-in track, so after such an update their Genes track is reported as not rebound
and is added again from the new bundle.

Validate every installed file, representative/search identity pair, search coverage of the detailed
track, and cytoband record without performing any network requests:

```sh
scripts/build-landmark-bundles.sh --validate-existing
```

The validation report lists primary contigs without cytobands or `acen` records so missing
centromere structure is explicit rather than inferred.

Outputs are written to:

```text
benchmark-data/large-public-alignments/landmarks/igv/
├── catalog.json
├── hg38/
├── hg19/
├── mm10/
├── mm39/
└── hs1/
```

The large resource directory is deliberately ignored by Git and shared between development
worktrees. The Python script and documentation are repository content. Use `--force` to redownload
existing resources; downloads are written to temporary files and atomically renamed when complete.

The earlier GENCODE v29 bundle matching the local RNA-seq FASTA remains under
`landmarks/grch38-gencode-v29`. It is a dataset-specific bundle, separate from the IGV-compatible
catalogue above; it is used through `genomes/grch38-gencode-v29.rgvgenome`, not through the
catalogue.

## Using a catalogue stored elsewhere

The viewer does not need the repository's `benchmark-data` folder. It uses the first of these
locations that holds `catalog.json` (a folder whose `igv/` subfolder holds it, such as the
`landmarks/` folder above, is accepted too):

1. `--landmark-resource-root DIR` — when given, nothing else is consulted;
2. the `RGV_LANDMARK_RESOURCE_ROOT` environment variable;
3. the `landmark_resource_root` desktop setting, set with **Genomes → Choose landmark folder…**
   (the catalogue is read and validated in the background and the setting is saved only if it is
   valid; the open reference is then re-matched at once);
4. the per-user data folder: `~/Library/Application Support/RGV/landmarks` (macOS),
   `%APPDATA%\RGV\landmarks` (Windows), `$XDG_DATA_HOME/rgv/landmarks` or
   `~/.local/share/rgv/landmarks` (elsewhere) — copy or link the built catalogue there to make it
   the default for every launch;
5. the development and packaged locations: `benchmark-data/large-public-alignments/landmarks/igv`
   and `resources/landmarks` relative to the working directory, then `landmarks`,
   `resources/landmarks` and `../Resources/landmarks` beside the executable.

For example, with the catalogue on an external drive:

```sh
export RGV_LANDMARK_RESOURCE_ROOT="/Volumes/Data/large-public-alignments/landmarks/igv"
target/release/rgv-desktop --bam sample.bam --reference GRCh38_with_spike_ins.fa
```

Finding no catalogue at all is reported as *not configured* (automation: `integrity:
"not_configured"`, `automatic_track_state: "unconfigured"`), not as a failure. A location you named
that holds no catalogue, an invalid catalogue, or an asset whose size or SHA-256 differs from the
catalogue is a failure and is reported with its reason; a damaged gene-name index leaves the Genes
track loaded but reports that symbol search is unavailable.

## Assembly recognition

A loaded reference (FASTA index or alignment header) is matched to a bundle by, in order of
strength: an explicit genome-configuration identity; the SHA-256 of the whole ordered sequence
dictionary (`dictionary_sha256` or one of `compatible_dictionary_sha256`); sequence MD5
fingerprints; or — the fallback for references with extra sequences — the build's **primary
chromosomes**. The primary-contig rule recognises a build when every nuclear primary chromosome is
present exactly once, under its catalogued name or its `chr`-prefix/mitochondrial spelling (`chr1`
or `1`, `chrM`, `MT` or `M`), with exactly the catalogued length. Sequences the build does not
catalogue (ERCC spike-ins, phiX, EBV, decoys such as `hs37d5`, differently named unplaced contigs)
are counted as extra contigs and never change the identity; the assembly is then shown as, for
example, `GRCh38 (+ 98 extra contigs)` with confidence *inferred from primary contigs*. It refuses
(never guesses) when a nuclear chromosome is missing or has another length, when a catalogued
sequence of the same name has another length, when one chromosome appears under two spellings,
when sequence MD5s contradict the build, or when two bundles fit equally. The mitochondrial genome
is optional, and one of another length is left unbound and noted in the reason.
