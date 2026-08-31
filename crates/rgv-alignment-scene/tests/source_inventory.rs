use std::{collections::HashSet, fs, path::Path};

fn rust_sources(root: &Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).unwrap_or_else(|error| {
        panic!(
            "production source root {} is unreadable: {error}",
            root.display()
        )
    }) {
        let path = entry
            .expect("production source directory entry is readable")
            .path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

fn contains_identifier(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(start, _)| {
        let end = start + identifier.len();
        let identifier_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let left_is_boundary = start == 0 || !identifier_byte(source.as_bytes()[start - 1]);
        let right_is_boundary = end == source.len() || !identifier_byte(source.as_bytes()[end]);
        left_is_boundary && right_is_boundary
    })
}

#[test]
fn unified_alignment_scene_is_the_exclusive_production_authority() {
    let raw = include_str!("../../../validation/alignment-scene-source-inventory.json");
    let document: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(document["schema_version"], 2);
    assert_eq!(
        document["branch_policy"],
        "unified_alignment_scene_is_the_exclusive_alignment_authority"
    );

    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in document["deleted_files"].as_array().unwrap() {
        let relative = relative.as_str().unwrap();
        assert!(
            !repository.join(relative).exists(),
            "deleted legacy authority reappeared at {relative}"
        );
    }

    let mut sources = Vec::new();
    for root in document["production_roots"].as_array().unwrap() {
        rust_sources(&repository.join(root.as_str().unwrap()), &mut sources);
    }
    let source_text = sources
        .iter()
        .map(|path| {
            (
                path.strip_prefix(&repository).unwrap().to_owned(),
                fs::read_to_string(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();

    let mut forbidden = HashSet::new();
    for identifier in document["forbidden_identifiers"].as_array().unwrap() {
        let identifier = identifier.as_str().unwrap();
        assert!(
            forbidden.insert(identifier),
            "duplicate forbidden identifier {identifier}"
        );
        for (path, source) in &source_text {
            assert!(
                !contains_identifier(source, identifier),
                "legacy alignment identifier {identifier} remains in {}",
                path.display()
            );
        }
    }

    let mut retained = HashSet::new();
    for boundary in document["retained_boundaries"].as_array().unwrap() {
        let identifier = boundary["identifier"].as_str().unwrap();
        assert!(
            retained.insert(identifier),
            "duplicate retained boundary {identifier}"
        );
        assert!(!boundary["purpose"].as_str().unwrap().is_empty());
        let relative = boundary["path"].as_str().unwrap();
        let source = fs::read_to_string(repository.join(relative))
            .unwrap_or_else(|error| panic!("retained boundary {relative} is unreadable: {error}"));
        assert!(
            contains_identifier(&source, identifier),
            "retained boundary {identifier} is absent from {relative}"
        );
    }
    assert!(forbidden.is_disjoint(&retained));
}
