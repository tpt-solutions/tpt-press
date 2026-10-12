//! Parallel chunking: split input, compress chunks across cores, keep order.

use rayon::prelude::*;

use crate::codec;
use crate::error::{Error, Result};

/// Describes how a data type is split into chunks and compressed.
///
/// Domain crates implement this to control chunk boundaries and level.
/// The default implementation uses fixed-size chunks.
pub trait Pressable: Sync {
    /// Target size of each uncompressed chunk, in bytes. Must be non-zero.
    fn chunk_size(&self) -> usize;

    /// zstd level used for every chunk.
    fn level(&self) -> i32 {
        codec::DEFAULT_LEVEL
    }

    /// Splits `input` into chunks. The default splits at fixed `chunk_size()`
    /// boundaries. Domain crates may override this to split on content
    /// boundaries (for example, tensor edges).
    fn split<'a>(&self, input: &'a [u8]) -> Vec<&'a [u8]> {
        input.chunks(self.chunk_size().max(1)).collect()
    }
}

/// Compresses `input` into chunks in parallel. Output order matches input order.
///
/// Empty input yields no chunks.
pub fn compress_chunks<P: Pressable>(input: &[u8], pressable: &P) -> Result<Vec<Vec<u8>>> {
    if pressable.chunk_size() == 0 {
        return Err(Error::InvalidArgument("chunk_size must be non-zero"));
    }
    let level = pressable.level();
    pressable
        .split(input)
        .par_iter()
        .map(|chunk| codec::compress(chunk, level))
        .collect()
}

/// Decompresses chunks in parallel. Each chunk is capped at `max_chunk_output`
/// bytes. Output order matches input order.
pub fn decompress_chunks(chunks: &[Vec<u8>], max_chunk_output: usize) -> Result<Vec<Vec<u8>>> {
    chunks
        .par_iter()
        .map(|chunk| codec::decompress(chunk, max_chunk_output))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(usize);

    impl Pressable for Fixed {
        fn chunk_size(&self) -> usize {
            self.0
        }
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn round_trip_preserves_order_and_bytes() {
        let data = sample(1_000_003);
        let pressable = Fixed(64 * 1024);
        let chunks = compress_chunks(&data, &pressable).unwrap();
        assert_eq!(chunks.len(), data.len().div_ceil(64 * 1024));

        let restored = decompress_chunks(&chunks, 64 * 1024).unwrap();
        assert_eq!(restored.concat(), data);
    }

    #[test]
    fn empty_input_has_no_chunks() {
        let chunks = compress_chunks(&[], &Fixed(1024)).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn zero_chunk_size_is_rejected() {
        assert!(matches!(
            compress_chunks(b"abc", &Fixed(0)),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn custom_split_is_used() {
        struct Halves;
        impl Pressable for Halves {
            fn chunk_size(&self) -> usize {
                usize::MAX
            }
            fn split<'a>(&self, input: &'a [u8]) -> Vec<&'a [u8]> {
                let mid = input.len() / 2;
                vec![&input[..mid], &input[mid..]]
            }
        }

        let data = sample(100);
        let chunks = compress_chunks(&data, &Halves).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(decompress_chunks(&chunks, 100).unwrap().concat(), data);
    }
}
