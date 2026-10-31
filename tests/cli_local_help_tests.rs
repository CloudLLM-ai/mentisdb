//! Help standard for the local (non-daemon) commands: every command has usage,
//! a description, examples, and documents every flag its parser accepts.

use mentisdb::cli::run_with_io;
use std::io::Cursor;

fn help_of(command: &str) -> String {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with_io(
        ["mentisdb", command, "--help"],
        &mut Cursor::new(""),
        &mut out,
        &mut err,
    );
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "{command}"
    );
    String::from_utf8(out).unwrap()
}

/// Flags each local parser accepts (from `src/cli/args.rs`), by command.
const FLAGS: &[(&str, &[&str])] = &[
    ("setup", &["--url", "--dry-run", "--yes"]),
    ("wizard", &["--url", "--yes", "--bearer", "--token"]),
    (
        "add",
        &["--type", "--scope", "--tag", "--agent", "--chain", "--url"],
    ),
    (
        "backup",
        &["-o", "--output", "--dir", "--flush", "--include-tls"],
    ),
    ("restore", &["--dir", "--overwrite", "--yes"]),
    (
        "bearertoken",
        &[
            "create", "list", "revoke", "remove", "--global", "--chain", "--dir",
        ],
    ),
    (
        "cert",
        &[
            "--force",
            "--reset",
            "--out-dir",
            "--env-file",
            "--no-env-update",
        ],
    ),
    ("key", &["generate", "show", "--out", "--force"]),
];

/// Every local command's help names its usage and examples, and every flag it parses.
#[test]
fn test_local_commands_document_usage_examples_and_every_flag() {
    for (command, flags) in FLAGS {
        let help = help_of(command);
        assert!(help.contains("Usage:"), "{command}: no Usage section");
        assert!(
            help.to_lowercase().contains("example"),
            "{command}: no examples"
        );
        assert!(
            help.contains(&format!("mentisdb {command}")),
            "{command}: examples/usage must show the command"
        );
        for flag in *flags {
            assert!(
                help.contains(flag),
                "{command}: help does not document '{flag}'"
            );
        }
    }
}

/// The top-level help lists every local command.
#[test]
fn test_overview_lists_every_local_command() {
    let overview = mentisdb::cli::help_text();
    for (command, _) in FLAGS {
        assert!(
            overview.contains(&format!("mentisdb {command}")),
            "{command} missing from mentisdb --help"
        );
    }
}
