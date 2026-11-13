//! The recorder must carry the same application manifest as the UI process
//! (PerMonitorV2 DPI awareness, Windows 10 supportedOS, asInvoker): the
//! restructure into its own crate silently dropped it.
#[test]
#[cfg(windows)]
fn recorder_exe_embeds_app_manifest() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_cliplite-recorder")).unwrap();
    let has = |needle: &[u8]| exe.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"PerMonitorV2"), "missing PerMonitorV2 DPI awareness");
    assert!(has(b"{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"), "missing Windows 10 supportedOS");
    assert!(has(b"asInvoker"), "missing requestedExecutionLevel asInvoker");
}
