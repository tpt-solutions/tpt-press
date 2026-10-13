# tpt-press

Domain-aware compression and archiving for Rust. Unlike generic tools that treat
data as a stream of bytes, tpt-press provides library crates that understand the
structure of the data they compress: game assets, ML model weights, and logs.

> **Status:** early development, pre-0.1. The generic, logs, tensor, assets,
> genomics and arrays formats are implemented and tested. The first four are
> fuzzed on Linux. Archive formats are unstable until 1.0. Nothing is published
> to crates.io yet.

## Crates

| Crate | Purpose |
|-------|---------|
| [`tpt-press-core`](crates/tpt-press-core/README.md) | Shared traits, parallel chunking, zstd wrapper, encryption, errors |
| [`tpt-press-assets`](crates/tpt-press-assets/README.md) | Memory-mapped game asset archives (`.tpa`) |
| [`tpt-press-tensors`](crates/tpt-press-tensors/README.md) | ML model archiving with tensor-aware chunking, sharding and async reads |
| [`tpt-press-logs`](crates/tpt-press-logs/README.md) | JSON/CSV/log archiving with dictionaries and time indexing. `query --sql` with the `strata` feature |
| [`tpt-press-genomics`](crates/tpt-press-genomics/README.md) | Lossless FASTA/FASTQ archiving with context-mixed bases and qualities, and random access by name |
| [`tpt-press-arrays`](crates/tpt-press-arrays/README.md) | Lossless chunked n-dimensional arrays, with `.npy` support and slice reads |
| [`tpt-press-cli`](crates/tpt-press-cli/README.md) | The `tpt-press` command-line tool |

The libraries are the primary product. The CLI is a thin consumer of them. Each
crate has its own README, CHANGELOG, and runnable examples in its `examples/`
folder.

## Command-line use

```sh
tpt-press pack big.bin big.tppr --level 9              # generic, zstd
tpt-press pack-logs app.jsonl app.tplg                 # logs: columnar, time-indexed
tpt-press query app.tplg --from 2026-03-01T00:00:00Z --columns timestamp,level
tpt-press pack-tensors model.safetensors model.tptn    # also PyTorch, GGUF, ONNX
tpt-press pack-tensors model.safetensors shards/ --shard-size 1000000000
tpt-press query model.tptn embed.weight > embed.bin
tpt-press pack-assets assets/ game.tpa --store
tpt-press keygen me.key && tpt-press pack big.bin big.tppr --recipient age1...
tpt-press pack-genomics genome.fna genome.tpgn         # FASTA or FASTQ
tpt-press fetch-genomics genome.tpgn chr1 --start 1000 --end 1100
tpt-press pack-arrays field.npy field.tpar             # .npy keeps its header
tpt-press slice-arrays field.tpar --range 100:110 --range 200:260 > part.bin
tpt-press query app.tplg --sql "SELECT level, COUNT(*) FROM logs GROUP BY level"  # build with --features strata
```

`inspect` works on every archive type, and detects the type from the file.

## Benchmarks

See [docs/benchmarks.md](docs/benchmarks.md). On a 58 MB JSON log file, columnar
`pack-logs` reaches 10x, and `pack --level 9` beats gzip -9 on size at about a
seventh of its time. The default level trails gzip -9 on size. That trade-off is
documented, and the default is left as it is.

On real data, the E. coli reference genome packs to 1.11 MB, about 1.92 bits per
base. That is smaller than gzip -9 (1.38 MB) or zstd -19 (1.31 MB). On a
synthetic NumPy float field, the arrays archive is 103x, against about 1.2x for
the generic tools. That field is smooth and analytic, so it is a best case, and
real data will compress far less.

## Building

Requires a stable Rust toolchain (see `rust-version` in `Cargo.toml`).

```sh
cargo build --workspace
cargo test --workspace
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.

Copyright TPT Solutions.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
