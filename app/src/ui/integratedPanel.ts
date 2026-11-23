import {
  settingsSet,
  type CatalogChar,
  type CharState,
  type ChatAiConfig,
  type ChatLogMessage,
  type ClipMeta,
  type PetStats,
  type PomodoroStatusPayload,
  type TelemetryError,
  type TelemetryQuotaPool,
  type UnifiedState,
} from "../shared/ipc";
import { createPixelAvatar } from "./avatars";
import { CONTRACT_CATEGORIES, getCategoryLabel } from "./categories";
import { createIcon, type IconName } from "./icons";
import { getCharMeta } from "./personas";
import { createStatusIcon } from "./statusIcon";
import {
  createOfflineTelemetryState,
  type PanelTabId,
} from "./telemetryIsland";
import { type UiAppearance } from "./themes";

const SVG_NS = "http://www.w3.org/2000/svg";
const MAX_SPARK_POINTS = 24;

const CONTINUATION_PROMPT =
  "请总结当前任务目标、已完成改动、关键文件路径、未解决阻塞点与下一步计划，方便我新开一个干净上下文的 Task 无缝继续。";

const QUICK_CHAT_CHIPS: ReadonlyArray<{ label: string; text: string }> = [
  { label: "摸摸头", text: "摸摸头" },
  { label: "夸夸你", text: "你好可爱呀" },
  { label: "饿了吗", text: "饿不饿呀" },
  { label: "跳个舞", text: "跳个舞给我看" },
  { label: "累不累", text: "累了就休息一下" },
  { label: "加油呀", text: "一起加油工作吧" },
];

const STAT_ITEMS: ReadonlyArray<{
  key: keyof PetStats;
  label: string;
  cls: string;
}> = [
  { key: "satiety", label: "饱食度", cls: "is-satiety" },
  { key: "energy", label: "精力值", cls: "is-energy" },
  { key: "mood", label: "心情值", cls: "is-mood" },
  { key: "affinity", label: "亲密度", cls: "is-affinity" },
];

const TABS: ReadonlyArray<{ id: PanelTabId; label: string; icon: IconName }> = [
  { id: "status", label: "监控", icon: "activity" },
  { id: "pet", label: "伙伴", icon: "users" },
  { id: "album", label: "动作", icon: "film" },
  { id: "chat", label: "互动", icon: "message" },
];

export interface IntegratedPanelCallbacks {
  onOpenChange: (open: boolean) => void;
  onTabChange?: (tab: PanelTabId) => void;
  onRefreshTelemetry: () => Promise<void>;
  onSelectSource: (source: string) => Promise<void>;
  onCopyContinuation: (text: string) => void;
  onSelectChar: (charId: string) => Promise<void>;
  onFeed: () => void;
  onPlayClip: (clipId: string) => void;
  onChatSend: (text: string) => Promise<void>;
  onToggleAppearance: () => void;
}

function formatTokens(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1_000_000) return `${(n / 1_000_000).toFixed(2).replace(/\.?0+$/, "")}M`;
  if (abs >= 1_000) {
    const k = n / 1_000;
    return `${abs >= 100_000 ? Math.round(k).toString() : k.toFixed(1).replace(/\.0$/, "")}K`;
  }
  return String(Math.round(n));
}

