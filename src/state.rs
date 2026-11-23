//! Everything the island animates. `State` is pure data plus springs: the app
//! (or the demo script) sets targets, `tick` advances them, `ui` draws them.

use crate::anim::{clamp01, droplet_bridge, Spring};
use crate::comp::WAVE_RAISE;
use crate::island::{wave_mid, wave_neutral, wave_speed, Liquid, Shape, WAVE_AMP};

/// How the music wave looks (settings page, "Music wave").
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WaveStyle {
    /// Plain sine instead of the liquid pattern.
    pub simple: bool,
    /// How strongly the middle follows the music: 0 off .. 3 strong.
    pub react: u8,
    /// 0 slow, 1 normal, 2 fast.
    pub speed: u8,
    /// Blend the line from the cover's second colour (else the accent only).
    pub two_colours: bool,
    /// Soft halo around the line.
    pub glow: bool,
    /// Drops falling from the wave on bass beats: 0 off, 1 subtle, 2 lively.
    pub drops: u8,
}

impl Default for WaveStyle {
    fn default() -> Self {
        Self { simple: false, react: 2, speed: 1, two_colours: true, glow: true, drops: 1 }
    }
}

/// A drop falling from the music wave on a beat: where along the edge, and
/// seconds since it began to swell (see `BeatDrop::shape`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatDrop {
    pub x: f32,
    pub t: f32,
    /// The wave's edge at `x` (without the drop): followed while the drop
    /// hangs from it, kept from the moment it falls.
    pub base: f32,
}

/// Radius of a beat drop, dp.
pub const BEAT_DROP_R: f32 = 4.6;
/// Its phases: swelling until `SWELL`, hanging lower until `BREAK` (the neck
/// snaps), falling and fading until `END`.
const DROP_SWELL: f32 = 0.32;
const DROP_BREAK: f32 = 0.6;
const DROP_END: f32 = 1.2;

impl BeatDrop {
    /// (Bulge amplitude and width on the edge, distance of the drop's centre
    /// below the edge, drop opacity, vertical stretch.)
    pub fn shape(&self) -> (f32, f32, Option<f32>, f32, f32) {
        let r = BEAT_DROP_R;
        let t = self.t;
        if t < DROP_SWELL {
            // A bulb grows out of the wave.
            let u = crate::anim::ease_out_cubic(t / DROP_SWELL);
            let amp = u * (4.0 + r * 0.9);
            // Once the bulb is deep enough the drop inside it gives it a round tip.
            let dist = amp - r * 0.9;
            return (amp, lerp_f(9.0, r * 0.95, u), (dist > 0.3).then_some(dist), 1.0, 1.0);
        }
        if t < DROP_BREAK {
            // It hangs lower, the neck thinning (the metaball bridge).
            let u = (t - DROP_SWELL) / (DROP_BREAK - DROP_SWELL);
            let dist = lerp_f(4.0, 2.8 * r, u * u * (3.0 - 2.0 * u));
            let (amp, sigma) = crate::anim::droplet_bridge(dist, r);
            return (amp, sigma, Some(dist), 1.0, 1.0 + 0.15 * u);
        }
        // Falls, a little faster each moment, stretching and fading.
        let f = t - DROP_BREAK;
        let dist = 2.8 * r + 30.0 * f + 0.5 * 320.0 * f * f;
        let fade = 1.0 - clamp01((t - (DROP_END - 0.3)) / 0.3);
        (0.0, r, Some(dist), fade, 1.0 + 0.25 * clamp01(f / 0.3))
    }

    pub fn done(&self) -> bool {
        self.t >= DROP_END
    }

    /// Whether the neck snapped between `before` and now.
    pub fn snapped(&self, before: f32) -> bool {
        before < DROP_BREAK && self.t >= DROP_BREAK
    }
}

fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
use crate::render::Rgba;
use crate::theme;

/// Window size in dp (large enough for the biggest panel plus liquid overshoot).
/// (Room for the widest and tallest panel setting, 1.15 x 650 by 1.15 x 330.)
pub const WIN_W: f32 = 800.0;
pub const WIN_H: f32 = 460.0;
pub const CX: f32 = WIN_W * 0.5;

pub const PILL_W: f32 = 344.0;
pub const PILL_H: f32 = 68.0;
pub const PANEL_W: f32 = 650.0;
pub const PANEL_H: f32 = 330.0;

pub const TAB_COUNT: usize = 8;

/// A click ripple: the button's outline, where it was pressed, and how far
/// along it is (0..1 over `RIPPLE_SECS`).
#[derive(Clone, Copy, Debug)]
pub struct Ripple {
    pub shape: crate::ui::Glow,
    pub x: f32,
    pub y: f32,
    pub t: f32,
}

pub const RIPPLE_SECS: f32 = 0.5;

/// Within this many dp of a screen edge the island reaches out to merge with it.
pub const MERGE_REACH: f32 = 48.0;
/// How far a merged island's liquid flows down the screen edge, dp.
pub const MERGE_FLARE: f32 = 16.0;

/// Tabs the user hid in the settings (bit per tab number).
static HIDDEN_TABS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_hidden_tabs(mask: u8) {
    // Home is always there.
    HIDDEN_TABS.store(mask & !1, std::sync::atomic::Ordering::Relaxed);
}

/// Tab 3 was a never-built "Text" tab; since 2026-11-20 it is the System tab.
/// Tabs keep their numbers; any but Home can be hidden in the settings.
pub fn tab_shown(i: usize) -> bool {
    HIDDEN_TABS.load(std::sync::atomic::Ordering::Relaxed) & (1 << i) == 0
}

/// The look settings the drawing needs (see `config::Look`).
#[derive(Clone, Debug)]
pub struct LookState {
    /// Corner radius of the pill and of the panel.
    pub corners: (f32, f32),
    pub pill_size: f32,
    pub splash: bool,
    pub alert_glow: bool,
    pub lively: bool,
    /// Quick actions in grid order (indexes into `ui::QUICK`).
    pub quick: Vec<usize>,
    /// Height of the line pills relative to normal.
    pub pill_height: f32,
    /// Width and height of the open panel relative to normal.
    pub panel: (f32, f32),
    /// Home tab parts shown: music card, volume row, dock.
    pub home: (bool, bool, bool),
    /// Controls tab: the tiles (0 Wi-Fi, 1 Bluetooth, 2 VPN, 3 dark theme) and
    /// power buttons (0 lock, 1 sleep, 2 restart, 3 shut down) shown, in order.
    pub tiles: Vec<usize>,
    pub power: Vec<usize>,
}

impl Default for LookState {
    fn default() -> Self {
        Self { corners: (26.0, 28.0), pill_size: 1.0, splash: true, alert_glow: true, lively: true, quick: (0..12).collect(), pill_height: 1.0, panel: (1.0, 1.0), home: (true, true, true), tiles: (0..4).collect(), power: (0..4).collect() }
    }
}

/// How long a notification's splash and a tab icon's bounce last, seconds.
pub const SPLASH_SECS: f32 = 0.75;
pub const BOUNCE_SECS: f32 = 0.5;
/// How long an important alert makes the outline pulse.
pub const ALERT_SECS: f32 = 4.0;
/// Width of an icon-only tab.
pub const TAB_W: f32 = 36.0;
/// Stiffness and damping of the tabs' position springs: a little softer than the
/// usual UI spring and nearly critically damped, so reordering glides without a wobble.
pub const TAB_GLIDE: (f32, f32) = (220.0, 28.0);

/// How long a power button waits for its second press.
pub const CONFIRM_SECS: f32 = 3.0;
/// Size of the permission card.
pub const PERMIT_W: f32 = 520.0;
pub const PERMIT_H: f32 = 156.0;
/// Height of the "Downloaded" completion card.
const DONE_H: f32 = 124.0;
const DROP_R: f32 = 6.5;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// The pill.
    Idle,
    /// The hover panel.
    Expanded,
    /// Auto-hidden: a thin strip at the screen edge.
    Hidden,
    /// The command bar (Alt+Space): a text field with result rows.
    Command,
}

