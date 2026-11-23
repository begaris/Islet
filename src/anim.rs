//! Shared animation primitives: closed-form springs, a Gaussian bump, a
//! droplet/metaball bridge helper and a few easing functions.
//!
//! Every animated value in the app is a [`Spring`], so retargeting one midway
//! keeps its velocity and looks natural. `settled()` lets the render loop sleep.

/// A unit-mass damped spring solved analytically (stable for any `dt`).
#[derive(Clone, Copy)]
pub struct Spring {
    pub x: f32,
    pub v: f32,
    pub target: f32,
    omega: f32,
    zeta: f32,
    eps: f32,
}

impl Spring {
    /// `stiffness` and `damping` in the usual k / c form (mass = 1).
    pub fn new(x: f32, stiffness: f32, damping: f32) -> Self {
        let omega = stiffness.sqrt();
        Self { x, v: 0.0, target: x, omega, zeta: damping / (2.0 * omega), eps: 0.02 }
    }

    /// The default UI spring: stiffness 300, damping 26 (about 0.35 s settle).
    pub fn ui(x: f32) -> Self {
        Self::new(x, 300.0, 26.0)
    }

    /// A loose, visibly under-damped spring for liquid motion.
    pub fn liquid(x: f32) -> Self {
        Self::new(x, 170.0, 13.0)
    }

    /// A spring for 0..1 values (opacity, mix factors) with a finer rest threshold.
    pub fn unit(x: f32) -> Self {
        let mut s = Self::new(x, 260.0, 30.0);
        s.eps = 0.002;
        s
    }

    /// New stiffness and damping, keeping position, velocity and target.
    pub fn tune(&mut self, stiffness: f32, damping: f32) {
        self.omega = stiffness.sqrt();
        self.zeta = damping / (2.0 * self.omega);
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn snap(&mut self, x: f32) {
        self.x = x;
        self.target = x;
        self.v = 0.0;
    }

    pub fn settled(&self) -> bool {
        (self.x - self.target).abs() < self.eps && self.v.abs() < self.eps * 4.0
    }

    /// Advances the spring by `dt` seconds. Returns `true` while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if self.settled() {
            self.x = self.target;
            self.v = 0.0;
            return false;
        }
        let x0 = self.x - self.target;
        let v0 = self.v;
        let w = self.omega;
        if self.zeta < 0.999 {
            // Under-damped.
            let wd = w * (1.0 - self.zeta * self.zeta).sqrt();
            let decay = (-self.zeta * w * dt).exp();
            let (sin, cos) = (wd * dt).sin_cos();
            let b = (v0 + self.zeta * w * x0) / wd;
            let x = decay * (x0 * cos + b * sin);
            let v = decay * ((b * wd - self.zeta * w * x0) * cos - (x0 * wd + self.zeta * w * b) * sin);
            self.x = self.target + x;
            self.v = v;
        } else {
            // Critically damped (over-damped values are treated as critical).
            let decay = (-w * dt).exp();
            let b = v0 + w * x0;
            self.x = self.target + (x0 + b * dt) * decay;
            self.v = (b - w * (x0 + b * dt)) * decay;
        }
        true
    }
}

#[inline]
pub fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp01((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}

#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - clamp01(t);
    1.0 - u * u * u
}

/// Unit-height Gaussian bump centred on zero.
#[inline]
pub fn gauss(x: f32, sigma: f32) -> f32 {
    (-(x * x) / (2.0 * sigma * sigma)).exp()
}

/// Metaball-style bridge between a flat edge and a droplet of radius `r` whose
/// centre is `dist` below the edge. Returns `(amplitude, sigma)` of the bump the
/// edge should grow toward the droplet: while the droplet is close the edge
/// swallows it, then the neck thins and snaps back.
pub fn droplet_bridge(dist: f32, r: f32) -> (f32, f32) {
    if dist <= 0.0 {
        return (0.0, r);
    }
    let near = 1.6 * r;
    let far = 2.7 * r;
    if dist < near {
        (dist + r * 0.9, r * 0.95)
    } else {
        let t = smoothstep(near, far, dist);
        ((near + r * 0.9) * (1.0 - t), r * lerp(0.95, 0.5, t))
    }
}
