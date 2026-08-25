use super::super::*;

pub(crate) const MAX_JUNCTION_ANNOTATION_RECORDS: usize = 200_000;
pub(crate) const MAX_JUNCTION_ANNOTATION_LABELS_PER_EVENT: usize = 8;

#[derive(Clone, Debug, Default)]
pub(crate) struct JunctionAnnotationContext {
    pub(crate) lookup: JunctionAnnotationLookup,
    pub(crate) labels: BTreeMap<(u64, u64), Vec<String>>,
    pub(crate) source_records_truncated: bool,
}

#[derive(Clone)]
pub(crate) struct SashimiExportTrack {
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) query: Arc<AlignmentQuery>,
    pub(crate) filters: SessionFilters,
}

pub(crate) struct SashimiExportBundle {
    pub(crate) tracks: Vec<SashimiExportTrack>,
    pub(crate) omitted_tracks: usize,
    pub(crate) viewport: Viewport,
    pub(crate) contig_name: String,
    pub(crate) annotations: JunctionAnnotationContext,
}

#[derive(Clone)]
pub(crate) struct AutomationAlignmentExportTrack {
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) query: Arc<AlignmentQuery>,
}

impl JunctionAnnotationContext {
    pub(crate) fn status(&self, start: u64, end: u64) -> JunctionAnnotationStatus {
        self.lookup.status(start, end)
    }

    pub(crate) fn labels(&self, start: u64, end: u64) -> &[String] {
        self.labels.get(&(start, end)).map_or(&[], Vec::as_slice)
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn junction_annotation_context(
    configs: &TrackCollection,
    snapshots: &[TrackSnapshot],
    loaded_interval: Option<GenomicInterval>,
    viewport_interval: GenomicInterval,
) -> JunctionAnnotationContext {
    let mut source_tracks = 0_usize;
    let mut processed_records = 0_usize;
    let mut source_records_truncated = false;
    let mut transcripts = BTreeMap::<(u64, String), (Vec<(u64, u64)>, BTreeSet<String>)>::new();

    for config in configs
        .tracks()
        .iter()
        .filter(|config| config.visible && config.kind == TrackKind::GeneModel)
    {
        let Some(records) = snapshots
            .iter()
            .find(|snapshot| snapshot.id == config.id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) => Some(records.as_slice()),
                TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
        else {
            continue;
        };
        source_tracks = source_tracks.saturating_add(1);
        for record in records {
            if processed_records >= MAX_JUNCTION_ANNOTATION_RECORDS {
                source_records_truncated = true;
                break;
            }
            processed_records = processed_records.saturating_add(1);
            let label = feature_annotation_label(record, &config.name);
            if record.blocks.len() >= 2 {
                let key = (
                    config.id.get(),
                    format!("blocks:{}", record.name.as_deref().unwrap_or("unnamed")),
                );
                let group = transcripts.entry(key).or_default();
                group.0.extend(record.blocks.iter().copied());
                group.1.insert(label);
                continue;
            }
            if !record
                .feature_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("exon"))
            {
                continue;
            }
            let transcript_keys = record
                .attributes
                .iter()
                .find(|(key, _)| key == "transcript_id" || key == "Parent")
                .map(|(_, value)| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                });
            let Some(transcript_keys) = transcript_keys else {
                continue;
            };
            for transcript in transcript_keys {
                let group = transcripts
                    .entry((config.id.get(), transcript.to_owned()))
                    .or_default();
                group.0.push((record.start, record.end));
                group.1.insert(label.clone());
            }
        }
    }

