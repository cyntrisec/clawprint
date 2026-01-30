#!/usr/bin/env bash
# Explore OpenClaw Gateway WebSocket protocol

echo "Connecting to OpenClaw Gateway WebSocket..."
echo "URL: ws://127.0.0.1:18789"
echo ""

# Use websocat or wscat if available, otherwise use curl for HTTP check
if command -v websocat &> /dev/null; then
    echo "{\"type\":\"connect\",\"role\":\"observer\",\"version\":\"0.1.0\"}" | websocat ws://127.0.0.1:18789
elif command -v wscat &> /dev/null; then
    echo "Using wscat (npm install -g wscat)"
    echo "Run: wscat -c ws://127.0.0.1:18789"
else
    # Check if gateway responds to HTTP
    curl -s http://127.0.0.1:18789/status 2>/dev/null || echo "No HTTP endpoint"
    echo ""
    echo "WebSocket endpoint: ws://127.0.0.1:18789"
    echo ""
    echo "Trying netcat connection test..."
    (echo -ne "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"; sleep 1) | nc localhost 18789 | head -20
fi
