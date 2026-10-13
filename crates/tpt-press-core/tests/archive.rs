//! Integration tests for the generic archive format.

use std::io::Cursor;

use tpt_press_core::archive::{inspect, pack, unpack, PackOptions, UnpackOptions, MAX_CHUNK_SIZE};
use tpt_press_core::Chunking;
use tpt_press_core::{Error, Result};

const CHUNK: usize = 4096;
const HEADER_LEN: usize = 8;
const FRAME_HEADER_LEN: usize = 12;

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7) % 253) as u8).collect()
}

fn pack_bytes(data: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = PackOptions {
        chunk_size,
        ..PackOptions::default()
    };
    pack(Cursor::new(data), &mut out, &opts).unwrap();
    out
}

fn unpack_bytes(archive: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    unpack(Cursor::new(archive), &mut out, &UnpackOptions::default())?;
    Ok(out)
}

#[test]
fn round_trip_edge_sizes() {
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 300 + 17] {
        let data = sample(len);
        let archive = pack_bytes(&data, CHUNK);
        assert_eq!(unpack_bytes(&archive).unwrap(), data, "length {len}");
    }
}

#[test]
fn round_trip_default_chunk_size() {
    let data = sample(3 * 1024 * 1024 + 5);
    let archive = pack_bytes(&data, PackOptions::default().chunk_size);
    assert_eq!(unpack_bytes(&archive).unwrap(), data);
}

#[test]
fn compressible_data_shrinks() {
    let data = vec![b'a'; 1024 * 1024];
    let archive = pack_bytes(&data, CHUNK);
    assert!(archive.len() < data.len() / 10);
}

#[test]
fn stats_match_stream_lengths() {
    let data = sample(CHUNK * 3 + 1);
    let mut archive = Vec::new();
    let opts = PackOptions {
        chunk_size: CHUNK,
        ..PackOptions::default()
    };
    let packed = pack(Cursor::new(&data), &mut archive, &opts).unwrap();
    assert_eq!(packed.uncompressed_bytes, data.len() as u64);
    assert_eq!(packed.archive_bytes, archive.len() as u64);
    assert_eq!(packed.chunks, 4);

    let mut restored = Vec::new();
    let unpacked = unpack(
        Cursor::new(&archive),
        &mut restored,
        &UnpackOptions::default(),
    )
    .unwrap();
    assert_eq!(unpacked, packed);
}

#[test]
fn bad_magic_is_rejected() {
    let mut archive = pack_bytes(&sample(100), CHUNK);
    archive[0] ^= 0xff;
    assert!(matches!(unpack_bytes(&archive), Err(Error::BadMagic)));
}

#[test]
fn unknown_version_is_rejected() {
    let mut archive = pack_bytes(&sample(100), CHUNK);
    archive[4] = 2;
    assert!(matches!(
        unpack_bytes(&archive),
        Err(Error::UnsupportedVersion(2))
    ));
}

#[test]
fn unknown_flags_are_rejected() {
    let mut archive = pack_bytes(&sample(100), CHUNK);
    // Bit 0 is the dictionary flag. Bit 1 is not assigned.
    archive[6] = 2;
    assert!(matches!(
        unpack_bytes(&archive),
        Err(Error::UnsupportedFlags(2))
    ));
}

#[test]
fn flipped_payload_byte_fails_checksum() {
    let mut archive = pack_bytes(&sample(CHUNK * 2), CHUNK);
    let first_payload = HEADER_LEN + FRAME_HEADER_LEN;
    archive[first_payload + 3] ^= 0x01;
    assert!(matches!(
        unpack_bytes(&archive),
        Err(Error::ChunkCorrupt { index: 0 })
    ));
}

