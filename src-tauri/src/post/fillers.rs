//! Remove filler words from transcribed text. Port of `src/vox/post/fillers.py`.

use regex::Regex;

pub const DEFAULT_FILLERS: &[&str] = &["um", "uh"];

pub struct FillerRemover {
    has_like: bool,
    base: Option<Regex>,
    like_copula: Regex,
    like_start: Regex,
    multi_space: Regex,
    leading_junk: Regex,
}

impl FillerRemover {
    pub fn new(fillers: Option<&[&str]>) -> Self {
        let list: Vec<String> = fillers
            .unwrap_or(DEFAULT_FILLERS)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let has_like = list.iter().any(|f| f.to_lowercase() == "like");
        let mut others: Vec<String> = list
            .into_iter()
            .filter(|f| f.to_lowercase() != "like")
            .collect();

        // Multi-word fillers first so they match before sub-phrases.
        let mut multi: Vec<String> = others.iter().filter(|f| f.contains(' ')).cloned().collect();
        multi.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let single: Vec<String> = others.drain(..).filter(|f| !f.contains(' ')).collect();
        let ordered: Vec<String> = multi.into_iter().chain(single).collect();

        let base = if ordered.is_empty() {
            None
        } else {
            let alts = ordered
                .iter()
                .map(|f| regex::escape(f))
                .collect::<Vec<_>>()
                .join("|");
            // Case-insensitive like the Python original: xAI returns formatted
            // text, so a leading filler arrives capitalised ("Um, open ...").
            Regex::new(&format!(r"(?i)(?:^|\b)(?:{alts})(?:\b)(?:[,\s]*)")).ok()
        };

        FillerRemover {
            has_like,
            base,
            like_copula: Regex::new(
                r"(?i)\b(is|am|are|was|were|be|so|just|all|and|but|or|get|got)\s+like\b[,\s]*",
            )
            .expect("valid like-copula regex"),
            // Python uses a lookbehind `(?<=[.!?])\s+`; capturing the
            // punctuation+space and re-emitting it is equivalent without
            // lookaround.
            like_start: Regex::new(r"(?i)(^|[.!?]\s+)like\b[,\s]*")
                .expect("valid like-start regex"),
            multi_space: Regex::new(r" {2,}").expect("valid multi-space regex"),
            leading_junk: Regex::new(r"^[,\s]+").expect("valid leading-junk regex"),
        }
    }

    pub fn process(&self, text: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();

        if self.has_like {
            out = self
                .like_copula
                .replace_all(&out, |caps: &regex::Captures| format!("{} ", &caps[1]))
                .into_owned();
            out = self
                .like_start
                .replace_all(&out, |caps: &regex::Captures| {
                    caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string()
                })
                .into_owned();
        }

        if let Some(base) = &self.base {
            out = base.replace_all(&out, " ").into_owned();
        }

        out = self.multi_space.replace_all(&out, " ").into_owned();
        out = out.trim().to_string();
        out = self.leading_junk.replace_all(&out, "").into_owned();
        out
    }
}
