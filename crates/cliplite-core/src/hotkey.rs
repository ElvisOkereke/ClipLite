//! Hotkey chord model and matcher.
//!
//! A [`Chord`] is a set of keyboard keys and/or extra mouse buttons. The
//! [`Matcher`] consumes raw down/up events (from a low-level hook) and reports
//! which bound action fired. Pure logic: no Win32 calls live here.
use std::cmp::Ordering;
use std::collections::BTreeSet;

pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_LMENU: u32 = 0xA4;
pub const VK_RMENU: u32 = 0xA5;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MouseButton {
    Middle,
    X1,
    X2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Input {
    /// Keyboard key by virtual-key code (modifiers normalised, see [`normalize_vk`]).
    Key(u32),
    Mouse(MouseButton),
}

/// Modifier order in canonical strings: Ctrl, Shift, Alt, Win.
const MODIFIER_ORDER: [u32; 4] = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN];

fn modifier_rank(vk: u32) -> Option<usize> {
    MODIFIER_ORDER.iter().position(|&m| m == vk)
}

impl Input {
    fn sort_key(&self) -> (u8, u32) {
        match *self {
            Input::Key(vk) => match modifier_rank(vk) {
                Some(r) => (0, r as u32),
                None => (1, vk),
            },
            Input::Mouse(b) => (2, b as u32),
        }
    }

    pub fn is_modifier(&self) -> bool {
        matches!(self, Input::Key(vk) if modifier_rank(*vk).is_some())
    }

    fn normalized(self) -> Input {
        match self {
            Input::Key(vk) => Input::Key(normalize_vk(vk)),
            m => m,
        }
    }
}

