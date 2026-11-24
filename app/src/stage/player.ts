import {
  assetUrl,
  clipUrl,
  reportFrameHint,
  type AgentActivityKind,
  type CharState,
  type ClipMeta,
  type PetTtsPayload,
  type PlayEvent,
  type TelemetryStatus,
  type UnifiedState,
} from "../shared/ipc";

const CROSSFADE_CLIP_MS = 120;
const CROSSFADE_CHAR_MS = 80;
const CROSSFADE_AFTER_DRAG_MS = 75;
const SUSTAIN_CROSSFADE_MS = 360;
const SUSTAIN_PREPARE_LEAD_SEC = 0.85;
const SUSTAIN_FADE_LEAD_SEC = SUSTAIN_CROSSFADE_MS / 1000;
const FRAME_HINT_INTERVAL_MS = 100;
const BASE_VIDEO_WIDTH = 1280;
const BASE_VIDEO_HEIGHT = 720;
const EDGE_PEEK_BASE_TILT_RAD = (14.0 * Math.PI) / 180;
const EDGE_PEEK_HOVER_EXTRA_TILT_RAD = (3.5 * Math.PI) / 180;
const EDGE_PEEK_HOVER_SLIDE_RATIO = 0.044;

/** 与 Rust scheduler.rs 严格同序，确保预热槽 100% 命中同一段拖拽动作 */
const LIVELY_DRAG_FALLBACKS: readonly string[] = [
  "被鼠标拖拽悬空反馈",
  "双手上举摇摆",
  "惊讶下巴掉",
  "小鹿受惊",
  "惊吓后缩",
  "得意炫耀转圈",
  "小兔蹦跳",
  "开合跳",
];

interface VideoSlot {
  video: HTMLVideoElement;
  clipId: string | null;
  meta: ClipMeta | null;
  mirror: boolean;
  safeStartSec: number;
  holdLoop: boolean;
  sustainStartSec: number | null;
  sustainEndSec: number | null;
  sustainPrepared: boolean;
}

interface OverlayParticle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  size: number;
  kind: "spark" | "plus" | "minus" | "zzz";
  color: string;
  startMs: number;
  durationMs: number;
}

interface ClickRipple {
  x: number;
  y: number;
  startMs: number;
  durationMs: number;
}

interface FeedParticle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  size: number;
  kind: "heart" | "spark";
  warm: boolean;
  delayMs: number;
  startMs: number;
  durationMs: number;
}

export interface BubbleAnchor {
  x: number;
  topY: number;
  headRightX: number;
  videoTopY: number;
  stageWidth: number;
  stageHeight: number;
}

function createHiddenVideo(): HTMLVideoElement {
  const video = document.createElement("video");
  video.playsInline = true;
  video.muted = true;
  video.preload = "auto";
  video.disablePictureInPicture = true;
  return video;
}

function releaseVideo(slot: VideoSlot): void {
  const { video } = slot;
  video.pause();
  video.onended = null;
  video.onerror = null;
  video.removeAttribute("src");
  while (video.firstChild) {
    video.removeChild(video.firstChild);
  }
  video.load();
  slot.clipId = null;
  slot.meta = null;
  slot.mirror = false;
  slot.safeStartSec = 0;
  slot.holdLoop = false;
  slot.sustainStartSec = null;
  slot.sustainEndSec = null;
  slot.sustainPrepared = false;
}

function waitVideoReady(
  video: HTMLVideoElement,
  isStale: () => boolean,
): Promise<boolean> {
  if (video.readyState >= 2) {
    return Promise.resolve(!isStale());
  }
  return new Promise<boolean>((resolve) => {
    let settled = false;
    const cleanup = () => {
      video.removeEventListener("loadeddata", onReady);
      video.removeEventListener("canplay", onReady);
      video.removeEventListener("error", onFail);
      window.clearTimeout(timer);
    };
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(ok && !isStale());
    };
    const onReady = () => finish(true);
    const onFail = () => finish(false);
    const timer = window.setTimeout(() => finish(video.readyState >= 2), 5000);

    video.addEventListener("loadeddata", onReady);
    video.addEventListener("canplay", onReady);
    video.addEventListener("error", onFail);
  });
}

/**
 * 等待 video seek 到干净安全帧后再上屏，根除片头未抠净首帧闪黑影。
 */
function waitVideoSeeked(
  video: HTMLVideoElement,
  targetSec: number,
  isStale: () => boolean,
): Promise<void> {
  if (video.currentTime >= targetSec - 0.02 && !video.seeking) {
    return Promise.resolve();
  }
  return new Promise<void>((resolve) => {
    let settled = false;
    const cleanup = () => {
      video.removeEventListener("seeked", onDone);
      video.removeEventListener("timeupdate", onTimeUpdate);
      video.removeEventListener("error", onDone);
      window.clearTimeout(timer);
    };
    const finish = () => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve();
    };
    const onDone = () => finish();
    const onTimeUpdate = () => {
      if (isStale() || video.currentTime >= targetSec - 0.02) {
        finish();
      }
    };
    const timer = window.setTimeout(finish, 360);

    video.addEventListener("seeked", onDone);
    video.addEventListener("timeupdate", onTimeUpdate);
    video.addEventListener("error", onDone);
  });
}

function resolvePrimaryDragClip(charState: CharState): ClipMeta | null {
  const byId = new Map<string, ClipMeta>();
  for (const clip of charState.clips) {
    byId.set(clip.id, clip);
  }
  const isPlayable = (c: ClipMeta | undefined): c is ClipMeta =>
    Boolean(c && c.qa?.status !== "fail");

  const exact = byId.get("被鼠标拖拽悬空反馈");
  if (isPlayable(exact)) return exact;

  for (const id of charState.bindings.drag ?? []) {
    const clip = byId.get(id);
    if (isPlayable(clip) && clip.category === "drag") return clip;
  }
  for (const name of LIVELY_DRAG_FALLBACKS) {
    const clip = byId.get(name);
    if (isPlayable(clip)) return clip;
  }
  for (const id of charState.bindings.drag ?? []) {
    const clip = byId.get(id);
    if (isPlayable(clip)) return clip;
  }
  return null;
}

export class StagePlayer {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly videoSink: HTMLDivElement;
  private readonly slots: [VideoSlot, VideoSlot];
  /** v1.5 专属预热拖拽视频槽：切换角色后静默预解码并停靠在干净首帧，拖拽时 <16ms 零延迟上屏 */
  private readonly dragSlot: VideoSlot;
  private activeVoiceAudio: HTMLAudioElement | null = null;
  private activeVoiceBlobUrl: string | null = null;
  private voiceSeq = 0;
  private readonly resizeObserver: ResizeObserver;

  private activeIndex: 0 | 1 | null = null;
  private outgoingIndex: 0 | 1 | null = null;
  private outgoingFromDrag = false;
  private crossfadeStartMs = 0;
  private crossfadeDurationMs = CROSSFADE_CLIP_MS;

  private charId = "";
  private clipMap = new Map<string, ClipMeta>();
  private pendingPlay: PlayEvent | null = null;
  private lastPlayPayload: PlayEvent | null = null;
  private safeStartStuckSinceMs = 0;
  private firstFrameWatchdogTimerId = 0;
  private resumeRetryTimers: number[] = [];
  private muted = false;
  private draggingMode = false;
  private dragInstantActive = false;
  private dragPrewarmReady = false;
  private dragPrewarmSeq = 0;
  private playbackSpeed = 1;
  private accentRgb = "139, 120, 166";
  private playSeq = 0;
  private sustainPrepSeq = 0;
  private sustainRelayInProgress = false;
  private lastContextDeltaUsedTokens: number | null = null;
  private justSwitchedChar = false;

  private rafId = 0;
  private hintTimerId = 0;
  private dragExitTimerId = 0;
  private dragPrewarmTimerId = 0;
  private ripples: ClickRipple[] = [];
  private feedParticles: FeedParticle[] = [];
  private overlayParticles: OverlayParticle[] = [];
  private lastOverlaySpawnMs = 0;

  private overlayActivity: AgentActivityKind = "process_offline";
  private overlayStatus: TelemetryStatus = "offline";
  private overlaySinceMs = Date.now();
  private overlayTps = 0;
  private overlayActiveAgents = 0;
  private overlayCompletedAgents = 0;
  private overlayContextHigh = false;
  private overlayQuotaLow = false;
  private edgeDockSide: "left" | "right" | null = null;
  private edgeHovered = false;
  private edgePeekDir: -1 | 0 | 1 = 0;
  private edgeDockBlend = 0;
  private edgeHoverBlend = 0;
  private currentEdgePeekAngleRad = 0;
  private currentEdgePeekShiftX = 0;
  private currentEdgePeekShiftY = 0;

  private squashPos = 0;
  private squashVel = 0;
  private lastRenderMs = 0;
  private clickSfxEnabled = true;
  private sfxCtx: AudioContext | null = null;
  private recentClickTimes: number[] = [];

  private cssWidth = 640;
  private cssHeight = 450;
  private dpr = 1;

  private onActiveClipChange?: (clipId: string | null) => void;
  private onVoiceStateChange?: (speaking: boolean) => void;

