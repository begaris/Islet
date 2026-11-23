//! Timers and reminders: a local phrase parser (no AI, no network) and a small
//! persisted store.
//!
//! Times are "wall seconds": seconds of local wall-clock time since 1601, so
//! "tomorrow 9:00" means 9:00 on the clock whatever happens to the UTC offset.

use std::path::PathBuf;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime};

const DAY: i64 = 86_400;

/// Current local wall-clock time in wall seconds.
pub fn now() -> i64 {
    unsafe { to_wall(&GetLocalTime()) }
}

unsafe fn to_wall(st: &SYSTEMTIME) -> i64 {
    let mut ft = FILETIME::default();
    unsafe {
        let _ = SystemTimeToFileTime(st, &mut ft);
    }
    ((((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) / 10_000_000) as i64
}

/// Hour and minute of a wall-seconds value.
pub fn clock_of(wall: i64) -> (u32, u32) {
    let secs = wall.rem_euclid(DAY);
    ((secs / 3600) as u32, (secs / 60 % 60) as u32)
}

/// "today", "tomorrow" or a date like "14 Nov" for a wall-seconds value.
pub fn day_label(wall: i64, now: i64) -> String {
    match wall.div_euclid(DAY) - now.div_euclid(DAY) {
        0 => "today".into(),
        1 => "tomorrow".into(),
        _ => unsafe {
            let ticks = wall as u64 * 10_000_000;
            let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
            let mut st = SYSTEMTIME::default();
            let _ = FileTimeToSystemTime(&ft, &mut st);
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {}", st.wDay, MONTHS[(st.wMonth.clamp(1, 12) - 1) as usize])
        },
    }
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Timer { secs: i64, label: String },
    Reminder { at: i64, daily: bool, text: String },
    CancelTimers,
    CancelReminders,
}

/// Parses phrases such as `timer 10 min`, `timer 1h 30m tea`,
/// `remind me tomorrow 9:00 call mom`, `remind me in 20 min check the oven`
/// and `pills every day at 11 pm`. `now` is the current wall-seconds value.
pub fn parse(input: &str, now: i64) -> Option<Command> {
    let lower = input.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    if matches!(lower.as_str(), "cancel timer" | "cancel timers" | "stop timer" | "stop timers") {
        return Some(Command::CancelTimers);
    }
    if matches!(lower.as_str(), "cancel reminders" | "clear reminders") {
        return Some(Command::CancelReminders);
    }

    // --- timer ---------------------------------------------------------------
    if words[0] == "timer" || words.last() == Some(&"timer") {
        let body: Vec<&str> = words.iter().copied().filter(|w| *w != "timer" && *w != "for").collect();
        let (secs, used) = duration(&body)?;
        if secs <= 0 {
            return None;
        }
        return Some(Command::Timer { secs, label: original_words(input, &body[used..]) });
    }

    // --- reminder ------------------------------------------------------------
    let is_reminder = lower.starts_with("remind") || words.contains(&"every") || words.contains(&"daily");
    if !is_reminder {
        return None;
    }
    let mut rest: Vec<&str> = words.clone();
    if rest[0] == "remind" || rest[0] == "reminder" {
        rest.remove(0);
        if rest.first() == Some(&"me") {
            rest.remove(0);
        }
    }
    let mut daily = false;
    let mut day_offset: Option<i64> = None;
    let mut time_of_day: Option<i64> = None;
    let mut relative: Option<i64> = None;
    let mut text: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let w = rest[i];
        let next = rest.get(i + 1).copied();
        if w == "daily" || (w == "every" && matches!(next, Some("day"))) || (w == "each" && matches!(next, Some("day"))) {
            daily = true;
            i += if w == "daily" { 1 } else { 2 };
        } else if w == "tomorrow" {
            day_offset = Some(1);
            i += 1;
        } else if w == "today" || w == "tonight" {
            day_offset = Some(0);
            i += 1;
        } else if w == "in" && relative.is_none() {
            match duration(&rest[i + 1..]) {
                Some((secs, used)) if used > 0 => {
                    relative = Some(secs);
                    i += 1 + used;
                }
                _ => {
                    text.push(w);
                    i += 1;
                }
            }
        } else if let Some((secs, used)) = clock(&rest[i..]).filter(|_| time_of_day.is_none()) {
            time_of_day = Some(secs);
            i += used;
        } else if (w == "at" && clock(&rest[i + 1..]).is_some()) || (w == "to" && text.is_empty()) {
            // Connective before the time or the message.
            i += 1;
        } else {
            text.push(w);
            i += 1;
        }
    }
    let at = if let Some(secs) = relative {
        now + secs
    } else {
        let time = time_of_day?;
        let today = now.div_euclid(DAY) * DAY;
        match day_offset {
            Some(days) => today + days * DAY + time,
            // No day given: the next time the clock shows that time.
            None if today + time > now => today + time,
            None => today + DAY + time,
        }
    };
    let text = original_words(input, &text);
    if text.is_empty() {
        return None;
    }
    Some(Command::Reminder { at, daily, text })
}

/// Recovers the user's capitalisation for the words kept as label / message.
fn original_words(input: &str, kept: &[&str]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut remaining = kept.iter().peekable();
    for word in input.split_whitespace() {
        if remaining.peek().is_some_and(|k| word.to_lowercase() == **k) {
            out.push(word);
            remaining.next();
        }
    }
    out.join(" ")
}

/// Leading duration such as `10 min`, `1h 30m`, `90s`, `5` (minutes).
/// Returns the seconds and how many words were consumed.
fn duration(words: &[&str]) -> Option<(i64, usize)> {
    let mut total = 0i64;
    let mut used = 0;
    while used < words.len() {
        let w = words[used];
        let digits = w.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            break;
        }
        let value: i64 = w[..digits].parse().ok()?;
        let (unit, extra) = if digits < w.len() {
            (&w[digits..], 0)
        } else {
            match words.get(used + 1).and_then(|u| unit_seconds(u)) {
                Some(_) => (words[used + 1], 1),
                None => ("min", 0),
            }
        };
        total += value * unit_seconds(unit)?;
        used += 1 + extra;
    }
    (used > 0).then_some((total, used))
}

