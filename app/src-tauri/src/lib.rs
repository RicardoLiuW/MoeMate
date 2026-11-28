//! Deskpet V1 Rust 后端。
//!
//! 契约：`docs/v1_contract.md` v1.2
//! - 11 个 command：catalog_get / char_switch / pet_state_get / pet_interact /
//!   action_play / settings_set / chat_send / hit_test_mask /
//!   telemetry_get / telemetry_refresh / telemetry_set_source
//! - 5 个事件：play / bubble / stats / telemetry（Rust→前端）、frame_hint（前端→Rust）
//!
//! 模块：
//! - `db`        SQLite 持久化（rusqlite bundled）
//! - `stats`     四维结算、12h 离线上限、作息
//! - `scheduler` 7 档优先级状态机、Agent 告警冷却、2h 去重加权
//! - `mask`      mask.bin MSB-first 解析与 16 段 LRU
//! - `passthrough` 33ms GetCursorPos 穿透轮询 + uiHitRects 常驻热区
//! - `foreground`  Windows 前台 IDE/终端检测
//! - `telemetry`  v1.2 Adapter / UnifiedState / Antigravity language_server
//! - `state`     命令 / 后台线程共享运行态
//! - `model`     与 ipc.ts 对齐的数据模型

mod ai_voice;
mod assets_mgr;
mod companion;
mod db;
mod foreground;
mod fish_speech;
mod http_hook;
mod mask;
mod model;
mod passthrough;
mod scheduler;
mod state;
mod stats;
pub mod telemetry;
mod vram;
mod window_layout;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Listener, LogicalSize, Manager, PhysicalPosition, State, WindowEvent};

use db::Db;
use model::{Catalog, CharState, ChatReply, PetPos, PetState, PlayPayload};
use scheduler::{
    due_schedule_slots, match_chat, pick_by_category, pick_for_binding, Action, PlayRequest, Rng,
    SchedulerCore, SchedulerEvent,
};
use state::{Shared, UiHitRect};
use stats::{
    clamp_stat, local_date_key, local_hhmm, now_ms, parse_hhmm, settle_stats,
    sleep_fraction_between,
};
use telemetry::antigravity::AntigravityAdapter;
use telemetry::{TelemetryManager, TelemetryStatus, UnifiedState};

const HISTORY_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const BASE_WIDTH: f64 = 1280.0;
const BASE_HEIGHT: f64 = 720.0;
/// v1.5：角色头顶专属灵动岛安全带高度（逻辑像素，不随角色缩放压扁）。
const TOP_CROWN_HEIGHT: f64 = 90.0;

/* ------------------------------------------------------------------ */
/* 路径与安全                                                          */
/* ------------------------------------------------------------------ */

fn project_root() -> PathBuf {
    if let Ok(root) = std::env::var("DESKPET_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn user_data_root() -> PathBuf {
    Db::default_path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

pub(crate) fn assets_root() -> PathBuf {
    if let Ok(dir) = std::env::var("DESKPET_ASSETS") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if cfg!(debug_assertions) {
        let dev = project_root().join("assets");
        if dev.is_dir() {
            return dev;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let portable = parent.join("assets");
            if portable.join("chars").is_dir() {
                return portable;
            }
        }
    }
    let writable = user_data_root().join("assets");
    let _ = fs::create_dir_all(writable.join("chars"));
    writable
}

pub(crate) fn base_root() -> PathBuf {
    if cfg!(debug_assertions) {
        let dev = project_root();
        if dev.join("assets").is_dir() {
            return dev;
        }
    }
    let writable = user_data_root();
    let _ = fs::create_dir_all(&writable);
    writable
}

pub(crate) fn ref_voices_dir(shared: &Shared) -> PathBuf {
    shared
        .inner
        .db
        .path()
        .parent()
        .map(|p| p.join("ref_voices"))
        .unwrap_or_else(|| base_root().join("ref_voices"))
}

pub(crate) fn voice_cache_dir(shared: &Shared) -> PathBuf {
    shared
        .inner
        .db
        .path()
        .parent()
        .map(|p| p.join("voice_cache"))
        .unwrap_or_else(|| base_root().join("voice_cache"))
}

pub(crate) fn dispatch_daily_scene_voice(
    shared: &Shared,
    char_id: &str,
    scene_key: &str,
    default_bubble: Option<&str>,
    ttl_ms: u64,
    force: bool,
) {
    let root = assets_root();
    let fallback_key = match scene_key {
        "pomodoro_done" | "done_2" => Some("done"),
        "work_start_2" => Some("work_start"),
        _ => None,
    };
    let cfg = shared.chat_ai_config();
    if !cfg.tts_enabled {
        return;
    }

    let manifest_line = ai_voice::load_voice_manifest_scene_for_lang(
        &root,
        char_id,
        scene_key,
        &cfg.builtin_voice_lang,
    )
    .or_else(|| {
        fallback_key.and_then(|fk| {
            ai_voice::load_voice_manifest_scene_for_lang(
                &root,
                char_id,
                fk,
                &cfg.builtin_voice_lang,
            )
        })
    })
    .map(|(line, _)| line);
    let zh_line = default_bubble
        .map(str::to_string)
        .or(manifest_line)
        .or_else(|| ai_voice::builtin_zh_voice_for_scene(char_id, scene_key).map(str::to_string))
        .unwrap_or_default();

    if !zh_line.is_empty() {
        shared.emit_bubble(zh_line.clone(), ttl_ms);
    }

    if shared.snapshot().muted {
        return;
    }

    shared.mark_any_voice_played(now_ms());

    let b_root = base_root();
    let c_dir = voice_cache_dir(shared);
    let shared_clone = shared.clone();
    let cid = char_id.to_string();
    let skey = scene_key.to_string();
    std::thread::spawn(move || {
        if let Some(payload) = ai_voice::build_scene_tts_payload(
            &root,
            &b_root,
            &c_dir,
            &cfg,
            &cid,
            &skey,
            &zh_line,
            force,
            &shared_clone.inner.fish_speech,
        ) {
            let _ = shared_clone.inner.app.emit_to("main", "pet_tts", &payload);
        }
    });
}

fn safe_component(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.contains('\0')
    {
        return Err(format!("非法路径片段: {value}"));
    }
    Ok(())
}

fn setup_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::Other,
        message.into(),
    ))
}

/* ------------------------------------------------------------------ */
/* 8 个 command                                                        */
/* ------------------------------------------------------------------ */

fn load_catalog_fast(root: &PathBuf) -> Catalog {
    let path = root.join("catalog.json");
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(cat) = serde_json::from_str::<Catalog>(&raw) {
            if !cat.chars.is_empty() {
                return cat;
            }
        }
    }
    model::scan_catalog(root)
}

/// catalog_get：读取 `assets/catalog.json`；缺失时扫描 `assets/chars` 生成。
#[tauri::command]
fn catalog_get() -> Result<Catalog, String> {
    let root = assets_root();
    let path = root.join("catalog.json");
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<Catalog>(&raw)
            .map_err(|err| format!("解析失败 {}: {err}", path.display())),
        Err(_) => Ok(model::scan_catalog(&root)),
    }
}

/// char_switch：切换角色，返回 character.json + 全部 clip meta。
#[tauri::command]
fn char_switch(char_id: String, shared: State<'_, Shared>) -> Result<CharState, String> {
    safe_component(&char_id)?;
    let character = shared.load_character_cached(&assets_root(), &char_id)?;
    let now = now_ms();
    let history = shared
        .inner
        .db
        .load_history(&char_id, now - HISTORY_LOOKBACK_MS)
        .unwrap_or_default();

    shared.set_character(character.clone(), history);
    if let Err(err) = shared.inner.db.save_char(&char_id) {
        eprintln!("[deskpet] 保存当前角色失败: {err}");
    }
    shared.send(SchedulerEvent::CharacterChanged(Box::new(
        character.clone(),
    )));
    shared.schedule_boot_replay();
    let char_state = character.to_char_state();
    let _ = shared.inner.app.emit("pet_char_switched", &char_state);

    let cfg = shared.chat_ai_config();
    if cfg.tts_mode == "fish_speech" && ai_voice::normalize_vram_mode(&cfg.vram_mode) != "zero_vram" {
        // Fish Speech uses zero-shot reference audio per request; no weight hot-swap needed.
        let _ = &char_id;
    }

    Ok(char_state)
}

/// pet_state_get：四维 + 位置 + 缩放 + 置顶/静音。
#[tauri::command]
fn pet_state_get(shared: State<'_, Shared>) -> Result<PetState, String> {
    let needs_ready = shared
        .inner
        .current
        .lock()
        .map(|current| current.is_none())
        .unwrap_or(false);
    if needs_ready {
        // 前端真正开始拉状态时补发 APP_READY，避免 setup 早于监听注册。
        shared.send(SchedulerEvent::AppReady);
    }
    Ok(shared.snapshot())
}

