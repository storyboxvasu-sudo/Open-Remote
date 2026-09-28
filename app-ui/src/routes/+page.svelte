<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  interface SystemInfo {
    peer_id: string;
    lan_ip: string;
    default_control_port: number;
    default_video_port: number;
    is_hosting: boolean;
    is_connected: boolean;
  }

  interface HostStatus {
    success: boolean;
    control_port: number;
    video_port: number;
    message: string;
  }

  interface ClientConnectResult {
    success: boolean;
    local_ws_port: number;
    target_ip: string;
    message: string;
  }

  // Reactive state
  let systemInfo = $state<SystemInfo | null>(null);
  let activeTab = $state<"connect" | "host">("connect");
  let isHosting = $state(false);
  let hostStatusMessage = $state("");
  let targetIp = $state("");
  let targetPort = $state(44321);
  let isConnecting = $state(false);
  let isConnected = $state(false);
  let connectionError = $state("");

  // Session metrics
  let fps = $state(0);
  let rttMs = $state(0);
  let frameCount = $state(0);
  let remoteResolution = $state({ width: 0, height: 0 });
  let isFullscreen = $state(false);

  // References
  let canvasRef = $state<HTMLCanvasElement | null>(null);
  let canvasContainerRef = $state<HTMLDivElement | null>(null);
  let ws: WebSocket | null = null;
  let fpsInterval: number | null = null;
  let lastMoveTime = 0;

  async function loadSystemInfo() {
    try {
      const info: SystemInfo = await invoke("get_system_info");
      systemInfo = info;
      isHosting = info.is_hosting;
      isConnected = info.is_connected;
      if (!targetIp && info.lan_ip) {
        // Pre-fill target ip subnet hint
        const parts = info.lan_ip.split(".");
        if (parts.length === 4) {
          targetIp = `${parts[0]}.${parts[1]}.${parts[2]}.`;
        }
      }
    } catch (err) {
      console.error("Failed to fetch system info:", err);
    }
  }

  async function toggleHosting() {
    if (isHosting) {
      try {
        await invoke("stop_hosting");
        isHosting = false;
        hostStatusMessage = "Host stopped";
      } catch (err: any) {
        hostStatusMessage = `Error stopping host: ${err}`;
      }
    } else {
      try {
        const res: HostStatus = await invoke("start_hosting", { port: 44321 });
        if (res.success) {
          isHosting = true;
          hostStatusMessage = `Hosting active on UDP:${res.control_port} (Input) / TCP:${res.video_port} (Stream)`;
        }
      } catch (err: any) {
        hostStatusMessage = `Failed to start host: ${err}`;
      }
    }
  }

  async function connectToRemote() {
    if (!targetIp || isConnecting) return;
    isConnecting = true;
    connectionError = "";

    try {
      const res: ClientConnectResult = await invoke("connect_to_remote", {
        targetIp: targetIp.trim(),
        port: Number(targetPort),
      });

      if (res.success) {
        isConnected = true;
        isConnecting = false;
        initStreamWebSocket(res.local_ws_port);
      }
    } catch (err: any) {
      connectionError = typeof err === "string" ? err : JSON.stringify(err);
      isConnecting = false;
      isConnected = false;
    }
  }

  async function disconnectRemote() {
    if (ws) {
      ws.close();
      ws = null;
    }
    try {
      await invoke("disconnect_remote");
    } catch (err) {
      console.error("Disconnect error:", err);
    }
    isConnected = false;
    remoteResolution = { width: 0, height: 0 };
    fps = 0;
  }

  function initStreamWebSocket(port: number) {
    if (ws) {
      ws.close();
    }

    const socket = new WebSocket(`ws://127.0.0.1:${port}`);
    socket.binaryType = "arraybuffer";

    socket.onopen = () => {
      console.log("Local stream pipe established on port", port);
    };

    socket.onmessage = async (event: MessageEvent) => {
      if (!(event.data instanceof ArrayBuffer)) return;
      frameCount++;

      const buffer = event.data;
      if (buffer.byteLength < 16) return;

      const view = new DataView(buffer);
      const width = view.getUint32(0);
      const height = view.getUint32(4);
      const timestampMs = Number(view.getBigUint64(8));

      // Calculate latency
      const now = Date.now();
      if (timestampMs > 0 && now >= timestampMs) {
        rttMs = now - timestampMs;
      }

      remoteResolution = { width, height };

      if (!canvasRef) return;
      const ctx = canvasRef.getContext("2d");
      if (!ctx) return;

      if (canvasRef.width !== width || canvasRef.height !== height) {
        canvasRef.width = width;
        canvasRef.height = height;
      }

      const pixelBytes = new Uint8ClampedArray(buffer, 16);
      const imageData = new ImageData(pixelBytes, width, height);

      // Render onto Canvas
      try {
        if ("createImageBitmap" in window) {
          const bitmap = await createImageBitmap(imageData);
          ctx.drawImage(bitmap, 0, 0);
          bitmap.close();
        } else {
          ctx.putImageData(imageData, 0, 0);
        }
      } catch (e) {
        ctx.putImageData(imageData, 0, 0);
      }
    };

    socket.onerror = (err) => {
      console.error("Stream socket error:", err);
    };

    socket.onclose = () => {
      console.log("Stream socket closed");
      if (isConnected) {
        disconnectRemote();
      }
    };

    ws = socket;
  }

  // Remote Input Injection Handlers
  function getNormalizedCoordinates(e: MouseEvent): { x: number; y: number } {
    if (!canvasRef) return { x: 0, y: 0 };
    const rect = canvasRef.getBoundingClientRect();
    const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
    return { x, y };
  }

  async function handleMouseMove(e: MouseEvent) {
    if (!isConnected) return;
    const now = performance.now();
    // Throttle mouse moves to ~120Hz (8ms)
    if (now - lastMoveTime < 8) return;
    lastMoveTime = now;

    const { x, y } = getNormalizedCoordinates(e);
    await invoke("send_input", {
      event: {
        type: "MouseMove",
        data: { x, y },
      },
    });
  }

  async function handleMouseDown(e: MouseEvent) {
    if (!isConnected) return;
    e.preventDefault();
    const { x, y } = getNormalizedCoordinates(e);
    const button = e.button === 0 ? "Left" : e.button === 1 ? "Middle" : "Right";

    await invoke("send_input", {
      event: {
        type: "MouseDown",
        data: { button, x, y },
      },
    });
  }

  async function handleMouseUp(e: MouseEvent) {
    if (!isConnected) return;
    e.preventDefault();
    const { x, y } = getNormalizedCoordinates(e);
    const button = e.button === 0 ? "Left" : e.button === 1 ? "Middle" : "Right";

    await invoke("send_input", {
      event: {
        type: "MouseUp",
        data: { button, x, y },
      },
    });
  }

  async function handleWheel(e: WheelEvent) {
    if (!isConnected) return;
    e.preventDefault();
    await invoke("send_input", {
      event: {
        type: "MouseWheel",
        data: {
          delta_x: Math.round(e.deltaX),
          delta_y: Math.round(e.deltaY),
        },
      },
    });
  }

  async function handleKeyDown(e: KeyboardEvent) {
    if (!isConnected) return;
    e.preventDefault();
    await invoke("send_input", {
      event: {
        type: "KeyDown",
        data: {
          scancode: e.keyCode,
          key: e.key,
        },
      },
    });
  }

  async function handleKeyUp(e: KeyboardEvent) {
    if (!isConnected) return;
    e.preventDefault();
    await invoke("send_input", {
      event: {
        type: "KeyUp",
        data: {
          scancode: e.keyCode,
          key: e.key,
        },
      },
    });
  }

  function toggleFullscreen() {
    if (!canvasContainerRef) return;
    if (!document.fullscreenElement) {
      canvasContainerRef.requestFullscreen().then(() => {
        isFullscreen = true;
      });
    } else {
      document.exitFullscreen().then(() => {
        isFullscreen = false;
      });
    }
  }

  function copyToClipboard(text: string) {
    navigator.clipboard.writeText(text);
  }

  onMount(() => {
    loadSystemInfo();

    fpsInterval = window.setInterval(() => {
      fps = frameCount;
      frameCount = 0;
    }, 1000);
  });

  onDestroy(() => {
    if (fpsInterval) clearInterval(fpsInterval);
    if (ws) ws.close();
  });
