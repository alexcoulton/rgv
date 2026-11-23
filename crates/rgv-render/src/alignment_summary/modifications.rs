//! Base-modification summary pages: instance layout, CPU oracle, page metadata and packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;
use rgv_alignment_scene::{MODIFICATION_LANE_BIN_LEVELS, modification_lane_bin_bases};
use rgv_model::{ModificationCodeTally, ModificationSiteTally};

/// One immutable modification-frequency candidate. A page stores two camera-independent
/// orderings of these instances (raw and probability-weighted). `later_code_mask` names every
/// better candidate at the same semantic bin; the shader therefore emits exactly the last
/// non-hidden candidate without repacking when lane filters change.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuModificationInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    /// `[raw modified fraction, probability-weighted modified fraction]`.
    pub fractions: [f32; 2],
    pub code_index: u32,
    pub later_code_mask: u32,
    pub modified_calls: u32,
    pub canonical_observations: u32,
    /// `0` is a modification bar and `1` is the optional unmodified-background band.
    pub shape: u32,
    /// Palette slot of a bar coloured by modification type (the read stack's type colour, see
    /// [`crate::modification_style::modification_type_token`]); zero for a background band.
    pub by_type_token: u32,
    pub(super) padding: [u32; 2],
}

impl AlignmentSummaryGpuModificationInstance {
    pub(super) fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Float32x2,
            3 => Uint32x4,
            4 => Uint32x2
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedModification {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub fraction: f64,
    pub code_index: u32,
    pub modified_calls: u32,
    pub canonical_observations: u32,
}

/// CPU oracle for the modification vertex contract. `hidden_code_mask` is page-local and is
/// derived from the immutable page code dictionary at frame construction; it is not part of the
/// page key. Only the exact dominant non-hidden series at a semantic bin projects a bar.
#[must_use]
pub fn project_alignment_summary_modification_instance(
    instance: AlignmentSummaryGpuModificationInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    probability_weighted: bool,
    hidden_code_mask: u32,
) -> Option<AlignmentSummaryGpuProjectedModification> {
    if instance.shape != 0
        || instance.modified_calls == 0
        || instance.code_index >= u32::BITS
        || hidden_code_mask & (1_u32 << instance.code_index) != 0
        || instance.later_code_mask & !hidden_code_mask != 0
    {
        return None;
    }
    let fraction = f64::from(instance.fractions[usize::from(probability_weighted)]).clamp(0.0, 1.0);
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let height = (fraction * camera.clip().height).max(1.0);
    Some(AlignmentSummaryGpuProjectedModification {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y: camera.clip().y + camera.clip().height - height,
            width,
            height,
        },
        fraction,
        code_index: instance.code_index,
        modified_calls: instance.modified_calls,
        canonical_observations: instance.canonical_observations,
    })
}

/// The instance ranges of one bin level of a modification page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSummaryModificationLevel {
    /// The level's bin width in bases (one of [`MODIFICATION_LANE_BIN_LEVELS`]).
    pub bases: u64,
    pub background_range: Range<u32>,
    pub raw_range: Range<u32>,
    pub weighted_range: Range<u32>,
}

#[derive(Clone, Debug)]
pub struct AlignmentSummaryModificationPageMetadata {
    /// One entry per [`MODIFICATION_LANE_BIN_LEVELS`] width, finest first.
    pub(super) levels: Vec<AlignmentSummaryModificationLevel>,
    pub(super) codes: Vec<ModificationCode>,
}

impl AlignmentSummaryModificationPageMetadata {
    /// The level the lane draws at `bases_per_pixel`, the same bin width the CPU projection
    /// uses.
    #[must_use]
    pub fn level(&self, bases_per_pixel: f64) -> &AlignmentSummaryModificationLevel {
        let bases = modification_lane_bin_bases(bases_per_pixel);
        self.levels
            .iter()
            .find(|level| level.bases == bases)
            .or_else(|| self.levels.first())
            .expect("a modification page packs every bin level")
    }

    /// The per-base level's called bands.
    #[must_use]
    pub fn background_range(&self) -> Range<u32> {
        self.level(0.0).background_range.clone()
    }

    /// The per-base level's bars ordered by modified fraction.
    #[must_use]
    pub fn raw_range(&self) -> Range<u32> {
        self.level(0.0).raw_range.clone()
    }

