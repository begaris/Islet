//! Notes: Markdown files in `%APPDATA%\Islet\notes\` (sub-folders are the
//! note folders), and the small text editor that edits one of them.
//!
//! The editor keeps the Markdown source visible and styles it in place:
//! headings grow, `**bold**` and `*italic*` take their weight, markers dim.
//! File names follow the note's first line.

use crate::render::{Span, Style};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};

/// Longest note the editor accepts, in UTF-16 units.
const MAX_UNITS: usize = 200_000;
const NEWLINE: u16 = b'\n' as u16;

/// Serials are unique for the whole run, so a cached layout can never be
/// mistaken for another note's.
fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct Entry {
    pub path: PathBuf,
    pub title: String,
}

pub struct Notes {
    root: PathBuf,
    /// Folder names; the first one is "" (notes directly in the root).
    pub folders: Vec<String>,
    pub folder: usize,
    /// Notes of the current folder, most recently edited first.
    pub list: Vec<Entry>,
    /// Which of `list` is in the editor.
    pub open: Option<usize>,
    pub text: Vec<u16>,
    pub caret: usize,
    /// The other end of the selection (equal to `caret` when nothing is selected).
    pub anchor: usize,
    /// Changes with every edit of `text`, so layouts can be cached.
    pub serial: u64,
    dirty: bool,
    /// Delete for good instead of moving to the Recycle Bin (tests).
    permanent: bool,
    /// Earlier states of the open note (text and caret) for Ctrl+Z, and when
    /// the last one was taken: edits in quick succession undo as one step.
    history: Vec<(Vec<u16>, usize)>,
    checkpointed: Option<std::time::Instant>,
}

/// Undo steps kept per note.
const UNDO_STEPS: usize = 60;
/// Edits closer together than this belong to one undo step.
const UNDO_PAUSE_MS: u128 = 700;

impl Notes {
    /// The user's notes.
    pub fn load() -> Option<Self> {
        let root = PathBuf::from(std::env::var_os("APPDATA")?).join("Islet").join("notes");
        Some(Self::at(root, false))
    }

    /// Notes kept in `root` (created if missing), with the newest one open.
    pub fn at(root: PathBuf, permanent: bool) -> Self {
        let _ = std::fs::create_dir_all(&root);
        let mut notes = Self { root, folders: vec![String::new()], folder: 0, list: Vec::new(), open: None, text: Vec::new(), caret: 0, anchor: 0, serial: next_serial(), dirty: false, permanent, history: Vec::new(), checkpointed: None };
        notes.scan();
        notes.select(0);
        notes
    }

    fn dir(&self) -> PathBuf {
        match self.folders.get(self.folder) {
            Some(name) if !name.is_empty() => self.root.join(name),
            _ => self.root.clone(),
        }
    }

    pub fn folder_label(&self) -> &str {
        match self.folders.get(self.folder) {
            Some(name) if !name.is_empty() => name,
            _ => "Notes",
        }
    }

