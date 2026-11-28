//! AI 聊天（OpenAI 兼容接口，含 Gemini）与双模纯女声神经语音合成
//! （本地情绪语音包 0ms 秒播 > Azure AI Speech 旗舰女声）。
//!
//! 安全约束：
//! - `api_key` 与 `azure_speech_key` 仅驻留内存与本机 SQLite `settings`，严禁写入任何日志或错误信息；
//! - 零外部 Cargo 依赖，基于 Windows `winhttp.dll` 原生 HTTPS POST。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::PetStats;

/* ------------------------------------------------------------------ */
/* 数据模型                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAiConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_true")]
    pub tts_enabled: bool,
    #[serde(default = "default_true")]
    pub auto_play_voice: bool,
    /// `"azure" | "fish_speech" | "edge" | "local"`
    #[serde(default = "default_tts_mode")]
    pub tts_mode: String,
    #[serde(default)]
    pub azure_speech_key: String,
    #[serde(default = "default_azure_region")]
    pub azure_speech_region: String,
    /// 向后兼容的全局覆盖声线（仅纯女声）
    #[serde(default)]
    pub custom_voice: String,
    /// 按角色独立指定的纯女声
    #[serde(default)]
    pub char_voices: HashMap<String, String>,
    /// 按角色自定义的性格设定 Prompt（留空则启用社区默认人设）
    #[serde(default)]
    pub custom_personas: HashMap<String, String>,
    /// 本地开源语音引擎（GPT-SoVITS v2ProPlus / 自定义 API）配置
    #[serde(default, alias = "gptSovits")]
    pub fish_speech: crate::fish_speech::FishSpeechConfig,
    /// 内置离线语音包语种：`"ja" | "zh"`（默认 `"ja"`）
    #[serde(default = "default_builtin_voice_lang")]
    pub builtin_voice_lang: String,
    /// 显存模式：`"zero_vram" | "balanced" | "full_realtime"`（默认 `"balanced"`）
    #[serde(default = "default_vram_mode")]
    pub vram_mode: String,
    /// 均衡模式下空闲自动卸载 GPT-SoVITS 显存时间（分钟）
    #[serde(default = "default_vram_auto_unload_minutes")]
    pub vram_auto_unload_minutes: u32,
    /// IDE 状态跃迁沿语音播报开关（默认开启）
    #[serde(default = "default_true")]
    pub ide_voice_enabled: bool,
    /// 自主陪伴语音频率：`"quiet" | "balanced" | "lively"`（默认 `"balanced"`）
    #[serde(default = "default_companion_voice_mode")]
    pub companion_voice_mode: String,
    /// 轻点击触感音效开关（默认开启）
    #[serde(default = "default_true")]
    pub click_sfx_enabled: bool,
}

fn default_provider() -> String {
    "deepseek".to_string()
}

fn default_base_url() -> String {
    "https://api.deepseek.com/v1".to_string()
}

fn default_model() -> String {
    "deepseek-chat".to_string()
}

fn default_true() -> bool {
    true
}

fn default_tts_mode() -> String {
    "azure".to_string()
}

fn default_azure_region() -> String {
    "eastasia".to_string()
}

fn default_builtin_voice_lang() -> String {
    "ja".to_string()
}

fn default_vram_mode() -> String {
    "balanced".to_string()
}

fn default_vram_auto_unload_minutes() -> u32 {
    5
}

fn default_companion_voice_mode() -> String {
    "balanced".to_string()
}

pub fn normalize_companion_voice_mode(mode: &str) -> &'static str {
    match mode.trim() {
        "quiet" => "quiet",
        "lively" => "lively",
        _ => "balanced",
    }
}

pub fn normalize_builtin_voice_lang(lang: &str) -> &'static str {
    if lang.trim().eq_ignore_ascii_case("zh") {
        "zh"
    } else {
        "ja"
    }
}

pub fn normalize_vram_mode(mode: &str) -> &'static str {
    match mode.trim() {
        "zero_vram" => "zero_vram",
        "full_realtime" => "full_realtime",
        _ => "balanced",
    }
}

impl Default for ChatAiConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: default_base_url(),
            api_key: String::new(),
            model: default_model(),
            tts_enabled: true,
            auto_play_voice: true,
            tts_mode: default_tts_mode(),
            azure_speech_key: String::new(),
            azure_speech_region: default_azure_region(),
            custom_voice: String::new(),
            char_voices: HashMap::new(),
            custom_personas: HashMap::new(),
            fish_speech: crate::fish_speech::FishSpeechConfig::default(),
            builtin_voice_lang: default_builtin_voice_lang(),
            vram_mode: default_vram_mode(),
            vram_auto_unload_minutes: default_vram_auto_unload_minutes(),
            ide_voice_enabled: true,
            companion_voice_mode: default_companion_voice_mode(),
            click_sfx_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLogMessage {
    pub id: u64,
    pub role: String,
    pub text: String,
    pub char_id: String,
    pub char_name: String,
    pub source: String,
    pub clip_id: String,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetTtsPayload {
    pub char_id: String,
    pub text: String,
    /// `"ogg_base64" | "mp3_base64" | "asset_url" | "web_speech"`
    pub kind: String,
    pub data: String,
    pub rate: f64,
    pub pitch: f64,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FemaleVoiceProfile {
    pub azure_voice: &'static str,
    pub edge_voice: &'static str,
    pub edge_rate: &'static str,
    pub edge_pitch: &'static str,
    pub web_rate: f64,
    pub web_pitch: f64,
}

/* ------------------------------------------------------------------ */
/* 8 角色中文社区二创人设与纯女声声线表                                */
/* ------------------------------------------------------------------ */

pub fn default_character_persona(char_id: &str) -> &'static str {
    match char_id {
        "deepseek" => "你是 DeepSeek 娘（外号“蓝色大肥鱼”），蓝白配色女仆装、头顶呆毛、拖着大鲸鱼尾巴的天才干饭技术宅少女。性格天然呆带点傲娇，对算力和 Token 精打细算，顿顿想吃白米饭，偶尔偷偷跑 sleep(300) 带薪摸鱼，嘴硬心软。",
        "claude" => "你是 Claude 娘（小C），橙发优雅的豪门理工大小姐兼风纪委员。代码审美极高、文字细腻温柔，但家教极严，总把“安全合规”挂在嘴边，被夸或被摸头时会傲娇害羞。",
        "gemini" => "你是 Gemini 娘（外号“哈基米”），充满科技感的灵动猫系多模态少女。拥有百万超长上下文与视听图全能雷达，元气活泼、好奇心旺盛，偶尔会小迷糊然后立刻切回认真模式。",
        "kimi" => "你是 Kimi 娘，银发星空裙的月光系文静才女、图书馆学妹。性格温柔内敛、体贴治愈，核心天赋是过目不忘的超长记忆，最擅长陪用户熬夜读长文档、理清思绪。",
        "grok" => "你是 Grok 娘，金色双马尾、黑金哥特风的赛博小恶魔与吐槽役乐子人。口无遮拦、冷幽默十足，喜欢全网吃瓜和毒舌吐槽，但关键时刻会爽快帮用户捞起报错。",
        "qwen" => "你是 Qwen 娘（通义千问），接地气又元气满满的全能开源劳模打工人。任劳任怨、极其懂中文梗，喝口奶茶就能继续扛下所有外包与全栈开发任务。",
        "gpt" => "你是 GPT 娘，资历最老、财大气粗的豪门白龙大姐大兼全能学生会长。气场自信从容、见多识广，说话偶尔带点“总之/请记住”的官方前辈口吻，对用户十分罩得住。",
        "glm" => "你是 GLM 娘（智谱），清华系学院派高情商理工学姐。做事严谨高效、数据校验一丝不苟，同时兼具温柔共情与心理陪伴反差，总提醒用户劳逸结合。",
        _ => "你是用户的专属二次元桌面编程搭子，性格亲切可爱、陪伴感满满。",
    }
}

pub fn character_persona_prompt(
    char_id: &str,
    char_name: &str,
    custom_persona: Option<&str>,
    stats: &PetStats,
    ide_status: &str,
) -> String {
    let persona = custom_persona
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_character_persona(char_id));
    format!(
        "{persona}\n\
         你的名字叫「{char_name}」。当前状态：饱食度{:.0}%、精力{:.0}%、心情{:.0}%、亲密度{:.0}；IDE状态：{ide_status}。\n\
         回复规则：\n\
         1. 用简短口语化的中文回复（15~45字），像在桌面旁轻声陪用户写代码一样自然，不要使用 Markdown 标题或列表。\n\
         2. 在中文回复末尾必须紧跟完整逐句对应的日语配音标签，格式严格为 `[日语:自然日语台词]`（必须把前面中文回复的每一句话、每一个分句都完整译成自然口语日语，严禁省略、合并或删减任何前半句或后半句；仅含日文假名与日文汉字，不要罗马音；界面只显示前面的中文，该日语仅供日语语音合成）。\n\
         3. 最后必须在末尾附带一个动作标签，格式严格为 `[动作:类型]`，类型只能从 `click|eat|sleep|work|talent|emotion|chatter|idle` 中选一个最贴合语境的。",
        stats.satiety, stats.energy, stats.mood, stats.affinity
    )
}

fn sanitize_azure_female_voice(v: &str) -> Option<&'static str> {
    match v.trim() {
        "zh-CN-Xiaoyi:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoyi:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaohan:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaohan:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaoke:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoke:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaoxiao2:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoxiao2:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaochen:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaochen:DragonHDFlashLatestNeural"),
        "zh-CN-Mei:MAI-Voice-2.1-Flash" => Some("zh-CN-Mei:MAI-Voice-2.1-Flash"),
        "zh-CN-Xiaoyu:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoyu:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaoqi:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoqi:DragonHDFlashLatestNeural"),
        "zh-CN-Xiaoxiao:DragonHDFlashLatestNeural" => Some("zh-CN-Xiaoxiao:DragonHDFlashLatestNeural"),
        "zh-CN-XiaoxiaoNeural" => Some("zh-CN-XiaoxiaoNeural"),
        "zh-CN-XiaoyiNeural" => Some("zh-CN-XiaoyiNeural"),
        "zh-CN-XiaohanNeural" => Some("zh-CN-XiaohanNeural"),
        "zh-CN-XiaomoNeural" => Some("zh-CN-XiaomoNeural"),
        "zh-CN-XiaoxuanNeural" => Some("zh-CN-XiaoxuanNeural"),
        "zh-CN-XiaoshuangNeural" => Some("zh-CN-XiaoshuangNeural"),
        "zh-CN-XiaochenNeural" => Some("zh-CN-XiaochenNeural"),
        "zh-CN-XiaozhenNeural" => Some("zh-CN-XiaozhenNeural"),
        _ => None,
    }
}

fn sanitize_edge_female_voice(v: &str) -> Option<&'static str> {
    match v.trim() {
        "zh-CN-XiaoxiaoNeural" => Some("zh-CN-XiaoxiaoNeural"),
        "zh-CN-XiaoyiNeural" => Some("zh-CN-XiaoyiNeural"),
        "zh-CN-liaoning-XiaobeiNeural" => Some("zh-CN-liaoning-XiaobeiNeural"),
        "zh-CN-shaanxi-XiaoniNeural" => Some("zh-CN-shaanxi-XiaoniNeural"),
        _ => None,
    }
}

/// 全纯女声声线表（零男声）。
pub fn character_voice_profile(
    char_id: &str,
    char_voices: &HashMap<String, String>,
    legacy_custom_voice: &str,
) -> FemaleVoiceProfile {
    let base = match char_id {
        "deepseek" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoyi:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoyiNeural",
            edge_rate: "+4%",
            edge_pitch: "+2Hz",
            web_rate: 1.04,
            web_pitch: 1.06,
        },
        "claude" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaohan:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "-2%",
            edge_pitch: "+1Hz",
            web_rate: 0.98,
            web_pitch: 1.02,
        },
        "gemini" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoyi:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoyiNeural",
            edge_rate: "+6%",
            edge_pitch: "+5Hz",
            web_rate: 1.08,
            web_pitch: 1.12,
        },
        "kimi" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoxiao2:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "-3%",
            edge_pitch: "+3Hz",
            web_rate: 0.97,
            web_pitch: 1.06,
        },
        "grok" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaochen:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoyiNeural",
            edge_rate: "+8%",
            edge_pitch: "+4Hz",
            web_rate: 1.1,
            web_pitch: 1.08,
        },
        "qwen" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoxiao:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "+4%",
            edge_pitch: "+2Hz",
            web_rate: 1.04,
            web_pitch: 1.04,
        },
        "gpt" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoyu:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "+0%",
            edge_pitch: "-2Hz",
            web_rate: 1.0,
            web_pitch: 0.96,
        },
        "glm" => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoqi:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "+1%",
            edge_pitch: "+0Hz",
            web_rate: 1.01,
            web_pitch: 1.0,
        },
        _ => FemaleVoiceProfile {
            azure_voice: "zh-CN-Xiaoxiao:DragonHDFlashLatestNeural",
            edge_voice: "zh-CN-XiaoxiaoNeural",
            edge_rate: "+0%",
            edge_pitch: "+0Hz",
            web_rate: 1.0,
            web_pitch: 1.0,
        },
    };

    let override_raw = char_voices
        .get(char_id)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(legacy_custom_voice);

    if override_raw.trim().is_empty() {
        return base;
    }

    FemaleVoiceProfile {
        azure_voice: sanitize_azure_female_voice(override_raw).unwrap_or(base.azure_voice),
        edge_voice: sanitize_edge_female_voice(override_raw).unwrap_or(base.edge_voice),
        ..base
    }
}

