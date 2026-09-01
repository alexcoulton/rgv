use std::{collections::BTreeSet, num::NonZeroUsize, sync::Arc};

use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    BisulfiteContext, ModifiedBaseColorMode, OverlappingMatePolicy, ReadBaseDisplayMode,
    ReadStackingMode, SbxSimplexTailMode, SmrtKineticsMode, UltimaQualityMode,
};
use rgv_hts::{ModificationCode, TemplateLengthBounds};

use crate::fingerprint::StableHasher;
use crate::{AnnotationRevision, ContractFingerprint, ReferenceRevision, RevisionFingerprint};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum CapabilityRequirement {
    #[default]
    NotRequired,
    Required,
}

impl CapabilityRequirement {
    #[must_use]
    pub const fn is_satisfied_by(self, available: Self) -> bool {
        matches!(self, Self::NotRequired) || matches!(available, Self::Required)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordContract {
    filter: ContractFingerprint,
    include_secondary: bool,
    include_supplementary: bool,
}

impl RecordContract {
    #[must_use]
    pub const fn new(
        filter: ContractFingerprint,
        include_secondary: bool,
        include_supplementary: bool,
    ) -> Self {
        Self {
            filter,
            include_secondary,
            include_supplementary,
        }
    }

    #[must_use]
    pub const fn filter(self) -> ContractFingerprint {
        self.filter
    }

    #[must_use]
    pub fn fingerprint(&self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(&self.filter.get().to_le_bytes());
        hash.write(&[
            u8::from(self.include_secondary),
            u8::from(self.include_supplementary),
        ]);
        hash.finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadContract {
    sequence: CapabilityRequirement,
    qualities: CapabilityRequirement,
    all_auxiliary_tags: bool,
    auxiliary_tags: Arc<[[u8; 2]]>,
    decode_reference: Option<ReferenceRevision>,
}

impl PayloadContract {
    #[must_use]
    pub fn new(
        sequence: CapabilityRequirement,
        qualities: CapabilityRequirement,
        auxiliary_tags: impl IntoIterator<Item = [u8; 2]>,
        decode_reference: Option<ReferenceRevision>,
    ) -> Self {
        let tags = auxiliary_tags.into_iter().collect::<BTreeSet<_>>();
        Self {
            sequence,
            qualities,
            all_auxiliary_tags: false,
            auxiliary_tags: tags.into_iter().collect::<Vec<_>>().into(),
            decode_reference,
        }
    }

    /// Requests lossless retention of every auxiliary field rather than a finite tag subset.
    /// This is required by full-record export and cannot be silently satisfied by a tag-filtered
    /// sidecar.
    #[must_use]
    pub fn new_all_auxiliary(
        sequence: CapabilityRequirement,
        qualities: CapabilityRequirement,
        decode_reference: Option<ReferenceRevision>,
    ) -> Self {
        Self {
            sequence,
            qualities,
            all_auxiliary_tags: true,
            auxiliary_tags: Arc::from([]),
            decode_reference,
        }
    }

    #[must_use]
    pub fn satisfies(&self, required: &Self) -> bool {
        required.sequence.is_satisfied_by(self.sequence)
            && required.qualities.is_satisfied_by(self.qualities)
            && (!required.all_auxiliary_tags || self.all_auxiliary_tags)
            && (self.all_auxiliary_tags
                || required
                    .auxiliary_tags
                    .iter()
                    .all(|tag| self.auxiliary_tags.binary_search(tag).is_ok()))
            && (required.decode_reference.is_none()
                || required.decode_reference == self.decode_reference)
    }

    #[must_use]
    pub fn auxiliary_tags(&self) -> &[[u8; 2]] {
        &self.auxiliary_tags
    }

    #[must_use]
    pub const fn all_auxiliary_tags(&self) -> bool {
        self.all_auxiliary_tags
    }

    #[must_use]
    pub const fn sequence(&self) -> CapabilityRequirement {
        self.sequence
    }

    #[must_use]
    pub const fn qualities(&self) -> CapabilityRequirement {
        self.qualities
    }

    #[must_use]
    pub const fn decode_reference(&self) -> Option<ReferenceRevision> {
        self.decode_reference
    }

    #[must_use]
    pub fn fingerprint(&self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(&[
            self.sequence as u8,
            self.qualities as u8,
            u8::from(self.all_auxiliary_tags),
        ]);
        for tag in self.auxiliary_tags.iter() {
            hash.write(tag);
        }
        hash.write(&[0xff]);
        if let Some(reference) = self.decode_reference {
            hash.write(&reference.fingerprint().high().to_le_bytes());
            hash.write(&reference.fingerprint().low().to_le_bytes());
        }
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum AlleleSummaryDetail {
    #[default]
    None,
    Counts,
    QualityWeighted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SummaryContract {
    pub overlapping_mate_policy: OverlappingMatePolicy,
    pub minimum_base_quality: u8,
    pub minimum_junction_flank: u32,
    pub allele_detail: AlleleSummaryDetail,
    pub collect_modifications: bool,
}

impl SummaryContract {
    #[must_use]
    pub const fn depth(self) -> DepthSummaryContract {
        DepthSummaryContract {
            overlapping_mate_policy: self.overlapping_mate_policy,
        }
    }

    #[must_use]
    pub const fn alleles(self) -> AlleleSummaryContract {
        AlleleSummaryContract {
            overlapping_mate_policy: self.overlapping_mate_policy,
            minimum_base_quality: self.minimum_base_quality,
            detail: self.allele_detail,
        }
    }

    #[must_use]
    pub const fn junctions(self) -> JunctionSummaryContract {
        JunctionSummaryContract {
            minimum_flank: self.minimum_junction_flank,
        }
    }

    #[must_use]
    pub const fn pairs(self) -> PairSummaryContract {
        PairSummaryContract
    }

    #[must_use]
    pub const fn filters(self) -> FilterSummaryContract {
        FilterSummaryContract
    }

    #[must_use]
    pub const fn modifications(self) -> Option<ModificationSummaryContract> {
        if self.collect_modifications {
            Some(ModificationSummaryContract)
        } else {
            None
        }
    }

    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(&[
            match self.overlapping_mate_policy {
                OverlappingMatePolicy::CountOnce => 0,
                OverlappingMatePolicy::CountBoth => 1,
            },
            self.minimum_base_quality,
            self.allele_detail as u8,
            u8::from(self.collect_modifications),
        ]);
        hash.write(&self.minimum_junction_flank.to_le_bytes());
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlleleSummaryContract {
    overlapping_mate_policy: OverlappingMatePolicy,
    minimum_base_quality: u8,
    detail: AlleleSummaryDetail,
}

impl AlleleSummaryContract {
    #[must_use]
    pub const fn new(
        overlapping_mate_policy: OverlappingMatePolicy,
        minimum_base_quality: u8,
        detail: AlleleSummaryDetail,
    ) -> Self {
        Self {
            overlapping_mate_policy,
            minimum_base_quality,
            detail,
        }
    }

    #[must_use]
    pub const fn overlapping_mate_policy(self) -> OverlappingMatePolicy {
        self.overlapping_mate_policy
    }

    #[must_use]
    pub const fn minimum_base_quality(self) -> u8 {
        self.minimum_base_quality
    }

    #[must_use]
    pub const fn detail(self) -> AlleleSummaryDetail {
        self.detail
    }

    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-allele-summary\0");
        hash.write(&[
            match self.overlapping_mate_policy {
                OverlappingMatePolicy::CountOnce => 0,
                OverlappingMatePolicy::CountBoth => 1,
            },
            self.minimum_base_quality,
            self.detail as u8,
        ]);
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JunctionSummaryContract {
    minimum_flank: u32,
}

impl JunctionSummaryContract {
    #[must_use]
    pub const fn new(minimum_flank: u32) -> Self {
        Self { minimum_flank }
    }

    #[must_use]
    pub const fn minimum_flank(self) -> u32 {
        self.minimum_flank
    }

    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-junction-summary\0");
        hash.write(&self.minimum_flank.to_le_bytes());
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PairSummaryContract;

impl PairSummaryContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-pair-summary\0");
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FilterSummaryContract;

impl FilterSummaryContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-filter-summary\0");
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ModificationSummaryContract;

impl ModificationSummaryContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-modification-summary\0");
        hash.finish()
    }
}

/// Contract for exact aligned-base depth, independently invalidated from alleles, junctions, and
/// modifications.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthSummaryContract {
    overlapping_mate_policy: OverlappingMatePolicy,
}

impl DepthSummaryContract {
    #[must_use]
    pub const fn new(overlapping_mate_policy: OverlappingMatePolicy) -> Self {
        Self {
            overlapping_mate_policy,
        }
    }

    #[must_use]
    pub const fn overlapping_mate_policy(self) -> OverlappingMatePolicy {
        self.overlapping_mate_policy
    }

    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"exact-depth-summary\0");
        hash.write(&[match self.overlapping_mate_policy {
            OverlappingMatePolicy::CountOnce => 0,
            OverlappingMatePolicy::CountBoth => 1,
        }]);
        hash.finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutContract {
    pub stacking: ReadStackingMode,
    pub grouping: AlignmentGroupMode,
    pub group_order: AlignmentOrderDirection,
    pub sorting: AlignmentSortMode,
    pub sort_order: AlignmentOrderDirection,
    pub sort_anchor: Option<u64>,
    pub view_as_pairs: bool,
    pub pair_length_bounds: TemplateLengthBounds,
    /// Stable template hash explicitly promoted to the top of its group. Promotion is a layout
    /// decision; hover and ordinary selection remain camera overlays and never rekey rows.
    pub promoted_template: Option<u64>,
}

impl LayoutContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(&[match self.stacking {
            ReadStackingMode::Stable => 0,
            ReadStackingMode::Compact => 1,
        }]);
        write_group_mode(&mut hash, self.grouping);
        hash.write(&[match self.group_order {
            AlignmentOrderDirection::Natural => 0,
            AlignmentOrderDirection::Reverse => 1,
        }]);
        write_sort_mode(&mut hash, self.sorting);
        hash.write(&[match self.sort_order {
            AlignmentOrderDirection::Natural => 0,
            AlignmentOrderDirection::Reverse => 1,
        }]);
        hash.write(&self.sort_anchor.unwrap_or(u64::MAX).to_le_bytes());
        hash.write(&[u8::from(self.view_as_pairs)]);
        hash.write(
            &self
                .pair_length_bounds
                .minimum
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hash.write(
            &self
                .pair_length_bounds
                .maximum
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hash.write(&self.promoted_template.unwrap_or(u64::MAX).to_le_bytes());
        hash.finish()
    }
}

fn write_group_mode(hash: &mut StableHasher, mode: AlignmentGroupMode) {
    let (kind, tag) = match mode {
        AlignmentGroupMode::None => (0, None),
        AlignmentGroupMode::Strand => (1, None),
        AlignmentGroupMode::ReadGroup => (2, None),
        AlignmentGroupMode::Haplotype => (3, None),
        AlignmentGroupMode::Chimeric => (4, None),
        AlignmentGroupMode::Supplementary => (5, None),
        AlignmentGroupMode::LeftClip => (6, None),
        AlignmentGroupMode::RightClip => (7, None),
        AlignmentGroupMode::PairOrientation => (8, None),
        AlignmentGroupMode::UnexpectedPair => (9, None),
        AlignmentGroupMode::MateContig => (10, None),
        AlignmentGroupMode::ReadOrder => (11, None),
        AlignmentGroupMode::FirstOfPairStrand => (12, None),
        AlignmentGroupMode::Base => (13, None),
        AlignmentGroupMode::Insertion => (14, None),
        AlignmentGroupMode::Tag(tag) => (15, Some(tag)),
    };
    hash.write(&[kind]);
    if let Some(tag) = tag {
        hash.write(&tag);
    }
}

fn write_sort_mode(hash: &mut StableHasher, mode: AlignmentSortMode) {
    let (kind, tag) = match mode {
        AlignmentSortMode::Start => (0, None),
        AlignmentSortMode::Strand => (1, None),
        AlignmentSortMode::FirstOfPairStrand => (2, None),
        AlignmentSortMode::MappingQuality => (3, None),
        AlignmentSortMode::ReadName => (4, None),
        AlignmentSortMode::ReadGroup => (5, None),
        AlignmentSortMode::Haplotype => (6, None),
        AlignmentSortMode::AlignedLength => (7, None),
        AlignmentSortMode::LeftClip => (8, None),
        AlignmentSortMode::RightClip => (9, None),
        AlignmentSortMode::TemplateLength => (10, None),
        AlignmentSortMode::PairOrientation => (11, None),
        AlignmentSortMode::MateContig => (12, None),
        AlignmentSortMode::PairStatus => (13, None),
        AlignmentSortMode::ReadOrder => (14, None),
        AlignmentSortMode::Base => (15, None),
        AlignmentSortMode::Tag(tag) => (16, Some(tag)),
    };
    hash.write(&[kind]);
    if let Some(tag) = tag {
        hash.write(&tag);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum GeometryLod {
    Overview,
    Span,
    #[default]
    Read,
    Base,
}

/// Independently cached read-page families. Interaction overlays are live handle state and are
/// therefore deliberately absent from immutable page identities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum AlignmentPageFamily {
    ReadGeometry,
    SpecialistOverlay,
    Labels,
}

pub const MAXIMUM_HIDDEN_MODIFICATION_CODES: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentReadLabelMode {
    #[default]
    Hidden,
    ReadName,
    Tag([u8; 2]),
}

/// Complete semantic styling contract for read pages. It deliberately excludes camera state,
/// hover, selection, and selected-chain overlays so interaction cannot invalidate immutable base
/// geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentStyleContract {
    pub color_mode: AlignmentColorMode,
    pub base_display: ReadBaseDisplayMode,
    pub shade_bases_by_quality: bool,
    pub minimum_indel_size: u32,
    pub minimum_event_label_size: u32,
    pub read_labels: AlignmentReadLabelMode,
    pub show_direction_terminals: bool,
    pub show_modified_bases: bool,
    pub minimum_modification_probability: u8,
    pub modified_base_color_mode: ModifiedBaseColorMode,
    pub hidden_modification_codes: [Option<ModificationCode>; MAXIMUM_HIDDEN_MODIFICATION_CODES],
}

impl Default for AlignmentStyleContract {
    fn default() -> Self {
        Self {
            color_mode: AlignmentColorMode::Strand,
            base_display: ReadBaseDisplayMode::Mismatches,
            shade_bases_by_quality: true,
            minimum_indel_size: 1,
            minimum_event_label_size: 1,
            read_labels: AlignmentReadLabelMode::Hidden,
            show_direction_terminals: true,
            show_modified_bases: true,
            minimum_modification_probability: 0,
            modified_base_color_mode: ModifiedBaseColorMode::ByType,
            hidden_modification_codes: [None; MAXIMUM_HIDDEN_MODIFICATION_CODES],
        }
    }
}

impl AlignmentStyleContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"alignment-style-v1\0");
        write_color_mode(&mut hash, self.color_mode);
        hash.write(&[
            match self.base_display {
                ReadBaseDisplayMode::Mismatches => 0,
                ReadBaseDisplayMode::All => 1,
                ReadBaseDisplayMode::Hidden => 2,
            },
            u8::from(self.shade_bases_by_quality),
            u8::from(self.show_direction_terminals),
            u8::from(self.show_modified_bases),
            self.minimum_modification_probability,
            match self.modified_base_color_mode {
                ModifiedBaseColorMode::ByType => 0,
                ModifiedBaseColorMode::Mono => 1,
                ModifiedBaseColorMode::ModifiedVsUnmodified => 2,
            },
        ]);
        hash.write(&self.minimum_indel_size.to_le_bytes());
        hash.write(&self.minimum_event_label_size.to_le_bytes());
        match self.read_labels {
            AlignmentReadLabelMode::Hidden => hash.write(&[0]),
            AlignmentReadLabelMode::ReadName => hash.write(&[1]),
            AlignmentReadLabelMode::Tag(tag) => {
                hash.write(&[2]);
                hash.write(&tag);
            }
        }
        for code in self.hidden_modification_codes {
            match code {
                None => hash.write(&[0]),
                Some(ModificationCode::Letter(letter)) => hash.write(&[1, letter]),
                Some(ModificationCode::Chebi(identifier)) => {
                    hash.write(&[2]);
                    hash.write(&identifier.to_le_bytes());
                }
            }
        }
        hash.finish()
    }
}

fn write_color_mode(hash: &mut StableHasher, mode: AlignmentColorMode) {
    match mode {
        AlignmentColorMode::None => hash.write(&[0]),
        AlignmentColorMode::Strand => hash.write(&[1]),
        AlignmentColorMode::MappingQuality => hash.write(&[2]),
        AlignmentColorMode::ReadGroup => hash.write(&[3]),
        AlignmentColorMode::Haplotype => hash.write(&[4]),
        AlignmentColorMode::TemplateLength => hash.write(&[5]),
        AlignmentColorMode::PairOrientation => hash.write(&[6]),
        AlignmentColorMode::UnexpectedPair => hash.write(&[7]),
        AlignmentColorMode::MateContig => hash.write(&[8]),
        AlignmentColorMode::FirstOfPairStrand => hash.write(&[9]),
        AlignmentColorMode::Bisulfite(context) => {
            hash.write(&[10, bisulfite_context_code(context)]);
        }
        AlignmentColorMode::SmrtKinetics(mode) => {
            hash.write(&[11, smrt_mode_code(mode)]);
        }
        AlignmentColorMode::Ultima(options) => {
            hash.write(&[
                12,
                match options.quality_mode {
                    UltimaQualityMode::Directional => 0,
                    UltimaQualityMode::Minimum => 1,
                },
            ]);
            hash.write(
                &options
                    .maximum_homopolymer_length_override
                    .unwrap_or(u16::MAX)
                    .to_le_bytes(),
            );
        }
        AlignmentColorMode::Sbx(options) => hash.write(&[
            13,
            match options.simplex_tail_mode {
                SbxSimplexTailMode::Off => 0,
                SbxSimplexTailMode::Highlight => 1,
                SbxSimplexTailMode::Hide => 2,
            },
            options.discordant_quality_maximum,
            options.simplex_tail_quality_maximum,
        ]),
        AlignmentColorMode::Tag(tag) => {
            hash.write(&[14]);
            hash.write(&tag);
        }
    }
}

const fn bisulfite_context_code(context: BisulfiteContext) -> u8 {
    match context {
        BisulfiteContext::Cg => 0,
        BisulfiteContext::Chg => 1,
        BisulfiteContext::Chh => 2,
        BisulfiteContext::Hcg => 3,
        BisulfiteContext::Gch => 4,
        BisulfiteContext::Wcg => 5,
    }
}

const fn smrt_mode_code(mode: SmrtKineticsMode) -> u8 {
    match mode {
        SmrtKineticsMode::SubreadIpd => 0,
        SmrtKineticsMode::SubreadPulseWidth => 1,
        SmrtKineticsMode::CcsForwardIpd => 2,
        SmrtKineticsMode::CcsForwardPulseWidth => 3,
        SmrtKineticsMode::CcsReverseIpd => 4,
        SmrtKineticsMode::CcsReversePulseWidth => 5,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationContract {
    pub geometry_lod: GeometryLod,
    pub style: AlignmentStyleContract,
    pub reference: Option<ReferenceRevision>,
    pub annotation: Option<AnnotationRevision>,
}

impl PresentationContract {
    #[must_use]
    pub fn fingerprint(self) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(&[self.geometry_lod as u8]);
        hash.write(&self.style.fingerprint().get().to_le_bytes());
        for revision in [
            self.reference.map(ReferenceRevision::fingerprint),
            self.annotation.map(AnnotationRevision::fingerprint),
        ] {
            if let Some(revision) = revision {
                hash.write(&[1]);
                hash.write(&revision.high().to_le_bytes());
                hash.write(&revision.low().to_le_bytes());
            } else {
                hash.write(&[0]);
            }
        }
        hash.finish()
    }

    /// Content fingerprint for one independently invalidated semantic page family.
    #[must_use]
    pub fn page_fingerprint(self, family: AlignmentPageFamily) -> ContractFingerprint {
        let mut hash = StableHasher::default();
        hash.write(b"alignment-page-contract-v1\0");
        hash.write(&[family as u8, self.geometry_lod as u8]);
        match family {
            AlignmentPageFamily::ReadGeometry => {
                write_color_mode(&mut hash, self.style.color_mode);
                hash.write(&[
                    match self.style.base_display {
                        ReadBaseDisplayMode::Mismatches => 0,
                        ReadBaseDisplayMode::All => 1,
                        ReadBaseDisplayMode::Hidden => 2,
                    },
                    u8::from(self.style.shade_bases_by_quality),
                    u8::from(self.style.show_direction_terminals),
                ]);
                hash.write(&self.style.minimum_indel_size.to_le_bytes());
                if self.geometry_lod == GeometryLod::Base {
                    write_optional_revision(
                        &mut hash,
                        self.reference.map(ReferenceRevision::fingerprint),
                    );
                }
            }
            AlignmentPageFamily::SpecialistOverlay => {
                write_specialist_color_mode(&mut hash, self.style.color_mode);
                hash.write(&[
                    u8::from(self.style.show_modified_bases),
                    self.style.minimum_modification_probability,
                    match self.style.modified_base_color_mode {
                        ModifiedBaseColorMode::ByType => 0,
                        ModifiedBaseColorMode::Mono => 1,
                        ModifiedBaseColorMode::ModifiedVsUnmodified => 2,
                    },
                ]);
                write_hidden_modification_codes(&mut hash, self.style.hidden_modification_codes);
                if matches!(
                    self.style.color_mode,
                    AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
                ) {
                    write_optional_revision(
                        &mut hash,
                        self.reference.map(ReferenceRevision::fingerprint),
                    );
                }
            }
            AlignmentPageFamily::Labels => {
                hash.write(&self.style.minimum_event_label_size.to_le_bytes());
                match self.style.read_labels {
                    AlignmentReadLabelMode::Hidden => hash.write(&[0]),
                    AlignmentReadLabelMode::ReadName => hash.write(&[1]),
                    AlignmentReadLabelMode::Tag(tag) => {
                        hash.write(&[2]);
                        hash.write(&tag);
                    }
                }
            }
        }
        hash.finish()
    }

    #[must_use]
    pub fn required_page_families(self) -> Vec<AlignmentPageFamily> {
        let mut families = vec![AlignmentPageFamily::ReadGeometry];
        if matches!(self.geometry_lod, GeometryLod::Read | GeometryLod::Base) {
            if self.style.show_modified_bases
                || matches!(
                    self.style.color_mode,
                    AlignmentColorMode::Bisulfite(_)
                        | AlignmentColorMode::SmrtKinetics(_)
                        | AlignmentColorMode::Ultima(_)
                        | AlignmentColorMode::Sbx(_)
                )
            {
                families.push(AlignmentPageFamily::SpecialistOverlay);
            }
            families.push(AlignmentPageFamily::Labels);
        }
        families
    }
}

fn write_specialist_color_mode(hash: &mut StableHasher, mode: AlignmentColorMode) {
    match mode {
        AlignmentColorMode::Bisulfite(_)
        | AlignmentColorMode::SmrtKinetics(_)
        | AlignmentColorMode::Ultima(_)
        | AlignmentColorMode::Sbx(_) => write_color_mode(hash, mode),
        _ => hash.write(&[u8::MAX]),
    }
}

fn write_hidden_modification_codes(
    hash: &mut StableHasher,
    codes: [Option<ModificationCode>; MAXIMUM_HIDDEN_MODIFICATION_CODES],
) {
    for code in codes {
        match code {
            None => hash.write(&[0]),
            Some(ModificationCode::Letter(letter)) => hash.write(&[1, letter]),
            Some(ModificationCode::Chebi(identifier)) => {
                hash.write(&[2]);
                hash.write(&identifier.to_le_bytes());
            }
        }
    }
}

fn write_optional_revision(hash: &mut StableHasher, revision: Option<RevisionFingerprint>) {
    if let Some(revision) = revision {
        hash.write(&[1]);
        hash.write(&revision.high().to_le_bytes());
        hash.write(&revision.low().to_le_bytes());
    } else {
        hash.write(&[0]);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionPolicy {
    complete_read_limit: NonZeroUsize,
    total_cache_budget: NonZeroUsize,
    desired_buffer_percent: u16,
}

impl AdmissionPolicy {
    #[must_use]
    pub const fn new(
        complete_read_limit: NonZeroUsize,
        total_cache_budget: NonZeroUsize,
        desired_buffer_percent: u16,
    ) -> Self {
        Self {
            complete_read_limit,
            total_cache_budget,
            desired_buffer_percent,
        }
    }

    #[must_use]
    pub const fn complete_read_limit(self) -> NonZeroUsize {
        self.complete_read_limit
    }

    #[must_use]
    pub const fn total_cache_budget(self) -> NonZeroUsize {
        self.total_cache_budget
    }

    #[must_use]
    pub const fn desired_buffer_percent(self) -> u16 {
        self.desired_buffer_percent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_capability_is_ordered_and_tags_are_canonical() {
        let rich = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"HP", *b"RG", *b"HP"],
            None,
        );
        let layout = PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [*b"RG"],
            None,
        );
        let sequence = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::NotRequired,
            [*b"MM"],
            None,
        );

        assert_eq!(rich.auxiliary_tags(), &[*b"HP", *b"RG"]);
        assert!(rich.satisfies(&layout));
        assert!(!layout.satisfies(&rich));

        let all = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        assert!(all.all_auxiliary_tags());
        assert!(all.satisfies(&rich));
        assert!(!rich.satisfies(&all));
        assert_ne!(all.fingerprint(), rich.fingerprint());
        assert!(!rich.satisfies(&sequence));
    }

    #[test]
    fn admission_policy_is_not_part_of_record_contract() {
        let record = RecordContract::new(ContractFingerprint::new(7), false, true);
        let before = record.fingerprint();
        let _policy = AdmissionPolicy::new(
            NonZeroUsize::new(100).unwrap(),
            NonZeroUsize::new(1024).unwrap(),
            400,
        );
        assert_eq!(record.fingerprint(), before);
    }

    #[test]
    fn page_family_contracts_invalidate_only_the_semantics_they_own() {
        let base = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let labels = PresentationContract {
            style: AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::ReadName,
                ..base.style
            },
            ..base
        };
        assert_eq!(
            base.page_fingerprint(AlignmentPageFamily::ReadGeometry),
            labels.page_fingerprint(AlignmentPageFamily::ReadGeometry)
        );
        assert_ne!(
            base.page_fingerprint(AlignmentPageFamily::Labels),
            labels.page_fingerprint(AlignmentPageFamily::Labels)
        );

        let modifications = PresentationContract {
            style: AlignmentStyleContract {
                minimum_modification_probability: 200,
                ..base.style
            },
            ..base
        };
        assert_eq!(
            base.page_fingerprint(AlignmentPageFamily::ReadGeometry),
            modifications.page_fingerprint(AlignmentPageFamily::ReadGeometry)
        );
        assert_ne!(
            base.page_fingerprint(AlignmentPageFamily::SpecialistOverlay),
            modifications.page_fingerprint(AlignmentPageFamily::SpecialistOverlay)
        );
    }
}
