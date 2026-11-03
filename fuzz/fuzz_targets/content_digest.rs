#![no_main]
//! Fuzzes the `X-Mentis-Content-Sha256` header: any text is a typed accept or
//! reject (never a panic); an accepted value is exactly the canonical rendering
//! of the 32 bytes it names, so there is one spelling per digest; and a proof
//! verifies from a declared digest only when the digest is the signed body's.

use libfuzzer_sys::fuzz_target;
use mentisdb::read_auth::{
    body_digest, content_digest_header_value, parse_content_digest, sign_request,
    verify_request_for_digest,
};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(digest) = parse_content_digest(text) {
        assert_eq!(content_digest_header_value(&digest), text);
        assert_eq!(text.len(), 64);
    }
    // Whatever the header says, a proof signed over `data` verifies from
    // `data`'s digest and from no other.
    let proof = sign_request(
        &[5; 32],
        "fuzz",
        "k",
        "POST",
        "/v1/import-markdown",
        data,
        1_767_225_600,
        "00112233445566778899aabbccddeeff",
    );
    let right = body_digest(data);
    assert!(
        verify_request_for_digest(&proof, "POST", "/v1/import-markdown", &right, 1_767_225_600)
            .is_ok()
    );
    let mut wrong = right;
    wrong[0] ^= 1;
    assert!(
        verify_request_for_digest(&proof, "POST", "/v1/import-markdown", &wrong, 1_767_225_600)
            .is_err()
    );
});