  constructor(
    canvas: HTMLCanvasElement,
    options?: {
      onActiveClipChange?: (clipId: string | null) => void;
      onVoiceStateChange?: (speaking: boolean) => void;
    },
  ) {
    const ctx = canvas.getContext("2d", { alpha: true });
    if (!ctx) {
      throw new Error("无法创建 Canvas 2D 上下文");
    }
    this.canvas = canvas;
    this.ctx = ctx;
    this.onActiveClipChange = options?.onActiveClipChange;
    this.onVoiceStateChange = options?.onVoiceStateChange;

    this.videoSink = document.createElement("div");
    this.videoSink.className = "video-buffer-sink";
    this.videoSink.setAttribute("aria-hidden", "true");

    const v0 = createHiddenVideo();
    const v1 = createHiddenVideo();
    const vDrag = createHiddenVideo();
    this.videoSink.append(v0, v1, vDrag);
    document.body.appendChild(this.videoSink);

    this.slots = [
      { video: v0, clipId: null, meta: null, mirror: false, safeStartSec: 0, holdLoop: false, sustainStartSec: null, sustainEndSec: null, sustainPrepared: false },
      { video: v1, clipId: null, meta: null, mirror: false, safeStartSec: 0, holdLoop: false, sustainStartSec: null, sustainEndSec: null, sustainPrepared: false },
    ];
    this.dragSlot = {
      video: vDrag,
      clipId: null,
      meta: null,
      mirror: false,
      safeStartSec: 0,
      holdLoop: false,
      sustainStartSec: null,
      sustainEndSec: null,
      sustainPrepared: false,
    };

    this.syncCanvasSize();
    this.resizeObserver = new ResizeObserver(() => this.syncCanvasSize());
    this.resizeObserver.observe(this.canvas);
    window.addEventListener("resize", this.handleWindowResize);

    this.rafId = window.requestAnimationFrame(this.renderLoop);
    this.hintTimerId = window.setInterval(
      this.emitFrameHint,
      FRAME_HINT_INTERVAL_MS,
    );
  }

  private handleWindowResize = (): void => {
    this.syncCanvasSize();
  };

  private syncCanvasSize(): void {
    const rect = this.canvas.getBoundingClientRect();
    const cssW = Math.max(1, Math.round(rect.width || window.innerWidth || 640));
    const cssH = Math.max(1, Math.round(rect.height || window.innerHeight || 450));
    const dpr = Math.min(window.devicePixelRatio || 1, 2.5);

    this.cssWidth = cssW;
    this.cssHeight = cssH;
    this.dpr = dpr;

    const targetW = Math.max(1, Math.round(cssW * dpr));
    const targetH = Math.max(1, Math.round(cssH * dpr));
    if (this.canvas.width !== targetW || this.canvas.height !== targetH) {
      this.canvas.width = targetW;
      this.canvas.height = targetH;
    }
  }

  public setAccentRgb(rgb: string): void {
    this.accentRgb = rgb;
  }

  public setEdgeDock(side: "left" | "right" | null): void {
    this.edgeDockSide = side;
    if (side === "left") {
      this.edgePeekDir = 1;
    } else if (side === "right") {
      this.edgePeekDir = -1;
    } else {
      this.edgeHovered = false;
    }
  }

  public setEdgeHover(hovered: boolean): void {
    this.edgeHovered = hovered;
  }

  private clearDragExitTimer(): void {
    if (this.dragExitTimerId !== 0) {
      window.clearTimeout(this.dragExitTimerId);
      this.dragExitTimerId = 0;
    }
  }

  /**
   * 零延迟拖拽起播：鼠标越过 4px 阈值当帧立刻把已预热解码的 `dragSlot`
   * 切到舞台顶层开播，无需等待 IPC 往返与 4–10MB WebM 冷解码。
   */
  public enterDragImmediately(): void {
    this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;
    this.sustainPrepSeq += 1;
    this.sustainRelayInProgress = false;
    this.draggingMode = true;
    this.edgeHovered = false;
    this.edgeDockBlend = 0;
    this.edgeHoverBlend = 0;
    this.currentEdgePeekAngleRad = 0;
    this.currentEdgePeekShiftX = 0;
    this.currentEdgePeekShiftY = 0;

    if (
      this.dragPrewarmReady &&
      this.dragSlot.clipId &&
      this.dragSlot.video.readyState >= 2
    ) {
      this.outgoingFromDrag = false;
      this.outgoingIndex = null;
      this.dragInstantActive = true;
      this.dragSlot.mirror = false;
      this.dragSlot.video.loop = true;
      this.dragSlot.video.playbackRate = 1;
      if (this.dragSlot.video.currentTime < this.dragSlot.safeStartSec - 0.015) {
        try {
          this.dragSlot.video.currentTime = this.dragSlot.safeStartSec;
        } catch {
          // ignore
        }
      }
      this.dragSlot.video.play().catch(() => {});
      this.onActiveClipChange?.(this.dragSlot.clipId);
      return;
    }

    if (this.activeIndex !== null) {
      const slot = this.slots[this.activeIndex];
      if (slot.clipId) {
        slot.video.loop = true;
      }
    }
  }

  public setDraggingMode(dragging: boolean): void {
    this.draggingMode = dragging;
    if (!dragging && this.dragInstantActive) {
      // 保留当前帧直到 Rust 的 dragEnd 切回 idle/work；160ms 后兜底自动恢复，绝不卡死在悬空态
      this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;
      this.dragExitTimerId = window.setTimeout(() => {
        this.dragExitTimerId = 0;
        if (!this.draggingMode && this.dragInstantActive) {
          this.cancelDragImmediately();
        }
      }, 160);
      return;
    }
    if (dragging) {
      this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;
    }
    if (this.activeIndex !== null) {
      const slot = this.slots[this.activeIndex];
      if (slot.clipId) {
        slot.video.loop = dragging || Boolean(slot.meta?.loop);
      }
    }
  }

  /** 右键或取消拖拽时立即退回原待机/工作槽并归位预热拖拽槽。 */
  public cancelDragImmediately(): void {
    this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;
    this.draggingMode = false;
    const wasInstant = this.dragInstantActive;
    if (wasInstant) {
      this.dragInstantActive = false;
      this.outgoingFromDrag = false;
      this.rearmDragSlot();
    }
    if (this.activeIndex !== null) {
      const slot = this.slots[this.activeIndex];
      if (slot.clipId) {
        const isClick = slot.meta?.category === "click";
        slot.video.loop = slot.holdLoop || (!isClick && Boolean(slot.meta?.loop));
        if (slot.video.paused) {
          slot.video.play().catch(() => {});
        }
        if (wasInstant) {
          this.onActiveClipChange?.(slot.clipId);
          this.emitFrameHint();
        }
      }
    }
  }

  /** 静默把当前角色的悬空拖拽视频预加载并停靠在安全干净首帧，随时待命 */
  private async prewarmDragClip(charState: CharState): Promise<void> {
    const seq = ++this.dragPrewarmSeq;
    const isStale = () => seq !== this.dragPrewarmSeq || this.charId !== charState.charId;

    this.dragPrewarmReady = false;
    this.dragInstantActive = false;
    this.outgoingFromDrag = false;
    releaseVideo(this.dragSlot);

    const meta = resolvePrimaryDragClip(charState);
    if (!meta) return;

    const safeStartSec = Math.max(0, (meta.safeStartMs ?? 0) / 1000);
    this.dragSlot.clipId = meta.id;
    this.dragSlot.meta = meta;
    this.dragSlot.mirror = false;
    this.dragSlot.safeStartSec = safeStartSec;

    const video = this.dragSlot.video;
    video.muted = true;
    video.loop = true;
    video.playbackRate = 1;
    video.src = clipUrl(charState.charId, meta.id);
    video.load();

    const ready = await waitVideoReady(video, isStale);
    if (!ready || isStale()) {
      if (isStale()) releaseVideo(this.dragSlot);
      return;
    }

    if (safeStartSec > 0.01 && Number.isFinite(video.duration) && video.duration > 0.1) {
      const targetSec = Math.min(safeStartSec, Math.max(0, video.duration - 0.08));
      if (targetSec > 0.01) {
        try {
          video.currentTime = targetSec;
          await waitVideoSeeked(video, targetSec, isStale);
        } catch {
          // ignore
        }
      }
    }

    if (isStale()) {
      releaseVideo(this.dragSlot);
      return;
    }

    video.pause();
    this.dragPrewarmReady = true;

    // 若用户恰好在预热完成前已经拎起拖拽，立刻无缝切入！
    if (this.draggingMode && !this.dragInstantActive) {
      this.enterDragImmediately();
    }
  }

  /** 拖拽结束后暂停并把预热槽重新归位到干净首帧，保证下一次拖拽依旧零延迟 */
  private rearmDragSlot(): void {
    const { video, safeStartSec } = this.dragSlot;
    video.pause();
    if (video.readyState >= 2) {
      try {
        video.currentTime = safeStartSec;
      } catch {
        // ignore
      }
    }
  }

