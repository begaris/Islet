//! Drawing and hit-testing of the island's contents. Everything is in dp and
//! drawn procedurally (no bitmaps): pill scenes, the hover panel, the tab bar,
//! icons, plus the demo caption and the debug HUD.

use crate::anim::{clamp01, ease_out_cubic, lerp};
use crate::debug::Stats;
use crate::island::{Shape, N, WAVE_AMP};
use crate::render::{ws, Align, Font, Gfx, IslandPaths, Rect, Rgba, WBuf};
use crate::state::*;
use crate::theme::{self, Palette};
use std::fmt::Write;
use windows::core::{w, Result, PCWSTR};
use windows_numerics::Vector2;

/// Tab labels by tab number. Number 3 was a "Text" tab that was never built and
/// was removed at the user's request (2026-11-15): it is never shown
/// (`tab_shown`), and the other tabs keep their numbers.
pub const TAB_LABELS: [PCWSTR; TAB_COUNT] = [w!("Home"), w!("Clipboard"), w!("Shelf"), w!("System"), w!("Chat"), w!("Notes"), w!("Weather"), w!("Controls")];
/// The Home tab's quick actions (icon tiles, 3 x 4), in grid order.
const QUICK: [PCWSTR; 12] = [
    w!("Area"),
    w!("Screen"),
    w!("Read text"),
    w!("QR code"),
    w!("Colour"),
    w!("Plain text"),
    w!("Record"),
    w!("Command"),
    w!("Voice"),
    w!("Timer 5m"),
    w!("Notes"),
    w!("Lock"),
];

const BAR_Y: f32 = 12.0;
const BAR_H: f32 = 32.0;

/// Left and right edge of the open panel, in panel content units.
fn panel_l(s: &State) -> f32 {
    CX - s.panel_local().0 * 0.5
}

fn panel_r(s: &State) -> f32 {
    CX + s.panel_local().0 * 0.5
}

fn tabs_x(s: &State) -> f32 {
    panel_l(s) + 14.0
}

const BLACK: Rgba = Rgba::hex(0x000000);
const WHITE: Rgba = Rgba::hex(0xffffff);

#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Tab(usize),
    Pin,
    Settings,
    Previous,
    Toggle,
    Next,
    /// The volume slider, with the level at the pointer.
    Volume(f32),
    /// The output-device label (opens the list).
    Device,
    /// A row of the open output-device list.
    Output(usize),
    /// Control centre.
    Radio(crate::control::Kind),
    Vpn,
    /// Opens the settings file (shown where a feature is switched off).
    OpenConfig,
    ChatInput,
    /// Sends the typed message, or stops the answer that is streaming.
    ChatSend,
    ChatNew,
    /// The paperclip: opens the file picker.
    ChatAttach,
    /// Removes pending attachment `i`.
    ChatUnattach(usize),
    /// Copies answer `i` (index into the messages).
    ChatCopy(usize),
    ChatRegenerate,
    /// The settings window's search field, and group `i` in its sidebar.
    SettingsSearch,
    SettingsGroup(usize),
    /// The Chat tab's model chip (next model) and "Open in Claude Code / Codex".
    ChatModel,
    ChatOpen,
    /// A control on the settings page: row, and which part (an option, minus / plus).
    Setting(usize, usize),
    Theme,
    Power(crate::control::Power),
    /// Clipboard tab: the search field, the clear button, a row, a row's pin.
    ClipSearch,
    ClipClear,
    ClipRow(usize),
    ClipPin(usize),
    /// A result row of the command bar.
    CmdRow(usize),
    /// Shelf tab: a row (press and drag it out), its remove button, "Drag all", clear.
    ShelfRow(usize),
    ShelfRemove(usize),
    ShelfAll,
    ShelfClear,
    /// Notes tab: next folder, new folder, new note, a title, delete, the editor.
    NoteFolder,
    NoteNewFolder,
    NoteNew,
    NoteRow(usize),
    NoteDelete,
    NoteText,
    /// Home tab: a pinned dock item, the dock's "+" slot, a quick-action button.
    Dock(usize),
    DockAdd,
    Quick(usize),
    /// Weather tab: a view of the switcher (hours, days, details), a day row, the sky card.
    WxView(usize),
    WxDay(usize),
    WxSky,
    /// The music card's cover (or its lyrics view): flips between the two.
    Lyrics,
    /// The settings window's colour picker: 0 the square, 1 the hue strip, 2 "Done".
    ColourPick(u8),
    /// The music card's progress bar, at this share of the track; shuffle; repeat.
    Seek(f32),
    Shuffle,
    Repeat,
}

/// The outline of a button, for the hover light.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glow {
    /// x, y, width, height, corner radius.
    Rect(f32, f32, f32, f32, f32),
    /// Centre and radius.
    Circle(f32, f32, f32),
}

/// Buttons light up under the pointer; text fields and the slider do not.
pub fn hoverable(hit: Hit) -> bool {
    !matches!(hit, Hit::Volume(_) | Hit::Seek(_) | Hit::ChatInput | Hit::ClipSearch | Hit::NoteText | Hit::WxSky)
}

/// Where button `hit` is drawn.
pub fn glow_shape(s: &State, hit: Hit) -> Option<Glow> {
    let home = || home_layout(s);
    let dock = |slot: usize| {
        let d = home().dock?;
        Some(Glow::Rect(d.x + 6.0 + slot as f32 * DOCK_STEP, d.y + 4.0, 38.0, 38.0, 10.0))
    };
    let transport = |dx: f32| {
        let (cx, cy) = home().transport()?;
        Some(Glow::Circle(cx + dx, cy, 18.0))
    };
    match hit {
        Hit::Tab(i) => {
            let x = tabs_x(s) + tabs_before(s, i);
            Some(Glow::Rect(x, BAR_Y, s.tab_w[i].x, BAR_H, BAR_H * 0.5))
        }
        Hit::Pin => Some(Glow::Circle(pin_x(s) + BAR_H * 0.5, BAR_Y + BAR_H * 0.5, BAR_H * 0.5)),
        Hit::Settings => Some(Glow::Circle(settings_x(s) + BAR_H * 0.5, BAR_Y + BAR_H * 0.5, BAR_H * 0.5)),
        Hit::Previous => transport(-home().transport_gap()),
        Hit::Toggle => transport(0.0),
        Hit::Next => transport(home().transport_gap()),
        Hit::Shuffle => transport(-2.0 * home().transport_gap()),
        Hit::Repeat => transport(2.0 * home().transport_gap()),
        Hit::Device => {
            let v = home().volume?;
            Some(Glow::Rect(v.x + v.w - 104.0, v.y + v.h * 0.5 - 20.0, 104.0, 40.0, 14.0))
        }
        Hit::Output(row) => {
            let (mx, my) = menu_origin(s);
            Some(Glow::Rect(mx + 4.0, my + 4.0 + row as f32 * MENU_ROW, MENU_W - 8.0, MENU_ROW, 8.0))
        }
        Hit::Quick(id) => {
            let slot = s.look.quick.iter().position(|&q| q == id)?;
            let t = home().tile(slot)?;
            Some(Glow::Rect(t.x, t.y, t.w, t.h, 12.0))
        }
        Hit::Dock(slot) => dock(slot),
        Hit::DockAdd => dock(s.dock.len()),
        Hit::Lyrics => {
            let spot = lyrics_spot(s, &home())?;
            Some(Glow::Rect(spot.x, spot.y, spot.w, spot.h, 12.0))
        }
        Hit::Setting(..) => crate::settings::glow_shape(&s.settings, hit),
        // Controls lay themselves out over the whole panel; the command bar is not in it.
        Hit::Radio(_) | Hit::Vpn | Hit::Theme | Hit::Power(_) | Hit::CmdRow(_) => crate::panels::glow_shape(s, hit),
        // The other tabs have a fixed layout, centred in a taller panel.
        _ => crate::panels::glow_shape(s, hit).map(|glow| match glow {
            Glow::Rect(x, y, w, h, r) => Glow::Rect(x, y + s.fixed_dy(), w, h, r),
            Glow::Circle(x, y, r) => Glow::Circle(x, y + s.fixed_dy(), r),
        }),
    }
}

/// The hover light: a soft fill, an accent outline, and the shape growing a
/// little as it lights up.
fn draw_glow(g: &Gfx, pal: &Palette, accent: Rgba, shape: Glow, amount: f32) {
    if amount <= 0.01 {
        return;
    }
    let grow = 1.5 * amount;
    let (fill, line) = (pal.text.alpha(0.07 * amount), accent.alpha(0.65 * amount));
    match shape {
        Glow::Rect(x, y, w, h, r) => {
            g.fill_rrect(x - grow, y - grow, w + grow * 2.0, h + grow * 2.0, r + grow, fill);
            g.stroke_rrect(x - grow, y - grow, w + grow * 2.0, h + grow * 2.0, r + grow, 1.3, line);
        }
        Glow::Circle(cx, cy, r) => {
            g.fill_circle(cx, cy, r + grow, fill);
            g.stroke_circle(cx, cy, r + grow, 1.3, line);
        }
    }
}

/// The lit button and the one fading out (panel and command bar only, once at rest).
fn draw_hover(g: &Gfx, s: &State, pal: &Palette) {
    if !matches!(s.mode, Mode::Expanded | Mode::Command) || (s.mode == Mode::Expanded && s.reveal < REVEAL_MAX) {
        return;
    }
    g.offset(0.0, 0.0);
    for (hit, amount) in [(s.hover_old, s.hover_out.x), (s.hover, s.hover_in.x)] {
        if let Some(shape) = hit.and_then(|hit| glow_shape(s, hit)) {
            draw_glow(g, pal, s.accent, shape, clamp01(amount));
        }
    }
    // A press: a soft disc spreading from the pointer across the button, and fading.
    if let Some(r) = s.ripple {
        let t = clamp01(r.t);
        let spread = ease_out_cubic(t);
        let fade = (1.0 - t) * (1.0 - t);
        match r.shape {
            Glow::Rect(x, y, w, h, _) => {
                let reach = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].iter().map(|(cx, cy)| (cx - r.x).hypot(cy - r.y)).fold(0.0f32, f32::max);
                g.clip(x, y, w, h);
                g.fill_circle(r.x, r.y, lerp(4.0, reach, spread), pal.text.alpha(0.16 * fade));
                g.unclip();
            }
            Glow::Circle(cx, cy, radius) => {
                g.fill_circle(cx, cy, radius * lerp(0.3, 1.0, spread), pal.text.alpha(0.18 * fade));
            }
        }
    }
}

fn pin_x(s: &State) -> f32 {
    panel_r(s) - 14.0 - BAR_H * 2.0 - 4.0
}

fn settings_x(s: &State) -> f32 {
    panel_r(s) - 14.0 - BAR_H
}

/// Horizontal centre of tab `i`'s icon (panel content units).
pub fn tab_center(s: &State, i: usize) -> f32 {
    tabs_x(s) + tabs_before(s, i) + TAB_W * 0.5
}

/// Left edge of tab `i` in the bar (panel content units).
pub fn tab_left(s: &State, i: usize) -> f32 {
    tabs_x(s) + tabs_before(s, i)
}

/// Where tab `i`'s left edge will settle (target widths, not the moving ones):
/// what a tab being dragged is measured against, so a neighbour still sliding
/// after a swap cannot bounce it back.
/// Where the tab bar starts (panel content units).
pub fn tabs_origin(s: &State) -> f32 {
    tabs_x(s)
}

pub fn tab_left_target(s: &State, i: usize) -> f32 {
    tabs_x(s) + s.tab_x[i].target
}

/// Where tab `i` is drawn now, from the bar's start (its position spring).
fn tabs_before(s: &State, i: usize) -> f32 {
    s.tab_x[i].x
}

/// What interactive element of the open panel is under `(x, y)`.
pub fn hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    if s.mode == Mode::Command {
        return crate::panels::command_hit(s, x, y);
    }
    if s.mode != Mode::Expanded {
        return None;
    }
    // The panel's content may be drawn scaled (panel size settings).
    let (x, y) = s.to_panel(x, y);
    if !(BAR_Y..=BAR_Y + BAR_H).contains(&y) {
        let fy = y - s.fixed_dy();
        return match s.tab {
            0 => home_hit(s, x, y),
            1 => crate::panels::clipboard_hit(s, x, fy),
            2 => crate::panels::shelf_hit(s, x, fy),
            4 => crate::panels::chat_hit(s, x, fy),
            5 => crate::panels::notes_hit(s, x, fy),
            6 => crate::panels::weather_hit(s, x, fy),
            7 => crate::panels::controls_hit(s, x, y),
            _ => None,
        };
    }
    // Each tab where it is drawn right now (it may be gliding).
    for i in s.tabs_in_order() {
        let (tx, tw) = (tabs_x(s) + s.tab_x[i].x, s.tab_w[i].x);
        if x >= tx && x < tx + tw {
            return Some(Hit::Tab(i));
        }
    }
    if x >= pin_x(s) && x < pin_x(s) + BAR_H {
        return Some(Hit::Pin);
    }
    if x >= settings_x(s) && x < settings_x(s) + BAR_H {
        return Some(Hit::Settings);
    }
    None
}

/// What the previous frame drew, for damage tracking. The swap chain has two
/// buffers, so each frame must repaint what changed over the last two frames.
#[derive(Clone, Copy)]
pub struct Damage {
    island: Rect,
    band: Rect,
    delta: Rect,
    cursor: Rect,
    cursor_delta: Rect,
    live: Rect,
    /// Where a falling beat drop was drawn last frame and the frame before
    /// (it moves, and the buffer being drawn is two frames old).
    drop: Rect,
    drop_prev: Rect,
}

impl Damage {
    pub const fn new() -> Self {
        Self { island: Rect::EMPTY, band: Rect::EMPTY, delta: Rect::EMPTY, cursor: Rect::EMPTY, cursor_delta: Rect::EMPTY, live: Rect::EMPTY, drop: Rect::EMPTY, drop_prev: Rect::EMPTY }
    }
}

/// Region of pill content that animates on its own (the mascot).
fn live_rect(s: &State, left: f32) -> Rect {
    if s.mode == Mode::Expanded && s.tab == 6 {
        return crate::panels::weather_live_rect(s);
    }
    if s.mode != Mode::Idle {
        return Rect::EMPTY;
    }
    let (x, y) = match mascot_spot(s, left) {
        Some(spot) => spot,
        None => return Rect::EMPTY,
    };
    // The character (drawn `PILL_ZOOM` bigger) with its marks, hearts, a jump, ears and tails.
    Rect::new(x - 34.0, y - 42.0, x + 46.0, y + 28.0)
}

/// Where the character sits in the pill, when it is shown.
pub fn mascot_spot(s: &State, left: f32) -> Option<(f32, f32)> {
    s.mascot?;
    let cy = PILL_H * 0.5 + 1.0 + s.pill_dy();
    match s.pill {
        Pill::Mascot => Some((left + 46.0, cy)),
        Pill::Music => Some((left + 40.0, cy)),
        _ => None,
    }
}

/// How far the music pill's cover and titles move right to make room for the character.
pub const MASCOT_ROOM: f32 = 52.0;

/// Test aid (`--snapshot`, hook wparam 20): every character in a dozen poses,
/// one row each, so their art can be looked at in one picture.
fn draw_gallery(g: &Gfx, pal: &Palette) {
    use crate::mascot::{Act, Eyes, Mark, Pose, CAST};
    g.fill_rrect(0.0, 0.0, WIN_W, WIN_H, 0.0, pal.island);
    let poses = [
        Pose::default(),
        Pose { look: (-1.0, 0.0), ..Default::default() },
        Pose { eyes: Eyes::Happy, ..Default::default() },
        Pose { act: Act::Wiggle, t: 0.12, ..Default::default() },
        Pose { act: Act::Stretch, t: 0.5, ..Default::default() },
        Pose { act: Act::Yawn, t: 0.5, ..Default::default() },
        Pose { act: Act::Jump, t: 0.5, ..Default::default() },
        Pose { act: Act::Twirl, t: 0.35, ..Default::default() },
        Pose { act: Act::Bounce, t: 0.5, side: -1.0, ..Default::default() },
        Pose { act: Act::Special, t: 0.3, ..Default::default() },
        Pose { act: Act::Special, t: 0.62, ..Default::default() },
        Pose { eyes: Eyes::Closed, mark: Mark::Sleep, ..Default::default() },
    ];
    for (row, pet) in CAST.iter().enumerate() {
        for (col, pose) in poses.iter().enumerate() {
            (pet.draw)(g, pose, 30.0 + col as f32 * 56.0, 30.0 + row as f32 * 44.0, 1.0, pal.light);
        }
    }
}

