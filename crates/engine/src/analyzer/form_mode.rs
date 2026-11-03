//! The split of the document's text into paragraphs, and each paragraph's form mode, classified
//! from its structure. The analyzer checks each sentence under its paragraph's mode. The module is
//! public for one other caller, the native capture harness, which prints each paragraph's mode
//! against a live document with the analyzer's own split and classification.
//!
//! Classification is one pass over the document's lines in order, carrying whether a fence is open
//! and whether the line before was indented, with a look at the line after. A line that opens a
//! fence is code, and so is every paragraph after it up to and including the line that closes it,
//! or up to the document's end when none does. Outside a fence, the first of these rules that holds
//! decides:
//!
//! 1. A paragraph indented by a tab or four spaces is code when it also reads as code: when it is
//!    symbol-dense by rule 3's test, which counts only non-whitespace characters and so not the
//!    indentation, or when it is part of a run of indented lines. A run is two or more consecutive
//!    lines of the document that are each indented. It is read over the document's lines before
//!    the empty and whitespace paragraphs are discarded, so a blank line ends a run, as an
//!    unindented line does. An indented line of ordinary sentences on its own is a paragraph with a
//!    first-line indent, typed with a tab, and the indent alone does not make it code: an indented
//!    line that is neither dense nor in a run falls to the rules below as an unindented line would.
//! 2. A bulleted or numbered list item, or a heading, is a fragment. Its marker declares the
//!    structure, and is itself a symbol, which on a short item or heading would carry the next
//!    rule's density over its threshold: one heading in five of this repository's documentation
//!    would read as code.
//! 3. A symbol-dense paragraph is code: one whose symbols, the non-whitespace characters that are
//!    neither letters nor digits, are more than `SYMBOL_DENSITY_THRESHOLD` of its non-whitespace
//!    characters.
//! 4. A paragraph shorter than `FRAGMENT_LENGTH_THRESHOLD` characters that ends without terminal
//!    punctuation is a fragment.
//! 5. Everything else is prose, a paragraph of complete sentences among it.

use crate::style::checker::FormMode;

/// The share of a paragraph's non-whitespace characters that are symbols above which the
/// paragraph is code. Prose carries a few punctuation marks a sentence, and a line of code puts a
/// bracket, an operator or a separator between most of its identifiers.
///
/// Measured on this repository at the commit before this module, with symbols counted as here.
/// Of the 216 paragraphs and list items of the README and `docs/`, six are above 0.22: four
/// thematic breaks, and two lists of inline code at 0.35 and 0.37, one of them a list item, which
/// rule 2 makes a fragment. The densest of the rest is just under 0.22, so any threshold from 0.22
/// to 0.34 classes the same prose paragraphs as code, and one quarter keeps a margin on each side.
/// Of the 19,418 non-blank lines of Rust and TypeScript that are not comments, 46% are above it,
/// the median being 0.25. Fenced and indented blocks carry the detection of code: this rule
/// catches a bare line of code, and a lower threshold would catch more of it only by sitting at the
/// edge of the prose sample.
const SYMBOL_DENSITY_THRESHOLD: f64 = 0.25;

/// The length in characters below which a paragraph that ends without terminal punctuation is a
/// fragment: a heading or a label of a few words. A prose paragraph being typed has no terminal
/// punctuation until its sentence ends, and this length is what keeps it prose once it passes its
/// first few words.
///
/// Measured on this repository's README and `docs/`: every one of their 78 headings ends without
/// terminal punctuation, and 2 of them are 40 characters or longer, at 50 and 56, the 95th
/// percentile being 31. Of their 584 sentences, 66 (11.3%) are shorter than 40, the 10th
/// percentile being 37. Such a sentence is a fragment only while it is typed, until its full stop,
/// and its grammar check waits until then.
const FRAGMENT_LENGTH_THRESHOLD: usize = 40;

/// The bullet characters that open a list item, beside the hyphen, the asterisk and the plus sign
/// a plain-text list is typed with.
const BULLETS: [char; 6] = [
    '\u{2022}', // bullet
    '\u{2023}', // triangular bullet
    '\u{2043}', // hyphen bullet
    '\u{25AA}', // black small square
    '\u{25CF}', // black circle
    '\u{25E6}', // white bullet
];

/// The marks that end a sentence. An ellipsis is a run of full stops, which Word's autocorrect
/// writes as one character.
const TERMINAL_PUNCTUATION: [char; 4] = ['.', '?', '!', '\u{2026}'];