/// One row of a list (command results, clipboard items).
#[derive(Default)]
pub struct ListRow {
    pub title: Vec<u16>,
    pub detail: Vec<u16>,
    pub marked: bool,
    /// Short badge text (a file's extension on the shelf).
    pub tag: Vec<u16>,
    /// Key of the row's picture (see `Gfx::image`), 0 for none.
    pub image: u64,
}

/// Shelf rows visible at once.
pub const SHELF_ROWS: usize = 5;
/// Note titles visible at once.
pub const NOTE_ROWS: usize = 6;
/// The note editor's text box.
pub const NOTE_TEXT_X: f32 = CX - PANEL_W * 0.5 + 16.0 + 188.0 + 14.0;
pub const NOTE_TEXT_Y: f32 = 68.0;
pub const NOTE_TEXT_W: f32 = 402.0;
pub const NOTE_TEXT_H: f32 = 228.0;

/// What the Notes tab shows.
#[derive(Default)]
pub struct NoteView {
    pub folder: Vec<u16>,
    /// Visible titles; `marked` is the note in the editor.
    pub rows: Vec<ListRow>,
    pub total: usize,
    /// A note is in the editor.
    pub open: bool,
    pub text: Vec<u16>,
    pub spans: Vec<crate::render::Span>,
    pub serial: u64,
    pub caret: u32,
    pub selection: Option<(u32, u32)>,
    /// How far the text is scrolled, in dp.
    pub scroll: f32,
    /// The editor has the keyboard.
    pub focus: bool,
}

impl NoteView {
    pub fn text(&self) -> crate::render::NoteText<'_> {
        crate::render::NoteText { serial: self.serial, text: &self.text, spans: &self.spans }
    }
}

pub const COMMAND_W: f32 = 560.0;
pub const COMMAND_INPUT_H: f32 = 62.0;
pub const COMMAND_ROW_H: f32 = 42.0;
/// Clipboard rows visible at once.
pub const CLIP_ROWS: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pill {
    Clock,
    Music,
    Download,
    Done,
    /// A running countdown.
    Timer,
    /// A short-lived alert (see `events::Toast`).
    Toast,
    Mascot,
    /// A coding agent asks for permission: a card with Allow / Deny.
    Permit,
    /// The screen is being recorded: elapsed time and a stop button.
    Record,
    /// Voice to text is listening: a microphone and the words so far.
    Listen,
}

/// Texts of the permission card.
#[derive(Default)]
pub struct PermitView {
    /// "Claude Code wants to use Bash".
    pub title: Vec<u16>,
    /// The command or file (or the question, or the plan), split over up to two lines.
    pub lines: [Vec<u16>; 2],
    /// The buttons along the bottom, left to right; the last one is the main one.
    pub buttons: Vec<(Vec<u16>, PermitAction)>,
    /// A question's options (label, description) and which are ticked.
    pub options: Vec<(Vec<u16>, Vec<u16>)>,
    pub picked: Vec<bool>,
    /// Height of the card (taller with options).
    pub h: f32,
}

/// What a click on the permission card does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PermitAction {
    Deny,
    Allow,
    /// Allow, and add the rule the agent suggested so it stops asking.
    Always,
    /// No decision here: the agent asks in its own window.
    Terminal,
    /// Sends the ticked options of a multiple-choice question.
    Send,
    /// Option `i` of the question.
    Choose(usize),
}

/// Things that can be put in another order with Ctrl + drag (besides tabs,
/// quick tiles and dock icons, which have their own code).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    /// System tab cards: CPU, memory, GPU, network.
    Sys,
    /// Controls tab: the four tiles, the four power buttons.
    CtlTile,
    CtlPower,
    /// Weather tab: sky card / forecast card, the six Details tiles, the view switcher.
    WxCard,
    WxDetail,
    WxView,
    /// Home tab sections: music card, volume, quick tiles, dock (they swap places).
    HomeSection,
}

impl Group {
    pub const ALL: [Group; 7] = [Group::Sys, Group::CtlTile, Group::CtlPower, Group::WxCard, Group::WxDetail, Group::WxView, Group::HomeSection];

    pub fn index(self) -> usize {
        self as usize
    }

    /// How many items the group has.
    pub fn len(self) -> usize {
        match self {
            Group::Sys | Group::CtlTile | Group::CtlPower | Group::HomeSection => 4,
            Group::WxCard => 2,
            Group::WxDetail => 6,
            Group::WxView => 3,
        }
    }

    /// Home sections trade places (they differ in size); the rest shift over.
    pub fn swaps(self) -> bool {
        matches!(self, Group::HomeSection | Group::WxCard)
    }
}

/// One item of a [`Group`] (by its id, which stays the same when it moves).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grip {
    pub group: Group,
    pub id: usize,
}

/// Per group and item id: how far it is drawn from its slot (x, y, width,
/// height), springing back to 0 so moved items glide (and resize) into place.
pub type GripOffsets = [[[Spring; 4]; 8]; 7];

/// Something on the Home tab that can be moved with Ctrl + drag.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HomeItem {
    /// A quick-action tile, by action id.
    Tile(usize),
    /// A dock icon, by its slot.
    Dock(usize),
}

/// Card height for `options` option rows (156 dp without any).
pub fn permit_height(options: usize) -> f32 {
    if options == 0 { PERMIT_H } else { PERMIT_OPTIONS_Y + options as f32 * PERMIT_ROW + 56.0 }
}
/// Where the option rows of a question start, and their pitch.
pub const PERMIT_OPTIONS_Y: f32 = 104.0;
pub const PERMIT_ROW: f32 = 38.0;

/// Texts of the toast currently in the pill.
pub struct ToastView {
    pub icon: crate::events::Icon,
    pub title: Vec<u16>,
    pub text: Vec<u16>,
    pub accent: Rgba,
}

/// A completed download shown as the "Downloaded" card.
#[derive(Default)]
pub struct DoneView {
    pub name: Vec<u16>,
    /// Upper-case file extension for the tile (at most 4 characters).
    pub ext: Vec<u16>,
    pub size: u64,
}

/// What the now-playing UI shows (filled from SMTC, or by the demo).
pub struct MediaView {
    pub session: bool,
    pub playing: bool,
    pub title: Vec<u16>,
    pub artist: Vec<u16>,
    pub source: Vec<u16>,
    /// Position in seconds as of `stamp`, and track length (0 if unknown).
    pub position: f32,
    pub duration: f32,
    pub stamp: std::time::Instant,
    /// Seek, shuffle and repeat, as far as the player allows them.
    pub can: crate::media::Abilities,
}

/// The playing song's lyrics as shown: in the music pill (the line being
/// sung) and in the music card's lyrics view.
pub struct LyricView {
    /// `lyrics = on`: the card's cover opens the lyrics view.
    pub enabled: bool,
    /// Show the line being sung in the pill (`lyrics_pill`).
    pub in_pill: bool,
    /// All lines (empty: none to show), and whether they have times.
    pub lines: Vec<Vec<u16>>,
    pub synced: bool,
    /// What the card says instead when there are no lines ("Looking up…").
    pub status: Vec<u16>,
    /// The line being sung, the text going out, and the cross-fade (0..1).
    pub index: Option<usize>,
    pub text: Vec<u16>,
    pub prev: Vec<u16>,
    pub fade: Spring,
    /// The music card shows the lyrics instead of its cover and titles.
    pub card: bool,
    /// The card's list, scrolled to this (eased) line.
    pub scroll: Spring,
}

