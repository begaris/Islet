//! Minimal settings file (`%APPDATA%\Islet\config.txt`, `key = value` lines).
//! The Settings UI arrives in a later phase; until then this file is the UI.

use crate::theme::ThemeMode;
use std::path::{Path, PathBuf};

pub struct Config {
    pub theme: ThemeMode,
    /// UI scale multiplier on top of the monitor DPI, 0.8 ..= 2.0.
    pub scale: f32,
    pub autohide: bool,
    pub click_through: bool,
    /// Show on the monitor that has the cursor instead of the primary one.
    pub follow_cursor: bool,
    /// Where along the top edge the island sits: -1 left .. 0 middle .. 1 right
    /// (Ctrl + drag sets it).
    pub island_x: f32,
    /// Render with the graphics driver instead of the software rasteriser.
    pub gpu: bool,
    /// Show a popup when Caps Lock is toggled (needs a keyboard hook that
    /// watches that one key) and when the keyboard layout changes.
    pub caps_popup: bool,
    pub layout_popup: bool,
    /// Show Windows notifications from other apps in the pill.
    pub notifications: bool,
    /// Command bar shortcut: `RegisterHotKey` modifier bits and virtual-key code.
    pub hotkey: (u32, u32),
    /// Lower-case process names that suspend the island while in the foreground.
    pub suspend_apps: Vec<String>,
    /// Network features, all off until switched on here.
    pub weather: bool,
    pub city: String,
    pub chat: bool,
    /// "anthropic" or "openai" (any OpenAI-compatible endpoint).
    pub chat_provider: String,
    pub chat_model: String,
    /// Base address for the OpenAI-compatible provider.
    pub chat_url: String,
    /// Currency conversion in the command bar, and the user's own currency.
    pub currency: bool,
    pub currency_home: String,
    /// Voice to text: "off", "local" (Windows speech recognition) or "api"
    /// (OpenAI-compatible transcription), its shortcut, and the API details.
    pub voice: String,
    pub voice_hotkey: (u32, u32),
    pub voice_url: String,
    pub voice_model: String,
    pub voice_language: String,
    /// The character on the pill: "none" or a name from `mascot::CAST`.
    pub character: String,
    /// Lively: it does a little act by itself every few seconds (about 0.5 %
    /// of a core). Calm: only reacts (music, cursor, agents, sleep), so at rest
    /// it costs nearly nothing.
    pub character_lively: bool,
    /// Look for a newer release once a day at `update_url` (off by default).
    pub update_check: bool,
    pub update_url: String,
    /// The music wave's look (`wave_shape`, `wave_music`, `wave_speed`,
    /// `wave_colours`, `wave_glow`).
    pub wave: crate::state::WaveStyle,
    /// Interface sounds and the Weather tab's ambience: switches, volumes, style.
    pub sounds: crate::sound::Settings,
    /// How the island and the panel look and move.
    pub look: Look,
    /// Song lyrics from lrclib.net (network: off by default), and whether the
    /// pill shows the line being sung.
    pub lyrics: bool,
    pub lyrics_pill: bool,
    /// CPU load and GPU temperature in the clock pill.
    pub stats_pill: bool,
}

/// The Home tab's quick actions by their settings names, in `ui::QUICK` order.
pub const QUICK_NAMES: [&str; 12] = ["area", "screen", "text", "qr", "colour", "plain", "record", "command", "voice", "timer", "notes", "lock"];

/// Tabs that can be hidden: (settings key, tab number).
pub const TAB_KEYS: [(&str, usize); 7] = [("tab_clipboard", 1), ("tab_shelf", 2), ("tab_system", 3), ("tab_chat", 4), ("tab_notes", 5), ("tab_weather", 6), ("tab_controls", 7)];

