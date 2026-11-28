use super::*;

fn c(s: &str) -> Chord {
    Chord::parse(s).unwrap_or_else(|e| panic!("parse {s:?}: {e}"))
}
fn key(s: &str) -> Input {
    *c(s).inputs().next().unwrap()
}
fn down(s: &str) -> RawEvent {
    RawEvent::Down(key(s))
}
fn up(s: &str) -> RawEvent {
    RawEvent::Up(key(s))
}

#[test]
fn legacy_strings_parse() {
    for s in [
        "Alt+F9",
        "CommandOrControl+Shift+F",
        "Ctrl+Digit1",
        "Ctrl+Shift+KeyR",
        "CommandOrControl+Shift+R",
        "CommandOrControl+Shift+S",
        "CommandOrControl+Shift+P",
        "CommandOrControl+Shift+L",
        "CommandOrControl+Shift+B",
        "CommandOrControl+Backquote",
        "CmdOrCtrl+Backtick",
        "Control+Space",
        "Ctrl+Return",
        "Ctrl+Numpad5",
        "Ctrl+NumpadAdd",
        "Ctrl+NumpadEnter",
        "Ctrl+ArrowLeft",
        "Ctrl+MediaPlayPause",
        "Ctrl+AudioVolumeUp",
        "Ctrl+[",
        "Ctrl+;",
        "Ctrl+BracketLeft",
        "Ctrl+Del",
        "Ctrl+PageDown",
    ] {
        let ch = c(s);
        let canon = ch.to_canonical();
        assert_eq!(c(&canon), ch, "round trip {s} -> {canon}");
        assert_eq!(c(&canon).to_canonical(), canon);
    }
    assert!(Chord::parse("Ctrl+Nope").is_err());
    assert!(Chord::parse("Shift+").is_err());
    assert!(Chord::parse("").is_err());
    assert_eq!(c("CommandOrControl+Shift+R").to_canonical(), "Ctrl+Shift+R");
    assert_eq!(c("Alt+Shift+Ctrl+Win+K").to_canonical(), "Ctrl+Shift+Alt+Win+K");
    assert_eq!(c("Ctrl+Backquote"), c("Control+`"));
}

#[test]
fn function_keys_to_f24_and_mouse_buttons_parse() {
    assert_eq!(c("Ctrl+F13").registerhotkey_form(), Some((MOD_CONTROL, 0x7C)));
    assert_eq!(c("F24").registerhotkey_form(), Some((0, 0x87)));
    assert_eq!(c("Mouse4").to_canonical(), "Mouse4");
    assert_eq!(c("Shift+Mouse5").to_canonical(), "Shift+Mouse5");
    assert_eq!(key("Mouse3"), Input::Mouse(MouseButton::Middle));
    assert_eq!(c("PrintScreen").to_canonical(), "PrintScreen");
    assert_eq!(c("Ctrl+VK_0xE8").to_canonical(), "Ctrl+VK_0xE8");
    assert_eq!(normalize_vk(0xA3), 0x11);
    assert_eq!(normalize_vk(0xA1), 0x10);
    assert_eq!(normalize_vk(0xA5), 0x12);
    assert_eq!(normalize_vk(0x5C), 0x5B);
    assert_eq!(normalize_vk(0x41), 0x41);
}

#[test]
fn meta_maps_to_win() {
    for s in ["Meta+F", "Super+F", "Cmd+F", "Command+F"] {
        assert_eq!(c(s).to_canonical(), "Win+F");
    }
    assert_eq!(c("Win+F").registerhotkey_form(), Some((MOD_WIN, 0x46)));
}

#[test]
fn display_is_human() {
    assert_eq!(c("Ctrl+Backquote").display(), "Ctrl + `");
    assert_eq!(c("Mouse4").display(), "Mouse 4");
    assert_eq!(c("Ctrl+Shift+F2").display(), "Ctrl + Shift + F2");
}

#[test]
fn fires_with_extra_held_keys() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2"))]);
    assert_eq!(m.on_event(down("Shift")), None);
    assert_eq!(m.on_event(down("W")), None);
    assert_eq!(m.on_event(down("Ctrl")), None);
    assert_eq!(m.on_event(down("F2")), Some("a".into()));
}

#[test]
fn most_specific_wins() {
    let mut m = Matcher::new(vec![
        ("plain".into(), c("Ctrl+F2")),
        ("shifted".into(), c("Ctrl+Shift+F2")),
    ]);
    m.on_event(down("Ctrl"));
    m.on_event(down("Shift"));
    assert_eq!(m.on_event(down("F2")), Some("shifted".into()));
    assert_eq!(m.on_event(up("F2")), None);
}

