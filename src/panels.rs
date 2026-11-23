//! Drawing and hit-testing for the Controls tab, the Clipboard tab and the
//! command bar. (The Home tab and the pill live in `ui`.)

use crate::anim::{clamp01, lerp};
use crate::control::{Kind, Power};
use crate::render::{ws, Align, Font, Gfx, Rgba, WBuf};
use std::fmt::Write;
use crate::state::*;
use crate::theme::{self, Palette};
use crate::ui::Hit;
use windows::core::{w, PCWSTR};

const X0: f32 = CX - PANEL_W * 0.5 + 16.0;
const WIDTH: f32 = PANEL_W - 32.0;
const RED: Rgba = Rgba::hex(0xff453a);

// ------------------------------------------------------------ control centre --

const POWER: [(Power, PCWSTR); 4] = [(Power::Lock, w!("Lock")), (Power::Sleep, w!("Sleep")), (Power::Restart, w!("Restart")), (Power::ShutDown, w!("Shut down"))];
const TILE_HITS: [Hit; 4] = [Hit::Radio(Kind::WiFi), Hit::Radio(Kind::Bluetooth), Hit::Vpn, Hit::Theme];

/// Items placed on the panel: (item index, where).
pub type Placed = Vec<(usize, crate::ui::Area)>;

/// Where the Controls tab's tiles and power buttons go: the ones shown share
/// the row width, and take the height of a row that is hidden.
/// Returns (tile index, area) and (power index, area) in panel content units.
pub fn controls_layout(s: &State) -> (Placed, Placed) {
    use crate::ui::Area;
    let (pw, ph) = s.panel_local();
    let (x0, width) = (CX - pw * 0.5 + 16.0, pw - 32.0);
    let (top, bottom) = (56.0, ph - 40.0);
    // The shown ones, in the order Ctrl + drag left them.
    let tiles: Vec<usize> = s.order_of(Group::CtlTile).iter().copied().filter(|i| s.look.tiles.contains(i)).collect();
    let power: Vec<usize> = s.order_of(Group::CtlPower).iter().copied().filter(|i| s.look.power.contains(i)).collect();
    let (tiles, power) = (&tiles, &power);
    let avail = bottom - top;
    const GAP: f32 = 9.0;
    let (tile_h, button_h) = match (tiles.is_empty(), power.is_empty()) {
        (false, false) => (avail - 76.0 - 8.0, 76.0),
        (false, true) => (avail, 0.0),
        (true, false) => (0.0, avail.min(120.0)),
        (true, true) => (0.0, 0.0),
    };
    let row = |items: &[usize], y: f32, h: f32| -> Vec<(usize, Area)> {
        let n = items.len().max(1) as f32;
        let w = (width - (n - 1.0) * GAP) / n;
        items.iter().enumerate().map(|(k, &i)| (i, Area { x: x0 + k as f32 * (w + GAP), y, w, h })).collect()
    };
    // Buttons alone sit in the middle of the space.
    let button_y = if tiles.is_empty() { top + (avail - button_h) * 0.5 } else { top + tile_h + 8.0 };
    (row(tiles, top, tile_h), row(power, button_y, button_h))
}

/// The Controls tiles or buttons as laid out: (id, slot).
pub fn controls_items(s: &State, group: Group) -> Vec<(usize, crate::ui::Area)> {
    let (tiles, buttons) = controls_layout(s);
    if group == Group::CtlTile { tiles } else { buttons }
}

/// Each item at the rectangle it is drawn in (gliding offsets, the held one
/// at the pointer), the held one last so it is drawn on top; and its lift.
fn grip_placed(s: &State, group: Group, placed: Placed) -> Vec<(usize, crate::ui::Area, f32)> {
    let mut out: Vec<(usize, crate::ui::Area, f32)> = placed.into_iter().map(|(i, slot)| {
        let (r, lift) = s.grip_rect(group, i, slot);
        (i, r, lift)
    }).collect();
    out.sort_by_key(|(i, _, _)| s.grip_on_top(group, *i));
    out
}

/// (x, y in panel content units.)
pub fn controls_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    let (tiles, buttons) = controls_layout(s);
    let inside = |r: &crate::ui::Area| (r.x..=r.x + r.w).contains(&x) && (r.y..=r.y + r.h).contains(&y);
    if let Some((i, _)) = tiles.iter().find(|(_, r)| inside(r)) {
        return Some(TILE_HITS[*i]);
    }
    buttons.iter().find(|(_, r)| inside(r)).map(|(i, _)| Hit::Power(POWER[*i].0))
}

// --------------------------------------------------------------- system --

const SYS_Y: f32 = 56.0;
const SYS_H: f32 = 250.0;
const SYS_GAP: f32 = 8.0;
/// Colours of the four measures: CPU, memory, GPU, network.
const SYS_COLOURS: [Rgba; 4] = [Rgba::hex(0x5ac8fa), Rgba::hex(0xbf5af2), Rgba::hex(0x30d158), Rgba::hex(0xff9f0a)];

/// System card slot `k` (the tab's units).
fn sys_slot(k: usize) -> crate::ui::Area {
    let w = (WIDTH - 3.0 * SYS_GAP) / 4.0;
    crate::ui::Area { x: X0 + k as f32 * (w + SYS_GAP), y: SYS_Y, w, h: SYS_H }
}

/// The System cards as laid out: (card id, slot).
pub fn sys_items(s: &State) -> Vec<(usize, crate::ui::Area)> {
    s.order_of(Group::Sys).iter().enumerate().map(|(k, &id)| (id, sys_slot(k))).collect()
}

/// System tab: four cards (CPU, memory, GPU, network), each with its value now
/// and a bar chart of the last minute.
pub fn draw_system(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    use crate::sysstat::{gb_text, speed_text, HISTORY};
    let w = (WIDTH - 3.0 * SYS_GAP) / 4.0;
    let st = s.stats.as_ref();
    let pct = |v: f32| format!("{:.0}%", v * 100.0);
    // Slots in order, the one held with Ctrl + drag last (drawn on top).
    let order = s.order_of(Group::Sys);
    let mut seq = [0usize, 1, 2, 3];
    seq.sort_by_key(|&k| s.grip_on_top(Group::Sys, order[k]));
    for &k in &seq {
        let i = order[k];
        let (r, lift) = s.grip_rect(Group::Sys, i, sys_slot(k));
        let x = r.x;
        g.card(x, SYS_Y, w, SYS_H, 16.0, pal.card.mix(pal.text, 0.04 * lift).alpha(a));
        if lift > 0.0 {
            g.stroke_rrect(x + 0.5, SYS_Y + 0.5, w - 1.0, SYS_H - 1.0, 16.0, 1.4, s.accent.alpha(a * lift));
        }
        let colour = SYS_COLOURS[i];
        g.fill_circle(x + 20.0, SYS_Y + 24.0, 4.0, colour.alpha(a));
        let title = [w!("CPU"), w!("Memory"), w!("GPU"), w!("Network")][i];
        g.text(ws(title), Font::Label, x + 30.0, SYS_Y + 12.0, w - 40.0, 24.0, pal.text2.alpha(a), Align::Left);
        // Value now, and one or two lines under it.
        let (big, line1, line2): (String, String, String) = match (i, st) {
            (_, None) => ("\u{2013}".into(), "Measuring\u{2026}".into(), String::new()),
            (0, Some(t)) => (pct(t.cpu), "All cores".into(), String::new()),
            (1, Some(t)) => (pct(t.mem()), format!("{} of {}", gb_text(t.mem_used), gb_text(t.mem_total)), "in use".into()),
            (2, Some(t)) => match t.gpu {
                Some(load) => (
                    pct(load),
                    t.gpu_temp.map_or(String::new(), |c| format!("{c:.0}\u{00B0}C")),
                    t.vram.map_or(String::new(), |(used, total)| format!("{} of {} VRAM", gb_text(used), gb_text(total))),
                ),
                None => ("\u{2013}".into(), "Needs an NVIDIA".into(), "graphics card".into()),
            },
            (_, Some(t)) => (speed_text(t.down), "\u{2193} download".into(), format!("\u{2191} {} upload", speed_text(t.up))),
        };
        let mut b = WBuf::new();
        let _ = write!(b, "{big}");
        let font = if i == 3 { Font::Title } else { Font::Big };
        g.text(b.as_slice(), font, x + 16.0, SYS_Y + 40.0, w - 24.0, 48.0, pal.text.alpha(a), Align::Left);
        for (k, line) in [line1, line2].iter().enumerate() {
            let mut b = WBuf::new();
            let _ = write!(b, "{line}");
            g.text(b.as_slice(), Font::Small, x + 16.0, SYS_Y + 88.0 + k as f32 * 18.0, w - 24.0, 18.0, pal.text2.alpha(a), Align::Left);
        }

        // The last minute as thin bars, newest on the right.
        let (cx0, cx1, cy0, cy1) = (x + 14.0, x + w - 14.0, SYS_Y + 140.0, SYS_Y + SYS_H - 16.0);
        g.fill_rrect(cx0 - 4.0, cy0 - 4.0, cx1 - cx0 + 8.0, cy1 - cy0 + 8.0, 10.0, pal.island.mix(pal.card, 0.4).alpha(a));
        let Some(t) = st else { continue };
        let hist = [&t.cpu_hist, &t.mem_hist, &t.gpu_hist, &t.net_hist][i];
        // Network has no fixed top: scale to the busiest second (at least 100 KB/s).
        let top = if i == 3 { hist.iter().cloned().fold(100e3, f32::max) } else { 1.0 };
        let step = (cx1 - cx0) / HISTORY as f32;
        for (k, v) in hist.iter().enumerate().skip(HISTORY - t.filled) {
            let h = (cy1 - cy0) * clamp01(v / top);
            if h < 0.5 {
                continue;
            }
            let bx = cx0 + k as f32 * step;
            let newest = k + 1 == HISTORY;
            g.fill_rrect(bx, cy1 - h, step + 0.3, h, 0.0, colour.alpha(a * if newest { 1.0 } else { 0.5 }));
        }
    }
}

pub fn draw_controls(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    let all: [(PCWSTR, Option<bool>, u8); 4] = [(w!("Wi-Fi"), s.radios.wifi, 0), (w!("Bluetooth"), s.radios.bluetooth, 1), (w!("VPN"), Some(s.vpn_on), 3), (w!("Dark theme"), Some(s.dark), 2)];
    let (tiles, buttons) = controls_layout(s);
    for (i, area, lift) in grip_placed(s, Group::CtlTile, tiles) {
        let (label, state, icon) = all[i];
        let (x, tile_y, tile_w, tile_h) = (area.x, area.y, area.w, area.h);
        g.card(x, tile_y, tile_w, tile_h, 16.0, pal.card.mix(pal.text, 0.04 * lift).alpha(a));
        if lift > 0.0 {
            g.stroke_rrect(x + 0.5, tile_y + 0.5, tile_w - 1.0, tile_h - 1.0, 16.0, 1.4, s.accent.alpha(a * lift));
        }
        let on = state == Some(true);
        let (cx, cy) = (x + 36.0, tile_y + 36.0);
        g.fill_circle(cx, cy, 20.0, if on { theme::accent() } else { pal.raised }.alpha(a));
        let ink = if on { Rgba::hex(0x0b0b12) } else { pal.text2 }.alpha(a);
        match icon {
            0 => {
                for (k, half) in [9.0, 6.0, 3.0].into_iter().enumerate() {
                    g.line(cx - half, cy - 5.0 + k as f32 * 4.0, cx + half, cy - 5.0 + k as f32 * 4.0, 2.0, ink);
                }
                g.fill_circle(cx, cy + 8.0, 1.6, ink);
            }
            1 => {
                g.line(cx, cy - 9.0, cx, cy + 9.0, 1.8, ink);
                g.line(cx, cy - 9.0, cx + 5.0, cy - 4.5, 1.8, ink);
                g.line(cx + 5.0, cy - 4.5, cx - 5.0, cy + 4.5, 1.8, ink);
                g.line(cx, cy + 9.0, cx + 5.0, cy + 4.5, 1.8, ink);
                g.line(cx + 5.0, cy + 4.5, cx - 5.0, cy - 4.5, 1.8, ink);
            }
            3 => {
                // Shield.
                for (x0, y0, x1, y1) in [(-7.0, -8.0, 7.0, -8.0), (7.0, -8.0, 7.0, 1.0), (7.0, 1.0, 0.0, 9.0), (0.0, 9.0, -7.0, 1.0), (-7.0, 1.0, -7.0, -8.0)] {
                    g.line(cx + x0, cy + y0, cx + x1, cy + y1, 1.8, ink);
                }
            }
            _ => {
                // Crescent: a disc with a bite taken out in the circle's own colour.
                g.fill_circle(cx, cy, 8.5, ink);
                g.fill_circle(cx + 4.5, cy - 3.5, 7.0, if on { theme::accent() } else { pal.raised }.alpha(a));
            }
        }
        g.text(ws(label), Font::Title, x + 16.0, tile_y + tile_h - 56.0, tile_w - 32.0, 24.0, pal.text.alpha(a), Align::Left);
        let status = match state {
            _ if icon == 3 => &s.vpn_text[..],
            Some(true) => ws(w!("On")),
            Some(false) => ws(w!("Off")),
            None => ws(w!("Not available")),
        };
        g.text(status, Font::Body, x + 16.0, tile_y + tile_h - 34.0, tile_w - 32.0, 20.0, pal.text2.alpha(a), Align::Left);
    }

    for (i, area, lift) in grip_placed(s, Group::CtlPower, buttons) {
        let (action, label) = &POWER[i];
        let (x, button_y, button_w, button_h) = (area.x, area.y, area.w, area.h);
        let waiting = s.confirm == Some(*action);
        g.card(x, button_y, button_w, button_h, 14.0, if waiting { RED.alpha(a * 0.9) } else { pal.card.alpha(a) });
        if lift > 0.0 {
            g.stroke_rrect(x + 0.5, button_y + 0.5, button_w - 1.0, button_h - 1.0, 14.0, 1.4, s.accent.alpha(a * lift));
        }
        let ink = if waiting { Rgba::hex(0xffffff) } else { pal.text }.alpha(a);
        // Icon over label, as a block in the middle of the button.
        let (cx, cy) = (x + button_w * 0.5, button_y + button_h * 0.5 - 12.0);
        match action {
            Power::Lock => {
                g.stroke_circle(cx, cy - 4.0, 5.0, 1.8, ink);
                g.fill_rrect(cx - 8.0, cy - 2.0, 16.0, 12.0, 3.0, ink);
            }
            Power::Sleep => {
                g.fill_circle(cx, cy, 8.5, ink);
                g.fill_circle(cx + 4.5, cy - 3.5, 7.0, if waiting { RED } else { pal.card }.alpha(a));
            }
            Power::Restart => {
                // A clockwise arrow round a circle, open at the top left.
                let r = 8.0;
                let at = |deg: f32| {
                    let t = deg.to_radians();
                    (cx + r * t.cos(), cy + r * t.sin())
                };
                let mut prev = at(-60.0);
                for k in 1..=24 {
                    let p = at(-60.0 + 290.0 * k as f32 / 24.0);
                    g.line(prev.0, prev.1, p.0, p.1, 1.8, ink);
                    prev = p;
                }
                // The head at the end (230°), pointing on along the circle.
                let t = 230f32.to_radians();
                let (dx, dy) = (-t.sin(), t.cos());
                let (tx, ty) = (prev.0 + dx * 1.5, prev.1 + dy * 1.5);
                for side in [-1.0f32, 1.0] {
                    let (c, s) = ((150f32).to_radians().cos(), (150f32 * side).to_radians().sin());
                    let (bx, by) = (dx * c - dy * s, dx * s + dy * c);
                    g.line(tx, ty, tx + bx * 5.0, ty + by * 5.0, 1.8, ink);
                }
            }
            Power::ShutDown => {
                g.stroke_circle(cx, cy, 8.0, 1.8, ink);
                g.fill_rrect(cx - 3.5, cy - 12.0, 7.0, 8.0, 0.0, if waiting { RED } else { pal.card }.alpha(a));
                g.line(cx, cy - 11.0, cx, cy - 1.0, 1.8, ink);
            }
        }
        let text = if waiting { w!("Press again") } else { *label };
        g.text(ws(text), Font::Label, x, cy + 18.0, button_w, 22.0, ink, Align::Center);
        if waiting {
            // The countdown: an underline that runs out.
            let left = clamp01(s.confirm_left / CONFIRM_SECS);
            g.line(x + 16.0, button_y + button_h - 8.0, x + 16.0 + (button_w - 32.0) * left, button_y + button_h - 8.0, 2.5, Rgba::hex(0xffffff).alpha(a));
        }
    }
}

