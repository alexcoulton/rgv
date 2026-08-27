use super::super::*;
use rgv_layout::{
    Direction, RectShape, breakend_bracket_dimensions, junction_arc_stroke_width,
    read_arrow_head_width,
};

pub(crate) fn cropped_display_list(display: &DisplayList, bounds: egui::Rect) -> DisplayList {
    let mut cropped = DisplayList::default();
    for rectangle in &display.rectangles {
        let left = rectangle.x.max(bounds.left());
        let top = rectangle.y.max(bounds.top());
        let right = (rectangle.x + rectangle.width).min(bounds.right());
        let bottom = (rectangle.y + rectangle.height).min(bounds.bottom());
        if right > left && bottom > top {
            let clipped = top > rectangle.y
                || bottom < rectangle.y + rectangle.height
                || left > rectangle.x
                || right < rectangle.x + rectangle.width;
            if clipped && rectangle.shape.is_junction_arc() {
                continue;
            }
            cropped.rectangles.push(RectPrimitive {
                x: left - bounds.left(),
                y: top - bounds.top(),
                width: right - left,
                height: bottom - top,
                color: rectangle.color,
                object_id: rectangle.object_id,
                shape: if top > rectangle.y
                    || bottom < rectangle.y + rectangle.height
                    || matches!(rectangle.shape.direction(), Some(Direction::Left))
                        && left > rectangle.x
                    || matches!(rectangle.shape.direction(), Some(Direction::Right))
                        && right < rectangle.x + rectangle.width
                {
                    RectShape::Rectangle
                } else {
                    rectangle.shape
                },
            });
        }
    }
    cropped.texts.extend(
        display
            .texts
            .iter()
            .filter(|text| {
                text.x >= bounds.left()
                    && text.x < bounds.right()
                    && text.y >= bounds.top()
                    && text.y < bounds.bottom()
            })
            .cloned()
            .map(|mut text| {
                text.x -= bounds.left();
                text.y -= bounds.top();
                text
            }),
    );
    cropped
}

pub(crate) fn automation_snapshot_metadata(
    assembly_name: &str,
    panels: &PanelWorkspace,
    alignment_tracks: &[AlignmentTrackController],
    tracks: &TrackCollection,
    presentation: &WorkspacePresentation,
    scope: &str,
) -> String {
    let mut metadata = format!(
        "RGV {} snapshot; scope {scope}; genome {}; ",
        env!("CARGO_PKG_VERSION"),
        sanitized_single_line(assembly_name, 4_096),
    );
    metadata.push_str("loci ");
    for (index, panel) in panels.iter().enumerate() {
        if index > 0 {
            metadata.push_str(", ");
        }
        let interval = panel.viewport.interval();
        let _ = write!(
            metadata,
            "{}:{}-{}",
            sanitized_single_line(&panel.contig_name, 4_096),
            interval.start().saturating_add(1),
            interval.end()
        );
    }
    metadata.push_str("; visible tracks ");
    let mut first = true;
    for (id, name) in alignment_tracks
        .iter()
        .filter(|track| {
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                .unwrap_or(false)
                || presentation
                    .track_content_exposed(ui::TrackPane::Reads, track.config.id)
                    .unwrap_or(false)
        })
        .map(|track| (track.config.id, track.config.name.as_str()))
        .chain(
            tracks
                .tracks()
                .iter()
                .filter(|track| {
                    presentation
                        .track_content_exposed(ui::TrackPane::Annotation, track.id)
                        .unwrap_or(false)
                })
                .map(|track| (track.id, track.name.as_str())),
        )
    {
        if !first {
            metadata.push_str(", ");
        }
        first = false;
        let _ = write!(
            metadata,
            "{}:{}",
            id.get(),
            sanitized_single_line(name, 4_096)
        );
    }
    metadata
}

