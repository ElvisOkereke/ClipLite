use super::*;
use cliplite_core::ipc::{Command, Event};
use std::sync::mpsc;
use std::time::Duration;

fn status(engine_running: bool, recording: bool) -> Event {
    Event::Status { engine_running, recording, paused: recording, elapsed: 4.5,
                    buffered_secs: 59.5, buffered_bytes: 1, restarts: 2 }
}

#[test]
fn status_maps_engine_status() {
    let s = RecordingStatus::from_event(&status(true, true), true, Some("r.mp4".into())).unwrap();
    assert!(s.is_recording && s.is_paused && s.replay_active);
    assert_eq!((s.elapsed_seconds, s.buffered_secs, s.restarts), (4.5, 59.5, 2));
    assert_eq!(s.output_path.as_deref(), Some("r.mp4"));

    let s = RecordingStatus::from_event(&status(true, false), false, None).unwrap();
    assert!(!s.replay_active, "engine kept alive for a recording, buffer marked off");
    assert_eq!(s.elapsed_seconds, 0.0);
    let s = RecordingStatus::from_event(&status(false, false), true, None).unwrap();
    assert!(!s.replay_active, "no engine, no buffer");
    assert!(RecordingStatus::from_event(&Event::EngineStarted, true, None).is_none());

    let json = serde_json::to_value(&s).unwrap();
    for k in ["is_recording", "is_paused", "elapsed_seconds", "output_path", "replay_active", "buffered_secs", "restarts"] {
        assert!(json.get(k).is_some(), "RecordingStatus JSON lacks {k}: {json}");
    }
}

#[test]
fn route_line_matches_cid_else_unsolicited() {
    let pending: Pending = Default::default();
    let (tx, rx) = mpsc::channel();
    pending.lock().unwrap().insert("a".into(), tx);
    let seen = std::sync::Mutex::new(Vec::new());
    let on = |e: Event| seen.lock().unwrap().push(e);

    route_line(r#"{"event":"engine_restarted","reason":"x","restarts":1,"cid":"fresh"}"#, &pending, &on);
    route_line(r#"{"event":"engine_started","cid":"a"}"#, &pending, &on);
    route_line(r#"{"event":"engine_stopped","cid":"a"}"#, &pending, &on); // a already answered
    route_line(r#"{"event":"recording_stopped","path":"p"}"#, &pending, &on); // no cid
    route_line("not json", &pending, &on);

    assert!(matches!(rx.try_recv(), Ok(Event::EngineStarted)));
    assert!(rx.try_recv().is_err(), "one response per request");
    assert!(pending.lock().unwrap().is_empty());
    let seen = seen.into_inner().unwrap();
    assert!(matches!(seen[0], Event::EngineRestarted { restarts: 1, .. }), "{seen:?}");
    assert!(matches!(seen[1], Event::EngineStopped), "{seen:?}");
    assert!(matches!(&seen[2], Event::RecordingStopped { path } if path == "p"), "{seen:?}");
    assert_eq!(seen.len(), 3, "unparseable lines are logged, not routed: {seen:?}");
}

/// The recorder binary next to this test's target dir (`target/<profile>/`).
#[cfg(windows)]
fn recorder_exe() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
    let p = profile_dir.join("cliplite-recorder.exe");
    assert!(p.exists(), "{} missing: run `cargo build -p cliplite-recorder` (cargo test --workspace builds it)", p.display());
    p
}

#[cfg(windows)]
fn isolated_home() {
    let home = std::env::temp_dir().join("cliplite engine client test");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("CLIPLITE_HOME", &home);
}

#[cfg(windows)]
#[test]
fn engine_client_matches_responses_by_cid() {
    isolated_home();
    let unsolicited = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let u = unsolicited.clone();
    let client = std::sync::Arc::new(
        EngineClient::spawn(&recorder_exe(), move |e| u.lock().unwrap().push(e)).expect("spawn recorder"));
    assert!(client.is_alive());

    let reqs = [("s1", Command::Status), ("p1", Command::Pause), ("s2", Command::Status)];
    let threads: Vec<_> = reqs.into_iter().map(|(cid, cmd)| {
        let c = client.clone();
        std::thread::spawn(move || (cid, c.request(cmd, cid, Duration::from_secs(10))))
    }).collect();
    for t in threads {
        let (cid, res) = t.join().unwrap();
        let ev = res.unwrap_or_else(|e| panic!("{cid}: {e:#}"));
        match cid {
            "p1" => assert!(matches!(&ev, Event::Error { message } if message == "engine not running"), "{cid}: {ev:?}"),
            _ => assert!(matches!(ev, Event::Status { engine_running: false, .. }), "{cid}: {ev:?}"),
        }
    }
    assert!(unsolicited.lock().unwrap().is_empty(), "{:?}", unsolicited.lock().unwrap());

    let err = client.request(Command::Status, "s1", Duration::from_secs(10));
    assert!(err.is_ok(), "a cid can be reused once answered: {err:?}");
}

#[cfg(windows)]
#[test]
fn engine_client_reports_dead_recorder() {
    isolated_home();
    let client = EngineClient::spawn(&recorder_exe(), |_| {}).expect("spawn recorder");
    client.kill_for_test();
    let t0 = std::time::Instant::now();
    while client.is_alive() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!client.is_alive());
    let err = client.request(Command::Status, "x", Duration::from_secs(10)).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(5), "fails fast, not by timeout: {err:#}");
}