/// The closing quotation marks and brackets that may follow a sentence's terminal punctuation.
const CLOSERS: [char; 8] = ['"', '\'', '\u{2019}', '\u{201D}', '\u{00BB}', ')', ']', '}'];

/// One paragraph of the document, with its form mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paragraph<'a> {
    /// The byte offset in the document's text where the paragraph starts.
    pub start: usize,
    /// The paragraph's text, without its paragraph mark.
    pub text: &'a str,
    pub mode: FormMode,
}

/// Splits `text` into its paragraphs and classifies each, in document order.
///
/// A paragraph ends at a mark capture delivers: a carriage return from Word, a newline from a
/// browser, or a carriage return followed by a newline as one mark, so that an empty line between
/// two marks is a blank line of the document. A paragraph that is empty or whitespace is discarded
/// once the runs of indented lines around it are read, since it ends them.
pub fn split(text: &str) -> Vec<Paragraph<'_>> {
    let mut open_fence = None;
    let mut previous_line_is_indented = false;
    let mut paragraphs = Vec::new();
    let mut lines = text
        .split("\r\n")
        .flat_map(|line| line.split(['\r', '\n']))
        .peekable();
    while let Some(line) = lines.next() {
        let line_is_indented = is_indented_and_not_blank(line);
        let next_line_is_indented = lines
            .peek()
            .is_some_and(|next| is_indented_and_not_blank(next));
        let in_indented_run =
            line_is_indented && (previous_line_is_indented || next_line_is_indented);
        previous_line_is_indented = line_is_indented;
        if line.trim().is_empty() {
            continue;
        }
        paragraphs.push(Paragraph {
            // Both splits return slices of `text`, so the distance between the two addresses is the
            // paragraph's byte offset in it.
            start: line.as_ptr().addr() - text.as_ptr().addr(),
            text: line,
            mode: classify(line, in_indented_run, &mut open_fence),
        });
    }
    paragraphs
}

/// Whether `paragraph` is a bulleted or numbered list item: after optional indentation, a bullet,
/// a hyphen, an asterisk or a plus sign, or a number or a letter followed by a full stop or a
/// closing parenthesis, and then a space or a tab. Public for the native capture harness, which
/// prints how an application delivers a list item's marker.
pub fn is_list_item(paragraph: &str) -> bool {
    let rest = paragraph.trim_start();
    let after_marker = match rest.chars().next() {
        Some(marker) if marker == '-' || marker == '*' || marker == '+' => &rest[1..],
        Some(marker) if BULLETS.contains(&marker) => &rest[marker.len_utf8()..],
        Some(marker) if marker.is_ascii_digit() => {
            let digits = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            match digits.strip_prefix(['.', ')']) {
                Some(after) => after,
                None => return false,
            }
        }
        Some(marker) if marker.is_ascii_alphabetic() => match rest[1..].strip_prefix(['.', ')']) {
            Some(after) => after,
            None => return false,
        },
        _ => return false,
    };
    after_marker.starts_with([' ', '\t'])
}

/// The opening run of an open fence, which a closing fence must match.
#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    /// The fence `line` opens: after optional indentation, a run of three or more backticks or
    /// tildes.
    fn opened_by(line: &str) -> Option<Self> {
        let rest = line.trim_start();
        let marker = rest.chars().next().filter(|&c| c == '`' || c == '~')?;
        let length = rest.chars().take_while(|&c| c == marker).count();
        (length >= 3).then_some(Self { marker, length })
    }

    /// Whether `line` closes this fence: a run of its marker at least as long as its opening, and
    /// nothing else, as Markdown has it, so a line that opens a block of another language inside
    /// the fence does not close it.
    fn closed_by(self, line: &str) -> bool {
        let run = line.trim();
        run.chars().all(|c| c == self.marker) && run.chars().count() >= self.length
    }
}

/// The form mode of `line`, given whether it is part of a run of indented lines and the fence open
/// before it, which it updates.
fn classify(line: &str, in_indented_run: bool, open_fence: &mut Option<Fence>) -> FormMode {
    if let Some(fence) = *open_fence {
        if fence.closed_by(line) {
            *open_fence = None;
        }
        return FormMode::Code;
    }
    if let Some(fence) = Fence::opened_by(line) {
        *open_fence = Some(fence);
        return FormMode::Code;
    }
    if is_indented(line) && (in_indented_run || is_symbol_dense(line)) {
        FormMode::Code
    } else if is_list_item(line) || is_heading(line) {
        FormMode::Fragment
    } else if is_symbol_dense(line) {
        FormMode::Code
    } else if is_short_and_unterminated(line) {
        FormMode::Fragment
    } else {
        FormMode::Prose
    }
}

