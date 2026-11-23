# RGV format support matrix

Status date: 11 November 2026

This matrix records implemented semantics, not merely recognized filename extensions. **Complete**
means that decoding, bounded query behavior, layout, interaction, inspection, and project restore
are implemented where the format requires them. Broad release qualification is still scheduled for
Phase 9 of `plans/RGV_IGV_PARITY_ROADMAP.md`.

| Data family | Formats | Implementation | Access and important limits |
| --- | --- | --- | --- |
| Alignments | BAM/BAI or CSI | Complete (local) | Indexed local BAM; Downsampled or Complete read display, exact summaries, caches, and cancellation |
| Alignments | CRAM/CRAI; HTTP/S BAM and CRAM | Automated coverage only | Open through the unified alignment scene with the same read display, summaries and stable read identity as local BAM. CRAM requires its reference FASTA; without one the track reports that a reference is needed. Not yet hands-on tested in the desktop; mid-transfer cancellation of superseded remote requests is not connected |
| Reference | FASTA/FAI | Complete | Indexed local and HTTP/S from the desktop URL dialog, CLI, automation, or projects; custom FAI persistence, dictionary validation, and bounded slices |
| Reference | UCSC 2bit | Automated coverage only | Local and HTTP/S, read by range request behind the same interface as FASTA; unknown bases reported as `N`, soft-masking not reported. An optional `chrom.sizes` companion avoids one read per sequence at open |
| Genome | IGV genome JSON and hosted genome list | Automated coverage only | Genomes → Hosted genomes, Open genome configuration, drag-and-drop, or `--genome <id, path or URL>`. Reference, cytobands, alias table and default annotation tracks are honoured; small resources are cached for offline reuse. `chromosomeOrder`, `hubs` and GenBank are not used |
| General annotation | BED/BED12, GFF3, GTF | Complete (local); remote by whole-file download | Local plain/gzip text loaded into a bounded in-memory index (2,000,000 input lines), or bgzip with an adjacent tabix/CSI index read on demand with no row limit. HTTP/S URLs are accepted (see "Remote text annotations" below). GTF and GFF3 lines are assembled into one transcript model per transcript (GTF `transcript_id`; GFF3 `Parent`, several parents allowed), as IGV draws them: exons (or CDS and UTR lines) as blocks, CDS plus start/stop codons as the thick part, the transcript line's span, attributes, strand and score, labelled by gene name. Gene lines of drawn transcripts are not drawn but still answer name search with the whole gene; childless genes and lines of no transcript are drawn as single features. An indexed read is widened (by up to 4 Mb) to the full extent of the transcripts it meets |
| Transcript annotation | genePred, extended genePred, refFlat | Complete (local); remote by whole-file download | Bounded plain/gzip text index; typed exons and CDS. HTTP/S URLs are accepted (see "Remote text annotations" below) |
| Indexed annotation | generic BigBed | Complete | Indexed local and HTTP/S from the file or URL dialogs, on-demand |
| Indexed transcripts | bigGenePred BED12+8 | Complete | Indexed local and HTTP/S; typed schema, on-demand; explicit format retained in projects |
| Peaks | narrowPeak BED6+4 | Complete (local); remote by whole-file download | Bounded plain/gzip text index; typed statistics and summit. HTTP/S URLs are accepted (see "Remote text annotations" below) |
| Indexed peaks | bigNarrowPeak BED6+4 | Complete | Indexed local and HTTP/S; typed schema, on-demand; explicit format retained in projects |
| Variants | VCF, bgzip VCF, BCF | Complete | Indexed local and HTTP/S from the file or URL dialogs using a conventional adjacent index; typed variants, genotypes, search, filters, evidence, and export |
| Quantitative | BigWig, TDF, bedGraph, fixedStep/variableStep WIG | Complete | BigWig and TDF are indexed local/HTTP sources reachable from the desktop; wide BigWig views read the file's precomputed zoom levels; text inputs are bounded local sources |
| Interactions | BEDPE, UCSC interact | Complete | Bounded local text sources with endpoint indexing |
| Copy number | SEG/CBS | Complete | Bounded local text source with typed samples and segments |
| Somatic mutations | GDC/TCGA Mutation Annotation Format | Complete | Bounded local plain/gzip text source |
| Additional annotation | broadPeak, nucleotide PSL | Complete (local); remote by whole-file download | Bounded plain/gzip text index; typed peak statistics or target alignment blocks. HTTP/S URLs are accepted (see "Remote text annotations" below) |
| Association summary | `.linear`, `.logistic`, `.assoc`, `.qassoc`, `.gwas` | Complete | Bounded local plain/gzip source; flexible IGV/PLINK headers, Manhattan layout, exact fields, search, navigation, and persisted display |
| Archived quantitative | TDF versions 1–4 | Complete | Indexed local/HTTP range access; compressed and uncompressed fixedStep, variableStep, BED, and named-BED tiles; bounded adaptive resolution; exact channel/dataset provenance |

The following are deliberately outside the current beta boundary rather than accidentally omitted:
Hi-C matrices/circular views, multiple-alignment MAF, RNA secondary-structure and chemical-
reactivity tracks, Birdsuite/GISTIC/LOH specialist formats, hosted *track* catalogues and UCSC
track hubs, BLAT or motif-search services, and igvtools transformations. (The hosted *genome* list
is implemented; see the Genome row above.)

## Remote text annotations

BED, GFF3, GTF, genePred, refFlat, narrowPeak, broadPeak, and PSL sources may be given as an
HTTP/S URL (File → Open URL, or a remote track source in a project). There is no remote random
access for these formats: RGV downloads the whole file, up to 256 MiB, into a local cache
directory (`RGV_REMOTE_TRACK_CACHE_DIR`, defaulting to `rgv-remote-tracks` under the system
temporary directory) and then reads it as a local file. A remote tabix/CSI index is not used, so a
remote bgzip file is decompressed and held in the bounded in-memory index like any other gzip
text. A file larger than 256 MiB fails to open. If a download fails for any reason and an earlier
copy of the same URL is still in the cache, that earlier copy is used.

The other text families in the table — bedGraph/WIG, BEDPE/interact, SEG/CBS, mutation MAF, and
association summaries — are local-only: their readers have no URL path.

For custom BigBed files ending in `.bb` or `.bigBed`, use `--big-gene-pred` or
`--big-narrow-peak` when the schema is known. Generic BigBed intentionally does not guess a custom
schema from its contents. A saved RGV project retains the explicit schema.

Every embedded channel in a multi-channel TDF opens as an independent quantitative track in source
order. The tracks share one indexed reader and bounded tile cache, while exact channel indices and
names survive project save/restore. The inspector and structured export report the selected channel
and total channel count; distinct signals are never averaged or merged.
