//! The read of an element's whole text. A provider can return less than a whole range from one
//! `GetText` call: on 4 November 2026 Word's returned the first 65,000 characters of a longer
//! document from `GetText(-1)` and `GetText(200000)` alike, and a Chromium `textarea`'s value held
//! its first 10,000 (#153). So the `TextPattern` document range is read in parts, and the value is
//! read only where the element has no `TextPattern` or the range read fails.
//!
//! Between parts the range's start moves by paragraphs, not characters. In Word a character move
//! cost about 12 µs a character, over a second per read of a document of 110,000 characters, and
//! skipped text at a part boundary on a document with auto-numbered lists, since `GetText` delivers
//! each list number as text and the character move does not count it. A paragraph move past the
//! same 146 paragraphs took under a millisecond and landed after the 146th carriage return (#160).

use windows::Win32::UI::Accessibility::{
    IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextRange,
    IUIAutomationValuePattern, TextPatternRangeEndpoint_Start, TextUnit_Paragraph,
    UIA_TextPatternId, UIA_ValuePatternId,
};

use super::error::NativeCaptureError;

/// The most parts one read joins. At Word's 65,000 characters a part, 256 parts hold 16,640,000
/// characters, about three million words of English prose; no document reaches it. It stops a
/// provider that keeps returning new text from holding the capture thread, which also serves the
/// overlay's positions.
const MAX_PARTS: usize = 256;

/// How many code units at a part's start must also stand in the previous part for [`placement`]
/// to place it by search: long enough that a match is the same text, not a common word.
const PLACEMENT_PROBE: usize = 64;

const CARRIAGE_RETURN: u16 = 0x0D;
const LINE_FEED: u16 = 0x0A;

/// The mark Word's `GetText` ends a table cell and a table row with, in place of a carriage
/// return. Word's provider counts each as a paragraph (#160).
const CELL_MARK: u16 = 0x07;

/// An element's whole text, and the number of parts the read took: one for a value, or for a
/// document range that one `GetText` call returned whole.
#[derive(Debug)]
pub struct DocumentText {
    pub text: String,
    pub parts: usize,
}

/// Reads `element`'s whole text: its `TextPattern` document range in parts where it supports the
/// pattern, and its value where it does not or the range read fails.
///
/// # Errors
/// The range read's error when that read fails and no value stands in for it, the value read's
/// error when the element has a value and no `TextPattern`, and
/// [`NativeCaptureError::NoTextPattern`] when it has neither.
pub fn document_text(element: &IUIAutomationElement) -> Result<DocumentText, NativeCaptureError> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let text_pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }.ok();
    choose(
        text_pattern.map(|pattern| move || read_document_range(&pattern)),
        || read_value(element),
    )
}

/// The text an element gives: the range read where the element supports the `TextPattern`, and the
/// value where it does not or the range read fails. `read_value` answers `None` for an element with
/// no `ValuePattern`, and runs only when the value is needed.
fn choose<R, V>(read_range: Option<R>, read_value: V) -> Result<DocumentText, NativeCaptureError>
where
    R: FnOnce() -> Result<DocumentText, NativeCaptureError>,
    V: FnOnce() -> Option<Result<String, NativeCaptureError>>,
{
    let range_error = match read_range.map(|read| read()) {
        Some(Ok(text)) => return Ok(text),
        Some(Err(error)) => Some(error),
        None => None,
    };
    match (read_value(), range_error) {
        (Some(Ok(text)), range_error) => {
            if let Some(error) = range_error {
                log::debug!("the document range read failed, so the value is the text: {error}");
            }
            Ok(DocumentText { text, parts: 1 })
        }
        (Some(Err(_)) | None, Some(range_error)) => Err(range_error),
        (Some(Err(value_error)), None) => Err(value_error),
        (None, None) => Err(NativeCaptureError::NoTextPattern),
    }
}

fn read_document_range(
    pattern: &IUIAutomationTextPattern,
) -> Result<DocumentText, NativeCaptureError> {
    // SAFETY: `pattern` was just obtained from a live element. DocumentRange returns a new range
    // object, so the read's moves change no range anyone else holds.
    let mut range = unsafe { pattern.DocumentRange() }?;
    let joined = read_in_parts(&mut range)?;
    log::debug!(
        "read the document range in {} parts, {} code units, ended by {:?}",
        joined.parts,
        joined.text.len(),
        joined.end
    );
    Ok(DocumentText {
        text: String::from_utf16_lossy(&joined.text),
        parts: joined.parts,
    })
}

