//! The installer must ship the recorder: it is a separate crate now, so the
//! Tauri bundler no longer picks it up as a [[bin]] of the app package.
#[test]
fn bundle_overlay_ships_recorder_and_ffmpeg() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.bundle.json");
    let text = std::fs::read_to_string(path).expect("tauri.bundle.json (used by `npm run bundle`)");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let res = v["bundle"]["resources"].as_object().expect("resources map");
    assert!(res.iter().any(|(k, v)| k.ends_with("target/release/cliplite-recorder.exe") && v == "cliplite-recorder.exe"));
    assert!(res.contains_key("bin/*"), "ffmpeg/ffprobe must still be bundled");
}
