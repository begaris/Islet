//! Direct3D 11 + Direct2D + DirectComposition renderer.
//!
//! One premultiplied-alpha composition swap chain is bound to the window through
//! a DirectComposition visual. All drawing is done in dp: the device context
//! carries a scale transform, so callers never deal with physical pixels.
//! Only damaged rectangles are redrawn and presented. Dropping [`Gfx`] releases
//! every graphics resource (used by game-suspend mode).
//!
//! The Direct3D device is either the in-process software rasteriser (WARP, the
//! default: a few MB of memory) or the hardware driver (least CPU per frame,
//! but the vendor driver adds tens of MB to the process). Measured alternative
//! that was rejected: Direct2D's CPU target + `UpdateLayeredWindow` cost more
//! CPU than WARP for the same frames.

use crate::island::{Shape, N};
use std::cell::{Cell, RefCell};
use windows::core::{w, Interface, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_numerics::{Matrix3x2, Vector2};

/// Straight-alpha colour; converted to what Direct2D expects at draw time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn hex(v: u32) -> Self {
        Self { r: ((v >> 16) & 255) as f32 / 255.0, g: ((v >> 8) & 255) as f32 / 255.0, b: (v & 255) as f32 / 255.0, a: 1.0 }
    }

    pub fn alpha(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }

    pub fn mix(self, o: Self, t: f32) -> Self {
        Self { r: self.r + (o.r - self.r) * t, g: self.g + (o.g - self.g) * t, b: self.b + (o.b - self.b) * t, a: self.a + (o.a - self.a) * t }
    }
}

