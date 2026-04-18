# MentisDB 0.9.3.39 — Ratatui TUI, Streamable HTTP Passthrough, and Log Panel Fix

Released April 18, 2026. Cargo version `0.9.3`, git tag `0.9.3.39`.

This is a usability and correctness release. The daemon now renders a live
ratatui-based terminal user interface by default on interactive terminals,
the stdio proxy forwards raw JSON-RPC to the daemon's Streamable HTTP MCP
endpoint instead of manually mapping methods, and log panel corruption from
raw `env_logger` stderr output is eliminated by a custom TUI-aware logger.

## Highlights

- **Ratatui TUI.** Three-zone live dashboard: server info, endpoints & TLS,
  tabbed tables (Chains, Agents, Skills), and a scrollable event log.
- **Agent primer panel.** A dedicated "Prime your agent" panel shows the
  ready-to-paste instruction line for AI chat clients.
- **Streamable HTTP passthrough.** The stdio proxy now forwards all JSON-RPC
  to `POST /` on the daemon's Streamable HTTP endpoint — no manual method
  mapping required. The full MCP protocol (initialize, tools, resources,
  prompts, sampling, roots, notifications) works transparently.
- **Log panel corruption fix.** `env_logger` writes raw ANSI to stderr which
  corrupts ratatui's alternate screen buffer. A custom `TuiLogger` routes
  formatted log records into the TUI's log buffer via an `mpsc` channel.
- **Update dialog.** A centered modal dialog appears before migrations when
  a newer release is available on GitHub.
- **Startup progress overlay.** The TUI renders immediately with a centered
  modal showing the current startup phase (checking updates, running
  migrations, starting servers, loading chains). No more blank screen while
  the daemon initializes.
- **`--force-update` flag.** Run `mentisdbd --force-update` to force the
  update dialog to appear even if you're already at the latest release —
  useful for testing the update flow.
- **Reversed log panel.** Newest log entries appear at the top so the most
  recent events are always visible without scrolling.

## Ratatui TUI

When `mentisdbd` detects an interactive terminal (`stdin` and `stdout` are
both TTYs), it renders a ratatui 0.30.0-based TUI instead of plain text.
The layout has five zones:

```
┌─────────────────────────────────┬──────────────────────┐
│  ████  MENTISDB BANNER          │  Endpoints           │
│  mentisdb v0.9.3                │  MCP  (HTTP)  ...    │
│  mentisdbd running              │  REST (HTTP)  ...    │
│                                 │  MCP  (TLS)   ...    │
│  Configuration:                 │  REST (TLS)   ...    │
│    MENTISDB_DIR=...             │  Dashboard    ...    │
│    MENTISDB_MCP_PORT=9471       │                      │
│    ...                          │                      │
├─────────────────────────────────┴──────────────────────┤
│  Prime your agent — paste into your AI chat:           │
│  prime yourself for optimal mentisdb usage, ...        │
├────────────────────────────────────────────────────────┤
│  [Chains | Agents | Skills]  (tabbed table)            │
│  ┌──────────────────────────────────────────────────┐  │
│  │ Chain Key  Ver  Adapter  Thoughts  Agents  ...   │  │
│  │ ...                                              │  │
│  └──────────────────────────────────────────────────┘  │
├────────────────────────────────────────────────────────┤
│  Logs (142)                                            │
│  [INF] mentisdbd: mentisdb v0.9.3 started             │
│  [INF] mentisdbd: MCP (HTTP) http://127.0.0.1:9471    │
│  ...                                                   │
├────────────────────────────────────────────────────────┤
│ Server Info │ ↑↓ scroll │ Tab next pane │ Click focus │
└────────────────────────────────────────────────────────┘
```

### Keyboard and mouse controls

- **Tab / Shift+Tab** — cycle focus forward / backward through panes
- **Arrow keys** — scroll or select rows depending on focused pane
- **Left / Right** — switch table tabs when Tables pane is focused
- **Click** — focus any pane by clicking it; click tabs to switch
- **Scroll wheel** — scroll the pane under the cursor
- **q / Esc** — quit the TUI (daemon continues running)

The TUI uses the standard 8-color ANSI palette (`Color::Green`,
`Color::Yellow`, etc.) so colors adapt to both dark and light terminal
backgrounds. Row highlights use `Modifier::REVERSED` + `Modifier::BOLD`.

`--mode stdio` bypasses the TUI entirely and runs in plain stdio mode.

### Terminal safety

An RAII `TerminalCleanup` guard (`impl Drop`) ensures the terminal is
restored (disable raw mode, leave alternate screen, show cursor, disable
mouse capture) on `?` error propagation or panic.

## Streamable HTTP Passthrough

The stdio proxy's `proxy_jsonrpc_to_daemon` function now forwards all
JSON-RPC requests to the daemon's `POST /` endpoint (the Streamable HTTP
MCP root). Previously it manually mapped methods (`initialize`, `ping`,
`tools/list`, `tools/call`, `resources/list`, `resources/read`) and
returned hardcoded responses.

