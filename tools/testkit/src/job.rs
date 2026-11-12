//! Process lifetime control: Job Objects (kill on close) and path-based cleanup.
#![cfg(windows)]
use anyhow::{anyhow, Context, Result};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::*;

/// A Job Object whose processes die when it is dropped (even if the harness
/// panics or is killed), so a fake game can never stay stuck in fullscreen.
pub struct Job {
    handle: HANDLE,
}

unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// `allow_breakaway` lets members spawn children with
    /// `CREATE_BREAKAWAY_FROM_JOB` (ClipLite does this for its recorder).
    pub fn new_kill_on_close(allow_breakaway: bool) -> Result<Job> {
        unsafe {
            let handle = CreateJobObjectW(None, None).context("CreateJobObjectW")?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | if allow_breakaway { JOB_OBJECT_LIMIT_BREAKAWAY_OK } else { JOB_OBJECT_LIMIT(0) };
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("SetInformationJobObject")?;
            Ok(Job { handle })
        }
    }

    /// Add an already-running process to this job (no-op error if it is gone).
    pub fn assign(&self, pid: u32) -> Result<()> {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
        unsafe {
            let h = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)?;
            let r = AssignProcessToJobObject(self.handle, h);
            let _ = CloseHandle(h);
            r.map_err(|e| anyhow!("AssignProcessToJobObject({pid}): {e}"))
        }
    }

    pub fn spawn(&self, cmd: &mut Command) -> Result<Child> {
        let child = cmd.spawn().with_context(|| format!("spawn {cmd:?}"))?;
        unsafe {
            AssignProcessToJobObject(self.handle, HANDLE(child.as_raw_handle() as _))
                .map_err(|e| anyhow!("AssignProcessToJobObject: {e}"))?;
        }
        Ok(child)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// A running process: pid, exe name and full image path.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub path: Option<PathBuf>,
}

pub fn list_processes() -> Vec<ProcInfo> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::Threading::*;
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut pe = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut pe).is_ok();
        while ok {
            let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
            let name = String::from_utf16_lossy(&pe.szExeFile[..len]);
            let mut path = None;
            if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pe.th32ProcessID) {
                let mut buf = [0u16; 1024];
                let mut n = buf.len() as u32;
                if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n).is_ok() {
                    path = Some(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])));
                }
                let _ = CloseHandle(h);
            }
            out.push(ProcInfo { pid: pe.th32ProcessID, parent: pe.th32ParentProcessID, name, path });
            ok = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Lowercased path without the `\\?\` verbatim prefix that `canonicalize()`
/// adds (process image paths never carry it).
fn normalize(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase();
    if let Some(rest) = s.strip_prefix(r"\\?\unc\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    }
}

/// Whether `path` lies inside `dir` (case-insensitive, `\\?\`-insensitive,
/// and on a path-component boundary: `release` does not contain `release-x`).
pub fn is_under(path: &Path, dir: &Path) -> bool {
    let mut d = normalize(dir);
    if !d.ends_with('\\') {
        d.push('\\');
    }
    normalize(path).starts_with(&d)
}

/// Processes whose image lives under `dir`.
pub fn processes_under(dir: &Path) -> Vec<ProcInfo> {
    list_processes().into_iter().filter(|p| p.path.as_ref().is_some_and(|x| is_under(x, dir))).collect()
}

/// `root` and every process descended from it (by parent pid).
pub fn descendants(root: u32) -> Vec<ProcInfo> {
    let all = list_processes();
    let mut keep: Vec<u32> = all.iter().filter(|p| p.pid == root).map(|p| p.pid).collect();
    let mut i = 0;
    while i < keep.len() {
        let parent = keep[i];
        for p in &all {
            if p.parent == parent && p.pid != parent && !keep.contains(&p.pid) {
                keep.push(p.pid);
            }
        }
        i += 1;
    }
    all.into_iter().filter(|p| keep.contains(&p.pid)).collect()
}

pub fn kill_pid(pid: u32) {
    use windows::Win32::System::Threading::*;
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
        }
    }
}

/// Terminate `pids` and wait (≤ `timeout`) until none is alive any more;
/// TerminateProcess is asynchronous. Returns whether they all exited.
pub fn kill_and_wait(pids: &[u32], timeout: std::time::Duration) -> bool {
    for &p in pids {
        kill_pid(p);
    }
    let t0 = std::time::Instant::now();
    loop {
        let live: Vec<u32> = list_processes().iter().map(|p| p.pid).collect();
        if !pids.iter().any(|p| live.contains(p)) {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn is_alive(pid: u32) -> bool {
    list_processes().iter().any(|p| p.pid == pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn job_kills_child_on_drop() {
        let job = Job::new_kill_on_close(false).unwrap();
        let child = job
            .spawn(Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()))
            .unwrap();
        let pid = child.id();
        assert!(is_alive(pid));
        drop(job);
        let t0 = Instant::now();
        while is_alive(pid) && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!is_alive(pid), "child survived job close");
    }

    #[test]
    fn kill_and_wait_returns_only_after_exit() {
        let job = Job::new_kill_on_close(false).unwrap();
        let a = job.spawn(Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null())).unwrap();
        let b = job.spawn(Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null())).unwrap();
        assert!(kill_and_wait(&[a.id(), b.id()], std::time::Duration::from_secs(5)));
        assert!(!is_alive(a.id()) && !is_alive(b.id()), "returned before the processes were gone");
    }

    #[test]
    fn under_requires_a_path_separator() {
        assert!(is_under(Path::new(r"C:\a\target\release\cliplite.exe"), Path::new(r"C:\a\target\release")));
        assert!(is_under(Path::new(r"c:\A\Target\Release\bin\ffmpeg.exe"), Path::new(r"\\?\C:\a\target\release\")));
        assert!(!is_under(Path::new(r"C:\a\target\release-x\cliplite.exe"), Path::new(r"C:\a\target\release")));
    }

    #[test]
    fn descendants_walks_the_tree_from_root_only() {
        let job = Job::new_kill_on_close(false).unwrap();
        let child = job.spawn(Command::new("cmd").args(["/c", "ping -n 20 127.0.0.1 >NUL"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let d = descendants(child.id());
        assert!(d.iter().any(|p| p.pid == child.id()));
        assert!(d.iter().any(|p| p.name.eq_ignore_ascii_case("PING.EXE")));
        assert!(!d.iter().any(|p| p.pid == std::process::id()));
    }

    #[test]
    fn finds_own_process_under_verbatim_dir() {
        // canonicalize() yields \\?\C:\... but process image paths are C:\...
        let dir = std::env::current_exe().unwrap().parent().unwrap().canonicalize().unwrap();
        assert!(dir.to_string_lossy().starts_with(r"\\?\"));
        assert!(processes_under(&dir).iter().any(|p| p.pid == std::process::id()));
    }

    #[test]
    fn finds_own_process_under_its_dir() {
        let me = std::env::current_exe().unwrap();
        let found = processes_under(me.parent().unwrap());
        assert!(found.iter().any(|p| p.pid == std::process::id()));
    }
}
