/// Tauri command handlers — pure Rust, no Python, no external scripts.
use std::path::PathBuf;
use std::process::Command;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    audio, ffmpeg,
    library::{self, ClipMeta},
    recorder::{RecorderState, RecorderChild, RecordingStatus},
    settings::{Settings, SettingsState},
    tray,
    EncoderCache,
};

// ── Shared types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub x:       i32,
    pub y:       i32,
    pub width:   i32,
    pub height:  i32,
    pub monitor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Legacy dshow name — kept for compatibility, not used for capture.
    pub mic_device:    Option<String>,
    /// WASAPI endpoint ID for system audio loopback.
    pub sys_device_id: Option<String>,
    /// WASAPI endpoint ID for microphone — captured in Rust, no dshow.
    pub mic_device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncConfig {
    pub fps:         u32,
    pub quality_crf: Option<u32>,
    pub format:      Option<String>,
    pub hw_encoder:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index:        u32,
    pub name:         String,
    pub x:            i32,
    pub y:            i32,
    pub width:        u32,
    pub height:       u32,
    pub is_primary:   bool,
    pub refresh_rate: f64,
}

/// Named pipe the replay recorder uses for audio (recording uses the default).
pub(crate) const REPLAY_AUDIO_PIPE: &str = r"\\.\pipe\cliplite_sysaudio_replay";

/// Convert UI-facing capture options into the recorder's IPC start args,
/// applying the defaults the UI has always relied on.
pub(crate) fn to_start_args(
    ffmpeg_path: &std::path::Path,
    output_path: &str,
    region: &Region,
    audio: &AudioConfig,
    enc: &EncConfig,
    audio_pipe: Option<&str>,
    max_replay_buffer_size_mb: u32,
) -> cliplite_core::ipc::StartArgs {
    use cliplite_core::ipc;
    ipc::StartArgs {
        ffmpeg_path: ffmpeg_path.to_string_lossy().into_owned(),
        output_path: output_path.to_string(),
        region: ipc::Region {
            x: region.x, y: region.y, width: region.width, height: region.height, monitor: region.monitor,
        },
        audio_cfg: ipc::AudioConfig {
            mic_device: audio.mic_device.clone(),
            sys_device_id: audio.sys_device_id.clone(),
            mic_device_id: audio.mic_device_id.clone(),
        },
        enc_cfg: ipc::EncConfig {
            fps: enc.fps,
            quality_crf: enc.quality_crf.unwrap_or(28),
            format: enc.format.clone().unwrap_or_else(|| "mp4".into()),
            hw_encoder: enc.hw_encoder.clone().unwrap_or_else(|| "h264_nvenc".into()),
        },
        audio_pipe: audio_pipe.map(String::from),
        max_replay_buffer_size_mb,
    }
}

/// Serialize one IPC command as a single JSON line (no trailing newline).
pub(crate) fn ipc_line(cmd: &cliplite_core::ipc::Command) -> String {
    serde_json::to_string(cmd).expect("IPC command always serializes")
}

// ── FFmpeg ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn find_ffmpeg() -> Result<String, String> {
    ffmpeg::find_ffmpeg()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn detect_hw_encoder() -> String {
    ffmpeg::detect_hw_encoder()
}

// ── Recorder spawn (Win32, no handle inheritance) ─────────────────────────────