  public setCharacter(charState: CharState): void {
    const isSwitching = this.charId !== "" && this.charId !== charState.charId;
    this.stopVoice();
    if (this.sfxCtx) {
      void this.sfxCtx.close().catch(() => {});
      this.sfxCtx = null;
    }
    if (isSwitching) {
      this.playSeq += 1;
      this.sustainPrepSeq += 1;
      this.sustainRelayInProgress = false;
      this.justSwitchedChar = true;
    }

    this.charId = charState.charId;
    this.clipMap.clear();
    for (const clip of charState.clips) {
      this.clipMap.set(clip.id, clip);
    }

    // 延迟 900ms 静默预热本角色拖拽动画槽，把首帧解码与磁盘带宽全额让给当前角色首播
    if (this.dragPrewarmTimerId !== 0) {
      window.clearTimeout(this.dragPrewarmTimerId);
      this.dragPrewarmTimerId = 0;
    }
    const warmChar = charState;
    this.dragPrewarmTimerId = window.setTimeout(() => {
      this.dragPrewarmTimerId = 0;
      if (this.charId === warmChar.charId) {
        void this.prewarmDragClip(warmChar);
      }
    }, 900);

    if (this.firstFrameWatchdogTimerId !== 0) {
      window.clearTimeout(this.firstFrameWatchdogTimerId);
      this.firstFrameWatchdogTimerId = 0;
    }
    const watchCharId = charState.charId;
    this.firstFrameWatchdogTimerId = window.setTimeout(() => {
      this.firstFrameWatchdogTimerId = 0;
      if (this.charId === watchCharId) {
        this.ensureFirstFrameVisible();
      }
    }, 1200);

    const pending = this.pendingPlay;
    this.pendingPlay = null;
    if (
      pending &&
      (!pending.charId || pending.charId === charState.charId) &&
      this.clipMap.has(pending.clipId)
    ) {
      void this.play(pending);
    }
  }

  public hasActiveOrLoadingClip(): boolean {
    return (
      this.activeIndex !== null ||
      this.pendingPlay !== null ||
      this.slots.some((s) => s.clipId !== null)
    );
  }

  public onAssetsRootReady(): void {
    if (this.activeIndex === null && this.charId) {
      const candidate = this.pendingPlay ?? this.lastPlayPayload;
      if (candidate && (!candidate.charId || candidate.charId === this.charId)) {
        void this.play(candidate);
      }
    }
  }

  public ensureFirstFrameVisible(): void {
    if (this.activeIndex !== null) {
      const activeSlot = this.slots[this.activeIndex];
      if (activeSlot.video.paused && activeSlot.video.readyState >= 2) {
        activeSlot.video.play().catch(() => {});
      }
      return;
    }
    if (!this.charId) return;
    const candidate = this.pendingPlay ?? this.lastPlayPayload;
    if (
      candidate &&
      (!candidate.charId || candidate.charId === this.charId) &&
      this.clipMap.has(candidate.clipId)
    ) {
      void this.play(candidate);
      return;
    }
    for (const clip of this.clipMap.values()) {
      if (clip.qa?.status !== "fail") {
        void this.play({
          charId: this.charId,
          clipId: clip.id,
          startAtMs: clip.safeStartMs ?? 0,
          mirror: false,
          audio: clip.audio ?? null,
        });
        return;
      }
    }
  }

  private clearResumeRetryTimers(): void {
    for (const id of this.resumeRetryTimers) {
      window.clearTimeout(id);
    }
    this.resumeRetryTimers = [];
  }

  public setMuted(muted: boolean): void {
    this.muted = muted;
    if (muted) {
      this.stopVoice();
    if (this.sfxCtx) {
      void this.sfxCtx.close().catch(() => {});
      this.sfxCtx = null;
    }
    }
    this.applyPlaybackSpeedToSlots();
  }

  /**
   * 仅静音模式下允许 0.95–1.20 温和倍速微调；
   * 非静音或存在音轨时强制 1.0，防止变调鬼畜。
   */
  public setPlaybackSpeed(rate: number): void {
    const safeRate = Number.isFinite(rate)
      ? Math.max(0.95, Math.min(1.2, rate))
      : 1;
    this.playbackSpeed = safeRate;
    this.applyPlaybackSpeedToSlots();
  }

  private resolveEffectivePlaybackSpeed(_slot: VideoSlot): number {
    return this.playbackSpeed;
  }

  private applyPlaybackSpeedToSlots(): void {
    for (const slot of this.slots) {
      if (!slot.clipId) continue;
      slot.video.playbackRate = this.resolveEffectivePlaybackSpeed(slot);
    }
  }

  public getCurrentClipId(): string | null {
    if (this.dragInstantActive && this.dragSlot.clipId) {
      return this.dragSlot.clipId;
    }
    if (this.activeIndex === null) return null;
    return this.slots[this.activeIndex].clipId;
  }

  public getCurrentClipMeta(): ClipMeta | null {
    if (this.dragInstantActive && this.dragSlot.meta) {
      return this.dragSlot.meta;
    }
    if (this.activeIndex === null) return null;
    return this.slots[this.activeIndex].meta;
  }

  public setClickSfxEnabled(enabled: boolean): void {
    this.clickSfxEnabled = enabled;
  }

  public triggerClickFeedback(clientX: number, clientY: number): void {
    this.spawnClickRipple(clientX, clientY);
    this.squashPos = Math.min(this.squashPos + 0.055, 0.14);
    this.squashVel = Math.min(this.squashVel + 1.45, 2.8);
    if (!this.muted && this.clickSfxEnabled) {
      this.playClickSfx();
    }
  }

  private playClickSfx(): void {
    try {
      const AudioCtx =
        window.AudioContext ||
        (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
      if (!AudioCtx) return;
      if (!this.sfxCtx) {
        this.sfxCtx = new AudioCtx();
      }
      const ctx = this.sfxCtx;
      if (ctx.state === "suspended") {
        void ctx.resume().catch(() => {});
      }

      const nowMs = performance.now();
      this.recentClickTimes = this.recentClickTimes.filter((t) => nowMs - t <= 3000);
      const comboIdx = Math.min(4, this.recentClickTimes.length);
      this.recentClickTimes.push(nowMs);

      const scaleHz = [523.25, 587.33, 659.25, 783.99, 880.0];
      const baseHz = scaleHz[comboIdx];

      const t0 = ctx.currentTime;
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.setValueAtTime(baseHz, t0);
      osc.frequency.exponentialRampToValueAtTime(baseHz * 1.18, t0 + 0.045);

      gain.gain.setValueAtTime(0.0001, t0);
      gain.gain.linearRampToValueAtTime(0.085, t0 + 0.008);
      gain.gain.exponentialRampToValueAtTime(0.0001, t0 + 0.065);

      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(t0);
      osc.stop(t0 + 0.07);
    } catch {
      // ignore Web Audio errors
    }
  }

  private isCurrentClipSleeping(): boolean {
    const meta = this.getCurrentClipMeta();
    if (!meta) return false;
    if (meta.tags?.includes("sleep")) return true;
    return /沉眠|打瞌睡|瞌睡|困倦|装睡/.test(meta.id);
  }

  public spawnClickRipple(clientX: number, clientY: number): void {
    const rect = this.canvas.getBoundingClientRect();
    this.ripples.push({
      x: clientX - rect.left,
      y: clientY - rect.top,
      startMs: performance.now(),
      durationMs: 480,
    });
  }

  /** 投喂点心时在角色周身飘起柔暖矢量爱心与星芒粒子 */
  public spawnFeedHearts(): void {
    const anchor = this.getBubbleAnchor();
    const baseX = anchor.x;
    const baseY = Math.max(anchor.videoTopY + 36, anchor.topY + 26);
    const now = performance.now();
    const offsets = [-28, -12, 2, 16, 30];
    for (let i = 0; i < offsets.length; i += 1) {
      const spread = offsets[i];
      this.feedParticles.push({
        x: baseX + spread,
        y: baseY + Math.abs(spread) * 0.3,
        vx: spread * 0.42,
        vy: -(34 + (i % 3) * 10),
        size: i % 2 === 0 ? 6.2 : 5.0,
        kind: i % 3 === 2 ? "spark" : "heart",
        warm: i % 2 === 0,
        delayMs: i * 65,
        startMs: now,
        durationMs: 950,
      });
    }
  }

  public stopVoice(): void {
    this.voiceSeq += 1;
    if (this.activeVoiceAudio) {
      this.activeVoiceAudio.onended = null;
      this.activeVoiceAudio.onerror = null;
      this.activeVoiceAudio.pause();
      this.activeVoiceAudio = null;
    }
    if (this.activeVoiceBlobUrl) {
      URL.revokeObjectURL(this.activeVoiceBlobUrl);
      this.activeVoiceBlobUrl = null;
    }
    this.onVoiceStateChange?.(false);
  }

  private base64ToBlobUrl(b64: string, mime: string): string {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) {
      bytes[i] = bin.charCodeAt(i);
    }
    return URL.createObjectURL(new Blob([bytes], { type: mime }));
  }

