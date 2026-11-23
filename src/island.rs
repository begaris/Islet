//! Island geometry: a black shape hanging from the top screen edge with
//! concave shoulders, rounded bottom corners and a liquid bottom contour.
//!
//! The bottom contour is `N` sampled points: the base shape plus the cursor
//! bump, the wave, the idle undulation and the droplet bridge. It is rebuilt
//! each animated frame into a fixed-size buffer (no allocation).

use crate::anim::{gauss, smoothstep};
use windows_numerics::Vector2;

const CORNER: usize = 12;
// Dense enough (about 3 dp apart on the widest panel) that the smoothed
// contour reproduces the travelling wave exactly; the compositor-driven wave
// layers must be indistinguishable from the live contour.
const FLAT: usize = 192;
/// Number of contour samples along the bottom edge.
pub const N: usize = CORNER * 2 + FLAT;

/// Everything that deforms the bottom edge. All values in dp.
#[derive(Clone, Copy, Default)]
pub struct Liquid {
    /// Cursor-reach teardrop.
    pub bump_amp: f32,
    pub bump_x: f32,
    /// Music wave: amplitude next to the corners (`wave_amp`) and in the
    /// middle (`wave_mid`, follows the music), extra amplitude under the
    /// cursor, phase.
    pub wave_amp: f32,
    pub wave_mid: f32,
    /// Plain sine instead of the liquid pattern.
    pub wave_simple: bool,
    pub wave_focus: f32,
    pub wave_focus_x: f32,
    pub wave_phase: f32,
    /// Idle undulation; the phase only advances while something animates.
    pub noise_amp: f32,
    pub noise_phase: f32,
    /// Droplet bridge (see `anim::droplet_bridge`).
    pub drop_amp: f32,
    pub drop_sigma: f32,
    pub drop_x: f32,
    /// Lifts the whole bottom edge by this much (used while compositor layers
    /// draw the wave on top of a slightly shorter body).
    pub raise: f32,
    /// Distance from the left / right side over which the edge stays still:
    /// where a joint with the screen edge takes over the corner, so its line
    /// meets the island's exactly (0 = only the usual corner fade).
    pub guard: [f32; 2],
}

const BUMP_SIGMA: f32 = 30.0;
/// Length after which the music wave's pattern repeats, in dp. The "liquid"
/// pattern is two overlapping waves (48 and 32 dp) plus a slow swell (96 dp);
/// the "simple" one a plain 48 dp sine.
pub const WAVE_LEN: f32 = 96.0;
/// Phase speed of the music wave in radians per second, normal setting
/// (about 26 dp/s); see `wave_speed`.
pub const WAVE_SPEED: f32 = 1.7;
/// Amplitude of the music wave next to the corners (and everywhere while no
/// audio levels arrive), in dp.
pub const WAVE_AMP: f32 = 3.0;
/// Smallest and largest middle amplitude any setting can reach (the
/// compositor layers are sized for these).
pub const WAVE_QUIET: f32 = 1.6;
pub const WAVE_MAX: f32 = 6.0;

/// How strongly the middle follows the music: 0 off, 1 soft, 2 normal, 3 strong.
/// Returns the middle amplitude for silence and for full bass.
pub fn wave_range(react: u8) -> (f32, f32) {
    match react {
        0 => (WAVE_AMP, WAVE_AMP),
        1 => (2.5, 3.8),
        2 => (2.0, 4.6),
        _ => (WAVE_QUIET, WAVE_MAX),
    }
}

/// Phase speed for the speed setting: 0 slow, 1 normal, 2 fast.
pub fn wave_speed(speed: u8) -> f32 {
    match speed {
        0 => 1.1,
        2 => 2.6,
        _ => WAVE_SPEED,
    }
}
/// The middle amplitude grows from `wave_amp` over this distance from the
/// point where the compositor's corner layers end (see `wave_seams`).
pub const WAVE_RAMP: f32 = 34.0;
/// The corner layers reach this far past the corner fade zone.
pub const WAVE_SEAM_PAD: f32 = 2.0;
const WAVE_FOCUS_SIGMA: f32 = 58.0;

/// Music level (0..1) at which the middle amplitude equals `WAVE_AMP`.
pub fn wave_neutral(react: u8) -> f32 {
    let (quiet, loud) = wave_range(react);
    if loud - quiet < 0.01 {
        0.0
    } else {
        (WAVE_AMP - quiet) / (loud - quiet)
    }
}

