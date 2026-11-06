//! Measures the wall time of one full-document read through the native capture backend, the read
//! the analyzer's debounce loop makes after each pause in typing and at each safety poll, so that
//! its cost on a long document is known per surface. A harness run by hand against a real
//! application, not a test.
//!
//! It starts [`NativeCapture`], waits the given number of seconds for focus to land on the target
//! document, then calls `current_text` through the `Capture` trait the given number of times in a
//! row, each call timed from the request to the owned text in hand. It prints each read's time in
//! milliseconds and length in characters, then the median, the 5th and 95th percentiles by nearest
//! rank and the maximum of the reads that returned text, a distribution-free 95% interval for the
//! median from two order statistics, the share of each second those reads would occupy at five
//! reads a second, and the document's length in characters and in paragraphs,
//! split by the analyzer's own form-mode module. A read that fails, with no focused document for
//! instance, is counted and reported with its error, and its time is left out of the statistics.
//! It never prints the text or any part of it.
//!
//! The length is part of the finding, since a read can return less than the whole document. On 4
//! November 2026, against a text of 20,200 words in 253 paragraphs, 110,319 characters without
//! their marks, Word's `TextPattern` document range returned its first 65,000 characters, and a
//! Chromium `textarea`'s value its first 10,000, which `current_text` then took before trying the
//! `TextPattern`; Notepad and a Chromium `contenteditable` field returned the whole text. #153
//! records the times. Since #160 the backend reads the `TextPattern` before the value and reads the
//! document range in parts past a cap, and #160 records the whole read's times.
//!
//! `--expect <file>` names a file holding the text the surface is expected to deliver, written from
//! the text the document was generated from with the surface's own paragraph marks. Each read is
//! compared with it, and the harness prints whether they are equal, the first character at which
//! they differ when they are not, and a hash of the expected text and of the first read, so that a
//! read joined from parts is shown whole with no boundary doubled or dropped. With `RUST_LOG=info`
//! the backend logs, once per focus, a read that took more than one part, with the count.
//!
//! Run from the repository root with a release build, so that a debug build's own overhead stays
//! out of the number. The target document is focused within the wait by a click inside it, since a
//! scripted foreground change alone has been seen to raise no focus event the backend acts on:
//!
//! ```text
//! cargo run --release -p wa-engine --example read_cost -- --reads 30 --wait 20 --expect <file>
//! ```

use std::error::Error;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wa_engine::analyzer::form_mode;
use wa_engine::capture::native::NativeCapture;
use wa_engine::capture::Capture;

/// Reads a second at the analyzer's 200 ms poll before #154 made the text-changed event the
/// trigger: the cadence #153 and #160 report each read's share of a second against.
const POLLS_PER_SECOND: f64 = 5.0;

/// The most reads one run makes. Past about 1,070, half to the power of the count underflows to
/// zero, and [`median_interval`] starts its binomial sum from that power.
const MAX_READS: usize = 1000;

/// The probability each tail of the median's interval may leave out, for a 95% interval.
const TAIL: f64 = 0.025;

const USAGE: &str = "usage: read_cost [--reads <count>] [--wait <seconds>] [--expect <file>]";

type Failure = Box<dyn Error>;

