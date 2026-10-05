# OpenRemote WebRTC Signaling Server

Lightweight, ultra-fast WebRTC signaling and rendezvous server for **OpenRemote**. Enables peer-to-peer desktop streaming and remote control across disparate Wi-Fi networks and public Internet using Google Public STUN servers.

---

## Features

- **9-Digit Peer ID Routing**: Routes SDP offers, answers, and ICE candidates by normalized peer IDs (e.g. `901-435-944` ↔ `901435944`).
- **Real-Time Offline Detection**: Automatically notifies dialing clients when partner ID is offline (`"Partner ID is offline or not registered."`).
- **Zero Heavy Dependencies**: Built on lightweight, robust WebSockets (`ws`).
- **Heartbeat & Self-Cleaning**: Prunes disconnected or silent peers every 30 seconds.
- **Health Check API**: Includes `GET /health` endpoint for uptime and monitoring.
- **Cloudflare Worker Ready**: Includes `worker.js` for instant serverless edge deployment with Cloudflare Durable Objects.

---

## Quickstart (Node.js)

### 1. Install Dependencies
```bash
npm install
```

### 2. Start the Server
```bash
npm start
```
By default, the server listens on `http://0.0.0.0:8765` (`ws://0.0.0.0:8765`).

To specify a custom port or host:
```bash
PORT=9000 HOST=127.0.0.1 node server.js
```

### 3. Verify Health
Visit `http://localhost:8765/health` in your browser:
```json
{
  "status": "ok",
  "service": "openremote-signaling-server",
  "version": "1.0.0",
  "activePeers": 0,
  "uptimeSeconds": 12,
  "timestamp": 1728000000000
}
```

---

## Deployment Options

### Option A: Systemd Service (Linux VPS / Ubuntu)
```ini
[Unit]
Description=OpenRemote Signaling Server
After=network.target

[Service]
Type=simple
User=www-data
WorkingDirectory=/opt/openremote/signaling-server
ExecStart=/usr/bin/node server.js
Restart=always
Environment=PORT=8765
Environment=HOST=0.0.0.0

[Install]
WantedBy=multi-user.target
```

### Option B: Docker Container
```dockerfile
FROM node:20-alpine
WORKDIR /app
COPY package*.json ./
RUN npm ci --only=production
COPY server.js ./
EXPOSE 8765
CMD ["node", "server.js"]
```

### Option C: Cloudflare Workers
Deploy `worker.js` to Cloudflare Workers for zero-cost, globally distributed edge signaling.

---

## Signaling Protocol Specification

All messages are JSON objects formatted as `{ "action": "<name>", "payload": { ... } }`.

### 1. Register Local Peer ID
**Request:**
```json
{
  "action": "register",
  "payload": {
    "peer_id": "901-435-944"
  }
}
```
**Response:**
```json
{
  "action": "registered",
  "payload": {
    "peer_id": "901-435-944",
    "success": true,
    "active_peers": 4
  }
}
```

### 2. SDP Offer
**Send:**
```json
{
  "action": "offer",
  "payload": {
    "target": "925-447-871",
    "from": "901-435-944",
    "sdp": "v=0\r\no=..."
  }
}
```
If the target is offline, server replies:
```json
{
  "action": "peer_not_found",
  "payload": {
    "target": "925-447-871",
    "reason": "Partner ID is offline or not registered."
  }
}
```

### 3. SDP Answer
```json
{
  "action": "answer",
  "payload": {
    "target": "901-435-944",
    "from": "925-447-871",
    "sdp": "v=0\r\no=..."
  }
}
```

### 4. ICE Candidate
```json
{
  "action": "candidate",
  "payload": {
    "target": "925-447-871",
    "from": "901-435-944",
    "candidate": { ... }
  }
}
```

### 5. Keepalive Ping / Pong
```json
{ "action": "ping" }
```
Response:
```json
{ "action": "pong", "payload": { "timestamp": 1728000000000 } }
```
