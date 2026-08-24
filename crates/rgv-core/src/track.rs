use serde::{Deserialize, Serialize};

pub const MAXIMUM_TDF_CHANNELS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct TrackId(u64);

impl TrackId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Alignment,
    Reference,
    Feature,
    GeneModel,
    Variant,
    Mutation,
    Gwas,
    Quantitative,
    CopyNumber,
    Interaction,
}

/// An exact source schema when the broad track kind and file extension are
/// insufficient to reconstruct the decoder (notably custom `BigBed` schemas).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackFormat {
    BigGenePred,
    BigNarrowPeak,
    TdfChannel { index: usize },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureDisplayMode {
    Collapsed,
    Squished,
    #[default]
    Expanded,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeatureTrackSettings {
    #[serde(default)]
    pub display_mode: FeatureDisplayMode,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GwasColorMode {
    /// A stable palette keyed by chromosome name.
    #[default]
    Chromosome,
    /// Alternate two user-selected colours between chromosomes.
    Alternating,
    /// Use one user-selected colour for every point.
    Uniform,
}

/// Persisted presentation state for a GWAS Manhattan track. Source p-values
/// remain exact and are transformed only for vertical placement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GwasTrackSettings {
    #[serde(default)]
    pub color_mode: GwasColorMode,
    #[serde(default = "default_gwas_point_size")]
    pub point_size: f32,
    #[serde(default = "default_gwas_maximum_score")]
    pub maximum_score: f32,
    #[serde(default = "default_gwas_uniform_color")]
    pub uniform_color: [u8; 4],
    #[serde(default = "default_gwas_alternating_color_one")]
    pub alternating_color_one: [u8; 4],
    #[serde(default = "default_gwas_alternating_color_two")]
    pub alternating_color_two: [u8; 4],
}

impl Default for GwasTrackSettings {
    fn default() -> Self {
        Self {
            color_mode: GwasColorMode::Chromosome,
            point_size: default_gwas_point_size(),
            maximum_score: default_gwas_maximum_score(),
            uniform_color: default_gwas_uniform_color(),
            alternating_color_one: default_gwas_alternating_color_one(),
            alternating_color_two: default_gwas_alternating_color_two(),
        }
    }
}

impl GwasTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.point_size.is_finite()
            && (1.0..=16.0).contains(&self.point_size)
            && self.maximum_score.is_finite()
            && (1.0..=1_000.0).contains(&self.maximum_score)
    }
}

const fn default_gwas_point_size() -> f32 {
    3.0
}

const fn default_gwas_maximum_score() -> f32 {
    25.0
}

const fn default_gwas_uniform_color() -> [u8; 4] {
    [45, 92, 175, 230]
}

const fn default_gwas_alternating_color_one() -> [u8; 4] {
    [45, 92, 175, 230]
}

const fn default_gwas_alternating_color_two() -> [u8; 4] {
    [124, 156, 204, 230]
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationColorMode {
    #[default]
    Classification,
    VariantType,
    Uniform,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MutationTrackSettings {
    #[serde(default)]
    pub color_mode: MutationColorMode,
    #[serde(default = "default_mutation_row_height")]
    pub row_height: f32,
    #[serde(default = "default_true")]
    pub show_sample_names: bool,
    #[serde(default = "default_true")]
    pub show_gene_labels: bool,
}

impl Default for MutationTrackSettings {
    fn default() -> Self {
        Self {
            color_mode: MutationColorMode::Classification,
            row_height: default_mutation_row_height(),
            show_sample_names: true,
            show_gene_labels: true,
        }
    }
}

impl MutationTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.row_height.is_finite() && (8.0..=40.0).contains(&self.row_height)
    }
}

