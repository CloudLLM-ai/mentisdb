//! Measures how agents actually record corrections in a chain copy.
//!
//! For every `Correction`, `AssumptionInvalidated` and `Decision` thought it
//! reports which relation kinds and kind-less `refs` it carries and how many of
//! the referenced targets are still live (not invalidated), which is the exact
//! population that default retrieval can resurface as stale.
//!
//! ```text
//! cargo run --release --example correction_audit -- <copy-of-chain-dir> <chain-key>
//! ```
use mentisdb::{is_invalidating_relation_kind, MentisDb, ThoughtType};
use std::collections::BTreeMap;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let chain = MentisDb::open_with_key(&a[1], &a[2]).expect("open chain copy");
    let ts = chain.thoughts();
    let inv = chain.invalidated_thought_ids();
    println!(
        "chain {} thoughts={} invalidated={}",
        a[2],
        ts.len(),
        inv.len()
    );
    for kind in [
        ThoughtType::Correction,
        ThoughtType::AssumptionInvalidated,
        ThoughtType::Decision,
    ] {
        let sel: Vec<_> = ts.iter().filter(|t| t.thought_type == kind).collect();
        let mut with_inv = 0;
        let mut weak_only = 0;
        let mut refs_only = 0;
        let mut bare = 0;
        let mut live_targets_weak = 0;
        let mut live_targets_refs = 0;
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for t in &sel {
            let has_inv = t
                .relations
                .iter()
                .any(|r| is_invalidating_relation_kind(r.kind));
            for r in &t.relations {
                *kinds.entry(format!("{:?}", r.kind)).or_default() += 1;
            }
            let weak: Vec<_> = t
                .relations
                .iter()
                .filter(|r| !is_invalidating_relation_kind(r.kind))
                .collect();
            if has_inv {
                with_inv += 1;
            } else if !weak.is_empty() {
                weak_only += 1;
            } else if !t.refs.is_empty() {
                refs_only += 1;
            } else {
                bare += 1;
            }
            if !has_inv {
                live_targets_weak += weak.iter().filter(|r| !inv.contains(&r.target_id)).count();
                live_targets_refs += t
                    .refs
                    .iter()
                    .filter(|i| ts.get(**i as usize).is_some_and(|x| !inv.contains(&x.id)))
                    .count();
            }
        }
        println!("  {:?}: n={} typed_invalidating={} weak_relations_only={} refs_only={} bare={} live_weak_targets={} live_ref_targets={} kinds={:?}",
            kind, sel.len(), with_inv, weak_only, refs_only, bare, live_targets_weak, live_targets_refs, kinds);
    }
    if std::env::var("SHOW").is_ok() {
        for t in ts.iter().filter(|t| {
            matches!(
                t.thought_type,
                ThoughtType::Correction | ThoughtType::AssumptionInvalidated
            )
        }) {
            let c: String = t.content.chars().take(220).collect();
            println!(
                "#{} {:?} refs={:?} rel={:?}\n   {}",
                t.index,
                t.thought_type,
                t.refs,
                t.relations
                    .iter()
                    .map(|r| (
                        format!("{:?}", r.kind),
                        r.target_id.to_string()[..6].to_string()
                    ))
                    .collect::<Vec<_>>(),
                c.replace('\n', " ")
            );
            for i in &t.refs {
                if let Some(x) = ts.get(*i as usize) {
                    println!(
                        "   -> #{} {:?} {}",
                        x.index,
                        x.thought_type,
                        x.content
                            .chars()
                            .take(120)
                            .collect::<String>()
                            .replace('\n', " ")
                    );
                }
            }
        }
    }
}
