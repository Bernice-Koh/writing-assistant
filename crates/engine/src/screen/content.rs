//! The content check, which enforces the second invariant: a candidate is shown only when it keeps
//! the facts of the passage it rewrites, so a model is never the source of a fact. Four rules
//! compare the two texts' numbers, capitalised tokens, negation words and lengths, and each reports
//! its own outcome, so the screen can name the rule a candidate failed and the bench can count
//! content failures as a class of their own.
//!
//! Every comparison errs toward rejection. A candidate that fails is regenerated once, so a false
//! failure costs one generation, while a false pass shows a changed fact.

use std::collections::HashMap;
use std::hash::Hash;

use unicode_segmentation::UnicodeSegmentation;

use crate::words::{word_count, words};

/// The length band in tenths of the passage's words, bounds included. Whole tenths compare
/// exactly, which a floating-point ratio does not promise at the bounds.
const LENGTH_BAND_TENTHS: (usize, usize) = (7, 13);

/// The negation words, each its own entry of the multiset except where [`negation_entry`] counts
/// another form under `not`.
const NEGATION_WORDS: [&str; 10] = [
    "not", "no", "never", "none", "nothing", "nobody", "nowhere", "neither", "nor", "without",
];

/// A contraction's negative, split off its word as a token of its own.
const CONTRACTED_NOT: &str = "n't";

/// The other endings a contraction or a possessive adds to a word, each split off its word as a
/// token of its own that counts under no rule, so `Tan's` holds the capitalised token `Tan` and
/// `nothing's` the negation word `nothing`.
const CLITICS: [&str; 6] = ["'s", "'m", "'re", "'ve", "'d", "'ll"];

/// Whether one rule of the content check held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
}

/// Each rule's own outcome. It carries no text from either side, so a log line or a stored result
/// that holds it holds no draft text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentResult {
    pub numbers: Outcome,
    pub capitalised_tokens: Outcome,
    pub negations: Outcome,
    pub length: Outcome,
}

impl ContentResult {
    /// Whether all four rules passed.
    pub fn passed(&self) -> bool {
        [
            self.numbers,
            self.capitalised_tokens,
            self.negations,
            self.length,
        ]
        .into_iter()
        .all(|outcome| outcome == Outcome::Pass)
    }
}

