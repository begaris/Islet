//! The application: owns the window state, renderer, cursor hook and timers,
//! and runs the hover / auto-hide / game-suspend state machine.
//!
//! Idle behaviour: no frames are presented and no periodic timers run except
//! one wake per minute for the clock and the coalesced fullscreen watcher.

use crate::config::{self, Config};
use crate::debug::Stats;
use crate::demo::Demo;
use crate::gamemode;
use crate::input::{self, MouseHook, WM_APP_CURSOR};
use crate::audio::{self, Capture, WM_APP_EQ};
use crate::events::{self, Icon, Toast};
use crate::input::WM_APP_CAPS;
use crate::agent::{self, WM_APP_AGENT};
use crate::events::Permit;
use crate::clipboard::{self, History, WM_APP_CLIP};
use crate::command;
use crate::dock::{self, Dock};
use crate::image::{self, Pixels};
use crate::notes::{self, Notes};
use crate::shelf::{self, Shelf, WM_APP_SHELF};
use crate::snip;
use std::path::PathBuf;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;
use crate::control::{self, WM_APP_CONTROL};
use crate::hooks;
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, SetFocus, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_SHIFT};
use crate::notify::{self, WM_APP_NOTIFY};
use crate::timer::{self, Due};
use crate::watch::{self, Watch, WM_APP_WATCH};
use windows::Win32::Globalization::{GetLocaleInfoEx, LCIDToLocaleName, LOCALE_SISO639LANGNAME, LOCALE_SLOCALIZEDLANGUAGENAME};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};
use crate::comp::EQ_BARS;
use crate::media::{self, Cmd, Media, WM_APP_MEDIA};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use crate::island::{wave_neutral, N};
use crate::render::{Font, Gfx, Rgba};
use crate::state::*;
use crate::theme::{self, ThemeMode};
use crate::tray;
use crate::ui::{self, Damage, Hit};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::time::Instant;
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

pub const WM_APP_FOREGROUND: u32 = WM_APP + 3;
pub const WM_APP_TEST: u32 = WM_APP + 10;
/// Time per frame spent drawing the compositor wave layers while they are built.
const WAVE_BUILD_BUDGET: std::time::Duration = std::time::Duration::from_millis(5);

const T_CLOCK: usize = 1;
const T_COLLAPSE: usize = 2;
const T_AUTOHIDE: usize = 3;
const T_HOVER: usize = 4;
const T_WATCH: usize = 5;
const T_DEBUG: usize = 6;
const T_TRIM: usize = 7;
const T_RECHECK: usize = 8;
const T_RETRY: usize = 9;
/// Once a second while the panel is open: progress text and external volume changes.
const T_TICK: usize = 10;
/// Shortly after the panel opens: load the output-device list off the animation's critical path.
const T_OUTPUTS: usize = 11;
/// The current toast's time is up.
const T_TOAST: usize = 12;
/// Twice a second while a countdown is in the pill.
const T_SECOND: usize = 13;
/// One-shot for the next due timer or reminder. Stays armed during game-suspend.
const T_SCHEDULE: usize = 14;
/// Shortly after a Caps Lock press, once the toggle state has updated.
const T_CAPS: usize = 15;
/// Shortly after a toast popup appeared, once the notification is in the list.
const T_NOTIFY: usize = 16;
/// Drops agent sessions that stopped reporting (a closed terminal sends no "ended").
const T_AGENT: usize = 17;
/// A moment after a history row was put on the clipboard: press Ctrl+V.
const T_PASTE: usize = 18;
/// A minute after the command bar closed: drop the Start Menu index.
const T_INDEX: usize = 19;
/// A moment after the last edit of a note: write it to disk.
const T_NOTE: usize = 20;
/// Fetches the forecast (every half hour while the feature is on).
const T_WEATHER: usize = 21;
/// Posted to itself when a shelf row is dragged: the drag runs outside the
/// app borrow (see `main::wndproc`), because it is a modal loop.
pub const WM_APP_DRAG: u32 = WM_APP + 14;
/// Posted to itself to open the attachment picker outside the `App` borrow.
pub const WM_APP_PICK: u32 = WM_APP + 21;
const HOTKEY_COMMAND: i32 = 1;
const HOTKEY_VOICE: i32 = 2;
/// Types the dictated text once no modifier key is held.
const T_TYPE: usize = 23;
/// The next lyric line starts.
const T_LYRIC: usize = 22;
/// The character's next change (blink, end of a hop, thinking dots, falling asleep).
const T_MASCOT: usize = 24;
/// The update check (first a while after start, then every six hours if a day has passed).
const T_UPDATE: usize = 25;
/// While the user signs in to Claude: look every few seconds whether it worked.
const T_CLAUDE: usize = 26;
/// Hides the volume shown in the pill after a wheel turn.
const T_VOLUME: usize = 27;
const VOLUME_SHOWN_MS: u32 = 1400;

/// The step-by-step setup guide for the network features (Lithuanian).
const SETUP_GUIDE: &str = include_str!("../assets/setup-guide.html");
/// Away from the keyboard and mouse this long: the character dozes off.
const AWAY_SECS: u32 = 120;
const AGENT_STALE_SECS: u64 = 1800;
/// How long the Allow / Deny card waits. Shorter than the hook's own timeout
/// (`hooks::PERMISSION_TIMEOUT`), so the agent always gets an answer in time.
const PERMIT_SECONDS: f32 = 120.0;
/// For this long after an agent's hooks were heard, its Windows notifications
/// are treated as repeats of what the hooks already reported.
const AGENT_QUIET: std::time::Duration = std::time::Duration::from_secs(15 * 60);

const HOVER_MS: u32 = 90;
const COLLAPSE_MS: u32 = 400;
const AUTOHIDE_MS: u32 = 3500;

static EVENT_TARGET: AtomicIsize = AtomicIsize::new(0);

/// The settings window: a normal window with its own small renderer, which
/// exists only while the window is open.
struct SettingsWin {
    hwnd: HWND,
    gfx: Option<Gfx>,
    scale: f32,
}

/// Screen x of the island's centre for the position `at` (-1: the pill
/// touches the left edge, 0: middle, 1: touches the right edge) on monitor `m`.
fn chosen_centre(m: RECT, scale: f32, at: f32) -> f32 {
    let mid = (m.left + m.right) as f32 * 0.5;
    let width = (m.right - m.left) as f32;
    // Docked: as far as it goes, so any pill width touches the edge.
    if at.abs() >= 0.999 {
        return mid + at.signum() * width;
    }
    let reach = (width * 0.5 - PILL_W * 0.5 * scale).max(0.0);
    mid + at.clamp(-1.0, 1.0) * reach
}

/// The inverse of [`chosen_centre`].
fn centre_position(m: RECT, scale: f32, centre: f32) -> f32 {
    let mid = (m.left + m.right) as f32 * 0.5;
    let reach = ((m.right - m.left) as f32 * 0.5 - PILL_W * 0.5 * scale).max(1.0);
    ((centre - mid) / reach).clamp(-1.0, 1.0)
}

/// Posted when a shell toast surface appeared: time to read the notification list.
const WM_APP_TOASTWIN: u32 = WM_APP + 9;

