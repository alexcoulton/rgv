# Manual fixtures for IGV

Each fixture folder collects its RGV session resources as relative symbolic
links. Load the FASTA first with **Genomes → Load Genome from File**, then load
the BAM/CRAM and annotation files from the same folder.

From the repository root, the whole collection is available through the
`igv-manual-fixtures` folder link.

The `session.rgv.json` link is included for provenance; IGV does not open RGV
session files. Index sidecars are linked beside their data files so IGV finds
them automatically.

| Fixture | IGV locus | Reference |
| --- | --- | --- |
| `mf-01-alignment-core` | `chr1:2,590,001-2,650,000` | IGV built-in GRCh38; synthetic reads are not reference-matched |
| `mf-02-alignment-events` | `chrVariants:9,401-12,600` | `variants.fa` |
| `mf-03-rna-sashimi` | `chrManual:19,901-21,100` | `manual.fa` |
| `mf-04-bisulfite` | `chrManual:901-1,200` | `manual.fa` |
| `mf-05-smrt-kinetics` | `chrManual:29,901-30,600` | `manual.fa` |
| `mf-06-sbx` | `chrManual:39,901-40,600` | `manual.fa` |
| `mf-07-ultima` | `chrManual:49,901-50,600` | `manual.fa` |
| `mf-08-single-cell` | `chrManual:4,901-5,400` | `manual.fa` |
| `mf-09-cohort-variants` | `chrVariants:14,901-18,100` | `variants.fa` |
| `mf-10-mixed-tracks` | `chrVariants:8,501-16,000` | `variants.fa` |
| `mf-11-genome-review` | `chrVariants:8,501-12,500` | `variants.fa` |
| `mf-12-eight-track` | `chr1:900,001-920,000` or `chr2:1-200,000` | IGV built-in GRCh38; synthetic reads are not reference-matched |
| `mf-12-failure-isolation` | `chr1:900,001-920,000` or `chr2:1-200,000` | IGV built-in GRCh38; includes the intentionally missing BAM link |
| `mf-12-four-track` | `chr1:900,001-920,000` or `chr2:1-200,000` | IGV built-in GRCh38; synthetic reads are not reference-matched |
| `mf-12-workspaces` | `chr1:900,001-920,000` or `chr2:1-200,000` | IGV built-in GRCh38; synthetic reads are not reference-matched |
| `mf-13-recovery` | `chr1:900,001-920,000` or `chr2:1-200,000` | IGV built-in GRCh38; synthetic reads are not reference-matched |
| `mf-14-reference-cram` | `chrManual:901-5,400` | `manual.fa`; load it before the CRAM |

Regenerate missing fixture folders and links from the repository root with:

```bash
./benchmark-data/manual-suite/build-igv-links.sh
```
