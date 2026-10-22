//! Verifies bounded-chunk embedding without needing the ONNX model.

#![cfg(feature = "local-embeddings")]

use mentisdb::search::fastembed_provider::embed_in_chunks;
use mentisdb::search::EmbeddingInput;

fn inputs(n: usize) -> Vec<EmbeddingInput> {
    (0..n)
        .map(|i| EmbeddingInput::new(format!("id-{i}"), format!("{i}")))
        .collect()
}

/// 70 inputs with chunk size 32 produce calls of 32, 32 and 6 texts, and the
/// output preserves input order.
#[test]
fn test_embed_in_chunks_bounds_each_call_and_preserves_order() {
    let mut sizes = Vec::new();
    let out = embed_in_chunks::<()>(&inputs(70), 32, |texts| {
        sizes.push(texts.len());
        Ok(texts
            .iter()
            .map(|t| vec![t.parse::<f32>().unwrap()])
            .collect())
    })
    .unwrap();
    assert_eq!(sizes, vec![32, 32, 6]);
    let flat: Vec<f32> = out.iter().map(|v| v.values[0]).collect();
    assert_eq!(flat, (0..70).map(|i| i as f32).collect::<Vec<_>>());
}

/// Empty input makes no calls; the first chunk error aborts later chunks.
#[test]
fn test_embed_in_chunks_empty_and_error_paths() {
    let mut calls = 0;
    let out = embed_in_chunks::<()>(&[], 32, |_| {
        calls += 1;
        Ok(vec![])
    })
    .unwrap();
    assert!(out.is_empty() && calls == 0);
    let err = embed_in_chunks(&inputs(10), 4, |_| {
        calls += 1;
        Err("boom")
    });
    assert_eq!(err.unwrap_err(), "boom");
    assert_eq!(calls, 1);
}
