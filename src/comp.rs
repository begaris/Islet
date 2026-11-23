//! Compositor-driven layers: content that DWM animates by itself, so the app
//! presents no frames while it runs.
//!
//! * The steady music wave. A travelling wave cannot simply be translated,
//!   because its amplitude fades out toward the island's rounded corners. So it
//!   is split into layers: a middle strip (pure wave, looping translation) and
//!   two corner flipbooks (every phase pre-rendered with the exact live contour
//!   code, stepped at 60 Hz). The main swap chain draws the body slightly
//!   shorter underneath; the layers add the sliver of body down to the moving
//!   edge plus the glowing accent line.
//!   The strip is drawn at the largest amplitude and shown through narrow
//!   segments, each with its own vertical scale about the resting edge: the
//!   music level sets those scales (a commit, no redraw), so the middle of the
//!   wave swells with the bass while the segments next to the corners stay at
//!   the corners' fixed amplitude (`island::wave_ramp`).
//! * The equalizer bars: one solid surface per bar with a rounded clip whose
//!   height is set from the audio level (a commit, not a redraw).

use crate::anim::smoothstep;
use crate::island::{wave_profile, Liquid, Shape, N, WAVE_AMP, WAVE_LEN, WAVE_MAX, WAVE_QUIET, WAVE_RAMP};
use crate::render::{Gfx, Rgba};
use std::f32::consts::TAU;
use windows::core::{Interface, Result};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D11::{D3D11_QUERY_DESC, D3D11_QUERY_EVENT};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;

/// Pixel storage of one compositor layer.
type LayerSurface = IDXGISwapChain1;
use windows_numerics::{Matrix3x2, Vector2};

/// How much shorter the main body is drawn while the layers supply the edge.
pub const WAVE_RAISE: f32 = WAVE_MAX + 1.0;
/// Width of the strip segments next to the corners, in dp.
const SEGMENT: f32 = 4.0;
/// Extra body drawn above the strip so a squeezed segment never leaves a gap.
const STRIP_MARGIN: f32 = (WAVE_RAISE + 1.5) * (WAVE_MAX / WAVE_QUIET - 1.0) + 1.0;
/// How far below the lowest crest the line's halo can reach, in dp.
const HALO: f32 = 8.0;

/// Thickness of the music line in the middle of the edge relative to the
/// corners, for a middle amplitude: the strip's line is stretched with it.
pub fn line_scale(mid: f32) -> f32 {
    mid / WAVE_AMP
}

/// Everything the wave layers depend on; they are rebuilt when it changes.
#[derive(Clone, Copy, PartialEq)]
pub struct WaveSpec {
    pub cx: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub island: Rgba,
    pub line: Rgba,
    pub line2: Rgba,
    pub line_width: f32,
    /// Plain sine instead of the liquid pattern.
    pub simple: bool,
    /// Phase speed in radians per second.
    pub speed: f32,
    /// Halo around the line.
    pub glow: bool,
}

impl WaveSpec {
    /// Seconds for the wave to travel one pattern length.
    fn period(&self) -> f32 {
        TAU / self.speed.max(0.1)
    }

    fn halo(&self) -> f32 {
        if self.glow { 1.0 } else { 0.0 }
    }
}

/// Sizes of the wave layers, in physical pixels (see `Gfx::wave_begin`).
#[derive(Clone, Copy)]
struct Geometry {
    lx0: i32,
    lx1: i32,
    rx0: i32,
    rx1: i32,
    zone_y: i32,
    cell_h: i32,
    band_top: i32,
    band_bot: i32,
    fill_top: f32,
    surf_top: i32,
    strip_h: i32,
    lambda_px: f32,
    strip_w: i32,
    frames: usize,
    cols: usize,
    rows: usize,
}

/// Wave layers being drawn over several frames (`Gfx::wave_prepare`).
pub struct WaveBuild {
    spec: WaveSpec,
    g: Geometry,
    left: LayerSurface,
    right: LayerSurface,
    /// Flipbook cells drawn so far, left corner's first.
    next: usize,
}

