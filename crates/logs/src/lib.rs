//! Semantic archiving for JSON Lines and CSV logs.
//!
//! Records are grouped into blocks and compressed with a zstd dictionary
//! trained on the input's first 10%. Each block records its time range, so a
//! query for a time window reads only the blocks that overlap it.
//!
//! Compression is per block, not per column. A column query therefore
//! decompresses every block that overlaps the time range, then drops the
//! columns that were not asked for.

#![forbid(unsafe_code)]

mod error;
mod format;
mod reader;
mod schema;
mod writer;

pub use error::{Error, Result};
pub use format::Format;
pub use reader::{query, Query, QueryStats};
pub use schema::parse_timestamp;
pub use writer::{pack, PackOptions, Stats, DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE};
