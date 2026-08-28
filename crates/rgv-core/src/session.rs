use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::Write,
    path::{Path, PathBuf},
};

use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContigAliases, GeneticCode, GenomeAssemblyIdentity, PanelId, ReferenceTranslationFrame,
    SampleMetadataError, SampleMetadataTable, TrackConfig, TrackFormat, TrackId, TrackKind,
    TrackSource, classify_genomic_source,
};

pub const SESSION_SCHEMA_VERSION: u32 = 4;
pub const MAXIMUM_ALIGNMENT_TRACKS: usize = 64;
pub const MAXIMUM_VIEW_PANELS: usize = 16;
pub const MAXIMUM_CONTIG_ALIASES: usize = 10_000;
pub const MAXIMUM_REGIONS_OF_INTEREST: usize = 100_000;
pub const MAXIMUM_REVIEW_ITEMS: usize = 100_000;
pub const MAXIMUM_REVIEW_NOTE_BYTES: usize = 16_384;
const MAXIMUM_REMOTE_INDEX_URL_BYTES: usize = 16_384;
const LEGACY_SESSION_SCHEMA_VERSION: u32 = 1;
const MULTI_TRACK_SESSION_SCHEMA_VERSION: u32 = 2;
const MULTI_PANEL_SESSION_SCHEMA_VERSION: u32 = 3;

