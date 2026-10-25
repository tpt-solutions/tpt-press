# tpt-press on-disk formats

Each archive type starts with its own four-byte magic, so `tpt-press inspect`
can identify any of them from the first bytes of the file. All integers are
little-endian. Every format is unstable until 1.0: the version field is
checked on read, and a reader refuses versions it does not know.

| Type    | Magic  | Footer magic | Version | Written by                          |
| ------- | ------ | ------------ | ------- | ----------------------------------- |
| Generic | `TPPR` | none (end frame) | 1  | `pack`                              |
| Logs    | `TPLG` | `TPLX`       | 3       | `pack-logs`                         |
| Tensors | `TPTN` | `TPTX`       | 1       | `pack-tensors`                      |
| Assets  | `TPAS` | `TPAX`       | 1       | `pack-assets`                       |
| Datasets | `TPDS` | `TPDX`      | 1       | `import-dataset`                    |

The layouts below are the same as the module docs in each crate's `format.rs`,
which are the reference if the two ever disagree.

## Passphrase encryption

Any archive written with `--encrypt` or `--passphrase-file` is wrapped in an
[age](https://age-encryption.org/) stream. age uses passphrase recipients, and
encrypts in 64 KiB authenticated chunks. The encrypted file starts with age's
magic, `age-encryption.org/v1`, so it is detected without the passphrase. The
decryption work factor is capped at 20 (about 1 GiB of scrypt memory) to refuse
hostile headers.

Logs archives can be encrypted the same way: `pack-logs` takes `--encrypt`,
`--passphrase-file`, `--recipient` or `--recipient-file`. The encryption is
applied as the archive is written, so no plaintext copy is made. An encrypted logs
archive is recognised by its age magic, so `query` and `inspect` decrypt it into
memory first, since a query must seek. Memory use is then about the size of the
archive. Decrypted data is never written to disk.

`pack --recipient KEY` (or `--recipient-file`) encrypts to age X25519 public keys
instead. Any one of the recipients' identities opens the archive, and no
passphrase is needed. The file is the same age format, with X25519 stanzas in
place of the scrypt one, so the magic is the same. A passphrase cannot open a
key-encrypted archive, and the reverse also holds. `keygen` writes an identity
file, and refuses to overwrite one.

## Generic archive (`TPPR`, version 1)

```text
header:  magic "TPPR" (4) | version u16 | flags u16
         if flags bit 0 (dictionary) is set: dict_len u32 | dictionary
frame*:  compressed_len u32 | uncompressed_len u32 | crc32 u32 | payload
end:     a frame whose compressed_len and uncompressed_len are both 0
```

Flag bit 0 means a zstd dictionary follows the header, of at most 1 MiB, and
every frame is compressed with it. `pack --dictionary BYTES` trains one on the
first 4 MiB of input, and is skipped when that is too small to train on. Other
flag bits are refused, so a reader never misreads an archive written by a newer
version.

The CRC32 covers the compressed payload, and is checked before decompression.
Frames appear in input order. A missing end frame means the archive was
truncated. Each frame is one zstd frame. Its decompressed output is capped, which guards
against decompression bombs.

Chunk boundaries are not part of the format. `pack --content-defined` ends
chunks where a gear rolling hash matches, rather than at fixed offsets. A reader
sees only frames, so both kinds of archive are read the same way.

## Logs archive (`TPLG`, version 3)

```text
header: magic "TPLG" | version u16 | format u8 | reserved u8
        | timestamp_field (u16 length + bytes)
        | dictionary      (u32 length + bytes, empty if none)
        | csv_header      (u32 length + bytes, empty for JSONL)
block*: payload_len u32 | crc32 u32 | payload
index:  count u32 | entry * count
footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPLX"
```

`format` is 1 for JSON Lines, 2 for CSV and 3 for Prometheus text samples (see below). The dictionary is a zstd
dictionary trained on the first 10% of the input's cells (at most 64 KiB). It
is used for every block, so it is written once.

Each block's CRC32 covers its whole payload. Blocks hold 256 KiB of input by
default, and are compressed in parallel.

### Block payload

```text
payload:      records u32 | column_count u16 | column_entry * column_count | streams
column_entry: name_len u16 | name | encoding u8 | offset u32 | compressed_len u32 | raw_len u32
```

`offset` counts from the start of the payload. Each column is its own zstd
frame, so a query decompresses only the columns it reads. A query that has a
time range also reads the timestamp column. In a raw column stream, each cell is
followed by `\n`. In JSON, an empty cell means the key is absent, since a real
value is never empty. In CSV, an empty cell is an empty field.

`encoding` is 0 for text cells, as above, and 1 for `Timestamps`. A timestamp
column uses 1 when every record's time cell is the canonical text for its time,
and that encoding compresses smaller in the block. The stream is then the
Gorilla delta-of-delta bit form of the record times (`gorilla.rs`), with no
newlines. The reader rebuilds the text cells exactly. Other timestamp cells keep
encoding 0, so the output is the same as the input text.

### Prometheus and OTLP input

With `format` 3, each line is a Prometheus text sample,
`name{label="value",...} value [timestamp_ms]`. Lines that start with `#`, and
blank lines, are skipped. Each sample is stored as a JSON object: the name under
`metric`, each label under its own name, the value under `value` (a number, or
the strings `NaN`, `+Inf` and `-Inf`), and the time under the timestamp field as
RFC 3339. A sample without a timestamp has no time. Output is JSON, as for JSON
Lines.

### Index entries

Each index entry holds the block's offset, its compressed length, its record
count, and two flags: `has_time` and `untimed`. When `has_time` is set, the
entry also holds the earliest and latest timestamp in the block, in Unix
nanoseconds. A block with an untimed record is always read, because that record
may fall in any range.

## Tensors archive (`TPTN`, version 1)

```text
header: magic "TPTN" | version u16 | flags u16 (reserved, must be 0)
pieces: each piece's payload, back to back
index:  metadata   (u32 count, then key: u16 len + bytes, value: u32 len + bytes)
        tensors    (u32 count, then per tensor: name u16 len + bytes,
                    dtype u8 len + bytes, rank u8, dims u64 * rank,
                    byte_len u64, first_piece u32, piece_count u32)
        pieces     (u32 count, then per piece: offset u64, stored_len u32,
                    raw_len u32, crc32 u32, compression u8)
footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPTX"
```

A tensor is split into pieces only at its own element boundaries. Pieces are
at most 16 MiB, so no piece holds bytes from two tensors. The pieces of one
tensor are contiguous in the piece table, in order. `compression` is:

- `0`: raw bytes;
- `1`: one zstd frame of the piece;
- `2`: one zstd frame of the piece after a byte shuffle. The bytes are regrouped
  by position within each element: all first bytes, then all second bytes, and
  so on. Only element widths of 2, 4 and 8 bytes use it (bf16, f16, f32, f64).
  Unshuffling after decompression restores the original bytes exactly.
- `3`: bf16 only. The bytes are split into an exponent plane (one byte per
  value) and a sign-and-mantissa plane (one byte per value), each its own zstd
  frame. Payload: `exponent_frame_len u32`, the exponent frame, then the other
  frame. Unsplitting restores the bytes exactly. The writer uses it only for
  `BF16` tensors. Code `3` is new in this version, like code `2` was.
- `4`: a delta. The piece is XOR-ed byte by byte with the same piece of a base
  archive, then zstd-compressed. The XOR is exact. The archive records the base's
  identity in its metadata as `base_id` (the identity is a CRC32 over tensor names,
  sizes and piece checksums). Reading needs the base: `Archive::open_with_base`, or
  `--base` in the CLI. A delta piece is only written when the base has the same
  tensor with the same byte length. Opening a delta archive without its base
  fails with `BaseNeeded` when a piece is read. The async reader refuses delta
  archives. Code `4` is new in this version too.

The writer tries every encoding that applies to a piece and keeps the smallest.
Raw wins ties, so incompressible data is stored as it is. Code `2` is new in
this version, so a reader built before it refuses such an archive with an
unknown-compression error, rather than misreading it.

Opening an archive reads only the footer and the index, so `inspect` does not
touch the data. The metadata is the source file's own metadata, such as safetensors
`__metadata__`. GGUF metadata arrays are stored as their length only.

Packing accepts safetensors, GGUF (versions 2 and 3), ONNX, and PyTorch zip
checkpoints. The format is detected from the file's first bytes. Only the plain
GGML types and the Q4/Q5/Q8 and K-quant block types are sized. Other GGML types
are refused. ONNX external data, typed `*_data` fields and string tensors are
refused.

### PyTorch checkpoints

Two formats are read: the zip format (PyTorch 1.6 and later), and the legacy
format (1.5 and earlier), recognised by its magic number. A legacy file is a run
of pickles, then each storage as an 8-byte element count and its raw data. Its
storage data is found by seeking, so only the pickles are held in memory. The
element count of every storage is checked against its persistent id, which
catches an offset error at the first storage it affects. The legacy reader was
tested on `sshleifer/tiny-gpt2`, a real legacy checkpoint: its embedding tensor
matches the original file byte for byte.

A `torch.save` checkpoint is read without executing anything. Its `data.pkl` is
interpreted by a fixed set of opcodes, and only allowlisted globals are called:
the storage types, `OrderedDict`, and the `_rebuild_tensor` family. Any other
opcode or global is refused. The checkpoint must be a dict of tensors, possibly
nested. Other values, such as an epoch number, are refused rather than dropped.
Each storage must be uncompressed in the zip, and each tensor must be
contiguous in its storage. Such a tensor is one run of bytes, so it is packed
straight from the checkpoint, and the archive is then the same as one packed
from safetensors.

### Shards

`pack-tensors --shard-size BYTES` writes a directory instead of one file: a
`manifest.json`, and `shard-NNN.tptn` files. Each shard is a complete archive
with a subset of the tensors, and a tensor larger than the shard size gets a
shard of its own. The manifest is JSON:

```json
{"format": "tpt-press-shards", "version": 1,
 "metadata": {"key": "value"},
 "shards": [{"file": "shard-000.tptn", "tensors": ["a", "b"]}]}
```

Shard file names must be plain names. Paths, `..`, and drive letters are refused,
and a tensor may appear in one shard only. A reader can then open only the
shards it needs, so the async reader (`ShardedArchive`) fetches each tensor from
its shard by range.

## Assets archive (`TPAS`, version 1)

```text
header: magic "TPAS" | version u16 | flags u16 (reserved, must be 0)
data:   each asset's payload, starting on a 4096-byte boundary
index:  count u32 | entry * count, sorted by name
footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPAX"
```

A stored asset's payload is its raw bytes, so a reader can use it straight from
the mapped file. A compressed asset's payload is one zstd frame. Page alignment
keeps stored payloads aligned for the OS and for GPU uploads. The packer stores
an asset raw when compression does not shrink it, and `--store` forces raw.

Names are relative paths with `/` separators. Absolute names, `..`, backslashes
and drive letters are refused when packing and when opening. The index is
sorted by name, so a lookup is a binary search.

Opening an archive checks that every payload lies inside the data area, before
the mapped bytes are ever read.

## Genomics archive (`TPGN`, version 1)

```text
header : magic "TPGN" | version u16 | kind u8 (1 FASTA, 2 FASTQ) | reserved u8
blocks : block payloads, back to back
index  : total_records u64 | total_bases u64 | count u64 | entry * count
         | lookup_raw_len u64 | lookup_comp_len u64 | lookup (zstd)
entry  : offset u64 | length u64 | first_record u64 | records u64 | crc32 u32 | pad u32
lookup : sorted (name hash u64, record u64) pairs, 16 bytes each, before zstd
footer : index offset u64 | index length u64 | index crc32 u32 | "TPGX"
```

Integers are little-endian. Each block's CRC32 is in the index, and the index's
is in the footer. A block is closed once its sequence reaches `block_bases`
(default 2^20). A record is never split across blocks, so one long record makes a
long block.

The lookup holds the FNV-1a hash of each record's name, the first
whitespace-delimited word of its header, with the record number. It is sorted, so
`fetch` finds candidates by binary search, and a candidate is confirmed by
comparing its name in the block. Where two records share a name, the first wins.

A block payload is a fixed head, then eight streams in this order. Each stream
is stored as `raw_len u64 | comp_len u64 | bytes`, and an empty stream has both
lengths zero. Most streams are zstd frames.

- head: `records u64 | seq_len u64 | base_count u64`
- `ids`: each header line without `>` or `@`, followed by `\n`
- `lengths`: each record's sequence length, as varints
- `widths`: FASTA only. Each record's line width, as varints
- `case`: runs of upper and lower case over the block's sequence, as varints,
  starting with an upper run, which may be empty
- `exceptions`: runs of bytes other than A, C, G and T, as triples of varints
  (gap, run length) and one byte. The gap counts from the end of the previous run
- `bases`: a mode byte, then either the bases packed at 2 bits each (mode 0,
  first base in the high bits of each byte), or the arithmetic-coded bases
  (mode 1). The writer keeps the smaller. Mode 1 is used only from 16 bases
- `plus`: FASTQ only. One byte per record, 1 if the `+` line repeats the id
- `quality`: FASTQ only. Empty if there are no qualities. Otherwise a mode byte.
  Mode 0 holds the raw quality lines. Mode 1 holds the alphabet of the bytes used
  (`n u16 | n bytes`), then the arithmetic-coded symbols. The writer keeps the smaller

**The base coder (mode 1).** Each base is two binary decisions: the high bit,
then the low bit. Seven context models predict each decision, from the previous
2, 3, 4, 6, 8, 11 and 16 bases. Each model keeps a counter per (context, node)
pair in a hashed table, and a logistic mixer with weights chosen by node combines
the seven predictions. The arithmetic is integer-only, so every platform decodes
the same. Table size is about four times the block's base count, capped at 2^22
counters per model, so a small block uses little memory.

**The quality coder (mode 1).** Each symbol is coded as a bit tree over its
alphabet index, with the same mixing. Four context models use the previous one,
two and three symbols in the record, and the position in the record divided by
4, together with the previous symbol. Each record starts with its history zeroed.

A FASTA record keeps its width, so the lines are rebuilt exactly. The parser
refuses input that this layout cannot give back byte for byte: CRLF endings, no
final newline, sequence lines of changing width, blank lines inside a record,
and a FASTQ `+` line that is neither bare nor the record id. Each refusal names
the line.

`fetch` looks the name up in the lookup, reads the one block that holds the
record, and decodes that block. It reads more than one block only when two names
share a hash.

## Arrays archive (`TPAR`, version 1)

```text
header : magic "TPAR" | version u16 | ndim u8 | elem u8 | descr_len u16 | descr
         | shape (ndim * u64) | chunk shape (ndim * u64)
         | raw_len u64 | raw bytes (the original .npy header, or none)
chunks : chunk payloads, back to back, in C order over the chunk grid
index  : count u64 | entry * count
entry  : offset u64 | length u64 | crc32 u32 | filter u8 | pad [u8; 3]
footer : index offset u64 | index length u64 | index crc32 u32 | "TPRX"
```

`.npz` files holding exactly one array are read from their `.npy` member, and the
archive holds that array. Unpacking gives a `.npy` file, not the zip container.

A `.npz` with several arrays is stored as a bundle (`TPBN`, see `bundle.rs`): an
index of named members, each member a complete `TPAR` archive. `read_member`
copies one out. `inspect` lists a bundle's members, and `unpack-arrays --member
NAME` writes one. A Zarr v2 directory store (a folder with `.zarray`) is packed
by `pack-arrays`, which supports C order, no filters, and the `null` and `zstd`
compressors. Blosc is refused by name.

`descr` is a NumPy type string such as `<f4`. Only little-endian f2, f4, f8,
i1 to i8, u1 to u8, and single-byte b1 are accepted. Fortran order is refused.
A chunk at the edge of an axis is stored at its real size, not padded.

Each chunk is one zstd frame over its bytes after a filter, and the writer keeps
the filter that gives the smallest frame. Each element is predicted from an
earlier one, and the residual is stored. `row` below is the length of the last
axis of the chunk, so "the row above" is `row` elements back.

- `0` none: the bytes as they are
- `1` shuffle: byte `j` of element `i` moves to `j * n + i`, so the same byte of
  every element sits together
- `2` XOR+shuffle: each element is XORed with the one before, then shuffled
- `3` XOR-up+shuffle: each element is XORed with the one `row` back, then shuffled
- `4` delta+shuffle: each element minus the one before, modulo the element size,
  then shuffled
- `5` delta-up+shuffle: each element minus the one `row` back, then shuffled
- `6` lorenzo+shuffle: each element minus its Lorenzo prediction, the sum of its
  neighbours along each axis with inclusion and exclusion (for two axes,
  `left + up - upper_left`). A neighbour outside the chunk counts as zero. Chunks
  of more than four axes are left unpredicted

The XOR filters suit floats, where neighbours share sign and exponent bits. The
delta filters suit integers that change slowly, and smooth floats too, since
nearby float bit patterns differ by small amounts.

All seven are exact inverses. Since the header keeps the original `.npy` header
bytes, unpacking a `.npy` archive reproduces the file. Raw input has no header,
and its bytes are stored in C order.

A slice read works out which chunks overlap the requested box, decodes only
those, and copies each overlapping run. The index is checked when the archive is
opened: entries must be contiguous and must reach the index. Each chunk is
checked by CRC before it is decompressed.

## Dataset archive (`TPDS`, version 1)

```text
header: magic "TPDS" | version u16 | flags u16 (reserved, must be 0)
members: each member's stored bytes, back to back
index:   one zstd frame of the index body:
           sample_count u32, then per sample:
             key u16 len + bytes | member_count u16
             then per member: name u16 len + bytes | offset u64 | stored_len u32
                              | raw_len u32 | crc32 u32 | codec u8
footer:  index_offset u64 | index_len u32 (stored) | index_raw_len u32
         | index_crc32 u32 (of the stored index) | magic "TPDX"
```

`codec` is 0 for stored bytes and 1 for one zstd frame. The writer stores a member
as it is when its name is a known compressed-media extension (jpg, png, mp4,
and so on), and otherwise uses zstd only when that is smaller. A stored member
has `stored_len == raw_len`. Member sizes are capped at 1 GiB, and the index at
256 MiB.

### OTLP input

With `format` 4, each line is one OTLP JSON `ExportLogsServiceRequest`, which can
hold several log records. Each record becomes a JSON object: `timestamp` (from
`timeUnixNano`, or `observedTimeUnixNano` if that is zero), `severity`,
`severityNumber`, `body`, `traceId` and `spanId` when present, record attributes
as `attr.<key>`, and resource attributes as `resource.<key>`. Metrics and traces
are refused.
