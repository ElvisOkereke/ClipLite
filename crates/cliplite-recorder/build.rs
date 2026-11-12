//! Embed the same application manifest the UI process gets from tauri-build
//! (PerMonitorV2 DPI awareness, Windows 10 supportedOS, asInvoker). The
//! recorder spawns FFmpeg/ddagrab, whose capture rate depends on this context.
fn main() {
    println!("cargo:rerun-if-changed=cliplite-recorder.manifest");
    println!("cargo:rerun-if-changed=cliplite-recorder.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("cliplite-recorder.rc", embed_resource::NONE)
            .manifest_required()
            .expect("embed recorder manifest");
    }
}
