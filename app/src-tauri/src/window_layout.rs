//! 窗口几何、独立工作台防遮挡定位与屏幕边缘吸附（贴边探头）。
//!
//! 设计原则：
//! - 使用 Win32 `MonitorFromWindow` / `GetMonitorInfoW` 获取真实多屏工作区 `rcWork`，
//!   自动避开 Windows 任务栏与多屏负坐标偏移；
//! - 基于 `ClipMeta.body_box`（底对齐 16:9 画布）与头顶灵动岛安全带计算物理占区，
//!   保证独立工作台窗口在 35% / 50% / 75% 全档缩放下 0% 遮挡角色或灵动岛；
//! - 边缘吸附时采用约 180ms cubic ease-out 平滑靠边，并向前端广播可见水平区间，
//!   使头顶灵动岛、气泡与右键菜单自动缩进至屏幕可见范围内。

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

use crate::model::PetPos;
use crate::passthrough;
use crate::scheduler::SchedulerEvent;
use crate::state::{EdgeSide, Shared};

pub const PANEL_LOGICAL_W: f64 = 376.0;
pub const PANEL_LOGICAL_H: f64 = 450.0;

const ISLAND_LOGICAL_W: f64 = 290.0;
const EDGE_SNAP_THRESHOLD_CSS: f64 = 44.0;
const TUCK_FRACTION: f64 = 0.44;
const UNDOCK_MARGIN_CSS: f64 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyRatios {
    pub left_u: f64,
    pub right_u: f64,
    pub top_v: f64,
    pub bottom_v: f64,
}

impl Default for BodyRatios {
    fn default() -> Self {
        Self {
            left_u: 0.30,
            right_u: 0.70,
            top_v: 0.12,
            bottom_v: 0.90,
        }
    }
}

impl BodyRatios {
    pub fn width_u(self) -> f64 {
        (self.right_u - self.left_u).max(0.16)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeDockEvent {
    pub enabled: bool,
    pub side: Option<EdgeSide>,
    pub visible_left_px: f64,
    pub visible_right_px: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelStateEvent {
    pub open: bool,
    pub tab: String,
}

#[cfg(windows)]
fn monitor_work_area_from_hmonitor(hmonitor: windows_sys::Win32::Graphics::Gdi::HMONITOR) -> Option<WorkArea> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};

    if hmonitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        rcWork: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        dwFlags: 0,
    };
    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFO) };
    if ok == 0 {
        return None;
    }
    let r = info.rcWork;
    if r.right <= r.left || r.bottom <= r.top {
        return None;
    }
    Some(WorkArea {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    })
}

#[cfg(windows)]
pub fn monitor_work_area_for_window(window: &WebviewWindow) -> WorkArea {
    use windows_sys::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};

    if let Ok(hwnd) = window.hwnd() {
        let hmon = unsafe { MonitorFromWindow(hwnd.0 as _, MONITOR_DEFAULTTONEAREST) };
        if let Some(area) = monitor_work_area_from_hmonitor(hmon) {
            return area;
        }
    }
    WorkArea {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    }
}

#[cfg(not(windows))]
pub fn monitor_work_area_for_window(_window: &WebviewWindow) -> WorkArea {
    WorkArea {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    }
}

#[cfg(windows)]
pub fn monitor_work_area_for_point(x: i32, y: i32) -> WorkArea {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};

    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    monitor_work_area_from_hmonitor(hmon).unwrap_or(WorkArea {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    })
}

#[cfg(not(windows))]
pub fn monitor_work_area_for_point(_x: i32, _y: i32) -> WorkArea {
    WorkArea {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    }
}

/// 读取当前角色（优先当前播放 clip，回退首个有效 idle clip）的归一化躯干矩形。
pub fn character_body_ratios(shared: &Shared) -> BodyRatios {
    let fallback = BodyRatios::default();
    let Some(character) = shared.character_snapshot() else {
        return fallback;
    };
    let current_id = shared.current_clip_id();
    let clip = current_id
        .as_deref()
        .and_then(|id| character.clip_map.get(id))
        .or_else(|| {
            character
                .bindings
                .idle
                .iter()
                .find_map(|id| character.clip_map.get(id).filter(|c| c.playable()))
        })
        .or_else(|| character.clips.iter().find(|c| c.playable()));

    let Some(clip) = clip else {
        return fallback;
    };

    let base_w = if clip.size[0] > 0 {
        clip.size[0] as f64
    } else {
        1280.0
    };
    let base_h = if clip.size[1] > 0 {
        clip.size[1] as f64
    } else {
        720.0
    };
    let [bx, by, bw, bh] = clip.body_box;
    if !bx.is_finite() || !by.is_finite() || !bw.is_finite() || !bh.is_finite() || bw < 40.0 || bh < 40.0 {
        return fallback;
    }

    let left_u = (bx / base_w).clamp(0.15, 0.72);
    let right_u = ((bx + bw) / base_w).clamp((left_u + 0.16).min(0.85), 0.85);
    let top_v = (by / base_h).clamp(0.04, 0.65);
    let bottom_v = ((by + bh) / base_h).clamp((top_v + 0.20).min(0.98), 0.98);
    BodyRatios {
        left_u,
        right_u,
        top_v,
        bottom_v,
    }
}

