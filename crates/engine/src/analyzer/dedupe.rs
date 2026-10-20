//! Suppression by severity, applied to one sentence's flags once every source has checked it. A
//! flag's severity comes from [`FlagOrigin::severity`] and nowhere else. The steps run in this
//! order:
//!
//! 1. Every flag's span is resolved to a range within the sentence. A flag that does not resolve
//!    is kept and takes no part in the steps after: suppression fires only on a confirmed overlap.
//! 2. A soft flag whose range shares at least one code unit with a hard flag's is dropped,
//!    whatever the two origins. Two hard flags both stand, and two ranges that touch end to start
//!    share no code unit.
//! 3. Among the telltales left, every connected group of overlapping ranges, identical, nested or
//!    partial, collapses into one flag. Its range runs from the group's earliest start to its
//!    latest end, its id and message are those of the widest match, and its detail lists every
//!    pattern id in the group, each once, in the order their matches start.
//! 4. A telltale on exactly the range of a style flag is dropped, and its pattern ids are added to
//!    the style flag's detail. A style flag and a telltale on unequal overlapping ranges both
//!    stand.
//!
//! Step 2 runs before step 3 so that a hard flag drops only the soft flags on its own span: a
//! telltale that overlaps a dropped one, and not the hard flag, stands.

use std::cmp::Reverse;
use std::ops::Range;

use crate::flag::{Flag, FlagOrigin, Severity};

/// A flag on its way through suppression.
struct Candidate {
    flag: Flag,
    /// `None` for a flag anchored on text other than the sentence (see
    /// [`resolve_within_sentence`]).
    range: Option<Range<usize>>,
    /// For a telltale, the catalogue patterns it stands for: its own, and once collapsed, every
    /// pattern in its group. Empty for every other origin.
    pattern_ids: Vec<String>,
}

/// One connected group of two or more telltales whose ranges overlap.
struct TelltaleGroup {
    /// Indices into the candidates, in the order their ranges start.
    members: Vec<usize>,
    /// From the earliest start among the members to the latest end.
    range: Range<usize>,
}

/// Applies suppression to `flags`, all computed against the same `sentence`, in the steps the
/// module comment gives. The flags that stand keep the order they arrived in.
pub(super) fn apply(sentence: &str, flags: Vec<Flag>) -> Vec<Flag> {
    let candidates = resolve(sentence, flags);
    let candidates = drop_soft_flags_overlapping_hard_flags(candidates);
    let candidates = collapse_overlapping_telltales(candidates);
    let candidates = fold_same_span_telltales_into_style_flags(candidates);
    candidates
        .into_iter()
        .map(|candidate| candidate.flag)
        .collect()
}

fn resolve(sentence: &str, flags: Vec<Flag>) -> Vec<Candidate> {
    flags
        .into_iter()
        .map(|flag| Candidate {
            range: resolve_within_sentence(sentence, &flag),
            pattern_ids: telltale_pattern_id(&flag)
                .map(str::to_owned)
                .into_iter()
                .collect(),
            flag,
        })
        .collect()
}

fn drop_soft_flags_overlapping_hard_flags(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let hard_ranges: Vec<Range<usize>> = candidates
        .iter()
        .filter(|candidate| candidate.flag.origin.severity() == Severity::Hard)
        .filter_map(|candidate| candidate.range.clone())
        .collect();
    candidates
        .into_iter()
        .filter(|candidate| !is_suppressed_by_hard_flag(candidate, &hard_ranges))
        .collect()
}

fn is_suppressed_by_hard_flag(candidate: &Candidate, hard_ranges: &[Range<usize>]) -> bool {
    candidate.flag.origin.severity() == Severity::Soft
        && candidate.range.as_ref().is_some_and(|range| {
            hard_ranges
                .iter()
                .any(|hard| shares_a_code_unit(hard, range))
        })
}

