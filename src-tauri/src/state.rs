//! Dictation state machine. Port of `src/vox/core/state.py` plus the
//! wiring that was in `VoxApp`.
//!
//! IDLE → LISTENING → PROCESSING → back to IDLE, always recovering to IDLE
//! after an error. Processing runs on its own worker thread; the coordinator
//! thread only moves state so a press during PROCESSING is ignored.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::audio::AudioController;
use crate::inject::{self, Win32TextInjector};
use crate::log;
use crate::pill::PillHandle;
use crate::post::{BacktrackDetector, PostProcessor};
use crate::stt::SttEngine;
use crate::store::{Config, HistoryStore};
use crate::vocab::{self, Pool};

const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Holds shorter than this are discarded. The Python `UtteranceBuffer`
/// declares a 300 ms `min_utterance_ms` but never enforces it; the Rust core
/// enforces it so a stray tap does not pay for an STT round-trip.
const MIN_HOLD: Duration = Duration::from_millis(300);
/// The coordinator wakes this often to check the hotkey and the time limit.
const TICK: Duration = Duration::from_millis(250);
/// Hard cap on one recording, both modes. He dictates for 3 to 4 minutes at
/// a time; anything past this is a stuck recording, not speech.
const MAX_LISTEN: Duration = Duration::from_secs(10 * 60);
const LIMIT_MESSAGE: &str = "Stopped: 10 minute limit";
const REPEAT_MESSAGE: &str = "Not typed: looked like a repeat";
const CRASH_MESSAGE: &str = "Dictation failed";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppState {
    Idle,
    Listening,
    Processing,
    Done,
    Error,
}

pub enum Event {
    Press,
    Release,
    AudioError(String),
}

struct Machine {
    state: AppState,
    toggle_active: bool,
    toggle_press_t0: Instant,
    press_t0: Instant,
    press_wall: SystemTime,
    /// Consecutive ticks on which the hotkey read as released (hold mode).
    up_ticks: u8,
}

