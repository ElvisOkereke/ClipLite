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
    assert!(start.registerable && !start.registered);
}

fn map_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(a, s)| (a.to_string(), s.to_string())).collect()
}

#[test]
fn build_bindings_registers_only_ctrl_alt_win_chords_once() {
    let b = build_bindings(&map_of(&[
        ("start", "Ctrl+Alt+F9"),
        ("stop", "Shift+F2"),                      // Shift-only: hook only
        ("pause", "F11"),                          // bare key: hook only
        ("replay_save", "CommandOrControl+Backquote"),
        ("replay_toggle", "Ctrl+`"),               // same chord as replay_save
        ("library", "Ctrl+Mouse4"),
    ]));
    let reg: Vec<&str> = b.register.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(reg, ["replay_save", "start"]);
    assert_ne!(b.register[0].id, b.register[1].id);
    let st = |a: &str| b.status.iter().find(|s| s.action == a).unwrap().clone();
    assert!(st("replay_save").registerable);
    assert!(!st("replay_toggle").registerable, "internal duplicate is not registered twice");
    for a in ["stop", "pause", "library"] {
        assert!(!st(a).registerable, "{a}");
    }
    assert!(b.status.iter().all(|s| !s.registered && s.register_error.is_none()));
}

#[test]
fn owner_settings_load_cleanly() {
    let b = build_bindings(&map_of(&[
        ("replay_save", "CommandOrControl+Backquote"),
        ("replay_toggle", "CommandOrControl+1"),
        ("stop", "CommandOrControl+F1"),
        ("pause", "CommandOrControl+F3"),
        ("library", "CommandOrControl+F4"),
        ("start", ""),
        ("annotate", "CommandOrControl+Shift+A"),
    ]));
    assert_eq!(b.active.len(), 5);
    assert!(b.status.iter().all(|s| s.parsed));
    assert_eq!(b.register.len(), 5);
}

/// Records RegisterHotKey / UnregisterHotKey calls; fails actions in `busy`.
#[derive(Default)]
struct FakeApi {
    calls: Vec<String>,
    busy: Vec<&'static str>,
}

impl HotkeyApi for FakeApi {
    fn register(&mut self, spec: &RegisterSpec) -> Result<(), String> {
        self.calls.push(format!("reg {}", spec.action));
        if self.busy.contains(&spec.action.as_str()) {
            Err("Hot key is already registered.".into())
        } else {
            Ok(())
        }
    }
    fn unregister(&mut self, id: i32) {
        self.calls.push(format!("unreg {id}"));
    }
}

fn registrar_setup() -> (Registrar, HotkeyStatus, FakeApi) {
    let b = build_bindings(&map_of(&[("start", "Ctrl+Alt+F9"), ("stop", "Ctrl+Alt+F10"), ("pause", "Shift+F2")]));
    let st = HotkeyStatus { bindings: b.status, ..Default::default() };
    (Registrar::new(b.register), st, FakeApi { busy: vec!["stop"], ..Default::default() })
}

fn registered(st: &HotkeyStatus) -> Vec<(&str, bool, bool)> {
    st.bindings.iter().map(|b| (b.action.as_str(), b.registered, b.register_error.is_some())).collect()
}

#[test]
fn registrar_registers_only_while_wanted() {
    let (mut r, mut st, mut api) = registrar_setup();
    assert!(!st.register_active);
    r.set_wanted(true, &mut api, &mut st);
    assert_eq!(api.calls, ["reg start", "reg stop"]);
    assert!(st.register_active);
    assert_eq!(registered(&st), [("pause", false, false), ("start", true, false), ("stop", false, true)]);
    let start_id = r.ids().next().unwrap();
    assert_eq!(r.action(start_id), Some("start"));
    assert_eq!(r.ids().count(), 1, "the failed one holds no id");

    api.calls.clear();
    r.set_wanted(true, &mut api, &mut st);
    assert!(api.calls.is_empty(), "no change, no calls");

    r.set_wanted(false, &mut api, &mut st);
    assert_eq!(api.calls, [format!("unreg {start_id}")]);
    assert!(!st.register_active);
    assert_eq!(r.action(start_id), None);
    // The last failure stays visible after unregistering.
    assert_eq!(registered(&st), [("pause", false, false), ("start", false, false), ("stop", false, true)]);

    // Success on a later attempt clears the error.
    api.busy.clear();
    r.set_wanted(true, &mut api, &mut st);
    assert_eq!(registered(&st), [("pause", false, false), ("start", true, false), ("stop", true, false)]);
}

#[test]
fn registrar_suspends_while_capturing() {
    let (mut r, mut st, mut api) = registrar_setup();
    r.set_capturing(true, &mut api, &mut st);
    r.set_wanted(true, &mut api, &mut st);
    assert!(api.calls.is_empty() && !st.register_active);
    r.set_capturing(false, &mut api, &mut st);
    assert_eq!(api.calls, ["reg start", "reg stop"]);
    api.calls.clear();
    r.set_capturing(true, &mut api, &mut st);
    assert_eq!(api.calls.len(), 1);
    assert!(api.calls[0].starts_with("unreg "));
    assert!(st.bindings.iter().all(|b| !b.registered));
}

#[test]
fn registrar_teardown_unregisters_everything() {
    let (mut r, mut st, mut api) = registrar_setup();
    api.busy.clear();
    r.set_wanted(true, &mut api, &mut st);
    api.calls.clear();
    r.unregister_all(&mut api, &mut st);
    assert_eq!(api.calls.len(), 2);
    assert!(!st.register_active && r.ids().next().is_none());
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
