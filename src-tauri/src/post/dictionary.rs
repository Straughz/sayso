//! User-defined word-correction dictionary. Port of `src/vox/post/dictionary.py`.

use regex::Regex;

use crate::log;

pub struct CustomDictionary {
    patterns: Vec<(Regex, String)>,
}

impl CustomDictionary {
    pub fn empty() -> Self {
        CustomDictionary { patterns: Vec::new() }
    }

    /// `None` means "no dictionary configured" (no-op, no warning).
    pub fn new(json: Option<&str>) -> Self {
        match json {
            Some(s) if !s.trim().is_empty() => Self::from_json(s),
            _ => Self::empty(),
        }
    }

    pub fn from_json(json: &str) -> Self {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                log::warn(&format!("dictionary: could not parse JSON: {e} — using empty"));
                return Self::empty();
            }
        };
        let Some(obj) = value.as_object() else {
            return Self::empty();
        };
        let mut corrections: Vec<(String, String)> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.to_lowercase(), s.to_string())))
            .collect();
        // Longest key first, stable for equal lengths (matches Python).
        corrections.sort_by_key(|x| std::cmp::Reverse(x.0.len()));
        let patterns = corrections
            .into_iter()
            .filter_map(|(k, v)| {
                Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&k)))
                    .ok()
                    .map(|re| (re, v))
            })
            .collect();
        CustomDictionary { patterns }
    }

    /// A dangling absolute path (e.g. left over from a directory rename) must
    /// not crash construction — it degrades to an empty dictionary.
    #[allow(dead_code)]
    pub fn from_path(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_json(&s),
            Err(e) => {
                log::warn(&format!(
                    "dictionary: could not load {path}: {e} — using empty dictionary"
                ));
                Self::empty()
            }
        }
    }

    pub fn process(&self, text: &str) -> String {
        self.process_traced(text).0
    }

    /// Like `process`, but also returns every replacement that changed the
    /// text as `(word as matched, replacement as applied)`.
    pub fn process_traced(&self, text: &str) -> (String, Vec<(String, String)>) {
        let mut out = text.to_string();
        let mut fixes: Vec<(String, String)> = Vec::new();
        for (re, replacement) in &self.patterns {
            out = re
                .replace_all(&out, |caps: &regex::Captures| {
                    let word = &caps[0];
                    let value = if word.chars().next().is_some_and(|c| c.is_uppercase()) {
                        capitalise(replacement)
                    } else {
                        replacement.clone()
                    };
                    if value != word {
                        fixes.push((word.to_string(), value.clone()));
                    }
                    value
                })
                .into_owned();
        }
        (out, fixes)
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let mut out = c.to_uppercase().collect::<String>();
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}
