//! Embeds the executable icon and Windows version resource into
//! `hl2box-launcher.exe`. Runs only when the target is Windows; other targets
//! are untouched.
//!
//! The resource compiler (`rc.exe` from the Windows SDK on MSVC, `windres` on
//! GNU) is required. If it cannot run, the build fails with a message instead
//! of producing an executable without its icon.

use std::path::PathBuf;

/// The icon path relative to this crate. Resource paths are resolved from the
/// generated .rc file, so the icon is passed as an absolute path.
const ICON_FROM_CRATE: &str = "../../res/launcher.ico";

fn icon_path() -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    // no canonicalize(): on Windows it adds a \\?\ prefix the resource compiler rejects
    PathBuf::from(manifest)
        .join(ICON_FROM_CRATE)
        .to_string_lossy()
        .into_owned()
}

/// `MAJOR.MINOR.PATCH` -> the 16-bit-word packing the version resource wants.
fn pack_version(v: &str) -> u64 {
    let mut parts = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    (major << 48) | (minor << 32) | (patch << 16)
}

fn main() {
    println!("cargo:rerun-if-changed={ICON_FROM_CRATE}");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let packed = pack_version(&version);

    let icon = icon_path();
    let mut res = winres::WindowsResource::new();
    res.set_icon(&icon)
        .set_language(0x0409)
        .set("FileDescription", "hl2box launcher")
        .set("ProductName", "hl2box")
        .set("InternalName", "hl2box-launcher")
        .set("OriginalFilename", "hl2box-launcher.exe")
        .set("CompanyName", "hl2box")
        .set("LegalCopyright", "hl2box contributors. MIT licence.")
        .set("Comments", "Companion launcher for the hl2box Source mod.")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .set_version_info(winres::VersionInfo::FILEVERSION, packed)
        .set_version_info(winres::VersionInfo::PRODUCTVERSION, packed);

    // Keep the generated .rc next to the build output so it can be inspected.
    if let Ok(out) = std::env::var("OUT_DIR") {
        let rc: PathBuf = PathBuf::from(out).join("hl2box-launcher.rc");
        if let Err(e) = res.write_resource_file(&rc) {
            println!("cargo:warning=could not write {}: {e}", rc.display());
        }
    }

    if let Err(e) = res.compile() {
        panic!(
            "embedding the exe icon and version info failed: {e}\n\
             On Windows this needs the resource compiler (rc.exe from the Windows SDK, \
             installed with Visual Studio Build Tools)."
        );
    }
}
