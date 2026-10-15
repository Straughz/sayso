//! Persistent state, including data moved from earlier releases:
//! * `%APPDATA%\Sayso\config.json` - read/write with unknown fields preserved.
//! * `%APPDATA%\Sayso\xai_key.enc` - DPAPI-encrypted xAI key.
//! * `%LOCALAPPDATA%\Sayso\history.jsonl` - append-only dictation history.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};
use serde::Serialize;

use crate::log;

/// See `Config::vad_threshold`.
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.3;

pub fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Sayso")
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Sayso")
}

/// Move the old user folders before the new log or settings are created.
/// Errors are reported after logging starts and never prevent startup.
pub fn migrate_legacy_dirs() -> Vec<String> {
    let old_cfg = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()))
        .join("NVGVoice");
    let old_data = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
        .join("NVGVoice");
    let new_cfg = config_dir();
    let new_data = data_dir();
    let mut lines = Vec::new();
    for (old, new) in [(&old_cfg, &new_cfg), (&old_data, &new_data)] {
        if let Some(line) = migrate_pair(old, new) {
            lines.push(line);
        }
    }

    let config_path = new_cfg.join("config.json");
    if config_path.is_file() {
        match Config::load_from(config_path) {
            Ok(mut config) => {
                let changed = rewrite_legacy_paths(
                    &mut config.raw, &old_cfg, &new_cfg, &old_data, &new_data,
                );
                if changed > 0 {
                    match config.save_checked() {
                        Ok(()) => lines.push(format!("Updated {changed} legacy path(s) in config.json")),
                        Err(e) => lines.push(format!("Could not update legacy paths in config.json: {e}")),
                    }
                }
            }
            Err(e) => lines.push(format!("Could not inspect config.json for legacy paths: {e}")),
        }
    }
    lines
}

fn migrate_pair(old: &Path, new: &Path) -> Option<String> {
    if !old.exists() {
        return None;
    }
    if new.exists() {
        let entries = match std::fs::read_dir(old) {
            Ok(entries) => entries,
            Err(e) => return Some(format!("Could not read legacy folder {}: {e}", old.display())),
        };
        let mut moved = 0;
        let mut skipped = 0;
        let mut failures = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    failures.push(format!("could not read entry: {e}"));
                    continue;
                }
            };
            let source = entry.path();
            let destination = new.join(entry.file_name());
            if destination.exists() {
                skipped += 1;
                continue;
            }
            if let Err(rename_error) = std::fs::rename(&source, &destination) {
                let copied = match entry.file_type() {
                    Ok(kind) if kind.is_dir() => copy_dir_recursive(&source, &destination),
                    Ok(_) => std::fs::copy(&source, &destination).map(|_| ()),
                    Err(e) => Err(e),
                };
                if let Err(copy_error) = copied {
                    // Leave the source in place, and clear a partial destination for a retry.
                    if destination.is_dir() {
                        let _ = std::fs::remove_dir_all(&destination);
                    } else if destination.is_file() {
                        let _ = std::fs::remove_file(&destination);
                    }
                    failures.push(format!("{}: move failed ({rename_error}), copy failed ({copy_error})", source.display()));
                    continue;
                }
                let removed = if source.is_dir() {
                    std::fs::remove_dir_all(&source)
                } else {
                    std::fs::remove_file(&source)
                };
                if let Err(e) = removed {
                    failures.push(format!("{}: copied but could not remove source ({e})", source.display()));
                    continue;
                }
            }
            moved += 1;
        }
        if let Ok(mut entries) = std::fs::read_dir(old) {
            if entries.next().is_none() {
                if let Err(e) = std::fs::remove_dir(old) {
                    failures.push(format!("could not remove empty folder {}: {e}", old.display()));
                }
            }
        }
        if moved == 0 && skipped == 0 && failures.is_empty() {
            return None;
        }
        let mut line = format!(
            "Merged {moved} entr(ies) from {} into {} ({skipped} skipped: already present)",
            old.display(), new.display(),
        );
        if !failures.is_empty() {
            line.push_str(&format!("; {}", failures.join("; ")));
        }
        return Some(line);
    }
    match std::fs::rename(old, new) {
        Ok(()) => Some(format!("Moved legacy folder {} to {}", old.display(), new.display())),
        Err(rename_error) => match copy_dir_recursive(old, new) {
            Ok(()) => Some(format!(
                "Copied legacy folder {} to {} after move failed ({rename_error}); old folder retained",
                old.display(), new.display(),
            )),
            Err(copy_error) => {
                // A partial copy must not become the app's new data directory.
                let cleanup = std::fs::remove_dir_all(new).err();
                Some(format!(
                    "Could not migrate legacy folder {} to {}: move failed ({rename_error}), copy failed ({copy_error}){}",
                    old.display(), new.display(),
                    cleanup.map(|e| format!(", partial-copy cleanup failed ({e})")).unwrap_or_default(),
                ))
            }
        },
    }
}

