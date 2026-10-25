//! Exponent/mantissa split for bf16 pieces.
//!
//! A bf16 value is 16 bits: a sign bit (15), an 8-bit exponent (14..7) and a
//! 7-bit mantissa (6..0). The byte shuffle cannot separate the exponent, because
//! its low bit shares a byte with the mantissa. This split gives each part its
//! own plane: one byte per value holds the exponent, and one byte holds the sign
//! and mantissa. The exponent of trained weights is strongly skewed, so its
//! plane compresses well on its own. The mantissa plane is close to random and
//! is stored as it is.
//!
//! The transform is exact. It is used only for `BF16` tensors.

use tpt_press_core::codec;

use crate::error::{Error, Result};

/// Encodes a bf16 piece as a split payload: the exponent plane and the other
/// plane, each as its own zstd frame. See [`Compression::Split`].
///
/// [`Compression::Split`]: crate::format::Compression::Split
pub(crate) fn encode_payload(raw: &[u8], level: i32) -> Result<Vec<u8>> {
    let (exponents, rest) = split_bf16(raw);
    let exponent_frame = codec::compress(&exponents, level)?;
    let rest_frame = codec::compress(&rest, level)?;
    let frame_len = u32::try_from(exponent_frame.len())
        .map_err(|_| Error::Corrupt("split piece is too large"))?;
    let mut out = Vec::with_capacity(4 + exponent_frame.len() + rest_frame.len());
    out.extend_from_slice(&frame_len.to_le_bytes());
    out.extend_from_slice(&exponent_frame);
    out.extend_from_slice(&rest_frame);
    Ok(out)
}

/// Decodes a split payload back to `raw_len` bytes of bf16 data.
pub(crate) fn decode_payload(stored: &[u8], raw_len: usize) -> Result<Vec<u8>> {
    if !raw_len.is_multiple_of(2) || stored.len() < 4 {
        return Err(Error::Corrupt("split piece is malformed"));
    }
    let frame_len = u32::from_le_bytes([stored[0], stored[1], stored[2], stored[3]]) as usize;
    let frames = &stored[4..];
    if frame_len > frames.len() {
        return Err(Error::Corrupt("split piece is malformed"));
    }
    let (exponent_frame, rest_frame) = frames.split_at(frame_len);
    let values = raw_len / 2;
    let exponents = codec::decompress(exponent_frame, values)?;
    let rest = codec::decompress(rest_frame, values)?;
    if exponents.len() != values || rest.len() != values {
        return Err(Error::Corrupt(
            "decoded piece length does not match its index",
        ));
    }
    Ok(join_bf16(&exponents, &rest))
}

/// Splits bf16 values (little-endian pairs of bytes) into an exponent plane and
/// a sign-and-mantissa plane. `data.len()` must be even.
pub(crate) fn split_bf16(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut exponents = Vec::with_capacity(data.len() / 2);
    let mut rest = Vec::with_capacity(data.len() / 2);
    let (pairs, _) = data.as_chunks::<2>();
    for &[low, high] in pairs {
        let value = u16::from_le_bytes([low, high]);
        exponents.push(((value >> 7) & 0xFF) as u8);
        rest.push((((value >> 8) & 0x80) | (value & 0x7F)) as u8);
    }
    (exponents, rest)
}

/// Reverses [`split_bf16`]. Both planes must hold the same number of values.
pub(crate) fn join_bf16(exponents: &[u8], rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(exponents.len() * 2);
    for (&exponent, &low) in exponents.iter().zip(rest) {
        let value = ((low as u16 & 0x80) << 8) | ((exponent as u16) << 7) | (low as u16 & 0x7F);
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sign_exponent_and_mantissa() {
        // 1.0 in bf16 is 0x3F80: sign 0, exponent 127, mantissa 0.
        // -2.0 is 0xC000: sign 1, exponent 128, mantissa 0.
        let data = [0x80, 0x3F, 0x00, 0xC0];
        let (exponents, rest) = split_bf16(&data);
        assert_eq!(exponents, [127, 128]);
        assert_eq!(rest, [0x00, 0x80]);
    }

    #[test]
    fn payload_round_trips_and_rejects_bad_lengths() {
        let data: Vec<u8> = (0..4096u32)
            .flat_map(|v| ((v.wrapping_mul(2654435761) >> 16) as u16).to_le_bytes())
            .collect();
        let stored = encode_payload(&data, 3).unwrap();
        assert_eq!(decode_payload(&stored, data.len()).unwrap(), data);
        assert!(decode_payload(&stored, data.len() + 1).is_err());
        assert!(decode_payload(&stored[..3], data.len()).is_err());
    }

    #[test]
    fn round_trips_every_bit_pattern() {
        let data: Vec<u8> = (0..65536u32)
            .flat_map(|v| (v as u16).to_le_bytes())
            .collect();
        let (exponents, rest) = split_bf16(&data);
        assert_eq!(join_bf16(&exponents, &rest), data);
    }
}