  public playVoice(payload: PetTtsPayload): void {
    if ((!payload.force && this.muted) || !payload.text.trim()) return;
    this.stopVoice();
    if (this.sfxCtx) {
      void this.sfxCtx.close().catch(() => {});
      this.sfxCtx = null;
    }
    const seq = this.voiceSeq;

    if (
      (payload.kind === "ogg_base64" ||
        payload.kind === "mp3_base64" ||
        payload.kind === "wav_base64") &&
      payload.data
    ) {
      try {
        const mime =
          payload.kind === "ogg_base64"
            ? "audio/ogg; codecs=opus"
            : payload.kind === "wav_base64"
              ? "audio/wav"
              : "audio/mpeg";
        const blobUrl = this.base64ToBlobUrl(payload.data, mime);
        const audio = new Audio(blobUrl);
        audio.volume = 0.95;
        this.activeVoiceAudio = audio;
        this.activeVoiceBlobUrl = blobUrl;

        const cleanup = () => {
          if (this.activeVoiceAudio === audio) {
            this.activeVoiceAudio = null;
            this.onVoiceStateChange?.(false);
          }
          if (this.activeVoiceBlobUrl === blobUrl) {
            URL.revokeObjectURL(blobUrl);
            this.activeVoiceBlobUrl = null;
          }
        };
        audio.onended = cleanup;
        audio.onerror = cleanup;
        this.onVoiceStateChange?.(true);
        audio.play().catch(cleanup);
        return;
      } catch {
        // ignore invalid base64
      }
    }

    if (payload.kind === "asset_url" && payload.data) {
      const audio = new Audio(assetUrl(payload.data));
      audio.volume = 0.95;
      this.activeVoiceAudio = audio;
      audio.onended = () => {
        if (this.activeVoiceAudio === audio) {
          this.activeVoiceAudio = null;
          this.onVoiceStateChange?.(false);
        }
      };
      audio.onerror = () => {
        if (this.activeVoiceAudio === audio) {
          this.activeVoiceAudio = null;
          this.onVoiceStateChange?.(false);
        }
      };
      this.onVoiceStateChange?.(true);
      audio.play().catch(() => {
        if (this.activeVoiceAudio === audio) {
          this.activeVoiceAudio = null;
          this.onVoiceStateChange?.(false);
        }
      });
    }
    void seq;
  }

  public async play(payload: PlayEvent): Promise<void> {
    // v1.3：若 char_switch 期间后端 play 事件抢跑，角色 ID 尚未同步，
    // 先暂存 pendingPlay，等角色挂载完成立刻补播，杜绝 404 空播。
    if (
      !this.charId ||
      (payload.charId && payload.charId !== this.charId) ||
      !this.clipMap.has(payload.clipId)
    ) {
      this.pendingPlay = payload;
      if (!this.charId || !payload.charId || payload.charId === this.charId) {
        this.lastPlayPayload = payload;
      }
      return;
    }
    this.lastPlayPayload = payload;

    // v1.5 零延迟拖拽去重：预热槽已抢先秒播同一段拖拽动作，直接无缝接管，不重载不卡顿
    if (
      this.draggingMode &&
      this.dragInstantActive &&
      this.dragSlot.clipId === payload.clipId
    ) {
      this.dragSlot.mirror = Boolean(payload.mirror);
      ++this.playSeq;
      this.onActiveClipChange?.(payload.clipId);
      this.emitFrameHint();
      return;
    }

    this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;

    const seq = ++this.playSeq;
    this.sustainPrepSeq += 1;
    this.sustainRelayInProgress = false;
    const isStale = () => seq !== this.playSeq;
    const charIdSnapshot = this.charId;

    const incomingIdx: 0 | 1 = this.activeIndex === 0 ? 1 : 0;
    const incomingSlot = this.slots[incomingIdx];

    if (this.outgoingIndex === incomingIdx) {
      this.outgoingIndex = null;
    }
    releaseVideo(incomingSlot);

    const meta = this.clipMap.get(payload.clipId) ?? null;
    const safeStartSec = Math.max(0, (meta?.safeStartMs ?? 0) / 1000);
    incomingSlot.clipId = payload.clipId;
    incomingSlot.meta = meta;
    incomingSlot.mirror = Boolean(payload.mirror);
    incomingSlot.safeStartSec = safeStartSec;
    incomingSlot.holdLoop = Boolean(payload.holdLoop);

    const video = incomingSlot.video;
    video.muted = true;
    const isClickReaction = meta?.category === "click";
    video.loop =
      this.draggingMode ||
      incomingSlot.holdLoop ||
      meta?.category === "drag" ||
      (!isClickReaction && Boolean(meta?.loop));
    video.playbackRate = this.resolveEffectivePlaybackSpeed(incomingSlot);
    video.src = clipUrl(charIdSnapshot, payload.clipId);
    video.load();

    const ready = await waitVideoReady(video, isStale);
    if (!ready) {
      if (isStale() && this.activeIndex !== incomingIdx) {
        releaseVideo(incomingSlot);
      }
      return;
    }

    if (incomingSlot.holdLoop && meta?.sustainLoop && meta.sustainLoop.length === 2) {
      const [rawStartMs, rawEndMs] = meta.sustainLoop;
      const startSec = Math.max(safeStartSec, rawStartMs / 1000);
      const endSec = rawEndMs / 1000;
      if (
        Number.isFinite(video.duration) &&
        endSec - startSec >= 1.8 &&
        endSec <= video.duration - 0.35
      ) {
        incomingSlot.sustainStartSec = startSec;
        incomingSlot.sustainEndSec = endSec;
      }
    }

    // 取 payload.startAtMs 与 meta.safeStartMs 最大值，严格等待 seeked 完成再上屏
    const requestedSec = payload.startAtMs > 0 ? payload.startAtMs / 1000 : 0;
    const rawTargetSec = Math.max(requestedSec, safeStartSec);
    if (rawTargetSec > 0.01 && Number.isFinite(video.duration) && video.duration > 0.1) {
      const targetSec = Math.min(rawTargetSec, Math.max(0, video.duration - 0.08));
      if (targetSec > 0.01) {
        try {
          video.currentTime = targetSec;
          await waitVideoSeeked(video, targetSec, isStale);
        } catch {
          // ignore seek error on unbuffered stream
        }
      }
    }

    if (isStale()) {
      if (this.activeIndex !== incomingIdx) {
        releaseVideo(incomingSlot);
      }
      return;
    }

    try {
      await video.play();
    } catch {
      if (isStale()) {
        if (this.activeIndex !== incomingIdx) {
          releaseVideo(incomingSlot);
        }
        return;
      }
      for (const delay of [180, 600, 1400]) {
        const tid = window.setTimeout(() => {
          if (!isStale() && video.paused) {
            video.play().catch(() => {});
          }
        }, delay);
        this.resumeRetryTimers.push(tid);
      }
      window.addEventListener(
        "pointerdown",
        () => {
          if (!isStale() && video.paused) {
            video.play().catch(() => {});
          }
        },
        { once: true },
      );
    }

    if (isStale()) {
      if (this.activeIndex !== incomingIdx) {
        releaseVideo(incomingSlot);
      }
      return;
    }

    if (this.dragInstantActive) {
      // 从预热拖拽槽平滑淡出回正常动作，淡出完毕后只暂停归位不销毁预热槽
      this.dragInstantActive = false;
      this.outgoingFromDrag = true;
      this.outgoingIndex = null;
      this.crossfadeStartMs = performance.now();
      this.crossfadeDurationMs = CROSSFADE_AFTER_DRAG_MS;
    } else {
      const prevActive = this.activeIndex;
      if (prevActive !== null && prevActive !== incomingIdx) {
        if (this.outgoingIndex !== null && this.outgoingIndex !== prevActive) {
          releaseVideo(this.slots[this.outgoingIndex]);
        }
        this.outgoingIndex = prevActive;
        this.outgoingFromDrag = false;
        this.crossfadeStartMs = performance.now();
        this.crossfadeDurationMs = this.justSwitchedChar
          ? CROSSFADE_CHAR_MS
          : CROSSFADE_CLIP_MS;
      } else {
        this.outgoingIndex = null;
        this.outgoingFromDrag = false;
      }
    }
    this.justSwitchedChar = false;

    this.activeIndex = incomingIdx;
    this.onActiveClipChange?.(payload.clipId);

    this.emitFrameHint();
  }

  private computeBodyRatios(meta: ClipMeta | null): {
    leftU: number;
    rightU: number;
    topV: number;
    bottomV: number;
  } {
    if (!meta || meta.bodyBox.length < 4) {
      return { leftU: 0.30, rightU: 0.70, topV: 0.12, bottomV: 0.90 };
    }
    const baseW = meta.size?.[0] > 0 ? meta.size[0] : BASE_VIDEO_WIDTH;
    const baseH = meta.size?.[1] > 0 ? meta.size[1] : BASE_VIDEO_HEIGHT;
    const [bx, by, bw, bh] = meta.bodyBox;
    if (!Number.isFinite(bx) || !Number.isFinite(by) || !Number.isFinite(bw) || !Number.isFinite(bh) || bw < 40 || bh < 40) {
      return { leftU: 0.30, rightU: 0.70, topV: 0.12, bottomV: 0.90 };
    }
    const leftU = Math.max(0.15, Math.min(0.72, bx / baseW));
    const rightU = Math.max(Math.min(leftU + 0.16, 0.85), Math.min(0.85, (bx + bw) / baseW));
    const topV = Math.max(0.04, Math.min(0.65, by / baseH));
    const bottomV = Math.max(Math.min(topV + 0.20, 0.98), Math.min(0.98, (by + bh) / baseH));
    return { leftU, rightU, topV, bottomV };
  }

  private computePeekPivotUv(meta: ClipMeta | null, mirror: boolean): [number, number] {
    const r = this.computeBodyRatios(meta);
    const rawPivotU = (r.leftU + r.rightU) * 0.5;
    const effPivotU = mirror ? 1 - rawPivotU : rawPivotU;
    const heightV = Math.max(0.20, r.bottomV - r.topV);
    const pivotV = Math.max(0.25, Math.min(0.82, r.topV + heightV * 0.52));
    return [effPivotU, pivotV];
  }

