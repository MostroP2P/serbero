//! Deterministic prompt-bundle hashing.
//!
//! Implements the algorithm defined in
//! `contracts/prompt-bundle.md` §Hashing: SHA-256 over a fixed-order
//! concatenation of labelled segments separated by a single null byte.
//! Equal bytes produce equal hashes regardless of the order of paths
//! in `[prompts]`. The null-byte delimiter prevents boundary-collision
//! between adjacent files.

use sha2::{Digest, Sha256};

const PREFIX: &[u8] = b"serbero/phase3\0";

/// Compute the deterministic SHA-256 policy hash (hex, lowercase)
/// over the five prompt-bundle files in the fixed canonical order.
pub fn policy_hash(
    system: &str,
    classification: &str,
    escalation: &str,
    mediation_style: &str,
    message_templates: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX);
    feed(&mut hasher, b"system", system.as_bytes());
    feed(&mut hasher, b"classification", classification.as_bytes());
    feed(&mut hasher, b"escalation", escalation.as_bytes());
    feed(&mut hasher, b"mediation_style", mediation_style.as_bytes());
    // Last segment is NOT followed by a trailing delimiter, matching
    // `contracts/prompt-bundle.md`.
    hasher.update(b"message_templates");
    hasher.update(b"\0");
    hasher.update(message_templates.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest)
}

/// Same shape as [`policy_hash`], plus the cooperative
/// self-resolution bundle bytes appended as a sixth segment.
/// Feature 005 ships its templates as a separate prompt file; the
/// hash MUST extend over those bytes so a forensic replay can pin
/// the exact rendered string per session.
pub fn policy_hash_v2(
    system: &str,
    classification: &str,
    escalation: &str,
    mediation_style: &str,
    message_templates: &str,
    self_resolution: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX);
    feed(&mut hasher, b"system", system.as_bytes());
    feed(&mut hasher, b"classification", classification.as_bytes());
    feed(&mut hasher, b"escalation", escalation.as_bytes());
    feed(&mut hasher, b"mediation_style", mediation_style.as_bytes());
    feed(
        &mut hasher,
        b"message_templates",
        message_templates.as_bytes(),
    );
    // Final segment uses no trailing delimiter, matching the v1
    // shape's last-segment rule.
    hasher.update(b"self_resolution");
    hasher.update(b"\0");
    hasher.update(self_resolution.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest)
}

fn feed(hasher: &mut Sha256, label: &[u8], bytes: &[u8]) {
    hasher.update(label);
    hasher.update(b"\0");
    hasher.update(bytes);
    hasher.update(b"\0");
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // write!-into-String is infallible; unwrap is idiomatic here.
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_identical_hashes() {
        let a = policy_hash("s", "c", "e", "m", "t");
        let b = policy_hash("s", "c", "e", "m", "t");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn any_byte_change_flips_the_hash() {
        let base = policy_hash("s", "c", "e", "m", "t");
        assert_ne!(base, policy_hash("S", "c", "e", "m", "t"));
        assert_ne!(base, policy_hash("s", "C", "e", "m", "t"));
        assert_ne!(base, policy_hash("s", "c", "E", "m", "t"));
        assert_ne!(base, policy_hash("s", "c", "e", "M", "t"));
        assert_ne!(base, policy_hash("s", "c", "e", "m", "T"));
    }

    #[test]
    fn null_delimiter_disambiguates_adjacent_files() {
        // If the hash naively concatenated bytes, these two inputs
        // would collide:
        //   system="ab" classification="cd"   -> "abcd"
        //   system="abc" classification="d"   -> "abcd"
        // The null-byte delimiter MUST prevent that.
        let a = policy_hash("ab", "cd", "", "", "");
        let b = policy_hash("abc", "d", "", "", "");
        assert_ne!(a, b);
    }

    #[test]
    fn policy_hash_v2_identical_inputs_produce_identical_hashes() {
        let a = policy_hash_v2("s", "c", "e", "m", "t", "r");
        let b = policy_hash_v2("s", "c", "e", "m", "t", "r");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn policy_hash_v2_self_resolution_change_flips_hash() {
        let base = policy_hash_v2("s", "c", "e", "m", "t", "r");
        assert_ne!(base, policy_hash_v2("s", "c", "e", "m", "t", "R"));
        assert_ne!(base, policy_hash_v2("s", "c", "e", "m", "t", ""));
    }

    #[test]
    fn policy_hash_v2_differs_from_v1_even_with_empty_self_resolution() {
        // Pin the domain-separation guarantee: even when the v2 hash
        // is computed with an empty self-resolution segment, it must
        // NOT collide with the v1 hash on the same first five
        // arguments. The labelled segment for `self_resolution`
        // (length 0) still feeds bytes into the hasher, so the two
        // outputs diverge.
        let v1 = policy_hash("s", "c", "e", "m", "t");
        let v2 = policy_hash_v2("s", "c", "e", "m", "t", "");
        assert_ne!(v1, v2);
    }
}