#[test]
fn repeat_fires_once() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2"))]);
    m.on_event(down("Ctrl"));
    let n = (0..3).filter_map(|_| m.on_event(down("F2"))).count();
    assert_eq!(n, 1);
    m.on_event(up("F2"));
    assert_eq!(m.on_event(down("F2")), Some("a".into()));
}

#[test]
fn modifier_only_fires_on_clean_release() {
    let mut m = Matcher::new(vec![("mo".into(), c("Ctrl+Alt"))]);
    m.on_event(down("Ctrl"));
    assert_eq!(m.on_event(down("Alt")), None);
    assert_eq!(m.on_event(up("Alt")), Some("mo".into()));
    assert_eq!(m.on_event(up("Ctrl")), None);

    m.on_event(down("Ctrl"));
    m.on_event(down("Alt"));
    m.on_event(down("K"));
    assert_eq!(m.on_event(up("K")), None);
    assert_eq!(m.on_event(up("Alt")), None);
    assert_eq!(m.on_event(up("Ctrl")), None);
}

#[test]
fn mouse_chord_fires() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+Mouse4"))]);
    m.on_event(down("Ctrl"));
    assert_eq!(m.on_event(down("Mouse4")), Some("a".into()));
}

#[test]
fn conflicts_detected() {
    let b = vec![
        ("a".to_string(), c("Ctrl+Shift+R")),
        ("b".to_string(), c("CommandOrControl+Shift+R")),
        ("c".to_string(), c("F2")),
    ];
    assert_eq!(find_conflicts(&b), vec![("a".to_string(), "b".to_string())]);
    assert!(find_conflicts(&b[2..]).is_empty());
}

#[test]
fn capture_records_union_on_full_release() {
    let mut cap = Capture::default();
    assert_eq!(cap.on_event(down("Ctrl")), CaptureState::Holding(c("Ctrl")));
    assert!(matches!(cap.on_event(down("Mouse4")), CaptureState::Holding(_)));
    assert_eq!(cap.on_event(up("Mouse4")), CaptureState::Holding(c("Ctrl+Mouse4")));
    assert_eq!(cap.on_event(up("Ctrl")), CaptureState::Done(c("Ctrl+Mouse4")));
    assert_eq!(cap.on_event(up("Ctrl")), CaptureState::Idle);
}

#[test]
fn registerhotkey_form() {
    assert_eq!(
        c("Ctrl+Shift+F2").registerhotkey_form(),
        Some((MOD_CONTROL | MOD_SHIFT, 0x71))
    );
    assert_eq!(c("Mouse4").registerhotkey_form(), None);
    assert_eq!(c("Ctrl+Alt").registerhotkey_form(), None);
    assert_eq!(c("Ctrl+A+B").registerhotkey_form(), None);
    assert!(c("Ctrl+Alt").is_modifier_only());
    assert!(!c("Ctrl+F2").is_modifier_only());
}

fn vk(v: u32) -> Input {
    Input::Key(v)
}

#[test]
fn lr_modifiers_tracked_independently() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2"))]);
    assert_eq!(m.on_event(RawEvent::Down(vk(VK_LCONTROL))), None);
    assert_eq!(m.on_event(RawEvent::Down(vk(VK_RCONTROL))), None);
    assert_eq!(m.on_event(RawEvent::Up(vk(VK_LCONTROL))), None);
    // Right Ctrl is still held, so Ctrl counts as held.
    assert_eq!(m.on_event(down("F2")), Some("a".into()));
    m.on_event(up("F2"));
    m.on_event(RawEvent::Up(vk(VK_RCONTROL)));
    // Now nothing is held: F2 alone must not fire.
    assert_eq!(m.on_event(down("F2")), None);
}

#[test]
fn lr_modifier_only_fires_once_on_last_release() {
    let mut m = Matcher::new(vec![("mo".into(), c("Ctrl+Alt"))]);
    m.on_event(RawEvent::Down(vk(VK_LCONTROL)));
    m.on_event(RawEvent::Down(vk(VK_LMENU)));
    m.on_event(RawEvent::Down(vk(VK_RCONTROL)));
    // Releasing one Ctrl while the other is held is not a release of Ctrl.
    assert_eq!(m.on_event(RawEvent::Up(vk(VK_RCONTROL))), None);
    assert_eq!(m.on_event(RawEvent::Up(vk(VK_LMENU))), Some("mo".into()));
}

