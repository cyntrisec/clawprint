<p align="center">
  <img src="clawprint.jpg" alt="Clawprint" width="300">
</p>

<h1 align="center">Clawprint</h1>

<p align="center"><b>Every molt leaves a mark. Trace. Verify. Trust.</b></p>

Clawprint is a tamper-evident audit trail for OpenClaw agent runs. It silently taps the gateway wire, capturing every trace a molt leaves behind — tool calls, outputs, lifecycle events — and seals them in a SHA-256 hash chain ledger. Replay sessions offline, inspect the evidence through a cybersecurity-style dashboard, or query activity from Claude Desktop via MCP.

**Not a proxy or firewall** — Clawprint is a passive observer. It watches the wire, never touches it.

## Features

- **24/7 daemon mode** — Continuous recording to a single ledger, auto-reconnect on disconnect
- **MCP server** — Claude Desktop integration for querying agent activity via natural language
- **Security scanner** — Detect destructive operations, prompt injection, privilege escalation, and anomalies
- **Tamper-evident ledger** — SHA-256 hash chain for every event with integrity verification
- **Secret redaction** — Automatic redaction of API keys, tokens, JWTs, AWS keys, GitHub PATs, and credentials
- **Offline replay** — Reconstruct agent runs with event breakdowns, agent run sections, and chat reconstruction
- **Web dashboard** — Dark-themed cybersecurity dashboard with filtered/paginated events, search, and bar charts
- **CLI analytics** — Colored output, event histograms, per-minute timeline, live recording spinner
- **Run diffing** — Compare two runs side-by-side with event kind breakdown
- **Cross-platform** — Prebuilt binaries for Linux (x86_64, aarch64) and macOS (Intel, Apple Silicon)
- **CI/CD** — GitHub Actions for testing, linting, and automated release builds

## Quick Start

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/tsyrulb/clawprint/master/install.sh | bash

# Or build from source
cargo install --path .

# Start 24/7 daemon (recommended for always-on agents)
clawprint daemon --gateway ws://127.0.0.1:18789 --out ./clawprints

# Or record a single session
clawprint record --gateway ws://127.0.0.1:18789 --out ./clawprints

# Open the latest recording in browser
clawprint open --out ./clawprints

