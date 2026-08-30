use std::collections::HashSet;

#[test]
fn feature_matrix_is_machine_readable_unique_and_covers_product_consumers() {
    let raw = include_str!("../../../validation/alignment-scene-feature-matrix.json");
    let document: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["cutover_policy"],
        "all_alignment_consumers_together"
    );
    assert_eq!(document["visual_acceptance"], "manual_user_review");

    let items = document["items"].as_array().unwrap();
    let ids = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), items.len(), "feature IDs must be unique");

    for required in [
        "consumer.picking",
        "consumer.hover_selection",
        "consumer.inspector",
        "consumer.export",
        "consumer.automation",
        "consumer.mcp",
        "consumer.properties_diagnostics",
        "summary.coverage",
        "summary.junctions",
        "summary.modifications",
        "legacy.complete_horizon",
        "legacy.panel_texture",
    ] {
        assert!(ids.contains(required), "missing cutover item {required}");
    }

    assert!(items.iter().all(|item| {
        item["gate"]
            .as_u64()
            .is_some_and(|gate| (1..=10).contains(&gate))
    }));
}
