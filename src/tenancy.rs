//! Multi-tenancy primitives for hosted `mentisdbd` deployments.
//!
//! This module provides the foundational types used when
//! `MENTISDB_MULTITENANT=true` is set on the daemon. All types are inert in
//! single-tenant (default) mode — they are only exercised by the auth and
//! chain-scoping layers introduced in later phases.
//!
//! # Design overview
//!
//! * [`TenantId`] is the primary isolation boundary. Every chain, API key, and
//!   registry entry is owned by exactly one tenant.
//! * [`ApiKeyId`] identifies a single issued API key within a tenant.
//! * [`ApiKeyRecord`] stores the key's SHA-256 hash (never the plaintext),
//!   lifecycle metadata (created, last-used, expiry, revocation), and an
//!   optional human-readable label.
//! * [`TenantRecord`] holds tenant-level metadata: display name, active plan,
//!   and the set of issued API key IDs.
//! * [`TenantPlan`] defines the per-plan limit on simultaneously active keys.
//! * [`TenantRegistry`] is the on-disk store for all tenant and key records.
//!   It carries a SHA-256 integrity checksum and a monotone version counter so
//!   any tampering or partial write is detected at load time.
//!
//! # Persistence layout
//!
//! The registry lives at `<MENTISDB_DIR>/tenants/tenant-registry.json`.
//! The directory is created automatically by [`TenantRegistry::save`] if it
//! does not yet exist.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ─── filename constants ───────────────────────────────────────────────────────

/// Sub-directory within `MENTISDB_DIR` that holds tenant registry files.
pub const TENANTS_DIRNAME: &str = "tenants";

/// On-disk filename for the tenant registry.
pub const TENANT_REGISTRY_FILENAME: &str = "tenant-registry.json";

// ─── TenantId ─────────────────────────────────────────────────────────────────

/// Opaque, stable identifier for a single tenant.
///
/// `TenantId` is a UUID v4 value wrapped in a newtype so that it cannot be
/// accidentally confused with other UUID fields (e.g. [`ApiKeyId`]) at compile
/// time.
///
/// # Serialisation
///
/// Serialises as the UUID hyphenated lowercase string, e.g.
/// `"550e8400-e29b-41d4-a716-446655440000"`.
///
/// # Examples
///
/// ```
/// use mentisdb::tenancy::TenantId;
///
/// let id = TenantId::new();
/// let s = serde_json::to_string(&id).unwrap();
/// let round_tripped: TenantId = serde_json::from_str(&s).unwrap();
/// assert_eq!(id, round_tripped);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TenantId(Uuid);