fn main() -> Result<(), Failure> {
    env_logger::init();
    let (reads, wait, expect) = parse_args()?;
    let expected = expect.map(std::fs::read_to_string).transpose()?;

    let capture = NativeCapture::start()?;
    println!(
        "Native capture running. Waiting {}s for focus to land on the target document.",
        wait.as_secs()
    );
    std::thread::sleep(wait);
    println!("Reading the focused document {reads} times in a row.");

    // A runtime for the trait calls alone; the rest of the harness is plain std.
    let runtime = tokio::runtime::Runtime::new()?;
    let outcomes = runtime.block_on(time_reads(&capture, reads));
    drop(capture);

    let mut durations = Vec::with_capacity(outcomes.len());
    let mut lengths = Vec::with_capacity(outcomes.len());
    let mut paragraphs = None;
    let mut first_hash = None;
    let mut equal_reads = 0;
    let mut failures = 0;
    for (index, (elapsed, outcome)) in outcomes.iter().enumerate() {
        let millis = millis(*elapsed);
        match outcome {
            Ok(text) => {
                let characters = text.chars().count();
                let comparison = match expected.as_deref().map(|want| first_difference(text, want))
                {
                    None => String::new(),
                    Some(None) => {
                        equal_reads += 1;
                        ", equal to the expected text".to_owned()
                    }
                    Some(Some(at)) => format!(", differs from the expected text at character {at}"),
                };
                println!("read {index}: {millis:.3} ms, {characters} characters{comparison}");
                durations.push(*elapsed);
                lengths.push(characters);
                if paragraphs.is_none() {
                    paragraphs = Some(form_mode::split(text).len());
                    first_hash = Some(hash_of(text));
                }
            }
            Err(error) => {
                println!("read {index}: failed after {millis:.3} ms: {error}");
                failures += 1;
            }
        }
    }

    println!("{failures} of {reads} reads failed");
    if let Some(want) = &expected {
        println!(
            "{equal_reads} of {} reads equal the expected text of {} characters; hash of the \
             expected text {:016x}, of the first read {}",
            lengths.len(),
            want.chars().count(),
            hash_of(want),
            first_hash.map_or_else(|| "none".to_owned(), |hash| format!("{hash:016x}"))
        );
    }
    let (Some(&shortest), Some(&longest)) = (lengths.iter().min(), lengths.iter().max()) else {
        return Err("no read returned text".into());
    };
    if shortest == longest {
        println!(
            "document: {shortest} characters, {} paragraphs, the same length on every read",
            paragraphs.unwrap_or_default()
        );
    } else {
        println!(
            "document: {shortest} to {longest} characters across the reads, {} paragraphs on the \
             first",
            paragraphs.unwrap_or_default()
        );
    }

    durations.sort();
    let median = nearest_rank(&durations, 0.5);
    let p95 = nearest_rank(&durations, 0.95);
    println!(
        "n={} median={:.3} ms p5={:.3} ms p95={:.3} ms max={:.3} ms",
        durations.len(),
        millis(median),
        millis(nearest_rank(&durations, 0.05)),
        millis(p95),
        millis(
            *durations
                .last()
                .expect("lengths is not empty, so neither is durations")
        ),
    );
    match median_interval(durations.len()) {
        Some(interval) => println!(
            "95% interval for the median: {:.3} to {:.3} ms, the order statistics {} and {} of {}, \
             with coverage {:.1}% for independent reads",
            millis(durations[interval.lower - 1]),
            millis(durations[interval.upper - 1]),
            interval.lower,
            interval.upper,
            durations.len(),
            interval.coverage * 100.0
        ),
        None => println!("too few reads for a 95% interval for the median"),
    }
    println!(
        "at {POLLS_PER_SECOND} reads a second: the median read occupies {:.1}% of each second, \
         the p95 read {:.1}%",
        share_of_second(median),
        share_of_second(p95)
    );
    Ok(())
}

/// A distribution-free interval for the median of `n` values, bounded by two of their order
/// statistics, ranked from 1 in ascending order.
struct MedianInterval {
    lower: usize,
    upper: usize,
    /// The probability that the interval holds the true median, for independent values.
    coverage: f64,
}

/// The narrowest interval for the median of `n` values whose tails each leave out at most
/// [`TAIL`]: the order statistics `k` and `n + 1 - k`, for the largest `k` at which a
/// Binomial(`n`, 1/2) count falls below `k` with probability [`TAIL`] or less. `None` below six
/// values, where no such `k` exists.
fn median_interval(n: usize) -> Option<MedianInterval> {
    let mut probability = 0.5_f64.powi(i32::try_from(n).ok()?);
    let mut below = 0.0;
    let mut lower = 0;
    for count in 0..n {
        if below + probability > TAIL {
            break;
        }
        below += probability;
        lower = count + 1;
        probability *= (n - count) as f64 / (count + 1) as f64;
    }
    (lower > 0).then(|| MedianInterval {
        lower,
        upper: n + 1 - lower,
        coverage: 1.0 - 2.0 * below,
    })
}

fn parse_args() -> Result<(usize, Duration, Option<PathBuf>), Failure> {
    let mut reads = 30;
    let mut wait = 10;
    let mut expect = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--reads" => reads = value.parse()?,
            "--wait" => wait = value.parse()?,
            "--expect" => expect = Some(PathBuf::from(value)),
            _ => return Err(USAGE.into()),
        }
    }
    if !(1..=MAX_READS).contains(&reads) {
        return Err(format!("--reads must be from 1 to {MAX_READS}").into());
    }
    Ok((reads, Duration::from_secs(wait), expect))
}

/// The index of the first character at which `read` and `expected` differ, or the shorter one's
/// length when one begins the other; `None` when they are equal.
fn first_difference(read: &str, expected: &str) -> Option<usize> {
    (read != expected).then(|| {
        read.chars()
            .zip(expected.chars())
            .take_while(|(a, b)| a == b)
            .count()
    })
}

/// A hash of `text` to print in its place.
fn hash_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Calls `current_text` `reads` times in a row, returning each call's wall time with its outcome.
async fn time_reads(
    capture: &NativeCapture,
    reads: usize,
) -> Vec<(Duration, Result<String, wa_engine::capture::CaptureError>)> {
    let mut outcomes = Vec::with_capacity(reads);
    for _ in 0..reads {
        let started = Instant::now();
        let outcome = capture.current_text().await;
        outcomes.push((started.elapsed(), outcome));
    }
    outcomes
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The percentage of each second that reads of `duration` take at [`POLLS_PER_SECOND`].
fn share_of_second(duration: Duration) -> f64 {
    duration.as_secs_f64() * POLLS_PER_SECOND * 100.0
}

/// The value at `quantile` of `sorted`, which is not empty, by the nearest-rank method.
fn nearest_rank(sorted: &[Duration], quantile: f64) -> Duration {
    let rank = (sorted.len() as f64 * quantile).ceil() as usize;
    sorted[rank.max(1) - 1]
}
