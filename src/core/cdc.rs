//! Content-Defined Chunking via Rabin-Karp rolling hash.
//!
//! Splits output into content-addressed chunks, then reorders so unchanged
//! chunks appear first — maximizing LLM prompt-cache prefix hit rate.

use sha2::{Digest, Sha256};
use std::collections::HashSet;

const PRIME: u64 = 1_000_000_007;
const BASE: u64 = 256;
const WINDOW: usize = 48;
const MIN_CHUNK: usize = 64;
const MAX_CHUNK: usize = 2048;
const TARGET_CHUNK: usize = 512;
const MASK: u64 = TARGET_CHUNK as u64 - 1;

/// Minimum output size to bother with CDC (below this, no cache benefit).
pub const CDC_MIN_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub offset: usize,
    pub length: usize,
    pub hash: String,
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

pub fn chunk(content: &str) -> Vec<Chunk> {
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return vec![];
    }

    if bytes.len() <= MIN_CHUNK {
        return vec![Chunk {
            offset: 0,
            length: bytes.len(),
            hash: sha256_hex(bytes),
        }];
    }

    let window = WINDOW.min(bytes.len());
    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut rolling = 0u64;
    let mut pow = 1u64;

    for _ in 0..window.saturating_sub(1) {
        pow = pow.wrapping_mul(BASE) % PRIME;
    }

    for i in 0..bytes.len() {
        rolling = rolling.wrapping_mul(BASE).wrapping_add(bytes[i] as u64) % PRIME;

        if i >= window {
            let old = bytes[i - window] as u64;
            rolling = (rolling + PRIME - old.wrapping_mul(pow) % PRIME) % PRIME;
        }

        let chunk_len = i + 1 - chunk_start;
        let is_boundary = chunk_len >= MIN_CHUNK && (rolling & MASK == 0);
        let is_max = chunk_len >= MAX_CHUNK;

        if is_boundary || is_max || i == bytes.len() - 1 {
            let slice = &bytes[chunk_start..=i];
            chunks.push(Chunk {
                offset: chunk_start,
                length: slice.len(),
                hash: sha256_hex(slice),
            });
            chunk_start = i + 1;
        }
    }

    chunks
}

/// Reorder chunks so unchanged ones (matching old_hashes) come first.
/// Returns the reassembled string with stable prefix.
pub fn stable_reorder(content: &str, new_chunks: &[Chunk], old_hashes: &HashSet<String>) -> String {
    if old_hashes.is_empty() || new_chunks.is_empty() {
        return content.to_string();
    }

    let bytes = content.as_bytes();
    let mut stable: Vec<&Chunk> = Vec::new();
    let mut changed: Vec<&Chunk> = Vec::new();

    for c in new_chunks {
        if old_hashes.contains(&c.hash) {
            stable.push(c);
        } else {
            changed.push(c);
        }
    }

    // If nothing changed or everything changed, skip reordering
    if stable.is_empty() || changed.is_empty() {
        return content.to_string();
    }

    let mut result = Vec::with_capacity(bytes.len() + 2);

    for c in &stable {
        let end = (c.offset + c.length).min(bytes.len());
        result.extend_from_slice(&bytes[c.offset..end]);
    }

    for c in &changed {
        let end = (c.offset + c.length).min(bytes.len());
        result.extend_from_slice(&bytes[c.offset..end]);
    }

    String::from_utf8_lossy(&result).into_owned()
}

/// Extract hash set from chunks for cache storage.
pub fn hashes(chunks: &[Chunk]) -> HashSet<String> {
    chunks.iter().map(|c| c.hash.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_empty() {
        assert!(chunk("").is_empty());
    }

    #[test]
    fn test_chunk_small_input() {
        let small = "hello world";
        let chunks = chunk(small);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, small.len());
    }

    #[test]
    fn test_chunk_deterministic() {
        let text = "a".repeat(5000);
        let c1 = chunk(&text);
        let c2 = chunk(&text);
        assert_eq!(c1.len(), c2.len());
        for (a, b) in c1.iter().zip(c2.iter()) {
            assert_eq!(a.hash, b.hash);
            assert_eq!(a.offset, b.offset);
            assert_eq!(a.length, b.length);
        }
    }

    #[test]
    fn test_chunk_respects_min_max() {
        let text = "x".repeat(10000);
        let chunks = chunk(&text);
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.length >= MIN_CHUNK, "chunk too small: {}", c.length);
            assert!(c.length <= MAX_CHUNK, "chunk too large: {}", c.length);
        }
    }

    #[test]
    fn test_stable_reorder_no_old_hashes() {
        let text = "hello world test";
        let chunks = chunk(text);
        let empty: HashSet<String> = HashSet::new();
        let result = stable_reorder(text, &chunks, &empty);
        assert_eq!(result, text);
    }

    #[test]
    fn test_stable_reorder_partial_change() {
        let text = "a".repeat(3000) + &"b".repeat(3000);
        let original_chunks = chunk(&text);
        let old_hashes = hashes(&original_chunks);

        // Modify the second half
        let modified = "a".repeat(3000) + &"c".repeat(3000);
        let new_chunks = chunk(&modified);

        let result = stable_reorder(&modified, &new_chunks, &old_hashes);

        // Stable chunks (from "aaa...") should be at the front
        assert!(result.starts_with("aaa"));
        assert_eq!(result.len(), modified.len());
    }

    #[test]
    fn test_stable_reorder_all_same() {
        let text = "x".repeat(5000);
        let chunks = chunk(&text);
        let old_hashes = hashes(&chunks);
        let result = stable_reorder(&text, &chunks, &old_hashes);
        assert_eq!(result, text);
    }

    #[test]
    fn test_hashes_extraction() {
        let text = "y".repeat(2000);
        let chunks = chunk(&text);
        let h = hashes(&chunks);
        assert_eq!(h.len(), chunks.len());
    }
}
