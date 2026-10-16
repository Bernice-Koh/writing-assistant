//! The one content hash the engine records: SHA-256 over bytes, written as lowercase hex. Every
//! hash the engine stores goes through [`sha256_hex`], so a hash recorded by one part of the
//! engine compares equal to the same bytes hashed by another.

use sha2::{Digest, Sha256};

/// The SHA-256 of `bytes`, as 64 lowercase hex digits.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two short vectors from FIPS 180-2's SHA-256 examples, and the empty input.
    #[test]
    fn known_inputs_hash_to_their_published_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