#[test]
fn corrupt_checksum_in_later_chunk_reports_its_index() {
    let mut archive = pack_bytes(&sample(CHUNK * 3), CHUNK);
    // Find the second frame: skip header, then the first frame's payload.
    let first_len = u32::from_le_bytes(archive[8..12].try_into().unwrap()) as usize;
    let second_crc = HEADER_LEN + FRAME_HEADER_LEN + first_len + 8;
    archive[second_crc] ^= 0xff;
    assert!(matches!(
        unpack_bytes(&archive),
        Err(Error::ChunkCorrupt { index: 1 })
    ));
}

#[test]
fn truncated_payload_is_an_io_error() {
    let archive = pack_bytes(&sample(CHUNK * 2), CHUNK);
    let cut = &archive[..archive.len() - 20];
    assert!(matches!(unpack_bytes(cut), Err(Error::Truncated)));
}

#[test]
fn missing_end_marker_is_an_io_error() {
    let archive = pack_bytes(&sample(CHUNK * 2), CHUNK);
    let cut = &archive[..archive.len() - FRAME_HEADER_LEN];
    assert!(matches!(unpack_bytes(cut), Err(Error::Truncated)));
}

#[test]
fn chunk_over_unpack_limit_is_rejected() {
    let archive = pack_bytes(&sample(CHUNK * 2), CHUNK);
    let mut out = Vec::new();
    let opts = UnpackOptions {
        max_chunk_size: CHUNK - 1,
        ..UnpackOptions::default()
    };
    assert!(matches!(
        unpack(Cursor::new(&archive), &mut out, &opts),
        Err(Error::ChunkTooLarge { .. })
    ));
}

#[test]
fn invalid_pack_options_are_rejected() {
    let mut out = Vec::new();
    for chunk_size in [0, MAX_CHUNK_SIZE + 1] {
        let opts = PackOptions {
            chunk_size,
            ..PackOptions::default()
        };
        assert!(matches!(
            pack(Cursor::new(b"x"), &mut out, &opts),
            Err(Error::InvalidArgument(_))
        ));
    }
    let opts = PackOptions {
        level: 0,
        ..PackOptions::default()
    };
    assert!(matches!(
        pack(Cursor::new(b"x"), &mut out, &opts),
        Err(Error::InvalidArgument(_))
    ));
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn content_defined_archives_round_trip() {
    let data = noise(700_000, 9);
    let opts = PackOptions {
        chunk_size: 8192,
        chunking: Chunking::ContentDefined,
        ..PackOptions::default()
    };
    let mut archive = Vec::new();
    let stats = pack(Cursor::new(&data), &mut archive, &opts).unwrap();
    assert!(stats.chunks > 1);

    let mut restored = Vec::new();
    unpack(
        Cursor::new(&archive),
        &mut restored,
        &UnpackOptions::default(),
    )
    .unwrap();
    assert_eq!(restored, data);
}

#[test]
fn content_defined_chunk_count_depends_on_the_average() {
    let data = noise(400_000, 10);
    let count = |average: usize| {
        let opts = PackOptions {
            chunk_size: average,
            chunking: Chunking::ContentDefined,
            ..PackOptions::default()
        };
        pack(Cursor::new(&data), Vec::new(), &opts).unwrap().chunks
    };
    // Smaller averages give more chunks, roughly in proportion.
    let small = count(2048);
    let large = count(16384);
    assert!(small > large * 4, "{small} vs {large}");
}

#[test]
fn bad_content_defined_averages_are_rejected() {
    for average in [0, 100, 3000, 32 * 1024 * 1024] {
        let opts = PackOptions {
            chunk_size: average,
            chunking: Chunking::ContentDefined,
            ..PackOptions::default()
        };
        assert!(
            matches!(
                pack(Cursor::new(b"x"), Vec::new(), &opts),
                Err(Error::InvalidArgument(_))
            ),
            "average {average} was accepted"
        );
    }
}

/// JSON lines that share their keys, as logs and API responses often do.
fn json_lines(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| {
            format!(
                "{{\"timestamp\":\"2026-01-01T00:{:02}:{:02}Z\",\"level\":\"info\",\"service\":\"api\",\"latency_ms\":{}}}\n",
                (i / 60) % 60,
                i % 60,
                (i * 37) % 900
            )
        })
        .collect::<String>()
        .into_bytes()
}