fn collapse_overlapping_telltales(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut collapsed_away = vec![false; candidates.len()];
    for group in overlapping_telltale_groups(&candidates) {
        // Of two equally wide matches, the one that arrived first.
        let widest = group
            .members
            .iter()
            .copied()
            .min_by_key(|&member| (Reverse(candidates[member].flag.span.local_length), member))
            .expect("a group holds at least two telltales");
        let mut pattern_ids: Vec<String> = Vec::new();
        for &member in &group.members {
            for id in &candidates[member].pattern_ids {
                if !pattern_ids.contains(id) {
                    pattern_ids.push(id.clone());
                }
            }
            collapsed_away[member] = member != widest;
        }
        // The widest match keeps its own id, which no other flag in the sentence holds once the
        // rest of its group is dropped, and which names a pattern and offsets, never draft text.
        let collapsed = &mut candidates[widest];
        collapsed.flag.span.local_start = group.range.start;
        collapsed.flag.span.local_length = group.range.len();
        collapsed.flag.source_detail = pattern_ids.join(", ");
        collapsed.pattern_ids = pattern_ids;
        collapsed.range = Some(group.range);
    }
    without(candidates, collapsed_away)
}

/// Every connected group of two or more telltales whose ranges overlap.
fn overlapping_telltale_groups(candidates: &[Candidate]) -> Vec<TelltaleGroup> {
    let mut telltales: Vec<(usize, Range<usize>)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.flag.origin == FlagOrigin::AiTell)
        .filter_map(|(index, candidate)| Some((index, candidate.range.clone()?)))
        // An empty range shares no code unit with any other, so it belongs to no group.
        .filter(|(_, range)| !range.is_empty())
        .collect();
    // A stable sort, so telltales that start together keep the order they arrived in.
    telltales.sort_by_key(|(_, range)| range.start);

    let mut groups: Vec<TelltaleGroup> = Vec::new();
    for (index, range) in telltales {
        match groups.last_mut() {
            // The group's ranges cover its span without a gap, so a range that starts before the
            // span ends shares a code unit with at least one of them.
            Some(group) if range.start < group.range.end => {
                group.members.push(index);
                group.range.end = group.range.end.max(range.end);
            }
            _ => groups.push(TelltaleGroup {
                members: vec![index],
                range,
            }),
        }
    }
    groups.retain(|group| group.members.len() > 1);
    groups
}

fn fold_same_span_telltales_into_style_flags(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let telltales: Vec<(usize, Range<usize>, String)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.flag.origin == FlagOrigin::AiTell)
        .filter_map(|(index, candidate)| {
            Some((
                index,
                candidate.range.clone()?,
                candidate.pattern_ids.join(", "),
            ))
        })
        .collect();
    let mut folded_away = vec![false; candidates.len()];
    for (telltale, range, pattern_ids) in telltales {
        let same_span_style_flags = candidates.iter_mut().filter(|candidate| {
            matches!(candidate.flag.origin, FlagOrigin::Style(_))
                && candidate.range.as_ref() == Some(&range)
        });
        for style in same_span_style_flags {
            style.flag.source_detail = format!("{}; {pattern_ids}", style.flag.source_detail);
            folded_away[telltale] = true;
        }
    }
    without(candidates, folded_away)
}

/// `candidates` less every one whose entry in `dropped` is `true`, the rest in their order.
fn without(candidates: Vec<Candidate>, dropped: Vec<bool>) -> Vec<Candidate> {
    candidates
        .into_iter()
        .zip(dropped)
        .filter_map(|(candidate, dropped)| (!dropped).then_some(candidate))
        .collect()
}

fn shares_a_code_unit(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start.max(b.start) < a.end.min(b.end)
}

/// The catalogue pattern a telltale flag matched, read from the id `style::ai_tell::scan` gives
/// every telltale, `ai-tell:<pattern id>:<matcher index>:<offset>`. A [`Flag`] holds the pattern
/// id nowhere else. `None` for every other origin.
fn telltale_pattern_id(flag: &Flag) -> Option<&str> {
    if flag.origin != FlagOrigin::AiTell {
        return None;
    }
    flag.id.strip_prefix("ai-tell:")?.split(':').next()
}

