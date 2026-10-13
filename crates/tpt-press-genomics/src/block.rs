//! One block of records, split into column streams, and back.
//!
//! A block holds the records of one run of the input. Each column is kept apart
//! and compressed on its own:
//!
//! - `ids`: each header line, after `>` or `@`, followed by `\n`
//! - `lengths`: sequence length of each record, as varints
//! - `widths`: FASTA only. The line width of each record, as varints
//! - `case`: run lengths of upper and lower case over the block's sequence, as
//!   varints. The first run is upper case, and it may be empty
//! - `exceptions`: runs of bytes other than A, C, G and T, as triples of varints
//!   (gap, run length) and one byte. The gap counts from the end of the previous run
//! - `bases`: the A, C, G and T bases, either packed at 2 bits or arithmetic-coded
//!   with several context models mixed together, whichever is smaller. A mode byte
//!   says which
//! - `plus`: FASTQ only. One byte per record, 1 if the `+` line repeats the id
//! - `quality`: FASTQ only. The quality lines, arithmetic-coded with context models
//!   over the preceding symbols and position, or raw if that is smaller. A mode
//!   byte says which
//!
//! Splitting the sequence this way is lossless. Case is restored from the case
//! runs, and the other bytes come from the exceptions or the bases.

use crate::entropy::{self, Decoder, Encoder, MixModel};
use crate::error::{Error, Result};
use crate::format::{put_u64, put_varint, Bytes, Kind, MAX_STREAM_LEN};
use tpt_press_core::codec;

const BASES: [u8; 4] = *b"ACGT";
/// Bases stored at 2 bits each.
const MODE_PACKED: u8 = 0;
/// Bases or qualities, arithmetic-coded with context mixing.
const MODE_MODELLED: u8 = 1;
/// Quality stored as raw bytes.
const MODE_RAW: u8 = 0;
/// Blocks with fewer bases than this are always packed.
const MIN_MODELLED_BASES: usize = 16;
/// Count added per quality symbol seen.
/// Orders of the base context models: the number of previous bases each one uses.
const BASE_ORDERS: [u32; 7] = [2, 3, 4, 6, 8, 11, 16];

/// One record, as read from the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    /// The header line without its leading `>` or `@`.
    pub id: Vec<u8>,
    /// The bases, in the case they were written.
    pub seq: Vec<u8>,
    /// FASTA: the width every sequence line but the last has. FASTQ: 0.
    pub width: u64,
    /// FASTQ: whether the `+` line repeats the id.
    pub plus_repeats_id: bool,
    /// FASTQ: the quality line. Empty for FASTA.
    pub quality: Vec<u8>,
}

/// Records of one block, in column form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Columns {
    pub kind: Kind,
    pub ids: Vec<u8>,
    pub lengths: Vec<u64>,
    pub widths: Vec<u64>,
    pub plus_repeats: Vec<u8>,
    pub seq: Vec<u8>,
    pub quality: Vec<u8>,
}

impl Columns {
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            kind,
            ids: Vec::new(),
            lengths: Vec::new(),
            widths: Vec::new(),
            plus_repeats: Vec::new(),
            seq: Vec::new(),
            quality: Vec::new(),
        }
    }

    pub(crate) fn records(&self) -> usize {
        self.lengths.len()
    }

    pub(crate) fn push(&mut self, record: &Record) {
        self.ids.extend_from_slice(&record.id);
        self.ids.push(b'\n');
        self.lengths.push(record.seq.len() as u64);
        self.seq.extend_from_slice(&record.seq);
        match self.kind {
            Kind::Fasta => self.widths.push(record.width),
            Kind::Vcf => {}
            Kind::Fastq => {
                self.plus_repeats.push(u8::from(record.plus_repeats_id));
                self.quality.extend_from_slice(&record.quality);
            }
        }
    }

    /// Splits the block's records back out.
    pub(crate) fn rows(&self) -> Result<Vec<Row<'_>>> {
        let mut ids = self.ids.split(|&b| b == b'\n');
        let mut rows = Vec::with_capacity(self.records());
        let mut seq_at = 0usize;
        let mut qual_at = 0usize;
        for i in 0..self.records() {
            let id = ids.next().ok_or(Error::Corrupt("block has too few ids"))?;
            let len = self.lengths[i] as usize;
            let seq = self
                .seq
                .get(seq_at..seq_at + len)
                .ok_or(Error::Corrupt("record lengths do not match the sequence"))?;
            seq_at += len;
            let (quality, plus_repeats_id) = match self.kind {
                Kind::Fasta => (&[][..], false),
                Kind::Vcf => return Err(Error::Corrupt("a VCF file is not a sequence block")),
                Kind::Fastq => {
                    let q = self
                        .quality
                        .get(qual_at..qual_at + len)
                        .ok_or(Error::Corrupt("quality does not match the sequence"))?;
                    qual_at += len;
                    (q, self.plus_repeats[i] != 0)
                }
            };
            let width = match self.kind {
                Kind::Fasta => self.widths[i],
                Kind::Vcf => return Err(Error::Corrupt("a VCF file is not a sequence block")),
                Kind::Fastq => 0,
            };
            rows.push(Row {
                id,
                seq,
                width,
                plus_repeats_id,
                quality,
            });
        }
        Ok(rows)
    }
}

