use crate::PresentedAlignmentLane;
use crate::inspection::model::SelectedSupplementaryChain;
use crate::inspection::rna::selected_supplementary_chain;
use anyhow::Result;
use rgv_alignment_scene::{AlignmentHandle, AlignmentPresentationScene};
use rgv_automation::{
    AutomationReadPresentationState, AutomationSelectionSnapshot, AutomationViewportOperation,
};
use rgv_core::{GenomicInterval, TrackSource, Viewport};
use rgv_engine::AlignmentIndividualReadCapability;
use rgv_hts::{AlignmentRecord, AuxValue, SupplementarySegment};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

pub(super) const MAXIMUM_AUTOMATION_SUPPLEMENTARY_SEGMENTS: usize = 64;

pub(super) fn automation_source_fingerprint(source: &TrackSource) -> Option<String> {
    let redacted = source.redacted_for_persistence();
    let encoded = serde_json::to_vec(&redacted).ok()?;
    let mut fingerprint = String::from("sha256:");
    for byte in Sha256::digest(encoded) {
        write!(&mut fingerprint, "{byte:02x}").ok()?;
    }
    Some(fingerprint)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutomationReadPresentationFields {
    pub(crate) state: AutomationReadPresentationState,
    pub(crate) requested_limit: usize,
    pub(crate) effective_limit: usize,
    pub(crate) eligible_records: usize,
    pub(crate) complete_records: usize,
    pub(crate) refusal_eligible_records: usize,
    pub(crate) refusal_scope: Option<String>,
}

pub(super) fn automation_scene_read_presentation_fields(
    presented: Option<PresentedAlignmentLane<'_>>,
    checking: bool,
    failed: bool,
    requested_limit: usize,
    effective_limit: usize,
) -> AutomationReadPresentationFields {
    if failed {
        return AutomationReadPresentationFields {
            state: AutomationReadPresentationState::Failed,
            requested_limit,
            effective_limit,
            eligible_records: 0,
            complete_records: 0,
            refusal_eligible_records: 0,
            refusal_scope: None,
        };
    }
    let Some(presented) = presented else {
        return AutomationReadPresentationFields {
            state: if checking {
                AutomationReadPresentationState::Checking
            } else {
                AutomationReadPresentationState::Unavailable
            },
            requested_limit,
            effective_limit,
            eligible_records: 0,
            complete_records: 0,
            refusal_eligible_records: 0,
            refusal_scope: None,
        };
    };
    match presented.source.individual_reads {
        AlignmentIndividualReadCapability::Complete { scene_revision } => {
            let records = presented
                .lane
                .snapshot
                .as_ref()
                .filter(|snapshot| snapshot.revision() == scene_revision)
                .map_or(0, |snapshot| snapshot.reads().evidence().record_count());
            let complete = records > 0 || presented.lane.snapshot.is_some();
            AutomationReadPresentationFields {
                state: if complete {
                    AutomationReadPresentationState::Complete
                } else {
                    AutomationReadPresentationState::Checking
                },
                requested_limit,
                effective_limit,
                eligible_records: records,
                complete_records: records,
                refusal_eligible_records: 0,
                refusal_scope: None,
            }
        }
        AlignmentIndividualReadCapability::Refused {
            eligible_records,
            complete_read_limit,
            ..
        } => AutomationReadPresentationFields {
            state: AutomationReadPresentationState::TooDense,
            requested_limit,
            effective_limit: complete_read_limit,
            eligible_records: usize::try_from(eligible_records).unwrap_or(usize::MAX),
            complete_records: 0,
            refusal_eligible_records: usize::try_from(eligible_records).unwrap_or(usize::MAX),
            refusal_scope: Some("scene_horizon".to_owned()),
        },
        AlignmentIndividualReadCapability::Pending => AutomationReadPresentationFields {
            state: AutomationReadPresentationState::Checking,
            requested_limit,
            effective_limit,
            eligible_records: 0,
            complete_records: 0,
            refusal_eligible_records: 0,
            refusal_scope: None,
        },
    }
}

pub(super) fn automation_locus(contig: &str, interval: GenomicInterval) -> String {
    format!(
        "{}:{}-{}",
        contig,
        interval.start().saturating_add(1),
        interval.end()
    )
}

pub(super) fn bounded_automation_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    format!("{}…", &value[..boundary])
}

