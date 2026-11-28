import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  settingsSet,
  type AssetsProgressPayload,
  type AssetsStatusPayload,
  type BuiltinVoiceLang,
  type CharState,
  type ChatAiConfig,
  type CompanionVoiceMode,
  type FishSpeechCharProfile,
  type FishSpeechConfig,
  type FishSpeechProgressPayload,
  type FishSpeechStatusPayload,
  type VramMode,
  type VramStatusPayload,
} from "./shared/ipc";
import { createPixelAvatar } from "./ui/avatars";
import { createIcon, type IconName } from "./ui/icons";
import {
  AI_PROVIDER_PRESETS,
  CHAR_DEFAULT_PERSONAS,
  CHAR_ORDER,
  FEMALE_VOICE_OPTIONS,
  getCharMeta,
} from "./ui/personas";
import {
  applyAppearance,
  applyCharTheme,
  loadSavedAppearance,
  type UiAppearance,
} from "./ui/themes";

type SettingsTabId = "ai" | "voice" | "resources" | "persona" | "system";

const TAB_META: ReadonlyArray<{
  id: SettingsTabId;
  label: string;
  icon: IconName;
  title: string;
  sub: string;
}> = [
  {
    id: "ai",
    label: "AI 对话模型",
    icon: "sparkles",
    title: "AI 对话模型",
    sub: "OpenAI 兼容接口（含 DeepSeek / Gemini / 千问 / 自定义端点）",
  },
  {
    id: "voice",
    label: "语音与声优克隆",
    icon: "volumeOn",
    title: "语音与声优克隆",
    sub: "Fish Speech v2ProPlus（NVIDIA / AMD 分流）· Azure 女声 · 本地语音包",
  },
  {
    id: "resources",
    label: "角色与语音下载",
    icon: "download",
    title: "角色与语音资源",
    sub: "GitHub 同仓库 Releases 按需下载角色动画与日语语音素材",
  },
  {
    id: "persona",
    label: "角色性格定制",
    icon: "heart",
    title: "角色性格定制",
    sub: "自定义 8 位编程搭子的口癖、称呼与傲娇度",
  },
  {
    id: "system",
    label: "系统与开发者",
    icon: "terminal",
    title: "系统与开发者",
    sub: "开机自启 · Alt+Space 全局热键 · 本地 HTTP Hook 联动",
  },
];

function createDefaultFishSpeechConfig(): FishSpeechConfig {
  return {
    enabled: false,
    installDir: "",
    apiUrl: "http://127.0.0.1:8080",
    apiKey: "",
    checkpointDir: "",
    decoderCheckpoint: "",
    decoderConfig: "modded_dac_vq",
    device: "auto",
    half: true,
    timeoutSecs: 45,
    chunkLength: 200,
    topP: 0.8,
    temperature: 0.8,
    repetitionPenalty: 1.1,
    maxNewTokens: 1024,
    useMemoryCache: "on",
    downloadSource: "mirror",
    gpuBackend: "nvidia",
    autoStartService: false,
    autoStopOnExit: true,
    dailyDynamicVoice: true,
    charProfiles: {},
  };
}

function createDefaultCharProfile(): FishSpeechCharProfile {
  return {
    refAudioPath: "",
    promptText: "",
    promptLang: "zh",
    textLang: "ja",
    referenceId: "",
  };
}

