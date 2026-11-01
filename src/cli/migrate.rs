//! `mentisdb migrate`: inspect, run, verify, retry, skip and roll back chain
//! format migrations from the command line.
//!
//! Everything here works directly on a storage directory and takes the same
//! per-chain lock the daemon takes, so it is safe to run next to a live
//! daemon: a chain the daemon is migrating reports `busy` instead of being
//! touched twice. `status --url` asks a running daemon instead.

use crate::migration::{
    detect_migration, discover_chain_keys, dry_run_chain, purge_backups, retry_chain,
    rollback_chain, run_chain_migration, skip_chain, verify_chain_file, JournalState,
    MigrationError, MigrationOptions, MigrationOutcome, MigrationPaths, MigrationProgress,
};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Help text for `mentisdb migrate`.
pub const MIGRATE_HELP: &str = "\
mentisdb migrate - inspect and drive chain format migrations

A new release migrates old chains in the background while the daemon serves.
Each migration backs up the original, is journaled, and survives a kill at any
point. These commands show what is happening and let you intervene.

Usage:
  mentisdb migrate status   [--dir <path>] [--json] [--url <rest-url>]
  mentisdb migrate run      [--dir <path>] [--chain <key>] [--dry-run]
  mentisdb migrate verify   [--dir <path>] [--chain <key>]
  mentisdb migrate retry    --chain <key> [--dir <path>]
  mentisdb migrate skip     --chain <key> [--reason <text>] [--dir <path>]
  mentisdb migrate rollback --chain <key> [--force] [--dir <path>]
  mentisdb migrate purge-backups [--older-than-days <n>] [--dir <path>]

Commands:
  status         List every chain that needs, is in, or finished a migration,
                 with progress and errors. --url asks a running daemon.
  run            Migrate one chain (--chain) or all of them now, resuming any
                 interrupted run. --dry-run rehearses: it checks hashes and
                 signatures and reports what would change, writing nothing.
  verify         Re-read each current chain file and check every hash link.
  retry          Clear a quarantine (failed, skipped or rolled-back chain) so
                 it is migrated again.
  skip           Leave a chain untouched and unavailable until retried.
  rollback       Restore the pre-migration backup. Refused when thoughts were
                 appended since, unless --force (which discards them).
  purge-backups  Delete preserved originals of finished migrations (all, or
                 older than --older-than-days).

Options:
  --dir <path>   Storage directory (default: MENTISDB_DIR or the default dir)
  --chain <key>  Chain key
  --json         Machine-readable status
  --url <url>    REST base URL of a running daemon, for status
  --dry-run      Rehearse; write nothing
  --force        Allow a rollback that discards newer thoughts
  -h, --help     Show this help

Exit status is 1 when any chain failed, 0 otherwise.

Environment:
  MENTISDB_MIGRATION_CONCURRENCY             chains migrated at once (default 2)
  MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS   keep originals this long (default 14, 0 = forever)

Examples:
  mentisdb migrate status
  mentisdb migrate run --dry-run
  mentisdb migrate rollback --chain my-project
";

/// A parsed `mentisdb migrate ...` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateCommand {
    /// List migration state.
    Status {
        /// Storage directory override.
        dir: Option<String>,
        /// Emit JSON.
        json: bool,
        /// Ask a running daemon instead.
        url: Option<String>,
    },
    /// Migrate now.
    Run {
        /// Storage directory override.
        dir: Option<String>,
        /// Only this chain.
        chain: Option<String>,
        /// Rehearse only.
        dry_run: bool,
    },
    /// Verify chain files.
    Verify {
        /// Storage directory override.
        dir: Option<String>,
        /// Only this chain.
        chain: Option<String>,
    },
    /// Clear a quarantine.
    Retry {
        /// Storage directory override.
        dir: Option<String>,
        /// The chain.
        chain: String,
    },
    /// Park a chain.
    Skip {
        /// Storage directory override.
        dir: Option<String>,
        /// The chain.
        chain: String,
        /// Why.
        reason: String,
    },
    /// Restore the backup.
    Rollback {
        /// Storage directory override.
        dir: Option<String>,
        /// The chain.
        chain: String,
        /// Discard newer thoughts.
        force: bool,
    },
    /// Delete preserved originals.
    PurgeBackups {
        /// Storage directory override.
        dir: Option<String>,
        /// Only those older than this many days.
        older_than_days: Option<u64>,
    },
}

