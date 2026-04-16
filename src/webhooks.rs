//! Webhook notification system for MentisDB.
//!
//! This module provides webhook registration and delivery functionality, allowing
//! external HTTP endpoints to be notified when thoughts are appended to any chain.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    webhooks: Arc<Mutex<Vec<WebhookRegistration>>>,
    dirty: Arc<Mutex<bool>>,
    delivery_queue: Option<mpsc::Sender<DeliveryJob>>,
}

impl Clone for WebhookManager {
    fn clone(&self) -> Self {
        Self {
            chain_dir: self.chain_dir.clone(),
            webhooks: Arc::clone(&self.webhooks),
            dirty: Arc::clone(&self.dirty),
            delivery_queue: self.delivery_queue.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct DeliveryJob {
    url: String,
    payload: WebhookPayload,
    webhook_id: Uuid,
}

impl WebhookManager {
    /// Creates a new WebhookManager that loads and persists webhook registrations
    /// in `mentisdb-webhooks.json` within the specified chain directory.
    pub fn new(chain_dir: PathBuf) -> io::Result<Self> {
        let webhooks = Self::load_webhooks(&chain_dir)?;
        Ok(Self {
            chain_dir,
            webhooks: Arc::new(Mutex::new(webhooks)),
            dirty: Arc::new(Mutex::new(false)),
            delivery_queue: tokio::runtime::Handle::try_current()
                .ok()
                .map(|_| spawn_delivery_worker()),
        })
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
        let webhooks = self.webhooks.lock().unwrap();
        let registry = WebhookRegistry {
            version: 1,
            webhooks: webhooks.clone(),
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
        let mut dirty = self.dirty.lock().unwrap();
        *dirty = false;
        Ok(())
    }

    /// Returns all webhook registrations.
    pub fn list_webhooks(&self) -> Vec<WebhookRegistration> {
        self.webhooks.lock().unwrap().clone()
    }

    /// Registers a new webhook endpoint.
    ///
    /// - `url`: The HTTP endpoint to call on thought append events.
    /// - `chain_key_filter`: If Some, only fire for this chain; None = all chains.
    /// - `thought_type_filter`: If Some, only fire for these thought types; None = all types.
    pub fn register_webhook(
        &self,
        url: String,
        chain_key_filter: Option<String>,
        thought_type_filter: Option<HashSet<ThoughtType>>,
    ) -> io::Result<WebhookRegistration> {
        let registration = WebhookRegistration {
            id: Uuid::new_v4(),
            url,
            chain_key_filter,
            thought_type_filter,
            created_at: Utc::now(),
            active: true,
        };
        {
            let mut webhooks = self.webhooks.lock().unwrap();
            webhooks.push(registration.clone());
        }
        {
            let mut dirty = self.dirty.lock().unwrap();
            *dirty = true;
        }
        self.save()?;
        Ok(registration)
    }

    /// Deletes a webhook registration by ID.
    ///
    /// Returns `true` if the webhook was found and deleted, `false` otherwise.
    pub fn delete_webhook(&self, id: Uuid) -> io::Result<bool> {
        let mut webhooks = self.webhooks.lock().unwrap();
        let initial_len = webhooks.len();
        webhooks.retain(|w| w.id != id);
        let deleted = webhooks.len() != initial_len;
        if deleted {
            let mut dirty = self.dirty.lock().unwrap();
            *dirty = true;
        }
        drop(webhooks);
        if deleted {
            self.save()?;
        }
        Ok(deleted)
    }

    /// Retrieves a webhook registration by ID.
    pub fn get_webhook(&self, id: Uuid) -> Option<WebhookRegistration> {
        self.webhooks
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.id == id)
            .cloned()
    }

    /// Delivers a webhook notification for a newly appended thought.
    ///
    /// This method is non-blocking: it spawns asynchronous tasks for each matching
    /// webhook and returns immediately.
    pub fn deliver_for_thought(&self, chain_key: &str, thought: &Thought) {
        let webhooks = self.list_webhooks();
        for registration in webhooks {
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

fn spawn_delivery_worker() -> mpsc::Sender<DeliveryJob> {
    let (tx, mut rx) = mpsc::channel::<DeliveryJob>(WEBHOOK_DELIVERY_QUEUE_CAPACITY);
    let semaphore = Arc::new(Semaphore::new(WEBHOOK_MAX_CONCURRENT_DELIVERIES));
    let runtime = tokio::runtime::Handle::try_current()
        .expect("webhook delivery worker requires a Tokio runtime");
    runtime.spawn(async move {
        while let Some(job) = rx.recv().await {
            let semaphore = Arc::clone(&semaphore);
            match semaphore.acquire_owned().await {
                Ok(permit) => {
                    tokio::spawn(async move {
                        let _permit = permit;
                        deliver_with_retries(job.url, job.payload, job.webhook_id).await;
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

async fn deliver_with_retries(url: String, payload: WebhookPayload, webhook_id: Uuid) {
    let client = reqwest::Client::new();
    let mut backoff_ms = WEBHOOK_INITIAL_BACKOFF_MS;

    for attempt in 0..WEBHOOK_MAX_RETRIES {
        match deliver_once(&client, &url, &payload).await {
            Ok(()) => {
                log::info!(
                    target: "mentisdb::webhooks",
                    "webhook {} delivered successfully to {} on attempt {}",
                    webhook_id,
                    url,
                    attempt + 1
                );
                return;
            }
            Err(e) => {
                log::warn!(
                    target: "mentisdb::webhooks",
                    "webhook {} delivery failed to {} on attempt {}: {}",
                    webhook_id,
                    url,
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
        url,
        WEBHOOK_MAX_RETRIES
    );
}

async fn deliver_once(
    client: &reqwest::Client,
    url: &str,
    payload: &WebhookPayload,
) -> io::Result<()> {
    let timeout = tokio::time::Duration::from_secs(WEBHOOK_DELIVERY_TIMEOUT_SECS);
    client
        .post(url)
        .json(payload)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| io::Error::other(format!("webhook request failed: {}", e)))?
        .error_for_status()
        .map_err(|e| io::Error::other(format!("webhook response error: {}", e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[test]
    fn webhook_registration_round_trips() {
        let dir = tempdir().unwrap();
        let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
        let registration = manager
            .register_webhook(
                "https://example.com/webhook".to_string(),
                Some("test-chain".to_string()),
                None,
            )
            .unwrap();
        assert!(registration.id != Uuid::nil());
        let all = manager.list_webhooks();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].url, "https://example.com/webhook");
        let loaded = manager.get_webhook(registration.id).unwrap();
        assert_eq!(loaded.id, registration.id);
    }

    #[test]
    fn webhook_delete() {
        let dir = tempdir().unwrap();
        let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
        let registration = manager
            .register_webhook("https://example.com/webhook".to_string(), None, None)
            .unwrap();
        let deleted = manager.delete_webhook(registration.id).unwrap();
        assert!(deleted);
        assert!(manager.list_webhooks().is_empty());
        assert!(manager.get_webhook(registration.id).is_none());
    }

    #[test]
    fn webhook_chain_key_filter() {
        let dir = tempdir().unwrap();
        let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
        manager
            .register_webhook("https://example.com/all".to_string(), None, None)
            .unwrap();
        manager
            .register_webhook(
                "https://example.com/only-test".to_string(),
                Some("test-chain".to_string()),
                None,
            )
            .unwrap();
        let all = manager.list_webhooks();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn webhook_thought_type_filter() {
        let dir = tempdir().unwrap();
        let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
        let mut type_filter = HashSet::new();
        type_filter.insert(ThoughtType::LessonLearned);
        manager
            .register_webhook(
                "https://example.com/lessons".to_string(),
                None,
                Some(type_filter),
            )
            .unwrap();
        let all = manager.list_webhooks();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].thought_type_filter.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn webhook_save_keeps_existing_registry_when_rename_fails() {
        let dir = tempdir().unwrap();
        let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
        manager
            .register_webhook("https://example.com/original".to_string(), None, None)
            .unwrap();

        let path = WebhookManager::webhooks_path(dir.path());
        let original = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();

        {
            let mut webhooks = manager.webhooks.lock().unwrap();
            webhooks.push(WebhookRegistration {
                id: Uuid::new_v4(),
                url: "https://example.com/new".to_string(),
                chain_key_filter: None,
                thought_type_filter: None,
                created_at: Utc::now(),
                active: true,
            });
        }

        let error = manager.save().unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::AlreadyExists
                    | io::ErrorKind::IsADirectory
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::Other
            ),
            "unexpected save error kind: {:?}",
            error.kind()
        );
        let restored_path = path.with_extension("json.restored");
        fs::write(&restored_path, &original).unwrap();
        assert_eq!(fs::read_to_string(&restored_path).unwrap(), original);
        assert!(!path.with_extension("json.tmp").exists());
        fs::remove_dir(&path).unwrap();
        fs::rename(&restored_path, &path).unwrap();
    }

    #[tokio::test]
    async fn delivery_worker_applies_backpressure_with_bounded_concurrency() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let active_for_server = Arc::clone(&active);
        let max_seen_for_server = Arc::clone(&max_seen);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let active_for_conn = Arc::clone(&active_for_server);
                let max_seen_for_conn = Arc::clone(&max_seen_for_server);
                tokio::spawn(async move {
                    let current = active_for_conn.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen_for_conn.fetch_max(current, Ordering::SeqCst);
                    let mut buffer = [0_u8; 4096];
                    loop {
                        let read = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer)
                            .await
                            .unwrap();
                        if read == 0
                            || buffer[..read]
                                .windows(4)
                                .any(|window| window == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                    tokio::io::AsyncWriteExt::write_all(
                        &mut socket,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                    active_for_conn.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        let receiver = spawn_delivery_worker();
        let payload = WebhookPayload {
            event: "thought.appended".to_string(),
            chain_key: "test-chain".to_string(),
            thought: WebhookThought {
                id: Uuid::new_v4(),
                thought_type: ThoughtType::Finding,
                content: "payload".to_string(),
                importance: 0.5,
                confidence: Some(0.9),
                tags: Vec::new(),
                concepts: Vec::new(),
                agent_id: "agent".to_string(),
                index: 1,
                timestamp: Utc::now(),
            },
            timestamp: Utc::now(),
        };

        for i in 0..(WEBHOOK_MAX_CONCURRENT_DELIVERIES * 3) {
            receiver
                .send(DeliveryJob {
                    url: format!("http://{address}/webhook/{i}"),
                    payload: payload.clone(),
                    webhook_id: Uuid::new_v4(),
                })
                .await
                .unwrap();
        }
        drop(receiver);

        tokio::time::sleep(tokio::time::Duration::from_millis(450)).await;
        assert!(max_seen.load(Ordering::SeqCst) <= WEBHOOK_MAX_CONCURRENT_DELIVERIES);
        server.abort();
    }
}
