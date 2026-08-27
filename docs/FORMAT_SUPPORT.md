# RGV format support matrix

Status date: 23 August 2026

This matrix records implemented semantics, not merely recognized filename extensions. **Complete**
means that decoding, bounded query behavior, layout, interaction, inspection, and project restore
are implemented where the format requires them. Broad release qualification is still scheduled for
Phase 9 of `plans/RGV_IGV_PARITY_ROADMAP.md`.

| Data family | Formats | Implementation | Access and important limits |
| --- | --- | --- | --- |
| Alignments | BAM/BAI or CSI; CRAM/CRAI | Complete | Indexed local and HTTP/S from the desktop URL dialog, CLI, automation, or projects; per-source custom indexes, bounded records, caches, reference decoding, and cancellation |
| Reference | FASTA/FAI | Complete | Indexed local and HTTP/S from the desktop URL dialog, CLI, automation, or projects; custom FAI persistence, dictionary validation, and bounded slices |
| General annotation | BED/BED12, GFF3, GTF | Complete | Bounded local plain/gzip text index |
| Transcript annotation | genePred, extended genePred, refFlat | Complete | Bounded local plain/gzip text index; typed exons and CDS |
| Indexed annotation | generic BigBed | Complete | Indexed local and HTTP/S from the file or URL dialogs, on-demand |
| Indexed transcripts | bigGenePred BED12+8 | Complete | Indexed local and HTTP/S; typed schema, on-demand; explicit format retained in projects |
| Peaks | narrowPeak BED6+4 | Complete | Bounded local plain/gzip text index; typed statistics and summit |
| Indexed peaks | bigNarrowPeak BED6+4 | Complete | Indexed local and HTTP/S; typed schema, on-demand; explicit format retained in projects |
| Variants | VCF, bgzip VCF, BCF | Complete | Indexed local and HTTP/S from the file or URL dialogs using a conventional adjacent index; typed variants, genotypes, search, filters, evidence, and export |
| Quantitative | BigWig, TDF, bedGraph, fixedStep/variableStep WIG | Complete | BigWig and TDF are indexed local/HTTP sources reachable from the desktop; text inputs are bounded local sources |
| Interactions | BEDPE, UCSC interact | Complete | Bounded local text sources with endpoint indexing |
| Copy number | SEG/CBS | Complete | Bounded local text source with typed samples and segments |
| Somatic mutations | GDC/TCGA Mutation Annotation Format | Complete | Bounded local plain/gzip text source |
| Additional annotation | broadPeak, nucleotide PSL | Complete | Bounded local plain/gzip text index; typed peak statistics or target alignment blocks |
| Association summary | `.linear`, `.logistic`, `.assoc`, `.qassoc`, `.gwas` | Complete | Bounded local plain/gzip source; flexible IGV/PLINK headers, Manhattan layout, exact fields, search, navigation, and persisted display |
| Archived quantitative | TDF versions 1–4 | Complete | Indexed local/HTTP range access; compressed and uncompressed fixedStep, variableStep, BED, and named-BED tiles; bounded adaptive resolution; exact channel/dataset provenance |

The following are deliberately outside the current beta boundary rather than accidentally omitted:
Hi-C matrices/circular views, multiple-alignment MAF, RNA secondary-structure and chemical-
reactivity tracks, Birdsuite/GISTIC/LOH specialist formats, hosted catalogues, BLAT or motif-search
services, and igvtools transformations.

For custom BigBed files ending in `.bb` or `.bigBed`, use `--big-gene-pred` or
`--big-narrow-peak` when the schema is known. Generic BigBed intentionally does not guess a custom
schema from its contents. A saved RGV project retains the explicit schema.

Every embedded channel in a multi-channel TDF opens as an independent quantitative track in source
order. The tracks share one indexed reader and bounded tile cache, while exact channel indices and
names survive project save/restore. The inspector and structured export report the selected channel
and total channel count; distinct signals are never averaged or merged.
