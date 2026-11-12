//! Characterization tests: the IPC wire format exactly as the Tauri process
//! sends it today (commands.rs) and as the recorder binary replies.
use super::*;

fn parse(s: &str) -> Command {
    serde_json::from_str(s).expect(s)
}

#[test]
fn simple_commands_parse() {
    assert!(matches!(parse(r#"{"cmd":"stop"}"#), Command::Stop));
    assert!(matches!(parse(r#"{"cmd":"stop_replay"}"#), Command::StopReplay));
    assert!(matches!(parse(r#"{"cmd":"pause"}"#), Command::Pause));
    assert!(matches!(parse(r#"{"cmd":"resume"}"#), Command::Resume));
    assert!(matches!(parse(r#"{"cmd":"status"}"#), Command::Status));
}

#[test]
fn save_replay_parses() {
    match parse(r#"{"cmd":"save_replay","output_path":"C:\\a b\\c.mp4","secs":60}"#) {
        Command::SaveReplay { secs, output_path } => {
            assert_eq!(secs, 60);
            assert_eq!(output_path, r"C:\a b\c.mp4");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn start_replay_parses_full_args() {
    let json = r#"{"args":{"audio_cfg":{"mic_device":"Mic","mic_device_id":"mic-id","sys_device_id":"sys-id"},"audio_pipe":"\\\\.\\pipe\\cliplite_sysaudio_replay","enc_cfg":{"format":"mp4","fps":60,"hw_encoder":"h264_nvenc","quality_crf":28},"ffmpeg_path":"C:\\bin\\ffmpeg.exe","max_replay_buffer_size_mb":500,"output_path":"","region":{"height":1080,"monitor":0,"width":1920,"x":0,"y":0}},"cmd":"start_replay"}"#;
    match parse(json) {
        Command::StartReplay { args } => {
            assert_eq!(args.audio_pipe.as_deref(), Some(r"\\.\pipe\cliplite_sysaudio_replay"));
            assert_eq!(args.max_replay_buffer_size_mb, 500);
            assert_eq!((args.region.width, args.region.height), (1920, 1080));
            assert_eq!(args.enc_cfg.hw_encoder, "h264_nvenc");
            assert_eq!(args.audio_cfg.mic_device_id.as_deref(), Some("mic-id"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn legacy_start_without_mic_id_or_pipe_parses() {
    // commands.rs fallback path (~line 347) omits mic_device_id, audio_pipe and the size cap.
    let json = r#"{"args":{"audio_cfg":{"mic_device":null,"sys_device_id":null},"enc_cfg":{"format":"mp4","fps":30,"hw_encoder":"libx264","quality_crf":28},"ffmpeg_path":"ffmpeg","output_path":"C:\\o.mp4","region":{"height":1080,"monitor":0,"width":1920,"x":0,"y":0}},"cmd":"start"}"#;
    match parse(json) {
        Command::Start { args } => {
            assert!(args.audio_pipe.is_none());
            assert_eq!(args.max_replay_buffer_size_mb, 0);
            assert!(args.audio_cfg.mic_device_id.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn events_serialize() {
    let p = || r"C:\a b\c.mp4".to_string();
    let cases = [
        (Event::Started { path: p() }, r#"{"event":"started","path":"C:\\a b\\c.mp4"}"#),
        (Event::Stopped { path: p() }, r#"{"event":"stopped","path":"C:\\a b\\c.mp4"}"#),
        (Event::ReplayStarted { path: p() }, r#"{"event":"replay_started","path":"C:\\a b\\c.mp4"}"#),
        (Event::ReplayStopped, r#"{"event":"replay_stopped"}"#),
        (Event::ReplaySaved { path: p() }, r#"{"event":"replay_saved","path":"C:\\a b\\c.mp4"}"#),
        (
            Event::Status { is_recording: true, is_paused: false, elapsed: 1.5, replay_active: true },
            r#"{"event":"status","is_recording":true,"is_paused":false,"elapsed":1.5,"replay_active":true}"#,
        ),
        (Event::Error { message: "boom".into() }, r#"{"event":"error","message":"boom"}"#),
    ];
    for (ev, want) in cases {
        assert_eq!(serde_json::to_string(&ev).unwrap(), want);
    }
}