fn read_value(element: &IUIAutomationElement) -> Option<Result<String, NativeCaptureError>> {
    // SAFETY: `element` is live; GetCurrentPatternAs fails safely when unsupported.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .ok()?;
    // SAFETY: `pattern` was just obtained from a live element.
    Some(
        unsafe { pattern.CurrentValue() }
            .map(|value| value.to_string())
            .map_err(NativeCaptureError::from),
    )
}

/// A text range read in parts: a live `IUIAutomationTextRange` in the backend, and a scripted or
/// simulated provider in this module's tests, so the joining is tested without a live element.
trait PartSource {
    /// The text from the range's start to its end, as much of it as the provider returns from one
    /// call, in UTF-16 code units.
    fn read(&mut self) -> Result<Vec<u16>, NativeCaptureError>;

    /// Moves the range's start forward by `count` paragraphs, answering the number of paragraphs
    /// the provider reports it moved.
    fn skip_paragraphs(&mut self, count: usize) -> Result<usize, NativeCaptureError>;
}

impl PartSource for IUIAutomationTextRange {
    fn read(&mut self) -> Result<Vec<u16>, NativeCaptureError> {
        // SAFETY: `self` is a live range; -1 asks for all of its text, which a provider may cap.
        Ok(unsafe { self.GetText(-1) }?.to_vec())
    }

    fn skip_paragraphs(&mut self, count: usize) -> Result<usize, NativeCaptureError> {
        let count = i32::try_from(count).unwrap_or(i32::MAX);
        // SAFETY: `self` is a live range; moving its own start changes only this range object.
        let moved = unsafe {
            self.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Paragraph, count)
        }?;
        Ok(usize::try_from(moved).unwrap_or(0))
    }
}

/// Why a read in parts ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartsEnd {
    /// The part read after a move held nothing past the text the previous part already held: the
    /// end of the document.
    Whole,
    /// The part held no paragraph mark with text after it, so the range's start had nothing to
    /// move past: the rest of the document, or the start of a paragraph longer than one part.
    NoMark,
    /// The move reported no paragraph moved.
    NoProgress,
    /// The part read after a move came back empty: the move reached the document's end.
    Empty,
    /// The part read after a move could not be placed in the previous part.
    Unplaced,
    /// [`MAX_PARTS`] parts were read.
    Bound,
}

/// The text a read in parts joined, the number of parts that brought new text, and why it ended.
struct Joined {
    text: Vec<u16>,
    parts: usize,
    end: PartsEnd,
}

/// Reads `source` in parts and joins them. After each part, the range's start moves by paragraphs
/// past the part's paragraph marks that have text after them ([`paragraph_ends`]), and the next
/// part is read from there. That part begins with the text the previous part held after the mark
/// the move reached, which is kept once. A move that reports a shortfall is logged at `debug` with
/// both counts, and the read resumes after the last mark it did move past.
///
/// The read ends with the text joined so far when a part holds nothing past the text already read
/// (the end of the document), when a part has no paragraph mark with text after it, and at
/// [`MAX_PARTS`]. It ends with the part read before the move, logged at `debug`, when the move
/// reports no progress, when the part read after it is empty, or when that part cannot be placed
/// in the one before ([`placement`]), so that no text is doubled or skipped without a log line.
fn read_in_parts<S: PartSource>(source: &mut S) -> Result<Joined, NativeCaptureError> {
    let mut text = Vec::new();
    let mut part = source.read()?;
    let mut parts = 1;
    loop {
        let marks = paragraph_ends(&part);
        if marks.is_empty() {
            return Ok(finish(text, &part, parts, PartsEnd::NoMark));
        }
        if parts == MAX_PARTS {
            log::debug!("the document range read stopped at its bound of {MAX_PARTS} parts");
            return Ok(finish(text, &part, parts, PartsEnd::Bound));
        }
        let asked = marks.len();
        let moved = source.skip_paragraphs(asked)?;
        if moved != asked {
            log::debug!(
                "after part {parts}: the range's start moved {moved} of {asked} paragraphs"
            );
        }
        if moved == 0 {
            return Ok(finish(text, &part, parts, PartsEnd::NoProgress));
        }
        let expected = marks[moved.min(asked) - 1];
        let next = source.read()?;
        if next.is_empty() {
            log::debug!("after part {parts}: the part read after the move was empty");
            return Ok(finish(text, &part, parts, PartsEnd::Empty));
        }
        let Some(resume) = placement(&part, expected, &next) else {
            log::debug!(
                "after part {parts}: a part of {} code units does not continue the part of {} \
                 before it, so the read ends with that part",
                next.len(),
                part.len()
            );
            return Ok(finish(text, &part, parts, PartsEnd::Unplaced));
        };
        if resume != expected {
            log::debug!(
                "after part {parts}: the next part begins at offset {resume} of the part before \
                 it, where the move was expected to reach {expected}"
            );
        }
        text.extend_from_slice(&part[..resume]);
        if next.len() == part.len() - resume {
            return Ok(finish(text, &next, parts, PartsEnd::Whole));
        }
        part = next;
        parts += 1;
    }
}