    let mut annotated = BTreeSet::new();
    let mut labels = BTreeMap::<(u64, u64), Vec<String>>::new();
    for (_, (mut exons, transcript_labels)) in transcripts {
        exons.sort_unstable();
        exons.dedup();
        for adjacent in exons.windows(2) {
            let [(left_start, left_end), (right_start, right_end)] = adjacent else {
                continue;
            };
            if left_start >= left_end || right_start >= right_end || left_end >= right_start {
                continue;
            }
            let junction = (*left_end, *right_start);
            annotated.insert(junction);
            let event_labels = labels.entry(junction).or_default();
            for label in &transcript_labels {
                if event_labels.len() == MAX_JUNCTION_ANNOTATION_LABELS_PER_EVENT {
                    break;
                }
                if !event_labels.contains(label) {
                    event_labels.push(label.clone());
                }
            }
        }
    }
    let complete_for_interval = loaded_interval.is_some_and(|loaded| {
        loaded.contig() == viewport_interval.contig()
            && loaded.start() < viewport_interval.start()
            && loaded.end() > viewport_interval.end()
    }) && !source_records_truncated;
    JunctionAnnotationContext {
        lookup: JunctionAnnotationLookup::new(annotated, source_tracks, complete_for_interval),
        labels,
        source_records_truncated,
    }
}

pub(crate) fn feature_annotation_label(
    record: &rgv_hts::FeatureRecord,
    track_name: &str,
) -> String {
    record
        .attributes
        .iter()
        .find(|(key, _)| key == "gene_name" || key == "gene_id" || key == "Name")
        .map(|(_, value)| value.as_str())
        .or(record.name.as_deref())
        .unwrap_or(track_name)
        .chars()
        .take(128)
        .collect()
}

pub(crate) fn rna_export_file_name(track_name: &str, suffix: &str, extension: &str) -> String {
    let stem = track_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .take(80)
        .collect::<String>();
    format!("{}-{suffix}.{extension}", stem.trim_matches('-'))
}

pub(crate) fn junction_bed_export(
    track: &SashimiExportTrack,
    contig: &str,
    viewport: GenomicInterval,
    annotations: &JunctionAnnotationContext,
) -> String {
    let provenance = track.query.junctions.provenance;
    let mut output = String::new();
    for line in [
        "# RGV splice junction BED export v1".to_owned(),
        "# coordinates\tzero-based half-open skipped interval".to_owned(),
        format!("# track\t{}", tsv_cell(&track.name)),
        format!(
            "# filters\tminimum_depth={}\tminimum_flank={}",
            track.filters.minimum_junction_depth, provenance.minimum_flank
        ),
        format!(
            "# provenance\teligible_records={}\tspliced_records={}\tsupporting_records={}\tobservations_retained={}\tobservations_filtered_by_flank={}\tall_eligible_records={}\tdropped_distinct={}\tdropped_per_record={}",
            provenance.eligible_records,
            provenance.spliced_records,
            provenance.supporting_spliced_records,
            provenance.junction_observations_retained,
            provenance.junction_observations_filtered_by_flank,
            provenance.all_eligible_records,
            provenance.dropped_distinct_junctions,
            provenance.dropped_record_junction_observations,
        ),
        "# columns\tchrom\tstart\tend\tname\tscore\tstrand\tdepth\tpositive\tnegative\tunknown\tminimum_left_flank\tminimum_right_flank\tannotation_status\tannotation_labels".to_owned(),
    ] {
        if !push_rna_export_line(&mut output, &line) {
            return output;
        }
    }
    for junction in track.query.junctions.junctions.iter().filter(|junction| {
        junction.depth >= track.filters.minimum_junction_depth
            && junction.start < viewport.end()
            && junction.end > viewport.start()
    }) {
        let strand = match (
            junction.positive_depth > 0,
            junction.negative_depth > 0,
            junction.unknown_depth > 0,
        ) {
            (true, false, false) => "+",
            (false, true, false) => "-",
            _ => ".",
        };
        let annotation = match annotations.status(junction.start, junction.end) {
            JunctionAnnotationStatus::Annotated => "annotated",
            JunctionAnnotationStatus::Novel => "novel",
            JunctionAnnotationStatus::Unknown => "unknown",
        };
        let line = format!(
            "{}\t{}\t{}\tjunction_{}_{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            tsv_cell(contig),
            junction.start,
            junction.end,
            junction.start.saturating_add(1),
            junction.end,
            junction.depth.min(1_000),
            strand,
            junction.depth,
            junction.positive_depth,
            junction.negative_depth,
            junction.unknown_depth,
            junction.minimum_left_flank,
            junction.minimum_right_flank,
            annotation,
            tsv_cell(&annotations.labels(junction.start, junction.end).join(",")),
        );
        if !push_rna_export_line(&mut output, &line) {
            break;
        }
    }
    output
}

