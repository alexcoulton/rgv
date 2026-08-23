use super::{
    Event, IgvImportReportBuilder, IgvSessionImport, Reader, SessionAlignmentTrack,
    SessionDocument, SessionError, SessionLocus, TrackConfig, TrackId, TrackKind, TrackSource,
    classify_genomic_source,
};

impl SessionDocument {
    pub fn import_igv(xml: &str) -> Result<Self, SessionError> {
        Ok(Self::import_igv_with_report(xml)?.document)
    }

    #[allow(clippy::too_many_lines)]
    pub fn import_igv_with_report(xml: &str) -> Result<IgvSessionImport, SessionError> {
        #[derive(Default)]
        struct Resource {
            path: String,
            name: Option<String>,
        }

        #[derive(Default)]
        struct TrackState {
            id: Option<String>,
            name: Option<String>,
            visible: Option<bool>,
            height: Option<f32>,
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut locus = None;
        let mut genome = None;
        let mut resources = Vec::new();
        let mut track_states = Vec::new();
        let mut report = IgvImportReportBuilder::default();
        loop {
            match reader.read_event() {
                Ok(Event::Start(event) | Event::Empty(event)) => {
                    let element = String::from_utf8_lossy(event.name().as_ref()).into_owned();
                    let mut attributes = Vec::new();
                    for attribute in event.attributes().with_checks(false) {
                        let attribute =
                            attribute.map_err(|error| SessionError::Igv(error.to_string()))?;
                        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                        let value = attribute
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .map_err(|error| SessionError::Igv(error.to_string()))?
                            .into_owned();
                        attributes.push((key, value));
                    }
                    match element.as_str() {
                        "Session" | "Global" => {
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "locus" => {
                                        locus = Some(parse_igv_locus(&value)?);
                                        report.imported("Session.locus -> active RGV locus");
                                    }
                                    "genome" => genome = Some(value),
                                    "version" => report.imported(
                                        "Session.version accepted as IGV compatibility metadata",
                                    ),
                                    _ => report.ignored(format!(
                                        "{element}.{key} has no RGV project equivalent"
                                    )),
                                }
                            }
                        }
                        "Resources" => {
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "Resources.{key} has no RGV project equivalent"
                                ));
                            }
                        }
                        "Resource" => {
                            let mut resource = Resource::default();
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "path" => resource.path = value,
                                    "name" | "label" if resource.name.is_none() => {
                                        resource.name = Some(value);
                                    }
                                    "index" => report.ignored(
                                        "Resource.index is not persisted by the RGV project model",
                                    ),
                                    "url" => report.ignored(
                                        "Resource.url feature-link metadata is not imported",
                                    ),
                                    _ => report.ignored(format!(
                                        "Resource.{key} has no RGV project equivalent"
                                    )),
                                }
                            }
                            if resource.path.is_empty() {
                                report.ignored("Resource without a path was ignored");
                            } else {
                                resources.push(resource);
                            }
                        }
                        "Panel" => {
                            report.approximated(
                                "IGV panel placement was flattened into RGV's ordered track stack",
                            );
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "Panel.{key} is not preserved after panel flattening"
                                ));
                            }
                        }
                        "Track" => {
                            let mut state = TrackState::default();
                            for (key, value) in attributes {
                                match key.as_str() {
                                    "id" => state.id = Some(value),
                                    "name" | "displayName" if state.name.is_none() => {
                                        state.name = Some(value);
                                    }
                                    "visible" => match value.parse::<bool>() {
                                        Ok(value) => state.visible = Some(value),
                                        Err(_) => report.ignored(
                                            "Track.visible had an invalid Boolean value",
                                        ),
                                    },
                                    "height" => match value.parse::<f32>() {
                                        Ok(value) if value.is_finite() && value > 0.0 => {
                                            state.height = Some(value);
                                        }
                                        _ => report
                                            .ignored("Track.height had an invalid numeric value"),
                                    },
                                    "expand" => report.approximated(
                                        "Track.expand was not mapped because IGV and RGV density modes differ",
                                    ),
                                    _ => report.ignored(format!(
                                        "Track.{key} has no exact RGV project equivalent"
                                    )),
                                }
                            }
                            track_states.push(state);
                        }
                        "DataRange" => {
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "DataRange.{key} is not yet imported into RGV scales"
                                ));
                            }
                        }
                        _ => {
                            report.ignored(format!("IGV element <{element}> was ignored"));
                            for (key, _) in attributes {
                                report.ignored(format!(
                                    "{element}.{key} was ignored with its containing element"
                                ));
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(error) => return Err(SessionError::Igv(error.to_string())),
            }
        }
        let locus =
            locus.ok_or_else(|| SessionError::Igv("IGV session has no locus".to_owned()))?;
        let mut document = Self::new(locus);
        if let Some(genome) = genome {
            if infer_track_kind(&genome) == TrackKind::Reference {
                document.reference = Some(source_from_string(genome));
                report.imported("Session.genome FASTA -> RGV reference source");
            } else {
                document.genome_name = Some(genome);
                report.approximated(
                    "Session.genome identifier was retained as an assembly label; RGV did not infer a reference file",
                );
            }
        }
        let mut next_id = 1_u64;
        let resource_count = resources.len();
        for resource in resources {
            let source = source_from_string(resource.path.clone());
            let kind = infer_track_kind(&resource.path);
            let default_name = resource
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&resource.path)
                .to_owned();
            let name = resource.name.unwrap_or(default_name);
            if kind == TrackKind::Alignment {
                document.alignment_tracks.push(SessionAlignmentTrack::new(
                    TrackId::new(next_id),
                    name,
                    source,
                ));
                next_id = next_id.saturating_add(1);
            } else if kind == TrackKind::Reference && document.reference.is_none() {
                document.reference = Some(source);
            } else {
                document
                    .tracks
                    .push(TrackConfig::new(TrackId::new(next_id), name, kind, source));
                next_id = next_id.saturating_add(1);
            }
        }
        report.imported(format!(
            "{} Resource.path entr{} -> RGV data sources",
            resource_count,
            if resource_count == 1 { "y" } else { "ies" }
        ));
        for state in track_states {
            let Some(id) = state.id.as_deref() else {
                report.ignored("Track without an id could not be associated with a Resource");
                continue;
            };
            let derived_alignment = [
                ("_coverage", true),
                ("_junctions", false),
                ("_splice_junctions", false),
            ]
            .into_iter()
            .find_map(|(suffix, coverage)| id.strip_suffix(suffix).map(|base| (base, coverage)));
            if let Some((base, coverage)) = derived_alignment
                && let Some(track) = document
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track_source_location(&track.source) == base)
            {
                if let Some(visible) = state.visible {
                    if coverage {
                        track.filters.show_coverage = visible;
                    } else {
                        track.filters.show_junctions = visible;
                    }
                }
                report.approximated(if coverage {
                    "IGV coverage subtrack visibility -> RGV alignment coverage visibility"
                } else {
                    "IGV junction subtrack visibility -> RGV alignment junction visibility"
                });
                if state.name.is_some() || state.height.is_some() {
                    report.ignored(
                        "Derived IGV coverage/junction name and height were not applied independently",
                    );
                }
                continue;
            }
            if let Some(track) = document
                .alignment_tracks
                .iter_mut()
                .find(|track| track_source_location(&track.source) == id)
            {
                apply_igv_common_track_state(
                    &mut track.name,
                    &mut track.visible,
                    &mut track.height,
                    state.name,
                    state.visible,
                    state.height,
                    &mut report,
                );
                continue;
            }
            if let Some(track) = document
                .tracks
                .iter_mut()
                .find(|track| track_source_location(&track.source) == id)
            {
                apply_igv_common_track_state(
                    &mut track.name,
                    &mut track.visible,
                    &mut track.height,
                    state.name,
                    state.visible,
                    state.height,
                    &mut report,
                );
                continue;
            }
            report.ignored("Track configuration did not match a Resource and was ignored");
        }
        document.validate()?;
        Ok(IgvSessionImport {
            document,
            report: report.finish(),
        })
    }
}

