# Changelog

All notable changes to `tpt-press-genomics` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- VCF input (`pack` on a file starting with `#`). The header lines are kept verbatim,
  and the data columns are stored as streams. `unpack` rebuilds the file byte for
  byte. `fetch` is refused on VCF archives.
- `TPGN` archive format, version 1 (footer `TPGX`), for FASTA and FASTQ. The
  layout is documented in `docs/formats.md` and at the top of `src/format.rs`.
- Lossless FASTA and FASTQ archives. Bases are coded with a context-mixing
  model over the previous 2 to 16 bases, or packed at 2 bits when that is
  smaller. FASTQ qualities are coded with a context-mixed coder, or stored raw
  when that is smaller.
- Case runs, non-ACGT runs, line widths and identifiers are kept as separate
  zstd streams, so the round trip is exact.
- `fetch`: reads one record, or a 1-based inclusive range of it, by name. A
  sorted name lookup in the index points at the one block to read.
- `inspect` and `unpack`, which restore the original file byte for byte.
- A strict parser. It refuses input it cannot return exactly, and names the
  line. This covers CRLF endings, a missing final newline, changing line
  widths, blank lines inside a record, and a FASTQ `+` line that is neither bare
  nor the record id.
- `PackOptions::block_bases` (default 2^20) and `PackOptions::level`.
- CLI: `pack-genomics`, `unpack-genomics` and `fetch-genomics` (with `--start`
  and `--end`). `inspect` detects the format.
- Examples: `fasta_round_trip`, `fetch_record` and `fastq_round_trip`.
- Experimental. The layout has not been reviewed by a sequence-data specialist,
  and may change before 1.0.
