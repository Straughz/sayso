//! Backtrack / undo detection — recognises retraction utterances.
//! Port of `src/vox/post/backtrack.py`.

const BACKTRACK_TRIGGERS: &[&str] = &[
    "backtrack",
    "undo",
    "scratch that",
    "go back",
    "never mind",
    "delete that",
    "cancel that",
    "forget it",
    "forget that",
];

pub struct BacktrackDetector;

impl BacktrackDetector {
    pub fn new() -> Self {
        BacktrackDetector
    }

    pub fn detect(&self, text: &str) -> bool {
        let cleaned = text.trim().to_lowercase();
        let cleaned = cleaned.trim_end_matches(['.', '!', '?', ',']);
        BACKTRACK_TRIGGERS.contains(&cleaned)
    }
}

impl Default for BacktrackDetector {
    fn default() -> Self {
        Self::new()
    }
}
