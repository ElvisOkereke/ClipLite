//! Per-process resource sampling (memory, handles, CPU time, disk writes).
#![cfg(windows)]

use crate::job::list_processes;
use serde::Serialize;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, FILETIME};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::*;

#[derive(Debug, Clone, Serialize)]
pub struct ProcSample {
    pub name: String,
    pub pid: u32,
    pub private_bytes: u64,
    pub working_set: u64,
    pub handles: u32,
    pub cpu_time_ms: u64,
    pub write_bytes: u64,
}

fn ft_ms(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) / 10_000
}

/// Sample processes named in `names` whose image lives under `dir` (so the
/// owner's own ClipLite elsewhere is never counted).
pub fn sample(names: &[&str], dir: &Path) -> Vec<ProcSample> {
    let wanted = |n: &str| names.iter().any(|w| w.eq_ignore_ascii_case(n));
    crate::job::processes_under(dir)
        .into_iter()
        .filter(|p| wanted(&p.name))
        .filter_map(|p| unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, p.pid).ok()?;
            let mut mem = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
            let mem_ok = GetProcessMemoryInfo(h, &mut mem as *mut _ as *mut PROCESS_MEMORY_COUNTERS, mem.cb).is_ok();
            let mut handles = 0u32;
            let _ = GetProcessHandleCount(h, &mut handles);
            let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            let cpu = if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok() { ft_ms(k) + ft_ms(u) } else { 0 };
            let mut io = IO_COUNTERS::default();
            let writes = if GetProcessIoCounters(h, &mut io).is_ok() { io.WriteTransferCount } else { 0 };
            let _ = CloseHandle(h);
            mem_ok.then(|| ProcSample {
                name: p.name.clone(),
                pid: p.pid,
                private_bytes: mem.PrivateUsage as u64,
                working_set: mem.WorkingSetSize as u64,
                handles,
                cpu_time_ms: cpu,
                write_bytes: writes,
            })
        })
        .collect()
}

/// Any capture running from a ClipLite install *other* than `dir` (it would skew numbers).
pub fn foreign_capture(dir: &Path) -> Vec<String> {
    let mine: Vec<u32> = crate::job::processes_under(dir).iter().map(|p| p.pid).collect();
    list_processes()
        .into_iter()
        .filter(|p| p.name.eq_ignore_ascii_case("cliplite-recorder.exe") && !mine.contains(&p.pid))
        .map(|p| format!("{} (pid {}) {}", p.name, p.pid, p.path.map(|x| x.display().to_string()).unwrap_or_default()))
        .collect()
}

/// Recursive size of a directory in bytes (0 if missing).
pub fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_own_process() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().to_string();
        let s = sample(&[name.as_str()], me.parent().unwrap());
        let mine = s.iter().find(|p| p.pid == std::process::id()).expect("own process sampled");
        assert!(mine.private_bytes > 0 && mine.working_set > 0 && mine.handles > 0);
    }

    #[test]
    fn ignores_processes_outside_dir() {
        let s = sample(&["cliplite.exe"], std::path::Path::new(r"C:\definitely\not\here"));
        assert!(s.is_empty());
    }
}
