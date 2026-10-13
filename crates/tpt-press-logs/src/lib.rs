//! Semantic archiving for JSON Lines and CSV logs.
//!
//! Records are grouped into blocks and compressed with a zstd dictionary
//! trained on the input's first 10%. Each block records its time range, so a
//! query for a time window reads only the blocks that overlap it.
//!
//! Each block stores its columns separately, and each column is compressed
//! on its own. A query decompresses only the columns it uses. For example,
//! `--columns timestamp,level` reads two streams per block.

#![forbid(unsafe_code)]

mod block;
mod error;
mod format;
pub mod gorilla;
mod inspect;
mod reader;
mod schema;
#[cfg(feature = "strata")]
mod sql;
mod writer;

pub use error::{Error, Result};
pub use format::Format;
pub use inspect::{inspect, Summary};
pub use reader::{query, Query, QueryStats};
pub use schema::{format_timestamp, parse_timestamp};
#[cfg(feature = "strata")]
pub use sql::{query_sql, TABLE_NAME};
pub use writer::{pack, PackOptions, Stats, DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE};

/// The logs archive format written by `pack-logs`.
pub struct LogsArchive;

impl tpt_press_core::ArchiveFormat for LogsArchive {
    const NAME: &'static str = "logs";
    const MAGIC: [u8; 4] = format::MAGIC;
    const VERSION: u16 = format::VERSION;
}