fn copy_dir_recursive(old: &Path, new: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(new)?;
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let destination = new.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// Rewrite top-level string values only. Match whole directory components so
/// a similarly named sibling folder is never changed.
fn rewrite_legacy_paths(
    value: &mut Map<String, Value>, old_cfg: &Path, new_cfg: &Path,
    old_data: &Path, new_data: &Path,
) -> usize {
    let mut changed = 0;
    for item in value.values_mut() {
        let Some(path) = item.as_str() else { continue };
        let replacement = [(old_cfg, new_cfg), (old_data, new_data)]
            .into_iter()
            .find_map(|(old, new)| rewrite_path_prefix(path, old, new));
        if let Some(replacement) = replacement {
            *item = Value::String(replacement);
            changed += 1;
        }
    }
    changed
}

fn rewrite_path_prefix(value: &str, old: &Path, new: &Path) -> Option<String> {
    let old = old.to_string_lossy();
    let prefix = value.get(..old.len())?;
    let same = prefix.replace('\\', "/").to_lowercase()
        == old.replace('\\', "/").to_lowercase();
    if !same {
        return None;
    }
    let suffix = value.get(old.len()..)?;
    if !suffix.is_empty() && !suffix.starts_with('\\') && !suffix.starts_with('/') {
        return None;
    }
    Some(format!("{}{suffix}", new.display()))
}

/// The capture ring keeps the newest `RING_CAP` recordings so the background
/// review can re-run the audio of a recent dictation.
pub const RING_CAP: usize = 30;

pub fn capture_ring_dir() -> PathBuf {
    data_dir().join("captures").join("ring")
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write one capture into the ring and delete all but the newest `RING_CAP`.
/// A non-WAV or unparseable file name is left alone.
pub fn write_capture(dir: &std::path::Path, wav: &[u8], name: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(name), wav)?;
    prune_ring(dir, RING_CAP);
    Ok(())
}

/// Delete the oldest `<unix_ms>.wav` files so at most `keep` remain.
pub fn prune_ring(dir: &std::path::Path, keep: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(i64, PathBuf)> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("wav") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            let stamp = stem.parse::<i64>().ok()?;
            Some((stamp, path))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|(stamp, _)| *stamp);
    for (_, path) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Thin wrapper over the JSON object so unknown fields survive a save. Stage 1
/// only writes `recording_mode` (from the tray), which is exactly the case this
/// guards: rewriting the parsed settings model would silently drop anything a
/// future Python version added.
pub struct Config {
    path: PathBuf,
    raw: Map<String, Value>,
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    /// For readers: a broken file reads as defaults (the error is logged once
    /// at start-up by `load_checked`, and every writer refuses to touch it).
    pub fn load() -> Self {
        Self::load_checked().unwrap_or_else(|_| Config { path: Self::path(), raw: Map::new() })
    }

    pub fn load_checked() -> Result<Self, String> {
        Self::load_from(Self::path())
    }

    /// Strict load. A missing file is an empty config; a file that exists
    /// but cannot be read or parsed is an error, so no caller ever writes a
    /// near-empty map over the user's settings.
    pub fn load_from(path: PathBuf) -> Result<Self, String> {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config { path, raw: Map::new() });
            }
            Err(e) => return Err(format!("Could not read the settings file ({e})")),
        };
        // Windows PowerShell 5 saves UTF-8 with a byte-order mark.
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let value: Value = serde_json::from_slice(body)
            .map_err(|e| format!("The settings file is not valid JSON ({e}); fix or delete it"))?;
        match value {
            Value::Object(raw) => Ok(Config { path, raw }),
            _ => Err("The settings file is not a JSON object; fix or delete it".into()),
        }
    }

    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.raw
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.raw.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn get_u64(&self, key: &str, default: u64) -> u64 {
        self.raw.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
    }

    pub fn get_f64(&self, key: &str, default: f64) -> f64 {
        self.raw.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
    }

    pub fn get_string_list(&self, key: &str) -> Vec<String> {
        self.raw
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.raw.insert(key.to_string(), value);
    }

    pub fn save_checked(&self) -> Result<(), String> {
        let doc = Value::Object(self.raw.clone());
        let s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        write_atomic(&self.path, s.as_bytes())
    }

    /// One-field transaction. Re-load immediately before writing so an open
    /// settings window cannot overwrite unrelated edits made by another app.
    /// A file that does not parse is left alone and the error returned.
    pub fn set_in_file(path: PathBuf, key: &str, value: Value) -> Result<Self, String> {
        let mut cfg = Self::load_from(path)?;
        cfg.set(key, value);
        cfg.save_checked()?;
        Ok(cfg)
    }

    // ── Typed accessors (names/defaults mirror VoxSettings) ─────────────

    pub fn hotkey(&self) -> String {
        self.get_str("hotkey", "Ctrl+Space")
    }

    pub fn mic_device(&self) -> String {
        self.get_str("mic_device", "")
    }

    pub fn remove_fillers(&self) -> bool {
        self.get_bool("remove_fillers", true)
    }

    pub fn auto_capitalize(&self) -> bool {
        self.get_bool("auto_capitalize", true)
    }

    pub fn trailing_space(&self) -> bool {
        self.get_bool("trailing_space", true)
    }

    pub fn post_process(&self) -> bool {
        self.get_bool("post_process", true)
    }

    pub fn code_mode(&self) -> bool {
        self.get_bool("code_mode", false)
    }

    pub fn suppress_hotkey(&self) -> bool {
        self.get_bool("suppress_hotkey", true)
    }

    pub fn restore_clipboard(&self) -> bool {
        self.get_bool("restore_clipboard", true)
    }

    /// `8` and `1000` were old defaults that pushed almost every dictation
    /// through the clipboard paste path; VoxSettings migrates both to 20000.
    pub fn clipboard_threshold(&self) -> usize {
        match self.get_u64("clipboard_threshold", 20000) {
            8 | 1000 => 20000,
            v => v as usize,
        }
    }

    pub fn recording_mode(&self) -> String {
        self.get_str("recording_mode", "hold")
    }

    pub fn history_enabled(&self) -> bool {
        self.get_bool("history_enabled", true)
    }

    pub fn dictionary_path(&self) -> String {
        self.get_str("dictionary_path", "")
    }

    pub fn snippets_path(&self) -> String {
        self.get_str("snippets_path", "")
    }

    pub fn hot_words(&self) -> Vec<String> {
        self.get_string_list("hot_words")
    }

    pub fn auto_vocabulary(&self) -> bool {
        self.get_bool("auto_vocabulary", false)
    }

    /// Model for the daily vocabulary review. `grok-4.3` was the default until
    /// 2026-10-14; Grok 4.7 costs about a cent more per review.
    pub fn review_model(&self) -> String {
        self.get_str("review_model", "grok-4.7")
    }

    /// xAI's speech-probability gate for batch transcription (API default 0.5).
    /// Quiet speakers lose first words at 0.5, so the default here is lower;
    /// push-to-talk already bounds the audio, so background noise rarely
    /// reaches the model. Config-only, clamped to 0.0..=1.0.
    pub fn vad_threshold(&self) -> f32 {
        let v = self.get_f64("vad_threshold", DEFAULT_VAD_THRESHOLD as f64);
        if v.is_finite() { v.clamp(0.0, 1.0) as f32 } else { DEFAULT_VAD_THRESHOLD }
    }

    pub fn workspace_folders(&self) -> Vec<String> {
        self.get_string_list("workspace_folders")
    }

    /// Folder names the workspace harvest skips (exact, case-sensitive).
    /// Default empty; set it to keep client or employer names out of the
    /// hint pool.
    pub fn workspace_exclude(&self) -> Vec<String> {
        self.get_string_list("workspace_exclude")
    }
}