/// The character, if one is chosen, at its place in the pill.
fn draw_mascot(g: &Gfx, s: &State, pal: &Palette, left: f32, alpha: f32) {
    if let (Some(index), Some((x, y))) = (s.mascot, mascot_spot(s, left)) {
        // (The pill's content offset already moves it with the pill's height.)
        let y = y - s.pill_dy();
        // Drawn bigger than its drawing, growing a little more downwards than up.
        g.zoom_begin(x, y + 5.0, crate::mascot::PILL_ZOOM);
        (crate::mascot::CAST[index].draw)(g, &s.pose, x, y, alpha, pal.light);
        g.zoom_end();
    }
}

/// Draws one frame, repainting only what changed since the buffer was last drawn.
pub fn draw(g: &Gfx, s: &State, pts: &mut [Vector2; N], hud: Option<&Stats>, status: &str, last: &mut Damage) -> Result<()> {
    let pal = theme::palette(clamp01(s.theme.x));
    let shape = s.shape();
    let t0 = std::time::Instant::now();
    shape.contour(pts);
    let paths = g.island_paths(&shape, pts)?;
    g.note(0, t0);

    let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
    for p in pts.iter() {
        min_y = min_y.min(p.Y);
        max_y = max_y.max(p.Y);
    }
    if let Some((dist, radius)) = s.droplet() {
        max_y = max_y.max(shape.h + dist + radius);
    }
    // A notification's droplets fly up to about 30 dp below the edge.
    if s.splash_t >= 0.0 {
        max_y = max_y.max(shape.h + 40.0);
    }
    // A drop falling from the music wave gets a small rectangle of its own
    // (the band along the edge would otherwise reach all the way down to it).
    let drop_rect = match s.beat_drop.map(|d| (d, d.shape())) {
        Some((d, (_, _, Some(dist), ..))) => {
            let (r, y) = (BEAT_DROP_R * 1.5 + 5.0, d.base + dist);
            Rect::new(d.x - r, y - r * 1.4, d.x + r, y + r * 1.4)
        }
        _ => Rect::EMPTY,
    };
    let (left, right) = (shape.left(), shape.right());
    // Whole island (with room for the light-theme shadow) vs. just the liquid bottom band.
    let mut island = Rect::new(left - shape.sr - 24.0, 0.0, right + shape.sr + 24.0, max_y + 16.0);
    let mut band = Rect::new(left - 14.0, min_y - 5.0, right + 14.0, max_y + 16.0);
    // A joint with a screen edge (and the room it may wobble into) is part of it.
    let merged = s.merged();
    let joint_bottom = shape.h + MERGE_FLARE * 1.4 + 4.0;
    let reach = shape.r.max(MERGE_FLARE * 1.4 * 2.4) + 4.0;
    if merged[0] > 0.0 || s.merge[0].target > 0.0 {
        let joint = Rect::new(s.edge.0 - 2.0, 0.0, left + reach, joint_bottom);
        island = island.union(joint);
        band = band.union(joint);
    }
    if merged[1] > 0.0 || s.merge[1].target > 0.0 {
        let joint = Rect::new(right - reach, 0.0, s.edge.1 + 2.0, joint_bottom);
        island = island.union(joint);
        band = band.union(joint);
    }
    let full = g.needs_full() || s.gallery || s.full_frame;
    // Only the character moved: nothing but its corner (`live`) is drawn.
    let delta = if s.repaint {
        island.union(last.island)
    } else if s.pet_only {
        Rect::EMPTY
    } else {
        band.union(last.band)
    };
    let cursor = if s.demo_cursor { Rect::new(s.cursor.0 - 9.0, s.cursor.1 - 9.0, s.cursor.0 + 9.0, s.cursor.1 + 9.0) } else { Rect::EMPTY };
    let cursor_delta = cursor.union(last.cursor);
    let live = live_rect(s, left);
    let mut overlay = Rect::EMPTY;
    if !s.caption.is_empty() {
        overlay = overlay.union(CAPTION);
    }
    if hud.is_some() {
        overlay = overlay.union(HUD);
    }
    // The buffer being drawn is two frames old: repaint last frame's damage too.
    // (A drop's rectangle covers where it is and where it was in the last two frames.)
    let drop_delta = drop_rect.union(last.drop).union(last.drop_prev);
    let rects = if full { [g.bounds(), Rect::EMPTY, Rect::EMPTY, Rect::EMPTY] } else { [delta.union(last.delta), cursor_delta.union(last.cursor_delta), live.union(last.live), overlay.union(drop_delta)] };
    *last = Damage { island, band, delta, cursor, cursor_delta, live, drop: drop_rect, drop_prev: last.drop };

    let t0 = std::time::Instant::now();
    g.begin_frame();
    for r in rects.iter().filter(|r| !r.is_empty()) {
        g.begin_pass(*r);
        paint(g, s, &pal, &shape, &paths, hud, status);
        g.end_pass();
    }
    g.note(1, t0);
    g.end_frame(if full { None } else { Some(&rects) })
}

/// Where the lyrics list sits inside the music card (block top `top`):
/// x, first visible y, width, height.
fn lyric_area(c: Area, top: f32) -> (f32, f32, f32, f32) {
    (c.x + 16.0, top + 46.0, c.w - 32.0, 62.0)
}

/// The music card flipped to its lyrics: a small cover with the title and
/// artist, then the lines scrolling past (the one being sung bright and in
/// the middle). The transport row stays under them.
fn draw_card_lyrics(g: &Gfx, s: &State, pal: &Palette, c: Area, top: f32, a: f32) {
    const LINE_H: f32 = 21.0;
    let m = &s.media;
    let x0 = c.x;
    album_art(g, s, x0 + 16.0, top + 10.0, 30.0, 7.0, a);
    g.text(&m.title, Font::Label, x0 + 54.0, top + 9.0, c.w - 70.0, 18.0, pal.text.alpha(a), Align::Left);
    g.text(&m.artist, Font::Small, x0 + 54.0, top + 26.0, c.w - 70.0, 15.0, pal.text2.alpha(a), Align::Left);
    let (lx, ly, lw, lh) = lyric_area(c, top);
    let l = &s.lyric;
    if l.lines.is_empty() {
        g.text(&l.status, Font::Body, lx, ly, lw, lh, pal.text2.alpha(a), Align::Center);
        return;
    }
    g.clip(lx, ly, lw, lh);
    let centre = ly + lh * 0.5;
    let scroll = l.scroll.x;
    let first = (scroll - 2.0).floor().max(0.0) as usize;
    for i in first..(first + 6).min(l.lines.len()) {
        let y = centre + (i as f32 - scroll) * LINE_H;
        let away = ((y - centre).abs() / (lh * 0.5 + 4.0)).min(1.0);
        let current = l.synced && l.index == Some(i);
        let line = &l.lines[i];
        let line: &[u16] = if line.is_empty() { ws(w!("\u{266A}")) } else { line };
        let (font, colour) = if current { (Font::Label, s.accent.mix(pal.text, 0.35)) } else { (Font::Body, pal.text2) };
        g.text(line, font, lx, y - LINE_H * 0.5, lw, LINE_H, colour.alpha(a * (1.0 - away * 0.85)), Align::Center);
    }
    g.unclip();
}

/// A drop hanging from (or falling off) the music wave: island-coloured, with
/// a ring in the line's colours so it reads as part of the line. (While it
/// hangs, the edge's own bulge is the neck; the outline follows it.)
fn draw_beat_drop(g: &Gfx, s: &State, pal: &Palette) {
    let Some(drop) = s.beat_drop else { return };
    let (amp, _, Some(dist), alpha, stretch) = drop.shape() else { return };
    let glow = clamp01(s.glow.x);
    let (rx, ry) = (BEAT_DROP_R / stretch.sqrt(), BEAT_DROP_R * stretch);
    let (cx, cy) = (drop.x, drop.base + dist);
    let ring = pal.edge.mix(s.accent, glow).mix(pal.edge.mix(s.line2(), glow), 0.5);
    g.fill_ellipse(cx, cy, rx, ry, pal.island.alpha(alpha));
    if s.wave_style.glow {
        g.fill_ellipse(cx, cy, rx + 3.0, ry + 3.0, ring.alpha(0.12 * alpha * glow));
        g.fill_ellipse(cx, cy, rx, ry, pal.island.alpha(alpha));
    }
    // The ring (a circle stretched by a pose). While the drop still hangs, only
    // the part below the bulge: above it the neck's own outline carries the line.
    let hanging = amp > 0.01;
    if hanging {
        g.clip(cx - 30.0, drop.base + amp - 0.5, 60.0, 60.0);
    }
    g.pose_begin(cx, cy, 0.0, 0.0, 0.0, rx / BEAT_DROP_R, ry / BEAT_DROP_R);
    g.stroke_circle(cx, cy, BEAT_DROP_R, EDGE_GLOW_W * 0.75, ring.alpha(alpha));
    g.pose_end();
    if hanging {
        g.unclip();
    }
    // A small light on it, like on a real drop.
    g.fill_circle(cx - rx * 0.35, cy - ry * 0.35, 1.1, WHITE.alpha(0.55 * alpha));
}

/// A notification's splash: droplets thrown from the middle of the bottom
/// edge, pulled back by the surface and swallowed again.
fn draw_splash(g: &Gfx, s: &State, pal: &Palette, shape: &Shape) {
    if s.splash_t < 0.0 {
        return;
    }
    let t = s.splash_t;
    let fade = 1.0 - (t / SPLASH_SECS).powi(3);
    for k in 0..7 {
        let u = (k as f32 - 3.0) / 3.0;
        // Middle drops fly furthest; each a little different.
        let jitter = ((k as f32 * 12.9898).sin() * 43_758.547).fract();
        let (vx, vy) = (u * (70.0 + jitter * 40.0), 155.0 - u.abs() * 45.0 + jitter * 25.0);
        let pull = 640.0;
        let dy = vy * t - 0.5 * pull * t * t;
        if dy <= 0.5 && t > 0.05 {
            continue;
        }
        let (x, y) = (CX + u * shape.w * 0.12 + vx * t, shape.h + dy.max(0.0));
        let r = (3.4 - u.abs() * 1.0) * (0.6 + 0.4 * clamp01(dy / 6.0));
        g.fill_circle(x, y, r, pal.island.alpha(fade));
        g.stroke_circle(x, y, r, 1.0, s.accent.alpha(0.55 * fade));
    }
}

type Path = windows::Win32::Graphics::Direct2D::ID2D1PathGeometry1;

/// The liquid joint between the island and a screen edge, for one side
/// (`side` -1 left, 1 right) merged by `m` (0 none, a thin web while it comes
/// close, 1 joined; a little over 1 while it wobbles). Returns the fill, the
/// outline along its bottom (screen edge -> island), and where the island's
/// own outline has to stop (the joint draws that corner).
fn merge_paths(g: &Gfx, shape: &Shape, edge: f32, side: f32, m: f32) -> Option<(Path, Path, f32)> {
    use crate::render::Seg::{Cubic, Line};
    // Built for the left side (island to the right of the edge); the right
    // side is the mirror image around the island's centre.
    let flip = |x: f32| if side < 0.0 { x } else { 2.0 * CX - x };
    let p = |x: f32, y: f32| (flip(x), y);
    let (e, i) = (if side < 0.0 { edge } else { 2.0 * CX - edge }, shape.left());
    let (h, r) = (shape.h, shape.r);
    let fill_to = |bottom: &[crate::render::Seg], corner: bool| {
        let mut segs = bottom.to_vec();
        if corner {
            segs.push(Line(p(i + 0.5, h - r)));
        }
        segs.push(Line(p(i + 0.5, 0.0)));
        g.path(p(e - 1.0, 0.0), &segs, true)
    };
    if i - e < 1.0 {
        // Touching: the island's corner becomes a meniscus that flows down the
        // screen edge, `m` deep (it wobbles in with the spring).
        let flare = MERGE_FLARE * m;
        let end = i + r.max(flare * 2.4);
        let bottom = [Line(p(e, h + flare)), Cubic(p(e, h + flare * 0.3), p(e + (end - e) * 0.42, h), p(end, h))];
        let fill = fill_to(&bottom, true)?;
        let line = g.path(p(e, h + flare), &bottom[1..], false)?;
        return Some((fill, line, flip(end)));
    }
    // Coming close: a web hangs from the top edge between them, filling down
    // and sagging thinner in the middle the further apart they still are.
    let full = m.min(1.0);
    let yb = h * full.powf(0.6);
    let neck = yb * lerp(0.45, 1.0, full);
    let (end, corner) = if yb <= h - r {
        (i + 0.5, false)
    } else {
        let dy = yb - (h - r);
        (i + r - (r * r - dy * dy).max(0.0).sqrt() + 0.5, true)
    };
    let flare = (MERGE_FLARE * 0.5 * m).min((end - e) * 0.3);
    let gap = end - e - flare;
    let bottom = [
        Line(p(e, yb + flare)),
        Cubic(p(e, yb + flare * 0.45), p(e + flare * 0.45, yb), p(e + flare, yb)),
        Cubic(p(e + flare + gap * 0.35, neck), p(end - gap * 0.35, neck), p(end, yb)),
    ];
    let fill = fill_to(&bottom, corner)?;
    let line = g.path(p(e, yb + flare), &bottom[1..], false)?;
    Some((fill, line, flip(i)))
}

/// Width of the accent line along the bottom edge.
pub const EDGE_GLOW_W: f32 = 2.6;

/// Centre of the equalizer in the music pill, and how visible it is (0..1).
pub fn eq_place(s: &State) -> (f32, f32, f32) {
    let right = CX + s.w.x * 0.5;
    let visible = if s.mode == Mode::Idle && s.pill == Pill::Music { clamp01(1.0 - s.past_pill() * 5.0) * clamp01(s.pill_fade.x) * clamp01(s.wave.x) } else { 0.0 };
    // It rises with the rest of the pill's content (see `paint`).
    let rise = (1.0 - ease_out_cubic(clamp01(s.pill_fade.x))) * 7.0;
    (right - 46.0, PILL_H * 0.5 + 1.0 + rise + s.pill_dy(), visible)
}

/// What the wave layers must look like for the current state.
pub fn wave_spec(s: &State) -> crate::comp::WaveSpec {
    let pal = theme::palette(clamp01(s.theme.x));
    let style = s.wave_style;
    crate::comp::WaveSpec { cx: CX, w: s.w.x, h: s.h.x, r: s.r.x, island: pal.island, line: s.accent, line2: s.line2(), line_width: EDGE_GLOW_W, simple: style.simple, speed: s.wave_speed(), glow: style.glow }
}

const CAPTION: Rect =Rect { x0: CX - 170.0, y0: WIN_H - 40.0, x1: CX + 170.0, y1: WIN_H - 12.0 };
const HUD: Rect = Rect { x0: 8.0, y0: WIN_H - 88.0, x1: 244.0, y1: WIN_H - 8.0 };

