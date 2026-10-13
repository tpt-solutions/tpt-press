//! Hostile-input tests for the logs archive reader.
//!
//! Random and damaged inputs must never panic. They must be rejected, or read
//! as some set of records. Nothing here checks the contents of accepted output.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_press_logs::{inspect, pack, query, PackOptions, Query};

fn valid_archive() -> Vec<u8> {
    let mut input = Vec::new();
    for i in 0..300 {
        input.extend_from_slice(
            format!(
                "{{\"timestamp\":\"2026-10-13T00:{:02}:{:02}Z\",\"level\":\"info\",\"msg\":\"m{i}\"}}\n",
                (i / 60) % 60,
                i % 60
            )
            .as_bytes(),
        );
    }
    let mut out = Vec::new();
    let opts = PackOptions {
        block_size: 2048,
        ..PackOptions::default()
    };
    pack(Cursor::new(input), &mut out, &opts).unwrap();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = inspect(Cursor::new(&bytes));
        let mut out = Vec::new();
        let _ = query(Cursor::new(&bytes), &mut out, &Query::default());
    }

    #[test]
    fn damaged_archives_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
        time_from in 0i64..1_000_000_000_000_000_000,
    ) {
        let mut archive = valid_archive();
        for (position, value) in edits {
            let at = position % archive.len();
            archive[at] = value;
        }
        archive.truncate(cut % (archive.len() + 1));

        let _ = inspect(Cursor::new(&archive));
        let q = Query {
            from: Some(time_from),
            to: None,
            columns: Some(vec!["level".to_owned()]),
        };
        let mut out = Vec::new();
        let _ = query(Cursor::new(&archive), &mut out, &q);
    }
}
