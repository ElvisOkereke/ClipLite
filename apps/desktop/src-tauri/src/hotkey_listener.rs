//! Global hotkeys: one input thread feeds one dispatcher thread, and actions
//! run in Rust (see `hotkey_actions`), never in the WebView.
//!
//! Input thread (Windows): owns a message pump, a message-only window and two
//! low-level hooks.
//! - `WH_KEYBOARD_LL` + `WH_MOUSE_LL`: the callbacks only translate the event
//!   into a [`RawEvent`], `try_send` it to the dispatcher and call
//!   `CallNextHookEx`. No logging, no locks, no IPC. LL hooks also see input
//!   while a fullscreen game has focus.
//! - `RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk)` for every binding that
//!   has a `registerhotkey_form`: a second, independent path (`WM_HOTKEY`) for
//!   when Windows silently drops the hooks (callback timeout, sleep, UAC).
//! - Watchdog: both hooks are unhooked and reinstalled every 60 s, and after
//!   resume from sleep / session unlock, on this thread (a posted thread
//!   message or `WM_TIMER`), so the message pump stays the owner.
//!
//! Dispatcher thread: runs the [`Matcher`] (or [`Capture`] while the settings
//! UI records a chord), de-duplicates the two paths ([`Dedup`]), logs
//! `hotkey.pressed` / `hotkey.dispatched`, and queues the action for one serial
//! worker task (actions run one at a time, in press order), which emits
//! `hotkey-fired` after each action ran. Captures time out after 30 s.
use std::{
    collections::HashMap,
    sync::{
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use cliplite_core::hotkey::{find_conflicts, parse_bindings, Capture, CaptureState, Chord, Input, Matcher, RawEvent};
use cliplite_core::telemetry::{new_cid, Stage};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::hotkey_actions::{self, Action};

/// Hook events queued for the dispatcher before the hook starts dropping them.
const CHANNEL_CAPACITY: usize = 4096;
/// The hook and `WM_HOTKEY` report the same press within this window.
const DEDUP_WINDOW: Duration = Duration::from_millis(250);
#[cfg(windows)]
const WATCHDOG_INTERVAL_MS: u32 = 60_000;

// ── Pure parts ────────────────────────────────────────────────────────────────

/// Which path reported a press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Hook,
    Register,
}

/// One press arrives once from the hook and once as `WM_HOTKEY`; run it once.
/// A second event from the *other* source within [`DEDUP_WINDOW`] is that
/// press's twin and is dropped (once). Events from the same source are always
/// separate presses.
#[derive(Debug, Default)]
pub struct Dedup {
    /// action -> (source, time, twin already dropped)
    last: HashMap<String, (Source, Instant, bool)>,
}

impl Dedup {
    pub fn should_run(&mut self, action: &str, source: Source, now: Instant) -> bool {
        if let Some((src, t, paired)) = self.last.get_mut(action) {
            if *src != source && !*paired && now.saturating_duration_since(*t) < DEDUP_WINDOW {
                *paired = true;
                return false;
            }
        }
        self.last.insert(action.to_string(), (source, now, false));
        true
    }
}

/// Per-binding health, shown by `get_debug_state` (`hotkey_status.bindings`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BindingStatus {
    pub action: String,
    /// The saved string.
    pub binding: String,
    pub parsed: bool,
    /// Parse error.
    pub error: Option<String>,
    pub canonical: Option<String>,
    pub display: Option<String>,
    /// `None`: not expressible as `RegisterHotKey` (mouse, modifier-only, two
    /// keys) or not attempted yet.
    pub registered: Option<bool>,
    /// Why `RegisterHotKey` failed (usually: another app owns the combo).
    pub register_error: Option<String>,
}

/// A binding to pass to `RegisterHotKey`; `id` is the `WM_HOTKEY` wParam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterSpec {
    pub id: i32,
    pub action: String,
    pub mods: u32,
    pub vk: u32,
}

