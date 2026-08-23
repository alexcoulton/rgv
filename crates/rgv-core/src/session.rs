use std::{
    collections::{BTreeSet, HashSet},
    io::Write,
    path::{Path, PathBuf},
};

use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContigAliases, GeneticCode, PanelId, ReferenceTranslationFrame, SampleMetadataError,
    SampleMetadataTable, TrackConfig, TrackFormat, TrackId, TrackKind, TrackSource,
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
            show_coverage_center_line: true,
            alignment_density: default_alignment_density(),
            inspector_activation: InspectorActivation::default(),
            read_base_display_mode: ReadBaseDisplayMode::default(),
            shade_read_bases_by_quality: true,
            minimum_indel_size: 1,
            minimum_event_label_size: 1,
            show_modified_bases: true,
            show_modification_frequency: true,
            minimum_modification_probability: 0,
            modified_base_color_mode: ModifiedBaseColorMode::ByType,
            hidden_modification_codes: Vec::new(),
            include_secondary: true,
            include_supplementary: true,
            auxiliary_predicates: Vec::new(),
        }
    }
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
    #[serde(default = "default_alignment_track_height")]
    pub height: f32,
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
            height: default_alignment_track_height(),
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

    pub fn import_igv(xml: &str) -> Result<Self, SessionError> {
        Ok(Self::import_igv_with_report(xml)?.document)
    }

    #[allow(clippy::too_many_lines)]
    pub fn import_igv_with_report(xml: &str) -> Result<IgvSessionImport, SessionError> {
        #[derive(Default)]
        struct Resource {
            path: String,
            name: Option<String>,
        }

        #[derive(Default)]
        struct TrackState {
            id: Option<String>,
            name: Option<String>,
            visible: Option<bool>,
            height: Option<f32>,
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut locus = None;
        let mut genome = None;
        let mut resources = Vec::new();
        let mut track_states = Vec::new();
        let mut report = IgvImportReportBuilder::default();
        loop {
            match reader.read_event() {
                Ok(Event::Start(event) | Event::Empty(event)) => {
                    let element = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                    let mut attributes = Vec::new();
                    for attribute in event.attributes().with_checks(false) {
                        let attribute =
                            attribute.map_err(|error| SessionError::Igv(error.to_string()))?;
                        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                        let value = attribute
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .map_err(|error| SessionError::Igv(error.to_string()))?
                            .into_owned();
                        attributes.push((key, value));
                    }
                    match element.as_str() {
                        "Session" | "Global" => {
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "locus" => {
                                        locus = Some(parse_igv_locus(&value)?);
                                        report.imported("Session.locus -> active RGV locus");
                                    }
                                    "genome" => genome = Some(value),
                                    "version" => report.imported(
                                        "Session.version accepted as IGV compatibility metadata",
                                    ),
                                    _ => report.ignored(format!(
                                        "{element}.{key} has no RGV project equivalent"
                                    )),
                                }
                            }
                        }
                        "Resources" => {
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "Resources.{key} has no RGV project equivalent"
                                ));
                            }
                        }
                        "Resource" => {
                            let mut resource = Resource::default();
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "path" => resource.path = value,
                                    "name" | "label" if resource.name.is_none() => {
                                        resource.name = Some(value);
                                    }
                                    "index" => report.ignored(
                                        "Resource.index is not persisted by the RGV project model",
                                    ),
                                    "url" => report.ignored(
                                        "Resource.url feature-link metadata is not imported",
                                    ),
                                    _ => report.ignored(format!(
                                        "Resource.{key} has no RGV project equivalent"
                                    )),
                                }
                            }
                            if resource.path.is_empty() {
                                report.ignored("Resource without a path was ignored");
                            } else {
                                resources.push(resource);
                            }
                        }
                        "Panel" => {
                            report.approximated(
                                "IGV panel placement was flattened into RGV's ordered track stack",
                            );
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "Panel.{key} is not preserved after panel flattening"
                                ));
                            }
                        }
                        "Track" => {
                            let mut state = TrackState::default();
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "id" => state.id = Some(value),
                                    "name" | "displayName" if state.name.is_none() => {
                                        state.name = Some(value);
                                    }
                                    "visible" => match value.parse::<bool>() {
                                        Ok(value) => state.visible = Some(value),
                                        Err(_) => report.ignored(
                                            "Track.visible had an invalid Boolean value",
                                        ),
                                    },
                                    "height" => match value.parse::<f32>() {
                                        Ok(value) if value.is_finite() && value > 0.0 => {
                                            state.height = Some(value);
                                        }
                                        _ => report
                                            .ignored("Track.height had an invalid numeric value"),
                                    },
                                    "expand" => report.approximated(
                                        "Track.expand was not mapped because IGV and RGV density modes differ",
                                    ),
                                    _ => report.ignored(format!(
                                        "Track.{key} has no exact RGV project equivalent"
                                    )),
                                }
                            }
                            track_states.push(state);
                        }
                        "DataRange" => {
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "DataRange.{key} is not yet imported into RGV scales"
                                ));
                            }
                        }
                        _ => {
                            report.ignored(format!("IGV element <{element}> was ignored"));
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "{element}.{key} was ignored with its containing element"
                                ));
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => return Err(SessionError::Igv(error.to_string())),
            }
        }
        let locus =
            locus.ok_or_else(|| SessionError::Igv("IGV session has no locus".to_owned()))?;
        let mut document = Self::new(locus);
        if let Some(genome) = genome {
            if infer_track_kind(&genome) == TrackKind::Reference {
                document.reference = Some(source_from_string(genome));
                report.imported("Session.genome FASTA -> RGV reference source");
            } else {
                document.genome_name = Some(genome);
                report.approximated(
                    "Session.genome identifier was retained as an assembly label; RGV did not infer a reference file",
                );
            }
        }
        let mut next_id = 1_u64;
        let resource_count = resources.len();
        for resource in resources {
            let source = source_from_string(resource.path.clone());
            let kind = infer_track_kind(&resource.path);
            let default_name = resource
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&resource.path)
                .to_owned();
            let name = resource.name.unwrap_or(default_name);
            if kind == TrackKind::Alignment {
                document.alignment_tracks.push(SessionAlignmentTrack::new(
                    TrackId::new(next_id),
                    name,
                    source,
                ));
                next_id = next_id.saturating_add(1);
            } else if kind == TrackKind::Reference && document.reference.is_none() {
                document.reference = Some(source);
            } else {
                document
                    .tracks
                    .push(TrackConfig::new(TrackId::new(next_id), name, kind, source));
                next_id = next_id.saturating_add(1);
            }
        }
        report.imported(format!(
            "{} Resource.path entr{} -> RGV data sources",
            resource_count,
            if resource_count == 1 { "y" } else { "ies" }
        ));
        for state in track_states {
            let Some(id) = state.id.as_deref() else {
                report.ignored("Track without an id could not be associated with a Resource");
                continue;
            };
            let derived_alignment = [
                ("_coverage", true),
                ("_junctions", false),
                ("_splice_junctions", false),
            ]
            .into_iter()
            .find_map(|(suffix, coverage)| id.strip_suffix(suffix).map(|base| (base, coverage)));
            if let Some((base, coverage)) = derived_alignment
                && let Some(track) = document
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track_source_location(&track.source) == base)
            {
                if let Some(visible) = state.visible {
                    if coverage {
                        track.filters.show_coverage = visible;
                    } else {
                        track.filters.show_junctions = visible;
                    }
                }
                report.approximated(if coverage {
                    "IGV coverage subtrack visibility -> RGV alignment coverage visibility"
                } else {
                    "IGV junction subtrack visibility -> RGV alignment junction visibility"
                });
                if state.name.is_some() || state.height.is_some() {
                    report.ignored(
                        "Derived IGV coverage/junction name and height were not applied independently",
                    );
                }
                continue;
            }
            if let Some(track) = document
                .alignment_tracks
                .iter_mut()
                .find(|track| track_source_location(&track.source) == id)
            {
                apply_igv_common_track_state(
                    &mut track.name,
                    &mut track.visible,
                    &mut track.height,
                    state.name,
                    state.visible,
                    state.height,
                    &mut report,
                );
                continue;
            }
            if let Some(track) = document
                .tracks
                .iter_mut()
                .find(|track| track_source_location(&track.source) == id)
            {
                apply_igv_common_track_state(
                    &mut track.name,
                    &mut track.visible,
                    &mut track.height,
                    state.name,
                    state.visible,
                    state.height,
                    &mut report,
                );
                continue;
            }
            report.ignored("Track configuration did not match a Resource and was ignored");
        }
        document.validate()?;
        Ok(IgvSessionImport {
            document,
            report: report.finish(),
        })
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
                || !track.height.is_finite()
                || track.height < 26.0
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

