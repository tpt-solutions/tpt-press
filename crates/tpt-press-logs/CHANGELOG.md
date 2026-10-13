# Changelog

All notable changes to `tpt-press-logs` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- `TPLG` archive format, version 2 (footer `TPLX`), for JSON Lines and CSV.
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
