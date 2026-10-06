/**
 * OpenRemote WebRTC Signaling Server
 * High performance, zero-dependency WebSocket rendezvous server.
 */

const http = require("http");
const { WebSocketServer, WebSocket } = require("ws");

const PORT = parseInt(process.env.PORT || "8765", 10);
const HOST = process.env.HOST || "0.0.0.0";

// Map normalized 9-digit peer ID -> { ws, rawPeerId, registeredAt }
const peers = new Map();
// Map WebSocket instance -> normalized 9-digit peer ID
const sockets = new Map();

/**
 * Normalizes peer IDs by stripping all non-digits (e.g. "901-435-944" -> "901435944")
 * @param {string} id
 * @returns {string}
 */
function normalizePeerId(id) {
  if (!id) return "";
  return String(id).replace(/\D/g, "");
}

// Create HTTP server for health checks & WebSocket upgrade
const server = http.createServer((req, res) => {
  // CORS headers
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "GET, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  if (req.url === "/health" || req.url === "/") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        status: "ok",
        service: "openremote-signaling-server",
        version: "1.0.0",
        activePeers: peers.size,
        uptimeSeconds: Math.floor(process.uptime()),
        timestamp: Date.now(),
      })
    );
    return;
  }

  res.writeHead(404, { "Content-Type": "application/json" });
  res.end(JSON.stringify({ error: "Not found" }));
});

const wss = new WebSocketServer({ server });

/**
 * Sends a JSON signaling envelope to a WebSocket client
 * @param {WebSocket} ws
 * @param {string} action
 * @param {object} payload
 */
function sendJson(ws, action, payload = {}) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    try {
      ws.send(
        JSON.stringify({
          type: action,
          action,
          ...payload,
          payload,
        })
      );
    } catch (err) {
      console.error("[signaling] Failed to send message:", err.message);
    }
  }
}

/**
 * Cleans up and unregisters a disconnected socket
 * @param {WebSocket} ws
 */
function cleanupSocket(ws) {
  const normId = sockets.get(ws);
  if (normId) {
    const existing = peers.get(normId);
    if (existing && existing.ws === ws) {
      peers.delete(normId);
      console.log(`[signaling] Peer unregistered: ${existing.rawPeerId} (${normId}) - Remaining: ${peers.size}`);
    }
    sockets.delete(ws);
  }
}