    /// Re-reads the folder names and the current folder's notes.
    fn scan(&mut self) {
        let current = self.folders.get(self.folder).cloned().unwrap_or_default();
        let mut folders: Vec<String> = std::fs::read_dir(&self.root)
            .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        folders.sort_by_key(|name| name.to_lowercase());
        folders.insert(0, String::new());
        self.folder = folders.iter().position(|f| *f == current).unwrap_or(0);
        self.folders = folders;

        let mut found: Vec<(std::time::SystemTime, Entry)> = Vec::new();
        for entry in std::fs::read_dir(self.dir()).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_file() || !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
                continue;
            }
            let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let title = title_of(&std::fs::read_to_string(&path).unwrap_or_default());
            found.push((modified, Entry { path, title }));
        }
        found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        self.list = found.into_iter().map(|(_, entry)| entry).collect();
        self.open = None;
    }

    fn load_text(&mut self, text: &str) {
        self.text = text.replace("\r\n", "\n").encode_utf16().take(MAX_UNITS).collect();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.serial = next_serial();
        self.dirty = false;
        self.history.clear();
        self.checkpointed = None;
    }

    /// Remembers the current text before an edit, unless one was remembered a moment ago.
    fn checkpoint(&mut self) {
        if self.checkpointed.is_some_and(|at| at.elapsed().as_millis() < UNDO_PAUSE_MS) {
            self.checkpointed = Some(std::time::Instant::now());
            return;
        }
        if self.history.len() >= UNDO_STEPS {
            self.history.remove(0);
        }
        self.history.push((self.text.clone(), self.caret));
        self.checkpointed = Some(std::time::Instant::now());
    }

    /// Ctrl+Z: back to the text before the last burst of edits.
    pub fn undo(&mut self) {
        if let Some((text, caret)) = self.history.pop() {
            self.text = text;
            self.caret = caret.min(self.text.len());
            self.anchor = self.caret;
            self.checkpointed = None;
            self.changed();
        }
    }

    /// Puts note `index` of the list into the editor (saving the one that was there).
    pub fn select(&mut self, index: usize) {
        self.save();
        match self.list.get(index) {
            Some(entry) => {
                let text = std::fs::read_to_string(&entry.path).unwrap_or_default();
                self.open = Some(index);
                self.load_text(&text);
            }
            None => {
                self.open = None;
                self.load_text("");
            }
        }
    }

    fn reopen(&mut self, path: &Path) {
        self.scan();
        let index = self.list.iter().position(|e| e.path == path).unwrap_or(0);
        self.select(index);
    }

    /// Starts an empty note in the current folder and opens it.
    pub fn create(&mut self) {
        self.save();
        let path = free_name(&self.dir(), "Untitled", ".md", None);
        if std::fs::write(&path, "").is_ok() {
            self.reopen(&path);
        }
    }

    /// Makes a new folder ("Folder", "Folder 2", ...) and switches to it.
    pub fn create_folder(&mut self) {
        self.save();
        let path = free_name(&self.root, "Folder", "", None);
        if std::fs::create_dir_all(&path).is_ok() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.scan();
            self.folder = self.folders.iter().position(|f| *f == name).unwrap_or(0);
            self.scan();
            self.select(0);
        }
    }

    pub fn next_folder(&mut self) {
        self.save();
        self.scan();
        self.folder = (self.folder + 1) % self.folders.len().max(1);
        self.scan();
        self.select(0);
    }

    /// Moves the open note to the Recycle Bin.
    pub fn delete(&mut self) {
        let Some(path) = self.open.and_then(|i| self.list.get(i)).map(|e| e.path.clone()) else { return };
        self.dirty = false;
        if self.permanent {
            let _ = std::fs::remove_file(&path);
        } else {
            recycle(&path);
        }
        self.scan();
        self.select(0);
    }

    /// Writes the open note if it changed, renaming the file after its first line.
    pub fn save(&mut self) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let Some(entry) = self.open.and_then(|i| self.list.get_mut(i)) else { return };
        let text = String::from_utf16_lossy(&self.text);
        let title = title_of(&text);
        let dir = entry.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let wanted = free_name(&dir, &file_stem(&title), ".md", Some(&entry.path));
        if wanted != entry.path && std::fs::rename(&entry.path, &wanted).is_ok() {
            entry.path = wanted;
        }
        entry.title = title;
        let _ = std::fs::write(&entry.path, text);
    }

    // ---------------------------------------------------------- editing --

    fn changed(&mut self) {
        self.serial = next_serial();
        self.dirty = true;
    }

    /// The selected range, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.caret != self.anchor).then(|| (self.caret.min(self.anchor), self.caret.max(self.anchor)))
    }

    pub fn selected_text(&self) -> String {
        self.selection().map(|(a, b)| String::from_utf16_lossy(&self.text[a..b])).unwrap_or_default()
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.drain(a..b);
        self.caret = a;
        self.anchor = a;
        self.changed();
        true
    }

    /// Types `units` at the caret, replacing the selection. Needs an open note.
    pub fn insert(&mut self, units: &[u16]) {
        if self.open.is_none() {
            return;
        }
        self.checkpoint();
        self.delete_selection();
        let room = MAX_UNITS.saturating_sub(self.text.len());
        let units = &units[..units.len().min(room)];
        self.text.splice(self.caret..self.caret, units.iter().copied().filter(|&u| u != b'\r' as u16));
        self.caret = (self.caret + units.iter().filter(|&&u| u != b'\r' as u16).count()).min(self.text.len());
        self.anchor = self.caret;
        self.changed();
    }

    /// The position one character before / after `pos` (surrogate pairs count as one).
    fn step(&self, pos: usize, forward: bool) -> usize {
        let low = |i: usize| self.text.get(i).is_some_and(|u| (0xDC00..0xE000).contains(u));
        if forward {
            let next = (pos + 1).min(self.text.len());
            if low(next) { (next + 1).min(self.text.len()) } else { next }
        } else {
            let previous = pos.saturating_sub(1);
            if low(previous) { previous.saturating_sub(1) } else { previous }
        }
    }

    pub fn backspace(&mut self) {
        self.checkpoint();
        if !self.delete_selection() && self.caret > 0 {
            let start = self.step(self.caret, false);
            self.text.drain(start..self.caret);
            self.caret = start;
            self.anchor = start;
            self.changed();
        }
    }

    pub fn delete_forward(&mut self) {
        self.checkpoint();
        if !self.delete_selection() && self.caret < self.text.len() {
            let end = self.step(self.caret, true);
            self.text.drain(self.caret..end);
            self.changed();
        }
    }

    /// Moves the caret; `extend` keeps the anchor so the selection grows (Shift).
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        self.caret = pos.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn left(&mut self, extend: bool) {
        match self.selection() {
            Some((start, _)) if !extend => self.move_to(start, false),
            _ => self.move_to(self.step(self.caret, false), extend),
        }
    }

    pub fn right(&mut self, extend: bool) {
        match self.selection() {
            Some((_, end)) if !extend => self.move_to(end, false),
            _ => self.move_to(self.step(self.caret, true), extend),
        }
    }

    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].iter().rposition(|&u| u == NEWLINE).map_or(0, |i| i + 1)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..].iter().position(|&u| u == NEWLINE).map_or(self.text.len(), |i| pos + i)
    }

    pub fn home(&mut self, extend: bool) {
        self.move_to(self.line_start(self.caret), extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.line_end(self.caret), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Enter: a new line that continues a list ("- ", "1. "); Enter on an
    /// empty list item ends the list instead.
    pub fn enter(&mut self) {
        if self.selection().is_some() {
            return self.insert(&[NEWLINE]);
        }
        let start = self.line_start(self.caret);
        let line = String::from_utf16_lossy(&self.text[start..self.caret]);
        let Some(marker) = list_marker(&line) else { return self.insert(&[NEWLINE]) };
        if line.len() == marker.len() {
            // Only the marker on this line: remove it.
            self.checkpoint();
            self.anchor = start;
            self.delete_selection();
            return;
        }
        let next = match marker.trim_end().strip_suffix('.').and_then(|n| n.parse::<u32>().ok()) {
            Some(number) => format!("\n{}. ", number + 1),
            None => format!("\n{marker}"),
        };
        self.insert(&next.encode_utf16().collect::<Vec<_>>());
    }

    /// Ctrl+B / Ctrl+I: puts `marker` around the selection, or an empty pair at the caret.
    pub fn wrap(&mut self, marker: &str) {
        let inner = self.selected_text();
        let m: Vec<u16> = marker.encode_utf16().collect();
        self.insert(&format!("{marker}{inner}{marker}").encode_utf16().collect::<Vec<_>>());
        if inner.is_empty() {
            self.caret -= m.len();
            self.anchor = self.caret;
        }
    }
}

/// "- ", "* " or "12. " at the start of a line.
fn list_marker(line: &str) -> Option<&str> {
    if line.starts_with("- ") || line.starts_with("* ") {
        return Some(&line[..2]);
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && digits < 4 && line[digits..].starts_with(". ")).then(|| &line[..digits + 2])
}

/// A note's title: its first non-empty line without heading marks.
pub fn title_of(text: &str) -> String {
    let line = text.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("Untitled");
    line.chars().take(60).collect()
}

/// A file name (without extension) for a title: no characters Windows rejects.
fn file_stem(title: &str) -> String {
    let clean: String = title.chars().filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c)).collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    if clean.is_empty() { "Untitled".into() } else { clean.to_string() }
}

