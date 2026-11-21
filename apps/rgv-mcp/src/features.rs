//! Annotation-feature queries and gene navigation.
//!
//! `rgv_query_features` and `rgv_goto_gene` are thin adapters over the viewer's `query_features`
//! and `goto_gene` control commands. This module builds those commands from tool arguments,
//! rejecting an out-of-range argument before anything is sent, and reshapes the viewer's typed
//! results into the bounded JSON the tools return. Nothing here talks to the viewer, so both
//! directions are tested directly.
//!
//! Every numeric coordinate accepted and returned is zero-based and half-open, like every other
//! numeric coordinate the MCP tools use. The one-based form appears only in `locus` strings.

use rgv_automation::{
    AutomationCommand, AutomationFeatureQueryResult, AutomationGeneNavigationResult,
    MAXIMUM_AUTOMATION_FEATURE_OFFSET, MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN,
    MAXIMUM_AUTOMATION_FEATURE_RESULTS, MAXIMUM_AUTOMATION_GENE_FLANK_BASES,
    MAXIMUM_AUTOMATION_GENE_QUERY_BYTES, MAXIMUM_AUTOMATION_STRING_BYTES,
    MAXIMUM_AUTOMATION_TRACK_SELECTION,
};
use rgv_core::{PanelId, TrackId};
use serde_json::{Value, json};
use thiserror::Error;

use crate::evidence::{bounded, page};

pub(crate) const DEFAULT_FEATURE_LIMIT: usize = 50;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum FeatureError {
    #[error("{0}")]
    InvalidInput(String),
    /// The viewer's result does not have the shape the request allows.
    #[error("RGV returned a feature result this adapter cannot interpret: {0}")]
    Malformed(String),
}

impl FeatureError {
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Malformed(_) => "invalid_response",
        }
    }
}

/// The interval a feature query asks about, zero-based and half-open, with its paging window.
#[derive(Clone, Debug)]
pub(crate) struct FeatureQuery {
    pub(crate) contig: String,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) track_ids: Option<Vec<u64>>,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
}

fn invalid(message: impl Into<String>) -> FeatureError {
    FeatureError::InvalidInput(message.into())
}

/// Builds the viewer's `query_features` command, or says which argument is out of range.
pub(crate) fn feature_query_command(
    query: &FeatureQuery,
) -> Result<AutomationCommand, FeatureError> {
    if query.contig.trim().is_empty() || query.contig.len() > MAXIMUM_AUTOMATION_STRING_BYTES {
        return Err(invalid(format!(
            "contig must be non-empty and no more than {MAXIMUM_AUTOMATION_STRING_BYTES} bytes"
        )));
    }
    if query.start >= query.end {
        return Err(invalid(
            "end must be greater than start (zero-based, half-open interval)",
        ));
    }
    if query.end - query.start > MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN {
        return Err(invalid(format!(
            "the interval must span no more than {MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN} bases; \
             query a narrower interval"
        )));
    }
    if let Some(track_ids) = &query.track_ids
        && (track_ids.is_empty()
            || track_ids.len() > MAXIMUM_AUTOMATION_TRACK_SELECTION
            || track_ids.contains(&0))
    {
        return Err(invalid(format!(
            "track_ids must contain 1 to {MAXIMUM_AUTOMATION_TRACK_SELECTION} positive track IDs"
        )));
    }
    if query.offset > MAXIMUM_AUTOMATION_FEATURE_OFFSET {
        return Err(invalid(format!(
            "offset must be no greater than {MAXIMUM_AUTOMATION_FEATURE_OFFSET}"
        )));
    }
    if !(1..=MAXIMUM_AUTOMATION_FEATURE_RESULTS).contains(&query.limit) {
        return Err(invalid(format!(
            "limit must be between 1 and {MAXIMUM_AUTOMATION_FEATURE_RESULTS}"
        )));
    }
    Ok(AutomationCommand::QueryFeatures {
        contig: query.contig.clone(),
        start: query.start,
        end: query.end,
        track_ids: query
            .track_ids
            .as_ref()
            .map(|ids| ids.iter().copied().map(TrackId::new).collect()),
        offset: query.offset,
        limit: query.limit,
    })
}

