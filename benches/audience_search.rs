//! Cost of the public/private audience filter on retrieval.
//!
//! One chain of `THOUGHTS` thoughts: the first half written while public, then
//! the chain goes private and the second half is signed. Ranked search for an
//! authorized reader (everything visible) is compared with a public reader
//! (half hidden), which pays for the segment lookups, the isolated BM25
//! statistics and the recency anchor. A chain with no private segment is the
//! baseline every existing deployment runs: the audience check is then one
//! comparison.
//!
//! Run with `cargo bench --bench audience_search`.

use criterion::{criterion_group, criterion_main, Criterion};
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::audience::Audience;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{
    MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtQuery, ThoughtType,
};
use std::hint::black_box;
use tempfile::TempDir;

const CHAIN: &str = "bench-audience";
const THOUGHTS: usize = 10_000;

fn words(n: usize) -> String {
    const WORDS: [&str; 12] = [
        "pelican",
        "registry",
        "postgres",
        "rollback",
        "deploy",
        "latency",
        "vector",
        "thesaurus",
        "ranking",
        "segment",
        "signature",
        "operator",
    ];
    format!(
        "{} {} {} note {n}",
        WORDS[n % 12],
        WORDS[(n / 12) % 12],
        WORDS[(n / 144) % 12]
    )
}

fn build(private: bool) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("mentisdb-bench-audience-")
        .tempdir()
        .unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    chain.set_auto_flush(false).unwrap();
    for n in 0..THOUGHTS {
        chain
            .append("bench", ThoughtType::Insight, &words(n))
            .unwrap();
    }
    if private {
        let operator = OperatorIdentity::load_or_create(dir.path()).unwrap();
        let key = SigningKey::from_bytes(&[9u8; 32]);
        apply(
            &mut chain,
            &operator,
            &OperatorOp::SetMode {
                mode: AccessMode::Allowlist,
                keys: vec![],
            },
        )
        .unwrap();
        apply(
            &mut chain,
            &operator,
            &OperatorOp::AllowKey {
                agent_id: "writer".into(),
                key_id: "w1".into(),
                public_key: to_hex(&key.verifying_key().to_bytes()),
            },
        )
        .unwrap();
        for n in 0..THOUGHTS {
            let head = chain.head_hash().unwrap_or("").to_string();
            let mut input = ThoughtInput::new(ThoughtType::Insight, words(n + THOUGHTS));
            let payload = thought_payload_v2(CHAIN, "writer", &head, &input);
            input.signing_key_id = Some("w1".into());
            input.thought_signature =
                Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
            chain.append_thought("writer", input).unwrap();
        }
    }
    (chain, dir)
}

fn bench(c: &mut Criterion) {
    let (plain, _plain_dir) = build(false);
    let (private, _private_dir) = build(true);
    let query = |audience| {
        RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_audience(audience))
            .with_text("pelican registry rollback")
            .with_limit(10)
    };
    let mut group = c.benchmark_group("ranked_search_audience");
    group.sample_size(30);
    group.bench_function("public_chain_no_private_segment", |b| {
        let q = query(Audience::Public);
        b.iter(|| black_box(plain.query_ranked(black_box(&q)).hits.len()));
    });
    group.bench_function("private_chain_authorized_reader", |b| {
        let q = query(Audience::Authorized);
        b.iter(|| black_box(private.query_ranked(black_box(&q)).hits.len()));
    });
    group.bench_function("private_chain_public_reader", |b| {
        let q = query(Audience::Public);
        b.iter(|| black_box(private.query_ranked(black_box(&q)).hits.len()));
    });
    group.finish();

    let mut group = c.benchmark_group("audience_primitives");
    group.bench_function("visible_thought_count_public", |b| {
        b.iter(|| black_box(private.visible_thought_count(Audience::Public)));
    });
    group.bench_function("is_visible_index", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i = (i + 7) % (2 * THOUGHTS as u64);
            black_box(private.is_visible_index(black_box(i), Audience::Public))
        });
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