// -------------------------------------------------------------------- weather --

const SKY_W: f32 = 236.0;
const CARD_Y: f32 = 56.0;
const CARD_H: f32 = 250.0;
const SIDE_X: f32 = X0 + SKY_W + 9.0;
const FORECAST_W: f32 = WIDTH - SKY_W - 9.0;
/// The view switcher (Hourly / 7 days / Details) over the right-hand card.
const SEG_Y: f32 = CARD_Y;
const SEG_H: f32 = 28.0;
const SEG_W: f32 = FORECAST_W / 3.0;
const BODY_Y: f32 = SEG_Y + SEG_H + 8.0;
const BODY_H: f32 = CARD_Y + CARD_H - BODY_Y;
/// Hourly graph: first and last point.
const GX0: f32 = SIDE_X + 16.0;
const GX1: f32 = SIDE_X + FORECAST_W - 16.0;
/// Day rows.
const DAY_Y: f32 = BODY_Y + 9.0;
const DAY_H: f32 = 28.0;
/// Detail tiles (3 x 2).
const TILE_GAP: f32 = 7.0;
const WX_TILE_W: f32 = (FORECAST_W - 12.0 - 2.0 * TILE_GAP) / 3.0;
const WX_TILE_H: f32 = (BODY_H - 12.0 - TILE_GAP) / 2.0;
const WEEKDAYS: [PCWSTR; 7] = [w!("Mon"), w!("Tue"), w!("Wed"), w!("Thu"), w!("Fri"), w!("Sat"), w!("Sun")];
const WEEKDAYS_LONG: [PCWSTR; 7] = [w!("Monday"), w!("Tuesday"), w!("Wednesday"), w!("Thursday"), w!("Friday"), w!("Saturday"), w!("Sunday")];
const VIEWS: [PCWSTR; 3] = [w!("Hourly"), w!("7 days"), w!("Details")];
const RAIN_BLUE: Rgba = Rgba::hex(0x64b5ff);
/// The "Open settings file" button of a switched-off feature.
const SETUP: (f32, f32, f32, f32) = (CX - 90.0, 214.0, 180.0, 34.0);

/// What the sky card shows: now, an hour of the graph (under the pointer) or a chosen day.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Focus {
    #[default]
    Now,
    Hour(usize),
    Day(usize),
}

/// The Weather tab's view and the eased values of its interaction (all in
/// window dp / seconds of `State::sky_phase`).
#[derive(Clone, Debug)]
pub struct Wx {
    /// 0 hourly, 1 days, 2 details; when it was picked (for the entry animations).
    pub view: usize,
    pub since: f32,
    /// The switcher's highlight, gliding between views.
    pub seg: f32,
    /// Hour under the pointer on the graph (eased index) and how visible the scrubber is.
    pub scrub: f32,
    pub scrub_a: f32,
    /// A day picked in the 7-day list (shown on the sky card).
    pub day: Option<usize>,
    /// The sky card's subject, the one fading out, and the cross-fade (0..1).
    pub focus: Focus,
    pub prev: Focus,
    pub mix: f32,
    /// Parallax of the sky's layers towards the pointer (-1..1 each).
    pub tilt: (f32, f32),
    /// A click on the sky: where and when (rings, pushed drops, a bolt in a storm).
    pub poke: Option<(f32, f32, f32)>,
    /// Hover lift of each detail tile.
    pub tiles: [f32; 6],
    /// Whether the right-hand card needs drawing every frame.
    pub side_live: bool,
    /// Something is being interacted with or is in transition: full frame rate
    /// (otherwise the sky alone moves, at half rate).
    pub busy: bool,
    /// Thunder to play (delay s, strength 0..1), taken by the app; the last
    /// lightning cycle that got its thunder.
    pub thunder: Option<(f32, f32)>,
    pub last_strike: u32,
}

impl Default for Wx {
    fn default() -> Self {
        Self { view: 0, since: -10.0, seg: 0.0, scrub: 0.0, scrub_a: 0.0, day: None, focus: Focus::Now, prev: Focus::Now, mix: 1.0, tilt: (0.0, 0.0), poke: None, tiles: [0.0; 6], side_live: true, busy: true, thunder: None, last_strike: u32::MAX }
    }
}

fn inside(x: f32, y: f32, rx: f32, ry: f32, rw: f32, rh: f32) -> bool {
    (rx..=rx + rw).contains(&x) && (ry..=ry + rh).contains(&y)
}

fn hour_step(n: usize) -> f32 {
    (GX1 - GX0) / (n.max(2) - 1) as f32
}

/// The hour of the graph under window x.
fn hour_at(n: usize, x: f32) -> usize {
    (((x - GX0) / hour_step(n)).round().max(0.0) as usize).min(n.saturating_sub(1))
}

fn tile_at(i: usize) -> (f32, f32) {
    (SIDE_X + 6.0 + (i % 3) as f32 * (WX_TILE_W + TILE_GAP), BODY_Y + 6.0 + (i / 3) as f32 * (WX_TILE_H + TILE_GAP))
}

/// Where the two Weather cards sit, as shifts from their usual places (sky
/// left, forecast right): both 0, or swapped by Ctrl + drag.
pub fn wx_shift(s: &State) -> (f32, f32) {
    if s.order_of(Group::WxCard).first() == Some(&1) { (FORECAST_W + 9.0, -(SKY_W + 9.0)) } else { (0.0, 0.0) }
}

/// Slot of view / detail tile `id` (its place in the order).
fn wx_slot(s: &State, group: Group, id: usize) -> usize {
    s.order_of(group).iter().position(|&i| i == id).unwrap_or(id)
}

/// The Weather tab's movable items as laid out: (id, slot) in the tab's units.
pub fn weather_items(s: &State, group: Group) -> Vec<(usize, crate::ui::Area)> {
    use crate::ui::Area;
    if s.weather.is_none() {
        return Vec::new();
    }
    let (_, side) = wx_shift(s);
    let order = s.order_of(group);
    match group {
        Group::WxCard => {
            let width = |id: usize| if id == 0 { SKY_W } else { FORECAST_W };
            let left = width(order[0]);
            vec![(order[0], Area { x: X0, y: CARD_Y, w: left, h: CARD_H }), (order[1], Area { x: X0 + left + 9.0, y: CARD_Y, w: width(order[1]), h: CARD_H })]
        }
        Group::WxView => order.iter().enumerate().map(|(k, &id)| (id, Area { x: SIDE_X + side + k as f32 * SEG_W, y: SEG_Y, w: SEG_W, h: SEG_H })).collect(),
        // The tiles only while the Details view is open.
        Group::WxDetail if s.wx.view == 2 => order.iter().enumerate().map(|(k, &id)| {
            let (tx, ty) = tile_at(k);
            (id, Area { x: tx + side, y: ty, w: WX_TILE_W, h: WX_TILE_H })
        }).collect(),
        _ => Vec::new(),
    }
}

pub fn weather_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    let Some(w) = &s.weather else {
        let (bx, by, bw, bh) = SETUP;
        return inside(x, y, bx, by, bw, bh).then_some(Hit::OpenConfig);
    };
    let (sky, side) = wx_shift(s);
    if inside(x, y, X0 + sky, CARD_Y, SKY_W, CARD_H) {
        return Some(Hit::WxSky);
    }
    // The forecast card's own units.
    let x = x - side;
    if inside(x, y, SIDE_X, SEG_Y, FORECAST_W, SEG_H) {
        let slot = (((x - SIDE_X) / SEG_W) as usize).min(2);
        return Some(Hit::WxView(s.order_of(Group::WxView)[slot]));
    }
    if s.wx.view == 1 && inside(x, y, SIDE_X, DAY_Y, FORECAST_W, DAY_H * 7.0) {
        let row = ((y - DAY_Y) / DAY_H) as usize;
        return (row < w.days.len().min(7)).then_some(Hit::WxDay(row));
    }
    None
}

/// The automatic lightning: one strike in each 5.5 s cycle, at a moment of
/// its own. (Age of the flash in s, cycle number) while it lights the sky.
fn strike(t: f32) -> Option<(f32, u32)> {
    const PERIOD: f32 = 5.5;
    let cycle = (t / PERIOD).floor().max(0.0);
    let since = t - cycle * PERIOD - scatter(cycle as u32, 40) * 3.0;
    (0.0..0.55).contains(&since).then_some((since, cycle as u32))
}

/// The sky card shows a thunderstorm.
pub fn weather_stormy(s: &State) -> bool {
    s.weather.as_ref().is_some_and(|w| scene_of(w, s.wx.focus).sky == crate::weather::Sky::Storm)
}

/// What the sky on the card sounds like: rain by its strength, wind by its
/// speed, birds on fair days, crickets on fair, mild nights.
pub fn weather_ambience(s: &State) -> crate::sound::Ambience {
    use crate::weather::Sky;
    let Some(w) = &s.weather else { return crate::sound::Ambience::SILENT };
    let sc = scene_of(w, s.wx.focus);
    let fair = matches!(sc.sky, Sky::Clear | Sky::Partly);
    let rain = match sc.sky {
        Sky::Rain => 0.65,
        Sky::Storm => 1.0,
        _ => 0.0,
    };
    let calm_air = match sc.sky {
        Sky::Snow => 0.3,
        Sky::Fog | Sky::Cloudy => 0.12,
        _ => 0.05,
    };
    let wind = ((sc.wind / 45.0).min(1.0) * 0.8 + calm_air).min(1.0);
    let day = sc.light >= 0.5;
    crate::sound::Ambience {
        rain,
        wind,
        birds: if fair && day { if sc.sky == Sky::Clear { 1.0 } else { 0.6 } } else { 0.0 },
        crickets: if fair && !day && w.temp > 4.0 { 1.0 } else { 0.0 },
    }
}

/// Where the Weather tab animates every frame: the sky card, and the
/// right-hand card while something there moves.
pub fn weather_live_rect(s: &State) -> crate::render::Rect {
    use crate::render::Rect;
    if s.weather.is_none() {
        return Rect::EMPTY;
    }
    // (With the cards swapped, or one moving, the sky is not on the left: the whole width.)
    let moved = wx_shift(s).0 != 0.0 || s.grip_held.is_some_and(|g| g.group == Group::WxCard) || !s.home_settled();
    let right = if s.wx.side_live || moved { X0 + WIDTH } else { X0 + SKY_W };
    // Content units -> window dp (the panel may be drawn scaled and the tab centred).
    let dy = s.fixed_dy();
    let (x0, y0) = s.panel_to_window(X0 - 3.0, CARD_Y - 3.0 + dy);
    let (x1, y1) = s.panel_to_window(right + 3.0, CARD_Y + CARD_H + 3.0 + dy);
    Rect::new(x0 - 1.0, y0 - 1.0, x1 + 1.0, y1 + 1.0)
}

/// Advances the Weather tab by `dt` seconds (sky clock, pointer-driven easing).
/// `true` while the tab is on screen and so animating.
pub fn weather_step(s: &mut State, dt: f32) -> bool {
    // (The settings window is a window of its own: the tab keeps moving while it is open.)
    if s.mode != Mode::Expanded || s.tab != 6 {
        return false;
    }
    let Some(w) = &s.weather else { return false };
    s.sky_phase += dt;
    let t = s.sky_phase;
    // The pointer in the tab's own units.
    let (x, y) = s.to_panel(s.cursor.0, s.cursor.1);
    let y = y - s.fixed_dy();
    // Each card's own units (the cards may have swapped places).
    let (sky, side) = wx_shift(s);
    let (sx, x) = (x - sky, x - side);
    let view_slot = wx_slot(s, Group::WxView, s.wx.view) as f32;
    let tile_slots: [usize; 6] = std::array::from_fn(|i| wx_slot(s, Group::WxDetail, i));
    let wx = &mut s.wx;
    let follow = |v: &mut f32, to: f32, rate: f32| *v += (to - *v) * (1.0 - (-rate * dt).exp());
    follow(&mut wx.seg, view_slot, 16.0);

    // Scrubbing the hourly graph.
    let n = w.hours.len();
    let on_graph = wx.view == 0 && n > 1 && inside(x, y, SIDE_X, BODY_Y, FORECAST_W, BODY_H);
    let hovered = on_graph.then(|| hour_at(n, x));
    if let Some(i) = hovered {
        if wx.scrub_a < 0.02 {
            wx.scrub = i as f32;
        }
        follow(&mut wx.scrub, i as f32, 20.0);
        follow(&mut wx.scrub_a, 1.0, 14.0);
    } else {
        follow(&mut wx.scrub_a, 0.0, 10.0);
    }

    // Detail tiles lift under the pointer.
    for (i, lift) in wx.tiles.iter_mut().enumerate() {
        let (tx, ty) = tile_at(tile_slots[i]);
        let on = wx.view == 2 && inside(x, y, tx, ty, WX_TILE_W, WX_TILE_H);
        follow(lift, on as u8 as f32, 12.0);
    }

    // The sky leans a little towards the pointer.
    let target = if inside(sx, y, X0, CARD_Y, SKY_W, CARD_H) { (((sx - X0) / SKY_W - 0.5) * 2.0, ((y - CARD_Y) / CARD_H - 0.5) * 2.0) } else { (0.0, 0.0) };
    follow(&mut wx.tilt.0, target.0, 3.5);
    follow(&mut wx.tilt.1, target.1, 3.5);

    // What the sky card is about.
    let focus = match (hovered, wx.day) {
        (Some(i), _) => Focus::Hour(i),
        (None, Some(d)) if d < w.days.len() => Focus::Day(d),
        _ => Focus::Now,
    };
    if focus != wx.focus {
        // Keep whichever of the two was more visible as the one fading out.
        if wx.mix >= 0.5 {
            wx.prev = wx.focus;
        }
        wx.focus = focus;
        wx.mix = 0.0;
    }
    wx.mix = (wx.mix + dt * 5.0).min(1.0);
    if wx.poke.is_some_and(|(_, _, at)| t - at > 1.6) {
        wx.poke = None;
    }
    // Each flash in a storm is followed by its thunder, from near or far.
    if scene_of(w, wx.focus).sky == crate::weather::Sky::Storm {
        if let Some((age, cycle)) = strike(t) {
            if cycle != wx.last_strike {
                wx.last_strike = cycle;
                let far = scatter(cycle, 44);
                wx.thunder = Some(((0.25 + far * 1.8 - age).max(0.0), 1.0 - far * 0.7));
            }
        }
    }

    // The right-hand card is still at rest: it moves while it enters, while the
    // graph is scrubbed and while a detail tile is under the pointer (or easing).
    let entering = t - wx.since < 1.6;
    let gliding = (wx.seg - wx.view as f32).abs() > 0.002;
    let tiles = wx.tiles.iter().any(|&lift| lift > 0.002);
    wx.side_live = entering || gliding || tiles || wx.scrub_a > 0.002;
    let leaning = (wx.tilt.0 - target.0).abs() + (wx.tilt.1 - target.1).abs() > 0.004;
    wx.busy = wx.side_live || leaning || wx.mix < 1.0 || wx.poke.is_some();
    true
}

