//! Structured JSON logging shared by both processes.
//!
//! Every line in `app_dir()/logs/<stem>.<date>.jsonl` is one JSON object with
//! top-level `timestamp`, `level`, `target`, `message` and any event fields.
//! Pipeline events carry `stage` (see [`Stage`]) and `cid`, a correlation id
//! that follows one user action (e.g. a hotkey press) across the UI process,
//! the IPC pipe and the recorder — the HIL harness asserts on these.
//!
//! Existing `log::` macros are bridged into `tracing` automatically.
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// Default filter: our crates at debug, everything else at info.
const DEFAULT_FILTER: &str =
    "info,cliplite=debug,cliplite_lib=debug,cliplite_core=debug,cliplite_recorder=debug";

/// Pipeline stages, logged as the `stage` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    HotkeyPressed,
    HotkeyDispatched,
    CommandReceived,
    IpcSent,
    RecorderCommand,
    FfmpegSpawned,
    FfmpegExited,
    ReplaySaved,
    Error,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::HotkeyPressed => "hotkey.pressed",
            Stage::HotkeyDispatched => "hotkey.dispatched",
            Stage::CommandReceived => "command.received",
            Stage::IpcSent => "ipc.sent",
            Stage::RecorderCommand => "recorder.command",
            Stage::FfmpegSpawned => "ffmpeg.spawned",
            Stage::FfmpegExited => "ffmpeg.exited",
            Stage::ReplaySaved => "replay.saved",
            Stage::Error => "error",
        }
    }
}

/// A fresh correlation id: UUID v4 as 32 lowercase hex characters.
pub fn new_cid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The JSON subscriber used in production, parameterised over the writer so
/// tests can capture output.
pub fn json_subscriber<W>(writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_from_env("CLIPLITE_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(true)
        .with_env_filter(filter)
        .with_writer(writer)
        .finish()
}

/// Install JSON file logging for this process and a panic hook that logs the
/// panic. Keep the returned guard alive for the process lifetime (dropping it
/// flushes and stops the background writer).
pub fn init_json_logging(file_stem: &str) -> Option<WorkerGuard> {
    let dir = crate::paths::app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let appender = match Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(file_stem)
        .filename_suffix("jsonl")
        .max_log_files(7)
        .build(&dir)
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("cliplite: cannot open log dir {}: {e}", dir.display());
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    if tracing::subscriber::set_global_default(json_subscriber(writer)).is_err() {
        return None; // already initialised (e.g. called twice)
    }
    let _ = tracing_log::LogTracer::init();

    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(stage = Stage::Error.as_str(), panic = true, %location, "{message}");
    }));
    Some(guard)
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
