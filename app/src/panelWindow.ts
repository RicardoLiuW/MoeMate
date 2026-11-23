import { emit, listen } from "@tauri-apps/api/event";
import {
  actionPlay,
  catalogGet,
  charSwitch,
  chatSend,
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
  type Catalog,
  type CharState,
  type ChatAiConfig,
  type ChatLogMessage,
  type PomodoroStatusPayload,
} from "./shared/ipc";
import { IntegratedPanel } from "./ui/integratedPanel";
import {
  createOfflineTelemetryState,
  type PanelTabId,
} from "./ui/telemetryIsland";
import {
  applyAppearance,
  applyCharTheme,
  loadSavedAppearance,
  type UiAppearance,
} from "./ui/themes";

const CHAR_STORAGE_KEY = "deskpet.lastCharId";
const APPEARANCE_STORAGE_KEY = "deskpet.appearance";

export async function bootPanelWindow(): Promise<void> {
  document.documentElement.classList.add("is-panel-window");
  document.getElementById("stage-canvas")?.remove();

  const root = document.getElementById("app");
  if (!root) {
    throw new Error("missing #app");
  }

  let uiLayer = document.getElementById("ui-layer") as HTMLDivElement | null;
  if (!uiLayer) {
    uiLayer = document.createElement("div");
    uiLayer.id = "ui-layer";
    root.appendChild(uiLayer);
  }
  uiLayer.className = "ui-layer";

  let currentAppearance: UiAppearance = applyAppearance(loadSavedAppearance());
  try {
    const savedCharId = localStorage.getItem(CHAR_STORAGE_KEY);
    applyCharTheme(savedCharId || "claude");
  } catch {
    applyCharTheme("claude");
  }

  const panel = new IntegratedPanel(
    uiLayer,
    {
      onOpenChange: (open) => {
        if (!open) {
          void settingsSet("panelOpen", { open: false }).catch(() => {});
        }
      },
      onTabChange: (tab) => {
        void settingsSet("panelTab", tab).catch(() => {});
      },
      onRefreshTelemetry: async () => {
        try {
          const next = await telemetryRefresh();
          panel.setTelemetry(next);
        } catch {
          // ignore
        }
      },
      onSelectSource: async (source) => {
        try {
          const next = await telemetrySetSource(source);
          panel.setTelemetry(next);
        } catch {
          // ignore
        }
      },
      onCopyContinuation: (message) => {
        void emit("bubble", { text: message, ttlMs: 3600 }).catch(() => {});
      },
      onSelectChar: async (charId) => {
        applyCharTheme(charId);
        try {
          localStorage.setItem(CHAR_STORAGE_KEY, charId);
        } catch {
          // ignore
        }
        try {
          const charState = await charSwitch(charId);
          panel.setCharacterClips(charState);
          panel.setActiveCharacter(
            charState.charId,
            charState.name,
            charState.clips.length,
          );
        } catch {
          // ignore
        }
      },
      onFeed: () => {
        void petInteract("feed", 0, 0)
          .then((state) => panel.setStats(state.stats))
          .catch(() => {});
      },
      onPlayClip: (clipId) => {
        void actionPlay(clipId).catch(() => {});
      },
      onChatSend: async (text) => {
        await chatSend(text);
      },
      onToggleAppearance: () => {
        currentAppearance = applyAppearance(
          currentAppearance === "dark" ? "light" : "dark",
        );
        panel.setAppearance(currentAppearance);
        void settingsSet("appearance", currentAppearance).catch(() => {});
      },
    },
    { standalone: true },
  );
  panel.setAppearance(currentAppearance);
  panel.setTelemetry(createOfflineTelemetryState());

  document.addEventListener("contextmenu", (e) => {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
      return;
    }
    e.preventDefault();
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      panel.setWindowVisible(false);
      void settingsSet("panelOpen", { open: false }).catch(() => {});
    }
  });

  window.addEventListener("storage", (e) => {
    if (
      e.key === APPEARANCE_STORAGE_KEY &&
      (e.newValue === "light" || e.newValue === "dark")
    ) {
      currentAppearance = applyAppearance(e.newValue);
      panel.setAppearance(currentAppearance);
    }
  });

  await Promise.all([
    onStats((stats) => panel.setStats(stats)),
    onTelemetry((telemetry) => panel.setTelemetry(telemetry)),
    onPlay((payload) => panel.setPlayingClipId(payload.clipId)),
    listen<CharState>("pet_char_switched", (event) => {
      const cs = event.payload;
      applyCharTheme(cs.charId);
      panel.setCharacterClips(cs);
      panel.setActiveCharacter(cs.charId, cs.name, cs.clips.length);
    }),
    listen<string | null>("panel_clip_sync", (event) => {
      panel.setPlayingClipId(event.payload ?? null);
    }),
    listen<{ open: boolean; tab: PanelTabId }>("panel_state", (event) => {
      const { open, tab } = event.payload;
      if (tab) {
        panel.switchTab(tab);
      }
      panel.setWindowVisible(Boolean(open));
    }),
    listen<UiAppearance>("pet_appearance", (event) => {
      const mode = event.payload === "dark" ? "dark" : "light";
      currentAppearance = applyAppearance(mode);
      panel.setAppearance(currentAppearance);
    }),
    listen<ChatAiConfig>("pet_chat_config", (event) => {
      panel.setChatConfig(event.payload);
    }),
    listen<ChatLogMessage>("pet_chat_message", (event) => {
      panel.appendChatMessage(event.payload);
    }),
    listen<boolean>("pet_chat_cleared", () => {
      panel.clearChatMessages();
    }),
    listen<boolean>("pet_open_chat_settings", () => {
      panel.openChatSettingsDrawer();
    }),
    listen<PomodoroStatusPayload>("pet_pomodoro", (event) => {
      panel.setPomodoro(event.payload);
    }),
    listen<boolean>("pet_focus_chat_input", () => {
      panel.focusChatInput();
    }),
    listen<string>("pet_assets_root", (event) => {
      if (event.payload) setAssetsRoot(event.payload);
    }),
    listen<Catalog>("pet_catalog_updated", (event) => {
      panel.setCatalog(event.payload.chars ?? []);
    }),
  ]);

  // 隐藏工作台延后 600ms 拉取初始数据，避免启动瞬间与主舞台争抢 IPC 与磁盘带宽
  await new Promise((resolve) => window.setTimeout(resolve, 600));
  const [catalog, petState, initialTelemetry] = await Promise.all([
    catalogGet(),
    petStateGet(),
    telemetryGet().catch(() => createOfflineTelemetryState()),
  ]);
  panel.setCatalog(catalog.chars);
  panel.setStats(petState.stats);
  panel.setTelemetry(initialTelemetry);

  await settingsSet("panelReady", true).catch(() => {});
}
