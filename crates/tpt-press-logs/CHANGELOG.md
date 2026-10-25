# Changelog

All notable changes to `tpt-press-logs` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- Gorilla delta-of-delta encoding for timestamp columns. A block uses it when
  every record's time is in canonical form and it compresses smaller. On a
  200,000-record sample the archive shrank 19% (1,202,174 to 973,487 bytes), and
  the round trip is exact.
- Prometheus text exposition input (`--format prometheus`, or a `.prom` file).
  Comment lines are skipped. Samples are stored as JSON objects, so queries and
  time ranges work as for JSON Lines.
- `TPLG` archive format, version 3 (footer `TPLX`), for JSON Lines, CSV and Prometheus text samples.
  Version 3 adds an encoding byte to each block column. (footer `TPLX`), for JSON Lines, CSV and Prometheus text samples.
  Records are grouped into blocks of about 256 KiB by default, and each block
  records its time range. The layout is documented at the top of
  `src/format.rs`.
- `pack`, with `PackOptions` for format, timestamp field, block size, level and
  dictionary.
- `query`, with `Query { from, to, columns }`. Blocks outside the time window
  are skipped from the index, and only the columns asked for are decompressed.
  `QueryStats` reports the blocks and columns read.
- `inspect`, which returns a `Summary` (columns, record count, time range and
  sizes) without decompressing any record.
- `parse_timestamp` and `format_timestamp`, for RFC 3339 and Unix nanoseconds.
- Untimed blocks: a record whose timestamp does not parse is kept, and its
  block is always read by a time query.
- Trained zstd dictionary, on by default, from the first 10% of the input.
  `PackOptions::dictionary` turns it off. See the README for measured results.
- `gorilla` module: delta-of-delta timestamp codec and XOR float codec. They are
  not yet part of the format.
- Optional `strata` feature: `query_sql` runs SQL over the matching records,
  through `tpt-strata`, as a table named `logs`.
- CLI: `pack-logs` (with `--format`, `--timestamp-field`, `--block-size`,
  `--no-dictionary` and encryption options), `query` (with `--from`, `--to`,
  `--columns` and `--sql`), and `inspect` for logs archives.
- Encrypted logs archives, made by the CLI. `query` and `inspect` decrypt into
  memory, and never write plaintext to disk.
- Examples: `query_time_range`, `csv_query`, `inspect_archive`,
  `dictionary_compare` and `gorilla_sizes`.
