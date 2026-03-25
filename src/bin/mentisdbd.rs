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
//! - `MENTISDB_THOUGHT_SOUNDS` (default `false`; set `1`/`true`/`yes`/`on` to enable per-thought sounds)
//! - `RUST_LOG`

use env_logger::Env;
use mentisdb::server::{
    adopt_legacy_default_mentisdb_dir, start_servers, MentisDbServerConfig, MentisDbServerHandles,
};
use mentisdb::{
    load_registered_chains, migrate_registered_chains_with_adapter, migrate_skill_registry,
    refresh_registered_chain_counts, MentisDb, MentisDbMigrationEvent, SkillRegistry, ThoughtType,
};
use serde::Deserialize;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
#[cfg(feature = "startup-sound")]
use std::sync::{Mutex, OnceLock};
use tokio::sync::{mpsc, oneshot};

const MENTIS_BANNER: &str = r#"███╗   ███╗███████╗███╗   ██╗████████╗██╗███████╗
████╗ ████║██╔════╝████╗  ██║╚══██╔══╝██║██╔════╝
██╔████╔██║█████╗  ██╔██╗ ██║   ██║   ██║███████╗
██║╚██╔╝██║██╔══╝  ██║╚██╗██║   ██║   ██║╚════██║
██║ ╚═╝ ██║███████╗██║ ╚████║   ██║   ██║███████║
╚═╝     ╚═╝╚══════╝╚═╝  ╚═══╝   ╚═╝   ╚═╝╚══════╝"#;
const DB_BANNER: &str = r#"██████╗ ██████╗ 
██╔══██╗██╔══██╗
██║  ██║██████╔╝
██║  ██║██╔══██╗
██████╔╝██████╔╝
╚═════╝ ╚═════╝ "#;
const GREEN: &str = "\x1b[38;5;82m";
const YELLOW: &str = "\x1b[38;5;226m";
const PINK: &str = "\x1b[38;5;213m";
const CYAN: &str = "\x1b[38;5;87m";
const DIM: &str = "\x1b[2m";
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

#[derive(Debug)]
struct RestartRequest {
    exe_path: PathBuf,
    args: Vec<OsString>,
    release_tag: String,
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
        format!("Install release {latest_display} and restart now? [Y/N]"),
    ]
}

fn prompt_yes_no(prompt: &str) -> io::Result<bool> {
    let mut input = String::new();
    loop {
        print!("{prompt} [Y/N]: ");
        io::stdout().flush()?;
        input.clear();
        io::stdin().read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please type Y or N."),
        }
    }
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

    let current_exe = std::env::current_exe()?;
    Ok(installed_binary_path()
        .filter(|path| path.exists())
        .unwrap_or(current_exe))
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

fn shutdown_all_servers(
    handles: &mut MentisDbServerHandles,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    handles.mcp.shutdown()?;
    handles.rest.shutdown()?;
    if let Some(handle) = handles.https_mcp.as_mut() {
        let _ = handle.shutdown();
    }
    if let Some(handle) = handles.https_rest.as_mut() {
        let _ = handle.shutdown();
    }
    if let Some(handle) = handles.dashboard.as_mut() {
        let _ = handle.shutdown();
    }
    Ok(())
}

fn restart_installed_binary(
    request: &RestartRequest,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Command::new(&request.exe_path)
        .args(&request.args)
        .spawn()?;
    Ok(())
}

