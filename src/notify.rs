//! Windows notifications in the pill.
//!
//! An unpackaged app may read the notification list
//! (`UserNotificationListener::GetNotificationsAsync`) but cannot subscribe to
//! `NotificationChanged` (that needs package identity). So instead of polling,
//! the app watches for Windows' own toast popup appearing (a WinEvent, see
//! `App`), and only then reads the list once, on a short-lived worker thread.
//!
//! The trigger relies on how Windows 11 shows toasts today: a
//! `Windows.UI.Core.CoreWindow` owned by ShellExperienceHost.exe. If a Windows
//! update changes that, notifications simply stop appearing in the pill.

use std::sync::Mutex;
use windows::core::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_TIMEOUT, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, PostMessageW, WM_APP};
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

/// Posted to the island window when [`take`] has a fresh list.
pub const WM_APP_NOTIFY: u32 = WM_APP + 8;

#[derive(Clone, Debug)]
pub struct Note {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: String,
}

static LATEST: Mutex<Option<Vec<Note>>> = Mutex::new(None);

/// The list read by the last [`read_async`] call, if it has not been taken yet.
pub fn take() -> Option<Vec<Note>> {
    LATEST.lock().ok()?.take()
}

/// `true` when `hwnd` looks like a shell toast / notification surface. The
/// event hooks are already limited to the shell host process.
pub fn is_shell_surface(hwnd: HWND) -> bool {
    let mut class = [0u16; 40];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    "Windows.UI.Core.CoreWindow".encode_utf16().eq(class[..len].iter().copied())
}

/// The process that shows toasts on Windows 11, held open so its exit can be noticed.
pub struct ShellHost {
    pub pid: u32,
    handle: HANDLE,
}

impl ShellHost {
    /// Finds ShellExperienceHost.exe, if it is running.
    pub fn find() -> Option<Self> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut found = None;
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("ShellExperienceHost.exe") {
                    found = Some(entry.th32ProcessID);
                    break;
                }
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
            let pid = found?;
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok()?;
            Some(Self { pid, handle })
        }
    }

    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }
}

impl Drop for ShellHost {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Reads the current toast notifications on a worker thread and posts
/// `WM_APP_NOTIFY` to `target` when done. Never blocks the caller.
pub fn read_async(target: HWND) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-notify".into()).stack_size(256 * 1024).spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if let Ok(notes) = read() {
            if let Ok(mut latest) = LATEST.lock() {
                *latest = Some(notes);
            }
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_NOTIFY, WPARAM(0), LPARAM(0));
        }
        CoUninitialize();
    });
}

fn read() -> Result<Vec<Note>> {
    let listener = UserNotificationListener::Current()?;
    if listener.GetAccessStatus()? != UserNotificationListenerAccessStatus::Allowed {
        return Ok(Vec::new());
    }
    let list = listener.GetNotificationsAsync(NotificationKinds::Toast)?.join()?;
    let mut notes = Vec::new();
    for item in list {
        if let Ok(note) = describe(&item) {
            notes.push(note);
        }
    }
    Ok(notes)
}

fn describe(item: &UserNotification) -> Result<Note> {
    let id = item.Id()?;
    let app = item.AppInfo().and_then(|a| a.DisplayInfo()).and_then(|d| d.DisplayName()).map(|s| s.to_string_lossy()).unwrap_or_default();
    let mut texts = Vec::new();
    if let Ok(binding) = item.Notification().and_then(|n| n.Visual()).and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?)) {
        for text in binding.GetTextElements()? {
            let line = text.Text().map(|s| s.to_string_lossy()).unwrap_or_default();
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if !line.is_empty() {
                texts.push(line);
            }
        }
    }
    let mut texts = texts.into_iter();
    Ok(Note { id, app, title: texts.next().unwrap_or_default(), body: texts.collect::<Vec<_>>().join(" ") })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diagnostic: for 7 seconds, prints which top-level windows raise show /
    /// uncloak / create events (class and process only). Fire a toast meanwhile
    /// to learn what Windows uses for it.
    #[test]
    #[ignore = "listens to window events for 7 seconds"]
    fn live_window_events() {
        use std::sync::Mutex;
        use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
        use windows::Win32::UI::WindowsAndMessaging::*;
        static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
        unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
            if object != 0 || child != 0 {
                return;
            }
            let mut class = [0u16; 80];
            let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
            let line = format!("event {event:#06x}  {:<28} {}", crate::gamemode::process_name(hwnd).unwrap_or_default(), String::from_utf16_lossy(&class[..len]));
            let mut seen = SEEN.lock().unwrap();
            if !seen.contains(&line) {
                seen.push(line);
            }
        }
        unsafe {
            // 0x8000 create, 0x8002 show, 0x8017 cloaked, 0x8018 uncloaked.
            let hooks = [(0x8000u32, 0x8002u32), (0x8017, 0x8018)].map(|(lo, hi)| SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT));
            let start = std::time::Instant::now();
            let mut msg = MSG::default();
            while start.elapsed().as_secs() < 7 {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            for hook in hooks {
                let _ = UnhookWinEvent(hook);
            }
        }
        for line in SEEN.lock().unwrap().iter() {
            println!("{line}");
        }
    }

    /// Reads the real notification list. Prints counts and app names only, never the contents.
    #[test]
    #[ignore = "reads the real notification list"]
    fn live_read() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match read() {
            Ok(notes) => {
                println!("{} notification(s)", notes.len());
                for n in notes.iter().take(5) {
                    println!("id {} from {:?}: title {} chars, body {} chars", n.id, n.app, n.title.chars().count(), n.body.chars().count());
                }
            }
            Err(e) => println!("read failed: {e}"),
        }
    }
}
