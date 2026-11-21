/// Tauri command handlers — pure Rust, no Python, no external scripts.
use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::Arc;
use std::time::Duration;
use cliplite_core::ipc::{Command, Event};
use cliplite_core::telemetry::{new_cid, Stage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    audio, capture_config, ffmpeg,
    library::{self, ClipMeta},
    recorder::{EngineClient, RecorderInner, RecorderState, RecordingStatus},
    settings::{Settings, SettingsState, SETTINGS_VERSION},
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

/// Log `command.received` for a Tauri command and return its correlation id
/// (the caller's, e.g. from a hotkey press, or a fresh one).
pub(crate) fn received(name: &str, cid: Option<String>) -> String {
    let cid = cid.filter(|c| !c.is_empty()).unwrap_or_else(new_cid);
    tracing::info!(stage = Stage::CommandReceived.as_str(), cid = %cid, name, "command received");
    cid
}

/// Delete `cliplite_replay_*.mkv` files the pre-WS2 replay buffer left in
/// `dir` (`app_dir()/temp`). Folders (e.g. the engine's `engine/`) are kept.
pub fn remove_legacy_replay_files(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("cliplite_replay_") && name.ends_with(".mkv")
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Fill audio devices missing from Settings with the UI's current selection
/// (e.g. the auto-selected first mic, which the UI does not persist), so the
/// Settings-built engine args capture what the UI shows. True if changed.
fn adopt_ui_audio(s: &mut Settings, ui: &AudioConfig) -> bool {
    let mut changed = false;
    if s.sys_audio_device_id.is_none() && ui.sys_device_id.is_some() {
        s.sys_audio_device_id = ui.sys_device_id.clone();
        changed = true;
    }
    if s.mic_device_id.is_none() && ui.mic_device_id.is_some() {
        s.mic_device_id = ui.mic_device_id.clone();
        s.mic_device = ui.mic_device.clone();
        changed = true;
    }
    changed
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

// ── Find recorder binary ──────────────────────────────────────────────────────

fn find_recorder() -> Result<PathBuf, String> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();

    #[cfg(windows)] let name = "cliplite-recorder.exe";
    #[cfg(not(windows))] let name = "cliplite-recorder";

    let p = dir.join(name);
    if p.exists() {
        Ok(p)
    } else {
        Err(format!(
            "{name} not found next to cliplite.exe.\nBoth files must be in the same directory.\nExpected: {}",
            p.display()
        ))
    }
}

// ── Capture engine plumbing ───────────────────────────────────────────────────
//
// One recorder process runs one capture engine; the replay buffer and
// recordings are both views of it. Requests run on the blocking pool and the
// RecorderState lock is never held across one.

const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const START_ENGINE_TIMEOUT: Duration = Duration::from_secs(20);
/// Stopping a recording remuxes all of it (stream copy).
const STOP_RECORDING_TIMEOUT: Duration = Duration::from_secs(300);
/// Saving remuxes the snapshot; on top of the clip length.
const SAVE_EXTRA: Duration = Duration::from_secs(30);

const BUFFER_INACTIVE: &str = "Replay buffer not active — start it first";

/// The recorder died: forget its state (the next command respawns it).
fn reset_after_exit(app: &AppHandle, rec: &mut RecorderInner) {
    rec.client = None;
    rec.buffer_on = false;
    rec.recording_path = None;
    rec.last_status = RecordingStatus::default();
    tray::set_recording_state(app, false);
    tray::set_replay_state(app, None);
}

/// The running recorder, if any (never spawns).
fn live_client(app: &AppHandle) -> Option<Arc<EngineClient>> {
    let state = app.state::<RecorderState>();
    let mut rec = state.0.lock().unwrap();
    let client = rec.client.clone()?;
    if client.is_alive() {
        return Some(client);
    }
    log::warn!("[recorder] pid={} exited unexpectedly", client.pid());
    reset_after_exit(app, &mut rec);
    None
}

/// The running recorder, spawning (or respawning) the process if needed.
fn ensure_client(app: &AppHandle) -> Result<Arc<EngineClient>, String> {
    let state = app.state::<RecorderState>();
    let mut rec = state.0.lock().unwrap();
    if let Some(c) = &rec.client {
        if c.is_alive() {
            return Ok(c.clone());
        }
        log::warn!("[recorder] pid={} exited unexpectedly; respawning", c.pid());
        reset_after_exit(app, &mut rec);
    }
    let exe = find_recorder()?;
    let handle = app.clone();
    let client = EngineClient::spawn(&exe, move |ev| on_unsolicited(&handle, ev))
        .map_err(|e| format!("Failed to spawn recorder: {e:#}"))?;
    let client = Arc::new(client);
    rec.client = Some(client.clone());
    Ok(client)
}

/// Events no request is waiting for.
fn on_unsolicited(app: &AppHandle, ev: Event) {
    match ev {
        Event::EngineRestarted { reason, restarts } => {
            tracing::warn!(%reason, restarts, "capture engine restarted");
            app.emit("engine-restarted", serde_json::json!({ "reason": reason, "restarts": restarts })).ok();
        }
        // StopEngine finalized a live recording.
        Event::RecordingStopped { path } => {
            log::info!("[recorder] recording finalized by engine stop: {path}");
            recording_finished(app, Some(&path));
        }
        // A save that outlived its request's timeout.
        Event::ReplaySaved { path, .. } => replay_saved(app, &path),
        Event::Error { message } => log::warn!("[recorder] unsolicited error: {message}"),
        other => log::debug!("[recorder] unsolicited event ignored: {other:?}"),
    }
}

/// Run one request on the blocking pool.
async fn request(client: &Arc<EngineClient>, cmd: Command, cid: &str, timeout: Duration) -> Result<Event, String> {
    let (client, cid) = (client.clone(), cid.to_string());
    tauri::async_runtime::spawn_blocking(move || client.request(cmd, &cid, timeout))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// A request whose `Error` response becomes `Err(message)`.
async fn request_ok(client: &Arc<EngineClient>, cmd: Command, cid: &str, timeout: Duration) -> Result<Event, String> {
    match request(client, cmd, cid, timeout).await? {
        Event::Error { message } => Err(message),
        ev => Ok(ev),
    }
}

fn settings_of(app: &AppHandle) -> Settings {
    app.state::<SettingsState>().0.lock().unwrap().clone()
}

/// `StartEngine` with args and ring limits from Settings (Rust owns capture config).
fn engine_start_command(app: &AppHandle) -> Result<Command, String> {
    let settings = settings_of(app);
    let cached = app.state::<EncoderCache>().0.lock().unwrap().clone();
    let encoder = capture_config::resolve_encoder(&cached, &settings.hw_encoder);
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    let source = capture_config::capture_source(std::env::var("CLIPLITE_CAPTURE_SOURCE").ok().as_deref());
    let args = capture_config::engine_args(&settings, &get_monitors(), &ffmpeg, &encoder, source);
    let (max_secs, max_mb) = capture_config::ring_limits(&settings);
    log::info!("[recorder] engine: source={source:?} encoder={encoder} monitor={} ring={max_secs}s/{max_mb}MB",
               args.region.monitor);
    Ok(Command::StartEngine { args, max_secs, max_mb })
}

/// Start the engine if it isn't running; if it is and `refresh_limits`, apply
/// the current ring limits instead.
async fn ensure_engine(app: &AppHandle, client: &Arc<EngineClient>, cid: &str, refresh_limits: bool) -> Result<(), String> {
    let running = matches!(request_ok(client, Command::Status, cid, STATUS_TIMEOUT).await?,
                           Event::Status { engine_running: true, .. });
    if running {
        if refresh_limits {
            let (max_secs, max_mb) = capture_config::ring_limits(&settings_of(app));
            request_ok(client, Command::SetLimits { max_secs, max_mb }, cid, CONTROL_TIMEOUT).await?;
        }
        return Ok(());
    }
    match request_ok(client, engine_start_command(app)?, cid, START_ENGINE_TIMEOUT).await {
        Ok(Event::EngineStarted) => Ok(()),
        // A concurrent command (e.g. hotkey + click) started it first.
        Err(e) if e == "engine already running" => Ok(()),
        Ok(other) => Err(format!("unexpected reply to start_engine: {other:?}")),
        Err(e) => Err(e),
    }
}

/// Stop the engine when neither the buffer nor a recording needs it.
async fn stop_engine_if_idle(app: &AppHandle, client: &Arc<EngineClient>, cid: &str) {
    let idle = {
        let state = app.state::<RecorderState>();
        let rec = state.0.lock().unwrap();
        !rec.buffer_on && rec.recording_path.is_none()
    };
    if idle {
        match request_ok(client, Command::StopEngine, cid, CONTROL_TIMEOUT).await {
            Ok(_) => {}
            Err(e) if e == "engine not running" => {}
            Err(e) => log::warn!("[recorder] stop idle engine: {e}"),
        }
    }
}

/// Turn the replay buffer on (engine started from Settings, or its limits
/// refreshed). Shared by `start_replay` and the launch path (incl. test mode).
pub(crate) async fn start_buffer(app: &AppHandle, cid: &str) -> Result<u32, String> {
    let secs = settings_of(app).replay_buffer_duration_secs;
    if secs == 0 {
        return Err("Replay buffer duration is Off in Settings".into());
    }
    let client = ensure_client(app)?;
    ensure_engine(app, &client, cid, true).await?;
    app.state::<RecorderState>().0.lock().unwrap().buffer_on = true;
    tray::set_replay_state(app, Some(secs));
    Ok(secs)
}

/// At launch, after encoder detection: clear legacy buffer files and start
/// the buffer when Settings has it on (this is also how test mode starts it).
pub fn start_buffer_at_launch(app: &AppHandle) {
    let n = remove_legacy_replay_files(&crate::settings::app_dir().join("temp"));
    if n > 0 {
        log::info!("[launch] removed {n} legacy replay buffer file(s)");
    }
    let prefix = if crate::test_mode::active() { "[test-mode] " } else { "" };
    if settings_of(app).replay_buffer_duration_secs == 0 {
        log::info!("{prefix}replay buffer off in Settings; not starting the engine");
        tray::set_replay_state(app, None);
        return;
    }
    let cid = received("start_buffer_at_launch", None);
    match tauri::async_runtime::block_on(start_buffer(app, &cid)) {
        Ok(secs) => log::info!("{prefix}replay buffer on at launch ({secs} s)"),
        Err(e) => {
            log::error!("{prefix}replay buffer failed to start at launch: {e}");
            tray::set_replay_state(app, None);
        }
    }
}

/// Apply changed ring limits to a running buffer (Settings saved).
fn apply_ring_limits(app: &AppHandle, settings: &Settings) {
    let secs = settings.replay_buffer_duration_secs;
    let buffer_on = app.state::<RecorderState>().0.lock().unwrap().buffer_on;
    if !buffer_on || secs == 0 {
        return;
    }
    let Some(client) = live_client(app) else { return };
    let (max_secs, max_mb) = capture_config::ring_limits(settings);
    let app = app.clone();
    std::thread::spawn(move || {
        match client.request(Command::SetLimits { max_secs, max_mb }, &new_cid(), CONTROL_TIMEOUT) {
            Ok(Event::Status { .. }) => tray::set_replay_state(&app, Some(secs)),
            other => log::warn!("[recorder] set_limits after settings change: {other:?}"),
        }
    });
}

fn add_clip_in_background(path: &str) {
    if path.is_empty() || !std::path::Path::new(path).exists() {
        return;
    }
    let p = path.to_string();
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

/// The recording ended (stopped, finalized by the engine, or failed).
fn recording_finished(app: &AppHandle, path: Option<&str>) {
    app.state::<RecorderState>().0.lock().unwrap().recording_path = None;
    tray::set_recording_state(app, false);
    if let Some(p) = path {
        add_clip_in_background(p);
    }
}

fn replay_saved(app: &AppHandle, path: &str) {
    add_clip_in_background(path);
    let filename = std::path::Path::new(path)
        .file_name().unwrap_or_default()
        .to_string_lossy().to_string();
    app.emit("replay-saved", path).ok();
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title("Replay saved").body(&filename).show();
}

fn remember_status(app: &AppHandle, ev: &Event) -> Option<RecordingStatus> {
    let state = app.state::<RecorderState>();
    let mut rec = state.0.lock().unwrap();
    let s = RecordingStatus::from_event(ev, rec.buffer_on, rec.recording_path.clone())?;
    rec.last_status = s.clone();
    Some(s)
}

/// Persist UI-selected audio devices that Settings lacks (see `adopt_ui_audio`).
fn adopt_ui_selection(app: &AppHandle, audio: &AudioConfig) {
    let state = app.state::<SettingsState>();
    let mut s = state.0.lock().unwrap();
    if adopt_ui_audio(&mut s, audio) {
        if let Err(e) = s.save() {
            log::warn!("Failed to persist UI audio selection: {e}");
        }
    }
}

// ── Recording ─────────────────────────────────────────────────────────────────
//
// `region`/`enc_cfg` from the UI are accepted for call compatibility but the
// engine is configured from Settings (Rust owns capture config); recordings
// are always MP4.

#[tauri::command]
pub async fn start_recording(
    app:       AppHandle,
    region:    Region,
    audio_cfg: AudioConfig,
    enc_cfg:   EncConfig,
    cid:       Option<String>,
) -> Result<String, String> {
    let cid = received("start_recording", cid);
    let _ = (&region, &enc_cfg);
    adopt_ui_selection(&app, &audio_cfg);
    let output = settings_of(&app).build_output_path(".mp4").to_string_lossy().into_owned();

    let client = ensure_client(&app)?;
    ensure_engine(&app, &client, &cid, false).await?;
    let res = request_ok(&client, Command::StartRecording { output_path: output }, &cid, CONTROL_TIMEOUT).await;
    match res {
        Ok(Event::RecordingStarted { path }) => {
            app.state::<RecorderState>().0.lock().unwrap().recording_path = Some(path.clone());
            tray::set_recording_state(&app, true);
            Ok(path)
        }
        other => {
            stop_engine_if_idle(&app, &client, &cid).await;
            Err(match other {
                Err(e) => e,
                Ok(ev) => format!("unexpected reply to start_recording: {ev:?}"),
            })
        }
    }
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle, cid: Option<String>) -> Result<String, String> {
    let cid = received("stop_recording", cid);
    let Some(client) = live_client(&app) else {
        recording_finished(&app, None);
        return Ok(String::new());
    };
    let out = match request(&client, Command::StopRecording, &cid, STOP_RECORDING_TIMEOUT).await? {
        Event::RecordingStopped { path } => {
            recording_finished(&app, Some(&path));
            Ok(path)
        }
        // e.g. "not recording", or a failed remux ("…; segments kept at <path>")
        Event::Error { message } => {
            recording_finished(&app, None);
            Err(message)
        }
        other => Err(format!("unexpected reply to stop_recording: {other:?}")),
    };
    stop_engine_if_idle(&app, &client, &cid).await;
    out
}

async fn pause_or_resume(app: AppHandle, cmd: Command, cid: String) -> Result<(), String> {
    let client = live_client(&app).ok_or("Not recording")?;
    let ev = request_ok(&client, cmd, &cid, CONTROL_TIMEOUT).await?;
    remember_status(&app, &ev);
    Ok(())
}

#[tauri::command]
pub async fn pause_recording(app: AppHandle, cid: Option<String>) -> Result<(), String> {
    let cid = received("pause_recording", cid);
    pause_or_resume(app, Command::Pause, cid).await
}

#[tauri::command]
pub async fn resume_recording(app: AppHandle, cid: Option<String>) -> Result<(), String> {
    let cid = received("resume_recording", cid);
    pause_or_resume(app, Command::Resume, cid).await
}

/// Engine status; short timeout, falling back to the last known status.
#[tauri::command]
pub async fn get_recording_status(app: AppHandle) -> Result<RecordingStatus, String> {
    let Some(client) = live_client(&app) else { return Ok(RecordingStatus::default()) };
    match request(&client, Command::Status, &new_cid(), STATUS_TIMEOUT).await {
        Ok(ev) => {
            if let Some(s) = remember_status(&app, &ev) {
                return Ok(s);
            }
            log::warn!("[recorder] unexpected reply to status: {ev:?}");
        }
        Err(e) => log::debug!("[recorder] status: {e}; serving last known"),
    }
    Ok(app.state::<RecorderState>().0.lock().unwrap().last_status.clone())
}

// ── Replay buffer ─────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_replay(
    app:       AppHandle,
    region:    Region,
    audio_cfg: AudioConfig,
    enc_cfg:   EncConfig,
    cid:       Option<String>,
) -> Result<String, String> {
    let cid = received("start_replay", cid);
    let _ = (&region, &enc_cfg);
    adopt_ui_selection(&app, &audio_cfg);
    start_buffer(&app, &cid).await?;
    Ok("replay_started".into())
}

/// Stop the buffer. While recording, the engine keeps running for the
/// recording and the buffer is only marked off (saves are refused).
#[tauri::command]
pub async fn stop_replay(app: AppHandle, cid: Option<String>) -> Result<(), String> {
    let cid = received("stop_replay", cid);
    let recording = {
        let state = app.state::<RecorderState>();
        let mut rec = state.0.lock().unwrap();
        rec.buffer_on = false;
        rec.recording_path.is_some()
    };
    tray::set_replay_state(&app, None);
    if recording {
        return Ok(());
    }
    let Some(client) = live_client(&app) else { return Ok(()) };
    match request_ok(&client, Command::StopEngine, &cid, CONTROL_TIMEOUT).await {
        Ok(_) => Ok(()),
        Err(e) if e == "engine not running" => Ok(()),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn save_replay(app: AppHandle, secs: u32, cid: Option<String>) -> Result<String, String> {
    let cid = received("save_replay", cid);
    let buffer_on = app.state::<RecorderState>().0.lock().unwrap().buffer_on;
    if !buffer_on {
        return Err(BUFFER_INACTIVE.into());
    }
    let client = live_client(&app).ok_or(BUFFER_INACTIVE)?;
    let output = settings_of(&app).build_replay_output_path(".mp4").to_string_lossy().into_owned();
    let timeout = Duration::from_secs(u64::from(secs)) + SAVE_EXTRA;
    match request_ok(&client, Command::SaveReplay { secs, output_path: output }, &cid, timeout).await? {
        Event::ReplaySaved { path, .. } => {
            replay_saved(&app, &path);
            Ok(path)
        }
        other => Err(format!("unexpected reply to save_replay: {other:?}")),
    }
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

/// Anything the running app saves is current-version settings; a changed
/// buffer length or memory cap is applied to a running buffer.
#[tauri::command]
pub fn save_settings(app: AppHandle, mut settings: Settings, state: State<'_, SettingsState>) -> Result<(), String> {
    settings.settings_version = SETTINGS_VERSION;
    settings.save().map_err(|e| e.to_string())?;
    let old = std::mem::replace(&mut *state.0.lock().unwrap(), settings.clone());
    if capture_config::ring_limits(&old) != capture_config::ring_limits(&settings) {
        apply_ring_limits(&app, &settings);
    }
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
    #[cfg(windows)] Process::new("explorer").arg(&path).spawn().ok();
    #[cfg(target_os="macos")] Process::new("open").arg(&path).spawn().ok();
    #[cfg(target_os="linux")] Process::new("xdg-open").arg(&path).spawn().ok();
    Ok(())
}

#[tauri::command]
pub fn trim_clip(input_path: String, output_path: String, start_time: f64, end_time: f64) -> Result<(), String> {
    let ffmpeg = ffmpeg::find_ffmpeg().map_err(|e| e.to_string())?;
    let ok = Process::new(&ffmpeg)
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
    let out = Process::new(ffprobe)
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
    Process::new(ffmpeg)
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
    let is_recording = recorder.0.lock().unwrap().recording_path.is_some();
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
