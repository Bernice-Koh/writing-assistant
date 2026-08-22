//! Suppresses AI-tell flags on a span that already carries a hard grammar error, the rule this
//! module's parent already documented before any of it was implemented: style flags are
//! suppressed on spans that already carry a hard grammar error.

use std::ops::Range;

use crate::flag::{Flag, FlagOrigin};

/// Applies the suppression rule to `flags`, all computed against the same `sentence`. Every
/// non-AI-tell flag passes through untouched; an AI-tell flag is dropped only when its span
/// overlaps a grammar flag's span, both resolved into the same coordinate system first, since
/// each origin anchors its span differently (see [`resolve_within_sentence`]).
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
/// passed to [`apply`] was computed against. Grammar flags and phrase-based AI-tell flags already
/// anchor on the sentence itself (see `languagetool::client` and `style::ai_tell`'s own
/// documentation), so their `local_start`/`local_length` apply directly. Spelling flags anchor on
/// the misspelled word, and regex-based AI-tell flags anchor on their own matched substring, so
/// both are resolved by first finding that anchor's own position within `sentence`. Returns
/// `None` if the anchor cannot be found in `sentence` at all, which should not happen for a flag
/// genuinely computed from `sentence`, but is treated as unresolvable rather than assumed away.
fn resolve_within_sentence(sentence: &str, flag: &Flag) -> Option<Range<usize>> {
    let anchor_start = if flag.span.anchor == sentence {
        0
    } else {
        let byte_index = sentence.find(&flag.span.anchor)?;
        sentence[..byte_index].encode_utf16().count()
    };
    let start = anchor_start + flag.span.local_start;
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
        let ai_tell = flag(FlagOrigin::AiTell, "boasts a", 0, 8);
        let result = apply(sentence, vec![grammar.clone(), ai_tell]);
        assert_eq!(result, vec![grammar]);
    }

    #[test]
    fn keeps_an_ai_tell_flag_that_does_not_overlap_a_grammar_flag() {
        let sentence = "The team boasts a strong record today.";
        let grammar = flag(FlagOrigin::Grammar, sentence, 4, 4); // "team"
        let ai_tell = flag(FlagOrigin::AiTell, "boasts a", 0, 8); // elsewhere in the sentence
        let result = apply(sentence, vec![grammar.clone(), ai_tell.clone()]);
        assert_eq!(result, vec![grammar, ai_tell]);
    }

    #[test]
    fn keeps_a_spelling_flag_even_when_it_overlaps_a_grammar_flag() {
        let sentence = "The team recieve praise.";
        let grammar = flag(FlagOrigin::Grammar, sentence, 9, 7); // "recieve"
        let spelling = flag(FlagOrigin::Spelling, "recieve", 0, 7);
        let result = apply(sentence, vec![grammar.clone(), spelling.clone()]);
        assert_eq!(result, vec![grammar, spelling]);
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
