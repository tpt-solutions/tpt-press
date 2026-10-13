//! Writes a seeded JSON Lines log corpus, for the benchmarks in `docs/benchmarks.md`.
//!
//! Run with `cargo run -p tpt-press-cli --release --example make_logs_corpus -- <output> [records]`.
//! The same arguments always write the same bytes.

use std::fs::File;
use std::io::{BufWriter, Write};

const LEVELS: [&str; 4] = ["debug", "info", "warn", "error"];
const SERVICES: [&str; 6] = ["auth", "billing", "search", "gateway", "worker", "catalog"];
const ROUTES: [&str; 8] = [
    "/v1/login",
    "/v1/charge",
    "/v1/search",
    "/v1/items",
    "/v1/items/{id}",
    "/v1/health",
    "/v1/refund",
    "/v1/session",
];

/// A small, fixed generator (64-bit LCG), so the corpus does not depend on any crate.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let output = args
        .first()
        .ok_or("usage: make_logs_corpus <output> [records]")?;
    let records: u64 = match args.get(1) {
        Some(n) => n.parse()?,
        None => 400_000,
    };

    let mut rng = Lcg(42);
    let mut out = BufWriter::new(File::create(output)?);
    // Timestamps step through a day, one record every 200 ms, with jitter.
    let start: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
    for i in 0..records {
        let second = start + (i as i64) / 5;
        let millis = (i % 5) * 200 + rng.below(50);
        let (y, mo, d, h, mi, s) = civil(second);
        writeln!(
            out,
            "{{\"timestamp\":\"{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z\",\"level\":\"{}\",\"service\":\"{}\",\"route\":\"{}\",\"status\":{},\"latency_ms\":{},\"user_id\":{}}}",
            LEVELS[pick(&mut rng, &[0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 3])],
            SERVICES[rng.below(SERVICES.len() as u64) as usize],
            ROUTES[rng.below(ROUTES.len() as u64) as usize],
            [200, 200, 200, 201, 204, 400, 404, 500][rng.below(8) as usize],
            rng.below(900) + 3,
            rng.below(250_000),
        )?;
    }
    out.flush()?;
    Ok(())
}

fn pick(rng: &mut Lcg, weights: &[usize]) -> usize {
    weights[rng.below(weights.len() as u64) as usize]
}

/// Civil date and time from Unix seconds (UTC), using the days-from-civil algorithm.
fn civil(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}
