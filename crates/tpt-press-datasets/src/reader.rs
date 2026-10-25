//! Reads samples from a `.tpds` archive by index.

use std::io::{Read, Seek, SeekFrom};

use crate::error::{map_read, Error, Result};
use crate::format::{self, Codec, MemberEntry, SampleEntry, FOOTER_LEN, HEADER_LEN};
use crate::writer::{Member, Sample};

/// An open dataset archive. Samples are read on request, so opening reads only
/// the header, the footer and the index.
pub struct Dataset<R> {
    input: R,
    samples: Vec<SampleEntry>,
}

impl<R: Read + Seek> Dataset<R> {
    /// Opens an archive. Checks the header, the footer and the index's checksum.
    pub fn open(mut input: R) -> Result<Self> {
        input.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_LEN as usize];
        input.read_exact(&mut header).map_err(map_read)?;
        format::check_header(&header)?;

        let file_len = input.seek(SeekFrom::End(0))?;
        if file_len < HEADER_LEN + FOOTER_LEN {
            return Err(Error::Truncated);
        }
        input.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
        let mut footer_bytes = [0u8; FOOTER_LEN as usize];
        input.read_exact(&mut footer_bytes).map_err(map_read)?;
        let footer = format::parse_footer(&footer_bytes)?;

        format::check_footer_location(&footer, file_len)?;

        input.seek(SeekFrom::Start(footer.index_offset))?;
        let mut stored = vec![0u8; footer.index_len as usize];
        input.read_exact(&mut stored).map_err(map_read)?;
        let samples = format::read_index(&stored, &footer)?;
        Ok(Self { input, samples })
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the archive has no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The key of sample `index`.
    pub fn key(&self, index: usize) -> Result<&str> {
        Ok(&self.entry(index)?.key)
    }

    /// The member names of sample `index`, in the order they were written.
    pub fn member_names(&self, index: usize) -> Result<Vec<&str>> {
        Ok(self
            .entry(index)?
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect())
    }

    /// How the named member of sample `index` is stored.
    pub fn member_codec(&self, index: usize, name: &str) -> Result<Codec> {
        let entry = self.entry(index)?;
        entry
            .members
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.codec)
            .ok_or_else(|| Error::MissingMember {
                sample: entry.key.clone(),
                member: name.to_owned(),
            })
    }

    /// Reads sample `index` with all its members.
    pub fn sample(&mut self, index: usize) -> Result<Sample> {
        let entry = self.entry(index)?.clone();
        let mut members = Vec::with_capacity(entry.members.len());
        for m in &entry.members {
            members.push(Member {
                name: m.name.clone(),
                data: self.read_member(&entry.key, m)?,
            });
        }
        Ok(Sample {
            key: entry.key,
            members,
        })
    }

    /// Reads one member of sample `index` by name, without reading the others.
    pub fn member(&mut self, index: usize, name: &str) -> Result<Vec<u8>> {
        let entry = self.entry(index)?.clone();
        let m = entry
            .members
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| Error::MissingMember {
                sample: entry.key.clone(),
                member: name.to_owned(),
            })?;
        self.read_member(&entry.key, m)
    }

    /// Reads and checks every member in the archive.
    pub fn verify(&mut self) -> Result<()> {
        for index in 0..self.samples.len() {
            self.sample(index)?;
        }
        Ok(())
    }

    fn entry(&self, index: usize) -> Result<&SampleEntry> {
        self.samples.get(index).ok_or(Error::OutOfRange {
            index,
            len: self.samples.len(),
        })
    }

    fn read_member(&mut self, key: &str, m: &MemberEntry) -> Result<Vec<u8>> {
        self.input.seek(SeekFrom::Start(m.offset))?;
        let mut stored = vec![0u8; m.stored_len as usize];
        self.input.read_exact(&mut stored).map_err(map_read)?;
        format::decode_member(key, m, stored)
    }
}
