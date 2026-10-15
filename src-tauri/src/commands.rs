//! Small, typed bridge between the two short-lived webviews and the native core.
//! The bridge never returns a secret or a filesystem path to JavaScript.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::post::PostProcessor;
use crate::review::AutoDictionary;
use crate::state::{AppState, Core};
use crate::store::{self, Config, HistoryEntry, HistoryStats};
use crate::stt::{LiveStt, MissingKeyStt, SttEngine, XaiStt};
use crate::vocab::{self, Pool};

pub struct Runtime {
    pub core: Arc<Core>,
    pub stt: Arc<LiveStt>,
    pub config: Arc<Mutex<Config>>,
    pub auto: Arc<Mutex<AutoDictionary>>,
}

static APP: OnceLock<AppHandle> = OnceLock::new();
pub fn init_events(app: AppHandle) { let _ = APP.set(app); }
pub fn emit_status(status: &'static str) {
    if let Some(win) = APP.get().and_then(|app| app.get_webview_window("flyout")) {
        let _ = win.emit("voice-status", status);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    status: &'static str,
    hotkey: String,
    suppress_hotkey: bool,
    recording_mode: String,
    mic_device: String,
    key_saved: bool,
    remove_fillers: bool,
    auto_capitalize: bool,
    code_mode: bool,
    trailing_space: bool,
    launch_on_startup: bool,
    auto_vocabulary: bool,
    hot_words: Vec<String>,
    paused: bool,
    version: &'static str,
}

#[tauri::command]
pub fn get_snapshot(runtime: State<'_, Runtime>) -> Snapshot {
    let cfg = Config::load();
    let status = if runtime.core.paused.load(Ordering::SeqCst) { "Paused" }
        else if !runtime.stt.has_key() { "Missing API key" }
        else { match runtime.core.status() {
            AppState::Listening => "Listening",
            AppState::Processing => "Processing",
            AppState::Done => "Ready",
            AppState::Error => "Error",
            AppState::Idle => "Ready",
        }};
    Snapshot {
        status,
        hotkey: cfg.hotkey(),
        suppress_hotkey: cfg.suppress_hotkey(),
        recording_mode: runtime.core.recording_mode.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        mic_device: cfg.mic_device(),
        key_saved: store::xai_key_saved(),
        remove_fillers: cfg.remove_fillers(),
        auto_capitalize: cfg.auto_capitalize(),
        code_mode: cfg.code_mode(),
        trailing_space: cfg.trailing_space(),
        launch_on_startup: get_autostart(),
        auto_vocabulary: cfg.auto_vocabulary(),
        hot_words: cfg.hot_words(),
        paused: runtime.core.paused.load(Ordering::SeqCst),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    entries: Vec<HistoryEntry>,
    stats: HistoryStats,
}

// Async so reading the whole history file runs off the main event loop.
// (An async command that borrows State must return a Result.)
#[tauri::command]
pub async fn get_history(runtime: State<'_, Runtime>) -> Result<HistoryResponse, String> {
    let mut entries = runtime.core.history.read_entries();
    let stats = store::compute_stats(&entries);
    entries.reverse();
    Ok(HistoryResponse { entries, stats })
}

const MAX_MIC_NAME_CHARS: usize = 256;
const MAX_KEY_CHARS: usize = 512;

fn rebuild_post(cfg: &Config, runtime: &Runtime) {
    let auto_json = {
        let auto = runtime.auto.lock().unwrap_or_else(|p| p.into_inner());
        auto.entries_json()
    };
    let next = PostProcessor::from_config(cfg, auto_json.as_deref());
    *runtime.core.post.write().unwrap_or_else(|p| p.into_inner()) = next;
}

fn rebuild_stt(cfg: &Config, runtime: &Runtime) -> Result<(), String> {
    let engine: Arc<dyn SttEngine> = match store::read_xai_key() {
        Some(key) => Arc::new(XaiStt::new(key, cfg.vad_threshold())?),
        None => Arc::new(MissingKeyStt),
    };
    runtime.stt.replace(engine);
    Ok(())
}

/// Only exposed settings may be changed. The current file is re-read while the
/// shared config lock is held, so unrelated keys survive a save.
#[tauri::command]
pub fn set_setting(runtime: State<'_, Runtime>, key: String, value: Value) -> Result<Snapshot, String> {
    match key.as_str() {
        "recording_mode" if value == "hold" || value == "toggle" => {},
        "hotkey" => {
            let hotkey = value.as_str().ok_or("Expected a hotkey")?;
            crate::hotkey::parse(hotkey)?;
        },
        "mic_device" if value.is_string() => {
            if value.as_str().is_some_and(|s| s.chars().count() > MAX_MIC_NAME_CHARS) {
                return Err("That microphone name is too long".into());
            }
        },
        "suppress_hotkey" | "remove_fillers" | "auto_capitalize" | "code_mode" |
        "trailing_space" | "launch_on_startup" | "auto_vocabulary" if value.is_boolean() => {},
        _ => return Err("Unsupported setting or value".into()),
    }
    let mut guard = runtime.config.lock().unwrap_or_else(|p| p.into_inner());
    if key == "launch_on_startup" {
        // Refuse before touching the registry if the settings file is broken.
        Config::load_checked()?;
        set_autostart(value.as_bool().unwrap_or(false))?;
    }
    let cfg = Config::set_in_file(store::config_dir().join("config.json"), &key, value)?;
    match key.as_str() {
        "recording_mode" => {
            let mode = cfg.recording_mode();
            *runtime.core.recording_mode.lock().unwrap_or_else(|p| p.into_inner()) = mode.clone();
            crate::tray::sync_mode(&mode);
        },
        "remove_fillers" | "auto_capitalize" | "code_mode" | "trailing_space" => rebuild_post(&cfg, &runtime),
        _ => {},
    }
    *guard = cfg;
    drop(guard);
    Ok(get_snapshot(runtime))
}

#[derive(Serialize)]
pub struct VocabItem {
    term: String,
    source: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

/// Manual terms first (source `manual`), then non-retired automatic terms,
/// then the automatic replacements (`auto-fix`).
#[tauri::command]
pub fn get_vocabulary(runtime: State<'_, Runtime>) -> Vec<VocabItem> {
    let cfg = Config::load();
    let pool = Pool::load();
    let mut items = Vec::new();
    for term in cfg.hot_words() {
        let status = pool
            .get(&term)
            .map(|t| t.status.clone())
            .unwrap_or_else(|| "active".to_string());
        items.push(VocabItem { term, source: "manual".to_string(), status, key: None });
    }
    for t in &pool.terms {
        if t.source != "manual" && t.status != "retired" {
            items.push(VocabItem {
                term: t.term.clone(),
                source: t.source.clone(),
                status: t.status.clone(),
                key: None,
            });
        }
    }
    let auto = runtime.auto.lock().unwrap_or_else(|p| p.into_inner());
    for (key, meant) in auto.entries() {
        items.push(VocabItem {
            term: format!("{key} → {meant}"),
            source: "auto-fix".to_string(),
            status: "active".to_string(),
            key: Some(key.clone()),
        });
    }
    items
}

/// Remove an automatic replacement: the key is remembered in `removed` and the
/// post-processor is rebuilt.
#[tauri::command]
pub fn remove_auto_fix(runtime: State<'_, Runtime>, key: String) -> Result<(), String> {
    {
        let mut auto = runtime.auto.lock().unwrap_or_else(|p| p.into_inner());
        if !auto.remove(&key) {
            return Err("No such automatic replacement".into());
        }
        auto.save()?;
    }
    let cfg = Config::load();
    rebuild_post(&cfg, &runtime);
    Ok(())
}

#[tauri::command]
pub fn edit_vocabulary(runtime: State<'_, Runtime>, term: String, add: bool) -> Result<Vec<String>, String> {
    let mut guard = runtime.config.lock().unwrap_or_else(|p| p.into_inner());
    let config_path = store::config_dir().join("config.json");
    let key = term.trim().to_lowercase();
    let was_manual = Config::load_from(config_path.clone())
        .map(|c| c.hot_words().iter().any(|w| w.trim().to_lowercase() == key))
        .unwrap_or(false);
    let words = store::edit_vocabulary(config_path, &term, add)?;
    {
        let now = vocab::now_epoch();
        let mut pool = runtime.core.pool.lock().unwrap_or_else(|p| p.into_inner());
        if add {
            // Typing it by hand makes it manual even if it was automatic.
            pool.upsert_manual(&term, now);
        } else if was_manual {
            // A term removed from `hot_words` loses its pool entry.
            pool.remove(&term);
        } else {
            // Automatic terms are retired for good.
            pool.retire(&term);
        }
        pool.maintain(now);
        if pool.dirty() {
            if let Err(e) = pool.save() {
                crate::log::warn(&format!("vocabulary not saved: {e}"));
            }
        }
    }
    let cfg = Config::load();
    rebuild_stt(&cfg, &runtime)?;
    *guard = cfg;
    Ok(words)
}

#[tauri::command]
pub fn fix_word(runtime: State<'_, Runtime>, wrong: String, right: String) -> Result<(), String> {
    let mut guard = runtime.config.lock().unwrap_or_else(|p| p.into_inner());
    store::save_correction_at(
        store::config_dir().join("config.json"),
        store::config_dir().join("dictionary.json"),
        crate::DICTIONARY_JSON, &wrong, &right,
    )?;
    let cfg = Config::load();
    rebuild_post(&cfg, &runtime);
    *guard = cfg;
    Ok(())
}

#[tauri::command]
pub fn save_xai_key(runtime: State<'_, Runtime>, key: String) -> Result<(), String> {
    if key.chars().count() > MAX_KEY_CHARS {
        return Err("That API key is too long".into());
    }
    store::write_xai_key(&key)?;
    let cfg = Config::load();
    rebuild_stt(&cfg, &runtime)
}

// Async so device enumeration runs off the main event loop.
#[tauri::command]
pub async fn list_microphones() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if !names.contains(&name) { names.push(name); }
            }
        }
    }
    names
}

