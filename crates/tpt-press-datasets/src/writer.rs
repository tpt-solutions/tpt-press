//! Writes samples into a `.tpds` archive, one sample at a time.

use std::collections::BTreeSet;
use std::io::Write;

use tpt_press_core::codec;

use crate::error::{Error, Result};
use crate::format::{self, Codec, MemberEntry, SampleEntry, HEADER_LEN, MAX_MEMBER_BYTES};

/// One named member of a sample, such as `jpg` or `cls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Member name. Must be non-empty and must not contain `/`.
    pub name: String,
    /// The member's bytes.
    pub data: Vec<u8>,
}

/// One training example: a key and its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The sample's key. Must be non-empty. Samples that share a key are still
    /// separate samples.
    pub key: String,
    /// The members. Names must be unique within the sample.
    pub members: Vec<Member>,
}

/// Counts from a completed write or import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Samples written.
    pub samples: u64,
    /// Members written.
    pub members: u64,
    /// Total member bytes before compression.
    pub input_bytes: u64,
    /// Size of the archive in bytes.
    pub archive_bytes: u64,
    /// Tar entries skipped because they are not members, such as files with no
    /// dot in their name. Only [`crate::import_tar`] sets this.
    pub skipped: u64,
}

/// Writes samples to an output in order.
pub struct Writer<W: Write> {
    out: W,
    offset: u64,
    level: i32,
    samples: Vec<SampleEntry>,
    stats: Stats,
}

impl<W: Write> Writer<W> {
    /// Starts an archive, writing its header. `level` is the zstd level, in `1..=22`.
    pub fn new(mut out: W, level: i32) -> Result<Self> {
        codec::validate_level(level)?;
        out.write_all(&format::header_bytes())?;
        Ok(Self {
            out,
            offset: HEADER_LEN,
            level,
            samples: Vec::new(),
            stats: Stats::default(),
        })
    }

    /// Appends one sample.
    pub fn push(&mut self, sample: Sample) -> Result<()> {
        validate(&sample)?;
        let mut members = Vec::with_capacity(sample.members.len());
        for member in sample.members {
            let raw_len = member.data.len() as u64;
            if raw_len > MAX_MEMBER_BYTES {
                return Err(Error::InvalidInput(format!(
                    "member {:?} of sample {:?} is larger than 1 GiB",
                    member.name, sample.key
                )));
            }
            let (stored, codec) = encode_member(&member.name, member.data, self.level)?;
            self.out.write_all(&stored)?;
            members.push(MemberEntry {
                name: member.name,
                offset: self.offset,
                stored_len: stored.len() as u32,
                raw_len: raw_len as u32,
                crc32: crc32fast::hash(&stored),
                codec,
            });
            self.offset += stored.len() as u64;
            self.stats.members += 1;
            self.stats.input_bytes += raw_len;
        }
        self.samples.push(SampleEntry {
            key: sample.key,
            members,
        });
        self.stats.samples += 1;
        Ok(())
    }

    /// Writes the index and footer, and returns the counts.
    pub fn finish(mut self) -> Result<Stats> {
        let footer = format::encode_index_and_footer(&self.samples, self.offset)?;
        self.out.write_all(&footer)?;
        self.out.flush()?;
        self.stats.archive_bytes = self.offset + footer.len() as u64;
        Ok(self.stats)
    }

    /// Adds to the count of skipped tar entries.
    pub(crate) fn note_skipped(&mut self) {
        self.stats.skipped += 1;
    }
}

/// Writes every sample from `samples` as one archive.
pub fn pack_samples<I, W>(samples: I, output: W, level: i32) -> Result<Stats>
where
    I: IntoIterator<Item = Sample>,
    W: Write,
{
    let mut writer = Writer::new(output, level)?;
    for sample in samples {
        writer.push(sample)?;
    }
    writer.finish()
}

fn validate(sample: &Sample) -> Result<()> {
    if sample.key.is_empty() {
        return Err(Error::InvalidInput("sample key is empty".to_owned()));
    }
    if sample.members.is_empty() {
        return Err(Error::InvalidInput(format!(
            "sample {:?} has no members",
            sample.key
        )));
    }
    let mut names = BTreeSet::new();
    for member in &sample.members {
        if member.name.is_empty() || member.name.contains('/') {
            return Err(Error::InvalidInput(format!(
                "member name {:?} in sample {:?} is empty or has a slash",
                member.name, sample.key
            )));
        }
        if !names.insert(member.name.as_str()) {
            return Err(Error::InvalidInput(format!(
                "sample {:?} has two members named {:?}",
                sample.key, member.name
            )));
        }
    }
    Ok(())
}

/// Stores a member as it is, or as zstd if that is smaller. Compressed media is
/// stored as it is, without trying zstd.
fn encode_member(name: &str, data: Vec<u8>, level: i32) -> Result<(Vec<u8>, Codec)> {
    if format::is_compressed_media(name) {
        return Ok((data, Codec::Stored));
    }
    let packed = codec::compress(&data, level)?;
    if packed.len() < data.len() {
        Ok((packed, Codec::Zstd))
    } else {
        Ok((data, Codec::Stored))
    }
}
