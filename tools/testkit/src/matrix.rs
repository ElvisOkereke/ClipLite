//! Hotkey matrix: cells, per-trial evaluation and result files.
use crate::clip::ClipReport;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowState {
    Visible,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuLoad {
    Idle,
    Saturated,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TrialResult {
    pub ok: bool,
    pub failure: Option<&'static str>,
    pub latency_ms: Option<u64>,
    pub file: Option<PathBuf>,
    pub duration_s: Option<f64>,
    /// Whether the fake game actually held exclusive fullscreen.
    pub fullscreen: Option<bool>,
    /// FFmpeg alive under the app's process tree when the chord was sent.
    pub capture_alive: Option<bool>,
    /// Minimum clip length required (in-game time before the chord − margin).
    pub expected_min_s: Option<f64>,
    /// Pipeline stages logged for this press (empty for builds without JSON logs).
    pub stages: Vec<String>,
}

impl TrialResult {
    pub fn failed(reason: &'static str) -> Self {
        TrialResult {
            ok: false,
            failure: Some(reason),
            latency_ms: None,
            file: None,
            duration_s: None,
            fullscreen: None,
            capture_alive: None,
            expected_min_s: None,
            stages: Vec::new(),
        }
    }
}

/// Judge one trial from the files that appeared and the validated clip.
pub fn evaluate_trial(new_files: &[PathBuf], report: Option<&ClipReport>, expected_min_s: f64) -> TrialResult {
    let mut r = TrialResult::failed("no_file");
    match new_files.len() {
        0 => return r,
        1 => {}
        _ => {
            r.failure = Some("duplicate");
            return r;
        }
    }
    r.file = Some(new_files[0].clone());
    let Some(rep) = report else {
        r.failure = Some("invalid");
        return r;
    };
    r.duration_s = Some(rep.duration_s);
    r.expected_min_s = Some(expected_min_s);
    r.failure = if !rep.has_video {
        Some("invalid")
    } else if rep.black {
        Some("black")
    } else if rep.frozen_tail {
        Some("frozen")
    } else if rep.duration_s < expected_min_s {
        Some("too_short")
    } else {
        None
    };
    r.ok = r.failure.is_none();
    r
}

/// Trial conditions that invalidate an otherwise good clip: an exclusive cell
/// whose game never held fullscreen, or capture already dead at the chord.
pub fn apply_context(mut r: TrialResult, exclusive: bool, fullscreen: bool, capture_alive: bool) -> TrialResult {
    r.fullscreen = Some(fullscreen);
    r.capture_alive = Some(capture_alive);
    if r.ok {
        if exclusive && !fullscreen {
            r.ok = false;
            r.failure = Some("not_fullscreen");
        } else if !capture_alive {
            r.ok = false;
            r.failure = Some("capture_dead");
        }
    }
    r
}

/// Stages logged for the first hotkey press at/after `after`, in log order.
pub fn stages_for_trial(lines: &[serde_json::Value], after: chrono::DateTime<chrono::Utc>) -> Vec<String> {
    let ts = |v: &serde_json::Value| {
        v["timestamp"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&chrono::Utc))
    };
    let Some(cid) = lines
        .iter()
        .filter(|v| v["stage"] == "hotkey.pressed" && ts(v).is_some_and(|t| t >= after))
        .min_by_key(|v| ts(v))
        .and_then(|v| v["cid"].as_str())
    else {
        return Vec::new();
    };
    let mut hits: Vec<(&serde_json::Value, String)> = lines
        .iter()
        .filter(|v| v["cid"].as_str() == Some(cid))
        .filter_map(|v| v["stage"].as_str().map(|s| (v, s.to_string())))
        .collect();
    hits.sort_by_key(|(v, _)| ts(v));
    let mut out: Vec<String> = Vec::new();
    for (_, s) in hits {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// All JSON log lines under `home/logs` (empty when the build has none).
pub fn read_json_logs(home: &std::path::Path) -> Vec<serde_json::Value> {
    let Ok(rd) = std::fs::read_dir(home.join("logs")) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .flat_map(|e| std::fs::read_to_string(e.path()).unwrap_or_default().lines().map(String::from).collect::<Vec<_>>())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub mode: &'static str,
    pub elevated: bool,
    pub window: WindowState,
    pub cpu: CpuLoad,
    pub trials: Vec<TrialResult>,
}

/// The 12 cells: mode × window × cpu (elevation is per run).
pub fn cells(elevated: bool) -> Vec<Cell> {
    let mut out = Vec::new();
    for mode in ["windowed", "borderless", "exclusive"] {
        for window in [WindowState::Visible, WindowState::Hidden] {
            for cpu in [CpuLoad::Idle, CpuLoad::Saturated] {
                out.push(Cell { mode, elevated, window, cpu, trials: Vec::new() });
            }
        }
    }
    out
}

/// Markdown summary: one row per cell.
pub fn summary_markdown(cells: &[Cell]) -> String {
    let mut s = String::from("| mode | elevated | window | cpu | passed | failures | median latency |\n|---|---|---|---|---|---|---|\n");
    for c in cells {
        let passed = c.trials.iter().filter(|t| t.ok).count();
        let mut fails: Vec<&str> = c.trials.iter().filter_map(|t| t.failure).collect();
        fails.sort();
        let mut hist: Vec<String> = Vec::new();
        let mut i = 0;
        while i < fails.len() {
            let n = fails[i..].iter().take_while(|f| **f == fails[i]).count();
            hist.push(format!("{}×{}", fails[i], n));
            i += n;
        }
        let mut lat: Vec<u64> = c.trials.iter().filter_map(|t| t.latency_ms).collect();
        lat.sort();
        let median = lat.get(lat.len() / 2).map(|m| format!("{m} ms")).unwrap_or_else(|| "–".into());
        s.push_str(&format!(
            "| {} | {} | {:?} | {:?} | {}/{} | {} | {} |\n",
            c.mode, c.elevated, c.window, c.cpu, passed, c.trials.len(),
            if hist.is_empty() { "–".into() } else { hist.join(", ") }, median
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn rep(duration_s: f64, black: bool) -> ClipReport {
        ClipReport { duration_s, has_video: true, has_audio: false, black, frozen_tail: false }
    }

    #[test]
    fn trial_no_file() {
        assert_eq!(evaluate_trial(&[], None, 3.0).failure, Some("no_file"));
    }
    #[test]
    fn trial_duplicate() {
        assert_eq!(evaluate_trial(&[p("a.mp4"), p("b.mp4")], None, 3.0).failure, Some("duplicate"));
    }
    #[test]
    fn trial_invalid_when_unreadable() {
        assert_eq!(evaluate_trial(&[p("a.mp4")], None, 3.0).failure, Some("invalid"));
    }
    #[test]
    fn trial_black() {
        assert_eq!(evaluate_trial(&[p("a.mp4")], Some(&rep(30.0, true)), 3.0).failure, Some("black"));
    }
    #[test]
    fn trial_too_short() {
        assert_eq!(evaluate_trial(&[p("a.mp4")], Some(&rep(2.0, false)), 3.0).failure, Some("too_short"));
    }
    #[test]
    fn trial_ok() {
        let r = evaluate_trial(&[p("a.mp4")], Some(&rep(29.5, false)), 3.0);
        assert!(r.ok);
        assert_eq!(r.duration_s, Some(29.5));
    }
    #[test]
    fn trial_frozen() {
        let mut r = rep(30.0, false);
        r.frozen_tail = true;
        assert_eq!(evaluate_trial(&[p("a.mp4")], Some(&r), 3.0).failure, Some("frozen"));
    }
    #[test]
    fn context_requires_fullscreen_in_exclusive_cells() {
        let ok = evaluate_trial(&[p("a.mp4")], Some(&rep(9.0, false)), 3.0);
        assert_eq!(apply_context(ok.clone(), true, false, true).failure, Some("not_fullscreen"));
        assert!(apply_context(ok.clone(), false, false, true).ok);
        assert_eq!(apply_context(ok, true, true, false).failure, Some("capture_dead"));
    }
    #[test]
    fn stages_follow_the_first_hotkey_after_the_chord() {
        let t = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
        let lines: Vec<serde_json::Value> = vec![
            serde_json::json!({"timestamp":"2026-01-01T00:00:00.000Z","stage":"hotkey.pressed","cid":"old"}),
            serde_json::json!({"timestamp":"2026-01-01T00:00:10.000Z","stage":"hotkey.pressed","cid":"c1"}),
            serde_json::json!({"timestamp":"2026-01-01T00:00:10.001Z","stage":"hotkey.dispatched","cid":"c1"}),
            serde_json::json!({"timestamp":"2026-01-01T00:00:10.002Z","stage":"hotkey.pressed","cid":"c2"}),
            serde_json::json!({"timestamp":"2026-01-01T00:00:10.005Z","stage":"command.received","cid":"c1"}),
        ];
        let s = stages_for_trial(&lines, t("2026-01-01T00:00:09.900Z"));
        assert_eq!(s, vec!["hotkey.pressed", "hotkey.dispatched", "command.received"]);
        assert!(stages_for_trial(&[], t("2026-01-01T00:00:00Z")).is_empty());
    }
    #[test]
    fn twelve_cells() {
        let c = cells(false);
        assert_eq!(c.len(), 12);
        assert!(c.iter().all(|c| !c.elevated));
    }
    #[test]
    fn summary_counts() {
        let mut c = cells(false);
        c.truncate(1);
        c[0].trials = vec![TrialResult::failed("no_file"), TrialResult::failed("no_file"),
                           TrialResult { ok: true, latency_ms: Some(900), duration_s: Some(9.0), ..TrialResult::failed("x") }];
        c[0].trials[2].failure = None;
        let md = summary_markdown(&c);
        assert!(md.contains("| 1/3 | no_file×2 | 900 ms |"), "{md}");
    }
}
