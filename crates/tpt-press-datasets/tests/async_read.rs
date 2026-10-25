//! The async reader reads the same archives as the blocking reader.
#![cfg(feature = "async")]

use std::io::Cursor;

use tpt_press_datasets::{pack_samples, AsyncDataset, Error, Member, Sample};

fn archive() -> Vec<u8> {
    let samples = (0..30).map(|i| Sample {
        key: format!("s/{i:04}"),
        members: vec![
            Member {
                name: "bin".to_owned(),
                data: (0..512u32).map(|b| (b * 7 + i) as u8).collect(),
            },
            Member {
                name: "txt".to_owned(),
                data: format!("sample number {i} ").repeat(20).into_bytes(),
            },
        ],
    });
    let mut out = Vec::new();
    pack_samples(samples, &mut out, 3).unwrap();
    out
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[test]
fn async_reads_match_the_archive() {
    runtime().block_on(async {
        let bytes = archive();
        let mut dataset = AsyncDataset::open(Cursor::new(bytes)).await.unwrap();
        assert_eq!(dataset.len(), 30);
        assert_eq!(dataset.key(4).unwrap(), "s/0004");
        let txt = dataset.member(4, "txt").await.unwrap();
        assert!(txt.starts_with(b"sample number 4 "));
        let sample = dataset.sample(29).await.unwrap();
        assert_eq!(sample.key, "s/0029");
        assert_eq!(sample.members.len(), 2);
        dataset.verify().await.unwrap();
        assert!(matches!(
            dataset.sample(30).await,
            Err(Error::OutOfRange { index: 30, len: 30 })
        ));
    });
}

#[test]
fn async_open_detects_a_damaged_member() {
    runtime().block_on(async {
        let mut bytes = archive();
        bytes[8 + 3] ^= 0xFF;
        let mut dataset = AsyncDataset::open(Cursor::new(bytes)).await.unwrap();
        assert!(matches!(
            dataset.verify().await,
            Err(Error::ChecksumMismatch { .. })
        ));
    });
}
