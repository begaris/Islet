//! Song lyrics from LRCLIB (lrclib.net: free, open, no account or key).
//!
//! Off unless `lyrics = on`. The only thing sent is the playing song's title,
//! artist and length. A worker thread fetches; `WM_APP_LYRICS` tells the UI.
//! Time-synced lyrics (LRC) drive the pill's lyric line and the music card's
//! lyrics view; plain lyrics only the card.

use crate::json::{self, Value};
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_LYRICS: u32 = WM_APP + 28;

/// The lyrics of one song: lines with their start time in seconds (synced),
/// or without times (plain: every time is negative).
#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub lines: Vec<(f32, String)>,
    pub synced: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Found(Lyrics),
    /// The song is known to have no words.
    Instrumental,
    Missing,
}

/// Parses LRC text: `[mm:ss.xx]` stamps (several per line allowed), an
/// optional `[offset:+/-ms]` tag; other tags are skipped. Sorted by time.
pub fn parse_lrc(text: &str) -> Vec<(f32, String)> {
    let mut offset = 0.0f32;
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(end) = body.find(']') else { break };
            let tag = &body[..end];
            rest = body[end + 1..].trim_start();
            if let Some(ms) = tag.strip_prefix("offset:") {
                offset = ms.trim().parse::<f32>().unwrap_or(0.0) / 1000.0;
            } else if let Some(t) = stamp(tag) {
                stamps.push(t);
            }
        }
        for t in stamps {
            out.push((t, rest.trim().to_string()));
        }
    }
    // A positive offset means the lyrics come earlier.
    for line in &mut out {
        line.0 = (line.0 - offset).max(0.0);
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// "01:23.45" (or "01:23", "1:23.4") -> seconds.
fn stamp(tag: &str) -> Option<f32> {
    let (m, s) = tag.split_once(':')?;
    let minutes: f32 = m.trim().parse().ok()?;
    let seconds: f32 = s.trim().parse().ok()?;
    (minutes >= 0.0 && (0.0..60.0).contains(&seconds)).then_some(minutes * 60.0 + seconds)
}

/// The line playing at `position` seconds (a moment early, so the words
/// appear as they are sung), if any has started.
pub fn line_at(lines: &[(f32, String)], position: f32) -> Option<usize> {
    const LEAD: f32 = 0.15;
    let n = lines.partition_point(|(t, _)| *t <= position + LEAD);
    n.checked_sub(1)
}

/// Seconds until the line after `index` starts (for the timer), if there is one.
pub fn next_change(lines: &[(f32, String)], index: Option<usize>, position: f32) -> Option<f32> {
    let next = index.map_or(0, |i| i + 1);
    lines.get(next).map(|(t, _)| (t - 0.15 - position).max(0.0))
}

/// Reads one LRCLIB record.
fn record(value: &Value) -> Answer {
    if matches!(value.get("instrumental"), Some(Value::Bool(true))) {
        return Answer::Instrumental;
    }
    let synced = parse_lrc(value.text("syncedLyrics"));
    if !synced.is_empty() {
        return Answer::Found(Lyrics { lines: synced, synced: true });
    }
    let plain: Vec<(f32, String)> = value.text("plainLyrics").lines().map(|l| (-1.0, l.trim().to_string())).collect();
    if plain.iter().any(|(_, l)| !l.is_empty()) {
        return Answer::Found(Lyrics { lines: plain, synced: false });
    }
    Answer::Missing
}

fn number(value: Option<&Value>) -> Option<f32> {
    match value? {
        Value::Num(text) => text.parse().ok(),
        _ => None,
    }
}

/// Picks from LRCLIB search results: the closest in length (within 4 s when
/// the length is known), synced lyrics before plain ones.
pub fn pick(results: &str, duration: f32) -> Answer {
    let Some(Value::Arr(items)) = json::parse(results) else { return Answer::Missing };
    let fits = |item: &&Value| duration <= 0.0 || number(item.get("duration")).is_none_or(|d| (d - duration).abs() <= 4.0);
    let mut best = Answer::Missing;
    for item in items.iter().filter(fits) {
        match record(item) {
            found @ Answer::Found(Lyrics { synced: true, .. }) => return found,
            found @ Answer::Found(_) if best == Answer::Missing => best = found,
            Answer::Instrumental if best == Answer::Missing => best = Answer::Instrumental,
            _ => {}
        }
    }
    best
}

fn fetch(title: &str, artist: &str, duration: f32) -> Result<Answer, String> {
    use crate::net::escape;
    let (t, a) = (escape(title), escape(artist));
    let mut url = format!("https://lrclib.net/api/get?track_name={t}&artist_name={a}");
    if duration > 0.0 {
        url.push_str(&format!("&duration={}", duration.round() as u32));
    }
    match crate::net::get(&url) {
        Ok(body) => {
            let answer = json::parse(&body).map_or(Answer::Missing, |v| record(&v));
            if answer != Answer::Missing {
                return Ok(answer);
            }
        }
        // Not found under exactly that name and length: search instead.
        Err(e) if e.contains("404") => {}
        Err(e) => return Err(e),
    }
    let body = crate::net::get(&format!("https://lrclib.net/api/search?track_name={t}&artist_name={a}"))?;
    Ok(pick(&body, duration))
}

/// A key for a song (title + artist), to match answers and cache them.
pub fn key(title: &str, artist: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in title.bytes().chain([0]).chain(artist.bytes()) {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

static LATEST: Mutex<Option<(u64, Result<Answer, String>)>> = Mutex::new(None);

pub fn take() -> Option<(u64, Result<Answer, String>)> {
    LATEST.lock().ok()?.take()
}

/// Looks the song up on a worker thread.
pub fn request(target: HWND, title: String, artist: String, duration: f32) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-lyrics".into()).stack_size(512 * 1024).spawn(move || {
        let answer = fetch(&title, &artist, duration);
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some((key(&title, &artist), answer));
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_LYRICS, WPARAM(0), LPARAM(0));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made-up words, so no real song is copied into the tests.
    const LRC: &str = "[ar:Test]\n[offset:+500]\n[00:01.50]first line\n[00:04.00][00:20.00]comes back\n[00:07.25]\n[00:10.00] third ";

    #[test]
    fn parses_lrc() {
        let lines = parse_lrc(LRC);
        let times: Vec<f32> = lines.iter().map(|l| l.0).collect();
        assert_eq!(times, vec![1.0, 3.5, 6.75, 9.5, 19.5]);
        assert_eq!((lines[1].1.as_str(), lines[2].1.as_str(), lines[3].1.as_str(), lines[4].1.as_str()), ("comes back", "", "third", "comes back"));
        assert!(parse_lrc("no stamps here\n[xx:yy]bad").is_empty());
        assert_eq!(stamp("1:02.5"), Some(62.5));
    }

    #[test]
    fn finds_lines_and_changes() {
        let lines = parse_lrc(LRC);
        assert_eq!((line_at(&lines, 0.5), line_at(&lines, 0.9), line_at(&lines, 5.0), line_at(&lines, 100.0)), (None, Some(0), Some(1), Some(4)));
        // Next change from before the first line, mid-way, and after the last.
        assert!((next_change(&lines, None, 0.0).unwrap() - 0.85).abs() < 1e-4);
        assert!((next_change(&lines, Some(1), 5.0).unwrap() - 1.6).abs() < 1e-4);
        assert_eq!(next_change(&lines, Some(4), 30.0), None);
    }

    #[test]
    fn reads_answers() {
        let get = r#"{"id":1,"trackName":"x","instrumental":false,"plainLyrics":"a\nb","syncedLyrics":"[00:01.00]a\n[00:02.00]b"}"#;
        assert!(matches!(record(&json::parse(get).unwrap()), Answer::Found(Lyrics { synced: true, .. })));
        let plain = r#"{"instrumental":false,"plainLyrics":"a\nb","syncedLyrics":null}"#;
        assert!(matches!(record(&json::parse(plain).unwrap()), Answer::Found(Lyrics { synced: false, .. })));
        assert_eq!(record(&json::parse(r#"{"instrumental":true}"#).unwrap()), Answer::Instrumental);
        // Search: the wrong length is skipped, synced wins over plain.
        let results = r#"[{"duration":300,"syncedLyrics":"[00:01.00]long"},{"duration":201,"plainLyrics":"p"},{"duration":199,"syncedLyrics":"[00:01.00]right"}]"#;
        match pick(results, 200.0) {
            Answer::Found(l) => assert_eq!(l.lines[0].1, "right"),
            other => panic!("{other:?}"),
        }
        assert_eq!(pick("[]", 200.0), Answer::Missing);
        assert_ne!(key("a", "bc"), key("ab", "c"));
    }

    /// Asks LRCLIB for a well-known song and checks the shape of the answer
    /// (prints counts only, never the words).
    #[test]
    #[ignore = "uses the network"]
    fn live_lookup() {
        let answer = fetch("Chop Suey!", "System Of A Down", 210.0).unwrap();
        match answer {
            Answer::Found(l) => {
                println!("{} lines, synced {}", l.lines.len(), l.synced);
                assert!(l.synced && l.lines.len() > 10);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fetch("zzqqxx no such song", "nobody at all", 0.0).unwrap(), Answer::Missing);
    }
}
