//! Embeds the app icon (native/installer/app.ico, made from native/installer/icon.png) into the exe, so the taskbar, the
//! Start-menu shortcut and Apps & features show it. Windows only.
fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=../installer/app.ico");
    // host cfg (the build-dependency exists only on a Windows host) and target check (cross builds to macOS)
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let _ = embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional();
    }
}