/// A card saying why a network feature shows nothing, with a way to the settings file.
fn setup_card(g: &Gfx, title: PCWSTR, note: &[u16], pal: &Palette, a: f32) {
    g.card(X0, CARD_Y, WIDTH, CARD_H, 16.0, pal.card.alpha(a));
    g.text(ws(title), Font::Big, X0, 126.0, WIDTH, 30.0, pal.text.alpha(a), Align::Center);
    g.text(note, Font::Body, X0 + 40.0, 160.0, WIDTH - 80.0, 44.0, pal.text2.alpha(a), Align::Center);
    let (bx, by, bw, bh) = SETUP;
    g.fill_rrect(bx, by, bw, bh, bh * 0.5, pal.raised.alpha(a));
    g.text(ws(w!("Open settings")), Font::Label, bx, by, bw, bh, pal.text.alpha(a), Align::Center);
}

/// A cheap repeatable 0..1 value per index (particle positions).
fn scatter(i: u32, salt: u32) -> f32 {
    let mut v = i.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    (v & 0xffff) as f32 / 65535.0
}

const WHITE: Rgba = Rgba::hex(0xffffff);
const BLACK: Rgba = Rgba::hex(0x000000);

fn ease_out(t: f32) -> f32 {
    crate::anim::ease_out_cubic(clamp01(t))
}

/// Springy ease that overshoots a little before settling.
fn ease_back(t: f32) -> f32 {
    let t = clamp01(t);
    1.0 + 2.5 * (t - 1.0).powi(3) + 1.5 * (t - 1.0).powi(2)
}

/// Colour of a temperature (°C): violet in frost, through blue, green and yellow, to orange heat.
fn temp_colour(t: f32) -> Rgba {
    const STOPS: [(f32, u32); 6] = [(-15.0, 0x9c86ff), (-5.0, 0x5e8bff), (5.0, 0x5ccbe8), (14.0, 0x86dc8a), (22.0, 0xffd34a), (32.0, 0xff7a3d)];
    if t <= STOPS[0].0 {
        return Rgba::hex(STOPS[0].1);
    }
    for pair in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            return Rgba::hex(c0).mix(Rgba::hex(c1), (t - t0) / (t1 - t0));
        }
    }
    Rgba::hex(STOPS[STOPS.len() - 1].1)
}

fn air_colour(aqi: f32) -> Rgba {
    Rgba::hex(match aqi as u32 {
        0..=20 => 0x50c878,
        21..=40 => 0xb8d94a,
        41..=60 => 0xffd84a,
        61..=80 => 0xff9f40,
        81..=100 => 0xff5b4a,
        _ => 0xb36bff,
    })
}

/// An arc of radius `r` from angle `a0` to `a1` (radians, clockwise on screen, 0 = east).
#[allow(clippy::too_many_arguments)]
fn arc(g: &Gfx, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, width: f32, c: Rgba) {
    use crate::render::Seg;
    if (a1 - a0).abs() < 0.01 || c.a <= 0.003 {
        return;
    }
    let steps = (((a1 - a0).abs() * r / 3.0).ceil() as usize).clamp(3, 48);
    let mut segs = [Seg::Line((0.0, 0.0)); 48];
    for (k, seg) in segs.iter_mut().enumerate().take(steps) {
        let angle = a0 + (a1 - a0) * (k + 1) as f32 / steps as f32;
        *seg = Seg::Line((cx + r * angle.cos(), cy + r * angle.sin()));
    }
    if let Some(path) = g.path((cx + r * a0.cos(), cy + r * a0.sin()), &segs[..steps], false) {
        g.stroke_path(&path, width, c);
    }
}

/// A puffy cloud about 55 dp wide at `k` = 1, flat underneath.
fn puff(g: &Gfx, cx: f32, cy: f32, k: f32, c: Rgba) {
    g.fill_circle(cx - 16.0 * k, cy + 4.0 * k, 10.0 * k, c);
    g.fill_circle(cx - 3.0 * k, cy - 4.0 * k, 14.0 * k, c);
    g.fill_circle(cx + 11.0 * k, cy - 0.5 * k, 11.0 * k, c);
    g.fill_circle(cx + 22.0 * k, cy + 6.0 * k, 7.5 * k, c);
    g.fill_rrect(cx - 26.0 * k, cy + 3.0 * k, 55.0 * k, 10.5 * k, 5.25 * k, c);
}

/// What one sky scene looks like: the cover, how light it is (0 night .. 1
/// day), how golden (sunrise / sunset), the wind, and where along its arc the
/// sun or moon is (0..1).
#[derive(Clone, Copy, PartialEq)]
struct Scene {
    sky: crate::weather::Sky,
    light: f32,
    golden: f32,
    wind: f32,
    arc: f32,
}

fn scene_of(w: &crate::weather::Weather, focus: Focus) -> Scene {
    let (rise, set) = w.days.first().and_then(|d| Some((d.sunrise? as f32, d.sunset? as f32))).unwrap_or((360.0, 1080.0));
    let progress = |minute: f32, light: f32| {
        if light >= 0.5 {
            ((minute - rise) / (set - rise).max(1.0)).clamp(0.0, 1.0)
        } else {
            // The night runs from sunset to the next sunrise.
            ((minute - set).rem_euclid(1440.0) / (1440.0 - (set - rise)).max(1.0)).clamp(0.0, 1.0)
        }
    };
    match focus {
        Focus::Hour(i) if i < w.hours.len() => {
            let hour = &w.hours[i];
            let minute = hour.hour as f32 * 60.0 + 30.0;
            let mut light = w.daylight(minute);
            if hour.day != (light >= 0.5) {
                light = if hour.day { 1.0 } else { 0.0 };
            }
            Scene { sky: hour.sky, light, golden: w.golden(minute), wind: hour.wind, arc: progress(minute, light) }
        }
        Focus::Day(d) if d < w.days.len() => Scene { sky: w.days[d].sky, light: 1.0, golden: 0.0, wind: w.days[d].wind, arc: 0.62 },
        _ => {
            let minute = w.minute as f32;
            let light = w.daylight(minute);
            Scene { sky: w.sky, light, golden: w.golden(minute), wind: w.wind, arc: progress(minute, light) }
        }
    }
}

/// The sky card's picture: gradient for the hour and cover, stars, sun or
/// moon, two layers of clouds, rain with splashes, snow, fog, wind streaks and
/// lightning. Layers lean towards the pointer; a click sends a ring through
/// them (and a bolt down in a storm).
fn draw_scene(g: &Gfx, sc: &Scene, t: f32, wx: &Wx, a: f32) {
    use crate::render::Seg;
    use crate::weather::Sky;
    use std::f32::consts::{PI, TAU};
    let (x, y, w, h) = (X0, CARD_Y, SKY_W, CARD_H);
    let heavy = !matches!(sc.sky, Sky::Clear | Sky::Partly);
    // Quantised so the gradient brushes stay cached while the scene eases.
    let light = (sc.light * 24.0).round() / 24.0;
    let gold = (sc.golden * 24.0).round() / 24.0 * if heavy { 0.35 } else { 0.85 };
    let pick = |day_clear: u32, day_grey: u32, night_clear: u32, night_grey: u32, golden: u32| {
        let day = Rgba::hex(if heavy { day_grey } else { day_clear });
        let night = Rgba::hex(if heavy { night_grey } else { night_clear });
        night.mix(day, light).mix(Rgba::hex(golden), gold)
    };
    let top = pick(0x2470d0, 0x4b5a6c, 0x060a24, 0x101318, 0x34508f);
    let mid = pick(0x4a95e2, 0x6b7888, 0x141c45, 0x22262f, 0x9a6f92);
    let bottom = pick(0x96cdf5, 0x9aa5b2, 0x2a3466, 0x3a404d, 0xf5a066);
    // Darkening towards the bottom so the text stays readable.
    let low = bottom.mix(BLACK, 0.28);
    g.fill_vgradient(x, y, w, h, 16.0, &[(0.0, top), (0.45, mid), (0.72, bottom.mix(BLACK, 0.08)), (1.0, low)], a);
    g.clip(x, y, w, h);

    let (tx, ty) = wx.tilt;
    let shift = |depth: f32| (-tx * depth * 7.0, -ty * depth * 4.0);
    let poke = wx.poke.map(|(px, py, at)| (px, py, t - at));
    // The ring of a click shoves what it passes over.
    let push = |px: f32, py: f32| -> (f32, f32) {
        let Some((cx, cy, age)) = poke else { return (0.0, 0.0) };
        let (dx, dy) = (px - cx, py - cy);
        let d = (dx * dx + dy * dy).sqrt().max(1.0);
        let k = (-((d - age * 140.0) / 22.0).powi(2)).exp() * (1.0 - age / 1.6).max(0.0) * 14.0;
        (dx / d * k, dy / d * k)
    };

    // Stars, twinkling, with a shooting star now and then.
    let night = 1.0 - light;
    if night > 0.02 {
        let clear = match sc.sky {
            Sky::Clear => 1.0,
            Sky::Partly => 0.7,
            _ => 0.12,
        };
        let (sx, sy) = shift(0.2);
        for i in 0..30u32 {
            let twinkle = 0.55 + 0.45 * (t * (0.8 + scatter(i, 12) * 2.2) + scatter(i, 13) * 6.3).sin();
            let r = 0.6 + scatter(i, 14) * 0.9;
            g.fill_circle(x + 6.0 + scatter(i, 10) * (w - 12.0) + sx, y + 8.0 + scatter(i, 11) * h * 0.55 + sy, r, WHITE.alpha(a * night * clear * twinkle * 0.9));
        }
        let cycle = (t / 7.0).floor();
        let local = t / 7.0 - cycle;
        if !heavy && local < 0.1 {
            let (k, c) = (local / 0.1, cycle.max(0.0) as u32);
            let (x0, y0) = (x + 60.0 + scatter(c, 15) * (w - 70.0), y + 14.0 + scatter(c, 16) * 40.0);
            let (hx, hy) = (x0 - k * 90.0, y0 + k * 45.0);
            let fade = (k * (1.0 - k) * 4.0).min(1.0) * night * clear;
            g.line(hx + 28.0, hy - 14.0, hx, hy, 1.2, WHITE.alpha(a * fade * 0.3));
            g.line(hx + 10.0, hy - 5.0, hx, hy, 1.6, WHITE.alpha(a * fade * 0.85));
        }
    }

    // Sun or moon, travelling its arc; dimmed behind heavy cover.
    let veil = match sc.sky {
        Sky::Clear | Sky::Partly => 1.0,
        Sky::Cloudy | Sky::Fog => 0.3,
        _ => 0.12,
    };
    let (sx, sy) = shift(0.35);
    let (bx, by) = (x + 40.0 + sc.arc * (w - 80.0) + sx, y + 104.0 - (sc.arc * PI).sin() * 60.0 + sy);
    let pulse = poke.map_or(0.0, |(px, py, age)| if age < 0.7 && (px - bx).hypot(py - by) < 46.0 { (1.0 - age / 0.7).powi(2) } else { 0.0 });
    let breathe = 1.0 + 0.05 * (t * 1.3).sin() + 0.25 * pulse;
    if light >= 0.5 {
        let warm = theme::SUN.mix(Rgba::hex(0xff8a3d), gold);
        g.fill_circle(bx, by, 54.0 * breathe, warm.alpha(a * veil * 0.07));
        g.fill_circle(bx, by, 37.0 * breathe, warm.alpha(a * veil * 0.13));
        for k in 0..12 {
            let angle = t * 0.22 + k as f32 * TAU / 12.0;
            let (c, s) = (angle.cos(), angle.sin());
            let (r0, r1) = (25.0 * breathe, if k % 2 == 0 { 34.0 } else { 30.0 } * breathe);
            g.line(bx + r0 * c, by + r0 * s, bx + r1 * c, by + r1 * s, 2.0, warm.alpha(a * veil * 0.55));
        }
        g.fill_circle(bx, by, 19.0 * (1.0 + 0.12 * pulse), warm.alpha(a * veil));
        g.fill_circle(bx - 5.0, by - 5.0, 8.0, WHITE.alpha(a * veil * 0.22));
    } else {
        let moon = Rgba::hex(0xe8edff);
        g.fill_circle(bx, by, 42.0 * breathe, moon.alpha(a * veil * 0.06));
        g.fill_circle(bx, by, 28.0 * breathe, moon.alpha(a * veil * 0.10));
        g.fill_circle(bx, by, 17.0 * (1.0 + 0.1 * pulse), moon.alpha(a * veil));
        for (cx, cy, r) in [(-5.0, -3.0, 3.4), (4.0, 4.0, 2.4), (-2.0, 8.0, 1.8)] {
            g.fill_circle(bx + cx, by + cy, r, Rgba::hex(0xc5cdea).alpha(a * veil * 0.8));
        }
        // The dark side: a disc in the sky's colour at that height.
        // (Tinted by the glow it sits in, so no darker disc shows inside the halo.)
        let sky_here = top.mix(mid, ((by - y) / (h * 0.55)).clamp(0.0, 1.0)).mix(moon, 0.15);
        g.fill_circle(bx + 9.0, by - 5.0, 15.5, sky_here.alpha(a * veil * 0.95));
    }

    // Clouds in two layers: far ones paler and slower. Opaque, so overlapping
    // puffs do not show seams.
    let count: u32 = match sc.sky {
        Sky::Clear => 1,
        Sky::Partly => 4,
        Sky::Fog => 3,
        Sky::Storm => 7,
        _ => 6,
    };
    let dark = sc.sky == Sky::Storm;
    let by_day = Rgba::hex(if dark { 0x5d6474 } else if heavy { 0xc9d0da } else { 0xffffff });
    let by_night = Rgba::hex(if dark { 0x2b303b } else if heavy { 0x4b5262 } else { 0x7f88a3 });
    let shade = by_night.mix(by_day, light).mix(Rgba::hex(0xffc9a0), gold * 0.5);
    let drift = 3.0 + sc.wind * 0.3;
    let span = w + 200.0;
    for i in 0..count {
        let near = i % 2 == 1 || count == 1;
        let depth = if near { 1.0 } else { 0.55 };
        let k = if near { 1.15 } else { 0.8 } * (0.8 + scatter(i, 2) * 0.5);
        let (sx, sy) = shift(depth);
        // Spread along the loop (with a little jitter) so the card is never empty.
        let slot = (i as f32 + scatter(i, 3) * 0.6) / count as f32;
        let cx = x - 100.0 + (slot * span + t * drift * depth).rem_euclid(span) + sx;
        let height = if heavy { 70.0 } else { 90.0 };
        let cy = y + 22.0 + scatter(i, 4) * height + (t * 0.4 + i as f32).sin() * 2.0 + sy;
        let (px, py) = push(cx, cy);
        let colour = if count == 1 { shade.mix(top, 0.6) } else if near { shade } else { shade.mix(mid, 0.35) };
        puff(g, cx + px, cy + py, k, colour.alpha(a));
    }

    // Fog: soft bands drifting across.
    if sc.sky == Sky::Fog {
        // Overlapping soft ovals of mist, each drifting and breathing at its own pace.
        for i in 0..11u32 {
            let (sx, sy) = shift(0.5 + scatter(i, 21) * 0.5);
            let (rx, ry) = (70.0 + scatter(i, 24) * 90.0, 12.0 + scatter(i, 22) * 14.0);
            let fy = y + 50.0 + scatter(i, 23) * (h - 70.0) + (t * 0.45 + i as f32).sin() * 5.0 + sy;
            let fx = x + scatter(i, 27) * w + (t * 0.1 * (1.0 + scatter(i, 25)) + scatter(i, 20) * 6.0).sin() * 50.0 + sx;
            let breathe = 1.0 + 0.08 * (t * 0.7 + i as f32 * 1.3).sin();
            g.fill_ellipse(fx, fy, rx * breathe, ry, WHITE.alpha(a * (0.07 + scatter(i, 26) * 0.08)));
        }
    }

    // Rain, slanted by the wind, the nearest drops splashing.
    if matches!(sc.sky, Sky::Rain | Sky::Storm) {
        let n = if dark { 90 } else { 70 };
        let slant = -(sc.wind / 70.0).clamp(0.05, 0.6);
        let fall = h + 30.0;
        for i in 0..n {
            let d = 0.45 + scatter(i, 7) * 0.55;
            let u = (scatter(i, 6) + t * (300.0 + 260.0 * d) / fall).fract();
            let py = y - 20.0 + u * fall;
            let (sx, sy) = shift(d);
            let base = x - 30.0 + scatter(i, 5) * (w + 60.0) + sx;
            let px = base + slant * (py - y);
            let len = 7.0 + 9.0 * d;
            let (ox, oy) = push(px, py);
            let (lx, ly) = (px + ox, py + sy + oy);
            g.line(lx, ly, lx + slant * len, ly + len, 0.9 + 0.7 * d, Rgba::hex(0xd6e9ff).alpha(a * (0.22 + 0.4 * d)));
            if d > 0.8 && u < 0.08 {
                let s = u / 0.08;
                let (gx, gy) = (base + slant * (h - 4.0), y + h - 8.0);
                g.fill_ellipse(gx, gy, 2.0 + 7.0 * s, 1.0 + 1.5 * s, WHITE.alpha(a * 0.35 * (1.0 - s)));
                let hop = s * (1.0 - s) * 4.0;
                g.fill_circle(gx - 5.0 * s, gy - 5.0 * hop, 0.9, WHITE.alpha(a * 0.5 * (1.0 - s)));
                g.fill_circle(gx + 5.0 * s, gy - 4.0 * hop, 0.9, WHITE.alpha(a * 0.5 * (1.0 - s)));
            }
        }
    }

    // Snow, swaying and carried by the wind, settling at the bottom.
    if sc.sky == Sky::Snow {
        let fall = h + 12.0;
        for i in 0..60u32 {
            let d = 0.4 + scatter(i, 7) * 0.6;
            let u = (scatter(i, 6) + t * (16.0 + 26.0 * d) / fall).fract();
            let py = y - 6.0 + u * fall;
            let carried = (scatter(i, 5) * (w + 40.0) + t * sc.wind * 0.18 * d).rem_euclid(w + 40.0);
            let px = x - 20.0 + carried + (t * (0.5 + d) + i as f32).sin() * 7.0 * d;
            let (sx, sy) = shift(d);
            let (ox, oy) = push(px, py);
            g.fill_circle(px + sx + ox, py + sy + oy, 0.9 + 1.9 * d, WHITE.alpha(a * (0.45 + 0.5 * d)));
        }
        g.fill_ellipse(x + w * 0.3, y + h + 4.0, w * 0.55, 14.0, WHITE.alpha(a * 0.22));
        g.fill_ellipse(x + w * 0.82, y + h + 6.0, w * 0.4, 13.0, WHITE.alpha(a * 0.18));
    }

    // Wind: curling streaks, more and faster as it picks up.
    if sc.wind >= 16.0 {
        let lines = ((sc.wind - 8.0) / 8.0).clamp(1.0, 5.0) as u32;
        for i in 0..lines {
            let u = (scatter(i, 30) + t * (0.3 + sc.wind / 100.0) * (0.8 + scatter(i, 31) * 0.4)).fract();
            let (lx, ly) = (x - 70.0 + u * (w + 140.0), y + 44.0 + scatter(i, 32) * (h - 120.0));
            let fade = (u * (1.0 - u) * 4.0).min(1.0);
            let wob = (t * 3.0 + i as f32).sin() * 3.0;
            if let Some(p) = g.path((lx, ly), &[Seg::Cubic((lx + 14.0, ly - 6.0 + wob), (lx + 28.0, ly + 6.0 - wob), (lx + 46.0, ly)), Seg::Cubic((lx + 54.0, ly - 4.0), (lx + 50.0, ly - 10.0), (lx + 45.0, ly - 7.0))], false) {
                g.stroke_path(&p, 1.3, WHITE.alpha(a * 0.32 * fade));
            }
        }
    }

    // Lightning: a strike every few seconds, and one where the sky is clicked.
    if dark {
        let mut strike = strike(t).map(|(since, cycle)| (since, x + 36.0 + scatter(cycle, 41) * (w - 72.0), cycle));
        if let (Some((px, _, age)), Some((_, _, at))) = (poke, wx.poke) {
            if age < 0.55 {
                strike = Some((age, px.clamp(x + 20.0, x + w - 20.0), (at * 60.0) as u32));
            }
        }
        if let Some((age, sx, seed)) = strike {
            let f = if age < 0.07 {
                1.0
            } else if age < 0.13 {
                0.25
            } else if age < 0.2 {
                0.85
            } else {
                0.85 * (1.0 - (age - 0.2) / 0.35).max(0.0)
            };
            g.fill_rrect(x, y, w, h, 0.0, Rgba::hex(0xdfe8ff).alpha(a * f * 0.3));
            let mut pts = [(0.0f32, 0.0f32); 8];
            let mut px = sx;
            for (k, p) in pts.iter_mut().enumerate() {
                if k > 0 {
                    px += (scatter(seed.wrapping_mul(8).wrapping_add(k as u32), 42) - 0.5) * 26.0;
                }
                *p = (px, y + 18.0 + k as f32 * 21.0);
            }
            for pair in pts.windows(2) {
                g.line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, 6.0, Rgba::hex(0xb9c8ff).alpha(a * f * 0.25));
                g.line(pair[0].0, pair[0].1, pair[1].0, pair[1].1, 1.8, WHITE.alpha(a * f));
            }
            let (bx, by) = pts[3];
            let side = if scatter(seed, 43) > 0.5 { 1.0 } else { -1.0 };
            g.line(bx, by, bx + side * 16.0, by + 18.0, 1.2, WHITE.alpha(a * f * 0.8));
            g.line(bx + side * 16.0, by + 18.0, bx + side * 22.0, by + 34.0, 1.0, WHITE.alpha(a * f * 0.6));
        }
    }

    // The click's ring.
    if let Some((px, py, age)) = poke {
        let k = 1.0 - age / 1.6;
        g.stroke_circle(px, py, 6.0 + age * 140.0, 1.6 * k, WHITE.alpha(a * 0.45 * k * k));
        g.stroke_circle(px, py, 4.0 + age * 90.0, 1.0, WHITE.alpha(a * 0.22 * k * k));
    }
    g.unclip();
}

