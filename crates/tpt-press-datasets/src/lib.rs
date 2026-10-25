//! Sharded training datasets: samples grouped by key, with random access.
//!
//! A dataset archive (`.tpds`) holds samples in the order they were written. Each
//! sample has a key and named members, such as `image.jpg` and `label.cls`.
//! Opening an archive reads only its index, so sample `i` is read without reading
//! the others. Members that are already compressed media (JPEG, PNG, MP4 and
//! similar) are stored as they are. Other members are compressed with zstd when
//! that is smaller.
//!
//! [`import_tar`] converts a WebDataset tar into an archive, streaming one sample
//! at a time. [`shuffled_order`] gives a repeatable random order over the samples.
//! The layout is documented at the top of `src/format.rs`.

#![forbid(unsafe_code)]

mod error;
mod format;
mod import;
mod reader;
mod shuffle;
mod writer;

pub use error::{Error, Result};
pub use format::Codec;
pub use import::import_tar;
pub use reader::Dataset;
pub use shuffle::shuffled_order;
pub use writer::{pack_samples, Member, Sample, Stats, Writer};

/// The dataset archive format.
pub struct DatasetsArchive;

impl tpt_press_core::ArchiveFormat for DatasetsArchive {
    const NAME: &'static str = "datasets";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
