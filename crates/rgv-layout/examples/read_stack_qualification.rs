use std::{collections::HashMap, num::NonZeroUsize, path::PathBuf, time::Instant};

use rgv_core::{GenomicInterval, ReadStackingMode};
use rgv_hts::{
    AlignmentOpenOptions, AlignmentSource, IndividualReadStatus, LocalAlignmentSource, QueryDetail,
    QueryOptions,
};
use rgv_layout::{
    AlignmentLayoutOptions, AlignmentRowHistory, AlignmentRowIdentity, AlignmentRowLayout,
    alignment_row_organization_metrics, assign_alignment_rows, reconcile_alignment_rows,
    reconcile_alignment_rows_with_horizon,
};

const DEFAULT_BAM: &str = "benchmark-data/large-public-alignments/ENCFF686MTA.bam";
const DEFAULT_CONTIG: &str = "chr1";
const BROAD_START: u64 = 36_323_328;
const BROAD_END: u64 = 36_347_904;
const NARROW_START: u64 = 36_340_224;
const NARROW_END: u64 = 36_341_248;
const MOTIVATING_BROAD_START: u64 = 36_337_133;
const MOTIVATING_BROAD_END: u64 = 36_345_430;
const MOTIVATING_NARROW_START: u64 = 36_339_308;
const MOTIVATING_NARROW_END: u64 = 36_343_089;
const CANONICAL_HORIZON_START: u64 = 36_274_176;
const CANONICAL_HORIZON_END: u64 = 36_405_248;
const COARSE_VIEW_START: u64 = 36_292_458;
const COARSE_VIEW_END: u64 = 36_386_166;
const COARSE_FORWARD_PREFETCH_START: u64 = 36_175_872;
const COARSE_FORWARD_PREFETCH_END: u64 = 36_569_088;

fn row_map(layout: &AlignmentRowLayout) -> HashMap<AlignmentRowIdentity, usize> {
    (0..layout.len())
        .filter_map(|index| Some((layout.identity(index)?, layout.row(index)?)))
        .collect()
}