/// The sky card's words for its subject; `dy` lifts them as they arrive.
fn hero_text(g: &Gfx, w: &crate::weather::Weather, focus: Focus, a: f32, dy: f32) {
    if a <= 0.004 {
        return;
    }
    let (x, top, width) = (X0 + 18.0, CARD_Y + dy, SKY_W - 36.0);
    let (mut big, mut title, mut place, mut line) = (WBuf::new(), WBuf::new(), WBuf::new(), WBuf::new());
    let dot = "\u{2002}\u{00B7}\u{2002}";
    match focus {
        Focus::Hour(i) if i < w.hours.len() => {
            let hour = &w.hours[i];
            let _ = write!(big, "{:.0}\u{00B0}", hour.temp);
            let _ = write!(title, "{}", crate::weather::describe(hour.code));
            if i == 0 {
                let _ = write!(place, "Now");
            } else {
                let _ = write!(place, "{:02}:00", hour.hour);
            }
            let _ = write!(line, "Rain {} %{dot}Wind {:.0} km/h{dot}{:.0} %", hour.chance, hour.wind, hour.humidity);
        }
        Focus::Day(d) if d < w.days.len() => {
            let day = &w.days[d];
            let _ = write!(big, "{:.0}\u{00B0}", day.max);
            let _ = write!(title, "{}", crate::weather::describe(day.code));
            let name: &[u16] = match d {
                0 => ws(w!("Today")),
                1 => ws(w!("Tomorrow")),
                _ => ws(WEEKDAYS_LONG[day.weekday as usize % 7]),
            };
            for &unit in name {
                place.push(unit);
            }
            let _ = write!(line, "Low {:.0}\u{00B0}{dot}Rain {} %{dot}{:.1} mm", day.min, day.chance, day.mm);
        }
        _ => {
            let _ = write!(big, "{:.0}\u{00B0}", w.temp);
            let _ = write!(title, "{}", crate::weather::describe(w.code));
            let _ = write!(place, "{}", w.place);
            match w.days.first() {
                Some(today) => {
                    let _ = write!(line, "Feels {:.0}\u{00B0}{dot}H {:.0}\u{00B0}  L {:.0}\u{00B0}", w.feels, today.max, today.min);
                }
                None => {
                    let _ = write!(line, "Feels {:.0}\u{00B0}", w.feels);
                }
            }
        }
    }
    let white = WHITE.alpha(a);
    // The temperature drawn large: the clock face scaled up about its left edge.
    g.zoom_begin(x, top + 146.0, 1.75);
    g.text(big.as_slice(), Font::Clock, x, top + 127.0, 110.0, 38.0, white, Align::Left);
    g.zoom_end();
    g.text(title.as_slice(), Font::Title, x, top + 168.0, width, 24.0, white, Align::Left);
    g.text(place.as_slice(), Font::Body, x, top + 191.0, width, 20.0, white.alpha(0.85), Align::Left);
    g.text(line.as_slice(), Font::Small, x, top + 214.0, width, 18.0, white.alpha(0.8), Align::Left);
}

/// Catmull-Rom through four values at `t` (0..1 between `b` and `c`).
fn catmull(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (3.0 * b - a - 3.0 * c + d) * t * t * t)
}

/// The next 24 hours: a temperature curve coloured by warmth, rain chance as
/// bars, icons every third hour; the pointer scrubs along it.
#[allow(clippy::too_many_arguments)]
fn draw_hours(g: &Gfx, wx: &Wx, w: &crate::weather::Weather, pal: &Palette, a: f32, e: f32) {
    use crate::render::Seg;
    let hours = &w.hours[..w.hours.len().min(24)];
    let n = hours.len();
    if n < 2 {
        return;
    }
    let step = hour_step(n);
    let lo = hours.iter().map(|h| h.temp).fold(f32::MAX, f32::min);
    let hi = hours.iter().map(|h| h.temp).fold(f32::MIN, f32::max);
    let span = (hi - lo).max(2.0);
    let (ty0, ty1) = (BODY_Y + 72.0, BODY_Y + 128.0);
    let ypos = |temp: f32| ty1 - (temp - lo) / span * (ty1 - ty0);
    let pt = |i: usize| (GX0 + i as f32 * step, ypos(hours[i].temp));
    let base = BODY_Y + BODY_H - 30.0;

    // Rain chance bars, growing in one after another.
    for (i, hour) in hours.iter().enumerate() {
        if hour.chance < 5 {
            continue;
        }
        let grow = ease_back((e - 0.25 - i as f32 * 0.02) / 0.45);
        let bh = hour.chance as f32 / 100.0 * 24.0 * grow;
        let bw = step * 0.56;
        let chance = hour.chance as f32 / 100.0;
        g.fill_rrect(GX0 + i as f32 * step - bw * 0.5, base - bh, bw, bh, (bw * 0.5).min(bh * 0.5).min(2.0), RAIN_BLUE.alpha(a * (0.25 + 0.55 * chance)));
    }
    g.line(SIDE_X + 10.0, base + 0.5, SIDE_X + FORECAST_W - 10.0, base + 0.5, 1.0, pal.text2.alpha(a * 0.18));

    // The curve, drawn in from the left.
    let mut segs = [Seg::Line((0.0, 0.0)); 26];
    for (i, seg) in segs.iter_mut().enumerate().take(n - 1) {
        let (p0, p1, p2, p3) = (pt(i.saturating_sub(1)), pt(i), pt(i + 1), pt((i + 2).min(n - 1)));
        let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
        let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);
        *seg = Seg::Cubic(c1, c2, p2);
    }
    let reveal = ease_out(e / 0.9);
    g.clip(SIDE_X, BODY_Y, (GX0 - 14.0 - SIDE_X) + (GX1 - GX0 + 28.0) * reveal, BODY_H);
    segs[n - 1] = Seg::Line((GX1, ty1 + 26.0));
    segs[n] = Seg::Line((GX0, ty1 + 26.0));
    if let Some(area) = g.path(pt(0), &segs[..n + 1], true) {
        g.fill_path(&area, temp_colour((lo + hi) * 0.5).alpha(a * 0.12));
    }
    if let Some(curve) = g.path(pt(0), &segs[..n - 1], false) {
        g.stroke_glow(&curve, 2.2, temp_colour(hi).alpha(a), temp_colour(lo).alpha(a), ty0, ty1, 0.9);
    }
    g.unclip();

    // Every third hour: icon (bobbing gently), temperature, time.
    for i in (0..n).step_by(3) {
        let pop = ease_back((e - 0.1 - i as f32 * 0.02) / 0.4);
        if pop <= 0.01 {
            continue;
        }
        let la = a * clamp01(pop);
        let (px, py) = pt(i);
        // The icons nearest the scrubber grow and lift.
        let near = (1.0 - (wx.scrub - i as f32).abs() / 2.0).max(0.0) * wx.scrub_a;
        crate::ui::sky_icon(g, hours[i].sky, hours[i].day, px, BODY_Y + 24.0 - 2.0 * near, 0.8 * pop * (1.0 + 0.3 * near), la);
        let mut b = WBuf::new();
        let _ = write!(b, "{:.0}\u{00B0}", hours[i].temp);
        g.text(b.as_slice(), Font::Small, px - 22.0, py - 24.0, 44.0, 18.0, pal.text.alpha(la), Align::Center);
        g.fill_circle(px, py, 2.4, pal.text.alpha(la * 0.85));
        let mut b = WBuf::new();
        if i == 0 {
            let _ = write!(b, "Now");
        } else {
            let _ = write!(b, "{:02}:00", hours[i].hour);
        }
        g.text(b.as_slice(), Font::Small, px - 22.0, BODY_Y + BODY_H - 25.0, 44.0, 18.0, pal.text2.alpha(la), Align::Center);
    }

    // The scrubber: a line, a dot riding the curve, and the hour's numbers.
    if wx.scrub_a > 0.01 {
        let sa = a * wx.scrub_a;
        let f = wx.scrub.clamp(0.0, (n - 1) as f32);
        let i0 = (f.floor() as usize).min(n - 2);
        let fr = f - i0 as f32;
        let temp_at = |i: usize| hours[i.min(n - 1)].temp;
        let temp = catmull(temp_at(i0.saturating_sub(1)), temp_at(i0), temp_at(i0 + 1), temp_at(i0 + 2), fr);
        let (sx, sy) = (GX0 + f * step, ypos(temp));
        let colour = temp_colour(temp);
        g.line(sx, BODY_Y + 44.0, sx, base, 1.0, pal.text.alpha(sa * 0.3));
        g.fill_circle(sx, sy, 9.0, colour.alpha(sa * 0.25));
        g.fill_circle(sx, sy, 4.5, WHITE.alpha(sa));
        g.stroke_circle(sx, sy, 4.5, 1.6, colour.alpha(sa));
        let hour = &hours[(f.round() as usize).min(n - 1)];
        let (bw, bh) = (140.0, 38.0);
        let bx = (sx - bw * 0.5).clamp(SIDE_X + 6.0, SIDE_X + FORECAST_W - 6.0 - bw);
        let by = BODY_Y + 4.0;
        g.fill_rrect(bx, by, bw, bh, 10.0, pal.raised.alpha(sa));
        g.stroke_rrect(bx, by, bw, bh, 10.0, 1.0, pal.edge.alpha(sa));
        let mut b = WBuf::new();
        let _ = write!(b, "{:02}:00\u{2002}{}", hour.hour, crate::weather::describe(hour.code));
        g.text(b.as_slice(), Font::Small, bx + 10.0, by + 3.0, bw - 20.0, 16.0, pal.text2.alpha(sa), Align::Left);
        let mut b = WBuf::new();
        let _ = write!(b, "{:.0}\u{00B0}\u{2002}\u{00B7}\u{2002}{} %\u{2002}\u{00B7}\u{2002}{:.0} km/h", temp, hour.chance, hour.wind);
        g.text(b.as_slice(), Font::Label, bx + 10.0, by + 18.0, bw - 20.0, 18.0, pal.text.alpha(sa), Align::Left);
    }
}

