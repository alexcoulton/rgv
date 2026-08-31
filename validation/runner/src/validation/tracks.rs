#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(crate) fn verify_mixed_track_truth(root: &Path, manifest: &FixtureManifest) -> Result<()> {
    let document = project_document(root, manifest, "mf-10-mixed-tracks")?;
    ensure!(
        document.tracks.iter().any(|track| {
            track.quantitative.arithmetic == QuantitativeArithmetic::Subtract
                && track.quantitative.operand_track.is_some()
        }) && document
            .tracks
            .iter()
            .filter(|track| track.quantitative.overlay_group == Some(1))
            .count()
            >= 2,
        "MF-10 lost its arithmetic track or two-track overlay group"
    );
    let specs = document
        .tracks
        .iter()
        .map(|track| {
            let TrackSource::Local { path } = &track.source else {
                anyhow::bail!("MF-10 unexpectedly contains a remote source");
            };
            Ok(TrackSpec {
                id: track.id,
                name: track.name.clone(),
                path: PathBuf::from(path),
                kind: fixture_track_spec_kind(track.kind, track.format, Path::new(path))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let engine = TrackEngine::spawn(specs.clone(), 128);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ready = None;
    let mut errors = Vec::new();
    let mut tdf_channels = None;
    while Instant::now() < deadline && ready.is_none() {
        match engine.try_receive()? {
            Some(TrackEngineEvent::Ready { opened, failed }) => ready = Some((opened, failed)),
            Some(TrackEngineEvent::TdfChannelsDiscovered { track_names, .. }) => {
                tdf_channels = Some(track_names);
            }
            Some(TrackEngineEvent::Error { message, .. }) => errors.push(message),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    let (opened, failed) = ready.context("MF-10 timed out while opening its production sources")?;
    ensure!(
        errors.is_empty(),
        "MF-10 source errors: {}",
        errors.join("; ")
    );
    ensure!(
        opened == specs.len() && failed == 0,
        "MF-10 opened {opened} sources with {failed} failures instead of {} healthy sources",
        specs.len()
    );
    ensure!(
        tdf_channels.as_deref().is_some_and(|names| {
            names
                == [
                    "archive sample one".to_owned(),
                    "archive sample two".to_owned(),
                ]
        }),
        "MF-10 did not discover both exact TDF channel names"
    );

    let clock = GenerationClock::default();
    let request = PanelGeneration::new(PanelId::new(1), clock.current());
    engine.try_query(
        request,
        clock,
        "chrVariants".to_owned(),
        GenomicInterval::new(rgv_core::ContigId::new(0), 8_500, 18_100)?,
    )?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lane_snapshots = Vec::new();
    let snapshots = loop {
        ensure!(
            Instant::now() < deadline,
            "MF-10 timed out querying its sources"
        );
        match engine.try_receive()? {
            Some(TrackEngineEvent::LaneCompleted { track, .. }) => lane_snapshots.push(track),
            Some(TrackEngineEvent::QueryCompleted { completed, .. }) => {
                lane_snapshots.retain(|track| completed.contains(&track.id));
                break lane_snapshots;
            }
            Some(TrackEngineEvent::Error { message, .. }) => {
                anyhow::bail!("MF-10 query failed: {message}");
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    ensure!(
        snapshots.len() == specs.len(),
        "MF-10 returned {} snapshots for {} sources",
        snapshots.len(),
        specs.len()
    );
    for snapshot in snapshots {
        verify_mixed_snapshot_exact(&snapshot)?;
        let has_data = match snapshot.data {
            TrackSnapshotData::Features(records) => !records.is_empty(),
            TrackSnapshotData::Variants(query) => !query.records.is_empty(),
            TrackSnapshotData::Quantitative(query) => !query.records.is_empty(),
            TrackSnapshotData::CopyNumber(query) => !query.segments.is_empty(),
            TrackSnapshotData::Mutations(query) => !query.records.is_empty(),
            TrackSnapshotData::Interactions(query) => !query.records.is_empty(),
            TrackSnapshotData::Gwas(query) => !query.records.is_empty(),
        };
        ensure!(
            has_data,
            "MF-10 source {} returned no expected evidence",
            snapshot.name
        );
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn verify_mixed_snapshot_exact(snapshot: &rgv_engine::TrackSnapshot) -> Result<()> {
    let feature_truth = match snapshot.name.as_str() {
        "Features (gzip BED)" => Some(("review-region", 8_800, 9_600)),
        "Genes (gzip GTF)" => Some(("Demo gene 1", 9_000, 10_100)),
        "Transcripts" => Some(("DEMO2-T1", 10_500, 12_000)),
        "Narrow peaks" => Some(("peak-with-summit", 9_200, 9_500)),
        "Broad peaks" => Some(("broad-region", 10_800, 11_400)),
        "PSL target alignments" => Some(("query-demo", 9_800, 9_890)),
        "Generic BigBed" => Some(("indexed-feature", 8_700, 9_000)),
        "Indexed transcripts" => Some(("DEMO-BIG-alt", 9_000, 10_200)),
        "Indexed peaks" => Some(("indexed-peak", 10_600, 11_100)),
        "Plain BED annotations" => Some(("EXPECTED_SEARCH_FEATURE", 8_500, 9_200)),
        "Plain GFF3 gene models" => Some(("Expected gene", 14_500, 17_500)),
        "Compressed GFF3 gene models" => Some(("Expected compressed gene", 14_500, 17_500)),
        "Plain GTF transcripts" => Some(("Expected GTF gene", 14_500, 17_500)),
        "refFlat transcripts" => Some(("EXPECTED_REF_FLAT", 14_500, 17_500)),
        _ => None,
    };
    if let Some((name, start, end)) = feature_truth {
        let TrackSnapshotData::Features(records) = &snapshot.data else {
            anyhow::bail!("MF-10 {} did not decode as features", snapshot.name);
        };
        ensure!(
            records.iter().any(|record| {
                record.name.as_deref() == Some(name) && record.start == start && record.end == end
            }),
            "MF-10 {} lost exact feature {name} at {start}-{end}",
            snapshot.name
        );
        return Ok(());
    }

    let quantitative_truth = match snapshot.name.as_str() {
        "Signal BigWig" | "Arithmetic signal difference" => Some((8_500, 9_300, 2.5)),
        "Archived TDF" => Some((9_000, 9_500, 1.0)),
        "Signal WIG" => Some((8_800, 8_900, 1.0)),
        "Fixed-step WIG" => Some((14_500, 14_600, 1.0)),
        _ => None,
    };
    if let Some((start, end, value)) = quantitative_truth {
        let TrackSnapshotData::Quantitative(query) = &snapshot.data else {
            anyhow::bail!("MF-10 {} did not decode as quantitative", snapshot.name);
        };
        ensure!(
            query.records.iter().any(|record| {
                record.start == start
                    && record.end == end
                    && (record.value - value).abs() <= f32::EPSILON
            }),
            "MF-10 {} lost exact signal {value} at {start}-{end}",
            snapshot.name
        );
        return Ok(());
    }

    match (&*snapshot.name, &snapshot.data) {
        ("Association study", TrackSnapshotData::Gwas(query)) => ensure!(
            query.records.iter().any(|record| {
                record.identifier == "rs-demo-1"
                    && record.position == 9_099
                    && (record.p_value - 1e-8).abs() <= 1e-16
            }),
            "MF-10 lost exact demo association"
        ),
        (
            "PLINK linear association"
            | "PLINK logistic association"
            | "PLINK association"
            | "PLINK quantitative association",
            TrackSnapshotData::Gwas(query),
        ) => ensure!(
            query.records.iter().any(|record| {
                record.identifier == "EXPECTED_ASSOC_1"
                    && record.position == 15_049
                    && (record.p_value - 1e-12).abs() <= 1e-20
            }),
            "MF-10 {} lost exact PLINK association",
            snapshot.name
        ),
        ("Variants and genotypes", TrackSnapshotData::Variants(query)) => ensure!(
            query.header.samples == ["tumour", "normal"]
                && query.records.iter().any(|record| {
                    record.id.as_deref() == Some("demo-snv")
                        && record.start == 9_949
                        && record.quality == Some(60.0)
                }),
            "MF-10 lost exact demo variant/genotype truth"
        ),
        ("BEDPE loops", TrackSnapshotData::Interactions(query)) => ensure!(
            query.records.iter().any(|record| {
                record.name.as_deref() == Some("loop-A")
                    && record.source.start == 9_000
                    && record.target.start == 11_200
            }),
            "MF-10 lost exact BEDPE loop"
        ),
        ("UCSC interactions", TrackSnapshotData::Interactions(query)) => ensure!(
            query.records.iter().any(|record| {
                record.name.as_deref() == Some("variant/gene/demo")
                    && record.value == Some(0.625)
                    && record.directional
            }),
            "MF-10 lost exact interact record"
        ),
        ("Copy number", TrackSnapshotData::CopyNumber(query)) => ensure!(
            query.samples == ["tumour", "normal"]
                && query.segments.iter().any(|segment| {
                    segment.sample_index == 0
                        && segment.start == 8_500
                        && segment.end == 10_000
                        && (segment.value - 0.85).abs() <= f32::EPSILON
                }),
            "MF-10 lost exact SEG record"
        ),
        ("CBS copy number", TrackSnapshotData::CopyNumber(query)) => ensure!(
            query.samples == ["child", "father"]
                && query.segments.iter().any(|segment| {
                    segment.sample_index == 0
                        && segment.start == 14_500
                        && segment.end == 16_000
                        && (segment.value - 0.75).abs() <= f32::EPSILON
                }),
            "MF-10 lost exact CBS record"
        ),
        ("Somatic mutations", TrackSnapshotData::Mutations(query)) => ensure!(
            query.samples == ["tumour"]
                && query.records.iter().any(|record| {
                    record.gene == "DEMO1"
                        && record.start == 9_950
                        && record.classification == "Missense_Mutation"
                }),
            "MF-10 lost exact MAF record"
        ),
        _ => anyhow::bail!(
            "MF-10 has no exact semantic oracle for track {} ({:?})",
            snapshot.name,
            snapshot.data
        ),
    }
    Ok(())
}

pub(crate) fn fixture_track_spec_kind(
    kind: TrackKind,
    format: Option<TrackFormat>,
    path: &Path,
) -> Result<TrackSpecKind> {
    if let Some(format) = format {
        return Ok(match format {
            TrackFormat::BigGenePred => TrackSpecKind::BigGenePred,
            TrackFormat::BigNarrowPeak => TrackSpecKind::BigNarrowPeak,
            TrackFormat::TdfChannel { index } => TrackSpecKind::TdfChannel { index },
        });
    }
    let filename = path
        .file_name()
        .context("manual track source has no filename")?
        .to_string_lossy()
        .to_ascii_lowercase();
    let uncompressed = filename
        .strip_suffix(".gz")
        .or_else(|| filename.strip_suffix(".bgz"))
        .unwrap_or(&filename);
    let extension = Path::new(uncompressed)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    Ok(match kind {
        TrackKind::Feature => match extension {
            "bignarrowpeak" => TrackSpecKind::BigNarrowPeak,
            "bb" | "bigbed" => TrackSpecKind::BigBed,
            "narrowpeak" => TrackSpecKind::NarrowPeak,
            "broadpeak" => TrackSpecKind::BroadPeak,
            "psl" => TrackSpecKind::Psl,
            _ => TrackSpecKind::Bed,
        },
        TrackKind::GeneModel => match extension {
            "biggenepred" | "bb" | "bigbed" => TrackSpecKind::BigGenePred,
            "gtf" => TrackSpecKind::Gtf,
            "genepred" | "gp" => TrackSpecKind::GenePred,
            "refflat" => TrackSpecKind::RefFlat,
            _ => TrackSpecKind::Gff3,
        },
        TrackKind::Variant => TrackSpecKind::VcfOrBcf,
        TrackKind::Mutation => TrackSpecKind::MutationMaf,
        TrackKind::Gwas => TrackSpecKind::Gwas,
        TrackKind::Quantitative => match extension {
            "bedgraph" | "bdg" => TrackSpecKind::BedGraph,
            "wig" | "wiggle" => TrackSpecKind::Wig,
            "tdf" => TrackSpecKind::Tdf,
            _ => TrackSpecKind::BigWig,
        },
        TrackKind::CopyNumber => TrackSpecKind::Seg,
        TrackKind::Interaction => {
            if extension == "interact" {
                TrackSpecKind::Interact
            } else {
                TrackSpecKind::BedPe
            }
        }
        TrackKind::Alignment | TrackKind::Reference => {
            anyhow::bail!("manual auxiliary project contains a non-auxiliary track kind")
        }
    })
}
