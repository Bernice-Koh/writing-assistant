//! Measures how long LanguageTool takes to answer a warm check in picky mode, one sentence per
//! request and in sequence, as the analyzer's grammar stage sends them. The one-second bound on a
//! steady-state check, `STEADY_STATE_TIMEOUT` in the engine's LanguageTool client, stands on 1,000
//! checks this harness timed against the 6.6 server under the bundled runtime, of sentences from
//! two public-domain books, a novel and a work of natural history.
//!
//! Each book is a Project Gutenberg plain-text file. Its body between the start and end markers
//! has its hard-wrapped lines joined into paragraphs, each paragraph is split as the analyzer
//! splits a document, and the sentences of three whitespace-separated words or more are kept. A
//! seeded shuffle draws the sample from each book, and twenty further sentences of each book warm
//! the server before any check is timed. A check is timed from sending its request to parsing its
//! response. Each timed check writes one row to a tab-separated file: the sentence's words, its
//! length in UTF-16 code units, LanguageTool's match count and the microseconds the check took,
//! with no sentence text. The summary printed is the n, the median, the 95th and 99th percentiles
//! by nearest rank, and the maximum.
//!
//! Run from the repository root, against a LanguageTool server already listening on the port
//! given, such as one started from the bundled runtime with
//! `src-tauri/resources/jre/bin/java.exe -cp src-tauri/resources/languagetool/languagetool-server.jar
//! org.languagetool.server.HTTPServer --port 8081`:
//!
//! ```text
//! cargo run -p wa-engine --example picky_latency --release -- <port> <sample per book> <out.tsv> <book.txt>...
//! ```
//!
//! The environment variable `SEED`, a nonzero decimal integer, replaces the shuffle's default
//! seed, so that a repeat run draws another sample. Without it every run draws the same one.

use std::error::Error;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde::Deserialize;
use unicode_segmentation::UnicodeSegmentation;

/// How many sentences of each book warm the server before any check is timed. They are drawn
/// after the book's sample, so no warm-up sentence is timed.
const WARM_UP_PER_BOOK: usize = 20;

/// The shortest sentence sampled, in whitespace-separated words.
const MIN_WORDS: usize = 3;

/// The shuffle's seed when `SEED` is not set.
const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

const USAGE: &str = "usage: picky_latency <port> <sample per book> <out.tsv> <book.txt>...";

type Failure = Box<dyn Error>;

fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [port, per_book, out, books @ ..] = args.as_slice() else {
        return Err(USAGE.into());
    };
    if books.is_empty() {
        return Err(USAGE.into());
    }
    let port: u16 = port.parse()?;
    let per_book: usize = per_book.parse()?;
    let seed = match std::env::var("SEED") {
        Ok(seed) => seed.parse()?,
        Err(_) => DEFAULT_SEED,
    };
    if seed == 0 {
        return Err(
            "SEED must be nonzero, since a xorshift seeded with zero draws only zero".into(),
        );
    }

    let mut shuffle = Xorshift(seed);
    let mut sample = Vec::new();
    let mut warm_up = Vec::new();
    for book in books {
        let sentences = sentences_of(&std::fs::read_to_string(book)?, book)?;
        println!(
            "{book}: {} sentences of {MIN_WORDS} words or more",
            sentences.len()
        );
        if sentences.len() < per_book + WARM_UP_PER_BOOK {
            return Err(format!(
                "{book} holds too few sentences for {per_book} timed checks and \
                 {WARM_UP_PER_BOOK} warm-up checks"
            )
            .into());
        }
        let order = shuffle.permutation(sentences.len());
        let drawn = |range: std::ops::Range<usize>| {
            order[range]
                .iter()
                .map(|&index| sentences[index].clone())
                .collect::<Vec<String>>()
        };
        sample.extend(drawn(0..per_book));
        warm_up.extend(drawn(per_book..per_book + WARM_UP_PER_BOOK));
    }

    // A runtime for the checks alone, so that reading the books and writing the rows stay out of
    // any async context.
    let runtime = tokio::runtime::Runtime::new()?;
    let checks = runtime.block_on(time_checks(port, &warm_up, &sample))?;

    let mut rows = String::from("words\tutf16\tmatches\tmicros\n");
    let mut durations = Vec::with_capacity(sample.len());
    for (sentence, (elapsed, matches)) in sample.iter().zip(checks) {
        durations.push(elapsed);
        writeln!(
            rows,
            "{}\t{}\t{matches}\t{}",
            words(sentence),
            sentence.encode_utf16().count(),
            elapsed.as_micros()
        )?;
    }
    std::fs::write(out, rows)?;

    durations.sort();
    let Some(max) = durations.last() else {
        return Err("no check was timed".into());
    };
    println!(
        "n={} median={:?} p95={:?} p99={:?} max={max:?}",
        durations.len(),
        nearest_rank(&durations, 0.5),
        nearest_rank(&durations, 0.95),
        nearest_rank(&durations, 0.99),
    );
    Ok(())
}