pub struct Bindings {
    /// Parsed bindings of known actions, for the matcher.
    pub active: Vec<(String, Chord)>,
    pub status: Vec<BindingStatus>,
    pub register: Vec<RegisterSpec>,
}

/// Turn `Settings::hotkeys` into matcher bindings. Empty strings are unbound;
/// unknown actions (e.g. `annotate`, which has no action) are skipped with a
/// debug log; strings that fail to parse are logged and skipped. Never fails.
pub fn build_bindings(map: &HashMap<String, String>) -> Bindings {
    let known = map
        .iter()
        .filter(|(action, _)| match action.parse::<Action>() {
            Ok(_) => true,
            Err(_) => {
                log::debug!("[hotkeys] no action named {action:?}; binding ignored");
                false
            }
        })
        .map(|(a, s)| (a.clone(), s.clone()));
    let (active, errors) = parse_bindings(known);

    let mut status = Vec::new();
    let mut register = Vec::new();
    for (action, chord) in &active {
        let form = chord.registerhotkey_form();
        if let Some((mods, vk)) = form {
            register.push(RegisterSpec { id: register.len() as i32 + 1, action: action.clone(), mods, vk });
        }
        status.push(BindingStatus {
            action: action.clone(),
            binding: map[action].clone(),
            parsed: true,
            error: None,
            canonical: Some(chord.to_canonical()),
            display: Some(chord.display()),
            registered: None,
            register_error: None,
        });
    }
    for (action, error) in errors {
        log::warn!("[hotkeys] {action}: cannot parse {error}; binding skipped");
        status.push(BindingStatus {
            action: action.clone(),
            binding: map[&action].clone(),
            parsed: false,
            error: Some(error),
            canonical: None,
            display: None,
            registered: None,
            register_error: None,
        });
    }
    status.sort_by(|a, b| a.action.cmp(&b.action));
    for (a, b) in find_conflicts(&active) {
        log::warn!("[hotkeys] {a} and {b} have the same chord; only one will fire");
    }
    Bindings { active, status, register }
}

// ── Shared state ──────────────────────────────────────────────────────────────

/// Listener health (`get_debug_state().hotkey_status`). Written by the input
/// and dispatcher threads (never by a hook callback).
#[derive(Debug, Clone, Default, Serialize)]
pub struct HotkeyStatus {
    pub bindings: Vec<BindingStatus>,
    pub keyboard_hook: bool,
    pub mouse_hook: bool,
    pub hook_error: Option<String>,
    /// Local time of the last (re)install of the hooks.
    pub last_hook_install: Option<String>,
    pub hook_installs: u32,
    pub capturing: bool,
    /// The input thread's message loop is running.
    pub input_alive: bool,
    pub dispatcher_alive: bool,
}

/// Messages to the dispatcher thread.
#[derive(Debug)]
pub(crate) enum Msg {
    /// From a hook callback.
    Raw(RawEvent),
    /// `WM_HOTKEY` for this action.
    Registered(String),
    /// Hooks reinstalled; these raw inputs are held according to the OS.
    /// Cancels a capture in progress (up-events may have been lost).
    Resync(Vec<Input>),
    Simulate(Action),
    BeginCapture,
    CancelCapture,
    Stop,
}

// ── Listener ──────────────────────────────────────────────────────────────────

pub struct HotkeyListener {
    tx: SyncSender<Msg>,
    status: Arc<Mutex<HotkeyStatus>>,
    /// Input thread id (0 if it failed to start / non-Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    input_tid: u32,
    input: Mutex<Option<JoinHandle<()>>>,
    dispatcher: Mutex<Option<JoinHandle<()>>>,
}

