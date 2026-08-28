# Gene-symbol and chromosome-context usability plan

Status: implemented and qualified

Plan date: 2026-08-28

Depends on:

- [Built-in genome landmark integration](GENOME_LANDMARK_INTEGRATION_PLAN.md)
- [GUI redesign](GUI_REDESIGN_PLAN.md)
- [Workspace adaptive layout](WORKSPACE_ADAPTIVE_LAYOUT_PLAN.md)

## Objective

Make built-in gene landmarks and chromosome context communicate biological names and positions
directly, without requiring selection or asking the user to interpret database accessions.

The completed behavior should be:

1. Gene symbols are the primary visible names in the Genes track, inspector, and annotation search.
2. Gene labels appear automatically when the current screen geometry can accommodate them without
   crowding; selection keeps its label even when ordinary labels are suppressed.
3. Ensembl/RefSeq gene and transcript accessions remain available as clearly labelled secondary
   identifiers.
4. Cytoband ideograms show a recognizable centromere and expose band identity and coordinates on
   hover and through accessibility metadata.
5. The chromosome ideogram and the overview/navigation bar beneath it use one exact full-contig
   horizontal coordinate transform.

## Current implementation findings

### Gene identity is present but promoted too late

The curated representative-gene resources already contain the required information:

- extended genePred `name2` contains the display symbol;
- `names.rgv.tsv.gz` contains `display_symbol`, stable gene ID, transcript ID, coordinates, and
  strand; and
- the genePred parser preserves `name2` as the `gene_name` attribute.

The runtime loses that distinction in three places:

- `crates/rgv-hts/src/tracks.rs::parse_gene_pred` sets `FeatureRecord.name` to the transcript name
  for extended genePred even after parsing the human-readable `name2` value;
- `apps/rgv-desktop/src/workspace/landmarks.rs::enrich_builtin_feature_record` adds `gene_name` and
  stable IDs only to the cloned record used by the inspector, so the rendered query still carries
  the transcript ID; and
- landmark search merging skips a symbol-bearing result if the ordinary track search already
  returned the same coordinate interval. The earlier raw hit therefore keeps its Ensembl
  transcript name in the popup.

The inspector already prefers `gene_name` for its title, but the selected label in the track is
drawn from `FeatureRecord.name`. This explains why selecting a feature can reveal the symbol in the
properties panel while leaving an `ENST...` label on the track.

### Automatic feature labels exist, but their input and policy are insufficient

`crates/rgv-layout/src/track_layout.rs::layout_features` already performs per-row collision checks,
caps label count, and always preserves the selected label. Ordinary labels are currently enabled
only when the row height is at least 16 points and are taken directly from `FeatureRecord.name`.

That is not yet an adequate gene-label policy because:

- it does not distinguish a gene symbol from a transcript or database identifier;
- row height is used as a proxy for genomic zoom even though label feasibility depends on pixel
  span, panel width, text width, and nearby labels; and
- the selected label is the only reliable path when ordinary candidates are long Ensembl IDs or
  collide.

### Centromere data exists but is not presented as chromosome structure

The cytoband parser preserves `acen` as `CytobandStain::Centromere`, and the current painter renders
it in red. Every band is nevertheless a plain rectangle, so the `acen` pair reads as two coloured
bands rather than the constricted waist of a chromosome. The cytoband row has no band-specific
interaction; only wide bands receive permanent text.

### The two chromosome rows currently align, but the invariant is implicit

`GenomeContextMetrics::rows` gives the cytoband and overview rows the same left edge and width.
`layout_ideogram`, the visible-interval marker, and overview pointer navigation all map the full
contig linearly across that width. They therefore align in the current layout.

The contract is duplicated rather than encoded in one object, however: ideogram segments are laid
out using the overview width and then painted into the cytoband rectangle, while pointer mapping is
implemented separately in `ideogram_position_at_pointer`. A future padding, multi-panel, or scale
change could make them drift without a type or test failing.

## Product and data contract

### Primary and secondary gene identity

For records that provide the fields, use this presentation hierarchy:

1. **Gene symbol** — primary display label, search-result title, and inspector title, for example
   `ACTB`.
2. **Stable gene ID** — secondary inspector/search metadata, for example
   `ENSG00000075624.17`.
3. **Transcript ID** — secondary inspector/search metadata, for example
   `ENST00000674681.1`.
4. **Source record name or coordinate** — explicit fallback only when no symbol exists.

Do not discard or rewrite identifiers. A symbol is a presentation name, not a replacement stable
identity. Picking, deduplication, navigation, session state, automation, and export must continue to
use stable track/record identity and coordinates.

