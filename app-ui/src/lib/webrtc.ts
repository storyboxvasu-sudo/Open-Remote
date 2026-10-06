/**
 * OpenRemote WebRTC & Signaling Client
 * Handles public Internet signaling, Google STUN NAT traversal, and P2P data channels.
 */

import { invoke } from "@tauri-apps/api/core";

export type SignalingStatus = "connecting" | "online" | "offline" | "error";

export interface SignalingMessage {
  action: string;
  payload?: any;
}

const GOOGLE_STUN_CONFIG: RTCConfiguration = {
  iceServers: [
    { urls: "stun:stun.l.google.com:19302" },
    { urls: "stun:stun1.l.google.com:19302" },
    { urls: "stun:stun2.l.google.com:19302" },
  ],
  iceCandidatePoolSize: 4,
};

// Chunking constants for RTCDataChannel safety across WAN
const CHUNK_MAGIC_0 = 0xff;
const CHUNK_MAGIC_1 = 0xfe;
const CHUNK_HEADER_SIZE = 8;
const MAX_CHUNK_PAYLOAD = 32 * 1024; // 32KB

export class SignalingClient {
  private ws: WebSocket | null = null;
  private url: string;
  private myPeerId: string = "";
  private reconnectTimer: any = null;
  private pingTimer: any = null;
  private isDestroyed: boolean = false;
  private reconnectAttempts: number = 0;

  public status: SignalingStatus = "offline";
  public onStatusChange?: (status: SignalingStatus, details?: string) => void;
  public onOffer?: (from: string, sdp: string) => void;
  public onAnswer?: (from: string, sdp: string) => void;
  public onCandidate?: (from: string, candidate: any) => void;
  public onPeerNotFound?: (target: string, reason: string) => void;

  constructor(defaultUrl: string = "wss://signaling.openremote.app") {
    this.url = defaultUrl;
  }

  public setUrl(newUrl: string) {
    if (this.url !== newUrl) {
      this.url = newUrl;
      this.reconnect(0);
    }
  }

  public getUrl(): string {
    return this.url;
  }

  public start(myPeerId: string) {
    this.myPeerId = (myPeerId || "").replace(/\D/g, "");
    this.isDestroyed = false;
    this.connect();
  }

  public stop() {
    this.isDestroyed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    if (this.pingTimer) clearInterval(this.pingTimer);
    if (this.ws) {
      try {
        this.ws.close();
      } catch (_e) {}
      this.ws = null;
    }
    this.setStatus("offline");
  }

  public updatePeerId(peerId: string) {
    const cleanId = (peerId || "").replace(/\D/g, "");
    this.myPeerId = cleanId;
    if (this.status === "online" && this.ws?.readyState === WebSocket.OPEN && cleanId) {
      this.ws.send(
        JSON.stringify({
          type: "register",
          action: "register",
          peerId: cleanId,
          peer_id: cleanId,
          payload: { peerId: cleanId, peer_id: cleanId },
        })
      );
    }
  }

  private setStatus(status: SignalingStatus, details?: string) {
    this.status = status;
    if (this.onStatusChange) {
      this.onStatusChange(status, details);
    }
  }

  private connect() {
    if (this.isDestroyed || !this.url) return;

    if (this.ws) {
      try {
        this.ws.close();
      } catch (_e) {}
      this.ws = null;
    }

    this.setStatus("connecting");

    try {
      const socket = new WebSocket(this.url);
      this.ws = socket;

      socket.onopen = () => {
        if (this.ws !== socket) return;
        this.reconnectAttempts = 0;
        this.setStatus("online");
        console.log(`[signaling] Connected to ${this.url}`);

        if (this.myPeerId) {
          const cleanLocalId = this.myPeerId.replace(/\D/g, "");
          this.ws.send(
            JSON.stringify({
              type: "register",
              action: "register",
              peerId: cleanLocalId,
              peer_id: cleanLocalId,
              payload: { peerId: cleanLocalId, peer_id: cleanLocalId },
            })
          );
        }

        // Heartbeat ping every 10s
        if (this.pingTimer) clearInterval(this.pingTimer);
        this.pingTimer = setInterval(() => {
          if (this.ws?.readyState === WebSocket.OPEN) {
            this.send("ping", { timestamp: Date.now() });
          }
        }, 10000);
      };

      socket.onmessage = (event) => {
        if (this.ws !== socket) return;
        try {
          const msg = JSON.parse(event.data);
          this.handleMessage(msg);
        } catch (err) {
          console.warn("[signaling] Invalid JSON received:", err);
        }
      };

      socket.onerror = (err) => {
        if (this.ws !== socket) return;
        console.warn("[signaling] Connection notice / error:", err);
        this.setStatus("error", "Signaling server unreachable");
      };

      socket.onclose = () => {
        if (this.ws !== socket) return;
        if (this.pingTimer) clearInterval(this.pingTimer);
        this.setStatus("offline");
        if (!this.isDestroyed) {
          this.scheduleReconnect();
        }
      };
    } catch (err: any) {
      console.warn("[signaling] Failed to open WebSocket:", err);
      this.setStatus("error", err?.message || "Failed to initialize WebSocket");
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect() {
    if (this.isDestroyed) return;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectAttempts++;
    // Exponential backoff with jitter: 2s, 4s, 8s, max 15s
    const delay = Math.min(15000, 1000 * Math.pow(1.8, this.reconnectAttempts));
    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, delay);
  }

  public reconnect(delay: number = 500) {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.reconnectAttempts = 0;
    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, delay);
  }

