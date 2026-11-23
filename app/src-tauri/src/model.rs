//! 数据模型：严格对齐 `docs/v1_contract.md` v1.1 与 `app/src/shared/ipc.ts`。
//!
//! 字段命名统一走 `camelCase`；`ClipMeta.loop` 是 Rust 关键字，内部字段名用
//! `looped`，序列化名保持契约里的 `loop`。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/* ------------------------------------------------------------------ */
/* catalog / character / clip                                          */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogChar {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub clips: u32,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub chars: Vec<CatalogChar>,
    #[serde(default)]
    pub generated_at: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: 1,
            chars: Vec::new(),
            generated_at: String::new(),
        }
    }
}

pub fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QaInfo {
    #[serde(default = "default_qa_status")]
    pub status: String,
    #[serde(default)]
    pub green_edge: f64,
    #[serde(default)]
    pub hole_ratio: f64,
}

fn default_qa_status() -> String {
    "unknown".to_string()
}

impl Default for QaInfo {
    fn default() -> Self {
        Self {
            status: default_qa_status(),
            green_edge: 0.0,
            hole_ratio: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipMeta {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub char: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub legacy_category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub frames: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub size: [u32; 2],
    #[serde(default)]
    pub body_box: [f64; 4],
    #[serde(default)]
    pub full_box: [f64; 4],
    #[serde(default)]
    pub anchor: [f64; 2],
    #[serde(rename = "loop", default)]
    pub looped: bool,
    #[serde(default)]
    pub interrupt_after_ms: u64,
    #[serde(default)]
    pub mirrorable: bool,
    #[serde(default)]
    pub audio: Option<String>,
    #[serde(default)]
    pub qa: QaInfo,
    #[serde(default)]
    pub hash: String,
    /// v1.3：跳过片头黑雾/未抠净首帧的安全起播偏移（ms），0 表示从第 0 帧直接起播。
    #[serde(default)]
    pub safe_start_ms: u32,
    /// v2.2：持续型动作（睡觉/工作等）的中段无缝循环窗口 [startMs, endMs]。
    #[serde(default)]
    pub sustain_loop: Option<[u32; 2]>,
}

impl Default for ClipMeta {
    fn default() -> Self {
        Self {
            schema_version: 1,
            id: String::new(),
            char: String::new(),
            category: String::new(),
            legacy_category: String::new(),
            tags: Vec::new(),
            frames: 0,
            fps: 30.0,
            duration_ms: 0,
            size: [1280, 720],
            body_box: [0.0; 4],
            full_box: [0.0; 4],
            anchor: [0.5, 0.89],
            looped: false,
            interrupt_after_ms: 0,
            mirrorable: true,
            audio: None,
            qa: QaInfo::default(),
            hash: String::new(),
            safe_start_ms: 0,
            sustain_loop: None,
        }
    }
}

impl ClipMeta {
    /// 只有 `qa.status != "fail"` 的片段才允许进入调度器。
    pub fn playable(&self) -> bool {
        self.qa.status != "fail" && self.frames > 0
    }

    pub fn effective_duration_ms(&self) -> u64 {
        if self.duration_ms > 0 {
            self.duration_ms
        } else if self.frames > 0 && self.fps > 0.0 {
            ((self.frames as f64 / self.fps) * 1000.0).round() as u64
        } else {
            0
        }
    }

    pub fn effective_fps(&self) -> f64 {
        if self.fps > 0.0 {
            self.fps
        } else {
            30.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Bindings {
    #[serde(default)]
    pub idle: Vec<String>,
    #[serde(default)]
    pub click: Vec<String>,
    #[serde(default)]
    pub drag: Vec<String>,
    #[serde(default)]
    pub move_left: Vec<String>,
    #[serde(default)]
    pub move_right: Vec<String>,
    #[serde(default)]
    pub work: Vec<String>,
    #[serde(default)]
    pub eat: Vec<String>,
    #[serde(default)]
    pub sleep: Vec<String>,
    #[serde(default)]
    pub wake: Vec<String>,
}

impl Bindings {
    pub fn get(&self, key: &str) -> Option<&[String]> {
        match key {
            "idle" => Some(&self.idle),
            "click" => Some(&self.click),
            "drag" => Some(&self.drag),
            "moveLeft" => Some(&self.move_left),
            "moveRight" => Some(&self.move_right),
            "work" => Some(&self.work),
            "eat" => Some(&self.eat),
            "sleep" => Some(&self.sleep),
            "wake" => Some(&self.wake),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub wake: String,
    pub lunch: String,
    pub dinner: String,
    pub sleep: String,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            wake: "08:30".to_string(),
            lunch: "12:00".to_string(),
            dinner: "18:30".to_string(),
            sleep: "23:30".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct CharacterFile {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub bindings: Bindings,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default = "default_scale")]
    pub default_scale: f64,
}

fn default_scale() -> f64 {
    0.5
}

/// 运行期角色快照：`character.json` + 全部 `*.meta.json`。
#[derive(Debug, Clone)]
pub struct CharacterRuntime {
    pub id: String,
    pub name: String,
    pub bindings: Bindings,
    pub bindings_json: Value,
    pub clips: Vec<ClipMeta>,
    pub clip_map: HashMap<String, ClipMeta>,
    pub schedule: Schedule,
    pub default_scale: f64,
}

impl CharacterRuntime {
    pub fn to_char_state(&self) -> CharState {
        CharState {
            char_id: self.id.clone(),
            name: self.name.clone(),
            bindings: self.bindings_json.clone(),
            clips: self.clips.clone(),
            default_scale: self.default_scale,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharState {
    pub char_id: String,
    pub name: String,
    pub bindings: Value,
    pub clips: Vec<ClipMeta>,
    pub default_scale: f64,
}

/* ------------------------------------------------------------------ */
/* 宠物状态                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PetStats {
    pub satiety: f64,
    pub energy: f64,
    pub mood: f64,
    pub affinity: f64,
}

impl Default for PetStats {
    fn default() -> Self {
        Self {
            satiety: 80.0,
            energy: 80.0,
            mood: 80.0,
            affinity: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PetPos {
    pub x: f64,
    pub y: f64,
}

impl Default for PetPos {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetState {
    pub stats: PetStats,
    pub pos: PetPos,
    pub scale: f64,
    pub pinned: bool,
    pub muted: bool,
}

impl Default for PetState {
    fn default() -> Self {
        Self {
            stats: PetStats::default(),
            pos: PetPos::default(),
            scale: 0.5,
            // 窗口配置本身是 alwaysOnTop，默认状态保持一致。
            pinned: true,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub clip_id: String,
    pub bubble: String,
}

/* ------------------------------------------------------------------ */
/* event 载荷                                                          */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayPayload {
    /// v1.3：附带角色 ID，前端收到跨角色抢跑 play 时可暂存避免 404 空播。
    pub char_id: String,
    pub clip_id: String,
    pub start_at_ms: u64,
    pub mirror: bool,
    pub audio: Option<String>,
    /// 状态锁定循环段（如持续工作/思考/审批/告警态），前端强制循环且不中途退回待机。
    #[serde(default)]
    pub hold_loop: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BubblePayload {
    pub text: String,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameHint {
    pub clip_id: String,
    pub frame: u32,
}

/* ------------------------------------------------------------------ */
/* 角色资产加载                                                        */
/* ------------------------------------------------------------------ */

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let raw =
        fs::read_to_string(path).map_err(|err| format!("读取失败 {}: {err}", path.display()))?;
    serde_json::from_str(&raw).map_err(|err| format!("解析失败 {}: {err}", path.display()))
}

/// 读取 `assets/chars/<charId>/character.json` 与 `clips/*.meta.json`。
///
/// 单个 clip 元数据损坏只跳过该段，不拖垮整个角色；角色文件缺失才是硬错误。
pub fn load_character(assets_root: &Path, char_id: &str) -> Result<CharacterRuntime, String> {
    if char_id.is_empty()
        || char_id.contains('/')
        || char_id.contains('\\')
        || char_id.contains("..")
    {
        return Err(format!("非法角色 id: {char_id}"));
    }

    let dir = assets_root.join("chars").join(char_id);
    let character_path = dir.join("character.json");
    let raw = fs::read_to_string(&character_path)
        .map_err(|err| format!("读取角色失败 {}: {err}", character_path.display()))?;
    let raw_value: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("解析角色失败 {}: {err}", character_path.display()))?;
    let character: CharacterFile = serde_json::from_value(raw_value.clone())
        .map_err(|err| format!("解析角色失败 {}: {err}", character_path.display()))?;

    let mut clips: Vec<ClipMeta> = Vec::new();
    let clips_dir: PathBuf = dir.join("clips");
    if clips_dir.is_dir() {
        let entries = fs::read_dir(&clips_dir)
            .map_err(|err| format!("读取目录失败 {}: {err}", clips_dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !file_name.ends_with(".meta.json") {
                continue;
            }
            match read_json::<ClipMeta>(&path) {
                Ok(mut meta) => {
                    if meta.id.is_empty() {
                        meta.id = file_name.trim_end_matches(".meta.json").to_string();
                    }
                    if meta.char.is_empty() {
                        meta.char = char_id.to_string();
                    }
                    clips.push(meta);
                }
                Err(err) => eprintln!("[deskpet] 跳过损坏的元数据 {file_name}: {err}"),
            }
        }
    }
    clips.sort_by(|left, right| left.id.cmp(&right.id));

    let mut clip_map = HashMap::with_capacity(clips.len());
    for clip in &clips {
        clip_map.insert(clip.id.clone(), clip.clone());
    }

    let bindings_json = raw_value
        .get("bindings")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    Ok(CharacterRuntime {
        id: char_id.to_string(),
        name: if character.name.trim().is_empty() {
            char_id.to_string()
        } else {
            character.name
        },
        bindings: character.bindings,
        bindings_json,
        clips,
        clip_map,
        schedule: character.schedule,
        default_scale: character.default_scale.clamp(0.25, 2.0),
    })
}

/// 没有可用 catalog 时，直接从 `assets/chars` 目录生成一份。
pub fn scan_catalog(assets_root: &Path) -> Catalog {
    let mut chars = Vec::new();
    let root = assets_root.join("chars");
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let clips = fs::read_dir(path.join("clips"))
                .map(|items| {
                    items
                        .flatten()
                        .filter(|item| item.file_name().to_string_lossy().ends_with(".meta.json"))
                        .count() as u32
                })
                .unwrap_or(0);
            let name = fs::read_to_string(path.join("character.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| {
                    value
                        .get("name")
                        .and_then(|name| name.as_str())
                        .map(|name| name.to_string())
                })
                .unwrap_or_else(|| id.clone());
            chars.push(CatalogChar {
                id,
                name,
                clips,
                path: format!("chars/{}", entry.file_name().to_string_lossy()),
            });
        }
    }
    chars.sort_by(|left, right| left.id.cmp(&right.id));
    Catalog {
        schema_version: 1,
        chars,
        generated_at: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_assets_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
    }

    #[test]
    fn loads_real_claude_character_and_clips() {
        let root = real_assets_root();
        if !root.join("chars").join("claude").is_dir() {
            return;
        }
        let character = load_character(&root, "claude").expect("load real claude");
        assert_eq!(character.id, "claude");
        assert!(!character.clips.is_empty(), "clips 不能为空");
        assert!(!character.bindings.idle.is_empty(), "idle 绑定不能为空");
        let first_idle = &character.bindings.idle[0];
        assert!(
            character.clip_map.contains_key(first_idle),
            "idle 绑定 {first_idle} 必须能在 clip_map 中找到"
        );
        let meta = character.clip_map.get(first_idle).expect("idle meta");
        assert!(meta.frames > 0);
        assert!(meta.duration_ms > 0);
    }

    #[test]
    fn real_catalog_scan_lists_all_eight_chars() {
        let root = real_assets_root();
        if !root.join("chars").is_dir() {
            return;
        }
        let catalog = scan_catalog(&root);
        assert!(catalog.chars.len() >= 8, "catalog={:?}", catalog.chars);
        assert!(catalog.chars.iter().any(|item| item.id == "deepseek"));
    }
}
