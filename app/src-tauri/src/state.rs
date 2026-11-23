//! 共享运行态：命令、调度线程、穿透线程、托盘都通过 `Shared` 读写。
//!
//! 所有可变字段都在 `Mutex` 后；`Shared` 本身是 `Clone`（内部 `Arc`），
//! 因此可以安全地交给 Tauri `manage` 与任意后台线程。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::db::Db;
use crate::mask::MaskCache;
use crate::model::{
    BubblePayload, CharacterRuntime, FrameHint, PetState, PetStats, PlayPayload, Schedule,
};
use crate::scheduler::SchedulerEvent;
use crate::stats::{now_ms, parse_hhmm, settle_stats, sleep_fraction_between};

/// v1.2：窗口内归一化常驻热区（迷你状态徽章等）。
///
/// 坐标系与角色镜像无关：前端上报的是窗口左上原点的 0..1 矩形，
/// 穿透线程先用原始 `(u, v)` 命它，再进入 mask 的镜像查表。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHitRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl UiHitRect {
    pub fn is_valid(&self) -> bool {
        const EPS: f64 = 0.001;
        [self.x, self.y, self.w, self.h]
            .iter()
            .all(|value| value.is_finite())
            && self.w > 0.0
            && self.h > 0.0
            && self.x >= -EPS
            && self.y >= -EPS
            && self.x + self.w <= 1.0 + EPS
            && self.y + self.h <= 1.0 + EPS
    }

    pub fn contains(&self, u: f64, v: f64) -> bool {
        self.w > 0.0
            && self.h > 0.0
            && u >= self.x
            && u <= self.x + self.w
            && v >= self.y
            && v <= self.y + self.h
    }
}

/// 纯函数：光标是否落在任意常驻热区。便于单测锁定「不随镜像翻转」。
pub fn ui_rect_hit(rects: &[UiHitRect], u: f64, v: f64) -> bool {
    rects.iter().any(|rect| rect.contains(u, v))
}

#[derive(Debug, Clone)]
pub struct CurrentPlay {
    pub char_id: String,
    pub clip_id: String,
    pub start_ms: i64,
    pub mirror: bool,
    pub frames: u32,
    pub fps: f64,
    pub duration_ms: u64,
    pub audio: Option<String>,
    pub hold_loop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeSide {
    Left,
    Right,
}

#[derive(Debug)]
pub struct UiFlags {
    pub dragging: bool,
    pub drag_grab_offset: Option<(i32, i32)>,
    pub drag_started_ms: i64,
    pub ui_interactive: bool,
    pub work_focus: bool,
    pub edge_docking_enabled: bool,
    pub edge_side: Option<EdgeSide>,
    pub panel_open: bool,
    pub panel_tab: String,
}

impl Default for UiFlags {
    fn default() -> Self {
        Self {
            dragging: false,
            drag_grab_offset: None,
            drag_started_ms: 0,
            ui_interactive: false,
            work_focus: false,
            edge_docking_enabled: true,
            edge_side: None,
            panel_open: false,
            panel_tab: "status".to_string(),
        }
    }
}

#[inline]
pub(crate) fn pack_i32_pair(a: i32, b: i32) -> i64 {
    ((a as i64) << 32) | ((b as u32) as i64)
}

#[inline]
pub(crate) fn unpack_i32_pair(packed: i64) -> (i32, i32) {
    ((packed >> 32) as i32, packed as i32)
}

/// 供 Win32 窗口子类主线程与后台 500Hz 拖拽跟踪线程共享的无锁原子状态。
pub struct DragFastState {
    dragging: AtomicBool,
    has_grab_offset: AtomicBool,
    grab_offset: AtomicI64,
    has_last_pos: AtomicBool,
    last_pos: AtomicI64,
    move_posted: AtomicBool,
    sync_posted: AtomicBool,
    subclass_installed: AtomicBool,
    hwnd: AtomicIsize,
}

impl DragFastState {
    pub fn new() -> Self {
        Self {
            dragging: AtomicBool::new(false),
            has_grab_offset: AtomicBool::new(false),
            grab_offset: AtomicI64::new(0),
            has_last_pos: AtomicBool::new(false),
            last_pos: AtomicI64::new(0),
            move_posted: AtomicBool::new(false),
            sync_posted: AtomicBool::new(false),
            subclass_installed: AtomicBool::new(false),
            hwnd: AtomicIsize::new(0),
        }
    }