/// Spawn cliplite-recorder.exe using Win32 CreateProcessW with
/// PROC_THREAD_ATTRIBUTE_HANDLE_LIST set to ONLY the pipe handles.
///
/// This prevents any D3D11/GPU handles from the Tauri/WebView2 process
/// from being inherited by the recorder subprocess. Without this,
/// the NVIDIA driver sees the recorder as part of the same D3D device
/// family as WebView2, causing 460ms periodic stalls in ddagrab.
#[cfg(windows)]
fn spawn_recorder_isolated(
    recorder_exe: &std::path::Path,
) -> anyhow::Result<RecorderChild> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE, BOOL, SetHandleInformation, HANDLE_FLAG_INHERIT},
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes::CreatePipe,
            Threading::{
                CreateProcessW, InitializeProcThreadAttributeList,
                UpdateProcThreadAttribute, DeleteProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS,
                EXTENDED_STARTUPINFO_PRESENT, CREATE_NEW_PROCESS_GROUP,
                CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW,
                PROCESS_INFORMATION, STARTUPINFOW, STARTUPINFOEXW,
                SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
                STARTF_USESTDHANDLES,
            },
        },
    };

    // Create inheritable pipe security attributes
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: BOOL(1),
    };

    // stdin pipe: Tauri writes commands → recorder reads
    let mut stdin_read  = HANDLE::default();
    let mut stdin_write = HANDLE::default();
    // stdout pipe: recorder writes events → Tauri reads
    let mut stdout_read  = HANDLE::default();
    let mut stdout_write = HANDLE::default();

    unsafe {
        CreatePipe(&mut stdin_read,  &mut stdin_write, Some(&sa), 0)
            .map_err(|e| anyhow::anyhow!("CreatePipe stdin: {e}"))?;
        CreatePipe(&mut stdout_read, &mut stdout_write, Some(&sa), 0)
            .map_err(|e| anyhow::anyhow!("CreatePipe stdout: {e}"))?;

        // Mark parent-side handles non-inheritable
        // (child only gets stdin_read + stdout_write via the attribute whitelist)
        use windows::Win32::Foundation::HANDLE_FLAGS;
        SetHandleInformation(stdin_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
        SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)).ok();
    }

    // Build PROC_THREAD_ATTRIBUTE_LIST that whitelists ONLY the two pipe ends
    let mut attr_size: usize = 0;
    unsafe {
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            1, 0, &mut attr_size,
        );
    }

    let mut attr_buf = vec![0u8; attr_size];
    let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);

    unsafe {
        InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size)
            .map_err(|e| anyhow::anyhow!("InitializeProcThreadAttributeList: {e}"))?;
    }

    // PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002
    // Only stdin_read and stdout_write cross the process boundary.
    // Every other handle (D3D11 devices, WebView2 GPU contexts) is blocked.
    let inherit_handles: [HANDLE; 2] = [stdin_read, stdout_write];
    const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x00020002;

    unsafe {
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            Some(inherit_handles.as_ptr() as *const std::ffi::c_void),
            std::mem::size_of_val(&inherit_handles),
            None,
            None,
        ).map_err(|e| anyhow::anyhow!("UpdateProcThreadAttribute: {e}"))?;
    }

    // Build STARTUPINFOEXW
    let mut si_ex = STARTUPINFOEXW::default();
    si_ex.StartupInfo.cb      = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si_ex.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si_ex.StartupInfo.hStdInput  = stdin_read;
    si_ex.StartupInfo.hStdOutput = stdout_write;
    si_ex.StartupInfo.hStdError  = HANDLE::default();
    si_ex.lpAttributeList = attr_list;

    // Build executable path as wide string (CreateProcessW reads from lpApplicationName)
    let exe_wide: Vec<u16> = OsStr::new(recorder_exe)
        .encode_wide().chain([0]).collect();

    let mut pi = PROCESS_INFORMATION::default();
    // CREATE_BREAKAWAY_FROM_JOB is the critical flag.
    // Tauri associates all its child processes with a Windows Job Object for
    // lifecycle management. This Job Object applies GPU scheduling constraints
    // that cause the NVIDIA audio driver to conflict with ddagrab's D3D lock,
    // producing the 460ms stall pattern. BREAKAWAY_FROM_JOB removes the
    // recorder from Tauri's Job Object, giving it the same clean scheduling
    // context as a process spawned directly from a terminal.
    let flags = PROCESS_CREATION_FLAGS(
        EXTENDED_STARTUPINFO_PRESENT.0
        | CREATE_NEW_PROCESS_GROUP.0
        | CREATE_BREAKAWAY_FROM_JOB.0
        | CREATE_NO_WINDOW.0          // suppress console window flash
    );

    unsafe {
        CreateProcessW(
            windows::core::PCWSTR(exe_wide.as_ptr()),
            // lpCommandLine must be PWSTR (mutable); None = use lpApplicationName only
            windows::core::PWSTR::null(),
            None, None,
            BOOL(1), // bInheritHandles — only whitelisted handles cross over
            flags,
            None, None,
            // Cast STARTUPINFOEXW* → STARTUPINFOW* (required with EXTENDED_STARTUPINFO_PRESENT)
            &si_ex.StartupInfo as *const STARTUPINFOW,
            &mut pi,
        ).map_err(|e| anyhow::anyhow!("CreateProcessW: {e}"))?;

        DeleteProcThreadAttributeList(attr_list);
        CloseHandle(stdin_read).ok();
        CloseHandle(stdout_write).ok();
        SetPriorityClass(pi.hProcess, ABOVE_NORMAL_PRIORITY_CLASS).ok();
    }

    let child = RecorderChild {
        process_handle: pi.hProcess,
        thread_handle:  pi.hThread,
        pid:            pi.dwProcessId,
        stdin_write,
        stdout_read,
        read_buf:       std::sync::Mutex::new(Vec::new()),
    };
    log::info!("[recorder] spawned cliplite-recorder pid={}", child.pid);
    Ok(child)
}

