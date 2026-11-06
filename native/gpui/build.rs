//! Embeds the app icon (native/installer/app.ico, made from build/icon.png) into the exe, so the taskbar, the
//! Start-menu shortcut and Apps & features show it. Windows only.
fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=../installer/app.ico");
    #[cfg(windows)]
    {
        let _ = embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional();
    }
}
