//! The settings window (the panel's sliders icon, the tray menu): groups and a
//! search field on the left, the settings on the right. Every row edits one
//! line of `config.txt` (see `config::set`), so the file stays the single source
//! and hand edits keep working. The app applies each change at once where it can.

use crate::render::{ws, Align, Font, Gfx, Rgba};
use crate::theme::{self, Palette};
use crate::ui::Hit;
use windows::core::w;

pub enum Kind {
    Header,
    Toggle,
    /// (value written to the file, label).
    Choice(&'static [(&'static str, &'static str)]),
    /// UI scale in percent, 80-200 in steps of 10.
    Scale,
    /// The character: previous / next with a live preview of it.
    Pet,
    Hotkey,
    /// A free text value; the placeholder shows while it is empty.
    Text(&'static str),
    /// Start with Windows: the HKCU Run key, not the settings file.
    Startup,
    /// "Open settings file".
    OpenFile,
    /// A short explanation (the row's label) under a setting, this many lines
    /// tall; not a control.
    Note(u8),
    /// Whether the user is signed in to Claude / ChatGPT, with a "Sign in" button.
    Login(crate::cli::Tool),
    /// The chat model: previous / next through the provider's models.
    Model,
    /// Opens the step-by-step setup guide.
    Guide,
    /// A colour picked from a row of swatches (`theme::ACCENTS`, `theme::ISLAND_COLOURS`),
    /// the last one opening a picker for any colour ("#rrggbb" in the file).
    Swatch(&'static [(&'static str, &'static str, u32)]),
    /// The Home tab's quick actions as buttons to switch on and off
    /// (`quick_actions`; their order is set with Ctrl + drag on the tab).
    Chips,
}

/// The colour picker open under a swatch row: the row, and the colour as hue
/// (0..1), saturation and brightness; which part is being dragged (0 the
/// square, 1 the hue strip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picker {
    pub row: usize,
    pub h: f32,
    pub s: f32,
    pub v: f32,
    pub drag: Option<u8>,
}

/// The hue strip (and the "any colour" swatch): red round to red.
const RAINBOW: [(f32, Rgba); 7] = [
    (0.0, Rgba::hex(0xff0000)),
    (1.0 / 6.0, Rgba::hex(0xffff00)),
    (2.0 / 6.0, Rgba::hex(0x00ff00)),
    (3.0 / 6.0, Rgba::hex(0x00ffff)),
    (4.0 / 6.0, Rgba::hex(0x0000ff)),
    (5.0 / 6.0, Rgba::hex(0xff00ff)),
    (1.0, Rgba::hex(0xff0000)),
];

/// The colour picker under a swatch row whose top is `top`: a square of
/// saturation (left to right) and brightness (bottom to top) for the hue, the
/// hue strip, the colour now with its code, and "Done".
fn draw_picker(g: &Gfx, v: &View, pal: &Palette, top: f32, a: f32) {
    let Some(p) = v.picker else { return };
    let k = |c: Rgba| c.alpha(a);
    let [(sx, sy, sw, sh), (hx, hy, hw, hh), (dx, dy, dw, dh)] = pick_rects(top);
    let hue = Rgba::hex(theme::hsv_to_rgb(p.h, 1.0, 1.0));
    // White to the hue across, then clear to black downwards over it.
    g.fill_hgradient(sx, sy, sw, sh, 8.0, &[(0.0, Rgba::hex(0xffffff)), (1.0, hue)], a);
    g.fill_vgradient(sx, sy, sw, sh, 8.0, &[(0.0, Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }), (1.0, Rgba::hex(0x000000))], a);
    let (mx, my) = (sx + p.s * sw, sy + (1.0 - p.v) * sh);
    g.stroke_circle(mx, my, 7.0, 2.4, k(Rgba::hex(0x000000).alpha(0.5)));
    g.stroke_circle(mx, my, 7.0, 1.6, k(Rgba::hex(0xffffff)));
    // The hue strip and its marker.
    g.fill_hgradient(hx, hy, hw, hh, hh * 0.5, &RAINBOW, a);
    let ux = hx + p.h * hw;
    g.stroke_rrect(ux - 4.0, hy - 3.0, 8.0, hh + 6.0, 4.0, 2.0, k(Rgba::hex(0xffffff)));
    // The colour now: a big swatch and its code.
    let colour = theme::hsv_to_rgb(p.h, p.s, p.v);
    g.fill_circle(hx + 22.0, hy + hh + 40.0, 20.0, k(Rgba::hex(colour)));
    g.stroke_circle(hx + 22.0, hy + hh + 40.0, 20.0, 1.0, k(pal.text2.alpha(0.5)));
    let code: Vec<u16> = theme::hex_text(colour).to_uppercase().encode_utf16().collect();
    g.text(&code, Font::Title, hx + 54.0, hy + hh + 26.0, 160.0, 28.0, k(pal.text), Align::Left);
    g.text(crate::render::ws(w!("Drag in the square and along the strip")), Font::Small, hx, hy + hh + 66.0, hw, 18.0, k(pal.text2), Align::Left);
    g.fill_rrect(dx, dy, dw, dh, dh * 0.5, k(theme::accent()));
    g.text(crate::render::ws(w!("Done")), Font::Label, dx, dy, dw, dh, k(Rgba::hex(0x0b0b12)), Align::Center);
}

/// Height the picker adds under its row.
const PICK_H: f32 = 150.0;
/// The picker's square, hue strip and "Done" (offsets from the row's top).
const PICK_SQ: (f32, f32, f32, f32) = (12.0, 44.0, 220.0, 128.0);
const PICK_HUE: (f32, f32, f32, f32) = (252.0, 44.0, 300.0, 18.0);
const PICK_DONE: (f32, f32) = (96.0, 30.0);
/// Quick-action chips: columns, size, gap (under the row's label).
const CHIP_COLS: usize = 6;
const CHIP_H: f32 = 30.0;
const CHIP_GAP: f32 = 6.0;

/// Spacing of the accent swatches, from the right edge leftwards.
const SWATCH_STEP: f32 = 27.0;

/// Centre x of swatch `k` of `n`.
fn swatch_x(k: usize, n: usize) -> f32 {
    RIGHT - 11.0 - (n - 1 - k) as f32 * SWATCH_STEP
}

pub struct Row {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}

const fn row(key: &'static str, label: &'static str, kind: Kind) -> Row {
    Row { key, label, kind }
}

const fn header(label: &'static str) -> Row {
    Row { key: "", label, kind: Kind::Header }
}

/// Every setting, in groups: each `header` starts a group (a category in the
/// window's sidebar).
pub const ROWS: &[Row] = &[
    header("General"),
    row("theme", "Theme", Kind::Choice(&[("dark", "Dark"), ("light", "Light"), ("system", "Like Windows")])),
    row("scale", "Size", Kind::Scale),
    row("", "Start with Windows", Kind::Startup),
    row("island_x", "Position", Kind::Choice(&[("0", "Back to the centre")])),
    row("", "To move the island, hold Ctrl and drag it. Let go near a screen edge and it melts into it.", Kind::Note(1)),
    row("autohide", "Hide to a thin line when not in use", Kind::Toggle),
    row("click_through", "Let clicks pass through the island", Kind::Toggle),
    row("follow_cursor", "Show on the screen the mouse is on", Kind::Toggle),
    header("Colours"),
    row("accent", "Accent", Kind::Swatch(&theme::ACCENTS)),
    row("", "Buttons that are on, highlights and links. The last circle: any colour you like.", Kind::Note(1)),
    row("island_colour", "Island", Kind::Swatch(&theme::ISLAND_COLOURS)),
    row("", "The island itself. \u{266A}: black, with the glow following the song's cover.", Kind::Note(1)),
    row("card_colour", "Cards and buttons", Kind::Swatch(&theme::CARD_COLOURS)),
    row("", "Everything inside the open island. \u{266A}: the song's cover colour while music plays.", Kind::Note(1)),
    row("surface", "Background", Kind::Choice(&[("plain", "Plain"), ("tinted", "Tinted"), ("gradient", "Glow")])),
    row("", "Tinted: a touch of the accent everywhere. Glow: the open island lights up from the top.", Kind::Note(1)),
    header("Shape and motion"),
    row("corners", "Corners", Kind::Choice(&[("capsule", "Capsule"), ("round", "Round"), ("soft", "Soft"), ("square", "Square")])),
    row("pill_size", "Closed island width", Kind::Choice(&[("compact", "Narrow"), ("normal", "Normal"), ("wide", "Wide")])),
    row("pill_height", "Closed island height", Kind::Choice(&[("short", "Low"), ("normal", "Normal"), ("tall", "Tall")])),
    row("panel_width", "Open island width", Kind::Choice(&[("narrow", "Narrow"), ("normal", "Normal"), ("wide", "Wide")])),
    row("panel_height", "Open island height", Kind::Choice(&[("short", "Low"), ("normal", "Normal"), ("tall", "Tall")])),
    row("motion", "How it opens and closes", Kind::Choice(&[("liquid", "Liquid"), ("bouncy", "Bouncy"), ("smooth", "Smooth")])),
    row("panel_motion", "Buttons and cards", Kind::Choice(&[("calm", "Still"), ("lively", "Lively")])),
    row("", "Lively: icons bounce, tiles lift under the mouse, the music card leans towards it.", Kind::Note(1)),
    row("toast_splash", "Splash when a notification arrives", Kind::Toggle),
    row("alert_glow", "Pulse for timers and questions", Kind::Toggle),
    header("Music"),
    row("wave_shape", "Wave", Kind::Choice(&[("liquid", "Liquid"), ("simple", "Simple")])),
    row("wave_music", "Moves with the music", Kind::Choice(&[("off", "Off"), ("soft", "A little"), ("normal", "Normal"), ("strong", "A lot")])),
    row("wave_speed", "Wave speed", Kind::Choice(&[("slow", "Slow"), ("normal", "Normal"), ("fast", "Fast")])),
    row("wave_colours", "Colours from the cover", Kind::Choice(&[("two", "Two"), ("one", "One")])),
    row("wave_glow", "Glow around the wave", Kind::Toggle),
    row("wave_drops", "Drops on the beat", Kind::Choice(&[("off", "Off"), ("subtle", "Few"), ("lively", "Many")])),
    row("lyrics", "Song lyrics", Kind::Toggle),
    row("", "Found on lrclib.net (free). Only the song's title, artist and length are sent.", Kind::Note(1)),
    row("lyrics_pill", "Lyrics line in the closed island", Kind::Toggle),
    header("Character"),
    row("character", "Character", Kind::Pet),
    row("character_motion", "Moves on its own", Kind::Choice(&[("calm", "Rarely"), ("lively", "Often")])),
    row("", "Often: a little trick every few seconds. Rarely: only when music plays, the mouse comes near or an AI agent finishes.", Kind::Note(2)),
    header("Pill and notifications"),
    row("notifications", "Windows notifications", Kind::Toggle),
    row("caps_popup", "Caps Lock on / off", Kind::Toggle),
    row("layout_popup", "Keyboard language changes", Kind::Toggle),
    row("stats_pill", "CPU and GPU next to the clock", Kind::Toggle),
    row("", "Instead of the weather. GPU numbers need an NVIDIA card.", Kind::Note(1)),
    header("Tabs"),
    row("", "Hold Ctrl and drag a tab to move it. Home is always there.", Kind::Note(1)),
    row("tab_clipboard", "Clipboard", Kind::Toggle),
    row("tab_shelf", "Shelf", Kind::Toggle),
    row("tab_system", "System", Kind::Toggle),
    row("tab_chat", "Chat", Kind::Toggle),
    row("tab_notes", "Notes", Kind::Toggle),
    row("tab_weather", "Weather", Kind::Toggle),
    row("tab_controls", "Controls", Kind::Toggle),
    header("Home tab"),
    row("", "Hold Ctrl and drag a button, icon or card to move it; its handle moves a whole part.", Kind::Note(1)),
    row("home_music", "Music card", Kind::Toggle),
    row("home_volume", "Volume", Kind::Toggle),
    row("home_dock", "App bar", Kind::Toggle),
    row("quick_actions", "Buttons", Kind::Chips),
    header("Controls tab"),
    row("ctl_wifi", "Wi-Fi", Kind::Toggle),
    row("ctl_bluetooth", "Bluetooth", Kind::Toggle),
    row("ctl_vpn", "VPN", Kind::Toggle),
    row("ctl_theme", "Dark theme", Kind::Toggle),
    row("ctl_lock", "Lock", Kind::Toggle),
    row("ctl_sleep", "Sleep", Kind::Toggle),
    row("ctl_restart", "Restart", Kind::Toggle),
    row("ctl_shutdown", "Shut down", Kind::Toggle),
    header("Sounds"),
    row("sounds_ui", "Sounds", Kind::Toggle),
    row("sounds_style", "Style", Kind::Choice(&[("soft", "Soft"), ("glassy", "Glassy"), ("retro", "Retro")])),
    row("sounds_volume", "Volume", Kind::Choice(&[("quiet", "Quiet"), ("normal", "Normal"), ("loud", "Loud")])),
    row("sounds_clicks", "Buttons and tabs", Kind::Toggle),
    row("sounds_panel", "Opening and closing", Kind::Toggle),
    row("sounds_alerts", "Notifications and timers", Kind::Toggle),
    row("sounds_character", "The character", Kind::Toggle),
    row("sounds_weather", "Rain, wind and birds on the Weather tab", Kind::Toggle),
    row("sounds_weather_volume", "Weather volume", Kind::Choice(&[("quiet", "Quiet"), ("normal", "Normal"), ("loud", "Loud")])),
    row("", "Islet makes all its sounds itself, and stays silent while a game is in front.", Kind::Note(1)),
    header("Shortcuts"),
    row("hotkey", "Command bar", Kind::Hotkey),
    row("voice_hotkey", "Voice to text", Kind::Hotkey),
    row("", "Click a shortcut, then press the new keys (Ctrl, Alt, Shift or Win with a letter, digit, F-key, Space or Tab).", Kind::Note(2)),
    header("AI chat"),
    row("chat", "AI chat", Kind::Toggle),
    row("", "Step-by-step help", Kind::Guide),
    row("chat_provider", "Use", Kind::Choice(&[("claude", "Claude login"), ("chatgpt", "ChatGPT login"), ("anthropic", "Anthropic key"), ("openai", "Other")])),
    row("", "Claude / ChatGPT login: your subscription, no key, no extra bill (sign in once below). Anthropic key: billed separately. Other: OpenAI, OpenRouter or a model on this PC (set its address below).", Kind::Note(3)),
    row("", "Claude login", Kind::Login(crate::cli::Tool::Claude)),
    row("", "ChatGPT login", Kind::Login(crate::cli::Tool::Codex)),
    row("chat_model", "Model", Kind::Model),
    row("chat_model", "Or type a model name", Kind::Text("the default one")),
    row("chat_url", "Address (Other)", Kind::Text("https://api.openai.com/v1")),
    header("Voice to text"),
    row("voice", "Voice to text", Kind::Choice(&[("off", "Off"), ("local", "On this PC"), ("api", "Online")])),
    row("", "On this PC: free and offline, English only. Online: any language, through OpenAI (needs an OpenAI key).", Kind::Note(2)),
    row("voice_language", "Language (online)", Kind::Text("detect by itself")),
    header("Weather and currency"),
    row("weather", "Weather", Kind::Toggle),
    row("city", "City", Kind::Text("e.g. Vilnius")),
    row("", "From open-meteo.com (free). Only the city's name is sent, once.", Kind::Note(1)),
    row("currency", "Currency conversion", Kind::Toggle),
    row("currency_home", "Your currency", Kind::Text("EUR")),
    row("", "Type \u{201C}100 usd to eur\u{201D} in the command bar. European Central Bank rates, once a day.", Kind::Note(1)),
    header("Advanced"),
    row("suspend_apps", "Sleep while these apps run", Kind::Text("e.g. javaw.exe, game.exe")),
    row("", "Islet uses no CPU while one of these is in front. Fullscreen games are noticed by themselves.", Kind::Note(1)),
    row("renderer", "Drawing", Kind::Choice(&[("software", "Light on memory"), ("gpu", "Graphics card")])),
    row("", "Graphics card: a little less CPU while animating, about 50 MB more memory.", Kind::Note(1)),
    row("update_check", "Check for a newer Islet", Kind::Toggle),
    row("update_url", "Where to look", Kind::Text("GitHub releases API address")),
    row("", "Every setting is in this file too", Kind::OpenFile),
];

/// The groups: (row index of its header, name).
pub fn categories() -> Vec<(usize, &'static str)> {
    ROWS.iter().enumerate().filter(|(_, r)| matches!(r.kind, Kind::Header)).map(|(i, r)| (i, r.label)).collect()
}

/// The group row `i` belongs to (index into [`categories`]).
fn category_of(i: usize) -> usize {
    ROWS[..=i].iter().filter(|r| matches!(r.kind, Kind::Header)).count().saturating_sub(1)
}

/// The rows to show: the chosen group's, or (while searching) every row
/// whose name, key or group matches, under the headings of their groups. A
/// note comes along with the setting above it.
pub fn shown_rows(category: usize, search: &str) -> Vec<usize> {
    let query = search.trim().to_lowercase();
    if query.is_empty() {
        let Some(&(start, _)) = categories().get(category) else { return Vec::new() };
        return (start + 1..ROWS.len()).take_while(|&i| !matches!(ROWS[i].kind, Kind::Header)).collect();
    }
    let groups = categories();
    let matches = |i: usize| {
        let row = &ROWS[i];
        let group = groups.get(category_of(i)).map_or("", |g| g.1);
        [row.label, row.key, group].iter().any(|text| text.to_lowercase().contains(&query))
    };
    let mut rows = Vec::new();
    let mut last_group = usize::MAX;
    let mut last_hit = false;
    for (i, row) in ROWS.iter().enumerate() {
        if matches!(row.kind, Kind::Header) {
            continue;
        }
        // A note shows when the setting above it does.
        let hit = if matches!(row.kind, Kind::Note(_)) { last_hit || matches(i) } else { matches(i) };
        last_hit = hit && !matches!(row.kind, Kind::Note(_));
        if hit {
            let group = category_of(i);
            if group != last_group {
                rows.push(groups[group].0);
                last_group = group;
            }
            rows.push(i);
        }
    }
    rows
}

/// What the page shows for each row (the file's text values) and what is being edited.
#[derive(Default)]
pub struct View {
    pub open: bool,
    /// The current value of each row, as written in the file.
    pub values: Vec<String>,
    pub startup: bool,
    pub scroll: f32,
    /// The text row being typed into, and its text so far.
    pub editing: Option<usize>,
    pub text: Vec<u16>,
    /// The shortcut row waiting for a key combination.
    pub capturing: Option<usize>,
    /// A line at the top, e.g. "Restart Islet to switch the language".
    pub notice: Vec<u16>,
    /// Whether the user is signed in to Claude / ChatGPT (`None` while checking).
    pub claude: Option<crate::cli::Login>,
    pub codex: Option<crate::cli::Login>,
    /// A sign-in window was opened for this one: its button reads "Check".
    pub signing: Option<crate::cli::Tool>,
    /// The models the chosen provider offers, as (value, label).
    pub models: Vec<(String, String)>,
    /// The window: chosen group, the search text (and whether it has the
    /// keyboard), the rows shown, and the control under the pointer.
    pub category: usize,
    pub search: Vec<u16>,
    pub search_focus: bool,
    pub shown: Vec<usize>,
    pub hover: Option<Hit>,
    /// The colour picker open under a swatch row.
    pub picker: Option<Picker>,
}

impl View {
    pub fn login(&self, tool: crate::cli::Tool) -> Option<crate::cli::Login> {
        match tool {
            crate::cli::Tool::Claude => self.claude,
            crate::cli::Tool::Codex => self.codex,
        }
    }