// ── Find recorder binary ──────────────────────────────────────────────────────

fn find_recorder() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();

    #[cfg(windows)] let name = "cliplite-recorder.exe";
    #[cfg(not(windows))] let name = "cliplite-recorder";

    let p = dir.join(name);
    if p.exists() { Some(p) } else { None }
}

// ── Recording ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_recording(
    app:            AppHandle,
    region:         Region,
    audio_cfg:      AudioConfig,
    enc_cfg:        EncConfig,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
    encoder_cache:  State<'_, EncoderCache>,
) -> Result<String, String> {
    let settings = settings_state.0.lock().unwrap().clone();

    // Read encoder from cache (detected at startup — no blocking test encode)
    let cached_encoder = encoder_cache.inner().0.lock().unwrap().clone();
    let hw_encoder = {
        let from_enc_cfg = enc_cfg.hw_encoder.as_deref().unwrap_or("auto");
        if from_enc_cfg == "auto" || from_enc_cfg.is_empty() {
            if cached_encoder.is_empty() {
                // Cache not ready yet (startup detection still running)
                if settings.hw_encoder == "auto" {
                    "h264_nvenc".to_string() // optimistic default
                } else {
                    settings.hw_encoder.clone()
                }
            } else {
                cached_encoder
            }
        } else {
            from_enc_cfg.to_string()
        }
    };

    let enc_cfg = EncConfig { hw_encoder: Some(hw_encoder), ..enc_cfg };

    // Build output path
    let ext = match enc_cfg.format.as_deref().unwrap_or("mp4") {
        "webm" => ".webm", "gif" => ".gif", _ => ".mp4",
    };
    let output_path = settings.build_output_path(ext);
    let output_str  = output_path.to_string_lossy().to_string();
    std::fs::create_dir_all(output_path.parent().unwrap_or(&output_path)).ok();

    let ffmpeg_path = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;

    // ── Preferred: spawn recorder with isolated Win32 CreateProcessW ──────────
    // No D3D handle inheritance → no 460ms stall pattern from WebView2's GPU context
    if let Some(recorder_exe) = find_recorder() {
        #[cfg(windows)]
        {
            let child = spawn_recorder_isolated(&recorder_exe)
                .map_err(|e| format!("Failed to spawn recorder: {e}"))?;

            // Send start command
            let cmd = cliplite_core::ipc::Command::Start {
                args: to_start_args(&ffmpeg_path, &output_str, &region, &audio_cfg, &enc_cfg, None, 0),
            };

            let line = ipc_line(&cmd);
            child.send_line(&line).map_err(|e| format!("IPC send failed: {e}"))?;

            // Store child handle — no blocking stdout read here
            {
                let mut rec = recorder.0.lock().unwrap();
                rec.recorder_child = Some(child);
                rec.output_path    = Some(output_str.clone());
                rec.start_time     = Some(std::time::Instant::now());
                rec.paused_duration = 0.0;
                rec.is_paused      = false;
            }

            tray::set_recording_state(&app, true);
            return Ok(output_str);
        }
    }

    // ── Error: recorder binary not found ─────────────────────────────────────
    // We do NOT fall back to direct FFmpeg — that produces choppy recordings.
    // The user needs to have cliplite-recorder.exe next to cliplite.exe.
    Err(format!(
        "cliplite-recorder.exe not found next to cliplite.exe.\n\
         Both files must be in the same directory.\n\
         Expected: {}",
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("cliplite-recorder.exe").to_string_lossy().into_owned()))
            .unwrap_or_default()
    ))
}

