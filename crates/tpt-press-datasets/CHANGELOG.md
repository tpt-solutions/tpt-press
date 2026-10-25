# Changelog

All notable changes to `tpt-press-datasets` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- `.tpds` dataset archive, version 1 (magic `TPDS`, footer `TPDX`). Samples keep
  write order, and each has named members. The layout is in `docs/formats.md`.
- Members that are compressed media are stored raw. Other members use zstd when
  smaller. The index is zstd-compressed.
- `Dataset`: `open`, `len`, `key`, `member_names`, `member_codec`, `sample`,
  `member` (reads one member without the others) and `verify`.
- `Writer` and `pack_samples`, with validation of keys and member names.
- `import_hf`: Hugging Face-style folder import (`metadata.jsonl` plus the files it names). Paths that leave the folder are refused.
- `import_tar`: WebDataset tar import, streaming. Files with no dot in their name
  are skipped and counted.
- `shuffled_order`: a deterministic permutation for a seed.
- `AsyncDataset` behind the `async` feature (tokio), for sources read by range.
  It reads the same archives as `Dataset`.
- CLI: `import-dataset`, `import-hf` and `list-dataset` (with `--verify`).
