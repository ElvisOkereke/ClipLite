//! Elevated helper for the "game runs as administrator" cells.
//!
//! UIPI blocks a medium-integrity process from injecting input into (or
//! focusing) an elevated window, so the harness launches `hil elevated-helper`
//! once via UAC (`runas`). The helper launches the elevated fake game and does
//! the focus + SendInput on the harness's behalf over a named pipe. ClipLite
//! itself stays medium integrity — that is the condition under test.
//!
//! Protocol: newline-delimited JSON requests/responses.
#![cfg(windows)]
use crate::harness::{ensure_foreground, launch_game, Game};
use crate::job::Job;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HANDLE;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    LaunchFakegame { args: Vec<String> },
    SendChord { chord: String },
    Close { pid: u32 },
    Quit,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwnd: Option<isize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
}

/// Pipe security: full access for the given user SID only (protected DACL),
/// labelled medium integrity so that user's non-elevated harness can write to
/// a pipe created by the elevated helper.
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})S:(ML;;NW;;;ME)")
}

/// Only the fake-game options the harness uses, with checked values: the
/// helper runs elevated, so it must not accept e.g. `--frametimes <any path>`.
pub fn validate_game_args(args: &[String]) -> Result<()> {
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--vsync" => {}
            "--mode" => match it.next().map(String::as_str) {
                Some("windowed" | "borderless" | "exclusive") => {}
                other => bail!("disallowed --mode value {other:?}"),
            },
            "--gpu-load" | "--cpu-threads" | "--duration-secs" | "--width" | "--height" => {
                let v = it.next().ok_or_else(|| anyhow!("{flag} needs a value"))?;
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("{flag} must be a number, got {v:?}");
                }
            }
            other => bail!("disallowed fake-game argument {other:?}"),
        }
    }
    Ok(())
}

/// String SID of the current process's user (same for elevated and not).
fn current_user_sid() -> Result<String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), len, &mut len)?;
        let _ = windows::Win32::Foundation::CloseHandle(token);
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s)?;
        let out = s.to_string()?;
        let _ = LocalFree(HLOCAL(s.0 as _));
        Ok(out)
    }
}

// ── client (harness side) ─────────────────────────────────────────────────────

pub struct Helper {
    reader: BufReader<File>,
    writer: File,
}

impl Helper {
    /// Launch `hil elevated-helper` via UAC and connect (waits ≤ 90 s for the prompt).
    pub fn launch(hil_exe: &Path) -> Result<Helper> {
        use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        let pipe = format!("cliplite_hil_{}", std::process::id());
        let file = HSTRING::from(hil_exe.as_os_str());
        let params = HSTRING::from(format!("elevated-helper --pipe {pipe} --client-pid {}", std::process::id()));
        let verb = HSTRING::from("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info).context("UAC launch of elevated helper (declined?)")? };

        let path = format!(r"\\.\pipe\{pipe}");
        let t0 = Instant::now();
        let handle = loop {
            match open_pipe_client(&path) {
                Ok(h) => break h,
                Err(_) if t0.elapsed() < Duration::from_secs(90) => std::thread::sleep(Duration::from_millis(250)),
                Err(e) => return Err(e.context("connect to elevated helper pipe")),
            }
        };
        let f = unsafe { File::from_raw_handle(handle.0 as _) };
        Ok(Helper { reader: BufReader::new(f.try_clone()?), writer: f })
    }

    fn call(&mut self, req: &Request) -> Result<Response> {
        writeln!(self.writer, "{}", serde_json::to_string(req)?)?;
        self.writer.flush()?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let resp: Response = serde_json::from_str(line.trim()).with_context(|| format!("bad helper reply: {line}"))?;
        if !resp.ok {
            bail!("helper error: {}", resp.error.unwrap_or_default());
        }
        Ok(resp)
    }

    /// Returns (pid, hwnd, fullscreen, focused).
    pub fn launch_fakegame(&mut self, args: Vec<String>) -> Result<(u32, isize, bool, bool)> {
        let r = self.call(&Request::LaunchFakegame { args })?;
        Ok((r.pid.unwrap_or(0), r.hwnd.unwrap_or(0), r.fullscreen.unwrap_or(false), r.focused.unwrap_or(false)))
    }

    pub fn send_chord(&mut self, chord: &str) -> Result<()> {
        self.call(&Request::SendChord { chord: chord.into() }).map(|_| ())
    }

    pub fn close(&mut self, pid: u32) -> Result<()> {
        self.call(&Request::Close { pid }).map(|_| ())
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.writer, "{}", serde_json::to_string(&Request::Quit).unwrap_or_default());
    }
}