pub(crate) fn display_list_svg_export(
    display: &DisplayList,
    width: u32,
    height: u32,
    metadata: &str,
) -> String {
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n<metadata>{}</metadata>\n<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n",
        xml_escape(metadata),
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
            let stroke_width = junction_arc_stroke_width(rectangle.height);
            let (start_y, control_y, end_y) = if rectangle.shape == RectShape::JunctionArcUp {
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
        } else if matches!(
            rectangle.shape,
            RectShape::BreakendJoinBefore | RectShape::BreakendJoinAfter
        ) {
            let (bar_width, stem_half_height) =
                breakend_bracket_dimensions(rectangle.width, rectangle.height);
            let bar_x = if rectangle.shape == RectShape::BreakendJoinBefore {
                rectangle.x
            } else {
                rectangle.x + rectangle.width - bar_width
            };
            let stem_y = rectangle.y + rectangle.height * 0.5 - stem_half_height;
            format!(
                "<rect x=\"{bar_x:.2}\" y=\"{:.2}\" width=\"{bar_width:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n<rect x=\"{:.2}\" y=\"{stem_y:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n",
                rectangle.y,
                rectangle.height,
                rectangle.x,
                rectangle.width,
                stem_half_height * 2.0,
            )
        } else if let Some(direction) = rectangle.shape.direction() {
            let left = rectangle.x;
            let right = rectangle.x + rectangle.width;
            let top = rectangle.y;
            let bottom = rectangle.y + rectangle.height;
            let middle = rectangle.y + rectangle.height * 0.5;
            let head_width = if matches!(
                rectangle.shape,
                RectShape::DirectionalTerminalLeft | RectShape::DirectionalTerminalRight
            ) {
                read_arrow_head_width(rectangle.width, rectangle.height)
            } else {
                rectangle.width
            };
            let points = match direction {
                Direction::Left => format!(
                    "{left:.2},{middle:.2} {:.2},{top:.2} {right:.2},{top:.2} {right:.2},{bottom:.2} {:.2},{bottom:.2}",
                    left + head_width,
                    left + head_width,
                ),
                Direction::Right => format!(
                    "{left:.2},{top:.2} {:.2},{top:.2} {right:.2},{middle:.2} {:.2},{bottom:.2} {left:.2},{bottom:.2}",
                    right - head_width,
                    right - head_width,
                ),
            };
            format!(
                "<polygon points=\"{points}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n"
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
            > MAX_AUTOMATION_EXPORT_BYTES
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
                > MAX_AUTOMATION_EXPORT_BYTES
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

pub(crate) fn automation_track_names_export(
    assembly_name: &str,
    panels: &PanelWorkspace,
    alignment_tracks: &[AlignmentTrackController],
    tracks: &TrackCollection,
    presentation: &WorkspacePresentation,
) -> String {
    let mut output = format!(
        "# RGV {} track-name export\n# genome\t{}\n# panel_count\t{}\ntrack_id\tkind\tname\tsample_key\tvisible\n",
        env!("CARGO_PKG_VERSION"),
        sanitized_tsv_field(assembly_name, 4_096),
        panels.len(),
    );
    for track in alignment_tracks {
        let _ = writeln!(
            output,
            "{}\talignment\t{}\t{}\t{}",
            track.config.id.get(),
            sanitized_tsv_field(&track.config.name, 4_096),
            track
                .config
                .sample_key
                .as_deref()
                .map_or_else(String::new, |value| sanitized_tsv_field(value, 4_096)),
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                .unwrap_or(false),
        );
    }
    for track in tracks.tracks() {
        let _ = writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}",
            track.id.get(),
            format!("{:?}", track.kind).to_ascii_lowercase(),
            sanitized_tsv_field(&track.name, 4_096),
            track
                .sample_key
                .as_deref()
                .map_or_else(String::new, |value| sanitized_tsv_field(value, 4_096)),
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.id)
                .unwrap_or(false),
        );
    }
    output
}

pub(crate) fn automation_export_header(
    kind: &str,
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
) -> String {
    format!(
        "# RGV {} {kind} export v1\n# genome\t{}\n# locus\t{}:{}-{}\n# coordinates\tstart is one-based; end is one-based inclusive; internal intervals are zero-based half-open\n# byte_limit\t{MAX_AUTOMATION_EXPORT_BYTES}\n",
        env!("CARGO_PKG_VERSION"),
        sanitized_tsv_field(assembly, 4_096),
        sanitized_tsv_field(contig, 4_096),
        interval.start().saturating_add(1),
        interval.end(),
    )
}

pub(crate) fn push_automation_export_line(output: &mut String, line: &str) -> bool {
    let required = line.len().saturating_add(1);
    if output.len().saturating_add(required) <= MAX_AUTOMATION_EXPORT_BYTES {
        output.push_str(line);
        output.push('\n');
        return true;
    }
    let marker = "# export_truncated\tbyte_limit_reached\n";
    if output.len().saturating_add(marker.len()) <= MAX_AUTOMATION_EXPORT_BYTES {
        output.push_str(marker);
    }
    false
}

