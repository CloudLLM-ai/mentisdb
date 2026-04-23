//! Standalone MentisDb daemon.
//!
//! This binary starts both:
//!
//! - an MCP server (HTTP and optionally HTTPS)
//! - a REST server (HTTP and optionally HTTPS)
//!
//! Configuration is read from environment variables:
//!
//! - `MENTISDB_DIR`
//! - `MENTISDB_DEFAULT_CHAIN_KEY` (deprecated alias: `MENTISDB_DEFAULT_KEY`)
//! - `MENTISDB_STORAGE_ADAPTER`
//! - `MENTISDB_AUTO_FLUSH` (defaults to `true`; set `false` for buffered writes instead of durable group commit)
//! - `MENTISDB_VERBOSE` (defaults to `true` when unset)
//! - `MENTISDB_LOG_FILE`
//! - `MENTISDB_BIND_HOST`
//! - `MENTISDB_MCP_PORT`
//! - `MENTISDB_REST_PORT`
//! - `MENTISDB_HTTPS_MCP_PORT` (set to 0 to disable; default 9473)
//! - `MENTISDB_HTTPS_REST_PORT` (set to 0 to disable; default 9474)
//! - `MENTISDB_TLS_CERT` (default `~/.cloudllm/mentisdb/tls/cert.pem`)
//! - `MENTISDB_TLS_KEY` (default `~/.cloudllm/mentisdb/tls/key.pem`)
//! - `MENTISDB_UPDATE_CHECK` (default `true`; set `0`/`false`/`no`/`off` to disable background GitHub release checks)
//! - `MENTISDB_UPDATE_REPO` (default `CloudLLM-ai/mentisdb`)
//! - `MENTISDB_STARTUP_SOUND` (default `true`; set `0`/`false`/`no`/`off` to silence)
//! - `MENTISDB_THOUGHT_SOUNDS` (default `false`; set `1`/`true`/`yes`/`on` to enable per-thought and per-read sounds)
//! - `RUST_LOG`

use env_logger::Env;
use mcp::ToolProtocol;
use mentisdb::integrations::detect::{detect_integrations_with_environment, DetectionStatus};
use mentisdb::paths::{HostPlatform, PathEnvironment};
use mentisdb::server::{
    adopt_legacy_default_mentisdb_dir, start_servers, LegacyDefaultStorageMigration,
    MentisDbMcpProtocol, MentisDbServerConfig, MentisDbServerHandles, MentisDbService,
};
use mentisdb::tui::{self, AgentInfo, ChainInfo, SkillInfo, TuiState};
use mentisdb::{
    load_registered_chains, migrate_chain_hash_algorithm, migrate_registered_chains_with_adapter,
    migrate_skill_registry, refresh_registered_chain_counts, MentisDb, MentisDbMigrationEvent,
    MentisDbMigrationReport, SkillRegistry, ThoughtType,
};
use serde::Deserialize;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(feature = "startup-sound")]
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Embedded MentisDB skill markdown for proxy-mode resource reads.
const MENTISDB_SKILL_MD: &str = include_str!("../../MENTISDB_SKILL.md");

type StartupData = (
    MentisDbServerConfig,
    UpdateConfig,
    MentisDbServerHandles,
    bool,
    FirstRunSetupStatus,
    String,
    Vec<MentisDbMigrationReport>,
    String,
    Option<LegacyDefaultStorageMigration>,
);

/// Raise the open-file-descriptor limit to the OS hard cap (up to 65 535).
///
/// The macOS default soft limit is 256, which is easily exhausted when the
/// daemon handles thousands of concurrent ingestion requests (each TCP
/// connection consumes one file descriptor). This is a no-op on Windows.
#[cfg(unix)]
fn raise_fd_limit() {
    // SAFETY: `rlimit` is a plain C struct; zero-initialization is valid.
    // `RLIMIT_NOFILE` is a standard POSIX constant always defined on Unix.
    // `getrlimit` and `setrlimit` are pure C FFI calls with no lifetime
    // requirements beyond the `rlimit` pointer being valid during the call.
    // The target value is bounded to 65_535, well within kernel limits.
    unsafe {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) != 0 {
            return;
        }
        let target = rlim.rlim_max.min(65_535);
        if rlim.rlim_cur < target {
            rlim.rlim_cur = target;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
        }
    }
}

#[cfg(not(unix))]
fn raise_fd_limit() {}

const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";
#[cfg(feature = "startup-sound")]
pub(crate) const THOUGHT_SOUND_GAP_MS: u64 = 90;
pub(crate) const DEFAULT_UPDATE_REPO: &str = "CloudLLM-ai/mentisdb";
const GITHUB_API_BASE: &str = "https://api.github.com";
const UPDATE_BINARY_NAME: &str = "mentisdbd";
const UPDATE_CRATE_NAME: &str = "mentisdb";

#[derive(Debug, Clone)]
pub(crate) struct UpdateConfig {
    pub(crate) enabled: bool,
    pub(crate) repo: String,
}

#[derive(Debug, Clone)]
struct UpdateRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    html_url: String,
}

// ── Startup jingle ────────────────────────────────────────────────────────────

/// A square-wave tone source for `rodio`.
///
/// Produces a mono square wave at `freq` Hz for exactly `num_samples` frames
/// at 44 100 Hz.  Amplitude is kept low (±0.25) so it stays pleasant even on
/// laptop speakers.
#[cfg(feature = "startup-sound")]
struct SquareWave {
    freq: f32,
    sample_rate: u32,
    num_samples: usize,
    elapsed: usize,
}

#[cfg(feature = "startup-sound")]
impl SquareWave {
    fn new(freq: f32, duration_ms: u64) -> Self {
        const SR: u32 = 44_100;
        let num_samples = (SR as f64 * duration_ms as f64 / 1_000.0) as usize;
        Self {
            freq,
            sample_rate: SR,
            num_samples,
            elapsed: 0,
        }
    }
}

#[cfg(feature = "startup-sound")]
impl Iterator for SquareWave {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.elapsed >= self.num_samples {
            return None;
        }
        let period = self.sample_rate as f32 / self.freq;
        let pos = self.elapsed as f32 % period;
        self.elapsed += 1;
        Some(if pos < period / 2.0 { 0.25 } else { -0.25 })
    }
}

#[cfg(feature = "startup-sound")]
impl rodio::Source for SquareWave {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(1).unwrap()
    }
    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate).unwrap()
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_millis(
            self.num_samples as u64 * 1_000 / self.sample_rate as u64,
        ))
    }
}

/// A sine-wave tone source for `rodio`.
///
/// Produces a mono pure sine wave at `freq` Hz.  Amplitude is low (±0.2)
/// so the high-frequency read pings stay pleasant.
#[cfg(feature = "startup-sound")]
struct SineWave {
    freq: f32,
    sample_rate: u32,
    num_samples: usize,
    elapsed: usize,
}

#[cfg(feature = "startup-sound")]
impl SineWave {
    fn new(freq: f32, duration_ms: u64) -> Self {
        const SR: u32 = 44_100;
        let num_samples = (SR as f64 * duration_ms as f64 / 1_000.0) as usize;
        Self {
            freq,
            sample_rate: SR,
            num_samples,
            elapsed: 0,
        }
    }
}

#[cfg(feature = "startup-sound")]
impl Iterator for SineWave {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.elapsed >= self.num_samples {
            return None;
        }
        let t = self.elapsed as f32 / self.sample_rate as f32;
        let sample = (self.freq * t * 2.0 * std::f32::consts::PI).sin() * 0.2;
        self.elapsed += 1;
        Some(sample)
    }
}

#[cfg(feature = "startup-sound")]
impl rodio::Source for SineWave {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(1).unwrap()
    }
    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate).unwrap()
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_millis(
            self.num_samples as u64 * 1_000 / self.sample_rate as u64,
        ))
    }
}

#[cfg(feature = "startup-sound")]
#[derive(Default)]
pub(crate) struct ThoughtSoundScheduler {
    next_available_ms: u128,
}

#[cfg(feature = "startup-sound")]
impl ThoughtSoundScheduler {
    pub(crate) fn reserve_delay_ms(&mut self, now_ms: u128, playback_ms: u64) -> u64 {
        let playback_ms = u128::from(playback_ms);
        let start_ms = self.next_available_ms.max(now_ms);
        self.next_available_ms = start_ms + playback_ms + u128::from(THOUGHT_SOUND_GAP_MS);
        start_ms.saturating_sub(now_ms) as u64
    }
}

#[cfg(feature = "startup-sound")]
fn thought_sound_total_duration_ms(notes: &[(f32, u64)]) -> u64 {
    notes.iter().map(|&(_, ms)| ms).sum()
}

#[cfg(feature = "startup-sound")]
fn reserve_thought_sound_delay_ms(playback_ms: u64) -> u64 {
    static THOUGHT_SOUND_SCHEDULER: OnceLock<Mutex<ThoughtSoundScheduler>> = OnceLock::new();
    static THOUGHT_SOUND_EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

    let scheduler =
        THOUGHT_SOUND_SCHEDULER.get_or_init(|| Mutex::new(ThoughtSoundScheduler::default()));
    let now_ms = THOUGHT_SOUND_EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis();
    let mut scheduler = scheduler.lock().expect("thought sound scheduler poisoned");
    scheduler.reserve_delay_ms(now_ms, playback_ms)
}