impl TenantId {
    /// Generate a new random `TenantId`.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID as a `TenantId`.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for TenantId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ─── ApiKeyId ─────────────────────────────────────────────────────────────────

/// Opaque, stable identifier for a single issued API key.
///
/// Like [`TenantId`], this is a UUID v4 newtype that prevents accidental
/// cross-field substitution at the type level.
///
/// # Serialisation
///
/// Serialises as the UUID hyphenated lowercase string.
///
/// # Examples
///
/// ```
/// use mentisdb::tenancy::ApiKeyId;
///
/// let id = ApiKeyId::new();
/// let s = id.to_string();
/// assert_eq!(s.len(), 36); // UUID string length
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ApiKeyId(Uuid);

impl ApiKeyId {
    /// Generate a new random `ApiKeyId`.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID as an `ApiKeyId`.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Return the inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ApiKeyId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ApiKeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ─── TenantPlan ───────────────────────────────────────────────────────────────

/// Hosting plan tier that determines per-tenant API key limits and future
/// feature gates.
///
/// # Key limits
///
/// | Plan | Max simultaneous active keys |
/// |------|------------------------------|
/// | [`Free`](TenantPlan::Free) | 1 |
/// | [`Professional`](TenantPlan::Professional) | 5 |
/// | [`Business`](TenantPlan::Business) | 20 |
/// | [`Enterprise`](TenantPlan::Enterprise) | unlimited (`u32::MAX`) |
/// | [`Custom(n)`](TenantPlan::Custom) | `n` (0 = unlimited) |
///
/// # Examples
///
/// ```
/// use mentisdb::tenancy::TenantPlan;
///
/// assert_eq!(TenantPlan::Free.max_active_keys(), 1);
/// assert_eq!(TenantPlan::Professional.max_active_keys(), 5);
/// assert_eq!(TenantPlan::Enterprise.max_active_keys(), u32::MAX);
/// assert_eq!(TenantPlan::Custom(0).max_active_keys(), u32::MAX);
/// assert_eq!(TenantPlan::Custom(10).max_active_keys(), 10);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantPlan {
    /// Free / self-hosted tier. One active API key at a time.
    Free,
    /// Hosted professional tier. Up to five active API keys.
    Professional,
    /// Hosted business tier. Up to twenty active API keys.
    Business,
    /// Hosted enterprise tier. Unlimited active API keys.
    Enterprise,
    /// Custom plan with an explicit key-count limit.
    /// A value of `0` means unlimited.
    Custom(u32),
}

impl TenantPlan {
    /// Return the maximum number of simultaneously active API keys for this plan.
    ///
    /// Returns [`u32::MAX`] for unlimited plans.
    pub fn max_active_keys(&self) -> u32 {
        match self {
            TenantPlan::Free => 1,
            TenantPlan::Professional => 5,
            TenantPlan::Business => 20,
            TenantPlan::Enterprise => u32::MAX,
            TenantPlan::Custom(0) => u32::MAX,
            TenantPlan::Custom(n) => *n,
        }
    }
}

impl Default for TenantPlan {
    fn default() -> Self {
        TenantPlan::Free
    }
}

// ─── ApiKeyRecord ─────────────────────────────────────────────────────────────

/// Persisted metadata for one issued API key.
///
/// The plaintext key is **never** stored. Only the SHA-256 hex digest of the
/// raw key bytes is persisted so that a leaked registry file cannot be used to
/// authenticate directly.
///
/// The raw key string is returned exactly once — at creation time — and must be
/// communicated to the client immediately, as it cannot be recovered later.
///
/// # Fields
///
/// | Field | Purpose |
/// |-------|---------|
/// | [`id`](Self::id) | Stable, opaque key identifier. |
/// | [`key_hash`](Self::key_hash) | Lowercase hex SHA-256 of the raw key bytes. |
/// | [`label`](Self::label) | Optional human-readable description (e.g. `"CI bot"`). |
/// | [`created_at`](Self::created_at) | UTC timestamp of key issuance. |
/// | [`last_used_at`](Self::last_used_at) | UTC timestamp of the most recent successful auth. |
/// | [`expires_at`](Self::expires_at) | Optional UTC expiry. `None` means the key never expires. |
/// | [`revoked_at`](Self::revoked_at) | Set when the key is explicitly revoked. |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Stable identifier for this key record.
    pub id: ApiKeyId,
    /// Lowercase hex SHA-256 digest of the raw key bytes.
    ///
    /// Computed as `hex(sha256(raw_key_bytes))`. Never store the plaintext key.
    pub key_hash: String,
    /// Optional human-readable label (e.g. `"CI pipeline"` or `"laptop"`).
    ///
    /// `None` when no label was supplied at creation time.
    pub label: Option<String>,
    /// UTC timestamp when this key was created.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp of the most recent successful authentication using this key.
    ///
    /// `None` when the key has never been used.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Optional UTC timestamp after which this key should be considered expired.
    ///
    /// `None` means the key does not expire (subject to revocation).
    pub expires_at: Option<DateTime<Utc>>,
    /// UTC timestamp when this key was explicitly revoked.
    ///
    /// `None` means the key has not been revoked. Once set, authentication
    /// with this key must be rejected immediately regardless of expiry.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyRecord {
    /// Return `true` when the key is currently valid for authentication.
    ///
    /// A key is valid iff:
    /// - it has **not** been revoked ([`revoked_at`](Self::revoked_at) is `None`), **and**
    /// - it has **not** expired ([`expires_at`](Self::expires_at) is either `None`
    ///   or in the future relative to `now`).
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::Utc;
    /// use mentisdb::tenancy::{ApiKeyId, ApiKeyRecord};
    ///
    /// let key = ApiKeyRecord {
    ///     id: ApiKeyId::new(),
    ///     key_hash: "abc123".to_string(),
    ///     label: None,
    ///     created_at: Utc::now(),
    ///     last_used_at: None,
    ///     expires_at: None,
    ///     revoked_at: None,
    /// };
    /// assert!(key.is_active(Utc::now()));
    /// ```
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        if let Some(exp) = self.expires_at {
            if now >= exp {
                return false;
            }
        }
        true
    }

    /// Compute the SHA-256 hex digest of the given raw key bytes.
    ///
    /// Use this to derive the value that should be stored in [`key_hash`](Self::key_hash).
    ///
    /// # Examples
    ///
    /// ```
    /// use mentisdb::tenancy::ApiKeyRecord;
    ///
    /// let hash = ApiKeyRecord::hash_key(b"super-secret-key");
    /// assert_eq!(hash.len(), 64); // 32 bytes × 2 hex chars
    /// ```
    pub fn hash_key(raw_key: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(raw_key);
        format!("{:x}", hasher.finalize())
    }
}

