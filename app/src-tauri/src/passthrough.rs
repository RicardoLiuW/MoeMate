//! 33ms 像素穿透轮询（Windows）。
//!
//! 逻辑：
//! 1. `GetCursorPos` 取物理屏幕坐标；
//! 2. 结合主窗口 `outer_position()` / `outer_size()` 归一化到 `(u,v)`；
//! 3. 镜像段 `u = 1.0 - u`；
//! 4. 查当前 clipId × 当前帧的 `mask.bin`（16 段 LRU，>2MB 拒载）；
//! 5. 命中 → `set_ignore_cursor_events(false)`；离开满 300ms → 恢复 true；
//! 6. `dragging` 或 `uiInteractive` 期间强制 false（防 UI 穿透死锁）。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tauri::{PhysicalPosition, WebviewWindow};

use crate::mask::load_mask_bytes;
use crate::state::{DragFastState, Shared};
use crate::stats::now_ms;

const POLL_INTERVAL_IDLE_MS: u64 = 33;
const POLL_INTERVAL_HOVER_MS: u64 = 16;
const DRAG_POLL_INTERVAL_MS: u64 = 2;
const DRAG_RELEASE_GRACE_MS: i64 = 20;
const LEAVE_RESTORE_MS: i64 = 300;

#[cfg(windows)]
const DESKPET_DRAG_SUBCLASS_ID: usize = 0x4450_4452;
#[cfg(windows)]
const WM_APP_DRAG_MOVE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x41;
#[cfg(windows)]
const WM_APP_DRAG_SYNC: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x42;

enum HitOutcome {
    Outside,
    InsideTransparent,
    Hit,
}

#[cfg(windows)]
struct HighResTimerGuard(u32);

#[cfg(windows)]
impl HighResTimerGuard {
    fn new(period_ms: u32) -> Self {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        unsafe {
            timeBeginPeriod(period_ms);
        }
        Self(period_ms)
    }
}

#[cfg(windows)]
impl Drop for HighResTimerGuard {
    fn drop(&mut self) {
        #[link(name = "winmm")]
        extern "system" {
            fn timeEndPeriod(period: u32) -> u32;
        }
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

#[cfg(not(windows))]
struct HighResTimerGuard;

#[cfg(not(windows))]
impl HighResTimerGuard {
    fn new(_period_ms: u32) -> Self {
        Self
    }
}

pub fn spawn(shared: Shared, window: WebviewWindow) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
            };
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
        }

        let mut last_ignore: Option<bool> = None;
        let mut last_hit_ms = now_ms() - LEAVE_RESTORE_MS;
        let mut last_drag_pos: Option<(i32, i32)> = None;
        let mut last_in_window = false;
        let mut logged_failures: HashSet<String> = HashSet::new();
        let mut wake_seq: u64 = 0;
        let mut _drag_timer: Option<HighResTimerGuard> = None;

        loop {
            let drag_info = shared.drag_state();
            if drag_info.is_some() {
                if _drag_timer.is_none() {
                    _drag_timer = Some(HighResTimerGuard::new(1));
                }
            } else {
                _drag_timer = None;
            }

            let sleep_ms = if drag_info.is_some() {
                DRAG_POLL_INTERVAL_MS
            } else if last_in_window {
                POLL_INTERVAL_HOVER_MS
            } else {
                POLL_INTERVAL_IDLE_MS
            };
            shared.wait_passthrough(&mut wake_seq, Duration::from_millis(sleep_ms));
            if shared.is_quitting() {
                break;
            }

            let now = now_ms();

            // v2.1：500Hz 无锁光标探测 + 主线程窗口子类单发合并直移 + 拖拽期屏蔽双 COM 位置广播。
            if let Some((grab_offset, started_ms)) = shared.drag_state() {
                if _drag_timer.is_none() {
                    _drag_timer = Some(HighResTimerGuard::new(1));
                }
                if !is_primary_button_down() && now - started_ms >= DRAG_RELEASE_GRACE_MS {
                    // 兜底：即便鼠标在窗口外或失焦释放，也能立刻安全结束拖拽并触发边缘吸附结算。
                    if shared.end_drag() {
                        crate::window_layout::finalize_drag_release(&shared, &window, now);
                    }
                    last_drag_pos = None;
                } else if let (Some((ox, oy)), Some((cx, cy))) = (grab_offset, cursor_point()) {
                    let target = (cx - ox, cy - oy);
                    if last_drag_pos != Some(target) {
                        request_drag_move(&shared, &window, target.0, target.1);
                        last_drag_pos = Some(target);
                    }
                }
            } else {
                last_drag_pos = None;
            }

            let locked = shared.passthrough_locked();
            let hit = if locked {
                last_in_window = false;
                false
            } else {
                match hit_test(&shared, &window, now, &mut logged_failures) {
                    HitOutcome::Hit => {
                        last_in_window = true;
                        true
                    }
                    HitOutcome::InsideTransparent => {
                        last_in_window = true;
                        false
                    }
                    HitOutcome::Outside => {
                        last_in_window = false;
                        false
                    }
                }
            };
            if hit {
                last_hit_ms = now;
            }

            let want_ignore = if locked {
                false
            } else if hit {
                false
            } else {
                now - last_hit_ms >= LEAVE_RESTORE_MS
            };

            if last_ignore != Some(want_ignore) {
                match window.set_ignore_cursor_events(want_ignore) {
                    Ok(()) => last_ignore = Some(want_ignore),
                    Err(err) => {
                        if logged_failures.insert("set_ignore".to_string()) {
                            eprintln!("[deskpet] 设置窗口穿透失败: {err}");
                        }
                    }
                }
            }
        }
    });
}