/// Builds the viewer's `goto_gene` command, or says which argument is out of range.
pub(crate) fn goto_gene_command(
    query: &str,
    panel_id: Option<u64>,
    flank_bases: u64,
) -> Result<AutomationCommand, FeatureError> {
    if query.trim().is_empty() || query.len() > MAXIMUM_AUTOMATION_GENE_QUERY_BYTES {
        return Err(invalid(format!(
            "query must be non-empty and no more than {MAXIMUM_AUTOMATION_GENE_QUERY_BYTES} bytes"
        )));
    }
    if panel_id == Some(0) {
        return Err(invalid("panel_id must be positive"));
    }
    if flank_bases > MAXIMUM_AUTOMATION_GENE_FLANK_BASES {
        return Err(invalid(format!(
            "flank_bases must be no greater than {MAXIMUM_AUTOMATION_GENE_FLANK_BASES}"
        )));
    }
    Ok(AutomationCommand::GotoGene {
        query: query.to_owned(),
        panel_id: panel_id.map(PanelId::new),
        flank_bases,
    })
}

/// Reshapes the viewer's `query_features` result into one page of features.
pub(crate) fn features_result(
    result: &AutomationFeatureQueryResult,
    query: &FeatureQuery,
) -> Result<Value, FeatureError> {
    if result.features.len() > query.limit {
        return Err(FeatureError::Malformed(format!(
            "{} features were returned for a limit of {}",
            result.features.len(),
            query.limit
        )));
    }
    if result.offset != query.offset {
        return Err(FeatureError::Malformed(
            "the page does not start at the requested offset".to_owned(),
        ));
    }
    let features = result
        .features
        .iter()
        .map(|feature| {
            let (name, name_truncated) = bounded(&feature.name);
            let (feature_type, _) = bounded(&feature.feature_type);
            let (contig, _) = bounded(&feature.contig);
            let mut value = json!({
                "track_id": feature.track_id,
                "name": name,
                "feature_type": feature_type,
                "contig": contig,
                "start": feature.start,
                "end": feature.end,
                "strand": feature.strand,
            });
            if name_truncated {
                value["name_truncated"] = json!(true);
            }
            if let Some(gene_name) = &feature.gene_name {
                value["gene_name"] = json!(bounded(gene_name).0);
            }
            value
        })
        .collect::<Vec<_>>();
    let returned = features.len();
    let mut page = page(result.total_matching, result.offset, returned);
    // The viewer's own flag is authoritative when it disagrees with the arithmetic.
    if result.truncated && page["truncated"] == json!(false) {
        return Err(FeatureError::Malformed(
            "the page is marked truncated but accounts for every matching feature".to_owned(),
        ));
    }
    page["limit"] = json!(query.limit);
    Ok(json!({
        "contig": bounded(&result.contig).0,
        "coordinates": "zero-based; intervals are half-open [start, end)",
        "query_start": result.start,
        "query_end": result.end,
        "track_ids": result.track_ids,
        "order": "track (workspace order), then start, end, name",
        "features": features,
        "page": page,
    }))
}

