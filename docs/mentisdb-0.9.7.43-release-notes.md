# MentisDB 0.9.7.43 - Dashboard Skill Editing and Stdio Reliability

MentisDB 0.9.7.43 makes the web dashboard a safer place to maintain skills and tightens stdio mode for strict MCP clients.

## Highlights

- Dashboard skill editing is available from both the Skills table and each skill detail page.
- Saving an edit creates a new immutable skill version through the existing upload path.
- Skill detail editing now defaults to the latest version and edits the version currently being viewed.
- Rendered skill Markdown links are sanitized and protocol-allowlisted to prevent scriptable stored content.
- Stdio proxy notification handling and background headless daemon launch are more reliable for Claude Desktop and other MCP clients.
- The agent primer now consistently says `use mentisdb as your memory system`.

## Verification

| Gate | Result |
|------|--------|
| `cargo fmt -- --check` | Passed |
| `cargo clippy --all-features -- -D warnings` | Passed |
| `cargo test --all-features` | Passed, including 80 doc-tests with 4 ignored |
| Benchmarks | Skipped for this release by operator request; no benchmark numbers were changed. |

## Upgrade

```bash
cargo install mentisdb --force
```

No manual migration is required.
