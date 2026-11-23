//! Now-playing through Windows SMTC (`GlobalSystemMediaTransportControlsSessionManager`).
//!
//! Everything WinRT happens on one worker thread that blocks on a channel
//! (0 % CPU when idle). Session events only push a message into that channel;
//! the worker then builds a [`Snapshot`], stores it and posts `WM_APP_MEDIA`.
//! Transport commands from the UI go through the same channel, so the UI
//! thread never waits on another app.

use crate::render::Rgba;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::*;
use windows::Media::Control::*;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the island window when a new [`Snapshot`] is ready.
pub const WM_APP_MEDIA: u32 = WM_APP + 5;
/// Album art is decoded to a square of this many pixels (86 dp at 200 %).
pub const ART_SIZE: u32 = 172;

pub enum Cmd {
    /// Something changed; `true` when title/artist/art may have changed.
    Refresh(bool),
    Toggle,
    Next,
    Previous,
    /// Jump to this many seconds into the track.
    Seek(f32),
    Shuffle(bool),
    /// 0 off, 1 this track, 2 the whole list.
    Repeat(u8),
    Quit,
}

/// What the player lets others change (players differ: browsers rarely allow
/// shuffle or repeat, music apps usually do), and the current setting.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Abilities {
    pub seek: bool,
    pub shuffle: Option<bool>,
    pub repeat: Option<u8>,
}

pub struct Snapshot {
    pub session: bool,
    pub playing: bool,
    pub title: Vec<u16>,
    pub artist: Vec<u16>,
    pub source: Vec<u16>,
    /// Seconds, as of `stamp`.
    pub position: f32,
    pub duration: f32,
    pub stamp: Instant,
    /// Changes whenever the artwork does; the pixels are sent only then.
    pub art_serial: u32,
    pub art: Option<Vec<u8>>,
    /// The cover's accent and second colour.
    pub accent: Option<(Rgba, Rgba)>,
    pub can: Abilities,
}

static LATEST: Mutex<Option<Snapshot>> = Mutex::new(None);

/// Takes the most recent snapshot (called by the UI thread on `WM_APP_MEDIA`).
pub fn take() -> Option<Snapshot> {
    LATEST.lock().ok()?.take()
}

pub struct Media {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Media {
    pub fn start(target: HWND) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let events = tx.clone();
        let target = target.0 as isize;
        let thread = std::thread::Builder::new()
            .name("islet-media".into())
            .spawn(move || unsafe {
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                    return;
                }
                let _ = worker(HWND(target as *mut _), events, rx);
                CoUninitialize();
            })
            .ok();
        Self { tx, thread }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Session = GlobalSystemMediaTransportControlsSession;

struct Bound {
    session: Session,
    tokens: (i64, i64, i64),
}

impl Bound {
    fn release(self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.tokens.0);
        let _ = self.session.RemovePlaybackInfoChanged(self.tokens.1);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.tokens.2);
    }
}

