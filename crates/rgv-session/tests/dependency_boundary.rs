//! ARCH-2: the session is headless. Its normal dependency graph, resolved from the workspace
//! manifests and `Cargo.lock`, must not reach a window system, a widget toolkit, a GPU API or a
//! native file dialog. The graph walk mirrors
//! `crates/rgv-alignment-scene/tests/dependency_boundary.rs`.

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
    let lock = fs::read_to_string(repository.join("Cargo.lock")).expect("Cargo.lock is readable");
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

#[test]
fn the_session_cannot_reach_a_window_gpu_widget_or_dialog_crate() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let closure = normal_closure(&repository, &repository.join("crates/rgv-session"));
    for expected in ["rgv-core", "rgv-engine", "rgv-automation"] {
        assert!(
            closure.contains(expected),
            "the normal dependency graph of rgv-session was not resolved ({expected} is missing)"
        );
    }
    for forbidden in [
        "winit",
        "egui",
        "egui-winit",
        "egui-wgpu",
        "epaint",
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "rfd",
        "rgv-render",
        "rgv-desktop",
    ] {
        assert!(
            !closure.contains(forbidden),
            "rgv-session must not have {forbidden} in its normal dependency graph"
        );
    }
}

/// The walk itself must be able to see the forbidden crates, or the test above proves nothing.
#[test]
fn the_graph_walk_finds_the_window_stack_from_the_desktop() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let desktop = normal_closure(&repository, &repository.join("apps/rgv-desktop"));
    for present in ["winit", "egui", "wgpu", "rfd", "rgv-session"] {
        assert!(
            desktop.contains(present),
            "{present} is absent from the desktop graph"
        );
    }
}
