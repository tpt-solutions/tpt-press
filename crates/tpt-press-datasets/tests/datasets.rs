//! Integration tests for dataset archives: samples, random access, tar import,
//! and damage detection.

use std::io::Cursor;

use tpt_press_datasets::{
    import_tar, pack_samples, shuffled_order, Codec, Dataset, Error, Member, Sample,
};

fn text_member(name: &str, text: &str) -> Member {
    Member {
        name: name.to_owned(),
        data: text.as_bytes().to_vec(),
    }
}

/// Samples with a JPEG-like member (random bytes, already compressed) and a text
/// member that compresses well.
fn samples(count: usize) -> Vec<Sample> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    (0..count)
        .map(|i| {
            let jpg: Vec<u8> = (0..2048)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x >> 24) as u8
                })
                .collect();
            Sample {
                key: format!("shard/{i:06}"),
                members: vec![
                    Member {
                        name: "jpg".to_owned(),
                        data: jpg,
                    },
                    text_member(
                        "json",
                        &format!(
                            "{{\"index\":{i},\"label\":\"cat\",\"notes\":\"{}\"}}",
                            "routine indoor sample ".repeat(8)
                        ),
                    ),
                    text_member("cls", &(i % 10).to_string()),
                ],
            }
        })
        .collect()
}

fn pack(samples: Vec<Sample>) -> Vec<u8> {
    let mut out = Vec::new();
    pack_samples(samples, &mut out, 3).unwrap();
    out
}

#[test]
fn samples_round_trip_by_index_and_by_member() {
    let input = samples(50);
    let archive = pack(input.clone());
    let mut dataset = Dataset::open(Cursor::new(archive)).unwrap();

    assert_eq!(dataset.len(), 50);
    assert_eq!(dataset.key(7).unwrap(), "shard/000007");
    assert_eq!(dataset.member_names(7).unwrap(), ["jpg", "json", "cls"]);
    assert_eq!(dataset.sample(7).unwrap(), input[7]);
    assert_eq!(dataset.member(49, "cls").unwrap(), b"9");
    dataset.verify().unwrap();
}

#[test]
fn media_is_stored_and_compressible_text_is_zstd() {
    let dataset = Dataset::open(Cursor::new(pack(samples(20)))).unwrap();
    // Media is never tried with zstd, even when it is random bytes.
    assert_eq!(dataset.member_codec(0, "jpg").unwrap(), Codec::Stored);
    // The JSON member is long and repetitive, so zstd wins.
    assert_eq!(dataset.member_codec(0, "json").unwrap(), Codec::Zstd);
    // A one-digit label is too short for a zstd frame to beat raw bytes.
    assert_eq!(dataset.member_codec(0, "cls").unwrap(), Codec::Stored);
}

#[test]
fn out_of_range_and_missing_member_are_errors() {
    let mut dataset = Dataset::open(Cursor::new(pack(samples(3)))).unwrap();
    assert!(matches!(
        dataset.sample(3),
        Err(Error::OutOfRange { index: 3, len: 3 })
    ));
    assert!(matches!(
        dataset.member(0, "png"),
        Err(Error::MissingMember { .. })
    ));
}

#[test]
fn empty_archive_opens_with_no_samples() {
    let mut dataset = Dataset::open(Cursor::new(pack(Vec::new()))).unwrap();
    assert!(dataset.is_empty());
    dataset.verify().unwrap();
}

#[test]
fn invalid_samples_are_refused_before_writing() {
    let bad = |sample: Sample| {
        let mut out = Vec::new();
        pack_samples(vec![sample], &mut out, 3).is_err()
    };
    assert!(bad(Sample {
        key: String::new(),
        members: vec![text_member("txt", "x")],
    }));
    assert!(bad(Sample {
        key: "k".to_owned(),
        members: vec![],
    }));
    assert!(bad(Sample {
        key: "k".to_owned(),
        members: vec![text_member("txt", "a"), text_member("txt", "b")],
    }));
    assert!(bad(Sample {
        key: "k".to_owned(),
        members: vec![text_member("a/b", "x")],
    }));
}

#[test]
fn shuffled_order_visits_every_sample_once() {
    let mut dataset = Dataset::open(Cursor::new(pack(samples(40)))).unwrap();
    let order = shuffled_order(dataset.len(), 2026);
    let mut keys: Vec<String> = order
        .iter()
        .map(|&i| dataset.key(i).unwrap().to_owned())
        .collect();
    keys.sort();
    let mut expected: Vec<String> = (0..40).map(|i| format!("shard/{i:06}")).collect();
    expected.sort();
    assert_eq!(keys, expected);
    // Reading in shuffled order gives the same sample as reading by index.
    let first = order[0];
    assert_eq!(
        dataset.sample(first).unwrap().key,
        format!("shard/{first:06}")
    );
}

