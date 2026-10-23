// Windows executable resources: the launcher icon and the version block
// the Explorer properties dialog shows. Non-Windows targets build without
// this step. The .ico lives with the engine launcher sources it is shared
// with (../../res/launcher.ico).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../res/launcher.ico");
        let version = env!("CARGO_PKG_VERSION").replace('-', ".");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../res/launcher.ico");
        res.set("FileDescription", "hl2box launcher");
        res.set("ProductName", "hl2box launcher");
        res.set("OriginalFilename", "hl2box-launcher.exe");
        res.set("CompanyName", "hl2sb");
        res.set("LegalCopyright", "Public domain content, MIT glue code");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set("InternalName", "hl2box-launcher");
        if let Err(e) = res.compile() {
            // A missing rc.exe must not fail the whole build; the exe just
            // keeps the default icon. The toolchain warning makes it visible.
            println!("cargo:warning=windows resource compile skipped: {e}");
        }
    }
}