fn qualify_canonical_horizon(
    source: &mut LocalAlignmentSource,
    contig_id: rgv_core::ContigId,
    query_options: QueryOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let horizon_started = Instant::now();
    let horizon = source.query(
        GenomicInterval::new(contig_id, CANONICAL_HORIZON_START, CANONICAL_HORIZON_END)?,
        query_options.clone(),
        &|| true,
    )?;
    let horizon_query_micros = horizon_started.elapsed().as_micros();
    let broad = source.query(
        GenomicInterval::new(contig_id, MOTIVATING_BROAD_START, MOTIVATING_BROAD_END)?,
        query_options.clone(),
        &|| true,
    )?;
    let narrow = source.query(
        GenomicInterval::new(contig_id, MOTIVATING_NARROW_START, MOTIVATING_NARROW_END)?,
        query_options.clone(),
        &|| true,
    )?;
    let options = AlignmentLayoutOptions {
        max_rows: horizon.batch.len().max(1),
        ..AlignmentLayoutOptions::default()
    };
    let canonical = assign_alignment_rows(&horizon, options, None);
    let canonical_rows = row_map(&canonical);
    let canonical_metrics = alignment_row_organization_metrics(&horizon, options, &canonical);

    let mut narrow_first_history = AlignmentRowHistory::default();
    let (narrow_first, narrow_first_stats) = reconcile_alignment_rows_with_horizon(
        &narrow,
        &horizon,
        options,
        &mut narrow_first_history,
        ReadStackingMode::Stable,
        false,
    );
    let (broad_after_narrow, broad_after_narrow_stats) = reconcile_alignment_rows_with_horizon(
        &broad,
        &horizon,
        options,
        &mut narrow_first_history,
        ReadStackingMode::Stable,
        false,
    );

    let mut broad_first_history = AlignmentRowHistory::default();
    let (broad_first, broad_first_stats) = reconcile_alignment_rows_with_horizon(
        &broad,
        &horizon,
        options,
        &mut broad_first_history,
        ReadStackingMode::Stable,
        false,
    );
    let (narrow_after_broad, narrow_after_broad_stats) = reconcile_alignment_rows_with_horizon(
        &narrow,
        &horizon,
        options,
        &mut broad_first_history,
        ReadStackingMode::Stable,
        false,
    );

    let narrow_first_rows = row_map(&narrow_first);
    let narrow_after_broad_rows = row_map(&narrow_after_broad);
    let broad_after_narrow_rows = row_map(&broad_after_narrow);
    let broad_first_rows = row_map(&broad_first);
    let visible_mismatches = narrow_first_rows
        .iter()
        .chain(broad_first_rows.iter())
        .filter(|(identity, row)| canonical_rows.get(identity) != Some(*row))
        .count();
    let narrow_order_moved = narrow_first_rows
        .iter()
        .filter(|(identity, row)| narrow_after_broad_rows.get(identity) != Some(*row))
        .count();
    let broad_order_moved = broad_first_rows
        .iter()
        .filter(|(identity, row)| broad_after_narrow_rows.get(identity) != Some(*row))
        .count();

    println!(
        "canonical_horizon={}-{} horizon_loaded={} horizon_status={:?} horizon_query_micros={} horizon_bytes={} motivating_broad_loaded={} motivating_narrow_loaded={}",
        CANONICAL_HORIZON_START,
        CANONICAL_HORIZON_END,
        horizon.batch.len(),
        horizon.stats.individual_reads,
        horizon_query_micros,
        horizon.estimated_bytes(),
        broad.batch.len(),
        narrow.batch.len(),
    );
    println!(
        "canonical_rows={} top_first_violations={} visible_mismatches={} narrow_order_moved={} broad_order_moved={} horizon_generation={} horizon_extensions={} broad_stable_rows={} broad_canonical_rows={} broad_displacement={} broad_empty_rows={} broad_empty_ratio_ppm={} first_micros={} revisit_micros={} history_bytes={}",
        canonical.used_row_count(),
        canonical_metrics.top_first_violations,
        visible_mismatches,
        narrow_order_moved,
        broad_order_moved,
        broad_first_stats.horizon_generation,
        broad_first_stats.horizon_extensions,
        broad_first_stats.stable_rows,
        broad_first_stats.canonical_rows,
        broad_first_stats.canonical_displacement,
        broad_first_stats.query_empty_rows,
        broad_first_stats.query_empty_ratio_ppm,
        narrow_first_stats.reconciliation_micros,
        narrow_after_broad_stats.reconciliation_micros,
        broad_after_narrow_stats.history_bytes,
    );
    if !horizon.individual_reads_complete()
        || canonical_metrics.top_first_violations != 0
        || visible_mismatches != 0
        || narrow_order_moved != 0
        || broad_order_moved != 0
        || broad_first_stats.horizon_generation != 1
        || broad_first_stats.horizon_extensions != 0
        || narrow_after_broad_stats.moved != 0
        || broad_after_narrow_stats.moved != 0
    {
        return Err("canonical stable read-architecture qualification failed".into());
    }

    let coarse_view = GenomicInterval::new(contig_id, COARSE_VIEW_START, COARSE_VIEW_END)?;
    let direct = source.query(coarse_view, query_options.clone(), &|| true)?;
    let prefetched = source.query(
        GenomicInterval::new(
            contig_id,
            COARSE_FORWARD_PREFETCH_START,
            COARSE_FORWARD_PREFETCH_END,
        )?,
        query_options,
        &|| true,
    )?;
    println!(
        "coarse_direct_loaded={} coarse_direct_status={:?} coarse_prefetch_eligible={} coarse_prefetch_loaded={} coarse_prefetch_status={:?}",
        direct.batch.len(),
        direct.stats.individual_reads,
        prefetched.stats.records_eligible,
        prefetched.batch.len(),
        prefetched.stats.individual_reads,
    );
    if !direct.individual_reads_complete()
        || prefetched.batch.len() != 0
        || !matches!(
            prefetched.stats.individual_reads,
            IndividualReadStatus::TooDense { limit: 50_000 }
        )
    {
        return Err("complete-or-refuse coarse-query qualification failed".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let bam = arguments
        .next()
        .map_or_else(|| PathBuf::from(DEFAULT_BAM), PathBuf::from);
    let contig = arguments
        .next()
        .unwrap_or_else(|| DEFAULT_CONTIG.to_owned());
    let mut source = LocalAlignmentSource::open(&bam, &AlignmentOpenOptions::default())?;
    let contig_id = source
        .reference_dictionary()
        .id_by_name(&contig)
        .ok_or_else(|| format!("{contig} is absent from {}", bam.display()))?;
    let query_options = QueryOptions {
        detail: QueryDetail::Layout,
        max_records: NonZeroUsize::new(50_000).expect("the qualification cap is non-zero"),
        ..QueryOptions::default()
    };
    qualify_canonical_horizon(&mut source, contig_id, query_options.clone())?;
    let broad = source.query(
        GenomicInterval::new(contig_id, BROAD_START, BROAD_END)?,
        query_options.clone(),
        &|| true,
    )?;
    let narrow = source.query(
        GenomicInterval::new(contig_id, NARROW_START, NARROW_END)?,
        query_options,
        &|| true,
    )?;
    let broad_options = AlignmentLayoutOptions {
        max_rows: broad.batch.len().max(1),
        pair_length_bounds: broad.pairs.inferred_bounds.unwrap_or_default(),
        ..AlignmentLayoutOptions::default()
    };
    let narrow_options = AlignmentLayoutOptions {
        max_rows: narrow.batch.len().max(1),
        pair_length_bounds: narrow.pairs.inferred_bounds.unwrap_or_default(),
        ..broad_options
    };

    let scratch_broad = assign_alignment_rows(&broad, broad_options, None);
    let scratch_narrow = assign_alignment_rows(&narrow, narrow_options, None);
    let scratch_broad_rows = row_map(&scratch_broad);
    let scratch_narrow_rows = row_map(&scratch_narrow);
    let shared = scratch_narrow_rows
        .keys()
        .filter(|identity| scratch_broad_rows.contains_key(identity))
        .count();
    let scratch_moved = scratch_narrow_rows
        .iter()
        .filter(|(identity, row)| {
            scratch_broad_rows
                .get(identity)
                .is_some_and(|old| old != *row)
        })
        .count();

    let mut history = AlignmentRowHistory::default();
    let (stable_broad, broad_stats) = reconcile_alignment_rows(
        &broad,
        broad_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let stable_broad_rows = row_map(&stable_broad);
    let (stable_narrow, narrow_stats) = reconcile_alignment_rows(
        &narrow,
        narrow_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let stable_narrow_rows = row_map(&stable_narrow);
    let stable_moved = stable_narrow_rows
        .iter()
        .filter(|(identity, row)| {
            stable_broad_rows
                .get(identity)
                .is_some_and(|old| old != *row)
        })
        .count();
    let (restored_broad, restored_stats) = reconcile_alignment_rows(
        &broad,
        broad_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let restored_moved = row_map(&restored_broad)
        .iter()
        .filter(|(identity, row)| {
            stable_broad_rows
                .get(identity)
                .is_some_and(|old| old != *row)
        })
        .count();

    println!(
        "broad_loaded={} narrow_loaded={} shared={} scratch_moved={} stable_moved={} restored_moved={}",
        broad.batch.len(),
        narrow.batch.len(),
        shared,
        scratch_moved,
        stable_moved,
        restored_moved
    );
    println!(
        "broad_revision={} narrow_revision={} restored_revision={} retained={} restored={} reservations={} islands={} history_bytes={} evictions={} broad_micros={} narrow_micros={} restored_micros={} scratch_narrow_rows={} stable_narrow_rows={} row_inflation={}",
        broad_stats.revision,
        narrow_stats.revision,
        restored_stats.revision,
        narrow_stats.retained,
        restored_stats.restored,
        narrow_stats.reservations,
        restored_stats.islands,
        restored_stats.history_bytes,
        restored_stats.evictions,
        broad_stats.reconciliation_micros,
        narrow_stats.reconciliation_micros,
        restored_stats.reconciliation_micros,
        scratch_narrow.used_row_count(),
        stable_narrow.row_capacity(),
        stable_narrow
            .row_capacity()
            .saturating_sub(scratch_narrow.used_row_count())
    );
    if shared == 0 || stable_moved != 0 || restored_moved != 0 {
        return Err("stable read-stack qualification failed".into());
    }
    Ok(())
}