The new implementation:

1. Parses the JSON-RPC request to extract the method.
2. Handles `resources/read` for `mentisdb://skill/core` locally (the skill
   markdown is embedded in the proxy binary).
3. Forwards everything else to `POST /` with `Content-Type: application/json`
   and `Accept: application/json, text/event-stream`.
4. Parses SSE responses to extract the first JSON-RPC data event.
5. Passes through bare JSON responses as-is.

This means the stdio proxy now supports the full MCP protocol without
manual method mapping — including server-to-client notifications, sampling,
and roots.

## Log Panel Corruption Fix

`env_logger` writes formatted log records directly to stderr. When ratatui
is running in the alternate screen buffer, these raw bytes corrupt the
display (garbled characters, broken layout, flickering).

The fix introduces a custom `TuiLogger` that implements `log::Log`:

- Log records are formatted with level tags (`[INF]`, `[WRN]`, `[ERR]`)
  and sent through an `mpsc::Sender<String>`.
- The TUI event loop drains the `mpsc::Receiver<String>` each frame
  before `terminal.draw()`, appending lines to the log buffer.
- The log buffer is capped at 5000 lines (drops oldest 1000 when full).
- `init_tui_logger()` replaces `init_logger()` in the TUI startup path.

## Update Dialog

When a newer release is detected on GitHub, `mentisdbd` shows a centered
modal dialog in the TUI before running migrations:

```
┌────────────────────────────────────────────┐
│                Update                      │
│                                            │
│        mentisdbd update available          │
│                                            │
│        Current core version: 0.9.3         │
│        Latest release tag : 0.9.3.39       │
│        Release page       : https://...    │
│                                            │
│        Install release and restart now?    │
│        [y/N]                               │
└────────────────────────────────────────────┘
```

The update dialog is rendered as an overlay on top of the live TUI — the
background continues to render (banner, config, log lines filling in) so
you can see startup progress even while the dialog is waiting for input.

## Startup Progress Overlay

Previously, `mentisdbd` showed a blank screen for several seconds while it
checked for updates, ran migrations, and started servers. Now the TUI
renders immediately with a centered modal overlay:

```
┌──────────────────────────────────────────────┐
│                 Startup                       │
│                                              │
│          Checking for updates…               │
│                                              │
│        Press q to quit during startup.       │
└──────────────────────────────────────────────┘
```

The status text updates in real-time as the daemon progresses through each
phase:

1. **Starting…** — initial setup
2. **Checking for updates…** — GitHub API call
3. **Running migrations…** — chain schema migrations (if any)
4. **Starting servers…** — binding HTTP/HTTPS ports
5. **Loading chains and agents…** — populating table data

Once all startup work completes, the overlay disappears and the full
dashboard is revealed. Press `q` during startup to quit.

## `--force-update` Flag

Run `mentisdbd --force-update` to force the update dialog to appear even
if you're already at the latest release. This is useful for testing the
update flow without waiting for an actual new release to be published.

## Reversed Log Panel

The log panel now shows newest entries at the top. The most recent events
are always visible without scrolling — scroll down only if you want to see
older entries.

## Agent Primer Simplification

The "Prime your agent" panel now shows a single concise instruction line:

```
prime yourself for optimal mentisdb usage, call mentisdb_skill_md and update your local mentisdb skill
```

This replaces the previous multi-line guidance that included MCP addresses,
chain hints, and bootstrap instructions. The simplified line is easier to
triple-click select and paste into any AI chat client.

## Scrollbar Knobs

All scrollable panels now display scrollbar knobs (↑/↓ indicators) when
content exceeds the visible area:

- Top-left (server info & configuration)
- Top-right (endpoints & TLS)
- Table panels (Chains, Agents, Skills)
- Logs panel

This provides a clear visual indicator that content is scrollable.

## Layout Adjustments

Vertical space allocation:

- Top panels: 17 lines total (was larger)
- Prime panel: 5 lines
- Tables: flexible, minimum 14 lines
- Logs: flexible, minimum 14 lines
- Hint bar: 1 line

The tables and logs panels share remaining vertical space with the logs
panel getting a slight preference (`Min(14)` vs `Max(14)`).

## Upgrade

```bash
cargo install mentisdb --locked --force
```

**Interactive terminal users:** `mentisdbd` now renders a TUI by default.
Use `--mode stdio` for plain stdio mode (MCP client proxy).

**Claude Desktop users:** stdio mode now uses Streamable HTTP passthrough.
Point your MCP config at the `mentisdbd` binary and the full MCP protocol
works without pre-launching the daemon.

No schema migration. Existing chains, skills, and webhook registrations
from 0.9.2.x carry forward unchanged.

## Links

- [GitHub Releases](https://github.com/CloudLLM-ai/mentisdb/releases)
- [Previous release: 0.9.2.38](https://github.com/CloudLLM-ai/mentisdb/releases/tag/0.9.2.38)
