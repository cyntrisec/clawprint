//! Daemon mode — continuous 24/7 recording to the single ledger.
//!
//! Unlike `record` (session-based, Ctrl+C to stop), the daemon runs forever,
//! auto-reconnects on disconnect, and writes to a single growing ledger.

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::{
    gateway::{GatewayClient, GatewayEvent},
    ledger::Ledger,
    record::gateway_event_to_event,
    Config, EventId, RunId,
};

/// Run the daemon: connect to gateway, record to ledger, auto-reconnect.
pub async fn run_daemon(config: Config) -> Result<()> {
    let ledger_path = config.output_dir.clone();
    let ledger = Ledger::open(&ledger_path, config.batch_size)?;
    let ledger = Arc::new(Mutex::new(ledger));

    let auth_token = config.auth_token.clone()
        .ok_or_else(|| anyhow::anyhow!(
            "No auth token provided. Use --token or set gateway.auth.token in ~/.openclaw/openclaw.json"
        ))?;

    // Store start time in meta
    {
        let l = ledger.lock().await;
        l.set_meta("daemon_started_at", &chrono::Utc::now().to_rfc3339())?;
        l.set_meta("gateway_url", &config.gateway_url)?;
    }

    // Progress spinner on stderr
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} [{elapsed_precise}] {msg}")
            .unwrap()
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb.set_message("Connecting to gateway...");

    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(60);

    // Shutdown flag — survives across reconnect loop iterations
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_clone.store(true, Ordering::SeqCst);
    });

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        match run_connection(
            &config.gateway_url,
            &auth_token,
            config.redact_secrets,
            config.flush_interval_ms,
            ledger.clone(),
            &pb,
            &shutdown,
        ).await {
            Ok(ShutdownReason::Signal) => {
                break;
            }
            Ok(ShutdownReason::Disconnected) => {
                // Connection was established then lost — reset backoff
                backoff = Duration::from_secs(1);
                warn!("Gateway disconnected, reconnecting in {:?}...", backoff);
                pb.set_message(format!("Disconnected. Reconnecting in {}s...", backoff.as_secs()));

                // Wait for backoff duration, but check shutdown periodically
                let sleep_until = tokio::time::Instant::now() + backoff;
                loop {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let remaining = sleep_until.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(remaining.min(Duration::from_millis(200))).await;
                }

                if shutdown.load(Ordering::SeqCst) {
                    break;
                }

                // Exponential backoff (will reset on next successful connection)
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(e) => {
                warn!("Connection error: {}. Reconnecting in {:?}...", e, backoff);
                pb.set_message(format!("Error. Reconnecting in {}s...", backoff.as_secs()));

                let sleep_until = tokio::time::Instant::now() + backoff;
                loop {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let remaining = sleep_until.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(remaining.min(Duration::from_millis(200))).await;
                }

                if shutdown.load(Ordering::SeqCst) {
                    break;
                }

                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }

    // Graceful shutdown: flush and record stop time
    pb.finish_and_clear();
    info!("Daemon shutting down gracefully");

    let mut l = ledger.lock().await;
    l.flush()?;
    l.set_meta("daemon_stopped_at", &chrono::Utc::now().to_rfc3339())?;

    let total = l.total_events();
    let size = l.storage_size_bytes().unwrap_or(0);
    eprintln!(
        "  Daemon stopped. {} events recorded, {} on disk.",
        total,
        format_bytes(size),
    );
    Ok(())
}

enum ShutdownReason {
    Signal,
    Disconnected,
}

/// Run a single gateway connection session, writing events to the ledger.
/// Returns the shutdown reason so the caller can decide whether to reconnect.
async fn run_connection(
    gateway_url: &str,
    auth_token: &str,
    redact: bool,
    flush_interval_ms: u64,
    ledger: Arc<Mutex<Ledger>>,
    pb: &ProgressBar,
    shutdown: &Arc<AtomicBool>,
) -> Result<ShutdownReason> {
    let mut client = GatewayClient::new(gateway_url, auth_token)?;
    let conn_id = client.connect().await?;

    info!("Daemon connected to gateway, connId: {}", conn_id);
    pb.set_message("Connected. Recording...");

    // Spawn gateway event reader, track the task handle
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<GatewayEvent>(1000);
    let handle = tokio::spawn(async move {
        if let Err(e) = client.run(event_tx).await {
            error!("Gateway event loop ended: {}", e);
        }
    });

    let run_id = RunId("daemon".to_string());
    let mut flush_interval = interval(Duration::from_millis(flush_interval_ms));
    // Poll shutdown flag every second
    let mut shutdown_check = interval(Duration::from_secs(1));

    let result = loop {
        tokio::select! {
            msg = event_rx.recv() => {
                match msg {
                    Some(gw_event) => {
                        let kind_name = match gw_event.event.as_str() {
                            "agent" => "AGENT_EVENT",
                            "chat" => "OUTPUT_CHUNK",
                            "tick" => "TICK",
                            "presence" => "PRESENCE",
                            "shutdown" => "SHUTDOWN",
                            _ => "CUSTOM",
                        };

                        let event = gateway_event_to_event(
                            &run_id,
                            EventId(0), // ledger assigns the real ID
                            gw_event,
                            redact,
                        );

                        {
                            let mut l = ledger.lock().await;
                            if let Err(e) = l.append_event(event) {
                                error!("Failed to write event: {}", e);
                            }
                        }

                        let total = {
                            let l = ledger.lock().await;
                            l.total_events()
                        };

                        pb.set_message(format!(
                            "{} events | Last: {}",
                            total, kind_name
                        ));
                    }
                    None => {
                        break ShutdownReason::Disconnected;
                    }
                }
            }

            _ = flush_interval.tick() => {
                let mut l = ledger.lock().await;
                if let Err(e) = l.flush() {
                    error!("Failed to flush: {}", e);
                }
            }

            _ = shutdown_check.tick() => {
                if shutdown.load(Ordering::SeqCst) {
                    break ShutdownReason::Signal;
                }
            }
        }
    };

    // Abort the spawned gateway reader to avoid leaking tasks
    handle.abort();

    Ok(result)
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
