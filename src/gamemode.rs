//! Game / fullscreen detection. Purely observational: it queries the shell's
//! notification state and looks at the foreground window from the outside.
//! Nothing is injected into or hooked inside other processes.

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Shell surfaces that legitimately cover a whole monitor.
const SHELL_CLASSES: [&str; 7] = ["Progman", "WorkerW", "Shell_TrayWnd", "XamlExplorerHostIslandWindow", "MultitaskingViewFrame", "ForegroundStaging", "Windows.UI.Core.CoreWindow"];

/// Returns `true` when the island should be in deep sleep.
pub fn should_suspend(own: HWND, suspend_apps: &[String]) -> bool {
    unsafe {
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                return true;
            }
        }
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == own {
            return false;
        }
        if !suspend_apps.is_empty() {
            if let Some(name) = process_name(fg) {
                if suspend_apps.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                    return true;
                }
            }
        }
        covers_monitor(fg)
    }
}

/// Borderless-windowed games and fullscreen video: a caption-less foreground
/// window that covers its entire monitor.
unsafe fn covers_monitor(fg: HWND) -> bool {
    unsafe {
        if fg == GetShellWindow() || fg == GetDesktopWindow() {
            return false;
        }
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 {
            // A normal (possibly maximised) window keeps its caption.
            return false;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(fg, &mut class) as usize;
        if SHELL_CLASSES.iter().any(|c| c.encode_utf16().eq(class[..len].iter().copied())) {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_err() {
            return false;
        }
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

/// Lower-case executable file name of the process that owns `hwnd`.
pub fn process_name(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(path.rsplit(['\\', '/']).next()?.to_ascii_lowercase())
    }
}
