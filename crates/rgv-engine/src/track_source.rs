//! One interface for every auxiliary track source.
//!
//! The track engine holds each opened file as a `Box<dyn TrackSource>` and never matches on the
//! file format after opening. A source owns its reader exclusively, so different tracks can be
//! queried on different threads at the same time.

use std::sync::{Arc, Mutex};

use rgv_core::{CopyNumberTrackSettings, InteractionTrackSettings, VariantTrackSettings};
use rgv_hts::{
    BigBedSource, BigWigSource, FeatureRecord, LocalVariantSource, RemoteCancellation,
    RemoteTransferBudget, Strand, TdfSource, TextCopyNumberSource, TextFeatureSource,
    TextGwasSource, TextInteractionSource, TextMutationSource, TextQuantitativeSource,
};

use crate::{TrackSnapshotData, TrackSourceDiagnostics};

/// Upper bound on variant records returned for one viewport query.
const MAX_VARIANT_QUERY_RECORDS: usize = 100_000;

/// Per-track display settings that change what a query returns. A source reads only the
/// setting that applies to its own kind.
#[derive(Clone, Copy, Default)]
pub(crate) struct TrackQuerySettings<'a> {
    pub(crate) variant: Option<&'a VariantTrackSettings>,
    pub(crate) interaction: Option<&'a InteractionTrackSettings>,
    pub(crate) copy_number: Option<&'a CopyNumberTrackSettings>,
}

/// Name-search results from one source, before the engine merges and ranks them.
#[derive(Default)]
pub(crate) struct SourceSearch {
    /// Matched term and the feature it belongs to.
    pub(crate) hits: Vec<(String, FeatureRecord)>,
    pub(crate) index_truncated: bool,
    pub(crate) scan_truncated: bool,
}

/// What a name search is looking for.
#[derive(Clone, Copy)]
pub(crate) struct SearchRequest<'a> {
    /// The query exactly as typed.
    pub(crate) query: &'a str,
    /// The query trimmed and lower-cased.
    pub(crate) normalized: &'a str,
    pub(crate) limit: usize,
    pub(crate) include_variant_ids: bool,
}

pub(crate) trait TrackSource: Send {
    /// Everything this track shows in `[start, end)` of `contig`.
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String>;

    /// The nearest feature after (or before) `position`; `None` for kinds without discrete
    /// navigable features.
    fn adjacent_feature(
        &mut self,
        _contig: &str,
        _position: u64,
        _forward: bool,
    ) -> Result<Option<FeatureRecord>, String> {
        Ok(None)
    }

    /// Features whose name matches; `None` for kinds that are not searchable.
    fn search(&mut self, _request: SearchRequest<'_>) -> Result<Option<SourceSearch>, String> {
        Ok(None)
    }

    /// For a multi-channel archive: every channel name and the channel this track shows.
    fn channels(&self) -> Option<(Vec<String>, usize)> {
        None
    }

    /// Governs the remote reads of the operations that follow: `keep_going` abandons them once
    /// it returns false, and `budget` is a fresh allowance of requests and bytes. `None` lifts
    /// either. Sources that read no remote data through RGV's range reader ignore both.
    fn set_remote_control(
        &mut self,
        _keep_going: Option<RemoteCancellation>,
        _budget: Option<RemoteTransferBudget>,
    ) {
    }

    /// Input the source had to skip; `None` for kinds that do not track it.
    fn diagnostics(&self) -> Option<TrackSourceDiagnostics> {
        None
    }
}

