//! Lossless transforms applied to a chunk before zstd.
//!
//! Each element is predicted from an earlier one, and the residual is stored.
//! The residual is then grouped by byte position, so zstd sees the repeated
//! zero bytes together.
//!
//! - [`Filter::Shuffle`] groups the bytes of each element by position, so all
//!   the sign and exponent bytes of floats sit together, where zstd finds them.
//! - [`Filter::XorShuffle`] XORs each element with the one before it, then
//!   shuffles. Smooth floats have many equal high bytes, which become zeros.
//! - [`Filter::Delta`] subtracts the element before it, modulo the element size,
//!   then shuffles. This suits integers that change slowly.
//! - [`Filter::UpShuffle`] and [`Filter::DeltaUp`] do the same against the element
//!   a row above, which suits images and fields where neighbouring rows are alike.
//!
//! All are exact inverses, so the choice never changes the data. The writer
//! keeps whichever filter compresses a chunk best.

use crate::error::{Error, Result};

/// A transform applied to a chunk's bytes before compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Compress the bytes as they are.
    None,
    /// Group the bytes of each element by position.
    Shuffle,
    /// XOR each element with the previous one, then shuffle.
    XorShuffle,
    /// XOR each element with the one a row above, then shuffle.
    UpShuffle,
    /// Subtract the previous element, then shuffle.
    Delta,
    /// Subtract the element a row above, then shuffle.
    DeltaUp,
    /// Subtract the Lorenzo prediction, the sum of the neighbours along each axis
    /// with inclusion and exclusion, then shuffle. For two axes that is
    /// `left + up - upper_left`. Chunks of more than four axes are left unpredicted.
    Lorenzo,
}

impl Filter {
    /// Every filter, in code order.
    pub const ALL: [Filter; 7] = [
        Filter::None,
        Filter::Shuffle,
        Filter::XorShuffle,
        Filter::UpShuffle,
        Filter::Delta,
        Filter::DeltaUp,
        Filter::Lorenzo,
    ];

    /// The code stored in the index.
    pub fn code(self) -> u8 {
        match self {
            Filter::None => 0,
            Filter::Shuffle => 1,
            Filter::XorShuffle => 2,
            Filter::UpShuffle => 3,
            Filter::Delta => 4,
            Filter::DeltaUp => 5,
            Filter::Lorenzo => 6,
        }
    }

    /// Reads a filter code from the index.
    pub fn from_code(code: u8) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.code() == code)
            .ok_or(Error::Corrupt("unknown chunk filter"))
    }

    /// A short name, for output.
    pub fn name(self) -> &'static str {
        match self {
            Filter::None => "none",
            Filter::Shuffle => "shuffle",
            Filter::XorShuffle => "xor+shuffle",
            Filter::UpShuffle => "xor-up+shuffle",
            Filter::Delta => "delta+shuffle",
            Filter::DeltaUp => "delta-up+shuffle",
            Filter::Lorenzo => "lorenzo+shuffle",
        }
    }

    /// Applies the filter to `data`, whose elements are `elem` bytes each.
    /// `extent` is the chunk's shape. The `Up` filters step back one row, which
    /// is the last axis, and Lorenzo uses every axis.
    pub fn encode(self, data: &[u8], elem: usize, extent: &[u64]) -> Vec<u8> {
        let row = row_length(extent);
        match self {
            Filter::None => data.to_vec(),
            Filter::Shuffle => shuffle(data, elem),
            Filter::XorShuffle => shuffle(&xor_delta(data, elem, 1), elem),
            Filter::UpShuffle => shuffle(&xor_delta(data, elem, row), elem),
            Filter::Delta => shuffle(&sub_delta(data, elem, 1), elem),
            Filter::DeltaUp => shuffle(&sub_delta(data, elem, row), elem),
            Filter::Lorenzo => shuffle(&lorenzo(data, elem, extent, true), elem),
        }
    }

    /// Undoes [`Filter::encode`], with the same `extent`.
    pub fn decode(self, data: &[u8], elem: usize, extent: &[u64]) -> Vec<u8> {
        let row = row_length(extent);
        match self {
            Filter::None => data.to_vec(),
            Filter::Shuffle => unshuffle(data, elem),
            Filter::XorShuffle => xor_undelta(&unshuffle(data, elem), elem, 1),
            Filter::UpShuffle => xor_undelta(&unshuffle(data, elem), elem, row),
            Filter::Delta => sub_undelta(&unshuffle(data, elem), elem, 1),
            Filter::DeltaUp => sub_undelta(&unshuffle(data, elem), elem, row),
            Filter::Lorenzo => lorenzo(&unshuffle(data, elem), elem, extent, false),
        }
    }
}