    /// The per-base level's bars ordered by mean call probability.
    #[must_use]
    pub fn weighted_range(&self) -> Range<u32> {
        self.level(0.0).weighted_range.clone()
    }

    #[must_use]
    pub fn codes(&self) -> &[ModificationCode] {
        &self.codes
    }

    /// The page-local mask of codes the lane does not plot: the hidden codes, and every code but
    /// `shown` when one is chosen (all of them when the page never called it).
    pub(super) fn hidden_code_mask(
        &self,
        hidden: &[ModificationCode],
        shown: Option<ModificationCode>,
    ) -> u32 {
        let bit = |code: &ModificationCode| {
            self.codes
                .binary_search(code)
                .ok()
                .and_then(|index| u32::try_from(index).ok())
                .filter(|index| *index < u32::BITS)
                .map(|index| 1_u32 << index)
        };
        let hidden = hidden
            .iter()
            .fold(0_u32, |mask, code| bit(code).map_or(mask, |bit| mask | bit));
        match shown {
            Some(shown) => hidden | !bit(&shown).unwrap_or(0),
            None => hidden,
        }
    }

    pub(super) fn owned_heap_bytes(&self) -> usize {
        self.codes
            .capacity()
            .saturating_mul(size_of::<ModificationCode>())
            .saturating_add(
                self.levels
                    .capacity()
                    .saturating_mul(size_of::<AlignmentSummaryModificationLevel>()),
            )
    }
}

#[derive(Clone, Copy)]
pub(super) struct ModificationPackingCandidate {
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) code: ModificationCode,
    pub(super) tally: ModificationCodeTally,
    pub(super) raw_fraction: f64,
    pub(super) weighted_fraction: f64,
    pub(super) code_index: u32,
    pub(super) by_type_token: u32,
}

impl ModificationPackingCandidate {
    fn fraction(&self, probability_weighted: bool) -> f64 {
        if probability_weighted {
            self.weighted_fraction
        } else {
            self.raw_fraction
        }
    }
}

