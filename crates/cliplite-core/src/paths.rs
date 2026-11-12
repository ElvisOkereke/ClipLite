//! Filesystem locations shared by both processes.
use std::path::PathBuf;

/// ClipLite's data directory: `CLIPLITE_HOME` if set (used by the test harness
/// to isolate runs), otherwise `~/.cliplite`.
pub fn app_dir() -> PathBuf {
    match std::env::var_os("CLIPLITE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir().unwrap_or_default().join(".cliplite"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_dir_honours_cliplite_home_with_spaces() {
        let dir = std::env::temp_dir().join("clip lite home");
        std::env::set_var("CLIPLITE_HOME", &dir);
        assert_eq!(app_dir(), dir);
        std::env::remove_var("CLIPLITE_HOME");
        assert_eq!(app_dir(), dirs::home_dir().unwrap().join(".cliplite"));
    }
}
