//! Exact-utterance snippet expansion. Port of `src/vox/post/snippets.py`.

use regex::Regex;
use std::collections::HashMap;

pub struct SnippetExpander {
    nonword: Regex,
    snippets: HashMap<String, String>,
}

impl SnippetExpander {
    pub fn new(json: Option<&str>) -> Self {
        let mut s = SnippetExpander {
            nonword: Regex::new(r"[^\w\s]").expect("valid snippet regex"),
            snippets: HashMap::new(),
        };
        if let Some(text) = json {
            if !text.trim().is_empty() {
                s.load(text);
            }
        }
        s
    }

    #[allow(dead_code)]
    pub fn from_json(json: &str) -> Self {
        Self::new(Some(json))
    }

    fn load(&mut self, json: &str) {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                crate::log::warn(&format!("snippets: could not parse JSON: {e} — using none"));
                return;
            }
        };
        if let Some(obj) = value.as_object() {
            self.snippets = obj
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|val| (self.normalise(k), val.to_string())))
                .collect();
        }
    }

    fn normalise(&self, text: &str) -> String {
        self.nonword.replace_all(text, "").trim().to_lowercase()
    }

    pub fn process(&self, text: &str) -> String {
        let key = self.normalise(text);
        self.snippets.get(&key).cloned().unwrap_or_else(|| text.to_string())
    }
}
