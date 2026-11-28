/**
 * Deskpet V1.2 IPC 契约（唯一交接物）
 *
 * 严格对齐 docs/v1_contract.md v1.2 §5 & §9：
 *   - 11 个 Tauri command：catalog_get / char_switch / pet_state_get / pet_interact /
 *     action_play / settings_set / chat_send / hit_test_mask /
 *     telemetry_get / telemetry_refresh / telemetry_set_source
 *   - 5 个 event：play / bubble / stats / telemetry（Rust → 前端）、frame_hint（前端 → Rust）
 *
 * Rust 端同名实现见 src-tauri/src/lib.rs；字段命名与返回结构不得单方面修改。
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

/* ------------------------------------------------------------------ */
/* 数据模型                                                            */
/* ------------------------------------------------------------------ */

export interface CatalogChar {
  id: string;
  name: string;
  clips: number;
  path: string;
}

export interface Catalog {
  schemaVersion: number;
  chars: CatalogChar[];
  generatedAt: string;
}

export type QaStatus = "pass" | "warn" | "fail";

export interface QaInfo {
  status: QaStatus;
  greenEdge: number;
  holeRatio: number;
}

/** [x, y, w, h] */
export type Box = [number, number, number, number];
/** 图像归一化坐标（左上原点、y 向下） */
export type Vec2Tuple = [number, number];

export interface ClipMeta {
  schemaVersion: number;
  id: string;
  char: string;
  category: string;
  legacyCategory: string;
  tags: string[];
  frames: number;
  fps: number;
  durationMs: number;
  size: [number, number];
  bodyBox: Box;
  fullBox: Box;
  anchor: Vec2Tuple;
  loop: boolean;
  interruptAfterMs: number;
  mirrorable: boolean;
  audio: string | null;
  qa: QaInfo;
  hash: string;
  /** v1.3：跳过片头黑雾/未抠净首帧的安全起播偏移（ms），0 表示从第 0 帧直接起播 */
  safeStartMs?: number;
  /** v2.2：持续型动作（睡觉/工作等）的中段无缝循环窗口 [startMs, endMs] */
  sustainLoop?: [number, number] | null;
}

export interface Bindings {
  idle: string[];
  click: string[];
  drag: string[];
  moveLeft: string[];
  moveRight: string[];
  work: string[];
  eat: string[];
  sleep: string[];
  wake: string[];
}

export interface Schedule {
  wake: string;
  lunch: string;
  dinner: string;
  sleep: string;
}

export interface CharacterFile {
  schemaVersion: number;
  id: string;
  name: string;
  bindings: Bindings;
  schedule: Schedule;
  defaultScale: number;
}

/** char_switch 的返回：角色文件 + 该角色全部 clip 元数据（相册/调度共用）。 */
export interface CharState {
  charId: string;
  name: string;
  bindings: Bindings;
  clips: ClipMeta[];
  defaultScale: number;
}

export interface PetStats {
  satiety: number;
  energy: number;
  mood: number;
  affinity: number;
}

export interface PetPos {
  x: number;
  y: number;
}

export interface PetState {
  stats: PetStats;
  pos: PetPos;
  scale: number;
  pinned: boolean;
  muted: boolean;
}

export type InteractKind = "click" | "dragStart" | "dragEnd" | "feed";

/* ------------------------------------------------------------------ */
/* Event 载荷                                                          */
/* ------------------------------------------------------------------ */

export interface PlayEvent {
  /** v1.3：附带角色 ID，前端收到跨角色抢跑 play 时可暂存避免 404 空播 */
  charId?: string;
  clipId: string;
  startAtMs: number;
  mirror: boolean;
  audio: string | null;
  /** v2.1：状态锁定循环段，无视 meta.loop 原地循环直至下一动作到达 */
  holdLoop?: boolean;
}

export interface BubbleEvent {
  text: string;
  ttlMs: number;
}

export type StatsEvent = PetStats;

export interface FrameHint {
  clipId: string;
  frame: number;
}

export interface ChatReply {
  clipId: string;
  bubble: string;
}

/* ------------------------------------------------------------------ */
/* 8 个 command                                                        */
/* ------------------------------------------------------------------ */

/** 读取 assets/catalog.json。 */
export function catalogGet(): Promise<Catalog> {
  return invoke<Catalog>("catalog_get");
}

/** 切换当前角色，返回角色文件与该角色全部 clip 元数据。 */
export function charSwitch(charId: string): Promise<CharState> {
  return invoke<CharState>("char_switch", { charId });
}

