//! Clawprint CLI - Flight recorder for OpenClaw agent runs
//!
//! Usage:
//!   clawprint record --gateway ws://127.0.0.1:18789 [--out ./clawprints]
//!   clawprint list --out ./clawprints
//!   clawprint view --run <run_id> [--open]
//!   clawprint replay --run <run_id> --offline
//!   clawprint stats --run <run_id>

use anyhow::Result;
use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

static USE_COLOR: AtomicBool = AtomicBool::new(true);

/// Print macro that strips ANSI codes when color is disabled
macro_rules! cprintln {
    ($($arg:tt)*) => {
        if USE_COLOR.load(Ordering::Relaxed) {
            println!($($arg)*);
        } else {
            let s = format!($($arg)*);
            println!("{}", strip_ansi(&s));
        }
    };
}

macro_rules! cprint {
    ($($arg:tt)*) => {
        if USE_COLOR.load(Ordering::Relaxed) {
            print!($($arg)*);
        } else {
            let s = format!($($arg)*);
            print!("{}", strip_ansi(&s));
        }
    };
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            out.push(c);
        }
    }
    out
}

use clawprint::{
    record::RecordingSession,
    replay::{diff_runs, replay_run, generate_transcript},
    storage::{list_runs_with_stats, RunStorage},
    viewer::start_viewer,
    Config, RunId,
};

#[derive(Parser)]
#[command(name = "clawprint")]
#[command(about = "Flight recorder and receipts for OpenClaw agent runs")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Record agent runs from gateway
    Record {
        /// Gateway WebSocket URL
        #[arg(short, long, default_value = "ws://127.0.0.1:18789")]
        gateway: String,
        /// Output directory for recordings
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
        /// Run name/ID (auto-generated if not specified)
        #[arg(long)]
        run_name: Option<String>,
        /// Gateway auth token (auto-discovered from ~/.openclaw/openclaw.json if omitted)
        #[arg(short, long)]
        token: Option<String>,
        /// Disable secret redaction
        #[arg(long)]
        no_redact: bool,
        /// Batch size for SQLite commits
        #[arg(long, default_value = "100")]
        batch_size: usize,
    },
    /// List recorded runs
    List {
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
    },
    /// View a recorded run (opens HTTP viewer)
    View {
        /// Run ID to view
        #[arg(short, long)]
        run: String,
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
        /// Open in browser automatically
        #[arg(long)]
        open: bool,
        /// Port for viewer server
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
    /// Replay a recorded run
    Replay {
        /// Run ID to replay
        #[arg(short, long)]
        run: String,
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
        /// Offline mode (no gateway contact)
        #[arg(long)]
        offline: bool,
        /// Export transcript to file
        #[arg(long)]
        export: Option<PathBuf>,
    },
    /// Compare two runs
    Diff {
        /// First run ID
        #[arg(long)]
        run_a: String,
        /// Second run ID
        #[arg(long)]
        run_b: String,
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
    },
    /// Verify run integrity
    Verify {
        /// Run ID to verify
        #[arg(short, long)]
        run: String,
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
    },
    /// Show run statistics
    Stats {
        /// Run ID to analyze
        #[arg(short, long)]
        run: String,
        /// Output directory
        #[arg(short, long, default_value = "./clawprints")]
        out: PathBuf,
    },
}