// ---------------------------------------------------------------------------
// Secrets (DPAPI)
// ---------------------------------------------------------------------------

/// Read the xAI key: `XAI_API_KEY` wins, then the DPAPI blob. The key is never
/// logged. Uses CryptUnprotectData directly (same blob format as
/// `src/vox/secrets.py`).
pub fn read_xai_key() -> Option<String> {
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }
    let path = config_dir().join("xai_key.enc");
    let blob = std::fs::read(path).ok()?;
    // A key saved by the old Python app may carry a trailing newline, which
    // makes every request fail at header construction.
    let key = dpapi_unprotect(&blob).ok()?;
    let key = key.trim();
    if key.is_empty() { None } else { Some(key.to_string()) }
}

/// Write through a temporary file in the same folder and rename it over the
/// target, so a crash or power loss never leaves a truncated file.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("No parent folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

/// The stored key is never returned to a webview. Environment overrides still
/// take precedence when constructing the live engine.
pub fn xai_key_saved() -> bool {
    config_dir().join("xai_key.enc").is_file()
}

pub fn write_xai_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("Enter an API key".into());
    }
    let blob = dpapi_protect(key.trim().as_bytes())?;
    write_atomic(&config_dir().join("xai_key.enc"), &blob)
}

#[cfg(windows)]
fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe { CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut output) };
    if ok == 0 || output.pbData.is_null() { return Err("Could not encrypt API key".into()); }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { let _ = LocalFree(output.pbData as *mut core::ffi::c_void); }
    Ok(bytes)
}

