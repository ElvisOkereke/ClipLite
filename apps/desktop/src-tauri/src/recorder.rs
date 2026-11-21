//! The recorder process (`cliplite-recorder.exe`) and the IPC client that
//! drives its single capture engine.
//!
//! One isolated recorder process serves everything (replay buffer and
//! recordings). `EngineClient` writes commands to its stdin and a reader
//! thread routes every event on its stdout: an event whose `cid` matches a
//! pending request answers that request; anything else (`EngineRestarted`, a
//! recording finalized by `StopEngine`, a response that arrived after its
//! request timed out) goes to the `on_unsolicited` callback.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use cliplite_core::ipc::{Command, Envelope, Event};
use cliplite_core::telemetry::Stage;
use serde::{Deserialize, Serialize};

// ── RecorderChild — Win32 process + pipe handles ─────────────────────────────
//
// Spawned with our own CreateProcessW call so that NO D3D/GPU handles from
// the Tauri/WebView2 process leak into the recorder (handle whitelist), and
// outside Tauri's job object.

#[cfg(windows)]
pub struct RecorderChild {
    process_handle: windows::Win32::Foundation::HANDLE,
    thread_handle:  windows::Win32::Foundation::HANDLE,
    pub pid:        u32,
    /// Write end of the recorder's stdin. The mutex serializes writers so
    /// lines never interleave; null once closed.
    stdin_write:    Mutex<windows::Win32::Foundation::HANDLE>,
    /// Read end of the recorder's stdout; only the reader thread reads it.
    stdout_read:    windows::Win32::Foundation::HANDLE,
}

// SAFETY: HANDLEs are plain kernel handle values, usable from any thread.
// stdin writes are serialized by the mutex; stdout is read by one thread.
#[cfg(windows)]
unsafe impl Send for RecorderChild {}
#[cfg(windows)]
unsafe impl Sync for RecorderChild {}

