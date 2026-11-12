//! Benchmark: the fake game's frame times with ClipLite off / replay / replay+record.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FrameStats {
    pub frames: usize,
    pub avg_fps: f64,
    /// 1000 / mean of the slowest 1 % of frame times.
    pub p1_low_fps: f64,
    /// Nearest-rank 99th percentile frame time.
    pub p99_frametime_ms: f64,
    /// Population standard deviation of frame times.
    pub frametime_std_ms: f64,
}

/// Frame-time statistics. Definitions: avg_fps = 1000 / mean; p99 = sorted
/// value at rank ceil(0.99·n); 1 % low = 1000 / mean of the slowest ceil(0.01·n).
pub fn summarize(frametimes_ms: &[f64]) -> FrameStats {
    let n = frametimes_ms.len();
    if n == 0 {
        return FrameStats { frames: 0, avg_fps: 0.0, p1_low_fps: 0.0, p99_frametime_ms: 0.0, frametime_std_ms: 0.0 };
    }
    let mean = frametimes_ms.iter().sum::<f64>() / n as f64;
    let mut sorted = frametimes_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = ((0.99 * n as f64).ceil() as usize).clamp(1, n);
    let k = ((0.01 * n as f64).ceil() as usize).clamp(1, n);
    let slow_mean = sorted[n - k..].iter().sum::<f64>() / k as f64;
    let var = frametimes_ms.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
    FrameStats {
        frames: n,
        avg_fps: 1000.0 / mean,
        p1_low_fps: 1000.0 / slow_mean,
        p99_frametime_ms: sorted[rank - 1],
        frametime_std_ms: var.sqrt(),
    }
}

/// CPU usage as a percentage of one core.
pub fn core_pct(cpu_time_ms: u64, wall_s: f64) -> f64 {
    if wall_s <= 0.0 {
        return 0.0;
    }
    cpu_time_ms as f64 / 1000.0 / wall_s * 100.0
}

/// CPU usage as a percentage of the whole machine.
pub fn cpu_pct(cpu_time_ms: u64, wall_s: f64, logical_cores: usize) -> f64 {
    if wall_s <= 0.0 || logical_cores == 0 {
        return 0.0;
    }
    cpu_time_ms as f64 / 1000.0 / wall_s / logical_cores as f64 * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_stats() {
        let mut ft = vec![10.0; 99];
        ft.push(30.0); // mean 10.2 ms
        let s = summarize(&ft);
        assert!((s.avg_fps - 1000.0 / 10.2).abs() < 0.01);
        assert_eq!(s.p99_frametime_ms, 10.0); // nearest rank: ceil(0.99*100)=99th value
        assert!((s.p1_low_fps - 1000.0 / 30.0).abs() < 0.01); // slowest 1% = [30.0]
        assert_eq!(s.frames, 100);
    }

    #[test]
    fn frame_stats_std() {
        let s = summarize(&[10.0, 20.0, 10.0, 20.0]);
        assert!((s.frametime_std_ms - 5.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_percent_of_one_core() {
        // 1.5 s of CPU over 3 s wall = 50 % of one core
        assert!((core_pct(1500, 3.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_percent_of_machine() {
        // 4 s of CPU time over 2 s wall on 8 logical cores = 25 %
        assert!((cpu_pct(4000, 2.0, 8) - 25.0).abs() < 1e-9);
    }
}
