import {
  setUiHitRects,
  settingsSet,
  type PomodoroStatusPayload,
  type UiHitRect,
  type UnifiedState,
} from "../shared/ipc";
import { createIcon } from "./icons";
import { createStatusIcon } from "./statusIcon";

export type PanelTabId = "status" | "pet" | "album" | "chat";
export type IslandMode = "full" | "minimal";

export interface EdgeDockState {
  enabled: boolean;
  side: "left" | "right" | null;
  visibleLeftPx: number;
  visibleRightPx: number;
}

export interface TelemetryIslandCallbacks {
  onToggleMain: () => void;
  onOpenTab: (tab: PanelTabId) => void;
  onOpenResources?: () => void;
  onRefresh: () => Promise<void>;
  onCopyContinuation: (message: string) => void;
  onModeChange?: (mode: IslandMode) => void;
  onContextMenu?: (clientX: number, clientY: number) => void;
}

const MODE_STORAGE_KEY = "deskpet.islandMode";
const CONTINUATION_PROMPT =
  "请总结当前任务目标、已完成改动、关键文件路径、未解决阻塞点与下一步计划，方便我新开一个干净上下文的 Task 无缝继续。";

const HIT_PAD_PX = 4;
const ARC_RADIUS = 10.5;
const ARC_CIRCUMFERENCE = 2 * Math.PI * ARC_RADIUS;
const ARC_SPAN = ARC_CIRCUMFERENCE * 0.75;
const RECENT_SPEED_HOLD_MS = 10_000;

function clamp01(v: number): number {
  return Math.max(0, Math.min(1, v));
}

function round4(v: number): number {
  return Math.round(v * 10000) / 10000;
}

function rectListsEqual(a: UiHitRect[], b: UiHitRect[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (
      Math.abs(a[i].x - b[i].x) >= 0.0006 ||
      Math.abs(a[i].y - b[i].y) >= 0.0006 ||
      Math.abs(a[i].w - b[i].w) >= 0.0006 ||
      Math.abs(a[i].h - b[i].h) >= 0.0006
    ) {
      return false;
    }
  }
  return true;
}

export function createOfflineTelemetryState(): UnifiedState {
  return {
    source: "antigravity",
    availableSources: ["antigravity"],
    status: "offline",
    statusSinceMs: Date.now(),
    activeStepLabel: null,
    activity: "process_offline",
    activeAgents: 0,
    completedAgents: 0,
    workspaceName: null,
    stepDetail: null,
    capabilities: {
      hasQuota: false,
      hasMultiPool: false,
      hasRealtimeSpeed: false,
      hasTtft: false,
      hasContext: false,
      hasApproval: false,
    },
    model: {
      name: null,
      contextLimit: null,
    },
    quota: {
      remainingPct: null,
      resetAtMs: null,
      pools: [],
      confidence: "unavailable",
    },
    speed: {
      tokensPerSec: null,
      ttftMs: null,
      confidence: "unavailable",
    },
    context: {
      usedTokens: null,
      limitTokens: null,
      usedPct: null,
      deltaTokens: null,
      confidence: "unavailable",
    },
    error: null,
    alerts: {
      contextHigh: false,
      quotaLow: false,
      waitingApproval: false,
      hasError: false,
    },
    updatedAtMs: Date.now(),
    stale: false,
  };
}

function cleanModelName(name: string | null): string | null {
  if (!name) return null;
  const clean = name.replace(/\s+/g, " ").trim();
  return clean || null;
}

function formatTps(tps: number): string {
  if (tps >= 100) return `${Math.round(tps)}`;
  return tps.toFixed(1).replace(/\.0$/, "");
}

function formatTtft(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  const sec = ms / 1000;
  return sec >= 10 ? `${Math.round(sec)}s` : `${sec.toFixed(1)}s`;
}

function formatElapsed(ms: number): string {
  const sec = Math.max(0, Math.floor(ms / 1000));
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  const rem = sec % 60;
  if (min < 60) return `${min}m${String(rem).padStart(2, "0")}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h${String(min % 60).padStart(2, "0")}m`;
}

