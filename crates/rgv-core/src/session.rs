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
/// Additive revision of schema [`SESSION_SCHEMA_VERSION`]. Bump it whenever a
/// field or enum variant is added that an older build of the same major
/// schema would drop or fail to parse. Files without writer metadata are
/// revision zero.
pub const SESSION_SCHEMA_MINOR_VERSION: u32 = 1;
pub const MAXIMUM_ALIGNMENT_TRACKS: usize = 64;
/// Bound on non-alignment tracks in one session document.
pub const MAXIMUM_SESSION_TRACKS: usize = 4_096;
pub const MAXIMUM_VIEW_PANELS: usize = 16;
/// Largest complete-read admission limit a session or an automation setting may request.
pub const MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT: usize = 10_000_000;
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
    /// How faithfully `document.locus` reflects `Session.locus`. Anything but
    /// [`IgvLocusResolution::Interval`] means the document holds a one-base
    /// placeholder that the caller must replace once a dictionary is known.
    pub locus_resolution: IgvLocusResolution,
}

/// The outcome of translating IGV's `Session.locus`, which may name things
/// only a reference dictionary or an annotation can place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IgvLocusResolution {
    /// `document.locus` is the interval IGV showed (the first, for a
    /// multi-locus session).
    Interval,
    /// IGV named a whole contig (`chr1`) or a feature (`BRCA1`).
    /// `document.locus` is a placeholder on `name`; resolve `name` with
    /// [`crate::Locus::resolve`] or a feature search.
    Named { name: String },
    /// IGV's whole-genome view (`All`), a missing locus, or an unparseable
    /// one. `document.locus` is a placeholder; open the default locus.
    Unresolved { requested: String },
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
        let mut end = MAXIMUM_IGV_NOTICE_BYTES;
        while !notice.is_char_boundary(end) {
            end -= 1;
        }
        notice.truncate(end);
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReadBaseDisplayMode {
    /// Draw only read bases that differ from the reference.
    #[default]
    Mismatches,
    /// Draw every read base.
    All,
    /// Draw no read bases.
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModifiedBaseColorMode {
    /// One colour per modification type (exact palette unverified).
    #[default]
    ByType,
    /// A single colour for every modification call (exact rendering unverified).
    Mono,
    /// Contrast modified calls with unmodified canonical bases.
    ModifiedVsUnmodified,
}

/// Reference-sequence context used to interpret bisulfite-conversion evidence.
///
/// The context is written in the direction of the cytosine-bearing strand. `H` is A, C, or T;
/// `W` is A or T. Both reference strands are considered by the layout layer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BisulfiteContext {
    /// `CG` (`CpG`) context.
    #[default]
    Cg,
    /// `CHG` context (`H` is A, C, or T).
    Chg,
    /// `CHH` context (`H` is A, C, or T).
    Chh,
    /// `HCG` context (`H` is A, C, or T).
    Hcg,
    /// `GCH` context (`H` is A, C, or T).
    Gch,
    /// `WCG` context (`W` is A or T).
    Wcg,
}

