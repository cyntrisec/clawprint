use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = "ws://127.0.0.1:18789";
    println!("Connecting to OpenClaw Gateway at {}...\n", url);

    let (ws_stream, response) = connect_async(url).await?;
    println!("Connected! HTTP Status: {:?}", response.status());
    println!("Headers: {:?}\n", response.headers());

    let (mut write, mut read) = ws_stream.split();

    // Try different handshake messages
    let handshakes = vec![
        json!({"type": "connect", "role": "observer", "version": "0.1.0"}),
        json!({"type": "hello", "role": "operator"}),
        json!({"action": "subscribe", "channel": "events"}),
        json!({"type": "ping"}),
    ];

    for (i, handshake) in handshakes.iter().enumerate() {
        println!("--- Test {}: {} ---", i + 1, handshake["type"]);
        
        let msg = Message::Text(handshake.to_string());
        write.send(msg).await?;
        println!("Sent: {}", handshake);

        // Wait for response with timeout
        match timeout(Duration::from_secs(5), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                println!("Received: {}", text);
                
                // Try to pretty print if JSON
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    println!("Parsed JSON:\n{}", serde_json::to_string_pretty(&json)?);
                }
            }
            Ok(Some(Ok(Message::Binary(bin)))) => {
                println!("Received binary: {} bytes", bin.len());
            }
            Ok(Some(Ok(Message::Ping(_)))) => println!("Received Ping"),
            Ok(Some(Ok(Message::Pong(_)))) => println!("Received Pong"),
            Ok(Some(Ok(Message::Close(_)))) => println!("Connection closed"),
            Ok(Some(Ok(Message::Frame(_)))) => println!("Received frame"),
            Ok(Some(Err(e))) => println!("Error: {}", e),
            Ok(None) => println!("Connection ended"),
            Err(_) => println!("Timeout (no response)"),
        }
        println!();
    }

    // Try to receive any spontaneous messages
    println!("--- Listening for spontaneous messages (10s) ---");
    let listen_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    
    while tokio::time::Instant::now() < listen_deadline {
        match timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                println!("Spontaneous: {}", text);
            }
            _ => {}
        }
    }

    println!("\nExploration complete.");
    Ok(())
}