</script>

<div class="app-layout">
  <!-- Top Application Bar -->
  <header class="top-nav">
    <div class="brand">
      <div class="brand-icon">
        <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
      </div>
      <div class="brand-text">
        <span class="brand-title">OpenRemote</span>
        <span class="brand-badge">Unlimited P2P</span>
      </div>
    </div>

    {#if !isConnected}
      <div class="tab-controls">
        <button
          class="tab-btn"
          class:active={activeTab === "connect"}
          onclick={() => (activeTab = "connect")}
        >
          Connect to Device
        </button>
        <button
          class="tab-btn"
          class:active={activeTab === "host"}
          onclick={() => (activeTab = "host")}
        >
          Share This Screen
        </button>
      </div>
    {/if}

    <div class="top-meta">
      {#if systemInfo}
        <div class="ip-chip" title="Your Local Network Address">
          <span class="chip-label">LAN:</span>
          <span class="chip-val">{systemInfo.lan_ip}</span>
        </div>
      {/if}
      {#if isConnected}
        <div class="session-badge active">
          <span class="pulse-dot"></span>
          Connected ({fps} FPS • {rttMs}ms)
        </div>
        <button class="btn-disconnect" onclick={disconnectRemote}>
          Disconnect
        </button>
      {:else if isHosting}
        <div class="session-badge hosting">
          <span class="pulse-dot"></span>
          Hosting Live
        </div>
      {/if}
    </div>
  </header>

  <!-- Main View Area -->
  <main class="main-content">
    {#if isConnected}
      <!-- Remote Viewport View -->
      <div
        class="remote-container"
        bind:this={canvasContainerRef}
        tabindex="0"
        role="region"
        aria-label="Remote Desktop Canvas"
        onkeydown={handleKeyDown}
        onkeyup={handleKeyUp}
      >
        <!-- Remote Canvas -->
        <canvas
          bind:this={canvasRef}
          class="remote-canvas"
          onmousemove={handleMouseMove}
          onmousedown={handleMouseDown}
          onmouseup={handleMouseUp}
          onwheel={handleWheel}
          oncontextmenu={(e) => e.preventDefault()}
        ></canvas>

        <!-- Floating Quick HUD -->
        <div class="floating-hud">
          <div class="hud-item resolution">
            {remoteResolution.width}x{remoteResolution.height}
          </div>
          <div class="hud-item metric">
            {fps} FPS
          </div>
          <div class="hud-item metric">
            {rttMs} ms
          </div>
          <button class="hud-btn" onclick={toggleFullscreen} title="Toggle Fullscreen">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"></path>
            </svg>
          </button>
          <button class="hud-btn danger" onclick={disconnectRemote} title="Close Session">
            ✕
          </button>
        </div>
      </div>
    {:else}
      <!-- Dashboard Panels -->
      <div class="dashboard-grid">
        <!-- Panel 1: Connect to Remote -->
        {#if activeTab === "connect"}
          <div class="card connect-card">
            <div class="card-header">
              <h2>Connect to Remote Machine</h2>
              <p>Establish high-speed zero-latency direct LAN or WAN connection</p>
            </div>

            <div class="form-body">
              <div class="input-group">
                <label for="remote-ip">Remote Machine IP Address</label>
                <div class="input-wrapper">
                  <input
                    id="remote-ip"
                    type="text"
                    placeholder="192.168.1.xxx or 127.0.0.1"
                    bind:value={targetIp}
                  />
                </div>
              </div>

              <div class="input-group">
                <label for="remote-port">Port (Default: 44321)</label>
                <div class="input-wrapper">
                  <input
                    id="remote-port"
                    type="number"
                    bind:value={targetPort}
                  />
                </div>
              </div>

              {#if connectionError}
                <div class="error-banner">
                  {connectionError}
                </div>
              {/if}

              <button
                class="btn-primary"
                disabled={isConnecting || !targetIp}
                onclick={connectToRemote}
              >
                {#if isConnecting}
                  <span class="spinner"></span> Connecting...
                {:else}
                  Connect Now
                {/if}
              </button>

              <div class="connection-features">
                <div class="feature-item">
                  <span class="check">✓</span> Zero Session Limits
                </div>
                <div class="feature-item">
                  <span class="check">✓</span> Direct P2P / Sub-10ms Input
                </div>
                <div class="feature-item">
                  <span class="check">✓</span> GPU Hardware Screen Pipeline
                </div>
              </div>
            </div>
          </div>
        {/if}

        <!-- Panel 2: Host Screen -->
        {#if activeTab === "host"}
          <div class="card host-card">
            <div class="card-header">
              <h2>Share This Desktop</h2>
              <p>Allow authorized devices to view and control this machine</p>
            </div>

            <div class="host-details">
              {#if systemInfo}
                <div class="detail-row">
                  <div class="detail-box">
                    <span class="detail-label">Your Peer ID</span>
                    <div class="detail-val-group">
                      <span class="detail-val">{systemInfo.peer_id}</span>
                      <button class="btn-copy" onclick={() => copyToClipboard(systemInfo?.peer_id || "")}>Copy</button>
                    </div>
                  </div>

                  <div class="detail-box">
                    <span class="detail-label">Your Local LAN IP</span>
                    <div class="detail-val-group">
                      <span class="detail-val">{systemInfo.lan_ip}</span>
                      <button class="btn-copy" onclick={() => copyToClipboard(systemInfo?.lan_ip || "")}>Copy</button>
                    </div>
                  </div>
                </div>

                <div class="port-info-box">
                  <div class="port-tag">UDP Control: {systemInfo.default_control_port}</div>
                  <div class="port-tag">TCP Stream: {systemInfo.default_video_port}</div>
                  <div class="port-tag tag-success">UAC Free (Win Graphics Capture)</div>
                </div>
              {/if}

              {#if hostStatusMessage}
                <div class="status-banner" class:active={isHosting}>
                  {hostStatusMessage}
                </div>
              {/if}

              <div class="host-actions">
                <button
                  class="btn-toggle-host"
                  class:active={isHosting}
                  onclick={toggleHosting}
                >
                  {#if isHosting}
                    Stop Screen Sharing
                  {:else}
                    Start Screen Sharing
                  {/if}
                </button>
              </div>
            </div>
          </div>
        {/if}

        <!-- Side Specs Card -->
        <div class="card specs-card">
          <div class="card-header">
            <h3>Engine Architecture</h3>
            <p>Native Rust Micro-Crates</p>
          </div>

          <div class="specs-list">
            <div class="spec-row">
              <span class="spec-name">Screen Capture</span>
              <span class="spec-val">Windows.Graphics.Capture</span>
            </div>
            <div class="spec-row">
              <span class="spec-name">Video Compression</span>
              <span class="spec-val">LZ4-Flex SIMD (&lt;11ms)</span>
            </div>
            <div class="spec-row">
              <span class="spec-name">Input Injection</span>
              <span class="spec-val">Win32 SendInput (Absolute)</span>
            </div>
            <div class="spec-row">
              <span class="spec-name">Network Channels</span>
              <span class="spec-val">UDP (Input) + TCP (Frames)</span>
            </div>
            <div class="spec-row">
              <span class="spec-name">License & Limits</span>
              <span class="spec-val highlight">100% Free • Unlimited</span>
            </div>
          </div>
        </div>
      </div>
    {/if}
  </main>
</div>

<style>
  :global(body) {
    margin: 0;
    padding: 0;
    background-color: #0b0f19;
    color: #f1f5f9;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
    user-select: none;
    height: 100vh;
    overflow: hidden;
  }

  .app-layout {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: radial-gradient(circle at 50% 0%, #172033 0%, #0b0f19 75%);
  }

  .top-nav {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px 24px;
    background: rgba(15, 23, 42, 0.75);
    backdrop-filter: blur(12px);
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    z-index: 50;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .brand-icon {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    background: linear-gradient(135deg, #0ea5e9, #6366f1);
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ffffff;
    box-shadow: 0 4px 12px rgba(14, 165, 233, 0.35);
  }

  .brand-title {
    font-size: 1.15rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: #ffffff;
  }

  .brand-badge {
    margin-left: 8px;
    font-size: 0.7rem;
    font-weight: 600;
    padding: 2px 7px;
    background: rgba(14, 165, 233, 0.15);
    color: #38bdf8;
    border: 1px solid rgba(14, 165, 233, 0.3);
    border-radius: 999px;
  }

  .tab-controls {
    display: flex;
    gap: 6px;
    background: rgba(30, 41, 59, 0.6);
    padding: 4px;
    border-radius: 10px;
    border: 1px solid rgba(255, 255, 255, 0.05);
  }

  .tab-btn {
    padding: 6px 16px;
    border-radius: 7px;
    border: none;
    background: transparent;
    color: #94a3b8;
    font-weight: 500;
    font-size: 0.85rem;
    cursor: pointer;
    transition: all 0.2s ease;
  }

  .tab-btn.active {
    background: #0ea5e9;
    color: #ffffff;
    box-shadow: 0 2px 8px rgba(14, 165, 233, 0.4);
  }

  .top-meta {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .ip-chip {
    display: flex;
    gap: 6px;
    align-items: center;
    background: rgba(255, 255, 255, 0.05);
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 0.8rem;
    border: 1px solid rgba(255, 255, 255, 0.08);
  }

  .chip-label {
    color: #64748b;
  }

  .chip-val {
    color: #38bdf8;
    font-family: monospace;
    font-weight: 600;
  }

  .session-badge {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.8rem;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 6px;
  }

  .session-badge.active {
    background: rgba(34, 197, 94, 0.15);
    color: #4ade80;
    border: 1px solid rgba(34, 197, 94, 0.3);
  }

  .session-badge.hosting {
    background: rgba(59, 130, 246, 0.15);
    color: #60a5fa;
    border: 1px solid rgba(59, 130, 246, 0.3);
  }

  .pulse-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
    animation: pulse 1.5s infinite;
  }

  @keyframes pulse {
    0% { transform: scale(0.95); opacity: 0.6; }
    50% { transform: scale(1.2); opacity: 1; }
    100% { transform: scale(0.95); opacity: 0.6; }
  }

  .btn-disconnect {
    background: #ef4444;
    color: #ffffff;
    border: none;
    padding: 5px 12px;
    font-size: 0.8rem;
    font-weight: 600;
    border-radius: 6px;
    cursor: pointer;
  }

  .main-content {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  /* Remote Viewport */
  .remote-container {
    position: relative;
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #000000;
    outline: none;
  }

  .remote-canvas {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    cursor: crosshair;
  }

  .floating-hud {
    position: absolute;
    top: 16px;
    right: 20px;
    background: rgba(15, 23, 42, 0.85);
    backdrop-filter: blur(10px);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 6px 12px;
    display: flex;
    align-items: center;
    gap: 12px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.5);
    z-index: 100;
  }

  .hud-item {
    font-size: 0.75rem;
    font-family: monospace;
    font-weight: 600;
    color: #94a3b8;
  }

  .hud-item.metric {
    color: #38bdf8;
  }

  .hud-btn {
    background: rgba(255, 255, 255, 0.08);
    border: none;
    color: #f1f5f9;
    padding: 4px 8px;
    border-radius: 4px;
    cursor: pointer;
    display: flex;
    align-items: center;
  }

  .hud-btn.danger {
    background: rgba(239, 68, 68, 0.2);
    color: #f87171;
  }

  /* Dashboard View */
  .dashboard-grid {
    flex: 1;
    display: grid;
    grid-template-columns: 1fr 340px;
    gap: 24px;
    padding: 32px 40px;
    max-width: 1200px;
    margin: 0 auto;
    align-items: start;
    overflow-y: auto;
  }

  .card {
    background: rgba(30, 41, 59, 0.4);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 14px;
    padding: 24px;
    backdrop-filter: blur(12px);
  }

  .card-header h2, .card-header h3 {
    margin: 0 0 6px 0;
    font-size: 1.25rem;
    font-weight: 700;
  }

  .card-header p {
    margin: 0;
    font-size: 0.85rem;
    color: #94a3b8;
  }

  .form-body {
    margin-top: 20px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .input-group label {
    display: block;
    font-size: 0.8rem;
    font-weight: 500;
    color: #cbd5e1;
    margin-bottom: 6px;
  }

  .input-wrapper input {
    width: 100%;
    padding: 10px 14px;
    border-radius: 8px;
    background: rgba(15, 23, 42, 0.6);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #ffffff;
    font-size: 0.95rem;
    font-family: monospace;
    outline: none;
    box-sizing: border-box;
    transition: border-color 0.2s;
  }

  .input-wrapper input:focus {
    border-color: #0ea5e9;
  }

  .btn-primary {
    background: linear-gradient(135deg, #0ea5e9, #2563eb);
    color: #ffffff;
    border: none;
    padding: 12px;
    border-radius: 8px;
    font-weight: 600;
    font-size: 0.95rem;
    cursor: pointer;
    box-shadow: 0 4px 14px rgba(14, 165, 233, 0.35);
    transition: opacity 0.2s;
  }

  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .connection-features {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 8px;
    padding-top: 14px;
    border-top: 1px solid rgba(255, 255, 255, 0.08);
  }

  .feature-item {
    font-size: 0.8rem;
    color: #94a3b8;
  }

  .feature-item .check {
    color: #38bdf8;
    font-weight: bold;
    margin-right: 6px;
  }

  /* Host details */
  .host-details {
    margin-top: 20px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .detail-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }

  .detail-box {
    background: rgba(15, 23, 42, 0.5);
    padding: 14px;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.06);
  }

  .detail-label {
    display: block;
    font-size: 0.75rem;
    color: #64748b;
    margin-bottom: 6px;
  }

  .detail-val-group {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .detail-val {
    font-size: 1.1rem;
    font-weight: 700;
    color: #38bdf8;
    font-family: monospace;
  }

  .btn-copy {
    background: rgba(255, 255, 255, 0.08);
    border: none;
    color: #94a3b8;
    padding: 3px 8px;
    font-size: 0.75rem;
    border-radius: 4px;
    cursor: pointer;
  }

  .btn-copy:hover {
    color: #ffffff;
    background: rgba(255, 255, 255, 0.15);
  }

  .port-info-box {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .port-tag {
    font-size: 0.75rem;
    font-family: monospace;
    background: rgba(255, 255, 255, 0.05);
    padding: 4px 10px;
    border-radius: 6px;
    color: #94a3b8;
    border: 1px solid rgba(255, 255, 255, 0.06);
  }

  .port-tag.tag-success {
    color: #4ade80;
    background: rgba(34, 197, 94, 0.1);
    border-color: rgba(34, 197, 94, 0.2);
  }

  .btn-toggle-host {
    width: 100%;
    padding: 12px;
    border-radius: 8px;
    font-weight: 600;
    border: none;
    cursor: pointer;
    font-size: 0.95rem;
    background: #0ea5e9;
    color: #ffffff;
    box-shadow: 0 4px 12px rgba(14, 165, 233, 0.3);
  }

  .btn-toggle-host.active {
    background: #ef4444;
    box-shadow: 0 4px 12px rgba(239, 68, 68, 0.3);
  }

  .status-banner {
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 0.85rem;
    background: rgba(14, 165, 233, 0.1);
    color: #38bdf8;
    border: 1px solid rgba(14, 165, 233, 0.2);
  }

  .error-banner {
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 0.85rem;
    background: rgba(239, 68, 68, 0.15);
    color: #f87171;
    border: 1px solid rgba(239, 68, 68, 0.25);
  }

  /* Specs list */
  .specs-list {
    margin-top: 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .spec-row {
    display: flex;
    justify-content: space-between;
    font-size: 0.8rem;
    padding-bottom: 8px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  }

  .spec-name {
    color: #64748b;
  }

  .spec-val {
    color: #cbd5e1;
    font-family: monospace;
    font-weight: 500;
  }

  .spec-val.highlight {
    color: #4ade80;
    font-weight: 700;
  }
</style>