fn hit_test(
    shared: &Shared,
    window: &WebviewWindow,
    now: i64,
    logged_failures: &mut HashSet<String>,
) -> HitOutcome {
    let Ok(position) = window.outer_position() else {
        return HitOutcome::Outside;
    };
    let Ok(size) = window.outer_size() else {
        return HitOutcome::Outside;
    };
    if size.width == 0 || size.height == 0 {
        return HitOutcome::Outside;
    }

    let Some((cursor_x, cursor_y)) = cursor_point() else {
        return HitOutcome::Outside;
    };
    let local_x = cursor_x - position.x;
    let local_y = cursor_y - position.y;
    let u = local_x as f64 / size.width as f64;
    let v = local_y as f64 / size.height as f64;
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return HitOutcome::Outside;
    }

    // v1.2：常驻 UI 热区（头顶/侧翼灵动岛）优先于 mask；用全窗原始 (u, v)，
    // 不随角色镜像翻转，保证灵动岛位置稳定可点。
    if shared.ui_hit_test(u, v) {
        return HitOutcome::Hit;
    }

    // v1.5：角色视频底对齐占满全窗宽度（16:9），顶部多出的透明头顶灵动岛空带直接穿透。
    let Some((mask_u, mask_v)) = map_mask_uv(
        local_x as f64,
        local_y as f64,
        size.width as f64,
        size.height as f64,
    ) else {
        return HitOutcome::InsideTransparent;
    };

    let Some((current, frame)) = shared.frame_for_hint(now) else {
        return HitOutcome::InsideTransparent;
    };
    let key = format!("{}::{}", current.char_id, current.clip_id);
    let path = crate::assets_root()
        .join("chars")
        .join(&current.char_id)
        .join("clips")
        .join(format!("{}.mask.bin", current.clip_id));

    let Ok(mut cache) = shared.inner.masks.lock() else {
        return HitOutcome::InsideTransparent;
    };
    match cache.get_or_load(&key, current.frames, || {
        load_mask_bytes(&path, current.frames)
    }) {
        Ok(mask) => {
            if mask.hit_relaxed(frame, mask_u, mask_v, current.mirror, 1) {
                HitOutcome::Hit
            } else {
                HitOutcome::InsideTransparent
            }
        }
        Err(err) => {
            if logged_failures.insert(key) {
                eprintln!("[deskpet] 掩码加载失败，跳过命中测试: {err}");
            }
            HitOutcome::InsideTransparent
        }
    }
}

/// 将窗口内物理坐标映射到“底对齐 16:9 角色视频区”的归一化 `(u, v)`。
/// 落入头顶透明安全带（`local_y < top_band`）时返回 `None`，保持空白穿透。
pub(crate) fn map_mask_uv(
    local_x: f64,
    local_y: f64,
    win_w: f64,
    win_h: f64,
) -> Option<(f64, f64)> {
    if win_w <= 0.0 || win_h <= 0.0 {
        return None;
    }
    let scale = (win_w / 1280.0).min(win_h / 720.0);
    let draw_w = 1280.0 * scale;
    let draw_h = 720.0 * scale;
    if draw_w <= 0.0 || draw_h <= 0.0 {
        return None;
    }
    let draw_x = (win_w - draw_w) * 0.5;
    let draw_y = win_h - draw_h;
    let u = (local_x - draw_x) / draw_w;
    let v = (local_y - draw_y) / draw_h;
    if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
        Some((u, v))
    } else {
        None
    }
}

#[cfg(windows)]
pub(crate) fn cursor_point() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok == 0 {
        None
    } else {
        Some((point.x, point.y))
    }
}