/// Whether `line`'s leading whitespace reaches the fourth column: four spaces, a tab, or spaces
/// before a tab, a tab stopping at the next multiple of four as Markdown counts it.
fn is_indented(line: &str) -> bool {
    let mut column = 0;
    for c in line.chars() {
        match c {
            ' ' => column += 1,
            '\t' => column += 4 - column % 4,
            _ => break,
        }
        if column >= 4 {
            return true;
        }
    }
    false
}

/// Whether `line` counts towards a run of indented lines: indented, with text after the
/// indentation, since a blank line ends a run.
fn is_indented_and_not_blank(line: &str) -> bool {
    is_indented(line) && !line.trim().is_empty()
}

/// Whether `line` is a heading: after optional indentation, a run of hashes and a space or a tab.
fn is_heading(line: &str) -> bool {
    let rest = line.trim_start();
    rest.starts_with('#') && rest.trim_start_matches('#').starts_with([' ', '\t'])
}

fn is_symbol_dense(line: &str) -> bool {
    let (symbols, characters) = line
        .chars()
        .filter(|c| !c.is_whitespace())
        .fold((0_u32, 0_u32), |(symbols, characters), c| {
            (symbols + u32::from(!c.is_alphanumeric()), characters + 1)
        });
    f64::from(symbols) > SYMBOL_DENSITY_THRESHOLD * f64::from(characters)
}

fn is_short_and_unterminated(line: &str) -> bool {
    let text = line.trim();
    text.chars().count() < FRAGMENT_LENGTH_THRESHOLD
        && !text
            .trim_end_matches(CLOSERS)
            .ends_with(TERMINAL_PUNCTUATION)
}

#[cfg(test)]
mod tests {
    use super::*;

    use FormMode::{Code, Fragment, Prose};

    fn modes(text: &str) -> Vec<FormMode> {
        split(text)
            .into_iter()
            .map(|paragraph| paragraph.mode)
            .collect()
    }

    fn mode_of(line: &str) -> FormMode {
        let paragraphs = split(line);
        assert_eq!(paragraphs.len(), 1, "{line:?}");
        paragraphs[0].mode
    }

    #[test]
    fn splits_on_each_mark_and_discards_an_empty_or_whitespace_paragraph() {
        let text = "Word ends here.\rA browser line.\nA CRLF line.\r\n\r\n  \t\nLast one.";
        let paragraphs = split(text);
        let texts: Vec<&str> = paragraphs.iter().map(|paragraph| paragraph.text).collect();
        assert_eq!(
            texts,
            [
                "Word ends here.",
                "A browser line.",
                "A CRLF line.",
                "Last one."
            ]
        );
        for paragraph in &paragraphs {
            assert_eq!(
                &text[paragraph.start..paragraph.start + paragraph.text.len()],
                paragraph.text
            );
        }
        let starts: Vec<usize> = paragraphs.iter().map(|paragraph| paragraph.start).collect();
        assert_eq!(starts, [0, 16, 32, 52]);
    }

    #[test]
    fn a_document_with_no_paragraph_text_has_no_paragraphs() {
        assert!(split("").is_empty());
        assert!(split("\r\n\r\n\n \r").is_empty());
    }

    #[test]
    fn a_paragraph_of_complete_sentences_is_prose() {
        assert_eq!(
            mode_of("The first sentence ends. So does the second one, with a comma."),
            Prose
        );
        assert_eq!(mode_of("Short and done."), Prose);
    }

    #[test]
    fn terminal_punctuation_is_a_full_stop_question_or_exclamation_mark_or_ellipsis() {
        for line in [
            "Is it done?",
            "It is done!",
            "Wait for it\u{2026}",
            "She said \"yes.\"",
            "She said \u{201C}yes.\u{201D}",
            "(See the notes.)",
        ] {
            assert_eq!(mode_of(line), Prose, "{line:?}");
        }
    }

    #[test]
    fn a_paragraph_that_meets_no_fragment_or_code_rule_is_prose() {
        // Long enough to pass the length threshold while its sentence is still being typed.
        assert_eq!(
            mode_of("The owner is still typing this sentence and has not finished it"),
            Prose
        );
    }

