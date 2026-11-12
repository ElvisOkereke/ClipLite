//! IPC protocol between `cliplite.exe` (UI) and `cliplite-recorder.exe`.
//!
//! Newline-delimited JSON over the recorder's stdin (commands) and stdout
//! (events). stdout carries nothing else.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Start       { args: StartArgs },
    Stop,
    Pause,
    Resume,
    Status,
    StartReplay { args: StartArgs },
    StopReplay,
    SaveReplay  { secs: u32, output_path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartArgs {
    pub ffmpeg_path:  String,
    pub output_path:  String,
    pub region:       Region,
    pub audio_cfg:    AudioConfig,
    pub enc_cfg:      EncConfig,
    /// Named pipe for audio. Recording uses cliplite_sysaudio, replay uses cliplite_sysaudio_replay.
    /// If absent, defaults to cliplite_sysaudio.
    #[serde(default)]
    pub audio_pipe:   Option<String>,
    /// Max size in MB for temporary replay buffer files (0 = unlimited).
    /// When exceeded, oldest temp files are automatically cleaned up.
    #[serde(default)]
    pub max_replay_buffer_size_mb: u32,
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
    Started       { path: String },
    Stopped       { path: String },
    ReplayStarted { path: String },
    ReplayStopped,
    ReplaySaved   { path: String },
    Status        { is_recording: bool, is_paused: bool, elapsed: f64, replay_active: bool },
    Error         { message: String },
}

#[cfg(test)]
#[path = "ipc_tests.rs"]
mod tests;
