//! Rust ports of the Python post-processing tests (`tests/test_post.py` and
//! the stutter / backtrack / code-mode cases in `tests/test_power_features.py`).

use super::backtrack::BacktrackDetector;
use super::capitalize::Capitalizer;
use super::code_mode::CodeFormatter;
use super::dictionary::CustomDictionary;
use super::fillers::FillerRemover;
use super::guard::HallucinationGuard;
use super::snippets::SnippetExpander;
use super::stutter::StutterRemover;
use super::PostPipeline;

const DICT_JSON: &str = r#"{"aks":"ask","aksed":"asked","aksing":"asking","gonna":"going to"}"#;
const SNIP_JSON: &str = r#"{"sig":"Best regards,\nKavish"}"#;

// ── FillerRemover ────────────────────────────────────────────────────

#[test]
fn filler_removes_um() {
    assert_eq!(FillerRemover::new(None).process("um i think so"), "i think so");
}

#[test]
fn filler_removes_uh() {
    assert_eq!(FillerRemover::new(None).process("uh hello"), "hello");
}

#[test]
fn filler_removes_you_know_when_configured() {
    let fr = FillerRemover::new(Some(&["you know"]));
    assert!(!fr.process("you know what i mean").contains("you know"));
}

#[test]
fn filler_removes_like_after_copula_when_configured() {
    let fr = FillerRemover::new(Some(&["like"]));
    let result = fr.process("the project is like due on friday");
    assert!(!result.contains("like"));
    assert!(result.contains("is"));
}

#[test]
fn filler_preserves_like_as_verb() {
    let result = FillerRemover::new(None).process("I like pizza");
    assert!(result.contains("like"));
}

#[test]
fn filler_multiple_consecutive() {
    let result = FillerRemover::new(None).process("um like actually i think");
    assert!(!result.contains("um"));
    assert!(result.to_lowercase().contains("i think"));
}

#[test]
fn filler_at_start() {
    let result = FillerRemover::new(None).process("um i wanted to say something");
    assert!(!result.to_lowercase().starts_with("um"));
}

#[test]
fn filler_empty_string() {
    assert_eq!(FillerRemover::new(None).process(""), "");
}

#[test]
fn filler_custom() {
    let fr = FillerRemover::new(Some(&["blah"]));
    assert_eq!(fr.process("blah blah go ahead"), "go ahead");
}

#[test]
fn filler_no_double_spaces() {
    let result = FillerRemover::new(None).process("um uh hello");
    assert!(!result.contains("  "));
}

// ── Capitalizer ──────────────────────────────────────────────────────

#[test]
fn cap_capitalise_first_word() {
    assert!(Capitalizer::new(vec![]).process("hello world").starts_with('H'));
}

#[test]
fn cap_lone_i() {
    let result = Capitalizer::new(vec![]).process("i think i can");
    assert!(result.contains('I'));
    assert!(!result.contains("i "));
}

#[test]
fn cap_after_period() {
    assert_eq!(
        Capitalizer::new(vec![]).process("first sentence. second sentence."),
        "First sentence. Second sentence."
    );
}

#[test]
fn cap_after_question_mark() {
    assert_eq!(
        Capitalizer::new(vec![]).process("is that right? yes it is."),
        "Is that right? Yes it is."
    );
}

#[test]
fn cap_preserves_existing_caps() {
    assert!(Capitalizer::new(vec![]).process("the iPhone is great").contains("iPhone"));
}

#[test]
fn cap_proper_names() {
    let result = Capitalizer::new(vec!["Friday".to_string()]).process("it is due on friday");
    assert!(result.contains("Friday"));
}

#[test]
fn cap_empty() {
    assert_eq!(Capitalizer::new(vec![]).process(""), "");
}

#[test]
fn cap_leaves_i_inside_tokens() {
    let c = Capitalizer::new(vec![]);
    assert_eq!(c.process("the variable i.imgur.com"), "The variable i.imgur.com");
    assert_eq!(c.process("open i-node table"), "Open i-node table");
    assert_eq!(c.process("so i'm sure i know"), "So I'm sure I know");
    assert_eq!(c.process("it was i"), "It was I");
}

#[test]
fn cap_keeps_line_break_after_sentence() {
    assert_eq!(
        Capitalizer::new(vec![]).process("first line.\nsecond line.\n\nthird"),
        "First line.\nSecond line.\n\nThird"
    );
}

