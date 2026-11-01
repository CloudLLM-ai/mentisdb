#![no_main]
//! Fuzzes the read-proof header: arbitrary bytes are a typed accept or reject
//! (never a panic), an accepted header re-renders byte for byte, and
//! verification of arbitrary headers against arbitrary requests never panics
//! and never accepts a header whose signature was not made for that request.

use libfuzzer_sys::fuzz_target;
use mentisdb::read_auth::{
    read_auth_payload, verify_request, NonceCache, ReadAuthHeader, MAX_READ_AUTH_HEADER_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(header) = ReadAuthHeader::parse(text) {
        // Canonical: the accepted text is exactly what we would have written.
        assert_eq!(header.to_header_value(), text);
        assert!(text.len() <= MAX_READ_AUTH_HEADER_LEN);
        assert!(!header.agent_id.is_empty() && !header.key_id.is_empty());
        // Verifying against an arbitrary request must not panic, and an
        // arbitrary header only verifies if its signature really covers it.
        let now = header.timestamp;
        if verify_request(text, "POST", "/v1/search", b"{}", now).is_ok() {
            use ed25519_dalek_probe::verifies;
            assert!(verifies(&header, "POST", "/v1/search", b"{}"));
        }
        let cache = NonceCache::new(4);
        let _ = cache.check_and_insert(&header.public_key, &header.nonce, now);
        let _ = read_auth_payload("GET", "/", b"", &header);
    }
    // Stale clocks and extreme timestamps never overflow.
    for now in [i64::MIN, -1, 0, i64::MAX] {
        let _ = verify_request(text, "GET", "/", b"", now);
    }
});

/// Re-derive "does this header's signature cover this request" independently of
/// `verify_request`, through the public payload builder and strict verification.
mod ed25519_dalek_probe {
    use mentisdb::read_auth::{read_auth_payload, ReadAuthHeader};
    use mentisdb::signing::verify_ed25519_signature;

    pub fn verifies(header: &ReadAuthHeader, method: &str, target: &str, body: &[u8]) -> bool {
        let payload = read_auth_payload(method, target, body, header);
        verify_ed25519_signature(&header.public_key, &payload, &header.signature).is_ok()
    }
}
