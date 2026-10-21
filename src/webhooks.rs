//! Webhook notification system for MentisDB.
//!
//! This module provides webhook registration and delivery functionality, allowing
//! external HTTP endpoints to be notified when thoughts are appended to any chain.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

use crate::Thought;
use crate::ThoughtType;

const MENTISDB_WEBHOOKS_FILENAME: &str = "mentisdb-webhooks.json";
const WEBHOOK_DELIVERY_TIMEOUT_SECS: u64 = 5;
const WEBHOOK_MAX_RETRIES: u32 = 3;
const WEBHOOK_INITIAL_BACKOFF_MS: u64 = 1000;
const WEBHOOK_DELIVERY_QUEUE_CAPACITY: usize = 256;
const WEBHOOK_MAX_CONCURRENT_DELIVERIES: usize = 16;
/// Consecutive failed deliveries (each already retried) before a webhook is paused.
pub const WEBHOOK_BREAKER_FAILURE_THRESHOLD: u32 = 5;
/// How long a tripped webhook is skipped before deliveries are attempted again.
pub const WEBHOOK_BREAKER_COOLDOWN: Duration = Duration::from_secs(300);

/// Locks a mutex, recovering the guard if a previous holder panicked.
///
/// Webhook state is simple data that stays consistent across a panic, so
/// poisoning must not take the daemon's append path down with it.
fn lock_tolerant<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Policy deciding which webhook targets may be contacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookUrlPolicy {
    /// Reject loopback, private, link-local and other internal targets (default).
    Strict,
    /// Permit internal targets. Intended only for local testing.
    AllowPrivate,
}

/// A registered webhook endpoint that receives notifications when thoughts are appended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookRegistration {
    /// Unique identifier for this webhook registration.
    pub id: Uuid,
    /// The HTTP endpoint URL to call on thought append events.
    pub url: String,
    /// Optional chain key filter. If set, only fire for this specific chain.
    pub chain_key_filter: Option<String>,
    /// Optional set of thought types to filter. If set, only fire for these thought types.
    pub thought_type_filter: Option<HashSet<ThoughtType>>,
    /// UTC timestamp when this webhook was registered.
    pub created_at: DateTime<Utc>,
    /// Whether this webhook is active and should receive notifications.
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WebhookRegistry {
    version: u32,
    webhooks: Vec<WebhookRegistration>,
}

impl Default for WebhookRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            webhooks: Vec::new(),
        }
    }
}

/// JSON payload sent to webhook endpoints when a thought is appended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// The event type, always "thought.appended".
    pub event: String,
    /// The chain key where the thought was appended.
    pub chain_key: String,
    /// The thought that was appended.
    pub thought: WebhookThought,
    /// UTC timestamp when the webhook was sent.
    pub timestamp: DateTime<Utc>,
}

/// Simplified thought data included in webhook payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookThought {
    /// Unique identifier of the thought.
    pub id: Uuid,
    /// Semantic type of the thought.
    pub thought_type: ThoughtType,
    /// Primary content of the thought.
    pub content: String,
    /// Importance score between 0.0 and 1.0.
    pub importance: f32,
    /// Optional confidence score between 0.0 and 1.0.
    pub confidence: Option<f32>,
    /// Tags attached to the thought.
    pub tags: Vec<String>,
    /// Concept labels attached to the thought.
    pub concepts: Vec<String>,
    /// Agent identifier who created the thought.
    pub agent_id: String,
    /// Zero-based position within the chain.
    pub index: u64,
    /// UTC timestamp when the thought was recorded.
    pub timestamp: DateTime<Utc>,
}

impl From<&Thought> for WebhookThought {
    fn from(thought: &Thought) -> Self {
        Self {
            id: thought.id,
            thought_type: thought.thought_type,
            content: thought.content.clone(),
            importance: thought.importance,
            confidence: thought.confidence,
            tags: thought.tags.clone(),
            concepts: thought.concepts.clone(),
            agent_id: thought.agent_id.clone(),
            index: thought.index,
            timestamp: thought.timestamp,
        }
    }
}

/// Manages webhook registrations and delivers notifications to registered endpoints.
pub struct WebhookManager {
    chain_dir: PathBuf,
    policy: WebhookUrlPolicy,
    webhooks: Arc<RwLock<Arc<Vec<WebhookRegistration>>>>,
    delivery_queue: Option<mpsc::Sender<DeliveryJob>>,
}

