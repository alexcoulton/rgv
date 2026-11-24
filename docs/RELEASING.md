# Releasing RGV

RGV targets macOS, Linux, and Windows from the same locked Rust workspace. Releases that advertise
agent control must build and package both `rgv-desktop` and `rgv-mcp` from the same lockfile. A release candidate must
pass formatting, strict Clippy, workspace tests, dependency policy checks, the interaction budget,
and the fixture-matrix audit before packaging.

`packaging/stage_package.py` is the single cross-platform staging authority used by the release
workflow. It refuses a non-empty destination, writes the platform layout, embeds both RGV
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

Releases are unsigned: there is no Apple Developer ID or Windows code-signing certificate.

- macOS: separate Apple Silicon and Intel `.app` bundles, ad-hoc signed, inside a `.dmg`. Ad-hoc
  signing lets Apple Silicon run the app and makes Gatekeeper offer "Open Anyway" (System
  Settings → Privacy & Security) instead of reporting it as damaged.
- Windows: a WiX MSI that installs to Program Files and adds a Start menu shortcut, plus a
  portable `.zip`. SmartScreen asks once ("More info → Run anyway"). Built for `x86_64-pc-windows-gnu`
  with MSYS2's UCRT64 MinGW toolchain (`.github/actions/windows-mingw`), because HTSlib does
  not build with MSVC; the executables need only DLLs that Windows 10 and 11 provide. HTSlib and
  its Rust bindings are vendored with small Windows patches (`vendor/README.md`).
- Linux: an x86-64 tarball plus AppImage; distribution packages can follow after beta. The release
  workflow pins and verifies the AppImage packaging tool before executing it.

Every platform artifact contains `LICENSE-MIT`, `LICENSE-APACHE`,
`THIRD_PARTY_LICENSES.txt`, `RGV.cdx.json`, `BUILD-PROVENANCE.json`, and
`PACKAGE-MANIFEST.sha256`. Platform-named SBOM and provenance copies are also published beside the
installers, with `SHA256SUMS` covering every published file. On macOS the inner manifest
deliberately excludes the code-signed Mach-O and `_CodeSignature` directory to avoid a
signature/hash cycle.

## Release workflow

`.github/workflows/release.yml` builds every package. Pushing a tag `vX.Y.Z` builds them and
publishes a GitHub release with installation notes and generated change notes. Running it by
hand (`gh workflow run release.yml --ref <branch>`, or "Run workflow" on the Actions page) is a dry
run: the packages are attached to the run as artifacts and nothing is published.

## Update strategy

RGV uses GitHub Releases as its update channel. Until beta telemetry proves that automatic
replacement and rollback are safe, the application does not self-update: users compare the
installed version with the latest release, verify `SHA256SUMS`, then install explicitly. A future updater must verify the
same signed artifact before replacing anything, preserve the prior binary for rollback, never run
with elevated privileges outside the installer, and honor an explicit disabled-by-default update
check setting. Session-schema compatibility is independent of application updates.

## Build profiles

`cargo build --release` uses thin LTO, one codegen unit, and strips debug information; it writes
to `target/release/`. The release workflow (`.github/workflows/release.yml`) builds with
`--profile dist`, which inherits `release`
and switches to fat LTO; its binaries are in `target/dist/`. Both keep `panic = "unwind"`. The
`dist` profile was added on 20 November 2026 and has not yet been exercised by a tagged release.
All workflows install the toolchain pinned in `rust-toolchain.toml`.

## Release checklist

1. Lock the release version and supported session schema in the changelog.
2. Run `cargo deny check`, `cargo test --workspace`, and strict Clippy on all target platforms;
   build `rgv-desktop` and `rgv-mcp` together when MCP is included.
3. Regenerate the alignment and interaction reports on the named benchmark machine.
4. Run the documented RGV-versus-IGV protocol and archive raw samples and fixture hashes.
5. Exercise BAM, CRAM, remote, corrupt-input, and mixed-track smoke sessions. For MCP-enabled
   packages, attach through stdio, inspect state, traverse review/history, exercise typed settings
   and opaque alignment selection, prove retained evidence/export stability, save/open in-process,
   background native-window capture through the MCP, and verify that both binaries report a compatible control
   schema and application instance.
6. Build each package with a dry run of the release workflow and install it on a clean machine.
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