/// A record borrowed from a block's columns.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row<'a> {
    pub id: &'a [u8],
    pub seq: &'a [u8],
    pub width: u64,
    pub plus_repeats_id: bool,
    pub quality: &'a [u8],
}

/// Encodes a block's columns into a payload.
pub(crate) fn encode(columns: &Columns, level: i32) -> Result<Vec<u8>> {
    let split = split_sequence(&columns.seq);
    let bases = encode_bases(&split.codes);
    let quality = encode_quality(&columns.quality, &columns.lengths);

    let mut lengths = Vec::new();
    for &len in &columns.lengths {
        put_varint(&mut lengths, len);
    }
    let mut widths = Vec::new();
    for &width in &columns.widths {
        put_varint(&mut widths, width);
    }

    let mut out = Vec::new();
    put_u64(&mut out, columns.records() as u64);
    put_u64(&mut out, columns.seq.len() as u64);
    put_u64(&mut out, split.codes.len() as u64);
    let streams: [&[u8]; 8] = [
        &columns.ids,
        &lengths,
        &widths,
        &split.case_runs,
        &split.exceptions,
        &bases,
        &columns.plus_repeats,
        &quality,
    ];
    for stream in streams {
        put_stream(&mut out, stream, level)?;
    }
    Ok(out)
}

/// Decodes a block payload into its columns.
pub(crate) fn decode(payload: &[u8], kind: Kind) -> Result<Columns> {
    let mut b = Bytes::new(payload);
    let records = b.u64()?;
    let seq_len = b.u64()?;
    let base_count = b.u64()?;
    let ids = take_stream(&mut b)?;
    let lengths_raw = take_stream(&mut b)?;
    let widths_raw = take_stream(&mut b)?;
    let case_raw = take_stream(&mut b)?;
    let exceptions_raw = take_stream(&mut b)?;
    let bases_raw = take_stream(&mut b)?;
    let plus_repeats = take_stream(&mut b)?;
    let quality_raw = take_stream(&mut b)?;
    if !b.is_empty() {
        return Err(Error::Corrupt("block has trailing bytes"));
    }

    let records =
        usize::try_from(records).map_err(|_| Error::Corrupt("record count is too large"))?;
    if ids.iter().filter(|&&c| c == b'\n').count() != records {
        return Err(Error::Corrupt("block id count does not match its records"));
    }

    let lengths = read_varints(&lengths_raw, records)?;
    let total: u64 = lengths
        .iter()
        .try_fold(0u64, |acc, &l| acc.checked_add(l))
        .ok_or(Error::Corrupt("record lengths overflow"))?;
    if total != seq_len {
        return Err(Error::Corrupt(
            "record lengths do not match the sequence length",
        ));
    }

    let widths = match kind {
        Kind::Fasta => read_varints(&widths_raw, records)?,
        Kind::Vcf => return Err(Error::Corrupt("a VCF file is not a sequence block")),
        Kind::Fastq => {
            if !widths_raw.is_empty() {
                return Err(Error::Corrupt("FASTQ block has line widths"));
            }
            Vec::new()
        }
    };
    let quality = match kind {
        Kind::Vcf => return Err(Error::Corrupt("a VCF file is not a sequence block")),
        Kind::Fasta => {
            if !quality_raw.is_empty() || !plus_repeats.is_empty() {
                return Err(Error::Corrupt("FASTA block has FASTQ columns"));
            }
            Vec::new()
        }
        Kind::Fastq => {
            if plus_repeats.len() != records {
                return Err(Error::Corrupt("FASTQ columns do not match the records"));
            }
            decode_quality(&quality_raw, &lengths, seq_len)?
        }
    };

    let codes = decode_bases(
        &bases_raw,
        usize::try_from(base_count).map_err(|_| Error::Corrupt("base count is too large"))?,
    )?;
    let seq = join_sequence(seq_len, &case_raw, &exceptions_raw, &codes)?;
    Ok(Columns {
        kind,
        ids,
        lengths,
        widths,
        plus_repeats,
        seq,
        quality,
    })
}

