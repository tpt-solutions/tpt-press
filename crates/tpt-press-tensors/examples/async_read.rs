//! Reads one tensor from an archive through the async reader. This is the path
//! for sources that are read by range, such as a file opened with tokio or an
//! object store. Opening reads only the index, and each piece is then fetched
//! with a seek and a read.
//!
//! Needs the `async` feature:
//! `cargo run -p tpt-press-tensors --features async --example async_read`.

use std::io::Cursor;

use tpt_press_tensors::{pack, AsyncArchive, PackOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A small safetensors file with two F32 tensors.
    let header = "{\"a\":{\"dtype\":\"F32\",\"shape\":[256],\"data_offsets\":[0,1024]},\
                  \"b\":{\"dtype\":\"F32\",\"shape\":[256],\"data_offsets\":[1024,2048]}}";
    let mut model = (header.len() as u64).to_le_bytes().to_vec();
    model.extend_from_slice(header.as_bytes());
    model.extend((0..2048u32).map(|i| (i % 97) as u8));

    let mut archive_bytes = Vec::new();
    pack(
        Cursor::new(&model),
        &mut archive_bytes,
        &PackOptions::default(),
    )?;

    // Any tokio reader that can seek will do. An in-memory cursor is used here.
    let mut archive = AsyncArchive::open(Cursor::new(archive_bytes)).await?;
    println!(
        "tensors: {:?}",
        archive
            .tensors()
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );

    let b = archive.read_tensor("b").await?;
    assert_eq!(b, &model[model.len() - 1024..]);
    println!("read 'b': {} bytes, matches the original", b.len());
    Ok(())
}
