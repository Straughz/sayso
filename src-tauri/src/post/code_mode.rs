//! Code mode: maps spoken words to programming symbols. Port of
//! `src/vox/post/code_mode.py`.

use regex::Regex;

/// Insertion order is significant: Python sorts a dict by key length
/// (descending) with a stable sort, so equal-length keys keep insertion order.
const RAW_MAPPINGS: &[(&str, &str)] = &[
    ("open paren", "("),
    ("close paren", ")"),
    ("open bracket", "["),
    ("close bracket", "]"),
    ("open brace", "{"),
    ("close brace", "}"),
    ("less than", "<"),
    ("greater than", ">"),
    ("single quote", "'"),
    ("colon equals", ":="),
    ("fat arrow", "=>"),
    ("semi colon", ";"),
    ("at sign", "@"),
    ("new line", "\n"),
    ("equals", "="),
    ("colon", ":"),
    ("dot", "."),
    ("dash", "-"),
    ("underscore", "_"),
    ("star", "*"),
    ("slash", "/"),
    ("backslash", "\\"),
    ("pipe", "|"),
    ("ampersand", "&"),
    ("hash", "#"),
    ("tilde", "~"),
    ("caret", "^"),
    ("percent", "%"),
    ("dollar", "$"),
    ("exclamation", "!"),
    ("question", "?"),
    ("quote", "\""),
    ("backtick", "`"),
    ("arrow", "->"),
    ("tab", "\t"),
    ("comma", ","),
    ("space", " "),
];

pub struct CodeFormatter {
    mappings: Vec<(Regex, String)>,
    #[allow(dead_code)]
    spoken_keys: Vec<String>,
}

impl CodeFormatter {
    pub fn new() -> Self {
        let mut items: Vec<(&str, &str)> = RAW_MAPPINGS.to_vec();
        items.sort_by_key(|x| std::cmp::Reverse(x.0.len()));
        let spoken_keys = items.iter().map(|(k, _)| k.to_string()).collect();
        let mappings = items
            .into_iter()
            .filter_map(|(spoken, symbol)| {
                Regex::new(&format!(r"\b{}\b", regex::escape(spoken)))
                    .ok()
                    .map(|re| (re, symbol.to_string()))
            })
            .collect();
        CodeFormatter { mappings, spoken_keys }
    }

    /// Spoken keys in application order (longest first) — used by tests to
    /// assert the sort, mirroring the Python `CODE_MAPPINGS` test.
    #[allow(dead_code)]
    pub fn spoken_keys(&self) -> &[String] {
        &self.spoken_keys
    }

    pub fn format(&self, text: &str) -> String {
        let mut result = text.to_lowercase();
        for (re, symbol) in &self.mappings {
            let sym = symbol.clone();
            result = re.replace_all(&result, move |_caps: &regex::Captures| sym.clone()).into_owned();
        }
        result
    }
}

impl Default for CodeFormatter {
    fn default() -> Self {
        Self::new()
    }
}
