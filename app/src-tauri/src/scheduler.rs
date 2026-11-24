//! 纯 Rust 调度器：6 级优先级状态机、加权选段与 2h 去重。
//!
//! 该模块不依赖 Tauri / Win32，因此可以在 `cargo test` 里完整验证抢占与权重；
//! 真正的后台线程、事件发射与落库在 `state.rs` / `lib.rs` 里接。

use std::collections::HashMap;

use crate::model::{CharacterRuntime, ClipMeta, Schedule};
use crate::telemetry::{AgentActivityKind, AgentSceneSignal, TelemetryStatus};

/// 2 小时。
pub const TWO_HOURS_MS: i64 = 2 * 60 * 60 * 1000;
/// 播放满 2 小时后权重降到 0.1。
pub const REPEAT_WEIGHT: f64 = 0.1;
/// 同优先级重播防抖。
const SAME_PRIORITY_DEBOUNCE_MS: i64 = 120;
/// 同类 Agent 告警 60s 内只提醒一次，防止流式采样刷屏。
pub const AGENT_ALERT_COOLDOWN_MS: i64 = 60_000;
/// 除 L1 阻塞/告警外，基础动作切换后的最小驻留防抖时长（900ms）。
pub const MIN_SCENE_DWELL_MS: i64 = 900;
/// 常规工作顺序轮播时单段最大播放时长（15s），避免 30s 超长段久占队列；10s/15s 段完整播完。
pub const MAX_PLAYLIST_CLIP_MS: u64 = 15_000;

