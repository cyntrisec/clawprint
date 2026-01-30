//! Recording session manager
//!
//! Coordinates gateway connection, event processing, and storage.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::{
    gateway::{GatewayClient, GatewayMessage},
    storage::RunStorage,
    redact::redact_json,
    Config, Event, EventId, EventKind, RunId, RunMeta,
};

/// Active recording session
pub struct RecordingSession {
    run_id: RunId,
    config: Config,
    storage: Arc<Mutex<RunStorage>>,
    shutdown_tx: mpsc::Sender<()>,
}

impl RecordingSession {
    /// Start a new recording session
    pub async fn start(config: Config, run_name: Option<String>) -> Result<Self> {
        let run_id = match run_name {
            Some(name) => RunId(name),
            None => RunId::new(),
        };

        info!("Starting recording session: {}", run_id.0);

        // Create storage
        let storage = RunStorage::new(
            run_id.clone(),
            &config.output_dir,
            config.batch_size,
        )?;

        let storage = Arc::new(Mutex::new(storage));
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        // Spawn recording task
        let config_clone = config.clone();
        let run_id_clone = run_id.clone();
        let storage_clone = storage.clone();
        
        tokio::spawn(async move {
            if let Err(e) = recording_loop(
                run_id_clone,
                config_clone,
                storage_clone,
                shutdown_rx,
            ).await {
                error!("Recording loop failed: {}", e);
            }
        });

        Ok(Self {
            run_id,
            config,
            storage,
            shutdown_tx,
        })
    }

    /// Get the run ID
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Stop the recording session gracefully
    pub async fn stop(self) -> Result<()> {
        info!("Stopping recording session: {}", self.run_id.0);
        
        // Signal shutdown
        let _ = self.shutdown_tx.send(()).await;
        
        // Give it a moment to flush
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        // Finalize storage
        let mut storage = self.storage.lock().await;
        let root_hash = storage.root_hash().unwrap_or_default();
        
        let meta = RunMeta {
            run_id: self.run_id.clone(),
            started_at: storage.load_events(Some(1))?
                .first()
                .map(|e| e.ts)
                .unwrap_or_else(chrono::Utc::now),
            ended_at: Some(chrono::Utc::now()),
            event_count: storage.event_count(),
            root_hash,
            gateway_url: self.config.gateway_url.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        
        storage.finalize(&meta)?;
        
        info!("Recording session finalized: {}", self.run_id.0);
        
        Ok(())
    }
}

/// Main recording loop
async fn recording_loop(
    run_id: RunId,
    config: Config,
    storage: Arc<Mutex<RunStorage>>,
    mut shutdown_rx: mpsc::Receiver<()>,
) -> Result<()> {
    // Connect to gateway
    let (mut client, mut gateway_rx) = GatewayClient::new(&config.gateway_url)?;
    let session_id = client.connect().await?;
    
    info!("Recording loop started, session: {}", session_id);

    // Write RUN_START event
    let start_event = Event::new(
        run_id.clone(),
        EventId(1),
        EventKind::RunStart,
        serde_json::json!({
            "gateway_url": config.gateway_url,
            "session_id": session_id,
        }),
        None,
    );
    
    {
        let mut storage = storage.lock().await;
        storage.write_event(start_event)?;
    }

    let mut event_counter: u64 = 2;
    let mut flush_interval = interval(Duration::from_millis(config.flush_interval_ms));

    loop {
        tokio::select! {
            // Incoming gateway messages
            Some(msg) = gateway_rx.recv() => {
                debug!("Received gateway message: {:?}", msg);
                
                let event = match gateway_message_to_event(
                    &run_id,
                    EventId(event_counter),
                    msg,
                    &storage,
                ).await {
                    Ok(Some(event)) => event,
                    Ok(None) => continue,
                    Err(e) => {
                        warn!("Failed to convert message: {}", e);
                        continue;
                    }
                };

                {
                    let mut storage = storage.lock().await;
                    if let Err(e) = storage.write_event(event) {
                        error!("Failed to write event: {}", e);
                    }
                }
                
                event_counter += 1;
            }
            
            // Periodic flush
            _ = flush_interval.tick() => {
                let mut storage = storage.lock().await;
                if let Err(e) = storage.flush() {
                    error!("Failed to flush: {}", e);
                }
            }
            
            // Shutdown signal
            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal");
                break;
            }
        }
    }

    // Write RUN_END event
    let end_event = Event::new(
        run_id.clone(),
        EventId(event_counter),
        EventKind::RunEnd,
        serde_json::json!({
            "session_id": session_id,
            "total_events": event_counter,
        }),
        None, // Will be set by storage
    );
    
    {
        let mut storage = storage.lock().await;
        storage.write_event(end_event)?;
        storage.flush()?;
    }

    info!("Recording loop ended, {} events captured", event_counter);
    
    Ok(())
}

/// Convert gateway message to event
async fn gateway_message_to_event(
    run_id: &RunId,
    event_id: EventId,
    msg: GatewayMessage,
    _storage: &Arc<Mutex<RunStorage>>,
) -> Result<Option<Event>> {
    let (kind, payload, span_id, actor) = match msg {
        GatewayMessage::AgentEvent { run_id: msg_run_id, event } => {
            // Filter events not from our tracked run (if specified)
            (
                EventKind::AgentEvent,
                event,
                None,
                Some(msg_run_id),
            )
        }
        GatewayMessage::ToolCall { run_id, tool, args, span_id } => {
            let mut payload = serde_json::json!({
                "tool": tool,
                "args": args,
            });
            
            // Redact secrets if configured
            redact_json(&mut payload);
            
            (
                EventKind::ToolCall,
                payload,
                Some(span_id),
                Some(run_id),
            )
        }
        GatewayMessage::ToolResult { run_id, span_id, mut result, duration_ms } => {
            redact_json(&mut result);
            
            let payload = serde_json::json!({
                "result": result,
                "duration_ms": duration_ms,
            });
            
            (
                EventKind::ToolResult,
                payload,
                Some(span_id),
                Some(run_id),
            )
        }
        GatewayMessage::OutputChunk { run_id, content } => {
            (
                EventKind::OutputChunk,
                serde_json::json!({"content": content}),
                None,
                Some(run_id),
            )
        }
        GatewayMessage::Presence { timestamp } => {
            (
                EventKind::Presence,
                serde_json::json!({"timestamp": timestamp}),
                None,
                None,
            )
        }
        GatewayMessage::Tick { timestamp } => {
            (
                EventKind::Tick,
                serde_json::json!({"timestamp": timestamp}),
                None,
                None,
            )
        }
        GatewayMessage::Shutdown { reason } => {
            (
                EventKind::Shutdown,
                serde_json::json!({"reason": reason}),
                None,
                None,
            )
        }
        GatewayMessage::Ping | GatewayMessage::Pong => {
            return Ok(None); // Skip ping/pong
        }
        _ => {
            return Ok(None); // Skip other messages
        }
    };

    let mut event = Event::new(
        run_id.clone(),
        event_id,
        kind,
        payload,
        None, // hash_prev set by storage
    );
    
    event.span_id = span_id;
    event.actor = actor;
    
    Ok(Some(event))
}