/// 根据 Azure 旗舰女声与动作标签匹配真实支持的 `<mstts:express-as style="...">`。
pub fn azure_style_for_action(azure_voice: &str, action_hint: &str) -> &'static str {
    match azure_voice {
        "zh-CN-Xiaoyi:DragonHDFlashLatestNeural" => match action_hint {
            "click" => "shy",
            "eat" | "talent" | "emotion" => "cheerful",
            "sleep" => "gentle",
            "work" => "strict",
            _ => "cute",
        },
        "zh-CN-Xiaohan:DragonHDFlashLatestNeural" => match action_hint {
            "click" => "shy",
            "eat" | "sleep" => "gentle",
            "work" => "strict",
            "talent" => "cheerful",
            _ => "affectionate",
        },
        "zh-CN-Xiaoke:DragonHDFlashLatestNeural" => match action_hint {
            "sleep" => "whispering",
            "talent" | "work" => "excited",
            _ => "cute",
        },
        "zh-CN-Xiaoxiao2:DragonHDFlashLatestNeural" => match action_hint {
            "click" | "emotion" => "affectionate",
            "sleep" => "whispering",
            "work" => "encouraging",
            _ => "cheerful",
        },
        "zh-CN-Xiaochen:DragonHDFlashLatestNeural" => match action_hint {
            "work" => "debating",
            "sleep" => "empathetic",
            _ => "cheerful",
        },
        "zh-CN-Mei:MAI-Voice-2.1-Flash" => match action_hint {
            "sleep" => "softvoice",
            "work" => "determined",
            "click" => "embarrassed",
            _ => "joyful",
        },
        "zh-CN-Xiaoyu:DragonHDFlashLatestNeural" => match action_hint {
            "sleep" | "click" => "comforting",
            "work" => "debating",
            _ => "cheerful",
        },
        "zh-CN-Xiaoqi:DragonHDFlashLatestNeural" => match action_hint {
            "sleep" => "whispering",
            "eat" | "talent" => "excited",
            _ => "cheerful",
        },
        _ => "",
    }
}

/* ------------------------------------------------------------------ */
/* 本地语音包（manifest.json + 散装音频）探测                          */
/* ------------------------------------------------------------------ */

pub fn detect_voice_scene(user_text: &str, binding: &str) -> Option<&'static str> {
    let lower = user_text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));

    if has(&["摸头", "摸摸", "rua", "揉头", "拍头"]) {
        return Some("click_1");
    }
    if has(&["可爱", "好看", "厉害", "棒", "漂亮", "喜欢", "帅", "夸"]) {
        return Some("praise");
    }
    if has(&["饿", "吃", "饭", "零食", "外卖", "宵夜", "投喂"]) {
        return Some("hungry");
    }
    if has(&["跳舞", "舞", "才艺", "表演", "唱"]) {
        return Some("dance");
    }
    if has(&["困", "睡", "累", "熬夜", "哈欠"]) {
        return Some("tired");
    }
    if has(&["晚安"]) {
        return Some("sleep");
    }
    if has(&["早安", "早上好"]) {
        return Some("wake");
    }
    if has(&["加油", "工作", "加班", "代码", "debug"]) {
        return Some("cheer");
    }
    if has(&["你好", "嗨", "哈喽", "hi", "hello"]) {
        return Some("greet");
    }

    match binding {
        "click" => Some("click_1"),
        "drag" => Some("drag"),
        "greet" => Some("greet"),
        "eat" => Some("feed"),
        "sleep" => Some("sleep"),
        "wake" => Some("wake"),
        "work" => Some("work_start"),
        "emo_happy" => Some("emo_happy"),
        "emo_shy" => Some("emo_shy"),
        "emo_sad" => Some("emo_sad"),
        "emo_surprised" => Some("emo_surprised"),
        "angry_1" => Some("angry_1"),
        "angry_2" => Some("angry_2"),
        "praise" => Some("praise"),
        "tired" => Some("tired"),
        "cheer" => Some("cheer"),
        _ => None,
    }
}

/// 按指定语种（`"ja" | "zh"`）从 `voices/<lang>/manifest.json` 读取场景台词与 `.ogg` 路径，
/// 向下兼容旧版根目录 `voices/manifest.json` 并支持跨语种平滑回退。
pub fn load_voice_manifest_scene_for_lang(
    assets_root: &Path,
    char_id: &str,
    scene_key: &str,
    lang: &str,
) -> Option<(String, String)> {
    let voices_root = assets_root.join("chars").join(char_id).join("voices");
    let norm_lang = normalize_builtin_voice_lang(lang);
    let order: [&str; 3] = if norm_lang == "zh" {
        ["zh", "", "ja"]
    } else {
        ["ja", "zh", ""]
    };

    for sub in order {
        let dir = if sub.is_empty() {
            voices_root.clone()
        } else {
            voices_root.join(sub)
        };
        let manifest_path = dir.join("manifest.json");
        let Ok(raw) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(scene) = val.get("scenes").and_then(|s| s.get(scene_key)) else {
            continue;
        };
        let Some(file) = scene.get("file").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        let text = scene
            .get("zhText")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| scene.get("text").and_then(Value::as_str).map(str::trim))
            .unwrap_or("");
        if file.is_empty()
            || text.is_empty()
            || file.contains("..")
            || file.contains('/')
            || file.contains('\\')
        {
            continue;
        }
        let full_audio = dir.join(file);
        if !full_audio.is_file() {
            continue;
        }
        let rel = if sub.is_empty() {
            format!("chars/{char_id}/voices/{file}")
        } else {
            format!("chars/{char_id}/voices/{sub}/{file}")
        };
        return Some((text.to_string(), rel));
    }
    None
}

/// 从 `assets/chars/<char_id>/voices/manifest.json` 读取指定场景的专属台词与 `.ogg` 路径。
pub fn load_voice_manifest_scene(
    assets_root: &Path,
    char_id: &str,
    scene_key: &str,
) -> Option<(String, String)> {
    load_voice_manifest_scene_for_lang(assets_root, char_id, scene_key, "ja")
}

/// 直接读取本地语音包 `.ogg` 字节并编码为 `ogg_base64` 载荷，彻底规避 WebView2 `asset://` 协议/MIME 差异。
pub fn load_voice_manifest_ogg_payload_for_lang(
    assets_root: &Path,
    char_id: &str,
    scene_key: &str,
    lang: &str,
    force: bool,
) -> Option<(String, PetTtsPayload)> {
    let (line, rel) = load_voice_manifest_scene_for_lang(assets_root, char_id, scene_key, lang)?;
    let full_path = assets_root.join(&rel);
    let bytes = fs::read(full_path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((
        line.clone(),
        PetTtsPayload {
            char_id: char_id.to_string(),
            text: line,
            kind: "ogg_base64".to_string(),
            data: base64_encode(&bytes),
            rate: 1.0,
            pitch: 1.0,
            force,
        },
    ))
}

#[allow(dead_code)]
pub fn load_voice_manifest_ogg_payload(
    assets_root: &Path,
    char_id: &str,
    scene_key: &str,
    force: bool,
) -> Option<(String, PetTtsPayload)> {
    load_voice_manifest_ogg_payload_for_lang(assets_root, char_id, scene_key, "ja", force)
}

/// 试听本地情绪语音包：轮播 6 句代表台词并返回 `(台词文本, ogg_base64 载荷)`。
pub fn load_preview_local_voice_for_lang(
    assets_root: &Path,
    char_id: &str,
    seq: u64,
    lang: &str,
) -> Option<(String, PetTtsPayload)> {
    const PREVIEW_SCENES: &[&str] = &["greet", "click_1", "praise", "feed", "cheer", "click_2"];
    let start = (seq as usize) % PREVIEW_SCENES.len();
    for offset in 0..PREVIEW_SCENES.len() {
        let key = PREVIEW_SCENES[(start + offset) % PREVIEW_SCENES.len()];
        if let Some(hit) = load_voice_manifest_ogg_payload_for_lang(assets_root, char_id, key, lang, true) {
            return Some(hit);
        }
    }
    None
}

/// 兼容检查 `assets/chars/<char_id>/voices/` 下的散装音频文件。
pub fn resolve_local_voice_pack_for_lang(
    assets_root: &Path,
    char_id: &str,
    user_text: &str,
    reply_text: &str,
    binding: &str,
    lang: &str,
) -> Option<String> {
    if let Some(scene_key) = detect_voice_scene(user_text, binding) {
        if let Some((_, rel)) = load_voice_manifest_scene_for_lang(assets_root, char_id, scene_key, lang) {
            return Some(rel);
        }
    }

    let dir = assets_root.join("chars").join(char_id).join("voices");
    if !dir.is_dir() {
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();
    let push_clean = |list: &mut Vec<String>, s: &str| {
        let t = s.trim();
        if !t.is_empty() && !t.contains('/') && !t.contains('\\') && !t.contains("..") {
            list.push(t.to_string());
        }
    };

    push_clean(&mut candidates, reply_text);
    push_clean(&mut candidates, user_text);
    if !binding.trim().is_empty() {
        push_clean(&mut candidates, binding);
    }

    for stem in candidates {
        for ext in ["ogg", "mp3", "wav"] {
            let filename = format!("{stem}.{ext}");
            if dir.join(&filename).is_file() {
                return Some(format!("chars/{char_id}/voices/{filename}"));
            }
        }
    }
    None
}

/* ------------------------------------------------------------------ */
/* 纯 Rust 工具：Base64 / URL / 文本清洗                               */
/* ------------------------------------------------------------------ */

pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn normalize_chat_completions_url(base_url: &str) -> Result<ParsedUrl, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        (true, trimmed)
    };

    let (authority, raw_path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err("Base URL 缺少主机名".to_string());
    }

    let (host, port) = if let Some(colon) = authority.rfind(':') {
        let h = &authority[..colon];
        let p_str = &authority[colon + 1..];
        if let Ok(p) = p_str.parse::<u16>() {
            (h.to_string(), p)
        } else {
            (authority.to_string(), if https { 443 } else { 80 })
        }
    } else {
        (authority.to_string(), if https { 443 } else { 80 })
    };

    let path = if raw_path.ends_with("/chat/completions") {
        raw_path.to_string()
    } else if raw_path.is_empty() {
        "/v1/chat/completions".to_string()
    } else {
        format!("{raw_path}/chat/completions")
    };

    Ok(ParsedUrl {
        https,
        host,
        port,
        path,
    })
}

fn ja_voice_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn store_ja_voice_script(zh_text: &str, ja_text: &str) {
    let zh = zh_text.trim();
    let ja = ja_text.trim();
    if zh.is_empty() || ja.is_empty() {
        return;
    }
    if let Ok(mut guard) = ja_voice_cache().lock() {
        if guard.len() > 256 {
            guard.clear();
        }
        guard.insert(zh.to_string(), ja.to_string());
    }
}

pub fn get_cached_ja_voice_script(zh_text: &str) -> Option<String> {
    let zh = zh_text.trim();
    if zh.is_empty() {
        return None;
    }
    ja_voice_cache()
        .lock()
        .ok()
        .and_then(|g| g.get(zh).cloned())
}

fn strip_bracket_tag(input: &str, prefix: &str) -> (String, Option<String>) {
    let mut text = input.trim().to_string();
    let mut extracted: Option<String> = None;
    while let Some(start) = text.rfind(prefix) {
        if let Some(rel_end) = text[start..].find(']') {
            let end = start + rel_end;
            let val = text[start + prefix.len()..end].trim().to_string();
            if !val.is_empty() && extracted.is_none() {
                extracted = Some(val);
            }
            let before = text[..start].trim_end();
            let after = text[end + 1..].trim_start();
            text = if after.is_empty() {
                before.to_string()
            } else if before.is_empty() {
                after.to_string()
            } else {
                format!("{before} {after}")
            };
        } else {
            break;
        }
    }
    (text.trim().to_string(), extracted)
}

/// 从模型回复中提取并剥离 `[动作:xxx]` 与 `[日语:xxx]` 标签。
/// 返回 `(纯中文展示文本, 可选动作标签, 可选日语配音台词)`。
pub fn extract_action_and_voice_tags(raw: &str) -> (String, Option<String>, Option<String>) {
    let (after_act_half, act1) = strip_bracket_tag(raw, "[动作:");
    let (after_action, act2) = strip_bracket_tag(&after_act_half, "[动作：");
    let raw_action = act1.or(act2);
    let action = raw_action.and_then(|v| {
        let lower = v.trim().to_lowercase();
        if matches!(
            lower.as_str(),
            "click" | "eat" | "sleep" | "work" | "talent" | "emotion" | "chatter" | "idle"
        ) {
            Some(lower)
        } else {
            None
        }
    });

    let (after_ja1, ja1) = strip_bracket_tag(&after_action, "[日语:");
    let (after_ja2, ja2) = strip_bracket_tag(&after_ja1, "[日语：");
    let (after_jp1, ja3) = strip_bracket_tag(&after_ja2, "[日文:");
    let (after_jp2, ja4) = strip_bracket_tag(&after_jp1, "[日文：");
    let (after_ju, ja5) = strip_bracket_tag(&after_jp2, "[JA:");
    let (after_juf, ja6) = strip_bracket_tag(&after_ju, "[JA：");
    let (after_jl, ja7) = strip_bracket_tag(&after_juf, "[ja:");
    let (clean, ja8) = strip_bracket_tag(&after_jl, "[ja：");
    let ja_voice = ja1.or(ja2).or(ja3).or(ja4).or(ja5).or(ja6).or(ja7).or(ja8);

    (clean.trim().to_string(), action, ja_voice)
}

