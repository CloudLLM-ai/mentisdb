//! Webhook SSRF guard: IPv6 transition and tunnel ranges.

use mentisdb::webhooks::{is_blocked_ip, validate_webhook_url};
use std::net::IpAddr;

fn blocked(s: &str) -> bool {
    is_blocked_ip(&s.parse::<IpAddr>().unwrap())
}

/// IPv4-translated ::ffff:0:a.b.c.d is judged by its embedded IPv4 address.
#[test]
fn ipv4_translated_prefix_unwraps_embedded_address() {
    assert!(blocked("::ffff:0:127.0.0.1"));
    assert!(blocked("::ffff:0:169.254.169.254"));
    assert!(blocked("::ffff:0:10.0.0.1"));
    assert!(!blocked("::ffff:0:8.8.8.8"));
    // The classic IPv4-mapped form keeps working.
    assert!(blocked("::ffff:127.0.0.1"));
    assert!(!blocked("::ffff:8.8.8.8"));
}

/// Local-use NAT64 and Teredo are blocked outright.
#[test]
fn local_nat64_and_teredo_are_blocked() {
    assert!(blocked("64:ff9b:1::1"));
    assert!(blocked("64:ff9b:1:ffff::8.8.8.8"));
    assert!(blocked("2001::1"));
    assert!(blocked("2001:0:4136:e378:8000:63bf:3fff:fdd2"));
    // Well-known NAT64 still defers to the embedded IPv4.
    assert!(blocked("64:ff9b::7f00:1"));
    assert!(!blocked("64:ff9b::808:808"));
    // Ordinary global IPv6 stays reachable.
    assert!(!blocked("2606:4700:4700::1111"));
    // 2001:4860::/32 (Google) is not Teredo.
    assert!(!blocked("2001:4860:4860::8888"));
}

/// URL validation rejects the new forms.
#[test]
fn urls_with_blocked_v6_literals_are_rejected() {
    for url in [
        "http://[::ffff:0:127.0.0.1]/hook",
        "http://[64:ff9b:1::1]/hook",
        "http://[2001::1]/hook",
    ] {
        assert!(validate_webhook_url(url).is_err(), "{url}");
    }
}