impl Ord for Input {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}
impl PartialOrd for Input {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Collapse left/right modifier variants onto the generic VK.
pub fn normalize_vk(vk: u32) -> u32 {
    match vk {
        VK_LCONTROL | VK_RCONTROL => VK_CONTROL,
        VK_LSHIFT | VK_RSHIFT => VK_SHIFT,
        VK_LMENU | VK_RMENU => VK_MENU,
        VK_RWIN => VK_LWIN,
        v => v,
    }
}

/// (canonical name, vk, display label). Display `None` means same as name.
const KEYS: &[(&str, u32, Option<&str>)] = &[
    ("Space", 0x20, None),
    ("Enter", 0x0D, None),
    ("Tab", 0x09, None),
    ("Escape", 0x1B, None),
    ("Backspace", 0x08, None),
    ("Delete", 0x2E, None),
    ("Insert", 0x2D, None),
    ("Home", 0x24, None),
    ("End", 0x23, None),
    ("PageUp", 0x21, None),
    ("PageDown", 0x22, None),
    ("ArrowLeft", 0x25, None),
    ("ArrowUp", 0x26, None),
    ("ArrowRight", 0x27, None),
    ("ArrowDown", 0x28, None),
    ("NumpadAdd", 0x6B, None),
    ("NumpadSubtract", 0x6D, None),
    ("NumpadMultiply", 0x6A, None),
    ("NumpadDivide", 0x6F, None),
    ("NumpadDecimal", 0x6E, None),
    ("Semicolon", 0xBA, Some(";")),
    ("Comma", 0xBC, Some(",")),
    ("Period", 0xBE, Some(".")),
    ("Slash", 0xBF, Some("/")),
    ("Backslash", 0xDC, Some("\\")),
    ("Quote", 0xDE, Some("'")),
    ("Backquote", 0xC0, Some("`")),
    ("Equal", 0xBB, Some("=")),
    ("Minus", 0xBD, Some("-")),
    ("BracketLeft", 0xDB, Some("[")),
    ("BracketRight", 0xDD, Some("]")),
    ("MediaPlayPause", 0xB3, None),
    ("MediaStop", 0xB2, None),
    ("MediaTrackPrevious", 0xB1, None),
    ("MediaTrackNext", 0xB0, None),
    ("AudioVolumeMute", 0xAD, None),
    ("AudioVolumeDown", 0xAE, None),
    ("AudioVolumeUp", 0xAF, None),
    ("PrintScreen", 0x2C, None),
    ("Pause", 0x13, None),
    ("ScrollLock", 0x91, None),
    ("CapsLock", 0x14, None),
    ("NumLock", 0x90, None),
    ("Menu", 0x5D, None),
    ("BrowserBack", 0xA6, None),
    ("BrowserForward", 0xA7, None),
    ("BrowserRefresh", 0xA8, None),
    ("BrowserStop", 0xA9, None),
    ("BrowserSearch", 0xAA, None),
    ("BrowserFavorites", 0xAB, None),
    ("BrowserHome", 0xAC, None),
];

/// Extra accepted spellings (compared upper-case) -> vk.
const ALIASES: &[(&str, u32)] = &[
    ("RETURN", 0x0D),
    ("NUMPADENTER", 0x0D),
    ("NUMENTER", 0x0D),
    ("ESC", 0x1B),
    ("DEL", 0x2E),
    ("INS", 0x2D),
    ("LEFT", 0x25),
    ("RIGHT", 0x27),
    ("UP", 0x26),
    ("DOWN", 0x28),
    ("NUMADD", 0x6B),
    ("NUMSUB", 0x6D),
    ("NUMMUL", 0x6A),
    ("NUMDIV", 0x6F),
    ("NUMDECIMAL", 0x6E),
    (";", 0xBA),
    (",", 0xBC),
    (".", 0xBE),
    ("/", 0xBF),
    ("\\", 0xDC),
    ("'", 0xDE),
    ("`", 0xC0),
    ("BACKTICK", 0xC0),
    ("=", 0xBB),
    ("-", 0xBD),
    ("[", 0xDB),
    ("]", 0xDD),
    ("APPS", 0x5D),
    ("BREAK", 0x13),
];

fn modifier_from_name(upper: &str) -> Option<u32> {
    match upper {
        "CTRL" | "CONTROL" | "COMMANDORCONTROL" | "CMDORCTRL" => Some(VK_CONTROL),
        "SHIFT" => Some(VK_SHIFT),
        "ALT" => Some(VK_MENU),
        "WIN" | "WINDOWS" | "META" | "SUPER" | "COMMAND" | "CMD" => Some(VK_LWIN),
        _ => None,
    }
}

fn modifier_name(vk: u32) -> &'static str {
    match vk {
        VK_CONTROL => "Ctrl",
        VK_SHIFT => "Shift",
        VK_MENU => "Alt",
        _ => "Win",
    }
}

fn parse_plain_key(upper: &str) -> Option<u32> {
    let b = upper.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(b[0] as u32);
    }
    if let Some(d) = upper.strip_prefix('F') {
        if d.bytes().all(|c| c.is_ascii_digit()) && !d.is_empty() && !d.starts_with('0') {
            if let Ok(n) = d.parse::<u32>() {
                if (1..=24).contains(&n) {
                    return Some(0x6F + n);
                }
            }
        }
    }
    if let Some(n) = upper.strip_prefix("NUMPAD").or_else(|| upper.strip_prefix("NUM")) {
        if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() {
            return Some(0x60 + (n.as_bytes()[0] - b'0') as u32);
        }
    }
    if let Some(h) = upper.strip_prefix("VK_0X") {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            if (1..=0xFF).contains(&v) {
                return Some(v);
            }
        }
    }
    KEYS.iter()
        .find(|(n, _, _)| n.to_ascii_uppercase() == upper)
        .map(|k| k.1)
        .or_else(|| ALIASES.iter().find(|(a, _)| *a == upper).map(|a| a.1))
}