impl TrackSource for TextFeatureSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        TextFeatureSource::query(self, contig, start, end)
            .map(TrackSnapshotData::Features)
            .map_err(|error| error.to_string())
    }

    fn adjacent_feature(
        &mut self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Result<Option<FeatureRecord>, String> {
        Ok(TextFeatureSource::adjacent_feature(
            self, contig, position, forward,
        ))
    }

    fn search(&mut self, request: SearchRequest<'_>) -> Result<Option<SourceSearch>, String> {
        let results = TextFeatureSource::search(self, request.normalized, request.limit);
        Ok(Some(SourceSearch {
            hits: results
                .hits
                .into_iter()
                .map(|hit| (hit.matched_term, hit.record))
                .collect(),
            index_truncated: results.index_truncated,
            scan_truncated: results.scan_truncated,
        }))
    }

    fn diagnostics(&self) -> Option<TrackSourceDiagnostics> {
        Some(TrackSourceDiagnostics {
            malformed_lines: self.malformed_line_count(),
            first_malformed_line: self
                .first_malformed_line()
                .map(|(line, message)| (line, message.to_owned())),
        })
    }
}

impl TrackSource for BigBedSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        BigBedSource::query(self, contig, start, end)
            .map(TrackSnapshotData::Features)
            .map_err(|error| error.to_string())
    }

    fn adjacent_feature(
        &mut self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Result<Option<FeatureRecord>, String> {
        BigBedSource::adjacent_feature(self, contig, position, forward)
            .map_err(|error| error.to_string())
    }

    fn search(&mut self, request: SearchRequest<'_>) -> Result<Option<SourceSearch>, String> {
        let results = BigBedSource::search(self, request.query, request.limit)
            .map_err(|error| error.to_string())?;
        Ok(Some(SourceSearch {
            hits: results
                .hits
                .into_iter()
                .map(|hit| (hit.matched_term, hit.record))
                .collect(),
            index_truncated: results.index_truncated,
            scan_truncated: results.scan_truncated,
        }))
    }

    fn set_remote_control(
        &mut self,
        keep_going: Option<RemoteCancellation>,
        budget: Option<RemoteTransferBudget>,
    ) {
        self.set_remote_cancellation(keep_going);
        self.set_remote_transfer_budget(budget);
    }
}

impl TrackSource for LocalVariantSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        let default_settings = VariantTrackSettings::default();
        self.query_with_settings(
            contig,
            start,
            end,
            MAX_VARIANT_QUERY_RECORDS,
            settings.variant.unwrap_or(&default_settings),
        )
        .map(|records| TrackSnapshotData::Variants(Arc::new(records)))
        .map_err(|error| error.to_string())
    }

    fn search(&mut self, request: SearchRequest<'_>) -> Result<Option<SourceSearch>, String> {
        if !request.include_variant_ids {
            return Ok(None);
        }
        let results = self
            .search_ids(request.query, request.limit)
            .map_err(|error| error.to_string())?;
        Ok(Some(SourceSearch {
            hits: results
                .hits
                .into_iter()
                .map(|hit| {
                    (
                        hit.matched_id,
                        FeatureRecord {
                            contig: hit.contig,
                            start: hit.start,
                            end: hit.end,
                            name: Some(hit.id),
                            feature_type: Some("variant".to_owned()),
                            score: None,
                            strand: Strand::Unstranded,
                            blocks: Vec::new(),
                            coding_interval: None,
                            peak: None,
                            attributes: Vec::new(),
                        },
                    )
                })
                .collect(),
            index_truncated: results.index_truncated,
            scan_truncated: results.scan_truncated,
        }))
    }

    fn set_remote_control(
        &mut self,
        keep_going: Option<RemoteCancellation>,
        budget: Option<RemoteTransferBudget>,
    ) {
        self.set_remote_cancellation(keep_going);
        self.set_remote_transfer_budget(budget);
    }
}

impl TrackSource for TextGwasSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        Ok(TrackSnapshotData::Gwas(Arc::new(TextGwasSource::query(
            self, contig, start, end,
        ))))
    }

    fn adjacent_feature(
        &mut self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Result<Option<FeatureRecord>, String> {
        Ok(self
            .adjacent_association(contig, position, forward)
            .map(gwas_feature_record))
    }

    fn search(&mut self, request: SearchRequest<'_>) -> Result<Option<SourceSearch>, String> {
        let results = TextGwasSource::search(self, request.query, request.limit);
        Ok(Some(SourceSearch {
            hits: results
                .hits
                .into_iter()
                .map(|hit| (hit.matched_term, gwas_feature_record(hit.record)))
                .collect(),
            index_truncated: results.index_truncated,
            scan_truncated: results.scan_truncated,
        }))
    }
}

