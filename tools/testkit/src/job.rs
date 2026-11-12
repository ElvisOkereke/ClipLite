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
            out.push(ProcInfo { pid: pe.th32ProcessID, name, path });
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

/// Processes whose image lives under `dir` (case-insensitive).
pub fn processes_under(dir: &Path) -> Vec<ProcInfo> {
    let d = normalize(dir);
    list_processes()
        .into_iter()
        .filter(|p| p.path.as_ref().is_some_and(|x| normalize(x).starts_with(&d)))
        .collect()
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