    /// The label of the model `value` (its name when it is not in the list).
    pub fn model_label(&self, value: &str) -> String {
        self.models.iter().find(|(v, _)| v == value).map_or_else(|| if value.is_empty() { "Default".to_string() } else { value.to_string() }, |(_, l)| l.clone())
    }

    /// Works out the rows to show again (after a group or the search changed).
    pub fn refilter(&mut self) {
        self.shown = shown_rows(self.category, &String::from_utf16_lossy(&self.search));
        let most = (content_height(self) - (BOTTOM - TOP) + 8.0).max(0.0);
        self.scroll = self.scroll.clamp(0.0, most);
    }
}

// ---------------------------------------------------------- window layout --

/// The settings window's client size, dp (fixed: it is not resizable).
pub const WIN_W_DP: f32 = 860.0;
pub const WIN_H_DP: f32 = 620.0;
/// The sidebar: search field and groups.
const SIDE_W: f32 = 230.0;
const SEARCH_Y: f32 = 64.0;
const SEARCH_H: f32 = 34.0;
const GROUP_Y: f32 = 112.0;
// (15 groups fit under the search field.)
const GROUP_H: f32 = 31.0;

const TOP: f32 = 66.0;
const BOTTOM: f32 = WIN_H_DP - 14.0;
const ROW_H: f32 = 40.0;
const HEADER_H: f32 = 30.0;
const X0: f32 = SIDE_W + 22.0;
const WIDTH: f32 = WIN_W_DP - X0 - 24.0;
const RIGHT: f32 = X0 + WIDTH - 10.0;
/// Left edge of the character picker's "previous" button.
const PET_PREV: f32 = RIGHT - 170.0;
/// Widths of the Claude sign-in and setup-guide buttons.
const LOGIN_W: f32 = 104.0;
/// Left edge of the model picker's "previous" button.
const MODEL_PREV: f32 = RIGHT - 280.0;
const GUIDE_W: f32 = 190.0;

/// Line height of the help notes.
const NOTE_LINE: f32 = 18.0;

fn height(row: &Row) -> f32 {
    match row.kind {
        Kind::Header => HEADER_H,
        Kind::Note(lines) => 6.0 + lines as f32 * NOTE_LINE,
        _ => ROW_H,
    }
}

/// A row's whole height in the window: its line, plus the colour picker when
/// it is open under it, plus the chips under the quick actions' label.
fn row_height(v: &View, i: usize) -> f32 {
    let row = &ROWS[i];
    let extra = match row.kind {
        Kind::Swatch(_) if v.picker.is_some_and(|p| p.row == i) => PICK_H,
        Kind::Chips => chip_rows() as f32 * (CHIP_H + CHIP_GAP) + 4.0,
        _ => 0.0,
    };
    height(row) + extra
}

/// Rows of quick-action chips.
fn chip_rows() -> usize {
    crate::config::QUICK_NAMES.len().div_ceil(CHIP_COLS)
}

/// Quick-action chip `k` of the chips row whose top is `top`.
fn chip_rect(k: usize, top: f32) -> (f32, f32, f32, f32) {
    let w = (WIDTH - 24.0 - (CHIP_COLS - 1) as f32 * CHIP_GAP) / CHIP_COLS as f32;
    let (col, line) = (k % CHIP_COLS, k / CHIP_COLS);
    (X0 + 12.0 + col as f32 * (w + CHIP_GAP), top + ROW_H - 2.0 + line as f32 * (CHIP_H + CHIP_GAP), w, CHIP_H)
}

/// The picker's parts for a swatch row whose top is `top`: square, hue strip, "Done".
fn pick_rects(top: f32) -> [(f32, f32, f32, f32); 3] {
    let (sx, sy, sw, sh) = PICK_SQ;
    let (hx, hy, hw, hh) = PICK_HUE;
    let (dw, dh) = PICK_DONE;
    [(X0 + sx, top + sy, sw, sh), (X0 + hx, top + hy, hw, hh), (RIGHT - dw, top + sy + sh - dh, dw, dh)]
}

/// The colour under (x, y) of the picker's square or hue strip (`part` 0 / 1):
/// the new (h, s, v).
pub fn pick_at(v: &View, part: u8, x: f32, y: f32) -> Option<(f32, f32, f32)> {
    let p = v.picker?;
    let (_, top) = tops(v).find(|(i, _)| *i == p.row)?;
    let rects = pick_rects(top);
    let (rx, ry, rw, rh) = rects[part.min(1) as usize];
    let fx = ((x - rx) / rw).clamp(0.0, 1.0);
    let fy = ((y - ry) / rh).clamp(0.0, 1.0);
    Some(if part == 0 { (p.h, fx, 1.0 - fy) } else { (fx, p.s, p.v) })
}

pub fn content_height(v: &View) -> f32 {
    v.shown.iter().map(|&i| row_height(v, i)).sum::<f32>() + if v.notice.is_empty() { 0.0 } else { 30.0 }
}

/// How far the content can scroll.
pub fn most_scroll(v: &View) -> f32 {
    (content_height(v) - (BOTTOM - TOP) + 8.0).max(0.0)
}

/// Top of every shown row on screen (scrolled).
fn tops(v: &View) -> impl Iterator<Item = (usize, f32)> + '_ {
    let mut y = TOP + 4.0 - v.scroll + if v.notice.is_empty() { 0.0 } else { 30.0 };
    v.shown.iter().map(move |&i| {
        let top = y;
        y += row_height(v, i);
        (i, top)
    })
}

