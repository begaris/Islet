//! Clipboard history (text): newest first, searchable, with pinned items.
//!
//! The history is kept on disk encrypted with DPAPI (only this Windows user
//! can read it). Content that a password manager marks as "do not record" is
//! never stored.

use crate::image::{self, Pixels};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, LocalFree, HANDLE, HGLOBAL, HLOCAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
/// Unpinned items kept; older ones fall off the end.
const LIMIT: usize = 50;
/// Longer clipboard text is cut to this many characters.
const MAX_CHARS: usize = 20_000;

/// Unpinned pictures kept.
const IMAGE_LIMIT: usize = 20;
/// Thumbnails are at most this big (twice the row's picture box, for sharp scaling).
const THUMB_W: u32 = 144;
const THUMB_H: u32 = 84;
/// Posted to the window when a copied picture has been stored (see `take_stored`).
pub const WM_APP_CLIP: u32 = WM_APP + 15;

/// A copied picture: the PNG lives on disk under its content hash.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    pub key: u64,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Empty for a picture.
    pub text: String,
    pub pinned: bool,
    pub image: Option<Image>,
}

#[derive(Default)]
pub struct History {
    pub items: Vec<Item>,
    /// `None`: memory only (tests).
    path: Option<PathBuf>,
    /// Where pictures are kept (`None`: nowhere, thumbnails only live in memory).
    dir: Option<PathBuf>,
}

/// The sealed PNG and the sealed thumbnail of picture `key`.
fn image_files(dir: &Path, key: u64) -> (PathBuf, PathBuf) {
    (dir.join(format!("{key:016x}.png.bin")), dir.join(format!("{key:016x}.thumb.bin")))
}

fn remove_image(dir: &Option<PathBuf>, item: &Item) {
    if let (Some(dir), Some(image)) = (dir, item.image) {
        let (full, thumb) = image_files(dir, image.key);
        let _ = std::fs::remove_file(full);
        let _ = std::fs::remove_file(thumb);
    }
}

impl History {
    /// Loads `%APPDATA%\Islet\clipboard.bin`; a missing or unreadable file gives an empty history.
    pub fn load() -> Self {
        let base = std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Islet"));
        let path = base.as_ref().map(|b| b.join("clipboard.bin"));
        let mut history = Self { items: Vec::new(), path, dir: base.map(|b| b.join("clips")) };
        if let Some(bytes) = history.path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|sealed| unseal(&sealed)) {
            history.items = decode(&String::from_utf8_lossy(&bytes));
        }
        history
    }

    pub fn memory_only() -> Self {
        Self::default()
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(sealed) = seal(encode(&self.items).as_bytes()) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, sealed);
        }
    }

    /// Adds copied text at the top. Text already in the history moves up
    /// instead of appearing twice (and keeps its pin).
    pub fn push(&mut self, text: &str) {
        let text: String = text.chars().take(MAX_CHARS).collect();
        if text.trim().is_empty() {
            return;
        }
        let pinned = match self.items.iter().position(|i| i.text == text) {
            Some(index) => self.items.remove(index).pinned,
            None => false,
        };
        self.items.insert(0, Item { text, pinned, image: None });
        self.trim();
        self.save();
    }

    /// Adds a copied picture at the top; the same picture again just moves up.
    pub fn push_image(&mut self, image: Image) {
        let pinned = match self.items.iter().position(|i| i.image.is_some_and(|x| x.key == image.key)) {
            Some(index) => self.items.remove(index).pinned,
            None => false,
        };
        self.items.insert(0, Item { text: String::new(), pinned, image: Some(image) });
        self.trim();
        self.save();
    }

    /// Drops the oldest unpinned items beyond the limits (texts and pictures
    /// are counted separately), together with the files of dropped pictures.
    fn trim(&mut self) {
        let (mut texts, mut images) = (0, 0);
        let dir = self.dir.clone();
        self.items.retain(|item| {
            if item.pinned {
                return true;
            }
            let keep = if item.image.is_some() {
                images += 1;
                images <= IMAGE_LIMIT
            } else {
                texts += 1;
                texts <= LIMIT
            };
            if !keep {
                remove_image(&dir, item);
            }
            keep
        });
    }

    /// Where the worker should store pictures.
    pub fn image_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    /// The stored thumbnail of a picture.
    pub fn thumb(&self, key: u64) -> Option<Pixels> {
        let bytes = unseal(&std::fs::read(image_files(self.dir.as_ref()?, key).1).ok()?)?;
        let (w, h) = (u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?), u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?));
        (bytes.len() == 8 + (w as usize * h as usize * 4)).then(|| Pixels { w, h, bgra: bytes[8..].to_vec() })
    }

    /// The full picture, decoded (for pasting). Needs COM on this thread.
    pub fn full(&self, key: u64) -> Option<Pixels> {
        image::decode_png(&unseal(&std::fs::read(image_files(self.dir.as_ref()?, key).0).ok()?)?)
    }

    pub fn toggle_pin(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            item.pinned = !item.pinned;
            self.save();
        }
    }

    /// Removes everything that is not pinned.
    pub fn clear(&mut self) {
        let dir = self.dir.clone();
        self.items.retain(|i| {
            if !i.pinned {
                remove_image(&dir, i);
            }
            i.pinned
        });
        self.save();
    }

    /// Indices of the items to list: pinned first, then newest first, filtered by `query`.
    /// Pictures match the word "image".
    pub fn matching(&self, query: &str) -> Vec<usize> {
        let q = query.trim().to_lowercase();
        let keep = |i: &Item| q.is_empty() || if i.image.is_some() { "image".contains(&q) } else { i.text.to_lowercase().contains(&q) };
        let pinned = self.items.iter().enumerate().filter(|(_, i)| i.pinned && keep(i));
        let rest = self.items.iter().enumerate().filter(|(_, i)| !i.pinned && keep(i));
        pinned.chain(rest).map(|(index, _)| index).collect()
    }
}

