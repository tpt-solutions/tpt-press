//! Error types shared by all tpt-press crates.

use thiserror::Error;

/// Errors produced by tpt-press core operations.
#[derive(Debug, Error)]
pub enum Error {
    /// An underlying IO operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Compression or decompression failed inside zstd.
    #[error("zstd error: {0}")]
    Zstd(std::io::Error),

    /// The whole archive would unpack to more than the caller's total limit.
    #[error("archive would unpack to more than {limit} bytes")]
    ArchiveTooLarge {
        /// The total limit that was exceeded, in bytes.
        limit: u64,
    },

    /// Decompressed output would exceed the caller's configured limit.
    #[error("decompressed size exceeds limit of {limit} bytes")]
    OutputTooLarge {
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },

    /// The caller supplied an argument that cannot be used.
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    /// The archive is encrypted but no passphrase was supplied.
    #[error("archive is encrypted; a passphrase is required")]
    PassphraseRequired,

    /// The passphrase is wrong, or the encrypted archive was altered.
    #[error("decryption failed: wrong passphrase, or the archive is damaged")]
    DecryptionFailed,

    /// The encrypted archive could not be decrypted for another reason.
    #[error("cannot decrypt archive: {0}")]
    Decrypt(String),

    /// The archive is not encrypted for any of the given keys.
    #[error("archive is not encrypted for this key")]
    NotForThisKey,

    /// Encryption failed. Usually means no recipients were given.
    #[error("cannot encrypt archive: {0}")]
    Encrypt(String),

    /// The archive ends before its end marker, so it was cut short.
    #[error("archive is truncated")]
    Truncated,

    /// The input does not start with the tpt-press archive magic.
    #[error("not a tpt-press archive (bad magic)")]
    BadMagic,

    /// The archive was written by a format version this build does not read.
    #[error("unsupported archive version {0}")]
    UnsupportedVersion(u16),

    /// The archive header sets flags this build does not understand.
    #[error("unsupported archive flags {0:#06x}")]
    UnsupportedFlags(u16),

    /// The header is structurally invalid, such as a dictionary of the wrong size.
    #[error("archive header is corrupt: {0}")]
    CorruptHeader(&'static str),

    /// A chunk failed its checksum or is structurally invalid.
    #[error("chunk {index} is corrupt")]
    ChunkCorrupt {
        /// Zero-based position of the chunk in the archive.
        index: u64,
    },

    /// A chunk header declares sizes above the configured limit.
    #[error("chunk of {size} bytes exceeds limit of {limit} bytes")]
    ChunkTooLarge {
        /// Declared size in bytes.
        size: u64,
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },
}

/// Convenience alias for results using [`enum@Error`].
pub type Result<T> = core::result::Result<T, Error>;