#[test]
fn dictionary_archives_round_trip_and_shrink_repetitive_data() {
    let data = json_lines(20_000);
    let plain = PackOptions {
        chunk_size: 8192,
        ..PackOptions::default()
    };
    let trained = PackOptions {
        dictionary: Some(64 * 1024),
        ..plain
    };

    let mut without = Vec::new();
    pack(Cursor::new(&data), &mut without, &plain).unwrap();
    let mut with = Vec::new();
    pack(Cursor::new(&data), &mut with, &trained).unwrap();

    let summary = inspect(Cursor::new(&with)).unwrap();
    assert!(summary.dictionary_bytes > 0);
    // The dictionary is stored in the archive, so compare the chunk data alone.
    let chunk_bytes = with.len() - 4 - summary.dictionary_bytes;
    assert!(
        chunk_bytes * 3 < without.len() * 2,
        "{chunk_bytes} vs {}",
        without.len()
    );

    let mut restored = Vec::new();
    unpack(Cursor::new(&with), &mut restored, &UnpackOptions::default()).unwrap();
    assert_eq!(restored, data);
}

#[test]
fn small_inputs_are_written_without_a_dictionary() {
    let data = b"too small to train on".repeat(10);
    let opts = PackOptions {
        dictionary: Some(64 * 1024),
        ..PackOptions::default()
    };
    let mut archive = Vec::new();
    pack(Cursor::new(&data), &mut archive, &opts).unwrap();
    assert_eq!(inspect(Cursor::new(&archive)).unwrap().dictionary_bytes, 0);

    let mut restored = Vec::new();
    unpack(
        Cursor::new(&archive),
        &mut restored,
        &UnpackOptions::default(),
    )
    .unwrap();
    assert_eq!(restored, data);
}

#[test]
fn bad_dictionary_sizes_are_rejected() {
    for size in [0, 2 * 1024 * 1024] {
        let opts = PackOptions {
            dictionary: Some(size),
            ..PackOptions::default()
        };
        assert!(matches!(
            pack(Cursor::new(b"x"), Vec::new(), &opts),
            Err(Error::InvalidArgument(_))
        ));
    }
}

#[test]
fn damaged_dictionary_headers_are_reported() {
    let data = json_lines(20_000);
    let opts = PackOptions {
        chunk_size: 8192,
        dictionary: Some(64 * 1024),
        ..PackOptions::default()
    };
    let mut archive = Vec::new();
    pack(Cursor::new(&data), &mut archive, &opts).unwrap();

    // A zero length is not a dictionary.
    let mut zero = archive.clone();
    zero[8..12].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        unpack(Cursor::new(&zero), Vec::new(), &UnpackOptions::default()),
        Err(Error::CorruptHeader(_))
    ));

    // A header that promises a dictionary but ends early.
    let cut = &archive[..10];
    assert!(matches!(
        unpack(Cursor::new(cut), Vec::new(), &UnpackOptions::default()),
        Err(Error::Truncated)
    ));
}

#[test]
fn total_output_limit_stops_an_archive_that_expands_too_far() {
    let data = sample(100_000);
    let mut archive = Vec::new();
    pack(Cursor::new(&data), &mut archive, &PackOptions::default()).unwrap();

    let limited = UnpackOptions {
        max_total_size: Some(50_000),
        ..UnpackOptions::default()
    };
    assert!(matches!(
        unpack(Cursor::new(&archive), Vec::new(), &limited),
        Err(Error::ArchiveTooLarge { limit: 50_000 })
    ));

    let exact = UnpackOptions {
        max_total_size: Some(100_000),
        ..UnpackOptions::default()
    };
    let mut restored = Vec::new();
    unpack(Cursor::new(&archive), &mut restored, &exact).unwrap();
    assert_eq!(restored, data);
}
