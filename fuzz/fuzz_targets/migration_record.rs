#![no_main]
//! Fuzzes the migration record decoders with arbitrary bytes.
//!
//! The binary and JSONL decoders read files that may come from any release,
//! a restored backup or a hostile peer. Invariants: never panic, never
//! allocate on a forged length (bounded by the decode limits), and every
//! record that decodes is upgraded to the current layout, passes through the
//! hash rebuilder without panicking, and survives a re-encode round trip.

use libfuzzer_sys::fuzz_target;
use mentisdb::migration::{
    decode_and_upgrade, decode_json_and_upgrade, peek_schema_version, HashRebuilder, RecordLayout,
};
use mentisdb::{Thought, MENTISDB_CURRENT_VERSION};

fuzz_target!(|data: &[u8]| {
    let _ = peek_schema_version(data);
    if let Ok(record) = decode_and_upgrade(data) {
        check(record.thought, record.layout);
    }
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(record) = decode_json_and_upgrade(text) {
            check(record.thought, record.layout);
        }
    }
});

fn check(thought: Thought, layout: RecordLayout) {
    // Round trip through the current encoding.
    let bytes = bincode::serde::encode_to_vec(&thought, bincode::config::standard())
        .expect("a decoded thought re-encodes");
    let again = decode_and_upgrade(&bytes).expect("a re-encoded thought decodes");
    assert_eq!(again.layout, RecordLayout::Current);
    assert_eq!(again.thought.id, thought.id);
    assert_eq!(again.thought.content, thought.content);
    // The hash rebuilder either accepts the record or returns a typed error.
    let mut rebuilder = HashRebuilder::default();
    if let Ok(rebuilt) = rebuilder.apply(thought, layout, mentisdb::MENTISDB_CURRENT_VERSION) {
        assert_eq!(rebuilt.schema_version, MENTISDB_CURRENT_VERSION);
    }
}
