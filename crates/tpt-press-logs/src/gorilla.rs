//! Gorilla-style codecs for time series (Pelkonen et al., "Gorilla: A Fast,
//! Scalable, In-Memory Time Series Database", VLDB 2015).
//!
//! Timestamps are stored as delta-of-delta: a regular interval costs one bit per
//! point. Floats are stored as the XOR with the previous value: a value that
//! changes little has few meaningful bits. Both are bit-packed, and the element
//! count is not stored, so callers record it alongside the bytes.
//!
//! The timestamp encoding is used by the logs format for timestamp columns
//! whose cells are canonical (see `block.rs`). The float encoding is not wired
//! in, and there is no query by series or label yet.

use crate::error::{Error, Result};

/// Encodes timestamps, in any order, as delta-of-delta bits.
pub fn encode_timestamps(times: &[i64]) -> Vec<u8> {
    let mut w = BitWriter::default();
    let Some((&first, rest)) = times.split_first() else {
        return Vec::new();
    };
    w.push_bits(first as u64, 64);
    let Some(&second) = rest.first() else {
        return w.into_bytes();
    };
    let mut prev_delta = second.wrapping_sub(first);
    w.push_bits(prev_delta as u64, 64);

    let mut prev = second;
    for &t in &rest[1..] {
        let delta = t.wrapping_sub(prev);
        let dod = delta.wrapping_sub(prev_delta);
        match dod {
            0 => w.push(false),
            -64..=63 => {
                w.push_bits(0b10, 2);
                w.push_bits(dod as u64, 7);
            }
            -256..=255 => {
                w.push_bits(0b110, 3);
                w.push_bits(dod as u64, 9);
            }
            -2048..=2047 => {
                w.push_bits(0b1110, 4);
                w.push_bits(dod as u64, 12);
            }
            _ => {
                w.push_bits(0b1111, 4);
                w.push_bits(dod as u64, 64);
            }
        }
        prev_delta = delta;
        prev = t;
    }
    w.into_bytes()
}

/// Decodes `count` timestamps written by [`encode_timestamps`].
pub fn decode_timestamps(data: &[u8], count: usize) -> Result<Vec<i64>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut r = BitReader::new(data);
    let mut out = Vec::with_capacity(count);
    let first = r.bits(64)? as i64;
    out.push(first);
    if count == 1 {
        return Ok(out);
    }
    let mut prev_delta = r.bits(64)? as i64;
    let mut prev = first.wrapping_add(prev_delta);
    out.push(prev);

    while out.len() < count {
        let dod = if !r.bit()? {
            0
        } else if !r.bit()? {
            signed(r.bits(7)?, 7)
        } else if !r.bit()? {
            signed(r.bits(9)?, 9)
        } else if !r.bit()? {
            signed(r.bits(12)?, 12)
        } else {
            r.bits(64)? as i64
        };
        let delta = prev_delta.wrapping_add(dod);
        prev = prev.wrapping_add(delta);
        out.push(prev);
        prev_delta = delta;
    }
    Ok(out)
}

/// Encodes floats as the XOR with the previous value. The bits are kept exact,
/// so NaN, infinities and signed zeros round-trip.
pub fn encode_floats(values: &[f64]) -> Vec<u8> {
    let mut w = BitWriter::default();
    let Some((&first, rest)) = values.split_first() else {
        return Vec::new();
    };
    let mut prev = first.to_bits();
    w.push_bits(prev, 64);

    // The leading and trailing zeros of the last changed run. A new value that
    // fits inside them reuses the window and skips the two five- and six-bit
    // fields.
    let mut window: Option<(u32, u32)> = None;
    for &value in rest {
        let bits = value.to_bits();
        let xor = bits ^ prev;
        prev = bits;
        if xor == 0 {
            w.push(false);
            continue;
        }
        w.push(true);
        let leading = xor.leading_zeros().min(31);
        let trailing = xor.trailing_zeros();
        match window {
            Some((wl, wt)) if leading >= wl && trailing >= wt => {
                w.push(false);
                let len = 64 - wl - wt;
                w.push_bits(xor >> wt, len);
            }
            _ => {
                w.push(true);
                let len = 64 - leading - trailing;
                w.push_bits(u64::from(leading), 5);
                w.push_bits(u64::from(len - 1), 6);
                w.push_bits(xor >> trailing, len);
                window = Some((leading, trailing));
            }
        }
    }
    w.into_bytes()
}

/// Decodes `count` floats written by [`encode_floats`].
pub fn decode_floats(data: &[u8], count: usize) -> Result<Vec<f64>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut r = BitReader::new(data);
    let mut prev = r.bits(64)?;
    let mut out = Vec::with_capacity(count);
    out.push(f64::from_bits(prev));

    let mut window: Option<(u32, u32)> = None;
    while out.len() < count {
        if !r.bit()? {
            out.push(f64::from_bits(prev));
            continue;
        }
        let xor = if !r.bit()? {
            let Some((wl, wt)) = window else {
                return Err(Error::Corrupt("float reuses a window that was never set"));
            };
            let len = 64 - wl - wt;
            r.bits(len)? << wt
        } else {
            let leading = r.bits(5)? as u32;
            let len = r.bits(6)? as u32 + 1;
            if leading + len > 64 {
                return Err(Error::Corrupt("float window is out of range"));
            }
            let trailing = 64 - leading - len;
            window = Some((leading, trailing));
            r.bits(len)? << trailing
        };
        prev ^= xor;
        out.push(f64::from_bits(prev));
    }
    Ok(out)
}

