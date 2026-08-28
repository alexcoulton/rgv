# Format-Agnostic Inspector Redesign

## Objective

Redesign RGV's Properties panel as a format-agnostic genomic inspector that presents a readable semantic summary for recognised data while preserving every available field from BAM, VCF, GTF, BED, quantitative, interaction, mutation, and unknown or custom sources.

The work should be delivered incrementally: introduce a general inspector model first, prove it with GENCODE/features, and then migrate the remaining selection types while preserving automation and copy/export behaviour.

## Current constraint

Today, each inspector produces a flat `Vec<(String, String)>`, and `apps/rgv-desktop/src/ui/inspectors.rs` renders separate, largely duplicated UI branches. The same flattened representation is consumed by automation, so it cannot simply be replaced without a compatibility layer.

The redesign must separate three responsibilities:

1. Source parsing preserves the complete evidence supplied by each file format.
2. Inspector adapters translate selections into a normalized presentation document.
3. The Properties panel renders that document without containing file-format-specific logic.

## Design principles

- Lead with biological or analytical meaning, not source-column order.
- Preserve all source data, including unknown attributes and repeated keys.
- Use progressive disclosure: summary first, details and raw fields underneath.
- Represent values according to their meaning instead of treating everything as plain text.
- Keep copy, export, navigation, review, and automation behaviour intact.
- Bound or virtualize large collections without silently discarding information.
- Provide a useful fallback for data RGV does not understand semantically.
- Keep the panel responsive across its supported width and interface scale range.

## 1. Define a normalized inspector document

Add a presentation model under `apps/rgv-desktop/src/inspection/`, separate from parsed BAM, VCF, GTF, and other source records.

Conceptually, the model should resemble:

```rust
InspectorDocument {
    header,
    sections,
    actions,
    raw_fields,
}

InspectorHeader {
    title,
    subtitle,
    badges,
    location,
    source,
}

InspectorSection {
    title,
    priority,
    initially_expanded,
    fields,
}

InspectorField {
    label,
    value: InspectorValue,
    copy_value,
    provenance,
}
```

`InspectorValue` should cover at least:

- Plain text
- Identifier
- Genomic location
- Number and measurement
- Status
- Tags
- List
- Sequence
- Table
- Missing value
- Raw key/value

Repeated attributes must remain repeated or become an ordered list; they must not be collapsed into a map.

The document should distinguish display text from the complete copy value so abbreviated or wrapped presentation never changes copied evidence.

## 2. Build shared rendering primitives

Refactor `apps/rgv-desktop/src/ui/inspectors.rs` around one `show_inspector_document` renderer.

Create reusable components for:

- Semantic title and subtitle
- Type and status badges
- Compact genomic location row
- Responsive label/value grid
- Identifier fields with copy affordances
- Tag chips
- Collapsible sections
- Raw-attribute presentation
- Tables with bounded or virtualized rows
- Empty, missing, warning, and truncated states
- Sticky or consistently positioned action area

The renderer should adapt across the existing 280–520 point panel width. It should use normal UI typography for labels and prose, reserving monospace for coordinates, stable identifiers, CIGAR strings, tags, and sequences.

The visual treatment should rely on hierarchy, spacing, and subtle dividers rather than a heavy group outline around every inspector.

## 3. Implement feature inspection as the first vertical slice

Replace `feature_inspector_fields` in `apps/rgv-desktop/src/inspection/auxiliary.rs` with a feature document adapter.

It must handle all `FeatureRecord` sources, not just GTF:

- GTF and GFF gene models
- BED and BED12
- genePred and refFlat
- narrowPeak and broadPeak
- PSL
- Unknown or custom attributes

For GENCODE, promote recognised fields into semantic sections:

- Gene
- Transcript
- Exon, CDS, or UTR context
- Biotype
- Transcript support level
- Stable identifiers
- Tags
- Source and provenance

For example, the current GENCODE selection should present approximately:

```text
STK40
UTR · STK40-203 · exon 11
chr1:36,339,624–36,341,973 · − strand · 2,350 bp

Gene
STK40                         ENSG00000196182.10

Transcript
STK40-203                     ENST00000373130.7
Protein coding · support level 1

Tags
[Basic] [CCDS] [APPRIS principal]
```

Preserve every supplied attribute in a collapsed **Raw attributes** section. Unknown features should still receive a useful generic title, location, score, blocks, source, and raw fields.

This phase establishes the visual design using the current `STK40` example before applying it to more complex inspectors.

## 4. Migrate core alignment inspectors

Add document adapters for:

- Read or alignment segment
- Read event
- Supplementary alignment chain
- Pileup position
- Modification frequency
- Splice junction

Preserve specialized content and actions such as:

- CIGAR display and copying
- Read and event sequence copying
- Auxiliary SAM tags
- Mate navigation
- Supplementary-chain navigation and diagrams
- Moving the selected read to the top
- Junction BED export
- Sashimi PNG and SVG export
- Visible reference and consensus copying

These should be represented as typed sections and declared actions rather than separate bespoke panel containers.

## 5. Migrate variants and auxiliary-track inspectors

Migrate the remaining inspector types in this order:

1. VCF variants and genotype tables
2. Quantitative tracks
3. GWAS
4. Copy number
5. Mutations and MAF annotations
6. Interactions

VCF requires particular care because INFO fields, alternate alleles, consequences, and sample genotypes can be arbitrarily large. Preserve the current sample search and export controls, virtualize large sample tables, and show explicit truncation and provenance notices.

