//! `--demo`: loops through the nine reference scenes with fake data and a
//! scripted cursor, so the island can be compared side by side with the
//! reference animation. The real mouse and game-suspend are ignored.

use crate::anim::smoothstep;
use crate::render::{ws, Rgba};
use crate::state::*;
use crate::theme;
use crate::ui;
use windows::core::{w, PCWSTR};

const CAPTIONS: [PCWSTR; 9] = [
    w!("1 \u{00B7} Reaches for the cursor"),
    w!("2 \u{00B7} Music in the colour of the cover"),
    w!("3 \u{00B7} Opens on a hover"),
    w!("4 \u{00B7} The AI chat reads your files"),
    w!("5 \u{00B7} Control centre"),
    w!("6 \u{00B7} Downloads from any app"),
    w!("7 \u{00B7} Auto-hide: back from the edge"),
    w!("8 \u{00B7} Characters watch the cursor"),
    w!("9 \u{00B7} A light theme"),
];
const DURATION: [f32; 9] = [6.5, 8.2, 6.6, 4.6, 5.0, 9.6, 6.6, 6.2, 7.2];

const COVER: Rgba = Rgba::hex(0xff7a45);
const COVER2: Rgba = Rgba::hex(0xff3d8b);
const AWAY: (f32, f32) = (CX + 270.0, 330.0);

pub struct Demo {
    scene: usize,
    t: f32,
    fired: u32,
    /// Hide the caption and the fake cursor (used when measuring CPU).
    bare: bool,
}

impl Demo {
    pub fn new(bare: bool) -> Self {
        Self { scene: 0, t: 0.0, fired: 0, bare }
    }

    /// `true` exactly once per scene, the first time the scene clock passes `at`.
    fn once(&mut self, bit: u32, at: f32) -> bool {
        if self.t >= at && self.fired & (1 << bit) == 0 {
            self.fired |= 1 << bit;
            true
        } else {
            false
        }
    }