    pub fn set_hwnd(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::Release);
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd.load(Ordering::Acquire)
    }

    pub fn mark_subclass_installed(&self) {
        self.subclass_installed.store(true, Ordering::Release);
    }

    pub fn is_subclass_installed(&self) -> bool {
        self.subclass_installed.load(Ordering::Acquire)
    }

    pub fn begin(&self, offset: Option<(i32, i32)>) {
        match offset {
            Some((ox, oy)) => {
                self.grab_offset.store(pack_i32_pair(ox, oy), Ordering::Release);
                self.has_grab_offset.store(true, Ordering::Release);
            }
            None => {
                self.has_grab_offset.store(false, Ordering::Release);
            }
        }
        self.has_last_pos.store(false, Ordering::Release);
        self.move_posted.store(false, Ordering::Release);
        self.dragging.store(true, Ordering::Release);
    }

    pub fn end(&self) {
        self.dragging.store(false, Ordering::Release);
        self.has_grab_offset.store(false, Ordering::Release);
        self.has_last_pos.store(false, Ordering::Release);
        self.move_posted.store(false, Ordering::Release);
    }

    #[inline]
    pub fn is_dragging(&self) -> bool {
        self.dragging.load(Ordering::Acquire)
    }

    pub fn grab_offset(&self) -> Option<(i32, i32)> {
        if self.has_grab_offset.load(Ordering::Acquire) {
            Some(unpack_i32_pair(self.grab_offset.load(Ordering::Acquire)))
        } else {
            None
        }
    }

    /// 坐标未记录或发生变化时更新并返回 `true`，用于去重冗余移动请求。
    pub fn update_last_pos(&self, x: i32, y: i32) -> bool {
        let packed = pack_i32_pair(x, y);
        if self.has_last_pos.load(Ordering::Acquire)
            && self.last_pos.load(Ordering::Acquire) == packed
        {
            return false;
        }
        self.last_pos.store(packed, Ordering::Release);
        self.has_last_pos.store(true, Ordering::Release);
        true
    }