  public send(action: string, payload: any = {}) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(
        JSON.stringify({
          type: action,
          action,
          ...payload,
          payload,
        })
      );
    }
  }

  private handleMessage(msg: any) {
    const action = msg.type || msg.action;
    const payload = msg.payload || msg;

    switch (action) {
      case "registered":
        const regId = payload.peerId || payload.peer_id || msg.peerId || msg.peer_id;
        console.log(`[signaling] Registered successfully as ${regId}`);
        this.setStatus("online");
        break;

      case "offer":
        if (this.onOffer) {
          const from = payload.from || msg.from;
          const sdp = payload.sdp || msg.sdp;
          this.onOffer(from, sdp);
        }
        break;

      case "answer":
        if (this.onAnswer) {
          const from = payload.from || msg.from;
          const sdp = payload.sdp || msg.sdp;
          this.onAnswer(from, sdp);
        }
        break;

      case "candidate":
        if (this.onCandidate) {
          const from = payload.from || msg.from;
          const candidate = payload.candidate || msg.candidate;
          this.onCandidate(from, candidate);
        }
        break;

      case "peer_not_found":
        if (this.onPeerNotFound) {
          const target = payload.target || msg.target;
          const reason = payload.reason || msg.reason || "Partner ID is offline or not registered.";
          this.onPeerNotFound(target, reason);
        }
        break;

      case "pong":
        // Heartbeat response
        break;

      default:
        break;
    }
  }
}

/**
 * Reassembles chunked RTCDataChannel binary frames
 */
class FrameChunkReassembler {
  private packets = new Map<number, { chunks: (Uint8Array | null)[]; received: number; total: number; timestamp: number }>();

  public processChunk(buffer: ArrayBuffer): ArrayBuffer | null {
    if (buffer.byteLength < CHUNK_HEADER_SIZE) return buffer;

    const bytes = new Uint8Array(buffer);
    if (bytes[0] === CHUNK_MAGIC_0 && bytes[1] === CHUNK_MAGIC_1) {
      const view = new DataView(buffer);
      const packetId = view.getUint16(2);
      const chunkIdx = view.getUint16(4);
      const totalChunks = view.getUint16(6);

      let record = this.packets.get(packetId);
      if (!record) {
        record = {
          chunks: new Array(totalChunks).fill(null),
          received: 0,
          total: totalChunks,
          timestamp: Date.now(),
        };
        this.packets.set(packetId, record);
      }

      if (record.chunks[chunkIdx] === null) {
        record.chunks[chunkIdx] = bytes.subarray(CHUNK_HEADER_SIZE);
        record.received++;
      }

      if (record.received === record.total) {
        this.packets.delete(packetId);
        // Calculate total length
        let totalLen = 0;
        for (const c of record.chunks) {
          if (c) totalLen += c.length;
        }
        const full = new Uint8Array(totalLen);
        let offset = 0;
        for (const c of record.chunks) {
          if (c) {
            full.set(c, offset);
            offset += c.length;
          }
        }
        return full.buffer;
      }

      // Cleanup stale incomplete packets (> 2 seconds old)
      const now = Date.now();
      for (const [id, p] of this.packets.entries()) {
        if (now - p.timestamp > 2000) {
          this.packets.delete(id);
        }
      }

      return null;
    }

    // Direct unchunked frame
    return buffer;
  }
}

/**
 * Sends a large ArrayBuffer safely over an RTCDataChannel by chunking if necessary
 */
