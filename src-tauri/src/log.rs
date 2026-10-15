//! Size-rotating log at `%LOCALAPPDATA%\NVGVoice\logs\nvg-voice.log`.
//!
//! Deliberately tiny: one global mutex serialises writes so lines from the
//! hotkey, audio, coordination and Tauri threads never interleave. Rotation is
//! by size only (rename to `.1`), matching the "rotating by size is enough"
//! requirement for stage 1.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Rotate once the active log passes this size.
const MAX_BYTES: u64 = 1_000_000;

static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn log_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("NVGVoice").join("logs").join("nvg-voice.log")
}

fn write_line(level: &str, msg: &str) {
    let _guard = LOG_LOCK.lock().ok();
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    rotate_if_needed(&path);
    let line = format!(
        "{} {:<5} {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        level,
        msg
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn rotate_if_needed(path: &PathBuf) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= MAX_BYTES {
            let rotated = path.with_extension("log.1");
            let _ = std::fs::rename(path, rotated);
        }
    }
}

pub fn info(msg: &str) {
    write_line("INFO", msg);
}

pub fn warn(msg: &str) {
    write_line("WARN", msg);
}

pub fn error(msg: &str) {
    write_line("ERROR", msg);
}
