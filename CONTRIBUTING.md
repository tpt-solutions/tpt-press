# Contributing to tpt-press

Thanks for your interest in tpt-press.

## Before you start

- Check [TODO.md](TODO.md) for planned work and open items.
- For anything beyond a small fix, open an issue first to discuss the approach.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass before a pull request is merged.

## Guidelines

- Keep the library crates free of CLI concerns. The CLI only consumes them.
- Avoid `unsafe` code. Where it is required (for example, memory mapping in
  `tpt-press-assets`), keep it minimal, document the safety invariants, and
  add tests.
- Parsers of archive data must treat input as untrusted: bounds-check every
  read, cap allocations, and never panic on malformed input.
- Do not implement custom cryptography. Use existing audited crates.

## Licensing of contributions

tpt-press is dual licensed under MIT OR Apache-2.0. By submitting a contribution,
you agree that it is licensed under the same terms, without additional conditions.
