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

mod normal_dependency_graph {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::{Path, PathBuf},
    };

    /// One `[dependencies]` entry of a workspace manifest: the package it names and, for a
    /// workspace crate, the directory holding that crate's manifest.
    struct ManifestDependency {
        package: String,
        path: Option<PathBuf>,
    }

    fn quoted_value(line: &str, key: &str) -> Option<String> {
        let start = line.find(&format!("{key} = \""))? + key.len() + 4;
        let end = line[start..].find('"')?;
        Some(line[start..start + end].to_owned())
    }

    /// The normal dependencies a manifest declares: `[dependencies]` and every
    /// `[target.….dependencies]` table, never `dev-dependencies` or `build-dependencies`.
    fn normal_dependencies(crate_directory: &Path) -> Vec<ManifestDependency> {
        let manifest_path = crate_directory.join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap_or_else(|error| panic!("{} is unreadable: {error}", manifest_path.display()));
        let mut in_normal_table = false;
        let mut dependencies = Vec::new();
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                in_normal_table = line == "[dependencies]"
                    || (line.starts_with("[target.") && line.ends_with(".dependencies]"));
                continue;
            }
            if !in_normal_table || line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let name = key.trim().trim_end_matches(".workspace").trim();
            dependencies.push(ManifestDependency {
                package: quoted_value(line, "package").unwrap_or_else(|| name.to_owned()),
                path: quoted_value(line, "path").map(|path| crate_directory.join(path)),
            });
        }
        dependencies
    }

    /// Package name to the names of everything any locked version of it depends on. Cargo
    /// resolves only normal and build dependencies for packages outside the workspace, so
    /// following these edges from a third-party crate never enters a dev-dependency.
    fn locked_dependencies(repository: &Path) -> BTreeMap<String, BTreeSet<String>> {
        let lock =
            fs::read_to_string(repository.join("Cargo.lock")).expect("Cargo.lock is readable");
        let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
        let mut package = None::<String>;
        let mut in_dependencies = false;
        for line in lock.lines().map(str::trim) {
            if line == "[[package]]" {
                package = None;
                in_dependencies = false;
            } else if let Some(name) = quoted_value(line, "name") {
                graph.entry(name.clone()).or_default();
                package = Some(name);
            } else if line == "dependencies = [" {
                in_dependencies = true;
            } else if line == "]" {
                in_dependencies = false;
            } else if in_dependencies {
                let dependency = line
                    .trim_matches(|character| character == '"' || character == ',')
                    .split(' ')
                    .next()
                    .expect("a locked dependency names a package");
                graph
                    .get_mut(
                        package
                            .as_ref()
                            .expect("dependencies follow a package name"),
                    )
                    .expect("the package was registered")
                    .insert(dependency.to_owned());
            }
        }
        graph
    }

    /// Every package in the normal dependency graph of the workspace crate at `crate_directory`.
    fn normal_closure(repository: &Path, crate_directory: &Path) -> BTreeSet<String> {
        let locked = locked_dependencies(repository);
        let mut closure = BTreeSet::new();
        let mut workspace_crates = vec![crate_directory.to_owned()];
        let mut external = Vec::new();
        while let Some(directory) = workspace_crates.pop() {
            for dependency in normal_dependencies(&directory) {
                if !closure.insert(dependency.package.clone()) {
                    continue;
                }
                match dependency.path {
                    Some(path) => workspace_crates.push(path),
                    None => external.push(dependency.package),
                }
            }
        }
        while let Some(package) = external.pop() {
            let dependencies = locked
                .get(&package)
                .unwrap_or_else(|| panic!("{package} is absent from Cargo.lock"));
            for dependency in dependencies {
                if closure.insert(dependency.clone()) {
                    external.push(dependency.clone());
                }
            }
        }
        closure
    }

    /// ARCH-8: the data model, layout, the scene authority and the renderer consume plain record
    /// types from `rgv-model`. None of them may link the I/O crate or what only it needs: htslib
    /// (a C toolchain), the HTTP client, or the alignment and `BigWig` decoders.
    #[test]
    fn model_layout_scene_and_render_do_not_link_the_io_stack() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in [
            "rgv-model",
            "rgv-layout",
            "rgv-alignment-scene",
            "rgv-render",
        ] {
            let closure = normal_closure(&repository, &repository.join("crates").join(name));
            assert!(
                closure.contains("rgv-core"),
                "the normal dependency graph of {name} was not resolved"
            );
            for forbidden in [
                "rgv-hts",
                "rust-htslib",
                "hts-sys",
                "reqwest",
                "noodles",
                "noodles-bam",
                "noodles-bgzf",
                "noodles-cram",
                "bigtools",
            ] {
                assert!(
                    !closure.contains(forbidden),
                    "{name} must not have {forbidden} in its normal dependency graph"
                );
            }
        }
    }

    /// ARCH-8: the automation protocol and stress tooling build on the domain crate, never the
    /// reverse, and the data model does not know about either.
    #[test]
    fn domain_and_model_crates_do_not_depend_on_the_automation_protocol() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in ["rgv-core", "rgv-model"] {
            let closure = normal_closure(&repository, &repository.join("crates").join(name));
            assert!(
                !closure.contains("rgv-automation"),
                "{name} must not depend on rgv-automation"
            );
        }
        let automation = normal_closure(&repository, &repository.join("crates/rgv-automation"));
        assert!(automation.contains("rgv-core"));
        assert!(!automation.contains("rgv-hts"));
    }

    /// The model crate is pure Rust data: it may depend on the domain crate and light derive
    /// helpers only, so it stays buildable for targets without a C toolchain or a network stack.
    #[test]
    fn model_crate_declares_only_light_dependencies() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let declared = normal_dependencies(&repository.join("crates/rgv-model"))
            .into_iter()
            .map(|dependency| dependency.package)
            .collect::<BTreeSet<_>>();
        let allowed = ["rgv-core", "serde", "thiserror"];
        for dependency in &declared {
            assert!(
                allowed.contains(&dependency.as_str()),
                "rgv-model must not depend on {dependency}"
            );
        }
        assert!(declared.contains("rgv-core"));
    }
}