/// pet_interact：click / dragStart / dragEnd / feed。
#[tauri::command]
fn pet_interact(
    kind: String,
    x: f64,
    y: f64,
    shared: State<'_, Shared>,
    app: tauri::AppHandle,
) -> Result<PetState, String> {
    let now = now_ms();
    let mut bubble: Option<String> = None;
    let mut drag_ended_now = false;
    let mut was_edge_docked = false;

    match kind.as_str() {
        "click" => {
            {
                let mut pet = shared
                    .inner
                    .pet
                    .lock()
                    .map_err(|_| "宠物状态锁已损坏".to_string())?;
                // 这里的 x/y 是窗口内 CSS 坐标，不能覆盖窗口在屏幕上的位置。
                pet.stats.mood = clamp_stat(pet.stats.mood + 2.0);
                pet.stats.affinity = clamp_stat(pet.stats.affinity + 1.0);
            }
            was_edge_docked = shared.edge_side().is_some();
            if was_edge_docked {
                window_layout::undock_from_edge(&shared, &app);
            }
        }
        "dragStart" => {
            // 右键硬拦截：非物理主键按下或副键处于按下状态时，绝不进入拖拽。
            if !passthrough::is_primary_button_down() || passthrough::is_secondary_button_down() {
                return Ok(shared.snapshot());
            }
            if shared.edge_side().is_some() {
                shared.set_edge_side(None);
                if let Some(win) = app.get_webview_window("main") {
                    window_layout::emit_edge_dock_to_main(&shared, &win);
                }
            }
            if shared.panel_state().0 {
                window_layout::hide_panel(&app, &shared);
            }
            let grab_offset = app.get_webview_window("main").and_then(|win| {
                let win_pos = win.outer_position().ok()?;
                let win_size = win.outer_size().ok()?;
                let scale = win.scale_factor().unwrap_or(1.0);
                let cand_ox = (x * scale).round() as i32;
                let cand_oy = (y * scale).round() as i32;
                let valid_local = x.is_finite()
                    && y.is_finite()
                    && cand_ox >= 0
                    && cand_oy >= 0
                    && (cand_ox as u32) <= win_size.width
                    && (cand_oy as u32) <= win_size.height;
                if valid_local {
                    Some((cand_ox, cand_oy))
                } else {
                    let (cx, cy) = passthrough::cursor_point()?;
                    Some((cx - win_pos.x, cy - win_pos.y))
                }
            });
            shared.begin_drag(grab_offset, now);
            if let Some(win) = app.get_webview_window("main") {
                passthrough::apply_drag_move_on_main_thread(&shared, &win);
            }
            if !shared.snapshot().muted
                && shared.chat_ai_config().tts_enabled
                && shared.try_claim_drag_voice(now)
            {
                if let Some(ch) = shared.character_snapshot() {
                    dispatch_daily_scene_voice(&shared, &ch.id, "drag", None, 2800, false);
                }
            }
            shared.send(SchedulerEvent::UserDragStart { x, y });
            return Ok(shared.snapshot());
        }
        "dragEnd" => {
            drag_ended_now = shared.end_drag();
            if !drag_ended_now {
                return Ok(shared.snapshot());
            }
            if let Some(win) = app.get_webview_window("main") {
                window_layout::finalize_drag_release(&shared, &win, now);
                return Ok(shared.snapshot());
            }
            passthrough::sync_after_drag(&shared);
            let final_pos = PetPos { x, y };
            {
                let mut pet = shared
                    .inner
                    .pet
                    .lock()
                    .map_err(|_| "宠物状态锁已损坏".to_string())?;
                pet.pos = final_pos;
            }
            let _ = shared
                .inner
                .db
                .save_setting("pos", &serde_json::json!({ "x": x, "y": y }));
        }
        "feed" => {
            let mut pet = shared
                .inner
                .pet
                .lock()
                .map_err(|_| "宠物状态锁已损坏".to_string())?;
            pet.stats.satiety = clamp_stat(pet.stats.satiety + 15.0);
            pet.stats.energy = clamp_stat(pet.stats.energy + 3.0);
            pet.stats.affinity = clamp_stat(pet.stats.affinity + 1.0);
            bubble = Some("好吃！再来一口～".to_string());
        }
        other => return Err(format!("未知互动类型: {other}")),
    }

    match kind.as_str() {
        "click" => {
            if !was_edge_docked {
                shared.send(SchedulerEvent::UserClick { x, y });
            }
        }
        "dragStart" => {
            shared.send(SchedulerEvent::UserDragStart { x, y });
        }
        "dragEnd" => {
            shared.persist_pet(now);
            if drag_ended_now {
                shared.send(SchedulerEvent::UserDragEnd { x, y });
            }
        }
        "feed" => {
            let _ = app.emit_to("main", "pet_feed_fx", true);
            if let Some(character) = shared.character_snapshot() {
                let history = shared.history_snapshot();
                let mut rng = Rng::new((now as u64) ^ 0xF00D_1234);
                if let Some(clip) = pick_for_binding(&character, "eat", &history, now, &mut rng) {
                    shared.send(SchedulerEvent::PlayCommand { clip_id: clip.id });
                }
                dispatch_daily_scene_voice(
                    &shared,
                    &character.id,
                    "feed",
                    bubble.as_deref(),
                    3400,
                    false,
                );
                bubble = None;
            }
            if let Some(text) = bubble {
                shared.emit_bubble(text, 3400);
            }
        }
        _ => {}
    }

    shared.persist_pet(now);
    shared.emit_stats();
    shared.refresh_thresholds();
    Ok(shared.snapshot())
}

/// action_play：相册/指令触发播放。
#[tauri::command]
fn action_play(clip_id: String, shared: State<'_, Shared>) -> Result<bool, String> {
    safe_component(&clip_id)?;
    let character = shared
        .character_snapshot()
        .ok_or_else(|| "尚未切换角色".to_string())?;
    if !character.clip_map.contains_key(&clip_id) {
        return Err(format!("clip 不存在: {clip_id}"));
    }
    shared.send(SchedulerEvent::PlayCommand {
        clip_id: clip_id.clone(),
    });
    Ok(true)
}

