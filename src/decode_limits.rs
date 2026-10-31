//! Bounded bincode configurations for decoding untrusted bytes.
//!
//! bincode sizes the buffer for a `String` or `Vec<u8>` from the length prefix
//! it just read and allocates it *before* checking that the input is that
//! long, so a forged prefix in a chain file, a restored backup or a derived
//! file would abort the whole process with a failed multi-terabyte allocation.
//! A [`Limit`](bincode::config::Limit) makes the decoder reject any claim above
//! a budget with `DecodeError::LimitExceeded` first. The wire format is
//! unchanged; only decoding is bounded.

use bincode::config::{Configuration, Limit, LittleEndian, Varint};

/// Decode budget for one length-prefixed thought record.
///
/// Records are capped at 10 MiB on disk, and element claims (`len * size_of`)
/// can exceed the encoded size, so the budget leaves generous headroom while
/// staying far below any allocation that could fail.
pub(crate) const THOUGHT_RECORD_DECODE_LIMIT: usize = 64 * 1024 * 1024;

/// Decode budget for whole-file derived state and registries (HNSW graphs,
/// implicit-edge overlays, skill registries, sidecar WAL records).
pub(crate) const FILE_DECODE_LIMIT: usize = 1024 * 1024 * 1024;

/// Standard bincode configuration bounded to [`THOUGHT_RECORD_DECODE_LIMIT`].
pub(crate) fn thought_record_config(
) -> Configuration<LittleEndian, Varint, Limit<THOUGHT_RECORD_DECODE_LIMIT>> {
    bincode::config::standard().with_limit::<THOUGHT_RECORD_DECODE_LIMIT>()
}

/// Standard bincode configuration bounded to [`FILE_DECODE_LIMIT`].
pub(crate) fn file_config() -> Configuration<LittleEndian, Varint, Limit<FILE_DECODE_LIMIT>> {
    bincode::config::standard().with_limit::<FILE_DECODE_LIMIT>()
}
