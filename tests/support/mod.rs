//! Shared corpora for the integration tests.

#![allow(dead_code, reason = "each integration test uses a different subset")]

/// Deterministic pseudo-random bytes.
pub fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// Text-like bytes drawn from a small vocabulary.
pub fn text(len: usize, seed: u32) -> Vec<u8> {
    const WORDS: [&[u8]; 12] = [
        b"openzl ",
        b"format ",
        b"aware ",
        b"compression ",
        b"graph ",
        b"codec ",
        b"zstd ",
        b"chunk ",
        b"frame ",
        b"stream ",
        b"the ",
        b"of ",
    ];
    let picks = noise(len / 2 + 1, seed);
    let mut out = Vec::with_capacity(len + 16);
    for pick in picks {
        if out.len() >= len {
            break;
        }
        out.extend_from_slice(WORDS[usize::from(pick) % WORDS.len()]);
    }
    out.truncate(len);
    out
}

/// A named set of inputs covering the encoder's decisions.
pub fn corpus() -> Vec<(String, Vec<u8>)> {
    let large = if cfg!(miri) { 5_000 } else { 300_000 };
    vec![
        ("empty".into(), Vec::new()),
        ("one".into(), b"x".to_vec()),
        ("nine".into(), b"123456789".to_vec()),
        ("ten".into(), b"1234567890".to_vec()),
        ("eleven-repeat".into(), vec![b'a'; 11]),
        ("noise-40".into(), noise(40, 1)),
        ("noise-large".into(), noise(large, 2)),
        ("text-1k".into(), text(1000, 3)),
        ("text-large".into(), text(large, 4)),
        ("zeros-large".into(), vec![0; large]),
        (
            "mixed".into(),
            [text(large / 2, 5), noise(large / 2, 6)].concat(),
        ),
    ]
}
