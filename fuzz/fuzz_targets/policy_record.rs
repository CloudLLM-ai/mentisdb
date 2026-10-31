#![no_main]
//! Fuzzes access-policy record parsing: whatever bytes arrive as the content of
//! an `AccessPolicy` thought, parsing is a typed accept or reject, never a panic,
//! and what is accepted is canonical and internally consistent.

use libfuzzer_sys::fuzz_target;
use mentisdb::access::{PolicyRecord, MAX_POLICY_KEYS};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(record) = PolicyRecord::parse(text) {
        // Accepted records are exactly the canonical encoding...
        assert_eq!(record.to_content(), text);
        // ...pass every history-independent rule...
        record.validate().expect("parse implies validate");
        assert!(record.keys.len() <= MAX_POLICY_KEYS);
        // ...and rebuilding them from their parts yields the same record.
        let rebuilt = PolicyRecord::new(record.seq, record.mode, record.keys.clone(), &record.prev_policy)
            .expect("an accepted record can be rebuilt");
        assert_eq!(rebuilt, record);
    }
});