function createDefaultConfig(): ChatAiConfig {
  return {
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
    fishSpeech: createDefaultFishSpeechConfig(),
    builtinVoiceLang: "ja",
    vramMode: "balanced",
    vramAutoUnloadMinutes: 5,
    ideVoiceEnabled: true,
    companionVoiceMode: "balanced",
    clickSfxEnabled: true,
  };
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function createToggleSwitchRow(
  title: string,
  desc: string,
  onToggle: () => void,
): { row: HTMLDivElement; setOn: (on: boolean) => void } {
  const row = document.createElement("div");
  row.className = "sw-switch-row";

  const info = document.createElement("div");
  info.className = "sw-switch-info";
  const tEl = document.createElement("span");
  tEl.className = "sw-switch-title";
  tEl.textContent = title;
  const dEl = document.createElement("span");
  dEl.className = "sw-switch-desc";
  dEl.textContent = desc;
  info.append(tEl, dEl);

  const sw = document.createElement("button");
  sw.type = "button";
  sw.className = "mj-switch";
  sw.dataset.on = "false";
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", "false");
  const thumb = document.createElement("span");
  thumb.className = "mj-switch-thumb";
  sw.appendChild(thumb);
  sw.addEventListener("click", onToggle);

  row.append(info, sw);
  return {
    row,
    setOn(on: boolean) {
      sw.dataset.on = on ? "true" : "false";
      sw.setAttribute("aria-checked", on ? "true" : "false");
    },
  };
}

export async function bootSettingsWindow(): Promise<void> {
  document.documentElement.classList.add("is-settings-window");
  document.getElementById("stage-canvas")?.remove();

  const root = document.getElementById("app");
  if (!root) throw new Error("missing #app");

  let uiLayer = document.getElementById("ui-layer") as HTMLDivElement | null;
  if (!uiLayer) {
    uiLayer = document.createElement("div");
    uiLayer.id = "ui-layer";
    root.appendChild(uiLayer);
  }
  uiLayer.className = "ui-layer";

  let currentAppearance: UiAppearance = applyAppearance(loadSavedAppearance());
  try {
    applyCharTheme(localStorage.getItem("deskpet.lastCharId") || "claude");
  } catch {
    applyCharTheme("claude");
  }

  let config: ChatAiConfig = createDefaultConfig();
  let activeTab: SettingsTabId = "ai";
  let selectedCharId = "deepseek";
  let autoStartEnabled = false;
  let globalHotkeyEnabled = true;
  let assetsStatus: AssetsStatusPayload = {
    assetsRoot: "",
    githubRepo: "RicardoLiuW/MoeMate",
    releaseTag: "latest",
    downloadSource: "mirror",
    installing: false,
    voicePackArchive: "ja-voices-pack.zip",
    voicePackInstalled: false,
    chars: [],
  };
  let gsStatus: FishSpeechStatusPayload = {
    state: "uninstalled",
    label: "未安装",
    installed: false,
    pythonReady: false,
    modelReady: false,
    running: false,
    pid: null,
    apiUrl: "http://127.0.0.1:8080",
    installDir: "",
    checkpointDir: "",
    detectedCheckpoints: [],
    error: null,
  };
  let pickSeq = 0;
  const pendingPickCallbacks = new Map<string, (path: string) => void>();

  function requestNativePick(
    kind: "audio" | "ckpt" | "pth" | "dir",
    onPicked: (path: string) => void,
  ): void {
    const requestId = `pick_${++pickSeq}`;
    pendingPickCallbacks.set(requestId, onPicked);
    void settingsSet("fishSpeechPickFile", { requestId, kind }).catch(() => {
      pendingPickCallbacks.delete(requestId);
    });
  }

  function ensureGsConfig(): FishSpeechConfig {
    if (!config.fishSpeech) {
      config.fishSpeech = createDefaultFishSpeechConfig();
    }
    if (!config.fishSpeech.charProfiles) {
      config.fishSpeech.charProfiles = {};
    }
    return config.fishSpeech;
  }

  function getCharProfile(charId: string): FishSpeechCharProfile {
    const gs = ensureGsConfig();
    if (!gs.charProfiles[charId]) {
      gs.charProfiles[charId] = createDefaultCharProfile();
    }
    return gs.charProfiles[charId];
  }

  const shellEl = document.createElement("div");
  shellEl.className = "porcelain-card settings-shell";

  /* ==================== 左侧导航栏 ==================== */
  const sidebar = document.createElement("aside");
  sidebar.className = "sw-sidebar";

  const brand = document.createElement("div");
  brand.className = "sw-brand";
  brand.setAttribute("data-tauri-drag-region", "");
  const brandLeft = document.createElement("div");
  brandLeft.className = "sw-brand-left";
  brandLeft.setAttribute("data-tauri-drag-region", "");
  const dot = document.createElement("span");
  dot.className = "menu-char-dot";
  const brandTitle = document.createElement("span");
  brandTitle.className = "sw-brand-title";
  brandTitle.textContent = "MoeMate";
  brandTitle.setAttribute("data-tauri-drag-region", "");
  brandLeft.append(dot, brandTitle);
  const verBadge = document.createElement("span");
  verBadge.className = "sw-ver-badge";
  verBadge.textContent = "v3.0";
  brand.append(brandLeft, verBadge);

  const nav = document.createElement("nav");
  nav.className = "sw-nav";
  const navBtns = new Map<SettingsTabId, HTMLButtonElement>();
  const tabPages = new Map<SettingsTabId, HTMLDivElement>();

  for (const t of TAB_META) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sw-nav-btn";
    btn.dataset.active = t.id === activeTab ? "true" : "false";
    btn.append(createIcon(t.icon, 14));
    const lbl = document.createElement("span");
    lbl.textContent = t.label;
    btn.appendChild(lbl);
    btn.addEventListener("click", () => switchTab(t.id));
    nav.appendChild(btn);
    navBtns.set(t.id, btn);
  }

  const sidebarFoot = document.createElement("div");
  sidebarFoot.className = "sw-sidebar-foot";
  const themeBtn = document.createElement("button");
  themeBtn.type = "button";
  themeBtn.className = "sw-theme-toggle";
  const themeLbl = document.createElement("span");
  const themeIconWrap = document.createElement("span");
  themeBtn.append(themeLbl, themeIconWrap);
  themeBtn.addEventListener("click", () => {
    const next: UiAppearance = currentAppearance === "dark" ? "light" : "dark";
    currentAppearance = applyAppearance(next);
    syncThemeBtn();
    void settingsSet("appearance", currentAppearance).catch(() => {});
  });
  function syncThemeBtn(): void {
    themeLbl.textContent = currentAppearance === "dark" ? "星糖暗夜" : "奶油手账";
    themeIconWrap.replaceChildren(createIcon(currentAppearance === "dark" ? "moon" : "sun", 13));
  }
  syncThemeBtn();
  sidebarFoot.appendChild(themeBtn);
  sidebar.append(brand, nav, sidebarFoot);

  /* ==================== 右侧主区 ==================== */
  const main = document.createElement("div");
  main.className = "sw-main";

  const header = document.createElement("div");
  header.className = "sw-header";
  header.setAttribute("data-tauri-drag-region", "");
  const headerTitles = document.createElement("div");
  headerTitles.className = "sw-header-titles";
  headerTitles.setAttribute("data-tauri-drag-region", "");
  const pageTitleEl = document.createElement("span");
  pageTitleEl.className = "sw-page-title";
  pageTitleEl.setAttribute("data-tauri-drag-region", "");
  const pageSubEl = document.createElement("span");
  pageSubEl.className = "sw-page-sub";
  pageSubEl.setAttribute("data-tauri-drag-region", "");
  headerTitles.append(pageTitleEl, pageSubEl);

  const closeBtn = document.createElement("button");
  closeBtn.type = "button";
  closeBtn.className = "ip-circle-btn";
  closeBtn.title = "关闭 (Esc)";
  closeBtn.appendChild(createIcon("close", 13));
  closeBtn.addEventListener("pointerdown", (e) => e.stopPropagation());
  closeBtn.addEventListener("mousedown", (e) => e.stopPropagation());
  closeBtn.addEventListener("click", () => {
    void settingsSet("settingsOpen", { open: false }).catch(() => {
      void getCurrentWindow().hide().catch(() => {});
    });
  });
  header.append(headerTitles, closeBtn);

  const body = document.createElement("div");
  body.className = "sw-body";

  /* ===== 页签 1：AI 对话模型 ===== */
  const aiPage = document.createElement("div");
  aiPage.className = "sw-page";

  const presetCard = document.createElement("div");
  presetCard.className = "sw-card";
  const presetLbl = document.createElement("div");
  presetLbl.className = "sw-card-title";
  presetLbl.textContent = "模型厂商预设（OpenAI 兼容，含 Gemini）";
  const presetStrip = document.createElement("div");
  presetStrip.className = "ip-preset-strip";
  const presetBtns = new Map<string, HTMLButtonElement>();

  const baseUrlInput = document.createElement("input");
  baseUrlInput.type = "text";
  baseUrlInput.className = "ip-field-input";
  baseUrlInput.placeholder = "Base URL（如 https://generativelanguage.googleapis.com/v1beta/openai）";

  const modelInput = document.createElement("input");
  modelInput.type = "text";
  modelInput.className = "ip-field-input";
  modelInput.placeholder = "Model（如 gemini-3.8-flash / deepseek-chat）";

  for (const p of AI_PROVIDER_PRESETS) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ip-preset-chip";
    btn.textContent = p.label;
    btn.addEventListener("click", () => {
      config.provider = p.id;
      if (p.baseUrl) baseUrlInput.value = p.baseUrl;
      if (p.model) modelInput.value = p.model;
      syncUi();
    });
    presetStrip.appendChild(btn);
    presetBtns.set(p.id, btn);
  }
  presetCard.append(presetLbl, presetStrip);

  const apiCard = document.createElement("div");
  apiCard.className = "sw-card";
  const apiLbl = document.createElement("div");
  apiLbl.className = "sw-card-title";
  apiLbl.textContent = "接口凭证与端点（仅存本机 SQLite）";

  const keyRow = document.createElement("div");
  keyRow.className = "ip-field-row";
  const apiKeyInput = document.createElement("input");
  apiKeyInput.type = "password";
  apiKeyInput.className = "ip-field-input";
  apiKeyInput.placeholder = "API Key（留空则使用本地人设台词与语音包）";
  apiKeyInput.autocomplete = "off";
  const keyEye = document.createElement("button");
  keyEye.type = "button";
  keyEye.className = "ip-mini-pill-btn";
  keyEye.textContent = "显示";
  keyEye.addEventListener("click", () => {
    const show = apiKeyInput.type === "password";
    apiKeyInput.type = show ? "text" : "password";
    keyEye.textContent = show ? "隐藏" : "显示";
  });
  keyRow.append(apiKeyInput, keyEye);

  const urlRow = document.createElement("div");
  urlRow.className = "sw-field-col";
  const urlLbl = document.createElement("span");
  urlLbl.className = "sw-field-label";
  urlLbl.textContent = "Base URL";
  urlRow.append(urlLbl, baseUrlInput);

  const modelRow = document.createElement("div");
  modelRow.className = "sw-field-col";
  const modelLbl = document.createElement("span");
  modelLbl.className = "sw-field-label";
  modelLbl.textContent = "Model 名称";
  modelRow.append(modelLbl, modelInput);

  const memRow = document.createElement("div");
  memRow.className = "ip-row-between";
  const memHint = document.createElement("span");
  memHint.className = "sw-hint";
  memHint.textContent = "每个角色独立保留最近 3 轮短期对话记忆";
  const clearMemBtn = document.createElement("button");
  clearMemBtn.type = "button";
  clearMemBtn.className = "ip-mini-pill-btn";
  clearMemBtn.textContent = "清空对话记忆";
  clearMemBtn.addEventListener("click", () => {
    void settingsSet("chatClearHistory", true).catch(() => {});
    clearMemBtn.textContent = "已清空";
    window.setTimeout(() => {
      clearMemBtn.textContent = "清空对话记忆";
    }, 1000);
  });
  memRow.append(memHint, clearMemBtn);

  apiCard.append(apiLbl, keyRow, urlRow, modelRow, memRow);
  aiPage.append(presetCard, apiCard);

  /* ===== 页签 2：语音与声优克隆 ===== */
  const voicePage = document.createElement("div");
  voicePage.className = "sw-page";
  voicePage.hidden = true;

  /* ----- 显存模式与实时占用诊断卡 ----- */
  const vramCard = document.createElement("div");
  vramCard.className = "sw-card";
  const vramHead = document.createElement("div");
  vramHead.className = "sw-card-head";
  const vramTitle = document.createElement("span");
  vramTitle.className = "sw-card-title";
  vramTitle.textContent = "显存模式与实时占用诊断（12G 显存防冲突）";
  const vramActions = document.createElement("div");
  vramActions.className = "sw-head-actions";
  const vramRefreshBtn = document.createElement("button");
  vramRefreshBtn.type = "button";
  vramRefreshBtn.className = "ip-mini-pill-btn";
  vramRefreshBtn.textContent = "刷新显存";
  vramRefreshBtn.addEventListener("click", () => {
    void settingsSet("vramStatus", true).catch(() => {});
  });
  const vramReleaseBtn = document.createElement("button");
  vramReleaseBtn.type = "button";
  vramReleaseBtn.className = "ip-mini-pill-btn";
  vramReleaseBtn.textContent = "释放语音显存";
  vramReleaseBtn.addEventListener("click", () => {
    void settingsSet("vramRelease", true).catch(() => {});
  });
  vramActions.append(vramRefreshBtn, vramReleaseBtn);
  vramHead.append(vramTitle, vramActions);

  const vramModeGrid = document.createElement("div");
  vramModeGrid.className = "sw-vram-grid";
  const vramModeBtns = new Map<VramMode, HTMLButtonElement>();
  for (const vm of [
    {
      id: "zero_vram" as VramMode,
      name: "零显存 / 生图优先",
      desc: "0 MB 语音显存 · 纯离线语音包+缓存",
    },
    {
      id: "balanced" as VramMode,
      name: "均衡省显存（推荐）",
      desc: "日常走离线包 · 空闲自动卸载",
    },
    {
      id: "full_realtime" as VramMode,
      name: "满血实时克隆",
      desc: "常驻 Fish Speech · 16G+ 显存推荐",
    },
  ]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sw-engine-card";
    const nEl = document.createElement("span");
    nEl.className = "sw-engine-name";
    nEl.textContent = vm.name;
    const dEl = document.createElement("span");
    dEl.className = "sw-engine-desc";
    dEl.textContent = vm.desc;
    btn.append(nEl, dEl);
    btn.addEventListener("click", () => {
      config.vramMode = vm.id;
      syncUi();
      void settingsSet("vramMode", vm.id).catch(() => {});
    });
    vramModeGrid.appendChild(btn);
    vramModeBtns.set(vm.id, btn);
  }

  const vramUnloadRow = document.createElement("div");
  vramUnloadRow.className = "sw-lang-row";
  const vramUnloadLbl = document.createElement("span");
  vramUnloadLbl.className = "sw-field-label";
  vramUnloadLbl.textContent = "均衡模式空闲自动卸载时间";
  const vramUnloadStrip = document.createElement("div");
  vramUnloadStrip.className = "ip-preset-strip";
  const vramUnloadBtns = new Map<number, HTMLButtonElement>();
  for (const mins of [3, 5, 10]) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "ip-preset-chip";
    b.textContent = `${mins} 分钟`;
    b.addEventListener("click", () => {
      config.vramAutoUnloadMinutes = mins;
      syncUi();
    });
    vramUnloadStrip.appendChild(b);
    vramUnloadBtns.set(mins, b);
  }
  vramUnloadRow.append(vramUnloadLbl, vramUnloadStrip);

  const vramMeterWrap = document.createElement("div");
  vramMeterWrap.className = "sw-vram-meter";
  const vramMeterMeta = document.createElement("div");
  vramMeterMeta.className = "ip-row-between";
  const vramGpuLabel = document.createElement("span");
  vramGpuLabel.className = "sw-field-label";
  vramGpuLabel.textContent = "GPU 显存检测中…";
  const vramUsageText = document.createElement("span");
  vramUsageText.className = "sw-hint";
  vramUsageText.textContent = "—";
  vramMeterMeta.append(vramGpuLabel, vramUsageText);

  const vramBar = document.createElement("div");
  vramBar.className = "sw-vram-bar";
  const vramFill = document.createElement("div");
  vramFill.className = "sw-vram-fill";
  vramBar.appendChild(vramFill);

  const vramProcsWrap = document.createElement("div");
  vramProcsWrap.className = "sw-vram-procs";
  const vramAdviceEl = document.createElement("div");
  vramAdviceEl.className = "sw-vram-advice";
  vramAdviceEl.textContent = "打开设置或点击「刷新显存」可查看当前显存占用进程。";

  vramMeterWrap.append(vramMeterMeta, vramBar, vramProcsWrap, vramAdviceEl);
  vramCard.append(vramHead, vramModeGrid, vramUnloadRow, vramMeterWrap);

  function renderVramStatus(v: VramStatusPayload): void {
    if (v.available && v.totalMb > 0) {
      vramGpuLabel.textContent = `${v.gpuName} (${v.backend})`;
      vramUsageText.textContent = `${(v.usedMb / 1024).toFixed(1)} / ${(v.totalMb / 1024).toFixed(1)} GB (${v.usedPct.toFixed(0)}%)`;
      vramFill.style.width = `${Math.max(2, Math.min(100, v.usedPct)).toFixed(1)}%`;
      vramFill.dataset.level = v.usedPct >= 85 ? "high" : v.usedPct >= 65 ? "mid" : "ok";
    } else {
      vramGpuLabel.textContent = v.gpuName || "未检测到独立显卡计数器";
      vramUsageText.textContent = v.moemateFishSpeechRunning ? "Fish Speech 运行中" : "MoeMate 0 MB";
      vramFill.style.width = "0%";
      vramFill.dataset.level = "ok";
    }
    vramReleaseBtn.disabled = !v.moemateFishSpeechRunning;

    vramProcsWrap.replaceChildren();
    if (v.processes.length > 0) {
      for (const p of v.processes.slice(0, 6)) {
        const tag = document.createElement("span");
        tag.className = "sw-vram-proc-tag";
        tag.dataset.source = p.source;
        tag.textContent = `${p.name} · ${(p.usedMb / 1024).toFixed(1)}G`;
        vramProcsWrap.appendChild(tag);
      }
    }
    vramAdviceEl.textContent = v.advice;
  }

  const engineCard = document.createElement("div");
  engineCard.className = "sw-card";
  const engineLbl = document.createElement("div");
  engineLbl.className = "sw-card-title";
  engineLbl.textContent = "语音合成引擎";

  const ttsToggle = createToggleSwitchRow(
    "启用语音朗读",
    "关闭后所有对话与互动仅显示气泡文字",
    () => {
      config.ttsEnabled = !config.ttsEnabled;
      syncUi();
    },
  );
  const autoPlayToggle = createToggleSwitchRow(
    "启动时默认播放语音",
    "开启后自动取消静音",
    () => {
      config.autoPlayVoice = !config.autoPlayVoice;
      syncUi();
    },
  );

  const mutedBanner = document.createElement("div");
  mutedBanner.className = "sw-switch-row";
  mutedBanner.hidden = true;
  const mutedLbl = document.createElement("span");
  mutedLbl.className = "sw-hint";
  mutedLbl.textContent = "当前处于静音模式，互动语音已暂停";
  const unmuteBtn = document.createElement("button");
  unmuteBtn.type = "button";
  unmuteBtn.className = "ip-mini-pill-btn is-primary";
  unmuteBtn.textContent = "立即取消静音";
  unmuteBtn.addEventListener("click", () => {
    void settingsSet("muted", false).catch(() => {});
  });
  mutedBanner.append(mutedLbl, unmuteBtn);

  const engineCardsWrap = document.createElement("div");
  engineCardsWrap.className = "sw-engine-cards";
  const engineCardBtns = new Map<string, HTMLButtonElement>();
  for (const m of [
    { id: "fish_speech", name: "Fish Speech 克隆", desc: "本地 8 声优日语配音" },
    { id: "azure", name: "Azure 旗舰女声", desc: "云端超拟真中文语音" },
    { id: "local", name: "仅本地离线包", desc: "0ms 秒播内置语音" },
  ]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sw-engine-card";
    const nEl = document.createElement("span");
    nEl.className = "sw-engine-name";
    nEl.textContent = m.name;
    const dEl = document.createElement("span");
    dEl.className = "sw-engine-desc";
    dEl.textContent = m.desc;
    btn.append(nEl, dEl);
    btn.addEventListener("click", () => {
      config.ttsMode = m.id as ChatAiConfig["ttsMode"];
      syncUi();
      if (config.ttsMode === "fish_speech") {
        void settingsSet("fishSpeechStatus", {
          installDir: gsInstallDirInput.value.trim(),
          apiUrl: gsApiUrlInput.value.trim() || "http://127.0.0.1:8080",
        }).catch(() => {});
      }
    });
    engineCardsWrap.appendChild(btn);
    engineCardBtns.set(m.id, btn);
  }

  const azureBox = document.createElement("div");
  azureBox.className = "sw-azure-box";
  const azureKeyRow = document.createElement("div");
  azureKeyRow.className = "ip-field-row";
  const azureKeyInput = document.createElement("input");
  azureKeyInput.type = "password";
  azureKeyInput.className = "ip-field-input";
  azureKeyInput.placeholder = "Azure Speech Key（用于自由聊天实时旗舰女声）";
  azureKeyInput.autocomplete = "off";
  const azureRegionInput = document.createElement("input");
  azureRegionInput.type = "text";
  azureRegionInput.className = "ip-field-input is-region";
  azureRegionInput.placeholder = "区域如 eastasia";
  azureKeyRow.append(azureKeyInput, azureRegionInput);
  azureBox.appendChild(azureKeyRow);

  const builtinLangRow = document.createElement("div");
  builtinLangRow.className = "sw-lang-row";
  const builtinLangLbl = document.createElement("span");
  builtinLangLbl.className = "sw-field-label";
  builtinLangLbl.textContent = "内置离线语音语种（0ms 秒播）";
  const builtinLangStrip = document.createElement("div");
  builtinLangStrip.className = "ip-preset-strip";
  const builtinLangBtns = new Map<BuiltinVoiceLang, HTMLButtonElement>();
  for (const item of [
    { id: "ja" as BuiltinVoiceLang, label: "日语配音（默认）" },
    { id: "zh" as BuiltinVoiceLang, label: "中文配音" },
  ]) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "ip-preset-chip";
    b.textContent = item.label;
    b.addEventListener("click", () => {
      config.builtinVoiceLang = item.id;
      syncUi();
      void settingsSet("builtinVoiceLang", item.id).catch(() => {});
    });
    builtinLangStrip.appendChild(b);
    builtinLangBtns.set(item.id, b);
  }
  builtinLangRow.append(builtinLangLbl, builtinLangStrip);

  engineCard.append(
    engineLbl,
    ttsToggle.row,
    autoPlayToggle.row,
    mutedBanner,
    engineCardsWrap,
    builtinLangRow,
    azureBox,
  );

  /* ----- 智能防打扰与事件语音卡 ----- */
  const companionCard = document.createElement("div");
  companionCard.className = "sw-card";
  const companionLbl = document.createElement("div");
  companionLbl.className = "sw-card-title";
  companionLbl.textContent = "智能防打扰与事件语音";

  const ideVoiceToggle = createToggleSwitchRow(
    "IDE 状态事件语音播报",
    "Agent 开始工作、完成、等待确认、报错及上下文 ≥80% 时轻声提示",
    () => {
      const next = !(config.ideVoiceEnabled !== false);
      config.ideVoiceEnabled = next;
      syncUi();
      void settingsSet("ideVoiceEnabled", next).catch(() => {});
    },
  );

  const clickSfxToggle = createToggleSwitchRow(
    "轻点击触感音效",
    "轻戳角色时播放柔和五声音阶触感反馈（静音时自动关闭）",
    () => {
      const next = !(config.clickSfxEnabled !== false);
      config.clickSfxEnabled = next;
      syncUi();
      void settingsSet("clickSfxEnabled", next).catch(() => {});
    },
  );

  const companionModeRow = document.createElement("div");
  companionModeRow.className = "sw-lang-row";
  const companionModeLbl = document.createElement("span");
  companionModeLbl.className = "sw-field-label";
  companionModeLbl.textContent = "自主陪伴语音频率（防打扰）";
  const companionModeStrip = document.createElement("div");
  companionModeStrip.className = "ip-preset-strip";
  const companionModeBtns = new Map<CompanionVoiceMode, HTMLButtonElement>();
  for (const item of [
    { id: "quiet" as CompanionVoiceMode, label: "安静" },
    { id: "balanced" as CompanionVoiceMode, label: "适中（推荐）" },
    { id: "lively" as CompanionVoiceMode, label: "活泼" },
  ]) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "ip-preset-chip";
    b.textContent = item.label;
    b.addEventListener("click", () => {
      config.companionVoiceMode = item.id;
      syncUi();
      void settingsSet("companionVoiceMode", item.id).catch(() => {});
    });
    companionModeStrip.appendChild(b);
    companionModeBtns.set(item.id, b);
  }
  companionModeRow.append(companionModeLbl, companionModeStrip);

  const companionHint = document.createElement("div");
  companionHint.className = "sw-hint";
  companionHint.textContent =
    "全屏游戏/演示免打扰自动静默；高频打字 (<8s) 与 Agent 忙碌时自动避让顺延。";

  companionCard.append(
    companionLbl,
    ideVoiceToggle.row,
    clickSfxToggle.row,
    companionModeRow,
    companionHint,
  );
  voicePage.appendChild(companionCard);

  /* ----- Fish Speech v2ProPlus 服务管理卡 ----- */
  const fishSpeechCard = document.createElement("div");
  fishSpeechCard.className = "sw-card";
  const gsStatusRow = document.createElement("div");
  gsStatusRow.className = "sw-status-row";
  const gsStatusLeft = document.createElement("div");
  gsStatusLeft.className = "sw-status-left";
  const gsCardTitle = document.createElement("span");
  gsCardTitle.className = "sw-card-title";
  gsCardTitle.textContent = "Fish Speech v2ProPlus 本地引擎";
  const gsStatusDot = document.createElement("span");
  gsStatusDot.className = "sw-status-dot";
  gsStatusDot.dataset.state = "uninstalled";
  const gsStatusText = document.createElement("span");
  gsStatusText.className = "sw-status-text";
  gsStatusText.textContent = "未安装";
  gsStatusLeft.append(gsCardTitle, gsStatusDot, gsStatusText);

  const gsRefreshBtn = document.createElement("button");
  gsRefreshBtn.type = "button";
  gsRefreshBtn.className = "ip-mini-pill-btn";
  gsRefreshBtn.textContent = "检测状态";
  gsRefreshBtn.addEventListener("click", () => {
    void settingsSet("fishSpeechStatus", {
      installDir: gsInstallDirInput.value.trim(),
      apiUrl: gsApiUrlInput.value.trim() || "http://127.0.0.1:8080",
    }).catch(() => {});
  });
  gsStatusRow.append(gsStatusLeft, gsRefreshBtn);

  const gsSourceStrip = document.createElement("div");
  gsSourceStrip.className = "ip-preset-strip";
  const gsSourceBtns = new Map<string, HTMLButtonElement>();
  for (const s of [
    { id: "mirror", label: "国内镜像 (ghfast / HF-Mirror)" },
    { id: "official", label: "官方源 (GitHub / HF)" },
  ]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ip-preset-chip";
    btn.textContent = s.label;
    btn.addEventListener("click", () => {
      ensureGsConfig().downloadSource = s.id;
      syncUi();
    });
    gsSourceStrip.appendChild(btn);
    gsSourceBtns.set(s.id, btn);
  }

  const gsGpuGrid = document.createElement("div");
  gsGpuGrid.className = "sw-gpu-grid";
  const gsGpuBtns = new Map<string, HTMLButtonElement>();
  for (const g of [
    {
      id: "nvidia",
      name: "NVIDIA 显卡（推荐）",
      desc: "原生 CUDA 加速 · 兼容稳定",
    },
    {
      id: "amd",
      name: "AMD 显卡（ROCm / ZLUDA）",
      desc: "原生 Windows ROCm (gfx103X) 或 ZLUDA",
    },
  ]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sw-gpu-card";
    btn.dataset.gpu = g.id;
    const t = document.createElement("span");
    t.className = "sw-engine-name";
    t.textContent = g.name;
    const d = document.createElement("span");
    d.className = "sw-engine-desc";
    d.textContent = g.desc;
    btn.append(t, d);
    btn.addEventListener("click", () => {
      ensureGsConfig().gpuBackend = g.id;
      syncUi();
    });
    gsGpuGrid.appendChild(btn);
    gsGpuBtns.set(g.id, btn);
  }

  const gsAmdWarnBox = document.createElement("div");
  gsAmdWarnBox.className = "sw-amd-warn";
  gsAmdWarnBox.hidden = true;
  gsAmdWarnBox.textContent =
    "提示：AMD 显卡优先使用全局原生 Windows ROCm（gfx103X / TheRock）加速；若未配置全局 ROCm，也可使用 ZLUDA 转译层。";

  const gsDirRow = document.createElement("div");
  gsDirRow.className = "ip-field-row";
  const gsInstallDirInput = document.createElement("input");
  gsInstallDirInput.type = "text";
  gsInstallDirInput.className = "ip-field-input";
  gsInstallDirInput.placeholder = "引擎目录（默认 %APPDATA%\\Deskpet\\runtime_engines\\fish-speech）";
  gsInstallDirInput.addEventListener("input", () => {
    ensureGsConfig().installDir = gsInstallDirInput.value.trim();
  });
  const gsPickDirBtn = document.createElement("button");
  gsPickDirBtn.type = "button";
  gsPickDirBtn.className = "ip-mini-pill-btn";
  gsPickDirBtn.textContent = "选择目录";
  gsPickDirBtn.addEventListener("click", () => {
    requestNativePick("dir", (path) => {
      if (!path) return;
      gsInstallDirInput.value = path;
      ensureGsConfig().installDir = path;
      void settingsSet("fishSpeechStatus", {
        installDir: path,
        apiUrl: gsApiUrlInput.value.trim() || "http://127.0.0.1:8080",
      }).catch(() => {});
    });
  });
  const gsApiUrlInput = document.createElement("input");
  gsApiUrlInput.type = "text";
  gsApiUrlInput.className = "ip-field-input is-region";
  gsApiUrlInput.placeholder = "http://127.0.0.1:8080";
  gsApiUrlInput.addEventListener("input", () => {
    ensureGsConfig().apiUrl = gsApiUrlInput.value.trim();
  });
  gsDirRow.append(gsInstallDirInput, gsPickDirBtn, gsApiUrlInput);

  const gsBtnRow = document.createElement("div");
  gsBtnRow.className = "sw-gs-actions";
  const gsInstallBtn = document.createElement("button");
  gsInstallBtn.type = "button";
  gsInstallBtn.className = "ip-mini-pill-btn is-primary";
  gsInstallBtn.textContent = "一键安装 v2ProPlus";
  gsInstallBtn.addEventListener("click", () => {
    const gs = ensureGsConfig();
    gs.installDir = gsInstallDirInput.value.trim();
    gs.apiUrl = gsApiUrlInput.value.trim() || "http://127.0.0.1:8080";
    void settingsSet("fishSpeechInstall", {
      installDir: gs.installDir,
      downloadSource: gs.downloadSource,
      gpuBackend: gs.gpuBackend || "nvidia",
      apiUrl: gs.apiUrl,
    }).catch(() => {});
  });
  const gsCancelBtn = document.createElement("button");
  gsCancelBtn.type = "button";
  gsCancelBtn.className = "ip-mini-pill-btn";
  gsCancelBtn.textContent = "取消下载";
  gsCancelBtn.hidden = true;
  gsCancelBtn.addEventListener("click", () => {
    void settingsSet("fishSpeechCancelInstall", true).catch(() => {});
  });
  const gsStartBtn = document.createElement("button");
  gsStartBtn.type = "button";
  gsStartBtn.className = "ip-mini-pill-btn";
  gsStartBtn.textContent = "启动引擎";
  gsStartBtn.addEventListener("click", () => {
    statusEl.textContent = "正在拉起 Fish Speech 本地服务…";
    void settingsSet("fishSpeechStart", {
      installDir: gsInstallDirInput.value.trim(),
      apiUrl: gsApiUrlInput.value.trim() || "http://127.0.0.1:8080",
    }).catch(() => {});
  });
  const gsStopBtn = document.createElement("button");
  gsStopBtn.type = "button";
  gsStopBtn.className = "ip-mini-pill-btn";
  gsStopBtn.textContent = "停止引擎";
  gsStopBtn.addEventListener("click", () => {
    void settingsSet("fishSpeechStop", {
      installDir: gsInstallDirInput.value.trim(),
      apiUrl: gsApiUrlInput.value.trim() || "http://127.0.0.1:8080",
    }).catch(() => {});
  });
  gsBtnRow.append(gsInstallBtn, gsCancelBtn, gsStartBtn, gsStopBtn);

  const gsProgressWrap = document.createElement("div");
  gsProgressWrap.className = "sw-progress-wrap";
  gsProgressWrap.hidden = true;
  const gsProgressBar = document.createElement("div");
  gsProgressBar.className = "sw-progress-bar";
  const gsProgressFill = document.createElement("div");
  gsProgressFill.className = "sw-progress-fill";
  gsProgressBar.appendChild(gsProgressFill);
  const gsProgressMeta = document.createElement("div");
  gsProgressMeta.className = "sw-hint";
  gsProgressWrap.append(gsProgressBar, gsProgressMeta);

  const gsAutoStartToggle = createToggleSwitchRow(
    "自由聊天时自动拉起服务",
    "发送对话若引擎未运行则自动启动",
    () => {
      const gs = ensureGsConfig();
      gs.autoStartService = !gs.autoStartService;
      syncUi();
    },
  );
  const gsAutoStopToggle = createToggleSwitchRow(
    "退出桌宠时自动清理后台进程",
    "防止关闭后继续占用显存",
    () => {
      const gs = ensureGsConfig();
      gs.autoStopOnExit = !gs.autoStopOnExit;
      syncUi();
    },
  );
  const gsDailyVoiceToggle = createToggleSwitchRow(
    "日常互动与提醒全覆盖克隆配音",
    "点击/拖拽/投喂/久坐/作息/番茄钟均走 Fish Speech 日语配音",
    () => {
      const gs = ensureGsConfig();
      gs.dailyDynamicVoice = !(gs.dailyDynamicVoice ?? true);
      syncUi();
    },
  );

  fishSpeechCard.append(
    gsStatusRow,
    gsSourceStrip,
    gsGpuGrid,
    gsAmdWarnBox,
    gsDirRow,
    gsBtnRow,
    gsProgressWrap,
    gsAutoStartToggle.row,
    gsAutoStopToggle.row,
    gsDailyVoiceToggle.row,
  );

  /* ----- 8 角色独立声源配置卡 ----- */
  const charVoiceCard = document.createElement("div");
  charVoiceCard.className = "sw-card";
  const cvHead = document.createElement("div");
  cvHead.className = "sw-card-head";
  const cvLbl = document.createElement("span");
  cvLbl.className = "sw-card-title";
  cvLbl.textContent = "8 角色声优配置";

  const previewBtn = document.createElement("button");
  previewBtn.type = "button";
  previewBtn.className = "ip-mini-pill-btn is-primary";
  previewBtn.appendChild(createIcon("volumeOn", 11));
  const previewLbl = document.createElement("span");
  previewLbl.textContent = "试听当前角色";
  previewBtn.appendChild(previewLbl);
  cvHead.append(cvLbl, previewBtn);

  const CHAR_PREVIEW_ZH: Record<string, string> = {
    claude: "你好呀，红茶已经泡好了，今天也一起安心写代码吧。",
    deepseek: "哼哼，这点计算量对我来说小菜一碟，今天也要跟上我的节奏哦！",
    gemini: "呀呼！今天的状态也是满分哦，有好玩的新点子记得马上告诉我！",
    glm: "各项系统的连接都已经确认完毕，工作告一段落的话请记得休息一下哦。",
    gpt: "放心吧，先把整体架构梳理清楚，复杂的地方就交给我来处理。",
    grok: "好啦好啦，真拿你没办法，让我来一起看看究竟是哪里出问题了吧。",
    kimi: "嗯，我会一直安静陪在你身边的，再长的资料我们也一起慢慢整理好。",
    qwen: "好耶！喝完奶茶打起精神，今天的开发任务也一口气漂亮地搞定吧！",
  };

  const CHAR_PREVIEW_JA: Record<string, string> = {
    claude: "指揮官、新しい任務の準備が完了しました。今日も一緒に頑張りましょうね。",
    deepseek: "ふふっ、私の声、ちゃんと聞こえてる？今日もいいコードを書きなさいよね。",
    gemini: "やっほー！音声クローンの準備もばっちりだよ！これからよろしくね！",
    glm: "音声エンジンの接続を確認しました。いつでも作業を始められますよ。",
    gpt: "準備は万全です。どんな複雑なコードも、私と一緒に片付けましょう。",
    grok: "おっ、いい感じに声が出てるじゃない。さあ、面白いものを作ろうよ！",
    kimi: "こんにちは。今日もあなたのそばで、ゆっくり丁寧にお手伝いするね。",
    qwen: "よしっ！元気いっぱいで準備完了だよ！今日も楽しくコーディングしようね！",
  };

  function getDefaultPreviewLine(charId: string, lang: string): string {
    if (lang === "all_ja") return CHAR_PREVIEW_JA[charId] ?? CHAR_PREVIEW_JA.claude;
    if (lang === "en") return "Hello! My local voice clone is online and ready.";
    return CHAR_PREVIEW_ZH[charId] ?? CHAR_PREVIEW_ZH.claude;
  }

  function triggerPreview(overrideText?: string, overrideLang?: string): void {
    const voice = config.charVoices[selectedCharId] ?? "";
    const gs = ensureGsConfig();
    gs.installDir = gsInstallDirInput.value.trim();
    gs.apiUrl = gsApiUrlInput.value.trim() || "http://127.0.0.1:8080";
    const charProf = getCharProfile(selectedCharId);
    const customLine = gsPreviewTextInput.value.trim();
    const previewLine =
      overrideText ||
      customLine ||
      getDefaultPreviewLine(selectedCharId, overrideLang || charProf.textLang || "ja");
    const previewGs = {
      ...gs,
      charProfiles: {
        ...gs.charProfiles,
        [selectedCharId]: {
          ...charProf,
          textLang: overrideLang || charProf.textLang || "ja",
        },
      },
    };
    statusEl.textContent =
      config.ttsMode === "local"
        ? "正在试听本地情绪语音包…"
        : config.ttsMode === "fish_speech"
          ? "正在合成 Fish Speech 克隆试听…"
          : "正在试听 Azure 旗舰女声…";
    void settingsSet("ttsPreview", {
      charId: selectedCharId,
      voice,
      ttsMode: config.ttsMode,
      azureSpeechKey: azureKeyInput.value.trim(),
      azureSpeechRegion: azureRegionInput.value.trim() || "eastasia",
      fishSpeech: previewGs,
      builtinVoiceLang: config.builtinVoiceLang || "ja",
      text: previewLine,
    }).catch(() => {});
  }

  previewBtn.addEventListener("click", () => triggerPreview());

  const voiceCharGrid = document.createElement("div");
  voiceCharGrid.className = "sw-char-grid";
  const voiceCharBtns = new Map<string, HTMLButtonElement>();

  const voiceSelect = document.createElement("select");
  voiceSelect.className = "ip-field-select";
  for (const v of FEMALE_VOICE_OPTIONS) {
    const opt = document.createElement("option");
    opt.value = v.value;
    opt.textContent = v.label;
    voiceSelect.appendChild(opt);
  }
  voiceSelect.addEventListener("change", () => {
    const val = voiceSelect.value;
    if (val) config.charVoices[selectedCharId] = val;
    else delete config.charVoices[selectedCharId];
  });

  const gsCharBox = document.createElement("div");
  gsCharBox.className = "sw-gs-char-box";
  gsCharBox.hidden = true;

  const refCol = document.createElement("div");
  refCol.className = "sw-field-col";
  const refLbl = document.createElement("span");
  refLbl.className = "sw-field-label";
  refLbl.textContent = "参考音频（推荐 3~10 秒人声干声，支持 wav/ogg/mp3/flac）";
  const refRow = document.createElement("div");
  refRow.className = "ip-field-row";
  const gsRefAudioInput = document.createElement("input");
  gsRefAudioInput.type = "text";
  gsRefAudioInput.className = "ip-field-input";
  gsRefAudioInput.placeholder = "未选择参考音频（留空则自动降级）";
  gsRefAudioInput.addEventListener("input", () => {
    getCharProfile(selectedCharId).refAudioPath = gsRefAudioInput.value.trim();
  });
  const gsPickRefBtn = document.createElement("button");
  gsPickRefBtn.type = "button";
  gsPickRefBtn.className = "ip-mini-pill-btn";
  gsPickRefBtn.textContent = "选择音频";
  gsPickRefBtn.addEventListener("click", () => {
    const targetChar = selectedCharId;
    requestNativePick("audio", (path) => {
      if (!path) return;
      getCharProfile(targetChar).refAudioPath = path;
      if (selectedCharId === targetChar) gsRefAudioInput.value = path;
    });
  });
  const gsClearRefBtn = document.createElement("button");
  gsClearRefBtn.type = "button";
  gsClearRefBtn.className = "ip-mini-pill-btn";
  gsClearRefBtn.textContent = "清空";
  gsClearRefBtn.addEventListener("click", () => {
    getCharProfile(selectedCharId).refAudioPath = "";
    gsRefAudioInput.value = "";
  });
  refRow.append(gsRefAudioInput, gsPickRefBtn, gsClearRefBtn);
  refCol.append(refLbl, refRow);

  const promptCol = document.createElement("div");
  promptCol.className = "sw-field-col";
  const promptLbl = document.createElement("span");
  promptLbl.className = "sw-field-label";
  promptLbl.textContent = "参考台词文本（与参考音频内容一致）";
  const gsPromptTextInput = document.createElement("input");
  gsPromptTextInput.type = "text";
  gsPromptTextInput.className = "ip-field-input";
  gsPromptTextInput.placeholder = "输入参考音频对应的台词文本…";
  gsPromptTextInput.addEventListener("input", () => {
    getCharProfile(selectedCharId).promptText = gsPromptTextInput.value;
  });
  promptCol.append(promptLbl, gsPromptTextInput);

  const langGrid = document.createElement("div");
  langGrid.className = "sw-grid-2";

  const pLangCol = document.createElement("div");
  pLangCol.className = "sw-field-col";
  const pLangLbl = document.createElement("span");
  pLangLbl.className = "sw-field-label";
  pLangLbl.textContent = "参考语种";
  const gsPromptLangSelect = document.createElement("select");
  gsPromptLangSelect.className = "ip-field-select";
  for (const l of [
    { value: "zh", label: "中文 zh" },
    { value: "ja", label: "日语 ja" },
    { value: "en", label: "英语 en" },
    { value: "auto", label: "自动 auto" },
  ]) {
    const opt = document.createElement("option");
    opt.value = l.value;
    opt.textContent = l.label;
    gsPromptLangSelect.appendChild(opt);
  }
  gsPromptLangSelect.addEventListener("change", () => {
    getCharProfile(selectedCharId).promptLang = gsPromptLangSelect.value;
  });
  pLangCol.append(pLangLbl, gsPromptLangSelect);

  const tLangCol = document.createElement("div");
  tLangCol.className = "sw-field-col";
  const tLangLbl = document.createElement("span");
  tLangLbl.className = "sw-field-label";
  tLangLbl.textContent = "输出语种";
  const gsTextLangSelect = document.createElement("select");
  gsTextLangSelect.className = "ip-field-select";
  for (const l of [
    { value: "ja", label: "日语配音（回复中文 · 语音日语）" },
    { value: "zh", label: "中文配音（回复中文 · 语音中文）" },
    { value: "auto", label: "按文本语种自动识别 (auto)" },
    { value: "all_ja", label: "纯日语/日英混合 (all_ja)" },
    { value: "all_zh", label: "中英混合配音 (all_zh)" },
    { value: "en", label: "英语配音 (en)" },
  ]) {
    const opt = document.createElement("option");
    opt.value = l.value;
    opt.textContent = l.label;
    gsTextLangSelect.appendChild(opt);
  }
  gsTextLangSelect.addEventListener("change", () => {
    getCharProfile(selectedCharId).textLang = gsTextLangSelect.value;
    gsPreviewTextInput.placeholder = getDefaultPreviewLine(selectedCharId, gsTextLangSelect.value);
  });
  tLangCol.append(tLangLbl, gsTextLangSelect);
  langGrid.append(pLangCol, tLangCol);

  const previewTextCol = document.createElement("div");
  previewTextCol.className = "sw-field-col";
  const previewTextHead = document.createElement("div");
  previewTextHead.className = "ip-row-between";
  const previewTextLbl = document.createElement("span");
  previewTextLbl.className = "sw-field-label";
  previewTextLbl.textContent = "试听台词（留空则按输出语种自动匹配）";
  const previewQuickBtns = document.createElement("div");
  previewQuickBtns.className = "sw-head-actions";
  const previewZhBtn = document.createElement("button");
  previewZhBtn.type = "button";
  previewZhBtn.className = "ip-mini-pill-btn";
  previewZhBtn.textContent = "试听中文";
  previewZhBtn.addEventListener("click", () => {
    triggerPreview(CHAR_PREVIEW_ZH[selectedCharId] ?? CHAR_PREVIEW_ZH.claude, "zh");
  });
  const previewJaBtn = document.createElement("button");
  previewJaBtn.type = "button";
  previewJaBtn.className = "ip-mini-pill-btn";
  previewJaBtn.textContent = "试听日语";
  previewJaBtn.addEventListener("click", () => {
    triggerPreview(CHAR_PREVIEW_ZH[selectedCharId] ?? CHAR_PREVIEW_ZH.claude, "ja");
  });
  previewQuickBtns.append(previewZhBtn, previewJaBtn);
  previewTextHead.append(previewTextLbl, previewQuickBtns);
  const gsPreviewTextInput = document.createElement("input");
  gsPreviewTextInput.type = "text";
  gsPreviewTextInput.className = "ip-field-input";
  gsPreviewTextInput.placeholder = getDefaultPreviewLine(selectedCharId, "ja");
  previewTextCol.append(previewTextHead, gsPreviewTextInput);

  const advDetails = document.createElement("details");
  advDetails.className = "sw-details";
  const advSummary = document.createElement("summary");
  advSummary.className = "sw-details-summary";
  advSummary.textContent = "进阶：服务端预设参考音色 ID（reference_id，留空使用上方参考音频）";
  const advBody = document.createElement("div");
  advBody.className = "sw-details-body";

  const refIdRow = document.createElement("div");
  refIdRow.className = "ip-field-row";
  const fsRefIdInput = document.createElement("input");
  fsRefIdInput.type = "text";
  fsRefIdInput.className = "ip-field-input";
  fsRefIdInput.placeholder = "服务端 reference_id（可选，如 char_neko）";
  fsRefIdInput.addEventListener("input", () => {
    getCharProfile(selectedCharId).referenceId = fsRefIdInput.value.trim();
  });
  refIdRow.append(fsRefIdInput);

  advBody.append(refIdRow);
  advDetails.append(advSummary, advBody);

  gsCharBox.append(refCol, promptCol, langGrid, previewTextCol, advDetails);
  charVoiceCard.append(cvHead, voiceCharGrid, voiceSelect, gsCharBox);
  voicePage.append(vramCard, engineCard, fishSpeechCard, charVoiceCard);

  /* ===== 页签 3：角色性格定制 ===== */
  const personaPage = document.createElement("div");
  personaPage.className = "sw-page";
  personaPage.hidden = true;

  const personaCard = document.createElement("div");
  personaCard.className = "sw-card";
  const personaCharGrid = document.createElement("div");
  personaCharGrid.className = "sw-char-grid";
  const personaCharBtns = new Map<string, HTMLButtonElement>();

  const personaHead = document.createElement("div");
  personaHead.className = "sw-persona-head";
  const personaTitleEl = document.createElement("span");
  personaTitleEl.className = "sw-card-title";
  const personaTagPill = document.createElement("span");
  personaTagPill.className = "ip-tag-pill";
  personaHead.append(personaTitleEl, personaTagPill);

  const personaTextarea = document.createElement("textarea");
  personaTextarea.className = "sw-textarea";
  personaTextarea.rows = 6;
  personaTextarea.placeholder = "留空则使用中文社区默认二次元人设…";
  personaTextarea.addEventListener("input", () => {
    const val = personaTextarea.value.trim();
    if (val) config.customPersonas[selectedCharId] = personaTextarea.value;
    else delete config.customPersonas[selectedCharId];
  });

  const personaFoot = document.createElement("div");
  personaFoot.className = "ip-row-between";
  const personaHint = document.createElement("span");
  personaHint.className = "sw-hint";
  personaHint.textContent = "支持自由修改口癖、称呼与傲娇度";
  const resetPersonaBtn = document.createElement("button");
  resetPersonaBtn.type = "button";
  resetPersonaBtn.className = "ip-mini-pill-btn";
  resetPersonaBtn.textContent = "恢复默认二创性格";
  resetPersonaBtn.addEventListener("click", () => {
    delete config.customPersonas[selectedCharId];
    personaTextarea.value = CHAR_DEFAULT_PERSONAS[selectedCharId]?.prompt ?? "";
  });
  personaFoot.append(personaHint, resetPersonaBtn);

  function buildCharCard(
    ch: (typeof CHAR_ORDER)[number],
    onSelect: () => void,
  ): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "sw-char-card";
    const top = document.createElement("div");
    top.className = "sw-char-top";
    const cAva = createPixelAvatar(ch.id, 20, "sw-char-pixel-ava");
    const cName = document.createElement("span");
    cName.className = "sw-char-name";
    cName.textContent = ch.name;
    top.append(cAva, cName);
    const cTag = document.createElement("span");
    cTag.className = "sw-char-tag";
    cTag.textContent = ch.shortTag;
    btn.append(top, cTag);
    btn.addEventListener("click", onSelect);
    return btn;
  }

  for (const ch of CHAR_ORDER) {
    const b1 = buildCharCard(ch, () => {
      selectedCharId = ch.id;
      syncUi();
    });
    voiceCharGrid.appendChild(b1);
    voiceCharBtns.set(ch.id, b1);

    const b2 = buildCharCard(ch, () => {
      selectedCharId = ch.id;
      syncUi();
    });
    personaCharGrid.appendChild(b2);
    personaCharBtns.set(ch.id, b2);
  }

  personaCard.append(personaCharGrid, personaHead, personaTextarea, personaFoot);
  personaPage.appendChild(personaCard);

  /* ===== 页签 3（新）：角色动画与语音资源下载 ===== */
  const resourcesPage = document.createElement("div");
  resourcesPage.className = "sw-page";
  resourcesPage.hidden = true;

  const resRepoCard = document.createElement("div");
  resRepoCard.className = "sw-card";
  const resRepoHead = document.createElement("div");
  resRepoHead.className = "sw-card-head";
  const resRepoTitle = document.createElement("span");
  resRepoTitle.className = "sw-card-title";
  resRepoTitle.textContent = "GitHub 同仓库 Releases 分发源";
  const resRefreshBtn = document.createElement("button");
  resRefreshBtn.type = "button";
  resRefreshBtn.className = "ip-mini-pill-btn";
  resRefreshBtn.textContent = "刷新本地状态";
  resRefreshBtn.addEventListener("click", () => {
    void settingsSet("assetsStatus", true).catch(() => {});
  });
  resRepoHead.append(resRepoTitle, resRefreshBtn);

  const resRepoRow = document.createElement("div");
  resRepoRow.className = "ip-field-row";
  const resRepoInput = document.createElement("input");
  resRepoInput.type = "text";
  resRepoInput.className = "ip-field-input";
  resRepoInput.placeholder = "owner/repo（例如 RicardoLiuW/MoeMate）";
  const resTagInput = document.createElement("input");
  resTagInput.type = "text";
  resTagInput.className = "ip-field-input is-region";
  resTagInput.placeholder = "latest";
  resRepoRow.append(resRepoInput, resTagInput);

  const resSourceStrip = document.createElement("div");
  resSourceStrip.className = "ip-preset-strip";
  const resSourceBtns = new Map<string, HTMLButtonElement>();
  for (const s of [
    { id: "mirror", label: "GitHub 国内加速 (ghfast.top)" },
    { id: "official", label: "GitHub 官方直连" },
  ]) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ip-preset-chip";
    btn.textContent = s.label;
    btn.addEventListener("click", () => {
      assetsStatus.downloadSource = s.id;
      void settingsSet("assetsDownloadConfig", {
        githubRepo: resRepoInput.value.trim() || assetsStatus.githubRepo,
        releaseTag: resTagInput.value.trim() || "latest",
        downloadSource: s.id,
      }).catch(() => {});
      renderResourcesUi();
    });
    resSourceStrip.appendChild(btn);
    resSourceBtns.set(s.id, btn);
  }
  const resRootHint = document.createElement("div");
  resRootHint.className = "sw-hint";
  resRootHint.textContent = "资产保存目录：—";
  resRepoCard.append(resRepoHead, resRepoRow, resSourceStrip, resRootHint);

  const resVoiceCard = document.createElement("div");
  resVoiceCard.className = "sw-card";
  const resVoiceRow = document.createElement("div");
  resVoiceRow.className = "sw-status-row";
  const resVoiceInfo = document.createElement("div");
  resVoiceInfo.className = "sw-switch-info";
  const resVoiceTitle = document.createElement("span");
  resVoiceTitle.className = "sw-card-title";
  resVoiceTitle.textContent = "8 角色日语预设语音包（免显卡 · 0ms 秒播）";
  const resVoiceSub = document.createElement("span");
  resVoiceSub.className = "sw-hint";
  resVoiceSub.textContent = "含全角色日常日语配音素材（ja-voices-pack.zip），AMD/无独显用户推荐优先下载";
  resVoiceInfo.append(resVoiceTitle, resVoiceSub);
  const resVoiceBtn = document.createElement("button");
  resVoiceBtn.type = "button";
  resVoiceBtn.className = "ip-mini-pill-btn is-primary";
  resVoiceBtn.textContent = "下载日语语音包";
  resVoiceBtn.addEventListener("click", () => {
    void triggerAssetInstall(["voices"]);
  });
  resVoiceRow.append(resVoiceInfo, resVoiceBtn);
  resVoiceCard.appendChild(resVoiceRow);

  const resCharsCard = document.createElement("div");
  resCharsCard.className = "sw-card";
  const resCharsHead = document.createElement("div");
  resCharsHead.className = "sw-card-head";
  const resCharsTitle = document.createElement("span");
  resCharsTitle.className = "sw-card-title";
  resCharsTitle.textContent = "8 角色动画包（按需下载 · 本体零内置）";
  const resCharsBtns = document.createElement("div");
  resCharsBtns.className = "sw-gs-actions";
  const resInstallMissingBtn = document.createElement("button");
  resInstallMissingBtn.type = "button";
  resInstallMissingBtn.className = "ip-mini-pill-btn is-primary";
  resInstallMissingBtn.textContent = "一键下载未安装角色";
  resInstallMissingBtn.addEventListener("click", () => {
    const missing = assetsStatus.chars.filter((c) => !c.installed).map((c) => c.id);
    if (missing.length > 0) {
      void triggerAssetInstall(missing);
    }
  });
  const resCancelBtn = document.createElement("button");
  resCancelBtn.type = "button";
  resCancelBtn.className = "ip-mini-pill-btn";
  resCancelBtn.textContent = "取消下载";
  resCancelBtn.hidden = true;
  resCancelBtn.addEventListener("click", () => {
    void settingsSet("assetsCancelInstall", true).catch(() => {});
  });
  resCharsBtns.append(resInstallMissingBtn, resCancelBtn);
  resCharsHead.append(resCharsTitle, resCharsBtns);

  const resProgressWrap = document.createElement("div");
  resProgressWrap.className = "sw-progress-wrap";
  resProgressWrap.hidden = true;
  const resProgressBar = document.createElement("div");
  resProgressBar.className = "sw-progress-bar";
  const resProgressFill = document.createElement("div");
  resProgressFill.className = "sw-progress-fill";
  resProgressBar.appendChild(resProgressFill);
  const resProgressMeta = document.createElement("div");
  resProgressMeta.className = "sw-hint";
  resProgressWrap.append(resProgressBar, resProgressMeta);

  const resCharsGrid = document.createElement("div");
  resCharsGrid.className = "sw-res-grid";
  resCharsCard.append(resCharsHead, resProgressWrap, resCharsGrid);
  resourcesPage.append(resRepoCard, resVoiceCard, resCharsCard);

  async function triggerAssetInstall(targets: string[]): Promise<void> {
    if (targets.length === 0) return;
    await settingsSet("assetsInstall", {
      githubRepo: resRepoInput.value.trim() || assetsStatus.githubRepo,
      releaseTag: resTagInput.value.trim() || "latest",
      downloadSource: assetsStatus.downloadSource || "mirror",
      targets,
    }).catch(() => {});
  }

  function renderResourcesUi(): void {
    if (document.activeElement !== resRepoInput && assetsStatus.githubRepo) {
      resRepoInput.value = assetsStatus.githubRepo;
    }
    if (document.activeElement !== resTagInput && assetsStatus.releaseTag) {
      resTagInput.value = assetsStatus.releaseTag;
    }
    resRootHint.textContent = assetsStatus.assetsRoot
      ? `资产目录：${assetsStatus.assetsRoot}`
      : "资产目录：由后端自动分配";

    for (const [id, btn] of resSourceBtns.entries()) {
      btn.dataset.active = (assetsStatus.downloadSource || "mirror") === id ? "true" : "false";
    }

    resVoiceBtn.disabled = assetsStatus.installing;
    resVoiceBtn.textContent = assetsStatus.voicePackInstalled
      ? "重新下载 / 更新语音"
      : "下载日语语音包";

    const missingCount = assetsStatus.chars.filter((c) => !c.installed).length;
    resInstallMissingBtn.disabled = assetsStatus.installing || missingCount === 0;
    resInstallMissingBtn.textContent =
      missingCount === 0 ? "全部角色已安装" : `一键下载未安装 (${missingCount})`;
    resCancelBtn.hidden = !assetsStatus.installing;

    resCharsGrid.replaceChildren();
    const list =
      assetsStatus.chars.length > 0
        ? assetsStatus.chars
        : CHAR_ORDER.map((c) => ({
            id: c.id,
            name: c.name,
            archiveName: `char-${c.id}.zip`,
            approxSizeMb: 750,
            installed: false,
            clipCount: 0,
            voiceReady: false,
          }));

    for (const item of list) {
      const card = document.createElement("div");
      card.className = "sw-res-item";
      card.dataset.installed = item.installed ? "true" : "false";

      const left = document.createElement("div");
      left.className = "sw-res-left";
      left.appendChild(createPixelAvatar(item.id, 30, "sw-char-pixel-ava"));
      const info = document.createElement("div");
      info.className = "sw-res-info";
      const nm = document.createElement("span");
      nm.className = "sw-char-name";
      nm.textContent = item.name;
      const st = document.createElement("span");
      st.className = "sw-char-tag";
      st.textContent = item.installed
        ? `已安装 · ${item.clipCount} 段`
        : `未下载 · 约 ${item.approxSizeMb} MB`;
      info.append(nm, st);
      left.appendChild(info);

      const ops = document.createElement("div");
      ops.className = "sw-res-ops";
      const dlBtn = document.createElement("button");
      dlBtn.type = "button";
      dlBtn.className = item.installed ? "ip-mini-pill-btn" : "ip-mini-pill-btn is-primary";
      dlBtn.textContent = item.installed ? "更新" : "下载";
      dlBtn.disabled = assetsStatus.installing;
      dlBtn.addEventListener("click", () => {
        void triggerAssetInstall([item.id]);
      });
      ops.appendChild(dlBtn);

      if (item.installed) {
        const rmBtn = document.createElement("button");
        rmBtn.type = "button";
        rmBtn.className = "ip-mini-pill-btn";
        rmBtn.title = "清理本地动画以释放空间";
        rmBtn.disabled = assetsStatus.installing;
        rmBtn.appendChild(createIcon("trash", 11));
        rmBtn.addEventListener("click", () => {
          void settingsSet("assetsDeleteChar", item.id).catch(() => {});
        });
        ops.appendChild(rmBtn);
      }

      card.append(left, ops);
      resCharsGrid.appendChild(card);
    }
  }

  /* ===== 页签 4：系统与开发者 ===== */
  const systemPage = document.createElement("div");
  systemPage.className = "sw-page";
  systemPage.hidden = true;

  const sysBootCard = document.createElement("div");
  sysBootCard.className = "sw-card";
  const sysBootLbl = document.createElement("div");
  sysBootLbl.className = "sw-card-title";
  sysBootLbl.textContent = "启动与全局快捷键";

  const autoStartToggle = createToggleSwitchRow(
    "Windows 开机自启",
    "写入 HKCU\\...\\Run，与主舞台右键菜单双向同步",
    () => {
      const next = !autoStartEnabled;
      autoStartEnabled = next;
      syncUi();
      void settingsSet("autoStart", next)
        .then(() => {
          statusEl.textContent = next ? "已开启 Windows 开机自启" : "已关闭开机自启";
        })
        .catch(() => {
          autoStartEnabled = !next;
          syncUi();
          statusEl.textContent = "开机自启设置失败";
        });
    },
  );

  const hotkeyToggle = createToggleSwitchRow(
    "全局快捷键 Alt + Space",
    "随时随地快速呼出/隐藏灵动岛聊天输入框",
    () => {
      const next = !globalHotkeyEnabled;
      globalHotkeyEnabled = next;
      syncUi();
      void settingsSet("globalHotkey", next).catch(() => {
        globalHotkeyEnabled = !next;
        syncUi();
      });
    },
  );
  sysBootCard.append(sysBootLbl, autoStartToggle.row, hotkeyToggle.row);

  const sysDevCard = document.createElement("div");
  sysDevCard.className = "sw-card";
  const sysDevLbl = document.createElement("div");
  sysDevLbl.className = "sw-card-title";
  sysDevLbl.textContent = "本地 HTTP Hook 联动（Claude Code / Git / CI）";

  const endpointBox = document.createElement("div");
  endpointBox.className = "sw-endpoint-box";
  const endpointCode = document.createElement("span");
  endpointCode.className = "sw-endpoint-code";
  endpointCode.textContent = "POST http://127.0.0.1:19820/trigger";
  const hookCopyBtn = document.createElement("button");
  hookCopyBtn.type = "button";
  hookCopyBtn.className = "ip-mini-pill-btn";
  hookCopyBtn.textContent = "复制测试命令";
  hookCopyBtn.addEventListener("click", () => {
    const cmd =
      'curl -X POST http://127.0.0.1:19820/trigger -H "Content-Type: application/json" -d "{\\"scene\\":\\"done\\",\\"bubble\\":\\"构建通过！干得漂亮～\\"}"';
    void navigator.clipboard?.writeText(cmd).then(() => {
      hookCopyBtn.textContent = "已复制";
      window.setTimeout(() => {
        hookCopyBtn.textContent = "复制测试命令";
      }, 1400);
    });
  });
  endpointBox.append(endpointCode, hookCopyBtn);

  const hookHint = document.createElement("div");
  hookHint.className = "sw-hint";
  hookHint.textContent = "支持 hook_event_name (Stop / PermissionRequest) 或 scene / clipId / bubble / pomodoro。";
  sysDevCard.append(sysDevLbl, endpointBox, hookHint);

  const sysCareCard = document.createElement("div");
  sysCareCard.className = "sw-card";
  const sysCareLbl = document.createElement("div");
  sysCareLbl.className = "sw-card-title";
  sysCareLbl.textContent = "智能陪伴与体积隔离";
  const sysCareLine1 = document.createElement("div");
  sysCareLine1.className = "sw-hint";
  sysCareLine1.textContent = "• 智能久坐提醒：连续专注写代码满 45 分钟后，仅在 Agent 回归 Idle 时弹泡并播放角色提醒语音。";
  const sysCareLine2 = document.createElement("div");
  sysCareLine2.className = "sw-hint";
  sysCareLine2.textContent = "• 本体零捆绑策略：Fish Speech v2ProPlus 引擎与权重按需隔离于独立目录，不打包进主程序。";
  sysCareCard.append(sysCareLbl, sysCareLine1, sysCareLine2);

  systemPage.append(sysBootCard, sysDevCard, sysCareCard);

  tabPages.set("ai", aiPage);
  tabPages.set("voice", voicePage);
  tabPages.set("resources", resourcesPage);
  tabPages.set("persona", personaPage);
  tabPages.set("system", systemPage);
  body.append(aiPage, voicePage, resourcesPage, personaPage, systemPage);

  // 底栏
  const footer = document.createElement("div");
  footer.className = "sw-footer";
  const statusEl = document.createElement("span");
  statusEl.className = "sw-hint";
  statusEl.textContent = "配置保存在本机 SQLite，即时同步主舞台 (Ctrl+S 保存)";
  const saveBtn = document.createElement("button");
  saveBtn.type = "button";
  saveBtn.className = "ip-mini-pill-btn is-primary sw-save-btn";
  saveBtn.textContent = "保存并生效";
  saveBtn.addEventListener("click", () => {
    void saveAll();
  });
  footer.append(statusEl, saveBtn);

  main.append(header, body, footer);
  shellEl.append(sidebar, main);
  uiLayer.appendChild(shellEl);

  function switchTab(tab: SettingsTabId): void {
    activeTab = tab;
    for (const [id, btn] of navBtns.entries()) {
      btn.dataset.active = id === tab ? "true" : "false";
    }
    for (const [id, page] of tabPages.entries()) {
      page.hidden = id !== tab;
    }
    const meta = TAB_META.find((t) => t.id === tab) ?? TAB_META[0];
    pageTitleEl.textContent = meta.title;
    pageSubEl.textContent = meta.sub;
  }
  switchTab(activeTab);

  function syncUi(): void {
    for (const [id, btn] of presetBtns.entries()) {
      btn.dataset.active = config.provider === id ? "true" : "false";
    }
    ttsToggle.setOn(config.ttsEnabled);
    autoPlayToggle.setOn(config.autoPlayVoice);
    ideVoiceToggle.setOn(config.ideVoiceEnabled !== false);
    clickSfxToggle.setOn(config.clickSfxEnabled !== false);
    const activeCompanionMode: CompanionVoiceMode =
      config.companionVoiceMode === "quiet" || config.companionVoiceMode === "lively"
        ? config.companionVoiceMode
        : "balanced";
    for (const [id, btn] of companionModeBtns.entries()) {
      btn.dataset.active = activeCompanionMode === id ? "true" : "false";
    }
    autoStartToggle.setOn(autoStartEnabled);
    hotkeyToggle.setOn(globalHotkeyEnabled);

    const activeMode =
      config.ttsMode === "local" || config.ttsMode === "fish_speech"
        ? config.ttsMode
        : "azure";
    for (const [id, btn] of engineCardBtns.entries()) {
      btn.dataset.active = activeMode === id ? "true" : "false";
    }

    const activeVramMode: VramMode =
      config.vramMode === "zero_vram" || config.vramMode === "full_realtime"
        ? config.vramMode
        : "balanced";
    for (const [id, btn] of vramModeBtns.entries()) {
      btn.dataset.active = activeVramMode === id ? "true" : "false";
    }
    vramUnloadRow.hidden = activeVramMode !== "balanced";
    const curUnload = config.vramAutoUnloadMinutes ?? 5;
    for (const [mins, btn] of vramUnloadBtns.entries()) {
      btn.dataset.active = curUnload === mins ? "true" : "false";
    }
    const activeLang: BuiltinVoiceLang = config.builtinVoiceLang === "zh" ? "zh" : "ja";
    for (const [id, btn] of builtinLangBtns.entries()) {
      btn.dataset.active = activeLang === id ? "true" : "false";
    }

    const isFishSpeech = activeMode === "fish_speech";
    azureBox.hidden = activeMode !== "azure";

    const gs = ensureGsConfig();
    for (const [id, btn] of gsSourceBtns.entries()) {
      btn.dataset.active = (gs.downloadSource || "mirror") === id ? "true" : "false";
    }
    const activeGpu = gs.gpuBackend === "amd" ? "amd" : "nvidia";
    for (const [id, btn] of gsGpuBtns.entries()) {
      btn.dataset.active = activeGpu === id ? "true" : "false";
    }
    gsAmdWarnBox.hidden = activeGpu !== "amd";
    gsInstallBtn.textContent =
      activeGpu === "amd"
        ? "安装 AMD (ZLUDA 实验版)"
        : "安装 NVIDIA (CUDA 推荐)";
    gsAutoStartToggle.setOn(gs.autoStartService);
    gsAutoStopToggle.setOn(gs.autoStopOnExit);
    gsDailyVoiceToggle.setOn(gs.dailyDynamicVoice ?? true);

    gsStatusDot.dataset.state = gsStatus.state;
    gsStatusText.textContent = gsStatus.label;
    gsInstallBtn.disabled = gsStatus.state === "installing";
    gsCancelBtn.hidden = gsStatus.state !== "installing";
    gsStartBtn.disabled = gsStatus.state === "running" || gsStatus.state === "installing";
    gsStopBtn.disabled = gsStatus.state !== "running";

    cvLbl.textContent = isFishSpeech
      ? "8 角色独立声优克隆（Fish Speech）"
      : "8 角色纯女声配置（已过滤全部男声）";
    voiceSelect.hidden = isFishSpeech;
    gsCharBox.hidden = !isFishSpeech;

    for (const [id, btn] of voiceCharBtns.entries()) {
      btn.dataset.active = selectedCharId === id ? "true" : "false";
    }
    for (const [id, btn] of personaCharBtns.entries()) {
      btn.dataset.active = selectedCharId === id ? "true" : "false";
    }

    voiceSelect.value = config.charVoices[selectedCharId] ?? "";

    const prof = getCharProfile(selectedCharId);
    gsRefAudioInput.value = prof.refAudioPath ?? "";
    gsPromptTextInput.value = prof.promptText ?? "";
    gsPromptLangSelect.value = prof.promptLang || "zh";
    gsTextLangSelect.value = prof.textLang || "ja";
    gsPreviewTextInput.placeholder = getDefaultPreviewLine(selectedCharId, gsTextLangSelect.value);
    fsRefIdInput.value = prof.referenceId ?? "";

    const p = CHAR_DEFAULT_PERSONAS[selectedCharId];
    const cMeta = getCharMeta(selectedCharId);
    personaTitleEl.textContent = p ? `${cMeta.name} · ${p.title}` : cMeta.name;
    personaTagPill.textContent = cMeta.shortTag;
    personaTextarea.value = config.customPersonas[selectedCharId] ?? p?.prompt ?? "";
    renderResourcesUi();
  }

  function applyConfigFromBackend(next: ChatAiConfig): void {
    const incomingGs = next.fishSpeech ?? createDefaultFishSpeechConfig();
    config = {
      ...createDefaultConfig(),
      ...next,
      charVoices: { ...(next.charVoices ?? {}) },
      customPersonas: { ...(next.customPersonas ?? {}) },
      fishSpeech: {
        ...createDefaultFishSpeechConfig(),
        ...incomingGs,
        charProfiles: { ...(incomingGs.charProfiles ?? {}) },
      },
    };
    apiKeyInput.value = config.apiKey ?? "";
    baseUrlInput.value = config.baseUrl ?? "";
    modelInput.value = config.model ?? "";
    azureKeyInput.value = config.azureSpeechKey ?? "";
    azureRegionInput.value = config.azureSpeechRegion || "eastasia";
    gsInstallDirInput.value = config.fishSpeech?.installDir ?? "";
    gsApiUrlInput.value = config.fishSpeech?.apiUrl || "http://127.0.0.1:8080";
    syncUi();
  }

  async function saveAll(): Promise<void> {
    const gs = ensureGsConfig();
    gs.installDir = gsInstallDirInput.value.trim();
    gs.apiUrl = gsApiUrlInput.value.trim() || "http://127.0.0.1:8080";

    const next: ChatAiConfig = {
      ...config,
      apiKey: apiKeyInput.value.trim(),
      baseUrl: baseUrlInput.value.trim() || "https://api.deepseek.com/v1",
      model: modelInput.value.trim() || "deepseek-chat",
      azureSpeechKey: azureKeyInput.value.trim(),
      azureSpeechRegion: azureRegionInput.value.trim() || "eastasia",
      fishSpeech: gs,
    };
    saveBtn.disabled = true;
    statusEl.textContent = "保存中…";
    try {
      await settingsSet("chatAiConfig", next);
      await settingsSet("assetsDownloadConfig", {
        githubRepo: resRepoInput.value.trim() || assetsStatus.githubRepo,
        releaseTag: resTagInput.value.trim() || "latest",
        downloadSource: assetsStatus.downloadSource || "mirror",
      });
      config = next;
      statusEl.textContent = "已保存并同步主舞台！";
      window.setTimeout(() => {
        statusEl.textContent = "配置保存在本机 SQLite，即时同步主舞台 (Ctrl+S 保存)";
      }, 2200);
    } catch {
      statusEl.textContent = "保存失败，请重试";
    } finally {
      saveBtn.disabled = false;
    }
  }

  document.addEventListener("contextmenu", (e) => {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
    e.preventDefault();
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      void settingsSet("settingsOpen", { open: false }).catch(() => {
        void getCurrentWindow().hide().catch(() => {});
      });
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void saveAll();
    }
  });

  await Promise.all([
    listen<CharState>("pet_char_switched", (event) => {
      applyCharTheme(event.payload.charId);
      selectedCharId = event.payload.charId;
      syncUi();
    }),
    listen<UiAppearance>("pet_appearance", (event) => {
      currentAppearance = applyAppearance(event.payload === "dark" ? "dark" : "light");
      syncThemeBtn();
    }),
    listen<ChatAiConfig>("pet_chat_config", (event) => {
      applyConfigFromBackend(event.payload);
    }),
    listen<boolean>("pet_muted", (event) => {
      mutedBanner.hidden = !Boolean(event.payload);
    }),
    listen<boolean>("pet_auto_start", (event) => {
      autoStartEnabled = Boolean(event.payload);
      syncUi();
    }),
    listen<boolean>("pet_global_hotkey", (event) => {
      globalHotkeyEnabled = Boolean(event.payload);
      syncUi();
    }),
    listen<{ ok: boolean; message: string }>("pet_tts_status", (event) => {
      statusEl.textContent = event.payload.message;
    }),
    listen<VramStatusPayload>("pet_vram_status", (event) => {
      renderVramStatus(event.payload);
    }),
    listen<FishSpeechStatusPayload>("pet_fish_speech_status", (event) => {
      gsStatus = event.payload;
      if (!gsInstallDirInput.value.trim() && event.payload.installDir) {
        gsInstallDirInput.value = event.payload.installDir;
      }
      if (event.payload.error) {
        statusEl.textContent = event.payload.error;
      }
      syncUi();
    }),
    listen<FishSpeechProgressPayload>("pet_fish_speech_progress", (event) => {
      const p = event.payload;
      gsProgressWrap.hidden = !p.active && p.stage === "done";
      gsProgressFill.style.width = `${Math.max(0, Math.min(100, p.percent)).toFixed(1)}%`;
      const speed = p.speedBps > 0 ? ` · ${formatBytes(p.speedBps)}/s` : "";
      const sizeInfo =
        p.totalBytes > 0
          ? ` (${formatBytes(p.downloadedBytes)} / ${formatBytes(p.totalBytes)})`
          : "";
      gsProgressMeta.textContent = `${p.message}${sizeInfo}${speed}`;
      if (p.stage === "error" || p.stage === "cancelled") {
        statusEl.textContent = p.message;
      }
    }),
    listen<{ requestId: string; path: string }>("pet_file_picked", (event) => {
      const cb = pendingPickCallbacks.get(event.payload.requestId);
      if (cb) {
        pendingPickCallbacks.delete(event.payload.requestId);
        cb(event.payload.path);
      }
    }),
    listen<string>("pet_open_settings_tab", (event) => {
      const t = event.payload as SettingsTabId;
      if (t && tabPages.has(t)) {
        switchTab(t);
      }
    }),
    listen<AssetsStatusPayload>("pet_assets_status", (event) => {
      assetsStatus = event.payload;
      renderResourcesUi();
    }),
    listen<AssetsProgressPayload>("pet_assets_progress", (event) => {
      const p = event.payload;
      resProgressWrap.hidden = !p.active && p.stage === "done";
      resProgressFill.style.width = `${Math.max(0, Math.min(100, p.percent)).toFixed(1)}%`;
      const speed = p.speedBps > 0 ? ` · ${formatBytes(p.speedBps)}/s` : "";
      const sizeInfo =
        p.totalBytes > 0
          ? ` (${formatBytes(p.downloadedBytes)} / ${formatBytes(p.totalBytes)})`
          : "";
      resProgressMeta.textContent = `${p.message}${sizeInfo}${speed}`;
      if (p.stage === "error" || p.stage === "cancelled" || p.stage === "done") {
        statusEl.textContent = p.message;
      }
    }),
  ]);

  syncUi();
  await settingsSet("settingsReady", true).catch(() => {});
}
