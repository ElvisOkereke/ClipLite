//! Foreground watcher: elevated-foreground notice and `RegisterHotKey` policy.
//!
//! Windows blocks a normal-integrity process from seeing input that goes to an
//! elevated window (UIPI), so ClipLite's hooks can go deaf while an
//! administrator-level game is focused. A small poller thread (every 500 ms)
//! watches the foreground window and
//! - emits `elevated-foreground {exe}` once per exe per session so the UI can
//!   say so;
//! - tells the current hotkey listener whether `RegisterHotKey` registrations
//!   should be held ([`should_register`]): only while ClipLite's own window or
//!   a definitely elevated app is focused, because registrations take the
//!   chord away from every other app.
//!
//! It shares no state with the hotkey dispatcher and survives listener
//! restarts (a new listener starts unregistered and gets the current verdict
//! on the next poll).

use std::collections::HashSet;

/// Fires at most once per exe per session, only while the foreground app is
/// elevated and ClipLite is not.
#[derive(Default)]
pub struct ElevationNotice {
    seen: HashSet<String>,
}

impl ElevationNotice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the exe name to announce, or `None` (not applicable / already
    /// announced). Exe names compare case-insensitively.
    pub fn observe(&mut self, fg_elevated: bool, self_elevated: bool, exe: &str) -> Option<String> {
        if !fg_elevated || self_elevated {
            return None;
        }
        let exe = exe.trim();
        let exe = if exe.is_empty() { "An elevated app" } else { exe };
        if self.seen.insert(exe.to_lowercase()) {
            Some(exe.to_string())
        } else {
            None
        }
    }
}

/// Whether a process runs with an elevated token. `Unknown` never raises a banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    Elevated,
    NotElevated,
    Unknown,
}

/// Why a token probe failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    AccessDenied,
    Other,
}

/// Pure decision rule. `open_ok`: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
/// succeeded. `token`: the token probe, `Ok(TokenIsElevated)` or why it failed.
/// Only a successful token read, or an access-denied token open on a process we
/// *could* open, counts as a verdict; everything else (protected, SYSTEM,
/// other-user, exited processes) is `Unknown`.
pub fn classify(open_ok: bool, token: Result<bool, ErrKind>) -> Elevation {
    if !open_ok {
        return Elevation::Unknown;
    }
    match token {
        Ok(true) => Elevation::Elevated,
        Ok(false) => Elevation::NotElevated,
        Err(ErrKind::AccessDenied) => Elevation::Elevated,
        Err(ErrKind::Other) => Elevation::Unknown,
    }
}

/// What the foreground watcher knows about the focused window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Foreground {
    pub own: bool,
    pub elevation: Elevation,
}

/// Whether `RegisterHotKey` registrations should exist right now. They are
/// exclusive system-wide (other apps stop seeing the chord), so they are held
/// only where the hook is known to be at risk: while ClipLite's own window is
/// focused, or while a definitely elevated app is (UIPI). `Unknown` is false.
pub fn should_register(fg: Foreground) -> bool {
    fg.own || fg.elevation == Elevation::Elevated
}

/// The foreground app's elevation as it matters to ClipLite's hooks: an
/// elevated ClipLite is not blocked by UIPI (so nothing counts as elevated),
/// and if ClipLite's own token is unreadable nothing is known.
pub fn foreground_elevation(fg: Elevation, self_elevation: Elevation) -> Elevation {
    match self_elevation {
        Elevation::NotElevated => fg,
        Elevation::Elevated if fg == Elevation::Elevated => Elevation::NotElevated,
        Elevation::Elevated => fg,
        Elevation::Unknown => Elevation::Unknown,
    }
}

