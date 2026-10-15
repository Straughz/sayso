//! Global hotkey via a low-level keyboard hook (WH_KEYBOARD_LL) on its own
//! thread. Port of the parsing and edge/suppression logic in
//! `src/vox/hotkey/win32.py`, with the message-loop structure from the spike.
//!
//! The hook callback does no work beyond posting a press/release event on a
//! channel: all dictation work happens on other threads.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetTimer, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
};

use crate::state::Event;

const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;

const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_SHIFT: u16 = 0x10;
const VK_MENU: u16 = 0x12; // Alt

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u16,
}

fn mod_flag(part: &str) -> Option<u32> {
    match part {
        "CTRL" | "CONTROL" => Some(MOD_CONTROL),
        "SHIFT" => Some(MOD_SHIFT),
        "ALT" => Some(MOD_ALT),
        "WIN" => Some(MOD_WIN),
        _ => None,
    }
}

fn vk_for(part: &str) -> Option<u16> {
    Some(match part {
        "SPACE" => 0x20,
        "ENTER" => 0x0D,
        "TAB" => 0x09,
        "ESCAPE" => 0x1B,
        "WIN" => VK_LWIN,
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        _ => return None,
    })
}

const NEED_MODIFIER: &str = "Add Ctrl, Alt, Shift or Win to the shortcut";
const UNSUPPORTED_KEY: &str = "That key is not supported";
const NEED_KEY: &str = "Add a key to go with the modifier";

/// Parse `"Ctrl+Win"` → modifiers + virtual key. Mirrors `parse_hotkey`.
///
/// A single character is accepted only for A-Z and 0-9 (their character code
/// is also their virtual-key code; for anything else it is not). A shortcut
/// with no modifier is accepted only for F1-F12, because a bare letter or
/// Space would be swallowed everywhere. Modifier-only shortcuts such as
/// `Ctrl+Win` (Win as the final key) work as before.
pub fn parse(hotkey: &str) -> Result<Hotkey, String> {
    let parts: Vec<String> = hotkey.split('+').map(|p| p.trim().to_uppercase()).collect();
    let mut modifiers = 0u32;
    let mut vk = 0u16;
    for (i, part) in parts.iter().enumerate() {
        if part == "WIN" && i == parts.len() - 1 && vk == 0 {
            vk = vk_for("WIN").unwrap_or(VK_LWIN);
        } else if let Some(flag) = mod_flag(part) {
            modifiers |= flag;
        } else if let Some(v) = vk_for(part) {
            vk = v;
        } else {
            let mut chars = part.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_uppercase() || c.is_ascii_digit() => vk = c as u16,
                _ => return Err(UNSUPPORTED_KEY.into()),
            }
        }
    }
    if vk == 0 {
        return Err(NEED_KEY.into());
    }
    if modifiers == 0 && !(0x70..=0x7B).contains(&vk) {
        return Err(NEED_MODIFIER.into());
    }
    Ok(Hotkey { modifiers, vk })
}

static VK: AtomicU32 = AtomicU32::new(0);
static MODS: AtomicU32 = AtomicU32::new(0);
static SUPPRESS: AtomicBool = AtomicBool::new(true);
static PRESSED: AtomicBool = AtomicBool::new(false);
static SUPPRESSED_DOWN: AtomicBool = AtomicBool::new(false);
/// Milliseconds (since `EPOCH`) of the last real key-down of the main key,
/// auto-repeat included.
static LAST_MAIN_DOWN_MS: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();
static EVENT_TX: OnceLock<Sender<Event>> = OnceLock::new();
static PAUSED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// Re-install the hook this often: Windows silently drops a low-level hook
/// whose callback ever exceeds its timeout, and nothing reports that.
const REINSTALL_MS: u32 = 5 * 60 * 1000;
const REINSTALL_LOG_EVERY: Duration = Duration::from_secs(3600);
/// With the main key suppressed its async state is not trustworthy, so a
/// held key is recognised by its auto-repeat instead. The slowest Windows
/// setting is a 1 s initial delay, then 2.5 repeats a second.
const REPEAT_GAP_MS: u64 = 1500;

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn spawn(hotkey: Hotkey, suppress: bool, paused: Arc<AtomicBool>, tx: Sender<Event>) {
    VK.store(hotkey.vk as u32, Ordering::SeqCst);
    MODS.store(hotkey.modifiers, Ordering::SeqCst);
    SUPPRESS.store(suppress, Ordering::SeqCst);
    let _ = EVENT_TX.set(tx);
    let _ = PAUSED.set(paused);
    let _ = now_ms();
    if let Err(e) = std::thread::Builder::new().name("hotkey".into()).spawn(|| unsafe { run() }) {
        crate::log::error(&format!("hotkey thread spawn failed: {e}"));
    }
}

