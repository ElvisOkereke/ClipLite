use super::*;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// In-memory writer so a test can read back what the JSON layer produced.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn stage_line_is_flat_json() {
    let buf = Buf::default();
    tracing::subscriber::with_default(json_subscriber(buf.clone()), || {
        tracing::info!(stage = Stage::ReplaySaved.as_str(), cid = "c1", path = "p", "saved");
    });
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["stage"], "replay.saved");
    assert_eq!(v["cid"], "c1");
    assert_eq!(v["path"], "p");
    assert_eq!(v["level"], "INFO");
    assert!(v["timestamp"].is_string());
}

#[test]
fn stage_names_are_stable() {
    let all = [
        (Stage::HotkeyPressed, "hotkey.pressed"),
        (Stage::HotkeyDispatched, "hotkey.dispatched"),
        (Stage::CommandReceived, "command.received"),
        (Stage::IpcSent, "ipc.sent"),
        (Stage::RecorderCommand, "recorder.command"),
        (Stage::FfmpegSpawned, "ffmpeg.spawned"),
        (Stage::FfmpegExited, "ffmpeg.exited"),
        (Stage::ReplaySaved, "replay.saved"),
        (Stage::Error, "error"),
    ];
    for (stage, name) in all {
        assert_eq!(stage.as_str(), name);
    }
}

#[test]
fn new_cid_is_32_hex_and_unique() {
    let a = new_cid();
    let b = new_cid();
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}
