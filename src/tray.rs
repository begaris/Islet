//! Notification-area icon with a small menu (Settings / Pause / Click-through /
//! Open settings file / Claude Code hooks / Quit).

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_TRAY: u32 = WM_APP + 2;
pub const CMD_PAUSE: usize = 1;
pub const CMD_CONFIG: usize = 2;
pub const CMD_QUIT: usize = 3;
pub const CMD_HOOKS_INSTALL: usize = 4;
pub const CMD_HOOKS_REMOVE: usize = 5;
pub const CMD_SETTINGS: usize = 6;
pub const CMD_CLICK_THROUGH: usize = 7;

/// Mirrors the app's paused state so the menu can show a check mark.
pub static PAUSED: AtomicBool = AtomicBool::new(false);
/// Mirrors the click-through setting: with it on, the tray is the way back.
pub static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);
pub const CMD_UPDATE: usize = 8;
/// A newer release found by the update check: (version, release page).
pub static UPDATE: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

pub fn add(hwnd: HWND) {
    let mut nid = data(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = make_icon();
    for (dst, src) in nid.szTip.iter_mut().zip("Islet".encode_utf16()) {
        *dst = src;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
    }
}

/// Handles the tray callback. Runs the menu's modal loop, so it must be called
/// without holding the app state; the chosen command is posted back as `WM_COMMAND`.
pub fn on_message(hwnd: HWND, lparam: LPARAM) {
    let event = lparam.0 as u32;
    if event != WM_RBUTTONUP && event != WM_LBUTTONUP && event != WM_CONTEXTMENU {
        return;
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let checked = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        if let Some((version, _)) = UPDATE.lock().ok().and_then(|u| u.clone()) {
            let label = windows::core::HSTRING::from(format!("Get Islet {version}\u{2026}"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_UPDATE, &label);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, w!("Settings\u{2026}"));
        let _ = AppendMenuW(menu, checked(PAUSED.load(Relaxed)), CMD_PAUSE, w!("Pause"));
        let _ = AppendMenuW(menu, checked(CLICK_THROUGH.load(Relaxed)), CMD_CLICK_THROUGH, w!("Click-through"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_CONFIG, w!("Open settings file"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_HOOKS_INSTALL, w!("Install Claude Code hooks"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_HOOKS_REMOVE, w!("Remove Claude Code hooks"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        if cmd.0 != 0 {
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(cmd.0 as usize), LPARAM(0));
        }
    }
}

/// Draws the tray icon procedurally: a black pill with a white rim.
fn make_icon() -> HICON {
    const S: i32 = 32;
    let mut pixels = [0u32; (S * S) as usize];
    let (hw, hh, r) = (13.0f32, 6.5f32, 6.5f32);
    for y in 0..S {
        for x in 0..S {
            let px = (x as f32 + 0.5 - S as f32 / 2.0).abs() - (hw - r);
            let py = (y as f32 + 0.5 - S as f32 / 2.0).abs() - (hh - r);
            let dist = px.max(0.0).hypot(py.max(0.0)) + px.max(py).min(0.0) - r;
            pixels[(y * S + x) as usize] = if dist < -2.0 {
                0xFF00_0000
            } else if dist < 0.0 {
                0xFFFF_FFFF
            } else {
                0
            };
        }
    }
    unsafe {
        let color = CreateBitmap(S, S, 1, 32, Some(pixels.as_ptr() as *const _));
        let mask = CreateBitmap(S, S, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
