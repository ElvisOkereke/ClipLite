//! Pure parts of the hotkey listener: dispatcher de-duplication and binding setup.
use super::*;
use std::time::{Duration, Instant};

#[test]
fn same_action_other_source_within_250ms_runs_once() {
    let mut d = Dedup::default();
    let t0 = Instant::now();
    assert!(d.should_run("start", Source::Hook, t0));
    assert!(!d.should_run("start", Source::Register, t0 + Duration::from_millis(5)));
    // Reverse order (WM_HOTKEY first) behaves the same.
    let t1 = t0 + Duration::from_secs(2);
    assert!(d.should_run("start", Source::Register, t1));
    assert!(!d.should_run("start", Source::Hook, t1 + Duration::from_millis(249)));
}

#[test]
fn same_source_twice_runs_twice() {
    let mut d = Dedup::default();
    let t0 = Instant::now();
    assert!(d.should_run("start", Source::Hook, t0));
    assert!(d.should_run("start", Source::Hook, t0 + Duration::from_millis(100)));
}

#[test]
fn after_window_runs_again() {
    let mut d = Dedup::default();
    let t0 = Instant::now();
    assert!(d.should_run("start", Source::Hook, t0));
    assert!(d.should_run("start", Source::Register, t0 + Duration::from_millis(251)));
}

#[test]
fn second_press_pair_within_window_runs_once_more() {
    // Press 1: hook + register (one run). Press 2 shortly after: hook + register (one run).
    let mut d = Dedup::default();
    let t0 = Instant::now();
    let ms = |n| t0 + Duration::from_millis(n);
    assert!(d.should_run("start", Source::Hook, ms(0)));
    assert!(!d.should_run("start", Source::Register, ms(2)));
    assert!(d.should_run("start", Source::Register, ms(120)));
    assert!(!d.should_run("start", Source::Hook, ms(121)));
}

#[test]
fn different_actions_do_not_dedup() {
    let mut d = Dedup::default();
    let t0 = Instant::now();
    assert!(d.should_run("start", Source::Hook, t0));
    assert!(d.should_run("stop", Source::Register, t0));
}

#[test]
fn build_bindings_reports_and_skips() {
    let mut map = HashMap::new();
    map.insert("start".to_string(), "CommandOrControl+Shift+R".to_string());
    map.insert("stop".to_string(), "Ctrl+Nope".to_string());
    map.insert("annotate".to_string(), "Ctrl+Shift+A".to_string());
    map.insert("library".to_string(), "Ctrl+Mouse4".to_string());
    map.insert("pause".to_string(), "".to_string());
    let b = build_bindings(&map);
    let actions: Vec<&str> = b.active.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(actions, ["library", "start"]);
    let stop = b.status.iter().find(|s| s.action == "stop").unwrap();
    assert!(!stop.parsed && stop.error.as_deref().unwrap().contains("Nope"));
    let start = b.status.iter().find(|s| s.action == "start").unwrap();
    assert!(start.parsed);
    assert_eq!(start.canonical.as_deref(), Some("Ctrl+Shift+R"));
    // Unknown actions and empty bindings are not reported as bindings.
    assert!(b.status.iter().all(|s| s.action != "annotate" && s.action != "pause"));
    // Only chords RegisterHotKey can express get an id.
    assert_eq!(b.register.len(), 1);
    assert_eq!(b.register[0].action, "start");
}

// ── Dispatcher state ──────────────────────────────────────────────────────────

use cliplite_core::hotkey::{Chord, Input, RawEvent};

fn chord(s: &str) -> Chord {
    Chord::parse(s).unwrap()
}
fn inp(s: &str) -> Input {
    *chord(s).inputs().next().unwrap()
}
fn dn(s: &str) -> Msg {
    Msg::Raw(RawEvent::Down(inp(s)))
}
fn upm(s: &str) -> Msg {
    Msg::Raw(RawEvent::Up(inp(s)))
}
fn dispatcher() -> Dispatcher {
    Dispatcher::new(vec![("start".into(), chord("Ctrl+F2")), ("stop".into(), chord("Ctrl+F3"))])
}
fn runs(effects: &[Effect]) -> Vec<Action> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run { action, .. } => Some(*action),
            _ => None,
        })
        .collect()
}

#[test]
fn dispatcher_runs_matched_action_once_for_both_sources() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(dn("Ctrl"), t);
    let e = d.handle(dn("F2"), t);
    assert_eq!(runs(&e), [Action::Start]);
    assert!(matches!(&e[0], Effect::Run { display, source: Some(Source::Hook), .. } if display == "Ctrl + F2"));
    assert!(runs(&d.handle(Msg::Registered("start".into()), t)).is_empty());
}

