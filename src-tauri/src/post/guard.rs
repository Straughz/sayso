//! Hallucination guard — reject obviously bad transcriptions.
//! Port of `src/vox/post/guard.py`.

use regex::Regex;
use std::collections::HashMap;

pub struct HallucinationGuard {
    min_words: usize,
    max_repeat_ratio: f64,
    punctuation_only: Regex,
}

impl HallucinationGuard {
    pub fn new(min_words: usize, max_repeat_ratio: f64) -> Self {
        HallucinationGuard {
            min_words,
            max_repeat_ratio,
            punctuation_only: Regex::new(r"^[\W_]+$").expect("valid guard regex"),
        }
    }

    pub fn is_valid(&self, text: &str) -> bool {
        self.check(text).is_ok()
    }

    /// Why a transcript is rejected, so the caller can tell a repetition loop
    /// (worth telling the user about) from silence or punctuation.
    pub fn check(&self, text: &str) -> Result<(), Reject> {
        let stripped = text.trim();
        if stripped.is_empty() {
            return Err(Reject::Empty);
        }
        if self.punctuation_only.is_match(stripped) {
            return Err(Reject::PunctuationOnly);
        }
        let words: Vec<&str> = stripped.split_whitespace().collect();
        if words.len() < self.min_words {
            return Err(Reject::TooShort);
        }
        if words.len() >= 4 {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for w in &words {
                *counts.entry(w.to_lowercase()).or_insert(0) += 1;
            }
            let most_common = counts.values().copied().max().unwrap_or(0);
            // A word said twice is normal speech ("testing one, two, testing");
            // a loop needs at least three repeats.
            if most_common >= 3 && most_common as f64 / words.len() as f64 >= self.max_repeat_ratio {
                return Err(Reject::Repetition);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Empty,
    PunctuationOnly,
    TooShort,
    Repetition,
}

impl Default for HallucinationGuard {
    fn default() -> Self {
        Self::new(1, 0.5)
    }
}
