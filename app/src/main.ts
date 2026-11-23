import "./styles.css";
import { LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { bootPanelWindow } from "./panelWindow";
import { bootSettingsWindow } from "./settingsWindow";
import {
  actionPlay,
  catalogGet,
  charSwitch,
  chatSend,
  onBubble,
  onPlay,
  onStats,
  onTelemetry,
  petInteract,
  petStateGet,
  setAssetsRoot,
  settingsSet,
  telemetryGet,
  telemetryRefresh,
  telemetrySetSource,
  type AssetsStatusPayload,
  type Catalog,
  type CharState,
  type ChatAiConfig,
  type ChatLogMessage,
  type PetState,
  type PetTtsPayload,
  type PomodoroStatusPayload,
  type TelemetryStatus,
  type UnifiedState,
} from "./shared/ipc";
import { StageInteraction } from "./stage/interaction";
import { StagePlayer } from "./stage/player";
import { BubbleOverlay } from "./ui/bubble";
import { ContextMenu, type ScalePreset } from "./ui/contextMenu";
import { IntegratedPanel } from "./ui/integratedPanel";
import {
  TelemetryIsland,
  createOfflineTelemetryState,
  type EdgeDockState,
  type PanelTabId,
} from "./ui/telemetryIsland";
import {
  applyAppearance,
  applyCharTheme,
  loadSavedAppearance,
  type UiAppearance,
} from "./ui/themes";

const BASE_WIDTH = 1280;
const BASE_HEIGHT = 720;
const TOP_CROWN_HEIGHT = 90;
const CHAR_STORAGE_KEY = "deskpet.lastCharId";

async function bootMainStage(): Promise<void> {
  const isTauri = "__TAURI_INTERNALS__" in window;

  type OverlayKey = "contextMenu" | "panel";
  const activeOverlays = new Set<OverlayKey>();
  let lastUiInteractive: boolean | null = null;

  function setOverlayLock(key: OverlayKey, open: boolean): void {
    if (open) {
      activeOverlays.add(key);
    } else {
      activeOverlays.delete(key);
    }
    const nextInteractive = activeOverlays.size > 0;
    if (nextInteractive !== lastUiInteractive) {
      lastUiInteractive = nextInteractive;
      settingsSet("uiInteractive", nextInteractive).catch(() => {
        // non-Tauri preview mode
      });
    }
  }

  let currentCatalog: Catalog | null = null;
  let currentChar: CharState | null = null;
  let currentAppearance: UiAppearance = applyAppearance(loadSavedAppearance());
  let currentTelemetryStatus: TelemetryStatus = "offline";
  let autoStartEnabled = false;
  let currentChatConfig: ChatAiConfig = {
    provider: "deepseek",
    baseUrl: "https://api.deepseek.com/v1",
    apiKey: "",
    model: "deepseek-chat",
    ttsEnabled: true,
    autoPlayVoice: true,
    ttsMode: "azure",
    azureSpeechKey: "",
    azureSpeechRegion: "eastasia",
    customVoice: "",
    charVoices: {},
    customPersonas: {},
  };
  let externalPanelOpen = false;
  let edgeDockState: EdgeDockState = {
    enabled: true,
    side: null,
    visibleLeftPx: 0,
    visibleRightPx: window.innerWidth || 640,
  };
  let currentPetState: PetState = {
    stats: { satiety: 80, energy: 80, mood: 80, affinity: 10 },
    pos: { x: 0, y: 0 },
    scale: 0.5,
    pinned: true,
    muted: false,
  };
  let lastPlayAtMs = 0;
  let lastFeedFxAtMs = 0;

  const root = document.getElementById("app");
  if (!root) {
    throw new Error("missing #app");
  }

  let stageCanvas = document.getElementById("stage-canvas") as HTMLCanvasElement | null;
  if (!stageCanvas) {
    stageCanvas = document.createElement("canvas");
    stageCanvas.id = "stage-canvas";
    root.appendChild(stageCanvas);
  }
  stageCanvas.className = "pet-stage-canvas";

  let uiLayer = document.getElementById("ui-layer") as HTMLDivElement | null;
  if (!uiLayer) {
    uiLayer = document.createElement("div");
    uiLayer.id = "ui-layer";
    root.appendChild(uiLayer);
  }
  uiLayer.className = "ui-layer";

  const player = new StagePlayer(stageCanvas, {
    onActiveClipChange: (clipId) => {
      previewPanel?.setPlayingClipId(clipId);
      bubbleOverlay.reposition();
    },
    onVoiceStateChange: (speaking) => {
      bubbleOverlay.setSpeaking(speaking);
    },
  });

  const bubbleOverlay = new BubbleOverlay(
    uiLayer,
    () => player.getBubbleAnchor(),
    () => island.isVisible(),
  );

  const island = new TelemetryIsland(uiLayer, {
    onToggleMain: () => {
      contextMenu.close();
      if (isTauri) {
        void settingsSet("panelOpen", { toggle: true, tab: "status" }).catch(() => {});
      } else {
        previewPanel?.toggle("status");
      }
    },
    onOpenTab: (tab: PanelTabId) => {
      contextMenu.close();
      if (isTauri) {
        void settingsSet("panelOpen", { toggle: true, tab }).catch(() => {});
      } else {
        previewPanel?.toggle(tab);
      }
    },
    onOpenResources: () => {
      contextMenu.close();
      if (isTauri) {
        void settingsSet("settingsOpen", { open: true, tab: "resources" }).catch(() => {});
      }
    },
    onRefresh: async () => {
      try {
        const next = await telemetryRefresh();
        applyTelemetryState(next);
      } catch {
        // non-Tauri preview fallback
      }
    },
    onCopyContinuation: (message) => {
      bubbleOverlay.show({ text: message, ttlMs: 3600 });
    },
    onModeChange: () => {
      syncMenuState();
    },
    onContextMenu: (x, y) => {
      openPetContextMenu(x, y);
    },
  });

  const previewPanel = isTauri
    ? null
    : new IntegratedPanel(uiLayer, {
        onOpenChange: (open) => {
          setOverlayLock("panel", open);
          island.setPanelOpen(open);
        },
        onRefreshTelemetry: async () => {
          try {
            const next = await telemetryRefresh();
            applyTelemetryState(next);
          } catch {
            // ignore
          }
        },
        onSelectSource: async (source) => {
          try {
            const next = await telemetrySetSource(source);
            applyTelemetryState(next);
          } catch {
            // ignore
          }
        },
        onCopyContinuation: (message) => {
          bubbleOverlay.show({ text: message, ttlMs: 3600 });
        },
        onSelectChar: async (charId) => {
          await switchCharacter(charId);
        },
        onFeed: () => {
          void handleFeed();
        },
        onPlayClip: (clipId) => {
          void triggerClipPlay(clipId);
        },
        onChatSend: async (text) => {
          await handleChatSend(text);
        },
        onToggleAppearance: () => {
          setAppearance(currentAppearance === "dark" ? "light" : "dark");
        },
      });
  previewPanel?.setAppearance(currentAppearance);

  const contextMenu = new ContextMenu(uiLayer, {
    onFeed: () => {
      void handleFeed();
    },
    onOpenTab: (tab) => {
      if (isTauri) {
        void settingsSet("panelOpen", { open: true, tab }).catch(() => {});
      } else {
        previewPanel?.open(tab);
      }
    },
    onOpenChatSettings: () => {
      if (isTauri) {
        void settingsSet("settingsOpen", { open: true }).catch(() => {});
      } else {
        previewPanel?.open("chat");
      }
    },
    onOpenResources: () => {
      if (isTauri) {
        void settingsSet("settingsOpen", { open: true, tab: "resources" }).catch(() => {});
      }
    },
    onSetScale: (scale) => {
      void handleSetScale(scale);
    },
    onTogglePinned: () => {
      void handleTogglePinned();
    },
    onToggleMuted: () => {
      void handleToggleMuted();
    },
    onToggleEdgeDocking: () => {
      void handleToggleEdgeDocking();
    },
    onToggleAutoStart: () => {
      void handleToggleAutoStart();
    },
    onSetIslandVisibility: (visible) => {
      island.setVisible(visible);
      bubbleOverlay.reposition();
      syncMenuState();
    },
    onSetIslandMode: (mode) => {
      island.setMode(mode);
      syncMenuState();
    },
    onSetAppearance: (appearance) => {
      setAppearance(appearance);
    },
    onOpenChange: (open) => {
      setOverlayLock("contextMenu", open);
    },
  });

  function setAppearance(mode: UiAppearance): void {
    currentAppearance = applyAppearance(mode);
    previewPanel?.setAppearance(currentAppearance);
    syncMenuState();
    settingsSet("appearance", currentAppearance).catch(() => {});
  }

  function applyEdgeDockState(dock: EdgeDockState): void {
    edgeDockState = dock;
    island.setEdgeDock(dock);
    bubbleOverlay.setEdgeDock(dock);
    player.setEdgeDock(dock.side);
    syncMenuState();
  }

  function openPetContextMenu(x: number, y: number): void {
    if (isTauri && externalPanelOpen) {
      void settingsSet("panelOpen", { open: false }).catch(() => {});
    }
    previewPanel?.close();
    const visBounds = edgeDockState.side
      ? { left: edgeDockState.visibleLeftPx, right: edgeDockState.visibleRightPx }
      : undefined;
    contextMenu.openAt(
      x,
      y,
      {
        scale: currentPetState.scale,
        pinned: currentPetState.pinned,
        muted: currentPetState.muted,
        edgeDocking: edgeDockState.enabled,
        autoStart: autoStartEnabled,
        islandVisible: island.isVisible(),
        islandMode: island.getMode(),
        charName: currentChar?.name ?? "MoeMate",
        appearance: currentAppearance,
        telemetryStatus: currentTelemetryStatus,
        aiConfigured: Boolean(currentChatConfig.apiKey?.trim()),
        ttsEnabled: currentChatConfig.ttsEnabled,
      },
      visBounds,
    );
  }

  function applyTelemetryState(next: UnifiedState): void {
    currentTelemetryStatus = next.status;
    island.setState(next);
    previewPanel?.setTelemetry(next);
    player.setTelemetryOverlay(next);
    syncMenuState();

    if (next.status === "streaming") {
      const tps = next.speed.tokensPerSec;
      const rate =
        tps !== null && tps > 0
          ? 1 + Math.min(0.18, (tps / 100) * 0.18)
          : 1.06;
      player.setPlaybackSpeed(rate);
    } else {
      player.setPlaybackSpeed(1.0);
    }
  }

  applyTelemetryState(createOfflineTelemetryState());

  function closeAllOverlays(): void {
    contextMenu.close();
    previewPanel?.close();
    if (isTauri && externalPanelOpen) {
      void settingsSet("panelOpen", { open: false }).catch(() => {});
    }
  }

  function syncMenuState(): void {
    contextMenu.syncState({
      scale: currentPetState.scale,
      pinned: currentPetState.pinned,
      muted: currentPetState.muted,
      edgeDocking: edgeDockState.enabled,
      autoStart: autoStartEnabled,
      islandVisible: island.isVisible(),
      islandMode: island.getMode(),
      charName: currentChar?.name ?? "MoeMate",
      appearance: currentAppearance,
      telemetryStatus: currentTelemetryStatus,
      aiConfigured: Boolean(currentChatConfig.apiKey?.trim()),
      ttsEnabled: currentChatConfig.ttsEnabled,
    });
  }

  function syncPetState(next: PetState): void {
    currentPetState = next;
    player.setMuted(next.muted);
    previewPanel?.setStats(next.stats);
    syncMenuState();
  }

  function applyCharStateLocal(charState: CharState): void {
    const theme = applyCharTheme(charState.charId);
    player.setAccentRgb(theme.accentRgb);
    currentChar = charState;
    try {
      localStorage.setItem(CHAR_STORAGE_KEY, charState.charId);
    } catch {
      // ignore storage restriction
    }
    player.setCharacter(charState);
    bubbleOverlay.setCharacter(charState.name);
    previewPanel?.setCharacterClips(charState);
    previewPanel?.setPlayingClipId(player.getCurrentClipId());
    previewPanel?.setActiveCharacter(
      charState.charId,
      charState.name,
      charState.clips.length,
    );
    syncMenuState();
  }

  async function triggerClipPlay(clipId: string): Promise<void> {
    const before = lastPlayAtMs;
    try {
      await actionPlay(clipId);
    } catch {
      // 非 Tauri 环境回退本地播放
    }
    window.setTimeout(() => {
      if (lastPlayAtMs === before && currentChar) {
        const meta = currentChar.clips.find((c) => c.id === clipId);
        void player.play({
          charId: currentChar.charId,
          clipId,
          startAtMs: meta?.safeStartMs ?? 0,
          mirror: false,
          audio: meta?.audio ?? null,
        });
      }
    }, 90);
  }

  function pickFallbackIdleClip(charState: CharState): string | null {
    const validSet = new Set(
      charState.clips
        .filter((c) => c.qa?.status !== "fail")
        .map((c) => c.id),
    );
    for (const id of charState.bindings.idle ?? []) {
      if (validSet.has(id)) return id;
    }
    const firstValid = charState.clips.find((c) => c.qa?.status !== "fail");
    return firstValid?.id ?? charState.clips[0]?.id ?? null;
  }

  async function switchCharacter(charId: string): Promise<void> {
    const theme = applyCharTheme(charId);
    player.setAccentRgb(theme.accentRgb);

    const beforePlay = lastPlayAtMs;
    const charState = await charSwitch(charId);
    applyCharStateLocal(charState);

    window.setTimeout(() => {
      if (lastPlayAtMs === beforePlay && currentChar?.charId === charState.charId) {
        const fallbackId = pickFallbackIdleClip(charState);
        if (fallbackId) {
          void triggerClipPlay(fallbackId);
        }
      }
    }, 60);
  }

  async function handleFeed(): Promise<void> {
    lastFeedFxAtMs = performance.now();
    player.spawnFeedHearts();
    try {
      const next = await petInteract("feed", 0, 0);
      syncPetState(next);
    } catch {
      // ignore in browser preview
    }
  }

  async function handleChatSend(text: string): Promise<void> {
    const beforePlay = lastPlayAtMs;
    try {
      const reply = await chatSend(text);
      if (reply.bubble) {
        bubbleOverlay.show({ text: reply.bubble, ttlMs: 4000 });
      }
      if (reply.clipId) {
        window.setTimeout(() => {
          if (lastPlayAtMs === beforePlay) {
            void triggerClipPlay(reply.clipId);
          }
        }, 100);
      }
    } catch {
      bubbleOverlay.show({ text: `「${text}」收到啦～`, ttlMs: 3600 });
    }
  }

  async function handleSetScale(scale: ScalePreset): Promise<void> {
    currentPetState = { ...currentPetState, scale };
    syncMenuState();
    settingsSet("scale", scale).catch(() => {});

    const targetW = Math.round(BASE_WIDTH * scale);
    const targetH = Math.round(BASE_HEIGHT * scale + TOP_CROWN_HEIGHT);
    try {
      await getCurrentWindow().setSize(new LogicalSize(targetW, targetH));
    } catch {
      // 由后端 settings_set 处理或无权限时忽略
    }
    bubbleOverlay.reposition();
    island.reportHitRects(true);
  }

  async function handleTogglePinned(): Promise<void> {
    const next = !currentPetState.pinned;
    currentPetState = { ...currentPetState, pinned: next };
    syncMenuState();
    settingsSet("pinned", next).catch(() => {});
    try {
      await getCurrentWindow().setAlwaysOnTop(next);
    } catch {
      // ignore if backend handles it
    }
  }

  async function handleToggleMuted(): Promise<void> {
    const next = !currentPetState.muted;
    currentPetState = { ...currentPetState, muted: next };
    player.setMuted(next);
    syncMenuState();
    settingsSet("muted", next).catch(() => {});
  }

  async function handleToggleEdgeDocking(): Promise<void> {
    const next = !edgeDockState.enabled;
    edgeDockState = { ...edgeDockState, enabled: next };
    syncMenuState();
    settingsSet("edgeDocking", next).catch(() => {});
  }

  async function handleToggleAutoStart(): Promise<void> {
    const next = !autoStartEnabled;
    autoStartEnabled = next;
    syncMenuState();
    try {
      await settingsSet("autoStart", next);
      bubbleOverlay.show({
        text: next ? "已开启 Windows 开机自启" : "已关闭开机自启",
        ttlMs: 2600,
      });
    } catch {
      autoStartEnabled = !next;
      syncMenuState();
    }
  }

  new StageInteraction(stageCanvas, {
    player,
    hasOpenOverlay: () => activeOverlays.size > 0,
    onDismissOverlays: () => closeAllOverlays(),
    onContextMenu: (x, y) => {
      openPetContextMenu(x, y);
    },
    onStateSync: (next) => {
      syncPetState(next);
    },
  });

  stageCanvas.addEventListener("pointerenter", () => player.setEdgeHover(true));
  stageCanvas.addEventListener("pointerleave", () => player.setEdgeHover(false));

  document.addEventListener("contextmenu", (e) => {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
      return;
    }
    e.preventDefault();
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && (activeOverlays.size > 0 || externalPanelOpen)) {
      e.preventDefault();
      closeAllOverlays();
    }
  });

  let resolveAssetsRootReady: (() => void) | null = null;
  const assetsRootReady = new Promise<void>((resolve) => {
    resolveAssetsRootReady = resolve;
    window.setTimeout(resolve, 250);
  });

  await Promise.all([
    onPlay((payload) => {
      if (currentChar && (!payload.charId || payload.charId === currentChar.charId)) {
        lastPlayAtMs = performance.now();
      }
      void player.play(payload);
    }),
    onBubble((payload) => {
      bubbleOverlay.show(payload);
    }),
    onStats((stats) => {
      currentPetState = { ...currentPetState, stats };
      previewPanel?.setStats(stats);
    }),
    onTelemetry((telemetry) => {
      applyTelemetryState(telemetry);
    }).catch(() => {}),
    listen<boolean>("pet_muted", (event) => {
      const muted = Boolean(event.payload);
      currentPetState = { ...currentPetState, muted };
      player.setMuted(muted);
      syncMenuState();
    }).catch(() => {}),
    listen<boolean>("pet_auto_start", (event) => {
      autoStartEnabled = Boolean(event.payload);
      syncMenuState();
    }).catch(() => {}),
    listen<EdgeDockState>("pet_edge_dock", (event) => {
      applyEdgeDockState(event.payload);
    }).catch(() => {}),
    listen<{ open: boolean; tab: PanelTabId }>("panel_state", (event) => {
      externalPanelOpen = Boolean(event.payload.open);
      island.setPanelOpen(externalPanelOpen);
    }).catch(() => {}),
    listen<CharState>("pet_char_switched", (event) => {
      const cs = event.payload;
      if (currentChar?.charId !== cs.charId) {
        applyCharStateLocal(cs);
      }
    }).catch(() => {}),
    listen<UiAppearance>("pet_appearance", (event) => {
      const mode = event.payload === "dark" ? "dark" : "light";
      currentAppearance = applyAppearance(mode);
      previewPanel?.setAppearance(currentAppearance);
      syncMenuState();
    }).catch(() => {}),
    listen<boolean>("pet_feed_fx", () => {
      if (performance.now() - lastFeedFxAtMs > 300) {
        player.spawnFeedHearts();
      }
    }).catch(() => {}),
    listen<PetTtsPayload>("pet_tts", (event) => {
      player.playVoice(event.payload);
    }).catch(() => {}),
    listen<ChatAiConfig>("pet_chat_config", (event) => {
      currentChatConfig = event.payload;
      previewPanel?.setChatConfig(event.payload);
      syncMenuState();
    }).catch(() => {}),
    listen<ChatLogMessage>("pet_chat_message", (event) => {
      previewPanel?.appendChatMessage(event.payload);
    }).catch(() => {}),
    listen<boolean>("pet_chat_cleared", () => {
      previewPanel?.clearChatMessages();
    }).catch(() => {}),
    listen<boolean>("pet_open_chat_settings", () => {
      previewPanel?.open("chat");
      previewPanel?.openChatSettingsDrawer();
    }).catch(() => {}),
    listen<PomodoroStatusPayload>("pet_pomodoro", (event) => {
      island.setPomodoro(event.payload);
      previewPanel?.setPomodoro(event.payload);
    }).catch(() => {}),
    listen<boolean>("pet_focus_chat_input", () => {
      previewPanel?.focusChatInput();
    }).catch(() => {}),
    listen<string>("pet_assets_root", (event) => {
      if (event.payload) {
        setAssetsRoot(event.payload);
        resolveAssetsRootReady?.();
      }
    }).catch(() => {}),
    listen<AssetsStatusPayload>("pet_assets_status", (event) => {
      if (event.payload?.assetsRoot) {
        setAssetsRoot(event.payload.assetsRoot);
        resolveAssetsRootReady?.();
      }
    }).catch(() => {}),
    listen<Catalog>("pet_catalog_updated", (event) => {
      const nextCat = event.payload;
      currentCatalog = nextCat;
      previewPanel?.setCatalog(nextCat.chars ?? []);
      const usable = (nextCat.chars ?? []).filter((c) => c.clips > 0);
      island.setMissingAssets(usable.length === 0);
      if (usable.length > 0 && (!currentChar || !usable.some((c) => c.id === currentChar?.charId))) {
        void switchCharacter(usable[0].id).catch(() => {});
      }
    }).catch(() => {}),
  ]);

  settingsSet("uiInteractive", false).catch(() => {});
  island.reportHitRects(true);

  if (isTauri) {
    void settingsSet("mainReady", true).catch(() => {});
    await assetsRootReady;
  }

  let savedCharId: string | null = null;
  try {
    savedCharId = localStorage.getItem(CHAR_STORAGE_KEY);
  } catch {
    savedCharId = null;
  }

  const [catalog, petState] = await Promise.all([
    catalogGet(),
    petStateGet(),
  ]);

  telemetryGet()
    .then((initialTelemetry) => applyTelemetryState(initialTelemetry))
    .catch(() => {});

  currentCatalog = catalog;
  previewPanel?.setCatalog(catalog.chars);
  syncPetState(petState);

  const usableChars = catalog.chars.filter((c) => c.clips > 0);
  if (usableChars.length === 0) {
    island.setMissingAssets(true);
    bubbleOverlay.show({
      text: "欢迎使用 MoeMate！本机暂无角色动画，点击上方按钮或右键菜单即可按需下载～",
      ttlMs: 9000,
    });
  } else {
    island.setMissingAssets(false);
    const hasSaved =
      savedCharId && usableChars.some((c) => c.id === savedCharId);
    const hasClaude = usableChars.some((c) => c.id === "claude");
    const defaultCharId = hasSaved
      ? savedCharId!
      : hasClaude
        ? "claude"
        : usableChars[0].id;

    if (defaultCharId && (currentChar as CharState | null)?.charId !== defaultCharId) {
      await switchCharacter(defaultCharId).catch(() => null);
    }
  }

  void currentCatalog;
}

const viewParam = new URLSearchParams(window.location.search).get("view");
if (viewParam === "panel") {
  void bootPanelWindow().catch((err) => {
    console.error("[deskpet] 独立工作台启动失败:", err);
  });
} else if (viewParam === "settings") {
  void bootSettingsWindow().catch((err) => {
    console.error("[deskpet] 独立设置窗口启动失败:", err);
  });
} else {
  void bootMainStage().catch((err) => {
    console.error("[deskpet] 启动初始化失败:", err);
  });
}
