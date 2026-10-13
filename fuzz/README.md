# Fuzzing

Coverage-guided fuzz targets for the parsers that read untrusted input. They use
`cargo-fuzz`, which needs nightly Rust and runs on Linux (or macOS). It does not
run on Windows.

```sh
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run core_unpack -- -max_total_time=300
cargo +nightly fuzz run logs_query -- -max_total_time=300
cargo +nightly fuzz run tensors_read -- -max_total_time=300
cargo +nightly fuzz run tensors_pytorch -- -max_total_time=300
cargo +nightly fuzz run tensors_manifest -- -max_total_time=300
cargo +nightly fuzz run assets_open -- -max_total_time=300
```

Targets:

- `core_unpack`: the generic `.tppr` archive reader (`inspect` and `unpack`)
- `logs_query`: the logs archive reader (`inspect` and `query`)
- `tensors_read`: safetensors and GGUF parsing, and the `.tptn` reader
- `tensors_pytorch`: PyTorch zip checkpoints, including the restricted pickle reader
- `tensors_manifest`: the JSON manifest of a sharded archive
- `assets_open`: the `.tpa` reader. It writes each input to a scratch file, because the reader memory-maps a file

A crash is saved under `fuzz/artifacts/`. Reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`.
