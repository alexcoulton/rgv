#[test]
fn scene_authority_remains_headless_and_independent_of_legacy_presentation() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in [
        "wgpu",
        "winit",
        "egui",
        "rgv-render",
        "rgv-layout",
        "rgv-engine",
        "rgv-desktop",
    ] {
        assert!(
            !manifest.lines().any(|line| {
                line.split_once('=')
                    .is_some_and(|(name, _)| name.trim() == forbidden)
            }),
            "the semantic scene authority must not depend on {forbidden}"
        );
    }
}
