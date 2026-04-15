//! Shared CLI helpers for `mentisdbd` setup, wizard, and memory subcommands.
//!
//! The daemon binary delegates subcommand parsing plus wizard/setup behavior to
//! this module so the command logic stays directly testable.

mod args;
mod prompt;
mod setup;
mod wizard;

use crate::backup::{
    create_backup, list_backup_contents, restore_backup, BackupOptions, RestoreOptions,
};
use crate::paths::default_mentisdb_dir;

pub use args::{
    parse_args, AddCommand, AgentsCommand, BackupCommand, CliCommand, RestoreCommand,
    SearchCommand, SetupCommand, WizardCommand,
};
pub use prompt::{boxed_apply_summary, boxed_skip_notice, boxed_text_prompt, boxed_yn_prompt};
pub use setup::{parse_node_major, render_setup_plan};

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Run the embedded CLI with caller-provided streams.
pub fn run_with_io<I, T>(
    args: I,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    match parse_args(args) {
        Ok(CliCommand::Help) => {
            let _ = write!(out, "{}", args::help_text());
            ExitCode::SUCCESS
        }
        Ok(CliCommand::Setup(command)) => match setup::run_setup(&command, input, out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "setup failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Wizard(command)) => match wizard::run_wizard(&command, input, out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "wizard failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Add(command)) => match run_add(&command, out, err) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "add failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Search(command)) => match run_search(&command, out, err) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "search failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Agents(command)) => match run_agents(&command, out, err) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "agents failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Backup(command)) => match run_backup(&command, out, err) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "backup failed: {error}");
                ExitCode::from(1)
            }
        },
        Ok(CliCommand::Restore(command)) => match run_restore(&command, input, out, err) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "restore failed: {error}");
                ExitCode::from(1)
            }
        },
        Err(message) => {
            let _ = writeln!(err, "{message}");
            let _ = writeln!(err);
            let _ = write!(err, "{}", args::help_text());
            ExitCode::from(2)
        }
    }
}