    pub fn try_claim_move_post(&self) -> bool {
        self.move_posted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn clear_move_post(&self) {
        self.move_posted.store(false, Ordering::Release);
    }

    pub fn try_claim_sync_post(&self) -> bool {
        self.sync_posted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn clear_sync_post(&self) {
        self.sync_posted.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PomodoroStatusPayload {
    pub active: bool,
    pub paused: bool,
    /// `"idle" | "focus"`
    pub phase: String,
    pub remaining_sec: u32,
    pub total_sec: u32,
    pub completed_count: u32,
}

impl Default for PomodoroStatusPayload {
    fn default() -> Self {
        Self {
            active: false,
            paused: false,
            phase: "idle".to_string(),
            remaining_sec: 25 * 60,
            total_sec: 25 * 60,
            completed_count: 0,
        }
    }
}

impl PomodoroStatusPayload {
    pub fn control(&mut self, action: &str, minutes: Option<u32>) {
        if let Some(m) = minutes {
            let secs = m.clamp(1, 120) * 60;
            self.total_sec = secs;
            if !self.active || action == "start" || action == "reset" {
                self.remaining_sec = secs;
            }
        }
        match action {
            "start" => {
                self.active = true;
                self.paused = false;
                self.phase = "focus".to_string();
                if self.remaining_sec == 0 {
                    self.remaining_sec = self.total_sec;
                }
            }
            "pause" => {
                if self.active {
                    self.paused = true;
                }
            }
            "resume" => {
                if self.active {
                    self.paused = false;
                }
            }
            "toggle" => {
                if !self.active {
                    self.active = true;
                    self.paused = false;
                    self.phase = "focus".to_string();
                    if self.remaining_sec == 0 {
                        self.remaining_sec = self.total_sec;
                    }
                } else {
                    self.paused = !self.paused;
                }
            }
            "reset" => {
                self.active = false;
                self.paused = false;
                self.phase = "idle".to_string();
                self.remaining_sec = self.total_sec;
            }
            _ => {}
        }
    }

    pub fn tick_one_sec(&mut self) -> Option<bool> {
        if !self.active || self.paused {
            return None;
        }
        if self.remaining_sec > 1 {
            self.remaining_sec -= 1;
            return Some(false);
        }
        self.completed_count = self.completed_count.saturating_add(1);
        self.active = false;
        self.paused = false;
        self.phase = "idle".to_string();
        self.remaining_sec = self.total_sec;
        Some(true)
    }
}

pub struct Inner {
    pub app: AppHandle,
    pub db: Db,
    pub pet: Mutex<PetState>,
    pub character: Mutex<Option<CharacterRuntime>>,
    /// v1.3：角色元数据内存缓存，切角免重读 100+ meta.json（<1ms）。
    pub char_cache: Mutex<HashMap<String, CharacterRuntime>>,
    pub current: Mutex<Option<CurrentPlay>>,
    pub ui: Mutex<UiFlags>,
    /// 无锁拖拽共享态（Win32 窗口子类 + 后台跟踪共用）。
    pub drag_fast: Arc<DragFastState>,
    /// v1.2 常驻热区：仅内存，不落库。
    pub ui_hit_rects: Mutex<Vec<UiHitRect>>,
    /// v1.2 Agent 遥测管理器（Adapter + 分级轮询 + Diff 状态）。
    pub telemetry: Mutex<crate::telemetry::TelemetryManager>,
    /// 无阻塞遥测快照缓存：供 `telemetry_snapshot` 毫秒级读取，避免被后台 RPC 锁阻塞。
    pub telemetry_cache: Mutex<crate::telemetry::UnifiedState>,
    pub hint: Mutex<Option<FrameHint>>,
    pub masks: Mutex<MaskCache>,
    pub history: Mutex<HashMap<String, i64>>,
    #[allow(dead_code)]
    pub settings: Mutex<HashMap<String, Value>>,
    pub scheduler_tx: Mutex<mpsc::Sender<SchedulerEvent>>,
    pub last_tick_ms: Mutex<i64>,
    pub hunger_flag: Mutex<bool>,
    pub sleepy_flag: Mutex<bool>,
    pub quitting: AtomicBool,
    /// 首个 `play` 可能早于前端 listen 注册；只补播一次当前段。
    pub boot_replay_done: AtomicBool,
    /// 用于取消进行中的平滑贴边/弹出动画，保证拖拽立刻抢占。
    pub move_anim_seq: AtomicU64,
    /// 用于从休眠中即时唤醒穿透/拖拽轮询线程。
    pub passthrough_wake: (Mutex<u64>, Condvar),
    /// AI 对话短期记忆（按角色分别保留最近 6 条消息）。
    pub chat_history: Mutex<HashMap<String, Vec<crate::ai_voice::ChatTurn>>>,
    pub chat_seq: AtomicU64,
    pub last_click_voice_ms: AtomicI64,
    pub last_drag_voice_ms: AtomicI64,
    pub gpt_sovits: Arc<crate::gpt_sovits::GptSovitsRuntime>,
    pub assets_rt: Arc<crate::assets_mgr::AssetsRuntime>,
    pub pomodoro: Mutex<PomodoroStatusPayload>,
}

#[derive(Clone)]
pub struct Shared {
    pub inner: Arc<Inner>,
}

impl Shared {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app: AppHandle,
        db: Db,
        pet: PetState,
        character: Option<CharacterRuntime>,
        settings: HashMap<String, Value>,
        history: HashMap<String, i64>,
        telemetry: crate::telemetry::TelemetryManager,
        scheduler_tx: mpsc::Sender<SchedulerEvent>,
    ) -> Self {
        let mut initial_cache = HashMap::new();
        if let Some(ref ch) = character {
            initial_cache.insert(ch.id.clone(), ch.clone());
        }
        let edge_docking_enabled = settings
            .get("edgeDocking")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let ui_flags = UiFlags {
            edge_docking_enabled,
            ..Default::default()
        };
        let mut telemetry = telemetry;
        let initial_telemetry = telemetry.snapshot();
        Self {
            inner: Arc::new(Inner {
                app,
                db,
                pet: Mutex::new(pet),
                character: Mutex::new(character),
                char_cache: Mutex::new(initial_cache),
                current: Mutex::new(None),
                ui: Mutex::new(ui_flags),
                drag_fast: Arc::new(DragFastState::new()),
                ui_hit_rects: Mutex::new(Vec::new()),
                telemetry: Mutex::new(telemetry),
                telemetry_cache: Mutex::new(initial_telemetry),
                hint: Mutex::new(None),
                masks: Mutex::new(MaskCache::default()),
                history: Mutex::new(history),
                settings: Mutex::new(settings),
                scheduler_tx: Mutex::new(scheduler_tx),
                last_tick_ms: Mutex::new(now_ms()),
                hunger_flag: Mutex::new(false),
                sleepy_flag: Mutex::new(false),
                quitting: AtomicBool::new(false),
                boot_replay_done: AtomicBool::new(false),
                move_anim_seq: AtomicU64::new(0),
                passthrough_wake: (Mutex::new(0), Condvar::new()),
                chat_history: Mutex::new(HashMap::new()),
                chat_seq: AtomicU64::new(1),
                last_click_voice_ms: AtomicI64::new(0),
                last_drag_voice_ms: AtomicI64::new(0),
                gpt_sovits: Arc::new(crate::gpt_sovits::GptSovitsRuntime::default()),
                assets_rt: Arc::new(crate::assets_mgr::AssetsRuntime::default()),
                pomodoro: Mutex::new(PomodoroStatusPayload::default()),
            }),
        }
    }

    pub fn pomodoro_snapshot(&self) -> PomodoroStatusPayload {
        self.inner
            .pomodoro
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn control_pomodoro(&self, action: &str, minutes: Option<u32>) -> PomodoroStatusPayload {
        if let Ok(mut guard) = self.inner.pomodoro.lock() {
            guard.control(action, minutes);
            return guard.clone();
        }
        PomodoroStatusPayload::default()
    }

    pub fn tick_pomodoro(&self) -> Option<(PomodoroStatusPayload, bool)> {
        let mut guard = self.inner.pomodoro.lock().ok()?;
        let completed = guard.tick_one_sec()?;
        Some((guard.clone(), completed))
    }

    pub fn try_claim_click_voice(&self, now_ms: i64) -> bool {
        let prev = self.inner.last_click_voice_ms.load(Ordering::Relaxed);
        if now_ms - prev < 2800 {
            return false;
        }
        self.inner
            .last_click_voice_ms
            .compare_exchange(prev, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub fn try_claim_drag_voice(&self, now_ms: i64) -> bool {
        let prev = self.inner.last_drag_voice_ms.load(Ordering::Relaxed);
        if now_ms - prev < 4000 {
            return false;
        }
        self.inner
            .last_drag_voice_ms
            .compare_exchange(prev, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub fn chat_ai_config(&self) -> crate::ai_voice::ChatAiConfig {
        self.cached_setting("chatAiConfig")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    pub fn assets_download_config(&self) -> crate::assets_mgr::AssetsDownloadConfig {
        self.cached_setting("assetsDownloadConfig")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    pub fn chat_history_for(&self, char_id: &str) -> Vec<crate::ai_voice::ChatTurn> {
        self.inner
            .chat_history
            .lock()
            .ok()
            .and_then(|map| map.get(char_id).cloned())
            .unwrap_or_default()
    }

    pub fn push_chat_turn(&self, char_id: &str, user_text: String, reply_text: String) {
        if let Ok(mut map) = self.inner.chat_history.lock() {
            let entry = map.entry(char_id.to_string()).or_default();
            entry.push(crate::ai_voice::ChatTurn {
                role: "user".to_string(),
                content: user_text,
            });
            entry.push(crate::ai_voice::ChatTurn {
                role: "assistant".to_string(),
                content: reply_text,
            });
            if entry.len() > 6 {
                let drain_count = entry.len() - 6;
                entry.drain(0..drain_count);
            }
        }
    }

    pub fn clear_chat_history(&self) {
        if let Ok(mut map) = self.inner.chat_history.lock() {
            map.clear();
        }
    }

    pub fn next_chat_seq(&self) -> u64 {
        self.inner.chat_seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn send(&self, event: SchedulerEvent) -> bool {
        match self.inner.scheduler_tx.lock() {
            Ok(tx) => tx.send(event).is_ok(),
            Err(_) => false,
        }
    }

    pub fn snapshot(&self) -> PetState {
        self.inner
            .pet
            .lock()
            .map(|pet| pet.clone())
            .unwrap_or_default()
    }

    pub fn stats_snapshot(&self) -> PetStats {
        self.snapshot().stats
    }

    pub fn character_snapshot(&self) -> Option<CharacterRuntime> {
        self.inner
            .character
            .lock()
            .ok()
            .and_then(|character| character.clone())
    }

    pub fn schedule_snapshot(&self) -> Schedule {
        self.character_snapshot()
            .map(|character| character.schedule)
            .unwrap_or_default()
    }

    pub fn history_snapshot(&self) -> HashMap<String, i64> {
        self.inner
            .history
            .lock()
            .map(|history| history.clone())
            .unwrap_or_default()
    }

    pub fn set_character(&self, character: CharacterRuntime, history: HashMap<String, i64>) {
        if let Ok(mut slot) = self.inner.character.lock() {
            *slot = Some(character);
        }
        if let Ok(mut slot) = self.inner.history.lock() {
            *slot = history;
        }
        if let Ok(mut slot) = self.inner.current.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.inner.hint.lock() {
            *slot = None;
        }
        if let Ok(mut masks) = self.inner.masks.lock() {
            masks.clear();
        }
    }

    pub fn record_play(&self, char_id: &str, clip_id: &str, played_at_ms: i64) {
        if let Ok(mut history) = self.inner.history.lock() {
            history.insert(clip_id.to_string(), played_at_ms);
        }
        if let Err(err) = self.inner.db.record_play(char_id, clip_id, played_at_ms) {
            eprintln!("[deskpet] 播放历史落库失败: {err}");
        }
    }

    /// 把 `last_tick_ms` 到 `now` 的衰减补算掉，避免「互动落库」把未结算的时长
    /// 一并写进 `updated_at` 导致离线结算漏算。
    fn settle_since_last(&self, now: i64) {
        let last = self
            .inner
            .last_tick_ms
            .lock()
            .map(|value| *value)
            .unwrap_or(now);
        let elapsed = now - last;
        if elapsed <= 0 {
            return;
        }
        let schedule = self.schedule_snapshot();
        let sleep_from = parse_hhmm(&schedule.sleep).unwrap_or(23 * 60 + 30);
        let sleep_to = parse_hhmm(&schedule.wake).unwrap_or(8 * 60 + 30);
        let sleep_fraction = sleep_fraction_between(last, now, sleep_from, sleep_to);
        if let Ok(mut pet) = self.inner.pet.lock() {
            settle_stats(&mut pet.stats, elapsed, sleep_fraction);
        }
        if let Ok(mut slot) = self.inner.last_tick_ms.lock() {
            *slot = now;
        }
    }

    pub fn persist_pet(&self, updated_at_ms: i64) {
        self.settle_since_last(updated_at_ms);
        let pet = self.snapshot();
        let char_id = self.character_snapshot().map(|character| character.id);
        if let Err(err) = self
            .inner
            .db
            .save_pet(char_id.as_deref(), &pet, updated_at_ms)
        {
            eprintln!("[deskpet] 宠物状态落库失败: {err}");
        }
    }

    pub fn emit_stats(&self) {
        let stats = self.stats_snapshot();
        let _ = self.inner.app.emit("stats", &stats);
    }

    pub fn emit_bubble(&self, text: impl Into<String>, ttl_ms: u64) {
        let _ = self.inner.app.emit(
            "bubble",
            BubblePayload {
                text: text.into(),
                ttl_ms,
            },
        );
    }

    /// 后台 60s 结算：从 `last_tick_ms` 到现在，按作息计算睡眠占比，落库并 emit。
    pub fn tick_stats(&self) {
        let now = now_ms();
        let last = self
            .inner
            .last_tick_ms
            .lock()
            .map(|value| *value)
            .unwrap_or(now);
        if now - last < 1_000 {
            return;
        }
        self.settle_since_last(now);
        let stats = self.stats_snapshot();
        if let Err(err) = self.inner.db.save_stats(&stats, now) {
            eprintln!("[deskpet] 数值落库失败: {err}");
        }
        self.emit_stats();
        self.refresh_thresholds();
    }

    /// 饱食/精力跌破 25 时各触发一次，回到 30 以上才重新武装，避免刷屏。
    pub fn refresh_thresholds(&self) {
        let stats = self.stats_snapshot();
        let mut fire_hungry = false;
        let mut fire_sleepy = false;

        if let Ok(mut armed) = self.inner.hunger_flag.lock() {
            if stats.satiety < 25.0 {
                if !*armed {
                    *armed = true;
                    fire_hungry = true;
                }
            } else if stats.satiety >= 30.0 {
                *armed = false;
            }
        }
        if let Ok(mut armed) = self.inner.sleepy_flag.lock() {
            if stats.energy < 25.0 {
                if !*armed {
                    *armed = true;
                    fire_sleepy = true;
                }
            } else if stats.energy >= 30.0 {
                *armed = false;
            }
        }

        if fire_hungry {
            self.send(SchedulerEvent::NeedHungry);
        }
        if fire_sleepy {
            self.send(SchedulerEvent::NeedSleepy);
        }
    }

    /// 供穿透线程使用：当前段 + 当前帧（优先 `frame_hint`，否则按时间推算）。
    pub fn frame_for_hint(&self, now: i64) -> Option<(CurrentPlay, u32)> {
        let current = self.inner.current.lock().ok()?.clone()?;
        let hint = self.inner.hint.lock().ok()?.clone();
        let last_frame = current.frames.saturating_sub(1);
        let elapsed_ms = (now - current.start_ms).max(0) as f64;
        let elapsed_frame = if current.frames > 0 && current.fps > 0.0 {
            ((elapsed_ms * current.fps / 1000.0).floor() as u32).min(last_frame)
        } else if current.frames > 0 && current.duration_ms > 0 {
            ((elapsed_ms / current.duration_ms as f64) * current.frames as f64).floor() as u32
        } else {
            0
        }
        .min(last_frame);
        let frame = match hint {
            Some(hint) if hint.clip_id == current.clip_id => hint.frame.min(last_frame),
            _ => elapsed_frame,
        };
        Some((current, frame))
    }

    pub fn install_current(&self, current: CurrentPlay) {
        if let Ok(mut slot) = self.inner.current.lock() {
            *slot = Some(current);
        }
        if let Ok(mut slot) = self.inner.hint.lock() {
            *slot = None;
        }
    }

    /// 重播当前段（带已播放偏移），用于前端 listen 注册晚于首个 `play` 的启动窗口。
    /// v1.3：按 char_id 命中缓存或首次读盘后缓存，避免切角重复读 100+ 文件。
    pub fn load_character_cached(
        &self,
        assets_root: &std::path::Path,
        char_id: &str,
    ) -> Result<CharacterRuntime, String> {
        if let Ok(cache) = self.inner.char_cache.lock() {
            if let Some(hit) = cache.get(char_id) {
                return Ok(hit.clone());
            }
        }
        let loaded = crate::model::load_character(assets_root, char_id)?;
        if let Ok(mut cache) = self.inner.char_cache.lock() {
            cache.insert(char_id.to_string(), loaded.clone());
        }
        Ok(loaded)
    }

    pub fn replay_current(&self) {
        if self.is_quitting() {
            return;
        }
        let Some(current) = self.inner.current.lock().ok().and_then(|slot| slot.clone()) else {
            return;
        };
        let elapsed = (now_ms() - current.start_ms).max(0) as u64;
        let _ = self.inner.app.emit(
            "play",
            PlayPayload {
                char_id: current.char_id,
                clip_id: current.clip_id,
                start_at_ms: elapsed,
                mirror: current.mirror,
                audio: current.audio,
                hold_loop: current.hold_loop,
            },
        );
    }

    /// 进程生命周期内最多补播一次；延迟 600ms 给前端注册监听。
    pub fn schedule_boot_replay(&self) {
        if self.inner.boot_replay_done.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            shared.replay_current();
        });
    }

    pub fn drag_fast(&self) -> Arc<DragFastState> {
        Arc::clone(&self.inner.drag_fast)
    }

    pub fn begin_drag(&self, grab_offset: Option<(i32, i32)>, now: i64) {
        self.bump_move_anim_seq();
        if let Ok(mut ui) = self.inner.ui.lock() {
            ui.dragging = true;
            ui.drag_grab_offset = grab_offset;
            ui.drag_started_ms = now;
        }
        self.inner.drag_fast.begin(grab_offset);
        self.wake_passthrough();
    }

    pub fn drag_state(&self) -> Option<(Option<(i32, i32)>, i64)> {
        self.inner.ui.lock().ok().and_then(|ui| {
            if ui.dragging {
                Some((ui.drag_grab_offset, ui.drag_started_ms))
            } else {
                None
            }
        })
    }

    /// 幂等结束拖拽：只有此前确实处于拖拽态才返回 true，防止前端与后端轮询重复触发 dragEnd。
    pub fn end_drag(&self) -> bool {
        self.inner.drag_fast.end();
        let was = if let Ok(mut ui) = self.inner.ui.lock() {
            let was = ui.dragging;
            ui.dragging = false;
            ui.drag_grab_offset = None;
            ui.drag_started_ms = 0;
            was
        } else {
            false
        };
        if was {
            self.wake_passthrough();
        }
        was
    }

    pub fn set_ui_interactive(&self, interactive: bool) {
        if let Ok(mut ui) = self.inner.ui.lock() {
            ui.ui_interactive = interactive;
        }
        self.wake_passthrough();
    }

    pub fn edge_docking_enabled(&self) -> bool {
        self.inner
            .ui
            .lock()
            .map(|ui| ui.edge_docking_enabled)
            .unwrap_or(true)
    }

    pub fn set_edge_docking_enabled(&self, enabled: bool) {
        if let Ok(mut ui) = self.inner.ui.lock() {
            ui.edge_docking_enabled = enabled;
        }
    }

    pub fn edge_side(&self) -> Option<EdgeSide> {
        self.inner.ui.lock().ok().and_then(|ui| ui.edge_side)
    }

    pub fn set_edge_side(&self, side: Option<EdgeSide>) {
        if let Ok(mut ui) = self.inner.ui.lock() {
            ui.edge_side = side;
        }
    }

    pub fn panel_state(&self) -> (bool, String) {
        self.inner
            .ui
            .lock()
            .map(|ui| (ui.panel_open, ui.panel_tab.clone()))
            .unwrap_or((false, "status".to_string()))
    }

    pub fn set_panel_state(&self, open: bool, tab: Option<String>) -> (bool, String) {
        if let Ok(mut ui) = self.inner.ui.lock() {
            ui.panel_open = open;
            if let Some(t) = tab {
                if !t.trim().is_empty() {
                    ui.panel_tab = t;
                }
            }
            (ui.panel_open, ui.panel_tab.clone())
        } else {
            (open, tab.unwrap_or_else(|| "status".to_string()))
        }
    }

    pub fn bump_move_anim_seq(&self) -> u64 {
        self.inner
            .move_anim_seq
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }

    pub fn current_move_anim_seq(&self) -> u64 {
        self.inner.move_anim_seq.load(Ordering::Acquire)
    }

    pub fn current_clip_id(&self) -> Option<String> {
        self.inner
            .current
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|c| c.clip_id.clone()))
    }

    pub fn cached_setting(&self, key: &str) -> Option<Value> {
        self.inner
            .settings
            .lock()
            .ok()
            .and_then(|map| map.get(key).cloned())
    }

    pub fn set_cached_setting(&self, key: &str, value: Value) {
        if let Ok(mut map) = self.inner.settings.lock() {
            map.insert(key.to_string(), value);
        }
    }

    /// 前端上报常驻热区（迷你徽章等），只存内存，不落库。
    pub fn set_ui_hit_rects(&self, rects: Vec<UiHitRect>) {
        if let Ok(mut slot) = self.inner.ui_hit_rects.lock() {
            *slot = rects;
        }
    }

    pub fn ui_hit_test(&self, u: f64, v: f64) -> bool {
        self.inner
            .ui_hit_rects
            .lock()
            .map(|rects| ui_rect_hit(&rects, u, v))
            .unwrap_or(false)
    }

    pub fn update_telemetry_cache(&self, state: &crate::telemetry::UnifiedState) {
        if let Ok(mut cache) = self.inner.telemetry_cache.lock() {
            *cache = state.clone();
        }
    }

    pub fn telemetry_snapshot(&self) -> crate::telemetry::UnifiedState {
        self.inner
            .telemetry_cache
            .lock()
            .map(|cache| cache.clone())
            .unwrap_or_else(|_| crate::telemetry::UnifiedState::offline("antigravity", now_ms()))
    }

    pub fn telemetry_refresh(&self) -> crate::telemetry::RefreshOutcome {
        match self.inner.telemetry.lock() {
            Ok(mut manager) => {
                let outcome = manager.refresh();
                self.update_telemetry_cache(&outcome.state);
                outcome
            }
            Err(_) => crate::telemetry::RefreshOutcome {
                state: crate::telemetry::UnifiedState::offline("antigravity", now_ms()),
                changed: false,
                scheduler_events: Vec::new(),
            },
        }
    }

    pub fn telemetry_set_source(
        &self,
        source: &str,
    ) -> Result<crate::telemetry::UnifiedState, String> {
        let mut manager = self
            .inner
            .telemetry
            .lock()
            .map_err(|_| "遥测状态锁已损坏".to_string())?;
        if source.eq_ignore_ascii_case(manager.source()) {
            let snap = manager.snapshot();
            self.update_telemetry_cache(&snap);
            Ok(snap)
        } else {
            Err(format!(
                "暂不支持遥测源 \"{source}\"，当前仅支持 \"{}\"",
                manager.source()
            ))
        }
    }

    pub fn passthrough_locked(&self) -> bool {
        self.inner
            .ui
            .lock()
            .map(|ui| ui.dragging || ui.ui_interactive)
            .unwrap_or(false)
    }

    pub fn wake_passthrough(&self) {
        let (lock, cvar) = &self.inner.passthrough_wake;
        if let Ok(mut seq) = lock.lock() {
            *seq = seq.wrapping_add(1);
            cvar.notify_all();
        }
    }

    pub fn wait_passthrough(&self, last_seq: &mut u64, timeout: Duration) {
        let (lock, cvar) = &self.inner.passthrough_wake;
        if let Ok(guard) = lock.lock() {
            let target = *last_seq;
            if let Ok((guard, _)) =
                cvar.wait_timeout_while(guard, timeout, |seq| *seq == target && !self.is_quitting())
            {
                *last_seq = *guard;
            }
        } else {
            std::thread::sleep(timeout);
        }
    }

    pub fn mark_quitting(&self) {
        let already = self.inner.quitting.swap(true, Ordering::SeqCst);
        self.wake_passthrough();
        if !already {
            let cfg = self.chat_ai_config();
            if cfg.gpt_sovits.auto_stop_on_exit {
                self.inner
                    .gpt_sovits
                    .cancel_install
                    .store(true, Ordering::SeqCst);
                crate::gpt_sovits::stop_service(&cfg.gpt_sovits.api_url, &self.inner.gpt_sovits);
            }
        }
    }

    pub fn is_quitting(&self) -> bool {
        self.inner.quitting.load(Ordering::SeqCst)
    }
}

/// 调度器线程重放 `Action::Play` 时更新共享当前段。
pub fn install_play(
    shared: &Shared,
    character_id: &str,
    request: &crate::scheduler::PlayRequest,
    now: i64,
) {
    shared.install_current(CurrentPlay {
        char_id: character_id.to_string(),
        clip_id: request.clip_id.clone(),
        start_ms: now,
        mirror: request.mirror,
        frames: request.frames,
        fps: request.fps,
        duration_ms: request.duration_ms,
        audio: request.audio.clone(),
        hold_loop: request.hold_loop,
    });
}

#[cfg(test)]
mod tests {
    use super::{pack_i32_pair, unpack_i32_pair, DragFastState};

    #[test]
    fn pack_unpack_i32_pair_handles_negative_and_extremes() {
        let cases = [
            (0, 0),
            (120, 450),
            (-1920, -1080),
            (-1, 640),
            (640, -1),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
        ];
        for (a, b) in cases {
            let packed = pack_i32_pair(a, b);
            assert_eq!(unpack_i32_pair(packed), (a, b));
        }
    }

    #[test]
    fn drag_fast_state_tracks_lifecycle_and_coalescing() {
        let state = DragFastState::new();
        assert!(!state.is_dragging());
        assert_eq!(state.grab_offset(), None);

        state.begin(Some((-320, 48)));
        assert!(state.is_dragging());
        assert_eq!(state.grab_offset(), Some((-320, 48)));

        assert!(state.update_last_pos(-100, 200));
        assert!(!state.update_last_pos(-100, 200));
        assert!(state.update_last_pos(-101, 200));

        assert!(state.try_claim_move_post());
        assert!(!state.try_claim_move_post());
        state.clear_move_post();
        assert!(state.try_claim_move_post());

        state.end();
        assert!(!state.is_dragging());
        assert_eq!(state.grab_offset(), None);
        assert!(state.update_last_pos(-101, 200));
        assert!(state.try_claim_move_post());
    }

    #[test]
    fn pomodoro_countdown_and_completion_reset() {
        let mut p = super::PomodoroStatusPayload::default();
        assert!(!p.active);
        assert_eq!(p.remaining_sec, 1500);

        p.control("start", Some(1));
        assert!(p.active);
        assert!(!p.paused);
        assert_eq!(p.phase, "focus");
        assert_eq!(p.total_sec, 60);
        assert_eq!(p.remaining_sec, 60);

        p.control("pause", None);
        assert!(p.paused);
        assert_eq!(p.tick_one_sec(), None);

        p.control("resume", None);
        assert!(!p.paused);
        for _ in 0..59 {
            assert_eq!(p.tick_one_sec(), Some(false));
        }
        assert_eq!(p.tick_one_sec(), Some(true));
        assert!(!p.active);
        assert_eq!(p.phase, "idle");
        assert_eq!(p.completed_count, 1);
        assert_eq!(p.remaining_sec, 60);
    }
}
