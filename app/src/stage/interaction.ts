import { petInteract, type PetState } from "../shared/ipc";
import type { StagePlayer } from "./player";

/** 3px 极速起拖阈值：兼顾点按防误触与零迟滞跟手起拖 */
const DRAG_THRESHOLD_PX = 3;

export interface InteractionHandlers {
  player: StagePlayer;
  hasOpenOverlay: () => boolean;
  onDismissOverlays: () => void;
  onContextMenu: (clientX: number, clientY: number) => void;
  onStateSync?: (state: PetState) => void;
}

export class StageInteraction {
  private readonly canvas: HTMLCanvasElement;
  private readonly handlers: InteractionHandlers;

  private pointerId: number | null = null;
  private startX = 0;
  private startY = 0;
  private dragging = false;
  private rightButtonDown = false;

  constructor(canvas: HTMLCanvasElement, handlers: InteractionHandlers) {
    this.canvas = canvas;
    this.handlers = handlers;

    this.canvas.addEventListener("pointerdown", this.onPointerDown);
    this.canvas.addEventListener("pointerrawupdate", this.onPointerRawUpdate);
    this.canvas.addEventListener("pointermove", this.onPointerMove);
    this.canvas.addEventListener("pointerup", this.onPointerUp);
    this.canvas.addEventListener("pointercancel", this.onPointerCancel);
    this.canvas.addEventListener("lostpointercapture", this.onLostPointerCapture);
    this.canvas.addEventListener("contextmenu", this.onContextMenu);
    window.addEventListener("pointerup", this.onGlobalPointerUp, false);
    window.addEventListener("blur", this.onWindowBlur);
  }

  private cancelPendingPointer(): void {
    if (this.pointerId === null) return;
    const pid = this.pointerId;
    this.pointerId = null;
    try {
      if (this.canvas.hasPointerCapture(pid)) {
        this.canvas.releasePointerCapture(pid);
      }
    } catch {
      // ignore
    }
  }

  private tryBeginDrag(e: PointerEvent): void {
    if (this.pointerId !== e.pointerId || this.dragging) return;

    const dx = e.clientX - this.startX;
    const dy = e.clientY - this.startY;
    if (Math.hypot(dx, dy) < DRAG_THRESHOLD_PX) return;

    // 越过 3px 阈值：第一优先发往 Rust 抢先起拖，再同步切出前端预热悬空槽。
    this.dragging = true;
    void petInteract("dragStart", this.startX, this.startY).catch(() => {
      // non-Tauri preview fallback
    });
    this.canvas.classList.add("is-dragging");
    this.handlers.player.enterDragImmediately();
  }

  private onPointerRawUpdate = (ev: Event): void => {
    const e = ev as PointerEvent;
    if (typeof e.clientX !== "number") return;
    if ((e.buttons & 1) === 0 || (e.buttons & 2) !== 0 || this.rightButtonDown) {
      return;
    }
    this.tryBeginDrag(e);
  };

  private onPointerDown = (e: PointerEvent): void => {
    if (e.button !== 0) {
      if (e.button === 2) {
        this.rightButtonDown = true;
      }
      this.cancelPendingPointer();
      if (this.dragging) {
        void this.finishDrag(true);
      }
      return;
    }

    if ((e.buttons & 1) === 0 || (e.buttons & 2) !== 0 || this.rightButtonDown) {
      return;
    }

    if (this.handlers.hasOpenOverlay()) {
      this.handlers.onDismissOverlays();
      return;
    }

    if (this.dragging) {
      void this.finishDrag();
    }

    this.pointerId = e.pointerId;
    this.startX = e.clientX;
    this.startY = e.clientY;
    try {
      this.canvas.setPointerCapture(e.pointerId);
    } catch {
      // ignore if capture fails
    }
  };

  private onPointerMove = (e: PointerEvent): void => {
    if ((e.buttons & 1) === 0 || (e.buttons & 2) !== 0 || this.rightButtonDown) {
      this.cancelPendingPointer();
      if (this.dragging) {
        void this.finishDrag(true);
      }
      return;
    }

    this.tryBeginDrag(e);
  };

  private onPointerUp = (e: PointerEvent): void => {
    if (e.button === 2) {
      this.rightButtonDown = false;
      return;
    }
    if (this.pointerId !== e.pointerId) return;

    const wasDragging = this.dragging;
    this.cancelPendingPointer();

    if (wasDragging) {
      void this.finishDrag();
      return;
    }

    if (e.button !== 0 || this.rightButtonDown) {
      return;
    }

    this.handlers.player.triggerClickFeedback(e.clientX, e.clientY);
    void petInteract("click", Math.round(e.clientX), Math.round(e.clientY))
      .then((nextState) => {
        this.handlers.onStateSync?.(nextState);
      })
      .catch(() => {
        // non-Tauri preview fallback
      });
  };

  private onGlobalPointerUp = (e: PointerEvent): void => {
    if (e.button === 2) {
      this.rightButtonDown = false;
    }
    if (e.button === 0) {
      this.cancelPendingPointer();
      if (this.dragging) {
        void this.finishDrag();
      }
      return;
    }
    if (this.dragging && (e.buttons & 1) === 0) {
      this.cancelPendingPointer();
      void this.finishDrag();
    }
  };

  private onPointerCancel = (e: PointerEvent): void => {
    if (this.pointerId === e.pointerId) {
      this.cancelPendingPointer();
    }
    if (this.dragging) {
      void this.finishDrag(true);
    }
  };

  private onLostPointerCapture = (e: PointerEvent): void => {
    if (!this.dragging && this.pointerId === e.pointerId) {
      this.pointerId = null;
    }
  };

  private onWindowBlur = (): void => {
    this.rightButtonDown = false;
    this.cancelPendingPointer();
    if (this.dragging) {
      void this.finishDrag();
    }
  };

  private onContextMenu = (e: MouseEvent): void => {
    e.preventDefault();
    this.rightButtonDown = false;
    this.cancelPendingPointer();
    if (this.dragging) {
      void this.finishDrag(true);
    }
    this.handlers.onContextMenu(e.clientX, e.clientY);
  };

  private async finishDrag(immediateCancel = false): Promise<void> {
    if (!this.dragging) return;
    this.dragging = false;
    this.canvas.classList.remove("is-dragging");
    if (immediateCancel) {
      this.handlers.player.cancelDragImmediately();
    } else {
      this.handlers.player.setDraggingMode(false);
    }

    const posX = Math.round(window.screenX);
    const posY = Math.round(window.screenY);

    try {
      const state = await petInteract("dragEnd", posX, posY);
      this.handlers.onStateSync?.(state);
    } catch {
      // ignore in browser preview
    }
  }

  public destroy(): void {
    this.canvas.removeEventListener("pointerdown", this.onPointerDown);
    this.canvas.removeEventListener("pointerrawupdate", this.onPointerRawUpdate);
    this.canvas.removeEventListener("pointermove", this.onPointerMove);
    this.canvas.removeEventListener("pointerup", this.onPointerUp);
    this.canvas.removeEventListener("pointercancel", this.onPointerCancel);
    this.canvas.removeEventListener("lostpointercapture", this.onLostPointerCapture);
    this.canvas.removeEventListener("contextmenu", this.onContextMenu);
    window.removeEventListener("pointerup", this.onGlobalPointerUp, false);
    window.removeEventListener("blur", this.onWindowBlur);
  }
}
