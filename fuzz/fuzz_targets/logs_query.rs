#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use tpt_press_logs::{inspect, query, Query};

fuzz_target!(|data: &[u8]| {
    let _ = inspect(Cursor::new(data));
    let mut out = Vec::new();
    let _ = query(Cursor::new(data), &mut out, &Query::default());
    let mut out = Vec::new();
    let q = Query {
        from: Some(0),
        to: Some(i64::MAX),
        columns: Some(vec!["level".to_owned()]),
    };
    let _ = query(Cursor::new(data), &mut out, &q);
});