fn paint(g: &Gfx, s: &State, pal: &Palette, shape: &Shape, paths: &IslandPaths, hud: Option<&Stats>, status: &str) {
    // Soft shadow for the light theme: a few wide, faint strokes under the shape.
    if pal.light > 0.01 {
        g.offset(0.0, 2.5);
        for (width, alpha) in [(16.0, 0.03), (9.0, 0.045), (4.0, 0.06)] {
            g.stroke_path(&paths.fill, width, BLACK.alpha(alpha * pal.light));
        }
        g.offset(0.0, 0.0);
    }

    g.fill_path(&paths.fill, pal.island);
    // Gradient surface: the open panel glows with the accent from the top.
    if theme::surface() == 2 && s.open() > 0.01 {
        let tint = theme::surface_colour(s.accent);
        g.fill_path_vgradient(&paths.fill, 0.0, shape.h, &[(0.0, tint.alpha(0.28)), (0.5, tint.alpha(0.07)), (1.0, tint.alpha(0.0))], clamp01(s.open()));
    }

    if let Some((dist, radius)) = s.droplet() {
        g.fill_circle(CX, shape.h + dist, radius, pal.island);
    }
    draw_splash(g, s, pal, shape);

    // Merged with a screen edge: the liquid joint, and the island's own outline
    // cut where the joint takes over its corner.
    let merged = s.merged();
    let mut joints = Vec::new();
    let (mut clip_l, mut clip_r) = (-1.0e4f32, 1.0e4f32);
    for (k, side) in [(0usize, -1.0f32), (1, 1.0)] {
        if merged[k] > 0.004 {
            let edge = if k == 0 { s.edge.0 } else { s.edge.1 };
            if let Some((fill, line, stop)) = merge_paths(g, shape, edge, side, merged[k]) {
                g.fill_path(&fill, pal.island);
                joints.push(line);
                if k == 0 {
                    clip_l = stop;
                } else {
                    clip_r = stop;
                }
            }
        }
    }
    let clipped = !joints.is_empty();
    if clipped {
        g.clip(clip_l, 0.0, clip_r - clip_l, WIN_H);
    }

    // Edge highlight; turns into the accent line while music plays or a download runs.
    // While compositor layers draw the wave they also draw this line.
    if !s.comp_wave {
        let glow = clamp01(s.glow.x);
        let wave = clamp01(s.wave.x);
        if wave > 0.004 {
            // The music line: thicker and brighter as the middle swells (the
            // compositor layers stretch theirs the same way), two cover colours,
            // and a halo.
            let width = lerp(1.5, EDGE_GLOW_W, glow) * lerp(1.0, crate::comp::line_scale(s.wave_mid_amp()), wave);
            let edge = pal.edge.mix(s.accent, glow);
            let upper = pal.edge.mix(s.line2(), glow);
            let amp = WAVE_AMP * wave;
            let halo = if s.wave_style.glow { glow * wave } else { 0.0 };
            g.stroke_glow(&paths.edge, width, upper, edge, shape.h - amp, shape.h + amp, halo);
        } else {
            g.stroke_path(&paths.edge, lerp(1.5, EDGE_GLOW_W, glow), pal.edge.mix(s.accent, glow));
        }
    }
    if clipped {
        g.unclip();
    }
    draw_beat_drop(g, s, pal);
    // The outline carries on along the joint, down into the screen edge, in
    // the same colours, width and glow as the line it continues.
    let glow = clamp01(s.glow.x);
    let wave = clamp01(s.wave.x);
    for line in &joints {
        if wave > 0.004 {
            let width = lerp(1.5, EDGE_GLOW_W, glow) * lerp(1.0, crate::comp::line_scale(s.wave_mid_amp()), wave);
            let amp = WAVE_AMP * wave;
            let halo = if s.wave_style.glow { glow * wave } else { 0.0 };
            g.stroke_glow(line, width, pal.edge.mix(s.line2(), glow), pal.edge.mix(s.accent, glow), shape.h - amp, shape.h + amp, halo);
        } else {
            g.stroke_path(line, lerp(1.5, EDGE_GLOW_W, glow), pal.edge.mix(s.accent, glow));
        }
    }
    // An important alert: the outline breathes in the alert's colour.
    if s.alert_t > 0.0 {
        let since = ALERT_SECS - s.alert_t;
        let envelope = clamp01(since / 0.3) * clamp01(s.alert_t / 0.8);
        let pulse = (0.55 + 0.45 * (since * std::f32::consts::TAU * 1.3).cos()) * envelope;
        let c = s.toast.accent;
        for (width, alpha) in [(11.0, 0.05), (6.0, 0.12), (2.4, 0.6)] {
            g.stroke_path(&paths.fill, width, c.alpha(alpha * pulse));
        }
    }

    g.clip(shape.left(), 0.0, shape.w, shape.h + 48.0);
    // Pill content fades out as the shape grows past the pill's own height
    // (toward the panel) and as it shrinks into the auto-hide strip.
    let pill_alpha = clamp01(1.0 - s.past_pill() * 5.0) * clamp01((s.h.x - 30.0) / 30.0) * clamp01(s.pill_fade.x) * clamp01(1.0 - s.cmd.x * 2.0);
    if pill_alpha > 0.004 {
        // New pill content rises gently into place as it fades in (and stays
        // centred in a taller or shorter pill).
        g.offset(0.0, (1.0 - ease_out_cubic(clamp01(s.pill_fade.x))) * 7.0 + s.pill_dy());
        draw_pill(g, s, pal, shape.left(), shape.right(), pill_alpha);
        g.offset(0.0, 0.0);
    }
    crate::panels::draw_command(g, s, pal);
    if s.reveal > 0.0 {
        // Panel content never leaves the island: it is still fading while the
        // island shrinks, and below the edge nothing would ever erase it.
        g.clip(shape.left(), 0.0, shape.w, (shape.h - 2.0).max(0.0));
        // A bigger or smaller panel draws its content scaled to fit (see `State::panel_scale`).
        let k = s.panel_scale();
        if k != 1.0 {
            g.base_begin(CX, 0.0, k);
        }
        draw_panel(g, s, pal);
        if s.mode == Mode::Expanded {
            draw_hover(g, s, pal);
        }
        if k != 1.0 {
            g.base_end();
        }
        g.unclip();
    }
    if s.mode == Mode::Command {
        draw_hover(g, s, pal);
    }
    g.unclip();
    g.offset(0.0, 0.0);

    if !s.caption.is_empty() {
        g.fill_rrect(CAPTION.x0, CAPTION.y0, 340.0, 28.0, 14.0, BLACK.alpha(0.72));
        g.text(s.caption, Font::Label, CAPTION.x0, CAPTION.y0, 340.0, 28.0, WHITE, Align::Center);
    }
    if s.demo_cursor {
        let (x, y) = s.cursor;
        g.fill_circle(x, y, 6.5, BLACK.alpha(0.55));
        g.fill_circle(x, y, 4.5, WHITE);
    }
    if let Some(stats) = hud {
        draw_hud(g, s, stats, status);
    }
    if s.gallery {
        draw_gallery(g, pal);
    }
}

// ---------------------------------------------------------------- pill ----

/// The pill's system-load texts: ("CPU 12%", "GPU 46°"), or memory in place
/// of the GPU without an NVIDIA card; dashes until the first reading.
fn pill_stats(s: &State) -> (String, String) {
    match &s.stats {
        None => ("CPU \u{2013}".into(), "GPU \u{2013}".into()),
        Some(t) => {
            let cpu = format!("CPU {:.0}%", t.cpu * 100.0);
            let other = match (t.gpu_temp, t.gpu) {
                (Some(c), _) => format!("GPU {c:.0}\u{00B0}"),
                (None, Some(load)) => format!("GPU {:.0}%", load * 100.0),
                _ => format!("RAM {:.0}%", t.mem() * 100.0),
            };
            (cpu, other)
        }
    }
}

/// A short string as UTF-16 for `Gfx::text` (stack buffer).
fn b_slice(text: &str) -> WBuf {
    let mut b = WBuf::new();
    let _ = write!(b, "{text}");
    b
}

fn draw_pill(g: &Gfx, s: &State, pal: &Palette, left: f32, right: f32, alpha: f32) {
    let text = pal.text.alpha(alpha);
    let text2 = pal.text2.alpha(alpha);
    let cy = PILL_H * 0.5 + 1.0;
    if s.volume_shown && !matches!(s.pill, Pill::Permit) {
        // A wheel turn over the pill: speaker, level bar, percentage.
        speaker_icon(g, left + 30.0, cy, text);
        let (x0, x1) = (left + 52.0, right - 70.0);
        g.line(x0, cy, x1, cy, 4.0, pal.raised.alpha(alpha));
        g.line(x0, cy, lerp(x0, x1, clamp01(s.volume)), cy, 4.0, pal.text.alpha(alpha));
        let pct = format!("{:.0}%", s.volume * 100.0);
        g.text(b_slice(&pct).as_slice(), Font::Label, right - 64.0, 0.0, 48.0, PILL_H + 2.0, text, Align::Right);
        return;
    }
    match s.pill {
        Pill::Clock if s.stats_pill => {
            // CPU 12% ...... 22:00 ...... GPU 46°
            clock(g, s, CX - 70.0, 140.0, text, Align::Center);
            let (cpu, gpu) = pill_stats(s);
            g.text(b_slice(&cpu).as_slice(), Font::Label, left + 22.0, 0.0, 120.0, PILL_H + 2.0, text, Align::Left);
            g.text(b_slice(&gpu).as_slice(), Font::Label, right - 142.0, 0.0, 120.0, PILL_H + 2.0, text, Align::Right);
        }
        Pill::Clock => {
            clock(g, s, CX - 70.0, 140.0, text, Align::Center);
            match &s.weather {
                Some(weather) => {
                    sky_icon(g, weather.sky, weather.day, left + 50.0, cy, 1.0, alpha);
                    let mut b = WBuf::new();
                    let _ = write!(b, "{:.0}\u{00B0}", weather.temp);
                    g.text(b.as_slice(), Font::Clock, right - 104.0, 0.0, 100.0, PILL_H + 2.0, text, Align::Center);
                }
                None => {
                    sun(g, left + 50.0, cy, 1.0, theme::SUN.alpha(alpha));
                    g.text(ws(w!("--\u{00B0}")), Font::Clock, right - 104.0, 0.0, 100.0, PILL_H + 2.0, text, Align::Center);
                }
            }
        }
        Pill::Mascot => {
            // [character] ........ 22:00 ☀ 18°
            draw_mascot(g, s, pal, left, alpha);
            clock(g, s, right - 232.0, 100.0, text, Align::Right);
            if s.stats_pill {
                let (cpu, gpu) = pill_stats(s);
                let both = format!("{cpu}  {gpu}");
                g.text(b_slice(&both).as_slice(), Font::Label, right - 124.0, 0.0, 112.0, PILL_H + 2.0, text, Align::Right);
            } else {
                let mut b = WBuf::new();
                match &s.weather {
                    Some(weather) => {
                        sky_icon(g, weather.sky, weather.day, right - 100.0, cy, 0.9, alpha);
                        let _ = write!(b, "{:.0}\u{00B0}", weather.temp);
                    }
                    None => {
                        sun(g, right - 100.0, cy, 0.9, theme::SUN.alpha(alpha));
                        let _ = write!(b, "--\u{00B0}");
                    }
                }
                g.text(b.as_slice(), Font::Clock, right - 78.0, 0.0, 66.0, PILL_H + 2.0, text, Align::Left);
            }
        }
        Pill::Music => {
            // With a character it sits first and the cover moves right to make room.
            let shift = if s.mascot.is_some() { MASCOT_ROOM } else { 0.0 };
            draw_mascot(g, s, pal, left, alpha);
            album_art(g, s, left + 14.0 + shift, 12.0, 44.0, 10.0, alpha);
            g.text(&s.media.title, Font::Title, left + 72.0 + shift, 13.0, 400.0 - shift, 22.0, text, Align::Left);
            if s.lyric.singing() {
                // The line being sung: the old one slides up and out, the new one rises in.
                let (x, w) = (left + 72.0 + shift, 400.0 - shift);
                let f = ease_out_cubic(clamp01(s.lyric.fade.x));
                g.clip(x, 34.0, w, 23.0);
                let shown = |line: &[u16], dy: f32, a: f32| {
                    let note = ws(w!("\u{266A}"));
                    let (line, colour) = if line.is_empty() { (note, text2) } else { (line, text) };
                    g.text(line, Font::Body, x, 35.0 + dy, w, 20.0, colour.alpha(a), Align::Left);
                };
                if f < 1.0 {
                    shown(&s.lyric.prev, -12.0 * f, 1.0 - f);
                }
                shown(&s.lyric.text, 12.0 * (1.0 - f), f);
                g.unclip();
            } else {
                g.text(&s.media.artist, Font::Body, left + 72.0 + shift, 35.0, 400.0 - shift, 20.0, text2, Align::Left);
            }
            // The equalizer bars are compositor layers (see `comp`), not drawn here.
        }
        Pill::Download => {
            g.fill_circle(left + 36.0, cy, 17.0, pal.raised.alpha(alpha));
            arrow_down(g, left + 36.0, cy, text);
            let d = &s.download;
            let mut name = WBuf::new();
            let _ = write!(name, "{}", d.name);
            g.text(name.as_slice(), Font::Title, left + 66.0, 0.0, 250.0, PILL_H + 2.0, text, Align::Left);
            let mut b = WBuf::new();
            write_size(&mut b, d.bytes);
            let _ = write!(b, " \u{00B7} ");
            write_size(&mut b, d.speed as u64);
            let _ = write!(b, "/s");
            g.text(b.as_slice(), Font::Body, right - 204.0, 0.0, 180.0, PILL_H + 2.0, text2, Align::Right);
            let (x0, x1) = (left + 28.0, right - 28.0);
            g.line(x0, PILL_H - 7.0, x1, PILL_H - 7.0, 2.5, pal.raised.alpha(alpha));
            if d.total > 0 {
                g.line(x0, PILL_H - 7.0, lerp(x0, x1, clamp01(d.bytes as f32 / d.total as f32)), PILL_H - 7.0, 2.5, s.accent.alpha(alpha));
            } else {
                // Browsers don't say how big the file will be: no fraction to show.
                g.line(x0, PILL_H - 7.0, x1, PILL_H - 7.0, 2.5, s.accent.alpha(alpha * 0.4));
            }
        }
        Pill::Done => {
            // Content fades in as the card grows taller than the pill.
            let a = alpha * clamp01((s.h.x - 90.0) / 26.0);
            g.fill_rrect(left + 24.0, 32.0, 60.0, 60.0, 14.0, Rgba::hex(0x7d5cf5).alpha(a));
            g.text(&s.done.ext, Font::Label, left + 24.0, 32.0, 60.0, 60.0, WHITE.alpha(a), Align::Center);
            g.text(ws(w!("Downloaded")), Font::Big, left + 100.0, 36.0, 280.0, 28.0, pal.text.alpha(a), Align::Left);
            let mut b = WBuf::new();
            for unit in s.done.name.iter().take(60) {
                let _ = b.push(*unit);
            }
            let _ = write!(b, " \u{00B7} ");
            write_size(&mut b, s.done.size);
            g.text(b.as_slice(), Font::Body, left + 100.0, 64.0, 310.0, 22.0, pal.text2.alpha(a), Align::Left);
            let (bx, by) = done_button(right);
            g.fill_circle(bx, by, 23.0, pal.raised.alpha(a));
            folder(g, bx, by, pal.text.alpha(a));
        }
        Pill::Listen => {
            // A microphone in an accent circle, then the words so far (their end when long).
            let (mx, ink) = (left + 36.0, theme::accent().alpha(alpha));
            g.fill_circle(mx, cy, 17.0, theme::accent().alpha(alpha * 0.22));
            g.fill_rrect(mx - 4.5, cy - 10.0, 9.0, 14.0, 4.5, ink);
            g.line(mx - 7.5, cy - 1.0, mx - 7.5, cy + 1.0, 1.6, ink);
            g.line(mx + 7.5, cy - 1.0, mx + 7.5, cy + 1.0, 1.6, ink);
            g.line(mx - 7.5, cy + 1.0, mx, cy + 7.5, 1.6, ink);
            g.line(mx + 7.5, cy + 1.0, mx, cy + 7.5, 1.6, ink);
            g.line(mx, cy + 7.5, mx, cy + 11.0, 1.6, ink);
            g.text(&s.voice_text, Font::Title, left + 66.0, 0.0, right - left - 90.0, PILL_H + 2.0, text, Align::Left);
        }
        Pill::Record => {
            // A red dot that blinks with the seconds, "Recording", the time, and a stop button.
            let on = s.record_secs % 2 == 0;
            g.fill_circle(left + 36.0, cy, 15.0, Rgba::hex(0xff453a).alpha(alpha * 0.22));
            g.fill_circle(left + 36.0, cy, 7.5, Rgba::hex(0xff453a).alpha(alpha * if on { 1.0 } else { 0.45 }));
            g.text(ws(w!("Recording")), Font::Title, left + 62.0, 0.0, 130.0, PILL_H + 2.0, text, Align::Left);
            let secs = s.record_secs.max(0);
            let mut b = WBuf::new();
            if secs >= 3600 {
                let _ = write!(b, "{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
            } else {
                let _ = write!(b, "{}:{:02}", secs / 60, secs % 60);
            }
            g.text(b.as_slice(), Font::Clock, right - 176.0, 0.0, 110.0, PILL_H + 2.0, text, Align::Right);
            let (bx, by) = stop_button(right);
            g.fill_circle(bx, by, 20.0, pal.raised.alpha(alpha));
            g.fill_rrect(bx - 6.5, by - 6.5, 13.0, 13.0, 3.0, Rgba::hex(0xff453a).alpha(alpha));
        }
        Pill::Timer => {
            g.fill_circle(left + 36.0, cy, 17.0, pal.raised.alpha(alpha));
            timer_icon(g, left + 36.0, cy, text);
            let label: &[u16] = if s.timer_label.is_empty() { ws(w!("Timer")) } else { &s.timer_label };
            g.text(label, Font::Title, left + 66.0, 0.0, 150.0, PILL_H + 2.0, text, Align::Left);
            let left_secs = s.timer_left.max(0);
            let mut b = WBuf::new();
            if left_secs >= 3600 {
                let _ = write!(b, "{}:{:02}:{:02}", left_secs / 3600, left_secs / 60 % 60, left_secs % 60);
            } else {
                let _ = write!(b, "{}:{:02}", left_secs / 60, left_secs % 60);
            }
            g.text(b.as_slice(), Font::Clock, right - 152.0, 0.0, 128.0, PILL_H + 2.0, text, Align::Right);
        }
        Pill::Permit => {
            // Content fades in as the card grows taller than the pill.
            let a = alpha * clamp01((s.h.x - 110.0) / 36.0);
            g.fill_circle(left + 36.0, 30.0, 15.0, theme::accent().alpha(a * 0.22));
            spark(g, left + 36.0, 30.0, 1.0, theme::accent().alpha(a));
            g.text(&s.permit.title, Font::Title, left + 62.0, 16.0, PERMIT_W - 84.0, 28.0, pal.text.alpha(a), Align::Left);
            g.card(left + 20.0, 52.0, PERMIT_W - 40.0, 44.0, 10.0, pal.card.alpha(a));
            let single = s.permit.lines[1].is_empty();
            for (i, line) in s.permit.lines.iter().enumerate() {
                let y = if single { 52.0 + 12.0 } else { 52.0 + 4.0 + i as f32 * 18.0 };
                g.text(line, Font::Hud, left + 32.0, y, PERMIT_W - 64.0, 20.0, pal.text.alpha(a), Align::Left);
            }
            // A question's options: one row each, ticked ones lit in the accent.
            let p = &s.permit;
            for (i, (label, detail)) in p.options.iter().enumerate() {
                let r = permit_option(left, i);
                let picked = p.picked.get(i).copied().unwrap_or(false);
                let hovered = s.permit_hover == Some(PermitAction::Choose(i));
                let fill = if picked { pal.card.mix(theme::accent(), 0.28) } else if hovered { pal.raised } else { pal.card };
                g.card(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0, 10.0, fill.alpha(a));
                if picked {
                    g.stroke_rrect(r.x0 + 0.5, r.y0 + 0.5, r.x1 - r.x0 - 1.0, r.y1 - r.y0 - 1.0, 10.0, 1.2, theme::accent().alpha(a));
                }
                // A numbered dot (or a tick when picked).
                let (dx, dy) = (r.x0 + 18.0, (r.y0 + r.y1) * 0.5);
                g.fill_circle(dx, dy, 9.0, if picked { theme::accent() } else { pal.raised }.alpha(a));
                if picked {
                    g.line(dx - 4.0, dy, dx - 1.0, dy + 3.0, 1.8, Rgba::hex(0x0b0b12).alpha(a));
                    g.line(dx - 1.0, dy + 3.0, dx + 4.5, dy - 3.5, 1.8, Rgba::hex(0x0b0b12).alpha(a));
                } else {
                    let n = format!("{}", i + 1);
                    g.text(b_slice(&n).as_slice(), Font::Small, dx - 9.0, dy - 9.0, 18.0, 18.0, pal.text.alpha(a), Align::Center);
                }
                g.text(label, Font::Label, r.x0 + 36.0, r.y0, 170.0, r.y1 - r.y0, pal.text.alpha(a), Align::Left);
                g.text(detail, Font::Small, r.x0 + 212.0, r.y0, r.x1 - r.x0 - 222.0, r.y1 - r.y0, pal.text2.alpha(a), Align::Left);
            }
            // The buttons; the last one is the main one.
            let count = p.buttons.len();
            for (i, (label, action)) in p.buttons.iter().enumerate() {
                let r = permit_button(left, p.h.max(PERMIT_H), i, count);
                // The last button is the main one, unless it only hands the question back.
                let main = i + 1 == count && *action != PermitAction::Terminal;
                let hovered = s.permit_hover == Some(*action);
                let fill = if main { theme::accent() } else if hovered { pal.raised.mix(pal.text, 0.08) } else { pal.raised };
                g.fill_rrect(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0, 12.0, fill.alpha(a));
                let ink = if main { Rgba::hex(0x0b0b12) } else { pal.text };
                g.text(label, Font::Label, r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0, ink.alpha(a), Align::Center);
            }
        }
        Pill::Toast => {
            let t = &s.toast;
            g.fill_circle(left + 36.0, cy, 17.0, t.accent.alpha(alpha * 0.22));
            toast_icon(g, t.icon, left + 36.0, cy, t.accent.alpha(alpha));
            if t.text.is_empty() {
                g.text(&t.title, Font::Title, left + 66.0, 0.0, 350.0, PILL_H + 2.0, text, Align::Left);
            } else {
                g.text(&t.title, Font::Title, left + 66.0, 13.0, 350.0, 22.0, text, Align::Left);
                g.text(&t.text, Font::Body, left + 66.0, 35.0, 350.0, 20.0, text2, Align::Left);
            }
        }
    }

    // A coding agent is working: a small spark at the pill's left end.
    if s.agent_working && s.pill != Pill::Permit {
        spark(g, left + 13.0, 13.0, 0.55, theme::accent().alpha(alpha));
    }

    // Microphone (orange) / camera (green) in use: a dot at the pill's right end.
    let mut dot_x = right - 13.0;
    for (on, colour) in [(s.cam, Rgba::hex(0x30d158)), (s.mic, Rgba::hex(0xff9f0a))] {
        if on {
            g.fill_circle(dot_x, 13.0, 3.6, colour.alpha(alpha));
            dot_x -= 11.0;
        }
    }
}

/// Button `i` of `count` along the bottom of a permission card `h` tall.
fn permit_button(left: f32, h: f32, i: usize, count: usize) -> Rect {
    let (x0, x1, gap) = (left + 20.0, left + PERMIT_W - 20.0, 10.0);
    let w = (x1 - x0 - gap * (count.max(1) - 1) as f32) / count.max(1) as f32;
    let bx = x0 + i as f32 * (w + gap);
    Rect::new(bx, h - 50.0, bx + w, h - 16.0)
}

/// Option row `i` of a question on the permission card.
fn permit_option(left: f32, i: usize) -> Rect {
    let y = PERMIT_OPTIONS_Y + i as f32 * PERMIT_ROW;
    Rect::new(left + 20.0, y, left + PERMIT_W - 20.0, y + PERMIT_ROW - 6.0)
}

/// What is under (x, y) on the permission card: an option or a button.
pub fn hit_permit(s: &State, x: f32, y: f32) -> Option<PermitAction> {
    if s.mode != Mode::Idle || s.pill != Pill::Permit {
        return None;
    }
    let left = CX - s.w.x * 0.5;
    let inside = |r: Rect| x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1;
    let p = &s.permit;
    if let Some(i) = (0..p.options.len()).find(|&i| inside(permit_option(left, i))) {
        return Some(PermitAction::Choose(i));
    }
    let count = p.buttons.len();
    p.buttons.iter().enumerate().find(|(i, _)| inside(permit_button(left, p.h.max(PERMIT_H), *i, count))).map(|(_, (_, action))| *action)
}

/// A four-pointed spark: the mark for coding agents.
pub fn spark(g: &Gfx, cx: f32, cy: f32, k: f32, c: Rgba) {
    let (long, short) = (8.0 * k, 2.4 * k);
    for (dx, dy) in [(1.0, 0.0), (0.0, 1.0)] {
        g.line(cx - dx * long, cy - dy * long, cx + dx * long, cy + dy * long, 2.2 * k, c);
    }
    g.fill_circle(cx, cy, short, c);
}

/// Centre of the open-folder button on the "Downloaded" card.
fn done_button(right: f32) -> (f32, f32) {
    (right - 52.0, 62.0)
}

/// `true` when `(x, y)` is on the open-folder button of the "Downloaded" card.
pub fn hit_done_button(s: &State, x: f32, y: f32) -> bool {
    let (bx, by) = done_button(CX + s.w.x * 0.5);
    s.mode == Mode::Idle && s.pill == Pill::Done && (x - bx).hypot(y - by) <= 26.0
}

/// Centre of the stop button on the recording pill.
fn stop_button(right: f32) -> (f32, f32) {
    (right - 40.0, PILL_H * 0.5 + 1.0)
}

/// `true` when `(x, y)` is on the recording pill's stop button.
pub fn hit_stop_button(s: &State, x: f32, y: f32) -> bool {
    let (bx, by) = stop_button(CX + s.w.x * 0.5);
    s.mode == Mode::Idle && s.pill == Pill::Record && (x - bx).hypot(y - by - s.pill_dy()) <= 24.0
}

/// "1.2 GB", "640 MB", "12 KB".
fn write_size(b: &mut WBuf, bytes: u64) {
    const KB: f64 = 1024.0;
    let v = bytes as f64;
    let _ = if v >= KB * KB * KB {
        write!(b, "{:.1} GB", v / (KB * KB * KB))
    } else if v >= KB * KB * 10.0 {
        write!(b, "{:.0} MB", v / (KB * KB))
    } else if v >= KB * KB {
        write!(b, "{:.1} MB", v / (KB * KB))
    } else {
        write!(b, "{:.0} KB", v / KB)
    };
}

fn timer_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_circle(cx, cy + 1.0, 6.5, 1.8, c);
    g.line(cx, cy + 1.0, cx, cy - 2.8, 1.8, c);
    g.line(cx, cy + 1.0, cx + 2.6, cy + 2.4, 1.8, c);
    g.line(cx - 2.2, cy - 8.2, cx + 2.2, cy - 8.2, 1.8, c);
}