/// Reshapes the viewer's `goto_gene` result.
pub(crate) fn gene_navigation_result(result: &AutomationGeneNavigationResult) -> Value {
    let (name, name_truncated) = bounded(&result.hit.name);
    let contig = bounded(&result.contig).0;
    let mut hit = json!({
        "track_id": result.hit.track_id,
        "name": name,
        "feature_type": bounded(&result.hit.feature_type).0,
    });
    if name_truncated {
        hit["name_truncated"] = json!(true);
    }
    json!({
        "panel_id": result.panel_id,
        "contig": contig,
        "coordinates": "zero-based; intervals are half-open [start, end)",
        "start": result.start,
        "end": result.end,
        // The same interval as a one-based inclusive locus string, as `rgv_navigate` takes it.
        "locus": format!("{contig}:{}-{}", result.start.saturating_add(1), result.end),
        "hit": hit,
        "candidate_count": result.candidate_count,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use rgv_automation::{AutomationFeature, AutomationGeneHit};

    use super::*;

    pub(crate) fn query(start: u64, end: u64) -> FeatureQuery {
        FeatureQuery {
            contig: "chr17".to_owned(),
            start,
            end,
            track_ids: None,
            offset: 0,
            limit: DEFAULT_FEATURE_LIMIT,
        }
    }

    fn feature(name: &str, start: u64, end: u64) -> AutomationFeature {
        AutomationFeature {
            track_id: 3,
            name: name.to_owned(),
            feature_type: "gene".to_owned(),
            contig: "chr17".to_owned(),
            start,
            end,
            strand: "-".to_owned(),
            gene_name: Some(name.to_owned()),
        }
    }

    fn viewer_result(
        features: Vec<AutomationFeature>,
        total_matching: usize,
        offset: usize,
    ) -> AutomationFeatureQueryResult {
        AutomationFeatureQueryResult {
            contig: "chr17".to_owned(),
            start: 100,
            end: 900,
            track_ids: vec![3],
            truncated: offset + features.len() < total_matching,
            features,
            total_matching,
            offset,
        }
    }

    #[test]
    fn a_feature_query_becomes_the_viewers_command_unchanged() {
        let mut request = query(100, 900);
        request.track_ids = Some(vec![3, 5]);
        request.offset = 50;
        request.limit = 500;
        assert_eq!(
            feature_query_command(&request).unwrap(),
            AutomationCommand::QueryFeatures {
                contig: "chr17".to_owned(),
                start: 100,
                end: 900,
                track_ids: Some(vec![TrackId::new(3), TrackId::new(5)]),
                offset: 50,
                limit: 500,
            }
        );
        assert_eq!(
            goto_gene_command("BRCA1", Some(2), 5_000).unwrap(),
            AutomationCommand::GotoGene {
                query: "BRCA1".to_owned(),
                panel_id: Some(PanelId::new(2)),
                flank_bases: 5_000,
            }
        );
        assert_eq!(
            goto_gene_command("TP53", None, 0).unwrap(),
            AutomationCommand::GotoGene {
                query: "TP53".to_owned(),
                panel_id: None,
                flank_bases: 0,
            }
        );
    }

    #[test]
    fn out_of_range_arguments_are_refused_with_the_bound_they_broke() {
        let widest = MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN;
        feature_query_command(&query(7, 7 + widest)).unwrap();
        let rejected = |change: &dyn Fn(&mut FeatureQuery)| {
            let mut request = query(100, 900);
            change(&mut request);
            let error = feature_query_command(&request).unwrap_err();
            assert_eq!(error.code(), "invalid_input");
            error.to_string()
        };
        assert!(rejected(&|request| request.contig = " ".to_owned()).contains("contig"));
        assert!(rejected(&|request| request.end = request.start).contains("half-open"));
        assert!(rejected(&|request| request.end = request.start - 1).contains("half-open"));
        assert!(rejected(&|request| request.end = request.start + widest + 1).contains("10000000"));
        assert!(rejected(&|request| request.track_ids = Some(Vec::new())).contains("track_ids"));
        assert!(rejected(&|request| request.track_ids = Some(vec![3, 0])).contains("positive"));
        assert!(
            rejected(&|request| {
                request.track_ids = Some(vec![1; MAXIMUM_AUTOMATION_TRACK_SELECTION + 1]);
            })
            .contains("1024")
        );
        assert!(
            rejected(&|request| request.offset = MAXIMUM_AUTOMATION_FEATURE_OFFSET + 1)
                .contains("offset")
        );
        assert!(rejected(&|request| request.limit = 0).contains("between 1 and 500"));
        assert!(rejected(&|request| request.limit = 501).contains("between 1 and 500"));

        for (gene, panel_id, flank_bases) in [
            ("", None, 0),
            ("  ", None, 0),
            ("BRCA1", Some(0), 0),
            ("BRCA1", None, MAXIMUM_AUTOMATION_GENE_FLANK_BASES + 1),
        ] {
            let error = goto_gene_command(gene, panel_id, flank_bases).unwrap_err();
            assert_eq!(error.code(), "invalid_input");
        }
        let long = "g".repeat(MAXIMUM_AUTOMATION_GENE_QUERY_BYTES + 1);
        assert!(
            goto_gene_command(&long, None, 0)
                .unwrap_err()
                .to_string()
                .contains("256 bytes")
        );
    }

    #[test]
    fn features_are_returned_as_a_page_with_an_explicit_truncation_flag() {
        let mut first = query(100, 900);
        first.limit = 2;
        let result = features_result(
            &viewer_result(
                vec![feature("BRCA1", 100, 400), feature("NBR2", 380, 500)],
                3,
                0,
            ),
            &first,
        )
        .unwrap();
        assert_eq!(result["contig"], "chr17");
        assert_eq!(
            (&result["query_start"], &result["query_end"]),
            (&json!(100), &json!(900))
        );
        assert_eq!(result["track_ids"], json!([3]));
        assert_eq!(
            result["features"][0],
            json!({
                "track_id": 3, "name": "BRCA1", "feature_type": "gene", "contig": "chr17",
                "start": 100, "end": 400, "strand": "-", "gene_name": "BRCA1",
            })
        );
        assert_eq!(
            result["page"],
            json!({
                "total_matching": 3, "offset": 0, "returned": 2, "truncated": true,
                "next_offset": 2, "limit": 2,
            })
        );

        let mut second = first.clone();
        second.offset = 2;
        let result = features_result(
            &viewer_result(vec![feature("NBR1", 600, 800)], 3, 2),
            &second,
        )
        .unwrap();
        assert_eq!(result["page"]["truncated"], false);
        assert_eq!(result["page"]["next_offset"], Value::Null);
        assert_eq!(result["page"]["returned"], 1);

        let empty = features_result(&viewer_result(Vec::new(), 0, 0), &query(100, 900)).unwrap();
        assert_eq!(empty["features"], json!([]));
        assert_eq!(empty["page"]["total_matching"], 0);
        assert_eq!(empty["page"]["truncated"], false);
    }

    #[test]
    fn oversized_names_are_bounded_and_absent_gene_names_are_omitted() {
        let mut long = feature(&"é".repeat(400), 100, 200);
        long.gene_name = None;
        let result = features_result(&viewer_result(vec![long], 1, 0), &query(100, 900)).unwrap();
        let returned = &result["features"][0];
        let name = returned["name"].as_str().unwrap();
        assert!(name.len() <= 256);
        assert!(name.chars().all(|character| character == 'é'));
        assert_eq!(returned["name_truncated"], true);
        assert!(returned.get("gene_name").is_none());
    }

    #[test]
    fn a_result_that_contradicts_the_request_is_an_invalid_response() {
        let mut one = query(100, 900);
        one.limit = 1;
        let over = viewer_result(vec![feature("A", 100, 200), feature("B", 300, 400)], 2, 0);
        assert_eq!(
            features_result(&over, &one).unwrap_err().code(),
            "invalid_response"
        );
        let wrong_page = viewer_result(vec![feature("A", 100, 200)], 5, 3);
        assert_eq!(
            features_result(&wrong_page, &query(100, 900))
                .unwrap_err()
                .code(),
            "invalid_response"
        );
        let mut inconsistent = viewer_result(vec![feature("A", 100, 200)], 1, 0);
        inconsistent.truncated = true;
        assert_eq!(
            features_result(&inconsistent, &query(100, 900))
                .unwrap_err()
                .code(),
            "invalid_response"
        );
    }

    #[test]
    fn gene_navigation_reports_the_interval_both_ways() {
        let result = gene_navigation_result(&AutomationGeneNavigationResult {
            panel_id: 2,
            contig: "chr17".to_owned(),
            start: 43_039_294,
            end: 43_130_483,
            hit: AutomationGeneHit {
                track_id: 3,
                name: "BRCA1".to_owned(),
                feature_type: "gene".to_owned(),
            },
            candidate_count: 4,
        });
        assert_eq!(
            result,
            json!({
                "panel_id": 2,
                "contig": "chr17",
                "coordinates": "zero-based; intervals are half-open [start, end)",
                "start": 43_039_294_u64,
                "end": 43_130_483_u64,
                "locus": "chr17:43039295-43130483",
                "hit": {"track_id": 3, "name": "BRCA1", "feature_type": "gene"},
                "candidate_count": 4,
            })
        );
    }
}