    pub fn step(&mut self, dt: f32, s: &mut State) {
        self.t += dt;
        if self.t >= DURATION[self.scene] {
            self.scene = (self.scene + 1) % DURATION.len();
            self.t = 0.0;
            self.fired = 0;
        }
        if !self.bare {
            s.demo_cursor = true;
            s.caption = ws(CAPTIONS[self.scene]);
        }
        let t = self.t;
        let on_pill = (CX + 10.0, 46.0);

        match self.scene {
            // 1. Reaches for the cursor.
            0 => {
                if self.once(0, 0.0) {
                    s.set_mode(Mode::Idle);
                    s.set_pill(Pill::Clock);
                    s.set_music(false, COVER, COVER2);
                    s.set_line(false, COVER);
                }
                s.cursor = path(t, &[(0.0, CX - 190.0, 280.0), (1.2, CX - 70.0, 150.0), (2.2, CX - 50.0, 106.0), (3.4, CX + 96.0, 100.0), (4.6, CX - 20.0, 96.0), (5.6, CX + 150.0, 260.0)]);
            }
            // 2. Music in the colour of the cover.
            1 => {
                if self.once(0, 0.0) {
                    s.set_pill(Pill::Music);
                    s.set_music(true, COVER, COVER2);
                }
                s.cursor = path(t, &[(0.0, CX + 150.0, 260.0), (2.2, CX + 230.0, 250.0), (3.3, CX - 170.0, 122.0), (5.6, CX + 180.0, 112.0), (6.6, CX + 40.0, 104.0), (7.6, AWAY.0, AWAY.1)]);
            }
            // 3. Opens on a hover.
            2 => {
                if self.once(0, 1.0) {
                    s.snap_tabs();
                    s.set_tab(0);
                    s.set_mode(Mode::Expanded);
                }
                if self.once(1, 5.4) {
                    s.set_mode(Mode::Idle);
                }
                s.cursor = path(t, &[(0.0, AWAY.0, AWAY.1), (0.9, on_pill.0, on_pill.1), (2.6, CX - 150.0, 170.0), (4.2, CX + 190.0, 130.0), (5.0, CX + 310.0, 390.0)]);
            }
            // 4. The AI chat reads your files.
            3 => {
                if self.once(0, 0.7) {
                    s.set_mode(Mode::Expanded);
                }
                if self.once(1, 2.0) {
                    s.set_tab(4);
                }
                let tab = (ui::tab_center(s, 4), 28.0);
                s.cursor = path(t, &[(0.0, CX + 310.0, 390.0), (0.6, on_pill.0, on_pill.1), (1.2, on_pill.0, on_pill.1), (1.9, tab.0, tab.1), (3.2, CX - 40.0, 190.0)]);
            }
            // 5. Control centre.
            4 => {
                if self.once(0, 1.0) {
                    s.set_tab(7);
                }
                if self.once(1, 3.9) {
                    s.set_mode(Mode::Idle);
                }
                if self.once(2, 4.7) {
                    s.set_tab(0);
                    s.set_music(false, COVER, COVER2);
                }
                let tab = (ui::tab_center(s, 7), 28.0);
                s.cursor = path(t, &[(0.0, CX - 40.0, 190.0), (0.9, tab.0, tab.1), (2.4, CX + 60.0, 200.0), (3.6, AWAY.0, 395.0)]);
            }
            // 6. Downloads from any app.
            5 => {
                if self.once(0, 0.0) {
                    s.set_music(false, COVER, COVER2);
                    s.set_pill(Pill::Download);
                    s.set_line(true, theme::accent());
                    s.done = DoneView { name: "Holiday 2026.mkv".encode_utf16().collect(), ext: "MKV".encode_utf16().collect(), size: TOTAL };
                }
                const TOTAL: u64 = 1_288_490_189; // 1.2 GB
                let fraction = (t / 4.2).min(1.0);
                s.download = crate::watch::Download { name: "Holiday 2026.mkv".into(), bytes: (TOTAL as f32 * fraction) as u64, total: TOTAL, speed: 12.0 * 1_048_576.0 };
                // The demo redraws the pill as the numbers change.
                s.repaint = fraction < 1.0 || s.repaint;
                if self.once(1, 4.4) {
                    s.set_pill(Pill::Done);
                    s.set_line(true, theme::GREEN);
                }
                if self.once(2, 7.2) {
                    s.set_pill(Pill::Clock);
                    s.set_line(false, theme::GREEN);
                }
                if self.once(3, 7.3) {
                    s.release_droplet();
                }
                s.cursor = AWAY;
            }
            // 7. Auto-hide.
            6 => {
                if self.once(0, 0.7) {
                    s.set_mode(Mode::Hidden);
                }
                if self.once(1, 4.0) {
                    s.set_mode(Mode::Idle);
                }
                s.cursor = path(t, &[(0.0, AWAY.0, AWAY.1), (2.8, CX + 200.0, 220.0), (3.9, CX + 20.0, 3.0), (5.2, CX + 30.0, 150.0), (6.2, AWAY.0, AWAY.1)]);
            }
            // 8. Characters watch the cursor.
            7 => {
                if self.once(0, 0.2) {
                    s.set_pill(Pill::Mascot);
                }
                if self.once(1, 5.7) {
                    s.set_pill(Pill::Clock);
                }
                let a = t * 1.5;
                s.cursor = (CX + 210.0 * a.cos(), 230.0 + 80.0 * a.sin());
            }
            // 9. A light theme.
            _ => {
                if self.once(0, 0.3) {
                    s.theme.set(1.0);
                }
                if self.once(1, 2.0) {
                    s.set_mode(Mode::Expanded);
                }
                if self.once(2, 4.8) {
                    s.set_mode(Mode::Idle);
                }
                if self.once(3, 6.4) {
                    s.theme.set(0.0);
                }
                s.cursor = path(t, &[(0.0, AWAY.0, AWAY.1), (1.9, on_pill.0, on_pill.1), (3.4, CX - 120.0, 180.0), (4.6, CX + 300.0, 392.0)]);
            }
        }
        s.reach();
    }
}

/// Smoothly interpolates a `(time, x, y)` keyframe path.
fn path(t: f32, keys: &[(f32, f32, f32)]) -> (f32, f32) {
    let first = keys[0];
    if t <= first.0 {
        return (first.1, first.2);
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t < b.0 {
            let k = smoothstep(a.0, b.0, t);
            return (a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k);
        }
    }
    let last = keys[keys.len() - 1];
    (last.1, last.2)
}