impl HotkeyListener {
    pub fn start(app: AppHandle, bindings: HashMap<String, String>) -> Self {
        let b = build_bindings(&bindings);
        for s in &b.status {
            if s.parsed {
                log::info!("[hotkeys] {} = {}", s.action, s.canonical.as_deref().unwrap_or_default());
            }
        }
        let status = Arc::new(Mutex::new(HotkeyStatus { bindings: b.status.clone(), ..Default::default() }));
        let (tx, rx) = sync_channel(CHANNEL_CAPACITY);

        let dispatcher = {
            let (app, status, active) = (app.clone(), status.clone(), b.active.clone());
            std::thread::Builder::new()
                .name("hotkey-dispatch".into())
                .spawn(move || dispatch_loop(app, rx, active, status))
                .map_err(|e| log::error!("[hotkeys] dispatcher thread failed to start: {e}"))
                .ok()
        };

        #[cfg(windows)]
        let (input, input_tid) = win::spawn_input_thread(tx.clone(), b.register, status.clone());
        #[cfg(not(windows))]
        let (input, input_tid) = {
            let _ = b.register;
            (None, 0)
        };

        Self { tx, status, input_tid, input: Mutex::new(input), dispatcher: Mutex::new(dispatcher) }
    }

    /// Stop both threads and wait for them: hooks unhooked, hotkeys
    /// unregistered and the window destroyed before this returns. Idempotent.
    pub fn stop_and_wait(&self) {
        if let Some(h) = self.input.lock().unwrap().take() {
            #[cfg(windows)]
            win::post_quit(self.input_tid);
            if h.join().is_err() {
                log::error!("[hotkeys] input thread panicked");
            }
        }
        if let Some(h) = self.dispatcher.lock().unwrap().take() {
            // Blocking send: the dispatcher drains the queue, so this returns.
            let _ = self.tx.send(Msg::Stop);
            if h.join().is_err() {
                log::error!("[hotkeys] dispatcher thread panicked");
            }
        }
    }

    pub fn status(&self) -> HotkeyStatus {
        self.status.lock().unwrap().clone()
    }

    fn send(&self, msg: Msg) -> Result<(), String> {
        self.tx.try_send(msg).map_err(|e| format!("hotkey dispatcher unavailable: {e}"))
    }

    pub fn simulate(&self, action: Action) -> Result<(), String> {
        self.send(Msg::Simulate(action))
    }

    pub fn begin_capture(&self) -> Result<(), String> {
        self.send(Msg::BeginCapture)
    }

