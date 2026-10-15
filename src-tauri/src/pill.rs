//! Native "pill" overlay: a single Win32 layered window drawn with tiny-skia
//! and presented per-pixel via `UpdateLayeredWindow`. No web view is involved.
//!
//! The window is created once at startup on its own thread and stays hidden.
//! State changes are posted from any thread. A ~30 fps timer runs only while
//! the pill is visible. Listening shows live mic bars, processing shows a
//! subtle three-dot animation, done/error show a short state label (error
//! carries the short message) and then auto-hide.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    GetDC, ReleaseDC, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FW_NORMAL, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, UpdateLayeredWindow, HWND_TOPMOST, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, ULW_ALPHA,
    WM_DESTROY, WM_ERASEBKGND, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const LOGICAL_W: f32 = 200.0;
const LOGICAL_H: f32 = 44.0;
const BARS: usize = 24;
const TIMER_ID: usize = 1;
const FRAME_MS: u32 = 16;
/// The waveform scrolls one bar per this interval, whatever the frame rate.
const SCROLL_MS: u64 = 33;
const BOTTOM_MARGIN: f32 = 72.0;
const DONE_MS: u64 = 850;
const FADE_IN_MS: f32 = 110.0;
const FADE_OUT_MS: f32 = 170.0;
// Capsule width per state (logical px); the capsule eases between them.
const W_LISTENING: f32 = 148.0;
const W_PROCESSING: f32 = 76.0;
const W_DONE: f32 = 58.0;
const ERROR_MS: u64 = 2400;

// Palette from docs/mockups/2026-10-03-directions/5-flyout.html.
const PANEL: (u8, u8, u8) = (0x21, 0x24, 0x27);
const STROKE: (u8, u8, u8) = (0x3a, 0x40, 0x45);
const ACCENT: (u8, u8, u8) = (0x3f, 0xd0, 0xc0);
const ERROR: (u8, u8, u8) = (0xe5, 0x84, 0x7f);

const WM_APP_SET_STATE: u32 = 0x8000 + 3;
const WM_APP_HIDE: u32 = 0x8000 + 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PillMode {
    Idle,
    Listening,
    Processing,
    Done,
    Error,
}

struct PillState {
    mode: PillMode,
    message: String,
    since: Instant,
    expiry: Option<Instant>,
}

static STATE: OnceLock<Mutex<PillState>> = OnceLock::new();
pub static PILL_HWND: AtomicIsize = AtomicIsize::new(0);

/// Cheap copyable handle; posts state changes to the pill thread.
#[derive(Clone, Copy)]
pub struct PillHandle;

impl PillHandle {
    pub fn listening(&self) {
        set_mode(PillMode::Listening, String::new(), None);
    }

    pub fn processing(&self) {
        set_mode(PillMode::Processing, String::new(), None);
    }

    pub fn done(&self) {
        set_mode(PillMode::Done, String::new(), Some(Duration::from_millis(DONE_MS)));
    }

    pub fn error(&self, message: &str) {
        set_mode(
            PillMode::Error,
            message.to_string(),
            Some(Duration::from_millis(ERROR_MS)),
        );
    }

    pub fn idle(&self) {
        if let Some(m) = STATE.get() {
            if let Ok(mut s) = m.lock() {
                s.mode = PillMode::Idle;
                s.expiry = None;
            }
        }
        post(WM_APP_HIDE);
    }
}

fn set_mode(mode: PillMode, message: String, ttl: Option<Duration>) {
    if let Some(m) = STATE.get() {
        if let Ok(mut s) = m.lock() {
            s.mode = mode;
            s.message = message;
            s.since = Instant::now();
            s.expiry = ttl.map(|d| Instant::now() + d);
        } else {
            return;
        }
    } else {
        return;
    }
    post(WM_APP_SET_STATE);
}

fn post(msg: u32) {
    let hwnd = PILL_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            PostMessageW(hwnd as *mut c_void, msg, 0, 0);
        }
    }
}