impl Machine {
    fn new() -> Self {
        let now = Instant::now();
        Machine {
            state: AppState::Idle,
            toggle_active: false,
            toggle_press_t0: now,
            press_t0: now,
            press_wall: SystemTime::now(),
            up_ticks: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Cancel {
    Paused,
    TimeLimit,
}

pub struct Core {
    pub post: Arc<RwLock<PostProcessor>>,
    pub injector: Arc<Win32TextInjector>,
    pub stt: Arc<dyn SttEngine>,
    pub history: Arc<HistoryStore>,
    pub backtrack: BacktrackDetector,
    pub audio: AudioController,
    pub pill: PillHandle,
    pub recording_mode: Arc<Mutex<String>>,
    pub paused: Arc<AtomicBool>,
    pub pool: Arc<Mutex<Pool>>,
    machine: Mutex<Machine>,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        post: Arc<RwLock<PostProcessor>>,
        injector: Arc<Win32TextInjector>,
        stt: Arc<dyn SttEngine>,
        history: Arc<HistoryStore>,
        audio: AudioController,
        pill: PillHandle,
        recording_mode: Arc<Mutex<String>>,
        paused: Arc<AtomicBool>,
        pool: Arc<Mutex<Pool>>,
    ) -> Self {
        Core {
            post,
            injector,
            stt,
            history,
            backtrack: BacktrackDetector::new(),
            audio,
            pill,
            recording_mode,
            paused,
            pool,
            machine: Mutex::new(Machine::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Machine> {
        self.machine.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self) -> AppState { self.lock().state }
}

pub fn run(events: Receiver<Event>, core: Arc<Core>) {
    log::info("dictation state machine ready");
    let mut last_tick = Instant::now();
    loop {
        match events.recv_timeout(TICK) {
            Ok(Event::Press) => on_press(&core),
            Ok(Event::Release) => on_release(&core),
            Ok(Event::AudioError(msg)) => on_audio_error(&core, &msg),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            tick(&core);
        }
    }
}

/// Ends a recording that nothing else would end: pause turned on, the time
/// limit, or (hold mode) a key-up the hook never saw.
fn tick(core: &Arc<Core>) {
    let (toggle, started) = {
        let mut machine = core.lock();
        if machine.state != AppState::Listening {
            machine.up_ticks = 0;
            return;
        }
        (machine.toggle_active, machine.press_t0)
    };
    if core.paused.load(Ordering::SeqCst) {
        cancel_listening(core, Cancel::Paused);
        return;
    }
    if started.elapsed() >= MAX_LISTEN {
        cancel_listening(core, Cancel::TimeLimit);
        return;
    }
    if toggle {
        return;
    }
    let released = {
        let mut machine = core.lock();
        if crate::hotkey::main_key_looks_up() {
            machine.up_ticks = machine.up_ticks.saturating_add(1);
        } else {
            machine.up_ticks = 0;
        }
        machine.up_ticks >= 2
    };
    if released {
        core.lock().up_ticks = 0;
        crate::hotkey::reset_latches();
        log::warn("hotkey release was not seen; ending the recording as a release");
        on_release(core);
    }
}

/// Called when pause is switched on: an active recording is dropped.
pub fn cancel_if_paused(core: &Arc<Core>) {
    if core.paused.load(Ordering::SeqCst) {
        cancel_listening(core, Cancel::Paused);
    }
}

/// Stop the microphone and return to Idle without uploading anything.
fn cancel_listening(core: &Arc<Core>, why: Cancel) {
    {
        let mut machine = core.lock();
        if machine.state != AppState::Listening {
            return;
        }
        machine.state = AppState::Idle;
        machine.toggle_active = false;
        machine.up_ticks = 0;
    }
    let _ = core.audio.stop(); // discarded: nothing is uploaded or typed
    crate::hotkey::reset_latches();
    match why {
        Cancel::Paused => {
            log::info("recording cancelled: dictation paused");
            core.pill.idle();
            crate::commands::emit_status("Paused");
        }
        Cancel::TimeLimit => {
            log::warn("recording stopped at the 10 minute limit; nothing was sent");
            core.pill.error(LIMIT_MESSAGE);
            crate::commands::emit_status("Ready");
        }
    }
}

fn on_press(core: &Arc<Core>) {
    let now = Instant::now();
    {
        let machine = core.lock();
        match machine.state {
            AppState::Listening => {
                let is_toggle_stop =
                    machine.toggle_active && now.duration_since(machine.toggle_press_t0) >= TOGGLE_DEBOUNCE;
                drop(machine);
                if is_toggle_stop {
                    begin_processing(core);
                }
                return;
            }
            AppState::Idle => {}
            _ => return, // a press during PROCESSING/DONE/ERROR is ignored
        }
    }
    // Pause only blocks starting a recording. A press that stops a toggle
    // recording still gets through (and `begin_processing` turns it into a
    // cancel while paused).
    if core.paused.load(Ordering::SeqCst) {
        return;
    }

    let toggle_active = core
        .recording_mode
        .lock()
        .map(|m| m.as_str() == "toggle")
        .unwrap_or(false);
    {
        let mut machine = core.lock();
        if machine.state != AppState::Idle {
            return;
        }
        machine.state = AppState::Listening;
        machine.toggle_active = toggle_active;
        if toggle_active {
            machine.toggle_press_t0 = now;
        }
        machine.press_t0 = now;
        machine.press_wall = SystemTime::now();
        machine.up_ticks = 0;
    }
    log::info("Listening…");
    crate::commands::emit_status("Listening");
    core.pill.listening();
    core.audio.start();
}

fn on_release(core: &Arc<Core>) {
    let hold = {
        let machine = core.lock();
        if machine.toggle_active || machine.state != AppState::Listening {
            return;
        }
        Instant::now().duration_since(machine.press_t0)
    };
    if hold < MIN_HOLD {
        log::info(&format!("ignored {} ms hold", hold.as_millis()));
        core.audio.stop();
        core.lock().state = AppState::Idle;
        crate::commands::emit_status("Ready");
        core.pill.idle();
        return;
    }
    begin_processing(core);
}

fn begin_processing(core: &Arc<Core>) {
    if core.paused.load(Ordering::SeqCst) {
        cancel_listening(core, Cancel::Paused);
        return;
    }
    // The window the text is meant for: whatever is in front at release.
    let target = inject::foreground_window();
    // The program in front, read once now while the window is still there.
    let exe = inject::exe_of(target);
    let (release_t0, release_wall, press_wall) = {
        let mut machine = core.lock();
        if machine.state != AppState::Listening {
            return;
        }
        machine.state = AppState::Processing;
        (Instant::now(), SystemTime::now(), machine.press_wall)
    };
    core.pill.processing();
    crate::commands::emit_status("Processing");
    let core2 = Arc::clone(core);
    let spawn = std::thread::Builder::new()
        .name("vox-processing".into())
        .spawn(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                process(&core2, release_t0, release_wall, press_wall, target, exe)
            }));
            if run.is_err() {
                // The panic payload is not logged: it could quote text.
                let _ = core2.audio.stop();
                recover_error(&core2, CRASH_MESSAGE);
            }
        });
    if spawn.is_err() {
        log::error("failed to spawn processing thread");
        let _ = core.audio.stop();
        core.pill.idle();
        core.lock().state = AppState::Idle;
        crate::commands::emit_status("Ready");
    }
}

fn process(
    core: &Arc<Core>,
    release_t0: Instant,
    release_wall: SystemTime,
    press_wall: SystemTime,
    target: isize,
    exe: Option<String>,
) {
    log::info("Processing…");
    let wav = core.audio.stop();
    // Keep the last capture for diagnosis (Python keeps five in the same folder).
    if !wav.is_empty() {
        let dir = crate::store::data_dir().join("captures");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("rust-last.wav"), &wav);
    }
    // Context engine: hints are picked per dictation. With auto vocabulary off
    // (the default) this is exactly the manual list and the pool is untouched.
    let cfg = Config::load();
    let manual = cfg.hot_words();
    let auto = cfg.auto_vocabulary();
    let history_enabled = cfg.history_enabled();