fn worker(target: HWND, events: Sender<Cmd>, rx: std::sync::mpsc::Receiver<Cmd>) -> Result<()> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()?;
    let tx = events.clone();
    let session_token = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        let _ = tx.send(Cmd::Refresh(true));
        Ok(())
    }))?;

    let mut bound: Option<Bound> = None;
    // Cached between refreshes so timeline ticks don't re-decode the artwork.
    let (mut title, mut artist, mut source) = (Vec::new(), Vec::new(), Vec::new());
    let (mut art_serial, mut art_key, mut accent) = (0u32, 0u64, None);
    let mut pending = Some(Cmd::Refresh(true));

    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            },
        };
        let mut props_changed = match cmd {
            Cmd::Quit => break,
            Cmd::Refresh(props) => props,
            Cmd::Toggle => {
                if let Some(b) = &bound {
                    let _ = b.session.TryTogglePlayPauseAsync();
                }
                continue;
            }
            Cmd::Next => {
                if let Some(b) = &bound {
                    let _ = b.session.TrySkipNextAsync();
                }
                continue;
            }
            Cmd::Previous => {
                if let Some(b) = &bound {
                    let _ = b.session.TrySkipPreviousAsync();
                }
                continue;
            }
            Cmd::Seek(seconds) => {
                if let Some(b) = &bound {
                    // Positions are in 100 ns ticks from the timeline's start.
                    let start = b.session.GetTimelineProperties().and_then(|t| t.StartTime()).map(|t| t.Duration).unwrap_or(0);
                    let _ = b.session.TryChangePlaybackPositionAsync(start + (seconds.max(0.0) as f64 * 1e7) as i64);
                }
                continue;
            }
            Cmd::Shuffle(on) => {
                if let Some(b) = &bound {
                    let _ = b.session.TryChangeShuffleActiveAsync(on);
                }
                continue;
            }
            Cmd::Repeat(mode) => {
                if let Some(b) = &bound {
                    let _ = b.session.TryChangeAutoRepeatModeAsync(windows::Media::MediaPlaybackAutoRepeatMode(mode.min(2) as i32));
                }
                continue;
            }
        };
        // Events arrive in bursts: fold everything already queued into this refresh.
        let mut quit = false;
        while let Ok(more) = rx.try_recv() {
            match more {
                Cmd::Refresh(p) => props_changed |= p,
                Cmd::Quit => quit = true,
                other => {
                    pending = Some(other);
                    break;
                }
            }
        }
        if quit {
            break;
        }

        // Follow the current session.
        let current = manager.GetCurrentSession().ok();
        let same = match (&bound, &current) {
            (Some(b), Some(c)) => b.session.as_raw() == c.as_raw(),
            (None, None) => true,
            _ => false,
        };
        if !same {
            if let Some(old) = bound.take() {
                old.release();
            }
            if let Some(session) = current {
                let handler = |props: bool| {
                    let tx = events.clone();
                    move || {
                        let _ = tx.send(Cmd::Refresh(props));
                    }
                };
                let (a, b, c) = (handler(true), handler(false), handler(false));
                let tokens = (
                    session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        a();
                        Ok(())
                    }))?,
                    session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                        b();
                        Ok(())
                    }))?,
                    session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        c();
                        Ok(())
                    }))?,
                );
                bound = Some(Bound { session, tokens });
            }
            props_changed = true;
        }

        let mut snapshot = Snapshot {
            session: false,
            playing: false,
            title: Vec::new(),
            artist: Vec::new(),
            source: Vec::new(),
            position: 0.0,
            duration: 0.0,
            stamp: Instant::now(),
            art_serial,
            art: None,
            accent,
            can: Abilities::default(),
        };
        if let Some(b) = &bound {
            let session = &b.session;
            if props_changed {
                if let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
                    title = wide(props.Title());
                    artist = wide(props.Artist());
                    source = source_name(session.SourceAppUserModelId().unwrap_or_default());
                    let key = hash(&title) ^ hash(&artist).rotate_left(17);
                    match props.Thumbnail().and_then(|t| decode(&t)) {
                        Ok(pixels) => {
                            // Players often publish the title first and the artwork a moment later.
                            let pixel_key = key ^ hash8(&pixels);
                            if pixel_key != art_key {
                                art_key = pixel_key;
                                art_serial += 1;
                                accent = Some(accents_of(&pixels));
                                snapshot.art = Some(pixels);
                            }
                        }
                        Err(_) => {
                            if art_key != key {
                                art_key = key;
                                art_serial += 1;
                                accent = None;
                            }
                        }
                    }
                }
            }
            snapshot.session = true;
            let info = session.GetPlaybackInfo().ok();
            snapshot.playing = info.as_ref().and_then(|i| i.PlaybackStatus().ok()).is_some_and(|s| s == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
            if let Some(info) = &info {
                let controls = info.Controls().ok();
                let enabled = |f: fn(&GlobalSystemMediaTransportControlsSessionPlaybackControls) -> Result<bool>| controls.as_ref().is_some_and(|c| f(c).unwrap_or(false));
                snapshot.can.seek = enabled(|c| c.IsPlaybackPositionEnabled());
                if enabled(|c| c.IsShuffleEnabled()) {
                    snapshot.can.shuffle = Some(info.IsShuffleActive().and_then(|v| v.Value()).unwrap_or(false));
                }
                if enabled(|c| c.IsRepeatEnabled()) {
                    snapshot.can.repeat = Some(info.AutoRepeatMode().and_then(|v| v.Value()).map_or(0, |m| m.0.clamp(0, 2) as u8));
                }
            }
            if let Ok(timeline) = session.GetTimelineProperties() {
                let seconds = |ticks: i64| ticks as f32 / 1e7;
                let start = timeline.StartTime().map(|t| t.Duration).unwrap_or(0);
                snapshot.duration = seconds(timeline.EndTime().map(|t| t.Duration).unwrap_or(0) - start);
                let mut position = seconds(timeline.Position().map(|t| t.Duration).unwrap_or(0) - start);
                // The reported position is as of `LastUpdatedTime`; bring it up to now.
                if snapshot.playing {
                    if let Ok(updated) = timeline.LastUpdatedTime() {
                        let age = seconds(now_ticks() - updated.UniversalTime);
                        if (0.0..36000.0).contains(&age) {
                            position += age;
                        }
                    }
                }
                snapshot.position = position.clamp(0.0, snapshot.duration.max(0.0));
            }
            snapshot.title = title.clone();
            snapshot.artist = artist.clone();
            snapshot.source = source.clone();
            snapshot.art_serial = art_serial;
            snapshot.accent = accent;
        }
        if let Ok(mut latest) = LATEST.lock() {
            // Don't lose artwork that the UI has not picked up yet.
            if snapshot.art.is_none() {
                if let Some(previous) = latest.take() {
                    if previous.art_serial == snapshot.art_serial {
                        snapshot.art = previous.art;
                    }
                }
            }
            *latest = Some(snapshot);
        }
        unsafe {
            let _ = PostMessageW(Some(target), WM_APP_MEDIA, WPARAM(0), LPARAM(0));
        }
    }

    if let Some(old) = bound.take() {
        old.release();
    }
    let _ = manager.RemoveCurrentSessionChanged(session_token);
    Ok(())
}

