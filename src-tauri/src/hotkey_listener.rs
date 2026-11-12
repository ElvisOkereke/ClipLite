/// Global hotkey listener using WH_KEYBOARD_LL (low-level keyboard hook).
///
/// WHY NOT RegisterHotKey / global-hotkey crate:
///   RegisterHotKey is blocked by DirectX fullscreen-exclusive (FSE) games —
///   the game owns keyboard focus and the OS never delivers WM_HOTKEY to
///   background windows. This is a known Windows limitation.
///
/// WHY WH_KEYBOARD_LL WORKS IN GAMES:
///   SetWindowsHookEx(WH_KEYBOARD_LL) installs a hook in the kernel's input
///   pipeline *before* messages are dispatched to any window. It fires even
///   when a DirectX FSE game has exclusive focus, making it the correct API
///   for screen-recording hotkeys that must work during gameplay.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
    thread,
};
use tauri::{AppHandle, Emitter, Manager};

pub struct HotkeyListener {
    stop_flag: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct ParsedHotkey {
    vk:    u32,   // Virtual-key code
    ctrl:  bool,
    shift: bool,
    alt:   bool,
    event: String,
}

impl HotkeyListener {
    pub fn start(app: AppHandle, hotkeys: HashMap<String, String>) -> Self {
        let stop_flag  = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_flag.clone();

        thread::spawn(move || {
            #[cfg(windows)]
            run_hook_thread(app, hotkeys, stop_clone);

            #[cfg(not(windows))]
            {
                let _ = (app, hotkeys);
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }

            log::info!("[hotkeys] Listener thread exited");
        });

        Self { stop_flag }
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    pub fn stop_and_wait(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

// ── Windows implementation ─────────────────────────────────────────────────────

#[cfg(windows)]
fn run_hook_thread(
    app:       AppHandle,
    hotkeys:   HashMap<String, String>,
    stop_flag: Arc<AtomicBool>,
) {
    use windows::Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, PostThreadMessageW,
            SetWindowsHookExW, UnhookWindowsHookEx,
            HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN, WM_QUIT,
        },
        System::Threading::GetCurrentThreadId,
    };

    // ── Map action names to Tauri event names ─────────────────────────
    let action_events: HashMap<&str, &str> = [
        ("start",         "hotkey-start-recording"),
        ("stop",          "hotkey-stop-recording"),
        ("pause",         "hotkey-pause-recording"),
        ("library",       "hotkey-open-library"),
        ("replay_toggle", "hotkey-replay-toggle"),
        ("replay_save",   "hotkey-replay-save"),
    ].into_iter().collect();

    // ── Parse hotkey strings ──────────────────────────────────────────
    let mut parsed: Vec<ParsedHotkey> = Vec::new();
    for (action, key_str) in &hotkeys {
        if key_str.is_empty() { continue; }
        let Some(&event_name) = action_events.get(action.as_str()) else { continue };
        match parse_hotkey_to_vk(key_str) {
            Some((vk, ctrl, shift, alt)) => {
                log::info!("[hotkeys] OK  {key_str} → {event_name} (vk=0x{vk:02X} C={ctrl} S={shift} A={alt})");
                parsed.push(ParsedHotkey { vk, ctrl, shift, alt, event: event_name.to_string() });
            }
            None => log::warn!("[hotkeys] SKIP {key_str} — could not parse"),
        }
    }

    if parsed.is_empty() {
        log::warn!("[hotkeys] No valid hotkeys configured, hook not installed");
        return;
    }

    // ── Store hook data in a process-global static ────────────────────
    // The LL hook callback is a bare `unsafe extern "system" fn` — it cannot
    // capture a closure. We share state via a Mutex<Option<>> static.
    // Safety: only one hook thread runs at a time (enforced by stop_and_wait
    // before restart), so there is never concurrent mutation.
    static HOOK_DATA: Mutex<Option<HookData>> = Mutex::new(None);

    struct HookData {
        hotkeys: Vec<ParsedHotkey>,
        app:     AppHandle,
    }
    // SAFETY: AppHandle is Send+Sync; storing it behind a Mutex is safe.
    unsafe impl Send for HookData {}
    unsafe impl Sync for HookData {}

    *HOOK_DATA.lock().unwrap() = Some(HookData { hotkeys: parsed, app });

    // ── The low-level keyboard hook callback ─────────────────────────
    unsafe extern "system" fn keyboard_hook(
        code:   i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

        if code >= 0 {
            let msg_kind = wparam.0 as u32;
            if msg_kind == WM_KEYDOWN || msg_kind == WM_SYSKEYDOWN {
                let kb    = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let vk    = kb.vkCode;
                let ctrl  = (GetAsyncKeyState(0x11) as u16) & 0x8000 != 0; // VK_CONTROL
                let shift = (GetAsyncKeyState(0x10) as u16) & 0x8000 != 0; // VK_SHIFT
                let alt   = (GetAsyncKeyState(0x12) as u16) & 0x8000 != 0; // VK_MENU

                if let Ok(guard) = HOOK_DATA.try_lock() {
                    if let Some(data) = guard.as_ref() {
                        for hk in &data.hotkeys {
                            if hk.vk == vk
                                && hk.ctrl  == ctrl
                                && hk.shift == shift
                                && hk.alt   == alt
                            {
                                log::info!("[hotkeys] FIRED: {}", hk.event);
                                if let Some(win) = data.app.get_webview_window("main") {
                                    if let Err(e) = win.emit(&hk.event, ()) {
                                        log::error!("[hotkeys] emit failed: {e}");
                                    }
                                } else {
                                    let _ = data.app.emit(&hk.event, ());
                                }
                            }
                        }
                    }
                }
            }
        }

        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    // ── Install the hook ──────────────────────────────────────────────
    let hook = match unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), HINSTANCE::default(), 0)
    } {
        Ok(h)  => { log::info!("[hotkeys] WH_KEYBOARD_LL installed"); h }
        Err(e) => { log::error!("[hotkeys] Hook install failed: {e}"); HOOK_DATA.lock().unwrap().take(); return; }
    };