#[inline]
fn stage_geometry(win_w: i32, win_h: i32) -> (f64, f64, f64, f64) {
    let wf = win_w.max(1) as f64;
    let hf = win_h.max(1) as f64;
    let scale = (wf / 1280.0).min(hf / 720.0);
    let draw_w = 1280.0 * scale;
    let draw_h = 720.0 * scale;
    let draw_x = (wf - draw_w) * 0.5;
    let draw_y = hf - draw_h;
    (draw_x, draw_y, draw_w, draw_h)
}

/// 按与 `passthrough::map_mask_uv` 一致的底对齐 16:9 舞台坐标计算角色在物理屏幕上的可见躯干矩形。
pub fn compute_char_screen_rect(
    win_x: i32,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    ratios: BodyRatios,
) -> ScreenRect {
    let (draw_x, draw_y, draw_w, draw_h) = stage_geometry(win_w, win_h);
    let left = win_x + (draw_x + ratios.left_u * draw_w).round() as i32;
    let right = win_x + (draw_x + ratios.right_u * draw_w).round() as i32;
    let top = win_y + (draw_y + ratios.top_v * draw_h).round() as i32;
    let bottom = win_y + (draw_y + ratios.bottom_v * draw_h).round() as i32;
    ScreenRect {
        left,
        top,
        right: right.max(left + 1),
        bottom: bottom.max(top + 1),
    }
}

/// 计算头顶灵动岛在屏幕上的水平占用范围 `[left, right]`。
pub fn compute_island_screen_span(
    win_x: i32,
    win_w: i32,
    dpi: f64,
    edge_side: Option<EdgeSide>,
    work: WorkArea,
) -> (i32, i32) {
    let dpi = dpi.clamp(0.5, 4.0);
    let pad = (9.0 * dpi).round() as i32;
    let full_w = (ISLAND_LOGICAL_W * dpi).round() as i32;

    match edge_side {
        Some(EdgeSide::Left) => {
            let vis_left = work.left.max(win_x);
            let vis_right = work.right.min(win_x + win_w);
            let avail = (vis_right - vis_left - pad * 2).max((176.0 * dpi).round() as i32);
            let w = full_w.min(avail);
            let left = vis_left + pad;
            (left, left + w)
        }
        Some(EdgeSide::Right) => {
            let vis_left = work.left.max(win_x);
            let vis_right = work.right.min(win_x + win_w);
            let avail = (vis_right - vis_left - pad * 2).max((176.0 * dpi).round() as i32);
            let w = full_w.min(avail);
            let right = vis_right - pad;
            (right - w, right)
        }
        None => {
            let w = full_w.min((win_w - pad * 2).max(120));
            let left = win_x + (win_w - w) / 2;
            (left, left + w)
        }
    }
}

