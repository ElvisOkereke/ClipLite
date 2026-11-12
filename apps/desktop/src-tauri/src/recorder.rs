/// Recording process management — pure Rust.
use std::sync::Mutex;
use std::time::Instant;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

// ── RecorderChild — Win32 handle wrapper ─────────────────────────────────────
//
// Wraps the process and pipe handles produced by our custom CreateProcessW
// call (with bInheritHandles whitelist). This ensures NO D3D/GPU handles
// from the Tauri/WebView2 process leak into the recorder subprocess.

#[cfg(windows)]
pub struct RecorderChild {
    pub process_handle: windows::Win32::Foundation::HANDLE,
    pub thread_handle:  windows::Win32::Foundation::HANDLE,
    pub pid:            u32,
    /// Write end of stdin pipe — Tauri writes IPC commands here
    pub stdin_write:    windows::Win32::Foundation::HANDLE,
    /// Read end of stdout pipe — Tauri reads IPC events here
    pub stdout_read:    windows::Win32::Foundation::HANDLE,
    /// Internal byte buffer for assembling complete newline-delimited lines
    /// from the raw pipe. Wrapped in Mutex so read_line can take &self.
    pub read_buf:       std::sync::Mutex<Vec<u8>>,
}

#[cfg(windows)]
impl RecorderChild {
    /// Send a newline-terminated JSON string to the recorder's stdin.
    pub fn send_line(&self, line: &str) -> Result<()> {
        use windows::Win32::Storage::FileSystem::WriteFile;
        let bytes = line.as_bytes();
        let mut written = 0u32;
        unsafe {
            WriteFile(self.stdin_write, Some(bytes), Some(&mut written), None)
                .map_err(|e| anyhow!("WriteFile: {e}"))?;
            let nl = b"\n";
            WriteFile(self.stdin_write, Some(nl), Some(&mut written), None).ok();
        }
        Ok(())
    }

    /// Close the stdin write handle, sending EOF to the recorder's stdin reader.
    /// The recorder's main loop exits on EOF, triggering a clean shutdown.
    pub fn close_stdin(&mut self) {
        let h = self.stdin_write;
        // HANDLE(*mut c_void): null = closed/invalid
        if !h.0.is_null() {
            unsafe { windows::Win32::Foundation::CloseHandle(h).ok(); }
            self.stdin_write = windows::Win32::Foundation::HANDLE(std::ptr::null_mut());
        }
    }

    /// Read exactly one newline-terminated line from the recorder's stdout (blocking).
    /// Accumulates bytes in an internal buffer so partial reads and multi-line
    /// chunks (common when FFmpeg flushes large bursts) are handled correctly.
    pub fn read_line(&self) -> Option<String> {
        use windows::Win32::Storage::FileSystem::ReadFile;
        let mut internal = self.read_buf.lock().unwrap();

        loop {
            // If we already have a complete line in the buffer, return it now.
            if let Some(pos) = internal.iter().position(|&b| b == b'\n') {
                let line_bytes = internal.drain(..=pos).collect::<Vec<u8>>();
                let line = String::from_utf8_lossy(&line_bytes)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                if !line.is_empty() {
                    return Some(line);
                }
                continue; // skip blank lines, keep looking
            }

            // Need more bytes — read from the pipe.
            let mut chunk = vec![0u8; 8192];
            let mut read = 0u32;
            let ok = unsafe {
                ReadFile(self.stdout_read, Some(&mut chunk), Some(&mut read), None).is_ok()
            };
            if !ok || read == 0 {
                // Pipe closed (process exited) — return whatever we have if non-empty.
                if !internal.is_empty() {
                    let remaining = String::from_utf8_lossy(&internal)
                        .trim_end_matches(['\r', '\n'])
                        .to_string();
                    internal.clear();
                    if !remaining.is_empty() { return Some(remaining); }
                }
                return None;
            }
            internal.extend_from_slice(&chunk[..read as usize]);
        }
    }

    /// Non-blocking check if the process has exited. Returns Some(exit_code) if done.
    pub fn try_wait(&self) -> Option<u32> {
        use windows::Win32::System::Threading::WaitForSingleObject;
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        unsafe {
            let r = WaitForSingleObject(self.process_handle, 0);
            if r == WAIT_OBJECT_0 {
                let mut code = 0u32;
                windows::Win32::System::Threading::GetExitCodeProcess(
                    self.process_handle, &mut code
                ).ok();
                Some(code)
            } else {
                None
            }
        }
    }


    pub fn kill(&self) {
        unsafe {
            windows::Win32::System::Threading::TerminateProcess(
                self.process_handle, 1
            ).ok();
        }
    }
}