impl LyricView {
    fn new() -> Self {
        Self { enabled: false, in_pill: true, lines: Vec::new(), synced: false, status: Vec::new(), index: None, text: Vec::new(), prev: Vec::new(), fade: Spring::unit(1.0), card: false, scroll: Spring::new(0.0, 170.0, 24.0) }
    }

    /// The pill shows a lyric line (rather than the artist).
    pub fn singing(&self) -> bool {
        self.enabled && self.in_pill && self.synced && (self.index.is_some() || self.fade.x < 1.0)
    }

    /// The open panel's music card shows the line being sung (lyrics on and
    /// synced, whether or not the pill shows them).
    pub fn singing_in_card(&self) -> bool {
        self.enabled && self.synced && (self.index.is_some() || self.fade.x < 1.0)
    }

    /// Moves to line `index` (`None`: before the first one / no lyrics).
    pub fn set_line(&mut self, index: Option<usize>) {
        if index == self.index {
            return;
        }
        self.prev = std::mem::take(&mut self.text);
        self.text = index.and_then(|i| self.lines.get(i)).cloned().unwrap_or_default();
        self.index = index;
        self.fade.snap(0.0);
        self.fade.set(1.0);
        if let Some(i) = index {
            self.scroll.set(i as f32);
        }
    }

    /// Forgets the lyrics (a new song, or the feature switched off).
    pub fn clear(&mut self, status: &str) {
        self.lines.clear();
        self.synced = false;
        self.index = None;
        self.text.clear();
        self.prev.clear();
        self.fade.snap(1.0);
        self.scroll.snap(0.0);
        self.status = status.encode_utf16().collect();
    }
}

impl MediaView {
    /// Current playback position in seconds.
    pub fn elapsed(&self) -> f32 {
        let now = if self.playing { self.position + self.stamp.elapsed().as_secs_f32() } else { self.position };
        if self.duration > 0.0 {
            now.min(self.duration)
        } else {
            now
        }
    }
}

/// Width the assistant's text wraps at, and the widest a user bubble's text gets.
pub const CHAT_TEXT_W: f32 = 540.0;
pub const CHAT_BUBBLE_W: f32 = 360.0;

/// An attachment as drawn: a picture thumbnail, or a file card with its name and detail.
pub struct Chip {
    /// Thumbnail key in `Gfx`'s picture cache.
    pub image: Option<u64>,
    pub name: Vec<u16>,
    pub detail: Vec<u16>,
    /// Still being read.
    pub busy: bool,
}

/// One chat message laid out for drawing.
pub struct ChatRow {
    pub user: bool,
    pub error: bool,
    pub text: Vec<u16>,
    /// Bold runs (start, length) in `text`.
    pub bold: Vec<(u32, u32)>,
    pub w: f32,
    pub h: f32,
    /// Attachments shown above a user message.
    pub chips: Vec<Chip>,
    /// A finished answer: a copy button under it, and "regenerate" on the last one.
    pub actions: bool,
    pub regenerate: bool,
}

#[derive(Default)]
pub struct ChatView {
    /// `chat = on` in the settings file.
    pub enabled: bool,
    /// No API key yet: the input field takes it.
    pub need_key: bool,
    /// The header: "Claude · Sonnet" (click: next model), and "Open in Claude
    /// Code" when the conversation is kept there (empty: no button).
    pub model: Vec<u16>,
    pub open: Vec<u16>,
    pub rows: Vec<ChatRow>,
    /// How far the list is scrolled up from its end, in dp.
    pub scroll: f32,
    /// What the input field shows (the end of a long text; dots for a key).
    pub shown: Vec<u16>,
    pub focus: bool,
    /// An answer is streaming in.
    pub busy: bool,
    /// Attachments waiting to go with the next message.
    pub pending: Vec<Chip>,
}