The same precedence must work for built-in extended genePred, refFlat, bigGenePred, GTF/GFF, and
ordinary user-loaded annotation tracks. Built-in metadata may be richer, but the display code must
not contain a one-off `ENST` string heuristic.

### Automatic gene-label policy

Labels should be driven by available screen space rather than a fixed genomic span alone:

- use the resolved gene symbol as the candidate text;
- measure the actual text width with the active font;
- require a usable label band in the rendered row;
- place labels near the visible feature midpoint, clipped to the panel;
- greedily accept labels in deterministic genomic order with a minimum horizontal gap;
- enforce the existing hard label budget;
- suppress ordinary labels when the visible candidates cannot be presented legibly; and
- always allow the selected gene label, resolving a collision in its favour.

The policy may use bases-per-pixel as an early coarse gate, but the final decision must be based on
pixel geometry so it behaves consistently across window sizes and multi-panel layouts. Labels must
not alter feature row assignment or picking identity.

### Chromosome coordinate contract

Introduce one immutable full-contig horizontal mapping for the chromosome context rows:

```text
ContigAxis {
    contig_length,
    left,
    width
}

position -> x
interval -> [x0, x1]
x -> position
```

The cytoband silhouette, band hit-testing, overview density bins, visible-interval handle, click to
navigate, drag to pan/zoom, hover crosshair, and accessibility bounds must all consume this same
mapping. Cytoband and overview rows may have different vertical rectangles, but their axis left and
right edges must be identical.

## Implementation plan

### Phase 0 — Lock representative regressions

1. Add compact extended-genePred fixtures containing a transcript ID, Ensembl gene ID, gene
   symbol, exons, and coding interval.
2. Add built-in search fixtures in which the ordinary feature index and landmark name index return
   the same gene at the same coordinates.
3. Add cytoband fixtures with p-arm bands, adjacent `acen` bands, q-arm bands, an acrocentric stalk,
   and a contig without cytobands.
4. Record current native screenshots at a readable gene scale and chromosome scale.

Exit criterion: tests reproduce the transcript-ID label/search result and prove that the existing
cytoband/overview rows currently share endpoints.

### Phase 1 — Establish one feature presentation identity

1. Add a format-neutral feature identity resolver, returning at least:
   `display_symbol`, `gene_id`, `transcript_id`, and fallback `record_name`.
2. Define attribute precedence centrally (`gene_name`, `gene_name2`, relevant `Name` aliases, then
   the source record name) and reuse it in layout, inspector, search, and automation presentation.
3. Correct extended genePred parsing so `name2`/`gene_name` is the display name when present while
   retaining the transcript name in `transcript_id`.
4. Keep generic BED/name semantics unchanged when the record has no gene-specific attributes.
5. Replace the selection-only semantic split: query records must already expose the correct
   display symbol. Keep selection-time landmark enrichment only for provenance and metadata absent
   from the source record.
6. If coordinate/transcript enrichment remains necessary, build a keyed lookup when loading the
   landmark index; do not linearly scan the entire index for every visible record.

Exit criterion: an unselected built-in ACTB/DAGLB feature has the symbol as its resolved display
label, with gene and transcript IDs still independently available.

### Phase 2 — Make search symbol-first and deduplicate semantically

1. Merge built-in landmark search metadata into a matching ordinary track hit instead of skipping
   it by coordinate.
2. Rank exact symbol matches first, followed by symbol prefixes, aliases, stable gene IDs, and
   transcript IDs. Preserve deterministic coordinate ordering for ties.
3. Present the symbol as the popup's primary text. Show stable gene/transcript ID as muted
   secondary text where useful, especially when that identifier was the matched query.
4. Ensure navigation status also leads with the symbol.
5. Deduplicate by stable feature identity (track plus gene/transcript identity and interval), not
   by display string. One biological result should not appear twice merely because it came from
   both indexes.
6. Preserve bounded search limits and truncated-result notices.

Exit criterion: searching `ACTB` shows `ACTB`, not an `ENST...` accession, and selecting it
navigates to the same representative model. Searching its stable ID still finds the gene and shows
the symbol as the primary result.

### Phase 3 — Enable adaptive automatic gene labels

1. Move label selection into a small deterministic layout helper that accepts resolved label text,
   feature pixel geometry, row, selection state, and the label budget.
2. Replace the row-height-only gate with the pixel-space policy above. Retain row height as a
   minimum vertical legibility condition.
3. Reserve label height only for rows that will actually show a label; do not push gene glyphs down
   at chromosome-scale views where every ordinary label is suppressed.
