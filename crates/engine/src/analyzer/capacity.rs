//! The capacity rule of the analyzer's two caches: before a recheck's first lookup, each cache's
//! capacity follows the text just read, sentences for the sentence cache and paragraphs for the
//! paragraph cache.
//!
//! A fixed capacity smaller than a document costs the document every entry: the deterministic stage
//! and the grammar stage walk the units in document order, so by the time a pass reaches a unit, the
//! misses since its last use have put more entries than the cache holds and the least-recently-used
//! rule has evicted it (#169).

use std::num::NonZeroUsize;

/// The capacity a cache of `capacity` takes for a text of `units` units: twice `units` when that
/// exceeds `capacity`, and `capacity` otherwise.
///
/// Twice, because a pass that misses puts a new entry and evicts the least recently used one: with
/// as many spare slots as the text has units, a pass evicts only entries for text the document no
/// longer holds, even when every unit changed at once, and never one it has yet to reach. Never
/// lower than `capacity`, because the scope moves between windows, and a short document checked
/// between two visits to a long one must not cost the long one its entries. A cache's capacity is
/// therefore twice the units of the longest text checked since the app started, or its floor.
pub(super) fn capacity_for(capacity: NonZeroUsize, units: usize) -> NonZeroUsize {
    NonZeroUsize::new(units.saturating_mul(2)).map_or(capacity, |twice| twice.max(capacity))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(capacity: usize) -> NonZeroUsize {
        NonZeroUsize::new(capacity).unwrap()
    }

    #[test]
    fn a_count_under_half_the_capacity_leaves_the_capacity() {
        assert_eq!(capacity_for(capacity(1000), 0), capacity(1000));
        assert_eq!(capacity_for(capacity(1000), 499), capacity(1000));
    }

    #[test]
    fn a_count_of_exactly_half_the_capacity_leaves_the_capacity() {
        assert_eq!(capacity_for(capacity(1000), 500), capacity(1000));
    }

    #[test]
    fn a_count_over_half_the_capacity_raises_it_to_twice_the_count() {
        assert_eq!(capacity_for(capacity(1000), 501), capacity(1002));
        assert_eq!(capacity_for(capacity(1000), 2011), capacity(4022));
    }

    #[test]
    fn a_count_whose_double_overflows_takes_the_largest_capacity() {
        assert_eq!(
            capacity_for(capacity(1000), usize::MAX),
            capacity(usize::MAX)
        );
    }
}