    // ── Watcher thread posts WM_QUIT when stop_flag is set ────────────
    let thread_id = unsafe { GetCurrentThreadId() };
    thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if stop_flag.load(Ordering::Relaxed) {
                unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).ok(); }
                break;
            }
        }
    });

    // ── Message loop (must stay on this thread) ───────────────────────
    // WH_KEYBOARD_LL requires an active message pump on the installing thread.
    // GetMessageW blocks until a message arrives — WM_QUIT breaks the loop.
    log::info!("[hotkeys] Message loop running");
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            // No DispatchMessage needed — LL hook callbacks fire directly
        }
        UnhookWindowsHookEx(hook).ok();
    }

    *HOOK_DATA.lock().unwrap() = None;
    log::info!("[hotkeys] Hook uninstalled");
}

// ── Parse "CommandOrControl+Shift+R" → (vk, ctrl, shift, alt) ────────────────

fn parse_hotkey_to_vk(s: &str) -> Option<(u32, bool, bool, bool)> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let key_str = parts.last()?.trim();
    let mod_parts = &parts[..parts.len() - 1];

    let mut ctrl  = false;
    let mut shift = false;
    let mut alt   = false;

    for m in mod_parts {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => ctrl  = true,
            "shift"                                               => shift = true,
            "alt"                                                 => alt   = true,
            "meta" | "super" | "command" | "cmd"                 => ctrl  = true,
            _ => {}
        }
    }

    // Strip web UI key name prefixes
    let ku = key_str.to_uppercase();
    let ku = ku.strip_prefix("KEY").unwrap_or(
             ku.strip_prefix("DIGIT").unwrap_or(&ku));

    let vk: u32 = match ku {
        // Letters — VK codes are the same as ASCII uppercase (0x41–0x5A)
        "A"=>0x41,"B"=>0x42,"C"=>0x43,"D"=>0x44,"E"=>0x45,"F"=>0x46,
        "G"=>0x47,"H"=>0x48,"I"=>0x49,"J"=>0x4A,"K"=>0x4B,"L"=>0x4C,
        "M"=>0x4D,"N"=>0x4E,"O"=>0x4F,"P"=>0x50,"Q"=>0x51,"R"=>0x52,
        "S"=>0x53,"T"=>0x54,"U"=>0x55,"V"=>0x56,"W"=>0x57,"X"=>0x58,
        "Y"=>0x59,"Z"=>0x5A,
        // Digits
        "0"=>0x30,"1"=>0x31,"2"=>0x32,"3"=>0x33,"4"=>0x34,
        "5"=>0x35,"6"=>0x36,"7"=>0x37,"8"=>0x38,"9"=>0x39,
        // Function keys
        "F1"=>0x70,"F2"=>0x71,"F3"=>0x72,"F4"=>0x73,
        "F5"=>0x74,"F6"=>0x75,"F7"=>0x76,"F8"=>0x77,
        "F9"=>0x78,"F10"=>0x79,"F11"=>0x7A,"F12"=>0x7B,
        // Navigation
        "SPACE"|" "          =>0x20,
        "RETURN"|"ENTER"     =>0x0D,
        "TAB"                =>0x09,
        "ESCAPE"|"ESC"       =>0x1B,
        "BACKSPACE"          =>0x08,
        "DELETE"|"DEL"       =>0x2E,
        "INSERT"|"INS"       =>0x2D,
        "HOME"               =>0x24,
        "END"                =>0x23,
        "PAGEUP"             =>0x21,
        "PAGEDOWN"           =>0x22,
        "ARROWLEFT"|"LEFT"   =>0x25,
        "ARROWRIGHT"|"RIGHT" =>0x27,
        "ARROWUP"|"UP"       =>0x26,
        "ARROWDOWN"|"DOWN"   =>0x28,
        // Numpad
        "NUMPAD0"|"NUM0"=>0x60,"NUMPAD1"|"NUM1"=>0x61,
        "NUMPAD2"|"NUM2"=>0x62,"NUMPAD3"|"NUM3"=>0x63,
        "NUMPAD4"|"NUM4"=>0x64,"NUMPAD5"|"NUM5"=>0x65,
        "NUMPAD6"|"NUM6"=>0x66,"NUMPAD7"|"NUM7"=>0x67,
        "NUMPAD8"|"NUM8"=>0x68,"NUMPAD9"|"NUM9"=>0x69,
        "NUMPADADD"|"NUMADD"        =>0x6B,
        "NUMPADSUBTRACT"|"NUMSUB"   =>0x6D,
        "NUMPADMULTIPLY"|"NUMMUL"   =>0x6A,
        "NUMPADDIVIDE"|"NUMDIV"     =>0x6F,
        "NUMPADENTER"|"NUMENTER"    =>0x0D,
        "NUMPADDECIMAL"|"NUMDECIMAL"=>0x6E,
        // Punctuation
        "SEMICOLON"|";"    =>0xBA,
        "COMMA"|","        =>0xBC,
        "PERIOD"|"."       =>0xBE,
        "SLASH"|"/"        =>0xBF,
        "BACKSLASH"|"\\"   =>0xDC,
        "QUOTE"|"'"        =>0xDE,
        "BACKTICK"|"BACKQUOTE"|"`"=>0xC0,
        "EQUAL"|"="        =>0xBB,
        "MINUS"|"-"        =>0xBD,
        "BRACKETLEFT"|"["  =>0xDB,
        "BRACKETRIGHT"|"]" =>0xDD,
        // Media
        "MEDIAPLAYPAUSE"     =>0xB3,
        "MEDIASTOP"          =>0xB2,
        "MEDIATRACKPREVIOUS" =>0xB1,
        "MEDIATRACKNEXT"     =>0xB0,
        "AUDIOVOLUMEMUTE"    =>0xAD,
        "AUDIOVOLUMEDOWN"    =>0xAE,
        "AUDIOVOLUMEUP"      =>0xAF,
        _ => return None,
    };

    Some((vk, ctrl, shift, alt))
}

#[cfg(test)]
#[path = "hotkey_listener_tests.rs"]
mod tests;
