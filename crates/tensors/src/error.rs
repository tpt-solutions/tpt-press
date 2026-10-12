//! Errors for the tensors crate.

use std::io;

use thiserror::Error;

/// Errors produced by the tensors crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A core operation failed, such as compression.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),

    /// An IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The input is not a valid model file. The detail says what is wrong.
    #[error("invalid model file: {0}")]
    InvalidModel(String),

    /// The input is not a valid safetensors file.
    #[error("invalid safetensors file: {0}")]
    Safetensors(String),

    /// The input is not a tpt-press tensor archive.
    #[error("not a tpt-press tensor archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported tensor archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive is structurally invalid, or a checksum failed.
    #[error("archive is corrupt: {0}")]
    Corrupt(&'static str),

    /// The archive ends early.
    #[error("archive is truncated or was not finished")]
    Truncated,

    /// A tensor with this name is not in the archive.
    #[error("no tensor named `{0}`")]
    NoSuchTensor(String),

    /// A tensor's data does not match its checksum.
    #[error("checksum mismatch in tensor `{0}`")]
    ChecksumMismatch(String),

    /// A dtype this crate does not know how to size.
    #[error("unsupported dtype `{0}`")]
    UnsupportedDtype(String),

    /// The file uses a feature this crate does not support yet.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),

    /// An option has a value that cannot be used.
    #[error("invalid option: {0}")]
    InvalidOption(&'static str),
}

/// Convenience alias for results using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Maps an unexpected end of input to [`Error::Truncated`], and other IO errors to [`Error::Io`].
pub(crate) fn map_io(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io(e)
    }
}
