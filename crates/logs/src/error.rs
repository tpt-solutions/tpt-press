//! Errors for the logs archive.

use std::io;

use thiserror::Error;

/// Errors produced by the logs crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A core operation failed, such as compression or decryption.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),

    /// An IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The input is not a tpt-press logs archive.
    #[error("not a tpt-press logs archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported logs archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive is structurally invalid, or a checksum failed.
    #[error("archive is corrupt: {0}")]
    Corrupt(&'static str),

    /// The archive ends early. Its footer or a block is missing.
    #[error("archive is truncated or was not finished")]
    Truncated,

    /// CSV input has no header line, so its columns cannot be named.
    #[error("CSV input has no header line")]
    MissingHeader,

    /// A requested column is not in the CSV header.
    #[error("unknown column `{0}`")]
    UnknownColumn(String),

    /// A timestamp given by the user is not RFC 3339.
    #[error("invalid timestamp `{0}`; expected RFC 3339 such as 2026-10-13T12:00:00Z")]
    InvalidTimestamp(String),

    /// A record cannot be parsed in the format the archive declares.
    #[error("cannot parse record: {0}")]
    InvalidRecord(String),

    /// A record is larger than the format can store.
    #[error("record of {0} bytes exceeds the {1}-byte limit")]
    RecordTooLarge(usize, usize),

    /// An option has a value that cannot be used.
    #[error("invalid option: {0}")]
    InvalidOption(&'static str),

    /// The SQL text is invalid, or the engine cannot run it on these records.
    #[cfg(feature = "strata")]
    #[error("{0}")]
    Engine(String),
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
