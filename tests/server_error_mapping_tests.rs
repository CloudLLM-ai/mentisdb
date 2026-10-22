//! Service error to HTTP status mapping.
#![cfg(feature = "server")]

use axum::http::StatusCode;
use mentisdb::server::{classify_service_error, ClientInputError};
use std::io::{Error, ErrorKind};

fn classify(error: Error) -> (StatusCode, String) {
    classify_service_error(&error)
}

/// `InvalidInput` maps to 400 and keeps its message.
#[test]
fn test_invalid_input_maps_to_400() {
    let (status, message) = classify(Error::new(ErrorKind::InvalidInput, "bad value"));
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message, "bad value");
}

/// `InvalidData` signals corrupt internal state, so it is a generic 500.
#[test]
fn test_invalid_data_maps_to_generic_500() {
    let (status, message) = classify(Error::new(
        ErrorKind::InvalidData,
        "corrupt /Users/secret/chain.tcbin",
    ));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(message, "internal server error");
}

/// A raw OS NotFound (for example a missing sidecar or registry file) must be
/// a generic 500 and never leak the absolute path.
#[test]
fn test_internal_os_not_found_does_not_leak_path() {
    let io_error = std::fs::read("/nonexistent-mentisdb-dir/secret-registry.json").unwrap_err();
    assert_eq!(io_error.kind(), ErrorKind::NotFound);
    let (status, message) = classify(io_error);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!message.contains("nonexistent"));
}

/// NotFound, PermissionDenied, AlreadyExists keep their dedicated statuses.
#[test]
fn test_not_found_forbidden_conflict_statuses() {
    assert_eq!(
        classify(Error::new(ErrorKind::NotFound, "x")).0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        classify(Error::new(ErrorKind::PermissionDenied, "x")).0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        classify(Error::new(ErrorKind::AlreadyExists, "x")).0,
        StatusCode::CONFLICT
    );
}

/// Other failures become a generic 500 that does not leak the path in the detail.
#[test]
fn test_other_errors_map_to_generic_500_without_leaking_paths() {
    let (status, message) = classify(Error::other("failed to open /Users/secret/chain.tcbin"));
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!message.contains("/Users/secret"));
    assert_eq!(message, "internal server error");
}

/// Request-content errors (JSON, UUID, explicit client errors) are 400s.
#[test]
fn test_client_error_types_map_to_400() {
    let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    assert_eq!(
        classify_service_error(&json_error).0,
        StatusCode::BAD_REQUEST
    );
    let uuid_error = "nope".parse::<uuid::Uuid>().unwrap_err();
    assert_eq!(
        classify_service_error(&uuid_error).0,
        StatusCode::BAD_REQUEST
    );
    let client = ClientInputError("Unknown ThoughtRole 'x'".to_string());
    assert_eq!(classify_service_error(&client).0, StatusCode::BAD_REQUEST);
}

/// Unrecognized error types are server faults, not client errors.
#[test]
fn test_unknown_error_type_maps_to_500() {
    #[derive(Debug)]
    struct Weird;
    impl std::fmt::Display for Weird {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "/abs/path/leak")
        }
    }
    impl std::error::Error for Weird {}
    let (status, message) = classify_service_error(&Weird);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!message.contains("/abs/path"));
}