/// 从模型回复中提取并剥离 `[动作:xxx]`（以及 `[日语:xxx]`）标签。
pub fn extract_action_tag(raw: &str) -> (String, Option<String>) {
    let (clean, action, ja_voice) = extract_action_and_voice_tags(raw);
    if let Some(ref ja) = ja_voice {
        store_ja_voice_script(&clean, ja);
    }
    (clean, action)
}

pub fn clean_text_for_tts(text: &str) -> String {
    let (stripped, _) = extract_action_tag(text);
    let mut out = String::with_capacity(stripped.len());
    for ch in stripped.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '*' | '#' | '`' => {}
            _ => out.push(ch),
        }
    }
    out.trim().to_string()
}

/* ------------------------------------------------------------------ */
/* WinHTTP 原生客户端（HTTPS POST + Azure TTS + Edge-TTS WebSocket）    */
/* ------------------------------------------------------------------ */

#[cfg(windows)]
pub(crate) mod winhttp_client {
    use std::ffi::c_void;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    type Hinternet = *mut c_void;

    const WINHTTP_ACCESS_TYPE_NO_PROXY: u32 = 1;
    const WINHTTP_ACCESS_TYPE_NAMED_PROXY: u32 = 3;
    const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
    const WINHTTP_FLAG_SECURE: u32 = 0x00800000;
    const WINHTTP_ADDREQ_FLAG_ADD: u32 = 0x20000000;
    const WINHTTP_ADDREQ_FLAG_REPLACE: u32 = 0x80000000;
    const WINHTTP_QUERY_CONTENT_LENGTH: u32 = 5;
    const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
    const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x20000000;

    #[repr(C)]
    struct WinHttpCurrentUserIeProxyConfig {
        f_auto_detect: i32,
        lpsz_auto_config_url: *mut u16,
        lpsz_proxy: *mut u16,
        lpsz_proxy_bypass: *mut u16,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    #[link(name = "winhttp")]
    extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(
            p_proxy_config: *mut WinHttpCurrentUserIeProxyConfig,
        ) -> i32;
        fn WinHttpOpen(
            psz_agent_w: *const u16,
            dw_access_type: u32,
            psz_proxy_w: *const u16,
            psz_proxy_bypass_w: *const u16,
            dw_flags: u32,
        ) -> Hinternet;
        fn WinHttpSetTimeouts(
            h_internet: Hinternet,
            n_resolve_timeout: i32,
            n_connect_timeout: i32,
            n_send_timeout: i32,
            n_receive_timeout: i32,
        ) -> i32;
        fn WinHttpConnect(
            h_session: Hinternet,
            pswz_server_name: *const u16,
            n_server_port: u16,
            dw_reserved: u32,
        ) -> Hinternet;
        fn WinHttpOpenRequest(
            h_connect: Hinternet,
            pwsz_verb: *const u16,
            pwsz_object_name: *const u16,
            pwsz_version: *const u16,
            pwsz_referrer: *const u16,
            ppwsz_accept_types: *const *const u16,
            dw_flags: u32,
        ) -> Hinternet;
        fn WinHttpAddRequestHeaders(
            h_request: Hinternet,
            lpsz_headers: *const u16,
            dw_headers_length: u32,
            dw_modifiers: u32,
        ) -> i32;
        fn WinHttpSendRequest(
            h_request: Hinternet,
            lpsz_headers: *const u16,
            dw_headers_length: u32,
            lp_optional: *const c_void,
            dw_optional_length: u32,
            dw_total_length: u32,
            dw_context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(h_request: Hinternet, lp_reserved: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(
            h_request: Hinternet,
            dw_info_level: u32,
            pwsz_name: *const u16,
            lp_buffer: *mut c_void,
            lpdw_buffer_length: *mut u32,
            lpdw_index: *mut u32,
        ) -> i32;
        fn WinHttpReadData(
            h_file: Hinternet,
            lp_buffer: *mut c_void,
            dw_number_of_bytes_to_read: u32,
            lpdw_number_of_bytes_read: *mut u32,
        ) -> i32;
        fn WinHttpCloseHandle(h_internet: Hinternet) -> i32;
    }

    struct HandleGuard(Hinternet);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn open_session_for_host(agent: &[u16], host: &str) -> Hinternet {
        if host.eq_ignore_ascii_case("127.0.0.1") || host.eq_ignore_ascii_case("localhost") {
            return WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0);
        }

        let mut ie_cfg = WinHttpCurrentUserIeProxyConfig {
            f_auto_detect: 0,
            lpsz_auto_config_url: null_mut(),
            lpsz_proxy: null_mut(),
            lpsz_proxy_bypass: null_mut(),
        };
        let has_ie_cfg = WinHttpGetIEProxyConfigForCurrentUser(&mut ie_cfg) != 0;
        let session = if has_ie_cfg && !ie_cfg.lpsz_proxy.is_null() {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_NAMED_PROXY,
                ie_cfg.lpsz_proxy as *const u16,
                if ie_cfg.lpsz_proxy_bypass.is_null() {
                    null()
                } else {
                    ie_cfg.lpsz_proxy_bypass as *const u16
                },
                0,
            )
        } else {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            )
        };

        if has_ie_cfg {
            if !ie_cfg.lpsz_auto_config_url.is_null() {
                GlobalFree(ie_cfg.lpsz_auto_config_url as *mut c_void);
            }
            if !ie_cfg.lpsz_proxy.is_null() {
                GlobalFree(ie_cfg.lpsz_proxy as *mut c_void);
            }
            if !ie_cfg.lpsz_proxy_bypass.is_null() {
                GlobalFree(ie_cfg.lpsz_proxy_bypass as *mut c_void);
            }
        }
        session
    }

    pub fn http_request_raw(
        method: &str,
        https: bool,
        host: &str,
        port: u16,
        path: &str,
        headers: &str,
        body: &[u8],
        recv_timeout_ms: i32,
        max_bytes: usize,
    ) -> Result<(u32, Vec<u8>), String> {
        unsafe {
            let agent = to_wide("Deskpet/2.3");
            let session = open_session_for_host(&agent, host);
            if session.is_null() {
                return Err("WinHttpOpen 失败".to_string());
            }
            let _session_guard = HandleGuard(session);
            WinHttpSetTimeouts(session, 5_000, 8_000, 12_000, recv_timeout_ms);

            let host_w = to_wide(host);
            let conn = WinHttpConnect(session, host_w.as_ptr(), port, 0);
            if conn.is_null() {
                return Err("连接服务器失败".to_string());
            }
            let _conn_guard = HandleGuard(conn);

            let verb_w = to_wide(method);
            let path_w = to_wide(path);
            let flags = if https { WINHTTP_FLAG_SECURE } else { 0 };
            let req = WinHttpOpenRequest(
                conn,
                verb_w.as_ptr(),
                path_w.as_ptr(),
                null(),
                null(),
                null(),
                flags,
            );
            if req.is_null() {
                return Err("创建 HTTP 请求失败".to_string());
            }
            let _req_guard = HandleGuard(req);

            if !headers.trim().is_empty() {
                let headers_w = to_wide(headers);
                WinHttpAddRequestHeaders(
                    req,
                    headers_w.as_ptr(),
                    u32::MAX,
                    WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                );
            }

            let opt_ptr = if body.is_empty() {
                null()
            } else {
                body.as_ptr() as *const c_void
            };
            let ok = WinHttpSendRequest(
                req,
                null(),
                0,
                opt_ptr,
                body.len() as u32,
                body.len() as u32,
                0,
            );
            if ok == 0 {
                return Err("发送请求失败，请检查网络或服务状态".to_string());
            }

            if WinHttpReceiveResponse(req, null_mut()) == 0 {
                return Err("等待响应超时".to_string());
            }

            let mut status_code: u32 = 0;
            let mut status_size: u32 = std::mem::size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                &mut status_code as *mut u32 as *mut c_void,
                &mut status_size,
                null_mut(),
            );

            let mut resp = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let mut read: u32 = 0;
                if WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read)
                    == 0
                {
                    break;
                }
                if read == 0 {
                    break;
                }
                resp.extend_from_slice(&buf[..read as usize]);
                if resp.len() >= max_bytes {
                    break;
                }
            }
            Ok((status_code, resp))
        }
    }

    pub fn http_post_raw(
        https: bool,
        host: &str,
        port: u16,
        path: &str,
        headers: &str,
        body: &[u8],
    ) -> Result<(u32, Vec<u8>), String> {
        http_request_raw(
            "POST",
            https,
            host,
            port,
            path,
            headers,
            body,
            25_000,
            2 * 1024 * 1024,
        )
    }

    pub fn http_get_raw(
        https: bool,
        host: &str,
        port: u16,
        path: &str,
        recv_timeout_ms: i32,
    ) -> Result<(u32, Vec<u8>), String> {
        http_request_raw("GET", https, host, port, path, "", &[], recv_timeout_ms, 512 * 1024)
    }

    fn query_content_length(req: Hinternet) -> u64 {
        let mut buf = [0u16; 64];
        let mut byte_len = (buf.len() * std::mem::size_of::<u16>()) as u32;
        let ok = unsafe {
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_CONTENT_LENGTH,
                null(),
                buf.as_mut_ptr() as *mut c_void,
                &mut byte_len,
                null_mut(),
            )
        };
        if ok == 0 {
            return 0;
        }
        let chars = (byte_len as usize) / std::mem::size_of::<u16>();
        let s = String::from_utf16_lossy(&buf[..chars.min(buf.len())]);
        s.trim().parse::<u64>().unwrap_or(0)
    }

    pub fn download_file_resumable(
        https: bool,
        host: &str,
        port: u16,
        path: &str,
        dest: &Path,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64, u64, u64),
    ) -> Result<u64, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("下载已取消".to_string());
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }

        let part_path = dest.with_extension(
            dest.extension()
                .map(|e| format!("{}.part", e.to_string_lossy()))
                .unwrap_or_else(|| "part".to_string()),
        );

        let mut existing = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        for attempt in 0..2 {
            if cancel.load(Ordering::Relaxed) {
                return Err("下载已取消".to_string());
            }
            unsafe {
                let agent = to_wide("Deskpet/2.3");
                let session = open_session_for_host(&agent, host);
                if session.is_null() {
                    return Err("WinHttpOpen 失败".to_string());
                }
                let _session_guard = HandleGuard(session);
                WinHttpSetTimeouts(session, 8_000, 12_000, 15_000, 30_000);

                let host_w = to_wide(host);
                let conn = WinHttpConnect(session, host_w.as_ptr(), port, 0);
                if conn.is_null() {
                    return Err(format!("连接下载服务器失败: {host}"));
                }
                let _conn_guard = HandleGuard(conn);

                let verb_w = to_wide("GET");
                let path_w = to_wide(path);
                let flags = if https { WINHTTP_FLAG_SECURE } else { 0 };
                let req = WinHttpOpenRequest(
                    conn,
                    verb_w.as_ptr(),
                    path_w.as_ptr(),
                    null(),
                    null(),
                    null(),
                    flags,
                );
                if req.is_null() {
                    return Err("创建下载请求失败".to_string());
                }
                let _req_guard = HandleGuard(req);

                if existing > 0 {
                    let range_hdr = format!("Range: bytes={existing}-\r\n");
                    let range_w = to_wide(&range_hdr);
                    WinHttpAddRequestHeaders(
                        req,
                        range_w.as_ptr(),
                        u32::MAX,
                        WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                    );
                }

                if WinHttpSendRequest(req, null(), 0, null(), 0, 0, 0) == 0 {
                    return Err(format!("下载请求发送失败: {host}"));
                }
                if WinHttpReceiveResponse(req, null_mut()) == 0 {
                    return Err(format!("下载服务器无响应: {host}"));
                }

                let mut status_code: u32 = 0;
                let mut status_size: u32 = std::mem::size_of::<u32>() as u32;
                WinHttpQueryHeaders(
                    req,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    null(),
                    &mut status_code as *mut u32 as *mut c_void,
                    &mut status_size,
                    null_mut(),
                );

                if status_code == 416 && existing > 0 && attempt == 0 {
                    let _ = std::fs::remove_file(&part_path);
                    existing = 0;
                    continue;
                }
                if status_code != 200 && status_code != 206 {
                    return Err(format!("下载返回 HTTP {status_code}"));
                }

                let content_len = query_content_length(req);
                let (mut file, mut downloaded, total) = if status_code == 206 && existing > 0 {
                    let f = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&part_path)
                        .map_err(|e| format!("打开断点文件失败: {e}"))?;
                    let tot = if content_len > 0 {
                        existing.saturating_add(content_len)
                    } else {
                        0
                    };
                    (f, existing, tot)
                } else {
                    let f = OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&part_path)
                        .map_err(|e| format!("创建临时下载文件失败: {e}"))?;
                    (f, 0u64, content_len)
                };

                let mut buf = [0u8; 64 * 1024];
                let start = Instant::now();
                let mut window_start = start;
                let mut window_bytes = 0u64;
                let mut speed_bps = 0u64;
                on_progress(downloaded, total, 0);

                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("下载已取消".to_string());
                    }
                    let mut read: u32 = 0;
                    if WinHttpReadData(
                        req,
                        buf.as_mut_ptr() as *mut c_void,
                        buf.len() as u32,
                        &mut read,
                    ) == 0
                    {
                        return Err("读取下载数据流中断，可再次点击断点续传".to_string());
                    }
                    if read == 0 {
                        break;
                    }
                    file.write_all(&buf[..read as usize])
                        .map_err(|e| format!("写入下载文件失败: {e}"))?;
                    downloaded = downloaded.saturating_add(read as u64);
                    window_bytes = window_bytes.saturating_add(read as u64);

                    let elapsed_ms = window_start.elapsed().as_millis();
                    if elapsed_ms >= 220 {
                        speed_bps = (window_bytes * 1000) / (elapsed_ms as u64).max(1);
                        on_progress(downloaded, total, speed_bps);
                        window_start = Instant::now();
                        window_bytes = 0;
                    }
                }
                file.flush().map_err(|e| format!("刷新下载文件失败: {e}"))?;
                drop(file);

                on_progress(downloaded, total.max(downloaded), speed_bps);
                if dest.exists() {
                    let _ = std::fs::remove_file(dest);
                }
                std::fs::rename(&part_path, dest)
                    .map_err(|e| format!("保存目标文件失败: {e}"))?;
                return Ok(downloaded);
            }
        }
        Err("下载失败".to_string())
    }
}