fn track_source_location(source: &TrackSource) -> &str {
    match source {
        TrackSource::Local { path } => path,
        TrackSource::Remote { url } => url,
    }
}

fn apply_igv_common_track_state(
    target_name: &mut String,
    target_visible: &mut bool,
    target_height: &mut f32,
    name: Option<String>,
    visible: Option<bool>,
    height: Option<f32>,
    report: &mut IgvImportReportBuilder,
) {
    if let Some(name) = name {
        if name.trim().is_empty() {
            report.ignored("Track.name was blank and the Resource name was retained");
        } else {
            *target_name = name;
            report.imported("Track.name -> RGV track name");
        }
    }
    if let Some(visible) = visible {
        *target_visible = visible;
        report.imported("Track.visible -> RGV track visibility");
    }
    if let Some(height) = height {
        let bounded = height.clamp(26.0, 4_096.0);
        *target_height = bounded;
        if (bounded - height).abs() > f32::EPSILON {
            report.approximated("Track.height was clamped to RGV's safe track-height range");
        } else {
            report.imported("Track.height -> RGV track height");
        }
    }
}

fn parse_igv_locus(raw: &str) -> Result<SessionLocus, SessionError> {
    let (contig, range) = raw
        .split_once(':')
        .ok_or_else(|| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let range = range.replace(',', "");
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let start = start
        .parse::<u64>()
        .map_err(|_| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| SessionError::Igv(format!("invalid IGV locus {raw}")))?;
    if start == 0 || start > end {
        return Err(SessionError::Igv(format!("invalid IGV locus {raw}")));
    }
    Ok(SessionLocus {
        contig: contig.to_owned(),
        start: start - 1,
        end,
    })
}

fn source_from_string(value: String) -> TrackSource {
    if value.starts_with("http://") || value.starts_with("https://") {
        TrackSource::Remote { url: value }
    } else {
        TrackSource::Local { path: value }
    }
}

pub(super) fn infer_track_kind(path: &str) -> TrackKind {
    classify_genomic_source(path).map_or(TrackKind::Feature, crate::ClassifiedGenomicSource::kind)
}
