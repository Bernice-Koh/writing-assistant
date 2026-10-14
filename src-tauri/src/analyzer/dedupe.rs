//! The suppression rule: a grammar flag suppresses an AI-tell flag on an overlapping span. A
//! spelling flag neither suppresses a flag nor is suppressed.

use std::ops::Range;

use crate::flag::{Flag, FlagOrigin};

/// Applies the suppression rule to `flags`, all computed against the same `sentence`. Every
/// non-AI-tell flag passes through untouched; an AI-tell flag is dropped only when its span
/// overlaps a grammar flag's span, both as offsets into `sentence` (see
/// [`resolve_within_sentence`]).
pub(super) fn apply(sentence: &str, flags: Vec<Flag>) -> Vec<Flag> {
    let resolved: Vec<Option<Range<usize>>> = flags
        .iter()
        .map(|flag| resolve_within_sentence(sentence, flag))
        .collect();

    // Owned, not borrowed from `resolved`: `resolved` itself is moved into the `zip` below, so
    // this needs its own copies rather than references into a value about to be consumed.
    let grammar_ranges: Vec<Range<usize>> = flags
        .iter()
        .zip(&resolved)
        .filter(|(flag, _)| flag.origin == FlagOrigin::Grammar)
        .filter_map(|(_, range)| range.clone())
        .collect();

    flags
        .into_iter()
        .zip(resolved)
        .filter(|(flag, range)| {
            if flag.origin != FlagOrigin::AiTell {
                return true;
            }
            match range {
                // A span this function could not resolve is kept rather than silently dropped:
                // suppression only fires on a confirmed overlap.
                None => true,
                Some(range) => !grammar_ranges
                    .iter()
                    .any(|grammar| overlaps(grammar, range)),
            }
        })
        .map(|(flag, _)| flag)
        .collect()
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Resolves `flag`'s span to a UTF-16 offset range within `sentence`, the single unit every flag
/// passed to [`apply`] was computed against. Every checking source anchors its flags on the text
/// it was handed (see `languagetool::client`, `spelling` and `style::ai_tell`'s own
/// documentation), so `local_start` and `local_length` are already offsets into `sentence`.
/// Returns `None` for a flag anchored on anything else: its offsets belong to another text and
/// cannot be compared with a grammar flag's.
fn resolve_within_sentence(sentence: &str, flag: &Flag) -> Option<Range<usize>> {
    if flag.span.anchor != sentence {
        return None;
    }
    let start = flag.span.local_start;
    Some(start..start + flag.span.local_length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::Span;

    fn flag(origin: FlagOrigin, anchor: &str, local_start: usize, local_length: usize) -> Flag {
        Flag {
            id: format!("{origin:?}:{anchor}:{local_start}"),
            origin,
            span: Span {
                anchor: anchor.to_string(),
                local_start,
                local_length,
            },
            message: "test flag".to_string(),
            suggestions: Vec::new(),
            source_detail: "test".to_string(),
        }
    }

    #[test]
    fn suppresses_an_ai_tell_flag_overlapping_a_grammar_flag() {
        let sentence = "The team boasts a strong record.";
        // "team b" (indices 4..10) genuinely overlaps "boasts a" (indices 9..17) at index 9,
        // not merely adjacent to it: the overlap this test means to exercise.
        let grammar = flag(FlagOrigin::Grammar, sentence, 4, 6);
        let ai_tell = flag(FlagOrigin::AiTell, sentence, 9, 8);
        let result = apply(sentence, vec![grammar.clone(), ai_tell]);
        assert_eq!(result, vec![grammar]);
    }

    #[test]
    fn keeps_an_ai_tell_flag_that_does_not_overlap_a_grammar_flag() {
        let sentence = "The team boasts a strong record today.";
        let grammar = flag(FlagOrigin::Grammar, sentence, 4, 4); // "team"
        let ai_tell = flag(FlagOrigin::AiTell, sentence, 9, 8); // "boasts a"
        let result = apply(sentence, vec![grammar.clone(), ai_tell.clone()]);
        assert_eq!(result, vec![grammar, ai_tell]);
    }

    #[test]
    fn keeps_a_spelling_flag_even_when_it_overlaps_a_grammar_flag() {
        let sentence = "The team recieve praise.";
        let grammar = flag(FlagOrigin::Grammar, sentence, 9, 7); // "recieve"
                                                                 // Sentence-anchored, matching how `spelling::SpellChecker::check` actually builds a span.
        let spelling = flag(FlagOrigin::Spelling, sentence, 9, 7);
        let result = apply(sentence, vec![grammar.clone(), spelling.clone()]);
        assert_eq!(result, vec![grammar, spelling]);
    }

    #[test]
    fn suppresses_only_the_overlapped_one_of_two_identical_regex_matches() {
        // Both tells cover the same text, ", ensuring". Each carries its own offset into the
        // sentence, so a grammar flag on the second leaves the first standing.
        let sentence = "The plan grew, ensuring reach, ensuring scale.";
        let tells = crate::style::ai_tell::scan(sentence);
        assert_eq!(tells.len(), 2, "{tells:#?}");
        let second_ensuring = sentence.rfind("ensuring").unwrap();
        let grammar = flag(FlagOrigin::Grammar, sentence, second_ensuring, 8);

        let mut flags = vec![grammar.clone()];
        flags.extend(tells.clone());
        let result = apply(sentence, flags);
        assert_eq!(result, vec![grammar, tells[0].clone()]);
    }

    #[test]
    fn keeps_an_unresolvable_ai_tell_flag_rather_than_drop_it() {
        let sentence = "The team boasts a strong record.";
        let grammar = flag(FlagOrigin::Grammar, sentence, 4, 5);
        let stray = flag(FlagOrigin::AiTell, "not in this sentence at all", 0, 5);
        let result = apply(sentence, vec![grammar.clone(), stray.clone()]);
        assert_eq!(result, vec![grammar, stray]);
    }
}
