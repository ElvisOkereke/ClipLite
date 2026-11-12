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
    crate::job::processes_under(dir).into_iter().filter(|p| wanted(&p.name)).filter_map(|p| measure(&p)).collect()
}

/// Sample `roots` and all their descendants — for ClipLite that is the UI
/// process, its WebView2 renderers, the recorder and its FFmpeg children.
pub fn sample_tree(roots: &[u32]) -> Vec<ProcSample> {
    roots.iter().flat_map(|r| crate::job::descendants(*r)).filter_map(|p| measure(&p)).collect()
}

fn measure(p: &crate::job::ProcInfo) -> Option<ProcSample> {
    unsafe {
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
    }
}

/// Any ClipLite capture already running (called before the harness starts its
/// own app, so every recorder found belongs to someone else and would skew numbers).
pub fn foreign_capture() -> Vec<String> {
    list_processes()
        .into_iter()
        .filter(|p| p.name.eq_ignore_ascii_case("cliplite-recorder.exe"))
        .map(|p| format!("{} (pid {}) {}", p.name, p.pid, p.path.map(|x| x.display().to_string()).unwrap_or_default()))
        .collect()
}

/// Recursive size of a directory in bytes (0 if missing).
///
/// Stats each file by path: `DirEntry::metadata` comes from the directory
/// listing, which on NTFS reports stale sizes for files another process still
/// has open (e.g. the replay buffer's MKV).
pub fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| {
                    let path = e.path();
                    match std::fs::metadata(&path) {
                        Ok(m) if m.is_dir() => dir_size(&path),
                        Ok(m) => m.len(),
                        Err(_) => 0,
                    }
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
    fn dir_size_sees_bytes_of_files_still_open_for_writing() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("clip lite dirsize test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let mut f = std::fs::File::create(dir.join("sub").join("growing.mkv")).unwrap();
        f.write_all(&vec![7u8; 1024]).unwrap();
        f.flush().unwrap();
        let _ = dir_size(&dir); // directory listing caches the size seen here
        f.write_all(&vec![7u8; 3 * 1024 * 1024]).unwrap();
        f.flush().unwrap();
        assert!(dir_size(&dir) >= 3 * 1024 * 1024, "stale size for an open file");
        drop(f);
    }

    #[test]
    fn tree_includes_root_and_descendants() {
        let job = crate::job::Job::new_kill_on_close(false).unwrap();
        // cmd → ping: a grandchild chain rooted at cmd
        let child = job
            .spawn(std::process::Command::new("cmd").args(["/c", "ping -n 20 127.0.0.1 >NUL"]))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let s = sample_tree(&[child.id()]);
        assert!(s.iter().any(|p| p.pid == child.id()), "root missing");
        assert!(s.iter().any(|p| p.name.eq_ignore_ascii_case("PING.EXE")), "descendant missing: {:?}", s.iter().map(|p| &p.name).collect::<Vec<_>>());
        assert!(!s.iter().any(|p| p.pid == std::process::id()), "unrelated process included");
    }

    #[test]
    fn ignores_processes_outside_dir() {
        let s = sample(&["cliplite.exe"], std::path::Path::new(r"C:\definitely\not\here"));
        assert!(s.is_empty());
    }
}
