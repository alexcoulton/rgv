# Package qualification

Status date: 23 August 2026

Status: **unsigned macOS development package passed; signed cross-platform release qualification
remains**.

## Current-platform result

After the Phase 8 feature freeze and invariant audit, the optimized `arm64` desktop binary was
rebuilt and staged through the same `stage_package.py` path used by the release workflows. The
resulting application was 31 MiB and contained:

- a native arm64 Mach-O executable and valid `Info.plist`;
- `LICENSE-MIT` and `LICENSE-APACHE`;
- a 932 KiB deduplicated third-party license/attribution bundle;
- a locked-graph CycloneDX 1.6 SBOM;
- build provenance and the project README; and
- a SHA-256 manifest covering every immutable bundle resource.

The manifest and metadata checker passed after staging and again after archiving and unpacking the
application. The executable inside the staged `.app`, not the source binary under
`target/release`, then ran the nineteen-command deterministic two-panel automation batch through
Metal on the Apple M4. All nineteen commands succeeded. The saved schema-v3 project retained two
ordered panels, active panel 2, stable alignment track 1, strand grouping, base sorting, and the
expected short/long-read source. Every structured export was non-empty. The visible and full-panel
SVG hashes remained exactly:

```text
ea837d45815dba8ed16e73f3ed65f536c6010a9b9fcb76e534703033ec4d54f9  automation-smoke.svg
027ecb709c3432fc93db082ffe00154c797cf976054ab3bf7cfcc6e5e57574df  automation-full-panel.svg
```

The structured result is `automation-packaged-macos-report.json`. The checked-in strict verifier
accepted the report, all nine generated artifacts, both reviewed hashes, saved project semantics,
and native telemetry, then wrote `automation-packaged-macos-verification.json`. That verification
record pins the qualification-manifest, batch, automation-report, and individual artifact digests
without retaining their absolute filesystem paths.

The packaged run also recorded native-window telemetry rather than inferring it from a headless
display list:

| Measurement | Result |
| --- | ---: |
| Startup to first presented frame | 1,677.13 ms |
| Startup to first settled analytical frame | 1,816.42 ms |
| Presented-frame median | 7.19 ms |
| Presented-frame p95 | 10.24 ms |
| Maximum presented frame | 23.14 ms |
| Presented frames / dropped samples | 31 / 0 |
| GPU uploaded frames / bytes | 2 / 263,904 |
| Peak instances / truncated frames | 5,497 / 0 |
| GPU buffer reallocations | 0 |

This is a deterministic automation acceptance run, not a matched cold-start benchmark. The startup
numbers are retained as evidence and are not a reason for speculative tuning before the user's
ordinary-use pass.

The sealed unsigned development archive was 13,668,041 bytes. Its identities were:

```text
ab555680399bcd58e4a8de25fbc3d4e66ba106a7f1f3d05cd2b20abdca989b11  RGV-macos-arm64-development-unsigned.zip
9d50fcf85c81526db0b5643402662a0b039e03453448b5abd575861152268a61  RGV.app/Contents/MacOS/rgv-desktop
edbd52bd0e78ec29f155153132256ddc85711be6a340d3bf0a8c044ed2dba151  RGV.app/Contents/Resources/PACKAGE-MANIFEST.sha256
```

This archive is a qualification artifact in temporary local storage, not a signed distributable.
The embedded provenance records the Cargo.lock digest, complete toolchain description, binary
digest, SBOM digest, target, and creation time; the source commit is intentionally null because the
current workspace has no committed revision to claim.

## Commands

```text
python3 -m unittest discover -s packaging/tests -v                 4 tests passed
python3 packaging/stage_package.py --platform macos --architecture arm64 \
  --version 0.1.0 --build-number 2 --binary target/release/rgv-desktop \
  --destination /private/tmp/rgv-phase9-macos.2yp93Y
/private/tmp/rgv-phase9-macos.2yp93Y/RGV.app/Contents/MacOS/rgv-desktop \
  --batch validation/automation-smoke.json \
  --automation-report benchmark-results/automation-packaged-macos-report.json
cargo run --release -p rgv-validation-runner \
  --bin rgv-automation-qualification -- \
  validation/automation-smoke-qualification.json \
  benchmark-results/automation-packaged-macos-report.json \
  --output benchmark-results/automation-packaged-macos-verification.json
shasum -a 256 -c Contents/Resources/PACKAGE-MANIFEST.sha256
ditto -c -k --sequesterRsrc --keepParent RGV.app \
  RGV-macos-arm64-development-unsigned.zip
ditto -x -k RGV-macos-arm64-development-unsigned.zip <fresh-directory>
```

## Remaining release package gates

- Sign the final macOS application and DMG, verify the signature, notarize, staple, and install it
  on a clean supported Mac.
- Build the actual Windows x86-64 payload on Windows, stage it through the common path, produce and
  sign the MSI, then test clean install, upgrade, and uninstall.
- Build the actual Linux x86-64 payload on Linux, stage it through the common path, produce the
  tarball and AppImage, then test on the declared distributions and GPU/scaling configurations.
- Verify sleep/wake, device loss, multiple displays, repeated shutdown under work, accessibility,
  schema migration, cache placement, and the one-hour soak using the exact signed artifacts.

The cross-platform staging layouts and their common metadata/manifest behavior are protected by
four host-independent packaging tests, but that is not evidence that the actual Windows and Linux
executables install or run correctly.