impl Clone for WebhookManager {
    fn clone(&self) -> Self {
        Self {
            chain_dir: self.chain_dir.clone(),
            policy: self.policy,
            webhooks: Arc::clone(&self.webhooks),
            delivery_queue: self.delivery_queue.clone(),
        }
    }
}

/// A queued webhook delivery.
#[derive(Debug, Clone)]
pub struct DeliveryJob {
    /// Target URL.
    pub url: String,
    /// Payload to POST.
    pub payload: WebhookPayload,
    /// Registration this job belongs to.
    pub webhook_id: Uuid,
}

impl WebhookManager {
    /// Creates a new WebhookManager that loads and persists webhook registrations
    /// in `mentisdb-webhooks.json` within the specified chain directory.
    pub fn new(chain_dir: PathBuf) -> io::Result<Self> {
        Self::new_with_policy(chain_dir, WebhookUrlPolicy::Strict)
    }

    /// Like [`WebhookManager::new`] but with an explicit target policy.
    ///
    /// Registrations loaded from disk are re-validated against the policy;
    /// any that fail (for example hand-edited to point at an internal host)
    /// are loaded inactive and a warning is logged.
    pub fn new_with_policy(chain_dir: PathBuf, policy: WebhookUrlPolicy) -> io::Result<Self> {
        let webhooks = Self::load_webhooks(&chain_dir)?
            .into_iter()
            .map(|mut registration| {
                if let Err(error) = validate_webhook_url_with(&registration.url, policy) {
                    log::warn!(
                        target: "mentisdb::webhooks",
                        "disabling webhook {} loaded from disk: {}",
                        registration.id,
                        error
                    );
                    registration.active = false;
                }
                registration
            })
            .collect();
        Ok(Self {
            chain_dir,
            policy,
            webhooks: Arc::new(RwLock::new(Arc::new(webhooks))),
            delivery_queue: tokio::runtime::Handle::try_current()
                .ok()
                .map(|_| spawn_delivery_worker(policy)),
        })
    }

    fn policy(&self) -> WebhookUrlPolicy {
        self.policy
    }

    fn webhooks_path(chain_dir: &Path) -> PathBuf {
        chain_dir.join(MENTISDB_WEBHOOKS_FILENAME)
    }

    fn load_webhooks(chain_dir: &Path) -> io::Result<Vec<WebhookRegistration>> {
        let path = Self::webhooks_path(chain_dir);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let registry: WebhookRegistry = serde_json::from_reader(reader)
            .map_err(|e| io::Error::other(format!("failed to parse webhooks registry: {}", e)))?;
        Ok(registry.webhooks)
    }