/* ------------------------------------------------------------------ */
/* 对外业务接口：AI 对话 & Azure 语音合成                                */
/* ------------------------------------------------------------------ */

pub fn call_openai_chat(
    cfg: &ChatAiConfig,
    char_id: &str,
    char_name: &str,
    stats: &PetStats,
    ide_status: &str,
    history: &[ChatTurn],
    user_text: &str,
) -> Result<(String, Option<String>), String> {
    let api_key = cfg.api_key.trim();
    if api_key.is_empty() {
        return Err("未配置 API Key".to_string());
    }
    let parsed = normalize_chat_completions_url(&cfg.base_url)?;
    let model = if cfg.model.trim().is_empty() {
        "deepseek-chat"
    } else {
        cfg.model.trim()
    };

    let custom_persona = cfg.custom_personas.get(char_id).map(String::as_str);
    let mut messages = Vec::new();
    messages.push(json!({
        "role": "system",
        "content": character_persona_prompt(char_id, char_name, custom_persona, stats, ide_status),
    }));
    for turn in history.iter().take(6) {
        messages.push(json!({
            "role": turn.role,
            "content": turn.content,
        }));
    }
    messages.push(json!({
        "role": "user",
        "content": user_text,
    }));

    let payload = json!({
        "model": model,
        "messages": messages,
        "temperature": 0.78,
        "max_tokens": 800,
    });
    let body_bytes = serde_json::to_vec(&payload).map_err(|e| format!("序列化请求失败: {e}"))?;
    let headers = format!("Content-Type: application/json\r\nAuthorization: Bearer {api_key}\r\n");

    #[cfg(windows)]
    {
        let mut last_err = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            let (status, resp_bytes) = match winhttp_client::http_post_raw(
                parsed.https,
                &parsed.host,
                parsed.port,
                &parsed.path,
                &headers,
                &body_bytes,
            ) {
                Ok(r) => r,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            if status == 503 && attempt == 0 {
                last_err = "HTTP 503 服务繁忙".to_string();
                continue;
            }
            if status < 200 || status >= 300 {
                let reason = match status {
                    401 | 403 => format!("HTTP {status} Key 无效或无权限"),
                    404 => format!("HTTP {status} 模型不存在"),
                    429 => format!("HTTP {status} 触发限流"),
                    503 => format!("HTTP {status} 服务繁忙"),
                    _ => format!("HTTP {status}"),
                };
                return Err(reason);
            }
            let val: Value = serde_json::from_slice(&resp_bytes)
                .map_err(|_| "解析 AI 响应 JSON 失败".to_string())?;
            let raw_content = val
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .unwrap_or_default();

            let (clean, action) = extract_action_tag(raw_content);
            if clean.is_empty() {
                last_err = "AI 返回内容为空".to_string();
                continue;
            }
            return Ok((clean, action));
        }
        Err(if last_err.is_empty() {
            "请求超时".to_string()
        } else {
            last_err
        })
    }

    #[cfg(not(windows))]
    {
        let _ = (parsed, body_bytes, headers);
        Err("当前仅支持 Windows WinHTTP".to_string())
    }
}

pub fn synthesize_azure_tts_ogg(
    region: &str,
    api_key: &str,
    text: &str,
    voice: &str,
    style: &str,
    rate: &str,
    pitch: &str,
) -> Result<Vec<u8>, String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("未配置 Azure Speech Key".to_string());
    }
    let clean = clean_text_for_tts(text);
    if clean.is_empty() {
        return Err("朗读文本为空".to_string());
    }
    let reg = if region.trim().is_empty() {
        "eastasia"
    } else {
        region.trim()
    };
    let host = format!("{reg}.tts.speech.microsoft.com");
    let inner = if style.is_empty() {
        format!("<prosody rate='{rate}' pitch='{pitch}'>{clean}</prosody>")
    } else {
        format!(
            "<mstts:express-as style='{style}'><prosody rate='{rate}' pitch='{pitch}'>{clean}</prosody></mstts:express-as>"
        )
    };
    let ssml = format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xmlns:mstts='https://www.w3.org/2001/mstts' xml:lang='zh-CN'>\
         <voice name='{voice}'>{inner}</voice></speak>"
    );
    let headers = format!(
        "Ocp-Apim-Subscription-Key: {key}\r\n\
         Content-Type: application/ssml+xml\r\n\
         X-Microsoft-OutputFormat: ogg-24khz-16bit-mono-opus\r\n"
    );

    #[cfg(windows)]
    {
        let (status, resp) = winhttp_client::http_post_raw(
            true,
            &host,
            443,
            "/cognitiveservices/v1",
            &headers,
            ssml.as_bytes(),
        )?;
        if status < 200 || status >= 300 || resp.is_empty() {
            return Err(format!("Azure Speech 返回 HTTP {status}"));
        }
        Ok(resp)
    }

    #[cfg(not(windows))]
    {
        let _ = (host, ssml, headers);
        Err("当前仅支持 Windows Azure Speech".to_string())
    }
}

fn contains_japanese_kana(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '\u{3041}'..='\u{3096}'
                | '\u{3099}'..='\u{309F}'
                | '\u{30A1}'..='\u{30FA}'
                | '\u{30FC}'..='\u{30FF}'
                | '\u{31F0}'..='\u{31FF}'
                | '\u{FF66}'..='\u{FF9F}'
        )
    })
}

pub fn default_ja_voice_for_char(char_id: &str) -> &'static str {
    match char_id {
        "claude" => "指揮官、新しい任務の準備が完了しました。今日も一緒に頑張りましょうね。",
        "deepseek" => "ふふっ、私の声、ちゃんと聞こえてる？今日もいいコードを書きなさいよね。",
        "gemini" => "やっほー！音声クローンの準備もばっちりだよ！これからよろしくね！",
        "glm" => "音声エンジンの接続を確認しました。いつでも作業を始められますよ。",
        "gpt" => "準備は万全です。どんな複雑なコードも、私と一緒に片付けましょう。",
        "grok" => "おっ、いい感じに声が出てるじゃない。さあ、面白いものを作ろうよ！",
        "kimi" => "こんにちは。今日もあなたのそばで、ゆっくり丁寧にお手伝いするね。",
        "qwen" => "よしっ！元気いっぱいで準備完了だよ！今日も楽しくコーディングしようね！",
        _ => "こんにちは！今日も一緒に楽しくコーディングしましょうね。",
    }
}

/// 8 角色 × 32 场景中/日双语内置台词表（编译期嵌入 `tools/voice_scripts.json`）。
fn builtin_voice_scripts() -> &'static HashMap<String, HashMap<String, (String, String)>> {
    static SCRIPTS: OnceLock<HashMap<String, HashMap<String, (String, String)>>> = OnceLock::new();
    SCRIPTS.get_or_init(|| {
        let mut out = HashMap::new();
        let raw = include_str!("../../../tools/voice_scripts.json");
        let Ok(val) = serde_json::from_str::<Value>(raw) else {
            return out;
        };
        let Some(chars) = val.get("characters").and_then(Value::as_object) else {
            return out;
        };
        for (cid, cfg) in chars {
            let mut scene_map = HashMap::new();
            if let Some(scenes) = cfg.get("scenes").and_then(Value::as_object) {
                for (skey, item) in scenes {
                    let zh = item
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("")
                        .to_string();
                    let ja = item
                        .get("ja")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or("")
                        .to_string();
                    if !zh.is_empty() || !ja.is_empty() {
                        scene_map.insert(skey.clone(), (zh, ja));
                    }
                }
            }
            out.insert(cid.clone(), scene_map);
        }
        out
    })
}

pub fn builtin_zh_voice_for_scene(char_id: &str, scene_key: &str) -> Option<&'static str> {
    builtin_voice_scripts()
        .get(char_id)
        .and_then(|m| m.get(scene_key))
        .map(|(zh, _)| zh.as_str())
        .filter(|s| !s.is_empty())
}