/// Middle amplitude for a music level (0..1).
pub fn wave_mid(level: f32, react: u8) -> f32 {
    let (quiet, loud) = wave_range(react);
    quiet + (loud - quiet) * level.clamp(0.0, 1.0)
}

/// The wave's profile over one pattern (`theta` in radians), in -1..1.
pub fn wave_profile(theta: f32, simple: bool) -> f32 {
    if simple {
        (2.0 * theta).sin()
    } else {
        raw_profile(theta) * profile_norm()
    }
}

fn raw_profile(t: f32) -> f32 {
    0.62 * (2.0 * t).sin() + 0.38 * (3.0 * t + 1.3).sin() + 0.24 * (t + 0.6).sin()
}

fn profile_norm() -> f32 {
    static NORM: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *NORM.get_or_init(|| {
        let peak = (0..2048).map(|i| raw_profile(i as f32 / 2048.0 * std::f32::consts::TAU).abs()).fold(0.0f32, f32::max);
        1.0 / peak.max(0.01)
    })
}

/// How far the middle amplitude has taken over at `x` (0 at the seams, 1 in the middle).
pub fn wave_ramp(left_seam: f32, right_seam: f32, x: f32) -> f32 {
    smoothstep(0.0, WAVE_RAMP, (x - left_seam).min(right_seam - x))
}

#[derive(Clone, Copy)]
pub struct Shape {
    /// Horizontal centre.
    pub cx: f32,
    pub w: f32,
    pub h: f32,
    /// Bottom corner radius.
    pub r: f32,
    /// Shoulder (top flare) radius.
    pub sr: f32,
    pub liquid: Liquid,
}

impl Shape {
    pub fn new(cx: f32, w: f32, h: f32, r: f32, liquid: Liquid) -> Self {
        let h = h.max(1.0);
        let w = w.max(8.0);
        let sr = 13.0f32.min(h * 0.5);
        let r = r.min(h - sr).min(w * 0.5).max(0.5);
        Self { cx, w, h, r, sr, liquid }
    }

    #[inline]
    pub fn left(&self) -> f32 {
        self.cx - self.w * 0.5
    }

    #[inline]
    pub fn right(&self) -> f32 {
        self.cx + self.w * 0.5
    }

    /// Width of the zone next to each side over which the liquid displacement
    /// fades in; beyond it the displacement applies in full.
    #[inline]
    pub fn fade(&self) -> f32 {
        self.r * 1.25
    }

    /// Where the compositor's corner layers hand over to the middle strip (dp).
    #[inline]
    pub fn wave_seams(&self) -> (f32, f32) {
        (self.left() + self.fade() + WAVE_SEAM_PAD, self.right() - self.fade() - WAVE_SEAM_PAD)
    }

    /// Liquid displacement (positive = downward) at horizontal position `x`.
    pub fn offset(&self, x: f32) -> f32 {
        let l = &self.liquid;
        let (left, right) = (self.left(), self.right());
        // Fade the displacement out over the corners so the sides stay attached.
        let fade = self.fade();
        let (from, to) = (left + l.guard[0], right - l.guard[1]);
        let window = smoothstep(from, from + fade, x) * smoothstep(to, to - fade, x);
        if window <= 0.0 {
            return 0.0;
        }
        let mut y = -l.raise;
        if l.bump_amp.abs() > 0.01 {
            y += l.bump_amp * gauss(x - l.bump_x, BUMP_SIGMA);
        }
        if l.wave_amp > 0.01 || l.wave_mid > 0.01 {
            let s = wave_profile(x * (std::f32::consts::TAU / WAVE_LEN) - l.wave_phase, l.wave_simple);
            let (ls, rs) = self.wave_seams();
            let amp = l.wave_amp + (l.wave_mid - l.wave_amp) * wave_ramp(ls, rs, x);
            y += amp * s;
            if l.wave_focus > 0.01 {
                // Under the cursor the wave turns into sharp downward peaks.
                let up = 0.5 + 0.5 * s;
                y += l.wave_focus * gauss(x - l.wave_focus_x, WAVE_FOCUS_SIGMA) * up * up;
            }
        }
        if l.noise_amp > 0.01 {
            y += l.noise_amp * ((x * 0.043 + l.noise_phase).sin() + 0.5 * (x * 0.107 - 1.7 * l.noise_phase).sin());
        }
        if l.drop_amp > 0.01 {
            y += l.drop_amp * gauss(x - l.drop_x, l.drop_sigma);
        }
        y * window
    }

