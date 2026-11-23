//! VPN toggle for the Controls tab.
//!
//! Works with the VPN profiles Windows itself keeps (Settings > Network > VPN,
//! the RAS phone book). Connecting goes through `rasdial.exe`, which uses the
//! credentials saved with the profile; disconnecting hangs the connection up.
//! WireGuard and vendor apps have no common switch and are only shown (the
//! watcher sees their adapter), not toggled.
//!
//! Everything runs on a short-lived worker thread and reports back with
//! `WM_APP_CONTROL`. (Written on a PC without a VPN profile to try it on.)

use crate::control::WM_APP_CONTROL;
use std::os::windows::process::CommandExt;
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::NetworkManagement::Rras::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Status {
    /// The profile the tile switches: the connected one, else the first.
    pub profile: Option<String>,
    pub on: bool,
    /// A connect or disconnect is under way.
    pub busy: bool,
    /// The last connect attempt did not come up (no saved password, server away).
    pub failed: bool,
}

static STATUS: Mutex<Status> = Mutex::new(Status { profile: None, on: false, busy: false, failed: false });

pub fn status() -> Status {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn text(buffer: &[u16]) -> String {
    String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())])
}

/// `true` unless the phone-book entry is known to be something else (dial-up, PPPoE).
unsafe fn is_vpn(entry: &RASENTRYNAMEW) -> bool {
    unsafe {
        let (book, name) = (PCWSTR(entry.szPhonebookPath.as_ptr()), PCWSTR(entry.szEntryName.as_ptr()));
        let mut bytes = 0u32;
        RasGetEntryPropertiesW(book, name, None, &mut bytes, None, None);
        let bytes_needed = (bytes as usize).max(size_of::<RASENTRYW>());
        // u64 storage keeps the structure aligned.
        let mut storage = vec![0u64; bytes_needed / 8 + 1];
        let properties = storage.as_mut_ptr() as *mut RASENTRYW;
        (*properties).dwSize = size_of::<RASENTRYW>() as u32;
        let mut bytes = (storage.len() * 8) as u32;
        RasGetEntryPropertiesW(book, name, Some(properties), &mut bytes, None, None) != 0 || (*properties).dwType == RASET_Vpn
    }
}

/// Names of the Windows VPN profiles.
fn profiles() -> Vec<String> {
    unsafe {
        let (mut bytes, mut count) = (0u32, 0u32);
        RasEnumEntriesW(PCWSTR::null(), PCWSTR::null(), None, &mut bytes, &mut count);
        if bytes == 0 {
            return Vec::new();
        }
        let mut entries: Vec<RASENTRYNAMEW> = (0..bytes as usize / size_of::<RASENTRYNAMEW>() + 1).map(|_| std::mem::zeroed()).collect();
        entries[0].dwSize = size_of::<RASENTRYNAMEW>() as u32;
        let mut bytes = (entries.len() * size_of::<RASENTRYNAMEW>()) as u32;
        if RasEnumEntriesW(PCWSTR::null(), PCWSTR::null(), Some(entries.as_mut_ptr()), &mut bytes, &mut count) != 0 {
            return Vec::new();
        }
        entries.iter().take(count as usize).filter(|entry| is_vpn(entry)).map(|entry| text(&entry.szEntryName)).collect()
    }
}

/// Live RAS connections: handle and profile name.
fn connections() -> Vec<(HRASCONN, String)> {
    unsafe {
        let (mut bytes, mut count) = (0u32, 0u32);
        RasEnumConnectionsW(None, &mut bytes, &mut count);
        if bytes == 0 {
            return Vec::new();
        }
        let mut live: Vec<RASCONNW> = (0..bytes as usize / size_of::<RASCONNW>() + 1).map(|_| std::mem::zeroed()).collect();
        live[0].dwSize = size_of::<RASCONNW>() as u32;
        let mut bytes = (live.len() * size_of::<RASCONNW>()) as u32;
        if RasEnumConnectionsW(Some(live.as_mut_ptr()), &mut bytes, &mut count) != 0 {
            return Vec::new();
        }
        live.iter().take(count as usize).map(|c| (c.hrasconn, text(&c.szEntryName))).collect()
    }
}

/// Which profile the tile stands for, and whether it is connected.
fn pick(profiles: &[String], live: &[String]) -> (Option<String>, bool) {
    match profiles.iter().find(|p| live.contains(p)) {
        Some(connected) => (Some(connected.clone()), true),
        None => (profiles.first().cloned(), false),
    }
}

fn dial(name: &str) -> bool {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(root) = std::env::var_os("SystemRoot") else { return false };
    let exe = std::path::PathBuf::from(root).join("System32\\rasdial.exe");
    let null = std::process::Stdio::null;
    std::process::Command::new(exe).arg(name).stdin(null()).stdout(null()).stderr(null()).creation_flags(CREATE_NO_WINDOW).status().is_ok_and(|s| s.success())
}

fn hang_up(connection: HRASCONN) {
    unsafe {
        if RasHangUpW(connection) != 0 {
            return;
        }
        // The handle stays valid until the link is really down (at most ~3 s here).
        for _ in 0..30 {
            let mut state: RASCONNSTATUSW = std::mem::zeroed();
            state.dwSize = size_of::<RASCONNSTATUSW>() as u32;
            if RasGetConnectStatusW(connection, &mut state) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// Reads the VPN state on a worker thread, first switching it when `toggle`.
pub fn refresh(target: HWND, toggle: bool) {
    if toggle {
        if let Ok(mut shared) = STATUS.lock() {
            if shared.busy {
                return;
            }
            shared.busy = true;
        }
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-vpn".into()).stack_size(256 * 1024).spawn(move || {
        let known = profiles();
        let mut failed = false;
        if toggle {
            let live = connections();
            let names: Vec<String> = live.iter().map(|(_, name)| name.clone()).collect();
            match pick(&known, &names) {
                (Some(name), true) => live.iter().filter(|(_, n)| *n == name).for_each(|(handle, _)| hang_up(*handle)),
                (Some(name), false) => failed = !dial(&name),
                _ => {}
            }
        }
        let names: Vec<String> = connections().into_iter().map(|(_, name)| name).collect();
        let (profile, on) = pick(&known, &names);
        if let Ok(mut shared) = STATUS.lock() {
            // A read that overlaps a switch must not clear its "busy".
            let busy = shared.busy && !toggle;
            let failed = if toggle { failed && !on } else { shared.failed && !on };
            *shared = Status { profile, on, busy, failed };
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_CONTROL, WPARAM(0), LPARAM(0));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connected_profile_wins_over_the_first() {
        let profiles = vec!["Home".to_string(), "Office".to_string()];
        assert_eq!(pick(&profiles, &[]), (Some("Home".into()), false));
        assert_eq!(pick(&profiles, &["Office".into()]), (Some("Office".into()), true));
        // A dial-up link that is not one of the VPN profiles does not count.
        assert_eq!(pick(&profiles, &["Modem".into()]), (Some("Home".into()), false));
        assert_eq!(pick(&[], &["Modem".into()]), (None, false));
    }

    /// Reads the real phone book and connections; changes nothing.
    #[test]
    #[ignore = "reads the real VPN profiles"]
    fn live_read() {
        println!("profiles: {:?}", profiles());
        println!("connections: {:?}", connections().into_iter().map(|(_, name)| name).collect::<Vec<_>>());
    }
}
