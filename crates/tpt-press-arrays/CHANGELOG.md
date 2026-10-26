# Changelog

All notable changes to `tpt-press-arrays` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- `TPAR` archive format, version 1 (footer `TPRX`). Each chunk is one zstd frame
  after a filter, in C order over the chunk grid. The layout is documented in
  `docs/formats.md` and at the top of `src/format.rs`.
- `pack_npy`, which keeps the original `.npy` header, so unpacking gives back
  the file byte for byte. `.npy` versions 1 to 3 are read.
- `pack_raw`, for headerless input, with the dtype descriptor and shape given.
- `.npz` files holding exactly one array are read from their `.npy` member.
- `.npz` files holding several arrays are stored as a `TPBN` bundle, with one
  complete `TPAR` archive per member. `read_member` and `bundle_members` read
  them, and `unpack-arrays --member` writes one out.
- Zarr v2 directory stores, through `pack_zarr`: C order, no filters, and the
  `null` and `zstd` compressors. Blosc is refused by name.
- Experimental. The layout has not been reviewed by an array-data specialist,
  and may change before 1.0.
- `read_slice`, which reads a rectangle and decodes only the chunks it overlaps.
  Each chunk is checked by CRC before it is decompressed.
- Seven lossless filters: none, shuffle, XOR with shuffle, XOR with the row above
  and shuffle, delta with shuffle, delta with the row above and shuffle, and the
  Lorenzo prediction with shuffle. `FilterChoice::Auto` keeps the smallest per
  chunk.
- `unpack` and `inspect`.
- Dtypes: little-endian `f2`, `f4`, `f8`, signed `i1` to `i8`, unsigned `u1` to
  `u8`, and `b1`. Fortran order and big-endian data are refused with a reason.
- CLI: `pack-arrays` (with `--dtype`, `--shape`, `--chunk` and `--filter`),
  `unpack-arrays`, and `slice-arrays` (with `--range START:END`). `inspect`
  detects the format.
- Examples: `npy_round_trip`, `read_slice` and `raw_with_filter`.
