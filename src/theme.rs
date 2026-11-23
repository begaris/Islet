//! Dark/light palettes and the Windows theme setting.

use crate::render::Rgba;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering::Relaxed};
use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy, PartialEq)]
pub enum ThemeMode {
    Dark,
    Light,
    System,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub island: Rgba,
    pub card: Rgba,
    pub raised: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub edge: Rgba,
    /// 0 in the dark theme, 1 in the light theme (drives the drop shadow).
    pub light: f32,
}

const DARK: Palette = Palette {
    island: Rgba::hex(0x000000),
    card: Rgba::hex(0x18181a),
    raised: Rgba::hex(0x2c2c2e),
    text: Rgba::hex(0xffffff),
    text2: Rgba::hex(0x8e8e93),
    edge: Rgba::hex(0x3a3a3e),
    light: 0.0,
};

const LIGHT: Palette = Palette {
    island: Rgba::hex(0xf6f6f8),
    card: Rgba::hex(0xe9e9ee),
    raised: Rgba::hex(0xd8d8df),
    text: Rgba::hex(0x111113),
    text2: Rgba::hex(0x6c6c72),
    edge: Rgba::hex(0xcfcfd6),
    light: 1.0,
};

pub const GREEN: Rgba = Rgba::hex(0x34c759);
pub const SUN: Rgba = Rgba::hex(0xffb938);

/// The accent colours to choose from (settings value, label, colour);
/// "system" follows the Windows accent colour.
pub const ACCENTS: [(&str, &str, u32); 9] = [
    ("periwinkle", "Periwinkle", 0x8b8cf7),
    ("blue", "Blue", 0x4f9dff),
    ("teal", "Teal", 0x2ed3b7),
    ("green", "Green", 0x4cd964),
    ("gold", "Gold", 0xf5c542),
    ("orange", "Orange", 0xff9f40),
    ("pink", "Pink", 0xff6fae),
    ("red", "Red", 0xff5a5f),
    ("system", "Windows", 0x8b8cf7),
];

static ACCENT: AtomicU32 = AtomicU32::new(0x8b8cf7);
/// How the island's surface is coloured: 0 plain, 1 tinted with the accent,
/// 2 an accent gradient across the open panel.
static SURFACE: AtomicU8 = AtomicU8::new(0);

/// The accent (buttons, highlights, the line), used until a scene supplies its own.
pub fn accent() -> Rgba {
    Rgba::hex(ACCENT.load(Relaxed))
}

/// Hue (0..1), saturation, brightness -> 0xRRGGBB.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = (h.rem_euclid(1.0)) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |f: f32| ((f + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// 0xRRGGBB -> hue (0..1), saturation, brightness.
pub fn rgb_to_hsv(colour: u32) -> (f32, f32, f32) {
    let (r, g, b) = (((colour >> 16) & 0xff) as f32 / 255.0, ((colour >> 8) & 0xff) as f32 / 255.0, (colour & 0xff) as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, if max <= 0.0 { 0.0 } else { d / max }, max)
}

/// "#8b8cf7" (or "8b8cf7") -> 0x8b8cf7: a colour of one's own in the settings.
pub fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

/// 0x8b8cf7 -> "#8b8cf7".
pub fn hex_text(colour: u32) -> String {
    format!("#{:06x}", colour & 0x00ff_ffff)
}

/// Sets the accent by its settings name (see [`ACCENTS`]) or "#rrggbb".
pub fn set_accent(name: &str) {
    let colour = match name {
        "system" => system_accent().unwrap_or(ACCENTS[0].2),
        _ => parse_hex(name).unwrap_or_else(|| ACCENTS.iter().find(|(key, _, _)| *key == name).map_or(ACCENTS[0].2, |a| a.2)),
    };
    ACCENT.store(colour, Relaxed);
}

/// Windows' accent colour (`DwmGetColorizationColor`), as 0xRRGGBB.
pub fn system_accent() -> Option<u32> {
    let (mut argb, mut opaque) = (0u32, windows::core::BOOL(0));
    unsafe { windows::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut argb, &mut opaque).ok()? };
    Some(argb & 0x00ff_ffff)
}

/// The island's own colour (settings value, label, colour). "black" is the
/// classic look; "music" keeps black and lets the gradient / tinted surface
/// follow the playing song's cover (the old behaviour). The others are deep
/// shades the cards, buttons and gradient are derived from.
pub const ISLAND_COLOURS: [(&str, &str, u32); 9] = [
    ("black", "Black", 0x000000),
    ("graphite", "Graphite", 0x1c1c22),
    ("midnight", "Midnight", 0x101632),
    ("ocean", "Ocean", 0x072c3a),
    ("forest", "Forest", 0x0d2b1e),
    ("plum", "Plum", 0x2b1436),
    ("wine", "Wine", 0x35111f),
    ("mocha", "Mocha", 0x2c1e15),
    ("music", "Follows the music", 0x3a2a1a),
];

/// 0xRRGGBB of the chosen island colour, or `NONE` for black / music.
static ISLAND: AtomicU32 = AtomicU32::new(NONE);
static ISLAND_MUSIC: AtomicU8 = AtomicU8::new(1);
const NONE: u32 = u32::MAX;

/// Sets the island colour by its settings name (see [`ISLAND_COLOURS`]).
pub fn set_island(name: &str) {
    let colour = match name {
        "black" | "music" | "" => NONE,
        _ => parse_hex(name).or_else(|| ISLAND_COLOURS.iter().find(|(key, _, _)| *key == name).map(|c| c.2)).unwrap_or(NONE),
    };
    ISLAND.store(colour, Relaxed);
    ISLAND_MUSIC.store((name == "music" || name.is_empty()) as u8, Relaxed);
}