    /// Persists webhook registrations to disk.
    pub fn save(&self) -> io::Result<()> {
        let snapshot = self.snapshot();
        let registry = WebhookRegistry {
            version: 1,
            webhooks: snapshot.as_ref().clone(),
        };
        let path = Self::webhooks_path(&self.chain_dir);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let temp_path = path.with_extension("json.tmp");
        if temp_path.exists() {
            fs::remove_file(&temp_path)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &registry).map_err(|e| {
            io::Error::other(format!("failed to serialize webhooks registry: {}", e))
        })?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        if let Err(error) = fs::rename(&temp_path, &path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        Ok(())
    }

    /// Cheap shared snapshot of the registrations (no per-call deep clone).
    fn snapshot(&self) -> Arc<Vec<WebhookRegistration>> {
        Arc::clone(&self.webhooks.read().unwrap_or_else(|p| p.into_inner()))
    }

    /// Copy-on-write mutation of the registration list.
    fn mutate<R>(&self, f: impl FnOnce(&mut Vec<WebhookRegistration>) -> R) -> R {
        let mut guard = self.webhooks.write().unwrap_or_else(|p| p.into_inner());
        let mut next = guard.as_ref().clone();
        let result = f(&mut next);
        *guard = Arc::new(next);
        result
    }

    /// Returns all webhook registrations.
    pub fn list_webhooks(&self) -> Vec<WebhookRegistration> {
        self.snapshot().as_ref().clone()
    }

    /// Registers a new webhook endpoint.
    ///
    /// - `url`: The HTTP endpoint to call on thought append events.
    /// - `chain_key_filter`: If Some, only fire for this chain; None = all chains.
    /// - `thought_type_filter`: If Some, only fire for these thought types; None = all types.
    ///
    /// # URL validation
    ///
    /// Only `http://` and `https://` schemes are accepted. URLs pointing to
    /// loopback, private, link-local, multicast, CGNAT, documentation or
    /// IPv4-mapped-IPv6 equivalents of those are rejected to prevent
    /// server-side request forgery (SSRF). Hostnames are additionally
    /// re-resolved and filtered at delivery time to defeat DNS rebinding.
    pub fn register_webhook(
        &self,
        url: String,
        chain_key_filter: Option<String>,
        thought_type_filter: Option<HashSet<ThoughtType>>,
    ) -> io::Result<WebhookRegistration> {
        validate_webhook_url_with(&url, self.policy())?;
        let registration = WebhookRegistration {
            id: Uuid::new_v4(),
            url,
            chain_key_filter,
            thought_type_filter,
            created_at: Utc::now(),
            active: true,
        };
        self.mutate(|webhooks| webhooks.push(registration.clone()));
        self.save()?;
        Ok(registration)
    }

    /// Deletes a webhook registration by ID.
    ///
    /// Returns `true` if the webhook was found and deleted, `false` otherwise.
    pub fn delete_webhook(&self, id: Uuid) -> io::Result<bool> {
        let deleted = self.mutate(|webhooks| {
            let initial_len = webhooks.len();
            webhooks.retain(|w| w.id != id);
            webhooks.len() != initial_len
        });
        if deleted {
            self.save()?;
        }
        Ok(deleted)
    }

    /// Retrieves a webhook registration by ID.
    pub fn get_webhook(&self, id: Uuid) -> Option<WebhookRegistration> {
        self.snapshot().iter().find(|w| w.id == id).cloned()
    }

    /// Delivers a webhook notification for a newly appended thought.
    ///
    /// This method is non-blocking: it spawns asynchronous tasks for each matching
    /// webhook and returns immediately.
    pub fn deliver_for_thought(&self, chain_key: &str, thought: &Thought) {
        let webhooks = self.snapshot();
        for registration in webhooks.iter() {
            if !registration.active {
                continue;
            }
            if let Some(ref filter_chain) = registration.chain_key_filter {
                if filter_chain != chain_key {
                    continue;
                }
            }
            if let Some(ref type_filter) = registration.thought_type_filter {
                if !type_filter.contains(&thought.thought_type) {
                    continue;
                }
            }
            let payload = WebhookPayload {
                event: "thought.appended".to_string(),
                chain_key: chain_key.to_string(),
                thought: WebhookThought::from(thought),
                timestamp: Utc::now(),
            };
            let Some(queue) = &self.delivery_queue else {
                continue;
            };
            let job = DeliveryJob {
                url: registration.url.clone(),
                payload,
                webhook_id: registration.id,
            };
            if let Err(error) = queue.try_send(job) {
                log::warn!(
                    target: "mentisdb::webhooks",
                    "dropping webhook delivery because the queue is full or unavailable: {}",
                    error
                );
            }
        }
    }
}

/// Per-webhook consecutive-failure circuit breaker.
///
/// After `threshold` consecutive failed deliveries a webhook is skipped for
/// `cooldown`, so dead endpoints stop holding delivery permits. A success
/// resets the counter.
#[derive(Debug)]
pub struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    state: Mutex<HashMap<Uuid, BreakerState>>,
}

#[derive(Debug, Default, Clone, Copy)]
struct BreakerState {
    consecutive_failures: u32,
    paused_until: Option<Instant>,
}

