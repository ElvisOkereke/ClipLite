//! The UI process must emit exactly what the recorder parses.
use super::*;
use crate::recorder::ipc_envelope_line;
use cliplite_core::ipc;

#[test]
fn envelope_line_carries_cid_and_still_parses() {
    let line = ipc_envelope_line(&ipc::Command::SaveReplay { secs: 30, output_path: "o.mp4".into() }, "cid123");
    assert!(!line.contains('\n'));
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["cmd"], "save_replay");
    assert_eq!(v["cid"], "cid123");
    let env: ipc::Envelope<ipc::Command> = serde_json::from_str(&line).unwrap();
    assert!(matches!(env.body, ipc::Command::SaveReplay { secs: 30, .. }));
}

#[test]
fn simple_command_lines_match_recorder_literals() {
    let line = |c: ipc::Command| ipc_envelope_line(&c, "c");
    assert_eq!(line(ipc::Command::StopEngine), r#"{"cmd":"stop_engine","cid":"c"}"#);
    assert_eq!(line(ipc::Command::StopRecording), r#"{"cmd":"stop_recording","cid":"c"}"#);
    assert_eq!(line(ipc::Command::Pause), r#"{"cmd":"pause","cid":"c"}"#);
    assert_eq!(line(ipc::Command::Resume), r#"{"cmd":"resume","cid":"c"}"#);
    assert_eq!(line(ipc::Command::Status), r#"{"cmd":"status","cid":"c"}"#);
}

#[test]
fn received_reuses_given_cid_or_mints_one() {
    assert_eq!(received("save_replay", Some("abc".into())), "abc");
    assert_eq!(received("save_replay", None).len(), 32);
}

#[test]
fn ui_audio_fills_only_missing_devices() {
    let ui = AudioConfig { mic_device: Some("Mic".into()), sys_device_id: Some("sys".into()), mic_device_id: Some("mic".into()) };
    let mut s = Settings::default();
    assert!(adopt_ui_audio(&mut s, &ui));
    assert_eq!((s.sys_audio_device_id.as_deref(), s.mic_device_id.as_deref(), s.mic_device.as_deref()),
               (Some("sys"), Some("mic"), Some("Mic")));
    assert!(!adopt_ui_audio(&mut s, &ui), "second time: nothing to fill");

    let mut s = Settings { sys_audio_device_id: Some("saved".into()), ..Settings::default() };
    let other = AudioConfig { mic_device: None, sys_device_id: Some("ui".into()), mic_device_id: None };
    assert!(!adopt_ui_audio(&mut s, &other));
    assert_eq!(s.sys_audio_device_id.as_deref(), Some("saved"), "a persisted choice wins");
}

#[test]
fn removes_only_legacy_replay_files() {
    let dir = std::env::temp_dir().join(format!("cliplite legacy temp {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("engine")).unwrap();
    std::fs::write(dir.join("engine").join("cliplite_replay_9.mkv"), b"x").unwrap();
    std::fs::write(dir.join("cliplite_replay_123.mkv"), b"x").unwrap();
    std::fs::write(dir.join("other.mkv"), b"x").unwrap();
    std::fs::write(dir.join("cliplite_replay_1.txt"), b"x").unwrap();

    assert_eq!(remove_legacy_replay_files(&dir), 1);
    assert!(!dir.join("cliplite_replay_123.mkv").exists());
    assert!(dir.join("other.mkv").exists());
    assert!(dir.join("cliplite_replay_1.txt").exists());
    assert!(dir.join("engine").join("cliplite_replay_9.mkv").exists(), "engine/ untouched");
    assert_eq!(remove_legacy_replay_files(&dir.join("missing")), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