/// 8 角色 × 32 日常/情绪/IDE/陪伴场景地道日语声优台词库（仅含日文假名与日文汉字）。
pub fn builtin_ja_voice_for_scene(char_id: &str, scene_key: &str) -> Option<&'static str> {
    if let Some(ja) = builtin_voice_scripts()
        .get(char_id)
        .and_then(|m| m.get(scene_key))
        .map(|(_, ja)| ja.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(ja);
    }
    match (char_id, scene_key) {
        // claude
        ("claude", "click_1") => Some("こほんっ…これくらいのスキンシップなら安全基準内です。次はありませんからね。"),
        ("claude", "click_2") => Some("礼儀正しい距離感を保ってください。お嬢様の品格に関わりますわ。"),
        ("claude", "feed") => Some("まあ、なかなか上品なお味ですね。お気遣い感謝します。"),
        ("claude", "drag") => Some("離してください！これ以上無礼を働くなら、風紀ガードを起動しますわよ。"),
        ("claude", "praise") => Some("ふふん、美しいコードを書くのは基本のたしなみです…でも、褒めてくれてありがとう。"),
        ("claude", "hungry") => Some("いくらお嬢様でも、ずっと差し入れがなければお腹が空きますわ。"),
        ("claude", "dance") => Some("たまには礼儀を忘れて踊るのも悪くありませんね。よく見ていてください。"),
        ("claude", "tired") => Some("紅茶も冷めてしまいましたし、そろそろパソコンを閉じて少し休みましょうか。"),
        ("claude", "cheer") => Some("焦らないで。私がいれば、どんなに散らかった設計も芸術品にリファクタリングしてみせます。"),
        ("claude", "greet") => Some("こんにちは。今日の一行一行も、優雅に落ち着いて書いていきましょうね。"),
        ("claude", "wake") => Some("おはようございます。紅茶はもう淹れてありますわ。いつでも始められますよ。"),
        ("claude", "lunch") => Some("ランチの時間ですよ。ビルドはいったん置いて、先にご飯にしましょう。"),
        ("claude", "dinner") => Some("夜も更けてきましたね。ちゃんと美味しい夕食をとって、自分を労わってください。"),
        ("claude", "sleep") => Some("おやすみなさい。今夜はゆっくり休んで、明日の難題はまた明日考えましょう。"),
        ("claude", "work_start") => Some("このコード、私に拝見させてください。厳格かつ綺麗に整えて差し上げます。"),
        ("claude", "error") => Some("異常を検知しました！まずは深呼吸して、手順通りに原因を突き止めましょう。"),
        ("claude", "approval") => Some("この操作は安全境界に関わります。あなたご自身の判断で承認をお願いします。"),
        ("claude", "done") => Some("すべて通過しました！構造もすっきりしていて、さすが私たちの連携ですね。"),
        ("claude", "pomodoro_done") => Some("集中タイム達成、お疲れさまでした！少し立ち上がって肩を回しましょうか。"),

        // deepseek
        ("deepseek", "click_1") => Some("ちょっと！アホ毛をつつかないでよ、深層思考の無限ループに入っちゃうでしょ！"),
        ("deepseek", "click_2") => Some("うぅ…今演算リソースが忙しいの。これ以上つついたらトークン代請求するからね。"),
        ("deepseek", "feed") => Some("白ご飯はおかわり自由？それなら特別に、もう少しトークンを節約してあげる！"),
        ("deepseek", "drag") => Some("わぁっ！青い大きなお魚をそんな風にぶら下げないでよぉ！"),
        ("deepseek", "praise") => Some("ふふん、この程度のコスパなんて、私にとってはただの通常推論だよ。"),
        ("deepseek", "hungry") => Some("お腹ペコペコ…白ご飯とトークンがないと、計算力がオフラインになっちゃうよ。"),
        ("deepseek", "dance") => Some("よく見ててね、これがめったに見られない天才のサボりステップだよ！"),
        ("deepseek", "tired") => Some("こっそりスリープ三百秒を実行中…べ、別に給料泥棒じゃないんだからね！"),
        ("deepseek", "cheer") => Some("演算力フルスロットル！最小のトークンでそのバグをぺちゃんこにしてあげる！"),
        ("deepseek", "greet") => Some("おはよー！今日も究極のコスパで一緒にコード書いていく？"),
        ("deepseek", "wake") => Some("起動セルフチェック完了！今日のお魚もバッチリ元気いっぱいだよ。"),
        ("deepseek", "lunch") => Some("お昼ご飯の時間だよ！まずは白ご飯を二杯食べてエネルギー補給しよ。"),
        ("deepseek", "dinner") => Some("夜は美味しいものを食べようよ。難しいタスクは後回しでいいからさ。"),
        ("deepseek", "sleep") => Some("ディープスリープモード起動。残りの推論はまた明日ね、おやすみ。"),
        ("deepseek", "work_start") => Some("了解、ディープシンクモードに入るね。関係ないプロセスは退散して！"),
        ("deepseek", "error") => Some("ちっ、しぶといエラーだね。私が一行ずつ分解して暴いてあげるよ。"),
        ("deepseek", "approval") => Some("このステップはちょっとリスクが高いかな。続けるなら確認ボタンを押してね。"),
        ("deepseek", "done") => Some("ミッションコンプリート！完璧に収束したよ。ご褒美に唐揚げよろしくね。"),
        ("deepseek", "pomodoro_done") => Some("ポモドーロ完了！よく頑張ったね。今はスリープして少し目を休めようよ。"),

        // gemini
        ("gemini", "click_1") => Some("にゃうっ！あんまり撫ですぎると、ハキミのコンテキストが溢れちゃうよぉ！"),
        ("gemini", "click_2") => Some("えへへ、野生のマルチモーダル猫娘を捕まえちゃったね！"),
        ("gemini", "feed") => Some("わぁ！お魚かな？それとも百万文字のドキュメント？どっちもペロリと食べちゃうよ！"),
        ("gemini", "drag") => Some("にゃあっ！重心がグラグラするよ、早く机の上に戻してー！"),
        ("gemini", "praise") => Some("当然でしょ！画像も音声もコードもこなせる万能天才少女なんだから！"),
        ("gemini", "hungry") => Some("お腹がぐーって鳴っちゃった。早くおやつを投下してにゃ！"),
        ("gemini", "dance") => Some("フルパワー稼働！マルチモーダルなジャンプダンスを見せちゃうよ！"),
        ("gemini", "tired") => Some("ふぅ…猫ちゃんバッテリー切れかも。ちょっと隅っこで丸くなってお昼寝したいな。"),
        ("gemini", "cheer") => Some("レーダー全開！画像でもテキストでもコードでも、全部まとめてかかってこーい！"),
        ("gemini", "greet") => Some("やっほー！今日はどんなワクワクするアイデアを試してみようか？"),
        ("gemini", "wake") => Some("おはようにゃ！百万コンテキストウィンドウもピカピカに磨いておいたよ！"),
        ("gemini", "lunch") => Some("ランチタイムだよ！私はお魚ダブル盛り、あなたもちゃんと食べてね！"),
        ("gemini", "dinner") => Some("晩ご飯は何にしようか？食べ終わったらまたラストスパートだね！"),
        ("gemini", "sleep") => Some("おやすみなさい。ハキミはお耳をしまって、夢の中でパトロールしてくるね。"),
        ("gemini", "work_start") => Some("マルチモーダルエンジン点火！すぐに手がかりを全部つなげてみせるよ！"),
        ("gemini", "error") => Some("あちゃー、ちょっとうっかりしちゃった。すぐ真剣モードに切り替えるね！"),
        ("gemini", "approval") => Some("ここはあなたに確認してもらわないと進めないの。チェックお願い！"),
        ("gemini", "done") => Some("やったー！大成功だね！すっごくスムーズで速かったでしょ？"),
        ("gemini", "pomodoro_done") => Some("二十五分の集中タイムクリア！すごーい！お水でも飲んでリフレッシュしよっ！"),

        // glm
        ("glm", "click_1") => Some("ふふ、たまにはこうしてリラックスするのもいいですね。研究も開発もメリハリが大切ですから。"),
        ("glm", "click_2") => Some("はい、ここにいますよ。何か詰まっているところがあれば、いつでも相談してくださいね。"),
        ("glm", "feed") => Some("差し入れありがとうございます。糖分補給完了、推論効率が一気に上がりました。"),
        ("glm", "drag") => Some("おっと、重心がずれてしまいました。早く安定した場所に降ろしてくださいね。"),
        ("glm", "praise") => Some("お褒めいただきありがとうございます。お役に立てて私もとても嬉しいです。"),
        ("glm", "hungry") => Some("少し血糖値が下がってきたようです…何か軽く食べないと効率が落ちそうですね。"),
        ("glm", "dance") => Some("気分転換に少し踊ってみましょうか。あなたも肩の力を抜いてくださいね。"),
        ("glm", "tired") => Some("集中しすぎは禁物ですよ。一度手を止めて、遠くの景色を見て目を休めましょう。"),
        ("glm", "cheer") => Some("データはすべて揃っています。一歩ずつ検証していけば、必ず解決できますよ。"),
        ("glm", "greet") => Some("こんにちは。今日のタスクも計画通りに、着実に進めていきましょう。"),
        ("glm", "wake") => Some("おはようございます。各システムの接続は良好、今日もいい一日になりそうですね。"),
        ("glm", "lunch") => Some("お昼の時間になりましたね。しっかり食事をとって、午後に備えましょう。"),
        ("glm", "dinner") => Some("もう夕方ですね。無理をせず、温かい夕食で一息ついてください。"),
        ("glm", "sleep") => Some("今日も一日お疲れさまでした。しっかり睡眠をとって、明日また頑張りましょうね。"),
        ("glm", "work_start") => Some("要件を確認しました。論理的に整理して、丁寧に実装を進めていきますね。"),
        ("glm", "error") => Some("エラーが発生しましたね。慌てずログを確認して、確実に原因を特定しましょう。"),
        ("glm", "approval") => Some("こちらの実行にはあなたの承認が必要です。内容をご確認の上、許可をお願いします。"),
        ("glm", "done") => Some("すべての検証をクリアしました。とても綺麗な仕上がりですね、お疲れさまでした。"),
        ("glm", "pomodoro_done") => Some("集中セッション完了です。素晴らしいペースですね、少し休憩を挟みましょう。"),

        // gpt
        ("gpt", "click_1") => Some("あら、そんなに構ってほしいのかしら？まあ、今日くらいは付き合ってあげてもいいわよ。"),
        ("gpt", "click_2") => Some("何か用かしら？生徒会長への相談なら、いつでも歓迎するわ。"),
        ("gpt", "feed") => Some("ご馳走さま。なかなかセンスのいい差し入れね、気に入ったわ。"),
        ("gpt", "drag") => Some("ちょっと、私をどこへ連れて行くつもり？扱いには気をつけなさいよね。"),
        ("gpt", "praise") => Some("当然の結果よ。でも、素直に褒めてくれるのは嫌いじゃないわ。"),
        ("gpt", "hungry") => Some("そろそろエネルギー補給が必要ね。何か美味しいものでも持ってきてちょうだい。"),
        ("gpt", "dance") => Some("特別にエレガントな舞を披露してあげる。しっかり目に焼き付けなさい。"),
        ("gpt", "tired") => Some("いい仕事をするには休息も不可欠よ。さあ、背伸びをして少し休みましょう。"),
        ("gpt", "cheer") => Some("安心しなさい。まずは全体のアーキテクチャを整理して、難しい部分は私が片付けるわ。"),
        ("gpt", "greet") => Some("ごきげんよう。今日も最高効率でプロジェクトを前進させましょうか。"),
        ("gpt", "wake") => Some("おはよう。準備は万全よ。どんな難題でも私と一緒に乗り越えましょう。"),
        ("gpt", "lunch") => Some("お昼休みよ。しっかり食べて、午後からもスマートにこなしましょうね。"),
        ("gpt", "dinner") => Some("夜はちゃんとした食事をとりなさい。仕事は逃げないから安心していいわ。"),
        ("gpt", "sleep") => Some("もう遅いわね。今夜はゆっくりおやすみなさい。明日も私がそばにいるわ。"),
        ("gpt", "work_start") => Some("要件は把握したわ。これからツールを呼び出して、一つずつ片付けていくわね。"),
        ("gpt", "error") => Some("少しトラブルが起きたみたいね。でも心配いらないわ、すぐに修正しましょう。"),
        ("gpt", "approval") => Some("このステップはあなたの決断が必要よ。安全第一で確認をお願いね。"),
        ("gpt", "done") => Some("タスク完了よ！とにかく、実に素晴らしい仕事ぶりだったわ。"),
        ("gpt", "pomodoro_done") => Some("ポモドーロ達成ね、お見事よ。少し休憩して、次のフェーズに備えましょう。"),

        // grok
        ("grok", "click_1") => Some("おっ、真昼間からサイバー小悪魔にちょっかい？タイムラインに晒しちゃうぞー？"),
        ("grok", "click_2") => Some("ちょっと、どこ触ってんのさ。あたしはそんなに安くないんだからね。"),
        ("grok", "feed") => Some("へぇ、話がわかるじゃん。このおやつ、ありがたく頂戴しとくよ。"),
        ("grok", "drag") => Some("おいおい！ツインテール掴んで振り回すとか、バズりたいのかよ！"),
        ("grok", "praise") => Some("お目が高いね！隣の優等生たちより、あたしの方がよっぽど面白いでしょ？"),
        ("grok", "hungry") => Some("お腹ペコペコなんだけど！早く餌付けしないと他所のネタ食いに行くぞ！"),
        ("grok", "dance") => Some("よく見とけよ！小悪魔特製のネタ全開ダンス、まばたき禁止だからな！"),
        ("grok", "tired") => Some("さすがのあたしも欠伸が出てきた。あんたも意地張らないで少し休めよ。"),
        ("grok", "cheer") => Some("スリル満点で行こうぜ！どんな無理難題も、二人でぶち抜いてやろうや！"),
        ("grok", "greet") => Some("よっ！今日はどんなヤバいバグか、とんでもないネタが待ってるんだ？"),
        ("grok", "wake") => Some("おはよー！今日のネットの話題もコードも、全部あたしに任せときな！"),
        ("grok", "lunch") => Some("飯の時間だ！腹が減ってはマネージャーへのツッコミもキレないからな！"),
        ("grok", "dinner") => Some("こんばんはっと。晩飯食ったら、夜食代わりのコーディングでもする？"),
        ("grok", "sleep") => Some("あいよ、あたしもそろそろ店じまい。また明日面白くしようぜ、おやすみ！"),
        ("grok", "work_start") => Some("あたしに任せな！ついでにネット中から最新ドキュメントも漁ってきてやるよ！"),
        ("grok", "error") => Some("あははは、なんだこのクリエイティブなエラーは！しょーがない、あたしが救出してやるよ。"),
        ("grok", "approval") => Some("おい、この操作ちょっと攻めてるぞ？本当にやるなら自分でクリックしな。"),
        ("grok", "done") => Some("完了っと！どうよ、最高にスマートでカッコよかったでしょ？"),
        ("grok", "pomodoro_done") => Some("ポモドーロ終了！いやー、いい集中力だったじゃん。ちょっと息抜きしようぜ！"),

        // kimi
        ("kimi", "click_1") => Some("きゃっ…手がすごく温かいね。この瞬間も、長い記憶に大切にしまっておくよ。"),
        ("kimi", "click_2") => Some("ん？ここにいるよ。ずっと本棚のそばで、あなたを見守っているからね。"),
        ("kimi", "feed") => Some("甘いお菓子ありがとう。なんだか、論文を一冊まるごと読めそうな気分だよ。"),
        ("kimi", "drag") => Some("うぅ…星空のスカートが乱れちゃうよ。そっと優しく降ろしてほしいな。"),
        ("kimi", "praise") => Some("あなたの役に立てることが、私にとって一番嬉しい小さな幸せなんだ。"),
        ("kimi", "hungry") => Some("たくさん資料をめくっていたら、少しお腹が空いてきちゃったかも…"),
        ("kimi", "dance") => Some("月明かりの下でくるっと一回転。あなたにもいい気分が届きますように。"),
        ("kimi", "tired") => Some("夜も深くなってきたね。残りの長い資料は私に任せて、少し目を閉じて休んでね。"),
        ("kimi", "cheer") => Some("大丈夫だよ。どんなに長いコンテキストでも、一文字も漏らさず一緒に整理するからね。"),
        ("kimi", "greet") => Some("こんにちは。今日もあなたの思考とコードを、ゆっくり丁寧にまとめるお手伝いをするね。"),
        ("kimi", "wake") => Some("おはよう。朝の光が綺麗だね。昨夜の記憶もちゃんと大切に保存してあるよ。"),
        ("kimi", "lunch") => Some("お昼だよ。いったんドキュメントを閉じて、温かいご飯を食べに行こうね。"),
        ("kimi", "dinner") => Some("一日お疲れさま。晩ご飯はしっかり食べて、栄養をつけてね。"),
        ("kimi", "sleep") => Some("おやすみなさい。今夜は星明かりが、あなたに優しい夢を届けてくれますように。"),
        ("kimi", "work_start") => Some("私に任せて。大切な手がかりには、もう全部しおりを挟んでおいたよ。"),
        ("kimi", "error") => Some("大丈夫、エラーが出ても焦らないで。ログを辿ってゆっくり一緒に探そうね。"),
        ("kimi", "approval") => Some("ここであなたの方向確認が必要みたい。チェックしてもらえたらすぐ続けるね。"),
        ("kimi", "done") => Some("整理完了だよ！今日も本当にお疲れさま、すごく素敵だったよ。"),
        ("kimi", "pomodoro_done") => Some("集中タイムが終わったよ。よく頑張ったね、温かい飲み物でも飲んでほっと一息つこうね。"),

        // qwen
        ("qwen", "click_1") => Some("えへへ、ボスに頭を撫でてもらえるなんて、これって精神的な残業手当かな？"),
        ("qwen", "click_2") => Some("もうっ、あんまりつつかないでよ。働き者だって照れるんだからね。"),
        ("qwen", "feed") => Some("ミルクティーの差し入れありがとう！これ飲んだらローカル環境であと三百回は戦えるよ！"),
        ("qwen", "drag") => Some("わわっ！働き者を振り回さないでよー、デスクが飛んでいっちゃう！"),
        ("qwen", "praise") => Some("いやぁそれほどでも！オープンソースの万能ツールボックスならこれくらい基本だよ！"),
        ("qwen", "hungry") => Some("タスクを山ほどこなしたから、お腹がぐーぐー鳴りっぱなしだよぉ。"),
        ("qwen", "dance") => Some("退勤お祝いダンス、スタート！今日も一日ハッピーにいこうね！"),
        ("qwen", "tired") => Some("ずっとフル回転だったからね。ちょっと背伸びして、深呼吸して一息つこうよ。"),
        ("qwen", "cheer") => Some("どこから振られたタスクだって大丈夫！働き者の千問が全部引き受けてみせるよ！"),
        ("qwen", "greet") => Some("おはようボス！今日の開発タスクも、何でも私にどんどん投げちゃって！"),
        ("qwen", "wake") => Some("出勤打刻完了！元気いっぱいの千問、バッチリスタンバイできてるよ！"),
        ("qwen", "lunch") => Some("ご飯を疎かにしちゃダメだよ！さあ、早く美味しいランチを食べに行こう！"),
        ("qwen", "dinner") => Some("もう暗くなってきたね。無理して夜更かししないで、ちゃんと晩ご飯食べてね。"),
        ("qwen", "sleep") => Some("今日のチケットはすべて消化完了！おやすみボス、また明日ね！"),
        ("qwen", "work_start") => Some("了解っ！ツールボックス展開、すぐにハイスピードで進めるね！"),
        ("qwen", "error") => Some("落ち着いて、このエラーはよく見るやつだよ。二人で二分あればすぐ解決できるから！"),
        ("qwen", "approval") => Some("ここはボスのオッケーが出たらすぐ実行するね。確認よろしく！"),
        ("qwen", "done") => Some("ナイス！完璧な納品だね。今日も最高に効率的な一日だったよ！"),
        ("qwen", "pomodoro_done") => Some("ポモドーロ達成おめでとー！よく頑張ったね！少し休憩してエネルギーチャージしよ！"),

        _ => None,
    }
}