/// Reads only the id column of a block. Used to find a record by name.
pub(crate) fn decode_ids(payload: &[u8]) -> Result<Vec<u8>> {
    let mut b = Bytes::new(payload);
    let _records = b.u64()?;
    let _seq_len = b.u64()?;
    let _base_count = b.u64()?;
    take_stream(&mut b)
}

/// The sequence streams, before compression. Bases are kept as codes 0 to 3.
struct SplitSequence {
    case_runs: Vec<u8>,
    exceptions: Vec<u8>,
    codes: Vec<u8>,
}

/// Splits a sequence into case runs, exception runs, and base codes.
fn split_sequence(seq: &[u8]) -> SplitSequence {
    let mut case_runs = Vec::new();
    let mut lower = false;
    let mut run = 0u64;
    for &b in seq {
        let is_lower = b.is_ascii_lowercase();
        if is_lower != lower {
            put_varint(&mut case_runs, run);
            run = 0;
            lower = is_lower;
        }
        run += 1;
    }
    if !seq.is_empty() {
        put_varint(&mut case_runs, run);
    }

    let mut exceptions = Vec::new();
    let mut codes = Vec::with_capacity(seq.len());
    let mut last_end = 0usize;
    let mut i = 0usize;
    while i < seq.len() {
        let upper = seq[i].to_ascii_uppercase();
        match base_code(upper) {
            Some(code) => {
                codes.push(code);
                i += 1;
            }
            None => {
                let mut j = i + 1;
                while j < seq.len() && seq[j].to_ascii_uppercase() == upper {
                    j += 1;
                }
                put_varint(&mut exceptions, (i - last_end) as u64);
                put_varint(&mut exceptions, (j - i) as u64);
                exceptions.push(upper);
                last_end = j;
                i = j;
            }
        }
    }

    SplitSequence {
        case_runs,
        exceptions,
        codes,
    }
}

/// Rebuilds a sequence from its streams.
fn join_sequence(
    seq_len: u64,
    case_raw: &[u8],
    exceptions_raw: &[u8],
    codes: &[u8],
) -> Result<Vec<u8>> {
    let seq_len = usize::try_from(seq_len).map_err(|_| Error::Corrupt("sequence is too large"))?;
    let mut out: Vec<u8> = Vec::with_capacity(seq_len);
    let mut next_base = 0usize;

    // Appends `count` bases from the code stream.
    let push_bases = |out: &mut Vec<u8>, count: usize, next_base: &mut usize| -> Result<()> {
        let end = *next_base + count;
        let slice = codes
            .get(*next_base..end)
            .ok_or(Error::Corrupt("ran out of bases"))?;
        out.extend(slice.iter().map(|&c| BASES[c as usize]));
        *next_base = end;
        Ok(())
    };

    let mut exceptions = Bytes::new(exceptions_raw);
    while !exceptions.is_empty() {
        let gap = exceptions.varint()? as usize;
        let run = exceptions.varint()? as usize;
        let byte = exceptions.u8()?;
        let before = out.len();
        let start = before
            .checked_add(gap)
            .filter(|&s| s <= seq_len)
            .ok_or(Error::Corrupt("exception offset runs past the sequence"))?;
        let end = start
            .checked_add(run)
            .filter(|&e| e <= seq_len)
            .ok_or(Error::Corrupt("exception runs past the sequence"))?;
        push_bases(&mut out, start - before, &mut next_base)?;
        out.extend(std::iter::repeat_n(byte, end - start));
    }
    let rest = seq_len - out.len();
    push_bases(&mut out, rest, &mut next_base)?;
    if next_base != codes.len() {
        return Err(Error::Corrupt("bases left over after the sequence"));
    }

    let mut case = Bytes::new(case_raw);
    let mut at = 0usize;
    let mut lower = false;
    while !case.is_empty() {
        let run =
            usize::try_from(case.varint()?).map_err(|_| Error::Corrupt("case run is too large"))?;
        let end = at
            .checked_add(run)
            .filter(|&e| e <= seq_len)
            .ok_or(Error::Corrupt("case runs past the sequence"))?;
        if lower {
            out[at..end].make_ascii_lowercase();
        }
        at = end;
        lower = !lower;
    }
    if at != seq_len {
        return Err(Error::Corrupt("case runs do not cover the sequence"));
    }
    Ok(out)
}