async fn run_update_check_task(
    config: UpdateConfig,
    restart_tx: mpsc::UnboundedSender<RestartRequest>,
    startup_ready: oneshot::Receiver<()>,
) {
    let latest = match fetch_latest_release(&config.repo).await {
        Ok(latest) => latest,
        Err(error) => {
            let _ = startup_ready.await;
            println!("Update check failed: {error}");
            return;
        }
    };
    let _ = startup_ready.await;

    let current_version = env!("CARGO_PKG_VERSION");
    if !release_tag_is_newer(&latest.tag_name, current_version) {
        println!(
            "Update check: mentisdbd is up to date (current {}, latest {}).",
            current_version,
            normalize_release_tag_display(&latest.tag_name)
        );
        return;
    }

    let latest_display = normalize_release_tag_display(&latest.tag_name);
    let dialog_lines =
        build_update_available_lines(current_version, &latest_display, &latest.html_url);
    ascii_notice_box("mentisdbd update available", &dialog_lines);

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        println!(
            "Update check: non-interactive terminal detected; skipping prompt. \
Run `cargo install --git https://github.com/{} --tag {} --locked --force --bin {UPDATE_BINARY_NAME} {UPDATE_CRATE_NAME}` to update manually.",
            config.repo,
            latest.tag_name
        );
        return;
    }

    let should_update = match tokio::task::spawn_blocking(move || prompt_yes_no("Selection")).await
    {
        Ok(Ok(approved)) => approved,
        Ok(Err(error)) => {
            println!("Update prompt failed: {error}");
            return;
        }
        Err(error) => {
            println!("Update prompt failed: {error}");
            return;
        }
    };

    if !should_update {
        println!("Update check: skipped update to {latest_display}.");
        return;
    }

    println!("Installing release {} via cargo...", latest_display);
    let restart_request = match tokio::task::spawn_blocking({
        let tag_name = latest.tag_name.clone();
        let repo = config.repo.clone();
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        move || -> Result<RestartRequest, Box<dyn std::error::Error + Send + Sync>> {
            let exe_path = install_latest_release(&tag_name, &repo)?;
            Ok(RestartRequest {
                exe_path,
                args,
                release_tag: tag_name,
            })
        }
    })
    .await
    {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            println!("Update install failed: {error}");
            return;
        }
        Err(error) => {
            println!("Update install failed: {error}");
            return;
        }
    };

    if restart_tx.send(restart_request).is_err() {
        println!(
            "Update installed, but restart signaling failed. Please restart mentisdbd manually."
        );
    }
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_logger();
    let storage_root_migration = if std::env::var_os("MENTISDB_DIR").is_none() {
        adopt_legacy_default_mentisdb_dir()?
    } else {
        None
    };
    let mut config = MentisDbServerConfig::from_env();

    // Run migrations before starting servers.  Progress lines print live here
    // (rare — only on first run or version upgrades).
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
            } => println!(
                "{} Migrating chain {} from version {} to version {}",
                progress_bar(current, total),
                chain_key,
                from_version,
                to_version
            ),
            MentisDbMigrationEvent::Completed {
                chain_key,
                from_version,
                to_version,
                current,
                total,
            } => println!(
                "{} Migrated chain {} from version {} to version {}",
                progress_bar(current, total),
                chain_key,
                from_version,
                to_version
            ),
            MentisDbMigrationEvent::StartedReconciliation {
                chain_key,
                from_storage_adapter,
                to_storage_adapter,
                current,
                total,
            } => println!(
                "{} Reconciling chain {} from {} storage to {} storage",
                progress_bar(current, total),
                chain_key,
                from_storage_adapter,
                to_storage_adapter
            ),
            MentisDbMigrationEvent::CompletedReconciliation {
                chain_key,
                from_storage_adapter,
                to_storage_adapter,
                current,
                total,
            } => println!(
                "{} Reconciled chain {} from {} storage to {} storage",
                progress_bar(current, total),
                chain_key,
                from_storage_adapter,
                to_storage_adapter
            ),
        },
    )?;

    // Capture skill registry migration result to print later.
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

    // Refresh any stale thought_count / agent_count values in the registry JSON.
    // This repairs counts from older versions, hard crashes, or chains appended
    // outside the running daemon.  On every append the registry is kept current
    // (persist_chain_registration), but a startup pass guarantees correctness.
    if let Err(e) = refresh_registered_chain_counts(&config.service.chain_dir) {
        log::warn!("Could not refresh chain registry counts: {e}");
    }

    // Register per-thought sound callback when MENTISDB_THOUGHT_SOUNDS is enabled.
    #[cfg(feature = "startup-sound")]
    {
        let thought_sounds_enabled = std::env::var("MENTISDB_THOUGHT_SOUNDS")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if thought_sounds_enabled {
            config.service = config
                .service
                .with_on_thought_appended(Arc::new(play_thought_sound));
        }
    }

    let mut handles = start_servers(config.clone()).await?;
    let update_config = update_config_from_env();
    let (restart_tx, mut restart_rx) = mpsc::unbounded_channel::<RestartRequest>();
    let mut startup_ready_tx = None;
    if update_config.enabled {
        let (tx, rx) = oneshot::channel::<()>();
        startup_ready_tx = Some(tx);
        tokio::spawn(run_update_check_task(update_config.clone(), restart_tx, rx));
    }

    // ── Useful info first ────────────────────────────────────────────────────
    print_endpoint_catalog(&handles);
    print_chain_summary(&config)?;
    print_agent_registry_summary(&config)?;
    print_skill_registry_summary(&config)?;
    print_tls_tip(&config, &handles);
    println!("Press Ctrl+C to stop.");

    // ── Startup summary at the bottom ────────────────────────────────────────
    println!();
    print_banner();
    // Flush banner to stdout before the jingle plays.
    let _ = std::io::stdout().flush();
    #[cfg(feature = "startup-sound")]
    play_startup_jingle();
    println!("mentisdb v{}", env!("CARGO_PKG_VERSION"));
    println!("mentisdbd started");

    if let Some(report) = &storage_root_migration {
        println!("Legacy storage adoption:");
        if report.renamed_root_dir {
            println!(
                "  Renamed {} -> {}",
                report.source_dir.display(),
                report.target_dir.display()
            );
        } else {
            println!(
                "  Merged {} legacy entries from {} into {}",
                report.merged_entries,
                report.source_dir.display(),
                report.target_dir.display()
            );
        }
        if report.renamed_registry_file {
            println!("  Renamed thoughtchain-registry.json -> mentisdb-registry.json");
        }
    }

    println!("Configuration:");
    print_env_var(
        "MENTISDB_DIR",
        Some(config.service.chain_dir.display().to_string()),
    );
    print_env_var(
        "MENTISDB_DEFAULT_CHAIN_KEY",
        Some(config.service.default_chain_key.clone()),
    );
    print_env_var(
        "MENTISDB_STORAGE_ADAPTER",
        Some(config.service.default_storage_adapter.to_string()),
    );
    print_env_var(
        "MENTISDB_AUTO_FLUSH",
        Some(config.service.auto_flush.to_string()),
    );
    print_env_var("MENTISDB_VERBOSE", Some(config.service.verbose.to_string()));
    print_env_var(
        "MENTISDB_LOG_FILE",
        config
            .service
            .log_file
            .as_ref()
            .map(|p| p.display().to_string()),
    );
    print_env_var("MENTISDB_BIND_HOST", Some(config.mcp_addr.ip().to_string()));
    print_env_var(
        "MENTISDB_MCP_PORT",
        Some(config.mcp_addr.port().to_string()),
    );
    print_env_var(
        "MENTISDB_REST_PORT",
        Some(config.rest_addr.port().to_string()),
    );
    print_env_var(
        "MENTISDB_HTTPS_MCP_PORT",
        Some(match config.https_mcp_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        }),
    );
    print_env_var(
        "MENTISDB_HTTPS_REST_PORT",
        Some(match config.https_rest_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        }),
    );
    print_env_var(
        "MENTISDB_TLS_CERT",
        Some(config.tls_cert_path.display().to_string()),
    );
    print_env_var(
        "MENTISDB_TLS_KEY",
        Some(config.tls_key_path.display().to_string()),
    );
    print_env_var(
        "MENTISDB_DASHBOARD_PORT",
        Some(match config.dashboard_addr {
            Some(addr) => addr.port().to_string(),
            None => "disabled".to_string(),
        }),
    );
    print_env_var(
        "MENTISDB_DASHBOARD_PIN",
        Some(if config.dashboard_pin.is_some() {
            "set".to_string()
        } else {
            "not set".to_string()
        }),
    );
    print_env_var(
        "MENTISDB_UPDATE_CHECK",
        Some(update_config.enabled.to_string()),
    );
    print_env_var("MENTISDB_UPDATE_REPO", Some(update_config.repo.clone()));
    print_env_var(
        "RUST_LOG",
        std::env::var("RUST_LOG")
            .ok()
            .or_else(|| Some("info (default)".to_string())),
    );
    #[cfg(feature = "startup-sound")]
    print_env_var(
        "MENTISDB_STARTUP_SOUND",
        std::env::var("MENTISDB_STARTUP_SOUND")
            .ok()
            .or_else(|| Some("true (default)".to_string())),
    );
    #[cfg(feature = "startup-sound")]
    print_env_var(
        "MENTISDB_THOUGHT_SOUNDS",
        std::env::var("MENTISDB_THOUGHT_SOUNDS")
            .ok()
            .or_else(|| Some("false (default)".to_string())),
    );

    if migration_reports.is_empty() {
        println!("No chain migrations required.");
    }
    println!("{skill_registry_msg}");
    println!("mentisdbd running");

    // ── Resolved endpoints (local + friendly) ────────────────────────────────
    let mcp_local = format!("http://{}", handles.mcp.local_addr());
    let rest_local = format!("http://{}", handles.rest.local_addr());
    let mcp_port = handles.mcp.local_addr().port();
    let rest_port = handles.rest.local_addr().port();
    let mcp_friendly = format!("http://my.mentisdb.com:{mcp_port}");
    let rest_friendly = format!("http://my.mentisdb.com:{rest_port}");

    println!("Resolved endpoints:");
    println!("  MCP  (HTTP)  {mcp_local:<32}  {YELLOW}{mcp_friendly}{RESET}");
    println!("  REST (HTTP)  {rest_local:<32}  {YELLOW}{rest_friendly}{RESET}");

    if let Some(ref h) = handles.https_mcp {
        let local = format!("https://{}", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}");
        println!("  MCP  (TLS)   {local:<32}  {YELLOW}{friendly}{RESET}");
    }
    if let Some(ref h) = handles.https_rest {
        let local = format!("https://{}", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}");
        println!("  REST (TLS)   {local:<32}  {YELLOW}{friendly}{RESET}");
    }
    if let Some(ref h) = handles.dashboard {
        let local = format!("https://{}/dashboard", h.local_addr());
        let port = h.local_addr().port();
        let friendly = format!("https://my.mentisdb.com:{port}/dashboard");
        println!("  Dashboard    {local:<32}  {YELLOW}{friendly}{RESET}");
    }
    if let Some(tx) = startup_ready_tx.take() {
        let _ = tx.send(());
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        restart = async {
            match restart_rx.recv().await {
                Some(request) => request,
                None => std::future::pending::<RestartRequest>().await,
            }
        }, if update_config.enabled => {
            println!(
                "Update installed: restarting mentisdbd with release {}...",
                normalize_release_tag_display(&restart.release_tag)
            );
            shutdown_all_servers(&mut handles)?;
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            restart_installed_binary(&restart)?;
            return Ok(());
        }
    }
    Ok(())
}