/// Appearance and motion (settings window: Appearance, Pill, Panel).
#[derive(Clone, PartialEq, Debug)]
pub struct Look {
    /// Name from `theme::ACCENTS`.
    pub accent: String,
    /// Bottom corner radius of the pill and of the panel.
    pub corners: (f32, f32),
    /// Width of the resting pill (clock, character, timer) relative to normal.
    pub pill_size: f32,
    /// Springs of the island's size: stiffness and damping.
    pub motion: (f32, f32),
    /// "plain", "tinted" or "gradient" (see `theme::set_surface`).
    pub surface: String,
    /// Droplets splash from the pill when a notification arrives.
    pub splash: bool,
    /// Important alerts make the island's outline pulse.
    pub alert_glow: bool,
    /// Bit `i` set: tab `i` is hidden.
    pub hidden_tabs: u8,
    /// Tab numbers in the order the bar shows them (`tab_order`; Ctrl + drag a tab).
    pub tab_order: [usize; 8],
    /// The island's own colour (`theme::ISLAND_COLOURS`; "music" follows the cover).
    pub island_colour: String,
    /// Colour of the cards and buttons (`theme::CARD_COLOURS`).
    pub card_colour: String,
    /// Item order per `state::Group` (`GROUP_KEYS`), changed with Ctrl + drag.
    pub orders: [Vec<usize>; 7],
    /// Quick actions shown, in order (indexes into `QUICK_NAMES`).
    pub quick: Vec<usize>,
    /// Tab icons bounce, tiles lift and wiggle, the music card leans to the pointer.
    pub lively_panel: bool,
    /// Height of the line pills relative to normal.
    pub pill_height: f32,
    /// Width and height of the open panel relative to normal.
    pub panel: (f32, f32),
    /// Home tab parts shown: music card, volume row, dock.
    pub home: (bool, bool, bool),
    /// Controls tab: bit per tile (Wi-Fi, Bluetooth, VPN, dark theme) and per
    /// power button (lock, sleep, restart, shut down) that is hidden.
    pub hidden_tiles: u8,
    pub hidden_power: u8,
}

impl Default for Look {
    fn default() -> Self {
        Self { accent: "periwinkle".into(), corners: (26.0, 28.0), pill_size: 1.0, motion: (300.0, 26.0), surface: "plain".into(), splash: true, alert_glow: true, hidden_tabs: 0, tab_order: [0, 1, 2, 3, 4, 5, 6, 7], island_colour: "music".into(), card_colour: "island".into(), orders: std::array::from_fn(|g| (0..GROUP_KEYS[g].1.len()).collect()), quick: (0..QUICK_NAMES.len()).collect(), lively_panel: true, pill_height: 1.0, panel: (1.0, 1.0), home: (true, true, true), hidden_tiles: 0, hidden_power: 0 }
    }
}

/// Controls tab items that can be hidden: (settings key, tile or power button, index).
pub const CONTROL_KEYS: [(&str, bool, u8); 8] = [
    ("ctl_wifi", true, 0),
    ("ctl_bluetooth", true, 1),
    ("ctl_vpn", true, 2),
    ("ctl_theme", true, 3),
    ("ctl_lock", false, 0),
    ("ctl_sleep", false, 1),
    ("ctl_restart", false, 2),
    ("ctl_shutdown", false, 3),
];

/// "voice, area, timer" -> their indexes in `QUICK_NAMES` (unknown names and
/// repeats are skipped; nothing usable means all of them).
/// Per `state::Group` (same order as `Group::ALL`): the settings key and the
/// names of its items by id, as the order is written ("home_sections = dock, music, ...").
pub const GROUP_KEYS: [(&str, &[&str]); 7] = [
    ("system_order", &["cpu", "memory", "gpu", "network"]),
    ("controls_tiles", &["wifi", "bluetooth", "vpn", "theme"]),
    ("controls_buttons", &["lock", "sleep", "restart", "shutdown"]),
    ("weather_cards", &["sky", "forecast"]),
    ("weather_details", &["wind", "humidity", "pressure", "uv", "sun", "air"]),
    ("weather_views", &["hourly", "days", "details"]),
    ("home_sections", &["music", "volume", "tiles", "dock"]),
];

/// "gpu, cpu" -> those first, then the rest of `names` in their usual order.
pub fn parse_order(names: &[&str], text: &str) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::with_capacity(names.len());
    for name in text.split(',').map(|n| n.trim().to_lowercase()) {
        if let Some(i) = names.iter().position(|t| *t == name) {
            if !order.contains(&i) {
                order.push(i);
            }
        }
    }
    order.extend((0..names.len()).filter(|i| !order.contains(i)).collect::<Vec<_>>());
    order
}

/// The settings value for an order of `names`.
pub fn order_text(names: &[&str], order: &[usize]) -> String {
    order.iter().filter_map(|&i| names.get(i).copied()).collect::<Vec<_>>().join(", ")
}

/// Names of the tabs by number, as `tab_order` writes them.
pub const TAB_NAMES: [&str; 8] = ["home", "clipboard", "shelf", "system", "chat", "notes", "weather", "controls"];

/// "chat, home, weather" -> those first, then the rest in their usual order.
pub fn parse_tab_order(text: &str) -> [usize; 8] {
    let mut order: Vec<usize> = Vec::with_capacity(8);
    for name in text.split(',').map(|n| n.trim().to_lowercase()) {
        if let Some(i) = TAB_NAMES.iter().position(|t| *t == name) {
            if !order.contains(&i) {
                order.push(i);
            }
        }
    }
    order.extend((0..8).filter(|i| !order.contains(i)).collect::<Vec<_>>());
    std::array::from_fn(|k| order[k])
}

