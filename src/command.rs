//! The command bar (Alt+Space): launches Start Menu programs, calculates, and
//! understands timer / reminder phrases. Everything is local.
//!
//! The Start Menu index is built the first time the bar opens and dropped
//! again a while after it closes.

use crate::timer;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Open this shortcut or file.
    Launch(PathBuf),
    /// Copy this text (a calculation result).
    Copy(String),
    /// Run this phrase through the timer / reminder parser.
    Phrase(String),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub title: String,
    pub detail: String,
    pub action: Action,
}

#[derive(Clone)]
pub struct App {
    pub name: String,
    lower: String,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Index {
    pub apps: Vec<App>,
}

/// Posted to the window when the list of Store apps is ready (`Index::merge_store`).
pub const WM_APP_STORE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 16;
const STORE_PREFIX: &str = "shell:AppsFolder\\";

/// The Store apps found by the last `refresh_store`. Asking the shell takes
/// about half a second, so a worker does it and the result is kept (it is small).
static STORE: std::sync::Mutex<Vec<App>> = std::sync::Mutex::new(Vec::new());

/// Lists the Store apps on a worker thread, then posts `WM_APP_STORE`.
pub fn refresh_store(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let mut apps = Vec::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            store_apps(&mut apps);
            CoUninitialize();
        }
        *STORE.lock().unwrap_or_else(|e| e.into_inner()) = apps;
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(HWND(target as *mut _)), WM_APP_STORE, WPARAM(0), LPARAM(0));
        }
    });
}

impl Index {
    /// Scans the per-user and all-users Start Menu for shortcuts, and adds the
    /// Store apps already known (see `refresh_store`).
    pub fn build() -> Self {
        let mut apps = Vec::new();
        let roots = [
            std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("Microsoft\\Windows\\Start Menu\\Programs")),
            std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("Microsoft\\Windows\\Start Menu\\Programs")),
        ];
        for root in roots.into_iter().flatten() {
            scan(&root, 0, &mut apps);
        }
        let mut index = Self { apps };
        index.merge_store();
        index
    }

    /// Replaces the Store apps in the index with the latest list. A shortcut
    /// wins over a Store entry of the same name.
    pub fn merge_store(&mut self) {
        self.apps.retain(|app| !app.path.to_string_lossy().starts_with(STORE_PREFIX));
        self.apps.extend(STORE.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned());
        self.apps.sort_by(|a, b| a.lower.cmp(&b.lower));
        self.apps.dedup_by(|a, b| a.lower == b.lower);
    }

    /// Best matches for `query`: names starting with it, then words starting
    /// with it, then names containing it.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&App> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<(u8, &App)> = self
            .apps
            .iter()
            .filter_map(|app| {
                let rank = if app.lower.starts_with(&q) {
                    0
                } else if app.lower.split([' ', '-', '_', '.']).any(|word| word.starts_with(&q)) {
                    1
                } else if app.lower.contains(&q) {
                    2
                } else {
                    return None;
                };
                Some((rank, app))
            })
            .collect();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.lower.len().cmp(&b.1.lower.len())));
        ranked.into_iter().take(limit).map(|(_, app)| app).collect()
    }
}

fn scan(dir: &Path, depth: usize, out: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 4 {
                scan(&path, depth + 1, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk") || e.eq_ignore_ascii_case("url")) {
            let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            let lower = name.to_lowercase();
            // Uninstallers and help files are noise in a launcher.
            if lower.contains("uninstall") || lower.contains("readme") || lower.ends_with(" help") {
                continue;
            }
            out.push(App { name, lower, path });
        }
    }
}