/// settings_set：实时生效并落库；`uiInteractive` 仅存内存。
#[tauri::command]
fn settings_set(
    key: String,
    value: Value,
    shared: State<'_, Shared>,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    if key.trim().is_empty() {
        return Err("key 不能为空".to_string());
    }
    let now = now_ms();
    let window = app.get_webview_window("main");

    match key.as_str() {
        "scale" => {
            let scale = value
                .as_f64()
                .ok_or_else(|| "scale 必须是数字".to_string())?
                .clamp(0.25, 2.0);
            if let Some(window) = &window {
                window
                    .set_size(LogicalSize::new(
                        BASE_WIDTH * scale,
                        BASE_HEIGHT * scale + TOP_CROWN_HEIGHT,
                    ))
                    .map_err(|err| format!("调整窗口尺寸失败: {err}"))?;
            }
            let mut pet = shared
                .inner
                .pet
                .lock()
                .map_err(|_| "宠物状态锁已损坏".to_string())?;
            pet.scale = scale;
            drop(pet);
            window_layout::reposition_after_scale(&shared, &app);
        }
        "pinned" => {
            let pinned = value
                .as_bool()
                .ok_or_else(|| "pinned 必须是布尔值".to_string())?;
            if let Some(window) = &window {
                window
                    .set_always_on_top(pinned)
                    .map_err(|err| format!("设置置顶失败: {err}"))?;
            }
            if let Some(panel) = app.get_webview_window("panel") {
                let _ = panel.set_always_on_top(pinned);
            }
            if let Some(settings_win) = app.get_webview_window("settings") {
                let _ = settings_win.set_always_on_top(pinned);
            }
            let mut pet = shared
                .inner
                .pet
                .lock()
                .map_err(|_| "宠物状态锁已损坏".to_string())?;
            pet.pinned = pinned;
            drop(pet);
        }
        "muted" => {
            let muted = value
                .as_bool()
                .ok_or_else(|| "muted 必须是布尔值".to_string())?;
            let mut pet = shared
                .inner
                .pet
                .lock()
                .map_err(|_| "宠物状态锁已损坏".to_string())?;
            pet.muted = muted;
            drop(pet);
            let _ = app.emit("pet_muted", muted);
        }
        "pos" => {
            let x = value
                .get("x")
                .and_then(Value::as_f64)
                .ok_or_else(|| "pos.x 必须是数字".to_string())?;
            let y = value
                .get("y")
                .and_then(Value::as_f64)
                .ok_or_else(|| "pos.y 必须是数字".to_string())?;
            if let Some(window) = &window {
                window
                    .set_position(PhysicalPosition::new(x as i32, y as i32))
                    .map_err(|err| format!("设置窗口位置失败: {err}"))?;
            }
            let mut pet = shared
                .inner
                .pet
                .lock()
                .map_err(|_| "宠物状态锁已损坏".to_string())?;
            pet.pos = PetPos { x, y };
            drop(pet);
        }
        "edgeDocking" => {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "edgeDocking 必须是布尔值".to_string())?;
            shared.set_edge_docking_enabled(enabled);
            shared.set_cached_setting("edgeDocking", Value::Bool(enabled));
            if !enabled && shared.edge_side().is_some() {
                window_layout::undock_from_edge(&shared, &app);
            } else if let Some(win) = &window {
                window_layout::emit_edge_dock_to_main(&shared, win);
            }
        }
        "appearance" => {
            let mode = value
                .as_str()
                .ok_or_else(|| "appearance 必须是 \"light\" 或 \"dark\"".to_string())?;
            if mode != "light" && mode != "dark" {
                return Err("appearance 仅支持 \"light\" 或 \"dark\"".to_string());
            }
            shared.set_cached_setting("appearance", Value::String(mode.to_string()));
            let _ = app.emit("pet_appearance", mode);
        }
        "panelOpen" => {
            window_layout::handle_panel_open_setting(&app, &shared, &value)?;
            if value.get("openChatSettings").and_then(Value::as_bool) == Some(true) {
                let _ = app.emit("pet_open_chat_settings", true);
            }
        }
        "panelTab" => {
            let tab = value
                .as_str()
                .ok_or_else(|| "panelTab 必须是字符串".to_string())?;
            let (open, _) = shared.panel_state();
            if open {
                let (_, updated) = shared.set_panel_state(true, Some(tab.to_string()));
                let _ = app.emit(
                    "panel_state",
                    window_layout::PanelStateEvent {
                        open: true,
                        tab: updated,
                    },
                );
            }
        }
        "panelReady" => {
            let root = assets_root();
            let _ = app.emit_to("panel", "pet_assets_root", root.to_string_lossy().to_string());
            if let Some(ch) = shared.character_snapshot() {
                let _ = app.emit_to("panel", "pet_char_switched", &ch.to_char_state());
            }
            let _ = app.emit_to("panel", "panel_clip_sync", shared.current_clip_id());
            let _ = app.emit_to("panel", "stats", &shared.stats_snapshot());
            let _ = app.emit_to("panel", "telemetry", &shared.telemetry_snapshot());
            let (open, tab) = shared.panel_state();
            let _ = app.emit_to(
                "panel",
                "panel_state",
                window_layout::PanelStateEvent { open, tab },
            );
            if let Some(mode) = shared
                .cached_setting("appearance")
                .and_then(|v| v.as_str().map(str::to_string))
            {
                let _ = app.emit_to("panel", "pet_appearance", mode);
            }
            let _ = app.emit_to("panel", "pet_chat_config", &shared.chat_ai_config());
            let _ = app.emit_to("panel", "pet_pomodoro", &shared.pomodoro_snapshot());
        }
        "mainReady" => {
            let root = assets_root();
            let _ = app.asset_protocol_scope().allow_directory(&root, true);
            let _ = app.emit_to("main", "pet_assets_root", root.to_string_lossy().to_string());
            let assets_st = assets_mgr::inspect_assets_status(
                &root,
                &shared.assets_download_config(),
                &shared.inner.assets_rt,
            );
            let _ = app.emit_to("main", "pet_assets_status", &assets_st);
            if let Some(win) = &window {
                window_layout::emit_edge_dock_to_main(&shared, win);
            }
            let (open, tab) = shared.panel_state();
            let _ = app.emit_to(
                "main",
                "panel_state",
                window_layout::PanelStateEvent { open, tab },
            );
            let _ = app.emit_to("main", "pet_chat_config", &shared.chat_ai_config());
            let _ = app.emit_to("main", "pet_auto_start", autostart::is_windows_auto_start_enabled());
            let _ = app.emit_to("main", "pet_pomodoro", &shared.pomodoro_snapshot());
            let _ = app.emit_to("main", "pet_global_hotkey", hotkey::is_enabled(&shared));
        }
        "autoStart" => {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "autoStart 必须是布尔值".to_string())?;
            autostart::set_windows_auto_start(enabled)?;
            let _ = app.emit("pet_auto_start", enabled);
        }
        "globalHotkey" => {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "globalHotkey 必须是布尔值".to_string())?;
            shared.set_cached_setting("globalHotkey", Value::Bool(enabled));
            let _ = shared
                .inner
                .db
                .save_setting("globalHotkey", &Value::Bool(enabled));
            let _ = app.emit("pet_global_hotkey", enabled);
        }
        "pomodoroControl" => {
            let action = value
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("toggle");
            let minutes = value
                .get("minutes")
                .and_then(Value::as_u64)
                .map(|m| m as u32);
            let before = shared.pomodoro_snapshot();
            let snap = shared.control_pomodoro(action, minutes);
            let _ = app.emit("pet_pomodoro", &snap);
            if !before.active && snap.active && !snap.paused {
                shared.emit_bubble(
                    format!("番茄钟已开启：专注 {} 分钟，一起加油吧！", snap.total_sec / 60),
                    3600,
                );
            }
        }
        "chatAiConfig" => {
            let cfg: ai_voice::ChatAiConfig = serde_json::from_value(value.clone())
                .map_err(|_| "chatAiConfig 格式非法".to_string())?;
            shared.set_cached_setting("chatAiConfig", value.clone());
            let next_muted = !(cfg.tts_enabled && cfg.auto_play_voice);
            if let Ok(mut pet) = shared.inner.pet.lock() {
                pet.muted = next_muted;
            }
            let _ = shared
                .inner
                .db
                .save_setting("muted", &Value::Bool(next_muted));
            let _ = app.emit("pet_muted", next_muted);
            let _ = app.emit("pet_chat_config", &cfg);
            if ai_voice::normalize_vram_mode(&cfg.vram_mode) == "zero_vram" {
                let gs_cfg = cfg.fish_speech.clone();
                let rt = shared.inner.fish_speech.clone();
                let app_handle = app.clone();
                std::thread::spawn(move || {
                    let _ = fish_speech::stop_service(&gs_cfg, &rt);
                    let status = fish_speech::inspect_status(&gs_cfg, &rt);
                    let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
                    let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                    let _ = app_handle.emit_to("settings", "pet_vram_status", &vram_st);
                });
            }
        }
        "vramMode" => {
            let mode = value
                .as_str()
                .ok_or_else(|| "vramMode 必须是字符串".to_string())?;
            let norm = ai_voice::normalize_vram_mode(mode).to_string();
            let mut cfg = shared.chat_ai_config();
            cfg.vram_mode = norm.clone();
            if let Ok(val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("chatAiConfig", val.clone());
                let _ = shared.inner.db.save_setting("chatAiConfig", &val);
            }
            let _ = app.emit("pet_chat_config", &cfg);
            if norm == "zero_vram" {
                let gs_cfg = cfg.fish_speech.clone();
                let rt = shared.inner.fish_speech.clone();
                let app_handle = app.clone();
                std::thread::spawn(move || {
                    let _ = fish_speech::stop_service(&gs_cfg, &rt);
                    let status = fish_speech::inspect_status(&gs_cfg, &rt);
                    let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
                    let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                    let _ = app_handle.emit_to("settings", "pet_vram_status", &vram_st);
                });
            }
        }
        "builtinVoiceLang" => {
            let lang = value
                .as_str()
                .ok_or_else(|| "builtinVoiceLang 必须是字符串".to_string())?;
            let norm = ai_voice::normalize_builtin_voice_lang(lang).to_string();
            let mut cfg = shared.chat_ai_config();
            cfg.builtin_voice_lang = norm;
            if let Ok(val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("chatAiConfig", val.clone());
                let _ = shared.inner.db.save_setting("chatAiConfig", &val);
            }
            let _ = app.emit("pet_chat_config", &cfg);
        }
        "ideVoiceEnabled" => {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "ideVoiceEnabled 必须是布尔值".to_string())?;
            let mut cfg = shared.chat_ai_config();
            cfg.ide_voice_enabled = enabled;
            if let Ok(val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("chatAiConfig", val.clone());
                let _ = shared.inner.db.save_setting("chatAiConfig", &val);
            }
            let _ = app.emit("pet_chat_config", &cfg);
        }
        "companionVoiceMode" => {
            let mode = value
                .as_str()
                .ok_or_else(|| "companionVoiceMode 必须是字符串".to_string())?;
            let norm = ai_voice::normalize_companion_voice_mode(mode).to_string();
            let mut cfg = shared.chat_ai_config();
            cfg.companion_voice_mode = norm;
            if let Ok(val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("chatAiConfig", val.clone());
                let _ = shared.inner.db.save_setting("chatAiConfig", &val);
            }
            let _ = app.emit("pet_chat_config", &cfg);
        }
        "clickSfxEnabled" => {
            let enabled = value
                .as_bool()
                .ok_or_else(|| "clickSfxEnabled 必须是布尔值".to_string())?;
            let mut cfg = shared.chat_ai_config();
            cfg.click_sfx_enabled = enabled;
            if let Ok(val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("chatAiConfig", val.clone());
                let _ = shared.inner.db.save_setting("chatAiConfig", &val);
            }
            let _ = app.emit("pet_chat_config", &cfg);
        }
        "vramStatus" => {
            let gs_cfg = shared.chat_ai_config().fish_speech;
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_vram_status", &vram_st);
            });
        }
        "vramRelease" => {
            let gs_cfg = shared.chat_ai_config().fish_speech;
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let _ = fish_speech::stop_service(&gs_cfg, &rt);
                let status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
                let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_vram_status", &vram_st);
                let _ = app_handle.emit_to(
                    "settings",
                    "pet_tts_status",
                    serde_json::json!({ "ok": true, "message": "已释放 MoeMate 语音引擎显存" }),
                );
            });
        }
        "ttsPreview" => {
            let mut cfg = shared.chat_ai_config();
            let mut char_id = shared
                .character_snapshot()
                .map(|c| c.id)
                .unwrap_or_else(|| "claude".to_string());
            let mut preview_text =
                "你好呀，我是你的桌面编程搭子，随时准备陪你写代码！".to_string();

            if let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                preview_text = s.to_string();
            } else if value.is_object() {
                if let Some(t) = value.get("text").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    preview_text = t.to_string();
                }
                if let Some(c) = value.get("charId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    char_id = c.to_string();
                }
                if let Some(v) = value.get("voice").and_then(Value::as_str) {
                    cfg.char_voices.insert(char_id.clone(), v.trim().to_string());
                }
                if let Some(m) = value.get("ttsMode").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.tts_mode = m.to_string();
                }
                if let Some(bl) = value.get("builtinVoiceLang").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.builtin_voice_lang = ai_voice::normalize_builtin_voice_lang(bl).to_string();
                }
                if let Some(k) = value.get("azureSpeechKey").and_then(Value::as_str) {
                    cfg.azure_speech_key = k.trim().to_string();
                }
                if let Some(r) = value.get("azureSpeechRegion").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.azure_speech_region = r.to_string();
                }
                if let Some(fs) = value.get("fishSpeech") {
                    if let Ok(parsed_fs) = serde_json::from_value::<fish_speech::FishSpeechConfig>(fs.clone()) {
                        cfg.fish_speech = parsed_fs;
                    }
                }
            }

            let app_handle = app.clone();
            let shared_clone = (*shared).clone();
            let root = assets_root();
            let b_root = base_root();
            let c_dir = voice_cache_dir(&shared);
            let seq = shared.next_chat_seq();
            std::thread::spawn(move || {
                match ai_voice::build_preview_tts_payload(
                    &root,
                    &b_root,
                    &c_dir,
                    &cfg,
                    &char_id,
                    seq,
                    &preview_text,
                    &shared_clone.inner.fish_speech,
                ) {
                    Ok((local_line, mut payload, status_msg)) => {
                        payload.force = true;
                        if let Some(line) = local_line {
                            shared_clone.emit_bubble(line, 3400);
                        }
                        let _ = app_handle.emit_to("main", "pet_tts", &payload);
                        let _ = app_handle.emit_to(
                            "settings",
                            "pet_tts_status",
                            serde_json::json!({ "ok": true, "message": status_msg }),
                        );
                    }
                    Err(err_msg) => {
                        shared_clone.emit_bubble(err_msg.clone(), 3400);
                        let _ = app_handle.emit_to(
                            "settings",
                            "pet_tts_status",
                            serde_json::json!({ "ok": false, "message": err_msg }),
                        );
                    }
                }
            });
        }
        "fishSpeechStatus" => {
            let mut gs_cfg = shared.chat_ai_config().fish_speech;
            if let Some(dir) = value.get("installDir").and_then(Value::as_str) {
                gs_cfg.install_dir = dir.trim().to_string();
            }
            if let Some(url) = value.get("apiUrl").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                gs_cfg.api_url = url.trim().to_string();
            }
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
            });
        }
        "fishSpeechInstall" => {
            let mut gs_cfg = shared.chat_ai_config().fish_speech;
            if let Some(dir) = value.get("installDir").and_then(Value::as_str) {
                gs_cfg.install_dir = dir.trim().to_string();
            }
            if let Some(src) = value.get("downloadSource").and_then(Value::as_str) {
                gs_cfg.download_source = src.trim().to_string();
            }
            if let Some(gpu) = value.get("gpuBackend").and_then(Value::as_str) {
                gs_cfg.gpu_backend = gpu.trim().to_string();
            }
            if let Some(url) = value.get("apiUrl").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                gs_cfg.api_url = url.trim().to_string();
            }
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let init_status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &init_status);

                let _ = fish_speech::install_engine(&app_handle, &gs_cfg, &rt);
                let final_status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &final_status);
            });
        }
        "assetsStatus" => {
            let root = assets_root();
            let st = assets_mgr::inspect_assets_status(
                &root,
                &shared.assets_download_config(),
                &shared.inner.assets_rt,
            );
            let _ = app.emit_to("settings", "pet_assets_status", &st);
            let _ = app.emit_to("main", "pet_assets_status", &st);
        }
        "assetsDownloadConfig" => {
            if let Ok(parsed) = serde_json::from_value::<assets_mgr::AssetsDownloadConfig>(value.clone()) {
                shared.set_cached_setting("assetsDownloadConfig", value.clone());
                let _ = shared.inner.db.save_setting("assetsDownloadConfig", &value);
                let root = assets_root();
                let st = assets_mgr::inspect_assets_status(&root, &parsed, &shared.inner.assets_rt);
                let _ = app.emit("pet_assets_status", &st);
            }
        }
        "assetsInstall" => {
            let mut cfg = shared.assets_download_config();
            if let Some(repo) = value.get("githubRepo").and_then(Value::as_str) {
                cfg.github_repo = assets_mgr::normalize_github_repo(repo);
            }
            if let Some(tag) = value.get("releaseTag").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                cfg.release_tag = tag.trim().to_string();
            }
            if let Some(src) = value.get("downloadSource").and_then(Value::as_str) {
                cfg.download_source = src.trim().to_string();
            }
            if let Ok(cfg_val) = serde_json::to_value(&cfg) {
                shared.set_cached_setting("assetsDownloadConfig", cfg_val.clone());
                let _ = shared.inner.db.save_setting("assetsDownloadConfig", &cfg_val);
            }

            let targets: Vec<String> = value
                .get("targets")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            let root = assets_root();
            let refs_dir = ref_voices_dir(&shared);
            let rt = shared.inner.assets_rt.clone();
            let app_handle = app.clone();
            let shared_clone = (*shared).clone();
            std::thread::spawn(move || {
                let st_start = assets_mgr::inspect_assets_status(&root, &cfg, &rt);
                let _ = app_handle.emit("pet_assets_status", &st_start);

                let res = assets_mgr::install_asset_targets(
                    &root,
                    &refs_dir,
                    &cfg,
                    &targets,
                    rt.clone(),
                    |prog| {
                        let _ = app_handle.emit("pet_assets_progress", &prog);
                    },
                );

                let _ = app_handle.asset_protocol_scope().allow_directory(&root, true);
                let st_end = assets_mgr::inspect_assets_status(&root, &cfg, &rt);
                let _ = app_handle.emit("pet_assets_status", &st_end);

                if let Ok(installed_chars) = res {
                    // 清空 char_cache 中可能存在的旧角色缓存并广播新目录
                    if let Ok(mut cache) = shared_clone.inner.char_cache.lock() {
                        for cid in &installed_chars {
                            cache.remove(cid);
                        }
                    }
                    let catalog = model::scan_catalog(&root);
                    let _ = app_handle.emit("pet_catalog_updated", &catalog);

                    // 若同步了参考音频，自动填充尚未绑定参考音频的角色配置
                    if refs_dir.is_dir() {
                        let mut chat_cfg = shared_clone.chat_ai_config();
                        let mut changed = false;
                        for ch in &st_end.chars {
                            let wav = refs_dir.join(format!("{}.wav", ch.id));
                            if wav.is_file() {
                                let entry = chat_cfg
                                    .fish_speech
                                    .char_profiles
                                    .entry(ch.id.clone())
                                    .or_default();
                                if entry.ref_audio_path.trim().is_empty() {
                                    entry.ref_audio_path = wav.to_string_lossy().to_string();
                                    changed = true;
                                }
                            }
                        }
                        if changed {
                            if let Ok(val) = serde_json::to_value(&chat_cfg) {
                                shared_clone.set_cached_setting("chatAiConfig", val.clone());
                                let _ = shared_clone.inner.db.save_setting("chatAiConfig", &val);
                                let _ = app_handle.emit("pet_chat_config", &chat_cfg);
                            }
                        }
                    }
                }
            });
        }
        "assetsCancelInstall" => {
            shared
                .inner
                .assets_rt
                .cancel_install
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        "assetsDeleteChar" => {
            if let Some(cid) = value.as_str().map(str::trim) {
                let root = assets_root();
                assets_mgr::delete_char_assets(&root, cid)?;
                if let Ok(mut cache) = shared.inner.char_cache.lock() {
                    cache.remove(cid);
                }
                let st = assets_mgr::inspect_assets_status(
                    &root,
                    &shared.assets_download_config(),
                    &shared.inner.assets_rt,
                );
                let _ = app.emit("pet_assets_status", &st);
                let catalog = model::scan_catalog(&root);
                let _ = app.emit("pet_catalog_updated", &catalog);
            }
        }
        "fishSpeechCancelInstall" => {
            shared.inner.fish_speech.cancel_install.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        "fishSpeechStart" => {
            let mut gs_cfg = shared.chat_ai_config().fish_speech;
            if let Some(dir) = value.get("installDir").and_then(Value::as_str) {
                gs_cfg.install_dir = dir.trim().to_string();
            }
            if let Some(url) = value.get("apiUrl").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                gs_cfg.api_url = url.trim().to_string();
            }
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let res = fish_speech::start_service(&app_handle, &gs_cfg, &rt);
                let status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
                match res {
                    Ok(_) => {
                        let _ = app_handle.emit_to(
                            "settings",
                            "pet_tts_status",
                            serde_json::json!({ "ok": true, "message": format!("服务已启动（{}）", if status.running { "运行中" } else { "已停止" }) }),
                        );
                    }
                    Err(err) => {
                        let _ = app_handle.emit_to(
                            "settings",
                            "pet_tts_status",
                            serde_json::json!({ "ok": false, "message": err }),
                        );
                    }
                }
            });
        }
        "fishSpeechStop" => {
            let mut gs_cfg = shared.chat_ai_config().fish_speech;
            if let Some(dir) = value.get("installDir").and_then(Value::as_str) {
                gs_cfg.install_dir = dir.trim().to_string();
            }
            if let Some(url) = value.get("apiUrl").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                gs_cfg.api_url = url.trim().to_string();
            }
            let rt = shared.inner.fish_speech.clone();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let _ = fish_speech::stop_service(&gs_cfg, &rt);
                let status = fish_speech::inspect_status(&gs_cfg, &rt);
                let _ = app_handle.emit_to("settings", "pet_fish_speech_status", &status);
                let _ = app_handle.emit_to(
                    "settings",
                    "pet_tts_status",
                    serde_json::json!({ "ok": true, "message": "已停止 Fish Speech 本地服务" }),
                );
            });
        }
        "fishSpeechPickFile" => {
            let request_id = value
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("audio")
                .to_string();
            let app_handle = app.clone();
            std::thread::spawn(move || {
                let picked = fish_speech::pick_file_or_dir(&kind);
                let _ = app_handle.emit_to(
                    "settings",
                    "pet_file_picked",
                    serde_json::json!({
                        "requestId": request_id,
                        "path": picked.unwrap_or_default(),
                    }),
                );
            });
        }
        "settingsOpen" => {
            let open = value
                .get("open")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let requested_tab = value
                .get("tab")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(settings_win) = app.get_webview_window("settings") {
                if open {
                    if let Some(ch) = shared.character_snapshot() {
                        let _ = app.emit_to("settings", "pet_char_switched", &ch.to_char_state());
                    }
                    if let Some(mode) = shared
                        .cached_setting("appearance")
                        .and_then(|v| v.as_str().map(str::to_string))
                    {
                        let _ = app.emit_to("settings", "pet_appearance", mode);
                    }
                    let _ = app.emit_to("settings", "pet_chat_config", &shared.chat_ai_config());
                    let _ = app.emit_to("settings", "pet_muted", shared.snapshot().muted);
                    let _ = app.emit_to("settings", "pet_auto_start", autostart::is_windows_auto_start_enabled());
                    let _ = app.emit_to("settings", "pet_global_hotkey", hotkey::is_enabled(&shared));
                    let gs_status = fish_speech::inspect_status(
                        &shared.chat_ai_config().fish_speech,
                        &shared.inner.fish_speech,
                    );
                    let _ = app.emit_to("settings", "pet_fish_speech_status", &gs_status);
                    let assets_st = assets_mgr::inspect_assets_status(
                        &assets_root(),
                        &shared.assets_download_config(),
                        &shared.inner.assets_rt,
                    );
                    let _ = app.emit_to("settings", "pet_assets_status", &assets_st);
                    {
                        let gs_cfg = shared.chat_ai_config().fish_speech;
                        let rt = shared.inner.fish_speech.clone();
                        let app_h = app.clone();
                        std::thread::spawn(move || {
                            let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                            let _ = app_h.emit_to("settings", "pet_vram_status", &vram_st);
                        });
                    }
                    if let Some(tab) = requested_tab {
                        let _ = app.emit_to("settings", "pet_open_settings_tab", tab);
                    }
                    let _ = settings_win.center();
                    let _ = settings_win.show();
                    let _ = settings_win.set_focus();
                } else {
                    let _ = settings_win.hide();
                }
            }
        }
        "settingsReady" => {
            if let Some(ch) = shared.character_snapshot() {
                let _ = app.emit_to("settings", "pet_char_switched", &ch.to_char_state());
            }
            if let Some(mode) = shared
                .cached_setting("appearance")
                .and_then(|v| v.as_str().map(str::to_string))
            {
                let _ = app.emit_to("settings", "pet_appearance", mode);
            }
            let _ = app.emit_to("settings", "pet_chat_config", &shared.chat_ai_config());
            let _ = app.emit_to("settings", "pet_muted", shared.snapshot().muted);
            let _ = app.emit_to("settings", "pet_auto_start", autostart::is_windows_auto_start_enabled());
            let _ = app.emit_to("settings", "pet_global_hotkey", hotkey::is_enabled(&shared));
            let gs_status = fish_speech::inspect_status(
                &shared.chat_ai_config().fish_speech,
                &shared.inner.fish_speech,
            );
            let _ = app.emit_to("settings", "pet_fish_speech_status", &gs_status);
            let assets_st = assets_mgr::inspect_assets_status(
                &assets_root(),
                &shared.assets_download_config(),
                &shared.inner.assets_rt,
            );
            let _ = app.emit_to("settings", "pet_assets_status", &assets_st);
            {
                let gs_cfg = shared.chat_ai_config().fish_speech;
                let rt = shared.inner.fish_speech.clone();
                let app_h = app.clone();
                std::thread::spawn(move || {
                    let vram_st = vram::inspect_vram_status(&gs_cfg, &rt);
                    let _ = app_h.emit_to("settings", "pet_vram_status", &vram_st);
                });
            }
        }
        "chatClearHistory" => {
            shared.clear_chat_history();
            let _ = app.emit("pet_chat_cleared", true);
        }
        "uiInteractive" => {
            let interactive = value
                .as_bool()
                .ok_or_else(|| "uiInteractive 必须是布尔值".to_string())?;
            shared.set_ui_interactive(interactive);
        }
        "uiHitRects" => {
            let rects: Vec<UiHitRect> = serde_json::from_value(value.clone())
                .map_err(|_| "uiHitRects 必须是 [{x,y,w,h}] 数组".to_string())?;
            if rects.len() > 32 {
                return Err("uiHitRects 最多 32 个矩形".to_string());
            }
            if !rects.iter().all(UiHitRect::is_valid) {
                return Err("uiHitRects 含非法矩形（需 0..1 且 w/h > 0）".to_string());
            }
            shared.set_ui_hit_rects(rects);
        }
        _ => {}
    }

    let persist_to_db = !matches!(
        key.as_str(),
        "uiInteractive"
            | "uiHitRects"
            | "panelOpen"
            | "panelTab"
            | "panelReady"
            | "mainReady"
            | "ttsPreview"
            | "chatClearHistory"
            | "settingsOpen"
            | "settingsReady"
            | "fishSpeechStatus"
            | "fishSpeechInstall"
            | "fishSpeechCancelInstall"
            | "fishSpeechStart"
            | "fishSpeechStop"
            | "fishSpeechPickFile"
            | "assetsStatus"
            | "assetsInstall"
            | "assetsCancelInstall"
            | "assetsDeleteChar"
            | "assetsDownloadConfig"
            | "pomodoroControl"
            | "globalHotkey"
            | "vramStatus"
            | "vramRelease"
            | "vramMode"
            | "builtinVoiceLang"
            | "ideVoiceEnabled"
            | "companionVoiceMode"
            | "clickSfxEnabled"
    );
    if persist_to_db {
        shared.set_cached_setting(&key, value.clone());
        if let Err(err) = shared.inner.db.save_setting(&key, &value) {
            eprintln!("[deskpet] 设置落库失败 {key}: {err}");
        }
        shared.persist_pet(now);
    }
    Ok(true)
}

