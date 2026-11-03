#![cfg(feature = "server")]
//! TLS certificate validity: a generated certificate is dated relative to now
//! (not to a fixed calendar date), and at startup an expired MentisDB
//! certificate is regenerated while an expiring or foreign one is only warned
//! about.

use chrono::{DateTime, Duration, Utc};
use mentisdb::server::{
    ensure_tls_cert, tls_cert_expiry_warning, tls_cert_validity, TLS_CERT_WARN_DAYS,
};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_cert_expiry_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ymd(at: DateTime<Utc>) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(at.timestamp()).unwrap()
}

/// Write a self-signed certificate and key valid `[from, to]` with the given
/// Common Name and extra DNS names; return their paths.
fn write_cert(
    dir: &Path,
    common_name: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    dns_names: &[&str],
) -> (PathBuf, PathBuf) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    params.distinguished_name = dn;
    params.subject_alt_names = dns_names
        .iter()
        .map(|name| SanType::DnsName((*name).to_string().try_into().unwrap()))
        .collect();
    let (from, to) = (ymd(from), ymd(to));
    params.not_before = rcgen::date_time_ymd(from.year(), from.month().into(), from.day());
    params.not_after = rcgen::date_time_ymd(to.year(), to.month().into(), to.day());
    let cert = params.self_signed(&key).unwrap();
    let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

fn validity_of(path: &Path) -> mentisdb::server::TlsCertValidity {
    tls_cert_validity(&std::fs::read_to_string(path).unwrap()).expect("a certificate")
}

/// A freshly generated certificate is valid from yesterday for about two
/// years from now, so it cannot "expire for everyone" on a fixed date, and it
/// is recognised as MentisDB's own.
#[test]
fn a_generated_certificate_is_dated_relative_to_now() {
    let dir = dir("fresh");
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let before = Utc::now();
    ensure_tls_cert(&cert, &key).unwrap();
    let validity = validity_of(&cert);
    assert!(validity.generated_by_mentisdb);
    assert!(validity.not_before <= before, "{validity:?}");
    assert!(
        validity.not_before >= before - Duration::days(3),
        "{validity:?}"
    );
    let days = (validity.not_after - before).num_days();
    assert!(
        (727..=731).contains(&days),
        "valid for {days} days: {validity:?}"
    );
    assert!(tls_cert_expiry_warning(&cert).is_none());
}

/// Regression: nothing re-read an existing certificate, so one that had
/// expired was served forever. An expired MentisDB certificate is regenerated
/// with its custom names and a new key; the new one is valid.
#[test]
fn an_expired_mentisdb_certificate_is_regenerated_keeping_its_names() {
    let dir = dir("expired");
    let (cert, key) = write_cert(
        &dir,
        "MentisDB Local",
        Utc::now() - Duration::days(900),
        Utc::now() - Duration::days(170),
        &["localhost", "vps.example.test"],
    );
    let old_key = std::fs::read_to_string(&key).unwrap();
    assert!(tls_cert_expiry_warning(&cert)
        .unwrap()
        .contains("expired on"));

    ensure_tls_cert(&cert, &key).unwrap();

    let validity = validity_of(&cert);
    assert!(
        validity.not_after > Utc::now() + Duration::days(700),
        "{validity:?}"
    );
    assert!(validity.generated_by_mentisdb);
    assert_ne!(
        std::fs::read_to_string(&key).unwrap(),
        old_key,
        "a new key pair"
    );
    let artifacts =
        mentisdb::server::ensure_tls_cert_with_sans(&cert, &key, Vec::new(), false).unwrap();
    assert!(
        artifacts
            .sans
            .iter()
            .any(|san| san == "dns:vps.example.test"),
        "custom SAN kept: {:?}",
        artifacts.sans
    );
    assert!(tls_cert_expiry_warning(&cert).is_none());
}

/// An expiring (not yet expired) certificate is warned about and left alone,
/// so the operator decides when clients must re-trust.
#[test]
fn an_expiring_certificate_warns_and_is_not_replaced() {
    let dir = dir("expiring");
    let (cert, key) = write_cert(
        &dir,
        "MentisDB Local",
        Utc::now() - Duration::days(700),
        Utc::now() + Duration::days(10),
        &["localhost"],
    );
    let before = std::fs::read_to_string(&cert).unwrap();
    let warning = tls_cert_expiry_warning(&cert).expect("warns");
    assert!(warning.contains("expires on"), "{warning}");
    assert!(warning.contains("mentisdb cert --force"), "{warning}");
    ensure_tls_cert(&cert, &key).unwrap();
    assert_eq!(std::fs::read_to_string(&cert).unwrap(), before, "untouched");

    // Comfortably valid: no warning, no change.
    let dir = self::dir("fine");
    let (cert, _) = write_cert(
        &dir,
        "MentisDB Local",
        Utc::now() - Duration::days(1),
        Utc::now() + Duration::days(TLS_CERT_WARN_DAYS + 20),
        &["localhost"],
    );
    assert!(tls_cert_expiry_warning(&cert).is_none());
}

/// A certificate the operator supplied is never regenerated, expired or not:
/// it is warned about.
#[test]
fn an_expired_foreign_certificate_is_warned_about_but_never_replaced() {
    let dir = dir("foreign");
    let (cert, key) = write_cert(
        &dir,
        "my-corp-internal-ca-issued",
        Utc::now() - Duration::days(900),
        Utc::now() - Duration::days(170),
        &["mem.corp.test"],
    );
    let (before_cert, before_key) = (
        std::fs::read_to_string(&cert).unwrap(),
        std::fs::read_to_string(&key).unwrap(),
    );
    assert!(!validity_of(&cert).generated_by_mentisdb);
    assert!(tls_cert_expiry_warning(&cert)
        .unwrap()
        .contains("expired on"));
    ensure_tls_cert(&cert, &key).unwrap();
    assert_eq!(std::fs::read_to_string(&cert).unwrap(), before_cert);
    assert_eq!(std::fs::read_to_string(&key).unwrap(), before_key);
}

/// Missing, unreadable and non-certificate files are not a warning.
#[test]
fn missing_or_garbage_files_give_no_warning() {
    let dir = dir("garbage");
    assert!(tls_cert_expiry_warning(&dir.join("nope.pem")).is_none());
    let junk = dir.join("junk.pem");
    std::fs::write(&junk, "not a certificate").unwrap();
    assert!(tls_cert_expiry_warning(&junk).is_none());
    assert!(tls_cert_validity("").is_none());
}