/// Checks each of `warm_up` untimed, then each of `sample`, returning for each sample sentence how
/// long its check took and how many matches it found.
async fn time_checks(
    port: u16,
    warm_up: &[String],
    sample: &[String],
) -> Result<Vec<(Duration, usize)>, Failure> {
    let checker = Checker::new(port);
    for sentence in warm_up {
        checker.check(sentence).await?;
    }
    let mut checks = Vec::with_capacity(sample.len());
    for sentence in sample {
        checks.push(checker.check(sentence).await?);
    }
    Ok(checks)
}

/// The sentences of `raw`, the Project Gutenberg file `book`, of [`MIN_WORDS`] words or more.
fn sentences_of(raw: &str, book: &str) -> Result<Vec<String>, Failure> {
    let body = gutenberg_body(raw)
        .ok_or_else(|| format!("{book} has no Project Gutenberg start and end markers"))?;
    Ok(body
        .replace("\r\n", "\n")
        .split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .flat_map(|paragraph| split(&paragraph))
        .filter(|sentence| words(sentence) >= MIN_WORDS)
        .collect())
}

/// The text between the line that holds a Project Gutenberg file's start marker and its end
/// marker, which leaves out the licence and the catalogue header.
fn gutenberg_body(raw: &str) -> Option<&str> {
    let marker = raw.find("*** START OF")?;
    let start = marker + raw[marker..].find('\n')?;
    let end = raw.find("*** END OF")?;
    raw.get(start..end)
}

/// The analyzer's split of a document into sentences: Unicode's sentence boundaries, each sentence
/// trimmed, and the empty ones dropped. The engine's split is private to the analyzer, so it is
/// repeated here.
fn split(text: &str) -> Vec<String> {
    text.unicode_sentences()
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .map(str::to_owned)
        .collect()
}

fn words(sentence: &str) -> usize {
    sentence.split_whitespace().count()
}

/// The value at `quantile` of `sorted`, which is not empty, by the nearest-rank method.
fn nearest_rank(sorted: &[Duration], quantile: f64) -> Duration {
    let rank = (sorted.len() as f64 * quantile).ceil() as usize;
    sorted[rank.max(1) - 1]
}

/// Sends checks as the engine's LanguageTool client does: the `en-GB` variant, picky mode, and one
/// text per request.
struct Checker {
    http: reqwest::Client,
    url: String,
}

#[derive(Deserialize)]
struct CheckResponse {
    matches: Vec<serde_json::Value>,
}

impl Checker {
    fn new(port: u16) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: format!("http://localhost:{port}/v2/check"),
        }
    }

    /// How long LanguageTool took to answer a check of `text`, and how many matches it found.
    async fn check(&self, text: &str) -> Result<(Duration, usize), Failure> {
        let request = self.http.get(&self.url).query(&[
            ("language", "en-GB"),
            ("level", "picky"),
            ("text", text),
        ]);
        let started = Instant::now();
        let response = request.send().await?.error_for_status()?;
        let body: CheckResponse = response.json().await?;
        Ok((started.elapsed(), body.matches.len()))
    }
}

/// A xorshift generator, so that one seed draws one sample on every run.
struct Xorshift(u64);

impl Xorshift {
    fn draw(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// The indices `0..len` in the order a Fisher-Yates shuffle with this generator draws.
    fn permutation(&mut self, len: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..len).collect();
        for last in (1..len).rev() {
            let swapped = (self.draw() % (last as u64 + 1)) as usize;
            indices.swap(last, swapped);
        }
        indices
    }
}