fn toast_icon(g: &Gfx, icon: crate::events::Icon, cx: f32, cy: f32, c: Rgba) {
    use crate::events::Icon;
    match icon {
        Icon::Timer => timer_icon(g, cx, cy, c),
        Icon::Bell => {
            g.stroke_rrect(cx - 5.5, cy - 6.5, 11.0, 11.0, 5.0, 1.8, c);
            g.line(cx - 7.5, cy + 4.5, cx + 7.5, cy + 4.5, 1.8, c);
            g.fill_circle(cx, cy + 7.6, 1.6, c);
        }
        Icon::Keyboard => {
            g.stroke_rrect(cx - 8.5, cy - 5.5, 17.0, 11.0, 2.5, 1.6, c);
            for dx in [-4.5, 0.0, 4.5] {
                g.fill_circle(cx + dx, cy - 1.6, 1.0, c);
            }
            g.line(cx - 4.0, cy + 2.4, cx + 4.0, cy + 2.4, 1.6, c);
        }
        Icon::Shield => {
            g.line(cx - 6.0, cy - 5.5, cx + 6.0, cy - 5.5, 1.8, c);
            g.line(cx - 6.0, cy - 5.5, cx - 6.0, cy + 0.5, 1.8, c);
            g.line(cx + 6.0, cy - 5.5, cx + 6.0, cy + 0.5, 1.8, c);
            g.line(cx - 6.0, cy + 0.5, cx, cy + 7.0, 1.8, c);
            g.line(cx + 6.0, cy + 0.5, cx, cy + 7.0, 1.8, c);
        }
        Icon::Spark => spark(g, cx, cy, 1.0, c),
        Icon::Info => {
            g.stroke_circle(cx, cy, 7.0, 1.8, c);
            g.line(cx - 3.2, cy + 0.2, cx - 0.8, cy + 2.8, 1.8, c);
            g.line(cx - 0.8, cy + 2.8, cx + 3.6, cy - 2.6, 1.8, c);
        }
    }
}

fn clock(g: &Gfx, s: &State, x: f32, width: f32, c: Rgba, align: Align) {
    let mut b = WBuf::new();
    let _ = write!(b, "{:02}:{:02}", s.time.0, s.time.1);
    g.text(b.as_slice(), Font::Clock, x, 0.0, width, PILL_H + 2.0, c, align);
}

/// A weather glyph about 22 dp wide at `k` = 1 (sun, moon, clouds, rain, snow, bolt, fog).
pub fn sky_icon(g: &Gfx, sky: crate::weather::Sky, day: bool, cx: f32, cy: f32, k: f32, a: f32) {
    use crate::weather::Sky;
    let cloud_c = Rgba::hex(0xd9dee8).alpha(a);
    let cloud = |x: f32, y: f32, k: f32| {
        g.fill_circle(x - 5.0 * k, y + 1.5 * k, 4.6 * k, cloud_c);
        g.fill_circle(x + 0.5 * k, y - 2.0 * k, 6.2 * k, cloud_c);
        g.fill_circle(x + 6.0 * k, y + 1.8 * k, 4.2 * k, cloud_c);
        g.fill_rrect(x - 9.0 * k, y + 1.0 * k, 19.0 * k, 5.4 * k, 2.7 * k, cloud_c);
    };
    let light = |x: f32, y: f32, k: f32| {
        if day {
            sun(g, x, y, k, theme::SUN.alpha(a));
        } else {
            g.fill_circle(x, y, 7.4 * k, Rgba::hex(0xdfe6ff).alpha(a));
            g.fill_circle(x - 2.4 * k, y - 1.6 * k, 1.8 * k, Rgba::hex(0xb4bfe6).alpha(a));
            g.fill_circle(x + 2.2 * k, y + 2.6 * k, 1.3 * k, Rgba::hex(0xb4bfe6).alpha(a));
        }
    };
    match sky {
        Sky::Clear => light(cx, cy, k),
        Sky::Partly => {
            light(cx + 4.5 * k, cy - 3.5 * k, k * 0.68);
            cloud(cx - 2.0 * k, cy + 2.5 * k, k * 0.82);
        }
        Sky::Cloudy => cloud(cx, cy, k),
        Sky::Fog => {
            for (i, half) in [9.0, 7.0, 9.0].into_iter().enumerate() {
                g.line(cx - half * k, cy - 5.0 * k + i as f32 * 5.0 * k, cx + half * k, cy - 5.0 * k + i as f32 * 5.0 * k, 2.0 * k, cloud_c);
            }
        }
        Sky::Rain | Sky::Snow | Sky::Storm => {
            cloud(cx, cy - 3.5 * k, k * 0.9);
            for i in 0..3 {
                let x = cx - 5.0 * k + i as f32 * 5.0 * k;
                match sky {
                    Sky::Rain => g.line(x + 1.0 * k, cy + 4.5 * k, x - 1.0 * k, cy + 9.0 * k, 1.6 * k, Rgba::hex(0x64b5ff).alpha(a)),
                    Sky::Snow => g.fill_circle(x, cy + 7.0 * k, 1.5 * k, Rgba::hex(0xffffff).alpha(a)),
                    _ => {}
                }
            }
            if sky == Sky::Storm {
                let bolt = theme::SUN.alpha(a);
                g.line(cx + 1.5 * k, cy + 3.0 * k, cx - 2.0 * k, cy + 7.0 * k, 1.8 * k, bolt);
                g.line(cx - 2.0 * k, cy + 7.0 * k, cx + 1.5 * k, cy + 7.0 * k, 1.8 * k, bolt);
                g.line(cx + 1.5 * k, cy + 7.0 * k, cx - 1.5 * k, cy + 11.0 * k, 1.8 * k, bolt);
            }
        }
    }
}