    pub fn cancel_capture(&self) -> Result<(), String> {
        self.send(Msg::CancelCapture)
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop_and_wait();
    }
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

/// A capture with no completed chord after this long is cancelled, so a lost
/// up-event (Win+L, UAC secure desktop, a dropped hook) cannot leave the
/// matcher paused forever.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the dispatcher loop must do after a message (keeps [`Dispatcher`] pure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Queue this action (already de-duplicated). `source` `None` = simulated.
    Run { action: Action, display: String, source: Option<Source> },
    /// Emit `hotkey-capture` `{ state, canonical, display }`.
    Capture { state: &'static str, canonical: String, display: String },
    /// Emit `hotkey-capture` `{ state: "cancelled", reason }`.
    CaptureCancelled { reason: &'static str },
    /// Read the OS key state and pass it to [`Dispatcher::resync`].
    Resync,
}

/// Dispatcher state: matcher, capture and de-duplication. Pure; the loop in
/// `dispatch_loop` performs the returned [`Effect`]s.
pub struct Dispatcher {
    matcher: Matcher,
    /// Capture in progress and when it started.
    capture: Option<(Capture, Instant)>,
    last_holding: Option<Chord>,
    dedup: Dedup,
    displays: HashMap<String, String>,
}

impl Dispatcher {
    pub fn new(active: Vec<(String, Chord)>) -> Self {
        let displays = active.iter().map(|(a, c)| (a.clone(), c.display())).collect();
        Self { matcher: Matcher::new(active), capture: None, last_holding: None, dedup: Dedup::default(), displays }
    }

    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    pub fn capture_deadline(&self) -> Option<Instant> {
        self.capture.as_ref().map(|(_, t)| *t + CAPTURE_TIMEOUT)
    }

    /// Held state from the OS (after a capture ends): nothing fires, and a
    /// modifier still held keeps counting.
    pub fn resync(&mut self, held: Vec<Input>) {
        self.matcher.reset_held(held);
    }

    /// Cancel the capture if it has run past [`CAPTURE_TIMEOUT`].
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        match self.capture_deadline() {
            Some(d) if now >= d => self.end_capture("timeout"),
            _ => Vec::new(),
        }
    }

    fn end_capture(&mut self, reason: &'static str) -> Vec<Effect> {
        self.capture = None;
        self.last_holding = None;
        vec![Effect::CaptureCancelled { reason }, Effect::Resync]
    }

    /// Handle one message (`Msg::Stop` is the loop's business: ignored here).
    pub fn handle(&mut self, msg: Msg, now: Instant) -> Vec<Effect> {
        match msg {
            Msg::Raw(ev) => match self.capture.as_mut() {
                Some((cap, _)) => match cap.on_event(ev) {
                    CaptureState::Idle => Vec::new(),
                    // Only report changes (not key repeat, clicks or wheel).
                    CaptureState::Holding(chord) => {
                        if self.last_holding.as_ref() == Some(&chord) {
                            return Vec::new();
                        }
                        let e = capture_effect("holding", &chord);
                        self.last_holding = Some(chord);
                        vec![e]
                    }
                    CaptureState::Done(chord) => {
                        self.capture = None;
                        self.last_holding = None;
                        vec![capture_effect("done", &chord), Effect::Resync]
                    }
                },
                None => match self.matcher.on_event(ev) {
                    Some(action) => self.run(&action, Some(Source::Hook), now),
                    None => Vec::new(),
                },
            },
            // The matcher is paused during a capture, and so is this path.
            Msg::Registered(action) if !self.capturing() => self.run(&action, Some(Source::Register), now),
            Msg::Registered(_) => Vec::new(),
            Msg::Simulate(action) => self.run(action.as_str(), None, now),
            Msg::Resync(held) => {
                self.matcher.reset_held(held);
                if self.capturing() {
                    self.capture = None;
                    self.last_holding = None;
                    vec![Effect::CaptureCancelled { reason: "reinstall" }]
                } else {
                    Vec::new()
                }
            }
            Msg::BeginCapture => {
                self.capture = Some((Capture::default(), now));
                self.last_holding = None;
                Vec::new()
            }
            Msg::CancelCapture if self.capturing() => self.end_capture("user"),
            Msg::CancelCapture | Msg::Stop => Vec::new(),
        }
    }

    fn run(&mut self, name: &str, source: Option<Source>, now: Instant) -> Vec<Effect> {
        let Ok(action) = name.parse::<Action>() else {
            log::debug!("[hotkeys] no action named {name:?}");
            return Vec::new();
        };
        if let Some(src) = source {
            if !self.dedup.should_run(name, src, now) {
                log::debug!("[hotkeys] {name}: duplicate from {src:?} dropped");
                return Vec::new();
            }
        }
        let display = self.displays.get(name).cloned().unwrap_or_default();
        vec![Effect::Run { action, display, source }]
    }
}

fn capture_effect(state: &'static str, chord: &Chord) -> Effect {
    Effect::Capture { state, canonical: chord.to_canonical(), display: chord.display() }
}

/// One hotkey action waiting for the serial worker.
struct Job {
    action: Action,
    cid: String,
    display: String,
}

/// Run jobs one at a time, in arrival order, until every sender is gone.
/// Hotkey actions are serialized: a double-tapped pause or replay toggle must
/// see the result of the first press, and a second start must not race the
/// first one's engine start.
pub(crate) async fn run_serially<J, F, Fut>(mut rx: tokio::sync::mpsc::UnboundedReceiver<J>, mut run: F)
where
    F: FnMut(J) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    while let Some(job) = rx.recv().await {
        run(job).await;
    }
}

