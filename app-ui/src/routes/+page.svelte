<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { check, type Update } from "@tauri-apps/plugin-updater";
  import { getVersion } from "@tauri-apps/api/app";
  import { relaunch } from "@tauri-apps/plugin-process";

  type AccessLevel = "ViewOnly" | "Standard" | "FullAccess";

  interface IncomingRequestInfo {
    request_id: string;
    client_peer_id: string;
    client_ip: string;
  }

  interface HostSessionInfo {
    is_active: boolean;
    client_peer_id: string | null;
    client_ip: string | null;
    access_level: AccessLevel;
    default_access_level: AccessLevel;
  }

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
    initial_access_level?: AccessLevel;
    requires_password?: boolean;
    challenge?: string;
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

  // Permissions & Incoming Requests State
  let incomingRequest = $state<IncomingRequestInfo | null>(null);
  let showIncomingModal = $state(false);
  let selectedIncomingPermission = $state<AccessLevel>("Standard");
  let hostSession = $state<HostSessionInfo | null>(null);
  let activeDashboardTab = $state<"connect" | "permissions" | "engine">("connect");
  let clientAccessLevel = $state<AccessLevel>("Standard");
  let unlistenIncoming: UnlistenFn | null = null;
  let unlistenSession: UnlistenFn | null = null;

  // Unattended Access State
  interface UnattendedAccessSummary {
    enabled: boolean;
    has_password: boolean;
    profile: AccessLevel;
  }

  let unattendedConfig = $state<UnattendedAccessSummary>({
    enabled: false,
    has_password: false,
    profile: "Standard",
  });
  let showSetPasswordModal = $state(false);
  let passwordInput = $state("");
  let confirmPasswordInput = $state("");
  let showPasswordText = $state(false);
  let showConfirmPasswordText = $state(false);
  let selectedProfileModal = $state<AccessLevel>("Standard");
  let isProfileDropdownOpen = $state(false);
  let setPasswordError = $state("");
  let isSavingPassword = $state(false);

  // Client-side Remote Device Password Challenge Modal
  let showClientPasswordModal = $state(false);
  let clientPasswordInput = $state("");
  let showClientPasswordText = $state(false);
  let clientPasswordError = $state("");

  // Auto-Updater State
  let currentAppVersion = $state("1.0.6");
  let availableUpdate = $state<Update | null>(null);
  let isCheckingUpdate = $state(false);
  let isDownloadingUpdate = $state(false);
  let downloadProgress = $state(0);
  let downloadedBytes = $state(0);
  let totalBytes = $state(0);
  let updateStatusText = $state("");
  let showUpdateModal = $state(false);
  let updateCheckTimer: number | null = null;

  function formatBytes(bytes: number): string {
    if (!bytes || bytes <= 0) return "0 MB";
    const mb = bytes / (1024 * 1024);
    return `${mb.toFixed(1)} MB`;
  }

  async function checkForUpdates(silent: boolean = false) {
    if (isCheckingUpdate || isDownloadingUpdate) return;
    isCheckingUpdate = true;
    if (!silent) {
      updateStatusText = "Checking for updates...";
    }

    try {
      // 10-second timeout guarantee so checking never hangs indefinitely
      const timeoutPromise = new Promise<null>((_, reject) =>
        setTimeout(() => reject(new Error("Update check request timed out")), 10000)
      );

      const update = await Promise.race([check(), timeoutPromise]);
      if (update) {
        availableUpdate = update;
        showUpdateModal = true;
        updateStatusText = `Update available: v${update.version}`;
      } else {
        availableUpdate = null;
        if (!silent) {
          updateStatusText = `OpenRemote is up to date (v${currentAppVersion})`;
        }
      }
    } catch (err: any) {
      console.warn("Auto-updater check notice:", err);
      availableUpdate = null;
      if (!silent) {
        const errMsg = typeof err === "string" ? err : (err?.message || JSON.stringify(err));
        if (errMsg.includes("timed out")) {
          updateStatusText = "Update check timed out. Please check your network connection.";
        } else if (errMsg.includes("404") || errMsg.includes("not found") || errMsg.includes("release")) {
          updateStatusText = `No new updates found (v${currentAppVersion} is latest)`;
        } else {
          updateStatusText = `OpenRemote v${currentAppVersion} is running`;
        }
      }
    } finally {
      isCheckingUpdate = false;
    }
  }

  async function startUpdateAndRestart() {
    if (!availableUpdate || isDownloadingUpdate) return;
    isDownloadingUpdate = true;
    downloadProgress = 0;
    downloadedBytes = 0;
    totalBytes = 0;

    try {
      await availableUpdate.downloadAndInstall((event) => {
        if (event.event === "Started") {
          totalBytes = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength;
          if (totalBytes > 0) {
            downloadProgress = Math.min(100, Math.round((downloadedBytes / totalBytes) * 100));
          }
        } else if (event.event === "Finished") {
          downloadProgress = 100;
        }
      });

      // Seamless relaunch using @tauri-apps/plugin-process relaunch() with app_relaunch fallback
      try {
        await relaunch();
      } catch (relaunchErr) {
        console.warn("relaunch() fallback to app_relaunch:", relaunchErr);
        await invoke("app_relaunch");
      }
    } catch (err: any) {
      console.error("Failed to download or install update:", err);
      alert(`Update installation error: ${err?.message || err}`);
      isDownloadingUpdate = false;
    }
  }

  function dismissUpdateModal() {
    if (isDownloadingUpdate) return;
    showUpdateModal = false;
  }

  async function loadHostSessionState() {
    try {
      const session: HostSessionInfo = await invoke("get_host_session_state");
      hostSession = session;
      if (!incomingRequest) {
        selectedIncomingPermission = session.default_access_level || "Standard";
      }
    } catch (err) {
      console.error("Failed to load host session state:", err);
    }
  }

  async function acceptIncomingConnection() {
    if (!incomingRequest) return;
    try {
      await invoke("respond_connection_request", {
        requestId: incomingRequest.request_id,
        accept: true,
        accessLevel: selectedIncomingPermission,
      });
      showIncomingModal = false;
      incomingRequest = null;
      await loadHostSessionState();
    } catch (err) {
      console.error("Failed to accept connection:", err);
    }
  }

  async function declineIncomingConnection() {
    if (!incomingRequest) return;
    try {
      await invoke("respond_connection_request", {
        requestId: incomingRequest.request_id,
        accept: false,
        accessLevel: "ViewOnly",
      });
      showIncomingModal = false;
      incomingRequest = null;
      await loadHostSessionState();
    } catch (err) {
      console.error("Failed to decline connection:", err);
    }
  }

  async function updateHostAccessLevel(level: AccessLevel) {
    try {
      await invoke("set_session_access_level", { accessLevel: level });
      if (hostSession) {
        hostSession.access_level = level;
      }
    } catch (err) {
      console.error("Failed to update session access level:", err);
    }
  }

  async function updateDefaultAccessLevel(level: AccessLevel) {
    try {
      await invoke("set_default_access_level", { accessLevel: level });
      if (hostSession) {
        hostSession.default_access_level = level;
      }
    } catch (err) {
      console.error("Failed to update default access level:", err);
    }
  }

  async function loadUnattendedConfig() {
    try {
      const cfg: UnattendedAccessSummary = await invoke("get_unattended_access_config");
      unattendedConfig = cfg;
      selectedProfileModal = cfg.profile;
    } catch (err) {
      console.error("Failed to load unattended access config:", err);
    }
  }

  async function handleToggleUnattended(enable: boolean) {
    if (enable && !unattendedConfig.has_password) {
      openSetPasswordModal();
      return;
    }
    try {
      const updated: UnattendedAccessSummary = await invoke("toggle_unattended_access", { enabled: enable });
      unattendedConfig = updated;
    } catch (err: any) {
      console.error("Failed to toggle unattended access:", err);
    }
  }

  async function handleProfileChange(newProfile: AccessLevel) {
    try {
      await invoke("update_unattended_access_profile", { profile: newProfile });
      unattendedConfig.profile = newProfile;
    } catch (err) {
      console.error("Failed to update unattended profile:", err);
    }
  }

  function openSetPasswordModal() {
    passwordInput = "";
    confirmPasswordInput = "";
    setPasswordError = "";
    showPasswordText = false;
    showConfirmPasswordText = false;
    selectedProfileModal = unattendedConfig.profile || "Standard";
    isProfileDropdownOpen = false;
    showSetPasswordModal = true;
  }

  function closeSetPasswordModal() {
    showSetPasswordModal = false;
    isProfileDropdownOpen = false;
    passwordInput = "";
    confirmPasswordInput = "";
    setPasswordError = "";
  }

  function selectModalProfile(profile: AccessLevel) {
    selectedProfileModal = profile;
    isProfileDropdownOpen = false;
  }

  async function saveUnattendedPassword() {
    if (!passwordInput.trim()) {
      setPasswordError = "Password cannot be empty";
      return;
    }
    if (passwordInput.length < 4) {
      setPasswordError = "Password must be at least 4 characters";
      return;
    }
    if (passwordInput !== confirmPasswordInput) {
      setPasswordError = "Passwords do not match";
      return;
    }
    isSavingPassword = true;
    setPasswordError = "";
    try {
      const updated: UnattendedAccessSummary = await invoke("set_unattended_access_password", {
        password: passwordInput,
        profile: selectedProfileModal,
      });
      unattendedConfig = updated;
      closeSetPasswordModal();
    } catch (err: any) {
      setPasswordError = typeof err === "string" ? err : JSON.stringify(err);
    } finally {
      isSavingPassword = false;
    }
  }

  async function removeUnattendedPassword() {
    try {
      const updated: UnattendedAccessSummary = await invoke("remove_unattended_access_password");
      unattendedConfig = updated;
    } catch (err) {
      console.error("Failed to remove unattended password:", err);
    }
  }

  async function disconnectHostPartner() {
    try {
      await invoke("disconnect_host_session");
      await loadHostSessionState();
    } catch (err) {
      console.error("Failed to disconnect host partner:", err);
    }
  }

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

  async function connectToRemote(passwordToUse?: string) {
    if (!targetAddress.trim() || isConnecting) return;
    isConnecting = true;
    connectionError = "";
    clientPasswordError = "";

    try {
      const res: ClientConnectResult = await invoke("connect_to_remote", {
        targetAddress: targetAddress.trim(),
        port: 44321,
        password: passwordToUse || null,
      });

      if (res.requires_password) {
        isConnecting = false;
        showClientPasswordModal = true;
        clientPasswordInput = "";
        clientPasswordError = "";
        return;
      }

      if (res.success) {
        showClientPasswordModal = false;
        clientPasswordInput = "";
        clientPasswordError = "";
        isConnected = true;
        isConnecting = false;
        if (res.initial_access_level) {
          clientAccessLevel = res.initial_access_level;
        }
        loadRecentSessions();
        initStreamWebSocket(res.local_ws_port);
      }
    } catch (err: any) {
      const errMsg = typeof err === "string" ? err : JSON.stringify(err);
      if (showClientPasswordModal) {
        clientPasswordError = errMsg.includes("Incorrect") ? "Incorrect Password. Please try again." : errMsg;
      } else {
        connectionError = errMsg;
      }
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
    clientAccessLevel = "Standard";
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
      // 1. Dynamic Display Manifest & Permission updates from Remote Host
      if (typeof event.data === "string") {
        try {
          const msg = JSON.parse(event.data);
          if (msg.type === "display_manifest" && Array.isArray(msg.displays)) {
            remoteDisplays = msg.displays;
            activeRemoteDisplayId = msg.active_display_id || 1;
          } else if (msg.type === "permission_update" && msg.access_level) {
            clientAccessLevel = msg.access_level;
            console.log("Remote permission level updated to:", clientAccessLevel);
          }
        } catch (e) {
          console.error("Stream json parse error:", e);
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
    if (clientAccessLevel === "ViewOnly") return;
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
    if (clientAccessLevel === "ViewOnly" && zoomScale <= 1.05) return;
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
    if (clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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
    if (!isConnected || clientAccessLevel === "ViewOnly") return;
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

  let fpsInterval: number | null = null;

  onMount(async () => {
    try {
      const ver = await getVersion();
      if (ver) {
        currentAppVersion = ver;
      }
    } catch (err) {
      console.warn("Could not read dynamic app version from Tauri:", err);
    }

    loadSystemInfo();
    loadMonitors();
    loadRecentSessions();
    loadHostSessionState();
    loadUnattendedConfig();

    // Wait 4 seconds on startup to allow network/app initialization, then silently check for updates in background
    updateCheckTimer = window.setTimeout(() => {
      checkForUpdates(true);
    }, 4000);

    animFrameId = requestAnimationFrame(renderLoop);

    fpsInterval = window.setInterval(() => {
      fps = frameCount;
      frameCount = 0;
    }, 1000);

    try {
      unlistenIncoming = await listen<IncomingRequestInfo>("incoming-connection-request", (event) => {
        incomingRequest = event.payload;
        showIncomingModal = true;
        if (hostSession) {
          selectedIncomingPermission = hostSession.default_access_level || "Standard";
        }
      });

      unlistenSession = await listen<HostSessionInfo>("session-status-changed", (event) => {
        hostSession = event.payload;
      });
    } catch (err) {
      console.error("Failed to attach event listeners:", err);
    }
  });

  onDestroy(() => {
    if (updateCheckTimer) clearTimeout(updateCheckTimer);
    if (animFrameId) cancelAnimationFrame(animFrameId);
    if (fpsInterval) clearInterval(fpsInterval);
    if (ws) ws.close();
    if (unlistenIncoming) unlistenIncoming();
    if (unlistenSession) unlistenSession();
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

      {#if availableUpdate}
        <button
          class="update-pill"
          data-tauri-drag-region="false"
          onclick={() => (showUpdateModal = true)}
          title="Software update is available"
        >
          <span class="update-dot"></span>
          <span>v{availableUpdate.version} Available</span>
        </button>
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

          <!-- Session Permission Badge -->
          <div
            class="hud-tag permission-badge"
            class:badge-viewonly={clientAccessLevel === "ViewOnly"}
            class:badge-standard={clientAccessLevel === "Standard"}
            class:badge-full={clientAccessLevel === "FullAccess"}
            title="Current Session Permission Level"
          >
            {#if clientAccessLevel === "ViewOnly"}
              🔒 View Only
            {:else if clientAccessLevel === "Standard"}
              ⚡ Standard
            {:else}
              🛡 Full Access
            {/if}
          </div>

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
        <!-- Top Navigation Tabs -->
        <div class="dashboard-tabs">
          <button
            class="tab-btn"
            class:active={activeDashboardTab === "connect"}
            onclick={() => (activeDashboardTab = "connect")}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
              <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
              <line x1="8" y1="21" x2="16" y2="21"></line>
              <line x1="12" y1="17" x2="12" y2="21"></line>
            </svg>
            <span>Connect & Share</span>
          </button>
          <button
            class="tab-btn"
            class:active={activeDashboardTab === "permissions"}
            onclick={() => (activeDashboardTab = "permissions")}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"></path>
            </svg>
            <span>Permissions</span>
            {#if hostSession?.is_active}
              <span class="active-session-indicator" title="Active Client Connected">●</span>
            {/if}
          </button>
          <button
            class="tab-btn"
            class:active={activeDashboardTab === "engine"}
            onclick={() => (activeDashboardTab = "engine")}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
              <circle cx="12" cy="12" r="3"></circle>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"></path>
            </svg>
            <span>Engine & Displays</span>
          </button>
        </div>

        <!-- Scrollable Dashboard Content Area -->
        <div class="dashboard-scroll-body">
          {#if activeDashboardTab === "connect"}
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
                    onclick={() => connectToRemote()}
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
        {:else if activeDashboardTab === "permissions"}
          <!-- Dedicated Permissions Tab View -->
          <div class="permissions-tab-container">
            <!-- Unattended Access Section (AnyDesk Style) -->
            <div class="perm-section-card unattended-card">
              <div class="unattended-header-row">
                <div class="card-caption">
                  <span class="section-label">UNATTENDED ACCESS</span>
                  <h2 class="card-heading">Password & Security Profile</h2>
                  <p class="section-desc">
                    Allows accessing this computer remotely by entering a password without requiring manual confirmation at this desk.
                  </p>
                </div>
                <div class="unattended-toggle-wrap">
                  <label class="switch-toggle" title="Enable / Disable Unattended Access">
                    <input
                      type="checkbox"
                      checked={unattendedConfig.enabled}
                      onchange={(e) => handleToggleUnattended(e.currentTarget.checked)}
                    />
                    <span class="switch-slider"></span>
                  </label>
                  <span class="toggle-status-text" class:status-enabled={unattendedConfig.enabled}>
                    {unattendedConfig.enabled ? "Enabled" : "Disabled"}
                  </span>
                </div>
              </div>

              <div class="unattended-body-grid">
                <!-- Permission Profile Selector -->
                <div class="unattended-box">
                  <label class="box-label" for="unattended-profile-dropdown">Permission Profile</label>
                  <div class="dropdown-wrapper">
                    <select
                      id="unattended-profile-dropdown"
                      class="profile-select-control"
                      value={unattendedConfig.profile}
                      onchange={(e) => handleProfileChange(e.currentTarget.value as AccessLevel)}
                    >
                      <option value="ViewOnly">Screen Sharing (View Only)</option>
                      <option value="Standard">Default (Standard Access)</option>
                      <option value="FullAccess">Full Access</option>
                    </select>
                  </div>
                  <p class="box-hint">
                    {#if unattendedConfig.profile === "ViewOnly"}
                      🔒 Screen Sharing (View Only): Remote clicks and typing will be blocked.
                    {:else if unattendedConfig.profile === "Standard"}
                      ⚡ Default (Standard Access): Remote user has standard mouse and keyboard input.
                    {:else}
                      🛡 Full Access: Remote user has complete interactive input and display switching.
                    {/if}
                  </p>
                </div>

                <!-- Password Credentials Box -->
                <div class="unattended-box">
                  <span class="box-label">Access Credentials</span>
                  <div class="pwd-status-indicator">
                    {#if unattendedConfig.has_password}
                      <span class="pwd-indicator-dot green">●</span>
                      <span class="pwd-indicator-label">Password is set & active</span>
                    {:else}
                      <span class="pwd-indicator-dot gray">○</span>
                      <span class="pwd-indicator-label">No password set</span>
                    {/if}
                  </div>
                  <div class="pwd-action-buttons">
                    <button class="btn-set-password" onclick={openSetPasswordModal}>
                      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">
                        <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
                        <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
                      </svg>
                      <span>{unattendedConfig.has_password ? "Change Password" : "Set Password"}</span>
                    </button>
                    {#if unattendedConfig.has_password}
                      <button class="btn-remove-password" onclick={removeUnattendedPassword} title="Remove password and disable unattended access">
                        Remove
                      </button>
                    {/if}
                  </div>
                  <p class="box-hint crypto-hint">
                    🔐 Protected using SHA-256 with 128-bit cryptographic salt.
                  </p>
                </div>
              </div>
            </div>

            <!-- Active Connection Security Status Card -->
            <div class="perm-section-card">
              <div class="card-caption">
                <span class="section-label">LIVE SESSION ACCESS</span>
                <h2 class="card-heading">Active Connection Controls</h2>
              </div>

              {#if hostSession && hostSession.is_active}
                <div class="active-session-panel">
                  <div class="session-connected-alert">
                    <span class="pulse-indicator"></span>
                    <div class="session-peer-details">
                      <span class="session-title">Remote Partner Connected</span>
                      <span class="session-sub">Peer ID: <strong>{hostSession.client_peer_id || "Direct Client"}</strong> • IP: {hostSession.client_ip || "Direct LAN"}</span>
                    </div>
                    <button class="btn-disconnect-host-partner" onclick={disconnectHostPartner} title="Disconnect remote partner">
                      Disconnect
                    </button>
                  </div>

                  <p class="section-desc">
                    Switch access mode dynamically in real-time. Changes apply instantly without dropping the video feed:
                  </p>

                  <div class="perm-options-grid">
                    <button
                      class="perm-option-card"
                      class:active={hostSession.access_level === "ViewOnly"}
                      onclick={() => updateHostAccessLevel("ViewOnly")}
                    >
                      <div class="perm-option-header">
                        <span class="perm-badge-icon">🔒</span>
                        <span class="perm-option-title">Screen Share (View Only)</span>
                      </div>
                      <p class="perm-option-desc">Remote partner can only observe your screen. All mouse clicks, typing, and gestures are blocked.</p>
                    </button>

                    <button
                      class="perm-option-card"
                      class:active={hostSession.access_level === "Standard"}
                      onclick={() => updateHostAccessLevel("Standard")}
                    >
                      <div class="perm-option-header">
                        <span class="perm-badge-icon">⚡</span>
                        <span class="perm-option-title">Standard (Interactive)</span>
                      </div>
                      <p class="perm-option-desc">Allows mouse pointer control, clicks, and keyboard typing. Ideal for paired work and standard remote support.</p>
                    </button>

                    <button
                      class="perm-option-card"
                      class:active={hostSession.access_level === "FullAccess"}
                      onclick={() => updateHostAccessLevel("FullAccess")}
                    >
                      <div class="perm-option-header">
                        <span class="perm-badge-icon">🛡</span>
                        <span class="perm-option-title">Full Access</span>
                      </div>
                      <p class="perm-option-desc">Complete interactive remote control with all input capabilities and monitor switching unlocked.</p>
                    </button>
                  </div>
                </div>
              {:else}
                <div class="no-session-empty">
                  <svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5">
                    <circle cx="12" cy="12" r="10"></circle>
                    <line x1="4.93" y1="4.93" x2="19.07" y2="19.07"></line>
                  </svg>
                  <span>No remote partner is currently connected to this host.</span>
                </div>
              {/if}
            </div>

            <!-- Default Permission Policy Card -->
            <div class="perm-section-card">
              <div class="card-caption">
                <span class="section-label">DEFAULT ACCESS POLICY</span>
                <h2 class="card-heading">Default Mode for Incoming Connections</h2>
              </div>
              <p class="section-desc">
                Choose the pre-selected permission level that will be proposed when a new incoming connection dialog appears:
              </p>

              <div class="perm-options-grid">
                <button
                  class="perm-option-card"
                  class:active={(hostSession?.default_access_level || "Standard") === "ViewOnly"}
                  onclick={() => updateDefaultAccessLevel("ViewOnly")}
                >
                  <div class="perm-option-header">
                    <span class="perm-badge-icon">🔒</span>
                    <span class="perm-option-title">Screen Share (View Only)</span>
                  </div>
                  <p class="perm-option-desc">Pre-selects view-only access by default for incoming requests.</p>
                </button>

                <button
                  class="perm-option-card"
                  class:active={(hostSession?.default_access_level || "Standard") === "Standard"}
                  onclick={() => updateDefaultAccessLevel("Standard")}
                >
                  <div class="perm-option-header">
                    <span class="perm-badge-icon">⚡</span>
                    <span class="perm-option-title">Standard (Interactive)</span>
                  </div>
                  <p class="perm-option-desc">Pre-selects mouse and keyboard control (Recommended).</p>
                </button>

                <button
                  class="perm-option-card"
                  class:active={(hostSession?.default_access_level || "Standard") === "FullAccess"}
                  onclick={() => updateDefaultAccessLevel("FullAccess")}
                >
                  <div class="perm-option-header">
                    <span class="perm-badge-icon">🛡</span>
                    <span class="perm-option-title">Full Access</span>
                  </div>
                  <p class="perm-option-desc">Pre-selects unrestricted full control mode.</p>
                </button>
              </div>
            </div>
          </div>
        {:else if activeDashboardTab === "engine"}
          <!-- Engine & Displays Tab View -->
          <div class="engine-tab-container">
            <div class="engine-section-card">
              <div class="card-caption">
                <span class="section-label">ATTACHED DISPLAYS</span>
                <h2 class="card-heading">Local Monitor Configuration</h2>
              </div>
              <div class="monitors-list">
                {#if availableMonitors.length > 0}
                  {#each availableMonitors as mon}
                    <div class="monitor-card" class:active={activeMonitorIndex === mon.index}>
                      <div class="mon-left">
                        <span class="mon-icon">🖥</span>
                        <div>
                          <div class="mon-name">{mon.name} {#if mon.is_primary}<span class="primary-badge">PRIMARY</span>{/if}</div>
                          <div class="mon-res">{mon.width} x {mon.height}</div>
                        </div>
                      </div>
                      <button
                        class="btn-select-mon"
                        class:selected={activeMonitorIndex === mon.index}
                        onclick={() => switchMonitor(mon.index)}
                      >
                        {activeMonitorIndex === mon.index ? "Active Stream" : "Switch Display"}
                      </button>
                    </div>
                  {/each}
                {:else}
                  <p class="text-muted">Primary Display (1920x1080)</p>
                {/if}
              </div>
            </div>
            <!-- Software Update Card -->
            <div class="engine-section-card">
              <div class="card-caption">
                <span class="section-label">SOFTWARE UPDATE</span>
                <h2 class="card-heading">Version & Updates</h2>
              </div>
              <div class="update-card-content">
                <div class="update-info-group">
                  <div class="update-version-row">
                    <span class="version-label">Current Version:</span>
                    <span class="version-badge">v{currentAppVersion}</span>
                    {#if availableUpdate}
                      <span class="update-ready-pill">v{availableUpdate.version} Ready</span>
                    {/if}
                  </div>
                  <p class="update-status-msg">{updateStatusText || `OpenRemote is up to date (v${currentAppVersion})`}</p>
                </div>
                <button
                  class="btn-check-updates"
                  disabled={isCheckingUpdate || isDownloadingUpdate}
                  onclick={() => checkForUpdates(false)}
                >
                  {#if isCheckingUpdate}
                    <span class="btn-spinner"></span>
                    <span>Checking...</span>
                  {:else}
                    <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/>
                    </svg>
                    <span>Check for Updates</span>
                  {/if}
                </button>
              </div>
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
    </div>
  {/if}

    <!-- Auto-Updater Modal Dialog -->
    {#if showUpdateModal && availableUpdate}
      <div class="modal-backdrop">
        <div class="modal-card update-modal-card">
          <div class="modal-header">
            <div class="modal-badge-icon update-badge-icon">
              <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
                <polyline points="7 10 12 15 17 10"></polyline>
                <line x1="12" y1="15" x2="12" y2="3"></line>
              </svg>
            </div>
            <div>
              <h3 class="modal-title">Update Available</h3>
              <p class="modal-subtitle">Version v{availableUpdate.version} is ready to install.</p>
            </div>
          </div>

          <div class="update-details-box">
            <div class="detail-row">
              <span class="detail-key">Current Version:</span>
              <span class="detail-val">v{availableUpdate.currentVersion || currentAppVersion}</span>
            </div>
            <div class="detail-row">
              <span class="detail-key">Latest Version:</span>
              <span class="detail-val update-val-highlight">v{availableUpdate.version}</span>
            </div>
            {#if availableUpdate.date}
              <div class="detail-row">
                <span class="detail-key">Release Date:</span>
                <span class="detail-val">{availableUpdate.date.split("T")[0]}</span>
              </div>
            {/if}
          </div>

          {#if availableUpdate.body}
            <div class="update-notes-box">
              <span class="notes-heading">RELEASE NOTES</span>
              <div class="notes-content">{availableUpdate.body}</div>
            </div>
          {/if}

          {#if isDownloadingUpdate}
            <div class="update-progress-section">
              <div class="progress-bar-track">
                <div class="progress-bar-fill" style="width: {downloadProgress}%;"></div>
              </div>
              <div class="progress-meta">
                <span>Downloading update... {downloadProgress}%</span>
                <span>{formatBytes(downloadedBytes)} / {formatBytes(totalBytes)}</span>
              </div>
            </div>
          {/if}

          <div class="modal-actions">
            <button
              type="button"
              class="btn-decline"
              disabled={isDownloadingUpdate}
              onclick={dismissUpdateModal}
            >
              Later
            </button>
            <button
              type="button"
              class="btn-accept"
              disabled={isDownloadingUpdate}
              onclick={startUpdateAndRestart}
            >
              {#if isDownloadingUpdate}
                <span class="btn-spinner"></span>
                <span>Installing Update...</span>
              {:else}
                <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
                  <polyline points="7 10 12 15 17 10"></polyline>
                  <line x1="12" y1="15" x2="12" y2="3"></line>
                </svg>
                <span>Update & Restart Now</span>
              {/if}
            </button>
          </div>
        </div>
      </div>
    {/if}

    <!-- Incoming Connection Request Modal Dialog -->
    {#if showIncomingModal && incomingRequest}
      <div class="modal-backdrop">
        <div class="modal-card">
          <div class="modal-header">
            <div class="modal-badge-icon">
              <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                <circle cx="8.5" cy="7" r="4"></circle>
                <line x1="20" y1="8" x2="20" y2="14"></line>
                <line x1="23" y1="11" x2="17" y2="11"></line>
              </svg>
            </div>
            <div>
              <h3 class="modal-title">Incoming Connection Request</h3>
              <p class="modal-subtitle">A remote device wants to connect to this computer</p>
            </div>
          </div>

          <div class="modal-details">
            <div class="detail-row">
              <span class="detail-key">Remote Peer ID:</span>
              <span class="detail-val">{incomingRequest.client_peer_id}</span>
            </div>
            <div class="detail-row">
              <span class="detail-key">Remote IP Address:</span>
              <span class="detail-val">{incomingRequest.client_ip}</span>
            </div>
          </div>

          <div class="modal-permission-section">
            <span class="section-label">GRANT PERMISSION LEVEL:</span>
            <div class="perm-cards-list">
              <button
                type="button"
                class="perm-choice-card"
                class:selected={selectedIncomingPermission === "ViewOnly"}
                onclick={() => (selectedIncomingPermission = "ViewOnly")}
              >
                <div class="choice-title">
                  <span class="choice-icon">🔒</span>
                  <span>Screen Share (View Only)</span>
                </div>
                <div class="choice-desc">Remote user can only watch your screen. Mouse clicks and typing are strictly blocked.</div>
              </button>

              <button
                type="button"
                class="perm-choice-card"
                class:selected={selectedIncomingPermission === "Standard"}
                onclick={() => (selectedIncomingPermission = "Standard")}
              >
                <div class="choice-title">
                  <span class="choice-icon">⚡</span>
                  <span>Standard (Default)</span>
                </div>
                <div class="choice-desc">Allows remote mouse clicks and keyboard typing for interactive assistance.</div>
              </button>

              <button
                type="button"
                class="perm-choice-card"
                class:selected={selectedIncomingPermission === "FullAccess"}
                onclick={() => (selectedIncomingPermission = "FullAccess")}
              >
                <div class="choice-title">
                  <span class="choice-icon">🛡</span>
                  <span>Full Access</span>
                </div>
                <div class="choice-desc">Complete unrestricted control of mouse, keyboard, and display switching.</div>
              </button>
            </div>
          </div>

          <div class="modal-actions">
            <button class="btn-decline" onclick={declineIncomingConnection}>
              Decline
            </button>
            <button class="btn-accept" onclick={acceptIncomingConnection}>
              Accept & Start Sharing
            </button>
          </div>
        </div>
      </div>
    {/if}

    <!-- Set Password for Unattended Access Modal Dialog -->
    {#if showSetPasswordModal}
      <div class="modal-backdrop">
        <div class="modal-card password-modal-card">
          <div class="modal-header">
            <div class="modal-badge-icon auth-badge-icon">
              <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
                <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
              </svg>
            </div>
            <div>
              <h3 class="modal-title">Set Password for Unattended Access</h3>
              <p class="modal-subtitle">Choose a secure password to access this computer from other devices</p>
            </div>
          </div>

          <div class="modal-form-body">
            <!-- New Password -->
            <div class="input-field-group">
              <label for="new-unattended-pwd">New Password</label>
              <div class="password-input-wrap">
                <input
                  id="new-unattended-pwd"
                  type={showPasswordText ? "text" : "password"}
                  placeholder="Enter a strong password"
                  bind:value={passwordInput}
                  class="modal-input"
                />
                <button
                  type="button"
                  class="btn-eye"
                  onclick={() => (showPasswordText = !showPasswordText)}
                  title={showPasswordText ? "Hide password" : "Show password"}
                >
                  {#if showPasswordText}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"></path>
                      <line x1="1" y1="1" x2="23" y2="23"></line>
                    </svg>
                  {:else}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"></path>
                      <circle cx="12" cy="12" r="3"></circle>
                    </svg>
                  {/if}
                </button>
              </div>
            </div>

            <!-- Confirm Password -->
            <div class="input-field-group">
              <label for="confirm-unattended-pwd">Confirm Password</label>
              <div class="password-input-wrap">
                <input
                  id="confirm-unattended-pwd"
                  type={showConfirmPasswordText ? "text" : "password"}
                  placeholder="Repeat your password"
                  bind:value={confirmPasswordInput}
                  class="modal-input"
                />
                <button
                  type="button"
                  class="btn-eye"
                  onclick={() => (showConfirmPasswordText = !showConfirmPasswordText)}
                  title={showConfirmPasswordText ? "Hide password" : "Show password"}
                >
                  {#if showConfirmPasswordText}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"></path>
                      <line x1="1" y1="1" x2="23" y2="23"></line>
                    </svg>
                  {:else}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"></path>
                      <circle cx="12" cy="12" r="3"></circle>
                    </svg>
                  {/if}
                </button>
              </div>
            </div>

            <!-- Permission Profile (Custom Dropdown) -->
            <div class="input-field-group profile-dropdown-group">
              <label for="modal-profile-trigger">Default Permission Profile</label>

              {#if isProfileDropdownOpen}
                <div
                  class="dropdown-overlay"
                  onclick={() => (isProfileDropdownOpen = false)}
                  role="presentation"
                ></div>
              {/if}

              <div class="custom-dropdown-container">
                <button
                  type="button"
                  id="modal-profile-trigger"
                  class="custom-dropdown-trigger"
                  class:active={isProfileDropdownOpen}
                  onclick={() => (isProfileDropdownOpen = !isProfileDropdownOpen)}
                  aria-haspopup="listbox"
                  aria-expanded={isProfileDropdownOpen}
                >
                  <div class="dropdown-trigger-left">
                    {#if selectedProfileModal === "ViewOnly"}
                      <span class="profile-icon">🔒</span>
                      <span class="profile-title">Screen Sharing (View Only)</span>
                    {:else if selectedProfileModal === "Standard"}
                      <span class="profile-icon">⚡</span>
                      <span class="profile-title">Default (Standard Access)</span>
                    {:else}
                      <span class="profile-icon">🛡</span>
                      <span class="profile-title">Full Access</span>
                    {/if}
                  </div>
                  <svg
                    class="chevron-icon"
                    class:rotated={isProfileDropdownOpen}
                    viewBox="0 0 24 24"
                    width="16"
                    height="16"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                  >
                    <polyline points="6 9 12 15 18 9"></polyline>
                  </svg>
                </button>

                {#if isProfileDropdownOpen}
                  <div class="custom-dropdown-menu" role="listbox">
                    <button
                      type="button"
                      class="dropdown-option-item"
                      class:selected={selectedProfileModal === "ViewOnly"}
                      onclick={() => selectModalProfile("ViewOnly")}
                      role="option"
                      aria-selected={selectedProfileModal === "ViewOnly"}
                    >
                      <span class="option-icon">🔒</span>
                      <div class="option-info">
                        <span class="option-name">Screen Sharing (View Only)</span>
                        <span class="option-desc">Remote partner can only view screen; mouse & typing blocked</span>
                      </div>
                      {#if selectedProfileModal === "ViewOnly"}
                        <span class="option-check">✓</span>
                      {/if}
                    </button>

                    <button
                      type="button"
                      class="dropdown-option-item"
                      class:selected={selectedProfileModal === "Standard"}
                      onclick={() => selectModalProfile("Standard")}
                      role="option"
                      aria-selected={selectedProfileModal === "Standard"}
                    >
                      <span class="option-icon">⚡</span>
                      <div class="option-info">
                        <span class="option-name">Default (Standard Access)</span>
                        <span class="option-desc">Allows remote mouse clicks and keyboard typing</span>
                      </div>
                      {#if selectedProfileModal === "Standard"}
                        <span class="option-check">✓</span>
                      {/if}
                    </button>

                    <button
                      type="button"
                      class="dropdown-option-item"
                      class:selected={selectedProfileModal === "FullAccess"}
                      onclick={() => selectModalProfile("FullAccess")}
                      role="option"
                      aria-selected={selectedProfileModal === "FullAccess"}
                    >
                      <span class="option-icon">🛡</span>
                      <div class="option-info">
                        <span class="option-name">Full Access</span>
                        <span class="option-desc">Unrestricted mouse, keyboard, and display switching</span>
                      </div>
                      {#if selectedProfileModal === "FullAccess"}
                        <span class="option-check">✓</span>
                      {/if}
                    </button>
                  </div>
                {/if}
              </div>
            </div>

            {#if setPasswordError}
              <div class="modal-error-alert">
                <span>⚠</span> {setPasswordError}
              </div>
            {/if}
          </div>

          <div class="modal-actions">
            <button type="button" class="btn-decline" onclick={closeSetPasswordModal}>
              Cancel
            </button>
            <button type="button" class="btn-accept" disabled={isSavingPassword} onclick={saveUnattendedPassword}>
              {#if isSavingPassword}
                <span class="btn-spinner"></span>
                <span>Saving...</span>
              {:else}
                Save Password
              {/if}
            </button>
          </div>
        </div>
      </div>
    {/if}

    <!-- Client Password Challenge Modal Dialog (When connecting to an Unattended Remote Host) -->
    {#if showClientPasswordModal}
      <div class="modal-backdrop">
        <div class="modal-card password-modal-card">
          <div class="modal-header">
            <div class="modal-badge-icon auth-badge-icon">
              <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
                <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
              </svg>
            </div>
            <div>
              <h3 class="modal-title">Enter Password for Remote Device</h3>
              <p class="modal-subtitle">Remote Host <strong>{targetAddress}</strong> requires authentication</p>
            </div>
          </div>

          <div class="modal-form-body">
            <div class="input-field-group">
              <label for="client-remote-pwd">Password</label>
              <div class="password-input-wrap">
                <input
                  id="client-remote-pwd"
                  type={showClientPasswordText ? "text" : "password"}
                  placeholder="Enter remote unattended password"
                  bind:value={clientPasswordInput}
                  class="modal-input"
                  onkeydown={(e) => { if (e.key === "Enter") connectToRemote(clientPasswordInput); }}
                />
                <button
                  type="button"
                  class="btn-eye"
                  onclick={() => (showClientPasswordText = !showClientPasswordText)}
                  title={showClientPasswordText ? "Hide password" : "Show password"}
                >
                  {#if showClientPasswordText}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"></path>
                      <line x1="1" y1="1" x2="23" y2="23"></line>
                    </svg>
                  {:else}
                    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
                      <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"></path>
                      <circle cx="12" cy="12" r="3"></circle>
                    </svg>
                  {/if}
                </button>
              </div>
            </div>

            {#if clientPasswordError}
              <div class="modal-error-alert">
                <span>⚠</span> {clientPasswordError}
              </div>
            {/if}
          </div>

          <div class="modal-actions">
            <button
              type="button"
              class="btn-decline"
              onclick={() => {
                showClientPasswordModal = false;
                clientPasswordInput = "";
                clientPasswordError = "";
              }}
            >
              Cancel
            </button>
            <button
              type="button"
              class="btn-accept"
              disabled={isConnecting}
              onclick={() => connectToRemote(clientPasswordInput)}
            >
              {#if isConnecting}
                <span class="btn-spinner"></span>
                <span>Connecting...</span>
              {:else}
                Connect
              {/if}
            </button>
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
    max-width: 1040px;
    width: 100%;
    margin: 0 auto;
    padding: 0 32px;
    height: 100%;
    box-sizing: border-box;
    overflow: hidden;
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
    flex-shrink: 0;
    margin-top: auto;
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
    .btn-connect-primary {
      width: 100%;
      justify-content: center;
    }
  }

  /* HUD Permission Badge */
  .permission-badge {
    padding: 3px 8px;
    border-radius: 4px;
    font-size: 0.72rem;
    font-weight: 700;
  }
  .badge-viewonly {
    background: rgba(234, 179, 8, 0.2);
    color: #facc15;
    border: 1px solid rgba(250, 204, 21, 0.4);
  }
  .badge-standard {
    background: rgba(56, 189, 248, 0.2);
    color: #38bdf8;
    border: 1px solid rgba(56, 189, 248, 0.4);
  }
  .badge-full {
    background: rgba(34, 197, 94, 0.2);
    color: #4ade80;
    border: 1px solid rgba(74, 222, 128, 0.4);
  }

  /* Dashboard Tabs (Fixed at Top of Dashboard) */
  .dashboard-tabs {
    flex-shrink: 0;
    display: flex;
    gap: 8px;
    padding: 16px 0 12px 0;
    margin-bottom: 0;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    position: relative;
    z-index: 20;
    background: transparent;
  }

  /* Scrollable Container for Dashboard Views */
  .dashboard-scroll-body {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding-top: 1.5rem; /* pt-6 / mt-4 spacing so top card does not stick to tab bar */
    padding-bottom: 3rem; /* pb-12 so bottom cards/status bar don't get cut off */
    display: flex;
    flex-direction: column;
    gap: 24px;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.16) transparent;
  }

  .dashboard-scroll-body::-webkit-scrollbar {
    width: 6px;
  }

  .dashboard-scroll-body::-webkit-scrollbar-track {
    background: transparent;
  }

  .dashboard-scroll-body::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.16);
    border-radius: 4px;
  }

  .dashboard-scroll-body::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.28);
  }

  .tab-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: #94a3b8;
    padding: 8px 16px;
    border-radius: 8px;
    font-size: 0.85rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
    position: relative;
  }
  .tab-btn:hover {
    background: rgba(255, 255, 255, 0.08);
    color: #f1f5f9;
  }
  .tab-btn.active {
    background: rgba(56, 189, 248, 0.15);
    color: #38bdf8;
    border-color: rgba(56, 189, 248, 0.35);
  }
  .active-session-indicator {
    color: #22c55e;
    font-size: 0.75rem;
    animation: pulse 1.5s infinite;
  }

  /* Permissions Tab Layout */
  .permissions-tab-container,
  .engine-tab-container {
    display: flex;
    flex-direction: column;
    gap: 20px;
    padding-top: 0.25rem;
  }
  .perm-section-card,
  .engine-section-card {
    background: rgba(15, 23, 42, 0.65);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 12px;
    padding: 20px;
    backdrop-filter: blur(12px);
  }
  .section-desc {
    font-size: 0.85rem;
    color: #94a3b8;
    margin: 8px 0 16px 0;
    line-height: 1.4;
  }
  .perm-options-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 12px;
  }
  .perm-option-card {
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 10px;
    padding: 16px;
    text-align: left;
    cursor: pointer;
    transition: all 0.2s ease;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .perm-option-card:hover {
    background: rgba(255, 255, 255, 0.06);
    border-color: rgba(255, 255, 255, 0.15);
  }
  .perm-option-card.active {
    background: rgba(56, 189, 248, 0.12);
    border-color: #38bdf8;
    box-shadow: 0 0 15px rgba(56, 189, 248, 0.2);
  }
  .perm-option-header {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .perm-badge-icon {
    font-size: 1.2rem;
  }
  .perm-option-title {
    font-size: 0.95rem;
    font-weight: 700;
    color: #f1f5f9;
  }
  .perm-option-desc {
    margin: 0;
    font-size: 0.8rem;
    color: #94a3b8;
    line-height: 1.35;
  }
  .active-session-panel {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .session-connected-alert {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: rgba(34, 197, 94, 0.1);
    border: 1px solid rgba(34, 197, 94, 0.3);
    border-radius: 8px;
  }
  .pulse-indicator {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #22c55e;
    box-shadow: 0 0 8px #22c55e;
    animation: pulse 1.5s infinite;
  }
  .session-title {
    display: block;
    font-weight: 700;
    font-size: 0.9rem;
    color: #4ade80;
  }
  .session-sub {
    font-size: 0.8rem;
    color: #94a3b8;
  }
  .no-session-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 36px 16px;
    color: #64748b;
    font-size: 0.9rem;
  }

  /* Engine Displays List */
  .monitors-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 12px;
  }
  .monitor-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
  }
  .monitor-card.active {
    border-color: rgba(56, 189, 248, 0.4);
    background: rgba(56, 189, 248, 0.05);
  }
  .mon-left {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .mon-icon {
    font-size: 1.4rem;
  }
  .mon-name {
    font-weight: 600;
    font-size: 0.9rem;
    color: #f1f5f9;
  }
  .primary-badge {
    font-size: 0.65rem;
    background: #0284c7;
    color: white;
    padding: 2px 6px;
    border-radius: 4px;
    margin-left: 6px;
  }
  .mon-res {
    font-size: 0.78rem;
    color: #94a3b8;
  }
  .btn-select-mon {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #f1f5f9;
    padding: 6px 14px;
    border-radius: 6px;
    font-size: 0.8rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .btn-select-mon.selected {
    background: #0284c7;
    border-color: #38bdf8;
    color: #fff;
  }

  /* Modal Backdrop & Card */
  .modal-backdrop {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.75);
    backdrop-filter: blur(8px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9999;
    animation: fadeIn 0.15s ease-out;
  }
  .modal-card {
    background: #0f172a;
    border: 1px solid rgba(56, 189, 248, 0.3);
    border-radius: 14px;
    width: 90%;
    max-width: 520px;
    padding: 24px;
    box-shadow: 0 20px 40px rgba(0, 0, 0, 0.6), 0 0 20px rgba(56, 189, 248, 0.15);
    display: flex;
    flex-direction: column;
    gap: 18px;
    position: relative;
    overflow: visible;
  }
  .modal-header {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .modal-badge-icon {
    width: 44px;
    height: 44px;
    border-radius: 10px;
    background: rgba(56, 189, 248, 0.15);
    color: #38bdf8;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }
  .modal-title {
    margin: 0;
    font-size: 1.15rem;
    font-weight: 700;
    color: #f8fafc;
  }
  .modal-subtitle {
    margin: 2px 0 0 0;
    font-size: 0.8rem;
    color: #94a3b8;
  }
  .modal-details {
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 8px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .detail-row {
    display: flex;
    justify-content: space-between;
    font-size: 0.82rem;
  }
  .detail-key {
    color: #94a3b8;
  }
  .detail-val {
    font-family: monospace;
    font-weight: 600;
    color: #f1f5f9;
  }
  .modal-permission-section {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .perm-cards-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .perm-choice-card {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    padding: 12px 14px;
    text-align: left;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .perm-choice-card:hover {
    background: rgba(255, 255, 255, 0.08);
  }
  .perm-choice-card.selected {
    background: rgba(56, 189, 248, 0.15);
    border-color: #38bdf8;
    box-shadow: 0 0 10px rgba(56, 189, 248, 0.2);
  }
  .choice-title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 0.88rem;
    color: #f1f5f9;
  }
  .choice-icon {
    font-size: 1rem;
  }
  .choice-desc {
    margin-top: 4px;
    font-size: 0.76rem;
    color: #94a3b8;
    line-height: 1.3;
  }
  .modal-actions {
    display: flex;
    gap: 10px;
    margin-top: 6px;
  }
  .btn-decline {
    flex: 1;
    background: rgba(239, 68, 68, 0.15);
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #f87171;
    padding: 10px;
    border-radius: 8px;
    font-weight: 600;
    font-size: 0.88rem;
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .btn-decline:hover {
    background: rgba(239, 68, 68, 0.25);
  }
  .btn-accept {
    flex: 2;
    background: #0284c7;
    border: 1px solid #38bdf8;
    color: #ffffff;
    padding: 10px;
    border-radius: 8px;
    font-weight: 700;
    font-size: 0.88rem;
    cursor: pointer;
    transition: all 0.15s ease;
    box-shadow: 0 2px 10px rgba(2, 132, 199, 0.4);
  }
  .btn-accept:hover {
    background: #0369a1;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: scale(0.97); }
    to { opacity: 1; transform: scale(1); }
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }

  /* Titlebar Update Pill */
  .update-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: rgba(34, 197, 94, 0.15);
    border: 1px solid rgba(34, 197, 94, 0.35);
    color: #4ade80;
    padding: 3px 9px;
    border-radius: 9999px;
    font-size: 0.72rem;
    font-weight: 700;
    cursor: pointer;
    transition: all 0.2s ease;
  }
  .update-pill:hover {
    background: rgba(34, 197, 94, 0.25);
    box-shadow: 0 0 10px rgba(34, 197, 94, 0.3);
  }
  .update-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #22c55e;
    animation: pulse 1.2s infinite;
  }

  /* Software Update Card in Engine Tab */
  .update-card-content {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  .update-info-group {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .update-version-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .version-label {
    font-size: 0.82rem;
    color: #94a3b8;
  }
  .version-badge {
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #f1f5f9;
    padding: 2px 7px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-family: monospace;
    font-weight: 700;
  }
  .update-ready-pill {
    background: rgba(34, 197, 94, 0.2);
    border: 1px solid rgba(34, 197, 94, 0.4);
    color: #4ade80;
    padding: 2px 7px;
    border-radius: 4px;
    font-size: 0.72rem;
    font-weight: 700;
  }
  .update-status-msg {
    margin: 0;
    font-size: 0.78rem;
    color: #64748b;
  }
  .btn-check-updates {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    background: rgba(56, 189, 248, 0.12);
    border: 1px solid rgba(56, 189, 248, 0.3);
    color: #38bdf8;
    padding: 8px 16px;
    border-radius: 8px;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s ease;
  }
  .btn-check-updates:hover:not(:disabled) {
    background: rgba(56, 189, 248, 0.22);
    border-color: #38bdf8;
  }
  .btn-check-updates:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  /* Auto-Updater Modal */
  .update-modal-card {
    max-width: 480px;
  }
  .update-badge-icon {
    background: rgba(34, 197, 94, 0.15);
    color: #4ade80;
  }
  .update-details-box {
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 8px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .update-val-highlight {
    color: #4ade80;
    font-weight: 700;
  }
  .update-notes-box {
    background: rgba(0, 0, 0, 0.3);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 8px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 160px;
    overflow-y: auto;
  }
  .notes-heading {
    font-size: 0.72rem;
    font-weight: 700;
    color: #64748b;
    letter-spacing: 0.5px;
  }
  .notes-content {
    font-size: 0.8rem;
    color: #cbd5e1;
    line-height: 1.4;
    white-space: pre-line;
  }
  .update-progress-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .progress-bar-track {
    width: 100%;
    height: 8px;
    background: rgba(255, 255, 255, 0.08);
    border-radius: 4px;
    overflow: hidden;
  }
  .progress-bar-fill {
    height: 100%;
    background: linear-gradient(90deg, #0284c7, #38bdf8);
    border-radius: 4px;
    transition: width 0.15s ease-out;
  }
  .progress-meta {
    display: flex;
    justify-content: space-between;
    font-size: 0.75rem;
    color: #94a3b8;
  }

  /* Unattended Access Styles (AnyDesk style) */
  .unattended-card {
    background: rgba(15, 23, 42, 0.65);
    border: 1px solid rgba(56, 189, 248, 0.18);
  }

  .unattended-header-row {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 18px;
    padding-bottom: 16px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }

  .unattended-toggle-wrap {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-shrink: 0;
  }

  /* Switch Toggle */
  .switch-toggle {
    position: relative;
    display: inline-block;
    width: 44px;
    height: 24px;
    cursor: pointer;
  }

  .switch-toggle input {
    opacity: 0;
    width: 0;
    height: 0;
  }

  .switch-slider {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background-color: rgba(255, 255, 255, 0.15);
    border: 1px solid rgba(255, 255, 255, 0.2);
    border-radius: 24px;
    transition: all 0.2s ease;
  }

  .switch-slider:before {
    position: absolute;
    content: "";
    height: 18px;
    width: 18px;
    left: 2px;
    bottom: 2px;
    background-color: #cbd5e1;
    border-radius: 50%;
    transition: all 0.2s ease;
  }

  .switch-toggle input:checked + .switch-slider {
    background-color: #0284c7;
    border-color: #38bdf8;
  }

  .switch-toggle input:checked + .switch-slider:before {
    transform: translateX(20px);
    background-color: #ffffff;
  }

  .toggle-status-text {
    font-size: 0.8rem;
    font-weight: 600;
    color: #64748b;
  }

  .toggle-status-text.status-enabled {
    color: #38bdf8;
  }

  .unattended-body-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 16px;
  }

  @media (max-width: 700px) {
    .unattended-body-grid {
      grid-template-columns: 1fr;
    }
  }

  .unattended-box {
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 10px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .box-label {
    font-size: 0.72rem;
    font-weight: 700;
    color: #94a3b8;
    letter-spacing: 0.5px;
    text-transform: uppercase;
  }

  .dropdown-wrapper {
    position: relative;
    width: 100%;
  }

  .profile-select-control {
    width: 100%;
    background: rgba(15, 23, 42, 0.9);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    color: #f1f5f9;
    padding: 9px 12px;
    font-size: 0.84rem;
    outline: none;
    cursor: pointer;
    transition: border-color 0.15s ease;
  }

  .profile-select-control:focus {
    border-color: #38bdf8;
  }

  .box-hint {
    margin: 0;
    font-size: 0.76rem;
    color: #94a3b8;
    line-height: 1.4;
  }

  .crypto-hint {
    color: #64748b;
    font-size: 0.72rem;
  }

  .pwd-status-indicator {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.82rem;
    font-weight: 600;
  }

  .pwd-indicator-dot.green {
    color: #22c55e;
  }

  .pwd-indicator-dot.gray {
    color: #64748b;
  }

  .pwd-indicator-label {
    color: #e2e8f0;
  }

  .pwd-action-buttons {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .btn-set-password {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    background: #0284c7;
    border: none;
    color: #ffffff;
    font-size: 0.8rem;
    font-weight: 600;
    padding: 7px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.15s ease;
  }

  .btn-set-password:hover {
    background: #0369a1;
  }

  .btn-remove-password {
    background: transparent;
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #f87171;
    font-size: 0.78rem;
    font-weight: 600;
    padding: 6px 12px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-remove-password:hover {
    background: rgba(239, 68, 68, 0.15);
    border-color: #ef4444;
  }

  /* Host Partner Disconnect button */
  .btn-disconnect-host-partner {
    margin-left: auto;
    background: rgba(239, 68, 68, 0.15);
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #fca5a5;
    font-size: 0.78rem;
    font-weight: 600;
    padding: 6px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-disconnect-host-partner:hover {
    background: rgba(239, 68, 68, 0.3);
    color: #ffffff;
  }

  /* Password Modals */
  .password-modal-card {
    max-width: 460px;
    position: relative;
    overflow: visible;
  }

  .auth-badge-icon {
    background: rgba(56, 189, 248, 0.15);
    color: #38bdf8;
  }

  .modal-form-body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    margin: 16px 0;
    position: relative;
    overflow: visible;
  }

  .input-field-group {
    display: flex;
    flex-direction: column;
    gap: 6px;
    position: relative;
    overflow: visible;
  }

  .input-field-group label {
    font-size: 0.76rem;
    font-weight: 600;
    color: #94a3b8;
  }

  .password-input-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }

  .modal-input {
    width: 100%;
    background: rgba(15, 23, 42, 0.8);
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 8px;
    color: #ffffff;
    font-size: 0.88rem;
    padding: 10px 38px 10px 12px;
    outline: none;
    box-sizing: border-box;
    transition: border-color 0.15s ease;
  }

  .modal-input:focus {
    border-color: #38bdf8;
  }

  .profile-select-control option {
    background-color: #0f172a;
    color: #f1f5f9;
    padding: 8px 12px;
  }

  /* Custom Profile Dropdown in Set Password Modal */
  .profile-dropdown-group {
    position: relative;
    z-index: 50;
  }

  .dropdown-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 40;
    background: transparent;
    cursor: default;
  }

  .custom-dropdown-container {
    position: relative;
    width: 100%;
    z-index: 45;
  }

  .custom-dropdown-trigger {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: rgba(15, 23, 42, 0.95);
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-radius: 8px;
    padding: 10px 14px;
    color: #ffffff;
    cursor: pointer;
    transition: all 0.15s ease;
    box-sizing: border-box;
    font-size: 0.86rem;
  }

  .custom-dropdown-trigger:hover,
  .custom-dropdown-trigger.active {
    border-color: #38bdf8;
    background: rgba(15, 23, 42, 1);
    box-shadow: 0 0 12px rgba(56, 189, 248, 0.25);
  }

  .dropdown-trigger-left {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .profile-icon {
    font-size: 1.05rem;
    line-height: 1;
  }

  .profile-title {
    font-weight: 600;
    color: #f1f5f9;
    font-size: 0.86rem;
  }

  .chevron-icon {
    color: #94a3b8;
    transition: transform 0.2s ease;
    flex-shrink: 0;
  }

  .chevron-icon.rotated {
    transform: rotate(180deg);
    color: #38bdf8;
  }

  .custom-dropdown-menu {
    position: absolute;
    left: 0;
    right: 0;
    top: calc(100% + 6px);
    background: #1e293b;
    border: 1px solid rgba(56, 189, 248, 0.45);
    border-radius: 10px;
    padding: 6px;
    box-shadow: 0 16px 36px rgba(0, 0, 0, 0.75), 0 0 15px rgba(56, 189, 248, 0.15);
    z-index: 55;
    display: flex;
    flex-direction: column;
    gap: 4px;
    animation: fadeIn 0.12s ease-out;
  }

  .dropdown-option-item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 10px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    color: #f1f5f9;
    text-align: left;
    cursor: pointer;
    transition: all 0.15s ease;
    box-sizing: border-box;
  }

  .dropdown-option-item:hover {
    background: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.12);
  }

  .dropdown-option-item.selected {
    background: rgba(56, 189, 248, 0.16);
    border-color: rgba(56, 189, 248, 0.45);
  }

  .option-icon {
    font-size: 1.15rem;
    line-height: 1;
    flex-shrink: 0;
  }

  .option-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
  }

  .option-name {
    font-size: 0.85rem;
    font-weight: 700;
    color: #f8fafc;
  }

  .option-desc {
    font-size: 0.74rem;
    color: #94a3b8;
    line-height: 1.3;
  }

  .option-check {
    font-size: 0.88rem;
    font-weight: 700;
    color: #38bdf8;
    margin-left: 6px;
    flex-shrink: 0;
  }

  .btn-eye {
    position: absolute;
    right: 8px;
    background: transparent;
    border: none;
    color: #64748b;
    padding: 4px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.15s ease;
  }

  .btn-eye:hover {
    color: #e2e8f0;
  }

  .modal-error-alert {
    background: rgba(239, 68, 68, 0.15);
    border: 1px solid rgba(239, 68, 68, 0.35);
    color: #fca5a5;
    padding: 8px 12px;
    border-radius: 6px;
    font-size: 0.8rem;
    display: flex;
    align-items: center;
    gap: 8px;
  }
</style>