/// Per-base SMRT kinetic signal selected for alignment colouring.
///
/// Subread/CLR records use the `ip` and `pw` arrays. CCS records carry separate forward and
/// reverse-strand arrays; the renderer selects the appropriate physical tag from the requested
/// strand and the record's alignment orientation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SmrtKineticsMode {
    /// Inter-pulse duration of subread/CLR records (the `ip` array).
    #[default]
    SubreadIpd,
    /// Pulse width of subread/CLR records (the `pw` array).
    SubreadPulseWidth,
    /// Forward-strand inter-pulse duration of CCS records.
    CcsForwardIpd,
    /// Forward-strand pulse width of CCS records.
    CcsForwardPulseWidth,
    /// Reverse-strand inter-pulse duration of CCS records.
    CcsReverseIpd,
    /// Reverse-strand pulse width of CCS records.
    CcsReversePulseWidth,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UltimaQualityMode {
    /// Take a homopolymer indel's quality from the base whose `tp` value matches the indel's
    /// direction.
    #[default]
    Directional,
    /// Take a homopolymer indel's quality as the minimum base quality across the homopolymer.
    Minimum,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UltimaDisplayOptions {
    /// How the quality of a homopolymer indel is derived (default `directional`).
    #[serde(default)]
    pub quality_mode: UltimaQualityMode,
    /// Overrides the Ultima `mc` read-group field when present. Maximum homopolymer length, in
    /// bases; omit to use the value from the file's read group.
    #[serde(default)]
    pub maximum_homopolymer_length_override: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SbxSimplexTailMode {
    /// Draw simplex-tail bases like any other base.
    Off,
    /// Mark simplex-tail bases.
    #[default]
    Highlight,
    /// Do not draw simplex-tail bases.
    Hide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SbxDisplayOptions {
    /// How simplex-tail bases are drawn (default `highlight`).
    #[serde(default)]
    pub simplex_tail_mode: SbxSimplexTailMode,
    /// Highest Phred base quality, 0 to 255, at which a base counts as discordant when
    /// classifying insertions (default 10).
    #[serde(default = "default_sbx_discordant_quality_maximum")]
    pub discordant_quality_maximum: u8,
    /// Highest Phred base quality, 0 to 255, at which a run of bases at either end of a read
    /// counts as simplex tail (default 30).
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OverlappingMatePolicy {
    /// Count a base covered by both mates of one template once.
    #[default]
    CountOnce,
    /// Count a base covered by both mates of one template twice, once per mate.
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

/// How an RNA-seq library's reads relate to the transcribed strand. Used to give a splice
/// junction a strand when no `XS`/`ts` tag names one and the splice motif is not canonical.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LibraryStrandedness {
    /// Read orientation says nothing about the transcribed strand.
    #[default]
    Unstranded,
    /// The first read of a pair (or a single-end read) aligns on the transcribed strand
    /// (e.g. ligation / "fr-secondstrand" libraries).
    FirstOfPairSense,
    /// The first read of a pair (or a single-end read) aligns opposite the transcribed strand
    /// (e.g. dUTP / "fr-firststrand" libraries).
    FirstOfPairAntisense,
}

/// What a Sashimi arc's label shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SashimiLabelMode {
    /// The junction's read count (or CPM).
    #[default]
    Counts,
    /// Percent spliced in at the donor and the acceptor.
    Psi,
    /// No labels.
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SashimiCountMode {
    #[default]
    Raw,
    CountsPerMillion,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentColorMode {
    /// Neutral colour for every read.
    #[default]
    None,
    /// Colour by alignment strand (forward or reverse).
    Strand,
    /// Colour by mapping quality (MAPQ).
    MappingQuality,
    /// Colour by read group (the `RG` tag).
    ReadGroup,
    /// Colour by haplotype (the `HP` tag).
    Haplotype,
    /// Colour by absolute template length and pair classification.
    TemplateLength,
    /// Colour by the relative orientation of the two mates.
    PairOrientation,
    /// Colour by pair classification, distinguishing unexpected pairs from proper ones.
    UnexpectedPair,
    /// Colour by the contig the mate maps to.
    MateContig,
    /// Colour by the strand of the first read of the pair.
    FirstOfPairStrand,
    /// Bisulfite-conversion colouring for the given cytosine context, e.g.
    /// `{"bisulfite": "cg"}`.
    Bisulfite(BisulfiteContext),
    /// SMRT kinetics colouring by the given per-base signal, e.g.
    /// `{"smrt_kinetics": "subread_ipd"}`.
    SmrtKinetics(SmrtKineticsMode),
    /// Ultima homopolymer-indel quality overlays, e.g. `{"ultima": {}}` for the defaults.
    Ultima(UltimaDisplayOptions),
    /// SBX insertion-discordance and simplex-tail overlays, e.g. `{"sbx": {}}` for the
    /// defaults.
    Sbx(SbxDisplayOptions),
    /// Colour by the value of a SAM auxiliary tag, given as its two ASCII byte values, e.g.
    /// `{"tag": [72, 80]}` for `HP`. The first byte must be a letter and the second a letter
    /// or digit.
    Tag([u8; 2]),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentGroupMode {
    /// No grouping: one block containing every read.
    #[default]
    None,
    /// Group into forward-strand and reverse-strand reads.
    Strand,
    /// Group by read group (the `RG` tag); reads without it form their own group.
    ReadGroup,
    /// Group by haplotype (the `HP` tag); reads without it form their own group.
    Haplotype,
    /// Group into reads with and without a supplementary-alignment (`SA`) tag.
    Chimeric,
    /// Group into supplementary records and primary/secondary records.
    Supplementary,
    /// Group into reads with and without a soft or hard clip at their left (lower-coordinate)
    /// end.
    LeftClip,
    /// Group into reads with and without a soft or hard clip at their right
    /// (higher-coordinate) end.
    RightClip,
    /// Group by the relative orientation of the two mates.
    PairOrientation,
    /// Group into expected (proper or concordant), unexpected, and unpaired reads.
    UnexpectedPair,
    /// Group by the contig the mate maps to.
    MateContig,
    /// Group by position in the template (first, last, unknown, or ambiguous).
    ReadOrder,
    /// Group by the strand of the first read of the pair.
    FirstOfPairStrand,
    /// Group by the read base observed at the anchor position.
    Base,
    /// Group by the insertion observed at the anchor position.
    Insertion,
    /// Group by the value of a SAM auxiliary tag, given as its two ASCII byte values, e.g.
    /// `{"tag": [72, 80]}` for `HP`. Reads without the tag form their own group.
    Tag([u8; 2]),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentOrderDirection {
    /// The mode's own order (for example ascending start, or highest mapping quality first).
    #[default]
    Natural,
    /// The reverse of the mode's own order.
    Reverse,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentSortMode {
    /// Ascending alignment start, then end.
    #[default]
    Start,
    /// Forward-strand reads before reverse-strand reads.
    Strand,
    /// By the strand of the first read of the pair: forward first, unknown last.
    FirstOfPairStrand,
    /// Highest mapping quality (MAPQ) first.
    MappingQuality,
    /// By read name, in byte order.
    ReadName,
    /// By read group (the `RG` tag); reads without it last.
    ReadGroup,
    /// By haplotype (the `HP` tag); reads without it last.
    Haplotype,
    /// Longest aligned length first.
    AlignedLength,
    /// Longest clipping at the left (lower-coordinate) end first.
    LeftClip,
    /// Longest clipping at the right (higher-coordinate) end first.
    RightClip,
    /// Longest absolute template length first.
    TemplateLength,
    /// By the relative orientation of the two mates.
    PairOrientation,
    /// By the contig and then the position the mate maps to.
    MateContig,
    /// By pair classification.
    PairStatus,
    /// By position in the template (first read of the pair before last).
    ReadOrder,
    /// By the read base observed at the anchor position.
    Base,
    /// By the value of a SAM auxiliary tag, given as its two ASCII byte values, e.g.
    /// `{"tag": [72, 80]}` for `HP`; reads without the tag last.
    Tag([u8; 2]),
}

/// Versioned logical AND of alignment predicates. Keeping the expression wrapper in the session
/// schema lets future versions add OR/NOT nodes without reinterpreting today's flat list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AlignmentPredicateExpression {
    /// Expression format version. Must be 1 (the default).
    #[serde(default = "default_alignment_predicate_version")]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub version: u16,
    /// Up to 64 predicates combined with logical AND: a record matches only when every
    /// predicate matches. An empty list matches every record.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    pub all: Vec<AlignmentViewPredicate>,
}

impl Default for AlignmentPredicateExpression {
    fn default() -> Self {
        Self {
            version: default_alignment_predicate_version(),
            all: Vec::new(),
        }
    }
}

const fn default_alignment_predicate_version() -> u16 {
    1
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AlignmentPredicateScope {
    /// No positional restriction: anywhere in the record.
    Anywhere,
    /// Restrict the predicate to one reference base.
    Position {
        /// Contig (reference sequence) name exactly as the workspace reports it, e.g. `chr1`;
        /// non-empty, at most 1024 bytes. An unknown name matches nothing.
        contig: String,
        /// Zero-based genomic position on `contig` (one less than the one-based coordinate shown
        /// in a locus string).
        position: u64,
    },
    /// Restrict the predicate to one zero-based, half-open reference interval `[start, end)`.
    Interval {
        /// Contig (reference sequence) name exactly as the workspace reports it, e.g. `chr1`;
        /// non-empty, at most 1024 bytes. An unknown name matches nothing.
        contig: String,
        /// Zero-based inclusive start on `contig`, in bases.
        start: u64,
        /// Zero-based exclusive end on `contig` (half-open `[start, end)`); must be greater than
        /// `start`.
        end: u64,
    },
}

impl Default for AlignmentPredicateScope {
    fn default() -> Self {
        Self::Anywhere
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentFlagPredicate {
    /// PCR or optical duplicate (SAM flag 0x400).
    Duplicate,
    /// Secondary alignment (SAM flag 0x100).
    Secondary,
    /// Supplementary alignment (SAM flag 0x800).
    Supplementary,
    /// Failed platform or vendor quality checks (SAM flag 0x200).
    FailedQualityControl,
    /// Read is paired (SAM flag 0x1).
    Paired,
    /// Read is mapped in a proper pair (SAM flag 0x2).
    ProperPair,
    /// Read is paired (0x1) and its mate is not unmapped (0x8 clear).
    MateMapped,
    /// Read aligns to the reverse strand (SAM flag 0x10).
    ReverseStrand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentCigarEventPredicate {
    /// Insertion (CIGAR `I`).
    Insertion,
    /// Deletion (CIGAR `D`).
    Deletion,
    /// Insertion or deletion (CIGAR `I` or `D`).
    Indel,
    /// Soft clip (CIGAR `S`).
    SoftClip,
    /// Hard clip (CIGAR `H`).
    HardClip,
    /// Reference skip, such as a spliced intron (CIGAR `N`).
    ReferenceSkip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlignmentAuxComparison {
    /// The tag is present; takes no operand.
    Exists,
    /// The tag's value equals the operand.
    Equal,
    /// The tag is present with a comparable value that differs from the operand.
    NotEqual,
    /// The tag's value is greater than the operand.
    Greater,
    /// The tag's value is greater than or equal to the operand.
    GreaterOrEqual,
    /// The tag's value is less than the operand.
    Less,
    /// The tag's value is less than or equal to the operand.
    LessOrEqual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AlignmentAuxFilterValue {
    /// Integer operand, compared numerically with integer and character tag values.
    Integer(i64),
    /// Floating-point operand, compared with float tag values. Stored as the IEEE-754 bit
    /// pattern of a 64-bit float, written as an unsigned integer (for example
    /// 4607182418800017408 for 1.0); the value must be finite.
    FloatBits(u64),
    /// String operand, compared in byte order with string (`Z`) and hex (`H`) tag values; at
    /// most 65536 bytes.
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum AlignmentViewPredicate {
    /// Match records whose mapping quality (MAPQ) lies in an inclusive range. Records without a
    /// mapping quality never match.
    MappingQuality {
        /// Inclusive lower bound on MAPQ (0 to 255). Omit for no lower bound; must not exceed
        /// `maximum`.
        minimum: Option<u8>,
        /// Inclusive upper bound on MAPQ (0 to 255). Omit for no upper bound.
        maximum: Option<u8>,
    },
    /// Match records by the state of one SAM flag.
    Flag {
        /// The SAM flag (or derived flag state) to test.
        flag: AlignmentFlagPredicate,
        /// Required flag state: true matches records with the flag set, false matches records
        /// with it clear.
        expected: bool,
    },
    /// Match records whose absolute template length (|TLEN|) lies in an inclusive range.
    TemplateLength {
        /// Inclusive lower bound on the absolute template length |TLEN|, in bases. Omit for no
        /// lower bound; must not exceed `maximum`.
        minimum: Option<u64>,
        /// Inclusive upper bound on the absolute template length |TLEN|, in bases. Omit for no
        /// upper bound.
        maximum: Option<u64>,
    },
    /// Match records whose mate maps to a named contig.
    MateContig {
        /// Contig name of the mate, exactly as the workspace reports it; non-empty, at most
        /// 1024 bytes. An unknown name matches nothing.
        contig: String,
    },
    /// Match records by the presence or value of one SAM auxiliary tag.
    Auxiliary {
        /// The two-character SAM auxiliary tag as its two ASCII byte values, e.g. `[72, 80]` for
        /// `HP` or `[78, 77]` for `NM`. The first byte must be a letter and the second a letter
        /// or digit.
        tag: [u8; 2],
        /// How the tag is tested. Every comparison except `exists` is false for records lacking
        /// the tag or holding a value of a different type from the operand.
        comparison: AlignmentAuxComparison,
        /// Operand for the comparison. Required for every comparison except `exists`, and must
        /// be omitted for `exists`; otherwise the predicate is rejected as invalid.
        value: Option<AlignmentAuxFilterValue>,
    },
    /// Match records containing a CIGAR event of a given kind, optionally within a scope and
    /// length range.
    CigarEvent {
        /// The kind of CIGAR event to look for.
        event: AlignmentCigarEventPredicate,
        /// Where the event must occur (default `anywhere`). Insertions and clips are located at
        /// the single reference position where they occur; deletions and reference skips match
        /// when their reference span overlaps the scope.
        #[serde(default)]
        scope: AlignmentPredicateScope,
        /// Inclusive lower bound on the event length, in bases. Omit for no lower bound; must
        /// not exceed `maximum_length`.
        minimum_length: Option<u32>,
        /// Inclusive upper bound on the event length, in bases. Omit for no upper bound.
        maximum_length: Option<u32>,
    },
    /// Match records whose aligned length lies in an inclusive range.
    AlignedLength {
        /// Inclusive lower bound on the aligned length: the sum of CIGAR `M`, `=`, and `X`
        /// operation lengths, in bases (not the reference span). Omit for no lower bound; must
        /// not exceed `maximum`.
        minimum: Option<u64>,
        /// Inclusive upper bound on the aligned length: the sum of CIGAR `M`, `=`, and `X`
        /// operation lengths, in bases. Omit for no upper bound.
        maximum: Option<u64>,
    },
    /// Match records by their base substitutions relative to the reference. Requires a loaded
    /// reference sequence.
    Substitution {
        /// Where substitutions are counted (default `anywhere` in the record).
        #[serde(default)]
        scope: AlignmentPredicateScope,
        /// Base the read must show at a substituted position, as an ASCII byte value: 65 `A`, 67
        /// `C`, 71 `G`, 84 `T`, or 78 `N` (lower-case values are accepted). Omit to count any
        /// substituted base.
        observed_base: Option<u8>,
        /// Inclusive lower bound on the number of matching substitutions within `scope`. Omit
        /// for the default of 1.
        minimum_count: Option<u32>,
        /// Inclusive upper bound on the number of matching substitutions within `scope`. Omit
        /// for no upper bound; must not be below `minimum_count`.
        maximum_count: Option<u32>,
    },
}

impl AlignmentViewPredicate {
    #[must_use]
    pub const fn requires_reference(&self) -> bool {
        matches!(self, Self::Substitution { .. })
    }
}

/// Controls whether alignment rows preserve navigation history or are packed
/// independently for every settled query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// Which eligible reads receive rows in an alignment track. Coverage and every other summary are
/// always computed from the complete eligible population.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AlignmentReadSampling {
    /// Present every eligible read, or refuse individual reads above the complete-read limit.
    Complete,
    /// Present at most `maximum_reads` reads starting in each `window_bases` window.
    Downsampled {
        window_bases: u32,
        maximum_reads: u32,
    },
}

impl AlignmentReadSampling {
    pub const DEFAULT_WINDOW_BASES: u32 = 50;
    pub const DEFAULT_MAXIMUM_READS: u32 = 100;
    pub const MAXIMUM_WINDOW_BASES: u32 = 1_000_000;
    pub const MAXIMUM_READS_PER_WINDOW: u32 = 1_000_000;

    /// The default for newly created tracks.
    #[must_use]
    pub const fn downsampled_default() -> Self {
        Self::Downsampled {
            window_bases: Self::DEFAULT_WINDOW_BASES,
            maximum_reads: Self::DEFAULT_MAXIMUM_READS,
        }
    }

    #[must_use]
    pub const fn is_downsampled(self) -> bool {
        matches!(self, Self::Downsampled { .. })
    }

    /// Clamps persisted or user-entered values into the supported range.
    #[must_use]
    pub fn normalized(self) -> Self {
        match self {
            Self::Complete => Self::Complete,
            Self::Downsampled {
                window_bases,
                maximum_reads,
            } => Self::Downsampled {
                window_bases: window_bases.clamp(1, Self::MAXIMUM_WINDOW_BASES),
                maximum_reads: maximum_reads.clamp(1, Self::MAXIMUM_READS_PER_WINDOW),
            },
        }
    }
}

/// Projects saved before read sampling existed presented every read; keep reproducing that.
const fn legacy_alignment_read_sampling() -> AlignmentReadSampling {
    AlignmentReadSampling::Complete
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
    #[serde(default = "legacy_alignment_read_sampling")]
    pub read_sampling: AlignmentReadSampling,
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
    /// Absolute weak-junction threshold: a junction with fewer reads is hidden in the Sashimi
    /// lane unless every junction is shown.
    #[serde(default = "default_minimum_junction_depth")]
    pub minimum_junction_depth: u32,
    /// Relative weak-junction threshold, in percent: a junction with fewer reads than this share
    /// of the strongest junction using either of its splice sites is hidden.
    #[serde(default = "default_junction_relative_threshold_percent")]
    pub junction_relative_threshold_percent: f32,
    /// Shows every junction, ignoring both weak-junction thresholds.
    #[serde(default)]
    pub show_all_junctions: bool,
    /// Library strandedness used to give a junction a strand when no tag or motif does.
    #[serde(default)]
    pub junction_library_strandedness: LibraryStrandedness,
    #[serde(default)]
    pub sashimi_label_mode: SashimiLabelMode,
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
    /// Additional typed retained-evidence predicates. Legacy MAPQ/flag/tag fields above are
    /// migrated into the same runtime expression and remain serialized for project compatibility.
    #[serde(default)]
    pub alignment_view_filter: AlignmentPredicateExpression,
    /// Uses the same predicate vocabulary without changing evidence membership or summaries.
    #[serde(default)]
    pub alignment_highlight_filter: AlignmentPredicateExpression,
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
            read_sampling: AlignmentReadSampling::downsampled_default(),
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
            minimum_junction_depth: default_minimum_junction_depth(),
            junction_relative_threshold_percent: default_junction_relative_threshold_percent(),
            show_all_junctions: false,
            junction_library_strandedness: LibraryStrandedness::Unstranded,
            sashimi_label_mode: SashimiLabelMode::Counts,
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
            alignment_view_filter: AlignmentPredicateExpression::default(),
            alignment_highlight_filter: AlignmentPredicateExpression::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CoverageScaleTransform {
    /// Bar height proportional to depth.
    #[default]
    Linear,
    /// Bar height proportional to the square root of depth, which makes low coverage easier to
    /// see.
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TrackChannel {
    /// The annotation pane, which holds the reference lane, annotation tracks, and the
    /// annotation-channel row of each alignment track.
    Annotation,
    /// The reads pane: the individual-read rows of alignment tracks.
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresentationNodeId {
    /// The reference sequence lane (annotation channel).
    Reference,
    /// One track row in a presentation channel.
    Track {
        /// Presentation channel (pane) that owns the row.
        channel: TrackChannel,
        /// Positive stable ID of the track, as reported in the workspace snapshot
        /// (`tracks[].id`).
        track_id: TrackId,
    },
    /// One presentation group in a channel.
    Group {
        /// Presentation channel (pane) that owns the group.
        channel: TrackChannel,
        /// Positive presentation-group ID as reported in `presentation_groups` of the workspace
        /// snapshot. IDs are never reused.
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
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
        let mut presentation = Self {
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
        };
        presentation.make_groups_contiguous();
        presentation
    }

    /// Moves every group's members next to its first member, in member order.
    ///
    /// Legacy schemas stored a group id per track, so two members could be
    /// separated by an unrelated track; schema four requires a group to
    /// occupy one contiguous run of its pane. Returns whether anything moved.
    fn make_groups_contiguous(&mut self) -> bool {
        let mut moved = false;
        for group in &self.groups {
            let position = |pane_tracks: &[SessionPaneTrackState], member: TrackId| {
                pane_tracks
                    .iter()
                    .position(|node| node.channel == group.channel && node.track_id == member)
            };
            let Some(first) = group.members.first() else {
                continue;
            };
            let Some(mut anchor) = position(&self.pane_tracks, *first) else {
                continue;
            };
            for member in group.members.iter().skip(1) {
                let Some(current) = position(&self.pane_tracks, *member) else {
                    continue;
                };
                if current == anchor + 1 {
                    anchor = current;
                    continue;
                }
                let node = self.pane_tracks.remove(current);
                if current < anchor {
                    anchor -= 1;
                }
                anchor += 1;
                self.pane_tracks.insert(anchor, node);
                moved = true;
            }
        }
        moved
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
    /// The individual-read admission limit the source must enforce. Downsampled tracks bound
    /// their rows by sampling, so only the hard configurable ceiling applies to decoding.
    #[must_use]
    pub fn effective_complete_read_limit(&self) -> usize {
        if self.filters.read_sampling.is_downsampled() {
            crate::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT
        } else {
            self.maximum_alignments.max(1)
        }
    }

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landmark_context: Option<crate::BuiltInLandmarkContext>,
    /// Bundle IDs whose automatic representative Genes track the user removed.
    /// Hiding a track does not populate this set; explicit restoration clears it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub suppressed_builtin_gene_bundles: BTreeSet<String>,
    pub tracks: Vec<TrackConfig>,
    /// Workspace-wide hard bound. Per-track limits are requests within this shared ceiling.
    pub maximum_alignments: usize,
}

#[derive(Deserialize)]
struct SessionVersionProbe {
    schema_version: u32,
    #[serde(default)]
    writer: Option<SessionWriter>,
}

const NEWER_WRITER_NOTICE_PREFIX: &str = "This project was written by a newer RGV";

/// The newer-writer data-loss warning among `migration_notices`, if any (see
/// [`SessionDocument::newer_writer_notice`]).
#[must_use]
pub fn newer_writer_notice(migration_notices: &[String]) -> Option<&str> {
    migration_notices
        .iter()
        .map(String::as_str)
        .find(|notice| notice.starts_with(NEWER_WRITER_NOTICE_PREFIX))
}

/// Identifies the build that wrote a session so a reader can tell "corrupt"
/// from "written by a newer RGV". Persisted as the trailing `writer` object.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SessionWriter {
    #[serde(default)]
    application: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    schema_minor_version: u32,
}

impl SessionWriter {
    fn current() -> Self {
        Self {
            application: "rgv".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_minor_version: SESSION_SCHEMA_MINOR_VERSION,
        }
    }

    /// A bounded, printable description for error messages.
    fn describe(&self) -> String {
        let clean = |value: &str| {
            value
                .chars()
                .filter(|character| !character.is_control())
                .take(64)
                .collect::<String>()
        };
        match (clean(&self.application), clean(&self.version)) {
            (application, version) if application.is_empty() && version.is_empty() => {
                format!("schema revision {}", self.schema_minor_version)
            }
            (application, version) => format!(
                "{application} {version}, schema revision {}",
                self.schema_minor_version
            )
            .trim()
            .to_owned(),
        }
    }
}

/// The on-disk shape: the document's own fields followed by writer metadata.
/// A wrapper keeps `SessionDocument` free of a field every in-memory
/// constructor would otherwise have to fill.
#[derive(Serialize)]
struct PersistedSessionDocument<'a> {
    #[serde(flatten)]
    document: &'a SessionDocument,
    writer: SessionWriter,
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
            landmark_context: None,
            suppressed_builtin_gene_bundles: BTreeSet::new(),
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
            landmark_context: None,
            suppressed_builtin_gene_bundles: BTreeSet::new(),
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
            landmark_context: None,
            suppressed_builtin_gene_bundles: BTreeSet::new(),
            tracks: Vec::new(),
            maximum_alignments: default_maximum_alignments(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, SessionError> {
        let probe: SessionVersionProbe = serde_json::from_str(json)?;
        let newer_writer = probe
            .writer
            .as_ref()
            .filter(|writer| writer.schema_minor_version > SESSION_SCHEMA_MINOR_VERSION);
        let mut document = match probe.schema_version {
            SESSION_SCHEMA_VERSION => match serde_json::from_str(json) {
                Ok(document) => document,
                // A newer revision of this schema may use an enum variant or
                // field shape this build cannot parse. Say so instead of
                // surfacing a raw serde error that reads like corruption.
                Err(error) => {
                    return Err(match newer_writer {
                        Some(writer) => SessionError::WrittenByNewerVersion {
                            schema_version: probe.schema_version,
                            writer: writer.describe(),
                            detail: Some(error.to_string()),
                        },
                        None => SessionError::Json(error),
                    });
                }
            },
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
            version if version > SESSION_SCHEMA_VERSION => {
                return Err(SessionError::WrittenByNewerVersion {
                    schema_version: version,
                    writer: probe
                        .writer
                        .as_ref()
                        .map_or_else(|| "a newer RGV".to_owned(), SessionWriter::describe),
                    detail: None,
                });
            }
            version => return Err(SessionError::UnsupportedVersion(version)),
        };
        if probe.schema_version < SESSION_SCHEMA_VERSION {
            document.migration_notices = document.legacy_presentation_migration_notices();
        }
        if let Some(writer) = newer_writer {
            // The document parsed, but fields this build does not model were
            // dropped and will be lost if the project is saved from here.
            document.migration_notices.push(format!(
                "{NEWER_WRITER_NOTICE_PREFIX} ({}). Settings this version does not understand were ignored and will be lost if the project is saved.",
                writer.describe()
            ));
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

    /// The data-loss warning recorded when this document was read from a file
    /// written by a newer RGV: saving it back over that file discards the
    /// settings this build could not model. Callers must show it before
    /// overwriting the original.
    #[must_use]
    pub fn newer_writer_notice(&self) -> Option<&str> {
        newer_writer_notice(&self.migration_notices)
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
                    continue;
                }
                if had_collapsed && had_expanded {
                    notices.push(format!(
                        "Repaired inconsistent legacy {channel:?} group {group_id}: collapsed state was applied because at least one member was collapsed"
                    ));
                }
                let mut positions = self
                    .alignment_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| {
                        Some(group_id)
                            == match channel {
                                TrackChannel::Annotation => track.track_group,
                                TrackChannel::Reads => track.read_track_group,
                            }
                    })
                    .map(|(index, _)| index);
                if let Some(first) = positions.next()
                    && positions
                        .enumerate()
                        .any(|(offset, index)| index != first + offset + 1)
                {
                    notices.push(format!(
                        "Repaired legacy {channel:?} group {group_id}: its members were not adjacent and were moved next to the first member"
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
        Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&PersistedSessionDocument {
                document: &persisted,
                writer: SessionWriter::current(),
            })?
        ))
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
            // The legacy group, height and eye mirrors are not checked here:
            // `normalize_presentation` always resets them before validation,
            // and their migrated values are validated on the presentation.
            if track.name.trim().is_empty()
                || !valid_sample_association(track.sample_key.as_deref())
            {
                return Err(SessionError::InvalidAlignmentTrack);
            }
            if track.maximum_alignments == 0 || track.maximum_alignments > self.maximum_alignments {
                return Err(SessionError::InvalidAlignmentLimit);
            }
            validate_filters(&track.filters)?;
        }
        if self.tracks.len() > MAXIMUM_SESSION_TRACKS {
            return Err(SessionError::TooManyTracks);
        }
        if self.tracks.iter().any(|track| !ids.insert(track.id)) {
            return Err(SessionError::DuplicateTrackId);
        }
        if self.tracks.iter().any(|track| {
            track.name.trim().is_empty()
                || !valid_sample_association(track.sample_key.as_deref())
                || !valid_track_format(track)
                || track.built_in.as_ref().is_some_and(|identity| {
                    !identity.is_valid()
                        || identity.asset_role != crate::LandmarkAssetRole::RepresentativeGenes
                            && identity.asset_role != crate::LandmarkAssetRole::DetailedTranscripts
                })
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

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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
                // Colons are legal in contig names (`HLA-A*01:01:01:01`).
                || alias.contains(char::is_whitespace)
                || target.contains(char::is_whitespace)
                || alias.contains(char::is_control)
                || target.contains(char::is_control)
        })
    {
        return Err(SessionError::InvalidContigAliases);
    }
    if document.suppressed_builtin_gene_bundles.len() > crate::MAXIMUM_LANDMARK_BUNDLES
        || document.suppressed_builtin_gene_bundles.iter().any(|id| {
            id.trim().is_empty()
                || id.len() > 256
                || id.contains([' ', '\t', '\r', '\n', '/', '\\'])
        })
    {
        return Err(SessionError::InvalidGenomeMetadata);
    }
    if document
        .landmark_context
        .as_ref()
        .is_some_and(|context| !context.is_valid())
    {
        return Err(SessionError::InvalidGenomeMetadata);
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

const fn default_minimum_junction_depth() -> u32 {
    2
}

const fn default_junction_relative_threshold_percent() -> f32 {
    1.0
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
    if !filters.junction_relative_threshold_percent.is_finite()
        || !(0.0..=100.0).contains(&filters.junction_relative_threshold_percent)
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
    validate_alignment_predicate_expression(&filters.alignment_view_filter)?;
    validate_alignment_predicate_expression(&filters.alignment_highlight_filter)?;
    Ok(())
}

pub fn validate_alignment_predicate_expression(
    expression: &AlignmentPredicateExpression,
) -> Result<(), SessionError> {
    const MAXIMUM_PREDICATES: usize = 64;
    const MAXIMUM_CONTIG_NAME_BYTES: usize = 1_024;
    if expression.version != 1 || expression.all.len() > MAXIMUM_PREDICATES {
        return Err(SessionError::InvalidAlignmentPredicate);
    }
    for predicate in &expression.all {
        let valid = match predicate {
            AlignmentViewPredicate::MappingQuality { minimum, maximum } => minimum
                .zip(*maximum)
                .is_none_or(|(minimum, maximum)| minimum <= maximum),
            AlignmentViewPredicate::Flag { .. } => true,
            AlignmentViewPredicate::TemplateLength { minimum, maximum }
            | AlignmentViewPredicate::AlignedLength { minimum, maximum } => minimum
                .zip(*maximum)
                .is_none_or(|(minimum, maximum)| minimum <= maximum),
            AlignmentViewPredicate::MateContig { contig } => {
                !contig.is_empty() && contig.len() <= MAXIMUM_CONTIG_NAME_BYTES
            }
            AlignmentViewPredicate::Auxiliary {
                tag,
                comparison,
                value,
            } => {
                tag[0].is_ascii_alphabetic()
                    && tag[1].is_ascii_alphanumeric()
                    && match comparison {
                        AlignmentAuxComparison::Exists => value.is_none(),
                        AlignmentAuxComparison::Equal
                        | AlignmentAuxComparison::NotEqual
                        | AlignmentAuxComparison::Greater
                        | AlignmentAuxComparison::GreaterOrEqual
                        | AlignmentAuxComparison::Less
                        | AlignmentAuxComparison::LessOrEqual => value.is_some(),
                    }
                    && value.as_ref().is_none_or(|value| match value {
                        AlignmentAuxFilterValue::FloatBits(bits) => {
                            f64::from_bits(*bits).is_finite()
                        }
                        AlignmentAuxFilterValue::String(value) => value.len() <= 64 * 1_024,
                        AlignmentAuxFilterValue::Integer(_) => true,
                    })
            }
            AlignmentViewPredicate::CigarEvent {
                scope,
                minimum_length,
                maximum_length,
                ..
            } => {
                valid_alignment_predicate_scope(scope, MAXIMUM_CONTIG_NAME_BYTES)
                    && minimum_length
                        .zip(*maximum_length)
                        .is_none_or(|(minimum, maximum)| minimum <= maximum)
            }
            AlignmentViewPredicate::Substitution {
                scope,
                observed_base,
                minimum_count,
                maximum_count,
            } => {
                valid_alignment_predicate_scope(scope, MAXIMUM_CONTIG_NAME_BYTES)
                    && observed_base.is_none_or(|base| {
                        matches!(base.to_ascii_uppercase(), b'A' | b'C' | b'G' | b'T' | b'N')
                    })
                    && maximum_count.is_none_or(|maximum| minimum_count.unwrap_or(1) <= maximum)
            }
        };
        if !valid {
            return Err(SessionError::InvalidAlignmentPredicate);
        }
    }
    Ok(())
}

fn valid_alignment_predicate_scope(scope: &AlignmentPredicateScope, max_name: usize) -> bool {
    match scope {
        AlignmentPredicateScope::Anywhere => true,
        AlignmentPredicateScope::Position { contig, .. } => {
            !contig.is_empty() && contig.len() <= max_name
        }
        AlignmentPredicateScope::Interval { contig, start, end } => {
            !contig.is_empty() && contig.len() <= max_name && start < end
        }
    }
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
    #[error(
        "this project was written by a newer RGV ({writer}, schema version {schema_version}) and cannot be opened by this version{}",
        detail.as_ref().map_or_else(String::new, |detail| format!(": {detail}"))
    )]
    WrittenByNewerVersion {
        schema_version: u32,
        writer: String,
        detail: Option<String>,
    },
    #[error("session contains more than {MAXIMUM_SESSION_TRACKS} tracks")]
    TooManyTracks,
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
    #[error("session alignment predicate expression is invalid or unsupported")]
    InvalidAlignmentPredicate,
    #[error("session auxiliary track has an invalid name, height, or variant setting")]
    InvalidAuxiliaryTrack,
    #[error("could not import IGV session: {0}")]
    Igv(String),
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
