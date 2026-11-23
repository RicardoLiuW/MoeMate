export interface CategoryInfo {
  id: string;
  label: string;
}

/**
 * 契约 v1.1 §3 锁定的 20 个标准化动作分类枚举
 */
export const CONTRACT_CATEGORIES: readonly CategoryInfo[] = [
  { id: "idle", label: "待机" },
  { id: "move", label: "移动" },
  { id: "turn", label: "转向" },
  { id: "click", label: "点击" },
  { id: "drag", label: "拖拽" },
  { id: "emotion", label: "情绪" },
  { id: "chatter", label: "闲聊" },
  { id: "daily", label: "日常" },
  { id: "eat", label: "进食" },
  { id: "play", label: "娱乐" },
  { id: "talent", label: "才艺" },
  { id: "work", label: "工作" },
  { id: "seasonal", label: "时节" },
  { id: "festival", label: "节日" },
  { id: "balance", label: "平衡" },
  { id: "outfit", label: "换装" },
  { id: "vfx", label: "特效" },
  { id: "etiquette", label: "礼仪" },
  { id: "species", label: "物种" },
  { id: "other", label: "其他" },
] as const;

const CATEGORY_LABEL_MAP = new Map<string, string>(
  CONTRACT_CATEGORIES.map((item) => [item.id, item.label]),
);

export function getCategoryLabel(category: string): string {
  return CATEGORY_LABEL_MAP.get(category) ?? category;
}
