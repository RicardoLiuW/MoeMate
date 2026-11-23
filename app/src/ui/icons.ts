const SVG_NS = "http://www.w3.org/2000/svg";

export type IconName =
  | "cake"
  | "message"
  | "users"
  | "film"
  | "activity"
  | "zap"
  | "pin"
  | "volumeOn"
  | "volumeOff"
  | "sun"
  | "moon"
  | "minimize"
  | "maximize"
  | "refresh"
  | "copy"
  | "check"
  | "close"
  | "search"
  | "send"
  | "music"
  | "chevronRight"
  | "alert"
  | "sliders"
  | "compass"
  | "heart"
  | "sparkles"
  | "dock"
  | "key"
  | "timer"
  | "shuffle"
  | "terminal"
  | "play"
  | "pause"
  | "rotateCcw"
  | "download"
  | "trash";

const ICON_PATHS: Record<IconName, string[]> = {
  cake: [
    "M3.5 11.2c0-1 .8-1.8 1.8-1.8h9.4c1 0 1.8.8 1.8 1.8v4.3c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5v-4.3z",
    "M3.7 12.6c1.1.9 2.2.9 3.2 0s2.1-.9 3.1 0 2.1.9 3.1 0 2.1-.9 3.2 0",
    "M10 9.4V6.6",
    "M10 4.8c.7-.9 1.3-1.4 0-2.3-1.3.9-.7 1.4 0 2.3z",
  ],
  message: [
    "M4 9.8c0-3.1 2.7-5.5 6-5.5s6 2.4 6 5.5-2.7 5.5-6 5.5c-.9 0-1.8-.2-2.6-.5L4.3 16l.8-2.6c-.7-1-1.1-2.2-1.1-3.6z",
  ],
  users: [
    "M7.6 9.3a2.6 2.6 0 1 0 0-5.2 2.6 2.6 0 0 0 0 5.2z",
    "M3.2 15.8c.4-2.1 2.2-3.4 4.4-3.4s4 1.3 4.4 3.4",
    "M13.6 5.2a2.1 2.1 0 0 1 0 4.1",
    "M14.4 12.5c1.4.3 2.5 1.4 2.8 3.1",
  ],
  film: [
    "M3.5 4.8h13a1.2 1.2 0 0 1 1.2 1.2v8a1.2 1.2 0 0 1-1.2 1.2h-13a1.2 1.2 0 0 1-1.2-1.2V6a1.2 1.2 0 0 1 1.2-1.2z",
    "M6.2 4.8v10.4",
    "M13.8 4.8v10.4",
    "M2.6 8.2h3.4",
    "M2.6 11.8h3.4",
    "M14 8.2h3.4",
    "M14 11.8h3.4",
  ],
  activity: ["M2.6 10.4h3l2-5.4 3.6 9.6 2.3-5.2h3.9"],
  zap: ["M11.2 2.8L4.8 11h4.8l-.8 6.2 6.4-8.2h-4.8l.8-6.2z"],
  pin: [
    "M7.2 4.2h5.6l-1 4.2 2.4 2.3v1.2H5.8v-1.2l2.4-2.3-1-4.2z",
    "M10 11.9v5.2",
  ],
  volumeOn: [
    "M3.6 8.2h2.7l3.6-3.1v9.8l-3.6-3.1H3.6V8.2z",
    "M13 7.3a4.2 4.2 0 0 1 0 5.4",
    "M15.3 5.2a6.9 6.9 0 0 1 0 9.6",
  ],
  volumeOff: [
    "M3.6 8.2h2.7l3.6-3.1v9.8l-3.6-3.1H3.6V8.2z",
    "M13 7.8l4 4.4",
    "M17 7.8l-4 4.4",
  ],
  sun: [
    "M10 13.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4z",
    "M10 2.6v1.6",
    "M10 15.8v1.6",
    "M4.8 4.8l1.1 1.1",
    "M14.1 14.1l1.1 1.1",
    "M2.6 10h1.6",
    "M15.8 10h1.6",
    "M4.8 15.2l1.1-1.1",
    "M14.1 5.9l1.1-1.1",
  ],
  moon: ["M14.8 11.8a5.4 5.4 0 0 1-6.6-6.6 5.6 5.6 0 1 0 6.6 6.6z"],
  minimize: ["M4.5 12.6h11", "M7.5 6.8l2.5 2.6 2.5-2.6"],
  maximize: ["M4.5 7.4h11", "M7.5 13.2L10 10.6l2.5 2.6"],
  refresh: [
    "M15.8 10a5.8 5.8 0 1 1-1.6-4",
    "M15.8 4.1v3h-3",
  ],
  copy: [
    "M7.6 7.4h7.2c.7 0 1.2.5 1.2 1.2v7.2c0 .7-.5 1.2-1.2 1.2H7.6c-.7 0-1.2-.5-1.2-1.2V8.6c0-.7.5-1.2 1.2-1.2z",
    "M12.6 7.4V4.8c0-.7-.5-1.2-1.2-1.2H4.8c-.7 0-1.2.5-1.2 1.2v6.6c0 .7.5 1.2 1.2 1.2h1.6",
  ],
  check: ["M4.6 10.4l3.5 3.6 7.3-7.6"],
  close: ["M5.4 5.4l9.2 9.2", "M14.6 5.4l-9.2 9.2"],
  search: [
    "M9.1 14.2a5.1 5.1 0 1 0 0-10.2 5.1 5.1 0 0 0 0 10.2z",
    "M13 13l3.6 3.6",
  ],
  send: [
    "M16.6 3.6L9.4 10.8",
    "M16.6 3.6l-4.5 13-2.7-5.8-5.8-2.7 13-4.5z",
  ],
  music: [
    "M8 14.5a2 2 0 1 1-4 0 2 2 0 0 1 4 0z",
    "M16 13a2 2 0 1 1-4 0 2 2 0 0 1 4 0z",
    "M8 14.5V5.2l8-1.4V13",
  ],
  chevronRight: ["M8 5.2l4.8 4.8L8 14.8"],
  alert: [
    "M10 3.3l7 12.4a1 1 0 0 1-.87 1.5H3.87a1 1 0 0 1-.87-1.5l7-12.4a1 1 0 0 1 1.74 0z",
    "M10 8.2v3.8",
    "M10 14.4h.01",
  ],
  sliders: [
    "M4 6h12",
    "M4 10h12",
    "M4 14h12",
    "M7.5 4.4v3.2",
    "M12.8 8.4v3.2",
    "M8.5 12.4v3.2",
  ],
  compass: [
    "M10 16.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13z",
    "M12.6 7.4l-1.5 3.7-3.7 1.5 1.5-3.7 3.7-1.5z",
  ],
  heart: [
    "M10 16.2s-5.8-3.6-5.8-7.7a3.3 3.3 0 0 1 5.8-2.1 3.3 3.3 0 0 1 5.8 2.1c0 4.1-5.8 7.7-5.8 7.7z",
  ],
  sparkles: [
    "M10 3l1.3 3.8L15 8.1l-3.7 1.3L10 13.2l-1.3-3.8L5 8.1l3.7-1.3L10 3z",
    "M15.2 12.8l.6 1.6 1.6.6-1.6.6-.6 1.6-.6-1.6-1.6-.6 1.6-.6.6-1.6z",
  ],
  dock: [
    "M3.2 4.8h13.6a1.2 1.2 0 0 1 1.2 1.2v8a1.2 1.2 0 0 1-1.2 1.2H3.2A1.2 1.2 0 0 1 2 14V6a1.2 1.2 0 0 1 1.2-1.2z",
    "M13.2 4.8v10.4",
  ],
  key: [
    "M7.4 13.4a3.6 3.6 0 1 1 2.5-6.1L16.5 14v2.5H14v-1.8h-1.8v-1.8l-1.4-1.4a3.6 3.6 0 0 1-3.4 1.9z",
  ],
  timer: [
    "M10 6.2a5.8 5.8 0 1 0 0 11.6 5.8 5.8 0 0 0 0-11.6z",
    "M10 9.2v3.2l2.2 1.4",
    "M8 3.2h4",
  ],
  shuffle: [
    "M14.5 4.2h2.7v2.7",
    "M3.2 14.5l4.2-4.2",
    "M17.2 4.2l-6.2 6.2",
    "M14.5 15.8h2.7v-2.7",
    "M3.2 5.5l4.1 4.1",
    "M11 10.6l6.2 5.2",
  ],
  terminal: [
    "M3.2 4.8h13.6a1.2 1.2 0 0 1 1.2 1.2v8a1.2 1.2 0 0 1-1.2 1.2H3.2A1.2 1.2 0 0 1 2 14V6a1.2 1.2 0 0 1 1.2-1.2z",
    "M5.5 8.5l2.2 1.8-2.2 1.8",
    "M9.8 12.1h3.5",
  ],
  play: [
    "M6.5 4.8l9 5.2-9 5.2V4.8z",
  ],
  pause: [
    "M6.5 5.2h2.4v9.6H6.5z",
    "M11.1 5.2h2.4v9.6h-2.4z",
  ],
  rotateCcw: [
    "M4.2 8.2a6 6 0 1 1 1.3 5.8",
    "M4.2 4.4v3.8h3.8",
  ],
  download: [
    "M10 3.5v9",
    "M6.5 9.2L10 12.8l3.5-3.6",
    "M4 15.8h12",
  ],
  trash: [
    "M4.2 5.6h11.6",
    "M7.8 5.6V4.1h4.4v1.5",
    "M6 5.6l.8 10.2h6.4L14 5.6",
  ],
};

/** 创建 1.75px 圆润矢量 SVG 图标，全站统一零 Emoji 质感。 */
export function createIcon(name: IconName, size = 14): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("class", `ui-icon ui-icon-${name}`);
  svg.setAttribute("aria-hidden", "true");

  for (const d of ICON_PATHS[name]) {
    const path = document.createElementNS(SVG_NS, "path");
    path.setAttribute("d", d);
    path.setAttribute("fill", "none");
    path.setAttribute("stroke", "currentColor");
    path.setAttribute("stroke-width", "1.75");
    path.setAttribute("stroke-linecap", "round");
    path.setAttribute("stroke-linejoin", "round");
    svg.appendChild(path);
  }
  return svg;
}
