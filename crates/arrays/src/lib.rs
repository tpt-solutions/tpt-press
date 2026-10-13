//! Lossless chunked storage for n-dimensional numeric arrays.
//!
//! An array is split into chunks, and each chunk is stored with the smallest of
//! six lossless filters (none, byte shuffle, XOR and subtraction deltas along the
//! last axis or the row above, with shuffle) and then compressed with zstd. The
//! original `.npy` header is kept, so unpacking a `.npy` archive reproduces the
//! file. [`read_slice`] reads a rectangular slice
//! and decodes only the chunks it touches.
//!
//! Only lossless storage is provided. Error-bounded lossy compression is not, and
//! is not planned as a default. Input is read whole, so memory use is about the
//! size of the array. See `docs/formats.md` for the layout.

#![forbid(unsafe_code)]

mod dtype;
mod error;
mod filter;
mod format;
mod layout;
mod reader;
mod writer;

pub use dtype::{is_npy, parse_npy, Dtype, NpyHeader};
pub use error::{Error, Result};
pub use filter::Filter;
pub use reader::{inspect, read_slice, unpack, Slice, Summary};
pub use writer::{
    default_chunk_shape, pack_npy, pack_raw, FilterChoice, PackOptions, Stats, TARGET_CHUNK_BYTES,
};

/// The arrays archive format written by `pack-arrays`.
pub struct ArraysArchive;

impl tpt_press_core::ArchiveFormat for ArraysArchive {
    const NAME: &'static str = "arrays";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
