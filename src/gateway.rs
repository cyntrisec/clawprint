//! Gateway WebSocket client for observing agent runs
//!
//! Connects to OpenClaw Gateway control-plane bus and streams events.

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use url::Url;

/// Challenge payload from gateway
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengePayload {
    pub nonce: String,
    pub ts: u64,
}

/// Gateway protocol message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum GatewayMessage {
    /// Initial connect handshake
    Connect {
        role: String,
        version: String,
    },
    /// Connect challenge (requires auth response)
    #[serde(rename = "event")]
    ConnectChallenge {
        event: String,
        payload: ChallengePayload,
    },
    /// Auth response to challenge
    Auth {
        token: String,
        nonce: String,
    },
    /// Connect acknowledgment
    Connected {
        session_id: String,
    },
    /// Subscribe to events
    Subscribe {
        channels: Vec<String>,
    },
    /// Agent event stream
    AgentEvent {
        run_id: String,
        event: serde_json::Value,
    },
    /// Tool call started
    ToolCall {
        run_id: String,
        tool: String,
        args: serde_json::Value,
        span_id: String,
    },
    /// Tool call completed
    ToolResult {
        run_id: String,
        span_id: String,
        result: serde_json::Value,
        duration_ms: u64,
    },
    /// Output chunk (streaming)
    OutputChunk {
        run_id: String,
        content: String,
    },
    /// Presence heartbeat
    Presence {
        timestamp: String,
    },
    /// Tick
    Tick {
        timestamp: String,
    },
    /// Gateway shutting down
    Shutdown {
        reason: String,
    },
    /// Error
    Error {
        code: String,
        message: String,
    },
    /// Ping/Pong
    Ping,
    Pong,
}

/// Client for connecting to OpenClaw Gateway
pub struct GatewayClient {
    url: Url,
    ws_stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    session_id: Option<String>,
}

impl GatewayClient {
    /// Create a new gateway client (does not connect yet)
    pub fn new(url: &str) -> Result<(Self, mpsc::Receiver<GatewayMessage>)> {
        let url = Url::parse(url)?;
        let (_out_tx, out_rx) = mpsc::channel::<GatewayMessage>(1000);
        
        Ok((Self {
            url,
            ws_stream: None,
            session_id: None,
        }, out_rx))
    }

    /// Connect to gateway and perform handshake
    pub async fn connect(&mut self) -> Result<String> {
        info!("Connecting to gateway at {}", self.url);

        let (ws_stream, _) = connect_async(&self.url).await?;
        self.ws_stream = Some(ws_stream);

        // Send connect handshake
        let connect_msg = GatewayMessage::Connect {
            role: "observer".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        self.send_message(connect_msg).await?;

        // Wait for challenge or connected
        let response = timeout(Duration::from_secs(5), self.receive_message()).await
            .map_err(|_| anyhow!("Connection handshake timeout"))??;

        match response {
            GatewayMessage::ConnectChallenge { payload, .. } => {
                info!("Received challenge, nonce: {}", payload.nonce);
                // For MVP: accept any challenge without auth
                // TODO: Implement proper auth response
                
                // Wait for connected
                let response2 = timeout(Duration::from_secs(5), self.receive_message()).await
                    .map_err(|_| anyhow!("Auth response timeout"))??;
                
                match response2 {
                    GatewayMessage::Connected { session_id } => {
                        info!("Connected to gateway, session_id: {}", session_id);
                        self.session_id = Some(session_id.clone());
                        
                        // Subscribe to events
                        self.send_message(GatewayMessage::Subscribe {
                            channels: vec![
                                "agent_events".to_string(),
                                "tool_calls".to_string(),
                                "outputs".to_string(),
                            ],
                        }).await?;
                        
                        Ok(session_id)
                    }
                    _ => Err(anyhow!("Unexpected response after challenge: {:?}", response2)),
                }
            }
            GatewayMessage::Connected { session_id } => {
                info!("Connected to gateway, session_id: {}", session_id);
                self.session_id = Some(session_id.clone());
                
                self.send_message(GatewayMessage::Subscribe {
                    channels: vec![
                        "agent_events".to_string(),
                        "tool_calls".to_string(),
                        "outputs".to_string(),
                    ],
                }).await?;
                
                Ok(session_id)
            }
            GatewayMessage::Error { code, message } => {
                Err(anyhow!("Connection error: {} - {}", code, message))
            }
            _ => Err(anyhow!("Unexpected response during handshake: {:?}", response)),
        }
    }

    /// Run the event loop, forwarding messages to channel
    pub async fn run(&mut self, output_tx: mpsc::Sender<GatewayMessage>) -> Result<()> {
        let mut ws_stream = self.ws_stream.take()
            .ok_or_else(|| anyhow!("Not connected"))?;

        let mut ping_interval = interval(Duration::from_secs(30));
        let mut consecutive_errors = 0;

        loop {
            tokio::select! {
                // Incoming WebSocket message
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            debug!("Received: {}", text);
                            consecutive_errors = 0;
                            
                            match serde_json::from_str::<GatewayMessage>(&text) {
                                Ok(gateway_msg) => {
                                    if let Err(e) = output_tx.send(gateway_msg).await {
                                        error!("Failed to forward message: {}", e);
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to parse message: {} - raw: {}", e, text);
                                    // Store raw message for later analysis
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            ws_stream.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("Connection closed by gateway");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            consecutive_errors += 1;
                            if consecutive_errors > 5 {
                                return Err(anyhow!("Too many consecutive errors"));
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                        _ => {}
                    }
                }
                
                // Periodic ping
                _ = ping_interval.tick() => {
                    if let Err(e) = ws_stream.send(Message::Ping(vec![])).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn send_message(&mut self, msg: GatewayMessage) -> Result<()> {
        let text = serde_json::to_string(&msg)?;
        if let Some(ref mut ws) = self.ws_stream {
            ws.send(Message::Text(text)).await?;
            Ok(())
        } else {
            Err(anyhow!("Not connected"))
        }
    }

    async fn receive_message(&mut self) -> Result<GatewayMessage> {
        if let Some(ref mut ws) = self.ws_stream {
            while let Some(msg) = ws.next().await {
                match msg? {
                    Message::Text(text) => {
                        return Ok(serde_json::from_str(&text)?);
                    }
                    Message::Ping(data) => {
                        ws.send(Message::Pong(data)).await?;
                    }
                    _ => {}
                }
            }
            Err(anyhow!("Connection closed"))
        } else {
            Err(anyhow!("Not connected"))
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = GatewayMessage::Connect {
            role: "observer".to_string(),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"connect\""));
        assert!(json.contains("observer"));
    }
}