impl TrackSource for BigWigSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        BigWigSource::query(self, contig, start, end)
            .map(|query| TrackSnapshotData::Quantitative(Arc::new(query)))
            .map_err(|error| error.to_string())
    }

    fn set_remote_control(
        &mut self,
        keep_going: Option<RemoteCancellation>,
        budget: Option<RemoteTransferBudget>,
    ) {
        self.set_remote_cancellation(keep_going);
        self.set_remote_transfer_budget(budget);
    }
}

impl TrackSource for TextQuantitativeSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        Ok(TrackSnapshotData::Quantitative(Arc::new(
            TextQuantitativeSource::query(self, contig, start, end),
        )))
    }
}

impl TrackSource for TextInteractionSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        let default_settings = InteractionTrackSettings::default();
        Ok(TrackSnapshotData::Interactions(Arc::new(
            TextInteractionSource::query(
                self,
                contig,
                start,
                end,
                settings.interaction.unwrap_or(&default_settings),
            ),
        )))
    }
}

impl TrackSource for TextCopyNumberSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        let default_settings = CopyNumberTrackSettings::default();
        Ok(TrackSnapshotData::CopyNumber(Arc::new(
            TextCopyNumberSource::query(
                self,
                contig,
                start,
                end,
                settings.copy_number.unwrap_or(&default_settings),
            ),
        )))
    }
}

impl TrackSource for TextMutationSource {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        Ok(TrackSnapshotData::Mutations(Arc::new(
            TextMutationSource::query(self, contig, start, end),
        )))
    }
}

/// One channel of a TDF archive. Channels of the same file share one reader.
pub(crate) struct TdfChannel {
    pub(crate) source: Arc<Mutex<TdfSource>>,
    pub(crate) track_index: usize,
    /// Remote control of this channel's next query. Channels of one archive share a reader, so
    /// it is installed while the reader is locked for the query, never ahead of it.
    pub(crate) keep_going: Option<RemoteCancellation>,
    pub(crate) budget: Option<RemoteTransferBudget>,
}

impl TrackSource for TdfChannel {
    fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        _settings: TrackQuerySettings<'_>,
    ) -> Result<TrackSnapshotData, String> {
        let mut source = self
            .source
            .lock()
            .map_err(|_| "the TDF reader failed in another query".to_owned())?;
        source.set_remote_cancellation(self.keep_going.clone());
        source.set_remote_transfer_budget(self.budget);
        let result = source
            .query_channel(self.track_index, contig, start, end)
            .map(|query| TrackSnapshotData::Quantitative(Arc::new(query)))
            .map_err(|error| error.to_string());
        // Another channel's query must not inherit this one's (possibly stale) check.
        source.set_remote_cancellation(None);
        result
    }

    fn set_remote_control(
        &mut self,
        keep_going: Option<RemoteCancellation>,
        budget: Option<RemoteTransferBudget>,
    ) {
        self.keep_going = keep_going;
        self.budget = budget;
    }

    fn channels(&self) -> Option<(Vec<String>, usize)> {
        let source = self.source.lock().ok()?;
        Some((source.metadata().track_names.clone(), self.track_index))
    }
}

pub(crate) fn gwas_feature_record(record: rgv_hts::GwasRecord) -> FeatureRecord {
    FeatureRecord {
        contig: record.contig,
        start: record.position,
        end: record.position.saturating_add(1),
        name: Some(record.identifier),
        feature_type: Some("GWAS association".to_owned()),
        score: record
            .minus_log10_p
            .is_finite()
            .then_some(record.minus_log10_p),
        strand: Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: record.fields,
    }
}
