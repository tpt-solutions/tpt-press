//! Integration tests for the generic archive format.

use std::io::Cursor;

use tpt_press_core::archive::{pack, unpack, PackOptions, UnpackOptions, MAX_CHUNK_SIZE};
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
    archive[6] = 1;
    assert!(matches!(
        unpack_bytes(&archive),
        Err(Error::UnsupportedFlags(1))
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