/// Parse the arguments after `migrate`.
///
/// # Errors
///
/// A message for an unknown subcommand or flag, or a missing value.
///
/// # Examples
///
/// ```
/// use mentisdb::cli::migrate::{parse_migrate_args, MigrateCommand};
///
/// let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
/// assert_eq!(
///     parse_migrate_args(&args(&["rollback", "--chain", "c", "--force"])).unwrap(),
///     MigrateCommand::Rollback { dir: None, chain: "c".into(), force: true }
/// );
/// assert!(parse_migrate_args(&args(&["retry"])).is_err());
/// assert!(parse_migrate_args(&args(&["explode"])).is_err());
/// ```
pub fn parse_migrate_args(args: &[String]) -> Result<MigrateCommand, String> {
    let Some((verb, flags)) = args.split_first() else {
        return Err("missing migrate subcommand (try `mentisdb migrate --help`)".to_owned());
    };
    let (mut dir, mut chain, mut json, mut url) = (None, None, false, None);
    let (mut dry_run, mut force, mut reason, mut days) = (false, false, None, None);
    let mut iter = flags.iter();
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--dir" => dir = Some(value("--dir")?),
            "--chain" => chain = Some(value("--chain")?),
            "--url" => url = Some(value("--url")?),
            "--reason" => reason = Some(value("--reason")?),
            "--older-than-days" => {
                days = Some(
                    value("--older-than-days")?
                        .parse::<u64>()
                        .map_err(|_| "--older-than-days needs a whole number".to_owned())?,
                )
            }
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "--force" => force = true,
            other => return Err(format!("unknown flag '{other}' for `migrate {verb}`")),
        }
    }
    let need_chain = |chain: Option<String>| {
        chain.ok_or_else(|| format!("`migrate {verb}` needs --chain <key>"))
    };
    match verb.as_str() {
        "status" => Ok(MigrateCommand::Status { dir, json, url }),
        "run" => Ok(MigrateCommand::Run {
            dir,
            chain,
            dry_run,
        }),
        "verify" => Ok(MigrateCommand::Verify { dir, chain }),
        "retry" => Ok(MigrateCommand::Retry {
            dir,
            chain: need_chain(chain)?,
        }),
        "skip" => Ok(MigrateCommand::Skip {
            dir,
            chain: need_chain(chain)?,
            reason: reason.unwrap_or_else(|| "operator request".to_owned()),
        }),
        "rollback" => Ok(MigrateCommand::Rollback {
            dir,
            chain: need_chain(chain)?,
            force,
        }),
        "purge-backups" => Ok(MigrateCommand::PurgeBackups {
            dir,
            older_than_days: days,
        }),
        other => Err(format!("unknown migrate subcommand '{other}'")),
    }
}

