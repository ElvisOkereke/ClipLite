//! IPC protocol between `cliplite.exe` (UI) and `cliplite-recorder.exe`.
//!
//! Newline-delimited JSON over the recorder's stdin (commands) and stdout
//! (events). stdout carries nothing else.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    StartEngine    { args: EngineArgs, max_secs: u32, max_mb: u32 },
    StopEngine,
    SetLimits      { max_secs: u32, max_mb: u32 },
    SaveReplay     { secs: u32, output_path: String },
    StartRecording { output_path: String },
    StopRecording,
    Pause,
    Resume,
    Status,
}

/// Arguments for the single capture+encode engine pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineArgs {
    pub ffmpeg_path: String,
    pub source:      crate::ffmpeg::CaptureSource,
    pub region:      Region,
    pub audio_cfg:   AudioConfig,
    pub enc_cfg:     EncConfig,
    #[serde(default)]
    pub audio_pipe:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub x: i32, pub y: i32, pub width: i32, pub height: i32, pub monitor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub mic_device:    Option<String>,
    pub sys_device_id: Option<String>,
    pub mic_device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncConfig {
    pub fps: u32, pub quality_crf: u32, pub format: String, pub hw_encoder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    EngineStarted,
    EngineStopped,
    /// Unsolicited (fresh cid).
    EngineRestarted { reason: String, restarts: u32 },
    ReplaySaved {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_s: Option<f64>,
    },
    RecordingStarted { path: String },
    RecordingStopped { path: String },
    Status {
        engine_running: bool, recording: bool, paused: bool, elapsed: f64,
        buffered_secs: f64, buffered_bytes: u64, restarts: u32,
    },
    Error { message: String },
}

/// A command or event plus an optional correlation id, serialized flat:
/// `{"cmd":"stop_recording","cid":"abc"}` / `{"event":"recording_stopped","path":"…","cid":"abc"}`.
/// A missing `cid` deserializes to `None`, so pre-telemetry JSON still parses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    #[serde(flatten)]
    pub body: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
}

#[cfg(test)]
#[path = "ipc_tests.rs"]
mod tests;
