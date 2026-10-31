#![no_main]
//! Fuzzes the v2 thought signing payload: it is deterministic, domain-prefixed,
//! and different (chain, agent, head) triples never collide, even when their
//! concatenations are equal (length prefixes must prevent ambiguity).

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mentisdb::signing::{thought_payload_v2, THOUGHT_V2_DOMAIN};
use mentisdb::{ThoughtInput, ThoughtType};

#[derive(Arbitrary, Debug)]
struct Input {
    a: (String, String, String),
    b: (String, String, String),
    content: String,
    tags: Vec<String>,
    refs: Vec<u64>,
    importance_bits: u32,
}

fuzz_target!(|input: Input| {
    let mut thought = ThoughtInput::new(ThoughtType::Insight, input.content.clone());
    thought.tags = input.tags.into_iter().take(16).collect();
    thought.refs = input.refs.into_iter().take(16).collect();
    thought.importance = f32::from_bits(input.importance_bits);
    if thought.importance.is_nan() {
        thought.importance = 0.0;
    }
    let payload = |t: &(String, String, String)| thought_payload_v2(&t.0, &t.1, &t.2, &thought);
    let (pa, pb) = (payload(&input.a), payload(&input.b));
    assert!(pa.starts_with(THOUGHT_V2_DOMAIN));
    assert_eq!(pa, payload(&input.a), "payload must be deterministic");
    if input.a != input.b {
        assert_ne!(pa, pb, "distinct chain/agent/head triples collided");
    }
});