pub(crate) fn automation_alignments_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> String {
    let mut output = automation_export_header("alignment", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tread_name\tflags\tmapq\tcigar\tmate_contig_id\tmate_start\ttemplate_length",
    ) {
        return output;
    }
    for track in tracks {
        let query = &track.query;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\trecords_seen={}\trecords_eligible={}\trecords_retained={}\tretained_truncated={}\tunmapped_ignored={}\tsecondary_filtered={}\tsupplementary_filtered={}\tuser_filtered={}\tduplicate_filtered={}\tqc_failed_filtered={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                query.stats.records_seen,
                query.stats.records_eligible,
                query.batch.len(),
                query.stats.truncated,
                query.stats.records_filtered_unmapped,
                query.stats.records_filtered_secondary,
                query.stats.records_filtered_supplementary,
                query.stats.records_filtered_user,
                query.stats.records_filtered_duplicate,
                query.stats.records_filtered_qc_failed,
            ),
        );
        for index in 0..query.batch.len() {
            let start = query.batch.starts[index];
            let end = query.batch.ends[index];
            if start >= interval.end() || end <= interval.start() {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t0x{:04x}\t{}\t{}\t{}\t{}\t{}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                start.saturating_add(1),
                end,
                sanitized_tsv_field(
                    &String::from_utf8_lossy(query.batch.read_name(index).unwrap_or_default()),
                    1_024,
                ),
                query.batch.flags[index].bits(),
                query.batch.mapping_qualities[index]
                    .map_or_else(|| ".".to_owned(), |value| value.to_string()),
                format_cigar(query.batch.cigar(index).unwrap_or_default()),
                query.batch.mate_contigs[index]
                    .map_or_else(|| ".".to_owned(), |value| value.get().to_string()),
                query.batch.mate_starts[index].map_or_else(
                    || ".".to_owned(),
                    |value| value.saturating_add(1).to_string()
                ),
                query.batch.template_lengths[index],
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    output
}

pub(crate) fn automation_pileup_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> String {
    let mut output = automation_export_header("pileup", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tdepth\tforward_depth\treverse_depth\tA\tC\tG\tT\tN\tinsertion\tdeletion\tmean_base_quality\tmean_mapping_quality",
    ) {
        return output;
    }
    for track in tracks {
        let query = &track.query;
        let provenance = query.pileup.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\teligible_records={}\trendered_records={}\tall_eligible_records={}\talleles_available={}\toverlap_collapse_complete={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                provenance.eligible_records,
                provenance.rendered_records,
                provenance.all_eligible_records,
                provenance.allele_observations_available,
                provenance.overlap_collapse_complete,
            ),
        );
        for (index, bin) in query.pileup.bins.iter().copied().enumerate() {
            let Some(bin_interval) = query.pileup.bin_interval(query.interval, index) else {
                continue;
            };
            if !bin_interval.overlaps(interval) {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                bin_interval.start().saturating_add(1),
                bin_interval.end(),
                bin.depth,
                bin.forward_depth,
                bin.reverse_depth,
                bin.alleles.a,
                bin.alleles.c,
                bin.alleles.g,
                bin.alleles.t,
                bin.alleles.n,
                bin.alleles.insertion,
                bin.alleles.deletion,
                bin.mean_base_quality()
                    .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
                bin.mean_mapping_quality()
                    .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    output
}

pub(crate) fn automation_modifications_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> String {
    let mut output = automation_export_header("modified-base", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tcanonical_base\tmodification_code\tmodification_strand\timplicit_mode\thaplotype\tmodified_calls\tcanonical_observations\tforward_modified_calls\treverse_modified_calls\traw_fraction\tweighted_fraction\tmean_probability\tmissing_probability_calls",
    ) {
        return output;
    }
    for track in tracks {
        let query = &track.query;
        let summary = &query.pileup.modifications;
        let provenance = summary.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\teligible_records={}\ttagged_records={}\tdecoded_calls={}\tmalformed_records={}\tunavailable_records={}\tdropped_series={}\tall_eligible_records={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                provenance.eligible_records,
                provenance.records_with_modification_tags,
                provenance.decoded_calls,
                provenance.malformed_records,
                provenance.unavailable_records,
                provenance.dropped_series,
                provenance.all_eligible_records,
            ),
        );
        for series in &summary.series {
            for (index, bin) in series.bins.iter().copied().enumerate() {
                if bin.canonical_observations == 0
                    && bin.modified_calls == 0
                    && bin.missing_probability_calls == 0
                {
                    continue;
                }
                let Some(bin_interval) = summary.bin_interval(query.interval, index) else {
                    continue;
                };
                if !bin_interval.overlaps(interval) {
                    continue;
                }
                let line = format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    track.id.get(),
                    sanitized_tsv_field(&track.name, 4_096),
                    sanitized_tsv_field(contig, 4_096),
                    bin_interval.start().saturating_add(1),
                    bin_interval.end(),
                    char::from(series.key.canonical_base),
                    modification_code_label(series.key.code),
                    series.key.modification_strand,
                    series.key.implicit_mode,
                    series
                        .key
                        .haplotype
                        .map_or_else(|| ".".to_owned(), |value| value.to_string()),
                    bin.modified_calls,
                    bin.canonical_observations,
                    bin.forward_modified_calls,
                    bin.reverse_modified_calls,
                    bin.raw_fraction()
                        .map_or_else(|| ".".to_owned(), |value| format!("{value:.6}")),
                    bin.weighted_fraction()
                        .map_or_else(|| ".".to_owned(), |value| format!("{value:.6}")),
                    bin.mean_probability()
                        .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
                    bin.missing_probability_calls,
                );
                if !push_automation_export_line(&mut output, &line) {
                    return output;
                }
            }
        }
    }
    output
}

pub(crate) fn automation_junctions_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> String {
    let mut output = automation_export_header("junction", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tdepth\tminimum_left_flank\tminimum_right_flank\tmaximum_left_flank\tmaximum_right_flank\tpositive_depth\tnegative_depth\tunknown_depth\ttagged_positive_depth\ttagged_negative_depth\tfallback_positive_depth\tfallback_negative_depth\tinvalid_strand_depth",
    ) {
        return output;
    }
    for track in tracks {
        let query = &track.query;
        let provenance = query.junctions.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\teligible_records={}\tspliced_records={}\tobservations_examined={}\tobservations_retained={}\tdropped_distinct={}\tall_eligible_records={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                provenance.eligible_records,
                provenance.spliced_records,
                provenance.junction_observations_examined,
                provenance.junction_observations_retained,
                provenance.dropped_distinct_junctions,
                provenance.all_eligible_records,
            ),
        );
        for junction in &query.junctions.junctions {
            if junction.start >= interval.end() || junction.end <= interval.start() {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                junction.start.saturating_add(1),
                junction.end,
                junction.depth,
                junction.minimum_left_flank,
                junction.minimum_right_flank,
                junction.maximum_left_flank,
                junction.maximum_right_flank,
                junction.positive_depth,
                junction.negative_depth,
                junction.unknown_depth,
                junction.tagged_positive_depth,
                junction.tagged_negative_depth,
                junction.fallback_positive_depth,
                junction.fallback_negative_depth,
                junction.invalid_strand_depth,
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    output
}

#[allow(clippy::too_many_lines)]
pub(crate) fn automation_variants_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    snapshots: &[TrackSnapshot],
) -> String {
    let mut output = automation_export_header("variant", assembly, contig, interval);
    for line in [
        "record\ttrack_id\ttrack_name\tcontig\tstart\tend\tid\tref\talt\tqual\tfilters\ttypes",
        "info\ttrack_id\trecord_ordinal\tkey\tvalue\tfield_truncated",
        "genotype\ttrack_id\trecord_ordinal\tsample\tgenotype\tgroup\tformat_fields",
    ] {
        if !push_automation_export_line(&mut output, line) {
            return output;
        }
    }
    for snapshot in snapshots {
        let TrackSnapshotData::Variants(query) = &snapshot.data else {
            continue;
        };
        let provenance = query.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\trecords_examined={}\trecords_filtered={}\trecords_truncated={}\ttotal_samples={}\tselected_samples={}\tgenotype_cells_omitted={}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                provenance.records_examined,
                provenance.records_filtered,
                provenance.records_truncated,
                provenance.total_samples,
                provenance.selected_samples,
                provenance.genotype_cells_omitted,
            ),
        );
        for (ordinal, record) in query.records.iter().enumerate() {
            if record.start >= interval.end() || record.end <= interval.start() {
                continue;
            }
            let filters = if record.filters.is_empty() {
                "PASS".to_owned()
            } else {
                record.filters.join(";")
            };
            let line = format!(
                "record\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                sanitized_tsv_field(&record.contig, 4_096),
                record.start.saturating_add(1),
                record.end,
                tsv_cell(record.id.as_deref().unwrap_or(".")),
                tsv_cell(&bounded_variant_allele(&record.reference)),
                tsv_cell(
                    &record
                        .alternates
                        .iter()
                        .map(|allele| bounded_variant_allele(allele))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                record
                    .quality
                    .map_or_else(|| ".".to_owned(), |value| value.to_string()),
                tsv_cell(&filters),
                tsv_cell(
                    &record
                        .variant_types
                        .iter()
                        .map(format_variant_type)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
            for field in &record.info {
                let value = bounded_variant_export_field(field);
                let line = format!(
                    "info\t{}\t{}\t{}\t{}\t{}",
                    snapshot.id.get(),
                    ordinal,
                    tsv_cell(&field.key),
                    tsv_cell(&value),
                    field.truncated || value.ends_with('…'),
                );
                if !push_automation_export_line(&mut output, &line) {
                    return output;
                }
            }
            for genotype in &record.genotypes {
                let sample = query
                    .header
                    .samples
                    .get(genotype.sample_index)
                    .map_or("?", String::as_str);
                let format_fields = genotype
                    .fields
                    .iter()
                    .map(|field| format!("{}={}", field.key, bounded_variant_export_field(field)))
                    .collect::<Vec<_>>()
                    .join(";");
                let line = format!(
                    "genotype\t{}\t{}\t{}\t{}\t{}\t{}",
                    snapshot.id.get(),
                    ordinal,
                    tsv_cell(sample),
                    tsv_cell(&format_genotype(genotype)),
                    variant_genotype_group_label(genotype),
                    tsv_cell(&format_fields),
                );
                if !push_automation_export_line(&mut output, &line) {
                    return output;
                }
            }
        }
    }
    output
}

pub(crate) fn automation_quantitative_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    snapshots: &[TrackSnapshot],
) -> String {
    let mut output = automation_export_header("quantitative-window", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tvalue",
    ) {
        return output;
    }
    for snapshot in snapshots {
        let TrackSnapshotData::Quantitative(query) = &snapshot.data else {
            continue;
        };
        let provenance = query.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\trecords_examined={}\trecords_retained={}\tnon_finite_records={}\trecords_truncated={}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                provenance.records_examined,
                provenance.records_retained,
                provenance.non_finite_records,
                provenance.records_truncated,
            ),
        );
        if let Some(rgv_hts::QuantitativeSourceMetadata::Tdf(metadata)) = &query.source_metadata {
            let _ = push_automation_export_line(
                &mut output,
                &format!(
                    "# tdf_source\t{}\tchannel={}\tchannel_index={}\tchannel_count={}\tdataset={}\tversion={}\tgenome={}\tcompressed={}",
                    snapshot.id.get(),
                    tsv_cell(&metadata.selected_track_name),
                    metadata.selected_track_index,
                    metadata.track_count,
                    tsv_cell(&metadata.dataset),
                    metadata.version,
                    tsv_cell(metadata.genome_id.as_deref().unwrap_or(".")),
                    metadata.compressed,
                ),
            );
        }
        for record in &query.records {
            if record.start >= interval.end() || record.end <= interval.start() {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                record.start.saturating_add(1),
                record.end,
                record.value,
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    output
}

pub(crate) fn automation_inspector_export(inspector: &InspectorSnapshot) -> String {
    let mut output = format!(
        "# RGV {} selected evidence export v1\n# byte_limit\t{MAX_AUTOMATION_EXPORT_BYTES}\nsection\tkey\tvalue\n",
        env!("CARGO_PKG_VERSION")
    );
    for (key, value) in &inspector.fields {
        if !push_automation_export_line(
            &mut output,
            &format!(
                "detail\t{}\t{}",
                sanitized_tsv_field(key, 4_096),
                sanitized_tsv_field(value, 64 * 1024)
            ),
        ) {
            return output;
        }
    }
    for (key, value) in &inspector.auxiliary {
        if !push_automation_export_line(
            &mut output,
            &format!(
                "tag\t{}\t{}",
                sanitized_tsv_field(key, 4_096),
                sanitized_tsv_field(value, 64 * 1024)
            ),
        ) {
            return output;
        }
    }
    for (key, value) in [
        ("cigar", inspector.cigar.as_deref()),
        ("event_sequence", inspector.event_sequence.as_deref()),
        (
            "read_sequence",
            inspector
                .read_sequence
                .as_ref()
                .map(|sequence| sequence.text.as_str()),
        ),
    ] {
        if let Some(value) = value
            && !push_automation_export_line(
                &mut output,
                &format!(
                    "evidence\t{key}\t{}",
                    sanitized_tsv_field(value, SEQUENCE_COPY_BASES)
                ),
            )
        {
            return output;
        }
    }
    output
}

pub(crate) fn sanitized_single_line(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = value.replace(['\r', '\n'], " ");
    truncate_string_to_bytes(&mut sanitized, maximum_bytes);
    sanitized
}

pub(crate) fn sanitized_tsv_field(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = value.replace(['\t', '\r', '\n'], " ");
    truncate_string_to_bytes(&mut sanitized, maximum_bytes);
    sanitized
}

pub(crate) fn create_output_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub(crate) fn atomic_write_text(path: &Path, contents: &str) -> std::result::Result<(), String> {
    create_output_parent(path).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(contents.as_bytes())
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}