pub struct WaveLayers {
    spec: WaveSpec,
    group: IDCompositionVisual2,
    /// Strip content visuals (one per segment, all sliding the same way).
    strip: Vec<IDCompositionVisual2>,
    /// Each segment's vertical scale and how far the music level applies there (0..1).
    scales: Vec<(IDCompositionScaleTransform, f32)>,
    left: IDCompositionVisual2,
    right: IDCompositionVisual2,
    /// Flipbook cell size (left and right width, shared height), grid columns
    /// and wavelength, in physical pixels.
    cell_w: (i32, i32),
    cols: usize,
    cell_h: i32,
    lambda_px: f32,
    frames: usize,
    shown: bool,
    // Surfaces and clip holders are referenced by the visuals; kept for clarity of ownership.
    _keep: Vec<IDCompositionVisual2>,
}

/// Vertical scale of a strip segment for a middle amplitude.
fn segment_scale(mid: f32, ramp: f32) -> f32 {
    (WAVE_AMP + (mid - WAVE_AMP) * ramp) / WAVE_MAX
}

impl Gfx {
    /// Creates the pixel storage for a layer: a swap chain of exactly this
    /// size. (DirectComposition's own surfaces come out of large pooled
    /// atlases, which cost tens of MB with the software rasteriser.)
    fn layer_surface(&self, width: i32, height: i32) -> Result<LayerSurface> {
        unsafe {
            let dxgi: IDXGIDevice = self.d3d.cast()?;
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1) as u32,
                Height: height.max(1) as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            factory.CreateSwapChainForComposition(&dxgi, &desc, None)
        }
    }

    /// Draws a layer's content with Direct2D and presents it once.
    fn draw_surface(&self, surface: &LayerSurface, width: i32, height: i32, draw: impl FnOnce(POINT)) -> Result<()> {
        self.paint_surface(surface, width, height, true, draw)?;
        unsafe { surface.Present(0, DXGI_PRESENT(0)).ok() }
    }

    /// Draws into a layer's back buffer without presenting it, so content can
    /// be added over several calls; `clear` empties it first.
    fn paint_surface(&self, surface: &LayerSurface, width: i32, height: i32, clear: bool, draw: impl FnOnce(POINT)) -> Result<()> {
        unsafe {
            let offset = POINT::default();
            let dxgi: IDXGISurface = surface.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let bitmap = self.ctx.CreateBitmapFromDxgiSurface(&dxgi, Some(&props))?;
            self.ctx.SetTarget(&bitmap);
            self.ctx.BeginDraw();
            self.cull_none();
            self.ctx.SetTransform(&Matrix3x2::identity());
            let area = D2D_RECT_F { left: offset.x as f32, top: offset.y as f32, right: (offset.x + width) as f32, bottom: (offset.y + height) as f32 };
            self.ctx.PushAxisAlignedClip(&area, D2D1_ANTIALIAS_MODE_ALIASED);
            if clear {
                self.ctx.Clear(None);
            }
            draw(offset);
            self.ctx.PopAxisAlignedClip();
            let drawn = self.ctx.EndDraw(None, None);
            self.ctx.SetTarget(&self.main_bitmap);
            drop(bitmap);
            drop(dxgi);
            drawn
        }
    }

    /// Submits what Direct2D has batched and waits until the device has drawn
    /// it. The software rasteriser runs behind the CPU, and every buffer it
    /// still needs is kept as a copy; drawing hundreds of flipbook cells in one
    /// go left tens of MB of such copies in the pool for good.
    fn drain(&self) {
        unsafe {
            if self.ctx.Flush(None, None).is_err() {
                return;
            }
            let Ok(context) = self.d3d.GetImmediateContext() else { return };
            let desc = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
            let mut query = None;
            if self.d3d.CreateQuery(&desc, Some(&mut query)).is_err() {
                return;
            }
            let Some(query) = query else { return };
            context.End(&query);
            context.Flush();
            let mut done = 0i32;
            for _ in 0..2000 {
                let got = context.GetData(&query, Some(&mut done as *mut i32 as *mut _), 4, 0);
                if got.is_ok() && done != 0 {
                    break;
                }
                std::thread::yield_now();
            }
        }
    }

    /// Maps dp to surface pixels: `px = dp * scale + (tx, ty)`.
    fn map(&self, tx: f32, ty: f32) {
        let s = self.scale;
        unsafe { self.ctx.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: tx, M32: ty }) };
    }

    /// Shows the compositor-animated wave for `spec`, starting at `phase`
    /// (same meaning as `Liquid::wave_phase`) with the middle amplitude `mid`.
    /// Rebuilds the layers if needed.
    pub fn wave_show(&self, spec: WaveSpec, phase: f32, mid: f32) -> Result<()> {
        // Normally `wave_prepare` has built them over earlier frames; otherwise
        // this builds them in one go.
        self.wave_prepare(spec, std::time::Duration::MAX)?;
        let mut slot = self.wave.borrow_mut();
        let Some(layers) = slot.as_mut() else { return Ok(()) };
        unsafe {
            let phase = phase.rem_euclid(TAU);
            let period = spec.period() as f64;

            // Middle strip: offset runs from `-lambda + o0` up to 0, then loops over [-lambda, 0).
            let lambda = layers.lambda_px;
            let speed = lambda / spec.period();
            let o0 = phase / TAU * lambda;
            let t1 = ((lambda - o0) / speed) as f64;
            let slide = self.dcomp.CreateAnimation()?;
            slide.AddCubic(0.0, -lambda + o0, speed, 0.0, 0.0)?;
            if t1 > 0.001 && t1 < period - 0.001 {
                slide.AddCubic(t1, -lambda, speed, 0.0, 0.0)?;
            }
            slide.AddRepeat(period, period)?;
            for content in &layers.strip {
                content.SetOffsetX(&slide)?;
            }
            for (scale, ramp) in &layers.scales {
                scale.SetScaleY2(segment_scale(mid, *ramp))?;
            }

            // Corner flipbooks: one cell per step, starting at the cell for `phase`.
            let n = layers.frames;
            let first = ((phase / TAU * n as f32) as usize).min(n - 1);
            let step = period / n as f64;
            // Cells are laid out in a grid, so both offsets step.
            for (visual, cell_w) in [(&layers.left, layers.cell_w.0), (&layers.right, layers.cell_w.1)] {
                let flip_x = self.dcomp.CreateAnimation()?;
                let flip_y = self.dcomp.CreateAnimation()?;
                for j in 0..n {
                    let cell = (first + j) % n;
                    let (col, row) = ((cell % layers.cols) as i32, (cell / layers.cols) as i32);
                    flip_x.AddCubic(j as f64 * step, -((col * cell_w) as f32), 0.0, 0.0, 0.0)?;
                    flip_y.AddCubic(j as f64 * step, -((row * layers.cell_h) as f32), 0.0, 0.0, 0.0)?;
                }
                flip_x.AddRepeat(period, period)?;
                flip_y.AddRepeat(period, period)?;
                visual.SetOffsetX(&flip_x)?;
                visual.SetOffsetY(&flip_y)?;
            }

            if !layers.shown {
                // `insertabove = false` with no reference visual means "on top of all siblings".
                self.root.AddVisual(&layers.group, false, None)?;
                layers.shown = true;
            }
            self.dcomp.Commit()
        }
    }

    /// Sets the middle amplitude of a shown wave (from the music level).
    /// Does not commit: call [`Gfx::commit`].
    pub fn wave_level(&self, mid: f32) {
        if let Some(layers) = self.wave.borrow().as_ref() {
            if layers.shown {
                for (scale, ramp) in &layers.scales {
                    unsafe {
                        let _ = scale.SetScaleY2(segment_scale(mid, *ramp));
                    }
                }
            }
        }
    }

    /// `true` while the compositor layers show the wave.
    pub fn wave_shown(&self) -> bool {
        self.wave.borrow().as_ref().is_some_and(|l| l.shown)
    }

    /// Hides the wave layers (they are kept for the next time).
    pub fn wave_hide(&self) {
        if let Some(layers) = self.wave.borrow_mut().as_mut() {
            if layers.shown {
                layers.shown = false;
                unsafe {
                    let _ = self.root.RemoveVisual(&layers.group);
                    let _ = self.dcomp.Commit();
                }
            }
        }
    }

    /// Builds the layers for `spec` a step at a time, spending about `budget`
    /// per call, so the UI thread never stalls on it (drawing everything at
    /// once took ~1.7 s with WARP, and the spec changes with every song's
    /// colours). Returns `Ok(true)` once layers for `spec` exist; until then
    /// the caller keeps drawing the live wave and calls again next frame.
    pub fn wave_prepare(&self, spec: WaveSpec, budget: std::time::Duration) -> Result<bool> {
        if self.wave.borrow().as_ref().is_some_and(|l| l.spec == spec) {
            return Ok(true);
        }
        let started = std::time::Instant::now();
        let mut pending = self.wave_pending.borrow_mut();
        if pending.as_ref().is_some_and(|b| b.spec != spec) {
            *pending = None;
        }
        if pending.is_none() {
            *pending = Some(self.wave_begin(spec)?);
        }
        let Some(build) = pending.as_mut() else { return Ok(false) };
        let total = build.g.frames * 2;
        while build.next < total {
            // A few cells per Direct2D batch, drained like the one-shot build was.
            let side = build.next / build.g.frames;
            let first = build.next % build.g.frames;
            let last = (first + 4).min(build.g.frames);
            let (surface, x0, x1) = if side == 0 { (&build.left, build.g.lx0, build.g.lx1) } else { (&build.right, build.g.rx0, build.g.rx1) };
            self.wave_cells(&build.spec, &build.g, surface, x0, x1, first..last)?;
            build.next += last - first;
            if started.elapsed() >= budget {
                return Ok(false);
            }
        }
        let Some(build) = pending.take() else { return Ok(false) };
        drop(pending);
        let layers = self.wave_finish(build)?;
        let mut slot = self.wave.borrow_mut();
        if let Some(old) = slot.take() {
            if old.shown {
                unsafe { self.root.RemoveVisual(&old.group)? };
            }
        }
        *slot = Some(layers);
        Ok(true)
    }

    /// Drops a build that is no longer wanted (its spec is gone).
    pub fn wave_forget_pending(&self) {
        *self.wave_pending.borrow_mut() = None;
    }

    /// Sizes everything and creates the two corner atlases (nothing drawn yet).
    fn wave_begin(&self, spec: WaveSpec) -> Result<WaveBuild> {
        let s = self.scale;
        let shape = Shape::new(spec.cx, spec.w, spec.h, spec.r, Liquid::default());
        let (left, right) = (shape.left(), shape.right());

        // Zone boundaries in physical pixels (whole pixels, so layers meet exactly).
        let (left_seam, right_seam) = shape.wave_seams();
        let lx0 = ((left - 6.0) * s).floor() as i32;
        let lx1 = (left_seam * s).round() as i32;
        let rx0 = (right_seam * s).round() as i32;
        let rx1 = ((right + 6.0) * s).ceil() as i32;
        let zone_y = ((spec.h - shape.r - 4.0) * s).floor() as i32;
        let cell_h = ((shape.r + WAVE_AMP + 12.0) * s).ceil() as i32;
        // The strip band: from the top of the body sliver to below the halo
        // of the deepest crest. Its surface also holds body above the band.
        let band_top = ((spec.h - WAVE_RAISE - 1.5) * s).floor() as i32;
        let band_bot = ((spec.h + WAVE_MAX + HALO) * s).ceil() as i32;
        let fill_top = spec.h - WAVE_RAISE - 1.5 - STRIP_MARGIN;
        let surf_top = (fill_top * s).floor() as i32 - 1;
        let strip_h = band_bot - surf_top;
        let lambda_px = WAVE_LEN * s;
        let strip_w = (rx0 - lx1).max(1) + lambda_px.ceil() as i32 + 2;
        let frames = (spec.period() * 60.0).round() as usize;
        // Flipbook cells in a roughly square grid: a tall, narrow surface makes
        // the compositor allocate far more memory than it uses.
        let cols = ((frames as f32 * cell_h as f32 / (lx1 - lx0).max(1) as f32).sqrt().round() as usize).clamp(1, frames);
        let rows = frames.div_ceil(cols);
        let g = Geometry { lx0, lx1, rx0, rx1, zone_y, cell_h, band_top, band_bot, fill_top, surf_top, strip_h, lambda_px, strip_w, frames, cols, rows };
        let left = self.layer_surface((lx1 - lx0) * cols as i32, cell_h * rows as i32)?;
        let right = self.layer_surface((rx1 - rx0) * cols as i32, cell_h * rows as i32)?;
        Ok(WaveBuild { spec, g, left, right, next: 0 })
    }

    /// Draws flipbook cells `cells` of one corner atlas into its back buffer
    /// (presented once, when the build finishes). The first batch clears it.
    fn wave_cells(&self, spec: &WaveSpec, g: &Geometry, surface: &LayerSurface, x0: i32, x1: i32, cells: std::ops::Range<usize>) -> Result<()> {
        let width = x1 - x0;
        let (atlas_w, atlas_h) = (width * g.cols as i32, g.cell_h * g.rows as i32);
        let clear = cells.start == 0;
        let mut pts = [Vector2::default(); N];
        let mut failed = None;
        unsafe {
            self.paint_surface(surface, atlas_w, atlas_h, clear, |off| {
                for i in cells {
                    let phase = TAU * (i as f32 + 0.5) / g.frames as f32;
                    let cell_left = off.x + (i % g.cols) as i32 * width;
                    let cell_top = off.y + (i / g.cols) as i32 * g.cell_h;
                    self.ctx.SetTransform(&Matrix3x2::identity());
                    let cell = D2D_RECT_F { left: cell_left as f32, top: cell_top as f32, right: (cell_left + width) as f32, bottom: (cell_top + g.cell_h) as f32 };
                    self.ctx.PushAxisAlignedClip(&cell, D2D1_ANTIALIAS_MODE_ALIASED);
                    self.map((cell_left - x0) as f32, (cell_top - g.zone_y) as f32);
                    let live = Shape::new(spec.cx, spec.w, spec.h, spec.r, Liquid { wave_amp: WAVE_AMP, wave_mid: WAVE_AMP, wave_simple: spec.simple, wave_phase: phase, ..Default::default() });
                    // Everything above this contour is drawn by the main swap chain.
                    let body = Shape::new(spec.cx, spec.w, spec.h, spec.r, Liquid { raise: WAVE_RAISE + 1.5, ..Default::default() });
                    live.contour(&mut pts);
                    let live_paths = self.island_paths(&live, &pts);
                    body.contour(&mut pts);
                    let body_paths = self.island_paths(&body, &pts);
                    match (live_paths, body_paths) {
                        (Ok(live_paths), Ok(body_paths)) => {
                            self.fill_path(&live_paths.fill, spec.island);
                            // Punch the main body's area back out so panel content shows through.
                            self.ctx.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
                            self.ctx.FillGeometry(&body_paths.fill, self.brush(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }), None);
                            self.ctx.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
                            self.stroke_glow(&live_paths.edge, spec.line_width, spec.line2, spec.line, spec.h - WAVE_AMP, spec.h + WAVE_AMP, spec.halo());
                        }
                        (Err(e), _) | (_, Err(e)) => failed = Some(e),
                    }
                    self.ctx.PopAxisAlignedClip();
                }
                // Direct2D batches everything until EndDraw; with the software
                // rasteriser hundreds of cells of wide strokes grow that batch by
                // tens of MB that are never given back.
                self.drain();
            })?;
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Presents the corner atlases, draws the strip and builds the visual tree.
    fn wave_finish(&self, build: WaveBuild) -> Result<WaveLayers> {
        let WaveBuild { spec, g, left: left_surface, right: right_surface, .. } = build;
        let Geometry { lx0, lx1, rx0, rx1, zone_y, cell_h, band_top, band_bot, fill_top, surf_top, strip_h, lambda_px, strip_w, frames, cols, .. } = g;
        let s = self.scale;
        unsafe {
            left_surface.Present(0, DXGI_PRESENT(0)).ok()?;
            right_surface.Present(0, DXGI_PRESENT(0)).ok()?;

            // --- middle strip: pure wave at full amplitude, one pattern wider than its window ----
            let strip_surface = self.layer_surface(strip_w, strip_h)?;
            let mut failed = None;
            self.draw_surface(&strip_surface, strip_w, strip_h, |off| {
                // Strip pixel u shows the wave at dp x = (lx1 - lambda_px + u) / scale, phase 0.
                self.map(off.x as f32 - (lx1 as f32 - lambda_px), (off.y - surf_top) as f32);
                let x_start = (lx1 as f32 - lambda_px) / s - 2.0;
                let x_end = x_start + strip_w as f32 / s + 4.0;
                let top = fill_top;
                let k = TAU / WAVE_LEN;
                let y = |x: f32| spec.h + WAVE_MAX * wave_profile(x * k, spec.simple);
                let build = |closed: bool| -> Result<ID2D1PathGeometry1> {
                    let geometry = self.factory.CreatePathGeometry()?;
                    let sink = geometry.Open()?;
                    let begin = if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
                    let mut x = x_start;
                    sink.BeginFigure(Vector2 { X: x, Y: y(x) }, begin);
                    while x < x_end {
                        x += 1.0;
                        sink.AddLine(Vector2 { X: x, Y: y(x) });
                    }
                    if closed {
                        sink.AddLine(Vector2 { X: x, Y: top });
                        sink.AddLine(Vector2 { X: x_start, Y: top });
                    }
                    sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
                    sink.Close()?;
                    Ok(geometry)
                };
                match (build(true), build(false)) {
                    (Ok(fill), Ok(edge)) => {
                        self.fill_path(&fill, spec.island);
                        // Drawn thicker by the ratio the segments squeeze it back by
                        // next to the corners, so the line meets theirs exactly.
                        let width = spec.line_width * WAVE_MAX / WAVE_AMP;
                        self.stroke_glow(&edge, width, spec.line2, spec.line, spec.h - WAVE_MAX, spec.h + WAVE_MAX, spec.halo());
                    }
                    (Err(e), _) | (_, Err(e)) => failed = Some(e),
                }
            })?;
            if let Some(e) = failed {
                return Err(e);
            }

            // --- visual tree ------------------------------------------------------
            let group = self.dcomp.CreateVisual()?;
            let mut keep = Vec::new();
            let window = |x: i32, y: i32, w: i32, h: i32, surface: &LayerSurface, keep: &mut Vec<IDCompositionVisual2>| -> Result<IDCompositionVisual2> {
                let frame = self.dcomp.CreateVisual()?;
                frame.SetOffsetX2(x as f32)?;
                frame.SetOffsetY2(y as f32)?;
                frame.SetClip2(&D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 })?;
                let content = self.dcomp.CreateVisual()?;
                content.SetContent(surface)?;
                frame.AddVisual(&content, false, None)?;
                group.AddVisual(&frame, false, None)?;
                keep.push(frame);
                Ok(content)
            };
            let left_v = window(lx0, zone_y, lx1 - lx0, cell_h, &left_surface, &mut keep)?;
            let right_v = window(rx0, zone_y, rx1 - rx0, cell_h, &right_surface, &mut keep)?;

            // Strip segments: narrow ones over the ramp next to each corner, one
            // wide one in the middle. frame (clip) -> scaler (vertical scale
            // about the resting edge) -> shifter (places the surface) -> content
            // (slides).
            let seg = ((SEGMENT * s).round() as i32).max(2);
            let ramp_px = (WAVE_RAMP * s).ceil() as i32;
            let mid_x = (lx1 + rx0) / 2;
            let mut xs = vec![lx1];
            while *xs.last().unwrap_or(&lx1) + seg < (lx1 + ramp_px).min(mid_x) {
                let next = xs.last().unwrap_or(&lx1) + seg;
                xs.push(next);
            }
            let mut right_xs = vec![rx0];
            while *right_xs.last().unwrap_or(&rx0) - seg > (rx0 - ramp_px).max(mid_x) {
                let next = right_xs.last().unwrap_or(&rx0) - seg;
                right_xs.push(next);
            }
            for x in right_xs.into_iter().rev() {
                if x > *xs.last().unwrap_or(&lx1) {
                    xs.push(x);
                }
            }
            let (seam_l, seam_r) = (lx1 as f32 / s, rx0 as f32 / s);
            let centre_y = spec.h * s - band_top as f32;
            let mut strip = Vec::new();
            let mut scales = Vec::new();
            for pair in xs.windows(2) {
                let (xa, xb) = (pair[0], pair[1]);
                let frame = self.dcomp.CreateVisual()?;
                frame.SetOffsetX2(xa as f32)?;
                frame.SetOffsetY2(band_top as f32)?;
                frame.SetClip2(&D2D_RECT_F { left: 0.0, top: 0.0, right: (xb - xa) as f32, bottom: (band_bot - band_top) as f32 })?;
                let scaler = self.dcomp.CreateVisual()?;
                let scale = self.dcomp.CreateScaleTransform()?;
                scale.SetScaleX2(1.0)?;
                scale.SetCenterY2(centre_y)?;
                let xc = (xa + xb) as f32 * 0.5 / s;
                let d = (xc - seam_l).min(seam_r - xc);
                let ramp = smoothstep(0.0, WAVE_RAMP, d);
                scale.SetScaleY2(segment_scale(WAVE_AMP, ramp))?;
                scaler.SetTransform(&scale)?;
                let shifter = self.dcomp.CreateVisual()?;
                shifter.SetOffsetX2((lx1 - xa) as f32)?;
                shifter.SetOffsetY2((surf_top - band_top) as f32)?;
                let content = self.dcomp.CreateVisual()?;
                content.SetContent(&strip_surface)?;
                shifter.AddVisual(&content, false, None)?;
                scaler.AddVisual(&shifter, false, None)?;
                frame.AddVisual(&scaler, false, None)?;
                group.AddVisual(&frame, false, None)?;
                keep.extend([frame, scaler, shifter]);
                strip.push(content);
                scales.push((scale, ramp));
            }

            Ok(WaveLayers { spec, group, strip, scales, left: left_v, right: right_v, cell_w: (lx1 - lx0, rx1 - rx0), cols, cell_h, lambda_px, frames, shown: false, _keep: keep })
        }
    }
}