/// Seven days: name, icon, rain chance, and the low-high range as a bar
/// coloured by temperature over the week's span (today with a dot for now).
#[allow(clippy::too_many_arguments)]
fn draw_days(g: &Gfx, wx: &Wx, w: &crate::weather::Weather, pal: &Palette, accent: Rgba, a: f32, e: f32) {
    let days = &w.days[..w.days.len().min(7)];
    if days.is_empty() {
        return;
    }
    let lo = days.iter().map(|d| d.min).fold(f32::MAX, f32::min);
    let hi = days.iter().map(|d| d.max).fold(f32::MIN, f32::max);
    let span = (hi - lo).max(1.0);
    let (bar0, bar1) = (SIDE_X + 196.0, SIDE_X + FORECAST_W - 54.0);
    let along = |temp: f32| bar0 + (temp - lo) / span * (bar1 - bar0);
    for (i, day) in days.iter().enumerate() {
        let y = DAY_Y + i as f32 * DAY_H;
        let show = ease_out((e - i as f32 * 0.045) / 0.35);
        let ra = a * show;
        if ra < 0.004 {
            continue;
        }
        let dx = (1.0 - show) * 16.0;
        let cy = y + DAY_H * 0.5;
        if wx.day == Some(i) {
            g.fill_rrect(SIDE_X + 6.0, y, FORECAST_W - 12.0, DAY_H, 10.0, accent.alpha(ra * 0.16));
            g.stroke_rrect(SIDE_X + 6.0, y, FORECAST_W - 12.0, DAY_H, 10.0, 1.0, accent.alpha(ra * 0.55));
        }
        let name = if i == 0 { w!("Today") } else { WEEKDAYS[day.weekday as usize % 7] };
        g.text(ws(name), Font::Label, SIDE_X + 16.0 + dx, y, 60.0, DAY_H, pal.text.alpha(ra), Align::Left);
        crate::ui::sky_icon(g, day.sky, true, SIDE_X + 94.0 + dx, cy, 0.72, ra);
        if day.chance >= 20 {
            let mut b = WBuf::new();
            let _ = write!(b, "{} %", day.chance);
            g.text(b.as_slice(), Font::Small, SIDE_X + 113.0 + dx, y, 40.0, DAY_H, RAIN_BLUE.alpha(ra), Align::Left);
        }
        let mut b = WBuf::new();
        let _ = write!(b, "{:.0}\u{00B0}", day.min);
        g.text(b.as_slice(), Font::Label, SIDE_X + 150.0 + dx, y, 38.0, DAY_H, pal.text2.alpha(ra), Align::Right);
        let mut b = WBuf::new();
        let _ = write!(b, "{:.0}\u{00B0}", day.max);
        g.text(b.as_slice(), Font::Label, bar1 + 9.0 + dx, y, 40.0, DAY_H, pal.text.alpha(ra), Align::Left);

        // The range bar grows out from its middle.
        g.fill_rrect(bar0 + dx, cy - 2.5, bar1 - bar0, 5.0, 2.5, pal.raised.alpha(ra));
        let grow = ease_back((e - 0.12 - i as f32 * 0.05) / 0.5);
        let (mid, half) = ((day.min + day.max) * 0.5, (day.max - day.min) * 0.5 * grow);
        let (t0, t1) = (mid - half, mid + half);
        let (x0, x1) = (along(t0) + dx, along(t1) + dx);
        const PARTS: usize = 10;
        if x1 - x0 < 6.0 {
            g.fill_rrect(x0, cy - 2.5, (x1 - x0).max(5.0), 5.0, 2.5, temp_colour(mid).alpha(ra));
        } else {
            g.fill_circle(x0 + 2.5, cy, 2.5, temp_colour(t0).alpha(ra));
            g.fill_circle(x1 - 2.5, cy, 2.5, temp_colour(t1).alpha(ra));
            let (inner0, inner1) = (x0 + 2.5, x1 - 2.5);
            for k in 0..PARTS {
                let (u0, u1) = (k as f32 / PARTS as f32, (k + 1) as f32 / PARTS as f32);
                let colour = temp_colour(t0 + (t1 - t0) * (u0 + u1) * 0.5);
                let xa = inner0 + (inner1 - inner0) * u0;
                g.fill_rrect(xa, cy - 2.5, (inner1 - inner0) / PARTS as f32 + 0.6, 5.0, 0.0, colour.alpha(ra));
            }
        }
        if i == 0 {
            let cx = along(w.temp.clamp(day.min, day.max)) + dx;
            g.fill_circle(cx, cy, 6.5, WHITE.alpha(ra * 0.18 * grow.min(1.0)));
            g.fill_circle(cx, cy, 3.6, WHITE.alpha(ra * grow.min(1.0)));
            g.stroke_circle(cx, cy, 3.6, 1.2, pal.card.alpha(ra));
        }
    }
}

/// The six detail tiles (wind, humidity, pressure, UV, sun, air). `place(id)` = where tile `id` is drawn and how lifted
/// it is by Ctrl + drag; `seq` = the order to draw them in (the held one last).
#[allow(clippy::too_many_arguments)]
fn draw_details(g: &Gfx, wx: &Wx, w: &crate::weather::Weather, pal: &Palette, accent: Rgba, a: f32, e: f32, t: f32, place: &dyn Fn(usize) -> (f32, f32, f32), seq: [usize; 6], held_ink: Rgba) {
    use std::f32::consts::{PI, TAU};
    const LABELS: [PCWSTR; 6] = [w!("WIND"), w!("HUMIDITY"), w!("PRESSURE"), w!("UV INDEX"), w!("SUN"), w!("AIR QUALITY")];
    for i in seq {
        let name = LABELS[i];
        let pop = ease_back((e - i as f32 * 0.06) / 0.4);
        if pop <= 0.01 {
            continue;
        }
        let (tx, ty, held) = place(i);
        let lift = wx.tiles[i].max(held);
        let k = (0.82 + 0.18 * pop) * (1.0 + 0.035 * lift);
        g.pose_begin(tx + WX_TILE_W * 0.5, ty + WX_TILE_H * 0.5, 0.0, -1.5 * lift, 0.0, k, k);
        let ta = a * clamp01(pop);
        g.fill_rrect(tx, ty, WX_TILE_W, WX_TILE_H, 12.0, pal.raised.mix(pal.text, 0.05 * lift).alpha(ta));
        if held > 0.0 {
            g.stroke_rrect(tx + 0.5, ty + 0.5, WX_TILE_W - 1.0, WX_TILE_H - 1.0, 12.0, 1.4, held_ink.alpha(ta * held));
        }
        let label = if i == 5 && w.aqi.is_none() { w!("VISIBILITY") } else { name };
        g.text(ws(label), Font::Small, tx + 10.0, ty + 6.0, WX_TILE_W - 20.0, 16.0, pal.text2.alpha(ta), Align::Left);
        let sweep = ease_out((e - 0.15 - i as f32 * 0.06) / 0.9);
        let (gx, gy, r) = (tx + WX_TILE_W * 0.5, ty + 44.0, 18.0);
        let (mut one, mut two) = (WBuf::new(), WBuf::new());
        let mut centre = WBuf::new();
        // Gauges sweep over 240 degrees, open at the bottom.
        let (a0, a1) = (PI * 0.75, PI * 2.25);
        match i {
            0 => {
                g.stroke_circle(gx, gy, r, 1.2, pal.text2.alpha(ta * 0.45));
                for k in 0..8 {
                    let angle = k as f32 * TAU / 8.0 - PI * 0.5;
                    let len = if k % 2 == 0 { 4.0 } else { 2.5 };
                    let ink = if k == 0 { accent } else { pal.text2 };
                    g.line(gx + angle.cos() * (r - len), gy + angle.sin() * (r - len), gx + angle.cos() * r, gy + angle.sin() * r, 1.2, ink.alpha(ta * 0.7));
                }
                // An arrowhead on the ring where the wind goes, a dot where it comes from;
                // it trembles more the gustier it is, and swings in when the view opens.
                let gust = ((w.gusts - w.wind) / w.wind.max(5.0)).clamp(0.0, 1.0);
                let wobble = ((t * 2.1).sin() * 0.6 + (t * 3.7).sin() * 0.4) * (0.04 + 0.12 * gust) * lift;
                let angle = (w.wind_dir + 180.0).to_radians() - PI * 0.5 + wobble - (1.0 - sweep) * 2.4;
                let (c, s) = (angle.cos(), angle.sin());
                let tip = (gx + c * (r + 2.0), gy + s * (r + 2.0));
                let back = (tip.0 - c * 8.0, tip.1 - s * 8.0);
                g.fill_poly(&[tip, (back.0 - s * 4.5, back.1 + c * 4.5), (back.0 + s * 4.5, back.1 - c * 4.5)], accent.alpha(ta));
                g.fill_circle(gx - c * r, gy - s * r, 2.4, accent.alpha(ta * 0.7));
                let _ = write!(centre, "{:.0}", w.wind * sweep);
                g.text(ws(w!("km/h")), Font::Small, gx - 20.0, gy + 2.0, 40.0, 13.0, pal.text2.alpha(ta), Align::Center);
                let _ = write!(one, "Gusts {:.0} km/h", w.gusts);
                let _ = write!(two, "From {}", crate::weather::compass(w.wind_dir));
            }
            1 => {
                // A glass of water filled to the humidity, its surface sloshing.
                g.fill_circle(gx, gy, r, pal.card.alpha(ta));
                let level = w.humidity / 100.0 * sweep;
                // Still at rest; it sloshes while the pointer is on the tile.
                for (phase, amp, alpha) in [(1.7, 1.6 * lift, 0.35), (0.0, 1.1 * lift, 0.7)] {
                    let surface = gy + r - 2.0 * r * level;
                    let mut pts = [(0.0f32, 0.0f32); 50];
                    for k in 0..25 {
                        let px = gx - r + 2.0 * r * k as f32 / 24.0;
                        let half = (r * r - (px - gx) * (px - gx)).max(0.0).sqrt();
                        let wave = surface + ((px - gx) * 0.28 + t * 2.4 + phase).sin() * amp + ((px - gx) * 0.13 - t * 1.5).sin() * amp * 0.6;
                        pts[k] = (px, wave.clamp(gy - half, gy + half));
                        pts[49 - k] = (px, gy + half);
                    }
                    g.fill_poly(&pts, RAIN_BLUE.alpha(ta * alpha));
                }
                g.stroke_circle(gx, gy, r, 1.2, RAIN_BLUE.alpha(ta * 0.6));
                let _ = write!(centre, "{:.0}%", w.humidity * sweep);
                let _ = write!(one, "Dew point {:.0}\u{00B0}", w.dew);
                let _ = write!(two, "Clouds {:.0} %", w.clouds);
            }
            2 => {
                arc(g, gx, gy, r, a0, a1, 3.0, pal.card.alpha(ta));
                let v = ((w.pressure - 970.0) / 80.0).clamp(0.0, 1.0) * sweep;
                let at = a0 + (a1 - a0) * v + (t * 5.0).sin() * 0.025 * lift;
                arc(g, gx, gy, r, a0, at, 3.0, accent.alpha(ta));
                g.line(gx, gy, gx + at.cos() * (r - 5.0), gy + at.sin() * (r - 5.0), 1.6, pal.text.alpha(ta));
                g.fill_circle(gx, gy, 2.6, pal.text.alpha(ta));
                let _ = write!(one, "{:.0} hPa", w.pressure);
                let trend = match w.pressure_trend {
                    d if d <= -1.0 => "Falling",
                    d if d >= 1.0 => "Rising",
                    _ => "Steady",
                };
                let _ = write!(two, "{trend}");
            }
            3 => {
                const BANDS: [(f32, f32, u32); 5] = [(0.0, 3.0, 0x6bd66b), (3.0, 6.0, 0xffd84a), (6.0, 8.0, 0xff9f40), (8.0, 11.0, 0xff5b4a), (11.0, 12.0, 0xb36bff)];
                let angle_of = |uv: f32| a0 + (a1 - a0) * (uv / 12.0).clamp(0.0, 1.0);
                for (u0, u1, colour) in BANDS {
                    arc(g, gx, gy, r, angle_of(u0) + 0.04, angle_of(u1) - 0.04, 3.5, Rgba::hex(colour).alpha(ta * 0.85));
                }
                let at = angle_of(w.uv * sweep);
                let (mx, my) = (gx + at.cos() * r, gy + at.sin() * r);
                g.fill_circle(mx, my, 5.0 + 2.0 * (t * 3.0).sin() * lift, WHITE.alpha(ta * 0.25));
                g.fill_circle(mx, my, 3.4, WHITE.alpha(ta));
                let _ = write!(centre, "{:.0}", w.uv);
                let _ = write!(one, "{}", crate::weather::uv_band(w.uv));
                let _ = write!(two, "Max today {:.0}", w.days.first().map_or(w.uv, |d| d.uv));
            }
            4 => {
                let (xa, xb, base) = (tx + 14.0, tx + WX_TILE_W - 14.0, ty + 58.0);
                g.line(tx + 8.0, base, tx + WX_TILE_W - 8.0, base, 1.0, pal.text2.alpha(ta * 0.35));
                let along = |u: f32| (xa + (xb - xa) * u, base - (u * PI).sin() * 30.0);
                let mut prev = along(0.0);
                for k in 1..=24 {
                    let p = along(k as f32 / 24.0);
                    g.line(prev.0, prev.1, p.0, p.1, 1.1, pal.text2.alpha(ta * 0.35));
                    prev = p;
                }
                match w.days.first().and_then(|d| Some((d.sunrise?, d.sunset?))) {
                    Some((rise, set)) => {
                        let minute = w.minute as f32;
                        let (rise, set) = (rise as f32, set as f32);
                        if (rise..=set).contains(&minute) {
                            let u = (minute - rise) / (set - rise).max(1.0) * sweep;
                            let mut prev = along(0.0);
                            for k in 1..=24 {
                                let p = along(u * k as f32 / 24.0);
                                g.line(prev.0, prev.1, p.0, p.1, 1.8, theme::SUN.alpha(ta * 0.8));
                                prev = p;
                            }
                            let (sx, sy) = along(u);
                            g.fill_circle(sx, sy, 7.0 + 2.0 * (t * 3.0).sin() * lift, theme::SUN.alpha(ta * 0.25));
                            g.fill_circle(sx, sy, 4.0, theme::SUN.alpha(ta));
                        } else {
                            // Night: the sun waits below the horizon.
                            let u = ((minute - set).rem_euclid(1440.0) / (1440.0 - (set - rise)).max(1.0)) * sweep;
                            let sx = xb - (xb - xa) * u;
                            g.fill_circle(sx, base + 7.0 + (u * PI).sin() * 4.0, 3.2, theme::SUN.alpha(ta * 0.35));
                        }
                        let (r, s) = (rise as u32, set as u32);
                        let _ = write!(one, "\u{2191} {:02}:{:02}\u{2002}\u{2193} {:02}:{:02}", r / 60, r % 60, s / 60, s % 60);
                        let light = s.saturating_sub(r);
                        let _ = write!(two, "{} h {:02} m of light", light / 60, light % 60);
                    }
                    None => {
                        let _ = write!(one, "No sunrise today");
                    }
                }
            }
            _ => match w.aqi {
                Some(aqi) => {
                    let colour = air_colour(aqi);
                    g.stroke_circle(gx, gy, r, 3.5, pal.card.alpha(ta));
                    let end = -PI * 0.5 + TAU * (aqi / 100.0).min(1.0) * sweep;
                    arc(g, gx, gy, r, -PI * 0.5, end, 3.5, colour.alpha(ta));
                    g.fill_circle(gx + end.cos() * r, gy + end.sin() * r, 3.0 + 2.0 * (t * 3.0).sin().abs() * lift, colour.alpha(ta * 0.5));
                    let _ = write!(centre, "{:.0}", aqi * sweep);
                    let _ = write!(one, "{}", crate::weather::air_band(aqi));
                    match w.pm25 {
                        Some(pm) => {
                            let _ = write!(two, "PM2.5 {:.0}\u{2002}\u{00B7}\u{2002}{:.0} km", pm, w.visibility / 1000.0);
                        }
                        None => {
                            let _ = write!(two, "Visibility {:.0} km", w.visibility / 1000.0);
                        }
                    }
                }
                None => {
                    // An eye: the lid's two curves and the iris.
                    for side in [-1.0, 1.0] {
                        arc(g, gx, gy + side * 12.0, 20.0, if side < 0.0 { PI * 0.25 } else { PI * 1.25 }, if side < 0.0 { PI * 0.75 } else { PI * 1.75 }, 1.6, pal.text2.alpha(ta));
                    }
                    g.fill_circle(gx, gy, 5.0 * sweep, accent.alpha(ta));
                    let _ = write!(one, "{:.0} km", w.visibility / 1000.0);
                    let _ = write!(two, "Clouds {:.0} %", w.clouds);
                }
            },
        }
        if !centre.as_slice().is_empty() {
            let dy = if i == 0 { -5.0 } else { 0.0 };
            g.text(centre.as_slice(), Font::Label, gx - 24.0, gy - 9.0 + dy, 48.0, 18.0, pal.text.alpha(ta), Align::Center);
        }
        g.text(one.as_slice(), Font::Small, tx + 4.0, ty + WX_TILE_H - 33.0, WX_TILE_W - 8.0, 16.0, pal.text.alpha(ta), Align::Center);
        g.text(two.as_slice(), Font::Small, tx + 4.0, ty + WX_TILE_H - 18.0, WX_TILE_W - 8.0, 16.0, pal.text2.alpha(ta), Align::Center);
        g.pose_end();
    }
}

