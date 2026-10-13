//! Binary arithmetic coding with context mixing, for bases and quality scores.
//!
//! A symbol is coded as a sequence of bits. Each bit is predicted by several
//! context models, whose predictions are combined by a logistic mixer and then
//! coded with a range coder. The arithmetic is integer-only, so an archive
//! decodes the same way on every platform.

use crate::error::{Error, Result};

const TOP: u32 = 1 << 24;

/// Counter state: a 16-bit probability of a 1, and an 8-bit count that sets how
/// fast the counter adapts. Packed into a `u32`. The count lets a counter settle
/// near a frequency average, rather than forgetting at a fixed rate.
const COUNTER_INIT: u32 = 32768 << 8;
const COUNTER_LIMIT: u32 = 255;
/// Weight of each input before any learning, in units of 1/65536.
const WEIGHT_INIT: i32 = 1 << 14;
/// Learning rate of the mixer.
const MIXER_RATE: i32 = 7;

/// The logistic squash of the mixer: 1/(1+e^-x) in units of 1/4096, for x in
/// units of 1/256. It interpolates between 33 fixed points, so every platform agrees.
fn squash(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994,
        3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// The inverse of [`squash`], as a table over all 4096 probabilities.
fn stretch_table() -> &'static [i16; 4096] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[i16; 4096]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [2047i16; 4096];
        let mut next = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for slot in table.iter_mut().take(v + 1).skip(next) {
                *slot = x as i16;
            }
            next = next.max(v + 1);
        }
        table
    })
}

fn stretch(p: i32) -> i32 {
    i32::from(stretch_table()[p as usize])
}

