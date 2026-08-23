# Portable genome configuration

RGV genome configuration files group a reference FASTA, display name, explicit contig aliases,
optional cytobands, and default annotation tracks. The current schema version is 1.

```json
{
  "schema_version": 1,
  "name": "Example assembly",
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
      "source": { "Local": { "path": "annotations/genes.gtf.gz" } }
    },
    {
      "name": "Clinically reviewed variants",
      "kind": "Variant",
      "source": { "Local": { "path": "annotations/reviewed.bcf" } }
    }
  ]
}
```

Local relative paths are resolved from the configuration file's directory, so the containing
directory can be moved as one bundle. `Remote` sources use the same `{ "Remote": { "url": "…" } }`
form as sessions. Cytobands are currently local UCSC-format TSV files.

Aliases are exact and case-sensitive. Each value must name a canonical FASTA contig. RGV does not
silently add or remove `chr`; an alias that conflicts with a different canonical contig is rejected.
Command-line aliases use repeatable `--contig-alias ALIAS=CANONICAL` arguments and must agree with
the configuration.

Open a bundle with:

```sh
rgv-desktop --genome-config genomes/example/genome.rgvgenome --locus 1:100001-110000
```

The **Genomes → Open genome configuration** action accepts `.rgvgenome` and `.json` files. Saved
RGV sessions retain the resolved genome name, FASTA, cytobands, aliases, and loaded default tracks.