#[test]
fn imports_a_webdataset_tar() {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (path, body) in [
            ("shard/0001.jpg", b"JPEGDATA".to_vec()),
            ("shard/0001.cls", b"3".to_vec()),
            ("shard/0001.json", b"{\"a\":1}".to_vec()),
            ("shard/0002.jpg", b"MOREJPEG".to_vec()),
            ("shard/0002.cls", b"7".to_vec()),
            ("README", b"not a member".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_slice())
                .unwrap();
        }
        builder.finish().unwrap();
    }

    let mut out = Vec::new();
    let stats = import_tar(Cursor::new(tar_bytes), &mut out, 3).unwrap();
    assert_eq!(stats.samples, 2);
    assert_eq!(stats.members, 5);
    assert_eq!(stats.skipped, 1);

    let mut dataset = Dataset::open(Cursor::new(out)).unwrap();
    assert_eq!(dataset.key(0).unwrap(), "shard/0001");
    assert_eq!(dataset.member_names(0).unwrap(), ["jpg", "cls", "json"]);
    assert_eq!(dataset.member(0, "jpg").unwrap(), b"JPEGDATA");
    assert_eq!(dataset.member(1, "cls").unwrap(), b"7");
    dataset.verify().unwrap();
}

#[test]
fn a_damaged_member_is_detected_by_verify() {
    let mut archive = pack(samples(5));
    // The first member's bytes start right after the 8-byte header.
    archive[8 + 10] ^= 0xFF;
    let mut dataset = Dataset::open(Cursor::new(archive)).unwrap();
    assert!(matches!(
        dataset.verify(),
        Err(Error::ChecksumMismatch { .. })
    ));
}

#[test]
fn a_damaged_index_is_detected_on_open() {
    let mut archive = pack(samples(5));
    // The footer is the last 24 bytes: index_offset comes first.
    let footer = archive.len() - 24;
    let index_offset = u64::from_le_bytes(archive[footer..footer + 8].try_into().unwrap()) as usize;
    archive[index_offset + 6] ^= 0x01;
    assert!(matches!(
        Dataset::open(Cursor::new(archive)),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn truncated_and_foreign_files_are_rejected() {
    let archive = pack(samples(5));
    for cut in [0, 4, 20, archive.len() / 2, archive.len() - 1] {
        assert!(
            Dataset::open(Cursor::new(archive[..cut].to_vec())).is_err(),
            "cut at {cut}"
        );
    }
    assert!(matches!(
        Dataset::open(Cursor::new(b"not a dataset at all, just text".to_vec())),
        Err(Error::BadMagic)
    ));
}

#[test]
fn random_bytes_never_panic() {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for round in 0..300 {
        let len = round * 7 % 300;
        let mut bytes: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        if bytes.len() >= 8 {
            bytes[..4].copy_from_slice(b"TPDS");
            bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        }
        if let Ok(mut dataset) = Dataset::open(Cursor::new(bytes)) {
            let _ = dataset.verify();
        }
    }
}

#[test]
fn imports_a_hugging_face_folder() {
    let dir = std::env::temp_dir().join(format!("tpt-hf-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("img")).unwrap();
    std::fs::write(dir.join("img/0001.png"), b"PNGDATA").unwrap();
    std::fs::write(dir.join("img/0002.png"), b"PNGMORE").unwrap();
    std::fs::write(
        dir.join(tpt_press_datasets::METADATA_FILE),
        "{\"file_name\":\"img/0001.png\",\"label\":\"cat\"}\n\n{\"file_name\":\"img/0002.png\",\"label\":\"dog\"}\n",
    )
    .unwrap();

    let mut out = Vec::new();
    let stats = tpt_press_datasets::import_hf(&dir, &mut out, 3).unwrap();
    assert_eq!(stats.samples, 2);
    assert_eq!(stats.members, 4);

    let mut dataset = Dataset::open(Cursor::new(out)).unwrap();
    assert_eq!(dataset.key(0).unwrap(), "img/0001");
    assert_eq!(dataset.member(0, "png").unwrap(), b"PNGDATA");
    let metadata: serde_json::Value =
        serde_json::from_slice(&dataset.member(1, "metadata").unwrap()).unwrap();
    assert_eq!(metadata["label"], "dog");
    assert!(metadata.get("file_name").is_none());
    dataset.verify().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hugging_face_rows_that_leave_the_folder_are_refused() {
    let dir = std::env::temp_dir().join(format!("tpt-hf-escape-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(tpt_press_datasets::METADATA_FILE),
        "{\"file_name\":\"../outside.png\"}\n",
    )
    .unwrap();
    let mut out = Vec::new();
    let result = tpt_press_datasets::import_hf(&dir, &mut out, 3);
    assert!(matches!(result, Err(Error::InvalidInput(_))));
    std::fs::remove_dir_all(&dir).unwrap();
}