#[tauri::command]
pub async fn stop_recording(
    app:      AppHandle,
    recorder: State<'_, RecorderState>,
) -> Result<String, String> {
    let path = {
        let mut rec = recorder.0.lock().unwrap();

        // ── Win32 recorder child path ─────────────────────────────────────
        if let Some(mut child) = rec.recorder_child.take() {
            let output_path = rec.output_path.take().unwrap_or_default();
            rec.start_time = None;
            rec.is_paused  = false;

            // 1. Send stop command
            child.send_line(&ipc_line(&cliplite_core::ipc::Command::Stop)).ok();

            // 2. Close stdin → sends EOF to recorder's BufReader → main loop exits
            child.close_stdin();

            // 3. Read "stopped" event from stdout
            let mut final_path = output_path.clone();
            if let Some(line) = child.read_line() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if v["event"] == "stopped" {
                        final_path = v["path"].as_str().unwrap_or(&output_path).to_string();
                    }
                }
            }

            // 4. Wait for recorder to exit (writes moov atom then returns)
            let t0 = std::time::Instant::now();
            loop {
                if child.try_wait().is_some() { break; }
                if t0.elapsed().as_secs() > 25 { child.kill(); break; }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }

            // child drops here, remaining handles closed via Drop
            final_path
        }
        // ── No recorder child: nothing was recording ─────────────────────
        else {
            rec.output_path.take().unwrap_or_default()
        }
    };

    tray::set_recording_state(&app, false);

    // Add to library in background
    let path_clone = path.clone();
    if !path_clone.is_empty() && std::path::Path::new(&path_clone).exists() {
        let p = path_clone.clone();
        std::thread::spawn(move || {
            if let Ok(meta) = probe_file(&p) {
                if let Ok(clip) = library::add_clip_to_db(&p, &meta) {
                    if let Ok(ffmpeg) = ffmpeg::find_ffmpeg() {
                        if let Ok(thumb) = generate_thumb(&ffmpeg, &p, clip.id) {
                            let _ = library::update_thumbnail(clip.id, &thumb);
                        }
                    }
                }
            }
        });
    }

    Ok(path_clone)
}