pub struct State {
    pub chat: ChatView,
    pub media: MediaView,
    /// Master volume 0..1, output devices, and whether the device list is open.
    pub volume: f32,
    pub outputs: Vec<crate::audio::Output>,
    pub device_menu: bool,
    /// The download in progress, the last completed one, the running timer
    /// (seconds left and label) and the current toast.
    pub download: crate::watch::Download,
    pub done: DoneView,
    pub timer_left: i64,
    pub timer_label: Vec<u16>,
    pub toast: ToastView,
    pub permit: PermitView,
    /// The permission card's option or button under the pointer.
    pub permit_hover: Option<PermitAction>,
    /// Control centre: radio states, dark theme, and the power button waiting
    /// for its second press (with the seconds it still waits).
    pub radios: crate::control::Radios,
    pub dark: bool,
    /// Screen recording: running (the quick action becomes "Stop"), seconds so far.
    pub recording: bool,
    pub record_secs: i64,
    /// Voice to text: what the listening pill says ("Listening…" or the words so far).
    pub voice_text: Vec<u16>,
    /// The button under the pointer and the one it just left, with their
    /// light-up (0..1) and fade-out springs.
    pub hover: Option<crate::ui::Hit>,
    pub hover_old: Option<crate::ui::Hit>,
    pub hover_in: Spring,
    pub hover_out: Spring,
    /// The settings page (shown instead of the tab's content while open).
    pub settings: crate::settings::View,
    /// The character on the pill (index into `mascot::CAST`) and how it looks right now.
    pub mascot: Option<usize>,
    pub pose: crate::mascot::Pose,
    /// Weather tab and pill: the forecast (when the feature is on and fetched),
    /// why there is none, and the clock of the sky animation in seconds.
    pub weather: Option<crate::weather::Weather>,
    /// System load (System tab, and the pill with `stats_pill`).
    pub stats: Option<crate::sysstat::Stats>,
    pub stats_pill: bool,
    /// The pill shows the volume for a moment (a wheel turn over it).
    pub volume_shown: bool,
    pub weather_note: Vec<u16>,
    pub sky_phase: f32,
    /// The Weather tab's view and interaction (see `panels::weather_step`).
    pub wx: crate::panels::Wx,
    /// The VPN tile: lit or not, and its status line.
    pub vpn_on: bool,
    pub vpn_text: Vec<u16>,
    pub confirm: Option<crate::control::Power>,
    pub confirm_left: f32,
    /// Command bar: presence (0..1), typed text, result rows, selected row.
    pub cmd: Spring,
    pub cmd_text: Vec<u16>,
    pub cmd_rows: Vec<ListRow>,
    pub cmd_sel: usize,
    /// Clipboard tab: visible rows, total matches, search text and whether the
    /// search field has the keyboard.
    pub clip_rows: Vec<ListRow>,
    pub clip_total: usize,
    /// Index of the first clipboard row shown (for the scroll indicator).
    pub clip_first: usize,
    pub clip_query: Vec<u16>,
    pub clip_focus: bool,
    /// Shelf tab: visible rows and how many files the shelf holds.
    pub shelf_rows: Vec<ListRow>,
    pub shelf_total: usize,
    /// Index of the first shelf row shown (for the scroll indicator).
    pub shelf_first: usize,
    pub note: NoteView,
    /// Home tab dock: pinned items (`image` = icon key), the one under the
    /// pointer, "files are being dragged over it", and "show the how-to hint".
    pub dock: Vec<ListRow>,
    pub dock_hover: Option<usize>,
    pub dock_drop: bool,
    pub dock_hint: bool,
    /// A coding agent (Claude Code, Codex, ...) is working right now.
    pub agent_working: bool,
    /// Microphone / camera currently in use by some app.
    pub mic: bool,
    pub cam: bool,
    pub mode: Mode,
    pub pill: Pill,
    pub w: Spring,
    pub h: Spring,
    pub r: Spring,
    /// Cursor-reach teardrop amplitude and position.
    pub bump: Spring,
    pub bump_x: Spring,
    /// Music wave presence (0..1), cursor focus (0..1) and focus position.
    pub wave: Spring,
    pub focus: Spring,
    pub focus_x: Spring,
    pub wave_phase: f32,
    /// Smoothed music level (0..1) that swells the middle of the wave;
    /// `WAVE_NEUTRAL` while no audio levels arrive.
    pub wave_level: f32,
    pub wave_style: WaveStyle,
    /// Test aid: draw every character in a grid over everything (see `ui`).
    pub gallery: bool,
    /// This frame only the character moved: draw its corner and nothing else.
    pub pet_only: bool,
    /// Which way new tab content slides in: 1 from the right, -1 from the left.
    pub tab_dir: f32,
    /// Tab numbers in the order the bar shows them (`tab_order` in the config).
    pub tab_order: [usize; TAB_COUNT],
    /// A tab being moved with Ctrl + drag: which, and the left edge it is drawn at.
    pub tab_drag: Option<(usize, f32)>,
    /// The soft ripple spreading from a click across the button pressed.
    pub ripple: Option<Ripple>,
    /// The monitor's left and right edges in window dp (the island can be moved
    /// right up to them with Ctrl + drag).
    pub edge: (f32, f32),
    /// How far the island has merged with the left / right screen edge: a thin
    /// liquid web while it comes close, 1 when it touches (springy).
    pub merge: [Spring; 2],
    /// The steady wave is currently drawn by compositor layers (see `comp`).
    pub comp_wave: bool,
    pub noise_phase: f32,
    /// Accent line along the bottom edge (music, downloads).
    pub glow: Spring,
    pub accent: Rgba,
    pub accent_target: Rgba,
    /// Second cover colour: the wave line blends from it (upper side of the
    /// wave) into `accent` (lower side). Equals the accent outside music.
    pub accent2: Rgba,
    pub accent2_target: Rgba,
    /// Cross-fade when the pill switches content.
    pub pill_fade: Spring,
    /// Seconds since the panel content started to reveal (0 ..= REVEAL_MAX).
    pub reveal: f32,
    pub tab: usize,
    pub tab_w: [Spring; TAB_COUNT],
    /// Each tab's left edge in the bar (from `tabs_x`): neighbours glide aside
    /// when tabs swap, and a released tab glides into its slot.
    pub tab_x: [Spring; TAB_COUNT],
    /// How lifted the held tab is (0..1), and which tab is drawn on top while it
    /// is held or still settling after being let go.
    pub tab_lift: Spring,
    pub tab_held: Option<usize>,
    /// Home tab Ctrl + drag: quick tiles (by action id) and dock icons (by slot)
    /// are drawn this far from their slot; it springs back to 0, so items that
    /// change places glide instead of jumping.
    pub quick_off: [(Spring, Spring); 12],
    pub dock_off: [Spring; 10],
    /// The item held on the Home tab and where its top-left corner is drawn
    /// (panel content units) while held; `home_held` stays while it settles.
    pub home_drag: Option<(HomeItem, f32, f32)>,
    pub home_held: Option<HomeItem>,
    pub home_lift: Spring,
    /// Item ids per [`Group`] in the order they are shown (config `*_order`).
    pub order: [Vec<usize>; 7],
    pub grip_off: GripOffsets,
    /// The [`Grip`] held with Ctrl + drag and its drawn rectangle (x, y) while
    /// held; `grip_held` stays while it settles back.
    pub grip_drag: Option<(Grip, f32, f32)>,
    pub grip_held: Option<Grip>,
    pub grip_lift: Spring,
    /// Ctrl is held over the open Home tab: the sections show their handles.
    pub ctrl_held: bool,
    pub tab_label_w: [f32; TAB_COUNT],
    pub hl_x: Spring,
    pub hl_w: Spring,
    pub content_fade: Spring,
    pub pinned: bool,
    pub pin: Spring,
    /// 0 = dark, 1 = light.
    pub theme: Spring,
    /// Droplet: seconds since release (negative = inactive) and distance below the edge.
    pub drop_t: f32,
    pub drop_y: Spring,
    /// Cursor in window dp.
    pub cursor: (f32, f32),
    pub hover_tab: Option<usize>,
    pub time: (u16, u16),
    /// Seconds of animated time, for procedural motion (equalizer, mascot).
    pub t: f32,
    pub demo_cursor: bool,
    pub caption: &'static [u16],
    /// Set when more than the liquid edge changed, so the next frame repaints
    /// the whole island. Cleared by the app after a successful frame.
    pub repaint: bool,
    /// Set by `tick` when the only motion is the ambient music wave.
    pub ambient: bool,
    /// Look settings.
    pub look: LookState,
    /// Seconds since a notification splashed droplets (negative: none).
    pub splash_t: f32,
    /// Seconds an important alert's outline still pulses.
    pub alert_t: f32,
    /// The tab icon bouncing and seconds since it was picked.
    pub tab_pop: (usize, f32),
    /// The music card leaning towards the pointer (-1..1 each way).
    pub lean: [Spring; 2],
    /// This frame repaints the whole window (clean-up after the panel closed).
    pub full_frame: bool,
    /// A drop falling from the music wave.
    pub beat_drop: Option<BeatDrop>,
    /// Song lyrics (pill line, card view).
    pub lyric: LyricView,
}

pub const REVEAL_MAX: f32 = 0.6;