/// chat_send：支持 OpenAI 兼容大模型对话 + 关键词模板兜底，并异步触发三层语音播报。
#[tauri::command]
async fn chat_send(
    text: String,
    shared: State<'_, Shared>,
    app: tauri::AppHandle,
) -> Result<ChatReply, String> {
    let trimmed = text.trim().to_string();
    let intent = match_chat(&trimmed);
    let intent_binding = intent.binding.to_string();
    let character = shared
        .character_snapshot()
        .ok_or_else(|| "尚未切换角色".to_string())?;
    let now = now_ms();

    let user_msg = ai_voice::ChatLogMessage {
        id: shared.next_chat_seq(),
        role: "user".to_string(),
        text: trimmed.clone(),
        char_id: character.id.clone(),
        char_name: character.name.clone(),
        source: "user".to_string(),
        clip_id: String::new(),
        ts_ms: now,
    };
    let _ = app.emit("pet_chat_message", &user_msg);

    let cfg = shared.chat_ai_config();
    let mut reply_source = "template".to_string();
    let mut action_hint: Option<String> = None;
    let mut bubble = if intent.bubble.contains("{text}") {
        let short: String = trimmed.chars().take(40).collect();
        intent.bubble.replace("{text}", &short)
    } else {
        intent.bubble.to_string()
    };

    if cfg.api_key.trim().is_empty() {
        if let Some(scene_key) = ai_voice::detect_voice_scene(&trimmed, &intent_binding) {
            if let Some((line, _)) =
                ai_voice::load_voice_manifest_scene(&assets_root(), &character.id, scene_key)
            {
                bubble = line;
            }
        }
    }

    if !cfg.api_key.trim().is_empty() && !trimmed.is_empty() {
        shared.emit_bubble("思考中…", 12_000);
        let cfg_clone = cfg.clone();
        let char_id_clone = character.id.clone();
        let char_name_clone = character.name.clone();
        let stats_snapshot = shared.stats_snapshot();
        let ide_status = shared
            .telemetry_snapshot()
            .active_step_label
            .unwrap_or_else(|| "待机".to_string());
        let history_turns = shared.chat_history_for(&character.id);
        let user_text_clone = trimmed.clone();

        let ai_res = tauri::async_runtime::spawn_blocking(move || {
            ai_voice::call_openai_chat(
                &cfg_clone,
                &char_id_clone,
                &char_name_clone,
                &stats_snapshot,
                &ide_status,
                &history_turns,
                &user_text_clone,
            )
        })
        .await;

        match ai_res {
            Ok(Ok((ai_text, hint))) => {
                bubble = ai_text.clone();
                action_hint = hint;
                reply_source = "ai".to_string();
                shared.push_chat_turn(&character.id, trimmed.clone(), ai_text);
            }
            Ok(Err(reason)) => {
                eprintln!("[deskpet] AI 对话回退: {reason}");
                reply_source = "fallback".to_string();
                bubble = format!("AI 暂不可用（{reason}），已回退本地回复");
            }
            Err(_) => {
                eprintln!("[deskpet] AI 对话任务异常中断");
                reply_source = "fallback".to_string();
                bubble = "AI 请求异常中断，已回退本地回复".to_string();
            }
        }
    }

    let history = shared.history_snapshot();
    let reply_now = now_ms();
    let mut rng = Rng::new((reply_now as u64) ^ (trimmed.len() as u64) ^ 0x5EED_5EED);

    let cached_ja = ai_voice::get_cached_ja_voice_script(&bubble);
    let reply_emotion = ai_voice::detect_reply_emotion(&bubble, cached_ja.as_deref(), &trimmed);

    let mut clip = None;
    let concrete_action = matches!(action_hint.as_deref(), Some("eat" | "talent" | "work"));
    if concrete_action {
        if let Some(ref hint) = action_hint {
            match hint.as_str() {
                "eat" | "work" => {
                    clip = pick_for_binding(&character, hint, &history, reply_now, &mut rng);
                }
                "talent" => {
                    clip = pick_by_category(&character, &["talent"], &history, reply_now, &mut rng);
                }
                _ => {}
            }
        }
    } else if action_hint.as_deref() == Some("sleep") {
        clip = scheduler::resolve_clip_for_emotion(
            &character,
            ai_voice::EmotionBucket::Sleepy,
            &history,
            reply_now,
            &mut rng,
        );
    } else if let Some(emo) = reply_emotion {
        clip = scheduler::resolve_clip_for_emotion(
            &character,
            emo.bucket,
            &history,
            reply_now,
            &mut rng,
        );
    }

    if clip.is_none() {
        if let Some(ref hint) = action_hint {
            match hint.as_str() {
                "click" | "eat" | "sleep" | "work" | "idle" => {
                    clip = pick_for_binding(&character, hint, &history, reply_now, &mut rng);
                }
                "talent" | "emotion" | "chatter" => {
                    clip = pick_by_category(&character, &[hint.as_str()], &history, reply_now, &mut rng);
                }
                _ => {}
            }
        }
    }
    if clip.is_none() {
        let reply_intent = if reply_source == "ai" {
            match_chat(&bubble)
        } else {
            intent
        };
        if !reply_intent.binding.is_empty() {
            clip = pick_for_binding(&character, reply_intent.binding, &history, reply_now, &mut rng);
        }
        if clip.is_none() {
            clip = pick_by_category(&character, reply_intent.categories, &history, reply_now, &mut rng);
        }
    }
    if clip.is_none() {
        clip = pick_for_binding(&character, "idle", &history, reply_now, &mut rng);
    }
    let clip_id = clip.map(|item| item.id).unwrap_or_default();

    {
        let mut pet = shared
            .inner
            .pet
            .lock()
            .map_err(|_| "宠物状态锁已损坏".to_string())?;
        pet.stats.affinity = clamp_stat(pet.stats.affinity + 1.0);
    }
    shared.persist_pet(reply_now);
    shared.emit_stats();

    let ttl_ms = (3600 + (bubble.chars().count() as u64) * 150).clamp(4000, 10_000);
    shared.emit_bubble(bubble.clone(), ttl_ms);
    if !clip_id.is_empty() {
        shared.send(SchedulerEvent::ChatReply {
            clip_id: clip_id.clone(),
        });
    }

    let is_ai_reply = reply_source == "ai";
    let assistant_msg = ai_voice::ChatLogMessage {
        id: shared.next_chat_seq(),
        role: "assistant".to_string(),
        text: bubble.clone(),
        char_id: character.id.clone(),
        char_name: character.name.clone(),
        source: reply_source,
        clip_id: clip_id.clone(),
        ts_ms: reply_now,
    };
    let _ = app.emit("pet_chat_message", &assistant_msg);

    if !shared.snapshot().muted && cfg.tts_enabled {
        let root = assets_root();
        let b_root = base_root();
        let c_dir = voice_cache_dir(&shared);
        let cfg_clone = cfg.clone();
        let char_id_clone = character.id.clone();
        let user_clone = trimmed;
        let reply_clone = bubble.clone();
        let binding_hint = if concrete_action {
            action_hint.unwrap_or(intent_binding)
        } else if let Some(emo) = reply_emotion {
            emo.bucket.voice_scene_key().to_string()
        } else {
            action_hint.unwrap_or(intent_binding)
        };
        let app_handle = app.clone();
        let shared_clone = (*shared).clone();
        std::thread::spawn(move || {
            match ai_voice::build_tts_payload(
                &root,
                &b_root,
                &c_dir,
                &cfg_clone,
                &char_id_clone,
                &user_clone,
                &reply_clone,
                &binding_hint,
                is_ai_reply,
                &shared_clone.inner.fish_speech,
            ) {
                Ok((payload, fallback_hint)) => {
                    if let Some(hint) = fallback_hint {
                        shared_clone.emit_bubble(format!("{reply_clone}（{hint}）"), 5500);
                    }
                    shared_clone.mark_any_voice_played(now_ms());
                    let _ = app_handle.emit_to("main", "pet_tts", &payload);
                }
                Err(reason) => {
                    eprintln!("[deskpet] 跳过语音合成: {reason}");
                    if cfg_clone.tts_mode == "fish_speech" {
                        shared_clone.emit_bubble(format!("{reply_clone}（{reason}）"), 5500);
                    }
                }
            }
        });
    }

    Ok(ChatReply { clip_id, bubble })
}