fn sun(g: &Gfx, cx: f32, cy: f32, k: f32, c: Rgba) {
    g.fill_circle(cx, cy, 7.6 * k, c);
    for i in 0..8 {
        let (sin, cos) = (i as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
        g.line(cx + cos * 11.6 * k, cy + sin * 11.6 * k, cx + cos * 15.4 * k, cy + sin * 15.4 * k, 2.4 * k, c);
    }
}

/// The cover, or an accent tile with a disc when the player supplies none.
fn album_art(g: &Gfx, s: &State, x: f32, y: f32, size: f32, r: f32, alpha: f32) {
    if g.art(x, y, size, r, alpha) {
        return;
    }
    g.fill_rrect(x, y, size, size, r, s.accent.alpha(alpha));
    g.fill_circle(x + size * 0.5, y + size * 0.5, size * 0.27, BLACK.alpha(0.28 * alpha));
    g.fill_circle(x + size * 0.5, y + size * 0.5, size * 0.08, WHITE.alpha(0.85 * alpha));
}

fn arrow_down(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.line(cx, cy - 6.0, cx, cy + 5.0, 2.0, c);
    g.line(cx - 4.5, cy + 0.8, cx, cy + 5.4, 2.0, c);
    g.line(cx + 4.5, cy + 0.8, cx, cy + 5.4, 2.0, c);
}

fn folder(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_rrect(cx - 9.0, cy - 5.0, 18.0, 12.5, 2.5, 1.8, c);
    g.line(cx - 9.0, cy - 5.0, cx - 7.0, cy - 8.0, 1.8, c);
    g.line(cx - 7.0, cy - 8.0, cx - 1.5, cy - 8.0, 1.8, c);
    g.line(cx - 1.5, cy - 8.0, cx + 0.5, cy - 5.0, 1.8, c);
}

// --------------------------------------------------------------- panel ----

/// Opacity and slide offset for stagger group `i` of the panel reveal.
fn group(s: &State, i: usize) -> (f32, f32) {
    let a = ease_out_cubic((s.reveal - i as f32 * 0.085) / 0.26);
    (a, (1.0 - a) * 9.0)
}

fn draw_panel(g: &Gfx, s: &State, pal: &Palette) {
    // Group 0: top bar.
    let (a, dy) = group(s, 0);
    g.offset(0.0, dy);
    // A tab moved with Ctrl + drag follows the pointer; the others make room.
    // Every tab is drawn where its position spring is, except the one held
    // with Ctrl + drag, which follows the pointer.
    let x_of = |i: usize| s.tab_drag.filter(|(d, _)| *d == i).map_or(tabs_x(s) + s.tab_x[i].x, |(_, x)| x);
    let held = s.tab_held;
    let lift = clamp01(s.tab_lift.x);
    // The active tab's highlight goes with it (the held one draws its own).
    if held != Some(s.tab) {
        g.fill_rrect(x_of(s.tab), BAR_Y, s.hl_w.x, BAR_H, BAR_H * 0.5, pal.raised.alpha(a));
    }
    // A fixed array (no allocation per frame), the held tab moved to the end so
    // it is drawn over the tabs it passes.
    let mut placed = [(0usize, 0.0f32, 0.0f32); TAB_COUNT];
    let mut count = 0;
    for i in s.tabs_in_order() {
        placed[count] = (i, x_of(i), s.tab_w[i].x);
        count += 1;
    }
    if let Some(k) = placed[..count].iter().position(|(i, _, _)| held == Some(*i)) {
        placed[k..count].rotate_left(1);
    }
    for &(i, tx, tw) in &placed[..count] {
        let label = &TAB_LABELS[i];
        let lifted = held == Some(i);
        if lifted {
            // Lifted: a little bigger, with its own background over whatever it
            // passes, and an outline that fades in and out with the lift.
            let k = 1.0 + 0.08 * lift;
            g.pose_begin(tx + tw * 0.5, BAR_Y + BAR_H * 0.5, 0.0, 0.0, 0.0, k, k);
            let fill = if i == s.tab { 1.0 } else { lift };
            g.fill_rrect(tx, BAR_Y, tw, BAR_H, BAR_H * 0.5, pal.raised.alpha(a * fill));
            g.stroke_rrect(tx, BAR_Y, tw, BAR_H, BAR_H * 0.5, 1.4, s.accent.alpha(a * lift));
        }
        let label_w = s.tab_label_w[i] + 8.0;
        let openness = clamp01((tw - TAB_W) / label_w);
        let hovered = s.hover_tab == Some(i);
        let tint = if hovered { 1.0 } else { openness };
        let c = pal.text2.mix(pal.text, tint).alpha(a);
        // A picked tab's icon hops and swells once.
        let (icx, icy) = (tx + TAB_W * 0.5, BAR_Y + BAR_H * 0.5);
        // (Poses do not nest: a lifted tab skips the hop.)
        if s.tab_pop.0 == i && s.tab_pop.1 < BOUNCE_SECS && !lifted {
            let u = s.tab_pop.1 / BOUNCE_SECS;
            let hop = (u * std::f32::consts::PI).sin() * (1.0 - u);
            let squash = (u * std::f32::consts::PI * 3.0).sin() * (1.0 - u) * 0.12;
            g.pose_begin(icx, icy + 7.0, 0.0, -5.0 * hop, 0.0, 1.0 + 0.3 * hop - squash, 1.0 + 0.3 * hop + squash);
            tab_icon(g, i, icx, icy, c);
            g.pose_end();
        } else {
            tab_icon(g, i, icx, icy, c);
        }
        if openness > 0.02 {
            // Fixed box (not the animated width) so the shaped text stays cached;
            // cut to the tab's animated width and faded early, so a name that is
            // leaving never runs into the next tab.
            g.clip(tx, BAR_Y, tw, BAR_H);
            g.text(ws(*label), Font::Label, tx + TAB_W - 3.0, BAR_Y - 0.5, label_w, BAR_H, pal.text.alpha(a * openness * openness), Align::Left);
            g.unclip();
        }
        if lifted {
            g.pose_end();
        }
    }
    let pin = clamp01(s.pin.x);
    let (px, sx, by) = (pin_x(s) + BAR_H * 0.5, settings_x(s) + BAR_H * 0.5, BAR_Y + BAR_H * 0.5);
    g.fill_circle(px, by, BAR_H * 0.5, pal.raised.alpha(a * pin));
    pin_icon(g, px, by, pal.text2.mix(s.accent, pin).alpha(a));
    if s.settings.open {
        g.fill_circle(sx, by, BAR_H * 0.5, pal.raised.alpha(a));
    }
    gear_icon(g, sx, by, if s.settings.open { s.accent } else { pal.text2 }.alpha(a));

    let fade = clamp01(s.content_fade.x);
    if s.tab == 0 {
        draw_home(g, s, pal, fade);
        if s.device_menu && !s.outputs.is_empty() {
            g.offset(0.0, 0.0);
            draw_device_menu(g, s, pal, a * fade);
        }
    } else {
        let (a, dy) = group(s, 1);
        // Controls spread over the panel; the other tabs keep their layout, centred.
        let fixed = if s.tab == 7 { 0.0 } else { s.fixed_dy() };
        g.offset(content_dx(s), dy + fixed);
        match s.tab {
            1 => crate::panels::draw_clipboard(g, s, pal, a * fade),
            2 => crate::panels::draw_shelf(g, s, pal, a * fade),
            3 => crate::panels::draw_system(g, s, pal, a * fade),
            4 => crate::panels::draw_chat(g, s, pal, a * fade),
            5 => crate::panels::draw_notes(g, s, pal, a * fade),
            6 => crate::panels::draw_weather(g, s, pal, a * fade),
            7 => crate::panels::draw_controls(g, s, pal, a * fade),
            _ => {}
        }
    }
}

// ------------------------------------------------------------- Home layout --

const TRANSPORT_GAP: f32 = 44.0;
const MENU_W: f32 = 190.0;
const MENU_ROW: f32 = 30.0;
const DOCK_H: f32 = 46.0;
const DOCK_STEP: f32 = 38.0;
/// The gap between the Home tab's cards (music, volume, tiles block, dock).
const GAP: f32 = 8.0;
/// The gap between quick-action tiles.
const TILE_GAP: f32 = 6.0;
/// The music card's content block (art, titles, progress, transport) as designed.
const CARD_BLOCK_H: f32 = 148.0;

/// A rectangle in panel content units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        (self.x..=self.x + self.w).contains(&x) && (self.y..=self.y + self.h).contains(&y)
    }
}

/// Where the Home tab's parts go for the parts shown and the panel's size.
/// Whatever is shown moves together and grows into the space of what is not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HomeLayout {
    pub card: Option<Area>,
    pub volume: Option<Area>,
    pub dock: Option<Area>,
    /// Quick-action grid: first tile's corner, tile size, columns, gaps.
    pub grid: Option<(f32, f32, f32, f32, usize, f32, f32)>,
    /// How many quick actions are shown.
    pub tiles: usize,
}

impl HomeLayout {
    /// Quick-action tile in grid slot `slot`.
    pub fn tile(&self, slot: usize) -> Option<Area> {
        let (x, y, w, h, cols, gx, gy) = self.grid?;
        (slot < self.tiles).then(|| Area { x: x + (slot % cols) as f32 * (w + gx), y: y + (slot / cols) as f32 * (h + gy), w, h })
    }

    /// How the music card is laid out in the space it has.
    pub fn card_class(&self) -> CardClass {
        match self.card {
            Some(c) if c.w >= 300.0 && c.h >= 118.0 => CardClass::Full,
            Some(c) if c.h >= 150.0 => CardClass::Column,
            _ => CardClass::Mini,
        }
    }

    /// Vertical offset of the card's content block inside a taller card.
    fn card_dy(&self) -> f32 {
        match (self.card, self.card_class()) {
            (Some(c), CardClass::Full) => ((c.h - CARD_BLOCK_H) * 0.5).max(0.0),
            _ => 0.0,
        }
    }

    /// The cover: x, y, size, corner radius.
    fn art(&self) -> Option<(f32, f32, f32, f32)> {
        let c = self.card?;
        Some(match self.card_class() {
            CardClass::Full => (c.x + 16.0, c.y + self.card_dy() + 16.0, 86.0, 12.0),
            CardClass::Column => (c.x + 14.0, c.y + 14.0, 64.0, 10.0),
            CardClass::Mini => {
                let size = (c.h - 12.0).clamp(20.0, 48.0);
                (c.x + 6.0, c.y + (c.h - size) * 0.5, size, 8.0)
            }
        })
    }

    /// The text block next to the cover: left, width.
    fn card_text(&self) -> Option<(f32, f32)> {
        let c = self.card?;
        let (ax, _, size, _) = self.art()?;
        let x = ax + size + if self.card_class() == CardClass::Full { 16.0 } else { 12.0 };
        let right = match self.card_class() {
            // In a strip the buttons sit on the right.
            CardClass::Mini => self.transport().map_or(c.x + c.w, |(tx, _)| tx - self.transport_gap() - 22.0),
            _ => c.x + c.w - 16.0,
        };
        Some((x, (right - x).max(10.0)))
    }

    /// The progress bar: left end, right end, height.
    fn progress(&self) -> Option<(f32, f32, f32)> {
        let c = self.card?;
        Some(match self.card_class() {
            CardClass::Full => (c.x + 118.0, c.x + c.w - 20.0, c.y + self.card_dy() + 90.0),
            CardClass::Column => (c.x + 16.0, c.x + c.w - 16.0, c.y + c.h - 66.0),
            CardClass::Mini => (c.x + 12.0, c.x + c.w - 12.0, c.y + c.h - 3.0),
        })
    }

    /// Distance between the play button and previous / next.
    fn transport_gap(&self) -> f32 {
        if self.card_class() == CardClass::Mini { 32.0 } else { TRANSPORT_GAP }
    }

    /// Centre of the play / pause button.
    pub fn transport(&self) -> Option<(f32, f32)> {
        let c = self.card?;
        Some(match self.card_class() {
            CardClass::Full => (c.x + 118.0 + (c.w - 138.0) * 0.5, c.y + self.card_dy() + 126.0),
            CardClass::Column => (c.x + c.w * 0.5, c.y + c.h - 30.0),
            CardClass::Mini => (c.x + c.w - 18.0 - 32.0, c.y + c.h * 0.5),
        })
    }

    /// Room for shuffle and repeat beside the transport.
    fn has_extras(&self) -> bool {
        self.card_class() != CardClass::Mini
    }

    /// The volume slider's two ends (narrow rows leave out the device name).
    fn slider(&self) -> Option<(f32, f32)> {
        let v = self.volume?;
        Some(if volume_wide(v) { (v.x + 46.0, v.x + v.w - 146.0) } else { (v.x + 40.0, v.x + v.w - 52.0) })
    }
}

/// How the music card fits the space Ctrl + drag gave it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CardClass {
    /// The designed card: cover, titles, progress, transport.
    Full,
    /// Narrow and tall: cover and titles on top, progress and transport under them.
    Column,
    /// A strip: small cover, title, transport on the right, a thin progress line.
    Mini,
}

/// A volume row wide enough for the output device's name.
fn volume_wide(v: Area) -> bool {
    v.w >= 300.0
}

/// A grid of `n` tiles filling `w` x `h` (gaps `gap`): the number of columns
/// that gives tiles closest to 1.6 : 1, at most `max_h` tall, preferring full rows.
fn fill_grid(x: f32, y: f32, w: f32, h: f32, n: usize, gap: f32, max_h: f32) -> (f32, f32, f32, f32, usize, f32, f32) {
    let mut best = (f32::MAX, 1usize);
    for cols in 1..=n.clamp(1, 6) {
        let rows = n.div_ceil(cols);
        let tw = (w - (cols - 1) as f32 * gap) / cols as f32;
        let th = ((h - (rows - 1) as f32 * gap) / rows as f32).min(max_h);
        let empty = (rows * cols - n) as f32;
        let score = (tw / th / 1.6).ln().abs() + empty * 0.6;
        if score < best.0 {
            best = (score, cols);
        }
    }
    let cols = best.1;
    let rows = n.div_ceil(cols);
    let tw = (w - (cols - 1) as f32 * gap) / cols as f32;
    let th = ((h - (rows - 1) as f32 * gap) / rows as f32).min(max_h);
    (x, y, tw, th, cols, gap, gap)
}

/// Home sections by id (the `Group::HomeSection` items).
pub const SECTION_MUSIC: usize = 0;
pub const SECTION_VOLUME: usize = 1;
pub const SECTION_TILES: usize = 2;
pub const SECTION_DOCK: usize = 3;

/// The four regions of the Home tab: 0 the big one (top left), 1 the strip
/// under it, 2 the column on the right, 3 the strip along the bottom. Each
/// holds the section `home_sections` puts there (music, volume, tiles and dock
/// by default); a region whose section is hidden gives its room to the others.
fn home_regions(s: &State) -> [Option<Area>; 4] {
    let (pw, ph) = s.panel_local();
    let x0 = CX - pw * 0.5 + 16.0;
    let width = pw - 32.0;
    let (top, bottom) = (56.0, ph - 24.0);
    let (music, volume, dock) = s.look.home;
    let tiles = s.look.quick.len().min(QUICK.len()) > 0;
    let shown = |section: usize| [music, volume, tiles, dock][section.min(3)];
    let order = s.order_of(Group::HomeSection);
    let occ: [bool; 4] = std::array::from_fn(|r| order.get(r).is_some_and(|&sec| shown(sec)));
    let mut regions = [None; 4];
    if occ[3] {
        regions[3] = Some(Area { x: x0, y: bottom - DOCK_H, w: width, h: DOCK_H });
    }
    let main_bottom = if occ[3] { bottom - DOCK_H - GAP } else { bottom };
    let main_h = main_bottom - top;
    if occ[0] {
        // The big one on the left (with the strip under it), the column on the right.
        let col_w = 3.0 * QUICK_W + 2.0 * TILE_GAP;
        let left_w = if occ[2] { width - col_w - GAP } else { width };
        if occ[2] {
            regions[2] = Some(Area { x: x0 + width - col_w, y: top, w: col_w, h: main_h });
        }
        let big_h = if occ[1] { main_h - 40.0 - GAP } else { main_h };
        regions[0] = Some(Area { x: x0, y: top, w: left_w, h: big_h });
        if occ[1] {
            regions[1] = Some(Area { x: x0, y: main_bottom - 40.0, w: left_w, h: 40.0 });
        }
    } else {
        // No big one: the strip runs across the top and the column takes the
        // rest; alone, the strip becomes a card filling the space.
        let mut rest_top = top;
        if occ[1] {
            let h = if occ[2] { 40.0 } else { main_h };
            regions[1] = Some(Area { x: x0, y: top, w: width, h });
            rest_top += 40.0 + GAP;
        }
        if occ[2] {
            regions[2] = Some(Area { x: x0, y: rest_top, w: width, h: main_bottom - rest_top });
        }
    }
    regions
}

/// Each section's slot (by section id): the region it was put in.
fn section_slots(s: &State) -> [Option<Area>; 4] {
    let regions = home_regions(s);
    let order = s.order_of(Group::HomeSection);
    std::array::from_fn(|sec| order.iter().position(|&o| o == sec).and_then(|r| regions[r]))
}

/// The Home sections as laid out: (section id, region), for Ctrl + drag.
fn home_sections(s: &State) -> Vec<(usize, Area)> {
    section_slots(s).iter().enumerate().filter_map(|(sec, r)| r.map(|r| (sec, r))).collect()
}