impl State {
    pub fn new() -> Self {
        Self {
            media: MediaView { session: false, playing: false, title: Vec::new(), artist: Vec::new(), source: Vec::new(), position: 0.0, duration: 0.0, stamp: std::time::Instant::now(), can: Default::default() },
            volume: 0.0,
            outputs: Vec::new(),
            device_menu: false,
            download: Default::default(),
            done: DoneView::default(),
            timer_left: 0,
            timer_label: Vec::new(),
            toast: ToastView { icon: crate::events::Icon::Info, title: Vec::new(), text: Vec::new(), accent: theme::accent() },
            permit: PermitView::default(),
            permit_hover: None,
            radios: Default::default(),
            dark: true,
            chat: ChatView::default(),
            recording: false,
            record_secs: 0,
            voice_text: Vec::new(),
            hover: None,
            hover_old: None,
            hover_in: Spring::new(0.0, 520.0, 42.0),
            hover_out: Spring::new(0.0, 380.0, 39.0),
            settings: Default::default(),
            mascot: None,
            pose: Default::default(),
            weather: None,
            stats: None,
            stats_pill: false,
            volume_shown: false,
            weather_note: Vec::new(),
            sky_phase: 0.0,
            wx: Default::default(),
            vpn_on: false,
            vpn_text: Vec::new(),
            confirm: None,
            confirm_left: 0.0,
            cmd: Spring::unit(0.0),
            cmd_text: Vec::new(),
            cmd_rows: Vec::new(),
            cmd_sel: 0,
            clip_rows: Vec::new(),
            clip_total: 0,
            clip_first: 0,
            clip_query: Vec::new(),
            clip_focus: false,
            shelf_rows: Vec::new(),
            shelf_total: 0,
            shelf_first: 0,
            note: NoteView::default(),
            dock: Vec::new(),
            dock_hover: None,
            dock_drop: false,
            dock_hint: false,
            agent_working: false,
            mic: false,
            cam: false,
            mode: Mode::Idle,
            pill: Pill::Clock,
            w: Spring::ui(PILL_W),
            h: Spring::ui(PILL_H),
            r: Spring::ui(26.0),
            bump: Spring::liquid(0.0),
            bump_x: Spring::new(CX, 220.0, 22.0),
            wave: Spring::unit(0.0),
            focus: Spring::unit(0.0),
            focus_x: Spring::new(CX, 220.0, 24.0),
            wave_phase: 0.0,
            wave_level: wave_neutral(WaveStyle::default().react),
            wave_style: WaveStyle::default(),
            gallery: false,
            pet_only: false,
            tab_dir: 1.0,
            tab_order: std::array::from_fn(|i| i),
            tab_drag: None,
            ripple: None,
            edge: (-10_000.0, 10_000.0),
            merge: [Spring::new(0.0, 150.0, 11.0), Spring::new(0.0, 150.0, 11.0)],
            comp_wave: false,
            noise_phase: 0.0,
            glow: Spring::unit(0.0),
            accent: theme::accent(),
            accent_target: theme::accent(),
            accent2: theme::accent(),
            accent2_target: theme::accent(),
            pill_fade: Spring::unit(1.0),
            reveal: 0.0,
            tab: 0,
            tab_w: [Spring::ui(TAB_W); TAB_COUNT],
            tab_x: [Spring::new(0.0, TAB_GLIDE.0, TAB_GLIDE.1); TAB_COUNT],
            tab_lift: Spring::unit(0.0),
            tab_held: None,
            quick_off: [(Spring::new(0.0, TAB_GLIDE.0, TAB_GLIDE.1), Spring::new(0.0, TAB_GLIDE.0, TAB_GLIDE.1)); 12],
            dock_off: [Spring::new(0.0, TAB_GLIDE.0, TAB_GLIDE.1); 10],
            home_drag: None,
            home_held: None,
            home_lift: Spring::unit(0.0),
            order: std::array::from_fn(|g| (0..Group::ALL[g].len()).collect()),
            grip_off: [[[Spring::new(0.0, TAB_GLIDE.0, TAB_GLIDE.1); 4]; 8]; 7],
            grip_drag: None,
            grip_held: None,
            grip_lift: Spring::unit(0.0),
            ctrl_held: false,
            tab_label_w: [40.0; TAB_COUNT],
            hl_x: Spring::ui(0.0),
            hl_w: Spring::ui(TAB_W),
            content_fade: Spring::unit(1.0),
            pinned: false,
            pin: Spring::unit(0.0),
            theme: Spring::unit(0.0),
            drop_t: -1.0,
            drop_y: Spring::new(0.0, 150.0, 11.0),
            cursor: (-1000.0, -1000.0),
            hover_tab: None,
            time: (0, 0),
            t: 0.0,
            demo_cursor: false,
            caption: &[],
            repaint: true,
            ambient: false,
            look: LookState::default(),
            splash_t: -1.0,
            alert_t: 0.0,
            tab_pop: (0, BOUNCE_SECS),
            lean: [Spring::new(0.0, 120.0, 16.0), Spring::new(0.0, 120.0, 16.0)],
            full_frame: false,
            beat_drop: None,
            lyric: LyricView::new(),
        }
    }

    /// Applies the look settings: corners, pill width, and how the island moves.
    pub fn set_look(&mut self, look: LookState, motion: (f32, f32)) {
        self.look = look;
        for s in [&mut self.w, &mut self.h, &mut self.r] {
            s.tune(motion.0, motion.1);
        }
        if !tab_shown(self.tab) {
            self.set_tab(0);
        }
        self.layout_tabs();
        self.retarget();
        self.repaint = true;
    }

    /// A notification arrived: droplets splash and the edge wobbles; an
    /// important one makes the outline pulse.
    pub fn splash(&mut self, important: bool) {
        if self.look.splash {
            self.splash_t = 0.0;
            self.bump_x.snap(CX);
            self.bump.v += 240.0;
        }
        if important && self.look.alert_glow {
            self.alert_t = ALERT_SECS;
        }
    }

    /// Height of the line pills (clock, music, toasts ...), dp.
    pub fn pill_h(&self) -> f32 {
        PILL_H * self.look.pill_height
    }

    /// How far pill content moves down to stay centred in a taller (or shorter) pill.
    pub fn pill_dy(&self) -> f32 {
        // The permission and "Downloaded" cards have heights of their own.
        if matches!(self.pill, Pill::Permit | Pill::Done) {
            return 0.0;
        }
        (self.pill_h() - PILL_H) * 0.5
    }

    /// The open panel's size on screen, dp.
    pub fn panel_size(&self) -> (f32, f32) {
        (PANEL_W * self.look.panel.0, PANEL_H * self.look.panel.1)
    }

    /// Panel content is drawn this many times bigger than designed: the smaller
    /// of the width and height factors, so everything fits.
    pub fn panel_scale(&self) -> f32 {
        self.look.panel.0.min(self.look.panel.1)
    }

    /// The panel's size in content units (before `panel_scale`): at least the
    /// designed 650 x 330; the extra goes to the side that grew more.
    pub fn panel_local(&self) -> (f32, f32) {
        let (w, h) = self.panel_size();
        let k = self.panel_scale();
        (w / k, h / k)
    }

    /// Tabs with a fixed layout sit in the middle of a taller panel.
    pub fn fixed_dy(&self) -> f32 {
        ((self.panel_local().1 - PANEL_H) * 0.5).max(0.0)
    }

    /// Window dp -> panel content units (the inverse of the content's zoom about (CX, 0)).
    pub fn to_panel(&self, x: f32, y: f32) -> (f32, f32) {
        let k = self.panel_scale();
        (CX + (x - CX) / k, y / k)
    }

    /// Panel content units -> window dp.
    pub fn panel_to_window(&self, x: f32, y: f32) -> (f32, f32) {
        let k = self.panel_scale();
        (CX + (x - CX) * k, y * k)
    }

    /// Size and corner radius the island is heading for.
    pub fn target_dims(&self) -> (f32, f32, f32) {
        let (pill_r, panel_r) = self.look.corners;
        let resting = PILL_W * self.look.pill_size;
        let line = self.pill_h();
        // A short pill cannot have corners rounder than half its height.
        let pill_r = pill_r.min(line * 0.5);
        match self.mode {
            Mode::Hidden => (resting * 0.55, 5.0, 2.5),
            Mode::Expanded => {
                let (w, h) = self.panel_size();
                (w, h, panel_r)
            }
            Mode::Command => {
                let rows = self.cmd_rows.len() as f32;
                (COMMAND_W, COMMAND_INPUT_H + rows * COMMAND_ROW_H + if rows > 0.0 { 12.0 } else { 0.0 }, pill_r)
            }
            Mode::Idle => match self.pill {
                Pill::Clock | Pill::Mascot => (resting, line, pill_r),
                Pill::Music => (570.0, line, pill_r),
                Pill::Download => (520.0, line, pill_r),
                Pill::Timer => (resting, line, pill_r),
                Pill::Record => (380.0, line, pill_r),
                Pill::Listen => (520.0, line, pill_r),
                Pill::Toast => (440.0, line, pill_r),
                Pill::Permit => (PERMIT_W, self.permit.h.max(PERMIT_H), panel_r),
                Pill::Done => (520.0, DONE_H, panel_r),
            },
        }
    }

    /// Springs the shape towards the size the current mode and pill want.
    pub fn retarget_now(&mut self) {
        self.retarget();
    }