/// Plays the "men-tis-D-B" startup jingle.
///
/// The four notes map directly to the name:
/// - **C5** (523 Hz) — "men"
/// - **E5** (659 Hz) — "tis"
/// - **D5** (587 Hz) — "D"  ← actual note name
/// - **B5** (988 Hz) — "B"  ← actual note name, high octave
///
/// Called **after** the banner has been flushed to stdout.
/// Silenced by setting `MENTISDB_STARTUP_SOUND=0` (or `false`/`no`/`off`).
#[cfg(feature = "startup-sound")]
#[allow(dead_code)]
fn play_startup_jingle() {
    let enabled = std::env::var("MENTISDB_STARTUP_SOUND")
        .map(|v| !matches!(v.to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true);
    if !enabled {
        return;
    }
    // men   tis    D      B
    let notes: &[(f32, u64)] = &[(523.25, 160), (659.25, 160), (587.33, 160), (987.77, 380)];
    play_notes(notes);
}

// ── Per-thought-type sounds ───────────────────────────────────────────────────

/// Returns the note sequence `(freq_hz, duration_ms)` for a given [`ThoughtType`].
///
/// Every sequence totals ≤ 200 ms so the sound never disrupts the workflow.
/// Sequences are designed to *feel* like the thought type:
/// - Rising tones → discovery, insight, completion.
/// - Falling tones → mistakes, handoffs, settling.
/// - Rapid ascending arpeggio → **Surprise** (Metal Gear Solid "!" alert).
/// - Palindromic patterns → **PatternDetected**.
#[cfg(feature = "startup-sound")]
fn thought_sound_sequence(tt: ThoughtType) -> &'static [(f32, u64)] {
    // Note reference (Hz):
    // C4=261  D4=293  E4=329  F4=349  G4=392  A4=440  B4=493
    // C5=523  D5=587  E5=659  F5=698  G5=783  A5=880  B5=987  C6=1046
    match tt {
        // ── Surprise: MGS "!" rapid ascending arpeggio ────────────────────────
        ThoughtType::Surprise => &[(523.25, 35), (659.25, 35), (783.99, 35), (1046.50, 95)],

        // ── Mistakes & corrections ────────────────────────────────────────────
        ThoughtType::Mistake => &[(783.99, 80), (523.25, 100)], // high → low, oops
        ThoughtType::Correction => &[(293.66, 50), (523.25, 50), (659.25, 80)], // resolve upward
        ThoughtType::AssumptionInvalidated => &[(783.99, 80), (523.25, 60)], // deflate

        // ── Discovery & learning ──────────────────────────────────────────────
        ThoughtType::Insight => &[(659.25, 80), (987.77, 100)], // bright jump
        ThoughtType::Idea => &[(523.25, 40), (659.25, 40), (987.77, 100)], // lightbulb
        ThoughtType::FactLearned => &[(587.33, 80), (783.99, 100)], // fact stored
        ThoughtType::LessonLearned => &[(659.25, 80), (783.99, 100)], // wisdom rise
        ThoughtType::Finding => &[(698.46, 80), (880.00, 100)], // discovery

        // ── Questions & exploration ───────────────────────────────────────────
        ThoughtType::Question => &[(783.99, 90), (880.00, 90)], // unresolved rise
        ThoughtType::Wonder => &[(523.25, 55), (587.33, 55), (659.25, 70)], // dreamy ascent
        ThoughtType::Hypothesis => &[(587.33, 90), (493.88, 90)], // tentative descent
        ThoughtType::Experiment => &[(440.00, 60), (523.25, 60), (440.00, 60)], // exploratory bounce

        // ── Patterns ──────────────────────────────────────────────────────────
        ThoughtType::PatternDetected => &[(523.25, 60), (659.25, 60), (523.25, 60)], // palindrome = pattern

        // ── Planning & decisions ──────────────────────────────────────────────
        ThoughtType::Plan => &[(523.25, 70), (783.99, 110)], // perfect fifth, stable
        ThoughtType::Subgoal => &[(329.63, 70), (392.00, 100)], // small step up
        ThoughtType::Decision => &[(392.00, 70), (523.25, 110)], // conclusive arrival
        ThoughtType::StrategyShift => &[(523.25, 55), (698.46, 55), (523.25, 70)], // pivot

        // ── Action & completion ───────────────────────────────────────────────
        ThoughtType::ActionTaken => &[(392.00, 70), (523.25, 100)], // purposeful
        ThoughtType::TaskComplete => &[(523.25, 55), (659.25, 55), (783.99, 70)], // C major arpeggio up
        ThoughtType::Checkpoint => &[(523.25, 80), (659.25, 100)],                // clean save

        // ── State & archive ───────────────────────────────────────────────────
        ThoughtType::StateSnapshot => &[(329.63, 70), (261.63, 100)], // camera settle
        ThoughtType::Handoff => &[(392.00, 55), (329.63, 55), (261.63, 70)], // descending pass
        ThoughtType::Summary => &[(523.25, 80), (392.00, 100)],       // gentle close
        ThoughtType::Reframe => &[(659.25, 60), (587.33, 60), (493.88, 60)], // E5→D5→B4 gentle recontextualisation

        // ── User & relationship ───────────────────────────────────────────────
        ThoughtType::PreferenceUpdate => &[(587.33, 80), (698.46, 100)], // soft note
        ThoughtType::UserTrait => &[(659.25, 80), (880.00, 100)],        // observation noted
        ThoughtType::RelationshipUpdate => &[(698.46, 55), (880.00, 55), (698.46, 70)], // warm embrace

        // ── Constraints ───────────────────────────────────────────────────────
        ThoughtType::Constraint => &[(349.23, 80), (293.66, 100)], // grounding descent
        ThoughtType::Goal => &[(440.00, 80), (554.37, 80), (659.25, 120)], // ascending triad — forward-looking

        // ── LLM Extracted ─────────────────────────────────────────────────────
        ThoughtType::LLMExtracted => &[(523.25, 50), (659.25, 50), (783.99, 70)], // synthesized chime
    }
}

/// Plays a sequence of square-wave notes.
#[cfg(feature = "startup-sound")]
fn play_notes(notes: &[(f32, u64)]) {
    if let Ok(mut device_sink) = rodio::DeviceSinkBuilder::open_default_sink() {
        device_sink.log_on_drop(false);
        let sink = rodio::Player::connect_new(device_sink.mixer());
        for &(freq, ms) in notes {
            sink.append(SquareWave::new(freq, ms));
        }
        sink.sleep_until_end();
    }
}

/// Plays the sound associated with a [`ThoughtType`].
///
/// Enabled only when the `startup-sound` feature is compiled in **and**
/// `MENTISDB_THOUGHT_SOUNDS` is set to a truthy value (defaults to `false`).
#[cfg(feature = "startup-sound")]
pub fn play_thought_sound(tt: ThoughtType) {
    let notes = thought_sound_sequence(tt);
    let delay_ms = reserve_thought_sound_delay_ms(thought_sound_total_duration_ms(notes));
    if delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    play_notes(notes);
}

// ── Per-read-operation sounds ─────────────────────────────────────────────────

/// Returns the note sequence `(freq_hz, duration_ms)` for a given read operation.
///
/// All read sounds use **sine waves** in the **2 500–4 500 Hz** range — two to
/// three octaves above the square-wave write sounds — so the distinction is
/// immediate and instinctive:
///
/// | Timbre | Frequency range | Feel |
/// |--------|----------------|------|
/// | **Write** (square) | 250–1 000 Hz | heavy, stamping, committing |
/// | **Read** (sine) | 2 500–4 500 Hz | light, scanning, querying |
///
/// Every sequence totals ≤ 150 ms.
#[cfg(feature = "startup-sound")]
fn read_sound_sequence(operation: &str) -> &'static [(f32, u64)] {
    match operation {
        // ── Search family ────────────────────────────────────────────────────
        "search" => &[(3200.0, 70), (3600.0, 80)], // rising scan
        "lexical_search" => &[(3000.0, 60), (3400.0, 70)], // word lookup
        "ranked_search" => &[(3400.0, 60), (4000.0, 80)], // ranking jump
        "federated_search" => &[(3000.0, 50), (3500.0, 50), (4000.0, 70)], // multi-source
        "context_bundles" => &[(2800.0, 60), (3200.0, 60), (3600.0, 70)], // bundling ascent

        // ── Agent registry ───────────────────────────────────────────────────
        "list_chains" => &[(4000.0, 60)], // single bright ping
        "list_agents" => &[(3800.0, 60)], // bright ping
        "get_agent" => &[(4200.0, 60)],   // specific retrieval
        "list_agent_registry" => &[(3900.0, 60)],
        "list_entity_types" => &[(3700.0, 60)],

        // ── Thought traversal ────────────────────────────────────────────────
        "recent_context" => &[(3500.0, 70), (3000.0, 50)], // context dip
        "memory_markdown" => &[(3300.0, 70), (3700.0, 70)], // export tone
        "get_thought" => &[(4100.0, 60)],                  // sharp fetch
        "get_genesis_thought" => &[(4300.0, 60)],          // origin ping
        "traverse_thoughts" => &[(3100.0, 60), (3400.0, 60), (3700.0, 70)], // walking through
        "head" => &[(4000.0, 60)],                         // tip ping

        // ── Skills ───────────────────────────────────────────────────────────
        "skill_md" => &[(3400.0, 60)],
        "list_skills" => &[(3200.0, 60)],
        "skill_manifest" => &[(3000.0, 60)],
        "search_skill" => &[(3500.0, 70), (3800.0, 70)],
        "read_skill" => &[(3600.0, 70), (4000.0, 70)],
        "skill_versions" => &[(3800.0, 60), (3400.0, 50)], // version bounce

        // ── Webhooks & misc ──────────────────────────────────────────────────
        "list_webhooks" => &[(2900.0, 60)], // lowest read, listener

        // ── Fallback ─────────────────────────────────────────────────────────
        _ => &[(3600.0, 60)], // generic read ping
    }
}

/// Plays a sequence of sine-wave notes.
#[cfg(feature = "startup-sound")]
fn play_read_notes(notes: &[(f32, u64)]) {
    if let Ok(mut device_sink) = rodio::DeviceSinkBuilder::open_default_sink() {
        device_sink.log_on_drop(false);
        let sink = rodio::Player::connect_new(device_sink.mixer());
        for &(freq, ms) in notes {
            sink.append(SineWave::new(freq, ms));
        }
        sink.sleep_until_end();
    }
}

/// Plays the sound associated with a read operation.
///
/// Enabled only when the `startup-sound` feature is compiled in **and**
/// `MENTISDB_THOUGHT_SOUNDS` is set to a truthy value.
#[cfg(feature = "startup-sound")]
pub fn play_read_sound(operation: &str) {
    let notes = read_sound_sequence(operation);
    play_read_notes(notes);
}

fn env_var_truthy(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

pub(crate) fn update_config_from_env() -> UpdateConfig {
    UpdateConfig {
        enabled: env_var_truthy("MENTISDB_UPDATE_CHECK", true),
        repo: std::env::var("MENTISDB_UPDATE_REPO")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_UPDATE_REPO.to_string()),
    }
}

pub(crate) fn release_core_version(input: &str) -> Option<[u64; 3]> {
    let normalized = input.trim().trim_start_matches(['v', 'V']);
    let mut components = normalized.split('.');
    let mut parsed = [0_u64; 3];
    for slot in &mut parsed {
        let component = components.next()?;
        let digits: String = component
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            return None;
        }
        *slot = digits.parse().ok()?;
    }
    Some(parsed)
}

pub(crate) fn release_tag_is_newer(latest_tag: &str, current_version: &str) -> bool {
    let Some(latest) = release_core_version(latest_tag) else {
        return false;
    };
    let Some(current) = release_core_version(current_version) else {
        return false;
    };
    latest > current
}

fn normalize_release_tag_display(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).to_string()
}

pub(crate) fn build_ascii_notice_box(title: &str, lines: &[String]) -> String {
    let width = std::iter::once(title.len())
        .chain(lines.iter().map(|line| line.len()))
        .max()
        .unwrap_or(0);
    let border = format!("+{}+", "-".repeat(width + 2));
    let mut output = String::new();
    output.push('\n');
    output.push_str(&format!("{YELLOW}{border}{RESET}\n"));
    output.push_str(&format!("| {:<width$} |\n", title, width = width));
    output.push_str(&format!("{YELLOW}{border}{RESET}\n"));
    for line in lines {
        output.push_str(&format!("| {:<width$} |\n", line, width = width));
    }
    output.push_str(&format!("{YELLOW}{border}{RESET}\n"));
    output
}

