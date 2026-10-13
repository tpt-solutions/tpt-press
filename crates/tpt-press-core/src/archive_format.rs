//! The identity of an archive format: how its files start, and which version
//! this crate writes. Each domain crate implements [`ArchiveFormat`] for its
//! own format, so a tool can tell the formats apart from the first bytes of a file.

use crate::format;

/// An archive format.
pub trait ArchiveFormat {
    /// A short name for messages, such as `logs`.
    const NAME: &'static str;
    /// The four bytes at the start of every file in this format.
    const MAGIC: [u8; 4];
    /// The format version this crate writes.
    const VERSION: u16;

    /// Whether `header`, the first bytes of a file, belongs to this format.
    fn recognises(header: &[u8]) -> bool {
        header.len() >= 4 && header[..4] == Self::MAGIC
    }
}

/// The generic archive written by `pack`.
pub struct GenericArchive;

impl ArchiveFormat for GenericArchive {
    const NAME: &'static str = "generic";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