struct PillCtx {
    hwnd: HWND,
    screen_dc: *mut c_void,
    mem_dc: *mut c_void,
    bitmap: *mut c_void,
    font: *mut c_void,
    bits: *mut u8,
    w: i32,
    h: i32,
    scale: f32,
    pos: POINT,
    pixmap: Pixmap,
    ring: Vec<f32>,
    last_level: f32,
    level_rx: Receiver<f32>,
    visible: bool,
    /// Current capsule width in device px; 0 means "just shown".
    cur_w: f32,
    last_scroll: Instant,
    shown_at: Instant,
}

unsafe impl Send for PillCtx {}

static CTX: OnceLock<Mutex<PillCtx>> = OnceLock::new();

pub fn spawn(level_rx: Receiver<f32>) -> PillHandle {
    let _ = STATE.set(Mutex::new(PillState {
        mode: PillMode::Idle,
        message: String::new(),
        since: Instant::now(),
        expiry: None,
    }));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    if let Err(e) = std::thread::Builder::new()
        .name("pill".into())
        .spawn(move || unsafe {
            if let Err(e) = run(level_rx, ready_tx) {
                crate::log::error(&format!("pill error: {e}"));
            }
        })
    {
        crate::log::error(&format!("pill thread spawn failed: {e}"));
    }
    let _ = ready_rx.recv_timeout(Duration::from_secs(5));
    PillHandle
}