pub(crate) fn daemon_help_text() -> &'static str {
    "\
mentisdbd daemon

Usage:
  mentisdbd
  mentisdbd --help

Role:
  Start the MentisDB MCP server, REST server, and web dashboard.

Setup and onboarding:
  The interactive setup and integration commands live in the separate `mentisdb` binary:
    mentisdb --help
    mentisdb setup <agent|all>
    mentisdb wizard

Important environment variables:
  MENTISDB_DIR
    Root storage directory. Default: ~/.cloudllm/mentisdb

  MENTISDB_DEFAULT_CHAIN_KEY
    Default chain key when requests omit one.

  MENTISDB_STORAGE_ADAPTER
    New-chain storage format: binary or jsonl

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

  MENTISDB_TLS_CERT
  MENTISDB_TLS_KEY
    TLS certificate and private-key paths

  MENTISDB_UPDATE_CHECK
  MENTISDB_UPDATE_REPO
    Release update check controls

  MENTISDB_STARTUP_SOUND
  MENTISDB_THOUGHT_SOUNDS
    Startup and per-thought audio controls

Examples:
  MENTISDB_DIR=~/.cloudllm/mentisdb mentisdbd
  MENTISDB_MCP_PORT=9471 MENTISDB_REST_PORT=9472 mentisdbd
