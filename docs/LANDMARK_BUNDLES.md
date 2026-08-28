# Curated gene landmark bundles

RGV keeps lightweight gene-context resources locally and will distribute the finished resources,
not the curation program. The offline curation workflow is a standard-library Python script. It
mirrors the annotation choices in IGV's official genome definitions and records the source URL,
size, and SHA-256 digest for every downloaded or derived file.

The initial catalogue contains the five current human and mouse builds discussed for RGV:

| Build | Assembly | Compact landmark source | Detailed source |
| --- | --- | --- | --- |
| `hg38` | GRCh38 | IGV MANE BigGenePred | IGV RefSeq All |
| `hg19` | GRCh37 | IGV RefSeq Select BigGenePred | IGV RefSeq All |
| `mm10` | GRCm38 | Python-selected representative per gene | IGV RefSeq All |
| `mm39` | GRCm39 | Python-selected representative per gene | IGV RefSeq Curated |
| `hs1` | T2T CHM13 v2.0 | UCSC RefSeq Curated BigGenePred | IGV RefSeq All BigGenePred |

Each directory also contains IGV's genome definition, chromosome sizes, aliases and cytobands when
the preset supplies them. IGV Trix search files are retained where available. The bundles do not
include multi-gigabyte FASTA or 2bit sequences; their manifests preserve the matching reference URL.

For mouse tables, representative selection prefers `NM_`, `NR_`, `XM_`, then `XR_` accessions,
followed by coding length, spliced length, genomic span, and stable accession. Exon arrays and all
coordinates are validated against IGV's chromosome-size file. The unfiltered source table is kept
beside the compact derived track.

## Build or refresh the local catalogue

```sh
scripts/build-landmark-bundles.sh
```

Build only selected assemblies with repeatable `--build` arguments:

```sh
scripts/build-landmark-bundles.sh --build hg19 --build mm39
```

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
catalogue above.