pub fn draw_weather(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    let Some(w) = &s.weather else {
        let title = if s.weather_note.is_empty() { w!("Weather is off") } else { w!("No forecast yet") };
        let note: &[u16] = if s.weather_note.is_empty() { ws(w!("Switch Weather on in the settings, under Network, and type your city. Free, no account or key.")) } else { &s.weather_note };
        setup_card(g, title, note, pal, a);
        return;
    };
    // The two cards where Ctrl + drag left them (gliding there); each is drawn
    // with its usual layout, shifted. The one held goes last, on top.
    let (sky_to, side_to) = wx_shift(s);
    let area = |x: f32, w: f32| crate::ui::Area { x, y: CARD_Y, w, h: CARD_H };
    let (sky_r, sky_lift) = s.grip_rect(Group::WxCard, 0, area(X0 + sky_to, SKY_W));
    let (side_r, side_lift) = s.grip_rect(Group::WxCard, 1, area(SIDE_X + side_to, FORECAST_W));
    let base = g.current_offset();
    let (sky_dx, side_dx) = (sky_r.x - X0, side_r.x - SIDE_X);
    let sky_card = || {
        g.offset(base.0 + sky_dx, base.1);
        draw_sky_card(g, s, w, a);
        if sky_lift > 0.0 {
            g.stroke_rrect(X0 + 0.5, CARD_Y + 0.5, SKY_W - 1.0, CARD_H - 1.0, 16.0, 1.6, s.accent.alpha(a * sky_lift));
        }
    };
    let side_card = || {
        g.offset(base.0 + side_dx, base.1);
        draw_side_card(g, s, w, pal, a, side_dx);
        if side_lift > 0.0 {
            g.stroke_rrect(SIDE_X + 0.5, SEG_Y + 0.5, FORECAST_W - 1.0, CARD_H - 1.0, 16.0, 1.6, s.accent.alpha(a * side_lift));
        }
    };
    if s.grip_on_top(Group::WxCard, 0) {
        side_card();
        sky_card();
    } else {
        sky_card();
        side_card();
    }
    g.offset(base.0, base.1);
}

/// The sky card (left, unless swapped): the scene and the big temperature.
fn draw_sky_card(g: &Gfx, s: &State, w: &crate::weather::Weather, a: f32) {
    let (wx, t) = (&s.wx, s.sky_phase);
    // The sky card: the subject easing into the next one. The same cover
    // glides (sun along its arc, light and wind), a different one cross-fades.
    let k = ease_out(wx.mix);
    let (before, now) = (scene_of(w, wx.prev), scene_of(w, wx.focus));
    if k >= 1.0 || before == now {
        draw_scene(g, &now, t, wx, a);
    } else if before.sky == now.sky {
        let mixed = Scene { light: lerp(before.light, now.light, k), golden: lerp(before.golden, now.golden, k), wind: lerp(before.wind, now.wind, k), arc: lerp(before.arc, now.arc, k), ..now };
        draw_scene(g, &mixed, t, wx, a);
    } else {
        draw_scene(g, &before, t, wx, a);
        draw_scene(g, &now, t, wx, a * k);
    }
    if k < 1.0 {
        hero_text(g, w, wx.prev, a * (1.0 - k) * (1.0 - k), -6.0 * k);
    }
    hero_text(g, w, wx.focus, a * k, (1.0 - k) * 8.0);
}

/// The forecast card (right, unless swapped): the view switcher over the
/// hourly graph, the 7 days or the details. `shift` is how far it is drawn
/// from its usual place (items held with Ctrl + drag are placed in the tab's
/// units, so they are brought back by it).
fn draw_side_card(g: &Gfx, s: &State, w: &crate::weather::Weather, pal: &Palette, a: f32, shift: f32) {
    let (wx, t) = (&s.wx, s.sky_phase);
    // The switcher, its highlight gliding (and stretching a little on the way).
    g.card(SIDE_X, SEG_Y, FORECAST_W, SEG_H, SEG_H * 0.5, pal.card.alpha(a));
    let view_slot = wx_slot(s, Group::WxView, wx.view) as f32;
    let stretch = (wx.seg - view_slot).abs().min(1.0) * 18.0;
    g.fill_rrect(SIDE_X + 3.0 + wx.seg * SEG_W - stretch * 0.5, SEG_Y + 3.0, SEG_W - 6.0 + stretch, SEG_H - 6.0, (SEG_H - 6.0) * 0.5, pal.raised.alpha(a));
    let (_, side_to) = wx_shift(s);
    let views = s.order_of(Group::WxView);
    let mut seq = [0usize, 1, 2];
    seq.sort_by_key(|&k| s.grip_on_top(Group::WxView, views[k]));
    for &k in &seq {
        let id = views[k];
        let slot = crate::ui::Area { x: SIDE_X + side_to + k as f32 * SEG_W, y: SEG_Y, w: SEG_W, h: SEG_H };
        let (r, lift) = s.grip_rect(Group::WxView, id, slot);
        let x = r.x - shift;
        if lift > 0.0 {
            g.fill_rrect(x + 3.0, SEG_Y + 3.0, SEG_W - 6.0, SEG_H - 6.0, (SEG_H - 6.0) * 0.5, pal.raised.alpha(a * lift));
            g.stroke_rrect(x + 3.0, SEG_Y + 3.0, SEG_W - 6.0, SEG_H - 6.0, (SEG_H - 6.0) * 0.5, 1.4, s.accent.alpha(a * lift));
        }
        let on = (1.0 - (wx.seg - k as f32).abs().min(1.0)).max(lift);
        g.text(ws(VIEWS[id]), Font::Label, x, SEG_Y, SEG_W, SEG_H, pal.text2.mix(pal.text, on).alpha(a), Align::Center);
    }
    g.card(SIDE_X, BODY_Y, FORECAST_W, BODY_H, 16.0, pal.card.alpha(a));
    let e = t - wx.since;
    // Detail tiles: at their slots (or gliding / held), the held one last.
    let tiles = s.order_of(Group::WxDetail);
    let place = |id: usize| {
        let k = wx_slot(s, Group::WxDetail, id);
        let (tx, ty) = tile_at(k);
        let (r, lift) = s.grip_rect(Group::WxDetail, id, crate::ui::Area { x: tx + side_to, y: ty, w: WX_TILE_W, h: WX_TILE_H });
        (r.x - shift, r.y, lift)
    };
    let mut seq = [0usize; 6];
    seq.copy_from_slice(&tiles[..6]);
    seq.sort_by_key(|&id| s.grip_on_top(Group::WxDetail, id));
    match wx.view {
        0 => draw_hours(g, wx, w, pal, a, e),
        1 => draw_days(g, wx, w, pal, s.accent, a, e),
        _ => draw_details(g, wx, w, pal, s.accent, a, e, t, &place, seq, s.accent),
    }
}

// ----------------------------------------------------------------------- chat --

/// The header: the model chip (left) and "Open in Claude Code / Codex" (right).
const HEAD_Y: f32 = 58.0;
const HEAD_H: f32 = 26.0;
const MODEL_W: f32 = 230.0;
const OPEN_W: f32 = 170.0;
const CHAT_TOP: f32 = HEAD_Y + HEAD_H + 6.0;
pub const CHAT_BOTTOM: f32 = 262.0;
const INPUT_Y: f32 = 270.0;
const INPUT_H: f32 = 36.0;
const CHAT_GAP: f32 = 10.0;

const CHIP_H: f32 = 48.0;
const CHIP_GAP: f32 = 6.0;
const FIELD_X: f32 = X0 + INPUT_H + 8.0;
const FIELD_W: f32 = WIDTH - 2.0 * (INPUT_H + 8.0);
/// Under an answer: the copy and regenerate buttons.
const ACTIONS_H: f32 = 26.0;

fn chip_w(chip: &Chip) -> f32 {
    if chip.image.is_some() { 70.0 } else { 176.0 }
}

fn row_height(row: &ChatRow) -> f32 {
    if row.user {
        let chips = if row.chips.is_empty() { 0.0 } else { CHIP_H + CHIP_GAP };
        chips + if row.text.is_empty() { 0.0 } else { row.h + 16.0 }
    } else {
        row.h + if row.actions { ACTIONS_H } else { 0.0 }
    }
}

/// Where the message list ends (pending attachments sit between it and the input bar).
fn list_bottom(s: &State) -> f32 {
    if s.chat.pending.is_empty() { CHAT_BOTTOM } else { CHAT_BOTTOM - CHIP_H - 8.0 }
}

/// Top of each message, newest at the bottom.
fn row_tops(s: &State) -> Vec<f32> {
    let mut tops = vec![0.0; s.chat.rows.len()];
    let mut bottom = list_bottom(s) - 2.0 + s.chat.scroll;
    for (i, row) in s.chat.rows.iter().enumerate().rev() {
        tops[i] = bottom - row_height(row);
        bottom = tops[i] - CHAT_GAP;
    }
    tops
}

pub fn chat_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    if !s.chat.enabled {
        let (bx, by, bw, bh) = SETUP;
        return ((bx..=bx + bw).contains(&x) && (by..=by + bh).contains(&y)).then_some(Hit::OpenConfig);
    }
    if (HEAD_Y..=HEAD_Y + HEAD_H).contains(&y) {
        if (X0..=X0 + MODEL_W).contains(&x) {
            return Some(Hit::ChatModel);
        }
        if !s.chat.open.is_empty() && (X0 + WIDTH - OPEN_W..=X0 + WIDTH).contains(&x) {
            return Some(Hit::ChatOpen);
        }
        return None;
    }
    if (INPUT_Y..=INPUT_Y + INPUT_H).contains(&y) && (X0..=X0 + WIDTH).contains(&x) {
        return Some(if x < X0 + INPUT_H + 4.0 {
            Hit::ChatNew
        } else if x > X0 + WIDTH - INPUT_H - 4.0 {
            Hit::ChatSend
        } else if x > FIELD_X + FIELD_W - 34.0 && !s.chat.need_key {
            Hit::ChatAttach
        } else {
            Hit::ChatInput
        });
    }
    // The little crosses on the pending attachments.
    let mut cx = FIELD_X;
    for (i, chip) in s.chat.pending.iter().enumerate() {
        let w = chip_w(chip);
        if (x - (cx + w - 4.0)).hypot(y - (CHAT_BOTTOM - CHIP_H + 4.0)) <= 11.0 {
            return Some(Hit::ChatUnattach(i));
        }
        cx += w + CHIP_GAP;
    }
    if !(CHAT_TOP..=list_bottom(s)).contains(&y) {
        return None;
    }
    for ((i, row), top) in s.chat.rows.iter().enumerate().zip(row_tops(s)) {
        let line = top + row.h + 4.0;
        if row.actions && (line..=line + 20.0).contains(&y) {
            if (X0 + 30.0..=X0 + 52.0).contains(&x) {
                return Some(Hit::ChatCopy(i));
            }
            if row.regenerate && (X0 + 56.0..=X0 + 78.0).contains(&x) {
                return Some(Hit::ChatRegenerate);
            }
        }
    }
    None
}

/// Height of all messages (what the wheel scrolls through).
pub fn chat_height(s: &State) -> f32 {
    s.chat.rows.iter().map(|row| row_height(row) + CHAT_GAP).sum()
}

/// A picture thumbnail, or a file card (page icon, name, detail).
fn draw_chip(g: &Gfx, chip: &Chip, x: f32, y: f32, pal: &Palette, a: f32) {
    let w = chip_w(chip);
    if let Some(key) = chip.image {
        if g.image(key, x, y, w, CHIP_H, 10.0, a) {
            return;
        }
    }
    g.fill_rrect(x, y, w, CHIP_H, 10.0, pal.raised.alpha(a));
    if chip.image.is_some() {
        return;
    }
    let ink = if chip.busy { pal.text2 } else { theme::accent() }.alpha(a);
    let (px, py) = (x + 11.0, y + 11.0);
    g.stroke_rrect(px, py, 19.0, 25.0, 3.0, 1.5, ink);
    for k in 0..3 {
        g.line(px + 5.0, py + 9.0 + k as f32 * 5.0, px + 14.0, py + 9.0 + k as f32 * 5.0, 1.3, ink);
    }
    g.text(&chip.name, Font::Label, x + 40.0, y + 5.0, w - 48.0, 20.0, pal.text.alpha(a), Align::Left);
    g.text(&chip.detail, Font::Small, x + 40.0, y + 25.0, w - 48.0, 18.0, pal.text2.alpha(a), Align::Left);
}

/// Copy: two overlapping sheets.
fn copy_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_rrect(cx - 5.5, cy - 3.5, 9.0, 10.0, 2.0, 1.4, c);
    g.line(cx - 2.5, cy - 6.5, cx + 4.5, cy - 6.5, 1.4, c);
    g.line(cx + 6.5, cy - 4.5, cx + 6.5, cy + 3.0, 1.4, c);
}

/// Regenerate: an open circle with an arrow head.
fn regenerate_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba, bg: Rgba) {
    g.stroke_circle(cx, cy, 6.0, 1.4, c);
    g.fill_rrect(cx + 1.0, cy - 8.0, 6.0, 5.0, 0.0, bg);
    g.line(cx + 1.0, cy - 6.0, cx + 5.5, cy - 6.0, 1.4, c);
    g.line(cx + 5.5, cy - 6.0, cx + 5.5, cy - 10.0, 1.4, c);
}

/// A paperclip: a long loop with a shorter one inside.
fn clip_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_rrect(cx - 4.0, cy - 8.5, 8.0, 17.0, 4.0, 1.5, c);
    g.line(cx, cy - 4.0, cx, cy + 5.0, 1.5, c);
}

