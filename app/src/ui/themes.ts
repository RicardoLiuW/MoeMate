/**
 * Deskpet v3.0 柔萌马卡龙手账色板与奶油 / 星糖双外观系统
 */

export type UiAppearance = "light" | "dark";

const APPEARANCE_STORAGE_KEY = "deskpet.appearance";

export interface CharTheme {
  accent: string;
  accentStrong: string;
  accentHover: string;
  accentSoft: string;
  accentBorder: string;
  accentRgb: string;
  accentGrad: string;
  badgeBg: string;
  pastelBg: string;
  pastelRing: string;
  monogram: string;
  tagline: string;
}

const DEFAULT_THEME: CharTheme = {
  accent: "#9679cf",
  accentStrong: "#7d5fb8",
  accentHover: "#8365be",
  accentSoft: "rgba(150, 121, 207, 0.14)",
  accentBorder: "rgba(150, 121, 207, 0.36)",
  accentRgb: "150, 121, 207",
  accentGrad: "linear-gradient(135deg, #b39be6 0%, #8668c4 100%)",
  badgeBg: "#f6f2fd",
  pastelBg: "#faf7ff",
  pastelRing: "rgba(150, 121, 207, 0.24)",
  monogram: "DP",
  tagline: "香芋云朵",
};

const CHAR_THEMES: Record<string, CharTheme> = {
  claude: {
    accent: "#de835f",
    accentStrong: "#c46844",
    accentHover: "#cb6f4b",
    accentSoft: "rgba(222, 131, 95, 0.14)",
    accentBorder: "rgba(222, 131, 95, 0.36)",
    accentRgb: "222, 131, 95",
    accentGrad: "linear-gradient(135deg, #f0a182 0%, #d4734e 100%)",
    badgeBg: "#fef4ef",
    pastelBg: "#fff8f4",
    pastelRing: "rgba(222, 131, 95, 0.24)",
    monogram: "Cl",
    tagline: "暖杏奶油",
  },
  deepseek: {
    accent: "#5b93db",
    accentStrong: "#437bc4",
    accentHover: "#4a82cb",
    accentSoft: "rgba(91, 147, 219, 0.14)",
    accentBorder: "rgba(91, 147, 219, 0.36)",
    accentRgb: "91, 147, 219",
    accentGrad: "linear-gradient(135deg, #82b3f0 0%, #4d83cb 100%)",
    badgeBg: "#f0f6fe",
    pastelBg: "#f5f9ff",
    pastelRing: "rgba(91, 147, 219, 0.24)",
    monogram: "DS",
    tagline: "苏打晴蓝",
  },
  gemini: {
    accent: "#9679cf",
    accentStrong: "#7d5fb8",
    accentHover: "#8365be",
    accentSoft: "rgba(150, 121, 207, 0.14)",
    accentBorder: "rgba(150, 121, 207, 0.36)",
    accentRgb: "150, 121, 207",
    accentGrad: "linear-gradient(135deg, #b59be6 0%, #8364c0 100%)",
    badgeBg: "#f5f1fc",
    pastelBg: "#faf7ff",
    pastelRing: "rgba(150, 121, 207, 0.24)",
    monogram: "Ge",
    tagline: "香芋星紫",
  },
  glm: {
    accent: "#4ea2bd",
    accentStrong: "#3989a4",
    accentHover: "#3f90ab",
    accentSoft: "rgba(78, 162, 189, 0.14)",
    accentBorder: "rgba(78, 162, 189, 0.36)",
    accentRgb: "78, 162, 189",
    accentGrad: "linear-gradient(135deg, #74bfd6 0%, #4191ac 100%)",
    badgeBg: "#eef8fb",
    pastelBg: "#f4fbfd",
    pastelRing: "rgba(78, 162, 189, 0.24)",
    monogram: "Gl",
    tagline: "薄荷海盐",
  },
  gpt: {
    accent: "#4aad88",
    accentStrong: "#369470",
    accentHover: "#3c9a76",
    accentSoft: "rgba(74, 173, 136, 0.14)",
    accentBorder: "rgba(74, 173, 136, 0.36)",
    accentRgb: "74, 173, 136",
    accentGrad: "linear-gradient(135deg, #70c9a7 0%, #3d9a77 100%)",
    badgeBg: "#eefaf5",
    pastelBg: "#f4fcf9",
    pastelRing: "rgba(74, 173, 136, 0.24)",
    monogram: "Gp",
    tagline: "开心果青",
  },
  grok: {
    accent: "#8b6bb8",
    accentStrong: "#7253a0",
    accentHover: "#7859a6",
    accentSoft: "rgba(139, 107, 184, 0.14)",
    accentBorder: "rgba(139, 107, 184, 0.36)",
    accentRgb: "139, 107, 184",
    accentGrad: "linear-gradient(135deg, #a98bd2 0%, #7656a4 100%)",
    badgeBg: "#f5f1fa",
    pastelBg: "#faf7fe",
    pastelRing: "rgba(139, 107, 184, 0.24)",
    monogram: "Gk",
    tagline: "树莓暮紫",
  },
  kimi: {
    accent: "#df74a3",
    accentStrong: "#c55988",
    accentHover: "#cc608f",
    accentSoft: "rgba(223, 116, 163, 0.14)",
    accentBorder: "rgba(223, 116, 163, 0.36)",
    accentRgb: "223, 116, 163",
    accentGrad: "linear-gradient(135deg, #f295be 0%, #cd6190 100%)",
    badgeBg: "#fef2f7",
    pastelBg: "#fff7fa",
    pastelRing: "rgba(223, 116, 163, 0.24)",
    monogram: "Km",
    tagline: "樱花奶昔",
  },
  qwen: {
    accent: "#7e73d4",
    accentStrong: "#6458bc",
    accentHover: "#6b5fc2",
    accentSoft: "rgba(126, 115, 212, 0.14)",
    accentBorder: "rgba(126, 115, 212, 0.36)",
    accentRgb: "126, 115, 212",
    accentGrad: "linear-gradient(135deg, #9d93ea 0%, #6b60c2 100%)",
    badgeBg: "#f3f1fd",
    pastelBg: "#f8f7ff",
    pastelRing: "rgba(126, 115, 212, 0.24)",
    monogram: "Qw",
    tagline: "薰衣草糖",
  },
};