"
}

pub(crate) fn args_request_help<I, T>(args: I) -> bool
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    args.into_iter()
        .map(|arg| arg.into().to_string_lossy().into_owned())
        .any(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
}

#[allow(dead_code)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if args_request_help(std::env::args_os().skip(1)) {
        println!("{}", daemon_help_text());
        return Ok(());
    }
    run().await
}

fn print_env_var(name: &str, effective_value: Option<String>) {
    if let Ok(raw_value) = std::env::var(name) {
        println!(
            "  {YELLOW}{name}{RESET}={raw_value} (effective: {GREEN}{}{RESET})",
            display_value(effective_value)
        );
        return;
    }

    println!(
        "  {YELLOW}{name}{RESET}=<unset> (effective default: {GREEN}{}{RESET})",
        display_value(effective_value)
    );
}

fn display_value(value: Option<String>) -> String {
    value.unwrap_or_else(|| "<none>".to_string())
}

fn init_logger() {
    let mut builder = env_logger::Builder::from_env(Env::default().default_filter_or("info"));
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

fn print_banner() {
    for (mentis, db) in MENTIS_BANNER.lines().zip(DB_BANNER.lines()) {
        println!("{GREEN}{mentis}{RESET} {PINK}{db}{RESET}");
    }
}

fn progress_bar(current: usize, total: usize) -> String {
    let total = total.max(1);
    let current = current.min(total);
    let filled = ((current * 20) / total).min(20);
    format!(
        "[{}{}] {}/{}",
        "#".repeat(filled),
        "-".repeat(20 - filled),
        current,
        total
    )
}

fn print_endpoint_catalog(handles: &MentisDbServerHandles) {
    println!();
    println!("Endpoints:");
    println!("  MCP");
    println!("    POST http://{}", handles.mcp.local_addr());
    println!("      Standard streamable HTTP MCP root endpoint.");
    println!("    GET  http://{}/health", handles.mcp.local_addr());
    println!("      Health check for the MCP surface.");
    println!("    POST http://{}/tools/list", handles.mcp.local_addr());
    println!("      Legacy CloudLLM-compatible MCP tool discovery.");
    println!("    POST http://{}/tools/execute", handles.mcp.local_addr());
    println!("      Legacy CloudLLM-compatible MCP tool execution.");
    println!("  REST");
    println!("    GET  http://{}/health", handles.rest.local_addr());
    println!("      Health check for the REST surface.");
    println!("    GET  http://{}/v1/chains", handles.rest.local_addr());
    println!("      List chains with version, adapter, counts, and storage location.");
    println!("    POST http://{}/v1/agents", handles.rest.local_addr());
    println!("      List agent identity summaries for one chain.");
    println!("    POST http://{}/v1/agent", handles.rest.local_addr());
    println!("      Return one full agent registry record.");
    println!(
        "    POST http://{}/v1/agent-registry",
        handles.rest.local_addr()
    );
    println!("      Return the full agent registry for one chain.");
    println!(
        "    POST http://{}/v1/agents/upsert",
        handles.rest.local_addr()
    );
    println!("      Create or update an agent registry record.");
    println!(
        "    POST http://{}/v1/agents/description",
        handles.rest.local_addr()
    );
    println!("      Set or clear one agent description.");
    println!(
        "    POST http://{}/v1/agents/aliases",
        handles.rest.local_addr()
    );
    println!("      Add one alias to a registered agent.");
    println!(
        "    POST http://{}/v1/agents/keys",
        handles.rest.local_addr()
    );
    println!("      Add or replace one agent public key.");
    println!(
        "    POST http://{}/v1/agents/keys/revoke",
        handles.rest.local_addr()
    );
    println!("      Revoke one agent public key.");
    println!(
        "    POST http://{}/v1/agents/disable",
        handles.rest.local_addr()
    );
    println!("      Disable one registered agent.");
    println!(
        "    GET  http://{}/mentisdb_skill_md",
        handles.rest.local_addr()
    );
    println!("      Return the embedded official MentisDB skill Markdown.");
    println!("    GET  http://{}/v1/skills", handles.rest.local_addr());
    println!("      List uploaded skill summaries from the registry.");
    println!(
        "    GET  http://{}/v1/skills/manifest",
        handles.rest.local_addr()
    );
    println!("      Describe searchable fields and supported skill formats.");
    println!(
        "    POST http://{}/v1/skills/upload",
        handles.rest.local_addr()
    );
    println!("      Upload a new immutable skill version.");
    println!(
        "    POST http://{}/v1/skills/search",
        handles.rest.local_addr()
    );
    println!("      Search skills by metadata, uploader identity, and time window.");
    println!(
        "    POST http://{}/v1/skills/read",
        handles.rest.local_addr()
    );
    println!("      Read one stored skill as Markdown or JSON with safety warnings.");
    println!(
        "    POST http://{}/v1/skills/versions",
        handles.rest.local_addr()
    );
    println!("      List immutable uploaded versions for one skill.");
    println!(
        "    POST http://{}/v1/skills/deprecate",
        handles.rest.local_addr()
    );
    println!("      Mark one skill as deprecated.");
    println!(
        "    POST http://{}/v1/skills/revoke",
        handles.rest.local_addr()
    );
    println!("      Mark one skill as revoked.");
    println!("    POST http://{}/v1/bootstrap", handles.rest.local_addr());
    println!("      Bootstrap an empty chain with an initial checkpoint.");
    println!("    POST http://{}/v1/thoughts", handles.rest.local_addr());
    println!("      Append a durable thought.");
    println!(
        "    POST http://{}/v1/retrospectives",
        handles.rest.local_addr()
    );
    println!("      Append a retrospective thought.");
    println!("    POST http://{}/v1/search", handles.rest.local_addr());
    println!("      Search thoughts by semantic and identity filters.");
    println!(
        "    POST http://{}/v1/recent-context",
        handles.rest.local_addr()
    );
    println!("      Render a recent-context prompt snippet.");
    println!(
        "    POST http://{}/v1/memory-markdown",
        handles.rest.local_addr()
    );
    println!("      Export a MEMORY.md-style markdown view.");
    println!("    POST http://{}/v1/thought", handles.rest.local_addr());
    println!("      Read one thought by id, hash, or append-order index.");
    println!(
        "    POST http://{}/v1/thoughts/genesis",
        handles.rest.local_addr()
    );
    println!("      Return the first thought in append order.");
    println!(
        "    POST http://{}/v1/thoughts/traverse",
        handles.rest.local_addr()
    );
    println!("      Traverse thoughts forward or backward in filtered chunks.");
    println!("    POST http://{}/v1/head", handles.rest.local_addr());
    println!("      Return the latest thought at the chain tip and head metadata.");
    println!();

    if let Some(https_mcp) = &handles.https_mcp {
        println!("  HTTPS MCP");
        println!("    POST https://{}", https_mcp.local_addr());
        println!("      Streamable HTTP MCP root endpoint over TLS.");
        println!("    GET  https://{}/health", https_mcp.local_addr());
        println!("      Health check for the HTTPS MCP surface.");
        println!("    POST https://{}/tools/list", https_mcp.local_addr());
        println!("      Legacy CloudLLM-compatible MCP tool discovery (HTTPS).");
        println!("    POST https://{}/tools/execute", https_mcp.local_addr());
        println!("      Legacy CloudLLM-compatible MCP tool execution (HTTPS).");
    }
    if let Some(https_rest) = &handles.https_rest {
        println!("  HTTPS REST");
        println!("    GET  https://{}/health", https_rest.local_addr());
        println!("      Health check for the HTTPS REST surface.");
        println!("    GET  https://{}/v1/chains", https_rest.local_addr());
        println!("      List chains with version, adapter, counts, and storage location.");
        println!("    POST https://{}/v1/agents", https_rest.local_addr());
        println!("      List agent identity summaries for one chain.");
        println!("    POST https://{}/v1/agent", https_rest.local_addr());
        println!("      Return one full agent registry record.");
        println!(
            "    POST https://{}/v1/agent-registry",
            https_rest.local_addr()
        );
        println!("      Return the full agent registry for one chain.");
        println!(
            "    POST https://{}/v1/agents/upsert",
            https_rest.local_addr()
        );
        println!("      Create or update an agent registry record.");
        println!(
            "    POST https://{}/v1/agents/description",
            https_rest.local_addr()
        );
        println!("      Set or clear one agent description.");
        println!(
            "    POST https://{}/v1/agents/aliases",
            https_rest.local_addr()
        );
        println!("      Add one alias to a registered agent.");
        println!(
            "    POST https://{}/v1/agents/keys",
            https_rest.local_addr()
        );
        println!("      Add or replace one agent public key.");
        println!(
            "    POST https://{}/v1/agents/keys/revoke",
            https_rest.local_addr()
        );
        println!("      Revoke one agent public key.");
        println!(
            "    POST https://{}/v1/agents/disable",
            https_rest.local_addr()
        );
        println!("      Disable one registered agent.");
        println!(
            "    GET  https://{}/mentisdb_skill_md",
            https_rest.local_addr()
        );
        println!("      Return the embedded official MentisDB skill Markdown.");
        println!("    GET  https://{}/v1/skills", https_rest.local_addr());
        println!("      List uploaded skill summaries from the registry.");
        println!(
            "    GET  https://{}/v1/skills/manifest",
            https_rest.local_addr()
        );
        println!("      Describe searchable fields and supported skill formats.");
        println!(
            "    POST https://{}/v1/skills/upload",
            https_rest.local_addr()
        );
        println!("      Upload a new immutable skill version.");
        println!(
            "    POST https://{}/v1/skills/search",
            https_rest.local_addr()
        );
        println!("      Search skills by metadata, uploader identity, and time window.");
        println!(
            "    POST https://{}/v1/skills/read",
            https_rest.local_addr()
        );
        println!("      Read one stored skill as Markdown or JSON with safety warnings.");
        println!(
            "    POST https://{}/v1/skills/versions",
            https_rest.local_addr()
        );
        println!("      List immutable uploaded versions for one skill.");
        println!(
            "    POST https://{}/v1/skills/deprecate",
            https_rest.local_addr()
        );
        println!("      Mark one skill as deprecated.");
        println!(
            "    POST https://{}/v1/skills/revoke",
            https_rest.local_addr()
        );
        println!("      Mark one skill as revoked.");
        println!("    POST https://{}/v1/bootstrap", https_rest.local_addr());
        println!("      Bootstrap an empty chain with an initial checkpoint.");
        println!("    POST https://{}/v1/thoughts", https_rest.local_addr());
        println!("      Append a durable thought.");
        println!(
            "    POST https://{}/v1/retrospectives",
            https_rest.local_addr()
        );
        println!("      Append a retrospective thought.");
        println!("    POST https://{}/v1/search", https_rest.local_addr());
        println!("      Search thoughts by semantic and identity filters.");
        println!(
            "    POST https://{}/v1/recent-context",
            https_rest.local_addr()
        );
        println!("      Render a recent-context prompt snippet.");
        println!(
            "    POST https://{}/v1/memory-markdown",
            https_rest.local_addr()
        );
        println!("      Export a MEMORY.md-style markdown view.");
        println!("    POST https://{}/v1/thought", https_rest.local_addr());
        println!("      Read one thought by id, hash, or append-order index.");
        println!(
            "    POST https://{}/v1/thoughts/genesis",
            https_rest.local_addr()
        );
        println!("      Return the first thought in append order.");
        println!(
            "    POST https://{}/v1/thoughts/traverse",
            https_rest.local_addr()
        );
        println!("      Traverse thoughts forward or backward in filtered chunks.");
        println!("    POST https://{}/v1/head", https_rest.local_addr());
        println!("      Return the latest thought at the chain tip and head metadata.");
        println!();
    }
}

// ── ASCII table renderer ───────────────────────────────────────────────────────

/// Renders a bordered ASCII table to stdout.
///
/// `title`   – printed as a bold header above the table (pass `""` to skip).  
/// `headers` – column header strings.  
/// `rows`    – each inner `Vec<String>` is one data row; must match `headers` length.
///
/// Produces output like:
/// ```text
/// ┌──────────────┬─────────┬──────────┐
/// │  Chain Key   │ Version │ Thoughts │
/// ├──────────────┼─────────┼──────────┤
/// │ borganism-.. │    1    │   177    │
/// └──────────────┴─────────┴──────────┘
/// ```
fn ascii_table(title: &str, headers: &[&str], rows: &[Vec<String>]) {
    // Compute column widths (max of header vs every cell, plus 2-char padding).
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Box-drawing helpers.
    let bar = |left: &str, fill: &str, sep: &str, right: &str| {
        let mut s = left.to_string();
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&fill.repeat(w + 2));
            s.push_str(if i + 1 < ncols { sep } else { right });
        }
        s
    };

    let top = bar("┌", "─", "┬", "┐");
    let mid = bar("├", "─", "┼", "┤");
    let bottom = bar("└", "─", "┴", "┘");

    let fmt_row = |cells: &[String]| {
        let mut s = "│".to_string();
        for (i, cell) in cells.iter().enumerate() {
            if i < ncols {
                s.push_str(&format!(" {:<width$} │", cell, width = widths[i]));
            }
        }
        s
    };

    let fmt_header = |cells: &[&str]| {
        let mut s = "│".to_string();
        for (i, cell) in cells.iter().enumerate() {
            if i < ncols {
                // Headers are bold/cyan.
                s.push_str(&format!(
                    " {CYAN}{:<width$}{RESET} │",
                    cell,
                    width = widths[i]
                ));
            }
        }
        s
    };

    if !title.is_empty() {
        println!("{YELLOW}{title}{RESET}");
    }
    println!("{DIM}{top}{RESET}");
    println!("{}", fmt_header(headers));
    println!("{DIM}{mid}{RESET}");
    for row in rows {
        println!("{}", fmt_row(row));
    }
    println!("{DIM}{bottom}{RESET}");
    println!();
}