/// One item per line: `P` or `-`, a tab, then the text with `\`, newline and CR escaped.
/// A picture has `I` after the first character and "key width height" as its text.
fn encode(items: &[Item]) -> String {
    let mut out = String::new();
    for item in items {
        out.push(if item.pinned { 'P' } else { '-' });
        if let Some(image) = item.image {
            out.push_str(&format!("I\t{:016x} {} {}\n", image.key, image.w, image.h));
            continue;
        }
        out.push('\t');
        for c in item.text.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push('\n');
    }
    out
}

fn decode(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for line in text.lines() {
        let Some((flag, body)) = line.split_once('\t') else { continue };
        let pinned = flag.starts_with('P');
        if flag.ends_with('I') {
            let mut parts = body.split(' ');
            let key = parts.next().and_then(|k| u64::from_str_radix(k, 16).ok());
            let size = (parts.next().and_then(|v| v.parse().ok()), parts.next().and_then(|v| v.parse().ok()));
            if let (Some(key), (Some(w), Some(h))) = (key, size) {
                items.push(Item { text: String::new(), pinned, image: Some(Image { key, w, h }) });
            }
            continue;
        }
        let mut value = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                value.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => value.push('\n'),
                Some('r') => value.push('\r'),
                Some(other) => value.push(other),
                None => {}
            }
        }
        items.push(Item { text: value, pinned, image: None });
    }
    items
}

// ----------------------------------------------------------------- pictures --

static STORED: Mutex<Vec<(Image, Pixels)>> = Mutex::new(Vec::new());

