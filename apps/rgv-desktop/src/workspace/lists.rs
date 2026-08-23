use super::super::*;

pub(crate) fn next_available_item_id(ids: impl IntoIterator<Item = u64>) -> Option<u64> {
    let used = ids.into_iter().collect::<BTreeSet<_>>();
    let search_end = u64::try_from(used.len()).ok()?.checked_add(1)?;
    (1..=search_end).find(|id| !used.contains(id))
}

pub(crate) fn merge_imported_regions(
    destination: &mut Vec<RegionOfInterest>,
    imported: Vec<RegionOfInterest>,
    maximum: usize,
) -> (usize, usize) {
    let imported_count = imported.len();
    let available = maximum.saturating_sub(destination.len());
    let mut used = destination
        .iter()
        .map(|region| region.id)
        .collect::<BTreeSet<_>>();
    let mut candidate = 1_u64;
    let mut added = 0;
    for mut region in imported.into_iter().take(available) {
        let Some(id) = take_next_available_id(&mut used, &mut candidate) else {
            break;
        };
        region.id = id;
        destination.push(region);
        added += 1;
    }
    normalize_regions_and_review_items(destination, &mut []);
    (added, imported_count.saturating_sub(added))
}

pub(crate) fn merge_imported_review_items(
    destination: &mut Vec<ReviewItem>,
    imported: Vec<ReviewItem>,
    maximum: usize,
) -> (usize, usize) {
    let imported_count = imported.len();
    let available = maximum.saturating_sub(destination.len());
    let mut used = destination
        .iter()
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    let mut candidate = 1_u64;
    let mut added = 0;
    for mut item in imported.into_iter().take(available) {
        let Some(id) = take_next_available_id(&mut used, &mut candidate) else {
            break;
        };
        item.id = id;
        destination.push(item);
        added += 1;
    }
    normalize_regions_and_review_items(&mut [], destination);
    (added, imported_count.saturating_sub(added))
}

pub(crate) fn take_next_available_id(used: &mut BTreeSet<u64>, candidate: &mut u64) -> Option<u64> {
    while used.contains(candidate) || *candidate == 0 {
        *candidate = candidate.checked_add(1)?;
    }
    let id = *candidate;
    used.insert(id);
    *candidate = candidate.checked_add(1).unwrap_or(u64::MAX);
    Some(id)
}

pub(crate) fn imported_list_status(kind: &str, added: usize, omitted: usize, path: &str) -> String {
    if omitted == 0 {
        format!("Imported {added} {kind} from {path}")
    } else {
        format!("Imported {added} {kind} from {path}; omitted {omitted} at the workspace cap")
    }
}

pub(crate) fn metadata_matching_track_ids(
    tracks: &[MetadataTrackRow],
    metadata: &SampleMetadataTable,
    column_index: usize,
    value: &str,
) -> BTreeSet<TrackId> {
    let sample_keys = metadata.sample_keys_matching(column_index, value);
    tracks
        .iter()
        .filter(|(_, _, sample_key, _)| {
            sample_key
                .as_deref()
                .is_some_and(|key| sample_keys.contains(key))
        })
        .map(|(id, _, _, _)| *id)
        .collect()
}

pub(crate) fn apply_quantitative_bulk_action(
    tracks: &mut [TrackConfig],
    selected: &BTreeSet<TrackId>,
    action: QuantitativeBulkAction,
) -> bool {
    let overlay_group = (action == QuantitativeBulkAction::Overlay).then(|| {
        tracks
            .iter()
            .filter_map(|track| track.quantitative.overlay_group)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1)
    });
    let mut changed = false;
    for track in tracks
        .iter_mut()
        .filter(|track| track.kind == TrackKind::Quantitative && selected.contains(&track.id))
    {
        let previous = track.quantitative.clone();
        match action {
            QuantitativeBulkAction::Autoscale => {
                track.quantitative.scale_mode = QuantitativeScaleMode::Automatic;
                track.quantitative.scale_group = None;
                track.quantitative.overlay_group = None;
            }
            QuantitativeBulkAction::Overlay => {
                track.quantitative.scale_mode = QuantitativeScaleMode::Shared;
                track.quantitative.scale_group = overlay_group;
                track.quantitative.overlay_group = overlay_group;
            }
            QuantitativeBulkAction::Separate => {
                track.quantitative.overlay_group = None;
                if track.quantitative.scale_mode == QuantitativeScaleMode::Shared {
                    track.quantitative.scale_mode = QuantitativeScaleMode::Automatic;
                    track.quantitative.scale_group = None;
                }
            }
        }
        changed |= previous != track.quantitative;
    }
    changed
}

