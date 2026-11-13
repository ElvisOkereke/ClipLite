//! Synthetic keyboard input (SendInput) for hotkey trials.
use anyhow::{anyhow, Result};

pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_F: u16 = 0x46;
pub const VK_F24: u16 = 0x87;

/// Parse "Ctrl+Alt+Shift+F11" into virtual-key codes (modifiers first, in order).
pub fn parse_chord(chord: &str) -> Result<Vec<u16>> {
    let mut keys = Vec::new();
    for part in chord.split('+').map(str::trim) {
        let up = part.to_ascii_uppercase();
        let vk = match up.as_str() {
            "CTRL" | "CONTROL" | "COMMANDORCONTROL" => VK_CONTROL,
            "SHIFT" => VK_SHIFT,
            "ALT" => VK_MENU,
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k.as_bytes()[0] as u16,
            k if k.starts_with('F') && k.len() > 1 => {
                let n: u16 = k[1..].parse().map_err(|_| anyhow!("bad key '{part}'"))?;
                if !(1..=24).contains(&n) {
                    return Err(anyhow!("bad function key '{part}'"));
                }
                0x70 + n - 1
            }
            _ => return Err(anyhow!("unsupported key '{part}' in chord '{chord}'")),
        };
        keys.push(vk);
    }
    if keys.is_empty() {
        return Err(anyhow!("empty chord"));
    }
    Ok(keys)
}

/// Press all keys in order, then release in reverse order.
#[cfg(windows)]
pub fn send_keys(keys: &[u16]) -> Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let ev = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    };
    let down: Vec<INPUT> = keys.iter().map(|k| ev(*k, false)).collect();
    let up: Vec<INPUT> = keys.iter().rev().map(|k| ev(*k, true)).collect();
    unsafe {
        let n = SendInput(&down, std::mem::size_of::<INPUT>() as i32);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let m = SendInput(&up, std::mem::size_of::<INPUT>() as i32);
        if n as usize != down.len() || m as usize != up.len() {
            return Err(anyhow!("SendInput blocked (UIPI?): sent {n}/{} down, {m}/{} up", down.len(), up.len()));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn send_chord(chord: &str) -> Result<()> {
    send_keys(&parse_chord(chord)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_parse() {
        assert_eq!(parse_chord("Ctrl+Shift+F").unwrap(), vec![VK_CONTROL, VK_SHIFT, VK_F]);
        assert_eq!(parse_chord("Ctrl+Alt+Shift+F11").unwrap(), vec![VK_CONTROL, VK_MENU, VK_SHIFT, 0x7A]);
        assert_eq!(parse_chord("F24").unwrap(), vec![VK_F24]);
    }

    #[test]
    fn chord_rejects_garbage() {
        assert!(parse_chord("Ctrl+Banana").is_err());
        assert!(parse_chord("F25").is_err());
    }
}
