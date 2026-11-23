//! One watcher thread for everything the OS can notify us about:
//! microphone / camera use (registry), downloads (Downloads folder and Steam
//! manifests) and VPN adapters (address changes).
//!
//! The thread blocks in `WaitForMultipleObjects` on change notifications, so
//! it costs nothing while nothing happens. Only while a download is being
//! tracked does it wake twice a second to measure size and speed. It observes
//! from outside: no hooks, nothing injected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Instant;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the island window when [`take`] has something new.
pub const WM_APP_WATCH: u32 = WM_APP + 6;

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Download {
    pub name: String,
    pub bytes: u64,
    /// 0 when the total size is not known (browser downloads).
    pub total: u64,
    /// Bytes per second.
    pub speed: f32,
}

#[derive(Clone, Debug)]
pub struct Finished {
    pub name: String,
    /// The file (or, for Steam, the library folder) to reveal.
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Default)]
pub struct Status {
    pub mic: bool,
    pub cam: bool,
    /// Friendly name of an active VPN adapter.
    pub vpn: Option<String>,
    pub download: Option<Download>,
    /// Downloads that completed since the last `take`.
    pub finished: Vec<Finished>,
}

static SHARED: Mutex<Status> = Mutex::new(Status { mic: false, cam: false, vpn: None, download: None, finished: Vec::new() });
static TARGET: AtomicIsize = AtomicIsize::new(0);

/// Current status; finished downloads are handed over exactly once.
pub fn take() -> Status {
    let Ok(mut shared) = SHARED.lock() else { return Status::default() };
    Status { mic: shared.mic, cam: shared.cam, vpn: shared.vpn.clone(), download: shared.download.clone(), finished: std::mem::take(&mut shared.finished) }
}

pub struct Watch {
    stop: isize,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    /// `downloads` overrides the folder to watch (tests); `None` = the user's Downloads folder.
    pub fn start(target: HWND, downloads: Option<PathBuf>) -> Option<Self> {
        TARGET.store(target.0 as isize, Relaxed);
        let stop = unsafe { CreateEventW(None, true, false, None).ok()? };
        let stop_raw = stop.0 as isize;
        let thread = std::thread::Builder::new()
            .name("islet-watch".into())
            .stack_size(256 * 1024)
            .spawn(move || unsafe { run(HANDLE(stop_raw as *mut _), downloads) })
            .ok();
        Some(Self { stop: stop_raw, thread })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let stop = HANDLE(self.stop as *mut _);
        unsafe {
            let _ = SetEvent(stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = CloseHandle(stop);
        }
    }
}

unsafe fn event() -> HANDLE {
    unsafe { CreateEventW(None, false, false, None).unwrap_or_default() }
}

// ------------------------------------------------------------- mic / camera --

struct Consent {
    key: HKEY,
    event: HANDLE,
}

impl Consent {
    unsafe fn open(device: PCWSTR) -> Option<Self> {
        unsafe {
            let mut root = HKEY::default();
            RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore"), None, KEY_READ, &mut root).ok().ok()?;
            let mut key = HKEY::default();
            let opened = RegOpenKeyExW(root, device, None, KEY_READ | KEY_NOTIFY, &mut key);
            let _ = RegCloseKey(root);
            opened.ok().ok()?;
            let watch = Self { key, event: event() };
            watch.arm();
            Some(watch)
        }
    }

    unsafe fn arm(&self) {
        unsafe {
            let _ = RegNotifyChangeKeyValue(self.key, true, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC, Some(self.event), true);
        }
    }

    /// An app is using the device when it has a start time but no stop time yet.
    unsafe fn in_use(&self) -> bool {
        unsafe {
            if any_active(self.key) {
                return true;
            }
            let mut desktop = HKEY::default();
            if RegOpenKeyExW(self.key, w!("NonPackaged"), None, KEY_READ, &mut desktop).is_ok() {
                let active = any_active(desktop);
                let _ = RegCloseKey(desktop);
                return active;
            }
            false
        }
    }
}

impl Drop for Consent {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.key);
            let _ = CloseHandle(self.event);
        }
    }
}

