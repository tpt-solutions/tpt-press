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
- [x] Archive format: magic `TPLG` v2, header (format, timestamp field, dictionary, CSV header), CRC32 per block, index and footer at the end. Layout is documented at the top of `crates/logs/src/format.rs`; block layout in `crates/logs/src/block.rs`
- [x] JSONL and CSV parsers. CSV handles quoted fields with commas. Records must be single lines, so a CSV field cannot contain a newline. Blank lines are dropped, and CRLF is accepted. CSV rows must match the header's field count, and JSON lines must be objects (errors give the line number)
- [x] Sample first 10% -> train zstd dictionary from the sample's cells (64 KiB max). Too-small samples write no dictionary
- [x] Store dictionary in archive; compress blocks with it (256 KiB blocks, parallel with rayon)
- [x] Columnar extraction. Each block stores one zstd stream per column, with a directory at its start. A query decompresses only the columns it needs, plus the timestamp column when it has a time range. On the 1M-record sample, `--columns timestamp,level` over a one-hour window decompressed 230 columns, against 345 for whole records
- [x] Block-level time-range index. Each block stores its min and max timestamp, and records without one are always read
- [x] Query API: `--from/--to` (RFC 3339), `--columns` (in order). JSONL keys missing from a record output as `null`
- [x] CLI `pack-logs` and `query` (records to stdout, summary to stderr). Deviation from spec: `pack-logs <input> <output>` and `query <archive>` rather than `pack <format> ...`
- [ ] Optional `strata` feature: `query --sql` via tpt-strata (note: no OR/IN/LIKE/timestamp type yet)
- [ ] Benchmarks: ratio with vs without dictionary (ratio measured on a sample; no formal benchmark yet)
- [x] Tests proving query decompresses only relevant blocks (`time_range_skips_blocks_it_does_not_need`, and on a 1M-record log a one-hour window read 115 of 317 blocks)
- [ ] Logs archives do not support passphrase encryption yet. `pack-logs` has no `--encrypt`
- [x] `inspect` works on logs archives: format, record and block counts, columns, time range, dictionary size, and ratio. It reads block directories, not records. Its ratio compares column data to the archive, so it differs from the ratio `pack-logs` prints, which compares the original file. `--chunks` is refused for logs
- [ ] `unpack` does not apply to logs archives. Use `query` to read them back
- [x] Output is rebuilt from columns. Requested columns keep their order. Whole JSON records have keys sorted, and whitespace is normalized, so the text can differ from the input. The values are the same. Storing the raw line would restore byte-exact output but roughly doubles storage. CSV rows are re-quoted, so quoting can differ too
- [x] Measured on the 1M-record sample: archive 1.97 MB → 0.85 MB (42x → 97x) after the columnar change

## 3. Phase 3: Assets (tpt-press-assets)
- [x] `.tpa` format spec v1 (little-endian, magic `TPAS`, version, footer `TPAX`). Layout is documented at the top of `crates/assets/src/format.rs`
- [x] Page-aligned (4 KB) payloads. One payload per asset. Large assets are not split into chunks yet
- [x] Sorted index at end of file, O(log n) lookup by binary search. Deviation from spec: a sorted array, not a B-tree. It gives the same lookup cost and is simpler to check. Index entries are read into memory on open
- [x] Bounds-checked mmap reader (memmap2), zero-copy slices. The one `unsafe` call is in `reader.rs` and has a safety note. The crate is `deny(unsafe_code)` otherwise
- [x] Stored vs compressed entries. Stored assets are used in place. Compressed ones are decompressed into memory. The packer stores an asset raw when compression does not shrink it. `--store` forces raw
- [x] Windows/Linux mmap behavior tests. Windows is run locally, and Linux (Ubuntu 24.04 under WSL2) passes the full suite and clippy. macOS is covered by the CI workflow but has not been run. Measured read speeds differ by platform: on Linux, zero-copy reads of all 105 MB took 3.5 ms against 9.6 ms for `fs::read`, but on Windows the full read was slower with zero-copy
- [x] CLI `pack-assets`, `list-assets` (with `--verify`), `unpack-assets`. Deviation from spec: subcommands for assets rather than format detection. Archive magic is checked, so `inspect`/`unpack` do not handle `.tpa` files
- [x] Unsafe names (absolute, `..`, backslashes, drive letters) are rejected when packing and when opening, so unpack cannot write outside its directory
- [x] Benchmark (`crates/assets/examples/read_bench.rs`), not criterion. Results on Windows, warm cache: reading all 105 MB, zero-copy 34.7 ms, decompressed 15.0 ms, `fs::read` 20.9 ms. Reading the first 4 KB of each asset, zero-copy 0.9 ms, `fs::read` 20.5 ms. Zero-copy wins for partial reads. For full reads it is slower here because each page costs a fault. No ZIP comparison yet
- [x] Fuzz the index/header parser, property-based (proptest, runs on Windows): `crates/assets/tests/hostile_input.rs`. Covers random bytes, damaged archives, and damaged indexes with a recomputed checksum. Coverage-guided fuzzing with cargo-fuzz is still open (see Cross-cutting)