/// Checks `candidate` against the `passage` it rewrites by four rules.
///
/// Each right single quotation mark, U+2019, is mapped to a straight apostrophe in both texts
/// before anything is tokenised, since Word turns a typed apostrophe into the curly one and a model
/// writes either. A token is then a word by the engine's one definition, in `crate::words`, with a
/// final `'s`, `'m`, `'re`, `'ve`, `'d` or `'ll` split off as a token of its own, and then a final
/// `n't`, so `isn't` is `is` and `n't`, and `Tan's` is `Tan` and `'s`. A split-off ending other
/// than `n't` counts under no rule.
///
/// - Numbers: the two texts hold the same numbers and the same tokens that hold a digit and are
///   not numbers, with the same count of each. A number is a token of ASCII digits with optional
///   commas, read as thousands separators wherever they fall between digits, and an optional
///   decimal point. Numbers compare by value through a canonical decimal string, never through
///   floating point: the commas stripped, then trailing zeros after the point and a trailing point
///   removed, so `1,000` equals `1000` and `2.50` equals `2.5`. Leading zeros are kept, so `09` and
///   `9` differ. Any other token that holds a digit, by Unicode's numeric property, such as `3pm`,
///   `1990s`, `2nd` or `v1.2.3`, is an entry of its own, compared as written, so `3pm` and `4pm`
///   differ, as do `3pm` and `3 pm`. A number word such as `three` is not counted.
/// - Capitalised tokens: a capitalised token starts with an uppercase letter and is not the
///   pronoun `I`. It is sentence-initial when it is the first word of a sentence under the split
///   the analyzer uses, Unicode's sentence boundaries from `unicode-segmentation`. A capitalised
///   token that is not sentence-initial is required on the other side as many times, at any
///   position there, sentence-initial included, and this holds in both directions. A
///   sentence-initial one is required nowhere, since its capital can come from its position alone.
///   The comparison is case-sensitive, since case is a capitalised token's content, so `Singapore`
///   and `singapore` differ.
/// - Negations: the two texts hold the same negation words, with the same count of each, compared
///   without regard to case. The words are `not`, `no`, `never`, `none`, `nothing`, `nobody`,
///   `nowhere`, `neither`, `nor` and `without`, with `n't` and `cannot` counted as `not`, so a
///   contraction expanded or contracted keeps the count. Every other word is its own entry, so
///   `nobody` and `no one` differ.
/// - Length: the candidate's length in words, by the engine's word count, is inside 0.7 to 1.3
///   times the passage's, bounds included. A candidate of no words fails. An empty passage
///   therefore fails every candidate, since no candidate of one word or more lies in a band of
///   zero words.
pub fn check(passage: &str, candidate: &str) -> ContentResult {
    let passage = passage.replace('\u{2019}', "'");
    let candidate = candidate.replace('\u{2019}', "'");
    let (passage_tokens, candidate_tokens) = (tokens(&passage), tokens(&candidate));
    ContentResult {
        numbers: outcome(numbers(&passage_tokens) == numbers(&candidate_tokens)),
        capitalised_tokens: outcome(
            holds_what_is_required(&candidate_tokens, &passage_tokens)
                && holds_what_is_required(&passage_tokens, &candidate_tokens),
        ),
        negations: outcome(negations(&passage_tokens) == negations(&candidate_tokens)),
        length: outcome(length_in_band(word_count(&passage), word_count(&candidate))),
    }
}

fn outcome(held: bool) -> Outcome {
    if held {
        Outcome::Pass
    } else {
        Outcome::Fail
    }
}

#[derive(Debug)]
struct Token<'t> {
    text: &'t str,
    sentence_initial: bool,
}

/// The tokens of `text`, sentence by sentence.
fn tokens(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    for sentence in text.unicode_sentences() {
        for (index, (_, word)) in words(sentence).enumerate() {
            let (rest, clitic) = CLITICS
                .into_iter()
                .find_map(|clitic| split_ending(word, clitic))
                .map_or((word, None), |(rest, clitic)| (rest, Some(clitic)));
            let (base, contracted_not) = split_ending(rest, CONTRACTED_NOT)
                .map_or((rest, None), |(base, contracted_not)| {
                    (base, Some(contracted_not))
                });
            if !base.is_empty() {
                tokens.push(Token {
                    text: base,
                    sentence_initial: index == 0,
                });
            }
            for ending in [contracted_not, clitic].into_iter().flatten() {
                tokens.push(Token {
                    text: ending,
                    sentence_initial: false,
                });
            }
        }
    }
    tokens
}

/// `word` split before its final `ending`, matched in any case, when it ends with it.
fn split_ending<'w>(word: &'w str, ending: &str) -> Option<(&'w str, &'w str)> {
    word.len()
        .checked_sub(ending.len())
        .and_then(|at| word.split_at_checked(at))
        .filter(|(_, found)| found.eq_ignore_ascii_case(ending))
}

fn multiset<K: Eq + Hash>(items: impl Iterator<Item = K>) -> HashMap<K, usize> {
    let mut counts = HashMap::new();
    for item in items {
        *counts.entry(item).or_insert(0) += 1;
    }
    counts
}

/// One entry of the numbers multiset.
#[derive(Debug, PartialEq, Eq, Hash)]
enum NumberEntry<'t> {
    /// A number, by its canonical decimal string.
    Value(String),
    /// A token that holds a digit and is not a number, as written.
    AsWritten(&'t str),
}