pub(crate) fn push_rna_export_line(output: &mut String, line: &str) -> bool {
    let required = line.len().saturating_add(1);
    if output.len().saturating_add(required) <= MAX_RNA_EXPORT_BYTES {
        output.push_str(line);
        output.push('\n');
        return true;
    }
    let marker = "# export_truncated\tbyte_limit_reached\n";
    output.truncate(MAX_RNA_EXPORT_BYTES.saturating_sub(marker.len()));
    output.push_str(marker);
    false
}

pub(crate) fn sashimi_export_display(bundle: &SashimiExportBundle) -> (DisplayList, u32, u32) {
    let width = bundle.viewport.pixel_width().ceil().clamp(320.0, 8_192.0) as u32;
    let shared_raw = sashimi_export_shared_maximum(bundle, SashimiCountMode::Raw);
    let shared_cpm = sashimi_export_shared_maximum(bundle, SashimiCountMode::CountsPerMillion);
    let mut display = DisplayList::default();
    let mut top = 0.0_f32;
    for track in &bundle.tracks {
        let lane_height = match track.filters.junction_display_mode {
            JunctionDisplayMode::Collapsed => 90.0,
            JunctionDisplayMode::Expanded => 170.0,
        };
        display.texts.push(rgv_layout::TextPrimitive {
            x: 120.0,
            y: top + 12.0,
            text: track.name.chars().take(128).collect(),
            font_size: 12.0,
            color: rgv_layout::Rgba::new(0.08, 0.08, 0.10, 1.0),
        });
        let options = AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_junctions: true,
            show_reads: false,
            show_modification_frequency: false,
            junction_top: top + 24.0,
            junction_height: lane_height,
            minimum_junction_depth: track.filters.minimum_junction_depth,
            junction_display_mode: track.filters.junction_display_mode,
            junction_scale_maximum: match track.filters.sashimi_scale_mode {
                SashimiScaleMode::Automatic => None,
                SashimiScaleMode::Fixed => {
                    Some(f64::from(track.filters.sashimi_fixed_scale_maximum))
                }
                SashimiScaleMode::Shared => match track.filters.sashimi_count_mode {
                    SashimiCountMode::Raw => shared_raw,
                    SashimiCountMode::CountsPerMillion => shared_cpm,
                },
            },
            junction_count_mode: track.filters.sashimi_count_mode,
            show_junction_labels: track.filters.show_junction_labels,
            junction_annotation_fingerprint: bundle.annotations.lookup.fingerprint(),
            ..AlignmentLayoutOptions::default()
        };
        let layout = layout_sashimi(
            &track.query.junctions,
            bundle.viewport,
            options,
            Some(&bundle.annotations.lookup),
        );
        display
            .rectangles
            .extend(layout.display_list.rectangles.into_iter());
        display.texts.extend(layout.display_list.texts);
        top += lane_height + 34.0;
    }
    if bundle.omitted_tracks > 0 {
        display.texts.push(rgv_layout::TextPrimitive {
            x: 180.0,
            y: top + 12.0,
            text: format!(
                "{} additional selected/visible tracks omitted at the export cap",
                bundle.omitted_tracks
            ),
            font_size: 11.0,
            color: rgv_layout::Rgba::new(0.55, 0.24, 0.08, 1.0),
        });
        top += 28.0;
    }
    (display, width, top.ceil().clamp(1.0, 8_192.0) as u32)
}

pub(crate) fn sashimi_export_shared_maximum(
    bundle: &SashimiExportBundle,
    count_mode: SashimiCountMode,
) -> Option<f64> {
    let participants = bundle
        .tracks
        .iter()
        .filter(|track| {
            track.filters.sashimi_scale_mode == SashimiScaleMode::Shared
                && track.filters.sashimi_count_mode == count_mode
        })
        .collect::<Vec<_>>();
    if participants.is_empty()
        || (count_mode == SashimiCountMode::CountsPerMillion
            && participants.iter().any(|track| {
                track
                    .query
                    .junctions
                    .provenance
                    .normalization_mapped_records
                    .is_none()
            }))
    {
        return None;
    }
    participants
        .into_iter()
        .flat_map(|track| {
            track
                .query
                .junctions
                .junctions
                .iter()
                .filter(move |junction| junction.depth >= track.filters.minimum_junction_depth)
                .map(move |junction| {
                    junction_count_value(
                        &track.query.junctions,
                        junction,
                        track.filters.sashimi_count_mode,
                    )
                })
        })
        .reduce(f64::max)
}

