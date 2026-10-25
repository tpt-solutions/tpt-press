//! ML model archiving for tpt-press (`.tptn` archives).
//!
//! Tensors are split only at their own element boundaries, so no compression
//! frame mixes bytes from two tensors. Opening an archive reads only its index,
//! which is enough to inspect shapes, dtypes and sizes. Tensors are then read
//! one piece at a time, which lets a loader copy each piece to its destination
//! as it arrives.
//!
//! Inputs are safetensors, GGUF, ONNX, and PyTorch zip checkpoints (`torch.save`).
//! The PyTorch reader never executes the pickle, and accepts only tensors.

#![forbid(unsafe_code)]

#[cfg(feature = "async")]
pub mod async_reader;
mod dtype;
mod error;
mod format;
pub mod gguf;
mod model;
pub mod onnx;
mod pickle;
pub mod pytorch;
mod reader;
pub mod safetensors;
pub mod sharded;
mod shuffle;
mod split;
mod unpack;
mod writer;

#[cfg(feature = "async")]
pub use async_reader::{AsyncArchive, ShardedArchive};
pub use error::{Error, Result};
pub use format::{Compression, Piece, TensorEntry, MAX_PIECE_BYTES};
pub use model::{Model, TensorInfo};
pub use reader::Archive;
pub use sharded::{pack_sharded, Manifest, Shard, MANIFEST_FILE};
pub use unpack::{unpack, Stats as UnpackStats};
pub use writer::{pack, pack_with_base, PackOptions, Stats};

/// The tensor archive format written by `pack-tensors`.
pub struct TensorsArchive;

impl tpt_press_core::ArchiveFormat for TensorsArchive {
    const NAME: &'static str = "tensors";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