/// Each number of `tokens` by its value and each other token that holds a digit as written, with
/// its count.
fn numbers<'t>(tokens: &[Token<'t>]) -> HashMap<NumberEntry<'t>, usize> {
    multiset(tokens.iter().filter_map(|token| {
        canonical_number(token.text)
            .map(NumberEntry::Value)
            .or_else(|| {
                token
                    .text
                    .chars()
                    .any(char::is_numeric)
                    .then_some(NumberEntry::AsWritten(token.text))
            })
    }))
}

/// `token` as a canonical decimal string, when it is a number, so two numbers of one value are
/// one string.
fn canonical_number(token: &str) -> Option<String> {
    let (integer, fraction) = match token.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (token, None),
    };
    let is_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let well_formed = integer
        .split(',')
        .all(|group| !group.is_empty() && is_digits(group))
        && fraction.is_none_or(is_digits);
    if !well_formed {
        return None;
    }
    let mut canonical = integer.replace(',', "");
    if let Some(fraction) = fraction
        .map(|fraction| fraction.trim_end_matches('0'))
        .filter(|fraction| !fraction.is_empty())
    {
        canonical.push('.');
        canonical.push_str(fraction);
    }
    Some(canonical)
}

fn is_capitalised(token: &str) -> bool {
    token != "I" && token.chars().next().is_some_and(char::is_uppercase)
}

/// Whether `holder` holds each capitalised token that is not sentence-initial in `requirer` at
/// least as many times as `requirer` does, at any position.
fn holds_what_is_required(holder: &[Token], requirer: &[Token]) -> bool {
    let held = multiset(
        holder
            .iter()
            .filter(|token| is_capitalised(token.text))
            .map(|token| token.text),
    );
    multiset(
        requirer
            .iter()
            .filter(|token| !token.sentence_initial && is_capitalised(token.text))
            .map(|token| token.text),
    )
    .into_iter()
    .all(|(token, required)| held.get(token).is_some_and(|&count| count >= required))
}

/// Each negation entry of `tokens`, with its count.
fn negations(tokens: &[Token]) -> HashMap<&'static str, usize> {
    multiset(tokens.iter().filter_map(|token| negation_entry(token.text)))
}

fn negation_entry(token: &str) -> Option<&'static str> {
    let token = token.to_lowercase();
    if token == CONTRACTED_NOT || token == "cannot" {
        return Some("not");
    }
    NEGATION_WORDS.into_iter().find(|&word| word == token)
}

