# tpt-press-tensors

Machine-learning model archives for [tpt-press](https://github.com/tpt-solutions/tpt-press/blob/master/README.md). It reads
model weights from safetensors, GGUF, ONNX and PyTorch checkpoints, and packs
them into a `.tptn` archive. Each tensor is split only at its own element
boundaries, so a loader can read one tensor, or one piece of it, without
decompressing the rest.

It is built for weights that are read often and at known positions: load a
model, read a layer, stream a large embedding to a device in pieces. It is not
a training checkpoint format, and it does not change the numbers in any tensor.

> **Status:** pre-0.1. Format version 1, which may change before 1.0. Measured
> on one real checkpoint and one synthetic one (see [Results](#results)).
> Nothing is published to crates.io yet.

## Contents

- [What it provides](#what-it-provides)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Input formats](#input-formats)
- [Reading tensors](#reading-tensors)
- [Sharding](#sharding)
- [Async reading](#async-reading)
- [The archive format](#the-archive-format)
- [Results](#results)
- [Safety](#safety)
- [Limits and open work](#limits-and-open-work)
- [Command line](#command-line)
- [Examples](#examples)
- [License](#license)

## What it provides

| Item | What it is for |
|------|----------------|
| `pack(input, output, &PackOptions)` | Pack a model (any supported input) into one archive |
| `pack_sharded(input, shard_bytes, &opts, open_shard)` | Split a model into shard archives, with a JSON manifest |
| `Archive::open(reader)` | Read the index. Shapes, dtypes and sizes are available without reading data |
| `Archive::read_tensor` and `read_tensor_into` | Read a whole tensor, or stream it one checksummed piece at a time |
| `Archive::verify` | Check every piece against its checksum |
| `unpack(&mut archive, output)` | Write the archive back out as a safetensors file |
| `AsyncArchive`, `ShardedArchive` | The same reads for a source read by range, behind the `async` feature |
| `safetensors`, `gguf`, `onnx`, `pytorch` modules | The parsers, usable on their own |
| `Manifest` | Parse and write the shard manifest |

## Installation

```toml
[dependencies]
tpt-press-tensors = { path = "crates/tpt-press-tensors" }
# For AsyncArchive and ShardedArchive:
# tpt-press-tensors = { path = "crates/tpt-press-tensors", features = ["async"] }
```

Once published, `cargo add tpt-press-tensors`. The minimum supported Rust
version is 1.96.

## Quick start

```rust
use std::io::Cursor;
use tpt_press_tensors::{pack, Archive, PackOptions};

let model: Vec<u8> = std::fs::read("model.safetensors")?;

let mut archive_bytes = Vec::new();
pack(Cursor::new(&model), &mut archive_bytes, &PackOptions::default())?;

let mut archive = Archive::open(Cursor::new(&archive_bytes))?;
for tensor in archive.tensors() {
    println!("{} {} {:?}", tensor.name, tensor.dtype, tensor.shape);
}
let embedding = archive.read_tensor("model.embed_tokens.weight")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`pack` detects the input format from its first bytes, so the same call takes a
safetensors, GGUF, ONNX or PyTorch file.

## Input formats

| Format | Detected by | Notes |
|--------|-------------|-------|
| safetensors | The header length and JSON header | All dtypes in the file are kept |
| GGUF | The `GGUF` magic | Versions 2 and 3 |
| ONNX | Protobuf `TensorProto` messages | Initializers stored inline (`raw_data`) only. External data files are not read |
| PyTorch (`torch.save`, zip) | Zip container, PyTorch 1.6 and later | Reads tensors only |
| PyTorch (legacy) | Pickle stream, PyTorch 1.5 and earlier | Reads tensors only |

The PyTorch reader **never executes the pickle**. It runs a restricted
interpreter that accepts only the constructs `torch.save` writes for tensors.
Anything else is refused, not run. This matters because a `.pt` file from an
unknown source is code as much as data.

## Reading tensors

`Archive::open` reads the footer and the index, which is enough to list every
tensor, with its name, dtype, shape and byte length. Data is read on demand:

```rust
// Whole tensor, in memory.
let bytes = archive.read_tensor("layer0.attn.q_proj.weight")?;

// Streamed. The sink sees each piece in order, after its checksum passes.
// A GPU loader can copy each piece to a device buffer as it arrives.
archive.read_tensor_into("embed.weight", |piece| {
    upload(piece); // your code
    Ok(())
})?;
```

Pieces are at most 16 MiB (`MAX_PIECE_BYTES`), and they end on element
boundaries, so a piece never splits a value.

`unpack` writes every tensor back out as a valid safetensors file. The tensors,
shapes, dtypes, bytes and metadata are the same as the source. Tensors are laid
out in name order, so the byte offsets can differ from the original file.

## Sharding

For a model too large for one file, `pack_sharded` writes shards of about a
given size. A tensor bigger than the limit gets a shard to itself. The caller
decides where each shard goes, through the `open_shard` callback, so shards can
go to files, memory or a network store.

The manifest is a JSON file (`manifest.json`, the constant `MANIFEST_FILE`)
listing each shard and the tensors in it. A tensor appears in one shard only.
The manifest is validated on read: shard names must be plain, so a hostile
manifest cannot point outside its directory.

```sh
tpt-press pack-tensors model.safetensors shards/ --shard-size 1000000000
tpt-press query shards/ embed.weight > embed.bin
```

## Async reading

With the `async` feature, `AsyncArchive` reads from any tokio reader that can
seek. It fetches each piece with a seek and a read, so the whole file is never
needed. `ShardedArchive::new` combines shard readers under one manifest.

```rust
let mut archive = AsyncArchive::open(file).await?;
let bytes = archive.read_tensor("lm_head.weight").await?;
```

## The archive format

| Field | Value |
|-------|-------|
| Magic | `TPTN` |
| Version | 1 |
| Footer magic | `TPTX` |
| Pieces | Each piece is stored raw, as one zstd frame, or as a byte shuffle then zstd frame. Each has a CRC32 |
| Piece codec | Chosen per piece: the smallest of raw, zstd and byte-shuffled zstd. The shuffle is tried for bf16, f16, f32 and f64 |
| Index | Tensor names, dtypes, shapes, piece offsets and metadata, at the end of the file |

The layout is documented at the top of [`src/format.rs`](src/format.rs), and
summarised in [`docs/formats.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/formats.md#tensors-archive-tptn-version-1).

The byte shuffle groups the bytes of each element by position, so the exponent
bytes of bf16 weights sit together and compress well. Unpacking only has to
undo the shuffle.

## Results

From [`docs/benchmarks.md`](https://github.com/tpt-solutions/tpt-press/blob/master/docs/benchmarks.md). Single runs on one
Windows 11 machine.

| Input | Tool | Saved |
|-------|------|------:|
| SmolLM2-135M, bf16, 269 MB | plain zstd, 1 MiB chunks | 21.8% |
| SmolLM2-135M, bf16, 269 MB | `pack-tensors` (byte shuffle, level 3) | 28.4% |
| SmolLM2-135M, bf16, 269 MB | `pack-tensors --level 9` | 29.5% |
| Synthetic Gaussian bf16 weights, 34 MB | `pack-tensors` (level 3) | 22.0% |

The embedding tensor of the real checkpoint was read back and compared with its
byte range in the original file. The two were identical.

## Safety

- The crate is `forbid(unsafe_code)`.
- Declared sizes, offsets and counts are checked before allocation.
- Each piece is checked against its CRC32 before a caller sees it.
- The PyTorch pickle reader does not execute code, and copies loaded values into
  a plain tree. A crafted file that made a container contain itself leaked
  memory before this was fixed; the fix has a regression test.
- Shard manifests are validated, as described above.

Property tests and hostile-input tests cover malformed input. Fuzz targets
exist for the tensor reader (`tensors_read`), the PyTorch reader
(`tensors_pytorch`) and the shard manifest (`tensors_manifest`), in
[`fuzz/`](https://github.com/tpt-solutions/tpt-press/tree/master/fuzz/).

## Limits and open work

- **Float codec.** The exponent and mantissa split, with ANS or Huffman coding,
  is not done. The byte shuffle is the only float codec. Fp8 is not supported.
- **Inputs.** Only the formats listed above. There is no reader yet for other
  containers.
- **Dtypes.** Each dtype is kept as the source named it. Tensors are not converted.

## Command line

```sh
tpt-press pack-tensors model.safetensors model.tptn   # also PyTorch, GGUF, ONNX
tpt-press inspect model.tptn --tensors                 # list tensors
tpt-press query model.tptn embed.weight > embed.bin    # one tensor, raw bytes
tpt-press unpack-tensors model.tptn model.safetensors
```

## Examples

| Example | Shows |
|---------|-------|
| [`pack_and_read`](examples/pack_and_read.rs) | Pack a safetensors model in memory, list tensors, read one whole and one streamed |
| [`unpack_to_safetensors`](examples/unpack_to_safetensors.rs) | Round trip back to safetensors, and a check that the tensors match |
| [`shard_model`](examples/shard_model.rs) | Split a model into shards in memory, and print the manifest |
| [`async_read`](examples/async_read.rs) | Read a tensor through `AsyncArchive` (needs `--features async`) |

```sh
cargo run -p tpt-press-tensors --example pack_and_read
cargo run -p tpt-press-tensors --features async --example async_read
```

## License

Licensed under either of [Apache License 2.0](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-APACHE) or
[MIT license](https://github.com/tpt-solutions/tpt-press/blob/master/LICENSE-MIT), at your option.