/// Store apps (Notepad, Calculator, Settings, ...) have no shortcut file: they
/// are listed in the shell's Applications folder and start through
/// `shell:AppsFolder\<app id>`. Only packaged apps are taken from there (their
/// id contains `!`); everything else already has a Start Menu shortcut.
fn store_apps(out: &mut Vec<App>) {
    use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
    use windows::Win32::UI::Shell::{BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING};
    unsafe {
        let name_of = |item: &IShellItem, kind: SIGDN| {
            let raw = item.GetDisplayName(kind).ok()?;
            let text = raw.to_string().ok();
            CoTaskMemFree(Some(raw.0 as *const _));
            text
        };
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems) else { return };
        loop {
            let mut one = [None];
            let mut fetched = 0;
            if items.Next(&mut one, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = one[0].take() else { break };
            let (Some(name), Some(id)) = (name_of(&item, SIGDN_NORMALDISPLAY), name_of(&item, SIGDN_PARENTRELATIVEPARSING)) else { continue };
            // "Publisher.App_hash!Entry"; a `!` inside an ordinary path does not count.
            if id.contains('!') && !id.contains(['\\', ':']) && !name.is_empty() {
                out.push(App { lower: name.to_lowercase(), name, path: PathBuf::from(format!("{STORE_PREFIX}{id}")) });
            }
        }
    }
}

/// Currency conversion when it is switched on: the rates (`None` until they
/// arrive) and the user's own currency.
pub type Money<'a> = Option<(Option<&'a crate::currency::Rates>, &'a str)>;

/// The rows to show for what the user typed.
pub fn rows(index: &Index, query: &str, now: i64, money: Money) -> Vec<Row> {
    let query = query.trim();
    let mut rows = Vec::new();
    if query.is_empty() {
        return rows;
    }
    if let (Some((rates, home)), Some(phrase)) = (money, crate::currency::parse(query)) {
        for (title, detail, copy) in crate::currency::rows(&phrase, rates, home) {
            rows.push(Row { title, detail, action: Action::Copy(copy) });
        }
    }
    if let Some(value) = calculate(query) {
        let text = format_number(value);
        rows.push(Row { title: format!("= {text}"), detail: "Enter copies the result".into(), action: Action::Copy(text) });
    }
    match timer::parse(query, now) {
        Some(timer::Command::Timer { secs, label }) => {
            let span = if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
            rows.push(Row { title: format!("Start a timer for {span}"), detail: label, action: Action::Phrase(query.into()) });
        }
        Some(timer::Command::Reminder { at, daily, text }) => {
            let (h, m) = timer::clock_of(at);
            let day = if daily { "every day".to_string() } else { timer::day_label(at, now) };
            rows.push(Row { title: format!("Remind me {day} at {h:02}:{m:02}"), detail: text, action: Action::Phrase(query.into()) });
        }
        Some(timer::Command::CancelTimers) => rows.push(Row { title: "Cancel all timers".into(), detail: String::new(), action: Action::Phrase(query.into()) }),
        Some(timer::Command::CancelReminders) => rows.push(Row { title: "Cancel all reminders".into(), detail: String::new(), action: Action::Phrase(query.into()) }),
        None => {}
    }
    let room = 6usize.saturating_sub(rows.len());
    for app in index.search(query, room) {
        rows.push(Row { title: app.name.clone(), detail: "App".into(), action: Action::Launch(app.path.clone()) });
    }
    rows
}

// --------------------------------------------------------------- calculator --

/// Evaluates an arithmetic expression (`+ - * / % ^`, parentheses, decimals).
/// Returns `None` for anything that is not clearly a calculation.
pub fn calculate(text: &str) -> Option<f64> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).map(|c| if c == ',' { '.' } else { c }).collect();
    // A lone number or a word is not a calculation.
    if !cleaned.chars().any(|c| "+-*/%^(".contains(c)) || !cleaned.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if cleaned.chars().any(|c| !(c.is_ascii_digit() || "+-*/%^().".contains(c))) {
        return None;
    }
    let mut p = Calc { chars: cleaned.chars().collect(), pos: 0 };
    let value = p.sum()?;
    (p.pos == p.chars.len() && value.is_finite()).then_some(value)
}

struct Calc {
    chars: Vec<char>,
    pos: usize,
}

