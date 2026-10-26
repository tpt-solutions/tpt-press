# Changelog

All notable changes to `tpt-press-core` are listed here. The project follows
[Semantic Versioning](https://semver.org/) once 1.0 is released. Until then,
the archive format may change between any two versions.

## [Unreleased]

### Added

- Generic archive format (`TPPR`, version 1) with `pack`, `unpack` and
  `archive::inspect`.
- Content-defined chunking (`Chunking::ContentDefined`), using a gear rolling
  hash. The average chunk size is a power of two from 256 bytes to 16 MiB.
- Optional zstd dictionary, trained on the start of the input
  (`PackOptions::dictionary`). Stored once in the header, header flag bit 0.
- Passphrase encryption (`tpt-enc/v1`): `crypto::pack_encrypted` and
  `crypto::open`. Keys are derived with Argon2id. Decryption refuses work
  factors above 1 GiB of memory, 10 passes and 16 lanes.
- Public-key encryption to X25519 recipients: `crypto::pack_for_recipients`
  and `crypto::open_with_identities`. Keys are `keys::Recipient` and
  `keys::Identity`, as `tptpub1...` and `TPTSEC1...` strings.
- `EncryptingWriter`, and `encrypt_with_passphrase` and
  `encrypt_for_recipients`, which encrypt while writing. No plaintext copy is
  made.
- `crypto::looks_encrypted`, to recognise an encrypted archive without a key.
- Output limit on unpack: `UnpackOptions::max_total_size`.
- `ArchiveFormat` trait, implemented by each format's marker type, so the CLI
  can recognise archives from their magic bytes.
- `Pressable`, `compress_chunks` and `decompress_chunks`, for domain crates
  that choose their own chunk boundaries.
- Examples: `round_trip`, `encrypted_archive`, `options_tour`,
  `inspect_and_limits` and `custom_pressable`.
- Rustdoc for every public item, with a runnable example on the crate root.

### Security

- Decompression checks each declared chunk size before it allocates, and
  `max_total_size` bounds the whole output. Both protect against archives that
  claim more than they hold.