/// 纯函数：计算独立工作台窗口位置，确保永不遮挡角色与头顶灵动岛，并完整留在屏幕工作区内。
#[allow(clippy::too_many_arguments)]
pub fn compute_panel_position(
    win_x: i32,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    panel_w: i32,
    panel_h: i32,
    dpi: f64,
    edge_side: Option<EdgeSide>,
    work: WorkArea,
    ratios: BodyRatios,
) -> (i32, i32) {
    let dpi = dpi.clamp(0.5, 4.0);
    let gap = (4.0 * dpi).round() as i32;
    let margin = (4.0 * dpi).round() as i32;

    let char_rect = compute_char_screen_rect(win_x, win_y, win_w, win_h, ratios);
    let (island_left, island_right) =
        compute_island_screen_span(win_x, win_w, dpi, edge_side, work);

    let avoid_left = char_rect.left.min(island_left);
    let avoid_right = char_rect.right.max(island_right);

    let min_x = work.left + margin;
    let max_x = (work.right - panel_w - margin).max(min_x);

    let right_x = avoid_right.max(work.left) + gap;
    let left_x = avoid_left.min(work.right) - gap - panel_w;
    let right_fits = right_x <= max_x;
    let left_fits = left_x >= min_x;

    let chosen_x = match edge_side {
        Some(EdgeSide::Left) => right_x,
        Some(EdgeSide::Right) => left_x,
        None => {
            if right_fits && left_fits {
                right_x
            } else if right_fits {
                right_x
            } else if left_fits {
                left_x
            } else {
                // 极端窄屏：挑剩余净空间更大的一侧
                let space_right = (work.right - margin) - (avoid_right + gap);
                let space_left = (avoid_left - gap) - (work.left + margin);
                if space_right >= space_left {
                    right_x
                } else {
                    left_x
                }
            }
        }
    };

    let x = chosen_x.clamp(min_x, max_x);

    let min_y = work.top + margin;
    let max_y = (work.bottom - panel_h - margin).max(min_y);
    let desired_y = win_y + (6.0 * dpi).round() as i32;
    let y = desired_y.clamp(min_y, max_y);

    (x, y)
}

fn clamp_vertical_position(win_y: i32, win_h: i32, dpi: f64, work: WorkArea) -> i32 {
    let min_y = work.top + (2.0 * dpi).round() as i32;
    let bottom_pad = (14.0 * dpi).round() as i32;
    let max_y = (work.bottom - win_h + bottom_pad).max(min_y);
    win_y.clamp(min_y, max_y)
}

/// 计算非吸附自由拖放下的安全在屏位置（防止角色被拖丢出桌面边界）。
pub fn clamp_free_window_position(
    win_x: i32,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    dpi: f64,
    work: WorkArea,
    ratios: BodyRatios,
) -> (i32, i32) {
    let (draw_x, _, draw_w, _) = stage_geometry(win_w, win_h);
    let pad = (8.0 * dpi).round() as i32;
    let body_left_offset = (draw_x + ratios.left_u * draw_w).round() as i32;
    let body_right_offset = (draw_x + ratios.right_u * draw_w).round() as i32;

    let min_x = work.left + pad - body_left_offset;
    let max_x = (work.right - pad - body_right_offset).max(min_x);

    (
        win_x.clamp(min_x, max_x),
        clamp_vertical_position(win_y, win_h, dpi, work),
    )
}

/// 计算某侧边缘吸附后的目标窗口位置（约 44% 角色躯干藏入边缘，56% 探头露出桌面）。
pub fn compute_edge_dock_target(
    side: EdgeSide,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    dpi: f64,
    work: WorkArea,
    ratios: BodyRatios,
) -> (i32, i32) {
    let (draw_x, _, draw_w, _) = stage_geometry(win_w, win_h);
    let body_w_u = ratios.width_u();
    let target_x = match side {
        EdgeSide::Left => {
            work.left - (draw_x + (ratios.left_u + TUCK_FRACTION * body_w_u) * draw_w).round() as i32
        }
        EdgeSide::Right => {
            work.right - (draw_x + (ratios.right_u - TUCK_FRACTION * body_w_u) * draw_w).round() as i32
        }
    };
    let target_y = clamp_vertical_position(win_y, win_h, dpi, work);
    (target_x, target_y)
}

/// 判断拖放松手时是否满足左右屏幕边缘吸附条件。
#[allow(clippy::too_many_arguments)]
pub fn evaluate_edge_snap(
    win_x: i32,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    dpi: f64,
    work: WorkArea,
    ratios: BodyRatios,
) -> Option<(EdgeSide, i32, i32)> {
    let dpi = dpi.clamp(0.5, 4.0);
    let snap_px = (EDGE_SNAP_THRESHOLD_CSS * dpi).round() as i32;
    let char_rect = compute_char_screen_rect(win_x, win_y, win_w, win_h, ratios);

    let near_left = char_rect.left - work.left <= snap_px;
    let near_right = work.right - char_rect.right <= snap_px;

    let side = match (near_left, near_right) {
        (false, false) => return None,
        (true, false) => EdgeSide::Left,
        (false, true) => EdgeSide::Right,
        (true, true) => {
            if (char_rect.left - work.left).abs() <= (work.right - char_rect.right).abs() {
                EdgeSide::Left
            } else {
                EdgeSide::Right
            }
        }
    };

    let (tx, ty) = compute_edge_dock_target(side, win_y, win_w, win_h, dpi, work, ratios);
    Some((side, tx, ty))
}

