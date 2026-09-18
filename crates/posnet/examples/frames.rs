//! Standalone frame encoding/decoding throughput benchmark.

use std::time::Instant;

use posnet::{decode_frame, encode_frame};

fn main() {
    let rounds: u32 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(200_000);
    let params = [
        ("na", "Wymiana oleju silnikowego".to_string()),
        ("vt", "2".to_string()),
        ("il", "1.000".to_string()),
        ("wa", "12345".to_string()),
    ];
    let started = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..rounds {
        let frame = encode_frame("trline", &params).expect("encode");
        checksum += decode_frame(&frame).expect("decode").1.len();
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "encode+decode roundtrips/s {} (checksum {checksum})",
        (f64::from(rounds) / elapsed) as u64
    );
}