    #[test]
    fn the_length_threshold_falls_between_39_and_40_characters() {
        let shorter = "Results of the first round of the trial";
        let at_threshold = "Results of the second round of the trial";
        assert_eq!(shorter.chars().count(), FRAGMENT_LENGTH_THRESHOLD - 1);
        assert_eq!(at_threshold.chars().count(), FRAGMENT_LENGTH_THRESHOLD);
        assert_eq!(mode_of(shorter), Fragment);
        assert_eq!(mode_of(at_threshold), Prose);
        // Terminal punctuation makes the shorter line a sentence.
        assert_eq!(mode_of("Results of the first round of the trial."), Prose);
    }

    #[test]
    fn a_short_line_that_ends_without_terminal_punctuation_is_a_fragment() {
        for line in [
            "Meeting notes",
            "Next steps:",
            "Introduction",
            "Best regards,",
        ] {
            assert_eq!(mode_of(line), Fragment, "{line:?}");
        }
    }

    #[test]
    fn bulleted_and_numbered_list_items_are_fragments() {
        for line in [
            "- First item",
            "* Second item, a whole sentence that ends with a full stop.",
            "+ Third",
            "\u{2022}\tA bullet and a tab, as a word processor writes one",
            "\u{25E6} A white bullet",
            "1. Step one is long enough to pass the length threshold and ends.",
            "12) Step twelve",
            "a. Option a",
            "B) Option B",
            "  - A nested item",
            "- `a::b()` and `c::d()`",
        ] {
            assert_eq!(mode_of(line), Fragment, "{line:?}");
            assert!(is_list_item(line), "{line:?}");
        }
    }

    #[test]
    fn a_marker_with_no_space_after_it_does_not_open_a_list_item() {
        for line in [
            "-not a list item, since no space follows the hyphen, and it ends.",
            "1.5 litres of water went into the pot before anything else.",
            "Ab. is not a single letter before the full stop, and this ends.",
        ] {
            assert!(!is_list_item(line), "{line:?}");
            assert_eq!(mode_of(line), Prose, "{line:?}");
        }
    }

    #[test]
    fn a_heading_is_a_fragment_whatever_its_length() {
        for line in [
            "# Title",
            "### Section three",
            "## A heading long enough to pass the length threshold, and more.",
        ] {
            assert_eq!(mode_of(line), Fragment, "{line:?}");
        }
        assert_eq!(
            mode_of("#hashtag is not a heading, and this sentence is long enough."),
            Prose
        );
    }

    #[test]
    fn a_fenced_block_is_code_from_its_opening_fence_to_its_closing_fence() {
        let text = "Before the block.\n```rust\nlet x = 1;\nThis line reads as prose.\n```\n\
                    After the block.";
        assert_eq!(modes(text), [Prose, Code, Code, Code, Code, Prose]);
    }

    #[test]
    fn a_fence_closes_only_on_a_bare_run_of_its_own_marker_at_least_as_long() {
        let text = "~~~~\nA tilde fence holds this.\n```\nStill inside.\n~~~\n~~~~~\nOutside.";
        assert_eq!(modes(text), [Code, Code, Code, Code, Code, Code, Prose]);
        let text = "```\nInside.\n```python\nStill inside.\n```\nOutside.";
        assert_eq!(modes(text), [Code, Code, Code, Code, Code, Prose]);
    }

    #[test]
    fn a_fence_that_never_closes_runs_to_the_end_of_the_document() {
        let text = "Before the block.\n  ```\nA sentence of prose inside it.\n- An item inside it";
        assert_eq!(modes(text), [Prose, Code, Code, Code]);
    }

    #[test]
    fn an_indented_block_is_code() {
        let text = "Before the block.\n\
                    \x20   let total = count(items);\n\
                    \tA sentence indented by a tab.\n\
                    \x20   - An item indented by four spaces\n\
                    After the block.";
        assert_eq!(modes(text), [Prose, Code, Code, Code, Prose]);
    }

    #[test]
    fn two_spaces_then_a_tab_reach_the_fourth_column_and_three_spaces_do_not() {
        let text = "\tThe first line holds ordinary words.\n\
                    \x20 \tTwo spaces then a tab make this the second line of a run.";
        assert_eq!(modes(text), [Code, Code]);
        let text = "\tThe first line holds ordinary words.\n\
                    \x20  Three spaces do not reach the fourth column, so there is no run.";
        assert_eq!(modes(text), [Prose, Prose]);
    }