pub(super) fn automation_supplementary_segment(
    record: &AlignmentRecord,
    reference_name: &str,
) -> SupplementarySegment {
    let edit_distance = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"NM")
        .and_then(|field| match field.value {
            AuxValue::Signed(value) => u32::try_from(value).ok(),
            AuxValue::Unsigned(value) => u32::try_from(value).ok(),
            _ => None,
        })
        .unwrap_or_default();
    SupplementarySegment {
        reference_name: reference_name.to_owned(),
        start: record.start,
        end: record.end,
        reverse: record.flags.is_reverse(),
        cigar: record.cigar.clone(),
        mapping_quality: record.mapping_quality,
        edit_distance,
    }
}

pub(super) fn automation_scene_supplementary_chain(
    scene: &AlignmentPresentationScene,
    handle: AlignmentHandle,
    record: &AlignmentRecord,
    current_contig: &str,
) -> Result<Option<SelectedSupplementaryChain>> {
    let mut chain = selected_supplementary_chain(record, current_contig).unwrap_or_else(|| {
        SelectedSupplementaryChain {
            read_name: record.read_name.clone(),
            segments: vec![automation_supplementary_segment(record, current_contig)],
            active_segment: 0,
            malformed_entries: 0,
            truncated: false,
            invalid_tag_type: false,
        }
    });
    for candidate_handle in scene.supplementary_chain_handles(handle) {
        if chain.segments.len() >= MAXIMUM_AUTOMATION_SUPPLEMENTARY_SEGMENTS {
            chain.truncated = true;
            break;
        }
        let Some(candidate) = scene.inspect_retained(candidate_handle)? else {
            continue;
        };
        let segment = automation_supplementary_segment(&candidate, current_contig);
        if !chain.segments.iter().any(|existing| {
            existing.reference_name == segment.reference_name
                && existing.start == segment.start
                && existing.reverse == segment.reverse
                && existing.cigar == segment.cigar
        }) {
            chain.segments.push(segment);
        }
    }
    Ok((chain.segments.len() > 1).then_some(chain))
}

pub(crate) fn automation_read_evidence_field_is_public(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "read" | "sequence" | "sequence copy orientation" | "template identity"
    ) && !normalized.contains("read group")
}

pub(crate) fn observed_retained_selection(
    retained: &AutomationSelectionSnapshot,
    viewport: GenomicInterval,
    viewport_contig: &str,
) -> AutomationSelectionSnapshot {
    let mut snapshot = retained.clone();
    snapshot.visible_in_active_view = snapshot.contig == viewport_contig
        && snapshot
            .evidence_start
            .zip(snapshot.evidence_end)
            .and_then(|(start, end)| GenomicInterval::new(viewport.contig(), start, end).ok())
            .is_some_and(|evidence| viewport.contains_interval(evidence));
    snapshot
}

pub(crate) fn automation_viewport_after_operation(
    current: Viewport,
    contig_length: u64,
    operation: AutomationViewportOperation,
) -> Result<Viewport> {
    Ok(match operation {
        AutomationViewportOperation::PanBases { delta } => {
            current.pan_pixels(delta as f64 / current.bases_per_pixel(), contig_length)?
        }
        AutomationViewportOperation::PanFraction { fraction } => {
            current.pan_pixels(current.pixel_width() * fraction, contig_length)?
        }
        AutomationViewportOperation::Zoom {
            factor,
            anchor_position,
        } => {
            let anchor_x = anchor_position.map_or(current.pixel_width() / 2.0, |position| {
                current.pixel_at_genomic(position)
            });
            current.zoom_at(anchor_x, factor.recip(), contig_length)?
        }
        AutomationViewportOperation::FitInterval { start, end } => {
            let bounded_length = contig_length.max(1);
            let start = start.min(bounded_length.saturating_sub(1));
            let end = end.min(bounded_length).max(start.saturating_add(1));
            let interval =
                GenomicInterval::bounded(current.interval().contig(), start, end, contig_length)?;
            Viewport::new(interval, current.pixel_width())?
        }
    })
}