/// Encodes bits with a range coder, LZMA style, with carry propagation.
pub(crate) struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl Encoder {
    pub(crate) fn new() -> Self {
        Self {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    /// Codes one bit, where `p1` is the probability of a 1 in units of 1/4096.
    pub(crate) fn encode_bit(&mut self, p1: u32, bit: u32) {
        let bound = (self.range >> 12) * p1;
        if bit != 0 {
            self.range = bound;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low as u32) >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = u64::from((self.low as u32) << 8);
    }

    /// Flushes the coder and returns the bytes.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

/// Decodes bits written by [`Encoder`].
pub(crate) struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    code: u32,
    range: u32,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        let mut decoder = Self {
            input,
            pos: 0,
            code: 0,
            range: u32::MAX,
        };
        // The first byte is always zero, from the encoder's initial cache.
        for _ in 0..5 {
            decoder.code = (decoder.code << 8) | u32::from(decoder.next_byte());
        }
        decoder
    }

    fn next_byte(&mut self) -> u8 {
        // Reading past the end yields zeros. `check` reports that afterwards.
        let byte = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        byte
    }

    /// Decodes one bit coded with probability `p1` of a 1, in units of 1/4096.
    pub(crate) fn decode_bit(&mut self, p1: u32) -> u32 {
        let bound = (self.range >> 12) * p1;
        let bit = if self.code < bound {
            self.range = bound;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        while self.range < TOP {
            self.code = (self.code << 8) | u32::from(self.next_byte());
            self.range <<= 8;
        }
        bit
    }

    /// Whether the decoder read past the end of its input. A complete stream never does.
    fn overran(&self) -> bool {
        self.pos > self.input.len()
    }
}

/// Fails if decoding read past the end of its input, which means the stream was cut short.
pub(crate) fn check(decoder: &Decoder<'_>) -> Result<()> {
    if decoder.overran() {
        return Err(Error::Corrupt("entropy-coded stream is truncated"));
    }
    Ok(())
}

/// A bit predictor that mixes several contexts.
///
/// Each input is a context value, set once per symbol with [`MixModel::set_contexts`].
/// The context picks a bucket of counters in the input's table, and the node of
/// the bit tree picks a counter within it. Hashing happens once per symbol, not
/// once per bit. The counters' predictions are mixed by a logistic mixer, whose
/// weights depend on the node.
pub(crate) struct MixModel {
    /// log2 of the number of counters per bucket: enough for every node.
    bucket_bits: u32,
    /// log2 of the number of buckets per input table.
    table_bits: u32,
    tables: Vec<Vec<u32>>,
    /// The first counter of each input's bucket for the current symbol.
    buckets: Vec<usize>,
    /// The counter used by each input for the current bit.
    slots: Vec<usize>,
    inputs: Vec<i32>,
    weights: Vec<i32>,
    node: usize,
    p: i32,
}

impl MixModel {
    /// `inputs` context models, each with a table of `2^bits` counters, and `nodes` bit-tree nodes.
    pub(crate) fn new(inputs: usize, bits: u32, nodes: usize) -> Self {
        let bucket_bits = nodes.max(1).next_power_of_two().trailing_zeros();
        let bits = bits.max(bucket_bits + 1);
        Self {
            bucket_bits,
            table_bits: bits - bucket_bits,
            tables: (0..inputs).map(|_| vec![COUNTER_INIT; 1 << bits]).collect(),
            buckets: vec![0; inputs],
            slots: vec![0; inputs],
            inputs: vec![0; inputs + 1],
            weights: vec![WEIGHT_INIT; nodes * (inputs + 1)],
            node: 0,
            p: 2048,
        }
    }

    /// The first counter of the bucket that `context` selects in input `input`.
    fn bucket(&self, input: usize, context: u64) -> usize {
        // Mix the input number into the context, so each input hashes differently.
        let mut h = context.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((input as u64) << 40);
        h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
        ((h >> (64 - self.table_bits)) as usize) << self.bucket_bits
    }

    /// Sets the context of each input for the next symbol.
    pub(crate) fn set_contexts(&mut self, contexts: &[u64]) {
        for (i, &context) in contexts.iter().enumerate() {
            self.buckets[i] = self.bucket(i, context);
        }
    }

    /// Learns a bit at `node` in another context of input `input`, without coding it.
    /// Used to teach the models what the other strand of the sequence implies.
    pub(crate) fn train(&mut self, input: usize, context: u64, node: usize, bit: u32) {
        let slot = self.bucket(input, context) + node;
        bump(&mut self.tables[input][slot], bit);
    }

    /// The probability that the bit at `node` is 1, in units of 1/4096.
    pub(crate) fn predict(&mut self, node: usize) -> u32 {
        self.node = node;
        for i in 0..self.tables.len() {
            let slot = self.buckets[i] + node;
            self.slots[i] = slot;
            self.inputs[i] = stretch((self.tables[i][slot] >> 12) as i32);
        }
        let n = self.tables.len();
        self.inputs[n] = 256;
        let w = &self.weights[node * (n + 1)..(node + 1) * (n + 1)];
        let dot: i64 = self
            .inputs
            .iter()
            .zip(w)
            .map(|(&x, &wi)| i64::from(x) * i64::from(wi))
            .sum();
        self.p = squash((dot >> 16).clamp(-2047, 2047) as i32).clamp(1, 4095);
        self.p as u32
    }

    /// Learns from the bit coded after the last [`MixModel::predict`].
    pub(crate) fn update(&mut self, bit: u32) {
        for (table, &slot) in self.tables.iter_mut().zip(&self.slots) {
            bump(&mut table[slot], bit);
        }
        let err = ((bit as i32) << 12).wrapping_sub(self.p) * MIXER_RATE;
        let n = self.tables.len();
        let w = &mut self.weights[self.node * (n + 1)..(self.node + 1) * (n + 1)];
        for (wi, &x) in w.iter_mut().zip(&self.inputs) {
            *wi += (x * err + 0x8000) >> 16;
        }
    }
}

/// Moves a counter towards `bit`, at a rate that slows as its count grows.
fn bump(counter: &mut u32, bit: u32) {
    let target = if bit != 0 { 65535 } else { 0 };
    let p = (*counter >> 8) as i32;
    let n = *counter & 255;
    let p = (p + (target - p) * 2 / (2 * n as i32 + 3)).clamp(1, 65535);
    let n = (n + 1).min(COUNTER_LIMIT);
    *counter = ((p as u32) << 8) | n;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes `symbols` (`bits` bits each) with one context per symbol, and decodes them back.
    fn round_trip(symbols: &[u32], bits: u32) -> Vec<u8> {
        let mut model = MixModel::new(2, 16, 1 << bits);
        let mut enc = Encoder::new();
        let mut prev = 0u64;
        for &s in symbols {
            model.set_contexts(&[prev, 0]);
            let mut node = 1usize;
            for j in (0..bits).rev() {
                let bit = (s >> j) & 1;
                enc.encode_bit(model.predict(node), bit);
                model.update(bit);
                node = (node << 1) | bit as usize;
            }
            prev = u64::from(s);
        }
        let bytes = enc.finish();

        let mut model = MixModel::new(2, 16, 1 << bits);
        let mut dec = Decoder::new(&bytes);
        let mut prev = 0u64;
        for &s in symbols {
            model.set_contexts(&[prev, 0]);
            let mut node = 1usize;
            let mut got = 0u32;
            for _ in 0..bits {
                let bit = dec.decode_bit(model.predict(node));
                model.update(bit);
                node = (node << 1) | bit as usize;
                got = (got << 1) | bit;
            }
            assert_eq!(got, s);
            prev = u64::from(got);
        }
        check(&dec).unwrap();
        bytes
    }

    #[test]
    fn round_trips_and_learns_a_repeating_pattern() {
        let symbols: Vec<u32> = (0..20_000)
            .map(|i| [3, 1, 4, 1, 5, 9, 2, 6][i % 8])
            .collect();
        let bytes = round_trip(&symbols, 4);
        // Eight symbols in a fixed cycle take far less than 4 bits each.
        assert!(bytes.len() < symbols.len() / 4);
    }

    #[test]
    fn carries_propagate_across_long_runs() {
        let symbols = vec![255u32; 20_000];
        round_trip(&symbols, 8);
    }

    #[test]
    fn squash_and_stretch_invert_each_other() {
        for p in [1, 100, 1000, 2048, 3000, 4000, 4095] {
            let back = squash(stretch(p));
            assert!((back - p).abs() <= p / 16 + 2, "p {p} came back as {back}");
        }
    }
}
