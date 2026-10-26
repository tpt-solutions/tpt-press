# tpt-press-cli

The `tpt-press` command-line tool, for [tpt-press](https://github.com/tpt-solutions/tpt-press/blob/master/README.md). It is a thin
front end over the library crates: each command calls one of them, and writes
the result to a file or to standard output.

Use it to pack files, inspect archives without unpacking them, query logs, fetch
one record from a genome, or slice one tensor out of a model. The libraries are
the main product. Use them directly when you are writing Rust.

> **Status:** pre-0.1. The command set is stable enough to script, but the
> archive formats may change before 1.0. Nothing is published to crates.io yet.

## Contents

- [Installation](#installation)
- [Commands](#commands)
- [Generic files](#generic-files)
- [Encryption](#encryption)
- [Logs](#logs)
- [Tensors](#tensors)
- [Assets](#assets)
- [Genomics](#genomics)
- [Arrays](#arrays)
- [Inspecting any archive](#inspecting-any-archive)
- [Exit behaviour](#exit-behaviour)
- [Features](#features)
- [Scripts and benchmark tools](#scripts-and-benchmark-tools)
- [License](#license)

## Installation

From the repository:

```sh
cargo install --path crates/tpt-press-cli
```

Or build it and run it from the target directory:

```sh
cargo build --release -p tpt-press-cli
./target/release/tpt-press --help
```

The minimum supported Rust version is 1.96. There are no published binaries yet.
The release workflow in `.github/workflows/release.yml` is written to build the
CLI on Linux, Windows and macOS, but it has not been run on GitHub.

## Commands

| Command | What it does |
|---------|--------------|
| `pack` | Compress any file into a generic archive |
| `unpack` | Restore the original file |
| `keygen` | Create an identity, and print its public key |
| `inspect` | Show an archive's structure and ratio. Detects every archive type |
| `pack-logs` | Compress JSON Lines or CSV into a time-indexed logs archive |
| `query` | Print the records in a time range, with chosen columns, or run SQL |
| `pack-tensors` | Pack a safetensors, GGUF, ONNX or PyTorch model, optionally sharded |
| `unpack-tensors` | Write a tensor archive back out as safetensors |
| `pack-assets` | Pack every file under a directory into an assets archive |
| `list-assets` | List the entries of an assets archive, optionally verifying them |
| `unpack-assets` | Write every asset to a directory |
| `pack-genomics` | Compress FASTA or FASTQ |
| `fetch-genomics` | Print one record by name, or a 1-based range of it |
| `unpack-genomics` | Restore the original FASTA or FASTQ |
| `pack-arrays` | Compress a `.npy` file, or raw bytes with `--dtype` and `--shape` |
| `slice-arrays` | Write a rectangle of an arrays archive to standard output |
| `unpack-arrays` | Restore the array, or the original `.npy` |

Run `tpt-press <command> --help` for every option.

## Generic files

```sh
tpt-press pack big.bin big.tppr --level 9
tpt-press pack big.bin big.tppr --content-defined --chunk-size 65536
tpt-press pack logs.txt logs.tppr --dictionary 110000
tpt-press unpack big.tppr big.bin
```

`--level` is 1 to 22, with a default of 3. `--chunk-size` is the uncompressed
size of each chunk, up to 64 MiB. With `--content-defined`, it is the average.
`--dictionary` trains a zstd dictionary from the start of the input.

## Encryption

Passphrase or public key:

```sh
# Passphrase: prompts, or read it from a file.
tpt-press pack secret.txt secret.tppr --encrypt
tpt-press pack secret.txt secret.tppr --passphrase-file pass.txt
tpt-press unpack secret.tppr secret.txt            # prompts for the passphrase

# Public keys. Create an identity once, and give senders the public key.
tpt-press keygen me.key                             # prints tptpub1...
tpt-press pack secret.txt secret.tppr --recipient tptpub1...
tpt-press pack secret.txt secret.tppr --recipient-file recipients.txt
tpt-press unpack secret.tppr secret.txt --identity me.key
```

Keep the identity file private. Anyone who has it can open every archive sent to
its public key. A passphrase and a recipient cannot be combined.

Generic and logs archives can be encrypted. Encrypted archives are recognised by
their magic header, and the commands then ask for the passphrase, or take
`--identity`, before they read. Logs archives are decrypted into memory and never
written to disk as plaintext. Tensor, asset, genomics and array archives are not
encrypted by the CLI yet.

## Logs

```sh
tpt-press pack-logs app.jsonl app.tplg
tpt-press pack-logs data.csv data.tplg --timestamp-field time
tpt-press pack-logs app.jsonl app.tplg --no-dictionary --block-size 262144

tpt-press query app.tplg --from 2026-03-01T01:00:00Z --to 2026-03-01T01:05:00Z
tpt-press query app.tplg --columns timestamp,level --output window.jsonl
tpt-press query app.tplg --sql "SELECT level, COUNT(*) FROM logs GROUP BY level"   # --features strata
```

The input is JSON Lines, unless the file name ends in `.csv`, which is read with
its header row. `--timestamp-field` names the field or column holding an RFC 3339
timestamp, and the default is `timestamp`. `--from` and `--to` are inclusive.

The records written by `query` are the ones in the window. Only the blocks that
overlap the window are read, and only the columns named by `--columns` are
decompressed.

## Tensors

```sh
tpt-press pack-tensors model.safetensors model.tptn   # also GGUF, ONNX, PyTorch
tpt-press pack-tensors model.safetensors shards/ --shard-size 1000000000
tpt-press inspect model.tptn --tensors                 # list tensors
tpt-press inspect shards/ --verify                     # check a sharded model
tpt-press query model.tptn embed.weight > embed.bin    # one tensor, raw bytes
tpt-press query shards/ embed.weight > embed.bin       # the same, from shards
tpt-press unpack-tensors model.tptn model.safetensors
```

A sharded model is a directory holding `manifest.json` and the shard archives.
`query` finds the shard that holds a tensor through the manifest.

## Assets

```sh
tpt-press pack-assets assets/ game.tpa --store --level 9
tpt-press list-assets game.tpa --verify
tpt-press unpack-assets game.tpa out/
```

`--store` keeps every asset raw, so the game can read them through the memory
map without copying. Without it, each asset is compressed when that makes it
smaller.

## Genomics

```sh
tpt-press pack-genomics genome.fna genome.tpgn       # FASTA or FASTQ, by first byte
tpt-press fetch-genomics genome.tpgn chr1 --start 1000 --end 1100
tpt-press fetch-genomics genome.tpgn chr1 > chr1.fasta
tpt-press unpack-genomics genome.tpgn genome.out.fna
```

`fetch-genomics` takes `--start` and `--end` together. The range is 1-based and
inclusive. It prints the record's header and the requested bases, wrapped as the
file was.

## Arrays

```sh
tpt-press pack-arrays field.npy field.tpar                     # keeps the .npy header
tpt-press pack-arrays raw.bin field.tpar --dtype '<u2' --shape 1024,1024 --chunk 128,128
tpt-press pack-arrays field.npy field.tpar --filter lorenzo
tpt-press slice-arrays field.tpar --range 100:110 --range 200:260 > part.bin
tpt-press unpack-arrays field.tpar field.npy
```

Filters are `auto` (the default), `none`, `shuffle`, `xor`, `up`, `delta`,
`delta-up` and `lorenzo`. `slice-arrays` writes raw bytes in C order, with no
header. Use `inspect` to get the dtype and shape.

## Inspecting any archive

`inspect` reads the first bytes of a file to decide what it is, then prints its
structure, and for a generic archive, its chunk table:

```sh
tpt-press inspect anything.tppr
tpt-press inspect anything.tppr --chunks
tpt-press inspect anything.tppr --verify       # reads and checks every chunk
```

`--chunks` is for generic archives. `--tensors` is for tensor archives.
`--verify` reads every payload and checks its checksum.

## Exit behaviour

- `pack` removes its output file if packing fails. A corrupt or truncated input
  does not leave a partial archive behind.
- `unpack` on a specialised archive names the subcommand to use, rather than
  guessing.
- Errors go to standard error and exit with a non-zero status.

## Features

| Feature | Default | What it adds |
|---------|---------|--------------|
| `strata` | off | `query --sql`, through `tpt-strata` 0.1.0 from crates.io |

```sh
cargo build --release -p tpt-press-cli --features strata
```

`tpt-strata` is on crates.io, so the feature also works from a published build: `cargo install tpt-press-cli --features strata`.

## Scripts and benchmark tools

The `examples/` folder has two kinds of thing.

**Walkthroughs.** [`workflow.ps1`](examples/workflow.ps1) and
[`workflow.sh`](examples/workflow.sh) pack and unpack each kind of archive,
query logs, fetch a genome record, and encrypt to a key pair. Each step checks
the round trip. They build their own sample inputs in a temporary folder.

```sh
cargo build --release -p tpt-press-cli
pwsh crates/tpt-press-cli/examples/workflow.ps1      # Windows, or PowerShell on any OS
bash crates/tpt-press-cli/examples/workflow.sh       # Linux and macOS
```

**Benchmark tools.** These are used for [`docs/benchmarks.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/benchmarks.md).

```sh
cargo run -p tpt-press-cli --release --example make_logs_corpus -- corpus.jsonl 400000
cargo run -p tpt-press-cli --release --example compare_formats -- corpus.jsonl
```

`make_logs_corpus` writes the same bytes for the same arguments.
`compare_formats` prints the size and time of zip and plain zstd for one file.

## License

Licensed under either of [Apache License 2.0](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-APACHE) or
[MIT license](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-MIT), at your option.
