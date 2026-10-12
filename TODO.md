# tpt-press TODO

License: `MIT OR Apache-2.0` · Copyright TPT Solutions · Org: `tpt-solutions`

## Decisions (locked)
- Crates: `tpt-press-{core,assets,tensors,logs,cli}`
- On-disk formats: magic bytes + version field, unstable until 1.0
- `.tpa` uses a fixed little-endian bounds-checked layout (e.g. `zerocopy`); logs/tensors headers use postcard/bincode 2 (not bincode 1.x)
- Optional `age` encryption in core (flag + header field)
- Tensor formats: safetensors, GGUF, ONNX, PyTorch pickle (restricted allowlist parser, never executes)
- Logs: JSONL + CSV, simple filters; optional `strata` cargo feature (tpt-strata) for `query --sql`
- CI: Windows/Linux/macOS, stable Rust, MSRV = stable minus ~2

## 0. Project setup & licensing
- [ ] Check crates.io availability for all `tpt-press-*` names (reserve early)
- [ ] Create GitHub repo under `tpt-solutions`
- [ ] Add `LICENSE-MIT` and `LICENSE-APACHE` (copyright TPT Solutions)
- [ ] Set `license = "MIT OR Apache-2.0"` in every Cargo.toml (workspace inheritance)
- [ ] License section in README + contribution clause (dual-license contributions)
- [ ] Workspace `Cargo.toml`, `rust-toolchain.toml`, `rust-version` (MSRV)
- [ ] `rustfmt.toml`, clippy config, `#![forbid(unsafe_code)]` policy (documented exceptions: memmap2)
- [ ] `cargo-deny` (licenses/advisories) and `cargo-audit` config
- [ ] `.gitignore`, `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`
- [ ] Create `crates/` skeleton and `tests/` dir

## 1. Phase 1: Foundation (core + CLI skeleton)
### tpt-press-core
- [ ] Error types (`thiserror`)
- [ ] `Pressable` trait
- [ ] `ArchiveFormat` trait (header, index, metadata)
- [ ] Zstd wrapper (levels, dictionary support)
- [ ] Parallel chunker (rayon): fixed-size chunks
- [ ] Parallel chunker: content-defined chunking
- [ ] Streaming Read -> parallel compress -> Write, bounded memory
- [ ] Optional `age` encryption (feature flag, header flag/field)
- [ ] Checksums/integrity per chunk
- [ ] Limits for untrusted input (max sizes, allocation caps, decompression-bomb guard)
- [ ] Unit + property tests
### tpt-press-cli (skeleton)
- [ ] clap setup, subcommands `pack`, `unpack`, `inspect`, `query`
- [ ] Generic `pack`/`unpack` using core only (better-zstd)
- [ ] indicatif progress bars; console output
- [ ] Integration test: round-trip

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
- [ ] `inspect` command: metadata, ratios, chunk maps
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
