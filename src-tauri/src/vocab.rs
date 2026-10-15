//! Vocabulary pool for the context engine (stage 1).
//!
//! * The manual list stays in `config.json` `hot_words`; the pool only keeps
//!   usage statistics for those terms (`source: manual`) and everything about
//!   the automatic ones.
//! * File `%APPDATA%\NVGVoice\vocabulary.json`, written temp-file + rename. A
//!   file that does not parse is logged once and never overwritten.
//! * Nothing here logs a term, a folder path or dictated text.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::log;
use crate::store::Config;

pub const MAX_TERMS: usize = 500;
pub const MAX_HINTS: usize = 100;
pub const MAX_TRIALS: usize = 10;
pub const MAX_TERM_CHARS: usize = 50;
const TRIAL_EXPIRY_DAYS: i64 = 30;
const DAY_SECS: i64 = 86_400;
const HARVEST_EXCLUDE: &[&str] = &["node_modules", "target", "dist", "build", ".worktrees"];
const HARVEST_FIRST_DELAY: Duration = Duration::from_secs(30);
const HARVEST_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn pool_path() -> PathBuf {
    crate::store::config_dir().join("vocabulary.json")
}

pub fn now_epoch() -> i64 {
    chrono::Local::now().timestamp()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Term {
    pub term: String,
    pub source: String,
    pub status: String,
    #[serde(default)]
    pub added: i64,
    #[serde(default)]
    pub last_used: i64,
    #[serde(default)]
    pub last_tried: i64,
    #[serde(default)]
    pub uses: u64,
    #[serde(default)]
    pub apps: BTreeMap<String, u64>,
    /// Fields written by a later stage are preserved verbatim across a save.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Term {
    fn automatic(&self) -> bool {
        self.source != "manual"
    }

    /// Misheard forms seen for this term, typed form -> count. Stored in the
    /// flattened `extra` map so a save preserves fields this stage does not
    /// know about.
    pub fn sounds_like(&self) -> BTreeMap<String, u64> {
        self.extra
            .get("sounds_like")
            .and_then(Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn set_sounds_like(&mut self, map: &BTreeMap<String, u64>) {
        let obj: Map<String, Value> = map
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(*v)))
            .collect();
        self.extra.insert("sounds_like".to_string(), Value::Object(obj));
    }
}

#[derive(Serialize, Deserialize)]
struct PoolFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    terms: Vec<Term>,
}

fn default_version() -> u32 {
    1
}

pub struct Pool {
    path: PathBuf,
    pub terms: Vec<Term>,
    /// False when the file existed but could not be read or parsed: it is never
    /// overwritten, exactly like a broken `config.json`.
    writable: bool,
    dirty: bool,
}

impl Pool {
    pub fn load() -> Self {
        Self::load_from(pool_path())
    }

