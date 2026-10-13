//! Property tests for the generic archive format.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_press_core::archive::{pack, unpack, PackOptions, UnpackOptions};

fn pack_bytes(data: &[u8], chunk_size: usize, level: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = PackOptions {
        chunk_size,
        level,
        ..PackOptions::default()
    };
    pack(Cursor::new(data), &mut out, &opts).unwrap();
    out
}

fn unpack_bytes(archive: &[u8]) -> tpt_press_core::Result<Vec<u8>> {
    let mut out = Vec::new();
    unpack(Cursor::new(archive), &mut out, &UnpackOptions::default())?;
    Ok(out)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn any_input_round_trips(
        data in proptest::collection::vec(any::<u8>(), 0..200_000),
        chunk_size in 1usize..70_000,
        level in 1i32..=19,
    ) {
        let archive = pack_bytes(&data, chunk_size, level);
        prop_assert_eq!(unpack_bytes(&archive).unwrap(), data);
    }

    #[test]
    fn single_byte_corruption_never_yields_wrong_data(
        data in proptest::collection::vec(any::<u8>(), 1..50_000),
        chunk_size in 1usize..20_000,
        position in any::<proptest::sample::Index>(),
        flip in 1u8..=255,
    ) {
        let mut archive = pack_bytes(&data, chunk_size, 3);
        let at = position.index(archive.len());
        archive[at] ^= flip;

        // Corruption must be detected, or leave the output unchanged.
        // It must never produce different bytes without an error.
        if let Ok(restored) = unpack_bytes(&archive) {
            prop_assert_eq!(restored, data);
        }
    }

    #[test]
    fn any_prefix_of_an_archive_is_rejected(
        data in proptest::collection::vec(any::<u8>(), 1..50_000),
        chunk_size in 1usize..20_000,
        cut in any::<proptest::sample::Index>(),
    ) {
        let archive = pack_bytes(&data, chunk_size, 3);
        // Cut strictly inside the archive, so something is always missing.
        let len = cut.index(archive.len());
        prop_assert!(unpack_bytes(&archive[..len]).is_err());
    }
}
