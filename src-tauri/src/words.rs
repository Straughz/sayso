//! English word check (stage 2).
//!
//! `assets/words_alpha.txt` (about 370,000 lines, one lower-case a-z word each)
//! is embedded at compile time and indexed lazily once. Nothing here logs.

use std::collections::HashSet;
use std::sync::OnceLock;

static WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();

fn words() -> &'static HashSet<&'static str> {
    WORDS.get_or_init(|| {
        include_str!("../assets/words_alpha.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect()
    })
}

/// Lower-case lookup in the embedded list.
pub fn is_english_word(word: &str) -> bool {
    let lower = word.to_lowercase();
    words().contains(lower.as_str())
}

/// True when every whitespace-separated token, stripped of non-letters, is in
/// the list. A token with no letters counts as English.
pub fn is_all_english(phrase: &str) -> bool {
    phrase.split_whitespace().all(|token| {
        let letters: String = token.chars().filter(|c| c.is_alphabetic()).collect();
        letters.is_empty() || is_english_word(&letters)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_words_are_english_and_invented_ones_are_not() {
        assert!(is_english_word("setup"));
        assert!(is_english_word("Portfolio"));
        assert!(is_english_word("Pains"));
        assert!(!is_english_word("satterbar"));
        assert!(!is_english_word("Kavish"));
        assert!(!is_english_word("NVG"));
    }

    #[test]
    fn all_english_requires_every_lettered_token() {
        assert!(is_all_english("Setup"));
        assert!(is_all_english("Install Me"));
        assert!(is_all_english("123 456"));
        assert!(!is_all_english("NVG Talk"));
        assert!(!is_all_english("Kavish Resume"));
        assert!(!is_all_english("satterbar panel"));
    }
}
