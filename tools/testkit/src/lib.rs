//! ClipLite hardware-in-the-loop test kit.
//!
//! - `fakegame`: D3D11 load generator that can hold exclusive fullscreen.
//! - `harness`/`job`/`input`/`clip`/`matrix`/`elevated`: the `hil` runner.
pub mod clip;
#[cfg(windows)]
pub mod elevated;
pub mod fakegame;
#[cfg(windows)]
pub mod harness;
pub mod input;
#[cfg(windows)]
pub mod job;
pub mod matrix;

#[cfg(windows)]
pub use harness::DEFAULT_CHORD;
#[cfg(not(windows))]
pub const DEFAULT_CHORD: &str = "Ctrl+Alt+Shift+F11";