/// Like `ascii_table` but inserts a full-width "section" separator row
/// (e.g. a chain name) to group subsequent rows under it.
///
/// `sections` is a slice of `(section_label, rows_for_that_section)`.
fn ascii_table_grouped(title: &str, headers: &[&str], sections: &[(String, Vec<Vec<String>>)]) {
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for (label, rows) in sections {
        // The section label spans the full table width; we account for it
        // separately after we know all column widths.
        let _ = label;
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < ncols {
                    widths[i] = widths[i].max(cell.len());
                }
            }
        }
    }

    // Total inner width (columns + separators) for a full-span label row.
    let total_inner: usize = widths.iter().sum::<usize>() + ncols * 3 - 1;
    // Ensure each section label fits.
    // (We'll truncate labels that are too long rather than widen the table.)

    let bar = |left: &str, fill: &str, sep: &str, right: &str| {
        let mut s = left.to_string();
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&fill.repeat(w + 2));
            s.push_str(if i + 1 < ncols { sep } else { right });
        }
        s
    };

    let section_bar = |left: &str, fill: &str, right: &str| {
        format!("{}{}{}", left, fill.repeat(total_inner), right)
    };

    let top = bar("┌", "─", "┬", "┐");
    let mid = bar("├", "─", "┼", "┤");
    let bottom = bar("└", "─", "┴", "┘");
    let sec_mid = section_bar("├", "─", "┤");
    let sec_mid2 = bar("├", "─", "┼", "┤");

    let fmt_row = |cells: &[String]| {
        let mut s = "│".to_string();
        for (i, cell) in cells.iter().enumerate() {
            if i < ncols {
                s.push_str(&format!(" {:<width$} │", cell, width = widths[i]));
            }
        }
        s
    };

    let fmt_header = |cells: &[&str]| {
        let mut s = "│".to_string();
        for (i, cell) in cells.iter().enumerate() {
            if i < ncols {
                s.push_str(&format!(
                    " {CYAN}{:<width$}{RESET} │",
                    cell,
                    width = widths[i]
                ));
            }
        }
        s
    };

    let fmt_section_label = |label: &str| {
        let label = if label.len() > total_inner {
            format!("{}…", &label[..total_inner.saturating_sub(1)])
        } else {
            label.to_string()
        };
        format!(
            "│ {PINK}{:<width$}{RESET} │",
            label,
            width = total_inner - 2
        )
    };

    if !title.is_empty() {
        println!("{YELLOW}{title}{RESET}");
    }
    println!("{DIM}{top}{RESET}");
    println!("{}", fmt_header(headers));

    for (s_idx, (label, rows)) in sections.iter().enumerate() {
        println!("{DIM}{}{RESET}", if s_idx == 0 { &mid } else { &sec_mid2 });
        println!("{DIM}{sec_mid}{RESET}");
        println!("{}", fmt_section_label(label));
        println!("{DIM}{sec_mid}{RESET}");
        for row in rows {
            println!("{}", fmt_row(row));
        }
    }

    println!("{DIM}{bottom}{RESET}");
    println!();
}