/// The long-lived worker that runs hotkey actions and emits `hotkey-fired`.
fn spawn_action_worker(app: AppHandle) -> tokio::sync::mpsc::UnboundedSender<Job> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
    tauri::async_runtime::spawn(run_serially(rx, move |job: Job| {
        let app = app.clone();
        async move {
            let Job { action, cid, display } = job;
            let name = action.as_str();
            let payload = match hotkey_actions::run(&app, action, &cid).await {
                Ok(()) => {
                    tracing::info!(cid = %cid, action = name, "hotkey action done");
                    serde_json::json!({ "action": name, "cid": cid, "display": display })
                }
                Err(error) => {
                    tracing::warn!(stage = Stage::Error.as_str(), cid = %cid, action = name, %error, "hotkey action failed");
                    serde_json::json!({ "action": name, "cid": cid, "error": error })
                }
            };
            app.emit("hotkey-fired", payload).ok();
        }
    }));
    tx
}

/// The OS's current key state (for [`Dispatcher::resync`]).
fn os_held_inputs() -> Vec<Input> {
    #[cfg(windows)]
    return win::held_now();
    #[cfg(not(windows))]
    return Vec::new();
}

fn dispatch_loop(app: AppHandle, rx: Receiver<Msg>, active: Vec<(String, Chord)>, status: Arc<Mutex<HotkeyStatus>>) {
    status.lock().unwrap().dispatcher_alive = true;
    let mut state = Dispatcher::new(active);
    let jobs = spawn_action_worker(app.clone());

    loop {
        let effects = match state.capture_deadline() {
            Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Msg::Stop) => break,
                Ok(msg) => state.handle(msg, Instant::now()),
                Err(RecvTimeoutError::Timeout) => state.tick(Instant::now()),
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(Msg::Stop) | Err(_) => break,
                Ok(msg) => state.handle(msg, Instant::now()),
            },
        };
        for effect in effects {
            match effect {
                Effect::Run { action, display: chord_display, source } => {
                    let cid = new_cid();
                    let src = source.map_or("simulate", |s| match s {
                        Source::Hook => "hook",
                        Source::Register => "register",
                    });
                    tracing::info!(stage = Stage::HotkeyPressed.as_str(), cid = %cid, action = action.as_str(),
                                   source = src, chord = %chord_display, "hotkey pressed");
                    // Logged before the job runs so the log order is pressed ->
                    // dispatched -> command.received (the HIL matrix sorts by time).
                    tracing::info!(stage = Stage::HotkeyDispatched.as_str(), cid = %cid,
                                   action = action.as_str(), "hotkey dispatched");
                    if jobs.send(Job { action, cid, display: chord_display }).is_err() {
                        log::error!("[hotkeys] action worker is gone; {} dropped", action.as_str());
                    }
                }
                Effect::Capture { state: s, canonical, display } => {
                    let payload = serde_json::json!({ "state": s, "canonical": canonical, "display": display });
                    app.emit("hotkey-capture", payload).ok();
                }
                Effect::CaptureCancelled { reason } => {
                    log::info!("[hotkeys] capture cancelled ({reason})");
                    app.emit("hotkey-capture", serde_json::json!({ "state": "cancelled", "reason": reason })).ok();
                }
                Effect::Resync => state.resync(os_held_inputs()),
            }
        }
        status.lock().unwrap().capturing = state.capturing();
    }
    {
        let mut st = status.lock().unwrap();
        st.dispatcher_alive = false;
        st.capturing = false;
    }
    log::info!("[hotkeys] dispatcher exited");
}