pub(crate) fn sashimi_svg_export(
    display: &DisplayList,
    width: u32,
    height: u32,
    bundle: &SashimiExportBundle,
) -> String {
    let interval = bundle.viewport.interval();
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n<metadata>RGV Sashimi export v1; locus {}:{}-{}; tracks {}; omitted {}; coordinates shown one-based inclusive, internal junctions zero-based half-open</metadata>\n<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n",
        xml_escape(&bundle.contig_name),
        interval.start().saturating_add(1),
        interval.end(),
        bundle.tracks.len(),
        bundle.omitted_tracks,
    );
    let mut truncated = false;
    for rectangle in &display.rectangles {
        let red = (rectangle.color.red.clamp(0.0, 1.0) * 255.0).round() as u8;
        let green = (rectangle.color.green.clamp(0.0, 1.0) * 255.0).round() as u8;
        let blue = (rectangle.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8;
        let opacity = rectangle.color.alpha.clamp(0.0, 1.0);
        let element = if rectangle.shape.is_junction_arc() {
            let left = rectangle.x;
            let right = rectangle.x + rectangle.width;
            let top = rectangle.y;
            let bottom = rectangle.y + rectangle.height;
            let shoulder = rectangle.width * 0.18;
            let stroke_width = rgv_layout::junction_arc_stroke_width(rectangle.height);
            let (start_y, control_y, end_y) =
                if rectangle.shape == rgv_layout::RectShape::JunctionArcUp {
                    (
                        bottom - stroke_width * 0.5,
                        top + stroke_width * 0.5,
                        bottom - stroke_width * 0.5,
                    )
                } else {
                    (
                        top + stroke_width * 0.5,
                        bottom - stroke_width * 0.5,
                        top + stroke_width * 0.5,
                    )
                };
            format!(
                "<path d=\"M {left:.2} {start_y:.2} C {:.2} {control_y:.2}, {:.2} {control_y:.2}, {right:.2} {end_y:.2}\" fill=\"none\" stroke=\"rgb({red},{green},{blue})\" stroke-opacity=\"{opacity:.3}\" stroke-width=\"{stroke_width:.2}\" stroke-linecap=\"round\"/>\n",
                left + shoulder,
                right - shoulder,
            )
        } else {
            format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n",
                rectangle.x, rectangle.y, rectangle.width, rectangle.height,
            )
        };
        if output
            .len()
            .saturating_add(element.len())
            .saturating_add(64)
            > MAX_RNA_EXPORT_BYTES
        {
            truncated = true;
            break;
        }
        output.push_str(&element);
    }
    if !truncated {
        for text in &display.texts {
            let red = (text.color.red.clamp(0.0, 1.0) * 255.0).round() as u8;
            let green = (text.color.green.clamp(0.0, 1.0) * 255.0).round() as u8;
            let blue = (text.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8;
            let element = format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-family=\"monospace\" font-size=\"{:.2}\" fill=\"rgb({red},{green},{blue})\">{}</text>\n",
                text.x,
                text.y,
                text.font_size,
                xml_escape(&text.text),
            );
            if output
                .len()
                .saturating_add(element.len())
                .saturating_add(64)
                > MAX_RNA_EXPORT_BYTES
            {
                truncated = true;
                break;
            }
            output.push_str(&element);
        }
    }
    if truncated {
        output.push_str("<!-- export truncated at the declared byte limit -->\n");
    }
    output.push_str("</svg>\n");
    output
}

pub(crate) fn xml_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            '\'' => "&apos;".chars().collect(),
            _ => vec![character],
        })
        .collect()
}

