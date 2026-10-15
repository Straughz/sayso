//! Tray icon, menu, and the placeholder flyout lifecycle (ported from the
//! spike). The flyout content is still the spike's static sample markup; the
//! real Flyout and Settings arrive in stage 2.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};

use crate::log;
use crate::store::Config;

static FLYOUT_T0: AtomicU64 = AtomicU64::new(0);
static FLYOUT_SHOWN: AtomicU64 = AtomicU64::new(0);
static FLYOUT_CLOSED: AtomicU64 = AtomicU64::new(0);
static FLYOUT_OPEN: AtomicBool = AtomicBool::new(false);
static FLYOUT_READY: AtomicBool = AtomicBool::new(false);
static FLYOUT_CLOSING: AtomicBool = AtomicBool::new(false);
static MODE_ITEM: OnceLock<CheckMenuItem<tauri::Wry>> = OnceLock::new();
static PAUSE_ITEM: OnceLock<CheckMenuItem<tauri::Wry>> = OnceLock::new();

static INSTANT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

const FLYOUT_W: f64 = 380.0;
const FLYOUT_H: f64 = 560.0;

fn now_us() -> u64 {
    INSTANT.get_or_init(Instant::now).elapsed().as_micros() as u64
}

pub fn build(
    app: &AppHandle,
    cfg: Arc<Mutex<Config>>,
    recording_mode: Arc<Mutex<String>>,
    paused: Arc<AtomicBool>,
) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let mode = CheckMenuItem::with_id(
        app,
        "mode",
        "Toggle mode",
        true,
        current_mode(&recording_mode) == "toggle",
        None::<&str>,
    )?;
    let pause =
        CheckMenuItem::with_id(app, "pause", "Pause dictation", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &mode, &pause, &quit])?;

    let mode_item = mode.clone();
    let pause_item = pause.clone();
    let _ = MODE_ITEM.set(mode.clone());
    let _ = PAUSE_ITEM.set(pause.clone());

    let builder = TrayIconBuilder::with_id("main")
        .tooltip("Sayso")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().0.as_str() {
            "open" => toggle_flyout(app),
            "mode" => {
                let current = current_mode(&recording_mode);
                let next = if current == "toggle" { "hold" } else { "toggle" };
                {
                    let mut c = cfg.lock().unwrap_or_else(|p| p.into_inner());
                    // Re-read first: the file may have changed since startup
                    // (a hand edit). A file that does not parse is never
                    // replaced; the mode stays as it was.
                    match Config::set_in_file(
                        Config::path(),
                        "recording_mode",
                        serde_json::Value::String(next.to_string()),
                    ) {
                        Ok(saved) => *c = saved,
                        Err(e) => {
                            log::error(&format!("recording mode not changed: {e}"));
                            let _ = mode_item.set_checked(current == "toggle");
                            return;
                        }
                    }
                }
                if let Ok(mut m) = recording_mode.lock() {
                    *m = next.to_string();
                }
                if let Err(e) = mode_item.set_checked(next == "toggle") {
                    log::error(&format!("tray: set_checked failed: {e}"));
                }
                log::info(&format!("recording mode → {next}"));
            }
            "pause" => {
                let next = !paused.load(Ordering::SeqCst);
                paused.store(next, Ordering::SeqCst);
                if let Err(e) = pause_item.set_checked(next) {
                    log::error(&format!("tray: pause set_checked failed: {e}"));
                }
                log::info(if next { "dictation paused" } else { "dictation resumed" });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_flyout(tray.app_handle());
            }
        });

    let builder = if let Some(icon) = app.default_window_icon().cloned() {
        builder.icon(icon)
    } else {
        builder
    };
    builder.build(app)?;
    Ok(())
}

pub fn sync_mode(mode: &str) {
    if let Some(item) = MODE_ITEM.get() { let _ = item.set_checked(mode == "toggle"); }
}

pub fn sync_paused(paused: bool) {
    if let Some(item) = PAUSE_ITEM.get() { let _ = item.set_checked(paused); }
}

fn current_mode(recording_mode: &Arc<Mutex<String>>) -> String {
    recording_mode
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "hold".to_string())
}