fn unit_seconds(unit: &str) -> Option<i64> {
    match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(3600),
        _ => None,
    }
}

/// Leading clock time: `9:00`, `18:30`, `11pm`, `11 pm`, `9:15 am`.
/// Returns seconds since midnight and the words consumed.
fn clock(words: &[&str]) -> Option<(i64, usize)> {
    let first = *words.first()?;
    let (body, mut meridiem) = match first {
        w if w.ends_with("am") && w.len() > 2 => (&w[..w.len() - 2], Some(false)),
        w if w.ends_with("pm") && w.len() > 2 => (&w[..w.len() - 2], Some(true)),
        w => (w, None),
    };
    let (hours, minutes) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None => (body.parse::<i64>().ok()?, 0),
    };
    let mut used = 1;
    if meridiem.is_none() {
        match words.get(1).copied() {
            Some("am") => (meridiem, used) = (Some(false), 2),
            Some("pm") => (meridiem, used) = (Some(true), 2),
            // A bare number is only a time when it looks like one ("9:00").
            _ if !body.contains(':') => return None,
            _ => {}
        }
    }
    let hours = match meridiem {
        Some(true) if hours < 12 => hours + 12,
        Some(false) if hours == 12 => 0,
        _ => hours,
    };
    ((0..24).contains(&hours) && (0..60).contains(&minutes)).then_some((hours * 3600 + minutes * 60, used))
}

// -------------------------------------------------------------------- store --

#[derive(Clone, Debug, PartialEq)]
pub struct Countdown {
    pub end: i64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reminder {
    pub at: i64,
    pub daily: bool,
    pub text: String,
}

/// Something that has just become due.
pub enum Due {
    Timer(String),
    Reminder(String),
}

#[derive(Default)]
pub struct Store {
    pub timers: Vec<Countdown>,
    pub reminders: Vec<Reminder>,
    path: Option<PathBuf>,
}

impl Store {
    /// Loads `%APPDATA%\Islet\reminders.txt` (missing file = empty store).
    pub fn load() -> Self {
        let path = std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Islet").join("reminders.txt"));
        let mut store = Self { path, ..Default::default() };
        let Some(text) = store.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) else { return store };
        for line in text.lines() {
            let mut parts = line.splitn(4, '|');
            match (parts.next(), parts.next().and_then(|v| v.parse::<i64>().ok()), parts.next(), parts.next()) {
                (Some("T"), Some(end), Some(_), Some(label)) => store.timers.push(Countdown { end, label: label.into() }),
                (Some("R"), Some(at), Some(daily), Some(text)) => store.reminders.push(Reminder { at, daily: daily == "1", text: text.into() }),
                _ => {}
            }
        }
        store
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut out = String::new();
        for t in &self.timers {
            out.push_str(&format!("T|{}|0|{}\n", t.end, t.label.replace(['\n', '\r'], " ")));
        }
        for r in &self.reminders {
            out.push_str(&format!("R|{}|{}|{}\n", r.at, r.daily as u8, r.text.replace(['\n', '\r'], " ")));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, out);
    }

    pub fn apply(&mut self, command: Command, now: i64) {
        match command {
            Command::Timer { secs, label } => self.timers.push(Countdown { end: now + secs, label }),
            Command::Reminder { at, daily, text } => self.reminders.push(Reminder { at, daily, text }),
            Command::CancelTimers => self.timers.clear(),
            Command::CancelReminders => self.reminders.clear(),
        }
        self.save();
    }

