//! Shared traits, IO utilities, chunking and error handling for tpt-press.
//!
//! This crate is domain-agnostic. Domain crates (`tpt-press-assets`,
//! `tpt-press-tensors`, `tpt-press-logs`) implement the traits defined here.

#![forbid(unsafe_code)]

pub mod archive;
pub mod chunker;
pub mod codec;
pub mod crypto;
pub mod error;
mod format;

pub use archive::{
    inspect, pack, unpack, ChunkInfo, Inspection, PackOptions, Stats, UnpackOptions,
};
pub use chunker::{compress_chunks, decompress_chunks, Pressable};
pub use crypto::{open as open_archive, pack_encrypted};
pub use error::{Error, Result};
