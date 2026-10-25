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
- [x] Check crates.io availability for all `tpt-press-*` names. All six (`tpt-press`, and the `-core`, `-assets`, `-tensors`, `-logs`, `-cli` crates) returned 404 from the crates.io API. This is a point-in-time check, and nothing is reserved yet. Reserving means publishing, which needs your decision
- [x] GitHub remote configured (`origin` → `tpt-solutions/tpt-press`); repo existence on GitHub not verified by me
- [x] Add `LICENSE-MIT` and `LICENSE-APACHE` (copyright TPT Solutions)
- [x] Set `license = "MIT OR Apache-2.0"` in every Cargo.toml (workspace inheritance)
- [x] License section in README + contribution clause (dual-license contributions)
- [x] Workspace `Cargo.toml`, `rust-toolchain.toml`, `rust-version` (MSRV). Set to `1.96`. Confirmed: `cargo +1.96.0 check --workspace --all-targets` passes, with and without the `async` feature. Re-check at each release
- [x] `rustfmt.toml`, clippy lints (workspace), `#![forbid(unsafe_code)]` in core. Exceptions for memmap2 in assets are still to be decided in Phase 3
- [x] `deny.toml` (licenses/advisories/bans/sources); `cargo deny check` passes. `cargo-audit` config not added (deny covers advisories)
- [x] `.gitignore` (pre-existing), `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`
- [x] Create `crates/` skeleton (5 crates, compiling). `tests/` dir created (README only, no tests yet)

## 1. Phase 1: Foundation (core + CLI skeleton)
### tpt-press-core
- [x] Error types (`thiserror`): `Io`, `Zstd`, `OutputTooLarge`, `InvalidArgument`
- [x] `Pressable` trait (chunk size, level, overridable `split`)
- [x] `ArchiveFormat` trait (`tpt_press_core::ArchiveFormat`): magic, version and recognition per format. Each crate has a marker (`GenericArchive`, `LogsArchive`, `TensorsArchive`, `AssetsArchive`), and the CLI detects formats through them. Header and index encoding stay in each crate
- [x] Zstd wrapper: levels 1..=22 validated. Dictionary support: `pack --dictionary BYTES` trains on the first 4 MiB and sets header flag bit 0. The dictionary is written after the header, and each frame is compressed with it. Skipped when the sample is too small
- [x] Parallel chunker (rayon): fixed-size chunks, order preserved
- [x] Parallel chunker: content-defined chunking (`Chunking::ContentDefined`, `pack --content-defined`). Gear rolling hash, as in FastCDC, with boundaries between a quarter and four times the average. Frames are unchanged, so no format bump. An insertion moves only nearby chunks (tested: 90% of chunks survive it, against almost none for fixed chunks)
- [x] Streaming Read -> parallel compress -> Write, bounded memory (`archive::pack` / `archive::unpack`, batches of 2 chunks per thread). The in-memory `compress_chunks` API still exists
- [x] Passphrase encryption with `age` 0.12 (`crypto.rs`). Whole archive wrapped in an age stream (age encrypts in 64 KiB authenticated chunks), so the header flag planned earlier is not needed. Encrypted archives are detected by age's magic. Work factor on decrypt is capped at 20 (~1 GiB). Recipient keys: `pack --recipient` and `--recipient-file` encrypt to age X25519 public keys, `unpack` and `inspect` take `--identity`, and `keygen` writes an identity file. A key-encrypted archive cannot be opened with a passphrase, and the reverse holds
- [x] Tampering and wrong passphrase are detected. Wrong passphrase gives "decryption failed"; damage gives "archive is damaged or was altered"
- [x] Checksums per chunk (CRC32 over compressed payload, verified before decompression)
- [x] Decompression-bomb guard (per-chunk output cap) and chunk size limits. Archive-wide limit: `UnpackOptions::max_total_size` (`unpack --max-output`), off by default
- [x] Unit tests (codec, chunker, format header/frame). Integration tests in `crates/tpt-press-core/tests/archive.rs` (13: round-trips, corruption, truncation, limits). Property tests in `crates/tpt-press-core/tests/properties.rs` (3: round-trip, corruption, truncation)
- [x] Truncated archives return `Error::Truncated` ("archive is truncated")
### tpt-press-cli (skeleton)
- [x] clap setup with `pack`, `unpack` and `inspect`. `query` not yet
- [x] Generic `pack`/`unpack` using core. Deviation from spec: no `<format>` argument yet, so `pack <input> <output>` and `unpack <archive> <output>`. `unpack` takes a file, not a directory
- [x] indicatif progress bars on pack and unpack (hidden when not a terminal). Final output is one summary line
- [x] CLI tests in `crates/tpt-press-cli/tests/cli.rs` (5): round-trip, corrupt and truncated archives leave no output, missing input, bad level