unsafe fn any_active(key: HKEY) -> bool {
    unsafe {
        let mut index = 0;
        loop {
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, index, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                return false;
            }
            index += 1;
            let read = |value: PCWSTR| {
                let mut data = 0u64;
                let mut size = 8u32;
                RegGetValueW(key, PCWSTR(name.as_ptr()), value, RRF_RT_REG_QWORD, None, Some(&mut data as *mut u64 as *mut _), Some(&mut size)).is_ok().then_some(data)
            };
            if read(w!("LastUsedTimeStop")) == Some(0) && read(w!("LastUsedTimeStart")).is_some_and(|start| start != 0) {
                return true;
            }
        }
    }
}

// ---------------------------------------------------------------------- VPN --

const VPN_HINTS: [&str; 16] =
    ["wireguard", "wintun", "tap-windows", "openvpn", "nordlynx", "nordvpn", "proton", "mullvad", "tailscale", "zerotier", "anyconnect", "fortinet", "expressvpn", "surfshark", "warp", "vpn"];

/// Friendly name of a connected VPN adapter, if any.
pub fn vpn_adapter() -> Option<String> {
    unsafe {
        let mut size = 32 * 1024u32;
        let mut buffer = vec![0u64; size as usize / 8];
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_UNICAST;
        if GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buffer.as_mut_ptr() as *mut _), &mut size) != 0 {
            return None;
        }
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() {
            let a = &*adapter;
            adapter = a.Next;
            if a.OperStatus.0 != 1 {
                continue; // not up
            }
            let text = |p: PWSTR| if p.is_null() { String::new() } else { String::from_utf16_lossy(p.as_wide()) };
            let (name, description) = (text(a.FriendlyName), text(a.Description));
            let haystack = format!("{name} {description}").to_lowercase();
            // PPP links are VPN or dial-up; other VPN clients are recognised by their adapter name.
            if a.IfType == 23 || VPN_HINTS.iter().any(|hint| haystack.contains(hint)) {
                return Some(name);
            }
        }
        None
    }
}

// ----------------------------------------------------------------- downloads --

const TEMP_EXTENSIONS: [&str; 5] = [".crdownload", ".part", ".partial", ".download", ".tmp"];

fn temp_extension(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    TEMP_EXTENSIONS.iter().copied().find(|ext| lower.ends_with(ext))
}

/// "movie.mkv.crdownload" -> "movie.mkv"; anonymous temp names -> "Download".
fn display_name(temp: &str) -> String {
    let base = temp_extension(temp).map_or(temp, |ext| &temp[..temp.len() - ext.len()]);
    if base.starts_with("Unconfirmed ") || !base.contains('.') {
        "Download".into()
    } else {
        base.into()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Browser,
    Steam,
}

struct Dir {
    kind: Kind,
    path: PathBuf,
    handle: HANDLE,
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u32>,
}

impl Dir {
    unsafe fn open(path: PathBuf, kind: Kind) -> Option<Self> {
        unsafe {
            let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
            let handle = CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
            .ok()?;
            let mut dir = Self { kind, path, handle, overlapped: Box::new(OVERLAPPED { hEvent: event(), ..Default::default() }), buffer: vec![0u32; 8192] };
            dir.arm();
            Some(dir)
        }
    }

    unsafe fn arm(&mut self) {
        unsafe {
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;
            let _ = ReadDirectoryChangesW(self.handle, self.buffer.as_mut_ptr() as *mut _, (self.buffer.len() * 4) as u32, false, filter, None, Some(&mut *self.overlapped), None);
        }
    }

    /// Collects the `(action, file name)` pairs of a completed notification.
    unsafe fn changes(&mut self) -> Vec<(FILE_ACTION, String)> {
        let mut out = Vec::new();
        unsafe {
            let mut bytes = 0u32;
            if GetOverlappedResult(self.handle, &*self.overlapped, &mut bytes, false).is_ok() && bytes > 0 {
                let base = self.buffer.as_ptr() as *const u8;
                let mut offset = 0usize;
                loop {
                    let info = &*(base.add(offset) as *const FILE_NOTIFY_INFORMATION);
                    let name = std::slice::from_raw_parts(info.FileName.as_ptr(), info.FileNameLength as usize / 2);
                    out.push((info.Action, String::from_utf16_lossy(name)));
                    if info.NextEntryOffset == 0 {
                        break;
                    }
                    offset += info.NextEntryOffset as usize;
                }
            }
            self.arm();
        }
        out
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.overlapped.hEvent);
        }
    }
}

struct Track {
    size: u64,
    measured: Instant,
    grew: Instant,
    started: Instant,
    speed: f32,
    total: u64,
    label: String,
}

