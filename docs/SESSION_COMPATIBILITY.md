# Session compatibility and migration policy

RGV JSON sessions have an explicit integer `schema_version`. Version 1 is the first stable schema;
version 4 is the current canonical development schema. Readers probe and validate the version,
locus, ordered view panels, stable panel and track identities, active panel, per-track state, and
workspace alignment ceiling before mutating viewer state.

Compatibility rules:

1. A stable RGV release must read every session version it has previously declared stable.
2. New optional fields use deterministic defaults and do not change the meaning of existing fields.
   For example, version-1 track records written before the GUI redesign omit `collapsed` and load
   expanded.
3. Removing or reinterpreting a field requires a new schema version and an explicit, tested
   one-way migration into the current in-memory model.
4. Unknown future versions fail with a specific error; RGV never guesses and overwrites them.
5. Saving writes only the current canonical schema and never embeds credentials. Migration does not
   edit the original file unless the user explicitly saves over it.
6. A schema bump adds fixtures for the oldest supported version, every migration boundary, the
   current version, malformed input, and a future-version rejection.
7. Support for a stable schema can be retired only in a major application release after at least
   two prior stable releases have warned users and documented an offline conversion path.

IGV XML is an import format, not an RGV compatibility promise. The importer extracts recognized
loci and resource paths and ignores unsupported IGV renderer state. Importing never rewrites the
source XML.

## Runtime replacement and file-task identity

Workspace epochs and persisted-document revisions are process-local and do not change session
JSON or the control protocol's `workspace_revision`. Native controls and automation share the
canonical persisted-document observation boundary; selection, hover and unapplied filter drafts
do not count as document edits.

A save records its captured revision, not the state at completion. Old-workspace and out-of-order
Save As completions cannot change the current destination or clear newer edits. Competing writes
to the same normalized destination (including SaveThenOpen and autosave) return an explicit busy
error until the earlier writer finishes.

Only the newest load request can commit. Intervening document edits require a new replacement
request; they are not implicitly discarded by an earlier confirmation. OpenWorkspace and
SaveThenOpen file work run asynchronously. Imports and recovered autosaves start dirty with no
native save destination. Capability/filter/panel preparation failures leave the current workspace
intact; valid projects with unavailable sources still open with per-source error states.

Reference and auxiliary engines retire through a bounded queue without joining blocked workers
on the UI thread. When too many sources are still closing, new replacement requests are rejected
with a retry status. Final application shutdown owns the remaining joins.

## Version 1 to version 2

Version 2 replaces the single document-level alignment source and filters with an ordered
`alignment_tracks` array. Each entry has a stable ID, name, source, visibility, collapsed state,
height, filters, and requested alignment limit. `maximum_alignments` remains at document level as a
workspace-wide hard ceiling; no track may request more than that ceiling. The field now represents
the maximum records in an admitted complete individual-read batch, not a retained sample size. A
query above its effective allocation exposes no individual reads while preserving complete
aggregates where available. This semantic correction removes an incomplete presentation and does
not change the serialized value, default, or migration path.

The version-1 migration is deterministic:

- a present `alignment` becomes one alignment-track entry;
- document-level `filters` and `maximum_alignments` become that track's settings;
- the track name is derived from the redacted source basename;
- the track receives the first positive ID not used by an auxiliary track;
- a missing alignment becomes an empty alignment-track list; and
- reference and auxiliary tracks preserve their order and values.

## Version 2 to version 3

Version 3 adds an ordered `panels` array and an `active_panel` stable ID. Every panel contains a
positive stable ID and its own locus. The existing top-level `locus` remains as a compatibility
field and must equal the active panel's locus.

Version-1 and version-2 sessions migrate to one panel with ID 1. That panel receives the existing
top-level locus and becomes active. Migration does not change alignment or auxiliary track order.

The desktop restores every panel during initial `--session` launch and when applying a session to
an existing window. It creates one query projection for every alignment-track/panel pair before
results can arrive. Saved order and IDs are independent: moving a panel changes presentation order,
not query, cache, selection, or persistence identity.

Version-3 validation rejects:

- an empty panel array or more than 16 panels;
- zero or duplicate panel IDs;
- empty contig names and empty or reversed intervals;
- an `active_panel` ID absent from the panel array; and
- a top-level compatibility locus different from the active panel locus.

New optional version-3 alignment settings retain deterministic defaults for older files. Group and
sort order default to natural ordering; base sorting uses the centre of each panel; and custom
colour, group, or sort tags must be valid two-character SAM tags. These additions do not reinterpret
an existing field and therefore do not require a schema bump.

Coverage-transform compatibility is deliberately asymmetric: newly created alignment tracks use
linear coverage heights, while a serialized track that predates the `coverage_scale_transform`
field loads as `square_root`, preserving its former presentation. The raw coverage metric itself is
not migrated: it now means exact maximum concurrent aligned-base depth within each display bin.
Likewise, changing the new-track secondary-alignment default to off does not reinterpret saved
`include_secondary` values. Optional assembly-identity fields mean unidentified/custom when absent.

## Version 3 to version 4

Version 4 replaces mirrored, source-level presentation fields with one normalized
`presentation` object. It stores independent ordered `annotation` and `reads` nodes, first-class
channel-qualified groups, separate desired content visibility and group expansion, literal
preferred row heights, persisted parent gates for composite overlays, and the workspace-owned
read-pane height. Every panel persists the workspace annotation-camera value as a compatibility
projection, plus its own read-stack camera and per-alignment read-panel scroll, content origin,
latent fraction, content extent, and optional density override. When an older file contains
different annotation offsets per panel, the active panel's value becomes the single workspace
offset and the next save writes that value to every panel.

The version-3 migration is deterministic:

- each alignment source becomes one annotation node and one read node;
- each auxiliary source becomes one annotation node;
- legacy group members are collected separately by `(channel, group_id)` in source order;
- singleton legacy groups are discarded and conflicting collapse flags resolve to collapsed if any
  member was collapsed;
- equal numeric group IDs in Annotation and Reads remain distinct channel-qualified identities;
- the former first alignment height becomes the workspace read-pane height; and
- legacy visibility, collapse, group, and pane-height mirrors are cleared after normalization and
  are never emitted by a version-4 save.

Version-4 validation rejects missing or duplicate pane nodes, cross-channel/unknown group members,
duplicate group membership, non-contiguous groups, groups smaller than two members, non-monotonic
group allocation state, invalid preferred sizes, and read-panel cameras owned by unknown alignment
sources.

Saving always emits version 4. Remote credentials, query parameters, and fragments are redacted for
every alignment, reference, and auxiliary source. Unknown future schemas are rejected before full
deserialization and are never overwritten automatically. A session is limited to 64 alignment
tracks and 16 view panels so loading untrusted input cannot instantiate unbounded engines or
projections.

The desktop instantiates every alignment entry as an ordered live controller with its own engine,
generation clock, lifecycle, filter editor and validation state, query result, row layout, and
source statistics. Every entry exposes the complete appearance/filter settings surface and
track-aware semantic inspection. Reordering can promote or demote the first entry while preserving
stable identity and retained controller state.
