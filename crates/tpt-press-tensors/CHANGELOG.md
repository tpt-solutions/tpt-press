# Changelog

All notable changes to `tpt-press-tensors` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- `.tptn` archive format, version 1 (magic `TPTN`, footer `TPTX`). The layout is
  documented at the top of `src/format.rs`.
- `pack`, which writes an archive from a safetensors, GGUF, ONNX or PyTorch
  input. The input format is detected from its first bytes.
- Readers for safetensors, GGUF (versions 2 and 3), ONNX (inline initializers)
  and PyTorch checkpoints, in both the zip format (PyTorch 1.6 and later) and
  the legacy format (PyTorch 1.5 and earlier). The pickle reader runs a
  restricted interpreter and never executes code.
- Byte-shuffle piece codec for bf16, f16, f32 and f64. It is chosen per piece
  when it is smaller. On a real bf16 checkpoint (SmolLM2-135M) it saves 28 to
  30%, against 22% for plain zstd.
- Pieces of at most 16 MiB, split at element boundaries, each with a CRC32.
- `Archive` with `metadata`, `tensors`, `tensor`, `read_tensor`,
  `read_tensor_into` (streamed, one piece at a time) and `verify`.
- `unpack`, which writes an archive back out as a safetensors file.
- Sharded archives: `pack_sharded` writes shards of a given size, with a JSON
  manifest (`manifest.json`). The caller decides where each shard goes.
- Async readers behind the `async` feature: `AsyncArchive` and
  `ShardedArchive`, for sources read by range.
- CLI: `pack-tensors` (with `--shard-size`), `unpack-tensors`, `inspect
  --tensors`, and `query <archive> <name>`, including on sharded directories.
- Examples: `pack_and_read`, `unpack_to_safetensors`, `shard_model` and
  `async_read`.

### Fixed

- The PyTorch pickle reader could leak memory on a crafted file that made a
  container contain itself. Loaded values are now copied into a plain tree, and
  the loader's containers are cleared. Found by fuzzing, and covered by a
  regression test.
- Large integers in pickles, such as the legacy magic number, are accepted as
  opaque values rather than refused.

### Security

- Shard manifests are validated. File names must be plain, and a tensor may
  appear in one shard only.