## 2. Phase 2: Logs (tpt-press-logs)
- [x] Archive format: magic `TPLG` v2, header (format, timestamp field, dictionary, CSV header), CRC32 per block, index and footer at the end. Layout is documented at the top of `crates/tpt-press-logs/src/format.rs`; block layout in `crates/tpt-press-logs/src/block.rs`
- [x] JSONL and CSV parsers. CSV handles quoted fields with commas. Records must be single lines, so a CSV field cannot contain a newline. Blank lines are dropped, and CRLF is accepted. CSV rows must match the header's field count, and JSON lines must be objects (errors give the line number)
- [x] Sample first 10% -> train zstd dictionary from the sample's cells (64 KiB max). Too-small samples write no dictionary
- [x] Store dictionary in archive; compress blocks with it (256 KiB blocks, parallel with rayon)
- [x] Columnar extraction. Each block stores one zstd stream per column, with a directory at its start. A query decompresses only the columns it needs, plus the timestamp column when it has a time range. On the 1M-record sample, `--columns timestamp,level` over a one-hour window decompressed 230 columns, against 345 for whole records
- [x] Block-level time-range index. Each block stores its min and max timestamp, and records without one are always read
- [x] Query API: `--from/--to` (RFC 3339), `--columns` (in order). JSONL keys missing from a record output as `null`
- [x] CLI `pack-logs` and `query` (records to stdout, summary to stderr). Deviation from spec: `pack-logs <input> <output>` and `query <archive>` rather than `pack <format> ...`
- [x] Optional `strata` feature: `query --sql` via tpt-strata. The crate is `tpt-solutions/tpt-strata` on GitHub, which is not on crates.io, so it is a git dependency pinned to commit `0439970`. Build with `--features strata` (`tpt-press-logs` and `tpt-press-cli`). Types are inferred per column, and all-null columns are left out. The result is held in memory. The engine's own limits apply (no OR, IN, LIKE, or timestamp type). See the publishing note in section 6
- [x] Benchmarks: ratio with vs without dictionary. `pack-logs --no-dictionary` added for this. On the 57.7 MB generated corpus the dictionary made the archive 7.7% larger and the pack slower (median of three). See `docs/benchmarks.md`
- [x] Tests proving query decompresses only relevant blocks (`time_range_skips_blocks_it_does_not_need`, and on a 1M-record log a one-hour window read 115 of 317 blocks)
- [x] Logs archives support encryption: `pack-logs --encrypt`, `--passphrase-file`, `--recipient`, `--recipient-file`. `query` and `inspect` take `--passphrase-file` or `--identity`. Decision taken: an encrypted archive is decrypted into memory for a query, and nothing is written to disk. Memory use is about the archive size, so very large logs need that much RAM
- [x] `inspect` works on logs archives: format, record and block counts, columns, time range, dictionary size, and ratio. It reads block directories, not records. Its ratio compares column data to the archive, so it differs from the ratio `pack-logs` prints, which compares the original file. `--chunks` is refused for logs
- [x] `unpack` does not apply to logs archives. Use `query` to read them back. Decided: by design, since a logs archive is a set of columns, not files
- [x] Output is rebuilt from columns. Requested columns keep their order. Whole JSON records have keys sorted, and whitespace is normalized, so the text can differ from the input. The values are the same. Storing the raw line would restore byte-exact output but roughly doubles storage. CSV rows are re-quoted, so quoting can differ too
- [x] Measured on the 1M-record sample: archive 1.97 MB → 0.85 MB (42x → 97x) after the columnar change

