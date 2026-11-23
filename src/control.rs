//! Control centre: Wi-Fi and Bluetooth radios, the Windows dark theme, and
//! Lock / Sleep / Restart / Shut down.
//!
//! Radio calls are WinRT async, so they run on a short-lived worker thread and
//! report back with `WM_APP_CONTROL`; the UI thread never waits. Nothing here
//! runs unless the user opens the Controls tab or presses a button.

use std::sync::Mutex;
use windows::core::{w, Result, PCWSTR};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LUID, WPARAM};
use windows::Win32::Security::{AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD};
use windows::Win32::System::Shutdown::{ExitWindowsEx, LockWorkStation, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_APP, WM_SETTINGCHANGE};

/// Posted to the island window when [`radios`] has fresh states.
pub const WM_APP_CONTROL: u32 = WM_APP + 12;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    WiFi,
    Bluetooth,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Power {
    Lock,
    Sleep,
    Restart,
    ShutDown,
}

impl Power {
    /// Restart and Shut down need a second press within a few seconds.
    pub fn needs_confirm(self) -> bool {
        matches!(self, Power::Restart | Power::ShutDown)
    }
}

/// On/off state per radio; `None` when the PC has no such radio.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Radios {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

static RADIOS: Mutex<Radios> = Mutex::new(Radios { wifi: None, bluetooth: None });

pub fn radios() -> Radios {
    RADIOS.lock().map(|r| *r).unwrap_or_default()
}

fn read(change: Option<(Kind, bool)>) -> Result<Radios> {
    let mut out = Radios::default();
    if Radio::RequestAccessAsync()?.join()? != RadioAccessStatus::Allowed {
        return Ok(out);
    }
    for radio in Radio::GetRadiosAsync()?.join()? {
        let kind = match radio.Kind()? {
            RadioKind::WiFi => Kind::WiFi,
            RadioKind::Bluetooth => Kind::Bluetooth,
            _ => continue,
        };
        if let Some((_, on)) = change.filter(|(k, _)| *k == kind) {
            let _ = radio.SetStateAsync(if on { RadioState::On } else { RadioState::Off })?.join();
        }
        let on = radio.State()? == RadioState::On;
        match kind {
            Kind::WiFi => out.wifi = Some(out.wifi.unwrap_or(false) || on),
            Kind::Bluetooth => out.bluetooth = Some(out.bluetooth.unwrap_or(false) || on),
        }
    }
    Ok(out)
}

/// Reads the radio states (after optionally switching one) on a worker thread.
pub fn refresh(target: HWND, change: Option<(Kind, bool)>) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-radios".into()).stack_size(256 * 1024).spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if let Ok(state) = read(change) {
            if let Ok(mut shared) = RADIOS.lock() {
                *shared = state;
            }
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_CONTROL, WPARAM(0), LPARAM(0));
        }
        CoUninitialize();
    });
}

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

/// `true` when Windows apps use the dark theme.
pub fn dark_theme() -> bool {
    let mut value = 1u32;
    let mut size = 4u32;
    unsafe {
        let _ = RegGetValueW(HKEY_CURRENT_USER, PERSONALIZE, w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut value as *mut u32 as *mut _), Some(&mut size));
    }
    value == 0
}

/// Switches both the app and the system (taskbar, Start) theme and tells running apps.
pub fn set_dark_theme(dark: bool) {
    let light = (!dark) as u32;
    unsafe {
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, PERSONALIZE, name, REG_DWORD.0, Some(&light as *const u32 as *const _), 4);
        }
        let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(w!("ImmersiveColorSet").as_ptr() as isize), SMTO_ABORTIFHUNG, 200, None);
    }
}

/// Carries out a power action. Restart and Shut down need the shutdown
/// privilege, which every interactive user can enable for their own process.
pub fn power(action: Power) {
    unsafe {
        match action {
            Power::Lock => {
                let _ = LockWorkStation();
            }
            Power::Sleep => {
                let _ = SetSuspendState(false, false, false);
            }
            Power::Restart | Power::ShutDown => {
                let mut token = HANDLE::default();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_ok() {
                    let mut luid = LUID::default();
                    if LookupPrivilegeValueW(None, w!("SeShutdownPrivilege"), &mut luid).is_ok() {
                        let privileges = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
                        let _ = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None);
                    }
                    let _ = CloseHandle(token);
                }
                let flags = if action == Power::Restart { EWX_REBOOT } else { EWX_SHUTDOWN | EWX_POWEROFF };
                let _ = ExitWindowsEx(flags, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_destructive_actions_need_a_second_press() {
        assert!(Power::Restart.needs_confirm() && Power::ShutDown.needs_confirm());
        assert!(!Power::Lock.needs_confirm() && !Power::Sleep.needs_confirm());
    }

    /// Reads the real radio states and theme; changes nothing.
    #[test]
    #[ignore = "reads the real radios"]
    fn live_read() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        println!("radios: {:?}", read(None));
        println!("dark theme: {}", dark_theme());
    }
}
