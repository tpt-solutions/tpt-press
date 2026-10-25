# tpt-press-datasets

Sharded training datasets for tpt-press: samples grouped by key, with random
access by sample index.

A dataset archive (`.tpds`) holds samples in write order. Each sample has a key
and named members, such as `image.jpg`, `label.cls` and `meta.json`. Opening an
archive reads only its index, so one sample can be read without reading the rest.

- Members that are already compressed media (JPEG, PNG, MP4, and so on) are stored
  as they are. Other members are compressed with zstd when that is smaller.
- Each member has a CRC32, and the index has its own. `Dataset::verify` checks all
  of them.
- `import_hf` converts a Hugging Face-style folder (`metadata.jsonl` plus files).
- `import_tar` converts a WebDataset tar (`<key>.<member>` file names) into an
  archive, streaming one sample at a time.
- `shuffled_order(len, seed)` gives a repeatable random order for an epoch.

```rust
use std::io::Cursor;
use tpt_press_datasets::{import_tar, Dataset};

let mut out = Vec::new();
import_tar(std::fs::File::open("shard-000.tar")?, &mut out, 3)?;
let mut dataset = Dataset::open(Cursor::new(out))?;
let sample = dataset.sample(0)?;
```

Status: pre-0.1. The format may change before 1.0. The layout is in
`docs/formats.md`. Not done: multi-file shard sets
import, and an async reader.

Licensed under MIT OR Apache-2.0.
