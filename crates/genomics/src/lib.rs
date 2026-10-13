//! Lossless archiving for FASTA and FASTQ sequence files.
//!
//! Each block splits its records into columns: identifiers, lengths, line
//! widths, case runs, non-ACGT runs, bases, and FASTQ qualities. Most columns
//! are compressed with zstd on their own. Bases and qualities are coded with
//! arithmetic coding and context mixing when that is smaller, and packed or raw
//! otherwise. The rest of the bytes are kept as runs, so the archive is lossless.
//!
//! [`pack`] refuses input it cannot give back byte for byte, and says which
//! line. [`unpack`] restores the original file. [`fetch`] reads one record, or
//! a range of it, by its identifier, and decodes only the block that holds it.
//!
//! This is version 1 of the format. Its context models are not tuned against
//! real sequencing data, which is not available here. See `docs/benchmarks.md`
//! for what was measured, and on which inputs.

#![forbid(unsafe_code)]

mod block;
mod entropy;
mod error;
mod format;
mod reader;
mod writer;

pub use error::{Error, Result};
pub use format::Kind;
pub use reader::{fetch, inspect, unpack, FetchStats, Summary};
pub use writer::{pack, PackOptions, Stats, DEFAULT_BLOCK_BASES};

/// The genomics archive format written by `pack-genomics`.
pub struct GenomicsArchive;

impl tpt_press_core::ArchiveFormat for GenomicsArchive {
    const NAME: &'static str = "genomics";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
