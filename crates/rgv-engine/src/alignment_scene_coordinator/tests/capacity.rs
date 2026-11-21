use super::*;

#[test]
fn foreground_low_water_reserves_the_measured_working_set_not_one_page() {
    let budget = 4 * 1024 * 1024 * 1024_usize;
    let measured_bundle = 115 * 1024 * 1024_usize;
    let one_page = 1024 * 1024_usize;
    let target =
        alignment_foreground_low_water_target(budget, 0, 900 * 1024 * 1024, measured_bundle);

    assert_eq!(budget.saturating_sub(target), measured_bundle);
    assert!(budget.saturating_sub(target) > one_page);
}

#[test]
fn foreground_low_water_never_crosses_the_committed_pin_floor() {
    assert_eq!(
        alignment_foreground_low_water_target(1_000, 100, 850, 400),
        850
    );
}

#[test]
fn unseen_island_inherits_the_recent_upper_envelope_not_the_last_sparse_sample() {
    let contig = ContigId::new(0);
    let dense = GenomicInterval::new(contig, 100, 200).unwrap();
    let sparse = GenomicInterval::new(contig, 300, 400).unwrap();
    let unseen = GenomicInterval::new(contig, 500, 600).unwrap();
    let history = VecDeque::from([
        ForegroundWorkingSetSample {
            interval: dense,
            bytes: 900,
        },
        ForegroundWorkingSetSample {
            interval: sparse,
            bytes: 100,
        },
    ]);

    assert_eq!(foreground_working_set_reserve(&history, dense), 900);
    assert_eq!(foreground_working_set_reserve(&history, sparse), 100);
    assert_eq!(foreground_working_set_reserve(&history, unseen), 900);
}
