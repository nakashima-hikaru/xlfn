//! Actual token authentication/cache path, including bounded-cache overflow.
use super::token::{HandleId, HandleToken, TokenCodec};
use crate::generation::BindingGeneration;
use std::time::Instant;

pub fn token_cache_associativity_probe() -> serde_json::Value {
    let codec = TokenCodec::new(7, [19; 32]);
    let corpus: Vec<_> = (0..1024)
        .map(|slot| {
            let id = HandleId {
                slot,
                generation: BindingGeneration::ONE,
            };
            (codec.format(id), id)
        })
        .collect();
    let mut cases = Vec::new();
    for (name, count, colliding) in [
        ("warm_one", 1, false),
        ("natural_8", 8, false),
        ("natural_16", 16, false),
        ("natural_32", 32, false),
        ("natural_64", 64, false),
        ("natural_128", 128, false),
        ("same_bucket_2", 2, true),
        ("same_bucket_4", 4, true),
        ("same_bucket_8", 8, true),
        ("same_bucket_16", 16, true),
    ] {
        let tokens: Vec<_> = corpus
            .iter()
            .filter(|(token, _)| !colliding || token.ends_with('0'))
            .take(count)
            .collect();
        assert_eq!(tokens.len(), count);
        let mut elapsed = Vec::new();
        for round in 0..7 {
            let started = Instant::now();
            for index in 0..100_000 {
                let (token, id) = tokens[index % tokens.len()];
                let verified = codec
                    .parse(
                        std::ptr::from_ref(&codec).addr(),
                        HandleToken::new(std::hint::black_box(token)),
                    )
                    .unwrap();
                assert_eq!(verified.id, *id);
                std::hint::black_box(verified);
            }
            if round > 0 {
                elapsed.push(started.elapsed().as_nanos() as u64);
            }
        }
        elapsed.sort_unstable();
        cases.push(serde_json::json!({ "case": name, "tokens": count, "operations": 100_000,
            "tag_nibbles": tokens.iter().map(|(token,_)| token.chars().last().unwrap()).collect::<String>(),
            "rounds_ns": elapsed, "median_ns": elapsed[elapsed.len()/2] }));
    }
    let mut elapsed = Vec::new();
    for round in 0..7 {
        let started = Instant::now();
        for index in 0..100_000 {
            let token = codec.format(std::hint::black_box(corpus[index % corpus.len()].1));
            std::hint::black_box(token);
        }
        if round > 0 {
            elapsed.push(started.elapsed().as_nanos() as u64);
        }
    }
    elapsed.sort_unstable();
    cases.push(serde_json::json!({ "case": "format", "operations": 100_000,
        "verification_cache_storage_bytes_per_thread": super::token::VERIFIED_TOKEN_CACHE_STORAGE_BYTES,
        "rounds_ns": elapsed, "median_ns": elapsed[elapsed.len()/2] }));
    serde_json::Value::Array(cases)
}
