//! Clawprint CLI - Flight recorder for OpenClaw agent runs
//!
//! Usage:
//!   clawprint record --gateway ws://127.0.0.1:18789 [--out ./clawprints]
//!   clawprint list --out ./clawprints
//!   clawprint view --run <run_id> [--open]
//!   clawprint replay --run <run_id> --offline

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{info, warn};

use clawprint::{
    record::RecordingSession,
    replay::{diff_runs, replay_run, generate_transcript},
    storage::list_runs,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "clawprint=info".to_string()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Record {
            gateway,
            out,
            run_name,
            no_redact,
            batch_size,
        } => {
            let config = Config {
                output_dir: out,
                redact_secrets: !no_redact,
                gateway_url: gateway,
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

            // Wait for Ctrl+C
            tokio::signal::ctrl_c().await?;

            info!("\nStopping recording...");
            session.stop().await?;

            info!("Recording saved: {}", run_id.0);
        }

        Commands::List { out } => {
            let runs = list_runs(&out)?;

            if runs.is_empty() {
                println!("No recorded runs found in {:?}", out);
                return Ok(());
            }

            println!("Recorded runs in {:?}:\n", out);
            println!("{:<12} {:<20} {:<12} {:<10}", "Run ID", "Started", "Duration", "Events");
            println!("{}", "-".repeat(60));

            for (run_id, meta) in runs {
                let duration = meta
                    .ended_at
                    .map(|end| {
                        let dur = end.signed_duration_since(meta.started_at);
                        format!("{}s", dur.num_seconds())
                    })
                    .unwrap_or_else(|| "-".to_string());

                println!(
                    "{:<12} {:<20} {:<12} {:<10}",
                    &run_id.0[..8.min(run_id.0.len())],
                    meta.started_at.format("%Y-%m-%d %H:%M:%S"),
                    duration,
                    meta.event_count
                );
            }
        }

        Commands::View { run, out, open, port } => {
            let run_id = RunId(run);
            info!("Starting viewer for run: {}", run_id.0);
            info!("Open http://127.0.0.1:{}/view/{} in your browser", port, run_id.0);

            if open {
                // Try to open browser
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
                info!("Transcript exported to: {:?}", export_path);
            } else {
                println!("{}", transcript);
            }
        }

        Commands::Diff { run_a, run_b, out } => {
            let run_a = RunId(run_a);
            let run_b = RunId(run_b);
            
            info!("Comparing runs: {} vs {}", run_a.0, run_b.0);
            
            let diff = diff_runs(&run_a, &run_b, &out)?;
            println!("{}", diff);
        }

        Commands::Verify { run, out } => {
            let run_id = RunId(run);
            
            use clawprint::storage::RunStorage;
            let storage = RunStorage::open(run_id.clone(), &out)?;
            
            print!("Verifying hash chain for {}... ", run_id.0);
            
            match storage.verify_chain() {
                Ok(true) => {
                    println!("✓ VALID");
                    println!("Events: {}", storage.event_count());
                    println!("Root hash: {}", storage.root_hash().unwrap_or_default());
                }
                Ok(false) => {
                    println!("✗ TAMPERED");
                    warn!("Hash chain verification failed - run may have been modified");
                    std::process::exit(1);
                }
                Err(e) => {
                    println!("✗ ERROR: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}