    #[test]
    fn an_indented_line_of_ordinary_sentences_on_its_own_is_prose() {
        for line in [
            "\tA paragraph with a first-line indent, typed with a tab. It goes on.",
            "    A paragraph indented by four spaces. It has a second sentence.",
            "  \tTwo spaces then a tab reach the fourth column. This is still prose.",
        ] {
            assert_eq!(mode_of(line), Prose, "{line:?}");
        }
        let text = "The first paragraph.\n\tThe second opens with a tab, as typed.\nThe third.";
        assert_eq!(modes(text), [Prose, Prose, Prose]);
    }

    #[test]
    fn an_indented_line_on_its_own_that_is_symbol_dense_is_code() {
        for line in [
            "    let total = f(a[i], b[j]);",
            "\t});",
            "    if (a && b) { run(); }",
        ] {
            assert_eq!(mode_of(line), Code, "{line:?}");
        }
    }

    #[test]
    fn the_density_of_an_indented_line_on_its_own_leaves_out_the_indentation() {
        // Twelve non-whitespace characters after the indentation, three of them symbols: exactly
        // a quarter.
        assert_eq!(mode_of("    Call fn(now)."), Prose);
        assert_eq!(mode_of("\tCall fn(now)."), Prose);
        // Eleven, three of them symbols: just over a quarter.
        assert_eq!(mode_of("    Call f(now)."), Code);
        assert_eq!(mode_of("\tCall f(now)."), Code);
    }

    #[test]
    fn a_run_of_two_indented_lines_of_ordinary_words_is_code_both_lines() {
        let text = "Before the run.\n\
                    \tThe first line holds ordinary words.\n\
                    \tSo does the second line of the run.\n\
                    After the run.";
        assert_eq!(modes(text), [Prose, Code, Code, Prose]);
        // A carriage return followed by a newline is one mark, with no blank line inside it.
        let text =
            "\tThe first line holds ordinary words.\r\n\tSo does the second line of the run.";
        assert_eq!(modes(text), [Code, Code]);
    }

    #[test]
    fn a_blank_or_unindented_line_between_two_indented_lines_leaves_each_on_its_own() {
        for between in ["\n\n", "\r\r", "\r\n\r\n", "\n    \n", "\n\t\n"] {
            let text = format!(
                "\tThe first paragraph opens with a tab.{between}\tSo does the second paragraph."
            );
            assert_eq!(modes(&text), [Prose, Prose], "{text:?}");
        }
        let text = "\tThe first paragraph opens with a tab.\n\
                    The second does not.\n\
                    \tThe third opens with a tab again.";
        assert_eq!(modes(text), [Prose, Prose, Prose]);
    }

    #[test]
    fn an_indented_list_item_or_short_line_on_its_own_is_a_fragment() {
        for line in [
            "    - An item indented by four spaces",
            "\t1. A numbered item",
            "\tMeeting notes",
            "    Next steps:",
        ] {
            assert_eq!(mode_of(line), Fragment, "{line:?}");
        }
    }

    #[test]
    fn a_symbol_dense_line_is_code() {
        for line in ["});", "if (a && b) { run(); }", "---", "x = f(a[i], b[j]);"] {
            assert_eq!(mode_of(line), Code, "{line:?}");
        }
    }

    #[test]
    fn the_density_threshold_falls_between_a_quarter_and_just_over() {
        // Twelve non-whitespace characters, three of them symbols: exactly a quarter.
        assert_eq!(mode_of("Call fn(now)."), Prose);
        // Eleven, three of them symbols: just over a quarter.
        assert_eq!(mode_of("Call f(now)."), Code);
    }

    #[test]
    fn each_paragraph_of_a_mixed_document_is_classified_on_its_own() {
        let text = "Some prose opens the document.\r\
                    The steps\r\
                    - Fetch the branch\r\
                    - Run the tests\r\
                    ```\r\
                    cargo test --workspace\r\
                    ```\r\
                    Then the prose resumes, as it does here.\r\
                    \tindented();\r\
                    And it ends with a last sentence.";
        // The indented line is on its own between two lines of prose, and code by its density.
        assert_eq!(
            modes(text),
            [Prose, Fragment, Fragment, Fragment, Code, Code, Code, Prose, Code, Prose]
        );
    }

    #[test]
    fn the_same_document_classified_twice_gives_the_same_modes() {
        let text = "# Notes\n- An item\n```\nlet x = 1;\nOpen fence runs on.\n```\nA sentence.";
        assert_eq!(modes(text), modes(text));
        assert_eq!(split(text), split(text));
    }
}