/** 读取四维数值、位置、缩放、置顶与静音状态。 */
export function petStateGet(): Promise<PetState> {
  return invoke<PetState>("pet_state_get");
}

/** 点击 / 拖拽开始 / 拖拽结束 / 喂食。 */
export function petInteract(
  kind: InteractKind,
  x: number,
  y: number,
): Promise<PetState> {
  return invoke<PetState>("pet_interact", { kind, x, y });
}

/** 相册或指令触发播放。 */
export function actionPlay(clipId: string): Promise<boolean> {
  return invoke<boolean>("action_play", { clipId });
}

/** 持久化设置项。 */
export function settingsSet(key: string, value: unknown): Promise<boolean> {
  return invoke<boolean>("settings_set", { key, value });
}

/** 模板气泡对话；返回要播放的 clipId 与气泡文本。 */
export function chatSend(text: string): Promise<ChatReply> {
  return invoke<ChatReply>("chat_send", { text });
}

/**
 * 取回 clip 的 mask.bin 字节（前端缓存）。
 * Rust 端对 > 2MB 的掩码直接拒绝加载（契约 v1.1 §2.8）。
 */
export async function hitTestMask(clipId: string): Promise<Uint8Array> {
  const bytes = await invoke<number[]>("hit_test_mask", { clipId });
  return Uint8Array.from(bytes);
}

/* ------------------------------------------------------------------ */
/* 4 个 event                                                          */
/* ------------------------------------------------------------------ */

/** Rust 调度器要求前端播放某段。返回取消监听的函数。 */
export function onPlay(handler: (payload: PlayEvent) => void): Promise<UnlistenFn> {
  return listen<PlayEvent>("play", (event) => handler(event.payload));
}

/** Rust 要求前端显示气泡。 */
export function onBubble(handler: (payload: BubbleEvent) => void): Promise<UnlistenFn> {
  return listen<BubbleEvent>("bubble", (event) => handler(event.payload));
}

/** 四维数值变化。 */
export function onStats(handler: (payload: StatsEvent) => void): Promise<UnlistenFn> {
  return listen<StatsEvent>("stats", (event) => handler(event.payload));
}

/** 前端每 100ms 上报当前 clip 与帧号，供 Rust 做像素穿透命中测试。 */
export function reportFrameHint(hint: FrameHint): Promise<void> {
  return emit("frame_hint", hint);
}

/* ------------------------------------------------------------------ */
/* 资产地址（assetProtocol，零拷贝流式播放）                            */
/* ------------------------------------------------------------------ */

/**
 * 运行期动态资产根目录。默认由 Rust 后端通过 `pet_assets_root` / `pet_assets_status` 下发
 * （开发态为工作区 ./assets，打包态为可写用户目录 %APPDATA%/Deskpet/assets），
 * 仅在构造 asset:// URL 时使用，不会把整段视频读进 JS 内存。
 */
let runtimeAssetsRoot = (
  (import.meta.env.VITE_DESKPET_ASSETS as string | undefined) ?? ""
)
  .replace(/\\/g, "/")
  .replace(/\/+$/, "");

export function setAssetsRoot(rootPath: string): void {
  const cleaned = rootPath.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  if (cleaned) {
    runtimeAssetsRoot = cleaned;
  }
}

export function getAssetsRoot(): string {
  return runtimeAssetsRoot;
}

export function assetUrl(relativePath: string): string {
  const rel = relativePath.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!runtimeAssetsRoot) {
    return `./assets/${rel}`;
  }
  return convertFileSrc(`${runtimeAssetsRoot}/${rel}`);
}

/** 透明 WebM 的流式地址。 */
export function clipUrl(charId: string, clipId: string): string {
  return assetUrl(`chars/${charId}/clips/${clipId}.webm`);
}

/** 音轨 OGG 的流式地址（audio 字段来自 ClipMeta）；无音轨时返回 null。 */
export function clipAudioUrl(charId: string, audio: string | null): string | null {
  if (!audio) return null;
  return assetUrl(`chars/${charId}/${audio}`);
}

/** mask.bin 的流式地址（通常走 hit_test_mask command 拿字节，不直接 fetch）。 */
export function clipMaskUrl(charId: string, clipId: string): string {
  return assetUrl(`chars/${charId}/clips/${clipId}.mask.bin`);
}

/* ------------------------------------------------------------------ */
/* v1.2 Agent 遥测模型（UnifiedState）与常驻热区穿透（uiHitRects）     */
/* ------------------------------------------------------------------ */

