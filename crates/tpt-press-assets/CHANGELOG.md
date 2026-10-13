# Changelog

All notable changes to `tpt-press-assets` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- `.tpa` archive format, version 1 (magic `TPAS`, footer `TPAX`). The layout is
  documented at the top of `src/format.rs`.
- `pack`, which writes an archive from an iterator of named assets, one asset
  in memory at a time.
- `PackOptions::store_all`, to store every asset raw. Without it, an asset is
  stored raw only when compression does not make it smaller.
- Memory-mapped reader: `Archive::open`, `entries`, `get` and `verify`. Stored
  assets are returned as slices into the map, with no copy. Compressed assets
  are decompressed into a new buffer.
- Raw assets are aligned to 4 KiB pages, so reads through the map are
  page-aligned.
- `Asset::is_zero_copy`, to check which path a read took.
- Name rules, enforced on pack and on read (`validate_name`): relative paths,
  forward slashes, no `..`, no duplicates.
- Index sorted by name, with binary-search lookup.
- Property tests and hostile-input tests, in `tests/`.
- `read_bench` example, and the examples `pack_and_read`, `zero_copy` and
  `pack_directory`.
- CLI: `pack-assets` (with `--store` and `--level`), `unpack-assets` and
  `list-assets` (with `--verify`).

### Security

- The one `unsafe` call, the memory map, is reviewed in
  `docs/unsafe-review.md`. The rest of the crate denies `unsafe`.
- A compressed asset may declare at most 1 GiB, and that size is checked before
  decompression allocates anything.
- Unpacked names are checked, so an archive cannot write outside its output
  directory.
