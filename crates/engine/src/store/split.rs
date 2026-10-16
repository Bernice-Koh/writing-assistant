//! The split a passage is assigned to at insert: a pure function of its content hash and its kind,
//! with no clock, process or machine among its inputs, so one text of one kind lands in one split
//! in every store, under every build.

use super::passages::PassageKind;

/// The group a passage belongs to for training and evaluation. Only a train passage ever reaches a
/// training file; the other four are held out. A synthetic passage lands in train, bench or sealed,
/// and a captured passage in any of the five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    Train,
    /// Captured passages only.
    Monitor,
    Bench,
    /// Captured passages only.
    Gate,
    Sealed,
}

/// Each split a captured passage can land in, with its weight in every hundred.
const CAPTURED_WEIGHTS: &[(Split, u64)] = &[
    (Split::Train, 65),
    (Split::Monitor, 20),
    (Split::Bench, 5),
    (Split::Gate, 7),
    (Split::Sealed, 3),
];

/// Each split a synthetic passage can land in, with its weight: the captured weights of the three
/// splits it shares, so each split's share is its weight over 73.
const SYNTHETIC_WEIGHTS: &[(Split, u64)] =
    &[(Split::Train, 65), (Split::Bench, 5), (Split::Sealed, 3)];

/// The split for a passage of `kind` whose content hash is `content_hash`, the 64 lowercase hex
/// digits [`sha256_hex`](crate::hash::sha256_hex) gives for its text.
///
/// The hash's first eight bytes, read as a big-endian integer, are a point in the range from 0 to
/// 2^64, and each split the kind can land in owns a run of that range in proportion to its weight
/// over the kind's total, in the order train, monitor, bench, gate, sealed. SHA-256 is fixed by its
/// standard and the arithmetic is on fixed-width integers, so the split does not vary by process,
/// machine or toolchain version.
///
/// The weights hold until the bench and the judgement instrument are sized (#61, #63). A stored
/// passage keeps the split its row records, so a change to them affects only passages inserted
/// after it.
pub(super) fn split_for(content_hash: &str, kind: PassageKind) -> Split {
    let point = content_hash
        .get(..16)
        .and_then(|leading| u64::from_str_radix(leading, 16).ok())
        .expect("a content hash is the 64 hex digits sha256_hex writes");
    let weights = match kind {
        PassageKind::Synthetic => SYNTHETIC_WEIGHTS,
        PassageKind::Captured => CAPTURED_WEIGHTS,
    };
    let total: u64 = weights.iter().map(|(_, weight)| weight).sum();
    // The point scaled into the range from 0 to the total: a product of two 64-bit integers fits
    // in 128 bits, and shifting it down by 64 divides by 2^64 and leaves less than the total.
    let scaled = ((u128::from(point) * u128::from(total)) >> 64) as u64;
    let mut upper = 0;
    weights
        .iter()
        .find_map(|&(split, weight)| {
            upper += weight;
            (scaled < upper).then_some(split)
        })
        .expect("the scaled point is below the weights' total")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A content hash whose first eight bytes are `point`.
    fn hash_with_point(point: u64) -> String {
        format!("{point:016x}{}", "0".repeat(48))
    }

    /// The lowest point at which a split whose predecessors weigh `below` in all begins, out of a
    /// kind's `total`: the least point that scales to `below`.
    fn first_point(below: u64, total: u64) -> u64 {
        u64::try_from((u128::from(below) << 64).div_ceil(u128::from(total)))
            .expect("below is less than the total")
    }

    /// Pins the order of the splits along the range and the share of the range each owns, with the
    /// weights written out here rather than read from the constants under test.
    #[test]
    fn each_split_owns_the_run_of_the_range_its_weight_gives_it_in_order() {
        let cases = [
            (
                PassageKind::Captured,
                vec![
                    (Split::Train, 65),
                    (Split::Monitor, 20),
                    (Split::Bench, 5),
                    (Split::Gate, 7),
                    (Split::Sealed, 3),
                ],
            ),
            (
                PassageKind::Synthetic,
                vec![(Split::Train, 65), (Split::Bench, 5), (Split::Sealed, 3)],
            ),
        ];
        for (kind, weights) in cases {
            let total: u64 = weights.iter().map(|(_, weight)| weight).sum();
            let mut below = 0;
            for (split, weight) in weights {
                let first = first_point(below, total);
                assert_eq!(
                    split_for(&hash_with_point(first), kind),
                    split,
                    "{kind:?}: the first point of {split:?}"
                );
                below += weight;
                let last = if below == total {
                    u64::MAX
                } else {
                    first_point(below, total) - 1
                };
                assert_eq!(
                    split_for(&hash_with_point(last), kind),
                    split,
                    "{kind:?}: the last point of {split:?}"
                );
            }
        }
    }
}
