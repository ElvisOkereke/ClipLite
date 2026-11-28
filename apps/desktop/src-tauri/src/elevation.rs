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

#[cfg(windows)]
pub fn spawn_poller(app: tauri::AppHandle) {
    use std::time::Duration;
    use tauri::Emitter;

    let spawned = std::thread::Builder::new()
        .name("elevation-poll".into())
        .spawn(move || {
            let self_elevated = win::self_elevated();
            if self_elevated {
                return; // an elevated ClipLite is not blocked; nothing to say
            }
            let mut notice = ElevationNotice::new();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if let Some(fg) = win::foreground() {
                    if let Some(exe) = notice.observe(fg.elevated, self_elevated, &fg.exe) {
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
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    pub struct Foreground {
        pub elevated: bool,
        pub exe: String,
    }

    unsafe fn token_elevated(token: HANDLE) -> Option<bool> {
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .ok()?;
        Some(elev.TokenIsElevated != 0)
    }

    pub fn self_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let r = token_elevated(token).unwrap_or(false);
            let _ = CloseHandle(token);
            r
        }
    }

    /// Foreground process info. `None` when there is no foreground window or
    /// it is ClipLite itself. If the process can't be opened at all (access
    /// denied) it is treated as elevated: a normal-integrity process can open
    /// any same-user, non-protected process for limited query.
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
                return Some(Foreground { elevated: true, exe: String::new() });
            };
            let mut exe = String::new();
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                exe = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
            }
            let mut token = HANDLE::default();
            let elevated = if OpenProcessToken(proc, TOKEN_QUERY, &mut token).is_ok() {
                let r = token_elevated(token).unwrap_or(false);
                let _ = CloseHandle(token);
                r
            } else {
                true // token not openable from a normal process: elevated/protected
            };
            let _ = CloseHandle(proc);
            Some(Foreground { elevated, exe })
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
}
