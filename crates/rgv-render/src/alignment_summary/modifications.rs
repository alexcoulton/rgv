//! Base-modification summary pages: instance layout, CPU oracle, page metadata and packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

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

#[derive(Clone, Debug)]
pub struct AlignmentSummaryModificationPageMetadata {
    pub(super) background_range: Range<u32>,
    pub(super) raw_range: Range<u32>,
    pub(super) weighted_range: Range<u32>,
    pub(super) codes: Vec<ModificationCode>,
}

impl AlignmentSummaryModificationPageMetadata {
    #[must_use]
    pub fn background_range(&self) -> Range<u32> {
        self.background_range.clone()
    }

    #[must_use]
    pub fn raw_range(&self) -> Range<u32> {
        self.raw_range.clone()
    }

    #[must_use]
    pub fn weighted_range(&self) -> Range<u32> {
        self.weighted_range.clone()
    }

    #[must_use]
    pub fn codes(&self) -> &[ModificationCode] {
        &self.codes
    }

    pub(super) fn hidden_code_mask(&self, hidden: &[ModificationCode]) -> u32 {
        hidden.iter().fold(0_u32, |mask, code| {
            self.codes
                .binary_search(code)
                .ok()
                .and_then(|index| u32::try_from(index).ok())
                .filter(|index| *index < u32::BITS)
                .map_or(mask, |index| mask | (1_u32 << index))
        })
    }

    pub(super) fn owned_heap_bytes(&self) -> usize {
        self.codes
            .capacity()
            .saturating_mul(size_of::<ModificationCode>())
    }
}

#[derive(Clone, Copy)]
pub(super) struct ModificationPackingCandidate {
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) series: ModificationSeriesKey,
    pub(super) bin: ModificationBin,
    pub(super) raw_fraction: f64,
    pub(super) weighted_fraction: f64,
    pub(super) code_index: u32,
    pub(super) by_type_token: u32,
}

pub(super) fn pack_modification_page(
    page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::modifications(page)?;
    let mut semantic = Vec::with_capacity(page.primitives().len());
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
        if bin.modified_calls == 0 {
            continue;
        }
        let (Some(raw_fraction), Some(weighted_fraction)) =
            (bin.raw_fraction(), bin.weighted_fraction())
        else {
            continue;
        };
        codes.insert(series.code);
        semantic.push((
            primitive.start_offset,
            primitive.end_offset,
            series,
            bin,
            raw_fraction.clamp(0.0, 1.0),
            weighted_fraction.clamp(0.0, 1.0),
        ));
    }
    let mut codes = codes.into_iter().collect::<Vec<_>>();
    if codes.len() > u32::BITS as usize {
        return Err(AlignmentSummaryGpuPageError::TooManyModificationCodes(
            codes.len(),
        ));
    }
    codes.shrink_to_fit();
    let mut groups = BTreeMap::<(u64, u64), Vec<ModificationPackingCandidate>>::new();
    for (start_offset, end_offset, series, bin, raw_fraction, weighted_fraction) in semantic {
        let code_index = u32::try_from(
            codes
                .binary_search(&series.code)
                .expect("semantic modification code belongs to the page dictionary"),
        )
        .expect("page modification code dictionary is bounded to u32 bits");
        groups
            .entry((start_offset, end_offset))
            .or_default()
            .push(ModificationPackingCandidate {
                start_offset,
                end_offset,
                series,
                bin,
                raw_fraction,
                weighted_fraction,
                code_index,
                by_type_token: u32::from(
                    crate::modification_style::modification_type_token(series.code).index(),
                ),
            });
    }

    let maximum_instances = groups
        .values()
        .try_fold(groups.len(), |total, group| {
            total.checked_add(group.len().saturating_mul(2))
        })
        .ok_or(AlignmentSummaryGpuPageError::CapacityOverflow)?;
    let mut instances = Vec::with_capacity(maximum_instances);
    for (&(start_offset, end_offset), candidates) in &groups {
        let modified_code_mask = candidates.iter().fold(0_u32, |mask, candidate| {
            mask | (1_u32 << candidate.code_index)
        });
        instances.push(AlignmentSummaryGpuModificationInstance {
            start_offset: split_f64(start_offset as f64),
            end_offset: split_f64(end_offset as f64),
            fractions: [0.0; 2],
            code_index: 0,
            later_code_mask: modified_code_mask,
            modified_calls: 0,
            canonical_observations: 0,
            shape: 1,
            by_type_token: 0,
            padding: [0; 2],
        });
    }
    let background_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;

    let raw_start = background_end;
    append_sorted_modification_candidates(&mut instances, &groups, false)?;
    let raw_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
    let weighted_start = raw_end;
    append_sorted_modification_candidates(&mut instances, &groups, true)?;
    let weighted_end = u32::try_from(instances.len())
        .map_err(|_| AlignmentSummaryGpuPageError::CapacityOverflow)?;
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Modifications {
        instances,
        metadata: AlignmentSummaryModificationPageMetadata {
            background_range: 0..background_end,
            raw_range: raw_start..raw_end,
            weighted_range: weighted_start..weighted_end,
            codes,
        },
    })
}

pub(super) fn append_sorted_modification_candidates(
    instances: &mut Vec<AlignmentSummaryGpuModificationInstance>,
    groups: &BTreeMap<(u64, u64), Vec<ModificationPackingCandidate>>,
    probability_weighted: bool,
) -> Result<(), AlignmentSummaryGpuPageError> {
    for candidates in groups.values() {
        let mut candidates = candidates.clone();
        candidates.sort_by(|left, right| {
            let left_fraction = if probability_weighted {
                left.weighted_fraction
            } else {
                left.raw_fraction
            };
            let right_fraction = if probability_weighted {
                right.weighted_fraction
            } else {
                right.raw_fraction
            };
            left_fraction
                .total_cmp(&right_fraction)
                .then_with(|| right.series.cmp(&left.series))
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
                modified_calls: candidate.bin.modified_calls,
                canonical_observations: candidate.bin.canonical_observations,
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
