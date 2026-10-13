//! Splits a safetensors model into shards in memory, and prints the manifest.
//!
//! Run with `cargo run -p tpt-press-tensors --example shard_model`.

use std::io::Cursor;

use tpt_press_tensors::{pack_sharded, PackOptions};

/// A safetensors file with four F32 tensors of 4 KiB each.
fn model() -> Vec<u8> {
    let header = "{\"layer0\":{\"dtype\":\"F32\",\"shape\":[1024],\"data_offsets\":[0,4096]},\
                  \"layer1\":{\"dtype\":\"F32\",\"shape\":[1024],\"data_offsets\":[4096,8192]},\
                  \"layer2\":{\"dtype\":\"F32\",\"shape\":[1024],\"data_offsets\":[8192,12288]},\
                  \"layer3\":{\"dtype\":\"F32\",\"shape\":[1024],\"data_offsets\":[12288,16384]}}";
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend((0..16_384u32).map(|i| (i % 251) as u8));
    file
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Two tensors per shard at most, so four tensors make two shards.
    let mut shard_count = 0;
    let (manifest, _) = pack_sharded(
        Cursor::new(model()),
        8192,
        &PackOptions::default(),
        |_name| {
            shard_count += 1;
            Ok(Vec::new())
        },
    )?;
    println!("{}", manifest.to_json());
    println!(
        "{} shards written, {} tensors",
        shard_count,
        manifest.tensor_count()
    );
    Ok(())
}