  public getBubbleAnchor(): BubbleAnchor {
    const stageW = this.cssWidth;
    const stageH = this.cssHeight;
    const scale = Math.min(stageW / BASE_VIDEO_WIDTH, stageH / BASE_VIDEO_HEIGHT);
    const drawW = BASE_VIDEO_WIDTH * scale;
    const drawH = BASE_VIDEO_HEIGHT * scale;
    const offsetX = (stageW - drawW) / 2;
    // v1.5：角色视频底对齐，顶部透明安全带专供头顶灵动岛
    const offsetY = stageH - drawH;

    const slot = this.dragInstantActive
      ? this.dragSlot
      : this.activeIndex !== null
        ? this.slots[this.activeIndex]
        : null;
    const meta = slot?.meta ?? null;

    let x: number;
    let topY: number;
    let headRightX: number;
    if (!meta || meta.bodyBox.length < 4) {
      x = stageW / 2;
      topY = offsetY + Math.max(18, drawH * 0.14);
      headRightX = stageW / 2 + drawW * 0.14;
    } else {
      const [bx, by, bw] = meta.bodyBox;
      const rawCenterX = bx + bw / 2;
      const centerX = slot?.mirror ? BASE_VIDEO_WIDTH - rawCenterX : rawCenterX;
      const halfHeadW = Math.min(bw * 0.42, 165);
      x = offsetX + centerX * scale;
      topY = offsetY + by * scale;
      headRightX = offsetX + (centerX + halfHeadW) * scale;
    }

    if (this.edgeDockBlend > 0.001 && Math.abs(this.currentEdgePeekAngleRad) > 0.0002) {
      const [pu, pv] = this.computePeekPivotUv(meta, Boolean(slot?.mirror));
      const pivotX = offsetX + pu * drawW;
      const pivotY = offsetY + pv * drawH;
      const cosT = Math.cos(this.currentEdgePeekAngleRad);
      const sinT = Math.sin(this.currentEdgePeekAngleRad);

      const rx1 = x - pivotX;
      const ry1 = topY - pivotY;
      const rotX = pivotX + this.currentEdgePeekShiftX + rx1 * cosT - ry1 * sinT;
      const rotTopY = pivotY + this.currentEdgePeekShiftY + rx1 * sinT + ry1 * cosT;

      const rx2 = headRightX - pivotX;
      const rotHeadRightX = pivotX + this.currentEdgePeekShiftX + rx2 * cosT - ry1 * sinT;

      x = rotX;
      topY = rotTopY;
      headRightX = rotHeadRightX;
    }

    return {
      x,
      topY,
      headRightX,
      videoTopY: offsetY,
      stageWidth: stageW,
      stageHeight: stageH,
    };
  }

  private emitFrameHint = (): void => {
    if (this.draggingMode) return;
    const slot = this.dragInstantActive
      ? this.dragSlot
      : this.activeIndex !== null
        ? this.slots[this.activeIndex]
        : null;
    if (!slot || !slot.clipId || slot.video.readyState < 2) return;

    const fps = slot.meta?.fps && slot.meta.fps > 0 ? slot.meta.fps : 30;
    const fallbackFrames = Math.max(
      1,
      Math.floor((Number.isFinite(slot.video.duration) ? slot.video.duration : 10) * fps),
    );
    const totalFrames =
      slot.meta?.frames && slot.meta.frames > 0 ? slot.meta.frames : fallbackFrames;

    const rawFrame = Math.floor(slot.video.currentTime * fps);
    const frame = Math.max(0, Math.min(totalFrames - 1, rawFrame));

    reportFrameHint({ clipId: slot.clipId, frame }).catch(() => {
      // non-Tauri preview mode
    });
  };

  private async prepareSustainStandby(
    standbyIdx: 0 | 1,
    activeSlot: VideoSlot,
  ): Promise<void> {
    if (
      !activeSlot.clipId ||
      activeSlot.sustainStartSec === null ||
      activeSlot.sustainEndSec === null ||
      !this.charId
    ) {
      return;
    }
    const seq = this.sustainPrepSeq;
    const playSeqSnap = this.playSeq;
    const charIdSnap = this.charId;
    const clipIdSnap = activeSlot.clipId;
    const targetSec = activeSlot.sustainStartSec;
    const standbySlot = this.slots[standbyIdx];
    const isStale = () =>
      seq !== this.sustainPrepSeq ||
      playSeqSnap !== this.playSeq ||
      charIdSnap !== this.charId ||
      this.activeIndex === standbyIdx;

    standbySlot.sustainPrepared = false;
    const video = standbySlot.video;

    if (standbySlot.clipId !== clipIdSnap || video.readyState < 2) {
      releaseVideo(standbySlot);
      standbySlot.clipId = clipIdSnap;
      standbySlot.meta = activeSlot.meta;
      standbySlot.mirror = activeSlot.mirror;
      standbySlot.safeStartSec = activeSlot.safeStartSec;
      standbySlot.holdLoop = activeSlot.holdLoop;
      standbySlot.sustainStartSec = activeSlot.sustainStartSec;
      standbySlot.sustainEndSec = activeSlot.sustainEndSec;

      video.muted = true;
      video.loop = true;
      video.playbackRate = this.resolveEffectivePlaybackSpeed(standbySlot);
      video.src = clipUrl(charIdSnap, clipIdSnap);
      video.load();

      const ready = await waitVideoReady(video, isStale);
      if (!ready || isStale()) {
        if (isStale() && this.activeIndex !== standbyIdx) {
          releaseVideo(standbySlot);
        }
        return;
      }
    } else {
      standbySlot.mirror = activeSlot.mirror;
      standbySlot.safeStartSec = activeSlot.safeStartSec;
      standbySlot.holdLoop = activeSlot.holdLoop;
      standbySlot.sustainStartSec = activeSlot.sustainStartSec;
      standbySlot.sustainEndSec = activeSlot.sustainEndSec;
      video.pause();
    }

    if (targetSec > 0.01) {
      try {
        video.currentTime = targetSec;
        await waitVideoSeeked(video, targetSec, isStale);
      } catch {
        // ignore seek error
      }
    }
    if (isStale()) return;
    video.pause();
    standbySlot.sustainPrepared = true;
  }

  private maybeTickSustainLoop(now: number): void {
    if (
      this.draggingMode ||
      this.dragInstantActive ||
      this.outgoingIndex !== null ||
      this.outgoingFromDrag ||
      this.activeIndex === null
    ) {
      return;
    }
    const activeIdx = this.activeIndex;
    const standbyIdx: 0 | 1 = activeIdx === 0 ? 1 : 0;
    const activeSlot = this.slots[activeIdx];
    const standbySlot = this.slots[standbyIdx];
    if (
      !activeSlot.holdLoop ||
      activeSlot.sustainStartSec === null ||
      activeSlot.sustainEndSec === null ||
      activeSlot.video.readyState < 2
    ) {
      return;
    }

    const curTime = activeSlot.video.currentTime;
    const endSec = activeSlot.sustainEndSec;

    if (
      curTime >= endSec - SUSTAIN_PREPARE_LEAD_SEC &&
      !standbySlot.sustainPrepared &&
      standbySlot.clipId !== activeSlot.clipId
    ) {
      void this.prepareSustainStandby(standbyIdx, activeSlot);
    } else if (
      curTime >= endSec - SUSTAIN_PREPARE_LEAD_SEC &&
      !standbySlot.sustainPrepared &&
      standbySlot.clipId === activeSlot.clipId &&
      standbySlot.video.paused
    ) {
      void this.prepareSustainStandby(standbyIdx, activeSlot);
    }

    if (
      curTime >= endSec - SUSTAIN_FADE_LEAD_SEC &&
      standbySlot.sustainPrepared &&
      standbySlot.clipId === activeSlot.clipId &&
      standbySlot.video.readyState >= 2
    ) {
      standbySlot.sustainPrepared = false;
      standbySlot.video.playbackRate = this.resolveEffectivePlaybackSpeed(standbySlot);
      standbySlot.video.loop = true;
      standbySlot.video.play().catch(() => {});
      this.outgoingIndex = activeIdx;
      this.activeIndex = standbyIdx;
      this.sustainRelayInProgress = true;
      this.crossfadeStartMs = now;
      this.crossfadeDurationMs = SUSTAIN_CROSSFADE_MS;
    }
  }

  private guardSafeStart(slot: VideoSlot): boolean {
    if (slot.safeStartSec <= 0) return true;
    const video = slot.video;
    if (video.readyState < 2) return false;
    // 循环片段快结束时提前回跳到 safeStartSec，严禁回绕到第 0 帧黑雾；返回 true 照常绘制当前帧避免闪白。
    if (
      video.loop &&
      Number.isFinite(video.duration) &&
      video.duration > slot.safeStartSec + 0.5 &&
      video.currentTime >= video.duration - 0.055
    ) {
      try {
        video.currentTime = slot.safeStartSec;
      } catch {
        // ignore
      }
      return true;
    }
    if (video.currentTime < slot.safeStartSec - 0.015) {
      if (!video.seeking) {
        try {
          video.currentTime = slot.safeStartSec;
        } catch {
          // ignore
        }
      }
      const now = performance.now();
      if (this.safeStartStuckSinceMs === 0) {
        this.safeStartStuckSinceMs = now;
      }
      if (now - this.safeStartStuckSinceMs > 450) {
        if (video.paused) {
          video.play().catch(() => {});
        }
        return true;
      }
      return false;
    }
    this.safeStartStuckSinceMs = 0;
    return true;
  }