fn track_source_location(source: &TrackSource) -> &str {
    match source {
        TrackSource::Local { path } => path,
        TrackSource::Remote { url } => url,
    }
}

fn apply_igv_common_track_state(
    target_name: &mut String,
    target_visible: &mut bool,
    target_height: &mut f32,
    name: Option<String>,
    visible: Option<bool>,
    height: Option<f32>,
    report: &mut IgvImportReportBuilder,
) {
    if let Some(name) = name {
        if name.trim().is_empty() {
            report.ignored("Track.name was blank and the Resource name was retained");
        } else {
            *target_name = name;
            report.imported("Track.name -> RGV track name");
        }
    }
    if let Some(visible) = visible {
        *target_visible = visible;
        report.imported("Track.visible -> RGV track visibility");
    }
    if let Some(height) = height {
        let bounded = height.clamp(26.0, 4_096.0);
        *target_height = bounded;
        if (bounded - height).abs() > f32::EPSILON {
            report.approximated("Track.height was clamped to RGV's safe track-height range");
        } else {
            report.imported("Track.height -> RGV track height");
        }
    }
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

fn parse_igv_locus(raw: &str) -> Result<SessionLocus, SessionError> {
    let (contig, range) = raw
        .split_once(':')
        .ok_or_else(|| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let range = range.replace(',', "");
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let start = start
        .parse::<u64>()
        .map_err(|_| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    if start == 0 || start > end {
        return Err(SessionError::Igv(format!("invalid IGV locus {raw}")));
    }
    Ok(SessionLocus {
        contig: contig.to_owned(),
        start: start - 1,
        end,
    })
}

fn source_from_string(value: String) -> TrackSource {
    if value.starts_with("http://") || value.starts_with("https://") {
        TrackSource::Remote { url: value }
    } else {
        TrackSource::Local { path: value }
    }
}

fn infer_track_kind(path: &str) -> TrackKind {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = Path::new(path);
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let inner_extension = path
        .file_stem()
        .and_then(|value| Path::new(value).extension())
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    match extension.as_str() {
        "bam" | "cram" => TrackKind::Alignment,
        "fa" | "fasta" | "fna" => TrackKind::Reference,
        "vcf" | "bcf" => TrackKind::Variant,
        "gz" if inner_extension.as_deref() == Some("vcf") => TrackKind::Variant,
        "linear" | "logistic" | "assoc" | "qassoc" | "gwas" => TrackKind::Gwas,
        "gz" if matches!(
            inner_extension.as_deref(),
            Some("linear" | "logistic" | "assoc" | "qassoc" | "gwas")
        ) =>
        {
            TrackKind::Gwas
        }
        "maf" => TrackKind::Mutation,
        "txt" | "gz" if inner_extension.as_deref() == Some("maf") => TrackKind::Mutation,
        "bw" | "bigwig" | "tdf" | "bedgraph" | "bdg" | "wig" | "wiggle" => TrackKind::Quantitative,
        "seg" | "cbs" => TrackKind::CopyNumber,
        "bedpe" | "interact" => TrackKind::Interaction,
        "gff" | "gff3" | "gtf" | "genepred" | "gp" | "refflat" | "biggenepred" => {
            TrackKind::GeneModel
        }
        "gz" if matches!(
            inner_extension.as_deref(),
            Some("gff" | "gff3" | "gtf" | "genepred" | "gp" | "refflat")
        ) =>
        {
            TrackKind::GeneModel
        }
        _ => TrackKind::Feature,
    }
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
mod tests {
    use super::*;

    fn add_test_alignment(document: &mut SessionDocument) -> &mut SessionAlignmentTrack {
        document.alignment_tracks.push(SessionAlignmentTrack::new(
            TrackId::new(1),
            "reads.bam".to_owned(),
            TrackSource::Local {
                path: "reads.bam".to_owned(),
            },
        ));
        document.alignment_tracks.last_mut().unwrap()
    }

    #[test]
    fn json_round_trip_preserves_schema_and_sources() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let alignment = add_test_alignment(&mut document);
        alignment.source = TrackSource::Remote {
            url: "https://example.org/sample.bam".to_owned(),
        };
        alignment.remote_index_url =
            Some("https://indexes.example.org/sample.custom.bai".to_owned());
        alignment.sample_key = Some("tumour".to_owned());
        let filters = &mut alignment.filters;
        filters.alignment_color_mode = AlignmentColorMode::Tag(*b"YC");
        filters.alignment_group_mode = AlignmentGroupMode::Chimeric;
        filters.alignment_group_order = AlignmentOrderDirection::Reverse;
        filters.alignment_sort_mode = AlignmentSortMode::Base;
        filters.alignment_sort_order = AlignmentOrderDirection::Reverse;
        filters.view_as_pairs = true;
        filters.minimum_template_length = Some(100);
        filters.maximum_template_length = Some(900);
        filters.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
        filters.minimum_base_quality = 20;
        filters.minimum_allele_fraction = 0.12;
        filters.quality_weighted_alleles = false;
        filters.show_reference = false;
        filters.show_coverage = false;
        filters.show_junctions = false;
        filters.minimum_junction_depth = 3;
        filters.minimum_junction_flank = 12;
        filters.junction_display_mode = JunctionDisplayMode::Expanded;
        filters.sashimi_scale_mode = SashimiScaleMode::Shared;
        filters.sashimi_fixed_scale_maximum = 250;
        filters.sashimi_count_mode = SashimiCountMode::CountsPerMillion;
        filters.show_junction_labels = false;
        filters.show_reads = false;
        filters.coverage_scale_maximum = Some(500);
        filters.show_coverage_center_line = false;
        filters.alignment_density = 0.55;
        filters.inspector_activation = InspectorActivation::Hover;
        filters.read_base_display_mode = ReadBaseDisplayMode::All;
        filters.shade_read_bases_by_quality = false;
        filters.minimum_indel_size = 5;
        filters.minimum_event_label_size = 12;
        filters.show_modified_bases = false;
        filters.show_modification_frequency = false;
        filters.minimum_modification_probability = 128;
        filters.modified_base_color_mode = ModifiedBaseColorMode::ModifiedVsUnmodified;
        filters.hidden_modification_codes = vec!["m".to_owned(), "CHEBI:76792".to_owned()];
        document
            .contig_aliases
            .insert("1".to_owned(), "chr1".to_owned());
        document.reference_reverse_complement = true;
        document.reference_translation_frames = vec![
            ReferenceTranslationFrame::Forward1,
            ReferenceTranslationFrame::Reverse2,
        ];
        document.reference_genetic_code = GeneticCode::VertebrateMitochondrial;
        document.reference = Some(TrackSource::Remote {
            url: "https://example.org/reference.fa".to_owned(),
        });
        document.remote_reference_index_url =
            Some("https://indexes.example.org/reference.custom.fai".to_owned());
        document.sample_metadata =
            Some(SampleMetadataTable::from_tsv("sample\tcondition\ntumour\tcase\n").unwrap());
        assert_eq!(
            SessionDocument::from_json(&document.to_json().unwrap()).unwrap(),
            document
        );
    }

    #[test]
    fn invalid_track_sample_associations_are_rejected() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        add_test_alignment(&mut document).sample_key = Some(" \n".to_owned());
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentTrack)
        ));
    }

    #[test]
    fn custom_remote_indexes_require_bounded_http_urls_and_remote_sources() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let alignment = add_test_alignment(&mut document);
        alignment.remote_index_url = Some("https://example.org/sample.bai".to_owned());
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRemoteIndexUrl)
        ));

        document.alignment_tracks[0].source = TrackSource::Remote {
            url: "https://example.org/sample.bam".to_owned(),
        };
        document.alignment_tracks[0].remote_index_url =
            Some("ftp://example.org/sample.bai".to_owned());
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRemoteIndexUrl)
        ));

        document.alignment_tracks[0].remote_index_url =
            Some("https://example.org/sample.bai".to_owned());
        document.remote_reference_index_url = Some("https://example.org/reference.fai".to_owned());
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRemoteIndexUrl)
        ));

        document.reference = Some(TrackSource::Remote {
            url: "https://example.org/reference.fa".to_owned(),
        });
        assert!(document.to_json().is_ok());

        document.remote_reference_index_url = Some(format!(
            "https://example.org/{}",
            "x".repeat(MAXIMUM_REMOTE_INDEX_URL_BYTES)
        ));
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRemoteIndexUrl)
        ));
    }

    #[test]
    fn regions_and_review_queue_round_trip_and_reject_invalid_private_state() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr6".to_owned(),
            start: 100,
            end: 200,
        });
        document.regions_of_interest.push(RegionOfInterest {
            id: 1,
            name: "candidate interval".to_owned(),
            locus: SessionLocus {
                contig: "chr6".to_owned(),
                start: 110,
                end: 140,
            },
            color: [12, 34, 56, 48],
        });
        document.review_items.push(ReviewItem {
            id: 1,
            label: "candidate variant".to_owned(),
            locus: SessionLocus {
                contig: "chr6".to_owned(),
                start: 120,
                end: 121,
            },
            status: ReviewStatus::InProgress,
            notes: "Needs orthogonal review".to_owned(),
            assignment: Some("reviewer-a".to_owned()),
        });

        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored.regions_of_interest, document.regions_of_interest);
        assert_eq!(restored.review_items, document.review_items);

        document.review_items[0].notes = "x".repeat(MAXIMUM_REVIEW_NOTE_BYTES + 1);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRegionsOrReview)
        ));
        document.review_items[0].notes.clear();
        document.regions_of_interest.push(RegionOfInterest {
            id: 1,
            name: "duplicate id".to_owned(),
            locus: SessionLocus {
                contig: "chr6".to_owned(),
                start: 150,
                end: 160,
            },
            color: [1, 2, 3, 4],
        });
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidRegionsOrReview)
        ));
    }

    #[test]
    fn variant_track_review_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "cohort".to_owned(),
            TrackKind::Variant,
            TrackSource::Local {
                path: "cohort.bcf".to_owned(),
            },
        );
        track.variant = crate::VariantTrackSettings {
            minimum_quality: Some(30.0),
            pass_only: true,
            variant_types: vec![crate::VariantTypeGroup::Snv],
            genotype: crate::VariantGenotypeFilter::NonReference,
            sample_subset_enabled: true,
            sample_subset: vec!["proband".to_owned()],
            genotype_grouping: crate::VariantGenotypeGrouping::Genotype,
            color_mode: crate::VariantColorMode::Genotype,
            ..crate::VariantTrackSettings::default()
        };
        document.tracks.push(track);
        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored.tracks[0].variant, document.tracks[0].variant);

        document.tracks[0].variant.minimum_allele_frequency = Some(2.0);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn custom_bigbed_schema_round_trips_and_must_match_the_track_kind() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.tracks.push(
            TrackConfig::new(
                TrackId::new(1),
                "genes".to_owned(),
                TrackKind::GeneModel,
                TrackSource::Local {
                    path: "genes.bb".to_owned(),
                },
            )
            .with_format(TrackFormat::BigGenePred),
        );
        document.tracks.push(
            TrackConfig::new(
                TrackId::new(2),
                "peaks".to_owned(),
                TrackKind::Feature,
                TrackSource::Local {
                    path: "peaks.bb".to_owned(),
                },
            )
            .with_format(TrackFormat::BigNarrowPeak),
        );
        document.tracks[0].feature.display_mode = crate::FeatureDisplayMode::Squished;

        let json = document.to_json().unwrap();
        assert!(json.contains("\"format\": \"big_gene_pred\""));
        assert!(json.contains("\"format\": \"big_narrow_peak\""));
        let restored = SessionDocument::from_json(&json).unwrap();
        assert_eq!(restored.tracks[0].format, Some(TrackFormat::BigGenePred));
        assert_eq!(
            restored.tracks[0].feature.display_mode,
            crate::FeatureDisplayMode::Squished
        );
        assert_eq!(restored.tracks[1].format, Some(TrackFormat::BigNarrowPeak));

        document.tracks[1].kind = TrackKind::Quantitative;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn tdf_channel_identity_round_trips_and_requires_a_quantitative_track() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.tracks.push(
            TrackConfig::new(
                TrackId::new(3),
                "sample two".to_owned(),
                TrackKind::Quantitative,
                TrackSource::Local {
                    path: "archive.tdf".to_owned(),
                },
            )
            .with_format(TrackFormat::TdfChannel { index: 1 }),
        );
        let json = document.to_json().unwrap();
        let restored = SessionDocument::from_json(&json).unwrap();
        assert_eq!(
            restored.tracks[0].format,
            Some(TrackFormat::TdfChannel { index: 1 })
        );

        document.tracks[0].kind = TrackKind::Feature;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
        document.tracks[0].kind = TrackKind::Quantitative;
        document.tracks[0].format = Some(TrackFormat::TdfChannel {
            index: crate::MAXIMUM_TDF_CHANNELS,
        });
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn quantitative_track_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "signal".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: "signal.bw".to_owned(),
            },
        );
        track.quantitative = crate::QuantitativeTrackSettings {
            scale_mode: crate::QuantitativeScaleMode::Shared,
            fixed_minimum: -4.0,
            fixed_maximum: 8.0,
            baseline: 1.0,
            scale_transform: crate::QuantitativeScaleTransform::SignedLog10,
            display_mode: crate::QuantitativeDisplayMode::Heatmap,
            window_function: crate::QuantitativeWindowFunction::Maximum,
            missing_mode: crate::QuantitativeMissingMode::Marker,
            positive_color: [1, 2, 3, 4],
            negative_color: [5, 6, 7, 8],
            opacity: 0.5,
            arithmetic: crate::QuantitativeArithmetic::None,
            operand_track: None,
            show_range_labels: false,
            overlay_group: Some(7),
            scale_group: Some(7),
        };
        document.tracks.push(track);

        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(
            restored.tracks[0].quantitative,
            document.tracks[0].quantitative
        );

        document.tracks.push(TrackConfig::new(
            TrackId::new(2),
            "control".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: "control.bw".to_owned(),
            },
        ));
        document.tracks[0].quantitative.arithmetic = crate::QuantitativeArithmetic::Subtract;
        document.tracks[0].quantitative.operand_track = Some(TrackId::new(2));
        assert!(document.to_json().is_ok());

        document.tracks[0].quantitative.operand_track = Some(TrackId::new(1));
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
        document.tracks[0].quantitative.arithmetic = crate::QuantitativeArithmetic::None;
        document.tracks[0].quantitative.operand_track = None;

        document.tracks[0].quantitative.fixed_minimum = 10.0;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
        document.tracks[0].quantitative.fixed_minimum = -10.0;
        document.tracks[0].quantitative.overlay_group = Some(0);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn interaction_track_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "loops".to_owned(),
            TrackKind::Interaction,
            TrackSource::Local {
                path: "loops.interact".to_owned(),
            },
        );
        track.interaction = crate::InteractionTrackSettings {
            display_mode: crate::InteractionDisplayMode::Blocks,
            minimum_score: Some(25.0),
            fixed_maximum_score: Some(1_000.0),
            show_labels: false,
        };
        document.tracks.push(track);
        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored, document);

        document.tracks[0].interaction.fixed_maximum_score = Some(0.0);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn gwas_track_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr6".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "association study".to_owned(),
            TrackKind::Gwas,
            TrackSource::Local {
                path: "study.assoc.gz".to_owned(),
            },
        );
        track.gwas = crate::GwasTrackSettings {
            color_mode: crate::GwasColorMode::Alternating,
            point_size: 6.0,
            maximum_score: 40.0,
            uniform_color: [1, 2, 3, 255],
            alternating_color_one: [4, 5, 6, 255],
            alternating_color_two: [7, 8, 9, 255],
        };
        document.tracks.push(track);
        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored, document);
        assert_eq!(infer_track_kind("study.assoc.gz"), TrackKind::Gwas);

        document.tracks[0].gwas.maximum_score = 0.0;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn copy_number_track_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr6".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "cohort".to_owned(),
            TrackKind::CopyNumber,
            TrackSource::Local {
                path: "cohort.seg".to_owned(),
            },
        );
        track.copy_number = crate::CopyNumberTrackSettings {
            value_mode: crate::CopyNumberValueMode::LogRatio,
            sort_mode: crate::CopyNumberSortMode::Amplification,
            sort_locus: Some(crate::CopyNumberSortLocus {
                contig: "chr6".to_owned(),
                start: 120,
                end: 180,
            }),
            row_height: 18.0,
            show_sample_names: false,
            show_values: true,
        };
        document.tracks.push(track);
        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored, document);

        document.tracks[0].copy_number.row_height = 2.0;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn mutation_track_state_round_trips_and_is_validated() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr17".to_owned(),
            start: 100,
            end: 200,
        });
        let mut track = TrackConfig::new(
            TrackId::new(1),
            "somatic mutations".to_owned(),
            TrackKind::Mutation,
            TrackSource::Local {
                path: "cohort.maf".to_owned(),
            },
        );
        track.mutation = crate::MutationTrackSettings {
            color_mode: crate::MutationColorMode::VariantType,
            row_height: 22.0,
            show_sample_names: false,
            show_gene_labels: false,
        };
        document.tracks.push(track);

        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored, document);

        document.tracks[0].mutation.row_height = 2.0;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAuxiliaryTrack)
        ));
    }

    #[test]
    fn version_three_round_trip_preserves_ordered_panels_and_active_locus() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr2".to_owned(),
            start: 20,
            end: 40,
        });
        document.panels = vec![
            SessionViewPanel::new(
                PanelId::new(8),
                SessionLocus {
                    contig: "chr1".to_owned(),
                    start: 100,
                    end: 200,
                },
            ),
            SessionViewPanel::new(PanelId::new(3), document.locus.clone()),
        ];
        document.active_panel = PanelId::new(3);
        add_test_alignment(&mut document);

        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();

        assert_eq!(restored, document);
        assert_eq!(
            restored
                .panels
                .iter()
                .map(|panel| panel.id)
                .collect::<Vec<_>>(),
            vec![PanelId::new(8), PanelId::new(3)]
        );
        assert_eq!(restored.active_panel, PanelId::new(3));
    }

    #[test]
    fn version_two_migrates_to_one_active_version_three_panel() {
        let document = SessionDocument::from_json(include_str!(
            "../../../validation/four-track-session-v2.json"
        ))
        .unwrap();

        assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
        assert_eq!(document.panels.len(), 1);
        assert_eq!(document.panels[0].id, PanelId::new(1));
        assert_eq!(document.panels[0].locus, document.locus);
        assert_eq!(document.active_panel, PanelId::new(1));
    }

    #[test]
    fn invalid_panel_collections_are_rejected() {
        let locus = SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 10,
        };
        let valid = SessionDocument::new(locus.clone());

        let mut empty = valid.clone();
        empty.panels.clear();
        assert!(matches!(empty.to_json(), Err(SessionError::InvalidPanels)));

        let mut zero_id = valid.clone();
        zero_id.panels[0].id = PanelId::new(0);
        zero_id.active_panel = PanelId::new(0);
        assert!(matches!(
            zero_id.to_json(),
            Err(SessionError::InvalidPanels)
        ));

        let mut duplicate = valid.clone();
        duplicate.panels.push(duplicate.panels[0].clone());
        assert!(matches!(
            duplicate.to_json(),
            Err(SessionError::InvalidPanels)
        ));

        let mut missing_active = valid.clone();
        missing_active.active_panel = PanelId::new(99);
        assert!(matches!(
            missing_active.to_json(),
            Err(SessionError::InvalidPanels)
        ));

        let mut mismatched_locus = valid.clone();
        mismatched_locus.locus = SessionLocus {
            contig: "chr2".to_owned(),
            start: 0,
            end: 10,
        };
        assert!(matches!(
            mismatched_locus.to_json(),
            Err(SessionError::InvalidPanels)
        ));

        let mut excessive = valid;
        excessive.panels = (1..=MAXIMUM_VIEW_PANELS + 1)
            .map(|value| {
                SessionViewPanel::new(PanelId::new(u64::try_from(value).unwrap()), locus.clone())
            })
            .collect();
        assert!(matches!(
            excessive.to_json(),
            Err(SessionError::InvalidPanels)
        ));
    }

    #[test]
    fn early_version_one_sessions_receive_pileup_defaults() {
        let json = r#"{"schema_version":1,"locus":{"contig":"chr1","start":0,"end":1},"alignment":{"Local":{"path":"reads.bam"}},"reference":null,"tracks":[],"filters":{"minimum_mapping_quality":null,"include_secondary":true,"include_supplementary":true,"auxiliary_predicates":[]},"maximum_alignments":1}"#;
        let document = SessionDocument::from_json(json).unwrap();
        assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
        assert_eq!(document.alignment_tracks.len(), 1);
        assert_eq!(document.alignment_tracks[0].maximum_alignments, 1);
        let filters = &document.alignment_tracks[0].filters;
        assert_eq!(filters.alignment_color_mode, AlignmentColorMode::None);
        assert_eq!(filters.alignment_group_mode, AlignmentGroupMode::None);
        assert_eq!(
            filters.alignment_group_order,
            AlignmentOrderDirection::Natural
        );
        assert_eq!(filters.alignment_sort_mode, AlignmentSortMode::Start);
        assert_eq!(
            filters.alignment_sort_order,
            AlignmentOrderDirection::Natural
        );
        assert!(!filters.view_as_pairs);
        assert_eq!(filters.minimum_template_length, None);
        assert_eq!(filters.maximum_template_length, None);
        assert_eq!(
            filters.overlapping_mate_policy,
            OverlappingMatePolicy::CountOnce
        );
        assert_eq!(filters.minimum_base_quality, 0);
        assert!((filters.minimum_allele_fraction - 0.20).abs() < f32::EPSILON);
        assert!(filters.quality_weighted_alleles);
        assert!(filters.show_reference);
        assert!(filters.show_coverage);
        assert!(filters.show_junctions);
        assert_eq!(filters.minimum_junction_depth, 1);
        assert_eq!(filters.minimum_junction_flank, 0);
        assert_eq!(
            filters.junction_display_mode,
            JunctionDisplayMode::Collapsed
        );
        assert_eq!(filters.sashimi_scale_mode, SashimiScaleMode::Automatic);
        assert_eq!(filters.sashimi_fixed_scale_maximum, 100);
        assert_eq!(filters.sashimi_count_mode, SashimiCountMode::Raw);
        assert!(filters.show_junction_labels);
        assert!(filters.show_reads);
        assert_eq!(filters.coverage_scale_maximum, None);
        assert!(filters.show_coverage_center_line);
        assert!((filters.alignment_density - 1.0).abs() < f32::EPSILON);
        assert_eq!(filters.inspector_activation, InspectorActivation::Click);
        assert_eq!(
            filters.read_base_display_mode,
            ReadBaseDisplayMode::Mismatches
        );
        assert!(filters.shade_read_bases_by_quality);
        assert_eq!(filters.minimum_indel_size, 1);
        assert_eq!(filters.minimum_event_label_size, 1);
        assert!(filters.show_modified_bases);
        assert!(filters.show_modification_frequency);
        assert_eq!(filters.minimum_modification_probability, 0);
        assert_eq!(
            filters.modified_base_color_mode,
            ModifiedBaseColorMode::ByType
        );
        assert!(filters.hidden_modification_codes.is_empty());
    }

    #[test]
    fn version_one_migration_avoids_auxiliary_track_id_collisions() {
        let document =
            SessionDocument::from_json(include_str!("../../../validation/session-v1.json"))
                .unwrap();
        assert_eq!(document.alignment_tracks[0].id, TrackId::new(2));
        assert_eq!(document.alignment_tracks[0].maximum_alignments, 1234);
        assert_eq!(
            document.alignment_tracks[0].filters.minimum_mapping_quality,
            Some(20)
        );
        assert_eq!(document.maximum_alignments, 1234);
    }

    #[test]
    fn multiple_alignment_tracks_preserve_order_and_independent_settings() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 100,
        });
        let mut tumour = SessionAlignmentTrack::new(
            TrackId::new(10),
            "Tumour".to_owned(),
            TrackSource::Local {
                path: "tumour.bam".to_owned(),
            },
        );
        tumour.filters.minimum_mapping_quality = Some(30);
        tumour.filters.alignment_color_mode = AlignmentColorMode::Haplotype;
        let mut normal = SessionAlignmentTrack::new(
            TrackId::new(11),
            "Normal".to_owned(),
            TrackSource::Local {
                path: "normal.bam".to_owned(),
            },
        );
        normal.filters.minimum_mapping_quality = Some(10);
        normal.filters.show_modification_frequency = false;
        document.alignment_tracks = vec![tumour.clone(), normal.clone()];

        let restored = SessionDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored.alignment_tracks, vec![tumour, normal]);
    }

    #[test]
    fn duplicate_workspace_track_ids_are_rejected() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        add_test_alignment(&mut document);
        document.tracks.push(TrackConfig::new(
            TrackId::new(1),
            "genes".to_owned(),
            TrackKind::GeneModel,
            TrackSource::Local {
                path: "genes.gtf".to_owned(),
            },
        ));
        assert!(matches!(
            document.to_json(),
            Err(SessionError::DuplicateTrackId)
        ));
    }

    #[test]
    fn malformed_or_excessive_hidden_modification_codes_are_rejected() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        add_test_alignment(&mut document);
        document.alignment_tracks[0]
            .filters
            .hidden_modification_codes = vec!["not-a-code".to_owned()];
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidModificationCodes)
        ));

        document.alignment_tracks[0]
            .filters
            .hidden_modification_codes = (0..9).map(|_| "m".to_owned()).collect();
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidModificationCodes)
        ));
    }

    #[test]
    fn invalid_coverage_scale_and_alignment_density_are_rejected() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        add_test_alignment(&mut document);
        document.alignment_tracks[0].filters.coverage_scale_maximum = Some(0);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidCoverageScale)
        ));

        document.alignment_tracks[0].filters.coverage_scale_maximum = None;
        document.alignment_tracks[0].filters.alignment_density = f32::NAN;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentDensity)
        ));

        document.alignment_tracks[0].filters.alignment_density = 0.34;
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentDensity)
        ));
    }

    #[test]
    fn invalid_alignment_style_tags_are_rejected_for_every_style_mode() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        add_test_alignment(&mut document);

        document.alignment_tracks[0].filters.alignment_sort_mode =
            AlignmentSortMode::Tag([0, b'1']);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentStyleTag)
        ));

        document.alignment_tracks[0].filters.alignment_sort_mode = AlignmentSortMode::Start;
        document.alignment_tracks[0].filters.alignment_group_mode = AlignmentGroupMode::Tag(*b"1X");
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentStyleTag)
        ));
    }

    #[test]
    fn reversed_template_length_bounds_are_rejected() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        let alignment = add_test_alignment(&mut document);
        alignment.filters.minimum_template_length = Some(500);
        alignment.filters.maximum_template_length = Some(100);
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidTemplateLengthBounds)
        ));
    }

    #[test]
    fn per_track_alignment_limit_cannot_exceed_the_workspace_ceiling() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        document.maximum_alignments = 1_000;
        add_test_alignment(&mut document).maximum_alignments = 1_001;

        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentLimit)
        ));
    }

    #[test]
    fn session_rejects_more_alignment_engines_than_the_workspace_limit() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1,
        });
        document.alignment_tracks = (0..=MAXIMUM_ALIGNMENT_TRACKS)
            .map(|index| {
                SessionAlignmentTrack::new(
                    TrackId::new(u64::try_from(index).unwrap().saturating_add(1)),
                    format!("reads-{index}.bam"),
                    TrackSource::Local {
                        path: format!("reads-{index}.bam"),
                    },
                )
            })
            .collect();

        assert!(matches!(
            document.to_json(),
            Err(SessionError::TooManyAlignmentTracks)
        ));
    }

    #[test]
    fn four_track_failure_isolation_fixture_is_a_valid_v2_session() {
        let document = SessionDocument::from_json(include_str!(
            "../../../validation/four-track-session-v2.json"
        ))
        .unwrap();

        assert_eq!(document.alignment_tracks.len(), 4);
        assert_eq!(document.alignment_tracks[0].id, TrackId::new(1));
        assert_eq!(document.alignment_tracks[3].id, TrackId::new(4));
        assert!(
            document.alignment_tracks[3]
                .source
                .redacted_label()
                .contains("missing")
        );
        assert_eq!(document.maximum_alignments, 50_000);
    }

    #[test]
    fn multi_panel_four_track_fixture_is_a_valid_v3_session() {
        let document = SessionDocument::from_json(include_str!(
            "../../../validation/multi-panel-four-track-session-v3.json"
        ))
        .unwrap();

        assert_eq!(document.schema_version, SESSION_SCHEMA_VERSION);
        assert_eq!(document.alignment_tracks.len(), 4);
        assert_eq!(document.panels.len(), 2);
        assert_eq!(document.panels[0].id, PanelId::new(7));
        assert_eq!(document.panels[1].id, PanelId::new(3));
        assert_eq!(document.active_panel, PanelId::new(3));
        assert_eq!(document.locus, document.panels[1].locus);
    }

    #[test]
    fn partial_igv_import_extracts_locus_and_resources() {
        let xml = r#"<Session genome="hg38" locus="chr1:101-200"><Resources><Resource path="reads.bam"/><Resource path="genes.gff3"/><Resource path="https://example.org/signal.bw"/></Resources></Session>"#;
        let document = SessionDocument::import_igv(xml).unwrap();
        assert_eq!((document.locus.start, document.locus.end), (100, 200));
        assert_eq!(
            document.alignment_tracks[0].source,
            TrackSource::Local {
                path: "reads.bam".to_owned()
            }
        );
        assert_eq!(document.tracks.len(), 2);
        assert_eq!(document.tracks[0].kind, TrackKind::GeneModel);
        assert_eq!(document.tracks[1].kind, TrackKind::Quantitative);
    }

    #[test]
    fn track_kind_inference_recognizes_gene_models_and_narrow_peaks() {
        for path in [
            "genes.genePred",
            "genes.genePred.gz",
            "genes.refFlat",
            "genes.refFlat.gz",
        ] {
            assert_eq!(infer_track_kind(path), TrackKind::GeneModel, "{path}");
        }
        assert_eq!(infer_track_kind("calls.narrowPeak"), TrackKind::Feature);
        assert_eq!(infer_track_kind("calls.narrowPeak.gz"), TrackKind::Feature);
        assert_eq!(
            infer_track_kind("archived-signal.tdf"),
            TrackKind::Quantitative
        );
    }

    #[test]
    fn igv_import_preserves_every_alignment_resource_in_order() {
        let xml = r#"<Session locus="chr1:1-10"><Resources><Resource path="tumour.bam"/><Resource path="normal.cram"/><Resource path="genes.gtf"/></Resources></Session>"#;

        let document = SessionDocument::import_igv(xml).unwrap();

        assert_eq!(document.alignment_tracks.len(), 2);
        assert_eq!(document.alignment_tracks[0].name, "tumour.bam");
        assert_eq!(document.alignment_tracks[1].name, "normal.cram");
        assert_eq!(document.tracks.len(), 1);
        assert_eq!(document.tracks[0].id, TrackId::new(3));
    }

    #[test]
    fn igv_import_applies_supported_track_state_and_reports_every_semantic_category() {
        let xml = r#"
            <Session genome="hg38" locus="chr1:101-200" version="8" custom="x">
              <Resources>
                <Resource path="tumour.bam" name="Tumour resource" index="tumour.bam.bai" url="https://example.org/details" type="bam"/>
              </Resources>
              <Panel height="900" name="DataPanel" width="1200">
                <Track id="tumour.bam" name="Tumour reads" visible="false" height="17" renderer="ALIGNMENTS" expand="false">
                  <DataRange minimum="0" maximum="100"/>
                </Track>
                <Track id="tumour.bam_coverage" visible="false" autoScale="true"/>
              </Panel>
              <Filter name="not-supported"/>
            </Session>
        "#;

        let imported = SessionDocument::import_igv_with_report(xml).unwrap();
        let track = &imported.document.alignment_tracks[0];
        assert_eq!(imported.document.genome_name.as_deref(), Some("hg38"));
        assert_eq!(track.name, "Tumour reads");
        assert!(!track.visible);
        assert!((track.height - 26.0).abs() < f32::EPSILON);
        assert!(!track.filters.show_coverage);
        assert!(
            imported
                .report
                .imported
                .iter()
                .any(|notice| notice.contains("Track.name"))
        );
        assert!(
            imported
                .report
                .approximated
                .iter()
                .any(|notice| notice.contains("panel placement"))
        );
        for ignored in [
            "Session.custom",
            "Resource.index",
            "Resource.url",
            "Resource.type",
            "Panel.height",
            "Track.renderer",
            "DataRange.minimum",
            "Filter.name",
        ] {
            assert!(
                imported
                    .report
                    .ignored
                    .iter()
                    .any(|notice| notice.contains(ignored)),
                "missing report for {ignored}: {:?}",
                imported.report
            );
        }
        assert_eq!(imported.report.omitted_notice_count, 0);
    }

    #[test]
    fn future_session_versions_are_rejected() {
        assert!(matches!(
            SessionDocument::from_json(include_str!("../../../validation/future-session-v99.json")),
            Err(SessionError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn project_sessions_store_inside_sources_relatively_and_resolve_them_on_read() {
        let project = tempfile::tempdir().unwrap();
        let data_directory = project.path().join("data");
        std::fs::create_dir(&data_directory).unwrap();
        let reads = data_directory.join("reads.bam");
        let reference = data_directory.join("reference.fa");
        std::fs::write(&reads, b"reads").unwrap();
        std::fs::write(&reference, b"reference").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();

        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.reference = Some(TrackSource::Local {
            path: reference.display().to_string(),
        });
        document.alignment_tracks.push(SessionAlignmentTrack::new(
            TrackId::new(1),
            "reads".to_owned(),
            TrackSource::Local {
                path: reads.display().to_string(),
            },
        ));
        document.tracks.push(TrackConfig::new(
            TrackId::new(2),
            "outside".to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: outside.path().display().to_string(),
            },
        ));
        let session_path = project.path().join("analysis.json");

        document.write(&session_path).unwrap();

        let canonical_reads = reads.canonicalize().unwrap();
        let canonical_reference = reference.canonicalize().unwrap();
        let canonical_outside = outside.path().canonicalize().unwrap();
        let stored = std::fs::read_to_string(&session_path).unwrap();
        assert!(stored.contains(&format!("data{}reads.bam", std::path::MAIN_SEPARATOR)));
        assert!(stored.contains(&format!("data{}reference.fa", std::path::MAIN_SEPARATOR)));
        assert!(stored.contains(&canonical_outside.display().to_string()));

        let reopened = SessionDocument::read(&session_path).unwrap();
        assert_eq!(
            reopened.alignment_tracks[0].source,
            TrackSource::Local {
                path: canonical_reads.display().to_string()
            }
        );
        assert_eq!(
            reopened.reference,
            Some(TrackSource::Local {
                path: canonical_reference.display().to_string()
            })
        );
        assert!(reopened.unresolved_local_sources().is_empty());
        let mut expected = document.clone();
        expected.alignment_tracks[0].source = TrackSource::Local {
            path: canonical_reads.display().to_string(),
        };
        expected.reference = Some(TrackSource::Local {
            path: canonical_reference.display().to_string(),
        });
        expected.tracks[0].source = TrackSource::Local {
            path: canonical_outside.display().to_string(),
        };
        assert_eq!(reopened, expected);

        let relocated = tempfile::tempdir().unwrap();
        std::fs::create_dir(relocated.path().join("data")).unwrap();
        std::fs::copy(&session_path, relocated.path().join("analysis.json")).unwrap();
        std::fs::copy(&reads, relocated.path().join("data/reads.bam")).unwrap();
        std::fs::copy(&reference, relocated.path().join("data/reference.fa")).unwrap();
        let moved = SessionDocument::read(relocated.path().join("analysis.json")).unwrap();
        let relocated_reads = relocated
            .path()
            .join("data/reads.bam")
            .canonicalize()
            .unwrap();
        assert_eq!(
            moved.alignment_tracks[0].source,
            TrackSource::Local {
                path: relocated_reads.display().to_string()
            }
        );
        assert!(moved.unresolved_local_sources().is_empty());
    }

    #[test]
    fn unresolved_project_sources_are_reported_individually() {
        let project = tempfile::tempdir().unwrap();
        let session_path = project.path().join("analysis.json");
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.reference = Some(TrackSource::Local {
            path: project.path().join("missing.fa").display().to_string(),
        });
        document.alignment_tracks.push(SessionAlignmentTrack::new(
            TrackId::new(1),
            "tumour".to_owned(),
            TrackSource::Local {
                path: project.path().join("missing.bam").display().to_string(),
            },
        ));
        document.write(&session_path).unwrap();

        let reopened = SessionDocument::read(&session_path).unwrap();
        let issues = reopened.unresolved_local_sources();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].label, "reference");
        assert_eq!(issues[1].label, "alignment track 1 (tumour)");
        assert!(issues.iter().all(|issue| issue.path.is_absolute()));
    }

    #[test]
    fn repeated_atomic_session_saves_replace_the_complete_document() {
        let project = tempfile::tempdir().unwrap();
        let session_path = project.path().join("analysis.json");
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.write(&session_path).unwrap();
        document.locus.start = 300;
        document.locus.end = 400;
        document.panels[0].locus = document.locus.clone();
        document.write(&session_path).unwrap();

        let reopened = SessionDocument::read(&session_path).unwrap();
        assert_eq!(reopened.locus.start, 300);
        assert_eq!(reopened.locus.end, 400);
    }

    #[test]
    fn failed_session_save_leaves_the_last_good_project_untouched() {
        let project = tempfile::tempdir().unwrap();
        let session_path = project.path().join("analysis.json");
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        document.write(&session_path).unwrap();
        document.locus.start = 500;
        document.locus.end = 400;
        assert!(matches!(
            document.write(&session_path),
            Err(SessionError::InvalidLocus)
        ));

        let reopened = SessionDocument::read(&session_path).unwrap();
        assert_eq!(reopened.locus.start, 100);
        assert_eq!(reopened.locus.end, 200);
    }

    #[test]
    fn session_json_never_contains_remote_credentials() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        let alignment = add_test_alignment(&mut document);
        alignment.source = TrackSource::Remote {
            url: "https://alice:secret@example.org/sample.bam?token=private#fragment".to_owned(),
        };
        alignment.remote_index_url = Some(
            "https://index-user:index-secret@example.org/sample.bai?access_token=index-private"
                .to_owned(),
        );
        document.tracks.push(TrackConfig::new(
            TrackId::new(2),
            "signal".to_owned(),
            TrackKind::Quantitative,
            TrackSource::Remote {
                url: "https://example.org/signal.bw?X-Amz-Signature=private".to_owned(),
            },
        ));
        document.cytobands = Some(TrackSource::Remote {
            url: "https://example.org/cytobands.txt?access_token=private".to_owned(),
        });
        document.reference = Some(TrackSource::Remote {
            url: "https://example.org/reference.fa".to_owned(),
        });
        document.remote_reference_index_url =
            Some("https://ref-user:ref-secret@example.org/reference.fai?token=private".to_owned());

        let json = document.to_json().unwrap();
        assert!(!json.contains("alice"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("token"));
        assert!(!json.contains("X-Amz"));
        assert!(!json.contains("index-user"));
        assert!(!json.contains("index-secret"));
        assert!(!json.contains("ref-user"));
        assert!(!json.contains("ref-secret"));
        assert!(json.contains("https://example.org/sample.bam"));
        assert!(json.contains("https://example.org/sample.bai"));
        assert!(json.contains("https://example.org/reference.fai"));
        assert!(json.contains("https://example.org/signal.bw"));
        assert!(json.contains("https://example.org/cytobands.txt"));
    }

    #[test]
    fn bisulfite_context_round_trips_without_changing_older_defaults() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        add_test_alignment(&mut document)
            .filters
            .alignment_color_mode = AlignmentColorMode::Bisulfite(BisulfiteContext::Wcg);

        let json = document.to_json().unwrap();
        assert!(json.contains("bisulfite"));
        assert!(json.contains("wcg"));
        let reopened = SessionDocument::from_json(&json).unwrap();
        assert_eq!(
            reopened.alignment_tracks[0].filters.alignment_color_mode,
            AlignmentColorMode::Bisulfite(BisulfiteContext::Wcg)
        );
        assert_eq!(
            SessionFilters::default().alignment_color_mode,
            AlignmentColorMode::None
        );
    }

    #[test]
    fn smrt_kinetics_mode_round_trips_without_changing_older_defaults() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        add_test_alignment(&mut document)
            .filters
            .alignment_color_mode =
            AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsReversePulseWidth);

        let json = document.to_json().unwrap();
        assert!(json.contains("smrt_kinetics"));
        assert!(json.contains("ccs_reverse_pulse_width"));
        let reopened = SessionDocument::from_json(&json).unwrap();
        assert_eq!(
            reopened.alignment_tracks[0].filters.alignment_color_mode,
            AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsReversePulseWidth)
        );
        assert_eq!(
            SessionFilters::default().alignment_color_mode,
            AlignmentColorMode::None
        );
    }

    #[test]
    fn ultima_display_options_round_trip_and_reject_zero_override() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        add_test_alignment(&mut document)
            .filters
            .alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions {
            quality_mode: UltimaQualityMode::Minimum,
            maximum_homopolymer_length_override: Some(14),
        });

        let json = document.to_json().unwrap();
        assert!(json.contains("ultima"));
        assert!(json.contains("minimum"));
        let reopened = SessionDocument::from_json(&json).unwrap();
        assert_eq!(
            reopened.alignment_tracks[0].filters.alignment_color_mode,
            document.alignment_tracks[0].filters.alignment_color_mode
        );

        if let AlignmentColorMode::Ultima(options) =
            &mut document.alignment_tracks[0].filters.alignment_color_mode
        {
            options.maximum_homopolymer_length_override = Some(0);
        }
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentDisplayThreshold)
        ));
    }

    #[test]
    fn sbx_display_options_round_trip_and_reject_reversed_thresholds() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
        });
        add_test_alignment(&mut document)
            .filters
            .alignment_color_mode = AlignmentColorMode::Sbx(SbxDisplayOptions {
            simplex_tail_mode: SbxSimplexTailMode::Hide,
            discordant_quality_maximum: 12,
            simplex_tail_quality_maximum: 32,
        });

        let json = document.to_json().unwrap();
        let reopened = SessionDocument::from_json(&json).unwrap();
        assert_eq!(
            reopened.alignment_tracks[0].filters.alignment_color_mode,
            document.alignment_tracks[0].filters.alignment_color_mode
        );

        if let AlignmentColorMode::Sbx(options) =
            &mut document.alignment_tracks[0].filters.alignment_color_mode
        {
            options.simplex_tail_quality_maximum = options.discordant_quality_maximum;
        }
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentDisplayThreshold)
        ));

        if let AlignmentColorMode::Sbx(options) =
            &mut document.alignment_tracks[0].filters.alignment_color_mode
        {
            options.discordant_quality_maximum = 10;
            options.simplex_tail_quality_maximum = 94;
        }
        assert!(matches!(
            document.to_json(),
            Err(SessionError::InvalidAlignmentDisplayThreshold)
        ));
    }
}