fn ascii_notice_box(title: &str, lines: &[String]) {
    print!("{}", build_ascii_notice_box(title, lines));
    let _ = io::stdout().flush();
}

pub(crate) fn build_update_available_lines(
    current_version: &str,
    latest_display: &str,
    release_url: &str,
) -> Vec<String> {
    vec![
        format!("Current core version: {current_version}"),
        format!("Latest release tag : {latest_display}"),
        format!("Release page       : {release_url}"),
        String::new(),
        format!("Install release {latest_display} and restart now? [y/N]"),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FirstRunSetupStatus {
    pub(crate) interactive_terminal: bool,
    pub(crate) has_registered_chains: bool,
    pub(crate) has_configured_integrations: bool,
}

pub(crate) fn should_show_first_run_setup_notice(status: &FirstRunSetupStatus) -> bool {
    status.interactive_terminal
        && !status.has_registered_chains
        && !status.has_configured_integrations
}

pub(crate) fn build_first_run_setup_lines() -> Vec<String> {
    vec![
        "No configured MentisDB client integrations were detected.".to_string(),
        "Run mentisdbd wizard to detect installed tools and configure them.".to_string(),
        "Or preview everything with: mentisdbd setup all --dry-run".to_string(),
        "Then apply one target with: mentisdbd setup <agent>".to_string(),
        String::new(),
        "Supported agents: codex, claude-code, claude-desktop, gemini,".to_string(),
        "opencode, qwen, copilot, vscode-copilot.".to_string(),
    ]
}

/// Builds the single ready-to-paste primer line shown at daemon startup.
///
/// The line is printed outside any box so it can be triple-click selected cleanly.
pub(crate) fn build_agent_primer_paste_line(_mcp_addr: &str, _has_chains: bool) -> String {
    "prime yourself for optimal mentisdb usage, call mentisdb_skill_md and update your local mentisdb skill".to_string()
}

fn detect_first_run_setup_status(chain_dir: &Path) -> FirstRunSetupStatus {
    let has_registered_chains = load_registered_chains(chain_dir)
        .map(|registry| !registry.chains.is_empty())
        .unwrap_or(false);
    let detection =
        detect_integrations_with_environment(HostPlatform::current(), PathEnvironment::capture());
    let has_configured_integrations = detection
        .integrations
        .iter()
        .any(|entry| entry.status == DetectionStatus::Configured);

    FirstRunSetupStatus {
        interactive_terminal: io::stdin().is_terminal() && io::stdout().is_terminal(),
        has_registered_chains,
        has_configured_integrations,
    }
}

pub(crate) fn maybe_run_first_run_setup_with_io(
    status: &FirstRunSetupStatus,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
    runner: impl FnOnce(&mut dyn BufRead, &mut dyn Write, &mut dyn Write) -> ExitCode,
) -> io::Result<bool> {
    if !should_show_first_run_setup_notice(status) {
        return Ok(false);
    }

    write!(
        out,
        "{}",
        build_ascii_notice_box("mentisdbd first-run setup", &build_first_run_setup_lines())
    )?;
    let response = mentisdb::cli::boxed_yn_prompt(
        out,
        "Run the MentisDB setup wizard now while the daemon is already running?",
        true,
        input,
    )?;
    if response.eq_ignore_ascii_case("n") || response.eq_ignore_ascii_case("no") {
        return Ok(false);
    }

    writeln!(out)?;
    let code = runner(input, out, err);
    if code != ExitCode::SUCCESS {
        writeln!(err, "Startup setup wizard exited with status {code:?}.")?;
    }
    Ok(true)
}

#[allow(dead_code)]
fn maybe_run_first_run_setup(status: &FirstRunSetupStatus) -> io::Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut input = stdin.lock();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    maybe_run_first_run_setup_with_io(status, &mut input, &mut out, &mut err, |input, out, err| {
        run_cli_subcommand_with_io(
            vec![OsString::from("mentisdbd"), OsString::from("wizard")],
            input,
            out,
            err,
        )
    })
}

pub(crate) fn prompt_yes_no_with_io(
    prompt: &str,
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
) -> io::Result<bool> {
    let mut input = String::new();
    loop {
        write!(writer, "{prompt} [y/N]: ")?;
        writer.flush()?;
        input.clear();
        reader.read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" | "" => return Ok(false),
            _ => writeln!(writer, "Please type Y or N.")?,
        }
    }
}

fn prompt_yes_no(prompt: &str) -> io::Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    prompt_yes_no_with_io(prompt, &mut reader, &mut writer)
}

pub(crate) fn build_cargo_install_args(tag: &str, repo: &str) -> Vec<OsString> {
    vec![
        OsString::from("install"),
        OsString::from("--git"),
        OsString::from(format!("https://github.com/{repo}")),
        OsString::from("--tag"),
        OsString::from(tag),
        OsString::from("--locked"),
        OsString::from("--force"),
        OsString::from("--bin"),
        OsString::from(UPDATE_BINARY_NAME),
        OsString::from(UPDATE_CRATE_NAME),
    ]
}

fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn cargo_bin_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return Some(path.join("bin"));
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .map(|home| home.join(".cargo").join("bin"))
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
                .map(|home| home.join(".cargo").join("bin"))
        })
}

fn installed_binary_path() -> Option<PathBuf> {
    let binary_name = if cfg!(windows) {
        format!("{UPDATE_BINARY_NAME}.exe")
    } else {
        UPDATE_BINARY_NAME.to_string()
    };
    cargo_bin_dir().map(|dir| dir.join(binary_name))
}

fn install_latest_release(
    tag: &str,
    repo: &str,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let cargo = cargo_program();
    let version_status = Command::new(&cargo).arg("--version").status()?;
    if !version_status.success() {
        return Err(format!(
            "cargo executable '{}' is not available",
            Path::new(&cargo).display()
        )
        .into());
    }

    let status = Command::new(&cargo)
        .args(build_cargo_install_args(tag, repo))
        .status()?;
    if !status.success() {
        return Err(format!("cargo install failed with status {status}").into());
    }

    // Prefer the standard cargo bin dir. If it doesn't exist there, search
    // PATH so non-standard CARGO_INSTALL_ROOT / CARGO_HOME layouts work too.
    // Never fall back to current_exe() — that would silently re-exec the old
    // binary and make the update appear to do nothing.
    if let Some(path) = installed_binary_path().filter(|p| p.exists()) {
        return Ok(path);
    }
    if let Some(path) = search_path_for_binary(UPDATE_BINARY_NAME) {
        return Ok(path);
    }
    Err(format!(
        "cargo install completed but '{UPDATE_BINARY_NAME}' was not found in \
         the cargo bin directory or PATH; check CARGO_HOME or CARGO_INSTALL_ROOT"
    )
    .into())
}

/// Searches each directory in `PATH` for an executable named `name`.
fn search_path_for_binary(name: &str) -> Option<PathBuf> {
    let binary_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(&binary_name))
        .find(|p| p.exists())
}

async fn fetch_latest_release(
    repo: &str,
) -> Result<UpdateRelease, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .user_agent(format!(
            "mentisdbd/{} update-check",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;
    let response = client
        .get(format!("{GITHUB_API_BASE}/repos/{repo}/releases/latest"))
        .send()
        .await?
        .error_for_status()?
        .json::<GitHubReleaseResponse>()
        .await?;
    Ok(UpdateRelease {
        tag_name: response.tag_name,
        html_url: response.html_url,
    })
}

/// Check if a daemon is already responding on the given MCP address.
fn is_daemon_running(mcp_addr: &str) -> bool {
    let url = format!("http://{mcp_addr}/health");
    ureq::get(&url)
        .timeout(std::time::Duration::from_millis(500))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

/// Launch the daemon in the background, detached from the current process.
/// Returns immediately after the process starts.
fn launch_daemon() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not resolve own executable path: {e}"))?;

    #[cfg(unix)]
    {
        // On Unix, use nohup + double-fork pattern via setsid
        let status = Command::new("nohup")
            .arg(&exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn daemon: {e}"))?;
        // Detach: the child will outlive us due to nohup
        drop(status);
    }

    #[cfg(windows)]
    {
        // On Windows, use START /B to run detached
        let exe_str = exe.to_string_lossy();
        let status = Command::new("cmd")
            .args(["/C", "start", "/B", &exe_str])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn daemon: {e}"))?;
        drop(status);
    }

    Ok(())
}

/// Wait for the daemon to become responsive on the health endpoint.
fn wait_for_daemon(mcp_addr: &str, max_attempts: u32) -> bool {
    for _ in 0..max_attempts {
        if is_daemon_running(mcp_addr) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

/// Forward a single JSON-RPC request to the daemon's streamable HTTP MCP
/// endpoint (`POST /`) and return the response.
///
/// Transparent passthrough — no manual method mapping. The daemon's
/// streamable HTTP endpoint handles the full MCP protocol (initialize,
/// ping, tools/list, tools/call, resources/list, resources/read, prompts,
/// sampling, roots, notifications).
async fn proxy_jsonrpc_to_daemon(mcp_addr: &str, request: &str) -> Option<String> {
    let url = format!("http://{mcp_addr}/");

    // Parse to extract the method for local-only optimizations
    let parsed: serde_json::Value = serde_json::from_str(request).ok()?;

    // Handle resources/read locally — the skill markdown is embedded in
    // the proxy binary, so we can serve it without hitting the daemon.
    let method = parsed.get("method")?.as_str()?;
    if method == "resources/read" {
        let id = parsed.get("id").cloned()?;
        let params = parsed
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
        return Some(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "contents": [{
                        "uri": uri,
                        "text": MENTISDB_SKILL_MD,
                        "mimeType": "text/markdown"
                    }]
                }
            })
            .to_string(),
        );
    }

    // Forward all other JSON-RPC requests to the daemon's POST / endpoint
    let request_bytes = request.as_bytes().to_vec();
    #[allow(clippy::result_large_err)]
    let resp = tokio::task::spawn_blocking(move || {
        ureq::post(&url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .send(&*request_bytes)
    })
    .await
    .ok()?;

    let resp = resp.ok()?;
    let resp_body: String = resp.into_string().ok()?;

    // The streamable HTTP endpoint may return:
    // 1. A bare JSON-RPC response (most common for single-request methods)
    // 2. An SSE event stream (for methods that produce server→client notifications)
    //
    // For SSE responses, extract the first JSON-RPC data event.
    if resp_body.starts_with("event:") || resp_body.contains("event:") {
        // Parse SSE: look for the first "data:" line containing JSON
        for line in resp_body.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data: ") {
                if data.starts_with('{') {
                    return Some(data.to_string());
                }
            }
        }
        return None;
    }

    // Bare JSON response — pass through as-is (already a JSON-RPC response)
    Some(resp_body)
}

