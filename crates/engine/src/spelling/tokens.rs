//! The tokens spelling skips, by the rule stated on the spelling module. A token is a run of
//! non-whitespace characters, wider than the segment [`crate::words`] defines a word by:
//! `src/analyzr` is one token of three segments, and a segment joined to a URL by punctuation alone
//! is skipped with it.

use std::ops::Range;

/// The byte range in `text` of each token shaped like a URL, a path or an identifier.
pub(super) fn skipped_tokens(text: &str) -> Vec<Range<usize>> {
    text.split(char::is_whitespace)
        .filter(|token| !token.is_empty() && is_url_path_or_identifier_shaped(token))
        .map(|token| {
            // `split` returns slices of `text`, so the distance between the two addresses is the
            // token's byte offset in it.
            let start = token.as_ptr().addr() - text.as_ptr().addr();
            start..start + token.len()
        })
        .collect()
}

/// The slash test covers the scheme separator, `://`, which holds two.
fn is_url_path_or_identifier_shaped(token: &str) -> bool {
    token.contains(['/', '@', '_'])
        || has_a_dot_between_letters(token)
        || has_a_capital_directly_after_a_lowercase_letter(token)
}

fn has_a_dot_between_letters(token: &str) -> bool {
    let characters: Vec<char> = token.chars().collect();
    characters
        .windows(3)
        .any(|window| window[0].is_alphabetic() && window[1] == '.' && window[2].is_alphabetic())
}

fn has_a_capital_directly_after_a_lowercase_letter(token: &str) -> bool {
    token
        .chars()
        .zip(token.chars().skip(1))
        .any(|(previous, next)| previous.is_lowercase() && next.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_of_each_named_shape_is_skipped() {
        for token in [
            "https://exampel.com/pth",
            "src/analyzr",
            "naem@exampel.com",
            "overlay.tsx",
            "get_current_flags",
            "FlagOrigin",
            "iPhone",
        ] {
            assert!(is_url_path_or_identifier_shaped(token), "{token}");
        }
    }

    #[test]
    fn a_word_with_a_full_stop_after_its_last_letter_is_not_skipped() {
        for token in ["recieve.", "recieve...", "(recieve)."] {
            assert!(!is_url_path_or_identifier_shaped(token), "{token}");
        }
    }

    #[test]
    fn a_word_of_ordinary_prose_is_not_skipped() {
        // A capital before a lowercase letter, an apostrophe, a hyphen, a dot between digits and
        // curly quotes are each ordinary.
        for token in [
            "Kopitiam",
            "LONDON",
            "HDBs",
            "don\u{2019}t",
            "O\u{2019}Brien",
            "well-known",
            "3.5km",
            "\u{201c}recieve,\u{201d}",
        ] {
            assert!(!is_url_path_or_identifier_shaped(token), "{token}");
        }
    }

    #[test]
    fn a_token_runs_from_whitespace_to_whitespace_punctuation_included() {
        // The no-break and ideographic spaces are whitespace of two and three bytes, so a range
        // counted in characters would fall short of the token.
        let text = "see\u{a0}(src/analyzr,recieve)\u{3000}and overlay.tsx.";
        let skipped: Vec<&str> = skipped_tokens(text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(skipped, ["(src/analyzr,recieve)", "overlay.tsx."]);
    }
}
