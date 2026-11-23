import type { TelemetryStatus } from "../shared/ipc";
import { createIcon, type IconName } from "./icons";
import type { IslandMode, PanelTabId } from "./telemetryIsland";
import type { UiAppearance } from "./themes";

export type ScalePreset = 0.35 | 0.5 | 0.75;

export interface ContextMenuState {
  scale: number;
  pinned: boolean;
  muted: boolean;
  edgeDocking: boolean;
  autoStart: boolean;
  islandVisible: boolean;
  islandMode: IslandMode;
  charName: string;
  appearance: UiAppearance;
  telemetryStatus: TelemetryStatus;
  aiConfigured?: boolean;
  ttsEnabled?: boolean;
}

export interface ContextMenuCallbacks {
  onFeed: () => void;
  onOpenTab: (tab: PanelTabId) => void;
  onOpenChatSettings?: () => void;
  onOpenResources?: () => void;
  onSetScale: (scale: ScalePreset) => void;
  onTogglePinned: () => void;
  onToggleMuted: () => void;
  onToggleEdgeDocking: () => void;
  onToggleAutoStart: () => void;
  onSetIslandVisibility: (visible: boolean) => void;
  onSetIslandMode: (mode: IslandMode) => void;
  onSetAppearance: (appearance: UiAppearance) => void;
  onOpenChange: (open: boolean) => void;
}

const SCALE_PRESETS: ReadonlyArray<{ value: ScalePreset; label: string }> = [
  { value: 0.35, label: "35%" },
  { value: 0.5, label: "50%" },
  { value: 0.75, label: "75%" },
];

type IslandStateChoice = "full" | "minimal" | "hidden";

/**
 * Deskpet v3.0 紧凑零溢出马卡龙右键菜单：
 * - 顶栏：角色色点 + 角色名 + 35%/50%/75% 缩放分段条；
 * - 4 列单行动作瓷砖（投喂 / 闲聊 / 伙伴 / 相册）；
 * - 双并排高亮入口（实时监控 / 桌宠设置）；
 * - 2×2 紧凑开关胶囊格（置顶 / 静音 / 贴边 / 自启）；
 * - 底部双行分段条（外观 / 灵动岛）。
 */
export class ContextMenu {
  private readonly el: HTMLDivElement;
  private readonly headerLabel: HTMLSpanElement;
  private readonly liveDotEl: HTMLSpanElement;
  private readonly aiBadgeEl: HTMLSpanElement;
  private readonly scaleButtons = new Map<ScalePreset, HTMLButtonElement>();

  private readonly pinPill: HTMLButtonElement;
  private readonly mutePill: HTMLButtonElement;
  private readonly muteIconWrap: HTMLSpanElement;
  private readonly edgeDockPill: HTMLButtonElement;
  private readonly autoStartPill: HTMLButtonElement;
  private readonly appearanceBtns = new Map<UiAppearance, HTMLButtonElement>();
  private readonly islandBtns = new Map<IslandStateChoice, HTMLButtonElement>();

  private readonly callbacks: ContextMenuCallbacks;
  private open = false;