/// Run stdio mode with smart daemon detection.
///
/// If a daemon is already running on the configured MCP port, this process acts
/// as a lightweight stdio-to-HTTP proxy, forwarding JSON-RPC requests to the
/// daemon's HTTP MCP endpoint. This avoids duplicate in-memory state and ensures
/// all clients share the same live chain cache.
///
/// If no daemon is running, one is launched in the background before proxying.
async fn run_stdio_mode(
    config: MentisDbServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_logger();
    let mcp_addr = config.mcp_addr.to_string();

    // Step 1: Check if daemon is already running
    if is_daemon_running(&mcp_addr) {
        eprintln!("[mentisdbd stdio] Daemon detected at {mcp_addr}, proxying to live instance.");
    } else {
        // Step 2: Launch daemon in background
        eprintln!("[mentisdbd stdio] No daemon running, launching background daemon...");
        launch_daemon().map_err(|e| format!("failed to launch daemon: {e}"))?;

        // Step 3: Wait for daemon to become responsive
        if wait_for_daemon(&mcp_addr, 50) {
            eprintln!("[mentisdbd stdio] Daemon started at {mcp_addr}, proxying.");
        } else {
            eprintln!("[mentisdbd stdio] Daemon did not become responsive at {mcp_addr}, falling back to local mode.");
            run_stdio_mode_local(config).await?;
            return Ok(());
        }
    }

    // Step 4: Proxy stdin/stdout to daemon's HTTP MCP
    run_stdio_mode_proxy(&mcp_addr).await
}

/// Local stdio mode — creates its own MentisDbService (fallback when daemon launch fails).
async fn run_stdio_mode_local(
    config: MentisDbServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = Arc::new(MentisDbService::new(config.service.clone()));
    let protocol = MentisDbMcpProtocol::new(service);

    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(resp_json) = handle_stdio_jsonrpc(line, &protocol).await {
            stdout.write_all(resp_json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Proxy stdio mode — transparently forwards JSON-RPC to the daemon's
/// streamable HTTP MCP endpoint (`POST /`), acting as a drop-in replacement
/// for `mcp-remote` but with built-in daemon auto-launch.
///
/// Handles the full MCP protocol (tools, resources, prompts, sampling,
/// roots, notifications) without manual method mapping.
async fn run_stdio_mode_proxy(
    mcp_addr: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mcp_addr = mcp_addr.to_string();
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(resp_json) = proxy_jsonrpc_to_daemon(&mcp_addr, line).await {
            stdout.write_all(resp_json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

async fn handle_stdio_jsonrpc(line: &str, protocol: &MentisDbMcpProtocol) -> Option<String> {
    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return None,
    };

    let jsonrpc = request.get("jsonrpc")?.as_str()?;
    if jsonrpc != "2.0" {
        return None;
    }

    let id = request.get("id").cloned()?;
    let method = request.get("method")?.as_str()?;
    let params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let result: Result<serde_json::Value, String> = match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false}
            },
            "serverInfo": {
                "name": "mentisdb",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "MentisDB is an append-only semantic memory server. READ THIS FIRST: call `resources/read` for `mentisdb://skill/core` immediately after initialize to load the embedded MentisDB operating skill."
        })),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => match protocol.list_tools().await {
            Ok(tools) => Ok(serde_json::json!({
                "tools": tools.into_iter().map(|t| {
                    let def = t.to_tool_definition();
                    serde_json::json!({
                        "name": def.name,
                        "description": def.description,
                        "inputSchema": def.parameters_schema
                    })
                }).collect::<Vec<_>>()
            })),
            Err(e) => Err(e.to_string()),
        },
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let canonical = canonical_tool_name(name);
            match protocol.execute(&canonical, arguments).await {
                Ok(result) => Ok(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": if result.output.is_string() {
                            result.output.as_str().unwrap_or_default().to_string()
                        } else {
                            serde_json::to_string(&result.output).unwrap_or_default()
                        }
                    }],
                    "isError": !result.success
                })),
                Err(e) => Err(e.to_string()),
            }
        }
        "resources/list" => match protocol.list_resources().await {
            Ok(resources) => Ok(serde_json::json!({"resources": resources})),
            Err(e) => Err(e.to_string()),
        },
        "resources/read" => {
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            match protocol.read_resource(uri).await {
                Ok(content) => Ok(serde_json::json!({"contents": [{"uri": uri, "text": content}]})),
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err(format!("Method not found: {method}")),
    };

    Some(match result {
        Ok(r) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": r
        })
        .to_string(),
        Err(e) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32603,
                "message": e
            }
        })
        .to_string(),
    })
}