/// WinEvent callback (out of context, on the UI thread) for windows being
/// shown or uncloaked anywhere. Almost everything is rejected by the first two
/// comparisons; only top-level shell surfaces go further.
unsafe extern "system" fn on_window_shown(_: HWINEVENTHOOK, _: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if object == 0 && child == 0 && notify::is_shell_surface(hwnd) {
        let target = HWND(EVENT_TARGET.load(Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(target), WM_APP_TOASTWIN, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    let hwnd = HWND(EVENT_TARGET.load(Relaxed) as *mut _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_FOREGROUND, WPARAM(0), LPARAM(0));
    }
}

/// Where the user is in a question card: request, question number, answers
/// given so far (question text, labels), ticks on the current question.
struct AskStep {
    id: u64,
    index: usize,
    answers: Vec<(String, Vec<String>)>,
    picked: Vec<bool>,
}

pub struct App {
    hwnd: HWND,
    cfg: Config,
    s: State,
    gfx: Option<Gfx>,
    hook: Option<MouseHook>,
    winevent: HWINEVENTHOOK,
    demo: Option<Demo>,
    stats: Option<Stats>,
    pts: [Vector2; N],
    origin: (i32, i32),
    monitor: RECT,
    size: (i32, i32),
    scale: f32,
    dirty: bool,
    fresh: bool,
    last: Instant,
    suspended: bool,
    paused: bool,
    interactive: bool,
    hover_armed: bool,
    collapse_armed: bool,
    labels_measured: bool,
    frames: usize,
    damage: Damage,
    test_hooks: bool,
    fake_cursor: Option<(f32, f32)>,
    /// Set by `invalidate`: draw a frame even if no animation is running.
    need_draw: bool,
    /// When the compositor took over the wave, at which phase and speed.
    comp: Option<(Instant, f32, f32)>,
    hide_wave: bool,
    /// The wave settings changed: the compositor layers must be rebuilt.
    wave_dirty: bool,
    steady_frames: u32,
    eq_levels: [f32; EQ_BARS],
    eq_place: (f32, f32, f32),
    eq_colour: Rgba,
    media: Option<Media>,
    capture: Option<Capture>,
    /// Current album art pixels, kept so they survive a renderer rebuild.
    art: Option<Vec<u8>>,
    art_serial: u32,
    drag_volume: bool,
    /// Ctrl + drag of the island: where the cursor and the window were (screen
    /// pixels) when it started.
    island_drag: Option<(i32, f32)>,
    /// Where the pointer puts the island's centre during a drag (screen px).
    drag_centre: Option<f32>,
    /// The island's centre on screen (px), gliding after a release.
    centre: crate::anim::Spring,
    gliding: bool,
    /// Tests: a pretend pointer x in screen pixels for the island drag.
    fake_screen_x: Option<i32>,
    /// Off-screen test mode: the window stays hidden, game-suspend is ignored
    /// and frames can be dumped into this directory.
    /// WinEvent hooks for the toast popup, and the notification ids already shown.
    toast_hooks: [HWINEVENTHOOK; 2],
    shell_host: Option<notify::ShellHost>,
    /// The agent pipe, and the sessions currently working (with their last sign of life).
    server: Option<agent::Server>,
    agent_seen: usize,
    /// Clipboard history (loaded on first use), the history indices of the
    /// visible rows, the scroll position, and "the next clipboard change is ours".
    history: Option<History>,
    clip_shown: Vec<usize>,
    clip_scroll: usize,
    clip_ignore: bool,
    /// Thumbnails of the clipboard pictures on screen (re-uploaded after a renderer rebuild).
    thumbs: Vec<(u64, Pixels)>,
    /// The file shelf (loaded on first use), its scroll position, a press on
    /// a row that may become a drag (`None` = "Drag all"), and the files of
    /// the drag in progress.
    shelf: Option<Shelf>,
    shelf_scroll: usize,
    /// Test hook 25 put made-up system load in place: the sampler stays off.
    stats_fake: bool,
    press: Option<(f32, f32, Option<usize>)>,
    dragging: Option<Vec<PathBuf>>,
    /// Notes (loaded while the tab is in use) and the title list's scroll position.
    notes: Option<Notes>,
    note_scroll: usize,
    /// The pointer is selecting text in the note editor.
    note_drag: bool,
    /// Home tab dock (loaded on first use) and its icons, kept for the whole
    /// run (a few kB each) so they survive the idle trim and renderer rebuilds.
    dock: Option<Dock>,
    dock_icons: Vec<(u64, Pixels)>,
    /// Folder to keep notes in instead of the user's (tests).
    pub notes_dir: Option<PathBuf>,
    /// Islet currently has the keyboard; the window that had it before.
    focused: bool,
    previous_window: HWND,
    /// Command bar: Start Menu index (while in use), typed text, result rows.
    index: Option<command::Index>,
    cmd_query: String,
    cmd_rows: Vec<command::Row>,
    hotkey_on: bool,
    hotkey_warned: bool,
    agents: std::collections::HashMap<String, Instant>,
    /// What each waiting permission request asks for (by request id), and the
    /// progress through a multi-question one on the card.
    asks: std::collections::HashMap<u64, agent::Ask>,
    /// When an agent's hooks last sent anything (see `AGENT_QUIET`).
    agent_heard: Option<Instant>,
    /// A tab moved with Ctrl + drag: which, and where on it the pointer took it.
    tab_grab: Option<(usize, f32)>,
    /// A Home tile or dock icon held with Ctrl + drag, and where on it the pointer took it.
    home_grab: Option<(HomeItem, f32, f32)>,
    /// An item of a `Group` held with Ctrl + drag, and where on it the pointer took it.
    grip: Option<(Grip, f32, f32)>,
    /// The held group's order when it was grabbed (sections trade places against it).
    grip_orig: Vec<usize>,
    ask_step: Option<AskStep>,
    seen: std::collections::HashSet<u32>,
    seen_ready: bool,
    watch: Option<Watch>,
    /// Folder to watch instead of the user's Downloads (tests).
    pub downloads: Option<std::path::PathBuf>,
    store: timer::Store,
    queue: events::Queue,
    /// The toast currently in the pill.
    toast: Option<Toast>,
    download_active: bool,
    /// Album accent to return to after a toast or download borrowed the line colour.
    /// The cover's accent and second colour.
    music_accent: (Rgba, Rgba),
    /// Test hook: the wave's music level is fixed (see `WM_APP_TEST` 2).
    level_pinned: bool,
    vpn: Option<String>,
    vpn_known: bool,
    /// The "rain on the way" heads-up was shown for the current spell.
    rain_warned: bool,
    /// AI chat: the conversation (loaded when the tab first opens), the API
    /// key, and what is typed into the input field.
    chat: Option<Vec<crate::chat::Message>>,
    chat_key: Option<String>,
    chat_input: String,
    /// The Claude Code session / Codex thread that holds this conversation.
    chat_session: Option<crate::cli::Session>,
    /// The settings window while it is open.
    settings_win: Option<SettingsWin>,
    /// The settings file this run reads and writes (a temporary copy under `--hidden`),
    /// and the pretend "start with Windows" switch of test runs.
    config_path: PathBuf,
    test_startup: bool,
    /// The character: eyes closed until (blink), lifted until (hop), happy until
    /// (an agent finished), the next blink, and a running bass level for beats.
    mascot_blink: Option<Instant>,
    /// The act the character is doing and since when.
    mascot_doing: Option<(crate::mascot::Act, Instant)>,
    mascot_next_idle: Instant,
    mascot_greeted: Instant,
    mascot_was_away: bool,
    mascot_sleep_since: Instant,
    /// Which way the next music bounce leans.
    mascot_side: f32,
    mascot_rng: u32,
    /// Tests: hold an act at one moment (for pictures).
    mascot_freeze: Option<(crate::mascot::Act, f32)>,
    /// Last update (for easing), and the eased look, lean and blush.
    mascot_last: Instant,
    mascot_look: (f32, f32),
    mascot_lean: f32,
    mascot_fond: f32,
    /// Stroking: last cursor x and direction over it, and when it turned.
    mascot_rub: (f32, f32),
    mascot_rubs: Vec<Instant>,
    /// Last click on it and how many in a row.
    mascot_clicks: (Instant, u8),
    /// The character is moving: the frame loop updates it every refresh.
    pet_live: bool,
    /// The Weather tab is on screen: every refresh draws its sky (and cards).
    sky_live: bool,
    /// Alternates each refresh: the resting sky is drawn on every other one.
    sky_odd: bool,
    /// Whole-window repaints still to do once the island settles (after the panel closed).
    clean_frames: u8,
    /// Lyrics: the song they are for (0: none), its lyrics, and the last few songs'.
    lyrics_key: u64,
    lyrics_now: Option<crate::lyrics::Lyrics>,
    lyrics_cache: Vec<(u64, crate::lyrics::Answer)>,
    /// Beat drops: running bass average, the last drop, and dice for where.
    drop_bass: f32,
    drop_last: Instant,
    drop_seed: u32,
    /// Its pose changed: the next frame draws its corner only.
    pet_dirty: bool,
    mascot_happy: Option<Instant>,
    mascot_next_blink: Instant,
    mascot_bass: f32,
    mascot_beat: Instant,
    /// Checks made so far while the user signs in to Claude.
    claude_polls: u32,
    /// Tests: pretend the user is away, or is not.
    mascot_away: bool,
    mascot_awake: bool,
    /// Voice to text: listening, the result waiting to be typed (with retries so
    /// far), and what a test run would have typed.
    voice_active: bool,
    voice_typing: Option<(String, u32)>,
    voice_typed: String,
    /// Screen recording: since when (after the first frame), and the helper being started.
    recording: Option<Instant>,
    record_starting: bool,
    /// Exchange rates for the command bar (when `currency = on`).
    rates: Option<crate::currency::Rates>,
    /// Attachments for the next message, the ids handed to their readers,
    /// small thumbnails of the chat's pictures, and the file picker being open.
    chat_pending: Vec<crate::chat::Pending>,
    chat_ids: u32,
    chat_thumbs: Vec<(u64, Pixels)>,
    picking: bool,
    /// Last seen input language (0 = not known yet) and the shell-hook message id.
    layout: u32,
    shell_msg: u32,
    pub snapshot: Option<std::path::PathBuf>,
    /// Test mode (`--hidden`): never show the window and ignore game-suspend,
    /// so behaviour can be measured without drawing on the user's screen.
    pub hidden: bool,
    /// Snapshot index and how many more frames to draw before saving it.
    snap_pending: Option<(isize, u8)>,
    /// The pending snapshot draws only what the damage tracking draws (test hook 3, lparam 1000 + n).
    snap_honest: bool,
}

/// Rough delay between committing an animation and the compositor starting it.
const COMMIT_LATENCY: f32 = 0.012;

impl App {
    /// `demo`: `None` for normal use, `Some(bare)` for the scripted demo
    /// (`bare` hides the caption and the fake cursor, for measuring).
    pub fn new(hwnd: HWND, cfg: Config, demo: Option<bool>, debug: bool, test_hooks: bool) -> Self {
        Self {
            hwnd,
            cfg,
            s: State::new(),
            gfx: None,
            hook: None,
            winevent: HWINEVENTHOOK::default(),
            demo: demo.map(Demo::new),
            stats: debug.then(Stats::new),
            pts: [Vector2::default(); N],
            origin: (0, 0),
            monitor: RECT::default(),
            size: (0, 0),
            scale: 1.0,
            dirty: false,
            fresh: true,
            last: Instant::now(),
            suspended: false,
            paused: false,
            interactive: false,
            hover_armed: false,
            collapse_armed: false,
            labels_measured: false,
            frames: 0,
            damage: Damage::new(),
            test_hooks,
            fake_cursor: None,
            need_draw: false,
            comp: None,
            hide_wave: false,
            wave_dirty: false,
            steady_frames: 0,
            eq_levels: [0.0; EQ_BARS],
            eq_place: (0.0, 0.0, -1.0),
            eq_colour: theme::accent(),
            media: None,
            capture: None,
            art: None,
            art_serial: 0,
            drag_volume: false,
            island_drag: None,
            drag_centre: None,
            // Critically damped: it glides in without overshooting the edge.
            centre: crate::anim::Spring::new(0.0, 140.0, 24.0),
            gliding: false,
            fake_screen_x: None,
            toast_hooks: [HWINEVENTHOOK::default(); 2],
            shell_host: None,
            server: None,
            agent_seen: 0,
            history: None,
            clip_shown: Vec::new(),
            clip_scroll: 0,
            clip_ignore: false,
            thumbs: Vec::new(),
            shelf: None,
            shelf_scroll: 0,
            stats_fake: false,
            press: None,
            dragging: None,
            notes: None,
            note_scroll: 0,
            note_drag: false,
            dock: None,
            dock_icons: Vec::new(),
            notes_dir: None,
            focused: false,
            previous_window: HWND::default(),
            index: None,
            cmd_query: String::new(),
            cmd_rows: Vec::new(),
            hotkey_on: false,
            hotkey_warned: false,
            agents: std::collections::HashMap::new(),
            asks: std::collections::HashMap::new(),
            agent_heard: None,
            tab_grab: None,
            home_grab: None,
            grip: None,
            grip_orig: Vec::new(),
            ask_step: None,
            seen: std::collections::HashSet::new(),
            seen_ready: false,
            watch: None,
            downloads: None,
            store: timer::Store::default(),
            queue: events::Queue::default(),
            toast: None,
            download_active: false,
            music_accent: (theme::accent(), theme::accent()),
            level_pinned: false,
            vpn: None,
            vpn_known: false,
            rain_warned: false,
            chat: None,
            chat_key: None,
            chat_input: String::new(),
            chat_session: None,
            settings_win: None,
            config_path: config::path().unwrap_or_else(|| std::env::temp_dir().join("islet-config.txt")),
            test_startup: false,
            mascot_blink: None,
            mascot_doing: None,
            mascot_next_idle: Instant::now() + std::time::Duration::from_secs(6),
            mascot_greeted: Instant::now(),
            mascot_was_away: false,
            mascot_sleep_since: Instant::now(),
            mascot_side: 1.0,
            mascot_rng: (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7) | 1),
            mascot_freeze: None,
            mascot_last: Instant::now(),
            mascot_look: (0.0, 0.0),
            mascot_lean: 0.0,
            mascot_fond: 0.0,
            mascot_rub: (0.0, 0.0),
            mascot_rubs: Vec::new(),
            mascot_clicks: (Instant::now(), 0),
            pet_live: false,
            sky_live: false,
            sky_odd: false,
            clean_frames: 0,
            lyrics_key: 0,
            lyrics_now: None,
            lyrics_cache: Vec::new(),
            drop_bass: 0.0,
            drop_last: Instant::now(),
            drop_seed: 0x2545_F491,
            pet_dirty: false,
            mascot_happy: None,
            mascot_next_blink: Instant::now(),
            mascot_bass: 0.0,
            mascot_beat: Instant::now(),
            claude_polls: 0,
            mascot_away: false,
            mascot_awake: false,
            voice_active: false,
            voice_typing: None,
            voice_typed: String::new(),
            recording: None,
            record_starting: false,
            rates: None,
            chat_pending: Vec::new(),
            chat_ids: 0,
            chat_thumbs: Vec::new(),
            picking: false,
            layout: 0,
            shell_msg: 0,
            snapshot: None,
            hidden: false,
            snap_pending: None,
            snap_honest: false,
        }
    }

    pub fn start(&mut self) {
        // Test runs (hidden) never make a sound.
        crate::sound::enable(!self.hidden && self.demo.is_none());
        crate::sound::configure(self.cfg.sounds);
        self.apply_look();
        self.s.lyric.enabled = self.cfg.lyrics;
        self.s.lyric.in_pill = self.cfg.lyrics_pill;
        self.s.stats_pill = self.cfg.stats_pill;
        self.sync_stats();
        self.place();
        self.apply_theme(false);
        self.s.snap_tabs();
        tray::add(self.hwnd);
        tray::CLICK_THROUGH.store(self.cfg.click_through, Relaxed);
        // Test runs change a copy of the settings, never the user's file.
        if self.hidden {
            let copy = std::env::temp_dir().join("islet-hidden-config.txt");
            let _ = std::fs::copy(&self.config_path, &copy);
            self.config_path = copy;
        }
        self.s.wave_style = self.cfg.wave;
        self.s.wave_level = wave_neutral(self.cfg.wave.react);
        // The demo's character scene uses the ghost.
        self.s.mascot = if self.demo.is_some() { crate::mascot::find("ghost") } else { crate::mascot::find(&self.cfg.character) };
        if self.demo.is_some() {
            let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
            let m = &mut self.s.media;
            (m.session, m.playing, m.position, m.duration) = (true, false, 88.0, 214.0);
            (m.title, m.artist, m.source) = (w("Paper Lanterns"), w("Demo Artist"), w("DEMO PLAYER"));
            self.s.volume = 0.62;
        } else {
            self.media = Some(Media::start(self.hwnd));
            self.watch = Watch::start(self.hwnd, self.downloads.clone());
            self.store = timer::Store::load();
        }
        unsafe {
            if self.demo.is_none() {
                self.hook = Some(MouseHook::start(self.hwnd, self.cfg.caps_popup));
                if self.cfg.layout_popup {
                    // Shell notifications tell us when the input language changes; no hook involved.
                    self.shell_msg = RegisterWindowMessageW(w!("SHELLHOOK"));
                    let _ = RegisterShellHookWindow(self.hwnd);
                }
                EVENT_TARGET.store(self.hwnd.0 as isize, Relaxed);
                self.winevent = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                // Fallback fullscreen check (covers apps that go fullscreen without a focus change).
                SetCoalescableTimer(Some(self.hwnd), T_WATCH, 3000, None, 1500);
            }
            if self.stats.is_some() {
                SetTimer(Some(self.hwnd), T_DEBUG, 500, None);
            }
            if !self.hidden {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.arm_clock();
        self.update_zone();
        self.invalidate();
        self.check_gamemode();
        // Anything that became due while Islet was not running fires now.
        self.fire_due();
        self.watch_toasts();
        if self.demo.is_none() {
            // The pipe stays open during game-suspend, so agents are never left hanging.
            self.server = agent::Server::start(self.hwnd);
            // The update check, when switched on, a quarter of a minute after start.
            if self.cfg.update_check && !self.hidden {
                unsafe { SetTimer(Some(self.hwnd), T_UPDATE, 15_000, None) };
            }
            // Weather only when it was switched on; tests never use the network.
            if self.cfg.weather && !self.hidden {
                if self.cfg.city.is_empty() {
                    self.s.weather_note = "Add city = your town to the settings file, then restart Islet.".encode_utf16().collect();
                } else {
                    self.s.weather_note = "Fetching the forecast\u{2026}".encode_utf16().collect();
                    unsafe { SetTimer(Some(self.hwnd), T_WEATHER, 3000, None) };
                }
            }
            if !self.hidden {
                unsafe {
                    let _ = AddClipboardFormatListener(self.hwnd);
                }
            }
            // Files dragged onto the island land on the shelf.
            let accepts = shelf::register(self.hwnd);
            if self.test_hooks {
                unsafe {
                    let _ = SetPropW(self.hwnd, w!("IsletDrop"), Some(windows::Win32::Foundation::HANDLE(accepts as usize as *mut _)));
                }
            }
        }
        self.register_hotkey();
        self.resolve();
    }

    pub fn shutdown(&mut self) {
        // A recording in progress is finished and saved by its helper.
        crate::record::stop();
        crate::voice::stop();
        // Closing the pipe answers any open permission request with "no decision".
        self.server = None;
        self.unregister_hotkey();
        self.save_note();
        shelf::unregister(self.hwnd);
        unsafe {
            let _ = RemoveClipboardFormatListener(self.hwnd);
        }
        self.unwatch_toasts();
        self.watch = None;
        if self.shell_msg != 0 {
            unsafe {
                let _ = DeregisterShellHookWindow(self.hwnd);
            }
        }
        tray::remove(self.hwnd);
        self.media = None;
        self.capture = None;
        self.hook = None;
        unsafe {
            if !self.winevent.is_invalid() {
                let _ = UnhookWinEvent(self.winevent);
            }
        }
        self.gfx = None;
    }

    // ------------------------------------------------------------ frames --

    pub fn wants_frame(&self) -> bool {
        self.dirty && !self.suspended
    }

    /// How many compositor refreshes to wait after the frame just drawn.
    /// The slow ambient wave is drawn on every second refresh.
    pub fn pace(&self) -> u32 {
        if self.s.ambient && !self.s.demo_cursor {
            2
        } else {
            1
        }
    }

    /// Requests a frame that repaints the whole island (a discrete change such
    /// as the clock, a hover highlight or a theme switch).
    fn repaint(&mut self) {
        self.s.repaint = true;
        self.invalidate();
        // What the settings window shows may have changed too.
        self.invalidate_settings();
    }

    fn invalidate(&mut self) {
        self.need_draw = true;
        if !self.dirty {
            self.dirty = true;
            self.fresh = true;
        }
    }

    /// Advances the animation and presents one frame.
    pub fn frame(&mut self) {
        let now = Instant::now();
        let mut dt = now.duration_since(self.last).as_secs_f32().min(0.05);
        if self.fresh {
            // First frame after sleeping: don't integrate the time spent asleep.
            dt = 1.0 / 120.0;
            self.fresh = false;
        }
        self.last = now;

        // The live wave was presented last frame, so the wave layers can go now.
        if self.hide_wave {
            self.hide_wave = false;
            if let Some(gfx) = &self.gfx {
                gfx.wave_hide();
            }
        }

        if let Some(demo) = &mut self.demo {
            demo.step(dt, &mut self.s);
            // The demo moves a fake cursor every frame: the eyes follow it, and it dances.
            if let Some((mx, my)) = ui::mascot_spot(&self.s, self.s.shape().left()) {
                self.s.pose.look = crate::mascot::look_at(self.s.cursor.0 - mx, self.s.cursor.1 - my);
                let beats = self.s.t * 2.0;
                self.s.pose.act = crate::mascot::Act::Bounce;
                self.s.pose.t = beats.fract();
                self.s.pose.side = if (beats as i32) % 2 == 0 { 1.0 } else { -1.0 };
            }
        } else {
            self.track_cursor();
            // The character moves on the refresh too while it is acting or easing.
            if self.pet_live {
                self.update_mascot();
            }
        }
        // The Weather tab animates on every refresh while it is on screen, and
        // sounds like the sky it shows (thunder follows each flash).
        self.sky_live = !self.suspended && crate::panels::weather_step(&mut self.s, dt);
        crate::sound::ambience(if self.sky_live { crate::panels::weather_ambience(&self.s) } else { crate::sound::Ambience::SILENT });
        if let Some((delay, strength)) = self.s.wx.thunder.take() {
            crate::sound::cue_after(crate::sound::Cue::Thunder(strength), delay);
        }
        let mut moving = self.s.tick(dt);
        // The window follows the island's width (attached to an edge) and glides.
        if self.sync_window_x(dt) {
            moving = true;
        }

        // The compositor can only keep animating the wave while nothing else
        // about the island changes. Otherwise take the wave back at the phase
        // the compositor has reached and draw it live again.
        if let Some((since, phase0, speed)) = self.comp {
            if !self.s.wave_steady() || self.s.demo_cursor || self.wave_dirty {
                let elapsed = (since.elapsed().as_secs_f32() - COMMIT_LATENCY).max(0.0);
                self.s.wave_phase = (phase0 + speed * elapsed) % std::f32::consts::TAU;
                self.wave_dirty = false;
                self.s.comp_wave = false;
                self.s.repaint = true;
                self.comp = None;
                self.hide_wave = true;
                moving = true;
            }
        }

        // The demo has no audio: drive the bar layers with a synthetic signal.
        if self.demo.is_some() && self.eq_place.2 > 0.02 {
            let t = self.s.t + self.frames as f32 / 60.0;
            for (i, level) in self.eq_levels.iter_mut().enumerate() {
                let f = i as f32;
                *level = 0.5 + 0.5 * (t * (5.3 + f * 1.7) + f * 1.9).sin() * (0.6 + 0.4 * (t * 2.1 + f).cos());
            }
            self.music_level();
            if let Some(gfx) = &self.gfx {
                let (cx, cy, visible) = self.eq_place;
                let _ = gfx.eq_update(cx, cy, self.eq_levels, self.s.accent, visible);
                gfx.commit();
            }
        }

        // In the demo the script advances every refresh, but a frame is only
        // presented when something actually changed.
        // At rest the sky is drawn every other refresh (30 fps); hover, scrubbing
        // and transitions get every one.
        let sky_draw = self.sky_live && {
            self.sky_odd = !self.sky_odd;
            self.s.wx.busy || self.sky_odd
        };
        let live = self.pet_dirty || sky_draw;
        // Once the island has settled after the panel or command bar closed, the
        // whole window is repainted in both buffers: nothing stale can survive.
        let cleanup = !moving && self.clean_frames > 0;
        let draw = moving || self.need_draw || self.s.repaint || live || cleanup;
        // Only the character (or the weather) changed: its rectangle is all that gets drawn.
        self.s.pet_only = live && !moving && !self.need_draw && !self.s.repaint;
        self.need_draw = false;
        self.pet_dirty = false;
        if !draw {
            self.dirty = self.demo.is_some() || self.pet_live || self.sky_live;
            // An animation usually ends here (its last step changes nothing),
            // so this is where the idle memory trim has to be armed.
            if self.demo.is_none() && self.stats.is_none() {
                unsafe { SetTimer(Some(self.hwnd), T_TRIM, 8000, None) };
            }
            return;
        }

        if self.gfx.is_none() && !self.create_gfx() {
            self.dirty = false;
            unsafe { SetTimer(Some(self.hwnd), T_RETRY, 1000, None) };
            return;
        }
        let mut audio_check = false;
        if let Some(gfx) = &self.gfx {
            let status = if moving { "animating" } else { "asleep" };
            self.s.full_frame = cleanup;
            if cleanup {
                self.clean_frames -= 1;
            }
            if ui::draw(gfx, &self.s, &mut self.pts, self.stats.as_ref(), status, &mut self.damage).is_err() {
                // Device lost: rebuild everything on the next frame.
                self.gfx = None;
                self.dirty = true;
                return;
            }
            self.s.repaint = false;

            // Keep the equalizer layers glued to the pill while it moves or fades.
            let place = ui::eq_place(&self.s);
            if place != self.eq_place || self.s.accent != self.eq_colour {
                self.eq_place = place;
                self.eq_colour = self.s.accent;
                let result = gfx.eq_update(place.0, place.1, self.eq_levels, self.s.accent, place.2);
                gfx.commit();
                if self.test_hooks {
                    let code = result.err().map_or(0, |e| e.code().0 as u32 as usize);
                    unsafe {
                        let _ = SetPropW(self.hwnd, w!("IsletEqError"), Some(windows::Win32::Foundation::HANDLE(code as *mut _)));
                    }
                }
                audio_check = true;
            }

            // Hand the wave to the compositor once it has been steady for a moment.
            if self.comp.is_none() && self.s.wave_steady() && !self.s.demo_cursor && self.snapshot.is_none() {
                self.steady_frames += 1;
            } else {
                self.steady_frames = 0;
            }
            // The layers are drawn a few milliseconds per frame (all at once froze
            // the island for ~1.7 s, again with every song's new colours); the live
            // wave keeps running until they are ready.
            let ready = self.steady_frames >= 10 && {
                let spec = ui::wave_spec(&self.s);
                match gfx.wave_prepare(spec, WAVE_BUILD_BUDGET) {
                    Ok(done) => done,
                    Err(_) => {
                        gfx.wave_forget_pending();
                        self.steady_frames = 0;
                        false
                    }
                }
            };
            if ready {
                self.steady_frames = 0;
                if gfx.wave_show(ui::wave_spec(&self.s), self.s.wave_phase, self.s.wave_mid_amp()).is_ok() {
                    self.comp = Some((Instant::now(), self.s.wave_phase, self.s.wave_speed()));
                    self.s.comp_wave = true;
                    // One more frame draws the shorter body under the layers.
                    self.s.repaint = true;
                    moving = true;
                }
            }
        }
        if audio_check {
            self.sync_audio();
        }
        if let Some((index, left)) = self.snap_pending {
            if left > 1 {
                self.snap_pending = Some((index, left - 1));
                if !self.snap_honest {
                    self.s.repaint = true;
                }
                moving = true;
            } else {
                self.snap_pending = None;
                if let (Some(dir), Some(gfx)) = (&self.snapshot, &self.gfx) {
                    let _ = gfx.save_bmp(&dir.join(format!("snap{index}.bmp")));
                }
            }
        }
        if let Some(stats) = &mut self.stats {
            stats.frame(now.elapsed().as_secs_f32() * 1000.0);
        }

        // Frame counter and stage timings (in microseconds) readable from outside
        // with GetProp, for measuring without the HUD's own overhead.
        self.frames += 1;
        unsafe {
            use windows::Win32::Foundation::HANDLE;
            let _ = SetPropW(self.hwnd, w!("IsletFrames"), Some(HANDLE(self.frames as *mut _)));
            if self.test_hooks {
                // 1 idle, 2 expanded, 3 hidden, 4 command bar; +32 when pinned; tab in bits 8+.
                let mode = match self.s.mode {
                    Mode::Idle => 1usize,
                    Mode::Expanded => 2,
                    Mode::Hidden => 3,
                    Mode::Command => 4,
                };
                // +64 when the command-bar shortcut is registered.
                let state = mode + self.s.tab * 256 + if self.s.pinned { 32 } else { 0 } + if self.hotkey_on { 64 } else { 0 };
                let _ = SetPropW(self.hwnd, w!("IsletState"), Some(HANDLE(state as *mut _)));
                // 1 clock, 2 music, 3 download, 4 done, 5 timer, 6 toast, 7 mascot.
                let _ = SetPropW(self.hwnd, w!("IsletPill"), Some(HANDLE((self.s.pill as usize + 1) as *mut _)));
                // 1 while the compositor layers draw the music wave.
                let _ = SetPropW(self.hwnd, w!("IsletWave"), Some(HANDLE(self.comp.is_some() as usize as *mut _)));
                // Ctrl + drag left over (for tests; 0 = nothing held, nothing settling):
                // bits 1/2/4 tab held / Home item held / group item held (drawn lifted),
                // 8/16/32 the same still grabbed, 64 offsets still gliding.
                let held = (self.s.tab_held.is_some() as usize)
                    | (self.s.home_held.is_some() as usize) << 1
                    | (self.s.grip_held.is_some() as usize) << 2
                    | (self.tab_grab.is_some() as usize) << 3
                    | (self.home_grab.is_some() as usize) << 4
                    | (self.grip.is_some() as usize) << 5
                    | (!self.s.home_settled() as usize) << 6;
                let _ = SetPropW(self.hwnd, w!("IsletHeld"), Some(HANDLE(held as *mut _)));
            }
            if self.frames.is_multiple_of(64) {
                if let Some(gfx) = &self.gfx {
                    let p = gfx.prof.get();
                    for (name, v) in [w!("IsletGeo"), w!("IsletDraw"), w!("IsletFlush"), w!("IsletPresent")].into_iter().zip(p) {
                        let _ = SetPropW(self.hwnd, name, Some(HANDLE((v * 1000.0) as usize as *mut _)));
                    }
                }
            }
        }

        self.dirty = moving || self.demo.is_some() || self.pet_live || self.sky_live || self.clean_frames > 0;
        if !moving && self.stats.is_none() {
            // One-shot: give memory back once the island has rested for a while.
            unsafe { SetTimer(Some(self.hwnd), T_TRIM, 8000, None) };
        }
    }

    /// Loopback capture only runs while the equalizer is on screen.
    fn sync_audio(&mut self) {
        let want = self.eq_place.2 > 0.02 && !self.suspended && self.demo.is_none();
        if want && self.capture.is_none() {
            self.capture = Capture::start(self.hwnd);
        } else if !want && self.capture.is_some() {
            self.capture = None;
            self.eq_levels = [0.0; EQ_BARS];
            // No more levels: the wave settles back to its usual height.
            if !self.level_pinned {
                self.s.wave_level = wave_neutral(self.s.wave_style.react);
            }
            if let Some(gfx) = &self.gfx {
                if gfx.wave_shown() {
                    gfx.wave_level(self.s.wave_mid_amp());
                    gfx.commit();
                }
            }
        }
    }

    /// Follows the bass with the middle of the music wave: fast rise, slower
    /// fall. While the compositor draws the wave this only moves its segment
    /// scales (committed with the equalizer); the live wave is redrawn anyway.
    fn music_level(&mut self) {
        if self.level_pinned {
            return;
        }
        let [bass, low, ..] = self.eq_levels;
        let target = (0.7 * bass + 0.3 * low).clamp(0.0, 1.0).powf(0.8);
        let k = if target > self.s.wave_level { 0.45 } else { 0.1 };
        self.s.wave_level += (target - self.s.wave_level) * k;
        if let Some(gfx) = &self.gfx {
            if gfx.wave_shown() {
                gfx.wave_level(self.s.wave_mid_amp());
            }
        }
    }

    /// Applies the newest SMTC snapshot: texts, artwork, accent, music pill.
    fn apply_media(&mut self) {
        let Some(snap) = media::take() else { return };
        if self.suspended {
            return;
        }
        if snap.art_serial != self.art_serial {
            self.art_serial = snap.art_serial;
            self.art = snap.art;
            self.upload_art();
        }
        let m = &mut self.s.media;
        let accent = snap.accent.unwrap_or((theme::accent(), theme::accent()));
        let show = snap.session && snap.playing;
        // Players report their position many times a second. That alone changes
        // nothing on screen (the open panel refreshes its progress once a second).
        let position_only = m.session == snap.session
            && m.playing == snap.playing
            && m.title == snap.title
            && m.artist == snap.artist
            && m.source == snap.source
            && accent == self.music_accent
            && m.can == snap.can
            && show == (self.s.wave.target > 0.5);
        m.position = snap.position;
        m.duration = snap.duration;
        m.stamp = snap.stamp;
        if position_only {
            // (A seek moves the lyric line; a plain position report does not repaint.)
            self.sync_lyric();
            return;
        }
        m.session = snap.session;
        m.playing = snap.playing;
        m.title = snap.title;
        m.artist = snap.artist;
        m.source = snap.source;
        m.can = snap.can;

        self.music_accent = accent;
        // Cards that follow the music take the cover's colour while it plays.
        theme::set_music_tint(if show { snap.accent.map(|a| a.0) } else { None });
        self.s.set_music(show, self.music_accent.0, self.music_accent.1);
        self.repaint();
        self.lyrics_for_song();
        self.sync_lyric();
        self.resolve();
    }

    // ------------------------------------------------------------ lyrics --

    /// The song may have changed (or lyrics were switched on or off): shows the
    /// cached lyrics, or looks them up.
    fn lyrics_for_song(&mut self) {
        self.s.lyric.enabled = self.cfg.lyrics;
        let m = &self.s.media;
        if !self.cfg.lyrics || !m.session || m.title.is_empty() {
            if self.lyrics_key != 0 || !self.s.lyric.lines.is_empty() {
                self.lyrics_key = 0;
                self.lyrics_now = None;
                self.s.lyric.clear("");
                self.kill(T_LYRIC);
                self.repaint();
            }
            return;
        }
        let (title, artist, duration) = (String::from_utf16_lossy(&m.title), String::from_utf16_lossy(&m.artist), m.duration);
        let key = crate::lyrics::key(&title, &artist);
        if key == self.lyrics_key {
            return;
        }
        self.lyrics_key = key;
        self.lyrics_now = None;
        self.kill(T_LYRIC);
        match self.lyrics_cache.iter().find(|(k, _)| *k == key).map(|(_, a)| a.clone()) {
            Some(answer) => self.show_lyrics(answer),
            None => {
                self.s.lyric.clear("Looking up the lyrics\u{2026}");
                // Test runs never use the network.
                if !self.hidden {
                    crate::lyrics::request(self.hwnd, title, artist, duration);
                }
                self.repaint();
            }
        }
    }

    fn show_lyrics(&mut self, answer: crate::lyrics::Answer) {
        use crate::lyrics::Answer;
        match answer {
            Answer::Found(lyrics) => {
                self.s.lyric.clear("");
                self.s.lyric.lines = lyrics.lines.iter().map(|(_, text)| text.encode_utf16().collect()).collect();
                self.s.lyric.synced = lyrics.synced;
                self.lyrics_now = Some(lyrics);
            }
            Answer::Instrumental => {
                self.lyrics_now = None;
                self.s.lyric.clear("Instrumental: no words to sing along");
            }
            Answer::Missing => {
                self.lyrics_now = None;
                self.s.lyric.clear("No lyrics found for this song");
            }
        }
        self.sync_lyric();
        self.repaint();
    }

    /// Puts the lyric line where the song is and arms `T_LYRIC` for the next
    /// one (nothing runs between lines). Plain lyrics scroll through the card
    /// with the song's progress, every few seconds while the card shows them.
    fn sync_lyric(&mut self) {
        let Some(lyrics) = &self.lyrics_now else { return };
        let m = &self.s.media;
        let position = m.elapsed();
        if !lyrics.synced {
            let card_open = self.s.lyric.card && self.s.mode == Mode::Expanded && self.s.tab == 0;
            let (duration, playing) = (m.duration, m.playing);
            let last = lyrics.lines.len().saturating_sub(1) as f32;
            if card_open && duration > 0.0 {
                self.s.lyric.scroll.set((position / duration * last).clamp(0.0, last));
                self.invalidate();
                if playing {
                    unsafe { SetTimer(Some(self.hwnd), T_LYRIC, 3000, None) };
                }
            }
            return;
        }
        let index = crate::lyrics::line_at(&lyrics.lines, position);
        let next = crate::lyrics::next_change(&lyrics.lines, index, position);
        let playing = m.playing;
        if index != self.s.lyric.index {
            self.s.lyric.set_line(index);
            self.invalidate();
        }
        match next {
            Some(wait) if playing => unsafe {
                SetTimer(Some(self.hwnd), T_LYRIC, (wait * 1000.0) as u32 + 5, None);
            },
            _ => self.kill(T_LYRIC),
        }
    }

    fn upload_art(&self) {
        if let Some(gfx) = &self.gfx {
            match &self.art {
                Some(pixels) => gfx.set_art(pixels, media::ART_SIZE),
                None => gfx.clear_art(),
            }
        }
    }

    fn set_volume(&mut self, level: f32) {
        if (level - self.s.volume).abs() > 0.004 {
            self.s.volume = level;
            audio::set_volume(level);
            self.repaint();
        }
    }

    /// Forgets everything that lived in the renderer (layers included).
    fn reset_layers(&mut self) {
        self.comp = None;
        self.hide_wave = false;
        self.steady_frames = 0;
        self.s.comp_wave = false;
        self.eq_place = (0.0, 0.0, -1.0);
    }

    fn create_gfx(&mut self) -> bool {
        self.damage = Damage::new();
        self.reset_layers();
        self.s.repaint = true;
        match Gfx::new(self.hwnd, self.size.0 as u32, self.size.1 as u32, self.scale, self.cfg.gpu) {
            Ok(gfx) => {
                if !self.labels_measured {
                    for (i, label) in ui::TAB_LABELS.iter().enumerate() {
                        self.s.tab_label_w[i] = gfx.measure(crate::render::ws(*label), Font::Label).ceil();
                    }
                    self.labels_measured = true;
                    self.s.snap_tabs();
                }
                for (key, picture) in self.thumbs.iter().chain(&self.dock_icons) {
                    gfx.set_image(*key, &picture.bgra, picture.w, picture.h);
                }
                self.gfx = Some(gfx);
                self.upload_art();
                true
            }
            Err(_) => false,
        }
    }

    // --------------------------------------------------------- placement --

    /// Centres the window at the top of the target monitor. Returns `true`
    /// when the pixel size or scale changed (the swap chain must be rebuilt).
    fn place(&mut self) -> bool {
        unsafe {
            let mut pt = POINT::default();
            if self.cfg.follow_cursor {
                let _ = GetCursorPos(&mut pt);
            }
            let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return false;
            }
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let scale = dpi_x as f32 / 96.0 * self.cfg.scale;
            let m = info.rcMonitor;
            let w = (WIN_W * scale).ceil() as i32;
            let h = (WIN_H * scale).ceil() as i32;
            let changed = (w, h) != self.size || scale != self.scale;
            self.monitor = m;
            self.size = (w, h);
            self.scale = scale;
            let centre = self.allowed_centre(chosen_centre(m, scale, self.cfg.island_x));
            self.centre.snap(centre);
            self.gliding = false;
            let x = (centre - CX * scale).round() as i32;
            self.origin = (x, m.top);
            self.s.edge = ((m.left - x) as f32 / scale, (m.right - x) as f32 / scale);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, m.top, w, h, SWP_NOACTIVATE);
            input::set_monitor(m, self.cfg.follow_cursor);
            changed
        }
    }

    fn replace(&mut self) {
        if self.place() {
            self.gfx = None;
        }
        self.update_zone();
        self.repaint();
    }

    /// The pointer in screen pixels (tests may pretend one: hook wparam 21).
    fn screen_cursor(&self) -> POINT {
        if let Some(x) = self.fake_screen_x {
            return POINT { x, y: self.origin.1 + 10 };
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        pt
    }

    /// Where the island's centre may be for its current width: anywhere the
    /// whole island stays on the monitor (so at an edge it touches the edge).
    fn allowed_centre(&self, want: f32) -> f32 {
        let m = self.monitor;
        let half = self.s.w.x * 0.5 * self.scale;
        let (lo, hi) = (m.left as f32 + half, m.right as f32 - half);
        if hi <= lo {
            (m.left + m.right) as f32 * 0.5
        } else {
            want.clamp(lo, hi)
        }
    }

    /// Keeps the window where the island's centre should be: under the
    /// pointer while dragging, gliding after a release, and pushed in from an
    /// edge while the island is wider (an open panel stays attached to the
    /// edge and grows away from it). Returns `true` while it glides.
    fn sync_window_x(&mut self, dt: f32) -> bool {
        let m = self.monitor;
        let want = self.drag_centre.unwrap_or_else(|| chosen_centre(m, self.scale, self.cfg.island_x));
        let target = self.allowed_centre(want);
        if self.gliding {
            self.centre.set(target);
            self.gliding = self.centre.step(dt);
        } else {
            self.centre.snap(target);
        }
        let x = (self.centre.x - CX * self.scale).round() as i32;
        if x != self.origin.0 {
            self.origin.0 = x;
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, m.top, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE);
            }
            self.update_zone();
        }
        self.s.edge = ((m.left - x) as f32 / self.scale, (m.right - x) as f32 / self.scale);
        self.gliding
    }

    /// The pointer moved while a tab is held: the tab follows it and swaps
    /// places with a neighbour once its middle passes the neighbour's.
    fn move_tab_drag(&mut self, x: f32, y: f32) {
        let Some((i, grab)) = self.tab_grab else { return };
        if self.s.mode != Mode::Expanded {
            self.end_tab_drag();
            return;
        }
        let (px, _) = self.s.to_panel(x, y);
        let shown: Vec<usize> = self.s.tabs_in_order().collect();
        let (first, last) = (shown.first().copied().unwrap_or(i), shown.last().copied().unwrap_or(i));
        // Kept within the bar. Everything is measured against where the tabs
        // will settle, never where they are mid-slide.
        let lo = ui::tab_left_target(&self.s, first);
        let hi = ui::tab_left_target(&self.s, last) + self.s.tab_w[last].target - self.s.tab_w[i].target;
        let left = (px - grab).clamp(lo, hi.max(lo));
        self.s.tab_drag = Some((i, left));
        // A fast move can pass several tabs at once: swap until it fits.
        let middle = left + self.s.tab_w[i].target * 0.5;
        for _ in 0..shown.len() {
            let order: Vec<usize> = self.s.tabs_in_order().collect();
            let at = order.iter().position(|&t| t == i).unwrap_or(0);
            let centre = |t: usize| ui::tab_left_target(&self.s, t) + self.s.tab_w[t].target * 0.5;
            // (At an end of the bar the middle lands exactly on the last
            // neighbour's centre, so equal counts as passed.)
            let moved = if at > 0 && middle <= centre(order[at - 1]) {
                self.s.shift_tab(i, -1)
            } else if at + 1 < order.len() && middle >= centre(order[at + 1]) {
                self.s.shift_tab(i, 1)
            } else {
                false
            };
            if !moved {
                break;
            }
        }
        // The tab bar is not part of the liquid band: it needs a full repaint.
        self.repaint();
        unsafe {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
        }
    }

    /// The tab was let go: it settles into its place and the order is saved.
    fn end_tab_drag(&mut self) {
        if self.tab_grab.take().is_none() {
            return;
        }
        // It glides from where it was let go into its slot (no jump).
        let left = self.s.tab_drag.map_or(0.0, |(_, x)| x - ui::tabs_origin(&self.s));
        self.s.drop_tab(left);
        if self.s.tab_order != self.cfg.look.tab_order {
            self.cfg.look.tab_order = self.s.tab_order;
            config::set(&self.config_path, "tab_order", &config::tab_order_text(&self.s.tab_order));
        }
        self.repaint();
        // Click-through again if the pointer was let go off the island.
        self.track_cursor();
    }

    /// Ctrl + press on an item of a [`Group`] (`px`, `py` in panel units): it
    /// lifts and follows the pointer.
    fn start_grip(&mut self, grip: Grip, px: f32, py: f32) {
        let dy = ui::grip_dy(&self.s, grip.group);
        let Some((_, slot)) = ui::grip_items(&self.s, grip.group).into_iter().find(|(i, _)| *i == grip.id) else { return };
        let (r, _) = self.s.grip_rect(grip.group, grip.id, slot);
        self.grip = Some((grip, px - r.x, py - dy - r.y));
        self.grip_orig = self.s.order_of(grip.group).to_vec();
        self.s.grip_drag = Some((grip, r.x, r.y));
        self.s.grip_held = Some(grip);
        self.s.grip_lift.set(1.0);
        self.s.set_hover(None);
        unsafe {
            SetCapture(self.hwnd);
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
        self.repaint();
    }

    /// The held item follows the pointer (kept over its group); when its middle
    /// is over another item they change places, gliding.
    fn move_grip(&mut self, x: f32, y: f32) {
        let Some((grip, gx, gy)) = self.grip else { return };
        if self.s.mode != Mode::Expanded || self.s.tab != ui::grip_tab(grip.group) {
            self.end_grip();
            return;
        }
        let (px, py) = self.s.to_panel(x, y);
        let py = py - ui::grip_dy(&self.s, grip.group);
        let items = ui::grip_items(&self.s, grip.group);
        let Some(&(_, slot)) = items.iter().find(|(i, _)| *i == grip.id) else { return };
        let (held, _) = self.s.grip_rect(grip.group, grip.id, slot);
        let x0 = items.iter().map(|(_, r)| r.x).fold(f32::MAX, f32::min);
        let y0 = items.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min);
        let x1 = items.iter().map(|(_, r)| r.x + r.w).fold(f32::MIN, f32::max);
        let y1 = items.iter().map(|(_, r)| r.y + r.h).fold(f32::MIN, f32::max);
        let left = (px - gx).clamp(x0, (x1 - held.w).max(x0));
        let top = (py - gy).clamp(y0, (y1 - held.h).max(y0));
        self.s.grip_drag = Some((grip, left, top));
        let (cx, cy) = (left + held.w * 0.5, top + held.h * 0.5);
        if grip.group.swaps() {
            // Trading places is decided against the order at the grab: the
            // section under the pointer trades with the held one (moving on
            // undoes that and trades with the next), so passing over a section
            // on the way leaves it where it was.
            // (By the pointer: a big section's middle may never reach a small one.)
            let order = self.s.order_of(grip.group).to_vec();
            let under = items.iter().find(|(_, r)| r.contains(px, py)).and_then(|(id, _)| order.iter().position(|o| o == id));
            let mut wanted = self.grip_orig.clone();
            if let (Some(r), Some(h)) = (under, wanted.iter().position(|&o| o == grip.id)) {
                wanted.swap(h, r);
            }
            if under.is_some() && wanted != order {
                self.apply_grip_order(grip.group, wanted);
            }
        } else if let Some(&(target, _)) = items.iter().find(|(i, r)| *i != grip.id && r.contains(cx, cy)) {
            self.reorder_grip(grip.group, grip.id, target);
        }
        self.repaint();
        unsafe {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
    }

    /// Puts `held` where `target` is (swapping groups trade the two places, the
    /// others shift over). Everything keeps where it is drawn as an offset that
    /// springs back, so items glide (and resize) into their new slots.
    fn reorder_grip(&mut self, group: Group, held: usize, target: usize) {
        let mut order = self.s.order_of(group).to_vec();
        let (Some(hi), Some(ti)) = (order.iter().position(|&i| i == held), order.iter().position(|&i| i == target)) else { return };
        if group.swaps() {
            order.swap(hi, ti);
        } else {
            let id = order.remove(hi);
            let at = order.iter().position(|&i| i == target).unwrap_or(0);
            order.insert(if hi < ti { at + 1 } else { at }, id);
        }
        self.apply_grip_order(group, order);
    }

    /// Sets `group`'s order; everything keeps where it is drawn as an offset
    /// that springs back, so items glide (and resize) into their new slots.
    fn apply_grip_order(&mut self, group: Group, order: Vec<usize>) {
        let g = group.index();
        let before: Vec<(usize, ui::Area)> = ui::grip_items(&self.s, group).into_iter().map(|(id, slot)| (id, self.s.grip_rect(group, id, slot).0)).collect();
        self.s.order[g] = order;
        for (id, slot) in ui::grip_items(&self.s, group) {
            if let Some((_, old)) = before.iter().find(|(i, _)| *i == id) {
                let o = &mut self.s.grip_off[g][id.min(7)];
                o[0].x = old.x - slot.x;
                o[1].x = old.y - slot.y;
                o[2].x = old.w - slot.w;
                o[3].x = old.h - slot.h;
            }
        }
    }

    /// The held item was let go: it glides into its slot; the order is saved.
    fn end_grip(&mut self) {
        let Some((grip, _, _)) = self.grip.take() else { return };
        let g = grip.group.index();
        if let Some((_, left, top)) = self.s.grip_drag.take() {
            if let Some((_, slot)) = ui::grip_items(&self.s, grip.group).into_iter().find(|(i, _)| *i == grip.id) {
                let o = &mut self.s.grip_off[g][grip.id.min(7)];
                o[0].x = left - slot.x;
                o[1].x = top - slot.y;
            }
        }
        self.s.grip_lift.set(0.0);
        if self.s.order[g] != self.cfg.look.orders[g] {
            self.cfg.look.orders[g] = self.s.order[g].clone();
            let (key, names) = config::GROUP_KEYS[g];
            config::set(&self.config_path, key, &config::order_text(names, &self.s.order[g]));
        }
        self.repaint();
        // Click-through again if the pointer was let go off the island.
        self.track_cursor();
    }

    /// Ctrl + press on a Home quick tile or dock icon: it lifts and follows the pointer.
    fn start_home_drag(&mut self, item: HomeItem, px: f32, py: f32) {
        let (left, top) = match item {
            HomeItem::Tile(id) => {
                let Some(slot) = self.s.look.quick.iter().position(|&q| q == id) else { return };
                let Some(t) = ui::quick_slot(&self.s, slot) else { return };
                (t.x + self.s.quick_off[id].0.x, t.y + self.s.quick_off[id].1.x)
            }
            HomeItem::Dock(i) => {
                let Some(x) = ui::dock_slot_x(&self.s, i) else { return };
                (x + self.s.dock_off[i].x, 0.0)
            }
        };
        self.home_grab = Some((item, px - left, py - top));
        self.s.home_drag = Some((item, left, top));
        self.s.home_held = Some(item);
        self.s.home_lift.set(1.0);
        self.s.dock_hover = None;
        // The button light belongs to the slot the pointer was over, not the item moving.
        self.s.set_hover(None);
        unsafe {
            SetCapture(self.hwnd);
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
        self.repaint();
    }

    /// The held Home item follows the pointer; when its middle reaches another
    /// slot it takes that place and the others shift over, gliding.
    fn move_home_drag(&mut self, x: f32, y: f32) {
        let Some((item, gx, gy)) = self.home_grab else { return };
        if self.s.mode != Mode::Expanded || self.s.tab != 0 {
            self.end_home_drag();
            return;
        }
        let (px, py) = self.s.to_panel(x, y);
        match item {
            HomeItem::Tile(id) => {
                let n = self.s.look.quick.len().min(12);
                let (Some(first), Some(last)) = (ui::quick_slot(&self.s, 0), ui::quick_slot(&self.s, n.saturating_sub(1))) else { return };
                // Kept over the grid.
                let (right, bottom) = ((0..n).filter_map(|k| ui::quick_slot(&self.s, k)).map(|t| t.x).fold(first.x, f32::max), last.y);
                let (left, top) = ((px - gx).clamp(first.x, right), (py - gy).clamp(first.y, bottom));
                self.s.home_drag = Some((item, left, top));
                let (cx, cy) = (left + first.w * 0.5, top + first.h * 0.5);
                let from = self.s.look.quick.iter().position(|&q| q == id).unwrap_or(0);
                if let Some(to) = (0..n).find(|&k| ui::quick_slot(&self.s, k).is_some_and(|t| (t.x..=t.x + t.w).contains(&cx) && (t.y..=t.y + t.h).contains(&cy))) {
                    if to != from {
                        self.reorder_quick(from, to);
                    }
                }
            }
            HomeItem::Dock(from) => {
                let n = self.s.dock.len();
                let (Some(x0), Some(x1)) = (ui::dock_slot_x(&self.s, 0), ui::dock_slot_x(&self.s, n.saturating_sub(1))) else { return };
                let left = (px - gx).clamp(x0, x1.max(x0));
                self.s.home_drag = Some((HomeItem::Dock(from), left, 0.0));
                let step = ui::dock_slot_x(&self.s, 1).map_or(38.0, |x| x - x0);
                let to = (((left - x0) / step).round().max(0.0) as usize).min(n.saturating_sub(1));
                if to != from {
                    self.reorder_dock(from, to);
                    let item = HomeItem::Dock(to);
                    self.home_grab = Some((item, gx, gy));
                    self.s.home_drag = Some((item, left, 0.0));
                    self.s.home_held = Some(item);
                }
            }
        }
        self.repaint();
        unsafe {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
        }
    }

    /// Moves quick action slot `from` to `to`; the tiles that shift keep where
    /// they are drawn and glide to their new slots.
    fn reorder_quick(&mut self, from: usize, to: usize) {
        let ids = self.s.look.quick.clone();
        let before: Vec<(f32, f32)> = ids.iter().enumerate().map(|(k, &id)| ui::quick_slot(&self.s, k).map_or((0.0, 0.0), |t| (t.x + self.s.quick_off[id].0.x, t.y + self.s.quick_off[id].1.x))).collect();
        let id = self.s.look.quick.remove(from);
        self.s.look.quick.insert(to, id);
        for (k, &moved) in self.s.look.quick.clone().iter().enumerate() {
            let Some(old) = ids.iter().position(|&q| q == moved) else { continue };
            if let Some(t) = ui::quick_slot(&self.s, k) {
                self.s.quick_off[moved].0.x = before[old].0 - t.x;
                self.s.quick_off[moved].1.x = before[old].1 - t.y;
            }
        }
    }

    /// Moves dock slot `from` to `to` (store and view); icons that shift glide.
    fn reorder_dock(&mut self, from: usize, to: usize) {
        let n = self.s.dock.len().min(self.s.dock_off.len());
        let before: Vec<f32> = (0..n).map(|k| ui::dock_slot_x(&self.s, k).unwrap_or(0.0) + self.s.dock_off[k].x).collect();
        let velocity: Vec<f32> = (0..n).map(|k| self.s.dock_off[k].v).collect();
        let mut order: Vec<usize> = (0..n).collect();
        let moved = order.remove(from);
        order.insert(to, moved);
        let item = self.s.dock.remove(from);
        self.s.dock.insert(to, item);
        self.dock().move_item(from, to);
        for (k, &old) in order.iter().enumerate() {
            let x = ui::dock_slot_x(&self.s, k).unwrap_or(0.0);
            self.s.dock_off[k].x = before[old] - x;
            self.s.dock_off[k].v = velocity[old];
        }
    }

    /// The held Home item was let go: it glides into its slot and the new order is saved.
    fn end_home_drag(&mut self) {
        let Some((item, _, _)) = self.home_grab.take() else { return };
        if let Some((_, left, top)) = self.s.home_drag.take() {
            match item {
                HomeItem::Tile(id) => {
                    if let Some(t) = self.s.look.quick.iter().position(|&q| q == id).and_then(|slot| ui::quick_slot(&self.s, slot)) {
                        self.s.quick_off[id].0.x = left - t.x;
                        self.s.quick_off[id].1.x = top - t.y;
                    }
                }
                HomeItem::Dock(i) => {
                    if let Some(x) = ui::dock_slot_x(&self.s, i) {
                        self.s.dock_off[i].x = left - x;
                    }
                }
            }
        }
        self.s.home_lift.set(0.0);
        match item {
            HomeItem::Tile(_) if self.s.look.quick != self.cfg.look.quick => {
                self.cfg.look.quick = self.s.look.quick.clone();
                let names: Vec<&str> = self.s.look.quick.iter().filter_map(|&q| config::QUICK_NAMES.get(q).copied()).collect();
                config::set(&self.config_path, "quick_actions", &names.join(", "));
            }
            HomeItem::Dock(_) => self.dock().commit(),
            _ => {}
        }
        self.repaint();
        // Click-through again if the pointer was let go off the island.
        self.track_cursor();
    }

    /// Ctrl + press on the island: from now on the pointer moves it sideways.
    fn start_island_drag(&mut self) {
        let pt = self.screen_cursor();
        // Not by itself the panel opening under a held Ctrl.
        self.hover_armed = false;
        self.kill(T_HOVER);
        self.gliding = false;
        self.island_drag = Some((pt.x, self.centre.x));
        self.drag_centre = Some(self.centre.x);
        unsafe {
            SetCapture(self.hwnd);
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
        }
    }

    /// The pointer moved during a Ctrl + drag: the island follows sideways (up
    /// to the screen edges, where it merges with them) and snaps to the middle near it.
    fn move_island_drag(&mut self) {
        let Some((start_cursor, start_centre)) = self.island_drag else { return };
        let pt = self.screen_cursor();
        let mid = (self.monitor.left + self.monitor.right) as f32 * 0.5;
        let mut centre = start_centre + (pt.x - start_cursor) as f32;
        if (centre - mid).abs() < 14.0 * self.scale {
            centre = mid;
        }
        self.drag_centre = Some(centre);
        self.sync_window_x(0.0);
        // The merge with an edge follows in the next frames.
        self.invalidate();
        unsafe {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
        }
    }

    /// Ends a Ctrl + drag and remembers where the island now is. Let go close
    /// to an edge, it glides the rest of the way and merges with it.
    fn end_island_drag(&mut self) {
        if self.island_drag.take().is_none() {
            return;
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        let m = self.monitor;
        // Where the island ended up (the pointer may have pushed past an edge).
        let pushed = self.drag_centre.take().unwrap_or(self.centre.x);
        let centre = self.allowed_centre(pushed);
        let half = self.s.w.x * 0.5 * self.scale;
        let reach = MERGE_REACH * self.scale;
        let mut at = (centre_position(m, self.scale, centre) * 1000.0).round() / 1000.0;
        if centre - half - (m.left as f32) < reach {
            at = -1.0;
        } else if (m.right as f32) - (centre + half) < reach {
            at = 1.0;
        }
        self.gliding = true;
        self.invalidate();
        self.cfg.island_x = at;
        config::set(&self.config_path, "island_x", &format!("{at}"));
        if self.s.settings.open {
            self.refresh_settings();
            self.repaint();
        }
    }

    /// Tells the mouse hook where cursor movement should wake the UI thread.
    fn update_zone(&self) {
        let (w, h, _) = self.s.target_dims();
        let (l, r) = (CX - w * 0.5, CX + w * 0.5);
        let (x0, x1, y1) = match self.s.mode {
            Mode::Idle => (l - 36.0, r + 36.0, h + 130.0),
            Mode::Expanded => (l - 44.0, r + 44.0, h + 130.0),
            Mode::Hidden => (CX - 220.0, CX + 220.0, 3.0),
            Mode::Command => (l - 44.0, r + 44.0, h + 60.0),
        };
        let px = |v: f32| (v * self.scale).round() as i32;
        input::set_zone(RECT { left: self.origin.0 + px(x0), top: self.origin.1, right: self.origin.0 + px(x1), bottom: self.origin.1 + px(y1).max(2) });
    }

    // ------------------------------------------------------------- state --

    fn set_mode(&mut self, mode: Mode) {
        if self.s.mode == mode {
            return;
        }
        self.s.ctrl_held = false;
        // A tab or Home item held with Ctrl + drag is let go when the panel goes away.
        if self.tab_grab.is_some() || self.home_grab.is_some() || self.grip.is_some() {
            self.end_tab_drag();
            self.end_home_drag();
            self.end_grip();
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        // A lit button belongs to the view being left.
        self.s.hover = None;
        self.s.hover_old = None;
        self.s.hover_in.snap(0.0);
        self.s.hover_out.snap(0.0);
        let was_hidden = self.s.mode == Mode::Hidden;
        self.s.lean.iter_mut().for_each(|l| l.snap(0.0));
        match (self.s.mode, mode) {
            (Mode::Idle, Mode::Expanded | Mode::Command) => crate::sound::cue(crate::sound::Cue::Open),
            (Mode::Expanded | Mode::Command, Mode::Idle) => crate::sound::cue(crate::sound::Cue::Close),
            _ => {}
        }
        if matches!(self.s.mode, Mode::Expanded | Mode::Command) && !matches!(mode, Mode::Expanded | Mode::Command) {
            self.clean_frames = 2;
        }
        if mode == Mode::Expanded && self.s.reveal == 0.0 {
            self.s.snap_tabs();
        }
        self.s.set_mode(mode);
        self.s.hover_tab = None;
        self.kill(T_COLLAPSE);
        self.kill(T_HOVER);
        self.kill(T_TRIM);
        self.collapse_armed = false;
        self.hover_armed = false;
        match mode {
            Mode::Idle => self.arm_autohide(),
            _ => self.kill(T_AUTOHIDE),
        }
        if mode == Mode::Hidden {
            self.kill(T_CLOCK);
            self.set_interactive(false);
        } else if was_hidden {
            self.arm_clock();
        }
        if mode == Mode::Expanded {
            if self.demo.is_none() {
                if let Some(level) = audio::volume() {
                    self.s.volume = level;
                }
                unsafe {
                    SetTimer(Some(self.hwnd), T_TICK, 1000, None);
                    SetTimer(Some(self.hwnd), T_OUTPUTS, 420, None);
                }
            }
            self.enter_tab();
        } else {
            self.kill(T_TICK);
            self.kill(T_OUTPUTS);
            self.s.device_menu = false;
            self.s.arm_confirm(None);
            self.end_volume_drag();
            self.press = None;
            self.end_note_drag();
            (self.s.dock_hover, self.s.dock_drop, self.s.dock_hint) = (None, false, false);
            self.blur();
            self.save_note();
        }
        self.update_zone();
        self.repaint();
        if mode == Mode::Idle {
            // Alerts that arrived while the panel was open get their turn now.
            self.resolve();
        }
        self.sync_stats();
    }

    /// Samples the system load only while it is on screen: the System tab is
    /// open, or the pill shows it (`stats_pill`). Never while suspended.
    fn sync_stats(&mut self) {
        let shown = (self.s.mode == Mode::Expanded && self.s.tab == 3) || (self.cfg.stats_pill && self.s.mode != Mode::Hidden);
        crate::sysstat::want(self.hwnd, shown && !self.suspended && !self.stats_fake);
    }

    fn end_volume_drag(&mut self) {
        if self.drag_volume {
            self.drag_volume = false;
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    /// Reacts to a click on the open panel or the pill.
    fn click(&mut self, x: f32, y: f32) {
        // A click on an alert dismisses it; on the "Downloaded" card's button it reveals the file.
        if self.s.mode == Mode::Idle && self.s.pill == Pill::Permit {
            // Only the two buttons do anything on the permission card.
            if let Some(action) = ui::hit_permit(&self.s, x, y) {
                self.permit_action(action);
            }
            return;
        }
        if ui::hit_stop_button(&self.s, x, y) {
            self.toggle_recording();
            return;
        }
        // A click on the listening pill ends the dictation.
        if self.s.mode == Mode::Idle && self.s.pill == Pill::Listen {
            crate::voice::stop();
            return;
        }
        // A click on the character: it reacts (the pill stays as it is).
        if self.s.mode == Mode::Idle && self.on_pet(x, y) {
            self.poke_pet();
            return;
        }
        if self.s.mode == Mode::Idle && self.toast.is_some() {
            if ui::hit_done_button(&self.s, x, y) {
                if let Some(done) = self.toast.as_ref().and_then(|t| t.done.as_ref()) {
                    reveal(&done.path);
                }
            }
            self.dismiss_toast();
            return;
        }
        let hit = ui::hit(&self.s, x, y);
        if let Some(cue) = hit.and_then(click_cue) {
            crate::sound::cue(cue);
        }
        // Every button answers a press with a ripple from the pointer.
        if let Some(shape) = hit.filter(|h| ui::hoverable(*h)).and_then(|h| ui::glow_shape(&self.s, h)) {
            // (In the panel the buttons are drawn in its content units.)
            let (x, y) = if self.s.mode == Mode::Expanded { self.s.to_panel(x, y) } else { (x, y) };
            self.s.ripple = Some(crate::state::Ripple { shape, x, y, t: 0.0 });
            self.invalidate();
        }
        if self.s.device_menu && !matches!(hit, Some(Hit::Output(_)) | Some(Hit::Device)) {
            self.s.device_menu = false;
        }
        // Clicking away from a power button waiting for its second press cancels it.
        if self.s.confirm.is_some() && !matches!(hit, Some(Hit::Power(_))) {
            self.s.arm_confirm(None);
        }
        if (self.s.clip_focus && !matches!(hit, Some(Hit::ClipSearch))) || (self.s.note.focus && !matches!(hit, Some(Hit::NoteText))) || (self.s.chat.focus && !matches!(hit, Some(Hit::ChatInput | Hit::ChatSend | Hit::ChatAttach | Hit::ChatUnattach(_)))) {
            self.blur();
        }
        match hit {
            Some(Hit::Tab(i)) => {
                self.save_note();
                self.s.set_tab(i);
                self.enter_tab();
            }
            Some(Hit::Setting(row, part)) => self.settings_click(row, part),
            // A press on a shelf row does nothing by itself: moving the
            // pointer with the button held starts the drag (see WM_MOUSEMOVE).
            Some(Hit::ShelfRow(row)) => self.press = Some((x, y, Some(row))),
            Some(Hit::ShelfAll) => self.press = Some((x, y, None)),
            Some(Hit::ShelfRemove(row)) => {
                let index = self.shelf_scroll + row;
                self.shelf().remove(index);
                self.refresh_shelf();
            }
            Some(Hit::ShelfClear) => {
                self.shelf().clear();
                self.refresh_shelf();
            }
            Some(Hit::NoteFolder) => self.edit_notes(Notes::next_folder),
            Some(Hit::NoteNewFolder) => self.edit_notes(Notes::create_folder),
            Some(Hit::NoteNew) => {
                self.edit_notes(Notes::create);
                self.focus_note();
            }
            Some(Hit::NoteDelete) => self.edit_notes(Notes::delete),
            Some(Hit::NoteRow(row)) => {
                let index = self.note_scroll + row;
                self.edit_notes(|notes| notes.select(index));
            }
            Some(Hit::NoteText) => {
                self.focus_note();
                let extend = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                let (x, y) = self.content_dp(x, y);
                let at = self.gfx.as_ref().and_then(|gfx| gfx.note_hit(&self.s.note.text(), NOTE_TEXT_W, x - NOTE_TEXT_X, y - NOTE_TEXT_Y + self.s.note.scroll));
                if let (Some(at), Some(notes)) = (at, &mut self.notes) {
                    notes.move_to(at as usize, extend);
                }
                self.sync_note(true);
                // Moving the pointer with the button held selects text.
                if self.s.note.focus {
                    self.note_drag = true;
                    unsafe { SetCapture(self.hwnd) };
                }
            }
            Some(Hit::Dock(slot)) => {
                if let (Some(path), false) = (self.dock().items.get(slot).cloned(), self.hidden) {
                    unsafe {
                        ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
                    }
                }
            }
            Some(Hit::DockAdd) => self.s.dock_hint = true,
            Some(Hit::Quick(ui::QUICK_COMMAND)) => self.open_command(),
            Some(Hit::Quick(ui::QUICK_VOICE)) => self.toggle_voice(),
            Some(Hit::Quick(ui::QUICK_TIMER)) => {
                self.command("timer 5 min");
            }
            Some(Hit::Quick(ui::QUICK_NOTES)) => {
                self.s.set_tab(5);
                self.enter_tab();
            }
            Some(Hit::Quick(ui::QUICK_LOCK)) => {
                if !self.hidden {
                    control::power(control::Power::Lock);
                }
            }
            // Plain text: what is on the clipboard goes back as text only, then Ctrl+V.
            Some(Hit::Quick(ui::QUICK_PLAIN)) => {
                if let Some(text) = clipboard::read_text(self.hwnd) {
                    self.clip_ignore = clipboard::write_text(self.hwnd, &text);
                    if self.clip_ignore && !self.hidden {
                        unsafe { SetTimer(Some(self.hwnd), T_PASTE, 80, None) };
                    }
                }
            }
            Some(Hit::Quick(i)) => {
                let tool = match i {
                    ui::QUICK_AREA => Some(snip::Tool::Area),
                    ui::QUICK_FULL => Some(snip::Tool::Full),
                    ui::QUICK_TEXT => Some(snip::Tool::Text),
                    ui::QUICK_QR => Some(snip::Tool::Qr),
                    ui::QUICK_COLOR => Some(snip::Tool::Color),
                    _ => None,
                };
                if let Some(tool) = tool {
                    self.snip(tool);
                } else if i == ui::QUICK_RECORD {
                    self.toggle_recording();
                }
            }
            Some(Hit::Radio(kind)) => {
                let on = match kind {
                    control::Kind::WiFi => self.s.radios.wifi,
                    control::Kind::Bluetooth => self.s.radios.bluetooth,
                };
                if let Some(on) = on {
                    control::refresh(self.hwnd, Some((kind, !on)));
                }
            }
            // A Windows VPN profile is switched; without one the VPN settings open.
            Some(Hit::Vpn) => {
                if crate::vpn::status().profile.is_some() {
                    if !self.hidden {
                        crate::vpn::refresh(self.hwnd, true);
                    }
                    self.sync_vpn();
                } else if !self.hidden {
                    unsafe {
                        ShellExecuteW(None, w!("open"), w!("ms-settings:network-vpn"), None, None, SW_SHOWNORMAL);
                    }
                }
            }
            Some(Hit::ChatInput) => {
                self.s.chat.focus = true;
                self.take_focus();
                self.repaint();
            }
            Some(Hit::ChatSend) => {
                if self.s.chat.busy {
                    self.chat_stop();
                } else {
                    self.chat_submit();
                }
            }
            // The picker is modal: it runs in `main::wndproc`, outside this borrow.
            Some(Hit::ChatAttach) => {
                if !self.hidden && !self.picking {
                    self.picking = true;
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_APP_PICK, WPARAM(0), LPARAM(0));
                    }
                }
            }
            Some(Hit::ChatUnattach(i)) => {
                if i < self.chat_pending.len() {
                    self.chat_pending.remove(i);
                }
                self.sync_chat();
            }
            Some(Hit::ChatCopy(i)) => {
                if let Some(message) = self.chat.as_ref().and_then(|m| m.get(i)) {
                    if !self.hidden {
                        clipboard::write_text(self.hwnd, &message.text);
                    }
                    self.queue.push(Toast::new(events::INFO, 1.6, Icon::Info, "Copied", "", theme::accent()).keyed("info"));
                }
            }
            Some(Hit::ChatRegenerate) => self.chat_regenerate(),
            Some(Hit::ChatNew) => {
                crate::chat::cancel();
                self.chat_pending.clear();
                self.s.chat.busy = false;
                if let Some(messages) = &mut self.chat {
                    messages.clear();
                    if !self.hidden {
                        crate::chat::save(messages);
                    }
                }
                // A new conversation: a new Claude Code session / Codex thread
                // (the old one stays there).
                self.chat_session = None;
                self.save_chat_session();
                self.sync_chat_header();
                self.sync_chat();
            }
            // The model chip: the next model of the provider.
            Some(Hit::ChatModel) => {
                if self.s.settings.models.is_empty() {
                    self.refresh_models();
                }
                self.step_model(true);
                self.sync_chat_header();
            }
            // Opens this conversation in Claude Code / Codex.
            Some(Hit::ChatOpen) => {
                if let (Some(session), false) = (&self.chat_session, self.hidden) {
                    crate::cli::open_session(session);
                }
            }
            // "Switched off" cards open the settings window at their group.
            Some(Hit::OpenConfig) => {
                let group = if self.s.tab == 6 { "Weather and currency" } else { "AI chat" };
                self.open_settings_at(group);
            }
            Some(Hit::WxView(view)) => {
                if self.s.wx.view != view {
                    self.s.wx.view = view;
                    self.s.wx.since = self.s.sky_phase;
                }
                self.repaint();
            }
            Some(Hit::Lyrics) => {
                self.s.lyric.card = !self.s.lyric.card;
                // The list starts at the line being sung, not scrolling there from the top.
                let at = self.s.lyric.index.unwrap_or(0) as f32;
                self.s.lyric.scroll.snap(at);
                self.sync_lyric();
                self.repaint();
            }
            Some(Hit::WxDay(day)) => {
                self.s.wx.day = if self.s.wx.day == Some(day) { None } else { Some(day) };
                self.repaint();
            }
            Some(Hit::WxSky) => {
                let (px, py) = self.content_dp(x, y);
                // In the sky card's own units (the cards may have swapped places).
                let px = px - crate::panels::wx_shift(&self.s).0;
                self.s.wx.poke = Some((px, py, self.s.sky_phase));
                // In a storm the click calls down a bolt, and its thunder right after.
                if crate::panels::weather_stormy(&self.s) {
                    self.s.wx.thunder = Some((0.12, 1.0));
                }
                // A click on the sky goes back to now.
                self.s.wx.day = None;
                self.repaint();
            }
            Some(Hit::Theme) => {
                self.s.dark = !self.s.dark;
                control::set_dark_theme(self.s.dark);
            }
            Some(Hit::Power(action)) => {
                if !action.needs_confirm() || self.s.confirm == Some(action) {
                    self.s.arm_confirm(None);
                    control::power(action);
                } else {
                    self.s.arm_confirm(Some(action));
                }
            }
            Some(Hit::ClipSearch) => {
                self.s.clip_focus = true;
                self.take_focus();
            }
            Some(Hit::ClipClear) => {
                self.history().clear();
                self.clip_scroll = 0;
                self.refresh_clipboard();
            }
            Some(Hit::ClipPin(row)) => {
                if let Some(&index) = self.clip_shown.get(row) {
                    self.history().toggle_pin(index);
                    self.refresh_clipboard();
                }
            }
            Some(Hit::ClipRow(row)) => self.paste_row(row),
            Some(Hit::CmdRow(row)) => self.run_command_row(row),
            Some(Hit::Pin) => {
                let pinned = !self.s.pinned;
                self.s.set_pinned(pinned);
            }
            Some(Hit::Settings) => self.open_settings(true),
            // Only in the settings window (see `on_settings_message`).
            Some(Hit::SettingsSearch | Hit::SettingsGroup(_) | Hit::ColourPick(_)) => {}
            Some(Hit::Previous) => self.media_cmd(Cmd::Previous),
            Some(Hit::Toggle) => self.media_cmd(Cmd::Toggle),
            Some(Hit::Next) => self.media_cmd(Cmd::Next),
            Some(Hit::Seek(share)) => {
                // Shown at once; the player's own report follows.
                let m = &mut self.s.media;
                let to = share * m.duration;
                (m.position, m.stamp) = (to, Instant::now());
                self.media_cmd(Cmd::Seek(to));
                self.sync_lyric();
                self.repaint();
            }
            Some(Hit::Shuffle) => {
                if let Some(on) = self.s.media.can.shuffle {
                    self.s.media.can.shuffle = Some(!on);
                    self.media_cmd(Cmd::Shuffle(!on));
                    self.repaint();
                }
            }
            Some(Hit::Repeat) => {
                // Off -> whole list -> this track -> off.
                if let Some(mode) = self.s.media.can.repeat {
                    let next = match mode { 0 => 2, 2 => 1, _ => 0 };
                    self.s.media.can.repeat = Some(next);
                    self.media_cmd(Cmd::Repeat(next));
                    self.repaint();
                }
            }
            Some(Hit::Volume(level)) => {
                self.drag_volume = true;
                unsafe { SetCapture(self.hwnd) };
                self.set_volume(level);
            }
            Some(Hit::Device) => {
                self.s.device_menu = !self.s.device_menu;
                if self.s.device_menu {
                    self.s.outputs = audio::outputs();
                }
            }
            Some(Hit::Output(i)) => {
                self.s.device_menu = false;
                if let Some(output) = self.s.outputs.get(i) {
                    if output.offline {
                        // A paired Bluetooth device: ask its driver to connect. Windows
                        // makes it the output by itself once it answers.
                        let name = output.name.clone();
                        self.queue.push(Toast::new(events::INFO, 3.0, Icon::Info, "Connecting", String::from_utf16_lossy(&name), theme::accent()).keyed("info"));
                        if !self.hidden {
                            audio::connect_bluetooth(name);
                        }
                    } else if !output.current && audio::set_default_output(&output.id) {
                        // The loopback stream must follow the new device.
                        self.capture = None;
                        self.sync_audio();
                    }
                }
                self.s.outputs = audio::outputs();
                if let Some(level) = audio::volume() {
                    self.s.volume = level;
                }
            }
            None => {
                if self.s.mode == Mode::Idle {
                    self.set_mode(Mode::Expanded);
                }
            }
        }
        self.repaint();
    }

    fn media_cmd(&self, cmd: Cmd) {
        if let Some(media) = &self.media {
            media.send(cmd);
        }
    }

    fn arm_autohide(&self) {
        if self.cfg.autohide && self.demo.is_none() && self.s.pill == Pill::Clock {
            unsafe { SetTimer(Some(self.hwnd), T_AUTOHIDE, AUTOHIDE_MS, None) };
        }
    }

    fn kill(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    /// Reads the wall clock and schedules one wake-up at the next minute boundary.
    fn arm_clock(&mut self) {
        let now = unsafe { GetLocalTime() };
        self.s.time = (now.wHour, now.wMinute);
        if !self.suspended && self.s.mode != Mode::Hidden {
            let ms = (60 - now.wSecond.min(59) as u32) * 1000 - now.wMilliseconds.min(999) as u32 + 30;
            unsafe { SetCoalescableTimer(Some(self.hwnd), T_CLOCK, ms, None, 250) };
        }
    }

    fn apply_theme(&mut self, animate: bool) {
        if self.demo.is_some() {
            return;
        }
        let light = match self.cfg.theme {
            ThemeMode::Dark => false,
            ThemeMode::Light => true,
            ThemeMode::System => theme::system_is_light(),
        };
        let target = if light { 1.0 } else { 0.0 };
        if animate {
            self.s.theme.set(target);
        } else {
            self.s.theme.snap(target);
        }
    }

    /// Toggles mouse transparency of the whole window. The island only takes
    /// mouse input while the cursor is over its shape; everywhere else clicks
    /// fall through to the apps below.
    fn set_interactive(&mut self, on: bool) {
        if self.interactive == on {
            return;
        }
        self.interactive = on;
        unsafe {
            let mut ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            if on {
                ex &= !(WS_EX_TRANSPARENT.0 as isize);
            } else {
                ex |= WS_EX_TRANSPARENT.0 as isize;
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex);
        }
    }

    fn cursor_dp(&self) -> (f32, f32) {
        if let Some(fake) = self.fake_cursor {
            return fake;
        }
        let (x, y) = input::cursor();
        ((x - self.origin.0) as f32 / self.scale, (y - self.origin.1) as f32 / self.scale)
    }

    /// Hover / reach / collapse logic driven by the real cursor.
    fn track_cursor(&mut self) {
        let (x, y) = self.cursor_dp();
        self.s.cursor = (x, y);
        self.s.reach();
        let shape = self.s.shape();
        let inside = shape.contains(x, y, 0.0);
        match self.s.mode {
            Mode::Idle => {
                // The permission card stays put under the cursor so its buttons can be clicked.
                // Resting on the character does not open the panel either (it can be stroked).
                let on_pet = self.on_pet(x, y);
                // A held Ctrl is about to move the island: no panel opening then.
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 || self.island_drag.is_some();
                if inside && !on_pet && !ctrl && !self.hover_armed && self.s.pill != Pill::Permit {
                    self.hover_armed = true;
                    unsafe { SetTimer(Some(self.hwnd), T_HOVER, HOVER_MS, None) };
                } else if (!inside || on_pet) && self.hover_armed {
                    self.hover_armed = false;
                    self.kill(T_HOVER);
                }
            }
            Mode::Expanded => {
                // Dragging the volume slider keeps the panel open even outside its shape.
                // Typing in a search field does too.
                // Dragging a file out of the shelf does as well.
                let near = shape.contains(x, y, 16.0) || self.drag_volume || self.focused || self.dragging.is_some() || self.picking || self.tab_grab.is_some() || self.home_grab.is_some() || self.grip.is_some();
                if near && self.collapse_armed {
                    self.collapse_armed = false;
                    self.kill(T_COLLAPSE);
                } else if !near && !self.collapse_armed && !self.s.pinned {
                    self.collapse_armed = true;
                    unsafe { SetTimer(Some(self.hwnd), T_COLLAPSE, COLLAPSE_MS, None) };
                }
                if !inside && self.s.hover_tab.is_some() {
                    self.s.hover_tab = None;
                    self.s.repaint = true;
                }
                // The pointer left the island: the lit button fades out, the card straightens.
                if !inside && self.s.hover.is_some() {
                    self.s.set_hover(None);
                    self.invalidate();
                }
                if !inside && self.s.lean.iter().any(|l| l.target != 0.0) {
                    self.s.lean.iter_mut().for_each(|l| l.set(0.0));
                    self.invalidate();
                }
            }
            Mode::Hidden => {
                if (-1.0..=3.0).contains(&y) && (x - CX).abs() < 220.0 {
                    self.set_mode(Mode::Idle);
                }
            }
            // The command bar stays until Esc, Enter or a click elsewhere.
            Mode::Command => {}
        }
        // Anything being dragged keeps the island taking the mouse even off its
        // shape: a click-through window gets no more moves (the held item froze
        // and its release went to the app below).
        self.set_interactive((inside || self.dragging_something()) && !self.cfg.click_through && self.s.mode != Mode::Hidden);
    }

    /// A slider, tab, Home item, card or the island itself is being dragged.
    fn dragging_something(&self) -> bool {
        self.drag_volume || self.tab_grab.is_some() || self.home_grab.is_some() || self.grip.is_some() || self.island_drag.is_some()
    }

    // ------------------------------------------------------------- tools --

    /// Loads what the tab that just became visible needs.
    fn enter_tab(&mut self) {
        self.sync_stats();
        if self.s.mode != Mode::Expanded || self.demo.is_some() {
            return;
        }
        match self.s.tab {
            // Icons that are not loaded yet follow from `T_OUTPUTS`, off the opening animation.
            0 => self.refresh_dock(false),
            1 => self.refresh_clipboard(),
            2 => {
                self.shelf().prune();
                self.refresh_shelf();
            }
            5 => {
                self.open_notes();
                self.sync_note(true);
            }
            4 => self.open_chat(),
            6 => {
                // The view plays its entry animation; the frame loop does the rest.
                self.s.wx.since = self.s.sky_phase;
                self.invalidate();
            }
            7 => {
                self.s.dark = control::dark_theme();
                self.s.radios = control::radios();
                control::refresh(self.hwnd, None);
                crate::vpn::refresh(self.hwnd, false);
                self.sync_vpn();
            }
            _ => {}
        }
    }

    /// The clipboard history, loaded from disk on first use.
    fn history(&mut self) -> &mut History {
        let hidden = self.hidden;
        self.history.get_or_insert_with(|| if hidden { History::memory_only() } else { History::load() })
    }

    /// Rebuilds the visible clipboard rows from the history, search text and scroll position.
    fn refresh_clipboard(&mut self) {
        let query = String::from_utf16_lossy(&self.s.clip_query);
        let matching = self.history().matching(&query);
        self.clip_scroll = self.clip_scroll.min(matching.len().saturating_sub(CLIP_ROWS));
        self.clip_shown = matching.iter().skip(self.clip_scroll).take(CLIP_ROWS).copied().collect();
        let mut rows: Vec<ListRow> = Vec::new();
        for i in self.clip_shown.clone() {
            let Some(item) = self.history.as_ref().and_then(|h| h.items.get(i)).cloned() else { continue };
            let Some(picture) = item.image else {
                // One line: whitespace runs (including line breaks) become single spaces.
                let line = item.text.split_whitespace().collect::<Vec<_>>().join(" ");
                rows.push(ListRow { title: line.encode_utf16().take(110).collect(), marked: item.pinned, ..Default::default() });
                continue;
            };
            // Thumbnails are only kept for the rows on screen.
            if !self.thumbs.iter().any(|(key, _)| *key == picture.key) {
                if let Some(thumb) = self.history.as_ref().and_then(|h| h.thumb(picture.key)) {
                    self.keep_thumb(picture.key, thumb);
                }
            }
            let title = format!("Image \u{00B7} {} \u{00D7} {}", picture.w, picture.h);
            rows.push(ListRow { title: title.encode_utf16().collect(), marked: item.pinned, image: picture.key, ..Default::default() });
        }
        if let Some(gfx) = &self.gfx {
            for (key, thumb) in &self.thumbs {
                gfx.set_image(*key, &thumb.bgra, thumb.w, thumb.h);
            }
        }
        self.s.clip_rows = rows;
        self.s.clip_total = matching.len();
        self.s.clip_first = self.clip_scroll;
        self.repaint();
    }

    /// Remembers a thumbnail for drawing (the oldest ones make room).
    fn keep_thumb(&mut self, key: u64, thumb: Pixels) {
        self.thumbs.retain(|(k, _)| *k != key);
        if self.thumbs.len() >= 12 {
            self.thumbs.remove(0);
        }
        self.thumbs.push((key, thumb));
    }

    /// The clipboard changed: record the text (or, without text, the picture)
    /// unless Islet put it there itself.
    fn on_clipboard(&mut self) {
        if std::mem::take(&mut self.clip_ignore) {
            return;
        }
        if let Some(text) = clipboard::read_text(self.hwnd) {
            self.history().push(&text);
            if self.s.mode == Mode::Expanded && self.s.tab == 1 {
                self.refresh_clipboard();
            }
        } else if let Some(dib) = clipboard::read_dib(self.hwnd) {
            // Encoding a large picture takes a moment: a worker does it and reports back.
            let dir = self.history().image_dir();
            clipboard::record_image(self.hwnd, dib, dir);
        }
    }

    /// The worker stored copied pictures: list them.
    fn on_clip_images(&mut self) {
        for (picture, thumb) in clipboard::take_stored() {
            self.history().push_image(picture);
            // Otherwise the thumbnail is read from disk when the tab opens
            // (off-screen tests have no disk copy).
            if (self.s.mode == Mode::Expanded && self.s.tab == 1) || self.hidden {
                self.keep_thumb(picture.key, thumb);
            }
        }
        if self.s.mode == Mode::Expanded && self.s.tab == 1 {
            self.refresh_clipboard();
        }
    }

    /// Puts a history row back on the clipboard and pastes it into the app the
    /// user is working in. With Shift held it is only copied.
    fn paste_row(&mut self, row: usize) {
        let Some(index) = self.clip_shown.get(row).copied() else { return };
        let Some(item) = self.history().items.get(index).cloned() else { return };
        if let Some(picture) = item.image {
            let Some(pixels) = self.history().full(picture.key) else { return };
            self.clip_ignore = clipboard::write_dib(self.hwnd, &image::to_dib(&pixels));
            self.history().push_image(picture);
        } else {
            self.clip_ignore = clipboard::write_text(self.hwnd, &item.text);
            self.history().push(&item.text);
        }
        self.clip_scroll = 0;
        self.refresh_clipboard();
        let copy_only = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        if !copy_only && !self.hidden {
            // Give the target app a moment to be the focused window again.
            unsafe { SetTimer(Some(self.hwnd), T_PASTE, 80, None) };
        }
    }

    // ------------------------------------------------------------- shelf --

    /// The file shelf, loaded from disk on first use.
    fn shelf(&mut self) -> &mut Shelf {
        let hidden = self.hidden;
        self.shelf.get_or_insert_with(|| if hidden { Shelf::memory_only() } else { Shelf::load() })
    }

    /// Rebuilds the visible shelf rows.
    fn refresh_shelf(&mut self) {
        let wide = |s: &str, max: usize| s.encode_utf16().take(max).collect::<Vec<u16>>();
        let total = self.shelf().items.len();
        self.shelf_scroll = self.shelf_scroll.min(total.saturating_sub(SHELF_ROWS));
        let scroll = self.shelf_scroll;
        let rows = self.shelf().items.iter().skip(scroll).take(SHELF_ROWS).map(|item| ListRow { title: wide(&item.name, 90), detail: wide(&item.detail(), 16), tag: wide(&item.badge(), 4), ..Default::default() }).collect();
        self.s.shelf_rows = rows;
        self.s.shelf_total = total;
        self.s.shelf_first = scroll;
        self.repaint();
    }

    // -------------------------------------------------------------- dock --

    /// The Home tab's dock, loaded from disk on first use.
    fn dock(&mut self) -> &mut Dock {
        let hidden = self.hidden;
        self.dock.get_or_insert_with(|| if hidden { Dock::memory_only() } else { Dock::load() })
    }

    /// Rebuilds the dock row. With `load`, also fetches the icons that are
    /// missing (a few ms each, once per run).
    fn refresh_dock(&mut self, load: bool) {
        let items = self.dock().items.clone();
        if load {
            for path in &items {
                let key = dock::key(path);
                if !self.dock_icons.iter().any(|(k, _)| *k == key) {
                    if let Some(icon) = dock::icon(path) {
                        self.dock_icons.push((key, icon));
                    }
                }
            }
            self.dock_icons.retain(|(key, _)| items.iter().any(|p| dock::key(p) == *key));
        }
        if let Some(gfx) = &self.gfx {
            for (key, icon) in &self.dock_icons {
                gfx.set_image(*key, &icon.bgra, icon.w, icon.h);
            }
        }
        self.s.dock = items.iter().map(|path| ListRow { title: dock::label(path).encode_utf16().take(40).collect(), image: dock::key(path), ..Default::default() }).collect();
        self.s.dock_hover = None;
        self.repaint();
    }

    /// Files are being dragged over the island, or were just dropped on it.
    /// They go to the shelf, or to the dock when dropped on the Home tab's dock row.
    fn on_shelf(&mut self, what: usize, lparam: LPARAM) {
        let dropped = if what == shelf::DROPPED { shelf::take_dropped() } else { Vec::new() };
        if self.suspended || self.demo.is_some() {
            return;
        }
        let (sx, sy) = shelf::point_of(lparam);
        let (x, y) = ((sx - self.origin.0) as f32 / self.scale, (sy - self.origin.1) as f32 / self.scale);
        if what == shelf::MOVED {
            // Holding the drag over the Home, Shelf or Chat tab switches to it.
            if let Some(Hit::Tab(tab @ (0 | 2 | 4))) = ui::hit(&self.s, x, y) {
                if tab != self.s.tab && (tab != 4 || self.cfg.chat) {
                    self.save_note();
                    self.blur();
                    self.s.set_tab(tab);
                    self.enter_tab();
                    self.refresh_dock(true);
                }
            }
        }
        // Files dropped on an open Chat tab become attachments.
        if self.s.mode == Mode::Expanded && self.s.tab == 4 && self.s.chat.enabled {
            if what == shelf::DROPPED {
                self.chat_attach(dropped);
            }
            return;
        }
        let home = self.s.mode == Mode::Expanded && self.s.tab == 0;
        let on_dock = home && what != shelf::LEFT && ui::dock_contains(&self.s, x, y);
        if what == shelf::DROPPED && on_dock {
            self.s.dock_drop = false;
            self.dock().add(dropped);
            self.refresh_dock(true);
            return;
        }
        let aiming = on_dock && what != shelf::DROPPED;
        if aiming != self.s.dock_drop {
            self.s.dock_drop = aiming;
            self.repaint();
        }
        // An open Home tab stays, so the dock can be reached; a drop elsewhere on it still shelves.
        if matches!(what, shelf::MOVED | shelf::LEFT) || (what == shelf::ENTERED && home) {
            return;
        }
        if !dropped.is_empty() {
            self.shelf().add(dropped);
            self.shelf_scroll = 0;
        }
        // Show the shelf, unless something more urgent owns the island.
        if self.s.mode == Mode::Command || (self.s.mode == Mode::Idle && self.s.pill == Pill::Permit) {
            return;
        }
        if self.s.tab != 2 {
            self.save_note();
            self.blur();
            self.s.set_tab(2);
        }
        if self.s.mode != Mode::Expanded {
            self.set_mode(Mode::Expanded);
        }
        self.shelf().prune();
        self.refresh_shelf();
    }

    /// The files of the drag that `WM_APP_DRAG` is about to run.
    pub fn take_drag(&mut self) -> Option<Vec<PathBuf>> {
        self.dragging.clone().filter(|paths| !paths.is_empty())
    }

    /// The drag ended; `taken` when the files were dropped somewhere.
    pub fn drag_done(&mut self, taken: bool) {
        let paths = self.dragging.take().unwrap_or_default();
        if taken {
            // They have arrived where they were needed: off the shelf.
            let shelf = self.shelf();
            for path in &paths {
                if let Some(index) = shelf.items.iter().position(|i| i.path == *path) {
                    shelf.remove(index);
                }
            }
        }
        self.shelf().prune();
        self.refresh_shelf();
        if !self.suspended {
            self.track_cursor();
        }
    }

    /// The pointer moved with the button held after a press on a shelf row:
    /// past a few dp this becomes a drag.
    fn maybe_drag(&mut self, x: f32, y: f32, button_down: bool) {
        let Some((px, py, which)) = self.press else { return };
        if !button_down {
            self.press = None;
            return;
        }
        if (x - px).hypot(y - py) < 5.0 || self.hidden {
            return;
        }
        self.press = None;
        let scroll = self.shelf_scroll;
        let items = &self.shelf().items;
        let paths: Vec<PathBuf> = match which {
            Some(row) => items.get(scroll + row).map(|i| i.path.clone()).into_iter().collect(),
            None => items.iter().map(|i| i.path.clone()).collect(),
        };
        if !paths.is_empty() {
            self.dragging = Some(paths);
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP_DRAG, WPARAM(0), LPARAM(0));
            }
        }
    }

    // ------------------------------------------------------------- notes --

    fn open_notes(&mut self) {
        if self.notes.is_none() {
            self.notes = match (&self.notes_dir, self.hidden) {
                (Some(dir), _) => Some(Notes::at(dir.clone(), true)),
                // Off-screen tests never touch the user's notes.
                (None, true) => Some(Notes::at(std::env::temp_dir().join("islet-notes-hidden"), true)),
                (None, false) => Notes::load(),
            };
            self.note_scroll = 0;
            self.s.note.scroll = 0.0;
        }
    }

    /// Runs a list operation (new, delete, switch) and shows the result.
    fn edit_notes(&mut self, action: impl FnOnce(&mut Notes)) {
        self.open_notes();
        if let Some(notes) = &mut self.notes {
            action(notes);
        }
        self.s.note.scroll = 0.0;
        self.sync_note(true);
    }

    fn end_note_drag(&mut self) {
        if std::mem::take(&mut self.note_drag) {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    /// Gives the editor the keyboard.
    fn focus_note(&mut self) {
        if self.notes.as_ref().is_some_and(|n| n.open.is_some()) {
            self.s.note.focus = true;
            self.take_focus();
            self.repaint();
        }
    }

    /// Writes the open note to disk if it changed.
    fn save_note(&mut self) {
        self.kill(T_NOTE);
        if let Some(notes) = &mut self.notes {
            notes.save();
        }
    }

    /// Copies the notes model into what the tab draws. With `reveal`, scrolls
    /// so the caret is visible.
    fn sync_note(&mut self, reveal: bool) {
        let wide = |s: &str, max: usize| s.encode_utf16().take(max).collect::<Vec<u16>>();
        let Some(notes) = &self.notes else {
            self.s.note = NoteView::default();
            return;
        };
        self.note_scroll = self.note_scroll.min(notes.list.len().saturating_sub(NOTE_ROWS));
        // Keep the open note's title in the visible part of the list.
        if let Some(open) = notes.open {
            if reveal && !(self.note_scroll..self.note_scroll + NOTE_ROWS).contains(&open) {
                self.note_scroll = open.min(notes.list.len().saturating_sub(NOTE_ROWS));
            }
        }
        let view = &mut self.s.note;
        view.folder = wide(notes.folder_label(), 14);
        view.rows = notes.list.iter().enumerate().skip(self.note_scroll).take(NOTE_ROWS).map(|(i, entry)| ListRow { title: wide(&entry.title, 40), marked: notes.open == Some(i), ..Default::default() }).collect();
        view.total = notes.list.len();
        view.open = notes.open.is_some();
        if view.serial != notes.serial {
            view.text = notes.text.clone();
            view.spans = notes::style(&notes.text);
            view.serial = notes.serial;
        }
        view.caret = notes.caret as u32;
        view.selection = notes.selection().map(|(a, b)| (a as u32, b as u32));
        if !view.open {
            view.focus = false;
        }
        if let Some(gfx) = &self.gfx {
            // Never scrolled past the end; the caret stays in view after an edit.
            let end = gfx.note_caret(&view.text(), NOTE_TEXT_W, view.text.len() as u32).map_or(0.0, |(_, y, h)| y + h);
            view.scroll = view.scroll.min((end - NOTE_TEXT_H).max(0.0));
            if let (true, Some((_, y, h))) = (reveal, gfx.note_caret(&view.text(), NOTE_TEXT_W, view.caret)) {
                if y < view.scroll {
                    view.scroll = y;
                } else if y + h > view.scroll + NOTE_TEXT_H {
                    view.scroll = y + h - NOTE_TEXT_H;
                }
            }
        }
        self.repaint();
    }

    /// A key press in the note editor. Returns `false` for keys it does not use.
    fn note_key(&mut self, key: u32) -> bool {
        let (shift, ctrl) = unsafe { (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_CONTROL.0 as i32) < 0) };
        let Some(notes) = &mut self.notes else { return false };
        let before = notes.serial;
        match (key, ctrl) {
            // Escape
            (0x1B, _) => {
                self.blur();
                self.save_note();
                self.repaint();
                return true;
            }
            (0x08, _) => notes.backspace(),
            (0x2E, _) => notes.delete_forward(),
            (0x0D, _) => notes.enter(),
            (0x25, _) => notes.left(shift),
            (0x27, _) => notes.right(shift),
            (0x24, _) => notes.home(shift),
            (0x23, _) => notes.end(shift),
            // Up / Down: the position above or below the caret in the wrapped layout.
            (0x26 | 0x28, _) => {
                let view = self.s.note.text();
                if let Some((x, y, h)) = self.gfx.as_ref().and_then(|gfx| gfx.note_caret(&view, NOTE_TEXT_W, notes.caret as u32)) {
                    let target = if key == 0x26 { y - 4.0 } else { y + h + 4.0 };
                    if target < 0.0 {
                        notes.move_to(0, shift);
                    } else if let Some(at) = self.gfx.as_ref().and_then(|gfx| gfx.note_hit(&view, NOTE_TEXT_W, x, target)) {
                        notes.move_to(at as usize, shift);
                    }
                }
            }
            // Ctrl+Z, Ctrl+A / C / X / V, Ctrl+B (bold), Ctrl+I (italic)
            (0x5A, true) => notes.undo(),
            (0x41, true) => notes.select_all(),
            (0x43 | 0x58, true) => {
                let text = notes.selected_text();
                if !text.is_empty() {
                    clipboard::write_text(self.hwnd, &text);
                    if key == 0x58 {
                        notes.backspace();
                    }
                }
            }
            (0x56, true) => {
                if let Some(text) = clipboard::read_text(self.hwnd) {
                    notes.insert(&text.encode_utf16().collect::<Vec<_>>());
                }
            }
            (0x42, true) => notes.wrap("**"),
            (0x49, true) => notes.wrap("*"),
            _ => return false,
        }
        if notes.serial != before {
            unsafe { SetTimer(Some(self.hwnd), T_NOTE, 1500, None) };
        }
        self.sync_note(true);
        true
    }

    /// Takes the keyboard away from the search field and the note editor.
    fn blur(&mut self) {
        if self.s.settings.editing.is_some() || self.s.settings.capturing.is_some() {
            self.settings_finish(true);
            self.release_focus();
        }
        if self.s.clip_focus || self.s.note.focus || self.s.chat.focus {
            self.s.clip_focus = false;
            self.s.note.focus = false;
            self.s.chat.focus = false;
            self.release_focus();
        }
    }

    /// Becomes the active window so the keyboard reaches Islet (command bar,
    /// search field, note editor). The window never activates on its own.
    fn take_focus(&mut self) {
        if self.focused || self.hidden {
            self.focused = true;
            return;
        }
        self.focused = true;
        unsafe {
            self.previous_window = GetForegroundWindow();
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    /// Gives the keyboard back to the window that had it.
    fn release_focus(&mut self) {
        if !self.focused {
            return;
        }
        self.focused = false;
        if self.hidden {
            return;
        }
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            if GetForegroundWindow() == self.hwnd && IsWindow(Some(self.previous_window)).as_bool() {
                let _ = SetForegroundWindow(self.previous_window);
            }
        }
    }

    /// The user switched to another window while Islet had the keyboard.
    fn on_focus_lost(&mut self) {
        if !self.focused {
            return;
        }
        self.focused = false;
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        }
        if self.s.settings.editing.is_some() || self.s.settings.capturing.is_some() {
            self.settings_finish(true);
            self.repaint();
        }
        if self.s.mode == Mode::Command {
            self.close_command();
        } else if self.s.clip_focus || self.s.note.focus || self.s.chat.focus {
            self.s.clip_focus = false;
            self.s.note.focus = false;
            self.s.chat.focus = false;
            self.save_note();
            self.repaint();
        }
    }

    fn register_hotkey(&mut self) {
        if self.demo.is_some() || self.hidden || self.hotkey_on {
            return;
        }
        let (modifiers, key) = self.cfg.hotkey;
        self.hotkey_on = unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY_COMMAND, HOT_KEY_MODIFIERS(modifiers) | MOD_NOREPEAT, key) }.is_ok();
        if !self.hotkey_on && !self.hotkey_warned {
            self.hotkey_warned = true;
            self.queue.push(Toast::new(events::INFO, 7.0, Icon::Info, "Command bar shortcut is taken", "Another app uses it. Change \u{201C}hotkey\u{201D} in the settings file.", Rgba::hex(0xff9f0a)));
        }
        if self.cfg.voice != "off" {
            let (modifiers, key) = self.cfg.voice_hotkey;
            let taken = unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY_VOICE, HOT_KEY_MODIFIERS(modifiers) | MOD_NOREPEAT, key) }.is_err();
            if taken && !self.hotkey_warned {
                self.hotkey_warned = true;
                self.queue.push(Toast::new(events::INFO, 7.0, Icon::Info, "Voice shortcut is taken", "Another app uses it. Change \u{201C}voice_hotkey\u{201D} in the settings file.", Rgba::hex(0xff9f0a)));
            }
        }
    }

    fn unregister_hotkey(&mut self) {
        if std::mem::take(&mut self.hotkey_on) {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_COMMAND);
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_VOICE);
            }
        }
    }

    // ---------------------------------------------------------- settings --

    /// Opens the settings window (or brings it to the front), or closes it.
    pub fn open_settings(&mut self, open: bool) {
        if open {
            if let Some(win) = &self.settings_win {
                unsafe {
                    let _ = ShowWindow(win.hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(win.hwnd);
                }
                return;
            }
            self.settings_finish(true);
            let v = &mut self.s.settings;
            v.open = true;
            v.scroll = 0.0;
            v.search.clear();
            v.search_focus = true;
            v.hover = None;
            self.blur();
            self.refresh_settings();
            // Whether the Claude / ChatGPT logins work, and which models the
            // provider has (both on workers).
            crate::cli::check_login(self.hwnd);
            self.refresh_models();
            self.create_settings_window();
        } else {
            self.settings_finish(true);
            self.s.settings.open = false;
            self.s.settings.signing = None;
            self.kill(T_CLAUDE);
            if let Some(win) = self.settings_win.take() {
                unsafe {
                    let _ = DestroyWindow(win.hwnd);
                }
            }
        }
        self.repaint();
    }

    /// Opens the settings window at a group ("AI chat", "Weather and currency" ...).
    fn open_settings_at(&mut self, group: &str) {
        self.open_settings(true);
        if let Some(k) = crate::settings::categories().iter().position(|c| c.1 == group) {
            let v = &mut self.s.settings;
            (v.category, v.scroll) = (k, 0.0);
            v.search.clear();
            v.refilter();
        }
        self.redraw_settings();
    }

    /// Creates the settings window (centred on the island's monitor) and its renderer.
    fn create_settings_window(&mut self) {
        use crate::anim::clamp01;
        use crate::settings::{WIN_H_DP, WIN_W_DP};
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else { return };
            // The monitor's own DPI (the island's size setting does not apply here).
            let scale = (self.scale / self.cfg.scale).max(0.5);
            let (cw, ch) = ((WIN_W_DP * scale).round() as i32, (WIN_H_DP * scale).round() as i32);
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW;
            let mut frame = RECT { left: 0, top: 0, right: cw, bottom: ch };
            let _ = AdjustWindowRectExForDpi(&mut frame, style, false, ex, (96.0 * scale).round() as u32);
            let (ww, wh) = (frame.right - frame.left, frame.bottom - frame.top);
            let m = self.monitor;
            let (x, y) = (m.left + ((m.right - m.left) - ww) / 2, m.top + ((m.bottom - m.top) - wh) / 2);
            let Ok(hwnd) = CreateWindowExW(ex, w!("IsletSettings"), w!("Islet settings"), style, x, y, ww, wh, None, None, Some(module.into()), None) else { return };
            // A dark title bar to go with the dark theme.
            let dark: u32 = (theme::palette(clamp01(self.s.theme.x)).light < 0.5) as u32;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            let gfx = Gfx::new(hwnd, cw as u32, ch as u32, scale, self.cfg.gpu).ok();
            self.settings_win = Some(SettingsWin { hwnd, gfx, scale });
            if !self.hidden {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        self.redraw_settings();
    }

    /// Draws the settings window now.
    fn redraw_settings(&mut self) {
        let Some(win) = &self.settings_win else { return };
        let Some(g) = &win.gfx else { return };
        let pal = theme::palette(crate::anim::clamp01(self.s.theme.x));
        g.begin_frame();
        g.begin_pass(g.bounds());
        crate::settings::draw_window(g, &self.s.settings, &pal);
        g.end_pass();
        if g.end_frame(None).is_err() {
            // Device lost: a new renderer for the next paint.
            let (hwnd, scale) = (win.hwnd, win.scale);
            let (w, h) = ((crate::settings::WIN_W_DP * scale).round() as u32, (crate::settings::WIN_H_DP * scale).round() as u32);
            if let Some(win) = &mut self.settings_win {
                win.gfx = Gfx::new(hwnd, w, h, scale, self.cfg.gpu).ok();
            }
        }
    }

    /// Asks the settings window to repaint (coalesced into one `WM_PAINT`).
    fn invalidate_settings(&self) {
        if let Some(win) = &self.settings_win {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(win.hwnd), None, false);
            }
        }
    }

    /// Messages of the settings window.
    pub fn on_settings_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let scale = self.settings_win.as_ref()?.scale;
        let at = |l: LPARAM| ((l.0 & 0xffff) as i16 as f32 / scale, ((l.0 >> 16) & 0xffff) as i16 as f32 / scale);
        match msg {
            WM_PAINT => {
                self.redraw_settings();
                if let Some(win) = &self.settings_win {
                    unsafe {
                        let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(win.hwnd), None);
                    }
                }
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_CLOSE => {
                self.open_settings(false);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = at(lparam);
                let hit = crate::settings::window_hit(&self.s.settings, x, y);
                if !matches!(hit, Some(Hit::Setting(..))) {
                    self.settings_finish(true);
                }
                self.s.settings.search_focus = matches!(hit, Some(Hit::SettingsSearch));
                match hit {
                    Some(Hit::SettingsGroup(k)) => {
                        let v = &mut self.s.settings;
                        (v.category, v.scroll, v.picker) = (k, 0.0, None);
                        v.search.clear();
                        v.refilter();
                    }
                    Some(Hit::Setting(row, part)) => self.settings_click(row, part),
                    // The picker: drag in the square or along the strip; "Done" closes it.
                    Some(Hit::ColourPick(part @ (0 | 1))) => {
                        if let Some(p) = self.s.settings.picker.as_mut() {
                            p.drag = Some(part);
                        }
                        if let Some(win) = &self.settings_win {
                            unsafe { SetCapture(win.hwnd) };
                        }
                        self.picker_move(x, y);
                    }
                    Some(Hit::ColourPick(_)) => {
                        self.s.settings.picker = None;
                        self.s.settings.refilter();
                    }
                    _ => {}
                }
                self.invalidate_settings();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.s.settings.picker.is_some_and(|p| p.drag.is_some()) => {
                let (x, y) = at(lparam);
                self.picker_move(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED if self.s.settings.picker.is_some_and(|p| p.drag.is_some()) => {
                // Let go: the colour is written to the settings file.
                if let Some(p) = self.s.settings.picker.as_mut() {
                    p.drag = None;
                }
                if msg == WM_LBUTTONUP {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                self.picker_commit();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = at(lparam);
                let hit = crate::settings::window_hit(&self.s.settings, x, y);
                if hit != self.s.settings.hover {
                    self.s.settings.hover = hit;
                    self.invalidate_settings();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let up = ((wparam.0 >> 16) & 0xffff) as i16 > 0;
                let most = crate::settings::most_scroll(&self.s.settings);
                let v = &mut self.s.settings;
                v.scroll = (v.scroll + if up { -60.0 } else { 60.0 }).clamp(0.0, most);
                self.invalidate_settings();
                Some(LRESULT(0))
            }
            // Alt shortcuts arrive as system keys (and Alt + Space would open the window menu).
            WM_SYSKEYDOWN | WM_SYSCHAR if self.s.settings.capturing.is_some() => {
                if msg == WM_SYSKEYDOWN {
                    self.settings_key(wparam.0 as u32);
                    self.invalidate_settings();
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                let v = &self.s.settings;
                if v.capturing.is_some() || v.editing.is_some() {
                    self.settings_key(key);
                } else if ctrl && key == 0x46 {
                    // Ctrl + F: to the search field.
                    self.s.settings.search_focus = true;
                } else if self.s.settings.search_focus {
                    let v = &mut self.s.settings;
                    match (key, ctrl) {
                        (0x08, _) => {
                            v.search.pop();
                        }
                        (0x1B, _) if !v.search.is_empty() => v.search.clear(),
                        (0x1B, _) => {
                            self.open_settings(false);
                            return Some(LRESULT(0));
                        }
                        (0x56, true) => {
                            if let Some(text) = clipboard::read_text(self.hwnd) {
                                v.search.extend(text.replace(['\r', '\n'], " ").encode_utf16().take(60));
                            }
                        }
                        _ => return None,
                    }
                    let v = &mut self.s.settings;
                    v.scroll = 0.0;
                    v.refilter();
                } else if key == 0x1B {
                    self.open_settings(false);
                    return Some(LRESULT(0));
                } else {
                    return None;
                }
                self.invalidate_settings();
                Some(LRESULT(0))
            }
            WM_CHAR => {
                let unit = wparam.0 as u16;
                if unit < 0x20 || unit == 0x7f {
                    return Some(LRESULT(0));
                }
                let v = &mut self.s.settings;
                if v.editing.is_some() {
                    if v.text.len() < 300 {
                        v.text.push(unit);
                    }
                } else if v.search_focus && v.capturing.is_none() && v.search.len() < 60 {
                    v.search.push(unit);
                    v.scroll = 0.0;
                    v.refilter();
                }
                self.invalidate_settings();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// The chosen provider's models: the known ones now, the full list (Codex's
    /// catalogue, an OpenAI-compatible server's `/models`) when a worker has it.
    fn refresh_models(&mut self) {
        let provider = self.chat_provider();
        self.s.settings.models = crate::chat::preset_models(provider);
        self.sync_chat_header();
        if matches!(provider, crate::chat::Provider::Codex | crate::chat::Provider::OpenAi) && !self.hidden {
            let key = crate::chat::key_for(provider).unwrap_or_default();
            crate::chat::fetch_models(self.hwnd, provider, self.cfg.chat_url.clone(), key);
        }
    }

    /// Moves the chat model one step through the provider's list (settings
    /// picker, or the model chip in the Chat tab).
    fn step_model(&mut self, forward: bool) {
        let list = &self.s.settings.models;
        if list.is_empty() {
            return;
        }
        let now = list.iter().position(|(v, _)| *v == self.cfg.chat_model);
        let next = match (now, forward) {
            (Some(i), true) => (i + 1) % list.len(),
            (Some(i), false) => (i + list.len() - 1) % list.len(),
            (None, _) => 0,
        };
        let value = list[next].0.clone();
        self.change_setting("chat_model", &value);
    }

    /// What the Chat tab's header shows: provider and model, and whether the
    /// conversation can be opened in Claude Code / Codex.
    fn sync_chat_header(&mut self) {
        let provider = self.chat_provider();
        let who = match provider {
            crate::chat::Provider::Claude => "Claude",
            crate::chat::Provider::Codex => "ChatGPT",
            crate::chat::Provider::Anthropic => "Anthropic",
            crate::chat::Provider::OpenAi => "OpenAI-compatible",
        };
        let model = self.s.settings.model_label(&self.cfg.chat_model);
        self.s.chat.model = format!("{who} \u{00B7} {model}").encode_utf16().collect();
        let tool = crate::cli::Tool::of(provider);
        self.s.chat.open = match (tool, &self.chat_session) {
            (Some(tool), Some(session)) if session.tool == tool => format!("Open in {}", tool.name()).encode_utf16().collect(),
            _ => Vec::new(),
        };
    }

    /// The conversation's Claude Code session / Codex thread, remembered in
    /// `%APPDATA%\Islet\chat-session.txt` (tests keep it in memory only).
    fn save_chat_session(&self) {
        if self.hidden {
            return;
        }
        if let Some(dir) = self.config_path.parent() {
            let path = dir.join("chat-session.txt");
            match &self.chat_session {
                Some(session) => {
                    let _ = std::fs::write(path, session.line());
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    fn load_chat_session(&self) -> Option<crate::cli::Session> {
        let path = self.config_path.parent()?.join("chat-session.txt");
        crate::cli::Session::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// Writes the step-by-step setup guide (kept inside the exe) next to the
    /// settings file and opens it in the browser.
    fn open_guide(&self) {
        if self.hidden {
            return;
        }
        let Some(dir) = self.config_path.parent() else { return };
        let path = dir.join("setup-guide.html");
        if std::fs::write(&path, SETUP_GUIDE).is_ok() {
            open_file(&path);
        }
    }

    /// A press on a login row: sign in, check again, or get the app.
    fn login_click(&mut self, tool: crate::cli::Tool) {
        use crate::cli::{Login, Tool};
        match self.s.settings.login(tool) {
            Some(Login::Missing) if tool == Tool::Claude => unsafe {
                ShellExecuteW(None, w!("open"), w!("https://claude.ai/download"), None, None, SW_SHOWNORMAL);
            },
            // The guide says where Codex comes from.
            Some(Login::Missing) => self.open_guide(),
            Some(Login::SignedOut) if self.s.settings.signing != Some(tool) && !self.hidden => {
                if crate::cli::sign_in(tool) {
                    // Look every few seconds until it worked (for three minutes at most).
                    self.s.settings.signing = Some(tool);
                    self.claude_polls = 0;
                    unsafe { SetTimer(Some(self.hwnd), T_CLAUDE, 4000, None) };
                }
            }
            _ => {
                match tool {
                    Tool::Claude => self.s.settings.claude = None,
                    Tool::Codex => self.s.settings.codex = None,
                }
                crate::cli::check_login(self.hwnd);
            }
        }
        self.repaint();
    }

    /// Reads what the page shows from the settings file (and the Run key).
    fn refresh_settings(&mut self) {
        let raw = config::raw(&self.config_path);
        self.s.settings.values = crate::settings::ROWS
            .iter()
            .map(|row| raw.iter().find(|(key, _)| key == row.key).map(|(_, value)| value.clone()).unwrap_or_default())
            .collect();
        self.s.settings.startup = if self.hidden { self.test_startup } else { crate::settings::starts_with_windows() };
        self.s.settings.refilter();
    }

    /// The colour picker is being dragged to settings-window (x, y): the colour
    /// changes at once everywhere (not written yet: that happens on release).
    fn picker_move(&mut self, x: f32, y: f32) {
        let v = &self.s.settings;
        let Some(p) = v.picker else { return };
        let Some(part) = p.drag else { return };
        let Some((h, s, val)) = crate::settings::pick_at(v, part, x, y) else { return };
        if let Some(p) = self.s.settings.picker.as_mut() {
            (p.h, p.s, p.v) = (h, s, val);
        }
        let hex = crate::theme::hex_text(crate::theme::hsv_to_rgb(h, s, val));
        match crate::settings::ROWS[p.row].key {
            "accent" => crate::theme::set_accent(&hex),
            "island_colour" => crate::theme::set_island(&hex),
            "card_colour" => crate::theme::set_card(&hex),
            _ => {}
        }
        self.repaint();
        self.invalidate_settings();
    }

    /// Writes the picker's colour to its setting ("#rrggbb").
    fn picker_commit(&mut self) {
        let Some(p) = self.s.settings.picker else { return };
        let key = crate::settings::ROWS[p.row].key;
        let hex = crate::theme::hex_text(crate::theme::hsv_to_rgb(p.h, p.s, p.v));
        self.change_setting(key, &hex);
        self.invalidate_settings();
    }

    /// Writes one setting and applies it.
    fn change_setting(&mut self, key: &str, value: &str) {
        config::set(&self.config_path, key, value);
        self.apply_config();
        self.refresh_settings();
        self.repaint();
    }

    fn settings_click(&mut self, row: usize, part: usize) {
        use crate::settings::{Kind, ROWS};
        self.settings_finish(true);
        let entry = &ROWS[row];
        crate::sound::cue(if matches!(entry.kind, Kind::Toggle | Kind::Choice(_)) { crate::sound::Cue::Toggle } else { crate::sound::Cue::Tap });
        let value = self.s.settings.values.get(row).cloned().unwrap_or_default();
        match &entry.kind {
            Kind::Header | Kind::Note(_) => {}
            Kind::Login(tool) => self.login_click(*tool),
            Kind::Model => self.step_model(part == 1),
            Kind::Guide => self.open_guide(),
            Kind::Toggle => {
                let lit = matches!(value.as_str(), "on" | "true" | "1" | "yes");
                self.change_setting(entry.key, if lit { "off" } else { "on" });
            }
            Kind::Choice(options) => {
                if let Some((option, _)) = options.get(part) {
                    self.change_setting(entry.key, option);
                }
            }
            Kind::Swatch(list) => {
                if let Some((name, _, _)) = list.get(part) {
                    self.s.settings.picker = None;
                    self.change_setting(entry.key, name);
                } else if self.s.settings.picker.is_some_and(|p| p.row == row) {
                    // The "any colour" circle again: the picker closes.
                    self.s.settings.picker = None;
                    self.s.settings.refilter();
                } else {
                    // The picker opens on the colour the row has now.
                    let colour = crate::theme::parse_hex(&value).unwrap_or_else(|| match value.as_str() {
                        "system" => crate::theme::system_accent().unwrap_or(list[0].2),
                        _ => list.iter().find(|(key, _, _)| *key == value).map_or(list[0].2, |c| c.2),
                    });
                    let (h, s, v) = crate::theme::rgb_to_hsv(colour);
                    self.s.settings.picker = Some(crate::settings::Picker { row, h, s, v, drag: None });
                    self.s.settings.refilter();
                }
            }
            Kind::Chips => {
                // A quick action on or off (switched on ones go to the end; Ctrl + drag orders them).
                let mut shown = config::parse_quick(&value);
                match shown.iter().position(|&q| q == part) {
                    Some(at) => {
                        shown.remove(at);
                    }
                    None if part < config::QUICK_NAMES.len() => shown.push(part),
                    None => {}
                }
                let text = if shown.is_empty() { "none".to_string() } else { shown.iter().map(|&q| config::QUICK_NAMES[q]).collect::<Vec<_>>().join(", ") };
                self.change_setting(entry.key, &text);
            }
            Kind::Pet => {
                // "none" first, then the cast, round and round.
                let count = crate::mascot::CAST.len() + 1;
                let now = crate::mascot::find(&value).map_or(0, |i| i + 1);
                let next = if part == 1 { (now + 1) % count } else { (now + count - 1) % count };
                let name = if next == 0 { "none" } else { crate::mascot::CAST[next - 1].name };
                self.change_setting(entry.key, name);
            }
            Kind::Scale => {
                let percent: i32 = value.trim_end_matches('%').trim().parse().unwrap_or(100);
                let next = (percent / 10 * 10 + if part == 1 { 10 } else { -10 }).clamp(80, 200);
                self.change_setting(entry.key, &next.to_string());
            }
            Kind::Hotkey => {
                // The shortcuts are let go meanwhile, or pressing one would trigger it.
                // (The settings window has the keyboard already.)
                self.unregister_hotkey();
                self.s.settings.capturing = Some(row);
                self.s.settings.search_focus = false;
            }
            Kind::Text(_) => {
                self.s.settings.editing = Some(row);
                self.s.settings.text = value.encode_utf16().collect();
                self.s.settings.search_focus = false;
            }
            Kind::Startup => {
                let on = !self.s.settings.startup;
                // Tests never touch the registry.
                if self.hidden {
                    self.test_startup = on;
                } else {
                    crate::settings::set_start_with_windows(on);
                }
                self.refresh_settings();
            }
            Kind::OpenFile => {
                if !self.hidden {
                    open_file(&self.config_path);
                }
            }
        }
        self.repaint();
    }

    /// Ends typing into a text row (`save`: keep what was typed) and shortcut capture.
    fn settings_finish(&mut self, save: bool) {
        if let Some(row) = self.s.settings.editing.take() {
            let text = String::from_utf16_lossy(&std::mem::take(&mut self.s.settings.text)).trim().to_string();
            if save && self.s.settings.values.get(row) != Some(&text) {
                self.change_setting(crate::settings::ROWS[row].key, &text);
            }
        }
        if self.s.settings.capturing.take().is_some() {
            self.register_hotkey();
        }
    }

    /// A key while the settings page has the keyboard. `false` for keys it does not use.
    fn settings_key(&mut self, key: u32) -> bool {
        if let Some(row) = self.s.settings.capturing {
            match key {
                // Modifiers alone: wait for the key that goes with them.
                0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 => return true,
                0x1B => self.settings_finish(false),
                _ => {
                    let held = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
                    let modifiers = held(0x12) as u32 | (held(0x11) as u32) << 1 | (held(0x10) as u32) << 2 | ((held(0x5B) || held(0x5C)) as u32) << 3;
                    match crate::settings::shortcut_text(modifiers, key) {
                        Some(text) => {
                            self.s.settings.capturing = None;
                            self.change_setting(crate::settings::ROWS[row].key, &text);
                            self.register_hotkey();
                        }
                        // Needs Ctrl, Alt, Shift or Win plus a letter, digit, F-key, Space or Tab.
                        None => return true,
                    }
                }
            }
            self.release_focus();
            self.repaint();
            return true;
        }
        if self.s.settings.editing.is_some() {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            match (key, ctrl) {
                (0x0D, _) | (0x09, _) => {
                    self.settings_finish(true);
                    self.release_focus();
                }
                (0x1B, _) => {
                    self.settings_finish(false);
                    self.release_focus();
                }
                (0x08, _) => {
                    self.s.settings.text.pop();
                }
                (0x56, true) => {
                    if let Some(text) = clipboard::read_text(self.hwnd) {
                        self.s.settings.text.extend(text.replace(['\r', '\n'], " ").encode_utf16().take(300));
                    }
                }
                _ => return false,
            }
            self.repaint();
            return true;
        }
        false
    }

    /// Re-reads the settings file and applies what changed, without a restart where possible.
    /// Applies the look settings: accent, surface, tabs, quick actions, corners,
    /// pill width, how the island moves.
    fn apply_look(&mut self) {
        let look = &self.cfg.look;
        crate::theme::set_accent(&look.accent);
        crate::theme::set_surface(&look.surface);
        crate::theme::set_island(&look.island_colour);
        crate::theme::set_card(&look.card_colour);
        crate::state::set_hidden_tabs(look.hidden_tabs);
        if self.grip.is_none() {
            self.s.order = look.orders.clone();
        }
        if self.s.tab_drag.is_none() {
            self.s.tab_order = look.tab_order;
        }
        let shown = |hidden: u8| (0..4).filter(|i| hidden & (1 << i) == 0).collect::<Vec<usize>>();
        let state = crate::state::LookState {
            corners: look.corners,
            pill_size: look.pill_size,
            splash: look.splash,
            alert_glow: look.alert_glow,
            lively: look.lively_panel,
            quick: look.quick.clone(),
            pill_height: look.pill_height,
            panel: look.panel,
            home: look.home,
            tiles: shown(look.hidden_tiles),
            power: shown(look.hidden_power),
        };
        self.s.set_look(state, look.motion);
        // Outside music and downloads the island wears the accent.
        if self.s.glow.target == 0.0 {
            self.s.accent_target = theme::accent();
            self.s.accent2_target = theme::accent();
        }
        self.eq_colour = theme::accent();
        // The compositor's wave layers have the old corners and colours baked in:
        // take the wave back (only while they are up) so it hands over with new ones.
        self.wave_dirty = self.comp.is_some();
        self.repaint();
    }

    fn apply_config(&mut self) {
        let fresh = config::load_from(&self.config_path);
        let old = std::mem::replace(&mut self.cfg, fresh);
        let new = &self.cfg;
        crate::sound::configure(new.sounds);
        if old.look != new.look {
            self.apply_look();
        }
        if old.stats_pill != self.cfg.stats_pill {
            self.s.stats_pill = self.cfg.stats_pill;
            self.sync_stats();
            self.repaint();
        }
        if old.lyrics != self.cfg.lyrics || old.lyrics_pill != self.cfg.lyrics_pill {
            self.s.lyric.in_pill = self.cfg.lyrics_pill;
            self.s.lyric.card &= self.cfg.lyrics;
            self.lyrics_key = 0;
            self.lyrics_for_song();
            self.sync_lyric();
            self.repaint();
        }
        let new = &self.cfg;
        let (theme, place, hotkeys) = (old.theme != new.theme, old.scale != new.scale || old.follow_cursor != new.follow_cursor || old.island_x != new.island_x, old.hotkey != new.hotkey || old.voice != new.voice || old.voice_hotkey != new.voice_hotkey);
        let (caps, layout, notifications) = (old.caps_popup != new.caps_popup, old.layout_popup != new.layout_popup, !old.notifications && new.notifications);
        let (character, weather, gpu) = (old.character != new.character, old.weather != new.weather || old.city != new.city, old.gpu != new.gpu);
        let autohide = old.autohide && !new.autohide;
        if theme {
            self.apply_theme(true);
        }
        if place {
            self.replace();
        }
        if hotkeys {
            self.unregister_hotkey();
            self.register_hotkey();
        }
        if caps && self.hook.is_some() {
            self.hook = None;
            self.hook = Some(MouseHook::start(self.hwnd, self.cfg.caps_popup));
        }
        if layout && !self.hidden {
            unsafe {
                if self.cfg.layout_popup {
                    self.shell_msg = RegisterWindowMessageW(w!("SHELLHOOK"));
                    let _ = RegisterShellHookWindow(self.hwnd);
                } else {
                    let _ = DeregisterShellHookWindow(self.hwnd);
                }
            }
        }
        if notifications {
            self.watch_toasts();
        }
        if character {
            self.s.mascot = crate::mascot::find(&self.cfg.character);
        }
        if character || old.character_lively != self.cfg.character_lively {
            self.mascot_plan_idle();
            self.update_mascot();
        }
        if old.wave != self.cfg.wave {
            self.s.wave_style = self.cfg.wave;
            if old.wave.react != self.cfg.wave.react && self.capture.is_none() {
                self.s.wave_level = wave_neutral(self.cfg.wave.react);
            }
            // The compositor layers were built for the old look: take the wave
            // back and let it hand over again with new layers.
            self.wave_dirty = self.comp.is_some();
            self.repaint();
        }
        if weather {
            self.kill(T_WEATHER);
            self.s.weather = None;
            self.s.weather_note.clear();
            if self.cfg.weather && !self.hidden {
                if self.cfg.city.is_empty() {
                    self.s.weather_note = "Add your city in the settings.".encode_utf16().collect();
                } else {
                    self.s.weather_note = "Fetching the forecast\u{2026}".encode_utf16().collect();
                    unsafe { SetTimer(Some(self.hwnd), T_WEATHER, 300, None) };
                }
            }
        }
        if gpu {
            // The device is rebuilt with the other renderer on the next frame.
            self.gfx = None;
        }
        if autohide && self.s.mode == Mode::Hidden {
            self.set_mode(Mode::Idle);
        }
        self.s.chat.enabled = self.cfg.chat;
        // Another provider may need a key (or none): look again.
        if old.chat_provider != self.cfg.chat_provider {
            if self.chat.is_some() && !self.hidden {
                self.chat_key = crate::chat::key_for(self.chat_provider());
                self.sync_chat();
            }
            // Another provider has other models (and the old model name means nothing there).
            if !old.chat_model.is_empty() && old.chat_model == self.cfg.chat_model {
                config::set(&self.config_path, "chat_model", "");
                self.cfg.chat_model.clear();
            }
            self.refresh_models();
        } else if old.chat_url != self.cfg.chat_url {
            self.refresh_models();
        }
        self.sync_chat_header();
        tray::CLICK_THROUGH.store(self.cfg.click_through, Relaxed);
        if !old.update_check && self.cfg.update_check {
            self.check_update(true);
        }
        self.resolve();
    }

    /// Looks for a newer release when the check is on (`force`: even if one ran today).
    fn check_update(&mut self, force: bool) {
        if !self.cfg.update_check || self.hidden || self.demo.is_some() {
            return;
        }
        if self.cfg.update_url.is_empty() {
            if force {
                self.queue.push(Toast::new(events::INFO, 5.0, Icon::Info, "Where are releases published?", "Add the address under \u{201C}Where releases are published\u{201D}.", Rgba::hex(0xff9f0a)).keyed("info"));
                self.resolve();
            }
            return;
        }
        if force || crate::update::due() {
            crate::update::check(self.hwnd, self.cfg.update_url.clone());
        }
    }

    // ------------------------------------------------------------ mascot --

    /// The character is on screen (clock or music pill, island at rest in view).
    fn mascot_visible(&self) -> bool {
        self.s.mascot.is_some() && self.s.mode == Mode::Idle && matches!(self.s.pill, Pill::Mascot | Pill::Music) && !self.suspended && self.demo.is_none()
    }

    /// Seconds since the last keyboard or mouse input anywhere.
    fn idle_secs(&self) -> u32 {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        unsafe {
            if !GetLastInputInfo(&mut info).as_bool() {
                return 0;
            }
            windows::Win32::System::SystemInformation::GetTickCount().wrapping_sub(info.dwTime) / 1000
        }
    }

    /// A cheap pseudo-random number (xorshift) for the character's whims.
    fn mascot_random(&mut self, below: u32) -> u32 {
        let mut x = self.mascot_rng.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.mascot_rng = x;
        x % below.max(1)
    }

    /// Starts an act now (it replaces whatever act was running).
    fn mascot_act(&mut self, act: crate::mascot::Act) {
        self.mascot_doing = Some((act, Instant::now()));
        self.update_mascot();
    }

    /// When the next act the character does by itself comes: 7 to 16 s away.
    fn mascot_plan_idle(&mut self) {
        let spread = self.mascot_random(9000) as u64;
        self.mascot_next_idle = Instant::now() + std::time::Duration::from_millis(7000 + spread);
    }

    /// `true` when (x, y) dp is on the character (it reacts instead of the pill).
    fn on_pet(&self, x: f32, y: f32) -> bool {
        if !self.mascot_visible() {
            return false;
        }
        let left = self.s.shape().left();
        ui::mascot_spot(&self.s, left).is_some_and(|(mx, my)| (x - mx).hypot(y - my) < crate::mascot::TOUCH_RADIUS)
    }

    /// A click on the character: one boops it, two quick ones make it twirl,
    /// three its trick.
    fn poke_pet(&mut self) {
        use crate::mascot::Act;
        crate::sound::cue(crate::sound::Cue::Boop);
        let now = Instant::now();
        let (last, count) = self.mascot_clicks;
        let count = if now.duration_since(last).as_millis() < 450 { count.saturating_add(1) } else { 1 };
        self.mascot_clicks = (now, count);
        let act = match count {
            1 => Act::Boop,
            2 => Act::Twirl,
            _ => Act::Special,
        };
        self.mascot_was_away = false;
        self.mascot_act(act);
    }

    /// Asks the frame loop to draw the character's corner (and nothing else).
    fn pet_wake(&mut self) {
        self.pet_dirty = true;
        if !self.dirty {
            self.dirty = true;
            self.fresh = true;
        }
    }

    /// Works out how the character looks now and repaints only its corner when
    /// that changed. While anything about it moves (an act, a blink, the eyes or
    /// the lean easing towards the cursor) the frame loop calls this every
    /// refresh (60 Hz, smooth); otherwise a timer waits for the next event and
    /// nothing runs in between.
    fn update_mascot(&mut self) {
        use crate::mascot::{look_at, Act, Eyes, Mark, Pose, TOUCH_RADIUS};
        use std::time::Duration;
        if !self.mascot_visible() {
            self.kill(T_MASCOT);
            self.pet_live = false;
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.mascot_last).as_secs_f32().min(0.1);
        self.mascot_last = now;
        let left = self.s.shape().left();
        let Some((mx, my)) = ui::mascot_spot(&self.s, left) else { return };
        let away = self.mascot_away || (self.idle_secs() >= AWAY_SECS && !self.mascot_awake);
        // Dozing off starts with a yawn; waking up with a stretch.
        if away != self.mascot_was_away {
            self.mascot_was_away = away;
            self.mascot_sleep_since = now;
            self.mascot_doing = Some((if away { Act::Yawn } else { Act::Stretch }, now));
            if !away {
                self.mascot_plan_idle();
            }
        }
        let (dx, dy) = (self.s.cursor.0 - mx, self.s.cursor.1 - my);
        let dist = dx.hypot(dy);
        let touching = !away && dist < TOUCH_RADIUS && self.s.mode == Mode::Idle;
        // Stroking: the cursor rubbing back and forth over it.
        if touching {
            let step = self.s.cursor.0 - self.mascot_rub.0;
            if step.abs() > 1.5 {
                let dir = step.signum();
                if dir != self.mascot_rub.1 {
                    self.mascot_rubs.retain(|t| now.duration_since(*t) < Duration::from_millis(1400));
                    self.mascot_rubs.push(now);
                    self.mascot_rub.1 = dir;
                }
                self.mascot_rub.0 = self.s.cursor.0;
            }
            let stroked = self.mascot_rubs.len() >= 3;
            let busy = self.mascot_doing.is_some_and(|(a, _)| a == Act::Pet);
            if stroked && !busy {
                self.mascot_rubs.clear();
                self.mascot_doing = Some((Act::Pet, now));
            }
        } else {
            self.mascot_rub = (self.s.cursor.0, 0.0);
        }
        // The cursor coming close: a happy little jump (not too often).
        if !away && !touching && dist < 70.0 && self.mascot_greeted.elapsed() > Duration::from_secs(20) && self.mascot_doing.is_none() {
            self.mascot_greeted = now;
            self.mascot_doing = Some((Act::Jump, now));
        }
        // Now and then it does something by itself (unless it is set to calm).
        let lively = self.cfg.character_lively;
        if lively && !away && self.mascot_doing.is_none() && now >= self.mascot_next_idle {
            let pick = self.mascot_random(Act::IDLE.len() as u32) as usize;
            self.mascot_doing = Some((Act::IDLE[pick], now));
            self.mascot_plan_idle();
        }
        // A blink every few seconds (never while asleep); the lids ease shut and open.
        if !away && now >= self.mascot_next_blink {
            self.mascot_blink = Some(now);
            let spread = self.mascot_random(3000);
            self.mascot_next_blink = now + Duration::from_millis(3500 + spread as u64);
        }
        let mut open = 1.0;
        if let Some(start) = self.mascot_blink {
            let u = now.duration_since(start).as_secs_f32() / 0.22;
            if u >= 1.0 {
                self.mascot_blink = None;
            } else {
                // Quick to close, a little slower to open.
                let shut = if u < 0.4 { (u / 0.4).powf(0.8) } else { 1.0 - ((u - 0.4) / 0.6).powf(1.4) };
                open = 1.0 - shut.clamp(0.0, 1.0);
            }
        }
        // The running act and how far it is.
        let (mut act, mut t) = (Act::None, 0.0);
        if let Some((doing, since)) = self.mascot_doing {
            let p = since.elapsed().as_secs_f32() / doing.secs().max(0.01);
            if p >= 1.0 {
                self.mascot_doing = None;
            } else {
                (act, t) = (doing, p);
            }
        }
        if let Some((frozen, at)) = self.mascot_freeze {
            (act, t) = (frozen, at);
        }
        let happy = self.mascot_happy.is_some_and(|until| until > now);
        let thinking = self.s.agent_working && !away;
        let asleep = away && act == Act::None;
        // Eyes, lean and blush ease towards where they want to be.
        let look_goal = if away { (0.0, 0.0) } else { look_at(dx, dy) };
        let lean_goal = if touching {
            (dx / 14.0).clamp(-1.0, 1.0) * 0.14
        } else if !away && dist < 90.0 {
            (dx / 90.0).clamp(-1.0, 1.0) * 0.06
        } else {
            0.0
        };
        let fond_goal = if touching { 1.0 } else { 0.0 };
        let k = |rate: f32| 1.0 - (-dt * rate).exp();
        let near = |a: f32, b: f32, eps: f32| (a - b).abs() < eps;
        let look = &mut self.mascot_look;
        look.0 += (look_goal.0 - look.0) * k(14.0);
        look.1 += (look_goal.1 - look.1) * k(14.0);
        if near(look.0, look_goal.0, 0.01) && near(look.1, look_goal.1, 0.01) {
            *look = look_goal;
        }
        self.mascot_lean += (lean_goal - self.mascot_lean) * k(9.0);
        if near(self.mascot_lean, lean_goal, 0.002) {
            self.mascot_lean = lean_goal;
        }
        self.mascot_fond += (fond_goal - self.mascot_fond) * k(7.0);
        if near(self.mascot_fond, fond_goal, 0.01) {
            self.mascot_fond = fond_goal;
        }
        let easing = self.mascot_look != look_goal || self.mascot_lean != lean_goal || self.mascot_fond != fond_goal;
        let pose = Pose {
            look: self.mascot_look,
            eyes: if asleep { Eyes::Closed } else if happy { Eyes::Happy } else { Eyes::Open },
            mark: if asleep { Mark::Sleep } else if thinking { Mark::Thinking } else { Mark::None },
            step: if thinking {
                (self.mascot_beat.elapsed().as_millis() / 500 % 3) as u8
            } else if asleep && lively {
                // Breathing and drifting z's: one small frame every 1.6 s.
                (self.mascot_sleep_since.elapsed().as_millis() / 1600 % 4) as u8
            } else {
                0
            },
            act,
            t,
            side: self.mascot_side,
            open,
            lean: self.mascot_lean,
            fond: self.mascot_fond,
        };
        if pose != self.s.pose {
            self.s.pose = pose;
            self.pet_wake();
        }
        // Moving now: the frame loop keeps calling (every refresh); no timer.
        self.pet_live = (act != Act::None && self.mascot_freeze.is_none()) || self.mascot_blink.is_some() || easing;
        if self.pet_live {
            self.kill(T_MASCOT);
            self.pet_wake();
            return;
        }
        // The next moment something changes on its own.
        let until = |at: Option<Instant>| at.filter(|t| *t > now).map(|t| t - now);
        let mut wait = if away {
            // Asleep: the z's drift (lively), or only a check for waking up (calm).
            Duration::from_millis(if lively { 1600 } else { 5000 })
        } else if lively {
            self.mascot_next_blink.saturating_duration_since(now).min(self.mascot_next_idle.saturating_duration_since(now))
        } else {
            self.mascot_next_blink.saturating_duration_since(now)
        };
        if let Some(soon) = until(self.mascot_happy) {
            wait = wait.min(soon);
        }
        if thinking {
            wait = wait.min(Duration::from_millis(500));
        }
        unsafe { SetTimer(Some(self.hwnd), T_MASCOT, (wait.as_millis() as u32).clamp(15, 10_000), None) };
    }

    /// New audio levels while the music pill shows: a bounce to alternating
    /// sides on each bass beat (it dances).
    fn mascot_beat(&mut self) {
        if !self.mascot_visible() || self.s.pill != Pill::Music {
            return;
        }
        let bass = self.eq_levels[0];
        let beat = bass > self.mascot_bass * 1.3 && bass > 0.18 && self.mascot_beat.elapsed().as_millis() > 300;
        self.mascot_bass = self.mascot_bass * 0.92 + bass * 0.08;
        // A longer act of its own is not cut short by the music.
        let busy = self.mascot_doing.is_some_and(|(act, _)| act != crate::mascot::Act::Bounce);
        if beat && !busy {
            self.mascot_beat = Instant::now();
            self.mascot_side = -self.mascot_side;
            self.mascot_act(crate::mascot::Act::Bounce);
        }
    }

    /// New audio levels: on a strong bass beat a drop falls from the music wave
    /// (subtle: only the biggest beats, a few seconds apart; lively: most beats).
    fn wave_drop_beat(&mut self) {
        let bass = self.eq_levels[0];
        let average = self.drop_bass;
        self.drop_bass = average * 0.94 + bass * 0.06;
        let (jump, floor, gap) = match self.s.wave_style.drops {
            1 => (1.6, 0.35, 4.5),
            2 => (1.3, 0.22, 1.2),
            _ => return,
        };
        let showing = self.s.mode == Mode::Idle && self.s.pill == Pill::Music && self.s.wave.x > 0.9;
        if !showing || self.s.beat_drop.is_some() || self.suspended || self.drop_last.elapsed().as_secs_f32() < gap {
            return;
        }
        if bass > average * jump && bass > floor {
            self.drop_last = Instant::now();
            // Somewhere along the middle of the wave, not under the cover or the bars.
            self.drop_seed = self.drop_seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let u = ((self.drop_seed >> 8) & 0xffff) as f32 / 65535.0;
            let shape = self.s.shape();
            let (from, to) = (shape.left() + 90.0, shape.right() - 90.0);
            self.s.drop_from_wave(from + (to - from) * u);
            self.invalidate();
        }
    }

    // ------------------------------------------------------------- voice --

    /// The voice shortcut: starts listening, or ends it (the words so far are kept).
    fn toggle_voice(&mut self) {
        if self.voice_active {
            crate::voice::stop();
            return;
        }
        // Tests never open the microphone (hook wparam 17 fakes it instead).
        if self.hidden || self.suspended {
            return;
        }
        let engine = if self.cfg.voice == "api" {
            let local = self.cfg.voice_url.starts_with("http://localhost") || self.cfg.voice_url.starts_with("http://127.0.0.1");
            // OPENAI_API_KEY, else the chat's saved key when the chat itself is
            // OpenAI-compatible (an Anthropic key would only be refused).
            let env = std::env::var("OPENAI_API_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
            let saved = if self.chat_provider() == crate::chat::Provider::OpenAi { crate::chat::load_key() } else { None };
            let key = env.or(saved).unwrap_or_default();
            if key.is_empty() && !local {
                self.queue.push(Toast::new(events::INFO, 5.0, Icon::Info, "Voice needs an API key", "Enter one in the Chat tab first (an OpenAI-compatible key).", Rgba::hex(0xff9f0a)).keyed("info"));
                self.resolve();
                return;
            }
            crate::voice::Engine::Api { url: self.cfg.voice_url.clone(), model: self.cfg.voice_model.clone(), language: self.cfg.voice_language.clone(), key }
        } else {
            crate::voice::Engine::Local
        };
        if crate::voice::start(self.hwnd, engine) {
            self.voice_active = true;
            self.s.voice_text = "Starting\u{2026}".encode_utf16().collect();
        } else {
            self.queue.push(Toast::new(events::INFO, 4.0, Icon::Info, "Voice input failed", "The listener could not be started", Rgba::hex(0xff9f0a)).keyed("info"));
        }
        self.resolve();
    }

    fn on_voice(&mut self, update: crate::voice::Update) {
        use crate::voice::Update;
        let say = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
        match update {
            Update::Listening => self.s.voice_text = say("Listening\u{2026}"),
            // The pill shows the end of a long sentence, from a word start.
            Update::Partial(words) => {
                let shown = match words.char_indices().rev().nth(44) {
                    Some((cut, _)) => format!("\u{2026}{}", words[cut..].split_once(' ').map_or(&words[cut..], |(_, rest)| rest)),
                    None => words,
                };
                self.s.voice_text = say(&shown);
            }
            Update::Transcribing => self.s.voice_text = say("Transcribing\u{2026}"),
            Update::Text(words) => {
                self.voice_active = false;
                if words.trim().is_empty() {
                    self.queue.push(Toast::new(events::INFO, 2.5, Icon::Info, "Nothing heard", "", Rgba::hex(0xff9f0a)).keyed("info"));
                } else if !self.hidden {
                    // Typed once the shortcut's keys are up, so they do not turn letters into shortcuts.
                    self.voice_typing = Some((words, 0));
                    unsafe { SetTimer(Some(self.hwnd), T_TYPE, 40, None) };
                } else {
                    self.voice_typed = words;
                }
            }
            Update::Failed(why) => {
                self.voice_active = false;
                self.queue.push(Toast::new(events::INFO, 5.0, Icon::Info, "Voice input failed", why, Rgba::hex(0xff9f0a)).keyed("info"));
            }
        }
        self.resolve();
        self.repaint();
    }

    fn open_command(&mut self) {
        if self.suspended || self.s.mode == Mode::Command {
            return;
        }
        self.kill(T_INDEX);
        if self.index.is_none() {
            self.index = Some(command::Index::build());
            // Store apps are looked up in the background and merged when ready.
            command::refresh_store(self.hwnd);
        }
        // Exchange rates: from disk, and from the network at most once a day (tests never fetch).
        if self.cfg.currency && !self.hidden {
            if self.rates.is_none() {
                self.rates = crate::currency::load();
            }
            if !self.rates.as_ref().is_some_and(|r| r.fresh()) {
                crate::currency::refresh(self.hwnd);
            }
        }
        self.cmd_query.clear();
        self.refresh_command();
        self.set_mode(Mode::Command);
        self.take_focus();
    }

    fn close_command(&mut self) {
        self.release_focus();
        if self.s.mode == Mode::Command {
            self.set_mode(Mode::Idle);
        }
        // The Start Menu index is only worth keeping while the bar is in use.
        unsafe { SetTimer(Some(self.hwnd), T_INDEX, 60_000, None) };
    }

    fn refresh_command(&mut self) {
        let wide = |s: &str, max: usize| s.encode_utf16().take(max).collect::<Vec<u16>>();
        self.cmd_rows = match &self.index {
            Some(index) => command::rows(index, &self.cmd_query, timer::now(), self.cfg.currency.then_some((self.rates.as_ref(), self.cfg.currency_home.as_str()))),
            None => Vec::new(),
        };
        self.s.cmd_text = wide(&self.cmd_query, 120);
        self.s.cmd_rows = self.cmd_rows.iter().map(|r| ListRow { title: wide(&r.title, 60), detail: wide(&r.detail, 40), ..Default::default() }).collect();
        self.s.cmd_sel = 0;
        if self.s.mode == Mode::Command {
            // The bar grows and shrinks with its rows.
            self.s.set_mode(Mode::Command);
            self.update_zone();
        }
        self.repaint();
    }

    fn run_command_row(&mut self, row: usize) {
        let Some(action) = self.cmd_rows.get(row).map(|r| r.action.clone()) else { return };
        self.close_command();
        match action {
            command::Action::Launch(path) => unsafe {
                ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
            },
            // A conversion still waiting for its rates has nothing to copy yet.
            command::Action::Copy(text) if text.is_empty() => {}
            command::Action::Copy(text) => {
                clipboard::write_text(self.hwnd, &text);
                self.queue.push(Toast::new(events::INFO, 2.0, Icon::Info, "Copied", text, theme::accent()).keyed("info"));
                self.resolve();
            }
            command::Action::Phrase(phrase) => {
                self.command(&phrase);
            }
        }
    }

    /// A typed character while Islet has the keyboard.
    fn on_char(&mut self, unit: u16) {
        if !self.focused || unit < 0x20 || unit == 0x7f {
            return;
        }
        if self.s.settings.editing.is_some() {
            if self.s.settings.text.len() < 300 {
                self.s.settings.text.push(unit);
            }
            self.repaint();
            return;
        }
        if self.s.settings.capturing.is_some() {
            return;
        }
        if self.s.mode == Mode::Command {
            if let Some(c) = char::from_u32(unit as u32) {
                if self.cmd_query.chars().count() < 120 {
                    self.cmd_query.push(c);
                    self.refresh_command();
                }
            }
        } else if self.s.clip_focus && self.s.clip_query.len() < 60 {
            self.s.clip_query.push(unit);
            self.clip_scroll = 0;
            self.refresh_clipboard();
        } else if self.s.chat.focus {
            // Surrogate halves arrive one by one; they are joined when both are in.
            if let Some(c) = char::from_u32(unit as u32) {
                if self.chat_input.len() < 8000 {
                    self.chat_input.push(c);
                }
            }
            self.sync_chat();
        } else if self.s.note.focus {
            if let Some(notes) = &mut self.notes {
                notes.insert(&[unit]);
                unsafe { SetTimer(Some(self.hwnd), T_NOTE, 1500, None) };
            }
            self.sync_note(true);
        }
    }

    /// A key press while Islet has the keyboard. Returns `false` for keys it does not use.
    fn on_key(&mut self, key: u32) -> bool {
        if !self.focused {
            return false;
        }
        if self.settings_key(key) {
            return true;
        }
        const BACK: u32 = 0x08;
        const ENTER: u32 = 0x0D;
        const ESCAPE: u32 = 0x1B;
        const UP: u32 = 0x26;
        const DOWN: u32 = 0x28;
        if self.s.mode == Mode::Command {
            match key {
                ESCAPE => self.close_command(),
                ENTER => self.run_command_row(self.s.cmd_sel),
                BACK => {
                    self.cmd_query.pop();
                    self.refresh_command();
                }
                UP | DOWN => {
                    let count = self.s.cmd_rows.len();
                    if count > 0 {
                        self.s.cmd_sel = if key == DOWN { (self.s.cmd_sel + 1) % count } else { (self.s.cmd_sel + count - 1) % count };
                        self.repaint();
                    }
                }
                _ => return false,
            }
            return true;
        }
        if self.s.note.focus {
            return self.note_key(key);
        }
        if self.s.chat.focus {
            return self.chat_key(key);
        }
        if self.s.clip_focus {
            match key {
                ESCAPE => {
                    self.blur();
                    self.repaint();
                }
                BACK => {
                    self.s.clip_query.pop();
                    self.clip_scroll = 0;
                    self.refresh_clipboard();
                }
                ENTER => self.paste_row(0),
                _ => return false,
            }
            return true;
        }
        false
    }

    // ------------------------------------------------------------ events --

    /// Decides what the pill shows: the current toast, else the next queued
    /// one, else the highest-ranked persistent source (see `events`).
    fn resolve(&mut self) {
        if self.suspended || self.demo.is_some() {
            return;
        }
        if self.toast.is_none() && !matches!(self.s.mode, Mode::Expanded | Mode::Command) {
            if let Some(toast) = self.queue.pop() {
                self.show_toast(toast);
            }
        }
        let music = self.s.wave.target > 0.5;
        let timer = self.store.next_timer(timer::now()).map(|(left, label)| (left, label.to_string()));
        if let Some((left, label)) = &timer {
            self.s.timer_left = *left;
            self.s.timer_label = label.encode_utf16().take(40).collect();
        }
        let pill = match &self.toast {
            Some(toast) if toast.permit.is_some() => Pill::Permit,
            Some(toast) if toast.done.is_some() => Pill::Done,
            Some(_) => Pill::Toast,
            None => events::persistent(self.voice_active, self.recording.is_some(), self.download_active, timer.is_some(), music),
        };
        // With a character chosen, the clock pill makes room for it.
        let pill = if pill == Pill::Clock && self.s.mascot.is_some() { Pill::Mascot } else { pill };
        if self.s.pill != pill {
            // The "Downloaded" card lets go of a droplet as it retracts.
            if self.s.pill == Pill::Done {
                self.s.release_droplet();
            }
            self.s.set_pill(pill);
        }
        match pill {
            Pill::Download => self.s.set_line(true, theme::accent()),
            Pill::Done => self.s.set_line(true, theme::GREEN),
            _ => {
                self.s.set_line(false, theme::accent());
                if music {
                    self.s.set_music(true, self.music_accent.0, self.music_accent.1);
                }
            }
        }
        if matches!(pill, Pill::Timer | Pill::Record) {
            unsafe { SetTimer(Some(self.hwnd), T_SECOND, 500, None) };
        } else {
            self.kill(T_SECOND);
        }
        if self.s.mode == Mode::Idle {
            self.kill(T_AUTOHIDE);
            self.arm_autohide();
        }
        self.update_zone();
        self.update_mascot();
        self.repaint();
    }

    fn show_toast(&mut self, toast: Toast) {
        let wide = |s: &str, max: usize| s.encode_utf16().take(max).collect::<Vec<u16>>();
        if let Some(done) = &toast.done {
            let ext = std::path::Path::new(&done.name).extension().map_or("FILE".to_string(), |e| e.to_string_lossy().to_uppercase());
            self.s.done = DoneView { name: wide(&done.name, 80), ext: wide(&ext, 4), size: done.size };
        }
        // Timers, reminders and questions ring; quieter news chimes. Windows
        // notifications already made their own sound, keyboard popups are too frequent.
        let important = toast.priority >= events::AGENT_WAITING;
        match toast.priority {
            _ if important => crate::sound::cue(crate::sound::Cue::Alert),
            events::NOTIFICATION | events::KEYBOARD => {}
            _ => crate::sound::cue(crate::sound::Cue::Chime),
        }
        // Droplets splash as it arrives (not for the frequent keyboard popups).
        if toast.priority != events::KEYBOARD {
            self.s.splash(important);
            if self.s.look.splash {
                crate::sound::cue_after(crate::sound::Cue::Splash, 0.08);
            }
            self.invalidate();
        }
        self.s.toast = ToastView { icon: toast.icon, title: wide(&toast.title, 60), text: wide(&toast.text, 90), accent: toast.accent };
        unsafe { SetTimer(Some(self.hwnd), T_TOAST, (toast.seconds * 1000.0) as u32, None) };
        let permit = toast.permit.is_some();
        self.toast = Some(toast);
        if permit {
            self.ask_step = None;
            self.sync_permit_view();
        }
    }

    /// Fills the permission card for the current request: the text box, a
    /// question's options and the buttons that fit what is asked.
    fn sync_permit_view(&mut self) {
        let wide = |s: &str, max: usize| s.encode_utf16().take(max).collect::<Vec<u16>>();
        let Some((permit, title, text)) = self.toast.as_ref().and_then(|t| Some((t.permit?, t.title.clone(), t.text.clone()))) else { return };
        let label = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let mut view = PermitView { title: wide(&title, 70), ..Default::default() };
        let mut body = text;
        match self.asks.get(&permit.id) {
            Some(agent::Ask::Questions { list, .. }) => {
                if self.ask_step.as_ref().is_none_or(|step| step.id != permit.id) {
                    self.ask_step = Some(AskStep { id: permit.id, index: 0, answers: Vec::new(), picked: Vec::new() });
                }
                let step = self.ask_step.as_mut().expect("just set");
                let q = &list[step.index.min(list.len() - 1)];
                step.picked.resize(q.options.len(), false);
                // "Claude Code asks · Framework (1 of 2)"
                let mut heading = title.clone();
                if !q.header.is_empty() {
                    heading = format!("{heading} \u{00B7} {}", q.header);
                }
                if list.len() > 1 {
                    heading = format!("{heading} ({} of {})", step.index + 1, list.len());
                }
                view.title = wide(&heading, 70);
                body = q.text.clone();
                view.options = q.options.iter().map(|(l, d)| (wide(l, 40), wide(d, 80))).collect();
                view.picked = step.picked.clone();
                view.buttons.push((label("In terminal"), PermitAction::Terminal));
                if q.multi {
                    let last = step.index + 1 == list.len();
                    view.buttons.push((label(if last { "Send" } else { "Next" }), PermitAction::Send));
                }
            }
            Some(agent::Ask::Plan { .. }) => {
                view.buttons = vec![(label("Keep planning"), PermitAction::Deny), (label("In terminal"), PermitAction::Terminal), (label("Approve"), PermitAction::Allow)];
            }
            Some(agent::Ask::Tool { always: Some(_) }) => {
                view.buttons = vec![(label("Deny"), PermitAction::Deny), (label("Always allow"), PermitAction::Always), (label("Allow"), PermitAction::Allow)];
            }
            _ => view.buttons = vec![(label("Deny"), PermitAction::Deny), (label("Allow"), PermitAction::Allow)],
        }
        // The text on up to two lines of the card's monospace box.
        const LINE: usize = 72;
        let chars: Vec<char> = body.chars().collect();
        let first: String = chars.iter().take(LINE).collect();
        let mut second: String = chars.iter().skip(LINE).take(LINE - 1).collect();
        if chars.len() > LINE * 2 - 1 {
            second.push('\u{2026}');
        }
        view.lines = [wide(&first, LINE), wide(&second, LINE)];
        view.h = crate::state::permit_height(view.options.len());
        self.s.permit = view;
        self.s.permit_hover = None;
        self.s.retarget_now();
        self.repaint();
    }

    /// Ends the current toast. An unanswered permission request gets "no
    /// decision", so the agent falls back to its own prompt.
    fn dismiss_toast(&mut self) {
        self.kill(T_TOAST);
        if let Some(permit) = self.toast.take().and_then(|t| t.permit) {
            agent::reply(permit.id, &agent::permit_reply(permit.claude, None));
            self.asks.remove(&permit.id);
        }
        self.resolve();
    }

    /// A click on the permission card: a button, or an option of a question.
    fn permit_action(&mut self, action: PermitAction) {
        let Some(permit) = self.toast.as_ref().and_then(|t| t.permit) else { return };
        let ask = self.asks.get(&permit.id).cloned().unwrap_or(agent::Ask::Tool { always: None });
        let claude = permit.claude;
        let reply = match (action, &ask) {
            (PermitAction::Terminal, _) => agent::permit_reply(claude, None),
            (PermitAction::Deny, agent::Ask::Plan { .. }) => agent::deny_reply("The user wants to keep planning."),
            (PermitAction::Deny, _) => agent::permit_reply(claude, Some(false)),
            (PermitAction::Allow, agent::Ask::Plan { input }) => agent::plan_reply(input),
            (PermitAction::Allow, _) => agent::permit_reply(claude, Some(true)),
            (PermitAction::Always, agent::Ask::Tool { always: Some(rules) }) => agent::always_reply(rules),
            (PermitAction::Choose(i), agent::Ask::Questions { raw, list }) => {
                let Some(step) = self.ask_step.as_mut().filter(|s| s.id == permit.id) else { return };
                let q = &list[step.index.min(list.len() - 1)];
                if q.multi {
                    // Several may be ticked; Send / Next moves on.
                    if let Some(p) = step.picked.get_mut(i) {
                        *p = !*p;
                    }
                    self.sync_permit_view();
                    return;
                }
                let Some((label, _)) = q.options.get(i) else { return };
                step.answers.push((q.text.clone(), vec![label.clone()]));
                match self.next_question(list.len()) {
                    true => agent::answers_reply(raw, &self.ask_step.take().map(|s| s.answers).unwrap_or_default()),
                    false => return,
                }
            }
            (PermitAction::Send, agent::Ask::Questions { raw, list }) => {
                let Some(step) = self.ask_step.as_mut().filter(|s| s.id == permit.id) else { return };
                let q = &list[step.index.min(list.len() - 1)];
                let labels: Vec<String> = q.options.iter().zip(&step.picked).filter(|(_, p)| **p).map(|((l, _), _)| l.clone()).collect();
                if labels.is_empty() {
                    return;
                }
                step.answers.push((q.text.clone(), labels));
                match self.next_question(list.len()) {
                    true => agent::answers_reply(raw, &self.ask_step.take().map(|s| s.answers).unwrap_or_default()),
                    false => return,
                }
            }
            _ => return,
        };
        agent::reply(permit.id, &reply);
        self.asks.remove(&permit.id);
        self.ask_step = None;
        self.toast = None;
        self.kill(T_TOAST);
        self.resolve();
    }

    /// Moves a question card to its next question; `true` when all `count` are answered.
    fn next_question(&mut self, count: usize) -> bool {
        let Some(step) = self.ask_step.as_mut() else { return true };
        step.index += 1;
        step.picked.clear();
        if step.index >= count {
            return true;
        }
        // More time for the next question.
        unsafe { SetTimer(Some(self.hwnd), T_TOAST, (PERMIT_SECONDS * 1000.0) as u32, None) };
        self.sync_permit_view();
        false
    }

    /// Requests that arrived on the pipe: agent status and permission requests.
    fn on_agent(&mut self) {
        let mut news = false;
        for request in agent::take() {
            let event = agent::interpret(&request.text);
            if self.test_hooks {
                // Lets tests see that a request arrived and how it was understood.
                self.agent_seen += 1;
                let understood = !matches!(event, agent::Event::Ignore) as usize;
                unsafe {
                    use windows::Win32::Foundation::HANDLE;
                    let _ = SetPropW(self.hwnd, w!("IsletAgentSeen"), Some(HANDLE((self.agent_seen * 2 + understood) as *mut _)));
                }
            }
            if !matches!(event, agent::Event::Ignore) {
                self.agent_heard = Some(Instant::now());
            }
            news |= matches!(event, agent::Event::Permit { .. } | agent::Event::Waiting { .. } | agent::Event::Done { .. } | agent::Event::Notice { .. });
            if let agent::Event::Permit { who, tool, detail, claude, ask } = event {
                if self.suspended {
                    // Nobody is looking at the island: let the agent ask in its own window.
                    agent::reply(request.id, &agent::permit_reply(claude, None));
                    continue;
                }
                let title = match &ask {
                    agent::Ask::Questions { .. } => format!("{who} asks"),
                    agent::Ask::Plan { .. } => format!("{who} has a plan"),
                    _ if tool.is_empty() => format!("{who} asks for permission"),
                    _ => format!("{who} wants to use {tool}"),
                };
                self.asks.insert(request.id, ask);
                let mut toast = Toast::new(events::PERMIT, PERMIT_SECONDS, Icon::Spark, title, detail, theme::accent());
                toast.permit = Some(Permit { id: request.id, claude });
                self.queue.push(toast);
                continue;
            }
            // Everything else needs no answer.
            agent::reply(request.id, "");
            match event {
                agent::Event::Working { key } => {
                    self.agents.insert(key, Instant::now());
                }
                agent::Event::Waiting { key, who, text } => {
                    self.agents.remove(&key);
                    self.queue.push(Toast::new(events::AGENT_WAITING, 8.0, Icon::Spark, format!("{who} needs you"), text, Rgba::hex(0xffd60a)).important());
                }
                agent::Event::Done { key, who, text } => {
                    self.agents.remove(&key);
                    self.mascot_happy = Some(Instant::now() + std::time::Duration::from_secs(6));
                    if self.mascot_visible() {
                        self.mascot_act(crate::mascot::Act::Jump);
                    }
                    self.queue.push(Toast::new(events::AGENT_DONE, 5.0, Icon::Spark, format!("{who} finished"), text, theme::GREEN).important());
                }
                agent::Event::Ended { key } => {
                    self.agents.remove(&key);
                }
                agent::Event::Notice { title, text } => self.queue.push(Toast::new(events::INFO, 4.0, Icon::Info, title, text, theme::accent())),
                agent::Event::Permit { .. } | agent::Event::Ignore => {}
            }
        }
        if news {
            // The same news may have come as a Windows notification first: the agent's own wins.
            self.queue.drop_key(events::AI_NOTE);
            if self.toast.as_ref().is_some_and(|t| t.key == Some(events::AI_NOTE)) {
                self.toast = None;
                self.kill(T_TOAST);
            }
        }
        self.refresh_agents();
        // A permission request must not wait behind an open panel.
        if self.queue.has_permit() && self.s.mode == Mode::Expanded {
            self.set_mode(Mode::Idle);
        } else {
            self.resolve();
        }
    }

    /// Updates the "agent working" mark; sessions that went silent are dropped.
    fn refresh_agents(&mut self) {
        self.agents.retain(|_, seen| seen.elapsed().as_secs() < AGENT_STALE_SECS);
        let working = !self.agents.is_empty();
        if working != self.s.agent_working {
            self.s.agent_working = working;
            self.s.repaint = true;
            self.update_mascot();
        }
        if working {
            unsafe { SetTimer(Some(self.hwnd), T_AGENT, (AGENT_STALE_SECS * 1000) as u32, None) };
        } else {
            self.kill(T_AGENT);
        }
    }

    /// Installs or removes the Claude Code / Codex hooks and reports the outcome in the pill.
    fn hooks(&mut self, install: bool) {
        let toast = match hooks::apply(install, None, None) {
            Ok(note) => Toast::new(events::INFO, 5.0, Icon::Spark, if install { "Claude Code hooks installed" } else { "Claude Code hooks removed" }, note, theme::GREEN),
            Err(error) => Toast::new(events::INFO, 8.0, Icon::Info, "Hooks not changed", error, Rgba::hex(0xff9f0a)),
        };
        self.queue.push(toast.keyed("hooks"));
        self.resolve();
    }

    // -------------------------------------------------------------- chat --

    fn chat_provider(&self) -> crate::chat::Provider {
        crate::chat::Settings::new(&self.cfg.chat_provider, "", "").provider
    }

    /// Loads the conversation and the key when the Chat tab first opens.
    fn open_chat(&mut self) {
        self.s.chat.enabled = self.cfg.chat;
        if !self.cfg.chat {
            return;
        }
        if self.chat.is_none() {
            // Tests keep everything in memory and have no key.
            self.chat = Some(if self.hidden { Vec::new() } else { crate::chat::load() });
            if !self.hidden {
                self.chat_key = crate::chat::key_for(self.chat_provider());
                self.chat_session = self.load_chat_session();
            }
            self.refresh_models();
        }
        self.sync_chat_header();
        self.sync_chat();
    }

    /// The drawn form of an attachment; uploads a picture's thumbnail when needed.
    fn chip(&mut self, attachment: &crate::chat::Attachment) -> Chip {
        let wide = |s: &str| s.encode_utf16().take(80).collect::<Vec<u16>>();
        let image = match &attachment.content {
            crate::chat::Content::Image { jpeg, .. } => self.chat_thumb(jpeg),
            crate::chat::Content::Text(_) => None,
        };
        Chip { image, name: wide(&attachment.name), detail: wide(&attachment.detail), busy: false }
    }

    /// Key of a picture's thumbnail in the `Gfx` cache, decoding it only the first time.
    fn chat_thumb(&mut self, jpeg: &[u8]) -> Option<u64> {
        // Enough of the file to tell pictures apart without hashing all of it on every key press.
        let mut key: u64 = 0xcbf2_9ce4_8422_2325 ^ jpeg.len() as u64;
        for &byte in jpeg.iter().take(4096).chain(jpeg.iter().rev().take(1024)) {
            key = (key ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let key = key | 1 << 62;
        let gfx = self.gfx.as_ref()?;
        if !gfx.has_image(key) {
            if !self.chat_thumbs.iter().any(|(k, _)| *k == key) {
                let thumb = image::decode_png(jpeg).map(|p| image::shrink(&p, 160, 160))?;
                if self.chat_thumbs.len() >= 24 {
                    self.chat_thumbs.remove(0);
                }
                self.chat_thumbs.push((key, thumb));
            }
            if let Some((_, thumb)) = self.chat_thumbs.iter().find(|(k, _)| *k == key) {
                gfx.set_image(key, &thumb.bgra, thumb.w, thumb.h);
            }
        }
        Some(key)
    }

    /// Lays the conversation and the input field out for drawing.
    fn sync_chat(&mut self) {
        let Some(messages) = self.chat.take() else { return };
        let busy = self.s.chat.busy;
        let last_answer = messages.iter().rposition(|m| !m.user && !m.error);
        let mut rows = Vec::with_capacity(messages.len());
        for (i, m) in messages.iter().enumerate() {
            let (text, bold) = if m.user { (m.text.encode_utf16().collect(), Vec::new()) } else { crate::chat::render(&m.text) };
            let (w, h) = self.gfx.as_ref().map_or((0.0, 0.0), |gfx| gfx.wrapped_size(&text, if m.user { CHAT_BUBBLE_W } else { CHAT_TEXT_W }, &bold));
            let chips = m.attachments.iter().map(|a| self.chip(a)).collect();
            let streaming = busy && i + 1 == messages.len();
            let actions = !m.user && !m.error && !streaming && !m.text.is_empty();
            rows.push(ChatRow { user: m.user, error: m.error, text, bold, w, h: h.max(if m.user { 18.0 } else { 22.0 }), chips, actions, regenerate: actions && Some(i) == last_answer && !busy });
        }
        self.chat = Some(messages);
        self.s.chat.rows = rows;
        let pending = std::mem::take(&mut self.chat_pending);
        self.s.chat.pending = pending
            .iter()
            .map(|p| match &p.ready {
                Some(attachment) => self.chip(attachment),
                None => Chip { image: None, name: p.name.encode_utf16().take(80).collect(), detail: p.status.encode_utf16().take(80).collect(), busy: true },
            })
            .collect();
        self.chat_pending = pending;
        self.s.chat.need_key = self.chat_key.is_none();
        // A key is never shown; a long message shows its end.
        let typed: Vec<u16> = self.chat_input.encode_utf16().collect();
        self.s.chat.shown = if self.s.chat.need_key { vec![0x2022; typed.len().min(48)] } else { typed[typed.len().saturating_sub(72)..].to_vec() };
        let most = (crate::panels::chat_height(&self.s) - 200.0).max(0.0);
        self.s.chat.scroll = self.s.chat.scroll.clamp(0.0, most);
        self.repaint();
    }

    /// Enter in the chat field: stores the key, runs a slash command or sends the
    /// message with the attachments that are ready.
    fn chat_submit(&mut self) {
        let reading = self.chat_pending.iter().any(|p| p.ready.is_none());
        let text = self.chat_input.trim().to_string();
        let empty = text.is_empty() && (self.chat_pending.is_empty() || self.chat_key.is_none());
        // Attachments still being read hold the message back (the typed text stays).
        if empty || self.s.chat.busy || self.chat.is_none() || (reading && self.chat_key.is_some()) {
            return self.sync_chat();
        }
        self.chat_input.clear();
        if text == "/key" {
            if !self.hidden {
                crate::chat::forget_key();
            }
            // The Claude login needs no key; others may still find one in the environment.
            self.chat_key = if self.hidden { None } else { crate::chat::key_for(self.chat_provider()) };
        } else if self.chat_key.is_none() {
            if !self.hidden {
                crate::chat::save_key(&text);
            }
            self.chat_key = Some(text);
        } else if let Some(messages) = &mut self.chat {
            let mut message = crate::chat::Message::new(true, text, false);
            message.attachments = self.chat_pending.drain(..).filter_map(|p| p.ready).collect();
            messages.push(message);
            self.chat_ask();
        }
        self.sync_chat();
    }

    /// Asks the provider to answer the conversation (which ends with the user's message).
    fn chat_ask(&mut self) {
        let (Some(messages), Some(key)) = (&mut self.chat, self.chat_key.clone()) else { return };
        let history = messages.clone();
        messages.push(crate::chat::Message::new(false, String::new(), false));
        self.s.chat.busy = true;
        self.s.chat.scroll = 0.0;
        // Tests never reach the network: the answer stays empty until a hook fills it.
        if !self.hidden {
            let settings = crate::chat::Settings::new(&self.cfg.chat_provider, &self.cfg.chat_model, &self.cfg.chat_url);
            crate::chat::send(self.hwnd, settings, key, history, self.chat_session.clone());
        }
    }

    /// Drops the last answer (and any error after it) and asks again.
    fn chat_regenerate(&mut self) {
        let Some(messages) = &mut self.chat else { return };
        if self.s.chat.busy || self.chat_key.is_none() {
            return;
        }
        while messages.last().is_some_and(|m| !m.user) {
            messages.pop();
        }
        if !messages.is_empty() {
            self.chat_ask();
        }
        self.sync_chat();
    }

    /// Reads files into pending attachments (at most ten wait at a time).
    fn chat_attach(&mut self, paths: Vec<PathBuf>) {
        if !self.s.chat.enabled || self.chat_key.is_none() || self.chat.is_none() {
            return;
        }
        for path in paths.into_iter().take(10usize.saturating_sub(self.chat_pending.len())) {
            let id = self.chat_next_id();
            self.chat_pending.push(crate::chat::Pending { id, name: crate::attach::file_name(&path), status: "Reading\u{2026}".into(), ready: None });
            crate::attach::read_file(self.hwnd, id, path);
        }
        self.sync_chat();
    }

    fn chat_next_id(&mut self) -> u32 {
        self.chat_ids += 1;
        self.chat_ids
    }

    /// The file picker closed (it runs in `main::wndproc`, outside this borrow).
    pub fn picked(&mut self, paths: Vec<PathBuf>) {
        self.picking = false;
        self.chat_attach(paths);
        if self.s.mode == Mode::Expanded && self.s.tab == 4 {
            self.s.chat.focus = true;
            self.focused = false;
            self.take_focus();
        }
        self.track_cursor();
    }

    /// Progress and results of the attachment readers.
    fn on_attach(&mut self) {
        for update in crate::attach::take() {
            match update {
                crate::attach::Update::Progress(id, status) => {
                    if let Some(pending) = self.chat_pending.iter_mut().find(|p| p.id == id) {
                        pending.status = status;
                    }
                }
                crate::attach::Update::Done(id, result) => {
                    let Some(at) = self.chat_pending.iter().position(|p| p.id == id) else { continue };
                    match result {
                        Ok(attachment) => self.chat_pending[at].ready = Some(attachment),
                        Err(why) => {
                            let name = self.chat_pending.remove(at).name;
                            self.queue.push(Toast::new(events::INFO, 4.0, Icon::Info, format!("Can\u{2019}t attach {name}"), why, Rgba::hex(0xff9f0a)).keyed("info"));
                            self.resolve();
                        }
                    }
                }
            }
        }
        self.sync_chat();
    }

    /// More of the answer arrived, or it ended.
    fn on_chat(&mut self) {
        let (text, done, session) = crate::chat::take();
        // The answer now lives in a Claude Code session / Codex thread too.
        if let Some(session) = session {
            self.chat_session = Some(session);
            self.save_chat_session();
            self.sync_chat_header();
        }
        self.chat_feed(&text, done);
    }

    fn chat_feed(&mut self, text: &str, done: Option<Result<(), String>>) {
        let Some(messages) = &mut self.chat else { return };
        if !self.s.chat.busy {
            return;
        }
        if let Some(last) = messages.last_mut().filter(|m| !m.user) {
            last.text.push_str(text);
        }
        if let Some(result) = done {
            self.s.chat.busy = false;
            if messages.last().is_some_and(|m| !m.user && m.text.is_empty()) {
                messages.pop();
            }
            if let Err(why) = result {
                messages.push(crate::chat::Message::new(false, why, true));
            }
            if !self.hidden {
                crate::chat::save(messages);
            }
        }
        self.sync_chat();
    }

    /// The send button doubles as "stop" while an answer streams.
    fn chat_stop(&mut self) {
        crate::chat::cancel();
        self.chat_feed("", Some(Ok(())));
    }

    /// A key press in the chat field.
    fn chat_key(&mut self, key: u32) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match (key, ctrl) {
            (0x1B, _) => self.blur(),
            (0x0D, _) => self.chat_submit(),
            (0x08, _) => {
                self.chat_input.pop();
            }
            // Ctrl+V: text (the usual way an API key gets in), or a picture to attach.
            (0x56, true) => match clipboard::read_text(self.hwnd).filter(|t| !t.is_empty()) {
                Some(text) => self.chat_input.extend(text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(8000)),
                None => {
                    let picture = clipboard::read_dib(self.hwnd).and_then(|dib| image::from_dib(&dib));
                    if let (Some(pixels), Some(_), true) = (picture, &self.chat_key, self.chat_pending.len() < 10) {
                        let id = self.chat_next_id();
                        self.chat_pending.push(crate::chat::Pending { id, name: "Pasted picture".into(), status: "Reading\u{2026}".into(), ready: None });
                        crate::attach::read_pixels(self.hwnd, id, "Pasted picture".into(), pixels);
                    }
                }
            },
            _ => return false,
        }
        self.sync_chat();
        true
    }

    /// Starts a screen tool: the panel gets out of the way and the tool freezes the screen.
    fn snip(&mut self, tool: snip::Tool) {
        if self.s.mode == Mode::Expanded {
            self.blur();
            self.set_mode(Mode::Idle);
        }
        // Tests never read the real screen or write into the user's Pictures folder.
        let dir = match (&self.snapshot, self.hidden) {
            (Some(dir), _) => dir.clone(),
            (None, true) => std::env::temp_dir().join("islet-shots-hidden"),
            (None, false) => unsafe {
                windows::Win32::UI::Shell::SHGetKnownFolderPath(&windows::Win32::UI::Shell::FOLDERID_Pictures, windows::Win32::UI::Shell::KF_FLAG_DEFAULT, None)
                    .ok()
                    .and_then(|path| {
                        let folder = path.to_string().ok();
                        windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
                        folder
                    })
                    .map(|folder| PathBuf::from(folder).join("Islet"))
                    .unwrap_or_else(|| std::env::temp_dir().join("Islet"))
            },
        };
        snip::start(self.hwnd, tool, dir, self.hidden);
    }

    /// Starts recording the monitor under the cursor, or stops the recording.
    fn toggle_recording(&mut self) {
        if self.recording.is_some() || self.record_starting {
            crate::record::stop();
            return;
        }
        if self.s.mode == Mode::Expanded {
            self.blur();
            self.set_mode(Mode::Idle);
        }
        // Tests never record the real screen (hook wparam 16 fakes it instead).
        if self.hidden {
            return;
        }
        let dir = unsafe {
            windows::Win32::UI::Shell::SHGetKnownFolderPath(&windows::Win32::UI::Shell::FOLDERID_Videos, windows::Win32::UI::Shell::KF_FLAG_DEFAULT, None)
                .ok()
                .and_then(|path| {
                    let folder = path.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
                    folder
                })
                .map(|folder| PathBuf::from(folder).join("Islet"))
                .unwrap_or_else(|| std::env::temp_dir().join("Islet"))
        };
        let mut at = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut at);
            // Islet stays out of its own recordings.
            let _ = SetWindowDisplayAffinity(self.hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
        self.record_starting = crate::record::start(self.hwnd, at, crate::record::file_in(&dir));
        if !self.record_starting {
            self.record_ended("Recording failed", "The recorder could not be started".into());
        }
    }

    /// The recording ended (saved or failed): back to normal.
    fn record_ended(&mut self, title: &str, detail: String) {
        self.recording = None;
        self.record_starting = false;
        self.s.recording = false;
        self.s.record_secs = 0;
        unsafe {
            let _ = SetWindowDisplayAffinity(self.hwnd, WDA_NONE);
        }
        let colour = if title.ends_with("saved") { theme::GREEN } else { Rgba::hex(0xff9f0a) };
        self.queue.push(Toast::new(events::INFO, 4.0, Icon::Info, title, detail, colour).keyed("info"));
        self.resolve();
    }

    fn on_record(&mut self, update: crate::record::Update) {
        match update {
            crate::record::Update::Started => {
                self.record_starting = false;
                self.recording = Some(Instant::now());
                self.s.recording = true;
                self.s.record_secs = 0;
                self.resolve();
            }
            crate::record::Update::Saved(path, secs) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.record_ended("Recording saved", format!("{name} \u{00B7} {}:{:02}", secs / 60, secs % 60));
            }
            crate::record::Update::Failed(why) => self.record_ended("Recording failed", why),
        }
    }

    /// Results of the screen tools: onto the clipboard, with a word in the pill.
    fn on_snip(&mut self) {
        // The frozen screen and the editor's buffers are gone: give the memory back soon.
        unsafe { SetTimer(Some(self.hwnd), T_TRIM, 8000, None) };
        for outcome in snip::take() {
            let (title, detail, text) = match outcome {
                snip::Outcome::Shot { path, pixels, save } => {
                    // Not ignored by the clipboard listener: the picture also joins the history.
                    let copied = !self.hidden && clipboard::write_dib(self.hwnd, &image::to_dib(&pixels));
                    let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    match (name, copied, save) {
                        (Some(name), ..) => ("Screenshot saved", name, None),
                        (None, true, false) => ("Screenshot copied", String::new(), None),
                        (None, true, true) => ("Screenshot copied", "Could not save the file".to_string(), None),
                        (None, false, true) => ("Screenshot failed", String::new(), None),
                        // Under --hidden nothing reaches the clipboard.
                        (None, false, false) => ("Screenshot copied", "(test run: clipboard untouched)".to_string(), None),
                    }
                }
                snip::Outcome::Text(text) if text.trim().is_empty() => ("No text found", String::new(), None),
                snip::Outcome::Text(text) => ("Text copied", text.replace("\r\n", " "), Some(text)),
                snip::Outcome::Qr(text) if text.is_empty() => ("No QR code on screen", String::new(), None),
                snip::Outcome::Qr(text) => ("QR code copied", text.replace('\n', " "), Some(text)),
                snip::Outcome::Color(hex) => ("Colour copied", hex.clone(), Some(hex)),
                snip::Outcome::Failed(why) => ("Screen tool failed", why.to_string(), None),
            };
            if let (Some(text), false) = (text, self.hidden) {
                clipboard::write_text(self.hwnd, &text);
            }
            let detail: String = detail.chars().take(80).collect();
            self.queue.push(Toast::new(events::INFO, 3.0, Icon::Info, title, detail, theme::accent()).keyed("info"));
        }
        self.resolve();
    }

    /// The VPN tile: a Windows profile can be switched; any other VPN is only shown.
    fn sync_vpn(&mut self) {
        let status = crate::vpn::status();
        let (on, text) = match (&status.profile, &self.vpn) {
            (Some(_), _) if status.busy => (status.on, if status.on { "Disconnecting\u{2026}" } else { "Connecting\u{2026}" }.to_string()),
            (Some(name), _) if status.on => (true, name.clone()),
            (Some(_), _) if status.failed => (false, "Couldn\u{2019}t connect".to_string()),
            (None, Some(adapter)) | (Some(_), Some(adapter)) => (true, adapter.clone()),
            (Some(_), None) => (false, "Off".to_string()),
            (None, None) => (false, "No profile".to_string()),
        };
        self.s.vpn_on = on;
        self.s.vpn_text = text.encode_utf16().collect();
    }

    /// New status from the watcher thread: mic/camera, VPN, downloads.
    fn on_watch(&mut self) {
        let status = watch::take();
        if self.suspended {
            return;
        }
        (self.s.mic, self.s.cam) = (status.mic, status.cam);
        if self.vpn_known && status.vpn != self.vpn {
            let toast = match &status.vpn {
                Some(name) => Toast::new(events::VPN, 3.0, Icon::Shield, "VPN connected", name.clone(), theme::GREEN),
                None => Toast::new(events::VPN, 3.0, Icon::Shield, "VPN disconnected", "", Rgba::hex(0xff9f0a)),
            };
            self.queue.push(toast.keyed("vpn"));
        }
        self.vpn = status.vpn;
        self.vpn_known = true;
        if self.s.mode == Mode::Expanded && self.s.tab == 7 {
            crate::vpn::refresh(self.hwnd, false);
            self.sync_vpn();
        }
        self.download_active = status.download.is_some();
        if let Some(download) = status.download {
            self.s.download = download;
        }
        for finished in status.finished {
            let mut toast = Toast::new(events::DOWNLOAD_DONE, 4.5, Icon::Info, "Downloaded", finished.name.clone(), theme::GREEN);
            toast.done = Some(finished);
            self.queue.push(toast);
        }
        self.resolve();
    }

    /// Queues alerts for every timer and reminder that is due, then re-arms.
    fn fire_due(&mut self) {
        for due in self.store.take_due(timer::now()) {
            let toast = match due {
                Due::Timer(label) => Toast::new(events::TIMER_DONE, 12.0, Icon::Timer, "Timer finished", label, Rgba::hex(0xff9f0a)),
                Due::Reminder(text) => Toast::new(events::REMINDER, 15.0, Icon::Bell, "Reminder", text, Rgba::hex(0xffd60a)),
            };
            self.queue.push(toast.important());
        }
        self.schedule();
        self.resolve();
    }

    /// One one-shot timer for whatever is due next (re-checked at least every 30 minutes).
    fn schedule(&self) {
        self.kill(T_SCHEDULE);
        if let Some(secs) = self.store.next_due_in(timer::now()) {
            unsafe { SetTimer(Some(self.hwnd), T_SCHEDULE, (secs * 1000 + 40).min(1_800_000) as u32, None) };
        }
    }

    /// Runs a typed phrase such as "timer 10 min" (see `timer::parse`).
    fn command(&mut self, text: &str) -> bool {
        let now = timer::now();
        let parsed = timer::parse(text, now);
        let (title, detail) = match &parsed {
            Some(timer::Command::Timer { secs, label }) => {
                let span = if *secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
                ("Timer set", if label.is_empty() { span } else { format!("{span} \u{00B7} {label}") })
            }
            Some(timer::Command::Reminder { at, daily, text }) => {
                let (h, m) = timer::clock_of(*at);
                let day = if *daily { "every day".to_string() } else { timer::day_label(*at, now) };
                ("Reminder set", format!("{day} {h:02}:{m:02} \u{00B7} {text}"))
            }
            Some(timer::Command::CancelTimers) => ("Timers cancelled", String::new()),
            Some(timer::Command::CancelReminders) => ("Reminders cancelled", String::new()),
            None => ("Not understood", text.to_string()),
        };
        self.queue.push(Toast::new(events::INFO, 2.6, Icon::Info, title, detail, theme::accent()).keyed("info"));
        let understood = parsed.is_some();
        if let Some(command) = parsed {
            self.store.apply(command, now);
        }
        self.fire_due();
        understood
    }

    fn on_caps(&mut self) {
        let on = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
        let title = if on { "Caps Lock on" } else { "Caps Lock off" };
        self.queue.push(Toast::new(events::KEYBOARD, 1.6, Icon::Keyboard, title, "", theme::accent()).keyed("keys"));
        // Replace a keyboard toast that is already showing instead of waiting behind it.
        if self.toast.as_ref().is_some_and(|t| t.key == Some("keys")) {
            self.dismiss_toast();
        } else {
            self.resolve();
        }
    }

    /// The input language of the foreground window changed (`HSHELL_LANGUAGE`).
    fn on_layout(&mut self, hkl: isize) {
        let lang = (hkl as usize & 0xffff) as u32;
        if lang == 0 || lang == self.layout {
            return;
        }
        let first = self.layout == 0;
        self.layout = lang;
        if first {
            return; // just learning the current layout
        }
        let mut locale = [0u16; 85];
        let info = |kind: u32, locale: &[u16; 85]| unsafe {
            let mut out = [0u16; 80];
            let len = GetLocaleInfoEx(windows::core::PCWSTR(locale.as_ptr()), kind, Some(&mut out));
            String::from_utf16_lossy(&out[..(len.max(1) - 1) as usize])
        };
        if unsafe { LCIDToLocaleName(lang, Some(&mut locale), 0) } == 0 {
            return;
        }
        let code = info(LOCALE_SISO639LANGNAME, &locale).to_uppercase();
        let name = info(LOCALE_SLOCALIZEDLANGUAGENAME, &locale);
        self.queue.push(Toast::new(events::KEYBOARD, 1.6, Icon::Keyboard, code, name, theme::accent()).keyed("keys"));
        if self.toast.as_ref().is_some_and(|t| t.key == Some("keys")) {
            self.dismiss_toast();
        } else {
            self.resolve();
        }
    }

    /// Starts watching for Windows' toast popup (see `notify`) and takes a
    /// baseline of the notifications that already exist.
    fn watch_toasts(&mut self) {
        if !self.cfg.notifications || self.demo.is_some() {
            return;
        }
        self.hook_shell_host();
        self.seen_ready = false;
        notify::read_async(self.hwnd);
    }

    /// (Re)attaches the window-event hooks to the shell host process. Listening
    /// to every process costs about 2 % CPU; one process costs nothing measurable.
    /// Called again on foreground changes in case the shell host (re)started.
    fn hook_shell_host(&mut self) {
        if !self.cfg.notifications || self.demo.is_some() || self.suspended {
            return;
        }
        if self.shell_host.as_ref().is_some_and(|host| host.alive()) {
            return;
        }
        self.unhook_shell_host();
        let Some(host) = notify::ShellHost::find() else { return };
        const SHOW: u32 = 0x8002; // EVENT_OBJECT_SHOW
        const UNCLOAKED: u32 = 0x8018; // EVENT_OBJECT_UNCLOAKED
        for (slot, event) in self.toast_hooks.iter_mut().zip([SHOW, UNCLOAKED]) {
            *slot = unsafe { SetWinEventHook(event, event, None, Some(on_window_shown), host.pid, 0, WINEVENT_OUTOFCONTEXT) };
        }
        self.shell_host = Some(host);
    }

    fn unhook_shell_host(&mut self) {
        for hook in &mut self.toast_hooks {
            if !hook.is_invalid() {
                unsafe {
                    let _ = UnhookWinEvent(*hook);
                }
                *hook = HWINEVENTHOOK::default();
            }
        }
        self.shell_host = None;
    }

    fn unwatch_toasts(&mut self) {
        self.unhook_shell_host();
        self.kill(T_NOTIFY);
    }

    /// A fresh notification list arrived: show the ones not seen before.
    fn on_notifications(&mut self) {
        let Some(notes) = notify::take() else { return };
        if self.suspended {
            return;
        }
        if self.seen_ready {
            // While an agent's hooks are talking to Islet, its Windows notifications
            // repeat what the island already showed (or is about to show).
            let hooks_live = self.agent_heard.is_some_and(|at| at.elapsed() < AGENT_QUIET);
            for note in notes.iter().filter(|n| !self.seen.contains(&n.id)) {
                let ai = events::from_ai_agent(&note.app, &note.title, &note.body);
                if ai && hooks_live {
                    continue;
                }
                let title = if note.title.is_empty() { note.app.clone() } else { note.title.clone() };
                let detail = match (note.title.is_empty(), note.body.is_empty()) {
                    (true, _) => note.body.clone(),
                    (false, true) => note.app.clone(),
                    (false, false) => format!("{} \u{00B7} {}", note.app, note.body),
                };
                let toast = Toast::new(events::NOTIFICATION, 4.0, Icon::Bell, title, detail, theme::accent());
                self.queue.push(if ai { toast.keyed(events::AI_NOTE) } else { toast });
            }
        }
        self.seen = notes.iter().map(|n| n.id).collect();
        self.seen_ready = true;
        self.resolve();
    }

    // ----------------------------------------------------------- suspend --

    fn check_gamemode(&mut self) {
        if self.demo.is_some() || self.hidden {
            return;
        }
        let want = self.paused || gamemode::should_suspend(self.hwnd, &self.cfg.suspend_apps);
        if want && !self.suspended {
            self.suspend();
        } else if !want && self.suspended {
            self.resume();
        }
    }

    /// Deep sleep: no window, no rendering, no hook, no GPU objects. Only the
    /// foreground-change event and the coalesced watcher timer stay alive.
    fn suspend(&mut self) {
        self.suspended = true;
        self.sync_stats();
        // Silent while a game is in front (the player thread winds down by itself).
        crate::sound::ambience(crate::sound::Ambience::SILENT);
        crate::sound::enable(false);
        // Games get their own Alt+Space back, and the keyboard if Islet had it.
        if self.s.mode == Mode::Command {
            self.close_command();
        }
        self.blur();
        self.release_focus();
        self.save_note();
        self.notes = None;
        self.thumbs.clear();
        self.press = None;
        self.unregister_hotkey();
        self.index = None;
        for id in [T_NOTE, T_CLOCK, T_COLLAPSE, T_AUTOHIDE, T_HOVER, T_DEBUG, T_TRIM, T_RETRY, T_TOAST, T_SECOND, T_CAPS, T_TICK, T_OUTPUTS, T_PASTE, T_INDEX] {
            self.kill(id);
        }
        // Important alerts (finished timer, reminder) wait for the wake-up;
        // `T_SCHEDULE` stays armed so they are still collected while asleep.
        self.unwatch_toasts();
        self.watch = None;
        if let Some(toast) = self.toast.take() {
            if let Some(permit) = toast.permit {
                agent::reply(permit.id, &agent::permit_reply(permit.claude, None));
                self.asks.remove(&permit.id);
            } else if toast.important {
                self.queue.push(toast);
            }
        }
        // Pending permission requests fall back to the agent's own prompt.
        for permit in self.queue.take_permits() {
            agent::reply(permit.id, &agent::permit_reply(permit.claude, None));
            self.asks.remove(&permit.id);
        }
        self.queue.keep_important();
        self.hook = None;
        self.media = None;
        self.capture = None;
        self.drag_volume = false;
        self.gfx = None;
        self.reset_layers();
        self.dirty = false;
        self.hover_armed = false;
        self.collapse_armed = false;
        self.set_interactive(false);
        // What the tabs loaded is read again on the first use after waking.
        self.history = None;
        self.clip_shown.clear();
        self.shelf = None;
        self.dock_icons = Vec::new();
        self.chat_thumbs = Vec::new();
        if !self.s.chat.busy && self.chat_pending.is_empty() {
            self.chat = None;
            self.s.chat.rows = Vec::new();
        }
        self.rates = None;
        self.kill(T_MASCOT);
        self.sky_live = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        give_memory_back();
    }

    fn resume(&mut self) {
        self.suspended = false;
        self.sync_stats();
        crate::sound::enable(!self.hidden && self.demo.is_none());
        // Come back as the resting pill, without replaying a collapse.
        self.s.set_pinned(false);
        self.s.set_mode(Mode::Idle);
        let (w, h, r) = self.s.target_dims();
        self.s.w.snap(w);
        self.s.h.snap(h);
        self.s.r.snap(r);
        self.s.reveal = 0.0;
        self.s.bump.snap(0.0);
        self.place();
        self.apply_theme(false);
        self.hook = Some(MouseHook::start(self.hwnd, self.cfg.caps_popup));
        if self.demo.is_none() {
            self.media = Some(Media::start(self.hwnd));
            self.watch = Watch::start(self.hwnd, self.downloads.clone());
            self.vpn_known = false;
        }
        unsafe {
            if self.stats.is_some() {
                SetTimer(Some(self.hwnd), T_DEBUG, 500, None);
            }
            if !self.hidden {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.arm_clock();
        self.arm_autohide();
        self.update_zone();
        self.repaint();
        self.register_hotkey();
        // Show what was queued while asleep (finished timers, reminders).
        self.resolve();
        // Notifications that arrived meanwhile become the new baseline; they are not replayed.
        self.watch_toasts();
    }

    // ---------------------------------------------------------- messages --

    pub fn on_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_APP_CURSOR => {
                input::ack();
                if self.suspended || self.demo.is_some() {
                    return Some(LRESULT(0));
                }
                if self.cfg.follow_cursor {
                    let (x, y) = input::cursor();
                    let m = self.monitor;
                    if x < m.left || x >= m.right || y < m.top || y >= m.bottom {
                        self.replace();
                    }
                }
                if self.cfg.autohide && self.s.mode == Mode::Idle {
                    self.arm_autohide();
                }
                self.track_cursor();
                if !self.s.at_rest() {
                    self.invalidate();
                }
                // The eyes follow (and the input wakes a dozing character).
                if self.mascot_visible() {
                    self.update_mascot();
                }
                Some(LRESULT(0))
            }
            // Test hook (`--test-hooks` only): drive the hover logic with a fake
            // cursor given in window dp, so tests never move the real mouse.
            // wparam 1 = set (lparam = x | y << 16), 0 = back to the real cursor.
            // wparam 3 (with `--snapshot=<dir>`): save the current frame as snap<lparam>.bmp.
            WM_APP_TEST if self.test_hooks && wparam.0 == 3 => {
                if lparam.0 >= 1000 {
                    // lparam 1000 + n: "honest" snapshot. No forced repaint, only what
                    // the damage tracking draws by itself, as on the real screen
                    // (catches changes that forget `repaint`).
                    self.snap_honest = true;
                    self.snap_pending = Some((lparam.0 - 1000, 2));
                    self.invalidate();
                    return Some(LRESULT(0));
                }
                // Two full repaints first, so the buffer that gets read is current.
                self.snap_honest = false;
                self.snap_pending = Some((lparam.0, 2));
                self.repaint();
                Some(LRESULT(0))
            }
            // wparam 2: fake "music playing" on (lparam 1) or off (lparam 0);
            // lparam 2 / 3 = on with the wave's music level pinned loud / quiet
            // (real audio levels are then ignored).
            WM_APP_TEST if self.test_hooks && wparam.0 == 2 => {
                let on = lparam.0 != 0;
                self.music_accent = (Rgba::hex(0xff7a45), Rgba::hex(0xff3d8b));
                theme::set_music_tint(on.then_some(self.music_accent.0));
                self.s.set_music(on, self.music_accent.0, self.music_accent.1);
                self.eq_levels = [0.9, 0.45, 0.7, 0.25];
                self.eq_place.2 = -1.0;
                self.level_pinned = lparam.0 >= 2;
                if self.level_pinned {
                    self.s.wave_level = if lparam.0 == 2 { 1.0 } else { 0.0 };
                    if let Some(gfx) = &self.gfx {
                        if gfx.wave_shown() {
                            gfx.wave_level(self.s.wave_mid_amp());
                            gfx.commit();
                        }
                    }
                }
                // lparam 4: a beat drop now, right of the middle.
                if lparam.0 == 4 {
                    self.s.drop_from_wave(crate::state::CX + 60.0);
                }
                // lparam 5: made-up synced lyrics from the start of the song (lines
                // every 2 s); 6: flip the music card to its lyrics view.
                if lparam.0 == 5 {
                    let lrc = "[00:00.10]Made-up words for a test\n[00:02.00]A second line slides in\n[00:04.00]\n[00:06.00]And a third one after a pause\n[00:08.00]The last line of all";
                    self.cfg.lyrics = true;
                    self.s.lyric.enabled = true;
                    let m = &mut self.s.media;
                    (m.session, m.playing, m.duration, m.position, m.stamp) = (true, true, 30.0, 0.0, Instant::now());
                    m.title = "Test song".encode_utf16().collect();
                    m.artist = "Nobody".encode_utf16().collect();
                    self.lyrics_key = crate::lyrics::key("Test song", "Nobody");
                    self.show_lyrics(crate::lyrics::Answer::Found(crate::lyrics::Lyrics { lines: crate::lyrics::parse_lrc(lrc), synced: true }));
                }
                // lparam 7: a made-up song from a player that allows seeking,
                // shuffle (on) and repeat (this track).
                if lparam.0 == 7 {
                    let m = &mut self.s.media;
                    (m.session, m.playing, m.duration, m.position, m.stamp) = (true, true, 214.0, 61.0, Instant::now());
                    m.title = "Test song".encode_utf16().collect();
                    m.artist = "Nobody".encode_utf16().collect();
                    m.source = "PLAYER".encode_utf16().collect();
                    m.can = crate::media::Abilities { seek: true, shuffle: Some(true), repeat: Some(1) };
                }
                if lparam.0 == 6 {
                    self.s.lyric.card = !self.s.lyric.card;
                    self.s.lyric.scroll.snap(self.s.lyric.index.unwrap_or(0) as f32);
                }
                self.resolve();
                self.invalidate();
                Some(LRESULT(0))
            }
            // wparam 24: open the panel pinned on tab `lparam` (0 Home .. 7 Controls).
            WM_APP_TEST if self.test_hooks && wparam.0 == 24 => {
                self.s.set_pinned(true);
                self.set_mode(Mode::Expanded);
                self.s.set_tab((lparam.0 as usize).min(crate::state::TAB_COUNT - 1));
                self.enter_tab();
                self.repaint();
                Some(LRESULT(0))
            }
            // wparam 22 (with `--snapshot`): the settings window as `settings<lparam>.bmp`.
            WM_APP_TEST if self.test_hooks && wparam.0 == 22 => {
                // Twice: the buffer that gets read is the one drawn two presents ago.
                self.redraw_settings();
                self.redraw_settings();
                if let (Some(dir), Some(Some(g))) = (&self.snapshot, self.settings_win.as_ref().map(|w| w.gfx.as_ref())) {
                    let _ = g.save_bmp(&dir.join(format!("settings{}.bmp", lparam.0)));
                }
                Some(LRESULT(0))
            }
            // wparam 21: a pretend pointer for the Ctrl + drag (the real mouse is never
            // moved): lparam = screen x in pixels; it moves a drag in progress.
            // Prop `IsletX` = the window's left edge.
            WM_APP_TEST if self.test_hooks && wparam.0 == 21 => {
                self.fake_screen_x = Some(lparam.0 as i32);
                self.move_island_drag();
                unsafe {
                    let _ = SetPropW(self.hwnd, w!("IsletX"), Some(windows::Win32::Foundation::HANDLE(self.origin.0 as isize as *mut _)));
                }
                Some(LRESULT(0))
            }
            // wparam 20 (with `--snapshot`): lparam 1 draws every character in a
            // dozen poses over everything (ui::draw_gallery), 0 stops.
            WM_APP_TEST if self.test_hooks && self.snapshot.is_some() && wparam.0 == 20 => {
                self.s.gallery = lparam.0 != 0;
                self.repaint();
                Some(LRESULT(0))
            }
            // wparam 19 (hidden only): settings page. lparam 0 = open it pinned and
            // scrolled to the "Music wave" rows; otherwise click a row through the
            // real path: lparam = row index | option index << 8 (the hidden
            // instance edits its temporary copy of the settings file).
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 19 => {
                if lparam.0 == 0 {
                    self.open_settings(true);
                } else {
                    let (row, part) = (lparam.0 as usize & 0xff, (lparam.0 as usize >> 8) & 0xff);
                    if row < crate::settings::ROWS.len() {
                        self.settings_click(row, part);
                    }
                }
                Some(LRESULT(0))
            }
            // wparam 4: fake microphone (bit 0) / camera (bit 1) in use, for looking at the dots.
            WM_APP_TEST if self.test_hooks && wparam.0 == 4 => {
                (self.s.mic, self.s.cam) = (lparam.0 & 1 != 0, lparam.0 & 2 != 0);
                self.repaint();
                Some(LRESULT(0))
            }
            WM_APP_WATCH => {
                self.on_watch();
                Some(LRESULT(0))
            }
            WM_APP_TOASTWIN => {
                // Several surfaces can appear in a burst; read once, a moment later.
                if !self.suspended {
                    unsafe { SetTimer(Some(self.hwnd), T_NOTIFY, 350, None) };
                }
                Some(LRESULT(0))
            }
            WM_HOTKEY if wparam.0 == HOTKEY_VOICE as usize => {
                self.toggle_voice();
                Some(LRESULT(0))
            }
            WM_HOTKEY => {
                if self.s.mode == Mode::Command {
                    self.close_command();
                } else {
                    self.open_command();
                }
                Some(LRESULT(0))
            }
            WM_CHAR => {
                self.on_char(wparam.0 as u16);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => self.on_key(wparam.0 as u32).then_some(LRESULT(0)),
            // Another window became active while Islet had the keyboard.
            WM_ACTIVATE if wparam.0 & 0xffff == 0 => {
                self.on_focus_lost();
                None
            }
            WM_CLIPBOARDUPDATE => {
                self.on_clipboard();
                Some(LRESULT(0))
            }
            WM_APP_CONTROL => {
                self.s.radios = control::radios();
                self.sync_vpn();
                self.repaint();
                Some(LRESULT(0))
            }
            // The wheel over the pill, or anywhere on the open Home tab, sets the
            // volume (2 % a notch); the pill shows it for a moment.
            WM_MOUSEWHEEL if !self.suspended && (self.s.mode == Mode::Idle || (self.s.mode == Mode::Expanded && self.s.tab == 0)) => {
                let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                let level = crate::anim::clamp01(self.s.volume + 0.02 * notches);
                if self.s.mode == Mode::Idle {
                    self.s.volume_shown = true;
                    unsafe { SetTimer(Some(self.hwnd), T_VOLUME, VOLUME_SHOWN_MS, None) };
                    self.repaint();
                }
                // `--hidden` test runs never touch the real volume.
                if self.hidden {
                    self.s.volume = level;
                    self.repaint();
                } else {
                    self.set_volume(level);
                }
                Some(LRESULT(0))
            }
            // The wheel scrolls the clipboard list, the shelf, the note or the note titles.
            WM_MOUSEWHEEL if self.s.mode == Mode::Expanded && self.s.tab == 4 => {
                let up = ((wparam.0 >> 16) & 0xffff) as i16 > 0;
                self.s.chat.scroll += if up { 48.0 } else { -48.0 };
                self.sync_chat();
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL if self.s.mode == Mode::Expanded && matches!(self.s.tab, 1 | 2 | 5) => {
                let up = ((wparam.0 >> 16) & 0xffff) as i16 > 0;
                let step = |at: usize| if up { at.saturating_sub(1) } else { at + 1 };
                match self.s.tab {
                    1 => {
                        self.clip_scroll = step(self.clip_scroll);
                        self.refresh_clipboard();
                    }
                    2 => {
                        self.shelf_scroll = step(self.shelf_scroll);
                        self.refresh_shelf();
                    }
                    _ if self.s.to_panel(self.cursor_dp().0, 0.0).0 >= NOTE_TEXT_X - 14.0 => {
                        self.s.note.scroll = (self.s.note.scroll + if up { -48.0 } else { 48.0 }).max(0.0);
                        self.sync_note(false);
                    }
                    _ => {
                        self.note_scroll = step(self.note_scroll);
                        self.sync_note(false);
                    }
                }
                Some(LRESULT(0))
            }
            crate::voice::WM_APP_VOICE => {
                for update in crate::voice::take() {
                    self.on_voice(update);
                }
                Some(LRESULT(0))
            }
            // wparam 18: the character. lparam 0-2 picks cat / ghost / jellyfish (100 + i
            // any of `mascot::CAST`), 3 makes it happy, 4 puts it to sleep, 5 wakes it,
            // 6 starts "thinking", 7 a jump, 8 blinks and idle acts as usual (for
            // measuring; otherwise both are pushed an hour away so pictures are
            // predictable). 1000 + act * 100 + percent holds `Act::ALL[act]` at that
            // moment; 999 lets go.
            WM_APP_TEST if self.test_hooks && wparam.0 == 18 => {
                let n = lparam.0;
                match n {
                    0..=2 => self.s.mascot = Some(n as usize),
                    3 => self.mascot_happy = Some(Instant::now() + std::time::Duration::from_secs(6)),
                    4 => (self.mascot_away, self.mascot_awake) = (true, false),
                    // Awake even when the real keyboard and mouse have been idle.
                    5 => (self.mascot_away, self.mascot_awake) = (false, true),
                    6 => self.s.agent_working = true,
                    7 => self.mascot_doing = Some((crate::mascot::Act::Jump, Instant::now())),
                    999 => self.mascot_freeze = None,
                    100..=199 if ((n - 100) as usize) < crate::mascot::CAST.len() => self.s.mascot = Some((n - 100) as usize),
                    1000..=2099 => {
                        let act = crate::mascot::Act::ALL.get(((n - 1000) / 100) as usize).copied().unwrap_or(crate::mascot::Act::None);
                        self.mascot_freeze = Some((act, ((n - 1000) % 100) as f32 / 100.0));
                    }
                    _ => {}
                }
                let later = std::time::Duration::from_secs(if n == 8 { 0 } else { 3600 });
                self.mascot_next_blink = Instant::now() + later;
                self.mascot_next_idle = Instant::now() + later;
                self.mascot_greeted = Instant::now();
                self.resolve();
                Some(LRESULT(0))
            }
            // wparam 17 (hidden only): pretend voice input. lparam 0 starts, 1 partial
            // words, 2 the final text (kept in `voice_typed`, never typed), 3 a failure.
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 17 => {
                use crate::voice::Update;
                if lparam.0 == 0 {
                    self.voice_active = true;
                }
                let update = match lparam.0 {
                    0 => Update::Listening,
                    1 => Update::Partial("please send the report to the team tomorrow morning".into()),
                    2 => Update::Text("please send the report to the team tomorrow morning".into()),
                    _ => Update::Failed("No microphone".into()),
                };
                self.on_voice(update);
                Some(LRESULT(0))
            }
            crate::update::WM_APP_UPDATE => {
                if let Some(release) = crate::update::take() {
                    let title = format!("Islet {} is available", release.version);
                    if let Ok(mut update) = tray::UPDATE.lock() {
                        *update = Some((release.version, release.page));
                    }
                    self.queue.push(Toast::new(events::INFO, 6.0, Icon::Info, title, "Get it from the tray menu", theme::accent()).keyed("update"));
                    self.resolve();
                }
                Some(LRESULT(0))
            }
            crate::record::WM_APP_RECORD => {
                for update in crate::record::take() {
                    self.on_record(update);
                }
                Some(LRESULT(0))
            }
            // wparam 16: a pretend recording (lparam 0 starts, 1 ends saved, 2 ends failed).
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 16 => {
                let update = match lparam.0 {
                    0 => crate::record::Update::Started,
                    1 => crate::record::Update::Saved(PathBuf::from("Recording 2026-11-14 120000.mp4"), 75),
                    _ => crate::record::Update::Failed("No H.264 encoder".into()),
                };
                self.on_record(update);
                if lparam.0 == 0 {
                    // Pretend it has been running for a minute and a quarter.
                    self.recording = Instant::now().checked_sub(std::time::Duration::from_secs(75));
                    self.s.record_secs = 75;
                    self.repaint();
                }
                Some(LRESULT(0))
            }
            crate::currency::WM_APP_RATES => {
                if let Some(rates) = crate::currency::take() {
                    self.rates = Some(rates);
                    if self.s.mode == Mode::Command && !self.cmd_query.is_empty() {
                        self.refresh_command();
                    }
                }
                Some(LRESULT(0))
            }
            // wparam 15: currency on, with made-up rates (no network).
            WM_APP_TEST if self.test_hooks && wparam.0 == 15 => {
                self.cfg.currency = true;
                self.rates = Some(crate::currency::sample());
                Some(LRESULT(0))
            }
            command::WM_APP_STORE => {
                if let Some(index) = &mut self.index {
                    index.merge_store();
                    if self.s.mode == Mode::Command && !self.cmd_query.is_empty() {
                        self.refresh_command();
                    }
                }
                Some(LRESULT(0))
            }
            WM_APP_SHELF => {
                self.on_shelf(wparam.0, lparam);
                Some(LRESULT(0))
            }
            // A right click unpins a dock item.
            WM_RBUTTONDOWN => {
                let (x, y) = self.client_dp(lparam);
                if let Some(Hit::Dock(slot)) = ui::hit(&self.s, x, y) {
                    self.dock().remove(slot);
                    self.refresh_dock(true);
                }
                Some(LRESULT(0))
            }
            crate::weather::WM_APP_WEATHER => {
                match crate::weather::take() {
                    Some(Ok(weather)) => {
                        // One heads-up per spell of rain, about an hour ahead.
                        if weather.rain_soon && !self.rain_warned {
                            self.queue.push(Toast::new(events::INFO, 6.0, Icon::Info, "Rain on the way", "Expected within the hour", Rgba::hex(0x64b5ff)).keyed("rain"));
                        }
                        self.rain_warned = weather.rain_soon || weather.sky.wet();
                        self.s.weather = Some(weather);
                    }
                    // A failed refresh keeps the last forecast on screen.
                    Some(Err(why)) if self.s.weather.is_none() => self.s.weather_note = why.encode_utf16().collect(),
                    _ => {}
                }
                self.repaint();
                self.resolve();
                Some(LRESULT(0))
            }
            crate::sysstat::WM_APP_STATS => {
                if let Some(stats) = crate::sysstat::take() {
                    if !self.stats_fake {
                        self.s.stats = Some(stats);
                        let pill = self.s.stats_pill && self.s.mode != Mode::Expanded && matches!(self.s.pill, Pill::Clock | Pill::Mascot);
                        if (self.s.mode == Mode::Expanded && self.s.tab == 3) || pill {
                            self.repaint();
                        }
                    }
                }
                // Still wanted? (A suspend or a closed tab stops the worker.)
                self.sync_stats();
                Some(LRESULT(0))
            }
            // wparam 26: island colour `ISLAND_COLOURS[lparam]`, as a settings click sets it
            // (under `--hidden` that is the hidden copy of the settings file).
            WM_APP_TEST if self.test_hooks && wparam.0 == 26 => {
                // (100 + n: card colour `CARD_COLOURS[n]`.)
                if lparam.0 >= 100 {
                    if let Some((name, _, _)) = crate::theme::CARD_COLOURS.get(lparam.0 as usize - 100) {
                        self.change_setting("card_colour", name);
                    }
                } else if let Some((name, _, _)) = crate::theme::ISLAND_COLOURS.get(lparam.0 as usize) {
                    self.change_setting("island_colour", name);
                }
                Some(LRESULT(0))
            }
            // wparam 25: made-up system load (lparam 1 also shows it in the pill).
            WM_APP_TEST if self.test_hooks && wparam.0 == 25 => {
                self.stats_fake = true;
                self.s.stats = Some(crate::sysstat::Stats::sample_data());
                if lparam.0 == 1 {
                    self.s.stats_pill = true;
                }
                self.sync_stats();
                self.repaint();
                Some(LRESULT(0))
            }
            crate::lyrics::WM_APP_LYRICS => {
                if let Some((key, answer)) = crate::lyrics::take() {
                    match answer {
                        Ok(answer) => {
                            // Kept for when the song comes round again (the last dozen).
                            if self.lyrics_cache.len() >= 12 {
                                self.lyrics_cache.remove(0);
                            }
                            self.lyrics_cache.push((key, answer.clone()));
                            if key == self.lyrics_key {
                                self.show_lyrics(answer);
                            }
                        }
                        // A failed lookup is not remembered: the next play of the song tries again.
                        Err(_) if key == self.lyrics_key => {
                            self.lyrics_key = 0;
                            self.s.lyric.clear("Lyrics could not be reached right now");
                            self.repaint();
                        }
                        Err(_) => {}
                    }
                }
                Some(LRESULT(0))
            }
            crate::chat::WM_APP_CHAT => {
                self.on_chat();
                Some(LRESULT(0))
            }
            crate::attach::WM_APP_ATTACH => {
                self.on_attach();
                Some(LRESULT(0))
            }
            // wparam 14 (hidden only): attach a sample PDF, a text file and a picture
            // through the real readers (files in a temporary folder).
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 14 => {
                let dir = std::env::temp_dir().join("islet-chat-hidden");
                let _ = std::fs::create_dir_all(&dir);
                let (pdf, notes) = (dir.join("Report Q3.pdf"), dir.join("notes.txt"));
                let _ = std::fs::write(&pdf, crate::attach::sample_pdf("Quarterly report 2026"));
                let _ = std::fs::write(&notes, "Meeting notes\n- ship phase 6\n- measure memory\n");
                self.chat_attach(vec![pdf, notes]);
                let mut bgra = Vec::with_capacity(320 * 200 * 4);
                for y in 0..200u32 {
                    for x in 0..320u32 {
                        bgra.extend_from_slice(&[(x * 255 / 320) as u8, (y * 255 / 200) as u8, 160, 255]);
                    }
                }
                let id = self.chat_next_id();
                self.chat_pending.push(crate::chat::Pending { id, name: "Pasted picture".into(), status: "Reading\u{2026}".into(), ready: None });
                crate::attach::read_pixels(self.hwnd, id, "Pasted picture".into(), Pixels { w: 320, h: 200, bgra });
                self.sync_chat();
                Some(LRESULT(0))
            }
            // wparam 13: chat without a network (hidden only). lparam 0 switches the
            // feature on with a pretend key, 1 streams a piece of a sample answer,
            // 2 ends the answer, 3 ends it with an error, 4 forgets the key.
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 13 => {
                match lparam.0 {
                    0 => {
                        self.cfg.chat = true;
                        self.chat = Some(Vec::new());
                        self.chat_key = Some("test".into());
                        self.open_chat();
                    }
                    1 => self.chat_feed("# Short answer\nIslet keeps **everything local**:\n- the key is encrypted with DPAPI\n- the conversation too\n\nAsk away, and scroll with the wheel when the answer gets longer than the panel.", None),
                    2 => self.chat_feed("", Some(Ok(()))),
                    3 => self.chat_feed("", Some(Err("The API key was not accepted. Type /key to enter another one.".into()))),
                    _ => {
                        self.chat_key = None;
                        self.sync_chat();
                    }
                }
                Some(LRESULT(0))
            }
            // wparam 23 (hidden only): looks and pill effects without touching any file.
            // lparam 0 default look, 1 teal / tinted / capsule / wide / bouncy, 2 pink /
            // gradient / square / compact, Chat and Controls hidden, six quick actions;
            // 3 a plain toast (splash), 4 a timer alert (splash + pulsing outline).
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 23 => {
                use crate::config::Look;
                match lparam.0 {
                    0..=2 => {
                        self.cfg.look = match lparam.0 {
                            1 => Look { accent: "teal".into(), surface: "tinted".into(), corners: (34.0, 30.0), pill_size: 1.15, motion: (330.0, 14.0), ..Look::default() },
                            2 => Look { accent: "pink".into(), surface: "gradient".into(), corners: (9.0, 12.0), pill_size: 0.9, hidden_tabs: (1 << 4) | (1 << 7), quick: vec![8, 0, 9, 4, 10, 11], ..Look::default() },
                            _ => Look::default(),
                        };
                        self.apply_look();
                    }
                    3 => self.queue.push(Toast::new(events::INFO, 3.0, Icon::Info, "Copied", "Screenshot saved to Pictures", theme::accent())),
                    // 5: unpin and collapse the panel.
                    5 => {
                        self.s.set_pinned(false);
                        self.set_mode(Mode::Idle);
                    }
                    // 6: open the settings window (never shown under --hidden).
                    6 => self.open_settings(true),
                    // 7..9: layouts. 7 no music card, six actions; 8 wide + tall panel,
                    // tall pill, no volume or dock, five actions, two tiles and three
                    // buttons hidden away; 9 narrow + short panel, short pill, only
                    // the volume row and the dock.
                    7..=9 => {
                        self.cfg.look = match lparam.0 {
                            7 => Look { home: (false, true, true), quick: vec![8, 0, 9, 4, 10, 11], ..Look::default() },
                            8 => Look { panel: (1.14, 1.15), pill_height: 1.18, home: (true, false, false), quick: vec![0, 1, 2, 9, 10], hidden_tiles: 0b0110, hidden_power: 0b0100, ..Look::default() },
                            _ => Look { panel: (0.88, 0.9), pill_height: 0.85, home: (false, true, true), quick: Vec::new(), hidden_tiles: 0b1111, ..Look::default() },
                        };
                        self.apply_look();
                    }
                    // 10: open the panel pinned on the Controls tab.
                    10 => {
                        self.s.set_pinned(true);
                        self.set_mode(Mode::Expanded);
                        self.s.set_tab(7);
                        self.enter_tab();
                    }
                    _ => self.queue.push(Toast::new(events::TIMER_DONE, 5.0, Icon::Info, "Timer done", "Tea is ready", Rgba::hex(0xff9f40))),
                }
                self.resolve();
                self.repaint();
                Some(LRESULT(0))
            }
            // wparam 12: a made-up forecast (lparam 0 day, 1 rainy night, 2 snow, 3 storm,
            // 4 clear night, 5 fog, 6 sunset, 7 no air quality); 10 + v picks view v,
            // 20 + d toggles day d, 30 clicks the sky's middle, 40 + i jumps the clock
            // i seconds ahead (for snapshots mid-animation).
            WM_APP_TEST if self.test_hooks && wparam.0 == 12 => {
                use crate::weather::Sky;
                let mut weather = crate::weather::sample();
                match lparam.0 {
                    10..=12 => {
                        self.s.wx.view = lparam.0 as usize - 10;
                        self.s.wx.since = self.s.sky_phase;
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                    20..=26 => {
                        let day = lparam.0 as usize - 20;
                        self.s.wx.day = if self.s.wx.day == Some(day) { None } else { Some(day) };
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                    30 => {
                        self.s.wx.poke = Some((crate::state::CX - crate::state::PANEL_W * 0.5 + 16.0 + 118.0, 140.0, self.s.sky_phase));
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                    40..=99 => {
                        self.s.sky_phase += (lparam.0 - 40) as f32;
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                    // 9: open the panel pinned on the Weather tab (8: on Home).
                    8 | 9 => {
                        self.s.set_pinned(true);
                        self.set_mode(Mode::Expanded);
                        self.s.set_tab(if lparam.0 == 8 { 0 } else { 6 });
                        self.enter_tab();
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                    1 => (weather.sky, weather.code, weather.day, weather.minute, weather.wind) = (Sky::Rain, 63, false, 21 * 60, 32.0),
                    2 => (weather.sky, weather.code, weather.temp) = (Sky::Snow, 73, -3.0),
                    3 => (weather.sky, weather.code, weather.wind) = (Sky::Storm, 95, 38.0),
                    4 => (weather.sky, weather.code, weather.day, weather.minute) = (Sky::Clear, 0, false, 23 * 60),
                    5 => (weather.sky, weather.code) = (Sky::Fog, 45),
                    6 => (weather.sky, weather.code, weather.minute) = (Sky::Clear, 0, 16 * 60 + 5),
                    7 => (weather.aqi, weather.pm25) = (None, None),
                    _ => {}
                }
                self.s.weather = Some(weather);
                self.repaint();
                Some(LRESULT(0))
            }
            snip::WM_APP_SNIP => {
                self.on_snip();
                Some(LRESULT(0))
            }
            // wparam 11: run a screen tool (lparam 0 area, 1 full, 2 text, 3 QR, 4 colour)
            // on the sample picture.
            WM_APP_TEST if self.test_hooks && self.hidden && wparam.0 == 11 => {
                let tools = [snip::Tool::Area, snip::Tool::Full, snip::Tool::Text, snip::Tool::Qr, snip::Tool::Color];
                if let Some(tool) = tools.get(lparam.0 as usize) {
                    self.snip(*tool);
                }
                Some(LRESULT(0))
            }
            // wparam 10: a connected VPN on the tile, and a paired but unconnected
            // headset in the (opened) output list.
            WM_APP_TEST if self.test_hooks && wparam.0 == 10 => {
                self.s.vpn_on = true;
                self.s.vpn_text = "Office VPN".encode_utf16().collect();
                self.s.outputs = audio::outputs();
                self.s.outputs.push(audio::Output { name: "WH-1000XM4".encode_utf16().collect(), id: Vec::new(), current: false, offline: true });
                self.s.device_menu = true;
                self.repaint();
                Some(LRESULT(0))
            }
            // wparam 9: pin a few things that exist on every PC to the (memory-only) dock.
            WM_APP_TEST if self.test_hooks && wparam.0 == 9 => {
                let windows = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_default());
                self.dock().add(vec![windows.join("explorer.exe"), windows.join("notepad.exe"), std::env::temp_dir(), windows.join("win.ini")]);
                self.refresh_dock(true);
                Some(LRESULT(0))
            }
            WM_APP_CLIP => {
                self.on_clip_images();
                Some(LRESULT(0))
            }
            // wparam 6: put sample files on the (memory-only) shelf.
            WM_APP_TEST if self.test_hooks && wparam.0 == 6 => {
                self.shelf().add(["C:\\Work\\Report Q3.pdf", "C:\\Work\\holiday-photo.jpg", "C:\\Work\\Budget 2026.xlsx", "C:\\Work\\Designs", "C:\\Work\\notes.txt", "C:\\Work\\clip.mp4", "C:\\Work\\archive.zip"].into_iter().map(PathBuf::from).collect());
                self.refresh_shelf();
                Some(LRESULT(0))
            }
            // wparam 7: make sure the test notes folder has a note, and give the editor the keyboard.
            WM_APP_TEST if self.test_hooks && wparam.0 == 7 => {
                self.open_notes();
                if let Some(notes) = &mut self.notes {
                    if notes.list.is_empty() {
                        notes.create();
                        notes.insert(&"# Weekend plan\nPack the **tent** and the *good* coffee.\n- charge the lamp\n- print tickets\n\n## Later\nCall the campsite about a late arrival.".encode_utf16().collect::<Vec<_>>());
                        notes.save();
                    }
                }
                self.sync_note(true);
                self.focus_note();
                Some(LRESULT(0))
            }
            // wparam 8: a sample picture goes through the clipboard-image worker (nothing is written to disk).
            WM_APP_TEST if self.test_hooks && wparam.0 == 8 => {
                let (w, h) = (320u32, 200u32);
                let bgra = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 160, 255]).collect();
                clipboard::record_image(self.hwnd, image::to_dib(&Pixels { w, h, bgra }), None);
                Some(LRESULT(0))
            }
            // wparam 5: put sample items into the (memory-only) clipboard history.
            WM_APP_TEST if self.test_hooks && wparam.0 == 5 => {
                for text in ["https://example.com/docs/getting-started", "The quick brown fox\njumps over the lazy dog", "npm run build -- --mode production", "Meeting moved to 15:30", "4f9c2a7e-11d0", "Remember the milk", "second line of notes"] {
                    self.history().push(text);
                }
                self.history().toggle_pin(3);
                self.refresh_clipboard();
                Some(LRESULT(0))
            }
            WM_APP_AGENT => {
                self.on_agent();
                Some(LRESULT(0))
            }
            WM_APP_NOTIFY => {
                self.on_notifications();
                Some(LRESULT(0))
            }
            WM_APP_CAPS => {
                if !self.suspended {
                    unsafe { SetTimer(Some(self.hwnd), T_CAPS, 70, None) };
                }
                Some(LRESULT(0))
            }
            // A second instance forwards `--cmd "<phrase>"` here as UTF-8.
            WM_COPYDATA => {
                let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
                let ok = data.dwData == 1 && !data.lpData.is_null() && {
                    let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
                    let text = String::from_utf8_lossy(bytes).into_owned();
                    self.command(&text)
                };
                Some(LRESULT(ok as isize))
            }
            // HSHELL_LANGUAGE = 8: lparam is the new keyboard layout handle.
            m if m == self.shell_msg && m != 0 => {
                if wparam.0 == 8 && !self.suspended {
                    self.on_layout(lparam.0);
                }
                Some(LRESULT(0))
            }
            WM_APP_TEST if self.test_hooks => {
                self.fake_cursor = (wparam.0 == 1).then_some(((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32));
                if !self.suspended {
                    self.track_cursor();
                    self.invalidate();
                    // Like real pointer movement: the character notices.
                    if self.mascot_visible() {
                        self.update_mascot();
                    }
                }
                Some(LRESULT(0))
            }
            WM_APP_FOREGROUND => {
                self.check_gamemode();
                self.hook_shell_host();
                if !self.suspended {
                    // Stay above other always-on-top windows that were just activated.
                    unsafe {
                        let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                }
                // The new foreground window may still be resizing itself.
                unsafe { SetTimer(Some(self.hwnd), T_RECHECK, 500, None) };
                Some(LRESULT(0))
            }
            WM_TIMER => {
                self.on_timer(wparam.0);
                Some(LRESULT(0))
            }
            WM_APP_MEDIA => {
                self.apply_media();
                Some(LRESULT(0))
            }
            crate::cli::WM_APP_LOGIN => {
                use crate::cli::{login_state, Login, Tool};
                self.s.settings.claude = login_state(Tool::Claude);
                self.s.settings.codex = login_state(Tool::Codex);
                if let Some(tool) = self.s.settings.signing {
                    if login_state(tool) == Some(Login::SignedIn) {
                        self.s.settings.signing = None;
                        self.kill(T_CLAUDE);
                    }
                }
                if self.s.settings.open {
                    self.repaint();
                }
                Some(LRESULT(0))
            }
            crate::chat::WM_APP_MODELS => {
                // Keep the list only if it is for the provider still chosen.
                if let Some((_, list)) = crate::chat::models().filter(|(p, _)| *p == self.chat_provider()) {
                    self.s.settings.models = list;
                    self.sync_chat_header();
                    self.repaint();
                }
                Some(LRESULT(0))
            }
            WM_APP_EQ => {
                // New audio levels: move the bar layers. No frame is drawn.
                self.eq_levels = audio::levels();
                self.music_level();
                if let Some(gfx) = &self.gfx {
                    let (cx, cy, visible) = self.eq_place;
                    if visible > 0.02 && !self.suspended {
                        let _ = gfx.eq_update(cx, cy, self.eq_levels, self.s.accent, visible);
                        gfx.commit();
                    }
                }
                self.mascot_beat();
                self.wave_drop_beat();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.grip.is_some() => {
                // No button: the release was never heard, the drag is over.
                if wparam.0 & 0x0001 == 0 {
                    self.end_grip();
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return Some(LRESULT(0));
                }
                let (x, y) = self.client_dp(lparam);
                self.move_grip(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.grip.is_some() => {
                self.end_grip();
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if self.grip.is_some() => {
                self.end_grip();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.home_grab.is_some() => {
                // No button: the release was never heard, the drag is over.
                if wparam.0 & 0x0001 == 0 {
                    self.end_home_drag();
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return Some(LRESULT(0));
                }
                let (x, y) = self.client_dp(lparam);
                self.move_home_drag(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.home_grab.is_some() => {
                self.end_home_drag();
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if self.home_grab.is_some() => {
                self.end_home_drag();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.tab_grab.is_some() => {
                // The button came up where Islet did not hear it: the drag is over.
                if wparam.0 & 0x0001 == 0 {
                    self.end_tab_drag();
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    return Some(LRESULT(0));
                }
                let (x, y) = self.client_dp(lparam);
                self.move_tab_drag(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.tab_grab.is_some() => {
                self.end_tab_drag();
                unsafe {
                    let _ = ReleaseCapture();
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if self.tab_grab.is_some() => {
                self.end_tab_drag();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.end_island_drag();
                self.end_volume_drag();
                self.end_note_drag();
                self.press = None;
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.island_drag.is_some() => {
                self.move_island_drag();
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.note_drag => {
                let (x, y) = self.client_dp(lparam);
                let (x, y) = self.content_dp(x, y);
                let at = self.gfx.as_ref().and_then(|gfx| gfx.note_hit(&self.s.note.text(), NOTE_TEXT_W, x - NOTE_TEXT_X, y - NOTE_TEXT_Y + self.s.note.scroll));
                if let (Some(at), Some(notes), true) = (at, &mut self.notes, wparam.0 & 1 != 0) {
                    if notes.caret != at as usize {
                        notes.move_to(at as usize, true);
                        self.sync_note(true);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.press.is_some() => {
                let (x, y) = self.client_dp(lparam);
                self.maybe_drag(x, y, wparam.0 & 1 != 0);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.drag_volume => {
                let (x, _) = self.client_dp(lparam);
                self.set_volume(ui::volume_at(&self.s, x));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = self.client_dp(lparam);
                let hit = ui::hit(&self.s, x, y);
                let (hover, dock) = match hit {
                    Some(Hit::Tab(i)) => (Some(i), None),
                    Some(Hit::Dock(i)) => (None, Some(i)),
                    _ => (None, None),
                };
                if hover != self.s.hover_tab || dock != self.s.dock_hover {
                    self.s.hover_tab = hover;
                    self.s.dock_hover = dock;
                    self.repaint();
                }
                // Ctrl held over the open Home tab: the sections show their handles.
                let ctrl = wparam.0 & 0x0008 != 0 && self.s.mode == Mode::Expanded && self.s.tab == 0;
                if ctrl != self.s.ctrl_held {
                    self.s.ctrl_held = ctrl;
                    self.repaint();
                }
                // The permission card's option or button under the pointer.
                let permit = ui::hit_permit(&self.s, x, y);
                if permit != self.s.permit_hover {
                    self.s.permit_hover = permit;
                    self.repaint();
                }
                // The button under the pointer lights up (a short spring; nothing runs at rest).
                let lit = hit.filter(|h| ui::hoverable(*h));
                if lit != self.s.hover {
                    self.s.set_hover(lit);
                    self.invalidate();
                }
                // The music card leans towards the pointer while it is over it.
                if self.s.mode == Mode::Expanded && self.s.tab == 0 {
                    let lean = if self.s.look.lively { ui::card_lean(&self.s, x, y) } else { (0.0, 0.0) };
                    if (lean.0 - self.s.lean[0].target).abs() + (lean.1 - self.s.lean[1].target).abs() > 0.02 {
                        self.s.lean[0].set(lean.0);
                        self.s.lean[1].set(lean.1);
                        self.invalidate();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                // Ctrl + drag on a tab of the open panel moves that tab along the bar.
                if wparam.0 & 0x0008 != 0 && self.s.mode == Mode::Expanded {
                    let (x, y) = self.client_dp(lparam);
                    // Home: a section's handle moves the whole section.
                    if self.s.tab == 0 {
                        let (px, py) = self.s.to_panel(x, y);
                        if let Some(sec) = ui::section_handle_at(&self.s, px, py) {
                            self.start_grip(Grip { group: Group::HomeSection, id: sec }, px, py);
                            return Some(LRESULT(0));
                        }
                    }
                    // Home: a quick tile or a dock icon moves the same way.
                    let home = match ui::hit(&self.s, x, y) {
                        Some(Hit::Quick(id)) if self.s.tab == 0 => Some(HomeItem::Tile(id)),
                        Some(Hit::Dock(i)) if self.s.tab == 0 => Some(HomeItem::Dock(i)),
                        _ => None,
                    };
                    if let Some(item) = home {
                        let (px, py) = self.s.to_panel(x, y);
                        self.start_home_drag(item, px, py);
                        return Some(LRESULT(0));
                    }
                    // Cards, tiles, buttons and sections of the open tab.
                    let (px, py) = self.s.to_panel(x, y);
                    if let Some(grip) = ui::grip_at(&self.s, px, py) {
                        self.start_grip(grip, px, py);
                        return Some(LRESULT(0));
                    }
                    if let Some(Hit::Tab(i)) = ui::hit(&self.s, x, y) {
                        let (px, _) = self.s.to_panel(x, y);
                        let left = ui::tab_left(&self.s, i);
                        self.tab_grab = Some((i, px - left));
                        self.s.hold_tab(i, left);
                        self.s.set_hover(None);
                        unsafe {
                            SetCapture(self.hwnd);
                            let _ = SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
                        }
                        self.repaint();
                        return Some(LRESULT(0));
                    }
                }
                // Ctrl + drag moves the island sideways (MK_CONTROL); nothing is clicked.
                if wparam.0 & 0x0008 != 0 && self.s.mode != Mode::Hidden {
                    self.start_island_drag();
                    return Some(LRESULT(0));
                }
                let (x, y) = self.client_dp(lparam);
                self.click(x, y);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if self.island_drag.is_some() => {
                self.end_island_drag();
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    tray::CMD_PAUSE => {
                        self.paused = !self.paused;
                        tray::PAUSED.store(self.paused, Relaxed);
                        if self.demo.is_some() || self.hidden {
                            // The demo and hidden mode ignore game-suspend, so pause directly.
                            if self.paused {
                                self.suspend();
                            } else {
                                self.resume();
                            }
                        } else {
                            self.check_gamemode();
                        }
                    }
                    tray::CMD_CONFIG => open_file(&self.config_path),
                    tray::CMD_SETTINGS => self.open_settings(true),
                    tray::CMD_UPDATE => {
                        // Only a web page is opened, never a file the check pointed at.
                        let page = tray::UPDATE.lock().ok().and_then(|u| u.as_ref().map(|(_, page)| page.clone()));
                        if let Some(page) = page.filter(|p| p.starts_with("https://")) {
                            unsafe {
                                ShellExecuteW(None, w!("open"), &HSTRING::from(page.as_str()), None, None, SW_SHOWNORMAL);
                            }
                        }
                    }
                    tray::CMD_CLICK_THROUGH => {
                        let on = !self.cfg.click_through;
                        self.change_setting("click_through", if on { "on" } else { "off" });
                    }
                    tray::CMD_HOOKS_INSTALL => self.hooks(true),
                    tray::CMD_HOOKS_REMOVE => self.hooks(false),
                    tray::CMD_QUIT => unsafe {
                        let _ = DestroyWindow(self.hwnd);
                    },
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                if !self.suspended {
                    self.apply_theme(true);
                    self.repaint();
                }
                None
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                if !self.suspended {
                    self.replace();
                }
                Some(LRESULT(0))
            }
            WM_TIMECHANGE => {
                self.fire_due();
                self.arm_clock();
                self.repaint();
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            T_CLOCK => {
                self.arm_clock();
                self.repaint();
            }
            T_HOVER => {
                self.kill(T_HOVER);
                self.hover_armed = false;
                let (x, y) = self.cursor_dp();
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 || self.island_drag.is_some();
                if self.s.mode == Mode::Idle && self.s.pill != Pill::Permit && self.s.shape().contains(x, y, 0.0) && !self.on_pet(x, y) && !ctrl {
                    self.set_mode(Mode::Expanded);
                }
            }
            T_COLLAPSE => {
                self.kill(T_COLLAPSE);
                self.collapse_armed = false;
                let (x, y) = self.cursor_dp();
                if self.s.mode == Mode::Expanded && !self.s.pinned && self.tab_grab.is_none() && self.home_grab.is_none() && self.grip.is_none() && !self.s.shape().contains(x, y, 16.0) {
                    self.set_mode(Mode::Idle);
                }
            }
            T_AUTOHIDE => {
                self.kill(T_AUTOHIDE);
                if self.s.mode == Mode::Idle {
                    let (x, y) = self.cursor_dp();
                    if self.s.shape().contains(x, y, 40.0) {
                        self.arm_autohide();
                    } else {
                        self.set_mode(Mode::Hidden);
                    }
                }
            }
            T_WATCH => self.check_gamemode(),
            T_RECHECK => {
                self.kill(T_RECHECK);
                self.check_gamemode();
            }
            T_DEBUG => {
                if let Some(stats) = &mut self.stats {
                    stats.sample();
                }
                self.invalidate();
            }
            T_TRIM => {
                self.kill(T_TRIM);
                if self.s.at_rest() && self.s.mode != Mode::Expanded {
                    // What the tabs loaded is not needed while the panel is closed.
                    self.save_note();
                    self.notes = None;
                    self.s.note = NoteView::default();
                    self.thumbs = Vec::new();
                    // The conversation (pictures included) is read again when the tab opens.
                    if !self.s.chat.busy && self.chat_pending.is_empty() {
                        self.chat = None;
                        self.s.chat.rows = Vec::new();
                    }
                    self.chat_thumbs = Vec::new();
                    if let Some(gfx) = &self.gfx {
                        gfx.clear_images();
                        gfx.trim();
                    }
                    give_memory_back();
                }
            }
            T_TICK => {
                if self.s.mode == Mode::Expanded && self.s.tab == 0 {
                    let mut changed = self.s.media.session && self.s.media.playing;
                    if !self.drag_volume {
                        if let Some(level) = audio::volume() {
                            if (level - self.s.volume).abs() > 0.004 {
                                self.s.volume = level;
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        self.repaint();
                    }
                }
            }
            T_TOAST => self.dismiss_toast(),
            T_SECOND => {
                if let Some(since) = self.recording {
                    let secs = since.elapsed().as_secs() as i64;
                    if secs != self.s.record_secs {
                        self.s.record_secs = secs;
                        if self.s.pill == Pill::Record {
                            self.repaint();
                        }
                    }
                }
                if let Some((left, _)) = self.store.next_timer(timer::now()) {
                    if left != self.s.timer_left {
                        self.s.timer_left = left;
                        if self.s.pill == Pill::Timer {
                            self.repaint();
                        }
                    }
                }
            }
            T_MASCOT => {
                self.kill(T_MASCOT);
                self.update_mascot();
            }
            T_VOLUME => {
                self.kill(T_VOLUME);
                self.s.volume_shown = false;
                self.repaint();
            }
            T_CLAUDE => {
                self.claude_polls += 1;
                if self.s.settings.signing.is_none() || self.claude_polls > 45 {
                    self.kill(T_CLAUDE);
                    self.s.settings.signing = None;
                    self.repaint();
                } else {
                    crate::cli::check_login(self.hwnd);
                }
            }
            T_UPDATE => {
                self.check_update(false);
                unsafe { SetTimer(Some(self.hwnd), T_UPDATE, 6 * 3600 * 1000, None) };
            }
            T_TYPE => {
                if let Some((text, tries)) = self.voice_typing.take() {
                    // Up to two seconds for the shortcut's keys to be let go.
                    if crate::voice::modifiers_down() && tries < 50 {
                        self.voice_typing = Some((text, tries + 1));
                    } else {
                        self.kill(T_TYPE);
                        crate::voice::type_text(&text);
                    }
                } else {
                    self.kill(T_TYPE);
                }
            }
            T_SCHEDULE => self.fire_due(),
            T_LYRIC => {
                self.kill(T_LYRIC);
                self.sync_lyric();
            }
            T_WEATHER => {
                // Asleep for a game: skip this round, the next one is half an hour away.
                if !self.suspended {
                    crate::weather::refresh(self.hwnd, self.cfg.city.clone());
                }
                unsafe { SetTimer(Some(self.hwnd), T_WEATHER, 30 * 60 * 1000, None) };
            }
            T_NOTE => {
                self.save_note();
                // The first line may have changed: refresh the titles.
                self.sync_note(false);
            }
            T_PASTE => {
                self.kill(T_PASTE);
                clipboard::send_paste();
            }
            T_INDEX => {
                self.kill(T_INDEX);
                if self.s.mode != Mode::Command {
                    self.index = None;
                    self.cmd_rows = Vec::new();
                    self.s.cmd_rows = Vec::new();
                }
            }
            T_AGENT => {
                self.refresh_agents();
                self.repaint();
            }
            T_NOTIFY => {
                self.kill(T_NOTIFY);
                notify::read_async(self.hwnd);
            }
            T_CAPS => {
                self.kill(T_CAPS);
                self.on_caps();
            }
            T_OUTPUTS => {
                self.kill(T_OUTPUTS);
                self.s.outputs = audio::outputs();
                if self.s.mode == Mode::Expanded && self.s.tab == 0 {
                    self.refresh_dock(true);
                }
                self.repaint();
            }
            T_RETRY => {
                self.kill(T_RETRY);
                self.invalidate();
            }
            _ => {}
        }
    }

    fn client_dp(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x / self.scale, y / self.scale)
    }

    /// Window dp -> the units a fixed-layout tab draws in (panel scale and centring undone).
    fn content_dp(&self, x: f32, y: f32) -> (f32, f32) {
        let (x, y) = self.s.to_panel(x, y);
        (x, y - self.s.fixed_dy())
    }
}

/// The sound a press on `hit` makes. Text fields and the slider are silent;
/// settings rows play theirs in `settings_click`.
fn click_cue(hit: Hit) -> Option<crate::sound::Cue> {
    use crate::sound::Cue;
    Some(match hit {
        Hit::Tab(_) | Hit::WxView(_) | Hit::SettingsGroup(_) => Cue::Tab,
        Hit::Radio(_) | Hit::Theme | Hit::Vpn | Hit::Pin | Hit::WxDay(_) => Cue::Toggle,
        Hit::WxSky => Cue::Drop,
        Hit::Volume(_) | Hit::ChatInput | Hit::ClipSearch | Hit::NoteText | Hit::SettingsSearch | Hit::Setting(..) => return None,
        _ => Cue::Tap,
    })
}

/// Opens Explorer with the file selected (or the folder itself, for a directory).
fn reveal(path: &std::path::Path) {
    let args = if path.is_dir() { format!("\"{}\"", path.display()) } else { format!("/select,\"{}\"", path.display()) };
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), &HSTRING::from(args), None, SW_SHOWNORMAL);
    }
}

/// Returns freed heap pages to Windows and empties the working set: private
/// bytes drop as well as the working set (used after a long rest and on suspend).
fn give_memory_back() {
    use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};
    unsafe {
        if let Ok(heap) = GetProcessHeap() {
            HeapCompact(heap, HEAP_FLAGS(0));
        }
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

fn open_file(path: &std::path::Path) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}
