//! Windows 前台工作态检测。
//!
//! 每 2 秒取一次 `GetForegroundWindow`，再取该窗口所属进程的可执行文件名；
//! 命中 IDE / 终端白名单时，调度器切到 `FOREGROUND_WORK`（30 级）。

/// 小写 exe 名；用 basename 比较，兼容完整路径。
pub const WORK_PROCESSES: &[&str] = &[
    "code.exe",
    "code - insiders.exe",
    "cursor.exe",
    "windsurf.exe",
    "antigravity.exe",
    "devenv.exe",
    "idea64.exe",
    "pycharm64.exe",
    "webstorm64.exe",
    "goland64.exe",
    "clion64.exe",
    "rustrover64.exe",
    "rider64.exe",
    "android studio.exe",
    "studio64.exe",
    "windows terminal.exe",
    "windowsterminal.exe",
    "wt.exe",
    "wezterm-gui.exe",
    "alacritty.exe",
    "tabby.exe",
    "powershell.exe",
    "pwsh.exe",
    "cmd.exe",
    "conhost.exe",
    "sublime_text.exe",
    "notepad++.exe",
    "zed.exe",
    "trae.exe",
];

/// 进程名（可带完整路径）是否属于工作应用白名单。
pub fn process_name_matches(exe: &str) -> bool {
    let base = exe
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe)
        .trim()
        .to_ascii_lowercase();
    WORK_PROCESSES.iter().any(|item| *item == base)
}

#[cfg(windows)]
pub fn current_foreground_process() -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        if ok == 0 || length == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

#[cfg(not(windows))]
pub fn current_foreground_process() -> Option<String> {
    None
}

pub fn foreground_is_work() -> bool {
    current_foreground_process()
        .map(|process| process_name_matches(&process))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ide_and_terminal_names() {
        assert!(process_name_matches("Code.exe"));
        assert!(process_name_matches(
            r"C:\Program Files\Microsoft VS Code\Code.exe"
        ));
        assert!(process_name_matches("CURSOR.EXE"));
        assert!(process_name_matches("Antigravity.exe"));
        assert!(process_name_matches("WindowsTerminal.exe"));
        assert!(process_name_matches("wezterm-gui.exe"));
        assert!(process_name_matches("pwsh.exe"));
        assert!(process_name_matches("pycharm64.exe"));
    }

    #[test]
    fn rejects_non_work_apps() {
        assert!(!process_name_matches("chrome.exe"));
        assert!(!process_name_matches("msedge.exe"));
        assert!(!process_name_matches("explorer.exe"));
        assert!(!process_name_matches("not-code.exe"));
        assert!(!process_name_matches(""));
    }
}