#[cfg(windows)]
impl Drop for RecorderChild {
    fn drop(&mut self) {
        // close_stdin() zeroes the handle after closing; check before closing again
        let close = |h: windows::Win32::Foundation::HANDLE| {
            if !h.0.is_null() {
                unsafe { windows::Win32::Foundation::CloseHandle(h).ok(); }
            }
        };
        close(self.stdin_write);
        close(self.stdout_read);
        close(self.thread_handle);
        close(self.process_handle);
    }
}

// SAFETY: RecorderChild contains Windows HANDLE values (pointer-sized integers).
// HANDLEs are safe to send between threads; we never use them concurrently
// because RecorderInner is behind a Mutex.
#[cfg(windows)]
unsafe impl Send for RecorderChild {}
#[cfg(windows)]
unsafe impl Sync for RecorderChild {}

// Stub for non-Windows
#[cfg(not(windows))]
pub struct RecorderChild;
#[cfg(not(windows))]
impl RecorderChild {
    pub fn send_line(&self, _: &str) -> Result<()> { Ok(()) }
    pub fn read_line(&self) -> Option<String> { None }
    pub fn try_wait(&self) -> Option<u32> { None }
    pub fn kill(&self) {}
}

// ── RecordingStatus ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStatus {
    pub is_recording: bool,
    pub is_paused: bool,
    pub elapsed_seconds: f64,
    pub output_path: Option<String>,
    pub replay_active: bool,
}

// ── RecorderInner ────────────────────────────────────────────────────────────

pub struct RecorderInner {
    pub output_path:      Option<String>,
    pub start_time:       Option<Instant>,
    pub paused_duration:  f64,
    pub pause_start:      Option<Instant>,
    pub is_paused:        bool,


    // Recorder subprocess via Win32 (preferred — no handle inheritance)
    pub recorder_child: Option<RecorderChild>,

    // Replay buffer subprocess — always a separate isolated process so stopping
    // recording never kills the replay buffer and vice versa.
    // Wrapped in Arc so save_replay can hold a reference without holding the mutex.
    pub replay_child: Option<std::sync::Arc<RecorderChild>>,

}

impl Default for RecorderInner {
    fn default() -> Self {
        Self {
            output_path:     None,
            start_time:      None,
            paused_duration: 0.0,
            pause_start:     None,
            is_paused:       false,
            recorder_child:  None,
            replay_child:    None,
        }
    }
}

#[derive(Default)]
pub struct RecorderState(pub Mutex<RecorderInner>);

impl RecorderInner {
    // ── Status ────────────────────────────────────────────────────────────────

    pub fn status(&mut self) -> RecordingStatus {
        // Check Win32 recorder child (main recording only — replay_child is separate)
        let alive_win32 = if let Some(child) = &self.recorder_child {
            child.try_wait().is_none()
        } else {
            false
        };

        let alive = alive_win32;

        if !alive && self.recorder_child.is_some() {
            self.recorder_child = None;
            self.start_time = None;
        }

        // Prune dead replay child too
        if let Some(rc) = &self.replay_child {
            if rc.try_wait().is_some() {
                self.replay_child = None;
            }
        }

        let replay_active = if let Some(rc) = &self.replay_child {
            rc.try_wait().is_none()
        } else {
            false
        };

        RecordingStatus {
            is_recording:    alive,
            is_paused:       self.is_paused,
            elapsed_seconds: if alive { self.elapsed_seconds() } else { 0.0 },
            output_path:     self.output_path.clone(),
            replay_active,
        }
    }

    fn elapsed_seconds(&self) -> f64 {
        let Some(start) = self.start_time else { return 0.0 };
        let elapsed = start.elapsed().as_secs_f64() - self.paused_duration;
        if self.is_paused {
            if let Some(ps) = self.pause_start {
                return (elapsed - ps.elapsed().as_secs_f64()).max(0.0);
            }
        }
        elapsed.max(0.0)
    }

    // ── Pause / resume (via recorder child) ────────────────────────────────────

    pub fn pause(&mut self) -> Result<()> {
        if self.is_paused { return Ok(()); }
        // Pause via recorder child if available
        if let Some(child) = &self.recorder_child {
            child.send_line(&crate::commands::ipc_line(&cliplite_core::ipc::Command::Pause)).ok();
            self.pause_start = Some(Instant::now());
            self.is_paused   = true;
            return Ok(());
        }
        Err(anyhow!("Not recording"))
    }

    pub fn resume(&mut self) -> Result<()> {
        if !self.is_paused { return Ok(()); }
        if let Some(child) = &self.recorder_child {
            child.send_line(&crate::commands::ipc_line(&cliplite_core::ipc::Command::Resume)).ok();
            if let Some(ps) = self.pause_start.take() {
                self.paused_duration += ps.elapsed().as_secs_f64();
            }
            self.is_paused = false;
            return Ok(());
        }
        Err(anyhow!("Not recording"))
    }
}
