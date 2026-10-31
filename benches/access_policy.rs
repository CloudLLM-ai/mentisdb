//! Benchmarks for the cost of write access control on the append hot path.
//!
//! - **`append_access`**: append latency/throughput with an `Open` chain
//!   (unsigned, the pre-access-control baseline), an `Open` chain with a v2
//!   signature (verified), and `Signed` / `Allowlist` chains. Signing happens
//!   outside the timed section (clients sign), so the difference between the
//!   `open_unsigned` row and the others is the verify-on-append overhead:
//!   registry lookup, payload rebuild and one Ed25519 verification.
//! - **`primitives`**: the Ed25519 verification and v2 payload construction on
//!   their own, to attribute that overhead.
//!
//! Run with `cargo bench --bench access_policy`.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::signing::{
    key_registration_payload, thought_payload_v2, verify_ed25519_signature, StoredSignature,
};
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtType};
use std::hint::black_box;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CHAIN: &str = "bench-access";
const AGENT: &str = "bench-agent";
const KEY_ID: &str = "bench-key";

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A chain with one registered key and, for `mode != Open`, a policy listing it.
fn chain_with(mode: AccessMode) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("mentisdb-bench-access-")
        .tempdir()
        .unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    chain.set_auto_flush(false).unwrap();
    chain.upsert_agent(AGENT, None, None, None, None).unwrap();
    let key = signing_key();
    let public = key.verifying_key().to_bytes().to_vec();
    let now = chrono::Utc::now().timestamp();
    let payload = key_registration_payload(CHAIN, AGENT, KEY_ID, &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(
            AGENT,
            KEY_ID,
            PublicKeyAlgorithm::Ed25519,
            public.clone(),
            &proof,
        )
        .unwrap();
    if mode != AccessMode::Open {
        let record =
            PolicyRecord::new(0, mode, vec![AllowedKey::new(AGENT, KEY_ID, &public)], "").unwrap();
        let head = chain.head_hash().unwrap_or("").to_string();
        let payload = thought_payload_v2(CHAIN, AGENT, &head, &policy_thought_input(&record));
        let signature = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
        chain
            .set_access_policy(AGENT, &record, KEY_ID, signature)
            .unwrap();
    }
    (chain, dir)
}

fn signed_input(chain: &MentisDb, key: &SigningKey, n: u64) -> ThoughtInput {
    let mut input = ThoughtInput::new(ThoughtType::Insight, format!("benchmark thought {n}"));
    let head = chain.head_hash().unwrap_or("").to_string();
    let payload = thought_payload_v2(CHAIN, AGENT, &head, &input);
    input.signing_key_id = Some(KEY_ID.to_string());
    input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
    input
}

fn bench_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_access");
    group.throughput(Throughput::Elements(1));
    let key = signing_key();
    let cases: [(&str, AccessMode, bool); 4] = [
        ("open_unsigned", AccessMode::Open, false),
        ("open_signed_v2", AccessMode::Open, true),
        ("signed_policy", AccessMode::Signed, true),
        ("allowlist_policy", AccessMode::Allowlist, true),
    ];
    for (name, mode, signed) in cases {
        let (mut chain, _dir) = chain_with(mode);
        let mut n = 0u64;
        group.bench_function(name, |b| {
            b.iter_custom(|iters| {
                let mut timed = Duration::ZERO;
                for _ in 0..iters {
                    n += 1;
                    let input = if signed {
                        signed_input(&chain, &key, n)
                    } else {
                        ThoughtInput::new(ThoughtType::Insight, format!("benchmark thought {n}"))
                    };
                    let start = Instant::now();
                    black_box(chain.append_thought(AGENT, input).unwrap());
                    timed += start.elapsed();
                }
                timed
            });
        });
    }
    group.finish();
}

fn bench_primitives(c: &mut Criterion) {
    let mut group = c.benchmark_group("primitives");
    let key = signing_key();
    let public = key.verifying_key().to_bytes();
    let input = ThoughtInput::new(
        ThoughtType::Insight,
        "a representative thought of moderate length for hashing",
    );
    let payload = thought_payload_v2(CHAIN, AGENT, &"ab".repeat(32), &input);
    let signature = key.sign(&payload).to_bytes();
    group.bench_function("ed25519_verify_strict", |b| {
        b.iter(|| {
            verify_ed25519_signature(
                black_box(&public),
                black_box(&payload),
                black_box(&signature),
            )
            .unwrap()
        });
    });
    group.bench_function("thought_payload_v2", |b| {
        b.iter(|| {
            thought_payload_v2(
                black_box(CHAIN),
                black_box(AGENT),
                black_box("prev"),
                black_box(&input),
            )
        });
    });
    group.finish();
}

criterion_group!(benches, bench_append, bench_primitives);
criterion_main!(benches);