impl Track {
    fn new(label: String) -> Self {
        let now = Instant::now();
        Self { size: 0, measured: now, grew: now, started: now, speed: 0.0, total: 0, label }
    }

    fn measure(&mut self, size: u64) {
        let now = Instant::now();
        let dt = now.duration_since(self.measured).as_secs_f32();
        if size > self.size {
            self.grew = now;
        }
        if dt >= 0.4 {
            let rate = size.saturating_sub(self.size) as f32 / dt;
            self.speed = if self.speed == 0.0 { rate } else { self.speed * 0.6 + rate * 0.4 };
            self.size = size;
            self.measured = now;
        } else if self.size == 0 {
            self.size = size;
        }
    }
}

#[derive(Default)]
struct Downloads {
    /// Keyed by temp file name (browser) or "steam:<manifest>" (Steam).
    tracks: HashMap<String, Track>,
    rename_from: Option<String>,
    finished: Vec<Finished>,
}

impl Downloads {
    fn browser_changes(&mut self, dir: &Path, changes: Vec<(FILE_ACTION, String)>) {
        for (action, name) in changes {
            match action {
                FILE_ACTION_ADDED | FILE_ACTION_MODIFIED => {
                    if temp_extension(&name).is_some() {
                        self.tracks.entry(name.clone()).or_insert_with(|| Track::new(display_name(&name)));
                    }
                }
                FILE_ACTION_RENAMED_OLD_NAME => self.rename_from = Some(name),
                FILE_ACTION_RENAMED_NEW_NAME => {
                    let Some(old) = self.rename_from.take() else { continue };
                    let Some(mut track) = self.tracks.remove(&old) else { continue };
                    if temp_extension(&name).is_some() {
                        // The browser learned the real file name mid-download.
                        track.label = display_name(&name);
                        self.tracks.insert(name, track);
                    } else {
                        let path = dir.join(&name);
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(track.size);
                        self.finished.push(Finished { name, path, size });
                    }
                }
                _ => {}
            }
        }
    }

    /// Measures every tracked browser file; handles files that disappeared.
    fn poll_browser(&mut self, dir: &Path) {
        let mut gone = Vec::new();
        for (name, track) in self.tracks.iter_mut().filter(|(name, _)| !name.starts_with("steam:")) {
            match std::fs::metadata(dir.join(name)) {
                Ok(meta) => {
                    track.measure(meta.len());
                    // Paused or abandoned: stop polling it (a later write re-adds it).
                    if track.grew.elapsed().as_secs() > 30 {
                        gone.push((name.clone(), false));
                    }
                }
                Err(_) => gone.push((name.clone(), true)),
            }
        }
        for (name, removed) in gone {
            self.tracks.remove(&name);
            // Firefox writes "file.ext.part" next to the final "file.ext" and deletes the part when done.
            if removed && name.to_lowercase().ends_with(".part") {
                let final_name = &name[..name.len() - 5];
                let path = dir.join(final_name);
                if let Ok(meta) = std::fs::metadata(&path) {
                    if meta.len() > 0 {
                        self.finished.push(Finished { name: final_name.into(), path, size: meta.len() });
                    }
                }
            }
        }
    }

    /// Steam writes download progress into `appmanifest_<id>.acf`.
    fn steam_changes(&mut self, dir: &Path, changes: Vec<(FILE_ACTION, String)>) {
        for (_, name) in changes {
            let lower = name.to_lowercase();
            if !(lower.starts_with("appmanifest_") && lower.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(dir.join(&name)) else { continue };
            let number = |key: &str| acf_value(&text, key).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let (total, done) = (number("BytesToDownload"), number("BytesDownloaded"));
            let key = format!("steam:{lower}");
            if total > 0 && done < total {
                let label = acf_value(&text, "name").unwrap_or("Steam download").to_string();
                let track = self.tracks.entry(key).or_insert_with(|| Track::new(label));
                track.total = total;
                track.measure(done);
            } else if let Some(track) = self.tracks.remove(&key) {
                if total > 0 {
                    self.finished.push(Finished { name: track.label, path: dir.to_path_buf(), size: total });
                }
            }
        }
    }

    /// The download to show: the largest one that is clearly a real download.
    fn current(&self) -> Option<Download> {
        self.tracks
            .values()
            .filter(|t| t.size >= 64 * 1024 && t.started.elapsed().as_millis() >= 700)
            .max_by_key(|t| t.size)
            .map(|t| Download { name: t.label.clone(), bytes: t.size, total: t.total, speed: t.speed })
    }
}

/// Value of `"key"  "value"` in Valve's text format.
fn acf_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let after = &text[text.find(&needle)? + needle.len()..];
    let start = after.find('"')? + 1;
    let end = after[start..].find('"')?;
    Some(&after[start..start + end])
}

