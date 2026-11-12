//! Soak test: run the replay buffer for hours and check memory/disk stay flat.

use crate::matrix::TrialResult;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SoakVerdict {
    pub pass: bool,
    pub slope_mb_per_hour: f64,
    pub band_mb: f64,
    pub max_temp_dir_mb: f64,
    pub saves_ok: usize,
    pub saves_total: usize,
}

/// Pass criteria (after warm-up), on total private bytes of ClipLite's processes:
/// least-squares slope < 5 MB/h; max − min of a 5-sample rolling median < 100 MB
/// (the median suppresses the short-lived extraction FFmpeg during saves);
/// replay temp dir ≤ cap × 1.1 (cap 0 = unlimited); every save ok.
pub fn verdict(samples: &[(f64, u64)], temp_max: u64, cap_mb: u32, saves: &[TrialResult], warmup_s: f64) -> SoakVerdict {
    const MB: f64 = 1024.0 * 1024.0;
    let pts: Vec<(f64, f64)> = samples.iter().filter(|(t, _)| *t >= warmup_s).map(|(t, b)| (*t, *b as f64 / MB)).collect();

    let slope_mb_per_hour = if pts.len() >= 2 {
        let n = pts.len() as f64;
        let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let num: f64 = pts.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
        let den: f64 = pts.iter().map(|(x, _)| (x - mx).powi(2)).sum();
        if den > 0.0 { num / den * 3600.0 } else { 0.0 }
    } else {
        0.0
    };

    let smoothed: Vec<f64> = (0..pts.len())
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(pts.len());
            let mut w: Vec<f64> = pts[lo..hi].iter().map(|p| p.1).collect();
            w.sort_by(|a, b| a.partial_cmp(b).unwrap());
            w[w.len() / 2]
        })
        .collect();
    let band_mb = match (smoothed.iter().cloned().reduce(f64::max), smoothed.iter().cloned().reduce(f64::min)) {
        (Some(hi), Some(lo)) => hi - lo,
        _ => 0.0,
    };

    let max_temp_dir_mb = temp_max as f64 / MB;
    let saves_ok = saves.iter().filter(|s| s.ok).count();
    let disk_ok = cap_mb == 0 || max_temp_dir_mb <= cap_mb as f64 * 1.1;
    let pass = slope_mb_per_hour < 5.0 && band_mb < 100.0 && disk_ok && saves_ok == saves.len();
    SoakVerdict { pass, slope_mb_per_hour, band_mb, max_temp_dir_mb, saves_ok, saves_total: saves.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix::TrialResult;

    const MB: u64 = 1024 * 1024;

    fn ok_save() -> TrialResult {
        TrialResult { ok: true, failure: None, latency_ms: Some(500), file: None, duration_s: Some(60.0), fullscreen: None }
    }
    /// 2 h of samples every 5 s, value from `f(t_s)` in MB.
    fn series(f: impl Fn(f64) -> f64) -> Vec<(f64, u64)> {
        (0..1440).map(|i| { let t = i as f64 * 5.0; (t, (f(t) * MB as f64) as u64) }).collect()
    }

    #[test]
    fn verdict_flat_passes() {
        let s = series(|t| 400.0 + 10.0 * (t / 300.0).sin());
        let v = verdict(&s, 300 * MB, 500, &[ok_save(), ok_save()], 600.0);
        assert!(v.pass, "{v:?}");
        assert!(v.slope_mb_per_hour.abs() < 5.0);
    }

    #[test]
    fn verdict_leak_fails() {
        let s = series(|t| 400.0 + 50.0 * t / 3600.0);
        let v = verdict(&s, 300 * MB, 500, &[ok_save()], 600.0);
        assert!(!v.pass);
        assert!((v.slope_mb_per_hour - 50.0).abs() < 1.0, "{v:?}");
    }

    #[test]
    fn verdict_disk_over_cap_fails() {
        let s = series(|_| 400.0);
        let v = verdict(&s, 600 * MB, 500, &[ok_save()], 600.0);
        assert!(!v.pass);
        assert!((v.max_temp_dir_mb - 600.0).abs() < 0.01);
    }

    #[test]
    fn verdict_failed_save_fails() {
        let s = series(|_| 400.0);
        let v = verdict(&s, 0, 500, &[ok_save(), TrialResult::failed("no_file")], 600.0);
        assert!(!v.pass);
        assert_eq!((v.saves_ok, v.saves_total), (1, 2));
    }

    #[test]
    fn verdict_ignores_warmup_and_transient_spikes() {
        let s = series(|t| if t < 600.0 { 100.0 + t } else if (t as u64) % 600 == 0 { 480.0 } else { 400.0 });
        let v = verdict(&s, 0, 500, &[ok_save()], 600.0);
        assert!(v.pass, "{v:?}");
    }
}
