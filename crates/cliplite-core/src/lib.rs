//! Code shared by the ClipLite UI process and the standalone recorder.
//!
//! Deliberately has no Tauri/WebView dependency: the recorder links only this
//! crate, which keeps it free of any GPU/WebView2 context (see ARCHITECTURE.md).
pub mod audio;
pub mod ffmpeg;
pub mod ipc;
pub mod paths;
pub mod telemetry;
