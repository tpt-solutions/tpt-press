//! Element types, and how many bytes a tensor of each type takes.
//!
//! Most types are one value per element. Quantized GGUF types store elements in
//! fixed-size blocks, so a tensor's byte size is a whole number of blocks.

use crate::error::{Error, Result};

/// How a dtype is laid out in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Elements per block. 1 for plain types.
    pub block_elements: u64,
    /// Bytes per block.
    pub block_bytes: u64,
}

/// Layout of a dtype name, from safetensors or GGUF.
pub fn layout(dtype: &str) -> Option<Layout> {
    let plain = |bytes| {
        Some(Layout {
            block_elements: 1,
            block_bytes: bytes,
        })
    };
    let block = |elements, bytes| {
        Some(Layout {
            block_elements: elements,
            block_bytes: bytes,
        })
    };
    match dtype {
        // safetensors
        "BOOL" | "U8" | "I8" | "F8_E5M2" | "F8_E4M3" => plain(1),
        "I16" | "U16" | "F16" | "BF16" => plain(2),
        "I32" | "U32" | "F32" => plain(4),
        "I64" | "U64" | "F64" => plain(8),
        // GGUF plain types
        "Q4_0" => block(32, 18),
        "Q4_1" => block(32, 20),
        "Q5_0" => block(32, 22),
        "Q5_1" => block(32, 24),
        "Q8_0" => block(32, 34),
        "Q8_1" => block(32, 36),
        // GGUF k-quants
        "Q2_K" => block(256, 84),
        "Q3_K" => block(256, 110),
        "Q4_K" => block(256, 144),
        "Q5_K" => block(256, 176),
        "Q6_K" => block(256, 210),
        "Q8_K" => block(256, 292),
        _ => None,
    }
}

/// Bytes taken by a tensor of `dtype` and `shape`.
///
/// Returns an error for an unknown dtype, for a size that overflows, or for an
/// element count that is not a whole number of blocks.
pub fn byte_len(dtype: &str, shape: &[u64]) -> Result<u64> {
    let layout = layout(dtype).ok_or_else(|| Error::UnsupportedDtype(dtype.to_owned()))?;
    let elements = shape
        .iter()
        .try_fold(1u64, |acc, &d| acc.checked_mul(d))
        .ok_or(Error::Safetensors("tensor is too large".to_owned()))?;
    if elements % layout.block_elements != 0 {
        return Err(Error::Safetensors(format!(
            "{elements} elements is not a whole number of {dtype} blocks"
        )));
    }
    (elements / layout.block_elements)
        .checked_mul(layout.block_bytes)
        .ok_or(Error::Safetensors("tensor is too large".to_owned()))
}

/// Bytes in one block of `dtype`. Piece boundaries are multiples of this, so
/// no block is split between pieces.
pub fn block_bytes(dtype: &str) -> Result<u64> {
    layout(dtype)
        .map(|l| l.block_bytes)
        .ok_or_else(|| Error::UnsupportedDtype(dtype.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_types_are_one_value_per_element() {
        assert_eq!(byte_len("F32", &[2, 3]).unwrap(), 24);
        assert_eq!(byte_len("BF16", &[]).unwrap(), 2);
    }

    #[test]
    fn quantized_types_are_whole_blocks() {
        assert_eq!(byte_len("Q8_0", &[64]).unwrap(), 68);
        assert!(byte_len("Q8_0", &[33]).is_err());
    }

    #[test]
    fn unknown_dtype_is_rejected() {
        assert!(matches!(
            byte_len("XYZ", &[1]),
            Err(Error::UnsupportedDtype(_))
        ));
    }
}
