//! Shared traits, IO utilities, chunking and error handling for tpt-press.
//!
//! This crate is domain-agnostic. Domain crates (`tpt-press-assets`,
//! `tpt-press-tensors`, `tpt-press-logs`) implement the traits defined here.
//!
//! # Example
//!
//! ```
//! use std::io::Cursor;
//! use tpt_press_core::{pack, unpack, PackOptions, UnpackOptions};
//!
//! let data = b"hello, hello, hello, hello".repeat(100);
//! let mut archive = Vec::new();
//! pack(Cursor::new(&data), &mut archive, &PackOptions::default())?;
//!
//! let mut restored = Vec::new();
//! unpack(Cursor::new(&archive), &mut restored, &UnpackOptions::default())?;
//! assert_eq!(restored, data);
//! # Ok::<(), tpt_press_core::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod archive;
pub mod archive_format;
pub mod chunker;
pub mod codec;
pub mod crypto;
pub mod error;
mod format;
pub mod split;

pub use archive::{
    inspect, pack, unpack, ChunkInfo, Inspection, PackOptions, Stats, UnpackOptions,
};
pub use archive_format::{ArchiveFormat, GenericArchive};
pub use chunker::{compress_chunks, decompress_chunks, Pressable};
pub use crypto::{
    encrypt_for_recipients, encrypt_with_passphrase, looks_encrypted, open as open_archive,
    open_with_identities, pack_encrypted, pack_for_recipients, EncryptingWriter,
};
pub use error::{Error, Result};
pub use split::Chunking;