fn run_add(cmd: &AddCommand, out: &mut dyn Write, _err: &mut dyn Write) -> Result<(), String> {
    let mut body = serde_json::Map::new();
    body.insert(
        "content".to_string(),
        serde_json::Value::String(cmd.content.clone()),
    );
    if let Some(ref thought_type) = cmd.thought_type {
        body.insert(
            "thought_type".to_string(),
            serde_json::Value::String(thought_type.clone()),
        );
    }
    if let Some(ref scope) = cmd.scope {
        body.insert(
            "scope".to_string(),
            serde_json::Value::String(scope.clone()),
        );
    }
    if !cmd.tags.is_empty() {
        body.insert(
            "tags".to_string(),
            serde_json::Value::Array(
                cmd.tags
                    .iter()
                    .map(|t| serde_json::Value::String(t.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(ref agent_id) = cmd.agent_id {
        body.insert(
            "agent_id".to_string(),
            serde_json::Value::String(agent_id.clone()),
        );
    }
    if let Some(ref chain_key) = cmd.chain_key {
        body.insert(
            "chain_key".to_string(),
            serde_json::Value::String(chain_key.clone()),
        );
    }
    let url = format!("{}/v1/thoughts", cmd.url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .send_json(serde_json::Value::Object(body))
        .map_err(|e| format!("POST {url}: {e}"))?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("parse response: {e}"))?;
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
    Ok(())
}

fn run_search(
    cmd: &SearchCommand,
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> Result<(), String> {
    let mut body = serde_json::Map::new();
    body.insert(
        "text".to_string(),
        serde_json::Value::String(cmd.text.clone()),
    );
    if let Some(limit) = cmd.limit {
        body.insert("limit".to_string(), serde_json::Value::Number(limit.into()));
    }
    if let Some(ref scope) = cmd.scope {
        body.insert(
            "scope".to_string(),
            serde_json::Value::String(scope.clone()),
        );
    }
    if let Some(ref chain_key) = cmd.chain_key {
        body.insert(
            "chain_key".to_string(),
            serde_json::Value::String(chain_key.clone()),
        );
    }
    let url = format!("{}/v1/ranked-search", cmd.url.trim_end_matches('/'));
    let response = ureq::post(&url)
        .send_json(serde_json::Value::Object(body))
        .map_err(|e| format!("POST {url}: {e}"))?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("parse response: {e}"))?;
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
    Ok(())
}

fn run_agents(
    cmd: &AgentsCommand,
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> Result<(), String> {
    let mut url = format!("{}/v1/agents", cmd.url.trim_end_matches('/'));
    if let Some(ref chain_key) = cmd.chain_key {
        url = format!("{url}?chain_key={chain_key}");
    }
    let response = ureq::get(&url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("parse response: {e}"))?;
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
    Ok(())
}

fn run_backup(
    cmd: &BackupCommand,
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> Result<(), String> {
    let source_dir = cmd
        .source_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_mentisdb_dir);

    if !source_dir.exists() {
        return Err(format!(
            "source directory does not exist: {}",
            source_dir.display()
        ));
    }

    // Try to flush the running daemon first. If connection is refused, the
    // daemon is not running — skip flush and proceed with whatever is on disk.
    match ureq::post("http://127.0.0.1:9472/v1/admin/flush").call() {
        Ok(resp) if resp.status() == 200 => {
            let _ = writeln!(out, "Daemon detected — chains flushed.");
        }
        Err(e) if e.kind() == ureq::ErrorKind::ConnectionFailed => {
            let _ = writeln!(out, "Daemon not running — capturing files as-is.");
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "Warning: could not flush daemon: {e} — proceeding anyway."
            );
        }
        Ok(resp) => {
            let _ = writeln!(
                out,
                "Warning: unexpected flush response status {}",
                resp.status()
            );
        }
    }

    let output_path = cmd.output_path.as_ref().map(PathBuf::from);

    let options = BackupOptions {
        source_dir,
        output_path,
        flush_before_backup: cmd.flush,
        include_tls: cmd.include_tls,
    };

    let manifest = create_backup(&options).map_err(|e| format!("create_backup: {e}"))?;
    let output = options
        .output_path
        .unwrap_or_else(|| PathBuf::from(crate::backup::generate_backup_filename()));

    writeln!(out, "Backup created: {}", output.display()).map_err(|e| e.to_string())?;
    writeln!(
        out,
        "  {} files, {} bytes total, {} chains",
        manifest.files.len(),
        manifest.total_uncompressed_bytes,
        manifest.chain_count
    )
    .map_err(|e| e.to_string())?;
    writeln!(out, "  mentisdb version: {}", manifest.mentisdb_version)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn run_restore(
    cmd: &RestoreCommand,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> Result<(), String> {
    let archive_path = PathBuf::from(&cmd.archive_path);
    if !archive_path.exists() {
        return Err(format!(
            "backup archive not found: {}",
            archive_path.display()
        ));
    }

    let target_dir = cmd
        .target_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_mentisdb_dir);

    let files = list_backup_contents(archive_path.clone())
        .map_err(|e| format!("list_backup_contents: {e}"))?;

    let required_count = files.iter().filter(|f| f.required).count();
    let optional_count = files.len() - required_count;

    writeln!(out, "Restore archive: {}", archive_path.display()).map_err(|e| e.to_string())?;
    writeln!(
        out,
        "  {} files ({} required, {} optional)",
        files.len(),
        required_count,
        optional_count
    )
    .map_err(|e| e.to_string())?;
    writeln!(out, "  Target directory: {}", target_dir.display()).map_err(|e| e.to_string())?;

    if cmd.overwrite {
        writeln!(out, "  Mode: overwrite existing files").map_err(|e| e.to_string())?;
    } else {
        writeln!(out, "  Mode: preserve existing files (idempotent)").map_err(|e| e.to_string())?;
    }
    writeln!(out).map_err(|e| e.to_string())?;

    // Interactive prompt if there are conflicting files and --overwrite not passed
    if !cmd.overwrite && !cmd.yes {
        let existing: Vec<&str> = files
            .iter()
            .filter(|f| target_dir.join(&f.relative_path).exists())
            .map(|f| f.relative_path.as_str())
            .collect();

        if !existing.is_empty() {
            let conflict_list = existing.join("\n  ");
            let question = format!(
                "The following files already exist in the target directory:\n\n  {}\n\nOverwrite existing files?",
                conflict_list
            );
            let answer =
                boxed_yn_prompt(out, &question, false, input).map_err(|e| e.to_string())?;
            if answer.is_empty() || answer.to_ascii_lowercase().starts_with('n') {
                let _ = writeln!(out, "Restore cancelled — no files were modified.");
                return Ok(());
            }
        }
    }

    restore_backup(
        archive_path,
        target_dir.clone(),
        RestoreOptions {
            overwrite: cmd.overwrite,
        },
    )
    .map_err(|e| format!("restore_backup: {e}"))?;

    writeln!(out, "Restore complete.").map_err(|e| e.to_string())?;
    Ok(())
}