/// hit_test_mask：返回 mask.bin 字节；>2MB 拒绝加载。
#[tauri::command]
fn hit_test_mask(clip_id: String, shared: State<'_, Shared>) -> Result<Vec<u8>, String> {
    safe_component(&clip_id)?;
    let character = shared
        .character_snapshot()
        .ok_or_else(|| "尚未切换角色".to_string())?;
    let path = assets_root()
        .join("chars")
        .join(&character.id)
        .join("clips")
        .join(format!("{clip_id}.mask.bin"));
    mask::load_mask_bytes(&path, 0)
}

/* ------------------------------------------------------------------ */
/* v1.2 遥测 command                                                   */
/* ------------------------------------------------------------------ */

/// telemetry_get：返回当前 UnifiedState 快照（不触发网络请求）。
#[tauri::command]
fn telemetry_get(shared: State<'_, Shared>) -> Result<UnifiedState, String> {
    Ok(shared.telemetry_snapshot())
}

/// telemetry_refresh：立即同步状态与额度，并广播变更。
#[tauri::command]
fn telemetry_refresh(
    shared: State<'_, Shared>,
    app: tauri::AppHandle,
) -> Result<UnifiedState, String> {
    let outcome = shared.telemetry_refresh();
    for event in outcome.scheduler_events {
        shared.send(event);
    }
    if outcome.changed {
        let _ = app.emit("telemetry", &outcome.state);
    }
    Ok(outcome.state)
}