let nextPacketId = 1;
export function sendChunkedData(channel: RTCDataChannel, buffer: ArrayBuffer) {
  if (channel.readyState !== "open") return;

  if (buffer.byteLength <= MAX_CHUNK_PAYLOAD) {
    channel.send(buffer);
    return;
  }

  const bytes = new Uint8Array(buffer);
  const totalLength = bytes.length;
  const totalChunks = Math.ceil(totalLength / MAX_CHUNK_PAYLOAD);
  const packetId = nextPacketId++ & 0xffff;

  for (let i = 0; i < totalChunks; i++) {
    const start = i * MAX_CHUNK_PAYLOAD;
    const end = Math.min(start + MAX_CHUNK_PAYLOAD, totalLength);
    const slice = bytes.subarray(start, end);

    const chunk = new Uint8Array(CHUNK_HEADER_SIZE + slice.length);
    chunk[0] = CHUNK_MAGIC_0;
    chunk[1] = CHUNK_MAGIC_1;
    const view = new DataView(chunk.buffer);
    view.setUint16(2, packetId);
    view.setUint16(4, i);
    view.setUint16(6, totalChunks);
    chunk.set(slice, CHUNK_HEADER_SIZE);

    try {
      channel.send(chunk.buffer);
    } catch (e) {
      console.warn("[webrtc] Error sending data chunk:", e);
      break;
    }
  }
}

/**
 * High-performance WebRTC Session Manager
 */
export class WebRTCSession {
  private pc: RTCPeerConnection | null = null;
  private videoChannel: RTCDataChannel | null = null;
  private controlChannel: RTCDataChannel | null = null;
  private inputChannel: RTCDataChannel | null = null;
  private reassembler = new FrameChunkReassembler();
  private hostWsFeed: WebSocket | null = null;
  private isCaller: boolean = false;
  private targetId: string = "";

  public onFrameData?: (data: ArrayBuffer) => void;
  public onControlMessage?: (msg: any) => void;
  public onConnected?: () => void;
  public onDisconnected?: () => void;
  public onError?: (err: any) => void;

  constructor(private signaling: SignalingClient, private myPeerId: string) {
    this.myPeerId = (myPeerId || "").replace(/\D/g, "");
  }

  public isConnected(): boolean {
    return (
      this.pc !== null &&
      this.pc.connectionState === "connected" &&
      (this.videoChannel?.readyState === "open" || this.isCaller === false)
    );
  }

  public sendInput(event: any) {
    if (this.inputChannel && this.inputChannel.readyState === "open") {
      try {
        this.inputChannel.send(JSON.stringify(event));
      } catch (e) {
        console.warn("[webrtc] sendInput error:", e);
      }
    }
  }

  public sendControl(msg: any) {
    if (this.controlChannel && this.controlChannel.readyState === "open") {
      try {
        this.controlChannel.send(JSON.stringify(msg));
      } catch (e) {
        console.warn("[webrtc] sendControl error:", e);
      }
    }
  }