/// The `tab_order` value for an order.
pub fn tab_order_text(order: &[usize; 8]) -> String {
    order.iter().map(|&i| TAB_NAMES[i]).collect::<Vec<_>>().join(", ")
}

pub fn parse_quick(text: &str) -> Vec<usize> {
    if text.trim().eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for word in text.split(',').map(|w| w.trim().to_ascii_lowercase()) {
        let word = match word.as_str() {
            "color" => "colour".to_string(),
            "full" | "fullscreen" => "screen".to_string(),
            "ocr" => "text".to_string(),
            _ => word,
        };
        if let Some(i) = QUICK_NAMES.iter().position(|n| *n == word) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    if out.is_empty() {
        (0..QUICK_NAMES.len()).collect()
    } else {
        out
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { theme: ThemeMode::Dark, scale: 1.0, autohide: false, click_through: false, follow_cursor: false, island_x: 0.0, gpu: false, caps_popup: true, layout_popup: true, notifications: true, hotkey: (1, 0x20), suspend_apps: Vec::new(), weather: false, city: String::new(), chat: false, chat_provider: "anthropic".into(), chat_model: String::new(), chat_url: "https://api.openai.com/v1".into(), currency: false, currency_home: "EUR".into(), voice: "off".into(), voice_hotkey: (1 | 4, 0x44), voice_url: "https://api.openai.com/v1".into(), voice_model: "whisper-1".into(), voice_language: String::new(), character: "none".into(), character_lively: true, update_check: false, update_url: "https://api.github.com/repos/begaris/islet/releases/latest".into(), wave: Default::default(), sounds: Default::default(), look: Default::default(), lyrics: false, lyrics_pill: true, stats_pill: false }
    }
}

const TEMPLATE: &str = "\
# Islet settings. Restart Islet (tray icon > Quit, then start it again) after editing.

# dark | light | system
theme = dark

# UI scale in percent, 80 to 200
scale = 100

# Shrink to a thin strip at the screen edge when idle: on | off
autohide = off

# Make the whole island ignore the mouse: on | off
click_through = off

# Show on the monitor that has the cursor instead of the primary one: on | off
follow_cursor = off

# Where along the top edge the island sits: -1 left, 0 middle, 1 right.
# Hold Ctrl and drag the island to move it.
island_x = 0

# software | gpu
# software: a few MB of memory, slightly more CPU while animating (default).
# gpu: least CPU while animating, but the graphics driver adds about 50 MB of memory.
renderer = software

# Popup when Caps Lock is toggled: on | off
# (uses a keyboard hook that looks at the Caps Lock key only; nothing is recorded)
caps_popup = on

# Shortcut that opens the command bar, e.g. alt+space, ctrl+shift+k, win+f2
hotkey = alt+space

# Show Windows notifications from other apps in the pill: on | off
# (read locally when Windows shows a toast; nothing leaves this PC)
notifications = on

# Popup when the keyboard layout changes: on | off
layout_popup = on

# Process names that put Islet into deep sleep while they are the foreground app,
# comma separated. Fullscreen games and videos are detected automatically.
# Example: suspend_apps = javaw.exe, MyUnityGame.exe
suspend_apps =
";

/// Added to settings files written before the network features existed.
const NETWORK: &str = "
# ---- Network features: everything below is off until you switch it on ----

# Weather from open-meteo.com (no account). Sends the city name once to find
# its coordinates, then only those coordinates: on | off
weather = off
city =

# AI chat: on | off
# claude:    your Claude subscription through the Claude Code that the Claude app
#            installs; no key, sign in once from the settings page
# chatgpt:   your ChatGPT subscription through the Codex that the Codex app
#            installs; no key, sign in once from the settings page
# anthropic: an API key from console.anthropic.com (asked for in the Chat tab,
#            stored encrypted with Windows DPAPI; ANTHROPIC_API_KEY also works)
# openai:    any OpenAI-compatible endpoint, see chat_url (OPENAI_API_KEY also works)
chat = off
# claude | chatgpt | anthropic | openai
chat_provider = anthropic
# Model name; empty uses the provider's default
chat_model =
# Base address for the openai provider
chat_url = https://api.openai.com/v1
";

/// Added to settings files written before currency conversion existed.
const CURRENCY: &str = "
# Currency conversion in the command bar (\"100 usd to eur\"): on | off
# European Central Bank rates from frankfurter.dev (no account), fetched at most once a day.
currency = off
# Your own currency: shown when you type an amount without a target, e.g. \"50 gbp\"
currency_home = EUR
";

/// Added to settings files written before the characters existed.
const CHARACTER: &str = "
# A little character on the left of the pill:
# none | cat | ghost | jellyfish | fox | frog | owl | bunny | robot | axolotl
# Its eyes follow the cursor, it dances to music, thinks while a coding agent works,
# jumps for joy when the agent is done, does a trick now and then and dozes off
# while you are away.
character = none
";

/// Added to settings files written before the character's liveliness setting existed.
const CHARACTER_MOTION: &str = "
# lively: the character does a little act by itself every few seconds
#         (about half a percent of one CPU core)
# calm:   it only reacts (music, the cursor, coding agents, sleep): nearly no CPU at rest
character_motion = lively
";

/// Added to settings files written before the music wave settings existed.
const WAVE: &str = "
# The wave along the bottom edge while music plays.
# Shape: liquid | simple
wave_shape = liquid
# How much the middle swells with the bass: off | soft | normal | strong
wave_music = normal
# slow | normal | fast
wave_speed = normal
# Line colours from the album cover: two | one
wave_colours = two
# Soft glow around the line: on | off
wave_glow = on
";

/// Added to settings files written before sounds existed.
const SOUNDS: &str = "
# Sounds, all made by Islet itself (no sound files). Silent while a game is in front.
# Soft clicks for buttons and tabs, a whoosh when the panel opens, chimes for alerts: on | off
sounds_ui = on
# Rain, wind, thunder, birds and crickets while the Weather tab is open: on | off
sounds_weather = on
# quiet | normal | loud
sounds_volume = normal
";

/// Added to settings files written before the finer sound settings existed.
const SOUNDS_MORE: &str = "
# Volume of the weather sounds: quiet | normal | loud
sounds_weather_volume = normal
# soft | glassy | retro
sounds_style = soft
# Which interface events make a sound: on | off
sounds_clicks = on
sounds_panel = on
sounds_alerts = on
sounds_character = on
";

/// Added to settings files written before the look settings existed.
const LOOK: &str = "
# Accent colour: periwinkle | blue | teal | green | gold | orange | pink | red | system
accent = periwinkle
# Bottom corners of the island: capsule | round | soft | square
corners = round
# Width of the resting pill: compact | normal | wide
pill_size = normal
# How the island grows and shrinks: liquid | bouncy | smooth
motion = liquid
# The island's surface: plain | tinted | gradient
surface = plain
# Droplets splash from the pill when a notification arrives: on | off
toast_splash = on
# The outline pulses for timers, reminders and questions: on | off
alert_glow = on
# Tab icons bounce, tiles lift, the music card leans to the pointer: lively | calm
panel_motion = lively
# Tabs to show: on | off (Home is always there)
tab_clipboard = on
tab_shelf = on
tab_chat = on
tab_notes = on
tab_weather = on
tab_controls = on
# Home tab quick actions in order, comma separated, from:
# area, screen, text, qr, colour, plain, record, command, voice, timer, notes, lock
quick_actions = area, screen, text, qr, colour, plain, record, command, voice, timer, notes, lock
";

/// Added to settings files written before the size and layout settings existed.
const LAYOUT: &str = "
# Height of the pill: short | normal | tall
pill_height = normal
# Size of the open panel: narrow | normal | wide, and short | normal | tall
panel_width = normal
panel_height = normal
# Home tab parts (the rest moves up and grows to fill the space): on | off
home_music = on
home_volume = on
home_dock = on
# Controls tab tiles and buttons: on | off
ctl_wifi = on
ctl_bluetooth = on
ctl_vpn = on
ctl_theme = on
ctl_lock = on
ctl_sleep = on
ctl_restart = on
ctl_shutdown = on
";

/// Added to settings files written before lyrics existed.
const LYRICS: &str = "
# Song lyrics from lrclib.net (free, no account). Sends the playing song's title,
# artist and length to find them: on | off
lyrics = off
# Show the line being sung in the music pill: on | off
lyrics_pill = on
";

/// Added to settings files written before the wave's drops existed.
const WAVE_DROPS: &str = "
# Drops falling from the wave on bass beats: off | subtle | lively
wave_drops = subtle
";

/// Added to settings files written before the update check existed.
/// Added to settings files written before the System tab existed.
/// Added to settings files written before the card colour existed.
const CARDS: &str = "
# Cards and buttons of the open panel: island (shades of the island) | music (the song's
# cover colour while it plays) | slate | blue | teal | green | purple | rose | amber
card_colour = island
";

/// Added to settings files written before the island colour existed.
const ISLAND: &str = "
# The island's colour: black | graphite | midnight | ocean | forest | plum | wine | mocha
# | music (stays black; the gradient and the music card follow the song's cover)
island_colour = music
";

const SYSTEM: &str = "
# System tab (CPU, memory, GPU, network): on | off
tab_system = on
# CPU load and GPU temperature in the pill instead of the weather: on | off
stats_pill = off
";

const UPDATE: &str = "
# Look for a newer Islet once a day: on | off
# update_url: a GitHub \"latest release\" API address
# (https://api.github.com/repos/<owner>/<repo>/releases/latest) or any JSON with
# \"version\" and \"url\". Only that address is contacted; nothing is downloaded.
update_check = off
update_url = https://api.github.com/repos/begaris/islet/releases/latest
";

/// Added to settings files written before voice to text existed.
const VOICE: &str = "
# Voice to text: press the shortcut, speak, and the words are typed where you are.
# off | local | api
#   local: Windows speech recognition, on this PC only (English on this PC)
#   api:   sends the recording to an OpenAI-compatible transcription service
#          (any language). Uses the API key entered in the Chat tab, or none
#          for a server on http://localhost.
voice = off
voice_hotkey = alt+shift+d
voice_url = https://api.openai.com/v1
voice_model = whisper-1
# Language code for api, e.g. lt or en; empty lets the service detect it
voice_language =
";

/// "alt+space", "ctrl+shift+k", "win+f2" -> (modifier bits, virtual key).
pub fn parse_hotkey(text: &str) -> Option<(u32, u32)> {
    let mut modifiers = 0;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part {
            "alt" => modifiers |= 1,
            "ctrl" | "control" => modifiers |= 2,
            "shift" => modifiers |= 4,
            "win" => modifiers |= 8,
            "space" => key = Some(0x20),
            "tab" => key = Some(0x09),
            p if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() => key = Some(p.as_bytes()[0].to_ascii_uppercase() as u32),
            p if p.starts_with('f') => match p[1..].parse::<u32>() {
                Ok(n @ 1..=12) => key = Some(0x70 + n - 1),
                _ => return None,
            },
            _ => return None,
        }
    }
    // A shortcut without a modifier would swallow an ordinary key.
    (modifiers != 0).then_some(())?;
    Some((modifiers, key?))
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Islet").join("config.txt"))
}

