//! Shows the chunking layer that domain crates build on. `Pressable` decides
//! where chunks end, and `compress_chunks` and `decompress_chunks` do the rest,
//! compressing the chunks in parallel and keeping their order.
//!
//! This example splits text at line breaks, so no chunk ends in the middle of
//! a line. The archive it makes is for this example only. It is not the
//! `.tppr` format written by `pack`.
//!
//! Run with `cargo run -p tpt-press-core --example custom_pressable`.

use tpt_press_core::{compress_chunks, decompress_chunks, Pressable};

/// Chunks of about `target` bytes, each ending just after a newline when one
/// is near enough.
struct LineChunks {
    target: usize,
    level: i32,
}

impl Pressable for LineChunks {
    fn chunk_size(&self) -> usize {
        self.target
    }

    fn level(&self) -> i32 {
        self.level
    }

    fn split<'a>(&self, input: &'a [u8]) -> Vec<&'a [u8]> {
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < input.len() {
            let limit = (start + self.target).min(input.len());
            // Prefer to end after the last newline in the window. Fall back to
            // the window's end when there is none, or at the end of the input.
            let end = match input[start..limit].iter().rposition(|&b| b == b'\n') {
                Some(pos) if limit < input.len() => start + pos + 1,
                _ => limit,
            };
            chunks.push(&input[start..end]);
            start = end;
        }
        chunks
    }
}

fn main() -> tpt_press_core::Result<()> {
    let text: String = (0..20_000)
        .map(|i| format!("sensor {} reading {}\n", i % 8, (i * 37) % 1000))
        .collect();
    let input = text.as_bytes();

    let pressable = LineChunks {
        target: 32 * 1024,
        level: 9,
    };
    let compressed = compress_chunks(input, &pressable)?;
    let sizes: Vec<usize> = compressed.iter().map(Vec::len).collect();
    println!(
        "{} bytes in {} chunks, {} bytes compressed",
        input.len(),
        compressed.len(),
        sizes.iter().sum::<usize>()
    );

    // Every chunk must decompress to at most `chunk_size()` bytes. Using that
    // as the bound also protects against a hostile chunk that claims more.
    let restored: Vec<u8> = decompress_chunks(&compressed, pressable.chunk_size())?
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(restored, input);

    // Each chunk except possibly the last ends in a newline.
    let split = pressable.split(input);
    let aligned = split
        .iter()
        .take(split.len().saturating_sub(1))
        .all(|c| c.ends_with(b"\n"));
    println!("round trip ok, every chunk but the last ends at a line: {aligned}");
    Ok(())
}