// ----------------------------------------------------------------- equalizer --

pub const EQ_BARS: usize = 4;
const EQ_BAR_W: f32 = 3.6;
const EQ_GAP: f32 = 7.0;
const EQ_MAX_H: f32 = 24.0;

pub struct EqLayers {
    group: IDCompositionVisual2,
    bars: [(IDCompositionVisual2, IDCompositionRectangleClip); EQ_BARS],
    surface: LayerSurface,
    colour: Rgba,
    shown: bool,
    size: (i32, i32),
}

impl Gfx {
    /// Positions the equalizer at `(cx, cy)` dp with the given bar levels
    /// (0..1), colour and opacity. Does not commit: call [`Gfx::commit`].
    pub fn eq_update(&self, cx: f32, cy: f32, levels: [f32; EQ_BARS], colour: Rgba, opacity: f32) -> Result<()> {
        let s = self.scale;
        let mut slot = self.eq.borrow_mut();
        if opacity <= 0.01 {
            if let Some(eq) = slot.as_mut() {
                if eq.shown {
                    eq.shown = false;
                    unsafe { self.root.RemoveVisual(&eq.group)? };
                }
            }
            return Ok(());
        }
        unsafe {
            if slot.is_none() {
                let size = ((EQ_BAR_W * s).ceil() as i32, (EQ_MAX_H * s).ceil() as i32);
                let surface = self.layer_surface(size.0, size.1)?;
                let group = self.dcomp.CreateVisual()?;
                let bar = || -> Result<(IDCompositionVisual2, IDCompositionRectangleClip)> {
                    let visual = self.dcomp.CreateVisual()?;
                    visual.SetContent(&surface)?;
                    let clip = self.dcomp.CreateRectangleClip()?;
                    clip.SetLeft2(0.0)?;
                    clip.SetRight2(size.0 as f32)?;
                    let radius = size.0 as f32 * 0.5;
                    clip.SetTopLeftRadiusX2(radius)?;
                    clip.SetTopLeftRadiusY2(radius)?;
                    clip.SetTopRightRadiusX2(radius)?;
                    clip.SetTopRightRadiusY2(radius)?;
                    clip.SetBottomLeftRadiusX2(radius)?;
                    clip.SetBottomLeftRadiusY2(radius)?;
                    clip.SetBottomRightRadiusX2(radius)?;
                    clip.SetBottomRightRadiusY2(radius)?;
                    visual.SetClip(&clip)?;
                    group.AddVisual(&visual, false, None)?;
                    Ok((visual, clip))
                };
                let bars = [bar()?, bar()?, bar()?, bar()?];
                *slot = Some(EqLayers { group, bars, surface, colour: Rgba { r: -1.0, g: 0.0, b: 0.0, a: 0.0 }, shown: false, size });
            }
            let Some(eq) = slot.as_mut() else { return Ok(()) };
            if eq.colour != colour {
                eq.colour = colour;
                let (w, h) = eq.size;
                self.draw_surface(&eq.surface, w, h, |off| {
                    let area = D2D_RECT_F { left: off.x as f32, top: off.y as f32, right: (off.x + w) as f32, bottom: (off.y + h) as f32 };
                    self.ctx.FillRectangle(&area, self.brush(colour));
                })?;
            }
            let (w, h) = (eq.size.0 as f32, eq.size.1 as f32);
            for (i, (visual, clip)) in eq.bars.iter().enumerate() {
                let x = ((cx + (i as f32 - 1.5) * EQ_GAP) * s - w * 0.5).round();
                visual.SetOffsetX2(x)?;
                visual.SetOffsetY2((cy * s - h * 0.5).round())?;
                // Never shorter than a dot, so the caps stay round.
                let half = ((0.16 + 0.84 * levels[i].clamp(0.0, 1.0)) * h * 0.5).max(w * 0.5);
                clip.SetTop2(h * 0.5 - half)?;
                clip.SetBottom2(h * 0.5 + half)?;
            }
            if let Ok(v3) = eq.group.cast::<IDCompositionVisual3>() {
                v3.SetOpacity2(opacity.clamp(0.0, 1.0))?;
            }
            if !eq.shown {
                self.root.AddVisual(&eq.group, false, None)?;
                eq.shown = true;
            }
        }
        Ok(())
    }

    /// Sends pending layer changes to the compositor.
    pub fn commit(&self) {
        unsafe {
            let _ = self.dcomp.Commit();
        }
    }
}