fn parse_token(tok: &str) -> Result<Input, String> {
    let upper = tok.trim().to_uppercase();
    if upper.is_empty() {
        return Err("empty key name".into());
    }
    match upper.as_str() {
        "MOUSE3" => return Ok(Input::Mouse(MouseButton::Middle)),
        "MOUSE4" => return Ok(Input::Mouse(MouseButton::X1)),
        "MOUSE5" => return Ok(Input::Mouse(MouseButton::X2)),
        _ => {}
    }
    if let Some(m) = modifier_from_name(&upper) {
        return Ok(Input::Key(m));
    }
    let found = parse_plain_key(&upper).or_else(|| {
        // Web UI prefixes: KeyR, Digit1.
        let rest = upper
            .strip_prefix("KEY")
            .or_else(|| upper.strip_prefix("DIGIT"))?;
        parse_plain_key(rest)
    });
    found
        .map(|vk| Input::Key(normalize_vk(vk)))
        .ok_or_else(|| format!("unknown key: {}", tok.trim()))
}

fn key_name(vk: u32) -> String {
    if let Some(r) = modifier_rank(vk) {
        return modifier_name(MODIFIER_ORDER[r]).to_string();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        _ => KEYS
            .iter()
            .find(|k| k.1 == vk)
            .map(|k| k.0.to_string())
            .unwrap_or_else(|| format!("VK_0x{vk:02X}")),
    }
}

/// A set of inputs that must be held together.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Chord {
    keys: BTreeSet<Input>,
}

impl Chord {
    pub fn parse(s: &str) -> Result<Chord, String> {
        let mut keys = BTreeSet::new();
        for tok in s.split('+') {
            keys.insert(parse_token(tok)?);
        }
        Ok(Chord { keys })
    }

    pub fn from_inputs(inputs: impl IntoIterator<Item = Input>) -> Chord {
        Chord {
            keys: inputs.into_iter().map(Input::normalized).collect(),
        }
    }

