//! Elevated-foreground detection.
//!
//! Windows blocks a normal-integrity process from seeing input that goes to an
//! elevated window (UIPI), so ClipLite's hooks can go deaf while an
//! administrator-level game is focused. A small poller thread watches the
//! foreground window and emits `elevated-foreground {exe}` once per exe per
//! session so the UI can say so. It is deliberately separate from the hotkey
//! dispatcher: it shares no state with it and survives listener restarts.

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

#[cfg(windows)]
pub fn spawn_poller(app: tauri::AppHandle) {
    use std::time::Duration;
    use tauri::Emitter;

    let spawned = std::thread::Builder::new()
        .name("elevation-poll".into())
        .spawn(move || {
            match win::self_elevation() {
                Elevation::NotElevated => {}
                Elevation::Elevated => return, // an elevated ClipLite is not blocked
                Elevation::Unknown => {
                    log::warn!("could not read ClipLite's own token; elevation notices disabled");
                    return;
                }
            }
            let self_elevated = false;
            let mut notice = ElevationNotice::new();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if let Some(fg) = win::foreground() {
                    if fg.elevation == Elevation::Unknown {
                        log::debug!("foreground elevation unknown ({}); no notice", fg.exe);
                        continue;
                    }
                    let elevated = fg.elevation == Elevation::Elevated;
                    if let Some(exe) = notice.observe(elevated, self_elevated, &fg.exe) {
                        log::info!("elevated foreground app: {exe}");
                        app.emit("elevated-foreground", serde_json::json!({ "exe": exe })).ok();
                    }
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("elevation poller not started: {e}");
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

    pub struct Foreground {
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

    /// Foreground process info. `None` when there is no foreground window or
    /// it is ClipLite itself.
    pub fn foreground() -> Option<Foreground> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return None;
            }
            let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return Some(Foreground {
                    elevation: classify(false, Err(ErrKind::Other)),
                    exe: format!("pid {pid}"),
                });
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
            Some(Foreground { elevation, exe })
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
}
