# Integration tests

This package (`tpt-press-tests`) holds tests that span more than one crate, and
it is a workspace member, so `cargo test --workspace` runs them. Unit tests stay
inside each crate's `src/`. CLI end-to-end tests live in `crates/tpt-press-cli/tests/`.