/// The box a Home section takes in layout `l` (the tiles: the box around them).
fn section_box(l: &HomeLayout, sec: usize) -> Option<Area> {
    match sec {
        SECTION_MUSIC => l.card,
        SECTION_VOLUME => l.volume,
        SECTION_DOCK => l.dock,
        _ => (0..l.tiles).filter_map(|k| l.tile(k)).fold(None::<Area>, |acc, t| {
            Some(acc.map_or(t, |b| {
                let (x0, y0) = (b.x.min(t.x), b.y.min(t.y));
                Area { x: x0, y: y0, w: (b.x + b.w).max(t.x + t.w) - x0, h: (b.y + b.h).max(t.y + t.h) - y0 }
            }))
        }),
    }
}

/// The handle shown on a section while Ctrl is held: grab it to move the
/// whole section (a press elsewhere on the tiles or the dock moves one item).
fn section_handle(r: Area) -> Area {
    Area { x: r.x + r.w - 30.0, y: r.y - 6.0, w: 26.0, h: 14.0 }
}

/// The section whose handle is under (x, y) (panel units), while Ctrl is held.
pub fn section_handle_at(s: &State, x: f32, y: f32) -> Option<usize> {
    let l = home_layout(s);
    (0..4).find(|&sec| section_box(&l, sec).is_some_and(|r| section_handle(r).contains(x, y)))
}

/// The parts of the Home tab for the sections at `rects` (by section id).
fn home_layout_in(s: &State, rects: [Option<Area>; 4]) -> HomeLayout {
    let tiles = s.look.quick.len().min(QUICK.len());
    let grid = rects[SECTION_TILES].filter(|_| tiles > 0).map(|r| {
        // A narrow column keeps tiles at most 64 tall (3 across, as designed).
        let max_h = if r.w < 260.0 { 64.0 } else { 110.0 };
        fill_grid(r.x, r.y, r.w, r.h, tiles, TILE_GAP, max_h)
    });
    HomeLayout { card: rects[SECTION_MUSIC], volume: rects[SECTION_VOLUME], dock: rects[SECTION_DOCK], grid, tiles }
}

pub fn home_layout(s: &State) -> HomeLayout {
    home_layout_in(s, section_slots(s))
}

/// The layout as it is drawn right now: sections that moved glide (and
/// resize) to their regions, the one held with Ctrl + drag follows the pointer.
fn home_layout_drawn(s: &State) -> HomeLayout {
    let slots = section_slots(s);
    let rects: [Option<Area>; 4] = std::array::from_fn(|sec| slots[sec].map(|slot| s.grip_rect(Group::HomeSection, sec, slot).0));
    home_layout_in(s, rects)
}

/// Top-left corner of the output-device list: above the volume row, or below
/// it when there is no room above.
fn menu_origin(s: &State) -> (f32, f32) {
    let v = home_layout(s).volume.unwrap_or(Area { x: CX, y: 212.0, w: 0.0, h: 40.0 });
    let h = s.outputs.len() as f32 * MENU_ROW + 8.0;
    let y = if v.y - 6.0 - h >= 50.0 { v.y - 6.0 - h } else { v.y + v.h + 6.0 };
    (v.x + v.w - MENU_W, y)
}

/// What a click flips between the cover and the lyrics: the cover itself, or
/// (in the lyrics view) the small cover and the lines.
fn lyrics_spot(s: &State, l: &HomeLayout) -> Option<Area> {
    if !(s.media.session && s.lyric.enabled) {
        return None;
    }
    let c = l.card?;
    let top = c.y + l.card_dy();
    // (The lyrics view needs the full card; smaller ones only flip back from it.)
    Some(match l.card_class() {
        CardClass::Full if s.lyric.card => {
            let (lx, ly, lw, lh) = lyric_area(c, top);
            Area { x: lx, y: top + 6.0, w: lw, h: ly + lh - top - 6.0 }
        }
        CardClass::Mini => return None,
        _ => {
            let (ax, ay, size, _) = l.art()?;
            Area { x: ax, y: ay, w: size, h: size }
        }
    })
}

/// (x, y in panel content units.)
fn home_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    let l = home_layout(s);
    if lyrics_spot(s, &l).is_some_and(|spot| spot.contains(x, y)) && !s.device_menu {
        return Some(Hit::Lyrics);
    }
    if s.device_menu {
        let (mx, my) = menu_origin(s);
        if x >= mx && x < mx + MENU_W && y >= my + 4.0 {
            let row = ((y - my - 4.0) / MENU_ROW) as usize;
            if row < s.outputs.len() {
                return Some(Hit::Output(row));
            }
        }
    }
    // Seek, shuffle and repeat, where the player allows them (not in the lyrics view).
    let card_view = s.media.session && !(s.lyric.enabled && s.lyric.card && l.card_class() == CardClass::Full);
    if let (true, true, Some((x0, x1, by))) = (card_view, s.media.can.seek && s.media.duration > 0.0, l.progress()) {
        if (y - by).abs() <= 8.0 && (x0 - 4.0..=x1 + 4.0).contains(&x) {
            return Some(Hit::Seek(clamp01((x - x0) / (x1 - x0))));
        }
    }
    let gap = l.transport_gap();
    if let (true, true, Some((tx, ty))) = (card_view, l.has_extras(), l.transport()) {
        if (y - ty).abs() <= 16.0 {
            if s.media.can.shuffle.is_some() && (x - tx + 2.0 * gap).abs() <= 16.0 {
                return Some(Hit::Shuffle);
            }
            if s.media.can.repeat.is_some() && (x - tx - 2.0 * gap).abs() <= 16.0 {
                return Some(Hit::Repeat);
            }
        }
    }
    if let (true, Some((tx, ty))) = (s.media.session, l.transport()) {
        if (y - ty).abs() <= 18.0 {
            for (dx, hit) in [(-gap, Hit::Previous), (0.0, Hit::Toggle), (gap, Hit::Next)] {
                if (x - tx - dx).abs() <= 18.0 {
                    return Some(hit);
                }
            }
        }
    }
    if let (Some(v), Some((x0, x1))) = (l.volume, l.slider()) {
        if v.contains(x, y) {
            if (x0 - 12.0..=x1 + 8.0).contains(&x) {
                return Some(Hit::Volume(clamp01((x - x0) / (x1 - x0))));
            }
            if volume_wide(v) && x >= v.x + v.w - 104.0 {
                return Some(Hit::Device);
            }
        }
    }
    for (slot, &id) in s.look.quick.iter().enumerate().take(QUICK.len()) {
        if l.tile(slot).is_some_and(|t| t.contains(x, y)) {
            return Some(Hit::Quick(id));
        }
    }
    if let Some(d) = l.dock {
        if d.contains(x, y) && x >= d.x + 6.0 {
            let slot = ((x - d.x - 6.0) / DOCK_STEP) as usize;
            if slot < s.dock.len() {
                return Some(Hit::Dock(slot));
            }
            if slot == s.dock.len() && slot < crate::dock::LIMIT {
                return Some(Hit::DockAdd);
            }
        }
    }
    None
}
/// Quick actions in grid order.
pub const QUICK_AREA: usize = 0;
pub const QUICK_RECORD: usize = 6;
pub const QUICK_FULL: usize = 1;
pub const QUICK_TEXT: usize = 2;
pub const QUICK_QR: usize = 3;
pub const QUICK_COLOR: usize = 4;
pub const QUICK_PLAIN: usize = 5;
pub const QUICK_COMMAND: usize = 7;
pub const QUICK_VOICE: usize = 8;
pub const QUICK_TIMER: usize = 9;
pub const QUICK_NOTES: usize = 10;
pub const QUICK_LOCK: usize = 11;
/// Width of a quick-action tile next to the music card.
const QUICK_W: f32 = 70.0;

/// Springy ease (overshoots a little before settling): the pop-in of tiles.
fn ease_out_back(t: f32) -> f32 {
    let t = clamp01(t);
    let (c1, c3) = (1.5, 2.5);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// How far element `k` of a staggered group has popped in as the panel opens
/// (starts `delay` s into the reveal, `step` s after the one before).
fn pop(s: &State, delay: f32, step: f32, k: usize) -> f32 {
    ease_out_back((s.reveal - delay - k as f32 * step) / 0.32)
}

/// Sideways slide of tab content while it fades in: from the side of the tab
/// bar it was chosen on.
fn content_dx(s: &State) -> f32 {
    (1.0 - clamp01(s.content_fade.x)) * 22.0 * s.tab_dir
}

/// How far the music card leans towards a pointer at window (x, y): -1..1
/// each way over the card, nothing elsewhere.
pub fn card_lean(s: &State, x: f32, y: f32) -> (f32, f32) {
    let Some(c) = home_layout(s).card else { return (0.0, 0.0) };
    let (x, y) = s.to_panel(x, y);
    let (cx, cy, hw, hh) = (c.x + c.w * 0.5, c.y + c.h * 0.5, c.w * 0.5, c.h * 0.5);
    if (x - cx).abs() > hw + 10.0 || (y - cy).abs() > hh + 10.0 {
        return (0.0, 0.0);
    }
    (((x - cx) / hw).clamp(-1.0, 1.0), ((y - cy) / hh).clamp(-1.0, 1.0))
}

/// The tab a [`Group`] lives on.
pub fn grip_tab(group: Group) -> usize {
    match group {
        Group::Sys => 3,
        Group::CtlTile | Group::CtlPower => 7,
        Group::WxCard | Group::WxDetail | Group::WxView => 6,
        Group::HomeSection => 0,
    }
}

/// The groups of a tab, in the order a press looks for them (small items
/// inside big ones first).
pub fn grip_groups(tab: usize) -> &'static [Group] {
    match tab {
        3 => &[Group::Sys],
        7 => &[Group::CtlTile, Group::CtlPower],
        6 => &[Group::WxView, Group::WxDetail, Group::WxCard],
        0 => &[Group::HomeSection],
        _ => &[],
    }
}

/// How far a tab's own units sit below the panel's (fixed-layout tabs are centred).
pub fn grip_dy(s: &State, group: Group) -> f32 {
    if matches!(grip_tab(group), 3 | 6) { s.fixed_dy() } else { 0.0 }
}

/// A group's items as they are laid out now: (id, slot) in the tab's units,
/// in the order shown (hidden ones left out).
pub fn grip_items(s: &State, group: Group) -> Vec<(usize, Area)> {
    match group {
        Group::Sys => crate::panels::sys_items(s),
        Group::CtlTile | Group::CtlPower => crate::panels::controls_items(s, group),
        Group::WxCard | Group::WxDetail | Group::WxView => crate::panels::weather_items(s, group),
        Group::HomeSection => home_sections(s),
    }
}

/// The item of the open tab under (x, y) (panel units) that Ctrl + drag would move.
pub fn grip_at(s: &State, x: f32, y: f32) -> Option<Grip> {
    for &group in grip_groups(s.tab) {
        let ty = y - grip_dy(s, group);
        if let Some((id, _)) = grip_items(s, group).into_iter().find(|(_, r)| r.contains(x, ty)) {
            return Some(Grip { group, id });
        }
    }
    None
}

/// Quick-action grid slot `slot` (panel content units), for Ctrl + drag.
pub fn quick_slot(s: &State, slot: usize) -> Option<Area> {
    home_layout(s).tile(slot)
}

/// Left edge of dock slot `slot` (panel content units), for Ctrl + drag.
pub fn dock_slot_x(s: &State, slot: usize) -> Option<f32> {
    home_layout(s).dock.map(|d| d.x + 10.0 + slot as f32 * DOCK_STEP)
}

/// `true` when window (x, y) is on the Home tab's dock row (where dropped files get pinned).
pub fn dock_contains(s: &State, x: f32, y: f32) -> bool {
    let (x, y) = s.to_panel(x, y);
    home_layout(s).dock.is_some_and(|d| d.contains(x, y))
}

/// Volume (0..1) for a pointer at window x (used while dragging the slider).
pub fn volume_at(s: &State, x: f32) -> f32 {
    let (x, _) = s.to_panel(x, 0.0);
    home_layout(s).slider().map_or(s.volume, |(x0, x1)| clamp01((x - x0) / (x1 - x0)))
}

fn draw_device_menu(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    let (mx, my) = menu_origin(s);
    let height = s.outputs.len() as f32 * MENU_ROW + 8.0;
    g.fill_rrect(mx, my, MENU_W, height, 12.0, pal.raised.alpha(a));
    g.stroke_rrect(mx, my, MENU_W, height, 12.0, 1.0, pal.edge.alpha(a));
    for (i, output) in s.outputs.iter().enumerate() {
        let y = my + 4.0 + i as f32 * MENU_ROW;
        if output.current {
            g.fill_circle(mx + 14.0, y + MENU_ROW * 0.5, 3.0, s.accent.alpha(a));
        } else if output.offline {
            // A paired Bluetooth device that is not connected: a small rune, click to connect.
            let (cx, cy, ink) = (mx + 14.0, y + MENU_ROW * 0.5, pal.text2.alpha(a * 0.8));
            g.line(cx, cy - 5.0, cx, cy + 5.0, 1.2, ink);
            g.line(cx, cy - 5.0, cx + 3.0, cy - 2.5, 1.2, ink);
            g.line(cx + 3.0, cy - 2.5, cx - 3.0, cy + 2.5, 1.2, ink);
            g.line(cx, cy + 5.0, cx + 3.0, cy + 2.5, 1.2, ink);
            g.line(cx + 3.0, cy + 2.5, cx - 3.0, cy - 2.5, 1.2, ink);
        }
        let c = if output.current { pal.text } else { pal.text2 };
        g.text(&output.name, Font::Body, mx + 26.0, y, MENU_W - 34.0, MENU_ROW, c.alpha(a), Align::Left);
    }
}

