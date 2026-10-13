//! Byte shuffling for floating-point pieces.
//!
//! A bf16, f16 or f32 value has a sign and an exponent that change slowly, and
//! mantissa bytes that look close to random. Interleaved, the skewed bytes sit
//! between the noisy ones, and a general compressor cannot see the pattern. Grouping
//! the bytes by position within each element puts the skewed bytes together.
//! The transform is lossless and exact, and it is undone on read.

/// Element widths the shuffle is used for: bf16 and f16, f32, and f64.
pub(crate) fn supports(width: u64) -> bool {
    matches!(width, 2 | 4 | 8)
}

/// Groups `data` by byte position within each `width`-byte element. `data.len()`
/// must be a multiple of `width`.
pub(crate) fn shuffle(data: &[u8], width: usize) -> Vec<u8> {
    let elements = data.len() / width;
    let mut out = vec![0u8; data.len()];
    for (i, element) in data.chunks_exact(width).enumerate() {
        for (b, &byte) in element.iter().enumerate() {
            out[b * elements + i] = byte;
        }
    }
    out
}

/// Reverses [`shuffle`].
pub(crate) fn unshuffle(data: &[u8], width: usize) -> Vec<u8> {
    let elements = data.len() / width;
    let mut out = vec![0u8; data.len()];
    for i in 0..elements {
        for b in 0..width {
            out[i * width + b] = data[b * elements + i];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_bytes_by_position() {
        // Two 4-byte elements: [a0 a1 a2 a3] [b0 b1 b2 b3].
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(shuffle(&data, 4), [1, 5, 2, 6, 3, 7, 4, 8]);
    }

    #[test]
    fn round_trips_every_supported_width() {
        let data: Vec<u8> = (0..960u32).map(|i| (i * 37 % 251) as u8).collect();
        for width in [2usize, 4, 8] {
            let shuffled = shuffle(&data, width);
            assert_eq!(unshuffle(&shuffled, width), data, "width {width}");
        }
    }
}