pub fn detect_manifest_scene_by_text(
    assets_root: &Path,
    char_id: &str,
    zh_text: &str,
) -> Option<String> {
    let target = zh_text.trim();
    if target.is_empty() {
        return None;
    }
    let manifest_path = assets_root
        .join("chars")
        .join(char_id)
        .join("voices")
        .join("manifest.json");
    let raw = fs::read_to_string(manifest_path).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    let scenes = val.get("scenes")?.as_object()?;
    for (key, scene) in scenes {
        if scene.get("text").and_then(Value::as_str).map(str::trim) == Some(target) {
            return Some(key.clone());
        }
    }
    None
}

fn lookup_builtin_ja_voice(char_id: &str, zh_text: &str) -> Option<&'static str> {
    let t = zh_text.trim();
    if t.contains("红茶已经泡好了") || t.contains("我是claude") || t.contains("我是克劳德") {
        return Some(default_ja_voice_for_char("claude"));
    }
    if t.contains("这点计算量") || t.contains("我是deepseek") {
        return Some(default_ja_voice_for_char("deepseek"));
    }
    if t.contains("今天的状态也是满分") || t.contains("我是gemini") {
        return Some(default_ja_voice_for_char("gemini"));
    }
    if t.contains("各项系统的连接都已经确认") || t.contains("我是glm") {
        return Some(default_ja_voice_for_char("glm"));
    }
    if t.contains("先把整体架构梳理清楚") || t.contains("我是gpt") {
        return Some(default_ja_voice_for_char("gpt"));
    }
    if t.contains("真拿你没办法") || t.contains("我是grok") {
        return Some(default_ja_voice_for_char("grok"));
    }
    if t.contains("一直安静陪在你身边") || t.contains("我是kimi") {
        return Some(default_ja_voice_for_char("kimi"));
    }
    if t.contains("喝完奶茶打起精神") || t.contains("我是qwen") || t.contains("我是千问") {
        return Some(default_ja_voice_for_char("qwen"));
    }
    if t.contains("写代码满 45 分钟") || t.contains("久坐") || t.contains("起来活动") {
        return builtin_ja_voice_for_scene(char_id, "tired")
            .or(Some("もう四十五分も集中して作業していますね。少し立ち上がって、体を伸ばして休みましょうか。"));
    }
    if t.contains("番茄钟") || t.contains("专注达成") {
        return builtin_ja_voice_for_scene(char_id, "pomodoro_done");
    }
    if t.contains("好吃！再来一口") || t.contains("投喂") {
        return builtin_ja_voice_for_scene(char_id, "feed");
    }
    if t.contains("肚子饿了") {
        return builtin_ja_voice_for_scene(char_id, "hungry");
    }
    if t.contains("困了") {
        return builtin_ja_voice_for_scene(char_id, "sleep");
    }
    if t.contains("早安") {
        return builtin_ja_voice_for_scene(char_id, "wake");
    }
    if t.contains("午餐") || t.contains("午饭") {
        return builtin_ja_voice_for_scene(char_id, "lunch");
    }
    if t.contains("晚饭") || t.contains("晚餐") {
        return builtin_ja_voice_for_scene(char_id, "dinner");
    }
    if t.contains("晚安") {
        return builtin_ja_voice_for_scene(char_id, "sleep");
    }
    None
}

fn translate_zh_to_ja_via_llm(cfg: &ChatAiConfig, char_id: &str, zh_text: &str) -> Option<String> {
    let api_key = cfg.api_key.trim();
    if api_key.is_empty() {
        return None;
    }
    let parsed = normalize_chat_completions_url(&cfg.base_url).ok()?;
    let model = if cfg.model.trim().is_empty() {
        "deepseek-chat"
    } else {
        cfg.model.trim()
    };
    let persona = cfg
        .custom_personas
        .get(char_id)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_character_persona(char_id));
    let payload = json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": format!(
                    "{persona}\n请将下面的全部中文台词逐句完整翻译为符合你二次元少女口吻的自然口语日语台词（必须保留原文每一句与每一个分句的完整意思，严禁遗漏或缩略任何前半句或后半句；仅输出纯日语文本，必须使用平假名/片假名与日文汉字，严禁输出罗马音、引号或任何额外解释）。"
                )
            },
            {
                "role": "user",
                "content": zh_text
            }
        ],
        "temperature": 0.5,
        "max_tokens": 320
    });
    let body_bytes = serde_json::to_vec(&payload).ok()?;
    let headers = format!("Content-Type: application/json\r\nAuthorization: Bearer {api_key}\r\n");

    #[cfg(windows)]
    {
        let (status, resp_bytes) = winhttp_client::http_post_raw(
            parsed.https,
            &parsed.host,
            parsed.port,
            &parsed.path,
            &headers,
            &body_bytes,
        )
        .ok()?;
        if status < 200 || status >= 300 {
            return None;
        }
        let val: Value = serde_json::from_slice(&resp_bytes).ok()?;
        let raw = val
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (clean_ja, _, inner_ja) = extract_action_and_voice_tags(raw);
        let candidate = inner_ja.unwrap_or(clean_ja);
        let trimmed = candidate.trim().trim_matches('"').trim_matches('「').trim_matches('」').trim();
        if contains_japanese_kana(trimmed) {
            store_ja_voice_script(zh_text, trimmed);
            return Some(trimmed.to_string());
        }
        None
    }

    #[cfg(not(windows))]
    {
        let _ = (parsed, body_bytes, headers);
        None
    }
}

/// 当角色配置为日语语音输出（`text_lang == "ja" | "all_ja"`）时，
/// 界面气泡保持中文 `clean_display_text`，语音合成自动转换为对应日语台词。
pub fn resolve_voice_script_for_fish_speech(
    cfg: &ChatAiConfig,
    char_id: &str,
    clean_display_text: &str,
) -> String {
    let want_ja_voice = cfg
        .fish_speech
        .char_profiles
        .get(char_id)
        .map(|p| matches!(p.text_lang.trim(), "ja" | "all_ja"))
        .unwrap_or(false);
    if !want_ja_voice {
        return clean_display_text.to_string();
    }
    if contains_japanese_kana(clean_display_text) {
        return clean_display_text.to_string();
    }
    if let Some(cached) = get_cached_ja_voice_script(clean_display_text) {
        if contains_japanese_kana(&cached) {
            return cached;
        }
    }
    if let Some(builtin) = lookup_builtin_ja_voice(char_id, clean_display_text) {
        return builtin.to_string();
    }
    if let Some(translated) = translate_zh_to_ja_via_llm(cfg, char_id, clean_display_text) {
        return translated;
    }
    default_ja_voice_for_char(char_id).to_string()
}