const fn default_mutation_row_height() -> f32 {
    16.0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyNumberValueMode {
    #[default]
    Automatic,
    LogRatio,
    Absolute,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyNumberSortMode {
    #[default]
    FileOrder,
    Amplification,
    Deletion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CopyNumberSortLocus {
    pub contig: String,
    pub start: u64,
    pub end: u64,
}

impl CopyNumberSortLocus {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.contig.trim().is_empty() && self.start < self.end
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopyNumberTrackSettings {
    #[serde(default)]
    pub value_mode: CopyNumberValueMode,
    #[serde(default)]
    pub sort_mode: CopyNumberSortMode,
    #[serde(default)]
    pub sort_locus: Option<CopyNumberSortLocus>,
    #[serde(default = "default_copy_number_row_height")]
    pub row_height: f32,
    #[serde(default = "default_true")]
    pub show_sample_names: bool,
    #[serde(default = "default_true")]
    pub show_values: bool,
}

impl Default for CopyNumberTrackSettings {
    fn default() -> Self {
        Self {
            value_mode: CopyNumberValueMode::Automatic,
            sort_mode: CopyNumberSortMode::FileOrder,
            sort_locus: None,
            row_height: default_copy_number_row_height(),
            show_sample_names: true,
            show_values: true,
        }
    }
}

impl CopyNumberTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.row_height.is_finite()
            && (6.0..=40.0).contains(&self.row_height)
            && self
                .sort_locus
                .as_ref()
                .is_none_or(CopyNumberSortLocus::is_valid)
            && (self.sort_mode == CopyNumberSortMode::FileOrder || self.sort_locus.is_some())
    }
}

const fn default_copy_number_row_height() -> f32 {
    14.0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionDisplayMode {
    #[default]
    Arcs,
    Blocks,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionTrackSettings {
    #[serde(default)]
    pub display_mode: InteractionDisplayMode,
    #[serde(default)]
    pub minimum_score: Option<f32>,
    #[serde(default)]
    pub fixed_maximum_score: Option<f32>,
    #[serde(default = "default_true")]
    pub show_labels: bool,
}

impl Default for InteractionTrackSettings {
    fn default() -> Self {
        Self {
            display_mode: InteractionDisplayMode::Arcs,
            minimum_score: None,
            fixed_maximum_score: None,
            show_labels: true,
        }
    }
}

impl InteractionTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.minimum_score
            .is_none_or(|value| value.is_finite() && value >= 0.0)
            && self
                .fixed_maximum_score
                .is_none_or(|value| value.is_finite() && value > 0.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrackSource {
    Local { path: String },
    Remote { url: String },
}

pub const MAX_VARIANT_SAMPLE_SUBSET: usize = 10_000;
pub const MAX_VARIANT_FIELD_PREDICATES: usize = 32;
pub const MAX_VARIANT_FILTER_VALUES: usize = 256;
pub const MAX_VARIANT_SETTING_STRING_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeScaleMode {
    #[default]
    Automatic,
    Fixed,
    Shared,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeScaleTransform {
    #[default]
    Linear,
    SignedLog10,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeDisplayMode {
    #[default]
    Bar,
    Line,
    Point,
    Heatmap,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeWindowFunction {
    Raw,
    Minimum,
    #[default]
    Mean,
    Maximum,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeMissingMode {
    #[default]
    Gap,
    Baseline,
    Marker,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitativeArithmetic {
    #[default]
    None,
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// Persisted presentation state shared by `BigWig`, `bedGraph`, `WIG`, copy-number,
/// and derived quantitative tracks. Source values remain untouched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuantitativeTrackSettings {
    #[serde(default)]
    pub scale_mode: QuantitativeScaleMode,
    #[serde(default = "default_quantitative_minimum")]
    pub fixed_minimum: f32,
    #[serde(default = "default_quantitative_maximum")]
    pub fixed_maximum: f32,
    #[serde(default)]
    pub baseline: f32,
    #[serde(default)]
    pub scale_transform: QuantitativeScaleTransform,
    #[serde(default)]
    pub display_mode: QuantitativeDisplayMode,
    #[serde(default)]
    pub window_function: QuantitativeWindowFunction,
    #[serde(default)]
    pub missing_mode: QuantitativeMissingMode,
    #[serde(default = "default_positive_quantitative_color")]
    pub positive_color: [u8; 4],
    #[serde(default = "default_negative_quantitative_color")]
    pub negative_color: [u8; 4],
    #[serde(default = "default_quantitative_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub arithmetic: QuantitativeArithmetic,
    #[serde(default)]
    pub operand_track: Option<TrackId>,
    #[serde(default = "default_true")]
    pub show_range_labels: bool,
    /// Tracks with the same non-zero group share one visual frame. `None` keeps the track separate.
    #[serde(default)]
    pub overlay_group: Option<u32>,
    /// Restricts shared autoscaling to the same non-zero group. `None` shares with all compatible
    /// visible tracks that also have no group.
    #[serde(default)]
    pub scale_group: Option<u32>,
}

impl Default for QuantitativeTrackSettings {
    fn default() -> Self {
        Self {
            scale_mode: QuantitativeScaleMode::Automatic,
            fixed_minimum: default_quantitative_minimum(),
            fixed_maximum: default_quantitative_maximum(),
            baseline: 0.0,
            scale_transform: QuantitativeScaleTransform::Linear,
            display_mode: QuantitativeDisplayMode::Bar,
            window_function: QuantitativeWindowFunction::Mean,
            missing_mode: QuantitativeMissingMode::Gap,
            positive_color: default_positive_quantitative_color(),
            negative_color: default_negative_quantitative_color(),
            opacity: default_quantitative_opacity(),
            arithmetic: QuantitativeArithmetic::None,
            operand_track: None,
            show_range_labels: true,
            overlay_group: None,
            scale_group: None,
        }
    }
}

impl QuantitativeTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.fixed_minimum.is_finite()
            && self.fixed_maximum.is_finite()
            && self.fixed_minimum < self.fixed_maximum
            && self.baseline.is_finite()
            && self.opacity.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
            && ((self.arithmetic == QuantitativeArithmetic::None && self.operand_track.is_none())
                || (self.arithmetic != QuantitativeArithmetic::None
                    && self.operand_track.is_some()))
            && self.overlay_group.is_none_or(|group| group > 0)
            && self.scale_group.is_none_or(|group| group > 0)
    }
}

const fn default_quantitative_minimum() -> f32 {
    -1.0
}

const fn default_quantitative_maximum() -> f32 {
    1.0
}

const fn default_positive_quantitative_color() -> [u8; 4] {
    [37, 156, 197, 230]
}

const fn default_negative_quantitative_color() -> [u8; 4] {
    [218, 75, 99, 230]
}

const fn default_quantitative_opacity() -> f32 {
    1.0
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantTypeGroup {
    Snv,
    Mnv,
    Insertion,
    Deletion,
    Complex,
    StructuralDeletion,
    StructuralInsertion,
    Inversion,
    Duplication,
    CopyNumber,
    Breakend,
    SpanningDeletion,
    OtherSymbolic,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantGenotypeFilter {
    #[default]
    Any,
    Called,
    Missing,
    HomReference,
    Heterozygous,
    HomAlternate,
    NonReference,
    Phased,
    Unphased,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantMissingValueRule {
    Include,
    #[default]
    Exclude,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantFieldScope {
    Info,
    Format,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantFieldComparison {
    Exists,
    Equals(String),
    NotEquals(String),
    Contains(String),
    GreaterThan(f64),
    GreaterOrEqual(f64),
    LessThan(f64),
    LessOrEqual(f64),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariantFieldPredicate {
    pub scope: VariantFieldScope,
    pub key: String,
    pub comparison: VariantFieldComparison,
    #[serde(default)]
    pub missing: VariantMissingValueRule,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantGenotypeGrouping {
    #[default]
    SampleOrder,
    Genotype,
    Pedigree,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantColorMode {
    #[default]
    Type,
    Genotype,
}

/// Persisted, per-track cohort and filtering state. Empty collections mean
/// "all" so older sessions retain their previous behavior.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariantTrackSettings {
    #[serde(default)]
    pub minimum_quality: Option<f32>,
    #[serde(default)]
    pub quality_missing: VariantMissingValueRule,
    #[serde(default)]
    pub pass_only: bool,
    #[serde(default)]
    pub allowed_filters: Vec<String>,
    #[serde(default)]
    pub variant_types: Vec<VariantTypeGroup>,
    #[serde(default)]
    pub minimum_allele_frequency: Option<f32>,
    #[serde(default)]
    pub maximum_allele_frequency: Option<f32>,
    #[serde(default)]
    pub allele_frequency_missing: VariantMissingValueRule,
    #[serde(default)]
    pub genotype: VariantGenotypeFilter,
    #[serde(default)]
    pub field_predicates: Vec<VariantFieldPredicate>,
    #[serde(default)]
    pub sample_subset_enabled: bool,
    #[serde(default)]
    pub sample_subset: Vec<String>,
    #[serde(default)]
    pub genotype_grouping: VariantGenotypeGrouping,
    #[serde(default)]
    pub color_mode: VariantColorMode,
}

impl Default for VariantTrackSettings {
    fn default() -> Self {
        Self {
            minimum_quality: None,
            quality_missing: VariantMissingValueRule::Exclude,
            pass_only: false,
            allowed_filters: Vec::new(),
            variant_types: Vec::new(),
            minimum_allele_frequency: None,
            maximum_allele_frequency: None,
            allele_frequency_missing: VariantMissingValueRule::Exclude,
            genotype: VariantGenotypeFilter::Any,
            field_predicates: Vec::new(),
            sample_subset_enabled: false,
            sample_subset: Vec::new(),
            genotype_grouping: VariantGenotypeGrouping::SampleOrder,
            color_mode: VariantColorMode::Type,
        }
    }
}

impl VariantTrackSettings {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let finite_frequency = |value: Option<f32>| {
            value.is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value))
        };
        self.minimum_quality
            .is_none_or(|value| value.is_finite() && value >= 0.0)
            && finite_frequency(self.minimum_allele_frequency)
            && finite_frequency(self.maximum_allele_frequency)
            && !self
                .minimum_allele_frequency
                .zip(self.maximum_allele_frequency)
                .is_some_and(|(minimum, maximum)| minimum > maximum)
            && self.allowed_filters.len() <= MAX_VARIANT_FILTER_VALUES
            && self.variant_types.len() <= MAX_VARIANT_FILTER_VALUES
            && self.field_predicates.len() <= MAX_VARIANT_FIELD_PREDICATES
            && self.sample_subset.len() <= MAX_VARIANT_SAMPLE_SUBSET
            && self
                .allowed_filters
                .iter()
                .chain(self.sample_subset.iter())
                .all(|value| valid_variant_setting_string(value))
            && self.field_predicates.iter().all(|predicate| {
                valid_variant_setting_string(&predicate.key)
                    && match &predicate.comparison {
                        VariantFieldComparison::Exists => true,
                        VariantFieldComparison::Equals(value)
                        | VariantFieldComparison::NotEquals(value)
                        | VariantFieldComparison::Contains(value) => {
                            valid_variant_setting_string(value)
                        }
                        VariantFieldComparison::GreaterThan(value)
                        | VariantFieldComparison::GreaterOrEqual(value)
                        | VariantFieldComparison::LessThan(value)
                        | VariantFieldComparison::LessOrEqual(value) => value.is_finite(),
                    }
            })
    }
}

fn valid_variant_setting_string(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_VARIANT_SETTING_STRING_BYTES
}

impl TrackSource {
    pub fn redacted_label(&self) -> String {
        match self {
            Self::Local { path } => path.clone(),
            Self::Remote { url } => redact_url_credentials(url),
        }
    }

    /// Returns a source safe to place in logs, diagnostics, and session files.
    ///
    /// Embedded URL credentials and signed query parameters are intentionally
    /// not recoverable. Callers that need authentication must provide it again
    /// through the runtime authentication abstraction.
    #[must_use]
    pub fn redacted_for_persistence(&self) -> Self {
        match self {
            Self::Local { path } => Self::Local { path: path.clone() },
            Self::Remote { url } => Self::Remote {
                url: redact_url_credentials(url),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackConfig {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub source: TrackSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<TrackFormat>,
    #[serde(default)]
    pub sample_key: Option<String>,
    pub visible: bool,
    #[serde(default)]
    pub collapsed: bool,
    pub height: f32,
    #[serde(default)]
    pub variant: VariantTrackSettings,
    #[serde(default)]
    pub mutation: MutationTrackSettings,
    #[serde(default)]
    pub gwas: GwasTrackSettings,
    #[serde(default)]
    pub quantitative: QuantitativeTrackSettings,
    #[serde(default)]
    pub copy_number: CopyNumberTrackSettings,
    #[serde(default)]
    pub interaction: InteractionTrackSettings,
    #[serde(default)]
    pub feature: FeatureTrackSettings,
}

impl TrackConfig {
    pub fn new(id: TrackId, name: String, kind: TrackKind, source: TrackSource) -> Self {
        let mut quantitative = QuantitativeTrackSettings::default();
        if kind == TrackKind::CopyNumber {
            quantitative.display_mode = QuantitativeDisplayMode::Heatmap;
            quantitative.positive_color = [220, 50, 47, 230];
            quantitative.negative_color = [38, 100, 210, 230];
            quantitative.fixed_minimum = -1.5;
            quantitative.fixed_maximum = 1.5;
        }
        Self {
            id,
            name,
            kind,
            source,
            format: None,
            sample_key: None,
            visible: true,
            collapsed: false,
            height: match kind {
                TrackKind::Gwas => 200.0,
                // Gene models are compact context lanes. A tall generic feature
                // default wastes most of the viewport for sparse MANE tracks.
                TrackKind::GeneModel => 48.0,
                _ => 120.0,
            },
            variant: VariantTrackSettings::default(),
            mutation: MutationTrackSettings::default(),
            gwas: GwasTrackSettings::default(),
            quantitative,
            copy_number: CopyNumberTrackSettings::default(),
            interaction: InteractionTrackSettings::default(),
            feature: FeatureTrackSettings::default(),
        }
    }

    #[must_use]
    pub fn with_format(mut self, format: TrackFormat) -> Self {
        self.format = Some(format);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackCollection {
    tracks: Vec<TrackConfig>,
}

impl TrackCollection {
    pub fn tracks(&self) -> &[TrackConfig] {
        &self.tracks
    }

    pub fn tracks_mut(&mut self) -> &mut [TrackConfig] {
        &mut self.tracks
    }

    pub fn push(&mut self, track: TrackConfig) {
        self.tracks.push(track);
    }

    pub fn insert_after(&mut self, id: TrackId, tracks: impl IntoIterator<Item = TrackConfig>) {
        let index = self
            .tracks
            .iter()
            .position(|track| track.id == id)
            .map_or(self.tracks.len(), |index| index + 1);
        self.tracks.splice(index..index, tracks);
    }

    pub fn remove(&mut self, id: TrackId) -> Option<TrackConfig> {
        let index = self.tracks.iter().position(|track| track.id == id)?;
        Some(self.tracks.remove(index))
    }

    pub fn move_track(&mut self, id: TrackId, new_index: usize) -> bool {
        let Some(old_index) = self.tracks.iter().position(|track| track.id == id) else {
            return false;
        };
        let track = self.tracks.remove(old_index);
        self.tracks.insert(new_index.min(self.tracks.len()), track);
        true
    }
}

fn redact_url_credentials(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return "[redacted remote URL]".to_owned();
    };
    if !parsed.username().is_empty() {
        let _ = parsed.set_username("");
    }
    if parsed.password().is_some() {
        let _ = parsed.set_password(None);
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64) -> TrackConfig {
        TrackConfig::new(
            TrackId::new(id),
            format!("track-{id}"),
            TrackKind::Feature,
            TrackSource::Local {
                path: format!("track-{id}.bed"),
            },
        )
    }

    #[test]
    fn collection_reorders_and_removes_by_stable_id() {
        let mut collection = TrackCollection::default();
        collection.push(track(1));
        collection.push(track(2));
        collection.push(track(3));
        assert!(collection.move_track(TrackId::new(3), 0));
        assert_eq!(collection.tracks()[0].id, TrackId::new(3));
        assert_eq!(
            collection.remove(TrackId::new(2)).unwrap().id,
            TrackId::new(2)
        );
        collection.insert_after(TrackId::new(3), [track(4), track(5)]);
        assert_eq!(
            collection
                .tracks()
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [
                TrackId::new(3),
                TrackId::new(4),
                TrackId::new(5),
                TrackId::new(1)
            ]
        );
    }

    #[test]
    fn gene_models_use_a_compact_default_height() {
        let track = TrackConfig::new(
            TrackId::new(1),
            "MANE Select".to_owned(),
            TrackKind::GeneModel,
            TrackSource::Local {
                path: "mane.bb".to_owned(),
            },
        );
        assert_eq!(track.height, 48.0);
    }

    #[test]
    fn legacy_track_configuration_defaults_to_expanded() {
        let json = r#"{
            "id": 7,
            "name": "variants",
            "kind": "Variant",
            "source": {"Local": {"path": "calls.vcf.gz"}},
            "visible": true,
            "height": 120.0
        }"#;
        let track: TrackConfig = serde_json::from_str(json).unwrap();
        assert!(!track.collapsed);
        assert_eq!(track.variant, VariantTrackSettings::default());
    }

    #[test]
    fn variant_settings_reject_unbounded_or_non_finite_state() {
        let mut settings = VariantTrackSettings {
            minimum_quality: Some(12.5),
            minimum_allele_frequency: Some(0.1),
            maximum_allele_frequency: Some(0.9),
            sample_subset_enabled: true,
            sample_subset: vec!["tumour".to_owned()],
            field_predicates: vec![VariantFieldPredicate {
                scope: VariantFieldScope::Info,
                key: "AF".to_owned(),
                comparison: VariantFieldComparison::GreaterOrEqual(0.2),
                missing: VariantMissingValueRule::Exclude,
            }],
            ..VariantTrackSettings::default()
        };
        assert!(settings.is_valid());
        settings.minimum_quality = Some(f32::NAN);
        assert!(!settings.is_valid());
        settings.minimum_quality = None;
        settings.sample_subset = vec!["x".repeat(MAX_VARIANT_SETTING_STRING_BYTES + 1)];
        assert!(!settings.is_valid());
    }

    #[test]
    fn gwas_settings_are_bounded_and_legacy_tracks_receive_exact_defaults() {
        let mut settings = GwasTrackSettings::default();
        assert!(settings.is_valid());
        assert_eq!(settings.color_mode, GwasColorMode::Chromosome);
        assert!((settings.point_size - 3.0).abs() < f32::EPSILON);
        assert!((settings.maximum_score - 25.0).abs() < f32::EPSILON);
        settings.point_size = 17.0;
        assert!(!settings.is_valid());
        settings.point_size = 3.0;
        settings.maximum_score = f32::NAN;
        assert!(!settings.is_valid());

        let json = r#"{
            "id": 8,
            "name": "associations",
            "kind": "Gwas",
            "source": {"Local": {"path": "study.gwas"}},
            "visible": true,
            "height": 200.0
        }"#;
        let track: TrackConfig = serde_json::from_str(json).unwrap();
        assert_eq!(track.gwas, GwasTrackSettings::default());
    }

    #[test]
    fn remote_labels_do_not_expose_credentials() {
        let source = TrackSource::Remote {
            url: "https://token:secret@example.org/data.bam?signature=visible".to_owned(),
        };
        let label = source.redacted_label();
        assert!(!label.contains("token"));
        assert!(!label.contains("secret"));
        assert!(!label.contains("signature"));
        assert_eq!(label, "https://example.org/data.bam");
    }

    #[test]
    fn malformed_remote_labels_are_not_echoed() {
        let source = TrackSource::Remote {
            url: "not a URL containing secret=abc".to_owned(),
        };
        assert_eq!(source.redacted_label(), "[redacted remote URL]");
    }
}