/// telemetry_set_source：首版仅支持 "antigravity"，非法 source 返回可读错误。
#[tauri::command]
fn telemetry_set_source(source: String, shared: State<'_, Shared>) -> Result<UnifiedState, String> {
    shared.telemetry_set_source(&source)
}

/* ------------------------------------------------------------------ */
/* 后台线程                                                            */
/* ------------------------------------------------------------------ */

fn spawn_scheduler(shared: Shared, receiver: mpsc::Receiver<SchedulerEvent>) {
    std::thread::spawn(move || {
        let mut core = SchedulerCore::new(now_ms() as u64 ^ 0x9E37_79B9);
        if let Some(character) = shared.character_snapshot() {
            core.set_character(character, shared.history_snapshot());
        }

        loop {
            if shared.is_quitting() {
                break;
            }
            let now = now_ms();
            let actions = match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(SchedulerEvent::CharacterChanged(character)) => {
                    let history = shared.history_snapshot();
                    core.set_character(*character, history);
                    core.on_event(SchedulerEvent::AppReady, now)
                }
                Ok(event) => core.on_event(event, now),
                Err(mpsc::RecvTimeoutError::Timeout) => core.tick(now),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            for action in actions {
                match action {
                    Action::Play(request) => execute_play(&shared, request),
                    Action::Bubble { text, ttl_ms } => shared.emit_bubble(text, ttl_ms),
                    Action::Voice {
                        scene_key,
                        default_bubble,
                        ttl_ms,
                        force,
                    } => {
                        if let Some(ch) = shared.character_snapshot() {
                            dispatch_daily_scene_voice(
                                &shared,
                                &ch.id,
                                &scene_key,
                                default_bubble.as_deref(),
                                ttl_ms,
                                force,
                            );
                        }
                    }
                }
            }
        }
    });
}

fn execute_play(shared: &Shared, request: PlayRequest) {
    let Some(character) = shared.character_snapshot() else {
        return;
    };
    let now = now_ms();
    eprintln!(
        "[deskpet] play {} [{}] mirror={}",
        request.clip_id,
        request.priority.label(),
        request.mirror
    );
    state::install_play(shared, &character.id, &request, now);
    shared.record_play(&character.id, &request.clip_id, now);
    let safe_start = character
        .clip_map
        .get(&request.clip_id)
        .map(|meta| meta.safe_start_ms as u64)
        .unwrap_or(0);
    let _ = shared.inner.app.emit(
        "play",
        PlayPayload {
            char_id: character.id.clone(),
            clip_id: request.clip_id,
            start_at_ms: safe_start,
            mirror: request.mirror,
            audio: request.audio,
            hold_loop: request.hold_loop,
        },
    );
}

fn spawn_vram_idle_guard(shared: Shared) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        if shared.is_quitting() {
            break;
        }
        let cfg = shared.chat_ai_config();
        if ai_voice::normalize_vram_mode(&cfg.vram_mode) != "balanced" {
            continue;
        }
        let last_used = shared.inner.fish_speech.last_used();
        if last_used <= 0 {
            continue;
        }
        let timeout_ms = (cfg.vram_auto_unload_minutes.clamp(1, 60) as i64) * 60_000;
        if now_ms() - last_used < timeout_ms {
            continue;
        }
        if fish_speech::check_api_health(&cfg.fish_speech.api_url).unwrap_or(false) {
            let _ = fish_speech::stop_service(&cfg.fish_speech, &shared.inner.fish_speech);
            let status = fish_speech::inspect_status(
                &cfg.fish_speech,
                &shared.inner.fish_speech,
            );
            let _ = shared
                .inner
                .app
                .emit_to("settings", "pet_fish_speech_status", &status);
            let vram_st = vram::inspect_vram_status(&cfg.fish_speech, &shared.inner.fish_speech);
            let _ = shared
                .inner
                .app
                .emit_to("settings", "pet_vram_status", &vram_st);
        }
    });
}

fn spawn_stats_loop(shared: Shared) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(60));
        if shared.is_quitting() {
            break;
        }
        shared.tick_stats();
    });
}

/// 智能久坐提醒追踪器：
/// - 累计连续活跃（IDE 前台或 Agent 运行）达到 45 分钟后进入待提醒状态；
/// - 仅在 Agent 回归 `Idle`（或 `Offline`）时触发提醒，绝不打断生成/工具执行；
/// - 若离开工作状态连续超过 10 分钟，则清零活跃计时。
#[derive(Debug, Clone)]
struct SedentaryTracker {
    active_ms: i64,
    inactive_ms: i64,
    last_tick_ms: Option<i64>,
    pending_reminder: bool,
}

impl SedentaryTracker {
    const ACTIVE_THRESHOLD_MS: i64 = 45 * 60 * 1000;
    const BREAK_RESET_MS: i64 = 10 * 60 * 1000;

    fn new() -> Self {
        Self {
            active_ms: 0,
            inactive_ms: 0,
            last_tick_ms: None,
            pending_reminder: false,
        }
    }

    fn tick(&mut self, now_ms: i64, is_active: bool, is_agent_idle: bool) -> bool {
        let Some(prev) = self.last_tick_ms else {
            self.last_tick_ms = Some(now_ms);
            return false;
        };
        let dt = (now_ms - prev).clamp(0, 30_000);
        self.last_tick_ms = Some(now_ms);

        if is_active {
            self.active_ms = self.active_ms.saturating_add(dt);
            self.inactive_ms = 0;
            if self.active_ms >= Self::ACTIVE_THRESHOLD_MS {
                self.pending_reminder = true;
            }
        } else {
            self.inactive_ms = self.inactive_ms.saturating_add(dt);
            if self.inactive_ms >= Self::BREAK_RESET_MS {
                self.active_ms = 0;
                self.pending_reminder = false;
            }
        }

        if self.pending_reminder && is_agent_idle {
            self.active_ms = 0;
            self.inactive_ms = 0;
            self.pending_reminder = false;
            return true;
        }
        false
    }
}

fn spawn_foreground_loop(shared: Shared) {
    std::thread::spawn(move || {
        let mut last: Option<bool> = None;
        let mut sedentary = SedentaryTracker::new();
        let mut companion_throttle = companion::CompanionVoiceThrottle::new(now_ms());
        loop {
            if shared.is_quitting() {
                break;
            }
            let now = now_ms();
            let is_work = foreground::foreground_is_work();
            if last != Some(is_work) {
                last = Some(is_work);
                if let Ok(mut ui) = shared.inner.ui.lock() {
                    ui.work_focus = is_work;
                }
                shared.send(if is_work {
                    SchedulerEvent::WorkAppFocus
                } else {
                    SchedulerEvent::WorkAppBlur
                });
            }

            let tele_status = shared.telemetry_snapshot().status;
            let agent_active = matches!(
                tele_status,
                TelemetryStatus::Streaming
                    | TelemetryStatus::ToolRunning
                    | TelemetryStatus::WaitingApproval
            );
            let agent_idle = matches!(
                tele_status,
                TelemetryStatus::Idle | TelemetryStatus::Offline
            );
            let dnd = foreground::is_user_in_fullscreen_or_dnd();
            if sedentary.tick(now, is_work || agent_active, agent_idle) {
                let default_line = "已经连续专注写代码 45 分钟啦，起来活动一下、喝口水吧～";
                if !dnd {
                    if let Some(ch) = shared.character_snapshot() {
                        dispatch_daily_scene_voice(&shared, &ch.id, "tired", None, 6000, false);
                    } else {
                        shared.emit_bubble(default_line.to_string(), 6000);
                    }
                }
            }

            let cfg = shared.chat_ai_config();
            let pet_muted = shared.snapshot().muted;
            let mode = ai_voice::normalize_companion_voice_mode(&cfg.companion_voice_mode);
            if let Some(scene_key) = companion_throttle.tick(companion::CompanionTickContext {
                now_ms: now,
                mode,
                muted_or_tts_off: pet_muted || !cfg.tts_enabled,
                dnd_or_fullscreen: dnd,
                idle_input_ms: foreground::idle_input_ms(),
                agent_busy: tele_status.is_agent_active(),
                dragging: shared.drag_fast().is_dragging(),
                last_any_voice_ms: shared.last_any_voice_ms(),
                local_minute_of_day: stats::local_minute_of_day(now),
            }) {
                if let Some(ch) = shared.character_snapshot() {
                    dispatch_daily_scene_voice(&shared, &ch.id, scene_key, None, 4800, false);
                }
            }

            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn spawn_pomodoro_loop(shared: Shared) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if shared.is_quitting() {
            break;
        }
        let Some((snap, completed)) = shared.tick_pomodoro() else {
            continue;
        };
        let _ = shared.inner.app.emit("pet_pomodoro", &snap);
        if completed {
            let now = now_ms();
            if let Ok(mut pet) = shared.inner.pet.lock() {
                pet.stats.mood = clamp_stat(pet.stats.mood + 5.0);
                pet.stats.affinity = clamp_stat(pet.stats.affinity + 2.0);
            }
            shared.persist_pet(now);
            shared.emit_stats();

            if let Some(ch) = shared.character_snapshot() {
                let history = shared.history_snapshot();
                let mut rng = Rng::new((now as u64) ^ 0x90D0_2026);
                let celebrate_clip = ["工作状态-雀跃庆祝", "点击回应-开心跃动", "狂喜原地跳"]
                    .iter()
                    .find_map(|name| ch.clip_map.get(*name).filter(|c| c.playable()).cloned())
                    .or_else(|| pick_for_binding(&ch, "click", &history, now, &mut rng));
                if let Some(clip) = celebrate_clip {
                    shared.send(SchedulerEvent::PlayCommand { clip_id: clip.id });
                }
                dispatch_daily_scene_voice(
                    &shared,
                    &ch.id,
                    "pomodoro_done",
                    Some("番茄钟专注达成！辛苦啦，站起来伸个懒腰休息一下吧～"),
                    6500,
                    true,
                );
            } else {
                shared.emit_bubble(
                    "番茄钟专注达成！辛苦啦，站起来伸个懒腰休息一下吧～".to_string(),
                    6500,
                );
            }
        }
    });
}

/// 每 20s 检查一次作息；同一槽位一天只触发一次（`SCHEDULE_TICK`）。
fn spawn_schedule_loop(shared: Shared) {
    std::thread::spawn(move || {
        let mut fired: HashMap<String, i32> = HashMap::new();
        loop {
            std::thread::sleep(Duration::from_secs(20));
            if shared.is_quitting() {
                break;
            }
            let now = now_ms();
            let hhmm = local_hhmm(now);
            let date_key = local_date_key(now);
            let schedule = shared.schedule_snapshot();
            for slot in due_schedule_slots(&schedule, &hhmm, date_key, &fired) {
                fired.insert(slot.clone(), date_key);
                shared.send(SchedulerEvent::ScheduleTick { slot: slot.clone() });
                if let Some(ch) = shared.character_snapshot() {
                    dispatch_daily_scene_voice(&shared, &ch.id, &slot, None, 5000, false);
                }
            }
        }
    });
}

/* ------------------------------------------------------------------ */
/* v1.2 遥测后台线程                                                   */
/* ------------------------------------------------------------------ */

/// 200ms 心跳；TelemetryManager 内部再按 1s / 400ms / 45s 分级采样。
/// 状态无变化时不广播，避免 WebView 被无意义唤醒。
fn spawn_telemetry_loop(shared: Shared) {
    std::thread::spawn(move || {
        // 错开启动首帧渲染窗口，避免冷启动瞬间争抢锁与 CPU
        std::thread::sleep(Duration::from_millis(800));
        let mut ide_voice = companion::IdeVoiceTracker::new();
        loop {
            if shared.is_quitting() {
                break;
            }
            let now = now_ms();
            let outcome = match shared.inner.telemetry.lock() {
                Ok(mut manager) => manager.tick(now),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(telemetry::TICK_INTERVAL_MS));
                    continue;
                }
            };
            shared.update_telemetry_cache(&outcome.state);
            if let Some(scene_key) = ide_voice.evaluate(&outcome.state, &outcome.scheduler_events, now) {
                let cfg = shared.chat_ai_config();
                if cfg.ide_voice_enabled
                    && cfg.tts_enabled
                    && !shared.snapshot().muted
                    && !foreground::is_user_in_fullscreen_or_dnd()
                {
                    if let Some(ch) = shared.character_snapshot() {
                        dispatch_daily_scene_voice(&shared, &ch.id, scene_key, None, 4800, false);
                    }
                }
            }
            for event in outcome.scheduler_events {
                shared.send(event);
            }
            if outcome.changed {
                let _ = shared.inner.app.emit("telemetry", &outcome.state);
            }
            std::thread::sleep(Duration::from_millis(telemetry::TICK_INTERVAL_MS));
        }
    });
}

