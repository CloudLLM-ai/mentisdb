//! Ranking knobs (`df_scope`, `df_cutoffs`, `vector_fusion`, `resolve_successors`,
//! `collapse_duplicates`, `synonym_weight`) are reachable from the library,
//! REST, MCP and the CLI, and the library defaults match the server's.

use mentisdb::{MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtType};
use tempfile::tempdir;

/// Library and server share one default: built-in thesaurus expansion at
/// weight 0.3, so "fast" finds "quick" without any setup.
#[test]
fn library_default_expands_with_the_thesaurus_like_the_server() {
    let dir = tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "syn", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, "Quick retrieval using BM25 scores."),
        )
        .unwrap();
    let on = chain.query_ranked(&RankedSearchQuery::new().with_text("fast"));
    assert_eq!(on.hits.len(), 1);
    let off = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("fast")
            .with_thesaurus(false),
    );
    assert!(off.hits.is_empty());
    let zero = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("fast")
            .with_synonym_weight(0.0),
    );
    assert!(zero.hits.is_empty(), "weight 0 disables expansion too");
    assert!((RankedSearchQuery::new().synonym_weight - 0.3).abs() < 1e-6);
}

mod cli {
    use mentisdb::cli::{parse_args, CliCommand};
    use serde_json::json;

    fn body(args: &[&str]) -> serde_json::Value {
        let argv = std::iter::once("mentisdb").chain(args.iter().copied());
        match parse_args(argv) {
            Ok(CliCommand::Remote(inv)) => inv.body.expect("body"),
            other => panic!("expected remote command, got {other:?}"),
        }
    }

    /// Every knob has a flag on `search`, `search federated` and `search bundles`
    /// that lands on the REST field of the same name.
    #[test]
    fn flags_map_onto_rest_fields() {
        let knobs = [
            "--resolve-successors",
            "false",
            "--collapse-duplicates",
            "0.8",
            "--vector-fusion",
            "absolute",
            "--synonym-weight",
            "0",
            "--df-scope",
            "field",
            "--df-cutoff-content",
            "0.5",
            "--df-cutoff-agent-registry",
            "0.9",
        ];
        for prefix in [
            vec!["search", "hello"],
            vec!["search", "federated", "hello", "-c", "a"],
            vec!["search", "bundles", "hello"],
        ] {
            let args: Vec<&str> = prefix
                .iter()
                .copied()
                .chain(knobs.iter().copied())
                .collect();
            let b = body(&args);
            assert_eq!(b["resolve_successors"], json!(false), "{prefix:?}");
            assert_eq!(b["collapse_duplicates"], json!(0.8));
            assert_eq!(b["vector_fusion"], "absolute");
            assert_eq!(b["synonym_weight"], json!(0.0));
            assert_eq!(b["df_scope"], "field");
            assert_eq!(b["df_cutoffs"]["content"], json!(0.5));
            assert_eq!(b["df_cutoffs"]["agent_registry"], json!(0.9));
        }
    }

    /// Bad values are rejected at parse time, not silently defaulted.
    #[test]
    fn bad_values_are_rejected() {
        for bad in [
            ["--vector-fusion", "weird"],
            ["--df-scope", "everywhere"],
            ["--collapse-duplicates", "2"],
        ] {
            let argv = ["mentisdb", "search", "x", bad[0], bad[1]];
            assert!(parse_args(argv.iter().copied()).is_err(), "{bad:?}");
        }
    }
}

#[cfg(feature = "server")]
mod transport {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
    use mentisdb::StorageAdapterKind;
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    async fn call(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn cfg(tag: &str) -> (MentisDbServiceConfig, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mentisdb_knobs_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (
            MentisDbServiceConfig::new(dir.clone(), "knobs", StorageAdapterKind::Binary),
            dir,
        )
    }

    #[tokio::test]
    async fn rest_accepts_every_knob_and_rejects_typos() {
        let (config, dir) = cfg("rest");
        let rest = rest_router(config);
        let (s, _) = call(
            rest.clone(),
            "/v1/thoughts",
            json!({"chain_key":"knobs","agent_id":"a","thought_type":"Finding","content":"Quick retrieval using BM25 scores."}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let search = |extra: Value| {
            let mut body = json!({"chain_key":"knobs","text":"fast","limit":5});
            for (k, v) in extra.as_object().unwrap() {
                body[k] = v.clone();
            }
            call(rest.clone(), "/v1/ranked-search", body)
        };
        let (s, ok) = search(json!({})).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            ok["results"].as_array().unwrap().len(),
            1,
            "thesaurus on by default"
        );
        let (_, off) = search(json!({"synonym_weight": 0})).await;
        // The vector channel may still surface the thought; the lexical score is
        // what the thesaurus changes.
        let lexical = |v: &Value| v["results"][0]["score"]["lexical"].as_f64().unwrap_or(0.0);
        assert!(lexical(&ok) > 0.0 && lexical(&off) == 0.0, "{ok} {off}");
        for good in [
            json!({"df_scope":"field"}),
            json!({"vector_fusion":"absolute"}),
            json!({"resolve_successors":false}),
            json!({"df_cutoffs":{"content":0.5,"agent_registry":0.9}}),
            json!({"collapse_duplicates":0.5}),
        ] {
            assert_eq!(search(good.clone()).await.0, StatusCode::OK, "{good}");
        }
        for bad in [
            json!({"df_scope":"nope"}),
            json!({"vector_fusion":"nope"}),
            json!({"df_cutoffs":{"content":2.0}}),
        ] {
            assert_ne!(search(bad.clone()).await.0, StatusCode::OK, "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The MCP tool schemas advertise every knob on all three ranked tools.
    #[tokio::test]
    async fn mcp_tools_list_the_knobs() {
        let (config, dir) = cfg("mcp");
        let mcp = mcp_router(config);
        let response = mcp
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tools/list")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let json: Value = serde_json::from_str(&text).unwrap();
        let tools = json["tools"]
            .as_array()
            .or(json["result"]["tools"].as_array())
            .expect(&text);
        for tool in [
            "mentisdb_ranked_search",
            "mentisdb_federated_search",
            "mentisdb_context_bundles",
        ] {
            let t = tools
                .iter()
                .find(|t| t["name"] == tool)
                .unwrap_or_else(|| panic!("{tool}"));
            let params = t["parameters"].to_string() + &t["inputSchema"].to_string();
            for knob in [
                "df_scope",
                "df_cutoffs",
                "vector_fusion",
                "resolve_successors",
                "collapse_duplicates",
                "synonym_weight",
            ] {
                assert!(params.contains(knob), "{tool} lacks {knob}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