fn canonical_tool_name(name: &str) -> String {
    let name = name.trim();
    if name.starts_with("mentisdb_") {
        return name.to_string();
    }
    format!("mentisdb_{}", name)
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // When the server feature is enabled, axum-server (tls-rustls) and reqwest
    // both pull in rustls.  Depending on the feature combination, both the
    // aws-lc-rs and ring crypto backends may be compiled in.  Rustls refuses to
    // start if multiple providers are available without an explicit default.
    // Install ring — the provider used by the rest of the TLS stack — before
    // any TLS operations.
    #[cfg(feature = "server")]
    let _ = rustls::crypto::ring::default_provider().install_default();
    raise_fd_limit();
    let log_rx = tui::init_tui_logger();

    // Build initial TUI state immediately so the TUI renders right away.
    let mut tui_state = TuiState::new(env!("CARGO_PKG_VERSION"));
    tui_state.startup_status = "Starting…".to_string();
    tui_state.log_lines.push(format!(
        "[{}] mentisdb v{} starting",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
        env!("CARGO_PKG_VERSION")
    ));

    let tui_state = Arc::new(std::sync::Mutex::new(tui_state));
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Spawn Ctrl+C handler
    let ctrlc_handle = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running_clone.store(false, Ordering::SeqCst);
    });

    // Start the TUI on a background thread so it renders immediately.
    let tui_state_for_tui = Arc::clone(&tui_state);
    let running_for_tui = Arc::clone(&running);
    let tui_handle =
        std::thread::spawn(move || tui::run_tui(tui_state_for_tui, running_for_tui, log_rx));

    // All startup work runs here on the main thread. Progress is fed
    // into the TUI via the log channel and startup_status field.
    let startup_result: Result<StartupData, _> = run_startup_sequence(&tui_state).await;

    if let Err(e) = startup_result {
        // Show crash overlay in TUI so the user can read the error + logs.
        {
            let mut s = tui_state.lock().unwrap();
            s.startup_error = Some(e.to_string());
        }
        if let Err(te) = tui_handle.join().unwrap() {
            eprintln!("TUI error: {te}");
        }
        ctrlc_handle.abort();
        return Err(e);
    }
    let startup_result = startup_result.unwrap();

    let (
        config,
        update_config,
        handles,
        is_first_run,
        _first_run_setup_status,
        primer_paste_line,
        migration_reports,
        skill_registry_msg,
        storage_root_migration,
    ) = startup_result;

    // ── Update TUI state with completed startup data ────────────────────────
    {
        let mut s = tui_state.lock().unwrap();
        s.started = true;
        s.startup_status = "mentisdbd running".to_string();
        s.chain_count = 0;
        s.primer_text = primer_paste_line.clone();
        s.config_lines = build_config_lines(&config, &update_config);

        if migration_reports.is_empty() {
            s.migration_lines
                .push("No chain migrations required.".to_string());
        } else {
            for report in &migration_reports {
                s.migration_lines.push(format!(
                    "Migrated chain {} from version {} to {} ({} thoughts)",
                    report.chain_key, report.from_version, report.to_version, report.thought_count
                ));
            }
        }
        s.migration_lines.push(skill_registry_msg.clone());

        if let Some(report) = &storage_root_migration {
            if report.renamed_root_dir {
                s.migration_lines.push(format!(
                    "Legacy storage adoption: renamed {} -> {}",
                    report.source_dir.display(),
                    report.target_dir.display()
                ));
            } else if report.merged_entries > 0 {
                s.migration_lines.push(format!(
                    "Legacy storage adoption: merged {} entries from {} into {}",
                    report.merged_entries,
                    report.source_dir.display(),
                    report.target_dir.display()
                ));
            }
            if report.renamed_registry_file {
                s.migration_lines.push(
                    "Renamed thoughtchain-registry.json -> mentisdb-registry.json".to_string(),
                );
            }
        }

        s.tls_info_lines = if handles.https_mcp.is_some() || handles.https_rest.is_some() {
            build_tls_info_lines(&config, &handles)
        } else {
            Vec::new()
        };

        let mcp_port = handles.mcp.local_addr().port();
        let rest_port = handles.rest.local_addr().port();
        s.endpoint_lines = build_endpoint_lines(&handles, mcp_port, rest_port);

        let registry = load_registered_chains(&config.service.chain_dir).unwrap_or_default();
        s.chain_count = registry.chains.len();

        for (key, reg) in &registry.chains {
            s.chains.push(ChainInfo {
                key: key.clone(),
                version: reg.version,
                adapter: reg.storage_adapter.to_string(),
                thoughts: reg.thought_count as usize,
                agents: reg.agent_count,
                storage_path: reg.storage_location.clone(),
            });
        }

        for (chain_key, reg) in &registry.chains {
            if let Ok(chain) = MentisDb::open_with_storage(
                reg.storage_adapter
                    .for_chain_key(&config.service.chain_dir, chain_key),
            ) {
                let agents = chain.list_agent_registry();
                let thoughts = chain.thoughts();
                let mut counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for t in thoughts {
                    *counts.entry(t.agent_id.as_str()).or_default() += 1;
                }
                for agent in &agents {
                    let live_count = counts.get(agent.agent_id.as_str()).copied().unwrap_or(0);
                    s.agents.push(AgentInfo {
                        chain_key: chain_key.clone(),
                        name: agent.display_name.clone(),
                        id: agent.agent_id.clone(),
                        status: agent.status.to_string(),
                        memories: live_count,
                        description: agent
                            .description
                            .as_deref()
                            .filter(|v| !v.trim().is_empty())
                            .unwrap_or("—")
                            .to_string(),
                    });
                }
            }
        }

        if let Ok(skill_registry) = SkillRegistry::open(&config.service.chain_dir) {
            let skills = skill_registry.list_skills();
            for summary in &skills {
                let uploaded_by = summary
                    .latest_uploaded_by_agent_name
                    .as_deref()
                    .unwrap_or(&summary.latest_uploaded_by_agent_id);
                s.skills.push(SkillInfo {
                    name: summary.name.clone(),
                    status: summary.status.to_string(),
                    versions: summary.version_count,
                    tags: summary.tags.clone(),
                    uploaded_by: uploaded_by.to_string(),
                });
            }
        }

        let mcp_local = format!("http://{}", handles.mcp.local_addr());
        let rest_local = format!("http://{}", handles.rest.local_addr());
        s.log_lines.push(format!(
            "[{}] MCP  (HTTP)  {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            mcp_local
        ));
        s.log_lines.push(format!(
            "[{}] REST (HTTP)  {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            rest_local
        ));
        if let Some(ref h) = handles.https_mcp {
            s.log_lines.push(format!(
                "[{}] MCP  (TLS)   https://{}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
        if let Some(ref h) = handles.https_rest {
            s.log_lines.push(format!(
                "[{}] REST (TLS)   https://{}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
        if let Some(ref h) = handles.dashboard {
            s.log_lines.push(format!(
                "[{}] Dashboard    https://{}/dashboard",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
    }

    // TUI stays running — no restart, no terminal flash.
    // If it is the user's first run, show setup instructions in the TUI
    // rather than an interactive wizard (which cannot run while the TUI is
    // active since the terminal is in raw/alternate-screen mode).
    if is_first_run {
        let mut s = tui_state.lock().unwrap();
        s.migration_lines.push(String::new());
        s.migration_lines
            .push("First-run setup: run `mentisdbd wizard` in a separate terminal.".to_string());
        for line in build_first_run_setup_lines() {
            s.migration_lines.push(format!("  {line}"));
        }
    }

    // Wait for the TUI thread (the user presses q to exit).
    if let Err(te) = tui_handle.join().unwrap() {
        eprintln!("TUI error: {te}");
    }
    ctrlc_handle.abort();

    Ok(())
}

/// Runs all startup work (update check, migrations, server start) and feeds
/// progress into the TUI. Returns all data needed to populate the TUI state
/// once startup completes.
async fn run_startup_sequence(
    tui_state: &Arc<std::sync::Mutex<TuiState>>,
) -> Result<StartupData, Box<dyn std::error::Error + Send + Sync>> {
    let storage_root_migration = if std::env::var_os("MENTISDB_DIR").is_none() {
        adopt_legacy_default_mentisdb_dir()?
    } else {
        None
    };
    let mut config = MentisDbServerConfig::from_env();

    // Update check
    {
        let mut s = tui_state.lock().unwrap();
        s.startup_status = "Checking for updates…".to_string();
    }

    let update_config = update_config_from_env();
    if update_config.enabled {
        let latest = fetch_latest_release(&update_config.repo).await;
        match latest {
            Ok(ref release)
                if release_tag_is_newer(&release.tag_name, env!("CARGO_PKG_VERSION")) =>
            {
                let latest_display = normalize_release_tag_display(&release.tag_name);
                let current_version = env!("CARGO_PKG_VERSION");

                if io::stdin().is_terminal() && io::stdout().is_terminal() {
                    let choice = tui::TuiState::request_update_dialog(
                        tui_state,
                        current_version,
                        &latest_display,
                        &release.html_url,
                    );
                    match choice {
                        Some(true) => {
                            {
                                let mut s = tui_state.lock().unwrap();
                                s.startup_status =
                                    format!("Installing {latest_display}…").to_string();
                            }
                            match install_latest_release(&release.tag_name, &update_config.repo) {
                                Ok(path) => {
                                    eprintln!("Installed mentisdbd to {}", path.display());
                                    eprintln!("Please restart mentisdbd to use the new version.");
                                    return Err("Update installed, please restart.".into());
                                }
                                Err(e) => {
                                    eprintln!("Install failed: {e}");
                                    eprintln!("Continuing with current version…");
                                }
                            }
                        }
                        Some(false) => {
                            eprintln!("Update skipped. Continuing with current version…");
                        }
                        None => {
                            eprintln!("Update skipped (TUI quit).");
                            return Err("TUI quit during update dialog.".into());
                        }
                    }
                } else {
                    eprintln!("mentisdbd update available: {current_version} -> {latest_display}",);
                    eprintln!(
                        "Non-interactive terminal. Update manually:\n\
                         cargo install --git https://github.com/{} --tag {} --locked --force --bin {UPDATE_BINARY_NAME} {UPDATE_CRATE_NAME}",
                        update_config.repo, release.tag_name
                    );
                    eprintln!("Continuing with current version…");
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("Update check failed: {e}"),
        }
    }

    // Migrations
    {
        let mut s = tui_state.lock().unwrap();
        s.startup_status = "Checking chains for migrations…".to_string();
    }

    let migration_reports = migrate_registered_chains_with_adapter(
        &config.service.chain_dir,
        config.service.default_storage_adapter,
        |event| match event {
            MentisDbMigrationEvent::Started {
                chain_key,
                from_version,
                to_version,
                current,
                total,
            } => {
                let msg = format!(
                    "[{}/{}] Migrating chain {} from v{} to v{}",
                    current, total, chain_key, from_version, to_version
                );
                log::info!("{}", msg);
            }
            MentisDbMigrationEvent::Completed {
                chain_key,
                from_version,
                to_version,
                current,
                total,
            } => {
                let msg = format!(
                    "[{}/{}] Migrated chain {} from v{} to v{}",
                    current, total, chain_key, from_version, to_version
                );
                log::info!("{}", msg);
            }
            MentisDbMigrationEvent::StartedReconciliation {
                chain_key,
                from_storage_adapter,
                to_storage_adapter,
                current,
                total,
            } => {
                let msg = format!(
                    "[{}/{}] Reconciling chain {} from {} to {} storage",
                    current, total, chain_key, from_storage_adapter, to_storage_adapter
                );
                log::info!("{}", msg);
            }
            MentisDbMigrationEvent::CompletedReconciliation {
                chain_key,
                from_storage_adapter,
                to_storage_adapter,
                current,
                total,
            } => {
                let msg = format!(
                    "[{}/{}] Reconciled chain {} from {} to {} storage",
                    current, total, chain_key, from_storage_adapter, to_storage_adapter
                );
                log::info!("{}", msg);
            }
            MentisDbMigrationEvent::StartedHashRehash { .. }
            | MentisDbMigrationEvent::CompletedHashRehash { .. } => {}
        },
    )?;

    match migrate_chain_hash_algorithm(&config.service.chain_dir, |event| match event {
        MentisDbMigrationEvent::StartedHashRehash {
            chain_key,
            current,
            total,
        } => log::info!(
            "[{}/{}] Rehashing chain {} (legacy JSON → bincode)",
            current,
            total,
            chain_key
        ),
        MentisDbMigrationEvent::CompletedHashRehash {
            chain_key,
            current,
            total,
        } => log::info!("[{}/{}] Rehashed chain {}", current, total, chain_key),
        _ => {}
    }) {
        Ok(0) => {}
        Ok(n) => log::info!("Hash algorithm migration complete: {n} chain(s) rehashed."),
        Err(e) => log::warn!("Hash algorithm migration failed: {e}"),
    }

    let skill_registry_msg = match migrate_skill_registry(&config.service.chain_dir) {
        Ok(None) => "Skill registry: up to date, no migration required.".to_string(),
        Ok(Some(report)) => format!(
            "Skill registry migrated: {} skill(s), {} version(s) converted (v{} → v{}) at {}",
            report.skills_migrated,
            report.versions_migrated,
            report.from_version,
            report.to_version,
            report.path.display()
        ),
        Err(e) => panic!("Skill registry migration failed — cannot start server: {e}"),
    };

    if let Err(e) = refresh_registered_chain_counts(&config.service.chain_dir) {
        log::warn!("Could not refresh chain registry counts: {e}");
    }

    #[cfg(feature = "startup-sound")]
    {
        let thought_sounds_enabled = std::env::var("MENTISDB_THOUGHT_SOUNDS")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if thought_sounds_enabled {
            config.service = config
                .service
                .with_on_thought_appended(Arc::new(play_thought_sound))
                .with_on_read_logged(Arc::new(|op: &str| play_read_sound(op)));
        }
    }

    // Start servers
    {
        let mut s = tui_state.lock().unwrap();
        s.startup_status = "Starting servers…".to_string();
    }

    let handles = start_servers(config.clone()).await?;

    let first_run_setup_status = detect_first_run_setup_status(&config.service.chain_dir);
    let is_first_run = should_show_first_run_setup_notice(&first_run_setup_status);

    // Primer
    let registry = load_registered_chains(&config.service.chain_dir).unwrap_or_default();
    let has_chains = !registry.chains.is_empty();
    let primer_mcp_addr = handles
        .https_mcp
        .as_ref()
        .map(|h| format!("https://{}", h.local_addr()))
        .unwrap_or_else(|| format!("http://{}", handles.mcp.local_addr()));
    let primer_paste_line = build_agent_primer_paste_line(&primer_mcp_addr, has_chains);

    {
        let mut s = tui_state.lock().unwrap();
        s.startup_status = "Loading chains and agents…".to_string();
    }

    #[cfg(feature = "startup-sound")]
    play_startup_jingle();

    Ok((
        config,
        update_config,
        handles,
        is_first_run,
        first_run_setup_status,
        primer_paste_line,
        migration_reports,
        skill_registry_msg,
        storage_root_migration,
    ))
}

/// Like `run()` but forces the update dialog to appear even if already at the
/// latest release. Used via `mentisdbd --force-update`.
async fn run_with_force_update() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    raise_fd_limit();
    let log_rx = tui::init_tui_logger();

    let mut tui_state = TuiState::new(env!("CARGO_PKG_VERSION"));
    tui_state.startup_status = "Checking for updates…".to_string();
    tui_state.log_lines.push(format!(
        "[{}] mentisdb v{} starting (--force-update)",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
        env!("CARGO_PKG_VERSION")
    ));

    let tui_state = Arc::new(std::sync::Mutex::new(tui_state));
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    let ctrlc_handle = tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running_clone.store(false, Ordering::SeqCst);
    });

    let tui_state_clone = Arc::clone(&tui_state);
    let running_clone2 = Arc::clone(&running);
    let tui_handle =
        std::thread::spawn(move || tui::run_tui(tui_state_clone, running_clone2, log_rx));

    // Force-update: always fetch latest and show dialog regardless of version.
    let update_config = update_config_from_env();
    let latest = fetch_latest_release(&update_config.repo).await;
    match latest {
        Ok(ref release) => {
            let latest_display = normalize_release_tag_display(&release.tag_name);
            let current_version = env!("CARGO_PKG_VERSION");

            if io::stdin().is_terminal() && io::stdout().is_terminal() {
                let choice = tui::TuiState::request_update_dialog(
                    &tui_state,
                    current_version,
                    &latest_display,
                    &release.html_url,
                );
                match choice {
                    Some(true) => {
                        {
                            let mut s = tui_state.lock().unwrap();
                            s.startup_status = format!("Installing {latest_display}…").to_string();
                        }
                        match install_latest_release(&release.tag_name, &update_config.repo) {
                            Ok(path) => {
                                eprintln!("Installed mentisdbd to {}", path.display());
                                eprintln!("Please restart mentisdbd to use the new version.");
                                return Err("Update installed, please restart.".into());
                            }
                            Err(e) => {
                                eprintln!("Install failed: {e}");
                                eprintln!("Continuing with current version…");
                            }
                        }
                    }
                    Some(false) => {
                        eprintln!("Update skipped. Continuing with current version…");
                    }
                    None => {
                        eprintln!("Update skipped (TUI quit).");
                        return Err("TUI quit during update dialog.".into());
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to fetch latest release: {e}");
            eprintln!("Continuing with current version…");
        }
    }

    // After the update dialog, continue with normal startup.
    let startup_result: Result<StartupData, _> = run_startup_sequence(&tui_state).await;

    if let Err(e) = startup_result {
        {
            let mut s = tui_state.lock().unwrap();
            s.startup_error = Some(e.to_string());
        }
        if let Err(te) = tui_handle.join().unwrap() {
            eprintln!("TUI error: {te}");
        }
        ctrlc_handle.abort();
        return Err(e);
    }
    let startup_result = startup_result.unwrap();

    let (
        config,
        update_config,
        handles,
        is_first_run,
        _first_run_setup_status,
        primer_paste_line,
        migration_reports,
        skill_registry_msg,
        storage_root_migration,
    ) = startup_result;

    {
        let mut s = tui_state.lock().unwrap();
        s.started = true;
        s.startup_status = "mentisdbd running".to_string();
        s.chain_count = 0;
        s.primer_text = primer_paste_line.clone();
        s.config_lines = build_config_lines(&config, &update_config);

        if migration_reports.is_empty() {
            s.migration_lines
                .push("No chain migrations required.".to_string());
        } else {
            for report in &migration_reports {
                s.migration_lines.push(format!(
                    "Migrated chain {} from version {} to {} ({} thoughts)",
                    report.chain_key, report.from_version, report.to_version, report.thought_count
                ));
            }
        }
        s.migration_lines.push(skill_registry_msg.clone());

        if let Some(report) = &storage_root_migration {
            if report.renamed_root_dir {
                s.migration_lines.push(format!(
                    "Legacy storage adoption: renamed {} -> {}",
                    report.source_dir.display(),
                    report.target_dir.display()
                ));
            } else if report.merged_entries > 0 {
                s.migration_lines.push(format!(
                    "Legacy storage adoption: merged {} entries from {} into {}",
                    report.merged_entries,
                    report.source_dir.display(),
                    report.target_dir.display()
                ));
            }
            if report.renamed_registry_file {
                s.migration_lines.push(
                    "Renamed thoughtchain-registry.json -> mentisdb-registry.json".to_string(),
                );
            }
        }

        s.tls_info_lines = if handles.https_mcp.is_some() || handles.https_rest.is_some() {
            build_tls_info_lines(&config, &handles)
        } else {
            Vec::new()
        };

        let mcp_port: u16 = handles.mcp.local_addr().port();
        let rest_port: u16 = handles.rest.local_addr().port();
        s.endpoint_lines = build_endpoint_lines(&handles, mcp_port, rest_port);

        let registry = load_registered_chains(&config.service.chain_dir).unwrap_or_default();
        s.chain_count = registry.chains.len();

        for (key, reg) in &registry.chains {
            s.chains.push(ChainInfo {
                key: key.clone(),
                version: reg.version,
                adapter: reg.storage_adapter.to_string(),
                thoughts: reg.thought_count as usize,
                agents: reg.agent_count,
                storage_path: reg.storage_location.clone(),
            });
        }

        for (chain_key, reg) in &registry.chains {
            if let Ok(chain) = MentisDb::open_with_storage(
                reg.storage_adapter
                    .for_chain_key(&config.service.chain_dir, chain_key),
            ) {
                let agents = chain.list_agent_registry();
                let thoughts = chain.thoughts();
                let mut counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for t in thoughts {
                    *counts.entry(t.agent_id.as_str()).or_default() += 1;
                }
                for agent in &agents {
                    let live_count = counts.get(agent.agent_id.as_str()).copied().unwrap_or(0);
                    s.agents.push(AgentInfo {
                        chain_key: chain_key.clone(),
                        name: agent.display_name.clone(),
                        id: agent.agent_id.clone(),
                        status: agent.status.to_string(),
                        memories: live_count,
                        description: agent
                            .description
                            .as_deref()
                            .filter(|v| !v.trim().is_empty())
                            .unwrap_or("—")
                            .to_string(),
                    });
                }
            }
        }

        if let Ok(skill_registry) = SkillRegistry::open(&config.service.chain_dir) {
            let skills = skill_registry.list_skills();
            for summary in &skills {
                let uploaded_by = summary
                    .latest_uploaded_by_agent_name
                    .as_deref()
                    .unwrap_or(&summary.latest_uploaded_by_agent_id);
                s.skills.push(SkillInfo {
                    name: summary.name.clone(),
                    status: summary.status.to_string(),
                    versions: summary.version_count,
                    tags: summary.tags.clone(),
                    uploaded_by: uploaded_by.to_string(),
                });
            }
        }

        let mcp_local = format!("http://{}", handles.mcp.local_addr());
        let rest_local = format!("http://{}", handles.rest.local_addr());
        s.log_lines.push(format!(
            "[{}] MCP  (HTTP)  {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            mcp_local
        ));
        s.log_lines.push(format!(
            "[{}] REST (HTTP)  {}",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            rest_local
        ));
        if let Some(ref h) = handles.https_mcp {
            s.log_lines.push(format!(
                "[{}] MCP  (TLS)   https://{}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
        if let Some(ref h) = handles.https_rest {
            s.log_lines.push(format!(
                "[{}] REST (TLS)   https://{}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
        if let Some(ref h) = handles.dashboard {
            s.log_lines.push(format!(
                "[{}] Dashboard    https://{}/dashboard",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                h.local_addr()
            ));
        }
    }

    if is_first_run {
        let mut s = tui_state.lock().unwrap();
        s.migration_lines.push(String::new());
        s.migration_lines
            .push("First-run setup: run `mentisdbd wizard` in a separate terminal.".to_string());
        for line in build_first_run_setup_lines() {
            s.migration_lines.push(format!("  {line}"));
        }
    }

    // TUI stays running — no restart, no terminal flash.
    if let Err(te) = tui_handle.join().unwrap() {
        eprintln!("TUI error: {te}");
    }
    ctrlc_handle.abort();

    Ok(())
}

pub(crate) fn daemon_help_text() -> &'static str {
    "\
mentisdbd daemon

Usage:
  mentisdbd
  mentisdbd --force-update
  mentisdbd --help
  mentisdbd --mode stdio
  mentisdbd --mode http
  mentisdbd --mode both
  mentisdbd update
  mentisdbd force-update
  mentisdbd setup <agent|all> [--url <url>] [--dry-run]
  mentisdbd wizard [--url <url>] [--yes]
  mentisdbd add <content> [--type <type>] [--scope <scope>] [--tag <tag>] [--agent <id>] [--chain <key>] [--url <url>]
  mentisdbd search <query> [--limit <n>] [--scope <scope>] [--chain <key>] [--url <url>]
  mentisdbd agents [--chain <key>] [--url <url>]
  mentisdbd backup [--dir <path>] [--output <path>] [--flush] [--include-tls]
  mentisdbd restore <archive.mbak> [--dir <path>] [--overwrite] [--yes]

Flags:
  --force-update
    Show the update dialog even if already at the latest release.
    Useful for testing the update flow.

Backup subcommand:
  backup
    Create a .mbak backup archive of the MENTISDB_DIR.

    The backup includes all chain data files (*.tcbin, *.agents.json,
    *.entity-types.json, *.vectors.*.json), the global registry, and
    optionally TLS certificates and keys.

    When run against a running daemon, all chains are flushed via
    POST /v1/admin/flush before files are read, ensuring a consistent
    backup even when MENTISDB_AUTO_FLUSH=false. If the daemon is not
    running, files are captured as-is.

    Examples:
      mentisdbd backup
      mentisdbd backup --output /backups/mentisdb-2026-04-14.mbak
      mentisdbd backup --dir ~/.cloudllm/mentisdb --include-tls

    Options:
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --output <path>    Path for the .mbak archive (default: ./mentisdb-YYYY-MM-DD-HH-MM-SS.mbak)
      --flush            Flush all storage adapters before backing up (recommended if daemon is running)
      --include-tls      Include TLS certificates and keys in the backup
      --help             Show this help text

  restore
    Restore a MENTISDB_DIR from a .mbak backup archive.

    Restores all chain data, registry, skills, and optionally TLS files.
    By default, existing files are preserved (idempotent restore). Pass
    --overwrite to replace all files with their backed-up versions.

    If files already exist in the target directory and --overwrite is not
    passed, an interactive prompt asks for confirmation before proceeding.
    Pass --yes to skip all prompts and assume yes.

    Examples:
      mentisdbd restore mentisdb-2026-04-14.mbak
      mentisdbd restore /backups/mentisdb-2026-04-14.mbak --dir ~/.cloudllm/mentisdb
      mentisdbd restore /backups/mentisdb-2026-04-14.mbak --overwrite

    Options:
      <archive.mbak>     Path to the .mbak backup archive (required)
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --overwrite        Overwrite existing files (skips interactive prompt)
      --yes              Assume yes to all prompts (skips interactive confirmation)
      --help             Show this help text

  Valid values for `mentisdbd setup <agent>`:
    codex
    claude-code
    claude-desktop
    gemini
    opencode
    qwen
    copilot
    vscode-copilot
    all

Important environment variables:
  MENTISDB_DIR
    Root storage directory. Default: ~/.cloudllm/mentisdb

  MENTISDB_DEFAULT_CHAIN_KEY
    Default chain key when requests omit one.

  MENTISDB_STORAGE_ADAPTER
    New-chain storage format: binary

  MENTISDB_BIND_HOST
    Bind address host. Default: 127.0.0.1

  MENTISDB_MCP_PORT
    HTTP MCP port. Default: 9471

  MENTISDB_REST_PORT
    HTTP REST port. Default: 9472

  MENTISDB_HTTPS_MCP_PORT
    HTTPS MCP port. Default: 9473, set 0 to disable

  MENTISDB_HTTPS_REST_PORT
    HTTPS REST port. Default: 9474, set 0 to disable

  MENTISDB_DASHBOARD_PORT
    HTTPS dashboard port. Set 0 to disable

  MENTISDB_DASHBOARD_PIN
    Optional PIN required to open the HTTPS dashboard

  MENTISDB_TLS_CERT
  MENTISDB_TLS_KEY
    TLS certificate and private-key paths

  MENTISDB_UPDATE_CHECK
  MENTISDB_UPDATE_REPO
    Release update check controls

  MENTISDB_STARTUP_SOUND
  MENTISDB_THOUGHT_SOUNDS
    Startup, per-thought, and per-read audio controls

Examples:
  MENTISDB_DIR=~/.cloudllm/mentisdb mentisdbd
  MENTISDB_MCP_PORT=9471 MENTISDB_REST_PORT=9472 mentisdbd
"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonArgMode {
    Help,
    Run,
    RunWithForceUpdate,
    Stdio,
    Both,
    Update,
    ForceUpdate,
    CliSubcommand(Vec<OsString>),
}

pub(crate) fn parse_daemon_args<I, T>(args: I) -> Result<DaemonArgMode, String>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();

    if args.is_empty() {
        return Ok(DaemonArgMode::Run);
    }

    if args.len() == 1 && matches!(args[0].to_string_lossy().as_ref(), "--help" | "-h" | "help") {
        return Ok(DaemonArgMode::Help);
    }

    let first = args[0].to_string_lossy();
    if matches!(
        first.as_ref(),
        "setup" | "wizard" | "add" | "search" | "agents" | "backup" | "restore"
    ) {
        let mut command = vec![OsString::from("mentisdbd")];
        command.extend(args);
        return Ok(DaemonArgMode::CliSubcommand(command));
    }

    // Handle --force-update flag (shows update dialog even if up to date)
    let force_update = args
        .iter()
        .any(|arg| arg.to_string_lossy() == "--force-update");

    if args.len() == 1 && matches!(first.as_ref(), "update" | "force-update") {
        return if first.as_ref() == "update" {
            Ok(DaemonArgMode::Update)
        } else {
            Ok(DaemonArgMode::ForceUpdate)
        };
    }

    // Handle --mode flag
    if let Some(mode_idx) = args
        .iter()
        .position(|arg| arg.to_string_lossy() == "--mode")
    {
        if mode_idx + 1 >= args.len() {
            return Err("--mode requires a value (stdio, http, or both)".to_string());
        }
        let mode = args[mode_idx + 1].to_string_lossy();
        match mode.as_ref() {
            "stdio" => return Ok(DaemonArgMode::Stdio),
            "http" => return Ok(DaemonArgMode::Run),
            "both" => return Ok(DaemonArgMode::Both),
            _ => {
                return Err(format!(
                    "Invalid --mode value '{}'. Valid values: stdio, http, both",
                    mode
                ))
            }
        }
    }

    // Also accept --stdio-mcp as an alias for --mode stdio
    if args
        .iter()
        .any(|arg| arg.to_string_lossy() == "--stdio-mcp")
    {
        return Ok(DaemonArgMode::Stdio);
    }

    if force_update {
        return Ok(DaemonArgMode::RunWithForceUpdate);
    }

    Err(format!(
        "Unexpected arguments for `mentisdbd`: {}",
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn run_cli_subcommand(args: Vec<OsString>) -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut errors = stderr.lock();

    run_cli_subcommand_with_io(args, &mut input, &mut output, &mut errors)
}

pub(crate) fn run_cli_subcommand_with_io(
    args: Vec<OsString>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    mentisdb::cli::run_with_io(args, input, out, err)
}

async fn run_update_standalone(force: bool) -> ExitCode {
    let update_config = update_config_from_env();
    if !update_config.enabled && !force {
        eprintln!("Update checks are disabled (MENTISDB_UPDATE_CHECK=false).");
        eprintln!("Use `mentisdbd force-update` to update regardless.");
        return ExitCode::from(1);
    }

    let current_version = env!("CARGO_PKG_VERSION");

    if force {
        println!("Force-updating mentisdbd to the latest release…");
        let latest = match fetch_latest_release(&update_config.repo).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to fetch latest release: {e}");
                return ExitCode::from(1);
            }
        };
        let latest_display = normalize_release_tag_display(&latest.tag_name);
        println!("Current: {current_version}, latest: {latest_display}");
        println!("Installing release {latest_display} via cargo…");
        match install_latest_release(&latest.tag_name, &update_config.repo) {
            Ok(path) => {
                println!("Installed mentisdbd to {}", path.display());
                println!("Restart mentisdbd to use the new version.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Install failed: {e}");
                ExitCode::from(1)
            }
        }
    } else {
        let latest = match fetch_latest_release(&update_config.repo).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to fetch latest release: {e}");
                return ExitCode::from(1);
            }
        };
        let latest_display = normalize_release_tag_display(&latest.tag_name);

        if !release_tag_is_newer(&latest.tag_name, current_version) {
            println!(
                "mentisdbd is up to date (current {}, latest {}).",
                current_version, latest_display
            );
            return ExitCode::SUCCESS;
        }

        let dialog_lines =
            build_update_available_lines(current_version, &latest_display, &latest.html_url);
        ascii_notice_box("mentisdbd update available", &dialog_lines);

        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!(
                "Non-interactive terminal. Run manually:\n\
                 cargo install --git https://github.com/{} --tag {} --locked --force --bin {UPDATE_BINARY_NAME} {UPDATE_CRATE_NAME}",
                update_config.repo, latest.tag_name
            );
            return ExitCode::from(1);
        }

        match prompt_yes_no("Selection") {
            Ok(true) => {}
            Ok(false) => {
                println!("Update skipped.");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("Prompt failed: {e}");
                return ExitCode::from(1);
            }
        }

        println!("Installing release {latest_display} via cargo…");
        match install_latest_release(&latest.tag_name, &update_config.repo) {
            Ok(path) => {
                println!("Installed mentisdbd to {}", path.display());
                println!("Restart mentisdbd to use the new version.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Install failed: {e}");
                ExitCode::from(1)
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match parse_daemon_args(std::env::args_os().skip(1)) {
        Ok(DaemonArgMode::Help) => {
            println!("{}", daemon_help_text());
            ExitCode::SUCCESS
        }
        Ok(DaemonArgMode::Run) => match run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        },
        Ok(DaemonArgMode::RunWithForceUpdate) => match run_with_force_update().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        },
        Ok(DaemonArgMode::Stdio) => {
            init_logger();
            let config = MentisDbServerConfig::from_env();
            match run_stdio_mode(config).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::from(1)
                }
            }
        }
        Ok(DaemonArgMode::Both) => {
            init_logger();
            let config = MentisDbServerConfig::from_env();
            let stdio_handle = tokio::spawn(async move { run_stdio_mode(config.clone()).await });
            let http_handle = tokio::spawn(async move { run().await });
            let (stdio_result, http_result) = tokio::join!(stdio_handle, http_handle);
            if let Err(e) = stdio_result {
                eprintln!("Stdio server error: {e}");
            }
            match http_result {
                Ok(Ok(())) => ExitCode::SUCCESS,
                Ok(Err(e)) => {
                    eprintln!("HTTP server error: {e}");
                    ExitCode::from(1)
                }
                Err(e) => {
                    eprintln!("HTTP server join error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Ok(DaemonArgMode::Update) => run_update_standalone(false).await,
        Ok(DaemonArgMode::ForceUpdate) => run_update_standalone(true).await,
        Ok(DaemonArgMode::CliSubcommand(args)) => run_cli_subcommand(args),
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            eprintln!("{}", daemon_help_text());
            ExitCode::from(2)
        }
    }
}

fn init_logger() {
    let mut builder = env_logger::Builder::from_env(Env::default().default_filter_or("info"));
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

#[cfg(test)]
pub(crate) fn build_endpoint_catalog(
    mcp_addr: std::net::SocketAddr,
    rest_addr: std::net::SocketAddr,
    https_mcp_addr: Option<std::net::SocketAddr>,
    https_rest_addr: Option<std::net::SocketAddr>,
) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;

    fn write_rest_catalog(out: &mut String, scheme: &str, addr: std::net::SocketAddr) {
        writeln!(out, "    GET  {scheme}://{addr}/health").unwrap();
        writeln!(out, "      Health check for the REST surface.").unwrap();
        writeln!(out, "    GET  {scheme}://{addr}/v1/chains").unwrap();
        writeln!(
            out,
            "      List chains with version, adapter, counts, and storage location."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/chains/branch").unwrap();
        writeln!(out, "      Create a new branch chain from a prior thought.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/chains/merge").unwrap();
        writeln!(
            out,
            "      Merge one chain into another and delete the source chain."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents").unwrap();
        writeln!(out, "      List agent identity summaries for one chain.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agent").unwrap();
        writeln!(out, "      Return one full agent registry record.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agent-registry").unwrap();
        writeln!(out, "      Return the full agent registry for one chain.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/upsert").unwrap();
        writeln!(out, "      Create or update an agent registry record.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/description").unwrap();
        writeln!(out, "      Set or clear one agent description.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/aliases").unwrap();
        writeln!(out, "      Add one alias to a registered agent.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/keys").unwrap();
        writeln!(out, "      Add or replace one agent public key.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/keys/revoke").unwrap();
        writeln!(out, "      Revoke one agent public key.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/agents/disable").unwrap();
        writeln!(out, "      Disable one registered agent.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/entity-types").unwrap();
        writeln!(out, "      List registered semantic entity types.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/entity-types/upsert").unwrap();
        writeln!(out, "      Create or update one entity-type definition.").unwrap();
        writeln!(out, "    GET  {scheme}://{addr}/mentisdb_skill_md").unwrap();
        writeln!(out, "      Return the embedded official MentisDB skill Markdown (compatibility fallback; MCP clients should use `initialize` plus `resources/read` for `mentisdb://skill/core`).").unwrap();
        writeln!(out, "    GET  {scheme}://{addr}/v1/skills").unwrap();
        writeln!(
            out,
            "      List uploaded skill summaries from the registry."
        )
        .unwrap();
        writeln!(out, "    GET  {scheme}://{addr}/v1/skills/manifest").unwrap();
        writeln!(
            out,
            "      Describe searchable fields and supported skill formats."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/upload").unwrap();
        writeln!(out, "      Upload a new immutable skill version.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/search").unwrap();
        writeln!(
            out,
            "      Search skills by metadata, uploader identity, and time window."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/read").unwrap();
        writeln!(
            out,
            "      Read one stored skill as Markdown or JSON with safety warnings."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/versions").unwrap();
        writeln!(out, "      List immutable uploaded versions for one skill.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/deprecate").unwrap();
        writeln!(out, "      Mark one skill as deprecated.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/skills/revoke").unwrap();
        writeln!(out, "      Mark one skill as revoked.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/bootstrap").unwrap();
        writeln!(
            out,
            "      Bootstrap an empty chain with an initial checkpoint."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/thoughts").unwrap();
        writeln!(out, "      Append a durable thought.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/retrospectives").unwrap();
        writeln!(out, "      Append a retrospective thought.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/search").unwrap();
        writeln!(
            out,
            "      Search thoughts by semantic and identity filters."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/lexical-search").unwrap();
        writeln!(
            out,
            "      Ranked lexical search with scores and matched-term diagnostics."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/ranked-search").unwrap();
        writeln!(
            out,
            "      Flat ranked search with optional graph-aware expansion scoring."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/federated-search").unwrap();
        writeln!(
            out,
            "      Query multiple chains in one request and merge the results."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/context-bundles").unwrap();
        writeln!(
            out,
            "      Seed-anchored grouped context bundles for agent reasoning."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/recent-context").unwrap();
        writeln!(out, "      Render a recent-context prompt snippet.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/memory-markdown").unwrap();
        writeln!(out, "      Export a MEMORY.md-style markdown view.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/import-markdown").unwrap();
        writeln!(
            out,
            "      Import a MEMORY.md-style markdown document into a chain."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/thought").unwrap();
        writeln!(
            out,
            "      Read one thought by id, hash, or append-order index."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/thoughts/genesis").unwrap();
        writeln!(out, "      Return the first thought in append order.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/thoughts/traverse").unwrap();
        writeln!(
            out,
            "      Traverse thoughts forward or backward in filtered chunks."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/head").unwrap();
        writeln!(
            out,
            "      Return the latest thought at the chain tip and head metadata."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/vectors/rebuild").unwrap();
        writeln!(out, "      Rebuild managed vector sidecars for a chain.").unwrap();
        writeln!(out, "    GET  {scheme}://{addr}/v1/webhooks").unwrap();
        writeln!(out, "      List registered webhook deliveries.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/webhooks").unwrap();
        writeln!(out, "      Register a webhook callback.").unwrap();
        writeln!(out, "    DELETE {scheme}://{addr}/v1/webhooks/{{id}}").unwrap();
        writeln!(out, "      Delete one registered webhook.").unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/extract-memories").unwrap();
        writeln!(
            out,
            "      Extract structured memories from free-form text."
        )
        .unwrap();
        writeln!(out, "    POST {scheme}://{addr}/v1/admin/flush").unwrap();
        writeln!(out, "      Flush all chains to durable storage.").unwrap();
    }

    writeln!(&mut out).unwrap();
    writeln!(&mut out, "Endpoints:").unwrap();
    writeln!(&mut out, "  MCP").unwrap();
    writeln!(&mut out, "    POST http://{mcp_addr}").unwrap();
    writeln!(
        &mut out,
        "      Standard streamable HTTP MCP root endpoint."
    )
    .unwrap();
    writeln!(
        &mut out,
        "      Supports `initialize`, tool calls, and MCP resources such as `mentisdb://skill/core` via `resources/list` and `resources/read`."
    )
    .unwrap();
    writeln!(&mut out, "    GET  http://{mcp_addr}/health").unwrap();
    writeln!(&mut out, "      Health check for the MCP surface.").unwrap();
    writeln!(&mut out, "    POST http://{mcp_addr}/tools/list").unwrap();
    writeln!(
        &mut out,
        "      Legacy CloudLLM-compatible MCP tool discovery."
    )
    .unwrap();
    writeln!(&mut out, "    POST http://{mcp_addr}/tools/execute").unwrap();
    writeln!(
        &mut out,
        "      Legacy CloudLLM-compatible MCP tool execution."
    )
    .unwrap();
    writeln!(&mut out, "  REST").unwrap();
    write_rest_catalog(&mut out, "http", rest_addr);
    writeln!(&mut out).unwrap();

    if let Some(https_mcp_addr) = https_mcp_addr {
        writeln!(&mut out, "  HTTPS MCP").unwrap();
        writeln!(&mut out, "    POST https://{https_mcp_addr}").unwrap();
        writeln!(
            &mut out,
            "      Streamable HTTP MCP root endpoint over TLS."
        )
        .unwrap();
        writeln!(&mut out, "      Supports `initialize`, tool calls, and MCP resources such as `mentisdb://skill/core` via `resources/list` and `resources/read`.").unwrap();
        writeln!(&mut out, "    GET  https://{https_mcp_addr}/health").unwrap();
        writeln!(&mut out, "      Health check for the HTTPS MCP surface.").unwrap();
        writeln!(&mut out, "    POST https://{https_mcp_addr}/tools/list").unwrap();
        writeln!(
            &mut out,
            "      Legacy CloudLLM-compatible MCP tool discovery (HTTPS)."
        )
        .unwrap();
        writeln!(&mut out, "    POST https://{https_mcp_addr}/tools/execute").unwrap();
        writeln!(
            &mut out,
            "      Legacy CloudLLM-compatible MCP tool execution (HTTPS)."
        )
        .unwrap();
    }
    if let Some(https_rest_addr) = https_rest_addr {
        writeln!(&mut out, "  HTTPS REST").unwrap();
        write_rest_catalog(&mut out, "https", https_rest_addr);
        writeln!(&mut out).unwrap();
    }

    out
}

/// Build configuration display lines for the TUI top-left pane.
fn build_config_lines(config: &MentisDbServerConfig, update_config: &UpdateConfig) -> Vec<String> {
    let mut lines = Vec::new();

    let mut add_var = |name: &str, raw: Option<String>, default: String| {
        let current = raw.unwrap_or_else(|| "<unset>".to_string());
        lines.push(format!("  {name}={current} (default: {default})"));
    };

    add_var(
        "MENTISDB_DIR",
        std::env::var("MENTISDB_DIR").ok(),
        config.service.chain_dir.display().to_string(),
    );
    add_var(
        "MENTISDB_DEFAULT_CHAIN_KEY",
        std::env::var("MENTISDB_DEFAULT_CHAIN_KEY")
            .ok()
            .or_else(|| std::env::var("MENTISDB_DEFAULT_KEY").ok()),
        config.service.default_chain_key.clone(),
    );
    add_var(
        "MENTISDB_STORAGE_ADAPTER",
        std::env::var("MENTISDB_STORAGE_ADAPTER").ok(),
        config.service.default_storage_adapter.to_string(),
    );
    add_var(
        "MENTISDB_AUTO_FLUSH",
        std::env::var("MENTISDB_AUTO_FLUSH").ok(),
        config.service.auto_flush.to_string(),
    );
    add_var(
        "MENTISDB_VERBOSE",
        std::env::var("MENTISDB_VERBOSE").ok(),
        config.service.verbose.to_string(),
    );
    add_var(
        "MENTISDB_LOG_FILE",
        std::env::var("MENTISDB_LOG_FILE").ok(),
        config
            .service
            .log_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<none>".to_string()),
    );
    add_var(
        "MENTISDB_BIND_HOST",
        std::env::var("MENTISDB_BIND_HOST").ok(),
        config.mcp_addr.ip().to_string(),
    );
    add_var(
        "MENTISDB_MCP_PORT",
        std::env::var("MENTISDB_MCP_PORT").ok(),
        config.mcp_addr.port().to_string(),
    );
    add_var(
        "MENTISDB_REST_PORT",
        std::env::var("MENTISDB_REST_PORT").ok(),
        config.rest_addr.port().to_string(),
    );
    add_var(
        "MENTISDB_HTTPS_MCP_PORT",
        std::env::var("MENTISDB_HTTPS_MCP_PORT").ok(),
        match config.https_mcp_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        },
    );
    add_var(
        "MENTISDB_HTTPS_REST_PORT",
        std::env::var("MENTISDB_HTTPS_REST_PORT").ok(),
        match config.https_rest_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        },
    );
    add_var(
        "MENTISDB_TLS_CERT",
        std::env::var("MENTISDB_TLS_CERT").ok(),
        config.tls_cert_path.display().to_string(),
    );
    add_var(
        "MENTISDB_TLS_KEY",
        std::env::var("MENTISDB_TLS_KEY").ok(),
        config.tls_key_path.display().to_string(),
    );
    add_var(
        "MENTISDB_DASHBOARD_PORT",
        std::env::var("MENTISDB_DASHBOARD_PORT").ok(),
        match config.dashboard_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        },
    );
    add_var(
        "MENTISDB_DASHBOARD_PIN",
        std::env::var("MENTISDB_DASHBOARD_PIN").ok(),
        if config.dashboard_pin.is_some() {
            "set".to_string()
        } else {
            "not set".to_string()
        },
    );
    add_var(
        "MENTISDB_UPDATE_CHECK",
        std::env::var("MENTISDB_UPDATE_CHECK").ok(),
        update_config.enabled.to_string(),
    );
    add_var(
        "MENTISDB_UPDATE_REPO",
        std::env::var("MENTISDB_UPDATE_REPO").ok(),
        update_config.repo.clone(),
    );
    add_var(
        "RUST_LOG",
        std::env::var("RUST_LOG").ok(),
        "info (default)".to_string(),
    );
    #[cfg(feature = "startup-sound")]
    add_var(
        "MENTISDB_STARTUP_SOUND",
        std::env::var("MENTISDB_STARTUP_SOUND").ok(),
        "true (default)".to_string(),
    );
    #[cfg(feature = "startup-sound")]
    add_var(
        "MENTISDB_THOUGHT_SOUNDS",
        std::env::var("MENTISDB_THOUGHT_SOUNDS").ok(),
        "false (default)".to_string(),
    );

    lines
}

/// Build TLS info lines for the TUI top-right pane.
fn build_tls_info_lines(
    config: &MentisDbServerConfig,
    handles: &MentisDbServerHandles,
) -> Vec<String> {
    if handles.https_mcp.is_none() && handles.https_rest.is_none() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mcp_port = handles.https_mcp.as_ref().map(|h| h.local_addr().port());
    let rest_port = handles.https_rest.as_ref().map(|h| h.local_addr().port());

    lines.push(format!(
        "TLS Certificate: {}",
        config.tls_cert_path.display()
    ));
    lines.push(String::new());
    lines.push("  my.mentisdb.com is a public DNS A-record → 127.0.0.1".to_string());
    lines.push("  You can use it as a friendly hostname for this local daemon.".to_string());
    if let Some(port) = mcp_port {
        lines.push(format!("  MCP:  https://my.mentisdb.com:{port}"));
    }
    if let Some(port) = rest_port {
        lines.push(format!("  REST: https://my.mentisdb.com:{port}"));
    }
    lines.push(String::new());
    lines.push("  To avoid certificate warnings, trust the self-signed cert once:".to_string());

    if cfg!(target_os = "macos") {
        lines.push("  macOS:   sudo security add-trusted-cert -d -r trustRoot \\".to_string());
        lines.push("           -k /Library/Keychains/System.keychain \\".to_string());
        lines.push(format!("           {}", config.tls_cert_path.display()));
    } else if cfg!(target_os = "linux") {
        lines.push(format!(
            "  Linux:   sudo cp {} /usr/local/share/ca-certificates/mentisdb.crt",
            config.tls_cert_path.display()
        ));
        lines.push("           sudo update-ca-certificates".to_string());
    } else if cfg!(target_os = "windows") {
        lines.push(format!(
            "  Windows: certutil -addstore Root {}",
            config.tls_cert_path.display()
        ));
    }

    lines
}

/// Build endpoint display lines for the TUI top-right pane.
fn build_endpoint_lines(
    handles: &MentisDbServerHandles,
    mcp_port: u16,
    rest_port: u16,
) -> Vec<String> {
    let mut lines = Vec::new();

    let mcp_local = format!("http://{}", handles.mcp.local_addr());
    let rest_local = format!("http://{}", handles.rest.local_addr());
    let mcp_friendly = format!("http://my.mentisdb.com:{mcp_port}");
    let rest_friendly = format!("http://my.mentisdb.com:{rest_port}");

    lines.push(format!("  MCP  (HTTP)  {mcp_local:<32}  {mcp_friendly}"));
    lines.push(format!("  REST (HTTP)  {rest_local:<32}  {rest_friendly}"));

    if let Some(ref h) = handles.https_mcp {
        let local = format!("https://{}", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}");
        lines.push(format!("  MCP  (TLS)   {local:<32}  {friendly}"));
    }
    if let Some(ref h) = handles.https_rest {
        let local = format!("https://{}", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}");
        lines.push(format!("  REST (TLS)   {local:<32}  {friendly}"));
    }
    if let Some(ref h) = handles.dashboard {
        let local = format!("https://{}/dashboard", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}/dashboard");
        lines.push(format!("  Dashboard    {local:<32}  {friendly}"));
    }

    lines
}
