//! Errors for the assets archive.

use std::io;

use thiserror::Error;

/// Errors produced by the assets crate.
#[derive(Debug, Error)]
pub enum Error {
    /// A core operation failed, such as decompression.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),

    /// An IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The input is not a tpt-press assets archive.
    #[error("not a tpt-press assets archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported assets archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive is structurally invalid, or a checksum failed.
    #[error("archive is corrupt: {0}")]
    Corrupt(&'static str),

    /// The archive ends early.
    #[error("archive is truncated or was not finished")]
    Truncated,

    /// An asset name is not a safe relative path.
    #[error("unsafe asset name `{0}`")]
    UnsafeName(String),

    /// Two files would share one asset name.
    #[error("duplicate asset name `{0}`")]
    DuplicateName(String),

    /// An asset's stored checksum does not match its contents.
    #[error("checksum mismatch for asset `{0}`")]
    ChecksumMismatch(String),

    /// An option has a value that cannot be used.
    #[error("invalid option: {0}")]
    InvalidOption(&'static str),
}

/// Convenience alias for results using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
