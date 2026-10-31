#![no_main]
//! Fuzzes the stored-signature envelope, public-key validation, hex decoding
//! and signature verification with hostile bytes.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mentisdb::signing::{
    from_hex, to_hex, validate_public_key, verify_ed25519_signature, SignatureVersion,
    StoredSignature,
};

#[derive(Arbitrary, Debug)]
struct Input {
    stored: Vec<u8>,
    public_key: Vec<u8>,
    message: Vec<u8>,
    hex: String,
}

fuzz_target!(|input: Input| {
    // Envelope: only 64 bytes (v1) or 0x02 + 64 bytes (v2) parse, and v2 round-trips.
    match StoredSignature::parse(&input.stored) {
        Ok(parsed) => match parsed.version {
            SignatureVersion::V1 => assert_eq!(input.stored.len(), 64),
            SignatureVersion::V2 => {
                assert_eq!(input.stored.len(), 65);
                assert_eq!(StoredSignature::encode_v2(&parsed.signature), input.stored);
            }
        },
        Err(_) => assert!(input.stored.len() != 64 && !(input.stored.len() == 65 && input.stored[0] == 2)),
    }
    // Keys: accepted keys are 32 bytes; verification with any bytes never panics
    // and never succeeds for a key that validation refused.
    let valid = validate_public_key(&input.public_key);
    if let Ok(key) = valid {
        assert_eq!(key.as_slice(), input.public_key.as_slice());
    }
    let signature = &input.stored[..input.stored.len().min(64)];
    if verify_ed25519_signature(&input.public_key, &input.message, signature).is_ok() {
        assert!(valid.is_ok(), "verified with a key that validation rejects");
    }
    // Hex: whatever decodes re-encodes to the lowercase canonical form.
    if let Ok(bytes) = from_hex(&input.hex) {
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
        assert_eq!(to_hex(&bytes), input.hex.trim().to_ascii_lowercase());
    }
});
