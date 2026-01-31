//! Gateway WebSocket client for observing OpenClaw agent runs
//!
//! Implements OpenClaw Gateway protocol v3 (req/res/event frames)
//! to passively observe bot activity.

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

// ---------------------------------------------------------------------------
// Wire types — OpenClaw protocol v3
// ---------------------------------------------------------------------------

/// Outgoing request frame (client → gateway)
#[derive(Debug, Clone, Serialize)]
struct RequestFrame {
    #[serde(rename = "type")]
    frame_type: &'static str, // always "req"
    id: String,
    method: String,
    params: serde_json::Value,
}

/// Incoming frame (gateway → client). Tagged on `"type"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum IncomingFrame {
    /// Response to a request
    #[serde(rename = "res")]
    Response {
        id: String,
        ok: bool,
        #[serde(default)]
        payload: serde_json::Value,
        #[serde(default)]
        error: Option<ErrorPayload>,
    },
    /// Server-pushed event
    #[serde(rename = "event")]
    Event {
        event: String,
        #[serde(default)]
        payload: serde_json::Value,
        #[serde(default)]
        seq: Option<u64>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

// ---------------------------------------------------------------------------
// Public event type forwarded to the recorder
// ---------------------------------------------------------------------------

/// A gateway event forwarded to the recording layer.
#[derive(Debug, Clone)]
pub struct GatewayEvent {
    /// Event name, e.g. "agent", "chat", "tick", "presence", "shutdown"
    pub event: String,
    /// Full payload JSON
    pub payload: serde_json::Value,
    /// Server sequence number (if provided)
    pub seq: Option<u64>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client for connecting to an OpenClaw Gateway as a passive observer.
pub struct GatewayClient {
    url: Url,
    auth_token: String,
    ws_stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    conn_id: Option<String>,
}

impl GatewayClient {
    /// Create a new gateway client. Does not connect yet.
    pub fn new(url: &str, auth_token: &str) -> Result<Self> {
        let url = Url::parse(url)?;
        Ok(Self {
            url,
            auth_token: auth_token.to_string(),
            ws_stream: None,
            conn_id: None,
        })
    }

    /// Connect to gateway and perform the protocol-v3 handshake.
    /// Returns the connection ID assigned by the server.
    pub async fn connect(&mut self) -> Result<String> {
        info!("Connecting to gateway at {}", self.url);

        let (ws_stream, _) = connect_async(&self.url).await?;
        self.ws_stream = Some(ws_stream);

        // Step 1: receive connect.challenge event
        let challenge = timeout(Duration::from_secs(10), self.recv_frame()).await
            .map_err(|_| anyhow!("Handshake timeout waiting for connect.challenge"))??;

        let nonce = match &challenge {
            IncomingFrame::Event { event, payload, .. } if event == "connect.challenge" => {
                let nonce = payload.get("nonce")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("connect.challenge missing nonce"))?;
                info!("Received challenge, nonce: {}", nonce);
                nonce.to_string()
            }
            other => return Err(anyhow!("Expected connect.challenge, got: {:?}", other)),
        };

        // Step 2: send connect request with auth
        let req_id = uuid::Uuid::new_v4().to_string();
        let connect_req = RequestFrame {
            frame_type: "req",
            id: req_id.clone(),
            method: "connect".to_string(),
            params: serde_json::json!({
                "minProtocol": 3,
                "maxProtocol": 3,
                "client": {
                    "id": "gateway-client",
                    "displayName": "Clawprint Recorder",
                    "version": env!("CARGO_PKG_VERSION"),
                    "platform": std::env::consts::OS,
                    "mode": "probe"
                },
                "role": "operator",
                "auth": {
                    "token": self.auth_token
                }
            }),
        };
        self.send_json(&connect_req).await?;

        // Step 3: receive hello-ok response
        let resp = timeout(Duration::from_secs(10), self.recv_frame()).await
            .map_err(|_| anyhow!("Handshake timeout waiting for hello-ok"))??;

        match resp {
            IncomingFrame::Response { ok: true, payload, id, .. } => {
                if id != req_id {
                    warn!("Response id mismatch: expected {}, got {}", req_id, id);
                }
                let conn_id = payload
                    .pointer("/server/connId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                info!("Connected to gateway, connId: {}", conn_id);
                self.conn_id = Some(conn_id.clone());
                Ok(conn_id)
            }
            IncomingFrame::Response { ok: false, error, .. } => {
                let msg = error
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "unknown error".to_string());
                Err(anyhow!("Gateway rejected connection: {}", msg))
            }
            other => Err(anyhow!("Unexpected frame during handshake: {:?}", other)),
        }
    }

    /// Run the event loop, forwarding gateway events into `tx`.
    /// This consumes the WebSocket stream — call after `connect()`.
    pub async fn run(mut self, tx: mpsc::Sender<GatewayEvent>) -> Result<()> {
        let mut ws = self.ws_stream.take()
            .ok_or_else(|| anyhow!("Not connected"))?;

        let mut ping_interval = interval(Duration::from_secs(25));
        let mut consecutive_errors: u32 = 0;

        loop {
            tokio::select! {
                msg = ws.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            consecutive_errors = 0;
                            debug!("Received: {}", text);

                            match serde_json::from_str::<IncomingFrame>(&text) {
                                Ok(IncomingFrame::Event { event, payload, seq }) => {
                                    if tx.send(GatewayEvent { event, payload, seq }).await.is_err() {
                                        info!("Receiver dropped, stopping gateway loop");
                                        break;
                                    }
                                }
                                Ok(IncomingFrame::Response { .. }) => {
                                    // Responses to requests we didn't send; log and skip
                                    debug!("Ignoring unsolicited response");
                                }
                                Err(e) => {
                                    warn!("Failed to parse frame: {} — raw: {}", e, &text[..text.len().min(200)]);
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            ws.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("Gateway closed the connection");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            consecutive_errors += 1;
                            if consecutive_errors > 5 {
                                return Err(anyhow!("Too many consecutive WebSocket errors"));
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                        None => {
                            info!("WebSocket stream ended");
                            break;
                        }
                        _ => {} // Binary, Pong, Frame — ignore
                    }
                }

                _ = ping_interval.tick() => {
                    if let Err(e) = ws.send(Message::Ping(vec![])).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn conn_id(&self) -> Option<&str> {
        self.conn_id.as_deref()
    }

    // -- internal helpers --

    async fn send_json<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        debug!("Sending: {}", text);
        if let Some(ref mut ws) = self.ws_stream {
            ws.send(Message::Text(text)).await?;
            Ok(())
        } else {
            Err(anyhow!("Not connected"))
        }
    }

    async fn recv_frame(&mut self) -> Result<IncomingFrame> {
        if let Some(ref mut ws) = self.ws_stream {
            while let Some(msg) = ws.next().await {
                match msg? {
                    Message::Text(text) => {
                        debug!("Received: {}", text);
                        return Ok(serde_json::from_str(&text)?);
                    }
                    Message::Ping(data) => {
                        ws.send(Message::Pong(data)).await?;
                    }
                    Message::Close(_) => {
                        return Err(anyhow!("Connection closed during handshake"));
                    }
                    _ => {}
                }
            }
            Err(anyhow!("Connection closed"))
        } else {
            Err(anyhow!("Not connected"))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_frame_serialization() {
        let req = RequestFrame {
            frame_type: "req",
            id: "abc-123".to_string(),
            method: "connect".to_string(),
            params: serde_json::json!({"minProtocol": 3}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""type":"req""#));
        assert!(json.contains(r#""method":"connect""#));
        assert!(json.contains(r#""minProtocol":3"#));
    }

    #[test]
    fn test_parse_event_frame() {
        let json = r#"{"type":"event","event":"tick","payload":{"ts":1706596040000},"seq":1}"#;
        let frame: IncomingFrame = serde_json::from_str(json).unwrap();
        match frame {
            IncomingFrame::Event { event, payload, seq } => {
                assert_eq!(event, "tick");
                assert_eq!(payload["ts"], 1706596040000u64);
                assert_eq!(seq, Some(1));
            }
            _ => panic!("Expected Event frame"),
        }
    }

    #[test]
    fn test_parse_response_ok() {
        let json = r#"{"type":"res","id":"abc","ok":true,"payload":{"type":"hello-ok","server":{"connId":"conn-1"}}}"#;
        let frame: IncomingFrame = serde_json::from_str(json).unwrap();
        match frame {
            IncomingFrame::Response { ok, payload, .. } => {
                assert!(ok);
                assert_eq!(payload.pointer("/server/connId").unwrap(), "conn-1");
            }
            _ => panic!("Expected Response frame"),
        }
    }

    #[test]
    fn test_parse_response_error() {
        let json = r#"{"type":"res","id":"abc","ok":false,"error":{"code":"INVALID_REQUEST","message":"unauthorized","retryable":false}}"#;
        let frame: IncomingFrame = serde_json::from_str(json).unwrap();
        match frame {
            IncomingFrame::Response { ok, error, .. } => {
                assert!(!ok);
                let err = error.unwrap();
                assert_eq!(err.code, "INVALID_REQUEST");
                assert_eq!(err.message, "unauthorized");
            }
            _ => panic!("Expected Response frame"),
        }
    }

    #[test]
    fn test_parse_challenge_event() {
        let json = r#"{"type":"event","event":"connect.challenge","payload":{"nonce":"abc-nonce","ts":1706596010000}}"#;
        let frame: IncomingFrame = serde_json::from_str(json).unwrap();
        match frame {
            IncomingFrame::Event { event, payload, .. } => {
                assert_eq!(event, "connect.challenge");
                assert_eq!(payload["nonce"], "abc-nonce");
            }
            _ => panic!("Expected Event frame"),
        }
    }
}