/// Execute a parsed command. Returns `Ok(true)` when everything succeeded and
/// `Ok(false)` when some chain failed (the caller exits non-zero).
///
/// # Errors
///
/// A message for failures that are not per-chain results (bad directory,
/// unreachable daemon, a refused rollback).
pub fn run_migrate(
    command: &MigrateCommand,
    default_dir: impl Fn(&Option<String>) -> PathBuf,
    out: &mut dyn Write,
) -> Result<bool, String> {
    let w = |e: std::io::Error| e.to_string();
    match command {
        MigrateCommand::Status { dir, json, url } => {
            if let Some(url) = url {
                return remote_status(url, *json, out);
            }
            let dir = default_dir(dir);
            let rows = local_status(&dir);
            if *json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?
                )
                .map_err(w)?;
            } else if rows.is_empty() {
                writeln!(out, "No chains need migration.").map_err(w)?;
            } else {
                for row in &rows {
                    writeln!(
                        out,
                        "{:<28} {:<12} {}",
                        row.chain_key, row.state, row.detail
                    )
                    .map_err(w)?;
                }
            }
            Ok(!rows.iter().any(|row| row.state == "failed"))
        }
        MigrateCommand::Run {
            dir,
            chain,
            dry_run,
        } => {
            let dir = default_dir(dir);
            let keys: Vec<String> = match chain {
                Some(key) => vec![key.clone()],
                None => discover_chain_keys(&dir).into_iter().collect(),
            };
            let mut all_ok = true;
            for key in keys {
                all_ok &= run_one(&dir, &key, *dry_run, out).map_err(w)?;
            }
            Ok(all_ok)
        }
        MigrateCommand::Verify { dir, chain } => {
            let dir = default_dir(dir);
            let keys: Vec<String> = match chain {
                Some(key) => vec![key.clone()],
                None => discover_chain_keys(&dir).into_iter().collect(),
            };
            let mut all_ok = true;
            for key in keys {
                let paths = MigrationPaths::new(&dir, &key);
                if !paths.live.exists() {
                    continue;
                }
                match verify_chain_file(&paths.live) {
                    Ok(report) => writeln!(
                        out,
                        "{key}: ok, {} thoughts, {} signed, head {}",
                        report.thoughts,
                        report.signed,
                        report.head_hash.get(..12).unwrap_or(&report.head_hash)
                    ),
                    Err(error) => {
                        all_ok = false;
                        writeln!(out, "{key}: FAILED: {error}")
                    }
                }
                .map_err(w)?;
            }
            Ok(all_ok)
        }
        MigrateCommand::Retry { dir, chain } => {
            retry_chain(&default_dir(dir), chain).map_err(|e| e.to_string())?;
            writeln!(
                out,
                "{chain}: quarantine cleared; it will be migrated on the next run"
            )
            .map_err(w)?;
            Ok(true)
        }
        MigrateCommand::Skip { dir, chain, reason } => {
            skip_chain(&default_dir(dir), chain, reason).map_err(|e| e.to_string())?;
            writeln!(
                out,
                "{chain}: skipped; it stays untouched until `migrate retry`"
            )
            .map_err(w)?;
            Ok(true)
        }
        MigrateCommand::Rollback { dir, chain, force } => {
            rollback_chain(&default_dir(dir), chain, *force).map_err(|e| e.to_string())?;
            writeln!(out, "{chain}: restored from the pre-migration backup").map_err(w)?;
            Ok(true)
        }
        MigrateCommand::PurgeBackups {
            dir,
            older_than_days,
        } => {
            let age = older_than_days.map(|days| Duration::from_secs(days * 86_400));
            let removed = purge_backups(&default_dir(dir), age).map_err(|e| e.to_string())?;
            writeln!(out, "removed {removed} backup(s)").map_err(w)?;
            Ok(true)
        }
    }
}

