# 🔴 Clawprint

**Flight recorder and receipts for OpenClaw agent runs**

> "Show the Clawprint" / "Receipts for agent actions"

Clawprint is an audit + replay + diff system for agent AI systems. It records every agent run (tool calls, outputs, metadata) in a tamper-evident ledger with offline replay capabilities.

**Not a proxy/firewall** — Clawprint is purely an observer and recorder.

## Features

- **Out-of-process observer** — Connects to OpenClaw Gateway WebSocket without modifying core code
- **Tamper-evident ledger** — SHA-256 hash chain for every event
- **Secret redaction** — Automatic redaction of API keys, tokens, credentials
- **Offline replay** — Reconstruct agent runs without gateway contact
- **Web viewer** — Timeline view with search and export
- **Cross-platform** — Runs on macOS, Linux, cloud VMs, old laptops

## Quick Start

```bash
# Install
cargo install --path .

# Start recording
clawprint record --gateway ws://127.0.0.1:18789 --out ./clawprints

# List recorded runs
clawprint list --out ./clawprints

# View in browser
clawprint view --run <run_id> --open

# Replay offline
clawprint replay --run <run_id> --offline

# Verify integrity
clawprint verify --run <run_id>
```

## Storage Format

Each run is a self-contained "case file":

```
clawprints/
└── runs/
    └── <run_id>/
        ├── ledger.sqlite      # Events with hash chain
        ├── artifacts/         # Compressed blobs (zstd)
        │   └── <hash_prefix>/<hash>.zst
        └── meta.json          # Run metadata + root hash
```

## Event Types

- `RUN_START` / `RUN_END` — Session boundaries
- `AGENT_EVENT` — Raw gateway stream events
- `TOOL_CALL` / `TOOL_RESULT` — Tool invocations
- `OUTPUT_CHUNK` — Streamed output
- `PRESENCE` / `TICK` / `SHUTDOWN` — Gateway status

## Architecture

```
┌─────────────┐     WebSocket      ┌──────────┐     SQLite      ┌─────────────┐
│   OpenClaw  │◄──────────────────►│ Clawprint│──────────────►│   Ledger    │
│   Gateway   │   (observer role)  │ Recorder │   + artifacts │   Storage   │
└─────────────┘                    └──────────┘               └─────────────┘
                                                                        │
                                    ┌──────────┐                        │
                                    │  Viewer  │◄───────────────────────┘
                                    │  (HTTP)  │      Query/replay
                                    └──────────┘
```

## Configuration

Environment variables:
- `RUST_LOG=clawprint=debug` — Debug logging
- `CLAWPRINT_OUTPUT` — Default output directory

## License

MIT