#[cfg(not(windows))]
pub(crate) fn cursor_point() -> Option<(i32, i32)> {
    None
}

#[cfg(windows)]
unsafe fn move_hwnd_fast(hwnd: windows_sys::Win32::Foundation::HWND, x: i32, y: i32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOSIZE,
        SWP_NOZORDER,
    };

    let flags =
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING;
    SetWindowPos(hwnd, std::ptr::null_mut(), x, y, 0, 0, flags) != 0
}

#[cfg(windows)]
unsafe extern "system" fn drag_subclass_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    uidsubclass: usize,
    dwrefdata: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_MOVE, WM_NCDESTROY,
        WM_WINDOWPOSCHANGED,
    };

    if dwrefdata == 0 {
        return DefSubclassProc(hwnd, msg, wparam, lparam);
    }
    let state = &*(dwrefdata as *const DragFastState);

    match msg {
        WM_WINDOWPOSCHANGED if state.is_dragging() && lparam != 0 => {
            let wp = &*(lparam as *const WINDOWPOS);
            if (wp.flags & SWP_NOSIZE) != 0 && (wp.flags & SWP_NOMOVE) == 0 {
                // 内核已完成原子位移动作；跳过拖拽期高频 tao Moved(ExecuteScript) 与 wry COM 位置通知。
                return 0;
            }
        }
        WM_APP_DRAG_MOVE => {
            state.clear_move_post();
            if state.is_dragging() {
                if let (Some((ox, oy)), Some((cx, cy))) = (state.grab_offset(), cursor_point()) {
                    let tx = cx - ox;
                    let ty = cy - oy;
                    if state.update_last_pos(tx, ty) {
                        let _ = move_hwnd_fast(hwnd, tx, ty);
                    }
                }
            }
            return 0;
        }
        WM_APP_DRAG_SYNC => {
            state.clear_sync_post();
            if !state.is_dragging() {
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if GetWindowRect(hwnd, &mut rect) != 0 {
                    let packed = (((rect.top as i16 as u16) as usize) << 16)
                        | ((rect.left as i16 as u16) as usize);
                    let _ = DefSubclassProc(hwnd, WM_MOVE, 0, packed as isize);
                }
            }
            return 0;
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(drag_subclass_proc), uidsubclass);
            drop(Arc::from_raw(dwrefdata as *const DragFastState));
            return DefSubclassProc(hwnd, msg, wparam, lparam);
        }
        _ => {}
    }

    DefSubclassProc(hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
pub fn install_drag_subclass(window: &WebviewWindow, shared: &Shared) {
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;

    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let fast = shared.drag_fast();
    fast.set_hwnd(hwnd.0 as isize);
    let raw = Arc::into_raw(fast);
    let ok = unsafe {
        SetWindowSubclass(
            hwnd.0 as _,
            Some(drag_subclass_proc),
            DESKPET_DRAG_SUBCLASS_ID,
            raw as usize,
        )
    };
    if ok != 0 {
        shared.drag_fast().mark_subclass_installed();
    } else {
        unsafe { drop(Arc::from_raw(raw)) };
        eprintln!("[deskpet] 安装拖拽快速子类失败，将使用常规移动通道");
    }
}

#[cfg(not(windows))]
pub fn install_drag_subclass(_window: &WebviewWindow, _shared: &Shared) {}

#[cfg(windows)]
pub(crate) fn apply_drag_move_on_main_thread(shared: &Shared, window: &WebviewWindow) {
    let fast = shared.drag_fast();
    if let (Some((ox, oy)), Some((cx, cy))) = (fast.grab_offset(), cursor_point()) {
        let tx = cx - ox;
        let ty = cy - oy;
        fast.update_last_pos(tx, ty);
        if let Ok(hwnd) = window.hwnd() {
            if unsafe { move_hwnd_fast(hwnd.0 as _, tx, ty) } {
                return;
            }
        }
        move_window_fast(window, tx, ty, false);
    }
}

#[cfg(not(windows))]
pub(crate) fn apply_drag_move_on_main_thread(shared: &Shared, window: &WebviewWindow) {
    let fast = shared.drag_fast();
    if let (Some((ox, oy)), Some((cx, cy))) = (fast.grab_offset(), cursor_point()) {
        move_window_fast(window, cx - ox, cy - oy, false);
    }
}

#[cfg(windows)]
pub(crate) fn request_drag_move(shared: &Shared, window: &WebviewWindow, tx: i32, ty: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    let fast = shared.drag_fast();
    let hwnd = fast.hwnd();
    if fast.is_subclass_installed() && hwnd != 0 {
        if fast.try_claim_move_post() {
            let posted = unsafe { PostMessageW(hwnd as _, WM_APP_DRAG_MOVE, 0, 0) };
            if posted == 0 {
                fast.clear_move_post();
            }
        }
        return;
    }
    move_window_fast(window, tx, ty, false);
}

#[cfg(not(windows))]
pub(crate) fn request_drag_move(_shared: &Shared, window: &WebviewWindow, tx: i32, ty: i32) {
    move_window_fast(window, tx, ty, false);
}

#[cfg(windows)]
pub(crate) fn sync_after_drag(shared: &Shared) {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    let fast = shared.drag_fast();
    let hwnd = fast.hwnd();
    if fast.is_subclass_installed() && hwnd != 0 && fast.try_claim_sync_post() {
        let posted = unsafe { PostMessageW(hwnd as _, WM_APP_DRAG_SYNC, 0, 0) };
        if posted == 0 {
            fast.clear_sync_post();
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn sync_after_drag(_shared: &Shared) {}

#[cfg(windows)]
pub(crate) fn move_window_fast(window: &WebviewWindow, x: i32, y: i32, async_move: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    };

    if let Ok(hwnd) = window.hwnd() {
        let mut flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
        if async_move {
            flags |= SWP_ASYNCWINDOWPOS;
        }
        let ok = unsafe { SetWindowPos(hwnd.0 as _, std::ptr::null_mut(), x, y, 0, 0, flags) };
        if ok != 0 {
            return;
        }
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[cfg(not(windows))]
pub(crate) fn move_window_fast(window: &WebviewWindow, x: i32, y: i32, _async_move: bool) {
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[cfg(windows)]
fn is_button_swapped() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
}

#[cfg(windows)]
fn is_vk_down(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

#[cfg(windows)]
pub(crate) fn is_primary_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON};
    let vk = if is_button_swapped() {
        VK_RBUTTON
    } else {
        VK_LBUTTON
    };
    is_vk_down(vk)
}

#[cfg(not(windows))]
pub(crate) fn is_primary_button_down() -> bool {
    true
}

#[cfg(windows)]
pub(crate) fn is_secondary_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON};
    let vk = if is_button_swapped() {
        VK_LBUTTON
    } else {
        VK_RBUTTON
    };
    is_vk_down(vk)
}

#[cfg(not(windows))]
pub(crate) fn is_secondary_button_down() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use crate::state::UiHitRect;

    fn rects() -> Vec<UiHitRect> {
        vec![UiHitRect {
            x: 0.05,
            y: 0.80,
            w: 0.20,
            h: 0.12,
        }]
    }

    #[test]
    fn ui_hit_rect_matches_inside_only() {
        let rects = rects();
        assert!(crate::state::ui_rect_hit(&rects, 0.10, 0.85));
        assert!(crate::state::ui_rect_hit(&rects, 0.25, 0.92));
        assert!(!crate::state::ui_rect_hit(&rects, 0.40, 0.85));
        assert!(!crate::state::ui_rect_hit(&rects, 0.10, 0.50));
    }

    #[test]
    fn ui_hit_rect_uses_raw_coordinates_not_mirrored() {
        // 镜像只发生在 mask 查表内部；热区用原始 u。
        // 原始 0.10 命中左下角徽章，镜像后的 0.90 不应命中同一矩形。
        let rects = rects();
        assert!(crate::state::ui_rect_hit(&rects, 0.10, 0.85));
        assert!(!crate::state::ui_rect_hit(&rects, 0.90, 0.85));
    }

    #[test]
    fn empty_rects_never_hit() {
        assert!(!crate::state::ui_rect_hit(&[], 0.5, 0.5));
    }

    #[test]
    fn map_mask_uv_skips_top_crown_band_and_aligns_bottom_stage() {
        // 640x450 = 顶部 90px 灵动岛空带 + 底部 640x360 16:9 角色舞台
        assert_eq!(super::map_mask_uv(320.0, 45.0, 640.0, 450.0), None);
        assert_eq!(super::map_mask_uv(320.0, 89.0, 640.0, 450.0), None);
        let (u, v) = super::map_mask_uv(320.0, 270.0, 640.0, 450.0).unwrap();
        assert!((u - 0.5).abs() < 1e-6);
        assert!((v - 0.5).abs() < 1e-6);
        let (u2, v2) = super::map_mask_uv(320.0, 90.0, 640.0, 450.0).unwrap();
        assert!((u2 - 0.5).abs() < 1e-6);
        assert!(v2.abs() < 1e-6);
    }
}