// ─── TenantRecord ─────────────────────────────────────────────────────────────

/// All metadata and API key records associated with a single tenant.
///
/// `TenantRecord` is the per-tenant container stored inside [`TenantRegistry`].
/// It holds the tenant's display name, plan tier, administrative state, and the
/// ordered map of issued [`ApiKeyRecord`]s keyed by their [`ApiKeyId`].
///
/// # Examples
///
/// ```
/// use mentisdb::tenancy::{TenantId, TenantRecord, TenantPlan};
///
/// let record = TenantRecord::new(TenantId::new(), "Alice's org", TenantPlan::Professional);
/// assert!(!record.disabled);
/// assert_eq!(record.plan, TenantPlan::Professional);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantRecord {
    /// Stable identifier for this tenant.
    pub id: TenantId,
    /// Human-readable display name (e.g. `"Alice's org"`).
    pub display_name: String,
    /// Hosting plan that determines feature gates and key limits.
    pub plan: TenantPlan,
    /// UTC timestamp when this tenant was created.
    pub created_at: DateTime<Utc>,
    /// When `true`, the tenant is administratively disabled and all requests
    /// from their keys must be rejected.
    pub disabled: bool,
    /// All API keys ever issued to this tenant, keyed by [`ApiKeyId`].
    ///
    /// Revoked and expired keys remain in the map so that the audit trail is
    /// preserved. Use [`active_key_count`](Self::active_key_count) to count
    /// only currently valid keys.
    pub keys: BTreeMap<ApiKeyId, ApiKeyRecord>,
}

impl TenantRecord {
    /// Create a new tenant record with no issued keys.
    pub fn new(id: TenantId, display_name: impl Into<String>, plan: TenantPlan) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            plan,
            created_at: Utc::now(),
            disabled: false,
            keys: BTreeMap::new(),
        }
    }

    /// Return the number of currently active (non-revoked, non-expired) keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::Utc;
    /// use mentisdb::tenancy::{ApiKeyId, ApiKeyRecord, TenantId, TenantRecord, TenantPlan};
    ///
    /// let mut record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Free);
    /// assert_eq!(record.active_key_count(Utc::now()), 0);
    /// ```
    pub fn active_key_count(&self, now: DateTime<Utc>) -> u32 {
        self.keys
            .values()
            .filter(|k| k.is_active(now))
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Return `true` when a new API key can be issued under the current plan.
    ///
    /// # Examples
    ///
    /// ```
    /// use chrono::Utc;
    /// use mentisdb::tenancy::{TenantId, TenantRecord, TenantPlan};
    ///
    /// let record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Free);
    /// assert!(record.can_issue_key(Utc::now()));
    /// ```
    pub fn can_issue_key(&self, now: DateTime<Utc>) -> bool {
        self.active_key_count(now) < self.plan.max_active_keys()
    }
}

// ─── TenantRegistry ───────────────────────────────────────────────────────────

/// On-disk registry of all tenants and their API keys.
///
/// The registry is serialised as pretty-printed JSON at
/// `<MENTISDB_DIR>/tenants/tenant-registry.json`. Every [`save`](Self::save)
/// call recomputes the [`checksum`](Self::checksum) field over the serialised
/// tenant data so that any tampering or truncation is detected at
/// [`load`](Self::load) time.
///
/// ## Integrity model
///
/// Before the registry is written, the `checksum` field is cleared and the
/// tenants map is serialised to a canonical JSON string. A SHA-256 digest of
/// that string is then stored as the `checksum` hex value alongside the data.
///
/// On load, the same procedure is repeated: the stored checksum is extracted,
/// the field is blanked in memory, the data is re-serialised, and the fresh
/// digest is compared against the stored one. A mismatch returns
/// [`io::ErrorKind::InvalidData`].
///
/// ## Registry format
///
/// ```json
/// {
///   "version": 1,
///   "tenants": { … },
///   "checksum": "abcdef0123…"
/// }
/// ```
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::PathBuf;
/// use mentisdb::tenancy::{TenantId, TenantRecord, TenantPlan, TenantRegistry};
///
/// let mut registry = TenantRegistry::default();
/// let tenant = TenantRecord::new(TenantId::new(), "alice", TenantPlan::Professional);
/// registry.tenants.insert(tenant.id, tenant);
///
/// let dir = PathBuf::from("/tmp/mentisdb/tenants");
/// registry.save(&dir).unwrap();
///
/// let loaded = TenantRegistry::load(&dir).unwrap();
/// assert_eq!(loaded.tenants.len(), 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRegistry {
    /// Registry format version. Currently `1`.
    pub version: u32,
    /// All known tenants keyed by [`TenantId`].
    pub tenants: BTreeMap<TenantId, TenantRecord>,
    /// SHA-256 hex checksum of the serialised `tenants` field.
    ///
    /// This field is recomputed on every [`save`](Self::save) and verified on
    /// every [`load`](Self::load). Treat it as opaque; do not edit manually.
    pub checksum: String,
}

