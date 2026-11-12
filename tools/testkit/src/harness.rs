//! Drives a ClipLite build from the outside: isolated app dir, test-mode
//! launch, window control, fake-game launch and focus.
#![cfg(windows)]
use crate::job::{kill_pid, processes_under, Job};
use crate::matrix::WindowState;
use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The replay-save chord written into the isolated settings. Chosen so the
/// owner's own ClipLite (default Ctrl+Shift+F) never reacts to test input.
pub const DEFAULT_CHORD: &str = "Ctrl+Alt+Shift+F11";
/// Start / stop recording chords for the bench's record scenario.
pub const RECORD_START_CHORD: &str = "Ctrl+Alt+Shift+F10";
pub const RECORD_STOP_CHORD: &str = "Ctrl+Alt+Shift+F9";

/// Executables the harness owns and may kill (never hil/fakegame themselves).
const APP_PROCS: [&str; 3] = ["cliplite.exe", "cliplite-recorder.exe", "ffmpeg.exe"];

pub struct Harness {
    pub app_dir: PathBuf,
    pub home: PathBuf,
    pub clips: PathBuf,
    pub chord: String,
    pub ready_at: Instant,
    child: Child,
    _job: Job,
}

/// Settings JSON for the isolated `CLIPLITE_HOME` (partial; serde defaults fill the rest).
pub fn test_settings(clips: &Path, buffer_secs: u32, chord: &str) -> serde_json::Value {
    serde_json::json!({
        "hotkeys": { "replay_save": chord, "start": RECORD_START_CHORD, "stop": RECORD_STOP_CHORD },
        "replay_buffer_duration_secs": buffer_secs,
        "replay_output_dir": clips,
        "output_dir": clips,
        "selected_monitor": 0,
        "max_replay_buffer_size_mb": 500
    })
}

fn kill_app_procs(app_dir: &Path) {
    for p in processes_under(app_dir) {
        if APP_PROCS.iter().any(|n| p.name.eq_ignore_ascii_case(n)) {
            kill_pid(p.pid);
        }
    }
}