function formatWindowLimit(n: number | null): string {
  if (!n || n <= 0) return "";
  if (n >= 1_000_000) return `${Math.round(n / 1_000_000)}M`;
  return `${Math.round(n / 1_000)}K`;
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
  if (d > 0) return `${d}天${h}时`;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m`;
  if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

function formatElapsed(ms: number): string {
  const sec = Math.max(0, Math.floor(ms / 1000));
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  const rem = sec % 60;
  return `${min}m${String(rem).padStart(2, "0")}s`;
}

function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  const sec = ms / 1000;
  return sec >= 10 ? `${Math.round(sec)}s` : `${sec.toFixed(1)}s`;
}

function formatTpsNum(tps: number): string {
  if (tps >= 100) return `${Math.round(tps)}`;
  return tps.toFixed(1);
}

function statusTitle(s: UnifiedState): string {
  switch (s.activity) {
    case "process_offline":
      return "等待连接";
    case "service_connecting":
      return "建立连接";
    case "service_reconnecting":
      return "正在重连";
    case "session_init":
      return "新会话就绪";
    case "ttft_waiting":
    case "deep_reasoning":
      return "深度思考中";
    case "planning_mode":
      return "规划方案中";
    case "research_lookup":
      return "查阅检索中";
    case "code_streaming":
      return "代码生成中";
    case "code_diffing":
      return "修改代码中";
    case "terminal_running":
      return "执行命令中";
    case "build_long_running":
      return "长耗时构建中";
    case "terminal_error":
      return "命令执行报错";
    case "browser_running":
      return "浏览器巡检";
    case "mcp_tool_calling":
      return "调用 MCP 工具";
    case "workflow_running":
      return "运行工作流";
    case "approval_terminal":
      return "等待你确认";
    case "approval_outside_dir":
      return "请求外部权限";
    case "artifact_review_ready":
      return "成果待审阅";
    case "task_completed":
      return "任务已完成";
    case "quota_exhausted":
      return "额度冷却中";
    case "api_auth_rate_error":
      if (s.error?.kind === "auth") return "登录已过期";
      if (s.error?.kind === "region") return "区域受限";
      if (s.error?.kind === "rate_limit") return "触发限流";
      return "服务异常";
    default:
      break;
  }
  switch (s.status) {
    case "offline":
      return "等待连接";
    case "idle":
      return "待机就绪";
    case "streaming":
      return "模型吐字中";
    case "tool_running":
      return "执行工具中";
    case "waiting_approval":
      return "等待你确认";
    case "error":
      return s.error?.kind === "network" ? "正在重连" : "服务异常";
  }
}

function errorDetail(err: TelemetryError): string {
  const code = err.code ? `${err.code} · ` : "";
  switch (err.kind) {
    case "auth":
      return `${code}登录态已过期，请重新登录 Antigravity`;
    case "region":
      return `${code}当前网络节点或区域受限`;
    case "rate_limit":
      return `${code}触发频控限流，已自动放慢刷新`;
    case "network":
      return `${code}本地服务连接中断，正在自动重连`;
    default:
      return `${code}${err.message || "遥测读取异常"}`;
  }
}

async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fallback below
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
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

interface PoolRowRefs {
  rowEl: HTMLDivElement;
  pctEl: HTMLSpanElement;
  resetEl: HTMLSpanElement;
  fillEl: HTMLDivElement;
  resetAtMs: number | null;
}

/**
 * Deskpet v2.0 精致四页签 Bento 工作台：
 * - 监控（双栏 Hero + 实时 t/s 24 点 Sparkline 曲线 + 80% 警戒线上下文卡 + 多模型池余量）
 * - 伙伴（主题渐变双字母徽章 + 四维养成条 + 8 角色秒切网格）
 * - 动作（矢量搜索 + 滚轮横滑分类条 + 实时播放微音律指示）
 * - 互动（轻盈快捷词片 + 圆润发送条）
 */
export class IntegratedPanel {
  private readonly el: HTMLDivElement;
  private readonly callbacks: IntegratedPanelCallbacks;

  private readonly tabButtons = new Map<PanelTabId, HTMLButtonElement>();
  private readonly tabPages = new Map<PanelTabId, HTMLDivElement>();
  private readonly refreshBtn: HTMLButtonElement;
  private readonly appearanceBtn: HTMLButtonElement;

  private readonly errorBanner: HTMLDivElement;
  private readonly errorTextEl: HTMLSpanElement;
  private readonly heroIcon = createStatusIcon();
  private readonly heroStatusEl: HTMLSpanElement;
  private readonly heroSourceWrap: HTMLDivElement;
  private readonly heroModelEl: HTMLSpanElement;
  private readonly heroLimitEl: HTMLSpanElement;
  private readonly heroStepPill: HTMLSpanElement;

  private readonly speedHeroCard: HTMLDivElement;
  private readonly speedValueEl: HTMLSpanElement;
  private readonly speedUnitEl: HTMLSpanElement;
  private readonly sparkLineEl: SVGPathElement;
  private readonly sparkAreaEl: SVGPathElement;
  private readonly sparkDotEl: SVGCircleElement;
  private readonly speedSubEl: HTMLDivElement;

  private readonly ctxSection: HTMLDivElement;
  private readonly ctxNumsEl: HTMLSpanElement;
  private readonly ctxDeltaEl: HTMLSpanElement;
  private readonly ctxFillEl: HTMLDivElement;
  private readonly ctxWarnEl: HTMLSpanElement;
  private readonly copyBtn: HTMLButtonElement;
  private readonly copyBtnIconWrap: HTMLSpanElement;
  private readonly copyBtnLabel: HTMLSpanElement;
  private readonly poolsSection: HTMLDivElement;
  private readonly poolsListEl: HTMLDivElement;
  private readonly statusEmptyEl: HTMLDivElement;

  private readonly pomodoroCountEl: HTMLSpanElement;
  private readonly pomodoroTimeEl: HTMLSpanElement;
  private readonly pomodoroFillEl: HTMLDivElement;
  private readonly pomodoroToggleBtn: HTMLButtonElement;
  private readonly pomodoroPresetBtns = new Map<number, HTMLButtonElement>();

  private readonly petAvatarEl: HTMLDivElement;
  private readonly petHeaderNameEl: HTMLSpanElement;
  private readonly petHeaderMetaEl: HTMLSpanElement;
  private readonly statRefs = new Map<
    keyof PetStats,
    { valEl: HTMLSpanElement; fillEl: HTMLDivElement }
  >();
  private readonly charsGridEl: HTMLDivElement;
  private readonly charBtns = new Map<string, HTMLButtonElement>();

  private readonly albumSearchEl: HTMLInputElement;
  private readonly albumClearBtn: HTMLButtonElement;
  private readonly albumCatsEl: HTMLDivElement;
  private readonly albumListEl: HTMLDivElement;
  private readonly albumCountEl: HTMLSpanElement;

  private readonly chatInputEl: HTMLInputElement;
  private readonly chatSendBtn: HTMLButtonElement;
  private readonly aiModeBadgeEl: HTMLSpanElement;
  private readonly aiVoiceBadgeEl: HTMLSpanElement;
  private readonly chatFeedEl: HTMLDivElement;
  private readonly chatMsgIds = new Set<number>();
  private chatConfig: ChatAiConfig = {
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

  private readonly standalone: boolean;
  private openState = false;
  private windowVisible = true;
  private activeTab: PanelTabId = "status";
  private telemetry: UnifiedState = createOfflineTelemetryState();
  private refreshing = false;
  private tickTimerId = 0;
  private poolRows = new Map<string, PoolRowRefs>();
  private lastPoolsSignature = "";

  private speedHistory: number[] = [];
  private peakTps = 0;
  private lastKnownTps: number | null = null;

  private activeCharId = "";
  private switchingChar = false;
  private clips: ClipMeta[] = [];
  private activeClipId: string | null = null;
  private selectedCategory = "all";
  private searchQuery = "";
  private chatSending = false;

  constructor(
    container: HTMLElement,
    callbacks: IntegratedPanelCallbacks,
    options?: { standalone?: boolean },
  ) {
    this.callbacks = callbacks;
    this.standalone = Boolean(options?.standalone);
    this.openState = this.standalone;
    this.windowVisible = !this.standalone;

    this.el = document.createElement("div");
    this.el.className = this.standalone
      ? "porcelain-card integrated-panel is-standalone"
      : "porcelain-card integrated-panel";
    this.el.dataset.open = this.openState ? "true" : "false";
    this.el.setAttribute("role", "dialog");
    this.el.setAttribute("aria-label", "桌宠灵动工作台");
    this.el.addEventListener("pointerdown", (e) => e.stopPropagation());
    this.el.addEventListener("contextmenu", (e) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
    });

    const topBar = document.createElement("div");
    topBar.className = "ip-topbar";

    const tabTrack = document.createElement("div");
    tabTrack.className = "ip-tabs";
    tabTrack.setAttribute("role", "tablist");
    for (const tab of TABS) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ip-tab-btn";
      btn.setAttribute("role", "tab");
      btn.dataset.active = tab.id === this.activeTab ? "true" : "false";
      btn.append(createIcon(tab.icon, 12));
      const lbl = document.createElement("span");
      lbl.textContent = tab.label;
      btn.appendChild(lbl);
      btn.addEventListener("click", () => this.switchTab(tab.id));
      tabTrack.appendChild(btn);
      this.tabButtons.set(tab.id, btn);
    }

    const actions = document.createElement("div");
    actions.className = "ip-top-actions";

    this.refreshBtn = document.createElement("button");
    this.refreshBtn.type = "button";
    this.refreshBtn.className = "ip-circle-btn";
    this.refreshBtn.title = "立即同步最新遥测";
    this.refreshBtn.setAttribute("aria-label", "手动刷新遥测");
    this.refreshBtn.appendChild(createIcon("refresh", 13));
    this.refreshBtn.addEventListener("click", () => {
      void this.handleRefresh();
    });

    this.appearanceBtn = document.createElement("button");
    this.appearanceBtn.type = "button";
    this.appearanceBtn.className = "ip-circle-btn";
    this.appearanceBtn.title = "切换浅瓷 / 暗墨外观";
    this.appearanceBtn.setAttribute("aria-label", "切换浅瓷或暗墨外观");
    this.appearanceBtn.appendChild(createIcon("moon", 13));
    this.appearanceBtn.addEventListener("click", () => {
      this.callbacks.onToggleAppearance();
    });

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "ip-circle-btn";
    closeBtn.title = "收起面板 (Esc)";
    closeBtn.setAttribute("aria-label", "收起面板");
    closeBtn.appendChild(createIcon("close", 13));
    closeBtn.addEventListener("click", () => this.close());

    actions.append(this.refreshBtn, this.appearanceBtn, closeBtn);
    topBar.append(tabTrack, actions);

    const body = document.createElement("div");
    body.className = "ip-body";

    /* ===== 页签 1：监控 ===== */
    const statusPage = document.createElement("div");
    statusPage.className = "ip-page";
    statusPage.dataset.tab = "status";

    this.errorBanner = document.createElement("div");
    this.errorBanner.className = "ip-error-banner";
    this.errorBanner.hidden = true;
    this.errorBanner.appendChild(createIcon("alert", 14));
    this.errorTextEl = document.createElement("span");
    this.errorBanner.appendChild(this.errorTextEl);

    const bentoGrid = document.createElement("div");
    bentoGrid.className = "ip-hero-bento";

    // 左栏：状态 + 来源/窗口徽章 + 模型 + 活动步骤
    const heroMeta = document.createElement("div");
    heroMeta.className = "ip-soft-card ip-hero-meta";

    const heroStatusRow = document.createElement("div");
    heroStatusRow.className = "ip-hero-status-row";
    this.heroStatusEl = document.createElement("span");
    this.heroStatusEl.className = "ip-hero-status";
    this.heroStatusEl.textContent = "等待连接";
    heroStatusRow.append(this.heroIcon.el, this.heroStatusEl);

    const heroTagsRow = document.createElement("div");
    heroTagsRow.className = "ip-hero-tags-row";
    this.heroSourceWrap = document.createElement("div");
    this.heroSourceWrap.className = "ip-source-wrap";
    this.heroLimitEl = document.createElement("span");
    this.heroLimitEl.className = "ip-limit-badge";
    this.heroLimitEl.hidden = true;
    heroTagsRow.append(this.heroSourceWrap, this.heroLimitEl);

    const modelWrap = document.createElement("div");
    modelWrap.className = "ip-hero-model-wrap";
    this.heroModelEl = document.createElement("span");
    this.heroModelEl.className = "ip-hero-model";
    this.heroModelEl.textContent = "等待本地 IDE";
    modelWrap.appendChild(this.heroModelEl);

    this.heroStepPill = document.createElement("span");
    this.heroStepPill.className = "ip-step-pill";
    this.heroStepPill.hidden = true;

    heroMeta.append(heroStatusRow, heroTagsRow, modelWrap, this.heroStepPill);

    // 右栏：实时输出速度 Hero + 24 点 SVG Sparkline
    this.speedHeroCard = document.createElement("div");
    this.speedHeroCard.className = "ip-soft-card ip-speed-hero";
    this.speedHeroCard.dataset.live = "false";

    const speedHead = document.createElement("div");
    speedHead.className = "ip-speed-head";
    speedHead.appendChild(createIcon("zap", 11));
    const speedHeadLbl = document.createElement("span");
    speedHeadLbl.textContent = "实时输出";
    speedHead.appendChild(speedHeadLbl);

    const speedValRow = document.createElement("div");
    speedValRow.className = "ip-speed-val-row";
    this.speedValueEl = document.createElement("span");
    this.speedValueEl.className = "ip-speed-num";
    this.speedValueEl.textContent = "—";
    this.speedUnitEl = document.createElement("span");
    this.speedUnitEl.className = "ip-speed-unit";
    this.speedUnitEl.textContent = "t/s";
    speedValRow.append(this.speedValueEl, this.speedUnitEl);

    const sparkSvg = document.createElementNS(SVG_NS, "svg");
    sparkSvg.setAttribute("viewBox", "0 0 112 28");
    sparkSvg.setAttribute("class", "ip-spark-svg");
    sparkSvg.setAttribute("aria-hidden", "true");

    const defs = document.createElementNS(SVG_NS, "defs");
    const grad = document.createElementNS(SVG_NS, "linearGradient");
    grad.setAttribute("id", "deskpet-spark-fill");
    grad.setAttribute("x1", "0");
    grad.setAttribute("y1", "0");
    grad.setAttribute("x2", "0");
    grad.setAttribute("y2", "1");
    const s1 = document.createElementNS(SVG_NS, "stop");
    s1.setAttribute("offset", "0%");
    s1.setAttribute("stop-color", "var(--accent)");
    s1.setAttribute("stop-opacity", "0.32");
    const s2 = document.createElementNS(SVG_NS, "stop");
    s2.setAttribute("offset", "100%");
    s2.setAttribute("stop-color", "var(--accent)");
    s2.setAttribute("stop-opacity", "0.02");
    grad.append(s1, s2);
    defs.appendChild(grad);

    this.sparkAreaEl = document.createElementNS(SVG_NS, "path");
    this.sparkAreaEl.setAttribute("class", "ip-spark-area");
    this.sparkAreaEl.setAttribute("fill", "url(#deskpet-spark-fill)");

    this.sparkLineEl = document.createElementNS(SVG_NS, "path");
    this.sparkLineEl.setAttribute("class", "ip-spark-line");
    this.sparkLineEl.setAttribute("fill", "none");

    this.sparkDotEl = document.createElementNS(SVG_NS, "circle");
    this.sparkDotEl.setAttribute("class", "ip-spark-dot");
    this.sparkDotEl.setAttribute("r", "2.4");
    this.sparkDotEl.setAttribute("cx", "109");
    this.sparkDotEl.setAttribute("cy", "24");
    this.sparkDotEl.style.opacity = "0";

    sparkSvg.append(defs, this.sparkAreaEl, this.sparkLineEl, this.sparkDotEl);

    this.speedSubEl = document.createElement("div");
    this.speedSubEl.className = "ip-speed-sub";
    this.speedSubEl.textContent = "待机中";

    this.speedHeroCard.append(speedHead, speedValRow, sparkSvg, this.speedSubEl);
    bentoGrid.append(heroMeta, this.speedHeroCard);

    // 上下文记忆卡
    this.ctxSection = document.createElement("div");
    this.ctxSection.className = "ip-soft-card ip-ctx-card";
    this.ctxSection.hidden = true;

    const ctxTop = document.createElement("div");
    ctxTop.className = "ip-row-between";
    const ctxTitleWrap = document.createElement("div");
    ctxTitleWrap.className = "ip-ctx-title-wrap";
    const ctxTitle = document.createElement("span");
    ctxTitle.className = "ip-sec-label";
    ctxTitle.textContent = "上下文记忆";
    this.ctxDeltaEl = document.createElement("span");
    this.ctxDeltaEl.className = "ip-delta-chip";
    this.ctxDeltaEl.hidden = true;
    ctxTitleWrap.append(ctxTitle, this.ctxDeltaEl);

    this.copyBtn = document.createElement("button");
    this.copyBtn.type = "button";
    this.copyBtn.className = "ip-mini-pill-btn";
    this.copyBtn.title = "复制交接总结提示词，无缝开启新 Task";
    this.copyBtnIconWrap = document.createElement("span");
    this.copyBtnIconWrap.className = "ip-btn-icon";
    this.copyBtnIconWrap.appendChild(createIcon("copy", 11));
    this.copyBtnLabel = document.createElement("span");
    this.copyBtnLabel.textContent = "一键续命";
    this.copyBtn.append(this.copyBtnIconWrap, this.copyBtnLabel);
    this.copyBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      void this.triggerCopyContinuation();
    });
    ctxTop.append(ctxTitleWrap, this.copyBtn);

    const ctxTrack = document.createElement("div");
    ctxTrack.className = "ip-bar-track ip-bar-clickable has-marker";
    ctxTrack.title = "点击也可一键复制续命提示词";
    ctxTrack.addEventListener("click", () => {
      void this.triggerCopyContinuation();
    });
    this.ctxFillEl = document.createElement("div");
    this.ctxFillEl.className = "ip-bar-fill is-accent";
    const ctxMarker = document.createElement("span");
    ctxMarker.className = "ip-ctx-marker";
    ctxMarker.title = "80% 建议续命警戒线";
    ctxTrack.append(this.ctxFillEl, ctxMarker);

    const ctxBottom = document.createElement("div");
    ctxBottom.className = "ip-row-between ip-ctx-foot";
    this.ctxNumsEl = document.createElement("span");
    this.ctxNumsEl.className = "ip-mono-sub";
    this.ctxWarnEl = document.createElement("span");
    this.ctxWarnEl.className = "ip-ctx-warn";
    this.ctxWarnEl.hidden = true;
    this.ctxWarnEl.textContent = "建议开新 Task 防降智";
    ctxBottom.append(this.ctxNumsEl, this.ctxWarnEl);

    this.ctxSection.append(ctxTop, ctxTrack, ctxBottom);

    // 模型池余量卡
    this.poolsSection = document.createElement("div");
    this.poolsSection.className = "ip-soft-card ip-pools-card";
    this.poolsSection.hidden = true;

    const poolsHead = document.createElement("div");
    poolsHead.className = "ip-sec-label";
    poolsHead.textContent = "模型池剩余额度";

    this.poolsListEl = document.createElement("div");
    this.poolsListEl.className = "ip-pools-list";

    this.poolsSection.append(poolsHead, this.poolsListEl);

    this.statusEmptyEl = document.createElement("div");
    this.statusEmptyEl.className = "ip-empty-hint";
    this.statusEmptyEl.textContent = "打开 Antigravity IDE 后自动同步额度、上下文与实时速度";
    this.statusEmptyEl.hidden = true;

    const pomodoroSection = document.createElement("div");
    pomodoroSection.className = "ip-soft-card ip-pomodoro-card";

    const pomoHead = document.createElement("div");
    pomoHead.className = "ip-row-between";
    const pomoTitleWrap = document.createElement("span");
    pomoTitleWrap.className = "ip-sec-label";
    pomoTitleWrap.appendChild(createIcon("timer", 12));
    const pomoTitle = document.createElement("span");
    pomoTitle.textContent = "番茄专注计时";
    this.pomodoroCountEl = document.createElement("span");
    this.pomodoroCountEl.className = "ip-limit-badge";
    this.pomodoroCountEl.textContent = "今日 0 个";
    pomoTitleWrap.append(pomoTitle, this.pomodoroCountEl);

    this.pomodoroTimeEl = document.createElement("span");
    this.pomodoroTimeEl.className = "ip-pomo-time";
    this.pomodoroTimeEl.textContent = "25:00";
    pomoHead.append(pomoTitleWrap, this.pomodoroTimeEl);

    const pomoTrack = document.createElement("div");
    pomoTrack.className = "ip-bar-track is-thin";
    this.pomodoroFillEl = document.createElement("div");
    this.pomodoroFillEl.className = "ip-bar-fill is-accent";
    pomoTrack.appendChild(this.pomodoroFillEl);

    const pomoActions = document.createElement("div");
    pomoActions.className = "ip-row-between";
    const pomoBtnsLeft = document.createElement("div");
    pomoBtnsLeft.className = "ip-pomo-btn-group";

    this.pomodoroToggleBtn = document.createElement("button");
    this.pomodoroToggleBtn.type = "button";
    this.pomodoroToggleBtn.className = "ip-mini-pill-btn is-primary";
    this.pomodoroToggleBtn.textContent = "开始 25m";
    this.pomodoroToggleBtn.addEventListener("click", () => {
      void settingsSet("pomodoroControl", { action: "toggle" }).catch(() => {});
    });

    const pomoResetBtn = document.createElement("button");
    pomoResetBtn.type = "button";
    pomoResetBtn.className = "ip-mini-pill-btn";
    pomoResetBtn.textContent = "重置";
    pomoResetBtn.addEventListener("click", () => {
      void settingsSet("pomodoroControl", { action: "reset" }).catch(() => {});
    });
    pomoBtnsLeft.append(this.pomodoroToggleBtn, pomoResetBtn);

    const pomoPresets = document.createElement("div");
    pomoPresets.className = "ip-pomo-btn-group";
    for (const mins of [15, 25, 45]) {
      const pBtn = document.createElement("button");
      pBtn.type = "button";
      pBtn.className = "ip-mini-pill-btn";
      pBtn.textContent = `${mins}m`;
      pBtn.dataset.active = mins === 25 ? "true" : "false";
      pBtn.addEventListener("click", () => {
        void settingsSet("pomodoroControl", { action: "start", minutes: mins }).catch(() => {});
      });
      pomoPresets.appendChild(pBtn);
      this.pomodoroPresetBtns.set(mins, pBtn);
    }
    pomoActions.append(pomoBtnsLeft, pomoPresets);
    pomodoroSection.append(pomoHead, pomoTrack, pomoActions);

    statusPage.append(
      this.errorBanner,
      bentoGrid,
      this.ctxSection,
      pomodoroSection,
      this.poolsSection,
      this.statusEmptyEl,
    );

    /* ===== 页签 2：伙伴 ===== */
    const petPage = document.createElement("div");
    petPage.className = "ip-page";
    petPage.dataset.tab = "pet";
    petPage.hidden = true;

    const petHeadCard = document.createElement("div");
    petHeadCard.className = "ip-soft-card ip-pet-head";

    const petIdentity = document.createElement("div");
    petIdentity.className = "ip-pet-identity";
    this.petAvatarEl = document.createElement("div");
    this.petAvatarEl.className = "ip-pet-avatar";
    this.petAvatarEl.appendChild(createPixelAvatar("claude", 36, "ip-pixel-avatar"));

    const petInfo = document.createElement("div");
    petInfo.className = "ip-pet-info";
    const petNameRow = document.createElement("div");
    petNameRow.className = "ip-pet-name-row";
    this.petHeaderNameEl = document.createElement("span");
    this.petHeaderNameEl.className = "ip-pet-name";
    this.petHeaderNameEl.textContent = "桌宠伙伴";
    petNameRow.append(this.petHeaderNameEl);
    this.petHeaderMetaEl = document.createElement("span");
    this.petHeaderMetaEl.className = "ip-pet-sub";
    petInfo.append(petNameRow, this.petHeaderMetaEl);
    petIdentity.append(this.petAvatarEl, petInfo);

    const feedBtn = document.createElement("button");
    feedBtn.type = "button";
    feedBtn.className = "ip-mini-pill-btn is-warm";
    feedBtn.appendChild(createIcon("cake", 12));
    const feedLbl = document.createElement("span");
    feedLbl.textContent = "投喂点心";
    feedBtn.appendChild(feedLbl);
    feedBtn.addEventListener("click", () => this.callbacks.onFeed());
    petHeadCard.append(petIdentity, feedBtn);

    const statsGrid = document.createElement("div");
    statsGrid.className = "ip-stats-grid";
    for (const item of STAT_ITEMS) {
      const box = document.createElement("div");
      box.className = `ip-stat-box ${item.cls}`;
      const row = document.createElement("div");
      row.className = "ip-row-between";
      const lbl = document.createElement("span");
      lbl.className = "ip-stat-lbl";
      lbl.textContent = item.label;
      const val = document.createElement("span");
      val.className = "ip-stat-val";
      val.textContent = "0";
      row.append(lbl, val);

      const tr = document.createElement("div");
      tr.className = "ip-bar-track is-thin";
      const fl = document.createElement("div");
      fl.className = "ip-bar-fill";
      tr.appendChild(fl);

      box.append(row, tr);
      statsGrid.appendChild(box);
      this.statRefs.set(item.key, { valEl: val, fillEl: fl });
    }

    const charsCard = document.createElement("div");
    charsCard.className = "ip-soft-card";
    const charsHead = document.createElement("div");
    charsHead.className = "ip-row-between";
    const charsLbl = document.createElement("div");
    charsLbl.className = "ip-sec-label";
    charsLbl.textContent = "切换伙伴（点击即换）";
    const dlCharsBtn = document.createElement("button");
    dlCharsBtn.type = "button";
    dlCharsBtn.className = "ip-mini-pill-btn";
    dlCharsBtn.append(createIcon("download", 11));
    const dlCharsSpan = document.createElement("span");
    dlCharsSpan.textContent = "角色下载";
    dlCharsBtn.appendChild(dlCharsSpan);
    dlCharsBtn.addEventListener("click", () => {
      void settingsSet("settingsOpen", { open: true, tab: "resources" }).catch(() => {});
    });
    charsHead.append(charsLbl, dlCharsBtn);
    this.charsGridEl = document.createElement("div");
    this.charsGridEl.className = "ip-chars-grid";
    charsCard.append(charsHead, this.charsGridEl);

    petPage.append(petHeadCard, statsGrid, charsCard);

    /* ===== 页签 3：动作相册 ===== */
    const albumPage = document.createElement("div");
    albumPage.className = "ip-page ip-album-page";
    albumPage.dataset.tab = "album";
    albumPage.hidden = true;

    const searchRow = document.createElement("div");
    searchRow.className = "ip-album-search-row";

    const searchBox = document.createElement("div");
    searchBox.className = "ip-search-box";
    const searchIconWrap = document.createElement("span");
    searchIconWrap.className = "ip-search-icon";
    searchIconWrap.appendChild(createIcon("search", 13));

    this.albumSearchEl = document.createElement("input");
    this.albumSearchEl.type = "text";
    this.albumSearchEl.className = "ip-search-input";
    this.albumSearchEl.placeholder = "搜索动作名或标签…";

    this.albumClearBtn = document.createElement("button");
    this.albumClearBtn.type = "button";
    this.albumClearBtn.className = "ip-search-clear";
    this.albumClearBtn.title = "清空搜索";
    this.albumClearBtn.hidden = true;
    this.albumClearBtn.appendChild(createIcon("close", 11));
    this.albumClearBtn.addEventListener("click", () => {
      this.albumSearchEl.value = "";
      this.searchQuery = "";
      this.albumClearBtn.hidden = true;
      this.renderAlbumList();
      this.albumSearchEl.focus();
    });

    this.albumSearchEl.addEventListener("input", () => {
      this.searchQuery = this.albumSearchEl.value.trim().toLowerCase();
      this.albumClearBtn.hidden = this.searchQuery.length === 0;
      this.renderAlbumList();
    });

    searchBox.append(searchIconWrap, this.albumSearchEl, this.albumClearBtn);
    const shuffleBtn = document.createElement("button");
    shuffleBtn.type = "button";
    shuffleBtn.className = "ip-mini-pill-btn";
    shuffleBtn.title = "从当前列表随机播放一段";
    shuffleBtn.appendChild(createIcon("shuffle", 11));
    const shuffleLbl = document.createElement("span");
    shuffleLbl.textContent = "随机";
    shuffleBtn.appendChild(shuffleLbl);
    shuffleBtn.addEventListener("click", () => {
      const q = this.searchQuery;
      const pool = this.clips.filter((c) => {
        if (this.selectedCategory !== "all" && c.category !== this.selectedCategory) return false;
        if (!q) return true;
        return `${c.id} ${c.category} ${getCategoryLabel(c.category)} ${c.tags.join(" ")}`
          .toLowerCase()
          .includes(q);
      });
      if (pool.length === 0) return;
      const candidates = pool.length > 1 ? pool.filter((c) => c.id !== this.activeClipId) : pool;
      const pick = candidates[Math.floor(Math.random() * candidates.length)];
      if (pick) {
        this.callbacks.onPlayClip(pick.id);
      }
    });
    this.albumCountEl = document.createElement("span");
    this.albumCountEl.className = "ip-album-count";
    searchRow.append(searchBox, shuffleBtn, this.albumCountEl);

    this.albumCatsEl = document.createElement("div");
    this.albumCatsEl.className = "ip-cat-strip";
    this.albumCatsEl.addEventListener(
      "wheel",
      (e) => {
        if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
          this.albumCatsEl.scrollLeft += e.deltaY;
          e.preventDefault();
        }
      },
      { passive: false },
    );

    this.albumListEl = document.createElement("div");
    this.albumListEl.className = "ip-album-list";

    albumPage.append(searchRow, this.albumCatsEl, this.albumListEl);

    /* ===== 页签 4：互动闲聊 ===== */
    const chatPage = document.createElement("div");
    chatPage.className = "ip-page ip-chat-page";
    chatPage.dataset.tab = "chat";
    chatPage.hidden = true;

    // 4.1 极简状态条 + 独立「桌宠设置」入口
    const aiBar = document.createElement("div");
    aiBar.className = "ip-soft-card ip-ai-bar";

    const aiBarLeft = document.createElement("div");
    aiBarLeft.className = "ip-ai-badges";
    this.aiModeBadgeEl = document.createElement("span");
    this.aiModeBadgeEl.className = "ip-ai-badge";
    this.aiModeBadgeEl.textContent = "人设模板";
    this.aiVoiceBadgeEl = document.createElement("span");
    this.aiVoiceBadgeEl.className = "ip-ai-badge is-voice";
    this.aiVoiceBadgeEl.textContent = "Edge 女声";
    aiBarLeft.append(this.aiModeBadgeEl, this.aiVoiceBadgeEl);

    const aiBarRight = document.createElement("div");
    aiBarRight.className = "ip-ai-actions";

    const clearChatBtn = document.createElement("button");
    clearChatBtn.type = "button";
    clearChatBtn.className = "ip-mini-pill-btn";
    clearChatBtn.textContent = "清空";
    clearChatBtn.addEventListener("click", () => {
      void settingsSet("chatClearHistory", true).catch(() => {});
      this.clearChatMessages();
    });

    const openSettingsBtn = document.createElement("button");
    openSettingsBtn.type = "button";
    openSettingsBtn.className = "ip-mini-pill-btn";
    openSettingsBtn.appendChild(createIcon("sliders", 11));
    const openSettingsLbl = document.createElement("span");
    openSettingsLbl.textContent = "桌宠设置";
    openSettingsBtn.appendChild(openSettingsLbl);
    openSettingsBtn.addEventListener("click", () => {
      void settingsSet("settingsOpen", { open: true }).catch(() => {});
    });

    aiBarRight.append(clearChatBtn, openSettingsBtn);
    aiBar.append(aiBarLeft, aiBarRight);

    // 4.2 单行横滑快捷互动词条
    const chipWrap = document.createElement("div");
    chipWrap.className = "ip-chat-chips";
    chipWrap.addEventListener(
      "wheel",
      (e) => {
        if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
          chipWrap.scrollLeft += e.deltaY;
          e.preventDefault();
        }
      },
      { passive: false },
    );
    for (const chip of QUICK_CHAT_CHIPS) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ip-chat-chip";
      btn.appendChild(createIcon("sparkles", 11));
      const t = document.createElement("span");
      t.textContent = chip.label;
      btn.appendChild(t);
      btn.addEventListener("click", () => {
        void this.submitChat(chip.text);
      });
      chipWrap.appendChild(btn);
    }

    // 4.4 对话记录流
    this.chatFeedEl = document.createElement("div");
    this.chatFeedEl.className = "ip-chat-feed";
    const emptyChatHint = document.createElement("div");
    emptyChatHint.className = "ip-empty-hint";
    emptyChatHint.textContent = "点击上方快捷词或输入消息，与桌宠实时对话并朗读";
    this.chatFeedEl.appendChild(emptyChatHint);

    // 4.5 底部发送条
    const chatForm = document.createElement("form");
    chatForm.className = "ip-chat-form";
    this.chatInputEl = document.createElement("input");
    this.chatInputEl.type = "text";
    this.chatInputEl.className = "ip-input";
    this.chatInputEl.placeholder = "Alt+Space 快速呼出，和桌宠说点什么…";
    this.chatInputEl.maxLength = 240;
    this.chatInputEl.autocomplete = "off";

    this.chatSendBtn = document.createElement("button");
    this.chatSendBtn.type = "submit";
    this.chatSendBtn.className = "ip-send-btn";
    this.chatSendBtn.title = "发送";
    this.chatSendBtn.setAttribute("aria-label", "发送消息");
    this.chatSendBtn.appendChild(createIcon("send", 14));

    chatForm.append(this.chatInputEl, this.chatSendBtn);
    chatForm.addEventListener("submit", (e) => {
      e.preventDefault();
      const text = this.chatInputEl.value.trim();
      if (!text) return;
      this.chatInputEl.value = "";
      void this.submitChat(text);
    });

    chatPage.append(aiBar, chipWrap, this.chatFeedEl, chatForm);
    this.syncAiDrawerUi();

    this.tabPages.set("status", statusPage);
    this.tabPages.set("pet", petPage);
    this.tabPages.set("album", albumPage);
    this.tabPages.set("chat", chatPage);

    body.append(statusPage, petPage, albumPage, chatPage);
    this.el.append(topBar, body);
    container.appendChild(this.el);

    this.renderTelemetry(false);
  }

  public isOpen(): boolean {
    return this.openState;
  }

  public setWindowVisible(visible: boolean): void {
    this.windowVisible = visible;
    this.syncTickTimer();
    if (visible) {
      this.renderTelemetry(true);
      if (this.activeTab === "chat") {
        window.requestAnimationFrame(() => this.chatInputEl.focus());
      }
    }
  }

  public setAppearance(mode: UiAppearance): void {
    this.appearanceBtn.replaceChildren(
      createIcon(mode === "dark" ? "sun" : "moon", 13),
    );
    this.appearanceBtn.title =
      mode === "dark" ? "切换为浅瓷白外观" : "切换为暗夜墨外观";
  }

  public getActiveTab(): PanelTabId {
    return this.activeTab;
  }

  public open(tab?: PanelTabId): void {
    if (tab) {
      this.switchTab(tab);
    }
    if (this.standalone) {
      this.setWindowVisible(true);
      return;
    }
    if (this.openState) {
      if (this.activeTab === "chat") {
        window.requestAnimationFrame(() => this.chatInputEl.focus());
      }
      return;
    }
    this.openState = true;
    this.windowVisible = true;
    this.el.dataset.open = "true";
    this.syncTickTimer();
    this.renderTelemetry(true);
    this.callbacks.onOpenChange(true);
    if (this.activeTab === "chat") {
      window.requestAnimationFrame(() => this.chatInputEl.focus());
    }
  }

  public close(): void {
    if (this.standalone) {
      this.windowVisible = false;
      this.syncTickTimer();
      this.callbacks.onOpenChange(false);
      return;
    }
    if (!this.openState) return;
    this.openState = false;
    this.windowVisible = false;
    this.el.dataset.open = "false";
    this.syncTickTimer();
    this.callbacks.onOpenChange(false);
  }

  public toggle(tab?: PanelTabId): void {
    if (this.openState && (!tab || tab === this.activeTab)) {
      this.close();
    } else {
      this.open(tab);
    }
  }

  public switchTab(tab: PanelTabId): void {
    const changed = this.activeTab !== tab;
    this.activeTab = tab;
    for (const [id, btn] of this.tabButtons.entries()) {
      btn.dataset.active = id === tab ? "true" : "false";
    }
    for (const [id, page] of this.tabPages.entries()) {
      page.hidden = id !== tab;
    }
    this.refreshBtn.hidden = tab !== "status";
    if (changed) {
      this.callbacks.onTabChange?.(tab);
    }
    if (tab === "chat" && (this.openState || this.standalone) && this.windowVisible) {
      window.requestAnimationFrame(() => this.chatInputEl.focus());
    }
  }

  public setTelemetry(next: UnifiedState): void {
    if (
      next.status === "streaming" &&
      next.speed.confidence !== "unavailable" &&
      next.speed.tokensPerSec !== null &&
      next.speed.tokensPerSec > 0
    ) {
      const tps = next.speed.tokensPerSec;
      this.lastKnownTps = tps;
      this.peakTps = Math.max(this.peakTps, tps);
      this.speedHistory.push(tps);
      if (this.speedHistory.length > MAX_SPARK_POINTS) {
        this.speedHistory.shift();
      }
    } else if (next.status === "offline") {
      this.speedHistory = [];
      this.peakTps = 0;
      this.lastKnownTps = null;
    }

    this.telemetry = next;
    this.syncTickTimer();
    if (this.openState && (!this.standalone || this.windowVisible)) {
      this.renderTelemetry(false);
    }
  }

  private syncTickTimer(): void {
    const active = this.openState && (!this.standalone || this.windowVisible);
    const need =
      active &&
      (this.telemetry.status === "tool_running" ||
        this.telemetry.quota.resetAtMs !== null ||
        this.telemetry.quota.pools.some((p) => p.resetAtMs !== null));

    if (need && this.tickTimerId === 0) {
      this.tickTimerId = window.setInterval(() => {
        if (!this.openState || (this.standalone && !this.windowVisible)) return;
        this.tickLiveCounters();
      }, 1000);
    } else if (!need && this.tickTimerId !== 0) {
      window.clearInterval(this.tickTimerId);
      this.tickTimerId = 0;
    }
  }

  private tickLiveCounters(): void {
    for (const row of this.poolRows.values()) {
      row.resetEl.textContent = formatCountdown(row.resetAtMs);
    }
    if (this.telemetry.status === "tool_running") {
      this.renderStepPill();
    }
  }

  private renderTelemetry(animateBars: boolean): void {
    const s = this.telemetry;
    const title = statusTitle(s);
    this.heroIcon.setStatus(s.status);
    this.heroStatusEl.textContent = title;
    this.heroStatusEl.title = title;
    this.heroStatusEl.dataset.status = s.status;

    const liveTps =
      s.status === "streaming" &&
      s.speed.confidence !== "unavailable" &&
      s.speed.tokensPerSec !== null &&
      s.speed.tokensPerSec > 0
        ? s.speed.tokensPerSec
        : null;
    this.heroIcon.setSpeedRatio(
      liveTps !== null ? Math.min(1, liveTps / 90) : s.status === "streaming" ? 0.45 : 0,
    );

    if (s.error && s.activity !== "service_reconnecting") {
      this.errorBanner.hidden = false;
      this.errorTextEl.textContent = errorDetail(s.error);
    } else {
      this.errorBanner.hidden = true;
    }

    this.heroSourceWrap.replaceChildren();
    const sources = s.availableSources.length > 0 ? s.availableSources : [s.source];
    if (sources.length > 1) {
      const sel = document.createElement("select");
      sel.className = "ip-source-select";
      sel.setAttribute("aria-label", "切换数据源");
      for (const src of sources) {
        const opt = document.createElement("option");
        opt.value = src;
        opt.textContent = src;
        opt.selected = src === s.source;
        sel.appendChild(opt);
      }
      sel.addEventListener("change", () => {
        void this.callbacks.onSelectSource(sel.value);
      });
      this.heroSourceWrap.appendChild(sel);
    } else {
      const tag = document.createElement("span");
      tag.className = "ip-source-tag";
      tag.textContent = s.source;
      this.heroSourceWrap.appendChild(tag);
    }

    if (s.model.name) {
      this.heroModelEl.textContent = s.model.name;
      this.heroModelEl.title = s.model.name;
    } else {
      this.heroModelEl.textContent =
        s.status === "offline" ? "等待本地 IDE" : "已连接服务";
      this.heroModelEl.title = "";
    }

    const limitStr = formatWindowLimit(s.model.contextLimit ?? s.context.limitTokens);
    if (limitStr) {
      this.heroLimitEl.hidden = false;
      this.heroLimitEl.textContent = limitStr;
    } else {
      this.heroLimitEl.hidden = true;
    }

    this.renderStepPill();
    this.renderSpeedHero();
    this.renderContext(animateBars);
    this.renderPools(animateBars);

    const isBareOffline =
      s.status === "offline" &&
      !s.error &&
      this.ctxSection.hidden &&
      this.poolsSection.hidden;
    this.statusEmptyEl.hidden = !isBareOffline;
  }

  private renderStepPill(): void {
    const s = this.telemetry;
    if (s.status === "tool_running") {
      const step = (s.activeStepLabel ?? "").trim() || "执行工具";
      this.heroStepPill.hidden = false;
      this.heroStepPill.dataset.tone = "tool";
      this.heroStepPill.textContent = `${step} · ${formatElapsed(Date.now() - s.statusSinceMs)}`;
    } else if (s.status === "waiting_approval") {
      this.heroStepPill.hidden = false;
      this.heroStepPill.dataset.tone = "warn";
      this.heroStepPill.textContent = "等你点击「允许」";
    } else {
      this.heroStepPill.hidden = true;
    }
  }

  private renderSpeedHero(): void {
    const s = this.telemetry;
    const isStreaming = s.status === "streaming";
    const liveTps =
      isStreaming &&
      s.speed.confidence !== "unavailable" &&
      s.speed.tokensPerSec !== null &&
      s.speed.tokensPerSec > 0
        ? s.speed.tokensPerSec
        : null;

    this.speedHeroCard.dataset.live = isStreaming ? "true" : "false";

    if (liveTps !== null) {
      this.speedValueEl.textContent = formatTpsNum(liveTps);
      this.speedUnitEl.hidden = false;
    } else if (isStreaming) {
      this.speedValueEl.textContent = "…";
      this.speedUnitEl.hidden = false;
    } else if (s.status === "offline") {
      this.speedValueEl.textContent = "—";
      this.speedUnitEl.hidden = true;
    } else if (this.lastKnownTps !== null) {
      this.speedValueEl.textContent = formatTpsNum(this.lastKnownTps);
      this.speedUnitEl.hidden = false;
    } else {
      this.speedValueEl.textContent = "0.0";
      this.speedUnitEl.hidden = false;
    }

    const subParts: string[] = [];
    if (this.peakTps > 0) {
      subParts.push(`峰值 ${formatTpsNum(this.peakTps)}`);
    }
    if (
      s.speed.confidence !== "unavailable" &&
      s.speed.ttftMs !== null &&
      s.speed.ttftMs > 0
    ) {
      const ttft =
        s.speed.ttftMs >= 1000
          ? `${(s.speed.ttftMs / 1000).toFixed(2)}s`
          : `${Math.round(s.speed.ttftMs)}ms`;
      subParts.push(`首字 ${ttft}`);
    }
    if (subParts.length === 0) {
      subParts.push(
        isStreaming ? "采样中" : s.status === "offline" ? "离线" : "待机中",
      );
    }
    this.speedSubEl.textContent = subParts.join(" · ");

    this.renderSparkline(isStreaming);
  }

  private renderSparkline(isStreaming: boolean): void {
    const W = 112;
    const H = 28;
    const padX = 3;
    const padTop = 4;
    const padBottom = 3;
    const usableW = W - padX * 2;
    const usableH = H - padTop - padBottom;

    if (this.speedHistory.length === 0) {
      const y = H - padBottom;
      this.sparkLineEl.setAttribute("d", `M ${padX} ${y} L ${W - padX} ${y}`);
      this.sparkAreaEl.setAttribute("d", "");
      this.sparkDotEl.style.opacity = "0";
      return;
    }

    const pts =
      this.speedHistory.length === 1
        ? [this.speedHistory[0], this.speedHistory[0]]
        : this.speedHistory;
    const maxVal = Math.max(20, ...pts) * 1.15;

    const coords = pts.map((v, idx) => {
      const x = padX + (idx / (pts.length - 1)) * usableW;
      const y = padTop + (1 - Math.min(1, v / maxVal)) * usableH;
      return { x, y };
    });

    const lineD = coords
      .map((pt, i) => `${i === 0 ? "M" : "L"} ${pt.x.toFixed(1)} ${pt.y.toFixed(1)}`)
      .join(" ");
    const last = coords[coords.length - 1];
    const first = coords[0];
    const areaD = `${lineD} L ${last.x.toFixed(1)} ${H} L ${first.x.toFixed(1)} ${H} Z`;

    this.sparkLineEl.setAttribute("d", lineD);
    this.sparkAreaEl.setAttribute("d", areaD);
    this.sparkDotEl.setAttribute("cx", last.x.toFixed(1));
    this.sparkDotEl.setAttribute("cy", last.y.toFixed(1));
    this.sparkDotEl.style.opacity = isStreaming ? "1" : "0.45";
  }

  private renderContext(animate: boolean): void {
    const ctx = this.telemetry.context;
    const hasCtx =
      ctx.confidence !== "unavailable" &&
      (ctx.usedPct !== null || ctx.usedTokens !== null);
    if (!hasCtx) {
      this.ctxSection.hidden = true;
      return;
    }
    this.ctxSection.hidden = false;

    const pct =
      ctx.usedPct !== null
        ? Math.max(0, Math.min(100, ctx.usedPct))
        : ctx.usedTokens !== null && ctx.limitTokens && ctx.limitTokens > 0
          ? Math.max(0, Math.min(100, (ctx.usedTokens / ctx.limitTokens) * 100))
          : 0;
    const high = pct >= 80 || this.telemetry.alerts.contextHigh;

    this.ctxFillEl.classList.toggle("is-warn", high);
    this.ctxWarnEl.hidden = !high;

    if (animate) {
      this.ctxFillEl.style.transition = "none";
      this.ctxFillEl.style.width = "0%";
      window.requestAnimationFrame(() => {
        this.ctxFillEl.style.transition = "";
        this.ctxFillEl.style.width = `${pct.toFixed(1)}%`;
      });
    } else {
      this.ctxFillEl.style.width = `${pct.toFixed(1)}%`;
    }

    if (ctx.usedTokens !== null && ctx.limitTokens !== null) {
      this.ctxNumsEl.textContent = `${formatTokens(ctx.usedTokens)} / ${formatTokens(ctx.limitTokens)} (${pct.toFixed(1).replace(/\.0$/, "")}%)`;
    } else {
      this.ctxNumsEl.textContent = `已占用 ${pct.toFixed(1).replace(/\.0$/, "")}%`;
    }

    if (ctx.deltaTokens !== null && ctx.deltaTokens > 0) {
      this.ctxDeltaEl.hidden = false;
      this.ctxDeltaEl.textContent = `+${formatTokens(ctx.deltaTokens)}`;
    } else {
      this.ctxDeltaEl.hidden = true;
    }
  }

  private renderPools(animate: boolean): void {
    const q = this.telemetry.quota;
    if (q.confidence === "unavailable" && q.pools.length === 0 && q.remainingPct === null) {
      this.poolsSection.hidden = true;
      return;
    }

    const pools: TelemetryQuotaPool[] =
      q.pools.length > 0
        ? q.pools
        : q.remainingPct !== null
          ? [
              {
                id: "current",
                label: this.telemetry.model.name ?? "当前模型池",
                remainingPct: q.remainingPct,
                resetAtMs: q.resetAtMs,
                isActive: true,
              },
            ]
          : [];

    if (pools.length === 0) {
      this.poolsSection.hidden = true;
      return;
    }
    this.poolsSection.hidden = false;

    const currentLow =
      (q.remainingPct !== null && q.remainingPct < 20) ||
      pools.some((p) => p.isActive && p.remainingPct < 20);

    const sig = pools.map((p) => `${p.id}:${p.label}:${p.isActive}`).join("|");
    const needRebuild = sig !== this.lastPoolsSignature;

    if (needRebuild) {
      this.lastPoolsSignature = sig;
      this.poolsListEl.replaceChildren();
      this.poolRows.clear();

      for (const pool of pools) {
        const rowEl = document.createElement("div");
        rowEl.className = "ip-pool-row";

        const top = document.createElement("div");
        top.className = "ip-row-between";

        const nameWrap = document.createElement("span");
        nameWrap.className = "ip-pool-name";
        nameWrap.textContent = pool.label;
        if (pool.isActive) {
          const cur = document.createElement("span");
          cur.className = "ip-pool-cur-dot";
          cur.textContent = "当前";
          nameWrap.appendChild(cur);
        }

        const right = document.createElement("span");
        right.className = "ip-pool-right";
        const resetEl = document.createElement("span");
        resetEl.className = "ip-pool-reset";
        const pctEl = document.createElement("span");
        pctEl.className = "ip-pool-pct";
        right.append(resetEl, pctEl);
        top.append(nameWrap, right);

        const track = document.createElement("div");
        track.className = "ip-bar-track is-thin";
        const fillEl = document.createElement("div");
        fillEl.className = "ip-bar-fill is-accent";
        track.appendChild(fillEl);

        rowEl.append(top, track);
        this.poolsListEl.appendChild(rowEl);
        this.poolRows.set(pool.id, {
          rowEl,
          pctEl,
          resetEl,
          fillEl,
          resetAtMs: pool.resetAtMs,
        });
      }
    }

    for (const pool of pools) {
      const refs = this.poolRows.get(pool.id);
      if (!refs) continue;
      refs.resetAtMs = pool.resetAtMs;

      const pct = Math.max(0, Math.min(100, pool.remainingPct));
      const pctText = pct > 0 && pct < 1 ? "<1%" : `${Math.round(pct)}%`;
      refs.pctEl.textContent = pctText;
      refs.resetEl.textContent = formatCountdown(pool.resetAtMs);

      const isLow = pct <= 10;
      const isAltHighlight = currentLow && !pool.isActive && pct >= 35;
      refs.rowEl.dataset.current = pool.isActive ? "true" : "false";
      refs.rowEl.dataset.recommend = isAltHighlight ? "true" : "false";
      refs.fillEl.classList.toggle("is-danger", isLow);
      refs.fillEl.classList.toggle("is-good", isAltHighlight);

      if (animate || needRebuild) {
        refs.fillEl.style.transition = "none";
        refs.fillEl.style.width = "0%";
        window.requestAnimationFrame(() => {
          refs.fillEl.style.transition = "";
          refs.fillEl.style.width = `${pct.toFixed(1)}%`;
        });
      } else {
        refs.fillEl.style.width = `${pct.toFixed(1)}%`;
      }
    }
  }

  private async handleRefresh(): Promise<void> {
    if (this.refreshing) return;
    this.refreshing = true;
    this.refreshBtn.dataset.spinning = "true";
    try {
      await this.callbacks.onRefreshTelemetry();
    } finally {
      window.setTimeout(() => {
        this.refreshing = false;
        this.refreshBtn.dataset.spinning = "false";
      }, 360);
    }
  }

  private async triggerCopyContinuation(): Promise<void> {
    const ok = await copyText(CONTINUATION_PROMPT);
    this.copyBtnIconWrap.replaceChildren(createIcon(ok ? "check" : "alert", 11));
    this.copyBtnLabel.textContent = ok ? "已复制" : "复制失败";
    this.copyBtn.dataset.copied = ok ? "true" : "false";
    this.callbacks.onCopyContinuation(
      ok
        ? "已复制续命提示词，新开一个 Task 粘贴就能无缝继续啦～"
        : "剪贴板写入失败，请再点一次试试",
    );
    window.setTimeout(() => {
      this.copyBtnIconWrap.replaceChildren(createIcon("copy", 11));
      this.copyBtnLabel.textContent = "一键续命";
      this.copyBtn.dataset.copied = "false";
    }, 1800);
  }

  public setCatalog(chars: CatalogChar[]): void {
    this.charsGridEl.replaceChildren();
    this.charBtns.clear();

    if (chars.length === 0) {
      const empty = document.createElement("div");
      empty.className = "ip-empty-chars";
      const hint = document.createElement("span");
      hint.textContent = "本机尚未下载角色动画包，点击下方前往按需下载";
      const goBtn = document.createElement("button");
      goBtn.type = "button";
      goBtn.className = "ip-mini-pill-btn is-accent";
      goBtn.append(createIcon("download", 12));
      const goLbl = document.createElement("span");
      goLbl.textContent = "下载角色动画与语音";
      goBtn.appendChild(goLbl);
      goBtn.addEventListener("click", () => {
        void settingsSet("settingsOpen", { open: true, tab: "resources" }).catch(() => {});
      });
      empty.append(hint, goBtn);
      this.charsGridEl.appendChild(empty);
      return;
    }

    for (const ch of chars) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ip-char-btn";
      btn.dataset.active = ch.id === this.activeCharId ? "true" : "false";

      const badge = document.createElement("span");
      badge.className = "ip-char-avatar";
      badge.appendChild(createPixelAvatar(ch.id, 30, "ip-pixel-avatar"));

      const textWrap = document.createElement("span");
      textWrap.className = "ip-char-text";
      const name = document.createElement("span");
      name.className = "ip-char-name";
      name.textContent = ch.name;
      const meta = document.createElement("span");
      meta.className = "ip-char-count";
      const charInfo = getCharMeta(ch.id);
      meta.textContent = `${charInfo.shortTag} · ${ch.clips} 段`;
      textWrap.append(name, meta);

      const check = document.createElement("span");
      check.className = "ip-char-check";
      check.appendChild(createIcon("check", 12));

      btn.append(badge, textWrap, check);
      btn.addEventListener("click", () => {
        void this.handleSelectChar(ch.id);
      });

      this.charsGridEl.appendChild(btn);
      this.charBtns.set(ch.id, btn);
    }
  }

  public setActiveCharacter(charId: string, name: string, clipCount: number): void {
    this.activeCharId = charId;
    const meta = getCharMeta(charId);
    this.petAvatarEl.replaceChildren(
      createPixelAvatar(charId, 36, "ip-pixel-avatar"),
    );
    this.petHeaderNameEl.textContent = name;
    this.petHeaderMetaEl.textContent = `${meta.shortTag} · ${clipCount} 段动作`;
    for (const [id, btn] of this.charBtns.entries()) {
      btn.dataset.active = id === charId ? "true" : "false";
    }
  }

  public setStats(stats: PetStats): void {
    for (const item of STAT_ITEMS) {
      const refs = this.statRefs.get(item.key);
      if (!refs) continue;
      const clamped = Math.max(0, Math.min(100, Math.round(stats[item.key])));
      refs.valEl.textContent = String(clamped);
      refs.fillEl.style.width = `${clamped}%`;
    }
  }

  private async handleSelectChar(charId: string): Promise<void> {
    if (this.switchingChar || charId === this.activeCharId) return;
    this.switchingChar = true;
    for (const [id, btn] of this.charBtns.entries()) {
      btn.dataset.active = id === charId ? "true" : "false";
    }
    try {
      await this.callbacks.onSelectChar(charId);
    } finally {
      this.switchingChar = false;
    }
  }

  public setCharacterClips(charState: CharState): void {
    this.clips = [...charState.clips].sort((a, b) => a.id.localeCompare(b.id, "zh-CN"));
    this.selectedCategory = "all";
    this.searchQuery = "";
    this.albumSearchEl.value = "";
    this.albumClearBtn.hidden = true;
    this.renderAlbumCategories();
    this.renderAlbumList();
  }

  public setPlayingClipId(clipId: string | null): void {
    this.activeClipId = clipId;
    const rows = this.albumListEl.querySelectorAll<HTMLButtonElement>(".ip-clip-row");
    for (const row of rows) {
      const playing = row.dataset.clipId === clipId;
      row.dataset.playing = playing ? "true" : "false";
      const eq = row.querySelector<HTMLElement>(".ip-clip-eq");
      if (eq) {
        eq.hidden = !playing;
      }
    }
  }

  private renderAlbumCategories(): void {
    this.albumCatsEl.replaceChildren();
    const counts = new Map<string, number>();
    for (const c of this.clips) {
      counts.set(c.category, (counts.get(c.category) ?? 0) + 1);
    }
    const order: string[] = CONTRACT_CATEGORIES.map((c) => c.id).filter(
      (cat) => (counts.get(cat) ?? 0) > 0,
    );
    for (const cat of counts.keys()) {
      if (!order.includes(cat)) order.push(cat);
    }

    const addChip = (key: string, label: string, count: number) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ip-cat-chip";
      btn.dataset.active = this.selectedCategory === key ? "true" : "false";
      btn.textContent = `${label} ${count}`;
      btn.addEventListener("click", () => {
        this.selectedCategory = key;
        this.renderAlbumCategories();
        this.renderAlbumList();
      });
      this.albumCatsEl.appendChild(btn);
    };

    addChip("all", "全部", this.clips.length);
    for (const cat of order) {
      addChip(cat, getCategoryLabel(cat), counts.get(cat) ?? 0);
    }
  }

  private renderAlbumList(): void {
    const q = this.searchQuery;
    const filtered = this.clips.filter((c) => {
      if (this.selectedCategory !== "all" && c.category !== this.selectedCategory) {
        return false;
      }
      if (!q) return true;
      const hay = `${c.id} ${c.category} ${getCategoryLabel(c.category)} ${c.tags.join(" ")}`.toLowerCase();
      return hay.includes(q);
    });

    this.albumCountEl.textContent = `${filtered.length}/${this.clips.length}`;
    this.albumListEl.replaceChildren();

    if (filtered.length === 0) {
      const empty = document.createElement("div");
      empty.className = "ip-empty-hint";
      empty.textContent = "没有找到匹配的动作";
      this.albumListEl.appendChild(empty);
      return;
    }

    const frag = document.createDocumentFragment();
    for (const clip of filtered) {
      const playing = clip.id === this.activeClipId;
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ip-clip-row";
      row.dataset.clipId = clip.id;
      row.dataset.playing = playing ? "true" : "false";

      const left = document.createElement("span");
      left.className = "ip-clip-left";

      const eq = document.createElement("span");
      eq.className = "mini-eq-wave ip-clip-eq";
      eq.hidden = !playing;
      eq.innerHTML = "<i></i><i></i><i></i>";

      const title = document.createElement("span");
      title.className = "ip-clip-title";
      title.textContent = clip.id;

      left.append(eq, title);

      const meta = document.createElement("span");
      meta.className = "ip-clip-meta";
      const dur = formatDuration(clip.durationMs);
      if (clip.qa?.status === "fail") {
        const warn = document.createElement("span");
        warn.className = "ip-clip-badge-warn";
        warn.textContent = "仅预览";
        meta.appendChild(warn);
      }
      if (dur) {
        const d = document.createElement("span");
        d.textContent = dur;
        meta.appendChild(d);
      }

      row.append(left, meta);
      row.addEventListener("click", () => {
        this.callbacks.onPlayClip(clip.id);
      });
      frag.appendChild(row);
    }
    this.albumListEl.appendChild(frag);
  }

  public setChatConfig(cfg: ChatAiConfig): void {
    this.chatConfig = { ...cfg };
    this.syncAiDrawerUi();
  }

  public openChatSettingsDrawer(): void {
    void settingsSet("settingsOpen", { open: true }).catch(() => {});
  }

  public focusChatInput(): void {
    this.switchTab("chat");
    window.requestAnimationFrame(() => {
      this.chatInputEl.focus();
      this.chatInputEl.select();
    });
  }

  public setPomodoro(next: PomodoroStatusPayload): void {
    const mm = String(Math.floor(next.remainingSec / 60)).padStart(2, "0");
    const ss = String(next.remainingSec % 60).padStart(2, "0");
    this.pomodoroTimeEl.textContent = `${mm}:${ss}`;
    this.pomodoroCountEl.textContent = `今日 ${next.completedCount} 个`;
    const total = Math.max(1, next.totalSec);
    const pct = next.active ? ((total - next.remainingSec) / total) * 100 : 0;
    this.pomodoroFillEl.style.width = `${Math.max(0, Math.min(100, pct)).toFixed(1)}%`;
    this.pomodoroToggleBtn.textContent = !next.active
      ? `开始 ${Math.round(total / 60)}m`
      : next.paused
        ? "继续"
        : "暂停";
    const activeMins = Math.round(total / 60);
    for (const [m, btn] of this.pomodoroPresetBtns.entries()) {
      btn.dataset.active = m === activeMins ? "true" : "false";
    }
  }

  private syncAiDrawerUi(): void {
    const hasKey = Boolean(this.chatConfig.apiKey?.trim());
    const providerLabel =
      this.chatConfig.provider === "deepseek"
        ? "DeepSeek"
        : this.chatConfig.provider === "gemini"
          ? "Gemini"
          : this.chatConfig.provider === "qwen"
            ? "千问"
            : this.chatConfig.provider === "glm"
              ? "智谱"
              : this.chatConfig.provider === "kimi"
                ? "Kimi"
                : this.chatConfig.provider === "siliconflow"
                  ? "硅基"
                  : this.chatConfig.provider === "openai"
                    ? "OpenAI"
                    : "自定义";
    this.aiModeBadgeEl.textContent = hasKey ? `AI · ${providerLabel}` : "人设模板";
    this.aiModeBadgeEl.dataset.ai = hasKey ? "true" : "false";

    this.aiVoiceBadgeEl.textContent = !this.chatConfig.ttsEnabled
      ? "语音关"
      : this.chatConfig.ttsMode === "local"
        ? "本地语音包"
        : this.chatConfig.ttsMode === "gpt_sovits"
          ? "克隆声优"
          : "Azure 女声";
    this.aiVoiceBadgeEl.dataset.on = this.chatConfig.ttsEnabled ? "true" : "false";
  }

  public appendChatMessage(msg: ChatLogMessage): void {
    if (this.chatMsgIds.has(msg.id)) return;
    this.chatMsgIds.add(msg.id);

    const emptyHint = this.chatFeedEl.querySelector(".ip-empty-hint");
    if (emptyHint) emptyHint.remove();

    const row = document.createElement("div");
    row.className = `ip-chat-msg ${msg.role === "user" ? "is-user" : "is-bot"}`;

    if (msg.role === "assistant") {
      const avatar = document.createElement("span");
      avatar.className = "ip-chat-avatar";
      avatar.appendChild(createPixelAvatar(msg.charId, 24, "ip-pixel-avatar"));
      row.appendChild(avatar);
    }

    const bubble = document.createElement("div");
    bubble.className = "ip-chat-bubble";

    const textEl = document.createElement("span");
    textEl.className = "ip-chat-text";
    textEl.textContent = msg.text;
    bubble.appendChild(textEl);

    if (msg.role === "assistant") {
      const meta = document.createElement("div");
      meta.className = "ip-chat-meta";

      const tag = document.createElement("span");
      tag.className = "ip-chat-source";
      tag.textContent =
        msg.source === "ai"
          ? `${msg.charName} · AI`
          : msg.source === "fallback"
            ? `${msg.charName} · 兜底`
            : `${msg.charName} · 模板`;

      const speakBtn = document.createElement("button");
      speakBtn.type = "button";
      speakBtn.className = "ip-chat-speak-btn";
      speakBtn.title = "重播语音";
      speakBtn.appendChild(createIcon("volumeOn", 11));
      speakBtn.addEventListener("click", () => {
        void settingsSet("ttsPreview", msg.text).catch(() => {});
      });

      meta.append(tag, speakBtn);
      bubble.appendChild(meta);
    }

    row.appendChild(bubble);
    this.chatFeedEl.appendChild(row);
    while (this.chatFeedEl.children.length > 40) {
      this.chatFeedEl.firstElementChild?.remove();
    }
    this.chatFeedEl.scrollTop = this.chatFeedEl.scrollHeight;
  }

  public clearChatMessages(): void {
    this.chatMsgIds.clear();
    this.chatFeedEl.replaceChildren();
    const emptyChatHint = document.createElement("div");
    emptyChatHint.className = "ip-empty-hint";
    emptyChatHint.textContent = "已清空对话记忆，随时开始新的聊天吧";
    this.chatFeedEl.appendChild(emptyChatHint);
  }

  private async submitChat(text: string): Promise<void> {
    if (this.chatSending) return;
    this.chatSending = true;
    this.chatSendBtn.disabled = true;
    try {
      await this.callbacks.onChatSend(text);
    } finally {
      this.chatSending = false;
      this.chatSendBtn.disabled = false;
    }
  }

  public destroy(): void {
    if (this.tickTimerId !== 0) {
      window.clearInterval(this.tickTimerId);
      this.tickTimerId = 0;
    }
    this.el.remove();
  }
}
