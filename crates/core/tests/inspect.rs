//! Integration tests for `inspect`, which reads archive structure without decompressing.

use std::io::Cursor;

use tpt_press_core::{inspect, pack, Error, PackOptions, UnpackOptions};

const CHUNK: usize = 4096;
const HEADER_LEN: u64 = 8;
const FRAME_HEADER_LEN: u64 = 12;

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 7) % 253) as u8).collect()
}

fn pack_bytes(data: &[u8]) -> (Vec<u8>, tpt_press_core::Stats) {
    let mut out = Vec::new();
    let opts = PackOptions {
        chunk_size: CHUNK,
        ..PackOptions::default()
    };
    let stats = pack(Cursor::new(data), &mut out, &opts).unwrap();
    (out, stats)
}

#[test]
fn stats_match_pack() {
    let data = sample(CHUNK * 5 + 3);
    let (archive, packed) = pack_bytes(&data);
    let inspection = inspect(Cursor::new(&archive)).unwrap();

    assert_eq!(inspection.stats, packed);
    assert_eq!(inspection.chunks.len(), 6);
    assert_eq!(inspection.stats.archive_bytes, archive.len() as u64);
}

#[test]
fn chunk_offsets_point_at_frame_headers() {
    let data = sample(CHUNK * 3);
    let (archive, _) = pack_bytes(&data);
    let inspection = inspect(Cursor::new(&archive)).unwrap();

    let mut expected_offset = HEADER_LEN;
    let mut total_uncompressed = 0u64;
    for chunk in &inspection.chunks {
        assert_eq!(chunk.offset, expected_offset);
        // The frame header at this offset must declare the same sizes.
        let at = chunk.offset as usize;
        let declared_compressed = u32::from_le_bytes(archive[at..at + 4].try_into().unwrap());
        let declared_uncompressed = u32::from_le_bytes(archive[at + 4..at + 8].try_into().unwrap());
        assert_eq!(declared_compressed, chunk.compressed_len);
        assert_eq!(declared_uncompressed, chunk.uncompressed_len);

        expected_offset += FRAME_HEADER_LEN + u64::from(chunk.compressed_len);
        total_uncompressed += u64::from(chunk.uncompressed_len);
    }
    assert_eq!(total_uncompressed, data.len() as u64);
}

#[test]
fn ratio_reports_compression() {
    let data = vec![b'z'; CHUNK * 10];
    let (archive, _) = pack_bytes(&data);
    let inspection = inspect(Cursor::new(&archive)).unwrap();
    assert!(inspection.ratio() > 10.0, "ratio {}", inspection.ratio());
}

#[test]
fn empty_archive_has_no_chunks() {
    let (archive, _) = pack_bytes(&[]);
    let inspection = inspect(Cursor::new(&archive)).unwrap();
    assert!(inspection.chunks.is_empty());
    assert_eq!(inspection.stats.uncompressed_bytes, 0);
}

#[test]
fn corrupt_payload_is_reported() {
    let (mut archive, _) = pack_bytes(&sample(CHUNK * 2));
    let second = inspect(Cursor::new(&archive)).unwrap().chunks[1];
    let payload_at = second.offset as usize + FRAME_HEADER_LEN as usize;
    archive[payload_at] ^= 0xff;
    assert!(matches!(
        inspect(Cursor::new(&archive)),
        Err(Error::ChunkCorrupt { index: 1 })
    ));
}

#[test]
fn truncated_archive_is_reported() {
    let (archive, _) = pack_bytes(&sample(CHUNK * 2));
    let cut = &archive[..archive.len() - 1];
    assert!(matches!(inspect(Cursor::new(cut)), Err(Error::Truncated)));
}

#[test]
fn non_archive_is_rejected() {
    assert!(matches!(
        inspect(Cursor::new(b"definitely not an archive")),
        Err(Error::BadMagic)
    ));
}

#[test]
fn inspect_and_unpack_agree_on_chunk_count() {
    let data = sample(CHUNK * 7 + 100);
    let (archive, _) = pack_bytes(&data);
    let inspection = inspect(Cursor::new(&archive)).unwrap();

    let mut restored = Vec::new();
    let unpacked = tpt_press_core::unpack(
        Cursor::new(&archive),
        &mut restored,
        &UnpackOptions::default(),
    )
    .unwrap();
    assert_eq!(inspection.stats.chunks, unpacked.chunks);
    assert_eq!(restored, data);
}