fn wide(s: Result<HSTRING>) -> Vec<u16> {
    s.map(|s| s.to_vec()).unwrap_or_default()
}

/// 100 ns ticks since 1601 (the unit of WinRT `DateTime`).
fn now_ticks() -> i64 {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (unix.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

fn hash(s: &[u16]) -> u64 {
    s.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &u| (h ^ u as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn hash8(bytes: &[u8]) -> u64 {
    // A sparse sample is enough to tell two covers apart.
    bytes.iter().step_by(97).fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify" -> "SPOTIFY", "chrome.exe" -> "CHROME".
fn source_name(id: HSTRING) -> Vec<u16> {
    let id = id.to_string_lossy();
    // Some desktop apps register an opaque id instead of a name.
    match id.as_str() {
        "308046B0AF4A39CB" | "6F193CCC56814779" => return "FIREFOX".encode_utf16().collect(),
        "MSEdge" => return "EDGE".encode_utf16().collect(),
        _ if id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit()) => return "MEDIA".encode_utf16().collect(),
        _ => {}
    }
    let mut name = id.rsplit('!').next().unwrap_or("");
    name = name.rsplit('\\').next().unwrap_or(name);
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".exe") {
        name = &name[..name.len() - 4];
    } else if let Some(dot) = name.rfind('.') {
        name = &name[dot + 1..];
    }
    name.to_uppercase().encode_utf16().take(28).collect()
}

/// Decodes the thumbnail to `ART_SIZE` x `ART_SIZE` premultiplied BGRA, centre-cropped.
fn decode(thumbnail: &windows::Storage::Streams::IRandomAccessStreamReference) -> Result<Vec<u8>> {
    let stream = thumbnail.OpenReadAsync()?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (w, h) = (decoder.PixelWidth()?.max(1), decoder.PixelHeight()?.max(1));
    let scale = ART_SIZE as f32 / w.min(h) as f32;
    let (sw, sh) = (((w as f32 * scale).round() as u32).max(ART_SIZE), ((h as f32 * scale).round() as u32).max(ART_SIZE));
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(sw)?;
    transform.SetScaledHeight(sh)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    transform.SetBounds(BitmapBounds { X: (sw - ART_SIZE) / 2, Y: (sh - ART_SIZE) / 2, Width: ART_SIZE, Height: ART_SIZE })?;
    let data = decoder.GetPixelDataTransformedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied, &transform, ExifOrientationMode::IgnoreExifOrientation, ColorManagementMode::DoNotColorManage)?.join()?;
    let pixels = data.DetachPixelData()?.to_vec();
    if pixels.len() != (ART_SIZE * ART_SIZE * 4) as usize {
        return Err(windows::core::Error::empty());
    }
    Ok(pixels)
}

// ----------------------------------------------------------- accent colour --

/// Dominant colour of the cover: median cut on a 32 x 32 downscale, then the
/// most vivid well-populated box, with saturation boosted and brightness kept
/// high enough to read against the black island.
#[cfg(test)]
fn accent_of(bgra: &[u8]) -> Rgba {
    accents_of(bgra).0
}

/// The accent plus a second cover colour for the music line: the best box
/// whose hue is clearly different, else the accent turned a little along the
/// colour wheel.
pub fn accents_of(bgra: &[u8]) -> (Rgba, Rgba) {
    const GRID: usize = 32;
    let size = ART_SIZE as usize;
    let mut colours = [[0u8; 3]; GRID * GRID];
    for (i, c) in colours.iter_mut().enumerate() {
        let (gx, gy) = (i % GRID, i / GRID);
        let (px, py) = ((gx * size + size / 2) / GRID, (gy * size + size / 2) / GRID);
        let o = (py * size + px) * 4;
        *c = [bgra[o + 2], bgra[o + 1], bgra[o]];
    }

    // Median cut into 8 boxes (index ranges over `colours`).
    let mut boxes = [(0usize, GRID * GRID); 8];
    let mut count = 1;
    while count < boxes.len() {
        // Split the box with the widest channel range.
        let mut best = (0usize, 0usize, 0u8);
        for (b, &(lo, hi)) in boxes[..count].iter().enumerate() {
            if hi - lo < 2 {
                continue;
            }
            for ch in 0..3 {
                let (mut min, mut max) = (255u8, 0u8);
                for c in &colours[lo..hi] {
                    min = min.min(c[ch]);
                    max = max.max(c[ch]);
                }
                if max - min >= best.2 {
                    best = (b, ch, max - min);
                }
            }
        }
        let (b, ch, range) = best;
        if range == 0 {
            break;
        }
        let (lo, hi) = boxes[b];
        colours[lo..hi].sort_unstable_by_key(|c| c[ch]);
        let mid = (lo + hi) / 2;
        boxes[b] = (lo, mid);
        boxes[count] = (mid, hi);
        count += 1;
    }

    let mut scored = [(0.0f32, [0.0f32; 3]); 8];
    let mut best = (0.0f32, [0.55f32, 0.55, 0.97]);
    for (slot, &(lo, hi)) in scored.iter_mut().zip(&boxes[..count]) {
        if hi == lo {
            continue;
        }
        let mut sum = [0u32; 3];
        for c in &colours[lo..hi] {
            for ch in 0..3 {
                sum[ch] += c[ch] as u32;
            }
        }
        let n = (hi - lo) as f32;
        let rgb = [sum[0] as f32 / n / 255.0, sum[1] as f32 / n / 255.0, sum[2] as f32 / n / 255.0];
        let (_, s, v) = to_hsv(rgb);
        // Prefer colourful, reasonably bright boxes; near-black and near-grey lose.
        let score = (0.15 + s) * (0.25 + v.min(0.9)) * (0.5 + n / (GRID * GRID) as f32);
        *slot = (score, rgb);
        if score > best.0 {
            best = (score, rgb);
        }
    }

    let vivid = |rgb: [f32; 3]| {
        let (h, s, v) = to_hsv(rgb);
        let s = if s < 0.08 { s } else { (s * 1.25 + 0.12).min(0.92) };
        let [r, g, b] = from_hsv(h, s, v.max(0.82));
        Rgba { r, g, b, a: 1.0 }
    };
    let first = vivid(best.1);
    let (h1, s1, _) = to_hsv(best.1);
    // Hue distance on the wheel (0..0.5).
    let apart = |h: f32| {
        let d = (h - h1).abs();
        d.min(1.0 - d)
    };
    let mut second = None;
    let mut second_score = 0.0;
    for &(score, rgb) in &scored {
        let (h, s, v) = to_hsv(rgb);
        if score > second_score && s > 0.18 && v > 0.2 && apart(h) > 0.07 {
            second_score = score;
            second = Some(rgb);
        }
    }
    let second = match second {
        // A clearly different colour that is not much weaker.
        Some(rgb) if second_score > best.0 * 0.35 && s1 >= 0.08 => vivid(rgb),
        _ if s1 < 0.08 => first,
        _ => {
            let (h, s, v) = to_hsv([first.r, first.g, first.b]);
            let [r, g, b] = from_hsv((h + 0.09).rem_euclid(1.0), (s * 0.85).min(0.92), v.max(0.9));
            Rgba { r, g, b, a: 1.0 }
        }
    };
    (first, second)
}

fn to_hsv([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d < 1e-5 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, if max <= 0.0 { 0.0 } else { d / max }, max)
}

fn from_hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let f = |n: f32| {
        let k = (n + h6).rem_euclid(6.0);
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

/// The ignored test reads this machine's real media session. It draws
/// nothing, so it is safe to run while a game is in the foreground:
/// `cargo test --release -- --ignored --nocapture`
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "reads the real SMTC session"]
    fn live_snapshot() {
        let media = Media::start(HWND::default());
        std::thread::sleep(std::time::Duration::from_millis(2500));
        match take() {
            Some(s) => {
                let text = |v: &[u16]| String::from_utf16_lossy(v);
                println!("session={} playing={} source={:?}", s.session, s.playing, text(&s.source));
                println!("title={:?} artist={:?}", text(&s.title), text(&s.artist));
                println!("position={:.1}s duration={:.1}s art_serial={} art_bytes={:?}", s.position, s.duration, s.art_serial, s.art.as_ref().map(|a| a.len()));
                if let Some((c, d)) = s.accent {
                    let hex = |c: Rgba| format!("#{:02x}{:02x}{:02x}", (c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8);
                    println!("accent={} second={}", hex(c), hex(d));
                }
            }
            None => println!("no snapshot was produced"),
        }
        drop(media);
    }

    #[test]
    fn accent_is_vivid_and_bright() {
        // A cover that is mostly dark grey with a saturated orange quarter.
        let size = ART_SIZE as usize;
        let mut px = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let o = (y * size + x) * 4;
                let (b, g, r) = if x < size / 2 && y < size / 2 { (20, 110, 240) } else { (40, 40, 44) };
                px[o..o + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        let c = accent_of(&px);
        assert!(c.r > 0.8 && c.r > c.g && c.g > c.b, "expected an orange accent, got {:.2} {:.2} {:.2}", c.r, c.g, c.b);
    }

    #[test]
    fn second_colour_comes_from_the_cover() {
        // Orange left half, blue right half: the second colour is the blue.
        let size = ART_SIZE as usize;
        let mut px = vec![0u8; size * size * 4];
        for y in 0..size {
            for x in 0..size {
                let o = (y * size + x) * 4;
                let (b, g, r) = if x < size / 2 { (20, 110, 240) } else { (230, 90, 30) };
                px[o..o + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        let (a, b) = accents_of(&px);
        assert!((a.r > a.b) != (b.r > b.b), "expected one orange and one blue colour, got {a:?} / {b:?}");

        // A single-colour cover: the second colour is the accent turned a little.
        let mut px = vec![0u8; size * size * 4];
        for p in px.chunks_mut(4) {
            p.copy_from_slice(&[20, 110, 240, 255]);
        }
        let (a, b) = accents_of(&px);
        assert!(a != b && b.r > 0.5, "expected a nearby second colour, got {a:?} / {b:?}");
    }

    #[test]
    fn source_names() {
        let name = |s: &str| String::from_utf16_lossy(&source_name(HSTRING::from(s)));
        assert_eq!(name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "SPOTIFY");
        assert_eq!(name("chrome.exe"), "CHROME");
        assert_eq!(name("Spotify.exe"), "SPOTIFY");
        assert_eq!(name("308046B0AF4A39CB"), "FIREFOX");
        assert_eq!(name("0123456789ABCDEF"), "MEDIA");
    }
}
