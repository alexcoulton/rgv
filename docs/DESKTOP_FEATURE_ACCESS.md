# Desktop feature-access audit

Status date: 23 August 2026

This audit checks that the major workflows in `RGV_IGV_PARITY_ROADMAP.md` are reachable from the
native desktop application. It is intentionally about product access and wiring, not broad manual
acceptance or performance tuning. `RGV_BETA_EXECUTION_PLAN.md` defines the current execution order.

## Loading and workspace construction

| Capability | Desktop entry point | Persistence or intentional exception | Audit |
| --- | --- | --- | --- |
| Local BAM/CRAM | **File → Open BAM/CRAM**, command search, or drop a file | Ordered alignment tracks persist in the project | Connected |
| Remote BAM/CRAM | **File → Open URL → BAM or CRAM** | Per-alignment custom BAI/CSI/CRAI URL persists after credential stripping | Corrected in this audit |
| Local FASTA | **Genomes → Open FASTA** or drop a file | Reference source and display options persist | Connected |
| Remote FASTA | **File → Open URL → Reference FASTA** | Custom FAI URL persists after credential stripping | Corrected in this audit |
| CRAM plus remote reference | **File → Open URL → BAM or CRAM**, then fill the optional reference fields | Alignment index, reference, and FAI persist | Corrected in this audit |
| Local auxiliary track | **Tracks → Add track** or drop a file | Typed source, format, order, appearance, and sample association persist | Connected; compressed BED/GFF/GTF corrected in this audit |
| Remote indexed auxiliary track | **File → Open URL → Indexed genomic track** | VCF/BCF, BigWig, TDF, BigBed, bigGenePred, and bigNarrowPeak persist; signed queries stay out of labels/status | Corrected in this audit |
| Portable genome | **Genomes → Open genome configuration** or drop `.rgvgenome` | FASTA, aliases, cytobands, and default annotations restore | Connected |
| Native or IGV project | **File → Open session** or drop JSON/XML | Native schema migration and explicit IGV import report | Connected |
| Multiple loci | Enter several loci in the navigation field | Ordered panels, active panel, and per-panel scroll persist | Connected |

## Alignment review

Alignment settings are reachable from the **Tracks** menu, a track's gutter menu, the selected-track
keyboard action, or command search. Clicking visible evidence opens the appropriate inspector.

| Capability | Desktop control or action | Audit |
| --- | --- | --- |
| Reference, coverage, junction/Sashimi, and read subtracks | Alignment track settings | Connected |
| Expanded/squished/collapsed and continuous vertical density | Alignment settings and right overview | Connected |
| Grouping, sorting, direction, pairing, template bounds, and tag expressions | Alignment track settings | Connected |
| Base display, quality shading, indel/label thresholds, modified-base visibility and legend | Alignment track settings | Connected |
| Bisulfite contexts, PacBio kinetics, SBX, and Ultima evidence | Alignment colour-mode selector plus mode-specific controls and copyable legends | Connected |
| Read, CIGAR event, pileup, modification, and junction inspection | Click or configured hover on semantic evidence | Connected |
| Consensus, reference, read, CIGAR, tags, and event sequence copy | Read and pileup inspectors | Connected |
| Mate and supplementary-chain navigation | Read/chain inspector actions, including linked panels | Connected |
| Several alignment samples | Repeated open/drop/URL actions; gutter selection and metadata bulk actions | Connected |

## Typed auxiliary tracks

Every auxiliary track can be selected from its visible geometry or gutter. Its gutter menu and
selected-track action open the settings window; previous/next evidence uses the shared keyboard or
command-search action.

| Workflow | Desktop path | Audit |
| --- | --- | --- |
| VCF/BCF and genotypes | Add track or indexed URL; variant settings; record/genotype inspector; evidence and breakend actions | Connected |
| RNA and Sashimi | Alignment settings; junction inspector; BED/PNG/SVG actions | Connected |
| BigWig/TDF/bedGraph/WIG | Add track or URL where indexed; quantitative settings and inspector | Connected |
| Arithmetic, shared scales, overlays | Quantitative settings or sample-metadata bulk actions | Connected |
| SEG/CBS copy number and MAF mutations | Add track; matrix settings; exact sample/record inspector | Connected |
| BEDPE/interact | Add track; arc settings; endpoint inspector and linked-panel actions | Connected |
| BED/GFF/GTF/genePred/refFlat/peaks/PSL/BigBed | Add track or indexed URL; feature display settings and inspector | Connected |
| GWAS association summary | Add track; Manhattan settings; point inspector and identifier navigation | Connected |
| Multi-channel TDF | Add local/remote TDF once; asynchronous channels become separate quantitative tracks | Connected |

## Navigation, review, projects, and product controls

| Capability | Desktop entry point | Audit |
| --- | --- | --- |
| Coordinate, annotation-name, and variant-ID search | Navigation field and bounded result popup | Connected |
| Previous/next annotation or loaded evidence | **Regions** menu, shortcuts, and command search | Connected |
| Reverse reference and six-frame translation | Reference track settings | Connected |
| Regions of interest | **Regions → Manage regions** | Connected |
| Review queue | **Regions → Review queue** | Connected |
| Sample metadata and bulk track actions | **View → Sample metadata** or **Tracks → Import sample attributes** | Connected |
| Track visibility, collapse, order, height, appearance, and removal | Gutter controls/menus, settings, shortcuts, and metadata bulk actions | Connected |
| Save/save-as and portable relocation | **File → Save project** | Connected |
| Autosave and recovery | **Settings**; recovery chooser appears after an unclean exit | Connected |
| PNG and semantic evidence exports | **File**, gutter menus, inspectors, ROI/review windows | Connected |
| Accessibility preferences and keyboard operation | **View → Settings**, shortcuts, focusable custom controls, command search | Connected |

## Intentional non-dialog entry points

These are product features but are intentionally opt-in operational interfaces rather than
ordinary point-and-click review actions:

- deterministic batch automation uses `--batch` and its versioned JSON commands;
- authenticated local external control uses `--control-port`, a secret file, and an explicit
  filesystem root;
- full structured evidence families and deterministic visible/full-panel snapshot scopes are
  available through automation, while the common interactive exports remain in inspectors and
  menus; and
- bearer authentication uses `RGV_REMOTE_BEARER_TOKEN`, so credentials are not written into a
  project or editable URL field.

These exceptions are documented in the user guide and do not hide an ordinary genome-review
workflow behind an internal API.

## Close gate

The inventory and handoff gate passed on 23 August 2026: formatting, all 505 active workspace tests,
strict all-feature Clippy, the optimized desktop build, and the pinned synthetic validation corpus
passed. The mixed-format handoff project additionally opens the checked-in alignment/reference
truth and 19 auxiliary tracks, and its generator verifies a non-empty production-engine query from
every track. The release desktop restored that project through Metal with all sources healthy,
expanded both TDF channels, settled, exported its track inventory and SVG, and exited. Broad visual
and ordinary-use acceptance now belongs to the user's manual pass rather than this source audit.