pub fn draw_chat(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    let chat = &s.chat;
    if !chat.enabled {
        setup_card(g, w!("AI chat is off"), ws(w!("Switch it on in the settings, under Network. With the Claude app installed, your Claude login works: no API key needed.")), pal, a);
        return;
    }
    // Header: which model answers (a click picks the next one), and the way to
    // this conversation in Claude Code / Codex.
    let hy = HEAD_Y + HEAD_H * 0.5;
    g.card(X0, HEAD_Y, MODEL_W, HEAD_H, HEAD_H * 0.5, pal.card.alpha(a));
    crate::ui::spark(g, X0 + 14.0, hy, 0.5, theme::accent().alpha(a));
    g.text(&chat.model, Font::Small, X0 + 26.0, HEAD_Y, MODEL_W - 46.0, HEAD_H, pal.text.alpha(a), Align::Left);
    g.line(X0 + MODEL_W - 16.0, hy - 3.5, X0 + MODEL_W - 12.0, hy, 1.4, pal.text2.alpha(a));
    g.line(X0 + MODEL_W - 12.0, hy, X0 + MODEL_W - 16.0, hy + 3.5, 1.4, pal.text2.alpha(a));
    if !chat.open.is_empty() {
        let bx = X0 + WIDTH - OPEN_W;
        g.card(bx, HEAD_Y, OPEN_W, HEAD_H, HEAD_H * 0.5, pal.card.alpha(a));
        g.text(&chat.open, Font::Small, bx + 12.0, HEAD_Y, OPEN_W - 34.0, HEAD_H, pal.text.alpha(a), Align::Left);
        // An arrow out of a box: "opens elsewhere".
        let (ax, ay) = (bx + OPEN_W - 16.0, hy);
        g.line(ax - 3.5, ay + 3.5, ax + 3.5, ay - 3.5, 1.4, pal.text2.alpha(a));
        g.line(ax, ay - 3.5, ax + 3.5, ay - 3.5, 1.4, pal.text2.alpha(a));
        g.line(ax + 3.5, ay - 3.5, ax + 3.5, ay, 1.4, pal.text2.alpha(a));
    }
    if chat.rows.is_empty() {
        let hint = if chat.need_key { w!("Paste your API key below and press Enter. It is stored encrypted on this PC.") } else { w!("Ask anything. The conversation stays on this PC, encrypted.") };
        crate::ui::spark(g, CX, 136.0, 1.6, theme::accent().alpha(a));
        g.text(ws(hint), Font::Body, X0 + 60.0, 160.0, WIDTH - 120.0, 44.0, pal.text2.alpha(a), Align::Center);
    }
    // Messages, newest at the bottom.
    let bottom = list_bottom(s);
    g.clip(X0, CHAT_TOP, WIDTH, bottom - CHAT_TOP);
    for ((i, row), top) in chat.rows.iter().enumerate().zip(row_tops(s)) {
        let height = row_height(row);
        if top > bottom || top + height < CHAT_TOP {
            continue;
        }
        if row.user {
            let mut y = top;
            if !row.chips.is_empty() {
                // Attachments above the bubble, right-aligned.
                let mut x = X0 + WIDTH - row.chips.iter().map(|c| chip_w(c) + CHIP_GAP).sum::<f32>() + CHIP_GAP;
                for chip in &row.chips {
                    draw_chip(g, chip, x, y, pal, a);
                    x += chip_w(chip) + CHIP_GAP;
                }
                y += CHIP_H + CHIP_GAP;
            }
            if !row.text.is_empty() {
                let width = row.w + 24.0;
                g.fill_rrect(X0 + WIDTH - width, y, width, row.h + 16.0, 14.0, pal.raised.alpha(a));
                g.wrapped(&row.text, X0 + WIDTH - width + 12.0, y + 8.0, CHAT_BUBBLE_W, pal.text.alpha(a), &[]);
            }
        } else {
            if row.actions {
                let line = top + row.h + 14.0;
                copy_icon(g, X0 + 41.0, line, pal.text2.alpha(a));
                if row.regenerate {
                    regenerate_icon(g, X0 + 67.0, line, pal.text2.alpha(a), pal.island.alpha(a));
                }
            }
            g.fill_circle(X0 + 12.0, top + 10.0, 11.0, pal.card.alpha(a));
            crate::ui::spark(g, X0 + 12.0, top + 10.0, 0.62, if row.error { RED } else { theme::accent() }.alpha(a));
            if row.text.is_empty() && chat.busy && i + 1 == chat.rows.len() {
                // Waiting for the first words.
                for dot in 0..3 {
                    g.fill_circle(X0 + 40.0 + dot as f32 * 9.0, top + 10.0, 2.4, pal.text2.alpha(a));
                }
            }
            let ink = if row.error { pal.text2 } else { pal.text };
            g.wrapped(&row.text, X0 + 34.0, top, CHAT_TEXT_W, ink.alpha(a), &row.bold);
        }
    }
    g.unclip();

    // Attachments waiting for the next message, each with a cross to drop it.
    let mut x = FIELD_X;
    for chip in &chat.pending {
        let (w, y) = (chip_w(chip), CHAT_BOTTOM - CHIP_H);
        draw_chip(g, chip, x, y, pal, a);
        g.fill_circle(x + w - 4.0, y + 4.0, 8.0, pal.card.alpha(a));
        g.line(x + w - 7.0, y + 1.0, x + w - 1.0, y + 7.0, 1.4, pal.text.alpha(a));
        g.line(x + w - 1.0, y + 1.0, x + w - 7.0, y + 7.0, 1.4, pal.text.alpha(a));
        x += w + CHIP_GAP;
    }

    // Input bar: new chat, the field, send (or stop while an answer streams).
    let (cy, r) = (INPUT_Y + INPUT_H * 0.5, INPUT_H * 0.5);
    let ink = pal.text2.alpha(a);
    g.fill_circle(X0 + r, cy, r, pal.card.alpha(a));
    g.stroke_rrect(X0 + r - 7.0, cy - 7.0, 14.0, 14.0, 4.0, 1.6, ink);
    g.line(X0 + r - 3.5, cy, X0 + r + 3.5, cy, 1.6, ink);
    g.line(X0 + r, cy - 3.5, X0 + r, cy + 3.5, 1.6, ink);
    let (fx, fw) = (FIELD_X, FIELD_W);
    g.card(fx, INPUT_Y, fw, INPUT_H, r, pal.card.alpha(a));
    let placeholder = if chat.need_key { w!("Paste your API key and press Enter") } else { w!("Ask anything, or drop a file here") };
    text_field(g, &chat.shown, ws(placeholder), Font::Body, fx + 16.0, INPUT_Y, fw - 54.0, INPUT_H, chat.focus, pal, a);
    if !chat.need_key {
        clip_icon(g, fx + fw - 19.0, cy, ink);
    }
    let sx = X0 + WIDTH - r;
    let reading = chat.pending.iter().any(|c| c.busy);
    let ready = chat.busy || ((!chat.shown.is_empty() || !chat.pending.is_empty()) && !reading);
    g.fill_circle(sx, cy, r, if ready { theme::accent() } else { pal.card }.alpha(a));
    let mark = if ready { Rgba::hex(0x0b0b12) } else { pal.text2 }.alpha(a);
    if chat.busy {
        g.fill_rrect(sx - 5.0, cy - 5.0, 10.0, 10.0, 2.0, mark);
    } else {
        g.line(sx, cy + 6.0, sx, cy - 6.0, 1.8, mark);
        g.line(sx, cy - 6.0, sx - 5.0, cy - 1.0, 1.8, mark);
        g.line(sx, cy - 6.0, sx + 5.0, cy - 1.0, 1.8, mark);
    }
}

// ------------------------------------------------------------------ clipboard --

const SEARCH_Y: f32 = 56.0;
const SEARCH_H: f32 = 34.0;
const ROWS_Y: f32 = 98.0;
const ROW_STEP: f32 = 40.0;
const ROW_H: f32 = 36.0;

pub fn clipboard_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    if !(X0..=X0 + WIDTH).contains(&x) {
        return None;
    }
    if (SEARCH_Y..=SEARCH_Y + SEARCH_H).contains(&y) {
        return Some(if x > X0 + WIDTH - 40.0 { Hit::ClipClear } else { Hit::ClipSearch });
    }
    if y >= ROWS_Y {
        let row = ((y - ROWS_Y) / ROW_STEP) as usize;
        if row < s.clip_rows.len() && y - ROWS_Y - row as f32 * ROW_STEP <= ROW_H {
            return Some(if x > X0 + WIDTH - 40.0 { Hit::ClipPin(row) } else { Hit::ClipRow(row) });
        }
    }
    None
}

pub fn draw_clipboard(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    // Search field and the clear button.
    let field_w = WIDTH - 44.0;
    g.card(X0, SEARCH_Y, field_w, SEARCH_H, 12.0, pal.card.alpha(a));
    if s.clip_focus {
        g.stroke_rrect(X0, SEARCH_Y, field_w, SEARCH_H, 12.0, 1.2, theme::accent().alpha(a));
    }
    search_icon(g, X0 + 18.0, SEARCH_Y + SEARCH_H * 0.5, pal.text2.alpha(a));
    text_field(g, &s.clip_query, ws(w!("Search history")), Font::Body, X0 + 34.0, SEARCH_Y, field_w - 44.0, SEARCH_H, s.clip_focus, pal, a);
    let (bx, by) = (X0 + WIDTH - 17.0, SEARCH_Y + SEARCH_H * 0.5);
    g.fill_circle(bx, by, 17.0, pal.card.alpha(a));
    trash_icon(g, bx, by, pal.text2.alpha(a));

    if s.clip_rows.is_empty() {
        let (title, hint) = if s.clip_query.is_empty() { (w!("History is empty"), w!("Copy text and it shows up here")) } else { (w!("Nothing found"), w!("No copied text matches the search")) };
        g.text(ws(title), Font::Title, X0, 170.0, WIDTH, 24.0, pal.text.alpha(a), Align::Center);
        g.text(ws(hint), Font::Body, X0, 196.0, WIDTH, 20.0, pal.text2.alpha(a), Align::Center);
        return;
    }
    for (i, row) in s.clip_rows.iter().enumerate() {
        let y = ROWS_Y + i as f32 * ROW_STEP;
        g.card(X0, y, WIDTH, ROW_H, 10.0, pal.card.alpha(a));
        if row.image != 0 {
            // A copied picture: its thumbnail, then its size.
            if !g.image(row.image, X0 + 8.0, y + 4.0, 48.0, ROW_H - 8.0, 6.0, a) {
                g.fill_rrect(X0 + 8.0, y + 4.0, 48.0, ROW_H - 8.0, 6.0, pal.raised.alpha(a));
            }
            g.text(&row.title, Font::Body, X0 + 68.0, y, WIDTH - 118.0, ROW_H, pal.text.alpha(a), Align::Left);
        } else {
            g.text(&row.title, Font::Body, X0 + 14.0, y, WIDTH - 64.0, ROW_H, pal.text.alpha(a), Align::Left);
        }
        // Pin: filled when pinned.
        let (px, py) = (X0 + WIDTH - 20.0, y + ROW_H * 0.5);
        let c = if row.marked { theme::accent() } else { pal.text2.alpha(0.6) }.alpha(a);
        g.fill_circle(px + 1.5, py - 1.5, 3.6, c);
        g.line(px - 1.0, py + 1.0, px - 5.0, py + 5.0, 1.6, c);
    }
    scroll_indicator(g, s.clip_first, s.clip_rows.len(), s.clip_total, CLIP_ROWS, pal, a);
}

/// More rows than fit: a faint track in the gutter right of the list with a
/// thumb showing which part is in view (`first` of `total`, `shown` at a time).
fn scroll_indicator(g: &Gfx, first: usize, shown: usize, total: usize, rows: usize, pal: &Palette, a: f32) {
    if total <= shown || total == 0 {
        return;
    }
    let (x, y0, y1) = (X0 + WIDTH + 7.0, ROWS_Y + 2.0, ROWS_Y + rows as f32 * ROW_STEP - ROW_STEP + ROW_H - 2.0);
    let span = y1 - y0;
    let len = (span * shown as f32 / total as f32).max(18.0);
    let top = y0 + (span - len) * first as f32 / (total - shown) as f32;
    g.line(x, y0, x, y1, 3.0, pal.raised.alpha(a * 0.5));
    g.line(x, top, x, top + len, 3.0, pal.text2.alpha(a * 0.8));
}

// ---------------------------------------------------------------------- shelf --

const ALL_W: f32 = 92.0;
const ALL_X: f32 = X0 + WIDTH - 44.0 - ALL_W;

pub fn shelf_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    if !(X0..=X0 + WIDTH).contains(&x) {
        return None;
    }
    if (SEARCH_Y..=SEARCH_Y + SEARCH_H).contains(&y) && !s.shelf_rows.is_empty() {
        if x > X0 + WIDTH - 40.0 {
            return Some(Hit::ShelfClear);
        }
        if (ALL_X..=ALL_X + ALL_W).contains(&x) {
            return Some(Hit::ShelfAll);
        }
    }
    if y >= ROWS_Y {
        let row = ((y - ROWS_Y) / ROW_STEP) as usize;
        if row < s.shelf_rows.len() && y - ROWS_Y - row as f32 * ROW_STEP <= ROW_H {
            return Some(if x > X0 + WIDTH - 40.0 { Hit::ShelfRemove(row) } else { Hit::ShelfRow(row) });
        }
    }
    None
}

pub fn draw_shelf(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    if s.shelf_rows.is_empty() {
        // An empty tray, drawn large.
        let (cx, cy) = (CX, 146.0);
        g.stroke_rrect(cx - 26.0, cy - 20.0, 52.0, 40.0, 10.0, 2.2, pal.text2.alpha(a * 0.7));
        g.line(cx, cy - 34.0, cx, cy - 6.0, 2.2, theme::accent().alpha(a));
        g.line(cx - 8.0, cy - 14.0, cx, cy - 6.0, 2.2, theme::accent().alpha(a));
        g.line(cx + 8.0, cy - 14.0, cx, cy - 6.0, 2.2, theme::accent().alpha(a));
        g.text(ws(w!("Drop files here")), Font::Title, X0, 188.0, WIDTH, 24.0, pal.text.alpha(a), Align::Center);
        g.text(ws(w!("Drag files onto the island to park them, then drag them out where you need them")), Font::Body, X0, 214.0, WIDTH, 20.0, pal.text2.alpha(a), Align::Center);
        return;
    }
    let mut count = WBuf::new();
    let _ = write!(count, "{} {}", s.shelf_total, if s.shelf_total == 1 { "item" } else { "items" });
    g.text(count.as_slice(), Font::Title, X0 + 4.0, SEARCH_Y, 200.0, SEARCH_H, pal.text.alpha(a), Align::Left);
    // The hint sits next to the button it explains.
    g.text(ws(w!("Drag a row out to drop it")), Font::Small, ALL_X - 262.0, SEARCH_Y, 250.0, SEARCH_H, pal.text2.alpha(a), Align::Right);
    g.fill_rrect(ALL_X, SEARCH_Y, ALL_W, SEARCH_H, SEARCH_H * 0.5, pal.raised.alpha(a));
    g.text(ws(w!("Drag all")), Font::Label, ALL_X, SEARCH_Y, ALL_W, SEARCH_H, pal.text.alpha(a), Align::Center);
    let (bx, by) = (X0 + WIDTH - 17.0, SEARCH_Y + SEARCH_H * 0.5);
    g.fill_circle(bx, by, 17.0, pal.card.alpha(a));
    trash_icon(g, bx, by, pal.text2.alpha(a));

    for (i, row) in s.shelf_rows.iter().enumerate() {
        let y = ROWS_Y + i as f32 * ROW_STEP;
        g.card(X0, y, WIDTH, ROW_H, 10.0, pal.card.alpha(a));
        g.fill_rrect(X0 + 8.0, y + 6.0, 42.0, ROW_H - 12.0, 7.0, pal.card.mix(theme::accent(), 0.4).alpha(a));
        g.text(&row.tag, Font::Small, X0 + 8.0, y + 6.0, 42.0, ROW_H - 12.0, pal.text.alpha(a), Align::Center);
        g.text(&row.title, Font::Body, X0 + 62.0, y, WIDTH - 62.0 - 150.0, ROW_H, pal.text.alpha(a), Align::Left);
        g.text(&row.detail, Font::Body, X0 + WIDTH - 144.0, y, 100.0, ROW_H, pal.text2.alpha(a), Align::Right);
        cross_icon(g, X0 + WIDTH - 20.0, y + ROW_H * 0.5, pal.text2.alpha(a));
    }
    scroll_indicator(g, s.shelf_first, s.shelf_rows.len(), s.shelf_total, SHELF_ROWS, pal, a);
}

