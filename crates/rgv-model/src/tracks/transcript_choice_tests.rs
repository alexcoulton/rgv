use super::*;

fn transcript(id: &str, gene: &str, start: u64, end: u64, tags: &[&str]) -> FeatureRecord {
    let mut attributes = vec![
        ("gene_id".to_owned(), gene.to_owned()),
        ("transcript_id".to_owned(), id.to_owned()),
        ("gene_name".to_owned(), format!("{gene}-name")),
    ];
    attributes.extend(tags.iter().map(|tag| ("tag".to_owned(), (*tag).to_owned())));
    FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(format!("{gene}-name")),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(start, end)],
        coding_interval: None,
        peak: None,
        attributes,
    }
}

/// The representative of the single gene in `records`, and the reason it was chosen.
fn chosen(mut records: Vec<FeatureRecord>) -> (String, RepresentativeReason) {
    annotate_transcript_choices(&mut records);
    let representatives = records
        .iter()
        .filter(|record| record.transcript_choice().unwrap().is_representative())
        .collect::<Vec<_>>();
    assert_eq!(representatives.len(), 1, "exactly one representative");
    let choice = representatives[0].transcript_choice().unwrap();
    assert_eq!(
        Some(choice.representative),
        representatives[0].transcript_identifier()
    );
    (choice.representative.to_owned(), choice.reason)
}

#[test]
fn each_rule_separates_in_order() {
    // Each case: the winner holds the rule, the loser holds every later rule.
    let loser_everything_later = |start| {
        let mut record = transcript("T9", "G", start, start + 5_000, &["CCDS", "basic"]);
        record.coding_interval = Some((start, start + 5_000));
        record
    };
    let cases: [(&[&str], RepresentativeReason); 5] = [
        (&["MANE_Select"], RepresentativeReason::ManeSelect),
        (&["Ensembl_canonical"], RepresentativeReason::Canonical),
        (
            &["appris_principal_2"],
            RepresentativeReason::AppRisPrincipal(2),
        ),
        (&["CCDS", "basic"], RepresentativeReason::Ccds),
        (&["basic"], RepresentativeReason::Basic),
    ];
    for (tags, reason) in cases {
        let winner = transcript("T1", "G", 100, 200, tags);
        let mut loser = loser_everything_later(100);
        if reason == RepresentativeReason::Ccds {
            loser.attributes.retain(|(_, value)| value != "CCDS");
        }
        if reason == RepresentativeReason::Basic {
            loser
                .attributes
                .retain(|(_, value)| value != "CCDS" && value != "basic");
        }
        assert_eq!(chosen(vec![loser, winner]), ("T1".to_owned(), reason));
    }

    // Space-separated MANE and RefSeq Select, as RefSeq GFF3 writes them.
    let refseq_mane = transcript("NM_2", "G", 100, 200, &["MANE Select"]);
    let other = transcript("NM_1", "G", 100, 900, &["RefSeq Select"]);
    assert_eq!(
        chosen(vec![other.clone(), refseq_mane]),
        ("NM_2".to_owned(), RepresentativeReason::ManeSelect)
    );
    let plain = transcript("NM_0", "G", 100, 900, &[]);
    assert_eq!(
        chosen(vec![plain, other]),
        ("NM_1".to_owned(), RepresentativeReason::Canonical)
    );

    // APPRIS: the lower level wins.
    let first = transcript("T5", "G", 100, 200, &["appris_principal_1"]);
    let third = transcript("T1", "G", 100, 900, &["appris_principal_3"]);
    assert_eq!(
        chosen(vec![third, first]),
        ("T5".to_owned(), RepresentativeReason::AppRisPrincipal(1))
    );

    // CCDS from a `ccdsid` attribute.
    let mut with_ccdsid = transcript("T2", "G", 100, 200, &[]);
    with_ccdsid
        .attributes
        .push(("ccdsid".to_owned(), "CCDS1.1".to_owned()));
    assert_eq!(
        chosen(vec![transcript("T1", "G", 100, 900, &[]), with_ccdsid]),
        ("T2".to_owned(), RepresentativeReason::Ccds)
    );

    // Longest CDS beats a longer transcript; then longest spliced; then smallest ID.
    let mut short_long_cds = transcript("T2", "G", 100, 400, &[]);
    short_long_cds.blocks = vec![(100, 200), (300, 400)];
    short_long_cds.coding_interval = Some((100, 400));
    let mut long_short_cds = transcript("T1", "G", 100, 900, &[]);
    long_short_cds.coding_interval = Some((100, 150));
    assert_eq!(
        chosen(vec![long_short_cds, short_long_cds]),
        ("T2".to_owned(), RepresentativeReason::LongestCoding)
    );
    let mut spliced_short = transcript("T1", "G", 100, 900, &[]);
    spliced_short.blocks = vec![(100, 200), (800, 900)];
    let spliced_long = transcript("T2", "G", 100, 500, &[]);
    assert_eq!(
        chosen(vec![spliced_short, spliced_long]),
        ("T2".to_owned(), RepresentativeReason::LongestSpliced)
    );
    assert_eq!(
        chosen(vec![
            transcript("ENST0002", "G", 100, 200, &[]),
            transcript("ENST0001", "G", 300, 400, &[]),
        ]),
        (
            "ENST0001".to_owned(),
            RepresentativeReason::SmallestIdentifier
        )
    );
}

