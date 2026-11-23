//! The characters that can sit on the left of the pill: drawn from circles,
//! ellipses, lines and a few small polygons (no sprites), about 26 dp tall.
//! Adding one is a draw function plus a line in [`CAST`].
//!
//! They never animate continuously (that would cost CPU at rest). The app
//! hands them a [`Pose`] that changes on events (the cursor moving to a new
//! direction, a blink, a beat, an agent working or finishing, the user going
//! away) plus short *acts*: a wiggle, a look around, a stretch, a jump, a
//! twirl, a bounce to the music or the character's own trick. An act lasts
//! about a second and is drawn at 30 frames a second in the character's small
//! corner only; between acts nothing is drawn at all.
//!
//! Every character reads the same [`Motion`] (lift, tilt, squash, how its
//! tail / ears / arms swing, mouth, eyes) worked out from the pose by
//! [`motion`], so a new act works for all of them.

use crate::render::{Gfx, Rgba};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Eyes {
    Open,
    Closed,
    /// Upturned arcs: pleased.
    Happy,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mark {
    None,
    /// Three dots, `step` picks the bright one.
    Thinking,
    /// "z z": asleep; `step` makes them drift.
    Sleep,
}

/// A short animation. `secs` is how long it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    None,
    /// Rocks from side to side, tail / ears / arms swinging.
    Wiggle,
    /// Looks left, then right.
    LookAround,
    /// Stretches up with the eyes shut (after waking).
    Stretch,
    /// A big yawn (falling asleep).
    Yawn,
    /// Crouches, jumps, lands (greeting, an agent finished).
    Jump,
    /// Turns once around.
    Twirl,
    /// A little hop to one side on a music beat (`Pose::side`).
    Bounce,
    /// The character's own trick (cat licks a paw, frog catches a fly ...).
    Special,
    /// Being stroked (the cursor rubbing back and forth over it): it sways
    /// happily, purrs and hearts float up.
    Pet,
    /// Clicked: squeezes its eyes shut, squashes, pops back up.
    Boop,
}

impl Act {
    pub fn secs(self) -> f32 {
        match self {
            Act::None => 0.0,
            Act::Wiggle => 0.9,
            Act::LookAround => 1.9,
            Act::Stretch => 1.4,
            Act::Yawn => 1.5,
            Act::Jump => 0.7,
            Act::Twirl => 0.9,
            Act::Bounce => 0.42,
            Act::Special => 1.8,
            Act::Pet => 1.9,
            Act::Boop => 0.8,
        }
    }

    /// What a character may do by itself now and then (its trick twice as often).
    pub const IDLE: [Act; 7] = [Act::Wiggle, Act::LookAround, Act::Stretch, Act::Twirl, Act::Jump, Act::Special, Act::Special];

    /// Test hooks name acts by number.
    pub const ALL: [Act; 11] = [Act::None, Act::Wiggle, Act::LookAround, Act::Stretch, Act::Yawn, Act::Jump, Act::Twirl, Act::Bounce, Act::Special, Act::Pet, Act::Boop];
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pose {
    /// Where the eyes look, each -1..1.
    pub look: (f32, f32),
    pub eyes: Eyes,
    pub mark: Mark,
    pub step: u8,
    pub act: Act,
    /// Progress through `act`, 0..1.
    pub t: f32,
    /// -1 or 1: which way a bounce leans (alternates with the beats).
    pub side: f32,
    /// Eyelids: 1 open, 0 shut (a blink eases through the values between).
    pub open: f32,
    /// Leaning towards the cursor, radians.
    pub lean: f32,
    /// 0..1: the cursor is on it (blushes, smiles).
    pub fond: f32,
}

impl Default for Pose {
    fn default() -> Self {
        Pose { look: (0.0, 0.0), eyes: Eyes::Open, mark: Mark::None, step: 0, act: Act::None, t: 0.0, side: 1.0, open: 1.0, lean: 0.0, fond: 0.0 }
    }
}

/// How much bigger the characters are drawn in the pill than their drawings.
pub const PILL_ZOOM: f32 = 1.3;

/// Radius (dp, around the spot in the pill) in which the cursor is "on" the character.
pub const TOUCH_RADIUS: f32 = 19.0;

pub struct Character {
    pub name: &'static str,
    /// Name shown on the settings page.
    pub label: &'static str,
    /// Draws it centred on (cx, cy); `light` is 0 for the dark theme, 1 for light.
    pub draw: fn(&Gfx, &Pose, f32, f32, f32, f32),
}

pub const CAST: &[Character] = &[
    Character { name: "cat", label: "Cat", draw: cat },
    Character { name: "ghost", label: "Ghost", draw: ghost },
    Character { name: "jellyfish", label: "Jellyfish", draw: jellyfish },
    Character { name: "fox", label: "Fox", draw: fox },
    Character { name: "frog", label: "Frog", draw: frog },
    Character { name: "owl", label: "Owl", draw: owl },
    Character { name: "bunny", label: "Bunny", draw: bunny },
    Character { name: "robot", label: "Robot", draw: robot },
    Character { name: "axolotl", label: "Axolotl", draw: axolotl },
];

/// The character called `name`, if there is one.
pub fn find(name: &str) -> Option<usize> {
    CAST.iter().position(|c| c.name.eq_ignore_ascii_case(name.trim()))
}

/// Where the eyes look for a cursor at (dx, dy) dp. Close by (within 160 dp)
/// they follow it closely; further away they snap to eight directions, so a
/// cursor moving about the screen rarely redraws anything. The app eases
/// between targets.
pub fn look_at(dx: f32, dy: f32) -> (f32, f32) {
    let len = dx.hypot(dy);
    if len < 12.0 {
        return (0.0, 0.0);
    }
    if len < 160.0 {
        let reach = 0.35 + 0.65 * ((len - 12.0) / 148.0);
        let q = |v: f32| (v * reach * 20.0).round() / 20.0;
        return (q(dx / len), q(dy / len));
    }
    let reach = if len < 90.0 { 0.5 } else { 1.0 };
    let angle = dy.atan2(dx);
    let step = (angle / (PI / 4.0)).round() * (PI / 4.0);
    let (sin, cos) = step.sin_cos();
    (((cos * reach) * 10.0).round() / 10.0, ((sin * reach) * 10.0).round() / 10.0)
}

// --------------------------------------------------------------- motion --

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mouth {
    None,
    Smile,
    /// Open, 0..1 how wide.
    Open(f32),
}

/// How the body moves for a pose: what every character reads.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// Lifted this far (dp) and moved sideways.
    pub lift: f32,
    pub dx: f32,
    /// Lean in radians and squash / stretch about the feet.
    pub tilt: f32,
    pub sx: f32,
    pub sy: f32,
    /// Tail, ears, arms, tentacles: -1..1.
    pub swing: f32,
    pub mouth: Mouth,
    pub eyes: Eyes,
    pub look: (f32, f32),
    /// Progress of the character's own trick, or -1.
    pub special: f32,
    /// Eyelids, 1 open .. 0 shut.
    pub open: f32,
    /// 0..1: blushing (the cursor is on it, being stroked).
    pub fond: f32,
}