/// Elements in one row, the last axis. A chunk with no axes counts as one element.
fn row_length(extent: &[u64]) -> usize {
    extent.last().copied().unwrap_or(1).max(1) as usize
}

/// Largest number of axes Lorenzo predicts over. It sums up to `2^axes` terms.
const LORENZO_MAX_AXES: usize = 4;

/// The Lorenzo predictor over a chunk of shape `extent`, in C order.
///
/// Forward: each element minus its prediction, computed from the original
/// values. Inverse: each residual plus the prediction, computed from values
/// already restored, which come earlier in C order. A neighbour outside the
/// chunk counts as zero.
fn lorenzo(data: &[u8], elem: usize, extent: &[u64], forward: bool) -> Vec<u8> {
    let axes = extent.len();
    if axes == 0 || axes > LORENZO_MAX_AXES {
        return data.to_vec();
    }
    let n = data.len() / elem;
    let extent: Vec<usize> = extent.iter().map(|&e| e as usize).collect();
    // Element distance between neighbours along each axis.
    let mut strides = vec![1usize; axes];
    for k in (0..axes - 1).rev() {
        strides[k] = strides[k + 1] * extent[k + 1];
    }

    let source: Vec<u64> = (0..n).map(|i| read_elem(data, elem, i)).collect();
    let mut values = source.clone();
    for i in 0..n {
        let mut prediction = 0u64;
        // Each non-empty set of axes adds or removes the neighbour that steps back along them.
        for mask in 1usize..(1 << axes) {
            let mut offset = 0usize;
            let mut inside = true;
            for k in 0..axes {
                if mask >> k & 1 == 1 {
                    if (i / strides[k]) % extent[k] == 0 {
                        inside = false;
                        break;
                    }
                    offset += strides[k];
                }
            }
            if !inside {
                continue;
            }
            let neighbour = if forward {
                source[i - offset]
            } else {
                values[i - offset]
            };
            if mask.count_ones() % 2 == 1 {
                prediction = prediction.wrapping_add(neighbour);
            } else {
                prediction = prediction.wrapping_sub(neighbour);
            }
        }
        values[i] = if forward {
            source[i].wrapping_sub(prediction)
        } else {
            values[i].wrapping_add(prediction)
        };
    }

    let mut out = data.to_vec();
    for (i, &value) in values.iter().enumerate() {
        write_elem(&mut out, elem, i, value);
    }
    out
}

/// Byte `j` of element `i` moves to `j * n + i`, where `n` is the element count.
fn shuffle(data: &[u8], elem: usize) -> Vec<u8> {
    let n = data.len() / elem;
    let mut out = vec![0u8; data.len()];
    for i in 0..n {
        for j in 0..elem {
            out[j * n + i] = data[i * elem + j];
        }
    }
    out
}

fn unshuffle(data: &[u8], elem: usize) -> Vec<u8> {
    let n = data.len() / elem;
    let mut out = vec![0u8; data.len()];
    for i in 0..n {
        for j in 0..elem {
            out[i * elem + j] = data[j * n + i];
        }
    }
    out
}

/// Element `i` becomes element `i` XOR element `i - stride`. The first `stride`
/// elements are kept.
fn xor_delta(data: &[u8], elem: usize, stride: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    let n = data.len() / elem;
    for i in (stride..n).rev() {
        for j in 0..elem {
            out[i * elem + j] ^= data[(i - stride) * elem + j];
        }
    }
    out
}

/// Inverts [`xor_delta`]. Earlier elements are already restored when each one is read.
fn xor_undelta(data: &[u8], elem: usize, stride: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    let n = data.len() / elem;
    for i in stride..n {
        for j in 0..elem {
            out[i * elem + j] ^= out[(i - stride) * elem + j];
        }
    }
    out
}

