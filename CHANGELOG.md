# Changelog

All notable changes to tpt-press are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
archive formats may change between any two versions.

## [Unreleased]

### Added

- **Generic archives (`tpt-press-core`)**
  - Content-defined chunking (`pack --content-defined`), using a gear rolling hash.
  - Optional zstd dictionary, trained on the start of the input (`pack --dictionary BYTES`). Header flag bit 0.
  - Passphrase encryption (age), and public-key encryption to X25519 recipients (`pack --recipient`, `--recipient-file`, `unpack --identity`, `keygen`).
  - Archive-wide output limit on unpack (`UnpackOptions::max_total_size`, `unpack --max-output`).
  - `ArchiveFormat` trait, implemented by each format's marker type.
  - `EncryptingWriter`, and `encrypt_with_passphrase` and `encrypt_for_recipients`, which encrypt as they write.
- **Tensors (`tpt-press-tensors`)**
  - Byte-shuffle encoding (piece code 2) for bf16, f16, f32 and f64. It is chosen per piece when smaller. It saves 28 to 30% on a real bf16 checkpoint, against 22% for plain zstd.
  - Readers for safetensors, GGUF, ONNX, and PyTorch checkpoints in both the zip format (1.6 and later) and the legacy format (1.5 and earlier). The pickle reader runs a restricted interpreter and never executes code.
  - Sharded archives: `pack-tensors --shard-size` writes shard files and a JSON manifest.
  - Async readers behind the `async` feature: `AsyncArchive` and `ShardedArchive`, for sources read by range.
  - CLI: `inspect --tensors`, `query <archive> <name>`, and `inspect` and `query` on sharded directories.
- **Logs (`tpt-press-logs`)**
  - Encrypted logs archives: `pack-logs` takes `--encrypt`, `--passphrase-file`, `--recipient` or `--recipient-file`. `query` and `inspect` decrypt into memory, and never write plaintext to disk.
  - `pack-logs --no-dictionary`, and the matching `PackOptions::dictionary` switch.
  - `gorilla` module: delta-of-delta timestamp codec and XOR float codec. They are not yet part of the format.
- **CLI (`tpt-press-cli`)**
  - `inspect` detects every archive type from its contents.
  - `unpack` on a specialised archive names the subcommand to use.
- **Genomics (`tpt-press-genomics`, new crate)**
  - Lossless FASTA and FASTQ archives (`TPGN`, version 1). Bases are coded with context mixing, or packed at 2 bits when that is smaller. FASTQ qualities are coded with a context-mixed coder. Case, non-ACGT runs, line widths and identifiers are separate zstd streams.
  - `fetch` reads one record, or a 1-based inclusive range of it, by name. A sorted name lookup in the index points at the one block to read.
  - The parser refuses input it cannot return byte for byte, and names the line. This covers CRLF endings, a missing final newline, changing line widths, blank lines, and a FASTQ `+` line that is not bare or the id.
  - CLI: `pack-genomics`, `unpack-genomics`, `fetch-genomics`. `inspect` detects the format.
- **Arrays (`tpt-press-arrays`, new crate)**
  - Chunked storage for NumPy-style arrays (`TPAR`, version 1). Each chunk is stored with the smallest of six lossless filters (none, byte shuffle, XOR and subtraction deltas, each along the last axis or the row above, with shuffle) and then zstd.
  - `.npy` input keeps its original header, so unpacking gives back the file byte for byte. Raw input needs `--dtype` and `--shape`.
  - `read_slice` reads a rectangular slice and decodes only the chunks it overlaps.
  - CLI: `pack-arrays`, `unpack-arrays`, `slice-arrays`. `inspect` detects the format.
- **Logs: SQL (`tpt-press-logs`, feature `strata`)**
  - `query --sql` runs SQL over the matching records, through tpt-strata. The result is a table named `logs`. Column types are inferred per column. Columns with no values are left out.
  - tpt-strata is not on crates.io, so this feature takes it from git, pinned to a commit. See the note under *Publishing* in `TODO.md`.
- **Tests, examples and CI**
  - `tests/` is a workspace member (`tpt-press-tests`) with six cross-crate tests.
  - Examples: `cli/examples/make_logs_corpus.rs` (seeded log corpus for the benchmarks) and `cli/examples/compare_formats.rs` (zip and zstd sizes).
  - CI runs clippy and tests with the `strata` feature.
  - Examples: `core/examples/round_trip.rs`, `logs/examples/query_time_range.rs`, `tensors/examples/shard_model.rs`, `logs/examples/gorilla_sizes.rs`.
  - CI runs clippy and tests with the `async` feature, and has an MSRV job on Rust 1.96.0.
  - `.github/workflows/release.yml` runs on version tags, tests, and uploads CLI binaries. It does not publish.

### Fixed

- `inspect --chunks` on a tensor archive is now refused, as it is for logs, rather than silently ignored.
- The PyTorch pickle reader could leak memory on a crafted file that made a container contain itself. Loaded values are now copied into a plain tree, and the loader's containers are cleared. Found by fuzzing, and covered by a regression test.
- Large integers in pickles (such as the legacy magic number) are accepted as opaque values, rather than refused.

### Security

- Shard manifests are validated: file names must be plain, and a tensor may appear in one shard only.
- Declared piece sizes are checked before any buffer is allocated, in both the blocking and async readers.
- Fuzz targets for the PyTorch reader (both formats) and the manifest parser. Runs of 1.7 million, 10.9 million, and 5.5 million inputs found no crashes after the leak fix. The generic-archive target ran 172,000 inputs with no crashes.

### Documentation

- `docs/formats.md`: the on-disk layout of every archive type, the PyTorch rules for both formats, the shard manifest, the piece encodings, and encrypted logs.
- `docs/unsafe-review.md`: review of the single `unsafe` block (mmap).
- `docs/benchmarks.md`: measured results against gzip, the logs dictionary, real and synthetic bf16 weights, and the Gorilla codecs against zstd.
- README: current status, usage examples, and a benchmarks summary.