/// 0 at both ends, 1 in the middle.
fn bell(t: f32) -> f32 {
    (PI * t.clamp(0.0, 1.0)).sin()
}

/// Smooth 0 -> 1 between `a` and `b` (eased at both ends).
fn ease(a: f32, b: f32, t: f32) -> f32 {
    let x = ((t - a) / (b - a)).clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// A soft bell: eased in and out (no kink at the ends).
fn soft(t: f32) -> f32 {
    let s = bell(t);
    s * s
}

/// A wobble that dies away: what a soft body does after a squash. `u` 0..1.
fn settle(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    (-4.5 * u).exp() * (u * TAU * 1.6).cos() * (1.0 - u)
}

pub fn motion(p: &Pose) -> Motion {
    let t = p.t.clamp(0.0, 1.0);
    let mut m = Motion { lift: 0.0, dx: 0.0, tilt: p.lean, sx: 1.0, sy: 1.0, swing: p.lean * 3.0, mouth: Mouth::None, eyes: p.eyes, look: p.look, special: -1.0, open: p.open, fond: p.fond };
    if p.fond > 0.3 && p.act == Act::None {
        m.mouth = Mouth::Smile;
    }
    match p.act {
        Act::None => {
            if p.mark == Mark::Sleep {
                // Slow breathing: one tiny change every step of the z's.
                let b = (p.step % 2) as f32;
                m.sy = 1.0 + 0.04 * b;
                m.sx = 1.0 - 0.02 * b;
            }
        }
        Act::Wiggle => {
            let w = (t * 2.0 * TAU).sin() * soft(t);
            m.tilt += 0.17 * w;
            // Ears and tail trail the body a little.
            m.swing = (t * 2.0 * TAU - 0.7).sin() * soft(t);
            m.mouth = Mouth::Smile;
        }
        Act::LookAround => {
            let x = -ease(0.06, 0.22, t) + 2.0 * ease(0.44, 0.6, t) - ease(0.82, 0.96, t);
            m.look = (x, -0.2 * x.abs());
            m.tilt += 0.08 * x;
            m.swing = 0.6 * x;
        }
        Act::Stretch => {
            let k = soft(t / 0.85);
            let wobble = if t > 0.85 { settle((t - 0.85) / 0.15) * 0.05 } else { 0.0 };
            m.sy = 1.0 + 0.17 * k - wobble;
            m.sx = 1.0 - 0.08 * k + wobble * 0.5;
            m.swing = k;
            m.open = m.open.min(1.0 - ease(0.15, 0.4, t) * (1.0 - ease(0.7, 0.85, t)));
            if k > 0.3 {
                m.mouth = Mouth::Open(k * 0.6);
            }
        }
        Act::Yawn => {
            let k = soft(t);
            m.sy = 1.0 + 0.07 * k;
            m.tilt -= 0.08 * k;
            m.open = m.open.min(1.0 - 0.9 * ease(0.1, 0.35, t));
            if k > 0.15 {
                m.mouth = Mouth::Open(k);
            }
        }
        Act::Jump => {
            if t < 0.2 {
                // Crouch, eased.
                let k = ease(0.0, 0.2, t);
                m.sy = 1.0 - 0.16 * k;
                m.sx = 1.0 + 0.1 * k;
            } else if t < 0.72 {
                let k = (t - 0.2) / 0.52;
                let up = (PI * k).sin();
                m.lift = 5.5 * up;
                // Stretched on the way up, round at the top.
                m.sy = 1.0 + 0.1 * (1.0 - k).max(0.0) * up;
                m.sx = 1.0 - 0.06 * (1.0 - k).max(0.0) * up;
                m.eyes = Eyes::Happy;
                m.mouth = Mouth::Smile;
                m.swing = (k * TAU).sin();
            } else {
                // Land with a squash that wobbles out.
                let u = (t - 0.72) / 0.28;
                let w = settle(u);
                m.sy = 1.0 - 0.15 * w;
                m.sx = 1.0 + 0.09 * w;
                m.swing = -0.6 * w;
                m.eyes = Eyes::Happy;
            }
        }
        Act::Twirl => {
            let turn = ease(0.0, 1.0, t);
            let c = (turn * TAU).cos();
            m.sx = if c.abs() < 0.06 { 0.06 * c.signum() } else { c };
            m.lift = 2.5 * soft(t);
            m.eyes = Eyes::Happy;
            m.swing = (t * 2.0 * TAU).sin() * soft(t);
        }
        Act::Bounce => {
            let k = soft(t);
            m.lift = 3.5 * k;
            m.tilt += p.side * 0.13 * k;
            m.dx = p.side * 0.8 * k;
            // The tail / ears follow a beat later.
            m.swing = p.side * soft((t - 0.15).max(0.0) / 0.85);
            if t < 0.25 {
                let w = 1.0 - ease(0.0, 0.25, t);
                m.sy = 1.0 - 0.1 * w;
                m.sx = 1.0 + 0.05 * w;
            }
            m.mouth = Mouth::Smile;
        }
        Act::Special => m.special = t,
        Act::Pet => {
            // Leans into the hand, sways, purrs (tiny fast squash).
            let k = soft(t);
            m.tilt += 0.1 * (t * 3.0 * TAU).sin() * k;
            m.sy = 1.0 + 0.03 * (t * 14.0 * TAU).sin() * k;
            m.swing = (t * 3.0 * TAU - 0.8).sin() * k;
            m.eyes = Eyes::Happy;
            m.mouth = Mouth::Smile;
            m.fond = m.fond.max(k);
        }
        Act::Boop => {
            if t < 0.22 {
                // Squeezed: eyes shut tight, squashed down.
                let k = ease(0.0, 0.22, t);
                m.sy = 1.0 - 0.2 * k;
                m.sx = 1.0 + 0.12 * k;
                m.open = 0.0;
                m.eyes = Eyes::Closed;
            } else {
                // Pops up, then wobbles back.
                let u = (t - 0.22) / 0.78;
                m.lift = 4.0 * bell((u * 2.0).min(1.0));
                let w = settle(u);
                m.sy = 1.0 + 0.12 * w;
                m.sx = 1.0 - 0.07 * w;
                m.swing = w;
                m.eyes = Eyes::Happy;
                m.mouth = Mouth::Open(0.4 * (1.0 - u));
            }
            m.fond = m.fond.max(soft(t));
        }
    }
    m
}

// -------------------------------------------------------------- helpers --

const INK: u32 = 0x15151f;

fn mix(dark: u32, light_theme: u32, light: f32, a: f32) -> Rgba {
    Rgba::hex(dark).mix(Rgba::hex(light_theme), light).alpha(a)
}

/// Starts drawing the body with the motion's lift, lean and squash about the feet at `feet_y`.
fn begin(g: &Gfx, m: &Motion, cx: f32, feet_y: f32) {
    FOND.with(|f| f.set(m.fond));
    g.pose_begin(cx, feet_y, m.dx, -m.lift, m.tilt, m.sx, m.sy);
}

/// Round eyes with a small shine, or closed / happy lines.
fn eyes(g: &Gfx, m: &Motion, cx: f32, cy: f32, gap: f32, r: f32, a: f32) {
    eyes_in(g, m, cx, cy, gap, r, Rgba::hex(INK).alpha(a), a);
}

#[allow(clippy::too_many_arguments)]
fn eyes_in(g: &Gfx, m: &Motion, cx: f32, cy: f32, gap: f32, r: f32, ink: Rgba, a: f32) {
    for side in [-1.0, 1.0] {
        let (x, y) = (cx + side * gap, cy);
        let eyes = if m.eyes == Eyes::Open && m.open < 0.18 { Eyes::Closed } else { m.eyes };
        match eyes {
            Eyes::Open => {
                // Eyelids squash the eye from above and below (a soft blink).
                let (ex, ey) = (x + m.look.0 * r * 0.6, y + m.look.1 * r * 0.5);
                let ry = r * m.open.clamp(0.0, 1.0);
                g.fill_ellipse(ex, ey, r, ry, ink);
                if m.open > 0.6 {
                    g.fill_circle(ex - r * 0.38, ey - r * 0.38, r * 0.36, Rgba::hex(0xffffff).alpha(a * 0.9 * (m.open - 0.6) / 0.4));
                }
            }
            Eyes::Closed => g.line(x - r, y + 0.5, x + r, y + 0.5, 1.5, ink),
            Eyes::Happy => {
                g.line(x - r, y + r * 0.6, x, y - r * 0.5, 1.5, ink);
                g.line(x, y - r * 0.5, x + r, y + r * 0.6, 1.5, ink);
            }
        }
    }
}

/// A curved line through three points (drawn as short segments).
fn curve(g: &Gfx, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), width: f32, c: Rgba) {
    let at = |t: f32| {
        let u = 1.0 - t;
        (u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0, u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1)
    };
    let mut last = p0;
    for i in 1..=6 {
        let p = at(i as f32 / 6.0);
        g.line(last.0, last.1, p.0, p.1, width, c);
        last = p;
    }
}