#[test]
fn capture_tracks_lr_modifiers() {
    let mut cap = Capture::default();
    cap.on_event(RawEvent::Down(vk(VK_LSHIFT)));
    cap.on_event(RawEvent::Down(vk(VK_RSHIFT)));
    assert_eq!(cap.on_event(RawEvent::Up(vk(VK_LSHIFT))), CaptureState::Holding(c("Shift")));
    assert_eq!(cap.on_event(RawEvent::Up(vk(VK_RSHIFT))), CaptureState::Done(c("Shift")));
}

#[test]
fn modifier_only_not_fired_after_click() {
    let mut m = Matcher::new(vec![("mo".into(), c("Ctrl+Alt")), ("ctrl".into(), c("Ctrl"))]);
    m.on_event(down("Ctrl"));
    m.on_event(down("Alt"));
    assert_eq!(m.on_event(RawEvent::Other), None);
    assert_eq!(m.on_event(up("Alt")), None);
    assert_eq!(m.on_event(up("Ctrl")), None);
    // A clean press afterwards still works.
    m.on_event(down("Ctrl"));
    assert_eq!(m.on_event(up("Ctrl")), Some("ctrl".into()));
}

#[test]
fn other_does_not_disturb_normal_chords_or_capture() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2"))]);
    m.on_event(down("Ctrl"));
    m.on_event(RawEvent::Other);
    assert_eq!(m.on_event(down("F2")), Some("a".into()));

    let mut cap = Capture::default();
    assert_eq!(cap.on_event(RawEvent::Other), CaptureState::Idle);
    cap.on_event(down("Ctrl"));
    assert_eq!(cap.on_event(RawEvent::Other), CaptureState::Holding(c("Ctrl")));
}

#[test]
fn matcher_reset_clears_held_state() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2"))]);
    m.on_event(down("Ctrl"));
    m.on_event(down("F2"));
    m.reset();
    // Ctrl's up was lost (e.g. hook reinstall): F2 alone must not fire now.
    assert_eq!(m.on_event(down("F2")), None);
    m.on_event(up("F2"));
    m.on_event(down("Ctrl"));
    assert_eq!(m.on_event(down("F2")), Some("a".into()));
}

#[test]
fn capture_reset_clears_state() {
    let mut cap = Capture::default();
    cap.on_event(down("Ctrl"));
    cap.reset();
    assert_eq!(cap.on_event(down("F2")), CaptureState::Holding(c("F2")));
    assert_eq!(cap.on_event(up("F2")), CaptureState::Done(c("F2")));
}

#[test]
fn parse_bindings_skips_bad_and_empty() {
    let input = vec![
        ("start".to_string(), "Ctrl+F2".to_string()),
        ("stop".to_string(), "Ctrl+Nope".to_string()),
        ("pause".to_string(), "".to_string()),
        ("save".to_string(), "Mouse4".to_string()),
    ];
    let (ok, errors) = parse_bindings(input);
    let names: Vec<&str> = ok.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(names, ["save", "start"]); // sorted by action
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "stop");
    assert!(errors[0].1.contains("Nope"), "{:?}", errors[0]);
}

#[test]
fn reset_held_resyncs_without_firing() {
    let mut m = Matcher::new(vec![("a".into(), c("Ctrl+F2")), ("mo".into(), c("Ctrl+Alt"))]);
    // After a hook reinstall the OS reports LCtrl + F2 held: no fire now...
    m.reset_held([vk(VK_LCONTROL), key("F2")]);
    // ...and not on repeats of the held chord.
    assert_eq!(m.on_event(down("F2")), None);
    m.on_event(up("F2"));
    // Ctrl is still held: pressing F2 again fires.
    assert_eq!(m.on_event(down("F2")), Some("a".into()));
    // A modifier-only chord completed only by resync is not armed.
    let mut m = Matcher::new(vec![("mo".into(), c("Ctrl+Alt"))]);
    m.reset_held([vk(VK_LCONTROL), vk(VK_LMENU)]);
    assert_eq!(m.on_event(RawEvent::Up(vk(VK_LMENU))), None);
}

#[test]
fn modifier_only_not_fired_after_longer_chord_any_order() {
    let orders: [&[&str]; 3] = [&["Ctrl", "Alt", "K"], &["Ctrl", "K", "Alt"], &["K", "Ctrl", "Alt"]];
    for order in orders {
        let mut m = Matcher::new(vec![("mo".into(), c("Ctrl+Alt")), ("long".into(), c("Ctrl+Alt+K"))]);
        let fired: Vec<String> = order.iter().filter_map(|k| m.on_event(down(k))).collect();
        assert_eq!(fired, ["long"], "{order:?}");
        for k in order.iter().rev() {
            assert_eq!(m.on_event(up(k)), None, "{order:?}: release {k}");
        }
        // A clean Ctrl+Alt tap afterwards still fires.
        m.on_event(down("Ctrl"));
        m.on_event(down("Alt"));
        assert_eq!(m.on_event(up("Alt")), Some("mo".into()), "{order:?}: clean tap");
        m.on_event(up("Ctrl"));
    }
}