fn base_code(upper: u8) -> Option<u8> {
    match upper {
        b'A' => Some(0),
        b'C' => Some(1),
        b'G' => Some(2),
        b'T' => Some(3),
        _ => None,
    }
}

/// Packs base codes at 2 bits each, first base in the high bits of each byte.
fn pack_codes(codes: &[u8]) -> Vec<u8> {
    let mut packed = vec![0u8; codes.len().div_ceil(4)];
    for (i, &c) in codes.iter().enumerate() {
        packed[i / 4] |= c << (6 - 2 * (i % 4));
    }
    packed
}

/// Bits of the base models' hashed tables for a block of `count` bases.
fn base_table_bits(count: usize) -> u32 {
    (count.max(1).ilog2() + 2).clamp(12, 22)
}

/// The bases seen so far. `forward` holds the most recent 32 bases, two bits
/// each, newest in the low bits.
///
/// A reverse-complement update, which teaches each model what the other strand
/// implies, was tried and removed. It gained 0.07% on E. coli, and it doubled
/// the table traffic.
#[derive(Default)]
struct BaseHistory {
    forward: u64,
}

impl BaseHistory {
    /// The context of each base model.
    fn contexts(&self) -> [u64; BASE_ORDERS.len()] {
        BASE_ORDERS.map(|k| self.forward & ((1u64 << (2 * k)) - 1))
    }

    fn push(&mut self, c: u8) {
        self.forward = (self.forward << 2) | u64::from(c);
    }
}

/// Writes the base stream: a mode byte, then the packed or modelled bases.
/// The modelled form is used only when it is smaller.
fn encode_bases(codes: &[u8]) -> Vec<u8> {
    let packed = pack_codes(codes);
    let mut out = vec![MODE_PACKED];
    out.extend_from_slice(&packed);
    if codes.len() < MIN_MODELLED_BASES {
        return out;
    }

    let mut model = MixModel::new(BASE_ORDERS.len(), base_table_bits(codes.len()), 3);
    let mut enc = Encoder::new();
    let mut history = BaseHistory::default();
    for &c in codes {
        model.set_contexts(&history.contexts());
        let high = u32::from(c >> 1);
        enc.encode_bit(model.predict(0), high);
        model.update(high);
        let low = u32::from(c & 1);
        enc.encode_bit(model.predict(1 + high as usize), low);
        model.update(low);
        history.push(c);
    }
    let body = enc.finish();
    if body.len() < packed.len() {
        out = vec![MODE_MODELLED];
        out.extend_from_slice(&body);
    }
    out
}

/// Reads the base stream back into `count` codes.
fn decode_bases(raw: &[u8], count: usize) -> Result<Vec<u8>> {
    let (&mode, rest) = raw
        .split_first()
        .ok_or(Error::Corrupt("base stream is empty"))?;
    match mode {
        MODE_PACKED => {
            if rest.len() < count.div_ceil(4) {
                return Err(Error::Corrupt("packed bases are truncated"));
            }
            Ok((0..count)
                .map(|i| (rest[i / 4] >> (6 - 2 * (i % 4))) & 3)
                .collect())
        }
        MODE_MODELLED => {
            let mut model = MixModel::new(BASE_ORDERS.len(), base_table_bits(count), 3);
            let mut dec = Decoder::new(rest);
            let mut history = BaseHistory::default();
            let mut codes = Vec::with_capacity(count);
            for _ in 0..count {
                model.set_contexts(&history.contexts());
                let high = dec.decode_bit(model.predict(0));
                model.update(high);
                let low = dec.decode_bit(model.predict(1 + high as usize));
                model.update(low);
                let code = (high * 2 + low) as u8;
                codes.push(code);
                history.push(code);
            }
            entropy::check(&dec)?;
            Ok(codes)
        }
        _ => Err(Error::Corrupt("unknown base stream mode")),
    }
}