/// The mouth at (cx, cy): `w` is half its width. `idle` is drawn when the
/// motion asks for none (e.g. a cat's little "w").
fn mouth(g: &Gfx, m: &Motion, cx: f32, cy: f32, w: f32, a: f32, idle: Mouth) {
    let ink = Rgba::hex(INK).alpha(a);
    let shape = if m.mouth == Mouth::None { idle } else { m.mouth };
    match shape {
        Mouth::None => {}
        Mouth::Smile => curve(g, (cx - w, cy - w * 0.15), (cx, cy + w * 0.9), (cx + w, cy - w * 0.15), 1.2, ink),
        Mouth::Open(k) => {
            let (rx, ry) = (w * (0.55 + 0.25 * k), w * (0.35 + 0.75 * k));
            g.fill_ellipse(cx, cy + ry * 0.5, rx, ry, ink);
            g.fill_ellipse(cx, cy + ry * 1.0, rx * 0.6, ry * 0.4, Rgba::hex(0xff7d9c).alpha(a));
        }
    }
}

/// The cat-style "w" mouth.
fn cat_mouth(g: &Gfx, cx: f32, cy: f32, w: f32, c: Rgba) {
    curve(g, (cx - w, cy), (cx - w * 0.5, cy + w * 0.8), (cx, cy), 1.0, c);
    curve(g, (cx, cy), (cx + w * 0.5, cy + w * 0.8), (cx + w, cy), 1.0, c);
}

/// Pink cheeks; deeper while it is being fussed over (`fond`).
fn blush(g: &Gfx, cx: f32, cy: f32, gap: f32, r: f32, a: f32) {
    let fond = FOND.with(|f| f.get());
    for side in [-1.0, 1.0] {
        let r = r * (1.0 + 0.25 * fond);
        g.fill_ellipse(cx + side * gap, cy, r, r * 0.65, Rgba::hex(0xff8fb0).alpha(a * (0.5 + 0.4 * fond)));
    }
}