    /// Removes and returns everything that is due at `now`. Daily reminders are
    /// moved to their next occurrence instead of being removed.
    pub fn take_due(&mut self, now: i64) -> Vec<Due> {
        let mut due = Vec::new();
        self.timers.retain(|t| {
            let fired = t.end <= now;
            if fired {
                due.push(Due::Timer(t.label.clone()));
            }
            !fired
        });
        for r in &mut self.reminders {
            if r.at <= now {
                due.push(Due::Reminder(r.text.clone()));
                if r.daily {
                    while r.at <= now {
                        r.at += DAY;
                    }
                }
            }
        }
        self.reminders.retain(|r| r.daily || r.at > now);
        if !due.is_empty() {
            self.save();
        }
        due
    }

    /// The running timer that ends first, as `(seconds left, label)`.
    pub fn next_timer(&self, now: i64) -> Option<(i64, &str)> {
        self.timers.iter().min_by_key(|t| t.end).map(|t| ((t.end - now).max(0), t.label.as_str()))
    }

    /// Seconds until the next timer or reminder is due.
    pub fn next_due_in(&self, now: i64) -> Option<i64> {
        self.timers.iter().map(|t| t.end).chain(self.reminders.iter().map(|r| r.at)).min().map(|at| (at - now).max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A Wednesday at 14:30:00 local time, expressed in wall seconds.
    const NOW: i64 = 150_000 * DAY + 14 * 3600 + 30 * 60;
    const MIDNIGHT: i64 = 150_000 * DAY;

    fn timer(secs: i64, label: &str) -> Option<Command> {
        Some(Command::Timer { secs, label: label.into() })
    }

    fn reminder(at: i64, daily: bool, text: &str) -> Option<Command> {
        Some(Command::Reminder { at, daily, text: text.into() })
    }

    #[test]
    fn timers() {
        assert_eq!(parse("timer 10 min", NOW), timer(600, ""));
        assert_eq!(parse("Timer 1h 30m Tea", NOW), timer(5400, "Tea"));
        assert_eq!(parse("timer 90s", NOW), timer(90, ""));
        assert_eq!(parse("timer 5", NOW), timer(300, ""));
        assert_eq!(parse("10 min timer", NOW), timer(600, ""));
        assert_eq!(parse("timer for 2 hours", NOW), timer(7200, ""));
        assert_eq!(parse("timer", NOW), None);
        assert_eq!(parse("cancel timers", NOW), Some(Command::CancelTimers));
    }

    #[test]
    fn reminders() {
        assert_eq!(parse("remind me tomorrow 9:00 call mom", NOW), reminder(MIDNIGHT + DAY + 9 * 3600, false, "call mom"));
        assert_eq!(parse("remind me at 18:30 take out trash", NOW), reminder(MIDNIGHT + 18 * 3600 + 1800, false, "take out trash"));
        // 9:00 has already passed today, so it means tomorrow.
        assert_eq!(parse("remind me 9:00 stand-up", NOW), reminder(MIDNIGHT + DAY + 9 * 3600, false, "stand-up"));
        assert_eq!(parse("remind me in 20 min check the oven", NOW), reminder(NOW + 1200, false, "check the oven"));
        assert_eq!(parse("pills every day at 11 pm", NOW), reminder(MIDNIGHT + 23 * 3600, true, "pills"));
        assert_eq!(parse("remind me daily 7am Run", NOW), reminder(MIDNIGHT + DAY + 7 * 3600, true, "Run"));
        assert_eq!(parse("remind me tomorrow", NOW), None);
        assert_eq!(parse("what is 2 + 2", NOW), None);
    }

    #[test]
    fn due_items() {
        let mut store = Store::default();
        store.apply(Command::Timer { secs: 60, label: "tea".into() }, NOW);
        store.apply(Command::Reminder { at: NOW + 30, daily: true, text: "pills".into() }, NOW);
        assert_eq!(store.next_due_in(NOW), Some(30));
        assert_eq!(store.next_timer(NOW + 10), Some((50, "tea")));
        assert!(store.take_due(NOW + 10).is_empty());
        let due = store.take_due(NOW + 61);
        assert_eq!(due.len(), 2);
        assert!(store.timers.is_empty());
        // The daily reminder moved to tomorrow.
        assert_eq!(store.reminders, vec![Reminder { at: NOW + 30 + DAY, daily: true, text: "pills".into() }]);
    }

    #[test]
    fn clock_formatting() {
        assert_eq!(clock_of(MIDNIGHT + 9 * 3600 + 5 * 60), (9, 5));
        assert_eq!(day_label(NOW + DAY, NOW), "tomorrow");
        assert_eq!(day_label(NOW, NOW), "today");
    }
}