#[cfg(windows)]
impl RecorderChild {
    /// Spawn the recorder with Win32 CreateProcessW and
    /// PROC_THREAD_ATTRIBUTE_HANDLE_LIST set to ONLY the two pipe ends.
    ///
    /// Without this, the NVIDIA driver sees the recorder as part of the same
    /// D3D device family as WebView2, causing 460 ms periodic stalls in ddagrab.
    pub fn spawn_isolated(recorder_exe: &std::path::Path) -> Result<Self> {
        use anyhow::anyhow;
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::{
            Foundation::{CloseHandle, SetHandleInformation, BOOL, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT},
            Security::SECURITY_ATTRIBUTES,
            System::{
                Pipes::CreatePipe,
                Threading::{
                    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
                    SetPriorityClass, UpdateProcThreadAttribute, ABOVE_NORMAL_PRIORITY_CLASS,
                    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
                    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS,
                    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
                },
            },
        };

        // Inheritable pipes; the parent-side ends are made non-inheritable below.
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: BOOL(1),
        };
        let (mut stdin_read, mut stdin_write) = (HANDLE::default(), HANDLE::default());
        let (mut stdout_read, mut stdout_write) = (HANDLE::default(), HANDLE::default());
        unsafe {
            CreatePipe(&mut stdin_read, &mut stdin_write, Some(&sa), 0)
                .map_err(|e| anyhow!("CreatePipe stdin: {e}"))?;
            if let Err(e) = CreatePipe(&mut stdout_read, &mut stdout_write, Some(&sa), 0) {
                CloseHandle(stdin_read).ok();
                CloseHandle(stdin_write).ok();
                bail!("CreatePipe stdout: {e}");
            }
            SetHandleInformation(stdin_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
            SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
        }
        // From here on `child` owns the parent-side ends (closed by Drop on error).
        let mut child = RecorderChild {
            process_handle: HANDLE::default(),
            thread_handle:  HANDLE::default(),
            pid:            0,
            stdin_write:    Mutex::new(stdin_write),
            stdout_read,
        };
        let close_child_ends = || unsafe {
            CloseHandle(stdin_read).ok();
            CloseHandle(stdout_write).ok();
        };

        // PROC_THREAD_ATTRIBUTE_LIST whitelisting ONLY the two child-side pipe ends.
        let mut attr_size: usize = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()), 1, 0, &mut attr_size);
        }
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);
        if let Err(e) = unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) } {
            close_child_ends();
            bail!("InitializeProcThreadAttributeList: {e}");
        }
        // PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002: every other handle
        // (D3D11 devices, WebView2 GPU contexts) stays in this process.
        let inherit_handles: [HANDLE; 2] = [stdin_read, stdout_write];
        const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x00020002;
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attr_list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                Some(inherit_handles.as_ptr() as *const std::ffi::c_void),
                std::mem::size_of_val(&inherit_handles), None, None,
            )
        };
        if let Err(e) = updated {
            unsafe { DeleteProcThreadAttributeList(attr_list) };
            close_child_ends();
            bail!("UpdateProcThreadAttribute: {e}");
        }

        let mut si_ex = STARTUPINFOEXW::default();
        si_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si_ex.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si_ex.StartupInfo.hStdInput = stdin_read;
        si_ex.StartupInfo.hStdOutput = stdout_write;
        si_ex.StartupInfo.hStdError = HANDLE::default();
        si_ex.lpAttributeList = attr_list;

        let exe_wide: Vec<u16> = OsStr::new(recorder_exe).encode_wide().chain([0]).collect();
        let mut pi = PROCESS_INFORMATION::default();
        // CREATE_BREAKAWAY_FROM_JOB is the critical flag: Tauri's job object
        // applies GPU scheduling constraints that make the NVIDIA audio driver
        // conflict with ddagrab's D3D lock (the 460 ms stall pattern). Breaking
        // away gives the recorder the same clean context as a terminal launch.
        let base = EXTENDED_STARTUPINFO_PRESENT.0 | CREATE_NEW_PROCESS_GROUP.0 | CREATE_NO_WINDOW.0;
        let mut create = |flags: u32| unsafe {
            CreateProcessW(
                windows::core::PCWSTR(exe_wide.as_ptr()),
                windows::core::PWSTR::null(),
                None, None,
                BOOL(1), // bInheritHandles: only the whitelisted handles cross over
                PROCESS_CREATION_FLAGS(flags),
                None, None,
                &si_ex.StartupInfo as *const STARTUPINFOW,
                &mut pi,
            )
        };
        let mut created = create(base | CREATE_BREAKAWAY_FROM_JOB.0);
        // A parent job without JOB_OBJECT_LIMIT_BREAKAWAY_OK (e.g. cargo's, for
        // `cargo test`/`tauri dev`) refuses the breakaway: spawn inside it
        // rather than not at all. The handle whitelist still applies.
        if matches!(&created, Err(e) if e.code() == windows::Win32::Foundation::E_ACCESSDENIED) {
            log::warn!("[recorder] parent job refuses CREATE_BREAKAWAY_FROM_JOB; spawning the recorder inside it");
            created = create(base);
        }
        unsafe { DeleteProcThreadAttributeList(attr_list) };
        close_child_ends();
        created.map_err(|e| anyhow!("CreateProcessW {}: {e}", recorder_exe.display()))?;
        unsafe { SetPriorityClass(pi.hProcess, ABOVE_NORMAL_PRIORITY_CLASS).ok() };

        child.process_handle = pi.hProcess;
        child.thread_handle = pi.hThread;
        child.pid = pi.dwProcessId;
        log::info!("[recorder] spawned cliplite-recorder pid={}", child.pid);
        Ok(child)
    }

    /// Write one newline-terminated line to the recorder's stdin, atomically
    /// with respect to other writers.
    pub fn send_line(&self, line: &str) -> Result<()> {
        use windows::Win32::Storage::FileSystem::WriteFile;
        let h = self.stdin_write.lock().unwrap_or_else(|e| e.into_inner());
        if h.0.is_null() {
            bail!("recorder stdin is closed");
        }
        let buf = format!("{line}\n");
        let mut rest = buf.as_bytes();
        while !rest.is_empty() {
            let mut written = 0u32;
            if let Err(e) = unsafe { WriteFile(*h, Some(rest), Some(&mut written), None) } {
                bail!("write to recorder: {e}");
            }
            rest = &rest[written as usize..];
        }
        Ok(())
    }

    /// Close stdin: the recorder stops its engine on EOF and exits.
    pub fn close_stdin(&self) {
        let mut h = self.stdin_write.lock().unwrap_or_else(|e| e.into_inner());
        if !h.0.is_null() {
            unsafe { windows::Win32::Foundation::CloseHandle(*h).ok() };
            *h = windows::Win32::Foundation::HANDLE(std::ptr::null_mut());
        }
    }

    /// Blocking read of the next bytes from the recorder's stdout; `None` once
    /// the pipe is closed (the recorder exited).
    fn read_chunk(&self, buf: &mut [u8]) -> Option<usize> {
        use windows::Win32::Storage::FileSystem::ReadFile;
        let mut read = 0u32;
        let ok = unsafe { ReadFile(self.stdout_read, Some(buf), Some(&mut read), None).is_ok() };
        (ok && read > 0).then_some(read as usize)
    }

    /// `Some(exit_code)` once the process has exited.
    pub fn try_wait(&self) -> Option<u32> {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        unsafe {
            if WaitForSingleObject(self.process_handle, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.process_handle, &mut code).ok();
            Some(code)
        }
    }

    #[cfg(test)]
    pub fn kill(&self) {
        unsafe { windows::Win32::System::Threading::TerminateProcess(self.process_handle, 1).ok() };
    }
}

