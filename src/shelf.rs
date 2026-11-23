//! The file shelf: drop files onto the island, drag them out again later.
//!
//! The shelf only remembers paths (`%APPDATA%\Islet\shelf.txt`); the files
//! stay where they are until they are dragged somewhere.
//!
//! OLE calls the drop target on the UI thread from inside the message pump,
//! so it only stores what was dropped and posts a message to the window.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::core::{implement, Ref, Result, BOOL, HRESULT, HSTRING};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, LPARAM, POINTL, S_OK, WPARAM};
use windows::Win32::System::Com::{IBindCtx, IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{BHID_DataObject, DragQueryFileW, ILFree, SHCreateShellItemArrayFromIDLists, SHParseDisplayName, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the window: wparam `ENTERED` (files are being dragged over the
/// island) or `DROPPED` (collect them with `take_dropped`).
pub const WM_APP_SHELF: u32 = WM_APP + 13;
/// `MOVED` follows the pointer during the drag and `LEFT` ends it without a
/// drop. Except for `LEFT`, lparam is the pointer's screen position (`point_of`).
pub const ENTERED: usize = 1;
pub const DROPPED: usize = 2;
pub const MOVED: usize = 3;
pub const LEFT: usize = 4;

/// The screen position packed into a shelf message.
pub fn point_of(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32)
}

/// More than this is almost certainly a mistake (a whole folder's contents).
const LIMIT: usize = 200;

static INBOX: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The paths dropped since the last call.
pub fn take_dropped() -> Vec<PathBuf> {
    std::mem::take(&mut *INBOX.lock().unwrap_or_else(|e| e.into_inner()))
}

pub struct Item {
    pub path: PathBuf,
    pub name: String,
    /// `None` for a folder.
    pub size: Option<u64>,
}

impl Item {
    fn new(path: PathBuf) -> Self {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let size = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => None,
            Ok(meta) => Some(meta.len()),
            Err(_) => Some(0),
        };
        Self { path, name, size }
    }

    /// Upper-case extension for the row's badge (at most 4 characters).
    pub fn badge(&self) -> String {
        match (&self.size, self.path.extension()) {
            (None, _) => "DIR".into(),
            (_, Some(ext)) => ext.to_string_lossy().to_uppercase().chars().take(4).collect(),
            _ => "FILE".into(),
        }
    }

    /// "Folder", "12 KB", "3.4 MB", "1.2 GB".
    pub fn detail(&self) -> String {
        const KB: f64 = 1024.0;
        match self.size {
            None => "Folder".into(),
            Some(bytes) => {
                let v = bytes as f64;
                if v >= KB * KB * KB {
                    format!("{:.1} GB", v / (KB * KB * KB))
                } else if v >= KB * KB {
                    format!("{:.1} MB", v / (KB * KB))
                } else {
                    format!("{:.0} KB", (v / KB).ceil())
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Shelf {
    pub items: Vec<Item>,
    /// `None`: memory only (tests); nothing is checked against the disk either.
    file: Option<PathBuf>,
}

impl Shelf {
    /// Loads the saved shelf, without the files that no longer exist.
    pub fn load() -> Self {
        let file = std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Islet").join("shelf.txt"));
        let mut shelf = Self { items: Vec::new(), file };
        if let Some(text) = shelf.file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            shelf.items = text.lines().filter(|l| !l.trim().is_empty()).map(|l| Item::new(PathBuf::from(l))).collect();
            shelf.prune();
        }
        shelf
    }

    pub fn memory_only() -> Self {
        Self::default()
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self.items.iter().map(|i| format!("{}\n", i.path.display())).collect();
        let _ = std::fs::write(file, text);
    }

    /// Adds files at the top (newest first). Returns how many were new.
    pub fn add(&mut self, paths: Vec<PathBuf>) -> usize {
        let mut added = 0;
        for path in paths {
            if self.items.len() >= LIMIT || self.items.iter().any(|i| i.path == path) {
                continue;
            }
            self.items.insert(added, Item::new(path));
            added += 1;
        }
        if added > 0 {
            self.save();
        }
        added
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.save();
    }

    /// Forgets files that are gone (moved by a drag, deleted, drive unplugged).
    pub fn prune(&mut self) {
        if self.file.is_none() {
            return;
        }
        let before = self.items.len();
        self.items.retain(|i| i.path.exists());
        if self.items.len() != before {
            self.save();
        }
    }
}

// ----------------------------------------------------------------- drop in --

fn hdrop_format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn has_files(data: &IDataObject) -> bool {
    unsafe { data.QueryGetData(&hdrop_format()) == S_OK }
}

/// The file paths a drag carries.
pub fn paths_of(data: &IDataObject) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else { return paths };
        let drop = HDROP(medium.u.hGlobal.0);
        for i in 0..DragQueryFileW(drop, u32::MAX, None) {
            let mut buffer = vec![0u16; DragQueryFileW(drop, i, None) as usize + 1];
            let len = DragQueryFileW(drop, i, Some(&mut buffer)) as usize;
            if len > 0 {
                paths.push(PathBuf::from(String::from_utf16_lossy(&buffer[..len])));
            }
        }
        ReleaseStgMedium(&mut medium);
    }
    paths
}

/// The shelf never touches the source files, so it asks for "copy" (or "link"
/// when the source offers nothing else) and never for "move".
fn pick(files: bool, allowed: DROPEFFECT) -> DROPEFFECT {
    if files && allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else if files && allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else {
        DROPEFFECT_NONE
    }
}

#[implement(IDropTarget)]
struct Target {
    hwnd: isize,
    accept: Cell<bool>,
    /// Where the pointer was when `MOVED` was last posted.
    last: Cell<(i32, i32)>,
}

impl Target {
    fn post(&self, what: usize, at: &POINTL) {
        self.last.set((at.x, at.y));
        let packed = (at.x as i16 as u16 as isize) | ((at.y as i16 as u16 as isize) << 16);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP_SHELF, WPARAM(what), LPARAM(packed));
        }
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let files = data.as_ref().is_some_and(has_files);
        self.accept.set(files);
        if files {
            self.post(ENTERED, at);
        }
        unsafe { *effect = pick(files, *effect) };
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        // OLE calls this continuously; only real movement is worth a message.
        let (x, y) = self.last.get();
        if self.accept.get() && (at.x - x).abs() + (at.y - y).abs() >= 6 {
            self.post(MOVED, at);
        }
        unsafe { *effect = pick(self.accept.get(), *effect) };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        if self.accept.replace(false) {
            self.post(LEFT, &POINTL::default());
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, at: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let paths = data.as_ref().map(paths_of).unwrap_or_default();
        unsafe { *effect = pick(!paths.is_empty(), *effect) };
        if paths.is_empty() {
            self.post(LEFT, at);
        } else {
            INBOX.lock().unwrap_or_else(|e| e.into_inner()).extend(paths);
            self.post(DROPPED, at);
        }
        self.accept.set(false);
        Ok(())
    }
}

fn target(hwnd: HWND) -> IDropTarget {
    Target { hwnd: hwnd.0 as isize, accept: Cell::new(false), last: Cell::new((0, 0)) }.into()
}

/// Makes the window accept dragged files. Needs `OleInitialize` on this thread.
pub fn register(hwnd: HWND) -> bool {
    unsafe { RegisterDragDrop(hwnd, &target(hwnd)).is_ok() }
}

pub fn unregister(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

// ---------------------------------------------------------------- drag out --

/// The shell's own data object for these files: it carries `CF_HDROP`, shell
/// item ids and everything else Explorer and other apps expect.
pub fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    unsafe {
        let mut ids: Vec<*const ITEMIDLIST> = Vec::new();
        for path in paths {
            let mut id = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(path.as_os_str()), None::<&IBindCtx>, &mut id, 0, None).is_ok() && !id.is_null() {
                ids.push(id);
            }
        }
        let array = if ids.is_empty() { None } else { SHCreateShellItemArrayFromIDLists(&ids).ok() };
        for id in ids {
            ILFree(Some(id));
        }
        array?.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject).ok()
    }
}

