//! ML model archiving for tpt-press (`.tptn` archives).
//!
//! Tensors are split only at their own element boundaries, so no compression
//! frame mixes bytes from two tensors. Opening an archive reads only its index,
//! which is enough to inspect shapes, dtypes and sizes. Tensors are then read
//! one piece at a time, which lets a loader copy each piece to its destination
//! as it arrives.
//!
//! Only safetensors input is supported so far. GGUF, ONNX and pickle are not.

#![forbid(unsafe_code)]

mod dtype;
mod error;
mod format;
pub mod gguf;
mod model;
pub mod onnx;
mod reader;
pub mod safetensors;
mod unpack;
mod writer;

pub use error::{Error, Result};
pub use format::{Compression, Piece, TensorEntry, MAX_PIECE_BYTES};
pub use model::{Model, TensorInfo};
pub use reader::Archive;
pub use unpack::{unpack, Stats as UnpackStats};
pub use writer::{pack, PackOptions, Stats};