/// 精选常规工作顺序轮播模板（不含思考开场、不含报错叹气、不含完成庆祝）。
const WORK_PLAYLIST_ORDER: &[&str] = &[
    "写代码",
    "打字哑剧",
    "工作加班",
    "轻快记录",
    "写字哑剧",
    "工作状态-忙碌点按",
    "工作状态-清点归档",
    "翻阅书册",
    "拨算盘珠",
    "工作状态-原地踱步张望",
    "工作状态-原地踱步张望 v2",
    "打电话汇报",
    "图书管理员嘘",
    "拍照取景",
    "侧身举相机拍别人",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkPhase {
    None,
    ThinkingIntro,
    Playlist,
}

/// 是否属于低频特色工作场景（保持原精确映射，不走顺序队列）。
fn is_specialty_work_signal(signal: &AgentSceneSignal) -> bool {
    matches!(
        signal.activity,
        AgentActivityKind::BrowserRunning
            | AgentActivityKind::McpToolCalling
            | AgentActivityKind::WorkflowRunning
            | AgentActivityKind::BuildLongRunning
            | AgentActivityKind::TerminalError
    ) || (signal.activity == AgentActivityKind::ResearchLookup
        && signal.detail.as_deref() == Some("web_search"))
}

/// 是否属于常规高频工作流（走「首次思考开场 → 顺序工作队列轮播」）。
fn is_playlist_work_signal(signal: &AgentSceneSignal) -> bool {
    matches!(
        signal.activity,
        AgentActivityKind::DeepReasoning
            | AgentActivityKind::CodeStreaming
            | AgentActivityKind::CodeDiffing
            | AgentActivityKind::TerminalRunning
            | AgentActivityKind::PlanningMode
    ) || (signal.activity == AgentActivityKind::ResearchLookup
        && signal.detail.as_deref() != Some("web_search"))
}

/// 构建当前角色的常规工作顺序轮播队列（不含思考、报错叹气、完成庆祝）。
pub fn build_work_playlist(character: &CharacterRuntime) -> Vec<ClipMeta> {
    let mut playlist = Vec::new();
    for name in WORK_PLAYLIST_ORDER {
        if let Some(clip) = character.clip_map.get(*name).filter(|c| c.playable()) {
            if !playlist.iter().any(|existing: &ClipMeta| existing.id == clip.id) {
                playlist.push(clip.clone());
            }
        }
    }
    if !playlist.is_empty() {
        return playlist;
    }

    let mut fallback = Vec::new();
    for id in &character.bindings.work {
        if let Some(clip) = character.clip_map.get(id).filter(|c| c.playable()) {
            let excluded = clip.id.contains("思考")
                || clip.id.contains("垂头叹气")
                || clip.id.contains("雀跃庆祝");
            if !excluded && !fallback.iter().any(|existing: &ClipMeta| existing.id == clip.id) {
                fallback.push(clip.clone());
            }
        }
    }
    if !fallback.is_empty() {
        return fallback;
    }

    for id in &character.bindings.work {
        if let Some(clip) = character.clip_map.get(id).filter(|c| c.playable()) {
            if !playlist.iter().any(|existing: &ClipMeta| existing.id == clip.id) {
                playlist.push(clip.clone());
            }
        }
    }
    playlist
}

fn find_playlist_index_for_activity(
    character: &CharacterRuntime,
    playlist: &[ClipMeta],
    activity: AgentActivityKind,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> usize {
    if let Some(matched) = resolve_clip_for_scene(character, activity, history, now_ms, rng) {
        if let Some(idx) = playlist.iter().position(|c| c.id == matched.id) {
            return idx;
        }
    }
    0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Idle = 10,
    ForegroundWork = 30,
    Schedule = 40,
    AgentAlert = 50,
    Command = 60,
    Click = 80,
    Drag = 100,
}

impl Priority {
    pub fn label(self) -> &'static str {
        match self {
            Priority::Idle => "IDLE",
            Priority::ForegroundWork => "FOREGROUND_WORK",
            Priority::Schedule => "SCHEDULE",
            Priority::AgentAlert => "AGENT_ALERT",
            Priority::Command => "COMMAND",
            Priority::Click => "CLICK",
            Priority::Drag => "DRAG",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SchedulerEvent {
    AppReady,
    UserClick {
        x: f64,
        y: f64,
    },
    UserDragStart {
        x: f64,
        y: f64,
    },
    UserDragEnd {
        x: f64,
        y: f64,
    },
    NeedHungry,
    NeedSleepy,
    ScheduleTick {
        slot: String,
    },
    WorkAppFocus,
    WorkAppBlur,
    ChatReply {
        clip_id: String,
    },
    PlayCommand {
        clip_id: String,
    },
    ClipEnded {
        clip_id: Option<String>,
    },
    CharacterChanged(Box<CharacterRuntime>),
    /// v1.2：Antigravity 采集到的精细状态（兼容接口，内部转为 AgentScene）。
    AgentActivity(TelemetryStatus),
    /// v2.1：Antigravity 全生命周期精细场景事件。
    AgentScene(AgentSceneSignal),
    /// v1.2：阈值告警（上下文 ≥80% / 额度 ≤10%），同类 60s 冷却。
    AgentAlert {
        key: String,
        bubble: String,
    },
    /// v2.2：纯气泡提醒（如单步上下文增量），绝不打断当前播放动画。
    AgentBubble {
        text: String,
        ttl_ms: u64,
    },
    /// v2.4：API 额度晴雨表余量档位联动。
    QuotaBarometer {
        remaining_pct: f64,
        announce: bool,
    },
}

#[derive(Debug, Clone)]
pub struct PlayRequest {
    pub clip_id: String,
    pub priority: Priority,
    pub mirror: bool,
    pub audio: Option<String>,
    pub frames: u32,
    pub fps: f64,
    pub duration_ms: u64,
    pub interrupt_after_ms: u64,
    pub looped: bool,
    pub hold_loop: bool,
}

#[derive(Debug, Clone)]
pub enum Action {
    Play(PlayRequest),
    Bubble { text: String, ttl_ms: u64 },
    Voice {
        scene_key: String,
        default_bubble: Option<String>,
        ttl_ms: u64,
        force: bool,
    },
}

#[derive(Debug, Clone)]
struct Current {
    clip_id: String,
    priority: Priority,
    started_ms: i64,
    mirror: bool,
    audio: Option<String>,
    frames: u32,
    fps: f64,
    duration_ms: u64,
    interrupt_after_ms: u64,
    looped: bool,
    hold_loop: bool,
    in_work_playlist: bool,
    scene: Option<AgentActivityKind>,
}

impl Current {
    fn from_request(request: &PlayRequest, started_ms: i64, scene: Option<AgentActivityKind>) -> Self {
        Self {
            clip_id: request.clip_id.clone(),
            priority: request.priority,
            started_ms,
            mirror: request.mirror,
            audio: request.audio.clone(),
            frames: request.frames,
            fps: request.fps,
            duration_ms: request.duration_ms,
            interrupt_after_ms: request.interrupt_after_ms,
            looped: request.looped,
            hold_loop: request.hold_loop,
            in_work_playlist: false,
            scene,
        }
    }

    fn to_request(&self) -> PlayRequest {
        PlayRequest {
            clip_id: self.clip_id.clone(),
            priority: self.priority,
            mirror: self.mirror,
            audio: self.audio.clone(),
            frames: self.frames,
            fps: self.fps,
            duration_ms: self.duration_ms,
            interrupt_after_ms: self.interrupt_after_ms,
            looped: self.looped,
            hold_loop: self.hold_loop,
        }
    }
}

/* ------------------------------------------------------------------ */
/* 随机数与加权选择                                                    */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

/// 2 小时内播过 → 权重 ×0.1；否则 1.0。未来时间戳（时钟回拨）按正常权重。
pub fn selection_weight(last_played_ms: Option<i64>, now_ms: i64) -> f64 {
    match last_played_ms {
        Some(last) if now_ms >= last && now_ms - last < TWO_HOURS_MS => REPEAT_WEIGHT,
        _ => 1.0,
    }
}

/// 纯本地公历/农历节日与四季时令权重加权（零联网）。
pub fn festival_and_season_weight(clip_id: &str, _category: &str, date_key: i32) -> f64 {
    if date_key <= 0 {
        return 1.0;
    }
    let mmdd = date_key % 10_000;
    let month = (mmdd / 100) % 100;

    // 2025-2035 农历核心节日公历窗口（YYYYMMDD 区间）：春节(除夕-初七)、元宵(十四-十六)、端午(初四-初六)、七夕(初六-初八)、中秋(十四-十六)、重阳(初八-初十)
    const SPRING_FESTIVAL: &[(i32, i32)] = &[
        (20250128, 20250204),
        (20260216, 20260223),
        (20270205, 20270212),
        (20280125, 20280201),
        (20290212, 20290219),
        (20300202, 20300209),
        (20310122, 20310129),
        (20320210, 20320217),
        (20330130, 20330206),
        (20340218, 20340225),
        (20350207, 20350214),
    ];
    const LANTERN_FESTIVAL: &[(i32, i32)] = &[
        (20250211, 20250213),
        (20260302, 20260304),
        (20270220, 20270222),
        (20280209, 20280211),
        (20290227, 20290301),
        (20300216, 20300218),
        (20310205, 20310207),
        (20320224, 20320226),
        (20330213, 20330215),
        (20340304, 20340306),
        (20350221, 20350223),
    ];
    const DRAGON_BOAT: &[(i32, i32)] = &[
        (20250530, 20250601),
        (20260618, 20260620),
        (20270608, 20270610),
        (20280527, 20280529),
        (20290615, 20290617),
        (20300604, 20300606),
        (20310623, 20310625),
        (20320611, 20320613),
        (20330531, 20330602),
        (20340619, 20340621),
        (20350609, 20350611),
    ];
    const QIXI: &[(i32, i32)] = &[
        (20250828, 20250830),
        (20260818, 20260820),
        (20270807, 20270809),
        (20280825, 20280827),
        (20290815, 20290817),
        (20300804, 20300806),
        (20310823, 20310825),
        (20320811, 20320813),
        (20330731, 20330802),
        (20340819, 20340821),
        (20350809, 20350811),
    ];
    const MID_AUTUMN: &[(i32, i32)] = &[
        (20251005, 20251007),
        (20260924, 20260926),
        (20270914, 20270916),
        (20281002, 20281004),
        (20290921, 20290923),
        (20300911, 20300913),
        (20310930, 20311002),
        (20320918, 20320920),
        (20330907, 20330909),
        (20340926, 20340928),
        (20350915, 20350917),
    ];
    const CHONGYANG: &[(i32, i32)] = &[
        (20251028, 20251030),
        (20261017, 20261019),
        (20271007, 20271009),
        (20281025, 20281027),
        (20291015, 20291017),
        (20301004, 20301006),
        (20311023, 20311025),
        (20321011, 20321013),
        (20330930, 20331002),
        (20341019, 20341021),
        (20351008, 20351010),
    ];

    let in_ranges = |ranges: &[(i32, i32)]| ranges.iter().any(|&(s, e)| date_key >= s && date_key <= e);

    if in_ranges(SPRING_FESTIVAL)
        && matches!(
            clip_id,
            "贴春联" | "写福字" | "收红包" | "敲锣打鼓拜年" | "舞狮头" | "舞狮" | "放烟花" | "放礼花筒" | "挂灯笼"
        )
    {
        return 6.0;
    }
    if in_ranges(LANTERN_FESTIVAL)
        && matches!(
            clip_id,
            "吃汤圆" | "挂灯笼" | "点许愿灯" | "点许愿灯 v2" | "点许愿灯 v3" | "放孔明灯" | "放孔明灯v2"
        )
    {
        return 6.0;
    }
    if in_ranges(DRAGON_BOAT) && matches!(clip_id, "吃粽子" | "折小纸船" | "编草蚱蜢") {
        return 6.0;
    }
    if in_ranges(QIXI)
        && matches!(clip_id, "穿针乞巧" | "放河灯" | "放河灯v2" | "放河灯v3" | "摘花送镜头")
    {
        return 6.0;
    }
    if in_ranges(MID_AUTUMN)
        && matches!(clip_id, "中秋赏月吃月饼" | "放孔明灯" | "放孔明灯v2" | "点许愿灯")
    {
        return 6.0;
    }
    if in_ranges(CHONGYANG) && matches!(clip_id, "插茱萸赏菊" | "写毛笔字" | "磨墨") {
        return 6.0;
    }
    if (1030..=1101).contains(&mmdd) && matches!(clip_id, "讨糖南瓜灯" | "萌化小幽灵") {
        return 6.0;
    }
    if (1224..=1226).contains(&mmdd)
        && matches!(clip_id, "装点圣诞树" | "装点圣诞树v2" | "装点圣诞树v3" | "拆礼物" | "打铁小铃")
    {
        return 6.0;
    }
    if (mmdd == 1231 || (101..=103).contains(&mmdd))
        && matches!(clip_id, "放烟花" | "放礼花筒" | "写福字" | "拆礼物" | "挂彩旗")
    {
        return 5.0;
    }

    // 四季时令加权
    match month {
        3..=5 if matches!(clip_id, "放风筝" | "接雨滴" | "撑透明小伞" | "撒花瓣" | "花艺师包花") => {
            2.2
        }
        6..=8 if matches!(clip_id, "听蝉" | "摇扇纳凉" | "举手遮阳偷看" | "刨冰屑" | "风扇前啊啊叫") => {
            2.2
        }
        9..=11 if matches!(clip_id, "被落叶淹没" | "被落叶淹没v2" | "剥栗子" | "扎束干花" | "被风吹乱发型") => {
            2.2
        }
        12 | 1 | 2
            if matches!(
                clip_id,
                "堆雪人" | "打雪仗" | "原地踏雪" | "掌心接雪花" | "抱暖手炉" | "搓手哈气取暖" | "织毛衣" | "冲可可"
            ) =>
        {
            2.2
        }
        _ => 1.0,
    }
}

/// API 额度晴雨表四档「余额」待机加权。
pub fn quota_balance_idle_weight(clip_id: &str, quota_pct: Option<f64>) -> f64 {
    let Some(pct) = quota_pct else {
        return 1.0;
    };
    if pct >= 70.0 {
        if matches!(clip_id, "余额-钱袋满溢" | "余额-金袋叮当") {
            return 1.35;
        }
    } else if pct >= 25.0 {
        if matches!(clip_id, "余额-钱袋如常" | "余额-金袋叮当") {
            return 1.35;
        }
    } else if pct > 10.0 {
        if matches!(clip_id, "余额-数金皱眉" | "余额-钱袋如常") {
            return 1.35;
        }
    } else if matches!(clip_id, "余额-袋空如洗" | "余额-分文不剩" | "余额-数金皱眉") {
        return 1.35;
    }
    1.0
}

fn quota_barometer_candidates(remaining_pct: f64) -> (&'static [&'static str], String) {
    let pct_round = remaining_pct.round() as i64;
    if remaining_pct >= 70.0 {
        (
            &["余额-钱袋满溢", "余额-金袋叮当"],
            format!("当前模型池额度充沛（{pct_round}%），金库满满当当～"),
        )
    } else if remaining_pct >= 25.0 {
        (
            &["余额-钱袋如常", "余额-金袋叮当"],
            format!("当前模型池余量稳健（{pct_round}%），收支平衡节奏正好～"),
        )
    } else if remaining_pct > 10.0 {
        (
            &["余额-数金皱眉", "余额-钱袋如常"],
            format!("当前额度余量 {pct_round}%，注意精打细算哦～"),
        )
    } else {
        (
            &["余额-袋空如洗", "余额-分文不剩", "余额-数金皱眉"],
            format!("当前模型池额度见底啦（{pct_round}%），考虑切个备选池吧～"),
        )
    }
}

/// 纯函数加权抽取，`r ∈ [0, 1)`，便于单测锁定 0.1 权重的行为。
pub fn pick_weighted_value<T: Clone>(items: &[T], weights: &[f64], r: f64) -> Option<T> {
    if items.is_empty() || items.len() != weights.len() {
        return None;
    }
    let total: f64 = weights
        .iter()
        .filter(|weight| weight.is_finite() && **weight > 0.0)
        .sum();
    if total <= 0.0 {
        return items.first().cloned();
    }
    let mut target = r.clamp(0.0, 0.999_999_999) * total;
    let mut last = None;
    for (item, weight) in items.iter().zip(weights.iter()) {
        if !weight.is_finite() || *weight <= 0.0 {
            continue;
        }
        last = Some(item.clone());
        target -= *weight;
        if target <= 0.0 {
            return Some(item.clone());
        }
    }
    last
}

fn pick_from_ids(
    character: &CharacterRuntime,
    ids: &[String],
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
    exclude_id: Option<&str>,
) -> Option<ClipMeta> {
    let all_candidates: Vec<&ClipMeta> = ids
        .iter()
        .filter_map(|id| character.clip_map.get(id))
        .filter(|clip| clip.playable())
        .collect();
    if all_candidates.is_empty() {
        return None;
    }
    let candidates: Vec<&ClipMeta> = match exclude_id {
        Some(excluded) if all_candidates.len() > 1 => {
            let filtered: Vec<&ClipMeta> = all_candidates
                .iter()
                .copied()
                .filter(|clip| clip.id != excluded)
                .collect();
            if filtered.is_empty() {
                all_candidates
            } else {
                filtered
            }
        }
        _ => all_candidates,
    };
    let weights: Vec<f64> = candidates
        .iter()
        .map(|clip| selection_weight(history.get(&clip.id).copied(), now_ms))
        .collect();
    pick_weighted_value(&candidates, &weights, rng.next_f64()).map(|clip| clip.clone())
}

/// 无专属 drag 动作的角色（如 deepseek / gpt）拖拽时优先挑选惊呼/摇摆/蹦跳类活灵活现片段。
const LIVELY_DRAG_FALLBACKS: &[&str] = &[
    "被鼠标拖拽悬空反馈",
    "双手上举摇摆",
    "惊讶下巴掉",
    "小鹿受惊",
    "惊吓后缩",
    "得意炫耀转圈",
    "小兔蹦跳",
    "开合跳",
];

/// 按绑定键选段；缺失/为空/fail 时回退 `idle`，再回退全部可播段。
pub fn pick_for_binding(
    character: &CharacterRuntime,
    key: &str,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> Option<ClipMeta> {
    pick_for_binding_excluding(character, key, history, now_ms, rng, None)
}

fn is_work_clip(clip: &ClipMeta) -> bool {
    clip.category == "work" || clip.id.starts_with("工作状态") || clip.id == "写代码"
}

pub fn pick_for_binding_excluding(
    character: &CharacterRuntime,
    key: &str,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
    exclude_id: Option<&str>,
) -> Option<ClipMeta> {
    if key == "drag" {
        // v1.5：拖拽选段保持确定性，与前端预热槽 `dragSlot` 100% 命中同一片段实现 <16ms 零延迟起播。
        if let Some(clip) = character
            .clip_map
            .get("被鼠标拖拽悬空反馈")
            .filter(|c| c.playable())
        {
            return Some(clip.clone());
        }
        if let Some(ids) = character.bindings.get("drag") {
            if let Some(clip) = ids
                .iter()
                .filter_map(|id| character.clip_map.get(id))
                .find(|c| c.playable() && c.category == "drag")
            {
                return Some(clip.clone());
            }
        }
        for name in LIVELY_DRAG_FALLBACKS {
            if let Some(clip) = character.clip_map.get(*name).filter(|c| c.playable()) {
                return Some(clip.clone());
            }
        }
    }
    if let Some(ids) = character.bindings.get(key) {
        if key == "idle" {
            let non_work: Vec<String> = ids
                .iter()
                .filter(|id| character.clip_map.get(*id).is_some_and(|c| !is_work_clip(c)))
                .cloned()
                .collect();
            if let Some(clip) =
                pick_from_ids(character, &non_work, history, now_ms, rng, exclude_id)
            {
                return Some(clip);
            }
        }
        if let Some(clip) = pick_from_ids(character, ids, history, now_ms, rng, exclude_id) {
            return Some(clip);
        }
    }
    if key != "idle" {
        if let Some(ids) = character.bindings.get("idle") {
            let non_work: Vec<String> = ids
                .iter()
                .filter(|id| character.clip_map.get(*id).is_some_and(|c| !is_work_clip(c)))
                .cloned()
                .collect();
            if let Some(clip) =
                pick_from_ids(character, &non_work, history, now_ms, rng, exclude_id)
            {
                return Some(clip);
            }
            if let Some(clip) = pick_from_ids(character, ids, history, now_ms, rng, exclude_id) {
                return Some(clip);
            }
        }
    }
    let all_playable: Vec<&ClipMeta> = character
        .clips
        .iter()
        .filter(|clip| clip.playable())
        .collect();
    let pool: Vec<&ClipMeta> = if key == "idle" {
        let non_work: Vec<&ClipMeta> = all_playable
            .iter()
            .copied()
            .filter(|clip| !is_work_clip(clip))
            .collect();
        if non_work.is_empty() {
            all_playable
        } else {
            non_work
        }
    } else {
        all_playable
    };
    let weights: Vec<f64> = pool
        .iter()
        .map(|clip| selection_weight(history.get(&clip.id).copied(), now_ms))
        .collect();
    pick_weighted_value(&pool, &weights, rng.next_f64()).map(|clip| clip.clone())
}

/// 高频连点生气触发参数（#1）。
pub const RAPID_CLICK_WINDOW_MS: i64 = 3_000;
pub const RAPID_CLICK_THRESHOLD: usize = 5;
pub const ANGRY_COOLDOWN_MS: i64 = 14_000;

pub const ANGRY_CLIP_CANDIDATES: &[&str] = &[
    "点击回应-傲娇生气",
    "傲娇生气",
    "生气鼓腮跺脚",
    "生气鼓腮",
    "憋气鼓腮",
    "委屈赌气",
    "合扇敲掌·等待不耐烦",
    "玩游戏气急败坏",
];

pub fn resolve_angry_clip(
    character: &CharacterRuntime,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> Option<ClipMeta> {
    let matched: Vec<&ClipMeta> = ANGRY_CLIP_CANDIDATES
        .iter()
        .filter_map(|name| character.clip_map.get(*name).filter(|c| c.playable()))
        .collect();
    if !matched.is_empty() {
        let weights: Vec<f64> = matched
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                let prio = if idx == 0 { 1.4 } else { 1.0 };
                prio * selection_weight(history.get(&c.id).copied(), now_ms)
            })
            .collect();
        return pick_weighted_value(&matched, &weights, rng.next_f64()).map(|c| c.clone());
    }
    pick_for_binding(character, "click", history, now_ms, rng)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbientTier {
    Rest,
    DailyPlayEat,
    TalentEaster,
}

fn is_ambient_excluded(clip: &ClipMeta) -> bool {
    if is_work_clip(clip) || clip.category == "drag" || clip.category == "click" {
        return true;
    }
    const EXCLUDED_SUBSTRS: &[&str] = &[
        "傲娇生气",
        "生气鼓腮",
        "委屈赌气",
        "哭泣",
        "害怕发抖",
        "被吓一跳",
        "惊吓后缩",
        "震惊僵住",
        "玩游戏气急败坏",
        "垂头叹气",
        "沮丧",
        "焦虑踱步",
        "深度思考",
    ];
    EXCLUDED_SUBSTRS.iter().any(|s| clip.id.contains(s))
}

pub fn classify_ambient_tier(clip: &ClipMeta, idle_bindings: &[String]) -> Option<AmbientTier> {
    if !clip.playable() || is_ambient_excluded(clip) {
        return None;
    }
    if matches!(
        clip.category.as_str(),
        "idle" | "chatter" | "balance" | "etiquette" | "species"
    ) || idle_bindings.iter().any(|id| id == &clip.id)
    {
        return Some(AmbientTier::Rest);
    }
    if matches!(
        clip.category.as_str(),
        "daily" | "play" | "eat" | "move" | "turn"
    ) {
        return Some(AmbientTier::DailyPlayEat);
    }
    if matches!(
        clip.category.as_str(),
        "talent" | "festival" | "seasonal" | "vfx" | "outfit" | "emotion"
    ) {
        return Some(AmbientTier::TalentEaster);
    }
    None
}

pub fn time_of_day_tier_weights(minute_of_day: i32, has_active_festival: bool) -> [f64; 3] {
    let mut w: [f64; 3] = if (690..810).contains(&minute_of_day) || (1050..1200).contains(&minute_of_day) {
        [0.45, 0.40, 0.15]
    } else if minute_of_day >= 1380 || minute_of_day < 390 {
        [0.72, 0.18, 0.10]
    } else {
        [0.55, 0.30, 0.15]
    };
    if has_active_festival {
        w[0] = (w[0] - 0.07).max(0.20);
        w[2] += 0.07;
    }
    w
}

pub fn time_of_day_clip_weight(clip: &ClipMeta, minute_of_day: i32) -> f64 {
    if (390..570).contains(&minute_of_day) {
        if clip.tags.iter().any(|t| t == "wake")
            || clip.id.contains("刷牙")
            || clip.id.contains("伸懒腰")
            || clip.id.contains("咖啡")
            || clip.id.contains("早餐")
        {
            return 2.4;
        }
    } else if (690..810).contains(&minute_of_day) || (1050..1200).contains(&minute_of_day) {
        if clip.category == "eat" {
            return 2.5;
        }
    } else if (810..1050).contains(&minute_of_day) {
        if ["咖啡", "奶茶", "可可", "布丁", "蛋糕", "饼干"]
            .iter()
            .any(|k| clip.id.contains(k))
        {
            return 1.6;
        }
    } else if minute_of_day >= 1380 || minute_of_day < 390 {
        if ["沉眠", "打瞌睡", "瞌睡", "哈欠", "困倦", "揉眼", "熬夜", "浴袍"]
            .iter()
            .any(|k| clip.id.contains(k))
        {
            return 3.0;
        }
        if ["深蹲", "冲刺", "跳绳", "开合跳", "蹦迪", "街舞"]
            .iter()
            .any(|k| clip.id.contains(k))
        {
            return 0.4;
        }
    }
    1.0
}

pub fn resolve_clip_for_emotion(
    character: &CharacterRuntime,
    bucket: crate::ai_voice::EmotionBucket,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> Option<ClipMeta> {
    use crate::ai_voice::EmotionBucket;
    let candidates: &[&str] = match bucket {
        EmotionBucket::Happy => &[
            "点击回应-开心跃动",
            "开心跃动",
            "工作状态-雀跃庆祝",
            "狂喜原地跳",
            "狂喜跃起",
            "开心蹦跳",
            "兴奋原地转",
            "大笑",
            "点击回应-挠痒咯咯笑",
            "挠痒咯咯笑",
            "鼓掌",
            "轻快摇摆舞",
        ],
        EmotionBucket::Shy => &[
            "点击回应-害羞惊讶",
            "点击回应-害羞惊讶 v2",
            "害羞惊讶",
            "害羞捂脸偷看",
            "害羞捂脸",
            "低头害羞",
            "尴尬抠手指",
            "遮眼偷看",
            "偷吃零食被抓住",
        ],
        EmotionBucket::Proud => &[
            "得意炫耀转圈",
            "骄傲昂首",
            "得意叉腰仰头",
            "得意炫耀",
            "点击回应-挠痒咯咯笑",
            "挠痒咯咯笑",
            "竖大拇指",
            "OK手势",
            "优雅裙摆舞",
            "优雅折扇舞",
            "优雅猫耳娘舞",
            "优雅女仆舞",
            "优雅提裙旋舞",
            "点击回应-开心跃动",
            "开心跃动",
        ],
        EmotionBucket::Angry => &[
            "点击回应-傲娇生气",
            "傲娇生气",
            "生气鼓腮跺脚",
            "生气鼓腮",
            "憋气鼓腮",
            "委屈赌气",
            "合扇敲掌·等待不耐烦",
            "玩游戏气急败坏",
            "无语扶额",
            "扶额无语",
            "嫉妒斜睨",
            "用金色双马尾拍打地面",
            "用蓬松紫色猫尾拍打地面",
            "用蓬松狐尾拍打地面",
            "斗篷披肩拍打地面",
        ],
        EmotionBucket::Sad => &[
            "心疼捂胸口",
            "感动抹泪",
            "委屈撇嘴",
            "哭泣",
            "沮丧垂头v2",
            "感动落泪",
            "工作状态-垂头叹气冒汗",
            "工作状态-垂头叹气冒汗 v2",
            "余额-数金皱眉",
            "余额-袋空如洗",
            "无语叹气",
            "双手摊开无奈",
        ],
        EmotionBucket::Surprised => &[
            "被吓一跳",
            "惊讶下巴掉",
            "惊讶瞪眼",
            "震惊僵住",
            "惊吓后缩",
            "紧张搓手抖",
            "小鹿受惊",
            "点击回应-害羞惊讶",
            "点击回应-害羞惊讶 v2",
            "害羞惊讶",
        ],
        EmotionBucket::Sleepy => &[
            "哈欠连天",
            "原地小憩沉眠",
            "沙发打瞌睡",
            "站着晃神打瞌睡",
            "待机打哈欠",
            "待机瞌睡点头",
            "困倦揉眼",
            "无聊打哈欠伸腰",
            "超大伸懒腰",
            "待机伸懒腰",
            "打瞌睡被惊醒",
            "打瞌睡被惊醒 v3",
        ],
        EmotionBucket::Thinking => &[
            "工作状态-思考冒泡",
            "深度思考碎碎念",
            "深度思考碎碎念v4",
            "深度思考走神",
            "换装侦探服思考",
            "咬手指思索",
            "疑惑挠头",
            "歪头好奇",
            "工作状态-清点归档",
            "碎碎念-发呆碎碎念",
            "碎碎念-发呆碎碎念 v3",
            "翻阅书册",
            "书灵现世",
            "拨算盘珠",
        ],
        EmotionBucket::Cheer => &[
            "握拳打气",
            "打气呐喊",
            "举荧光棒应援",
            "写代码",
            "工作状态-忙碌点按",
            "工作状态-雀跃庆祝",
            "点击回应-元气挥手",
            "元气挥手",
            "OK手势",
            "竖大拇指",
            "点击回应-开心跃动",
            "开心跃动",
        ],
        EmotionBucket::Affection => &[
            "比心双手头顶v2",
            "比心单手",
            "飞吻",
            "摘花送镜头",
            "点击回应-元气挥手",
            "元气挥手",
            "挥手双手",
            "淑女屈膝礼",
            "提裙屈膝礼仪",
            "猫耳娘屈膝礼仪",
            "女仆屈膝礼仪",
            "感激鞠躬",
            "鞠躬轻",
            "温柔注视",
            "点击回应-开心跃动",
            "开心跃动",
        ],
    };

    let matched: Vec<&ClipMeta> = candidates
        .iter()
        .filter_map(|name| character.clip_map.get(*name).filter(|c| c.playable()))
        .collect();
    if matched.is_empty() {
        return None;
    }
    let weights: Vec<f64> = matched
        .iter()
        .enumerate()
        .map(|(idx, c)| {
            let prio = if idx < 3 { 1.35 } else { 1.0 };
            prio * selection_weight(history.get(&c.id).copied(), now_ms)
        })
        .collect();
    pick_weighted_value(&matched, &weights, rng.next_f64()).map(|c| c.clone())
}

/// 待机选段（融合 2h 去重 × 本地节日/四季时令加权 × API 额度晴雨表余额档位）。
pub fn pick_idle_with_context(
    character: &CharacterRuntime,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
    exclude_id: Option<&str>,
    quota_pct: Option<f64>,
) -> Option<ClipMeta> {
    let date_key = crate::stats::local_date_key(now_ms);
    let mut pool: Vec<&ClipMeta> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(ids) = character.bindings.get("idle") {
        for id in ids {
            if let Some(clip) = character.clip_map.get(id).filter(|c| c.playable() && !is_work_clip(c)) {
                if seen.insert(clip.id.as_str()) {
                    pool.push(clip);
                }
            }
        }
    }

    for clip in &character.clips {
        if !clip.playable() || is_work_clip(clip) {
            continue;
        }
        let fest_w = festival_and_season_weight(&clip.id, &clip.category, date_key);
        let quota_w = quota_balance_idle_weight(&clip.id, quota_pct);
        if (fest_w > 1.0 || quota_w > 1.0) && seen.insert(clip.id.as_str()) {
            pool.push(clip);
        }
    }

    if pool.is_empty() {
        return pick_for_binding_excluding(character, "idle", history, now_ms, rng, exclude_id);
    }

    if let Some(ex) = exclude_id {
        if pool.len() > 1 {
            let filtered: Vec<&ClipMeta> = pool.iter().copied().filter(|c| c.id != ex).collect();
            if !filtered.is_empty() {
                pool = filtered;
            }
        }
    }

    let weights: Vec<f64> = pool
        .iter()
        .map(|clip| {
            let base = selection_weight(history.get(&clip.id).copied(), now_ms);
            let fest = festival_and_season_weight(&clip.id, &clip.category, date_key);
            let quota = quota_balance_idle_weight(&clip.id, quota_pct);
            base * fest * quota
        })
        .collect();
    pick_weighted_value(&pool, &weights, rng.next_f64()).map(|clip| clip.clone())
}

/// 按 category 选段（聊天里的「跳舞」「夸奖」等）。
pub fn pick_by_category(
    character: &CharacterRuntime,
    categories: &[&str],
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> Option<ClipMeta> {
    if categories.is_empty() {
        return None;
    }
    let candidates: Vec<&ClipMeta> = character
        .clips
        .iter()
        .filter(|clip| clip.playable() && categories.contains(&clip.category.as_str()))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let weights: Vec<f64> = candidates
        .iter()
        .map(|clip| selection_weight(history.get(&clip.id).copied(), now_ms))
        .collect();
    pick_weighted_value(&candidates, &weights, rng.next_f64()).map(|clip| clip.clone())
}

pub fn request_from_clip(clip: &ClipMeta, priority: Priority, mirror: bool) -> PlayRequest {
    PlayRequest {
        clip_id: clip.id.clone(),
        priority,
        mirror,
        audio: clip.audio.clone(),
        frames: clip.frames,
        fps: clip.effective_fps(),
        duration_ms: clip.effective_duration_ms(),
        interrupt_after_ms: clip.interrupt_after_ms,
        looped: clip.looped,
        hold_loop: false,
    }
}

fn find_first_playable(character: &CharacterRuntime, names: &[&str]) -> Option<ClipMeta> {
    for name in names {
        if let Some(clip) = character.clip_map.get(*name).filter(|c| c.playable()) {
            return Some(clip.clone());
        }
    }
    None
}

/// 按 Antigravity 全生命周期精细场景 + 8 位角色真实素材库存，精确解析目标动画片段。
///
/// 覆盖第 1–8 组共 26 态；缺特定片段时按「特定语义名 -> 通用同义段 -> 角色绑定池」优雅降级。
pub fn resolve_clip_for_scene(
    character: &CharacterRuntime,
    activity: AgentActivityKind,
    history: &HashMap<String, i64>,
    now_ms: i64,
    rng: &mut Rng,
) -> Option<ClipMeta> {
    let candidates: &[&str] = match activity {
        AgentActivityKind::ProcessOffline => &[
            "原地小憩沉眠",
            "沙发打瞌睡",
            "待机瞌睡点头",
            "困倦揉眼",
        ],
        AgentActivityKind::ServiceConnecting => &[
            "超大伸懒腰",
            "打瞌睡被惊醒",
            "打瞌睡被惊醒 v3",
            "待机伸懒腰",
            "变身亮相",
        ],
        AgentActivityKind::ServiceReconnecting => &[
            "东张西望",
            "左右张望",
            "站着晃神打瞌睡",
            "焦虑踱步",
        ],
        AgentActivityKind::SessionInit => &[
            "点击回应-元气挥手",
            "书灵现世",
            "星星精灵现世",
            "小蓝狐现世",
            "小金蝠现世",
            "小星灵现世",
            "收拾书桌 v2",
            "挥手双手",
            "招手过来",
        ],
        AgentActivityKind::PlanningMode => &[
            "工作状态-清点归档",
            "轻快记录",
            "拨算盘珠",
            "写字哑剧",
            "翻阅书册",
            "收拾书桌 v2",
            "写代码",
        ],
        AgentActivityKind::TtftWaiting | AgentActivityKind::DeepReasoning => &[
            "工作状态-思考冒泡",
            "深度思考碎碎念",
            "深度思考碎碎念v4",
            "深度思考走神",
            "换装侦探服思考",
            "咬手指思索",
            "疑惑挠头",
            "写代码",
        ],
        AgentActivityKind::ResearchLookup => &[
            "工作状态-清点归档",
            "书灵现世",
            "翻阅书册",
            "图书管理员嘘",
            "东张西望",
            "轻快记录",
            "写代码",
        ],
        AgentActivityKind::CodeStreaming => &[
            "写代码",
            "打字哑剧",
            "工作加班",
            "工作状态-忙碌点按",
            "轻快记录",
        ],
        AgentActivityKind::CodeDiffing => &[
            "轻快记录",
            "工作状态-忙碌点按",
            "写字哑剧",
            "理发师剪发",
            "写代码",
        ],
        AgentActivityKind::TerminalRunning => &[
            "工作状态-忙碌点按",
            "碎碎念-对屏碎碎念",
            "工作加班",
            "打字哑剧",
            "写代码",
        ],
        AgentActivityKind::BuildLongRunning => &[
            "工作状态-原地踱步张望",
            "工作状态-原地踱步张望 v2",
            "焦虑踱步",
            "打瞌睡被惊醒",
            "打瞌睡被惊醒 v3",
            "吃Token",
            "工作加班",
        ],
        AgentActivityKind::TerminalError => &[
            "工作状态-垂头叹气冒汗",
            "工作状态-垂头叹气冒汗 v2",
            "被吓一跳",
            "惊讶下巴掉",
            "惊吓后缩",
            "无语扶额",
            "扶额无语",
        ],
        AgentActivityKind::BrowserRunning => &[
            "换装侦探服思考",
            "侧身举相机拍别人",
            "拍照取景",
            "工作状态-忙碌点按",
            "碎碎念-对屏碎碎念",
            "工作状态-原地踱步张望",
            "工作状态-原地踱步张望 v2",
            "写代码",
        ],
        AgentActivityKind::McpToolCalling => &[
            "书灵现世",
            "星星精灵现世",
            "小蓝狐现世",
            "小金蝠现世",
            "小星灵现世",
            "召唤姿势",
            "施法手势",
            "打电话汇报",
            "工作状态-忙碌点按",
        ],
        AgentActivityKind::WorkflowRunning => &[
            "轻快记录",
            "工作状态-忙碌点按",
            "工作状态-清点归档",
            "召唤姿势",
            "工作加班",
            "写代码",
        ],
        AgentActivityKind::ApprovalTerminal => &[
            "招手过来",
            "挥手双手",
            "打电话汇报",
            "点击回应-元气挥手",
            "点击回应-开心跃动",
        ],
        AgentActivityKind::ApprovalOutsideDir => &[
            "保安举金属探测仪",
            "警察开罚单",
            "慌张摆手",
            "合扇敲掌·等待不耐烦",
            "工作状态-原地踱步张望",
            "工作状态-原地踱步张望 v2",
            "点击回应-傲娇生气",
        ],
        AgentActivityKind::ArtifactReviewReady => &[
            "工作状态-清点归档",
            "工作状态-雀跃庆祝",
            "书灵现世",
            "打电话汇报",
            "OK手势",
            "点击回应-元气挥手",
        ],
        AgentActivityKind::TaskCompleted => &[
            "工作状态-雀跃庆祝",
            "点击回应-开心跃动",
            "狂喜原地跳",
            "得意炫耀转圈",
            "狂喜跃起",
            "鼓掌",
            "OK手势",
        ],
        AgentActivityKind::ContextDeltaSpike => &[
            "被吓一跳",
            "惊讶下巴掉",
            "震惊僵住",
            "惊吓后缩",
        ],
        AgentActivityKind::ContextWarning80 => &[
            "工作状态-垂头叹气冒汗",
            "工作状态-垂头叹气冒汗 v2",
            "吃Token",
            "玩游戏气急败坏",
            "无语扶额",
            "扶额无语",
        ],
        AgentActivityKind::QuotaLowWarning => &[
            "余额-袋空如洗",
            "余额-数金皱眉",
            "余额-分文不剩",
            "吃Token",
            "尴尬抠手指",
            "委屈撇嘴",
        ],
        AgentActivityKind::QuotaExhausted => &[
            "余额-分文不剩",
            "余额-袋空如洗",
            "原地小憩沉眠",
            "沙发打瞌睡",
            "沮丧垂头v2",
        ],
        AgentActivityKind::ApiAuthRateError => &[
            "工作状态-垂头叹气冒汗",
            "工作状态-垂头叹气冒汗 v2",
            "被吓一跳",
            "惊吓后缩",
            "慌张摆手",
            "警察开罚单",
        ],
        AgentActivityKind::Idle => &[],
    };

    if let Some(clip) = find_first_playable(character, candidates) {
        return Some(clip);
    }

    let fallback_binding = match activity {
        AgentActivityKind::ProcessOffline | AgentActivityKind::QuotaExhausted => "sleep",
        AgentActivityKind::ServiceConnecting => "wake",
        AgentActivityKind::ApprovalTerminal
        | AgentActivityKind::ApprovalOutsideDir
        | AgentActivityKind::ArtifactReviewReady
        | AgentActivityKind::ApiAuthRateError
        | AgentActivityKind::ContextWarning80
        | AgentActivityKind::QuotaLowWarning
        | AgentActivityKind::ContextDeltaSpike
        | AgentActivityKind::TaskCompleted => "click",
        AgentActivityKind::Idle
        | AgentActivityKind::SessionInit
        | AgentActivityKind::ServiceReconnecting => "idle",
        _ => "work",
    };
    pick_for_binding(character, fallback_binding, history, now_ms, rng)
}

/* ------------------------------------------------------------------ */
/* 状态机                                                              */
/* ------------------------------------------------------------------ */

pub struct SchedulerCore {
    character: Option<CharacterRuntime>,
    current: Option<Current>,
    pending: Option<PlayRequest>,
    history: HashMap<String, i64>,
    rng: Rng,
    work_mode: bool,
    agent_active: bool,
    work_phase: WorkPhase,
    work_playlist_idx: usize,
    alert_cooldowns: HashMap<String, i64>,
    dragging: bool,
    active_scene: Option<AgentSceneSignal>,
    last_scene_switch_ms: i64,
    pending_scene: Option<AgentSceneSignal>,
    quota_remaining_pct: Option<f64>,
    click_timestamps: Vec<i64>,
    last_angry_ms: Option<i64>,
    angry_voice_toggle: bool,
    last_ambient_tier: Option<AmbientTier>,
    last_ambient_category: Option<String>,
    consecutive_same_tier: u8,
}

impl SchedulerCore {
    pub fn new(seed: u64) -> Self {
        Self {
            character: None,
            current: None,
            pending: None,
            history: HashMap::new(),
            rng: Rng::new(seed),
            work_mode: false,
            agent_active: false,
            work_phase: WorkPhase::None,
            work_playlist_idx: 0,
            alert_cooldowns: HashMap::new(),
            dragging: false,
            active_scene: None,
            last_scene_switch_ms: 0,
            pending_scene: None,
            quota_remaining_pct: None,
            click_timestamps: Vec::new(),
            last_angry_ms: None,
            angry_voice_toggle: false,
            last_ambient_tier: None,
            last_ambient_category: None,
            consecutive_same_tier: 0,
        }
    }

    pub fn set_character(&mut self, character: CharacterRuntime, history: HashMap<String, i64>) {
        self.character = Some(character);
        self.current = None;
        self.pending = None;
        self.history = history;
        self.work_mode = false;
        self.agent_active = false;
        self.work_phase = WorkPhase::None;
        self.work_playlist_idx = 0;
        self.alert_cooldowns.clear();
        self.dragging = false;
        self.active_scene = None;
        self.last_scene_switch_ms = 0;
        self.pending_scene = None;
        self.click_timestamps.clear();
        self.last_angry_ms = None;
        self.angry_voice_toggle = false;
        self.last_ambient_tier = None;
        self.last_ambient_category = None;
        self.consecutive_same_tier = 0;
    }

    #[allow(dead_code)]
    pub fn current_clip(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|current| current.clip_id.as_str())
    }

    #[allow(dead_code)]
    pub fn is_work_mode(&self) -> bool {
        self.work_mode
    }

    pub fn on_event(&mut self, event: SchedulerEvent, now_ms: i64) -> Vec<Action> {
        match event {
            SchedulerEvent::AppReady => {
                if self.current.is_some() {
                    return Vec::new();
                }
                self.begin_binding("idle", Priority::Idle, now_ms, false)
            }
            SchedulerEvent::UserClick { .. } => {
                self.click_timestamps
                    .retain(|&ts| now_ms >= ts && now_ms - ts <= RAPID_CLICK_WINDOW_MS);
                self.click_timestamps.push(now_ms);
                if self.click_timestamps.len() < RAPID_CLICK_THRESHOLD {
                    return Vec::new();
                }
                if self
                    .last_angry_ms
                    .is_some_and(|last| now_ms >= last && now_ms - last < ANGRY_COOLDOWN_MS)
                {
                    return Vec::new();
                }
                let Some(character) = self.character.as_ref() else {
                    return Vec::new();
                };
                let Some(clip) = resolve_angry_clip(character, &self.history, now_ms, &mut self.rng)
                else {
                    return Vec::new();
                };
                self.click_timestamps.clear();
                self.last_angry_ms = Some(now_ms);
                let mirror = clip.mirrorable && self.rng.next_f64() < 0.35;
                let mut request = request_from_clip(&clip, Priority::Click, mirror);
                request.looped = false;
                request.hold_loop = false;
                let mut actions = self.try_start(request, now_ms, false);
                if actions.iter().any(|a| matches!(a, Action::Play(_))) {
                    self.angry_voice_toggle = !self.angry_voice_toggle;
                    let scene_key = if self.angry_voice_toggle {
                        "angry_1"
                    } else {
                        "angry_2"
                    };
                    actions.push(Action::Voice {
                        scene_key: scene_key.to_string(),
                        default_bubble: None,
                        ttl_ms: 4_200,
                        force: false,
                    });
                }
                actions
            }
            SchedulerEvent::UserDragStart { .. } => {
                self.dragging = true;
                // 契约硬条款：DRAG 立即打断，不看 interruptAfterMs。
                self.begin_binding("drag", Priority::Drag, now_ms, true)
            }
            SchedulerEvent::UserDragEnd { .. } => {
                self.dragging = false;
                self.resume_or_ambient(now_ms)
            }
            SchedulerEvent::NeedHungry => {
                let mut actions = self.begin_binding("eat", Priority::Schedule, now_ms, false);
                actions.push(Action::Bubble {
                    text: "肚子饿了…有吃的吗？".to_string(),
                    ttl_ms: 3000,
                });
                actions
            }
            SchedulerEvent::NeedSleepy => {
                let mut actions = self.begin_binding("sleep", Priority::Schedule, now_ms, false);
                actions.push(Action::Bubble {
                    text: "困了…先眯一会儿。".to_string(),
                    ttl_ms: 3000,
                });
                actions
            }
            SchedulerEvent::ScheduleTick { slot } => {
                let key = match slot.as_str() {
                    "wake" => "wake",
                    "lunch" | "dinner" => "eat",
                    "sleep" => "sleep",
                    _ => "idle",
                };
                self.begin_binding(key, Priority::Schedule, now_ms, false)
            }
            SchedulerEvent::WorkAppFocus | SchedulerEvent::WorkAppBlur => {
                // 仅在 Agent 真正工作时才进入工作动画；切到 IDE/终端不随机播工作段。
                Vec::new()
            }
            SchedulerEvent::ChatReply { clip_id } | SchedulerEvent::PlayCommand { clip_id } => {
                self.begin_specific(&clip_id, Priority::Command, now_ms)
            }
            SchedulerEvent::AgentActivity(status) => {
                let signal = AgentSceneSignal {
                    status,
                    activity: AgentActivityKind::from_status(status),
                    detail: None,
                    bubble: None,
                };
                self.on_agent_scene(signal, now_ms)
            }
            SchedulerEvent::AgentScene(signal) => self.on_agent_scene(signal, now_ms),
            SchedulerEvent::AgentAlert { key, bubble } => {
                self.trigger_agent_alert(&key, bubble, now_ms)
            }
            SchedulerEvent::AgentBubble { text, ttl_ms } => {
                vec![Action::Bubble { text, ttl_ms }]
            }
            SchedulerEvent::QuotaBarometer {
                remaining_pct,
                announce,
            } => {
                self.quota_remaining_pct = Some(remaining_pct);
                if !announce || self.dragging || self.agent_active {
                    return Vec::new();
                }
                let Some(character) = self.character.as_ref() else {
                    return Vec::new();
                };
                let (cands, bubble_text) = quota_barometer_candidates(remaining_pct);
                let Some(clip) = find_first_playable(character, cands) else {
                    return Vec::new();
                };
                let request = request_from_clip(&clip, Priority::Schedule, false);
                let mut actions = self.try_start(request, now_ms, false);
                actions.push(Action::Bubble {
                    text: bubble_text,
                    ttl_ms: 4_500,
                });
                actions
            }
            SchedulerEvent::ClipEnded { clip_id } => self.on_clip_ended(clip_id, now_ms),
            SchedulerEvent::CharacterChanged(_) => Vec::new(),
        }
    }

    fn scene_priority(signal: &AgentSceneSignal) -> Priority {
        match signal.activity {
            AgentActivityKind::ApprovalTerminal
            | AgentActivityKind::ApprovalOutsideDir
            | AgentActivityKind::ArtifactReviewReady
            | AgentActivityKind::ApiAuthRateError => Priority::AgentAlert,
            AgentActivityKind::ProcessOffline
            | AgentActivityKind::Idle
            | AgentActivityKind::QuotaExhausted => Priority::Idle,
            AgentActivityKind::TaskCompleted => Priority::Schedule,
            _ => Priority::ForegroundWork,
        }
    }

    fn start_thinking_intro(&mut self, now_ms: i64) -> Vec<Action> {
        let Some(character) = self.character.as_ref() else {
            return Vec::new();
        };
        let Some(clip) = resolve_clip_for_scene(
            character,
            AgentActivityKind::DeepReasoning,
            &self.history,
            now_ms,
            &mut self.rng,
        ) else {
            return Vec::new();
        };
        let mut request = request_from_clip(&clip, Priority::ForegroundWork, false);
        request.hold_loop = false;
        request.looped = false;
        let force = !self.dragging
            && self
                .current
                .as_ref()
                .is_none_or(|c| c.priority < Priority::Command);
        let played =
            self.try_start_with_scene(request, Some(AgentActivityKind::DeepReasoning), now_ms, force);
        if played.iter().any(|a| matches!(a, Action::Play(_))) {
            self.last_scene_switch_ms = now_ms;
        }
        played
    }

    fn play_work_playlist_at(
        &mut self,
        index: usize,
        activity_hint: AgentActivityKind,
        now_ms: i64,
    ) -> Vec<Action> {
        let Some(character) = self.character.as_ref() else {
            return Vec::new();
        };
        let playlist = build_work_playlist(character);
        if playlist.is_empty() {
            return self.begin_binding("work", Priority::ForegroundWork, now_ms, false);
        }
        let actual_idx = index % playlist.len();
        let clip = playlist[actual_idx].clone();
        self.work_playlist_idx = (actual_idx + 1) % playlist.len();

        let mut request = request_from_clip(&clip, Priority::ForegroundWork, false);
        if playlist.len() == 1 {
            request.hold_loop = true;
            request.looped = true;
        } else {
            request.hold_loop = false;
            request.looped = false;
            if request.duration_ms > MAX_PLAYLIST_CLIP_MS {
                request.duration_ms = MAX_PLAYLIST_CLIP_MS;
            }
        }
        let force = !self.dragging
            && self
                .current
                .as_ref()
                .is_none_or(|c| c.priority < Priority::Command);
        let played = self.try_start_with_scene(request, Some(activity_hint), now_ms, force);
        if played.iter().any(|a| matches!(a, Action::Play(_))) {
            if let Some(cur) = self.current.as_mut() {
                cur.in_work_playlist = true;
            }
            self.last_scene_switch_ms = now_ms;
        }
        played
    }

    fn start_work_playlist_for_activity(
        &mut self,
        activity: AgentActivityKind,
        now_ms: i64,
    ) -> Vec<Action> {
        let idx = match self.character.as_ref() {
            Some(character) => {
                let playlist = build_work_playlist(character);
                if playlist.is_empty() {
                    0
                } else {
                    find_playlist_index_for_activity(
                        character,
                        &playlist,
                        activity,
                        &self.history,
                        now_ms,
                        &mut self.rng,
                    )
                }
            }
            None => 0,
        };
        self.play_work_playlist_at(idx, activity, now_ms)
    }

    fn play_next_work_playlist_clip(&mut self, now_ms: i64) -> Vec<Action> {
        self.play_work_playlist_at(self.work_playlist_idx, AgentActivityKind::CodeStreaming, now_ms)
    }

    /// v2.2：Antigravity 全生命周期状态调度：
    /// - 常规工作流：首次进入先播 1 次思考开场（可随时被后续动作即时打断），随后按顺序循环播放精选工作队列；
    /// - 低频特色场景（网页搜索/浏览器/MCP/工作流/长构建/命令报错）与 L1 审批保持精准触发；
    /// - 离线睡觉、额度冷却、L1 审批等长驻态继续使用 `hold_loop` 锁定。
    fn on_agent_scene(&mut self, signal: AgentSceneSignal, now_ms: i64) -> Vec<Action> {
        if signal.activity == AgentActivityKind::TtftWaiting {
            return Vec::new();
        }

        let was_active = self.agent_active;

        match signal.status {
            TelemetryStatus::Streaming
            | TelemetryStatus::ToolRunning
            | TelemetryStatus::WaitingApproval
            | TelemetryStatus::Error => {
                self.agent_active = true;
                self.work_mode = true;
            }
            TelemetryStatus::Idle | TelemetryStatus::Offline => {
                self.agent_active = false;
                self.work_mode = false;
                self.work_phase = WorkPhase::None;
                self.work_playlist_idx = 0;
                if matches!(
                    signal.activity,
                    AgentActivityKind::Idle | AgentActivityKind::ProcessOffline
                ) && !was_active
                    && self.active_scene.is_none()
                {
                    return Vec::new();
                }
            }
        }

        if matches!(
            signal.activity,
            AgentActivityKind::Idle | AgentActivityKind::ProcessOffline
        ) {
            self.active_scene = None;
            self.pending_scene = None;
            return self.begin_binding("idle", Priority::Idle, now_ms, true);
        }

        // 路径 A：常规高频工作流（思考开场 -> 工作顺序队列轮播）
        if is_playlist_work_signal(&signal) {
            let in_specialty = self.current.as_ref().is_some_and(|c| {
                !c.in_work_playlist
                    && c.scene.is_some_and(|s| {
                        s != AgentActivityKind::DeepReasoning
                            && matches!(
                                s,
                                AgentActivityKind::BrowserRunning
                                    | AgentActivityKind::McpToolCalling
                                    | AgentActivityKind::WorkflowRunning
                                    | AgentActivityKind::BuildLongRunning
                                    | AgentActivityKind::ResearchLookup
                            )
                    })
            });
            if in_specialty
                && self.last_scene_switch_ms > 0
                && now_ms - self.last_scene_switch_ms < MIN_SCENE_DWELL_MS
            {
                self.pending_scene = Some(signal);
                return Vec::new();
            }

            self.pending_scene = None;
            self.active_scene = None;

            return match self.work_phase {
                WorkPhase::None => {
                    if signal.activity == AgentActivityKind::DeepReasoning {
                        self.work_phase = WorkPhase::ThinkingIntro;
                        self.start_thinking_intro(now_ms)
                    } else {
                        self.work_phase = WorkPhase::Playlist;
                        self.start_work_playlist_for_activity(signal.activity, now_ms)
                    }
                }
                WorkPhase::ThinkingIntro => {
                    if signal.activity == AgentActivityKind::DeepReasoning {
                        Vec::new()
                    } else {
                        self.work_phase = WorkPhase::Playlist;
                        self.start_work_playlist_for_activity(signal.activity, now_ms)
                    }
                }
                WorkPhase::Playlist => {
                    if self.current.as_ref().is_some_and(|c| c.in_work_playlist) {
                        Vec::new()
                    } else {
                        self.play_next_work_playlist_clip(now_ms)
                    }
                }
            };
        }

        // 路径 B：低频特色场景 / L1 审批与报错 / 离线 / 瞬态
        if self.work_phase == WorkPhase::ThinkingIntro {
            self.work_phase = WorkPhase::Playlist;
        }

        let is_l1 = signal.activity.tier() == 1;
        if signal.activity.is_held() {
            self.active_scene = Some(signal.clone());
        } else if !signal.status.is_agent_active() {
            self.active_scene = None;
        }

        if !is_l1 && !is_specialty_work_signal(&signal) && self.last_scene_switch_ms > 0 {
            let since_switch = now_ms - self.last_scene_switch_ms;
            let cur_tier = self
                .current
                .as_ref()
                .and_then(|c| c.scene)
                .map(AgentActivityKind::tier)
                .unwrap_or(5);
            if since_switch < MIN_SCENE_DWELL_MS && signal.activity.tier() >= cur_tier {
                self.pending_scene = Some(signal);
                return Vec::new();
            }
        }
        self.pending_scene = None;
        self.apply_agent_scene(signal, now_ms)
    }

    fn apply_agent_scene(&mut self, signal: AgentSceneSignal, now_ms: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        let priority = Self::scene_priority(&signal);

        if matches!(
            signal.activity,
            AgentActivityKind::ApprovalTerminal
                | AgentActivityKind::ApprovalOutsideDir
                | AgentActivityKind::ArtifactReviewReady
                | AgentActivityKind::ApiAuthRateError
        ) {
            let (key, default_text) = match signal.activity {
                AgentActivityKind::ApprovalOutsideDir => (
                    "approval_outside",
                    "Agent 请求访问工作区外路径，去 IDE 放行吧。",
                ),
                AgentActivityKind::ArtifactReviewReady => (
                    "artifact_review",
                    "方案或演示成果已就绪，等你批注。",
                ),
                AgentActivityKind::ApiAuthRateError => (
                    "agent_error",
                    "Antigravity 服务鉴权或限流异常，去查看一下吧。",
                ),
                _ => (
                    "waiting_approval",
                    "Agent 停在等待确认了，去点一下「允许」吧。",
                ),
            };
            let cooled = self
                .alert_cooldowns
                .get(key)
                .map(|last| (0..AGENT_ALERT_COOLDOWN_MS).contains(&(now_ms - *last)))
                .unwrap_or(false);
            if !cooled {
                self.alert_cooldowns.insert(key.to_string(), now_ms);
                let text = signal
                    .bubble
                    .clone()
                    .unwrap_or_else(|| default_text.to_string());
                actions.push(Action::Bubble { text, ttl_ms: 6_000 });
            } else if signal.activity == AgentActivityKind::ApiAuthRateError {
                // 原版 60s 冷却期间不重复播警报段（仅维持当前工作态）
                if self.current.as_ref().is_some_and(|c| c.hold_loop) {
                    return actions;
                }
            }
        } else if let Some(text) = signal.bubble.clone() {
            actions.push(Action::Bubble { text, ttl_ms: 4_000 });
        }

        let Some(character) = self.character.as_ref() else {
            return actions;
        };
        let Some(clip) =
            resolve_clip_for_scene(character, signal.activity, &self.history, now_ms, &mut self.rng)
        else {
            return actions;
        };

        let held = signal.activity.is_held();
        // 同段保活：当前已经在循环播同一段且均处于锁定态，只刷新场景标签，不重载视频！
        if held {
            if let Some(cur) = self.current.as_mut() {
                if cur.hold_loop && cur.clip_id == clip.id && !self.dragging {
                    cur.scene = Some(signal.activity);
                    cur.priority = priority;
                    return actions;
                }
            }
        }

        let mut request = request_from_clip(&clip, priority, false);
        request.hold_loop = held;
        request.looped = held;
        let force = !self.dragging
            && priority < Priority::Click
            && self
                .current
                .as_ref()
                .is_none_or(|c| c.priority < Priority::Command);
        let played = self.try_start_with_scene(request, Some(signal.activity), now_ms, force);
        if played.iter().any(|a| matches!(a, Action::Play(_))) {
            self.last_scene_switch_ms = now_ms;
        }
        actions.extend(played);
        actions
    }

    /// 同类告警 60s 内只提醒一次；不打断 DRAG/CLICK（优先级 50 < 80/100）。
    fn trigger_agent_alert(&mut self, key: &str, bubble: String, now_ms: i64) -> Vec<Action> {
        if let Some(last) = self.alert_cooldowns.get(key) {
            let elapsed = now_ms - *last;
            if (0..AGENT_ALERT_COOLDOWN_MS).contains(&elapsed) {
                return Vec::new();
            }
        }
        self.alert_cooldowns.insert(key.to_string(), now_ms);
        let mut actions = self.begin_binding("click", Priority::AgentAlert, now_ms, false);
        actions.push(Action::Bubble {
            text: bubble,
            ttl_ms: 6_000,
        });
        actions
    }

    /// Rust 计时器每 100ms 调用：负责按 `meta.durationMs` 收段，并处理排队抢占与最小驻留补偿。
    pub fn tick(&mut self, now_ms: i64) -> Vec<Action> {
        if let Some(pending_scene) = self.pending_scene.clone() {
            if now_ms - self.last_scene_switch_ms >= MIN_SCENE_DWELL_MS {
                self.pending_scene = None;
                return self.on_agent_scene(pending_scene, now_ms);
            }
        }

        if let Some(current) = &self.current {
            if !current.hold_loop
                && current.duration_ms > 0
                && now_ms - current.started_ms >= current.duration_ms as i64
            {
                let clip_id = current.clip_id.clone();
                return self.on_clip_ended(Some(clip_id), now_ms);
            }
        }

        if let Some(pending) = self.pending.clone() {
            match &self.current {
                Some(current)
                    if now_ms - current.started_ms >= current.interrupt_after_ms as i64 =>
                {
                    self.pending = None;
                    return self.start_with_scene(pending, None, now_ms);
                }
                None => {
                    self.pending = None;
                    return self.start_with_scene(pending, None, now_ms);
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn on_clip_ended(&mut self, clip_id: Option<String>, now_ms: i64) -> Vec<Action> {
        let Some(current) = self.current.as_ref() else {
            return Vec::new();
        };
        if let Some(expected) = clip_id.as_deref() {
            if expected != current.clip_id {
                return Vec::new();
            }
            // frame_hint 驱动的收段做防抖：同一 clip 循环重播时，上一轮的末帧
            // 可能晚到，不能把刚开始的新一轮直接判定结束。
            if current.duration_ms > 0
                && now_ms - current.started_ms < (current.duration_ms / 2) as i64
            {
                return Vec::new();
            }
        }
        // 状态锁定循环段（hold_loop）：前端持续原地循环，绝不因 10 秒到期而自己切换或掉回 idle。
        if current.hold_loop {
            return Vec::new();
        }
        let should_replay = match current.priority {
            Priority::Drag => self.dragging,
            Priority::ForegroundWork => current.looped && !self.dragging,
            // 待机、点击、聊天指令、告警等一次性段播完即轮换下一段，绝不无限循环锁死。
            Priority::Idle
            | Priority::Click
            | Priority::Command
            | Priority::AgentAlert
            | Priority::Schedule => false,
        };
        let replay = if should_replay {
            Some((current.to_request(), current.scene))
        } else {
            None
        };
        self.current = None;

        if let Some(pending) = self.pending.take() {
            return self.start_with_scene(pending, None, now_ms);
        }
        if let Some((request, scene)) = replay {
            return self.start_with_scene(request, scene, now_ms);
        }
        self.resume_or_ambient(now_ms)
    }

    fn resume_or_ambient(&mut self, now_ms: i64) -> Vec<Action> {
        if self.dragging {
            return self.begin_binding("drag", Priority::Drag, now_ms, true);
        }
        if let Some(signal) = self.active_scene.clone() {
            if signal.activity.is_held() {
                return self.apply_agent_scene(signal, now_ms);
            }
        }
        if self.agent_active {
            if self.work_phase != WorkPhase::Playlist {
                self.work_phase = WorkPhase::Playlist;
            }
            return self.play_next_work_playlist_clip(now_ms);
        }
        self.ambient(now_ms)
    }

    fn ambient(&mut self, now_ms: i64) -> Vec<Action> {
        if self.dragging {
            return self.begin_binding("drag", Priority::Drag, now_ms, true);
        }
        if self.agent_active && self.work_mode && self.rng.next_f64() < 0.75 {
            let actions = self.begin_binding("work", Priority::ForegroundWork, now_ms, true);
            if !actions.is_empty() {
                return actions;
            }
        }
        self.begin_binding("idle", Priority::Idle, now_ms, true)
    }

    fn pick_ambient_breathing(&mut self, now_ms: i64, exclude_id: Option<&str>) -> Option<ClipMeta> {
        let character = self.character.as_ref()?;
        let date_key = crate::stats::local_date_key(now_ms);
        let minute_of_day = crate::stats::local_minute_of_day(now_ms);

        let mut tier1: Vec<&ClipMeta> = Vec::new();
        let mut tier2: Vec<&ClipMeta> = Vec::new();
        let mut tier3: Vec<&ClipMeta> = Vec::new();
        let mut has_active_festival = false;

        for clip in &character.clips {
            let Some(tier) = classify_ambient_tier(clip, &character.bindings.idle) else {
                continue;
            };
            if festival_and_season_weight(&clip.id, &clip.category, date_key) >= 5.0 {
                has_active_festival = true;
            }
            match tier {
                AmbientTier::Rest => tier1.push(clip),
                AmbientTier::DailyPlayEat => tier2.push(clip),
                AmbientTier::TalentEaster => tier3.push(clip),
            }
        }

        if tier1.is_empty() && tier2.is_empty() && tier3.is_empty() {
            return pick_idle_with_context(
                character,
                &self.history,
                now_ms,
                &mut self.rng,
                exclude_id,
                self.quota_remaining_pct,
            );
        }

        let mut tier_weights = time_of_day_tier_weights(minute_of_day, has_active_festival);
        match self.last_ambient_tier {
            Some(AmbientTier::TalentEaster) => tier_weights[2] *= 0.25,
            Some(AmbientTier::DailyPlayEat) => tier_weights[1] *= 0.55,
            Some(AmbientTier::Rest) if self.consecutive_same_tier >= 3 => {
                tier_weights[1] *= 1.6;
                tier_weights[2] *= 1.6;
            }
            _ => {}
        }
        if tier1.is_empty() {
            tier_weights[0] = 0.0;
        }
        if tier2.is_empty() {
            tier_weights[1] = 0.0;
        }
        if tier3.is_empty() {
            tier_weights[2] = 0.0;
        }

        let tiers = [
            AmbientTier::Rest,
            AmbientTier::DailyPlayEat,
            AmbientTier::TalentEaster,
        ];
        let chosen_tier = pick_weighted_value(&tiers, &tier_weights, self.rng.next_f64())
            .unwrap_or(AmbientTier::Rest);
        let mut pool = match chosen_tier {
            AmbientTier::Rest => tier1,
            AmbientTier::DailyPlayEat => tier2,
            AmbientTier::TalentEaster => tier3,
        };
        if let Some(ex) = exclude_id {
            if pool.len() > 1 {
                let filtered: Vec<&ClipMeta> = pool.iter().copied().filter(|c| c.id != ex).collect();
                if !filtered.is_empty() {
                    pool = filtered;
                }
            }
        }

        let distinct_cats = pool
            .iter()
            .map(|c| c.category.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let weights: Vec<f64> = pool
            .iter()
            .map(|clip| {
                let base = selection_weight(self.history.get(&clip.id).copied(), now_ms);
                let fest = festival_and_season_weight(&clip.id, &clip.category, date_key);
                let quota = quota_balance_idle_weight(&clip.id, self.quota_remaining_pct);
                let tod = time_of_day_clip_weight(clip, minute_of_day);
                let cat_pen = if distinct_cats > 1
                    && self
                        .last_ambient_category
                        .as_deref()
                        .is_some_and(|cat| cat == clip.category)
                {
                    0.25
                } else {
                    1.0
                };
                base * fest * quota * tod * cat_pen
            })
            .collect();

        let picked = pick_weighted_value(&pool, &weights, self.rng.next_f64())?.clone();
        if self.last_ambient_tier == Some(chosen_tier) {
            self.consecutive_same_tier = self.consecutive_same_tier.saturating_add(1);
        } else {
            self.last_ambient_tier = Some(chosen_tier);
            self.consecutive_same_tier = 1;
        }
        self.last_ambient_category = Some(picked.category.clone());
        Some(picked)
    }

    fn begin_binding(
        &mut self,
        key: &str,
        priority: Priority,
        now_ms: i64,
        force: bool,
    ) -> Vec<Action> {
        let exclude = self.current.as_ref().map(|c| c.clip_id.as_str()).map(str::to_string);
        let clip = if key == "idle" {
            self.pick_ambient_breathing(now_ms, exclude.as_deref())
        } else {
            let Some(character) = self.character.as_ref() else {
                return Vec::new();
            };
            pick_for_binding_excluding(
                character,
                key,
                &self.history,
                now_ms,
                &mut self.rng,
                exclude.as_deref(),
            )
        };
        let Some(clip) = clip else {
            return Vec::new();
        };
        let mirror = if priority == Priority::Drag {
            // 拖拽段与前端预热槽对齐，不随机水平翻转，避免起拖 20ms 后画面突跳。
            false
        } else {
            clip.mirrorable && self.rng.next_f64() < 0.35
        };
        let mut request = request_from_clip(&clip, priority, mirror);
        if priority == Priority::Drag {
            // 拖拽悬空段无论原 meta 是否标 loop，均强制循环直至松手。
            request.looped = true;
        } else if matches!(priority, Priority::Click | Priority::Idle) {
            // 点击反应与非工作态呼吸待机段只播一遍，播完自然轮换下一段，不锁死调度。
            request.looped = false;
            request.hold_loop = false;
        }
        self.try_start(request, now_ms, force)
    }

    fn begin_specific(&mut self, clip_id: &str, priority: Priority, now_ms: i64) -> Vec<Action> {
        let Some(character) = self.character.as_ref() else {
            return Vec::new();
        };
        let Some(clip) = character.clip_map.get(clip_id) else {
            return Vec::new();
        };
        let mirror = clip.mirrorable && self.rng.next_f64() < 0.35;
        let mut request = request_from_clip(clip, priority, mirror);
        if priority == Priority::Command {
            request.looped = false;
        }
        self.try_start(request, now_ms, false)
    }

    fn try_start(&mut self, request: PlayRequest, now_ms: i64, force: bool) -> Vec<Action> {
        self.try_start_with_scene(request, None, now_ms, force)
    }

    fn try_start_with_scene(
        &mut self,
        request: PlayRequest,
        scene: Option<AgentActivityKind>,
        now_ms: i64,
        force: bool,
    ) -> Vec<Action> {
        if let Some(current) = &self.current {
            let elapsed = now_ms - current.started_ms;
            if !force {
                // 若当前已不处于拖拽态但残留 Drag 优先级，允许点击直接接管，避免永久失灵。
                let stale_drag = current.priority == Priority::Drag && !self.dragging;
                if request.priority < current.priority && !stale_drag {
                    return Vec::new();
                }
                if request.priority == current.priority
                    && elapsed < SAME_PRIORITY_DEBOUNCE_MS
                    && scene.is_none()
                {
                    return Vec::new();
                }
                // v1.4：DRAG 与 CLICK 属于直接手动交互，必须即时响应，不被最长 3s 的 interruptAfterMs 吞掉。
                let immediate_user_action =
                    matches!(request.priority, Priority::Drag | Priority::Click);
                if !immediate_user_action
                    && scene.is_none()
                    && elapsed < current.interrupt_after_ms as i64
                {
                    if request.priority >= current.priority {
                        self.pending = Some(request);
                    }
                    return Vec::new();
                }
            }
        }
        self.start_with_scene(request, scene, now_ms)
    }

    fn start_with_scene(
        &mut self,
        request: PlayRequest,
        scene: Option<AgentActivityKind>,
        now_ms: i64,
    ) -> Vec<Action> {
        self.history.insert(request.clip_id.clone(), now_ms);
        self.current = Some(Current::from_request(&request, now_ms, scene));
        vec![Action::Play(request)]
    }
}

/* ------------------------------------------------------------------ */
/* 模板气泡对话                                                        */
/* ------------------------------------------------------------------ */

pub struct ChatIntent {
    pub binding: &'static str,
    pub categories: &'static [&'static str],
    pub bubble: &'static str,
}

pub fn match_chat(text: &str) -> ChatIntent {
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));

    if has(&["摸头", "摸摸", "rua", "揉头", "拍头"]) {
        return ChatIntent {
            binding: "click",
            categories: &["emotion", "chatter"],
            bubble: "唔…再摸一下下。",
        };
    }
    if has(&["跳舞", "舞", "才艺", "表演", "唱"]) {
        return ChatIntent {
            binding: "",
            categories: &["talent"],
            bubble: "看好了，这是我的拿手舞步！",
        };
    }
    if has(&["饿", "吃", "饭", "零食", "外卖", "宵夜"]) {
        return ChatIntent {
            binding: "eat",
            categories: &["eat"],
            bubble: "肚子在叫了…有零食吗？",
        };
    }
    if has(&["困", "睡", "累", "晚安", "熬夜", "哈欠"]) {
        return ChatIntent {
            binding: "sleep",
            categories: &["sleep"],
            bubble: "眼皮好重，陪我眯一会儿嘛。",
        };
    }
    if has(&["工作", "加班", "代码", "写", "忙", "项目", "bug", "debug"]) {
        return ChatIntent {
            binding: "work",
            categories: &["work"],
            bubble: "代码在跑，我也在陪你加班。",
        };
    }
    if has(&["可爱", "好看", "厉害", "棒", "漂亮", "喜欢", "帅"]) {
        return ChatIntent {
            binding: "click",
            categories: &["emotion", "chatter"],
            bubble: "嘿嘿，被夸到了，尾巴要翘起来啦。",
        };
    }
    if has(&[
        "你好",
        "早上好",
        "早安",
        "在吗",
        "hello",
        "hi",
        "嗨",
        "晚上好",
    ]) {
        return ChatIntent {
            binding: "click",
            categories: &["emotion", "chatter"],
            bubble: "在呢在呢，今天也一起加油～",
        };
    }
    ChatIntent {
        binding: "idle",
        categories: &[],
        bubble: "「{text}」…我记住啦。",
    }
}

/* ------------------------------------------------------------------ */
/* 作息触发                                                            */
/* ------------------------------------------------------------------ */

/// 返回当前分钟应当触发、且今天尚未触发过的作息槽位。
///
/// 调度线程每 20s 调一次；用 `YYYYMMDD` 去重，保证同一槽位一天只触发一次。
pub fn due_schedule_slots(
    schedule: &Schedule,
    hhmm: &str,
    date_key: i32,
    fired: &HashMap<String, i32>,
) -> Vec<String> {
    let mut slots = Vec::new();
    for (slot, value) in [
        ("wake", &schedule.wake),
        ("lunch", &schedule.lunch),
        ("dinner", &schedule.dinner),
        ("sleep", &schedule.sleep),
    ] {
        if value.trim() == hhmm && fired.get(slot).copied() != Some(date_key) {
            slots.push(slot.to_string());
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bindings, QaInfo, Schedule};
    use std::collections::HashMap;

    fn clip(id: &str, interrupt_after_ms: u64) -> ClipMeta {
        ClipMeta {
            id: id.to_string(),
            frames: 300,
            fps: 30.0,
            duration_ms: 10_000,
            interrupt_after_ms,
            qa: QaInfo {
                status: "pass".to_string(),
                green_edge: 0.0,
                hole_ratio: 1.0,
            },
            mirrorable: true,
            ..ClipMeta::default()
        }
    }

    fn runtime() -> CharacterRuntime {
        let clips = vec![
            clip("idle1", 0),
            clip("click1", 0),
            clip("drag1", 0),
            clip("work1", 0),
            clip("cmd1", 0),
            clip("eat1", 0),
        ];
        let mut bindings = Bindings::default();
        bindings.idle = vec!["idle1".to_string()];
        bindings.click = vec!["click1".to_string()];
        bindings.drag = vec!["drag1".to_string()];
        bindings.work = vec!["work1".to_string()];
        bindings.eat = vec!["eat1".to_string()];
        let mut clip_map = HashMap::new();
        for item in &clips {
            clip_map.insert(item.id.clone(), item.clone());
        }
        CharacterRuntime {
            id: "test".to_string(),
            name: "Test".to_string(),
            bindings,
            bindings_json: serde_json::json!({}),
            clips,
            clip_map,
            schedule: Schedule::default(),
            default_scale: 0.5,
        }
    }

    fn play_id(actions: &[Action]) -> Option<&str> {
        actions.iter().find_map(|action| match action {
            Action::Play(request) => Some(request.clip_id.as_str()),
            _ => None,
        })
    }

    fn trigger_rapid_click(core: &mut SchedulerCore, base_ms: i64) -> Vec<Action> {
        let mut last = Vec::new();
        for i in 0..RAPID_CLICK_THRESHOLD {
            last = core.on_event(
                SchedulerEvent::UserClick { x: 0.0, y: 0.0 },
                base_ms + (i as i64) * 150,
            );
        }
        last
    }

    #[test]
    fn click_preempts_idle() {
        let mut core = SchedulerCore::new(1);
        core.set_character(runtime(), HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        // 普通单击不再切换动画
        assert!(core
            .on_event(SchedulerEvent::UserClick { x: 0.0, y: 0.0 }, 100)
            .is_empty());
        // 3s 内连点 5 次触发专属生气动画 + angry_1 语音
        let rapid = trigger_rapid_click(&mut core, 4_000);
        assert_eq!(play_id(&rapid), Some("click1"));
        assert!(rapid.iter().any(|a| matches!(
            a,
            Action::Voice { scene_key, .. } if scene_key == "angry_1"
        )));
    }

    #[test]
    fn drag_preempts_even_with_long_interrupt_after() {
        let mut core = SchedulerCore::new(1);
        let mut character = runtime();
        // idle 段要求播放满 5s 才允许普通高优先级打断。
        if let Some(idle) = character.clip_map.get_mut("idle1") {
            idle.interrupt_after_ms = 5_000;
        }
        if let Some(idle) = character.clips.iter_mut().find(|clip| clip.id == "idle1") {
            idle.interrupt_after_ms = 5_000;
        }
        core.set_character(character, HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::UserDragStart { x: 0.0, y: 0.0 }, 100)),
            Some("drag1")
        );
    }

    #[test]
    fn command_waits_until_interrupt_after() {
        let mut core = SchedulerCore::new(1);
        let mut character = runtime();
        if let Some(idle) = character.clip_map.get_mut("idle1") {
            idle.interrupt_after_ms = 2_000;
        }
        if let Some(idle) = character.clips.iter_mut().find(|clip| clip.id == "idle1") {
            idle.interrupt_after_ms = 2_000;
        }
        core.set_character(character, HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        assert!(core
            .on_event(
                SchedulerEvent::ChatReply {
                    clip_id: "cmd1".to_string()
                },
                100
            )
            .is_empty());
        assert!(core.tick(1_999).is_empty());
        assert_eq!(play_id(&core.tick(2_100)), Some("cmd1"));
    }

    #[test]
    fn lower_priority_never_preempts() {
        let mut core = SchedulerCore::new(1);
        let mut character = runtime();
        if let Some(click) = character.clip_map.get_mut("click1") {
            click.interrupt_after_ms = 3_000;
        }
        core.set_character(character, HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        assert_eq!(
            play_id(&trigger_rapid_click(&mut core, 100)),
            Some("click1")
        );
        assert!(core
            .on_event(
                SchedulerEvent::ScheduleTick {
                    slot: "lunch".to_string()
                },
                200
            )
            .is_empty());
        assert_eq!(core.current_clip(), Some("click1"));
    }

    #[test]
    fn work_focus_does_not_play_work_when_agent_idle() {
        let mut core = SchedulerCore::new(2);
        core.set_character(runtime(), HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        assert!(core.on_event(SchedulerEvent::WorkAppFocus, 100).is_empty());
        assert!(!core.is_work_mode());

        // 即使在前台 IDE 焦点下多轮循环收段，非 Agent 工作态也绝不随机抽中 work 片段。
        for round in 1..=6 {
            let ended = core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("idle1".to_string()),
                },
                round * 12_000,
            );
            assert_eq!(play_id(&ended), Some("idle1"));
            assert!(!core.is_work_mode());
        }
    }

    #[test]
    fn two_hour_recency_weight_is_point_one() {
        let now = 10_000_000;
        assert!((selection_weight(Some(now - 1_000), now) - 0.1).abs() < 1e-9);
        assert!((selection_weight(Some(now - TWO_HOURS_MS + 1), now) - 0.1).abs() < 1e-9);
        assert!((selection_weight(Some(now - TWO_HOURS_MS), now) - 1.0).abs() < 1e-9);
        assert!((selection_weight(None, now) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn weighted_pick_uses_point_one_weight() {
        let items = vec!["recent", "old"];
        let weights = vec![0.1, 1.0];
        // 总权重 1.1；r=0.05 落在 recent（0.1），r=0.5 落在 old。
        assert_eq!(pick_weighted_value(&items, &weights, 0.05), Some("recent"));
        assert_eq!(pick_weighted_value(&items, &weights, 0.5), Some("old"));
        // 10% 权重段对应的累计概率边界。
        assert_eq!(pick_weighted_value(&items, &weights, 0.089), Some("recent"));
        assert_eq!(pick_weighted_value(&items, &weights, 0.10), Some("old"));
    }

    #[test]
    fn fail_clips_are_not_scheduled() {
        let mut character = runtime();
        for clip in character.clips.iter_mut().filter(|clip| clip.id == "idle1") {
            clip.qa.status = "fail".to_string();
        }
        if let Some(fail) = character.clip_map.get_mut("idle1") {
            fail.qa.status = "fail".to_string();
        }
        let mut core = SchedulerCore::new(3);
        core.set_character(character, HashMap::new());
        let actions = core.on_event(SchedulerEvent::AppReady, 0);
        let picked = play_id(&actions);
        assert_ne!(picked, Some("idle1"));
    }

    #[test]
    fn schedule_slot_fires_once_per_day() {
        let schedule = Schedule {
            wake: "08:30".to_string(),
            lunch: "12:00".to_string(),
            dinner: "18:30".to_string(),
            sleep: "23:30".to_string(),
        };
        let mut fired = HashMap::new();
        assert_eq!(
            due_schedule_slots(&schedule, "12:00", 20261122, &fired),
            vec!["lunch".to_string()]
        );
        fired.insert("lunch".to_string(), 20261122);
        assert!(due_schedule_slots(&schedule, "12:00", 20261122, &fired).is_empty());
        // 次日同一分钟可以再次触发。
        assert_eq!(
            due_schedule_slots(&schedule, "12:00", 20261123, &fired),
            vec!["lunch".to_string()]
        );
    }

    #[test]
    fn chat_keyword_matching() {
        assert_eq!(match_chat("今天好饿呀").binding, "eat");
        assert_eq!(match_chat("困死了晚安").binding, "sleep");
        assert_eq!(match_chat("一起写代码加班").binding, "work");
        assert_eq!(match_chat("你好呀").binding, "click");
        assert_eq!(match_chat("跳个舞看看").categories, &["talent"]);
    }

    #[test]
    fn agent_alert_priority_sits_between_schedule_and_command() {
        assert!(Priority::AgentAlert > Priority::Schedule);
        assert!(Priority::AgentAlert < Priority::Command);
        assert!(Priority::AgentAlert < Priority::Click);
        assert!(Priority::AgentAlert < Priority::Drag);
        assert_eq!(Priority::AgentAlert.label(), "AGENT_ALERT");
    }

    #[test]
    fn agent_streaming_activates_work_and_idle_restores() {
        let mut core = SchedulerCore::new(4);
        core.set_character(runtime(), HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentActivity(TelemetryStatus::Streaming),
                100
            )),
            Some("work1")
        );
        assert!(core.is_work_mode());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AgentActivity(TelemetryStatus::Idle), 200)),
            Some("idle1")
        );
        assert!(!core.is_work_mode());
    }

    #[test]
    fn work_blur_does_not_leave_work_mode_while_agent_active() {
        let mut core = SchedulerCore::new(5);
        core.set_character(runtime(), HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);
        core.on_event(SchedulerEvent::WorkAppFocus, 50);
        core.on_event(
            SchedulerEvent::AgentActivity(TelemetryStatus::ToolRunning),
            100,
        );
        assert!(core.is_work_mode());
        assert!(core.on_event(SchedulerEvent::WorkAppBlur, 200).is_empty());
        assert!(core.is_work_mode());

        // 即使再次处于 WorkAppFocus，Agent 退出为空闲时也必须立即退出工作模式并回到 idle。
        core.on_event(SchedulerEvent::WorkAppFocus, 250);
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AgentActivity(TelemetryStatus::Idle), 300)),
            Some("idle1")
        );
        assert!(!core.is_work_mode());
    }

    #[test]
    fn agent_alert_plays_and_cools_down_for_60s() {
        let mut core = SchedulerCore::new(6);
        core.set_character(runtime(), HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        let first = core.on_event(
            SchedulerEvent::AgentActivity(TelemetryStatus::WaitingApproval),
            100,
        );
        assert_eq!(play_id(&first), Some("click1"));
        assert!(first
            .iter()
            .any(|action| matches!(action, Action::Bubble { .. })));

        // 30s 后同状态重复上报：冷却期内不再播放、不再弹泡。
        let cooled = core.on_event(
            SchedulerEvent::AgentActivity(TelemetryStatus::WaitingApproval),
            30_100,
        );
        assert!(play_id(&cooled).is_none());
        assert!(!cooled
            .iter()
            .any(|action| matches!(action, Action::Bubble { .. })));

        // 超过 60s 后允许再次提醒。
        let later = core.on_event(
            SchedulerEvent::AgentActivity(TelemetryStatus::WaitingApproval),
            61_100,
        );
        assert!(later
            .iter()
            .any(|action| matches!(action, Action::Bubble { .. })));
    }

    #[test]
    fn agent_alert_does_not_interrupt_drag() {
        let mut core = SchedulerCore::new(7);
        core.set_character(runtime(), HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::UserDragStart { x: 0.0, y: 0.0 }, 100)),
            Some("drag1")
        );
        let actions = core.on_event(
            SchedulerEvent::AgentAlert {
                key: "context_high".to_string(),
                bubble: "上下文快满了".to_string(),
            },
            200,
        );
        assert!(play_id(&actions).is_none());
        assert!(actions
            .iter()
            .any(|action| matches!(action, Action::Bubble { .. })));
        assert_eq!(core.current_clip(), Some("drag1"));
    }

    #[test]
    fn click_preempts_long_interrupt_after_and_avoids_repeating_same_clip() {
        let mut core = SchedulerCore::new(11);
        let mut character = runtime();
        let mut c2 = clip("click2", 3_000);
        c2.looped = true;
        if let Some(c1) = character.clip_map.get_mut("click1") {
            c1.interrupt_after_ms = 3_000;
            c1.looped = true;
        }
        character.clips.push(c2.clone());
        character.clip_map.insert("click2".to_string(), c2);
        character.bindings.click.push("click2".to_string());
        if let Some(idle) = character.clip_map.get_mut("idle1") {
            idle.interrupt_after_ms = 3_000;
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        let first = play_id(&trigger_rapid_click(&mut core, 200)).map(|s| s.to_string());
        assert!(first.as_deref() == Some("click1") || first.as_deref() == Some("click2"));

        // 冷却期内再次连点不重复触发；超过 14s 冷却后可再次触发 angry_2 语音。
        assert!(trigger_rapid_click(&mut core, 2_000).is_empty());
        let second_actions = trigger_rapid_click(&mut core, 16_000);
        let second = play_id(&second_actions).map(|s| s.to_string());
        assert!(second.as_deref() == Some("click1") || second.as_deref() == Some("click2"));
        assert!(second_actions.iter().any(|a| matches!(
            a,
            Action::Voice { scene_key, .. } if scene_key == "angry_2"
        )));

        // 点击段播放结束后回到待机，不无限循环。
        let after_end = core.on_event(SchedulerEvent::ClipEnded { clip_id: second }, 28_000);
        assert_eq!(play_id(&after_end), Some("idle1"));
    }

    #[test]
    fn drag_forces_looped_even_when_meta_is_not_looped() {
        let mut core = SchedulerCore::new(13);
        let character = runtime();
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        let actions = core.on_event(SchedulerEvent::UserDragStart { x: 0.0, y: 0.0 }, 100);
        let req = actions.iter().find_map(|a| match a {
            Action::Play(r) => Some(r),
            _ => None,
        });
        assert!(req.is_some_and(|r| r.clip_id == "drag1" && r.looped));
    }

    #[test]
    fn held_work_scene_stays_locked_past_duration_until_next_scene() {
        let mut core = SchedulerCore::new(17);
        let mut character = runtime();
        let mut c_build = clip("工作状态-原地踱步张望", 0);
        c_build.looped = false;
        let mut c_wait = clip("招手过来", 0);
        c_wait.looped = false;
        character.clips.push(c_build.clone());
        character.clips.push(c_wait.clone());
        character
            .clip_map
            .insert("工作状态-原地踱步张望".to_string(), c_build);
        character.clip_map.insert("招手过来".to_string(), c_wait);

        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        let start = core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::ToolRunning,
                activity: AgentActivityKind::BuildLongRunning,
                detail: None,
                bubble: None,
            }),
            100,
        );
        let req = start.iter().find_map(|a| match a {
            Action::Play(r) => Some(r),
            _ => None,
        });
        assert!(req.is_some_and(|r| r.clip_id == "工作状态-原地踱步张望" && r.hold_loop && r.looped));

        // 超过 10s durationMs 且收到前端 ClipEnded，依旧锁定在「工作状态-原地踱步张望」，不掉回 idle 也不重播。
        assert!(core.tick(25_000).is_empty());
        assert!(core
            .on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("工作状态-原地踱步张望".to_string()),
                },
                26_000
            )
            .is_empty());
        assert_eq!(core.current_clip(), Some("工作状态-原地踱步张望"));

        // 直到 Agent 切入下一个审批动作，才切换动画。
        let next = core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::WaitingApproval,
                activity: AgentActivityKind::ApprovalTerminal,
                detail: None,
                bubble: None,
            }),
            27_000,
        );
        assert_eq!(play_id(&next), Some("招手过来"));
    }

    #[test]
    fn drag_and_click_restore_held_agent_scene_after_ending() {
        let mut core = SchedulerCore::new(19);
        let mut character = runtime();
        let c_code = clip("写代码", 0);
        character.clips.push(c_code.clone());
        character.clip_map.insert("写代码".to_string(), c_code);
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::Streaming,
                activity: AgentActivityKind::CodeStreaming,
                detail: None,
                bubble: None,
            }),
            100,
        );
        assert_eq!(core.current_clip(), Some("写代码"));

        // 拖拽打断后松手，自动恢复原工作锁定态。
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::UserDragStart { x: 0.0, y: 0.0 }, 200)),
            Some("drag1")
        );
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::UserDragEnd { x: 0.0, y: 0.0 }, 500)),
            Some("写代码")
        );

        // 高频连点生气反应播完后也恢复工作锁定态，绝不掉回 idle。
        assert_eq!(
            play_id(&trigger_rapid_click(&mut core, 1_000)),
            Some("click1")
        );
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("click1".to_string())
                },
                12_000
            )),
            Some("写代码")
        );
    }

    #[test]
    fn min_dwell_defers_rapid_same_tier_scene_but_l1_preempts_immediately() {
        let mut core = SchedulerCore::new(23);
        let mut character = runtime();
        for id in ["工作状态-原地踱步张望", "书灵现世", "招手过来"] {
            let c = clip(id, 0);
            character.clips.push(c.clone());
            character.clip_map.insert(id.to_string(), c);
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::ToolRunning,
                    activity: AgentActivityKind::BuildLongRunning,
                    detail: None,
                    bubble: None,
                }),
                1_000
            )),
            Some("工作状态-原地踱步张望")
        );

        // 300ms 内切回常规工作状态会被 900ms 驻留防抖暂存；L1 审批立即无视驻留强抢占。
        assert!(core
            .on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::CodeStreaming,
                    detail: None,
                    bubble: None,
                }),
                1_300
            )
            .is_empty());
        assert_eq!(core.current_clip(), Some("工作状态-原地踱步张望"));

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::WaitingApproval,
                    activity: AgentActivityKind::ApprovalTerminal,
                    detail: None,
                    bubble: None,
                }),
                1_500
            )),
            Some("招手过来")
        );
    }

    #[test]
    fn thinking_intro_plays_once_and_interrupted_immediately_by_matching_work_clip() {
        let mut core = SchedulerCore::new(27);
        let mut character = runtime();
        for id in ["工作状态-思考冒泡", "写代码", "轻快记录", "工作状态-忙碌点按"] {
            let c = clip(id, 0);
            character.clips.push(c.clone());
            character.clip_map.insert(id.to_string(), c);
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        // 1. 进入工作先播思考开场
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::DeepReasoning,
                    detail: None,
                    bubble: None,
                }),
                1_000
            )),
            Some("工作状态-思考冒泡")
        );

        // 2. 1.5s 后切到 CodeDiffing：立即打断思考开场，切到匹配的「轻快记录」
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::ToolRunning,
                    activity: AgentActivityKind::CodeDiffing,
                    detail: None,
                    bubble: None,
                }),
                2_500
            )),
            Some("轻快记录")
        );

        // 3. 轮播期间后续思考/工具均不打断当前「轻快记录」
        assert!(core
            .on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::DeepReasoning,
                    detail: None,
                    bubble: None,
                }),
                4_000
            )
            .is_empty());
        assert_eq!(core.current_clip(), Some("轻快记录"));

        // 4. 播完「轻快记录」后按队列顺序切到「工作状态-忙碌点按」->「写代码」
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("轻快记录".to_string())
                },
                13_000,
            )),
            Some("工作状态-忙碌点按")
        );
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("工作状态-忙碌点按".to_string())
                },
                23_500,
            )),
            Some("写代码")
        );

        // 5. 回到 Idle 后开启新一轮工作，再次先演 1 次思考开场
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AgentActivity(TelemetryStatus::Idle), 25_000)),
            Some("idle1")
        );
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::DeepReasoning,
                    detail: None,
                    bubble: None,
                }),
                26_000
            )),
            Some("工作状态-思考冒泡")
        );
    }

    #[test]
    fn thinking_intro_advances_to_work_playlist_after_finishing_one_cycle() {
        let mut core = SchedulerCore::new(33);
        let mut character = runtime();
        for id in ["工作状态-思考冒泡", "写代码", "轻快记录"] {
            let c = clip(id, 0);
            character.clips.push(c.clone());
            character.clip_map.insert(id.to_string(), c);
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::DeepReasoning,
                    detail: None,
                    bubble: None,
                }),
                1_000
            )),
            Some("工作状态-思考冒泡")
        );
        // 持续思考播完 1 遍后自动进入工作队列第 1 段「写代码」
        assert_eq!(play_id(&core.tick(11_200)), Some("写代码"));
    }

    #[test]
    fn specialty_web_search_and_mcp_interrupt_playlist_and_resume_playlist() {
        let mut core = SchedulerCore::new(39);
        let mut character = runtime();
        for id in ["写代码", "轻快记录", "东张西望", "书灵现世"] {
            let c = clip(id, 0);
            character.clips.push(c.clone());
            character.clip_map.insert(id.to_string(), c);
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::CodeStreaming,
                    detail: None,
                    bubble: None,
                }),
                1_000
            )),
            Some("写代码")
        );

        // 低频网页搜索立即切特色段
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::ToolRunning,
                    activity: AgentActivityKind::ResearchLookup,
                    detail: Some("web_search".to_string()),
                    bubble: None,
                }),
                2_000
            )),
            Some("书灵现世")
        );

        // 离开网页搜索回到思考/常规工作：超过 900ms 防抖后顺接工作队列下一段「轻快记录」，不再重播思考
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::DeepReasoning,
                    detail: None,
                    bubble: None,
                }),
                3_200
            )),
            Some("轻快记录")
        );
    }

    #[test]
    fn agent_bubble_emits_without_interrupting_current_work_clip() {
        let mut core = SchedulerCore::new(41);
        let mut character = runtime();
        let c_code = clip("写代码", 0);
        character.clips.push(c_code.clone());
        character.clip_map.insert("写代码".to_string(), c_code);
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::Streaming,
                activity: AgentActivityKind::CodeStreaming,
                detail: None,
                bubble: None,
            }),
            1_000,
        );
        let actions = core.on_event(
            SchedulerEvent::AgentBubble {
                text: "本步上下文 +18.5K Tokens（当前 38%）".to_string(),
                ttl_ms: 4_200,
            },
            2_500,
        );
        assert!(play_id(&actions).is_none());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Bubble { text, .. } if text.contains("+18.5K"))));
        assert_eq!(core.current_clip(), Some("写代码"));
    }

    #[test]
    fn same_clip_across_held_scenes_does_not_reemit_play() {
        let mut core = SchedulerCore::new(29);
        let mut character = runtime();
        let c_build = clip("工作状态-原地踱步张望", 0);
        character.clips.push(c_build.clone());
        character
            .clip_map
            .insert("工作状态-原地踱步张望".to_string(), c_build);
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::ToolRunning,
                    activity: AgentActivityKind::BuildLongRunning,
                    detail: None,
                    bubble: None,
                }),
                1_000
            )),
            Some("工作状态-原地踱步张望")
        );

        // 若新场景解析出的仍是同一段正在播的 hold_loop 片段，不重发 Play 导致黑闪重播。
        let actions = core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::ToolRunning,
                activity: AgentActivityKind::BrowserRunning,
                detail: None,
                bubble: None,
            }),
            3_000,
        );
        assert!(play_id(&actions).is_none());
    }

    #[test]
    fn ttft_waiting_does_not_pin_or_override_animation() {
        let mut core = SchedulerCore::new(31);
        let mut character = runtime();
        let c_code = clip("写代码", 0);
        character.clips.push(c_code.clone());
        character.clip_map.insert("写代码".to_string(), c_code);
        core.set_character(character, HashMap::new());
        assert_eq!(
            play_id(&core.on_event(SchedulerEvent::AppReady, 0)),
            Some("idle1")
        );

        let ignored = core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::Streaming,
                activity: AgentActivityKind::TtftWaiting,
                detail: None,
                bubble: None,
            }),
            1_000,
        );
        assert!(ignored.is_empty());
        assert_eq!(core.current_clip(), Some("idle1"));
        assert!(!core.is_work_mode());
        assert!(core.active_scene.is_none());

        // 待机段播完后仍继续保持 idle，不会因曾收到 TtftWaiting 而切到 work。
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("idle1".to_string())
                },
                12_000,
            )),
            Some("idle1")
        );
    }

    #[test]
    fn task_completed_clears_held_work_scene_and_returns_to_idle() {
        let mut core = SchedulerCore::new(37);
        let mut character = runtime();
        for id in ["写代码", "工作状态-雀跃庆祝"] {
            let c = clip(id, 0);
            character.clips.push(c.clone());
            character.clip_map.insert(id.to_string(), c);
        }
        core.set_character(character, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Streaming,
                    activity: AgentActivityKind::CodeStreaming,
                    detail: None,
                    bubble: None,
                }),
                100,
            )),
            Some("写代码")
        );
        assert!(core.is_work_mode());

        // 任务完成后播放庆祝段（Status=Idle），不锁定工作场景，播完即回 idle 而非复活「写代码」。
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::AgentScene(AgentSceneSignal {
                    status: TelemetryStatus::Idle,
                    activity: AgentActivityKind::TaskCompleted,
                    detail: None,
                    bubble: None,
                }),
                6_000,
            )),
            Some("工作状态-雀跃庆祝")
        );
        assert!(!core.is_work_mode());
        assert!(core.active_scene.is_none());
        assert_eq!(
            play_id(&core.on_event(
                SchedulerEvent::ClipEnded {
                    clip_id: Some("工作状态-雀跃庆祝".to_string())
                },
                16_500,
            )),
            Some("idle1")
        );
    }

    #[test]
    fn real_eight_chars_resolve_all_antigravity_scenes() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets");
        if !root.join("chars").is_dir() {
            return;
        }
        let scenes = [
            AgentActivityKind::ProcessOffline,
            AgentActivityKind::ServiceConnecting,
            AgentActivityKind::ServiceReconnecting,
            AgentActivityKind::SessionInit,
            AgentActivityKind::TtftWaiting,
            AgentActivityKind::PlanningMode,
            AgentActivityKind::DeepReasoning,
            AgentActivityKind::ResearchLookup,
            AgentActivityKind::CodeStreaming,
            AgentActivityKind::CodeDiffing,
            AgentActivityKind::TerminalRunning,
            AgentActivityKind::BuildLongRunning,
            AgentActivityKind::TerminalError,
            AgentActivityKind::BrowserRunning,
            AgentActivityKind::McpToolCalling,
            AgentActivityKind::WorkflowRunning,
            AgentActivityKind::ApprovalTerminal,
            AgentActivityKind::ApprovalOutsideDir,
            AgentActivityKind::ArtifactReviewReady,
            AgentActivityKind::TaskCompleted,
            AgentActivityKind::ContextDeltaSpike,
            AgentActivityKind::ContextWarning80,
            AgentActivityKind::QuotaLowWarning,
            AgentActivityKind::QuotaExhausted,
            AgentActivityKind::ApiAuthRateError,
        ];
        let history = HashMap::new();
        let mut rng = Rng::new(7);
        for char_id in ["claude", "deepseek", "gemini", "glm", "gpt", "grok", "kimi", "qwen"] {
            let ch = crate::model::load_character(&root, char_id).expect(char_id);
            for scene in scenes {
                let clip = resolve_clip_for_scene(&ch, scene, &history, 1_000, &mut rng);
                let resolved = clip.as_ref().expect("clip");
                assert!(
                    resolved.playable(),
                    "{char_id} failed to resolve {scene:?}"
                );
                if scene == AgentActivityKind::ServiceReconnecting {
                    assert!(
                        !is_work_clip(resolved),
                        "{char_id} ServiceReconnecting should not resolve work clip {}",
                        resolved.id
                    );
                }
            }
            let playlist = build_work_playlist(&ch);
            assert!(
                playlist.len() >= 2,
                "{char_id} work playlist too short: {}",
                playlist.len()
            );
            for item in &playlist {
                assert!(
                    !item.id.contains("思考")
                        && !item.id.contains("垂头叹气")
                        && !item.id.contains("雀跃庆祝"),
                    "{char_id} work playlist should not include {}",
                    item.id
                );
            }
        }
    }

    #[test]
    fn festival_and_season_weights_match_calendar_windows() {
        // 2026-02-17 春节
        assert_eq!(festival_and_season_weight("贴春联", "festival", 20260217), 6.0);
        // 2026-09-25 中秋
        assert_eq!(festival_and_season_weight("中秋赏月吃月饼", "eat", 20260925), 6.0);
        // 2026-12-25 圣诞
        assert_eq!(festival_and_season_weight("装点圣诞树", "festival", 20261225), 6.0);
        // 2026-11-23 秋季
        assert_eq!(festival_and_season_weight("被落叶淹没", "seasonal", 20261123), 2.2);
        // 非当季非节日
        assert_eq!(festival_and_season_weight("贴春联", "festival", 20260715), 1.0);
    }

    #[test]
    fn quota_barometer_selects_matching_tier_and_bubble() {
        let (rich_cands, rich_msg) = quota_barometer_candidates(92.0);
        assert!(rich_cands.contains(&"余额-钱袋满溢"));
        assert!(rich_msg.contains("92%"));

        let (normal_cands, normal_msg) = quota_barometer_candidates(52.0);
        assert!(normal_cands.contains(&"余额-钱袋如常"));
        assert!(normal_msg.contains("52%"));

        let (tight_cands, tight_msg) = quota_barometer_candidates(18.0);
        assert!(tight_cands.contains(&"余额-数金皱眉"));
        assert!(tight_msg.contains("18%"));
    }

    #[test]
    fn process_offline_does_not_hold_loop_and_breathes_on_clip_end() {
        let mut core = SchedulerCore::new(71);
        let mut ch = runtime();
        let mut idle2 = clip("idle2", 0);
        idle2.category = "idle".to_string();
        ch.clips.push(idle2.clone());
        ch.clip_map.insert("idle2".to_string(), idle2);
        ch.bindings.idle.push("idle2".to_string());
        core.set_character(ch, HashMap::new());

        let start = core.on_event(SchedulerEvent::AppReady, 0);
        let first_req = start.iter().find_map(|a| match a {
            Action::Play(r) => Some(r),
            _ => None,
        });
        assert!(first_req.is_some_and(|r| !r.hold_loop && !r.looped));

        let offline = core.on_event(
            SchedulerEvent::AgentScene(AgentSceneSignal {
                status: TelemetryStatus::Offline,
                activity: AgentActivityKind::ProcessOffline,
                detail: None,
                bubble: None,
            }),
            500,
        );
        assert!(offline.is_empty());

        let ended = core.on_event(
            SchedulerEvent::ClipEnded {
                clip_id: core.current_clip().map(str::to_string),
            },
            10_500,
        );
        assert!(play_id(&ended).is_some());
    }

    #[test]
    fn ambient_three_tier_distribution_and_category_anti_repeat() {
        let mut core = SchedulerCore::new(83);
        let mut ch = runtime();
        for (id, cat) in [
            ("待机呼吸休闲", "idle"),
            ("东张西望", "idle"),
            ("晨间刷牙", "daily"),
            ("撸猫", "play"),
            ("吃白饭", "eat"),
            ("可爱宅舞", "talent"),
        ] {
            let mut c = clip(id, 0);
            c.category = cat.to_string();
            ch.clips.push(c.clone());
            ch.clip_map.insert(id.to_string(), c);
        }
        core.set_character(ch, HashMap::new());
        core.on_event(SchedulerEvent::AppReady, 0);

        let mut seen_tiers = std::collections::HashSet::new();
        let mut now = 10_000i64;
        for _ in 0..30 {
            let cur = core.current_clip().map(str::to_string);
            let acts = core.on_event(SchedulerEvent::ClipEnded { clip_id: cur }, now);
            if let Some(id) = play_id(&acts) {
                if let Some(meta) = core.character.as_ref().and_then(|c| c.clip_map.get(id)) {
                    if let Some(tier) = classify_ambient_tier(meta, &core.character.as_ref().unwrap().bindings.idle) {
                        seen_tiers.insert(match tier {
                            AmbientTier::Rest => 1,
                            AmbientTier::DailyPlayEat => 2,
                            AmbientTier::TalentEaster => 3,
                        });
                    }
                }
            }
            now += 10_500;
        }
        assert!(seen_tiers.len() >= 2);
    }

    #[test]
    fn all_eight_character_archetypes_resolve_angry_and_ten_emotion_buckets() {
        use crate::ai_voice::EmotionBucket;
        let mut ch = runtime();
        let sample_ids = [
            "点击回应-傲娇生气",
            "傲娇生气",
            "生气鼓腮跺脚",
            "生气鼓腮",
            "点击回应-开心跃动",
            "开心跃动",
            "狂喜原地跳",
            "点击回应-害羞惊讶",
            "害羞惊讶",
            "害羞捂脸偷看",
            "低头害羞",
            "得意炫耀转圈",
            "骄傲昂首",
            "心疼捂胸口",
            "工作状态-垂头叹气冒汗",
            "被吓一跳",
            "惊讶下巴掉",
            "哈欠连天",
            "沙发打瞌睡",
            "工作状态-思考冒泡",
            "换装侦探服思考",
            "握拳打气",
            "写代码",
            "比心双手头顶v2",
            "点击回应-元气挥手",
            "元气挥手",
        ];
        for id in sample_ids {
            let c = clip(id, 0);
            ch.clips.push(c.clone());
            ch.clip_map.insert(id.to_string(), c);
        }
        let history = HashMap::new();
        let mut rng = Rng::new(97);
        assert!(resolve_angry_clip(&ch, &history, 1_000, &mut rng).is_some());

        for bucket in [
            EmotionBucket::Happy,
            EmotionBucket::Shy,
            EmotionBucket::Proud,
            EmotionBucket::Angry,
            EmotionBucket::Sad,
            EmotionBucket::Surprised,
            EmotionBucket::Sleepy,
            EmotionBucket::Thinking,
            EmotionBucket::Cheer,
            EmotionBucket::Affection,
        ] {
            assert!(
                resolve_clip_for_emotion(&ch, bucket, &history, 1_000, &mut rng).is_some(),
                "failed to resolve {bucket:?}"
            );
        }
    }
}