## 3. Phase 3: Assets (tpt-press-assets)
- [x] `.tpa` format spec v1 (little-endian, magic `TPAS`, version, footer `TPAX`). Layout is documented at the top of `crates/tpt-press-assets/src/format.rs`
- [x] Page-aligned (4 KB) payloads. One payload per asset. Large assets are not split into chunks yet
- [x] Sorted index at end of file, O(log n) lookup by binary search. Deviation from spec: a sorted array, not a B-tree. It gives the same lookup cost and is simpler to check. Index entries are read into memory on open
- [x] Bounds-checked mmap reader (memmap2), zero-copy slices. The one `unsafe` call is in `reader.rs` and has a safety note. The crate is `deny(unsafe_code)` otherwise
- [x] Stored vs compressed entries. Stored assets are used in place. Compressed ones are decompressed into memory. The packer stores an asset raw when compression does not shrink it. `--store` forces raw
- [x] Windows/Linux mmap behavior tests. Windows is run locally, and Linux (Ubuntu 24.04 under WSL2) passes the full suite and clippy. macOS is covered by the CI workflow but has not been run. Measured read speeds differ by platform: on Linux, zero-copy reads of all 105 MB took 3.5 ms against 9.6 ms for `fs::read`, but on Windows the full read was slower with zero-copy
- [x] CLI `pack-assets`, `list-assets` (with `--verify`), `unpack-assets`. Deviation from spec: subcommands for assets rather than format detection. Archive magic is checked, so `inspect`/`unpack` do not handle `.tpa` files
- [x] Unsafe names (absolute, `..`, backslashes, drive letters) are rejected when packing and when opening, so unpack cannot write outside its directory
- [x] Benchmark (`crates/tpt-press-assets/examples/read_bench.rs`), not criterion. Results on Windows, warm cache: reading all 105 MB, zero-copy 34.7 ms, decompressed 15.0 ms, `fs::read` 20.9 ms. Reading the first 4 KB of each asset, zero-copy 0.9 ms, `fs::read` 20.5 ms. Zero-copy wins for partial reads. For full reads it is slower here because each page costs a fault. No ZIP comparison yet
- [x] Fuzz the index/header parser, property-based (proptest, runs on Windows): `crates/tpt-press-assets/tests/hostile_input.rs`. Covers random bytes, damaged archives, and damaged indexes with a recomputed checksum. Coverage-guided fuzzing with cargo-fuzz is still open (see Cross-cutting)

