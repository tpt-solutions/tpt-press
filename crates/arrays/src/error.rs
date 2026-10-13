//! Errors for the arrays archive.

use std::io;

use thiserror::Error;

/// Errors produced by the arrays crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A core operation failed, such as compression.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),

    /// An IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The input is not a tpt-press arrays archive.
    #[error("not a tpt-press arrays archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported arrays archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive is structurally invalid, or a checksum failed.
    #[error("archive is corrupt: {0}")]
    Corrupt(&'static str),

    /// The archive or file ends early.
    #[error("archive is truncated or was not finished")]
    Truncated,

    /// The input is not a valid array: a bad `.npy` header, or data that does not match the shape.
    #[error("{message}")]
    Format {
        /// What is wrong with the input.
        message: String,
    },

    /// The element type is not one this crate stores.
    #[error("unsupported element type `{0}`; use little-endian f2, f4, f8, i1-i8, u1-u8, or b1")]
    InvalidDtype(String),

    /// The input is valid, but this format version cannot store it.
    #[error("{message}")]
    Unsupported {
        /// What the format does not support.
        message: &'static str,
    },

    /// A requested slice does not fit the array.
    #[error("slice {start}..{end} does not fit axis {axis} of length {length}")]
    RangeOutOfBounds {
        /// The axis, counting from 0.
        axis: usize,
        /// First index, inclusive.
        start: u64,
        /// Last index, exclusive.
        end: u64,
        /// The axis length.
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
