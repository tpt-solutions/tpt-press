//! Packs a model, then unpacks it back to a safetensors file, and parses that
//! file to confirm the tensors, shapes and dtypes survived. Use it when a
//! `.tptn` archive has to go back into a tool that reads safetensors.
//!
//! Run with `cargo run -p tpt-press-tensors --example unpack_to_safetensors`.

use std::io::Cursor;

use tpt_press_tensors::{pack, safetensors, unpack, Archive, PackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Four tensors of different dtypes, with the data filled in a simple pattern.
    let specs: [(&str, &str, Vec<u64>, usize); 4] = [
        ("tok.embeddings", "BF16", vec![256, 16], 256 * 16 * 2),
        ("blocks.0.q", "BF16", vec![16, 16], 16 * 16 * 2),
        ("blocks.0.scale", "F32", vec![16], 16 * 4),
        ("head.bias", "F32", vec![8], 8 * 4),
    ];
    let mut header = String::from("{");
    let mut data = Vec::new();
    for (i, (name, dtype, shape, len)) in specs.iter().enumerate() {
        if i > 0 {
            header.push(',');
        }
        let begin = data.len();
        data.extend((0..*len).map(|b| (b * 13 % 256) as u8));
        let shape = shape
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        header.push_str(&format!(
            "\"{name}\":{{\"dtype\":\"{dtype}\",\"shape\":[{shape}],\"data_offsets\":[{begin},{}]}}",
            data.len()
        ));
    }
    header.push('}');
    let mut original = (header.len() as u64).to_le_bytes().to_vec();
    original.extend_from_slice(header.as_bytes());
    original.extend(data);

    let mut archive_bytes = Vec::new();
    pack(
        Cursor::new(&original),
        &mut archive_bytes,
        &PackOptions::default(),
    )?;

    // Unpack writes a valid safetensors file. Tensors are laid out in name order,
    // so the bytes can differ from the original, while the content does not.
    let mut archive = Archive::open(Cursor::new(&archive_bytes))?;
    let mut restored = Vec::new();
    let stats = unpack(&mut archive, &mut restored)?;
    println!(
        "unpacked {} tensors ({} bytes)",
        stats.tensors, stats.data_bytes
    );

    let before = safetensors::parse(&mut Cursor::new(&original))?;
    let after = safetensors::parse(&mut Cursor::new(&restored))?;
    for (a, b) in before.tensors.iter().zip(&after.tensors) {
        assert_eq!((&a.name, &a.dtype, &a.shape), (&b.name, &b.dtype, &b.shape));
        println!("  {:<18} {:<5} {:?} ok", b.name, b.dtype, b.shape);
    }
    assert_eq!(before.tensors.len(), after.tensors.len());
    println!("safetensors header matches the original");
    Ok(())
}