#[test]
fn capture_pauses_matching_and_registered() {
    let mut d = dispatcher();
    let t = Instant::now();
    assert!(d.handle(Msg::BeginCapture, t).is_empty());
    assert!(d.capturing());
    d.handle(dn("Ctrl"), t);
    let e = d.handle(dn("F2"), t);
    assert!(runs(&e).is_empty());
    assert!(e.iter().any(|x| matches!(x, Effect::Capture { state: "holding", canonical, .. } if canonical == "Ctrl+F2")));
    assert!(d.handle(Msg::Registered("start".into()), t).is_empty());
}

#[test]
fn capture_done_ends_capture_and_requests_resync() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(Msg::BeginCapture, t);
    d.handle(dn("Ctrl"), t);
    d.handle(dn("F3"), t);
    d.handle(upm("F3"), t);
    let e = d.handle(upm("Ctrl"), t);
    assert!(matches!(&e[0], Effect::Capture { state: "done", canonical, .. } if canonical == "Ctrl+F3"));
    assert!(e.contains(&Effect::Resync));
    assert!(!d.capturing());
    // Matching is back on.
    d.handle(dn("Ctrl"), t);
    assert_eq!(runs(&d.handle(dn("F3"), t)), [Action::Stop]);
}

#[test]
fn holding_is_reported_only_on_change() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(Msg::BeginCapture, t);
    assert_eq!(d.handle(dn("Ctrl"), t).len(), 1);
    assert!(d.handle(dn("Ctrl"), t).is_empty()); // key repeat
    assert!(d.handle(Msg::Raw(RawEvent::Other), t).is_empty()); // click
}

#[test]
fn cancel_ends_capture() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(Msg::BeginCapture, t);
    d.handle(dn("Ctrl"), t);
    let e = d.handle(Msg::CancelCapture, t);
    assert_eq!(e, [Effect::CaptureCancelled { reason: "user" }, Effect::Resync]);
    assert!(!d.capturing());
    // Cancel without a capture does nothing.
    assert!(d.handle(Msg::CancelCapture, t).is_empty());
}

#[test]
fn capture_times_out() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(Msg::BeginCapture, t);
    d.handle(dn("Ctrl"), t);
    assert_eq!(d.capture_deadline(), Some(t + CAPTURE_TIMEOUT));
    assert!(d.tick(t + CAPTURE_TIMEOUT - Duration::from_millis(1)).is_empty());
    assert_eq!(d.tick(t + CAPTURE_TIMEOUT), [Effect::CaptureCancelled { reason: "timeout" }, Effect::Resync]);
    assert!(!d.capturing());
    assert_eq!(d.capture_deadline(), None);
}

#[test]
fn reinstall_resync_cancels_capture() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.handle(Msg::BeginCapture, t);
    d.handle(dn("Ctrl"), t);
    let e = d.handle(Msg::Resync(vec![]), t);
    assert_eq!(e, [Effect::CaptureCancelled { reason: "reinstall" }]);
    assert!(!d.capturing());
    // Registered works again.
    assert_eq!(runs(&d.handle(Msg::Registered("stop".into()), t)), [Action::Stop]);
}

#[test]
fn resync_keeps_held_modifier() {
    let mut d = dispatcher();
    let t = Instant::now();
    d.resync(vec![Input::Key(cliplite_core::hotkey::VK_LCONTROL)]);
    assert_eq!(runs(&d.handle(dn("F2"), t)), [Action::Start]);
}

#[test]
fn simulate_runs_without_dedup() {
    let mut d = dispatcher();
    let t = Instant::now();
    assert_eq!(runs(&d.handle(Msg::Simulate(Action::Pause), t)), [Action::Pause]);
    assert_eq!(runs(&d.handle(Msg::Simulate(Action::Pause), t)), [Action::Pause]);
}

// ── Serial action worker ──────────────────────────────────────────────────────

#[tokio::test]
async fn actions_run_one_at_a_time_in_press_order() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let log = Arc::new(Mutex::new(Vec::new()));
    for i in 0..3 {
        tx.send(i).unwrap();
    }
    drop(tx);
    let l = log.clone();
    run_serially(rx, move |i: u32| {
        let l = l.clone();
        async move {
            l.lock().unwrap().push(format!("start {i}"));
            // Later jobs finish faster: concurrency would reorder the ends.
            tokio::time::sleep(Duration::from_millis(30 - 10 * u64::from(i))).await;
            l.lock().unwrap().push(format!("end {i}"));
        }
    })
    .await;
    assert_eq!(*log.lock().unwrap(), ["start 0", "end 0", "start 1", "end 1", "start 2", "end 2"]);
}