unsafe fn run(level_rx: Receiver<f32>, ready_tx: std::sync::mpsc::Sender<()>) -> Result<(), String> {
    let scale = GetDpiForSystem() as f32 / 96.0;
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (LOGICAL_H * scale).round() as i32;
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let x = (sw - w) / 2;
    let y = sh - h - (BOTTOM_MARGIN * scale).round() as i32;
    let pos = POINT { x, y };

    let hinstance = GetModuleHandleW(std::ptr::null());
    let class_name = wide("SaysoPill\0");
    let window_name = wide("Sayso Pill\0");

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: class_name.as_ptr(),
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 {
        return Err("RegisterClassW failed".into());
    }

    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
        class_name.as_ptr(),
        window_name.as_ptr(),
        WS_POPUP,
        x,
        y,
        w,
        h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }

    let screen_dc = GetDC(std::ptr::null_mut());
    if screen_dc.is_null() {
        return Err("GetDC failed".into());
    }
    let mem_dc = CreateCompatibleDC(screen_dc);

    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut bits_raw: *mut c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(
        screen_dc,
        &bmi,
        DIB_RGB_COLORS,
        &mut bits_raw,
        std::ptr::null_mut(),
        0,
    );
    if bitmap.is_null() || bits_raw.is_null() {
        return Err("CreateDIBSection failed".into());
    }
    SelectObject(mem_dc, bitmap);

    let face = wide("Segoe UI\0");
    let font = CreateFontW(
        -(13.0 * scale).round() as i32,
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        ANTIALIASED_QUALITY as u32,
        DEFAULT_PITCH as u32,
        face.as_ptr(),
    );
    if !font.is_null() {
        SelectObject(mem_dc, font);
    }

    let pixmap = Pixmap::new(w as u32, h as u32).ok_or("Pixmap::new failed")?;

    let ctx = PillCtx {
        hwnd,
        screen_dc,
        mem_dc,
        bitmap,
        font,
        bits: bits_raw as *mut u8,
        w,
        h,
        scale,
        pos,
        pixmap,
        ring: vec![0.0; BARS],
        last_level: 0.0,
        level_rx,
        visible: false,
        cur_w: 0.0,
        last_scroll: Instant::now(),
        shown_at: Instant::now(),
    };
    if CTX.set(Mutex::new(ctx)).is_err() {
        return Err("pill context already set".into());
    }
    PILL_HWND.store(hwnd as isize, Ordering::SeqCst);
    let _ = ready_tx.send(());

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if let Some(mutex) = CTX.get() {
        let guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
        ReleaseDC(std::ptr::null_mut(), guard.screen_dc);
        DeleteDC(guard.mem_dc);
        DeleteObject(guard.bitmap);
        if !guard.font.is_null() {
            DeleteObject(guard.font);
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SET_STATE => {
            show(hwnd);
            0
        }
        WM_APP_HIDE => {
            hide(hwnd);
            0
        }
        WM_TIMER => {
            on_timer(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show(hwnd: HWND) {
    let mode = current_mode();
    if mode == PillMode::Idle {
        hide(hwnd);
        return;
    }
    let Some(mutex) = CTX.get() else { return };
    {
        let mut ctx = mutex.lock().unwrap_or_else(|p| p.into_inner());
        if !ctx.visible {
            ctx.cur_w = 0.0;
            ctx.shown_at = Instant::now();
            ctx.ring.iter_mut().for_each(|v| *v = 0.0);
            ctx.last_level = 0.0;
        }
        ctx.present();
        ctx.visible = true;
    }
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    SetTimer(hwnd, TIMER_ID, FRAME_MS, None);
}

unsafe fn hide(hwnd: HWND) {
    KillTimer(hwnd, TIMER_ID);
    ShowWindow(hwnd, SW_HIDE);
    if let Some(mutex) = CTX.get() {
        mutex.lock().unwrap_or_else(|p| p.into_inner()).visible = false;
    }
}

unsafe fn on_timer(hwnd: HWND) {
    let expired = STATE
        .get()
        .and_then(|m| m.lock().ok().map(|s| s.expiry.map(|e| Instant::now() >= e).unwrap_or(false)))
        .unwrap_or(false);
    if expired {
        hide(hwnd);
        return;
    }
    if let Some(mutex) = CTX.get() {
        let mut ctx = mutex.lock().unwrap_or_else(|p| p.into_inner());
        if ctx.visible {
            ctx.present();
        }
    }
}

fn current_mode() -> PillMode {
    STATE
        .get()
        .and_then(|m| m.lock().ok().map(|s| s.mode))
        .unwrap_or(PillMode::Idle)
}

/// Mode, message, time in this mode, and time left before it expires.
fn state_snapshot() -> (PillMode, String, Duration, Option<Duration>) {
    STATE
        .get()
        .and_then(|m| {
            m.lock().ok().map(|s| {
                let left = s.expiry.map(|e| e.saturating_duration_since(Instant::now()));
                (s.mode, s.message.clone(), s.since.elapsed(), left)
            })
        })
        .unwrap_or((PillMode::Idle, String::new(), Duration::ZERO, None))
}

impl PillCtx {
    fn present(&mut self) {
        let (mode, message, elapsed, left) = state_snapshot();
        if mode == PillMode::Listening {
            let mut peak = self.last_level * 0.90; // decay per 16 ms frame
            while let Ok(v) = self.level_rx.try_recv() {
                peak = peak.max(v);
            }
            self.last_level = peak.clamp(0.0, 1.0);
            if self.last_scroll.elapsed() >= Duration::from_millis(SCROLL_MS) {
                self.last_scroll = Instant::now();
                self.ring.remove(0);
                self.ring.push(self.last_level);
            } else if let Some(newest) = self.ring.last_mut() {
                *newest = self.last_level;
            }
        }
        let fade_in = (self.shown_at.elapsed().as_secs_f32() * 1000.0 / FADE_IN_MS).min(1.0);
        let fade_out = left
            .map(|d| (d.as_secs_f32() * 1000.0 / FADE_OUT_MS).min(1.0))
            .unwrap_or(1.0);
        let alpha = (fade_in * fade_out * 255.0).round().clamp(0.0, 255.0) as u8;
        self.render(mode, elapsed);
        self.blit(mode, &message, alpha);
    }

    fn render(&mut self, mode: PillMode, elapsed: Duration) {
        let w = self.w as f32;
        let h = self.h as f32;
        let s = self.scale;
        self.pixmap.fill(Color::TRANSPARENT);

        // Ease the capsule toward this state's width.
        let target = match mode {
            PillMode::Listening => W_LISTENING * s,
            PillMode::Processing => W_PROCESSING * s,
            PillMode::Done => W_DONE * s,
            PillMode::Error => w,
            PillMode::Idle => self.cur_w.max(W_DONE * s),
        };
        if self.cur_w <= 0.0 {
            self.cur_w = target * 0.86; // just shown: open with a small grow
        }
        self.cur_w += (target - self.cur_w) * 0.24;
        if (target - self.cur_w).abs() < 0.5 {
            self.cur_w = target;
        }
        let cw = self.cur_w.min(w);

        let (stroke_rgb, stroke_w) = match mode {
            PillMode::Done => (ACCENT, 1.0),
            PillMode::Error => (ERROR, 1.2),
            _ => (STROKE, 1.0),
        };

        let inset = 0.5 * s;
        let x0 = (w - cw) / 2.0 + inset;
        let x1 = (w + cw) / 2.0 - inset;
        let r = (h - 2.0 * inset) / 2.0;
        if let Some(path) = capsule_path(x0, inset, x1, h - inset, r) {
            let mut fill = Paint::default();
            fill.set_color_rgba8(PANEL.0, PANEL.1, PANEL.2, 255);
            fill.anti_alias = true;
            self.pixmap
                .fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

            let mut stroke = Paint::default();
            stroke.set_color_rgba8(stroke_rgb.0, stroke_rgb.1, stroke_rgb.2, 255);
            stroke.anti_alias = true;
            let stroke_props = Stroke {
                width: stroke_w * s,
                ..Default::default()
            };
            self.pixmap
                .stroke_path(&path, &stroke, &stroke_props, Transform::identity(), None);
        }

        match mode {
            PillMode::Listening => self.render_bars(cw),
            PillMode::Processing => self.render_dots(elapsed),
            PillMode::Done => self.render_check(elapsed),
            PillMode::Error | PillMode::Idle => {}
        }
    }

    /// Rounded bars; the newest (right) is brightest and older ones fade.
    fn render_bars(&mut self, cw: f32) {
        let w = self.w as f32;
        let h = self.h as f32;
        let s = self.scale;
        let pad = 18.0 * s;
        let bar_w = 2.5 * s;
        let avail = (cw - 2.0 * pad).max(bar_w);
        let gap = if BARS > 1 {
            ((avail - BARS as f32 * bar_w) / (BARS as f32 - 1.0)).max(0.0)
        } else {
            0.0
        };
        let left = (w - cw) / 2.0 + pad;
        let max_bar = h - 14.0 * s;
        let min_bar = bar_w;
        for (i, level) in self.ring.iter().enumerate() {
            let lv = level.clamp(0.0, 1.0);
            let bh = min_bar + lv * (max_bar - min_bar);
            let bx = left + i as f32 * (bar_w + gap);
            let by = (h - bh) / 2.0;
            let age = i as f32 / (BARS as f32 - 1.0);
            let alpha = (70.0 + 185.0 * age) as u8;
            let mut paint = Paint::default();
            paint.set_color_rgba8(ACCENT.0, ACCENT.1, ACCENT.2, alpha);
            paint.anti_alias = true;
            if let Some(path) = capsule_path(bx, by, bx + bar_w, by + bh, bar_w / 2.0) {
                self.pixmap
                    .fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    /// Three dots riding a wave: each rises and falls a beat after the last.
    fn render_dots(&mut self, elapsed: Duration) {
        let w = self.w as f32;
        let h = self.h as f32;
        let s = self.scale;
        let t = elapsed.as_secs_f32();
        let spacing = 11.0 * s;
        let cx0 = w / 2.0 - spacing;
        for k in 0..3 {
            let phase = t * std::f32::consts::TAU / 0.95 - k as f32 * 0.8;
            let wave = phase.sin();
            let cy = h / 2.0 - wave * 4.0 * s;
            let alpha = (135.0 + 120.0 * (wave + 1.0) / 2.0) as u8;
            let mut paint = Paint::default();
            paint.set_color_rgba8(ACCENT.0, ACCENT.1, ACCENT.2, alpha);
            paint.anti_alias = true;
            let mut pb = PathBuilder::new();
            pb.push_circle(cx0 + k as f32 * spacing, cy, 2.7 * s);
            if let Some(path) = pb.finish() {
                self.pixmap
                    .fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    /// A check mark that draws itself in about 220 ms.
    fn render_check(&mut self, elapsed: Duration) {
        let s = self.scale;
        let cx = self.w as f32 / 2.0;
        let cy = self.h as f32 / 2.0;
        let a = (cx - 7.0 * s, cy + 0.5 * s);
        let b = (cx - 2.5 * s, cy + 5.0 * s);
        let c = (cx + 7.0 * s, cy - 5.0 * s);
        let l1 = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let l2 = ((c.0 - b.0).powi(2) + (c.1 - b.1).powi(2)).sqrt();
        let p = (elapsed.as_secs_f32() / 0.22).min(1.0);
        let eased = 1.0 - (1.0 - p).powi(3);
        let drawn = eased * (l1 + l2);
        if drawn <= 0.0 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        if drawn <= l1 {
            let f = drawn / l1;
            pb.line_to(a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
        } else {
            let f = ((drawn - l1) / l2).min(1.0);
            pb.line_to(b.0, b.1);
            pb.line_to(b.0 + (c.0 - b.0) * f, b.1 + (c.1 - b.1) * f);
        }
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color_rgba8(ACCENT.0, ACCENT.1, ACCENT.2, 255);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: 2.2 * s,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Default::default()
            };
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }

    fn blit(&mut self, mode: PillMode, message: &str, alpha: u8) {
        let data = self.pixmap.data();
        let len = (self.w * self.h * 4) as usize;
        // tiny-skia stores premultiplied RGBA; the DIB is premultiplied BGRA.
        unsafe {
            let dst = std::slice::from_raw_parts_mut(self.bits, len);
            for i in 0..(self.w * self.h) as usize {
                let o = i * 4;
                dst[o] = data[o + 2];
                dst[o + 1] = data[o + 1];
                dst[o + 2] = data[o];
                dst[o + 3] = data[o + 3];
            }
        }
        // Only the error state carries words, and only once the capsule has
        // opened to full width (the text box must sit inside it).
        if mode == PillMode::Error && !message.is_empty() && self.cur_w >= self.w as f32 - 1.0 {
            self.draw_text(message, ERROR);
            // GDI clears the alpha byte of every pixel it touches, which would
            // punch see-through holes where the letters are. The text box lies
            // wholly inside the opaque capsule, so make it opaque again.
            let pad = (self.h as f32 / 2.0).ceil() as i32;
            let (top, bottom) = (2, self.h - 2);
            unsafe {
                let dst = std::slice::from_raw_parts_mut(self.bits, len);
                for y in top..bottom {
                    for x in pad..(self.w - pad) {
                        dst[((y * self.w + x) * 4 + 3) as usize] = 255;
                    }
                }
            }
        }
        let size = SIZE { cx: self.w, cy: self.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: 1,
        };
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                self.screen_dc,
                &self.pos,
                &size,
                self.mem_dc,
                &src,
                0,
                &blend,
                ULW_ALPHA,
            );
        }
    }

    fn draw_text(&self, text: &str, rgb: (u8, u8, u8)) {
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        buf.push(0);
        let colorref = rgb.0 as u32 | ((rgb.1 as u32) << 8) | ((rgb.2 as u32) << 16);
        let pad = (self.h as f32 / 2.0).ceil() as i32;
        let mut rect = RECT {
            left: pad,
            top: 0,
            right: self.w - pad,
            bottom: self.h,
        };
        let format = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
        unsafe {
            SetBkMode(self.mem_dc, TRANSPARENT as i32);
            SetTextColor(self.mem_dc, colorref);
            DrawTextW(self.mem_dc, buf.as_ptr(), -1, &mut rect, format);
        }
    }
}

/// Rounded-rectangle path built from cubic Beziers (kappa approximation).
fn capsule_path(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.cubic_to(x1 - r + k, y0, x1, y0 + r - k, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.cubic_to(x0 + r - k, y1, x0, y1 - r + k, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.cubic_to(x0, y0 + r - k, x0 + r - k, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
