/**
 * OpenRemote WebRTC Signaling Server for Cloudflare Workers
 * Serverless global signaling using Cloudflare Durable Objects / WebSockets.
 */

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname === "/health" || url.pathname === "/") {
      return new Response(
        JSON.stringify({
          status: "ok",
          service: "openremote-signaling-worker",
          version: "1.0.0",
          edge: true,
          timestamp: Date.now(),
        }),
        {
          headers: {
            "Content-Type": "application/json",
            "Access-Control-Allow-Origin": "*",
          },
        }
      );
    }

    const upgradeHeader = request.headers.get("Upgrade");
    if (!upgradeHeader || upgradeHeader.toLowerCase() !== "websocket") {
      return new Response("Expected WebSocket Upgrade", { status: 426 });
    }

    // Connect to the global signaling room Durable Object
    const id = env.SIGNALING_ROOM ? env.SIGNALING_ROOM.idFromName("global") : null;
    if (id && env.SIGNALING_ROOM) {
      const room = env.SIGNALING_ROOM.get(id);
      return room.fetch(request);
    }

    // Fallback: WebSockets pair in Worker memory
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    server.addEventListener("message", (event) => {
      try {
        const msg = JSON.parse(event.data);
        if (msg.action === "ping") {
          server.send(JSON.stringify({ action: "pong", payload: { timestamp: Date.now() } }));
        } else if (msg.action === "register") {
          server.send(JSON.stringify({ action: "registered", payload: { peer_id: msg.payload?.peer_id, success: true } }));
        }
      } catch (_e) {}
    });

    return new Response(null, { status: 101, webSocket: client });
  },
};

/**
 * Durable Object for maintaining distributed signaling state
 */
export class SignalingRoom {
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.peers = new Map(); // normalizedId -> { ws, rawPeerId }
    this.sockets = new Map(); // ws -> normalizedId
  }

  normalizePeerId(id) {
    if (!id) return "";
    return String(id).replace(/\D/g, "");
  }

  sendJson(ws, action, payload = {}) {
    try {
      ws.send(JSON.stringify({ action, payload }));
    } catch (_e) {}
  }

  async fetch(request) {
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    server.addEventListener("message", (event) => {
      try {
        const msg = JSON.parse(event.data);
        const action = msg.action;
        const payload = msg.payload || {};

        switch (action) {
          case "register": {
            const rawId = (payload.peer_id || "").trim();
            const normId = this.normalizePeerId(rawId);
            this.peers.set(normId, { ws: server, rawPeerId: rawId });
            this.sockets.set(server, normId);
            this.sendJson(server, "registered", { peer_id: rawId, success: true, active_peers: this.peers.size });
            break;
          }
          case "offer": {
            const senderNorm = this.sockets.get(server);
            const targetNorm = this.normalizePeerId(payload.target);
            const targetEntry = this.peers.get(targetNorm);
            if (!targetEntry) {
              this.sendJson(server, "peer_not_found", {
                target: payload.target,
                reason: "Partner ID is offline or not registered.",
              });
              return;
            }
            this.sendJson(targetEntry.ws, "offer", {
              target: payload.target,
              from: payload.from || senderNorm,
              sdp: payload.sdp,
            });
            break;
          }
          case "answer": {
            const senderNorm = this.sockets.get(server);
            const targetNorm = this.normalizePeerId(payload.target);
            const targetEntry = this.peers.get(targetNorm);
            if (targetEntry) {
              this.sendJson(targetEntry.ws, "answer", {
                target: payload.target,
                from: payload.from || senderNorm,
                sdp: payload.sdp,
              });
            }
            break;
          }
          case "candidate": {
            const senderNorm = this.sockets.get(server);
            const targetNorm = this.normalizePeerId(payload.target);
            const targetEntry = this.peers.get(targetNorm);
            if (targetEntry) {
              this.sendJson(targetEntry.ws, "candidate", {
                target: payload.target,
                from: payload.from || senderNorm,
                candidate: payload.candidate,
              });
            }
            break;
          }
          case "ping": {
            this.sendJson(server, "pong", { timestamp: Date.now() });
            break;
          }
        }
      } catch (_e) {}
    });

    server.addEventListener("close", () => {
      const normId = this.sockets.get(server);
      if (normId) {
        this.peers.delete(normId);
        this.sockets.delete(server);
      }
    });

    return new Response(null, { status: 101, webSocket: client });
  }
}