impl CircuitBreaker {
    /// Creates a breaker that trips after `threshold` consecutive failures.
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            threshold,
            cooldown,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` when a delivery to `id` may be attempted at `now`.
    pub fn allow(&self, id: Uuid, now: Instant) -> bool {
        let mut state = lock_tolerant(&self.state);
        match state.get_mut(&id) {
            Some(entry) => match entry.paused_until {
                Some(until) if now < until => false,
                Some(_) => {
                    // Cooldown elapsed: half-open, one more failure re-trips.
                    entry.paused_until = None;
                    entry.consecutive_failures = self.threshold.saturating_sub(1);
                    true
                }
                None => true,
            },
            None => true,
        }
    }

    /// Records a successful delivery, closing the breaker.
    pub fn record_success(&self, id: Uuid) {
        lock_tolerant(&self.state).remove(&id);
    }

    /// Records a failed delivery; returns `true` if this failure tripped the breaker.
    pub fn record_failure(&self, id: Uuid, now: Instant) -> bool {
        let mut state = lock_tolerant(&self.state);
        let entry = state.entry(id).or_default();
        entry.consecutive_failures += 1;
        if entry.consecutive_failures >= self.threshold {
            entry.paused_until = Some(now + self.cooldown);
            true
        } else {
            false
        }
    }
}

/// Spawns the background delivery worker and returns its job queue.
///
/// Must be called from within a Tokio runtime.
pub fn spawn_delivery_worker(policy: WebhookUrlPolicy) -> mpsc::Sender<DeliveryJob> {
    spawn_delivery_worker_with_breaker(
        policy,
        Arc::new(CircuitBreaker::new(
            WEBHOOK_BREAKER_FAILURE_THRESHOLD,
            WEBHOOK_BREAKER_COOLDOWN,
        )),
    )
}

/// Like [`spawn_delivery_worker`] with a caller-provided circuit breaker.
pub fn spawn_delivery_worker_with_breaker(
    policy: WebhookUrlPolicy,
    breaker: Arc<CircuitBreaker>,
) -> mpsc::Sender<DeliveryJob> {
    let (tx, mut rx) = mpsc::channel::<DeliveryJob>(WEBHOOK_DELIVERY_QUEUE_CAPACITY);
    let semaphore = Arc::new(Semaphore::new(WEBHOOK_MAX_CONCURRENT_DELIVERIES));
    let runtime = tokio::runtime::Handle::try_current()
        .expect("webhook delivery worker requires a Tokio runtime");
    runtime.spawn(async move {
        while let Some(job) = rx.recv().await {
            if !breaker.allow(job.webhook_id, Instant::now()) {
                log::debug!(
                    target: "mentisdb::webhooks",
                    "webhook {} paused by circuit breaker; skipping delivery",
                    job.webhook_id
                );
                continue;
            }
            let semaphore = Arc::clone(&semaphore);
            match semaphore.acquire_owned().await {
                Ok(permit) => {
                    let breaker = Arc::clone(&breaker);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let ok = deliver_with_retries(job.url, job.payload, job.webhook_id, policy)
                            .await;
                        if ok {
                            breaker.record_success(job.webhook_id);
                        } else if breaker.record_failure(job.webhook_id, Instant::now()) {
                            log::error!(
                                target: "mentisdb::webhooks",
                                "webhook {} paused after repeated delivery failures",
                                job.webhook_id
                            );
                        }
                    });
                }
                Err(error) => {
                    log::error!(
                        target: "mentisdb::webhooks",
                        "webhook delivery worker stopped before dispatch: {}",
                        error
                    );
                    break;
                }
            }
        }
    });
    tx
}

/// DNS resolver that drops blocked addresses, defeating DNS rebinding and
/// hostnames that point at internal ranges.
struct SsrfGuardResolver;

impl reqwest::dns::Resolve for SsrfGuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| !is_blocked_ip(&addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "webhook host resolves only to blocked addresses",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn build_client(policy: WebhookUrlPolicy) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    let builder = match policy {
        WebhookUrlPolicy::Strict => builder.dns_resolver(Arc::new(SsrfGuardResolver)),
        WebhookUrlPolicy::AllowPrivate => builder,
    };
    builder
        .build()
        .expect("webhook HTTP client configuration is static and valid")
}

fn webhook_http_client(policy: WebhookUrlPolicy) -> &'static reqwest::Client {
    static STRICT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    static PERMISSIVE: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    match policy {
        WebhookUrlPolicy::Strict => STRICT.get_or_init(|| build_client(policy)),
        WebhookUrlPolicy::AllowPrivate => PERMISSIVE.get_or_init(|| build_client(policy)),
    }
}

/// Reduces a URL to `scheme://host[:port]` so credentials, paths and query
/// strings (which often carry secrets) never reach logs.
pub fn redact_webhook_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("?");
            match parsed.port() {
                Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
                None => format!("{}://{}", parsed.scheme(), host),
            }
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}

async fn deliver_with_retries(
    url: String,
    payload: WebhookPayload,
    webhook_id: Uuid,
    policy: WebhookUrlPolicy,
) -> bool {
    let client = webhook_http_client(policy);
    let redacted = redact_webhook_url(&url);
    let mut backoff_ms = WEBHOOK_INITIAL_BACKOFF_MS;

    for attempt in 0..WEBHOOK_MAX_RETRIES {
        match deliver_once(client, &url, &payload, policy).await {
            Ok(()) => {
                log::info!(
                    target: "mentisdb::webhooks",
                    "webhook {} delivered successfully to {} on attempt {}",
                    webhook_id,
                    redacted,
                    attempt + 1
                );
                return true;
            }
            Err(e) => {
                log::warn!(
                    target: "mentisdb::webhooks",
                    "webhook {} delivery failed to {} on attempt {}: {}",
                    webhook_id,
                    redacted,
                    attempt + 1,
                    e
                );
                if attempt < WEBHOOK_MAX_RETRIES - 1 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= 2;
                }
            }
        }
    }
    log::error!(
        target: "mentisdb::webhooks",
        "webhook {} failed to deliver to {} after {} attempts",
        webhook_id,
        redacted,
        WEBHOOK_MAX_RETRIES
    );
    false
}