/// 单击贴边探头的角色后，计算平滑弹出到桌面可见区内的目标位置。
pub fn compute_undock_position(
    side: EdgeSide,
    win_y: i32,
    win_w: i32,
    win_h: i32,
    dpi: f64,
    work: WorkArea,
    ratios: BodyRatios,
) -> (i32, i32) {
    let dpi = dpi.clamp(0.5, 4.0);
    let margin = (UNDOCK_MARGIN_CSS * dpi).round() as i32;
    let (draw_x, _, draw_w, _) = stage_geometry(win_w, win_h);

    let island_w = ((ISLAND_LOGICAL_W * dpi).round() as i32).min(win_w);
    let island_left_offset = (win_w - island_w) / 2;
    let island_right_offset = island_left_offset + island_w;

    let body_left_offset = (draw_x + ratios.left_u * draw_w).round() as i32;
    let body_right_offset = (draw_x + ratios.right_u * draw_w).round() as i32;

    let effective_left = body_left_offset.min(island_left_offset);
    let effective_right = body_right_offset.max(island_right_offset);

    let target_x = match side {
        EdgeSide::Left => work.left + margin - effective_left,
        EdgeSide::Right => work.right - margin - effective_right,
    };
    let target_y = clamp_vertical_position(win_y, win_h, dpi, work);
    (target_x, target_y)
}

/// 计算当前主窗处于工作区内的水平可见区间（CSS 像素），供前端自适应安置灵动岛/气泡/菜单。
pub fn compute_edge_dock_event(
    enabled: bool,
    side: Option<EdgeSide>,
    win_x: i32,
    win_w: i32,
    dpi: f64,
    work: WorkArea,
) -> EdgeDockEvent {
    let dpi = dpi.clamp(0.5, 4.0);
    let win_w_css = (win_w.max(1) as f64) / dpi;
    let vis_left = ((work.left - win_x).max(0) as f64 / dpi).clamp(0.0, win_w_css);
    let vis_right = ((work.right - win_x).min(win_w).max(0) as f64 / dpi).clamp(vis_left, win_w_css);

    EdgeDockEvent {
        enabled,
        side,
        visible_left_px: (vis_left * 10.0).round() / 10.0,
        visible_right_px: (vis_right * 10.0).round() / 10.0,
    }
}

pub fn emit_edge_dock_for_rect(
    shared: &Shared,
    win_x: i32,
    win_w: i32,
    dpi: f64,
    work: WorkArea,
    target_window: Option<&str>,
) {
    let enabled = shared.edge_docking_enabled();
    let side = shared.edge_side();
    let payload = compute_edge_dock_event(enabled, side, win_x, win_w, dpi, work);
    if let Some(label) = target_window {
        let _ = shared.inner.app.emit_to(label, "pet_edge_dock", &payload);
    } else {
        let _ = shared.inner.app.emit("pet_edge_dock", &payload);
    }
}

pub fn emit_edge_dock_to_main(shared: &Shared, window: &WebviewWindow) {
    let Ok(pos) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let dpi = window.scale_factor().unwrap_or(1.0);
    let work = monitor_work_area_for_window(window);
    emit_edge_dock_for_rect(shared, pos.x, size.width as i32, dpi, work, Some("main"));
}

pub fn animate_window_move(
    shared: Shared,
    window: WebviewWindow,
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    duration_ms: u64,
) {
    if from_x == to_x && from_y == to_y {
        return;
    }
    let seq = shared.bump_move_anim_seq();
    std::thread::spawn(move || {
        let steps = 14u32;
        let step_ms = (duration_ms / steps as u64).max(8);
        for i in 1..=steps {
            if shared.is_quitting()
                || shared.current_move_anim_seq() != seq
                || shared.drag_fast().is_dragging()
            {
                return;
            }
            let t = i as f64 / steps as f64;
            let eased = 1.0 - (1.0 - t).powi(3);
            let cur_x = from_x + ((to_x - from_x) as f64 * eased).round() as i32;
            let cur_y = from_y + ((to_y - from_y) as f64 * eased).round() as i32;
            passthrough::move_window_fast(&window, cur_x, cur_y, false);
            if i < steps {
                std::thread::sleep(Duration::from_millis(step_ms));
            }
        }
        if shared.current_move_anim_seq() == seq && !shared.is_quitting() {
            passthrough::sync_after_drag(&shared);
        }
    });
}