fn open_pipe_client(path: &str) -> Result<HANDLE> {
    use windows::Win32::Storage::FileSystem::*;
    let p = HSTRING::from(path);
    let h = unsafe {
        CreateFileW(
            PCWSTR(p.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }?;
    Ok(h)
}

// ── server (elevated side) ────────────────────────────────────────────────────

/// Entry point for `hil elevated-helper --pipe NAME`.
pub fn serve(pipe: &str, fakegame: &Path, client_pid: u32) -> Result<()> {
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::*;

    let mut sd = PSECURITY_DESCRIPTOR::default();
    let sddl = HSTRING::from(pipe_sddl(&current_user_sid()?));
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None)? };
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let name = HSTRING::from(format!(r"\\.\pipe\{pipe}"));
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | windows::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            64 * 1024,
            64 * 1024,
            0,
            Some(&sa),
        )
    };
    if handle.is_invalid() {
        bail!("CreateNamedPipeW failed");
    }
    unsafe { ConnectNamedPipe(handle, None).or_else(|e| if e.code().0 as u32 == 0x80070217 { Ok(()) } else { Err(e) })? };
    // Serve only the harness that launched us.
    let mut connected_pid = 0u32;
    unsafe { GetNamedPipeClientProcessId(handle, &mut connected_pid)? };
    if connected_pid != client_pid {
        unsafe {
            let _ = DisconnectNamedPipe(handle);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        bail!("pipe client pid {connected_pid} is not the harness ({client_pid}); refusing");
    }
    let f = unsafe { File::from_raw_handle(handle.0 as _) };
    let mut writer = f.try_clone()?;
    let reader = BufReader::new(f);

    let job = Job::new_kill_on_close(false)?;
    let mut games: HashMap<u32, Game> = HashMap::new();
    for line in reader.lines() {
        let line = line?;
        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response { ok: false, error: Some(format!("parse: {e}")), ..Default::default() },
            Ok(Request::Quit) => break,
            Ok(req) => handle_request(req, &job, fakegame, &mut games).unwrap_or_else(|e| Response {
                ok: false,
                error: Some(format!("{e:#}")),
                ..Default::default()
            }),
        };
        writeln!(writer, "{}", serde_json::to_string(&resp)?)?;
        writer.flush()?;
    }
    for (_, g) in games.drain() {
        g.close();
    }
    Ok(())
}

fn handle_request(req: Request, job: &Job, fakegame: &Path, games: &mut HashMap<u32, Game>) -> Result<Response> {
    match req {
        Request::LaunchFakegame { args } => {
            validate_game_args(&args)?;
            let game = launch_game(job, fakegame, &args)?;
            let focused = ensure_foreground(game.hwnd);
            let r = Response {
                ok: true,
                pid: Some(game.child.id()),
                hwnd: Some(game.hwnd.0 as isize),
                fullscreen: Some(game.fullscreen),
                focused: Some(focused),
                ..Default::default()
            };
            games.insert(game.child.id(), game);
            Ok(r)
        }
        Request::SendChord { chord } => {
            crate::input::send_chord(&chord)?;
            Ok(Response { ok: true, ..Default::default() })
        }
        Request::Close { pid } => {
            let g = games.remove(&pid).ok_or_else(|| anyhow!("unknown pid {pid}"))?;
            g.close();
            Ok(Response { ok: true, ..Default::default() })
        }
        Request::Quit => Ok(Response { ok: true, ..Default::default() }),
    }
}

/// `fakegame.exe` next to the running `hil.exe`.
pub fn sibling_exe(name: &str) -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let p = me.parent().unwrap().join(name);
    if !p.exists() {
        bail!("{} not found next to {} (cargo build -p cliplite-testkit)", name, me.display());
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format() {
        let s = serde_json::to_string(&Request::SendChord { chord: "Ctrl+F".into() }).unwrap();
        assert_eq!(s, r#"{"op":"send_chord","chord":"Ctrl+F"}"#);
        let r: Request = serde_json::from_str(r#"{"op":"launch_fakegame","args":["--mode","exclusive"]}"#).unwrap();
        assert_eq!(r, Request::LaunchFakegame { args: vec!["--mode".into(), "exclusive".into()] });
    }

    #[test]
    fn pipe_acl_grants_only_the_given_user() {
        let sddl = pipe_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(sddl, "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)S:(ML;;NW;;;ME)");
        assert!(!sddl.contains(";WD)"), "must not grant Everyone");
    }

    #[test]
    fn game_args_are_whitelisted() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert!(validate_game_args(&v(&["--mode", "exclusive", "--gpu-load", "50", "--cpu-threads", "16"])).is_ok());
        assert!(validate_game_args(&v(&["--mode", "exclusive", "--frametimes", r"C:\Windows\x.csv"])).is_err());
        assert!(validate_game_args(&v(&["--mode", "evil"])).is_err());
        assert!(validate_game_args(&v(&["--gpu-load", "50;rm"])).is_err());
    }

    #[test]
    fn response_omits_empty_fields() {
        assert_eq!(serde_json::to_string(&Response { ok: true, ..Default::default() }).unwrap(), r#"{"ok":true}"#);
    }
}
