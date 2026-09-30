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
    peer_id?: string;
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

  interface MonitorDescriptor {
    index: number;
    name: string;
    width: number;
    height: number;
    is_primary: boolean;
  }

  interface RecentSession {
    peer_id: string;
    alias?: string;
    last_connected_at: number;
  }

  interface RemoteDisplay {
    id: number;
    name: string;
    resolution: string;
    width: number;
    height: number;
    is_primary: boolean;
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

  // Multi-Monitor state (Host local vs Remote)
  let availableMonitors = $state<MonitorDescriptor[]>([]);
  let activeMonitorIndex = $state(1);
  let remoteDisplays = $state<RemoteDisplay[]>([]);
  let activeRemoteDisplayId = $state(1);

  // Recent Sessions History
  let recentSessions = $state<RecentSession[]>([]);

  // Performance metrics
  let fps = $state(0);
  let rttMs = $state(0);
  let frameCount = $state(0);
  let remoteResolution = $state({ width: 0, height: 0 });
  let isFullscreen = $state(false);

  // High Performance 60 FPS Canvas References
  let canvasRef = $state<HTMLCanvasElement | null>(null);
  let canvasContainerRef = $state<HTMLDivElement | null>(null);
  let backCanvas: HTMLCanvasElement | null = null;
  let animFrameId: number | null = null;
  let dirtyFramePending = false;
  let ws: WebSocket | null = null;
  let lastMoveTime = 0;

  // Touch & Mobile Gestures State
  let zoomScale = $state(1.0);
  let panX = $state(0);
  let panY = $state(0);
  let touchStartTime = 0;
  let touchStartPos = { x: 0, y: 0 };
  let initialPinchDist = 0;
  let initialPinchScale = 1.0;
  let initialPan = { x: 0, y: 0 };
  let holdTimer: any = null;
  let isHoldTriggered = false;
  let isPinchingOrPanning = false;
  let virtualKeyboardInputRef = $state<HTMLInputElement | null>(null);

  async function loadSystemInfo() {
    try {
      const info: SystemInfo = await invoke("get_system_info");
      systemInfo = info;
      isHosting = info.is_hosting;
      isConnected = info.is_connected;

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

  async function loadMonitors() {
    try {
      const list: MonitorDescriptor[] = await invoke("get_available_monitors");
      availableMonitors = list;
    } catch (err) {
      console.error("Failed to load monitors:", err);
    }
  }

  async function loadRecentSessions() {
    try {
      const recents: RecentSession[] = await invoke("get_recent_sessions");
      recentSessions = recents;
    } catch (err) {
      console.error("Failed to load recent sessions:", err);
    }
  }

  async function removeRecentSession(peerId: string) {
    try {
      await invoke("remove_recent_session", { peerId });
      recentSessions = recentSessions.filter((s) => s.peer_id !== peerId);
    } catch (err) {
      console.error("Failed to remove recent session:", err);
    }
  }

  function connectRecent(peerId: string) {
    targetAddress = peerId;
    connectToRemote();
  }

  function formatTimeAgo(timestamp: number): string {
    const elapsed = Math.floor(Date.now() / 1000 - timestamp);
    if (elapsed < 60) return "Just now";
    if (elapsed < 3600) return `${Math.floor(elapsed / 60)}m ago`;
    if (elapsed < 86400) return `${Math.floor(elapsed / 3600)}h ago`;
    return `${Math.floor(elapsed / 86400)}d ago`;
  }

  async function switchMonitor(index: number) {
    try {
      await invoke("switch_monitor", { monitorIndex: index });
      activeMonitorIndex = index;
    } catch (err) {
      console.error("Failed to switch monitor:", err);
    }
  }

  async function switchRemoteDisplay(index: number) {
    try {
      await invoke("switch_remote_monitor", { monitorIndex: index });
      activeRemoteDisplayId = index;
    } catch (err) {
      console.error("Failed to switch remote display:", err);
    }
  }

  async function startHostingAuto() {
    try {
      const res: HostStatus = await invoke("start_hosting", { port: 44321 });
      if (res.success) {
        isHosting = true;
        if (res.peer_id && systemInfo) {
          systemInfo.peer_id = res.peer_id;
        }
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

    try {
      const res: ClientConnectResult = await invoke("connect_to_remote", {
        targetAddress: targetAddress.trim(),
        port: 44321,
      });

      if (res.success) {
        isConnected = true;
        isConnecting = false;
        loadRecentSessions();
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
    remoteDisplays = [];
    activeRemoteDisplayId = 1;
    dirtyFramePending = false;
    if (canvasRef) {
      const ctx = canvasRef.getContext("2d");
      if (ctx) ctx.clearRect(0, 0, canvasRef.width, canvasRef.height);
    }
    backCanvas = null;
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

    socket.onmessage = (event: MessageEvent) => {
      // 1. Dynamic Display Manifest from Remote Host
      if (typeof event.data === "string") {
        try {
          const msg = JSON.parse(event.data);
          if (msg.type === "display_manifest" && Array.isArray(msg.displays)) {
            remoteDisplays = msg.displays;
            activeRemoteDisplayId = msg.active_display_id || 1;
          }
        } catch (e) {
          console.error("Display manifest parse error:", e);
        }
        return;
      }

      if (!(event.data instanceof ArrayBuffer)) return;
      frameCount++;

      const buffer = event.data;
      if (buffer.byteLength < 36) return;

      const view = new DataView(buffer);
      const width = view.getUint32(0);
      const height = view.getUint32(4);
      const timestampMs = Number(view.getBigUint64(8));
      const dirtyX = view.getUint32(16);
      const dirtyY = view.getUint32(20);
      const dirtyW = view.getUint32(24);
      const dirtyH = view.getUint32(28);

      const now = Date.now();
      if (timestampMs > 0 && now >= timestampMs) {
        rttMs = now - timestampMs;
      }

      remoteResolution = { width, height };

      if (dirtyW === 0 || dirtyH === 0) return;

      // Offscreen canvas back-buffer to assemble dirty rects before VSync render
      if (!backCanvas) {
        backCanvas = document.createElement("canvas");
      }
      if (backCanvas.width !== width || backCanvas.height !== height) {
        backCanvas.width = width;
        backCanvas.height = height;
      }

      const backCtx = backCanvas.getContext("2d", { alpha: false });
      if (!backCtx) return;

      const pixelBytes = new Uint8ClampedArray(buffer, 36);
      if (pixelBytes.length !== dirtyW * dirtyH * 4) {
        return;
      }

      try {
        const dirtyImageData = new ImageData(pixelBytes, dirtyW, dirtyH);
        backCtx.putImageData(dirtyImageData, dirtyX, dirtyY);
        dirtyFramePending = true;
      } catch (e) {
        console.error("Frame dirty rect blit error:", e);
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
    const rawX = (e.clientX - rect.left - panX) / zoomScale;
    const rawY = (e.clientY - rect.top - panY) / zoomScale;
    const x = Math.max(0, Math.min(1, rawX / rect.width));
    const y = Math.max(0, Math.min(1, rawY / rect.height));
    return { x, y };
  }

  function getTouchNormalizedCoordinates(touch: Touch): { x: number; y: number } {
    if (!canvasRef) return { x: 0, y: 0 };
    const rect = canvasRef.getBoundingClientRect();
    const rawX = (touch.clientX - rect.left - panX) / zoomScale;
    const rawY = (touch.clientY - rect.top - panY) / zoomScale;
    const x = Math.max(0, Math.min(1, rawX / rect.width));
    const y = Math.max(0, Math.min(1, rawY / rect.height));
    return { x, y };
  }

  function handleTouchStart(e: TouchEvent) {
    if (!isConnected) return;
    e.preventDefault();

    if (e.touches.length === 1) {
      const t = e.touches[0];
      touchStartTime = performance.now();
      touchStartPos = { x: t.clientX, y: t.clientY };
      isHoldTriggered = false;
      isPinchingOrPanning = false;

      // Long press / hold (500ms) -> Right Click
      if (holdTimer) clearTimeout(holdTimer);
      holdTimer = setTimeout(async () => {
        isHoldTriggered = true;
        const { x, y } = getTouchNormalizedCoordinates(t);
        await invoke("send_input", {
          event: {
            type: "MouseDown",
            data: { button: "Right", x, y },
          },
        });
        setTimeout(async () => {
          await invoke("send_input", {
            event: {
              type: "MouseUp",
              data: { button: "Right", x, y },
            },
          });
        }, 30);
        if (typeof navigator !== "undefined" && navigator.vibrate) {
          navigator.vibrate(50);
        }
      }, 500);
    } else if (e.touches.length === 2) {
      if (holdTimer) clearTimeout(holdTimer);
      isPinchingOrPanning = true;
      initialPinchDist = Math.hypot(
        e.touches[0].clientX - e.touches[1].clientX,
        e.touches[0].clientY - e.touches[1].clientY
      );
      initialPinchScale = zoomScale;
      initialPan = { x: panX, y: panY };
      touchStartPos = {
        x: (e.touches[0].clientX + e.touches[1].clientX) / 2,
        y: (e.touches[0].clientY + e.touches[1].clientY) / 2,
      };
    }
  }

  async function handleTouchMove(e: TouchEvent) {
    if (!isConnected) return;
    e.preventDefault();

    if (e.touches.length === 1) {
      const t = e.touches[0];
      const dist = Math.hypot(t.clientX - touchStartPos.x, t.clientY - touchStartPos.y);
      if (dist > 10 && holdTimer) {
        clearTimeout(holdTimer);
      }

      if (zoomScale > 1.05) {
        // Pan the viewport when zoomed in
        panX = initialPan.x + (t.clientX - touchStartPos.x);
        panY = initialPan.y + (t.clientY - touchStartPos.y);
      } else {
        // Follow touch as mouse cursor
        const now = performance.now();
        if (now - lastMoveTime > 16) {
          lastMoveTime = now;
          const { x, y } = getTouchNormalizedCoordinates(t);
          await invoke("send_input", {
            event: {
              type: "MouseMove",
              data: { x, y },
            },
          });
        }
      }
    } else if (e.touches.length === 2) {
      if (holdTimer) clearTimeout(holdTimer);
      const currentDist = Math.hypot(
        e.touches[0].clientX - e.touches[1].clientX,
        e.touches[0].clientY - e.touches[1].clientY
      );
      if (initialPinchDist > 0) {
        zoomScale = Math.max(1.0, Math.min(4.0, initialPinchScale * (currentDist / initialPinchDist)));
      }
      const midX = (e.touches[0].clientX + e.touches[1].clientX) / 2;
      const midY = (e.touches[0].clientY + e.touches[1].clientY) / 2;
      panX = initialPan.x + (midX - touchStartPos.x);
      panY = initialPan.y + (midY - touchStartPos.y);

      if (zoomScale <= 1.02) {
        zoomScale = 1.0;
        panX = 0;
        panY = 0;
      }
    }
  }

  async function handleTouchEnd(e: TouchEvent) {
    if (!isConnected) return;
    e.preventDefault();
    if (holdTimer) clearTimeout(holdTimer);

    if (isHoldTriggered) {
      isHoldTriggered = false;
      return;
    }

    if (isPinchingOrPanning) {
      if (e.touches.length === 0) {
        isPinchingOrPanning = false;
        initialPan = { x: panX, y: panY };
        if (zoomScale <= 1.02) {
          zoomScale = 1.0;
          panX = 0;
          panY = 0;
        }
      }
      return;
    }

    // Single finger tap -> Left Click
    if (e.changedTouches.length === 1 && e.touches.length === 0) {
      const t = e.changedTouches[0];
      const duration = performance.now() - touchStartTime;
      const moveDist = Math.hypot(t.clientX - touchStartPos.x, t.clientY - touchStartPos.y);

      if (duration < 350 && moveDist < 15) {
        const { x, y } = getTouchNormalizedCoordinates(t);
        await invoke("send_input", {
          event: {
            type: "MouseMove",
            data: { x, y },
          },
        });
        await invoke("send_input", {
          event: {
            type: "MouseDown",
            data: { button: "Left", x, y },
          },
        });
        setTimeout(async () => {
          await invoke("send_input", {
            event: {
              type: "MouseUp",
              data: { button: "Left", x, y },
            },
          });
        }, 25);
      }
    } else if (e.changedTouches.length === 2 && e.touches.length === 0) {
      // Two-finger tap -> Right Click
      const duration = performance.now() - touchStartTime;
      if (duration < 350) {
        const t = e.changedTouches[0];
        const { x, y } = getTouchNormalizedCoordinates(t);
        await invoke("send_input", {
          event: {
            type: "MouseDown",
            data: { button: "Right", x, y },
          },
        });
        setTimeout(async () => {
          await invoke("send_input", {
            event: {
              type: "MouseUp",
              data: { button: "Right", x, y },
            },
          });
        }, 25);
      }
    }
  }

  function handleTouchCancel() {
    if (holdTimer) clearTimeout(holdTimer);
    isHoldTriggered = false;
    isPinchingOrPanning = false;
  }

  function resetZoom() {
    zoomScale = 1.0;
    panX = 0;
    panY = 0;
  }

  function toggleVirtualKeyboard() {
    if (!virtualKeyboardInputRef) return;
    virtualKeyboardInputRef.focus();
    virtualKeyboardInputRef.click();
  }

  async function handleVirtualKeyDown(e: KeyboardEvent) {
    if (!isConnected) return;
    if (e.key === "Backspace") {
      await invoke("send_input", {
        event: { type: "KeyDown", data: { scancode: 8, key: "Backspace" } }
      });
      setTimeout(() => invoke("send_input", {
        event: { type: "KeyUp", data: { scancode: 8, key: "Backspace" } }
      }), 25);
    } else if (e.key === "Enter") {
      await invoke("send_input", {
        event: { type: "KeyDown", data: { scancode: 13, key: "Enter" } }
      });
      setTimeout(() => invoke("send_input", {
        event: { type: "KeyUp", data: { scancode: 13, key: "Enter" } }
      }), 25);
    }
  }

  async function handleVirtualInput(e: Event) {
    if (!isConnected) return;
    const input = e.target as HTMLInputElement;
    const val = input.value;
    if (!val) return;
    for (const ch of val) {
      await invoke("send_input", {
        event: { type: "KeyDown", data: { scancode: ch.charCodeAt(0), key: ch } }
      });
      setTimeout(() => invoke("send_input", {
        event: { type: "KeyUp", data: { scancode: ch.charCodeAt(0), key: ch } }
      }), 25);
    }
    input.value = "";
  }

  async function handleMouseMove(e: MouseEvent) {
    if (!isConnected) return;
    const now = performance.now();
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
    const tag = (e.target as HTMLElement)?.tagName?.toLowerCase();
    if (tag === "input" || tag === "textarea") return;

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
    const tag = (e.target as HTMLElement)?.tagName?.toLowerCase();
    if (tag === "input" || tag === "textarea") return;

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

  // Window Controls
  async function minimizeWindow() {
    await invoke("app_minimize");
  }

  async function toggleMaximizeWindow() {
    await invoke("app_toggle_maximize");
  }

  async function closeWindow() {
    await invoke("app_close");
  }

  async function handleTitlebarMouseDown(e: MouseEvent) {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target?.closest('[data-tauri-drag-region="false"], button, input, a, .win-btn, .network-pill, .btn-disconnect-titlebar')) {
      return;
    }
    try {
      await invoke("app_start_dragging");
    } catch (err) {
      console.error("Window drag error:", err);
    }
  }

  async function handleTitlebarDblClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target?.closest('[data-tauri-drag-region="false"], button, input, a, .win-btn, .network-pill, .btn-disconnect-titlebar')) {
      return;
    }
    await toggleMaximizeWindow();
  }

  function renderLoop() {
    if (isConnected && dirtyFramePending && canvasRef && backCanvas) {
      if (canvasRef.width !== backCanvas.width || canvasRef.height !== backCanvas.height) {
        canvasRef.width = backCanvas.width;
        canvasRef.height = backCanvas.height;
      }
      const ctx = canvasRef.getContext("2d", { alpha: false });
      if (ctx) {
        ctx.drawImage(backCanvas, 0, 0);
        dirtyFramePending = false;
      }
    }
    animFrameId = requestAnimationFrame(renderLoop);
  }

  onMount(() => {
    loadSystemInfo();
    loadMonitors();
    loadRecentSessions();

    animFrameId = requestAnimationFrame(renderLoop);

    fpsInterval = window.setInterval(() => {
      fps = frameCount;
      frameCount = 0;
    }, 1000);
  });

  onDestroy(() => {
    if (animFrameId) cancelAnimationFrame(animFrameId);
    if (fpsInterval) clearInterval(fpsInterval);
    if (ws) ws.close();
  });
</script>

<svelte:window onkeydown={handleKeyDown} onkeyup={handleKeyUp} />

<div class="window-shell">
  <!-- Custom Multi-DPI Native Drag Titlebar -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <header
    class="custom-titlebar"
    role="region"
    aria-label="Window Titlebar"
    data-tauri-drag-region
    onmousedown={handleTitlebarMouseDown}
    ondblclick={handleTitlebarDblClick}
  >
    <div class="titlebar-left" data-tauri-drag-region>
      <div class="brand-badge" data-tauri-drag-region="false">
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
      </div>
      <span class="titlebar-text" data-tauri-drag-region>OpenRemote</span>

      {#if systemInfo}
        <div class="network-pill" data-tauri-drag-region="false" title="Local IP">
          <span class="pill-dot"></span>
          <span>{systemInfo.lan_ip}</span>
        </div>
      {/if}
    </div>

    <div class="titlebar-center" data-tauri-drag-region>
      {#if isConnected}
        <div class="session-status-chip" data-tauri-drag-region="false">
          <span class="status-live-dot"></span>
          <span>Live ({fps} FPS • {rttMs}ms)</span>
        </div>
      {/if}
    </div>

    <div class="titlebar-right" data-tauri-drag-region="false">
      {#if isConnected}
        <button class="btn-disconnect-titlebar" data-tauri-drag-region="false" onclick={disconnectRemote}>
          Disconnect
        </button>
      {/if}

      <!-- Window Action Buttons (Minimize, Maximize, Close) -->
      <div class="window-actions" data-tauri-drag-region="false">
        <button class="win-btn win-min" data-tauri-drag-region="false" onclick={minimizeWindow} title="Minimize">
          <svg width="10" height="2" viewBox="0 0 10 2" fill="currentColor">
            <rect width="10" height="2" rx="1"/>
          </svg>
        </button>
        <button class="win-btn win-max" data-tauri-drag-region="false" onclick={toggleMaximizeWindow} title="Maximize">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.5">
            <rect x="1" y="1" width="8" height="8" rx="1"/>
          </svg>
        </button>
        <button class="win-btn win-close" data-tauri-drag-region="false" onclick={closeWindow} title="Close">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.6">
            <path d="M1 1L9 9M9 1L1 9"/>
          </svg>
        </button>
      </div>
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
        <!-- Mobile Hidden Virtual Keyboard Input -->
        <input
          bind:this={virtualKeyboardInputRef}
          type="text"
          class="mobile-virtual-keyboard-input"
          autocapitalize="off"
          autocorrect="off"
          spellcheck="false"
          oninput={handleVirtualInput}
          onkeydown={handleVirtualKeyDown}
        />

        <canvas
          bind:this={canvasRef}
          class="canvas-element"
          style="transform: translate({panX}px, {panY}px) scale({zoomScale}); transform-origin: center center;"
          onmousemove={handleMouseMove}
          onmousedown={handleMouseDown}
          onmouseup={handleMouseUp}
          onwheel={handleWheel}
          ontouchstart={handleTouchStart}
          ontouchmove={handleTouchMove}
          ontouchend={handleTouchEnd}
          ontouchcancel={handleTouchCancel}
          oncontextmenu={(e) => e.preventDefault()}
        ></canvas>

        <!-- Floating Quick HUD with Multi-Monitor Switcher & Mobile Tools -->
        <div class="session-hud">
          <!-- Multi-Monitor Switcher Buttons (Dynamic Remote Displays) -->
          {#if remoteDisplays.length > 1}
            <div class="monitor-switcher">
              <span class="switcher-title">Display:</span>
              {#each remoteDisplays as disp}
                <button
                  class="btn-mon-pill"
                  class:active={activeRemoteDisplayId === disp.id}
                  onclick={() => switchRemoteDisplay(disp.id)}
                  title={`${disp.name} (${disp.width}x${disp.height})`}
                >
                  🖥 Display {disp.id}
                </button>
              {/each}
            </div>
          {:else if remoteDisplays.length === 1}
            <div class="monitor-badge">
              <span class="badge-icon">🖥</span> {remoteDisplays[0].name || `Display ${remoteDisplays[0].id}`}
            </div>
          {/if}

          <!-- Zoom Reset Button when pinched/zoomed -->
          {#if zoomScale > 1.05}
            <button class="hud-action zoom-reset-btn" onclick={resetZoom} title="Reset Zoom">
              🔍 {Math.round(zoomScale * 100)}%
            </button>
          {/if}

          <!-- On-Screen Virtual Keyboard Toggle -->
          <button class="hud-action keyboard-btn" onclick={toggleVirtualKeyboard} title="Toggle Keyboard">
            ⌨
          </button>

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
                  placeholder="Enter 9-Digit Peer ID or IP (e.g. 901-435-944 or 192.168.1.50)"
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

        <!-- Recent Sessions / History Section -->
        {#if recentSessions.length > 0}
          <div class="recent-sessions-card">
            <div class="recent-card-header">
              <div class="recent-title-group">
                <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2">
                  <circle cx="12" cy="12" r="10"></circle>
                  <polyline points="12 6 12 12 16 14"></polyline>
                </svg>
                <span class="recent-heading">RECENT SESSIONS</span>
                <span class="recent-badge">{recentSessions.length}</span>
              </div>
            </div>

            <div class="recent-chips-container">
              {#each recentSessions as session}
                <div class="recent-chip">
                  <div class="recent-chip-left" onclick={() => connectRecent(session.peer_id)} role="button" tabindex="0" onkeypress={(e) => e.key === 'Enter' && connectRecent(session.peer_id)}>
                    <div class="recent-icon-box">
                      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">
                        <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
                        <line x1="8" y1="21" x2="16" y2="21"></line>
                        <line x1="12" y1="17" x2="12" y2="21"></line>
                      </svg>
                    </div>
                    <div class="recent-text-group">
                      <span class="recent-chip-id">{session.alias || session.peer_id}</span>
                      <span class="recent-chip-time">{formatTimeAgo(session.last_connected_at)}</span>
                    </div>
                  </div>

                  <div class="recent-chip-right">
                    <button class="btn-chip-connect" onclick={() => connectRecent(session.peer_id)} title="Connect">
                      Connect
                    </button>
                    <button class="btn-chip-remove" onclick={() => removeRecentSession(session.peer_id)} title="Remove">
                      ✕
                    </button>
                  </div>
                </div>
              {/each}
            </div>
          </div>
        {/if}

        <!-- Multi-Monitor Detection & Engine Bar -->
        <div class="engine-bar">
          <div class="engine-item">
            <span class="engine-bullet">●</span>
            <span class="engine-name">Displays:</span>
            <span class="engine-tech">
              {#if availableMonitors.length > 0}
                {availableMonitors.length} Attached ({availableMonitors[0].width}x{availableMonitors[0].height})
              {:else}
                1 Display (1920x1080)
              {/if}
            </span>
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
    border: 1px solid rgba(255, 255, 255, 0.08);
  }

  /* Custom Native Drag Titlebar */
  .custom-titlebar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 42px;
    background: rgba(15, 23, 42, 0.85);
    backdrop-filter: blur(14px);
    border-bottom: 1px solid rgba(255, 255, 255, 0.07);
    padding: 0 0 0 14px;
    user-select: none;
    z-index: 100;
    -webkit-app-region: drag;
  }

  .titlebar-left {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 100%;
    -webkit-app-region: drag;
  }

  .brand-badge {
    width: 24px;
    height: 24px;
    border-radius: 6px;
    background: linear-gradient(135deg, #0284c7, #2563eb);
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ffffff;
    box-shadow: 0 2px 8px rgba(2, 132, 199, 0.35);
  }

  .titlebar-text {
    font-size: 0.85rem;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: #ffffff;
  }

  .network-pill {
    display: flex;
    align-items: center;
    gap: 6px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.72rem;
    font-family: monospace;
    color: #38bdf8;
    margin-left: 6px;
    -webkit-app-region: no-drag;
  }

  .pill-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #0284c7;
  }

  .titlebar-center {
    flex: 1;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    -webkit-app-region: drag;
  }

  .session-status-chip {
    display: flex;
    align-items: center;
    gap: 6px;
    background: rgba(34, 197, 94, 0.12);
    border: 1px solid rgba(34, 197, 94, 0.3);
    color: #4ade80;
    padding: 2px 10px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
    -webkit-app-region: no-drag;
  }

  .status-live-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #22c55e;
    animation: live-pulse 1.4s infinite;
  }

  @keyframes live-pulse {
    0% { transform: scale(0.9); opacity: 0.6; }
    50% { transform: scale(1.3); opacity: 1; }
    100% { transform: scale(0.9); opacity: 0.6; }
  }

  .titlebar-right {
    display: flex;
    align-items: center;
    height: 100%;
    -webkit-app-region: no-drag;
  }

  .btn-disconnect-titlebar {
    background: #ef4444;
    color: #ffffff;
    border: none;
    padding: 4px 10px;
    font-size: 0.75rem;
    font-weight: 600;
    border-radius: 4px;
    cursor: pointer;
    margin-right: 12px;
    -webkit-app-region: no-drag;
  }

  /* Window Control Buttons (Minimize, Maximize, Close) */
  .window-actions {
    display: flex;
    height: 100%;
    -webkit-app-region: no-drag;
  }

  .win-btn {
    width: 44px;
    height: 100%;
    border: none;
    background: transparent;
    color: #94a3b8;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: all 0.15s ease;
    -webkit-app-region: no-drag;
  }

  .win-btn:hover {
    background: rgba(255, 255, 255, 0.08);
    color: #ffffff;
  }

  .win-btn.win-close:hover {
    background: #e11d48;
    color: #ffffff;
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
    background: rgba(15, 23, 42, 0.9);
    backdrop-filter: blur(14px);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    padding: 6px 12px;
    display: flex;
    align-items: center;
    gap: 10px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.5);
    z-index: 100;
  }

  /* Multi-Monitor Switcher in Session */
  .monitor-switcher {
    display: flex;
    align-items: center;
    gap: 5px;
    padding-right: 8px;
    border-right: 1px solid rgba(255, 255, 255, 0.12);
  }

  .switcher-title {
    font-size: 0.72rem;
    color: #64748b;
    font-weight: 600;
  }

  .btn-mon-pill {
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.1);
    color: #94a3b8;
    padding: 3px 8px;
    border-radius: 5px;
    font-size: 0.72rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-mon-pill:hover {
    background: rgba(56, 189, 248, 0.15);
    color: #38bdf8;
    border-color: rgba(56, 189, 248, 0.3);
  }

  .btn-mon-pill.active {
    background: #0284c7;
    color: #ffffff;
    border-color: #38bdf8;
    box-shadow: 0 1px 6px rgba(2, 132, 199, 0.4);
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

  /* Recent Sessions / History Section */
  .recent-sessions-card {
    background: rgba(15, 23, 42, 0.65);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 12px;
    padding: 14px 18px;
    backdrop-filter: blur(12px);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .recent-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .recent-title-group {
    display: flex;
    align-items: center;
    gap: 8px;
    color: #64748b;
  }

  .recent-heading {
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: #94a3b8;
  }

  .recent-badge {
    background: rgba(56, 189, 248, 0.12);
    color: #38bdf8;
    font-size: 0.68rem;
    font-weight: 700;
    padding: 1px 6px;
    border-radius: 10px;
  }

  .recent-chips-container {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 10px;
  }

  .recent-chip {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 8px;
    padding: 8px 12px;
    transition: all 0.2s ease;
  }

  .recent-chip:hover {
    background: rgba(255, 255, 255, 0.06);
    border-color: rgba(56, 189, 248, 0.25);
    transform: translateY(-1px);
  }

  .recent-chip-left {
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
    flex: 1;
    min-width: 0;
  }

  .recent-icon-box {
    width: 28px;
    height: 28px;
    border-radius: 6px;
    background: rgba(56, 189, 248, 0.1);
    color: #38bdf8;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .recent-text-group {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .recent-chip-id {
    font-size: 0.85rem;
    font-weight: 600;
    font-family: monospace;
    color: #f1f5f9;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .recent-chip-time {
    font-size: 0.7rem;
    color: #64748b;
  }

  .recent-chip-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    margin-left: 10px;
  }

  .btn-chip-connect {
    background: #0284c7;
    border: none;
    color: #ffffff;
    font-size: 0.74rem;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 5px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-chip-connect:hover {
    background: #0369a1;
  }

  .btn-chip-remove {
    background: transparent;
    border: none;
    color: #64748b;
    font-size: 0.78rem;
    padding: 4px 6px;
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-chip-remove:hover {
    background: rgba(239, 68, 68, 0.2);
    color: #f87171;
  }

  /* Mobile and Touch Responsive Styles */
  .mobile-virtual-keyboard-input {
    position: absolute;
    top: -9999px;
    left: -9999px;
    opacity: 0;
    pointer-events: auto;
    width: 1px;
    height: 1px;
  }

  .canvas-element {
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    transition: transform 0.05s ease-out;
  }

  .remote-viewport {
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    overflow: hidden;
  }

  .zoom-reset-btn {
    background: #0284c7 !important;
    color: #ffffff !important;
    font-weight: 700;
  }

  .keyboard-btn {
    font-size: 1rem;
  }

  @media (max-width: 768px) {
    .desks-row {
      flex-direction: column !important;
      gap: 16px;
    }
    .desk-card {
      min-width: 100% !important;
    }
    .session-hud {
      bottom: 12px;
      right: 12px;
      padding: 6px 10px;
      gap: 6px;
      max-width: calc(100vw - 24px);
      flex-wrap: wrap;
    }
    .hud-tag {
      font-size: 0.68rem;
      padding: 3px 6px;
    }
    .hud-action {
      padding: 5px 8px;
      font-size: 0.85rem;
    }
    .quick-connect-form {
      flex-direction: column;
    }
    .btn-connect-primary {
      width: 100%;
      justify-content: center;
    }
  }
</style>
