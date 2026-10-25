//! Errors from reading and writing dataset archives.

use thiserror::Error;

/// Errors from dataset archives.
#[derive(Debug, Error)]
pub enum Error {
    /// An I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The file does not start with the dataset magic bytes.
    #[error("not a dataset archive (bad magic)")]
    BadMagic,
    /// The archive was written by a format version this build does not read.
    #[error("unsupported dataset archive version {0}")]
    UnsupportedVersion(u16),
    /// The archive ends before its declared contents.
    #[error("archive is truncated")]
    Truncated,
    /// The archive's structure is inconsistent.
    #[error("archive is damaged or was altered: {0}")]
    Corrupt(&'static str),
    /// A member's bytes do not match their checksum.
    #[error("checksum mismatch in sample {sample:?}, member {member:?}")]
    ChecksumMismatch {
        /// Key of the sample.
        sample: String,
        /// Name of the member.
        member: String,
    },
    /// The input cannot be stored as given.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A sample index is past the end of the archive.
    #[error("sample index {index} is out of range ({len} samples)")]
    OutOfRange {
        /// Requested index.
        index: usize,
        /// Samples in the archive.
        len: usize,
    },
    /// A sample has no member with the requested name.
    #[error("sample {sample:?} has no member {member:?}")]
    MissingMember {
        /// Key of the sample.
        sample: String,
        /// Requested member name.
        member: String,
    },
    /// An error from the shared compression layer.
    #[error(transparent)]
    Core(#[from] tpt_press_core::Error),
}

/// Result type for dataset archives.
pub type Result<T> = std::result::Result<T, Error>;

/// Maps an I/O error from a read: running out of bytes means the archive is truncated.
pub(crate) fn map_read(err: std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io(err)
    }
}
