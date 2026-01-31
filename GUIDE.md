# Clawprint User Guide

A step-by-step guide to installing, configuring, and using Clawprint to record, audit, and replay OpenClaw agent runs.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Try It Without a Gateway (Demo)](#try-it-without-a-gateway-demo)
- [Recording Agent Runs](#recording-agent-runs)
- [Browsing Recordings](#browsing-recordings)
- [Web Dashboard](#web-dashboard)
- [Verifying Integrity](#verifying-integrity)
- [Run Statistics](#run-statistics)
- [Replaying Runs Offline](#replaying-runs-offline)
- [Comparing Two Runs](#comparing-two-runs)
- [Exporting Data](#exporting-data)
- [Querying the REST API](#querying-the-rest-api)
- [Secret Redaction](#secret-redaction)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

- **Rust toolchain** (1.80+) — install from https://rustup.rs
- **OpenClaw** running locally (for live recording) — Clawprint connects to its gateway WebSocket

## Installation

### From source

```bash
git clone https://github.com/tsyrulb/clawprint.git
cd clawprint
cargo build --release
```

The binary is at `./target/release/clawprint`. You can copy it to your PATH:

```bash
cp ./target/release/clawprint ~/.local/bin/
```

### Using cargo install

```bash
cargo install --path .
```

### Verify installation

```bash
clawprint --version
clawprint --help
```

---

## Try It Without a Gateway (Demo)

You don't need a running OpenClaw instance to explore Clawprint. The included demo generates a synthetic recording with realistic agent events.

### Quick demo

```bash
cargo run --example demo
```

This creates a recording in `./clawprints/` with 2 agent conversation runs and 30 events. The output tells you the run ID and which commands to try next.

### Full walkthrough script

```bash
./demo.sh
```

This script builds Clawprint, generates demo data, and automatically runs every command (list, verify, stats, replay, view) so you can see the full workflow.

---

## Recording Agent Runs

### Step 1: Start OpenClaw

Make sure your OpenClaw instance is running. By default the gateway listens on `ws://127.0.0.1:18789`.

### Step 2: Authentication

Clawprint needs a gateway auth token. It looks for it automatically in:

```
~/.openclaw/openclaw.json
```

Expected format:

```json
{
  "gateway": {
    "auth": {
      "token": "your-gateway-token"
    }
  }
}
```

If the file exists and has a token, Clawprint picks it up automatically. Otherwise, pass it manually with `--token`.

### Step 3: Start recording

```bash
clawprint record --gateway ws://127.0.0.1:18789 --out ./clawprints
```

You will see a live spinner showing:
- Elapsed time
- Number of events captured
- Last event type received

Now use OpenClaw normally — have conversations, let the agent use tools, etc. Every event flows through the gateway and Clawprint records it.

### Step 4: Stop recording

Press **Ctrl+C**. Clawprint will:
1. Write a `RUN_END` event
2. Flush all buffered events to SQLite
3. Compute the root hash of the hash chain
4. Write `meta.json` with run metadata

### Recording options

```bash
# Custom run name instead of auto-generated UUID
clawprint record --run-name my-experiment-1

# Disable secret redaction (not recommended for production)
clawprint record --no-redact

# Explicit token
clawprint record --token your-token-here

# Larger batch size for high-throughput recording
clawprint record --batch-size 500

# Custom output directory
clawprint record --out /var/log/clawprints
```

### What gets recorded

Clawprint captures everything flowing through the OpenClaw gateway:

| Event | What it contains |
|-------|-----------------|
| Agent lifecycle | Start/end of each agent conversation, prompts, exit codes |
| Tool usage | Which tools the agent called, arguments passed, results returned |
| Chat output | Streamed assistant messages (delta chunks and final text) |
| Presence | Agent status heartbeats |
| Ticks | Gateway event loop ticks |

All events are chained with SHA-256 hashes, so any modification after recording is detectable.

---

## Browsing Recordings

### List all runs

```bash
clawprint list --out ./clawprints
```

Output shows a table with:
- **Run ID** (first 8 characters)
- **Started** timestamp
- **Duration** (human-readable: "53m 0s")
- **Event count**
- **Storage size**
- Summary footer with totals

### Understanding run IDs

Each run gets a UUID like `78cc70fb-a2eb-4c35-afe1-481d9e1752cf`. Most commands accept either the full UUID or just the first few characters (enough to be unique):

```bash
# These are equivalent if only one run starts with "78cc"
clawprint stats --run 78cc70fb-a2eb-4c35-afe1-481d9e1752cf --out ./clawprints
clawprint stats --run 78cc70fb --out ./clawprints
```

Note: the short form must match a directory name in `./clawprints/runs/`, so use the full ID if the short form doesn't work.

---

## Web Dashboard

The web viewer provides an interactive dark-themed dashboard for exploring recordings.

### Launch

```bash
clawprint view --run <run_id> --out ./clawprints
```

Open http://127.0.0.1:8080 in your browser.

### Auto-open in browser

```bash
clawprint view --run <run_id> --out ./clawprints --open
```

### Custom port

```bash
clawprint view --run <run_id> --out ./clawprints --port 3000
```

### Dashboard page (/)

The main page shows:
- **Summary cards** — total runs, total events, total storage used
- **Run list** — each run as a clickable card showing ID, status badge (Complete/In Progress), duration, event count, size, and a lock icon for chain integrity

Click any run to open its detail page.

### Run detail page (/view/:run_id)

The detail page has:

- **Header** — run ID, duration, event count, agent run count, integrity status
- **Event breakdown** — horizontal bar chart showing count per event type
- **Filter buttons** — click event type buttons to show/hide specific kinds (e.g., show only AGENT_EVENT and OUTPUT_CHUNK)
- **Search** — text search across event payloads with 300ms debounce
- **Event list** — paginated (50 per page), each event shows:
  - Color-coded left border by type
  - Timestamp
  - Event kind badge
  - Event ID and hash
  - Collapsible JSON payload (click to expand/collapse)
- **Pagination** — Previous/Next buttons with page numbers

---

## Verifying Integrity

Every event's hash depends on the previous event's hash, forming a chain. If any event is modified, inserted, or deleted, the chain breaks.

```bash
clawprint verify --run <run_id> --out ./clawprints
```

Output:
- **VALID** (green) — all hashes check out, recording is untampered
- **TAMPERED** (red) — hash chain is broken, the recording may have been modified

This is useful for:
- Compliance audits — prove agent actions haven't been altered
- Incident investigation — verify the recording is authentic
- Data integrity checks — detect storage corruption

---

## Run Statistics

```bash
clawprint stats --run <run_id> --out ./clawprints
```

Shows:
- **Event breakdown** — table with count and percentage per event type, with inline bar chart
- **Agent runs** — number of distinct agent conversation sessions
- **Events per minute** — timeline showing activity over the recording duration
- **Storage size** — disk space used by this run

This gives you a quick overview of what happened during the recording without opening the web dashboard.

---

## Replaying Runs Offline

Replay reconstructs what happened during a run from the stored events, without contacting the gateway.

### Print transcript to terminal

```bash
clawprint replay --run <run_id> --out ./clawprints --offline
```

The transcript includes:
- **Summary** — duration, period, event count, tool call count, agent run count
- **Event breakdown** — table of events by type
- **Agent runs** — each agent conversation as a section with:
  - Time range and duration
  - Tool calls with timestamps
  - Assistant chat output (reconstructed from output chunks)
- **All tool calls** — flat list with timestamps and JSON arguments
- **Final output** — the last complete assistant message

### Export to file

```bash
clawprint replay --run <run_id> --out ./clawprints --offline --export report.md
```

This writes the transcript to `report.md` in Markdown format, suitable for sharing or archiving.

---

## Comparing Two Runs

If you have multiple recordings (e.g., before and after a code change), you can compare them:

```bash
clawprint diff --run-a <run_id_1> --run-b <run_id_2> --out ./clawprints
```

Output includes:
- **Event counts** for both runs
- **Event kind comparison** — table showing count per type for each run with delta
- **Structural differences** — events where the kind changed at the same position
- **Length differences** — if runs have different event counts

---

## Exporting Data

### JSON export via web API

While the viewer is running, you can export all events as JSON:

```bash
# Start viewer
clawprint view --run <run_id> --out ./clawprints &

# Export all events
curl http://127.0.0.1:8080/api/runs/<run_id>/export > events.json
```

### Markdown transcript

```bash
clawprint replay --run <run_id> --out ./clawprints --offline --export transcript.md
```

### Direct SQLite access

Each run's data is in a standard SQLite database. You can query it directly:

```bash
sqlite3 ./clawprints/runs/<run_id>/ledger.sqlite \
  "SELECT event_id, kind, ts FROM events ORDER BY event_id"
```

---

## Querying the REST API

When the viewer is running, these API endpoints are available:

### List all runs

```bash
curl http://127.0.0.1:8080/api/runs
```

Returns JSON array of runs with `run_id`, `event_count`, `started_at`, `ended_at`, `size`.

### Get run metadata

```bash
curl http://127.0.0.1:8080/api/runs/<run_id>/meta
```

### Get run statistics

```bash
curl http://127.0.0.1:8080/api/runs/<run_id>/stats
```

Returns `event_breakdown`, `timeline` (events per minute), `agent_run_count`.

### Query events (filtered + paginated)

```bash
# Page 1, 50 events per page
curl "http://127.0.0.1:8080/api/runs/<run_id>/events?page=1&per_page=50"

# Filter by event kind
curl "http://127.0.0.1:8080/api/runs/<run_id>/events?kind=AGENT_EVENT"

# Search payload text
curl "http://127.0.0.1:8080/api/runs/<run_id>/events?search=read_file"

# Combine filters
curl "http://127.0.0.1:8080/api/runs/<run_id>/events?kind=AGENT_EVENT&search=tool_use&page=2"
```

Response format:

```json
{
  "events": [...],
  "total": 6061,
  "page": 1,
  "per_page": 50,
  "total_pages": 122
}
```

### Export all events

```bash
curl http://127.0.0.1:8080/api/runs/<run_id>/export > full_export.json
```

---

## Secret Redaction

By default, Clawprint redacts sensitive data before writing to the ledger. This protects against accidental credential exposure in audit logs.

### What gets redacted

| Pattern | Example | Replaced with |
|---------|---------|--------------|
| Bearer tokens | `Authorization: Bearer eyJhbG...` | `Bearer [REDACTED]` |
| Basic auth | `Authorization: Basic dXNlcjp...` | `Basic [REDACTED]` |
| AWS access keys | `AKIAIOSFODNN7EXAMPLE` | `[REDACTED]` |
| GitHub PATs | `ghp_xxxxxxxxxxxx` | `[REDACTED]` |
| JWTs | `eyJhbGciOiJ...eyJzdWIi...` | `[REDACTED]` |
| Sensitive JSON fields | `"api_key": "sk-..."` | `"api_key": "[REDACTED]"` |

### Disabling redaction

If you need the raw data (e.g., for debugging in a secure environment):

```bash
clawprint record --no-redact
```

This is not recommended for recordings that will be shared or stored long-term.

---

## Troubleshooting

### "No auth token found"

Clawprint can't find a gateway token. Either:
1. Create `~/.openclaw/openclaw.json` with the token (see [Authentication](#step-2-authentication))
2. Pass `--token <your-token>` on the command line

### "Connection refused" when recording

The OpenClaw gateway isn't running or is on a different address. Check:
```bash
# Verify gateway is listening
curl -s http://127.0.0.1:18789 || echo "Gateway not reachable"

# Use a different address
clawprint record --gateway ws://192.168.1.100:18789
```

### "Run not found"

The run ID doesn't match any directory in the output folder. Check available runs:
```bash
clawprint list --out ./clawprints
ls ./clawprints/runs/
```

### Empty recording (0 events after agent use)

The gateway auth token may be wrong. Try with debug logging:
```bash
RUST_LOG=clawprint=debug clawprint record --gateway ws://127.0.0.1:18789
```

Look for authentication errors or "challenge" responses in the debug output.

### Web viewer shows blank page

Check the browser console for errors. Common causes:
- Wrong run ID in the URL
- The viewer serves on `http://`, not `https://` — some browsers block mixed content

### Hash chain shows TAMPERED

The recording was modified after it was written. Possible causes:
- Manual editing of the SQLite database
- Disk corruption
- File was copied with metadata changes

The recording is still readable, but its integrity can't be guaranteed.

### Debug logging

For any issue, enable debug logging to see what Clawprint is doing:
```bash
RUST_LOG=clawprint=debug clawprint <command> ...
```

This shows WebSocket frames, event processing, hash chain operations, and storage writes.
