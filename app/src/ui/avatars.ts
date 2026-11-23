import claudeAvatar from "../assets/avatars/claude.png";
import deepseekAvatar from "../assets/avatars/deepseek.png";
import geminiAvatar from "../assets/avatars/gemini.png";
import glmAvatar from "../assets/avatars/glm.png";
import gptAvatar from "../assets/avatars/gpt.png";
import grokAvatar from "../assets/avatars/grok.png";
import kimiAvatar from "../assets/avatars/kimi.png";
import qwenAvatar from "../assets/avatars/qwen.png";

const AVATAR_MAP: Record<string, string> = {
  claude: claudeAvatar,
  deepseek: deepseekAvatar,
  gemini: geminiAvatar,
  glm: glmAvatar,
  gpt: gptAvatar,
  grok: grokAvatar,
  kimi: kimiAvatar,
  qwen: qwenAvatar,
};

export function getCharAvatarUrl(charId: string): string {
  return AVATAR_MAP[charId.trim().toLowerCase()] ?? geminiAvatar;
}

export function createPixelAvatar(
  charId: string,
  sizePx = 32,
  className = "pixel-avatar-img",
): HTMLImageElement {
  const img = document.createElement("img");
  img.src = getCharAvatarUrl(charId);
  img.alt = charId;
  img.width = sizePx;
  img.height = sizePx;
  img.draggable = false;
  img.className = className;
  img.style.width = `${sizePx}px`;
  img.style.height = `${sizePx}px`;
  return img;
}
