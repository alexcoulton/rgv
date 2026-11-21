//! Selections of records in auxiliary-track lanes, and keeping them attached to the same record
//! across lane refreshes.
//!
//! A selection stores an index into the lane's current query result. When a refreshed result
//! replaces that lane the index is meaningless on its own, so the selected record itself is the
//! stable identity: it is looked up again in the new result. Only when the record is no longer
//! loaded is the selection dropped.

use rgv_core::TrackId;
use rgv_engine::{TrackSnapshot, TrackSnapshotData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureSelection {
    pub track_id: TrackId,
    pub record_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VariantSelection {
    pub track_id: TrackId,
    pub record_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GwasSelection {
    pub track_id: TrackId,
    pub record_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuantitativeSelection {
    pub track_id: TrackId,
    pub record_index: usize,
    pub position: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyNumberSelection {
    pub track_id: TrackId,
    pub segment_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationSelection {
    pub track_id: TrackId,
    pub record_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InteractionSelection {
    pub track_id: TrackId,
    pub record_index: usize,
}

/// Index of the record previously at `previous_index` in `previous`, within `refreshed`.
pub fn reresolve_record_index<T: PartialEq>(
    previous: &[T],
    previous_index: usize,
    refreshed: &[T],
) -> Option<usize> {
    let record = previous.get(previous_index)?;
    // The common refresh returns the same records in the same order.
    if refreshed.get(previous_index) == Some(record) {
        return Some(previous_index);
    }
    refreshed.iter().position(|candidate| candidate == record)
}

/// The auxiliary selections of one panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AuxiliarySelections {
    pub feature: Option<FeatureSelection>,
    pub variant: Option<VariantSelection>,
    pub gwas: Option<GwasSelection>,
    pub quantitative: Option<QuantitativeSelection>,
    pub copy_number: Option<CopyNumberSelection>,
    pub mutation: Option<MutationSelection>,
    pub interaction: Option<InteractionSelection>,
}

fn reresolve<S: Copy>(
    selection: Option<S>,
    track_id: TrackId,
    selection_track: impl Fn(&S) -> TrackId,
    resolve: impl FnOnce(S) -> Option<S>,
) -> Option<S> {
    match selection {
        Some(current) if selection_track(&current) == track_id => resolve(current),
        other => other,
    }
}

/// Selections after the lane `refreshed.id` is replaced. Selections on other tracks are
/// untouched; a selection on this track follows its record or, if the record is gone (or the
/// lane had no previous result to identify it by), is cleared.
#[allow(clippy::too_many_lines)]
pub fn reresolve_auxiliary_selections(
    selections: AuxiliarySelections,
    previous: Option<&TrackSnapshot>,
    refreshed: &TrackSnapshot,
) -> AuxiliarySelections {
    let track_id = refreshed.id;
    let previous = previous.map(|snapshot| &snapshot.data);
    let current = &refreshed.data;
    AuxiliarySelections {
        feature: reresolve(
            selections.feature,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (Some(TrackSnapshotData::Features(old)), TrackSnapshotData::Features(new)) => {
                    reresolve_record_index(old, selection.record_index, new).map(|record_index| {
                        FeatureSelection {
                            track_id,
                            record_index,
                        }
                    })
                }
                _ => None,
            },
        ),
        variant: reresolve(
            selections.variant,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (Some(TrackSnapshotData::Variants(old)), TrackSnapshotData::Variants(new)) => {
                    reresolve_record_index(&old.records, selection.record_index, &new.records).map(
                        |record_index| VariantSelection {
                            track_id,
                            record_index,
                        },
                    )
                }
                _ => None,
            },
        ),
        gwas: reresolve(
            selections.gwas,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (Some(TrackSnapshotData::Gwas(old)), TrackSnapshotData::Gwas(new)) => {
                    reresolve_record_index(&old.records, selection.record_index, &new.records).map(
                        |record_index| GwasSelection {
                            track_id,
                            record_index,
                        },
                    )
                }
                _ => None,
            },
        ),
        quantitative: reresolve(
            selections.quantitative,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (
                    Some(TrackSnapshotData::Quantitative(old)),
                    TrackSnapshotData::Quantitative(new),
                ) => reresolve_record_index(&old.records, selection.record_index, &new.records)
                    .map(|record_index| QuantitativeSelection {
                        track_id,
                        record_index,
                        position: selection.position,
                    }),
                _ => None,
            },
        ),
        copy_number: reresolve(
            selections.copy_number,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (Some(TrackSnapshotData::CopyNumber(old)), TrackSnapshotData::CopyNumber(new)) => {
                    // A segment's sample is an index into the sample table, so the same segment
                    // is only the same record if it still names the same sample.
                    let sample = |query: &rgv_hts::CopyNumberQuery, index: usize| {
                        query
                            .segments
                            .get(index)
                            .and_then(|segment| query.samples.get(segment.sample_index))
                            .cloned()
                    };
                    reresolve_record_index(&old.segments, selection.segment_index, &new.segments)
                        .filter(|index| sample(old, selection.segment_index) == sample(new, *index))
                        .map(|segment_index| CopyNumberSelection {
                            track_id,
                            segment_index,
                        })
                }
                _ => None,
            },
        ),
        mutation: reresolve(
            selections.mutation,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (Some(TrackSnapshotData::Mutations(old)), TrackSnapshotData::Mutations(new)) => {
                    reresolve_record_index(&old.records, selection.record_index, &new.records).map(
                        |record_index| MutationSelection {
                            track_id,
                            record_index,
                        },
                    )
                }
                _ => None,
            },
        ),
        interaction: reresolve(
            selections.interaction,
            track_id,
            |selection| selection.track_id,
            |selection| match (previous, current) {
                (
                    Some(TrackSnapshotData::Interactions(old)),
                    TrackSnapshotData::Interactions(new),
                ) => reresolve_record_index(&old.records, selection.record_index, &new.records)
                    .map(|record_index| InteractionSelection {
                        track_id,
                        record_index,
                    }),
                _ => None,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_hts::{FeatureRecord, Strand};

    fn feature(name: &str, start: u64) -> FeatureRecord {
        FeatureRecord {
            contig: "chr1".to_owned(),
            start,
            end: start + 100,
            name: Some(name.to_owned()),
            feature_type: Some("gene".to_owned()),
            score: None,
            strand: Strand::Forward,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        }
    }

    fn lane(id: u64, records: Vec<FeatureRecord>) -> TrackSnapshot {
        TrackSnapshot {
            id: TrackId::new(id),
            name: "genes".to_owned(),
            data: TrackSnapshotData::Features(records),
        }
    }

    fn selected(track: u64, record_index: usize) -> AuxiliarySelections {
        AuxiliarySelections {
            feature: Some(FeatureSelection {
                track_id: TrackId::new(track),
                record_index,
            }),
            ..AuxiliarySelections::default()
        }
    }

    #[test]
    fn a_record_is_found_again_at_its_new_index() {
        assert_eq!(reresolve_record_index(&[1, 2, 3], 1, &[1, 2, 3]), Some(1));
        assert_eq!(
            reresolve_record_index(&[1, 2, 3], 1, &[0, 1, 2, 3]),
            Some(2)
        );
        assert_eq!(reresolve_record_index(&[1, 2, 3], 2, &[3]), Some(0));
        assert_eq!(reresolve_record_index(&[1, 2, 3], 1, &[1, 3]), None);
        assert_eq!(reresolve_record_index(&[1, 2, 3], 9, &[1, 2, 3]), None);
    }

    #[test]
    fn a_selected_feature_survives_a_refresh_that_reorders_the_lane() {
        let before = lane(4, vec![feature("A", 100), feature("B", 500)]);
        // Panning left loads an extra upstream feature: B moves from index 1 to 2.
        let after = lane(
            4,
            vec![feature("Z", 10), feature("A", 100), feature("B", 500)],
        );
        let resolved = reresolve_auxiliary_selections(selected(4, 1), Some(&before), &after);
        assert_eq!(resolved, selected(4, 2));
    }

    #[test]
    fn a_selection_is_cleared_only_when_its_record_is_no_longer_loaded() {
        let before = lane(4, vec![feature("A", 100), feature("B", 500)]);
        let after = lane(4, vec![feature("A", 100)]);
        assert_eq!(
            reresolve_auxiliary_selections(selected(4, 1), Some(&before), &after),
            AuxiliarySelections::default()
        );
        // Without a previous result there is nothing to identify the record by.
        assert_eq!(
            reresolve_auxiliary_selections(selected(4, 0), None, &after),
            AuxiliarySelections::default()
        );
    }

    #[test]
    fn selections_on_other_tracks_are_untouched_by_a_lane_refresh() {
        let before = lane(4, vec![feature("A", 100)]);
        let after = lane(4, Vec::new());
        assert_eq!(
            reresolve_auxiliary_selections(selected(9, 7), Some(&before), &after),
            selected(9, 7)
        );
    }

    #[test]
    fn a_lane_that_changes_kind_drops_its_selection() {
        let before = lane(4, vec![feature("A", 100)]);
        let after = TrackSnapshot {
            id: TrackId::new(4),
            name: "calls".to_owned(),
            data: TrackSnapshotData::Mutations(std::sync::Arc::default()),
        };
        assert_eq!(
            reresolve_auxiliary_selections(selected(4, 0), Some(&before), &after),
            AuxiliarySelections::default()
        );
    }
}