/// Sign-extends a `width`-bit two's-complement value.
fn signed(value: u64, width: u32) -> i64 {
    let shift = 64 - width;
    ((value << shift) as i64) >> shift
}

/// Writes bits, most significant first.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    /// Bits used in the last byte, from 0 to 7. Zero means a new byte is due.
    used: u32,
}

impl BitWriter {
    fn push(&mut self, bit: bool) {
        if self.used == 0 {
            self.out.push(0);
        }
        if bit {
            if let Some(last) = self.out.last_mut() {
                *last |= 0x80 >> self.used;
            }
        }
        self.used = (self.used + 1) % 8;
    }

    /// Writes the low `n` bits of `value`, most significant first.
    fn push_bits(&mut self, value: u64, n: u32) {
        for i in (0..n).rev() {
            self.push((value >> i) & 1 == 1);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.out
    }
}

/// Reads bits written by [`BitWriter`]. Reading past the end is an error.
struct BitReader<'a> {
    data: &'a [u8],
    /// The next bit to read, counted from the start.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<bool> {
        let byte = *self.data.get(self.pos / 8).ok_or(Error::Truncated)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1 == 1;
        self.pos += 1;
        Ok(bit)
    }

    fn bits(&mut self, n: u32) -> Result<u64> {
        let mut value = 0u64;
        for _ in 0..n {
            value = (value << 1) | u64::from(self.bit()?);
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_times(times: &[i64]) {
        let encoded = encode_timestamps(times);
        assert_eq!(decode_timestamps(&encoded, times.len()).unwrap(), times);
    }

    fn round_trip_floats(values: &[f64]) {
        let encoded = encode_floats(values);
        let decoded = decode_floats(&encoded, values.len()).unwrap();
        // Compare bits, so that NaN payloads and signed zeros are checked too.
        let a: Vec<u64> = decoded.iter().map(|v| v.to_bits()).collect();
        let b: Vec<u64> = values.iter().map(|v| v.to_bits()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn timestamps_round_trip_for_every_delta_size() {
        round_trip_times(&[]);
        round_trip_times(&[42]);
        round_trip_times(&[42, 99]);
        // Deltas of each control-code width: 0, 7, 9, 12 and 64 bits of delta-of-delta.
        let mut t = 1_700_000_000_000i64;
        let mut times = vec![t];
        for dod in [
            0i64,
            50,
            -60,
            200,
            -250,
            1500,
            -2000,
            100_000,
            -7,
            0,
            i64::MAX / 3,
        ] {
            let delta = 1000 + dod;
            t = t.wrapping_add(delta);
            times.push(t);
        }
        round_trip_times(&times);
    }

    #[test]
    fn extreme_timestamps_round_trip() {
        round_trip_times(&[i64::MIN, i64::MAX, 0, i64::MIN, i64::MAX]);
    }

    #[test]
    fn a_regular_interval_costs_one_bit_per_point() {
        let times: Vec<i64> = (0..10_000).map(|i| 1_000_000 + i * 15).collect();
        let encoded = encode_timestamps(&times);
        // Two 64-bit headers, then one bit for each point whose spacing repeats.
        assert!(
            encoded.len() < 16 + 10_000 / 8 + 2,
            "{} bytes",
            encoded.len()
        );
        round_trip_times(&times);
    }

    #[test]
    fn floats_round_trip_including_special_values() {
        round_trip_floats(&[]);
        round_trip_floats(&[1.5]);
        round_trip_floats(&[
            0.0,
            -0.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::MAX,
            1.0,
            1.0,
            1.0000000001,
            -3.25,
        ]);
    }

    #[test]
    fn slowly_changing_floats_are_small() {
        // A sensor reading to 0.1 degree, which changes slowly and repeats often.
        let values: Vec<f64> = (0..10_000)
            .map(|i| ((21.5 + (i as f64 * 0.01).sin() * 0.25) * 10.0).round() / 10.0)
            .collect();
        let encoded = encode_floats(&values);
        assert!(
            encoded.len() < values.len() * 8 / 2,
            "{} bytes",
            encoded.len()
        );
        round_trip_floats(&values);
    }

    #[test]
    fn decoding_damaged_or_short_data_fails_cleanly() {
        let times: Vec<i64> = (0..100).map(|i| i * 7).collect();
        let encoded = encode_timestamps(&times);
        assert!(decode_timestamps(&encoded[..encoded.len() / 2], times.len()).is_err());

        let values: Vec<f64> = (0..100).map(|i| i as f64 * 0.5).collect();
        let encoded = encode_floats(&values);
        assert!(decode_floats(&encoded[..encoded.len() / 2], values.len()).is_err());

        // A window reuse before any window was set is refused.
        let mut w = BitWriter::default();
        w.push_bits(0, 64);
        w.push(true);
        w.push(false);
        assert!(matches!(
            decode_floats(&w.into_bytes(), 2),
            Err(Error::Corrupt(_))
        ));
    }
}