/* ------------------------------------------------------------------ */
/* 系统托盘                                                            */
/* ------------------------------------------------------------------ */

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主舞台", true, None::<&str>)?;
    let center = MenuItem::with_id(app, "center", "居中复位", true, None::<&str>)?;
    let resources = MenuItem::with_id(app, "resources", "下载角色动画与语音", true, None::<&str>)?;
    let mute = MenuItem::with_id(app, "mute", "静音切换", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &center, &resources, &mute, &quit])?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok();
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("MoeMate")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "resources" => {
                if let Some(settings_win) = app.get_webview_window("settings") {
                    if let Some(shared) = app.try_state::<Shared>() {
                        let _ = app.emit_to("settings", "pet_chat_config", &shared.chat_ai_config());
                        let assets_st = assets_mgr::inspect_assets_status(
                            &assets_root(),
                            &shared.assets_download_config(),
                            &shared.inner.assets_rt,
                        );
                        let _ = app.emit_to("settings", "pet_assets_status", &assets_st);
                    }
                    let _ = app.emit_to("settings", "pet_open_settings_tab", "resources");
                    let _ = settings_win.center();
                    let _ = settings_win.show();
                    let _ = settings_win.set_focus();
                }
            }
            "center" => {
                if let Some(window) = app.get_webview_window("main") {
                    let shared = app.state::<Shared>();
                    shared.bump_move_anim_seq();
                    shared.set_edge_side(None);
                    let _ = shared
                        .inner
                        .db
                        .save_setting("edgeSide", &serde_json::Value::Null);
                    shared.set_cached_setting("edgeSide", serde_json::Value::Null);
                    let _ = window.center();
                    if let Ok(position) = window.outer_position() {
                        if let Ok(mut pet) = shared.inner.pet.lock() {
                            pet.pos = PetPos {
                                x: position.x as f64,
                                y: position.y as f64,
                            };
                        }
                        shared.persist_pet(now_ms());
                    }
                    window_layout::emit_edge_dock_to_main(&shared, &window);
                    window_layout::reposition_panel_if_open(app, &shared);
                }
            }
            "mute" => {
                let shared = app.state::<Shared>();
                let muted = if let Ok(mut pet) = shared.inner.pet.lock() {
                    pet.muted = !pet.muted;
                    pet.muted
                } else {
                    false
                };
                shared.persist_pet(now_ms());
                let _ = app.emit("pet_muted", muted);
                shared.emit_bubble(
                    if muted {
                        "已静音"
                    } else {
                        "声音回来啦"
                    },
                    2500,
                );
            }
            "quit" => {
                if let Some(shared) = app.try_state::<Shared>() {
                    shared.mark_quitting();
                }
                app.exit(0);
            }
            _ => {}
        });

    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

mod autostart {
    use std::path::Path;

    pub fn format_autostart_command(exe_path: &Path) -> String {
        format!("\"{}\"", exe_path.display())
    }

    #[cfg(windows)]
    mod win_reg {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::{null, null_mut};

        const HKEY_CURRENT_USER: isize = -2147483647;
        const KEY_READ: u32 = 0x20019;
        const KEY_WRITE: u32 = 0x20006;
        const REG_SZ: u32 = 1;
        const ERROR_SUCCESS: i32 = 0;
        const ERROR_FILE_NOT_FOUND: i32 = 2;

        const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
        const VALUE_NAME: &str = "MoeMate";

        #[link(name = "advapi32")]
        extern "system" {
            fn RegOpenKeyExW(
                hkey: isize,
                lp_sub_key: *const u16,
                ul_options: u32,
                sam_desired: u32,
                phk_result: *mut isize,
            ) -> i32;
            fn RegCreateKeyExW(
                hkey: isize,
                lp_sub_key: *const u16,
                reserved: u32,
                lp_class: *const u16,
                dw_options: u32,
                sam_desired: u32,
                lp_security_attributes: *const core::ffi::c_void,
                phk_result: *mut isize,
                lpdw_disposition: *mut u32,
            ) -> i32;
            fn RegQueryValueExW(
                hkey: isize,
                lp_value_name: *const u16,
                lp_reserved: *const u32,
                lp_type: *mut u32,
                lp_data: *mut u8,
                lpcb_data: *mut u32,
            ) -> i32;
            fn RegSetValueExW(
                hkey: isize,
                lp_value_name: *const u16,
                reserved: u32,
                dw_type: u32,
                lp_data: *const u8,
                cb_data: u32,
            ) -> i32;
            fn RegDeleteValueW(hkey: isize, lp_value_name: *const u16) -> i32;
            fn RegCloseKey(hkey: isize) -> i32;
        }

        fn to_wide(s: &str) -> Vec<u16> {
            OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
        }

        pub fn is_enabled() -> bool {
            unsafe {
                let subkey = to_wide(RUN_SUBKEY);
                let name = to_wide(VALUE_NAME);
                let mut hkey: isize = 0;
                if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey)
                    != ERROR_SUCCESS
                {
                    return false;
                }
                let status = RegQueryValueExW(
                    hkey,
                    name.as_ptr(),
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                );
                RegCloseKey(hkey);
                status == ERROR_SUCCESS
            }
        }

