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
