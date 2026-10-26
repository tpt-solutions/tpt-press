# tpt-press-assets

Game asset archives for [tpt-press](https://github.com/tpt-solutions/tpt-press/blob/master/README.md). An archive packs many
named files into one `.tpa` file. Each asset is stored raw or compressed, and
raw assets are page-aligned, so they can be read straight from a memory map
without being copied.

Use it for the data a game loads at runtime: textures, meshes, audio, levels
and configuration. Load one asset by name, and read only the bytes you need.

> **Status:** pre-0.1. Format version 1, which may change before 1.0.
> Nothing is published to crates.io yet.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Storage modes](#storage-modes)
- [Names](#names)
- [The archive format](#the-archive-format)
- [Performance](#performance)
- [Safety](#safety)
- [Command line](#command-line)
- [Examples](#examples)
- [License](#license)

## What it provides

| Item | What it is for |
|------|----------------|
| `pack(assets, output, &PackOptions)` | Write an archive from an iterator of `(name, contents)` pairs |
| `PackOptions { level, store_all }` | zstd level, and whether to store every asset raw |
| `Stats` | Counts of stored and compressed assets, and sizes |
| `Archive::open(path)` | Memory-map an archive and read its index |
| `Archive::entries()` | Every entry, sorted by name |
| `Archive::get(name)` | Returns `Option<Asset>`. Borrowed for stored assets, owned for compressed ones |
| `Archive::verify()` | Checks every payload against its CRC32 |
| `Asset::is_zero_copy()` | Whether the bytes were borrowed from the map |
| `validate_name` | The name rules used by the writer and the reader |

## Installation

```toml
[dependencies]
tpt-press-assets = { path = "crates/tpt-press-assets" }
```

Once published, `cargo add tpt-press-assets`. The minimum supported Rust
version is 1.96.

## Quick start

```rust
use std::fs::File;
use tpt_press_assets::{pack, Archive, PackOptions};

let assets = vec![
    Ok(("levels/forest.json".to_owned(), br#"{"spawns":[1,4,9]}"#.to_vec())),
    Ok(("textures/grass.raw".to_owned(), vec![0x3c; 64 * 1024])),
];
pack(assets, File::create("game.tpa")?, &PackOptions::default())?;

let archive = Archive::open("game.tpa".as_ref())?;
let level = archive.get("levels/forest.json")?.expect("present");
assert_eq!(&level[..], br#"{"spawns":[1,4,9]}"#);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`pack` takes an iterator of `Result<(String, Vec<u8>)>`, so a caller can read
each file from disk as the packer asks for it. Memory use is then one asset at
a time, not the whole game.

## Storage modes

Each asset is stored in one of two ways:

| Mode | Stored when | Read cost |
|------|-------------|-----------|
| `Stored` (raw) | `store_all` is set, or compression does not make it smaller | No copy. `get` returns a slice into the map |
| `Zstd` (one frame) | Compression makes it smaller | Decompresses into a new buffer on each `get` |

Raw assets start on 4 KiB boundaries (`PAGE_SIZE`), so the slice is aligned to
a page in the map. Without `store_all`, the packer keeps an incompressible asset
raw anyway. The `zero_copy` example shows both cases.

## Names

Names are the keys of the archive, and they come from the caller on pack and
from the file on unpack. The rules are checked on both sides:

- Non-empty, at most `MAX_NAME_LEN` bytes.
- Relative, with `/` as the separator. No leading `/`, no `\`, no `:` and no NUL.
- No empty segment, `.` or `..`.
- Unique within an archive.

Names are sorted in the index, and `get` binary-searches it.

## The archive format

| Field | Value |
|-------|-------|
| Magic | `TPAS` |
| Version | 1 |
| Footer magic | `TPAX` |
| Payload alignment | 4 KiB for stored assets |
| Compression | zstd, one frame per compressed asset |
| Integrity | CRC32 per payload, over the stored bytes |

The index and footer are at the end of the file. Opening reads only the header,
the footer and the index, so listing a large archive is cheap. The layout is
documented at the top of [`src/format.rs`](src/format.rs), and summarised in
[`docs/formats.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/formats.md#assets-archive-tpas-version-1).

## Performance

From the benchmark in the project's TODO list. This is one run on Windows 11
with a warm cache, so treat it as a rough guide.

| Read | Zero-copy (`get`) | Decompressed | `fs::read` |
|------|------------------:|-------------:|-----------:|
| All 105 MB of assets | 34.7 ms | 15.0 ms | 20.9 ms |
| First 4 KB of each asset | 0.9 ms | not measured | 20.5 ms |

Zero-copy wins when you read part of each asset. For a full read it can be
slower, because each page of the map costs a fault. Reproduce it with
`cargo run -p tpt-press-assets --example read_bench --release`.

## Safety

- The crate denies `unsafe` code except for one memory-mapping call in
  `reader.rs`. That call is reviewed in [`docs/unsafe-review.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/unsafe-review.md).
- Every offset and length read from the index is checked against the file
  before it is used. A declared size is checked before any allocation.
- A compressed asset may declare at most `MAX_COMPRESSED_ASSET_BYTES` (1 GiB).
- Names are checked on unpack, so a hostile archive cannot write outside its
  output directory.

Property tests and hostile-input tests cover random bytes, damaged archives and
damaged indexes with a recomputed checksum.

## Command line

```sh
tpt-press pack-assets assets/ game.tpa --store      # pack a folder, store raw
tpt-press list-assets game.tpa --verify              # list and check checksums
tpt-press unpack-assets game.tpa out/                # restore the folder
```

`pack-assets` takes `--level` for compressed assets. See the
[CLI README](https://github.com/tpt-solutions/tpt-press/blob/master/crates/tpt-press-cli/README.md).

## Examples

| Example | Shows |
|---------|-------|
| [`pack_and_read`](examples/pack_and_read.rs) | Pack in-memory assets, list the index, read each one back and verify |
| [`zero_copy`](examples/zero_copy.rs) | Stored and compressed assets side by side, and which reads are zero-copy |
| [`pack_directory`](examples/pack_directory.rs) | Walk a directory tree, and pack it with names relative to the root |
| [`read_bench`](examples/read_bench.rs) | Timing for full and partial reads, zero-copy against `fs::read` |

```sh
cargo run -p tpt-press-assets --example pack_directory
```

## License

Licensed under either of [Apache License 2.0](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-APACHE) or
[MIT license](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-MIT), at your option.
