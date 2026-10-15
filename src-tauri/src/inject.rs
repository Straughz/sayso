//! Win32 text injection via `SendInput` and the clipboard. Port of
//! `src/vox/inject/win32.py`.
//!
//! Normal dictation is typed one Unicode character per `SendInput` call (see
//! `inject_sendinput`). The clipboard + Ctrl+V path is used for multi-line
//! text, text over `clipboard_threshold` characters, and the `PASTE_APPS`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VK_BACK, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V, VK_Z,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::log;

const CF_UNICODETEXT: u32 = 13;

/// Pill messages for a failed injection (no dictated text in them).
pub const FOCUS_CHANGED: &str = "Focus changed: text is in history";
pub const TYPE_FAILED: &str = "Could not type here";

/// A spoken "undo" acts only on an injection this recent, in the same window.
const UNDO_WINDOW: Duration = Duration::from_secs(30);
/// Compare the foreground window with the target every this many characters.
const FOCUS_CHECK_EVERY: usize = 32;
/// Wait at most this long for Ctrl/Shift/Alt/Win to be released.
const MODIFIER_WAIT: Duration = Duration::from_millis(1500);
/// The clipboard is not preserved when its saved copy would exceed this.
const MAX_CLIPBOARD_SAVE: usize = 64 * 1024 * 1024;
/// Apps where typed Unicode key events arrive corrupted no matter the pace.
/// Windows 11 Notepad resolves them late and in bursts, so 211 characters
/// typed 1.5 ms apart came out as long runs of one letter (measured
/// 2026-10-04). These get clipboard + Ctrl+V, which they handle correctly.
/// Terminals are the opposite case (Ctrl+V is not paste there), so typing
/// stays the default for everything not listed.
const PASTE_APPS: &[&str] = &["notepad.exe"];

/// Gap between typed characters; see `inject_sendinput`.
const CHAR_PACE: Duration = Duration::from_micros(900);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    SendInput,
    Clipboard,
}

struct UndoState {
    method: Option<Method>,
    len: usize,
    at: Instant,
    /// Foreground window that received the injection (as an integer so the
    /// state stays `Send`); 0 when unknown.
    hwnd: isize,
}

impl UndoState {
    fn none() -> Self {
        UndoState { method: None, len: 0, at: Instant::now(), hwnd: 0 }
    }
}

/// One saved clipboard format: its id and a copy of its global-memory bytes.
type ClipItem = (u32, Vec<u8>);

pub struct Win32TextInjector {
    delay: Duration,
    use_clipboard_fallback: bool,
    threshold: usize,
    restore_clipboard: bool,
    restore_delay: Duration,
    state: Mutex<UndoState>,
    clip_lock: Mutex<()>,
    restore_gen: AtomicU64,
}

impl Win32TextInjector {
    pub fn copy_to_clipboard(&self, text: &str) -> Result<(), String> {
        self.set_clipboard(text)
    }
    pub fn new(
        char_delay_ms: u64,
        clipboard_threshold: usize,
        restore_clipboard: bool,
        restore_delay_s: f64,
    ) -> Self {
        Win32TextInjector {
            delay: Duration::from_millis(char_delay_ms),
            use_clipboard_fallback: true,
            threshold: clipboard_threshold,
            restore_clipboard,
            restore_delay: Duration::from_secs_f64(restore_delay_s.max(0.0)),
            state: Mutex::new(UndoState::none()),
            clip_lock: Mutex::new(()),
            restore_gen: AtomicU64::new(0),
        }
    }

    /// Type or paste `text` into `target` (the foreground window captured at
    /// release; 0 skips the focus check). On error nothing is recorded for
    /// undo and the returned message is meant for the pill.
    pub fn inject(self: &Arc<Self>, text: &str, target: isize) -> Result<(), &'static str> {
        if text.is_empty() {
            return Ok(());
        }
        // Cancel any pending restore from a previous injection, and forget
        // the previous injection: an undo must never act on stale state.
        self.restore_gen.fetch_add(1, Ordering::SeqCst);
        self.clear_undo();

        wait_modifiers_up();
        if focus_moved(target) {
            return Err(FOCUS_CHANGED);
        }

