//! Tests that span several tpt-press crates.

use std::io::Cursor;

use tpt_press_assets::AssetsArchive;
use tpt_press_core::archive::{self, PackOptions as CorePackOptions, UnpackOptions};
use tpt_press_core::{ArchiveFormat, Chunking, GenericArchive};
use tpt_press_logs::{self as logs, Format, LogsArchive, PackOptions as LogsPackOptions, Query};
use tpt_press_tensors::{self as tensors, AsyncArchive, ShardedArchive};

/// A safetensors file with two F32 tensors, and the tensor bytes.
fn safetensors_file() -> (Vec<u8>, Vec<(&'static str, Vec<u8>)>) {
    let weights: Vec<u8> = (0..256u32)
        .flat_map(|v| (v as f32 * 0.25).to_le_bytes())
        .collect();
    let bias: Vec<u8> = (0..8u32).flat_map(|v| (v as f32).to_le_bytes()).collect();
    let header = "{\"bias\":{\"dtype\":\"F32\",\"shape\":[8],\"data_offsets\":[0,32]},\
         \"weights\":{\"dtype\":\"F32\",\"shape\":[16,16],\"data_offsets\":[32,1056]}}";
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend_from_slice(&bias);
    file.extend_from_slice(&weights);
    (file, vec![("bias", bias), ("weights", weights)])
}

#[test]
fn safetensors_survives_a_trip_through_a_tensor_archive() {
    let (model, expected) = safetensors_file();

    // Pack with the tensors crate, then unpack back to safetensors.
    let mut packed = Vec::new();
    tensors::pack(
        Cursor::new(&model),
        &mut packed,
        &tensors::PackOptions::default(),
    )
    .unwrap();
    let mut restored = Vec::new();
    let mut archive = tensors::Archive::open(Cursor::new(packed)).unwrap();
    tensors::unpack(&mut archive, &mut restored).unwrap();

    // The unpacked file is safetensors. Parse it, and compare each tensor's bytes.
    let model = tensors::safetensors::parse(&mut Cursor::new(&restored)).unwrap();
    for (name, bytes) in &expected {
        let info = model.tensors.iter().find(|t| t.name == *name).unwrap();
        let begin = (model.data_start + info.begin) as usize;
        let end = (model.data_start + info.end) as usize;
        assert_eq!(&restored[begin..end], bytes.as_slice(), "tensor {name}");
    }
}

#[test]
fn chunking_strategy_does_not_change_what_unpacks() {
    let data: Vec<u8> = (0..500_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let fixed = CorePackOptions {
        chunk_size: 16 * 1024,
        ..CorePackOptions::default()
    };
    let content = CorePackOptions {
        chunk_size: 4096,
        chunking: Chunking::ContentDefined,
        ..CorePackOptions::default()
    };

    let mut a = Vec::new();
    archive::pack(Cursor::new(&data), &mut a, &fixed).unwrap();
    let mut b = Vec::new();
    archive::pack(Cursor::new(&data), &mut b, &content).unwrap();
    assert_ne!(a, b, "the two layouts should differ");

    for packed in [a, b] {
        let mut restored = Vec::new();
        archive::unpack(
            Cursor::new(&packed),
            &mut restored,
            &UnpackOptions::default(),
        )
        .unwrap();
        assert_eq!(restored, data);
    }
}

#[test]
fn each_crate_recognises_only_its_own_archives() {
    let text = b"plain text, packed by the generic format".repeat(50);
    let mut generic = Vec::new();
    archive::pack(
        Cursor::new(&text),
        &mut generic,
        &CorePackOptions::default(),
    )
    .unwrap();

    let line = "{\"timestamp\":\"2026-01-01T00:00:00Z\",\"level\":\"info\"}\n";
    let mut logs_archive = Vec::new();
    logs::pack(
        Cursor::new(line.repeat(20).into_bytes()),
        &mut logs_archive,
        &LogsPackOptions {
            format: Format::Jsonl,
            timestamp_field: "timestamp".to_owned(),
            block_size: 1024,
            level: 3,
            dictionary: true,
        },
    )
    .unwrap();

    let (model, _) = safetensors_file();
    let mut tensor_archive = Vec::new();
    tensors::pack(
        Cursor::new(&model),
        &mut tensor_archive,
        &tensors::PackOptions::default(),
    )
    .unwrap();

    let mut assets_bytes = Vec::new();
    tpt_press_assets::pack(
        [Ok(("a.txt".to_owned(), b"asset bytes".to_vec()))],
        &mut assets_bytes,
        &tpt_press_assets::PackOptions::default(),
    )
    .unwrap();

    let generic_ok = |h: &[u8]| GenericArchive::recognises(h);
    let logs_ok = |h: &[u8]| LogsArchive::recognises(h);
    let tensors_ok = |h: &[u8]| tensors::TensorsArchive::recognises(h);
    let assets_ok = |h: &[u8]| AssetsArchive::recognises(h);

    let checks: [(&str, &[u8], [bool; 4]); 4] = [
        ("generic", &generic, [true, false, false, false]),
        ("logs", &logs_archive, [false, true, false, false]),
        ("tensors", &tensor_archive, [false, false, true, false]),
        ("assets", &assets_bytes, [false, false, false, true]),
    ];
    for (label, bytes, expected) in checks {
        let header = &bytes[..bytes.len().min(16)];
        let actual = [
            generic_ok(header),
            logs_ok(header),
            tensors_ok(header),
            assets_ok(header),
        ];
        assert_eq!(actual, expected, "{label} archive");
    }
}

#[test]
fn logs_round_trip_through_a_query_with_a_time_range() {
    let records: String = (0..600)
        .map(|i| {
            format!(
                "{{\"timestamp\":\"2026-02-01T{:02}:{:02}:00Z\",\"level\":\"{}\",\"n\":{i}}}\n",
                i / 60,
                i % 60,
                if i % 7 == 0 { "error" } else { "info" }
            )
        })
        .collect();

    let mut archive = Vec::new();
    logs::pack(
        Cursor::new(records.as_bytes().to_vec()),
        &mut archive,
        &LogsPackOptions {
            format: Format::Jsonl,
            timestamp_field: "timestamp".to_owned(),
            block_size: 2048,
            level: 3,
            dictionary: true,
        },
    )
    .unwrap();

    let query = Query {
        from: Some(logs::parse_timestamp("2026-02-01T05:00:00Z").unwrap()),
        to: Some(logs::parse_timestamp("2026-02-01T05:09:59Z").unwrap()),
        columns: Some(vec!["level".to_owned()]),
    };
    let mut out = Vec::new();
    logs::query(Cursor::new(&archive), &mut out, &query).unwrap();
    let rows = String::from_utf8(out).unwrap();
    // Ten minutes of records, one row each.
    // One record a minute, so the ten-minute window holds ten rows.
    assert_eq!(rows.lines().count(), 10);
}

#[test]
fn sharded_archive_reads_match_the_blocking_reader() {
    let (model, expected) = safetensors_file();
    let shard_bytes = 600; // Splits the two tensors into two shards.
    let shards = std::cell::RefCell::new(Vec::<std::rc::Rc<std::cell::RefCell<Vec<u8>>>>::new());

    struct Writer(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (manifest, _) = tensors::pack_sharded(
        Cursor::new(&model),
        shard_bytes,
        &tensors::PackOptions::default(),
        |_| {
            let cell = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            shards.borrow_mut().push(cell.clone());
            Ok(Writer(cell))
        },
    )
    .unwrap();
    assert_eq!(manifest.shards.len(), 2);

    // Round-trip the manifest through its JSON, as a loader would.
    let manifest = tensors::Manifest::parse(&manifest.to_json()).unwrap();
    let bytes: Vec<Vec<u8>> = shards.borrow().iter().map(|c| c.borrow().clone()).collect();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut opened = Vec::new();
        for shard in bytes {
            opened.push(AsyncArchive::open(Cursor::new(shard)).await.unwrap());
        }
        let mut sharded = ShardedArchive::new(&manifest, opened).unwrap();
        for (name, data) in &expected {
            assert_eq!(
                &sharded.read_tensor(name).await.unwrap(),
                data,
                "tensor {name}"
            );
        }
    });
}

#[test]
fn encrypted_generic_archive_round_trips_with_a_passphrase() {
    let data = b"secret payload ".repeat(500);
    let mut archive = Vec::new();
    tpt_press_core::pack_encrypted(
        Cursor::new(&data),
        &mut archive,
        &CorePackOptions::default(),
        "correct horse",
    )
    .unwrap();

    let reader =
        tpt_press_core::open_archive(Cursor::new(&archive), Some("correct horse")).unwrap();
    let mut restored = Vec::new();
    archive::unpack(reader, &mut restored, &UnpackOptions::default()).unwrap();
    assert_eq!(restored, data);
}