/// Reads element `i` as a little-endian integer of `elem` bytes.
fn read_elem(data: &[u8], elem: usize, i: usize) -> u64 {
    data[i * elem..(i + 1) * elem]
        .iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// Writes element `i` as a little-endian integer of `elem` bytes, keeping the low bytes.
fn write_elem(data: &mut [u8], elem: usize, i: usize, value: u64) {
    for (j, byte) in data[i * elem..(i + 1) * elem].iter_mut().enumerate() {
        *byte = (value >> (8 * j)) as u8;
    }
}

/// Element `i` becomes element `i` minus element `i - stride`, modulo `2^(8 * elem)`.
/// The first `stride` elements are kept.
fn sub_delta(data: &[u8], elem: usize, stride: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    let n = data.len() / elem;
    for i in (stride..n).rev() {
        let residual = read_elem(data, elem, i).wrapping_sub(read_elem(data, elem, i - stride));
        write_elem(&mut out, elem, i, residual);
    }
    out
}

/// Inverts [`sub_delta`]. Each element is restored from the one before it, already restored.
fn sub_undelta(data: &[u8], elem: usize, stride: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    let n = data.len() / elem;
    for i in stride..n {
        let value = read_elem(&out, elem, i).wrapping_add(read_elem(&out, elem, i - stride));
        write_elem(&mut out, elem, i, value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_inverts_exactly() {
        let data: Vec<u8> = (0..240u32).map(|i| (i * 37 % 251) as u8).collect();
        for elem in [1, 2, 4, 8] {
            let n = 240 / elem as u64;
            for row in [1u64, 3, 10] {
                let extent = [n / row, row];
                for filter in Filter::ALL {
                    let encoded = filter.encode(&data, elem, &extent);
                    assert_eq!(
                        filter.decode(&encoded, elem, &extent),
                        data,
                        "{filter:?} elem {elem} row {row}"
                    );
                }
            }
        }
    }

    #[test]
    fn lorenzo_inverts_on_three_axes_and_predicts_a_plane() {
        // A 2 x 3 x 5 chunk of 8-byte values that are exactly linear in the coordinates.
        let extent = [2u64, 3, 5];
        let values: Vec<u8> = (0..30u64)
            .flat_map(|i| {
                let (a, b, c) = (i / 15, (i / 5) % 3, i % 5);
                (100 + 7 * a + 3 * b + 2 * c).to_le_bytes()
            })
            .collect();
        let encoded = Filter::Lorenzo.encode(&values, 8, &extent);
        assert_eq!(Filter::Lorenzo.decode(&encoded, 8, &extent), values);
        // A linear field has zero Lorenzo residual wherever every neighbour exists,
        // which is every element with a coordinate of at least 1 on each axis.
        let residuals = lorenzo(&values, 8, &extent, true);
        for a in 1..2u64 {
            for b in 1..3u64 {
                for c in 1..5u64 {
                    let i = (a * 15 + b * 5 + c) as usize;
                    assert_eq!(read_elem(&residuals, 8, i), 0, "element {i}");
                }
            }
        }
    }

    #[test]
    fn lorenzo_leaves_more_than_four_axes_alone() {
        let data: Vec<u8> = (0..32u8).collect();
        let extent = [2u64; 5];
        assert_eq!(lorenzo(&data, 1, &extent, true), data);
    }

    #[test]
    fn xor_shuffle_zeroes_repeated_high_bytes() {
        // Eight equal f32 values. After XOR-delta the last seven are all zero
        // (28 bytes). The first value 1.5 has two zero bytes of its own.
        let data: Vec<u8> = (0..8).flat_map(|_| 1.5f32.to_le_bytes()).collect();
        let encoded = Filter::XorShuffle.encode(&data, 4, &[8]);
        assert_eq!(encoded.iter().filter(|&&b| b == 0).count(), 28 + 2);
    }

    #[test]
    fn up_filters_use_the_row_above() {
        // Two rows of three identical values each: the second row becomes zero.
        let row = [7u8, 7, 7];
        let data: Vec<u8> = [row, row].concat();
        assert_eq!(
            &Filter::UpShuffle.encode(&data, 1, &[2, 3])[3..],
            &[0, 0, 0]
        );
        assert_eq!(&Filter::DeltaUp.encode(&data, 1, &[2, 3])[3..], &[0, 0, 0]);
    }

    #[test]
    fn delta_wraps_within_the_element() {
        // 255 then 0 in one-byte elements: the residual wraps to 1.
        let encoded = sub_delta(&[255, 0], 1, 1);
        assert_eq!(encoded, vec![255, 1]);
        assert_eq!(sub_undelta(&encoded, 1, 1), vec![255, 0]);
    }

    #[test]
    fn codes_round_trip() {
        for filter in Filter::ALL {
            assert_eq!(Filter::from_code(filter.code()).unwrap(), filter);
        }
        assert!(Filter::from_code(9).is_err());
    }
}