#[cfg(windows)]
impl Drop for RecorderChild {
    fn drop(&mut self) {
        self.close_stdin();
        for h in [self.stdout_read, self.thread_handle, self.process_handle] {
            if !h.0.is_null() {
                unsafe { windows::Win32::Foundation::CloseHandle(h).ok() };
            }
        }
    }
}

#[cfg(not(windows))]
pub struct RecorderChild {
    pub pid: u32,
}

#[cfg(not(windows))]
impl RecorderChild {
    pub fn spawn_isolated(_: &std::path::Path) -> Result<Self> {
        bail!("the recorder process is only supported on Windows")
    }
    pub fn send_line(&self, _: &str) -> Result<()> { bail!("unsupported") }
    pub fn close_stdin(&self) {}
    fn read_chunk(&self, _: &mut [u8]) -> Option<usize> { None }
    pub fn try_wait(&self) -> Option<u32> { Some(0) }
    #[cfg(test)]
    pub fn kill(&self) {}
}

// ── IPC helpers ──────────────────────────────────────────────────────────────

/// Serialize one IPC command with its correlation id as a single JSON line.
pub(crate) fn ipc_envelope_line(cmd: &Command, cid: &str) -> String {
    let env = Envelope { body: cmd.clone(), cid: Some(cid.to_string()) };
    serde_json::to_string(&env).expect("IPC envelope always serializes")
}

fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::StartEngine { .. } => "start_engine",
        Command::StopEngine => "stop_engine",
        Command::SetLimits { .. } => "set_limits",
        Command::SaveReplay { .. } => "save_replay",
        Command::StartRecording { .. } => "start_recording",
        Command::StopRecording => "stop_recording",
        Command::Pause => "pause",
        Command::Resume => "resume",
        Command::Status => "status",
    }
}

/// Requests waiting for their response, by cid.
pub type Pending = Mutex<HashMap<String, mpsc::Sender<Event>>>;

/// Route one stdout line: to the request with the same cid, else to
/// `on_unsolicited`. Unparseable lines are logged and dropped.
pub fn route_line(line: &str, pending: &Pending, on_unsolicited: &dyn Fn(Event)) {
    let env: Envelope<Event> = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("[recorder] unparseable IPC line ({e}): {line}");
            return;
        }
    };
    let waiter = env.cid.as_deref().and_then(|cid| pending.lock().unwrap_or_else(|e| e.into_inner()).remove(cid));
    match waiter {
        Some(tx) => {
            // The requester gave up between our lookup and now: treat as unsolicited.
            if let Err(mpsc::SendError(ev)) = tx.send(env.body) {
                on_unsolicited(ev);
            }
        }
        None => {
            tracing::debug!(cid = env.cid.as_deref().unwrap_or(""), "unsolicited recorder event");
            on_unsolicited(env.body);
        }
    }
}

// ── EngineClient ─────────────────────────────────────────────────────────────

/// One recorder process plus its reader thread. Requests may be issued
/// concurrently from any thread; each waits for the event carrying its cid.
pub struct EngineClient {
    child:       Arc<RecorderChild>,
    pending:     Arc<Pending>,
    reader_done: Arc<AtomicBool>,
}

