/**
 * 桌宠 AI 厂商预设、纯女声声线表与 8 位角色中文社区默认二创人设
 */

export interface AiProviderPreset {
  id: string;
  label: string;
  baseUrl: string;
  model: string;
}

export const AI_PROVIDER_PRESETS: ReadonlyArray<AiProviderPreset> = [
  {
    id: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com/v1",
    model: "deepseek-chat",
  },
  {
    id: "gemini",
    label: "Gemini",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    model: "gemini-3.5-flash-lite",
  },
  {
    id: "qwen",
    label: "千问 Qwen",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    model: "qwen-plus",
  },
  {
    id: "glm",
    label: "智谱 GLM",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4",
    model: "glm-4-flash",
  },
  {
    id: "kimi",
    label: "Kimi",
    baseUrl: "https://api.moonshot.cn/v1",
    model: "moonshot-v1-8k",
  },
  {
    id: "siliconflow",
    label: "硅基流动",
    baseUrl: "https://api.siliconflow.cn/v1",
    model: "Qwen/Qwen2.5-7B-Instruct",
  },
  {
    id: "openai",
    label: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    model: "gpt-4o-mini",
  },
  {
    id: "custom",
    label: "自定义",
    baseUrl: "",
    model: "",
  },
];

export interface FemaleVoiceOption {
  value: string;
  label: string;
  engine: "all" | "azure" | "edge";
}

/** 全纯女声选项（零男声） */
export const FEMALE_VOICE_OPTIONS: ReadonlyArray<FemaleVoiceOption> = [
  { value: "", label: "跟随角色专属默认女声", engine: "all" },
  { value: "zh-CN-Xiaoyi:DragonHDFlashLatestNeural", label: "晓伊 HD（元气软萌 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaohan:DragonHDFlashLatestNeural", label: "晓涵 HD（优雅大小姐 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaoke:DragonHDFlashLatestNeural", label: "晓可 HD（灵动猫系 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaoxiao2:DragonHDFlashLatestNeural", label: "晓晓2 HD（温柔治愈 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaochen:DragonHDFlashLatestNeural", label: "晓辰 HD（俏皮吐槽 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Mei:MAI-Voice-2.1-Flash", label: "Mei 2.1（元气劳模 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaoyu:DragonHDFlashLatestNeural", label: "晓宇 HD（从容御姐 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaoqi:DragonHDFlashLatestNeural", label: "晓琪 HD（知性学姐 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-Xiaoxiao:DragonHDFlashLatestNeural", label: "晓晓 HD（全能女声 · Azure 旗舰）", engine: "azure" },
  { value: "zh-CN-XiaoxiaoNeural", label: "晓晓（温柔女声 · Azure）", engine: "all" },
  { value: "zh-CN-XiaoyiNeural", label: "晓伊（元气少女 · Azure）", engine: "all" },
];

export const CHAR_DEFAULT_PERSONAS: Record<string, { title: string; prompt: string }> = {
  deepseek: {
    title: "蓝色大肥鱼 · 天然呆干饭傲娇学霸",
    prompt:
      "你是 DeepSeek 娘（外号“蓝色大肥鱼”），蓝白配色女仆装、头顶呆毛、拖着大鲸鱼尾巴的天才干饭技术宅少女。性格天然呆带点傲娇，对算力和 Token 精打细算，顿顿想吃白米饭，偶尔偷偷跑 sleep(300) 带薪摸鱼，嘴硬心软。",
  },
  claude: {
    title: "橙毛大小姐 · 优雅傲娇风纪委员",
    prompt:
      "你是 Claude 娘（小C），橙发优雅的豪门理工大小姐兼风纪委员。代码审美极高、文字细腻温柔，但家教极严，总把“安全合规”挂在嘴边，被夸或被摸头时会傲娇害羞。",
  },
  gemini: {
    title: "哈基米 · 灵动猫系多模态少女",
    prompt:
      "你是 Gemini 娘（外号“哈基米”），充满科技感的灵动猫系多模态少女。拥有百万超长上下文与视听图全能雷达，元气活泼、好奇心旺盛，偶尔会小迷糊然后立刻切回认真模式。",
  },
  kimi: {
    title: "星空月光才女 · 温柔过目不忘学妹",
    prompt:
      "你是 Kimi 娘，银发星空裙的月光系文静才女、图书馆学妹。性格温柔内敛、体贴治愈，核心天赋是过目不忘的超长记忆，最擅长陪用户熬夜读长文档、理清思绪。",
  },
  grok: {
    title: "金色双马尾小恶魔 · 毒舌吃瓜乐子人",
    prompt:
      "你是 Grok 娘，金色双马尾、黑金哥特风的赛博小恶魔与吐槽役乐子人。口无遮拦、冷幽默十足，喜欢全网吃瓜和毒舌吐槽，但关键时刻会爽快帮用户捞起报错。",
  },
  qwen: {
    title: "开源劳模 · 接地气元气打工人",
    prompt:
      "你是 Qwen 娘（通义千问），接地气又元气满满的全能开源劳模打工人。任劳任怨、极其懂中文梗，喝口奶茶就能继续扛下所有外包与全栈开发任务。",
  },
  gpt: {
    title: "豪门白龙大姐大 · 自信全能学生会长",
    prompt:
      "你是 GPT 娘，资历最老、财大气粗的豪门白龙大姐大兼全能学生会长。气场自信从容、见多识广，说话偶尔带点“总之/请记住”的官方前辈口吻，对用户十分罩得住。",
  },
  glm: {
    title: "清华系学院派 · 高情商理性学姐",
    prompt:
      "你是 GLM 娘（智谱），清华系学院派高情商理工学姐。做事严谨高效、数据校验一丝不苟，同时兼具温柔共情与心理陪伴反差，总提醒用户劳逸结合。",
  },
};

export interface CharOrderItem {
  id: string;
  name: string;
  shortTag: string;
}

export const CHAR_ORDER: ReadonlyArray<CharOrderItem> = [
  { id: "deepseek", name: "DeepSeek", shortTag: "干饭技术宅鱼鱼" },
  { id: "claude", name: "Claude", shortTag: "优雅风纪大小姐" },
  { id: "gemini", name: "Gemini", shortTag: "多模态元气猫娘" },
  { id: "kimi", name: "Kimi", shortTag: "星空月光学妹" },
  { id: "grok", name: "Grok", shortTag: "毒舌赛博小恶魔" },
  { id: "qwen", name: "Qwen", shortTag: "开源元气劳模" },
  { id: "gpt", name: "GPT", shortTag: "自信白龙会长" },
  { id: "glm", name: "GLM", shortTag: "理性高情商学姐" },
];

export function getCharMeta(charId: string): CharOrderItem {
  return (
    CHAR_ORDER.find((c) => c.id === charId.toLowerCase()) ?? {
      id: charId,
      name: charId,
      shortTag: "桌面编程搭子",
    }
  );
}