fn print_chain_summary(
    config: &MentisDbServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let registry = load_registered_chains(&config.service.chain_dir)?;
    if registry.chains.is_empty() {
        println!("{YELLOW}Chain Summary{RESET}");
        println!("  No registered chains.\n");
        return Ok(());
    }

    let headers = &[
        "Chain Key",
        "Ver",
        "Adapter",
        "Thoughts",
        "Agents",
        "Storage Location",
    ];
    // `refresh_registered_chain_counts` has already run before servers start and
    // written live thought/agent counts to the registry.  Read directly from
    // that refreshed registry — no need to re-open every chain file here.
    let rows: Vec<Vec<String>> = registry
        .chains
        .values()
        .map(|e| {
            vec![
                e.chain_key.clone(),
                e.version.to_string(),
                e.storage_adapter.to_string(),
                e.thought_count.to_string(),
                e.agent_count.to_string(),
                e.storage_location.clone(),
            ]
        })
        .collect();

    ascii_table("Chain Summary", headers, &rows);
    Ok(())
}

fn print_agent_registry_summary(
    config: &MentisDbServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let registry = load_registered_chains(&config.service.chain_dir)?;
    if registry.chains.is_empty() {
        println!("{YELLOW}Agent Registry{RESET}");
        println!("  No registered chains.\n");
        return Ok(());
    }

    let headers = &["Name", "ID", "Status", "Memories", "Description"];
    let mut sections: Vec<(String, Vec<Vec<String>>)> = Vec::new();

    for entry in registry.chains.values() {
        match MentisDb::open_with_storage(
            entry
                .storage_adapter
                .for_chain_key(&config.service.chain_dir, &entry.chain_key),
        ) {
            Ok(chain) => {
                let agents = chain.list_agent_registry();
                if agents.is_empty() {
                    continue;
                }
                let thoughts = chain.thoughts();
                let rows: Vec<Vec<String>> = agents
                    .into_iter()
                    .map(|agent| {
                        let live_count = thoughts
                            .iter()
                            .filter(|t| t.agent_id == agent.agent_id)
                            .count();
                        let desc = agent
                            .description
                            .as_deref()
                            .filter(|v| !v.trim().is_empty())
                            .unwrap_or("—");
                        let desc = if desc.len() > 60 {
                            format!("{}…", &desc[..59])
                        } else {
                            desc.to_string()
                        };
                        vec![
                            agent.display_name.clone(),
                            agent.agent_id.clone(),
                            agent.status.to_string(),
                            live_count.to_string(),
                            desc,
                        ]
                    })
                    .collect();
                sections.push((entry.chain_key.clone(), rows));
            }
            Err(error) => {
                sections.push((
                    entry.chain_key.clone(),
                    vec![vec![
                        format!("error: {error}"),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]],
                ));
            }
        }
    }

    if sections.is_empty() {
        println!("{YELLOW}Agent Registry{RESET}");
        println!("  No agents registered.\n");
    } else {
        ascii_table_grouped("Agent Registry", headers, &sections);
    }
    Ok(())
}

