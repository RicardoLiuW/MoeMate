import type { TelemetryStatus } from "../shared/ipc";

const SVG_NS = "http://www.w3.org/2000/svg";

function svgEl<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attrs: Record<string, string>,
): SVGElementTagNameMap[K] {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) {
    el.setAttribute(k, v);
  }
  return el;
}

export interface StatusIconHandle {
  el: HTMLSpanElement;
  setStatus: (status: TelemetryStatus) => void;
  setSpeedRatio: (ratio: number) => void;
}

/**
 * 精致六态动态矢量球（18px 常规 / 22px 极简悬浮态），兼容浅瓷与暗墨双外观：
 * - offline: 静谧小月牙呼吸环
 * - idle: 主题色呼吸光晕 + 微闪菱星
 * - streaming: 三圆绕心柔转（转速随 t/s 实时平滑加快）
 * - tool_running: 琥珀虚线齿轮环旋转 + 核心脉冲
 * - waiting_approval: 琥珀三点波浪弹跳提醒
 * - error: 珊瑚红脉冲微晃感叹号
 */
export function createStatusIcon(size: "sm" | "md" = "sm"): StatusIconHandle {
  const px = size === "md" ? "22" : "18";
  const wrap = document.createElement("span");
  wrap.className = size === "md" ? "status-orb status-orb-md" : "status-orb";
  wrap.dataset.status = "offline";
  wrap.setAttribute("aria-hidden", "true");

  const svg = svgEl("svg", {
    viewBox: "0 0 16 16",
    width: px,
    height: px,
    class: "status-orb-svg",
  });

  // 1) offline 静谧小月亮
  const offlineGroup = svgEl("g", { class: "orb-layer orb-offline" });
  offlineGroup.append(
    svgEl("circle", { cx: "8", cy: "8", r: "5.5", class: "orb-offline-ring" }),
    svgEl("path", {
      d: "M9.6 4.7a3.6 3.6 0 1 0 1.7 6.3A3.1 3.1 0 0 1 9.6 4.7z",
      class: "orb-offline-moon",
    }),
  );

  // 2) idle 柔光呼吸 + 四角小星芒
  const idleGroup = svgEl("g", { class: "orb-layer orb-idle" });
  idleGroup.append(
    svgEl("circle", { cx: "8", cy: "8", r: "5.5", class: "orb-halo" }),
    svgEl("circle", { cx: "8", cy: "8", r: "3.1", class: "orb-core" }),
    svgEl("path", {
      d: "M12.5 2.4l.45 1.15 1.15.45-1.15.45-.45 1.15-.45-1.15-1.15-.45 1.15-.45z",
      class: "orb-sparkle",
    }),
  );

  // 3) streaming 三圆绕心旋转
  const streamGroup = svgEl("g", { class: "orb-layer orb-streaming" });
  streamGroup.append(
    svgEl("circle", { cx: "8", cy: "3.1", r: "2.1", class: "orb-tri orb-tri-1" }),
    svgEl("circle", { cx: "12.25", cy: "10.45", r: "1.95", class: "orb-tri orb-tri-2" }),
    svgEl("circle", { cx: "3.75", cy: "10.45", r: "1.95", class: "orb-tri orb-tri-3" }),
  );

  // 4) tool_running 旋转虚线环 + 脉动中心
  const toolGroup = svgEl("g", { class: "orb-layer orb-tool" });
  toolGroup.append(
    svgEl("circle", {
      cx: "8",
      cy: "8",
      r: "5.5",
      fill: "none",
      "stroke-width": "1.75",
      "stroke-dasharray": "9.5 5.5",
      "stroke-linecap": "round",
      class: "orb-ring",
    }),
    svgEl("circle", { cx: "8", cy: "8", r: "2.4", class: "orb-tool-dot" }),
  );

  // 5) waiting_approval 三点呼吸波浪提醒
  const waitGroup = svgEl("g", { class: "orb-layer orb-wait" });
  waitGroup.append(
    svgEl("circle", { cx: "3.5", cy: "8", r: "1.65", class: "orb-dot-b1" }),
    svgEl("circle", { cx: "8", cy: "8", r: "1.65", class: "orb-dot-b2" }),
    svgEl("circle", { cx: "12.5", cy: "8", r: "1.65", class: "orb-dot-b3" }),
  );

  // 6) error 轻晃感叹号
  const errGroup = svgEl("g", { class: "orb-layer orb-error" });
  errGroup.append(
    svgEl("rect", {
      x: "7.0",
      y: "2.4",
      width: "2.0",
      height: "6.7",
      rx: "1.0",
      class: "orb-bang",
    }),
    svgEl("circle", { cx: "8", cy: "12.35", r: "1.2", class: "orb-bang" }),
  );

  svg.append(offlineGroup, idleGroup, streamGroup, toolGroup, waitGroup, errGroup);
  wrap.appendChild(svg);

  return {
    el: wrap,
    setStatus(status: TelemetryStatus) {
      if (wrap.dataset.status !== status) {
        wrap.dataset.status = status;
      }
    },
    setSpeedRatio(ratio: number) {
      // 吐字越快，三圆绕心与微波形节奏越流畅（1.85s → 0.70s）
      const clamped = Math.max(0, Math.min(1, Number.isFinite(ratio) ? ratio : 0));
      const sec = 1.85 - clamped * 1.15;
      wrap.style.setProperty("--orbit-dur", `${sec.toFixed(2)}s`);
    },
  };
}