// ---------------------------------------------------------------------- notes --

const SIDE_W: f32 = 178.0;
const FOLDER_W: f32 = 104.0;
const NEW_FOLDER_X: f32 = X0 + 125.0;
const NEW_NOTE_X: f32 = X0 + 161.0;
const NOTE_STEP: f32 = 35.0;
const NOTE_H: f32 = 31.0;
const EDITOR_X: f32 = X0 + 188.0;
const EDITOR_W: f32 = WIDTH - 188.0;
const EDITOR_H: f32 = 252.0;

pub fn notes_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    if (EDITOR_X..=EDITOR_X + EDITOR_W).contains(&x) && (SEARCH_Y..=SEARCH_Y + EDITOR_H).contains(&y) {
        return s.note.open.then_some(Hit::NoteText);
    }
    if !(X0..=X0 + SIDE_W).contains(&x) {
        return None;
    }
    if (SEARCH_Y..=SEARCH_Y + SEARCH_H).contains(&y) {
        return if x <= X0 + FOLDER_W {
            Some(Hit::NoteFolder)
        } else if (x - NEW_FOLDER_X).abs() <= 17.0 {
            Some(Hit::NoteNewFolder)
        } else if (x - NEW_NOTE_X).abs() <= 17.0 {
            Some(Hit::NoteNew)
        } else {
            None
        };
    }
    if y >= ROWS_Y {
        let row = ((y - ROWS_Y) / NOTE_STEP) as usize;
        if let Some(entry) = s.note.rows.get(row) {
            if y - ROWS_Y - row as f32 * NOTE_STEP <= NOTE_H {
                return Some(if entry.marked && x > X0 + SIDE_W - 32.0 { Hit::NoteDelete } else { Hit::NoteRow(row) });
            }
        }
    }
    None
}

pub fn draw_notes(g: &Gfx, s: &State, pal: &Palette, a: f32) {
    let n = &s.note;
    // Folder chip (click: next folder), new folder, new note.
    let mid = SEARCH_Y + SEARCH_H * 0.5;
    g.card(X0, SEARCH_Y, FOLDER_W, SEARCH_H, 12.0, pal.card.alpha(a));
    folder_icon(g, X0 + 18.0, mid, pal.text2.alpha(a));
    g.text(&n.folder, Font::Label, X0 + 34.0, SEARCH_Y, FOLDER_W - 40.0, SEARCH_H, pal.text.alpha(a), Align::Left);
    g.fill_circle(NEW_FOLDER_X, mid, 17.0, pal.card.alpha(a));
    folder_icon(g, NEW_FOLDER_X - 1.5, mid + 0.5, pal.text2.alpha(a));
    g.line(NEW_FOLDER_X + 4.5, mid - 7.5, NEW_FOLDER_X + 9.5, mid - 7.5, 1.5, pal.text.alpha(a));
    g.line(NEW_FOLDER_X + 7.0, mid - 10.0, NEW_FOLDER_X + 7.0, mid - 5.0, 1.5, pal.text.alpha(a));
    g.fill_circle(NEW_NOTE_X, mid, 17.0, theme::accent().alpha(a));
    let ink = Rgba::hex(0x0b0b12).alpha(a);
    g.line(NEW_NOTE_X - 5.5, mid, NEW_NOTE_X + 5.5, mid, 1.9, ink);
    g.line(NEW_NOTE_X, mid - 5.5, NEW_NOTE_X, mid + 5.5, 1.9, ink);

    for (i, row) in n.rows.iter().enumerate() {
        let y = ROWS_Y + i as f32 * NOTE_STEP;
        g.card(X0, y, SIDE_W, NOTE_H, 9.0, if row.marked { pal.raised } else { pal.card }.alpha(a));
        let width = if row.marked { SIDE_W - 46.0 } else { SIDE_W - 22.0 };
        g.text(&row.title, Font::Body, X0 + 12.0, y, width, NOTE_H, if row.marked { pal.text } else { pal.text2 }.alpha(a), Align::Left);
        if row.marked {
            trash_icon(g, X0 + SIDE_W - 17.0, y + NOTE_H * 0.5, pal.text2.alpha(a));
        }
    }
    if n.total > n.rows.len() {
        g.line(X0 - 6.0, ROWS_Y + 4.0, X0 - 6.0, ROWS_Y + NOTE_ROWS as f32 * NOTE_STEP - 8.0, 2.0, pal.raised.alpha(a));
    }

    g.card(EDITOR_X, SEARCH_Y, EDITOR_W, EDITOR_H, 14.0, pal.card.alpha(a));
    if n.focus {
        g.stroke_rrect(EDITOR_X, SEARCH_Y, EDITOR_W, EDITOR_H, 14.0, 1.2, theme::accent().alpha(a));
    }
    if !n.open {
        g.text(ws(w!("No note open")), Font::Title, EDITOR_X, 158.0, EDITOR_W, 24.0, pal.text.alpha(a), Align::Center);
        g.text(ws(w!("Press + to write one")), Font::Body, EDITOR_X, 184.0, EDITOR_W, 20.0, pal.text2.alpha(a), Align::Center);
        return;
    }
    if n.text.is_empty() && !n.focus {
        g.text(ws(w!("Click to write. # heading, **bold**, *italic*, - list")), Font::Body, NOTE_TEXT_X, NOTE_TEXT_Y, NOTE_TEXT_W, 20.0, pal.text2.alpha(a * 0.8), Align::Left);
    }
    g.note_draw(&n.text(), NOTE_TEXT_X, NOTE_TEXT_Y, NOTE_TEXT_W, NOTE_TEXT_H, n.scroll, n.focus.then_some(n.caret), n.selection, pal.text.alpha(a), pal.text2.alpha(a * 0.75), theme::accent().alpha(a));
}

// ------------------------------------------------------------------ hover map --

/// Where the buttons of the tabs and the command bar are drawn (see `ui::glow_shape`).
pub fn glow_shape(s: &State, hit: Hit) -> Option<crate::ui::Glow> {
    use crate::ui::Glow;
    let (tiles, buttons) = controls_layout(s);
    let tile = |i: usize| tiles.iter().find(|(k, _)| *k == i).map(|(_, r)| Glow::Rect(r.x, r.y, r.w, r.h, 16.0));
    let row_y = |row: usize| ROWS_Y + row as f32 * ROW_STEP;
    let corner = Some(Glow::Circle(X0 + WIDTH - 17.0, SEARCH_Y + SEARCH_H * 0.5, 17.0));
    let input = INPUT_Y + INPUT_H * 0.5;
    match hit {
        Hit::Radio(Kind::WiFi) => tile(0),
        Hit::Radio(Kind::Bluetooth) => tile(1),
        Hit::Vpn => tile(2),
        Hit::Theme => tile(3),
        Hit::Power(action) => {
            let i = POWER.iter().position(|(a, _)| *a == action)?;
            buttons.iter().find(|(k, _)| *k == i).map(|(_, r)| Glow::Rect(r.x, r.y, r.w, r.h, 14.0))
        }
        Hit::OpenConfig => {
            let (x, y, w, h) = SETUP;
            Some(Glow::Rect(x, y, w, h, h * 0.5))
        }
        Hit::ClipClear | Hit::ShelfClear => corner,
        Hit::ClipRow(row) | Hit::ShelfRow(row) => Some(Glow::Rect(X0, row_y(row), WIDTH, ROW_H, 10.0)),
        Hit::ClipPin(row) | Hit::ShelfRemove(row) => Some(Glow::Circle(X0 + WIDTH - 19.0, row_y(row) + ROW_H * 0.5, 13.0)),
        Hit::ShelfAll => Some(Glow::Rect(ALL_X, SEARCH_Y, ALL_W, SEARCH_H, SEARCH_H * 0.5)),
        Hit::NoteFolder => Some(Glow::Rect(X0, SEARCH_Y, FOLDER_W, SEARCH_H, 12.0)),
        Hit::NoteNewFolder => Some(Glow::Circle(NEW_FOLDER_X, SEARCH_Y + SEARCH_H * 0.5, 17.0)),
        Hit::NoteNew => Some(Glow::Circle(NEW_NOTE_X, SEARCH_Y + SEARCH_H * 0.5, 17.0)),
        Hit::NoteRow(row) => Some(Glow::Rect(X0, ROWS_Y + row as f32 * NOTE_STEP, SIDE_W, NOTE_H, 9.0)),
        Hit::NoteDelete => s.note.rows.iter().position(|r| r.marked).map(|row| Glow::Circle(X0 + SIDE_W - 17.0, ROWS_Y + row as f32 * NOTE_STEP + NOTE_H * 0.5, 12.0)),
        Hit::ChatModel => Some(Glow::Rect(X0, HEAD_Y, MODEL_W, HEAD_H, HEAD_H * 0.5)),
        Hit::ChatOpen => Some(Glow::Rect(X0 + WIDTH - OPEN_W, HEAD_Y, OPEN_W, HEAD_H, HEAD_H * 0.5)),
        Hit::ChatNew => Some(Glow::Circle(X0 + INPUT_H * 0.5, input, INPUT_H * 0.5)),
        Hit::ChatSend => Some(Glow::Circle(X0 + WIDTH - INPUT_H * 0.5, input, INPUT_H * 0.5)),
        Hit::ChatAttach => Some(Glow::Circle(FIELD_X + FIELD_W - 19.0, input, 13.0)),
        Hit::ChatUnattach(i) => {
            let x = FIELD_X + s.chat.pending.iter().take(i + 1).map(|c| chip_w(c) + CHIP_GAP).sum::<f32>() - CHIP_GAP;
            Some(Glow::Circle(x - 4.0, CHAT_BOTTOM - CHIP_H + 4.0, 9.0))
        }
        Hit::ChatCopy(i) => row_tops(s).get(i).map(|top| Glow::Circle(X0 + 41.0, top + s.chat.rows[i].h + 14.0, 11.0)),
        Hit::ChatRegenerate => {
            let i = s.chat.rows.iter().rposition(|r| r.regenerate)?;
            row_tops(s).get(i).map(|top| Glow::Circle(X0 + 67.0, top + s.chat.rows[i].h + 14.0, 11.0))
        }
        Hit::WxView(i) => Some(Glow::Rect(SIDE_X + wx_shift(s).1 + 3.0 + wx_slot(s, Group::WxView, i) as f32 * SEG_W, SEG_Y + 3.0, SEG_W - 6.0, SEG_H - 6.0, (SEG_H - 6.0) * 0.5)),
        Hit::WxDay(row) => Some(Glow::Rect(SIDE_X + wx_shift(s).1 + 6.0, DAY_Y + row as f32 * DAY_H, FORECAST_W - 12.0, DAY_H, 10.0)),
        Hit::CmdRow(row) => Some(Glow::Rect(CX - COMMAND_W * 0.5 + 12.0, COMMAND_INPUT_H + row as f32 * COMMAND_ROW_H, COMMAND_W - 24.0, COMMAND_ROW_H - 4.0, 12.0)),
        _ => None,
    }
}

// ---------------------------------------------------------------- command bar --

pub fn command_hit(s: &State, x: f32, y: f32) -> Option<Hit> {
    let left = CX - COMMAND_W * 0.5;
    if s.mode != Mode::Command || !(left..=left + COMMAND_W).contains(&x) || y < COMMAND_INPUT_H {
        return None;
    }
    let row = ((y - COMMAND_INPUT_H) / COMMAND_ROW_H) as usize;
    (row < s.cmd_rows.len()).then_some(Hit::CmdRow(row))
}

pub fn draw_command(g: &Gfx, s: &State, pal: &Palette) {
    let a = clamp01(s.cmd.x);
    if a <= 0.004 {
        return;
    }
    let left = CX - COMMAND_W * 0.5;
    search_icon(g, left + 30.0, COMMAND_INPUT_H * 0.5, pal.text2.alpha(a));
    text_field(g, &s.cmd_text, ws(w!("Type an app, a calculation or \u{201C}timer 10 min\u{201D}")), Font::Title, left + 54.0, 0.0, COMMAND_W - 80.0, COMMAND_INPUT_H, true, pal, a);
    for (i, row) in s.cmd_rows.iter().enumerate() {
        let y = COMMAND_INPUT_H + i as f32 * COMMAND_ROW_H;
        // Rows appear as the shape grows to hold them.
        let ra = a * clamp01((s.h.x - y - 8.0) / 24.0);
        if i == s.cmd_sel {
            g.fill_rrect(left + 12.0, y, COMMAND_W - 24.0, COMMAND_ROW_H - 4.0, 12.0, pal.raised.alpha(ra));
        }
        g.fill_rrect(left + 22.0, y + 6.0, 26.0, 26.0, 8.0, pal.card.mix(theme::accent(), 0.35).alpha(ra));
        g.text(&row.title[..row.title.len().min(1)], Font::Label, left + 22.0, y + 6.0, 26.0, 26.0, pal.text.alpha(ra), Align::Center);
        g.text(&row.title, Font::Label, left + 60.0, y, COMMAND_W - 240.0, COMMAND_ROW_H - 4.0, pal.text.alpha(ra), Align::Left);
        g.text(&row.detail, Font::Body, left + COMMAND_W - 200.0, y, 176.0, COMMAND_ROW_H - 4.0, pal.text2.alpha(ra), Align::Right);
    }
}

// --------------------------------------------------------------------- shared --

fn trash_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_rrect(cx - 5.0, cy - 4.0, 10.0, 10.5, 2.0, 1.5, c);
    g.line(cx - 7.0, cy - 6.0, cx + 7.0, cy - 6.0, 1.5, c);
}

fn cross_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, 1.6, c);
    g.line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, 1.6, c);
}

fn folder_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_rrect(cx - 7.0, cy - 4.0, 14.0, 10.0, 2.0, 1.5, c);
    g.line(cx - 7.0, cy - 4.0, cx - 5.5, cy - 6.5, 1.5, c);
    g.line(cx - 5.5, cy - 6.5, cx - 1.0, cy - 6.5, 1.5, c);
    g.line(cx - 1.0, cy - 6.5, cx + 0.5, cy - 4.0, 1.5, c);
}

fn search_icon(g: &Gfx, cx: f32, cy: f32, c: Rgba) {
    g.stroke_circle(cx - 1.5, cy - 1.5, 5.0, 1.7, c);
    g.line(cx + 2.4, cy + 2.4, cx + 6.0, cy + 6.0, 1.7, c);
}

/// A single-line text field: the text (or a grey placeholder) and, when it has
/// the keyboard, a caret after the last character.
#[allow(clippy::too_many_arguments)]
fn text_field(g: &Gfx, text: &[u16], placeholder: &[u16], font: Font, x: f32, y: f32, w: f32, h: f32, focused: bool, pal: &Palette, a: f32) {
    if text.is_empty() {
        g.text(placeholder, font, x, y, w, h, pal.text2.alpha(a * 0.8), Align::Left);
    } else {
        g.text(text, font, x, y, w, h, pal.text.alpha(a), Align::Left);
    }
    if focused {
        let caret = x + g.measure(text, font).min(w) + 1.5;
        let half = if font == Font::Title { 10.0 } else { 8.0 };
        g.line(caret, y + h * 0.5 - half, caret, y + h * 0.5 + half, 1.6, theme::accent().alpha(a));
    }
}
