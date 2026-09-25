//! Embeds the icon and version information into the Windows exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "Snekkie")
        .set("FileDescription", "Snekkie - SSH and serial terminal")
        .set("LegalCopyright", "GPL-3.0-or-later")
        .set("OriginalFilename", "snekkie.exe");
    if let Err(e) = res.compile() {
        // A missing resource compiler shouldn't stop a debug build, but a
        // release without the icon would look broken, so fail those.
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            panic!("could not embed Windows resources: {e}");
        }
        println!("cargo:warning=could not embed Windows resources: {e}");
    }
}