/// Clear the press and suppression latches. Called when the state machine
/// ends a recording on its own (lost key-up, time limit, pause), so the next
/// real press of the hotkey is seen as a press.
pub fn reset_latches() {
    PRESSED.store(false, Ordering::SeqCst);
    SUPPRESSED_DOWN.store(false, Ordering::SeqCst);
}

/// Best reading of "the hotkey's main key is no longer held", for recovering
/// a key-up the hook never saw (secure desktop, elevated window).
///
/// `GetAsyncKeyState` is the primary signal. When the hook swallowed the key
/// (suppression on), Windows may never record it as down in the async state,
/// so in that case the key also has to have stopped auto-repeating.
pub fn main_key_looks_up() -> bool {
    let vk = VK.load(Ordering::SeqCst) as u16;
    if vk == 0 || key_down(vk) {
        return false;
    }
    if SUPPRESSED_DOWN.load(Ordering::SeqCst) {
        return now_ms().saturating_sub(LAST_MAIN_DOWN_MS.load(Ordering::SeqCst)) > REPEAT_GAP_MS;
    }
    true
}

fn is_paused() -> bool {
    PAUSED.get().is_some_and(|p| p.load(Ordering::SeqCst))
}

unsafe fn install() -> HHOOK {
    let hmod = GetModuleHandleW(std::ptr::null());
    SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod, 0)
}

unsafe fn run() {
    let mut hook = install();
    if hook.is_null() {
        crate::log::error("hotkey: SetWindowsHookExW failed");
        return;
    }
    crate::log::info("hotkey: keyboard hook installed");
    // A thread timer (no window): WM_TIMER arrives in this thread's queue.
    if SetTimer(std::ptr::null_mut(), 0, REINSTALL_MS, None) == 0 {
        crate::log::warn("hotkey: could not start the hook refresh timer");
    }
    let mut last_log: Option<Instant> = None;

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            if !PRESSED.load(Ordering::SeqCst) && !SUPPRESSED_DOWN.load(Ordering::SeqCst) {
                // Install the new hook before removing the old one; both run
                // on this thread, so no event is handled while both exist.
                let fresh = install();
                let quiet = last_log.is_some_and(|t| t.elapsed() < REINSTALL_LOG_EVERY);
                if fresh.is_null() {
                    if !quiet {
                        crate::log::error("hotkey: hook refresh failed; keeping the old hook");
                        last_log = Some(Instant::now());
                    }
                } else {
                    UnhookWindowsHookEx(hook);
                    hook = fresh;
                    if !quiet {
                        crate::log::info("hotkey: keyboard hook refreshed (every 5 min; logged hourly)");
                        last_log = Some(Instant::now());
                    }
                }
            }
            continue;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    UnhookWindowsHookEx(hook);
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

/// Which modifier group a virtual key belongs to (0 for ordinary keys). The
/// hook reports left/right codes; `parse` only ever yields `VK_LWIN` here.
fn mod_class(vk: u16) -> u32 {
    match vk {
        VK_CONTROL | 0xA2 | 0xA3 => MOD_CONTROL,
        VK_SHIFT | 0xA0 | 0xA1 => MOD_SHIFT,
        VK_MENU | 0xA4 | 0xA5 => MOD_ALT,
        VK_LWIN | VK_RWIN => MOD_WIN,
        _ => 0,
    }
}

/// The required modifiers are down and no other modifier is. The main key's
/// own group is ignored (for `Ctrl+Win` the Win key is down by definition).
fn mods_match(mods: u32, main_vk: u16, held: u32) -> bool {
    let ignore = mod_class(main_vk);
    let need = mods & !ignore;
    let extra = held & !mods & !ignore;
    held & need == need && extra == 0
}

fn mods_held(mods: u32, main_vk: u16) -> bool {
    let mut held = 0u32;
    if key_down(VK_CONTROL) {
        held |= MOD_CONTROL;
    }
    if key_down(VK_SHIFT) {
        held |= MOD_SHIFT;
    }
    if key_down(VK_MENU) {
        held |= MOD_ALT;
    }
    if key_down(VK_LWIN) || key_down(VK_RWIN) {
        held |= MOD_WIN;
    }
    mods_match(mods, main_vk, held)
}

fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, VK_CONTROL | VK_SHIFT | VK_MENU | VK_LWIN | VK_RWIN)
}