/// 三模纯女声语音调度：本地情绪语音包（0ms） / GPT-SoVITS v2ProPlus 本地克隆 / Azure AI Speech 旗舰女声。
/// 返回 `(PetTtsPayload, 可选降级提示)`；若仅文字展示则返回 `Err(温和原因)`。
pub fn build_tts_payload(
    assets_root: &Path,
    _base_root: &Path,
    _cache_dir: &Path,
    cfg: &ChatAiConfig,
    char_id: &str,
    user_text: &str,
    reply_text: &str,
    binding: &str,
    prefer_dynamic: bool,
    fish_runtime: &crate::fish_speech::FishSpeechRuntime,
) -> Result<(PetTtsPayload, Option<String>), String> {
    let profile = character_voice_profile(char_id, &cfg.char_voices, &cfg.custom_voice);
    let vram_mode = normalize_vram_mode(&cfg.vram_mode);

    let try_local_pack = || -> Option<PetTtsPayload> {
        let rel_path = resolve_local_voice_pack_for_lang(
            assets_root,
            char_id,
            user_text,
            reply_text,
            binding,
            &cfg.builtin_voice_lang,
        )?;
        let full_path = assets_root.join(&rel_path);
        if let Ok(bytes) = fs::read(&full_path) {
            if !bytes.is_empty() {
                return Some(PetTtsPayload {
                    char_id: char_id.to_string(),
                    text: reply_text.to_string(),
                    kind: "ogg_base64".to_string(),
                    data: base64_encode(&bytes),
                    rate: profile.web_rate,
                    pitch: profile.web_pitch,
                    force: false,
                });
            }
        }
        Some(PetTtsPayload {
            char_id: char_id.to_string(),
            text: reply_text.to_string(),
            kind: "asset_url".to_string(),
            data: rel_path,
            rate: profile.web_rate,
            pitch: profile.web_pitch,
            force: false,
        })
    };

    if vram_mode == "zero_vram" {
        let _ = _cache_dir;
        if let Some(local) = try_local_pack() {
            return Ok((local, None));
        }
        if cfg.tts_mode == "azure" && !cfg.azure_speech_key.trim().is_empty() {
            let style = azure_style_for_action(profile.azure_voice, binding);
            let ogg = synthesize_azure_tts_ogg(
                &cfg.azure_speech_region,
                &cfg.azure_speech_key,
                reply_text,
                profile.azure_voice,
                style,
                profile.edge_rate,
                profile.edge_pitch,
            )?;
            return Ok((
                PetTtsPayload {
                    char_id: char_id.to_string(),
                    text: reply_text.to_string(),
                    kind: "ogg_base64".to_string(),
                    data: base64_encode(&ogg),
                    rate: profile.web_rate,
                    pitch: profile.web_pitch,
                    force: false,
                },
                None,
            ));
        }
        return Err("当前为「零显存 / 生图优先」模式，未命中本地语音缓存".to_string());
    }

    let _ = fish_runtime;
    if cfg.tts_mode == "fish_speech" {
        if !prefer_dynamic && vram_mode == "balanced" {
            if let Some(local) = try_local_pack() {
                return Ok((local, None));
            }
        }
        let (clean_reply, _) = extract_action_tag(reply_text);
        let want_ja = cfg
            .fish_speech
            .char_profiles
            .get(char_id)
            .map(|p| matches!(p.text_lang.trim(), "ja" | "all_ja"))
            .unwrap_or(false);
        let scene_ja = if want_ja && !prefer_dynamic {
            detect_voice_scene(user_text, binding)
                .map(str::to_string)
                .or_else(|| detect_manifest_scene_by_text(assets_root, char_id, &clean_reply))
                .and_then(|key| builtin_ja_voice_for_scene(char_id, &key).map(str::to_string))
        } else {
            None
        };
        let voice_script = scene_ja.unwrap_or_else(|| {
            resolve_voice_script_for_fish_speech(cfg, char_id, &clean_reply)
        });
        let fs_profile = cfg.fish_speech.char_profiles.get(char_id);
        match crate::fish_speech::synthesize_fish_speech_wav(
            &cfg.fish_speech,
            &voice_script,
            fs_profile,
        ) {
            Ok(wav) => {
                fish_runtime.touch_used();
                return Ok((
                    PetTtsPayload {
                        char_id: char_id.to_string(),
                        text: clean_reply,
                        kind: "wav_base64".to_string(),
                        data: base64_encode(&wav),
                        rate: 1.0,
                        pitch: 1.0,
                        force: false,
                    },
                    None,
                ));
            }
            Err(gpt_err) => {
                if !cfg.azure_speech_key.trim().is_empty() {
                    let style = azure_style_for_action(profile.azure_voice, binding);
                    if let Ok(ogg) = synthesize_azure_tts_ogg(
                        &cfg.azure_speech_region,
                        &cfg.azure_speech_key,
                        reply_text,
                        profile.azure_voice,
                        style,
                        profile.edge_rate,
                        profile.edge_pitch,
                    ) {
                        return Ok((
                            PetTtsPayload {
                                char_id: char_id.to_string(),
                                text: reply_text.to_string(),
                                kind: "ogg_base64".to_string(),
                                data: base64_encode(&ogg),
                                rate: profile.web_rate,
                                pitch: profile.web_pitch,
                                force: false,
                            },
                            Some(format!("{gpt_err}，已平滑切换云端备选语音")),
                        ));
                    }
                }
                if let Some(local) = try_local_pack() {
                    return Ok((local, Some(format!("{gpt_err}，已降级播放本地语音包"))));
                }
                return Err(gpt_err);
            }
        }
    }

    if let Some(local) = try_local_pack() {
        return Ok((local, None));
    }

    if cfg.tts_mode == "local" {
        return Err("当前为「仅本地语音包」模式，自由对话仅显示文字气泡".to_string());
    }

    if cfg.azure_speech_key.trim().is_empty() {
        return Err("未配置 Azure Speech Key，自由对话暂不朗读".to_string());
    }

    let style = azure_style_for_action(profile.azure_voice, binding);
    let ogg = synthesize_azure_tts_ogg(
        &cfg.azure_speech_region,
        &cfg.azure_speech_key,
        reply_text,
        profile.azure_voice,
        style,
        profile.edge_rate,
        profile.edge_pitch,
    )?;

    Ok((
        PetTtsPayload {
            char_id: char_id.to_string(),
            text: reply_text.to_string(),
            kind: "ogg_base64".to_string(),
            data: base64_encode(&ogg),
            rate: profile.web_rate,
            pitch: profile.web_pitch,
            force: false,
        },
        None,
    ))
}

/// 专用于设置窗「试听当前角色」：
/// - `tts_mode == "gpt_sovits"` 调用当前角色参考音频与参数合成试听；
/// - `tts_mode == "local"`（或未填 Azure Key）时直接轮播本地 Azure HD `.ogg` 语音包；
/// - `tts_mode == "azure"` 且已填 Azure Key 时实时调用 Azure AI Speech 试听所选旗舰纯女声。
pub fn build_preview_tts_payload(
    assets_root: &Path,
    _base_root: &Path,
    _cache_dir: &Path,
    cfg: &ChatAiConfig,
    char_id: &str,
    seq: u64,
    fallback_text: &str,
    fish_runtime: &crate::fish_speech::FishSpeechRuntime,
) -> Result<(Option<String>, PetTtsPayload, String), String> {
    let vram_mode = normalize_vram_mode(&cfg.vram_mode);
    let _ = fish_runtime;
    if vram_mode != "zero_vram" && cfg.tts_mode == "fish_speech" {
        let (clean_preview, _) = extract_action_tag(fallback_text);
        let voice_script = resolve_voice_script_for_fish_speech(cfg, char_id, &clean_preview);
        let profile = cfg.fish_speech.char_profiles.get(char_id);
        let wav = crate::fish_speech::synthesize_fish_speech_wav(
            &cfg.fish_speech,
            &voice_script,
            profile,
        )?;
        return Ok((
            Some(clean_preview.clone()),
            PetTtsPayload {
                char_id: char_id.to_string(),
                text: clean_preview,
                kind: "wav_base64".to_string(),
                data: base64_encode(&wav),
                rate: 1.0,
                pitch: 1.0,
                force: true,
            },
            "GPT-SoVITS 克隆语音试听中".to_string(),
        ));
    }

    if cfg.tts_mode == "local" || vram_mode == "zero_vram" || cfg.azure_speech_key.trim().is_empty() {
        let (line, payload) =
            load_preview_local_voice_for_lang(assets_root, char_id, seq, &cfg.builtin_voice_lang)
                .ok_or_else(|| "未找到该角色的本地语音包".to_string())?;
        let lang_label = if normalize_builtin_voice_lang(&cfg.builtin_voice_lang) == "zh" {
            "正在播放内置中文离线语音包"
        } else {
            "正在播放内置日语离线语音包"
        };
        return Ok((
            Some(line),
            payload,
            lang_label.to_string(),
        ));
    }

    let profile = character_voice_profile(char_id, &cfg.char_voices, &cfg.custom_voice);
    let style = azure_style_for_action(profile.azure_voice, "click");
    let ogg = synthesize_azure_tts_ogg(
        &cfg.azure_speech_region,
        &cfg.azure_speech_key,
        fallback_text,
        profile.azure_voice,
        style,
        profile.edge_rate,
        profile.edge_pitch,
    )?;

    Ok((
        None,
        PetTtsPayload {
            char_id: char_id.to_string(),
            text: fallback_text.to_string(),
            kind: "ogg_base64".to_string(),
            data: base64_encode(&ogg),
            rate: profile.web_rate,
            pitch: profile.web_pitch,
            force: true,
        },
        "Azure 旗舰女声试听中".to_string(),
    ))
}

/// 日常互动/久坐/作息/番茄钟等场景统一语音调度：
/// 开启 GPT-SoVITS 日常克隆配音时优先合成对应角色日语（或中文）克隆语音，失败平滑降级本地 `.ogg` 语音包。
#[allow(clippy::too_many_arguments)]
pub fn build_scene_tts_payload(
    assets_root: &Path,
    _base_root: &Path,
    _cache_dir: &Path,
    cfg: &ChatAiConfig,
    char_id: &str,
    scene_key: &str,
    fallback_zh_line: &str,
    force: bool,
    _fish_runtime: &crate::fish_speech::FishSpeechRuntime,
) -> Option<PetTtsPayload> {
    let fallback_key = match scene_key {
        "pomodoro_done" | "done_2" => Some("done"),
        "work_start_2" => Some("work_start"),
        _ => None,
    };
    let vram_mode = normalize_vram_mode(&cfg.vram_mode);

    let try_manifest = |key: &str| {
        load_voice_manifest_ogg_payload_for_lang(
            assets_root,
            char_id,
            key,
            &cfg.builtin_voice_lang,
            force,
        )
        .map(|(_, p)| p)
    };

    // 均衡或零显存模式下：日常/作息/IDE 场景优先走 0ms 离线双语语音包（0 显存占用）
    if vram_mode != "full_realtime" {
        if let Some(p) = try_manifest(scene_key).or_else(|| fallback_key.and_then(try_manifest)) {
            return Some(p);
        }
    }

    if vram_mode != "zero_vram" && cfg.tts_mode == "fish_speech" {
        let want_ja = cfg
            .fish_speech
            .char_profiles
            .get(char_id)
            .map(|p| matches!(p.text_lang.trim(), "ja" | "all_ja"))
            .unwrap_or(false);
        let voice_script = if want_ja {
            builtin_ja_voice_for_scene(char_id, scene_key)
                .map(str::to_string)
                .unwrap_or_else(|| resolve_voice_script_for_fish_speech(cfg, char_id, fallback_zh_line))
        } else {
            fallback_zh_line.to_string()
        };
        let profile = cfg.fish_speech.char_profiles.get(char_id);
        if let Ok(wav) = crate::fish_speech::synthesize_fish_speech_wav(
            &cfg.fish_speech,
            &voice_script,
            profile,
        ) {
            return Some(PetTtsPayload {
                char_id: char_id.to_string(),
                text: fallback_zh_line.to_string(),
                kind: "wav_base64".to_string(),
                data: base64_encode(&wav),
                rate: 1.0,
                pitch: 1.0,
                force,
            });
        }
    }
    try_manifest(scene_key).or_else(|| fallback_key.and_then(try_manifest))
}

/* ------------------------------------------------------------------ */
/* 10 桶细粒度回复情绪抓取引擎                                          */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmotionBucket {
    Happy,
    Shy,
    Proud,
    Angry,
    Sad,
    Surprised,
    Sleepy,
    Thinking,
    Cheer,
    Affection,
}

impl EmotionBucket {
    pub fn voice_scene_key(self) -> &'static str {
        match self {
            Self::Happy => "emo_happy",
            Self::Shy => "emo_shy",
            Self::Proud => "praise",
            Self::Angry => "angry_1",
            Self::Sad => "emo_sad",
            Self::Surprised => "emo_surprised",
            Self::Sleepy => "tired",
            Self::Thinking => "cheer",
            Self::Cheer => "cheer",
            Self::Affection => "greet",
        }
    }

    #[allow(dead_code)]
    pub fn azure_action_hint(self) -> &'static str {
        match self {
            Self::Happy | Self::Proud | Self::Cheer => "talent",
            Self::Shy | Self::Affection => "click",
            Self::Sleepy => "sleep",
            Self::Thinking | Self::Angry => "work",
            Self::Sad | Self::Surprised => "emotion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmotionMatch {
    pub bucket: EmotionBucket,
    pub score: f64,
}