wss.on("connection", (ws, req) => {
  const remoteIp = req.socket.remoteAddress || "unknown";
  ws.isAlive = true;

  ws.on("pong", () => {
    ws.isAlive = true;
  });

  ws.on("message", (raw) => {
    let msg;
    try {
      msg = JSON.parse(raw.toString());
    } catch (_e) {
      sendJson(ws, "error", { message: "Invalid JSON format" });
      return;
    }

    const action = msg.type || msg.action;
    const payload = msg.payload || msg;

    switch (action) {
      case "register": {
        const rawPeerId = String(payload.peerId || payload.peer_id || msg.peerId || msg.peer_id || "").trim();
        const normId = normalizePeerId(rawPeerId);

        if (!normId || normId.length < 6) {
          sendJson(ws, "registered", {
            peer_id: rawPeerId,
            peerId: rawPeerId,
            success: false,
            reason: "Invalid peer ID format",
          });
          return;
        }

        // If old socket exists for this ID, close it
        const oldEntry = peers.get(normId);
        if (oldEntry && oldEntry.ws !== ws) {
          try {
            oldEntry.ws.close(1000, "Replaced by new registration");
          } catch (_e) {}
        }

        peers.set(normId, {
          ws,
          rawPeerId: normId,
          registeredAt: Date.now(),
        });
        sockets.set(ws, normId);

        console.log(`[signaling] Registered: ${rawPeerId} (${normId}) from ${remoteIp} - Total active: ${peers.size}`);

        sendJson(ws, "registered", {
          peer_id: normId,
          peerId: normId,
          success: true,
          active_peers: peers.size,
          activePeers: peers.size,
        });
        break;
      }

      case "lookup": {
        const targetRaw = payload.target || msg.target || "";
        const target = normalizePeerId(targetRaw);
        const online = peers.has(target);
        sendJson(ws, "lookup_result", {
          target: target,
          targetId: target,
          online,
        });
        break;
      }

      case "offer": {
        const senderNormId = sockets.get(ws);
        const senderEntry = senderNormId ? peers.get(senderNormId) : null;
        const fromRaw = payload.from || msg.from || (senderEntry ? senderEntry.rawPeerId : senderNormId) || "unknown";
        const fromNorm = normalizePeerId(fromRaw) || senderNormId || fromRaw;

        const targetRaw = payload.target || msg.target || "";
        const targetNormId = normalizePeerId(targetRaw);
        const targetEntry = peers.get(targetNormId);

        if (!targetEntry || targetEntry.ws.readyState !== WebSocket.OPEN) {
          console.log(`[signaling] Offer target ${targetRaw} (${targetNormId}) is OFFLINE. Notifying sender.`);
          sendJson(ws, "peer_not_found", {
            target: targetRaw,
            targetId: targetNormId,
            reason: "Partner ID is offline or not registered.",
          });
          return;
        }

        console.log(`[signaling] Routing OFFER from ${fromRaw} (${fromNorm}) -> ${targetEntry.rawPeerId} (${targetNormId})`);
        sendJson(targetEntry.ws, "offer", {
          target: targetNormId,
          targetId: targetNormId,
          from: fromNorm,
          fromId: fromNorm,
          sdp: payload.sdp || msg.sdp,
        });
        break;
      }

      case "answer": {
        const senderNormId = sockets.get(ws);
        const senderEntry = senderNormId ? peers.get(senderNormId) : null;
        const fromRaw = payload.from || msg.from || (senderEntry ? senderEntry.rawPeerId : senderNormId) || "unknown";
        const fromNorm = normalizePeerId(fromRaw) || senderNormId || fromRaw;

        const targetRaw = payload.target || msg.target || "";
        const targetNormId = normalizePeerId(targetRaw);
        const targetEntry = peers.get(targetNormId);

        if (!targetEntry || targetEntry.ws.readyState !== WebSocket.OPEN) {
          sendJson(ws, "peer_not_found", {
            target: targetRaw,
            targetId: targetNormId,
            reason: "Partner ID is offline or not registered.",
          });
          return;
        }

        console.log(`[signaling] Routing ANSWER from ${fromRaw} (${fromNorm}) -> ${targetEntry.rawPeerId} (${targetNormId})`);
        sendJson(targetEntry.ws, "answer", {
          target: targetNormId,
          targetId: targetNormId,
          from: fromNorm,
          fromId: fromNorm,
          sdp: payload.sdp || msg.sdp,
        });
        break;
      }

      case "candidate": {
        const senderNormId = sockets.get(ws);
        const senderEntry = senderNormId ? peers.get(senderNormId) : null;
        const fromRaw = payload.from || msg.from || (senderEntry ? senderEntry.rawPeerId : senderNormId) || "unknown";
        const fromNorm = normalizePeerId(fromRaw) || senderNormId || fromRaw;

        const targetRaw = payload.target || msg.target || "";
        const targetNormId = normalizePeerId(targetRaw);
        const targetEntry = peers.get(targetNormId);

        if (targetEntry && targetEntry.ws.readyState === WebSocket.OPEN) {
          sendJson(targetEntry.ws, "candidate", {
            target: targetNormId,
            targetId: targetNormId,
            from: fromNorm,
            fromId: fromNorm,
            candidate: payload.candidate || msg.candidate,
          });
        }
        break;
      }

      case "ping": {
        ws.isAlive = true;
        sendJson(ws, "pong", { timestamp: Date.now() });
        break;
      }

      case "pong": {
        ws.isAlive = true;
        break;
      }

      default:
        console.warn(`[signaling] Unknown action received: ${action}`);
        break;
    }
  });

  ws.on("close", () => {
    cleanupSocket(ws);
  });

  ws.on("error", (err) => {
    console.error(`[signaling] Socket error (${remoteIp}):`, err.message);
    cleanupSocket(ws);
  });
});

// Periodic heartbeat keepalive check (every 10 seconds)
const interval = setInterval(() => {
  for (const client of wss.clients) {
    if (client.isAlive === false) {
      cleanupSocket(client);
      client.terminate();
      continue;
    }
    client.isAlive = false;
    client.ping();
  }
}, 10000);

wss.on("close", () => {
  clearInterval(interval);
});

server.listen(PORT, HOST, () => {
  console.log(`[openremote-signaling] Server running on http://${HOST}:${PORT}`);
  console.log(`[openremote-signaling] WebSocket endpoint: ws://${HOST}:${PORT}`);
  console.log(`[openremote-signaling] Health check: http://${HOST}:${PORT}/health`);
});
