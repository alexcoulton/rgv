use rgv_core::{ContigId, GenomicInterval, Viewport};
use rgv_model::{Strand, annotate_transcript_choices};

use super::*;
use crate::{DisplayList, TrackLayoutOptions, layout_features, layout_presented_features};

fn viewport() -> Viewport {
    Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap(),
        1_000.0,
    )
    .unwrap()
}

fn transcript(gene: &str, id: &str, start: u64, end: u64, tags: &[&str]) -> FeatureRecord {
    let mut attributes = vec![
        ("gene_id".to_owned(), format!("{gene}_ID")),
        ("transcript_id".to_owned(), id.to_owned()),
        ("gene_name".to_owned(), gene.to_owned()),
        ("transcript_name".to_owned(), format!("{gene}-{id}")),
        ("gene_type".to_owned(), "protein_coding".to_owned()),
    ];
    attributes.extend(tags.iter().map(|tag| ("tag".to_owned(), (*tag).to_owned())));
    FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(gene.to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(start, end)],
        coding_interval: Some((start, end)),
        peak: None,
        attributes,
    }
}

/// Two overlapping genes: CLASP2 with twelve transcripts (one MANE), and LNC, a non-coding
/// gene with two.
fn genes() -> Vec<FeatureRecord> {
    let mut records = (0..12)
        .map(|index| {
            let tags: &[&str] = if index == 5 { &["MANE_Select"] } else { &[] };
            transcript(
                "CLASP2",
                &format!("T{index:02}"),
                1_000 + index * 10,
                6_000,
                tags,
            )
        })
        .collect::<Vec<_>>();
    for (id, start) in [("L1", 2_000), ("L2", 2_500)] {
        let mut record = transcript("LNC", id, start, 4_000, &[]);
        record.attributes[4].1 = "lncRNA".to_owned();
        record.coding_interval = None;
        records.push(record);
    }
    annotate_transcript_choices(&mut records);
    records
}

fn options() -> TrackLayoutOptions {
    TrackLayoutOptions {
        maximum_rows: 20,
        height: 20.0 * 20.0,
        ..TrackLayoutOptions::default()
    }
}

/// Distinct rows holding a drawn record: the top of each record's primitives.
fn drawn_rows(display: &DisplayList) -> usize {
    let mut tops = std::collections::BTreeMap::<u32, f32>::new();
    for rectangle in display
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.object_id != 0)
    {
        let top = tops.entry(rectangle.object_id).or_insert(rectangle.y);
        *top = top.min(rectangle.y);
    }
    tops.values()
        .map(|top| top.to_bits())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn labels(display: &DisplayList) -> Vec<&str> {
    display
        .texts
        .iter()
        .map(|text| text.text.as_str())
        .collect()
}

#[test]
fn one_per_gene_draws_each_representative_with_a_count_and_uses_fewer_rows() {
    let records = genes();
    let settings = FeatureTrackSettings::default();
    assert_eq!(settings.transcripts, TranscriptDisplay::OnePerGene);
    let presentation = FeaturePresentation::gene_model(&records, &settings);
    assert_eq!(presentation.shown(), 2);
    let (one, stats) =
        layout_presented_features(&records, viewport(), options(), None, Some(&presentation));
    assert_eq!(stats.rendered_records, 2);
    let mut drawn = labels(&one);
    drawn.sort_unstable();
    assert_eq!(drawn, ["CLASP2 +11", "LNC +1"]);
    // The MANE transcript stands for CLASP2.
    let mane = records
        .iter()
        .position(|record| record.transcript_identifier() == Some("T05"))
        .unwrap();
    assert!(!presentation.is_hidden(mane));
    assert_eq!(presentation.label(mane), Some("CLASP2 +11"));

    let all = FeatureTrackSettings {
        transcripts: TranscriptDisplay::All,
        ..FeatureTrackSettings::default()
    };
    let (every, stats) = layout_presented_features(
        &records,
        viewport(),
        options(),
        None,
        Some(&FeaturePresentation::gene_model(&records, &all)),
    );
    assert_eq!(stats.rendered_records, 14);
    assert_eq!(drawn_rows(&one), 2);
    assert_eq!(drawn_rows(&every), 14);
    // All is exactly the unpresented layout.
    assert_eq!(every, layout_features(&records, viewport(), options()).0);
    // Labels are bounded with their count kept.
    assert_eq!(
        representative_label(&"X".repeat(40), 123),
        format!("{} +123", "X".repeat(27))
    );
}

#[test]
fn an_expanded_gene_draws_every_transcript_and_the_others_stay_one_per_gene() {
    let records = genes();
    let mut settings = FeatureTrackSettings::default();
    settings.expanded_genes.insert("CLASP2_ID".to_owned());
    let presentation = FeaturePresentation::gene_model(&records, &settings);
    assert_eq!(presentation.shown(), 13);
    let (display, _) =
        layout_presented_features(&records, viewport(), options(), None, Some(&presentation));
    let drawn = labels(&display);
    assert!(drawn.contains(&"LNC +1"));
    assert!(drawn.contains(&"CLASP2-T05"));
    assert!(!drawn.iter().any(|label| label.starts_with("CLASP2 +")));
    assert_eq!(drawn_rows(&display), 13);
    // Collapsing it again restores one row for the gene.
    settings.expanded_genes.clear();
    assert_eq!(
        FeaturePresentation::gene_model(&records, &settings).shown(),
        2
    );
}

#[test]
fn the_protein_coding_filter_hides_other_genes_and_collapsed_mode_overlays_every_transcript() {
    let records = genes();
    let settings = FeatureTrackSettings {
        protein_coding_only: true,
        ..FeatureTrackSettings::default()
    };
    let presentation = FeaturePresentation::gene_model(&records, &settings);
    let (display, _) =
        layout_presented_features(&records, viewport(), options(), None, Some(&presentation));
    assert_eq!(labels(&display), ["CLASP2 +11"]);

    // Collapsed overlays every transcript of every drawn gene, as before; the filter still
    // applies.
    let collapsed = FeatureTrackSettings {
        display_mode: rgv_core::FeatureDisplayMode::Collapsed,
        ..settings
    };
    assert_eq!(
        FeaturePresentation::gene_model(&records, &collapsed).shown(),
        12
    );
}

#[test]
fn records_without_a_choice_are_drawn_as_they_are() {
    // A single-transcript gene and an untyped feature carry no choice.
    let mut records = vec![transcript("SOLO", "T1", 100, 900, &[])];
    annotate_transcript_choices(&mut records);
    let presentation = FeaturePresentation::gene_model(&records, &FeatureTrackSettings::default());
    assert!(!presentation.is_hidden(0));
    assert_eq!(presentation.label(0), None);
    assert_eq!(
        FeaturePresentation::label_text(Some(&presentation), &records, 0),
        "SOLO"
    );
}