# Start MCP server for Claude Desktop
clawprint mcp --out ./clawprints
```

## Commands

| Command | Description |
|---------|-------------|
| `daemon` | 24/7 continuous recording to a single ledger with auto-reconnect |
| `record` | Record a single session with post-recording summary |
| `mcp` | Start MCP server for Claude Desktop integration |
| `open` | Open the latest (or specific) recording in the web dashboard |
| `list` | List all recorded runs with duration, event count, and storage size |
| `view` | Launch web dashboard for a specific run |
| `replay` | Reconstruct a run offline with agent run sections and chat output |
| `stats` | Show event type histogram, events-per-minute timeline, and agent run count |
| `verify` | Verify SHA-256 hash chain integrity for a recorded run |
| `diff` | Compare two runs with event kind breakdown |

## Daemon Mode (24/7 Recording)

For always-on agents like OpenClaw, use daemon mode. It records to a single continuous ledger and auto-reconnects on disconnect:

```bash
clawprint daemon --gateway ws://127.0.0.1:18789 --out ./clawprints
```

The daemon:
- Writes to a single `ledger.sqlite` that grows forever
- Automatically groups events into agent conversation runs
- Reconnects with exponential backoff (1s, 2s, 4s... up to 60s)
- Shuts down gracefully on Ctrl+C / SIGTERM

### As a systemd service

```ini
# /etc/systemd/system/clawprint.service
[Unit]
Description=Clawprint Flight Recorder
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/clawprint daemon --gateway ws://127.0.0.1:18789 --out /var/lib/clawprints
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now clawprint
```

## MCP Server (Claude Desktop Integration)

Clawprint includes an MCP (Model Context Protocol) server so you can query agent activity directly from Claude Desktop using natural language.

### Local setup (stdio)

Add to your Claude Desktop config (`~/.claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "clawprint": {
      "command": "clawprint",
      "args": ["mcp", "--out", "/path/to/clawprints"]
    }
  }
}
```

### Remote setup (SSE)

Run the MCP server in SSE mode on the machine with your recordings:

```bash
clawprint mcp --transport sse --host 0.0.0.0 --port 3000 --token mysecret
```

Then on your remote machine, configure Claude Desktop:

```json
{
  "mcpServers": {
    "clawprint": {
      "url": "http://<host-ip>:3000/mcp"
    }
  }
}
```

### Available tools

Once connected, Claude can use these tools:

| Tool | What it does |
|------|-------------|
| `clawprint_status` | Recording status, total events, ledger size, integrity |
| `clawprint_list_runs` | List agent conversation runs with duration and tool call count |
| `clawprint_get_run` | Full transcript of an agent run (use `run_id='latest'`) |
| `clawprint_search` | Search event payloads across all history |
| `clawprint_tool_calls` | List tool calls with filtering by run, time, or tool name |
| `clawprint_security_check` | Scan for destructive ops, prompt injection, privilege escalation |
| `clawprint_verify` | Verify hash chain integrity |
| `clawprint_stats` | Event statistics, breakdown by type, timeline |

### Example queries

Ask Claude things like:
- "What did my agent do in the last hour?"
- "Show me all tool calls from today"
- "Run a security check on the latest agent run"
- "Search for any file deletions"
- "Is the recording ledger intact?"

## Security Auditing

The built-in security scanner detects suspicious patterns in recorded events:

| Category | What it detects |
|----------|----------------|
| Destructive Operations | `rm -rf`, `DROP TABLE`, `DELETE FROM`, `git push --force`, `git reset --hard` |
| Prompt Injection | "ignore previous instructions", role switching, obfuscated base64 payloads |
| Privilege Escalation | `sudo`, `chmod 777`, writes to `/etc/`, `/root/` |
| External Access | `curl`/`wget` in tool calls, HTTP URLs in tool arguments |
| Cost Anomaly | >50 tool calls per agent run, >100 events per minute |

Use via MCP: ask Claude "run a security check on today's activity"

Or programmatically via the `clawprint::security::scan_events()` API.

## Web Dashboard

The `view` command launches a dark-themed cybersecurity dashboard at `http://127.0.0.1:8080`:

- **Dashboard page** — Summary cards (total runs, events, storage), clickable run list with status badges and integrity indicators
- **Run detail page** — Event breakdown bar chart, filter buttons per event kind, text search with debounce, paginated event list (50/page), collapsible JSON payloads, color-coded event cards

### API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /api/runs` | List all runs with metadata and size |
| `GET /api/runs/:id/events` | Paginated events with `?kind=X&search=Y&page=N&per_page=50` |
| `GET /api/runs/:id/stats` | Event breakdown, timeline, agent run count |
| `GET /api/runs/:id/meta` | Run metadata |
| `GET /api/runs/:id/export` | Full event export as JSON |

## Storage Format

Each run is a self-contained "case file":

```
clawprints/
  runs/
    <run_id>/
      ledger.sqlite      # Events with SHA-256 hash chain (WAL mode, synchronous=FULL)
      artifacts/          # Compressed blobs (zstd)
        <hash_prefix>/<hash>.zst
      meta.json           # Run metadata + root hash
  ledger.sqlite           # Continuous ledger (daemon mode)
```

### SQLite Schema

Events are stored with sequential IDs, timestamps, event kind, JSON payload, span/parent IDs, and hash chain fields (`hash_prev`, `hash_self`). Artifacts are deduplicated by content hash and compressed with Zstandard.

## Event Types

