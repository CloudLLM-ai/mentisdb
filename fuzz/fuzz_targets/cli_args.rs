#![no_main]
//! Fuzzes the CLI argument parser: whatever the shell hands `mentisdb`, parsing
//! either fails with a message or yields a well-formed request, and never panics.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mentisdb::cli::{parse_args, remote::HttpMethod, CliCommand};

/// Words that steer the fuzzer into real command paths and flags.
const WORDS: &[&str] = &[
    "thought", "append", "get", "search", "lexical", "chain", "branch", "agent", "key", "add",
    "skill", "diff", "daemon", "settings", "set", "--chain", "--type", "--limit", "--since",
    "--public-key-hex", "--yes", "--json", "--url", "--", "-", "=", "--help", "help",
];

#[derive(Arbitrary, Debug)]
enum Token {
    Known(u8),
    Free(String),
}

fuzz_target!(|tokens: Vec<Token>| {
    let args = tokens.iter().take(24).map(|t| match t {
        Token::Known(i) => WORDS[*i as usize % WORDS.len()].to_string(),
        Token::Free(s) => s.chars().take(64).collect(),
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