fn run_one(
    dir: &std::path::Path,
    key: &str,
    dry_run: bool,
    out: &mut dyn Write,
) -> std::io::Result<bool> {
    let options = MigrationOptions {
        failpoint: None,
        progress: Some(Arc::new(|_: &MigrationProgress| {})),
    };
    if dry_run {
        return match dry_run_chain(dir, key, &options) {
            Ok(None) => writeln!(out, "{key}: current, nothing to do").map(|()| true),
            Ok(Some(report)) => writeln!(
                out,
                "{key}: would migrate {} thoughts ({}), schema {} -> {}, hashes {}, {} signed; steps: {}",
                report.thoughts,
                report.reason.map(|r| r.to_string()).unwrap_or_default(),
                report.from_schema,
                report.to_schema,
                if report.hash_changed { "rebuilt" } else { "unchanged" },
                report.signed,
                report.steps.join(", ")
            )
            .map(|()| true),
            Err(error) => writeln!(out, "{key}: dry run FAILED: {error}").map(|()| false),
        };
    }
    match run_chain_migration(dir, key, &options) {
        Ok(MigrationOutcome::NotNeeded) => {
            writeln!(out, "{key}: current, nothing to do").map(|()| true)
        }
        Ok(MigrationOutcome::Migrated(report)) => writeln!(
            out,
            "{key}: migrated {} thoughts in {} ms (backup: {})",
            report.thoughts,
            report.elapsed_ms,
            report
                .backup_dir
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
        .map(|()| true),
        Ok(MigrationOutcome::Quarantined { state, error }) => writeln!(
            out,
            "{key}: quarantined ({state:?}): {}; `mentisdb migrate retry --chain {key}` clears it",
            error.unwrap_or_default()
        )
        .map(|()| false),
        Err(MigrationError::Busy { .. }) => {
            writeln!(out, "{key}: busy (a running daemon is migrating it)").map(|()| true)
        }
        Err(error) => writeln!(out, "{key}: FAILED: {error}").map(|()| false),
    }
}

/// One row of `mentisdb migrate status`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusRow {
    /// Chain key.
    pub chain_key: String,
    /// `pending`, `in-progress`, `completed`, `failed`, `skipped` or `rolled-back`.
    pub state: String,
    /// Reason, error or path, for humans.
    pub detail: String,
}

/// Compute status rows from the journals and a first-record look at each chain.
pub fn local_status(dir: &std::path::Path) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    for key in discover_chain_keys(dir) {
        let paths = MigrationPaths::new(dir, &key);
        let journal = crate::migration::MigrationJournal::load(&paths.journal)
            .ok()
            .flatten();
        let row = match journal {
            Some(j) if j.state == JournalState::Finalized => {
                // A finished run; list it only while its backup is kept.
                if paths.backup_dir.exists() {
                    Some((
                        "completed",
                        format!(
                            "{} thoughts; backup at {}",
                            j.thoughts_total,
                            paths.backup_dir.display()
                        ),
                    ))
                } else {
                    None
                }
            }
            Some(j) if j.state == JournalState::Failed => {
                Some(("failed", j.error.unwrap_or_default()))
            }
            Some(j) if j.state == JournalState::Skipped => {
                Some(("skipped", j.error.unwrap_or_default()))
            }
            Some(j) if j.state == JournalState::RolledBack => Some((
                "rolled-back",
                "restored from backup; `migrate retry` to migrate again".to_owned(),
            )),
            Some(j) => Some((
                "in-progress",
                format!("interrupted at {:?}, attempt {}", j.state, j.attempts),
            )),
            None => match detect_migration(dir, &key) {
                Ok(Some(reason)) => Some(("pending", reason.to_string())),
                Ok(None) => None,
                Err(error) => Some(("failed", error.to_string())),
            },
        };
        if let Some((state, detail)) = row {
            rows.push(StatusRow {
                chain_key: key,
                state: state.to_owned(),
                detail,
            });
        }
    }
    rows
}

fn remote_status(url: &str, json: bool, out: &mut dyn Write) -> Result<bool, String> {
    let endpoint = format!("{}/v1/migrations", url.trim_end_matches('/'));
    let body: serde_json::Value = ureq::get(&endpoint)
        .call()
        .map_err(|e| format!("could not reach {endpoint}: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("bad response from {endpoint}: {e}"))?;
    if json {
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        )
        .map_err(|e| e.to_string())?;
    } else {
        let empty = Vec::new();
        let chains = body["chains"].as_array().unwrap_or(&empty);
        if chains.is_empty() {
            writeln!(out, "No chains are migrating.").map_err(|e| e.to_string())?;
        }
        for chain in chains {
            writeln!(
                out,
                "{:<28} {:<10} {:>3}%  {}",
                chain["chain_key"].as_str().unwrap_or(""),
                chain["phase"].as_str().unwrap_or(""),
                chain["percent"].as_u64().unwrap_or(0),
                chain["error"].as_str().unwrap_or("")
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(!body["chains"]
        .as_array()
        .is_some_and(|chains| chains.iter().any(|c| c["phase"] == "failed")))
}