#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Runs a drag of these files from the island (blocks until the button is
/// released). Returns `true` when they were dropped somewhere that took them.
pub fn drag(paths: &[PathBuf]) -> bool {
    let Some(data) = data_object(paths) else { return false };
    let source: IDropSource = Source.into();
    let mut effect = DROPEFFECT_NONE;
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_MOVE, &mut effect) };
    result == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_no_duplicates() {
        let mut shelf = Shelf::memory_only();
        assert_eq!(shelf.add(vec!["C:\\a\\one.txt".into(), "C:\\b\\two.PDF".into()]), 2);
        assert_eq!(shelf.add(vec!["C:\\a\\one.txt".into(), "C:\\c\\three".into()]), 1);
        assert_eq!(shelf.items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["three", "one.txt", "two.PDF"]);
        shelf.remove(0);
        shelf.remove(9);
        assert_eq!(shelf.items.len(), 2);
        shelf.clear();
        assert!(shelf.items.is_empty());
    }

    /// Both directions without a mouse: the shell data object built for a drag
    /// out is handed to the drop target as if it were dragged in.
    #[test]
    fn drag_data_reaches_the_drop_target() {
        unsafe {
            let _ = OleInitialize(None);
        }
        let dir = std::env::temp_dir().join("islet-shelf-test");
        let other = dir.join("sub");
        std::fs::create_dir_all(&other).unwrap();
        let files = [dir.join("first.txt"), other.join("second.md")];
        for file in &files {
            std::fs::write(file, "shelf").unwrap();
        }
        let data = data_object(&files).expect("shell data object");
        assert_eq!(paths_of(&data), files);

        let item = Item::new(files[0].clone());
        assert_eq!((item.badge().as_str(), item.detail().as_str()), ("TXT", "1 KB"));
        assert_eq!(Item::new(other.clone()).badge(), "DIR");

        let target = target(HWND::default());
        let mut effect = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
        unsafe {
            target.DragEnter(&data, MK_LBUTTON, POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            effect = DROPEFFECT_MOVE;
            target.DragOver(MK_LBUTTON, POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_NONE);
            effect = DROPEFFECT_COPY | DROPEFFECT_LINK;
            target.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
        }
        assert_eq!(take_dropped(), files);
        assert!(take_dropped().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
