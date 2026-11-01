#![no_main]
//! Fuzzes the operator command bodies the dashboard route accepts and the CLI
//! produces: arbitrary JSON is a typed accept or reject, and key specs parse
//! only when they carry exactly `AGENT:KEY_ID:<64 hex>`.

use libfuzzer_sys::fuzz_target;
use mentisdb::operator::{KeySpec, OperatorOp};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(op) = serde_json::from_str::<OperatorOp>(text) {
        // Whatever was accepted serializes back to something that parses to the
        // same command.
        let again = serde_json::to_string(&op).expect("serializes");
        assert_eq!(serde_json::from_str::<OperatorOp>(&again).expect("round trip"), op);
    }
    if let Ok(spec) = text.parse::<KeySpec>() {
        assert_eq!(spec.public_key.len(), 64);
        assert!(!spec.agent_id.is_empty() && !spec.key_id.is_empty());
    }
});
