//! The lexical scan reuses per-thread scratch arrays between queries. These
//! tests pin that reuse as unobservable: no score, term or restriction state
//! may leak from one scan into the next.

use mentisdb::search::lexical::{LexicalCandidates, LexicalIndex, LexicalQuery};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};
use tempfile::tempdir;

fn index_of(contents: &[&str]) -> (LexicalIndex, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key(dir.path(), "scratch").unwrap();
    for content in contents {
        chain
            .append_thought("agent", ThoughtInput::new(ThoughtType::Idea, *content))
            .unwrap();
    }
    (LexicalIndex::build(chain.thoughts()), dir)
}

const DOCS: [&str; 5] = [
    "alpha rollback notes",
    "beta deployment notes",
    "gamma rollback deployment",
    "delta unrelated",
    "alpha beta gamma",
];

fn scores(index: &LexicalIndex, text: &str, candidates: LexicalCandidates<'_>) -> Vec<Option<f32>> {
    let scan = index.scan(&LexicalQuery::new(text), candidates);
    (0..DOCS.len())
        .map(|position| scan.score(position))
        .collect()
}

/// A scan after a different scan on the same thread sees none of the earlier
/// scan's scores, and repeating a query reproduces its result exactly.
#[test]
fn test_scan_results_are_independent_of_earlier_scans() {
    let (index, _dir) = index_of(&DOCS);
    let first = scores(&index, "rollback", LexicalCandidates::All);
    let other = scores(&index, "deployment", LexicalCandidates::All);
    assert_ne!(first, other);
    assert_eq!(first[1], None, "doc 1 does not mention rollback");
    assert_eq!(scores(&index, "rollback", LexicalCandidates::All), first);
    assert_eq!(scores(&index, "deployment", LexicalCandidates::All), other);
}

/// A restricted scan leaves no restriction behind: a following unrestricted
/// scan still sees every document.
#[test]
fn test_subset_restriction_does_not_leak_into_next_scan() {
    let (index, _dir) = index_of(&DOCS);
    let all = scores(&index, "rollback", LexicalCandidates::All);
    let subset = scores(&index, "rollback", LexicalCandidates::Subset(&[0]));
    assert_eq!(subset[0], all[0]);
    assert_eq!(subset[2], None, "restricted to position 0 only");
    assert_eq!(scores(&index, "rollback", LexicalCandidates::All), all);
}

/// Two scans alive at once do not share arrays, and a clone keeps its own.
#[test]
fn test_overlapping_and_cloned_scans_do_not_alias() {
    let (index, _dir) = index_of(&DOCS);
    let rollback = index.scan(&LexicalQuery::new("rollback"), LexicalCandidates::All);
    let deployment = index.scan(&LexicalQuery::new("deployment"), LexicalCandidates::All);
    let copy = rollback.clone();
    assert!(rollback.score(0).is_some() && rollback.score(1).is_none());
    assert!(deployment.score(1).is_some() && deployment.score(0).is_none());
    drop(rollback);
    assert!(copy.score(0).is_some(), "the clone survives its source");
    drop(deployment);
    drop(copy);
    let after = scores(&index, "rollback", LexicalCandidates::All);
    assert!(after[1].is_none() && after[3].is_none());
}

/// A growing index (more documents than the pooled arrays) and many term
/// words per document still produce correct scans after reuse.
#[test]
fn test_scratch_regrows_for_larger_index_and_wider_queries() {
    let (small, _d1) = index_of(&DOCS);
    let _ = small.scan(&LexicalQuery::new("alpha"), LexicalCandidates::All);
    let many: Vec<String> = (0..70).map(|i| format!("word{i}")).collect();
    let contents: Vec<String> = (0..20)
        .map(|i| format!("word{i} word{} common", 69 - i))
        .collect();
    let refs: Vec<&str> = contents.iter().map(String::as_str).collect();
    let (large, _d2) = index_of(&refs);
    let wide = LexicalQuery::new(many.join(" "));
    let first = large.scan(&wide, LexicalCandidates::All);
    let positions: Vec<usize> = first.positions().to_vec();
    assert_eq!(positions.len(), 20);
    drop(first);
    let again = large.scan(&wide, LexicalCandidates::All);
    assert_eq!(again.positions(), positions.as_slice());
}