/// `dir\stem.ext`, or `dir\stem 2.ext` and so on when that is taken by
/// something other than `own` (the file being renamed).
fn free_name(dir: &Path, stem: &str, ext: &str, own: Option<&Path>) -> PathBuf {
    let mut path = dir.join(format!("{stem}{ext}"));
    let mut n = 2;
    while path.exists() && !own.is_some_and(|o| same_file(o, &path)) {
        path = dir.join(format!("{stem} {n}{ext}"));
        n += 1;
    }
    path
}

/// Windows file names ignore case.
fn same_file(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

fn recycle(path: &Path) {
    // The list of files is double-NUL terminated.
    let from: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW { wFunc: FO_DELETE, pFrom: PCWSTR(from.as_ptr()), fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16, ..Default::default() };
    unsafe {
        SHFileOperationW(&mut op);
    }
}

// ------------------------------------------------------------------ styling --

/// Markdown styling for the editor: which ranges are headings, bold, italic
/// or dimmed markers. Positions are UTF-16 units.
pub fn style(text: &[u16]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut start = 0;
    for line in text.split(|&u| u == NEWLINE) {
        style_line(line, start as u32, &mut spans);
        start += line.len() + 1;
    }
    spans
}

fn style_line(line: &[u16], at: u32, out: &mut Vec<Span>) {
    let is = |i: usize, c: char| line.get(i) == Some(&(c as u16));
    let mut from = 0;
    let hashes = line.iter().take_while(|&&u| u == b'#' as u16).count();
    if (1..=3).contains(&hashes) && is(hashes, ' ') {
        let style = [Style::H1, Style::H2, Style::H3][hashes - 1];
        out.push(Span { start: at, len: line.len() as u32, style });
        out.push(Span { start: at, len: hashes as u32, style: Style::Dim });
        from = hashes + 1;
    } else if (is(0, '-') || is(0, '*')) && is(1, ' ') {
        out.push(Span { start: at, len: 1, style: Style::Dim });
        from = 2;
    }
    let mut i = from;
    while i < line.len() {
        let strong = (is(i, '*') && is(i + 1, '*')) || (is(i, '_') && is(i + 1, '_'));
        let mark = line[i];
        let opens = (mark == b'*' as u16 || mark == b'_' as u16) && (mark == b'*' as u16 || i == 0 || is(i - 1, ' '));
        let width = if strong { 2 } else { 1 };
        // The closing marker: the same characters again, after at least one
        // character that is not a space.
        let close = (opens && !is(i + width, ' ')).then(|| (i + width + 1..line.len() + 1 - width).find(|&j| line[j..j + width].iter().all(|&u| u == mark) && !is(j - 1, ' '))).flatten();
        match close {
            Some(j) => {
                let style = if strong { Style::Bold } else { Style::Italic };
                out.push(Span { start: at + (i + width) as u32, len: (j - i - width) as u32, style });
                out.push(Span { start: at + i as u32, len: width as u32, style: Style::Dim });
                out.push(Span { start: at + j as u32, len: width as u32, style: Style::Dim });
                i = j + width;
            }
            None => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn text(n: &Notes) -> String {
        String::from_utf16_lossy(&n.text)
    }

    #[test]
    fn files_follow_titles() {
        let root = std::env::temp_dir().join(format!("islet-notes-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut n = Notes::at(root.clone(), true);
        assert!(n.open.is_none() && n.list.is_empty());
        n.insert(&w("ignored without a note"));
        assert!(n.text.is_empty());

        n.create();
        n.insert(&w("# Shopping: list?\n- milk"));
        n.save();
        assert!(root.join("Shopping list.md").is_file());
        assert!(!root.join("Untitled.md").exists());
        assert_eq!(n.list[0].title, "Shopping: list?");

        // A second note with the same title gets its own file.
        n.create();
        n.insert(&w("Shopping list"));
        n.save();
        assert!(root.join("Shopping list 2.md").is_file());
        assert_eq!(n.list.len(), 2);

        // Folders are directories; each has its own list.
        n.create_folder();
        assert_eq!((n.folder_label(), n.list.len(), n.open), ("Folder", 0, None));
        n.create();
        n.insert(&w("inside"));
        n.next_folder(); // saves, back to the root
        assert_eq!((n.folder_label(), n.list.len()), ("Notes", 2));
        assert_eq!(std::fs::read_to_string(root.join("Folder").join("inside.md")).unwrap(), "inside");

        // Reloading opens the newest note; deleting removes its file.
        let mut n = Notes::at(root.clone(), true);
        assert_eq!(n.list.len(), 2);
        let open = n.list[n.open.unwrap()].path.clone();
        n.delete();
        assert!(!open.exists());
        assert_eq!(n.list.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn editing() {
        let root = std::env::temp_dir().join(format!("islet-notes-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut n = Notes::at(root.clone(), true);
        n.create();
        n.insert(&w("hello world"));
        n.home(false);
        n.right(false);
        n.right(true);
        n.right(true);
        assert_eq!(n.selected_text(), "el");
        n.insert(&w("EL"));
        assert_eq!(text(&n), "hELlo world");
        n.end(false);
        n.backspace();
        n.home(true);
        n.wrap("**");
        assert_eq!(text(&n), "**hELlo worl**");
        n.select_all();
        n.backspace();
        n.wrap("*");
        n.insert(&w("x"));
        assert_eq!(text(&n), "*x*");

        // Lists continue on Enter and end on an empty item.
        n.select_all();
        n.insert(&w("- one"));
        n.enter();
        n.insert(&w("two"));
        n.enter();
        assert_eq!(text(&n), "- one\n- two\n- ");
        n.enter();
        assert_eq!(text(&n), "- one\n- two\n");
        n.insert(&w("9. nine"));
        n.enter();
        assert_eq!(text(&n), "- one\n- two\n9. nine\n10. ");

        // Quick edits undo as one step; an edit after a pause is its own step.
        n.select_all();
        n.insert(&w("base"));
        n.checkpointed = None;
        n.insert(&w(" one"));
        n.insert(&w(" two"));
        n.checkpointed = None;
        n.backspace();
        assert_eq!(text(&n), "base one tw");
        n.undo();
        assert_eq!(text(&n), "base one two");
        n.undo();
        assert_eq!((text(&n).as_str(), n.caret), ("base", 4));

        // A surrogate pair is one step and one backspace.
        n.select_all();
        n.insert(&w("a\u{1F600}"));
        n.left(false);
        assert_eq!(n.caret, 1);
        n.end(false);
        n.backspace();
        n.delete_forward();
        assert_eq!(text(&n), "a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn markdown_styles() {
        let spans = style(&w("## Title\n- a **b** and *c*\nsnake_case_name _d_"));
        let has = |start: u32, len: u32, style: Style| spans.iter().any(|s| s.start == start && s.len == len && s.style == style);
        assert!(has(0, 8, Style::H2) && has(0, 2, Style::Dim));
        assert!(has(9, 1, Style::Dim)); // the list dash
        assert!(has(15, 1, Style::Bold) && has(13, 2, Style::Dim) && has(16, 2, Style::Dim));
        assert!(has(24, 1, Style::Italic));
        // Underscores inside a word are not emphasis; " _d_" is.
        assert_eq!(spans.iter().filter(|s| s.style == Style::Italic).count(), 2);
        assert!(has(44, 1, Style::Italic));
        assert_eq!(title_of("\n\n## Plan for *today*\nmore"), "Plan for *today*");
        assert_eq!(title_of("  \n"), "Untitled");
        assert_eq!(file_stem("a/b: c?."), "ab c");
    }
}