impl Harness {
    pub fn start(app: &Path, out: &Path, buffer_secs: u32, chord: &str) -> Result<Harness> {
        let app = app.canonicalize().with_context(|| format!("app not found: {}", app.display()))?;
        let app_dir = app.parent().unwrap().to_path_buf();
        kill_app_procs(&app_dir); // stale processes from an earlier crashed run

        let home = out.join("home");
        let clips = home.join("clips");
        std::fs::create_dir_all(&clips)?;
        std::fs::write(home.join("settings.json"), serde_json::to_vec_pretty(&test_settings(&clips, buffer_secs, chord))?)?;

        let job = Job::new_kill_on_close(true)?;
        let child = job.spawn(
            Command::new(&app)
                .env("CLIPLITE_HOME", &home)
                .env("CLIPLITE_TEST_MODE", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )?;

        let t0 = Instant::now();
        loop {
            let up = processes_under(&app_dir).iter().any(|p| p.name.eq_ignore_ascii_case("cliplite-recorder.exe"));
            if up {
                break;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                bail!("replay buffer did not start within 30 s (no cliplite-recorder.exe under {})", app_dir.display());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        std::thread::sleep(Duration::from_secs(10)); // let the buffer fill and the UI poll sync
        Ok(Harness { app_dir, home, clips, chord: chord.to_string(), ready_at: Instant::now(), child, _job: job })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn ffmpeg_tools(&self) -> Result<(PathBuf, PathBuf)> {
        tools_in(&self.app_dir.join("bin"))
    }

    /// `.mp4` files currently in the clips folder.
    pub fn list_clips(&self) -> Vec<PathBuf> {
        list_mp4(&self.clips)
    }

    pub fn main_window(&self) -> Option<HWND> {
        windows_of(self.pid()).into_iter().find(|(_, title)| title == "ClipLite").map(|(h, _)| h)
    }

    pub fn set_window(&self, state: WindowState) -> Result<()> {
        let hwnd = self.main_window().ok_or_else(|| anyhow!("ClipLite main window not found"))?;
        unsafe {
            match state {
                // The app hides to tray on close (it never exits on WM_CLOSE).
                WindowState::Hidden => PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0))?,
                WindowState::Visible => {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        kill_app_procs(&self.app_dir); // the recorder breaks away from the job
    }
}

pub fn tools_in(bin: &Path) -> Result<(PathBuf, PathBuf)> {
    let (ff, fp) = (bin.join("ffmpeg.exe"), bin.join("ffprobe.exe"));
    if !ff.exists() || !fp.exists() {
        bail!("ffmpeg.exe/ffprobe.exe not found in {} (run scripts/fetch-ffmpeg.ps1, then rebuild)", bin.display());
    }
    Ok((ff, fp))
}

pub fn list_mp4(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"))).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Top-level windows (including hidden) owned by `pid`, with titles.
pub fn windows_of(pid: u32) -> Vec<(HWND, String)> {
    struct Ctx {
        pid: u32,
        out: Vec<(HWND, String)>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == ctx.pid {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf);
            ctx.out.push((hwnd, String::from_utf16_lossy(&buf[..n.max(0) as usize])));
        }
        BOOL(1)
    }
    let mut ctx = Ctx { pid, out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.out
}

/// Bring `hwnd` to the foreground despite the foreground lock: the process
/// that produced the last input event may set the foreground window, so tap
/// an unused key (F24) first.
pub fn ensure_foreground(hwnd: HWND) -> bool {
    for _ in 0..5 {
        unsafe {
            if GetForegroundWindow() == hwnd {
                return true;
            }
            let _ = crate::input::send_keys(&[crate::input::VK_F24]);
            let _ = SetForegroundWindow(hwnd);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unsafe { GetForegroundWindow() == hwnd }
}

/// A running fake game.
pub struct Game {
    pub child: Child,
    pub hwnd: HWND,
    pub fullscreen: bool,
}

/// Parse the fake game's `READY hwnd=0x… fullscreen=…` line.
pub fn parse_ready(line: &str) -> Option<(isize, bool)> {
    let rest = line.strip_prefix("READY ")?;
    let mut hwnd = None;
    let mut fs = None;
    for kv in rest.split_whitespace() {
        if let Some(h) = kv.strip_prefix("hwnd=0x") {
            hwnd = isize::from_str_radix(h, 16).ok();
        } else if let Some(f) = kv.strip_prefix("fullscreen=") {
            fs = f.parse().ok();
        }
    }
    Some((hwnd?, fs?))
}

/// Launch the fake game inside `job` and wait (≤ 15 s) for READY.
pub fn launch_game(job: &Job, fakegame: &Path, args: &[String]) -> Result<Game> {
    let mut child = job.spawn(Command::new(fakegame).args(args).stdout(Stdio::piped()).stderr(Stdio::null()))?;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(r) = parse_ready(&line) {
                let _ = tx.send(r);
                return;
            }
        }
    });
    // Grant focus as soon as the window exists, so exclusive fullscreen can
    // engage before READY (the game waits for it, up to 5 s).
    let pid = child.id();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if let Some((h, _)) = windows_of(pid).into_iter().find(|(_, t)| t == crate::fakegame::WINDOW_TITLE) {
            ensure_foreground(h);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok((h, fullscreen)) => Ok(Game { child, hwnd: HWND(h as _), fullscreen }),
        Err(_) => {
            let _ = child.kill();
            bail!("fake game did not report READY within 15 s")
        }
    }
}

impl Game {
    /// WM_CLOSE, then kill after 3 s.
    pub fn close(mut self) {
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Wait up to `timeout` for new files (not in `before`) whose size is stable
/// for 1 s. Returns them and the latency to the first appearance.
pub fn wait_new_clips(dir: &Path, before: &[PathBuf], timeout: Duration) -> (Vec<PathBuf>, Option<u64>) {
    let t0 = Instant::now();
    let mut first_seen: Option<u64> = None;
    loop {
        let new: Vec<PathBuf> = list_mp4(dir).into_iter().filter(|p| !before.contains(p)).collect();
        if !new.is_empty() {
            first_seen.get_or_insert(t0.elapsed().as_millis() as u64);
            let sizes = |v: &[PathBuf]| v.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect::<Vec<_>>();
            let a = sizes(&new);
            std::thread::sleep(Duration::from_secs(1));
            let again: Vec<PathBuf> = list_mp4(dir).into_iter().filter(|p| !before.contains(p)).collect();
            if again == new && sizes(&again) == a && a.iter().all(|s| *s > 0) {
                return (again, first_seen);
            }
            continue;
        }
        if t0.elapsed() >= timeout {
            return (new, first_seen);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_line() {
        assert_eq!(parse_ready("READY hwnd=0x1a2b fullscreen=true"), Some((0x1a2b, true)));
        assert_eq!(parse_ready("READY hwnd=0x10 fullscreen=false"), Some((0x10, false)));
        assert_eq!(parse_ready("hello"), None);
    }

    #[test]
    fn settings_use_given_chord_and_dirs() {
        let v = test_settings(Path::new(r"C:\x y\clips"), 60, DEFAULT_CHORD);
        assert_eq!(v["hotkeys"]["replay_save"], "Ctrl+Alt+Shift+F11");
        assert_eq!(v["replay_buffer_duration_secs"], 60);
        assert_eq!(v["replay_output_dir"], r"C:\x y\clips");
        assert_eq!(v["hotkeys"]["start"], RECORD_START_CHORD);
        assert_eq!(v["hotkeys"]["stop"], RECORD_STOP_CHORD);
    }

    #[test]
    fn new_clip_detection_ignores_existing_files() {
        let dir = std::env::temp_dir().join("clip lite wait test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.mp4"), b"x").unwrap();
        let before = list_mp4(&dir);
        std::fs::write(dir.join("new.mp4"), b"yy").unwrap();
        let (new, lat) = wait_new_clips(&dir, &before, Duration::from_secs(3));
        assert_eq!(new, vec![dir.join("new.mp4")]);
        assert!(lat.is_some());
        let (none, _) = wait_new_clips(&dir, &list_mp4(&dir), Duration::from_millis(300));
        assert!(none.is_empty());
    }
}
