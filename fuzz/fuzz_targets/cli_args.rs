#![no_main]
//! Fuzzes the CLI argument parser: whatever the shell hands `mentisdb`, parsing
//! either fails with a message or yields a well-formed request, and never panics.

use libfuzzer_sys::fuzz_target;
use mentisdb::cli::{parse_args, remote::HttpMethod, CliCommand};

/// Words that steer the fuzzer into real command paths and flags.
const WORDS: &[&str] = &[
    "thought", "append", "get", "search", "lexical", "chain", "branch", "agent", "key", "add",
    "skill", "diff", "daemon", "settings", "set", "--chain", "--type", "--limit", "--since",
    "--public-key-hex", "--yes", "--json", "--url", "--", "-", "=", "--help", "help",
];

/// Input encoding: the argument list, NUL-separated. A piece that is a single
/// byte `1..=WORDS.len()` stands for that command word, so short inputs reach
/// real command paths; any other piece is the argument itself (lossy UTF-8).
fuzz_target!(|data: &[u8]| {
    let args = data.split(|b| *b == 0).take(24).map(|piece| match piece {
        [b] if (1..=WORDS.len()).contains(&usize::from(*b)) => WORDS[usize::from(*b) - 1].to_string(),
        other => String::from_utf8_lossy(other).chars().take(64).collect(),
    });
    let argv = std::iter::once("mentisdb".to_string()).chain(args);
    if let Ok(CliCommand::Remote(invocation)) = parse_args(argv) {
        // A built request always has a parseable absolute URL.
        assert!(url::Url::parse(&invocation.url).is_ok(), "bad url {}", invocation.url);
        // POST carries an object body; other verbs carry none.
        match invocation.method {
            HttpMethod::Post => assert!(invocation.body.as_ref().is_some_and(|b| b.is_object())),
            _ => assert!(invocation.body.is_none()),
        }
    }
});