pub(crate) fn selected_supplementary_chain(
    record: &AlignmentRecord,
    current_contig: &str,
) -> Option<SelectedSupplementaryChain> {
    let decoded = decode_supplementary_chain(record);
    if !decoded.tag_present && !record.flags.is_supplementary() {
        return None;
    }
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
    let mut segments = Vec::with_capacity(decoded.segments.len().saturating_add(1));
    segments.push(SupplementarySegment {
        reference_name: current_contig.to_owned(),
        start: record.start,
        end: record.end,
        reverse: record.flags.is_reverse(),
        cigar: record.cigar.clone(),
        mapping_quality: record.mapping_quality,
        edit_distance,
    });
    for segment in decoded.segments {
        if !segments.iter().any(|candidate| {
            candidate.reference_name == segment.reference_name
                && candidate.start == segment.start
                && candidate.reverse == segment.reverse
                && candidate.cigar == segment.cigar
        }) {
            segments.push(segment);
        }
    }
    Some(SelectedSupplementaryChain {
        read_name: record.read_name.clone(),
        segments,
        active_segment: 0,
        malformed_entries: decoded.malformed_entries,
        truncated: decoded.truncated,
        invalid_tag_type: decoded.invalid_tag_type,
    })
}

pub(crate) fn supplementary_chain_diagram(
    chain: &SelectedSupplementaryChain,
) -> SupplementaryChainDiagram {
    let segments = chain
        .segments
        .iter()
        .enumerate()
        .map(|(segment_index, segment)| {
            let (read_start, read_end, read_length) =
                segment_read_coordinates(&segment.cigar, segment.reverse);
            ChainDiagramSegment {
                segment_index,
                read_start,
                read_end,
                read_length,
            }
        })
        .collect::<Vec<_>>();
    let read_length = segments
        .iter()
        .map(|segment| segment.read_length)
        .max()
        .unwrap_or(1)
        .max(1);
    let mut read_order = (0..chain.segments.len()).collect::<Vec<_>>();
    read_order.sort_by_key(|index| {
        let segment = segments[*index];
        (segment.read_start, segment.read_end, segment.segment_index)
    });
    let mut alignment_order = (0..chain.segments.len()).collect::<Vec<_>>();
    alignment_order.sort_by(|left, right| {
        let left_segment = &chain.segments[*left];
        let right_segment = &chain.segments[*right];
        (
            left_segment.reference_name.as_str(),
            left_segment.start,
            left_segment.end,
            left_segment.reverse,
            *left,
        )
            .cmp(&(
                right_segment.reference_name.as_str(),
                right_segment.start,
                right_segment.end,
                right_segment.reverse,
                *right,
            ))
    });
    SupplementaryChainDiagram {
        read_length,
        segments,
        read_order,
        alignment_order,
    }
}