/// What is inside the music card `c`, in whichever shape it has (full card,
/// narrow column, strip): cover, titles (or the line being sung), progress,
/// transport, shuffle / repeat; or the lyrics view; or "Nothing playing".
fn draw_music_content(g: &Gfx, s: &State, pal: &Palette, l: &HomeLayout, c: Area, a: f32) {
    let class = l.card_class();
    let top = c.y + l.card_dy();
    let m = &s.media;
    if !m.session {
        if class == CardClass::Mini {
            g.text(ws(w!("Nothing playing")), Font::Label, c.x, c.y, c.w, c.h, pal.text2.alpha(a), Align::Center);
        } else {
            g.text(ws(w!("Nothing playing")), Font::Title, c.x, c.y + c.h * 0.5 - 24.0, c.w, 24.0, pal.text.alpha(a), Align::Center);
            g.text(ws(w!("Start music or a video in any app")), Font::Body, c.x, c.y + c.h * 0.5 + 2.0, c.w, 20.0, pal.text2.alpha(a), Align::Center);
        }
        return;
    }
    if s.lyric.enabled && s.lyric.card && class == CardClass::Full {
        draw_card_lyrics(g, s, pal, c, top, a);
        return;
    }
    let (Some((ax, ay, size, radius)), Some((tx, tw))) = (l.art(), l.card_text()) else { return };
    album_art(g, s, ax, ay, size, radius, a);
    // Rows of the text block: the player's name, the title, the artist (or the line being sung).
    let (src_y, title_y, line_y) = match class {
        CardClass::Full => (Some(top + 16.0), top + 34.0, top + 58.0),
        CardClass::Column => (Some(c.y + 14.0), c.y + 30.0, c.y + 52.0),
        CardClass::Mini if c.h < 44.0 => (None, c.y + (c.h - 22.0) * 0.5, f32::NAN),
        CardClass::Mini => (None, c.y + c.h * 0.5 - 21.0, c.y + c.h * 0.5 - 1.0),
    };
    g.text(&m.title, Font::Title, tx, title_y, tw, 24.0, pal.text.alpha(a), Align::Left);
    if s.lyric.singing_in_card() && !line_y.is_nan() {
        // The line being sung takes the artist's place, sliding like in the
        // pill; the artist moves up next to the player's name.
        if let Some(src_y) = src_y {
            let mut src = WBuf::new();
            for &u in m.source.iter().chain(&[0x20, 0xB7, 0x20]).chain(m.artist.iter()) {
                if !src.push(u) {
                    break;
                }
            }
            g.text(src.as_slice(), Font::Small, tx, src_y, tw, 16.0, pal.text2.alpha(a), Align::Left);
        }
        let f = ease_out_cubic(clamp01(s.lyric.fade.x));
        g.clip(tx, line_y - 1.0, tw, 23.0);
        let shown = |line: &[u16], dy: f32, k: f32| {
            let (line, colour) = if line.is_empty() { (ws(w!("\u{266A}")), pal.text2) } else { (line, pal.text) };
            g.text(line, Font::Body, tx, line_y + dy, tw, 20.0, colour.alpha(a * k), Align::Left);
        };
        if f < 1.0 {
            shown(&s.lyric.prev, -12.0 * f, 1.0 - f);
        }
        shown(&s.lyric.text, 12.0 * (1.0 - f), f);
        g.unclip();
    } else {
        if let Some(src_y) = src_y {
            g.text(&m.source, Font::Small, tx, src_y, tw, 16.0, pal.text2.alpha(a), Align::Left);
        }
        if !line_y.is_nan() {
            g.text(&m.artist, Font::Body, tx, line_y, tw, 20.0, pal.text2.alpha(a), Align::Left);
        }
    }
    // Progress (a thin line along the bottom of a strip, with times otherwise).
    if let Some((bx0, bx1, by)) = l.progress() {
        let elapsed = m.elapsed();
        let thick = if class == CardClass::Mini { 2.0 } else { 3.0 };
        g.line(bx0, by, bx1, by, thick, pal.raised.alpha(a));
        if m.duration > 0.0 {
            let at = lerp(bx0, bx1, clamp01(elapsed / m.duration));
            g.line(bx0, by, at, by, thick, s.accent.alpha(a));
            if m.can.seek && class != CardClass::Mini {
                // A knob says the bar can be clicked to jump.
                g.fill_circle(at, by, 4.5, pal.text.alpha(a));
            }
        }
        if class != CardClass::Mini {
            if m.duration > 0.0 {
                let mut b = WBuf::new();
                let left = (m.duration - elapsed).max(0.0) as u32;
                let _ = write!(b, "-{}:{:02}", left / 60, left % 60);
                g.text(b.as_slice(), Font::Small, bx1 - 60.0, by + 6.0, 60.0, 16.0, pal.text2.alpha(a), Align::Right);
            }
            let mut b = WBuf::new();
            let _ = write!(b, "{}:{:02}", elapsed as u32 / 60, elapsed as u32 % 60);
            g.text(b.as_slice(), Font::Small, bx0, by + 6.0, 60.0, 16.0, pal.text2.alpha(a), Align::Left);
        }
    }
    // A small hint on the cover that it opens the lyrics.
    if s.lyric.enabled && class != CardClass::Mini && size >= 56.0 {
        let (hx, hy) = (ax + size - 10.0, ay + size - 10.0);
        g.fill_circle(hx, hy, 9.0, BLACK.alpha(0.55 * a));
        g.text(ws(w!("\u{266A}")), Font::Small, hx - 9.0, hy - 9.0, 18.0, 18.0, WHITE.alpha(a), Align::Center);
    }
    if let Some((cx, cy)) = l.transport() {
        let (r, gap) = (if class == CardClass::Mini { 13.0 } else { 15.0 }, l.transport_gap());
        g.fill_circle(cx, cy, r, pal.text.alpha(a));
        if m.playing {
            g.line(cx - 3.2, cy - 5.0, cx - 3.2, cy + 5.0, 3.0, pal.island.alpha(a));
            g.line(cx + 3.2, cy - 5.0, cx + 3.2, cy + 5.0, 3.0, pal.island.alpha(a));
        } else {
            // Play triangle.
            g.line(cx - 3.0, cy - 5.5, cx - 3.0, cy + 5.5, 2.6, pal.island.alpha(a));
            g.line(cx - 3.0, cy - 5.5, cx + 5.5, cy, 2.6, pal.island.alpha(a));
            g.line(cx - 3.0, cy + 5.5, cx + 5.5, cy, 2.6, pal.island.alpha(a));
            g.fill_circle(cx - 0.3, cy, 2.6, pal.island.alpha(a));
        }
        skip_icon(g, cx - gap, cy, -1.0, pal.text.alpha(a));
        skip_icon(g, cx + gap, cy, 1.0, pal.text.alpha(a));
        if l.has_extras() {
            // Lit in the accent while on; only when the player allows them.
            let lit = |on: bool| if on { s.accent } else { pal.text2 }.alpha(a);
            if let Some(on) = m.can.shuffle {
                shuffle_icon(g, cx - 2.0 * gap, cy, lit(on));
            }
            if let Some(mode) = m.can.repeat {
                repeat_icon(g, cx + 2.0 * gap, cy, mode, lit(mode != 0));
            }
        }
    }
}

/// Home tab: now playing, volume, quick actions and dock (the last two become live in phases 5-6).
fn draw_home(g: &Gfx, s: &State, pal: &Palette, fade: f32) {
    let l = home_layout_drawn(s);
    let playing = clamp01(s.wave.x);
    let dx = content_dx(s);

    let music_section = || {
    // Group 1: now-playing card and volume row.
    let (a, dy) = group(s, 1);
    let a = a * fade;
    if let Some(c) = l.card {
        // The card leans a few dp towards the pointer, a soft light following it.
        let (lx, ly) = (s.lean[0].x, s.lean[1].x);
        g.offset(dx + lx * 3.0, dy + ly * 2.0);
        // Tinted while music plays: by the cover when the island follows the music, else by its colour.
        let card = pal.card.mix(theme::surface_colour(s.accent), 0.16 * playing).alpha(a);
        g.card(c.x, c.y, c.w, c.h, 16.0, card);
        let lit = lx.abs().max(ly.abs());
        if lit > 0.01 {
            g.clip(c.x, c.y, c.w, c.h);
            g.fill_circle(c.x + c.w * 0.5 + lx * c.w * 0.38, c.y + c.h * 0.5 + ly * c.h * 0.4, 120.0, pal.text.alpha(0.035 * lit * a));
            g.unclip();
        }
        draw_music_content(g, s, pal, &l, c, a);
    }
    };
    let volume_section = || {
        let (a, dy) = group(s, 1);
        let a = a * fade;
    g.offset(dx, dy);
    if let (Some(v), Some((sx0, sx1))) = (l.volume, l.slider()) {
        // (A volume card taller than its row keeps the row in its middle.)
        let cy = v.y + v.h * 0.5;
        let row = cy - 20.0;
        g.card(v.x, v.y, v.w, v.h, if v.h > 40.0 { 16.0 } else { 14.0 }, pal.card.alpha(a));
        speaker_icon(g, v.x + 22.0, cy, pal.text2.alpha(a));
        let knob = lerp(sx0, sx1, clamp01(s.volume));
        g.line(sx0, cy, sx1, cy, 3.0, pal.raised.alpha(a));
        g.line(sx0, cy, knob, cy, 3.0, pal.text.alpha(a));
        g.fill_circle(knob, cy, 6.0, pal.text.alpha(a));
        let mut b = WBuf::new();
        let _ = write!(b, "{}", (s.volume * 100.0).round() as u32);
        if volume_wide(v) {
            g.text(b.as_slice(), Font::Label, v.x + v.w - 140.0, row, 34.0, 40.0, pal.text.alpha(a), Align::Center);
            let device: &[u16] = s.outputs.iter().find(|o| o.current).map_or(ws(w!("Output")), |o| &o.name);
            g.text(device, Font::Body, v.x + v.w - 98.0, row, 76.0, 40.0, pal.text2.alpha(a), Align::Left);
            let ax = v.x + v.w - 12.0;
            g.line(ax - 4.0, cy - 2.0, ax, cy + 2.0, 1.6, pal.text2.alpha(a));
            g.line(ax + 4.0, cy - 2.0, ax, cy + 2.0, 1.6, pal.text2.alpha(a));
        } else {
            // Narrow: just the level on the right.
            g.text(b.as_slice(), Font::Label, v.x + v.w - 44.0, row, 34.0, 40.0, pal.text.alpha(a), Align::Center);
        }
    }
    };

    let tiles_section = || {
    // Group 2: quick actions (the chosen ones, in the chosen order), icon tiles
    // that pop in one after another, lift under the pointer and wiggle their icon.
    let (a, dy) = group(s, 2);
    let a = a * fade;
    g.offset(dx, dy);
    // Two passes: the tile held with Ctrl + drag comes last, over the others.
    let held_tile = match s.home_held {
        Some(HomeItem::Tile(id)) => Some(id),
        _ => None,
    };
    let hlift = clamp01(s.home_lift.x);
    for pass in 0..2 {
    for (slot, &id) in s.look.quick.iter().enumerate().take(QUICK.len()) {
        let Some(t) = l.tile(slot) else { break };
        let held = held_tile == Some(id);
        if held != (pass == 1) {
            continue;
        }
        // At its slot plus the offset it is gliding back from; the held one
        // follows the pointer.
        let (mut x, mut y) = (t.x + s.quick_off[id].0.x, t.y + s.quick_off[id].1.x);
        if let (true, Some((_, px, py))) = (held, s.home_drag) {
            (x, y) = (px, py);
        }
        let (tw, th) = (t.w, t.h);
        let p = pop(s, 0.12, 0.022, slot);
        if p <= 0.001 {
            continue;
        }
        let k = a * clamp01(p * 1.4);
        let lift = if held {
            hlift
        } else if !s.look.lively {
            0.0
        } else if s.hover == Some(Hit::Quick(id)) {
            clamp01(s.hover_in.x)
        } else if s.hover_old == Some(Hit::Quick(id)) {
            clamp01(s.hover_out.x)
        } else {
            0.0
        };
        // Grows from 80 % with a little overshoot.
        let grow = (0.8 + 0.2 * p) * (1.0 + 0.05 * lift);
        let rise = -2.5 * lift;
        let (cx, cy) = (x + tw * 0.5, y + th * 0.5);
        // While recording, "Record" becomes a red "Stop".
        let stopping = id == QUICK_RECORD && s.recording;
        let grow = if held { grow * (1.0 + 0.04 * hlift) } else { grow };
        g.pose_begin(cx, cy, 0.0, rise, 0.0, grow, grow);
        g.card(x, y, tw, th, 12.0, pal.card.mix(pal.text, 0.05 * lift).alpha(k));
        if held {
            g.stroke_rrect(x + 0.5, y + 0.5, tw - 1.0, th - 1.0, 12.0, 1.4, s.accent.alpha(k * hlift));
        }
        let label = if stopping { w!("Stop") } else { QUICK[id] };
        // Icon over label, as a block in the middle of the tile (bigger in big tiles).
        let big = (th / 46.0).clamp(1.0, 1.5);
        // Tiles too small for a label (the block put in a strip) show the icon alone.
        let labelled = tw >= 54.0 && th >= 40.0;
        let icon_y = if labelled { cy - 6.0 * big } else { cy };
        if labelled {
            g.text(ws(label), Font::Small, x + 2.0, icon_y + 10.0 * big, tw - 4.0, 16.0, pal.text2.alpha(k), Align::Center);
        }
        // The icon wiggles once as the light comes on.
        let wiggle = (lift * std::f32::consts::TAU).sin() * 0.18;
        let icon_grow = grow * (1.0 + 0.08 * lift) * big;
        g.pose_begin(cx, icon_y, 0.0, rise - 1.0 * lift, wiggle, icon_grow, icon_grow);
        quick_icon(g, id, cx, icon_y, pal.text.alpha(k), s.accent.alpha(k), stopping);
        g.pose_end();
    }
    }

    };
    let dock_section = || {
    let hlift = clamp01(s.home_lift.x);
    // Group 3: app dock (icons pop in after the tiles).
    let Some(d) = l.dock else { return };
    let (a, dy) = group(s, 3);
    let a = a * fade;
    g.offset(dx, dy);
    // The card fills its region; the row of icons sits in its middle.
    let (x0, dock_y) = (d.x, d.y + ((d.h - DOCK_H) * 0.5).max(0.0));
    g.card(x0, d.y, d.w, d.h, if d.h > DOCK_H { 16.0 } else { 14.0 }, pal.card.alpha(a));
    if s.dock_drop {
        g.stroke_rrect(x0, d.y, d.w, d.h, 14.0, 1.4, theme::accent().alpha(a));
    }
    // Two passes again: an icon held with Ctrl + drag is drawn last, lifted.
    let held_dock = match s.home_held {
        Some(HomeItem::Dock(i)) => Some(i),
        _ => None,
    };
    for pass in 0..2 {
    for (i, item) in s.dock.iter().enumerate() {
        let held = held_dock == Some(i);
        if held != (pass == 1) {
            continue;
        }
        let mut x = x0 + 10.0 + i as f32 * DOCK_STEP + s.dock_off.get(i).map_or(0.0, |o| o.x);
        if let (true, Some((_, px, _))) = (held, s.home_drag) {
            x = px;
        }
        let p = pop(s, 0.3, 0.03, i);
        if p <= 0.001 {
            continue;
        }
        let a = a * clamp01(p * 1.4);
        // Rises into place with a small bounce.
        g.offset(dx, dy + (1.0 - p) * 10.0);
        if held {
            let k = 1.0 + 0.1 * hlift;
            g.pose_begin(x + 15.0, dock_y + 23.0, 0.0, -2.0 * hlift, 0.0, k, k);
            g.fill_rrect(x - 4.0, dock_y + 4.0, 38.0, 38.0, 10.0, pal.raised.alpha(a * hlift));
            g.stroke_rrect(x - 4.0, dock_y + 4.0, 38.0, 38.0, 10.0, 1.4, s.accent.alpha(a * hlift));
        } else if s.dock_hover == Some(i) && s.home_drag.is_none() {
            g.fill_rrect(x - 4.0, dock_y + 4.0, 38.0, 38.0, 10.0, pal.raised.alpha(a));
        }
        if !g.image(item.image, x, dock_y + 8.0, 30.0, 30.0, 6.0, a) {
            // The icon is still on its way: a tile with the first letter.
            g.fill_rrect(x, dock_y + 8.0, 30.0, 30.0, 9.0, pal.raised.alpha(a * 0.75));
            g.text(&item.title[..item.title.len().min(1)], Font::Label, x, dock_y + 8.0, 30.0, 30.0, pal.text.alpha(a), Align::Center);
        }
        if held {
            g.pose_end();
        }
    }
    }
    g.offset(dx, dy);
    if s.dock.len() < crate::dock::LIMIT {
        let px = x0 + 10.0 + s.dock.len() as f32 * DOCK_STEP;
        g.stroke_rrect(px + 1.0, dock_y + 9.0, 28.0, 28.0, 9.0, 1.2, pal.text2.alpha(a * 0.6));
        g.line(px + 10.0, dock_y + 23.0, px + 20.0, dock_y + 23.0, 1.6, pal.text2.alpha(a));
        g.line(px + 15.0, dock_y + 18.0, px + 15.0, dock_y + 28.0, 1.6, pal.text2.alpha(a));
    }
    // To the right: the name under the pointer, or how to pin something.
    let slots = (crate::dock::LIMIT as f32 + 1.0) * DOCK_STEP;
    let (tx, tw) = (x0 + 10.0 + slots, d.w - 24.0 - slots);
    // (A narrow dock has no room for the name or the hint.)
    if tw < 60.0 {
        return;
    }
    match s.dock_hover.and_then(|i| s.dock.get(i)) {
        Some(item) => g.text(&item.title, Font::Label, tx, dock_y, tw, DOCK_H, pal.text.alpha(a), Align::Right),
        None if s.dock_drop => g.text(ws(w!("Drop to pin")), Font::Label, tx, dock_y, tw, DOCK_H, theme::accent().alpha(a), Align::Right),
        None if s.dock_hint || s.dock.is_empty() => g.text(ws(w!("Drag an app or folder here")), Font::Body, tx, dock_y, tw, DOCK_H, pal.text2.alpha(a), Align::Right),
        // Otherwise the same hint, quieter, so the row's free space has a purpose.
        None => g.text(ws(w!("Drag an app or folder here")), Font::Small, tx, dock_y, tw, DOCK_H, pal.text2.alpha(a * 0.55), Align::Right),
    }
    };
    // Each section where it is (or is gliding to), the one held with Ctrl + drag
    // last, on top, with an outline while it is lifted.
    let mut seq = [SECTION_MUSIC, SECTION_VOLUME, SECTION_TILES, SECTION_DOCK];
    seq.sort_by_key(|&sec| s.grip_on_top(Group::HomeSection, sec));
    for sec in seq {
        match sec {
            SECTION_MUSIC => music_section(),
            SECTION_VOLUME => volume_section(),
            SECTION_TILES => tiles_section(),
            _ => dock_section(),
        }
        if s.grip_on_top(Group::HomeSection, sec) {
            if let Some(r) = section_box(&l, sec) {
                let (_, lift) = s.grip_rect(Group::HomeSection, sec, r);
                g.offset(dx, group(s, 1).1);
                g.stroke_rrect(r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0, 17.0, 1.6, s.accent.alpha(lift * fade));
            }
        }
    }
    // While Ctrl is held: a handle on each section to move it whole.
    if s.ctrl_held && s.grip_drag.is_none() && s.home_drag.is_none() {
        g.offset(dx, group(s, 1).1);
        for sec in 0..4 {
            let Some(r) = section_box(&l, sec) else { continue };
            let hd = section_handle(r);
            g.fill_rrect(hd.x, hd.y, hd.w, hd.h, hd.h * 0.5, s.accent.alpha(fade));
            for dot in [-6.0f32, 0.0, 6.0] {
                for row in [-2.2f32, 2.2] {
                    g.fill_circle(hd.x + hd.w * 0.5 + dot, hd.y + hd.h * 0.5 + row, 1.3, BLACK.alpha(0.8 * fade));
                }
            }
        }
    }
}

