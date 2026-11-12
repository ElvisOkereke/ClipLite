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
}

impl TrialResult {
    pub fn failed(reason: &'static str) -> Self {
        TrialResult { ok: false, failure: Some(reason), latency_ms: None, file: None, duration_s: None, fullscreen: None }
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
    r.failure = if !rep.has_video {
        Some("invalid")
    } else if rep.black {
        Some("black")
    } else if rep.duration_s < expected_min_s {
        Some("too_short")
    } else {
        None
    };
    r.ok = r.failure.is_none();
    r
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
        ClipReport { duration_s, has_video: true, has_audio: false, black }
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
                           TrialResult { ok: true, failure: None, latency_ms: Some(900), file: None, duration_s: Some(9.0), fullscreen: None }];
        let md = summary_markdown(&c);
        assert!(md.contains("| 1/3 | no_file×2 | 900 ms |"), "{md}");
    }
}
