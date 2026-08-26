use std::{
    collections::{BTreeSet, HashSet},
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

pub const SESSION_SCHEMA_VERSION: u32 = 3;
pub const MAXIMUM_ALIGNMENT_TRACKS: usize = 64;
pub const MAXIMUM_VIEW_PANELS: usize = 16;
pub const MAXIMUM_CONTIG_ALIASES: usize = 10_000;
pub const MAXIMUM_REGIONS_OF_INTEREST: usize = 100_000;
pub const MAXIMUM_REVIEW_ITEMS: usize = 100_000;
pub const MAXIMUM_REVIEW_NOTE_BYTES: usize = 16_384;
const MAXIMUM_REMOTE_INDEX_URL_BYTES: usize = 16_384;
const LEGACY_SESSION_SCHEMA_VERSION: u32 = 1;
const MULTI_TRACK_SESSION_SCHEMA_VERSION: u32 = 2;

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
    #[serde(default)]
    pub annotation_scroll_points: f32,
    #[serde(default)]
    pub vertical_content_origin_points: f32,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOrderDirection {
    #[default]
    Natural,
    Reverse,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
    #[serde(default = "default_true")]
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
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub collapsed: bool,
    /// Optional presentation group for alignment tracks. Group membership is
    /// independent of alignment read grouping and quantitative overlays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_group: Option<u32>,
    /// Whether the complete presentation group is minimized. The value is
    /// mirrored across every member so projects remain self-contained when
    /// tracks are reordered or removed.
    #[serde(default)]
    pub track_group_collapsed: bool,
    /// Optional presentation group for the read-data pane. This is deliberately
    /// independent from `track_group`, which groups annotation summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_track_group: Option<u32>,
    /// Whether this track's read-data group is minimized.
    #[serde(default)]
    pub read_track_group_collapsed: bool,
    #[serde(default = "default_alignment_track_height")]
    pub height: f32,
    /// Optional independently resized height of this track's annotation
    /// summary. The shared read-pane height remains in `height`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation_height: Option<f32>,
    /// Optional independently resized height of this sample's read group
    /// inside the shared, scrollable read pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_group_height: Option<f32>,
    #[serde(default)]
    pub filters: SessionFilters,
    #[serde(default = "default_maximum_alignments")]
    pub maximum_alignments: usize,
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
    pub locus: SessionLocus,
    pub panels: Vec<SessionViewPanel>,
    pub active_panel: PanelId,
    pub alignment_tracks: Vec<SessionAlignmentTrack>,
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
        SessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            locus: self.locus.clone(),
            panels: vec![SessionViewPanel::new(panel_id, self.locus)],
            active_panel: panel_id,
            alignment_tracks: self.alignment_tracks,
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
        SessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            locus: self.locus.clone(),
            panels: vec![SessionViewPanel::new(PanelId::new(1), self.locus)],
            active_panel: PanelId::new(1),
            alignment_tracks,
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
            locus: locus.clone(),
            panels: vec![SessionViewPanel::new(panel_id, locus)],
            active_panel: panel_id,
            alignment_tracks: Vec::new(),
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
        let document = match probe.schema_version {
            SESSION_SCHEMA_VERSION => serde_json::from_str(json)?,
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
        document.validate()?;
        Ok(document)
    }

    pub fn to_json(&self) -> Result<String, SessionError> {
        self.validate()?;
        let mut persisted = self.clone();
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
        Ok(())
    }
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
