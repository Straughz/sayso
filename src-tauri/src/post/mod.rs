//! Post-processing pipeline — a direct port of `src/vox/post/` plus the
//! `_PostWrapper` ordering from `src/vox/app.py`.
//!
//! Order (per dictation): snippets → hallucination guard → stutter → optional
//! fillers → optional capitalisation → dictionary → snippets again, then code
//! mode (when enabled), then a trailing space for the *typed* text only.

pub mod backtrack;
pub mod capitalize;
pub mod code_mode;
pub mod dictionary;
pub mod fillers;
pub mod guard;
pub mod snippets;
pub mod stutter;

#[cfg(test)]
mod tests;

pub use backtrack::BacktrackDetector;
pub use code_mode::CodeFormatter;

use capitalize::Capitalizer;
use dictionary::CustomDictionary;
use fillers::FillerRemover;
use guard::HallucinationGuard;
use snippets::SnippetExpander;
use stutter::StutterRemover;

pub struct PostPipeline {
    guard: HallucinationGuard,
    snippets: SnippetExpander,
    stutter: StutterRemover,
    fillers: Option<FillerRemover>,
    cap: Option<Capitalizer>,
    dict: CustomDictionary,
    /// Automatic replacements learned by the background review. Applied after
    /// the user dictionary, so a user entry for the same word wins.
    auto: CustomDictionary,
}

impl PostPipeline {
    pub fn new(
        remove_fillers: bool,
        auto_capitalize: bool,
        dict_json: Option<&str>,
        snippets_json: Option<&str>,
    ) -> Self {
        PostPipeline {
            guard: HallucinationGuard::new(1, 0.5),
            snippets: SnippetExpander::new(snippets_json),
            stutter: StutterRemover::new(),
            fillers: if remove_fillers { Some(FillerRemover::new(None)) } else { None },
            cap: if auto_capitalize { Some(Capitalizer::new(Vec::new())) } else { None },
            dict: CustomDictionary::new(dict_json),
            auto: CustomDictionary::empty(),
        }
    }

    pub fn with_auto(mut self, auto_json: Option<&str>) -> Self {
        self.auto = CustomDictionary::new(auto_json);
        self
    }

    #[allow(dead_code)]
    pub fn run(&self, text: &str) -> String {
        self.run_traced(text).0
    }

    /// Runs the pipeline and also returns the automatic replacements that
    /// changed the text, as `(typed, meant)` pairs for the history entry.
    pub fn run_traced(&self, text: &str) -> (String, Vec<(String, String)>) {
        let text = text.trim();
        if text.is_empty() {
            return (String::new(), Vec::new());
        }

        // Snippet expansion first — a single-word trigger like "sig" is valid.
        let expanded = self.snippets.process(text);
        if expanded != text {
            return (expanded, Vec::new());
        }

        if !self.guard.is_valid(text) {
            return (String::new(), Vec::new());
        }

        let mut work = self.stutter.process(text);
        if let Some(f) = &self.fillers {
            work = f.process(&work);
        }
        if let Some(c) = &self.cap {
            work = c.process(&work);
        }
        work = self.dict.process(&work);
        let (work, auto_fixes) = self.auto.process_traced(&work);

        // Final snippet check after cleaning (e.g. "sig." cleaned to "sig").
        let cleaned_key = work
            .trim_end_matches(['.', '!', '?', ','])
            .trim();
        let expanded_final = self.snippets.process(cleaned_key);
        if expanded_final != cleaned_key {
            return (expanded_final, auto_fixes);
        }

        (work, auto_fixes)
    }

    /// True when `run` would drop this text because the guard saw a
    /// repetition loop (a snippet trigger is never rejected).
    pub fn rejected_as_repeat(&self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || self.snippets.process(text) != text {
            return false;
        }
        self.guard.check(text) == Err(guard::Reject::Repetition)
    }
}

/// `_PostWrapper`: PostPipeline → CodeFormatter → trailing space. The LLM style
/// rewriter is intentionally not ported (dropped from the Rust rewrite).
pub struct PostProcessor {
    post: Option<PostPipeline>,
    code: Option<CodeFormatter>,
    trailing_space: bool,
}

impl PostProcessor {
    pub fn new(
        post_process: bool,
        remove_fillers: bool,
        auto_capitalize: bool,
        dict_json: Option<&str>,
        snippets_json: Option<&str>,
        code_mode: bool,
        trailing_space: bool,
        auto_json: Option<&str>,
    ) -> Self {
        PostProcessor {
            post: if post_process {
                Some(
                    PostPipeline::new(remove_fillers, auto_capitalize, dict_json, snippets_json)
                        .with_auto(auto_json),
                )
            } else {
                None
            },
            code: if code_mode { Some(CodeFormatter::new()) } else { None },
            trailing_space,
        }
    }

    /// Build from the live settings. `auto_json` is the automatic-replacement
    /// dictionary (already serialized) or `None`.
    pub fn from_config(cfg: &crate::store::Config, auto_json: Option<&str>) -> Self {
        let dictionary = crate::store::load_or_embedded(
            &cfg.dictionary_path(),
            crate::DICTIONARY_JSON,
            "dictionary",
        );
        let snippets = crate::store::load_or_embedded(
            &cfg.snippets_path(),
            crate::SNIPPETS_JSON,
            "snippets",
        );
        PostProcessor::new(
            cfg.post_process(),
            cfg.remove_fillers(),
            cfg.auto_capitalize(),
            Some(&dictionary),
            Some(&snippets),
            cfg.code_mode(),
            cfg.trailing_space(),
            auto_json,
        )
    }

    #[allow(dead_code)]
    pub fn run(&self, text: &str) -> String {
        self.run_traced(text).0
    }

    pub fn run_traced(&self, text: &str) -> (String, Vec<(String, String)>) {
        let (mut out, fixes) = match &self.post {
            Some(p) => p.run_traced(text),
            None => (text.to_string(), Vec::new()),
        };
        if let Some(code) = &self.code {
            if !out.is_empty() {
                out = code.format(&out);
            }
        }
        (out, fixes)
    }

    /// See `PostPipeline::rejected_as_repeat`; false when clean-up is off.
    pub fn rejected_as_repeat(&self, text: &str) -> bool {
        self.post.as_ref().is_some_and(|p| p.rejected_as_repeat(text))
    }

    /// Add typing whitespace after post-processing, without changing the
    /// history text.
    pub fn for_injection(&self, text: &str) -> String {
        if !text.is_empty() && self.trailing_space && !text.chars().last().is_some_and(|c| c.is_whitespace()) {
            format!("{text} ")
        } else {
            text.to_string()
        }
    }
}