/// The settings as written: `key = value` pairs of the lines that are not comments.
pub fn raw(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// `text` with `key`'s line set to `value`: the first line that sets it is
/// replaced (comments and everything else stay), or a line is added at the end.
pub fn with_value(text: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut done = false;
    for line in text.lines() {
        let setting = !line.trim_start().starts_with('#') && line.split_once('=').is_some_and(|(k, _)| k.trim() == key);
        if setting && !done {
            out.push_str(&format!("{key} = {value}"));
            done = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !done {
        out.push_str(&format!("{key} = {value}\n"));
    }
    out
}

/// Changes one setting in the file at `path`.
pub fn set(path: &Path, key: &str, value: &str) -> bool {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, with_value(&text, key, value)).is_ok()
}

pub fn load() -> Config {
    match path() {
        Some(path) => load_from(&path),
        None => Config::default(),
    }
}

/// Reads the settings file at `path` (writing the template first when there is none).
pub fn load_from(path: &Path) -> Config {
    let mut cfg = Config::default();
    let path = path.to_path_buf();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, format!("{TEMPLATE}{LOOK}{LAYOUT}{CHARACTER}{CHARACTER_MOTION}{WAVE}{WAVE_DROPS}{SOUNDS}{SOUNDS_MORE}{NETWORK}{LYRICS}{CURRENCY}{VOICE}{UPDATE}{SYSTEM}{ISLAND}{CARDS}"));
            return cfg;
        }
    };
    // Older files get the (switched-off) newer sections appended once.
    let has = |key: &str| text.lines().any(|line| line.trim_start().starts_with(key));
    let mut added = String::new();
    if !has("weather") {
        added.push_str(NETWORK);
    }
    if !has("currency") {
        added.push_str(CURRENCY);
    }
    if !has("voice") {
        added.push_str(VOICE);
    }
    if !has("character") {
        added.push_str(CHARACTER);
    }
    if !has("update_check") {
        added.push_str(UPDATE);
    }
    if !has("wave_shape") {
        added.push_str(WAVE);
    }
    if !has("character_motion") {
        added.push_str(CHARACTER_MOTION);
    }
    if !has("sounds_ui") {
        added.push_str(SOUNDS);
    }
    if !has("sounds_style") {
        added.push_str(SOUNDS_MORE);
    }
    if !has("accent") {
        added.push_str(LOOK);
    }
    if !has("card_colour") {
        added.push_str(CARDS);
    }
    if !has("island_colour") {
        added.push_str(ISLAND);
    }
    if !has("stats_pill") {
        added.push_str(SYSTEM);
    }
    if !has("pill_height") {
        added.push_str(LAYOUT);
    }
    if !has("wave_drops") {
        added.push_str(WAVE_DROPS);
    }
    if !has("lyrics") {
        added.push_str(LYRICS);
    }
    if !has("island_x") {
        added.push_str("\n# Where along the top edge the island sits: -1 left, 0 middle, 1 right.\n# Hold Ctrl and drag the island to move it.\nisland_x = 0\n");
    }
    if !added.is_empty() {
        let _ = std::fs::write(&path, format!("{}\n{added}", text.trim_end()));
    }
    let on = |v: &str| matches!(v, "on" | "true" | "1" | "yes");
    let volume = |v: &str| match v {
        "quiet" => 0.4,
        "loud" => 1.0,
        _ => 0.7,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let exact = value.trim();
        let value = exact.to_ascii_lowercase();
        match key.trim() {
            "weather" => cfg.weather = on(&value),
            "lyrics" => cfg.lyrics = on(&value),
            "lyrics_pill" => cfg.lyrics_pill = on(&value),
            "stats_pill" => cfg.stats_pill = on(&value),
            "currency" => cfg.currency = on(&value),
            "character" => cfg.character = value.clone(),
            "character_motion" => cfg.character_lively = value != "calm",
            "update_check" => cfg.update_check = on(&value),
            "wave_shape" => cfg.wave.simple = value == "simple",
            "wave_music" => {
                cfg.wave.react = match value.as_str() {
                    "off" => 0,
                    "soft" => 1,
                    "strong" => 3,
                    _ => 2,
                }
            }
            "wave_speed" => {
                cfg.wave.speed = match value.as_str() {
                    "slow" => 0,
                    "fast" => 2,
                    _ => 1,
                }
            }
            "wave_colours" | "wave_colors" => cfg.wave.two_colours = value != "one",
            "wave_glow" => cfg.wave.glow = on(&value),
            "wave_drops" => {
                cfg.wave.drops = match value.as_str() {
                    "off" => 0,
                    "lively" => 2,
                    _ => 1,
                }
            }
            "sounds_ui" => cfg.sounds.ui = on(&value),
            "sounds_weather" => cfg.sounds.weather = on(&value),
            "sounds_volume" => cfg.sounds.ui_volume = volume(&value),
            "sounds_weather_volume" => cfg.sounds.weather_volume = volume(&value),
            "sounds_style" => {
                cfg.sounds.style = match value.as_str() {
                    "glassy" => 1,
                    "retro" => 2,
                    _ => 0,
                }
            }
            "sounds_clicks" => cfg.sounds.clicks = on(&value),
            "sounds_panel" => cfg.sounds.panel = on(&value),
            "sounds_alerts" => cfg.sounds.alerts = on(&value),
            "sounds_character" => cfg.sounds.character = on(&value),
            "accent" if crate::theme::ACCENTS.iter().any(|(k, _, _)| *k == value) || crate::theme::parse_hex(&value).is_some() => cfg.look.accent = value.clone(),
            "corners" => {
                cfg.look.corners = match value.as_str() {
                    "capsule" => (34.0, 30.0),
                    "soft" => (17.0, 20.0),
                    "square" => (9.0, 12.0),
                    _ => (26.0, 28.0),
                }
            }
            "pill_size" => {
                cfg.look.pill_size = match value.as_str() {
                    "compact" => 0.9,
                    "wide" => 1.15,
                    _ => 1.0,
                }
            }
            "motion" => {
                cfg.look.motion = match value.as_str() {
                    "bouncy" => (330.0, 14.0),
                    "smooth" => (230.0, 31.0),
                    _ => (300.0, 26.0),
                }
            }
            "surface" if matches!(value.as_str(), "plain" | "tinted" | "gradient") => cfg.look.surface = value.clone(),
            "toast_splash" => cfg.look.splash = on(&value),
            "alert_glow" => cfg.look.alert_glow = on(&value),
            "panel_motion" => cfg.look.lively_panel = value != "calm",
            "quick_actions" => cfg.look.quick = parse_quick(&value),
            "tab_order" => cfg.look.tab_order = parse_tab_order(&value),
            key if GROUP_KEYS.iter().any(|(k, _)| *k == key) => {
                if let Some(g) = GROUP_KEYS.iter().position(|(k, _)| *k == key) {
                    cfg.look.orders[g] = parse_order(GROUP_KEYS[g].1, &value);
                }
            }
            "island_colour" if crate::theme::ISLAND_COLOURS.iter().any(|(k, _, _)| *k == value) || crate::theme::parse_hex(&value).is_some() => cfg.look.island_colour = value.clone(),
            "card_colour" if crate::theme::CARD_COLOURS.iter().any(|(k, _, _)| *k == value) || crate::theme::parse_hex(&value).is_some() => cfg.look.card_colour = value.clone(),
            "pill_height" => {
                cfg.look.pill_height = match value.as_str() {
                    "short" => 0.85,
                    "tall" => 1.18,
                    _ => 1.0,
                }
            }
            "panel_width" => {
                cfg.look.panel.0 = match value.as_str() {
                    "narrow" => 0.88,
                    "wide" => 1.14,
                    _ => 1.0,
                }
            }
            "panel_height" => {
                cfg.look.panel.1 = match value.as_str() {
                    "short" => 0.9,
                    "tall" => 1.15,
                    _ => 1.0,
                }
            }
            "home_music" => cfg.look.home.0 = on(&value),
            "home_volume" => cfg.look.home.1 = on(&value),
            "home_dock" => cfg.look.home.2 = on(&value),
            key if CONTROL_KEYS.iter().any(|(k, _, _)| *k == key) => {
                if let Some(&(_, tile, index)) = CONTROL_KEYS.iter().find(|(k, _, _)| *k == key) {
                    let mask = if tile { &mut cfg.look.hidden_tiles } else { &mut cfg.look.hidden_power };
                    if on(&value) {
                        *mask &= !(1 << index);
                    } else {
                        *mask |= 1 << index;
                    }
                }
            }
            tab if TAB_KEYS.iter().any(|(k, _)| *k == tab) => {
                let bit = 1u8 << TAB_KEYS.iter().find(|(k, _)| *k == tab).map_or(0, |t| t.1);
                if on(&value) {
                    cfg.look.hidden_tabs &= !bit;
                } else {
                    cfg.look.hidden_tabs |= bit;
                }
            }
            "update_url" => cfg.update_url = exact.to_string(),
            "voice" if matches!(value.as_str(), "off" | "local" | "api") => cfg.voice = value.clone(),
            "voice_hotkey" => {
                if let Some(hotkey) = parse_hotkey(&value) {
                    cfg.voice_hotkey = hotkey;
                }
            }
            "voice_url" if !exact.is_empty() => cfg.voice_url = exact.trim_end_matches('/').to_string(),
            "voice_model" if !exact.is_empty() => cfg.voice_model = exact.to_string(),
            "voice_language" => cfg.voice_language = value.clone(),
            "currency_home" if exact.len() == 3 => cfg.currency_home = exact.to_ascii_uppercase(),
            "city" => cfg.city = exact.to_string(),
            "chat" => cfg.chat = on(&value),
            "chat_provider" => cfg.chat_provider = value.clone(),
            "chat_model" => cfg.chat_model = exact.to_string(),
            "chat_url" if !exact.is_empty() => cfg.chat_url = exact.trim_end_matches('/').to_string(),
            "theme" => {
                cfg.theme = match value.as_str() {
                    "light" => ThemeMode::Light,
                    "system" => ThemeMode::System,
                    _ => ThemeMode::Dark,
                }
            }
            "scale" => {
                if let Ok(p) = value.trim_end_matches('%').trim().parse::<f32>() {
                    cfg.scale = (p / 100.0).clamp(0.8, 2.0);
                }
            }
            "autohide" => cfg.autohide = on(&value),
            "click_through" => cfg.click_through = on(&value),
            "follow_cursor" => cfg.follow_cursor = on(&value),
            "island_x" => {
                if let Ok(at) = value.parse::<f32>() {
                    cfg.island_x = if at.is_finite() { at.clamp(-1.0, 1.0) } else { 0.0 };
                }
            }
            "renderer" => cfg.gpu = value == "gpu",
            "notifications" => cfg.notifications = on(&value),
            "hotkey" => {
                if let Some(hotkey) = parse_hotkey(&value) {
                    cfg.hotkey = hotkey;
                }
            }
            "caps_popup" => cfg.caps_popup = on(&value),
            "layout_popup" => cfg.layout_popup = on(&value),
            "suspend_apps" => {
                cfg.suspend_apps = value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            }
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_one_setting_keeps_the_rest() {
        let text = "# Theme\n# theme = light (an example in a comment)\ntheme = dark\n\nscale = 100\n";
        let changed = with_value(text, "theme", "light");
        assert_eq!(changed, "# Theme\n# theme = light (an example in a comment)\ntheme = light\n\nscale = 100\n");
        assert_eq!(with_value("scale = 100\n", "character", "cat"), "scale = 100\ncharacter = cat\n");
        let path = std::env::temp_dir().join("islet-config-test.txt");
        let _ = std::fs::remove_file(&path);
        // A missing file gets the full template, then the change.
        let cfg = load_from(&path);
        assert_eq!(cfg.character, "none");
        assert!(cfg.character_lively);
        assert_eq!(cfg.island_x, 0.0);
        assert!(set(&path, "island_x", "-0.5"));
        assert_eq!(load_from(&path).island_x, -0.5);
        assert!(set(&path, "island_x", "7"));
        assert_eq!(load_from(&path).island_x, 1.0, "kept on the screen");
        assert!(set(&path, "character", "ghost") && set(&path, "scale", "120") && set(&path, "character_motion", "calm"));
        let cfg = load_from(&path);
        assert_eq!((cfg.character.as_str(), cfg.scale, cfg.character_lively), ("ghost", 1.2, false));
        assert!(raw(&path).contains(&("voice".to_string(), "off".to_string())));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sound_settings() {
        let path = std::env::temp_dir().join("islet-config-sound-test.txt");
        // An older file gets the section appended, switched on at normal volume.
        std::fs::write(&path, "theme = dark\n").unwrap();
        let cfg = load_from(&path);
        assert!(cfg.sounds.ui && cfg.sounds.weather && cfg.sounds.ui_volume == 0.7);
        assert!(std::fs::read_to_string(&path).unwrap().contains("sounds_volume = normal"));
        assert!(set(&path, "sounds_weather", "off") && set(&path, "sounds_volume", "quiet") && set(&path, "sounds_style", "retro") && set(&path, "sounds_panel", "off"));
        let cfg = load_from(&path);
        assert!(cfg.sounds.ui && !cfg.sounds.weather && cfg.sounds.ui_volume == 0.4 && cfg.sounds.style == 2 && !cfg.sounds.panel && cfg.sounds.clicks);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn look_settings() {
        let path = std::env::temp_dir().join("islet-config-look-test.txt");
        std::fs::write(&path, "theme = dark\n").unwrap();
        // An older file gets the section with the defaults.
        assert_eq!(load_from(&path).look, Look::default());
        for (key, value) in [("accent", "teal"), ("corners", "square"), ("pill_size", "wide"), ("motion", "bouncy"), ("surface", "tinted"), ("toast_splash", "off"), ("tab_chat", "off"), ("tab_controls", "off"), ("panel_motion", "calm"), ("quick_actions", "voice, Color, nonsense, voice, lock")] {
            assert!(set(&path, key, value));
        }
        let look = load_from(&path).look;
        assert_eq!((look.accent.as_str(), look.corners, look.pill_size, look.surface.as_str()), ("teal", (9.0, 12.0), 1.15, "tinted"));
        assert_eq!((look.splash, look.alert_glow, look.lively_panel, look.hidden_tabs), (false, true, false, (1 << 4) | (1 << 7)));
        assert_eq!(look.quick, vec![8, 4, 11]);
        assert_eq!(parse_quick(" , junk"), (0..12).collect::<Vec<_>>());
        assert_eq!(parse_tab_order("Chat, home, nonsense, chat"), [4, 0, 1, 2, 3, 5, 6, 7]);
        assert_eq!(parse_tab_order(&tab_order_text(&[7, 6, 5, 4, 3, 2, 1, 0])), [7, 6, 5, 4, 3, 2, 1, 0]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wave_settings() {
        let path = std::env::temp_dir().join("islet-config-wave-test.txt");
        // An older file without the section gets it appended with the defaults.
        std::fs::write(&path, "theme = dark\nweather = off\ncurrency = off\nvoice = off\ncharacter = none\nupdate_check = off\n").unwrap();
        assert_eq!(load_from(&path).wave, crate::state::WaveStyle::default());
        assert!(raw(&path).contains(&("wave_music".to_string(), "normal".to_string())));
        assert!(raw(&path).contains(&("wave_drops".to_string(), "subtle".to_string())));
        for (key, value) in [("wave_shape", "simple"), ("wave_music", "strong"), ("wave_speed", "slow"), ("wave_colours", "one"), ("wave_glow", "off"), ("wave_drops", "lively")] {
            assert!(set(&path, key, value));
        }
        let wave = load_from(&path).wave;
        assert_eq!(wave, crate::state::WaveStyle { simple: true, react: 3, speed: 0, two_colours: false, glow: false, drops: 2 });
        let _ = std::fs::remove_file(&path);
    }
}