thread_local! {
    /// The `fond` of the character being drawn (read by `blush`, set by `begin`).
    static FOND: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

fn heart(g: &Gfx, x: f32, y: f32, size: f32, c: Rgba) {
    let r = size * 0.32;
    g.fill_circle(x - r * 0.95, y - r * 0.3, r, c);
    g.fill_circle(x + r * 0.95, y - r * 0.3, r, c);
    g.fill_poly(&[(x - r * 1.9, y - r * 0.05), (x + r * 1.9, y - r * 0.05), (x, y + size * 0.62)], c);
}

/// Thinking dots or sleep z's above and to the right of the character, and
/// hearts floating up while it is stroked or booped.
fn mark(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    if matches!(pose.act, Act::Pet | Act::Boop) {
        let n = if pose.act == Act::Pet { 3 } else { 1 };
        for i in 0..n {
            // Each heart rises and fades over its own part of the act.
            let start = i as f32 * 0.22;
            let u = ((pose.t - start) / 0.7).clamp(0.0, 1.0);
            if u <= 0.0 || u >= 1.0 {
                continue;
            }
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            let x = cx + side * (6.0 + 3.0 * i as f32) + (u * TAU).sin() * 1.5;
            let y = cy - 11.0 - 8.0 * ease(0.0, 1.0, u);
            let fade = (1.0 - u) * ease(0.0, 0.15, u);
            heart(g, x, y, 4.0 + 1.5 * u, Rgba::hex(0xff6f9a).alpha(a * fade));
        }
    }
    let c = Rgba::hex(0xdcdcf0).mix(Rgba::hex(0x45455a), light);
    match pose.mark {
        Mark::None => {}
        Mark::Thinking => {
            for i in 0..3 {
                let bright = i as u8 == pose.step % 3;
                let lift = if bright { 1.2 } else { 0.0 };
                g.fill_circle(cx + 12.0 + i as f32 * 4.6, cy - 13.0 - i as f32 * 1.6 - lift, 1.7, c.alpha(a * if bright { 1.0 } else { 0.4 }));
            }
        }
        Mark::Sleep => {
            // Two z's drifting up a little with every step.
            let drift = (pose.step % 4) as f32 * 0.8;
            for (i, size) in [3.0f32, 2.2].into_iter().enumerate() {
                let (x, y) = (cx + 12.0 + i as f32 * 6.0, cy - 9.0 - i as f32 * 6.0 - drift);
                let ink = c.alpha(a * (0.9 - i as f32 * 0.3));
                g.line(x - size, y - size, x + size, y - size, 1.3, ink);
                g.line(x + size, y - size, x - size, y + size, 1.3, ink);
                g.line(x - size, y + size, x + size, y + size, 1.3, ink);
            }
        }
    }
}

// --------------------------------------------------------------- the cast --

fn cat(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let fur = mix(0xf4b26a, 0xe08a3c, light, a);
    let dark = mix(0xd4813e, 0xb5622a, light, a);
    let cream = mix(0xffe6c9, 0xf5cfa6, light, a);
    // Trick: licks a raised paw, eyes shut, an ear twitching.
    let lick = if m.special >= 0.0 { bell(m.special) } else { 0.0 };
    if lick > 0.0 {
        m.eyes = Eyes::Happy;
        m.swing = (m.special * 3.0 * TAU).sin() * 0.6;
    }
    begin(g, &m, cx, cy + 12.5);
    // Tail behind, curling up on the right; it swings.
    let s = m.swing;
    curve(g, (cx + 5.0, cy + 10.0), (cx + 14.0, cy + 9.0 - 2.0 * s), (cx + 13.0 + 3.0 * s, cy - 1.0 - 2.0 * s.abs()), 3.0, dark);
    g.fill_ellipse(cx, cy + 8.5, 7.5, 4.2, fur);
    g.fill_circle(cx - 3.6, cy + 11.4, 2.1, cream);
    if lick <= 0.0 {
        g.fill_circle(cx + 3.6, cy + 11.4, 2.1, cream);
    }
    // Ears: thick round-capped strokes that meet at the tips read as triangles.
    for side in [-1.0f32, 1.0] {
        let twitch = if side > 0.0 { lick * 2.0 * (m.special * 4.0 * TAU).sin().max(0.0) } else { 0.0 };
        let (base, tip) = ((cx + side * 7.0, cy - 4.5), (cx + side * (8.0 + twitch), cy - 13.5 + twitch));
        g.line(base.0, base.1, tip.0, tip.1, 5.0, fur);
        g.line(cx + side * 2.8, cy - 7.5, tip.0, tip.1, 4.0, fur);
        g.line(cx + side * 7.2, cy - 7.0, tip.0, tip.1 + 2.5, 1.6, Rgba::hex(0xffc9d6).alpha(a * 0.9));
    }
    g.fill_circle(cx, cy - 1.0, 9.6, fur);
    // Forehead stripes.
    for dx in [-2.4f32, 0.0, 2.4] {
        g.line(cx + dx, cy - 10.0, cx + dx * 0.7, cy - 7.2, 1.1, dark);
    }
    g.fill_ellipse(cx, cy + 3.0, 4.6, 3.2, cream);
    if m.fond > 0.02 {
        blush(g, cx, cy + 1.8, 6.2, 1.8, a * m.fond);
    }
    eyes(g, &m, cx, cy - 2.0, 4.0, 1.85, a);
    g.fill_circle(cx, cy + 1.2, 1.1, Rgba::hex(0xff7aa0).alpha(a));
    let ink = Rgba::hex(INK).alpha(a);
    match m.mouth {
        Mouth::None | Mouth::Smile => cat_mouth(g, cx, cy + 2.3, 1.8, ink.alpha(a * 0.8)),
        open => mouth(g, &Motion { mouth: open, ..m }, cx, cy + 2.4, 2.0, a, Mouth::None),
    }
    let whisker = ink.alpha(a * 0.5);
    for side in [-1.0f32, 1.0] {
        g.line(cx + side * 5.0, cy + 2.6, cx + side * 12.5, cy + 1.2, 0.8, whisker);
        g.line(cx + side * 5.0, cy + 3.8, cx + side * 12.5, cy + 5.0, 0.8, whisker);
    }
    if lick > 0.0 {
        // The raised paw at the mouth.
        g.fill_circle(cx + 3.6 - lick * 2.0, cy + 11.4 - lick * 8.0, 2.3, cream);
    }
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn ghost(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    // Trick: fades away, then pops back with a "boo".
    let mut fade = 1.0;
    if m.special >= 0.0 {
        let t = m.special;
        if t < 0.55 {
            fade = 1.0 - 0.75 * bell(t / 1.1);
        } else {
            let k = bell((t - 0.55) / 0.45);
            m.mouth = Mouth::Open(k);
            m.sy = 1.0 + 0.12 * k;
            m.sx = 1.0 + 0.08 * k;
            m.swing = k;
        }
    }
    // Ghosts float a little above the ground.
    m.lift += 1.5;
    let a = a * fade;
    let body = mix(0xe4e4ff, 0x7a7af0, light * 0.7, a);
    let shade = mix(0xc4c4f0, 0x6464d8, light * 0.7, a);
    begin(g, &m, cx, cy + 12.0);
    // Arms: little nubs that wave with the swing.
    for side in [-1.0f32, 1.0] {
        let up = (m.swing * side).max(0.0) * 4.0;
        g.fill_ellipse(cx + side * 11.0, cy + 2.5 - up, 2.6, 3.4, shade);
    }
    g.fill_circle(cx, cy - 2.0, 11.0, body);
    g.fill_rrect(cx - 11.0, cy - 2.0, 22.0, 11.0, 2.0, body);
    let sway = m.swing * 1.4;
    for i in 0..3 {
        g.fill_circle(cx - 7.3 + i as f32 * 7.3 + sway, cy + 9.0, 3.7, body);
    }
    eyes(g, &m, cx, cy - 3.0, 4.4, 2.5, a);
    blush(g, cx, cy + 1.5, 7.5, 2.0, a);
    mouth(g, &m, cx, cy + 2.0, 2.2, a, Mouth::None);
    g.pose_end();
    mark(g, pose, cx, cy, a / fade, light);
}

fn jellyfish(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let m = motion(pose);
    let bell_c = mix(0xff9fe0, 0xd65cb4, light, a);
    let frill = mix(0xff7fd0, 0xbf4aa0, light, a);
    let cy = cy - 2.0;
    // Trick: glows and lets out bubbles.
    let glow = if m.special >= 0.0 { bell(m.special) } else { 0.0 };
    if glow > 0.0 {
        // A soft bright halo: a few faint rings, brighter towards the bell.
        for (r, k) in [(15.0, 0.10), (13.0, 0.16), (11.5, 0.24)] {
            g.fill_circle(cx, cy + 1.0, r + 1.5 * glow, Rgba::hex(0xffc4ef).alpha(a * k * glow));
        }
        for i in 0..3 {
            let t = (m.special * 1.6 - i as f32 * 0.3).clamp(0.0, 1.0);
            if t > 0.0 && t < 1.0 {
                g.stroke_circle(cx + 9.0 + i as f32 * 2.5, cy - 4.0 - t * 14.0, 1.2 + i as f32 * 0.4, 0.8, Rgba::hex(0xffffff).alpha(a * 0.6 * (1.0 - t)));
            }
        }
    }
    begin(g, &m, cx, cy + 14.0);
    // Tentacles first, so the bell sits on top; they ripple with the step and swing.
    let phase = pose.step as f32 * 1.7 + pose.t * TAU * 2.0;
    for i in 0..4 {
        let x = cx - 6.0 + i as f32 * 4.0;
        let wave = (phase + i as f32 * 1.3).sin() * 1.5 + m.swing * 1.5;
        let mid = (x + wave, cy + 8.0);
        g.line(x, cy + 3.0, mid.0, mid.1, 1.6, bell_c.alpha(a * 0.85));
        g.line(mid.0, mid.1, x - wave * 0.6, cy + 13.0, 1.4, bell_c.alpha(a * 0.7));
    }
    g.fill_circle(cx, cy, 10.0, bell_c);
    g.fill_rrect(cx - 10.0, cy - 1.0, 20.0, 5.0, 2.5, bell_c);
    // A frilly hem.
    for i in 0..5 {
        g.fill_circle(cx - 8.0 + i as f32 * 4.0, cy + 4.0, 2.0, frill);
    }
    // Spots and a highlight on the bell.
    g.fill_circle(cx - 4.5, cy - 5.5, 2.2, Rgba::hex(0xffffff).alpha(a * 0.45));
    g.fill_circle(cx + 5.0, cy - 6.0, 1.1, Rgba::hex(0xffffff).alpha(a * 0.3));
    eyes(g, &m, cx, cy - 0.5, 3.8, 1.8, a);
    mouth(g, &m, cx, cy + 1.6, 1.6, a, Mouth::None);
    blush(g, cx, cy + 1.5, 6.5, 1.6, a);
    g.pose_end();
    mark(g, pose, cx, cy + 2.0, a, light);
}

fn fox(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let orange = mix(0xff8f45, 0xe8691c, light, a);
    let white = mix(0xfff4e8, 0xf0dcc8, light, a);
    let dark = mix(0x3b2a2e, 0x2a1c20, light, a);
    // Trick: wags its big tail, very pleased.
    if m.special >= 0.0 {
        m.swing = (m.special * 5.0 * TAU).sin() * bell(m.special);
        m.eyes = Eyes::Happy;
        m.mouth = Mouth::Smile;
    }
    begin(g, &m, cx, cy + 12.5);
    // Tail: a big bushy brush curling up behind on the right, white at the tip.
    let s = m.swing;
    let (bx, by) = (cx + 4.0, cy + 10.0);
    let tip = (cx + 15.0 + 2.5 * s, cy + 1.0 - 1.5 * s.abs());
    let bend = (cx + 16.0 + 1.0 * s, cy + 9.5);
    // A fat body along a curve: overlapping circles growing then shrinking.
    let at = |t: f32| {
        let u = 1.0 - t;
        (u * u * bx + 2.0 * u * t * bend.0 + t * t * tip.0, u * u * by + 2.0 * u * t * bend.1 + t * t * tip.1)
    };
    for i in 0..=8 {
        let t = i as f32 / 8.0;
        let (x, y) = at(t);
        let r = 2.2 + 2.6 * bell(t * 0.8 + 0.1);
        g.fill_circle(x, y, r, if t > 0.78 { white } else { orange });
    }
    g.fill_ellipse(cx, cy + 8.5, 7.0, 4.2, orange);
    g.fill_ellipse(cx, cy + 9.0, 3.8, 3.0, white);
    for side in [-1.0f32, 1.0] {
        g.fill_circle(cx + side * 3.6, cy + 11.6, 1.9, dark);
    }
    // Pointed ears with dark tips.
    for side in [-1.0f32, 1.0] {
        let (b0, b1, t) = ((cx + side * 2.5, cy - 6.5), (cx + side * 9.5, cy - 5.0), (cx + side * 8.5, cy - 15.5));
        g.fill_poly(&[b0, b1, t], orange);
        g.fill_poly(&[(cx + side * 7.0, cy - 11.5), (cx + side * 9.2, cy - 11.0), t], dark);
        g.fill_poly(&[(cx + side * 4.5, cy - 6.5), (cx + side * 8.0, cy - 6.0), (cx + side * 7.6, cy - 11.5)], Rgba::hex(0xffd1b8).alpha(a * 0.8));
    }
    g.fill_circle(cx, cy - 1.0, 8.8, orange);
    // White cheeks meeting at the chin.
    g.fill_poly(&[(cx - 9.0, cy + 0.5), (cx, cy + 7.5), (cx + 9.0, cy + 0.5), (cx + 4.0, cy + 1.5), (cx, cy + 0.0), (cx - 4.0, cy + 1.5)], white);
    g.fill_ellipse(cx, cy + 4.0, 5.2, 3.6, white);
    if m.fond > 0.02 {
        blush(g, cx, cy + 1.5, 5.8, 1.7, a * m.fond);
    }
    eyes(g, &m, cx, cy - 2.2, 3.7, 1.75, a);
    g.fill_ellipse(cx, cy + 2.0, 1.5, 1.1, dark);
    mouth(g, &m, cx, cy + 3.4, 1.6, a, Mouth::None);
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn frog(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let green = mix(0x86e05c, 0x4fb42e, light, a);
    let belly = mix(0xdcf7b8, 0xb8e690, light, a);
    let dark = mix(0x3f8a2a, 0x2c6a1c, light, a);
    // Trick: flicks its tongue at a passing fly, then gulps.
    let mut tongue = 0.0;
    if m.special >= 0.0 {
        let t = m.special;
        tongue = if t < 0.55 { bell(t / 0.55) } else { 0.0 };
        if t >= 0.55 {
            let k = bell((t - 0.55) / 0.45);
            m.eyes = Eyes::Happy;
            m.sy = 1.0 - 0.06 * k;
            m.sx = 1.0 + 0.05 * k;
            m.mouth = Mouth::Smile;
        } else {
            m.look = (1.0, -0.3);
        }
        if t < 0.3 {
            g.fill_circle(cx + 17.0 + (t * 40.0).sin(), cy - 1.0 + (t * 55.0).cos(), 1.1, Rgba::hex(INK).alpha(a * 0.8));
        }
    }
    begin(g, &m, cx, cy + 12.0);
    // Feet.
    for side in [-1.0f32, 1.0] {
        g.fill_ellipse(cx + side * 8.0, cy + 10.8, 3.4, 1.7, dark);
    }
    g.fill_ellipse(cx, cy + 4.0, 11.0, 7.8, green);
    g.fill_ellipse(cx, cy + 6.5, 7.0, 4.2, belly);
    // Eye bulbs on top of the head.
    for side in [-1.0f32, 1.0] {
        g.fill_circle(cx + side * 5.5, cy - 3.0, 4.4, green);
        g.fill_circle(cx + side * 5.5, cy - 3.0, 3.2, Rgba::hex(0xffffff).alpha(a));
    }
    eyes(g, &m, cx, cy - 3.0, 5.5, 1.7, a);
    blush(g, cx, cy + 2.6, 7.8, 1.8, a);
    let ink = Rgba::hex(INK).alpha(a);
    match m.mouth {
        Mouth::Open(_) => mouth(g, &m, cx, cy + 2.0, 3.0, a, Mouth::None),
        _ => curve(g, (cx - 5.5, cy + 1.6), (cx, cy + 4.8), (cx + 5.5, cy + 1.6), 1.2, ink),
    }
    if tongue > 0.0 {
        let end = (cx + 3.0 + 14.0 * tongue, cy + 2.5 - 3.5 * tongue);
        g.line(cx + 1.0, cy + 3.4, end.0, end.1, 1.7, Rgba::hex(0xff6f8e).alpha(a));
        g.fill_circle(end.0, end.1, 1.6, Rgba::hex(0xff6f8e).alpha(a));
    }
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn owl(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let brown = mix(0xa9765a, 0x7a4d33, light, a);
    let wing = mix(0x8a5c44, 0x633b26, light, a);
    let disc = mix(0xf0dcc0, 0xdcc19e, light, a);
    // Trick: turns its head right round (the face slides across) and back.
    let mut turn = 0.0;
    if m.special >= 0.0 {
        turn = (m.special * TAU).sin() * 4.0;
        if m.special > 0.4 && m.special < 0.6 {
            m.eyes = Eyes::Happy;
        }
        m.swing = bell(m.special) * (m.special * 4.0 * TAU).sin();
    }
    begin(g, &m, cx, cy + 12.5);
    // Wings flap with the swing.
    for side in [-1.0f32, 1.0] {
        let flap = (m.swing * side).max(0.0) * 3.5;
        g.fill_ellipse(cx + side * (9.0 + flap * 0.4), cy + 3.0 - flap, 3.0, 6.5, wing);
    }
    // Ear tufts and body.
    for side in [-1.0f32, 1.0] {
        g.fill_poly(&[(cx + side * 3.0, cy - 8.0), (cx + side * 9.0, cy - 14.0), (cx + side * 9.0, cy - 5.0)], brown);
    }
    g.fill_ellipse(cx, cy + 1.0, 9.8, 11.2, brown);
    // Belly feathers.
    for (dx, dy) in [(-3.0f32, 6.0f32), (0.0, 8.0), (3.0, 6.0)] {
        curve(g, (cx + dx - 1.3, cy + dy), (cx + dx, cy + dy + 1.4), (cx + dx + 1.3, cy + dy), 0.9, disc.alpha(a * 0.7));
    }
    let fx = cx + turn;
    for side in [-1.0f32, 1.0] {
        g.fill_circle(fx + side * 3.9, cy - 2.5, 4.6, disc);
        g.fill_circle(fx + side * 3.9, cy - 2.5, 3.2, Rgba::hex(0xffc94a).alpha(a));
    }
    eyes(g, &m, fx, cy - 2.5, 3.9, 1.8, a);
    g.fill_poly(&[(fx - 1.5, cy + 0.6), (fx + 1.5, cy + 0.6), (fx, cy + 3.4)], Rgba::hex(0xff9f2e).alpha(a));
    if let Mouth::Open(k) = m.mouth {
        g.fill_ellipse(fx, cy + 2.8, 1.2, 0.8 + k, Rgba::hex(INK).alpha(a));
    }
    // Little feet.
    for side in [-1.0f32, 1.0] {
        g.fill_ellipse(cx + side * 3.4, cy + 12.0, 1.9, 1.1, Rgba::hex(0xff9f2e).alpha(a));
    }
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn bunny(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let fur = mix(0xf6f2ff, 0xd8cfee, light, a);
    let shade = mix(0xe2dcf2, 0xc2b8de, light, a);
    let pink = Rgba::hex(0xffb3c9).alpha(a);
    // Trick: one ear flops over, the nose twitches, the ear springs back.
    let mut flop = 0.0;
    let mut sniff = 0.0;
    if m.special >= 0.0 {
        flop = bell(m.special);
        sniff = (m.special * 6.0 * TAU).sin() * 0.6;
        m.look = (0.0, -0.4);
    }
    begin(g, &m, cx, cy + 12.5);
    // Long ears.
    for side in [-1.0f32, 1.0] {
        let base = (cx + side * 3.4, cy - 7.0);
        let up = (cx + side * (4.5 + m.swing * side * 1.5), cy - 21.0);
        let flopped = (cx + side * 14.0, cy - 9.0);
        let k = if side > 0.0 { flop } else { 0.0 };
        let tip = (up.0 + (flopped.0 - up.0) * k, up.1 + (flopped.1 - up.1) * k);
        g.line(base.0, base.1, tip.0, tip.1, 5.2, fur);
        g.line(base.0 + (tip.0 - base.0) * 0.15, base.1 + (tip.1 - base.1) * 0.15, base.0 + (tip.0 - base.0) * 0.85, base.1 + (tip.1 - base.1) * 0.85, 2.0, pink.alpha(a * 0.8));
    }
    g.fill_ellipse(cx, cy + 8.6, 7.0, 4.2, shade);
    for side in [-1.0f32, 1.0] {
        g.fill_ellipse(cx + side * 4.2, cy + 11.6, 2.8, 1.6, fur);
    }
    g.fill_circle(cx, cy - 0.5, 8.9, fur);
    eyes(g, &m, cx, cy - 1.8, 3.7, 1.8, a);
    blush(g, cx, cy + 2.0, 5.8, 1.7, a);
    g.fill_ellipse(cx + sniff, cy + 1.4, 1.3, 0.95, pink);
    let ink = Rgba::hex(INK).alpha(a * 0.8);
    match m.mouth {
        Mouth::Open(_) => mouth(g, &m, cx, cy + 2.6, 2.0, a, Mouth::None),
        _ => {
            cat_mouth(g, cx + sniff * 0.5, cy + 2.4, 1.5, ink);
            g.fill_rrect(cx - 1.1, cy + 3.3, 2.2, 1.8, 0.4, Rgba::hex(0xffffff).alpha(a));
        }
    }
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn robot(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let m = motion(pose);
    let metal = mix(0xc3cde0, 0x8e9bb2, light, a);
    let edge = mix(0x9aa6bd, 0x6f7c94, light, a);
    let screen = Rgba::hex(0x1b2130).alpha(a);
    let glow = Rgba::hex(0x5cf2ff).alpha(a);
    let scan = if m.special >= 0.0 { bell(m.special) } else { 0.0 };
    begin(g, &m, cx, cy + 12.5);
    // Antenna with a light: lit while thinking, blinking during the trick.
    let lit = pose.mark == Mark::Thinking && pose.step.is_multiple_of(2) || (scan > 0.0 && ((m.special * 8.0) as u32).is_multiple_of(2));
    let sway = m.swing * 1.5;
    g.line(cx, cy - 9.0, cx + sway, cy - 13.5, 1.3, edge);
    g.fill_circle(cx + sway, cy - 14.5, 1.9, if lit { Rgba::hex(0xff5c7a).alpha(a) } else { edge });
    // Arms.
    for side in [-1.0f32, 1.0] {
        let up = (m.swing * side).max(0.0) * 4.0;
        g.line(cx + side * 6.5, cy + 9.0, cx + side * 9.5, cy + 10.5 - up, 1.6, edge);
        g.fill_circle(cx + side * 9.8, cy + 10.8 - up, 1.4, edge);
    }
    g.fill_rrect(cx - 6.0, cy + 7.0, 12.0, 5.5, 2.0, metal);
    g.fill_circle(cx, cy + 9.6, 1.0, glow.alpha(a * 0.7));
    g.fill_rrect(cx - 10.5, cy - 9.5, 21.0, 17.0, 5.0, metal);
    for side in [-1.0f32, 1.0] {
        g.fill_circle(cx + side * 11.0, cy - 1.0, 1.6, edge);
    }
    g.fill_rrect(cx - 7.8, cy - 7.0, 15.6, 11.5, 3.2, screen);
    // Pink pixels for cheeks while it is fussed over.
    if m.fond > 0.02 {
        for side in [-1.0f32, 1.0] {
            g.fill_rrect(cx + side * 5.4 - 1.0, cy + 0.2, 2.0, 1.2, 0.4, Rgba::hex(0xff7aa8).alpha(a * m.fond));
        }
    }
    // Face on the screen.
    let (lx, ly) = (m.look.0 * 1.2, m.look.1 * 0.8);
    for side in [-1.0f32, 1.0] {
        let (x, y) = (cx + side * 3.4 + lx, cy - 2.6 + ly);
        let eyes = if m.eyes == Eyes::Open && m.open < 0.18 { Eyes::Closed } else { m.eyes };
        match eyes {
            Eyes::Open => {
                let h = 3.6 * m.open.clamp(0.0, 1.0);
                g.fill_rrect(x - 1.3, y - h * 0.5, 2.6, h, 1.2_f32.min(h * 0.5), glow);
            }
            Eyes::Closed => g.line(x - 1.5, y, x + 1.5, y, 1.2, glow),
            Eyes::Happy => {
                g.line(x - 1.5, y + 0.8, x, y - 0.8, 1.2, glow);
                g.line(x, y - 0.8, x + 1.5, y + 0.8, 1.2, glow);
            }
        }
    }
    match m.mouth {
        Mouth::Smile => curve(g, (cx - 2.4, cy + 1.0), (cx, cy + 2.8), (cx + 2.4, cy + 1.0), 1.1, glow),
        Mouth::Open(k) => g.fill_ellipse(cx, cy + 1.6, 1.4, 0.7 + k, glow),
        Mouth::None => g.line(cx - 1.6, cy + 1.6, cx + 1.6, cy + 1.6, 1.1, glow.alpha(a * 0.8)),
    }
    if scan > 0.0 {
        let x = cx - 7.0 + 14.0 * m.special;
        g.line(x, cy - 6.2, x, cy + 3.7, 1.2, glow.alpha(a * 0.8 * scan));
    }
    g.pose_end();
    mark(g, pose, cx, cy, a, light);
}

fn axolotl(g: &Gfx, pose: &Pose, cx: f32, cy: f32, a: f32, light: f32) {
    let mut m = motion(pose);
    let pink = mix(0xffbad8, 0xf07fb0, light, a);
    let gill = mix(0xff6f9f, 0xe0457c, light, a);
    let belly = mix(0xffe3ef, 0xffc6dc, light, a);
    // Trick: blows a string of bubbles.
    let mut puff = 0.0;
    if m.special >= 0.0 {
        puff = bell(m.special);
        m.mouth = Mouth::Open(0.3 * puff);
        m.swing = (m.special * 3.0 * TAU).sin() * 0.5;
    }
    begin(g, &m, cx, cy + 12.0);
    // Tail and body.
    let s = m.swing;
    curve(g, (cx + 4.0, cy + 9.0), (cx + 12.0, cy + 10.0 + 2.0 * s), (cx + 16.0, cy + 6.0 - 2.0 * s), 3.0, pink);
    g.fill_ellipse(cx + 1.5, cy + 8.6, 7.5, 3.8, pink);
    g.fill_ellipse(cx + 1.0, cy + 9.6, 4.5, 2.0, belly);
    // Feathery gills, three a side.
    for side in [-1.0f32, 1.0] {
        for i in 0..3 {
            let fi = i as f32;
            let root = (cx + side * 7.5, cy - 4.0 + fi * 3.2);
            let end = (cx + side * (13.5 + (s * side).max(0.0) * 1.5), cy - 8.5 + fi * 4.6 - s * side * 0.8);
            g.line(root.0, root.1, end.0, end.1, 2.0, gill);
            g.fill_circle(end.0, end.1, 1.5, gill);
        }
    }
    g.fill_ellipse(cx, cy, 10.5, 8.2, pink);
    eyes(g, &m, cx, cy - 1.0, 5.4, 1.5, a);
    blush(g, cx, cy + 2.6, 6.8, 1.7, a);
    mouth(g, &m, cx, cy + 2.4, 2.6, a, Mouth::Smile);
    g.pose_end();
    if puff > 0.0 {
        for i in 0..3 {
            let t = (m.special * 1.5 - i as f32 * 0.25).clamp(0.0, 1.0);
            if t > 0.0 && t < 1.0 {
                g.stroke_circle(cx + 3.0 + i as f32 * 3.0 + (t * 9.0).sin(), cy - 1.0 - t * 15.0, 1.2 + i as f32 * 0.5, 0.8, Rgba::hex(0xbfe9ff).alpha(a * 0.8 * (1.0 - t)));
            }
        }
    }
    mark(g, pose, cx, cy, a, light);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_looks() {
        assert_eq!(find("Cat"), Some(0));
        assert_eq!(find(" jellyfish "), Some(2));
        assert_eq!(find("axolotl"), Some(CAST.len() - 1));
        assert_eq!(find("none"), None);
        // Right on it: straight ahead; close by: towards the cursor, further
        // the further it is; far away: one of eight directions, in full.
        assert_eq!(look_at(3.0, 4.0), (0.0, 0.0));
        assert_eq!(look_at(200.0, 1.0), (1.0, 0.0));
        let (x, y) = look_at(-50.0, 0.0);
        assert!((-0.55..=-0.45).contains(&x) && y == 0.0, "{x} {y}");
        assert_eq!(look_at(500.0, 500.0), (0.7, 0.7));
        let near = look_at(30.0, 30.0);
        assert!(near.0 > 0.2 && near.0 < 0.45 && near.0 == near.1, "{near:?}");
        // Small cursor moves inside one direction give the same look (no redraw).
        assert_eq!(look_at(300.0, 20.0), look_at(320.0, 25.0));
    }

    #[test]
    fn acts_start_and_end_at_rest() {
        for act in Act::ALL {
            for t in [0.0, 1.0] {
                let m = motion(&Pose { act, t, ..Default::default() });
                if act == Act::Special {
                    continue;
                }
                assert!(m.lift.abs() < 0.01 && m.tilt.abs() < 0.03 && (m.sy - 1.0).abs() < 0.11 && (m.sx.abs() - 1.0).abs() < 0.11, "{act:?} at {t}: {m:?}");
            }
            // Every act moves something in the middle.
            let mid = motion(&Pose { act, t: 0.3, ..Default::default() });
            if act != Act::None {
                let moved = mid.lift.abs() + mid.tilt.abs() + (mid.sy - 1.0).abs() + (mid.sx - 1.0).abs() + mid.swing.abs() + mid.look.0.abs() + if mid.special >= 0.0 { 1.0 } else { 0.0 };
                assert!(moved > 0.05, "{act:?} does nothing");
            }
        }
    }
}