        let multiline = text.contains('\n') || text.contains('\r');
        let chars = text.chars().count();
        let paste_app = foreground_exe().is_some_and(|exe| PASTE_APPS.contains(&exe.as_str()));
        if paste_app {
            log::info("foreground app is on the paste list — pasting instead of typing");
        }
        let use_clip = self.use_clipboard_fallback
            && (chars > self.threshold || multiline || paste_app);
        let method = if use_clip {
            self.inject_clipboard(text)?;
            Method::Clipboard
        } else {
            self.inject_sendinput(text, target)?;
            Method::SendInput
        };
        let hwnd = if target != 0 { target } else { foreground_window() };
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *st = UndoState { method: Some(method), len: chars, at: Instant::now(), hwnd };
        Ok(())
    }

    fn clear_undo(&self) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = UndoState::none();
    }

    /// Undo the last injection, but only when it is under `UNDO_WINDOW` old
    /// and the window that received it is still in front. Returns true when
    /// an undo was actually sent.
    pub fn undo_recent(&self) -> bool {
        let (method, len, at, hwnd) = {
            let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            (st.method, st.len, st.at, st.hwnd)
        };
        let Some(method) = method else {
            return false;
        };
        if at.elapsed() > UNDO_WINDOW || hwnd == 0 || foreground_window() != hwnd {
            return false;
        }
        self.clear_undo();
        wait_modifiers_up();
        match method {
            Method::Clipboard => self.send_ctrl_key(VK_Z),
            Method::SendInput => {
                let mut sent_any = false;
                for _ in 0..len {
                    if !self.send_key(VK_BACK) {
                        break;
                    }
                    sent_any = true;
                    if !self.delay.is_zero() {
                        std::thread::sleep(self.delay);
                    }
                }
                sent_any
            }
        }
    }

    // ── SendInput path ─────────────────────────────────────────────────

    /// One character per `SendInput` call, paced about 0.9 ms apart.
    ///
    /// A Unicode key event carries no character of its own once queued: the
    /// receiving app resolves it when it translates the message, and if several
    /// are waiting it reads a later character for all of them ("eesting", or a
    /// run of the final space). Measured 2026-10-04: 16 characters in one call
    /// did this in Notepad. The Python app was only safe because its own
    /// keyboard hook slowed each event down to about this pace.
    fn inject_sendinput(&self, text: &str, target: isize) -> Result<(), &'static str> {
        let mut warned = false;
        for (n, c) in text.chars().enumerate() {
            if n > 0 && n % FOCUS_CHECK_EVERY == 0 && focus_moved(target) {
                log::warn(&format!("focus changed after {n} characters; typing stopped"));
                return Err(FOCUS_CHANGED);
            }
            let mut events: Vec<INPUT> = Vec::with_capacity(4);
            for unit in c.encode_utf16(&mut [0u16; 2]).iter() {
                events.push(make_key_event(*unit, false));
                events.push(make_key_event(*unit, true));
            }
            let sent = unsafe {
                SendInput(
                    events.len() as u32,
                    events.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                )
            };
            if sent == 0 {
                // Blocked (elevated window, secure desktop): stop at once.
                log::warn(&format!("SendInput sent nothing after {n} characters; typing stopped"));
                return Err(TYPE_FAILED);
            }
            if sent != events.len() as u32 && !warned {
                log::warn(&format!("SendInput short write: sent={sent}/{}", events.len()));
                warned = true;
            }
            pace(CHAR_PACE);
        }
        Ok(())
    }

    // ── Clipboard path ─────────────────────────────────────────────────

    fn inject_clipboard(self: &Arc<Self>, text: &str) -> Result<(), &'static str> {
        let saved = if self.restore_clipboard {
            match self.save_clipboard() {
                Ok(items) => Some(items),
                Err(e) => {
                    log::warn(&format!("clipboard could not be preserved ({e}); pasting anyway"));
                    None
                }
            }
        } else {
            None
        };
        if let Err(e) = self.set_clipboard(text) {
            log::error(&format!("clipboard set failed: {e}"));
            return Err(TYPE_FAILED);
        }
        // Restore only if nobody has touched the clipboard after our own set.
        let seq = unsafe { GetClipboardSequenceNumber() };
        std::thread::sleep(Duration::from_millis(50));
        let pasted = self.send_ctrl_key(VK_V);
        if let Some(prev) = saved {
            self.schedule_restore(prev, seq);
        }
        if pasted { Ok(()) } else { Err(TYPE_FAILED) }
    }

    fn schedule_restore(self: &Arc<Self>, items: Vec<ClipItem>, seq: u32) {
        let gen = self.restore_gen.load(Ordering::SeqCst);
        let delay = self.restore_delay;
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("clipboard-restore".into())
            .spawn(move || {
                std::thread::sleep(delay);
                if this.restore_gen.load(Ordering::SeqCst) != gen {
                    return; // superseded by a newer injection
                }
                if unsafe { GetClipboardSequenceNumber() } != seq {
                    log::info("clipboard changed after the paste; not restoring it");
                    return;
                }
                if let Err(e) = this.restore_saved(&items) {
                    log::error(&format!("clipboard restore failed: {e}"));
                }
            });
        if let Err(e) = spawned {
            log::error(&format!("clipboard restore thread failed to start: {e}"));
        }
    }

    /// Copy every global-memory clipboard format (text, rich text, HTML,
    /// `CF_DIB`/`CF_DIBV5` images, `CF_HDROP` file lists, registered
    /// formats). GDI-handle formats are skipped; the DIB carries the image.
    fn save_clipboard(&self) -> Result<Vec<ClipItem>, String> {
        let _guard = self.clip_lock.lock().map_err(|_| "clipboard lock poisoned".to_string())?;
        if !open_clipboard() {
            return Err("the clipboard is busy".into());
        }
        let mut items: Vec<ClipItem> = Vec::new();
        let mut total = 0usize;
        let mut too_big = false;
        let mut format = 0u32;
        unsafe {
            loop {
                format = EnumClipboardFormats(format);
                if format == 0 {
                    break;
                }
                if !is_global_memory_format(format) {
                    continue;
                }
                let handle = GetClipboardData(format) as HGLOBAL;
                if handle.is_null() {
                    continue;
                }
                let size = GlobalSize(handle);
                if size == 0 {
                    continue;
                }
                total = total.saturating_add(size);
                if total > MAX_CLIPBOARD_SAVE {
                    too_big = true;
                    break;
                }
                let ptr = GlobalLock(handle) as *const u8;
                if ptr.is_null() {
                    continue;
                }
                let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
                GlobalUnlock(handle);
                items.push((format, bytes));
            }
            CloseClipboard();
        }
        if too_big {
            return Err("it holds more than 64 MB".into());
        }
        Ok(items)
    }

    /// Put the saved formats back (an empty list leaves the clipboard empty,
    /// as it was).
    fn restore_saved(&self, items: &[ClipItem]) -> Result<(), String> {
        let _guard = self.clip_lock.lock().map_err(|_| "clipboard lock poisoned".to_string())?;
        if !open_clipboard() {
            return Err("OpenClipboard failed".into());
        }
        let mut failed = 0usize;
        unsafe {
            if EmptyClipboard() == 0 {
                CloseClipboard();
                return Err("EmptyClipboard failed".into());
            }
            for (format, bytes) in items {
                let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
                if hmem.is_null() {
                    failed += 1;
                    continue;
                }
                let ptr = GlobalLock(hmem) as *mut u8;
                if ptr.is_null() {
                    GlobalFree(hmem);
                    failed += 1;
                    continue;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
                GlobalUnlock(hmem);
                // Ownership of hmem transfers to the system only on success.
                if SetClipboardData(*format, hmem).is_null() {
                    GlobalFree(hmem);
                    failed += 1;
                }
            }
            CloseClipboard();
        }
        if failed > 0 {
            return Err(format!("{failed} of {} formats could not be put back", items.len()));
        }
        Ok(())
    }

    fn set_clipboard(&self, text: &str) -> Result<(), String> {
        let mut data: Vec<u16> = text.encode_utf16().collect();
        data.push(0);
        let bytes = data.len() * 2;
        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if hmem.is_null() {
                return Err("GlobalAlloc failed".into());
            }
            let ptr = GlobalLock(hmem) as *mut u8;
            if ptr.is_null() {
                GlobalFree(hmem);
                return Err("GlobalLock failed".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, ptr, bytes);
            GlobalUnlock(hmem);

            let _guard = match self.clip_lock.lock() {
                Ok(g) => g,
                Err(_) => {
                    GlobalFree(hmem);
                    return Err("clipboard lock poisoned".into());
                }
            };
            if !open_clipboard() {
                GlobalFree(hmem);
                return Err("OpenClipboard failed".into());
            }
            if EmptyClipboard() == 0 {
                GlobalFree(hmem);
                CloseClipboard();
                return Err("EmptyClipboard failed".into());
            }
            // Ownership of hmem transfers to the system only on success.
            if SetClipboardData(CF_UNICODETEXT, hmem).is_null() {
                GlobalFree(hmem);
                CloseClipboard();
                return Err("SetClipboardData failed".into());
            }
            CloseClipboard();
        }
        Ok(())
    }

    // ── Key helpers ────────────────────────────────────────────────────

    /// True when Windows accepted at least one event.
    fn send_key(&self, vk: u16) -> bool {
        let events = [make_vk_event(vk, false), make_vk_event(vk, true)];
        let sent = unsafe { SendInput(2, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if sent != 2 {
            log::warn(&format!("key short write: sent={sent}/2 vk={vk:#X}"));
        }
        sent > 0
    }

    /// True when Windows accepted at least one event.
    fn send_ctrl_key(&self, vk: u16) -> bool {
        let events = [
            make_vk_event(VK_CONTROL, false),
            make_vk_event(vk, false),
            make_vk_event(vk, true),
            make_vk_event(VK_CONTROL, true),
        ];
        let sent = unsafe { SendInput(4, events.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if sent != 4 {
            log::warn(&format!("Ctrl+key short write: sent={sent}/4 vk={vk:#X}"));
        }
        sent > 0
    }
}

/// `OpenClipboard` with up to 10 tries 20 ms apart: clipboard managers and
/// RDP hold it open for a moment.
fn open_clipboard() -> bool {
    for attempt in 0..10 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    false
}

/// Formats whose clipboard handle is an `HGLOBAL` we can copy byte for byte.
/// Excluded: `CF_BITMAP` (2), `CF_METAFILEPICT` (3), `CF_PALETTE` (9),
/// `CF_ENHMETAFILE` (14), `CF_OWNERDISPLAY` (0x80), the display variants
/// `CF_DSPBITMAP` (0x82), `CF_DSPMETAFILEPICT` (0x83), `CF_DSPENHMETAFILE`
/// (0x8E), and the private / GDI-object ranges 0x200-0x3FF.
fn is_global_memory_format(format: u32) -> bool {
    !matches!(format, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E) && !(0x200..=0x3FF).contains(&format)
}

/// Foreground window as an integer (0 when there is none).
pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

fn focus_moved(target: isize) -> bool {
    target != 0 && foreground_window() != target
}

fn key_is_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

/// Typing or pasting while a modifier is still held turns characters into
/// shortcuts (Ctrl+V into Win+Ctrl+V, for example). Wait for all of them to
/// be released, polling every 10 ms, for at most 1.5 s.
fn wait_modifiers_up() {
    let t0 = Instant::now();
    loop {
        let held = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|&vk| key_is_down(vk));
        if !held {
            return;
        }
        if t0.elapsed() >= MODIFIER_WAIT {
            log::warn("modifier keys still held after 1.5 s; injecting anyway");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait without `sleep`: the default Windows timer tick is about 15 ms,
/// which would make a long dictation take half a minute.
fn pace(gap: Duration) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < gap {
        std::thread::yield_now();
    }
}

/// Lower-cased file name of the process owning `hwnd` (0 = none). Pure lookup:
/// no window is touched, so it is safe to call for a window captured earlier.
pub fn exe_of(hwnd: isize) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if hwnd == 0 {
        return None;
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd as *mut core::ffi::c_void, &mut pid);
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(|n| n.to_lowercase())
    }
}

/// Lower-cased file name of the foreground window's process.
fn foreground_exe() -> Option<String> {
    exe_of(foreground_window())
}

fn make_key_event(scan: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    make_input(0, scan, flags)
}

fn make_vk_event(vk: u16, key_up: bool) -> INPUT {
    let flags = if key_up { KEYEVENTF_KEYUP } else { 0 };
    make_input(vk, 0, flags)
}

fn make_input(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Exposed for unit tests: whether an injection would take the clipboard path.
#[cfg(test)]
pub fn uses_clipboard(text: &str, threshold: usize) -> bool {
    let multiline = text.contains('\n') || text.contains('\r');
    text.chars().count() > threshold || multiline
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live probe: types into whatever window has focus. Run by hand only.
    #[test]
    #[ignore]
    fn live_type_probe() {
        std::thread::sleep(Duration::from_secs(3));
        let inj = Arc::new(Win32TextInjector::new(5, 20000, true, 1.5));
        let _ = inj.inject("Testing one, two. Testing. ", 0);
    }

    #[test]
    fn gdi_handle_formats_are_not_copied() {
        for f in [2u32, 3, 9, 14, 0x80, 0x82, 0x83, 0x8E, 0x200, 0x2FF, 0x300, 0x3FF] {
            assert!(!is_global_memory_format(f), "format {f:#X}");
        }
        // CF_TEXT, CF_DIB, CF_UNICODETEXT, CF_HDROP, CF_LOCALE, CF_DIBV5, registered.
        for f in [1u32, 8, 13, 15, 16, 17, 0xC0FF] {
            assert!(is_global_memory_format(f), "format {f:#X}");
        }
    }

    #[test]
    fn short_single_line_types() {
        assert!(!uses_clipboard("hello world", 20000));
    }

    #[test]
    fn long_text_uses_clipboard() {
        let long = "a".repeat(20001);
        assert!(uses_clipboard(&long, 20000));
    }

    #[test]
    fn threshold_is_exclusive() {
        let exact = "a".repeat(20000);
        assert!(!uses_clipboard(&exact, 20000));
    }

    #[test]
    fn multiline_uses_clipboard() {
        assert!(uses_clipboard("line one\nline two", 20000));
        assert!(uses_clipboard("line one\r\nline two", 20000));
    }

    #[test]
    fn char_count_is_code_points() {
        // 20 astral emoji = 20 code points but 40 UTF-16 units.
        let s = "\u{1F600}".repeat(20);
        assert!(!uses_clipboard(&s, 20));
        assert!(uses_clipboard(&s, 19));
    }
}