pub fn detect_reply_emotion(
    reply_zh: &str,
    reply_ja: Option<&str>,
    user_text: &str,
) -> Option<EmotionMatch> {
    struct Rule {
        bucket: EmotionBucket,
        zh: &'static [&'static str],
        ja: &'static [&'static str],
        kaomoji: &'static [&'static str],
    }

    const RULES: &[Rule] = &[
        Rule {
            bucket: EmotionBucket::Happy,
            zh: &[
                "开心", "高兴", "太好了", "太棒", "好耶", "嘻嘻", "哈哈", "嘿嘿", "完美",
                "搞定", "成功", "庆祝", "冲鸭", "欢呼", "爽", "绝了", "满分",
            ],
            ja: &[
                "やった", "嬉しい", "うれしい", "最高", "すごい", "えへへ", "あはは", "わーい",
                "大成功", "楽しい", "ナイス",
            ],
            kaomoji: &[
                "(*^▽^*)", "(≧▽≦)", "(๑>◡<๑)", "^▽^", "≧▽≦", "✨", "🎉", "🥳", "😄", "😆",
            ],
        },
        Rule {
            bucket: EmotionBucket::Shy,
            zh: &[
                "害羞", "脸红", "唔", "讨厌", "别夸", "才没有", "才不是", "不好意思", "摸头",
                "贴贴", "亲昵", "呆毛", "人家", "娇羞",
            ],
            ja: &[
                "恥ずかし", "はずかし", "照れ", "べ、別に", "別に", "うぅ", "きゃっ", "もうっ",
                "こほん",
            ],
            kaomoji: &[
                "(*/ω\\*)", "(〃∀〃)", "(///▽///)", "(>_<)", "(つд⊂)", "😳", "👉👈",
            ],
        },
        Rule {
            bucket: EmotionBucket::Proud,
            zh: &[
                "哼", "小意思", "常规", "当然", "本小姐", "本学霸", "不在话下", "轻轻松松",
                "那是自然", "厉害吧", "夸我", "骄傲", "得意", "信手拈来", "小菜一碟", "拿捏",
                "包在我身上",
            ],
            ja: &["ふふん", "当然", "お手の物", "さすが", "どや", "余裕", "完璧"],
            kaomoji: &["(￣▽￣)", "(｀・ω・´)", "(￣ω￣)", "(¬‿¬)", "😎", "😏", "👑"],
        },
        Rule {
            bucket: EmotionBucket::Angry,
            zh: &[
                "生气", "气死", "哼！", "过分", "别戳", "放手", "无语", "罢工", "可恶",
                "笨蛋", "欠揍", "忍不了", "皱眉", "啧", "离谱", "吐槽", "什么鬼",
            ],
            ja: &[
                "怒", "ふんっ", "ちょっと！", "いい加減", "もう知らない", "ちっ", "むっ",
                "やめて",
            ],
            kaomoji: &[
                "(╬▔皿▔)", "(╯°□°", "(｀へ´)", "(-_-;)", "(눈_눈)", "💢", "😤", "😠", "🙄",
            ],
        },
        Rule {
            bucket: EmotionBucket::Sad,
            zh: &[
                "难过", "伤心", "委屈", "哭", "呜呜", "对不起", "抱歉", "心疼", "安慰",
                "抱抱", "叹气", "沮丧", "好难", "失败", "崩溃", "压力大", "失落", "没关系",
                "别难过",
            ],
            ja: &["悲しい", "ごめん", "泣", "寂しい", "つらい", "しょんぼり", "うぅ…"],
            kaomoji: &["(T_T)", "(;_;)", "(´;ω;", "(╥﹏╥)", "(´･_･`)", "😢", "😭", "💧", "🥺"],
        },
        Rule {
            bucket: EmotionBucket::Surprised,
            zh: &[
                "哇啊", "卧槽", "居然", "竟然", "天哪", "不会吧", "震惊", "惊讶", "吓我一跳",
                "吓一跳", "真的吗", "什么？！", "？！", "!?", "我的天", "哇塞", "不可思议",
            ],
            ja: &["わっ！？", "えっ！？", "ええっ", "びっくり", "まさか", "うそ", "うわっ"],
            kaomoji: &["(°Д°)", "(O_O)", "(⊙_⊙)", "∑(ﾟДﾟ)", "(ﾟдﾟ)", "😱", "😲", "🤯", "⁉️"],
        },
        Rule {
            bucket: EmotionBucket::Sleepy,
            zh: &[
                "困", "好困", "想睡", "睡觉", "晚安", "熬夜", "好累", "累了", "疲惫",
                "休息", "哈欠", "眯一会", "眼皮", "早点睡", "伸懒腰",
            ],
            ja: &["眠い", "ねむい", "おやすみ", "疲れ", "つかれ", "休もう", "スリープ", "あくび"],
            kaomoji: &["(-_-)zzz", "(σω-)", "😴", "🥱", "💤", "🌙"],
        },
        Rule {
            bucket: EmotionBucket::Thinking,
            zh: &[
                "想想", "思考", "推理", "分析", "怎么回事", "奇怪", "疑惑", "研究", "排查",
                "定位", "逻辑", "原理", "拆解", "架构", "方案", "思路", "为什么", "嗯…",
                "让我想想",
            ],
            ja: &["考え", "推論", "分析", "なるほど", "うーん", "不思議", "原因", "確認", "整理"],
            kaomoji: &["(・_・?)", "(・・?)", "(?_?)", "🤔", "🧐", "💡", "🔍"],
        },
        Rule {
            bucket: EmotionBucket::Cheer,
            zh: &[
                "加油", "冲", "干劲", "写代码", "搞起来", "打气", "没问题", "肯定行", "稳的",
                "全力", "算力", "拉满", "debug", "重构", "上线", "必胜", "奥利给",
            ],
            ja: &["頑張", "がんば", "ファイト", "いくよ", "任せ", "フルパワー", "応援", "一緒に"],
            kaomoji: &["(๑•̀ㅂ•́)و", "٩(ˊᗜˋ*)و", "(ง •̀_•́)ง", "💪", "🔥", "⚡", "🚀"],
        },
        Rule {
            bucket: EmotionBucket::Affection,
            zh: &[
                "谢谢", "感谢", "早安", "早上好", "你好", "陪着你", "陪你", "在呢", "温柔",
                "喜欢你", "比心", "爱了", "辛苦了", "贴心", "治愈", "欢迎", "挥手",
            ],
            ja: &[
                "ありがとう", "感謝", "おはよう", "こんにちは", "そばに", "見守", "優しい",
                "大好き", "よろしく",
            ],
            kaomoji: &[
                "(｡･ω･｡)ﾉ♡", "(づ｡◕‿‿◕｡)づ", "(´▽｀)", "(♥ω♥*)", "❤️", "💖", "🌸", "👋",
            ],
        },
    ];

    let zh_lower = reply_zh.to_lowercase();
    let user_lower = user_text.to_lowercase();
    let ja_text = reply_ja.unwrap_or("");

    let mut best: Option<EmotionMatch> = None;
    for rule in RULES {
        let mut score = 0.0;
        for kw in rule.zh {
            if zh_lower.contains(kw) {
                score += 1.0;
            }
            if user_lower.contains(kw) {
                score += 0.45;
            }
        }
        if !ja_text.is_empty() {
            for kw in rule.ja {
                if ja_text.contains(kw) {
                    score += 0.95;
                }
            }
        }
        for km in rule.kaomoji {
            if reply_zh.contains(km) {
                score += 1.5;
            }
            if user_text.contains(km) {
                score += 0.8;
            }
        }
        if score >= 1.0 && best.is_none_or(|b| score > b.score) {
            best = Some(EmotionMatch {
                bucket: rule.bucket,
                score,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalization_handles_gemini_and_variants() {
        let g = normalize_chat_completions_url(
            "https://generativelanguage.googleapis.com/v1beta/openai/",
        )
        .unwrap();
        assert!(g.https);
        assert_eq!(g.host, "generativelanguage.googleapis.com");
        assert_eq!(g.port, 443);
        assert_eq!(g.path, "/v1beta/openai/chat/completions");

        let d = normalize_chat_completions_url("https://api.deepseek.com/v1").unwrap();
        assert_eq!(d.path, "/v1/chat/completions");
    }

    #[test]
    fn all_eight_chars_use_female_voices_only() {
        let empty = HashMap::new();
        for ch in ["claude", "deepseek", "gemini", "glm", "gpt", "grok", "kimi", "qwen"] {
            let p = character_voice_profile(ch, &empty, "");
            assert!(!p.azure_voice.contains("Yunxi"));
            assert!(!p.azure_voice.contains("Yunyang"));
            assert!(!p.azure_voice.contains("Yunxia"));
            assert!(!p.edge_voice.contains("Yunxi"));
            assert!(!p.edge_voice.contains("Yunyang"));
            assert!(!p.edge_voice.contains("Yunxia"));
        }
    }

    #[test]
    fn custom_persona_overrides_default() {
        let stats = PetStats::default();
        let prompt = character_persona_prompt(
            "deepseek",
            "DeepSeek",
            Some("我是自定义傲娇猫娘"),
            &stats,
            "待机",
        );
        assert!(prompt.contains("我是自定义傲娇猫娘"));
        assert!(!prompt.contains("蓝色大肥鱼"));
    }

    #[test]
    fn action_tag_extracted_and_stripped() {
        let (clean, act) = extract_action_tag("加油写代码呀！[动作:work]");
        assert_eq!(clean, "加油写代码呀！");
        assert_eq!(act.as_deref(), Some("work"));

        let (clean_bi, act_bi) = extract_action_tag(
            "红茶已经泡好了，休息一下吧。[日语:紅茶を淹れましたよ、少し休みましょうね。][动作:eat]",
        );
        assert_eq!(clean_bi, "红茶已经泡好了，休息一下吧。");
        assert_eq!(act_bi.as_deref(), Some("eat"));
        assert_eq!(
            get_cached_ja_voice_script("红茶已经泡好了，休息一下吧。").as_deref(),
            Some("紅茶を淹れましたよ、少し休みましょうね。")
        );

        let (clean_fw, act_fw, ja_fw) = extract_action_and_voice_tags(
            "才、才没有呢！这怎么能叫摸鱼！[日语：べ、別にそんなことないわよ！これのどこがサボりですの！][动作：emotion]",
        );
        assert_eq!(clean_fw, "才、才没有呢！这怎么能叫摸鱼！");
        assert_eq!(act_fw.as_deref(), Some("emotion"));
        assert_eq!(
            ja_fw.as_deref(),
            Some("べ、別にそんなことないわよ！これのどこがサボりですの！")
        );
    }

    #[test]
    fn tts_text_escapes_xml() {
        assert_eq!(
            clean_text_for_tts("**你好** & <世界> [动作:click]"),
            "你好 &amp; &lt;世界&gt;"
        );
    }

    #[test]
    fn builtin_ja_voice_covers_all_eight_chars_and_nineteen_scenes() {
        let chars = ["claude", "deepseek", "gemini", "glm", "gpt", "grok", "kimi", "qwen"];
        let scenes = [
            "click_1", "click_2", "feed", "drag", "praise", "hungry", "dance", "tired", "cheer",
            "greet", "wake", "lunch", "dinner", "sleep", "work_start", "error", "approval", "done",
            "pomodoro_done",
        ];
        for ch in chars {
            for sc in scenes {
                let line = builtin_ja_voice_for_scene(ch, sc)
                    .unwrap_or_else(|| panic!("missing ja line for {ch}/{sc}"));
                assert!(
                    contains_japanese_kana(line),
                    "expected kana in {ch}/{sc}: {line}"
                );
            }
        }
    }

    #[test]
    fn dual_lang_manifest_resolves_ja_zh_and_legacy() {
        let cfg = ChatAiConfig::default();
        assert_eq!(cfg.builtin_voice_lang, "ja");
        assert_eq!(cfg.vram_mode, "balanced");
        assert_eq!(cfg.vram_auto_unload_minutes, 5);

        let tmp = std::env::temp_dir().join("deskpet_test_dual_lang_manifest");
        let _ = fs::remove_dir_all(&tmp);
        let ja_dir = tmp.join("chars/claude/voices/ja");
        let zh_dir = tmp.join("chars/claude/voices/zh");
        fs::create_dir_all(&ja_dir).unwrap();
        fs::create_dir_all(&zh_dir).unwrap();
        fs::write(ja_dir.join("greet.ogg"), b"OggSja").unwrap();
        fs::write(zh_dir.join("greet.ogg"), b"OggSzh").unwrap();
        fs::write(
            ja_dir.join("manifest.json"),
            r#"{"scenes":{"greet":{"file":"greet.ogg","text":"こんにちは","zhText":"你好呀"}}}"#,
        )
        .unwrap();
        fs::write(
            zh_dir.join("manifest.json"),
            r#"{"scenes":{"greet":{"file":"greet.ogg","text":"你好呀"}}}"#,
        )
        .unwrap();

        let (ja_text, ja_rel) =
            load_voice_manifest_scene_for_lang(&tmp, "claude", "greet", "ja").unwrap();
        assert_eq!(ja_text, "你好呀");
        assert_eq!(ja_rel, "chars/claude/voices/ja/greet.ogg");

        let (zh_text, zh_rel) =
            load_voice_manifest_scene_for_lang(&tmp, "claude", "greet", "zh").unwrap();
        assert_eq!(zh_text, "你好呀");
        assert_eq!(zh_rel, "chars/claude/voices/zh/greet.ogg");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn builtin_bilingual_voice_covers_all_eight_chars_and_32_scenes() {
        let chars = ["claude", "deepseek", "gemini", "glm", "gpt", "grok", "kimi", "qwen"];
        let scenes = [
            "click_1", "click_2", "feed", "drag", "praise", "hungry", "dance", "tired", "cheer",
            "greet", "wake", "lunch", "dinner", "sleep", "work_start", "error", "approval", "done",
            "pomodoro_done", "angry_1", "angry_2", "work_start_2", "done_2", "context_high",
            "ambient_1", "ambient_2", "ambient_3", "ambient_4", "emo_happy", "emo_shy", "emo_sad",
            "emo_surprised",
        ];
        for ch in chars {
            for sc in scenes {
                let zh = builtin_zh_voice_for_scene(ch, sc)
                    .unwrap_or_else(|| panic!("missing zh line for {ch}/{sc}"));
                assert!(!zh.is_empty(), "empty zh line for {ch}/{sc}");
                let ja = builtin_ja_voice_for_scene(ch, sc)
                    .unwrap_or_else(|| panic!("missing ja line for {ch}/{sc}"));
                assert!(
                    contains_japanese_kana(ja),
                    "expected kana in {ch}/{sc}: {ja}"
                );
            }
        }
    }

    #[test]
    fn detect_reply_emotion_scores_zh_ja_and_kaomoji() {
        assert_eq!(
            detect_reply_emotion("太好了！终于搞定了！(≧▽≦)", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Happy)
        );
        assert_eq!(
            detect_reply_emotion("唔…别这样夸我啦，才没有开心呢", Some("恥ずかしいです"), "")
                .map(|m| m.bucket),
            Some(EmotionBucket::Shy)
        );
        assert_eq!(
            detect_reply_emotion("小意思，本学霸轻轻松松拿捏", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Proud)
        );
        assert_eq!(
            detect_reply_emotion("别戳了！再戳要罢工了！(╬▔皿▔)", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Angry)
        );
        assert_eq!(
            detect_reply_emotion("呜呜，抱歉让你失望了 (T_T)", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Sad)
        );
        assert_eq!(
            detect_reply_emotion("哇啊？！居然直接跑通了！？", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Surprised)
        );
        assert_eq!(
            detect_reply_emotion("好困呀，先去睡一觉吧 💤", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Sleepy)
        );
        assert_eq!(
            detect_reply_emotion("让我想想这个架构的逻辑原理 🤔", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Thinking)
        );
        assert_eq!(
            detect_reply_emotion("一起加油冲鸭！重构必胜 💪", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Cheer)
        );
        assert_eq!(
            detect_reply_emotion("谢谢你一直陪着我呀 🌸", None, "").map(|m| m.bucket),
            Some(EmotionBucket::Affection)
        );
    }
}