// --------------------------------------------------------------- icons ----

/// The icon of quick action `i`, centred on (cx, cy), about 18 dp.
fn quick_icon(g: &Gfx, i: usize, cx: f32, cy: f32, c: Rgba, accent: Rgba, stopping: bool) {
    const W: f32 = 1.5;
    match i {
        // Area: the four corners of a selection.
        QUICK_AREA => {
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (x, y) = (cx + sx * 8.0, cy + sy * 6.5);
                g.line(x, y, x - sx * 4.5, y, W, c);
                g.line(x, y, x, y - sy * 4.0, W, c);
            }
            g.fill_circle(cx, cy, 1.6, accent);
        }
        // Full screen: a monitor on its stand.
        QUICK_FULL => {
            g.stroke_rrect(cx - 8.5, cy - 7.0, 17.0, 11.5, 2.5, W, c);
            g.line(cx, cy + 4.5, cx, cy + 7.5, W, c);
            g.line(cx - 4.0, cy + 7.5, cx + 4.0, cy + 7.5, W, c);
            g.fill_rrect(cx - 5.5, cy - 4.0, 11.0, 5.5, 1.0, accent.alpha(accent.a * 0.5));
        }
        // Read text: a "T" between selection corners.
        QUICK_TEXT => {
            for sx in [-1.0f32, 1.0] {
                let x = cx + sx * 8.0;
                g.line(x, cy - 7.0, x, cy - 3.0, W, c);
                g.line(x, cy + 7.0, x, cy + 3.0, W, c);
                g.line(x, cy - 7.0, x - sx * 3.0, cy - 7.0, W, c);
                g.line(x, cy + 7.0, x - sx * 3.0, cy + 7.0, W, c);
            }
            g.line(cx - 3.5, cy - 4.0, cx + 3.5, cy - 4.0, 1.8, accent);
            g.line(cx, cy - 4.0, cx, cy + 4.5, 1.8, accent);
        }
        // QR code: three finder squares and a few modules.
        QUICK_QR => {
            for (x, y) in [(cx - 8.0, cy - 8.0), (cx + 2.0, cy - 8.0), (cx - 8.0, cy + 2.0)] {
                g.stroke_rrect(x, y, 6.0, 6.0, 1.2, 1.4, c);
                g.fill_rrect(x + 2.0, y + 2.0, 2.0, 2.0, 0.4, c);
            }
            for (x, y) in [(cx + 3.0, cy + 3.0), (cx + 6.5, cy + 6.0), (cx + 6.5, cy + 2.5), (cx + 3.0, cy + 6.5)] {
                g.fill_rrect(x - 1.0, y - 1.0, 2.2, 2.2, 0.4, accent);
            }
        }
        // Colour: an eyedropper with a drop of colour.
        QUICK_COLOR => {
            g.line(cx - 5.5, cy + 5.5, cx + 3.0, cy - 3.0, 2.6, c);
            g.line(cx + 2.0, cy - 6.5, cx + 6.5, cy - 2.0, 2.0, c);
            g.fill_circle(cx + 5.2, cy - 5.2, 2.6, c);
            g.fill_circle(cx - 6.5, cy + 6.5, 2.2, accent);
        }
        // Plain text: a clipboard with lines of text.
        QUICK_PLAIN => {
            g.stroke_rrect(cx - 6.5, cy - 6.5, 13.0, 15.0, 2.5, W, c);
            g.fill_rrect(cx - 3.0, cy - 8.5, 6.0, 3.5, 1.2, c);
            for k in 0..3 {
                let y = cy - 1.5 + k as f32 * 3.2;
                g.line(cx - 3.5, y, cx + if k == 2 { 1.0 } else { 3.5 }, y, 1.3, accent);
            }
        }
        // Record: a ring with a red dot (a red square while recording).
        QUICK_RECORD => {
            g.stroke_circle(cx, cy, 7.5, W, c);
            let red = Rgba::hex(0xff453a).alpha(c.a);
            if stopping {
                g.fill_rrect(cx - 3.2, cy - 3.2, 6.4, 6.4, 1.2, red);
            } else {
                g.fill_circle(cx, cy, 4.0, red);
            }
        }
        // Command: a search field with a magnifier.
        QUICK_COMMAND => {
            g.stroke_circle(cx - 1.5, cy - 1.5, 5.5, W, c);
            g.line(cx + 2.5, cy + 2.5, cx + 7.0, cy + 7.0, 2.0, c);
            g.fill_circle(cx - 1.5, cy - 1.5, 1.6, accent);
        }
        // Voice: a microphone.
        QUICK_VOICE => {
            g.stroke_rrect(cx - 3.5, cy - 8.0, 7.0, 11.0, 3.5, W, c);
            g.fill_circle(cx, cy - 4.0, 1.6, accent);
            g.line(cx - 6.5, cy - 1.0, cx - 6.5, cy + 0.5, W, c);
            g.line(cx + 6.5, cy - 1.0, cx + 6.5, cy + 0.5, W, c);
            g.line(cx - 6.5, cy + 0.5, cx - 3.0, cy + 5.0, W, c);
            g.line(cx + 6.5, cy + 0.5, cx + 3.0, cy + 5.0, W, c);
            g.line(cx - 3.0, cy + 5.0, cx + 3.0, cy + 5.0, W, c);
            g.line(cx, cy + 5.0, cx, cy + 8.0, W, c);
        }
        // Timer: a stopwatch.
        QUICK_TIMER => {
            g.stroke_circle(cx, cy + 1.5, 7.0, W, c);
            g.line(cx - 2.0, cy - 8.0, cx + 2.0, cy - 8.0, 1.8, c);
            g.line(cx, cy - 8.0, cx, cy - 5.5, W, c);
            g.line(cx, cy + 1.5, cx + 3.2, cy - 2.0, 1.8, accent);
            g.fill_circle(cx, cy + 1.5, 1.4, accent);
        }
        // Notes: a page with a pencil.
        QUICK_NOTES => {
            g.stroke_rrect(cx - 7.5, cy - 7.5, 12.0, 15.0, 2.2, W, c);
            g.line(cx - 4.5, cy - 3.5, cx + 1.0, cy - 3.5, 1.3, c);
            g.line(cx - 4.5, cy, cx - 0.5, cy, 1.3, c);
            g.line(cx + 7.0, cy - 5.0, cx + 0.5, cy + 6.0, 2.2, c);
            g.fill_circle(cx + 0.3, cy + 6.3, 1.3, accent);
        }
        // Lock: a padlock.
        QUICK_LOCK => {
            g.fill_rrect(cx - 6.5, cy - 1.5, 13.0, 9.5, 2.5, c);
            g.stroke_rrect(cx - 4.0, cy - 8.0, 8.0, 10.0, 4.0, 1.7, c);
            g.fill_circle(cx, cy + 3.0, 1.6, accent);
        }
        _ => {}
    }
}

fn tab_icon(g: &Gfx, i: usize, cx: f32, cy: f32, c: Rgba) {
    const W: f32 = 1.6;
    match i {
        // Home
        0 => {
            g.line(cx - 7.0, cy - 0.5, cx, cy - 6.5, W, c);
            g.line(cx, cy - 6.5, cx + 7.0, cy - 0.5, W, c);
            g.stroke_rrect(cx - 5.0, cy - 1.5, 10.0, 8.0, 1.5, W, c);
        }
        // Clipboard
        1 => {
            g.stroke_rrect(cx - 5.5, cy - 6.0, 11.0, 13.0, 2.5, W, c);
            g.line(cx - 2.0, cy - 6.5, cx + 2.0, cy - 6.5, 3.0, c);
            g.line(cx - 2.5, cy - 0.5, cx + 2.5, cy - 0.5, W, c);
            g.line(cx - 2.5, cy + 3.0, cx + 1.0, cy + 3.0, W, c);
        }
        // Shelf (tray)
        2 => {
            g.stroke_rrect(cx - 7.0, cy - 5.5, 14.0, 11.5, 3.0, W, c);
            g.line(cx - 7.0, cy + 0.5, cx - 3.0, cy + 0.5, W, c);
            g.line(cx + 3.0, cy + 0.5, cx + 7.0, cy + 0.5, W, c);
            g.line(cx - 3.0, cy + 0.5, cx - 1.5, cy + 2.8, W, c);
            g.line(cx + 3.0, cy + 0.5, cx + 1.5, cy + 2.8, W, c);
            g.line(cx - 1.5, cy + 2.8, cx + 1.5, cy + 2.8, W, c);
        }
        // System: a gauge, an arc with a needle.
        3 => {
            let (r, gy) = (7.0, cy + 3.0);
            let mut prev = (cx - r, gy);
            for k in 1..=12 {
                let t = std::f32::consts::PI * (1.0 + k as f32 / 12.0);
                let p = (cx + r * t.cos(), gy + r * t.sin());
                g.line(prev.0, prev.1, p.0, p.1, W, c);
                prev = p;
            }
            g.line(cx, gy, cx + 3.5, gy - 4.5, W, c);
            g.fill_circle(cx, gy, 1.6, c);
        }
        // Chat
        4 => {
            g.stroke_rrect(cx - 7.0, cy - 6.0, 14.0, 10.5, 4.5, W, c);
            g.line(cx - 3.5, cy + 4.5, cx - 5.0, cy + 7.2, W, c);
            g.line(cx - 5.0, cy + 7.2, cx - 0.5, cy + 4.6, W, c);
        }
        // Notes
        5 => {
            g.line(cx - 6.0, cy - 5.0, cx - 6.0, cy + 6.0, W, c);
            g.line(cx - 6.0, cy + 6.0, cx + 5.0, cy + 6.0, W, c);
            g.line(cx - 6.0, cy - 5.0, cx - 0.5, cy - 5.0, W, c);
            g.line(cx + 5.0, cy + 0.5, cx + 5.0, cy + 6.0, W, c);
            g.line(cx - 1.5, cy + 1.5, cx + 6.0, cy - 6.0, 2.0, c);
        }
        // Weather
        6 => {
            g.stroke_circle(cx, cy, 3.4, W, c);
            for k in 0..8 {
                let (sin, cos) = (k as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
                g.line(cx + cos * 6.0, cy + sin * 6.0, cx + cos * 7.6, cy + sin * 7.6, W, c);
            }
        }
        // Controls
        _ => sliders_icon(g, cx, cy, c),
    }
}

/// The settings button: a cogwheel (a ring with eight teeth).
fn gear_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_circle(cx, cy, 4.6, 2.3, c);
    for k in 0..8 {
        let t = k as f32 * std::f32::consts::TAU / 8.0;
        let (dx, dy) = (t.cos(), t.sin());
        g.line(cx + dx * 5.6, cy + dy * 5.6, cx + dx * 7.6, cy + dy * 7.6, 2.6, c);
    }
}

fn sliders_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.line(cx - 6.5, cy - 3.5, cx + 6.5, cy - 3.5, 1.6, c);
    g.line(cx - 6.5, cy + 3.5, cx + 6.5, cy + 3.5, 1.6, c);
    g.fill_circle(cx + 2.5, cy - 3.5, 2.6, c);
    g.fill_circle(cx - 2.5, cy + 3.5, 2.6, c);
}

fn pin_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.fill_circle(cx + 1.8, cy - 1.8, 4.2, c);
    g.line(cx - 1.2, cy + 1.2, cx - 5.8, cy + 5.8, 1.8, c);
}

/// Two crossing arrows.
fn shuffle_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    const W: f32 = 1.6;
    g.line(cx - 7.0, cy - 4.0, cx - 3.0, cy - 4.0, W, c);
    g.line(cx - 3.0, cy - 4.0, cx + 3.0, cy + 4.0, W, c);
    g.line(cx + 3.0, cy + 4.0, cx + 7.0, cy + 4.0, W, c);
    g.line(cx - 7.0, cy + 4.0, cx - 3.0, cy + 4.0, W, c);
    g.line(cx - 3.0, cy + 4.0, cx + 3.0, cy - 4.0, W, c);
    g.line(cx + 3.0, cy - 4.0, cx + 7.0, cy - 4.0, W, c);
    for y in [-4.0f32, 4.0] {
        g.line(cx + 7.0, cy + y, cx + 4.5, cy + y - 2.5, W, c);
        g.line(cx + 7.0, cy + y, cx + 4.5, cy + y + 2.5, W, c);
    }
}

/// A loop with an arrow; `mode` 1 (this track) adds a small "1".
fn repeat_icon(g: &Gfx, cx: f32, cy: f32, mode: u8, c: Rgba) {
    const W: f32 = 1.6;
    g.stroke_rrect(cx - 7.0, cy - 4.5, 14.0, 9.0, 3.5, W, c);
    // The arrowhead on the top edge, pointing right.
    g.line(cx + 1.0, cy - 4.5, cx - 1.5, cy - 7.0, W, c);
    g.line(cx + 1.0, cy - 4.5, cx - 1.5, cy - 2.0, W, c);
    if mode == 1 {
        g.fill_circle(cx + 7.0, cy + 4.5, 4.2, c);
        g.text(ws(w!("1")), Font::Hud, cx + 3.0, cy + 0.5, 8.0, 8.0, Rgba::hex(0x000000).alpha(c.a), Align::Center);
    }
}

fn skip_icon(g: &Gfx, cx: f32, cy: f32, dir: f32, c: Rgba) {
    g.line(cx - 4.0 * dir, cy - 5.0, cx + 3.0 * dir, cy, 2.2, c);
    g.line(cx + 3.0 * dir, cy, cx - 4.0 * dir, cy + 5.0, 2.2, c);
    g.line(cx + 6.0 * dir, cy - 5.0, cx + 6.0 * dir, cy + 5.0, 2.2, c);
}

fn speaker_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.fill_rrect(cx - 7.0, cy - 3.0, 5.0, 6.0, 1.0, c);
    g.line(cx - 2.5, cy - 3.0, cx + 1.5, cy - 6.0, 1.8, c);
    g.line(cx + 1.5, cy - 6.0, cx + 1.5, cy + 6.0, 1.8, c);
    g.line(cx + 1.5, cy + 6.0, cx - 2.5, cy + 3.0, 1.8, c);
    g.line(cx + 5.5, cy - 3.0, cx + 5.5, cy + 3.0, 1.6, c);
}

// ----------------------------------------------------------------- HUD ----

fn draw_hud(g: &Gfx, s: &State, stats: &Stats, status: &str) {
    let (x, y) = (8.0, WIN_H - 88.0);
    g.fill_rrect(x, y, 236.0, 80.0, 8.0, BLACK.alpha(0.78));
    let c = Rgba::hex(0x7dff9b);
    let line = |i: usize, b: &WBuf| g.text(b.as_slice(), Font::Hud, x + 10.0, y + 7.0 + i as f32 * 16.5, 220.0, 16.0, c, Align::Left);
    let mut b = WBuf::new();
    let _ = write!(b, "WS {:5.1} MB   private {:5.1} MB", stats.working_set_mb, stats.private_mb);
    line(0, &b);
    let mut b = WBuf::new();
    let _ = write!(b, "CPU {:4.1} %    frame {:5.2} ms", stats.cpu, stats.frame_ms);
    line(1, &b);
    let mut b = WBuf::new();
    let p = g.prof.get();
    let _ = write!(b, "geo {:.2} draw {:.2} flush {:.2} pres {:.2}", p[0], p[1], p[2], p[3]);
    line(2, &b);
    let mut b = WBuf::new();
    let _ = write!(b, "{:?}/{:?} {}", s.mode, s.pill, status);
    line(3, &b);
}
