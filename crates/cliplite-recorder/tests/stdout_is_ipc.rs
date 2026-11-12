//! The recorder's stdout is the IPC channel back to the UI process: every
//! line must be a JSON `Event`. A stray print or log line would corrupt it.
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn every_stdout_line_is_an_event() {
    let home = std::env::temp_dir().join("clip lite ipc test");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cliplite-recorder"))
        .env("CLIPLITE_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn recorder");

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"cmd":"status"}}"#).unwrap();
        writeln!(stdin, r#"{{"cmd":"bogus"}}"#).unwrap();
        writeln!(stdin, r#"{{"cmd":"save_replay","secs":5,"output_path":"C:\\x y\\z.mp4"}}"#).unwrap();
    }
    drop(child.stdin.take()); // EOF ends the recorder's command loop

    let out = child.wait_with_output().expect("recorder exits on EOF");
    let text = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines.len(), 3, "one reply per command, nothing else:\n{text}");
    for line in &lines {
        serde_json::from_str::<cliplite_core::ipc::Event>(line)
            .unwrap_or_else(|e| panic!("not an Event ({e}): {line}"));
    }
    assert!(lines[0].contains(r#""event":"status""#));
    assert!(lines[1].contains(r#""event":"error""#), "unknown command reports an error event");
    assert!(lines[2].contains(r#""event":"error""#), "save without an active buffer reports an error event");
}
