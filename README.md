# tpt-press

Domain-aware compression and archiving for Rust. Unlike generic tools that treat
data as a stream of bytes, tpt-press provides library crates that understand the
structure of the data they compress: game assets, ML model weights, and logs.

> **Status:** early development. The workspace skeleton exists; no format is
> implemented yet. Archive formats are unstable until 1.0.

## Crates

| Crate | Purpose |
|-------|---------|
| `tpt-press-core` | Shared traits, parallel chunking, zstd wrapper, errors |
| `tpt-press-assets` | Memory-mapped game asset archives (`.tpa`) |
| `tpt-press-tensors` | ML model archiving with tensor-aware chunking |
| `tpt-press-logs` | JSON/CSV/log archiving with dictionaries and time indexing |
| `tpt-press-cli` | The `tpt-press` command-line tool |

The libraries are the primary product. The CLI is a thin consumer of them.

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
