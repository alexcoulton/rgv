# Releasing RGV

RGV targets macOS, Linux, and Windows from the same locked Rust workspace. Releases that advertise
agent control must build and package both `rgv-desktop` and `rgv-mcp` from the same lockfile. A release candidate must
pass formatting, strict Clippy, workspace tests, dependency policy checks, the interaction budget,
and the fixture-matrix audit before packaging.

`packaging/stage_package.py` is the single cross-platform staging authority used by development and
signed workflows. It refuses a non-empty destination, writes the platform layout, embeds both RGV
license choices, a locked-graph CycloneDX 1.6 SBOM, third-party license inventory, build provenance,
and a SHA-256 package manifest, and verifies the result before returning. It uses only the Python
standard library and offline Cargo metadata after the release binary has built. Run its unit and
integration checks with:

```sh
python3 -m unittest discover -s packaging/tests -v
```

For example, an unsigned macOS staging check is:

```sh
python3 packaging/stage_package.py \
  --platform macos --architecture arm64 --version 0.1.0 --build-number 1 \
  --binary target/release/rgv-desktop --mcp-binary target/release/rgv-mcp \
  --destination /tmp/rgv-package-stage
```

## Artifacts

- macOS: universal or separate Apple Silicon and Intel `.app` bundles inside a signed and notarized
  `.dmg`.
- Windows: a signed x86-64 executable and WiX MSI.
- Linux: an x86-64 tarball plus AppImage; distribution packages can follow after beta. The release
  workflow pins and verifies the AppImage packaging tool before executing it.

Every platform artifact contains `LICENSE-MIT`, `LICENSE-APACHE`,
`THIRD_PARTY_LICENSES.txt`, `RGV.cdx.json`, `BUILD-PROVENANCE.json`, and
`PACKAGE-MANIFEST.sha256`. Platform-named SBOM and provenance copies are also published beside the
installers, and `SHA256SUMS` plus the GitHub/Sigstore attestation cover the final signed artifacts.
On macOS the inner manifest deliberately excludes the code-signed Mach-O and `_CodeSignature`
directory to avoid a signature/hash cycle; the signed DMG checksum and attestation cover them.

Never publish an unsigned artifact under a stable release tag. CI builds are explicitly development
artifacts until the signing jobs have access to protected secrets. macOS signing requires a
Developer ID Application certificate and notarization credentials. Windows signing requires an
EV/OV code-signing certificate exposed through the protected runner. Signing secrets must never be
placed in repository variables, logs, session files, or diagnostic bundles.

The `Signed release` workflow runs only for semantic-version tags. It fails closed if protected
Apple or Windows credentials are absent, signs and verifies both platform packages, submits macOS
disk images to Apple notarization, creates SHA-256 checksums, and emits GitHub/Sigstore build
provenance attestations. The `release` environment should require maintainer approval.

## Update strategy

RGV uses immutable signed GitHub Releases as its update channel. Until beta telemetry proves that
automatic replacement and rollback are safe, the application does not self-update: users compare
the installed version with the latest stable release, verify the platform signature and
`SHA256SUMS` or `gh attestation verify`, then install explicitly. A future updater must verify the
same signed artifact before replacing anything, preserve the prior binary for rollback, never run
with elevated privileges outside the installer, and honor an explicit disabled-by-default update
check setting. Session-schema compatibility is independent of application updates.

## Release checklist

1. Lock the release version and supported session schema in the changelog.
2. Run `cargo deny check`, `cargo test --workspace`, and strict Clippy on all target platforms;
   build `rgv-desktop` and `rgv-mcp` together when MCP is included.
3. Regenerate the alignment and interaction reports on the named benchmark machine.
4. Run the documented RGV-versus-IGV protocol and archive raw samples and fixture hashes.
5. Exercise BAM, CRAM, remote, corrupt-input, and mixed-track smoke sessions. For MCP-enabled
   packages, attach through stdio, inspect state, traverse review/history, exercise typed settings
   and opaque alignment selection, prove retained evidence/export stability, save/open in-process,
   capture by the MCP-reported PID, and verify that both binaries report a compatible control
   schema and application instance.
6. Build, sign, notarize where applicable, and install each artifact on a clean machine.
7. Verify that diagnostics remain opt-in and URLs/credentials are redacted.
8. Publish artifacts and checksums together, then test session migration from the previous release.

The manual `Beta qualification` workflow must pass first on a runner carrying the
`self-hosted,rgv-benchmark` labels. Its two inputs point to the pinned private corpus manifest and
matched IGV raw samples already present on that controlled machine. It enforces the complete
technology matrix, local and WAN budgets, and RGV-better-than-IGV p50/p95 before retaining one
combined report artifact.

The latest current-host unsigned staging evidence is recorded in
[`benchmark-results/PACKAGE_QUALIFICATION.md`](../benchmark-results/PACKAGE_QUALIFICATION.md). It
proves that the executable inside the staged arm64 macOS app completes deterministic automation;
it deliberately does not stand in for signing, notarization, clean installation, or actual Windows
and Linux package execution.