async fn deliver_once(
    client: &reqwest::Client,
    url: &str,
    payload: &WebhookPayload,
    policy: WebhookUrlPolicy,
) -> io::Result<()> {
    // Re-validate at delivery time: literal IPs bypass the DNS resolver.
    validate_webhook_url_with(url, policy)?;
    let timeout = tokio::time::Duration::from_secs(WEBHOOK_DELIVERY_TIMEOUT_SECS);
    // Redirects are disabled, so a 3xx surfaces as a non-success status
    // (an error) instead of a hop to an unvalidated target.
    client
        .post(url)
        .json(payload)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| io::Error::other(format!("webhook request failed: {}", e.without_url())))?
        .error_for_status()
        .map_err(|e| io::Error::other(format!("webhook response error: {}", e.without_url())))?;
    Ok(())
}

/// Validate that a webhook URL is safe to deliver to (strict policy).
///
/// # Errors
///
/// Returns `InvalidInput` for unparsable URLs, non-HTTP(S) schemes,
/// `localhost`, and literal IPs in blocked ranges.
pub fn validate_webhook_url(url: &str) -> io::Result<()> {
    validate_webhook_url_with(url, WebhookUrlPolicy::Strict)
}

/// Validate a webhook URL under an explicit [`WebhookUrlPolicy`].
pub fn validate_webhook_url_with(url: &str, policy: WebhookUrlPolicy) -> io::Result<()> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
    let parsed = url::Url::parse(url).map_err(|e| invalid(format!("invalid webhook URL: {e}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(invalid(format!(
                "webhook URL scheme '{scheme}' is not allowed; use http or https"
            )));
        }
    }
    if policy == WebhookUrlPolicy::AllowPrivate {
        return Ok(());
    }

    match parsed.host() {
        None => Err(invalid("webhook URL has no host".to_string())),
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            if domain == "localhost" || domain.ends_with(".localhost") {
                Err(invalid(
                    "webhook URLs pointing to localhost are not allowed".to_string(),
                ))
            } else {
                Ok(())
            }
        }
        Some(url::Host::Ipv4(ip)) => check_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => check_ip(IpAddr::V6(ip)),
    }
}

fn check_ip(ip: IpAddr) -> io::Result<()> {
    if is_blocked_ip(&ip) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("webhook URL points to a blocked IP address ({ip}); internal, private, loopback and link-local addresses are not allowed"),
        ))
    } else {
        Ok(())
    }
}

fn is_blocked_v4(v4: &Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (b & 0xc0) == 64) // CGNAT 100.64.0.0/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // TEST-NET-1
        || (a == 198 && b == 51 && c == 100) // TEST-NET-2
        || (a == 203 && b == 0 && c == 113) // TEST-NET-3
        || (a == 198 && (b & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || a >= 240 // reserved
}

/// Returns `true` if the IP address is in a range that must not be reachable
/// from a webhook delivery worker.
///
/// IPv6 addresses that embed an IPv4 address (IPv4-mapped, IPv4-compatible,
/// NAT64 and 6to4) are checked as the embedded IPv4 address.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v6(v6: &Ipv6Addr) -> bool {
    if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
        return true;
    }
    if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
        return is_blocked_v4(&v4);
    }
    let seg = v6.segments();
    let embedded =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    // NAT64 well-known prefix 64:ff9b::/96 embeds the IPv4 target.
    if seg[0] == 0x64 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0) {
        return is_blocked_v4(&embedded(seg[6], seg[7]));
    }
    // 6to4 2002::/16 embeds the IPv4 target in segments 1-2.
    if seg[0] == 0x2002 {
        return is_blocked_v4(&embedded(seg[1], seg[2]));
    }
    (seg[0] & 0xfe00) == 0xfc00 // ULA
        || (seg[0] & 0xffc0) == 0xfe80 // link-local
        || (seg[0] & 0xffc0) == 0xfec0 // deprecated site-local
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
        || (seg[0] == 0x0100 && seg[1..4].iter().all(|s| *s == 0)) // discard-only
}
