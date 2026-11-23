//! Cursor tracking through a low-level mouse hook.
//!
//! The hook lives on its own tiny thread so the UI thread can block on vblank
//! without delaying system mouse input. The callback only stores the cursor
//! position into atomics and posts one coalesced wake-up message while the
//! cursor is inside (or has just left) the island's reach zone. Nothing is
//! injected into other processes. The thread is stopped while suspended.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the island window when the cursor needs attention.
pub const WM_APP_CURSOR: u32 = WM_APP + 1;

static CUR_X: AtomicI32 = AtomicI32::new(i32::MIN);
static CUR_Y: AtomicI32 = AtomicI32::new(i32::MIN);
static ZONE: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];
static MONITOR: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];
static FOLLOW: AtomicBool = AtomicBool::new(false);
static WAS_INSIDE: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Last cursor position in screen pixels.
pub fn cursor() -> (i32, i32) {
    let (x, y) = (CUR_X.load(Relaxed), CUR_Y.load(Relaxed));
    if x == i32::MIN {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (p.x, p.y)
    } else {
        (x, y)
    }
}

/// Screen-pixel rectangle in which cursor movement wakes the UI thread.
pub fn set_zone(r: RECT) {
    for (slot, v) in ZONE.iter().zip([r.left, r.top, r.right, r.bottom]) {
        slot.store(v, Relaxed);
    }
}

/// When `follow` is on, leaving `r` (the current monitor) also wakes the UI thread.
pub fn set_monitor(r: RECT, follow: bool) {
    for (slot, v) in MONITOR.iter().zip([r.left, r.top, r.right, r.bottom]) {
        slot.store(v, Relaxed);
    }
    FOLLOW.store(follow, Relaxed);
}

/// The UI thread calls this when it handles `WM_APP_CURSOR`.
pub fn ack() {
    PENDING.store(false, Relaxed);
}

#[inline]
fn inside(rect: &[AtomicI32; 4], x: i32, y: i32) -> bool {
    x >= rect[0].load(Relaxed) && y >= rect[1].load(Relaxed) && x < rect[2].load(Relaxed) && y < rect[3].load(Relaxed)
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE {
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to a MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let (x, y) = (info.pt.x, info.pt.y);
        CUR_X.store(x, Relaxed);
        CUR_Y.store(y, Relaxed);
        let now = inside(&ZONE, x, y);
        let was = WAS_INSIDE.swap(now, Relaxed);
        let wake = now || was || (FOLLOW.load(Relaxed) && !inside(&MONITOR, x, y));
        if wake && !PENDING.swap(true, Relaxed) {
            let hwnd = HWND(TARGET.load(Relaxed) as *mut _);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_CURSOR, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Posted to the island window when Caps Lock was pressed.
pub const WM_APP_CAPS: u32 = WM_APP + 7;

/// Low-level keyboard hook that reacts to one key only: Caps Lock. Every other
/// key is passed on untouched and unrecorded.
unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_KEYDOWN {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if info.vkCode == 0x14 {
            let hwnd = HWND(TARGET.load(Relaxed) as *mut _);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_CAPS, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

static WATCH_CAPS: AtomicBool = AtomicBool::new(false);

pub struct MouseHook {
    thread: Option<JoinHandle<()>>,
}

impl MouseHook {
    /// `caps` also installs the Caps Lock watcher on the same thread.
    pub fn start(target: HWND, caps: bool) -> Self {
        WATCH_CAPS.store(caps, Relaxed);
        TARGET.store(target.0 as isize, Relaxed);
        PENDING.store(false, Relaxed);
        CUR_X.store(i32::MIN, Relaxed);
        let thread = std::thread::Builder::new()
            .name("islet-mouse".into())
            .stack_size(64 * 1024)
            .spawn(|| unsafe {
                THREAD_ID.store(GetCurrentThreadId(), Relaxed);
                let Ok(hook) = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) else {
                    return;
                };
                let keys = if WATCH_CAPS.load(Relaxed) { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0).ok() } else { None };
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
                let _ = UnhookWindowsHookEx(hook);
                if let Some(keys) = keys {
                    let _ = UnhookWindowsHookEx(keys);
                }
            })
            .ok();
        Self { thread }
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // The thread may not have created its message queue yet; retry briefly.
            for _ in 0..200 {
                let id = THREAD_ID.load(Relaxed);
                if thread.is_finished() || (id != 0 && unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let _ = thread.join();
            THREAD_ID.store(0, Relaxed);
        }
    }
}