#[tauri::command]
pub fn copy_text(runtime: State<'_, Runtime>, text: String) -> Result<(), String> {
    runtime.core.injector.copy_to_clipboard(&text)
}

#[tauri::command]
pub fn set_paused(runtime: State<'_, Runtime>, paused: bool) -> bool {
    runtime.core.paused.store(paused, Ordering::SeqCst);
    crate::state::cancel_if_paused(&runtime.core);
    crate::tray::sync_paused(paused);
    emit_status(if paused { "Paused" } else { "Ready" });
    paused
}

// Must be async: building a webview window inside a synchronous command
// deadlocks on Windows (the window appears but stays blank and frozen).
#[tauri::command]
pub async fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings.html".into()))
        .title("NVG Voice Settings")
        .inner_size(520.0, 600.0)
        .min_inner_size(400.0, 440.0)
        .build().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn close_settings(app: AppHandle) {
    if let Some(win) = app.get_webview_window("settings") { let _ = win.destroy(); }
}

#[tauri::command]
pub fn open_log_folder() -> Result<(), String> {
    let dir = crate::log::log_path().parent().ok_or("Log folder unavailable")?.to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer.exe").arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) { app.exit(0); }

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APP_NAME: &str = "NVGVoice";

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

fn get_autostart() -> bool {
    use windows_sys::Win32::System::Registry::*;
    let mut key = std::ptr::null_mut();
    let path = wide(RUN_KEY);
    let name = wide(APP_NAME);
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_QUERY_VALUE, &mut key) != 0 { return false; }
        let found = RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0;
        RegCloseKey(key);
        found
    }
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::*;
    let mut key = std::ptr::null_mut();
    let path = wide(RUN_KEY);
    let name = wide(APP_NAME);
    // Resolve the exe before opening the key, so no error path leaks it.
    let quoted = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Some(wide(&format!("\"{}\"", exe.display())))
    } else {
        None
    };
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_SET_VALUE, &mut key) != 0 {
            return Err("Could not open the Windows startup setting".into());
        }
        let result = if let Some(quoted) = &quoted {
            RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, quoted.as_ptr() as *const u8, (quoted.len() * 2) as u32)
        } else { RegDeleteValueW(key, name.as_ptr()) };
        RegCloseKey(key);
        if result != 0 && (enabled || result != windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND) {
            return Err("Could not update the Windows startup setting".into());
        }
    }
    Ok(())
}