pub(crate) fn apply_compatible_auxiliary_appearance(
    tracks: &mut [TrackConfig],
    selected: &BTreeSet<TrackId>,
) -> bool {
    let Some(source) = tracks
        .iter()
        .find(|track| selected.contains(&track.id) && track.kind != TrackKind::Alignment)
        .cloned()
    else {
        return false;
    };
    let selected_count = tracks
        .iter()
        .filter(|track| selected.contains(&track.id) && track.kind == source.kind)
        .count();
    if selected_count < 2
        || tracks
            .iter()
            .any(|track| selected.contains(&track.id) && track.kind != source.kind)
    {
        return false;
    }
    let mut changed = false;
    for track in tracks
        .iter_mut()
        .filter(|track| selected.contains(&track.id) && track.id != source.id)
    {
        let previous = track.clone();
        match source.kind {
            TrackKind::Variant => track.variant.clone_from(&source.variant),
            TrackKind::Mutation => track.mutation.clone_from(&source.mutation),
            TrackKind::Gwas => track.gwas.clone_from(&source.gwas),
            TrackKind::Quantitative | TrackKind::CopyNumber => {
                let arithmetic = track.quantitative.arithmetic;
                let operand = track.quantitative.operand_track;
                let overlay_group = track.quantitative.overlay_group;
                let scale_group = track.quantitative.scale_group;
                track.quantitative.clone_from(&source.quantitative);
                track.quantitative.arithmetic = arithmetic;
                track.quantitative.operand_track = operand;
                track.quantitative.overlay_group = overlay_group;
                track.quantitative.scale_group = scale_group;
                if let Some(group) = overlay_group {
                    track.quantitative.scale_mode = QuantitativeScaleMode::Shared;
                    track.quantitative.scale_group = Some(group);
                }
                if source.kind == TrackKind::CopyNumber {
                    track.copy_number.clone_from(&source.copy_number);
                }
            }
            TrackKind::Interaction => track.interaction.clone_from(&source.interaction),
            TrackKind::Alignment
            | TrackKind::Reference
            | TrackKind::Feature
            | TrackKind::GeneModel => return false,
        }
        changed |= *track != previous;
    }
    changed
}

pub(crate) fn sample_association_ui(
    ui: &mut egui::Ui,
    sample_key: &mut Option<String>,
    metadata: Option<&SampleMetadataTable>,
) {
    ui.horizontal(|ui| {
        let label = ui.label("Sample key");
        let mut editor = sample_key.clone().unwrap_or_default();
        if ui
            .add(
                egui::TextEdit::singleline(&mut editor)
                    .desired_width(180.0)
                    .hint_text("Unassigned"),
            )
            .labelled_by(label.id)
            .changed()
        {
            truncate_string_to_bytes(&mut editor, rgv_core::MAXIMUM_SAMPLE_KEY_BYTES);
            *sample_key = (!editor.trim().is_empty()).then_some(editor);
        }
        if let Some(metadata) = metadata {
            let picker_label = ui.label("Known samples");
            egui::ComboBox::from_id_salt("track-sample-key")
                .selected_text(sample_key.as_deref().unwrap_or("Choose…"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(sample_key, None, "Unassigned");
                    for row in metadata.rows.iter().take(MAX_METADATA_FILTER_VALUES) {
                        ui.selectable_value(
                            sample_key,
                            Some(row.sample_key.clone()),
                            &row.sample_key,
                        );
                    }
                })
                .response
                .labelled_by(picker_label.id);
        }
    });
    match (metadata, sample_key.as_deref()) {
        (None, Some(_)) => {
            ui.small("The stable association is retained; import sample metadata to resolve it.");
        }
        (Some(metadata), Some(key)) if metadata.row(key).is_none() => {
            ui.colored_label(
                egui::Color32::DARK_RED,
                "This sample key is not present in the loaded metadata table.",
            );
        }
        (Some(metadata), _) if metadata.rows.len() > MAX_METADATA_FILTER_VALUES => {
            ui.small(format!(
                "The picker shows the first {MAX_METADATA_FILTER_VALUES} samples; type any exact key directly."
            ));
        }
        _ => {}
    }
}