/// Bits needed to code an alphabet index, so `n` symbols take `quality_bits(n)` bits each.
fn quality_bits(n: usize) -> u32 {
    if n <= 1 {
        0
    } else {
        (n - 1).ilog2() + 1
    }
}

/// Number of context models for quality scores.
const QUALITY_INPUTS: usize = 4;

/// The contexts for the quality symbol at `pos` in a record, given the three
/// symbols before it in that record (zero before the record starts).
fn quality_contexts(prev: [u64; 3], pos: usize) -> [u64; QUALITY_INPUTS] {
    let [p1, p2, p3] = prev;
    [
        p1,
        p1 | (p2 << 8),
        p1 | (p2 << 8) | (p3 << 16),
        (((pos / 4) as u64) << 8) | p1,
    ]
}

/// Table size, in bits, for quality models over `symbols` quality bytes.
fn quality_table_bits(symbols: usize) -> u32 {
    (symbols.max(1).ilog2() + 2).clamp(12, 22)
}

/// Writes the quality stream: a mode byte, then either raw bytes, or the
/// alphabet and the modelled symbols. The modelled form is used only when smaller.
fn encode_quality(quality: &[u8], lengths: &[u64]) -> Vec<u8> {
    // No qualities (FASTA, or only empty FASTQ records) means no stream at all.
    if quality.is_empty() {
        return Vec::new();
    }
    let mut out = vec![MODE_RAW];
    out.extend_from_slice(quality);

    let mut present = [false; 256];
    for &q in quality {
        present[q as usize] = true;
    }
    let alphabet: Vec<u8> = (0..=255u8).filter(|&b| present[b as usize]).collect();
    let mut index = [0usize; 256];
    for (i, &b) in alphabet.iter().enumerate() {
        index[b as usize] = i;
    }
    let n = alphabet.len();
    let bits = quality_bits(n);
    let mut model = MixModel::new(QUALITY_INPUTS, quality_table_bits(quality.len()), 1 << bits);
    let mut enc = Encoder::new();
    let mut at = 0usize;
    for &len in lengths {
        // Each record starts its own contexts.
        let mut prev = [0u64; 3];
        for pos in 0..len as usize {
            let s = index[quality[at] as usize];
            model.set_contexts(&quality_contexts(prev, pos));
            let mut node = 1usize;
            for j in (0..bits).rev() {
                let bit = ((s >> j) & 1) as u32;
                enc.encode_bit(model.predict(node), bit);
                model.update(bit);
                node = (node << 1) | bit as usize;
            }
            prev = [s as u64, prev[0], prev[1]];
            at += 1;
        }
    }
    let body = enc.finish();

    if 3 + alphabet.len() + body.len() < out.len() {
        out = vec![MODE_MODELLED];
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&alphabet);
        out.extend_from_slice(&body);
    }
    out
}

/// Reads the quality stream back to `total` bytes.
fn decode_quality(raw: &[u8], lengths: &[u64], total: u64) -> Result<Vec<u8>> {
    let total = usize::try_from(total).map_err(|_| Error::Corrupt("quality is too large"))?;
    if raw.is_empty() {
        return if total == 0 {
            Ok(Vec::new())
        } else {
            Err(Error::Corrupt("quality stream is missing"))
        };
    }
    let (&mode, rest) = raw
        .split_first()
        .ok_or(Error::Corrupt("quality stream is empty"))?;
    match mode {
        MODE_RAW => {
            if rest.len() != total {
                return Err(Error::Corrupt("quality does not match the sequence"));
            }
            Ok(rest.to_vec())
        }
        MODE_MODELLED => {
            let mut b = Bytes::new(rest);
            let n = u16::from_le_bytes([b.u8()?, b.u8()?]) as usize;
            if n == 0 {
                return Err(Error::Corrupt("quality alphabet is empty"));
            }
            let alphabet = b.take(n)?.to_vec();
            let body = rest
                .get(2 + n..)
                .ok_or(Error::Corrupt("quality alphabet is truncated"))?;
            let bits = quality_bits(n);
            let mut model = MixModel::new(QUALITY_INPUTS, quality_table_bits(total), 1 << bits);
            let mut dec = Decoder::new(body);
            let mut out = Vec::with_capacity(total);
            for &len in lengths {
                let mut prev = [0u64; 3];
                for pos in 0..len as usize {
                    model.set_contexts(&quality_contexts(prev, pos));
                    let mut node = 1usize;
                    for _ in 0..bits {
                        let bit = dec.decode_bit(model.predict(node));
                        model.update(bit);
                        node = (node << 1) | bit as usize;
                    }
                    let s = node - (1 << bits);
                    out.push(
                        *alphabet
                            .get(s)
                            .ok_or(Error::Corrupt("quality symbol is out of range"))?,
                    );
                    prev = [s as u64, prev[0], prev[1]];
                }
            }
            entropy::check(&dec)?;
            if out.len() != total {
                return Err(Error::Corrupt("quality does not match the sequence"));
            }
            Ok(out)
        }
        _ => Err(Error::Corrupt("unknown quality stream mode")),
    }
}

