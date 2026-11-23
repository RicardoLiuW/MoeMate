import type { BubbleEvent } from "../shared/ipc";
import type { BubbleAnchor } from "../stage/player";
import type { EdgeDockState } from "./telemetryIsland";

export class BubbleOverlay {
  private readonly el: HTMLDivElement;
  private readonly nameEl: HTMLSpanElement;
  private readonly waveEl: HTMLSpanElement;
  private readonly textEl: HTMLSpanElement;
  private readonly getAnchor: () => BubbleAnchor;
  private readonly isTopIslandVisible: () => boolean;
  private edgeDock: EdgeDockState | null = null;
  private hideTimer: number | null = null;
  private visible = false;

  constructor(
    container: HTMLElement,
    getAnchor: () => BubbleAnchor,
    isTopIslandVisible: () => boolean = () => false,
  ) {
    this.getAnchor = getAnchor;
    this.isTopIslandVisible = isTopIslandVisible;

    this.el = document.createElement("div");
    this.el.className = "pet-bubble";
    this.el.setAttribute("role", "status");
    this.el.setAttribute("aria-live", "polite");
    this.el.dataset.visible = "false";
    this.el.dataset.align = "top";
    this.el.dataset.speaking = "false";

    const head = document.createElement("div");
    head.className = "pet-bubble-head";

    const badge = document.createElement("span");
    badge.className = "pet-bubble-badge";
    const dot = document.createElement("span");
    dot.className = "pet-bubble-dot";
    this.nameEl = document.createElement("span");
    this.nameEl.textContent = "MoeMate";
    badge.append(dot, this.nameEl);

    this.waveEl = document.createElement("span");
    this.waveEl.className = "pet-bubble-wave";
    this.waveEl.hidden = true;
    this.waveEl.innerHTML = "<i></i><i></i><i></i>";
    head.append(badge, this.waveEl);

    this.textEl = document.createElement("span");
    this.textEl.className = "pet-bubble-text";

    const tail = document.createElement("span");
    tail.className = "pet-bubble-tail";
    tail.setAttribute("aria-hidden", "true");

    this.el.append(head, this.textEl, tail);
    container.appendChild(this.el);

    window.addEventListener("resize", this.reposition);
  }

  public setCharacter(name: string): void {
    const clean = name.trim();
    if (clean) {
      this.nameEl.textContent = clean;
    }
  }

  public setSpeaking(speaking: boolean): void {
    this.el.dataset.speaking = speaking ? "true" : "false";
    this.waveEl.hidden = !speaking;
  }

  public show(event: BubbleEvent): void {
    const content = event.text?.trim();
    if (!content) return;

    this.textEl.textContent = content;
    this.visible = true;
    this.el.dataset.visible = "true";
    this.reposition();

    if (this.hideTimer !== null) {
      window.clearTimeout(this.hideTimer);
    }
    const ttl = Math.max(1400, Math.min(event.ttlMs || 3800, 12000));
    this.hideTimer = window.setTimeout(() => {
      this.hide();
    }, ttl);
  }

  public hide(): void {
    this.visible = false;
    this.el.dataset.visible = "false";
    if (this.hideTimer !== null) {
      window.clearTimeout(this.hideTimer);
      this.hideTimer = null;
    }
  }

  public setEdgeDock(dock: EdgeDockState): void {
    this.edgeDock = dock;
    if (this.visible) {
      this.reposition();
    }
  }

  public reposition = (): void => {
    if (!this.visible) return;
    const anchor = this.getAnchor();
    const padX = 12;
    const visLeft = this.edgeDock?.side ? this.edgeDock.visibleLeftPx : 0;
    const visRight = this.edgeDock?.side ? this.edgeDock.visibleRightPx : anchor.stageWidth;

    if (this.edgeDock?.side === "right") {
      // 角色贴右屏边探头：气泡停靠在角色左内侧并向内展开
      this.el.dataset.align = "side-left";
      const availWidth = Math.max(108, visRight - visLeft - padX * 2);
      const maxW = Math.min(200, availWidth);
      this.el.style.maxWidth = `${Math.round(maxW)}px`;

      const width = this.el.offsetWidth || Math.min(156, maxW);
      const height = this.el.offsetHeight || 36;
      const charLeftBound = Math.min(anchor.x - 40, visRight - 52);
      const desiredLeft = charLeftBound - width - 8;
      const left = Math.max(
        visLeft + padX,
        Math.min(desiredLeft, visRight - width - padX),
      );
      const topMin = this.isTopIslandVisible()
        ? Math.max(anchor.videoTopY + 8, 94)
        : Math.max(14, anchor.topY);
      const desiredTop = Math.max(topMin, anchor.topY + 12);
      const clampedTop = Math.max(
        topMin,
        Math.min(desiredTop, anchor.stageHeight - height - 20),
      );

      this.el.style.left = `${Math.round(left)}px`;
      this.el.style.top = `${Math.round(clampedTop)}px`;
      return;
    }

    if (this.edgeDock?.side === "left" || this.isTopIslandVisible()) {
      // 头顶有灵动岛或贴左屏边时：气泡智能停靠在角色右耳/肩侧透明空翼，既不撞头顶岛也不挡脸
      this.el.dataset.align = "side";
      const rightStart =
        this.edgeDock?.side === "left"
          ? Math.max(visLeft + 52, anchor.headRightX + 8)
          : Math.max(anchor.x + 56, anchor.headRightX + 8);
      const availRight = visRight - rightStart - padX;
      const maxW = Math.max(
        108,
        Math.min(206, availRight > 116 ? availRight : visRight - visLeft - padX * 2),
      );
      this.el.style.maxWidth = `${Math.round(maxW)}px`;

      const width = this.el.offsetWidth || Math.min(168, maxW);
      const height = this.el.offsetHeight || 36;
      const left = Math.max(
        visLeft + padX,
        Math.min(rightStart, visRight - width - padX),
      );
      const topMin = this.isTopIslandVisible()
        ? Math.max(anchor.videoTopY + 8, 94)
        : Math.max(14, anchor.topY);
      const desiredTop = Math.max(topMin, anchor.topY + 12);
      const clampedTop = Math.max(
        topMin,
        Math.min(desiredTop, anchor.stageHeight - height - 20),
      );

      this.el.style.left = `${Math.round(left)}px`;
      this.el.style.top = `${Math.round(clampedTop)}px`;
      return;
    }

    // 头顶无灵动岛时：经典头顶居中云朵气泡
    this.el.dataset.align = "top";
    const padTop = 14;
    const maxW = Math.max(120, Math.min(246, visRight - visLeft - padX * 2));
    this.el.style.maxWidth = `${Math.round(maxW)}px`;

    const width = this.el.offsetWidth || Math.min(196, maxW);
    const height = this.el.offsetHeight || 38;

    const halfW = width / 2;
    const clampedX = Math.max(
      visLeft + padX + halfW,
      Math.min(anchor.x, visRight - padX - halfW),
    );
    const desiredTop = anchor.topY - height - 10;
    const clampedTop = Math.max(
      padTop,
      Math.min(desiredTop, anchor.stageHeight - height - 28),
    );

    this.el.style.left = `${Math.round(clampedX)}px`;
    this.el.style.top = `${Math.round(clampedTop)}px`;
  };

  public destroy(): void {
    this.hide();
    window.removeEventListener("resize", this.reposition);
    this.el.remove();
  }
}
