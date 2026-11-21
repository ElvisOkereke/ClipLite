//! FFmpeg process helpers for the recorder: spawning (stdout is never
//! inherited: the recorder's stdout is the IPC channel) and stderr draining.
use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Last lines FFmpeg wrote to stderr (progress lines included), shared with
/// the drain thread. Draining also prevents FFmpeg from blocking on a full pipe.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    pub const CAPACITY: usize = 64;

    pub fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap();
        if q.len() == Self::CAPACITY {
            q.pop_front();
        }
        q.push_back(line);
    }

    #[cfg(test)]
    pub fn text(&self) -> String {
        self.0.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// The last `n` lines that are not progress updates, oldest first.
    pub fn last_messages(&self, n: usize) -> Vec<String> {
        let q = self.0.lock().unwrap();
        let mut v: Vec<String> = q.iter().rev().filter(|l| !is_progress(l)).take(n).cloned().collect();
        v.reverse();
        v
    }
}

fn is_progress(line: &str) -> bool {
    line.starts_with("frame=") || line.starts_with("size=")
}

/// Split FFmpeg output into lines; progress updates are terminated by `\r`.
pub fn drain_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(['\r', '\n'])
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Read FFmpeg's stderr until EOF: keep a tail and log non-progress lines.
/// The thread ends when the process exits.
pub fn spawn_stderr_drain(
    mut stderr: std::process::ChildStderr,
    tail: StderrTail,
    purpose: &'static str,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let n = match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buf[..n]);
            // emit only complete lines; keep the trailing partial line
            if let Some(last) = pending.iter().rposition(|b| *b == b'\n' || *b == b'\r') {
                let complete: Vec<u8> = pending.drain(..=last).collect();
                for line in drain_lines(&complete) {
                    if !is_progress(&line) {
                        tracing::info!(target: "ffmpeg", purpose, "{line}");
                    }
                    tail.push(line);
                }
            }
        }
        for line in drain_lines(&pending) {
            tracing::info!(target: "ffmpeg", purpose, "{line}");
            tail.push(line);
        }
        tracing::info!(target: "ffmpeg", purpose, "ffmpeg stderr closed (process ended)");
    })
}

/// Spawn `cmd[0]` with `cmd[1..]`: stdin null (FFmpeg must never read the
/// recorder's IPC stdin), stderr piped for `spawn_stderr_drain`, stdout as given.
/// `high_priority` raises the capture process above normal priority.
pub fn spawn_ffmpeg(cmd: &[String], stdout: Stdio, high_priority: bool) -> Result<Child> {
    use std::process::Command;
    let exe = cmd.first().ok_or_else(|| anyhow!("Empty command"))?;
    let mut command = Command::new(exe);
    command.args(&cmd[1..]).stdin(Stdio::null()).stdout(stdout).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command.spawn().map_err(|e| anyhow!("Spawn failed ({exe}): {e}"))?;
    #[cfg(windows)]
    if high_priority {
        elevate_priority(child.id());
    }
    #[cfg(not(windows))]
    let _ = high_priority;
    Ok(child)
}

#[cfg(windows)]
fn elevate_priority(pid: u32) {
    use windows::Win32::System::Threading::{OpenProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, PROCESS_ALL_ACCESS};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_ALL_ACCESS, false, pid) {
            let _ = SetPriorityClass(h, HIGH_PRIORITY_CLASS);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_ffmpeg_output_on_cr_and_lf() {
        let lines = drain_lines(&b"Input #0\nframe=  1 fps=0\rframe=  2 fps=1\r\n[ddagrab] AcquireNextFrame failed: 887a0026\n"[..]);
        assert_eq!(lines, vec!["Input #0", "frame=  1 fps=0", "frame=  2 fps=1", "[ddagrab] AcquireNextFrame failed: 887a0026"]);
    }

    #[test]
    fn tail_keeps_last_lines_only() {
        let tail = StderrTail::default();
        for i in 0..100 { tail.push(format!("line {i}")); }
        let text = tail.text();
        assert!(text.starts_with(&format!("line {}", 100 - StderrTail::CAPACITY)));
        assert!(text.ends_with("line 99"));
    }

    #[test]
    fn last_messages_skip_progress_lines() {
        let tail = StderrTail::default();
        for l in ["a", "b", "c", "frame=  1 fps=0", "d", "size=  10kB time=00:00:01"] {
            tail.push(l.into());
        }
        assert_eq!(tail.last_messages(3), vec!["b", "c", "d"]);
        assert_eq!(tail.last_messages(10), vec!["a", "b", "c", "d"]);
    }
}
