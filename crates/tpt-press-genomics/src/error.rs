//! Errors for the genomics archive.

use std::io;

use thiserror::Error;

/// Errors produced by the genomics crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A core operation failed, such as compression.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),

    /// An IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The input is not a tpt-press genomics archive.
    #[error("not a tpt-press genomics archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported genomics archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive is structurally invalid, or a checksum failed.
    #[error("archive is corrupt: {0}")]
    Corrupt(&'static str),

    /// The archive ends early. Its footer or a block is missing.
    #[error("archive is truncated or was not finished")]
    Truncated,

    /// The input is not valid FASTA or FASTQ.
    #[error("line {line}: {message}")]
    Format {
        /// The line that caused the error, counting from 1.
        line: u64,
        /// What is wrong with the line.
        message: String,
    },

    /// The input is valid, but this format version cannot store it.
    #[error("line {line}: {message}")]
    Unsupported {
        /// The line that caused the error, counting from 1.
        line: u64,
        /// What the format does not support.
        message: &'static str,
    },

    /// No record has the requested identifier.
    #[error("no record named `{0}`")]
    NotFound(String),

    /// A requested range does not fit the record.
    #[error("range {start}-{end} does not fit a record of {length} bases")]
    RangeOutOfBounds {
        /// First base, 1-based.
        start: u64,
        /// Last base, 1-based and inclusive.
        end: u64,
        /// The record's length in bases.
        length: u64,
    },

    /// An option has a value that cannot be used.
    #[error("invalid option: {0}")]
    InvalidOption(&'static str),
}

/// Convenience alias for results using [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Maps an unexpected end of input to [`Error::Truncated`], and other IO errors to [`Error::Io`].
pub(crate) fn map_io(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io(e)
    }
}