fn toggle_flyout(app: &AppHandle) {
    if FLYOUT_OPEN.load(Ordering::SeqCst) {
        close_flyout_inner(app);
    } else if now_us().saturating_sub(FLYOUT_CLOSED.load(Ordering::SeqCst)) > 400_000 {
        // Clicking the tray icon while the flyout is open first takes focus
        // away (which closes it) and then delivers this click. Without the
        // guard that click would open it straight back up.
        open_flyout(app);
    }
}

fn open_flyout(app: &AppHandle) {
    if app.get_webview_window("flyout").is_some() {
        return;
    }
    FLYOUT_T0.store(now_us(), Ordering::SeqCst);
    FLYOUT_READY.store(false, Ordering::SeqCst);
    FLYOUT_CLOSING.store(false, Ordering::SeqCst);

    let (x, y) = flyout_position(app);
    let built = WebviewWindowBuilder::new(app, "flyout", WebviewUrl::App("index.html".into()))
        .title("Sayso Flyout")
        .inner_size(FLYOUT_W, FLYOUT_H)
        .position(x, y)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(true)
        .build();

    match built {
        Ok(win) => {
            FLYOUT_OPEN.store(true, Ordering::SeqCst);
            FLYOUT_SHOWN.store(now_us(), Ordering::SeqCst);
            if let Ok(h) = win.hwnd() {
                set_round_corners(h.0);
            }
            let _ = win.set_focus();

            let app2 = app.clone();
            win.on_window_event(move |ev| {
                if let WindowEvent::Focused(false) = ev {
                    // The age check below covers the focus flicker during
                    // creation; also waiting for the page to report ready
                    // meant a click outside during load was ignored for good.
                    if FLYOUT_CLOSING.load(Ordering::SeqCst) {
                        return;
                    }
                    let age = now_us().saturating_sub(FLYOUT_SHOWN.load(Ordering::SeqCst));
                    if age > 250_000 {
                        close_flyout_inner(&app2);
                    }
                }
            });
        }
        Err(e) => log::error(&format!("flyout create error: {e}")),
    }
}

fn close_flyout_inner(app: &AppHandle) {
    if FLYOUT_CLOSING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(win) = app.get_webview_window("flyout") {
        let _ = win.destroy();
    }
    FLYOUT_OPEN.store(false, Ordering::SeqCst);
    FLYOUT_READY.store(false, Ordering::SeqCst);
    FLYOUT_CLOSING.store(false, Ordering::SeqCst);
    FLYOUT_CLOSED.store(now_us(), Ordering::SeqCst);
    log::info("flyout closed");
}

fn flyout_position(app: &AppHandle) -> (f64, f64) {
    if let Ok(Some(m)) = app.primary_monitor() {
        let sf = m.scale_factor();
        let wa = m.work_area();
        let left = wa.position.x as f64 / sf;
        let top = wa.position.y as f64 / sf;
        let width = wa.size.width as f64 / sf;
        let height = wa.size.height as f64 / sf;
        const MARGIN: f64 = 12.0;
        // Bottom-right, clamped so the window never starts outside the work
        // area (a work area narrower than the flyout pins it to the left/top).
        let max_x = (left + width - FLYOUT_W).max(left);
        let max_y = (top + height - FLYOUT_H).max(top);
        let x = (left + width - FLYOUT_W - MARGIN).clamp(left, max_x);
        let y = (top + height - FLYOUT_H - MARGIN).clamp(top, max_y);
        (x, y)
    } else {
        (100.0, 100.0)
    }
}

fn set_round_corners(hwnd: *mut core::ffi::c_void) {
    let pref: i32 = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[tauri::command]
pub fn report_ready() {
    let t0 = FLYOUT_T0.load(Ordering::SeqCst);
    if t0 != 0 {
        let ms = now_us().saturating_sub(t0) as f64 / 1000.0;
        log::info(&format!("flyout create dom_loaded_ms={ms:.2}"));
    }
    FLYOUT_READY.store(true, Ordering::SeqCst);
    // No set_focus here: creation already focuses the window, and taking
    // focus back after the page loads defeated a quick click outside.
}

#[tauri::command]
pub fn close_flyout(app: AppHandle) {
    close_flyout_inner(&app);
}