  private drawSlot(
    slot: VideoSlot,
    alpha: number,
    drawX: number,
    drawY: number,
    drawW: number,
    drawH: number,
    peekAngleRad = 0,
    peekTx = 0,
    peekTy = 0,
  ): void {
    const { video, mirror } = slot;
    if (video.readyState < 2 || alpha <= 0.001) return;
    if (!this.guardSafeStart(slot)) return;

    const ctx = this.ctx;
    ctx.save();
    ctx.globalAlpha = alpha;
    if (
      Math.abs(peekAngleRad) > 0.0002 ||
      Math.abs(peekTx) > 0.05 ||
      Math.abs(peekTy) > 0.05
    ) {
      const [effPivotU, pivotV] = this.computePeekPivotUv(slot.meta, mirror);
      const pivotX = drawX + effPivotU * drawW;
      const pivotY = drawY + pivotV * drawH;
      ctx.translate(pivotX + peekTx, pivotY + peekTy);
      ctx.rotate(peekAngleRad);
      ctx.translate(-pivotX, -pivotY);
    }
    if (Math.abs(this.squashPos) > 0.0004) {
      const anchorX = slot.meta?.anchor?.[0] ?? 0.5;
      const anchorY = slot.meta?.anchor?.[1] ?? 0.89;
      const effAnchorX = mirror ? 1 - anchorX : anchorX;
      const pivotX = drawX + effAnchorX * drawW;
      const pivotY = drawY + anchorY * drawH;
      const sx = 1 + this.squashPos * 0.68;
      const sy = 1 - this.squashPos;
      ctx.translate(pivotX, pivotY);
      ctx.scale(sx, sy);
      ctx.translate(-pivotX, -pivotY);
    }
    if (mirror) {
      ctx.translate(drawX + drawW, drawY);
      ctx.scale(-1, 1);
      ctx.drawImage(video, 0, 0, drawW, drawH);
    } else {
      ctx.drawImage(video, drawX, drawY, drawW, drawH);
    }
    ctx.restore();
  }

  private drawDiamond(cx: number, cy: number, r: number): void {
    const ctx = this.ctx;
    ctx.beginPath();
    ctx.moveTo(cx, cy - r);
    ctx.quadraticCurveTo(cx + r * 0.25, cy - r * 0.25, cx + r, cy);
    ctx.quadraticCurveTo(cx + r * 0.25, cy + r * 0.25, cx, cy + r);
    ctx.quadraticCurveTo(cx - r * 0.25, cy + r * 0.25, cx - r, cy);
    ctx.quadraticCurveTo(cx - r * 0.25, cy - r * 0.25, cx, cy - r);
    ctx.closePath();
    ctx.fill();
  }

  private drawHeart(cx: number, cy: number, size: number): void {
    const ctx = this.ctx;
    const s = size;
    ctx.beginPath();
    ctx.moveTo(cx, cy + s * 0.42);
    ctx.bezierCurveTo(cx - s * 1.15, cy - s * 0.28, cx - s * 0.65, cy - s * 1.12, cx, cy - s * 0.42);
    ctx.bezierCurveTo(cx + s * 0.65, cy - s * 1.12, cx + s * 1.15, cy - s * 0.28, cx, cy + s * 0.42);
    ctx.closePath();
    ctx.fill();
  }