#[cfg(not(windows))]
fn dpapi_protect(_data: &[u8]) -> Result<Vec<u8>, String> { Err("DPAPI is Windows-only".into()) }

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<String, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return Err("CryptUnprotectData failed".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(output.pbData as *mut core::ffi::c_void);
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn dpapi_unprotect(_data: &[u8]) -> Result<String, String> {
    Err("DPAPI is Windows-only".into())
}

// ---------------------------------------------------------------------------
// History (JSONL)
// ---------------------------------------------------------------------------

pub const HISTORY_MAX_ENTRIES: usize = 1000;

pub struct HistoryStore {
    path: PathBuf,
    enabled: bool,
    session: String,
    lock: Mutex<()>,
}

impl HistoryStore {
    pub fn new(enabled: bool) -> Self {
        let path = data_dir().join("history.jsonl");
        let store = HistoryStore {
            path,
            enabled,
            session: new_session_id(),
            lock: Mutex::new(()),
        };
        store.prune();
        store
    }

    /// `cap` names the capture-ring file for this dictation; `auto_fixes` is
    /// the list of automatic replacements applied to the text. Both are
    /// optional and never written when empty.
    pub fn append(
        &self,
        text: &str,
        duration_s: f64,
        backend: &str,
        cap: Option<&str>,
        auto_fixes: &[(String, String)],
    ) {
        if !self.enabled {
            return;
        }
        let mut entry = serde_json::json!({
            "ts": now_epoch_s(),
            "text": text,
            "duration_s": duration_s,
            "backend": backend,
            "chars": text.chars().count(),
            "words": text.split_whitespace().count(),
            "session": self.session,
        });
        if let (Some(cap), Some(obj)) = (cap, entry.as_object_mut()) {
            obj.insert("cap".to_string(), Value::String(cap.to_string()));
        }
        if !auto_fixes.is_empty() {
            if let Some(obj) = entry.as_object_mut() {
                let list: Vec<Value> = auto_fixes
                    .iter()
                    .map(|(typed, meant)| serde_json::json!([typed, meant]))
                    .collect();
                obj.insert("auto_fixes".to_string(), Value::Array(list));
            }
        }
        self.write_line(&entry.to_string());
    }

    pub fn append_undo(&self) {
        if !self.enabled {
            return;
        }
        let entry = serde_json::json!({ "event": "undo", "ts": now_epoch_s() });
        self.write_line(&entry.to_string());
    }

    pub fn read_entries(&self) -> Vec<HistoryEntry> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        read_history_from(&self.path)
    }

    fn write_line(&self, line: &str) {
        let _guard = self.lock.lock().ok();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = writeln!(f, "{line}") {
                    log::error(&format!("history write failed: {e}"));
                }
            }
            Err(e) => log::error(&format!("history open failed: {e}")),
        }
    }

    /// Truncate to the newest `HISTORY_MAX_ENTRIES` lines at startup.
    fn prune(&self) {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return;
        };
        // One bad byte must not stop pruning (or reading) for good.
        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() <= HISTORY_MAX_ENTRIES {
            return;
        }
        let trimmed = lines[lines.len() - HISTORY_MAX_ENTRIES..].join("\n");
        if let Err(e) = write_atomic(&self.path, format!("{trimmed}\n").as_bytes()) {
            log::error(&format!("history prune failed: {e}"));
        }
    }
}

#[derive(Clone, Serialize)]
pub struct HistoryEntry {
    pub ts: f64,
    pub text: String,
    pub duration_s: f64,
    pub words: usize,
    pub undone: bool,
    /// Capture-ring file name for this dictation, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<String>,
    /// Automatic replacements applied to this dictation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub auto_fixes: Vec<[String; 2]>,
}

#[derive(Serialize)]
pub struct HistoryStats {
    pub total_words: usize,
    pub speaking_time_s: f64,
    pub entry_count: usize,
    pub avg_wpm: f64,
    pub week_words: usize,
    pub week_dictations: usize,
}

