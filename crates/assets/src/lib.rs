//! Game asset packing for tpt-press (`.tpa` archives).
//!
//! Each asset is stored raw or compressed. Raw assets sit on 4 KB boundaries
//! in a memory-mapped file, so [`Archive::get`] can return them without
//! copying. The index is sorted by name, and lookups binary-search it.
//!
//! The crate forbids `unsafe` code except for the one memory-mapping call in
//! [`reader`], which is documented there.

#![deny(unsafe_code)]

mod error;
mod format;
mod reader;
mod writer;

pub use error::{Error, Result};
pub use format::{validate_name, Compression, Entry, PAGE_SIZE};
pub use reader::{Archive, Asset};
pub use writer::{pack, PackOptions, Stats};