fn save_pos_and_edge(shared: &Shared, x: i32, y: i32, side: Option<EdgeSide>) {
    if let Ok(mut pet) = shared.inner.pet.lock() {
        pet.pos = PetPos {
            x: x as f64,
            y: y as f64,
        };
    }
    let _ = shared
        .inner
        .db
        .save_setting("pos", &serde_json::json!({ "x": x, "y": y }));
    shared.set_cached_setting("pos", serde_json::json!({ "x": x, "y": y }));

    let edge_val = match side {
        Some(EdgeSide::Left) => serde_json::json!("left"),
        Some(EdgeSide::Right) => serde_json::json!("right"),
        None => serde_json::Value::Null,
    };
    let _ = shared.inner.db.save_setting("edgeSide", &edge_val);
    shared.set_cached_setting("edgeSide", edge_val);
}

/// 拖拽结束统一出口（`pet_interact("dragEnd")` 与 `passthrough` 兜底共用）：
/// 检测屏幕边缘吸附、平滑贴边/防越界回弹、同步坐标与落库。
pub fn finalize_drag_release(shared: &Shared, window: &WebviewWindow, now: i64) {
    passthrough::sync_after_drag(shared);

    let (win_x, win_y, win_w, win_h, dpi, work) = match (
        window.outer_position(),
        window.outer_size(),
    ) {
        (Ok(p), Ok(s)) => (
            p.x,
            p.y,
            s.width as i32,
            s.height as i32,
            window.scale_factor().unwrap_or(1.0),
            monitor_work_area_for_window(window),
        ),
        _ => {
            shared.set_edge_side(None);
            shared.persist_pet(now);
            shared.send(SchedulerEvent::UserDragEnd { x: 0.0, y: 0.0 });
            shared.emit_stats();
            return;
        }
    };

    let ratios = character_body_ratios(shared);
    let enabled = shared.edge_docking_enabled();

    let (final_x, final_y, side) = if enabled {
        if let Some((snap_side, tx, ty)) =
            evaluate_edge_snap(win_x, win_y, win_w, win_h, dpi, work, ratios)
        {
            (tx, ty, Some(snap_side))
        } else {
            let (cx, cy) = clamp_free_window_position(win_x, win_y, win_w, win_h, dpi, work, ratios);
            (cx, cy, None)
        }
    } else {
        let (cx, cy) = clamp_free_window_position(win_x, win_y, win_w, win_h, dpi, work, ratios);
        (cx, cy, None)
    };

    shared.set_edge_side(side);
    save_pos_and_edge(shared, final_x, final_y, side);
    emit_edge_dock_for_rect(shared, final_x, win_w, dpi, work, None);

    if final_x != win_x || final_y != win_y {
        animate_window_move(shared.clone(), window.clone(), win_x, win_y, final_x, final_y, 180);
    }

    shared.persist_pet(now);
    shared.send(SchedulerEvent::UserDragEnd {
        x: final_x as f64,
        y: final_y as f64,
    });
    shared.emit_stats();
}

/// 单击贴边状态下的角色，或关闭边缘吸附开关时，将其平滑弹出到桌面可见区内。
pub fn undock_from_edge(shared: &Shared, app: &AppHandle) {
    let Some(side) = shared.edge_side() else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        shared.set_edge_side(None);
        return;
    };
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        shared.set_edge_side(None);
        return;
    };
    let dpi = window.scale_factor().unwrap_or(1.0);
    let work = monitor_work_area_for_window(&window);
    let ratios = character_body_ratios(shared);
    let win_w = size.width as i32;
    let win_h = size.height as i32;

    let (tx, ty) = compute_undock_position(side, pos.y, win_w, win_h, dpi, work, ratios);
    shared.set_edge_side(None);
    save_pos_and_edge(shared, tx, ty, None);
    emit_edge_dock_for_rect(shared, tx, win_w, dpi, work, None);
    animate_window_move(shared.clone(), window, pos.x, pos.y, tx, ty, 190);

    reposition_panel_at_main_rect(app, shared, tx, ty, win_w, win_h, dpi, work, false);
}