fn should_suppress(vk: u16, is_down: bool) -> bool {
    if vk as u32 != VK.load(Ordering::SeqCst) {
        return false;
    }
    if is_modifier_vk(vk) {
        return false;
    }
    if !is_down {
        // keyup: the latch is authoritative and cleared on use.
        if SUPPRESSED_DOWN.swap(false, Ordering::SeqCst) {
            return true;
        }
        return false;
    }
    if SUPPRESSED_DOWN.load(Ordering::SeqCst) {
        // Auto-repeat while the key is held: stay latched.
        return true;
    }
    if !SUPPRESS.load(Ordering::SeqCst) {
        return false;
    }
    if mods_held(MODS.load(Ordering::SeqCst), vk) {
        SUPPRESSED_DOWN.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
    let vk = kb.vkCode as u16;
    let wm = wparam as u32;
    let is_down = wm == WM_KEYDOWN || wm == WM_SYSKEYDOWN;
    let is_up = wm == WM_KEYUP || wm == WM_SYSKEYUP;

    if is_down || is_up {
        // Injected input (our own Ctrl+V / Ctrl+Z / Backspace, or any other
        // program's SendInput) never drives the hotkey or its latches.
        if kb.flags & LLKHF_INJECTED != 0 {
            return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
        }
        let is_main = vk as u32 == VK.load(Ordering::SeqCst);
        if is_main && is_down {
            LAST_MAIN_DOWN_MS.store(now_ms(), Ordering::SeqCst);
        }
        // Paused with nothing latched: the hotkey is an ordinary key again.
        if is_paused()
            && is_down
            && !PRESSED.load(Ordering::SeqCst)
            && !SUPPRESSED_DOWN.load(Ordering::SeqCst)
        {
            return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
        }
        if is_main {
            if is_down {
                // Edge-triggered: auto-repeat cannot re-arm a held press.
                if !PRESSED.load(Ordering::SeqCst) && mods_held(MODS.load(Ordering::SeqCst), vk) {
                    PRESSED.store(true, Ordering::SeqCst);
                    if let Some(tx) = EVENT_TX.get() {
                        let _ = tx.send(Event::Press);
                    }
                }
            } else if PRESSED.swap(false, Ordering::SeqCst) {
                if let Some(tx) = EVENT_TX.get() {
                    let _ = tx.send(Event::Release);
                }
            }
        }
        if should_suppress(vk, is_down) {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_space() {
        assert_eq!(parse("Ctrl+Space").unwrap(), Hotkey { modifiers: MOD_CONTROL, vk: 0x20 });
    }

    #[test]
    fn parses_ctrl_win_as_terminal_win() {
        assert_eq!(parse("Ctrl+Win").unwrap(), Hotkey { modifiers: MOD_CONTROL, vk: VK_LWIN });
    }

    #[test]
    fn parses_letter_and_function_keys() {
        assert_eq!(parse("Ctrl+Shift+K").unwrap(), Hotkey { modifiers: MOD_CONTROL | MOD_SHIFT, vk: 'K' as u16 });
        assert_eq!(parse("Ctrl+F9").unwrap(), Hotkey { modifiers: MOD_CONTROL, vk: 0x78 });
    }

    #[test]
    fn rejects_unknown_and_keyless() {
        assert_eq!(parse("Ctrl+Banana").unwrap_err(), UNSUPPORTED_KEY);
        assert_eq!(parse("Ctrl").unwrap_err(), NEED_KEY);
    }

    #[test]
    fn single_character_must_be_letter_or_digit() {
        assert_eq!(parse("Ctrl+7").unwrap(), Hotkey { modifiers: MOD_CONTROL, vk: 0x37 });
        assert_eq!(parse("Alt+q").unwrap(), Hotkey { modifiers: MOD_ALT, vk: 'Q' as u16 });
        assert_eq!(parse("Ctrl+;").unwrap_err(), UNSUPPORTED_KEY);
        assert_eq!(parse("Shift+!").unwrap_err(), UNSUPPORTED_KEY);
        assert_eq!(parse("Ctrl+\u{e9}").unwrap_err(), UNSUPPORTED_KEY);
    }

    #[test]
    fn bare_key_needs_a_modifier_unless_function_key() {
        assert_eq!(parse("Space").unwrap_err(), NEED_MODIFIER);
        assert_eq!(parse("A").unwrap_err(), NEED_MODIFIER);
        assert_eq!(parse("1").unwrap_err(), NEED_MODIFIER);
        assert_eq!(parse("Enter").unwrap_err(), NEED_MODIFIER);
        assert_eq!(parse("F9").unwrap(), Hotkey { modifiers: 0, vk: 0x78 });
        assert_eq!(parse("F1").unwrap(), Hotkey { modifiers: 0, vk: 0x70 });
        assert_eq!(parse("F12").unwrap(), Hotkey { modifiers: 0, vk: 0x7B });
    }

    #[test]
    fn modifier_only_shortcuts_unchanged() {
        assert_eq!(parse("Ctrl+Win").unwrap(), Hotkey { modifiers: MOD_CONTROL, vk: VK_LWIN });
        assert_eq!(parse("Alt+Win").unwrap(), Hotkey { modifiers: MOD_ALT, vk: VK_LWIN });
        assert_eq!(parse("Ctrl+Shift+Win").unwrap(), Hotkey { modifiers: MOD_CONTROL | MOD_SHIFT, vk: VK_LWIN });
    }

    #[test]
    fn extra_modifiers_do_not_match() {
        // Ctrl+Space: Ctrl alone matches; Ctrl+Shift or Ctrl+Alt does not.
        assert!(mods_match(MOD_CONTROL, 0x20, MOD_CONTROL));
        assert!(!mods_match(MOD_CONTROL, 0x20, MOD_CONTROL | MOD_SHIFT));
        assert!(!mods_match(MOD_CONTROL, 0x20, MOD_CONTROL | MOD_ALT));
        assert!(!mods_match(MOD_CONTROL, 0x20, 0));
        // Ctrl+Win: Win is the main key, so its own group is not "extra".
        assert!(mods_match(MOD_CONTROL, VK_LWIN, MOD_CONTROL | MOD_WIN));
        assert!(!mods_match(MOD_CONTROL, VK_LWIN, MOD_CONTROL | MOD_WIN | MOD_SHIFT));
        // Bare F9: any held modifier means a different shortcut.
        assert!(mods_match(0, 0x78, 0));
        assert!(!mods_match(0, 0x78, MOD_CONTROL));
    }
}
