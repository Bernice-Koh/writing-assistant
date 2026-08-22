//! Sentence-level diffing: which sentences actually changed since the last check, so a recheck
//! covers only those instead of the whole document.

use std::collections::HashSet;

use similar::{capture_diff_slices, Algorithm, DiffOp};
use unicode_segmentation::UnicodeSegmentation;

/// Splits `text` into its sentences, owned rather than borrowed: the analyzer holds the
/// last-seen sentence list across polls, past the lifetime of any one `current_text()` call's
/// return value.
///
/// Trimmed: `unicode_sentences` includes trailing whitespace up to the start of the next
/// sentence, which is not present after whatever is currently the *last* sentence in `text`.
/// Appending a new sentence therefore adds trailing whitespace to what was previously the last
/// sentence, changing its exact text with nothing the user actually edited in it; diffing
/// untrimmed sentences would treat that former-last sentence as changed on every new sentence
/// appended, defeating "only the changed sentence is rechecked" for the single most common way a
/// document grows.
pub(super) fn split(text: &str) -> Vec<String> {
    text.unicode_sentences()
        .map(|sentence| sentence.trim().to_string())
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

/// Indices into `current` of every sentence that is new or changed relative to `previous`, found
/// by diffing the two sentence lists at sentence granularity with Myers' algorithm. An index left
/// out of the result is a sentence `capture_diff_slices` reports as `Equal`, so its caller can
/// serve it from cache instead of rechecking it.
pub(super) fn changed_indices(previous: &[String], current: &[String]) -> HashSet<usize> {
    let mut changed = HashSet::new();
    for op in capture_diff_slices(Algorithm::Myers, previous, current) {
        match op {
            DiffOp::Equal { .. } => {}
            // A pure deletion touches no index in `current`; nothing to mark changed for it.
            DiffOp::Delete { .. } => {}
            DiffOp::Insert {
                new_index, new_len, ..
            }
            | DiffOp::Replace {
                new_index, new_len, ..
            } => changed.extend(new_index..new_index + new_len),
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_paragraph_into_its_sentences() {
        let sentences = split("First sentence. Second one! Is this the third?");
        assert_eq!(
            sentences,
            vec!["First sentence.", "Second one!", "Is this the third?"]
        );
    }

    #[test]
    fn a_sentence_appended_to_the_end_is_the_only_change() {
        let previous = split("First sentence. Second sentence.");
        let current = split("First sentence. Second sentence. Third sentence.");
        let changed = changed_indices(&previous, &current);
        assert_eq!(changed, HashSet::from([2]));
    }

    #[test]
    fn editing_one_sentence_in_the_middle_leaves_the_others_untouched() {
        let previous = split("First sentence. Second sentence. Third sentence.");
        let current = split("First sentence. Edited sentence. Third sentence.");
        let changed = changed_indices(&previous, &current);
        assert_eq!(changed, HashSet::from([1]));
    }

    #[test]
    fn identical_text_reports_no_changed_sentences() {
        let previous = split("Nothing changed here.");
        let current = split("Nothing changed here.");
        assert!(changed_indices(&previous, &current).is_empty());
    }

    #[test]
    fn deleting_a_sentence_reports_no_changed_index_in_the_shorter_current_list() {
        let previous = split("Keep this. Delete this one. Keep this too.");
        let current = split("Keep this. Keep this too.");
        assert!(changed_indices(&previous, &current).is_empty());
    }
}