export function getCharTheme(charId: string): CharTheme {
  return CHAR_THEMES[charId.toLowerCase()] ?? DEFAULT_THEME;
}

export function applyCharTheme(charId: string): CharTheme {
  const theme = getCharTheme(charId);
  const root = document.documentElement;
  root.style.setProperty("--accent", theme.accent);
  root.style.setProperty("--accent-strong", theme.accentStrong);
  root.style.setProperty("--accent-hover", theme.accentHover);
  root.style.setProperty("--accent-soft", theme.accentSoft);
  root.style.setProperty("--accent-border", theme.accentBorder);
  root.style.setProperty("--accent-rgb", theme.accentRgb);
  root.style.setProperty("--accent-grad", theme.accentGrad);
  root.style.setProperty("--accent-tint-bg", theme.badgeBg);
  root.style.setProperty("--accent-pastel-bg", theme.pastelBg);
  root.style.setProperty("--accent-pastel-ring", theme.pastelRing);
  return theme;
}

export function loadSavedAppearance(): UiAppearance {
  try {
    const saved = localStorage.getItem(APPEARANCE_STORAGE_KEY);
    if (saved === "dark" || saved === "light") {
      return saved;
    }
  } catch {
    // ignore storage errors
  }
  return "light";
}

export function applyAppearance(mode: UiAppearance): UiAppearance {
  const root = document.documentElement;
  root.dataset.theme = mode;
  const meta = document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]');
  if (meta) {
    meta.content = mode;
  }
  try {
    localStorage.setItem(APPEARANCE_STORAGE_KEY, mode);
  } catch {
    // ignore storage errors
  }
  return mode;
}
