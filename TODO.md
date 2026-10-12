# tpt-press TODO

License: `MIT OR Apache-2.0` · Copyright TPT Solutions · Org: `tpt-solutions`

## Decisions (locked)
- Crates: `tpt-press-{core,assets,tensors,logs,cli}`
- On-disk formats: magic bytes + version field, unstable until 1.0
- `.tpa` uses a fixed little-endian bounds-checked layout (e.g. `zerocopy`); logs/tensors headers use postcard/bincode 2 (not bincode 1.x)
- Optional `age` encryption in core, passphrase only for now (whole-stream age, no header flag; see Phase 1)
- Tensor formats: safetensors, GGUF, ONNX, PyTorch pickle (restricted allowlist parser, never executes)
- Logs: JSONL + CSV, simple filters; optional `strata` cargo feature (tpt-strata) for `query --sql`
- CI: Windows/Linux/macOS, stable Rust, MSRV = stable minus ~2

## 0. Project setup & licensing
- [ ] Check crates.io availability for all `tpt-press-*` names (reserve early)
- [x] GitHub remote configured (`origin` → `tpt-solutions/tpt-press`); repo existence on GitHub not verified by me
- [x] Add `LICENSE-MIT` and `LICENSE-APACHE` (copyright TPT Solutions)
- [x] Set `license = "MIT OR Apache-2.0"` in every Cargo.toml (workspace inheritance)
- [x] License section in README + contribution clause (dual-license contributions)
- [x] Workspace `Cargo.toml`, `rust-toolchain.toml`, `rust-version` (MSRV). Set to `1.96` as a placeholder; confirm against the real stable-minus-2 at release time
- [x] `rustfmt.toml`, clippy lints (workspace), `#![forbid(unsafe_code)]` in core. Exceptions for memmap2 in assets are still to be decided in Phase 3
- [x] `deny.toml` (licenses/advisories/bans/sources); `cargo deny check` passes. `cargo-audit` config not added (deny covers advisories)
- [x] `.gitignore` (pre-existing), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`
- [x] Create `crates/` skeleton (5 crates, compiling). `tests/` dir created (README only, no tests yet)

## 1. Phase 1: Foundation (core + CLI skeleton)
### tpt-press-core
- [x] Error types (`thiserror`): `Io`, `Zstd`, `OutputTooLarge`, `InvalidArgument`
- [x] `Pressable` trait (chunk size, level, overridable `split`)
- [ ] `ArchiveFormat` trait (header, index, metadata). Format v1 header and frame layout are implemented in `format.rs`, but not yet behind a trait
- [x] Zstd wrapper: levels 1..=22 validated. Dictionary support not yet
- [x] Parallel chunker (rayon): fixed-size chunks, order preserved
- [ ] Parallel chunker: content-defined chunking
- [x] Streaming Read -> parallel compress -> Write, bounded memory (`archive::pack` / `archive::unpack`, batches of 2 chunks per thread). The in-memory `compress_chunks` API still exists
- [x] Passphrase encryption with `age` 0.12 (`crypto.rs`). Whole archive wrapped in an age stream (age encrypts in 64 KiB authenticated chunks), so the header flag planned earlier is not needed. Encrypted archives are detected by age's magic. Work factor on decrypt is capped at 20 (~1 GiB). Recipient-key mode not yet
- [x] Tampering and wrong passphrase are detected. Wrong passphrase gives "decryption failed"; damage gives "archive is damaged or was altered"
- [x] Checksums per chunk (CRC32 over compressed payload, verified before decompression)
- [x] Decompression-bomb guard (per-chunk output cap) and chunk size limits. Archive-wide limits are still open
- [x] Unit tests (codec, chunker, format header/frame). Integration tests in `crates/core/tests/archive.rs` (13: round-trips, corruption, truncation, limits). Property tests in `crates/core/tests/properties.rs` (3: round-trip, corruption, truncation)
- [x] Truncated archives return `Error::Truncated` ("archive is truncated")
### tpt-press-cli (skeleton)
- [x] clap setup with `pack`, `unpack` and `inspect`. `query` not yet
- [x] Generic `pack`/`unpack` using core. Deviation from spec: no `<format>` argument yet, so `pack <input> <output>` and `unpack <archive> <output>`. `unpack` takes a file, not a directory
- [x] indicatif progress bars on pack and unpack (hidden when not a terminal). Final output is one summary line
- [x] CLI tests in `crates/cli/tests/cli.rs` (5): round-trip, corrupt and truncated archives leave no output, missing input, bad level

## 2. Phase 2: Logs (tpt-press-logs)
- [ ] Archive format: magic/version/header/block index
- [ ] JSONL parser; CSV parser
- [ ] Sample first 10% -> train zstd dictionary
- [ ] Store dictionary in archive; compress blocks with it
- [ ] Columnar extraction (read selected columns only)
- [ ] Block-level time-range index
- [ ] Query API: `--from/--to`, `--columns`
- [ ] CLI `query` for logs
- [ ] Optional `strata` feature: `query --sql` via tpt-strata (note: no OR/IN/LIKE/timestamp type yet)
- [ ] Benchmarks: ratio with vs without dictionary
- [ ] Tests proving query decompresses only relevant blocks

## 3. Phase 3: Assets (tpt-press-assets)
- [ ] `.tpa` format spec (fixed little-endian layout, magic/version)
- [ ] Page-aligned (4KB) chunk packing
- [ ] Flat binary B-tree index at end of file, O(log n) lookup
- [ ] Bounds-checked mmap reader (memmap2), zero-copy slices
- [ ] Handling of compressed vs stored (uncompressed, directly mapped) entries
- [ ] Windows/Linux/macOS mmap behavior tests
- [ ] CLI format detection by extension (+ magic bytes)
- [ ] Benchmarks vs ZIP (criterion)
- [ ] Fuzz the index/header parser

## 4. Phase 4: Tensors (tpt-press-tensors)
- [ ] Archive format (magic/version/header/shards)
- [ ] safetensors parser
- [ ] GGUF parser
- [ ] ONNX parser (protobuf)
- [ ] PyTorch pickle: restricted allowlist reader, never executes, rejects unknown opcodes (last)
- [ ] Tensor-boundary chunking
- [ ] Metadata-only read (layers, dims, dtypes) without decompressing weights
- [ ] tokio async sharded streaming (network source)
- [ ] Integration test: load into mock GPU buffer chunk-by-chunk
- [ ] CLI `inspect`/`query` support for tensors

## 5. Cross-cutting
- [x] `inspect` command for the generic format: totals, ratio, chunk map (`--chunks`), checksums verified, payloads not decompressed. Domain metadata (tensors, logs) still to come
- [ ] Format auto-detection in CLI
- [ ] Fuzz targets (cargo-fuzz) for every parser
- [ ] Memory-safety review of all `unsafe` (mmap)
- [ ] Cross-crate integration tests in `tests/`
- [ ] Rustdoc for all public APIs; examples
- [ ] Format specs documented in `docs/`

## 6. Phase 5: Polish & release
- [ ] README with benchmarks vs tar.gz, zip, plain zstd
- [ ] GitHub Actions: fmt, clippy, tests on Windows/Linux/macOS, MSRV job, cargo-deny
- [ ] Release automation / CHANGELOG
- [ ] Publish crates in order: core -> logs/assets/tensors -> cli
- [ ] Tag v0.1.0
