//! Hostile-input tests for the generic archive reader.
//!
//! Random and damaged inputs must never panic. A damaged archive must either be
//! rejected, or unpack to exactly the original bytes.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_press_core::archive::{inspect, unpack, UnpackOptions};
use tpt_press_core::{pack, PackOptions};

fn valid_archive(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = PackOptions {
        chunk_size: 512,
        ..PackOptions::default()
    };
    pack(Cursor::new(data), &mut out, &opts).unwrap();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = inspect(Cursor::new(&bytes));
        let mut out = Vec::new();
        let _ = unpack(Cursor::new(&bytes), &mut out, &UnpackOptions::default());
    }

    #[test]
    fn damaged_archives_are_rejected_or_restored_exactly(
        data in proptest::collection::vec(any::<u8>(), 0..3000),
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let mut archive = valid_archive(&data);
        for (position, value) in edits {
            let at = position % archive.len();
            archive[at] = value;
        }
        archive.truncate(cut % (archive.len() + 1));

        let _ = inspect(Cursor::new(&archive));
        let mut out = Vec::new();
        if unpack(Cursor::new(&archive), &mut out, &UnpackOptions::default()).is_ok() {
            prop_assert_eq!(out, data);
        }
    }
}