| Kind | Description |
|------|-------------|
| `RUN_START` / `RUN_END` | Session boundaries |
| `AGENT_EVENT` | Raw gateway stream events (tool calls, results, agent lifecycle) |
| `TOOL_CALL` / `TOOL_RESULT` | Direct tool invocations and results |
| `OUTPUT_CHUNK` | Streamed assistant output (chat messages) |
| `PRESENCE` / `TICK` | Gateway heartbeat and event loop ticks |
| `SHUTDOWN` | Gateway shutdown signal |
| `CUSTOM` | Unknown/custom event types |

## Architecture

```
                                                        Storage
 OpenClaw       WebSocket        Clawprint             (SQLite + zstd)
 Gateway  <------------------>  Daemon/Recorder  --->  ledger.sqlite
           (observer role)         |
                                   |--- MCP Server ---> Claude Desktop
                                   |
                                   v
                              Web Viewer  <---  Browser
                              (Axum HTTP)       (Dashboard UI)
```

### Modules

| Module | Role |
|--------|------|
| `gateway` | WebSocket client for OpenClaw Gateway protocol v3 (req/res/event frames) |
| `record` | Recording session coordinator with live progress spinner |
| `daemon` | 24/7 continuous recording with auto-reconnect |
| `ledger` | Single continuous SQLite ledger with agent run grouping |
| `mcp` | MCP server for Claude Desktop integration (8 tools) |
| `security` | Security scanner for detecting suspicious patterns |
| `storage` | Per-session SQLite ledger with hash chain, artifact store, filtered queries |
| `replay` | Offline replay with agent run grouping and chat reconstruction |
| `viewer` | Axum web server with dashboard UI and REST API |
| `redact` | Secret detection and redaction (regex-based, supports JWT/AWS/GitHub patterns) |

## Authentication

Clawprint auto-discovers the gateway auth token from `~/.openclaw/openclaw.json`:

```json
{
  "gateway": {
    "auth": {
      "token": "your-token-here"
    }
  }
}
```

Or pass it directly: `clawprint record --token <token>`

## Configuration

| Option | Default | Description |
|--------|---------|-------------|
| `--gateway` | `ws://127.0.0.1:18789` | Gateway WebSocket URL |
| `--out` | `./clawprints` | Output directory for recordings |
| `--token` | auto-discovered | Gateway auth token (record/daemon) or HTTP bearer token (view/open/mcp) |
| `--no-redact` | `false` | Disable secret redaction |
| `--batch-size` | `100` | SQLite batch commit size |
| `--host` | `127.0.0.1` | Bind address for viewer/MCP (use `0.0.0.0` for network access) |
| `--port` | `8080` / `3000` | Web viewer / MCP SSE server port |
| `--transport` | `stdio` | MCP transport: `stdio` (local) or `sse` (network) |
| `RUST_LOG` | `clawprint=info` | Log level (set to `clawprint=debug` for verbose output) |

## Integrity Verification

Every trace includes a SHA-256 hash computed from its canonical form. Each trace's `hash_prev` points to the previous trace's `hash_self`, forming a tamper-evident chain. The `verify` command inspects the entire chain of evidence and reports `INTACT` or `COMPROMISED`.

```bash
$ clawprint verify --run <run_id> --out ./clawprints
  Verifying hash chain for a1b2c3d4... VALID
  Events:    1234
  Root hash: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

## Installation

### Prebuilt binaries

```bash
curl -fsSL https://raw.githubusercontent.com/tsyrulb/clawprint/master/install.sh | bash
```

Downloads the latest release binary for your platform (Linux x86_64/aarch64, macOS Intel/Apple Silicon). Falls back to building from source if no binary is available.

### From source

```bash
cargo install --path .
```

Or manually:

```bash
cargo build --release
cp target/release/clawprint ~/.local/bin/
```

## Building

```bash
# Development
cargo build

# Release (optimized, LTO, stripped)
cargo build --release

# Run tests
cargo test

# Without web viewer or MCP features
cargo build --no-default-features
```

## Releasing

Releases are automated via GitHub Actions. To publish a new version:

```bash
git tag v0.1.0
git push origin v0.1.0
```

This builds binaries for all 4 platforms and creates a GitHub Release with the assets attached.

## License

MIT