function formatCountdown(resetAtMs: number | null): string {
  if (!resetAtMs || !Number.isFinite(resetAtMs)) return "";
  const diff = resetAtMs - Date.now();
  if (diff <= 0) return "即将刷新";
  const sec = Math.floor(diff / 1000);
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (d > 0) return `${d}d${h}h`;
  if (h > 0) return `${h}h${String(m).padStart(2, "0")}m`;
  if (m > 0) return `${m}m${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

function formatQuotaCenter(pct: number | null): {
  num: string;
  unit: string;
  compact: boolean;
} {
  if (pct === null || !Number.isFinite(pct)) {
    return { num: "—", unit: "", compact: false };
  }
  const clamped = Math.max(0, Math.min(100, pct));
  if (clamped > 0 && clamped < 1) {
    return { num: "<1", unit: "%", compact: true };
  }
  const rounded = clamped >= 99.95 ? 100 : Math.floor(clamped + 0.5);
  return {
    num: String(rounded),
    unit: "%",
    compact: rounded >= 100,
  };
}

async function copyContinuationText(): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(CONTINUATION_PROMPT);
      return true;
    }
  } catch {
    // fallback below
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = CONTINUATION_PROMPT;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}

function loadSavedMode(): IslandMode {
  try {
    const raw = localStorage.getItem(MODE_STORAGE_KEY);
    if (raw === "minimal" || raw === "full") return raw;
  } catch {
    // ignore
  }
  return "full";
}

/**
 * Deskpet v2.0 精致双形态灵动岛：
 * - 完整态（full · 286×64）：顶栏「状态球 + 模型名 + 醒目实时 t/s 胶囊 + 极简钮」，
 *   底栏「额度条 + 上下文一键续命条 + 3 枚矢量快捷圆钮（工作台/伙伴/刷新）」。
 * - 极简态（minimal · 38px 全圆角胶囊）：状态球 + 动态实时 t/s 槽（吐字时丝滑展宽，空闲优雅收敛）
 *   + 270° 开口额度圆环 + 展开钮。
 */
export class TelemetryIsland {
  private readonly dockEl: HTMLDivElement;
  private readonly callbacks: TelemetryIslandCallbacks;

  // --- 形态 A：完整灵动岛 ---
  private readonly fullCardEl: HTMLDivElement;
  private readonly fullIcon = createStatusIcon("sm");
  private readonly fullModelEl: HTMLSpanElement;
  private readonly fullPomoChipEl: HTMLButtonElement;
  private readonly fullPomoTextEl: HTMLSpanElement;
  private readonly fullSpeedChipEl: HTMLSpanElement;
  private readonly fullWaveEl: HTMLSpanElement;
  private readonly fullSpeedTextEl: HTMLSpanElement;

  private readonly metersWrapEl: HTMLDivElement;
  private readonly quotaRowEl: HTMLDivElement;
  private readonly quotaPctEl: HTMLSpanElement;
  private readonly quotaCountdownEl: HTMLSpanElement;
  private readonly quotaFillEl: HTMLDivElement;

  private readonly ctxRowEl: HTMLButtonElement;
  private readonly ctxPctEl: HTMLSpanElement;
  private readonly ctxCopiedIconEl: HTMLSpanElement;
  private readonly ctxFillEl: HTMLDivElement;
  private readonly offlineHintEl: HTMLSpanElement;

  private readonly chatBtn: HTMLButtonElement;
  private readonly pomodoroBtn: HTMLButtonElement;
  private readonly refreshBtn: HTMLButtonElement;

  // --- 形态 B：极简悬浮胶囊 ---
  private readonly miniShellEl: HTMLDivElement;
  private readonly miniOrbBtn: HTMLButtonElement;
  private readonly miniIcon = createStatusIcon("md");
  private readonly miniLiveSlotEl: HTMLSpanElement;
  private readonly miniWaveEl: HTMLSpanElement;
  private readonly miniLiveTextEl: HTMLSpanElement;
  private readonly miniPomoChipEl: HTMLButtonElement;
  private readonly miniPomoTextEl: HTMLSpanElement;
  private readonly miniQuotaBtn: HTMLButtonElement;
  private readonly miniQuotaArcEl: SVGCircleElement;
  private readonly miniQuotaNumEl: HTMLSpanElement;
  private readonly miniQuotaUnitEl: HTMLSpanElement;
  private readonly missingAssetsBtn: HTMLButtonElement;

  private readonly resizeObserver: ResizeObserver;

  private state: UnifiedState = createOfflineTelemetryState();
  private mode: IslandMode = loadSavedMode();
  private visible = true;
  private refreshing = false;
  private tickTimerId = 0;
  private recentFadeTimerId = 0;
  private copyResetTimerId = 0;
  private edgeHitTimerId = 0;
  private rafReportId = 0;
  private lastSentRects: UiHitRect[] = [];

  private lastKnownTps: number | null = null;
  private lastStreamEndedAtMs = 0;
  private wasStreaming = false;

  constructor(container: HTMLElement, callbacks: TelemetryIslandCallbacks) {
    this.callbacks = callbacks;

    this.dockEl = document.createElement("div");
    this.dockEl.className = "island-dock";
    this.dockEl.dataset.mode = this.mode;
    this.dockEl.dataset.edgeDock = "none";
    this.dockEl.dataset.open = "false";
    this.dockEl.addEventListener("pointerdown", (e) => e.stopPropagation());
    this.dockEl.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.callbacks.onContextMenu?.(e.clientX, e.clientY);
    });

    /* ==================== 形态 A：完整灵动岛 ==================== */
    this.fullCardEl = document.createElement("div");
    this.fullCardEl.className = "island-full-card";
    this.fullCardEl.dataset.status = "offline";
    this.fullCardEl.dataset.alert = "none";

    // Row 1：状态球 + 模型名 + 醒目实时速度胶囊 + 极简切态钮
    const header = document.createElement("div");
    header.className = "if-header";

    const statusTrigger = document.createElement("button");
    statusTrigger.type = "button";
    statusTrigger.className = "if-status-trigger";
    statusTrigger.title = "点击展开监控工作台";

    this.fullModelEl = document.createElement("span");
    this.fullModelEl.className = "if-model-name";
    this.fullModelEl.textContent = "等待 Antigravity";

    statusTrigger.append(this.fullIcon.el, this.fullModelEl);
    statusTrigger.addEventListener("click", (e) => {
      e.stopPropagation();
      this.callbacks.onToggleMain();
    });

    const rightHead = document.createElement("div");
    rightHead.className = "if-header-right";

    this.fullPomoChipEl = document.createElement("button");
    this.fullPomoChipEl.type = "button";
    this.fullPomoChipEl.className = "if-pomo-pill";
    this.fullPomoChipEl.hidden = true;
    this.fullPomoChipEl.title = "番茄钟（左键暂停/继续，右键重置）";
    this.fullPomoChipEl.appendChild(createIcon("timer", 10));
    this.fullPomoTextEl = document.createElement("span");
    this.fullPomoTextEl.textContent = "25:00";
    this.fullPomoChipEl.appendChild(this.fullPomoTextEl);
    this.fullPomoChipEl.addEventListener("click", (e) => {
      e.stopPropagation();
      void settingsSet("pomodoroControl", { action: "toggle" }).catch(() => {});
    });
    this.fullPomoChipEl.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void settingsSet("pomodoroControl", { action: "reset" }).catch(() => {});
    });

    this.fullSpeedChipEl = document.createElement("span");
    this.fullSpeedChipEl.className = "if-speed-chip";
    this.fullSpeedChipEl.dataset.tone = "offline";

    this.fullWaveEl = document.createElement("span");
    this.fullWaveEl.className = "mini-eq-wave";
    this.fullWaveEl.hidden = true;
    this.fullWaveEl.innerHTML = "<i></i><i></i><i></i>";

    this.fullSpeedTextEl = document.createElement("span");
    this.fullSpeedTextEl.className = "if-speed-text";
    this.fullSpeedTextEl.textContent = "未连接";

    this.fullSpeedChipEl.append(this.fullWaveEl, this.fullSpeedTextEl);

    const toMiniBtn = document.createElement("button");
    toMiniBtn.type = "button";
    toMiniBtn.className = "if-mode-btn";
    toMiniBtn.title = "收起为极简胶囊";
    toMiniBtn.setAttribute("aria-label", "收起为极简胶囊");
    toMiniBtn.appendChild(createIcon("minimize", 12));
    toMiniBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.setMode("minimal");
    });

    rightHead.append(this.fullPomoChipEl, this.fullSpeedChipEl, toMiniBtn);
    header.append(statusTrigger, rightHead);

    // Row 2：双列微渐变仪表 + 3 枚矢量圆钮
    const bottomRow = document.createElement("div");
    bottomRow.className = "if-bottom-row";

    this.metersWrapEl = document.createElement("div");
    this.metersWrapEl.className = "if-meters-grid";

    this.quotaRowEl = document.createElement("div");
    this.quotaRowEl.className = "if-meter-block";
    this.quotaRowEl.title = "点击查看全部模型池余量";
    this.quotaRowEl.addEventListener("click", (e) => {
      e.stopPropagation();
      this.callbacks.onOpenTab("status");
    });

    const quotaTop = document.createElement("div");
    quotaTop.className = "if-meter-head";
    const quotaLbl = document.createElement("span");
    quotaLbl.className = "if-meter-label";
    quotaLbl.textContent = "额度";
    const quotaVals = document.createElement("span");
    quotaVals.className = "if-meter-vals";
    this.quotaPctEl = document.createElement("span");
    this.quotaPctEl.className = "if-meter-pct";
    this.quotaPctEl.textContent = "—";
    this.quotaCountdownEl = document.createElement("span");
    this.quotaCountdownEl.className = "if-meter-sub";
    quotaVals.append(this.quotaPctEl, this.quotaCountdownEl);
    quotaTop.append(quotaLbl, quotaVals);

    const quotaTrack = document.createElement("div");
    quotaTrack.className = "if-meter-track";
    this.quotaFillEl = document.createElement("div");
    this.quotaFillEl.className = "if-meter-fill is-quota";
    quotaTrack.appendChild(this.quotaFillEl);
    this.quotaRowEl.append(quotaTop, quotaTrack);

    this.ctxRowEl = document.createElement("button");
    this.ctxRowEl.type = "button";
    this.ctxRowEl.className = "if-meter-block is-clickable";
    this.ctxRowEl.title = "点击一键复制「续命交接提示词」";
    this.ctxRowEl.addEventListener("click", (e) => {
      e.stopPropagation();
      void copyContinuationText().then((ok) => {
        if (ok) {
          this.flashCopiedState();
        }
        this.callbacks.onCopyContinuation(
          ok ? "已复制续命提示词，可直接开新对话续接～" : "复制失败，请稍后重试",
        );
      });
    });

    const ctxTop = document.createElement("div");
    ctxTop.className = "if-meter-head";
    const ctxLbl = document.createElement("span");
    ctxLbl.className = "if-meter-label";
    ctxLbl.textContent = "上下文";
    const ctxVals = document.createElement("span");
    ctxVals.className = "if-meter-vals";
    this.ctxCopiedIconEl = document.createElement("span");
    this.ctxCopiedIconEl.className = "if-copied-badge";
    this.ctxCopiedIconEl.hidden = true;
    this.ctxCopiedIconEl.appendChild(createIcon("check", 10));
    this.ctxPctEl = document.createElement("span");
    this.ctxPctEl.className = "if-meter-pct";
    this.ctxPctEl.textContent = "—";
    ctxVals.append(this.ctxCopiedIconEl, this.ctxPctEl);
    ctxTop.append(ctxLbl, ctxVals);

    const ctxTrack = document.createElement("div");
    ctxTrack.className = "if-meter-track";
    this.ctxFillEl = document.createElement("div");
    this.ctxFillEl.className = "if-meter-fill is-context";
    ctxTrack.appendChild(this.ctxFillEl);
    this.ctxRowEl.append(ctxTop, ctxTrack);

    this.offlineHintEl = document.createElement("span");
    this.offlineHintEl.className = "if-offline-hint";
    this.offlineHintEl.textContent = "打开 IDE 自动同步状态与 t/s";
    this.offlineHintEl.hidden = true;

    this.metersWrapEl.append(this.quotaRowEl, this.ctxRowEl, this.offlineHintEl);

    const actionsGroup = document.createElement("div");
    actionsGroup.className = "if-quick-icons";

    this.chatBtn = this.makeMicroIconBtn(
      "快捷闲聊 (Alt + Space)",
      "message",
      () => this.callbacks.onOpenTab("chat"),
    );
    this.pomodoroBtn = this.makeMicroIconBtn(
      "番茄钟专注计时（左键开始/暂停，右键重置）",
      "timer",
      () => {
        void settingsSet("pomodoroControl", { action: "toggle" }).catch(() => {});
      },
    );
    this.pomodoroBtn.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void settingsSet("pomodoroControl", { action: "reset" }).catch(() => {});
    });
    const petBtn = this.makeMicroIconBtn(
      "切换 8 位伙伴与养成",
      "users",
      () => this.callbacks.onOpenTab("pet"),
    );
    this.refreshBtn = this.makeMicroIconBtn(
      "立即强刷最新遥测数据",
      "refresh",
      () => {
        if (this.refreshing) return;
        this.refreshing = true;
        this.refreshBtn.classList.add("is-spinning");
        void this.callbacks.onRefresh().finally(() => {
          this.refreshing = false;
          this.refreshBtn.classList.remove("is-spinning");
        });
      },
    );

    actionsGroup.append(this.chatBtn, this.pomodoroBtn, petBtn, this.refreshBtn);
    bottomRow.append(this.metersWrapEl, actionsGroup);
    this.fullCardEl.append(header, bottomRow);

    /* ==================== 形态 B：极简悬浮胶囊 ==================== */
    this.miniShellEl = document.createElement("div");
    this.miniShellEl.className = "island-mini-shell";
    this.miniShellEl.dataset.status = "offline";
    this.miniShellEl.dataset.alert = "none";

    this.miniOrbBtn = document.createElement("button");
    this.miniOrbBtn.type = "button";
    this.miniOrbBtn.className = "im-orb-btn";
    this.miniOrbBtn.title = "点击展开监控工作台";
    this.miniOrbBtn.setAttribute("aria-label", "展开监控工作台");

    this.miniLiveSlotEl = document.createElement("span");
    this.miniLiveSlotEl.className = "im-live-slot";
    this.miniLiveSlotEl.hidden = true;

    this.miniWaveEl = document.createElement("span");
    this.miniWaveEl.className = "mini-eq-wave is-compact";
    this.miniWaveEl.hidden = true;
    this.miniWaveEl.innerHTML = "<i></i><i></i><i></i>";

    this.miniLiveTextEl = document.createElement("span");
    this.miniLiveTextEl.className = "im-live-text";

    this.miniLiveSlotEl.append(this.miniWaveEl, this.miniLiveTextEl);
    this.miniOrbBtn.append(this.miniIcon.el, this.miniLiveSlotEl);
    this.miniOrbBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.callbacks.onToggleMain();
    });

    this.miniQuotaBtn = document.createElement("button");
    this.miniQuotaBtn.type = "button";
    this.miniQuotaBtn.className = "im-quota-btn";
    this.miniQuotaBtn.dataset.alert = "none";
    this.miniQuotaBtn.title = "当前池余量 · 点击查看详情";
    this.miniQuotaBtn.setAttribute("aria-label", "查看模型额度详情");

    const svgNs = "http://www.w3.org/2000/svg";
    const ringSvg = document.createElementNS(svgNs, "svg");
    ringSvg.setAttribute("viewBox", "0 0 28 28");
    ringSvg.setAttribute("class", "im-quota-svg");
    ringSvg.setAttribute("aria-hidden", "true");

    const track = document.createElementNS(svgNs, "circle");
    track.setAttribute("cx", "14");
    track.setAttribute("cy", "14");
    track.setAttribute("r", String(ARC_RADIUS));
    track.setAttribute("class", "island-quota-track");
    track.setAttribute(
      "stroke-dasharray",
      `${ARC_SPAN.toFixed(2)} ${ARC_CIRCUMFERENCE.toFixed(2)}`,
    );
    track.setAttribute("transform", "rotate(135 14 14)");

    this.miniQuotaArcEl = document.createElementNS(svgNs, "circle");
    this.miniQuotaArcEl.setAttribute("cx", "14");
    this.miniQuotaArcEl.setAttribute("cy", "14");
    this.miniQuotaArcEl.setAttribute("r", String(ARC_RADIUS));
    this.miniQuotaArcEl.setAttribute("class", "island-quota-arc");
    this.miniQuotaArcEl.setAttribute(
      "stroke-dasharray",
      `0 ${ARC_CIRCUMFERENCE.toFixed(2)}`,
    );
    this.miniQuotaArcEl.setAttribute("transform", "rotate(135 14 14)");

    ringSvg.append(track, this.miniQuotaArcEl);

    const quotaCenter = document.createElement("span");
    quotaCenter.className = "im-quota-label";
    this.miniQuotaNumEl = document.createElement("span");
    this.miniQuotaNumEl.className = "im-quota-num";
    this.miniQuotaNumEl.textContent = "—";
    this.miniQuotaUnitEl = document.createElement("span");
    this.miniQuotaUnitEl.className = "im-quota-unit";
    quotaCenter.append(this.miniQuotaNumEl, this.miniQuotaUnitEl);

    this.miniQuotaBtn.append(ringSvg, quotaCenter);
    this.miniQuotaBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.callbacks.onOpenTab("status");
    });

    const toFullBtn = document.createElement("button");
    toFullBtn.type = "button";
    toFullBtn.className = "im-expand-btn";
    toFullBtn.title = "展开为完整灵动岛";
    toFullBtn.setAttribute("aria-label", "展开为完整灵动岛");
    toFullBtn.appendChild(createIcon("maximize", 12));
    toFullBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.setMode("full");
    });

    this.miniPomoChipEl = document.createElement("button");
    this.miniPomoChipEl.type = "button";
    this.miniPomoChipEl.className = "im-pomo-pill";
    this.miniPomoChipEl.hidden = true;
    this.miniPomoChipEl.title = "番茄钟（左键暂停/继续，右键重置）";
    this.miniPomoChipEl.appendChild(createIcon("timer", 10));
    this.miniPomoTextEl = document.createElement("span");
    this.miniPomoTextEl.textContent = "25:00";
    this.miniPomoChipEl.appendChild(this.miniPomoTextEl);
    this.miniPomoChipEl.addEventListener("click", (e) => {
      e.stopPropagation();
      void settingsSet("pomodoroControl", { action: "toggle" }).catch(() => {});
    });
    this.miniPomoChipEl.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void settingsSet("pomodoroControl", { action: "reset" }).catch(() => {});
    });

    this.miniShellEl.append(this.miniOrbBtn, this.miniPomoChipEl, this.miniQuotaBtn, toFullBtn);

    this.missingAssetsBtn = document.createElement("button");
    this.missingAssetsBtn.type = "button";
    this.missingAssetsBtn.className = "island-missing-btn";
    this.missingAssetsBtn.hidden = true;
    this.missingAssetsBtn.appendChild(createIcon("download", 12));
    const missingLbl = document.createElement("span");
    missingLbl.textContent = "未检测到角色动画 · 点击前往下载";
    this.missingAssetsBtn.appendChild(missingLbl);
    this.missingAssetsBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.callbacks.onOpenResources?.();
    });

    this.dockEl.append(this.fullCardEl, this.miniShellEl, this.missingAssetsBtn);
    container.appendChild(this.dockEl);

    this.resizeObserver = new ResizeObserver(() => {
      this.scheduleHitRectReport();
    });
    this.resizeObserver.observe(this.dockEl);
    this.resizeObserver.observe(this.fullCardEl);
    this.resizeObserver.observe(this.miniShellEl);
    window.addEventListener("resize", this.handleResize);

    this.applyModeVisibility();
    this.render();
    this.scheduleHitRectReport();
  }

  private makeMicroIconBtn(
    title: string,
    iconName: Parameters<typeof createIcon>[0],
    onClick: () => void,
  ): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "if-micro-btn";
    btn.title = title;
    btn.setAttribute("aria-label", title);
    btn.appendChild(createIcon(iconName, 12));
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      onClick();
    });
    return btn;
  }

  private flashCopiedState(): void {
    if (this.copyResetTimerId !== 0) {
      window.clearTimeout(this.copyResetTimerId);
    }
    this.ctxRowEl.dataset.copied = "true";
    this.ctxCopiedIconEl.hidden = false;
    this.copyResetTimerId = window.setTimeout(() => {
      this.copyResetTimerId = 0;
      this.ctxRowEl.dataset.copied = "false";
      this.ctxCopiedIconEl.hidden = true;
    }, 1800);
  }

  private handleResize = (): void => {
    this.scheduleHitRectReport();
  };

  public getMode(): IslandMode {
    return this.mode;
  }

  public setMode(mode: IslandMode): void {
    if (this.mode === mode) return;
    this.mode = mode;
    this.dockEl.dataset.mode = mode;
    try {
      localStorage.setItem(MODE_STORAGE_KEY, mode);
    } catch {
      // ignore
    }
    this.applyModeVisibility();
    this.syncTickTimer();
    this.render();
    this.callbacks.onModeChange?.(mode);
    this.scheduleHitRectReport();
  }

  public toggleMode(): void {
    this.setMode(this.mode === "full" ? "minimal" : "full");
  }

  public setEdgeDock(dock: EdgeDockState): void {
    this.dockEl.dataset.edgeDock = dock.side ?? "none";
    if (!dock.side) {
      this.dockEl.style.left = "";
      this.dockEl.style.right = "";
      this.fullCardEl.style.removeProperty("--island-edge-max-w");
    } else {
      const visLeft = Math.max(0, dock.visibleLeftPx);
      const visRight = Math.max(visLeft, dock.visibleRightPx);
      const avail = Math.max(176, visRight - visLeft - 18);
      this.fullCardEl.style.setProperty("--island-edge-max-w", `${Math.round(avail)}px`);
      if (dock.side === "left") {
        this.dockEl.style.left = `${Math.round(visLeft + 9)}px`;
        this.dockEl.style.right = "auto";
      } else {
        const vw = window.innerWidth || 640;
        const rightOffset = Math.max(9, vw - visRight + 9);
        this.dockEl.style.left = "auto";
        this.dockEl.style.right = `${Math.round(rightOffset)}px`;
      }
    }
    this.scheduleHitRectReport();
    if (this.edgeHitTimerId !== 0) {
      window.clearTimeout(this.edgeHitTimerId);
    }
    this.edgeHitTimerId = window.setTimeout(() => {
      this.edgeHitTimerId = 0;
      this.reportHitRects(true);
    }, 220);
  }

  private applyModeVisibility(): void {
    this.fullCardEl.hidden = this.mode !== "full";
    this.miniShellEl.hidden = this.mode !== "minimal";
  }

  public setState(next: UnifiedState): void {
    const isNowStreaming = next.status === "streaming";
    if (
      isNowStreaming &&
      next.speed.confidence !== "unavailable" &&
      next.speed.tokensPerSec !== null &&
      next.speed.tokensPerSec > 0
    ) {
      this.lastKnownTps = next.speed.tokensPerSec;
    }

    if (this.wasStreaming && !isNowStreaming) {
      this.lastStreamEndedAtMs = Date.now();
      this.scheduleRecentFade();
    } else if (isNowStreaming && this.recentFadeTimerId !== 0) {
      window.clearTimeout(this.recentFadeTimerId);
      this.recentFadeTimerId = 0;
    }

    if (next.status === "offline") {
      this.lastKnownTps = null;
      this.lastStreamEndedAtMs = 0;
    }

    this.wasStreaming = isNowStreaming;
    this.state = next;
    this.syncTickTimer();
    this.render();
    this.scheduleHitRectReport();
  }

  private scheduleRecentFade(): void {
    if (this.recentFadeTimerId !== 0) {
      window.clearTimeout(this.recentFadeTimerId);
    }
    this.recentFadeTimerId = window.setTimeout(() => {
      this.recentFadeTimerId = 0;
      this.render();
      this.scheduleHitRectReport();
    }, RECENT_SPEED_HOLD_MS + 40);
  }

  public setPanelOpen(open: boolean): void {
    this.dockEl.dataset.open = open ? "true" : "false";
    this.chatBtn.dataset.active = open ? "true" : "false";
  }

  public setPomodoro(next: PomodoroStatusPayload): void {
    this.pomodoroBtn.dataset.active = next.active ? "true" : "false";
    const mm = String(Math.floor(next.remainingSec / 60)).padStart(2, "0");
    const ss = String(next.remainingSec % 60).padStart(2, "0");
    const timeStr = `${mm}:${ss}`;
    this.fullPomoChipEl.hidden = !next.active;
    this.fullPomoChipEl.dataset.paused = next.paused ? "true" : "false";
    this.fullPomoTextEl.textContent = timeStr;
    this.miniPomoChipEl.hidden = !next.active;
    this.miniPomoChipEl.dataset.paused = next.paused ? "true" : "false";
    this.miniPomoTextEl.textContent = timeStr;
    this.render();
    this.scheduleHitRectReport();
  }

  public isVisible(): boolean {
    return this.visible;
  }

  public setVisible(visible: boolean): void {
    if (this.visible === visible) return;
    this.visible = visible;
    this.dockEl.hidden = !visible;
    this.syncTickTimer();
    this.reportHitRects(true);
  }

  public setMissingAssets(missing: boolean): void {
    if (this.missingAssetsBtn.hidden === !missing) return;
    this.missingAssetsBtn.hidden = !missing;
    if (missing) {
      this.setVisible(true);
    }
    this.scheduleHitRectReport();
  }

  private syncTickTimer(): void {
    const needTick =
      this.visible &&
      (this.state.status === "tool_running" ||
        (this.mode === "full" && this.state.quota.resetAtMs !== null));
    if (needTick && this.tickTimerId === 0) {
      this.tickTimerId = window.setInterval(() => {
        this.renderDynamicTimers();
      }, 1000);
    } else if (!needTick && this.tickTimerId !== 0) {
      window.clearInterval(this.tickTimerId);
      this.tickTimerId = 0;
    }
  }

  private renderDynamicTimers(): void {
    const s = this.state;
    const suffix = s.activeAgents >= 2 ? ` ×${s.activeAgents}` : "";
    if (s.status === "tool_running") {
      const label = (s.activeStepLabel ?? "").trim() || "执行工具";
      const elapsed = formatElapsed(Date.now() - s.statusSinceMs);
      const text = `${label} ${elapsed}${suffix}`;
      this.fullSpeedTextEl.textContent = text;
      this.miniLiveTextEl.textContent = text;
    }
    if (s.quota.resetAtMs) {
      const cd = formatCountdown(s.quota.resetAtMs);
      this.quotaCountdownEl.textContent = cd ? `· ${cd}` : "";
    }
  }

  private formatErrorTag(s: UnifiedState): string {
    const code = s.error?.code;
    if (s.activity === "service_reconnecting" || s.error?.kind === "network") {
      return "重连中";
    }
    if (s.activity === "terminal_error") {
      return "命令报错";
    }
    if (s.error?.kind === "rate_limit") {
      return code ? `${code} 限流` : "限流";
    }
    if (s.error?.kind === "auth") {
      return code ? `${code} 登录过期` : "登录过期";
    }
    if (s.error?.kind === "region") {
      return code ? `${code} 区域受限` : "区域受限";
    }
    return code ? `${code} 异常` : "步骤失败";
  }

  private render(): void {
    const s = this.state;
    const isReconnecting = s.activity === "service_reconnecting";
    const hasDanger =
      s.status === "error" || s.alerts.hasError || s.alerts.quotaLow;
    const hasWarn =
      !hasDanger &&
      (s.alerts.contextHigh || s.alerts.waitingApproval || isReconnecting);
    const alertLevel = hasDanger ? "danger" : hasWarn ? "warn" : "none";

    const liveTps =
      s.status === "streaming" &&
      s.speed.confidence !== "unavailable" &&
      s.speed.tokensPerSec !== null &&
      s.speed.tokensPerSec > 0
        ? s.speed.tokensPerSec
        : null;

    const speedRatio = liveTps !== null
      ? Math.min(1, liveTps / 90)
      : s.status === "streaming"
        ? 0.45
        : 0;

    this.fullCardEl.dataset.status = s.status;
    this.fullCardEl.dataset.alert = alertLevel;
    this.miniShellEl.dataset.status = s.status;
    this.miniShellEl.dataset.alert = alertLevel;

    this.fullIcon.setStatus(s.status);
    this.fullIcon.setSpeedRatio(speedRatio);
    this.miniIcon.setStatus(s.status);
    this.miniIcon.setSpeedRatio(speedRatio);

    const waveDur = `${(0.92 - speedRatio * 0.44).toFixed(2)}s`;
    this.fullWaveEl.style.setProperty("--wave-dur", waveDur);
    this.miniWaveEl.style.setProperty("--wave-dur", waveDur);

    const isStreaming = s.status === "streaming";
    this.fullWaveEl.hidden = !isStreaming;
    this.miniWaveEl.hidden = !isStreaming;

    // 1. 顶栏模型名
    const modelName = cleanModelName(s.model.name);
    this.fullModelEl.textContent =
      modelName ?? (s.status === "offline" ? "等待 Antigravity" : "已连接");
    this.fullModelEl.title = s.workspaceName
      ? `${modelName ?? "Antigravity"} · ${s.workspaceName}`
      : (modelName ?? "");

    // 2. 醒目实时速度 / 活动胶囊（完整态 + 极简态联动）
    let chipText = "未连接";
    let chipTone: "stream" | "tool" | "warn" | "danger" | "idle" | "offline" = "offline";
    let miniSlotVisible = false;
    let miniSlotText = "";
    let miniSlotStale = false;
    const swarmSuffix = s.activeAgents >= 2 ? ` ×${s.activeAgents}` : "";

    switch (s.status) {
      case "offline":
        chipText = "未连接";
        chipTone = "offline";
        break;
      case "idle":
        if (isReconnecting) {
          chipTone = "warn";
          chipText = "重连中";
          miniSlotVisible = true;
          miniSlotText = "重连中";
        } else if (s.activity === "quota_exhausted") {
          chipTone = "warn";
          chipText = "额度冷却";
          miniSlotVisible = true;
          miniSlotText = "额度冷却";
        } else {
          chipTone = "idle";
          if (this.lastKnownTps !== null) {
            chipText = `上次 ${formatTps(this.lastKnownTps)} t/s`;
            if (Date.now() - this.lastStreamEndedAtMs < RECENT_SPEED_HOLD_MS) {
              miniSlotVisible = true;
              miniSlotText = `${formatTps(this.lastKnownTps)} t/s`;
              miniSlotStale = true;
            }
          } else {
            chipText = "0 t/s";
          }
        }
        break;
      case "streaming":
        chipTone = "stream";
        miniSlotVisible = true;
        if (liveTps !== null) {
          const est = s.speed.confidence === "estimated" ? "~" : "";
          const label = `${est}${formatTps(liveTps)} t/s${swarmSuffix}`;
          chipText = label;
          miniSlotText = label;
        } else if (s.activity === "deep_reasoning" || s.activity === "ttft_waiting") {
          chipText = `深度思考${swarmSuffix}`;
          miniSlotText = `思考中${swarmSuffix}`;
        } else {
          chipText = `生成中${swarmSuffix}`;
          miniSlotText = `生成中${swarmSuffix}`;
        }
        break;
      case "tool_running": {
        const elapsed = formatElapsed(Date.now() - s.statusSinceMs);
        let label = (s.activeStepLabel ?? "").trim() || "执行工具";
        if (s.activity === "mcp_tool_calling" && s.stepDetail) {
          label = `MCP ${s.stepDetail}`;
        } else if (s.activity === "build_long_running") {
          label = "长耗时";
        } else if (s.activity === "browser_running") {
          label = "浏览器";
        }
        chipTone =
          s.activity === "terminal_error" || s.activity === "build_long_running"
            ? "warn"
            : "tool";
        chipText = `${label} ${elapsed}${swarmSuffix}`;
        miniSlotVisible = true;
        miniSlotText = `${label} ${elapsed}${swarmSuffix}`;
        break;
      }
      case "waiting_approval": {
        chipTone = "warn";
        const waitText =
          s.activity === "approval_outside_dir"
            ? "外部权限"
            : s.activity === "artifact_review_ready"
              ? "待审阅"
              : "待确认";
        chipText = `${waitText}${swarmSuffix}`;
        miniSlotVisible = true;
        miniSlotText = waitText;
        break;
      }
      case "error": {
        const errTag = this.formatErrorTag(s);
        chipTone = isReconnecting ? "warn" : "danger";
        chipText = errTag;
        miniSlotVisible = true;
        miniSlotText = errTag;
        break;
      }
    }

    this.fullSpeedChipEl.dataset.tone = chipTone;
    this.fullSpeedTextEl.textContent = chipText;
    const ttftText = s.speed.ttftMs && s.speed.ttftMs > 0 ? `首字延迟 ${formatTtft(s.speed.ttftMs)}` : "";
    this.fullSpeedChipEl.title = ttftText || chipText;

    this.miniLiveSlotEl.hidden = !miniSlotVisible;
    this.miniLiveSlotEl.dataset.tone = chipTone;
    this.miniLiveSlotEl.dataset.stale = miniSlotStale ? "true" : "false";
    this.miniLiveTextEl.textContent = miniSlotText;

    // 3. 剩余额度条 + 极简 270° 圆环
    const quotaPct =
      s.quota.confidence !== "unavailable" && s.quota.remainingPct !== null
        ? Math.max(0, Math.min(100, s.quota.remainingPct))
        : null;
    const quotaLow = quotaPct !== null && quotaPct <= 10;
    const quotaMid = !quotaLow && quotaPct !== null && quotaPct <= 25;
    const quotaAlert = quotaLow ? "danger" : quotaMid ? "warn" : "none";

    if (quotaPct !== null) {
      this.quotaRowEl.hidden = false;
      const pctText =
        quotaPct > 0 && quotaPct < 1
          ? "<1%"
          : `${quotaPct >= 99.95 ? 100 : Math.round(quotaPct)}%`;
      this.quotaPctEl.textContent = pctText;
      const cd = formatCountdown(s.quota.resetAtMs);
      this.quotaCountdownEl.textContent = cd ? `· ${cd}` : "";
      this.quotaFillEl.style.width = `${Math.max(3, quotaPct).toFixed(1)}%`;
      this.quotaFillEl.dataset.alert = quotaAlert;
    } else {
      this.quotaRowEl.hidden = s.status === "offline";
      this.quotaPctEl.textContent = "—";
      this.quotaCountdownEl.textContent = "";
      this.quotaFillEl.style.width = "0%";
      this.quotaFillEl.dataset.alert = "none";
    }

    const fmt = formatQuotaCenter(quotaPct);
    this.miniQuotaNumEl.textContent = fmt.num;
    this.miniQuotaNumEl.dataset.compact = fmt.compact ? "true" : "false";
    this.miniQuotaUnitEl.textContent = fmt.unit;
    const frac = quotaPct !== null ? quotaPct / 100 : 0;
    this.miniQuotaArcEl.setAttribute(
      "stroke-dasharray",
      `${(ARC_SPAN * frac).toFixed(2)} ${ARC_CIRCUMFERENCE.toFixed(2)}`,
    );
    this.miniQuotaBtn.dataset.alert = quotaAlert;
    this.miniQuotaBtn.title =
      quotaPct !== null
        ? `${modelName ?? "Antigravity"} · 剩余额度 ${Math.round(quotaPct)}%`
        : "模型额度 · 点击查看详情";

    this.miniOrbBtn.title =
      `${modelName ?? "Antigravity"} · ${chipText}` +
      (ttftText ? ` · ${ttftText}` : "") +
      "（点击展开监控工作台）";

    // 4. 上下文条 + 80% 警戒
    const ctxPct =
      s.context.confidence !== "unavailable" && s.context.usedPct !== null
        ? Math.max(0, Math.min(100, s.context.usedPct))
        : null;
    if (ctxPct !== null) {
      this.ctxRowEl.hidden = false;
      this.ctxPctEl.textContent = `${Math.round(ctxPct)}%`;
      this.ctxFillEl.style.width = `${Math.max(3, ctxPct).toFixed(1)}%`;
      this.ctxFillEl.dataset.alert =
        ctxPct >= 80 ? "danger" : ctxPct >= 60 ? "warn" : "none";
      this.ctxRowEl.dataset.alert = ctxPct >= 80 ? "danger" : "none";
    } else {
      this.ctxRowEl.hidden = s.status === "offline";
      this.ctxPctEl.textContent = "—";
      this.ctxFillEl.style.width = "0%";
      this.ctxFillEl.dataset.alert = "none";
      this.ctxRowEl.dataset.alert = "none";
    }

    const bothHidden = this.quotaRowEl.hidden && this.ctxRowEl.hidden;
    this.offlineHintEl.hidden = !bothHidden;
  }

  private scheduleHitRectReport(): void {
    if (this.rafReportId !== 0) return;
    this.rafReportId = window.requestAnimationFrame(() => {
      this.rafReportId = 0;
      this.reportHitRects(false);
    });
  }

  public reportHitRects(force = false): void {
    if (!this.visible || this.dockEl.hidden) {
      if (force || this.lastSentRects.length > 0) {
        this.lastSentRects = [];
        setUiHitRects([]).catch(() => {});
      }
      return;
    }

    const vw = window.innerWidth;
    const vh = window.innerHeight;
    if (vw <= 0 || vh <= 0) return;

    const activeEl = this.mode === "full" ? this.fullCardEl : this.miniShellEl;
    const rects: UiHitRect[] = [];
    if (!activeEl.hidden) {
      const box = activeEl.getBoundingClientRect();
      if (box.width > 0 && box.height > 0) {
        const x1 = clamp01((box.left - HIT_PAD_PX) / vw);
        const y1 = clamp01((box.top - HIT_PAD_PX) / vh);
        const x2 = clamp01((box.right + HIT_PAD_PX) / vw);
        const y2 = clamp01((box.bottom + HIT_PAD_PX) / vh);
        const w = x2 - x1;
        const h = y2 - y1;
        if (w > 0.001 && h > 0.001) {
          rects.push({
            x: round4(x1),
            y: round4(y1),
            w: round4(w),
            h: round4(h),
          });
        }
      }
    }

    if (!force && rectListsEqual(rects, this.lastSentRects)) {
      return;
    }
    this.lastSentRects = rects;
    setUiHitRects(rects).catch(() => {});
  }

  public destroy(): void {
    if (this.tickTimerId !== 0) {
      window.clearInterval(this.tickTimerId);
      this.tickTimerId = 0;
    }
    if (this.recentFadeTimerId !== 0) {
      window.clearTimeout(this.recentFadeTimerId);
      this.recentFadeTimerId = 0;
    }
    if (this.copyResetTimerId !== 0) {
      window.clearTimeout(this.copyResetTimerId);
      this.copyResetTimerId = 0;
    }
    if (this.edgeHitTimerId !== 0) {
      window.clearTimeout(this.edgeHitTimerId);
      this.edgeHitTimerId = 0;
    }
    if (this.rafReportId !== 0) {
      window.cancelAnimationFrame(this.rafReportId);
      this.rafReportId = 0;
    }
    this.resizeObserver.disconnect();
    window.removeEventListener("resize", this.handleResize);
    setUiHitRects([]).catch(() => {});
    this.dockEl.remove();
  }
}
