//! The CLI's daemon flush request must carry the configured bearer token.

use mentisdb::cli::bearer_authorization_header;

/// A configured token becomes a trimmed `Bearer` header; blank or missing
/// tokens send no header.
#[test]
fn test_bearer_authorization_header_builder() {
    assert_eq!(
        bearer_authorization_header(Some("  abc123 \n")).as_deref(),
        Some("Bearer abc123")
    );
    assert_eq!(bearer_authorization_header(Some("   ")), None);
    assert_eq!(bearer_authorization_header(None), None);
}