    /// Y of the undeformed bottom profile (rounded corners) at `x`.
    fn base_y(&self, x: f32) -> f32 {
        let d = (x - self.left()).min(self.right() - x);
        if d >= self.r {
            self.h
        } else if d <= 0.0 {
            self.h - self.r
        } else {
            let k = self.r - d;
            self.h - self.r + (self.r * self.r - k * k).sqrt()
        }
    }

    /// Y of the liquid bottom edge at `x`.
    pub fn edge_y(&self, x: f32) -> f32 {
        self.base_y(x) + self.offset(x)
    }

    /// Hit test in dp. `slop` grows the shape a little on every side.
    pub fn contains(&self, x: f32, y: f32, slop: f32) -> bool {
        x >= self.left() - slop && x <= self.right() + slop && y >= -1.0 && y <= self.edge_y(x.clamp(self.left(), self.right())) + slop
    }

    /// Fills `out` with the bottom contour from the left side to the right side.
    pub fn contour(&self, out: &mut [Vector2; N]) {
        let (left, right, r) = (self.left(), self.right(), self.r);
        let top = self.h - r;
        let mut i = 0;
        for k in 0..CORNER {
            let a = k as f32 / CORNER as f32 * std::f32::consts::FRAC_PI_2;
            let x = left + r * (1.0 - a.cos());
            out[i] = Vector2 { X: x, Y: top + r * a.sin() + self.offset(x) };
            i += 1;
        }
        let (x0, x1) = (left + r, right - r);
        for k in 0..FLAT {
            let x = x0 + (x1 - x0) * (k as f32 / (FLAT - 1) as f32);
            out[i] = Vector2 { X: x, Y: self.h + self.offset(x) };
            i += 1;
        }
        for k in (0..CORNER).rev() {
            let a = k as f32 / CORNER as f32 * std::f32::consts::FRAC_PI_2;
            let x = right - r * (1.0 - a.cos());
            out[i] = Vector2 { X: x, Y: top + r * a.sin() + self.offset(x) };
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn wave_profile_is_bounded_and_repeats() {
        let samples: Vec<f32> = (0..4096).map(|i| wave_profile(i as f32 / 4096.0 * TAU, false)).collect();
        let peak = samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 1.0001 && peak > 0.99, "peak {peak}");
        for simple in [false, true] {
            assert!((wave_profile(0.3, simple) - wave_profile(0.3 + TAU, simple)).abs() < 1e-4);
        }
        // Not a plain sine: crests of different heights within one pattern.
        let crests: Vec<f32> = (1..4095).filter(|&i| samples[i] > samples[i - 1] && samples[i] >= samples[i + 1]).map(|i| samples[i]).collect();
        assert!(crests.len() >= 2 && crests.iter().any(|c| (c - crests[0]).abs() > 0.05), "crests {crests:?}");
    }

    #[test]
    fn middle_swells_but_corners_keep_their_amplitude() {
        let shape = |mid: f32| Shape::new(360.0, 344.0, 68.0, 22.0, Liquid { wave_amp: WAVE_AMP, wave_mid: mid, ..Default::default() });
        let (quiet, loud) = (shape(wave_mid(0.0, 3)), shape(wave_mid(1.0, 3)));
        let (seam, _) = quiet.wave_seams();
        // At the seam both agree (the compositor's corner layers never change).
        assert!((quiet.offset(seam) - loud.offset(seam)).abs() < 1e-3);
        // In the middle the loud wave reaches further.
        let reach = |s: &Shape| (0..400).map(|i| s.offset(260.0 + i as f32 * 0.5).abs()).fold(0.0f32, f32::max);
        assert!(reach(&loud) > WAVE_MAX * 0.95 && reach(&quiet) < WAVE_QUIET * 1.01);
        for react in 0..4 {
            let (q, l) = wave_range(react);
            assert!(q >= WAVE_QUIET && l <= WAVE_MAX);
            assert!((wave_mid(wave_neutral(react), react) - WAVE_AMP).abs() < 1e-4, "react {react}");
        }
    }
}
