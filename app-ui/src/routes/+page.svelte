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

  // Reactive state using Svelte 5 runes
  let systemInfo = $state<SystemInfo | null>(null);
  let isHosting = $state(false);
  let hostStatusMessage = $state("Initializing background service...");
  let targetAddress = $state("");
  let isConnecting = $state(false);
  let isConnected = $state(false);
  let connectionError = $state("");
  let copied = $state(false);

  // Performance metrics
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

      // Automatically start hosting so the desk is immediately ready for incoming connections (like AnyDesk)
      if (!isHosting) {
        startHostingAuto();
      } else {
        hostStatusMessage = "Ready for incoming connections";
      }
    } catch (err) {
      console.error("Failed to fetch system info:", err);
      hostStatusMessage = "Ready for incoming connections";
    }
  }

  async function startHostingAuto() {
    try {
      const res: HostStatus = await invoke("start_hosting", { port: 44321 });
      if (res.success) {
        isHosting = true;
        hostStatusMessage = "Ready for incoming connections";
      }
    } catch (err: any) {
      isHosting = false;
      hostStatusMessage = `Hosting standby (${err})`;
    }
  }

  async function toggleHosting() {
    if (isHosting) {
      try {
        await invoke("stop_hosting");
        isHosting = false;
        hostStatusMessage = "Direct screen sharing paused";
      } catch (err: any) {
        hostStatusMessage = `Error stopping host: ${err}`;
      }
    } else {
      startHostingAuto();
    }
  }

  async function connectToRemote() {
    if (!targetAddress.trim() || isConnecting) return;
    isConnecting = true;
    connectionError = "";

    let ip = targetAddress.trim();
    let port = 44321;

    // Support ip:port notation
    if (ip.includes(":")) {
      const parts = ip.split(":");
      ip = parts[0];
      const parsedPort = parseInt(parts[1], 10);
      if (!isNaN(parsedPort)) {
        port = parsedPort;
      }
    }

    try {
      const res: ClientConnectResult = await invoke("connect_to_remote", {
        targetIp: ip,
        port: port,
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
      console.log("Direct frame pipe established on port", port);
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
    if (now - lastMoveTime < 8) return; // 120 Hz throttle
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

  function copyAddress() {
    if (!systemInfo?.peer_id) return;
    navigator.clipboard.writeText(systemInfo.peer_id);
    copied = true;
    setTimeout(() => {
      copied = false;
    }, 2000);
  }

  function handleKeypressConnect(e: KeyboardEvent) {
    if (e.key === "Enter") {
      connectToRemote();
    }
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

<div class="window-shell">
  <!-- Top App Bar -->
  <header class="header">
    <div class="header-brand">
      <div class="brand-badge">
        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2.2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
      </div>
      <div class="brand-info">
        <h1 class="app-title">OpenRemote</h1>
        <span class="app-subtitle">Direct LAN & Internet P2P</span>
      </div>
    </div>

    <div class="header-status">
      {#if systemInfo}
        <div class="network-badge">
          <span class="network-label">LAN</span>
          <span class="network-ip">{systemInfo.lan_ip}</span>
        </div>
      {/if}
      {#if isConnected}
        <div class="live-tag">
          <span class="live-dot"></span>
          <span>In Session ({fps} FPS • {rttMs}ms)</span>
        </div>
        <button class="btn-disconnect-header" onclick={disconnectRemote}>
          Disconnect
        </button>
      {/if}
    </div>
  </header>

  <!-- Main Content Body -->
  <main class="content-body">
    {#if isConnected}
      <!-- Remote Canvas Viewport -->
      <div
        class="remote-viewport"
        bind:this={canvasContainerRef}
        role="region"
        aria-label="Remote Session"
      >
        <canvas
          bind:this={canvasRef}
          class="canvas-element"
          onmousemove={handleMouseMove}
          onmousedown={handleMouseDown}
          onmouseup={handleMouseUp}
          onwheel={handleWheel}
          oncontextmenu={(e) => e.preventDefault()}
        ></canvas>

        <!-- Floating Quick HUD -->
        <div class="session-hud">
          <div class="hud-tag res">{remoteResolution.width}x{remoteResolution.height}</div>
          <div class="hud-tag">{fps} FPS</div>
          <div class="hud-tag">{rttMs} ms</div>
          <button class="hud-action" onclick={toggleFullscreen} title="Fullscreen">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"></path>
            </svg>
          </button>
          <button class="hud-action danger" onclick={disconnectRemote} title="End Session">
            ✕
          </button>
        </div>
      </div>
    {:else}
      <!-- Unified AnyDesk-Style Dashboard -->
      <div class="dashboard-container">
        <!-- Dual Column Unified Desk Cards -->
        <div class="desks-row">
          <!-- Left Column: This Desk (Host / Share) -->
          <div class="desk-card this-desk">
            <div class="card-caption">
              <span class="section-label">THIS DESK</span>
              <h2 class="card-heading">Your Address</h2>
            </div>

            <div class="address-display">
              <div class="address-digits">
                {#if systemInfo}
                  <span class="digits-text">{systemInfo.peer_id}</span>
                {:else}
                  <span class="digits-placeholder">Connecting...</span>
                {/if}
              </div>
              <button
                class="btn-copy-address"
                onclick={copyAddress}
                title="Copy Address to Clipboard"
              >
                {#if copied}
                  <span class="copy-success">✓ Copied!</span>
                {:else}
                  <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                    <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
                    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
                  </svg>
                  <span>Copy ID</span>
                {/if}
              </button>
            </div>

            <!-- Listening Status Dot -->
            <div class="status-indicator-row">
              <span class="status-dot" class:active={isHosting}></span>
              <span class="status-text">{hostStatusMessage}</span>
            </div>

            <div class="desk-footer">
              <div class="meta-item">
                <span class="meta-title">Direct LAN Address:</span>
                <span class="meta-val">{systemInfo?.lan_ip || "127.0.0.1"}:44321</span>
              </div>
              <button
                class="btn-toggle-service"
                class:active={isHosting}
                onclick={toggleHosting}
              >
                {isHosting ? "Sharing Active" : "Start Sharing"}
              </button>
            </div>
          </div>

          <!-- Right Column: Remote Desk (Connect) -->
          <div class="desk-card remote-desk">
            <div class="card-caption">
              <span class="section-label">REMOTE DESK</span>
              <h2 class="card-heading">Connect to Partner</h2>
            </div>

            <div class="connect-form">
              <div class="input-and-button">
                <input
                  type="text"
                  class="remote-input"
                  placeholder="Enter Remote Address or IP (e.g. 192.168.1.50)"
                  bind:value={targetAddress}
                  onkeypress={handleKeypressConnect}
                />
                <button
                  class="btn-connect-primary"
                  disabled={isConnecting || !targetAddress.trim()}
                  onclick={connectToRemote}
                >
                  {#if isConnecting}
                    <span class="btn-spinner"></span>
                    <span>Connecting...</span>
                  {:else}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.5">
                      <line x1="5" y1="12" x2="19" y2="12"></line>
                      <polyline points="12 5 19 12 12 19"></polyline>
                    </svg>
                    <span>Connect</span>
                  {/if}
                </button>
              </div>

              {#if connectionError}
                <div class="inline-error">
                  <span>⚠</span> {connectionError}
                </div>
              {/if}

              <!-- Quick Localhost Loopback Shortcut -->
              <div class="quick-targets">
                <span class="quick-label">Quick Loopback:</span>
                <button
                  class="quick-pill"
                  onclick={() => { targetAddress = "127.0.0.1:44321"; connectToRemote(); }}
                >
                  127.0.0.1:44321
                </button>
              </div>
            </div>

            <div class="desk-footer security-note">
              <div class="feature-tag">
                <span class="feature-icon">⚡</span> Sub-10ms Input Response
              </div>
              <div class="feature-tag">
                <span class="feature-icon">🛡</span> Direct P2P Encryption
              </div>
            </div>
          </div>
        </div>

        <!-- Bottom Capabilities Bar -->
        <div class="engine-bar">
          <div class="engine-item">
            <span class="engine-bullet">●</span>
            <span class="engine-name">GPU Pipeline:</span>
            <span class="engine-tech">Windows.Graphics.Capture</span>
          </div>
          <div class="engine-item">
            <span class="engine-bullet">●</span>
            <span class="engine-name">Codec:</span>
            <span class="engine-tech">SIMD LZ4-Flex (&lt;11ms)</span>
          </div>
          <div class="engine-item">
            <span class="engine-bullet">●</span>
            <span class="engine-name">Input:</span>
            <span class="engine-tech">Native SendInput</span>
          </div>
          <div class="engine-item">
            <span class="engine-bullet">●</span>
            <span class="engine-name">Channels:</span>
            <span class="engine-tech">UDP (Control) + TCP (Frames)</span>
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
    background-color: #080d16;
    color: #e2e8f0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
    user-select: none;
    height: 100vh;
    overflow: hidden;
  }

  .window-shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: radial-gradient(circle at 50% 0%, #111a2e 0%, #080d16 85%);
  }

  /* Top Bar */
  .header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 14px 28px;
    background: rgba(15, 23, 42, 0.7);
    backdrop-filter: blur(14px);
    border-bottom: 1px solid rgba(255, 255, 255, 0.07);
    z-index: 50;
  }

  .header-brand {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .brand-badge {
    width: 36px;
    height: 36px;
    border-radius: 9px;
    background: linear-gradient(135deg, #0284c7, #2563eb);
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ffffff;
    box-shadow: 0 4px 14px rgba(2, 132, 199, 0.35);
  }

  .brand-info {
    display: flex;
    flex-direction: column;
  }

  .app-title {
    margin: 0;
    font-size: 1.15rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: #ffffff;
    line-height: 1.2;
  }

  .app-subtitle {
    font-size: 0.72rem;
    color: #64748b;
    font-weight: 500;
  }

  .header-status {
    display: flex;
    align-items: center;
    gap: 14px;
  }

  .network-badge {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 0.8rem;
    font-family: monospace;
  }

  .network-label {
    color: #64748b;
    font-weight: 600;
  }

  .network-ip {
    color: #38bdf8;
    font-weight: 600;
  }

  .live-tag {
    display: flex;
    align-items: center;
    gap: 7px;
    background: rgba(34, 197, 94, 0.12);
    border: 1px solid rgba(34, 197, 94, 0.3);
    color: #4ade80;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 0.8rem;
    font-weight: 600;
  }

  .live-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #22c55e;
    animation: live-pulse 1.4s infinite;
  }

  @keyframes live-pulse {
    0% { transform: scale(0.9); opacity: 0.6; }
    50% { transform: scale(1.3); opacity: 1; }
    100% { transform: scale(0.9); opacity: 0.6; }
  }

  .btn-disconnect-header {
    background: #ef4444;
    color: #ffffff;
    border: none;
    padding: 6px 14px;
    font-size: 0.8rem;
    font-weight: 600;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.2s;
  }

  .btn-disconnect-header:hover {
    background: #dc2626;
  }

  /* Content Body */
  .content-body {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  /* Unified AnyDesk Dashboard */
  .dashboard-container {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: center;
    max-width: 1040px;
    margin: 0 auto;
    padding: 24px 32px;
    gap: 24px;
  }

  .desks-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 24px;
  }

  .desk-card {
    background: rgba(18, 26, 43, 0.65);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 14px;
    padding: 26px 28px;
    backdrop-filter: blur(16px);
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    min-height: 240px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
    transition: border-color 0.2s;
  }

  .desk-card:hover {
    border-color: rgba(255, 255, 255, 0.14);
  }

  .card-caption {
    margin-bottom: 18px;
  }

  .section-label {
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: #0284c7;
    text-transform: uppercase;
  }

  .card-heading {
    margin: 4px 0 0 0;
    font-size: 1.35rem;
    font-weight: 700;
    color: #ffffff;
  }

  /* Address Display (This Desk) */
  .address-display {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: rgba(8, 13, 22, 0.75);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 10px;
    padding: 10px 16px;
    margin-bottom: 14px;
  }

  .address-digits {
    font-family: "SF Mono", Monaco, "Cascadia Code", Consolas, monospace;
    font-size: 1.55rem;
    font-weight: 700;
    letter-spacing: 0.05em;
    color: #38bdf8;
  }

  .digits-placeholder {
    color: #64748b;
    font-size: 1.1rem;
  }

  .btn-copy-address {
    display: flex;
    align-items: center;
    gap: 6px;
    background: rgba(56, 189, 248, 0.12);
    border: 1px solid rgba(56, 189, 248, 0.25);
    color: #38bdf8;
    padding: 7px 14px;
    border-radius: 7px;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.2s ease;
  }

  .btn-copy-address:hover {
    background: rgba(56, 189, 248, 0.2);
    border-color: rgba(56, 189, 248, 0.4);
    color: #ffffff;
  }

  .copy-success {
    color: #4ade80;
    font-weight: 700;
  }

  /* Listening Status Indicator */
  .status-indicator-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.82rem;
    color: #94a3b8;
    margin-bottom: 18px;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #64748b;
  }

  .status-dot.active {
    background: #22c55e;
    box-shadow: 0 0 8px #22c55e;
  }

  .desk-footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding-top: 14px;
    border-top: 1px solid rgba(255, 255, 255, 0.06);
  }

  .meta-item {
    font-size: 0.78rem;
    color: #64748b;
  }

  .meta-val {
    font-family: monospace;
    color: #94a3b8;
    font-weight: 600;
  }

  .btn-toggle-service {
    background: rgba(34, 197, 94, 0.12);
    border: 1px solid rgba(34, 197, 94, 0.25);
    color: #4ade80;
    padding: 6px 14px;
    border-radius: 6px;
    font-size: 0.78rem;
    font-weight: 600;
    cursor: pointer;
  }

  /* Remote Desk (Connect Input) */
  .connect-form {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .input-and-button {
    display: flex;
    gap: 8px;
  }

  .remote-input {
    flex: 1;
    background: rgba(8, 13, 22, 0.75);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 10px;
    padding: 12px 16px;
    color: #ffffff;
    font-size: 0.95rem;
    font-family: monospace;
    outline: none;
    transition: border-color 0.2s;
  }

  .remote-input:focus {
    border-color: #0284c7;
    box-shadow: 0 0 0 2px rgba(2, 132, 199, 0.2);
  }

  .btn-connect-primary {
    display: flex;
    align-items: center;
    gap: 8px;
    background: linear-gradient(135deg, #0284c7, #2563eb);
    color: #ffffff;
    border: none;
    padding: 12px 22px;
    border-radius: 10px;
    font-size: 0.95rem;
    font-weight: 600;
    cursor: pointer;
    box-shadow: 0 4px 14px rgba(2, 132, 199, 0.35);
    transition: opacity 0.2s;
  }

  .btn-connect-primary:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .btn-spinner {
    width: 14px;
    height: 14px;
    border: 2px solid rgba(255, 255, 255, 0.3);
    border-top-color: #ffffff;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .inline-error {
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.25);
    color: #f87171;
    padding: 8px 12px;
    border-radius: 7px;
    font-size: 0.82rem;
  }

  .quick-targets {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.78rem;
  }

  .quick-label {
    color: #64748b;
  }

  .quick-pill {
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: #94a3b8;
    padding: 3px 9px;
    border-radius: 5px;
    font-family: monospace;
    font-size: 0.75rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .quick-pill:hover {
    color: #38bdf8;
    border-color: rgba(56, 189, 248, 0.3);
    background: rgba(56, 189, 248, 0.1);
  }

  .security-note {
    display: flex;
    gap: 16px;
  }

  .feature-tag {
    font-size: 0.76rem;
    color: #94a3b8;
    display: flex;
    align-items: center;
    gap: 5px;
  }

  /* Bottom Engine Bar */
  .engine-bar {
    display: flex;
    justify-content: space-around;
    background: rgba(15, 23, 42, 0.45);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 10px;
    padding: 12px 18px;
  }

  .engine-item {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.78rem;
  }

  .engine-bullet {
    color: #0284c7;
    font-size: 0.6rem;
  }

  .engine-name {
    color: #64748b;
    font-weight: 500;
  }

  .engine-tech {
    color: #cbd5e1;
    font-family: monospace;
    font-weight: 600;
  }

  /* Remote Viewport */
  .remote-viewport {
    position: relative;
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #000000;
    outline: none;
  }

  .canvas-element {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    cursor: crosshair;
  }

  .session-hud {
    position: absolute;
    top: 14px;
    right: 20px;
    background: rgba(15, 23, 42, 0.85);
    backdrop-filter: blur(12px);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 6px 12px;
    display: flex;
    align-items: center;
    gap: 10px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.5);
    z-index: 100;
  }

  .hud-tag {
    font-size: 0.74rem;
    font-family: monospace;
    font-weight: 600;
    color: #38bdf8;
  }

  .hud-tag.res {
    color: #94a3b8;
  }

  .hud-action {
    background: rgba(255, 255, 255, 0.08);
    border: none;
    color: #f1f5f9;
    padding: 4px 8px;
    border-radius: 4px;
    cursor: pointer;
    display: flex;
    align-items: center;
  }

  .hud-action.danger {
    background: rgba(239, 68, 68, 0.2);
    color: #f87171;
  }
</style>
