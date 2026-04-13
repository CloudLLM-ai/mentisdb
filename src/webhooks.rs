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
use uuid::Uuid;

use crate::Thought;
use crate::ThoughtType;

const MENTISDB_WEBHOOKS_FILENAME: &str = "mentisdb-webhooks.json";
const WEBHOOK_DELIVERY_TIMEOUT_SECS: u64 = 5;
const WEBHOOK_MAX_RETRIES: u32 = 3;
const WEBHOOK_INITIAL_BACKOFF_MS: u64 = 1000;

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
}

impl Clone for WebhookManager {
    fn clone(&self) -> Self {
        Self {
            chain_dir: self.chain_dir.clone(),
            webhooks: Arc::clone(&self.webhooks),
            dirty: Arc::clone(&self.dirty),
        }
    }
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
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &registry).map_err(|e| {
            io::Error::other(format!("failed to serialize webhooks registry: {}", e))
        })?;
        writer.flush()?;
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
            let url = registration.url.clone();
            let webhook_id = registration.id;
            tokio::spawn(async move {
                deliver_with_retries(url, payload, webhook_id).await;
            });
        }
    }
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
}
