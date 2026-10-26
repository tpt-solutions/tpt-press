# Review of `unsafe` code

Reviewed against the tree at the commit that adds this file. Re-run the search
in "How this was checked" when the code changes.

## Scope

tpt-press has one `unsafe` block of its own:

- `crates/tpt-press-assets/src/reader.rs`, `Archive::open`: `memmap2::Mmap::map` on a
  read-only file handle.

Every other crate forbids unsafe code: `core`, `logs` and `tensors` use
`#![forbid(unsafe_code)]`, and `assets` uses `#![deny(unsafe_code)]` with the
single `#[allow]` above. The CLI inherits the workspace lint `unsafe_code =
"warn"`, and it contains no `unsafe`.

Dependencies (zstd, memmap2, rayon and others) contain their own unsafe
code. This review does not cover them. `cargo deny` checks licenses and
advisories, but not unsafe use.

## The mmap call

Soundness of `Mmap::map` depends on the file not changing size or content
while it is mapped. Rust cannot prevent that. The consequences are:

- Another process that truncates the file can make a later read of a mapped
  page raise SIGBUS on Unix, or an access violation on Windows. This is a
  crash, not a memory-safety violation in tpt-press's own data structures.
- Another process that rewrites bytes in place changes what `get` returns.
  The data is then not the data that was checked. The checksums are not
  rechecked on each read.

The `open` docs state this, and that the caller must not change the file while
it is open. This is a documented precondition, not a guarantee the crate can
enforce. A future change could copy the file or take an advisory lock, but
neither is done now.

## Why the slices that follow the mapping are in bounds

The mapped bytes are sliced in `Archive::payload`:
`&self.map[start..start + stored_len]`. That slice operation is bounds-checked,
so a bad range panics and cannot read outside the mapping. The more important
question is whether a bad range can happen after a successful open, and it
cannot:

- `format::read_index` computes each entry's end with `checked_add`, and
  rejects the archive if the end is past the start of the index. The index
  itself starts at or before the file's end. Its end is also checked with
  `checked_add` against the file length, before the index bytes are sliced.
- Every entry's offset must be a multiple of `PAGE_SIZE` and at least the
  header length.
- The map covers the whole file, so a payload that lies inside the data area
  lies inside the map.

The index slice in `read_index` (`&bytes[index_offset as usize..]`) follows the
same checked pattern. On a 32-bit target, `as usize` can truncate a `u64`, but
the truncated slice is still bounds-checked. That makes it a panic, not a read
outside the map.

## Other checks

- `Archive::get` returns a borrowed slice that lives as long as the `Archive`,
  because the `Asset<'a>` lifetime ties it to `&self`. No slice outlives the
  mapping.
- Short files are refused before any byte is indexed. `check_header` and
  `read_index` both test the length first. A zero-length file fails either in
  the map call or in that length check, so no payload is read.

## Result

No unsoundness found in tpt-press's own code. The single `unsafe` block is
sound given its documented precondition, that the file is not modified while
mapped. The residual risk is an external process changing the file, which is a
documented caller responsibility.

## How this was checked

```text
grep -rn "unsafe" --include=*.rs --include=*.toml crates
```

Each hit was read. The only real `unsafe` is `reader.rs:58`.