export type TelemetryStatus =
  | "offline"
  | "idle"
  | "streaming"
  | "tool_running"
  | "waiting_approval"
  | "error";

export type AgentActivityKind =
  | "process_offline"
  | "service_connecting"
  | "service_reconnecting"
  | "session_init"
  | "idle"
  | "ttft_waiting"
  | "planning_mode"
  | "deep_reasoning"
  | "research_lookup"
  | "code_streaming"
  | "code_diffing"
  | "terminal_running"
  | "build_long_running"
  | "terminal_error"
  | "browser_running"
  | "mcp_tool_calling"
  | "workflow_running"
  | "approval_terminal"
  | "approval_outside_dir"
  | "artifact_review_ready"
  | "task_completed"
  | "context_delta_spike"
  | "context_warning_80"
  | "quota_low_warning"
  | "quota_exhausted"
  | "api_auth_rate_error";

export type TelemetryConfidence = "exact" | "estimated" | "unavailable";

export type TelemetryErrorKind =
  | "auth"
  | "region"
  | "rate_limit"
  | "network"
  | "unknown";

export interface TelemetryCapabilities {
  hasQuota: boolean;
  hasMultiPool: boolean;
  hasRealtimeSpeed: boolean;
  hasTtft: boolean;
  hasContext: boolean;
  hasApproval: boolean;
}

export interface TelemetryModelInfo {
  name: string | null;
  contextLimit: number | null;
}

export interface TelemetryQuotaPool {
  id: string;
  label: string;
  remainingPct: number;
  resetAtMs: number | null;
  isActive: boolean;
}

export interface TelemetryQuotaInfo {
  remainingPct: number | null;
  resetAtMs: number | null;
  pools: TelemetryQuotaPool[];
  confidence: TelemetryConfidence;
}

export interface TelemetrySpeedInfo {
  tokensPerSec: number | null;
  ttftMs: number | null;
  confidence: TelemetryConfidence;
}

export interface TelemetryContextInfo {
  usedTokens: number | null;
  limitTokens: number | null;
  usedPct: number | null;
  deltaTokens: number | null;
  confidence: TelemetryConfidence;
}

export interface TelemetryError {
  code: number | null;
  kind: TelemetryErrorKind;
  message: string;
}

export interface TelemetryAlerts {
  contextHigh: boolean;
  quotaLow: boolean;
  waitingApproval: boolean;
  hasError: boolean;
}

export interface UnifiedState {
  source: string;
  availableSources: string[];
  status: TelemetryStatus;
  statusSinceMs: number;
  activeStepLabel: string | null;
  activity: AgentActivityKind;
  activeAgents: number;
  completedAgents: number;
  workspaceName: string | null;
  stepDetail: string | null;
  capabilities: TelemetryCapabilities;
  model: TelemetryModelInfo;
  quota: TelemetryQuotaInfo;
  speed: TelemetrySpeedInfo;
  context: TelemetryContextInfo;
  error: TelemetryError | null;
  alerts: TelemetryAlerts;
  updatedAtMs: number;
  stale: boolean;
}

/** 窗口内归一化矩形（0..1），用于穿透层对常驻迷你徽章放行点击/悬停。 */
export interface UiHitRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 获取当前最新遥测状态快照。 */
export function telemetryGet(): Promise<UnifiedState> {
  return invoke<UnifiedState>("telemetry_get");
}

/** 手动强刷一次最新遥测状态与额度。 */
export function telemetryRefresh(): Promise<UnifiedState> {
  return invoke<UnifiedState>("telemetry_refresh");
}

/** 切换遥测数据源（首版固定 "antigravity"）。 */
export function telemetrySetSource(source: string): Promise<UnifiedState> {
  return invoke<UnifiedState>("telemetry_set_source", { source });
}

/** 监听 Rust 推送的 UnifiedState 变更（Diff 推送）。 */
export function onTelemetry(
  handler: (payload: UnifiedState) => void,
): Promise<UnlistenFn> {
  return listen<UnifiedState>("telemetry", (event) => handler(event.payload));
}

/** 上报常驻 UI 控件（如迷你状态徽章）的窗口归一化命中矩形。 */
export function setUiHitRects(rects: UiHitRect[]): Promise<boolean> {
  return settingsSet("uiHitRects", rects);
}

/* ------------------------------------------------------------------ */
/* v2.2 AI 聊天与三层神经语音模型（走 settings_set 与内部事件）        */
/* ------------------------------------------------------------------ */

