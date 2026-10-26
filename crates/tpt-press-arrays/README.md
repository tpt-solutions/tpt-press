# tpt-press-arrays

Lossless chunked storage for n-dimensional numeric arrays, for
[tpt-press](https://github.com/tpt-solutions/tpt-press/blob/master/README.md). An array is split into chunks, and each chunk is
filtered, then compressed. The `.npy` header is kept, so unpacking a NumPy file
gives back the same file, byte for byte. A rectangular slice can be read
without decoding the rest of the array.

Use it for arrays that are large and read in pieces: simulation output, sensor
grids, image stacks, feature tables. It is for exact storage. It is not a lossy
or error-bounded codec.

> **Status:** experimental. Format version 1, which may change before 1.0. The
> layout has not been reviewed by an array-data specialist. Measured on
> synthetic fields only, and the filters have not been tested on real
> scientific data. Nothing is published to crates.io yet.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Input](#input)
- [Chunks and slices](#chunks-and-slices)
- [Filters](#filters)
- [The archive format](#the-archive-format)
- [Results](#results)
- [Limits](#limits)
- [Command line](#command-line)
- [Examples](#examples)
- [License](#license)

## What it provides

| Item | What it is for |
|------|----------------|
| `pack_npy(input, output, &PackOptions)` | Pack a `.npy` file. The header is kept |
| `pack_raw(input, output, descr, shape, &PackOptions)` | Pack headerless bytes, with the dtype and shape given |
| `unpack(input, output)` | Restore the original `.npy` file, or the raw bytes |
| `read_slice(input, &[Range<u64>])` | Read a rectangle. Only the chunks it touches are decoded |
| `inspect(input)` | A `Summary`: dtype, shape, chunk shape, chunk count and sizes |
| `PackOptions { chunk_shape, level, filter }` | Chunk shape, zstd level, and filter choice (`Auto` or `Fixed`) |
| `Filter`, `FilterChoice` | The seven lossless filters, and whether the writer picks one |
| `Dtype`, `NpyHeader`, `parse_npy`, `is_npy` | Reading `.npy` headers on their own |

## Installation

```toml
[dependencies]
tpt-press-arrays = { path = "crates/tpt-press-arrays" }
```

Once published, `cargo add tpt-press-arrays`. The minimum supported Rust version
is 1.96.

## Quick start

```rust
use std::io::Cursor;
use tpt_press_arrays::{pack_npy, read_slice, unpack, PackOptions};

let npy = std::fs::read("field.npy")?;

let mut archive = Vec::new();
pack_npy(Cursor::new(&npy), &mut archive, &PackOptions::default())?;

// Rows 100 to 110 and columns 200 to 260, as raw bytes in C order.
let slice = read_slice(Cursor::new(&archive), &[100..110, 200..260])?;
println!("decoded {} chunks", slice.chunks_read);

// The whole file, back to the original .npy.
let mut restored = Vec::new();
unpack(Cursor::new(&archive), &mut restored)?;
assert_eq!(restored, npy);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`read_slice` returns the elements in C order with no header. Its `shape` field
gives the length of each range, so the bytes can be reinterpreted with the
dtype from `inspect`.

## Input

- **`.npy`**, versions 1 to 3. The dtype and shape come from the header. The
  header is stored verbatim, so unpacking reproduces the file.
- **Raw bytes.** Pass the NumPy `descr` string, such as `<f4`, and the shape.
  Unpacking gives back the bytes, with no header.
- **`.npz`** holding exactly one array is read from its `.npy` member, and the
  archive holds that array. Unpacking gives a `.npy` file, not the zip. An
  `.npz` with several arrays is refused.

Accepted dtypes: little-endian `f2`, `f4` and `f8`, signed `i1` to `i8`,
unsigned `u1` to `u8`, and single-byte `b1`. Fortran order and big-endian data
are refused with an error that says why. Save with C order and native
little-endian. Other dtypes, including complex numbers, are not supported.

## Chunks and slices

The chunk shape is either given (`PackOptions::chunk_shape`) or chosen by the
writer. The default aims at about 1 MiB per chunk, by splitting the first axis.
A chunk at the edge of an axis is stored at its real size, not padded.

A slice read works out which chunks overlap the requested box, decodes only
those, and copies each overlapping run into the output. The index is checked
when the archive is opened. Each chunk is checked by CRC before it is
decompressed.

Chunk shape is the main tuning choice. Chunks that match how the array is read
make slices cheap. Chunks that are too small cost index space and compress
worse. Measure with your own access pattern.

## Filters

Each chunk is filtered before zstd. A filter predicts each element from an
earlier one and stores the difference, then groups the bytes of each element so
that the zero bytes sit together. The filters are:

| Filter | CLI name | What it does |
|--------|----------|--------------|
| `None` | `none` | The bytes as they are |
| `Shuffle` | `shuffle` | Groups the bytes of each element by position |
| `XorShuffle` | `xor` | XOR with the previous element, then shuffle. Suits floats |
| `UpShuffle` | `up` | XOR with the element one row back, then shuffle |
| `Delta` | `delta` | Subtract the previous element, then shuffle |
| `DeltaUp` | `delta-up` | Subtract the element one row back, then shuffle |
| `Lorenzo` | `lorenzo` | Subtract the Lorenzo prediction, the neighbours along each axis with inclusion and exclusion, then shuffle |

`FilterChoice::Auto` (the default) tries all seven on each chunk and keeps the
smallest result. `FilterChoice::Fixed(filter)` uses one filter everywhere, which
is faster to pack. Auto is slower in proportion to the number of filters. All
seven are exact inverses, so the choice never changes the data.

The best fixed filter depends on the data. The
[`raw_with_filter`](examples/raw_with_filter.rs) example compares them on one
input.

## The archive format

| Field | Value |
|-------|-------|
| Magic | `TPAR` |
| Version | 1 |
| Footer magic | `TPRX` |
| Header | Dtype descriptor, shape, chunk shape, and the original `.npy` header (or none) |
| Chunks | One zstd frame each, after the chosen filter, in C order over the chunk grid |
| Index | Offset, length, CRC32 and filter for each chunk |

The layout is documented at the top of [`src/format.rs`](src/format.rs), and in
full in [`docs/formats.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/formats.md#arrays-archive-tpar-version-1).

## Results

From [`docs/benchmarks.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/benchmarks.md). **These are synthetic
inputs.** A smooth analytic field is a best case, so real data will compress
far less.

| Input | Tool | Size | Ratio |
|-------|------|-----:|------:|
| 2000 x 1500 float32 field, smooth | `pack-arrays` (auto) | 116,378 | 103x |
| Same field | plain zstd, level 19 | 9,657,036 | 1.24x |
| Same field | `pack-arrays`, XOR filter only (v1) | 4,110,682 | 2.92x |

Every chunk of the smooth field chose delta with shuffle. On a uniform random
int16 array the ratio was 1.54x, where the data's entropy allows about 1.7x.

## Limits

- **Memory.** Input is read whole, so memory use is about the size of the array.
- **Lossless only.** Error-bounded lossy compression is not provided, and is not
  planned as the default.
- **Formats.** `.npz` with one array only. No Zarr, HDF5 or NetCDF yet.
- **Dtypes.** The list above, and nothing else.

## Command line

```sh
tpt-press pack-arrays field.npy field.tpar              # keeps the .npy header
tpt-press slice-arrays field.tpar --range 100:110 --range 200:260 > part.bin
tpt-press unpack-arrays field.tpar field.npy
tpt-press inspect field.tpar
```

For raw input, `pack-arrays` takes `--dtype` and `--shape`, and `--filter` picks
a filter by its CLI name.

## Examples

| Example | Shows |
|---------|-------|
| [`npy_round_trip`](examples/npy_round_trip.rs) | Pack a `.npy` in small chunks, inspect it, and unpack to the same bytes |
| [`read_slice`](examples/read_slice.rs) | Read a rectangle that crosses chunk edges, check every value, and count the chunks decoded |
| [`raw_with_filter`](examples/raw_with_filter.rs) | Raw input with no header, and the output size for each filter |

```sh
cargo run -p tpt-press-arrays --example read_slice --release
```

## License

Licensed under either of [Apache License 2.0](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-APACHE) or
[MIT license](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-MIT), at your option.