    fn retarget(&mut self) {
        let (w, h, r) = self.target_dims();
        self.w.set(w);
        self.h.set(h);
        self.r.set(r);
        if self.mode != Mode::Idle {
            self.bump.set(0.0);
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.cmd.set(if mode == Mode::Command { 1.0 } else { 0.0 });
        self.retarget();
    }

    /// Starts the "press again" window of a power button.
    pub fn arm_confirm(&mut self, action: Option<crate::control::Power>) {
        self.confirm = action;
        self.confirm_left = if action.is_some() { CONFIRM_SECS } else { 0.0 };
        self.repaint = true;
    }

    pub fn set_pill(&mut self, pill: Pill) {
        if self.pill != pill {
            self.pill = pill;
            self.pill_fade.x = 0.0;
            self.pill_fade.set(1.0);
            self.retarget();
        }
    }

    /// Music playing: wave along the bottom edge in the album accent.
    pub fn set_music(&mut self, on: bool, accent: Rgba, accent2: Rgba) {
        self.wave.set(if on { 1.0 } else { 0.0 });
        if on {
            self.accent_target = accent;
            self.accent2_target = accent2;
        }
        self.update_glow();
    }

    /// Solid accent line (download progress / completion) without the wave.
    pub fn set_line(&mut self, on: bool, accent: Rgba) {
        if on {
            self.accent_target = accent;
            if self.wave.target < 0.5 {
                self.accent2_target = accent;
            }
        }
        self.glow.set(if on || self.wave.target > 0.5 { 1.0 } else { 0.0 });
        if self.glow.target == 0.0 {
            self.accent_target = theme::accent();
            self.accent2_target = theme::accent();
        }
    }

    fn update_glow(&mut self) {
        self.glow.set(if self.wave.target > 0.5 { 1.0 } else { 0.0 });
        if self.glow.target == 0.0 {
            self.accent_target = theme::accent();
            self.accent2_target = theme::accent();
        }
    }

    /// Middle amplitude of the music wave for the current level, in dp.
    pub fn wave_mid_amp(&self) -> f32 {
        wave_mid(self.wave_level, self.wave_style.react)
    }

    /// Space between the island's sides and the screen's left / right edge, dp.
    pub fn edge_gaps(&self) -> [f32; 2] {
        let half = self.w.x * 0.5;
        [CX - half - self.edge.0, self.edge.1 - (CX + half)]
    }

    /// For each side touching a screen edge: how far in the joint's line runs
    /// (`ui::merge_paths`), where the liquid edge has to be still to meet it.
    pub fn merge_guard(&self) -> [f32; 2] {
        let (m, gaps) = (self.merged(), self.edge_gaps());
        let reach = |k: usize| if m[k] > 0.004 && gaps[k] < 1.0 { self.r.x.max(MERGE_FLARE * m[k] * 2.4) } else { 0.0 };
        [reach(0), reach(1)]
    }

    /// How much of each side is merged with the screen edge (0..~1.2 with the wobble).
    pub fn merged(&self) -> [f32; 2] {
        [self.merge[0].x.max(0.0), self.merge[1].x.max(0.0)]
    }

    /// Phase speed of the music wave (radians per second).
    pub fn wave_speed(&self) -> f32 {
        wave_speed(self.wave_style.speed)
    }

    /// Colour of the music line's upper side.
    pub fn line2(&self) -> Rgba {
        if self.wave_style.two_colours {
            self.accent2
        } else {
            self.accent
        }
    }

    /// The pointer moved onto another button (or off all of them): the new one
    /// lights up, the old one fades from wherever it had got to.
    pub fn set_hover(&mut self, hit: Option<crate::ui::Hit>) {
        if hit == self.hover {
            return;
        }
        self.hover_old = self.hover;
        self.hover_out.snap(self.hover_in.x);
        self.hover_out.set(0.0);
        self.hover = hit;
        self.hover_in.snap(0.0);
        self.hover_in.set(if hit.is_some() { 1.0 } else { 0.0 });
    }

    pub fn set_tab(&mut self, tab: usize) {
        if tab != self.tab && tab < TAB_COUNT && tab_shown(tab) {
            self.tab_dir = if self.tab_slot(tab) > self.tab_slot(self.tab) { 1.0 } else { -1.0 };
            self.tab = tab;
            if self.look.lively {
                self.tab_pop = (tab, 0.0);
            }
            self.content_fade.x = 0.0;
            self.content_fade.set(1.0);
        }
        self.layout_tabs();
    }

    /// Target width of tab `i` (the active one also shows its label).
    fn tab_target_w(&self, i: usize) -> f32 {
        if i == self.tab {
            TAB_W + self.tab_label_w[i] + 8.0
        } else {
            TAB_W
        }
    }

    /// Where tab `i` sits in the bar (its index in `tab_order`).
    pub fn tab_slot(&self, i: usize) -> usize {
        self.tab_order.iter().position(|&t| t == i).unwrap_or(i)
    }

    /// The shown tabs, left to right.
    pub fn tabs_in_order(&self) -> impl Iterator<Item = usize> + '_ {
        self.tab_order.iter().copied().filter(|&i| tab_shown(i))
    }

    /// Moves tab `i` one place towards `dir` (-1 left, 1 right) among the shown
    /// tabs. Returns `true` if it moved.
    pub fn shift_tab(&mut self, i: usize, dir: i32) -> bool {
        let shown: Vec<usize> = self.tabs_in_order().collect();
        let Some(at) = shown.iter().position(|&t| t == i) else { return false };
        let other = match dir {
            d if d < 0 && at > 0 => shown[at - 1],
            d if d > 0 && at + 1 < shown.len() => shown[at + 1],
            _ => return false,
        };
        let (a, b) = (self.tab_slot(i), self.tab_slot(other));
        self.tab_order.swap(a, b);
        self.layout_tabs();
        true
    }

    pub fn layout_tabs(&mut self) {
        let mut x = 0.0;
        for i in self.tab_order {
            if !tab_shown(i) {
                self.tab_w[i].snap(0.0);
                continue;
            }
            let w = self.tab_target_w(i);
            self.tab_w[i].set(w);
            self.tab_x[i].set(x);
            if i == self.tab {
                self.hl_x.set(x);
                self.hl_w.set(w);
            }
            x += w + 2.0;
        }
    }

    /// Snaps the tab bar to its targets (used before the first reveal).
    pub fn snap_tabs(&mut self) {
        self.layout_tabs();
        for s in self.tab_w.iter_mut().chain(self.tab_x.iter_mut()).chain([&mut self.hl_x, &mut self.hl_w]) {
            s.snap(s.target);
        }
    }

    /// No Home tile or dock icon (or other moved item) is still gliding back to its slot.
    pub fn home_settled(&self) -> bool {
        self.quick_off.iter().all(|(x, y)| x.settled() && y.settled()) && self.dock_off.iter().all(|s| s.settled()) && self.grip_off.iter().flatten().flatten().all(|s| s.settled())
    }

    /// Ids of `group` in the order they are shown.
    pub fn order_of(&self, group: Group) -> &[usize] {
        &self.order[group.index()]
    }

    /// Where item `id` of `group` is drawn when its slot is `slot`: the slot
    /// plus the offset it is gliding back from, or the pointer while held.
    /// Also returns how lifted it is (0 unless it is the held one).
    pub fn grip_rect(&self, group: Group, id: usize, slot: crate::ui::Area) -> (crate::ui::Area, f32) {
        let o = &self.grip_off[group.index()][id.min(7)];
        let mut r = crate::ui::Area { x: slot.x + o[0].x, y: slot.y + o[1].x, w: (slot.w + o[2].x).max(1.0), h: (slot.h + o[3].x).max(1.0) };
        let grip = Grip { group, id };
        if let Some((held, x, y)) = self.grip_drag {
            if held == grip {
                (r.x, r.y) = (x, y);
            }
        }
        let lift = if self.grip_held == Some(grip) { self.grip_lift.x.clamp(0.0, 1.0) } else { 0.0 };
        (r, lift)
    }

    /// `true` when `id` of `group` is the item held (or settling): draw it last.
    pub fn grip_on_top(&self, group: Group, id: usize) -> bool {
        self.grip_held == Some(Grip { group, id })
    }

    /// Ctrl + press picked up tab `i`: it lifts.
    pub fn hold_tab(&mut self, i: usize, left: f32) {
        self.tab_drag = Some((i, left));
        self.tab_held = Some(i);
        self.tab_lift.set(1.0);
    }

