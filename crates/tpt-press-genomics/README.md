# tpt-press-genomics

Lossless archives for FASTA and FASTQ sequence files, for
[tpt-press](../../README.md). Bases are coded with a context-mixing model, and
records can be fetched by name without unpacking the rest of the file.

Use it for sequencing data you keep and look up: reference genomes, assemblies,
read sets. Pack once, then fetch one contig or one read, or a range inside it,
and only the block that holds it is decoded.

> **Status:** pre-0.1. Format version 1, which may change before 1.0. Its
> context models are tuned on one real genome, and the FASTQ quality numbers
> come from synthetic data. Nothing is published to crates.io yet.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [What is accepted](#what-is-accepted)
- [Fetching records](#fetching-records)
- [How it compresses](#how-it-compresses)
- [The archive format](#the-archive-format)
- [Results](#results)
- [Limits](#limits)
- [Command line](#command-line)
- [Examples](#examples)
- [License](#license)

## What it provides

| Item | What it is for |
|------|----------------|
| `pack(input, output, &PackOptions)` | Pack a FASTA or FASTQ file. Input is `BufRead` |
| `PackOptions { block_bases, level }` | Bases per block (default 2^20) and the zstd level for the side streams |
| `unpack(input, output)` | Restore the original file, byte for byte |
| `fetch(input, id, range, output)` | Write one record, or a 1-based inclusive range of it, by name |
| `inspect(input)` | A `Summary`: kind, record count, bases, blocks and size |
| `Kind` | `Fasta` or `Fastq` |
| `Stats`, `FetchStats` | Counts from a pack, and the cost of a fetch |

## Installation

```toml
[dependencies]
tpt-press-genomics = { path = "crates/tpt-press-genomics" }
```

Once published, `cargo add tpt-press-genomics`. The minimum supported Rust
version is 1.96.

## Quick start

```rust
use std::io::Cursor;
use tpt_press_genomics::{fetch, pack, unpack, PackOptions};

let fasta = std::fs::read("genome.fna")?;

let mut archive = Vec::new();
let stats = pack(Cursor::new(&fasta), &mut archive, &PackOptions::default())?;
println!("{:.3} bits per base", stats.output_bytes as f64 * 8.0 / stats.bases as f64);

// The whole record, with its header line.
let mut record = Vec::new();
fetch(Cursor::new(&archive), "chr1", None, &mut record)?;

// Bases 1,001 to 1,100 of that record.
let mut part = Vec::new();
fetch(Cursor::new(&archive), "chr1", Some((1_001, 1_100)), &mut part)?;

// The whole file, byte for byte.
let mut restored = Vec::new();
unpack(Cursor::new(&archive), &mut restored)?;
assert_eq!(restored, fasta);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`fetch` writes the header line and then the sequence, wrapped as the file was.
For a range, the output starts with the header and holds only the requested
bases.

## What is accepted

The parser is strict, because the archive has to give back the input exactly.
It refuses, and names the line, for:

- CRLF line endings, and a missing final newline
- Sequence lines whose width changes inside a record (FASTA)
- Blank lines inside a record
- A FASTQ `+` line that is neither bare nor a repeat of the id

Input must start with `>` (FASTA) or `@` (FASTQ). Any other bytes are refused.
Other bytes in a sequence are kept as exceptions: lowercase is stored as case
runs, and characters other than A, C, G and T as runs, so soft-masked and
ambiguous sequence round-trips.

A record's name is the first whitespace-delimited word of its header line.

## Fetching records

`fetch` looks the name up in a sorted table of name hashes, then reads and
decodes only the one block that holds the record. A candidate is confirmed by
its name, so a hash collision cannot return the wrong record. If two records
share a name, the first one wins.

The range is 1-based and inclusive, as in `samtools faidx`. `FetchStats` reports
the record's length and the number of blocks read, which is one unless two names
share a hash.

Block size is the trade-off. `fetch` decodes whole blocks, so a smaller
`block_bases` means less work per fetch, for a little more archive size. Choose
it by which matters more for your data: archive size or fetch latency.

## How it compresses

A block splits its records into columns: identifiers, lengths, line widths, case
runs, non-ACGT runs, bases, and FASTQ qualities. The small columns are zstd
frames. The two large ones are coded as follows:

- **Bases.** Each base is two binary decisions, each predicted by seven context
  models over the previous 2 to 16 bases, combined by a logistic mixer. The
  arithmetic is integer-only, so every platform decodes the same. The writer
  keeps the smaller of this and plain 2-bit packing.
- **Qualities.** A bit-tree coder over the bytes used, with four context models
  over the previous symbols and the position in the record. Stored raw when
  that is smaller.

Both coders are deterministic and checked by round-trip tests.

## The archive format

| Field | Value |
|-------|-------|
| Magic | `TPGN` |
| Version | 1 |
| Footer magic | `TPGX` |
| Blocks | Eight streams each, most of them zstd frames. Each block has a CRC32, held in the index |
| Index | Record and base totals, one entry per block, and a sorted name lookup |

The layout is documented at the top of [`src/format.rs`](src/format.rs), and in
full in [`docs/formats.md`](../../docs/formats.md#genomics-archive-tpgn-version-1).

## Results

From [`docs/benchmarks.md`](../../docs/benchmarks.md).

On the E. coli K-12 MG1655 reference (`NC_000913.3`, 4.7 MB, one record), a real
genome:

| Tool | Size | Ratio |
|------|-----:|------:|
| `pack-genomics` | 1,114,813 | 4.22x |
| plain zstd, level 19 | 1,305,697 | 3.60x |
| gzip -9 | 1,379,130 | 3.41x |

The archive codes bases at 1.92 bits per base, against the 2.00 floor of 2-bit
packing. The mixed coder runs at about 3 MB/s, against about 11 MB/s for 2-bit
packing. That is the price of the smaller archive.

On a synthetic FASTQ with 60,000 reads, the quality coder came within about 1%
of the entropy of the generating process. zstd -19 spent about 29% more on the
same qualities. **Those reads are synthetic**, so treat that as the coder's
behaviour on that process, not a claim about sequencer output.

**Not measured:** bgzip, CRAM, and genomes with N runs or soft-masked
lowercase. Those paths are covered by tests, not benchmarks.

## Limits

- Format version 1 is not stable until 1.0.
- The whole input is read through `BufRead`, one record at a time. Memory use is
  about one block, not the file.
- A record is never split across blocks, so one very long record makes one very
  long block. A fetch of a record in that block decodes all of it.
- Qualities have only been measured on synthetic data.

## Command line

```sh
tpt-press pack-genomics genome.fna genome.tpgn          # FASTA or FASTQ
tpt-press fetch-genomics genome.tpgn chr1 --start 1000 --end 1100
tpt-press unpack-genomics genome.tpgn genome.out.fna
tpt-press inspect genome.tpgn
```

## Examples

| Example | Shows |
|---------|-------|
| [`fasta_round_trip`](examples/fasta_round_trip.rs) | Pack FASTA, check the bits per base, unpack, and see a refused CRLF file |
| [`fetch_record`](examples/fetch_record.rs) | Fetch a record and a range by name, check the range against the whole record, and see a missing name |
| [`fastq_round_trip`](examples/fastq_round_trip.rs) | Pack FASTQ with qualities, and check the round trip |

```sh
cargo run -p tpt-press-genomics --example fetch_record --release
```

## License

Licensed under either of [Apache License 2.0](../../LICENSE-APACHE) or
[MIT license](../../LICENSE-MIT), at your option.
