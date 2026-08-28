# Portable genome configuration

RGV genome configuration files group a reference FASTA, display name, explicit assembly identity,
contig aliases, optional cytobands, default annotation tracks, and asset provenance. The current
schema version is 1.

```json
{
  "schema_version": 1,
  "name": "Example assembly",
  "assembly_identity": {
    "stable_id": "GRCh38.no_alt_analysis_set",
    "display_name": "GRCh38",
    "confidence": "explicit",
    "provenance": "Confirmed from the bundle manifest and reference fingerprints.",
    "matched_fingerprints": 3,
    "expected_primary_contigs": 25
  },
  "reference": { "Local": { "path": "reference/genome.fa" } },
  "cytobands": { "Local": { "path": "metadata/cytobands.tsv" } },
  "contig_aliases": {
    "1": "chr1",
    "MT": "chrM"
  },
  "default_annotations": [
    {
      "name": "Genes",
      "kind": "GeneModel",
      "source": { "Remote": { "url": "https://example.org/genes.bb" } },
      "format": "big_gene_pred"
    },
    {
      "name": "Clinically reviewed variants",
      "kind": "Variant",
      "source": { "Local": { "path": "annotations/reviewed.bcf" } }
    }
  ],
  "asset_provenance": [
    {
      "role": "cytobands",
      "source": "https://example.org/cytobands.tsv",
      "version": "2026-01",
      "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "license": "Redistribution terms or a link to them."
    }
  ]
}
```

Local relative paths are resolved from the configuration file's directory, so the containing
directory can be moved as one bundle. `Remote` sources use the same `{ "Remote": { "url": "…" } }`
form as sessions. Set `format` to `big_gene_pred` or `big_narrow_peak` when a generic `.bb` path
does not identify its binary schema. Cytobands are currently local UCSC-format TSV files.

Aliases are exact and case-sensitive. Each value must name a canonical FASTA contig. RGV does not
silently add or remove `chr`; an alias that conflicts with a different canonical contig is rejected.
Command-line aliases use repeatable `--contig-alias ALIAS=CANONICAL` arguments and must agree with
the configuration.

Open a bundle with:

```sh
rgv-desktop --genome-config genomes/example/genome.rgvgenome --locus 1:100001-110000
```

The **Genomes → Open genome configuration** action accepts `.rgvgenome` and `.json` files. Saved
RGV sessions retain the resolved genome name, stable identity and evidence, FASTA, cytobands,
aliases, and loaded default tracks. A partial reference is labelled as a subset; alignment-header
contig names alone never cause RGV to claim an assembly. The Genomes menu and MCP workspace output
separately report reference readiness, reference contig count, cytobands, aliases, and gene tracks.

The real ONT parity bundle is [`validation/ont-grch38-subset.rgvgenome`](../validation/ont-grch38-subset.rgvgenome).
It intentionally identifies a three-contig GRCh38 reference subset and records exact provenance for
its UCSC cytoband and MANE assets.

See [Curated gene landmark bundles](LANDMARK_BUNDLES.md) for the Python curation workflow, the local
IGV-compatible hg38, hg19, mm10, mm39, and hs1 resources, and the dataset-specific GENCODE example.
