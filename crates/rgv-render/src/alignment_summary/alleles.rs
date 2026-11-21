//! Allele (and bisulfite) summary pages: instance layout, CPU oracles and packing.
//!
//! Moved verbatim out of `alignment_summary.rs`; items that file and its tests use are
//! `pub(super)`.

// The family modules share the parent module's namespace, exactly as when they were one file.
#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AlignmentSummaryAlleleToken {
    A = 0,
    C = 1,
    G = 2,
    T = 3,
    N = 4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AlignmentSummaryGpuAlleleInstance {
    pub start_offset: [f32; 2],
    pub end_offset: [f32; 2],
    pub local_depth: u32,
    pub allele: u32,
    /// `[fraction, aggregate alternate fraction, preceding stack fraction]`.
    pub raw: [f32; 3],
    /// `[fraction, aggregate alternate fraction, preceding stack fraction]`.
    pub quality_weighted: [f32; 3],
    /// `[0 generic / 1 bisulfite, context mask, strand token, observation token]`.
    pub interpretation: [u32; 4],
}

impl AlignmentSummaryGpuAlleleInstance {
    pub(super) fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
            0 => Float32x2,
            1 => Float32x2,
            2 => Uint32,
            3 => Uint32,
            4 => Float32x3,
            5 => Float32x3,
            6 => Uint32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRIBUTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedAllele {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub allele: AlignmentSummaryAlleleToken,
    pub fraction: f64,
    pub aggregate_alternate_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSummaryGpuProjectedBisulfite {
    pub bounds: AlignmentSummaryGpuProjectedBounds,
    pub observation: BisulfiteObservationToken,
    pub strand: CpuBisulfiteStrandToken,
    pub fraction: f64,
}

#[must_use]
pub fn project_alignment_summary_allele_instance(
    instance: AlignmentSummaryGpuAlleleInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
    minimum_alternate_fraction: f64,
    quality_weighted: bool,
) -> Option<AlignmentSummaryGpuProjectedAllele> {
    if instance.interpretation[0] != 0 {
        return None;
    }
    let values = if quality_weighted {
        instance.quality_weighted
    } else {
        instance.raw
    };
    let fraction = f64::from(values[0]);
    let aggregate = f64::from(values[1]);
    let preceding = f64::from(values[2]);
    if instance.local_depth == 0 || fraction <= 0.0 || aggregate < minimum_alternate_fraction {
        return None;
    }
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let coverage_height =
        (transformed_depth_fraction(instance.local_depth, scale_maximum, transform)
            * camera.clip().height)
            .max(1.0);
    let semantic_height = fraction * coverage_height;
    let height = semantic_height.max(1.0).min(coverage_height);
    let y = (camera.clip().y + camera.clip().height - (preceding + fraction) * coverage_height)
        .max(camera.clip().y);
    Some(AlignmentSummaryGpuProjectedAllele {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y,
            width,
            height,
        },
        allele: allele_token(instance.allele)?,
        fraction,
        aggregate_alternate_fraction: aggregate,
    })
}

#[must_use]
pub fn project_alignment_summary_bisulfite_instance(
    instance: AlignmentSummaryGpuAlleleInstance,
    page_start: u64,
    camera: CpuSummaryCamera,
    scale_maximum: u32,
    transform: CoverageScaleTransform,
    quality_weighted: bool,
    context: BisulfiteContext,
) -> Option<AlignmentSummaryGpuProjectedBisulfite> {
    if instance.interpretation[0] != 1
        || instance.interpretation[1] & u32::from(bisulfite_context_mask(context)) == 0
    {
        return None;
    }
    let values = if quality_weighted {
        instance.quality_weighted
    } else {
        instance.raw
    };
    let fraction = f64::from(values[0]);
    let preceding = f64::from(values[2]);
    if instance.local_depth == 0 || fraction <= 0.0 {
        return None;
    }
    let (x, width) = projected_horizontal(
        instance.start_offset,
        instance.end_offset,
        page_start,
        camera,
    )?;
    let coverage_height =
        (transformed_depth_fraction(instance.local_depth, scale_maximum, transform)
            * camera.clip().height)
            .max(1.0);
    let semantic_height = fraction * coverage_height;
    let height = semantic_height.max(1.0).min(coverage_height);
    let y = (camera.clip().y + camera.clip().height - (preceding + fraction) * coverage_height)
        .max(camera.clip().y);
    let strand = match instance.interpretation[2] {
        0 => CpuBisulfiteStrandToken::Forward,
        1 => CpuBisulfiteStrandToken::Reverse,
        _ => return None,
    };
    let observation = match instance.interpretation[3] {
        0 => BisulfiteObservationToken::Protected,
        1 => BisulfiteObservationToken::Converted,
        _ => return None,
    };
    Some(AlignmentSummaryGpuProjectedBisulfite {
        bounds: AlignmentSummaryGpuProjectedBounds {
            x,
            y,
            width,
            height,
        },
        observation,
        strand,
        fraction,
    })
}

pub(super) const fn allele_token(value: u32) -> Option<AlignmentSummaryAlleleToken> {
    match value {
        0 => Some(AlignmentSummaryAlleleToken::A),
        1 => Some(AlignmentSummaryAlleleToken::C),
        2 => Some(AlignmentSummaryAlleleToken::G),
        3 => Some(AlignmentSummaryAlleleToken::T),
        4 => Some(AlignmentSummaryAlleleToken::N),
        _ => None,
    }
}

pub(super) fn pack_allele_page(
    page: &SummarySemanticPage,
    depth_page: &SummarySemanticPage,
) -> Result<PackedAlignmentSummaryPage, AlignmentSummaryGpuPageError> {
    AlignmentSummaryGpuPageKey::alleles(page, depth_page)?;
    let depth = AlignmentSummaryDepthRangeIndex::build(depth_page)?;
    let reference_contexts = page
        .allele_reference_contexts()
        .filter(|contexts| contexts.len() == page.primitives().len())
        .ok_or(AlignmentSummaryGpuPageError::MissingReferenceContext(
            page.key(),
        ))?;
    let mut instances = Vec::with_capacity(page.primitives().len().saturating_mul(8));
    for (primitive_index, primitive) in page.primitives().iter().enumerate() {
        if primitive.start_offset >= primitive.end_offset {
            return Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan {
                page: page.key(),
                start: primitive.start_offset,
                end: primitive.end_offset,
            });
        }
        let SummarySemanticComponent::Alleles {
            bin,
            reference_base: Some(reference_base),
        } = primitive.component
        else {
            return Err(AlignmentSummaryGpuPageError::MissingReferenceBase(
                page.key(),
            ));
        };
        let start = page
            .interval()
            .start()
            .saturating_add(primitive.start_offset);
        let end = page.interval().start().saturating_add(primitive.end_offset);
        let local_depth = depth.maximum_depth_exact(start as f64, end as f64);
        let raw_denominator = f64::from(bin.alleles.base_observations());
        let weighted_denominator = bin.quality_weighted.total() as f64;
        let bases = [
            (AlignmentSummaryAlleleToken::A, b'A'),
            (AlignmentSummaryAlleleToken::C, b'C'),
            (AlignmentSummaryAlleleToken::G, b'G'),
            (AlignmentSummaryAlleleToken::T, b'T'),
            (AlignmentSummaryAlleleToken::N, b'N'),
        ];
        let raw = bases.map(|(_, base)| {
            if base == reference_base.to_ascii_uppercase() {
                0.0
            } else {
                f64::from(bin.alleles.for_base(base))
            }
        });
        let weighted = bases.map(|(_, base)| {
            if base == reference_base.to_ascii_uppercase() {
                0.0
            } else {
                bin.quality_weighted.for_base(base) as f64
            }
        });
        let raw_alternate = raw.iter().sum::<f64>();
        let weighted_alternate = weighted.iter().sum::<f64>();
        let mut raw_before = 0.0;
        let mut weighted_before = 0.0;
        for (index, (allele, base)) in bases.into_iter().enumerate() {
            if base == reference_base.to_ascii_uppercase() {
                continue;
            }
            let raw_fraction = safe_fraction(raw[index], raw_denominator);
            let weighted_fraction = safe_fraction(weighted[index], weighted_denominator);
            if raw_fraction == 0.0 && weighted_fraction == 0.0 {
                continue;
            }
            instances.push(AlignmentSummaryGpuAlleleInstance {
                start_offset: split_f64(primitive.start_offset as f64),
                end_offset: split_f64(primitive.end_offset as f64),
                local_depth,
                allele: allele as u32,
                raw: [
                    raw_fraction as f32,
                    safe_fraction(raw_alternate, raw_denominator) as f32,
                    raw_before as f32,
                ],
                quality_weighted: [
                    weighted_fraction as f32,
                    safe_fraction(weighted_alternate, weighted_denominator) as f32,
                    weighted_before as f32,
                ],
                interpretation: [0; 4],
            });
            raw_before += raw_fraction;
            weighted_before += weighted_fraction;
        }

        let reference_context = reference_contexts[primitive_index];
        let forward_mask = u32::from(reference_context.context_mask(false));
        // The CPU oracle deliberately gives forward context precedence at the rare sites where
        // policies overlap. Encode that precedence once in immutable instance masks.
        let reverse_mask = u32::from(reference_context.context_mask(true)) & !forward_mask;
        for (context_mask, strand, strand_alleles, strand_weighted, protected, converted) in [
            (
                forward_mask,
                0_u32,
                bin.bisulfite_forward_alleles,
                bin.bisulfite_forward_quality_weighted,
                b'C',
                b'T',
            ),
            (
                reverse_mask,
                1_u32,
                bin.bisulfite_reverse_alleles,
                bin.bisulfite_reverse_quality_weighted,
                b'G',
                b'A',
            ),
        ] {
            if context_mask == 0 {
                continue;
            }
            let raw_values = [
                f64::from(strand_alleles.for_base(protected)),
                f64::from(strand_alleles.for_base(converted)),
            ];
            let weighted_values = [
                strand_weighted.for_base(protected) as f64,
                strand_weighted.for_base(converted) as f64,
            ];
            let raw_total = raw_values.iter().sum::<f64>();
            let weighted_total = weighted_values.iter().sum::<f64>();
            let mut raw_before = 0.0_f64;
            let mut weighted_before = 0.0_f64;
            for observation in 0..2 {
                let raw_fraction = safe_fraction(raw_values[observation], raw_denominator);
                let weighted_fraction =
                    safe_fraction(weighted_values[observation], weighted_denominator);
                if raw_fraction == 0.0 && weighted_fraction == 0.0 {
                    continue;
                }
                instances.push(AlignmentSummaryGpuAlleleInstance {
                    start_offset: split_f64(primitive.start_offset as f64),
                    end_offset: split_f64(primitive.end_offset as f64),
                    local_depth,
                    allele: 0,
                    raw: [
                        raw_fraction as f32,
                        safe_fraction(raw_total, raw_denominator) as f32,
                        raw_before as f32,
                    ],
                    quality_weighted: [
                        weighted_fraction as f32,
                        safe_fraction(weighted_total, weighted_denominator) as f32,
                        weighted_before as f32,
                    ],
                    interpretation: [1, context_mask, strand, observation as u32],
                });
                raw_before += raw_fraction;
                weighted_before += weighted_fraction;
            }
        }
    }
    instances.shrink_to_fit();
    Ok(PackedAlignmentSummaryPage::Alleles { instances })
}