#[tauri::command]
pub async fn pause_recording(recorder: State<'_, RecorderState>) -> Result<(), String> {
    recorder.0.lock().unwrap().pause().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resume_recording(recorder: State<'_, RecorderState>) -> Result<(), String> {
    recorder.0.lock().unwrap().resume().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_recording_status(recorder: State<'_, RecorderState>) -> RecordingStatus {
    recorder.0.lock().unwrap().status()
}

// ── Replay buffer ─────────────────────────────────────────────────────────────
//
// The replay buffer runs as a background recording in the same recorder process.
// The recorder binary keeps a rolling temp file; SaveReplay extracts the last
// N seconds using FFmpeg's -sseof (seek from end of file).


#[tauri::command]
pub async fn start_replay(
    _app:           AppHandle,
    region:         Region,
    audio_cfg:      AudioConfig,
    enc_cfg:        EncConfig,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
    encoder_cache:  State<'_, EncoderCache>,
) -> Result<String, String> {
    let settings       = settings_state.0.lock().unwrap().clone();
    let cached_encoder = encoder_cache.inner().0.lock().unwrap().clone();
    let hw_encoder     = if cached_encoder.is_empty() { "h264_nvenc".into() } else { cached_encoder };
    let enc_cfg        = EncConfig { hw_encoder: Some(hw_encoder), ..enc_cfg };
    let ffmpeg_path    = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;

    // Replay always spawns its OWN isolated process — completely independent from
    // the main recording child. This means stop_recording never touches replay,
    // and replay always has its own audio pipe (cliplite_sysaudio_replay).
    let recorder_exe = find_recorder()
        .ok_or_else(|| "cliplite-recorder.exe not found".to_string())?;

    #[cfg(windows)]
    {
        // Kill any existing replay child before starting a new one
        {
            let old = recorder.0.lock().unwrap().replay_child.take();
            if let Some(old_child) = old {
                old_child.send_line(&ipc_line(&cliplite_core::ipc::Command::StopReplay)).ok();
                old_child.send_line(&ipc_line(&cliplite_core::ipc::Command::Stop)).ok();
                std::thread::sleep(std::time::Duration::from_millis(150));
                // old_child drops here, closing all Win32 handles
            }
        }

        let cmd = cliplite_core::ipc::Command::StartReplay {
            args: to_start_args(&ffmpeg_path, "", &region, &audio_cfg, &enc_cfg,
                                Some(REPLAY_AUDIO_PIPE), settings.max_replay_buffer_size_mb),
        };
        let line = ipc_line(&cmd);

        let child = spawn_recorder_isolated(&recorder_exe)
            .map_err(|e| format!("Failed to spawn replay recorder: {e}"))?;
        child.send_line(&line).map_err(|e| format!("IPC send failed: {e}"))?;

        // Read the started/error confirmation from the subprocess
        loop {
            let resp = match child.read_line() {
                Some(r) => r,
                None => return Err("Replay recorder closed unexpectedly".into()),
            };
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                if v["event"] == "replay_started" {
                    let mut rec = recorder.0.lock().unwrap();
                    rec.replay_child = Some(std::sync::Arc::new(child));
                    return Ok("replay_started".into());
                } else if v["event"] == "error" {
                    return Err(v["message"].as_str().unwrap_or("replay start failed").to_string());
                }
            }
        }
    }

    #[cfg(not(windows))]
    Err("Replay buffer is only supported on Windows".into())
}

#[tauri::command]
pub async fn stop_replay(recorder: State<'_, RecorderState>) -> Result<(), String> {
    #[cfg(windows)]
    {
        let child = recorder.0.lock().unwrap().replay_child.take();
        if let Some(child) = child {
            child.send_line(&ipc_line(&cliplite_core::ipc::Command::StopReplay)).ok();
            child.send_line(&ipc_line(&cliplite_core::ipc::Command::Stop)).ok();
            // Brief pause so the subprocess can flush StopReplay event and clean temp file.
            // Drop the Arc *after* the sleep so handles stay open during the flush.
            std::thread::sleep(std::time::Duration::from_millis(200));
            // child Arc drops here — Drop closes all Win32 handles including stdin (sends EOF)
            return Ok(());
        }
        return Err("Replay buffer not running".into());
    }

    #[cfg(not(windows))]
    Err("Replay buffer is only supported on Windows".into())
}

#[tauri::command]
pub async fn save_replay(
    app:            AppHandle,
    secs:           u32,
    recorder:       State<'_, RecorderState>,
    settings_state: State<'_, SettingsState>,
    encoder_cache:  State<'_, EncoderCache>,
) -> Result<String, String> {
    let settings   = settings_state.0.lock().unwrap().clone();
    let output_str = settings.build_replay_output_path(".mp4").to_string_lossy().to_string();

    let cmd = cliplite_core::ipc::Command::SaveReplay { secs, output_path: output_str.clone() };
    let line = ipc_line(&cmd);

    #[cfg(windows)]
    {
        // ── Phase 1: send the command and read the response ──────────────────
        // We MUST release the mutex lock before calling read_line() — the blocking
        // read would hold the lock for the entire extraction duration (10-60+ seconds
        // for long buffers), freezing any concurrent status polls or stop calls.
        // Instead, Arc-clone the child so we can use it without holding the lock.
        use std::sync::Arc;
        let child_arc: Arc<crate::recorder::RecorderChild> = {
            let rec = recorder.0.lock().unwrap();
            match rec.replay_child.as_ref() {
                Some(c) => Arc::clone(c),
                None    => return Err("Replay buffer not active — start it first".into()),
            }
        }; // <-- lock released here

        child_arc.send_line(&line).map_err(|e| e.to_string())?;

        // Drain responses until we see replay_saved or error.
        // The lock is NOT held here — other commands can run concurrently.
        let saved_path = loop {
            let resp = match child_arc.read_line() {
                Some(r) => r,
                None    => return Err("Recorder process closed pipe unexpectedly".into()),
            };
            let v = match serde_json::from_str::<serde_json::Value>(&resp) {
                Ok(v)  => v,
                Err(_) => continue,
            };
            if v["event"] == "replay_saved" {
                break v["path"].as_str().unwrap_or("").to_string();
            } else if v["event"] == "error" {
                return Err(v["message"].as_str().unwrap_or("save failed").to_string());
            }
            // status or other events — keep waiting
        };

        // ── Phase 2: replace the old child with a freshly started buffer ─────
        // The subprocess cleared its own state after saving; we spawn a new
        // subprocess so the user can save again without manually restarting.
        let recorder_exe = find_recorder()
            .ok_or_else(|| "cliplite-recorder.exe not found".to_string())?;
        let ffmpeg_path = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
        let cached_encoder = encoder_cache.inner().0.lock().unwrap().clone();
        let hw_encoder = if cached_encoder.is_empty() { "h264_nvenc".into() } else { cached_encoder };

        // Retrieve the original start args stored on the old child for replay config
        // (region, audio, enc). We stored them in settings already.
        // Build the start_replay JSON using those settings fields.
        // NOTE: hardcoded 1920x1080 region and dropped mic name are pre-existing
        // behaviour, kept for the baseline; WS2 removes this restart path.
        let restart_args = to_start_args(
            &ffmpeg_path, "",
            &Region { x: 0, y: 0, width: 1920, height: 1080, monitor: settings.selected_monitor },
            &AudioConfig { mic_device: None, sys_device_id: settings.sys_audio_device_id.clone(),
                           mic_device_id: settings.mic_device_id.clone() },
            &EncConfig { fps: settings.default_fps, quality_crf: Some(settings.default_quality_crf),
                         format: Some(settings.default_format.clone()), hw_encoder: Some(hw_encoder.clone()) },
            Some(REPLAY_AUDIO_PIPE), settings.max_replay_buffer_size_mb,
        );
        let restart_line = ipc_line(&cliplite_core::ipc::Command::StartReplay { args: restart_args });

        match spawn_recorder_isolated(&recorder_exe) {
            Ok(new_child) => {
                if new_child.send_line(&restart_line).is_ok() {
                    // Wait for replay_started confirmation (with timeout via try_wait loop)
                    let started = loop {
                        match new_child.read_line() {
                            None => break false,
                            Some(resp) => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                                    if v["event"] == "replay_started" { break true; }
                                    if v["event"] == "error"          { break false; }
                                }
                            }
                        }
                    };
                    let mut rec = recorder.0.lock().unwrap();
                    if started {
                        // Replace old child with new one — old child drops here, closing handles
                        rec.replay_child = Some(Arc::new(new_child));
                    } else {
                        rec.replay_child = None;
                    }
                } else {
                    recorder.0.lock().unwrap().replay_child = None;
                }
            }
            Err(e) => {
                log::warn!("[save_replay] Failed to restart replay buffer: {e}");
                recorder.0.lock().unwrap().replay_child = None;
            }
        }

        // ── Phase 3: notify the UI ────────────────────────────────────────────
        let filename = std::path::Path::new(&saved_path)
            .file_name().unwrap_or_default()
            .to_string_lossy().to_string();
        app.emit("replay-saved", &saved_path).ok();
        if let Some(tray) = app.tray_by_id("main-tray") {
            let _ = tray.set_tooltip(Some(&format!("Replay saved: {filename}")));
        }
        use tauri_plugin_notification::NotificationExt;
        let _ = app.notification()
            .builder()
            .title("Replay saved")
            .body(&filename)
            .show();
        return Ok(saved_path);
    }

    #[cfg(not(windows))]
    Err("Replay buffer is only supported on Windows".into())
}

