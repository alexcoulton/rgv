#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) struct QualificationOutput {
    suite: PathBuf,
    validation: PathBuf,
}

impl Drop for QualificationOutput {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.suite);
        let _ = fs::remove_dir_all(&self.validation);
    }
}

pub(crate) fn qualify_generation(root: &Path) -> Result<()> {
    let suffix = std::process::id();
    let output = QualificationOutput {
        suite: root.join(format!(
            "benchmark-data/manual-suite-qualification-{suffix}"
        )),
        validation: root.join(format!("validation/manual-suite-qualification-{suffix}")),
    };
    ensure!(
        !output.suite.exists() && !output.validation.exists(),
        "qualification output already exists; remove only the named stale qualification directories"
    );
    generate(root, &output.suite, &output.validation)?;
    let first = declared_tree_digest(root, &output.suite, &output.validation)?;
    generate(root, &output.suite, &output.validation)?;
    let second = declared_tree_digest(root, &output.suite, &output.validation)?;
    ensure!(first == second, "two clean fixture generations differed");

    let tampered = output.suite.join("data/tracks/default-annotations.bed");
    fs::OpenOptions::new()
        .append(true)
        .open(&tampered)?
        .write_all(b"# controlled tamper\n")?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted changed bytes"
    );
    generate(root, &output.suite, &output.validation)?;
    let undeclared = output.suite.join("failures/UNDECLARED.stale");
    write_text(&undeclared, "controlled stale output\n")?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted undeclared stale output"
    );
    fs::remove_file(&undeclared)?;
    fs::remove_file(&tampered)?;
    ensure!(
        verify_manual_suite(
            root,
            &output.suite.join("manifest.json"),
            &output.validation.join("coverage-catalog.json"),
            false,
        )
        .is_err(),
        "fixture verification accepted a missing declared artifact"
    );
    println!(
        "manual fixture generation qualification passed: deterministic tree {first}, tamper/stale/missing rejection, relocation, production semantics, and size budget"
    );
    Ok(())
}

pub(crate) fn declared_tree_digest(root: &Path, suite: &Path, validation: &Path) -> Result<String> {
    let manifest: FixtureManifest = read_json(&suite.join("manifest.json"))?;
    let mut lines = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            format!(
                "artifact\t{}\t{}\t{}",
                artifact.id, artifact.path, artifact.sha256
            )
        })
        .chain(manifest.dependencies.iter().map(|dependency| {
            format!(
                "dependency\t{}\t{}\t{}",
                dependency.id, dependency.path, dependency.sha256
            )
        }))
        .collect::<Vec<_>>();
    lines.push(format!(
        "manifest\t{}",
        sha256_file(&suite.join("manifest.json"))?
    ));
    lines.push(format!(
        "catalog\t{}",
        sha256_file(&validation.join("coverage-catalog.json"))?
    ));
    lines.sort();
    ensure!(
        suite.starts_with(root) && validation.starts_with(root),
        "qualification outputs escaped the workspace"
    );
    Ok(sha256_bytes(lines.join("\n").as_bytes()))
}