  constructor(container: HTMLElement, callbacks: ContextMenuCallbacks) {
    this.callbacks = callbacks;

    this.el = document.createElement("div");
    this.el.className = "porcelain-card context-menu";
    this.el.dataset.open = "false";
    this.el.setAttribute("role", "menu");
    this.el.addEventListener("pointerdown", (e) => e.stopPropagation());
    this.el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
    });

    // 1. 顶栏
    const header = document.createElement("div");
    header.className = "menu-header";

    const charIdent = document.createElement("span");
    charIdent.className = "menu-header-ident";
    const charDot = document.createElement("span");
    charDot.className = "menu-char-dot";
    this.headerLabel = document.createElement("span");
    this.headerLabel.className = "menu-header-name";
    this.headerLabel.textContent = "MoeMate";
    charIdent.append(charDot, this.headerLabel);

    const scaleGroup = document.createElement("div");
    scaleGroup.className = "menu-seg-group";
    for (const preset of SCALE_PRESETS) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "menu-seg-btn";
      btn.textContent = preset.label;
      btn.addEventListener("click", () => {
        this.callbacks.onSetScale(preset.value);
        this.close();
      });
      scaleGroup.appendChild(btn);
      this.scaleButtons.set(preset.value, btn);
    }
    header.append(charIdent, scaleGroup);
    this.el.appendChild(header);

    // 2. 4 列单行动作瓷砖
    const quickGrid = document.createElement("div");
    quickGrid.className = "menu-quick-grid";
    quickGrid.append(
      this.makeQuickTile("cake", "投喂", () => {
        this.close();
        this.callbacks.onFeed();
      }),
      this.makeQuickTile("message", "闲聊", () => {
        this.close();
        this.callbacks.onOpenTab("chat");
      }),
      this.makeQuickTile("users", "伙伴", () => {
        this.close();
        this.callbacks.onOpenTab("pet");
      }),
      this.makeQuickTile("film", "相册", () => {
        this.close();
        this.callbacks.onOpenTab("album");
      }),
    );
    this.el.appendChild(quickGrid);

    // 3. 双并排高亮入口
    const featuredRow = document.createElement("div");
    featuredRow.className = "menu-featured-row";

    const statusBtn = document.createElement("button");
    statusBtn.type = "button";
    statusBtn.className = "menu-featured-btn";
    statusBtn.setAttribute("role", "menuitem");
    const statusLeft = document.createElement("span");
    statusLeft.className = "menu-featured-left";
    this.liveDotEl = document.createElement("span");
    this.liveDotEl.className = "menu-live-dot";
    this.liveDotEl.dataset.status = "offline";
    const statusText = document.createElement("span");
    statusText.textContent = "实时监控";
    statusLeft.append(this.liveDotEl, statusText);
    statusBtn.append(statusLeft, createIcon("chevronRight", 11));
    statusBtn.addEventListener("click", () => {
      this.close();
      this.callbacks.onOpenTab("status");
    });

    const aiBtn = document.createElement("button");
    aiBtn.type = "button";
    aiBtn.className = "menu-featured-btn";
    aiBtn.setAttribute("role", "menuitem");
    const aiLeft = document.createElement("span");
    aiLeft.className = "menu-featured-left";
    aiLeft.appendChild(createIcon("sliders", 11));
    const aiText = document.createElement("span");
    aiText.textContent = "设置";
    aiLeft.appendChild(aiText);
    this.aiBadgeEl = document.createElement("span");
    this.aiBadgeEl.className = "menu-ai-badge";
    this.aiBadgeEl.textContent = "模板";
    aiBtn.append(aiLeft, this.aiBadgeEl);
    aiBtn.addEventListener("click", () => {
      this.close();
      if (this.callbacks.onOpenChatSettings) {
        this.callbacks.onOpenChatSettings();
      } else {
        this.callbacks.onOpenTab("chat");
      }
    });

    featuredRow.append(statusBtn, aiBtn);
    this.el.appendChild(featuredRow);

    const downloadBtn = document.createElement("button");
    downloadBtn.type = "button";
    downloadBtn.className = "menu-download-btn";
    downloadBtn.setAttribute("role", "menuitem");
    const dlLeft = document.createElement("span");
    dlLeft.className = "menu-featured-left";
    dlLeft.appendChild(createIcon("download", 11));
    const dlText = document.createElement("span");
    dlText.textContent = "下载角色动画与语音包";
    dlLeft.appendChild(dlText);
    downloadBtn.append(dlLeft, createIcon("chevronRight", 11));
    downloadBtn.addEventListener("click", () => {
      this.close();
      this.callbacks.onOpenResources?.();
    });
    this.el.appendChild(downloadBtn);

    const div = document.createElement("div");
    div.className = "menu-divider";
    this.el.appendChild(div);

    // 4. 2×2 紧凑开关胶囊格
    const toggleGrid = document.createElement("div");
    toggleGrid.className = "menu-toggle-grid";

    const pinRow = this.makeTogglePill("pin", "置顶", () => this.callbacks.onTogglePinned());
    this.pinPill = pinRow.btn;

    const muteRow = this.makeTogglePill("volumeOff", "静音", () => this.callbacks.onToggleMuted());
    this.mutePill = muteRow.btn;
    this.muteIconWrap = muteRow.iconWrap;

    const dockRow = this.makeTogglePill("dock", "贴边", () => this.callbacks.onToggleEdgeDocking());
    this.edgeDockPill = dockRow.btn;

    const autoRow = this.makeTogglePill("zap", "自启", () => this.callbacks.onToggleAutoStart());
    this.autoStartPill = autoRow.btn;

    toggleGrid.append(this.pinPill, this.mutePill, this.edgeDockPill, this.autoStartPill);
    this.el.appendChild(toggleGrid);

    // 5. 底部双行分段条
    this.el.appendChild(
      this.makeSegmentRow(
        "外观",
        [
          { key: "light" as UiAppearance, label: "奶油" },
          { key: "dark" as UiAppearance, label: "星糖" },
        ],
        this.appearanceBtns,
        (key) => this.callbacks.onSetAppearance(key),
      ),
    );
    this.el.appendChild(
      this.makeSegmentRow(
        "灵动岛",
        [
          { key: "full" as IslandStateChoice, label: "完全" },
          { key: "minimal" as IslandStateChoice, label: "极简" },
          { key: "hidden" as IslandStateChoice, label: "隐藏" },
        ],
        this.islandBtns,
        (key) => {
          if (key === "hidden") {
            this.callbacks.onSetIslandVisibility(false);
          } else {
            this.callbacks.onSetIslandVisibility(true);
            this.callbacks.onSetIslandMode(key);
          }
        },
      ),
    );

    container.appendChild(this.el);
  }

  private makeQuickTile(
    iconName: IconName,
    label: string,
    onClick: () => void,
  ): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "menu-tile";
    btn.setAttribute("role", "menuitem");

    const iconWrap = document.createElement("span");
    iconWrap.className = "menu-tile-icon";
    iconWrap.appendChild(createIcon(iconName, 13));

    const textEl = document.createElement("span");
    textEl.className = "menu-tile-label";
    textEl.textContent = label;

    btn.append(iconWrap, textEl);
    btn.addEventListener("click", onClick);
    return btn;
  }

  private makeTogglePill(
    iconName: IconName,
    label: string,
    onClick: () => void,
  ): { btn: HTMLButtonElement; iconWrap: HTMLSpanElement } {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "menu-toggle-pill";
    btn.setAttribute("role", "menuitemcheckbox");
    btn.dataset.on = "false";

    const left = document.createElement("span");
    left.className = "menu-toggle-left";
    const iconWrap = document.createElement("span");
    iconWrap.style.display = "inline-flex";
    iconWrap.appendChild(createIcon(iconName, 11));
    const textEl = document.createElement("span");
    textEl.textContent = label;
    left.append(iconWrap, textEl);

    const dot = document.createElement("span");
    dot.className = "menu-toggle-dot";

    btn.append(left, dot);
    btn.addEventListener("click", onClick);
    return { btn, iconWrap };
  }

  private makeSegmentRow<T extends string>(
    label: string,
    options: ReadonlyArray<{ key: T; label: string }>,
    map: Map<T, HTMLButtonElement>,
    onSelect: (key: T) => void,
  ): HTMLDivElement {
    const row = document.createElement("div");
    row.className = "menu-seg-row";

    const lbl = document.createElement("span");
    lbl.className = "menu-seg-label";
    lbl.textContent = label;

    const group = document.createElement("div");
    group.className = "menu-seg-group";
    for (const opt of options) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "menu-seg-btn";
      btn.textContent = opt.label;
      btn.addEventListener("click", () => onSelect(opt.key));
      group.appendChild(btn);
      map.set(opt.key, btn);
    }

    row.append(lbl, group);
    return row;
  }

  public syncState(state: ContextMenuState): void {
    this.headerLabel.textContent = state.charName || "MoeMate";
    this.liveDotEl.dataset.status = state.telemetryStatus;
    this.aiBadgeEl.textContent = state.aiConfigured
      ? state.ttsEnabled !== false
        ? "AI+语音"
        : "AI就绪"
      : state.ttsEnabled !== false
        ? "语音开"
        : "模板";

    this.pinPill.dataset.on = state.pinned ? "true" : "false";
    this.mutePill.dataset.on = state.muted ? "true" : "false";
    this.edgeDockPill.dataset.on = state.edgeDocking ? "true" : "false";
    this.autoStartPill.dataset.on = state.autoStart ? "true" : "false";
    this.muteIconWrap.replaceChildren(
      createIcon(state.muted ? "volumeOff" : "volumeOn", 11),
    );

    for (const [preset, btn] of this.scaleButtons.entries()) {
      const active = Math.abs(state.scale - preset) < 0.03;
      btn.dataset.active = active ? "true" : "false";
    }

    for (const [key, btn] of this.appearanceBtns.entries()) {
      btn.dataset.active = state.appearance === key ? "true" : "false";
    }

    const islandChoice: IslandStateChoice = !state.islandVisible
      ? "hidden"
      : state.islandMode;
    for (const [key, btn] of this.islandBtns.entries()) {
      btn.dataset.active = islandChoice === key ? "true" : "false";
    }
  }

  public isOpen(): boolean {
    return this.open;
  }

  public openAt(
    clientX: number,
    clientY: number,
    state: ContextMenuState,
    visibleBounds?: { left: number; right: number },
  ): void {
    this.syncState(state);
    this.el.dataset.open = "true";

    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const pad = 8;
    const visLeft = visibleBounds?.left ?? 0;
    const visRight = visibleBounds?.right ?? vw;
    const availW = Math.max(180, visRight - visLeft - pad * 2);
    const menuW = Math.min(this.el.offsetWidth || 230, availW);
    const menuH = Math.min(this.el.offsetHeight || 228, vh - pad * 2);

    const minX = visLeft + pad;
    const maxX = Math.max(minX, visRight - menuW - pad);
    const left = Math.max(minX, Math.min(clientX, maxX));
    const top = Math.max(pad, Math.min(clientY, vh - menuH - pad));
    this.el.style.left = `${Math.round(left)}px`;
    this.el.style.top = `${Math.round(top)}px`;
    this.el.scrollTop = 0;

    this.open = true;
    this.callbacks.onOpenChange(true);
  }

  public close(): void {
    if (!this.open) return;
    this.open = false;
    this.el.dataset.open = "false";
    this.callbacks.onOpenChange(false);
  }

  public destroy(): void {
    this.close();
    this.el.remove();
  }
}