export interface FishSpeechCharProfile {
  refAudioPath: string;
  promptText: string;
  promptLang: string;
  textLang: string;
  referenceId: string;
}

export interface FishSpeechConfig {
  enabled: boolean;
  installDir: string;
  apiUrl: string;
  apiKey: string;
  checkpointDir: string;
  decoderCheckpoint: string;
  decoderConfig: string;
  device: string;
  half: boolean;
  timeoutSecs: number;
  chunkLength: number;
  topP: number;
  temperature: number;
  repetitionPenalty: number;
  maxNewTokens: number;
  useMemoryCache: string;
  downloadSource: string;
  gpuBackend?: string;
  autoStartService: boolean;
  autoStopOnExit: boolean;
  dailyDynamicVoice?: boolean;
  charProfiles: Record<string, FishSpeechCharProfile>;
}

export interface AssetCharPackStatus {
  id: string;
  name: string;
  archiveName: string;
  approxSizeMb: number;
  installed: boolean;
  clipCount: number;
  voiceReady: boolean;
}

export interface AssetsStatusPayload {
  assetsRoot: string;
  githubRepo: string;
  releaseTag: string;
  downloadSource: "mirror" | "official" | string;
  installing: boolean;
  voicePackArchive: string;
  voicePackInstalled: boolean;
  chars: AssetCharPackStatus[];
}

export interface AssetsProgressPayload {
  active: boolean;
  target: string;
  stage: "downloading" | "extracting" | "done" | "cancelled" | "error";
  label: string;
  itemIndex: number;
  itemTotal: number;
  downloadedBytes: number;
  totalBytes: number;
  percent: number;
  speedBps: number;
  message: string;
}

export interface FishSpeechStatusPayload {
  state: "uninstalled" | "installing" | "ready" | "running" | string;
  label: string;
  installed: boolean;
  pythonReady: boolean;
  modelReady: boolean;
  running: boolean;
  pid: number | null;
  apiUrl: string;
  installDir: string;
  checkpointDir: string;
  detectedCheckpoints: string[];
  error: string | null;
}

export interface FishSpeechProgressPayload {
  active: boolean;
  stage: string;
  message: string;
  percent: number;
  downloadedBytes: number;
  totalBytes: number;
  speedBps: number;
  done: boolean;
  error: string | null;
}

export type BuiltinVoiceLang = "ja" | "zh";
export type VramMode = "zero_vram" | "balanced" | "full_realtime";
export type CompanionVoiceMode = "quiet" | "balanced" | "lively";

export interface VramProcessInfo {
  pid: number;
  name: string;
  usedMb: number;
  source: "gpu" | "ollama" | "moemate" | string;
}

export interface VramStatusPayload {
  available: boolean;
  gpuName: string;
  backend: string;
  usedMb: number;
  totalMb: number;
  usedPct: number;
  moemateFishSpeechRunning: boolean;
  moemateFishSpeechPid: number | null;
  moemateEstimatedMb: number;
  processes: VramProcessInfo[];
  advice: string;
  updatedAtMs: number;
}

export interface ChatAiConfig {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  ttsEnabled: boolean;
  autoPlayVoice: boolean;
  ttsMode: "azure" | "fish_speech" | "edge" | "local";
  azureSpeechKey: string;
  azureSpeechRegion: string;
  customVoice: string;
  charVoices: Record<string, string>;
  customPersonas: Record<string, string>;
  fishSpeech?: FishSpeechConfig;
  builtinVoiceLang?: BuiltinVoiceLang;
  vramMode?: VramMode;
  vramAutoUnloadMinutes?: number;
  ideVoiceEnabled?: boolean;
  companionVoiceMode?: CompanionVoiceMode;
  clickSfxEnabled?: boolean;
}

export interface ChatLogMessage {
  id: number;
  role: "user" | "assistant";
  text: string;
  charId: string;
  charName: string;
  source: "user" | "ai" | "template" | "fallback";
  clipId: string;
  tsMs: number;
}

export interface PetTtsPayload {
  charId: string;
  text: string;
  kind: "ogg_base64" | "mp3_base64" | "wav_base64" | "asset_url" | "web_speech";
  data: string;
  rate: number;
  pitch: number;
  force?: boolean;
}

export interface PomodoroStatusPayload {
  active: boolean;
  paused: boolean;
  phase: "idle" | "focus" | string;
  remainingSec: number;
  totalSec: number;
  completedCount: number;
}
