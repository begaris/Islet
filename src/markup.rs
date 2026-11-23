//! Screenshot markup: pen, arrow, rectangle, text and blur over a frozen
//! screenshot, then copy or save.
//!
//! [`Editor`] is the model (marks, tool, colour, toolbar layout, input);
//! [`Painter`] draws it with Direct2D's *software* DC render target, so it
//! antialiases without a Direct3D device and the same code draws the marks into
//! the exported picture. Coordinates are window pixels (the window covers the
//! monitor 1:1); `k` is the monitor's DPI scale for sizes.

use windows::core::{w, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows_numerics::{Matrix3x2, Vector2};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pick {
    Pen,
    Arrow,
    Rect,
    Text,
    /// Pixelation (unlike a real blur it cannot be undone to read hidden text).
    Blur,
}

const TOOLS: [Pick; 5] = [Pick::Pen, Pick::Arrow, Pick::Rect, Pick::Text, Pick::Blur];
/// Red, yellow, green, blue, white (RGB).
pub const COLORS: [u32; 5] = [0xff453a, 0xffd60a, 0x30d158, 0x0a84ff, 0xffffff];
const ACCENT: u32 = 0x8b8cf7;

type Point = (f32, f32);

#[derive(Clone, PartialEq, Debug)]
pub enum Mark {
    Pen { pts: Vec<Point>, color: u32, w: f32 },
    Arrow { a: Point, b: Point, color: u32, w: f32 },
    Rect { a: Point, b: Point, color: u32, w: f32 },
    Text { at: Point, text: String, color: u32 },
    /// Blocks of averaged colour (RGB) covering the rectangle a-b.
    Blur { a: Point, b: Point, blocks: Vec<(RECT, u32)> },
}

fn span(a: Point, b: Point) -> (f32, f32, f32, f32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn rect_of(l: f32, t: f32, r: f32, b: f32, grow: f32) -> RECT {
    RECT { left: (l - grow).floor() as i32, top: (t - grow).floor() as i32, right: (r + grow).ceil() as i32, bottom: (b + grow).ceil() as i32 }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

fn contains(r: &RECT, x: f32, y: f32) -> bool {
    x >= r.left as f32 && x < r.right as f32 && y >= r.top as f32 && y < r.bottom as f32
}

impl Mark {
    /// What a redraw must cover for this mark.
    fn bounds(&self, k: f32) -> RECT {
        match self {
            Mark::Pen { pts, w, .. } => {
                let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for &(x, y) in pts {
                    (l, t, r, b) = (l.min(x), t.min(y), r.max(x), b.max(y));
                }
                rect_of(l, t, r, b, w + 2.0)
            }
            Mark::Arrow { a, b, w, .. } => {
                let (l, t, r, bt) = span(*a, *b);
                rect_of(l, t, r, bt, arrow_head(*w, k) + 2.0)
            }
            Mark::Rect { a, b, w, .. } => {
                let (l, t, r, bt) = span(*a, *b);
                rect_of(l, t, r, bt, w + 2.0)
            }
            Mark::Blur { a, b, .. } => {
                let (l, t, r, bt) = span(*a, *b);
                rect_of(l, t, r, bt, 2.0)
            }
            // Text width is not known here: callers redraw everything for text.
            Mark::Text { at, .. } => rect_of(at.0, at.1, at.0 + 4000.0, at.1 + 80.0 * k, 4.0),
        }
    }
}

fn arrow_head(w: f32, k: f32) -> f32 {
    (w * 4.5).max(16.0 * k)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Tool(Pick),
    Color(usize),
    Undo,
    Copy,
    Save,
    Close,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reply {
    Nothing,
    /// Repaint this part of the window (`None`: all of it).
    Redraw(Option<RECT>),
    Copy,
    Save,
    Close,
}

pub struct Editor {
    /// The part of the screen being marked up (window pixels).
    pub area: RECT,
    size: (i32, i32),
    k: f32,
    pub tool: Pick,
    pub color: usize,
    pub marks: Vec<Mark>,
    drawing: Option<Mark>,
    /// Text being typed: where, and what so far.
    typing: Option<(Point, String)>,
    hover: Option<Button>,
}

impl Editor {
    pub fn new(area: RECT, size: (i32, i32), k: f32) -> Editor {
        Editor { area, size, k, tool: Pick::Arrow, color: 0, marks: Vec::new(), drawing: None, typing: None, hover: None }
    }

    fn stroke(&self) -> f32 {
        match self.tool {
            Pick::Pen => 4.0 * self.k,
            Pick::Rect => 3.0 * self.k,
            _ => 4.0 * self.k,
        }
    }

    fn block(&self) -> i32 {
        ((12.0 * self.k) as i32).max(6)
    }

    pub fn text_size(&self) -> f32 {
        26.0 * self.k
    }

    /// The toolbar and its buttons: under the area, else above it, else inside its bottom.
    pub fn toolbar(&self) -> (RECT, Vec<(Button, RECT)>) {
        let k = self.k;
        let (unit, pad, gap, group) = ((36.0 * k) as i32, (6.0 * k) as i32, (4.0 * k) as i32, (12.0 * k) as i32);
        let swatch = (28.0 * k) as i32;
        let wide = (64.0 * k) as i32;
        let mut items: Vec<(Button, i32, i32)> = Vec::new();
        for tool in TOOLS {
            items.push((Button::Tool(tool), unit, gap));
        }
        items.last_mut().unwrap().2 = group;
        for i in 0..COLORS.len() {
            items.push((Button::Color(i), swatch, if i + 1 == COLORS.len() { group } else { 0 }));
        }
        items.push((Button::Undo, unit, group));
        items.push((Button::Copy, wide, gap));
        items.push((Button::Save, wide, gap));
        items.push((Button::Close, unit, 0));
        let width = pad * 2 + items.iter().map(|(_, w, g)| w + g).sum::<i32>();
        let height = unit + pad * 2;
        let margin = (10.0 * k) as i32;
        let centre = (self.area.left + self.area.right) / 2;
        let left = (centre - width / 2).clamp(8, (self.size.0 - width - 8).max(8));
        let top = if self.area.bottom + margin + height <= self.size.1 - 8 {
            self.area.bottom + margin
        } else if self.area.top - margin - height >= 8 {
            self.area.top - margin - height
        } else {
            self.area.bottom - height - 2 * margin
        };
        let mut x = left + pad;
        let buttons = items
            .into_iter()
            .map(|(button, w, g)| {
                let r = RECT { left: x, top: top + pad, right: x + w, bottom: top + pad + unit };
                x += w + g;
                (button, r)
            })
            .collect();
        (RECT { left, top, right: left + width, bottom: top + height }, buttons)
    }

    fn button_at(&self, x: f32, y: f32) -> Option<Button> {
        let (bar, buttons) = self.toolbar();
        if !contains(&bar, x, y) {
            return None;
        }
        buttons.into_iter().find(|(_, r)| contains(r, x, y)).map(|(b, _)| b)
    }

    pub fn on_toolbar(&self, x: f32, y: f32) -> bool {
        contains(&self.toolbar().0, x, y)
    }

    fn clamp(&self, x: f32, y: f32) -> Point {
        (x.clamp(self.area.left as f32, self.area.right as f32), y.clamp(self.area.top as f32, self.area.bottom as f32))
    }

    /// Finishes the text being typed (an empty one is dropped).
    fn commit(&mut self) -> bool {
        match self.typing.take() {
            Some((at, text)) if !text.is_empty() => {
                self.marks.push(Mark::Text { at, text, color: COLORS[self.color] });
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn undo(&mut self) -> Reply {
        if self.typing.take().is_none() {
            self.marks.pop();
        }
        Reply::Redraw(None)
    }

    /// The blocks of a blur over a-b; `average` gives the RGB of a rectangle of the screenshot.
    fn blocks(&self, a: Point, b: Point, average: &dyn Fn(RECT) -> u32) -> Vec<(RECT, u32)> {
        let (l, t, r, bt) = span(a, b);
        let (l, t, r, bt) = (l as i32, t as i32, r as i32, bt as i32);
        let step = self.block();
        let mut blocks = Vec::new();
        let mut y = t;
        while y < bt {
            let mut x = l;
            while x < r {
                let block = RECT { left: x, top: y, right: (x + step).min(r), bottom: (y + step).min(bt) };
                blocks.push((block, average(block)));
                x += step;
            }
            y += step;
        }
        blocks
    }

    fn press(&mut self, button: Button) -> Reply {
        match button {
            Button::Tool(tool) => {
                self.commit();
                self.tool = tool;
                Reply::Redraw(None)
            }
            Button::Color(i) => {
                self.color = i;
                Reply::Redraw(None)
            }
            Button::Undo => self.undo(),
            Button::Copy => {
                self.commit();
                Reply::Copy
            }
            Button::Save => {
                self.commit();
                Reply::Save
            }
            Button::Close => Reply::Close,
        }
    }

    pub fn down(&mut self, x: f32, y: f32, average: &dyn Fn(RECT) -> u32) -> Reply {
        if let Some(button) = self.button_at(x, y) {
            return self.press(button);
        }
        let typed = self.commit();
        if !contains(&self.area, x, y) {
            return if typed { Reply::Redraw(None) } else { Reply::Nothing };
        }
        let (color, w, p) = (COLORS[self.color], self.stroke(), (x, y));
        self.drawing = match self.tool {
            Pick::Text => {
                self.typing = Some(((x, y - self.text_size() * 0.65), String::new()));
                return Reply::Redraw(None);
            }
            Pick::Pen => Some(Mark::Pen { pts: vec![p], color, w }),
            Pick::Arrow => Some(Mark::Arrow { a: p, b: p, color, w }),
            Pick::Rect => Some(Mark::Rect { a: p, b: p, color, w }),
            Pick::Blur => Some(Mark::Blur { a: p, b: p, blocks: self.blocks(p, p, average) }),
        };
        let bounds = self.drawing.as_ref().map(|m| m.bounds(self.k));
        if typed { Reply::Redraw(None) } else { Reply::Redraw(bounds) }
    }

    pub fn drag(&mut self, x: f32, y: f32, average: &dyn Fn(RECT) -> u32) -> Reply {
        let p = self.clamp(x, y);
        let Some(mark) = self.drawing.take() else { return Reply::Nothing };
        let before = mark.bounds(self.k);
        let mark = match mark {
            Mark::Pen { mut pts, color, w } => {
                if pts.last().is_none_or(|last| (last.0 - p.0).hypot(last.1 - p.1) >= 1.0) {
                    pts.push(p);
                }
                Mark::Pen { pts, color, w }
            }
            Mark::Arrow { a, color, w, .. } => Mark::Arrow { a, b: p, color, w },
            Mark::Rect { a, color, w, .. } => Mark::Rect { a, b: p, color, w },
            Mark::Blur { a, .. } => Mark::Blur { a, b: p, blocks: self.blocks(a, p, average) },
            other => other,
        };
        let after = mark.bounds(self.k);
        self.drawing = Some(mark);
        Reply::Redraw(Some(union(before, after)))
    }

    pub fn up(&mut self, x: f32, y: f32, average: &dyn Fn(RECT) -> u32) -> Reply {
        if self.drawing.is_none() {
            return Reply::Nothing;
        }
        let reply = self.drag(x, y, average);
        let Some(mark) = self.drawing.take() else { return reply };
        // A click without a drag leaves nothing (except a pen dot).
        let big = match &mark {
            Mark::Arrow { a, b, .. } => (a.0 - b.0).hypot(a.1 - b.1) >= 4.0,
            Mark::Rect { a, b, .. } | Mark::Blur { a, b, .. } => (a.0 - b.0).abs() >= 3.0 && (a.1 - b.1).abs() >= 3.0,
            _ => true,
        };
        if big {
            self.marks.push(mark);
        }
        reply
    }

    /// The pointer moved without a button: hover highlight on the toolbar.
    pub fn hover(&mut self, x: f32, y: f32) -> Reply {
        let over = self.button_at(x, y);
        if over == self.hover {
            return Reply::Nothing;
        }
        self.hover = over;
        Reply::Redraw(Some(self.toolbar().0))
    }

    pub fn char(&mut self, c: char) -> Reply {
        if c.is_control() {
            return Reply::Nothing;
        }
        if let Some((_, text)) = &mut self.typing {
            if text.chars().count() < 200 {
                text.push(c);
            }
            return Reply::Redraw(None);
        }
        // Without text being typed, letters pick tools and digits colours.
        match c.to_ascii_lowercase() {
            'p' => self.press(Button::Tool(Pick::Pen)),
            'a' => self.press(Button::Tool(Pick::Arrow)),
            'r' => self.press(Button::Tool(Pick::Rect)),
            't' => self.press(Button::Tool(Pick::Text)),
            'b' => self.press(Button::Tool(Pick::Blur)),
            d @ '1'..='5' => self.press(Button::Color(d as usize - '1' as usize)),
            _ => Reply::Nothing,
        }
    }

    /// Esc closes (or drops the text being typed), Enter saves (or finishes the
    /// text), Backspace deletes, Ctrl+Z undoes, Ctrl+C copies, Ctrl+S saves.
    pub fn key(&mut self, key: u32, ctrl: bool) -> Reply {
        match (key, ctrl) {
            (0x1B, _) if self.typing.is_some() => {
                self.typing = None;
                Reply::Redraw(None)
            }
            (0x1B, _) => Reply::Close,
            (0x0D, _) if self.typing.is_some() => {
                self.commit();
                Reply::Redraw(None)
            }
            (0x0D, _) => Reply::Save,
            (0x08, _) => match &mut self.typing {
                Some((_, text)) => {
                    text.pop();
                    Reply::Redraw(None)
                }
                None => Reply::Nothing,
            },
            (0x5A, true) => self.undo(),
            (0x43, true) => self.press(Button::Copy),
            (0x53, true) => self.press(Button::Save),
            _ => Reply::Nothing,
        }
    }
}

// ------------------------------------------------------------------- painter --

fn colour(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((rgb >> 16) & 255) as f32 / 255.0, g: ((rgb >> 8) & 255) as f32 / 255.0, b: (rgb & 255) as f32 / 255.0, a }
}

fn v(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn rf(r: &RECT) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 }
}

pub struct Painter {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    text: IDWriteTextFormat,
    label: IDWriteTextFormat,
    k: f32,
}

impl Painter {
    pub fn new(k: f32) -> Result<Painter> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            // Software: no Direct3D device (the GPU one costs ~50 MB), 96 dpi so units are pixels.
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            let brush = rt.CreateSolidColorBrush(&colour(0, 1.0), None)?;
            let round = factory.CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 4.0, ..Default::default() }, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = dwrite.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 26.0 * k, w!("en-us"))?;
            text.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let label = dwrite.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 13.0 * k, w!("en-us"))?;
            label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            label.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            Ok(Painter { factory, dwrite, rt, brush, round, text, label, k })
        }
    }

    fn paint_with(&self, rgb: u32, a: f32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&colour(rgb, a)) };
        &self.brush
    }

    fn line(&self, a: Point, b: Point, w: f32, rgb: u32, alpha: f32) {
        unsafe { self.rt.DrawLine(v(a.0, a.1), v(b.0, b.1), self.paint_with(rgb, alpha), w, &self.round) };
    }

    fn polygon(&self, pts: &[Point], fill: bool, w: f32, rgb: u32) {
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geometry.Open() else { return };
            sink.BeginFigure(v(pts[0].0, pts[0].1), if fill { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            for p in &pts[1..] {
                sink.AddLine(v(p.0, p.1));
            }
            sink.EndFigure(if fill { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            if sink.Close().is_err() {
                return;
            }
            if fill {
                self.rt.FillGeometry(&geometry, self.paint_with(rgb, 1.0), None);
            } else {
                self.rt.DrawGeometry(&geometry, self.paint_with(rgb, 1.0), w, &self.round);
            }
        }
    }

    fn text_width(&self, text: &[u16]) -> f32 {
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            match self.dwrite.CreateTextLayout(text, &self.text, 8000.0, 1000.0) {
                Ok(layout) if layout.GetMetrics(&mut metrics).is_ok() => metrics.widthIncludingTrailingWhitespace,
                _ => 0.0,
            }
        }
    }

    fn draw_text(&self, at: Point, text: &str, rgb: u32) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let k = self.k;
        unsafe {
            // A dark rim keeps light text readable on light backgrounds (and vice versa).
            let rim = if rgb == 0xffffff || rgb == 0xffd60a { 0x000000 } else { 0xffffff };
            for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
                let box_ = D2D_RECT_F { left: at.0 + dx * k, top: at.1 + dy * k, right: at.0 + 8000.0, bottom: at.1 + 1000.0 };
                self.rt.DrawText(&units, &self.text, &box_, self.paint_with(rim, 0.55), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            let box_ = D2D_RECT_F { left: at.0, top: at.1, right: at.0 + 8000.0, bottom: at.1 + 1000.0 };
            self.rt.DrawText(&units, &self.text, &box_, self.paint_with(rgb, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    fn mark(&self, mark: &Mark) {
        unsafe {
            match mark {
                Mark::Pen { pts, color, w } if pts.len() == 1 => {
                    self.rt.FillEllipse(&D2D1_ELLIPSE { point: v(pts[0].0, pts[0].1), radiusX: w * 0.5, radiusY: w * 0.5 }, self.paint_with(*color, 1.0));
                }
                Mark::Pen { pts, color, w } => self.polygon(pts, false, *w, *color),
                Mark::Arrow { a, b, color, w } => {
                    let length = (b.0 - a.0).hypot(b.1 - a.1);
                    if length < 1.0 {
                        return;
                    }
                    let head = arrow_head(*w, self.k).min(length * 0.6);
                    let (dx, dy) = ((b.0 - a.0) / length, (b.1 - a.1) / length);
                    let base = (b.0 - dx * head, b.1 - dy * head);
                    self.line(*a, (base.0 + dx * 1.5, base.1 + dy * 1.5), *w, *color, 1.0);
                    let half = head * 0.55;
                    self.polygon(&[*b, (base.0 - dy * half, base.1 + dx * half), (base.0 + dy * half, base.1 - dx * half)], true, 0.0, *color);
                }
                Mark::Rect { a, b, color, w } => {
                    let (l, t, r, bt) = span(*a, *b);
                    self.rt.DrawRectangle(&D2D_RECT_F { left: l, top: t, right: r, bottom: bt }, self.paint_with(*color, 1.0), *w, &self.round);
                }
                Mark::Text { at, text, color } => self.draw_text(*at, text, *color),
                Mark::Blur { blocks, .. } => {
                    for (block, rgb) in blocks {
                        self.rt.FillRectangle(&rf(block), self.paint_with(*rgb, 1.0));
                    }
                }
            }
        }
    }

    /// Draws the marks onto `dc` (`size` pixels), limited to `clip`, shifted by
    /// `offset`. With `ui`, also the text caret and the toolbar.
    pub fn paint(&self, dc: HDC, size: (i32, i32), clip: RECT, editor: &Editor, offset: Point, ui: bool) -> Result<()> {
        unsafe {
            self.rt.BindDC(dc, &RECT { left: 0, top: 0, right: size.0, bottom: size.1 })?;
            self.rt.BeginDraw();
            self.rt.SetTransform(&Matrix3x2::identity());
            self.rt.PushAxisAlignedClip(&rf(&clip), D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: offset.0, M32: offset.1 });
            for mark in editor.marks.iter().chain(editor.drawing.as_ref()) {
                self.mark(mark);
            }
            if let Some((at, text)) = &editor.typing {
                let rgb = COLORS[editor.color];
                self.draw_text(*at, text, rgb);
                if ui {
                    let x = at.0 + self.text_width(&text.encode_utf16().collect::<Vec<_>>()) + 2.0 * self.k;
                    self.line((x, at.1 + 4.0 * self.k), (x, at.1 + editor.text_size() * 1.3), 2.0 * self.k, rgb, 1.0);
                }
            }
            if ui {
                self.toolbar(editor);
            }
            self.rt.SetTransform(&Matrix3x2::identity());
            self.rt.PopAxisAlignedClip();
            self.rt.EndDraw(None, None)
        }
    }

    fn toolbar(&self, editor: &Editor) {
        let k = self.k;
        let (bar, buttons) = editor.toolbar();
        unsafe {
            let rounded = |r: &RECT, radius: f32| D2D1_ROUNDED_RECT { rect: rf(r), radiusX: radius, radiusY: radius };
            self.rt.FillRoundedRectangle(&rounded(&bar, 12.0 * k), self.paint_with(0x1c1c1e, 0.97));
            self.rt.DrawRoundedRectangle(&rounded(&bar, 12.0 * k), self.paint_with(0x3a3a3c, 1.0), 1.0, &self.round);
            for (button, r) in buttons {
                let (cx, cy) = ((r.left + r.right) as f32 * 0.5, (r.top + r.bottom) as f32 * 0.5);
                let active = button == Button::Tool(editor.tool);
                let hover = editor.hover == Some(button);
                let ink = if active { 0x0b0b12 } else { 0xffffff };
                let fill = match button {
                    _ if active => Some(ACCENT),
                    Button::Save => Some(ACCENT),
                    Button::Copy => Some(if hover { 0x48484a } else { 0x2c2c2e }),
                    _ if hover => Some(0x3a3a3c),
                    _ => None,
                };
                if let (Some(fill), false) = (fill, matches!(button, Button::Color(_))) {
                    self.rt.FillRoundedRectangle(&rounded(&r, 8.0 * k), self.paint_with(fill, if button == Button::Save && hover { 0.85 } else { 1.0 }));
                }
                let s = 1.8 * k;
                match button {
                    Button::Tool(Pick::Pen) => {
                        self.line((cx - 7.0 * k, cy + 7.0 * k), (cx + 6.0 * k, cy - 6.0 * k), 3.2 * k, ink, 1.0);
                        self.line((cx - 9.0 * k, cy + 9.0 * k), (cx - 8.0 * k, cy + 8.0 * k), 1.6 * k, ink, 1.0);
                    }
                    Button::Tool(Pick::Arrow) => {
                        self.line((cx - 7.0 * k, cy + 7.0 * k), (cx + 7.0 * k, cy - 7.0 * k), s, ink, 1.0);
                        self.line((cx + 7.0 * k, cy - 7.0 * k), (cx - 1.0 * k, cy - 7.0 * k), s, ink, 1.0);
                        self.line((cx + 7.0 * k, cy - 7.0 * k), (cx + 7.0 * k, cy + 1.0 * k), s, ink, 1.0);
                    }
                    Button::Tool(Pick::Rect) => {
                        self.rt.DrawRectangle(&D2D_RECT_F { left: cx - 8.0 * k, top: cy - 6.0 * k, right: cx + 8.0 * k, bottom: cy + 6.0 * k }, self.paint_with(ink, 1.0), s, &self.round);
                    }
                    Button::Tool(Pick::Text) => {
                        self.line((cx - 7.0 * k, cy - 7.0 * k), (cx + 7.0 * k, cy - 7.0 * k), s * 1.2, ink, 1.0);
                        self.line((cx, cy - 7.0 * k), (cx, cy + 8.0 * k), s * 1.2, ink, 1.0);
                    }
                    Button::Tool(Pick::Blur) => {
                        for i in 0..3 {
                            for j in 0..3 {
                                let a = if (i + j) % 2 == 0 { 1.0 } else { 0.45 };
                                let (x, y) = (cx - 9.0 * k + i as f32 * 6.0 * k, cy - 9.0 * k + j as f32 * 6.0 * k);
                                self.rt.FillRectangle(&D2D_RECT_F { left: x, top: y, right: x + 5.0 * k, bottom: y + 5.0 * k }, self.paint_with(ink, a));
                            }
                        }
                    }
                    Button::Color(i) => {
                        if editor.color == i {
                            self.rt.DrawEllipse(&D2D1_ELLIPSE { point: v(cx, cy), radiusX: 11.0 * k, radiusY: 11.0 * k }, self.paint_with(0xffffff, 1.0), 1.6 * k, &self.round);
                        }
                        self.rt.FillEllipse(&D2D1_ELLIPSE { point: v(cx, cy), radiusX: 8.0 * k, radiusY: 8.0 * k }, self.paint_with(COLORS[i], 1.0));
                    }
                    Button::Undo => {
                        // An arc from the right round to the left, with an arrow head.
                        let pts: Vec<Point> = (0..=8).map(|i| {
                            let angle = std::f32::consts::PI * (1.15 - i as f32 * 0.16);
                            (cx + 7.0 * k * angle.cos(), cy + 2.0 * k - 7.0 * k * angle.sin())
                        }).collect();
                        self.polygon(&pts, false, s, ink);
                        let tip = pts[0];
                        self.line(tip, (tip.0 + 5.0 * k, tip.1 - 1.0 * k), s, ink, 1.0);
                        self.line(tip, (tip.0 + 1.0 * k, tip.1 - 5.5 * k), s, ink, 1.0);
                    }
                    Button::Copy | Button::Save => {
                        let label: Vec<u16> = if button == Button::Copy { "Copy" } else { "Save" }.encode_utf16().collect();
                        let rgb = if button == Button::Save { 0x0b0b12 } else { 0xffffff };
                        self.rt.DrawText(&label, &self.label, &rf(&r), self.paint_with(rgb, 1.0), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                    Button::Close => {
                        self.line((cx - 6.0 * k, cy - 6.0 * k), (cx + 6.0 * k, cy + 6.0 * k), s, ink, 1.0);
                        self.line((cx + 6.0 * k, cy - 6.0 * k), (cx - 6.0 * k, cy + 6.0 * k), s, ink, 1.0);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new(RECT { left: 200, top: 200, right: 1000, bottom: 600 }, (1280, 720), 1.0)
    }

    const FLAT: &dyn Fn(RECT) -> u32 = &|_| 0x808080;

    #[test]
    fn drawing_marks_and_undo() {
        let mut e = editor();
        // Arrow (the default tool): drag, release; a click alone leaves nothing.
        e.down(300.0, 300.0, FLAT);
        e.drag(400.0, 350.0, FLAT);
        e.up(420.0, 360.0, FLAT);
        e.down(500.0, 500.0, FLAT);
        e.up(500.0, 500.0, FLAT);
        assert_eq!(e.marks.len(), 1);
        assert!(matches!(e.marks[0], Mark::Arrow { b: (420.0, 360.0), color: 0xff453a, .. }));
        // Points outside the area are kept inside it.
        e.char('r');
        e.char('4');
        e.down(900.0, 500.0, FLAT);
        e.up(5000.0, 5000.0, FLAT);
        assert!(matches!(e.marks[1], Mark::Rect { b: (1000.0, 600.0), color: 0x0a84ff, .. }));
        // Blur: blocks of 12 px cover the rectangle.
        e.char('b');
        e.down(300.0, 300.0, FLAT);
        e.up(330.0, 324.0, FLAT);
        let Mark::Blur { blocks, .. } = &e.marks[2] else { panic!() };
        assert_eq!(blocks.len(), 3 * 2);
        assert_eq!(blocks[2].0, RECT { left: 324, top: 300, right: 330, bottom: 312 });
        assert_eq!(e.key(0x5A, true), Reply::Redraw(None));
        assert_eq!(e.marks.len(), 2);
    }

    #[test]
    fn typing_text() {
        let mut e = editor();
        e.char('t');
        e.down(400.0, 400.0, FLAT);
        for c in "Hi!".chars() {
            e.char(c);
        }
        // While typing, letters are text, not tool shortcuts; Backspace deletes.
        e.char('p');
        e.key(0x08, false);
        assert_eq!(e.tool, Pick::Text);
        // Enter finishes the text; the next Enter saves.
        assert_eq!(e.key(0x0D, false), Reply::Redraw(None));
        assert!(matches!(&e.marks[0], Mark::Text { text, .. } if text == "Hi!"));
        assert_eq!(e.key(0x0D, false), Reply::Save);
        // Esc drops unfinished text first, then closes.
        e.down(500.0, 450.0, FLAT);
        e.char('x');
        assert_eq!(e.key(0x1B, false), Reply::Redraw(None));
        assert_eq!(e.key(0x1B, false), Reply::Close);
        assert_eq!(e.marks.len(), 1);
    }

    #[test]
    fn toolbar_sits_by_the_area() {
        let e = editor();
        let (bar, buttons) = e.toolbar();
        assert_eq!(bar.top, 610);
        assert!(bar.left >= 8 && bar.right <= 1272);
        assert_eq!(buttons.len(), 5 + 5 + 4);
        // Under a full-screen area there is no room: it goes inside, at the bottom.
        let full = Editor::new(RECT { left: 0, top: 0, right: 1280, bottom: 720 }, (1280, 720), 1.0);
        let (bar, _) = full.toolbar();
        assert!(bar.bottom < 720 && bar.top > 600);
        // Its buttons answer clicks.
        let mut e = editor();
        let save = buttons.iter().find(|(b, _)| *b == Button::Save).unwrap().1;
        assert_eq!(e.down((save.left + 5) as f32, (save.top + 5) as f32, FLAT), Reply::Save);
        let blur = buttons.iter().find(|(b, _)| *b == Button::Tool(Pick::Blur)).unwrap().1;
        e.down((blur.left + 5) as f32, (blur.top + 5) as f32, FLAT);
        assert_eq!(e.tool, Pick::Blur);
    }
}