// ── Windows input thread ──────────────────────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use cliplite_core::hotkey::MouseButton;
    use std::cell::RefCell;
    use windows::core::w;
    use windows::Win32::{
        Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Power::{RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification},
            RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION},
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT},
            WindowsAndMessaging::*,
        },
    };

    /// Posted to the input thread: reinstall both hooks.
    const WM_APP_REINSTALL: u32 = WM_APP + 1;

    thread_local! {
        /// The dispatcher channel for the hook callbacks. LL hook callbacks run
        /// on the installing thread, so a thread-local needs no lock.
        static HOOK_TX: RefCell<Option<SyncSender<Msg>>> = const { RefCell::new(None) };
    }

    /// Hook callback side: never blocks (drops the event if the queue is full).
    fn forward(ev: RawEvent) {
        let _ = HOOK_TX.try_with(|tx| {
            if let Ok(tx) = tx.try_borrow() {
                if let Some(tx) = tx.as_ref() {
                    let _ = tx.try_send(Msg::Raw(ev));
                }
            }
        });
    }

    // Injected input (LLKHF_INJECTED / LLMHF_INJECTED) is accepted on purpose,
    // in and out of test mode: the HIL kit injects, and so do macro tools
    // people bind hotkeys to (Stream Deck, AutoHotkey, mouse software).
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            match wparam.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => forward(RawEvent::Down(Input::Key(kb.vkCode))),
                WM_KEYUP | WM_SYSKEYUP => forward(RawEvent::Up(Input::Key(kb.vkCode))),
                _ => {}
            }
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let msg = wparam.0 as u32;
        if code >= 0 && msg != WM_MOUSEMOVE {
            let ev = match msg {
                WM_MBUTTONDOWN => Some(RawEvent::Down(Input::Mouse(MouseButton::Middle))),
                WM_MBUTTONUP => Some(RawEvent::Up(Input::Mouse(MouseButton::Middle))),
                WM_XBUTTONDOWN | WM_XBUTTONUP => {
                    let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                    let button = match (ms.mouseData >> 16) as u16 {
                        XBUTTON1 => Some(MouseButton::X1),
                        XBUTTON2 => Some(MouseButton::X2),
                        _ => None,
                    };
                    button.map(|b| {
                        if msg == WM_XBUTTONDOWN {
                            RawEvent::Down(Input::Mouse(b))
                        } else {
                            RawEvent::Up(Input::Mouse(b))
                        }
                    })
                }
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(RawEvent::Other),
                _ => None,
            };
            if let Some(ev) = ev {
                forward(ev);
            }
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let resume = msg == WM_POWERBROADCAST
            && matches!(wparam.0 as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND);
        let unlock = msg == WM_WTSSESSION_CHANGE && wparam.0 as u32 == WTS_SESSION_UNLOCK;
        if resume || unlock {
            // Runs on the input thread; reinstall from the pump, not from here.
            let _ = PostThreadMessageW(GetCurrentThreadId(), WM_APP_REINSTALL, WPARAM(0), LPARAM(0));
            return if resume { LRESULT(1) } else { LRESULT(0) };
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    pub(super) fn post_quit(tid: u32) {
        if tid != 0 {
            unsafe {
                if let Err(e) = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) {
                    log::warn!("[hotkeys] post WM_QUIT to input thread: {e}");
                }
            }
        }
    }

    /// Start the input thread; returns its handle and thread id once its
    /// message queue exists (so `post_quit` cannot be lost).
    pub(super) fn spawn_input_thread(
        tx: SyncSender<Msg>,
        register: Vec<RegisterSpec>,
        status: Arc<Mutex<HotkeyStatus>>,
    ) -> (Option<JoinHandle<()>>, u32) {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("hotkey-input".into())
            .spawn(move || input_thread(tx, register, status, ready_tx));
        let handle = match spawned {
            Ok(h) => h,
            Err(e) => {
                log::error!("[hotkeys] input thread failed to start: {e}");
                return (None, 0);
            }
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(tid) => (Some(handle), tid),
            Err(e) => {
                // Without its id it cannot be told to quit: detach rather
                // than let stop_and_wait join it forever.
                log::error!("[hotkeys] input thread did not report ready: {e}");
                drop(handle);
                (None, 0)
            }
        }
    }

    struct Hooks {
        keyboard: Option<HHOOK>,
        mouse: Option<HHOOK>,
    }

    impl Hooks {
        fn unhook(&mut self) {
            for h in [self.keyboard.take(), self.mouse.take()].into_iter().flatten() {
                unsafe {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
        }

        fn install(&mut self, hinst: HINSTANCE, status: &Mutex<HotkeyStatus>) {
            self.unhook();
            let kb = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hinst, 0) };
            let ms = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hinst, 0) };
            let mut errors = Vec::new();
            match kb {
                Ok(h) => self.keyboard = Some(h),
                Err(e) => errors.push(format!("WH_KEYBOARD_LL: {e}")),
            }
            match ms {
                Ok(h) => self.mouse = Some(h),
                Err(e) => errors.push(format!("WH_MOUSE_LL: {e}")),
            }
            let mut st = status.lock().unwrap();
            st.keyboard_hook = self.keyboard.is_some();
            st.mouse_hook = self.mouse.is_some();
            st.hook_error = (!errors.is_empty()).then(|| errors.join("; "));
            st.last_hook_install = Some(chrono::Local::now().to_rfc3339());
            st.hook_installs += 1;
            for e in &errors {
                log::error!("[hotkeys] hook install failed: {e}");
            }
        }
    }

    /// Raw inputs the OS reports held right now (for `Matcher::reset_held`).
    /// Generic VK_SHIFT/CONTROL/MENU are skipped: the L/R variants report too.
    pub(super) fn held_now() -> Vec<Input> {
        let mut held = Vec::new();
        for vk in 4..=0xFEu32 {
            if matches!(vk, 0x10..=0x12) || unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 == 0 {
                continue;
            }
            held.push(match vk {
                0x04 => Input::Mouse(MouseButton::Middle),
                0x05 => Input::Mouse(MouseButton::X1),
                0x06 => Input::Mouse(MouseButton::X2),
                v => Input::Key(v),
            });
        }
        held
    }

    fn input_thread(
        tx: SyncSender<Msg>,
        register: Vec<RegisterSpec>,
        status: Arc<Mutex<HotkeyStatus>>,
        ready: std::sync::mpsc::Sender<u32>,
    ) {
        unsafe {
            // Create this thread's message queue before anyone posts to it.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, HWND::default(), WM_USER, WM_USER, PM_NOREMOVE);
        }
        let _ = ready.send(unsafe { GetCurrentThreadId() });
        HOOK_TX.with(|t| *t.borrow_mut() = Some(tx.clone()));

        let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }.map(Into::into).unwrap_or_default();
        let hwnd = create_message_window(hinst);

        // RegisterHotKey: a second path that survives dropped hooks.
        let mut registered: HashMap<i32, String> = HashMap::new();
        if let Some(hwnd) = hwnd {
            for spec in &register {
                let mods = HOT_KEY_MODIFIERS(spec.mods) | MOD_NOREPEAT;
                let res = unsafe { RegisterHotKey(hwnd, spec.id, mods, spec.vk) };
                let mut st = status.lock().unwrap();
                let entry = st.bindings.iter_mut().find(|b| b.action == spec.action);
                match res {
                    Ok(()) => {
                        registered.insert(spec.id, spec.action.clone());
                        if let Some(b) = entry {
                            b.registered = Some(true);
                        }
                    }
                    Err(e) => {
                        log::warn!("[hotkeys] RegisterHotKey for {} failed ({e}); the hook path still works",
                                   spec.action);
                        if let Some(b) = entry {
                            b.registered = Some(false);
                            b.register_error = Some(e.message().to_string());
                        }
                    }
                }
            }
        }

        // Resume / unlock notifications for the watchdog (best effort).
        let power = hwnd.and_then(|h| unsafe {
            RegisterSuspendResumeNotification(HANDLE(h.0), DEVICE_NOTIFY_WINDOW_HANDLE)
                .map_err(|e| log::warn!("[hotkeys] resume notifications unavailable: {e}"))
                .ok()
        });
        let session = hwnd.is_some_and(|h| unsafe {
            WTSRegisterSessionNotification(h, NOTIFY_FOR_THIS_SESSION)
                .map_err(|e| log::warn!("[hotkeys] unlock notifications unavailable: {e}"))
                .is_ok()
        });

        let mut hooks = Hooks { keyboard: None, mouse: None };
        hooks.install(hinst, &status);
        log::info!("[hotkeys] input thread running (keyboard hook {}, mouse hook {}, {} RegisterHotKey)",
                   hooks.keyboard.is_some(), hooks.mouse.is_some(), registered.len());
        // Thread timer (no window): the watchdog works even if the message
        // window could not be created. WM_TIMER arrives with hwnd NULL.
        let watchdog = unsafe { SetTimer(HWND::default(), 0, WATCHDOG_INTERVAL_MS, None) };
        if watchdog == 0 {
            log::error!("[hotkeys] watchdog timer failed; hooks are not reinstalled periodically");
        }
        status.lock().unwrap().input_alive = true;

        unsafe {
            let mut msg = MSG::default();
            loop {
                let r = GetMessageW(&mut msg, HWND::default(), 0, 0);
                if r.0 == 0 {
                    break; // WM_QUIT
                }
                if r.0 == -1 {
                    log::error!("[hotkeys] GetMessageW failed ({}); input thread exiting",
                                windows::core::Error::from_win32());
                    break;
                }
                match msg.message {
                    WM_HOTKEY => {
                        if let Some(action) = registered.get(&(msg.wParam.0 as i32)) {
                            if let Err(TrySendError::Full(_)) = tx.try_send(Msg::Registered(action.clone())) {
                                log::warn!("[hotkeys] dispatcher queue full; WM_HOTKEY dropped");
                            }
                        }
                    }
                    WM_TIMER if msg.hwnd.is_invalid() && watchdog != 0 && msg.wParam.0 == watchdog => reinstall(&mut hooks, hinst, &status, &tx),
                    WM_APP_REINSTALL => {
                        log::info!("[hotkeys] resume/unlock: reinstalling hooks");
                        reinstall(&mut hooks, hinst, &status, &tx);
                    }
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }

        // Teardown on this thread (it owns the hooks, hotkeys and window).
        hooks.unhook();
        unsafe {
            if watchdog != 0 {
                let _ = KillTimer(HWND::default(), watchdog);
            }
            if let Some(h) = hwnd {
                for id in registered.keys() {
                    let _ = UnregisterHotKey(h, *id);
                }
                if session {
                    let _ = WTSUnRegisterSessionNotification(h);
                }
                if let Some(p) = power {
                    let _ = UnregisterSuspendResumeNotification(p);
                }
                let _ = DestroyWindow(h);
            }
        }
        HOOK_TX.with(|t| *t.borrow_mut() = None);
        {
            let mut st = status.lock().unwrap();
            st.keyboard_hook = false;
            st.mouse_hook = false;
            st.input_alive = false;
        }
        log::info!("[hotkeys] input thread exited (hooks removed, {} hotkeys unregistered)", registered.len());
    }

    fn reinstall(hooks: &mut Hooks, hinst: HINSTANCE, status: &Mutex<HotkeyStatus>, tx: &SyncSender<Msg>) {
        hooks.install(hinst, status);
        // Up-events may have been missed while unhooked: resync from the OS.
        let _ = tx.try_send(Msg::Resync(held_now()));
    }

    fn create_message_window(hinst: HINSTANCE) -> Option<HWND> {
        let class = w!("ClipLiteHotkeyWindow");
        unsafe {
            let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinst, lpszClassName: class, ..Default::default() };
            // Fails harmlessly with "class already exists" on a re-register.
            RegisterClassW(&wc);
            match CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("ClipLite hotkeys"), WINDOW_STYLE(0),
                                  0, 0, 0, 0, HWND_MESSAGE, HMENU::default(), hinst, None) {
                Ok(h) => Some(h),
                Err(e) => {
                    log::error!("[hotkeys] message window failed ({e}); RegisterHotKey path disabled");
                    None
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "hotkey_listener_tests.rs"]
mod tests;
