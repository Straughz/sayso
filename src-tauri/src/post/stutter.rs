//! Collapse immediately repeated function words ("we'll we'll" -> "we'll").
//! Port of `src/vox/post/stutter.py`.
//!
//! The Python implementation uses a regex with a backreference and lookaround,
//! which the Rust `regex` crate does not support. This is an explicit scan
//! that reproduces the same conservative behaviour: only words in `WORDS`
//! collapse, and a repeat next to an apostrophe or hyphen ("to to-do") is left
//! alone.

/// Deliberately conservative word list (spelled-out sequences, emphasis and
/// legitimate doubles are left untouched). `you`, `is`, `so`, `a`, `i` and `it`
/// are left out on purpose: "I told you you should wait", "what it is is",
/// "so so" and "set a a b" are real speech.
const WORDS: &[&str] = &[
    "an", "the", "to", "of", "in", "on", "for", "with", "and", "but", "or",
    "then", "if", "we", "they", "he", "she", "my", "our",
    "this", "can", "will", "would", "should", "could", "i'll", "i'm", "i'd", "we'll",
    "we're", "it's", "let's",
];

pub struct StutterRemover;

impl StutterRemover {
    pub fn new() -> Self {
        StutterRemover
    }

    pub fn process(&self, text: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if let Some((wlen, word)) = match_word_at(&chars, i) {
                if valid_preceding(&chars, i) {
                    let mut end = i + wlen;
                    let mut count = 1usize;
                    loop {
                        let mut j = end;
                        while j < chars.len() && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j == end {
                            break; // no whitespace between repeats
                        }
                        if let Some((w2len, w2)) = match_word_at(&chars, j) {
                            if w2.eq_ignore_ascii_case(&word) {
                                let after = j + w2len;
                                if after < chars.len() && is_word_char(chars[after]) {
                                    break;
                                }
                                end = after;
                                count += 1;
                                continue;
                            }
                        }
                        break;
                    }
                    if count >= 2 {
                        out.push_str(&word);
                        i = end;
                        continue;
                    }
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }
}

impl Default for StutterRemover {
    fn default() -> Self {
        Self::new()
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\'' || c == '-'
}

fn valid_preceding(chars: &[char], i: usize) -> bool {
    i == 0 || !is_word_char(chars[i - 1])
}

/// Longest `WORDS` entry matching (ASCII case-insensitive) at `i`, if any.
fn match_word_at(chars: &[char], i: usize) -> Option<(usize, String)> {
    let mut best: Option<(usize, String)> = None;
    for w in WORDS {
        let wc: Vec<char> = w.chars().collect();
        if i + wc.len() > chars.len() {
            continue;
        }
        let ok = wc
            .iter()
            .zip(&chars[i..i + wc.len()])
            .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if ok && best.as_ref().map_or(true, |(len, _)| wc.len() > *len) {
            best = Some((wc.len(), chars[i..i + wc.len()].iter().collect()));
        }
    }
    best
}