pub(crate) fn segment_read_coordinates(
    cigar: &[rgv_hts::CigarOp],
    reverse: bool,
) -> (u64, u64, u64) {
    let is_clip = |operation: &&rgv_hts::CigarOp| {
        matches!(
            operation.kind,
            CigarOpKind::SoftClip | CigarOpKind::HardClip
        )
    };
    let leading_clip = cigar
        .iter()
        .take_while(is_clip)
        .map(|operation| u64::from(operation.len))
        .sum::<u64>();
    let trailing_clip = cigar
        .iter()
        .rev()
        .take_while(is_clip)
        .map(|operation| u64::from(operation.len))
        .sum::<u64>();
    let aligned_query = cigar
        .iter()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::Match
                    | CigarOpKind::Insertion
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
            )
        })
        .map(|operation| u64::from(operation.len))
        .sum::<u64>();
    let read_length = leading_clip
        .saturating_add(aligned_query)
        .saturating_add(trailing_clip)
        .max(1);
    let read_start = if reverse { trailing_clip } else { leading_clip }.min(read_length);
    let read_end = read_start.saturating_add(aligned_query).min(read_length);
    (read_start, read_end, read_length)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn paint_supplementary_chain_diagram(
    ui: &mut egui::Ui,
    chain: &SelectedSupplementaryChain,
) -> Option<usize> {
    let diagram = supplementary_chain_diagram(chain);
    let desired = egui::vec2(ui.available_width().max(320.0), 112.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(250));
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(190)),
        egui::StrokeKind::Inside,
    );
    let label_width = 92.0;
    let lane = egui::Rect::from_min_max(
        egui::pos2(rect.left() + label_width, rect.top() + 8.0),
        egui::pos2(rect.right() - 8.0, rect.bottom() - 8.0),
    );
    let rows = [
        ("Read order", lane.top() + 13.0, &diagram.read_order),
        (
            "Alignment order",
            lane.top() + 61.0,
            &diagram.alignment_order,
        ),
    ];
    let mut clicked = None;
    for (row_number, (label, centre_y, order)) in rows.into_iter().enumerate() {
        painter.text(
            egui::pos2(rect.left() + 8.0, centre_y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(55),
        );
        painter.line_segment(
            [
                egui::pos2(lane.left(), centre_y),
                egui::pos2(lane.right(), centre_y),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_gray(165)),
        );
        let count = order.len().max(1) as f32;
        let slot_width = lane.width() / count;
        for (rank, segment_index) in order.iter().copied().enumerate() {
            let segment = diagram.segments[segment_index];
            let (left, right) = if row_number == 0 {
                let scale = lane.width() / diagram.read_length as f32;
                (
                    lane.left() + segment.read_start as f32 * scale,
                    lane.left() + segment.read_end as f32 * scale,
                )
            } else {
                (
                    lane.left() + rank as f32 * slot_width + 2.0,
                    lane.left() + (rank + 1) as f32 * slot_width - 2.0,
                )
            };
            let block = egui::Rect::from_center_size(
                egui::pos2(
                    ((left + right) * 0.5).clamp(lane.left(), lane.right()),
                    centre_y,
                ),
                egui::vec2((right - left).abs().max(7.0), 20.0),
            )
            .intersect(lane.expand2(egui::vec2(0.0, 10.0)));
            let source = &chain.segments[segment_index];
            let fill = if source.reverse {
                egui::Color32::from_rgb(126, 87, 194)
            } else {
                egui::Color32::from_rgb(44, 119, 176)
            };
            painter.rect_filled(block, 2.0, fill);
            painter.rect_stroke(
                block,
                2.0,
                if segment_index == chain.active_segment {
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(230, 126, 34))
                } else {
                    egui::Stroke::new(0.75, egui::Color32::from_gray(55))
                },
                egui::StrokeKind::Outside,
            );
            let strand = if source.reverse { '−' } else { '+' };
            if block.width() >= 22.0 {
                painter.text(
                    block.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}{strand}", segment_index + 1),
                    egui::FontId::monospace(10.0),
                    egui::Color32::WHITE,
                );
            }
            let response = ui
                .interact(
                    block,
                    ui.make_persistent_id(("supplementary-chain", row_number, segment_index)),
                    egui::Sense::click(),
                )
                .on_hover_text(format!(
                    "Segment {} · {}:{}-{} {strand} · read {}-{} of {} bases",
                    segment_index + 1,
                    source.reference_name,
                    source.start + 1,
                    source.end,
                    segment.read_start + 1,
                    segment.read_end,
                    diagram.read_length,
                ));
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    true,
                    segment_index == chain.active_segment,
                    format!(
                        "Supplementary alignment segment {} on {} bases {} through {} strand {strand}; read bases {} through {} of {}",
                        segment_index + 1,
                        source.reference_name,
                        source.start + 1,
                        source.end,
                        segment.read_start + 1,
                        segment.read_end,
                        diagram.read_length,
                    ),
                )
            });
            if response.has_focus() {
                painter.rect_stroke(
                    block.expand(2.0),
                    2.0,
                    egui::Stroke::new(2.0, egui::Color32::BLACK),
                    egui::StrokeKind::Outside,
                );
            }
            if response.clicked() {
                clicked = Some(segment_index);
            }
        }
    }
    painter.text(
        egui::pos2(lane.left(), rect.bottom() - 5.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "{} inferred read bases · blue + strand · purple − strand · orange active",
            diagram.read_length
        ),
        egui::FontId::proportional(10.0),
        egui::Color32::from_gray(75),
    );
    clicked
}