  private renderRipples(now: number): void {
    if (this.ripples.length === 0) return;
    const ctx = this.ctx;
    const remaining: ClickRipple[] = [];

    for (const r of this.ripples) {
      const elapsed = now - r.startMs;
      if (elapsed >= r.durationMs) continue;
      remaining.push(r);

      const p = elapsed / r.durationMs;
      const easeOut = 1 - Math.pow(1 - p, 3);
      const cx = r.x * this.dpr;
      const cy = r.y * this.dpr;
      const outerRadius = (7 + easeOut * 27) * this.dpr;
      const innerRadius = (3 + easeOut * 16) * this.dpr;
      const alpha = (1 - p) * 0.72;

      ctx.save();
      // 外层主扩散环
      ctx.beginPath();
      ctx.arc(cx, cy, outerRadius, 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(${this.accentRgb}, ${alpha.toFixed(3)})`;
      ctx.lineWidth = 1.9 * this.dpr;
      ctx.stroke();

      // 内层柔和副环
      if (p < 0.8) {
        const innerAlpha = (1 - p / 0.8) * 0.48;
        ctx.beginPath();
        ctx.arc(cx, cy, innerRadius, 0, Math.PI * 2);
        ctx.strokeStyle = `rgba(255, 255, 255, ${innerAlpha.toFixed(3)})`;
        ctx.lineWidth = 1.4 * this.dpr;
        ctx.stroke();
      }

      // 四向菱形星芒
      const starDist = (7 + easeOut * 20) * this.dpr;
      const starAlpha = (1 - p) * 0.88;
      ctx.fillStyle = `rgba(${this.accentRgb}, ${starAlpha.toFixed(3)})`;
      for (let i = 0; i < 4; i += 1) {
        const ang = Math.PI / 4 + (i * Math.PI) / 2;
        const sx = cx + Math.cos(ang) * starDist;
        const sy = cy + Math.sin(ang) * starDist - easeOut * 4 * this.dpr;
        const sr = 3.8 * (1 - p * 0.55) * this.dpr;
        this.drawDiamond(sx, sy, sr);
      }

      if (p < 0.55) {
        const dotAlpha = (1 - p / 0.55) * 0.65;
        ctx.beginPath();
        ctx.arc(cx, cy, 3.2 * this.dpr, 0, Math.PI * 2);
        ctx.fillStyle = `rgba(255, 255, 255, ${dotAlpha.toFixed(3)})`;
        ctx.fill();
      }
      ctx.restore();
    }

    this.ripples = remaining;
  }

  private renderFeedParticles(now: number): void {
    if (this.feedParticles.length === 0) return;
    const ctx = this.ctx;
    const remaining: FeedParticle[] = [];

    for (const pt of this.feedParticles) {
      const elapsed = now - pt.startMs - pt.delayMs;
      if (elapsed < 0) {
        remaining.push(pt);
        continue;
      }
      if (elapsed >= pt.durationMs) continue;
      remaining.push(pt);

      const p = elapsed / pt.durationMs;
      const easeOut = 1 - Math.pow(1 - p, 2.4);
      const sec = elapsed / 1000;
      const x = (pt.x + pt.vx * sec + Math.sin(p * Math.PI * 2) * 3.5) * this.dpr;
      const y = (pt.y + pt.vy * easeOut) * this.dpr;
      const alpha = p < 0.2 ? (p / 0.2) * 0.92 : (1 - (p - 0.2) / 0.8) * 0.92;
      const scale = p < 0.2 ? 0.65 + (p / 0.2) * 0.45 : 1.1 - p * 0.22;

      ctx.save();
      ctx.fillStyle = pt.warm
        ? `rgba(232, 118, 134, ${alpha.toFixed(3)})`
        : `rgba(${this.accentRgb}, ${alpha.toFixed(3)})`;
      if (pt.kind === "heart") {
        this.drawHeart(x, y, pt.size * scale * this.dpr);
      } else {
        this.drawDiamond(x, y, pt.size * 0.95 * scale * this.dpr);
      }
      ctx.restore();
    }

    this.feedParticles = remaining;
  }

  public setTelemetryOverlay(state: UnifiedState): void {
    const prevActivity = this.overlayActivity;
    this.overlayActivity = state.activity;
    this.overlayStatus = state.status;
    this.overlaySinceMs = state.statusSinceMs || Date.now();
    this.overlayTps =
      state.status === "streaming" && state.speed.tokensPerSec
        ? Math.max(0, state.speed.tokensPerSec)
        : 0;
    this.overlayActiveAgents = Math.max(0, state.activeAgents || 0);
    this.overlayCompletedAgents = Math.max(0, state.completedAgents || 0);
    this.overlayContextHigh = Boolean(state.alerts?.contextHigh);
    this.overlayQuotaLow = Boolean(state.alerts?.quotaLow);

    if (prevActivity !== state.activity) {
      if (state.activity === "task_completed" || state.activity === "service_connecting") {
        this.spawnBurstOverlay(state.activity === "task_completed" ? 8 : 5, "spark");
      }
    }

    if (state.status === "offline") {
      this.lastContextDeltaUsedTokens = null;
    } else if (
      state.context.deltaTokens !== null &&
      state.context.deltaTokens >= 10_000 &&
      state.context.usedTokens !== null &&
      state.context.usedTokens !== this.lastContextDeltaUsedTokens
    ) {
      this.lastContextDeltaUsedTokens = state.context.usedTokens;
      this.spawnBurstOverlay(5, "plus");
    }
  }

  private spawnBurstOverlay(count: number, kind: OverlayParticle["kind"]): void {
    const anchor = this.getBubbleAnchor();
    const baseX = anchor.x;
    const baseY = Math.max(108, anchor.topY + 12);
    const now = performance.now();
    for (let i = 0; i < count; i += 1) {
      const ang = (Math.PI * 2 * i) / count - Math.PI / 2;
      const spd = 22 + (i % 3) * 9;
      this.overlayParticles.push({
        x: baseX + Math.cos(ang) * 14,
        y: baseY + Math.sin(ang) * 10,
        vx: Math.cos(ang) * spd,
        vy: Math.sin(ang) * spd - 16,
        size: 4.5 + (i % 2) * 1.4,
        kind,
        color: kind === "plus" ? "214, 148, 52" : this.accentRgb,
        startMs: now,
        durationMs: 920,
      });
    }
  }

  private maybeSpawnAmbientOverlayParticles(now: number): void {
    if (this.draggingMode) return;
    const act = this.overlayActivity;
    const sleeping = this.isCurrentClipSleeping();
    let interval = 0;
    if (act === "code_streaming") {
      interval = Math.max(150, 560 - Math.min(380, this.overlayTps * 4.2));
    } else if (act === "code_diffing") {
      interval = 340;
    } else if (sleeping) {
      interval = 1400;
    } else if (act === "knowledge_lookup" as string || act === "research_lookup") {
      interval = 650;
    }
    if (interval <= 0 || now - this.lastOverlaySpawnMs < interval) return;
    this.lastOverlaySpawnMs = now;

    const anchor = this.getBubbleAnchor();
    const safeY = Math.max(104, anchor.topY + 18);
    const side = (Math.floor(now / 170) % 2 === 0 ? 1 : -1) * (24 + (now % 18));

    if (act === "code_streaming") {
      this.overlayParticles.push({
        x: anchor.x + side,
        y: safeY + 24,
        vx: side * 0.15,
        vy: -26 - Math.min(24, this.overlayTps * 0.25),
        size: 3.4,
        kind: "spark",
        color: this.accentRgb,
        startMs: now,
        durationMs: 780,
      });
    } else if (act === "code_diffing") {
      const isAdd = Math.floor(now / 300) % 2 === 0;
      this.overlayParticles.push({
        x: anchor.x + side,
        y: safeY + 20,
        vx: side * 0.12,
        vy: -24,
        size: 9,
        kind: isAdd ? "plus" : "minus",
        color: isAdd ? "72, 168, 120" : "214, 92, 114",
        startMs: now,
        durationMs: 880,
      });
    } else if (sleeping) {
      this.overlayParticles.push({
        x: anchor.headRightX + 6,
        y: Math.max(102, safeY - 6),
        vx: 9,
        vy: -16,
        size: 9,
        kind: "zzz",
        color: "152, 146, 166",
        startMs: now,
        durationMs: 1650,
      });
    } else if (act === "research_lookup") {
      this.overlayParticles.push({
        x: anchor.x + side * 0.9,
        y: safeY + 18,
        vx: side * 0.1,
        vy: -20,
        size: 3.2,
        kind: "spark",
        color: "218, 168, 72",
        startMs: now,
        durationMs: 820,
      });
    }
  }

  private renderTelemetryOverlay(now: number): void {
    this.maybeSpawnAmbientOverlayParticles(now);
    const ctx = this.ctx;

    if (this.overlayParticles.length > 0) {
      const keep: OverlayParticle[] = [];
      for (const pt of this.overlayParticles) {
        const elapsed = now - pt.startMs;
        if (elapsed >= pt.durationMs) continue;
        keep.push(pt);
        const p = Math.max(0, elapsed / pt.durationMs);
        const sec = elapsed / 1000;
        const alpha = p < 0.18 ? (p / 0.18) * 0.9 : (1 - (p - 0.18) / 0.82) * 0.9;
        const x = (pt.x + pt.vx * sec) * this.dpr;
        const y = (pt.y + pt.vy * sec) * this.dpr;

        ctx.save();
        if (pt.kind === "spark") {
          ctx.fillStyle = `rgba(${pt.color}, ${alpha.toFixed(3)})`;
          this.drawDiamond(x, y, pt.size * (1 - p * 0.35) * this.dpr);
        } else if (pt.kind === "plus" || pt.kind === "minus") {
          ctx.font = `700 ${Math.round(pt.size * this.dpr)}px "Segoe UI", sans-serif`;
          ctx.textAlign = "center";
          ctx.textBaseline = "middle";
          ctx.fillStyle = `rgba(${pt.color}, ${alpha.toFixed(3)})`;
          ctx.fillText(pt.kind === "plus" ? "+" : "−", x, y);
        } else {
          ctx.font = `700 ${Math.round((pt.size + p * 2.5) * this.dpr)}px "Segoe UI", sans-serif`;
          ctx.fillStyle = `rgba(${pt.color}, ${(alpha * 0.85).toFixed(3)})`;
          ctx.fillText("z", x, y);
        }
        ctx.restore();
      }
      this.overlayParticles = keep;
    }

    if (this.draggingMode) return;
    const anchor = this.getBubbleAnchor();
    const safeTopY = Math.max(106, anchor.topY + 6);
    const act = this.overlayActivity;

    // 1. 深度思考：头侧缓慢流转的 3 颗思维星点
    if (act === "deep_reasoning") {
      const cx = (anchor.headRightX + 10) * this.dpr;
      const cy = safeTopY * this.dpr;
      ctx.save();
      for (let i = 0; i < 3; i += 1) {
        const phase = now * 0.0022 + (i * Math.PI * 2) / 3;
        const ox = cx + Math.cos(phase) * 9 * this.dpr;
        const oy = cy + Math.sin(phase) * 4.5 * this.dpr;
        const r = (2.1 + 0.6 * Math.sin(phase + 1)) * this.dpr;
        ctx.beginPath();
        ctx.arc(ox, oy, r, 0, Math.PI * 2);
        ctx.fillStyle = `rgba(${this.accentRgb}, ${(0.55 + 0.3 * Math.sin(phase)).toFixed(3)})`;
        ctx.fill();
      }
      ctx.restore();
    }

    // 2. 重连中：头侧呼吸无线信号弧
    if (act === "service_reconnecting") {
      const cx = (anchor.headRightX + 10) * this.dpr;
      const cy = (safeTopY + 8) * this.dpr;
      ctx.save();
      for (let i = 0; i < 2; i += 1) {
        const wave = (Math.sin(now * 0.006 - i * 0.9) + 1) * 0.5;
        ctx.beginPath();
        ctx.arc(cx, cy, (5 + i * 4.5) * this.dpr, -Math.PI * 0.8, -Math.PI * 0.2);
        ctx.strokeStyle = `rgba(218, 152, 68, ${(0.35 + wave * 0.55).toFixed(3)})`;
        ctx.lineWidth = 1.6 * this.dpr;
        ctx.lineCap = "round";
        ctx.stroke();
      }
      ctx.restore();
    }

    // 3. 终端执行 / 长耗时构建 / 浏览器巡检：肩侧紧凑矢量秒表/扫描光环
    if (act === "terminal_running" || act === "build_long_running" || act === "browser_running") {
      const sec = Math.max(0, Math.floor((Date.now() - this.overlaySinceMs) / 1000));
      const bx = Math.min(this.cssWidth - 36, anchor.headRightX + 18) * this.dpr;
      const by = (safeTopY + 12) * this.dpr;
      const isWarn = act === "build_long_running";
      const badgeColor = isWarn ? "214, 146, 54" : act === "browser_running" ? "64, 166, 214" : this.accentRgb;

      if (act === "browser_running") {
        const ringP = (now % 1400) / 1400;
        ctx.save();
        ctx.beginPath();
        ctx.arc(bx, by, (6 + ringP * 10) * this.dpr, 0, Math.PI * 2);
        ctx.strokeStyle = `rgba(${badgeColor}, ${((1 - ringP) * 0.55).toFixed(3)})`;
        ctx.lineWidth = 1.4 * this.dpr;
        ctx.stroke();
        ctx.restore();
      }

      ctx.save();
      const label = `${sec}s`;
      const bw = 28 * this.dpr;
      const bh = 16 * this.dpr;
      const rx = bx - bw / 2;
      const ry = by - bh / 2;
      ctx.fillStyle = "rgba(32, 28, 42, 0.78)";
      ctx.strokeStyle = `rgba(${badgeColor}, 0.85)`;
      ctx.lineWidth = 1.2 * this.dpr;
      ctx.beginPath();
      ctx.roundRect(rx, ry, bw, bh, 8 * this.dpr);
      ctx.fill();
      ctx.stroke();

      ctx.font = `600 ${Math.round(9.5 * this.dpr)}px "Segoe UI", sans-serif`;
      ctx.fillStyle = "#ffffff";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(label, bx, by + 0.5 * this.dpr);
      ctx.restore();
    }

    // 4. L1/L2 醒目徽标：等待确认 / 审阅 / 鉴权告警 / 上下文 80%
    if (
      act === "approval_terminal" ||
      act === "approval_outside_dir" ||
      act === "artifact_review_ready" ||
      act === "api_auth_rate_error" ||
      act === "terminal_error" ||
      this.overlayContextHigh ||
      this.overlayQuotaLow
    ) {
      const bounce = Math.sin(now * 0.007) * 2.2;
      const ax = Math.min(this.cssWidth - 22, anchor.headRightX + 12) * this.dpr;
      const ay = (safeTopY + bounce) * this.dpr;
      const isDanger =
        act === "api_auth_rate_error" ||
        act === "terminal_error" ||
        this.overlayQuotaLow ||
        this.overlayStatus === "error";
      const fillRgb = isDanger ? "214, 78, 98" : "224, 152, 52";

      ctx.save();
      ctx.beginPath();
      ctx.arc(ax, ay, 8 * this.dpr, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(${fillRgb}, 0.92)`;
      ctx.fill();
      ctx.font = `700 ${Math.round(10 * this.dpr)}px "Segoe UI", sans-serif`;
      ctx.fillStyle = "#ffffff";
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      const sym = act === "artifact_review_ready" ? "✓" : "!";
      ctx.fillText(sym, ax, ay + 0.5 * this.dpr);
      ctx.restore();
    }

    // 5. 多 Agent 并发环绕分身（>= 2 个并行会话）
    if (this.overlayActiveAgents >= 2) {
      const runningCount = Math.min(5, this.overlayActiveAgents);
      const doneCount = Math.min(3, this.overlayCompletedAgents);
      const total = runningCount + doneCount;
      const cx = anchor.x * this.dpr;
      const cy = Math.max(104, anchor.topY) * this.dpr;
      ctx.save();
      for (let i = 0; i < total; i += 1) {
        const span = Math.PI * 0.8;
        const frac = total > 1 ? i / (total - 1) - 0.5 : 0;
        const ang = -Math.PI / 2 + frac * span + Math.sin(now * 0.0025 + i) * 0.08;
        const r = 34 * this.dpr;
        const px = cx + Math.cos(ang) * r;
        const py = Math.max(100 * this.dpr, cy + Math.sin(ang) * 14 * this.dpr);
        const isDone = i >= runningCount;
        ctx.beginPath();
        ctx.arc(px, py, 4.2 * this.dpr, 0, Math.PI * 2);
        ctx.fillStyle = isDone ? "rgba(68, 178, 122, 0.94)" : `rgba(${this.accentRgb}, 0.88)`;
        ctx.fill();
        ctx.strokeStyle = "rgba(255, 255, 255, 0.85)";
        ctx.lineWidth = 1.1 * this.dpr;
        ctx.stroke();
      }
      ctx.restore();
    }
  }

  private renderLoop = (now: number): void => {
    const dt = this.lastRenderMs > 0 ? Math.min(0.05, (now - this.lastRenderMs) / 1000) : 0.016;
    this.lastRenderMs = now;
    if (Math.abs(this.squashPos) > 0.0004 || Math.abs(this.squashVel) > 0.0004) {
      const subDt = dt * 0.5;
      for (let s = 0; s < 2; s += 1) {
        const accel = -320 * this.squashPos - 22 * this.squashVel;
        this.squashVel += accel * subDt;
        this.squashPos += this.squashVel * subDt;
      }
      if (Math.abs(this.squashPos) < 0.0004 && Math.abs(this.squashVel) < 0.0004) {
        this.squashPos = 0;
        this.squashVel = 0;
      }
    }

    this.maybeTickSustainLoop(now);
    const ctx = this.ctx;
    const cw = this.canvas.width;
    const ch = this.canvas.height;
    ctx.clearRect(0, 0, cw, ch);

    // v1.5：角色视频底对齐（drawY = ch - drawH），上方透明头顶安全带留给灵动岛
    const scale = Math.min(cw / BASE_VIDEO_WIDTH, ch / BASE_VIDEO_HEIGHT);
    const drawW = BASE_VIDEO_WIDTH * scale;
    const drawH = BASE_VIDEO_HEIGHT * scale;
    const drawX = (cw - drawW) / 2;
    const drawY = ch - drawH;

    // 贴边「暗中观察」：62% 深藏身 + 14° 歪头探望 + 悬停平滑探出 + 周期呼吸微动
    const dockTarget = !this.draggingMode && this.edgeDockSide !== null ? 1 : 0;
    const hoverTarget =
      !this.draggingMode && this.edgeDockSide !== null && this.edgeHovered ? 1 : 0;
    this.edgeDockBlend += (dockTarget - this.edgeDockBlend) * (1 - Math.exp(-dt * 10));
    if (Math.abs(dockTarget - this.edgeDockBlend) < 0.001) {
      this.edgeDockBlend = dockTarget;
      if (dockTarget === 0) {
        this.edgePeekDir = 0;
      }
    }
    this.edgeHoverBlend += (hoverTarget - this.edgeHoverBlend) * (1 - Math.exp(-dt * 11));
    if (Math.abs(hoverTarget - this.edgeHoverBlend) < 0.001) {
      this.edgeHoverBlend = hoverTarget;
    }

    let peekAngleRad = 0;
    let peekTx = 0;
    let peekTy = 0;
    if (this.edgeDockBlend > 0.001 && this.edgePeekDir !== 0) {
      const breathWave = Math.sin(now * 0.0017);
      const curiousPulse =
        Math.pow(Math.max(0, Math.sin(now * 0.00048)), 4) * (1 - this.edgeHoverBlend);
      const breathTiltRad =
        breathWave * ((1.8 * Math.PI) / 180) +
        this.edgePeekDir * curiousPulse * ((2.2 * Math.PI) / 180);
      const breathShiftX =
        Math.sin(now * 0.0017 - 0.5) * (0.0055 * drawW) +
        this.edgePeekDir * curiousPulse * (0.014 * drawW);
      const breathShiftY = Math.sin(now * 0.0034) * (0.0025 * drawH);

      const baseTilt =
        this.edgePeekDir *
        (EDGE_PEEK_BASE_TILT_RAD + this.edgeHoverBlend * EDGE_PEEK_HOVER_EXTRA_TILT_RAD);
      const hoverShiftX =
        this.edgePeekDir * (this.edgeHoverBlend * EDGE_PEEK_HOVER_SLIDE_RATIO * drawW);

      peekAngleRad = (baseTilt + breathTiltRad) * this.edgeDockBlend;
      peekTx = (hoverShiftX + breathShiftX) * this.edgeDockBlend;
      peekTy = breathShiftY * this.edgeDockBlend;
    }
    this.currentEdgePeekAngleRad = peekAngleRad;
    this.currentEdgePeekShiftX = peekTx / this.dpr;
    this.currentEdgePeekShiftY = peekTy / this.dpr;

    if (this.dragInstantActive) {
      this.drawSlot(this.dragSlot, 1, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
    } else if (this.outgoingFromDrag && this.activeIndex !== null) {
      const t = Math.min(
        1,
        Math.max(0, (now - this.crossfadeStartMs) / this.crossfadeDurationMs),
      );
      const activeSlot = this.slots[this.activeIndex];
      if (t < 1) {
        this.drawSlot(this.dragSlot, 1 - t, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
        this.drawSlot(activeSlot, t, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
      } else {
        this.outgoingFromDrag = false;
        this.rearmDragSlot();
        this.drawSlot(activeSlot, 1, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
      }
    } else if (this.outgoingIndex !== null && this.activeIndex !== null) {
      const t = Math.min(
        1,
        Math.max(0, (now - this.crossfadeStartMs) / this.crossfadeDurationMs),
      );
      const outgoingSlot = this.slots[this.outgoingIndex];
      const activeSlot = this.slots[this.activeIndex];

      if (t < 1) {
        const outAlpha = this.sustainRelayInProgress ? Math.sqrt(1 - t) : 1 - t;
        const inAlpha = this.sustainRelayInProgress ? Math.sqrt(t) : t;
        this.drawSlot(outgoingSlot, outAlpha, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
        this.drawSlot(activeSlot, inAlpha, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
      } else {
        if (this.sustainRelayInProgress) {
          outgoingSlot.video.pause();
          outgoingSlot.sustainPrepared = false;
          this.sustainRelayInProgress = false;
        } else {
          releaseVideo(outgoingSlot);
        }
        this.outgoingIndex = null;
        this.drawSlot(activeSlot, 1, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
      }
    } else if (this.activeIndex !== null) {
      this.drawSlot(this.slots[this.activeIndex], 1, drawX, drawY, drawW, drawH, peekAngleRad, peekTx, peekTy);
    }

    this.renderTelemetryOverlay(now);
    this.renderRipples(now);
    this.renderFeedParticles(now);
    this.rafId = window.requestAnimationFrame(this.renderLoop);
  };

  public destroy(): void {
    this.clearDragExitTimer();
    this.clearResumeRetryTimers();
    if (this.firstFrameWatchdogTimerId !== 0) {
      window.clearTimeout(this.firstFrameWatchdogTimerId);
      this.firstFrameWatchdogTimerId = 0;
    }
    this.clearResumeRetryTimers();
    this.safeStartStuckSinceMs = 0;
    if (this.dragPrewarmTimerId !== 0) {
      window.clearTimeout(this.dragPrewarmTimerId);
      this.dragPrewarmTimerId = 0;
    }
    window.cancelAnimationFrame(this.rafId);
    window.clearInterval(this.hintTimerId);
    window.removeEventListener("resize", this.handleWindowResize);
    this.resizeObserver.disconnect();
    this.stopVoice();
    if (this.sfxCtx) {
      void this.sfxCtx.close().catch(() => {});
      this.sfxCtx = null;
    }
    releaseVideo(this.slots[0]);
    releaseVideo(this.slots[1]);
    releaseVideo(this.dragSlot);
    this.videoSink.remove();
  }
}
