//! Characterization tests for the hotkey string format the UI saves.
use super::parse_hotkey_to_vk;

#[test]
fn parses_ui_hotkey_strings() {
    // (vk, ctrl, shift, alt)
    assert_eq!(parse_hotkey_to_vk("CommandOrControl+Shift+F"), Some((0x46, true, true, false)));
    assert_eq!(parse_hotkey_to_vk("Alt+F9"), Some((0x78, false, false, true)));
    assert_eq!(parse_hotkey_to_vk("Ctrl+Shift+KeyR"), Some((0x52, true, true, false)));
    assert_eq!(parse_hotkey_to_vk("Ctrl+Digit1"), Some((0x31, true, false, false)));
}

#[test]
fn rejects_empty_and_dangling() {
    assert_eq!(parse_hotkey_to_vk(""), None);
    assert_eq!(parse_hotkey_to_vk("Shift+"), None);
    assert_eq!(parse_hotkey_to_vk("Ctrl+Nope"), None);
}

#[test]
fn meta_is_treated_as_ctrl_today() {
    // Current behaviour (quirk noted for WS1): Meta/Super/Cmd map to Ctrl.
    assert_eq!(parse_hotkey_to_vk("Meta+F"), Some((0x46, true, false, false)));
}