        pub fn set_enabled(enabled: bool, command_line: Option<&str>) -> Result<(), String> {
            unsafe {
                let subkey = to_wide(RUN_SUBKEY);
                let name = to_wide(VALUE_NAME);
                let mut hkey: isize = 0;
                if enabled {
                    let cmd = command_line.ok_or_else(|| "缺少可执行文件路径".to_string())?;
                    let open_rc = RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        subkey.as_ptr(),
                        0,
                        null(),
                        0,
                        KEY_WRITE,
                        null(),
                        &mut hkey,
                        null_mut(),
                    );
                    if open_rc != ERROR_SUCCESS {
                        return Err(format!("打开开机自启注册表项失败 (code={open_rc})"));
                    }
                    let wide_val = to_wide(cmd);
                    let byte_len = (wide_val.len() * std::mem::size_of::<u16>()) as u32;
                    let set_rc = RegSetValueExW(
                        hkey,
                        name.as_ptr(),
                        0,
                        REG_SZ,
                        wide_val.as_ptr() as *const u8,
                        byte_len,
                    );
                    RegCloseKey(hkey);
                    if set_rc != ERROR_SUCCESS {
                        return Err(format!("写入开机自启注册表失败 (code={set_rc})"));
                    }
                    Ok(())
                } else {
                    let open_rc =
                        RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_WRITE, &mut hkey);
                    if open_rc == ERROR_FILE_NOT_FOUND {
                        return Ok(());
                    }
                    if open_rc != ERROR_SUCCESS {
                        return Err(format!("打开开机自启注册表项失败 (code={open_rc})"));
                    }
                    let del_rc = RegDeleteValueW(hkey, name.as_ptr());
                    RegCloseKey(hkey);
                    if del_rc != ERROR_SUCCESS && del_rc != ERROR_FILE_NOT_FOUND {
                        return Err(format!("删除开机自启注册表项失败 (code={del_rc})"));
                    }
                    Ok(())
                }
            }
        }
    }

    pub fn is_windows_auto_start_enabled() -> bool {
        #[cfg(windows)]
        {
            win_reg::is_enabled()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn set_windows_auto_start(enabled: bool) -> Result<(), String> {
        #[cfg(windows)]
        {
            if enabled {
                let exe = std::env::current_exe()
                    .map_err(|err| format!("获取当前程序路径失败: {err}"))?;
                let cmd = format_autostart_command(&exe);
                win_reg::set_enabled(true, Some(&cmd))
            } else {
                win_reg::set_enabled(false, None)
            }
        }
        #[cfg(not(windows))]
        {
            let _ = enabled;
            Err("仅 Windows 支持注册表开机自启".to_string())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        #[test]
        fn format_autostart_command_quotes_spaces() {
            let p = PathBuf::from(r"C:\Program Files\Deskpet\deskpet.exe");
            assert_eq!(
                format_autostart_command(&p),
                "\"C:\\Program Files\\Deskpet\\deskpet.exe\""
            );
        }

        #[test]
        fn sedentary_tracker_waits_for_agent_idle_and_resets_on_break() {
            use crate::SedentaryTracker;
            let mut tracker = SedentaryTracker::new();
            let mut now = 1_000_000i64;
            assert!(!tracker.tick(now, true, true));

            // 推进 45 分钟（按 20s 步长），期间 Agent 一直忙碌
            for _ in 0..135 {
                now += 20_000;
                assert!(!tracker.tick(now, true, false));
            }
            assert!(tracker.pending_reminder);

            // Agent 回归 Idle 的瞬间触发提醒并清零
            now += 2_000;
            assert!(tracker.tick(now, true, true));
            assert!(!tracker.pending_reminder);
            assert_eq!(tracker.active_ms, 0);

            // 连续离开超过 10 分钟后，活跃计时归零
            now += 20_000;
            assert!(!tracker.tick(now, true, true));
            for _ in 0..31 {
                now += 20_000;
                assert!(!tracker.tick(now, false, true));
            }
            assert_eq!(tracker.active_ms, 0);
        }
    }
}

mod hotkey {
    use std::time::Duration;
    use tauri::AppHandle;

    use crate::state::Shared;

    pub fn is_enabled(shared: &Shared) -> bool {
        shared
            .cached_setting("globalHotkey")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    pub fn spawn(shared: Shared, app: AppHandle) {
        #[cfg(windows)]
        {
            use std::ffi::c_void;
            use std::ptr::null_mut;

            const HOTKEY_ID: i32 = 0x4450;
            const MOD_ALT: u32 = 0x0001;
            const MOD_NOREPEAT: u32 = 0x4000;
            const VK_SPACE: u32 = 0x20;
            const WM_HOTKEY: u32 = 0x0312;
            const PM_REMOVE: u32 = 0x0001;

            #[repr(C)]
            #[derive(Default)]
            struct Point {
                x: i32,
                y: i32,
            }

            #[repr(C)]
            #[derive(Default)]
            struct Msg {
                hwnd: *mut c_void,
                message: u32,
                w_param: usize,
                l_param: isize,
                time: u32,
                pt: Point,
            }

            #[link(name = "user32")]
            extern "system" {
                fn RegisterHotKey(h_wnd: *mut c_void, id: i32, fs_modifiers: u32, vk: u32) -> i32;
                fn UnregisterHotKey(h_wnd: *mut c_void, id: i32) -> i32;
                fn PeekMessageW(
                    lp_msg: *mut Msg,
                    h_wnd: *mut c_void,
                    w_msg_filter_min: u32,
                    w_msg_filter_max: u32,
                    w_remove_msg: u32,
                ) -> i32;
            }

            std::thread::spawn(move || {
                let mut registered = false;
                let mut msg = Msg::default();
                loop {
                    if shared.is_quitting() {
                        if registered {
                            unsafe {
                                UnregisterHotKey(null_mut(), HOTKEY_ID);
                            }
                        }
                        break;
                    }
                    let want = is_enabled(&shared);
                    if want && !registered {
                        let ok = unsafe {
                            RegisterHotKey(null_mut(), HOTKEY_ID, MOD_ALT | MOD_NOREPEAT, VK_SPACE)
                        };
                        registered = ok != 0;
                    } else if !want && registered {
                        unsafe {
                            UnregisterHotKey(null_mut(), HOTKEY_ID);
                        }
                        registered = false;
                    }

                    if registered {
                        unsafe {
                            while PeekMessageW(&mut msg, null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE)
                                != 0
                            {
                                if msg.message == WM_HOTKEY && msg.w_param == HOTKEY_ID as usize {
                                    crate::window_layout::toggle_quick_chat_hotkey(&app, &shared);
                                }
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            });
        }
        #[cfg(not(windows))]
        {
            let _ = (shared, app);
        }
    }
}

/* ------------------------------------------------------------------ */
/* 启动                                                                */
/* ------------------------------------------------------------------ */

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();

            let root = assets_root();
            if let Err(err) = handle.asset_protocol_scope().allow_directory(&root, true) {
                eprintln!("[deskpet] 无法放行 asset 目录 {}: {err}", root.display());
            }

            /* ---------- 打开 DB + 离线结算 ---------- */
            let db = Db::open_default()
                .map_err(|err| setup_error(format!("初始化 SQLite 失败: {err}")))?;
            eprintln!("[deskpet] SQLite: {}", db.path().display());
            let stored = db
                .load_state()
                .map_err(|err| setup_error(format!("读取持久化状态失败: {err}")))?;
            let now = now_ms();

            let catalog = load_catalog_fast(&root);
            let preferred = stored
                .char_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| "claude".to_string());
            let character = model::load_character(&root, &preferred).ok().or_else(|| {
                catalog
                    .chars
                    .iter()
                    .find_map(|item| model::load_character(&root, &item.id).ok())
            });
            let schedule = character
                .as_ref()
                .map(|item| item.schedule.clone())
                .unwrap_or_default();

            let mut pet = stored.to_pet_state();
            let sleep_from = parse_hhmm(&schedule.sleep).unwrap_or(23 * 60 + 30);
            let sleep_to = parse_hhmm(&schedule.wake).unwrap_or(8 * 60 + 30);
            let sleep_fraction =
                sleep_fraction_between(stored.updated_at_ms, now, sleep_from, sleep_to);
            settle_stats(&mut pet.stats, now - stored.updated_at_ms, sleep_fraction);

            let char_id = character
                .as_ref()
                .map(|item| item.id.clone())
                .unwrap_or(preferred);
            if let Err(err) = db.save_pet(Some(&char_id), &pet, now) {
                eprintln!("[deskpet] 写入离线结算结果失败: {err}");
            }
            let settings = db.load_settings().unwrap_or_default();
            let startup_cfg: ai_voice::ChatAiConfig = settings
                .get("chatAiConfig")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            pet.muted = !(startup_cfg.auto_play_voice && startup_cfg.tts_enabled);
            let history = db
                .load_history(&char_id, now - HISTORY_LOOKBACK_MS)
                .unwrap_or_default();
            if let Err(err) = db.prune_history(now - HISTORY_LOOKBACK_MS) {
                eprintln!("[deskpet] 清理历史失败: {err}");
            }

            /* ---------- 共享状态 ---------- */
            let (tx, rx) = mpsc::channel();
            let telemetry = TelemetryManager::new(Box::new(AntigravityAdapter::new()));
            let shared = Shared::new(
                handle.clone(),
                db,
                pet,
                character,
                settings,
                history,
                telemetry,
                tx,
            );
            app.manage(shared.clone());

            /* ---------- 托盘 / 穿透 / 线程 ---------- */
            setup_tray(app)?;

            if let Some(window) = app.get_webview_window("main") {
                let restored = shared.snapshot();
                let scale = restored.scale.clamp(0.25, 2.0);
                let _ = window.set_size(LogicalSize::new(
                    BASE_WIDTH * scale,
                    BASE_HEIGHT * scale + TOP_CROWN_HEIGHT,
                ));
                let _ = window.set_always_on_top(restored.pinned);
                window_layout::restore_and_clamp_startup_position(&shared, &window);
                let _ = window.set_ignore_cursor_events(true);
                passthrough::install_drag_subclass(&window, &shared);

                let main_handle = handle.clone();
                let main_shared = shared.clone();
                window.on_window_event(move |event| {
                    if matches!(event, WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed) {
                        main_shared.mark_quitting();
                        if let Some(panel) = main_handle.get_webview_window("panel") {
                            let _ = panel.close();
                        }
                        if let Some(settings_win) = main_handle.get_webview_window("settings") {
                            let _ = settings_win.close();
                        }
                    }
                });

                passthrough::spawn(shared.clone(), window);
            }
            if let Some(settings_win) = app.get_webview_window("settings") {
                let pinned = shared.snapshot().pinned;
                let _ = settings_win.set_always_on_top(pinned);
                let sw_clone = settings_win.clone();
                settings_win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = sw_clone.hide();
                    }
                });
            }
            if let Some(panel) = app.get_webview_window("panel") {
                let pinned = shared.snapshot().pinned;
                let _ = panel.set_always_on_top(pinned);
                let panel_handle = handle.clone();
                let panel_shared = shared.clone();
                panel.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        window_layout::hide_panel(&panel_handle, &panel_shared);
                    }
                });
            }
            spawn_scheduler(shared.clone(), rx);
            spawn_stats_loop(shared.clone());
            spawn_foreground_loop(shared.clone());
            spawn_schedule_loop(shared.clone());
            spawn_telemetry_loop(shared.clone());
            spawn_pomodoro_loop(shared.clone());
            spawn_vram_idle_guard(shared.clone());
            hotkey::spawn(shared.clone(), handle.clone());
            http_hook::spawn_http_hook_server(shared.clone());
            // v1.3：启动后后台静默预热全部 8 位角色的元数据缓存，确保切角 <1ms 响应。
            // 延后 2.5s 启动，把冷启动磁盘 I/O 与 CPU 全额让给当前角色首帧渲染。
            {
                let warm_shared = shared.clone();
                let warm_root = root.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2500));
                    if warm_shared.is_quitting() {
                        return;
                    }
                    let catalog = load_catalog_fast(&warm_root);
                    for entry in catalog.chars {
                        if warm_shared.is_quitting() {
                            break;
                        }
                        let _ = warm_shared.load_character_cached(&warm_root, &entry.id);
                    }
                });
            }
            shared.refresh_thresholds();
            shared.emit_stats();

            /* ---------- frame_hint：Rust 计时器之外的第二重收段驱动 ---------- */
            let frame_handle = handle.clone();
            handle.listen("frame_hint", move |event| {
                let Ok(hint) = serde_json::from_str::<model::FrameHint>(event.payload()) else {
                    return;
                };
                let Some(shared) = frame_handle.try_state::<Shared>() else {
                    return;
                };
                let frames = shared.inner.current.lock().ok().and_then(|current| {
                    current
                        .as_ref()
                        .filter(|item| item.clip_id == hint.clip_id)
                        .map(|item| item.frames)
                });
                let Some(frames) = frames else {
                    return;
                };
                if let Ok(mut slot) = shared.inner.hint.lock() {
                    *slot = Some(hint.clone());
                }
                if frames > 0 && hint.frame >= frames.saturating_sub(1) {
                    shared.send(SchedulerEvent::ClipEnded {
                        clip_id: Some(hint.clip_id),
                    });
                }
            });

            /* ---------- 前端主动 APP_READY（可选） ---------- */
            let ready_handle = handle.clone();
            handle.listen("app_ready", move |_| {
                if let Some(shared) = ready_handle.try_state::<Shared>() {
                    shared.send(SchedulerEvent::AppReady);
                }
            });

            /* ---------- 延迟补发 APP_READY，避免前端监听注册晚于 setup ---------- */
            let delayed = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(900));
                if !delayed.is_quitting() {
                    delayed.send(SchedulerEvent::AppReady);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            catalog_get,
            char_switch,
            pet_state_get,
            pet_interact,
            action_play,
            settings_set,
            chat_send,
            hit_test_mask,
            telemetry_get,
            telemetry_refresh,
            telemetry_set_source
        ])
        .run(tauri::generate_context!())
        .expect("MoeMate 启动失败");
}