    /// The held tab was let go at `left` (from `tabs_x`): it settles into its
    /// slot from there instead of jumping, and sinks back down.
    pub fn drop_tab(&mut self, left: f32) {
        if let Some((i, _)) = self.tab_drag.take() {
            self.tab_x[i].snap(left);
            self.tab_x[i].v = 0.0;
        }
        self.tab_lift.set(0.0);
        self.layout_tabs();
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        self.pin.set(if pinned { 1.0 } else { 0.0 });
    }

    /// Releases a droplet below the bottom edge; it is absorbed back shortly after.
    pub fn release_droplet(&mut self) {
        self.drop_t = 0.0;
        self.drop_y.snap(0.0);
        self.drop_y.v = 220.0;
        self.drop_y.set(34.0);
    }

    /// How far the shape is toward the full panel, 0..1.
    pub fn open(&self) -> f32 {
        let pill = self.pill_h();
        clamp01((self.h.x - pill) / (self.panel_size().1 - pill))
    }

    /// How far the shape has grown beyond the current pill's own height, 0..1.
    pub fn past_pill(&self) -> f32 {
        let pill_h = match self.pill {
            Pill::Done => DONE_H,
            Pill::Permit => self.permit.h.max(PERMIT_H),
            _ => self.pill_h(),
        };
        clamp01((self.h.x - pill_h) / (self.panel_size().1 - pill_h).max(1.0))
    }

    /// Droplet centre distance below the edge and its radius, if visible.
    pub fn droplet(&self) -> Option<(f32, f32)> {
        if self.drop_t < 0.0 {
            return None;
        }
        Some((self.drop_y.x.max(0.0), DROP_R * clamp01(self.drop_y.x / 9.0)))
    }

    pub fn shape(&self) -> Shape {
        let wave = clamp01(self.wave.x);
        let calm = clamp01(self.h.x / 40.0);
        let (drop_amp, drop_sigma, drop_x) = match (self.droplet(), self.beat_drop) {
            (Some((dist, _)), _) => {
                let (amp, sigma) = droplet_bridge(dist, DROP_R);
                (amp, sigma, CX)
            }
            // A beat drop swelling out of the music wave and hanging from it.
            (None, Some(drop)) => {
                let (amp, sigma, ..) = drop.shape();
                (amp, sigma, drop.x)
            }
            (None, None) => (0.0, DROP_R, CX),
        };
        // While compositor layers draw the wave, the body is drawn slightly
        // shorter and without a wave of its own.
        let live = if self.comp_wave { 0.0 } else { wave };
        let liquid = Liquid {
            raise: if self.comp_wave { WAVE_RAISE } else { 0.0 },
            bump_amp: self.bump.x * calm,
            bump_x: self.bump_x.x,
            wave_amp: live * WAVE_AMP,
            wave_mid: live * self.wave_mid_amp(),
            wave_simple: self.wave_style.simple,
            wave_focus: live * clamp01(self.focus.x) * 34.0,
            wave_focus_x: self.focus_x.x,
            wave_phase: self.wave_phase,
            noise_amp: 1.25 * calm * (1.0 - wave),
            noise_phase: self.noise_phase,
            drop_amp,
            drop_sigma,
            drop_x,
            guard: self.merge_guard(),
        };
        Shape::new(CX, self.w.x, self.h.x, self.r.x, liquid)
    }

    /// Reacts to the cursor position: the idle pill grows a teardrop toward a
    /// cursor approaching from below; the music wave sharpens under the cursor.
    pub fn reach(&mut self) {
        let (x, y) = self.cursor;
        let (left, right) = (CX - self.w.x * 0.5, CX + self.w.x * 0.5);
        let dy = y - self.h.x;
        let within_x = x > left - 30.0 && x < right + 30.0;
        if self.wave.target > 0.5 && self.mode != Mode::Hidden {
            const RANGE: f32 = 120.0;
            let near = within_x && dy > -40.0 && dy < RANGE;
            if near {
                let closeness = 1.0 - clamp01(dy.max(0.0) / RANGE);
                self.focus.set(closeness * closeness.sqrt());
                self.focus_x.set(x.clamp(left + 24.0, right - 24.0));
            } else {
                self.focus.set(0.0);
            }
            self.bump.set(0.0);
        } else if self.mode == Mode::Idle {
            const RANGE: f32 = 86.0;
            if within_x && dy > 0.0 && dy < RANGE {
                self.bump.set(31.0 * (1.0 - dy / RANGE).powf(0.8));
                self.bump_x.set(x.clamp(left + 36.0, right - 36.0));
            } else {
                self.bump.set(0.0);
            }
            self.focus.set(0.0);
        } else {
            self.bump.set(0.0);
            self.focus.set(0.0);
        }
    }

    fn wants_reveal(&self) -> bool {
        self.mode == Mode::Expanded && self.open() > 0.7
    }

    /// `true` when a `tick` would not change anything, so no frame is needed.
    pub fn at_rest(&self) -> bool {
        let springs = [
            &self.w,
            &self.h,
            &self.r,
            &self.bump,
            &self.bump_x,
            &self.wave,
            &self.focus,
            &self.focus_x,
            &self.glow,
            &self.pill_fade,
            &self.hl_x,
            &self.hl_w,
            &self.content_fade,
            &self.pin,
            &self.theme,
            &self.cmd,
            &self.hover_in,
            &self.hover_out,
        ];
        springs.iter().all(|s| s.settled())
            && self.confirm_left <= 0.0
            && self.tab_w.iter().chain(self.tab_x.iter()).all(|s| s.settled())
            && self.tab_lift.settled()
            && self.tab_held.is_none()
            && self.home_settled()
            && self.home_lift.settled()
            && self.home_held.is_none()
            && self.grip_lift.settled()
            && self.grip_held.is_none()
            && self.reveal == if self.wants_reveal() { REVEAL_MAX } else { 0.0 }
            && (self.wave.x <= 0.004 || self.comp_wave)
            && self.drop_t < 0.0
            && self.accent == self.accent_target
            && self.accent2 == self.accent2_target
            && self.merge.iter().all(|s| s.settled())
            && self.splash_t < 0.0
            && self.alert_t <= 0.0
            && self.tab_pop.1 >= BOUNCE_SECS
            && self.lean.iter().all(|s| s.settled())
            && self.beat_drop.is_none()
            && self.lyric.fade.settled()
            && self.lyric.scroll.settled()
    }

    /// `true` when the music wave is on and nothing else about the island's
    /// outline or colours is changing: the compositor can animate it alone.
    pub fn wave_steady(&self) -> bool {
        let still = [&self.w, &self.h, &self.r, &self.wave, &self.glow, &self.theme, &self.focus, &self.bump];
        // Merged with a screen edge, the main buffer draws the joined corner and
        // the compositor's corner layers would not match it.
        self.wave.target > 0.5
            && self.merged().iter().all(|m| *m < 0.004)
            && still.iter().all(|s| s.settled())
            && self.focus.target == 0.0
            && self.bump.target == 0.0
            && self.drop_t < 0.0
            && self.mode != Mode::Hidden
            && self.accent == self.accent_target
            && self.accent2 == self.accent2_target
            // (A falling drop needs the live wave: the layers cannot grow it.)
            && self.beat_drop.is_none()
    }

    /// Lets a drop fall from the music wave at `x` (window dp).
    pub fn drop_from_wave(&mut self, x: f32) {
        if self.beat_drop.is_none() {
            self.beat_drop = Some(BeatDrop { x, t: 0.0, base: self.h.x });
            self.repaint = true;
        }
    }