Quantitative and derived-value inspectors must clearly distinguish source values from displayed aggregates or arithmetic results.

Mutation and GWAS adapters must preserve arbitrary source columns without allowing them to overwhelm the summary.

## 6. Guarantee a generic fallback

Every inspector must be constructible even when RGV understands none of the source-specific attributes.

The fallback presentation should include:

- Best available title
- Record or selection type
- Track and source
- Genomic location, when available
- Known core fields
- All bounded raw fields
- Parsing and truncation provenance
- Copy details action

No supported file should produce an empty panel merely because it lacks a semantic adapter.

Missing optional values should generally be omitted from the summary rather than filling the panel with “not supplied.” Missingness that affects interpretation should remain visible as a status or warning.

## 7. Preserve compatibility

Automation in `apps/rgv-desktop/src/automation/commands.rs` currently exposes flattened fields.

During this work:

- Keep automation schema version 4 unchanged.
- Derive the existing flattened field list from each `InspectorDocument`.
- Preserve labels, values, and ordering where compatibility requires it.
- Keep copy and export output stable where practical.
- Preserve review-queue actions and retained-evidence snapshots.
- Preserve complete underlying values when their on-screen presentation is abbreviated.
- Consider a future automation schema version for structured inspector sections, but do not make that a dependency of the UI redesign.

The initial model migration should therefore be capable of reproducing the existing flat field output exactly before any visible redesign is enabled.

## 8. Performance and bounds

The inspector must remain responsive for pathological but valid inputs.

- Do not eagerly duplicate very large values solely for presentation.
- Virtualize large genotype and sample tables.
- Bound raw attribute presentation using existing resource policies.
- Surface every truncation or omission explicitly.
- Retain a complete copy/export path when the underlying bounded source is available.
- Preserve duplicate keys and source order where they carry meaning.
- Give expandable sections stable identities so scrolling and expansion do not jump as the UI redraws.

## 9. Accessibility and interaction

- Support keyboard focus and activation for sections and actions.
- Provide accessible labels for icon-only copy, close, and navigation controls.
- Maintain sufficient contrast for muted labels, badges, warnings, and selected states.
- Use consistent minimum pointer targets.
- Ensure long identifiers can be read and copied without requiring precise horizontal scrolling.
- Keep the current resizable and collapsible panel behaviour.
- Place “Clear selection” in a consistent, low-emphasis location rather than among primary evidence actions.

## 10. Validation

### Unit coverage

Add adapter and renderer-model tests for:

- GTF attributes with duplicate `tag` keys
- GTF gene, transcript, exon, CDS, and UTR records
- Missing optional fields
- BED12 blocks
- genePred and refFlat records
- Peak statistics
- PSL alignment blocks
- Unknown attributes
- Very long values and identifiers
- Bounded sequences and CIGAR strings
- BAM auxiliary tags
- VCF custom INFO fields
- Multi-allelic variants
- Large VCF sample collections
- Quantitative source versus rendered values
- Mutation custom annotations
- Truncation and provenance
- Flattened automation compatibility

### Visual and interaction coverage

Verify the panel at:

- 280, 340, and 520 point widths
- Supported interface scale extremes
- Retina and standard logical scaling
- Short and extremely long identifiers
- Empty, typical, and information-dense selections
- GENCODE, BAM, VCF, BED, quantitative, mutation, and interaction examples

Verify that:

- Expansion state remains stable while the viewer redraws.
- Actions remain visible and correctly enabled.
- Copy actions return complete values.
- Large sections scroll independently or within the panel as designed.
- Switching rapidly between selection types does not retain stale content or controls.
- Automation `describe_selection` output remains compatible.

## Suggested delivery sequence

### Patch 1: Inspector document foundation

- Add `InspectorDocument`, typed values, sections, actions, and flattening.
- Adapt existing snapshots to produce documents while continuing to render the legacy UI.
- Prove that flattened output matches current automation fields.
- No intended visible change.

### Patch 2: Shared renderer and feature vertical slice

- Add the shared document renderer and design tokens.
- Migrate feature inspection, including GENCODE and generic feature fallbacks.
- Validate the design at all panel widths.
- Keep a raw attributes section for complete evidence access.

### Patch 3: Core alignment inspection

- Migrate reads, events, pileups, modifications, junctions, and supplementary chains.
- Preserve all navigation, copy, and export actions.

### Patch 4: Variants and auxiliary tracks

- Migrate VCF and genotype inspection.
- Migrate quantitative, GWAS, copy-number, mutation, and interaction inspectors.
- Add virtualization and source-specific bounds where required.

### Patch 5: Consolidation and polish

- Remove legacy `PropertySection` rendering branches.
- Remove duplicated label/value and action layouts.
- Complete accessibility and interaction polish.
- Run the full automated and manual visual validation matrix.

## Completion criteria

The work is complete when:

- Every existing selection type uses the shared inspector renderer.
- Recognised formats receive a useful semantic summary.
- Arbitrary and unknown fields remain accessible through a generic fallback.
- Repeated attributes and source ordering are preserved where meaningful.
- Missing, truncated, inferred, and aggregated values are represented honestly.
- Existing copy, export, navigation, review, and clear-selection actions still work.
- Automation schema version 4 output remains compatible.
- Large sample tables and attribute collections remain responsive.
- The panel is readable and usable throughout its supported width and interface scale range.