fn format_duration(secs: i64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, mins, s)
    } else if mins > 0 {
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", s)
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Try to read the gateway auth token from ~/.openclaw/openclaw.json
fn discover_openclaw_token() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let config_path = PathBuf::from(home).join(".openclaw").join("openclaw.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    config.pointer("/gateway/auth/token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Respect NO_COLOR env var and non-TTY output
    if std::env::var("NO_COLOR").is_ok() || !std::io::stdout().is_terminal() {
        USE_COLOR.store(false, Ordering::Relaxed);
    }

    let cli = Cli::parse();

    // Only show info logs for record command; others use warn to keep output clean
    let default_log = match &cli.command {
        Commands::Record { .. } => "clawprint=info",
        _ => "clawprint=warn",
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| default_log.to_string()),
        )
        .init();

    match cli.command {
        Commands::Record {
            gateway,
            out,
            run_name,
            token,
            no_redact,
            batch_size,
        } => {
            let auth_token = match token {
                Some(t) => {
                    info!("Using token from --token flag");
                    Some(t)
                }
                None => {
                    match discover_openclaw_token() {
                        Some(t) => {
                            info!("Auto-discovered token from ~/.openclaw/openclaw.json");
                            Some(t)
                        }
                        None => {
                            warn!("No auth token found. Pass --token or configure gateway.auth.token in ~/.openclaw/openclaw.json");
                            None
                        }
                    }
                }
            };

            let config = Config {
                output_dir: out,
                redact_secrets: !no_redact,
                gateway_url: gateway,
                auth_token,
                batch_size,
                flush_interval_ms: 200,
            };

            info!("Starting Clawprint recorder...");
            info!("Gateway: {}", config.gateway_url);
            info!("Output: {:?}", config.output_dir);
            info!("Redaction: {}", if config.redact_secrets { "enabled" } else { "disabled" });

            let session = RecordingSession::start(config, run_name).await?;
            let run_id = session.run_id().clone();

            info!("Recording started: {}", run_id.0);
            info!("Press Ctrl+C to stop recording...");

            tokio::signal::ctrl_c().await?;

            info!("\nStopping recording...");
            session.stop().await?;

            info!("Recording saved: {}", run_id.0);
        }

        Commands::List { out } => {
            let runs = list_runs_with_stats(&out)?;

            if runs.is_empty() {
                cprintln!("{}", "No recorded runs found.".yellow());
                return Ok(());
            }

            cprintln!("\n  {}\n", "Clawprint Recordings".bright_cyan().bold());
            cprintln!(
                "  {:<14} {:<20} {:<14} {:>8}  {:>10}",
                "RUN ID".bold().dimmed(),
                "STARTED".bold().dimmed(),
                "DURATION".bold().dimmed(),
                "EVENTS".bold().dimmed(),
                "SIZE".bold().dimmed(),
            );
            cprintln!("  {}", "─".repeat(70).dimmed());

            let mut total_events: u64 = 0;
            let mut total_size: u64 = 0;

            for (run_id, meta, size) in &runs {
                let dur = meta
                    .ended_at
                    .map(|end| {
                        let d = end.signed_duration_since(meta.started_at);
                        format_duration(d.num_seconds())
                    })
                    .unwrap_or_else(|| "recording...".to_string());

                total_events += meta.event_count;
                total_size += size;

                let id_short = &run_id.0[..8.min(run_id.0.len())];
                cprintln!(
                    "  {:<14} {:<20} {:<14} {:>8}  {:>10}",
                    id_short.bright_blue(),
                    meta.started_at.format("%Y-%m-%d %H:%M:%S").to_string().dimmed(),
                    dur.green(),
                    meta.event_count.to_string().cyan(),
                    format_bytes(*size).dimmed(),
                );
            }

            cprintln!("  {}", "─".repeat(70).dimmed());
            cprintln!(
                "  {} runs  {}  {}\n",
                runs.len().to_string().bold(),
                format!("{} events", total_events).cyan(),
                format_bytes(total_size).dimmed(),
            );
        }

        Commands::View { run, out, open, port } => {
            let run_id = RunId(run);
            let id_short = &run_id.0[..8.min(run_id.0.len())];
            cprintln!(
                "\n  {} Viewer for run {}\n  {}\n",
                "Clawprint".bright_cyan().bold(),
                id_short.bright_blue(),
                format!("http://127.0.0.1:{}", port).underline(),
            );

            if open {
                let url = format!("http://127.0.0.1:{}/view/{}", port, run_id.0);
                let _ = open::that(&url);
            }

            start_viewer(out, port).await?;
        }

        Commands::Replay { run, out, offline, export } => {
            let run_id = RunId(run);
            info!("Replaying run: {}", run_id.0);

            let result = replay_run(&run_id, &out, offline)?;
            let transcript = generate_transcript(&result);

            if let Some(export_path) = export {
                std::fs::write(&export_path, &transcript)?;
                cprintln!(
                    "  {} Transcript exported to {:?}",
                    "OK".green().bold(),
                    export_path,
                );
            } else {
                cprintln!("{}", transcript);
            }
        }

        Commands::Diff { run_a, run_b, out } => {
            let run_a = RunId(run_a);
            let run_b = RunId(run_b);

            info!("Comparing runs: {} vs {}", run_a.0, run_b.0);

            let diff = diff_runs(&run_a, &run_b, &out)?;
            cprintln!("{}", diff);
        }

        Commands::Verify { run, out } => {
            let run_id = RunId(run);
            let storage = RunStorage::open(run_id.clone(), &out)?;

            let id_short = &run_id.0[..8.min(run_id.0.len())];
            cprint!("  Verifying hash chain for {}... ", id_short.bright_blue());
            std::io::stdout().flush()?;

            match storage.verify_chain() {
                Ok(true) => {
                    cprintln!("{}", "VALID".green().bold());
                    cprintln!("  Events:    {}", storage.event_count().to_string().cyan());
                    cprintln!("  Root hash: {}", storage.root_hash().unwrap_or_default().dimmed());
                }
                Ok(false) => {
                    cprintln!("{}", "TAMPERED".red().bold());
                    eprintln!("  Hash chain verification failed - run may have been modified");
                    std::process::exit(1);
                }
                Err(e) => {
                    cprintln!("{}: {}", "ERROR".red().bold(), e);
                    std::process::exit(1);
                }
            }
        }

        Commands::Stats { run, out } => {
            let run_id = RunId(run);
            let storage = RunStorage::open(run_id.clone(), &out)?;

            let id_short = &run_id.0[..8.min(run_id.0.len())];
            cprintln!(
                "\n  {} Run {}\n",
                "Statistics".bright_cyan().bold(),
                id_short.bright_blue(),
            );

            // Event breakdown
            let breakdown = storage.event_count_by_kind()?;
            let total: u64 = breakdown.values().sum();

            cprintln!("  {}", "Event Breakdown".bold());
            cprintln!("  {}", "─".repeat(50).dimmed());

            let mut sorted: Vec<_> = breakdown.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));

            let max_count = sorted.first().map(|(_, c)| **c).unwrap_or(1);

            for (kind, count) in &sorted {
                let pct = (**count as f64 / total as f64) * 100.0;
                let bar_len = ((**count as f64 / max_count as f64) * 25.0) as usize;
                let bar: String = "█".repeat(bar_len);
                cprintln!(
                    "  {:<16} {:>6} ({:>5.1}%) {}",
                    kind.cyan(),
                    count.to_string().bright_white(),
                    pct,
                    bar.green(),
                );
            }
            cprintln!("  {:<16} {:>6}", "TOTAL".bold(), total.to_string().bright_white().bold());

            // Agent runs
            let agent_runs = storage.agent_run_ids()?;
            cprintln!(
                "\n  {}: {}",
                "Agent Runs".bold(),
                agent_runs.len().to_string().cyan(),
            );

            // Timeline
            let timeline = storage.events_timeline()?;
            if !timeline.is_empty() {
                cprintln!("\n  {}", "Events per Minute".bold());
                cprintln!("  {}", "─".repeat(50).dimmed());

                let max_rate = timeline.iter().map(|(_, c)| *c).max().unwrap_or(1);
                for (minute, count) in &timeline {
                    let bar_len = ((*count as f64 / max_rate as f64) * 30.0) as usize;
                    let bar: String = "▓".repeat(bar_len);
                    cprintln!(
                        "  {} {:>5} {}",
                        minute.dimmed(),
                        count.to_string().bright_white(),
                        bar.bright_blue(),
                    );
                }
            }

            // Storage
            let size = storage.storage_size_bytes()?;
            cprintln!(
                "\n  {}: {}\n",
                "Storage Size".bold(),
                format_bytes(size).cyan(),
            );
        }
    }

    Ok(())
}