fn length_in_band(passage_words: usize, candidate_words: usize) -> bool {
    let (fewest, most) = LENGTH_BAND_TENTHS;
    candidate_words > 0
        && (fewest * passage_words..=most * passage_words).contains(&(10 * candidate_words))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Outcome::{Fail, Pass};

    const ALL_PASS: ContentResult = ContentResult {
        numbers: Pass,
        capitalised_tokens: Pass,
        negations: Pass,
        length: Pass,
    };

    /// A text of `count` words.
    fn text_of_words(count: usize) -> String {
        vec!["tide"; count].join(" ")
    }

    #[test]
    fn the_same_numbers_pass_the_numbers_rule() {
        let result = check(
            "The ferry leaves at 9 and carries 40 cars for 2.50 each.",
            "For 2.50 each, the ferry carries 40 cars and leaves at 9.",
        );
        assert_eq!(result.numbers, Pass);
    }

    #[test]
    fn a_number_dropped_changed_or_added_fails_the_numbers_rule() {
        let passage = "The ferry leaves at 9 and carries 40 cars.";
        for candidate in [
            "The ferry leaves at 9 and carries cars.",
            "The ferry leaves at 9 and carries 45 cars.",
            "The ferry leaves at 9 and carries 40 cars and 2 vans.",
        ] {
            assert_eq!(check(passage, candidate).numbers, Fail, "{candidate}");
        }
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(
            check(
                "The hall seats 1,000 people.",
                "The hall seats 1000 people."
            )
            .numbers,
            Pass
        );
        assert_eq!(
            check(
                "The fare is 1,250.50 dollars.",
                "The fare is 1250.5 dollars."
            )
            .numbers,
            Pass
        );
    }

    #[test]
    fn a_number_word_is_not_counted() {
        assert_eq!(
            check("We saw three ferries.", "We saw 3 ferries.").numbers,
            Fail
        );
    }

    #[test]
    fn a_token_that_holds_a_digit_and_is_not_a_number_is_its_own_entry_compared_as_written() {
        for token in ["3pm", "1990s", "2nd", "v1.2.3"] {
            let text = format!("It was {token} again.");
            assert_eq!(check(&text, &text).numbers, Pass, "{token}");
        }
        for (passage, candidate) in [
            ("We met at 3pm.", "We met at 4pm."),
            ("We met at 3pm.", "We met at 3 pm."),
            ("We met at 3pm.", "We met at 3PM."),
            ("It was the 1990s.", "It was the 1980s."),
            ("She came 2nd.", "She came 3rd."),
            ("Install v1.2.3 today.", "Install v1.2.4 today."),
            ("Install v1.2.3 today.", "Install v1.2.3 and v1.2.3 today."),
        ] {
            assert_eq!(check(passage, candidate).numbers, Fail, "{candidate}");
        }
    }

    #[test]
    fn leading_zeros_stay() {
        assert_eq!(check("Gate 09 opens.", "Gate 9 opens.").numbers, Fail);
        assert_eq!(check("Gate 09 opens.", "Gate 09 opens.").numbers, Pass);
        assert_eq!(check("Agent 007 left.", "Agent 7 left.").numbers, Fail);
    }

    #[test]
    fn a_number_reads_as_its_canonical_decimal_string() {
        for (token, canonical) in [
            ("1000", "1000"),
            ("1,000", "1000"),
            ("1,250.50", "1250.5"),
            ("2.0", "2"),
            ("0.25", "0.25"),
            ("10", "10"),
            ("09", "09"),
        ] {
            assert_eq!(
                canonical_number(token).as_deref(),
                Some(canonical),
                "{token}"
            );
        }
        for token in ["three", "3pm", "1990s", "1.2.3", "v2"] {
            assert_eq!(canonical_number(token), None, "{token}");
        }
    }

    #[test]
    fn numbers_compare_as_a_multiset_in_any_order() {
        assert_eq!(
            check("It rose from 4 to 7.", "It reached 7, up from 4.").numbers,
            Pass
        );
        assert_eq!(
            check(
                "It rose from 4 to 7 and fell to 4.",
                "It rose from 4 to 7 and fell."
            )
            .numbers,
            Fail
        );
    }

    #[test]
    fn a_mid_sentence_capitalised_token_kept_passes_the_capitalised_tokens_rule() {
        assert_eq!(
            check(
                "We flew to Singapore last week.",
                "Last week we flew to Singapore."
            )
            .capitalised_tokens,
            Pass
        );
    }

    #[test]
    fn a_mid_sentence_capitalised_token_dropped_or_added_fails_the_capitalised_tokens_rule() {
        let passage = "We flew to Singapore last week.";
        assert_eq!(
            check(passage, "We flew home last week.").capitalised_tokens,
            Fail
        );
        assert_eq!(
            check(passage, "We flew to Singapore with Tan last week.").capitalised_tokens,
            Fail
        );
    }

    #[test]
    fn a_name_twice_in_the_passage_and_once_in_the_candidate_fails_the_capitalised_tokens_rule() {
        assert_eq!(
            check(
                "We asked Tan, and then Tan asked us.",
                "We asked Tan, and then he asked us."
            )
            .capitalised_tokens,
            Fail
        );
    }

    #[test]
    fn capitalised_tokens_compare_case_sensitively() {
        assert_eq!(
            check(
                "We flew to Singapore last week.",
                "We flew to singapore last week."
            )
            .capitalised_tokens,
            Fail
        );
    }

    #[test]
    fn a_word_capitalised_only_because_it_starts_a_sentence_is_not_required() {
        assert_eq!(
            check(
                "The ferry is late. It waits for the tide.",
                "Our ferry is late. Again it waits for the tide."
            )
            .capitalised_tokens,
            Pass
        );
    }

    #[test]
    fn a_name_at_a_sentences_start_satisfies_the_same_name_mid_sentence_on_the_other_side() {
        let at_the_start = "Tan booked the ferry for us.";
        let mid_sentence = "For us, Tan booked the ferry.";
        assert_eq!(check(at_the_start, mid_sentence).capitalised_tokens, Pass);
        assert_eq!(check(mid_sentence, at_the_start).capitalised_tokens, Pass);
    }

    #[test]
    fn a_mid_sentence_name_needs_as_many_occurrences_at_any_position_on_the_other_side() {
        // Tan opens the passage's first sentence and sits mid-sentence in its second, so the
        // passage requires one Tan and holds two.
        let passage = "Tan called. Later Tan wrote.";
        assert_eq!(
            check(passage, "Later Tan wrote, and Tan called.").capitalised_tokens,
            Pass
        );
        assert_eq!(
            check(passage, "Later Tan wrote, Tan called and Tan left.").capitalised_tokens,
            Fail
        );
    }

    #[test]
    fn the_pronoun_i_mid_sentence_on_one_side_only_leaves_the_capitalised_tokens_rule_unaffected() {
        let with_i = "Yesterday I booked the ferry.";
        let without_i = "Yesterday the ferry was booked.";
        assert_eq!(check(with_i, without_i).capitalised_tokens, Pass);
        assert_eq!(check(without_i, with_i).capitalised_tokens, Pass);
    }

    #[test]
    fn two_negation_words_against_one_fail_the_negation_rule() {
        assert_eq!(
            check(
                "Nobody came and nothing changed.",
                "Nobody came and everything changed."
            )
            .negations,
            Fail
        );
    }

    #[test]
    fn the_same_negation_words_in_any_order_pass_the_negation_rule() {
        assert_eq!(
            check(
                "Nobody came, and the ferry never left.",
                "The ferry never left, and nobody came."
            )
            .negations,
            Pass
        );
    }

    #[test]
    fn the_nt_of_isnt_counts_once_as_a_token_of_its_own_under_the_entry_of_not() {
        let contracted = tokens("It isn't late.");
        assert_eq!(
            contracted
                .iter()
                .map(|token| token.text)
                .collect::<Vec<_>>(),
            ["It", "is", "n't", "late"]
        );
        assert_eq!(negations(&contracted), HashMap::from([("not", 1)]));
        assert_eq!(
            negations(&tokens("It is not late.")),
            negations(&contracted)
        );
    }

    #[test]
    fn a_contractions_or_a_possessives_ending_is_split_off_and_counts_under_no_rule() {
        let split: Vec<&str> = tokens("Tan's dog. I'm sure they're, we've, he'd and she'll.")
            .iter()
            .map(|token| token.text)
            .collect();
        assert_eq!(
            split,
            [
                "Tan", "'s", "dog", "I", "'m", "sure", "they", "'re", "we", "'ve", "he", "'d",
                "and", "she", "'ll"
            ]
        );
        assert_eq!(
            check("We took Tan's ferry.", "We took the ferry of Tan.").capitalised_tokens,
            Pass
        );
        assert_eq!(
            check(
                "Yes, I'm late for the ferry today.",
                "Yes, I am late for the ferry today."
            ),
            ALL_PASS
        );
        assert_eq!(
            check(
                "Here nothing's late for the ferry today.",
                "Here nothing is late for the ferry today."
            ),
            ALL_PASS
        );
        assert_eq!(
            check(
                "They shouldn\u{2019}t\u{2019}ve left.",
                "They should not have left."
            )
            .negations,
            Pass
        );
    }

    #[test]
    fn a_contraction_expanded_or_contracted_keeps_the_negation_count() {
        assert_eq!(check("I don't know.", "I do not know.").negations, Pass);
        assert_eq!(check("I cannot come.", "I can't come.").negations, Pass);
    }

    #[test]
    fn nobody_and_no_one_are_different_entries() {
        assert_eq!(
            check("Nobody came to the pier.", "No one came to the pier.").negations,
            Fail
        );
    }

    #[test]
    fn a_sentence_initial_never_moved_mid_sentence_keeps_the_negation() {
        // Negation words compare without regard to case, and `Never` capitalised by its position
        // is not required as a capitalised token.
        let result = check(
            "Never leave the gate open.",
            "Please never leave the gate open.",
        );
        assert_eq!(result.negations, Pass);
        assert_eq!(result.capitalised_tokens, Pass);
    }

    #[test]
    fn a_length_inside_the_band_passes_bounds_included_and_outside_it_fails() {
        let passage = text_of_words(10);
        for (words, expected) in [(6, Fail), (7, Pass), (10, Pass), (13, Pass), (14, Fail)] {
            assert_eq!(
                check(&passage, &text_of_words(words)).length,
                expected,
                "{words} words"
            );
        }
    }

    #[test]
    fn an_empty_candidate_fails_the_length_rule() {
        assert_eq!(check("The ferry is late.", "").length, Fail);
        assert_eq!(check("The ferry is late.", " ... ").length, Fail);
    }

    #[test]
    fn an_empty_passage_fails_every_candidates_length_rule() {
        assert_eq!(check("", "").length, Fail);
        assert_eq!(check("", "The ferry is late.").length, Fail);
    }

    #[test]
    fn a_candidate_identical_to_its_passage_passes_all_four_rules() {
        let passage = "On 3 May, Tan said the 1,250 tickets weren't sold, and nobody knew why.";
        let result = check(passage, passage);
        assert_eq!(result, ALL_PASS);
        assert!(result.passed());
    }

    #[test]
    fn curly_and_straight_apostrophes_are_equal() {
        // Unmapped, the curly `O’Brien` would differ from the straight one as a capitalised token,
        // and the curly `wasn’t` would hold no `n't`.
        let curly = "We met O\u{2019}Brien, who wasn\u{2019}t sure.";
        let straight = "We met O'Brien, who wasn't sure.";
        assert_eq!(check(curly, straight), ALL_PASS);
        assert_eq!(check(straight, curly), ALL_PASS);
        assert_eq!(
            check(curly, "We met O'Brien, who was sure.").negations,
            Fail
        );
    }

    #[test]
    fn each_rule_reports_its_own_outcome() {
        let passage = "On 3 May, Tan said nobody had booked the ferry for the island.";
        for (candidate, expected) in [
            (
                "On 4 May, Tan said nobody had booked the ferry for the island.",
                ContentResult {
                    numbers: Fail,
                    ..ALL_PASS
                },
            ),
            (
                "On 3 May, Lim said nobody had booked the ferry for the island.",
                ContentResult {
                    capitalised_tokens: Fail,
                    ..ALL_PASS
                },
            ),
            (
                "On 3 May, Tan said everybody had booked the ferry for the island.",
                ContentResult {
                    negations: Fail,
                    ..ALL_PASS
                },
            ),
            (
                "On 3 May, Tan said nobody had booked.",
                ContentResult {
                    length: Fail,
                    ..ALL_PASS
                },
            ),
        ] {
            let result = check(passage, candidate);
            assert_eq!(result, expected, "{candidate}");
            assert!(!result.passed());
        }
    }

    #[test]
    fn the_same_passage_and_candidate_give_identical_results() {
        // `check` takes the two texts alone, so nothing else can reach its result.
        let passage = "On 3 May, Tan said nobody had booked the ferry.";
        let candidate = "Tan said on 3 May that nobody had booked the ferry.";
        assert_eq!(check(passage, candidate), check(passage, candidate));
    }
}