/// Pictures the worker finished since the last call, each with its thumbnail.
pub fn take_stored() -> Vec<(Image, Pixels)> {
    std::mem::take(&mut *STORED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Stores a copied picture off the UI thread: PNG (encrypted like the text
/// history) plus a small thumbnail in `dir`, then `WM_APP_CLIP` to the window.
pub fn record_image(hwnd: HWND, dib: Vec<u8>, dir: Option<PathBuf>) {
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let Some(pixels) = image::from_dib(&dib) else { return };
        drop(dib);
        let found = Image { key: image::key(&pixels), w: pixels.w, h: pixels.h };
        let thumb = image::shrink(&pixels, THUMB_W, THUMB_H);
        if let Some(dir) = dir {
            let (full, small) = image_files(&dir, found.key);
            if !full.exists() {
                let _ = std::fs::create_dir_all(&dir);
                let png = unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    let png = image::encode_png(&pixels);
                    CoUninitialize();
                    png
                };
                let Some(sealed) = png.and_then(|png| seal(&png)) else { return };
                if std::fs::write(&full, sealed).is_err() {
                    return;
                }
                let mut raw = Vec::with_capacity(8 + thumb.bgra.len());
                raw.extend_from_slice(&thumb.w.to_le_bytes());
                raw.extend_from_slice(&thumb.h.to_le_bytes());
                raw.extend_from_slice(&thumb.bgra);
                if let Some(sealed) = seal(&raw) {
                    let _ = std::fs::write(&small, sealed);
                }
            }
        }
        STORED.lock().unwrap_or_else(|e| e.into_inner()).push((found, thumb));
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_CLIP, WPARAM(0), LPARAM(0));
        }
    });
}

/// Encrypts for the current Windows user (DPAPI).
pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: plain.len() as u32, pbData: plain.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, None, None, None, None, 0, &mut output).ok()?;
        let sealed = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(sealed)
    }
}

pub fn unseal(sealed: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: sealed.len() as u32, pbData: sealed.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output).ok()?;
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
        Some(plain)
    }
}

// --------------------------------------------------------------- clipboard --

/// Text currently on the clipboard, unless its owner asked not to be recorded
/// (password managers set these formats) or there is no text.
pub fn read_text(owner: HWND) -> Option<String> {
    unsafe {
        if excluded() || IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        OpenClipboard(Some(owner)).ok()?;
        let mut text = None;
        if allowed() {
            if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
                let global = HGLOBAL(handle.0);
                let p = GlobalLock(global) as *const u16;
                if !p.is_null() {
                    let max = GlobalSize(global) / 2;
                    let len = (0..max).take_while(|&i| *p.add(i) != 0).count();
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                    let _ = GlobalUnlock(global);
                }
            }
        }
        let _ = CloseClipboard();
        text
    }
}

/// The owner marked the clipboard as "do not record" (password managers do).
unsafe fn excluded() -> bool {
    unsafe { [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("Clipboard Viewer Ignore")].into_iter().any(|name| IsClipboardFormatAvailable(RegisterClipboardFormatW(name)).is_ok()) }
}

/// "CanIncludeInClipboardHistory" = 0 also means "do not record". The clipboard must be open.
unsafe fn allowed() -> bool {
    unsafe {
        match GetClipboardData(RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"))) {
            Ok(handle) => {
                let p = GlobalLock(HGLOBAL(handle.0)) as *const u32;
                let allowed = p.is_null() || *p != 0;
                let _ = GlobalUnlock(HGLOBAL(handle.0));
                allowed
            }
            Err(_) => true,
        }
    }
}

/// The picture currently on the clipboard as a packed DIB, under the same
/// "do not record" rules as text. Very large pictures are skipped.
pub fn read_dib(owner: HWND) -> Option<Vec<u8>> {
    unsafe {
        if excluded() || IsClipboardFormatAvailable(CF_DIB).is_err() {
            return None;
        }
        OpenClipboard(Some(owner)).ok()?;
        let mut dib = None;
        if allowed() {
            if let Ok(handle) = GetClipboardData(CF_DIB) {
                let global = HGLOBAL(handle.0);
                let size = GlobalSize(global);
                let p = GlobalLock(global) as *const u8;
                if !p.is_null() && size > 40 && size < 200_000_000 {
                    dib = Some(std::slice::from_raw_parts(p, size).to_vec());
                }
                let _ = GlobalUnlock(global);
            }
        }
        let _ = CloseClipboard();
        dib
    }
}

/// Puts `text` on the clipboard.
pub fn write_text(owner: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    put(owner, CF_UNICODETEXT, unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) })
}

/// Puts a picture (packed DIB, see `image::to_dib`) on the clipboard.
pub fn write_dib(owner: HWND, dib: &[u8]) -> bool {
    put(owner, CF_DIB, dib)
}