mod igv;
#[cfg(test)]
use igv::infer_track_kind;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLocus {
    pub contig: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionViewPanel {
    pub id: PanelId,
    pub locus: SessionLocus,
    #[serde(default)]
    pub vertical_scroll_points: f32,
    /// Compatibility projection of the workspace-wide annotation camera.
    /// Current writers store the same value in every panel.
    #[serde(default)]
    pub annotation_scroll_points: f32,
    #[serde(default)]
    pub vertical_content_origin_points: f32,
    /// Conventional parent camera used only when the stack of read-panel
    /// headers/minimums is taller than the read pane. This is independent of
    /// every panel's genomic-read density camera.
    #[serde(default)]
    pub read_stack_scroll_points: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_panel_states: Vec<SessionReadPanelState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionReadPanelState {
    pub track_id: TrackId,
    #[serde(default)]
    pub scroll_points: f32,
    #[serde(default)]
    pub content_origin_points: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latent_scroll_fraction: Option<f32>,
    #[serde(default)]
    pub content_bottom_points: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegionOfInterest {
    pub id: u64,
    pub name: String,
    pub locus: SessionLocus,
    pub color: [u8; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReviewStatus {
    #[default]
    Pending,
    InProgress,
    Complete,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: u64,
    pub label: String,
    pub locus: SessionLocus,
    #[serde(default)]
    pub status: ReviewStatus,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub assignment: Option<String>,
}

/// A local file referenced by a session that is not currently available.
///
/// Issues are reported individually so callers can keep the rest of a
/// workspace usable instead of reducing several missing resources to one
/// generic project error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedSessionSource {
    pub label: String,
    pub path: PathBuf,
}

const MAXIMUM_IGV_IMPORT_NOTICES: usize = 4_096;
const MAXIMUM_IGV_NOTICE_BYTES: usize = 512;

/// An explicit account of how an IGV XML session was translated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IgvImportReport {
    pub imported: Vec<String>,
    pub approximated: Vec<String>,
    pub ignored: Vec<String>,
    pub omitted_notice_count: usize,
}

impl IgvImportReport {
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} imported · {} approximated · {} ignored{}",
            self.imported.len(),
            self.approximated.len(),
            self.ignored.len(),
            if self.omitted_notice_count == 0 {
                String::new()
            } else {
                format!(
                    " · {} additional notices omitted",
                    self.omitted_notice_count
                )
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IgvSessionImport {
    pub document: SessionDocument,
    pub report: IgvImportReport,
}

#[derive(Default)]
struct IgvImportReportBuilder {
    imported: BTreeSet<String>,
    approximated: BTreeSet<String>,
    ignored: BTreeSet<String>,
    omitted_notice_count: usize,
}

impl IgvImportReportBuilder {
    fn imported(&mut self, notice: impl Into<String>) {
        insert_igv_notice(
            &mut self.imported,
            &mut self.omitted_notice_count,
            notice.into(),
        );
    }

    fn approximated(&mut self, notice: impl Into<String>) {
        insert_igv_notice(
            &mut self.approximated,
            &mut self.omitted_notice_count,
            notice.into(),
        );
    }

    fn ignored(&mut self, notice: impl Into<String>) {
        insert_igv_notice(
            &mut self.ignored,
            &mut self.omitted_notice_count,
            notice.into(),
        );
    }

    fn finish(self) -> IgvImportReport {
        IgvImportReport {
            imported: self.imported.into_iter().collect(),
            approximated: self.approximated.into_iter().collect(),
            ignored: self.ignored.into_iter().collect(),
            omitted_notice_count: self.omitted_notice_count,
        }
    }
}

fn insert_igv_notice(notices: &mut BTreeSet<String>, omitted: &mut usize, mut notice: String) {
    if notice.len() > MAXIMUM_IGV_NOTICE_BYTES {
        notice.truncate(MAXIMUM_IGV_NOTICE_BYTES);
    }
    if notices.contains(&notice) {
        return;
    }
    if notices.len() >= MAXIMUM_IGV_IMPORT_NOTICES {
        *omitted = omitted.saturating_add(1);
    } else {
        notices.insert(notice);
    }
}

impl SessionViewPanel {
    pub fn new(id: PanelId, locus: SessionLocus) -> Self {
        Self {
            id,
            locus,
            vertical_scroll_points: 0.0,
            annotation_scroll_points: 0.0,
            vertical_content_origin_points: 0.0,
            read_stack_scroll_points: 0.0,
            read_panel_states: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadBaseDisplayMode {
    #[default]
    Mismatches,
    All,
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifiedBaseColorMode {
    #[default]
    ByType,
    Mono,
    ModifiedVsUnmodified,
}

/// Reference-sequence context used to interpret bisulfite-conversion evidence.
///
/// The context is written in the direction of the cytosine-bearing strand. `H` is A, C, or T;
/// `W` is A or T. Both reference strands are considered by the layout layer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BisulfiteContext {
    #[default]
    Cg,
    Chg,
    Chh,
    Hcg,
    Gch,
    Wcg,
}

/// Per-base SMRT kinetic signal selected for alignment colouring.
///
/// Subread/CLR records use the `ip` and `pw` arrays. CCS records carry separate forward and
/// reverse-strand arrays; the renderer selects the appropriate physical tag from the requested
/// strand and the record's alignment orientation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmrtKineticsMode {
    #[default]
    SubreadIpd,
    SubreadPulseWidth,
    CcsForwardIpd,
    CcsForwardPulseWidth,
    CcsReverseIpd,
    CcsReversePulseWidth,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UltimaQualityMode {
    #[default]
    Directional,
    Minimum,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UltimaDisplayOptions {
    #[serde(default)]
    pub quality_mode: UltimaQualityMode,
    /// Overrides the Ultima `mc` read-group field when present.
    #[serde(default)]
    pub maximum_homopolymer_length_override: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SbxSimplexTailMode {
    Off,
    #[default]
    Highlight,
    Hide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SbxDisplayOptions {
    #[serde(default)]
    pub simplex_tail_mode: SbxSimplexTailMode,
    #[serde(default = "default_sbx_discordant_quality_maximum")]
    pub discordant_quality_maximum: u8,
    #[serde(default = "default_sbx_simplex_tail_quality_maximum")]
    pub simplex_tail_quality_maximum: u8,
}

impl Default for SbxDisplayOptions {
    fn default() -> Self {
        Self {
            simplex_tail_mode: SbxSimplexTailMode::Highlight,
            discordant_quality_maximum: default_sbx_discordant_quality_maximum(),
            simplex_tail_quality_maximum: default_sbx_simplex_tail_quality_maximum(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlappingMatePolicy {
    #[default]
    CountOnce,
    CountBoth,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectorActivation {
    #[default]
    Click,
    Hover,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JunctionDisplayMode {
    #[default]
    Collapsed,
    Expanded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SashimiScaleMode {
    #[default]
    Automatic,
    Fixed,
    Shared,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SashimiCountMode {
    #[default]
    Raw,
    CountsPerMillion,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentColorMode {
    #[default]
    None,
    Strand,
    MappingQuality,
    ReadGroup,
    Haplotype,
    TemplateLength,
    PairOrientation,
    UnexpectedPair,
    MateContig,
    FirstOfPairStrand,
    Bisulfite(BisulfiteContext),
    SmrtKinetics(SmrtKineticsMode),
    Ultima(UltimaDisplayOptions),
    Sbx(SbxDisplayOptions),
    Tag([u8; 2]),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentGroupMode {
    #[default]
    None,
    Strand,
    ReadGroup,
    Haplotype,
    Chimeric,
    Supplementary,
    LeftClip,
    RightClip,
    PairOrientation,
    UnexpectedPair,
    MateContig,
    ReadOrder,
    FirstOfPairStrand,
    Tag([u8; 2]),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOrderDirection {
    #[default]
    Natural,
    Reverse,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentSortMode {
    #[default]
    Start,
    Strand,
    FirstOfPairStrand,
    MappingQuality,
    ReadName,
    ReadGroup,
    Haplotype,
    AlignedLength,
    LeftClip,
    RightClip,
    TemplateLength,
    PairOrientation,
    MateContig,
    PairStatus,
    ReadOrder,
    Base,
    Tag([u8; 2]),
}

/// Controls whether alignment rows preserve navigation history or are packed
/// independently for every settled query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadStackingMode {
    /// Keep known alignments on the same rows across zoom, pan, filtering, and
    /// other query-membership changes. This is the evidence-review default.
    #[default]
    Stable,
    /// Repack every settled query for minimum height. Reads may move when the
    /// query membership changes.
    Compact,
}

/// Why the current read-row projection was established. This is reported to
/// diagnostics and automation so an intentional reflow is distinguishable
/// from ordinary navigation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadStackingTransitionReason {
    #[default]
    InitialCanonical,
    Navigation,
    HorizonExtension,
    PolicyContext,
    CompactMode,
    ExplicitRebuild,
    /// Legacy diagnostic value retained for older automation captures.
    Initial,
    /// Legacy diagnostic value retained for older automation captures.
    Repack,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionFilters {
    #[serde(default)]
    pub alignment_color_mode: AlignmentColorMode,
    #[serde(default)]
    pub alignment_group_mode: AlignmentGroupMode,
    #[serde(default)]
    pub alignment_group_order: AlignmentOrderDirection,
    #[serde(default)]
    pub alignment_sort_mode: AlignmentSortMode,
    #[serde(default)]
    pub alignment_sort_order: AlignmentOrderDirection,
    /// Fixed genomic anchor captured when base-at-position sorting begins.
    #[serde(default)]
    pub alignment_sort_anchor: Option<u64>,
    #[serde(default)]
    pub read_stacking_mode: ReadStackingMode,
    #[serde(default)]
    pub view_as_pairs: bool,
    #[serde(default)]
    pub minimum_template_length: Option<u64>,
    #[serde(default)]
    pub maximum_template_length: Option<u64>,
    #[serde(default)]
    pub overlapping_mate_policy: OverlappingMatePolicy,
    pub minimum_mapping_quality: Option<u8>,
    #[serde(default)]
    pub minimum_base_quality: u8,
    #[serde(default = "default_minimum_allele_fraction")]
    pub minimum_allele_fraction: f32,
    #[serde(default = "default_true")]
    pub quality_weighted_alleles: bool,
    #[serde(default = "default_true")]
    pub show_reference: bool,
    #[serde(default = "default_true")]
    pub show_coverage: bool,
    #[serde(default = "default_true")]
    pub show_junctions: bool,
    #[serde(default = "default_one")]
    pub minimum_junction_depth: u32,
    #[serde(default)]
    pub minimum_junction_flank: u32,
    #[serde(default)]
    pub junction_display_mode: JunctionDisplayMode,
    #[serde(default)]
    pub sashimi_scale_mode: SashimiScaleMode,
    #[serde(default = "default_sashimi_fixed_scale_maximum")]
    pub sashimi_fixed_scale_maximum: u32,
    #[serde(default)]
    pub sashimi_count_mode: SashimiCountMode,
    #[serde(default = "default_true")]
    pub show_junction_labels: bool,
    /// Legacy read-channel visibility. Schema-four writers persist the
    /// canonical Reads presentation node instead; this remains readable only
    /// so older sessions can migrate without losing the eye state.
    #[serde(default = "default_true", skip_serializing)]
    pub show_reads: bool,
    #[serde(default)]
    pub coverage_scale_maximum: Option<u32>,
    /// The legacy renderer used square-root heights. Missing fields deserialize
    /// to that transform so old projects retain their appearance, while newly
    /// constructed filters use the linear default below.
    #[serde(default = "legacy_coverage_scale_transform")]
    pub coverage_scale_transform: CoverageScaleTransform,
    #[serde(default = "default_true")]
    pub show_coverage_center_line: bool,
    #[serde(default = "default_alignment_density")]
    pub alignment_density: f32,
    #[serde(default)]
    pub inspector_activation: InspectorActivation,
    #[serde(default)]
    pub read_base_display_mode: ReadBaseDisplayMode,
    #[serde(default = "default_true")]
    pub shade_read_bases_by_quality: bool,
    #[serde(default = "default_one")]
    pub minimum_indel_size: u32,
    #[serde(default = "default_one")]
    pub minimum_event_label_size: u32,
    #[serde(default = "default_true")]
    pub show_modified_bases: bool,
    #[serde(default = "default_true")]
    pub show_modification_frequency: bool,
    #[serde(default)]
    pub minimum_modification_probability: u8,
    #[serde(default)]
    pub modified_base_color_mode: ModifiedBaseColorMode,
    #[serde(default)]
    pub hidden_modification_codes: Vec<String>,
    pub include_secondary: bool,
    pub include_supplementary: bool,
    pub auxiliary_predicates: Vec<String>,
}

impl Default for SessionFilters {
    fn default() -> Self {
        Self {
            alignment_color_mode: AlignmentColorMode::None,
            alignment_group_mode: AlignmentGroupMode::None,
            alignment_group_order: AlignmentOrderDirection::Natural,
            alignment_sort_mode: AlignmentSortMode::Start,
            alignment_sort_order: AlignmentOrderDirection::Natural,
            alignment_sort_anchor: None,
            read_stacking_mode: ReadStackingMode::Stable,
            view_as_pairs: false,
            minimum_template_length: None,
            maximum_template_length: None,
            overlapping_mate_policy: OverlappingMatePolicy::default(),
            minimum_mapping_quality: None,
            minimum_base_quality: 0,
            minimum_allele_fraction: default_minimum_allele_fraction(),
            quality_weighted_alleles: true,
            show_reference: true,
            show_coverage: true,
            show_junctions: true,
            minimum_junction_depth: 1,
            minimum_junction_flank: 0,
            junction_display_mode: JunctionDisplayMode::Collapsed,
            sashimi_scale_mode: SashimiScaleMode::Automatic,
            sashimi_fixed_scale_maximum: default_sashimi_fixed_scale_maximum(),
            sashimi_count_mode: SashimiCountMode::Raw,
            show_junction_labels: true,
            show_reads: true,
            coverage_scale_maximum: None,
            coverage_scale_transform: CoverageScaleTransform::Linear,
            show_coverage_center_line: true,
            alignment_density: default_alignment_density(),
            inspector_activation: InspectorActivation::default(),
            read_base_display_mode: ReadBaseDisplayMode::default(),
            shade_read_bases_by_quality: true,
            minimum_indel_size: 1,
            minimum_event_label_size: 10,
            show_modified_bases: true,
            show_modification_frequency: true,
            minimum_modification_probability: 0,
            modified_base_color_mode: ModifiedBaseColorMode::ByType,
            hidden_modification_codes: Vec::new(),
            include_secondary: false,
            include_supplementary: true,
            auxiliary_predicates: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageScaleTransform {
    #[default]
    Linear,
    SquareRoot,
}

const fn legacy_coverage_scale_transform() -> CoverageScaleTransform {
    CoverageScaleTransform::SquareRoot
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionAlignmentTrack {
    pub id: TrackId,
    pub name: String,
    pub source: TrackSource,
    /// Optional non-conventional BAI, CSI, or CRAI URL for this alignment source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_index_url: Option<String>,
    #[serde(default)]
    pub sample_key: Option<String>,
    /// Legacy source/presentation visibility, consumed only while migrating
    /// pre-schema-four sessions.
    #[serde(default = "default_true", skip_serializing)]
    pub visible: bool,
    #[serde(default, skip_serializing)]
    pub collapsed: bool,
    /// Legacy annotation-group membership, read only during migration.
    #[serde(default, skip_serializing)]
    pub track_group: Option<u32>,
    /// Legacy annotation-group expansion flag, read only during migration.
    #[serde(default, skip_serializing)]
    pub track_group_collapsed: bool,
    /// Legacy read-group membership, read only during migration.
    #[serde(default, skip_serializing)]
    pub read_track_group: Option<u32>,
    /// Legacy read-group expansion flag, read only during migration.
    #[serde(default, skip_serializing)]
    pub read_track_group_collapsed: bool,
    #[serde(default = "default_alignment_track_height", skip_serializing)]
    pub height: f32,
    /// Legacy annotation preferred height, read only during migration.
    #[serde(default, skip_serializing)]
    pub annotation_height: Option<f32>,
    /// Legacy read preferred height, read only during migration.
    #[serde(default, skip_serializing)]
    pub read_group_height: Option<f32>,
    #[serde(default)]
    pub filters: SessionFilters,
    #[serde(default = "default_maximum_alignments")]
    pub maximum_alignments: usize,
}

/// The independent presentation channel in which a source track appears.
/// A single alignment source normally has one node in each channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackChannel {
    Annotation,
    Reads,
}

/// Pane-local state for one loaded source. Vector order is the persisted order
/// within the channel and is intentionally independent between channels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionPaneTrackState {
    pub channel: TrackChannel,
    pub track_id: TrackId,
    #[serde(default = "default_true")]
    pub content_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_height: Option<f32>,
}

/// A presentation group is stored once rather than mirrored on every member.
/// Expansion controls whether member rows are shown; content visibility is the
/// independent group-eye state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionPresentationGroup {
    pub id: u32,
    pub label: String,
    pub channel: TrackChannel,
    pub members: Vec<TrackId>,
    #[serde(default = "default_true")]
    pub expanded: bool,
    #[serde(default = "default_true")]
    pub content_visible: bool,
}

/// Parent visibility for a data-composition object such as a quantitative
/// overlay. Member nodes retain their individual desired eye states; toggling
/// this gate must never rewrite them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionPresentationComposite {
    pub channel: TrackChannel,
    pub id: u32,
    #[serde(default = "default_true")]
    pub content_visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePresentation {
    #[serde(default = "default_alignment_track_height")]
    pub read_pane_height: f32,
    /// Content-fit the annotation/read split until the user explicitly
    /// allocates pane height with the shared divider.
    #[serde(default = "default_true")]
    pub annotation_pane_auto_fit: bool,
    /// Non-destructive workspace gate for the complete annotation section.
    /// Child visibility, grouping, heights, and cameras remain authoritative
    /// while the section is folded.
    #[serde(default = "default_true")]
    pub annotation_section_expanded: bool,
    /// Non-destructive workspace gate for the complete read-data section.
    #[serde(default = "default_true")]
    pub read_section_expanded: bool,
    #[serde(default = "default_true")]
    pub reference_content_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_preferred_height: Option<f32>,
    #[serde(default)]
    pub pane_tracks: Vec<SessionPaneTrackState>,
    #[serde(default)]
    pub groups: Vec<SessionPresentationGroup>,
    #[serde(default)]
    pub composites: Vec<SessionPresentationComposite>,
    /// Monotonic allocator for presentation-group identities. IDs are never
    /// recycled, so a stale UI or automation reference cannot silently target
    /// a later, unrelated group.
    #[serde(default = "default_next_presentation_group_id")]
    pub next_group_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresentationNodeId {
    Reference,
    Track {
        channel: TrackChannel,
        track_id: TrackId,
    },
    Group {
        channel: TrackChannel,
        group_id: u32,
    },
}

impl PresentationNodeId {
    #[must_use]
    pub const fn channel(self) -> TrackChannel {
        match self {
            Self::Reference => TrackChannel::Annotation,
            Self::Track { channel, .. } | Self::Group { channel, .. } => channel,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackIntent {
    SetSectionExpanded {
        channel: TrackChannel,
        expanded: bool,
    },
    SetPaneRowsExpanded {
        channel: TrackChannel,
        expanded: bool,
    },
    ResetPaneRowHeights {
        channel: TrackChannel,
    },
    SetNodeVisibility {
        target: PresentationNodeId,
        visible: bool,
    },
    SetGroupExpanded {
        target: PresentationNodeId,
        expanded: bool,
    },
    SetGroupVisibility {
        target: PresentationNodeId,
        visible: bool,
    },
    SetCompositeVisibility {
        channel: TrackChannel,
        composite_id: u32,
        visible: bool,
    },
    ResizeNode {
        target: PresentationNodeId,
        preferred_height: f32,
    },
    ResizeReadPane {
        height: f32,
    },
    GroupNodes {
        channel: TrackChannel,
        members: Vec<TrackId>,
    },
    Ungroup {
        target: PresentationNodeId,
    },
    MoveNode {
        target: PresentationNodeId,
        before: Option<PresentationNodeId>,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackChangeSet {
    pub presentation_changed: bool,
    pub layout_changed: bool,
    pub query_demand_changed: BTreeSet<TrackId>,
    pub selection_changed: bool,
    pub persistence_changed: bool,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TrackIntentError {
    #[error("presentation node is unavailable")]
    MissingNode,
    #[error("track group members must form one contiguous range")]
    NonContiguousGroup,
    #[error("a grouped member must be explicitly ungrouped before it can move independently")]
    GroupMemberMove,
    #[error("a grouped track must be explicitly ungrouped before it can be regrouped")]
    AlreadyGrouped,
    #[error("presentation size is invalid")]
    InvalidSize,
    #[error("presentation move crosses channels")]
    CrossChannelMove,
}

impl Default for WorkspacePresentation {
    fn default() -> Self {
        Self {
            read_pane_height: default_alignment_track_height(),
            annotation_pane_auto_fit: true,
            annotation_section_expanded: true,
            read_section_expanded: true,
            reference_content_visible: true,
            reference_preferred_height: None,
            pane_tracks: Vec::new(),
            groups: Vec::new(),
            composites: Vec::new(),
            next_group_id: default_next_presentation_group_id(),
        }
    }
}

const fn default_next_presentation_group_id() -> u32 {
    1
}

impl WorkspacePresentation {
    #[must_use]
    pub const fn section_expanded(&self, channel: TrackChannel) -> bool {
        match channel {
            TrackChannel::Annotation => self.annotation_section_expanded,
            TrackChannel::Reads => self.read_section_expanded,
        }
    }

    fn repair_group_allocator(&mut self) {
        let minimum = self
            .groups
            .iter()
            .map(|group| group.id)
            .max()
            .unwrap_or_default()
            .saturating_add(1)
            .max(default_next_presentation_group_id());
        self.next_group_id = self.next_group_id.max(minimum);
    }

    /// Reconciles presentation-node lifetime with loaded source lifetime while
    /// preserving every existing eye, size, order, and group decision. New
    /// source nodes receive the supplied defaults; missing sources are the
    /// only nodes removed. Runtime and session normalization share this path
    /// so their repair semantics cannot drift.
    pub fn reconcile_sources(
        &mut self,
        alignment_sources: &[(TrackId, Option<f32>, Option<f32>)],
        auxiliary_sources: &[(TrackId, Option<f32>)],
    ) {
        let alignment = alignment_sources
            .iter()
            .map(|(track_id, annotation_height, read_height)| {
                (*track_id, (*annotation_height, *read_height))
            })
            .collect::<BTreeMap<_, _>>();
        let auxiliary = auxiliary_sources
            .iter()
            .copied()
            .collect::<BTreeMap<_, _>>();
        self.pane_tracks.retain(|node| match node.channel {
            TrackChannel::Annotation => {
                alignment.contains_key(&node.track_id) || auxiliary.contains_key(&node.track_id)
            }
            TrackChannel::Reads => alignment.contains_key(&node.track_id),
        });
        for (track_id, annotation_height, read_height) in alignment_sources {
            for (channel, preferred_height) in [
                (TrackChannel::Annotation, *annotation_height),
                (TrackChannel::Reads, *read_height),
            ] {
                if self.pane_track_state(channel, *track_id).is_none() {
                    self.pane_tracks.push(SessionPaneTrackState {
                        channel,
                        track_id: *track_id,
                        content_visible: true,
                        preferred_height,
                    });
                }
            }
        }
        for (track_id, preferred_height) in auxiliary_sources {
            if self
                .pane_track_state(TrackChannel::Annotation, *track_id)
                .is_none()
            {
                self.pane_tracks.push(SessionPaneTrackState {
                    channel: TrackChannel::Annotation,
                    track_id: *track_id,
                    content_visible: true,
                    preferred_height: *preferred_height,
                });
            }
        }
        let mut claimed = BTreeSet::new();
        for group in &mut self.groups {
            group.members.retain(|member| {
                let loaded = match group.channel {
                    TrackChannel::Annotation => {
                        alignment.contains_key(member) || auxiliary.contains_key(member)
                    }
                    TrackChannel::Reads => alignment.contains_key(member),
                };
                loaded && claimed.insert((group.channel, *member))
            });
        }
        self.groups.retain(|group| group.members.len() >= 2);
        self.repair_group_allocator();
    }

    /// Reconciles presentation-only parent gates for data-composition objects.
    /// Existing values survive member reorder and ordinary eye changes.
    pub fn reconcile_composites(&mut self, channel: TrackChannel, ids: &BTreeSet<u32>) {
        self.composites
            .retain(|composite| composite.channel != channel || ids.contains(&composite.id));
        for id in ids {
            if !self
                .composites
                .iter()
                .any(|composite| composite.channel == channel && composite.id == *id)
            {
                self.composites.push(SessionPresentationComposite {
                    channel,
                    id: *id,
                    content_visible: true,
                });
            }
        }
    }

    #[must_use]
    pub fn composite_content_visible(&self, channel: TrackChannel, id: u32) -> bool {
        self.composites
            .iter()
            .find(|composite| composite.channel == channel && composite.id == id)
            .is_none_or(|composite| composite.content_visible)
    }

    #[must_use]
    pub fn from_legacy(
        alignment_tracks: &[SessionAlignmentTrack],
        auxiliary_tracks: &[TrackConfig],
    ) -> Self {
        let mut pane_tracks =
            Vec::with_capacity(alignment_tracks.len().saturating_mul(2) + auxiliary_tracks.len());
        pane_tracks.extend(alignment_tracks.iter().map(|track| SessionPaneTrackState {
            channel: TrackChannel::Annotation,
            track_id: track.id,
            content_visible: track.visible && !track.collapsed,
            preferred_height: track.annotation_height,
        }));
        pane_tracks.extend(auxiliary_tracks.iter().map(|track| SessionPaneTrackState {
            channel: TrackChannel::Annotation,
            track_id: track.id,
            content_visible: track.visible && !track.collapsed,
            preferred_height: Some(track.height),
        }));
        pane_tracks.extend(alignment_tracks.iter().map(|track| {
            SessionPaneTrackState {
                channel: TrackChannel::Reads,
                track_id: track.id,
                content_visible: track.visible && track.filters.show_reads,
                preferred_height: track
                    .read_group_height
                    .or(Some(track.height.clamp(40.0, 1_200.0))),
            }
        }));

        let mut groups = Vec::new();
        for channel in [TrackChannel::Annotation, TrackChannel::Reads] {
            let mut members = std::collections::BTreeMap::<u32, Vec<TrackId>>::new();
            let mut collapsed = BTreeSet::<u32>::new();
            for track in alignment_tracks {
                let (group, is_collapsed) = match channel {
                    TrackChannel::Annotation => (track.track_group, track.track_group_collapsed),
                    TrackChannel::Reads => {
                        (track.read_track_group, track.read_track_group_collapsed)
                    }
                };
                if let Some(group) = group {
                    members.entry(group).or_default().push(track.id);
                    if is_collapsed {
                        collapsed.insert(group);
                    }
                }
            }
            groups.extend(members.into_iter().filter_map(|(id, members)| {
                (members.len() > 1).then(|| SessionPresentationGroup {
                    id,
                    label: format!("Group {id}"),
                    channel,
                    members,
                    expanded: !collapsed.contains(&id),
                    content_visible: true,
                })
            }));
        }
        Self {
            read_pane_height: alignment_tracks
                .first()
                .map_or_else(default_alignment_track_height, |track| {
                    track.height.clamp(100.0, 1_200.0)
                }),
            annotation_pane_auto_fit: true,
            annotation_section_expanded: true,
            read_section_expanded: true,
            reference_content_visible: true,
            reference_preferred_height: None,
            pane_tracks,
            next_group_id: groups
                .iter()
                .map(|group| group.id)
                .max()
                .unwrap_or_default()
                .saturating_add(1)
                .max(default_next_presentation_group_id()),
            groups,
            composites: Vec::new(),
        }
    }

    /// Returns the ordered root nodes for one pane. Group members occupy one
    /// root slot represented by the group identity; ungrouped tracks remain
    /// ordinary root nodes. This is the canonical ordering consumed by layout,
    /// selection, drag/drop, and automation.
    #[must_use]
    pub fn pane_root_nodes(&self, channel: TrackChannel) -> Vec<PresentationNodeId> {
        let groups_by_anchor = self
            .groups
            .iter()
            .filter(|group| group.channel == channel)
            .filter_map(|group| group.members.first().copied().map(|anchor| (anchor, group)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let grouped = self
            .groups
            .iter()
            .filter(|group| group.channel == channel)
            .flat_map(|group| group.members.iter().copied())
            .collect::<BTreeSet<_>>();
        self.pane_tracks
            .iter()
            .filter(|node| node.channel == channel)
            .filter_map(|node| {
                if let Some(group) = groups_by_anchor.get(&node.track_id) {
                    Some(PresentationNodeId::Group {
                        channel,
                        group_id: group.id,
                    })
                } else if grouped.contains(&node.track_id) {
                    None
                } else {
                    Some(PresentationNodeId::Track {
                        channel,
                        track_id: node.track_id,
                    })
                }
            })
            .collect()
    }

    /// Returns the ordered interactive rows in a pane. Expanded groups retain
    /// their header followed by their members; minimized groups retain only
    /// their header.
    #[must_use]
    pub fn pane_interactive_nodes(&self, channel: TrackChannel) -> Vec<PresentationNodeId> {
        let mut nodes = Vec::new();
        for root in self.pane_root_nodes(channel) {
            nodes.push(root);
            if let PresentationNodeId::Group { group_id, .. } = root
                && let Some(group) = self
                    .groups
                    .iter()
                    .find(|group| group.channel == channel && group.id == group_id)
                && group.expanded
            {
                nodes.extend(
                    group
                        .members
                        .iter()
                        .copied()
                        .map(|track_id| PresentationNodeId::Track { channel, track_id }),
                );
            }
        }
        nodes
    }

    #[must_use]
    pub fn pane_track_state(
        &self,
        channel: TrackChannel,
        track_id: TrackId,
    ) -> Option<&SessionPaneTrackState> {
        self.pane_tracks
            .iter()
            .find(|node| node.channel == channel && node.track_id == track_id)
    }

    #[must_use]
    pub fn group_for_track(
        &self,
        channel: TrackChannel,
        track_id: TrackId,
    ) -> Option<&SessionPresentationGroup> {
        self.groups
            .iter()
            .find(|group| group.channel == channel && group.members.contains(&track_id))
    }

    /// Desired node visibility resolved through its parent gate. Expansion is
    /// deliberately not part of this value: expansion controls whether a row
    /// is exposed, while visibility controls whether its data content paints.
    #[must_use]
    pub fn effective_track_visibility(
        &self,
        channel: TrackChannel,
        track_id: TrackId,
    ) -> Option<bool> {
        let desired = self.pane_track_state(channel, track_id)?.content_visible;
        let parent_visible = self
            .group_for_track(channel, track_id)
            .is_none_or(|group| group.content_visible);
        Some(desired && parent_visible)
    }

    /// Returns whether a track's content is currently exposed for layout and
    /// painting. Unlike `effective_track_visibility`, this also applies the
    /// parent group's expansion gate. Callers that decide rendering, shared
    /// scales, exports, or hit testing should normally use this projection.
    #[must_use]
    pub fn track_content_exposed(&self, channel: TrackChannel, track_id: TrackId) -> Option<bool> {
        let visible = self.effective_track_visibility(channel, track_id)?;
        let expanded = self
            .group_for_track(channel, track_id)
            .is_none_or(|group| group.expanded);
        Some(visible && expanded)
    }

    pub fn apply_intents(
        &mut self,
        intents: &[TrackIntent],
    ) -> Result<TrackChangeSet, TrackIntentError> {
        let mut next = self.clone();
        let mut changes = TrackChangeSet::default();
        for intent in intents {
            next.apply_intent(intent, &mut changes)?;
        }
        if changes.presentation_changed {
            changes.layout_changed = true;
            changes.persistence_changed = true;
            *self = next;
        }
        Ok(changes)
    }

    /// Removes every pane representation owned by one unloaded source and
    /// repairs any affected groups. This is deliberately a lifecycle
    /// operation rather than an eye/collapse intent; it is the only ordinary
    /// path that destroys saved per-source presentation state.
    pub fn remove_source(&mut self, track_id: TrackId) -> TrackChangeSet {
        let before_nodes = self.pane_tracks.len();
        self.pane_tracks.retain(|node| node.track_id != track_id);
        let before_groups = self.groups.len();
        for group in &mut self.groups {
            group.members.retain(|member| *member != track_id);
        }
        self.groups.retain(|group| group.members.len() >= 2);
        let changed = before_nodes != self.pane_tracks.len() || before_groups != self.groups.len();
        TrackChangeSet {
            presentation_changed: changed,
            layout_changed: changed,
            selection_changed: changed,
            persistence_changed: changed,
            ..TrackChangeSet::default()
        }
    }

    fn apply_intent(
        &mut self,
        intent: &TrackIntent,
        changes: &mut TrackChangeSet,
    ) -> Result<(), TrackIntentError> {
        match intent {
            TrackIntent::SetSectionExpanded { channel, expanded } => {
                let current = match channel {
                    TrackChannel::Annotation => &mut self.annotation_section_expanded,
                    TrackChannel::Reads => &mut self.read_section_expanded,
                };
                changes.presentation_changed |= *current != *expanded;
                *current = *expanded;
            }
            TrackIntent::SetPaneRowsExpanded { channel, expanded } => {
                if *channel == TrackChannel::Annotation {
                    changes.presentation_changed |= self.reference_content_visible != *expanded;
                    self.reference_content_visible = *expanded;
                }
                for node in self
                    .pane_tracks
                    .iter_mut()
                    .filter(|node| node.channel == *channel)
                {
                    changes.presentation_changed |= node.content_visible != *expanded;
                    node.content_visible = *expanded;
                }
                for group in self
                    .groups
                    .iter_mut()
                    .filter(|group| group.channel == *channel)
                {
                    // Every child header must be reachable after either bulk
                    // command. Expanding also opens the group's content gate.
                    changes.presentation_changed |= !group.expanded;
                    group.expanded = true;
                    if *expanded {
                        changes.presentation_changed |= !group.content_visible;
                        group.content_visible = true;
                    }
                }
                if *expanded {
                    for composite in self
                        .composites
                        .iter_mut()
                        .filter(|composite| composite.channel == *channel)
                    {
                        changes.presentation_changed |= !composite.content_visible;
                        composite.content_visible = true;
                    }
                }
                changes.selection_changed |= !*expanded;
            }
            TrackIntent::ResetPaneRowHeights { channel } => {
                if *channel == TrackChannel::Annotation {
                    changes.presentation_changed |= self.reference_preferred_height.is_some();
                    self.reference_preferred_height = None;
                }
                for node in self
                    .pane_tracks
                    .iter_mut()
                    .filter(|node| node.channel == *channel)
                {
                    changes.presentation_changed |= node.preferred_height.is_some();
                    node.preferred_height = None;
                }
            }
            TrackIntent::SetNodeVisibility { target, visible } => match *target {
                PresentationNodeId::Reference => {
                    changes.presentation_changed |= self.reference_content_visible != *visible;
                    changes.selection_changed |= !*visible;
                    self.reference_content_visible = *visible;
                }
                PresentationNodeId::Track { channel, track_id } => {
                    let node = self
                        .pane_tracks
                        .iter_mut()
                        .find(|node| node.channel == channel && node.track_id == track_id)
                        .ok_or(TrackIntentError::MissingNode)?;
                    changes.presentation_changed |= node.content_visible != *visible;
                    changes.selection_changed |= !*visible;
                    node.content_visible = *visible;
                }
                PresentationNodeId::Group { .. } => {
                    return Err(TrackIntentError::MissingNode);
                }
            },
            TrackIntent::SetGroupExpanded { target, expanded } => {
                let PresentationNodeId::Group { channel, group_id } = *target else {
                    return Err(TrackIntentError::MissingNode);
                };
                let group = self
                    .groups
                    .iter_mut()
                    .find(|group| group.channel == channel && group.id == group_id)
                    .ok_or(TrackIntentError::MissingNode)?;
                changes.presentation_changed |= group.expanded != *expanded;
                changes.selection_changed |= !*expanded;
                group.expanded = *expanded;
            }
            TrackIntent::SetGroupVisibility { target, visible } => {
                let PresentationNodeId::Group { channel, group_id } = *target else {
                    return Err(TrackIntentError::MissingNode);
                };
                let group = self
                    .groups
                    .iter_mut()
                    .find(|group| group.channel == channel && group.id == group_id)
                    .ok_or(TrackIntentError::MissingNode)?;
                changes.presentation_changed |= group.content_visible != *visible;
                changes.selection_changed |= !*visible;
                group.content_visible = *visible;
            }
            TrackIntent::SetCompositeVisibility {
                channel,
                composite_id,
                visible,
            } => {
                let composite = self
                    .composites
                    .iter_mut()
                    .find(|composite| {
                        composite.channel == *channel && composite.id == *composite_id
                    })
                    .ok_or(TrackIntentError::MissingNode)?;
                changes.presentation_changed |= composite.content_visible != *visible;
                changes.selection_changed |= !*visible;
                composite.content_visible = *visible;
            }
            TrackIntent::ResizeNode {
                target,
                preferred_height,
            } => {
                let channel = target.channel();
                let valid = preferred_height.is_finite()
                    && match channel {
                        TrackChannel::Annotation => (26.0..=1_200.0).contains(preferred_height),
                        TrackChannel::Reads => (40.0..=1_200.0).contains(preferred_height),
                    };
                if !valid {
                    return Err(TrackIntentError::InvalidSize);
                }
                if *target == PresentationNodeId::Reference {
                    changes.presentation_changed |= self
                        .reference_preferred_height
                        .is_none_or(|height| (height - preferred_height).abs() > f32::EPSILON);
                    self.reference_preferred_height = Some(*preferred_height);
                    return Ok(());
                }
                let PresentationNodeId::Track { track_id, .. } = *target else {
                    return Err(TrackIntentError::MissingNode);
                };
                let node = self
                    .pane_tracks
                    .iter_mut()
                    .find(|node| node.channel == channel && node.track_id == track_id)
                    .ok_or(TrackIntentError::MissingNode)?;
                changes.presentation_changed |= node
                    .preferred_height
                    .is_none_or(|height| (height - preferred_height).abs() > f32::EPSILON);
                node.preferred_height = Some(*preferred_height);
            }
            TrackIntent::ResizeReadPane { height } => {
                if !height.is_finite() || !(100.0..=1_200.0).contains(height) {
                    return Err(TrackIntentError::InvalidSize);
                }
                changes.presentation_changed |= self.annotation_pane_auto_fit
                    || (self.read_pane_height - height).abs() > f32::EPSILON;
                self.read_pane_height = *height;
                self.annotation_pane_auto_fit = false;
            }
            TrackIntent::GroupNodes { channel, members } => {
                let unique = members.iter().copied().collect::<BTreeSet<_>>();
                if unique.len() < 2 || unique.len() != members.len() {
                    return Err(TrackIntentError::NonContiguousGroup);
                }
                if self.groups.iter().any(|group| {
                    group.channel == *channel
                        && group.members.iter().any(|member| unique.contains(member))
                }) {
                    return Err(TrackIntentError::AlreadyGrouped);
                }
                let positions = members
                    .iter()
                    .map(|member| {
                        self.pane_tracks
                            .iter()
                            .filter(|node| node.channel == *channel)
                            .position(|node| node.track_id == *member)
                            .ok_or(TrackIntentError::MissingNode)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut sorted_positions = positions;
                sorted_positions.sort_unstable();
                if sorted_positions
                    .windows(2)
                    .any(|pair| pair[1] != pair[0] + 1)
                {
                    return Err(TrackIntentError::NonContiguousGroup);
                }
                let id = self.next_group_id;
                self.next_group_id = self
                    .next_group_id
                    .checked_add(1)
                    .ok_or(TrackIntentError::MissingNode)?;
                let ordered_members = self
                    .pane_tracks
                    .iter()
                    .filter(|node| node.channel == *channel && unique.contains(&node.track_id))
                    .map(|node| node.track_id)
                    .collect();
                self.groups.push(SessionPresentationGroup {
                    id,
                    label: format!("Group {id}"),
                    channel: *channel,
                    members: ordered_members,
                    expanded: true,
                    content_visible: true,
                });
                changes.presentation_changed = true;
            }
            TrackIntent::Ungroup { target } => {
                let PresentationNodeId::Group { channel, group_id } = *target else {
                    return Err(TrackIntentError::MissingNode);
                };
                let before = self.groups.len();
                self.groups
                    .retain(|group| group.channel != channel || group.id != group_id);
                if before == self.groups.len() {
                    return Err(TrackIntentError::MissingNode);
                }
                changes.presentation_changed = true;
            }
            TrackIntent::MoveNode { target, before } => {
                let channel = target.channel();
                if before.is_some_and(|node| node.channel() != channel) {
                    return Err(TrackIntentError::CrossChannelMove);
                }
                if *before == Some(*target) {
                    return Ok(());
                }
                let moving = match *target {
                    PresentationNodeId::Track { track_id, .. } => {
                        if self.groups.iter().any(|group| {
                            group.channel == channel && group.members.contains(&track_id)
                        }) {
                            return Err(TrackIntentError::GroupMemberMove);
                        }
                        vec![track_id]
                    }
                    PresentationNodeId::Group { group_id, .. } => self
                        .groups
                        .iter()
                        .find(|group| group.channel == channel && group.id == group_id)
                        .map(|group| group.members.clone())
                        .ok_or(TrackIntentError::MissingNode)?,
                    PresentationNodeId::Reference => return Err(TrackIntentError::MissingNode),
                };
                let before_track = match before {
                    None => None,
                    Some(PresentationNodeId::Track { track_id, .. }) => {
                        if moving.contains(track_id) {
                            return Ok(());
                        }
                        let node_exists = self.pane_track_state(channel, *track_id).is_some();
                        if !node_exists {
                            return Err(TrackIntentError::MissingNode);
                        }
                        Some(
                            self.group_for_track(channel, *track_id)
                                .and_then(|group| group.members.first().copied())
                                .unwrap_or(*track_id),
                        )
                    }
                    Some(PresentationNodeId::Group { group_id, .. }) => Some(
                        self.groups
                            .iter()
                            .find(|group| group.channel == channel && group.id == *group_id)
                            .and_then(|group| group.members.first().copied())
                            .ok_or(TrackIntentError::MissingNode)?,
                    ),
                    Some(PresentationNodeId::Reference) => {
                        return Err(TrackIntentError::MissingNode);
                    }
                };
                let mut channel_nodes = self
                    .pane_tracks
                    .iter()
                    .filter(|node| node.channel == channel)
                    .cloned()
                    .collect::<Vec<_>>();
                let moving_set = moving.iter().copied().collect::<BTreeSet<_>>();
                let moved = channel_nodes
                    .iter()
                    .filter(|node| moving_set.contains(&node.track_id))
                    .cloned()
                    .collect::<Vec<_>>();
                if moved.len() != moving.len() {
                    return Err(TrackIntentError::MissingNode);
                }
                channel_nodes.retain(|node| !moving_set.contains(&node.track_id));
                let insertion = match before_track {
                    Some(track_id) => channel_nodes
                        .iter()
                        .position(|node| node.track_id == track_id)
                        .ok_or(TrackIntentError::MissingNode)?,
                    None => channel_nodes.len(),
                };
                channel_nodes.splice(insertion..insertion, moved);
                self.pane_tracks.retain(|node| node.channel != channel);
                let insertion = match channel {
                    TrackChannel::Annotation => 0,
                    TrackChannel::Reads => self.pane_tracks.len(),
                };
                self.pane_tracks.splice(insertion..insertion, channel_nodes);
                changes.presentation_changed = true;
            }
        }
        Ok(())
    }
}

impl SessionAlignmentTrack {
    pub fn new(id: TrackId, name: String, source: TrackSource) -> Self {
        Self {
            id,
            name,
            source,
            remote_index_url: None,
            sample_key: None,
            visible: true,
            collapsed: false,
            track_group: None,
            track_group_collapsed: false,
            read_track_group: None,
            read_track_group_collapsed: false,
            height: default_alignment_track_height(),
            annotation_height: None,
            read_group_height: None,
            filters: SessionFilters::default(),
            maximum_alignments: default_maximum_alignments(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDocument {
    pub schema_version: u32,
    /// Bounded, non-persisted repairs reported while migrating an older
    /// schema. The desktop surfaces these as project issues instead of
    /// silently normalizing inconsistent mirrored presentation fields.
    #[serde(skip, default)]
    pub migration_notices: Vec<String>,
    pub locus: SessionLocus,
    pub panels: Vec<SessionViewPanel>,
    pub active_panel: PanelId,
    pub alignment_tracks: Vec<SessionAlignmentTrack>,
    #[serde(default)]
    pub presentation: WorkspacePresentation,
    pub reference: Option<TrackSource>,
    /// Optional non-conventional FAI URL for the reference source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_reference_index_url: Option<String>,
    #[serde(default)]
    pub genome_name: Option<String>,
    #[serde(default)]
    pub assembly_identity: Option<GenomeAssemblyIdentity>,
    #[serde(default)]
    pub cytobands: Option<TrackSource>,
    #[serde(default)]
    pub reference_reverse_complement: bool,
    #[serde(default)]
    pub reference_translation_frames: Vec<ReferenceTranslationFrame>,
    #[serde(default)]
    pub reference_genetic_code: GeneticCode,
    #[serde(default)]
    pub regions_of_interest: Vec<RegionOfInterest>,
    #[serde(default)]
    pub review_items: Vec<ReviewItem>,
    #[serde(default)]
    pub sample_metadata: Option<SampleMetadataTable>,
    #[serde(default)]
    pub contig_aliases: ContigAliases,
    pub tracks: Vec<TrackConfig>,
    /// Workspace-wide hard bound. Per-track limits are requests within this shared ceiling.
    pub maximum_alignments: usize,
}

#[derive(Deserialize)]
struct SessionVersionProbe {
    schema_version: u32,
}

#[derive(Deserialize)]
struct LegacySessionDocument {
    schema_version: u32,
    locus: SessionLocus,
    alignment: Option<TrackSource>,
    reference: Option<TrackSource>,
    tracks: Vec<TrackConfig>,
    filters: SessionFilters,
    maximum_alignments: usize,
}

#[derive(Deserialize)]
struct MultiTrackSessionDocument {
    schema_version: u32,
    locus: SessionLocus,
    alignment_tracks: Vec<SessionAlignmentTrack>,
    reference: Option<TrackSource>,
    tracks: Vec<TrackConfig>,
    maximum_alignments: usize,
}

impl MultiTrackSessionDocument {
    fn migrate(self) -> SessionDocument {
        debug_assert_eq!(self.schema_version, MULTI_TRACK_SESSION_SCHEMA_VERSION);
        let panel_id = PanelId::new(1);
        let presentation = WorkspacePresentation::from_legacy(&self.alignment_tracks, &self.tracks);
        SessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            migration_notices: Vec::new(),
            locus: self.locus.clone(),
            panels: vec![SessionViewPanel::new(panel_id, self.locus)],
            active_panel: panel_id,
            alignment_tracks: self.alignment_tracks,
            presentation,
            reference: self.reference,
            remote_reference_index_url: None,
            genome_name: None,
            assembly_identity: None,
            cytobands: None,
            reference_reverse_complement: false,
            reference_translation_frames: Vec::new(),
            reference_genetic_code: GeneticCode::default(),
            regions_of_interest: Vec::new(),
            review_items: Vec::new(),
            sample_metadata: None,
            contig_aliases: ContigAliases::default(),
            tracks: self.tracks,
            maximum_alignments: self.maximum_alignments,
        }
    }
}

impl LegacySessionDocument {
    fn migrate(self) -> SessionDocument {
        debug_assert_eq!(self.schema_version, LEGACY_SESSION_SCHEMA_VERSION);
        let alignment_tracks = self.alignment.map_or_else(Vec::new, |source| {
            let id = next_available_track_id(&self.tracks);
            let mut track =
                SessionAlignmentTrack::new(id, alignment_name_from_source(&source), source);
            track.filters = self.filters;
            track.maximum_alignments = self.maximum_alignments;
            vec![track]
        });
        let presentation = WorkspacePresentation::from_legacy(&alignment_tracks, &self.tracks);
        SessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            migration_notices: Vec::new(),
            locus: self.locus.clone(),
            panels: vec![SessionViewPanel::new(PanelId::new(1), self.locus)],
            active_panel: PanelId::new(1),
            alignment_tracks,
            presentation,
            reference: self.reference,
            remote_reference_index_url: None,
            genome_name: None,
            assembly_identity: None,
            cytobands: None,
            reference_reverse_complement: false,
            reference_translation_frames: Vec::new(),
            reference_genetic_code: GeneticCode::default(),
            regions_of_interest: Vec::new(),
            review_items: Vec::new(),
            sample_metadata: None,
            contig_aliases: ContigAliases::default(),
            tracks: self.tracks,
            maximum_alignments: self.maximum_alignments,
        }
    }
}

impl SessionDocument {
    pub fn new(locus: SessionLocus) -> Self {
        let panel_id = PanelId::new(1);
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            migration_notices: Vec::new(),
            locus: locus.clone(),
            panels: vec![SessionViewPanel::new(panel_id, locus)],
            active_panel: panel_id,
            alignment_tracks: Vec::new(),
            presentation: WorkspacePresentation::default(),
            reference: None,
            remote_reference_index_url: None,
            genome_name: None,
            assembly_identity: None,
            cytobands: None,
            reference_reverse_complement: false,
            reference_translation_frames: Vec::new(),
            reference_genetic_code: GeneticCode::default(),
            regions_of_interest: Vec::new(),
            review_items: Vec::new(),
            sample_metadata: None,
            contig_aliases: ContigAliases::default(),
            tracks: Vec::new(),
            maximum_alignments: default_maximum_alignments(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, SessionError> {
        let probe: SessionVersionProbe = serde_json::from_str(json)?;
        let mut document = match probe.schema_version {
            SESSION_SCHEMA_VERSION => serde_json::from_str(json)?,
            MULTI_PANEL_SESSION_SCHEMA_VERSION => {
                let mut version_three: SessionDocument = serde_json::from_str(json)?;
                version_three.schema_version = SESSION_SCHEMA_VERSION;
                version_three.presentation = WorkspacePresentation::from_legacy(
                    &version_three.alignment_tracks,
                    &version_three.tracks,
                );
                version_three
            }
            MULTI_TRACK_SESSION_SCHEMA_VERSION => {
                let version_two: MultiTrackSessionDocument = serde_json::from_str(json)?;
                version_two.migrate()
            }
            LEGACY_SESSION_SCHEMA_VERSION => {
                let legacy: LegacySessionDocument = serde_json::from_str(json)?;
                legacy.migrate()
            }
            version => return Err(SessionError::UnsupportedVersion(version)),
        };
        if probe.schema_version < SESSION_SCHEMA_VERSION {
            document.migration_notices = document.legacy_presentation_migration_notices();
        }
        document.normalize_presentation();
        document.validate()?;
        Ok(document)
    }

    /// Populates and repairs the normalized presentation tree after a caller
    /// has assembled sources through the low-level public vectors. Normal
    /// application/session code maintains this state incrementally; importers
    /// and programmatic document builders can call this once before validation
    /// or comparison.
    pub fn normalize_presentation(&mut self) {
        if self.presentation.pane_tracks.is_empty()
            && (!self.alignment_tracks.is_empty() || !self.tracks.is_empty())
        {
            self.presentation =
                WorkspacePresentation::from_legacy(&self.alignment_tracks, &self.tracks);
        }
        let alignment_sources = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.id,
                    track.annotation_height,
                    track
                        .read_group_height
                        .or(Some(track.height.clamp(40.0, 1_200.0))),
                )
            })
            .collect::<Vec<_>>();
        let auxiliary_sources = self
            .tracks
            .iter()
            .map(|track| (track.id, Some(track.height.clamp(26.0, 1_200.0))))
            .collect::<Vec<_>>();
        self.presentation
            .reconcile_sources(&alignment_sources, &auxiliary_sources);
        let overlay_counts = self
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Quantitative)
            .filter_map(|track| track.quantitative.overlay_group)
            .fold(BTreeMap::<u32, usize>::new(), |mut counts, id| {
                *counts.entry(id).or_default() += 1;
                counts
            });
        let overlay_ids = overlay_counts
            .into_iter()
            .filter_map(|(id, count)| (count >= 2).then_some(id))
            .collect::<BTreeSet<_>>();
        self.presentation
            .reconcile_composites(TrackChannel::Annotation, &overlay_ids);
        self.presentation
            .reconcile_composites(TrackChannel::Reads, &BTreeSet::new());
        // Schema four has one presentation source of truth. Clear the legacy
        // mirrors after they have contributed to migration so in-memory
        // round trips cannot retain a second, contradictory eye/group/size.
        for track in &mut self.alignment_tracks {
            track.visible = true;
            track.collapsed = false;
            track.track_group = None;
            track.track_group_collapsed = false;
            track.read_track_group = None;
            track.read_track_group_collapsed = false;
            track.height = default_alignment_track_height();
            track.annotation_height = None;
            track.read_group_height = None;
            track.filters.show_reads = true;
        }
        for track in &mut self.tracks {
            track.visible = true;
            track.collapsed = false;
            track.height = 120.0;
        }
    }

    fn legacy_presentation_migration_notices(&self) -> Vec<String> {
        let mut notices = Vec::new();
        for channel in [TrackChannel::Annotation, TrackChannel::Reads] {
            let mut groups = BTreeMap::<u32, (usize, bool, bool)>::new();
            for track in &self.alignment_tracks {
                let (group_id, collapsed) = match channel {
                    TrackChannel::Annotation => (track.track_group, track.track_group_collapsed),
                    TrackChannel::Reads => {
                        (track.read_track_group, track.read_track_group_collapsed)
                    }
                };
                if let Some(group_id) = group_id {
                    let state = groups.entry(group_id).or_default();
                    state.0 = state.0.saturating_add(1);
                    state.1 |= collapsed;
                    state.2 |= !collapsed;
                }
            }
            for (group_id, (member_count, had_collapsed, had_expanded)) in groups {
                if member_count < 2 {
                    notices.push(format!(
                        "Discarded legacy {channel:?} group {group_id}: groups require at least two members"
                    ));
                } else if had_collapsed && had_expanded {
                    notices.push(format!(
                        "Repaired inconsistent legacy {channel:?} group {group_id}: collapsed state was applied because at least one member was collapsed"
                    ));
                }
            }
        }
        notices
    }

    pub fn to_json(&self) -> Result<String, SessionError> {
        let mut persisted = self.clone();
        persisted.normalize_presentation();
        persisted.validate()?;
        for track in &mut persisted.alignment_tracks {
            track.source = track.source.redacted_for_persistence();
            track.remote_index_url = track.remote_index_url.as_deref().map(redact_remote_url);
        }
        persisted.reference = persisted
            .reference
            .as_ref()
            .map(TrackSource::redacted_for_persistence);
        persisted.remote_reference_index_url = persisted
            .remote_reference_index_url
            .as_deref()
            .map(redact_remote_url);
        persisted.cytobands = persisted
            .cytobands
            .as_ref()
            .map(TrackSource::redacted_for_persistence);
        for track in &mut persisted.tracks {
            track.source = track.source.redacted_for_persistence();
        }
        Ok(format!("{}\n", serde_json::to_string_pretty(&persisted)?))
    }

    /// Reads a session and resolves every relative local source against the
    /// directory containing the session file.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        let project_directory = project_directory(path)?;
        let mut document = Self::from_json(&std::fs::read_to_string(path)?)?;
        document.resolve_project_paths(&project_directory);
        Ok(document)
    }

    /// Atomically writes a portable project session.
    ///
    /// Local sources inside the session directory are stored as relative
    /// paths. Sources outside it remain absolute. The completed JSON is
    /// flushed to a temporary file in the same directory before one atomic
    /// replacement, so an interrupted save cannot leave half a JSON file.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        let path = path.as_ref();
        let project_directory = project_directory_for_write(path)?;
        let json = self.to_project_json(&project_directory)?;
        atomic_write(path, json.as_bytes())?;
        Ok(())
    }

    /// Serializes the session using paths portable within `project_directory`.
    pub fn to_project_json(
        &self,
        project_directory: impl AsRef<Path>,
    ) -> Result<String, SessionError> {
        let project_directory = absolute_path(project_directory.as_ref())?;
        let mut portable = self.clone();
        portable.make_project_paths_portable(&project_directory)?;
        portable.to_json()
    }

    /// Resolves relative local sources against an explicit project root.
    pub fn resolve_project_paths(&mut self, project_directory: impl AsRef<Path>) {
        let project_directory = project_directory.as_ref();
        self.for_each_local_source_mut(|source| {
            let path = Path::new(source);
            if path.is_relative() {
                *source = project_directory.join(path).display().to_string();
            }
        });
    }

    /// Returns every unavailable local project source with its track identity.
    #[must_use]
    pub fn unresolved_local_sources(&self) -> Vec<UnresolvedSessionSource> {
        let mut unresolved = Vec::new();
        push_unresolved_source(&mut unresolved, "reference", self.reference.as_ref());
        push_unresolved_source(&mut unresolved, "cytobands", self.cytobands.as_ref());
        for track in &self.alignment_tracks {
            push_unresolved_source(
                &mut unresolved,
                &format!("alignment track {} ({})", track.id.get(), track.name),
                Some(&track.source),
            );
        }
        for track in &self.tracks {
            push_unresolved_source(
                &mut unresolved,
                &format!("track {} ({})", track.id.get(), track.name),
                Some(&track.source),
            );
        }
        unresolved
    }

    fn make_project_paths_portable(
        &mut self,
        project_directory: &Path,
    ) -> Result<(), SessionError> {
        let working_directory = std::env::current_dir()?;
        self.for_each_local_source_mut(|source| {
            let source_path = Path::new(source);
            let absolute = if source_path.is_absolute() {
                source_path.to_path_buf()
            } else {
                working_directory.join(source_path)
            };
            let absolute = absolute_path(&absolute).unwrap_or(absolute);
            if let Ok(relative) = absolute.strip_prefix(project_directory)
                && !relative.as_os_str().is_empty()
            {
                *source = relative.display().to_string();
            } else {
                *source = absolute.display().to_string();
            }
        });
        Ok(())
    }

    fn for_each_local_source_mut(&mut self, mut visit: impl FnMut(&mut String)) {
        for source in self
            .reference
            .iter_mut()
            .chain(self.cytobands.iter_mut())
            .chain(
                self.alignment_tracks
                    .iter_mut()
                    .map(|track| &mut track.source),
            )
            .chain(self.tracks.iter_mut().map(|track| &mut track.source))
        {
            if let TrackSource::Local { path } = source {
                visit(path);
            }
        }
    }

    fn validate(&self) -> Result<(), SessionError> {
        if self.schema_version != SESSION_SCHEMA_VERSION {
            return Err(SessionError::UnsupportedVersion(self.schema_version));
        }
        if self.locus.contig.is_empty() || self.locus.start >= self.locus.end {
            return Err(SessionError::InvalidLocus);
        }
        if self.panels.is_empty() || self.panels.len() > MAXIMUM_VIEW_PANELS {
            return Err(SessionError::InvalidPanels);
        }
        let mut panel_ids = HashSet::new();
        for panel in &self.panels {
            let mut read_panel_ids = HashSet::new();
            if panel.id.get() == 0
                || !panel_ids.insert(panel.id)
                || panel.locus.contig.is_empty()
                || panel.locus.start >= panel.locus.end
                || !panel.vertical_scroll_points.is_finite()
                || panel.vertical_scroll_points < 0.0
                || !panel.annotation_scroll_points.is_finite()
                || panel.annotation_scroll_points < 0.0
                || !panel.vertical_content_origin_points.is_finite()
                || panel.vertical_content_origin_points < 0.0
                || !panel.read_stack_scroll_points.is_finite()
                || panel.read_stack_scroll_points < 0.0
                || panel.read_panel_states.iter().any(|state| {
                    state.track_id.get() == 0
                        || !read_panel_ids.insert(state.track_id)
                        || !state.scroll_points.is_finite()
                        || state.scroll_points < 0.0
                        || !state.content_origin_points.is_finite()
                        || state.content_origin_points < 0.0
                        || state.latent_scroll_fraction.is_some_and(|fraction| {
                            !fraction.is_finite() || !(0.0..=1.0).contains(&fraction)
                        })
                        || !state.content_bottom_points.is_finite()
                        || state.content_bottom_points < 0.0
                        || state
                            .density
                            .is_some_and(|density| !density.is_finite() || density <= 0.0)
                })
            {
                return Err(SessionError::InvalidPanels);
            }
        }
        let Some(active_panel) = self
            .panels
            .iter()
            .find(|panel| panel.id == self.active_panel)
        else {
            return Err(SessionError::InvalidPanels);
        };
        if active_panel.locus != self.locus {
            return Err(SessionError::InvalidPanels);
        }
        if self.maximum_alignments == 0 {
            return Err(SessionError::InvalidAlignmentLimit);
        }
        validate_session_genome_metadata(self)?;
        if let Some(metadata) = &self.sample_metadata {
            metadata.validate()?;
        }
        if self.alignment_tracks.len() > MAXIMUM_ALIGNMENT_TRACKS {
            return Err(SessionError::TooManyAlignmentTracks);
        }
        validate_session_remote_indexes(self)?;
        // Track identity is an opaque value, so uniqueness must not require or
        // imply a presentation ordering. The vectors remain the sole source of
        // persisted track order.
        let mut ids = HashSet::new();
        for track in &self.alignment_tracks {
            if !ids.insert(track.id) {
                return Err(SessionError::DuplicateTrackId);
            }
            if track.name.trim().is_empty()
                || !valid_sample_association(track.sample_key.as_deref())
                || track.track_group == Some(0)
                || (track.track_group.is_none() && track.track_group_collapsed)
                || track.read_track_group == Some(0)
                || (track.read_track_group.is_none() && track.read_track_group_collapsed)
                || !track.height.is_finite()
                || track.height < 26.0
                || track
                    .annotation_height
                    .is_some_and(|height| !height.is_finite() || !(80.0..=600.0).contains(&height))
                || track.read_group_height.is_some_and(|height| {
                    !height.is_finite() || !(40.0..=1_200.0).contains(&height)
                })
            {
                return Err(SessionError::InvalidAlignmentTrack);
            }
            if track.maximum_alignments == 0 || track.maximum_alignments > self.maximum_alignments {
                return Err(SessionError::InvalidAlignmentLimit);
            }
            validate_filters(&track.filters)?;
        }
        if self.tracks.iter().any(|track| !ids.insert(track.id)) {
            return Err(SessionError::DuplicateTrackId);
        }
        if self.tracks.iter().any(|track| {
            track.name.trim().is_empty()
                || !valid_sample_association(track.sample_key.as_deref())
                || !track.height.is_finite()
                || track.height < 26.0
                || !valid_track_format(track)
                || (track.kind == TrackKind::Variant && !track.variant.is_valid())
                || (track.kind == TrackKind::Mutation && !track.mutation.is_valid())
                || (track.kind == TrackKind::Gwas && !track.gwas.is_valid())
                || (matches!(track.kind, TrackKind::Quantitative | TrackKind::CopyNumber)
                    && !track.quantitative.is_valid())
                || (track.kind == TrackKind::CopyNumber && !track.copy_number.is_valid())
                || (track.kind == TrackKind::Interaction && !track.interaction.is_valid())
                || (track.kind == TrackKind::Quantitative
                    && track.quantitative.overlay_group.is_some_and(|group| {
                        track.quantitative.scale_mode != crate::QuantitativeScaleMode::Shared
                            || track.quantitative.scale_group != Some(group)
                            || self.tracks.iter().any(|candidate| {
                                candidate.kind == TrackKind::Quantitative
                                    && candidate.quantitative.overlay_group == Some(group)
                                    && candidate.quantitative.scale_transform
                                        != track.quantitative.scale_transform
                            })
                    }))
                || (track.kind == TrackKind::Quantitative
                    && track.quantitative.operand_track.is_some_and(|operand| {
                        operand == track.id
                            || !self.tracks.iter().any(|candidate| {
                                candidate.id == operand && candidate.kind == TrackKind::Quantitative
                            })
                    }))
        }) {
            return Err(SessionError::InvalidAuxiliaryTrack);
        }
        validate_workspace_presentation(self, &ids)?;
        Ok(())
    }
}

fn validate_workspace_presentation(
    document: &SessionDocument,
    loaded_track_ids: &HashSet<TrackId>,
) -> Result<(), SessionError> {
    let presentation = &document.presentation;
    if !presentation.read_pane_height.is_finite()
        || !(100.0..=1_200.0).contains(&presentation.read_pane_height)
        || presentation
            .reference_preferred_height
            .is_some_and(|height| !height.is_finite() || !(26.0..=1_200.0).contains(&height))
    {
        return Err(SessionError::InvalidAlignmentTrack);
    }
    let mut pane_nodes = HashSet::new();
    for node in &presentation.pane_tracks {
        if !loaded_track_ids.contains(&node.track_id)
            || !pane_nodes.insert((node.channel, node.track_id))
            || node.preferred_height.is_some_and(|height| {
                !height.is_finite()
                    || match node.channel {
                        TrackChannel::Annotation => !(26.0..=1_200.0).contains(&height),
                        TrackChannel::Reads => !(40.0..=1_200.0).contains(&height),
                    }
            })
        {
            return Err(SessionError::InvalidAlignmentTrack);
        }
    }
    let alignment_ids = document
        .alignment_tracks
        .iter()
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    if document.panels.iter().any(|panel| {
        panel
            .read_panel_states
            .iter()
            .any(|state| !alignment_ids.contains(&state.track_id))
    }) {
        return Err(SessionError::InvalidAlignmentTrack);
    }
    let auxiliary_ids = document
        .tracks
        .iter()
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    let expected_count = alignment_ids
        .len()
        .saturating_mul(2)
        .saturating_add(auxiliary_ids.len());
    if pane_nodes.len() != expected_count
        || alignment_ids.iter().any(|id| {
            !pane_nodes.contains(&(TrackChannel::Annotation, *id))
                || !pane_nodes.contains(&(TrackChannel::Reads, *id))
        })
        || auxiliary_ids.iter().any(|id| {
            !pane_nodes.contains(&(TrackChannel::Annotation, *id))
                || pane_nodes.contains(&(TrackChannel::Reads, *id))
        })
    {
        return Err(SessionError::InvalidAlignmentTrack);
    }
    let mut group_ids = HashSet::new();
    let mut grouped_members = HashSet::new();
    for group in &presentation.groups {
        let mut members = HashSet::new();
        let positions = group
            .members
            .iter()
            .filter_map(|member| {
                presentation
                    .pane_tracks
                    .iter()
                    .filter(|node| node.channel == group.channel)
                    .position(|node| node.track_id == *member)
            })
            .collect::<Vec<_>>();
        if group.id == 0
            || group.label.trim().is_empty()
            || !group_ids.insert((group.channel, group.id))
            || group.members.len() < 2
            || group.members.iter().any(|member| {
                (match group.channel {
                    TrackChannel::Annotation => !loaded_track_ids.contains(member),
                    TrackChannel::Reads => !alignment_ids.contains(member),
                }) || !members.insert(*member)
                    || !grouped_members.insert((group.channel, *member))
                    || !pane_nodes.contains(&(group.channel, *member))
            })
            || positions.len() != group.members.len()
            || positions.windows(2).any(|pair| pair[1] != pair[0] + 1)
        {
            return Err(SessionError::InvalidAlignmentTrack);
        }
    }
    if presentation.next_group_id == 0
        || presentation
            .groups
            .iter()
            .any(|group| group.id >= presentation.next_group_id)
    {
        return Err(SessionError::InvalidAlignmentTrack);
    }
    let expected_composites = document
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Quantitative)
        .filter_map(|track| track.quantitative.overlay_group)
        .fold(BTreeMap::<u32, usize>::new(), |mut counts, id| {
            *counts.entry(id).or_default() += 1;
            counts
        })
        .into_iter()
        .filter_map(|(id, count)| (count >= 2).then_some((TrackChannel::Annotation, id)))
        .collect::<HashSet<_>>();
    let actual_composites = presentation
        .composites
        .iter()
        .map(|composite| (composite.channel, composite.id))
        .collect::<HashSet<_>>();
    if actual_composites.len() != presentation.composites.len()
        || actual_composites != expected_composites
        || presentation
            .composites
            .iter()
            .any(|composite| composite.id == 0)
    {
        return Err(SessionError::InvalidAlignmentTrack);
    }
    Ok(())
}

fn validate_session_remote_indexes(document: &SessionDocument) -> Result<(), SessionError> {
    let invalid_alignment = document
        .alignment_tracks
        .iter()
        .any(|track| !valid_remote_index(&track.source, track.remote_index_url.as_deref()));
    let invalid_reference = document.remote_reference_index_url.is_some()
        && document.reference.as_ref().is_none_or(|source| {
            !valid_remote_index(source, document.remote_reference_index_url.as_deref())
        });
    if invalid_alignment || invalid_reference {
        Err(SessionError::InvalidRemoteIndexUrl)
    } else {
        Ok(())
    }
}

fn valid_remote_index(source: &TrackSource, index_url: Option<&str>) -> bool {
    let Some(index_url) = index_url else {
        return true;
    };
    if !matches!(source, TrackSource::Remote { .. })
        || index_url.is_empty()
        || index_url.len() > MAXIMUM_REMOTE_INDEX_URL_BYTES
        || index_url.chars().any(char::is_control)
    {
        return false;
    }
    url::Url::parse(index_url)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn redact_remote_url(url: &str) -> String {
    match (TrackSource::Remote {
        url: url.to_owned(),
    })
    .redacted_for_persistence()
    {
        TrackSource::Remote { url } => url,
        TrackSource::Local { .. } => unreachable!(),
    }
}

fn valid_track_format(track: &TrackConfig) -> bool {
    match track.format {
        None => true,
        Some(TrackFormat::BigGenePred) => track.kind == TrackKind::GeneModel,
        Some(TrackFormat::BigNarrowPeak) => track.kind == TrackKind::Feature,
        Some(TrackFormat::TdfChannel { index }) => {
            track.kind == TrackKind::Quantitative && index < crate::MAXIMUM_TDF_CHANNELS
        }
    }
}

fn push_unresolved_source(
    unresolved: &mut Vec<UnresolvedSessionSource>,
    label: &str,
    source: Option<&TrackSource>,
) {
    let Some(TrackSource::Local { path }) = source else {
        return;
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        unresolved.push(UnresolvedSessionSource {
            label: label.to_owned(),
            path,
        });
    }
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    canonicalize_with_missing_tail(&absolute)
}

fn canonicalize_with_missing_tail(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) => {
                let Some(name) = ancestor.file_name() else {
                    return Err(error);
                };
                missing.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    return Err(error);
                };
                ancestor = parent;
            }
        }
    }
}

fn session_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn project_directory(path: &Path) -> std::io::Result<PathBuf> {
    absolute_path(session_parent(path))
}

fn project_directory_for_write(path: &Path) -> std::io::Result<PathBuf> {
    let parent = session_parent(path);
    std::fs::create_dir_all(parent)?;
    absolute_path(parent)
}

fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = session_parent(path);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn valid_sample_association(sample_key: Option<&str>) -> bool {
    sample_key.is_none_or(|key| {
        !key.trim().is_empty()
            && key.len() <= crate::MAXIMUM_SAMPLE_KEY_BYTES
            && !key.contains(['\t', '\r', '\n'])
    })
}

fn validate_session_genome_metadata(document: &SessionDocument) -> Result<(), SessionError> {
    if document
        .genome_name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty() || name.len() > 256)
    {
        return Err(SessionError::InvalidGenomeMetadata);
    }
    if document.contig_aliases.len() > MAXIMUM_CONTIG_ALIASES
        || document.contig_aliases.iter().any(|(alias, target)| {
            alias.is_empty()
                || target.is_empty()
                || alias.len() > 256
                || target.len() > 256
                || alias.contains([':', ' ', '\t', '\n'])
                || target.contains([':', ' ', '\t', '\n'])
        })
    {
        return Err(SessionError::InvalidContigAliases);
    }
    let unique_frames = document
        .reference_translation_frames
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if unique_frames.len() != document.reference_translation_frames.len()
        || unique_frames.len() > ReferenceTranslationFrame::ALL.len()
    {
        return Err(SessionError::InvalidReferenceTranslation);
    }
    validate_regions_and_review(document)?;
    Ok(())
}

fn validate_regions_and_review(document: &SessionDocument) -> Result<(), SessionError> {
    if document.regions_of_interest.len() > MAXIMUM_REGIONS_OF_INTEREST
        || document.review_items.len() > MAXIMUM_REVIEW_ITEMS
    {
        return Err(SessionError::InvalidRegionsOrReview);
    }
    let valid_locus = |locus: &SessionLocus| {
        !locus.contig.is_empty() && locus.start < locus.end && locus.contig.len() <= 256
    };
    let mut ids = HashSet::new();
    if document.regions_of_interest.iter().any(|region| {
        region.id == 0
            || !ids.insert(region.id)
            || region.name.trim().is_empty()
            || region.name.len() > 256
            || !valid_locus(&region.locus)
    }) {
        return Err(SessionError::InvalidRegionsOrReview);
    }
    ids.clear();
    if document.review_items.iter().any(|item| {
        item.id == 0
            || !ids.insert(item.id)
            || item.label.trim().is_empty()
            || item.label.len() > 256
            || item.notes.len() > MAXIMUM_REVIEW_NOTE_BYTES
            || item
                .assignment
                .as_ref()
                .is_some_and(|assignment| assignment.len() > 256)
            || !valid_locus(&item.locus)
    }) {
        return Err(SessionError::InvalidRegionsOrReview);
    }
    Ok(())
}

const fn default_minimum_allele_fraction() -> f32 {
    0.20
}

const fn default_true() -> bool {
    true
}

const fn default_one() -> u32 {
    1
}

const fn default_alignment_density() -> f32 {
    1.0
}

const fn default_sashimi_fixed_scale_maximum() -> u32 {
    100
}

const fn default_sbx_discordant_quality_maximum() -> u8 {
    10
}

const fn default_sbx_simplex_tail_quality_maximum() -> u8 {
    30
}

const fn default_alignment_track_height() -> f32 {
    360.0
}

const fn default_maximum_alignments() -> usize {
    50_000
}

fn validate_filters(filters: &SessionFilters) -> Result<(), SessionError> {
    let style_tags = [
        match filters.alignment_color_mode {
            AlignmentColorMode::Tag(tag) => Some(tag),
            _ => None,
        },
        match filters.alignment_group_mode {
            AlignmentGroupMode::Tag(tag) => Some(tag),
            _ => None,
        },
        match filters.alignment_sort_mode {
            AlignmentSortMode::Tag(tag) => Some(tag),
            _ => None,
        },
    ];
    if style_tags
        .into_iter()
        .flatten()
        .any(|tag| !tag[0].is_ascii_alphabetic() || !tag[1].is_ascii_alphanumeric())
    {
        return Err(SessionError::InvalidAlignmentStyleTag);
    }
    if !filters.minimum_allele_fraction.is_finite()
        || !(0.0..=1.0).contains(&filters.minimum_allele_fraction)
    {
        return Err(SessionError::InvalidAlleleFraction);
    }
    if filters.minimum_indel_size == 0
        || filters.minimum_event_label_size == 0
        || filters.minimum_junction_depth == 0
    {
        return Err(SessionError::InvalidAlignmentDisplayThreshold);
    }
    if filters.coverage_scale_maximum == Some(0) {
        return Err(SessionError::InvalidCoverageScale);
    }
    if filters.sashimi_fixed_scale_maximum == 0 {
        return Err(SessionError::InvalidAlignmentDisplayThreshold);
    }
    if let AlignmentColorMode::Sbx(options) = filters.alignment_color_mode
        && (options.discordant_quality_maximum >= options.simplex_tail_quality_maximum
            || options.simplex_tail_quality_maximum > 93)
    {
        return Err(SessionError::InvalidAlignmentDisplayThreshold);
    }
    if let AlignmentColorMode::Ultima(options) = filters.alignment_color_mode
        && options.maximum_homopolymer_length_override == Some(0)
    {
        return Err(SessionError::InvalidAlignmentDisplayThreshold);
    }
    if !filters.alignment_density.is_finite() || !(0.35..=9.0).contains(&filters.alignment_density)
    {
        return Err(SessionError::InvalidAlignmentDensity);
    }
    if filters
        .minimum_template_length
        .zip(filters.maximum_template_length)
        .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(SessionError::InvalidTemplateLengthBounds);
    }
    if filters.hidden_modification_codes.len() > 8
        || filters
            .hidden_modification_codes
            .iter()
            .any(|code| !valid_modification_code(code))
    {
        return Err(SessionError::InvalidModificationCodes);
    }
    Ok(())
}

fn next_available_track_id(tracks: &[TrackConfig]) -> TrackId {
    let mut next = 1_u64;
    while tracks.iter().any(|track| track.id == TrackId::new(next)) {
        next = next.saturating_add(1);
    }
    TrackId::new(next)
}

fn alignment_name_from_source(source: &TrackSource) -> String {
    let label = source.redacted_label();
    label
        .split(['/', '\\'])
        .next_back()
        .filter(|name| !name.is_empty())
        .unwrap_or("Alignments")
        .to_owned()
}

fn valid_modification_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii_alphabetic())
        || value
            .strip_prefix("CHEBI:")
            .is_some_and(|code| !code.is_empty() && code.bytes().all(|byte| byte.is_ascii_digit()))
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    SampleMetadata(#[from] SampleMetadataError),
    #[error("session schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("session locus is invalid")]
    InvalidLocus,
    #[error("session view panels or active panel are invalid")]
    InvalidPanels,
    #[error("session contig aliases are invalid or exceed {MAXIMUM_CONTIG_ALIASES} entries")]
    InvalidContigAliases,
    #[error("session genome metadata is invalid")]
    InvalidGenomeMetadata,
    #[error("session reference translation frames are invalid")]
    InvalidReferenceTranslation,
    #[error("session regions of interest or review items are invalid or exceed their limits")]
    InvalidRegionsOrReview,
    #[error("session alignment limit must be positive")]
    InvalidAlignmentLimit,
    #[error("session alignment track has an invalid id, name, or height")]
    InvalidAlignmentTrack,
    #[error("session custom remote index must be a bounded HTTP/S URL on a remote source")]
    InvalidRemoteIndexUrl,
    #[error("session alignment colour, group, or sort tag is not a valid SAM tag")]
    InvalidAlignmentStyleTag,
    #[error("session contains more than {MAXIMUM_ALIGNMENT_TRACKS} alignment tracks")]
    TooManyAlignmentTracks,
    #[error("session track ids must be unique across the workspace")]
    DuplicateTrackId,
    #[error("session minimum allele fraction must be between zero and one")]
    InvalidAlleleFraction,
    #[error("session alignment display thresholds must be positive")]
    InvalidAlignmentDisplayThreshold,
    #[error("session fixed coverage scale must be positive")]
    InvalidCoverageScale,
    #[error("session alignment density must be finite and between 0.35 and 9")]
    InvalidAlignmentDensity,
    #[error("session minimum template length cannot exceed its maximum")]
    InvalidTemplateLengthBounds,
    #[error("session hidden modification codes are invalid")]
    InvalidModificationCodes,
    #[error("session auxiliary track has an invalid name, height, or variant setting")]
    InvalidAuxiliaryTrack,
    #[error("could not import IGV session: {0}")]
    Igv(String),
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
