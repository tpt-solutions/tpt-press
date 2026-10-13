# Changelog

All notable changes to `tpt-press-cli` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive formats may change between any two versions.

## [Unreleased]

### Added

- The `tpt-press` binary, with a command for each archive type:
  - Generic: `pack`, `unpack`, `inspect`, and `keygen` for age identities.
  - Logs: `pack-logs`, and `query` with `--from`, `--to`, `--columns`, `--output`
    and `--sql`.
  - Tensors: `pack-tensors` (with `--shard-size`), `unpack-tensors`, and
    `query` on a tensor archive or a sharded directory.
  - Assets: `pack-assets` (with `--store` and `--level`), `list-assets` (with
    `--verify`), and `unpack-assets`.
  - Genomics: `pack-genomics`, `unpack-genomics` and `fetch-genomics`.
  - Arrays: `pack-arrays` (with `--dtype`, `--shape`, `--chunk` and `--filter`),
    `unpack-arrays` and `slice-arrays`.
- `inspect` detects every archive type from its contents. `--chunks`,
  `--tensors` and `--verify` add detail.
- Passphrase encryption (`--encrypt`, `--passphrase-file`) and public-key
  encryption (`--recipient`, `--recipient-file`, `--identity`) for generic and
  logs archives.
- `unpack` on a specialised archive names the subcommand to use.
- Optional `strata` feature: `query --sql`, through `tpt-strata`.
- Examples: `make_logs_corpus` and `compare_formats`, used for the benchmarks in
  `docs/benchmarks.md`. Walkthrough scripts `workflow.ps1` and `workflow.sh`,
  which pack, unpack and check each archive type, and encrypt to a key pair.
- Integration tests for each command, in `tests/`.

### Fixed

- `inspect --chunks` on a tensor archive is refused, as it is for logs, rather
  than silently ignored.

### Security

- `pack` removes its output file when packing fails, so a corrupt or truncated
  input does not leave a partial archive behind.
- Output limits on unpack (`--max-output`) bound what a hostile archive can
  write.