  /**
   * Client: Initiate call to partner 9-digit peer ID over WAN via Google STUN
   */
  public async call(targetId: string): Promise<void> {
    this.close();
    this.isCaller = true;
    const cleanTargetId = (targetId || "").replace(/\D/g, "");
    const cleanMyId = (this.myPeerId || "").replace(/\D/g, "");
    this.targetId = cleanTargetId;
    this.myPeerId = cleanMyId;

    const pc = new RTCPeerConnection(GOOGLE_STUN_CONFIG);
    this.pc = pc;

    // ICE Candidate gathering
    pc.onicecandidate = (event) => {
      if (event.candidate) {
        this.signaling.send("candidate", {
          target: this.targetId,
          from: this.myPeerId,
          candidate: event.candidate.toJSON(),
        });
      }
    };

    pc.onconnectionstatechange = () => {
      console.log(`[webrtc] Connection state: ${pc.connectionState}`);
      if (pc.connectionState === "connected") {
        if (this.onConnected) this.onConnected();
      } else if (pc.connectionState === "failed" || pc.connectionState === "closed" || pc.connectionState === "disconnected") {
        if (this.onDisconnected) this.onDisconnected();
      }
    };

    // Create DataChannels
    // 1. video channel: unordered, maxRetransmits=0 for lowest video latency
    const videoChannel = pc.createDataChannel("video", {
      ordered: false,
      maxRetransmits: 0,
    });
    videoChannel.binaryType = "arraybuffer";
    videoChannel.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        const full = this.reassembler.processChunk(event.data);
        if (full && this.onFrameData) {
          this.onFrameData(full);
        }
      }
    };
    this.videoChannel = videoChannel;

    // 2. control channel: ordered reliable
    const controlChannel = pc.createDataChannel("control", { ordered: true });
    controlChannel.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);
        if (this.onControlMessage) this.onControlMessage(msg);
      } catch (_e) {}
    };
    this.controlChannel = controlChannel;

    // 3. input channel: ordered reliable
    const inputChannel = pc.createDataChannel("input", { ordered: true });
    this.inputChannel = inputChannel;

    // Create & send Offer
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);

    this.signaling.send("offer", {
      target: this.targetId,
      from: this.myPeerId,
      sdp: offer.sdp,
    });
  }

  /**
   * Host: Handle incoming WebRTC call from client
   */
  public async handleIncomingOffer(fromPeerId: string, sdp: string, hostFeedPort: number): Promise<void> {
    this.close();
    this.isCaller = false;
    const cleanFromId = (fromPeerId || "").replace(/\D/g, "");
    const cleanMyId = (this.myPeerId || "").replace(/\D/g, "");
    this.targetId = cleanFromId;
    this.myPeerId = cleanMyId;

    const pc = new RTCPeerConnection(GOOGLE_STUN_CONFIG);
    this.pc = pc;

    pc.onicecandidate = (event) => {
      if (event.candidate) {
        this.signaling.send("candidate", {
          target: this.targetId,
          from: this.myPeerId,
          candidate: event.candidate.toJSON(),
        });
      }
    };

    pc.onconnectionstatechange = () => {
      console.log(`[webrtc host] Connection state: ${pc.connectionState}`);
      if (pc.connectionState === "connected") {
        if (this.onConnected) this.onConnected();
      } else if (pc.connectionState === "failed" || pc.connectionState === "closed" || pc.connectionState === "disconnected") {
        if (this.onDisconnected) this.onDisconnected();
      }
    };

    // Listen for incoming DataChannels from caller
    pc.ondatachannel = (event) => {
      const channel = event.channel;
      if (channel.label === "video") {
        this.videoChannel = channel;
        channel.binaryType = "arraybuffer";
        channel.onopen = () => {
          console.log("[webrtc host] Video data channel open. Connecting to host frame feed...");
          this.startHostFrameFeed(hostFeedPort);
        };
      } else if (channel.label === "control") {
        this.controlChannel = channel;
        channel.onmessage = (e) => {
          try {
            const msg = JSON.parse(e.data);
            if (this.onControlMessage) this.onControlMessage(msg);
          } catch (_e) {}
        };
      } else if (channel.label === "input") {
        this.inputChannel = channel;
        channel.onmessage = async (e) => {
          try {
            const inputEvent = JSON.parse(e.data);
            await invoke("inject_host_input", { event: inputEvent });
          } catch (_e) {}
        };
      }
    };

    await pc.setRemoteDescription({ type: "offer", sdp });
    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);

    this.signaling.send("answer", {
      target: cleanFromId,
      from: this.myPeerId,
      sdp: answer.sdp,
    });
  }

  public async handleAnswer(sdp: string) {
    if (this.pc) {
      await this.pc.setRemoteDescription({ type: "answer", sdp });
    }
  }

  public async addIceCandidate(candidate: any) {
    if (this.pc) {
      try {
        await this.pc.addIceCandidate(new RTCIceCandidate(candidate));
      } catch (e) {
        console.warn("[webrtc] Error adding ICE candidate:", e);
      }
    }
  }

  private startHostFrameFeed(hostFeedPort: number) {
    if (this.hostWsFeed) {
      try {
        this.hostWsFeed.close();
      } catch (_e) {}
      this.hostWsFeed = null;
    }

    try {
      const feedSocket = new WebSocket(`ws://127.0.0.1:${hostFeedPort}`);
      feedSocket.binaryType = "arraybuffer";

      feedSocket.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer && this.videoChannel && this.videoChannel.readyState === "open") {
          sendChunkedData(this.videoChannel, event.data);
        }
      };

      feedSocket.onerror = (err) => {
        console.warn("[webrtc host] Host frame feed error:", err);
      };

      feedSocket.onclose = () => {
        console.log("[webrtc host] Host frame feed closed");
      };

      this.hostWsFeed = feedSocket;
    } catch (err) {
      console.error("[webrtc host] Failed to connect to host frame feed:", err);
    }
  }

  public close() {
    if (this.hostWsFeed) {
      try {
        this.hostWsFeed.close();
      } catch (_e) {}
      this.hostWsFeed = null;
    }

    if (this.videoChannel) {
      try {
        this.videoChannel.close();
      } catch (_e) {}
      this.videoChannel = null;
    }

    if (this.controlChannel) {
      try {
        this.controlChannel.close();
      } catch (_e) {}
      this.controlChannel = null;
    }

    if (this.inputChannel) {
      try {
        this.inputChannel.close();
      } catch (_e) {}
      this.inputChannel = null;
    }

    if (this.pc) {
      try {
        this.pc.close();
      } catch (_e) {}
      this.pc = null;
    }
  }
}
