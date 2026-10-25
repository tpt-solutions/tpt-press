//! Compares reading samples in random order from a dataset archive and from a
//! tar shard. A plain tar has no index, so the tar side gets an offset table
//! built from its headers. That is the fair version of the comparison.
//!
//! Run with `cargo run --release -p tpt-press-datasets --example random_access`.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::time::Instant;

use tpt_press_datasets::{pack_samples, shuffled_order, Dataset, Member, Sample};

const SAMPLES: usize = 20_000;
const READS: usize = 5_000;

fn main() {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut random = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    };
    let samples: Vec<(String, Vec<u8>, Vec<u8>)> = (0..SAMPLES)
        .map(|i| {
            let jpg = random(4096);
            let json = format!(
                "{{\"index\":{i},\"label\":\"cat\",\"notes\":\"{}\"}}",
                "routine indoor sample ".repeat(8)
            )
            .into_bytes();
            (format!("shard/{i:06}"), jpg, json)
        })
        .collect();

    // Tar shard, in the WebDataset layout.
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (key, jpg, json) in &samples {
            for (name, data) in [("jpg", jpg), ("json", json)] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{key}.{name}"), data.as_slice())
                    .unwrap();
            }
        }
        builder.finish().unwrap();
    }

    // Offset table for the tar: where each `.jpg` and `.json` data starts.
    let mut tar_offsets = Vec::with_capacity(SAMPLES);
    {
        let mut archive = tar::Archive::new(Cursor::new(&tar_bytes));
        let mut pending: Option<u64> = None;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let pos = entry.raw_file_position();
            if pending.is_none() {
                pending = Some(pos);
            } else {
                tar_offsets.push((pending.take().unwrap(), pos));
            }
        }
    }

    // Dataset archive.
    let dataset_samples = samples.iter().map(|(key, jpg, json)| Sample {
        key: key.clone(),
        members: vec![
            Member {
                name: "jpg".to_owned(),
                data: jpg.clone(),
            },
            Member {
                name: "json".to_owned(),
                data: json.clone(),
            },
        ],
    });
    let mut dataset_bytes = Vec::new();
    pack_samples(dataset_samples, &mut dataset_bytes, 3).unwrap();

    let raw: usize = samples.iter().map(|(_, j, t)| j.len() + t.len()).sum();
    println!(
        "{SAMPLES} samples, {:.1} MB of member bytes",
        raw as f64 / 1e6
    );
    println!(
        "tar shard:      {:>10} bytes ({:.1}%)",
        tar_bytes.len(),
        tar_bytes.len() as f64 * 100.0 / raw as f64
    );
    println!(
        "dataset (.tpds): {:>9} bytes ({:.1}%)",
        dataset_bytes.len(),
        dataset_bytes.len() as f64 * 100.0 / raw as f64
    );

    let order = shuffled_order(SAMPLES, 2026);
    let picks: Vec<usize> = order.iter().copied().take(READS).collect();

    let mut tar_input = Cursor::new(tar_bytes);
    let start = Instant::now();
    let mut checksum = 0u64;
    for &i in &picks {
        let (jpg_at, json_at) = tar_offsets[i];
        for at in [jpg_at, json_at] {
            tar_input.seek(SeekFrom::Start(at)).unwrap();
            let mut buf = vec![0u8; 4096.min(tar_input.get_ref().len() - at as usize)];
            tar_input.read_exact(&mut buf).unwrap();
            checksum = checksum.wrapping_add(buf[0] as u64);
        }
    }
    let tar_time = start.elapsed();

    let mut dataset = Dataset::open(Cursor::new(dataset_bytes)).unwrap();
    let start = Instant::now();
    for &i in &picks {
        let sample = dataset.sample(i).unwrap();
        checksum = checksum.wrapping_add(sample.members[0].data[0] as u64);
    }
    let dataset_time = start.elapsed();

    println!(
        "random reads of {READS} samples: tar+offsets {:.1} ms, dataset {:.1} ms (checksum {checksum})",
        tar_time.as_secs_f64() * 1e3,
        dataset_time.as_secs_f64() * 1e3
    );
}