fn print_skill_registry_summary(
    config: &MentisDbServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match SkillRegistry::open(&config.service.chain_dir) {
        Ok(registry) => {
            let skills = registry.list_skills();
            if skills.is_empty() {
                println!("{YELLOW}Skill Registry{RESET}");
                println!("  No skills registered.\n");
                return Ok(());
            }
            let headers = &["Name", "Status", "Versions", "Tags", "Uploaded By"];
            let rows: Vec<Vec<String>> = skills
                .iter()
                .map(|skill| {
                    vec![
                        skill.name.clone(),
                        format!("{:?}", skill.status),
                        skill.version_count.to_string(),
                        if skill.tags.is_empty() {
                            "—".to_string()
                        } else {
                            skill.tags.join(", ")
                        },
                        skill.latest_uploaded_by_agent_id.clone(),
                    ]
                })
                .collect();
            ascii_table("Skill Registry", headers, &rows);
        }
        Err(_) => {
            println!("{YELLOW}Skill Registry{RESET}");
            println!("  No skill registry found.\n");
        }
    }
    Ok(())
}

/// Prints TLS certificate trust instructions and the `my.mentisdb.com` tip,
/// but only when at least one HTTPS listener is active.
///
/// `my.mentisdb.com` is a public DNS A-record that resolves to `127.0.0.1`,
/// providing a human-friendly hostname for the local daemon once the
/// self-signed certificate has been trusted.
fn print_tls_tip(config: &MentisDbServerConfig, handles: &MentisDbServerHandles) {
    if handles.https_mcp.is_none() && handles.https_rest.is_none() {
        return;
    }

    let mcp_port = handles.https_mcp.as_ref().map(|h| h.local_addr().port());
    let rest_port = handles.https_rest.as_ref().map(|h| h.local_addr().port());

    println!("TLS Certificate: {}", config.tls_cert_path.display());
    println!();
    println!("  {YELLOW}my.mentisdb.com{RESET} is a public DNS A-record \u{2192} 127.0.0.1");
    println!("  You can use it as a friendly hostname for this local daemon.");
    if let Some(port) = mcp_port {
        println!("  MCP:  https://my.mentisdb.com:{port}");
    }
    if let Some(port) = rest_port {
        println!("  REST: https://my.mentisdb.com:{port}");
    }
    println!();
    println!("  To avoid certificate warnings, trust the self-signed cert once:");
    println!("  {GREEN}macOS{RESET}:   sudo security add-trusted-cert -d -r trustRoot \\");
    println!("             -k /Library/Keychains/System.keychain \\");
    println!("             {}", config.tls_cert_path.display());
    println!(
        "  {GREEN}Linux{RESET}:   sudo cp {} /usr/local/share/ca-certificates/mentisdb.crt",
        config.tls_cert_path.display()
    );
    println!("           sudo update-ca-certificates");
    println!(
        "  {GREEN}Windows{RESET}: certutil -addstore Root {}",
        config.tls_cert_path.display()
    );
    println!();
}
