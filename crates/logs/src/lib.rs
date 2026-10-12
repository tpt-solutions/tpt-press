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
mod inspect;
mod reader;
mod schema;
mod writer;

pub use error::{Error, Result};
pub use format::Format;
pub use inspect::{inspect, Summary};
pub use reader::{query, Query, QueryStats};
pub use schema::{format_timestamp, parse_timestamp};
pub use writer::{pack, PackOptions, Stats, DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE};
