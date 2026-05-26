use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::embedding_synonyms::EmbeddingSynonymGenerator;
use mentisdb::search::thesaurus;
use mentisdb::{MentisDb, RankedSearchQuery, ThoughtInput, ThoughtType};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_quality_research_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// One query with ground-truth relevant thought indices.
struct EvalQuery {
    text: &'static str,
    relevant: HashSet<usize>,
}

/// Metrics averaged over all queries.
#[derive(Clone, Copy)]
struct Metrics {
    recall_at_5: f32,
    recall_at_10: f32,
    precision_at_5: f32,
    mrr: f32,
    ndcg_at_5: f32,
}

fn compute_ndcg(relevant: &HashSet<usize>, results: &[usize], k: usize) -> f32 {
    let k = k.min(results.len());
    let mut dcg = 0.0_f32;
    for (i, &idx) in results.iter().take(k).enumerate() {
        let rel = if relevant.contains(&idx) { 1.0 } else { 0.0 };
        dcg += rel / ((i + 2) as f32).log2();
    }
    let ideal = relevant.len().min(k);
    let mut idcg = 0.0_f32;
    for i in 0..ideal {
        idcg += 1.0 / ((i + 2) as f32).log2();
    }
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

fn evaluate(
    chain: &MentisDb,
    queries: &[EvalQuery],
    synonyms: &HashMap<String, Vec<String>>,
    synonym_weight: f32,
) -> Metrics {
    let mut total_r5 = 0.0_f32;
    let mut total_r10 = 0.0_f32;
    let mut total_p5 = 0.0_f32;
    let mut total_mrr = 0.0_f32;
    let mut total_ndcg5 = 0.0_f32;

    for q in queries {
        let mut rq = RankedSearchQuery::new()
            .with_text(q.text)
            .with_limit(10)
            .with_synonyms(synonyms.clone(), synonym_weight);
        rq.synonyms = synonyms.clone();
        rq.synonym_weight = synonym_weight;
        let ranked = chain.query_ranked(&rq);
        let results: Vec<usize> = ranked.hits.iter().map(|h| h.thought.index as usize).collect();

        let hits5 = results.iter().take(5).filter(|idx| q.relevant.contains(*idx)).count();
        let hits10 = results.iter().take(10).filter(|idx| q.relevant.contains(*idx)).count();
        let nrel = q.relevant.len().max(1);

        total_r5 += hits5 as f32 / nrel as f32;
        total_r10 += hits10 as f32 / nrel as f32;
        total_p5 += hits5 as f32 / 5.0;
        total_mrr += results.iter().enumerate().find_map(|(i, idx)| {
            if q.relevant.contains(idx) {
                Some(1.0 / (i + 1) as f32)
            } else {
                None
            }
        }).unwrap_or(0.0);
        total_ndcg5 += compute_ndcg(&q.relevant, &results, 5);
    }

    let n = queries.len() as f32;
    Metrics {
        recall_at_5: total_r5 / n,
        recall_at_10: total_r10 / n,
        precision_at_5: total_p5 / n,
        mrr: total_mrr / n,
        ndcg_at_5: total_ndcg5 / n,
    }
}

fn build_corpus() -> (MentisDb, Vec<EvalQuery>, Vec<String>, PathBuf) {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-quality-research-v3").unwrap();

    chain
        .upsert_agent(
            "backend-dev",
            Some("Backend Developer"),
            Some("search-team"),
            Some("Builds fast retrieval systems and debugs latency."),
            None,
        )
        .unwrap();

    chain
        .upsert_agent(
            "frontend-dev",
            Some("Frontend Engineer"),
            Some("ui-team"),
            Some("Designs rapid search dashboards and swift user interfaces."),
            None,
        )
        .unwrap();

    // Define 12 queries. For each: 2 exact-match docs, 2 synonym-match docs.
    let mut docs: Vec<String> = Vec::with_capacity(140);

    // Q1: fast retrieval
    docs.push("fast retrieval systems for production".into());
    docs.push("fast retrieval pipeline architecture".into());
    docs.push("quick search infrastructure design".into());
    docs.push("rapid lookup framework overview".into());

    // Q2: search ranking
    docs.push("search ranking algorithm implementation".into());
    docs.push("search ranking quality metrics".into());
    docs.push("lookup scoring methodology guide".into());
    docs.push("querying ordering system description".into());

    // Q3: went to store
    docs.push("went to store for supplies".into());
    docs.push("went to store yesterday".into());
    docs.push("travel to shop for equipment".into());
    docs.push("move to market for resources".into());

    // Q4: debugging latency
    docs.push("debugging latency in microservices".into());
    docs.push("debugging latency with tracing tools".into());
    docs.push("diagnostics delay analysis report".into());
    docs.push("tracing slowness root cause study".into());

    // Q5: backend engineer
    docs.push("backend engineer scaling challenges".into());
    docs.push("backend engineer api design patterns".into());
    docs.push("server developer deployment strategies".into());
    docs.push("service architect cloud migration".into());

    // Q6: graph expansion
    docs.push("graph expansion for neighborhood search".into());
    docs.push("graph expansion traversal algorithms".into());
    docs.push("network tree growth analysis".into());
    docs.push("tree traversal breadth first study".into());

    // Q7: data integrity
    docs.push("data integrity validation pipeline".into());
    docs.push("data integrity check scheduling".into());
    docs.push("records consistency audit process".into());
    docs.push("information soundness verification step".into());

    // Q8: vector search
    docs.push("vector search embedding similarity".into());
    docs.push("vector search nearest neighbor index".into());
    docs.push("tensor lookup distance computation".into());
    docs.push("embedding querying cosine match".into());

    // Q9: recall precision
    docs.push("recall precision tradeoff analysis".into());
    docs.push("recall precision evaluation framework".into());
    docs.push("accuracy exactness measurement suite".into());
    docs.push("fidelity correctness benchmark tool".into());

    // Q10: tree traversal
    docs.push("tree traversal depth first order".into());
    docs.push("tree traversal iterative implementation".into());
    docs.push("network expansion pathfinding method".into());
    docs.push("graph traversal node visit sequence".into());

    // Q11: build system
    docs.push("build system configuration management".into());
    docs.push("build system dependency resolution".into());
    docs.push("create construct assembly pipeline".into());
    docs.push("make develop compile toolchain".into());

    // Q12: update deploy
    docs.push("update deploy release automation".into());
    docs.push("update deploy pipeline orchestration".into());
    docs.push("modify change publish rollout".into());
    docs.push("refresh upgrade ship launch".into());

    // Extract vocabulary for embedding generator
    let mut vocab: HashSet<String> = HashSet::new();
    for doc in &docs {
        for word in doc.split_whitespace() {
            let normalized = word.to_ascii_lowercase();
            let cleaned: String = normalized.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            if !cleaned.is_empty() {
                vocab.insert(cleaned);
            }
        }
    }

    // Add some extra vocabulary words for better embedding coverage
    let extra = [
        "fast", "quick", "rapid", "speedy", "swift", "slow", "sluggish",
        "search", "lookup", "query", "find", "seek", "retrieve",
        "ranking", "scoring", "ordering", "sorting", "grading",
        "debugging", "diagnostics", "tracing", "testing",
        "backend", "server", "service", "api", "gateway",
        "engineer", "developer", "programmer", "architect", "designer",
        "graph", "network", "tree", "web", "structure",
        "data", "information", "records", "facts", "content",
        "integrity", "consistency", "validity", "correctness", "soundness",
        "vector", "embedding", "tensor", "array", "matrix",
        "latency", "delay", "lag", "overhead", "performance",
        "store", "shop", "market", "warehouse", "repository",
        "build", "create", "construct", "develop", "assemble",
        "update", "modify", "change", "revise", "refresh",
        "deploy", "release", "launch", "ship", "publish",
        "went", "go", "travel", "move", "walk",
        "recall", "precision", "accuracy", "fidelity", "exactness",
        "analysis", "study", "examination", "investigation", "review",
    ];
    for word in extra {
        vocab.insert(word.into());
    }

    // Distractor docs: 80 random docs unrelated to any query.
    let distractors = [
        "machine learning model training batch normalization",
        "kubernetes cluster autoscaling policy configuration",
        "react component state management hooks pattern",
        "database replication lag monitoring and alerting",
        "oauth2 token refresh flow implementation details",
        "ci cd pipeline artifact caching strategy",
        "distributed tracing sampling rate optimization",
        "redis cache eviction policy memory management",
        "grpc protobuf message versioning compatibility",
        "aws lambda cold start mitigation techniques",
        "terraform infrastructure as code module reuse",
        "prometheus metrics cardinality explosion prevention",
        "elastic search index shard allocation balancing",
        "webassembly module compilation performance tuning",
        "graphql federation schema stitching conflicts",
        "postgresql query planner statistics update frequency",
        "kafka consumer group partition rebalance handling",
        "typescript strict mode type inference limitations",
        "docker multi stage build image size reduction",
        "cdn edge caching cache invalidation strategies",
        "flutter widget tree rebuild optimization tips",
        "rust ownership borrowing lifetime compiler checks",
        "svelte compiler reactive statement dependency tracking",
        "mongodb aggregation pipeline memory limit tuning",
        "nginx reverse proxy load balancing algorithm selection",
        "rabbitmq quorum queue replication factor sizing",
        "spark dataframe partition skew join optimization",
        "swift concurrency actor isolation data races",
        "cassandra compaction strategy read write ratio",
        "ethereum smart contract gas optimization patterns",
        "hadoop mapreduce shuffle phase io tuning",
        "vault dynamic secrets database credential rotation",
        "istio service mesh traffic routing policy",
        "pandas dataframe vectorization apply performance",
        "opencv image preprocessing pipeline normalization",
        "ansible playbook idempotency handler notification",
        "consul service discovery health check intervals",
        "pytorch model quantization int8 accuracy tradeoff",
        "jenkins plugin dependency version conflict resolution",
        "hbase region splitting pre splitting strategy",
        "vue composition api ref reactive differences",
        "neo4j cypher query pattern matching optimization",
        "snowflake warehouse auto suspend credit management",
        "cloudflare workers edge computing request limits",
        "bigtable row key design hotspot avoidance",
        "dynamodb global table conflict resolution strategies",
        "fastapi dependency injection middleware stack ordering",
        "selenium webdriver headless mode screenshot reliability",
        "zookeeper leader election session timeout tuning",
        "kibana dashboard filter pinning drill down",
        "spark streaming checkpoint exactly once semantics",
        "git lfs large file storage bandwidth usage",
        "heroku dyno sleeping free tier behavior",
        "logstash grok pattern cpu overhead parsing",
        "airflow dag dependency sensor timeout configuration",
        "nomad job constraint affinity spread placement",
        "supabase realtime subscription postgres listen notify",
        "firebase cloud functions cold start latency",
        "stripe webhook event idempotency key replay",
        "twilio sms delivery status callback reliability",
        "sendgrid email template dynamic data substitution",
        "mixpanel event tracking batching network efficiency",
        "amplitude user session identification cross device",
        "datadog apm trace retention policy pricing",
        "new relic alert condition anomaly detection",
        "pagerduty incident escalation rotation schedule overlap",
        "slack bot interactive modal block kit validation",
        "discord bot gateway intent permission denied handling",
        "shopify api rate limit graphql cost calculation",
        "wordpress plugin hook priority execution order conflict",
        "drupal module dependency compatibility version matrix",
        "magento cache full page cache varnish configuration",
        "salesforce apex trigger bulkification governor limits",
        "hubspot workflow enrollment criteria property change",
        "marketo smart list segment logic nested filters",
        "zendesk ticket automation trigger condition regex",
        "jira workflow transition screen field validation",
        "confluence page tree hierarchy space permissions",
        "notion api database query filter pagination cursor",
        "asana project section task custom field sorting",
        "trello board list card attachment power up integration",
        "monday com item column value type casting",
        "basecamp message board thread notification digest frequency",
        "linear issue cycle time estimation confidence interval",
        "shortcut story epic iteration capacity planning",
    ];
    for d in distractors {
        docs.push(d.into());
        for word in d.split_whitespace() {
            let normalized = word.to_ascii_lowercase();
            let cleaned: String = normalized.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            if !cleaned.is_empty() {
                vocab.insert(cleaned);
            }
        }
    }

    // Append all thoughts.
    for (i, content) in docs.iter().enumerate() {
        let agent = if i % 3 == 0 { "backend-dev" } else { "frontend-dev" };
        chain
            .append_thought(agent, ThoughtInput::new(ThoughtType::Insight, content.clone()))
            .unwrap();
    }

    let queries = vec![
        EvalQuery { text: "fast retrieval",      relevant: HashSet::from([0, 1, 2, 3]) },
        EvalQuery { text: "search ranking",      relevant: HashSet::from([4, 5, 6, 7]) },
        EvalQuery { text: "went to store",       relevant: HashSet::from([8, 9, 10, 11]) },
        EvalQuery { text: "debugging latency",   relevant: HashSet::from([12, 13, 14, 15]) },
        EvalQuery { text: "backend engineer",    relevant: HashSet::from([16, 17, 18, 19]) },
        EvalQuery { text: "graph expansion",     relevant: HashSet::from([20, 21, 22, 23]) },
        EvalQuery { text: "data integrity",      relevant: HashSet::from([24, 25, 26, 27]) },
        EvalQuery { text: "vector search",       relevant: HashSet::from([28, 29, 30, 31]) },
        EvalQuery { text: "recall precision",    relevant: HashSet::from([32, 33, 34, 35]) },
        EvalQuery { text: "tree traversal",      relevant: HashSet::from([36, 37, 38, 39]) },
        EvalQuery { text: "build system",        relevant: HashSet::from([40, 41, 42, 43]) },
        EvalQuery { text: "update deploy",       relevant: HashSet::from([44, 45, 46, 47]) },
    ];

    let vocab_vec: Vec<String> = vocab.into_iter().collect();
    (chain, queries, vocab_vec, dir)
}

fn print_table(header: &str, rows: &[(String, Metrics)]) {
    println!("\n=== {} ===", header);
    println!(
        "{:<38} | {:>8} | {:>9} | {:>9} | {:>8} | {:>8}",
        "config", "R@5", "R@10", "P@5", "MRR", "NDCG@5"
    );
    println!("{}", "-".repeat(101));
    for (name, m) in rows {
        println!(
            "{:<38} | {:>8.4} | {:>9.4} | {:>9.4} | {:>8.4} | {:>8.4}",
            name, m.recall_at_5, m.recall_at_10, m.precision_at_5, m.mrr, m.ndcg_at_5
        );
    }
}

#[test]
fn search_quality_research_loop() {
    let (chain, queries, vocab, _dir) = build_corpus();

    // ------------------------------------------------------------------
    // BASELINE: no synonyms
    // ------------------------------------------------------------------
    let baseline = evaluate(&chain, &queries, &HashMap::new(), 0.0);

    // ------------------------------------------------------------------
    // MANUAL: hand-crafted synonym map (from previous experiments)
    // ------------------------------------------------------------------
    let mut manual = HashMap::new();
    manual.insert("fast".into(), vec!["quick".into(), "rapid".into(), "speedy".into(), "swift".into()]);
    manual.insert("search".into(), vec!["lookup".into(), "querying".into(), "find".into()]);
    manual.insert("ranking".into(), vec!["scoring".into(), "ordering".into(), "sorting".into()]);
    manual.insert("debugging".into(), vec!["diagnostics".into(), "tracing".into(), "logging".into(), "monitoring".into()]);
    manual.insert("backend".into(), vec!["server".into(), "service".into(), "api".into(), "gateway".into()]);
    manual.insert("engineer".into(), vec!["developer".into(), "programmer".into(), "architect".into(), "designer".into()]);
    manual.insert("graph".into(), vec!["network".into(), "tree".into()]);
    manual.insert("expansion".into(), vec!["growth".into(), "traversal".into()]);
    manual.insert("data".into(), vec!["information".into(), "records".into(), "state".into(), "content".into()]);
    manual.insert("integrity".into(), vec!["consistency".into(), "validity".into(), "correctness".into(), "soundness".into()]);
    manual.insert("vector".into(), vec!["embedding".into(), "tensor".into(), "array".into(), "matrix".into()]);
    manual.insert("recall".into(), vec!["precision".into(), "accuracy".into(), "fidelity".into(), "exactness".into()]);
    manual.insert("went".into(), vec!["go".into(), "travel".into(), "move".into(), "walk".into()]);
    manual.insert("store".into(), vec!["shop".into(), "market".into(), "warehouse".into(), "depot".into()]);
    manual.insert("latency".into(), vec!["delay".into(), "slowness".into(), "responsiveness".into(), "performance".into()]);
    manual.insert("tree".into(), vec!["graph".into(), "network".into()]);
    manual.insert("build".into(), vec!["create".into(), "construct".into(), "develop".into(), "assemble".into(), "make".into()]);
    manual.insert("update".into(), vec!["modify".into(), "change".into(), "revise".into(), "refresh".into(), "upgrade".into()]);
    manual.insert("deploy".into(), vec!["release".into(), "launch".into(), "ship".into(), "publish".into(), "rollout".into()]);
    let manual_07 = evaluate(&chain, &queries, &manual, 0.7);

    // ------------------------------------------------------------------
    // THESAURUS: built-in static thesaurus
    // ------------------------------------------------------------------
    let mut thesaurus_map = HashMap::new();
    for q in &queries {
        let expanded = thesaurus::expand_text(q.text);
        for (k, v) in expanded {
            thesaurus_map.entry(k).or_insert_with(Vec::new).extend(v);
        }
    }
    // Deduplicate
    for v in thesaurus_map.values_mut() {
        let seen: HashSet<_> = v.iter().cloned().collect();
        *v = seen.into_iter().collect();
    }
    let thesaurus_07 = evaluate(&chain, &queries, &thesaurus_map, 0.7);

    // ------------------------------------------------------------------
    // EMBEDDING: nearest-neighbor over corpus vocabulary
    // ------------------------------------------------------------------
    let embed_gen = EmbeddingSynonymGenerator::from_vocabulary(&vocab);
    let mut embed_map = HashMap::new();
    for q in &queries {
        let expanded = embed_gen.expand_text(q.text, 4);
        for (k, v) in expanded {
            embed_map.entry(k).or_insert_with(Vec::new).extend(v);
        }
    }
    // Deduplicate
    for v in embed_map.values_mut() {
        let seen: HashSet<_> = v.iter().cloned().collect();
        *v = seen.into_iter().collect();
    }
    let embed_07 = evaluate(&chain, &queries, &embed_map, 0.7);

    // ------------------------------------------------------------------
    // COMBINED: thesaurus + embedding
    // ------------------------------------------------------------------
    let mut combined = thesaurus_map.clone();
    for (k, v) in &embed_map {
        combined.entry(k.clone()).or_insert_with(Vec::new).extend(v.clone());
    }
    for v in combined.values_mut() {
        let seen: HashSet<_> = v.iter().cloned().collect();
        *v = seen.into_iter().collect();
    }
    let combined_07 = evaluate(&chain, &queries, &combined, 0.7);

    // ------------------------------------------------------------------
    // TRIPLE: manual + thesaurus + embedding
    // ------------------------------------------------------------------
    let mut triple = manual.clone();
    for (k, v) in &thesaurus_map {
        triple.entry(k.clone()).or_insert_with(Vec::new).extend(v.clone());
    }
    for (k, v) in &embed_map {
        triple.entry(k.clone()).or_insert_with(Vec::new).extend(v.clone());
    }
    for v in triple.values_mut() {
        let seen: HashSet<_> = v.iter().cloned().collect();
        *v = seen.into_iter().collect();
    }
    let triple_07 = evaluate(&chain, &queries, &triple, 0.7);

    // ------------------------------------------------------------------
    // Print results
    // ------------------------------------------------------------------
    let mut rows = vec![
        ("baseline (no synonyms)".to_string(), baseline),
        ("manual map w=0.7".to_string(), manual_07),
        ("thesaurus auto w=0.7".to_string(), thesaurus_07),
        ("embedding auto w=0.7".to_string(), embed_07),
        ("thesaurus + embedding w=0.7".to_string(), combined_07),
        ("manual + thesaurus + embedding w=0.7".to_string(), triple_07),
    ];

    rows.sort_by(|a, b| b.1.ndcg_at_5.total_cmp(&a.1.ndcg_at_5));
    print_table("Synonym Generator Comparison (weight=0.7)", &rows);

    // Best by NDCG
    let best = rows[0].1;
    let baseline_copy = baseline;
    assert!(
        best.recall_at_5 >= baseline_copy.recall_at_5 - 0.05,
        "best config should not severely degrade recall@5"
    );

    println!("\nBest config by NDCG@5: {}", rows[0].0);
    println!(
        "  recall@5={:.4} recall@10={:.4} precision@5={:.4} mrr={:.4} ndcg@5={:.4}",
        best.recall_at_5, best.recall_at_10, best.precision_at_5, best.mrr, best.ndcg_at_5
    );

    // Also print MRR ranking to see exact-match preservation
    let mut by_mrr = rows.clone();
    by_mrr.sort_by(|a, b| b.1.mrr.total_cmp(&a.1.mrr));
    println!("\nRanking by MRR (exact-match preservation):");
    for (name, m) in by_mrr.iter().take(5) {
        println!("  {:<40} MRR={:.4}", name, m.mrr);
    }

    // Print synonym map sizes for reference
    println!("\nSynonym map sizes:");
    println!("  manual:      {} terms", manual.len());
    println!("  thesaurus:   {} terms", thesaurus_map.len());
    println!("  embedding:   {} terms", embed_map.len());
    println!("  combined:    {} terms", combined.len());
    println!("  triple:      {} terms", triple.len());
}