// ── Audio ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
pub fn list_system_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_output_devices()
}

// ── Monitors ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_monitors() -> Vec<MonitorInfo> {
    #[cfg(windows)] return enumerate_monitors_windows();
    #[cfg(not(windows))] return vec![MonitorInfo {
        index:0, name:"Display 1".into(), x:0, y:0, width:1920, height:1080,
        is_primary:true, refresh_rate:60.0,
    }];
}

#[cfg(windows)]
fn enumerate_monitors_windows() -> Vec<MonitorInfo> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MONITORINFOEXW, HDC, HMONITOR};
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};

    struct Context { monitors: Vec<MonitorInfo> }

    unsafe extern "system" fn callback(hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        let ctx = &mut *(data.0 as *mut Context);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            return BOOL(1);
        }
        let r = info.monitorInfo.rcMonitor;
        let is_primary = (info.monitorInfo.dwFlags & 1) != 0;
        let name_end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..name_end]);

        let mut dm = windows::Win32::Graphics::Gdi::DEVMODEW::default();
        dm.dmSize = std::mem::size_of::<windows::Win32::Graphics::Gdi::DEVMODEW>() as u16;
        let dev_name: Vec<u16> = info.szDevice.iter().cloned().collect();
        let refresh = if windows::Win32::Graphics::Gdi::EnumDisplaySettingsW(
            windows::core::PCWSTR(dev_name.as_ptr()),
            windows::Win32::Graphics::Gdi::ENUM_CURRENT_SETTINGS,
            &mut dm,
        ).as_bool() { dm.dmDisplayFrequency as f64 } else { 60.0 };

        ctx.monitors.push(MonitorInfo {
            index: ctx.monitors.len() as u32, name: name.trim_matches('\0').into(),
            x: r.left, y: r.top, width: (r.right-r.left) as u32, height: (r.bottom-r.top) as u32,
            is_primary, refresh_rate: refresh,
        });
        BOOL(1)
    }

    let mut ctx = Context { monitors: vec![] };
    unsafe { let _ = EnumDisplayMonitors(HDC::default(), None, Some(callback), LPARAM(&mut ctx as *mut _ as isize)); }
    ctx.monitors
}