4. Give the selected label first priority, then fill remaining non-overlapping slots with ordinary
   symbols.
5. Keep labels non-pickable or explicitly map them to the same record object ID as the gene; do not
   create a second selection identity.
6. Expose the same labels in deterministic PNG/SVG captures and accessibility descriptions.

Exit criterion: symbols appear without clicking at legible scales, do not overlap, disappear
gracefully when the panel becomes crowded, and reappear deterministically after zooming back in.

### Phase 4 — Render chromosome structure and cytoband hover

1. Derive a typed centromere interval from contiguous `acen` bands. Keep the original individual
   band records for names and hover metadata.
2. Render rounded telomere ends and an unmistakable centromere waist. Represent the two `acen`
   sides as opposing tapered shapes clipped to the chromosome silhouette rather than ordinary
   rectangles.
3. Preserve the standard cytoband stain palette inside the silhouette, including variable and
   stalk regions.
4. Add one interaction region for the ideogram. Convert pointer x through the shared `ContigAxis`,
   then locate the containing band; this remains reliable for bands narrower than a comfortable
   widget hit target.
5. On hover, highlight the resolved band and show, for example:
   `chr7 · q22.1 · 98,300,001–101,800,000 · gpos50`.
6. Give the ideogram an accessibility role and band description. Keyboard focus should expose the
   chromosome/centromere summary even if fine-grained band traversal is deferred.
7. Fall back to the existing plain chromosome bar when cytobands or `acen` are absent; never invent
   a centromere coordinate.

Exit criterion: the centromere is recognizable without relying on colour, every loaded band is
discoverable by hover, and missing-band assemblies degrade explicitly.

### Phase 5 — Unify ideogram and overview navigation geometry

1. Construct `ContigAxis` once from the committed `GenomeContextRows` horizontal geometry.
2. Pass it to ideogram layout, overview density layout, visible-window layout, hover, click, drag,
   and keyboard/accessibility presentation.
3. Remove independent fraction calculations and any use of one row's width to paint into another
   row.
4. Optionally draw a subtle shared hover guide through both rows so the same genomic coordinate is
   visually obvious.
5. Add debug assertions that cytoband and overview horizontal bounds agree in every committed
   workspace layout.

Exit criterion: the same genomic position resolves to the same x coordinate in both rows after
window resize, Properties-panel resize, display scaling, and multi-panel reflow.

### Phase 6 — Qualify and document

1. Run HTS parser, landmark, layout, desktop interaction, automation, MCP, and snapshot suites.
2. Exercise the installed hg19, hg38, mm10, mm39, and hs1 bundles; record which primary
   chromosomes legitimately lack cytobands or `acen` data.
3. Verify symbol-first behavior with protein-coding and non-coding genes, duplicated symbols,
   aliases, and stable-ID searches.
4. Verify adaptive labels at narrow, medium, and chromosome-scale spans in one- and multi-panel
   layouts.
5. Capture native screenshots showing unselected labels, a selected gene inspector, symbol-first
   search, the centromere, and band hover.
6. Update the user guide with gene identity precedence, label-density behavior, cytoband hover, and
   the exact chromosome-axis contract.

Exit criterion: all acceptance criteria below pass in a release build with the built-in landmark
resources loaded automatically.

## Test matrix

### Gene identity and parsing

- Extended genePred with transcript ID plus symbol-bearing `name2`.
- refFlat, bigGenePred, GTF/GFF, and plain BED fallbacks.
- Missing/empty symbol with stable gene ID and transcript fallback.
- Duplicate symbols at different loci and multiple transcripts for one gene.
- Protein-coding and non-coding records with exon and UTR geometry unchanged.

### Search

- Exact symbol, case-insensitive symbol, prefix, synonym, gene ID, and transcript ID.
- Duplicate raw-track and landmark-index hits merge into one symbol-first result.
- Result ordering and truncation are deterministic.
- Navigation, status text, automation, and MCP agree on the selected symbol and locus.

### Label layout

- Automatic labels at a readable scale with no selection.
- Collision suppression for adjacent genes and deterministic recovery after zoom.
- Selected label wins a collision and survives narrow row height.
- Labels stay inside panel bounds after resizing and multi-panel reflow.
- Label count and text length remain bounded for pathological annotation inputs.

### Ideogram and coordinate mapping

- Two-band centromere, single/partial `acen` fallback, acrocentric stalk, and no-centromere data.
- Band hover at left/right edges and across subpixel-width bands.
- Full-contig endpoints map to identical cytoband and overview endpoints.
- Position-to-x-to-position round trips stay within one pixel's genomic span.
- Visible interval, click navigation, pan handle, and hover guide all use the same axis.
- Properties resize, display scale, accessibility scale, and multiple panels preserve alignment.