/// Resolves `flag`'s span to a UTF-16 offset range within `sentence`, the single unit every flag
/// passed to [`apply`] was computed against. Every checking source anchors its flags on the text
/// it was handed (see `languagetool::client`, `spelling` and `style::ai_tell`'s own
/// documentation), so `local_start` and `local_length` are already offsets into `sentence`.
/// Returns `None` for a flag anchored on anything else: its offsets belong to another text and
/// cannot be compared with another flag's.
fn resolve_within_sentence(sentence: &str, flag: &Flag) -> Option<Range<usize>> {
    if flag.span.anchor != sentence {
        return None;
    }
    let start = flag.span.local_start;
    Some(start..start + flag.span.local_length)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::flag::{ClauseCitation, ClauseId, RuleId, Span};
    use crate::style::ai_tell;

    fn flag(origin: FlagOrigin, anchor: &str, local_start: usize, local_length: usize) -> Flag {
        Flag {
            id: format!("{origin:?}:{anchor}:{local_start}"),
            origin,
            span: Span {
                anchor: anchor.to_string(),
                local_start,
                local_length,
                occurrence: 0,
            },
            message: "test flag".to_string(),
            suggestions: Vec::new(),
            source_detail: "test".to_string(),
            ruleset_version: None,
            catalogue_hash: None,
        }
    }

    /// A telltale with the id `ai_tell::scan` would give a match of `pattern_id`, and a message
    /// that names the pattern so a test can tell whose message a collapsed flag kept.
    fn telltale(pattern_id: &str, anchor: &str, local_start: usize, local_length: usize) -> Flag {
        Flag {
            id: format!("ai-tell:{pattern_id}:0:{local_start}"),
            message: format!("{pattern_id} message"),
            ..flag(FlagOrigin::AiTell, anchor, local_start, local_length)
        }
    }

    fn style(anchor: &str, local_start: usize, local_length: usize) -> Flag {
        let origin = FlagOrigin::Style(ClauseCitation {
            clause_id: ClauseId(7),
            rule_id: RuleId("HEDGING_PER_PARAGRAPH".to_owned()),
            clause_text: "Hedge at most once a paragraph.".to_owned(),
        });
        Flag {
            id: format!("style:{local_start}"),
            source_detail: "HEDGING_PER_PARAGRAPH".to_owned(),
            ..flag(origin, anchor, local_start, local_length)
        }
    }

    /// The exact text a flag's span addresses, sliced back out of its own anchor.
    fn flagged_text(flag: &Flag) -> String {
        let anchor: Vec<u16> = flag.span.anchor.encode_utf16().collect();
        let end = flag.span.local_start + flag.span.local_length;
        String::from_utf16_lossy(&anchor[flag.span.local_start..end])
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
        // Both on "recieve", sentence-anchored as `spelling::SpellChecker::check` builds a span.
        let grammar = flag(FlagOrigin::Grammar, sentence, 9, 7);
        let spelling = flag(FlagOrigin::Spelling, sentence, 9, 7);
        let result = apply(sentence, vec![grammar.clone(), spelling.clone()]);
        assert_eq!(result, vec![grammar, spelling]);
    }

    #[test]
    fn suppresses_only_the_overlapped_one_of_two_identical_regex_matches() {
        // Both tells cover the same text, ", ensuring". Each carries its own offset into the
        // sentence, so a grammar flag on the second leaves the first standing.
        let sentence = "The plan grew, ensuring reach, ensuring scale.";
        let tells = ai_tell::scan(sentence);
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

    #[test]
    fn a_hard_flag_suppresses_an_overlapping_soft_flag_whatever_the_two_origins() {
        let sentence = "The team boasts a strong record.";
        for hard_origin in [FlagOrigin::Spelling, FlagOrigin::Grammar] {
            for soft in [
                style(sentence, 9, 8),
                telltale("copula-avoidance", sentence, 9, 8),
            ] {
                // "team b" (4..10) shares index 9 with "boasts a" (9..17).
                let hard = flag(hard_origin.clone(), sentence, 4, 6);
                let result = apply(sentence, vec![hard.clone(), soft.clone()]);
                assert_eq!(result, vec![hard], "{hard_origin:?} over {:?}", soft.origin);
            }
        }
    }

    #[test]
    fn a_hard_flag_and_soft_flags_that_touch_it_end_to_start_all_stand() {
        let sentence = "The team boasts a strong record.";
        let hard = flag(FlagOrigin::Spelling, sentence, 4, 5); // "team ", 4..9
        let before = telltale("pattern-a", sentence, 0, 4); // "The ", ending where it starts
        let after = style(sentence, 9, 8); // "boasts a", starting where it ends
        let result = apply(sentence, vec![hard.clone(), before.clone(), after.clone()]);
        assert_eq!(result, vec![hard, before, after]);
    }

    #[test]
    fn two_overlapping_hard_flags_both_stand() {
        let sentence = "The team recieve praise.";
        let first = flag(FlagOrigin::Grammar, sentence, 4, 12); // "team recieve"
        let second = flag(FlagOrigin::Grammar, sentence, 9, 14); // "recieve praise"
        let result = apply(sentence, vec![first.clone(), second.clone()]);
        assert_eq!(result, vec![first, second]);
    }

    #[test]
    fn a_style_flag_on_a_telltales_exact_span_stands_alone_and_names_its_pattern() {
        let sentence = "The team boasts a strong record.";
        let on_boasts = style(sentence, 9, 8);
        let tell = telltale("copula-avoidance", sentence, 9, 8);
        let result = apply(sentence, vec![tell, on_boasts.clone()]);
        let expected = Flag {
            source_detail: "HEDGING_PER_PARAGRAPH; copula-avoidance".to_owned(),
            ..on_boasts
        };
        assert_eq!(result, vec![expected]);

        // The collapse runs first, so a style flag on a collapsed telltale's span names every
        // pattern in the group.
        let sentence = "This is a testament to the team.";
        let on_testament = style(sentence, 5, 17); // "is a testament to"
        let mut flags = ai_tell::scan(sentence);
        assert_eq!(flags.len(), 2, "{flags:#?}");
        flags.push(on_testament.clone());
        let result = apply(sentence, flags);
        let expected = Flag {
            source_detail:
                "HEDGING_PER_PARAGRAPH; significance-legacy-emphasis, ai-vocabulary-words"
                    .to_owned(),
            ..on_testament
        };
        assert_eq!(result, vec![expected]);
    }

    #[test]
    fn a_style_flag_and_a_telltale_on_unequal_overlapping_spans_both_stand() {
        let sentence = "The team boasts a strong record.";
        let on_team_boasts = style(sentence, 4, 13); // "team boasts a"
        let tell = telltale("copula-avoidance", sentence, 9, 8); // "boasts a"
        let result = apply(sentence, vec![on_team_boasts.clone(), tell.clone()]);
        assert_eq!(result, vec![on_team_boasts, tell]);
    }

    #[test]
    fn identical_telltales_collapse_into_one_flag_listing_both_patterns() {
        let sentence = "The team boasts a strong record.";
        let first = telltale("copula-avoidance", sentence, 9, 8);
        let second = telltale("promotional-language", sentence, 9, 8);
        let result = apply(sentence, vec![first.clone(), second]);
        let expected = Flag {
            source_detail: "copula-avoidance, promotional-language".to_owned(),
            ..first
        };
        assert_eq!(result, vec![expected]);
    }

    #[test]
    fn nested_telltales_collapse_onto_the_widest_span_with_its_message() {
        let sentence = "This is a testament to the team.";
        let tells = ai_tell::scan(sentence);
        let mut matched: Vec<String> = tells.iter().map(flagged_text).collect();
        matched.sort();
        assert_eq!(matched, ["is a testament to", "testament to"], "{tells:#?}");
        let widest = tells
            .iter()
            .find(|tell| flagged_text(tell) == "is a testament to")
            .unwrap();

        let result = apply(sentence, tells.clone());
        assert_eq!(result.len(), 1, "{result:#?}");
        assert_eq!(flagged_text(&result[0]), "is a testament to");
        assert_eq!(result[0].message, widest.message);
        assert_eq!(
            result[0].source_detail,
            "significance-legacy-emphasis, ai-vocabulary-words"
        );
    }

    #[test]
    fn partially_overlapping_telltales_collapse_from_the_earliest_start_to_the_latest_end() {
        let sentence = "The team boasts a strong record of growth.";
        // Each overlaps the next, and the first does not overlap the third: one connected group.
        let first = telltale("pattern-a", sentence, 4, 11); // "team boasts"
        let second = telltale("pattern-b", sentence, 9, 15); // "boasts a strong"
        let third = telltale("pattern-c", sentence, 18, 13); // "strong record"
        let result = apply(sentence, vec![first, second.clone(), third]);
        let expected = Flag {
            span: Span {
                anchor: sentence.to_owned(),
                local_start: 4,
                local_length: 27,
                occurrence: 0,
            },
            source_detail: "pattern-a, pattern-b, pattern-c".to_owned(),
            ..second
        };
        assert_eq!(result, vec![expected]);
        assert_eq!(flagged_text(&result[0]), "team boasts a strong record");
        assert_eq!(result[0].message, "pattern-b message");
    }

    #[test]
    fn a_hard_flag_drops_only_the_telltale_on_its_own_span_before_telltales_collapse() {
        let sentence = "The team boasts a strong record.";
        // "team b" overlaps "boasts a", which overlaps "a strong".
        let hard = flag(FlagOrigin::Spelling, sentence, 4, 6); // "team b"
        let first = telltale("pattern-a", sentence, 9, 8); // "boasts a"
        let second = telltale("pattern-b", sentence, 16, 8); // "a strong"
        let result = apply(sentence, vec![hard.clone(), first, second.clone()]);
        assert_eq!(result, vec![hard, second]);
    }

    #[test]
    fn a_collapsed_flag_has_an_id_unique_in_its_sentence_and_free_of_draft_text() {
        // Two groups: "is a testament to" holding "testament to", and "stands as a testament"
        // holding "stands as" and partly overlapping a second "testament to".
        let sentence = "Our work is a testament to the team, and it stands as a testament to grit.";
        let tells = ai_tell::scan(sentence);
        assert_eq!(tells.len(), 5, "{tells:#?}");

        let result = apply(sentence, tells);
        let mut matched: Vec<String> = result.iter().map(flagged_text).collect();
        matched.sort();
        assert_eq!(
            matched,
            ["is a testament to", "stands as a testament to"],
            "{result:#?}"
        );
        let ids: HashSet<&str> = result.iter().map(|flag| flag.id.as_str()).collect();
        assert_eq!(ids.len(), result.len(), "{result:#?}");
        let words: Vec<String> = sentence
            .split(|character: char| !character.is_alphabetic())
            .filter(|word| word.len() > 3)
            .map(str::to_lowercase)
            .collect();
        for collapsed in &result {
            for word in &words {
                assert!(!collapsed.id.contains(word.as_str()), "{collapsed:#?}");
            }
        }
    }

    #[test]
    fn a_flag_anchored_on_another_text_neither_suppresses_nor_collapses_with_one() {
        let sentence = "The team boasts a strong record.";
        let elsewhere = "The team boasts a strong record, the report said.";
        let stray_hard = flag(FlagOrigin::Grammar, elsewhere, 9, 8);
        let stray_telltale = telltale("pattern-a", elsewhere, 9, 8);
        let tell = telltale("pattern-b", sentence, 9, 8);
        let result = apply(
            sentence,
            vec![stray_hard.clone(), stray_telltale.clone(), tell.clone()],
        );
        assert_eq!(result, vec![stray_hard, stray_telltale, tell]);
    }
}