    /// Advances every animation by `dt`. Returns `true` while anything still moves.
    pub fn tick(&mut self, dt: f32) -> bool {
        // "Body" animations change the whole island; "liquid" ones only its bottom edge.
        let mut body = false;
        for s in [
            &mut self.w,
            &mut self.h,
            &mut self.r,
            &mut self.wave,
            &mut self.glow,
            &mut self.pill_fade,
            &mut self.hl_x,
            &mut self.hl_w,
            &mut self.content_fade,
            &mut self.pin,
            &mut self.theme,
            &mut self.cmd,
            &mut self.hover_in,
            &mut self.hover_out,
        ] {
            body |= s.step(dt);
        }
        // The "press again" window of a power button runs down and then reverts.
        if self.confirm_left > 0.0 {
            self.confirm_left -= dt;
            if self.confirm_left <= 0.0 {
                self.confirm_left = 0.0;
                self.confirm = None;
            }
            body = true;
        }
        for s in self.tab_w.iter_mut().chain(self.tab_x.iter_mut()) {
            body |= s.step(dt);
        }
        body |= self.tab_lift.step(dt);
        for (sx, sy) in &mut self.quick_off {
            body |= sx.step(dt) | sy.step(dt);
        }
        for s in &mut self.dock_off {
            body |= s.step(dt);
        }
        body |= self.home_lift.step(dt);
        for s in self.grip_off.iter_mut().flatten().flatten() {
            body |= s.step(dt);
        }
        body |= self.grip_lift.step(dt);
        if self.grip_held.is_some() && self.grip_drag.is_none() && self.grip_lift.settled() && self.home_settled() {
            self.grip_held = None;
            body = true;
        }
        if self.home_held.is_some() && self.home_drag.is_none() && self.home_lift.settled() && self.home_settled() {
            self.home_held = None;
            body = true;
        }
        // A let-go tab stays on top until it has landed and sunk back.
        if let Some(i) = self.tab_held {
            if self.tab_drag.is_none() && self.tab_lift.settled() && self.tab_x[i].settled() {
                self.tab_held = None;
                body = true;
            }
        }
        // Notification splash, alert pulse, tab bounce, the music card's lean.
        if self.splash_t >= 0.0 {
            self.splash_t += dt;
            if self.splash_t > SPLASH_SECS {
                self.splash_t = -1.0;
            }
            body = true;
        }
        if self.alert_t > 0.0 {
            self.alert_t = (self.alert_t - dt).max(0.0);
            body = true;
        }
        if self.tab_pop.1 < BOUNCE_SECS {
            self.tab_pop.1 += dt;
            body = true;
        }
        for s in &mut self.lean {
            body |= s.step(dt);
        }
        // A new lyric line slides in; the card's list scrolls to it.
        body |= self.lyric.fade.step(dt);
        body |= self.lyric.scroll.step(dt);

        // Panel content reveals once the shape is ~70 % open, and hides quickly.
        let want = self.wants_reveal();
        if want && self.reveal < REVEAL_MAX {
            self.reveal = (self.reveal + dt).min(REVEAL_MAX);
            body = true;
        } else if !want && self.reveal > 0.0 {
            self.reveal = (self.reveal - dt * 5.0).max(0.0);
            body = true;
        }

        let mut moving = false;
        for s in [&mut self.bump, &mut self.bump_x] {
            moving |= s.step(dt);
        }
        // (The wave sharpening under the cursor wants every refresh; see `ambient`.)
        let mut pointer = false;
        for s in [&mut self.focus, &mut self.focus_x] {
            pointer |= s.step(dt);
        }
        moving |= pointer;

        // The music wave travels continuously for as long as it is visible.
        let live_wave = self.wave.x > 0.004 && !self.comp_wave;
        if live_wave {
            self.wave_phase = (self.wave_phase + dt * self.wave_speed()) % std::f32::consts::TAU;
        }

        // A drop from the music wave: when its neck snaps the edge springs back
        // up with a little wobble.
        let edge_here = self.beat_drop.map(|drop| {
            let shape = self.shape();
            shape.edge_y(drop.x) - shape.liquid.drop_amp
        });
        if let Some(drop) = &mut self.beat_drop {
            let before = drop.t;
            if drop.t < DROP_BREAK {
                drop.base = edge_here.unwrap_or(drop.base);
            }
            drop.t += dt;
            let (x, snapped, done) = (drop.x, drop.snapped(before), drop.done());
            if snapped {
                self.bump_x.snap(x);
                self.bump.v -= 150.0;
            }
            if done {
                self.beat_drop = None;
            }
            moving = true;
        }

        if self.drop_t >= 0.0 {
            self.drop_t += dt;
            if self.drop_t > 0.42 {
                self.drop_y.set(0.0);
            }
            let alive = self.drop_y.step(dt);
            if self.drop_t > 0.42 && (!alive || self.drop_y.x <= 0.2) {
                self.drop_t = -1.0;
                self.drop_y.snap(0.0);
            }
        }

        if self.accent != self.accent_target || self.accent2 != self.accent2_target {
            let k = 1.0 - (-dt * 9.0).exp();
            for (c, target) in [(&mut self.accent, self.accent_target), (&mut self.accent2, self.accent2_target)] {
                *c = c.mix(target, k);
                let d = (c.r - target.r).abs() + (c.g - target.g).abs() + (c.b - target.b).abs();
                if d < 0.006 {
                    *c = target;
                }
            }
            body = true;
        }

        if let Some(ripple) = &mut self.ripple {
            ripple.t += dt / RIPPLE_SECS;
            if ripple.t >= 1.0 {
                self.ripple = None;
            }
            body = true;
        }

        // Merging with a screen edge: a web reaches out as the island comes
        // within `MERGE_REACH`, and it fuses (with a wobble) once it touches.
        let gaps = self.edge_gaps();
        for (spring, gap) in self.merge.iter_mut().zip(gaps) {
            let target = if gap < 0.75 { 1.0 } else { 0.5 * (1.0 - gap / MERGE_REACH).max(0.0).powi(2) };
            spring.set(target);
            body |= spring.step(dt);
        }

        // Only the slow travelling wave is moving (perhaps with a beat drop and
        // the edge's recoil after it): half the refresh rate is enough.
        let drop_only = self.beat_drop.is_some() || (moving && self.bump.target == 0.0 && !pointer && self.wave.target > 0.5);
        self.ambient = !body && (!moving || drop_only) && !pointer && self.drop_t < 0.0 && live_wave;
        if live_wave || self.drop_t >= 0.0 {
            moving = true;
        }
        if body {
            self.repaint = true;
            moving = true;
        }
        if moving {
            // The idle undulation only drifts while something else animates,
            // so a resting island is a still image.
            self.noise_phase = (self.noise_phase + dt * 1.5) % (std::f32::consts::TAU * 10.0);
            self.t += dt;
        }
        moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drop's phases join without jumps, the neck is gone when it falls,
    /// it falls downwards, fades out and ends.
    #[test]
    fn beat_drop_timeline() {
        let at = |t: f32| BeatDrop { x: 0.0, t, base: 0.0 }.shape();
        let (before, after) = (at(DROP_SWELL - 1e-4), at(DROP_SWELL + 1e-4));
        assert!((before.0 - after.0).abs() < 0.1 && (before.1 - after.1).abs() < 0.1, "{before:?} {after:?}");
        let (hanging, falling) = (at(DROP_BREAK - 1e-4), at(DROP_BREAK + 1e-4));
        assert!(hanging.0 < 0.1 && falling.0 == 0.0);
        assert!((hanging.2.unwrap() - falling.2.unwrap()).abs() < 0.1);
        let mut last = 0.0;
        for k in 0..20 {
            let dist = at(DROP_BREAK + k as f32 * 0.03).2.unwrap();
            assert!(dist >= last);
            last = dist;
        }
        assert!(at(DROP_END - 0.01).3 < 0.1 && at(0.8).3 == 1.0);
        let mut s = State::new();
        s.drop_from_wave(CX);
        for _ in 0..100 {
            s.tick(1.0 / 60.0);
        }
        assert!(s.beat_drop.is_none());
    }
}