/// 切换 35% / 50% / 75% 缩放后，保持贴边对齐并重排已打开的独立面板窗口。
pub fn reposition_after_scale(shared: &Shared, app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return;
    };
    let dpi = window.scale_factor().unwrap_or(1.0);
    let work = monitor_work_area_for_window(&window);
    let ratios = character_body_ratios(shared);
    let win_w = size.width as i32;
    let win_h = size.height as i32;

    let (fx, fy) = if let Some(side) = shared.edge_side() {
        let (tx, ty) = compute_edge_dock_target(side, pos.y, win_w, win_h, dpi, work, ratios);
        if tx != pos.x || ty != pos.y {
            shared.bump_move_anim_seq();
            passthrough::move_window_fast(&window, tx, ty, false);
            save_pos_and_edge(shared, tx, ty, Some(side));
        }
        (tx, ty)
    } else {
        let (cx, cy) = clamp_free_window_position(pos.x, pos.y, win_w, win_h, dpi, work, ratios);
        if cx != pos.x || cy != pos.y {
            shared.bump_move_anim_seq();
            passthrough::move_window_fast(&window, cx, cy, false);
            save_pos_and_edge(shared, cx, cy, None);
        }
        (cx, cy)
    };

    emit_edge_dock_for_rect(shared, fx, win_w, dpi, work, None);
    reposition_panel_at_main_rect(app, shared, fx, fy, win_w, win_h, dpi, work, false);
}

/// 启动时恢复位置并按所在显示器工作区做安全钳位，避免拔掉外接屏后角色丢失。
pub fn restore_and_clamp_startup_position(shared: &Shared, window: &WebviewWindow) {
    let pet = shared.snapshot();
    let has_saved_pos = shared.cached_setting("pos").is_some() || pet.pos.x != 0.0 || pet.pos.y != 0.0;
    if !has_saved_pos {
        emit_edge_dock_to_main(shared, window);
        return;
    }

    let Ok(size) = window.outer_size() else {
        return;
    };
    let win_w = size.width as i32;
    let win_h = size.height as i32;
    let dpi = window.scale_factor().unwrap_or(1.0);
    let raw_x = pet.pos.x.round() as i32;
    let raw_y = pet.pos.y.round() as i32;
    let work = monitor_work_area_for_point(raw_x + win_w / 2, raw_y + win_h / 2);
    let ratios = character_body_ratios(shared);

    let saved_edge = if shared.edge_docking_enabled() {
        match shared
            .cached_setting("edgeSide")
            .and_then(|v| v.as_str().map(str::to_string))
            .as_deref()
        {
            Some("left") => Some(EdgeSide::Left),
            Some("right") => Some(EdgeSide::Right),
            _ => None,
        }
    } else {
        None
    };

    let (fx, fy, side) = if let Some(s) = saved_edge {
        let (tx, ty) = compute_edge_dock_target(s, raw_y, win_w, win_h, dpi, work, ratios);
        (tx, ty, Some(s))
    } else {
        let (cx, cy) = clamp_free_window_position(raw_x, raw_y, win_w, win_h, dpi, work, ratios);
        (cx, cy, None)
    };

    shared.set_edge_side(side);
    let _ = window.set_position(PhysicalPosition::new(fx, fy));
    if fx != raw_x || fy != raw_y {
        save_pos_and_edge(shared, fx, fy, side);
    }
    emit_edge_dock_for_rect(shared, fx, win_w, dpi, work, Some("main"));
}

#[allow(clippy::too_many_arguments)]
fn reposition_panel_at_main_rect(
    app: &AppHandle,
    shared: &Shared,
    main_x: i32,
    main_y: i32,
    main_w: i32,
    main_h: i32,
    dpi: f64,
    work: WorkArea,
    focus_panel: bool,
) {
    let (open, tab) = shared.panel_state();
    if !open {
        return;
    }
    let Some(panel) = app.get_webview_window("panel") else {
        return;
    };

    let ratios = character_body_ratios(shared);
    let panel_w = (PANEL_LOGICAL_W * dpi).round() as i32;
    let panel_h = (PANEL_LOGICAL_H * dpi).round() as i32;
    let (px, py) = compute_panel_position(
        main_x,
        main_y,
        main_w,
        main_h,
        panel_w,
        panel_h,
        dpi,
        shared.edge_side(),
        work,
        ratios,
    );

    let pinned = shared.snapshot().pinned;
    let _ = panel.set_always_on_top(pinned);
    let _ = panel.set_position(PhysicalPosition::new(px, py));
    let _ = panel.show();
    if focus_panel || tab == "chat" {
        let _ = panel.set_focus();
    }
}

pub fn reposition_panel_if_open(app: &AppHandle, shared: &Shared) {
    let Some(main_win) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(pos), Ok(size)) = (main_win.outer_position(), main_win.outer_size()) else {
        return;
    };
    let dpi = main_win.scale_factor().unwrap_or(1.0);
    let work = monitor_work_area_for_window(&main_win);
    reposition_panel_at_main_rect(
        app,
        shared,
        pos.x,
        pos.y,
        size.width as i32,
        size.height as i32,
        dpi,
        work,
        false,
    );
}

