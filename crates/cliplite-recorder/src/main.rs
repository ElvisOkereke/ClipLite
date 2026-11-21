//! cliplite-recorder: the standalone capture process.
//!
//! IPC (newline-delimited JSON, `cliplite_core::ipc`): commands on stdin,
//! events on stdout. stdout carries nothing else. It drives one capture
//! `Engine`; every request gets exactly one response event with its cid
//! (`EngineRestarted` is the only unsolicited event, apart from a recording
//! finalized by `StopEngine`). EOF on stdin stops the engine and exits.
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;

mod engine;
mod recorder;
mod recording;

use cliplite_core::ipc::{Command, Envelope, Event};
use cliplite_core::ring::RingLimits;
use cliplite_core::telemetry::{new_cid, Stage};
use engine::{Emit, Engine};

/// Write one event to stdout (the IPC channel), tagged with the command's cid.
fn emit(event: Event, cid: &str) {
    if let Event::Error { message } = &event {
        tracing::error!(stage = Stage::Error.as_str(), cid = %cid, %message, "recorder error");
    }
    let env = Envelope { body: event, cid: Some(cid.to_string()) };
    let json = serde_json::to_string(&env).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{json}");
    let _ = out.flush();
}

fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::StartEngine { .. } => "start_engine",
        Command::StopEngine => "stop_engine",
        Command::SetLimits { .. } => "set_limits",
        Command::SaveReplay { .. } => "save_replay",
        Command::StartRecording { .. } => "start_recording",
        Command::StopRecording => "stop_recording",
        Command::Pause => "pause",
        Command::Resume => "resume",
        Command::Status => "status",
    }
}

fn limits(max_secs: u32, max_mb: u32) -> RingLimits {
    RingLimits { max_secs: f64::from(max_secs), max_bytes: u64::from(max_mb) * 1024 * 1024 }
}

fn error(message: impl Into<String>) -> Event {
    Event::Error { message: message.into() }
}

fn idle_status() -> Event {
    Event::Status {
        engine_running: false, recording: false, paused: false, elapsed: 0.0,
        buffered_secs: 0.0, buffered_bytes: 0, restarts: 0,
    }
}

/// Handle one command. Responses that need a remux (`SaveReplay`,
/// `StopRecording`) are emitted later by the engine; everything else here.
fn dispatch(engine: &mut Option<Engine>, cmd: Command, cid: &str, emit_fn: &Emit) {
    let reply = |event: Event| emit(event, cid);
    match cmd {
        Command::StartEngine { args, max_secs, max_mb } => {
            if engine.is_some() {
                return reply(error("engine already running"));
            }
            match Engine::start(args, limits(max_secs, max_mb), emit_fn.clone(), cid) {
                Ok(e) => {
                    *engine = Some(e);
                    reply(Event::EngineStarted)
                }
                Err(e) => reply(error(format!("{e:#}"))),
            }
        }
        Command::Status => reply(engine.as_ref().map(Engine::status).unwrap_or_else(idle_status)),
        cmd => {
            let Some(e) = engine.as_ref() else { return reply(error("engine not running")) };
            match cmd {
                Command::StopEngine => {
                    if let Some(e) = engine.take() {
                        e.stop();
                    }
                    reply(Event::EngineStopped)
                }
                Command::SetLimits { max_secs, max_mb } => {
                    e.set_limits(limits(max_secs, max_mb));
                    reply(e.status())
                }
                Command::SaveReplay { secs, output_path } => e.save(secs, PathBuf::from(output_path), cid.to_string()),
                Command::StartRecording { output_path } => match e.start_recording(PathBuf::from(&output_path)) {
                    Ok(()) => reply(Event::RecordingStarted { path: output_path }),
                    Err(err) => reply(error(format!("{err:#}"))),
                },
                Command::StopRecording => e.stop_recording(cid.to_string()),
                Command::Pause | Command::Resume => {
                    let res = if matches!(cmd, Command::Pause) { e.pause() } else { e.resume() };
                    match res {
                        Ok(()) => reply(e.status()),
                        Err(err) => reply(error(format!("{err:#}"))),
                    }
                }
                _ => unreachable!("handled above"),
            }
        }
    }
}

fn main() {
    let _log_guard = cliplite_core::telemetry::init_json_logging("recorder");
    tracing::info!(pid = std::process::id(), "cliplite-recorder started");

    let emit_fn: Emit = Arc::new(emit);
    let mut engine: Option<Engine> = None;

    for line in BufReader::new(std::io::stdin().lock()).lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            Ok(_) => continue,
            Err(e) => {
                // e.g. invalid UTF-8: the line is consumed, keep serving
                emit(error(format!("Parse error: {e}")), &new_cid());
                continue;
            }
        };
        let env: Envelope<Command> = match serde_json::from_str(&line) {
            Ok(c) => c,
            Err(e) => {
                emit(error(format!("Parse error: {e}")), &new_cid());
                continue;
            }
        };
        let cid = env.cid.unwrap_or_else(new_cid);
        tracing::info!(stage = Stage::RecorderCommand.as_str(), cid = %cid,
                       cmd = command_name(&env.body), "command received");
        dispatch(&mut engine, env.body, &cid, &emit_fn);
    }

    if let Some(e) = engine.take() {
        e.stop();
    }
    tracing::info!("stdin closed; cliplite-recorder exiting");
}
