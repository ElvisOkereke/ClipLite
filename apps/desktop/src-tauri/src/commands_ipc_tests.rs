//! The UI process must emit exactly what the recorder parses.
use super::*;
use cliplite_core::ipc;

/// Bare command line without an envelope (the pre-telemetry wire format).
fn ipc_line(cmd: &ipc::Command) -> String {
    serde_json::to_string(cmd).unwrap()
}

fn ui_args() -> (Region, AudioConfig, EncConfig) {
    (
        Region { x: 10, y: 20, width: 2560, height: 1440, monitor: 1 },
        AudioConfig { mic_device: Some("Mic".into()), sys_device_id: Some("sys".into()), mic_device_id: Some("mic".into()) },
        EncConfig { fps: 60, quality_crf: None, format: None, hw_encoder: None },
    )
}

#[test]
fn start_args_apply_ui_defaults() {
    let (r, a, e) = ui_args();
    let args = to_start_args(std::path::Path::new(r"C:\a b\ffmpeg.exe"), r"C:\o.mp4", &r, &a, &e, None, 0);
    assert_eq!(args.ffmpeg_path, r"C:\a b\ffmpeg.exe");
    assert_eq!((args.region.x, args.region.y, args.region.width, args.region.height, args.region.monitor), (10, 20, 2560, 1440, 1));
    assert_eq!(args.audio_cfg.mic_device_id.as_deref(), Some("mic"));
    assert_eq!((args.enc_cfg.quality_crf, args.enc_cfg.format.as_str(), args.enc_cfg.hw_encoder.as_str()), (28, "mp4", "h264_nvenc"));
    assert!(args.audio_pipe.is_none());
}

#[test]
fn start_replay_line_round_trips_through_recorder_types() {
    let (r, a, e) = ui_args();
    let args = to_start_args(std::path::Path::new("ffmpeg"), "", &r, &a, &e, Some(REPLAY_AUDIO_PIPE), 500);
    let line = ipc_line(&ipc::Command::StartReplay { args });
    assert!(!line.contains('\n'));
    match serde_json::from_str::<ipc::Command>(&line).unwrap() {
        ipc::Command::StartReplay { args } => {
            assert_eq!(args.audio_pipe.as_deref(), Some(r"\\.\pipe\cliplite_sysaudio_replay"));
            assert_eq!(args.max_replay_buffer_size_mb, 500);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn simple_command_lines_match_legacy_literals() {
    assert_eq!(ipc_line(&ipc::Command::Stop), r#"{"cmd":"stop"}"#);
    assert_eq!(ipc_line(&ipc::Command::StopReplay), r#"{"cmd":"stop_replay"}"#);
    assert_eq!(ipc_line(&ipc::Command::Pause), r#"{"cmd":"pause"}"#);
    assert_eq!(ipc_line(&ipc::Command::Resume), r#"{"cmd":"resume"}"#);
}

#[test]
fn envelope_line_carries_cid_and_still_parses() {
    let line = ipc_envelope_line(&ipc::Command::SaveReplay { secs: 30, output_path: "o.mp4".into() }, "cid123");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["cmd"], "save_replay");
    assert_eq!(v["cid"], "cid123");
    let env: ipc::Envelope<ipc::Command> = serde_json::from_str(&line).unwrap();
    assert!(matches!(env.body, ipc::Command::SaveReplay { secs: 30, .. }));
}

#[test]
fn received_reuses_given_cid_or_mints_one() {
    assert_eq!(received("save_replay", Some("abc".into())), "abc");
    assert_eq!(received("save_replay", None).len(), 32);
}
