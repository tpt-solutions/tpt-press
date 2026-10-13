//! Reads genomics archives: unpacks them, summarises them, and fetches one record.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::block;
use crate::error::{map_io, Error, Result};
use crate::format::{self, Entry, Kind};

/// What an archive holds, from its index alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// The record format.
    pub kind: Kind,
    /// Number of records.
    pub records: u64,
    /// Sequence bases, summed over records.
    pub bases: u64,
    /// Number of blocks.
    pub blocks: u64,
    /// Size of the archive file, in bytes.
    pub archive_bytes: u64,
}

/// Counts from a completed [`fetch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchStats {
    /// Length of the whole record, in bases. A range is a part of it.
    pub record_length: u64,
    /// Blocks that were read. The name lookup points at the one block that
    /// holds the record, so this is 1 unless two records share a hash.
    pub blocks_read: u64,
}

/// Reads the archive's header and index, and returns its summary. No block is decoded.
pub fn inspect<R: Read + Seek>(mut input: R) -> Result<Summary> {
    let kind = format::read_header(&mut input)?;
    let index = format::read_index(&mut input)?;
    let archive_bytes = input.seek(SeekFrom::End(0)).map_err(map_io)?;
    Ok(Summary {
        kind,
        records: index.total_records,
        bases: index.total_bases,
        blocks: index.entries.len() as u64,
        archive_bytes,
    })
}

/// Writes the original FASTA or FASTQ back out, byte for byte.
pub fn unpack<R: Read + Seek, W: Write>(mut input: R, mut output: W) -> Result<Summary> {
    let kind = format::read_header(&mut input)?;
    let index = format::read_index(&mut input)?;
    for entry in &index.entries {
        let payload = read_payload(&mut input, entry)?;
        let columns = block::decode(&payload, kind)?;
        for row in columns.rows()? {
            write_record(
                &mut output,
                kind,
                row.id,
                row.seq,
                row.width,
                row.plus_repeats_id,
                row.quality,
            )?;
        }
    }
    output.flush()?;
    let archive_bytes = input.seek(SeekFrom::End(0)).map_err(map_io)?;
    Ok(Summary {
        kind,
        records: index.total_records,
        bases: index.total_bases,
        blocks: index.entries.len() as u64,
        archive_bytes,
    })
}

/// Writes one record whose identifier is `id`, or a part of it.
///
/// `range` is 1-based and inclusive, as in `samtools faidx`. `None` writes the
/// whole record. The record's header is written unchanged. The identifier is
/// matched against the first whitespace-delimited word of each header.
pub fn fetch<R: Read + Seek, W: Write>(
    mut input: R,
    id: &str,
    range: Option<(u64, u64)>,
    mut output: W,
) -> Result<FetchStats> {
    let kind = format::read_header(&mut input)?;
    let index = format::read_index(&mut input)?;
    let wanted = id.as_bytes();
    let hash = format::name_hash(wanted);
    let first = index.lookup.partition_point(|&(h, _)| h < hash);

    for (candidate, &(_, record)) in index.lookup[first..]
        .iter()
        .take_while(|&&(h, _)| h == hash)
        .enumerate()
    {
        // The block whose first record is the last one at or before `record`.
        let block_number = index.entries.partition_point(|e| e.first_record <= record) - 1;
        let entry = &index.entries[block_number];
        let payload = read_payload(&mut input, entry)?;
        let blocks_read = candidate as u64 + 1;

        let within = (record - entry.first_record) as usize;
        let ids = block::decode_ids(&payload)?;
        let Some(header) = ids.split(|&b| b == b'\n').nth(within) else {
            return Err(Error::Corrupt("name lookup points past its block"));
        };
        if name_of(header) != wanted {
            // A hash collision. Keep looking.
            continue;
        }

        let columns = block::decode(&payload, kind)?;
        let rows = columns.rows()?;
        let row = rows[within];
        let length = row.seq.len() as u64;
        let (seq, quality) = slice_range(row.seq, row.quality, range, length)?;
        write_record(
            &mut output,
            kind,
            row.id,
            seq,
            row.width,
            row.plus_repeats_id,
            quality,
        )?;
        output.flush()?;
        return Ok(FetchStats {
            record_length: length,
            blocks_read,
        });
    }
    Err(Error::NotFound(id.to_owned()))
}

/// The record's name: the header up to its first whitespace.
fn name_of(header: &[u8]) -> &[u8] {
    header
        .split(|b| b.is_ascii_whitespace())
        .next()
        .unwrap_or(&[])
}

/// Applies a 1-based inclusive range to the sequence and its quality.
fn slice_range<'a>(
    seq: &'a [u8],
    quality: &'a [u8],
    range: Option<(u64, u64)>,
    length: u64,
) -> Result<(&'a [u8], &'a [u8])> {
    let Some((start, end)) = range else {
        return Ok((seq, quality));
    };
    if start < 1 || start > end || end > length {
        return Err(Error::RangeOutOfBounds { start, end, length });
    }
    let (from, to) = ((start - 1) as usize, end as usize);
    let quality = if quality.is_empty() {
        quality
    } else {
        &quality[from..to]
    };
    Ok((&seq[from..to], quality))
}

/// Writes one record in the archive's format.
fn write_record<W: Write>(
    out: &mut W,
    kind: Kind,
    id: &[u8],
    seq: &[u8],
    width: u64,
    plus_repeats_id: bool,
    quality: &[u8],
) -> Result<()> {
    match kind {
        Kind::Vcf => return Err(Error::Corrupt("a VCF file is not written as records")),
        Kind::Fasta => {
            out.write_all(b">")?;
            out.write_all(id)?;
            out.write_all(b"\n")?;
            if !seq.is_empty() {
                if width == 0 {
                    return Err(Error::Corrupt("sequence has no line width"));
                }
                for line in seq.chunks(width as usize) {
                    out.write_all(line)?;
                    out.write_all(b"\n")?;
                }
            }
        }
        Kind::Fastq => {
            out.write_all(b"@")?;
            out.write_all(id)?;
            out.write_all(b"\n")?;
            out.write_all(seq)?;
            out.write_all(b"\n+")?;
            if plus_repeats_id {
                out.write_all(id)?;
            }
            out.write_all(b"\n")?;
            out.write_all(quality)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// Reads one block's payload and checks its CRC.
fn read_payload<R: Read + Seek>(input: &mut R, entry: &Entry) -> Result<Vec<u8>> {
    input.seek(SeekFrom::Start(entry.offset)).map_err(map_io)?;
    let mut payload = vec![0u8; entry.len as usize];
    input.read_exact(&mut payload).map_err(map_io)?;
    if crc32fast::hash(&payload) != entry.crc {
        return Err(Error::Corrupt("block checksum does not match"));
    }
    Ok(payload)
}
