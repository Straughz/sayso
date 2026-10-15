//! Sayso - push-to-talk dictation core.
//!
//! Tray app with the native pill and working push-to-talk dictation. Reads the
//! Python app's config, DPAPI key and history files. No flyout content yet.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod commands;
mod hotkey;
mod inject;
mod log;
mod pill;
mod post;
mod review;
mod state;
mod store;
mod stt;
mod tray;
mod vocab;
mod words;

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex, RwLock};

use tauri::RunEvent;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::audio::AudioController;
use crate::inject::Win32TextInjector;
use crate::post::PostProcessor;
use crate::state::{Core, Event};
use crate::stt::{LiveStt, MissingKeyStt, SttEngine, XaiStt};
use crate::store::{Config, HistoryStore};

const DICTIONARY_JSON: &str = include_str!("../../config/dictionary.json");
const SNIPPETS_JSON: &str = include_str!("../../config/snippets.json");

fn main() {
    // reqwest is built with rustls-no-provider; install the ring provider
    // before any network use.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let migration_lines = store::migrate_legacy_dirs();
    let _ = std::fs::create_dir_all(log::log_path().parent().unwrap_or(std::path::Path::new(".")));
    log::info("process_start");
    for line in migration_lines {
        log::info(&line);
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    if !single_instance() {
        log::info("second_instance_exit");
        return;
    }

    // A settings file that does not parse is reported once here; the app runs
    // on defaults and every writer refuses to replace the file.
    let cfg = match Config::load_checked() {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error(&format!("config.json not loaded, running on defaults and not saving settings: {e}"));
            Config::load()
        }
    };
    let hotkey_setting = cfg.hotkey();
    let parsed = match hotkey::parse(&hotkey_setting) {
        Ok(h) => h,
        Err(e) => {
            log::warn(&format!("bad hotkey {hotkey_setting:?} ({e}); using Ctrl+Space"));
            hotkey::parse("Ctrl+Space").unwrap_or(hotkey::Hotkey { modifiers: 0x0002, vk: 0x20 })
        }
    };

    // Pill + mic level channel.
    let (level_tx, level_rx) = mpsc::sync_channel::<f32>(4096);
    let pill = pill::spawn(level_rx);

    // Event channel: hotkey and audio thread -> state machine.
    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    let ev_tx_hotkey = ev_tx.clone();
    let ev_tx_audio = ev_tx.clone();

    let audio: AudioController = audio::spawn(
        cfg.mic_device(),
        level_tx,
        Arc::new(move |msg| {
            let _ = ev_tx_audio.send(Event::AudioError(msg));
        }),
    );
    let paused = Arc::new(AtomicBool::new(false));
    hotkey::spawn(parsed, cfg.suppress_hotkey(), Arc::clone(&paused), ev_tx_hotkey);

    let stt: Arc<dyn SttEngine> = match store::read_xai_key() {
        Some(key) => match XaiStt::new(key, cfg.vad_threshold()) {
            Ok(engine) => {
                log::info("xAI STT engine ready");
                Arc::new(engine)
            }
            Err(e) => {
                log::error(&format!("xAI engine init failed: {e}"));
                Arc::new(MissingKeyStt)
            }
        },
        None => {
            log::warn("no xAI API key configured");
            Arc::new(MissingKeyStt)
        }
    };
    let live_stt = Arc::new(LiveStt::new(stt));

    // Loaded once; a broken file is logged here and never overwritten.
    let auto = Arc::new(Mutex::new(review::AutoDictionary::load()));
    let auto_json = {
        let guard = auto.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries_json()
    };
    let post = Arc::new(RwLock::new(PostProcessor::from_config(&cfg, auto_json.as_deref())));

    let injector = Arc::new(Win32TextInjector::new(
        5,
        cfg.clipboard_threshold(),
        cfg.restore_clipboard(),
        1.5,
    ));
    let history = Arc::new(HistoryStore::new(cfg.history_enabled()));
    let recording_mode = Arc::new(Mutex::new(cfg.recording_mode()));
    // Loaded once; a broken file is logged here and never overwritten.
    let pool = Arc::new(Mutex::new(vocab::Pool::load()));
    let cfg = Arc::new(Mutex::new(cfg));

    let core = Arc::new(Core::new(
        Arc::clone(&post),
        injector,
        Arc::clone(&live_stt) as Arc<dyn SttEngine>,
        Arc::clone(&history),
        audio,
        pill,
        Arc::clone(&recording_mode),
        Arc::clone(&paused),
        Arc::clone(&pool),
    ));
    // Workspace names, at start-up + 30 s and then daily, only when enabled.
    vocab::spawn_harvester(Arc::clone(&pool));
    // Background review: first wake after 10 minutes, then hourly. It runs
    // only while `auto_vocabulary` is on.
    {
        let core_for_idle = Arc::clone(&core);
        review::spawn_reviewer(review::ReviewDeps {
            pool: Arc::clone(&pool),
            auto: Arc::clone(&auto),
            post: Arc::clone(&post),
            stt: Arc::clone(&live_stt) as Arc<dyn SttEngine>,
            history: Arc::clone(&history),
            idle: Arc::new(move || core_for_idle.status() == state::AppState::Idle),
        });
    }
    {
        let core = Arc::clone(&core);
        if let Err(e) = std::thread::Builder::new()
            .name("vox-coordinator".into())
            .spawn(move || state::run(ev_rx, core))
        {
            log::error(&format!("coordinator thread spawn failed: {e}"));
        }
    }

    let cfg_for_tray = Arc::clone(&cfg);
    let mode_for_tray = Arc::clone(&recording_mode);
    let paused_for_tray = Arc::clone(&paused);
    let runtime = commands::Runtime {
        core: Arc::clone(&core),
        stt: live_stt,
        config: Arc::clone(&cfg),
        auto: Arc::clone(&auto),
    };

    let app = match tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            tray::report_ready,
            tray::close_flyout,
            commands::get_snapshot,
            commands::get_history,
            commands::set_setting,
            commands::get_vocabulary,
            commands::edit_vocabulary,
            commands::remove_auto_fix,
            commands::fix_word,
            commands::save_xai_key,
            commands::list_microphones,
            commands::copy_text,
            commands::set_paused,
            commands::open_settings,
            commands::close_settings,
            commands::open_log_folder,
            commands::quit_app
        ])
        .manage(runtime)
        .setup(move |app| {
            let handle = app.handle().clone();
            commands::init_events(handle.clone());
            tray::build(
                &handle,
                Arc::clone(&cfg_for_tray),
                Arc::clone(&mode_for_tray),
                Arc::clone(&paused_for_tray),
            )?;
            log::info("tray ready");
            Ok(())
        })
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            log::error(&format!("app build failed: {e}"));
            return;
        }
    };

    app.run(|_app, event| {
        if let RunEvent::ExitRequested { code, api, .. } = event {
            // Tray-only app: closing the flyout window must not exit.
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

fn single_instance() -> bool {
    let name: Vec<u16> = "Sayso_SingleInstance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return false;
        }
        // Intentionally leak the handle so the mutex lives for the process.
        true
    }
}
