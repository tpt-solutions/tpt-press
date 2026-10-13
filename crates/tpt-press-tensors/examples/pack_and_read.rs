//! Packs a safetensors model into a `.tptn` archive in memory. The archive is
//! then opened, its tensors listed, and each one read back. A tensor can also
//! be streamed a piece at a time, which is how a loader avoids holding it all
//! in memory.
//!
//! Run with `cargo run -p tpt-press-tensors --example pack_and_read`.

use std::io::Cursor;

use tpt_press_tensors::{pack, Archive, PackOptions};

/// Builds a small safetensors file: an 8-byte length, a JSON header, then the
/// tensor data. The data is a smooth ramp, so it compresses well, as trained
/// weights often do.
fn safetensors_model() -> Vec<u8> {
    let tensors: [(&str, &str, &[u64], usize); 3] = [
        ("embed.weight", "F32", &[64, 32], 64 * 32 * 4),
        ("layer0.attn.weight", "F16", &[32, 32], 32 * 32 * 2),
        ("layer0.norm.bias", "F32", &[32], 32 * 4),
    ];

    let mut header = String::from("{\"__metadata__\":{\"format\":\"example\"}");
    let mut data = Vec::new();
    for (name, dtype, shape, len) in tensors {
        let begin = data.len();
        data.extend((0..len).map(|i| ((i / 4) % 200) as u8));
        let shape = shape
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        header.push_str(&format!(
            ",\"{name}\":{{\"dtype\":\"{dtype}\",\"shape\":[{shape}],\"data_offsets\":[{begin},{}]}}",
            data.len()
        ));
    }
    header.push('}');

    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend(data);
    file
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = safetensors_model();

    let mut archive_bytes = Vec::new();
    let stats = pack(
        Cursor::new(&model),
        &mut archive_bytes,
        &PackOptions::default(),
    )?;
    println!(
        "packed {} tensor bytes into {} bytes ({} pieces)",
        stats.input_bytes, stats.archive_bytes, stats.pieces
    );

    // Opening reads only the index, which is enough to list tensors and shapes.
    let mut archive = Archive::open(Cursor::new(&archive_bytes))?;
    println!("\nmetadata: {:?}", archive.metadata());
    for tensor in archive.tensors() {
        println!(
            "  {:<22} {:<4} {:?}  {} bytes",
            tensor.name, tensor.dtype, tensor.shape, tensor.byte_len
        );
    }

    // Read a whole tensor into memory.
    let embed = archive.read_tensor("embed.weight")?;
    assert_eq!(embed.len(), 64 * 32 * 4);

    // Or stream it. The sink gets one checksummed piece at a time, in order.
    let mut pieces = 0;
    let mut total = 0usize;
    archive.read_tensor_into("layer0.attn.weight", |piece| {
        pieces += 1;
        total += piece.len();
        Ok(())
    })?;
    println!("\nstreamed layer0.attn.weight: {total} bytes in {pieces} piece(s)");

    archive.verify()?;
    println!("verify() passed");
    Ok(())
}
