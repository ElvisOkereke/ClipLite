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