    /// Inputs in canonical order (modifiers, keys by VK, mouse).
    pub fn inputs(&self) -> impl Iterator<Item = &Input> {
        self.keys.iter()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn to_canonical(&self) -> String {
        self.join(
            |i| match *i {
                Input::Key(vk) => key_name(vk),
                Input::Mouse(MouseButton::Middle) => "Mouse3".to_string(),
                Input::Mouse(MouseButton::X1) => "Mouse4".to_string(),
                Input::Mouse(MouseButton::X2) => "Mouse5".to_string(),
            },
            "+",
        )
    }

    pub fn display(&self) -> String {
        self.join(
            |i| match *i {
                Input::Key(vk) => KEYS
                    .iter()
                    .find(|k| k.1 == vk)
                    .and_then(|k| k.2.map(str::to_string))
                    .unwrap_or_else(|| key_name(vk)),
                Input::Mouse(MouseButton::Middle) => "Mouse 3".to_string(),
                Input::Mouse(MouseButton::X1) => "Mouse 4".to_string(),
                Input::Mouse(MouseButton::X2) => "Mouse 5".to_string(),
            },
            " + ",
        )
    }

    fn join(&self, f: impl Fn(&Input) -> String, sep: &str) -> String {
        self.keys.iter().map(f).collect::<Vec<_>>().join(sep)
    }

    pub fn is_modifier_only(&self) -> bool {
        !self.keys.is_empty() && self.keys.iter().all(Input::is_modifier)
    }

    /// `(MOD_* flags, vk)` when expressible via `RegisterHotKey`: zero or more
    /// modifiers plus exactly one non-modifier keyboard key. `MOD_NOREPEAT` is
    /// left to the caller.
    pub fn registerhotkey_form(&self) -> Option<(u32, u32)> {
        let mut mods = 0;
        let mut main = None;
        for i in &self.keys {
            match *i {
                Input::Mouse(_) => return None,
                Input::Key(VK_CONTROL) => mods |= MOD_CONTROL,
                Input::Key(VK_SHIFT) => mods |= MOD_SHIFT,
                Input::Key(VK_MENU) => mods |= MOD_ALT,
                Input::Key(VK_LWIN) => mods |= MOD_WIN,
                Input::Key(vk) => {
                    if main.replace(vk).is_some() {
                        return None;
                    }
                }
            }
        }
        main.map(|vk| (mods, vk))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEvent {
    Down(Input),
    Up(Input),
}

struct Binding {
    action: String,
    chord: Chord,
    /// Non-modifier-only: already fired this hold. Modifier-only: armed for release.
    flag: bool,
}

/// Turns raw input events into fired actions.
pub struct Matcher {
    bindings: Vec<Binding>,
    held: BTreeSet<Input>,
}

impl Matcher {
    pub fn new(bindings: Vec<(String, Chord)>) -> Matcher {
        Matcher {
            bindings: bindings
                .into_iter()
                .filter(|(_, c)| !c.is_empty())
                .map(|(action, chord)| Binding {
                    action,
                    chord,
                    flag: false,
                })
                .collect(),
            held: BTreeSet::new(),
        }
    }

    pub fn on_event(&mut self, ev: RawEvent) -> Option<String> {
        match ev {
            RawEvent::Down(i) => self.down(i.normalized()),
            RawEvent::Up(i) => self.up(i.normalized()),
        }
    }

    fn down(&mut self, i: Input) -> Option<String> {
        if !self.held.insert(i) {
            return None; // key repeat
        }
        let mut best: Option<(usize, usize)> = None; // (len, idx)
        for (idx, b) in self.bindings.iter_mut().enumerate() {
            let full = b.chord.keys.is_subset(&self.held);
            if b.chord.is_modifier_only() {
                if full {
                    // Armed only if this down completed the chord; a foreign
                    // key going down while fully held disarms it.
                    b.flag = b.chord.keys.contains(&i);
                }
            } else if full && !b.flag && b.chord.keys.contains(&i) {
                b.flag = true;
                let len = b.chord.len();
                if best.map_or(true, |(l, _)| len > l) {
                    best = Some((len, idx));
                }
            }
        }
        best.map(|(_, idx)| self.bindings[idx].action.clone())
    }

    fn up(&mut self, i: Input) -> Option<String> {
        if !self.held.remove(&i) {
            return None;
        }
        let mut best: Option<(usize, usize)> = None;
        for (idx, b) in self.bindings.iter_mut().enumerate() {
            if !b.chord.keys.contains(&i) {
                continue;
            }
            let was_armed = std::mem::take(&mut b.flag);
            if b.chord.is_modifier_only() && was_armed {
                let len = b.chord.len();
                if best.map_or(true, |(l, _)| len > l) {
                    best = Some((len, idx));
                }
            }
        }
        best.map(|(_, idx)| self.bindings[idx].action.clone())
    }
}

/// Pairs of actions bound to identical chords.
pub fn find_conflicts(bindings: &[(String, Chord)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (n, (a, ca)) in bindings.iter().enumerate() {
        for (b, cb) in &bindings[n + 1..] {
            if ca == cb {
                out.push((a.clone(), b.clone()));
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureState {
    Idle,
    Holding(Chord),
    Done(Chord),
}

/// Records a chord for the settings UI: the union of everything held, reported
/// once every input has been released.
#[derive(Debug, Default)]
pub struct Capture {
    held: BTreeSet<Input>,
    union: BTreeSet<Input>,
}

impl Capture {
    pub fn on_event(&mut self, ev: RawEvent) -> CaptureState {
        match ev {
            RawEvent::Down(i) => {
                let i = i.normalized();
                self.held.insert(i);
                self.union.insert(i);
            }
            RawEvent::Up(i) => {
                self.held.remove(&i.normalized());
            }
        }
        if self.union.is_empty() {
            return CaptureState::Idle;
        }
        let chord = Chord {
            keys: self.union.clone(),
        };
        if self.held.is_empty() {
            self.union.clear();
            CaptureState::Done(chord)
        } else {
            CaptureState::Holding(chord)
        }
    }
}

#[cfg(test)]
#[path = "hotkey_tests.rs"]
mod tests;
