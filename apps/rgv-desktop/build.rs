//! Embeds the application icon and version information in the Windows executable, so File
//! Explorer, the taskbar and Task Manager show RGV's icon and name. Other targets need nothing:
//! macOS takes the icon from the app bundle, Linux from the desktop entry.
//!
//! Windows builds use the MinGW target (HTSlib does not build with MSVC), so the resource script
//! is compiled with MinGW's `windres` into an object linked into the executable. `WINDRES` names
//! the tool when it is not on the path as `x86_64-w64-mingw32-windres` or `windres`.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        println!("cargo:warning=the Windows icon is embedded only for the MinGW (gnu) target");
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let icon = manifest.join("../../packaging/icons/rgv.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets it"));
    let script = out.join("rgv.rc");
    let version = env::var("CARGO_PKG_VERSION").expect("cargo sets it");
    fs::write(&script, resource_script(&icon, &version)).expect("OUT_DIR is writable");
    let object = out.join("rgv-resources.o");
    let windres = windres();
    let status = Command::new(&windres)
        .arg("--input")
        .arg(&script)
        .arg("--output")
        .arg(&object)
        .arg("--output-format=coff")
        .status()
        .unwrap_or_else(|error| panic!("could not run {windres}: {error}"));
    assert!(
        status.success(),
        "{windres} failed to compile {}",
        script.display()
    );
    println!("cargo:rustc-link-arg-bins={}", object.display());
}

/// The first resource compiler that runs: `WINDRES`, then MinGW's usual names.
fn windres() -> String {
    env::var("WINDRES")
        .ok()
        .into_iter()
        .chain([
            "x86_64-w64-mingw32-windres".to_owned(),
            "windres".to_owned(),
        ])
        .find(|tool| {
            Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("no windres found: install MinGW binutils or set WINDRES")
}

/// The icon (resource 1, which Windows shows for the executable) and a version block.
fn resource_script(icon: &Path, version: &str) -> String {
    // Forward slashes need no escaping in a resource-script string, and windres accepts them.
    let icon = icon.display().to_string().replace('\\', "/");
    let mut numbers = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let [major, minor, patch] = [(); 3].map(|()| numbers.next().unwrap_or(0));
    format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "RGV Project"
      VALUE "FileDescription", "RGV - Rust Genome Viewer"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rgv-desktop"
      VALUE "OriginalFilename", "rgv-desktop.exe"
      VALUE "ProductName", "RGV"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
