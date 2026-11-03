//! The split of the document's text into sentences, each with its position in the document.

use unicode_segmentation::UnicodeSegmentation;

/// One sentence of the document, trimmed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Sentence<'a> {
    /// The byte offset in the document's text where the trimmed sentence starts.
    pub(crate) start: usize,
    pub(crate) text: &'a str,
}

/// Splits `text` into its sentences.
///
/// Trimmed: `unicode_sentences` includes trailing whitespace up to the start of the next
/// sentence, which is not present after whatever is currently the *last* sentence in `text`.
/// Appending a new sentence therefore adds trailing whitespace to what was previously the last
/// sentence, changing its exact text with nothing the user actually edited in it. The analyzer's
/// cache is keyed by a sentence's text, so an untrimmed former-last sentence would miss the cache
/// and be rechecked on every sentence appended, the single most common way a document grows.
pub(crate) fn split(text: &str) -> Vec<Sentence<'_>> {
    text.unicode_sentences()
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .map(|sentence| Sentence {
            // `unicode_sentences` and `trim` both return slices of `text`, so the distance
            // between the two addresses is the sentence's byte offset in it.
            start: sentence.as_ptr().addr() - text.as_ptr().addr(),
            text: sentence,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_paragraph_into_its_sentences() {
        let sentences: Vec<&str> = split("First sentence. Second one! Is this the third?")
            .into_iter()
            .map(|sentence| sentence.text)
            .collect();
        assert_eq!(
            sentences,
            ["First sentence.", "Second one!", "Is this the third?"]
        );
    }

    #[test]
    fn each_sentence_starts_at_its_trimmed_texts_byte_offset_in_the_document() {
        let text = "  Café first. Second one!\n\nThird.";
        let sentences = split(text);
        for sentence in &sentences {
            assert_eq!(
                &text[sentence.start..sentence.start + sentence.text.len()],
                sentence.text
            );
        }
        let starts: Vec<usize> = sentences.iter().map(|sentence| sentence.start).collect();
        assert_eq!(starts, [2, 15, 28]);
    }

    #[test]
    fn appending_a_sentence_leaves_the_former_last_sentence_unchanged() {
        let before = split("First sentence. Second sentence.");
        let after = split("First sentence. Second sentence. Third sentence.");
        assert_eq!(before[..], after[..2]);
    }
}