    let t_transcribe = Instant::now();
    let result = if wav.is_empty() {
        Ok(String::new())
    } else {
        let hints = if auto {
            let mut pool = core.pool.lock().unwrap_or_else(|p| p.into_inner());
            vocab::select_hints(&manual, &mut pool, exe.as_deref(), vocab::now_epoch())
        } else {
            vocab::manual_hints(&manual)
        };
        core.stt.transcribe(&wav, &hints)
    };
    let transcribe_ms = t_transcribe.elapsed().as_millis();

    let raw = match result {
        Ok(t) => t,
        Err(e) => {
            recover_error(core, &e);
            return;
        }
    };
    // Dictated text stays out of the log (Python logs it at DEBUG only).
    log::info(&format!("[asr] {} chars", raw.chars().count()));

    // A backtrack phrase is an undo only right after an injection into the
    // window that is still in front; otherwise it is ordinary text.
    if !raw.is_empty() && core.backtrack.detect(&raw) {
        if core.injector.undo_recent() {
            core.history.append_undo();
            log::info("↩ Undone");
            core.lock().state = AppState::Done;
            core.pill.done();
            core.lock().state = AppState::Idle;
            crate::commands::emit_status("Ready");
            return;
        }
        log::info("backtrack phrase typed as text (no recent injection in this window)");
    }

    let (processed, auto_fixes, repeat) = if raw.is_empty() { (raw, Vec::new(), false) } else {
        let post = core.post.read().unwrap_or_else(|p| p.into_inner());
        let (out, fixes) = post.run_traced(&raw);
        let repeat = out.is_empty() && post.rejected_as_repeat(&raw);
        (out, fixes, repeat)
    };
    if !processed.is_empty() {
        let typed = core.post.read().unwrap_or_else(|p| p.into_inner()).for_injection(&processed);
        let t_inject = Instant::now();
        let injected = core.injector.inject(&typed, target);
        let type_ms = t_inject.elapsed().as_millis();
        let total_ms = release_t0.elapsed().as_millis();
        let duration_s = release_wall
            .duration_since(press_wall)
            .unwrap_or_default()
            .as_secs_f64();
        // Capture ring: only when the context engine is on and history is kept
        // (there is no history entry to point at otherwise). A failure is one
        // warning line and nothing else, and never blocks typing.
        let cap_name = if auto && history_enabled && !wav.is_empty() {
            let name = format!("{}.wav", crate::store::now_ms());
            match crate::store::write_capture(&crate::store::capture_ring_dir(), &wav, &name) {
                Ok(()) => Some(name),
                Err(_) => {
                    log::warn("capture ring write failed");
                    None
                }
            }
        } else {
            None
        };
        // History keeps the text either way, so it can be copied from the
        // flyout when typing failed or focus moved.
        core.history.append(
            &processed,
            duration_s,
            &core.stt.name(),
            cap_name.as_deref(),
            &auto_fixes,
        );
        if let Err(msg) = injected {
            recover_error(core, msg);
            return;
        }
        log::info(&format!(
            "✓ Done ({} chars) in {} ms after release (transcribe {}, type {})",
            processed.chars().count(),
            total_ms,
            transcribe_ms,
            type_ms
        ));
        core.lock().state = AppState::Done;
        core.pill.done();
    } else if repeat {
        // The text itself never goes to the log.
        log::info("(nothing) transcription rejected by the repetition guard");
        recover_error(core, REPEAT_MESSAGE);
        return;
    } else {
        log::info("(nothing) transcription rejected by clean-up");
        core.pill.idle();
    }
    // Usage is recorded only after typing, so it never delays the text.
    if auto {
        let now = vocab::now_epoch();
        let mut pool = core.pool.lock().unwrap_or_else(|p| p.into_inner());
        if !processed.is_empty() {
            vocab::track_usage(&manual, &mut pool, &processed, exe.as_deref(), now);
        }
        pool.maintain(now);
        if pool.dirty() {
            if let Err(e) = pool.save() {
                log::warn(&format!("vocabulary not saved: {e}"));
            }
        }
    }
    core.lock().state = AppState::Idle;
    crate::commands::emit_status("Ready");
}

fn on_audio_error(core: &Arc<Core>, msg: &str) {
    let should_report = {
        let mut machine = core.lock();
        if machine.state == AppState::Listening {
            // Claim the recording so the release that follows is ignored.
            machine.state = AppState::Error;
            true
        } else {
            false
        }
    };
    if should_report {
        let _ = core.audio.stop(); // the partial capture is not sent
        recover_error(core, msg);
    }
}

/// ERROR (with the short message on the pill) then back to IDLE.
fn recover_error(core: &Arc<Core>, msg: &str) {
    log::error(&format!("processing error: {msg}"));
    core.lock().state = AppState::Error;
    crate::commands::emit_status("Error");
    core.pill.error(msg);
    core.lock().state = AppState::Idle;
    crate::commands::emit_status("Ready");
}
