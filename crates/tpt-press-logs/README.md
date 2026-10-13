# tpt-press-logs

Archives for JSON Lines and CSV logs, for [tpt-press](../../README.md). Records
are grouped into time-stamped blocks, and each column is stored and compressed
separately. A query for a time window reads only the blocks that overlap it,
and inside those blocks it decompresses only the columns it asks for.

Use it for logs you keep and search: application logs, request logs, event
streams. A query like "warnings from 10:00 to 10:05, showing timestamp and
level" touches a small fraction of the archive.

> **Status:** pre-0.1. Format version 2, which may change before 1.0. Nothing is
> published to crates.io yet, and the optional SQL feature takes a git
> dependency.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Input rules](#input-rules)
- [Querying](#querying)
- [Inspecting](#inspecting)
- [Dictionary](#dictionary)
- [Encryption](#encryption)
- [Time-series codecs](#time-series-codecs)
- [SQL (feature `strata`)](#sql-feature-strata)
- [The archive format](#the-archive-format)
- [Results](#results)
- [Command line](#command-line)
- [Examples](#examples)
- [License](#license)

## What it provides

| Item | What it is for |
|------|----------------|
| `pack(input, output, &PackOptions)` | Write an archive. The input must be seekable, because the dictionary is trained from its start |
| `PackOptions` | Format, timestamp field, block size, level, and whether to use a dictionary |
| `query(input, output, &Query)` | Write matching records, filtered by time and by columns |
| `Query { from, to, columns }` | Times in Unix nanoseconds, and an optional column list |
| `inspect(input)` | A `Summary`: columns, record count, time range, and sizes, without decompressing records |
| `parse_timestamp`, `format_timestamp` | RFC 3339 to and from Unix nanoseconds |
| `query_sql` (feature `strata`) | Run SQL over the matching records |
| `gorilla` | Delta-of-delta timestamp and XOR float codecs. Not yet part of the format |

## Installation

```toml
[dependencies]
tpt-press-logs = { path = "crates/tpt-press-logs" }
```

Once published, `cargo add tpt-press-logs`. The minimum supported Rust version
is 1.96.

## Quick start

```rust
use std::io::Cursor;
use tpt_press_logs::{pack, parse_timestamp, query, Format, PackOptions, Query};

let jsonl = std::fs::read("app.jsonl")?;
let mut archive = Vec::new();
pack(
    Cursor::new(&jsonl),
    &mut archive,
    &PackOptions {
        format: Format::Jsonl,
        timestamp_field: "timestamp".to_owned(),
        ..PackOptions::default()
    },
)?;

let mut rows = Vec::new();
let stats = query(
    Cursor::new(&archive),
    &mut rows,
    &Query {
        from: Some(parse_timestamp("2026-03-01T01:00:00Z")?),
        to: Some(parse_timestamp("2026-03-01T01:04:59Z")?),
        columns: Some(vec!["timestamp".to_owned(), "level".to_owned()]),
    },
)?;
println!("matched {} of {} blocks read", stats.records_matched, stats.blocks_read);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Input rules

- **JSON Lines.** One object per line. The timestamp is a field, named by
  `timestamp_field`, and must be an RFC 3339 string. Columns are the union of
  fields, sorted by name in the summary.
- **CSV.** The first line is a header. The timestamp is the column named by
  `timestamp_field`. Columns keep header order.
- The CLI infers CSV from a `.csv` extension, and JSON Lines otherwise.
- Blank lines are skipped and are not records.
- A record whose timestamp does not parse is kept, and its block is marked
  *untimed*. Such blocks are never skipped by a time query, so no record is lost
  from a query that has no time bounds.
- A single record may be at most 64 MiB (`MAX_RECORD_SIZE`).

## Querying

`Query` takes two optional bounds and a column list:

- `from` and `to` are inclusive, in Unix nanoseconds. `None` means no bound.
- `columns: None` writes whole records, with the columns the file had. `Some`
  writes the listed columns in the order given.

`QueryStats` reports what the query cost: `blocks_total`, `blocks_read`,
`columns_read` and `records_matched`. Blocks outside the time window are skipped
from the index, and a column that is not needed is never decompressed. Check
`columns_read` to see the effect on your own queries.

For CSV, a column name that is not in the header is an error, so a typo is not
silently an empty result. JSON Lines fields are optional, so a field a record
does not have is not an error.

## Inspecting

`inspect` reads the header and the index. It then reads each block's column
directory, not its data, so it is fast even for large archives:

```rust
let summary = tpt_press_logs::inspect(Cursor::new(&archive))?;
println!("{} records in {} blocks", summary.records, summary.blocks);
if let Some((first, last)) = summary.time_range {
    println!("{} to {}", format_timestamp(first), format_timestamp(last));
}
```

`Summary` also reports `untimed_blocks`, `dictionary_bytes`, and the
uncompressed and archived sizes.

## Dictionary

By default, `PackOptions::dictionary` is `true`. The writer trains a zstd
dictionary of up to 64 KiB from the cells in the first 10% of the input, stores
it once in the header, and uses it for every column. Set it to `false` to
compress each block on its own.

**Measure it on your data before you rely on it.** The results so far are
mixed:

- On the 57.7 MB logs corpus in [`docs/benchmarks.md`](../../docs/benchmarks.md),
  the dictionary made the archive 7.7% larger, and the pack slower. Turning it
  off is the better choice for that corpus.
- On a synthetic JSON Lines input with a regular structure, the dictionary made
  the archive about three to four times larger, at every size tried. The
  [`dictionary_compare`](examples/dictionary_compare.rs) example prints this.

The cause is not established. The default stays on, because the dictionary may
help smaller blocks or more uniform records. The next step is to test it on
real logs before changing the default.

## Encryption

This crate writes plaintext archives. The CLI encrypts a logs archive by
writing its output through the age encryption in
[`tpt-press-core`](../tpt-press-core/README.md#encryption), and it decrypts into
memory on read, so no plaintext copy reaches the disk. Do the same in your own
code: wrap the output with `crypto::encrypt_with_passphrase` or
`crypto::encrypt_for_recipients`, and read with `crypto::open`.

## Time-series codecs

The `gorilla` module has two codecs for numeric series:

- `encode_timestamps` and `decode_timestamps`: delta-of-delta coding. An exact
  interval costs about one bit per point.
- `encode_floats` and `decode_floats`: the Gorilla XOR codec.

They are measured in [`docs/benchmarks.md`](../../docs/benchmarks.md) against
zstd and shuffle-plus-zstd. Delta-of-delta wins on regular timestamps. The XOR
codec loses badly on quantised readings, where zstd finds repeats it misses. So
the codecs are not used by the format, and the example
[`gorilla_sizes`](examples/gorilla_sizes.rs) shows why.

## SQL (feature `strata`)

With `--features strata`, `query_sql` runs SQL over the records that match the
time and column filters. The result is a table named `logs` (`TABLE_NAME`), and
column types are inferred per column. Columns with no values are left out.

The engine is `tpt-strata`, which is not on crates.io yet, so this feature takes
it from git, pinned to a commit. The engine has no `OR`, `IN`, `LIKE` or
timestamp type, and the result is held in memory.

## The archive format

| Field | Value |
|-------|-------|
| Magic | `TPLG` |
| Version | 2 |
| Footer magic | `TPLX` |
| Header | Format, timestamp field, dictionary, and the CSV header line |
| Block | Columns, each zstd compressed (with the dictionary, if any), and a CRC32 |
| Index | One entry per block: record count, time range, and whether it is untimed |

The layout is documented at the top of [`src/format.rs`](src/format.rs), and
block layout is in [`src/block.rs`](src/block.rs). A summary is in
[`docs/formats.md`](../../docs/formats.md#logs-archive-tplg-version-2).

## Results

From [`docs/benchmarks.md`](../../docs/benchmarks.md), on a 57.7 MB corpus of
400,000 generated JSON lines, single runs on Windows 11:

| Tool | Size | Ratio |
|------|-----:|------:|
| gzip -9 | 6,337,785 | 9.10x |
| `pack` (generic, level 3) | 7,637,702 | 7.55x |
| `pack --level 19` | 5,089,382 | 11.34x |
| `pack-logs` (columnar, dictionary) | 5,744,000 | 10.04x |
| `pack-logs --no-dictionary` | 5,333,970 | 10.8x |

The columnar format beats the generic archive at the same default level, and a
query reads only the columns it names. The benchmark doc is the reference for
these numbers, and it notes which ones are noise.

## Command line

```sh
tpt-press pack-logs app.jsonl app.tplg                 # JSON Lines (the default)
tpt-press pack-logs data.csv data.tplg --format csv --timestamp-field time
tpt-press query app.tplg --from 2026-03-01T00:00:00Z --columns timestamp,level
tpt-press inspect app.tplg
tpt-press query app.tplg --sql "SELECT level, COUNT(*) FROM logs GROUP BY level"  # --features strata
```

## Examples

| Example | Shows |
|---------|-------|
| [`query_time_range`](examples/query_time_range.rs) | JSON Lines, one time window, two columns, and the blocks read |
| [`csv_query`](examples/csv_query.rs) | CSV input with a header, and whole-record queries |
| [`inspect_archive`](examples/inspect_archive.rs) | The summary: columns, time range and sizes, with no decompression |
| [`dictionary_compare`](examples/dictionary_compare.rs) | The same input with and without the dictionary |
| [`gorilla_sizes`](examples/gorilla_sizes.rs) | The time-series codecs against zstd, on generated series |

```sh
cargo run -p tpt-press-logs --example query_time_range
```

## License

Licensed under either of [Apache License 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT), at your option.