impl Default for TenantRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            tenants: BTreeMap::new(),
            checksum: String::new(),
        }
    }
}

impl TenantRegistry {
    /// Serialise the `tenants` map to a canonical JSON string and return its
    /// SHA-256 hex digest.
    ///
    /// This is the same digest stored in [`checksum`](Self::checksum) and
    /// re-derived on load.
    fn compute_checksum(tenants: &BTreeMap<TenantId, TenantRecord>) -> String {
        let canonical =
            serde_json::to_string(tenants).expect("TenantRegistry checksum serialisation failed");
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Persist the registry to `<dir>/tenant-registry.json`.
    ///
    /// The directory is created if it does not yet exist. The [`checksum`](Self::checksum)
    /// field is recomputed before writing so the file is always self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the directory cannot be created or the file
    /// cannot be written.
    pub fn save(&mut self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        self.checksum = Self::compute_checksum(&self.tenants);
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(dir.join(TENANT_REGISTRY_FILENAME), json.as_bytes())
    }

    /// Load the registry from `<dir>/tenant-registry.json`.
    ///
    /// Returns a default (empty) registry when the file does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] with kind [`InvalidData`](io::ErrorKind::InvalidData)
    /// when the JSON is malformed or the checksum does not match the stored
    /// tenant data.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let path = dir.join(TENANT_REGISTRY_FILENAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(&path)?;
        let registry: Self = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let expected = Self::compute_checksum(&registry.tenants);
        if registry.checksum != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "tenant registry checksum mismatch: stored={}, computed={}",
                    registry.checksum, expected
                ),
            ));
        }
        Ok(registry)
    }

    /// Return the filesystem path used by [`save`](Self::save) and
    /// [`load`](Self::load) given a tenant directory root.
    ///
    /// This is a convenience method for callers that need the path without
    /// performing any I/O.
    pub fn registry_path(dir: &Path) -> PathBuf {
        dir.join(TENANT_REGISTRY_FILENAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // ── TenantId ─────────────────────────────────────────────────────────────

    #[test]
    fn tenant_id_round_trips_through_json() {
        let id = TenantId::new();
        let json = serde_json::to_string(&id).unwrap();
        let decoded: TenantId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, decoded);
    }

    #[test]
    fn tenant_id_display_matches_inner_uuid() {
        let uuid = Uuid::new_v4();
        let id = TenantId::from_uuid(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }

    // ── ApiKeyId ─────────────────────────────────────────────────────────────

    #[test]
    fn api_key_id_round_trips_through_json() {
        let id = ApiKeyId::new();
        let json = serde_json::to_string(&id).unwrap();
        let decoded: ApiKeyId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, decoded);
    }

    // ── TenantPlan ───────────────────────────────────────────────────────────

    #[test]
    fn tenant_plan_max_active_keys() {
        assert_eq!(TenantPlan::Free.max_active_keys(), 1);
        assert_eq!(TenantPlan::Professional.max_active_keys(), 5);
        assert_eq!(TenantPlan::Business.max_active_keys(), 20);
        assert_eq!(TenantPlan::Enterprise.max_active_keys(), u32::MAX);
        assert_eq!(TenantPlan::Custom(0).max_active_keys(), u32::MAX);
        assert_eq!(TenantPlan::Custom(7).max_active_keys(), 7);
    }

    #[test]
    fn tenant_plan_serialises_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&TenantPlan::Professional).unwrap(),
            r#""professional""#
        );
        assert_eq!(
            serde_json::to_string(&TenantPlan::Enterprise).unwrap(),
            r#""enterprise""#
        );
    }

    // ── ApiKeyRecord ─────────────────────────────────────────────────────────

    fn make_active_key() -> ApiKeyRecord {
        ApiKeyRecord {
            id: ApiKeyId::new(),
            key_hash: ApiKeyRecord::hash_key(b"test-key"),
            label: Some("test".to_string()),
            created_at: Utc::now(),
            last_used_at: None,
            expires_at: None,
            revoked_at: None,
        }
    }

    #[test]
    fn api_key_record_is_active_when_no_revocation_or_expiry() {
        let key = make_active_key();
        assert!(key.is_active(Utc::now()));
    }

    #[test]
    fn api_key_record_is_inactive_after_revocation() {
        let mut key = make_active_key();
        key.revoked_at = Some(Utc::now());
        assert!(!key.is_active(Utc::now()));
    }

    #[test]
    fn api_key_record_is_inactive_after_expiry() {
        let mut key = make_active_key();
        key.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert!(!key.is_active(Utc::now()));
    }

    #[test]
    fn api_key_record_is_active_before_expiry() {
        let mut key = make_active_key();
        key.expires_at = Some(Utc::now() + chrono::Duration::hours(24));
        assert!(key.is_active(Utc::now()));
    }

    #[test]
    fn api_key_hash_key_produces_64_char_hex() {
        let hash = ApiKeyRecord::hash_key(b"test");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn api_key_hash_key_is_deterministic() {
        let h1 = ApiKeyRecord::hash_key(b"hello");
        let h2 = ApiKeyRecord::hash_key(b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn api_key_hash_key_differs_for_different_inputs() {
        assert_ne!(
            ApiKeyRecord::hash_key(b"key-a"),
            ApiKeyRecord::hash_key(b"key-b")
        );
    }

    // ── TenantRecord ─────────────────────────────────────────────────────────

    #[test]
    fn tenant_record_starts_with_zero_active_keys() {
        let record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Free);
        assert_eq!(record.active_key_count(Utc::now()), 0);
    }

    #[test]
    fn tenant_record_can_issue_key_while_under_plan_limit() {
        let record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Professional);
        assert!(record.can_issue_key(Utc::now()));
    }

    #[test]
    fn tenant_record_cannot_issue_key_when_at_plan_limit() {
        let now = Utc::now();
        let mut record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Free);
        // Add one active key to hit the Free plan limit.
        let key = make_active_key();
        record.keys.insert(key.id, key);
        assert_eq!(record.active_key_count(now), 1);
        assert!(!record.can_issue_key(now));
    }

    #[test]
    fn tenant_record_can_issue_key_after_revoking_to_under_limit() {
        let now = Utc::now();
        let mut record = TenantRecord::new(TenantId::new(), "test", TenantPlan::Free);
        let mut key = make_active_key();
        key.revoked_at = Some(now);
        record.keys.insert(key.id, key);
        // Revoked key does not count → still under limit.
        assert!(record.can_issue_key(now));
    }

    // ── TenantRegistry ───────────────────────────────────────────────────────

    #[test]
    fn tenant_registry_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let tenants_dir = dir.path().join("tenants");
        let tenant = TenantRecord::new(TenantId::new(), "alice", TenantPlan::Professional);
        let tenant_id = tenant.id;

        let mut registry = TenantRegistry::default();
        registry.tenants.insert(tenant_id, tenant);
        registry.save(&tenants_dir).unwrap();

        let loaded = TenantRegistry::load(&tenants_dir).unwrap();
        assert_eq!(loaded.tenants.len(), 1);
        assert!(loaded.tenants.contains_key(&tenant_id));
    }

    #[test]
    fn tenant_registry_load_returns_default_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let tenants_dir = dir.path().join("tenants");
        let registry = TenantRegistry::load(&tenants_dir).unwrap();
        assert!(registry.tenants.is_empty());
    }

    #[test]
    fn tenant_registry_detects_checksum_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let tenants_dir = dir.path().join("tenants");
        let mut registry = TenantRegistry::default();
        registry.save(&tenants_dir).unwrap();

        // Corrupt the saved file by replacing the checksum.
        let path = tenants_dir.join(TENANT_REGISTRY_FILENAME);
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replace(&registry.checksum, "0000000000000000");
        std::fs::write(&path, tampered).unwrap();

        let result = TenantRegistry::load(&tenants_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn tenant_registry_checksum_changes_when_tenants_change() {
        let mut r1 = TenantRegistry::default();
        let _ = r1.checksum; // empty before first save

        let dir1 = tempfile::tempdir().unwrap();
        let tenants_dir1 = dir1.path().join("tenants");
        r1.save(&tenants_dir1).unwrap();
        let checksum_empty = r1.checksum.clone();

        let tenant = TenantRecord::new(TenantId::new(), "bob", TenantPlan::Business);
        let id = tenant.id;
        r1.tenants.insert(id, tenant);
        r1.save(&tenants_dir1).unwrap();
        let checksum_with_tenant = r1.checksum.clone();

        assert_ne!(checksum_empty, checksum_with_tenant);
    }
}