pub fn hide_panel(app: &AppHandle, shared: &Shared) {
    let (_, tab) = shared.panel_state();
    let (open, tab) = shared.set_panel_state(false, Some(tab));
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
    let _ = app.emit("panel_state", PanelStateEvent { open, tab });
}

/// 处理来自前端的 `settings_set("panelOpen", ...)`。
/// 支持 `{ open: bool, tab?: string }` 与 `{ toggle: true, tab?: string }`。
pub fn handle_panel_open_setting(
    app: &AppHandle,
    shared: &Shared,
    value: &serde_json::Value,
) -> Result<(), String> {
    let req_tab = value
        .get("tab")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let (cur_open, cur_tab) = shared.panel_state();
    let toggle = value
        .get("toggle")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let target_open = if toggle {
        if let Some(ref t) = req_tab {
            !(cur_open && cur_tab == *t)
        } else {
            !cur_open
        }
    } else {
        value
            .get("open")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| "panelOpen 需要 { open: boolean } 或 { toggle: true }".to_string())?
    };

    let (open, tab) = shared.set_panel_state(target_open, req_tab);
    if !open {
        if let Some(panel) = app.get_webview_window("panel") {
            let _ = panel.hide();
        }
        let _ = app.emit("panel_state", PanelStateEvent { open: false, tab });
        return Ok(());
    }

    if let Some(main_win) = app.get_webview_window("main") {
        if let (Ok(pos), Ok(size)) = (main_win.outer_position(), main_win.outer_size()) {
            let dpi = main_win.scale_factor().unwrap_or(1.0);
            let work = monitor_work_area_for_window(&main_win);
            reposition_panel_at_main_rect(
                app,
                shared,
                pos.x,
                pos.y,
                size.width as i32,
                size.height as i32,
                dpi,
                work,
                true,
            );
        }
    }
    let _ = app.emit("panel_state", PanelStateEvent { open: true, tab });
    Ok(())
}