    /// Missing file = empty pool, writable. Present but unreadable or
    /// unparseable = empty pool in memory, never written over.
    pub fn load_from(path: PathBuf) -> Self {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Pool { path, terms: Vec::new(), writable: true, dirty: false };
            }
            Err(_) => {
                log::error(
                    "vocabulary.json could not be read; running with an empty pool and not overwriting it",
                );
                return Pool { path, terms: Vec::new(), writable: false, dirty: false };
            }
        };
        // Windows PowerShell 5 saves UTF-8 with a byte-order mark.
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        match serde_json::from_slice::<PoolFile>(body) {
            Ok(file) => {
                let mut pool = Pool { path, terms: file.terms, writable: true, dirty: false };
                pool.sanitize();
                pool
            }
            Err(_) => {
                log::error(
                    "vocabulary.json is not valid JSON; running with an empty pool and not overwriting it",
                );
                Pool { path, terms: Vec::new(), writable: false, dirty: false }
            }
        }
    }

    /// Drop over-long or duplicate terms so a hand-edited file cannot break the
    /// pool. First occurrence wins.
    fn sanitize(&mut self) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut kept: Vec<Term> = Vec::with_capacity(self.terms.len());
        for mut t in std::mem::take(&mut self.terms) {
            t.term = t.term.trim().to_string();
            if t.term.is_empty() || t.term.chars().count() > MAX_TERM_CHARS {
                continue;
            }
            if !seen.insert(t.term.to_lowercase()) {
                continue;
            }
            if t.source.is_empty() {
                t.source = "workspace".into();
            }
            if t.status.is_empty() {
                t.status = "trial".into();
            }
            let apps = std::mem::take(&mut t.apps);
            t.apps = apps.into_iter().map(|(k, v)| (k.to_lowercase(), v)).collect();
            kept.push(t);
        }
        self.terms = kept;
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the pool as changed so the next save persists it.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    #[cfg(test)]
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn save(&mut self) -> Result<(), String> {
        if !self.writable {
            return Err("the vocabulary file could not be read; it is left untouched".into());
        }
        let doc = PoolFile { version: 1, terms: self.terms.clone() };
        let s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        crate::store::write_atomic(&self.path, s.as_bytes())?;
        self.dirty = false;
        Ok(())
    }

    pub fn index_of(&self, term: &str) -> Option<usize> {
        let key = term.trim().to_lowercase();
        self.terms.iter().position(|t| t.term.to_lowercase() == key)
    }

    pub fn get(&self, term: &str) -> Option<&Term> {
        self.index_of(term).map(|i| &self.terms[i])
    }

    /// Typing a term by hand makes it manual and active, even if it already
    /// existed as an automatic entry (which then stops being automatic).
    pub fn upsert_manual(&mut self, term: &str, now: i64) {
        let term = term.trim();
        if term.is_empty() || term.chars().count() > MAX_TERM_CHARS {
            return;
        }
        match self.index_of(term) {
            Some(i) => {
                let t = &mut self.terms[i];
                t.source = "manual".into();
                t.status = "active".into();
            }
            None => self.terms.push(Term {
                term: term.to_string(),
                source: "manual".into(),
                status: "active".into(),
                added: now,
                last_used: 0,
                last_tried: 0,
                uses: 0,
                apps: BTreeMap::new(),
                extra: Map::new(),
            }),
        }
        self.dirty = true;
    }

    /// A term removed from `hot_words` loses its pool entry entirely.
    pub fn remove(&mut self, term: &str) {
        let key = term.trim().to_lowercase();
        let before = self.terms.len();
        self.terms.retain(|t| t.term.to_lowercase() != key);
        if self.terms.len() != before {
            self.dirty = true;
        }
    }

    /// Removing an automatic term retires it for good (harvest will not re-add).
    pub fn retire(&mut self, term: &str) {
        if let Some(i) = self.index_of(term) {
            if self.terms[i].status != "retired" {
                self.terms[i].status = "retired".into();
                self.dirty = true;
            }
        }
    }

    /// Record one use. Creates a manual entry on first use; promotes a trial
    /// to active. Returns true when anything changed.
    pub fn touch_use(&mut self, term: &str, now: i64, exe: Option<&str>, manual: bool) -> bool {
        let term = term.trim();
        if term.is_empty() || term.chars().count() > MAX_TERM_CHARS {
            return false;
        }
        match self.index_of(term) {
            Some(i) => {
                let t = &mut self.terms[i];
                t.uses = t.uses.saturating_add(1);
                t.last_used = now;
                if let Some(exe) = exe.filter(|e| !e.is_empty()) {
                    *t.apps.entry(exe.to_lowercase()).or_insert(0) += 1;
                }
                if t.status == "trial" {
                    t.status = "active".into();
                }
                if manual {
                    t.source = "manual".into();
                    t.status = "active".into();
                }
            }
            None => {
                if !manual {
                    return false;
                }
                let mut apps = BTreeMap::new();
                if let Some(exe) = exe.filter(|e| !e.is_empty()) {
                    apps.insert(exe.to_lowercase(), 1);
                }
                self.terms.push(Term {
                    term: term.to_string(),
                    source: "manual".into(),
                    status: "active".into(),
                    added: now,
                    last_used: now,
                    last_tried: 0,
                    uses: 1,
                    apps,
                    extra: Map::new(),
                });
            }
        }
        self.dirty = true;
        true
    }

    /// Expire stale trials and enforce the 500 non-retired automatic cap.
    /// Returns true when anything changed.
    pub fn maintain(&mut self, now: i64) -> bool {
        let mut changed = false;
        for t in &mut self.terms {
            if t.automatic()
                && t.status == "trial"
                && t.uses == 0
                && t.added > 0
                && now - t.added > TRIAL_EXPIRY_DAYS * DAY_SECS
            {
                t.status = "retired".into();
                changed = true;
            }
        }
        let mut auto: Vec<usize> = self
            .terms
            .iter()
            .enumerate()
            .filter(|(_, t)| t.automatic() && t.status != "retired")
            .map(|(i, _)| i)
            .collect();
        if auto.len() > MAX_TERMS {
            auto.sort_by(|&a, &b| {
                self.terms[a]
                    .last_used
                    .cmp(&self.terms[b].last_used)
                    .then(self.terms[a].added.cmp(&self.terms[b].added))
            });
            for &i in auto.iter().take(auto.len() - MAX_TERMS) {
                self.terms[i].status = "retired".into();
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }
}

fn key(term: &str) -> String {
    term.trim().to_lowercase()
}

/// Filter a manual list to the recogniser's limits, de-duplicated
/// case-insensitively, order preserved.
pub fn manual_hints(manual: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for term in manual {
        let term = term.trim();
        if term.is_empty() || term.chars().count() > MAX_TERM_CHARS {
            continue;
        }
        if out.len() >= MAX_HINTS {
            break;
        }
        if seen.insert(key(term)) {
            out.push(term.to_string());
        }
    }
    out
}

/// Pick up to 100 hints: every manual term in order, then active automatic
/// terms ranked by uses in the program in front and recency, then up to 10
/// trial terms (least recently tried first), marking those trials as tried.
pub fn select_hints(manual: &[String], pool: &mut Pool, exe: Option<&str>, now: i64) -> Vec<String> {
    let mut out = manual_hints(manual);
    let mut seen: HashSet<String> = out.iter().map(|t| key(t)).collect();
    if out.len() >= MAX_HINTS {
        return out;
    }

    let mut active: Vec<usize> = pool
        .terms
        .iter()
        .enumerate()
        .filter(|(_, t)| t.automatic() && t.status == "active")
        .map(|(i, _)| i)
        .collect();
    active.sort_by(|&a, &b| {
        let ta = &pool.terms[a];
        let tb = &pool.terms[b];
        let ua = exe.and_then(|e| ta.apps.get(e).copied()).unwrap_or(0);
        let ub = exe.and_then(|e| tb.apps.get(e).copied()).unwrap_or(0);
        ub.cmp(&ua)
            .then(tb.last_used.cmp(&ta.last_used))
            .then(ta.term.cmp(&tb.term))
    });
    for &i in &active {
        if out.len() >= MAX_HINTS {
            break;
        }
        let t = &pool.terms[i];
        if seen.insert(key(&t.term)) {
            out.push(t.term.clone());
        }
    }

    if out.len() < MAX_HINTS {
        let mut trials: Vec<usize> = pool
            .terms
            .iter()
            .enumerate()
            .filter(|(_, t)| t.automatic() && t.status == "trial")
            .map(|(i, _)| i)
            .collect();
        trials.sort_by(|&a, &b| {
            pool.terms[a]
                .last_tried
                .cmp(&pool.terms[b].last_tried)
                .then(pool.terms[a].added.cmp(&pool.terms[b].added))
                .then(pool.terms[a].term.cmp(&pool.terms[b].term))
        });
        let mut picked = 0usize;
        for &i in &trials {
            if picked >= MAX_TRIALS || out.len() >= MAX_HINTS {
                break;
            }
            let term = pool.terms[i].term.clone();
            if seen.insert(key(&term)) {
                out.push(term);
                pool.terms[i].last_tried = now;
                pool.dirty = true;
                picked += 1;
            }
        }
    }
    out
}

/// Normalise to a padded, lower-cased token string so a whole-word test is a
/// simple `contains`: `" pane "` is found in `" the pane is "` but not in
/// `" the panes are "`.
fn padded_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    let mut last_space = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !last_space {
        out.push(' ');
    }
    if !out.starts_with(' ') {
        out.insert(0, ' ');
    }
    out
}