unsafe fn downloads_folder() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let result = PathBuf::from(String::from_utf16_lossy(path.as_wide()));
        CoTaskMemFree(Some(path.0 as *const _));
        Some(result)
    }
}

/// `steamapps` folders of every Steam library on this PC.
fn steam_libraries() -> Vec<PathBuf> {
    let mut buffer = [0u16; 520];
    let mut size = (buffer.len() * 2) as u32;
    let found = unsafe { RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Valve\\Steam"), w!("SteamPath"), RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr() as *mut _), Some(&mut size)) };
    if found.is_err() {
        return Vec::new();
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
    let main = PathBuf::from(String::from_utf16_lossy(&buffer[..len]).replace('/', "\\")).join("steamapps");
    let mut libraries = vec![main.clone()];
    if let Ok(text) = std::fs::read_to_string(main.join("libraryfolders.vdf")) {
        for line in text.lines().filter(|l| l.contains("\"path\"")) {
            if let Some(path) = acf_value(line, "path") {
                let library = PathBuf::from(path.replace("\\\\", "\\")).join("steamapps");
                if !libraries.iter().any(|l| l.to_string_lossy().eq_ignore_ascii_case(&library.to_string_lossy())) {
                    libraries.push(library);
                }
            }
        }
    }
    libraries.retain(|l| l.is_dir());
    libraries
}

// --------------------------------------------------------------------- loop --

unsafe fn run(stop: HANDLE, downloads_override: Option<PathBuf>) {
    unsafe {
        let mic = Consent::open(w!("microphone"));
        let cam = Consent::open(w!("webcam"));

        let net_event = event();
        let net_overlapped = OVERLAPPED { hEvent: net_event, ..Default::default() };
        let mut net_handle = HANDLE::default();
        let _ = NotifyAddrChange(&mut net_handle, &net_overlapped);

        let mut dirs: Vec<Dir> = Vec::new();
        if let Some(dir) = downloads_override.clone().or_else(|| downloads_folder()).and_then(|p| Dir::open(p, Kind::Browser)) {
            dirs.push(dir);
        }
        if downloads_override.is_none() {
            dirs.extend(steam_libraries().into_iter().filter_map(|p| Dir::open(p, Kind::Steam)));
        }

        let mut downloads = Downloads::default();
        let mut vpn = vpn_adapter();
        let mut published: Option<(bool, bool, Option<String>, Option<Download>)> = None;

        loop {
            // Publish whatever changed.
            let state = (mic.as_ref().is_some_and(|m| m.in_use()), cam.as_ref().is_some_and(|c| c.in_use()), vpn.clone(), downloads.current());
            if published.as_ref() != Some(&state) || !downloads.finished.is_empty() {
                if let Ok(mut shared) = SHARED.lock() {
                    (shared.mic, shared.cam, shared.vpn, shared.download) = state.clone();
                    shared.finished.append(&mut downloads.finished);
                }
                published = Some(state);
                let _ = PostMessageW(Some(HWND(TARGET.load(Relaxed) as *mut _)), WM_APP_WATCH, WPARAM(0), LPARAM(0));
            }

            let mut handles = vec![stop, net_event];
            handles.extend(mic.iter().chain(cam.iter()).map(|c| c.event));
            let first_dir = handles.len();
            handles.extend(dirs.iter().map(|d| d.overlapped.hEvent));
            let timeout = if downloads.tracks.is_empty() { INFINITE } else { 500 };
            let wait = WaitForMultipleObjects(&handles, false, timeout);
            let index = wait.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if wait == WAIT_FAILED || index == 0 {
                break;
            }
            if wait != WAIT_TIMEOUT {
                if index == 1 {
                    let _ = NotifyAddrChange(&mut net_handle, &net_overlapped);
                    vpn = vpn_adapter();
                } else if index < first_dir {
                    // A consent key changed: re-arm both (cheap) and re-read on the next pass.
                    for consent in mic.iter().chain(cam.iter()) {
                        consent.arm();
                    }
                } else if let Some(dir) = dirs.get_mut(index - first_dir) {
                    let changes = dir.changes();
                    match dir.kind {
                        Kind::Browser => downloads.browser_changes(&dir.path, changes),
                        Kind::Steam => downloads.steam_changes(&dir.path, changes),
                    }
                }
            }
            if let Some(dir) = dirs.iter().find(|d| d.kind == Kind::Browser) {
                downloads.poll_browser(&dir.path);
            }
        }

        let _ = CancelIPChangeNotify(&net_overlapped);
        let _ = CloseHandle(net_event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(display_name("Holiday 2026.mkv.crdownload"), "Holiday 2026.mkv");
        assert_eq!(display_name("Unconfirmed 481516.crdownload"), "Download");
        assert_eq!(display_name("setup.exe.part"), "setup.exe");
        assert_eq!(temp_extension("notes.txt"), None);
        assert_eq!(temp_extension("A.CRDOWNLOAD"), Some(".crdownload"));
    }

    #[test]
    fn steam_manifest() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"570\"\n\t\"name\"\t\t\"Dota 2\"\n\t\"BytesToDownload\"\t\t\"1000\"\n\t\"BytesDownloaded\"\t\t\"250\"\n}";
        assert_eq!(acf_value(acf, "name"), Some("Dota 2"));
        assert_eq!(acf_value(acf, "BytesDownloaded"), Some("250"));
        assert_eq!(acf_value(acf, "missing"), None);
    }

    #[test]
    fn browser_download_lifecycle() {
        let dir = std::env::temp_dir().join(format!("islet-watch-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut d = Downloads::default();
        std::fs::write(dir.join("a.zip.crdownload"), vec![0u8; 100_000]).unwrap();
        d.browser_changes(&dir, vec![(FILE_ACTION_ADDED, "a.zip.crdownload".into())]);
        d.poll_browser(&dir);
        assert_eq!(d.tracks["a.zip.crdownload"].size, 100_000);
        std::fs::rename(dir.join("a.zip.crdownload"), dir.join("a.zip")).unwrap();
        d.browser_changes(&dir, vec![(FILE_ACTION_RENAMED_OLD_NAME, "a.zip.crdownload".into()), (FILE_ACTION_RENAMED_NEW_NAME, "a.zip".into())]);
        assert!(d.tracks.is_empty());
        assert_eq!((d.finished[0].name.as_str(), d.finished[0].size), ("a.zip", 100_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads this machine's real state; draws and changes nothing.
    #[test]
    #[ignore = "reads the real registry, adapters and folders"]
    fn live_status() {
        unsafe {
            println!("microphone in use: {:?}", Consent::open(w!("microphone")).map(|c| c.in_use()));
            println!("camera in use: {:?}", Consent::open(w!("webcam")).map(|c| c.in_use()));
            println!("vpn adapter: {:?}", vpn_adapter());
            println!("downloads folder: {:?}", downloads_folder());
            println!("steam libraries: {:?}", steam_libraries());
        }
        // Reading other apps' notifications needs the `userNotificationListener`
        // capability, i.e. package identity. This only reads the status (no prompt).
        use windows::UI::Notifications::Management::UserNotificationListener;
        match UserNotificationListener::Current().and_then(|l| l.GetAccessStatus()) {
            Ok(status) => println!("notification listener access status: {status:?}"),
            Err(e) => println!("notification listener unavailable: {e}"),
        }
        // Counts only: notification contents are not printed.
        let probe = || -> windows::core::Result<(u32, bool)> {
            use windows::UI::Notifications::NotificationKinds;
            let listener = UserNotificationListener::Current()?;
            let count = listener.GetNotificationsAsync(NotificationKinds::Toast)?.join()?.Size()?;
            let handler = windows::Foundation::TypedEventHandler::new(|_, _| Ok(()));
            let subscribed = match listener.NotificationChanged(&handler) {
                Ok(token) => {
                    let _ = listener.RemoveNotificationChanged(token);
                    true
                }
                Err(e) => {
                    println!("NotificationChanged subscription failed: {e}");
                    false
                }
            };
            Ok((count, subscribed))
        };
        match probe() {
            Ok((count, subscribed)) => println!("notifications readable: {count} in the action centre, change event subscribes: {subscribed}"),
            Err(e) => println!("reading notifications failed: {e}"),
        }
    }
}