pub(crate) fn normalize_sample_association(sample_key: &mut Option<String>) {
    let Some(key) = sample_key else {
        return;
    };
    truncate_string_to_bytes(key, rgv_core::MAXIMUM_SAMPLE_KEY_BYTES);
    let trimmed = key.trim();
    if trimmed.is_empty() {
        *sample_key = None;
    } else if trimmed.len() != key.len() {
        *key = trimmed.to_owned();
    }
}

pub(crate) fn region_overlay_horizontal_span(
    region: &RegionOfInterest,
    contig: &str,
    viewport: GenomicInterval,
    bases_per_pixel: f64,
    panel_left: f32,
) -> Option<(f32, f32)> {
    if region.locus.contig != contig
        || region.locus.start >= viewport.end()
        || region.locus.end <= viewport.start()
        || !bases_per_pixel.is_finite()
        || bases_per_pixel <= 0.0
    {
        return None;
    }
    let start = region.locus.start.max(viewport.start());
    let end = region.locus.end.min(viewport.end());
    let x = panel_left + (start - viewport.start()) as f32 / bases_per_pixel as f32;
    let right = panel_left + (end - viewport.start()) as f32 / bases_per_pixel as f32;
    Some((x, right))
}

pub(crate) fn normalize_regions_and_review_items(
    regions: &mut [RegionOfInterest],
    review_items: &mut [ReviewItem],
) {
    for region in regions {
        truncate_string_to_bytes(&mut region.name, 256);
        let trimmed = region.name.trim();
        if trimmed.is_empty() {
            region.name = format!("Region {}", region.id);
        } else if trimmed.len() != region.name.len() {
            region.name = trimmed.to_owned();
        }
    }
    for item in review_items {
        truncate_string_to_bytes(&mut item.label, 256);
        truncate_string_to_bytes(&mut item.notes, MAXIMUM_REVIEW_NOTE_BYTES);
        let trimmed = item.label.trim();
        if trimmed.is_empty() {
            item.label = session_locus_text(&item.locus);
        } else if trimmed.len() != item.label.len() {
            item.label = trimmed.to_owned();
        }
        if let Some(assignment) = &mut item.assignment {
            truncate_string_to_bytes(assignment, 256);
            let trimmed = assignment.trim();
            if trimmed.is_empty() {
                item.assignment = None;
            } else if trimmed.len() != assignment.len() {
                *assignment = trimmed.to_owned();
            }
        }
    }
}

pub(crate) fn session_locus_text(locus: &SessionLocus) -> String {
    format!(
        "{}:{}-{}",
        locus.contig,
        locus.start.saturating_add(1),
        locus.end
    )
}

pub(crate) const fn review_status_label(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Pending => "Pending",
        ReviewStatus::InProgress => "In progress",
        ReviewStatus::Complete => "Complete",
        ReviewStatus::Skipped => "Skipped",
    }
}

pub(crate) fn adjacent_review_locus(
    items: &[ReviewItem],
    status_filter: Option<ReviewStatus>,
    contig: &str,
    start: u64,
    end: u64,
    forward: bool,
) -> Option<SessionLocus> {
    let candidates = items
        .iter()
        .filter(|item| status_filter.is_none_or(|status| item.status == status))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let current = candidates.iter().position(|item| {
        item.locus.contig == contig && item.locus.start == start && item.locus.end == end
    });
    let index = match (current, forward) {
        (Some(index), true) => (index + 1) % candidates.len(),
        (Some(0) | None, false) => candidates.len() - 1,
        (Some(index), false) => index - 1,
        (None, true) => 0,
    };
    Some(candidates[index].locus.clone())
}