#[test]
fn web_codes_map_to_inputs() {
    let k = |code: &str| Input::from_web_code(code);
    assert_eq!(k("KeyA"), Some(Input::Key(0x41)));
    assert_eq!(k("KeyZ"), Some(Input::Key(0x5A)));
    assert_eq!(k("Digit0"), Some(Input::Key(0x30)));
    assert_eq!(k("Digit9"), Some(Input::Key(0x39)));
    assert_eq!(k("F1"), Some(Input::Key(0x70)));
    assert_eq!(k("F12"), Some(Input::Key(0x7B)));
    assert_eq!(k("F24"), Some(Input::Key(0x87)));
    assert_eq!(k("Numpad0"), Some(Input::Key(0x60)));
    assert_eq!(k("Numpad9"), Some(Input::Key(0x69)));
    assert_eq!(k("NumpadAdd"), Some(Input::Key(0x6B)));
    assert_eq!(k("NumpadDecimal"), Some(Input::Key(0x6E)));
    assert_eq!(k("NumpadEnter"), Some(Input::Key(0x0D)));
    assert_eq!(k("Backquote"), Some(Input::Key(0xC0)));
    assert_eq!(k("Minus"), Some(Input::Key(0xBD)));
    assert_eq!(k("Equal"), Some(Input::Key(0xBB)));
    assert_eq!(k("BracketLeft"), Some(Input::Key(0xDB)));
    assert_eq!(k("Semicolon"), Some(Input::Key(0xBA)));
    assert_eq!(k("Quote"), Some(Input::Key(0xDE)));
    assert_eq!(k("Slash"), Some(Input::Key(0xBF)));
    assert_eq!(k("IntlBackslash"), Some(Input::Key(0xE2)));
    assert_eq!(k("Space"), Some(Input::Key(0x20)));
    assert_eq!(k("Escape"), Some(Input::Key(0x1B)));
    assert_eq!(k("ArrowUp"), Some(Input::Key(0x26)));
    assert_eq!(k("PageDown"), Some(Input::Key(0x22)));
    assert_eq!(k("PrintScreen"), Some(Input::Key(0x2C)));
    assert_eq!(k("ContextMenu"), Some(Input::Key(0x5D)));
    assert_eq!(k("MediaPlayPause"), Some(Input::Key(0xB3)));
    assert_eq!(k("AudioVolumeUp"), Some(Input::Key(0xAF)));
    assert_eq!(k("ControlLeft"), Some(Input::Key(VK_LCONTROL)));
    assert_eq!(k("ControlRight"), Some(Input::Key(VK_RCONTROL)));
    assert_eq!(k("ShiftLeft"), Some(Input::Key(VK_LSHIFT)));
    assert_eq!(k("ShiftRight"), Some(Input::Key(VK_RSHIFT)));
    assert_eq!(k("AltLeft"), Some(Input::Key(VK_LMENU)));
    assert_eq!(k("AltRight"), Some(Input::Key(VK_RMENU)));
    assert_eq!(k("MetaLeft"), Some(Input::Key(VK_LWIN)));
    assert_eq!(k("MetaRight"), Some(Input::Key(VK_RWIN)));
    assert_eq!(k("OSLeft"), Some(Input::Key(VK_LWIN)));
    for bad in ["", "keya", "Key", "KeyAB", "Digit", "F0", "F25", "F01", "Numpad10", "Unidentified", "Ctrl", "Mouse4", "Menu"] {
        assert_eq!(k(bad), None, "{bad:?}");
    }
    // Chords built from web codes match the hook's canonical form.
    let chord = Chord::from_inputs(["ControlRight", "AltLeft", "F12"].iter().filter_map(|c| Input::from_web_code(c)));
    assert_eq!(chord.to_canonical(), "Ctrl+Alt+F12");
    assert_eq!(chord, c("CommandOrControl+Alt+F12"));
}

#[test]
fn registerable_needs_ctrl_alt_or_win() {
    for s in ["Ctrl+F2", "Alt+F9", "Win+K", "Ctrl+Shift+A", "Ctrl+Alt+F12", "CommandOrControl+Backquote", "CommandOrControl+1"] {
        assert!(registerable(&c(s)), "{s}");
    }
    for s in ["F2", "Shift+F2", "K", "Mouse4", "Ctrl+Mouse4", "Ctrl+Alt", "Ctrl+A+B", "Shift"] {
        assert!(!registerable(&c(s)), "{s}");
    }
}