fn put(owner: HWND, format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
        let p = GlobalLock(global) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(global));
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(global);
        if OpenClipboard(Some(owner)).is_err() {
            let _ = GlobalFree(Some(global));
            return false;
        }
        let _ = EmptyClipboard();
        // On success the clipboard owns the memory.
        let stored = SetClipboardData(format, Some(HANDLE(global.0))).is_ok();
        if !stored {
            let _ = GlobalFree(Some(global));
        }
        let _ = CloseClipboard();
        stored
    }
}

/// Presses Ctrl+V in whichever window has the keyboard focus.
pub fn send_paste() {
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    let inputs = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_no_duplicates_pins_survive() {
        let mut h = History::memory_only();
        for text in ["one", "two", "three", "two"] {
            h.push(text);
        }
        assert_eq!(h.items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), ["two", "three", "one"]);
        h.toggle_pin(2); // pin "one"
        for n in 0..LIMIT + 5 {
            h.push(&format!("item {n}"));
        }
        assert_eq!(h.items.iter().filter(|i| !i.pinned).count(), LIMIT);
        assert!(h.items.iter().any(|i| i.text == "one" && i.pinned));
        // Pinned items are listed first; search filters both groups.
        assert_eq!(h.items[h.matching("")[0]].text, "one");
        assert_eq!(h.matching("item 5").len(), 6); // "item 5" and "item 50".."item 54"
        h.clear();
        assert_eq!(h.items, vec![Item { text: "one".into(), pinned: true, image: None }]);
        h.push("   ");
        assert_eq!(h.items.len(), 1);
    }

    /// The worker's whole path: DIB in, encrypted PNG and thumbnail on disk,
    /// both readable again, files gone when the picture leaves the history.
    #[test]
    fn pictures_are_stored_encrypted_and_read_back() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let dir = std::env::temp_dir().join(format!("islet-clips-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (w, h) = (400u32, 260u32);
        let pixels = Pixels { w, h, bgra: (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, 77, 255]).collect() };
        record_image(HWND::default(), image::to_dib(&pixels), Some(dir.clone()));
        let started = std::time::Instant::now();
        let (stored, thumb) = loop {
            if let Some(found) = take_stored().into_iter().find(|(i, _)| i.w == w) {
                break found;
            }
            assert!(started.elapsed().as_secs() < 10, "worker did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!((stored.w, stored.h, stored.key), (w, h, image::key(&pixels)));
        let mut history = History { items: Vec::new(), path: None, dir: Some(dir.clone()) };
        history.push_image(stored);
        assert_eq!(history.thumb(stored.key).unwrap(), thumb);
        assert!(thumb.w <= THUMB_W && thumb.h <= THUMB_H);
        assert_eq!(history.full(stored.key).unwrap(), pixels);
        let (full, small) = image_files(&dir, stored.key);
        assert!(!std::fs::read(&full).unwrap().windows(3).any(|b| b == b"PNG"), "stored file must be sealed");
        history.clear();
        assert!(!full.exists() && !small.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pictures_have_their_own_limit() {
        let mut h = History::memory_only();
        h.push("text stays");
        for key in 1..=IMAGE_LIMIT as u64 + 3 {
            h.push_image(Image { key, w: 10, h: 10 });
        }
        h.push_image(Image { key: 9, w: 10, h: 10 }); // copied again: moves up
        assert_eq!(h.items[0].image.unwrap().key, 9);
        assert_eq!(h.items.iter().filter(|i| i.image.is_some()).count(), IMAGE_LIMIT);
        assert!(h.items.iter().any(|i| i.text == "text stays"));
        assert_eq!(h.matching("ima").len(), IMAGE_LIMIT);
        assert_eq!(h.matching("stays").len(), 1);
    }

    #[test]
    fn storage_round_trip() {
        let items = vec![
            Item { text: "line 1\nline 2\r\n\ttab \\ slash".into(), pinned: true, image: None },
            Item { text: "caf\u{e9}".into(), pinned: false, image: None },
            Item { text: String::new(), pinned: true, image: Some(Image { key: 0xfeed_0000_0000_0001, w: 1920, h: 1080 }) },
        ];
        assert_eq!(decode(&encode(&items)), items);
        let sealed = seal(b"secret text").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(unseal(&sealed).unwrap(), b"secret text");
        assert_eq!(unseal(b"not a dpapi blob"), None);
    }
}