pub fn read_history_from(path: &std::path::Path) -> Vec<HistoryEntry> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let content = String::from_utf8_lossy(&bytes);
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("event").and_then(Value::as_str) == Some("undo") {
            if let Some(last) = entries.iter_mut().rev().find(|e| !e.undone) { last.undone = true; }
            continue;
        }
        let (Some(ts), Some(text)) = (v.get("ts").and_then(Value::as_f64), v.get("text").and_then(Value::as_str)) else { continue };
        let auto_fixes = v
            .get("auto_fixes")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|pair| {
                        let pair = pair.as_array()?;
                        let typed = pair.first()?.as_str()?;
                        let meant = pair.get(1)?.as_str()?;
                        Some([typed.to_string(), meant.to_string()])
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.push(HistoryEntry {
            ts, text: text.to_string(),
            duration_s: v.get("duration_s").and_then(Value::as_f64).unwrap_or(0.0).max(0.0),
            words: v.get("words").and_then(Value::as_u64).map(|n| n as usize).unwrap_or_else(|| text.split_whitespace().count()),
            undone: false,
            cap: v.get("cap").and_then(Value::as_str).map(|s| s.to_string()),
            auto_fixes,
        });
    }
    entries
}

pub fn compute_stats(entries: &[HistoryEntry]) -> HistoryStats {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now().date_naive();
    let week_start = now - chrono::Duration::days(now.weekday().num_days_from_monday() as i64);
    let mut stats = HistoryStats { total_words: 0, speaking_time_s: 0.0, entry_count: 0, avg_wpm: 0.0, week_words: 0, week_dictations: 0 };
    for e in entries.iter().filter(|e| !e.undone) {
        stats.total_words += e.words;
        stats.speaking_time_s += e.duration_s;
        stats.entry_count += 1;
        if let Some(day) = Local.timestamp_opt(e.ts as i64, 0).single().map(|dt| dt.date_naive()) {
            if day >= week_start && day < week_start + chrono::Duration::days(7) {
                stats.week_words += e.words;
                stats.week_dictations += 1;
            }
        }
    }
    if stats.speaking_time_s > 0.0 { stats.avg_wpm = stats.total_words as f64 * 60.0 / stats.speaking_time_s; }
    stats
}

pub fn edit_vocabulary(path: PathBuf, term: &str, add: bool) -> Result<Vec<String>, String> {
    let term = term.trim();
    if term.is_empty() || term.chars().count() > 50 { return Err("Enter a term of 1 to 50 characters".into()); }
    let mut cfg = Config::load_from(path)?;
    let mut words = cfg.hot_words();
    if add {
        if !words.iter().any(|w| w.eq_ignore_ascii_case(term)) {
            if words.len() >= 100 { return Err("Vocabulary is limited to 100 terms".into()); }
            words.push(term.to_string());
        }
    } else {
        words.retain(|w| !w.eq_ignore_ascii_case(term));
    }
    cfg.set("hot_words", serde_json::json!(words));
    cfg.save_checked()?;
    Ok(words)
}

/// A user dictionary replaces the bundled copy. Seed it on first correction.
pub fn save_correction_at(config_path: PathBuf, default_path: PathBuf, embedded: &str, wrong: &str, right: &str) -> Result<String, String> {
    let (wrong, right) = (wrong.trim().to_lowercase(), right.trim());
    if wrong.is_empty() || right.is_empty() || wrong.chars().count() > 80 || right.chars().count() > 80 {
        return Err("Enter a word or phrase up to 80 characters in each field".into());
    }
    let mut cfg = Config::load_from(config_path)?;
    let configured = cfg.dictionary_path();
    let path = if configured.is_empty() { default_path } else { PathBuf::from(&configured) };
    let source = if configured.is_empty() { embedded.to_string() } else {
        std::fs::read_to_string(&path).map_err(|e| format!("Could not read dictionary: {e}"))?
    };
    let mut map = serde_json::from_str::<Map<String, Value>>(&source).map_err(|e| format!("Invalid dictionary: {e}"))?;
    let old = map.keys().find(|k| k.eq_ignore_ascii_case(&wrong)).cloned();
    if let Some(old) = old { map.remove(&old); }
    map.insert(wrong, Value::String(right.to_string()));
    write_atomic(&path, serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?.as_bytes())?;
    if configured.is_empty() {
        cfg.set("dictionary_path", Value::String(path.to_string_lossy().into_owned()));
        cfg.save_checked()?;
    }
    Ok(path.to_string_lossy().into_owned())
}

fn now_epoch_s() -> f64 {
    chrono::Local::now().timestamp_millis() as f64 / 1000.0
}

fn new_session_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut h);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}

// ---------------------------------------------------------------------------
// Bundled dictionary / snippets helpers
// ---------------------------------------------------------------------------