/// Axis-aligned rectangle in dp, used for damage tracking and culling.
#[derive(Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub const EMPTY: Rect = Rect { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 };

    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn union(self, o: Rect) -> Rect {
        if self.is_empty() {
            o
        } else if o.is_empty() {
            self
        } else {
            Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Font {
    Clock,
    Big,
    Title,
    Label,
    Body,
    Small,
    Hud,
}
const FONT_COUNT: usize = 7;

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Fixed-capacity UTF-16 buffer for formatting dynamic text without the heap.
pub struct WBuf {
    buf: [u16; 160],
    len: usize,
}

impl WBuf {
    pub fn new() -> Self {
        Self { buf: [0; 160], len: 0 }
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.buf[..self.len]
    }

    /// Appends one UTF-16 unit; `false` when the buffer is full.
    pub fn push(&mut self, unit: u16) -> bool {
        if self.len < self.buf.len() {
            self.buf[self.len] = unit;
            self.len += 1;
            true
        } else {
            false
        }
    }
}

impl std::fmt::Write for WBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for unit in s.encode_utf16() {
            if self.len < self.buf.len() {
                self.buf[self.len] = unit;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Borrows the UTF-16 contents of a `w!` literal.
pub fn ws(s: PCWSTR) -> &'static [u16] {
    // SAFETY: only ever called with `w!` literals, which are static and NUL-terminated.
    unsafe {
        let mut len = 0;
        while *s.0.add(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(s.0, len)
    }
}

/// How a range of note text is styled (see `notes::style`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    H1,
    H2,
    H3,
    Bold,
    Italic,
    /// Markdown markers: drawn in the secondary colour.
    Dim,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Span {
    pub start: u32,
    pub len: u32,
    pub style: Style,
}

/// The note in the editor. `serial` changes whenever the text does.
#[derive(Clone, Copy)]
pub struct NoteText<'a> {
    pub serial: u64,
    pub text: &'a [u16],
    pub spans: &'a [Span],
}

pub struct IslandPaths {
    pub fill: ID2D1PathGeometry1,
    pub edge: ID2D1PathGeometry1,
}

pub struct Gfx {
    pub(crate) d3d: ID3D11Device,
    swap: IDXGISwapChain1,
    // DirectComposition tree: root -> main (the swap chain) plus optional
    // compositor-animated layers on top (see `comp`).
    pub(crate) dcomp: IDCompositionDesktopDevice,
    _target: IDCompositionTarget,
    pub(crate) root: IDCompositionVisual2,
    _main: IDCompositionVisual2,
    /// The swap chain's D2D target, restored after drawing into a layer surface.
    pub(crate) main_bitmap: ID2D1Bitmap1,
    pub(crate) wave: RefCell<Option<crate::comp::WaveLayers>>,
    /// Wave layers being built a few cells per frame.
    pub(crate) wave_pending: RefCell<Option<crate::comp::WaveBuild>>,
    pub(crate) eq: RefCell<Option<crate::comp::EqLayers>>,
    /// Vertical two-colour gradient for the music line, kept for its colours.
    edge_brush: RefCell<Option<(Rgba, Rgba, ID2D1LinearGradientBrush)>>,
    /// Vertical gradients by their colours (the weather sky); the last few are kept.
    gradients: RefCell<Vec<(u64, ID2D1LinearGradientBrush)>>,
    /// Album art as a brush, with its pixel size.
    art: RefCell<Option<(ID2D1BitmapBrush1, u32)>>,
    /// Small pictures (clipboard thumbnails) by key, with their pixel size.
    images: RefCell<Vec<(u64, ID2D1BitmapBrush1, u32, u32)>>,
    /// Wrapping format, marker brush and cached layout of the note editor
    /// (keyed by the note's serial and the layout width).
    note_format: IDWriteTextFormat,
    dim: ID2D1SolidColorBrush,
    note: RefCell<Option<(u64, u32, IDWriteTextLayout)>>,
    /// Wrapped paragraphs (chat messages), keyed by a hash of text, width and bold runs.
    wraps: RefCell<Vec<(u64, IDWriteTextLayout)>>,
    pub(crate) factory: ID2D1Factory1,
    pub(crate) ctx: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle1,
    fonts: [IDWriteTextFormat; FONT_COUNT],
    pub(crate) scale: f32,
    size_px: (i32, i32),
    /// Current pass rectangle (dp, already offset-free) used to skip invisible primitives.
    cull: Cell<Rect>,
    shift: Cell<(f32, f32)>,
    /// A pose transform or a zoom is active: culling is off.
    posing: Cell<bool>,
    /// Zoom about a point (x, y, factor): the characters are drawn bigger.
    /// Applied before the offset.
    zoom: Cell<(f32, f32, f32)>,
    /// Zoom of everything drawn, offsets included (x, y, factor): the open
    /// panel at another size. Applied after the offset.
    base: Cell<(f32, f32, f32)>,
    /// Shaped text, keyed by a hash of content, font, box and alignment.
    layouts: RefCell<Vec<(u64, IDWriteTextLayout)>>,
    presented: Cell<u8>,
    /// Smoothed stage timings in ms for the debug HUD: geometry, draw calls, flush, present.
    pub prof: Cell<[f32; 4]>,
}

const LAYOUT_CACHE: usize = 96;

impl Gfx {
    /// `gpu = false` renders with the software rasteriser, `gpu = true` with the
    /// graphics driver (see the module docs for the trade-off).
    pub fn new(hwnd: HWND, width_px: u32, height_px: u32, scale: f32, gpu: bool) -> Result<Self> {
        unsafe {
            let d3d = create_d3d(gpu)?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width_px,
                Height: height_px,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;

            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi)?;
            let ctx = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let surface: IDXGISurface = swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let bitmap = ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            ctx.SetTarget(&bitmap);
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let dcomp: IDCompositionDesktopDevice = DCompositionCreateDevice2(&dxgi)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let root = dcomp.CreateVisual()?;
            let main = dcomp.CreateVisual()?;
            main.SetContent(&swap)?;
            root.AddVisual(&main, false, None)?;
            target.SetRoot(&root)?;
            dcomp.Commit()?;

            let brush = ctx.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?;
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES1 {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 4.0,
                    ..Default::default()
                },
                None,
            )?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let display = w!("Segoe UI Variable Display");
            let text = w!("Segoe UI Variable Text");
            let fmt = |family: PCWSTR, weight: DWRITE_FONT_WEIGHT, size: f32| -> Result<IDWriteTextFormat> {
                let f = dwrite.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                Ok(f)
            };
            // Order must match the `Font` enum.
            let fonts = [
                fmt(display, DWRITE_FONT_WEIGHT_BOLD, 26.0)?,
                fmt(display, DWRITE_FONT_WEIGHT_BOLD, 20.0)?,
                fmt(display, DWRITE_FONT_WEIGHT_BOLD, 16.0)?,
                fmt(text, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0)?,
                fmt(text, DWRITE_FONT_WEIGHT_NORMAL, 13.0)?,
                fmt(text, DWRITE_FONT_WEIGHT_SEMI_BOLD, 10.5)?,
                fmt(w!("Consolas"), DWRITE_FONT_WEIGHT_NORMAL, 11.0)?,
            ];

            let note_format = dwrite.CreateTextFormat(text, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 13.5, w!("en-us"))?;
            note_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let dim = ctx.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.5, g: 0.5, b: 0.5, a: 1.0 }, None)?;

            Ok(Self {
                images: RefCell::new(Vec::new()),
                note_format,
                dim,
                note: RefCell::new(None),
                wraps: RefCell::new(Vec::new()),
                d3d,
                swap,
                dcomp,
                _target: target,
                root,
                _main: main,
                main_bitmap: bitmap,
                wave: RefCell::new(None),
                wave_pending: RefCell::new(None),
                eq: RefCell::new(None),
                edge_brush: RefCell::new(None),
                gradients: RefCell::new(Vec::new()),
                art: RefCell::new(None),
                factory,
                ctx,
                dwrite,
                brush,
                round,
                fonts,
                scale,
                size_px: (width_px as i32, height_px as i32),
                cull: Cell::new(Rect::EMPTY),
                shift: Cell::new((0.0, 0.0)),
                posing: Cell::new(false),
                zoom: Cell::new((0.0, 0.0, 1.0)),
                base: Cell::new((0.0, 0.0, 1.0)),
                layouts: RefCell::new(Vec::with_capacity(LAYOUT_CACHE)),
                presented: Cell::new(0),
                prof: Cell::new([0.0; 4]),
            })
        }
    }

    /// `true` until both swap-chain buffers hold a complete frame; until then
    /// every frame must be a full redraw.
    pub fn needs_full(&self) -> bool {
        self.presented.get() < 2
    }

    /// Whole drawable area in dp.
    pub fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.size_px.0 as f32 / self.scale, self.size_px.1 as f32 / self.scale)
    }

    /// Grows `r` to whole physical pixels, clamped to the surface.
    fn to_px(&self, r: Rect) -> RECT {
        let s = self.scale;
        RECT {
            left: ((r.x0 * s).floor() as i32).clamp(0, self.size_px.0),
            top: ((r.y0 * s).floor() as i32).clamp(0, self.size_px.1),
            right: ((r.x1 * s).ceil() as i32).clamp(0, self.size_px.0),
            bottom: ((r.y1 * s).ceil() as i32).clamp(0, self.size_px.1),
        }
    }

    pub fn begin_frame(&self) {
        unsafe { self.ctx.BeginDraw() };
    }

    /// Starts redrawing one damaged rectangle: clips to it and clears it.
    pub fn begin_pass(&self, r: Rect) {
        let px = self.to_px(r);
        let s = self.scale;
        let snapped = Rect::new(px.left as f32 / s, px.top as f32 / s, px.right as f32 / s, px.bottom as f32 / s);
        self.cull.set(snapped);
        self.offset(0.0, 0.0);
        unsafe {
            self.ctx.PushAxisAlignedClip(&rect(snapped.x0, snapped.y0, snapped.x1 - snapped.x0, snapped.y1 - snapped.y0), D2D1_ANTIALIAS_MODE_ALIASED);
            self.ctx.Clear(None);
        }
    }

    pub fn end_pass(&self) {
        self.offset(0.0, 0.0);
        unsafe { self.ctx.PopAxisAlignedClip() };
    }

    /// Finishes the frame and presents the damaged rectangles (or everything
    /// when `damage` is `None`). An error means the device was lost and the
    /// caller should drop and recreate the renderer.
    pub fn end_frame(&self, damage: Option<&[Rect]>) -> Result<()> {
        unsafe {
            let t0 = std::time::Instant::now();
            self.ctx.EndDraw(None, None)?;
            self.note(2, t0);
            let t0 = std::time::Instant::now();
            let mut rects = [RECT::default(); 4];
            let mut count = 0;
            if let Some(list) = damage {
                for r in list.iter().filter(|r| !r.is_empty()).take(rects.len()) {
                    let px = self.to_px(*r);
                    if px.right > px.left && px.bottom > px.top {
                        rects[count] = px;
                        count += 1;
                    }
                }
            }
            let hr = if count > 0 && !self.needs_full() {
                let params = DXGI_PRESENT_PARAMETERS { DirtyRectsCount: count as u32, pDirtyRects: rects.as_mut_ptr(), ..Default::default() };
                self.swap.Present1(0, DXGI_PRESENT(0), &params)
            } else {
                self.swap.Present(0, DXGI_PRESENT(0))
            };
            hr.ok()?;
            self.note(3, t0);
            self.presented.set(self.presented.get().saturating_add(1));
            Ok(())
        }
    }

    /// Records how long stage `i` took since `since` (exponential moving average).
    pub fn note(&self, i: usize, since: std::time::Instant) {
        let mut p = self.prof.get();
        let ms = since.elapsed().as_secs_f32() * 1000.0;
        p[i] = if p[i] == 0.0 { ms } else { p[i] * 0.95 + ms * 0.05 };
        self.prof.set(p);
    }

    /// `true` when a box (in the current offset space) can touch the current pass.
    #[inline]
    fn visible(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        // Under a pose transform the untransformed box says nothing; the
        // pass's clip still limits the drawing.
        if self.posing.get() {
            return true;
        }
        let c = self.cull.get();
        let (dx, dy) = self.shift.get();
        x1 + dx >= c.x0 && x0 + dx <= c.x1 && y1 + dy >= c.y0 && y0 + dy <= c.y1
    }

    /// Lets the driver drop its caches once the island is at rest.
    pub fn trim(&self) {
        unsafe {
            if let Ok(dev) = self.d3d.cast::<IDXGIDevice3>() {
                dev.Trim();
            }
        }
        self.layouts.borrow_mut().clear();
        self.wraps.borrow_mut().clear();
        self.gradients.borrow_mut().clear();
        *self.note.borrow_mut() = None;
    }

    // ------------------------------------------------------------ images --

    /// Uploads a small premultiplied BGRA picture under `key` (at most 32 are kept).
    pub fn set_image(&self, key: u64, bgra: &[u8], w: u32, h: u32) {
        if self.has_image(key) || w == 0 || h == 0 || bgra.len() < (w * h * 4) as usize {
            return;
        }
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0, ..Default::default() };
        let brush = unsafe {
            self.ctx
                .CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as *const _), w * 4, &props)
                .and_then(|bitmap| self.ctx.CreateBitmapBrush(&bitmap, None, None))
        };
        if let Ok(brush) = brush {
            let mut images = self.images.borrow_mut();
            if images.len() >= 32 {
                images.remove(0);
            }
            images.push((key, brush, w, h));
        }
    }

    pub fn has_image(&self, key: u64) -> bool {
        self.images.borrow().iter().any(|(k, ..)| *k == key)
    }

    pub fn clear_images(&self) {
        self.images.borrow_mut().clear();
    }

    /// Draws picture `key` filling a rounded box (cropped to cover it).
    /// Returns `false` when it has not been uploaded.
    #[allow(clippy::too_many_arguments)]
    pub fn image(&self, key: u64, x: f32, y: f32, w: f32, h: f32, r: f32, alpha: f32) -> bool {
        let images = self.images.borrow();
        let Some((_, brush, pw, ph)) = images.iter().find(|(k, ..)| *k == key) else { return false };
        if alpha <= 0.003 || !self.visible(x, y, x + w, y + h) {
            return true;
        }
        let k = (w / *pw as f32).max(h / *ph as f32);
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe {
            brush.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x + (w - *pw as f32 * k) * 0.5, M32: y + (h - *ph as f32 * k) * 0.5 });
            brush.SetOpacity(alpha);
            self.ctx.FillRoundedRectangle(&rr, brush);
        }
        true
    }

    // -------------------------------------------------------- note editor --

    /// The wrapped, styled layout of a note, shaped again only after an edit.
    fn note_layout(&self, n: &NoteText, w: f32) -> Option<IDWriteTextLayout> {
        let mut slot = self.note.borrow_mut();
        if let Some((serial, width, layout)) = slot.as_ref() {
            if *serial == n.serial && *width == w.to_bits() {
                return Some(layout.clone());
            }
        }
        let layout = unsafe {
            let layout = self.dwrite.CreateTextLayout(n.text, &self.note_format, w, 100_000.0).ok()?;
            for span in n.spans {
                let range = DWRITE_TEXT_RANGE { startPosition: span.start, length: span.len };
                let heading = |size: f32| layout.SetFontSize(size, range).and_then(|_| layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range));
                let _ = match span.style {
                    Style::H1 => heading(21.0),
                    Style::H2 => heading(17.5),
                    Style::H3 => heading(15.0),
                    Style::Bold => layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range),
                    Style::Italic => layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, range),
                    Style::Dim => layout.SetDrawingEffect(&self.dim, range),
                };
            }
            layout
        };
        *slot = Some((n.serial, w.to_bits(), layout.clone()));
        Some(layout)
    }

    /// Where the caret sits before text position `pos`: x, top and height,
    /// relative to the top-left corner of the note.
    pub fn note_caret(&self, n: &NoteText, w: f32, pos: u32) -> Option<(f32, f32, f32)> {
        let layout = self.note_layout(n, w)?;
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe { layout.HitTestTextPosition(pos.min(n.text.len() as u32), false, &mut x, &mut y, &mut m).ok()? };
        Some((x, y, m.height.max(12.0)))
    }

    /// The text position nearest to a point given relative to the note's top-left corner.
    pub fn note_hit(&self, n: &NoteText, w: f32, x: f32, y: f32) -> Option<u32> {
        let layout = self.note_layout(n, w)?;
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe { layout.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m).ok()? };
        let mut pos = m.textPosition;
        // The trailing side of a line break belongs to the next line.
        if trailing.as_bool() && n.text.get(pos as usize) != Some(&(b'\n' as u16)) {
            pos += m.length;
        }
        Some(pos.min(n.text.len() as u32))
    }

    /// Draws the note in a box, scrolled by `scroll` dp, with its selection and caret.
    #[allow(clippy::too_many_arguments)]
    pub fn note_draw(&self, n: &NoteText, x: f32, y: f32, w: f32, h: f32, scroll: f32, caret: Option<u32>, selection: Option<(u32, u32)>, ink: Rgba, dim: Rgba, highlight: Rgba) {
        if ink.a <= 0.003 || !self.visible(x, y, x + w, y + h) {
            return;
        }
        let Some(layout) = self.note_layout(n, w) else { return };
        let top = y - scroll;
        unsafe {
            self.dim.SetColor(&D2D1_COLOR_F { r: dim.r, g: dim.g, b: dim.b, a: dim.a });
            self.ctx.PushAxisAlignedClip(&rect(x - 2.0, y, w + 4.0, h), D2D1_ANTIALIAS_MODE_ALIASED);
            if let Some((from, to)) = selection {
                let mut boxes = [DWRITE_HIT_TEST_METRICS::default(); 48];
                let mut count = 0;
                let _ = layout.HitTestTextRange(from, to - from, x, top, Some(&mut boxes), &mut count);
                for m in &boxes[..(count as usize).min(boxes.len())] {
                    self.ctx.FillRectangle(&rect(m.left, m.top, m.width.max(3.0), m.height), self.brush(highlight.alpha(highlight.a * 0.45)));
                }
            }
            self.ctx.DrawTextLayout(Vector2 { X: x, Y: top }, &layout, self.brush(ink), D2D1_DRAW_TEXT_OPTIONS_NONE);
            if let Some(pos) = caret {
                let (mut cx, mut cy) = (0.0, 0.0);
                let mut m = DWRITE_HIT_TEST_METRICS::default();
                if layout.HitTestTextPosition(pos.min(n.text.len() as u32), false, &mut cx, &mut cy, &mut m).is_ok() {
                    let height = m.height.max(12.0);
                    self.ctx.DrawLine(Vector2 { X: x + cx + 0.5, Y: top + cy + 1.0 }, Vector2 { X: x + cx + 0.5, Y: top + cy + height - 1.0 }, self.brush(highlight), 1.6, &self.round);
                }
            }
            self.ctx.PopAxisAlignedClip();
        }
    }

    /// Translates subsequent drawing by `(dx, dy)` dp.
    /// The offset set by [`Gfx::offset`] (to shift something further and restore it).
    pub fn current_offset(&self) -> (f32, f32) {
        self.shift.get()
    }

    pub fn offset(&self, dx: f32, dy: f32) {
        self.shift.set((dx, dy));
        // (Inside a zoom: the offset is in the zoomed units.)
        self.apply_local(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe { self.ctx.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn unclip(&self) {
        unsafe { self.ctx.PopAxisAlignedClip() };
    }

    /// Test aid: writes the last drawn frame, composited over mid-grey, as a
    /// 32-bit BMP. Compositor layers (wave strip, equalizer) are not included.
    pub fn save_bmp(&self, path: &std::path::Path) -> Result<()> {
        unsafe {
            let size = self.main_bitmap.GetPixelSize();
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let copy = self.ctx.CreateBitmap(size, None, 0, &props)?;
            // This is the back buffer, i.e. the frame before the last one presented;
            // the caller draws two identical frames first so both buffers match.
            copy.CopyFromBitmap(None, &self.main_bitmap, None)?;
            let map = copy.Map(D2D1_MAP_OPTIONS_READ)?;
            let (w, h) = (size.width as usize, size.height as usize);
            let mut out = Vec::with_capacity(54 + w * h * 4);
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&((54 + w * h * 4) as u32).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(w as i32).to_le_bytes());
            out.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&[0; 24]);
            for y in 0..h {
                let row = std::slice::from_raw_parts(map.bits.add(y * map.pitch as usize), w * 4);
                for px in row.as_chunks::<4>().0 {
                    // Premultiplied source over a 0x60 grey background.
                    let rest = 255 - px[3] as u32;
                    for &c in &px[..3] {
                        out.push((c as u32 + 0x60 * rest / 255).min(255) as u8);
                    }
                    out.push(255);
                }
            }
            copy.Unmap()?;
            std::fs::write(path, out).map_err(|_| windows::core::Error::empty())
        }
    }

    /// Uploads square premultiplied-BGRA album art (`size` x `size` pixels).
    pub fn set_art(&self, bgra: &[u8], size: u32) {
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }, dpiX: 96.0, dpiY: 96.0, ..Default::default() };
        let brush = unsafe {
            self.ctx
                .CreateBitmap(D2D_SIZE_U { width: size, height: size }, Some(bgra.as_ptr() as *const _), size * 4, &props)
                .and_then(|bitmap| self.ctx.CreateBitmapBrush(&bitmap, None, None))
        };
        *self.art.borrow_mut() = brush.ok().map(|b| (b, size));
    }

    pub fn clear_art(&self) {
        *self.art.borrow_mut() = None;
    }

    /// Draws the album art as a rounded square. Returns `false` when there is none.
    pub fn art(&self, x: f32, y: f32, size: f32, r: f32, alpha: f32) -> bool {
        let slot = self.art.borrow();
        let Some((brush, pixels)) = slot.as_ref() else { return false };
        if alpha <= 0.003 || !self.visible(x, y, x + size, y + size) {
            return true;
        }
        let k = size / *pixels as f32;
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, size, size), radiusX: r, radiusY: r };
        unsafe {
            brush.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x, M32: y });
            brush.SetOpacity(alpha);
            self.ctx.FillRoundedRectangle(&rr, brush);
        }
        true
    }

    /// Disables primitive culling (used while drawing into layer surfaces).
    pub(crate) fn cull_none(&self) {
        self.cull.set(Rect::new(-1.0e9, -1.0e9, 1.0e9, 1.0e9));
        self.shift.set((0.0, 0.0));
    }

    pub(crate) fn brush(&self, c: Rgba) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }) };
        &self.brush
    }

    pub fn fill_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        if c.a <= 0.003 || !self.visible(x, y, x + w, y + h) {
            return;
        }
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.ctx.FillRoundedRectangle(&rr, self.brush(c)) };
    }

    /// A card: the filled rounded rectangle plus a hairline rim just inside it
    /// (light on dark fills, dark on light ones), so cards read as surfaces
    /// instead of melting into the island.
    pub fn card(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        self.fill_rrect(x, y, w, h, r, c);
        let luma = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        let rim = if luma < 0.5 { Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.075 * c.a } } else { Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.07 * c.a } };
        self.stroke_rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, (r - 0.5).max(0.0), 1.0, rim);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: Rgba) {
        if c.a <= 0.003 || !self.visible(x - width, y - width, x + w + width, y + h + width) {
            return;
        }
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.ctx.DrawRoundedRectangle(&rr, self.brush(c), width, &self.round) };
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Rgba) {
        if c.a <= 0.003 || !self.visible(cx - r, cy - r, cx + r, cy + r) {
            return;
        }
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe { self.ctx.FillEllipse(&e, self.brush(c)) };
    }

    pub fn fill_ellipse(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Rgba) {
        if c.a <= 0.003 || !self.visible(cx - rx, cy - ry, cx + rx, cy + ry) {
            return;
        }
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: rx, radiusY: ry };
        unsafe { self.ctx.FillEllipse(&e, self.brush(c)) };
    }

    /// A path of lines and cubic curves from `start` (closed for filling,
    /// open for stroking).
    pub fn path(&self, start: (f32, f32), segs: &[Seg], closed: bool) -> Option<ID2D1PathGeometry1> {
        unsafe {
            let geometry = self.factory.CreatePathGeometry().ok()?;
            let sink = geometry.Open().ok()?;
            let v = |p: (f32, f32)| Vector2 { X: p.0, Y: p.1 };
            sink.BeginFigure(v(start), if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            for seg in segs {
                match *seg {
                    Seg::Line(p) => sink.AddLine(v(p)),
                    Seg::Cubic(a, b, p) => sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(a), point2: v(b), point3: v(p) }),
                }
            }
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(geometry)
        }
    }

    /// A filled polygon (ears, beaks, tails): at most a handful of points.
    pub fn fill_poly(&self, pts: &[(f32, f32)], c: Rgba) {
        if c.a <= 0.003 || pts.len() < 3 {
            return;
        }
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geometry.Open() else { return };
            sink.BeginFigure(Vector2 { X: pts[0].0, Y: pts[0].1 }, D2D1_FIGURE_BEGIN_FILLED);
            for p in &pts[1..] {
                sink.AddLine(Vector2 { X: p.0, Y: p.1 });
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                self.ctx.FillGeometry(&geometry, self.brush(c), None);
            }
        }
    }

    /// Draws what follows tilted by `tilt` radians and scaled by (`sx`, `sy`)
    /// about the pivot (`px`, `py`), then moved by (`dx`, `dy`) dp, on top of
    /// the current [`Gfx::offset`]. [`Gfx::pose_end`] goes back.
    #[allow(clippy::too_many_arguments)]
    pub fn pose_begin(&self, px: f32, py: f32, dx: f32, dy: f32, tilt: f32, sx: f32, sy: f32) {
        let (sin, cos) = tilt.sin_cos();
        let (m11, m12, m21, m22) = (sx * cos, sx * sin, -sy * sin, sy * cos);
        // x' = x * m11 + y * m21 + tx, y' = x * m12 + y * m22 + ty
        let (tx, ty) = (px - px * m11 - py * m21 + dx, py - px * m12 - py * m22 + dy);
        self.apply_local(m11, m12, m21, m22, tx, ty);
        self.posing.set(true);
    }

    /// Ends a pose transform (back to the zoom, if one is set).
    pub fn pose_end(&self) {
        self.posing.set(self.zoom.get().2 != 1.0 || self.base.get().2 != 1.0);
        self.apply_local(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    /// Draws what follows `k` times bigger about (`cx`, `cy`) dp, pose
    /// transforms included, until [`Gfx::zoom_end`].
    pub fn zoom_begin(&self, cx: f32, cy: f32, k: f32) {
        self.zoom.set((cx, cy, k));
        self.posing.set(k != 1.0 || self.base.get().2 != 1.0);
        self.apply_local(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    pub fn zoom_end(&self) {
        self.zoom.set((0.0, 0.0, 1.0));
        self.posing.set(self.base.get().2 != 1.0);
        let (ox, oy) = self.shift.get();
        self.offset(ox, oy);
    }

    /// Draws everything that follows, offsets included, `k` times bigger about
    /// (`cx`, `cy`) dp until [`Gfx::base_end`] (the open panel at another size).
    pub fn base_begin(&self, cx: f32, cy: f32, k: f32) {
        self.base.set((cx, cy, k));
        self.posing.set(true);
        self.apply_local(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    pub fn base_end(&self) {
        self.base.set((0.0, 0.0, 1.0));
        self.posing.set(self.zoom.get().2 != 1.0);
        self.apply_local(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    }

    /// Sets a dp-space transform, then the zoom, the offset, the base zoom and the DPI scale.
    fn apply_local(&self, m11: f32, m12: f32, m21: f32, m22: f32, tx: f32, ty: f32) {
        let (zx, zy, k) = self.zoom.get();
        // Zoom: p -> z + k (p - z).
        let (m11, m12, m21, m22) = (k * m11, k * m12, k * m21, k * m22);
        let (tx, ty) = (zx + k * (tx - zx), zy + k * (ty - zy));
        let (ox, oy) = self.shift.get();
        let (tx, ty) = (tx + ox, ty + oy);
        // Base zoom: p -> b + kb (p - b).
        let (bx, by, kb) = self.base.get();
        let (m11, m12, m21, m22) = (kb * m11, kb * m12, kb * m21, kb * m22);
        let (tx, ty) = (bx + kb * (tx - bx), by + kb * (ty - by));
        let s = self.scale;
        let m = Matrix3x2 { M11: s * m11, M12: s * m12, M21: s * m21, M22: s * m22, M31: s * tx, M32: s * ty };
        unsafe { self.ctx.SetTransform(&m) };
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, width: f32, c: Rgba) {
        let e = r + width;
        if c.a <= 0.003 || !self.visible(cx - e, cy - e, cx + e, cy + e) {
            return;
        }
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe { self.ctx.DrawEllipse(&e, self.brush(c), width, &self.round) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        if c.a <= 0.003 || !self.visible(x0.min(x1) - width, y0.min(y1) - width, x0.max(x1) + width, y0.max(y1) + width) {
            return;
        }
        unsafe { self.ctx.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, self.brush(c), width, &self.round) };
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(&self, s: &[u16], font: Font, x: f32, y: f32, w: f32, h: f32, c: Rgba, align: Align) {
        if c.a <= 0.003 || s.is_empty() || !self.visible(x, y, x + w, y + h) {
            return;
        }
        if let Some(layout) = self.layout(s, font, w, h, align) {
            unsafe { self.ctx.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// Returns the shaped layout for a string, shaping it only on first use.
    fn layout(&self, s: &[u16], font: Font, w: f32, h: f32, align: Align) -> Option<IDWriteTextLayout> {
        // FNV-1a over everything that affects the layout.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |v: u64| hash = (hash ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        for &unit in s {
            feed(unit as u64);
        }
        feed(0x1_0000 | font as u64);
        feed(0x2_0000 | align as u64);
        feed(w.to_bits() as u64);
        feed(h.to_bits() as u64);

        let mut cache = self.layouts.borrow_mut();
        if let Some((_, layout)) = cache.iter().find(|(key, _)| *key == hash) {
            return Some(layout.clone());
        }
        let f = &self.fonts[font as usize];
        let a = match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        };
        let layout = unsafe {
            let _ = f.SetTextAlignment(a);
            self.dwrite.CreateTextLayout(s, f, w, h).ok()?
        };
        if cache.len() >= LAYOUT_CACHE {
            cache.clear();
        }
        cache.push((hash, layout.clone()));
        Some(layout)
    }

    /// A paragraph wrapped to `w` dp in the body size, with `bold` runs
    /// (start, length in UTF-16 units). The last few are kept.
    fn wrap_layout(&self, s: &[u16], w: f32, bold: &[(u32, u32)]) -> Option<IDWriteTextLayout> {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |v: u64| hash = (hash ^ v).wrapping_mul(0x0000_0100_0000_01b3);
        s.iter().for_each(|&unit| feed(unit as u64));
        bold.iter().for_each(|&(start, len)| feed((start as u64) << 32 | len as u64));
        feed(w.to_bits() as u64);
        let mut cache = self.wraps.borrow_mut();
        if let Some((_, layout)) = cache.iter().find(|(key, _)| *key == hash) {
            return Some(layout.clone());
        }
        let layout = unsafe {
            let layout = self.dwrite.CreateTextLayout(s, &self.note_format, w, 100_000.0).ok()?;
            for &(start, len) in bold {
                let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_RANGE { startPosition: start, length: len });
            }
            layout
        };
        if cache.len() >= 48 {
            cache.remove(0);
        }
        cache.push((hash, layout.clone()));
        Some(layout)
    }

    /// Width and height the wrapped paragraph takes.
    pub fn wrapped_size(&self, s: &[u16], w: f32, bold: &[(u32, u32)]) -> (f32, f32) {
        let mut m = DWRITE_TEXT_METRICS::default();
        match self.wrap_layout(s, w, bold) {
            Some(layout) if unsafe { layout.GetMetrics(&mut m) }.is_ok() => (m.widthIncludingTrailingWhitespace, m.height),
            _ => (0.0, 0.0),
        }
    }

    /// Draws the wrapped paragraph with its top-left corner at (x, y).
    pub fn wrapped(&self, s: &[u16], x: f32, y: f32, w: f32, c: Rgba, bold: &[(u32, u32)]) {
        if c.a <= 0.003 || s.is_empty() {
            return;
        }
        if let Some(layout) = self.wrap_layout(s, w, bold) {
            unsafe { self.ctx.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, self.brush(c), D2D1_DRAW_TEXT_OPTIONS_NONE) };
        }
    }

    /// Width in dp of `s` set in `font`. Allocates a layout; call at setup, not per frame.
    pub fn measure(&self, s: &[u16], font: Font) -> f32 {
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(s, &self.fonts[font as usize], 4096.0, 64.0) else {
                return 0.0;
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                m.widthIncludingTrailingWhitespace
            } else {
                0.0
            }
        }
    }

    /// Builds the island outline (filled) and its bottom edge (open stroke path).
    pub fn island_paths(&self, shape: &Shape, pts: &[Vector2; N]) -> Result<IslandPaths> {
        const K: f32 = 0.552_284_8;
        let (left, right, sr) = (shape.left(), shape.right(), shape.sr);
        let mid = |a: Vector2, b: Vector2| Vector2 { X: (a.X + b.X) * 0.5, Y: (a.Y + b.Y) * 0.5 };

        // Smooth the sampled contour: quadratic segments through the midpoints.
        let mut quads = [D2D1_QUADRATIC_BEZIER_SEGMENT::default(); N - 2];
        for i in 1..N - 1 {
            let end = if i == N - 2 { pts[N - 1] } else { mid(pts[i], pts[i + 1]) };
            quads[i - 1] = D2D1_QUADRATIC_BEZIER_SEGMENT { point1: pts[i], point2: end };
        }
        let first_mid = mid(pts[0], pts[1]);

        unsafe {
            let fill = self.factory.CreatePathGeometry()?;
            let sink = fill.Open()?;
            sink.BeginFigure(Vector2 { X: left - sr, Y: 0.0 }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                point1: Vector2 { X: left - sr + K * sr, Y: 0.0 },
                point2: Vector2 { X: left, Y: sr - K * sr },
                point3: Vector2 { X: left, Y: sr },
            });
            sink.AddLine(pts[0]);
            sink.AddLine(first_mid);
            sink.AddQuadraticBeziers(&quads);
            sink.AddLine(Vector2 { X: right, Y: sr });
            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                point1: Vector2 { X: right, Y: sr - K * sr },
                point2: Vector2 { X: right + sr - K * sr, Y: 0.0 },
                point3: Vector2 { X: right + sr, Y: 0.0 },
            });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;

            let edge = self.factory.CreatePathGeometry()?;
            let sink = edge.Open()?;
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLine(first_mid);
            sink.AddQuadraticBeziers(&quads);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;

            Ok(IslandPaths { fill, edge })
        }
    }

    pub fn fill_path(&self, g: &ID2D1PathGeometry1, c: Rgba) {
        if c.a <= 0.003 {
            return;
        }
        unsafe { self.ctx.FillGeometry(g, self.brush(c), None) };
    }

    /// A rounded rectangle filled top to bottom through `stops` (position 0..1,
    /// colour), at opacity `alpha`. Brushes are cached by their colours.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_vgradient(&self, x: f32, y: f32, w: f32, h: f32, r: f32, stops: &[(f32, Rgba)], alpha: f32) {
        if alpha <= 0.003 || stops.is_empty() || !self.visible(x, y, x + w, y + h) {
            return;
        }
        let Some(brush) = self.gradient(stops, (0.0, y), (0.0, y + h), alpha) else {
            self.fill_rrect(x, y, w, h, r, stops[0].1.alpha(alpha));
            return;
        };
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.ctx.FillRoundedRectangle(&rr, &brush) };
    }

    /// Fills a rounded rectangle left to right through `stops`.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_hgradient(&self, x: f32, y: f32, w: f32, h: f32, r: f32, stops: &[(f32, Rgba)], alpha: f32) {
        if alpha <= 0.003 || stops.is_empty() || !self.visible(x, y, x + w, y + h) {
            return;
        }
        let Some(brush) = self.gradient(stops, (x, 0.0), (x + w, 0.0), alpha) else { return };
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.ctx.FillRoundedRectangle(&rr, &brush) };
    }

    /// Fills a path top to bottom through `stops` between `y0` and `y1`.
    pub fn fill_path_vgradient(&self, path: &ID2D1PathGeometry1, y0: f32, y1: f32, stops: &[(f32, Rgba)], alpha: f32) {
        if alpha <= 0.003 || stops.is_empty() {
            return;
        }
        if let Some(brush) = self.gradient(stops, (0.0, y0), (0.0, y1), alpha) {
            unsafe { self.ctx.FillGeometry(path, &brush, None) };
        }
    }

    /// A linear gradient brush from point `from` to point `to` (cached by its colours).
    fn gradient(&self, stops: &[(f32, Rgba)], from: (f32, f32), to: (f32, f32), alpha: f32) -> Option<ID2D1LinearGradientBrush> {
        let mut key: u64 = 0xcbf2_9ce4_8422_2325;
        for (p, c) in stops {
            for v in [*p, c.r, c.g, c.b, c.a] {
                key = (key ^ v.to_bits() as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        let mut cache = self.gradients.borrow_mut();
        let brush = match cache.iter().find(|(k, _)| *k == key) {
            Some((_, brush)) => brush.clone(),
            None => {
                let list: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a } }).collect();
                let brush = unsafe {
                    ID2D1RenderTarget::CreateGradientStopCollection(&self.ctx, &list, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                        .and_then(|stops| {
                            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: 0.0 }, endPoint: Vector2 { X: 0.0, Y: 1.0 } };
                            self.ctx.CreateLinearGradientBrush(&props, None, &stops)
                        })
                        .ok()?
                };
                if cache.len() >= 8 {
                    cache.remove(0);
                }
                cache.push((key, brush.clone()));
                brush
            }
        };
        unsafe {
            brush.SetStartPoint(Vector2 { X: from.0, Y: from.1 });
            brush.SetEndPoint(Vector2 { X: to.0.max(from.0 + if to.1 == from.1 { 0.01 } else { 0.0 }), Y: to.1.max(from.1 + if to.0 == from.0 { 0.01 } else { 0.0 }) });
            brush.SetOpacity(alpha);
        }
        Some(brush)
    }

    pub fn stroke_path(&self, g: &ID2D1PathGeometry1, width: f32, c: Rgba) {
        if c.a <= 0.003 {
            return;
        }
        unsafe { self.ctx.DrawGeometry(g, self.brush(c), width, &self.round) };
    }

    /// The glowing music line: `upper` at `y0` blending into `lower` at `y1`
    /// (dp, clamped beyond), with a soft halo of wider faint strokes under it.
    /// `glow` (0..1) scales the halo.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_glow(&self, g: &ID2D1PathGeometry1, width: f32, upper: Rgba, lower: Rgba, y0: f32, y1: f32, glow: f32) {
        let mut slot = self.edge_brush.borrow_mut();
        if slot.as_ref().is_none_or(|(a, b, _)| *a != upper || *b != lower) {
            *slot = None;
            let stop = |position: f32, c: Rgba| D2D1_GRADIENT_STOP { position, color: D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a } };
            let stops = [stop(0.0, upper), stop(1.0, lower)];
            let made = unsafe {
                ID2D1RenderTarget::CreateGradientStopCollection(&self.ctx, &stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).and_then(|stops| {
                    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: 0.0 }, endPoint: Vector2 { X: 0.0, Y: 1.0 } };
                    self.ctx.CreateLinearGradientBrush(&props, None, &stops)
                })
            };
            match made {
                Ok(brush) => *slot = Some((upper, lower, brush)),
                Err(_) => {
                    drop(slot);
                    self.stroke_path(g, width, lower);
                    return;
                }
            }
        }
        let Some((_, _, brush)) = slot.as_ref() else { return };
        unsafe {
            brush.SetStartPoint(Vector2 { X: 0.0, Y: y0 });
            brush.SetEndPoint(Vector2 { X: 0.0, Y: y1.max(y0 + 0.01) });
            if glow > 0.01 {
                for (w, a) in [(3.6, 0.09), (2.2, 0.16)] {
                    brush.SetOpacity(a * glow);
                    self.ctx.DrawGeometry(g, brush, width * w, &self.round);
                }
            }
            brush.SetOpacity(1.0);
            self.ctx.DrawGeometry(g, brush, width, &self.round);
        }
    }
}

/// A piece of a [`Gfx::path`].
#[derive(Clone, Copy)]
pub enum Seg {
    Line((f32, f32)),
    /// Two control points, then the end.
    Cubic((f32, f32), (f32, f32), (f32, f32)),
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

unsafe fn create_d3d(gpu: bool) -> Result<ID3D11Device> {
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS;
    let mut device = None;
    let create = |driver, device: &mut Option<ID3D11Device>| unsafe { D3D11CreateDevice(None, driver, HMODULE::default(), flags, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(device), None, None) };
    // The software rasteriser is also the fallback when no GPU is usable
    // (remote session, driver update in progress).
    if !gpu || create(D3D_DRIVER_TYPE_HARDWARE, &mut device).is_err() {
        create(D3D_DRIVER_TYPE_WARP, &mut device)?;
    }
    device.ok_or_else(windows::core::Error::empty)
}
