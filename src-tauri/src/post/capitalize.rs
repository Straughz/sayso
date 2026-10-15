//! Capitalisation fixes for ASR output. Port of `src/vox/post/capitalize.py`.

use regex::Regex;

pub struct Capitalizer {
    sent_end: Regex,
    proper_names: Vec<String>,
    proper_re: Vec<Regex>,
}

impl Capitalizer {
    pub fn new(proper_names: Vec<String>) -> Self {
        let proper_re = proper_names
            .iter()
            .map(|n| {
                Regex::new(&format!(r"\b{}\b", regex::escape(&n.to_lowercase())))
                    .expect("valid proper-name regex")
            })
            .collect();
        Capitalizer {
            // The whitespace is captured and re-emitted, so a line break after
            // a full stop survives.
            sent_end: Regex::new(r"([.!?])(\s+)([a-z])").expect("valid sent-end regex"),
            proper_names,
            proper_re,
        }
    }

    pub fn process(&self, text: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }

        // Capitalise the first character.
        let mut out = capitalise_first(text);

        // Capitalise after sentence-ending punctuation.
        out = self
            .sent_end
            .replace_all(&out, |caps: &regex::Captures| {
                format!("{}{}{}", &caps[1], &caps[2], caps[3].to_uppercase())
            })
            .into_owned();

        // "i" → "I" only as the pronoun: a lone "i" between whitespace (or
        // the text edges) or before an apostrophe ("i'm"). "i.imgur.com" and
        // "i-node" are left alone.
        out = capitalise_lone_i(&out);

        // Apply proper names.
        for (re, name) in self.proper_re.iter().zip(self.proper_names.iter()) {
            out = re.replace_all(&out, name.as_str()).into_owned();
        }

        out
    }
}

fn capitalise_lone_i(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (k, &c) in chars.iter().enumerate() {
        let before_ok = k == 0 || chars[k - 1].is_whitespace();
        let after_ok = match chars.get(k + 1) {
            None => true,
            Some(&n) => n.is_whitespace() || n == '\'' || n == '\u{2019}',
        };
        if c == 'i' && before_ok && after_ok {
            out.push('I');
        } else {
            out.push(c);
        }
    }
    out
}

fn capitalise_first(text: &str) -> String {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return String::new(),
    };
    let rest: String = chars.collect();
    if rest.is_empty() {
        first.to_uppercase().collect()
    } else {
        let mut s = first.to_uppercase().collect::<String>();
        s.push_str(&rest);
        s
    }
}