/// Alt + Space 全局快捷键呼出/隐藏灵动岛聊天输入框。
pub fn toggle_quick_chat_hotkey(app: &AppHandle, shared: &Shared) {
    let (open, tab) = shared.panel_state();
    if open && tab == "chat" {
        hide_panel(app, shared);
        return;
    }
    let _ = handle_panel_open_setting(
        app,
        shared,
        &serde_json::json!({ "open": true, "tab": "chat" }),
    );
    if let Some(panel) = app.get_webview_window("panel") {
        #[cfg(windows)]
        {
            if let Ok(hwnd) = panel.hwnd() {
                #[link(name = "user32")]
                extern "system" {
                    fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
                }
                unsafe {
                    SetForegroundWindow(hwnd.0 as *mut core::ffi::c_void);
                }
            }
        }
        let _ = panel.set_focus();
    }
    let _ = app.emit_to("panel", "pet_focus_chat_input", true);
    let _ = app.emit_to("main", "pet_focus_chat_input", true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std_work() -> WorkArea {
        WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }

    fn rects_overlap(ax1: i32, ay1: i32, ax2: i32, ay2: i32, bx1: i32, by1: i32, bx2: i32, by2: i32) -> bool {
        ax1 < bx2 && ax2 > bx1 && ay1 < by2 && ay2 > by1
    }

    #[test]
    fn panel_never_overlaps_character_or_island_across_all_scales_and_edges() {
        let work = std_work();
        let ratios = BodyRatios::default();
        let dpi = 1.0;
        let pw = PANEL_LOGICAL_W as i32;
        let ph = PANEL_LOGICAL_H as i32;

        // (win_x, win_y, win_w, win_h, edge_side)
        let scenarios = [
            // 50% 缩放（640x450）：居中、靠左、靠右
            (640, 300, 640, 450, None),
            (0, 300, 640, 450, None),
            (1280, 300, 640, 450, None),
            // 35% 缩放（448x342）：居中、靠右
            (736, 360, 448, 342, None),
            (1472, 360, 448, 342, None),
            // 75% 缩放（960x630）：居中、贴左、贴右
            (480, 200, 960, 630, None),
            (-120, 200, 960, 630, None),
            (1100, 200, 960, 630, None),
            // 左侧 / 右侧边缘吸附态
            (-305, 300, 640, 450, Some(EdgeSide::Left)),
            (1585, 300, 640, 450, Some(EdgeSide::Right)),
            (-213, 350, 448, 342, Some(EdgeSide::Left)),
            (1707, 350, 448, 342, Some(EdgeSide::Right)),
        ];

        for (wx, wy, ww, wh, side) in scenarios {
            let (px, py) = compute_panel_position(wx, wy, ww, wh, pw, ph, dpi, side, work, ratios);
            assert!(px >= work.left && px + pw <= work.right, "x out of bounds: {px}");
            assert!(py >= work.top && py + ph <= work.bottom, "y out of bounds: {py}");

            let char_rect = compute_char_screen_rect(wx, wy, ww, wh, ratios);
            let (il, ir) = compute_island_screen_span(wx, ww, dpi, side, work);

            assert!(
                !rects_overlap(px, py, px + pw, py + ph, char_rect.left, char_rect.top, char_rect.right, char_rect.bottom),
                "panel ({px},{py}) overlaps char {:?} at scale win=({wx},{wy},{ww},{wh})",
                char_rect
            );
            assert!(
                !rects_overlap(px, py, px + pw, py + ph, il, wy, ir, wy + 86),
                "panel ({px},{py}) overlaps island [{il},{ir}] at win=({wx},{wy},{ww},{wh})"
            );
        }
    }

    #[test]
    fn edge_snap_triggers_on_left_right_and_multi_monitor_negative_coords() {
        let work = WorkArea {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let ratios = BodyRatios::default();

        // 50% scale on secondary monitor [-1920, 0]
        // body_left = win_x + 192; trigger when char_left <= -1920 + 44 = -1876 => win_x <= -2068
        let snap_left = evaluate_edge_snap(-2090, 300, 640, 450, 1.0, work, ratios)
            .expect("should snap left on negative monitor");
        assert_eq!(snap_left.0, EdgeSide::Left);
        let docked_char = compute_char_screen_rect(snap_left.1, snap_left.2, 640, 450, ratios);
        assert!(docked_char.left < work.left && docked_char.right > work.left);

        // Near right edge (0): char_right = win_x + 448 >= -44 => win_x >= -492
        let snap_right = evaluate_edge_snap(-470, 300, 640, 450, 1.0, work, ratios)
            .expect("should snap right on negative monitor");
        assert_eq!(snap_right.0, EdgeSide::Right);
        let docked_r = compute_char_screen_rect(snap_right.1, snap_right.2, 640, 450, ratios);
        assert!(docked_r.left < work.right && docked_r.right > work.right);

        // Center of negative monitor should not snap
        assert!(evaluate_edge_snap(-1280, 300, 640, 450, 1.0, work, ratios).is_none());
    }

    #[test]
    fn undock_brings_character_and_island_cleanly_inside_work_area() {
        let work = std_work();
        let ratios = BodyRatios::default();

        for side in [EdgeSide::Left, EdgeSide::Right] {
            let (ux, uy) = compute_undock_position(side, 300, 640, 450, 1.0, work, ratios);
            let char_rect = compute_char_screen_rect(ux, uy, 640, 450, ratios);
            let (il, ir) = compute_island_screen_span(ux, 640, 1.0, None, work);
            assert!(char_rect.left >= work.left + 16);
            assert!(char_rect.right <= work.right - 16);
            assert!(il >= work.left + 16);
            assert!(ir <= work.right - 16);
        }
    }

    #[test]
    fn edge_dock_event_reports_exact_visible_window_span() {
        let work = std_work();
        let left_ev = compute_edge_dock_event(true, Some(EdgeSide::Left), -305, 640, 1.0, work);
        assert_eq!(left_ev.side, Some(EdgeSide::Left));
        assert!((left_ev.visible_left_px - 305.0).abs() < 0.2);
        assert!((left_ev.visible_right_px - 640.0).abs() < 0.2);

        let right_ev = compute_edge_dock_event(true, Some(EdgeSide::Right), 1585, 640, 1.0, work);
        assert_eq!(right_ev.side, Some(EdgeSide::Right));
        assert!((right_ev.visible_left_px - 0.0).abs() < 0.2);
        assert!((right_ev.visible_right_px - 335.0).abs() < 0.2);

        // HiDPI 1.5x test
        let hidpi_ev = compute_edge_dock_event(true, Some(EdgeSide::Left), -450, 960, 1.5, work);
        assert!((hidpi_ev.visible_left_px - 300.0).abs() < 0.2);
        assert!((hidpi_ev.visible_right_px - 640.0).abs() < 0.2);
    }
}