## Performance and safety constraints

- Do not regenerate or enlarge the installed bundles merely to fix presentation if their existing
  extended genePred and name index already contain the symbols.
- Add curation validation that representative records and search rows agree on symbol, stable IDs,
  transcript, and coordinates. Regenerate only bundles that fail this validation.
- Do not perform an `O(visible records × landmark index entries)` enrichment pass.
- Preserve existing search, feature-count, label, primitive, and decompression bounds.
- Cytoband hover must not create one persistent UI widget per band per frame; use the shared axis
  and a bounded lookup over the active contig.
- Never infer a symbol or centromere by parsing an accession string or guessing a coordinate.

## Acceptance criteria

This plan is complete when:

- clicking a built-in gene presents its gene symbol as the primary name and labels Ensembl/RefSeq
  identifiers separately;
- appropriate zoom levels show non-overlapping gene-symbol labels without requiring a click;
- search results and navigation status lead with the gene symbol, including when duplicate search
  providers return the same feature;
- stable IDs remain searchable, inspectable, exportable, and unchanged;
- the ideogram has a structurally recognizable centromere wherever `acen` data exists;
- hovering the ideogram reveals the correct band name, coordinates, and stain;
- the ideogram, overview data, visible-window marker, and pointer navigation share one tested
  full-contig axis; and
- resource, parser, layout, desktop, automation, MCP, accessibility, and native visual checks pass.

## Implementation and qualification record

Completed on 2026-08-28.

- A shared format-neutral presentation identity now resolves display symbol, stable gene ID,
  transcript ID, and source record name. Extended genePred, bigGenePred, GTF/GFF, refFlat, and
  ordinary BED fallbacks retain their distinct identity fields.
- Built-in and raw-track search results are merged by biological identity and interval. Exact and
  prefix symbol matches lead the ranking, aliases follow, and stable gene/transcript identifiers
  remain searchable as secondary metadata.
- Feature labels use measured font widths, deterministic collision placement, a hard label budget,
  and selected-label priority. The representative Genes track uses an expanded row so ordinary
  symbols have a legible label band without exposing every transcript by default.
- A shared `ContigAxis` now drives ideogram bands, centromere structure, hover lookup, overview
  bins, visible-window geometry, and navigation. Accessibility reports the chromosome and typed
  centromere summary.
- Exact-base automation selection recognizes settled auxiliary annotation snapshots. Selecting a
  gene therefore preserves the useful regional viewport instead of forcing an unnecessary
  one-base BAM query.
- Bundle curation validates representative genePred rows against the symbol search index by
  symbol, stable IDs, transcript, strand, and coordinates. `--validate-existing` also checks every
  declared asset's path, size, digest, and cytoband bounds.

Validation completed:

- `cargo fmt --all -- --check`
- `cargo check --all-targets`
- `cargo test --workspace --quiet`: 875 passed, 2 ignored
- Python landmark tooling: 11 passed
- Installed-bundle validation: hg38 (19,062 genes), hg19 (19,254), mm10 (38,779), mm39
  (26,683), and hs1 (28,738)
- Release build: `rgv-desktop` and `rgv-mcp`

Qualified bundle fallbacks are explicit: mitochondrial contigs lack cytobands in hg19, mm10, and
hs1; mitochondrial `acen` is absent in all five bundles; mm10 provides band intervals but no typed
`acen`; and mm39 chrY has no typed `acen`. RGV does not invent centromere coordinates for these
cases.

Native evidence is recorded in
`benchmark-results/gene-symbol-chromosome-context-v2/`: automatic ACTB labeling, a symbol-first
ACTB inspector with stable gene and transcript IDs separated, a one-base inspector stress case,
and the chr7 centromere/shared-axis view. The final MCP selection retained
`chr7:5520001-5538000`, reported `ACTB`, `ENSG00000075624.17`, and `ENST00000646664.1`, and remained
visible in the active view. Deterministic tests cover search ranking, band hover including
subpixel-width bands, accessibility, multi-panel endpoints, and label collisions.

## Out of scope

- Showing every transcript label simultaneously in the representative Genes lane.
- Inventing gene symbols for annotation sources that provide only stable IDs.
- Inferring centromeres for assemblies without qualified cytoband data.
- Replacing cytogenetic bands with sequence-derived centromere annotations.
- Changing the biological coordinates or transcript-selection policy of the curated bundles.