#[cfg(windows)]
pub fn spawn_poller(app: tauri::AppHandle) {
    use std::time::Duration;
    use tauri::{Emitter, Manager};

    let spawned = std::thread::Builder::new()
        .name("foreground-watch".into())
        .spawn(move || {
            let self_elevation = win::self_elevation();
            match self_elevation {
                Elevation::NotElevated => {}
                // An elevated ClipLite is not blocked by UIPI; registrations
                // are still held while its own window is focused.
                Elevation::Elevated => log::info!("ClipLite is elevated; elevation notices off"),
                Elevation::Unknown => log::warn!("could not read ClipLite's own token; elevation notices disabled"),
            }
            let mut notice = ElevationNotice::new();
            // Last foreground window: its process is probed only when it changes.
            let mut last: Option<((isize, u32), win::FgProcess)> = None;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let fg = match win::foreground_window() {
                    None => None,
                    Some(key) if last.as_ref().is_some_and(|(k, _)| *k == key) => {
                        last.as_ref().map(|(_, p)| p.clone())
                    }
                    Some(key @ (_, pid)) => {
                        let p = win::probe_process(pid);
                        if !p.own && p.elevation == Elevation::Unknown {
                            log::debug!("foreground elevation unknown ({}); no notice", p.exe);
                        }
                        last = Some((key, p.clone()));
                        Some(p)
                    }
                };
                let need = fg.as_ref().is_some_and(|p| {
                    should_register(Foreground { own: p.own, elevation: foreground_elevation(p.elevation, self_elevation) })
                });
                if let Some(l) = app.state::<crate::HotkeyListenerHandle>().0.lock().unwrap().as_ref() {
                    l.set_register_wanted(need);
                }
                let Some(p) = fg.filter(|p| !p.own && p.elevation != Elevation::Unknown) else { continue };
                let self_elevated = self_elevation != Elevation::NotElevated;
                if let Some(exe) = notice.observe(p.elevation == Elevation::Elevated, self_elevated, &p.exe) {
                    log::info!("elevated foreground app: {exe}");
                    app.emit("elevated-foreground", serde_json::json!({ "exe": exe })).ok();
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("foreground watcher not started: {e}");
    }
}

#[cfg(not(windows))]
pub fn spawn_poller(_app: tauri::AppHandle) {}

#[cfg(windows)]
mod win {
    use super::{classify, Elevation, ErrKind};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    #[derive(Clone)]
    pub struct FgProcess {
        /// ClipLite's own process (its top-level window; WebView2 renders in
        /// child processes but the window belongs to us).
        pub own: bool,
        pub elevation: Elevation,
        pub exe: String,
    }

    /// Reads `TokenIsElevated`; a failure is an error, never "not elevated".
    unsafe fn token_elevated(token: HANDLE) -> Result<bool, ErrKind> {
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .map_err(|_| ErrKind::Other)?;
        Ok(elev.TokenIsElevated != 0)
    }

    /// Open `proc`'s token and read elevation, mapping errors for `classify`.
    unsafe fn probe(proc: HANDLE) -> Result<bool, ErrKind> {
        let mut token = HANDLE::default();
        match OpenProcessToken(proc, TOKEN_QUERY, &mut token) {
            Ok(()) => {
                let r = token_elevated(token);
                let _ = CloseHandle(token);
                r
            }
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(ErrKind::AccessDenied),
            Err(_) => Err(ErrKind::Other),
        }
    }

    pub fn self_elevation() -> Elevation {
        unsafe { classify(true, probe(GetCurrentProcess())) }
    }

    /// The foreground window (as an integer, for change detection) and its
    /// process id. `None` when there is no foreground window.
    pub fn foreground_window() -> Option<(isize, u32)> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            (pid != 0).then_some((hwnd.0 as isize, pid))
        }
    }

    /// Elevation and exe name of `pid`. ClipLite itself is not probed.
    pub fn probe_process(pid: u32) -> FgProcess {
        if pid == std::process::id() {
            return FgProcess { own: true, elevation: Elevation::Unknown, exe: String::new() };
        }
        unsafe {
            let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return FgProcess {
                    own: false,
                    elevation: classify(false, Err(ErrKind::Other)),
                    exe: format!("pid {pid}"),
                };
            };
            let mut exe = String::new();
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                exe = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
            }
            let elevation = classify(true, probe(proc));
            let _ = CloseHandle(proc);
            FgProcess { own: false, elevation, exe }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_per_exe() {
        let mut n = ElevationNotice::new();
        assert_eq!(n.observe(true, false, "game.exe"), Some("game.exe".into()));
        assert_eq!(n.observe(true, false, "game.exe"), None);
        assert_eq!(n.observe(true, false, "GAME.EXE"), None);
    }

    #[test]
    fn different_exe_fires_again() {
        let mut n = ElevationNotice::new();
        assert!(n.observe(true, false, "a.exe").is_some());
        assert!(n.observe(true, false, "b.exe").is_some());
    }

    #[test]
    fn silent_when_foreground_not_elevated() {
        let mut n = ElevationNotice::new();
        assert_eq!(n.observe(false, false, "a.exe"), None);
        // and a non-elevated sighting doesn't consume the one-shot
        assert!(n.observe(true, false, "a.exe").is_some());
    }

    #[test]
    fn silent_when_self_elevated() {
        let mut n = ElevationNotice::new();
        assert_eq!(n.observe(true, true, "a.exe"), None);
        assert!(n.observe(true, false, "a.exe").is_some());
    }

    #[test]
    fn unknown_exe_gets_generic_name_once() {
        let mut n = ElevationNotice::new();
        assert_eq!(n.observe(true, false, ""), Some("An elevated app".into()));
        assert_eq!(n.observe(true, false, "  "), None);
    }

    #[test]
    fn classify_open_failed_is_unknown() {
        assert_eq!(classify(false, Ok(true)), Elevation::Unknown);
        assert_eq!(classify(false, Err(ErrKind::AccessDenied)), Elevation::Unknown);
    }

    #[test]
    fn classify_token_access_denied_is_elevated() {
        assert_eq!(classify(true, Err(ErrKind::AccessDenied)), Elevation::Elevated);
    }

    #[test]
    fn classify_other_token_error_is_unknown() {
        assert_eq!(classify(true, Err(ErrKind::Other)), Elevation::Unknown);
    }

    #[test]
    fn classify_token_value() {
        assert_eq!(classify(true, Ok(true)), Elevation::Elevated);
        assert_eq!(classify(true, Ok(false)), Elevation::NotElevated);
    }

    #[test]
    fn register_while_own_window_focused() {
        for elevation in [Elevation::Elevated, Elevation::NotElevated, Elevation::Unknown] {
            assert!(should_register(Foreground { own: true, elevation }), "{elevation:?}");
        }
    }

    #[test]
    fn register_only_for_definitely_elevated_foreground() {
        assert!(should_register(Foreground { own: false, elevation: Elevation::Elevated }));
        assert!(!should_register(Foreground { own: false, elevation: Elevation::NotElevated }));
        assert!(!should_register(Foreground { own: false, elevation: Elevation::Unknown }));
    }

    #[test]
    fn elevated_cliplite_sees_elevated_apps_without_registering() {
        // An elevated (or unreadable) ClipLite never counts a foreground app as elevated.
        assert_eq!(foreground_elevation(Elevation::Elevated, Elevation::NotElevated), Elevation::Elevated);
        assert_eq!(foreground_elevation(Elevation::Elevated, Elevation::Elevated), Elevation::NotElevated);
        assert_eq!(foreground_elevation(Elevation::Elevated, Elevation::Unknown), Elevation::Unknown);
        assert_eq!(foreground_elevation(Elevation::NotElevated, Elevation::NotElevated), Elevation::NotElevated);
    }
}
