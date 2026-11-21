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
        (Event::ReplaySaved { path: p(), duration_s: None }, r#"{"event":"replay_saved","path":"C:\\a b\\c.mp4"}"#),
        (
            Event::Status { engine_running: true, recording: false, paused: false, elapsed: 1.5, buffered_secs: 0.0, buffered_bytes: 0, restarts: 0 },
            r#"{"event":"status","engine_running":true,"recording":false,"paused":false,"elapsed":1.5,"buffered_secs":0.0,"buffered_bytes":0,"restarts":0}"#,
        ),
        (Event::Error { message: "boom".into() }, r#"{"event":"error","message":"boom"}"#),
    ];
    for (ev, want) in cases {
        assert_eq!(serde_json::to_string(&ev).unwrap(), want);
    }
}

#[test]
fn envelope_with_and_without_cid() {
    let e: Envelope<Command> = serde_json::from_str(r#"{"cmd":"stop","cid":"abc"}"#).unwrap();
    assert_eq!(e.cid.as_deref(), Some("abc"));
    assert!(matches!(e.body, Command::Stop));

    let e: Envelope<Command> = serde_json::from_str(r#"{"cmd":"stop"}"#).unwrap();
    assert!(e.cid.is_none());

    let e: Envelope<Command> =
        serde_json::from_str(r#"{"cmd":"save_replay","secs":30,"output_path":"o.mp4","cid":"z"}"#).unwrap();
    assert!(matches!(e.body, Command::SaveReplay { secs: 30, .. }));

    let s = serde_json::to_string(&Envelope { cid: Some("x".into()), body: Event::ReplayStopped }).unwrap();
    assert_eq!(s, r#"{"event":"replay_stopped","cid":"x"}"#);

    let s = serde_json::to_string(&Envelope { cid: None, body: Event::ReplayStopped }).unwrap();
    assert_eq!(s, r#"{"event":"replay_stopped"}"#);
}

fn sample_engine_args() -> EngineArgs {
    EngineArgs {
        ffmpeg_path: "ffmpeg".into(),
        source: crate::ffmpeg::CaptureSource::Gfxcapture,
        region: Region { x: 0, y: 0, width: 1920, height: 1080, monitor: 0 },
        audio_cfg: AudioConfig { mic_device: None, sys_device_id: Some("sys".into()), mic_device_id: None },
        enc_cfg: EncConfig { fps: 30, quality_crf: 23, format: "mp4".into(), hw_encoder: "libx264".into() },
        audio_pipe: Some("pipe".into()),
    }
}

#[test]
fn start_engine_round_trips() {
    let cmd = Command::StartEngine { args: sample_engine_args(), max_secs: 60, max_mb: 512 };
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.starts_with(r#"{"cmd":"start_engine","args":{"#), "{json}");
    assert!(json.contains(r#""source":"gfxcapture""#), "{json}");
    assert!(json.ends_with(r#""max_secs":60,"max_mb":512}"#), "{json}");
    match parse(&json) {
        Command::StartEngine { args, max_secs, max_mb } => {
            assert_eq!((max_secs, max_mb), (60, 512));
            assert_eq!(args.source, crate::ffmpeg::CaptureSource::Gfxcapture);
            assert_eq!(args.audio_pipe.as_deref(), Some("pipe"));
            assert_eq!(args.enc_cfg.fps, 30);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn v2_simple_commands_parse() {
    assert!(matches!(parse(r#"{"cmd":"stop_engine"}"#), Command::StopEngine));
    assert!(matches!(parse(r#"{"cmd":"stop_recording"}"#), Command::StopRecording));
    assert!(matches!(parse(r#"{"cmd":"set_limits","max_secs":30,"max_mb":100}"#), Command::SetLimits { max_secs: 30, max_mb: 100 }));
    match parse(r#"{"cmd":"start_recording","output_path":"o.mp4"}"#) {
        Command::StartRecording { output_path } => assert_eq!(output_path, "o.mp4"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn v2_events_serialize() {
    let cases = [
        (Event::EngineStarted, r#"{"event":"engine_started"}"#),
        (Event::EngineStopped, r#"{"event":"engine_stopped"}"#),
        (Event::EngineRestarted { reason: "died".into(), restarts: 2 }, r#"{"event":"engine_restarted","reason":"died","restarts":2}"#),
        (Event::ReplaySaved { path: "a".into(), duration_s: Some(12.5) }, r#"{"event":"replay_saved","path":"a","duration_s":12.5}"#),
        (Event::RecordingStarted { path: "a".into() }, r#"{"event":"recording_started","path":"a"}"#),
        (Event::RecordingStopped { path: "a".into() }, r#"{"event":"recording_stopped","path":"a"}"#),
    ];
    for (ev, want) in cases {
        assert_eq!(serde_json::to_string(&ev).unwrap(), want);
    }
    let ev: Event = serde_json::from_str(r#"{"event":"replay_saved","path":"a"}"#).unwrap();
    assert!(matches!(ev, Event::ReplaySaved { duration_s: None, .. }));
}

#[test]
fn status_event_shape() {
    assert_eq!(serde_json::to_string(&Event::Status { engine_running: true, recording: false, paused: false, elapsed: 0.0,
        buffered_secs: 59.5, buffered_bytes: 12_000_000, restarts: 1 }).unwrap(),
        r#"{"event":"status","engine_running":true,"recording":false,"paused":false,"elapsed":0.0,"buffered_secs":59.5,"buffered_bytes":12000000,"restarts":1}"#);
}