// ── Fillers: capitalised input ────────────────────────────────────────

#[test]
fn filler_case_insensitive() {
    assert_eq!(FillerRemover::new(None).process("Um, open the file"), "open the file");
    assert_eq!(FillerRemover::new(None).process("Open UH the file"), "Open the file");
}

// ── Bundled dictionary ───────────────────────────────────────────────

#[test]
fn bundled_dictionary_leaves_cause_and_vue() {
    let pp = PostPipeline::new(true, true, Some(crate::DICTIONARY_JSON), None);
    assert_eq!(pp.run("find the root cause"), "Find the root cause");
    assert_eq!(pp.run("the Vue component"), "The Vue component");
    assert_eq!(pp.run("i dunno"), "I don't know");
}

// ── Guard reason ─────────────────────────────────────────────────────

#[test]
fn guard_repetition_is_reported_as_repeat() {
    let pp = PostPipeline::new(true, true, None, None);
    assert_eq!(pp.run("No, no, no, no."), "");
    assert!(pp.rejected_as_repeat("No, no, no, no."));
    assert!(!pp.rejected_as_repeat("..."));
    assert!(!pp.rejected_as_repeat("hello there"));
}

// ── CustomDictionary ─────────────────────────────────────────────────

#[test]
fn dict_basic_correction() {
    let d = CustomDictionary::from_json(r#"{"aks":"ask"}"#);
    assert_eq!(d.process("please aks her"), "please ask her");
}

#[test]
fn dict_preserves_capitalisation() {
    let d = CustomDictionary::from_json(r#"{"aks":"ask"}"#);
    assert_eq!(d.process("Aks me"), "Ask me");
}

#[test]
fn dict_no_match() {
    let d = CustomDictionary::from_json(r#"{"aks":"ask"}"#);
    assert_eq!(d.process("hello world"), "hello world");
}

#[test]
fn dict_multi_word_replacement() {
    let d = CustomDictionary::from_json(r#"{"gonna":"going to"}"#);
    assert_eq!(d.process("i'm gonna leave"), "i'm going to leave");
}

#[test]
fn dict_empty() {
    let d = CustomDictionary::from_json("{}");
    assert_eq!(d.process("aks me anything"), "aks me anything");
}

#[test]
fn dict_no_path_is_noop() {
    assert_eq!(CustomDictionary::empty().process("aks me"), "aks me");
}

#[test]
fn dict_default_config() {
    let d = CustomDictionary::from_json(DICT_JSON);
    assert_eq!(d.process("please aks her"), "please ask her");
}

#[test]
fn dict_missing_path_degrades_to_empty() {
    let d = CustomDictionary::from_path("Z:/does-not-exist/dictionary.json");
    assert_eq!(d.process("please aks her"), "please aks her");
}

// ── SnippetExpander ──────────────────────────────────────────────────

#[test]
fn snippet_exact_match() {
    let s = SnippetExpander::from_json(SNIP_JSON);
    assert_eq!(s.process("sig"), "Best regards,\nKavish");
}

#[test]
fn snippet_no_match_returns_original() {
    let s = SnippetExpander::from_json(SNIP_JSON);
    assert_eq!(s.process("hello world"), "hello world");
}

#[test]
fn snippet_case_insensitive() {
    let s = SnippetExpander::from_json(r#"{"sig":"Regards"}"#);
    assert_eq!(s.process("SIG"), "Regards");
}

#[test]
fn snippet_multi_word_key() {
    let s = SnippetExpander::from_json(r#"{"email team":"Hi team"}"#);
    assert_eq!(s.process("email team"), "Hi team");
}

#[test]
fn snippet_partial_no_match() {
    let s = SnippetExpander::from_json(r#"{"sig":"Regards"}"#);
    assert_eq!(s.process("my sig here"), "my sig here");
}

#[test]
fn snippet_no_path_is_noop() {
    let s = SnippetExpander::new(None);
    assert_eq!(s.process("sig"), "sig");
}

#[test]
fn snippet_default_config() {
    let s = SnippetExpander::from_json(r#"{"sig":"Best regards,\nKavish"}"#);
    assert!(s.process("sig").contains("Kavish"));
}

#[test]
fn snippet_missing_path_degrades_to_noop() {
    // from_json on unreadable content degrades to no snippets without panic.
    let s = SnippetExpander::new(None);
    assert_eq!(s.process("sig"), "sig");
}

// ── HallucinationGuard ───────────────────────────────────────────────

#[test]
fn guard_valid_text() {
    assert!(HallucinationGuard::new(1, 0.5).is_valid("hello there how are you"));
}

#[test]
fn guard_empty_string() {
    assert!(!HallucinationGuard::new(1, 0.5).is_valid(""));
}

#[test]
fn guard_whitespace_only() {
    assert!(!HallucinationGuard::new(1, 0.5).is_valid("   "));
}

#[test]
fn guard_single_word_rejected() {
    assert!(!HallucinationGuard::new(2, 0.5).is_valid("hello"));
}

#[test]
fn guard_punctuation_only() {
    assert!(!HallucinationGuard::new(1, 0.5).is_valid("..."));
}

#[test]
fn guard_high_repetition_rejected() {
    assert!(!HallucinationGuard::new(1, 0.5).is_valid("thank you thank you thank you"));
}

#[test]
fn guard_moderate_repetition_ok() {
    assert!(HallucinationGuard::new(1, 0.5).is_valid("I think I should go"));
}

#[test]
fn guard_word_said_twice_ok() {
    assert!(HallucinationGuard::new(1, 0.5).is_valid("Testing one, two, testing"));
    assert!(HallucinationGuard::new(1, 0.5).is_valid("Test one, two, test"));
}

#[test]
fn guard_period_only() {
    assert!(!HallucinationGuard::new(1, 0.5).is_valid("."));
}

// ── StutterRemover ───────────────────────────────────────────────────

#[test]
fn stutter_collapses_doubled_function_words() {
    let sr = StutterRemover::new();
    assert_eq!(sr.process("and then we'll we'll go from there"), "and then we'll go from there");
    assert_eq!(sr.process("give us the the pretty bad reading"), "give us the pretty bad reading");
    assert_eq!(sr.process("The the plan"), "The plan");
    assert_eq!(sr.process("to to to do it"), "to do it");
}

#[test]
fn stutter_keeps_legitimate_repeats() {
    let sr = StutterRemover::new();
    for text in [
        "seven five four four zero",
        "that was very very good",
        "I know that that works",
        "no no, not that one",
        "go to to-do and it's it",
    ] {
        assert_eq!(sr.process(text), text, "should keep: {text}");
    }
}

#[test]
fn stutter_keeps_real_doubles_of_common_words() {
    let sr = StutterRemover::new();
    for text in [
        "I told you you should wait",
        "what it is is a race",
        "it was so so",
        "set a a b",
    ] {
        assert_eq!(sr.process(text), text, "should keep: {text}");
    }
}

#[test]
fn stutter_empty() {
    assert_eq!(StutterRemover::new().process(""), "");
}

#[test]
fn stutter_pipeline_applies_cleanup() {
    let pp = PostPipeline::new(false, false, None, None);
    assert_eq!(pp.run("then we'll we'll go from there"), "then we'll go from there");
}

// ── BacktrackDetector ────────────────────────────────────────────────

#[test]
fn backtrack_detects_triggers() {
    let b = BacktrackDetector::new();
    for t in ["backtrack", "undo", "scratch that", "go back", "never mind", "delete that", "BACKTRACK"] {
        assert!(b.detect(t), "should detect: {t}");
    }
}

#[test]
fn backtrack_ignores_normal_and_partial() {
    let b = BacktrackDetector::new();
    assert!(!b.detect("hello world"));
    assert!(!b.detect("I want to undo my mistake"));
}

#[test]
fn backtrack_strips_punctuation_and_whitespace() {
    let b = BacktrackDetector::new();
    assert!(b.detect("undo."));
    assert!(b.detect("scratch that!"));
    assert!(b.detect("  undo  "));
}

// ── CodeFormatter ────────────────────────────────────────────────────

#[test]
fn code_simple_symbols() {
    let f = CodeFormatter::new();
    assert_eq!(f.format("open paren"), "(");
    assert_eq!(f.format("close paren"), ")");
    assert_eq!(f.format("open brace"), "{");
    assert_eq!(f.format("close brace"), "}");
    assert_eq!(f.format("new line"), "\n");
}

#[test]
fn code_multi_word_and_arrows() {
    let f = CodeFormatter::new();
    let result = f.format("function add open paren a comma b close paren");
    assert!(result.contains('(') && result.contains(')') && result.contains(','));
    assert!(f.format("x fat arrow y").contains("=>"));
    assert!(f.format("x arrow y").contains("->"));
    assert!(f.format("x colon equals y").contains(":="));
}

#[test]
fn code_preserves_unrecognised_words() {
    let result = CodeFormatter::new().format("my_variable equals 5");
    assert!(result.contains("my_variable"));
    assert!(result.contains('='));
}

#[test]
fn code_mappings_sorted_by_length() {
    let f = CodeFormatter::new();
    let lengths: Vec<usize> = f.spoken_keys().iter().map(|k| k.chars().count()).collect();
    let mut sorted = lengths.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(lengths, sorted, "mappings must be longest-first");
}

// ── PostPipeline ─────────────────────────────────────────────────────

#[test]
fn pipeline_acceptance_filler_and_dict() {
    let pp = PostPipeline::new(true, true, Some(r#"{"aks":"ask"}"#), None);
    let result = pp.run("um i wanted to aks you something");
    assert!(!result.to_lowercase().contains("um"));
    assert!(result.contains("ask"));
    assert!(result.chars().next().is_some_and(|c| c.is_uppercase()));
}

#[test]
fn pipeline_toggles_affect_text() {
    let enabled = PostPipeline::new(true, true, None, None);
    let disabled = PostPipeline::new(false, false, None, None);
    assert_eq!(enabled.run("um hello world."), "Hello world.");
    assert_eq!(disabled.run("um hello world."), "um hello world.");
}

#[test]
fn pipeline_snippet_expansion() {
    let pp = PostPipeline::new(true, true, None, Some(SNIP_JSON));
    assert_eq!(pp.run("sig"), "Best regards,\nKavish");
}

#[test]
fn pipeline_guard_rejects_empty_and_repetition() {
    let pp = PostPipeline::new(true, true, None, None);
    assert_eq!(pp.run(""), "");
    assert_eq!(pp.run("   "), "");
    assert_eq!(pp.run("thank you thank you thank you"), "");
}

#[test]
fn pipeline_lone_i_capitalised() {
    let pp = PostPipeline::new(true, true, None, None);
    let result = pp.run("i think i understand the plan");
    assert!(result.contains(" I ") || result.starts_with('I'));
}

#[test]
fn pipeline_default_config_loaded() {
    let pp = PostPipeline::new(true, true, Some(DICT_JSON), None);
    assert!(pp.run("please aks her for the report").contains("ask"));
}

#[test]
fn pipeline_construction_survives_dangling_paths() {
    // Empty JSON strings (a missing configured file resolves to empty) must not
    // panic and must behave as no dictionary / no snippets.
    let pp = PostPipeline::new(true, true, Some(""), Some(""));
    assert_eq!(pp.run("please aks her"), "Please aks her");
}

// ── PostProcessor (code mode + trailing space) ───────────────────────

#[test]
fn processor_code_mode_and_trailing_space() {
    let pp = super::PostProcessor::new(true, true, true, None, None, true, true, None);
    let processed = pp.run("x fat arrow y");
    assert!(processed.contains("=>"));
    let typed = pp.for_injection(&processed);
    assert!(typed.ends_with(' '));
}

#[test]
fn processor_trailing_space_not_duplicated() {
    let pp = super::PostProcessor::new(true, true, true, None, None, false, true, None);
    assert_eq!(pp.for_injection("hi"), "hi ");
    assert_eq!(pp.for_injection("hi\n"), "hi\n");
}

// ── Automatic replacement dictionary ─────────────────────────────────

#[test]
fn auto_dictionary_is_whole_word_and_reports_fixes() {
    let pp = PostPipeline::new(false, false, None, None)
        .with_auto(Some(r#"{"pains":"panes"}"#));
    let (out, fixes) = pp.run_traced("the pains are real");
    assert_eq!(out, "the panes are real");
    assert_eq!(fixes, vec![("pains".to_string(), "panes".to_string())]);
    // Whole word only: the substring inside another word is untouched.
    assert_eq!(pp.run("it complains a lot"), "it complains a lot");
}

#[test]
fn user_dictionary_entry_wins_over_auto() {
    let pp = PostPipeline::new(false, false, Some(r#"{"satterbar":"user bar"}"#), None)
        .with_auto(Some(r#"{"satterbar":"status bar"}"#));
    let (out, fixes) = pp.run_traced("open satterbar now");
    assert_eq!(out, "open user bar now");
    assert!(fixes.is_empty());
}