/// Resolve a configured path (non-empty wins; a missing file degrades to None
/// with a warning, exactly like the Python loaders) or fall back to the
/// embedded copy.
pub fn load_or_embedded(path: &str, embedded: &str, label: &str) -> String {
    if path.trim().is_empty() {
        return embedded.to_string();
    }
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            log::warn(&format!("{label}: could not load {path}: {e} — using empty"));
            String::new()
        }
    }
}

#[cfg(test)]
mod stage2_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn stt_option_defaults_and_clamping() {
        let cfg = Config { path: PathBuf::from("unused"), raw: serde_json::Map::new() };
        assert_eq!(cfg.review_model(), "grok-4.7");
        assert_eq!(cfg.vad_threshold(), DEFAULT_VAD_THRESHOLD);
        let mut raw = serde_json::Map::new();
        raw.insert("vad_threshold".into(), serde_json::json!(0.15));
        raw.insert("review_model".into(), serde_json::json!("grok-4.3"));
        let cfg = Config { path: PathBuf::from("unused"), raw };
        assert!((cfg.vad_threshold() - 0.15).abs() < 1e-6);
        assert_eq!(cfg.review_model(), "grok-4.3");
        let mut raw = serde_json::Map::new();
        raw.insert("vad_threshold".into(), serde_json::json!(7));
        assert_eq!(Config { path: PathBuf::from("unused"), raw }.vad_threshold(), 1.0);
        let mut raw = serde_json::Map::new();
        raw.insert("vad_threshold".into(), serde_json::json!("loud"));
        assert_eq!(Config { path: PathBuf::from("unused"), raw }.vad_threshold(), DEFAULT_VAD_THRESHOLD);
    }

    fn test_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("sayso-stage2-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_path_rewrite_preserves_other_values() {
        let old_cfg = Path::new(r"C:\Users\User\AppData\Roaming\NVGVoice");
        let new_cfg = Path::new(r"C:\Users\User\AppData\Roaming\Sayso");
        let old_data = Path::new(r"C:\Users\User\AppData\Local\NVGVoice");
        let new_data = Path::new(r"C:\Users\User\AppData\Local\Sayso");
        let mut raw = serde_json::json!({
            "snippets_path": r"C:\Users\User\AppData\Roaming\NVGVoice\snippets.json",
            "dictionary_path": "c:/users/user/appdata/roaming/nvgvoice/dictionary.json",
            "capture_path": r"C:\Users\User\AppData\Local\NVGVoice\captures\ring",
            "similar_sibling": r"C:\Users\User\AppData\Roaming\NVGVoiceExtra\leave.json",
            "other": "leave this alone",
            "number": 7,
            "nested": { "path": r"C:\Users\User\AppData\Roaming\NVGVoice\nested.json" }
        }).as_object().unwrap().clone();
        assert_eq!(rewrite_legacy_paths(&mut raw, old_cfg, new_cfg, old_data, new_data), 3);
        assert_eq!(raw["snippets_path"], r"C:\Users\User\AppData\Roaming\Sayso\snippets.json");
        assert_eq!(raw["dictionary_path"], r"C:\Users\User\AppData\Roaming\Sayso/dictionary.json");
        assert_eq!(raw["capture_path"], r"C:\Users\User\AppData\Local\Sayso\captures\ring");
        assert_eq!(raw["similar_sibling"], r"C:\Users\User\AppData\Roaming\NVGVoiceExtra\leave.json");
        assert_eq!(raw["other"], "leave this alone");
        assert_eq!(raw["number"], 7);
        assert_eq!(raw["nested"]["path"], r"C:\Users\User\AppData\Roaming\NVGVoice\nested.json");
    }

    #[test]
    fn legacy_folder_move_is_idempotent() {
        let root = test_dir("migration");
        let old = root.join("NVGVoice");
        let new = root.join("Sayso");
        std::fs::create_dir_all(old.join("nested")).unwrap();
        std::fs::write(old.join("nested").join("file.txt"), b"kept").unwrap();
        assert!(migrate_pair(&old, &new).is_some());
        assert_eq!(std::fs::read(new.join("nested").join("file.txt")).unwrap(), b"kept");
        assert!(!old.exists());
        assert!(migrate_pair(&old, &new).is_none());

        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("another.txt"), b"old").unwrap();
        assert!(migrate_pair(&old, &new).unwrap().starts_with("Merged 1"));
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("another.txt")).unwrap(), b"old");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrate_pair_merges_into_existing_new_dir() {
        let root = test_dir("migration-merge");
        let old = root.join("NVGVoice");
        let new = root.join("Sayso");
        std::fs::create_dir_all(old.join("captures").join("ring")).unwrap();
        std::fs::write(old.join("history.jsonl"), b"history").unwrap();
        std::fs::write(old.join("captures").join("ring").join("1.wav"), b"audio").unwrap();
        std::fs::create_dir_all(new.join("logs")).unwrap();
        std::fs::write(new.join("sayso.exe"), b"exe").unwrap();
        std::fs::write(new.join("logs").join("sayso.log"), b"log").unwrap();

        assert!(migrate_pair(&old, &new).unwrap().starts_with("Merged 2"));
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("history.jsonl")).unwrap(), b"history");
        assert_eq!(std::fs::read(new.join("captures").join("ring").join("1.wav")).unwrap(), b"audio");
        assert_eq!(std::fs::read(new.join("sayso.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(new.join("logs").join("sayso.log")).unwrap(), b"log");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrate_pair_keeps_entries_that_already_exist_in_new() {
        let root = test_dir("migration-conflict");
        let old = root.join("NVGVoice");
        let new = root.join("Sayso");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("history.jsonl"), b"old").unwrap();
        std::fs::write(old.join("extra.txt"), b"extra").unwrap();
        std::fs::write(new.join("history.jsonl"), b"new").unwrap();

        assert!(migrate_pair(&old, &new).unwrap().contains("1 skipped"));
        assert_eq!(std::fs::read(new.join("history.jsonl")).unwrap(), b"new");
        assert_eq!(std::fs::read(new.join("extra.txt")).unwrap(), b"extra");
        assert_eq!(std::fs::read(old.join("history.jsonl")).unwrap(), b"old");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn read(path: &Path) -> Value { serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap() }

    #[test]
    fn history_reader_skips_bad_lines_marks_undo_and_computes_stats() {
        let dir = test_dir("history");
        let path = dir.join("history.jsonl");
        let now = chrono::Local::now().timestamp();
        std::fs::write(&path, format!(
            "{{\"ts\":{now},\"text\":\"first two words\",\"duration_s\":3,\"words\":3}}\ninvalid\n{{\"ts\":{now},\"text\":\"second text\",\"duration_s\":2}}\n{{\"event\":\"undo\",\"ts\":{now}}}\n"
        )).unwrap();
        let entries = read_history_from(&path);
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].undone);
        assert!(entries[1].undone);
        let stats = compute_stats(&entries);
        assert_eq!(stats.total_words, 3);
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.speaking_time_s, 3.0);
        assert_eq!(stats.avg_wpm, 60.0);
        assert_eq!(stats.week_dictations, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn vocabulary_add_remove_persists_without_losing_other_keys() {
        let dir = test_dir("vocab");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"other":"keep","hot_words":["Astra"]}"#).unwrap();
        assert_eq!(edit_vocabulary(path.clone(), "Codex", true).unwrap(), vec!["Astra", "Codex"]);
        assert_eq!(edit_vocabulary(path.clone(), "astra", false).unwrap(), vec!["Codex"]);
        let doc = read(&path);
        assert_eq!(doc["other"], "keep");
        assert_eq!(doc["hot_words"], serde_json::json!(["Codex"]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn correction_updates_existing_dictionary_case_insensitively() {
        let dir = test_dir("dictionary-existing");
        let config = dir.join("config.json");
        let dictionary = dir.join("custom.json");
        std::fs::write(&dictionary, r#"{"Nvg":"old","keep":"value"}"#).unwrap();
        std::fs::write(&config, serde_json::json!({"dictionary_path":dictionary,"other":42}).to_string()).unwrap();
        save_correction_at(config.clone(), dir.join("default.json"), r#"{"bundled":"word"}"#, "nVG", "NVG").unwrap();
        let doc = read(&dictionary);
        assert_eq!(doc["nvg"], "NVG");
        assert_eq!(doc["keep"], "value");
        assert_eq!(doc.as_object().unwrap().len(), 2);
        assert_eq!(read(&config)["other"], 42);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn correction_seeds_bundled_dictionary_and_sets_path() {
        let dir = test_dir("dictionary-new");
        let config = dir.join("config.json");
        let dictionary = dir.join("dictionary.json");
        std::fs::write(&config, r#"{"other":"untouched"}"#).unwrap();
        save_correction_at(config.clone(), dictionary.clone(), r#"{"aks":"ask","vue":"view"}"#, "CokPit", "Cokpit").unwrap();
        let doc = read(&dictionary);
        assert_eq!(doc["aks"], "ask");
        assert_eq!(doc["vue"], "view");
        assert_eq!(doc["cokpit"], "Cokpit");
        assert_eq!(read(&config)["dictionary_path"].as_str(), dictionary.to_str());
        assert_eq!(read(&config)["other"], "untouched");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn single_field_save_reloads_and_preserves_unrelated_edit() {
        let dir = test_dir("config");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"hotkey":"Ctrl+Space","future":{"x":1}}"#).unwrap();
        let stale = Config::load_from(path.clone()).unwrap();
        std::fs::write(&path, r#"{"hotkey":"Ctrl+Win","future":{"x":2}}"#).unwrap();
        let saved = Config::set_in_file(path.clone(), "recording_mode", Value::String("toggle".into())).unwrap();
        assert_eq!(stale.hotkey(), "Ctrl+Space");
        assert_eq!(saved.hotkey(), "Ctrl+Win");
        assert_eq!(read(&path)["future"]["x"], 2);
        assert_eq!(read(&path)["recording_mode"], "toggle");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_with_utf8_bom_is_accepted() {
        let dir = test_dir("bom");
        let path = dir.join("config.json");
        std::fs::write(&path, b"\xEF\xBB\xBF{\"hotkey\":\"Ctrl+Win\",\"keep\":1}").unwrap();
        let cfg = Config::load_from(path.clone()).unwrap();
        assert_eq!(cfg.hotkey(), "Ctrl+Win");
        Config::set_in_file(path.clone(), "recording_mode", Value::String("toggle".into())).unwrap();
        let doc = read(&path);
        assert_eq!(doc["keep"], 1);
        assert_eq!(doc["recording_mode"], "toggle");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_config_loads_empty() {
        let dir = test_dir("missing");
        let cfg = Config::load_from(dir.join("config.json")).unwrap();
        assert_eq!(cfg.hotkey(), "Ctrl+Space");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_config_is_never_overwritten() {
        let dir = test_dir("broken");
        let path = dir.join("config.json");
        let broken = b"{\"hotkey\":\"Ctrl+Win\",\"hot_words\":[\"Astra\"],}";
        std::fs::write(&path, broken).unwrap();
        assert!(Config::load_from(path.clone()).is_err());
        assert!(Config::set_in_file(path.clone(), "recording_mode", Value::String("toggle".into())).is_err());
        assert!(edit_vocabulary(path.clone(), "Codex", true).is_err());
        assert!(save_correction_at(path.clone(), dir.join("dictionary.json"), "{}", "nvg", "NVG").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), broken.to_vec());
        assert!(!dir.join("dictionary.json").exists());
        // Non-object JSON is refused as well.
        std::fs::write(&path, b"[1,2]").unwrap();
        assert!(Config::set_in_file(path.clone(), "hotkey", Value::String("Ctrl+Win".into())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"[1,2]".to_vec());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_file() {
        let dir = test_dir("atomic");
        let path = dir.join("config.json");
        std::fs::write(&path, b"old").unwrap();
        write_atomic(&path, b"{\"new\":true}").unwrap();
        assert_eq!(read(&path)["new"], true);
        let leftovers = std::fs::read_dir(&dir).unwrap().filter(|e| {
            e.as_ref().map(|e| e.file_name().to_string_lossy().ends_with(".tmp")).unwrap_or(false)
        }).count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_round_trips_cap_and_auto_fixes() {
        let dir = test_dir("history-cap");
        let path = dir.join("history.jsonl");
        std::fs::write(&path, concat!(
            "{\"ts\":1.0,\"text\":\"one\",\"cap\":\"111.wav\",\"auto_fixes\":[[\"satterbar\",\"status bar\"]]}\n",
            "{\"ts\":2.0,\"text\":\"two\"}\n"
        )).unwrap();
        let entries = read_history_from(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cap.as_deref(), Some("111.wav"));
        assert_eq!(entries[0].auto_fixes, vec![["satterbar".to_string(), "status bar".to_string()]]);
        assert!(entries[1].cap.is_none());
        assert!(entries[1].auto_fixes.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ring_keeps_newest_thirty_and_deletes_oldest() {
        let dir = test_dir("ring");
        for i in 1..=35i64 {
            write_capture(&dir, b"x", &format!("{i}.wav")).unwrap();
        }
        let mut stamps: Vec<i64> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.path().file_stem()?.to_str()?.parse::<i64>().ok())
            .collect();
        stamps.sort();
        assert_eq!(stamps.len(), RING_CAP);
        assert_eq!(stamps.first().copied(), Some(6));
        assert_eq!(stamps.last().copied(), Some(35));
        assert!(!dir.join("5.wav").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_with_invalid_utf8_still_reads() {
        let dir = test_dir("history-utf8");
        let path = dir.join("history.jsonl");
        let mut bytes = b"{\"ts\":1,\"text\":\"ok\"}\n".to_vec();
        bytes.extend_from_slice(b"\xFF\xFE garbage\n{\"ts\":2,\"text\":\"also ok\"}\n");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read_history_from(&path).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