## 4. Phase 4: Tensors (tpt-press-tensors)
- [x] Archive format v1 (`TPTN`, index and footer at the end). Layout is documented at the top of `crates/tensors/src/format.rs`. Sharding is not done: one archive per model, with no shard files
- [x] safetensors parser: header checks, dtype sizes, shape against byte range, sorted tensor names. Also a writer, used by `unpack-tensors`
- [x] GGUF parser (versions 2 and 3). Reads metadata, tensor directory and alignment. Packs with `pack-tensors` (detected from the file's first bytes). Plain types and the Q4/Q5/Q8 and K-quant block types are sized. Other GGML types are refused. Tested on synthetic files built from the GGUF layout, not on files from llama.cpp. Metadata arrays are stored as their length only. GGUF archives cannot be unpacked to safetensors while they hold quantized tensors
- [x] ONNX parser. Reads the protobuf wire format directly, without `prost`. Weights are the graph's initializers, stored inline in `raw_data`. Their file locations are recorded, not read, so packing streams them like safetensors. Refused: external data, typed `*_data` fields, and string tensors. Tested with hand-encoded protobuf files, not with real ONNX models. `pack-tensors` detects it (GGUF by magic, then safetensors, then ONNX). Also in the fuzz target and the random-input tests
- [ ] PyTorch pickle: restricted allowlist reader, never executes, rejects unknown opcodes (last) (not started)
- [x] Tensor-boundary chunking. Tensors are split only at element boundaries, and pieces are at most 16 MiB. No frame mixes two tensors. Pieces are zstd, or raw when compression does not shrink them
- [x] Metadata-only read: opening an archive reads only the footer and index. `inspect` reports tensors, parameters, dtypes and metadata without touching the data
- [ ] tokio async sharded streaming (network source). Not started: the reader is synchronous and works on any `Read + Seek`. Async streaming would wrap it
- [x] Integration test: load into a mock GPU buffer chunk-by-chunk (`loads_a_tensor_chunk_by_chunk_into_a_device_buffer`), with a 2-piece tensor
- [x] Measured: 200 MB single tensor, raw random data. Pack 501 ms, unpack 732 ms, `inspect --verify` ok. Data section identical after round-trip. Compressible weights not measured yet
- [ ] CLI `inspect`/`query` support for tensors

## 5. Cross-cutting
- [x] `inspect` command for the generic format: totals, ratio, chunk map (`--chunks`), checksums verified, payloads not decompressed. Domain metadata (tensors, logs) still to come
- [ ] Format auto-detection in CLI
- [x] Fuzz targets (cargo-fuzz) for the readers: `core_unpack`, `logs_query`, `tensors_read`, `assets_open` in `fuzz/`. Run under nightly on Linux (WSL). Each ran about 90 s with no crashes: 310k, 715k, 4.6M and 928k inputs. Property tests also cover each crate (`tests/hostile_input.rs`). Not run in CI, and no seed corpus yet. Earlier review fixed two allocation risks: an asset's declared size and a logs block's declared length are now checked before allocation
- [ ] Memory-safety review of all `unsafe` (mmap)
- [ ] Cross-crate integration tests in `tests/`
- [ ] Rustdoc for all public APIs; examples
- [ ] Format specs documented in `docs/`

## 6. Phase 5: Polish & release
- [ ] README with benchmarks vs tar.gz, zip, plain zstd
- [x] GitHub Actions (`.github/workflows/ci.yml`): fmt, clippy and tests on Windows, Linux and macOS, plus cargo-deny. Written but not yet run on GitHub. MSRV job not added
- [ ] Release automation / CHANGELOG
- [ ] Publish crates in order: core -> logs/assets/tensors -> cli
- [ ] Tag v0.1.0

## 7. Future domains (post-v0.1)
Non-goals: no image/audio/video recompression. ML datasets store image blobs opaquely.

### 7a. Time-series & metrics (extend `tpt-press-logs` or sibling `tpt-press-timeseries`)
- [ ] Decide: new input format in logs vs separate crate
- [ ] Parsers: Prometheus text/remote-write dumps, OpenTelemetry JSON, CSV with timestamp column
- [ ] Delta-of-delta timestamp encoding
- [ ] XOR (Gorilla-style) float encoding
- [ ] Per-column dictionaries for labels/tags
- [ ] Reuse block/column directory and time-range index from `logs` (block.rs, inspect.rs)
- [ ] Query by time range and series/label; `inspect` support
- [ ] Benchmarks vs plain zstd and Parquet

### 7b. ML datasets (`tpt-press-datasets`, builds on `.tpa` + tensors streaming)
- [ ] Depends on Phase 3 (`.tpa` index/mmap) and Phase 4 (async sharded streaming)
- [ ] Shard format: WebDataset-style samples grouped by key, random access by sample index
- [ ] Store already-compressed media (JPEG/PNG/etc.) as-is; compress only text/metadata/tensors
- [ ] Deterministic shuffling-friendly index; shard-level checksums
- [ ] Network/streamed shard reading
- [ ] Import from WebDataset tar, Hugging Face-style JSONL+files
- [ ] Benchmarks: sample throughput vs tar shards

### 7c. Genomics (`tpt-press-genomics`; needs domain review)
- [ ] FASTA/FASTQ parsers (split into ID, sequence, quality columns)
- [ ] 2-bit nucleotide packing + context-model/zstd for sequence; quality-score modelling
- [ ] VCF as a stretch goal
- [ ] Region/read-range index for random access
- [ ] Compare against bgzip and CRAM
- [ ] Find a domain reviewer before committing to the format
- [ ] SIMD-friendly parts: 2-bit packing, FASTQ column split and quality-score modelling (see 7g)

### 7d. Prerequisites that make any new domain cheaper
- [ ] `ArchiveFormat` trait (already open in Phase 1)
- [ ] CLI format auto-detection (already open in Section 5)
- [ ] Content-defined chunking (already open in Phase 1)

### 7e. Lossless model-weight compression (extend `tpt-press-tensors`)
Lossless only: output is byte-identical after unpack. Lossy quantization is a non-goal.
- [ ] Literature check first: ZipNN, DFloat11 and newer work, so we build on published results
- [ ] Float-aware codec for bf16/fp16/fp8: split exponent and mantissa streams, entropy-code them (ANS or Huffman). Target about 25-35% savings. Hook in at tensor-boundary chunking (`crates/tensors/src/writer.rs`) with a per-piece codec flag in the frame format (`crates/tensors/src/format.rs`). Keep the raw fallback when it does not shrink
- [ ] Benchmarks on real bf16 checkpoints (Llama-class) vs plain zstd; put numbers in the README. Also closes the Phase 4 note "compressible weights not measured yet"
- [ ] Training checkpoints: fp32 master weights, Adam moments (`m`, `v`), step counter, scheduler and RNG state. Delta against the previous checkpoint plus the float codec. Lossless only, since a resumed run must be exact. Needs PyTorch/DeepSpeed/safetensors checkpoint layouts (depends on the open PyTorch pickle reader in Phase 4). Benchmark on a real small-model run
- [ ] Delta compression against a base model: fine-tunes, merges and training checkpoints stored as differences from a parent, plus dedup of identical tensors across archives. Needs a way to reference the base archive and a clear error when it is missing
- [ ] Entropy coding for quantized formats: GGUF scales, zero-points, indices and higher-precision tensors (small gain, cheap once the float codec exists)
- [ ] Stretch: streaming decode at load time (layer by layer) so run-time memory also shrinks, as DFloat11 shows for bf16. Needs fast SIMD/GPU decoders. Only after the float codec and benchmarks show strong numbers

### 7f. Scientific arrays (`tpt-press-arrays`; needs domain review)
- [ ] Decide scope: lossless first (float byte-shuffle or XOR-delta, then zstd). Error-bounded lossy (SZ/ZFP-style) only as an explicit opt-in, never the default
- [ ] Input formats: raw n-dim arrays, NumPy `.npy`/`.npz`, Zarr. HDF5/NetCDF as a stretch (large parsers, so a read-only subset first)
- [ ] Chunked layout with a per-chunk index for random-access slicing (reuse the `.tpa` and tensors chunk + index design)
- [ ] Predictors: delta along an axis, Lorenzo-style neighbour prediction, XOR of float bit patterns
- [ ] Benchmarks vs Blosc, zstd with shuffle, and Zarr defaults
- [ ] Find a domain reviewer before fixing the format

### 7g. tpt-simd integration (optional)
Optional `simd` cargo feature. A scalar fallback always exists, and results must be bit-identical to it.
- [ ] Float codec: exponent/mantissa split and bf16/fp16 handling via `tpt-simd-half` plus `tpt-simd-permute`/`tpt-simd-transpose` (byte-plane shuffle)
- [ ] Delta and XOR-delta coding (checkpoints, time-series, arrays) via `tpt-simd-bits`
- [ ] 2-bit nucleotide packing and unpacking (genomics) via `tpt-simd-bits` and `tpt-simd-permute`
- [ ] Newline/record splitting for FASTQ and logs via `tpt-simd-search`
- [ ] Per-chunk CRC32 via `tpt-simd-hash` (core uses a scalar CRC32 today)
- [ ] Decide: dependency on a pre-release crate (path vs published). `unsafe` must stay inside tpt-simd so `forbid(unsafe_code)` in core still holds
- [ ] Benchmark that SIMD beats the compiler's autovectorized scalar code before keeping each kernel