fn on(value: &str) -> bool {
    matches!(value, "on" | "true" | "1" | "yes")
}

/// "alt+shift+d" -> "Alt+Shift+D".
pub fn shortcut_label(value: &str) -> String {
    value
        .split('+')
        .map(|part| {
            let part = part.trim();
            let mut chars = part.chars();
            match chars.next() {
                Some(first) if part.len() == 1 => first.to_ascii_uppercase().to_string(),
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The settings-file text for a key combination (modifier bits as in `RegisterHotKey`).
pub fn shortcut_text(modifiers: u32, key: u32) -> Option<String> {
    let name = match key {
        0x20 => "space".to_string(),
        0x09 => "tab".to_string(),
        0x30..=0x39 | 0x41..=0x5A => ((key as u8) as char).to_ascii_lowercase().to_string(),
        0x70..=0x7B => format!("f{}", key - 0x6F),
        _ => return None,
    };
    if modifiers == 0 {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for (bit, word) in [(2, "ctrl"), (1, "alt"), (4, "shift"), (8, "win")] {
        if modifiers & bit != 0 {
            parts.push(word);
        }
    }
    Some(format!("{}+{name}", parts.join("+")))
}

/// Width of a choice button (no text measuring at draw time).
fn choice_w(label: &str) -> f32 {
    label.chars().count() as f32 * 7.0 + 22.0
}

pub fn hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if !(TOP..BOTTOM).contains(&y) {
        return None;
    }
    for (i, top) in tops(v) {
        let row = &ROWS[i];
        if !(top..top + row_height(v, i)).contains(&y) {
            continue;
        }
        // Below the row's line: the colour picker, or the quick-action chips.
        if y >= top + height(row) {
            if v.picker.is_some_and(|p| p.row == i) {
                let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| (rx - 4.0..=rx + rw + 4.0).contains(&x) && (ry - 4.0..=ry + rh + 4.0).contains(&y);
                return pick_rects(top).into_iter().position(inside).map(|k| Hit::ColourPick(k as u8));
            }
            if matches!(row.kind, Kind::Chips) {
                let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| (rx..=rx + rw).contains(&x) && (ry..=ry + rh).contains(&y);
                return (0..crate::config::QUICK_NAMES.len()).find(|&k| inside(chip_rect(k, top))).map(|k| Hit::Setting(i, k));
            }
            return None;
        }
        let part = match &row.kind {
            Kind::Header => return None,
            Kind::Chips => None,
            Kind::Toggle | Kind::Startup => ((RIGHT - 44.0..=RIGHT).contains(&x)).then_some(0),
            Kind::Choice(options) => {
                let mut right = RIGHT;
                let mut found = None;
                for (k, (_, label)) in options.iter().enumerate().rev() {
                    let w = choice_w(label);
                    if (right - w..=right).contains(&x) {
                        found = Some(k);
                    }
                    right -= w + 4.0;
                }
                found
            }
            Kind::Scale => {
                if (RIGHT - 26.0..=RIGHT).contains(&x) {
                    Some(1)
                } else if (RIGHT - 110.0..=RIGHT - 84.0).contains(&x) {
                    Some(0)
                } else {
                    None
                }
            }
            Kind::Pet => {
                if (RIGHT - 26.0..=RIGHT).contains(&x) {
                    Some(1)
                } else if (PET_PREV..=PET_PREV + 26.0).contains(&x) {
                    Some(0)
                } else {
                    None
                }
            }
            Kind::Hotkey => ((RIGHT - 160.0..=RIGHT).contains(&x)).then_some(0),
            Kind::Text(_) => ((RIGHT - 240.0..=RIGHT).contains(&x)).then_some(0),
            Kind::OpenFile => ((RIGHT - 150.0..=RIGHT).contains(&x)).then_some(0),
            Kind::Note(_) => None,
            Kind::Login(_) => ((RIGHT - LOGIN_W..=RIGHT).contains(&x)).then_some(0),
            Kind::Model => {
                if (RIGHT - 26.0..=RIGHT).contains(&x) {
                    Some(1)
                } else if (MODEL_PREV..=MODEL_PREV + 26.0).contains(&x) {
                    Some(0)
                } else {
                    None
                }
            }
            Kind::Guide => ((RIGHT - GUIDE_W..=RIGHT).contains(&x)).then_some(0),
            // The colours, then (one more) the circle that opens the picker.
            Kind::Swatch(list) => (0..=list.len()).find(|&k| (x - swatch_x(k, list.len() + 1)).abs() <= SWATCH_STEP * 0.5),
        };
        return part.map(|part| Hit::Setting(i, part));
    }
    None
}

/// Where the control of `Hit::Setting(row, part)` is drawn (for the hover light).
pub fn glow_shape(v: &View, hit: Hit) -> Option<crate::ui::Glow> {
    use crate::ui::Glow;
    let Hit::Setting(row, part) = hit else { return None };
    let (_, top) = tops(v).find(|(i, _)| *i == row)?;
    let cy = top + height(&ROWS[row]) * 0.5;
    if !(TOP..BOTTOM).contains(&cy) {
        return None;
    }
    Some(match &ROWS[row].kind {
        Kind::Header => return None,
        Kind::Toggle | Kind::Startup => Glow::Rect(RIGHT - 40.0, cy - 11.0, 40.0, 22.0, 11.0),
        Kind::Choice(options) => {
            let mut right = RIGHT;
            for (_, label) in options.iter().skip(part + 1).rev() {
                right -= choice_w(label) + 4.0;
            }
            let w = choice_w(options.get(part)?.1);
            Glow::Rect(right - w, cy - 12.0, w, 24.0, 12.0)
        }
        Kind::Scale => Glow::Circle(if part == 1 { RIGHT - 13.0 } else { RIGHT - 97.0 }, cy, 12.0),
        Kind::Pet => Glow::Circle(if part == 1 { RIGHT - 13.0 } else { PET_PREV + 13.0 }, cy, 12.0),
        Kind::Hotkey => Glow::Rect(RIGHT - 160.0, cy - 13.0, 160.0, 26.0, 8.0),
        Kind::Text(_) => Glow::Rect(RIGHT - 240.0, cy - 13.0, 240.0, 26.0, 8.0),
        Kind::OpenFile => Glow::Rect(RIGHT - 150.0, cy - 13.0, 150.0, 26.0, 13.0),
        Kind::Note(_) => return None,
        Kind::Login(_) => Glow::Rect(RIGHT - LOGIN_W, cy - 13.0, LOGIN_W, 26.0, 13.0),
        Kind::Model => Glow::Circle(if part == 1 { RIGHT - 13.0 } else { MODEL_PREV + 13.0 }, cy, 12.0),
        Kind::Guide => Glow::Rect(RIGHT - GUIDE_W, cy - 13.0, GUIDE_W, 26.0, 13.0),
        Kind::Swatch(list) => Glow::Circle(swatch_x(part, list.len() + 1), cy, 11.0),
        Kind::Chips => {
            let (x, y, w, h) = chip_rect(part, top);
            Glow::Rect(x, y, w, h, h * 0.5)
        }
    })
}

pub fn draw(g: &Gfx, v: &View, pal: &Palette, a: f32) {
    let k = |c: Rgba| c.alpha(a);
    g.clip(X0, TOP, WIDTH, BOTTOM - TOP);
    if !v.notice.is_empty() {
        let y = TOP + 4.0 - v.scroll;
        g.fill_rrect(X0, y, WIDTH, 24.0, 12.0, k(theme::accent().alpha(0.18)));
        g.text(&v.notice, Font::Small, X0 + 12.0, y, WIDTH - 24.0, 24.0, k(pal.text), Align::Left);
    }
    for (i, top) in tops(v) {
        let row = &ROWS[i];
        let h = height(row);
        if top + h < TOP || top > BOTTOM {
            continue;
        }
        let label: Vec<u16> = row.label.encode_utf16().collect();
        let value = v.values.get(i).map(String::as_str).unwrap_or("");
        let cy = top + h * 0.5;
        if matches!(row.kind, Kind::Header) {
            g.text(&label, Font::Small, X0 + 4.0, top + 8.0, WIDTH, 18.0, k(pal.text2), Align::Left);
            continue;
        }
        if matches!(row.kind, Kind::Note(_)) {
            g.wrapped(&label, X0 + 14.0, top + 1.0, WIDTH - 28.0, k(pal.text2), &[]);
            continue;
        }
        // The card covers the picker / chips under the row's line too.
        g.fill_rrect(X0, top + 2.0, WIDTH, row_height(v, i) - 4.0, 10.0, k(pal.card));
        g.text(&label, Font::Body, X0 + 12.0, top, WIDTH * 0.55, h, k(pal.text), Align::Left);
        match &row.kind {
            Kind::Header => {}
            Kind::Toggle | Kind::Startup => {
                let lit = if matches!(row.kind, Kind::Startup) { v.startup } else { on(value) };
                let (sx, sw) = (RIGHT - 40.0, 40.0);
                g.fill_rrect(sx, cy - 11.0, sw, 22.0, 11.0, k(if lit { theme::accent() } else { pal.raised }));
                g.fill_circle(if lit { sx + sw - 11.0 } else { sx + 11.0 }, cy, 8.0, k(Rgba::hex(0xffffff)));
            }
            Kind::Chips => {
                // Every quick action as a button: lit = shown on the Home tab.
                let shown = crate::config::parse_quick(value);
                let names = ["Area", "Screen", "Read text", "QR code", "Colour", "Plain text", "Record", "Command", "Voice", "Timer 5m", "Notes", "Lock"];
                for (n, name) in names.iter().enumerate() {
                    let (cx0, cy0, cw, ch) = chip_rect(n, top);
                    let lit = shown.contains(&n);
                    g.fill_rrect(cx0, cy0, cw, ch, ch * 0.5, k(if lit { theme::accent() } else { pal.raised }));
                    let text: Vec<u16> = name.encode_utf16().collect();
                    g.text(&text, Font::Small, cx0, cy0, cw, ch, k(if lit { Rgba::hex(0x0b0b12) } else { pal.text2 }), Align::Center);
                }
                g.text(crate::render::ws(w!("Click to show or hide")), Font::Small, RIGHT - 220.0, top, 220.0, h, k(pal.text2), Align::Right);
            }
            Kind::Swatch(list) => {
                let n_all = list.len() + 1;
                // The last circle: a colour of one's own (rainbow until one is chosen).
                let own = theme::parse_hex(value);
                let x = swatch_x(list.len(), n_all);
                let open = v.picker.is_some_and(|p| p.row == i);
                if own.is_some() || open {
                    g.stroke_circle(x, cy, 11.5, 1.6, k(pal.text));
                }
                match own {
                    Some(c) => g.fill_circle(x, cy, 8.5, k(Rgba::hex(c))),
                    None => g.fill_hgradient(x - 8.5, cy - 8.5, 17.0, 17.0, 8.5, &RAINBOW, a),
                }
                g.line(x - 3.5, cy, x + 3.5, cy, 1.6, k(Rgba::hex(0xffffff)));
                g.line(x, cy - 3.5, x, cy + 3.5, 1.6, k(Rgba::hex(0xffffff)));
                if open {
                    draw_picker(g, v, pal, top, a);
                }
                for (n, (key, _, colour)) in list.iter().enumerate() {
                    let x = swatch_x(n, n_all);
                    let chosen = own.is_none() && (*key == value || (value.is_empty() && n == 0));
                    if chosen {
                        g.stroke_circle(x, cy, 11.5, 1.6, k(pal.text));
                    }
                    if *key == "system" {
                        // Windows' own accent, marked with a dot: it follows the system.
                        g.fill_circle(x, cy, 8.5, k(Rgba::hex(theme::system_accent().unwrap_or(*colour))));
                        g.fill_circle(x, cy, 2.6, k(Rgba::hex(0xffffff)));
                    } else {
                        g.fill_circle(x, cy, 8.5, k(Rgba::hex(*colour)));
                        // Dark shades on a dark card: a thin ring shows where they are.
                        g.stroke_circle(x, cy, 8.5, 1.0, k(pal.text2.alpha(0.45)));
                    }
                    if *key == "music" {
                        // Follows the song's cover: marked with a note.
                        g.text(crate::render::ws(w!("\u{266A}")), Font::Small, x - 8.0, cy - 9.0, 16.0, 18.0, k(Rgba::hex(0xffffff)), Align::Center);
                    }
                }
            }
            Kind::Choice(options) => {
                let mut right = RIGHT;
                for (option, text) in options.iter().rev() {
                    let w = choice_w(text);
                    let chosen = *option == value;
                    g.fill_rrect(right - w, cy - 12.0, w, 24.0, 12.0, k(if chosen { theme::accent() } else { pal.raised }));
                    let text: Vec<u16> = text.encode_utf16().collect();
                    g.text(&text, Font::Small, right - w, cy - 12.0, w, 24.0, k(if chosen { Rgba::hex(0x0b0b12) } else { pal.text }), Align::Center);
                    right -= w + 4.0;
                }
            }
            Kind::Scale => {
                let percent: u32 = value.trim_end_matches('%').trim().parse().unwrap_or(100);
                for (x, sign) in [(RIGHT - 110.0, "\u{2212}"), (RIGHT - 26.0, "+")] {
                    g.fill_circle(x + 13.0, cy, 12.0, k(pal.raised));
                    let s: Vec<u16> = sign.encode_utf16().collect();
                    g.text(&s, Font::Title, x, cy - 12.0, 26.0, 24.0, k(pal.text), Align::Center);
                }
                let s: Vec<u16> = format!("{percent} %").encode_utf16().collect();
                g.text(&s, Font::Label, RIGHT - 84.0, cy - 12.0, 58.0, 24.0, k(pal.text), Align::Center);
            }
            Kind::Pet => {
                for (x, sign) in [(PET_PREV, "\u{2039}"), (RIGHT - 26.0, "\u{203A}")] {
                    g.fill_circle(x + 13.0, cy, 12.0, k(pal.raised));
                    let s: Vec<u16> = sign.encode_utf16().collect();
                    g.text(&s, Font::Title, x, cy - 13.0, 26.0, 24.0, k(pal.text), Align::Center);
                }
                let pet = crate::mascot::find(value);
                let name = pet.map_or("None", |i| crate::mascot::CAST[i].label);
                let s: Vec<u16> = name.encode_utf16().collect();
                g.text(&s, Font::Label, PET_PREV + 60.0, cy - 12.0, RIGHT - 30.0 - (PET_PREV + 60.0), 24.0, k(pal.text), Align::Center);
                if let Some(i) = pet {
                    (crate::mascot::CAST[i].draw)(g, &crate::mascot::Pose::default(), PET_PREV + 44.0, cy + 1.0, a, pal.light);
                }
            }
            Kind::Hotkey => {
                let waiting = v.capturing == Some(i);
                g.fill_rrect(RIGHT - 160.0, cy - 13.0, 160.0, 26.0, 8.0, k(if waiting { theme::accent().alpha(0.3) } else { pal.raised }));
                let text: Vec<u16> = if waiting { "Press the keys\u{2026}".encode_utf16().collect() } else { shortcut_label(value).encode_utf16().collect() };
                g.text(&text, Font::Label, RIGHT - 160.0, cy - 13.0, 160.0, 26.0, k(pal.text), Align::Center);
            }
            Kind::Text(placeholder) => {
                let editing = v.editing == Some(i);
                let (fx, fw) = (RIGHT - 240.0, 240.0);
                g.fill_rrect(fx, cy - 13.0, fw, 26.0, 8.0, k(pal.raised));
                if editing {
                    g.stroke_rrect(fx, cy - 13.0, fw, 26.0, 8.0, 1.2, k(theme::accent()));
                }
                let shown: Vec<u16> = if editing { v.text.clone() } else { value.encode_utf16().collect() };
                if shown.is_empty() && !editing {
                    let p: Vec<u16> = placeholder.encode_utf16().collect();
                    g.text(&p, Font::Body, fx + 10.0, cy - 13.0, fw - 20.0, 26.0, k(pal.text2), Align::Left);
                } else {
                    // The end of a long value, so the caret stays in view.
                    let tail = &shown[shown.len().saturating_sub(30)..];
                    g.text(tail, Font::Body, fx + 10.0, cy - 13.0, fw - 20.0, 26.0, k(pal.text), Align::Left);
                }
                if editing {
                    let caret = fx + 10.0 + g.measure(&shown[shown.len().saturating_sub(30)..], Font::Body) + 1.0;
                    g.line(caret.min(fx + fw - 8.0), cy - 7.0, caret.min(fx + fw - 8.0), cy + 7.0, 1.4, k(theme::accent()));
                }
            }
            Kind::OpenFile => {
                g.fill_rrect(RIGHT - 150.0, cy - 13.0, 150.0, 26.0, 13.0, k(pal.raised));
                g.text(ws(w!("Open settings file")), Font::Label, RIGHT - 150.0, cy - 13.0, 150.0, 26.0, k(pal.text), Align::Center);
            }
            Kind::Note(_) => {}
            Kind::Model => {
                for (x, sign) in [(MODEL_PREV, "\u{2039}"), (RIGHT - 26.0, "\u{203A}")] {
                    g.fill_circle(x + 13.0, cy, 12.0, k(pal.raised));
                    let s: Vec<u16> = sign.encode_utf16().collect();
                    g.text(&s, Font::Title, x, cy - 13.0, 26.0, 24.0, k(pal.text), Align::Center);
                }
                let s: Vec<u16> = v.model_label(value).encode_utf16().take(40).collect();
                g.text(&s, Font::Label, MODEL_PREV + 28.0, cy - 12.0, RIGHT - 28.0 - (MODEL_PREV + 28.0), 24.0, k(pal.text), Align::Center);
            }
            Kind::Login(tool) => {
                use crate::cli::Login;
                let signing = v.signing == Some(*tool);
                let state = v.login(*tool);
                let missing = if *tool == crate::cli::Tool::Claude { "Claude app not found" } else { "Codex app not found" };
                let (status, colour) = match state {
                    None => ("Checking\u{2026}", pal.text2),
                    Some(Login::Missing) => (missing, pal.text2),
                    Some(Login::SignedOut) if signing => ("Finish in the window that opened", pal.text2),
                    Some(Login::SignedOut) => ("Not signed in", pal.text2),
                    Some(Login::SignedIn) => ("Signed in \u{2713}", theme::GREEN),
                };
                let s: Vec<u16> = status.encode_utf16().collect();
                g.text(&s, Font::Small, RIGHT - LOGIN_W - 230.0, cy - 12.0, 220.0, 24.0, k(colour), Align::Right);
                let (button, lit) = match state {
                    Some(Login::Missing) => ("Get the app", false),
                    Some(Login::SignedIn) => ("Check again", false),
                    _ if signing => ("Check", true),
                    _ => ("Sign in", true),
                };
                g.fill_rrect(RIGHT - LOGIN_W, cy - 13.0, LOGIN_W, 26.0, 13.0, k(if lit { theme::accent() } else { pal.raised }));
                let s: Vec<u16> = button.encode_utf16().collect();
                g.text(&s, Font::Label, RIGHT - LOGIN_W, cy - 13.0, LOGIN_W, 26.0, k(if lit { Rgba::hex(0x0b0b12) } else { pal.text }), Align::Center);
            }
            Kind::Guide => {
                g.fill_rrect(RIGHT - GUIDE_W, cy - 13.0, GUIDE_W, 26.0, 13.0, k(theme::accent()));
                g.text(ws(w!("Open the setup guide")), Font::Label, RIGHT - GUIDE_W, cy - 13.0, GUIDE_W, 26.0, k(Rgba::hex(0x0b0b12)), Align::Center);
            }
        }
    }
    g.unclip();
}

// ------------------------------------------------------------------ window --

/// What is under (x, y) dp in the settings window.
pub fn window_hit(v: &View, x: f32, y: f32) -> Option<Hit> {
    if x < SIDE_W {
        if (SEARCH_Y..SEARCH_Y + SEARCH_H).contains(&y) && (14.0..SIDE_W - 14.0).contains(&x) {
            return Some(Hit::SettingsSearch);
        }
        if y >= GROUP_Y && (10.0..SIDE_W - 10.0).contains(&x) {
            let k = ((y - GROUP_Y) / GROUP_H) as usize;
            if k < categories().len() {
                return Some(Hit::SettingsGroup(k));
            }
        }
        return None;
    }
    hit(v, x, y)
}

/// The whole settings window: sidebar (title, search, groups) and the rows.
pub fn draw_window(g: &Gfx, v: &View, pal: &Palette) {
    let ink = pal.text;
    g.fill_rrect(0.0, 0.0, WIN_W_DP, WIN_H_DP, 0.0, pal.island);
    g.fill_rrect(0.0, 0.0, SIDE_W, WIN_H_DP, 0.0, pal.card.alpha(0.55));
    g.text(ws(w!("Settings")), Font::Big, 22.0, 14.0, SIDE_W - 30.0, 40.0, ink, Align::Left);

    // Search field.
    let (sx, sw) = (14.0, SIDE_W - 28.0);
    g.fill_rrect(sx, SEARCH_Y, sw, SEARCH_H, 10.0, pal.raised);
    if v.search_focus {
        g.stroke_rrect(sx, SEARCH_Y, sw, SEARCH_H, 10.0, 1.3, theme::accent());
    }
    let (mx, my) = (sx + 17.0, SEARCH_Y + SEARCH_H * 0.5);
    g.stroke_circle(mx - 1.0, my - 1.0, 5.0, 1.5, pal.text2);
    g.line(mx + 2.6, my + 2.6, mx + 6.0, my + 6.0, 1.7, pal.text2);
    if v.search.is_empty() {
        g.text(ws(w!("Search settings")), Font::Body, sx + 32.0, SEARCH_Y, sw - 40.0, SEARCH_H, pal.text2, Align::Left);
    } else {
        g.text(&v.search, Font::Body, sx + 32.0, SEARCH_Y, sw - 40.0, SEARCH_H, ink, Align::Left);
    }
    if v.search_focus {
        let caret = sx + 32.0 + g.measure(&v.search, Font::Body) + 1.0;
        g.line(caret.min(sx + sw - 8.0), my - 8.0, caret.min(sx + sw - 8.0), my + 8.0, 1.4, theme::accent());
    }

    // Groups.
    let searching = !v.search.is_empty();
    for (k, (_, name)) in categories().iter().enumerate() {
        let y = GROUP_Y + k as f32 * GROUP_H;
        let chosen = k == v.category && !searching;
        let hovered = v.hover == Some(Hit::SettingsGroup(k));
        if chosen || hovered {
            g.fill_rrect(10.0, y + 2.0, SIDE_W - 20.0, GROUP_H - 4.0, 9.0, pal.raised.alpha(if chosen { 1.0 } else { 0.5 }));
        }
        if chosen {
            g.fill_rrect(10.0, y + 9.0, 3.0, GROUP_H - 18.0, 1.5, theme::accent());
        }
        let label: Vec<u16> = name.encode_utf16().collect();
        g.text(&label, Font::Body, 24.0, y, SIDE_W - 40.0, GROUP_H, if chosen { ink } else { pal.text2.mix(ink, 0.35) }, Align::Left);
    }

    // Title over the rows.
    let title: Vec<u16> = if searching {
        let count = v.shown.iter().filter(|&&i| !matches!(ROWS[i].kind, Kind::Header | Kind::Note(_))).count();
        match count {
            0 => "Nothing matches".encode_utf16().collect(),
            1 => "1 setting".encode_utf16().collect(),
            n => format!("{n} settings").encode_utf16().collect(),
        }
    } else {
        categories().get(v.category).map_or("", |c| c.1).encode_utf16().collect()
    };
    g.text(&title, Font::Title, X0 + 4.0, 20.0, WIDTH - 8.0, 32.0, ink, Align::Left);

    draw(g, v, pal, 1.0);

    // The control under the pointer lights up.
    if let Some(shape) = v.hover.filter(|h| matches!(h, Hit::Setting(..))).and_then(|h| glow_shape(v, h)) {
        use crate::ui::Glow;
        match shape {
            Glow::Rect(x, y, w, h, r) => {
                g.fill_rrect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, r + 1.0, ink.alpha(0.07));
                g.stroke_rrect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, r + 1.0, 1.2, theme::accent().alpha(0.65));
            }
            Glow::Circle(x, y, r) => {
                g.fill_circle(x, y, r + 1.0, ink.alpha(0.07));
                g.stroke_circle(x, y, r + 1.0, 1.2, theme::accent().alpha(0.65));
            }
        }
    }

    // A thin scroll bar when the rows do not fit.
    let most = most_scroll(v);
    if most > 0.0 {
        let track = BOTTOM - TOP;
        let bar = (track * track / (track + most)).max(30.0);
        let y = TOP + (track - bar) * (v.scroll / most);
        g.fill_rrect(WIN_W_DP - 10.0, y, 4.0, bar, 2.0, pal.text2.alpha(0.45));
    }
}

// ----------------------------------------------------------------- startup --

const RUN: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// `true` when Islet starts with Windows (a value in the user's Run key).
pub fn starts_with_windows() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, w!("Islet"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

pub fn set_start_with_windows(on: bool) -> bool {
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return false };
            let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN, w!("Islet"), REG_SZ.0, Some(command.as_ptr() as *const _), (command.len() * 2) as u32).is_ok()
        } else {
            let result = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("Islet"));
            result.is_ok() || result == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts() {
        assert_eq!(shortcut_label("alt+shift+d"), "Alt+Shift+D");
        assert_eq!(shortcut_label("ctrl+space"), "Ctrl+Space");
        assert_eq!(shortcut_text(1 | 4, 0x44).as_deref(), Some("alt+shift+d"));
        assert_eq!(shortcut_text(2, 0x71).as_deref(), Some("ctrl+f2"));
        assert_eq!(shortcut_text(1, 0x20).as_deref(), Some("alt+space"));
        // Without a modifier, or with a key the file format cannot name: refused.
        assert_eq!(shortcut_text(0, 0x44), None);
        assert_eq!(shortcut_text(1, 0xBA), None);
        // What is written reads back the same.
        assert_eq!(crate::config::parse_hotkey(&shortcut_text(2 | 8, 0x4B).unwrap()), Some((2 | 8, 0x4B)));
    }

    #[test]
    fn groups_and_search() {
        let groups = categories();
        assert!(groups.len() >= 10 && groups.iter().any(|g| g.1 == "AI chat"));
        // A group shows its own rows only, without its heading.
        let chat = groups.iter().position(|g| g.1 == "AI chat").unwrap();
        let rows = shown_rows(chat, "");
        assert!(rows.iter().all(|&i| category_of(i) == chat && !matches!(ROWS[i].kind, Kind::Header)));
        assert!(rows.iter().any(|&i| ROWS[i].key == "chat_provider"));
        // Searching finds rows in any group, under their headings; notes come along.
        let found = shown_rows(0, "voice");
        assert!(found.iter().any(|&i| ROWS[i].key == "voice_hotkey") && found.iter().any(|&i| ROWS[i].key == "voice"));
        assert!(found.iter().any(|&i| matches!(ROWS[i].kind, Kind::Header) && ROWS[i].label == "Shortcuts"));
        assert!(found.iter().any(|&i| matches!(ROWS[i].kind, Kind::Note(_))));
        assert!(shown_rows(0, "  CITY ").iter().any(|&i| ROWS[i].key == "city"));
        assert!(shown_rows(0, "zzzz").is_empty());
    }

    #[test]
    fn every_row_is_reachable() {
        let view = View { values: vec![String::new(); ROWS.len()], shown: (0..ROWS.len()).collect(), ..Default::default() };
        // Scrolled to each row in turn, its control answers a click.
        for (i, row) in ROWS.iter().enumerate() {
            let top: f32 = (0..i).map(|k| row_height(&view, k)).sum();
            let v = View { scroll: top, ..view_clone(&view) };
            let (x, y) = match row.kind {
                Kind::Scale => (RIGHT - 5.0, TOP + 4.0 + height(row) * 0.5),
                // The quick actions: the first of the buttons under the label.
                Kind::Chips => {
                    let (cx, cy, _, ch) = chip_rect(0, TOP + 4.0);
                    (cx + 10.0, cy + ch * 0.5)
                }
                _ => (RIGHT - 8.0, TOP + 4.0 + height(row) * 0.5),
            };
            let at = hit(&v, x, y);
            // Headings and help notes are not controls: a click there does nothing.
            if matches!(row.kind, Kind::Header | Kind::Note(_)) {
                assert!(at.is_none(), "row {i} should not answer a click");
                continue;
            }
            assert!(matches!(at, Some(Hit::Setting(r, _)) if r == i), "row {i}");
        }
    }

    #[test]
    fn colours_round_trip_and_hex() {
        for c in [0x8b8cf7u32, 0xff0000, 0x00ff00, 0x0000ff, 0x123456, 0xffffff, 0x000000] {
            let (h, s, v) = theme::rgb_to_hsv(c);
            let back = theme::hsv_to_rgb(h, s, v);
            let close = |a: u32, b: u32, shift: u32| ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff) <= 1;
            assert!(close(c, back, 16) && close(c, back, 8) && close(c, back, 0), "{c:06x} -> {back:06x}");
        }
        assert_eq!(theme::parse_hex("#8B8CF7"), Some(0x8b8cf7));
        assert_eq!(theme::parse_hex("8b8cf7"), Some(0x8b8cf7));
        assert_eq!(theme::parse_hex("teal"), None);
        assert_eq!(theme::hex_text(0x0a0b0c), "#0a0b0c");
    }

    fn view_clone(v: &View) -> View {
        View { values: v.values.clone(), shown: v.shown.clone(), ..Default::default() }
    }
}
