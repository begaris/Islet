//! Optional update check: off unless `update_check = on` and `update_url` is set.
//!
//! Once a day (and when switched on) it reads `update_url`, which can be a GitHub
//! "latest release" API address (`tag_name`, `html_url`) or any JSON with
//! `version` and `url`, and compares that version with this exe's. A newer one
//! is announced; nothing is downloaded or installed. Only the address is
//! contacted, and nothing about the user is sent.

use crate::json;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_UPDATE: u32 = WM_APP + 25;
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq, Debug)]
pub struct Release {
    pub version: String,
    /// Where to get it (the release page).
    pub page: String,
}

/// "v1.2.10" -> [1, 2, 10]; anything after the numbers ("-beta") is ignored.
fn numbers(version: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in version.trim().trim_start_matches(['v', 'V']).split('.') {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse() else { break };
        out.push(number);
        // "4-beta": the number counts, nothing after it does.
        if digits.len() < part.len() {
            break;
        }
    }
    out
}

/// `true` when `candidate` is a higher version than `current`.
pub fn newer(candidate: &str, current: &str) -> bool {
    let (mut a, mut b) = (numbers(candidate), numbers(current));
    if a.is_empty() {
        return false;
    }
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Reads a GitHub release or a `{"version", "url"}` document.
pub fn parse(body: &str) -> Option<Release> {
    let root = json::parse(body)?;
    let (version, page) = match root.text("tag_name") {
        "" => (root.text("version"), root.text("url")),
        tag => (tag, root.text("html_url")),
    };
    (!version.is_empty()).then(|| Release { version: version.trim_start_matches(['v', 'V']).to_string(), page: page.to_string() })
}

static FOUND: Mutex<Option<Release>> = Mutex::new(None);

pub fn take() -> Option<Release> {
    FOUND.lock().ok()?.take()
}

fn stamp() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Islet").join("update-checked.txt"))
}

fn today() -> String {
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", now.wYear, now.wMonth, now.wDay)
}

/// `true` when no check has run today.
pub fn due() -> bool {
    stamp().and_then(|path| std::fs::read_to_string(path).ok()).is_none_or(|day| day.trim() != today())
}

/// Checks `url` on a worker thread; a newer release is posted as `WM_APP_UPDATE`.
pub fn check(target: HWND, url: String) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-update".into()).stack_size(256 * 1024).spawn(move || {
        let Ok(body) = crate::net::get(&url) else { return };
        if let Some(path) = stamp() {
            let _ = std::fs::write(path, today());
        }
        if let Some(release) = parse(&body).filter(|r| newer(&r.version, CURRENT)) {
            if let Ok(mut found) = FOUND.lock() {
                *found = Some(release);
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("v0.1", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(!newer("latest", "0.1.0"));
        assert_eq!(numbers("v2.3.4-beta.1"), [2, 3, 4]);
    }

    #[test]
    fn release_documents() {
        let github = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/someone/islet/releases/tag/v0.3.0","assets":[]}"#;
        assert_eq!(parse(github), Some(Release { version: "0.3.0".into(), page: "https://github.com/someone/islet/releases/tag/v0.3.0".into() }));
        assert_eq!(parse(r#"{"version":"0.2.1","url":"https://example.com/islet"}"#).unwrap().version, "0.2.1");
        assert_eq!(parse(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse("<html>"), None);
    }

    /// Reads the latest release of a public GitHub project.
    #[test]
    #[ignore = "uses the network"]
    fn live_github() {
        let body = crate::net::get("https://api.github.com/repos/microsoft/windows-rs/releases/latest").unwrap();
        let release = parse(&body).unwrap();
        println!("windows-rs latest: {} at {}", release.version, release.page);
        assert!(!numbers(&release.version).is_empty() && release.page.starts_with("https://github.com/"));
    }
}
