use std::{collections::HashSet, fs, path::Path};

#[test]
fn source_inventory_is_unique_resolvable_and_schedules_legacy_deletion() {
    let raw = include_str!("../../../validation/alignment-scene-source-inventory.json");
    let document: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["branch_policy"],
        "legacy_is_oracle_only_until_atomic_cutover"
    );

    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entries = document["entries"].as_array().unwrap();
    let mut symbols = HashSet::with_capacity(entries.len());
    for entry in entries {
        let symbol = entry["symbol"].as_str().unwrap();
        assert!(
            symbols.insert(symbol),
            "duplicate source inventory symbol {symbol}"
        );
        let phase = entry["phase"].as_u64().unwrap();
        assert!(
            (1..=9).contains(&phase),
            "invalid migration phase for {symbol}"
        );
        assert!(!entry["replacement"].as_str().unwrap().is_empty());

        let relative = entry["path"].as_str().unwrap();
        let source = fs::read_to_string(repository.join(relative))
            .unwrap_or_else(|error| panic!("inventory path {relative} is unreadable: {error}"));
        let leaf = symbol.rsplit("::").next().unwrap();
        assert!(
            source.contains(leaf),
            "inventory symbol {symbol} is absent from {relative}"
        );
    }

    for required in [
        "rgv_engine::AlignmentResidentStore",
        "rgv_layout::AlignmentRowScene",
        "AlignmentPanelProjection::current_query",
        "AlignmentPanelProjection::layout_query",
        "AlignmentPanelProjection::resident_query",
        "AlignmentPanelProjection::cached_layout",
        "rgv_desktop::NavigationPreviewKind::CompleteHorizon",
        "rgv_render::NavigationPreviewRenderer",
    ] {
        assert!(
            symbols.contains(required),
            "missing deletion-map symbol {required}"
        );
    }
}