#[test]
fn the_choice_is_stable_across_input_order_and_partial_sets() {
    let gene = vec![
        transcript("T4", "G", 100, 900, &["basic"]),
        transcript("T1", "G", 150, 700, &["basic", "appris_principal_4"]),
        transcript(
            "T3",
            "G",
            200,
            800,
            &["basic", "appris_principal_1", "CCDS"],
        ),
        transcript("T2", "G", 120, 600, &[]),
        transcript("T5", "G", 400, 950, &["basic", "appris_principal_1"]),
    ];
    let expected = chosen(gene.clone());
    assert_eq!(
        expected,
        ("T3".to_owned(), RepresentativeReason::Ccds),
        "T3 and T5 tie on APPRIS; CCDS separates them"
    );
    // Every rotation and the reverse order.
    for rotation in 0..gene.len() {
        let mut order = gene.clone();
        order.rotate_left(rotation);
        assert_eq!(chosen(order.clone()), expected);
        order.reverse();
        assert_eq!(chosen(order), expected);
    }
    // Any partial set that holds the representative picks it again, and ranks agree.
    let mut full = gene.clone();
    annotate_transcript_choices(&mut full);
    let rank_of = |records: &[FeatureRecord], id: &str| {
        records
            .iter()
            .find(|record| record.transcript_identifier() == Some(id))
            .and_then(FeatureRecord::transcript_choice)
            .map(|choice| choice.rank)
    };
    for mask in 1_u32..(1 << gene.len()) {
        let subset = gene
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, record)| record.clone())
            .collect::<Vec<_>>();
        if subset.len() < 2
            || !subset
                .iter()
                .any(|record| record.transcript_identifier() == Some("T3"))
        {
            continue;
        }
        let mut annotated = subset.clone();
        annotate_transcript_choices(&mut annotated);
        assert_eq!(rank_of(&annotated, "T3"), Some(0), "mask {mask:b}");
        // Relative order of every pair matches the full set's.
        for left in &annotated {
            for right in &annotated {
                let (left_id, right_id) = (
                    left.transcript_identifier().unwrap(),
                    right.transcript_identifier().unwrap(),
                );
                let subset_order = rank_of(&annotated, left_id) < rank_of(&annotated, right_id);
                let full_order = rank_of(&full, left_id) < rank_of(&full, right_id);
                assert_eq!(
                    subset_order, full_order,
                    "{left_id} {right_id} mask {mask:b}"
                );
            }
        }
    }
    // Annotating twice changes nothing.
    let mut twice = full.clone();
    annotate_transcript_choices(&mut twice);
    assert_eq!(twice, full);
}

#[test]
fn genes_group_by_id_else_by_name_strand_and_proximity() {
    let mut records = vec![
        transcript("A1", "GA", 100, 200, &[]),
        transcript("A2", "GA", 150, 300, &["basic"]),
        // Alone in its gene: no choice is written.
        transcript("B1", "GB", 100, 300, &[]),
    ];
    // Without gene_id: same name and strand nearby are one gene; far away, or on the other
    // strand, another.
    let by_name = |id: &str, start: u64, strand: Strand| {
        let mut record = transcript(id, "unused", start, start + 100, &[]);
        record.attributes.retain(|(key, _)| key != "gene_id");
        record.attributes[1].1 = "SHARED".to_owned();
        record.strand = strand;
        record
    };
    records.push(by_name("N1", 10_000, Strand::Forward));
    records.push(by_name("N2", 10_050, Strand::Forward));
    records.push(by_name(
        "N3",
        10_000 + GENE_GROUP_MAX_GAP + 1_000,
        Strand::Forward,
    ));
    records.push(by_name("N4", 10_020, Strand::Reverse));
    annotate_transcript_choices(&mut records);
    let choice = |id: &str| {
        records
            .iter()
            .find(|record| record.transcript_identifier() == Some(id))
            .unwrap()
            .transcript_choice()
            .map(|choice| (choice.rank, choice.count, choice.gene_key.to_owned()))
    };
    assert_eq!(choice("A2"), Some((0, 2, "GA".to_owned())));
    assert_eq!(choice("A1"), Some((1, 2, "GA".to_owned())));
    assert_eq!(choice("B1"), None);
    assert_eq!(choice("N1"), Some((0, 2, "SHARED@chr1+".to_owned())));
    assert_eq!(choice("N2"), Some((1, 2, "SHARED@chr1+".to_owned())));
    assert_eq!(choice("N3"), None);
    assert_eq!(choice("N4"), None);
}

#[test]
fn group_extent_widens_to_the_genes_in_the_window() {
    let records = vec![
        transcript("A1", "GA", 100, 200, &[]),
        transcript("A2", "GA", 50, 1_000, &[]),
        // A gene read beyond the window that has no transcript inside it does not widen it.
        transcript("C1", "GC", 5_000, 9_000, &[]),
    ];
    assert_eq!(
        transcript_group_read_extent(&records, 150, 160, 10_000),
        (50, 1_000)
    );
    assert_eq!(
        transcript_group_read_extent(&records, 150, 160, 20),
        (130, 180)
    );
}

#[test]
fn protein_coding_uses_gene_then_transcript_biotype_then_the_coding_interval() {
    let mut record = transcript("T1", "G", 100, 200, &[]);
    assert!(!record.is_protein_coding());
    record.coding_interval = Some((120, 180));
    assert!(record.is_protein_coding());
    record
        .attributes
        .push(("transcript_type".to_owned(), "retained_intron".to_owned()));
    assert!(!record.is_protein_coding());
    record
        .attributes
        .push(("gene_type".to_owned(), "protein_coding".to_owned()));
    assert!(record.is_protein_coding());
}