impl Calc {
    fn eat(&mut self, c: char) -> bool {
        let found = self.chars.get(self.pos) == Some(&c);
        if found {
            self.pos += 1;
        }
        found
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        loop {
            if self.eat('+') {
                value += self.product()?;
            } else if self.eat('-') {
                value -= self.product()?;
            } else {
                return Some(value);
            }
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        loop {
            if self.eat('*') {
                value *= self.power()?;
            } else if self.eat('/') {
                value /= self.power()?;
            } else if self.eat('%') {
                value %= self.power()?;
            } else {
                return Some(value);
            }
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        // Right-associative: 2^3^2 = 2^(3^2).
        if self.eat('^') {
            return Some(base.powf(self.power()?));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            return Some(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        if self.eat('(') {
            let value = self.sum()?;
            return self.eat(')').then_some(value);
        }
        let start = self.pos;
        while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect::<String>().parse().ok()
    }
}

/// Up to 10 significant decimals, without trailing zeros.
pub fn format_number(value: f64) -> String {
    let text = format!("{value:.10}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lists the Store apps found on this PC and how long the index takes.
    #[test]
    #[ignore]
    fn live_store_apps() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let started = std::time::Instant::now();
        let mut apps = Vec::new();
        store_apps(&mut apps);
        println!("{} store apps in {:?}", apps.len(), started.elapsed());
        for app in apps.iter().take(12) {
            println!("  {} -> {}", app.name, app.path.display());
        }
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|a| !a.path.to_string_lossy().contains(".exe")));
        // Merged into the index, without duplicates, and found by name.
        *STORE.lock().unwrap() = apps;
        let started = std::time::Instant::now();
        let index = Index::build();
        println!("whole index: {} entries in {:?}", index.apps.len(), started.elapsed());
        println!("  \"note\" finds: {:?}", index.search("note", 3).iter().map(|a| &a.name).collect::<Vec<_>>());
    }

    #[test]
    fn calculator() {
        assert_eq!(calculate("2+2"), Some(4.0));
        assert_eq!(calculate("(1 + 2) * 3 - 4 / 2"), Some(7.0));
        assert_eq!(calculate("2^3^2"), Some(512.0));
        assert_eq!(calculate("-3 * -(2 + 1)"), Some(9.0));
        assert_eq!(calculate("10 % 4"), Some(2.0));
        assert_eq!(format_number(calculate("0,1 + 0,2").unwrap()), "0.3");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
        assert_eq!(calculate("42"), None);
        assert_eq!(calculate("timer 10 min"), None);
        assert_eq!(calculate("2 +"), None);
        assert_eq!(calculate("1/0"), None);
    }

    #[test]
    fn search_ranking_and_rows() {
        let app = |name: &str| App { name: name.into(), lower: name.to_lowercase(), path: PathBuf::from(format!("{name}.lnk")) };
        let index = Index { apps: vec![app("Visual Studio Code"), app("Codex"), app("Notepad"), app("Barcode Studio")] };
        let names = |q: &str| index.search(q, 5).iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("cod"), ["Codex", "Visual Studio Code", "Barcode Studio"]);
        assert_eq!(names("note"), ["Notepad"]);
        assert!(names("zzz").is_empty() && names("").is_empty());

        let now = 150_000 * 86_400 + 14 * 3600;
        let r = rows(&index, "timer 10 min", now, None);
        assert_eq!((r[0].title.as_str(), &r[0].action), ("Start a timer for 10:00", &Action::Phrase("timer 10 min".into())));
        let r = rows(&index, "12*12", now, None);
        assert_eq!((r[0].title.as_str(), &r[0].action), ("= 144", &Action::Copy("144".into())));
        assert_eq!(rows(&index, "note", now, None)[0].action, Action::Launch(PathBuf::from("Notepad.lnk")));
        // Currency only when switched on.
        let rates = crate::currency::sample();
        assert!(rows(&index, "100 usd to eur", now, None).is_empty());
        let r = rows(&index, "100 usd to eur", now, Some((Some(&rates), "EUR")));
        assert_eq!((r[0].title.as_str(), &r[0].action), ("100.00 USD = 92.59 EUR", &Action::Copy("92.59".into())));
    }
}