/// Writes one stream: its raw length, its compressed length, and the bytes.
/// An empty stream is stored as two zeros.
pub(crate) fn put_stream(out: &mut Vec<u8>, raw: &[u8], level: i32) -> Result<()> {
    put_u64(out, raw.len() as u64);
    if raw.is_empty() {
        put_u64(out, 0);
        return Ok(());
    }
    let compressed = codec::compress(raw, level)?;
    put_u64(out, compressed.len() as u64);
    out.extend_from_slice(&compressed);
    Ok(())
}

pub(crate) fn take_stream(b: &mut Bytes<'_>) -> Result<Vec<u8>> {
    let raw_len = b.u64()?;
    let comp_len = b.u64()?;
    if raw_len > MAX_STREAM_LEN {
        return Err(Error::Corrupt("stream is too large"));
    }
    if raw_len == 0 {
        if comp_len != 0 {
            return Err(Error::Corrupt("empty stream has data"));
        }
        return Ok(Vec::new());
    }
    let comp_len = usize::try_from(comp_len).map_err(|_| Error::Corrupt("stream is too large"))?;
    let compressed = b.take(comp_len)?;
    let raw = codec::decompress(compressed, raw_len as usize)?;
    if raw.len() as u64 != raw_len {
        return Err(Error::Corrupt("stream length does not match its header"));
    }
    Ok(raw)
}

/// Reads exactly `count` varints from `raw`, and checks that nothing is left over.
fn read_varints(raw: &[u8], count: usize) -> Result<Vec<u64>> {
    let mut b = Bytes::new(raw);
    let mut values = Vec::with_capacity(count);
    while !b.is_empty() {
        values.push(b.varint()?);
    }
    if values.len() != count {
        return Err(Error::Corrupt("varint count does not match the records"));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modelled_bases_are_chosen_for_repetitive_sequence() {
        let codes: Vec<u8> = (0..20_000).map(|i| ((i * i) % 3) as u8 & 3).collect();
        let encoded = encode_bases(&codes);
        assert_eq!(encoded[0], MODE_MODELLED);
        assert_eq!(decode_bases(&encoded, codes.len()).unwrap(), codes);
    }

    #[test]
    fn short_base_streams_stay_packed_and_round_trip() {
        for n in [0usize, 1, 5, 15, 16, 17] {
            let codes: Vec<u8> = (0..n).map(|i| (i % 4) as u8).collect();
            let encoded = encode_bases(&codes);
            assert_eq!(decode_bases(&encoded, n).unwrap(), codes, "n = {n}");
        }
    }

    #[test]
    fn quality_round_trips_in_both_modes() {
        let lengths = vec![50u64, 0, 30];
        let qual: Vec<u8> = (0..80).map(|i| b'!' + ((i * 7) % 40) as u8).collect();
        let encoded = encode_quality(&qual, &lengths);
        assert_eq!(decode_quality(&encoded, &lengths, 80).unwrap(), qual);

        // Random-looking quality with nothing to model stays raw.
        let noisy: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(151)).collect();
        let encoded = encode_quality(&noisy, &[16]);
        assert_eq!(decode_quality(&encoded, &[16], 16).unwrap(), noisy);
    }
}