/// The chosen island colour (`None`: black, or following the music).
pub fn island_colour() -> Option<Rgba> {
    let c = ISLAND.load(Relaxed);
    (c != NONE).then(|| Rgba::hex(c))
}

/// The gradient / tinted surface follows the song's cover.
pub fn island_follows_music() -> bool {
    ISLAND_MUSIC.load(Relaxed) != 0
}

/// The colour a gradient surface uses: a lighter shade of the island colour,
/// else the cover's accent while following the music, else the accent setting.
pub fn surface_colour(music_accent: Rgba) -> Rgba {
    match island_colour() {
        Some(c) => c.mix(Rgba::hex(0xffffff), 0.32),
        None if island_follows_music() => music_accent,
        None => accent(),
    }
}

/// The colour of every card and button in the open panel (settings value,
/// label, colour): "island" = shades of the island (the default), "music" =
/// tinted by the playing song's cover, or one of a few mid-dark colours.
pub const CARD_COLOURS: [(&str, &str, u32); 9] = [
    ("island", "Island shade", 0x1c1c20),
    ("music", "Follows the music", 0x4a3420),
    ("slate", "Slate", 0x2b303b),
    ("blue", "Blue", 0x1e3357),
    ("teal", "Teal", 0x15464a),
    ("green", "Green", 0x1d4430),
    ("purple", "Purple", 0x382a5c),
    ("rose", "Rose", 0x582a40),
    ("amber", "Amber", 0x56411d),
];

/// 0: island shade, 1: music, 2: a fixed colour (`CARD`).
static CARD_MODE: AtomicU8 = AtomicU8::new(0);
static CARD: AtomicU32 = AtomicU32::new(0);
/// The playing song's cover colour while music plays (`NONE` otherwise).
static MUSIC_TINT: AtomicU32 = AtomicU32::new(NONE);

/// Sets the card colour by its settings name (see [`CARD_COLOURS`]).
pub fn set_card(name: &str) {
    let (mode, colour) = match name {
        "music" => (1, 0),
        _ => match parse_hex(name).or_else(|| CARD_COLOURS.iter().skip(2).find(|(key, _, _)| *key == name).map(|c| c.2)) {
            Some(c) => (2, c),
            None => (0, 0),
        },
    };
    CARD_MODE.store(mode, Relaxed);
    CARD.store(colour, Relaxed);
}

/// The cover colour of the song playing now (`None` when nothing plays).
pub fn set_music_tint(colour: Option<Rgba>) {
    let packed = colour.map_or(NONE, |c| ((c.r.clamp(0.0, 1.0) * 255.0) as u32) << 16 | ((c.g.clamp(0.0, 1.0) * 255.0) as u32) << 8 | (c.b.clamp(0.0, 1.0) * 255.0) as u32);
    MUSIC_TINT.store(packed, Relaxed);
}

/// Applies the card colour setting to a palette whose light mix is `t`.
fn card_colour(p: &mut Palette, t: f32) {
    let white = Rgba::hex(0xffffff);
    match CARD_MODE.load(Relaxed) {
        1 => {
            let tint = MUSIC_TINT.load(Relaxed);
            if tint != NONE {
                let c = Rgba::hex(tint);
                let k = 0.24 - 0.12 * t;
                p.card = p.card.mix(c, k);
                p.raised = p.raised.mix(c, k * 0.85);
            }
        }
        2 => {
            let c = Rgba::hex(CARD.load(Relaxed));
            // Dark theme: the colour itself; light theme: a pale wash of it.
            p.card = c.mix(LIGHT.card.mix(c, 0.22), t);
            p.raised = c.mix(white, 0.1).mix(LIGHT.raised.mix(c, 0.22), t);
        }
        _ => {}
    }
}

/// "plain", "tinted" or "gradient".
pub fn set_surface(name: &str) {
    SURFACE.store(
        match name {
            "tinted" => 1,
            "gradient" => 2,
            _ => 0,
        },
        Relaxed,
    );
}

pub fn surface() -> u8 {
    SURFACE.load(Relaxed)
}

/// Blends the two palettes; `t` = 0 is dark, 1 is light. A tinted surface
/// leans the island and its cards towards the accent.
pub fn palette(t: f32) -> Palette {
    // An island colour replaces the black and gives the cards lighter shades
    // of it; in the light theme it only tints the greys a little.
    let white = Rgba::hex(0xffffff);
    let (dark, light) = match island_colour() {
        Some(c) => (
            Palette { island: c, card: c.mix(white, 0.075), raised: c.mix(white, 0.15), edge: c.mix(white, 0.22), ..DARK },
            Palette { island: LIGHT.island.mix(c, 0.08), card: LIGHT.card.mix(c, 0.07), raised: LIGHT.raised.mix(c, 0.07), ..LIGHT },
        ),
        None => (DARK, LIGHT),
    };
    let mut p = Palette {
        island: dark.island.mix(light.island, t),
        card: dark.card.mix(light.card, t),
        raised: dark.raised.mix(light.raised, t),
        text: dark.text.mix(light.text, t),
        text2: dark.text2.mix(light.text2, t),
        edge: dark.edge.mix(light.edge, t),
        light: t,
    };
    if surface() == 1 {
        let accent = accent();
        let k = 0.11 - 0.05 * t;
        p.island = p.island.mix(accent, k);
        p.card = p.card.mix(accent, k * 0.9);
        p.raised = p.raised.mix(accent, k * 0.8);
        p.edge = p.edge.mix(accent, 0.25);
    }
    card_colour(&mut p, t);
    p
}

/// Reads `AppsUseLightTheme` (apps theme) from the registry.
pub fn system_is_light() -> bool {
    let mut value: u32 = 0;
    let mut size = 4u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok() && value != 0
}