fn finish(mut text: Vec<u16>, last: &[u16], parts: usize, end: PartsEnd) -> Joined {
    text.extend_from_slice(last);
    Joined { text, parts, end }
}

/// The offsets just past each paragraph mark in `part` that has text after it. A mark is a carriage
/// return, a newline, the two together as one mark, or [`CELL_MARK`]. A mark at the very end is
/// left out, so the text after the last offset is never empty and the next part has something to
/// begin with, and a carriage return at the end may be the first half of a pair whose newline the
/// next part holds.
fn paragraph_ends(part: &[u16]) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut index = 0;
    while index < part.len() {
        let end = match part[index] {
            CARRIAGE_RETURN if part.get(index + 1) == Some(&LINE_FEED) => index + 2,
            CARRIAGE_RETURN | LINE_FEED | CELL_MARK => index + 1,
            _ => {
                index += 1;
                continue;
            }
        };
        if end < part.len() {
            ends.push(end);
        }
        index = end;
    }
    ends
}

/// The offset in `previous` at which `next` begins, so that `previous` from that offset on is the
/// start of `next`. `expected`, where the move was asked to land, is tried first. A provider that
/// counts a paragraph this module does not, such as a section break, which Word delivers as a lone
/// U+000C and counts as a paragraph, lands the move earlier; then every offset past the start of
/// `previous` that holds the first [`PLACEMENT_PROBE`] code units of `next` is tried, nearest
/// `expected` first, and the first from which `previous` runs on as `next` is taken. `None` when
/// no offset does, as when the move landed past the end of `previous`.
fn placement(previous: &[u16], expected: usize, next: &[u16]) -> Option<usize> {
    if next.starts_with(&previous[expected..]) {
        return Some(expected);
    }
    let probe = &next[..next.len().min(PLACEMENT_PROBE)];
    let mut candidates: Vec<usize> = (1..previous.len())
        .filter(|&offset| previous[offset..].starts_with(probe))
        .collect();
    candidates.sort_by_key(|&offset| offset.abs_diff(expected));
    candidates
        .into_iter()
        .find(|&offset| next.starts_with(&previous[offset..]))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;

    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// A provider that answers each read with the next scripted part and each move with the next
    /// scripted count, recording the counts asked.
    struct Script {
        parts: VecDeque<Vec<u16>>,
        moves: VecDeque<usize>,
        asked: Vec<usize>,
    }

    impl Script {
        fn new(parts: &[&str], moves: &[usize]) -> Self {
            Self {
                parts: parts.iter().map(|part| units(part)).collect(),
                moves: moves.iter().copied().collect(),
                asked: Vec::new(),
            }
        }
    }

    impl PartSource for Script {
        fn read(&mut self) -> Result<Vec<u16>, NativeCaptureError> {
            Ok(self.parts.pop_front().expect("a read past the script"))
        }

        fn skip_paragraphs(&mut self, count: usize) -> Result<usize, NativeCaptureError> {
            self.asked.push(count);
            Ok(self.moves.pop_front().expect("a move past the script"))
        }
    }

    /// A provider like Word's: a read returns at most `cap` code units from the range's start, and
    /// a paragraph move goes past the next carriage return, stopping at the last paragraph's start.
    struct Capped {
        text: Vec<u16>,
        cap: usize,
        start: usize,
    }

    impl PartSource for Capped {
        fn read(&mut self) -> Result<Vec<u16>, NativeCaptureError> {
            let end = (self.start + self.cap).min(self.text.len());
            Ok(self.text[self.start..end].to_vec())
        }

        fn skip_paragraphs(&mut self, count: usize) -> Result<usize, NativeCaptureError> {
            let mut moved = 0;
            while moved < count {
                let Some(at) = self.text[self.start..]
                    .iter()
                    .position(|&unit| unit == CARRIAGE_RETURN)
                else {
                    break;
                };
                self.start += at + 1;
                moved += 1;
            }
            Ok(moved)
        }
    }

    /// `count` distinct paragraphs of about 70 characters with a carriage return between each pair
    /// and none after the last, as Word's document range delivers them.
    fn paragraphs(count: usize) -> String {
        (0..count)
            .map(|index| {
                format!(
                    "Paragraph {index:03} holds a sentence written for the check, then another."
                )
            })
            .collect::<Vec<_>>()
            .join("\r")
    }

    fn read(source: &mut impl PartSource) -> (String, usize, PartsEnd) {
        let joined = read_in_parts(source).expect("the scripted reads succeed");
        (
            String::from_utf16(&joined.text).expect("the parts are whole UTF-16"),
            joined.parts,
            joined.end,
        )
    }

    #[test]
    fn one_part_with_no_paragraph_mark_is_one_read_and_no_move() {
        let mut script = Script::new(&["A single line."], &[]);
        assert_eq!(
            read(&mut script),
            ("A single line.".to_owned(), 1, PartsEnd::NoMark)
        );
        assert!(script.asked.is_empty());
    }

    #[test]
    fn one_part_is_confirmed_whole_by_reading_its_last_paragraph_again() {
        let mut script = Script::new(&["First.\rSecond.\rLast.", "Last."], &[2]);
        assert_eq!(
            read(&mut script),
            ("First.\rSecond.\rLast.".to_owned(), 1, PartsEnd::Whole)
        );
        assert_eq!(script.asked, vec![2]);
    }

    #[test]
    fn two_parts_join_with_the_text_read_twice_kept_once() {
        let mut script = Script::new(
            &[
                "First.\rSecond.\rThi",
                "Third, the rest.\rFourth.",
                "Fourth.",
            ],
            &[2, 1],
        );
        assert_eq!(
            read(&mut script),
            (
                "First.\rSecond.\rThird, the rest.\rFourth.".to_owned(),
                2,
                PartsEnd::Whole
            )
        );
        assert_eq!(script.asked, vec![2, 1]);
    }

    #[test]
    fn a_capped_provider_is_read_whole_in_two_parts() {
        let text = paragraphs(20);
        let mut capped = Capped {
            text: units(&text),
            cap: 900,
            start: 0,
        };
        assert_eq!(read(&mut capped), (text, 2, PartsEnd::Whole));
    }

    #[test]
    fn a_last_part_that_is_empty_ends_the_read_with_the_part_before_it() {
        let mut script = Script::new(&["First.\rLast.", ""], &[1]);
        assert_eq!(
            read(&mut script),
            ("First.\rLast.".to_owned(), 1, PartsEnd::Empty)
        );
    }

    #[test]
    fn a_move_that_reports_a_shortfall_resumes_after_the_last_mark_it_moved_past() {
        // Asked to move past three marks, the provider reports two, so the next part begins at
        // "Third." and that text is kept once.
        let mut script = Script::new(
            &[
                "First.\rSecond.\rThird.\rFou",
                "Third.\rFourth, whole.",
                "Fourth, whole.",
            ],
            &[2, 1],
        );
        assert_eq!(
            read(&mut script),
            (
                "First.\rSecond.\rThird.\rFourth, whole.".to_owned(),
                2,
                PartsEnd::Whole
            )
        );
        assert_eq!(script.asked, vec![3, 1]);
    }

    #[test]
    fn a_move_that_reports_no_progress_ends_the_read_with_the_part() {
        let mut script = Script::new(&["First.\rSecond."], &[0]);
        assert_eq!(
            read(&mut script),
            ("First.\rSecond.".to_owned(), 1, PartsEnd::NoProgress)
        );
    }

    #[test]
    fn the_bound_ends_the_read_at_max_parts() {
        // Each part holds one paragraph and the start of the next, so each brings one paragraph.
        let text = paragraphs(MAX_PARTS + 40);
        let mut capped = Capped {
            text: units(&text),
            cap: 100,
            start: 0,
        };
        let (joined, parts, end) = read(&mut capped);
        assert_eq!((parts, end), (MAX_PARTS, PartsEnd::Bound));
        assert!(text.starts_with(&joined));
        assert!(joined.len() < text.len());
    }

    #[test]
    fn a_move_that_lands_before_the_expected_mark_is_placed_by_search() {
        // The provider counts the form feed as a paragraph, as Word counts a section break, so a
        // move past the first part's two carriage returns lands at the paragraph after the form
        // feed, 81 code units before the second carriage return's end.
        let document = format!(
            "{}\r{}\x0C{}\r{}",
            "a".repeat(80),
            "b".repeat(80),
            "c".repeat(80),
            "d".repeat(120)
        );
        let first = &document[..document.len() - 80];
        let landed = &document[162..];
        let last = &document[243..];
        let mut script = Script::new(&[first, landed, last], &[2, 1]);
        assert_eq!(read(&mut script), (document.clone(), 2, PartsEnd::Whole));
        assert_eq!(script.asked, vec![2, 1]);
    }

    #[test]
    fn a_part_that_cannot_be_placed_ends_the_read_with_the_part_before_it() {
        let mut script = Script::new(&["First.\rSecond.", "Unrelated text."], &[1]);
        assert_eq!(
            read(&mut script),
            ("First.\rSecond.".to_owned(), 1, PartsEnd::Unplaced)
        );
    }

    #[test]
    fn paragraph_ends_count_a_pair_once_and_leave_out_a_mark_at_the_end() {
        assert_eq!(
            paragraph_ends(&units("a\r\nb\nc\rd\x07e\r\n")),
            vec![3, 5, 7, 9]
        );
        assert_eq!(paragraph_ends(&units("a\rb\r")), vec![2]);
        assert_eq!(paragraph_ends(&units("no mark")), Vec::<usize>::new());
        assert_eq!(paragraph_ends(&units("")), Vec::<usize>::new());
    }

    #[test]
    fn a_carriage_return_at_a_part_end_stays_in_the_text_read_again() {
        // The newline of the pair is in the next part, so the pair is kept whole.
        let mut script = Script::new(
            &["First.\r\nSecond.\r", "Second.\r\nThird.", "Third."],
            &[1, 1],
        );
        assert_eq!(
            read(&mut script),
            ("First.\r\nSecond.\r\nThird.".to_owned(), 2, PartsEnd::Whole)
        );
    }

    /// The type of a range read, for a call that passes none.
    type RangeRead = fn() -> Result<DocumentText, NativeCaptureError>;

    fn text(parts: usize) -> Result<DocumentText, NativeCaptureError> {
        Ok(DocumentText {
            text: "range".to_owned(),
            parts,
        })
    }

    fn chosen(result: Result<DocumentText, NativeCaptureError>) -> Result<(String, usize), String> {
        result
            .map(|read| (read.text, read.parts))
            .map_err(|error| error.to_string())
    }

    #[test]
    fn the_range_is_the_text_and_the_value_is_not_read() {
        let value_read = Cell::new(false);
        let result = choose(Some(|| text(2)), || {
            value_read.set(true);
            Some(Ok("value".to_owned()))
        });
        assert_eq!(chosen(result), Ok(("range".to_owned(), 2)));
        assert!(!value_read.get());
    }

    #[test]
    fn a_failed_range_read_falls_back_to_the_value() {
        let result = choose(Some(|| Err(NativeCaptureError::TextNotFound)), || {
            Some(Ok("value".to_owned()))
        });
        assert_eq!(chosen(result), Ok(("value".to_owned(), 1)));
    }

    #[test]
    fn a_failed_range_read_with_no_value_answers_the_range_error() {
        let result = choose(Some(|| Err(NativeCaptureError::TextNotFound)), || None);
        assert_eq!(
            chosen(result),
            Err(NativeCaptureError::TextNotFound.to_string())
        );
    }

    #[test]
    fn a_failed_range_read_and_a_failed_value_answer_the_range_error() {
        let result = choose(Some(|| Err(NativeCaptureError::TextNotFound)), || {
            Some(Err(NativeCaptureError::NoValuePattern))
        });
        assert_eq!(
            chosen(result),
            Err(NativeCaptureError::TextNotFound.to_string())
        );
    }

    #[test]
    fn with_no_text_pattern_the_value_stands() {
        let result = choose(None::<RangeRead>, || Some(Ok("value".to_owned())));
        assert_eq!(chosen(result), Ok(("value".to_owned(), 1)));
    }

    #[test]
    fn with_no_text_pattern_a_failed_value_answers_its_error() {
        let result = choose(None::<RangeRead>, || {
            Some(Err(NativeCaptureError::NoValuePattern))
        });
        assert_eq!(
            chosen(result),
            Err(NativeCaptureError::NoValuePattern.to_string())
        );
    }

    #[test]
    fn with_neither_pattern_there_is_no_text_pattern() {
        let result = choose(None::<RangeRead>, || None);
        assert_eq!(
            chosen(result),
            Err(NativeCaptureError::NoTextPattern.to_string())
        );
    }
}