// ── Library ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_clips(search: String) -> Result<Vec<library::Clip>, String> {
    library::get_all_clips(&search).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_clip(clip_id: i64, delete_file: bool) -> Result<(), String> {
    let (filepath, thumbnail) = library::delete_clip_from_db(clip_id).map_err(|e| e.to_string())?;
    if delete_file {
        for p in [&filepath, &thumbnail] {
            if !p.is_empty() && std::path::Path::new(p).exists() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn update_clip_tags(clip_id: i64, tags: String) -> Result<(), String> {
    library::update_tags_in_db(clip_id, &tags).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_clip(filepath: String) -> Result<library::Clip, String> {
    let meta = probe_file(&filepath).map_err(|e| e.to_string())?;
    let mut clip = library::add_clip_to_db(&filepath, &meta).map_err(|e| e.to_string())?;
    if let Ok(ffmpeg) = ffmpeg::find_ffmpeg() {
        if let Ok(thumb) = generate_thumb(&ffmpeg, &filepath, clip.id) {
            let _ = library::update_thumbnail(clip.id, &thumb);
            clip.thumbnail = thumb;
        }
    }
    Ok(clip)
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_settings(state: State<'_, SettingsState>) -> Settings {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(settings: Settings, state: State<'_, SettingsState>) -> Result<(), String> {
    settings.save().map_err(|e| e.to_string())?;
    *state.0.lock().unwrap() = settings;
    Ok(())
}

/// Re-register global hotkeys from the current settings.
/// Called explicitly after the user confirms changes in SettingsView —
/// NOT on every incremental save (which fires for every slider/dropdown change).
#[tauri::command]
pub fn apply_hotkeys(app: AppHandle, state: State<'_, SettingsState>) -> Result<(), String> {
    let hotkeys = state.0.lock().unwrap().hotkeys.clone();
    crate::reregister_hotkeys(&app, &hotkeys);
    Ok(())
}

// ── Utility ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    #[cfg(windows)] Command::new("explorer").arg(&path).spawn().ok();
    #[cfg(target_os="macos")] Command::new("open").arg(&path).spawn().ok();
    #[cfg(target_os="linux")] Command::new("xdg-open").arg(&path).spawn().ok();
    Ok(())
}

#[tauri::command]
pub fn trim_clip(input_path: String, output_path: String, start_time: f64, end_time: f64) -> Result<(), String> {
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    let ok = Command::new(&ffmpeg)
        .args(["-y", "-ss", &fmt_time(start_time), "-to", &fmt_time(end_time),
               "-i", &input_path, "-c", "copy", &output_path])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    if ok { Ok(()) } else { Err("FFmpeg trim failed".into()) }
}

#[tauri::command]
pub fn generate_thumbnail(filepath: String, clip_id: i64) -> Result<String, String> {
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    generate_thumb(&ffmpeg, &filepath, clip_id).map_err(|e| e.to_string())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn probe_file(filepath: &str) -> anyhow::Result<ClipMeta> {
    let ffprobe = ffmpeg::find_ffprobe()?;
    let out = Command::new(ffprobe)
        .args(["-v","quiet","-print_format","json","-show_streams","-show_format",filepath])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .output()?;
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let fmt = &data["format"];
    let video = data["streams"].as_array()
        .and_then(|s| s.iter().find(|s| s["codec_type"] == "video"));
    let (w, h, fps) = video.map(|v| (
        v["width"].as_i64().unwrap_or(0),
        v["height"].as_i64().unwrap_or(0),
        parse_fps(v["r_frame_rate"].as_str().unwrap_or("30/1")),
    )).unwrap_or((0, 0, 30.0));
    Ok(ClipMeta {
        duration_s: fmt["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0),
        width: w, height: h, fps,
        filesize_b: fmt["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        format: fmt["format_name"].as_str().unwrap_or("").into(),
    })
}

fn parse_fps(s: &str) -> f64 {
    s.split_once('/').map(|(n, d)| {
        let n: f64 = n.parse().unwrap_or(30.0);
        let d: f64 = d.parse().unwrap_or(1.0);
        if d != 0.0 { n / d } else { 30.0 }
    }).unwrap_or_else(|| s.parse().unwrap_or(30.0))
}

fn generate_thumb(ffmpeg: &PathBuf, filepath: &str, clip_id: i64) -> anyhow::Result<String> {
    let dir = crate::settings::app_dir().join("thumbnails");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("{clip_id}.jpg"));
    Command::new(ffmpeg)
        .args(["-y","-ss","0","-i",filepath,"-frames:v","1","-vf","scale=320:-1",
               out.to_str().unwrap_or("")])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .output()?;
    Ok(out.to_string_lossy().into())
}

fn fmt_time(s: f64) -> String {
    let h = (s / 3600.0) as u64;
    let m = ((s % 3600.0) / 60.0) as u64;
    let sec = s % 60.0;
    format!("{h:02}:{m:02}:{sec:06.3}")
}

// ── Debug commands ────────────────────────────────────────────────────────────

/// Expose full internal state for debugging.
#[tauri::command]
pub fn get_debug_state(
    hotkey_handle: State<'_, crate::HotkeyListenerHandle>,
    settings: State<'_, SettingsState>,
    recorder: State<'_, RecorderState>,
) -> serde_json::Value {
    let hotkeys = settings.0.lock().unwrap().hotkeys.clone();
    let is_recording = recorder.0.lock().unwrap().status().is_recording;
    let listener_alive = hotkey_handle.0.lock().unwrap().is_some();
    
    serde_json::json!({
        "hotkeys": hotkeys,
        "listener_alive": listener_alive,
        "is_recording": is_recording,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

/// Simulate a hotkey event directly (useful for testing).
/// Bypasses the OS hotkey system entirely, goes straight to the event emitter.
#[tauri::command]
pub fn simulate_hotkey(app: AppHandle, action: String) -> Result<(), String> {
    let event = format!("hotkey-{action}");
    log::info!("[debug] Simulating hotkey event: {event}");
    // Target the main window directly — same path as the real hotkey listener.
    if let Some(win) = app.get_webview_window("main") {
        win.emit(&event, ()).map_err(|e| e.to_string())
    } else {
        app.emit(&event, ()).map_err(|e| e.to_string())
    }
}


#[cfg(test)]
#[path = "commands_ipc_tests.rs"]
mod ipc_tests;