impl EngineClient {
    pub fn spawn(recorder_exe: &std::path::Path, on_unsolicited: impl Fn(Event) + Send + 'static) -> Result<Self> {
        let child = Arc::new(RecorderChild::spawn_isolated(recorder_exe)?);
        let pending: Arc<Pending> = Arc::default();
        let reader_done = Arc::new(AtomicBool::new(false));
        let (c, p, done) = (child.clone(), pending.clone(), reader_done.clone());
        std::thread::Builder::new()
            .name("recorder-ipc-reader".into())
            .spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 8192];
                while let Some(n) = c.read_chunk(&mut chunk) {
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let raw: Vec<u8> = buf.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&raw);
                        let line = line.trim();
                        if !line.is_empty() {
                            route_line(line, &p, &on_unsolicited);
                        }
                    }
                }
                // Order matters (see `request`): mark done, then fail the waiters.
                done.store(true, Ordering::SeqCst);
                p.lock().unwrap_or_else(|e| e.into_inner()).clear();
                log::warn!("[recorder] pid={} closed its stdout (exit code {:?})", c.pid, c.try_wait());
            })?;
        Ok(Self { child, pending, reader_done })
    }

    pub fn pid(&self) -> u32 {
        self.child.pid
    }

    /// The process is running and its stdout is still open.
    pub fn is_alive(&self) -> bool {
        !self.reader_done.load(Ordering::SeqCst) && self.child.try_wait().is_none()
    }

    /// Send with cid; wait for the event carrying the same cid.
    pub fn request(&self, cmd: Command, cid: &str, timeout: Duration) -> Result<Event> {
        let name = command_name(&cmd);
        let (tx, rx) = mpsc::channel();
        {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if p.contains_key(cid) {
                bail!("a request with cid {cid} is already in flight");
            }
            p.insert(cid.to_string(), tx);
        }
        // Checked after inserting: the reader marks done before it clears
        // `pending`, so either we see done here or our sender gets dropped.
        if self.reader_done.load(Ordering::SeqCst) {
            self.forget(cid);
            bail!("recorder process is not running");
        }
        tracing::info!(stage = Stage::IpcSent.as_str(), cid = %cid, recorder_pid = self.child.pid,
                       cmd = name, "ipc sent");
        if let Err(e) = self.child.send_line(&ipc_envelope_line(&cmd, cid)) {
            self.forget(cid);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(ev) => {
                tracing::debug!(cid = %cid, cmd = name, reply = ?ev, "ipc reply");
                Ok(ev)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.forget(cid);
                bail!("recorder did not answer {name} within {} s", timeout.as_secs_f64())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("recorder process exited before answering {name}"),
        }
    }

    fn forget(&self, cid: &str) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
    }

    #[cfg(test)]
    pub fn kill_for_test(&self) {
        self.child.kill();
    }
}

impl Drop for EngineClient {
    /// EOF on stdin: the recorder stops its engine (finalizing any recording) and exits.
    fn drop(&mut self) {
        self.child.close_stdin();
    }
}

// ── RecordingStatus ──────────────────────────────────────────────────────────

/// What `get_recording_status` returns to the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordingStatus {
    pub is_recording:    bool,
    pub is_paused:       bool,
    pub elapsed_seconds: f64,
    pub output_path:     Option<String>,
    pub replay_active:   bool,
    pub buffered_secs:   f64,
    pub restarts:        u32,
}

impl RecordingStatus {
    /// Map the engine's `Status` event. The replay buffer is active only while
    /// the engine runs and the user has it on (it can be marked off while the
    /// engine stays up for a recording).
    pub fn from_event(ev: &Event, buffer_on: bool, output_path: Option<String>) -> Option<Self> {
        let Event::Status { engine_running, recording, paused, elapsed, buffered_secs, restarts, .. } = ev else {
            return None;
        };
        Some(Self {
            is_recording:    *recording,
            is_paused:       *recording && *paused,
            elapsed_seconds: if *recording { *elapsed } else { 0.0 },
            output_path,
            replay_active:   *engine_running && buffer_on,
            buffered_secs:   *buffered_secs,
            restarts:        *restarts,
        })
    }
}

// ── RecorderState ────────────────────────────────────────────────────────────

/// UI-side engine state. The lock is only held briefly, never across a request.
#[derive(Default)]
pub struct RecorderInner {
    /// The recorder process; respawned by the next command if it died.
    pub client:         Option<Arc<EngineClient>>,
    /// The user wants the replay buffer (saves are refused while false).
    pub buffer_on:      bool,
    /// Output path of the recording in progress.
    pub recording_path: Option<String>,
    /// Last status from the engine, served when a status request times out.
    pub last_status:    RecordingStatus,
}

#[derive(Default)]
pub struct RecorderState(pub Mutex<RecorderInner>);

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
