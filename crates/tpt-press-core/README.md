# tpt-press-core

The shared foundation of [tpt-press](../../README.md). It provides the generic
archive format, parallel chunking, zstd and dictionary helpers, passphrase and
public-key encryption, and the error types that every other tpt-press crate
uses.

Use it directly when you want compressed, optionally encrypted archives of
arbitrary bytes. Use one of the domain crates (`tpt-press-assets`,
`tpt-press-tensors`, `tpt-press-logs`, `tpt-press-genomics`,
`tpt-press-arrays`) when your data has structure that a generic compressor
cannot see.

> **Status:** pre-0.1. The on-disk format is version 1 and may change before
> 1.0. Nothing is published to crates.io yet.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [The archive format](#the-archive-format)
- [Options](#options)
- [Encryption](#encryption)
- [Chunking for domain crates](#chunking-for-domain-crates)
- [Safety limits](#safety-limits)
- [Examples](#examples)
- [Module map](#module-map)
- [License](#license)

## What it provides

| Area | Items | What it is for |
|------|-------|----------------|
| Archive | `pack`, `unpack`, `archive::inspect`, `PackOptions`, `UnpackOptions`, `Stats`, `Inspection` | Compress a stream into a `TPPR` archive and back |
| Chunking | `Chunking::{Fixed, ContentDefined}`, `Pressable`, `compress_chunks`, `decompress_chunks` | Split input into chunks, and compress them in parallel |
| Codec | `codec::compress`, `codec::decompress`, `codec::train_dictionary` | Thin wrappers over zstd, with level checks |
| Encryption | `crypto::pack_encrypted`, `crypto::pack_for_recipients`, `crypto::open`, `crypto::open_with_identities`, `EncryptingWriter` | Encryption by passphrase or to X25519 keys |
| Format trait | `ArchiveFormat`, `GenericArchive` | Lets the CLI recognise each archive type from its magic bytes |
| Errors | `Error`, `Result` | One error type for the whole family |

Everything is `Read`/`Write` based. Nothing needs the whole archive in memory,
although `Vec<u8>` is convenient for examples.

## Installation

Until the crates are published, depend on the workspace path:

```toml
[dependencies]
tpt-press-core = { path = "crates/tpt-press-core" }
```

Once published:

```sh
cargo add tpt-press-core
```

The minimum supported Rust version is 1.96.

## Quick start

```rust
use std::io::Cursor;
use tpt_press_core::{pack, unpack, PackOptions, UnpackOptions};

let data = b"hello, hello, hello, hello".repeat(100);

let mut archive = Vec::new();
pack(Cursor::new(&data), &mut archive, &PackOptions::default())?;

let mut restored = Vec::new();
unpack(Cursor::new(&archive), &mut restored, &UnpackOptions::default())?;
assert_eq!(restored, data);
# Ok::<(), tpt_press_core::Error>(())
```

`pack` returns `Stats` (uncompressed bytes, archive bytes, chunk count), so you
can report a ratio without a second pass.

## The archive format

A generic archive starts with the magic `TPPR` and a version field, and each
chunk is stored as its own zstd frame with a CRC32. The chunk table is at the
end, so `archive::inspect` can list chunk sizes without decompressing anything.
If a dictionary was trained, it is stored once in the header and used by every
chunk.

| Field | Value |
|-------|-------|
| Magic | `TPPR` |
| Version | 1 |
| Chunk codec | zstd, one frame per chunk |
| Integrity | CRC32 per chunk, checked before output is returned |
| Dictionary | optional, flag bit 0 in the header |

The byte layout is in [`src/format.rs`](src/format.rs), and summarised in
[`docs/formats.md`](../../docs/formats.md#generic-archive-tppr-version-1).

## Options

`PackOptions`:

| Field | Default | Meaning |
|-------|---------|---------|
| `chunk_size` | 1 MiB | With `Fixed`, every chunk is this size. With `ContentDefined`, it is the average (a power of two from 256 bytes to 16 MiB). Maximum 64 MiB for `Fixed`. |
| `chunking` | `Chunking::Fixed` | `ContentDefined` uses a gear rolling hash, so chunk boundaries follow the content |
| `level` | 3 | zstd level, 1 to 22 |
| `dictionary` | `None` | Train a zstd dictionary of up to this many bytes from the start of the input. 1 byte to 1 MiB |

The defaults are a reasonable starting point, and the
[options tour](examples/options_tour.rs) shows how each setting changes size on
one input. Measure on your own data before choosing.

`UnpackOptions`:

| Field | Default | Meaning |
|-------|---------|---------|
| `max_chunk_size` | 64 MiB | Largest uncompressed chunk accepted, checked before allocation |
| `max_total_size` | `None` | Largest total output. Set this for archives from untrusted sources |

## Encryption

Two modes, both using ChaCha20-Poly1305 in 64 KiB chunks, with RustCrypto primitives:

- **Passphrase.** `crypto::pack_encrypted(input, output, &opts, passphrase)`.
  Opening needs the passphrase. Argon2id runs with a capped memory cost, so a
  hostile header cannot force gigabytes of allocation.
- **Recipients.** `crypto::pack_for_recipients(input, output, &opts, &[recipient])`.
  Anyone holding a matching X25519 identity can open the archive. No secret is
  shared with senders.

To read, call `crypto::open(reader, passphrase)` or
`crypto::open_with_identities(reader, &identities)`. Both return a plain
`Read` over the decrypted archive, which goes straight into `unpack` or
`archive::inspect`. An unencrypted archive passes through unchanged, so one code
path handles both.

`crypto::looks_encrypted` checks the first bytes without a key. The
`EncryptingWriter` type and `encrypt_with_passphrase` and
`encrypt_for_recipients` encrypt while writing, so no plaintext copy is made.
The CLI uses them to encrypt logs archives, which the logs crate does not do
itself.

See [`examples/encrypted_archive.rs`](examples/encrypted_archive.rs).

## Chunking for domain crates

`Pressable` lets a domain crate decide where chunks end. Its `split` method
has a default of fixed-size chunks, and it can be overridden to split on
content boundaries, such as tensor edges or line ends. `compress_chunks`
then compresses the chunks across cores and keeps their order, and
`decompress_chunks` reverses it, with a bound on each chunk's output.

This is the layer below the archive format. An archive written with
`compress_chunks` is not a `TPPR` archive, and that is intentional. The
[custom chunking example](examples/custom_pressable.rs) splits text at line
breaks and checks the round trip.

## Safety limits

Archives can come from untrusted sources, so every size read from one is
checked before it is used:

- Chunk sizes are checked against `max_chunk_size` before allocation.
- `max_total_size` stops unpacking as soon as the output would pass it.
- Encryption work factors are capped at `MAX_WORK_FACTOR` (20).
- Levels outside `1..=22` are rejected.

The crate has property tests, hostile-input tests, and CLI-level tests that
check corrupt and truncated archives leave no output. Fuzz targets for the
readers live in [`fuzz/`](../../fuzz/) at the workspace root.

## Examples

| Example | Shows |
|---------|-------|
| [`round_trip`](examples/round_trip.rs) | The smallest complete pack and unpack |
| [`encrypted_archive`](examples/encrypted_archive.rs) | Passphrase and recipient encryption, and refusals for a missing or wrong key |
| [`options_tour`](examples/options_tour.rs) | Level, chunk size, content-defined chunking and dictionary, measured on one input |
| [`inspect_and_limits`](examples/inspect_and_limits.rs) | Reading the chunk table, and an output limit that refuses an oversized archive |
| [`custom_pressable`](examples/custom_pressable.rs) | Writing a `Pressable` that splits on line breaks |

Run any of them with:

```sh
cargo run -p tpt-press-core --example options_tour --release
```

## Module map

| Module | Contents |
|--------|----------|
| `archive` | `pack`, `unpack`, `inspect`, the option and stats types |
| `archive_format` | The `ArchiveFormat` trait and `GenericArchive` marker |
| `chunker` | `Pressable`, `compress_chunks`, `decompress_chunks` |
| `codec` | zstd wrappers, levels and dictionaries |
| `crypto` | Passphrase and recipient encryption, and `open` |
| `keys` | X25519 `Recipient` and `Identity`, as bech32m strings |
| `split` | `Chunking` and the content-defined chunk limits |
| `error` | `Error` and `Result` |

## License

Licensed under either of [Apache License 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT), at your option.