/// Packs a modification page: for every bin level, a called band per bin and the bin's codes
/// as candidate bars in two orders (by modified fraction and by mean probability). The shader
/// draws a bin's last candidate that is not masked, and 5mC always sorts last, so the lane
/// plots 5mC wherever it was called and otherwise the code with the highest fraction, exactly
/// as [`rgv_alignment_scene::modification_lane_code`] chooses on the CPU.
#[allow(clippy::too_many_lines)]
pub(super) fn pack_modification_page(
    page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::modifications(page)?;
    let mut sites = BTreeMap::<(u64, u64), ModificationSiteTally>::new();
    let mut codes = BTreeSet::new();
    for primitive in page.primitives() {
        if primitive.start_offset >= primitive.end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page: page.key(),
                start: primitive.start_offset,
                end: primitive.end_offset,
            });
        }
        let SummarySemanticComponent::Modification { series, bin } = primitive.component else {
            return Err(AlignmentSummaryGpuPageError::UnexpectedSemanticComponent(
                page.key(),
            ));
        };
        if bin.canonical_observations == 0 {
            continue;
        }
        codes.insert(series.code);
        sites
            .entry((primitive.start_offset, primitive.end_offset))
            .or_default()
            .add(series.code, bin);
    }
    let mut codes = codes.into_iter().collect::<Vec<_>>();
    if codes.len() > u32::BITS as usize {
        return Err(AlignmentSummaryGpuPageError::TooManyModificationCodes(
            codes.len(),
        ));
    }
    codes.shrink_to_fit();
    let page_length = page.interval().len();
    let mut instances = Vec::new();
    let mut levels = Vec::with_capacity(MODIFICATION_LANE_BIN_LEVELS.len());
    for bases in MODIFICATION_LANE_BIN_LEVELS {
        let mut bins = BTreeMap::<(u64, u64), ModificationSiteTally>::new();
        for (&(start, end), tally) in &sites {
            let key = if bases == 1 {
                (start, end)
            } else {
                let start = start / bases * bases;
                (start, start.saturating_add(bases).min(page_length))
            };
            bins.entry(key).or_default().merge(tally);
        }
        let mut groups = BTreeMap::<(u64, u64), Vec<ModificationPackingCandidate>>::new();
        for (&(start_offset, end_offset), tally) in &bins {
            for entry in &tally.codes {
                let Some(code) = entry.code else {
                    continue;
                };
                let (Some(raw_fraction), Some(weighted_fraction)) =
                    (entry.fraction(), entry.weighted_fraction())
                else {
                    continue;
                };
                let code_index = u32::try_from(
                    codes
                        .binary_search(&code)
                        .expect("semantic modification code belongs to the page dictionary"),
                )
                .expect("page modification code dictionary is bounded to u32 bits");
                groups.entry((start_offset, end_offset)).or_default().push(
                    ModificationPackingCandidate {
                        start_offset,
                        end_offset,
                        code,
                        tally: *entry,
                        raw_fraction: raw_fraction.clamp(0.0, 1.0),
                        weighted_fraction: weighted_fraction.clamp(0.0, 1.0),
                        code_index,
                        by_type_token: u32::from(
                            crate::modification_style::modification_type_token(code).index(),
                        ),
                    },
                );
            }
        }
        let background_start = u32::try_from(instances.len())
            .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
        for (&(start_offset, end_offset), candidates) in &groups {
            let called_code_mask = candidates.iter().fold(0_u32, |mask, candidate| {
                mask | (1_u32 << candidate.code_index)
            });
            instances.push(AlignmentSummaryGpuModificationInstance {
                start_offset: split_f64(start_offset as f64),
                end_offset: split_f64(end_offset as f64),
                fractions: [0.0; 2],
                code_index: 0,
                later_code_mask: called_code_mask,
                modified_calls: 0,
                canonical_observations: candidates
                    .iter()
                    .map(|candidate| candidate.tally.called)
                    .max()
                    .unwrap_or(0),
                shape: 1,
                by_type_token: 0,
                padding: [0; 2],
            });
        }
        let background_end = u32::try_from(instances.len())
            .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
        append_sorted_modification_candidates(&mut instances, &groups, false)?;
        let raw_end = u32::try_from(instances.len())
            .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
        append_sorted_modification_candidates(&mut instances, &groups, true)?;
        let weighted_end = u32::try_from(instances.len())
            .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
        levels.push(AlignmentSummaryModificationLevel {
            bases,
            background_range: background_start..background_end,
            raw_range: background_end..raw_end,
            weighted_range: raw_end..weighted_end,
        });
    }
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Modifications {
        instances,
        metadata: AlignmentSummaryModificationPageMetadata { levels, codes },
    })
}

pub(super) fn append_sorted_modification_candidates(
    instances: &mut Vec<AlignmentSummaryGpuModificationInstance>,
    groups: &BTreeMap<(u64, u64), Vec<ModificationPackingCandidate>>,
    probability_weighted: bool,
) -> Result<(), AlignmentSummaryGpuPageError> {
    for candidates in groups.values() {
        let mut candidates = candidates.clone();
        // Ascending priority: the shader draws the last unmasked candidate. 5mC outranks every
        // other code; the rest rank by fraction, ties to the lower code.
        candidates.sort_by(|left, right| {
            rgv_model::is_five_methylcytosine(left.code)
                .cmp(&rgv_model::is_five_methylcytosine(right.code))
                .then_with(|| {
                    left.fraction(probability_weighted)
                        .total_cmp(&right.fraction(probability_weighted))
                })
                .then_with(|| right.code.cmp(&left.code))
        });
        for (index, candidate) in candidates.iter().enumerate() {
            let later_code_mask = candidates[index.saturating_add(1)..]
                .iter()
                .fold(0_u32, |mask, later| mask | (1_u32 << later.code_index));
            instances.push(AlignmentSummaryGpuModificationInstance {
                start_offset: split_f64(candidate.start_offset as f64),
                end_offset: split_f64(candidate.end_offset as f64),
                fractions: [
                    candidate.raw_fraction as f32,
                    candidate.weighted_fraction as f32,
                ],
                code_index: candidate.code_index,
                later_code_mask,
                modified_calls: candidate.tally.modified,
                canonical_observations: candidate.tally.called,
                shape: 0,
                by_type_token: candidate.by_type_token,
                padding: [0; 2],
            });
        }
    }
    if instances.len() > u32::MAX as usize {
        return Err(AlignmentSummaryGpuPageError::CapacityOverflow);
    }
    Ok(())
}