/// True when `term` occurs in `text` as a whole word (case-insensitive).
pub fn contains_whole_word(text: &str, term: &str) -> bool {
    let needle = padded_words(term);
    if needle.trim().is_empty() {
        return false;
    }
    padded_words(text).contains(&needle)
}

/// Number of whole-word (case-insensitive) occurrences of `term` in `text`.
pub fn count_whole_word(text: &str, term: &str) -> usize {
    let needle = padded_words(term);
    if needle.trim().is_empty() {
        return 0;
    }
    padded_words(text).matches(&needle).count()
}

/// Count uses in the final post-processed text for every manual or non-retired
/// pool term that occurs as a whole word. Never called before text is typed.
pub fn track_usage(
    manual: &[String],
    pool: &mut Pool,
    text: &str,
    exe: Option<&str>,
    now: i64,
) -> bool {
    let hay = padded_words(text);
    if hay.trim().is_empty() {
        return false;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut changed = false;

    for term in manual {
        let term = term.trim();
        if term.is_empty() || term.chars().count() > MAX_TERM_CHARS {
            continue;
        }
        if !seen.insert(key(term)) {
            continue;
        }
        let needle = padded_words(term);
        if needle.trim().is_empty() {
            continue;
        }
        if hay.contains(&needle) && pool.touch_use(term, now, exe, true) {
            changed = true;
        }
    }

    let candidates: Vec<String> = pool
        .terms
        .iter()
        .filter(|t| t.automatic() && t.status != "retired")
        .map(|t| t.term.clone())
        .collect();
    for term in candidates {
        if !seen.insert(key(&term)) {
            continue;
        }
        let needle = padded_words(&term);
        if needle.trim().is_empty() {
            continue;
        }
        if hay.contains(&needle) && pool.touch_use(&term, now, exe, false) {
            changed = true;
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Workspace harvest
// ---------------------------------------------------------------------------

pub struct Harvest {
    pub added: usize,
    pub skipped: usize,
}

fn skip_dir(name: &str, exclude: &HashSet<String>) -> bool {
    if name.starts_with('.') || name.starts_with('_') {
        return true;
    }
    let lower = name.to_lowercase();
    HARVEST_EXCLUDE.contains(&lower.as_str()) || exclude.contains(&lower)
}

/// A folder name is only a candidate when it looks like a name: letters,
/// digits, spaces and the three separators. Anything else (`%SystemDrive%`,
/// slash paths) is not.
fn valid_name_chars(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

fn consider(pool: &mut Pool, manual: &HashSet<String>, name: &str, now: i64, report: &mut Harvest) {
    if !name.chars().any(|c| c.is_uppercase()) || !valid_name_chars(name) {
        return;
    }
    let term = name.replace('-', " ").replace('_', " ");
    if term.chars().count() > MAX_TERM_CHARS {
        return;
    }
    // Generic English names are not vocabulary. A lone short token is generic
    // too ("ME", "X1").
    if crate::words::is_all_english(&term) {
        return;
    }
    let mut tokens = term.split_whitespace();
    if let (Some(only), None) = (tokens.next(), tokens.next()) {
        if only.chars().count() <= 3 {
            return;
        }
    }
    let k = key(&term);
    if k.is_empty() || manual.contains(&k) || pool.index_of(&term).is_some() {
        return;
    }
    pool.terms.push(Term {
        term,
        source: "workspace".into(),
        status: "trial".into(),
        added: now,
        last_used: 0,
        last_tried: 0,
        uses: 0,
        apps: BTreeMap::new(),
        extra: Map::new(),
    });
    pool.dirty = true;
    report.added += 1;
}

/// List sub-folder names at depth 1 and 2 only; never opens a file. A missing
/// or unreadable folder is counted, never named.
pub fn harvest(pool: &mut Pool, manual: &[String], folders: &[String], exclude: &[String], now: i64) -> Harvest {
    let manual_keys: HashSet<String> = manual.iter().map(|m| key(m)).collect();
    let exclude_keys: HashSet<String> = exclude.iter().map(|e| e.trim().to_lowercase()).collect();
    let mut report = Harvest { added: 0, skipped: 0 };
    for folder in folders {
        let root = match std::fs::read_dir(folder) {
            Ok(rd) => rd,
            Err(_) => {
                report.skipped += 1;
                continue;
            }
        };
        for e1 in root.flatten() {
            if !e1.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let p1 = e1.path();
            let name1 = match p1.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if skip_dir(&name1, &exclude_keys) {
                continue;
            }
            consider(pool, &manual_keys, &name1, now, &mut report);
            let Ok(inner) = std::fs::read_dir(&p1) else { continue };
            for e2 in inner.flatten() {
                if !e2.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name2 = match e2.path().file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if skip_dir(&name2, &exclude_keys) {
                    continue;
                }
                consider(pool, &manual_keys, &name2, now, &mut report);
            }
        }
    }
    report
}

/// Startup delay then every 24 h, only while `auto_vocabulary` is on. Reads the
/// config fresh each run so a hand edit and the Settings switch are honoured.
pub fn spawn_harvester(pool: Arc<Mutex<Pool>>) {
    let spawned = std::thread::Builder::new().name("vox-harvest".into()).spawn(move || {
        std::thread::sleep(HARVEST_FIRST_DELAY);
        loop {
            run_harvest(&pool);
            std::thread::sleep(HARVEST_INTERVAL);
        }
    });
    if let Err(e) = spawned {
        log::error(&format!("vocabulary harvest thread failed to start: {e}"));
    }
}

fn run_harvest(pool: &Arc<Mutex<Pool>>) {
    let cfg = Config::load();
    if !cfg.auto_vocabulary() {
        return;
    }
    let folders = cfg.workspace_folders();
    if folders.is_empty() {
        return;
    }
    let exclude = cfg.workspace_exclude();
    let manual = cfg.hot_words();
    let now = now_epoch();
    let (added, skipped) = {
        let mut p = pool.lock().unwrap_or_else(|e| e.into_inner());
        let report = harvest(&mut p, &manual, &folders, &exclude, now);
        maintain_and_save(&mut p, now, report.added > 0);
        (report.added, report.skipped)
    };
    if added > 0 || skipped > 0 {
        // Counts only: no term and no folder path.
        log::info(&format!(
            "vocabulary harvest: added {added} term(s), skipped {skipped} folder(s)"
        ));
    }
}

fn maintain_and_save(pool: &mut Pool, now: i64, anything: bool) {
    let maintained = pool.maintain(now);
    if (anything || maintained) && pool.dirty() {
        if let Err(e) = pool.save() {
            log::warn(&format!("vocabulary not saved: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nvg-voice-vocab-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn term(name: &str, source: &str, status: &str) -> Term {
        Term {
            term: name.into(),
            source: source.into(),
            status: status.into(),
            added: 0,
            last_used: 0,
            last_tried: 0,
            uses: 0,
            apps: BTreeMap::new(),
            extra: Map::new(),
        }
    }

    #[test]
    fn manual_hints_keep_order_and_drop_duplicates_and_long_terms() {
        let manual = vec!["Astra".to_string(), "astra".to_string(), "Codex".to_string(), "x".repeat(60)];
        assert_eq!(manual_hints(&manual), vec!["Astra", "Codex"]);
    }

    #[test]
    fn select_hints_puts_manual_first_then_ranked_active() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        pool.terms.push(term("Zed", "workspace", "active"));
        pool.terms.push(term("Alpha", "workspace", "active"));
        pool.terms[0].apps.insert("cokpit.exe".into(), 9);
        pool.terms[1].apps.insert("cokpit.exe".into(), 2);
        let got = select_hints(&["Manual".into()], &mut pool, Some("cokpit.exe"), 100);
        assert_eq!(got, vec!["Manual", "Zed", "Alpha"]);
    }

    #[test]
    fn select_hints_caps_at_100_with_manual_first() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        for i in 0..150 {
            let mut t = term(&format!("Term{i}"), "workspace", "active");
            t.apps.insert("cokpit.exe".into(), i as u64);
            pool.terms.push(t);
        }
        let manual = vec!["One".to_string(), "Two".to_string()];
        let got = select_hints(&manual, &mut pool, Some("cokpit.exe"), 100);
        assert_eq!(got.len(), MAX_HINTS);
        assert_eq!(&got[..2], &["One", "Two"]);
        assert_eq!(got[2], "Term149");
    }

    #[test]
    fn select_hints_ranks_by_program_then_recency() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        let mut a = term("Alpha", "workspace", "active");
        a.apps.insert("cokpit.exe".into(), 3);
        a.last_used = 10;
        let mut b = term("Beta", "workspace", "active");
        b.apps.insert("cokpit.exe".into(), 3);
        b.last_used = 99;
        let mut c = term("Gamma", "workspace", "active");
        c.apps.insert("chrome.exe".into(), 50);
        pool.terms.extend([a, b, c]);
        let got = select_hints(&[], &mut pool, Some("cokpit.exe"), 100);
        assert_eq!(got, vec!["Beta", "Alpha", "Gamma"]);
    }

    #[test]
    fn trial_rotation_uses_last_tried_and_marks_selected() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        for i in 0..12 {
            let mut t = term(&format!("Trial{i:02}"), "workspace", "trial");
            t.last_tried = i as i64;
            pool.terms.push(t);
        }
        let first = select_hints(&[], &mut pool, None, 1000);
        assert_eq!(first.len(), MAX_TRIALS);
        assert_eq!(first[0], "Trial00");
        assert_eq!(first[9], "Trial09");
        assert!(pool.terms[10].last_tried == 10 && pool.terms[11].last_tried == 11);
        let second = select_hints(&[], &mut pool, None, 2000);
        assert_eq!(second.len(), 10);
        // The two never-tried rotate in first.
        assert_eq!(&second[..2], &["Trial10", "Trial11"]);
    }

    #[test]
    fn track_usage_promotes_trial_and_counts_per_program() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        pool.terms.push(term("Cokpit", "workspace", "trial"));
        let manual = vec!["Astra".to_string()];
        let changed = track_usage(&manual, &mut pool, "I opened Cokpit with Astra in Cokpit.", Some("cokpit.exe"), 50);
        assert!(changed);
        let c = pool.get("Cokpit").unwrap();
        assert_eq!(c.status, "active");
        // One dictation that mentions the term counts once, whatever the count
        // of occurrences in the sentence.
        assert_eq!(c.uses, 1);
        assert_eq!(c.apps.get("cokpit.exe").copied(), Some(1));
        let a = pool.get("Astra").unwrap();
        assert_eq!(a.source, "manual");
        assert_eq!(a.uses, 1);
    }

    #[test]
    fn track_usage_matches_whole_words_only() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        pool.terms.push(term("pane", "workspace", "active"));
        assert!(!track_usage(&[], &mut pool, "the panes are open", Some("cokpit.exe"), 1));
        assert_eq!(pool.get("pane").unwrap().uses, 0);
        assert!(track_usage(&[], &mut pool, "the pane is open", Some("cokpit.exe"), 2));
        assert_eq!(pool.get("pane").unwrap().uses, 1);
    }

    #[test]
    fn broken_vocabulary_file_is_never_overwritten() {
        let dir = test_dir("broken");
        let path = dir.join("vocabulary.json");
        let broken = b"{\"version\":1,\"terms\":[{\"term\":\"Astra\",}]}";
        std::fs::write(&path, broken).unwrap();
        let mut pool = Pool::load_from(path.clone());
        assert!(!pool.is_writable());
        assert!(pool.terms.is_empty());
        pool.upsert_manual("Codex", 1);
        assert!(pool.save().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), broken.to_vec());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_vocabulary_file_loads_empty_without_creating_one() {
        let dir = test_dir("missing");
        let path = dir.join("vocabulary.json");
        let pool = Pool::load_from(path.clone());
        assert!(pool.is_writable());
        assert!(pool.terms.is_empty());
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn trial_expiry_and_never_used_rule() {
        let now = 1_000_000_000i64;
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        let mut stale = term("Stale", "workspace", "trial");
        stale.added = now - 31 * DAY_SECS;
        let mut used = term("Used", "workspace", "trial");
        used.added = now - 31 * DAY_SECS;
        used.uses = 2;
        let mut fresh = term("Fresh", "workspace", "trial");
        fresh.added = now - DAY_SECS;
        pool.terms.extend([stale, used, fresh]);
        pool.maintain(now);
        assert_eq!(pool.get("Stale").unwrap().status, "retired");
        assert_eq!(pool.get("Used").unwrap().status, "trial");
        assert_eq!(pool.get("Fresh").unwrap().status, "trial");
    }

    #[test]
    fn cap_retires_least_recently_used_automatic_but_keeps_manual() {
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        for i in 0..505 {
            let mut t = term(&format!("Auto{i}"), "workspace", "active");
            t.last_used = i as i64;
            pool.terms.push(t);
        }
        pool.terms.push(term("Pinned", "manual", "active"));
        pool.maintain(1_000_000);
        let active = pool.terms.iter().filter(|t| t.automatic() && t.status != "retired").count();
        assert_eq!(active, MAX_TERMS);
        assert_eq!(pool.get("Auto0").unwrap().status, "retired");
        assert_eq!(pool.get("Auto4").unwrap().status, "retired");
        assert_eq!(pool.get("Auto5").unwrap().status, "active");
        assert_eq!(pool.get("Pinned").unwrap().status, "active");
    }

    #[test]
    fn harvest_applies_uppercase_excludes_depth_and_hyphen_rules() {
        let dir = test_dir("harvest");
        for name in ["Alpha", "Client", "node_modules", "_hidden", ".git", "lowercase", "NVG-Talk", "Outer", "OuterA"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::create_dir_all(dir.join("Outer").join("DeepName")).unwrap();
        std::fs::create_dir_all(dir.join("Outer").join("Client")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules").join("Inner")).unwrap();
        std::fs::create_dir_all(dir.join("OuterA").join("target")).unwrap();
        let long = "A".repeat(60);
        std::fs::create_dir_all(dir.join("OuterA").join(&long)).unwrap();

        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        let folders = vec![dir.to_string_lossy().into_owned(), dir.join("does-not-exist").to_string_lossy().into_owned()];
        let report = harvest(&mut pool, &[], &folders, &["Client".to_string()], 100);
        let mut names: Vec<String> = pool.terms.iter().map(|t| t.term.clone()).collect();
        names.sort();
        // Alpha / Outer are skipped now: they are ordinary English words.
        assert_eq!(names, vec!["DeepName", "NVG Talk", "OuterA"]);
        assert_eq!(report.skipped, 1);
        assert!(pool.terms.iter().all(|t| t.source == "workspace" && t.status == "trial"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn harvest_skips_generic_short_and_invalid_names() {
        let dir = test_dir("harvest-filter");
        for name in [
            "%SystemDrive%",
            "Setup",
            "Installer",
            "Portfolio",
            "X1",
            "ME",
            "Kavish Resume",
            "NVG Talk",
            "Satterbar",
        ] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        let report = harvest(&mut pool, &[], &[dir.to_string_lossy().into_owned()], &[], 1);
        let mut names: Vec<String> = pool.terms.iter().map(|t| t.term.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["Kavish Resume", "NVG Talk", "Satterbar"]);
        assert_eq!(report.added, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn harvest_does_not_readd_a_retired_or_manual_term() {
        let dir = test_dir("harvest-retired");
        std::fs::create_dir_all(dir.join("Closed")).unwrap();
        std::fs::create_dir_all(dir.join("Manual")).unwrap();
        let mut pool = Pool { path: PathBuf::new(), terms: vec![], writable: true, dirty: false };
        pool.terms.push(term("Closed", "workspace", "retired"));
        let report = harvest(&mut pool, &["Manual".to_string()], &[dir.to_string_lossy().into_owned()], &[], 1);
        assert_eq!(report.added, 0);
        assert_eq!(pool.terms.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pool_round_trips_through_a_file() {
        let dir = test_dir("roundtrip");
        let path = dir.join("vocabulary.json");
        let mut pool = Pool::load_from(path.clone());
        pool.upsert_manual("NVG Talk", 10);
        pool.terms.push(term("Cokpit", "workspace", "trial"));
        pool.terms.last_mut().unwrap().apps.insert("cokpit.exe".into(), 3);
        pool.save().unwrap();
        let reloaded = Pool::load_from(path);
        assert_eq!(reloaded.get("NVG Talk").unwrap().source, "manual");
        assert_eq!(reloaded.get("Cokpit").unwrap().apps.get("cokpit.exe").copied(), Some(3));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