## 4. Phase 4: Tensors (tpt-press-tensors)
- [x] Archive format v1 (`TPTN`, index and footer at the end). Layout is documented at the top of `crates/tpt-press-tensors/src/format.rs`. Sharding is not done: one archive per model, with no shard files
- [x] safetensors parser: header checks, dtype sizes, shape against byte range, sorted tensor names. Also a writer, used by `unpack-tensors`
- [x] GGUF parser (versions 2 and 3). Reads metadata, tensor directory and alignment. Packs with `pack-tensors` (detected from the file's first bytes). Plain types and the Q4/Q5/Q8 and K-quant block types are sized. Other GGML types are refused. Tested on synthetic files built from the GGUF layout, not on files from llama.cpp. Metadata arrays are stored as their length only. GGUF archives cannot be unpacked to safetensors while they hold quantized tensors
- [x] ONNX parser. Reads the protobuf wire format directly, without `prost`. Weights are the graph's initializers, stored inline in `raw_data`. Their file locations are recorded, not read, so packing streams them like safetensors. Refused: external data, typed `*_data` fields, and string tensors. Tested with hand-encoded protobuf files, not with real ONNX models. `pack-tensors` detects it (GGUF by magic, then safetensors, then ONNX). Also in the fuzz target and the random-input tests
- [x] PyTorch pickle: `torch.save` zip and legacy checkpoints are read by a restricted pickle interpreter (`pickle.rs`, `pytorch.rs`). Only a fixed set of opcodes runs, and only allowlisted globals are called. Other values are refused rather than dropped. Each tensor must be contiguous in an uncompressed storage. Tested with hand-built checkpoints, since torch is not installed here. Tested on real checkpoints in both formats: the zip format on `hf-internal-testing/tiny-random-gpt2`, and the legacy format on `sshleifer/tiny-gpt2`. In each, the embedding tensor matches the original file byte for byte. The legacy non-zip format is read too, and was checked against `sshleifer/tiny-gpt2`
- [x] Tensor-boundary chunking. Tensors are split only at element boundaries, and pieces are at most 16 MiB. No frame mixes two tensors. Pieces are zstd, or raw when compression does not shrink them
- [x] Metadata-only read: opening an archive reads only the footer and index. `inspect` reports tensors, parameters, dtypes and metadata without touching the data
- [x] tokio async streaming (feature `async`): `AsyncArchive` reads the footer and index by seek, then fetches each piece by range. Sharding: `pack-tensors --shard-size` writes a manifest and shards, and `ShardedArchive` reads them asynchronously, one range-readable source per shard. The network transport is left to the caller, who supplies any `AsyncRead + AsyncSeek`
- [x] Integration test: load into a mock GPU buffer chunk-by-chunk (`loads_a_tensor_chunk_by_chunk_into_a_device_buffer`), with a 2-piece tensor
- [x] Measured: 200 MB single tensor, raw random data. Pack 501 ms, unpack 732 ms, `inspect --verify` ok. Data section identical after round-trip. Compressible weights: see the bf16 checkpoint measurement in `docs/benchmarks.md` (28% saved)
- [x] CLI `inspect` (`--tensors` lists each tensor) and `query <archive> <name>` (writes one tensor's raw bytes, to stdout or `--output`), for tensor and sharded archives

## 5. Cross-cutting
- [x] `inspect` command for the generic format: totals, ratio, chunk map (`--chunks`), checksums verified, payloads not decompressed. Domain metadata (tensors, logs) still to come
- [x] Format auto-detection in CLI. `inspect` identifies all four archive types from their magic (`--verify`, and the `--chunks` refusal, apply to each). `unpack` on a logs, tensors or assets archive fails and names the subcommand to use. `pack` still takes a subcommand per input kind
- [x] Fuzz targets (cargo-fuzz) for the readers: `core_unpack`, `logs_query`, `tensors_read`, `assets_open`, `tensors_pytorch`, `tensors_manifest` in `fuzz/`. Run under nightly on Linux (WSL). Each ran about 90 s with no crashes: 310k, 715k, 4.6M and 928k inputs. Property tests also cover each crate (`tests/hostile_input.rs`). Not run in CI, and no seed corpus yet. Earlier review fixed two allocation risks: an asset's declared size and a logs block's declared length are now checked before allocation
- [x] Memory-safety review of all `unsafe` (mmap). One `unsafe` block in the workspace, in `tpt-press-assets/src/reader.rs`. Sound under its documented precondition (file not modified while mapped). See `docs/unsafe-review.md`
- [x] Cross-crate integration tests in `tests/`. The directory is now a workspace member, `tpt-press-tests`, so `cargo test --workspace` runs its six tests. They cover round trips across crates, format recognition, sharded and async reads, and encryption
- [x] Rustdoc for all public APIs; examples. `cargo doc --workspace --no-deps` has zero warnings, and `missing_docs` is enforced by clippy. Runnable examples: `tpt-press-core/examples/round_trip.rs`, `tpt-press-logs/examples/query_time_range.rs`, `tpt-press-tensors/examples/shard_model.rs`
- [x] Format specs documented in `docs/formats.md` (all four archive types, plus the age wrapper). Checked against each crate's `format.rs`

## 6. Phase 5: Polish & release
- [~] README with benchmarks vs tar.gz, zip, plain zstd. Done: zip (deflate 6 and 9) and plain zstd (levels 3 and 19) are measured in `docs/benchmarks.md` on a seeded 57 MB log corpus, with `tpt-press-cli/examples/compare_formats.rs`. The README links there. Still open: multi-run variance. Every row is one run, so differences under about 10% are noise
- [x] GitHub Actions (`.github/workflows/ci.yml`): fmt, clippy and tests on Windows, Linux and macOS, plus cargo-deny. Written but not yet run on GitHub. Now also runs clippy and tests with the `async` feature, and has an MSRV job (Rust 1.96.0, `cargo check`)
- [~] Release automation / CHANGELOG. `CHANGELOG.md` is written by hand. `.github/workflows/release.yml` runs on version tags: it tests, builds the CLI on three OSes, and uploads the binaries. It does not publish. Not run on GitHub yet
- [ ] Publish crates in order: core -> logs/assets/tensors/genomics/arrays -> cli. **Blocked for logs and cli:** crates.io rejects git dependencies, even optional ones, and `tpt-press-logs` and `tpt-press-cli` both depend on tpt-strata from git. Either publish `tpt-strata` to crates.io first (it has no external dependencies), or drop the `strata` feature before publishing. genomics and arrays have no git dependencies. Recommendation on record: keep the `strata` feature and publish `tpt-strata` first. Not done here: publishing was excluded for this round
- [ ] Tag v0.1.0. Not done here, same reason

## 7. Future domains (post-v0.1)
Non-goals: no image/audio/video recompression. ML datasets store image blobs opaquely.

### 7a. Time-series & metrics (extend `tpt-press-logs` or sibling `tpt-press-timeseries`)
- [x] Decide: new input format in logs vs separate crate. Decided: in logs. It already has the time index, columnar blocks and query path, and a new format needs only a parser and a cell mapping
- [~] Parsers: Prometheus text done (`src/prometheus.rs`, `--format prometheus`, `.prom` files). OpenTelemetry logs in OTLP JSON done (`src/otlp.rs`, `--format otlp`, several records per line). Not done: Prometheus remote-write (protobuf and snappy), and OTLP metrics and traces. CSV with a timestamp column already works
- [x] Delta-of-delta timestamp encoding. `tpt-press-logs/src/gorilla.rs` (Gorilla bit scheme, with tests). Beats zstd on jittered and exact timestamps. Wired into the logs format (version 3): a timestamp column uses it when every cell is canonical and it compresses smaller. 19% smaller on a 200k-record sample, round trip exact
- [~] XOR (Gorilla-style) float encoding. Implemented in `tpt-press-logs/src/gorilla.rs`, round-trips NaN, infinities and signed zeros. Measured: worse than plain zstd on quantised readings (27.7 against about 5.3 bits per value), and worse than shuffle-plus-zstd on full-precision floats. Decided: floats are not wired into logs. Logs store cells as text, so the float codec does not apply there, and per-column float choice is already done in arrays (see 7f). Closed as no-op
- [ ] Per-column dictionaries for labels/tags
- [x] Reuse block/column directory and time-range index from `logs` (block.rs, inspect.rs). The Prometheus parser feeds the same blocks and index
- [~] Query by time range and series/label; `inspect` support. Done: time range, and label/field equality with `--where COLUMN=VALUE` (ANDed, repeatable). Not done: a series index that avoids reading blocks for a label, and `inspect` by label
- [~] Benchmarks vs plain zstd and Parquet. zstd and the shuffle are measured in `docs/benchmarks.md`. Parquet is not measured, since no Parquet tool is installed here

### 7b. ML datasets (`tpt-press-datasets`, builds on `.tpa` + tensors streaming)
- [~] Depends on Phase 3 (`.tpa` index/mmap) and Phase 4 (async sharded streaming). Not used. The dataset format is its own `.tpds` layout, which reads any `Read + Seek` source. Async reads are done with `AsyncDataset`, and the `.tpa` mmap path is not used
- [x] Shard format: WebDataset-style samples grouped by key, random access by sample index (`crates/tpt-press-datasets/src/format.rs`)
- [x] Store already-compressed media (JPEG/PNG/etc.) as-is; compress only text/metadata/tensors. Decided by member extension, so media is never tried with zstd
- [x] Deterministic shuffling-friendly index (`shuffled_order`, SplitMix64 and Fisher-Yates); per-member CRC32 and an index checksum. Multi-file shard sets are not done: one archive per shard, and the caller splits the tar
- [x] Network/streamed shard reading. `AsyncDataset` (feature `async`, tokio) reads any `AsyncRead + AsyncSeek` source by range: header, footer and index, then one seek and read per member. Tests run in CI with the async feature
- [x] Import from WebDataset tar (streaming, `import-dataset`) and Hugging Face-style folders (`metadata.jsonl` plus files, `import-hf`). Paths that leave the folder are refused
- [x] Benchmarks: sample throughput vs tar shards (`examples/random_access.rs`, `docs/benchmarks.md`). Archive is 97.2% of member bytes against 130.6% for tar. Random reads are noisy across runs, and the two sides do different work, so no speed claim is made

### 7c. Genomics (`tpt-press-genomics`; needs domain review)
- [x] FASTA/FASTQ parsers (split into ID, sequence, quality columns). `crates/tpt-press-genomics`, format `TPGN` v1, documented in `docs/formats.md`. Strict: refuses input it cannot return byte for byte, and names the line
- [~] Sequence context model. Done: bases are coded as two binary decisions, with seven context orders (2 to 16) mixed by an integer-only logistic mixer. E. coli K-12 codes at 1.92 bits per base, against the 2-bit floor of 2.00 and zstd -19's 2.25. The 2-bit packing stays as a fallback, and the writer keeps the smaller. A reverse-complement update and a secondary estimation stage were both tried, and neither paid for itself on E. coli (0.07% and none). The numbers are in `docs/benchmarks.md`. Speed is about 3 MB/s, which is the next thing to improve. Real bacterial and eukaryotic data may behave differently, and has not been tested
- [~] Quality-score modelling. Done: a context-mixed coder over the previous one to three symbols and the position. On a synthetic quality process it is about 1% above that process's empirical entropy, and 22% below zstd -19 on the same qualities. It is not yet measured on real FASTQ, and none is available here
- [ ] VCF as a stretch goal
- [x] Region/read-range index for random access. `fetch` finds a record by name through a sorted name-hash lookup stored in the index, reads only the block that holds it, and returns a 1-based inclusive range. A record is never split across blocks, so a block can be large. Block size trades archive size against how much `fetch` decodes
- [~] Compare against bgzip and CRAM. Done: gzip, zip and plain zstd on E. coli and on a 464-contig split (`docs/benchmarks.md`). Not done: bgzip and CRAM, which are not installed here
- [ ] Find a domain reviewer before committing to the format. **Open.** Version 1 was written without one, so the layout is not yet reviewed by someone who works with sequence data
- [ ] SIMD-friendly parts: 2-bit packing, FASTQ column split and quality-score modelling (see 7g)

### 7d. Prerequisites that make any new domain cheaper
- [x] `ArchiveFormat` trait (done in Phase 1)
- [x] CLI format auto-detection (done in Section 5)
- [x] Content-defined chunking (done in Phase 1)

### 7e. Lossless model-weight compression (extend `tpt-press-tensors`)
Lossless only: output is byte-identical after unpack. Lossy quantization is a non-goal.
- [x] Literature check first: ZipNN, DFloat11 and newer work, so we build on published results. Findings in `docs/literature.md`: the exponent carries the savings, published gains are about 30 to 33% on BF16, and the 25 to 35% target is reasonable
- [~] Float-aware codec for bf16/fp16/fp8. fp16 field split tried and removed: 5.7% larger than the shuffle on synthetic fp16 (`docs/benchmarks.md`). Real fp16 weights still need measuring. Done: a byte-plane shuffle with zstd (piece code 2) for 2, 4 and 8-byte elements, chosen per piece when it is smaller. It saves 28% on SmolLM2-135M (plain zstd: 22%), and round-trips exactly. Done: the exponent/mantissa split for `BF16` (piece code 3, `src/split.rs`). Each plane is entropy-coded with zstd, which uses Huffman for literals, not a separate ANS or Huffman coder. Synthetic bf16: 70.2% against 71.0% for the shuffle. Not measured on real checkpoints yet. Not done: fp16 (its 5-bit exponent does not fit the same byte split), and fp8. Keep the raw fallback when it does not shrink
- [~] Benchmarks on real bf16 checkpoints (Llama-class) vs plain zstd; put numbers in the README. Done for SmolLM2-135M, a Llama-style model, in `docs/benchmarks.md`. Synthetic Gaussian bf16 is also measured in `docs/benchmarks.md` (generic compressors save about 21%, `pack-tensors` 22 to 23%), but no real checkpoint is available here. Also closes the Phase 4 note "compressible weights not measured yet"
- [ ] Training checkpoints: fp32 master weights, Adam moments (`m`, `v`), step counter, scheduler and RNG state. Delta against the previous checkpoint plus the float codec. Lossless only, since a resumed run must be exact. Needs PyTorch/DeepSpeed/safetensors checkpoint layouts (depends on the open PyTorch pickle reader in Phase 4). Benchmark on a real small-model run
- [x] Delta compression against a base model (`pack-tensors --base`, piece code 4). Fine-tunes stored as XOR against the base, with the base identity recorded. Not done: dedup of identical tensors across archives, and an async reader for delta archives. Measured on synthetic data only (see docs/benchmarks.md)
- [ ] Entropy coding for quantized formats: GGUF scales, zero-points, indices and higher-precision tensors (small gain, cheap once the float codec exists)
- [ ] Stretch: streaming decode at load time (layer by layer) so run-time memory also shrinks, as DFloat11 shows for bf16. Needs fast SIMD/GPU decoders. Only after the float codec and benchmarks show strong numbers

### 7f. Scientific arrays (`tpt-press-arrays`; needs domain review)
- [x] Decide scope: lossless first (float byte-shuffle or XOR-delta, then zstd). Done: only lossless storage is implemented. Lossy is not implemented, and is not planned as a default
- [~] Input formats: raw n-dim arrays, NumPy `.npy`, single-array and multi-array `.npz`, and Zarr v2 directory stores are done (`crates/tpt-press-arrays`, format `TPAR` v1, with multi-array `.npz` as a `TPBN` bundle). Multi-array design: each member is a full `TPAR` archive, so one array is still read the same way. Zarr: C order, no filters, `null` and `zstd` compressors, with unit tests in `zarr.rs`. Not done: HDF5/NetCDF, and Zarr filters and Blosc. Fortran order and big-endian data are refused
- [x] Chunked layout with a per-chunk index for random-access slicing. Chunks are stored at their real size at the edges. `read_slice` decodes only the chunks it touches
- [x] Predictors. Done: XOR and subtraction deltas along the last axis or the row above, and the Lorenzo predictor over up to four axes, each with byte shuffle. The writer picks the filter per chunk from seven. Subtraction wins on smooth floats and slowly changing integers. Lorenzo wins on a 2-D integer image. Not done: a choice of axis other than the last two for the delta filters
- [~] Benchmarks vs Blosc, zstd with shuffle, and Zarr defaults. Done against gzip, zip and plain zstd, on a synthetic float field (`docs/benchmarks.md`). Not done: Blosc and Zarr, which are not installed here, and any real dataset
- [ ] Find a domain reviewer before fixing the format. **Open.** Version 1 was written without one

### 7g. tpt-simd integration (optional)
Optional `simd` cargo feature. A scalar fallback always exists, and results must be bit-identical to it.
- [ ] Float codec: exponent/mantissa split and bf16/fp16 handling via `tpt-simd-half` plus `tpt-simd-permute`/`tpt-simd-transpose` (byte-plane shuffle)
- [ ] Delta and XOR-delta coding (checkpoints, time-series, arrays) via `tpt-simd-bits`
- [ ] 2-bit nucleotide packing and unpacking (genomics) via `tpt-simd-bits` and `tpt-simd-permute`
- [ ] Newline/record splitting for FASTQ and logs via `tpt-simd-search`
- [ ] Per-chunk CRC32 via `tpt-simd-hash` (core uses a scalar CRC32 today)
- [ ] Decide: dependency on a pre-release crate (path vs published). `unsafe` must stay inside tpt-simd so `forbid(unsafe_code)` in core still holds. Decision on record: no dependency for now. The scalar code is the only implementation, and nothing here needs SIMD to be correct. Revisit once there is a benchmark that shows a gap
- [ ] Benchmark that SIMD beats the compiler's autovectorized scalar code before keeping each kernel
