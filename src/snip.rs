//! Screen tools behind the Home tab's quick actions: area and full-screen
//! screenshots, screen text (OCR), QR scan and the colour picker.
//!
//! A tool freezes the monitor under the cursor into a bitmap. Area, Screen text
//! and Color picker then show that frozen picture in a full-screen selection
//! window of their own (plain GDI: two bitmaps, no Direct2D); Full screen and
//! QR code work on the picture directly. Encoding, OCR and QR decoding run on a
//! worker thread, and the result comes back with `WM_APP_SNIP`. Nothing here is
//! loaded or kept between uses.
//!
//! Stills use GDI (`BitBlt`) rather than Windows.Graphics.Capture: it needs no
//! Direct3D device (the GPU one costs ~50 MB) and shows no capture border. The
//! island keeps itself out of the picture with `WDA_EXCLUDEFROMCAPTURE`.

use crate::image::{self, Pixels};
use crate::markup;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the island window when [`take`] has results.
pub const WM_APP_SNIP: u32 = WM_APP + 17;
/// Sent to the selection window by tests: paint into `snip<N>.bmp`.
const WM_SNIP_DUMP: u32 = WM_APP + 10;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Area,
    Full,
    Text,
    Qr,
    Color,
}

pub enum Outcome {
    /// A screenshot for the clipboard; with `save`, also written as PNG (`path` when that worked).
    Shot { path: Option<PathBuf>, pixels: Pixels, save: bool },
    /// Recognised text (may be empty).
    Text(String),
    /// Contents of the QR codes found, one per line (empty when there is none).
    Qr(String),
    /// `#RRGGBB`.
    Color(String),
    Failed(&'static str),
}

static OUTCOMES: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());

pub fn take() -> Vec<Outcome> {
    OUTCOMES.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

fn report(target: isize, outcome: Outcome) {
    if let Ok(mut list) = OUTCOMES.lock() {
        list.push(outcome);
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_SNIP, WPARAM(0), LPARAM(0));
    }
}

// ------------------------------------------------------------------ bitmaps --

/// A top-down 32-bit bitmap selected into its own memory DC.
struct Shot {
    w: i32,
    h: i32,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

impl Shot {
    fn blank(w: i32, h: i32) -> Option<Shot> {
        unsafe {
            let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bitmap.into());
            Some(Shot { w, h, dc, bitmap, old, bits: bits as *mut u8 })
        }
    }

    /// What is on screen inside `area` (virtual-screen pixels).
    fn of_screen(area: RECT) -> Option<Shot> {
        let shot = Shot::blank(area.right - area.left, area.bottom - area.top)?;
        unsafe {
            let screen = GetDC(None);
            let done = BitBlt(shot.dc, 0, 0, shot.w, shot.h, Some(screen), area.left, area.top, SRCCOPY | CAPTUREBLT);
            ReleaseDC(None, screen);
            done.ok()?;
        }
        Some(shot)
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }

    /// The same picture at 45 % brightness (what lies outside the selection).
    fn dimmed(&self) -> Option<Shot> {
        let mut dim = Shot::blank(self.w, self.h)?;
        for (to, from) in dim.bytes_mut().iter_mut().zip(self.bytes()) {
            *to = ((*from as u32 * 115) >> 8) as u8;
        }
        Some(dim)
    }

    /// Opaque pixels of `area` (clamped to the bitmap).
    fn pixels(&self, area: RECT) -> Pixels {
        let (x0, y0) = (area.left.clamp(0, self.w), area.top.clamp(0, self.h));
        let (x1, y1) = (area.right.clamp(x0, self.w), area.bottom.clamp(y0, self.h));
        let mut bgra = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 4) as usize);
        for y in y0..y1 {
            let row = ((y * self.w + x0) * 4) as usize;
            bgra.extend_from_slice(&self.bytes()[row..row + ((x1 - x0) * 4) as usize]);
        }
        for px in bgra.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Pixels { w: (x1 - x0) as u32, h: (y1 - y0) as u32, bgra }
    }

    /// `#RRGGBB` of one pixel.
    fn hex_at(&self, x: i32, y: i32) -> String {
        let i = ((y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) * 4) as usize;
        let px = &self.bytes()[i..i + 3];
        format!("#{:02X}{:02X}{:02X}", px[2], px[1], px[0])
    }
}

impl Drop for Shot {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The monitor under the cursor, in virtual-screen pixels.
fn monitor_area() -> RECT {
    unsafe {
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTOPRIMARY), &mut info);
        info.rcMonitor
    }
}

/// Freezes the monitor under the cursor without the island in the picture.
fn capture(island: HWND) -> Option<(Shot, RECT)> {
    unsafe {
        let _ = SetWindowDisplayAffinity(island, WDA_EXCLUDEFROMCAPTURE);
        let _ = DwmFlush();
        let area = monitor_area();
        let shot = Shot::of_screen(area);
        let _ = SetWindowDisplayAffinity(island, WDA_NONE);
        shot.map(|shot| (shot, area))
    }
}

/// A stand-in for the screen in tests: stripes and a line of text, so nothing
/// of the user's real screen is read while a test runs.
fn sample() -> Option<Shot> {
    let shot = Shot::blank(1280, 720)?;
    unsafe {
        for (i, colour) in [0x00ffb37a_u32, 0x007ad1ff, 0x0090e0a0, 0x00c8a0f0].into_iter().enumerate() {
            let brush = CreateSolidBrush(COLORREF(colour));
            FillRect(shot.dc, &RECT { left: i as i32 * 320, top: 0, right: (i as i32 + 1) * 320, bottom: 720 }, brush);
            let _ = DeleteObject(brush.into());
        }
        let white = CreateSolidBrush(COLORREF(0x00ffffff));
        let mut card = RECT { left: 240, top: 260, right: 1040, bottom: 460 };
        FillRect(shot.dc, &card, white);
        let _ = DeleteObject(white.into());
        let font = CreateFontW(64, 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let old = SelectObject(shot.dc, font.into());
        SetBkMode(shot.dc, TRANSPARENT);
        SetTextColor(shot.dc, COLORREF(0));
        let mut text: Vec<u16> = "Islet screen text 2026".encode_utf16().collect();
        DrawTextW(shot.dc, &mut text, &mut card, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(shot.dc, old);
        let _ = DeleteObject(font.into());
    }
    Some(shot)
}

// ---------------------------------------------------------------- selection --

const ZOOM: i32 = 9;
const CELLS: i32 = 13;
const LENS: i32 = ZOOM * CELLS;
const LENS_LABEL: i32 = 26;

struct Overlay {
    tool: Tool,
    target: isize,
    dir: PathBuf,
    hidden: bool,
    bright: Shot,
    /// Only for the tools that select an area.
    dim: Option<Shot>,
    anchor: Option<(i32, i32)>,
    at: (i32, i32),
    /// After an Area selection (or at once for Full screen): the markup editor,
    /// its painter, and the back buffer it is drawn in (marks overlap the picture).
    edit: Option<markup::Editor>,
    painter: Option<markup::Painter>,
    canvas: Option<Shot>,
}

/// Average colour (RGB) of a rectangle of the picture, every second pixel.
fn average(shot: &Shot, r: RECT) -> u32 {
    let (x0, y0) = (r.left.clamp(0, shot.w), r.top.clamp(0, shot.h));
    let (x1, y1) = (r.right.clamp(x0, shot.w), r.bottom.clamp(y0, shot.h));
    let (mut sum, mut count) = ([0u64; 3], 0u64);
    let bytes = shot.bytes();
    for y in (y0..y1).step_by(2) {
        for x in (x0..x1).step_by(2) {
            let i = ((y * shot.w + x) * 4) as usize;
            for c in 0..3 {
                sum[c] += bytes[i + c] as u64;
            }
            count += 1;
        }
    }
    let count = count.max(1);
    ((sum[2] / count) as u32) << 16 | ((sum[1] / count) as u32) << 8 | (sum[0] / count) as u32
}

fn intersect(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) }
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// The rectangle between two corners, in any order.
fn between(a: (i32, i32), b: (i32, i32)) -> RECT {
    RECT { left: a.0.min(b.0), top: a.1.min(b.1), right: a.0.max(b.0), bottom: a.1.max(b.1) }
}

/// Where the magnifier sits for a cursor at `at`: below right, flipped at the edges.
fn lens_rect(at: (i32, i32), w: i32, h: i32) -> RECT {
    let (lw, lh) = (LENS + 2, LENS + LENS_LABEL + 2);
    let left = if at.0 + 24 + lw > w { at.0 - 24 - lw } else { at.0 + 24 };
    let top = if at.1 + 24 + lh > h { at.1 - 24 - lh } else { at.1 + 24 };
    RECT { left, top, right: left + lw, bottom: top + lh }
}

fn inflate(r: RECT, by: i32) -> RECT {
    RECT { left: r.left - by, top: r.top - by, right: r.right + by, bottom: r.bottom + by }
}

impl Overlay {
    fn selection(&self) -> Option<RECT> {
        self.anchor.map(|anchor| between(anchor, self.at))
    }

    /// The part of the window that depends on the pointer (to invalidate on a move).
    fn live_rect(&self) -> RECT {
        match self.tool {
            Tool::Color => lens_rect(self.at, self.bright.w, self.bright.h),
            _ => inflate(self.selection().unwrap_or_default(), 2),
        }
    }

    /// The editor's picture into `dc` (window-sized), limited to `clip`: dimmed
    /// screen, the bright area, the marks and the toolbar.
    unsafe fn compose(&self, dc: HDC, clip: RECT) {
        let (Some(editor), Some(painter)) = (&self.edit, &self.painter) else { return };
        unsafe {
            let copy = |from: &Shot, r: RECT| {
                if r.right > r.left && r.bottom > r.top {
                    let _ = BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(from.dc), r.left, r.top, SRCCOPY);
                }
            };
            if let Some(dim) = &self.dim {
                copy(dim, clip);
            }
            copy(&self.bright, intersect(clip, editor.area));
            FrameRect(dc, &inflate(editor.area, 1), HBRUSH(GetStockObject(WHITE_BRUSH).0));
            let _ = painter.paint(dc, (self.bright.w, self.bright.h), clip, editor, (0.0, 0.0), true);
        }
    }

    /// Paints without drawing any pixel twice (no flicker, no back buffer):
    /// dimmed bands around the selection, the bright picture inside it.
    unsafe fn paint(&self, dc: HDC) {
        unsafe {
            let (w, h) = (self.bright.w, self.bright.h);
            let copy = |from: &Shot, r: RECT| {
                if r.right > r.left && r.bottom > r.top {
                    let _ = BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(from.dc), r.left, r.top, SRCCOPY);
                }
            };
            if self.tool == Tool::Color {
                let lens = lens_rect(self.at, w, h);
                let saved = SaveDC(dc);
                ExcludeClipRect(dc, lens.left, lens.top, lens.right, lens.bottom);
                copy(&self.bright, RECT { left: 0, top: 0, right: w, bottom: h });
                let _ = RestoreDC(dc, saved);
                if let Some(glass) = self.lens() {
                    let _ = BitBlt(dc, lens.left, lens.top, glass.w, glass.h, Some(glass.dc), 0, 0, SRCCOPY);
                }
                return;
            }
            let dim = self.dim.as_ref().unwrap_or(&self.bright);
            let Some(sel) = self.selection() else {
                copy(dim, RECT { left: 0, top: 0, right: w, bottom: h });
                return;
            };
            copy(dim, RECT { left: 0, top: 0, right: w, bottom: sel.top });
            copy(dim, RECT { left: 0, top: sel.bottom, right: w, bottom: h });
            copy(dim, RECT { left: 0, top: sel.top, right: sel.left, bottom: sel.bottom });
            copy(dim, RECT { left: sel.right, top: sel.top, right: w, bottom: sel.bottom });
            copy(&self.bright, sel);
            FrameRect(dc, &inflate(sel, 1), HBRUSH(GetStockObject(WHITE_BRUSH).0));
        }
    }

    /// The magnifier: the pixels around the cursor enlarged, the middle one
    /// outlined, and its colour as text underneath.
    unsafe fn lens(&self) -> Option<Shot> {
        unsafe {
            let glass = Shot::blank(LENS + 2, LENS + LENS_LABEL + 2)?;
            let frame = RECT { left: 0, top: 0, right: glass.w, bottom: glass.h };
            FillRect(glass.dc, &frame, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            SetStretchBltMode(glass.dc, COLORONCOLOR);
            let _ = StretchBlt(glass.dc, 1, 1, LENS, LENS, Some(self.bright.dc), self.at.0 - CELLS / 2, self.at.1 - CELLS / 2, CELLS, CELLS, SRCCOPY);
            let middle = 1 + (CELLS / 2) * ZOOM;
            // White inside black: visible on any colour. The outer frame stays black.
            FrameRect(glass.dc, &RECT { left: middle - 1, top: middle - 1, right: middle + ZOOM + 1, bottom: middle + ZOOM + 1 }, HBRUSH(GetStockObject(WHITE_BRUSH).0));
            FrameRect(glass.dc, &RECT { left: middle - 2, top: middle - 2, right: middle + ZOOM + 2, bottom: middle + ZOOM + 2 }, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            let font = CreateFontW(18, 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
            let old = SelectObject(glass.dc, font.into());
            SetBkMode(glass.dc, TRANSPARENT);
            SetTextColor(glass.dc, COLORREF(0x00ffffff));
            let mut text: Vec<u16> = self.bright.hex_at(self.at.0, self.at.1).encode_utf16().collect();
            DrawTextW(glass.dc, &mut text, &mut RECT { left: 1, top: LENS + 1, right: LENS + 1, bottom: LENS + LENS_LABEL + 1 }, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(glass.dc, old);
            let _ = DeleteObject(font.into());
            Some(glass)
        }
    }
}

fn point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32)
}

/// Closes the selection window; with `done`, hands the result on.
unsafe fn finish(hwnd: HWND, done: bool) {
    let overlay = OVERLAY.with(|cell| cell.borrow_mut().take());
    unsafe {
        let _ = ReleaseCapture();
        let _ = DestroyWindow(hwnd);
    }
    let Some(overlay) = overlay else { return };
    if !done {
        return;
    }
    match overlay.tool {
        Tool::Color => report(overlay.target, Outcome::Color(overlay.bright.hex_at(overlay.at.0, overlay.at.1))),
        tool => {
            let area = overlay.selection().unwrap_or_default();
            if area.right - area.left >= 4 && area.bottom - area.top >= 4 {
                work(overlay.target, tool, overlay.bright.pixels(area), overlay.dir, overlay.hidden, true);
            }
        }
    }
}

/// Turns the window into the markup editor for `area`. `false` when Direct2D
/// is not available (the caller then saves the plain picture).
unsafe fn enter_edit(hwnd: HWND, area: RECT) -> bool {
    let k = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let ready = OVERLAY.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(overlay) = cell.as_mut() else { return false };
        let Ok(painter) = markup::Painter::new(k) else { return false };
        overlay.edit = Some(markup::Editor::new(area, (overlay.bright.w, overlay.bright.h), k));
        overlay.painter = Some(painter);
        overlay.canvas = Shot::blank(overlay.bright.w, overlay.bright.h);
        overlay.canvas.is_some()
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    ready
}

/// Closes the editor: with `keep` = Some(save), the picture with its marks
/// goes to the clipboard (and with `save` to a PNG file too).
unsafe fn finish_edit(hwnd: HWND, keep: Option<bool>) {
    let overlay = OVERLAY.with(|cell| cell.borrow_mut().take());
    unsafe {
        let _ = ReleaseCapture();
        let _ = DestroyWindow(hwnd);
    }
    let (Some(overlay), Some(save)) = (overlay, keep) else { return };
    let (Some(editor), Some(painter)) = (&overlay.edit, &overlay.painter) else { return };
    let area = editor.area;
    let Some(out) = Shot::blank(area.right - area.left, area.bottom - area.top) else { return };
    unsafe {
        let _ = BitBlt(out.dc, 0, 0, out.w, out.h, Some(overlay.bright.dc), area.left, area.top, SRCCOPY);
    }
    let whole = RECT { left: 0, top: 0, right: out.w, bottom: out.h };
    let _ = painter.paint(out.dc, (out.w, out.h), whole, editor, (-area.left as f32, -area.top as f32), false);
    work(overlay.target, Tool::Area, out.pixels(whole), overlay.dir, overlay.hidden, save);
}

/// The editor's share of the window messages; `None` while there is no editor
/// (the selection and the colour picker handle those themselves).
unsafe fn edit_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    use markup::Reply;
    // Messages Windows sends back while the state is borrowed (e.g. WM_CAPTURECHANGED)
    // get the default handling instead of a second borrow.
    let Ok(editing) = OVERLAY.with(|cell| cell.try_borrow().map(|o| o.as_ref().is_some_and(|o| o.edit.is_some()))) else {
        return Some(unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) });
    };
    if !editing {
        return None;
    }
    unsafe {
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                OVERLAY.with(|cell| {
                    if let Some(overlay) = cell.borrow().as_ref() {
                        if let Some(canvas) = &overlay.canvas {
                            let r = paint.rcPaint;
                            overlay.compose(canvas.dc, r);
                            let _ = BitBlt(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, Some(canvas.dc), r.left, r.top, SRCCOPY);
                        }
                    }
                });
                let _ = EndPaint(hwnd, &paint);
                return Some(LRESULT(0));
            }
            WM_SNIP_DUMP => {
                OVERLAY.with(|cell| {
                    if let Some(overlay) = cell.borrow().as_ref().filter(|o| o.hidden) {
                        if let Some(page) = Shot::blank(overlay.bright.w, overlay.bright.h) {
                            let whole = RECT { left: 0, top: 0, right: page.w, bottom: page.h };
                            overlay.compose(page.dc, whole);
                            let _ = std::fs::write(overlay.dir.join(format!("snip{}.bmp", wparam.0)), image::to_bmp(&page.pixels(whole)));
                        }
                    }
                });
                return Some(LRESULT(0));
            }
            WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
                let mut at = POINT::default();
                let _ = GetCursorPos(&mut at);
                let _ = ScreenToClient(hwnd, &mut at);
                let shape = OVERLAY.with(|cell| match cell.borrow().as_ref().and_then(|o| o.edit.as_ref()) {
                    Some(e) if e.on_toolbar(at.x as f32, at.y as f32) => IDC_ARROW,
                    Some(e) if e.tool == markup::Pick::Text => IDC_IBEAM,
                    _ => IDC_CROSS,
                });
                SetCursor(LoadCursorW(None, shape).ok());
                return Some(LRESULT(1));
            }
            // An editor with work in it stays when another window comes to the front.
            WM_ACTIVATE | WM_RBUTTONDOWN => return Some(LRESULT(0)),
            _ => {}
        }
        let (x, y) = point(lparam);
        let (x, y) = (x as f32, y as f32);
        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
        let reply = OVERLAY.with(|cell| {
            let mut cell = cell.borrow_mut();
            let Some(Overlay { edit: Some(editor), bright, .. }) = cell.as_mut() else { return None };
            let average = |r: RECT| average(bright, r);
            Some(match msg {
                WM_LBUTTONDOWN => editor.down(x, y, &average),
                WM_MOUSEMOVE if wparam.0 & 1 != 0 => editor.drag(x, y, &average),
                WM_MOUSEMOVE => editor.hover(x, y),
                WM_LBUTTONUP => editor.up(x, y, &average),
                WM_KEYDOWN => editor.key(wparam.0 as u32, ctrl),
                WM_CHAR => char::from_u32(wparam.0 as u32).map_or(Reply::Nothing, |c| editor.char(c)),
                _ => return None,
            })
        })?;
        // Capture changes send messages back to this window: only outside the borrow.
        match msg {
            WM_LBUTTONDOWN => {
                SetCapture(hwnd);
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
            }
            _ => {}
        }
        match reply {
            Reply::Nothing => {}
            Reply::Redraw(Some(r)) => {
                let _ = InvalidateRect(Some(hwnd), Some(&r), false);
            }
            Reply::Redraw(None) => {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Reply::Copy => finish_edit(hwnd, Some(false)),
            Reply::Save => finish_edit(hwnd, Some(true)),
            Reply::Close => finish_edit(hwnd, None),
        }
        Some(LRESULT(0))
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if let Some(result) = edit_message(hwnd, msg, wparam, lparam) {
            return result;
        }
        match msg {
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                OVERLAY.with(|cell| {
                    if let Some(overlay) = cell.borrow().as_ref() {
                        overlay.paint(dc);
                    }
                });
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN => {
                OVERLAY.with(|cell| {
                    if let Some(overlay) = cell.borrow_mut().as_mut() {
                        let before = overlay.live_rect();
                        overlay.at = point(lparam);
                        if msg == WM_LBUTTONDOWN && overlay.tool != Tool::Color {
                            overlay.anchor = Some(overlay.at);
                            SetCapture(hwnd);
                        }
                        let _ = InvalidateRect(Some(hwnd), Some(&before), false);
                        let _ = InvalidateRect(Some(hwnd), Some(&overlay.live_rect()), false);
                    }
                });
                if msg == WM_LBUTTONDOWN && OVERLAY.with(|cell| cell.borrow().as_ref().is_some_and(|o| o.tool == Tool::Color)) {
                    finish(hwnd, true);
                }
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let (selecting, markup) = OVERLAY.with(|cell| {
                    let mut cell = cell.borrow_mut();
                    let Some(overlay) = cell.as_mut() else { return (false, None) };
                    overlay.at = point(lparam);
                    // A real Area selection moves on to the markup editor.
                    let area = overlay.selection().filter(|r| overlay.tool == Tool::Area && r.right - r.left >= 4 && r.bottom - r.top >= 4);
                    (overlay.anchor.is_some(), area)
                });
                if let Some(area) = markup {
                    let _ = ReleaseCapture();
                    if !enter_edit(hwnd, area) {
                        finish(hwnd, true);
                    }
                } else if selecting {
                    finish(hwnd, true);
                }
                return LRESULT(0);
            }
            WM_RBUTTONDOWN => {
                finish(hwnd, false);
                return LRESULT(0);
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                finish(hwnd, false);
                return LRESULT(0);
            }
            // Another window came to the front: give up rather than linger on top.
            WM_ACTIVATE if wparam.0 & 0xffff == 0 => {
                if OVERLAY.with(|cell| cell.borrow().as_ref().is_some_and(|o| !o.hidden)) {
                    finish(hwnd, false);
                }
                return LRESULT(0);
            }
            WM_SNIP_DUMP => {
                OVERLAY.with(|cell| {
                    if let Some(overlay) = cell.borrow().as_ref().filter(|o| o.hidden) {
                        if let Some(page) = Shot::blank(overlay.bright.w, overlay.bright.h) {
                            overlay.paint(page.dc);
                            let _ = std::fs::write(overlay.dir.join(format!("snip{}.bmp", wparam.0)), image::to_bmp(&page.pixels(RECT { left: 0, top: 0, right: page.w, bottom: page.h })));
                        }
                    }
                });
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

unsafe fn open(island: HWND, tool: Tool, bright: Shot, area: RECT, dir: PathBuf, hidden: bool) -> Result<()> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW { cbSize: size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: module.into(), hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(), lpszClassName: w!("IsletSnip"), ..Default::default() };
        // Registering twice fails harmlessly.
        RegisterClassExW(&class);
        // Full screen goes straight to the editor, with nothing outside the area to dim.
        let dim = if matches!(tool, Tool::Color | Tool::Full) { None } else { bright.dimmed() };
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let overlay = Overlay { tool, target: island.0 as isize, dir, hidden, bright, dim, anchor: None, at: (at.x - area.left, at.y - area.top), edit: None, painter: None, canvas: None };
        let (w, h) = (overlay.bright.w, overlay.bright.h);
        OVERLAY.with(|cell| *cell.borrow_mut() = Some(overlay));
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, w!("IsletSnip"), PCWSTR::null(), WS_POPUP, area.left, area.top, w, h, None, None, Some(module.into()), None)?;
        if tool == Tool::Full && !enter_edit(hwnd, RECT { left: 0, top: 0, right: w, bottom: h }) {
            // No Direct2D: save the plain picture as before.
            OVERLAY.with(|cell| {
                if let Some(overlay) = cell.borrow_mut().as_mut() {
                    overlay.anchor = Some((0, 0));
                    overlay.at = (w, h);
                    overlay.tool = Tool::Area;
                }
            });
            finish(hwnd, true);
            return Ok(());
        }
        if !hidden {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }
}

/// Starts a tool. `dir` is where screenshots are saved. With `hidden` (tests)
/// a sample picture stands in for the screen and no window is shown.
pub fn start(island: HWND, tool: Tool, dir: PathBuf, hidden: bool) {
    if OVERLAY.with(|cell| cell.borrow().is_some()) {
        return;
    }
    let frozen = if hidden { sample().map(|shot| (shot, RECT { left: 0, top: 0, right: 1280, bottom: 720 })) } else { capture(island) };
    let Some((shot, area)) = frozen else {
        report(island.0 as isize, Outcome::Failed("Could not read the screen"));
        return;
    };
    match tool {
        Tool::Qr => {
            let pixels = shot.pixels(RECT { left: 0, top: 0, right: shot.w, bottom: shot.h });
            drop(shot);
            work(island.0 as isize, tool, pixels, dir, hidden, true);
        }
        _ => unsafe {
            if open(island, tool, shot, area, dir, hidden).is_err() {
                OVERLAY.with(|cell| *cell.borrow_mut() = None);
                report(island.0 as isize, Outcome::Failed("Could not open the selection"));
            }
        },
    }
}

// ------------------------------------------------------------------- worker --

fn work(target: isize, tool: Tool, pixels: Pixels, dir: PathBuf, hidden: bool, keep: bool) {
    let _ = std::thread::Builder::new().name("islet-snip".into()).stack_size(1024 * 1024).spawn(move || {
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        let outcome = match tool {
            Tool::Text => match read_text(&pixels) {
                Ok(text) => Outcome::Text(text),
                Err(_) => Outcome::Failed("No text recognition language is installed"),
            },
            Tool::Qr => Outcome::Qr(read_qr(&pixels).join("\n")),
            _ => Outcome::Shot { path: if keep { save(&pixels, &dir, hidden) } else { None }, pixels, save: keep },
        };
        report(target, outcome);
        if com {
            unsafe { CoUninitialize() };
        }
    });
}

/// "Screenshot 2026-11-14 153012.png" in `dir`.
fn save(pixels: &Pixels, dir: &std::path::Path, hidden: bool) -> Option<PathBuf> {
    let png = image::encode_png(pixels)?;
    std::fs::create_dir_all(dir).ok()?;
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let name = if hidden { "screenshot.png".to_string() } else { format!("Screenshot {:04}-{:02}-{:02} {:02}{:02}{:02}.png", now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond) };
    let path = dir.join(name);
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// Text in the picture, through the OCR engine that ships with Windows
/// (the languages of the user's profile).
fn read_text(pixels: &Pixels) -> Result<String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
    // Small text reads much better enlarged; very large pictures must be reduced.
    let limit = OcrEngine::MaxImageDimension().unwrap_or(2600).min(10_000);
    let scaled;
    let source = if pixels.w > limit || pixels.h > limit {
        scaled = image::shrink(pixels, limit, limit);
        &scaled
    } else if pixels.w * 2 <= limit && pixels.h * 2 <= limit && pixels.h < 1200 {
        scaled = image::double(pixels);
        &scaled
    } else {
        pixels
    };
    let writer = DataWriter::new()?;
    writer.WriteBytes(&source.bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&writer.DetachBuffer()?, BitmapPixelFormat::Bgra8, source.w as i32, source.h as i32)?;
    let result = engine.RecognizeAsync(&bitmap)?.join()?;
    let mut text = String::new();
    for line in result.Lines()? {
        if !text.is_empty() {
            text.push_str("\r\n");
        }
        text.push_str(&line.Text()?.to_string_lossy());
    }
    Ok(text)
}

/// Contents of every QR code in the picture.
fn read_qr(pixels: &Pixels) -> Vec<String> {
    let (w, h) = (pixels.w as usize, pixels.h as usize);
    let luma: Vec<u8> = pixels.bgra.as_chunks::<4>().0.iter().map(|px| ((px[0] as u32 * 29 + px[1] as u32 * 150 + px[2] as u32 * 77) >> 8) as u8).collect();
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| luma[y * w + x]);
    let mut found: Vec<String> = Vec::new();
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode() {
            if !found.contains(&content) {
                found.push(content);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_and_lens_geometry() {
        assert_eq!(between((30, 40), (10, 90)), RECT { left: 10, top: 40, right: 30, bottom: 90 });
        // Below right of the cursor, flipped where it would leave the picture.
        let lens = lens_rect((100, 100), 1280, 720);
        assert_eq!((lens.left, lens.top), (124, 124));
        let lens = lens_rect((1270, 710), 1280, 720);
        assert!(lens.right < 1270 && lens.bottom < 710 && lens.left >= 0 && lens.top >= 0);
    }

    #[test]
    fn crops_and_reads_colours_from_the_sample() {
        let shot = sample().unwrap();
        assert_eq!(shot.hex_at(10, 10), "#7AB3FF");
        assert_eq!(shot.hex_at(400, 10), "#FFD17A");
        // Out of range clamps instead of reading outside the bitmap.
        assert_eq!(shot.hex_at(-5, 9000), "#7AB3FF");
        let crop = shot.pixels(RECT { left: 310, top: 0, right: 330, bottom: 2 });
        assert_eq!((crop.w, crop.h), (20, 2));
        assert_eq!(&crop.bgra[..4], [0xff, 0xb3, 0x7a, 255]);
        assert_eq!(&crop.bgra[19 * 4..20 * 4], [0x7a, 0xd1, 0xff, 255]);
        let outside = shot.pixels(RECT { left: 1270, top: 700, right: 5000, bottom: 5000 });
        assert_eq!((outside.w, outside.h), (10, 20));
        let dim = shot.dimmed().unwrap();
        assert_eq!(dim.hex_at(10, 10), "#365072");
    }

    #[test]
    fn finds_a_qr_code_in_a_picture() {
        let code = qrcode::QrCode::new(b"https://example.com/islet").unwrap();
        let (side, scale, margin) = (code.width(), 6usize, 40usize);
        let size = side * scale + margin * 2;
        let dark = code.to_colors();
        let mut bgra = vec![255u8; size * size * 4];
        for y in 0..side * scale {
            for x in 0..side * scale {
                if dark[(y / scale) * side + x / scale] == qrcode::Color::Dark {
                    let i = ((y + margin) * size + x + margin) * 4;
                    bgra[i..i + 3].fill(0);
                }
            }
        }
        let picture = Pixels { w: size as u32, h: size as u32, bgra };
        assert_eq!(read_qr(&picture), ["https://example.com/islet"]);
        assert!(read_qr(&sample().unwrap().pixels(RECT { left: 0, top: 0, right: 1280, bottom: 720 })).is_empty());
    }

    /// Runs the Windows OCR engine on the sample picture.
    #[test]
    #[ignore = "needs an installed OCR language"]
    fn live_ocr() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let shot = sample().unwrap();
        let text = read_text(&shot.pixels(RECT { left: 240, top: 260, right: 1040, bottom: 460 })).unwrap();
        println!("ocr: {text:?}");
        assert!(text.contains("Islet") && text.contains("2026"));
    }

    /// Captures the real screen and only reports its size and how varied it is.
    #[test]
    #[ignore = "reads the real screen"]
    fn live_capture() {
        let area = monitor_area();
        let shot = Shot::of_screen(area).unwrap();
        let pixels = shot.pixels(RECT { left: 0, top: 0, right: shot.w, bottom: shot.h });
        let mut seen = std::collections::HashSet::new();
        for px in pixels.bgra.as_chunks::<4>().0.iter().step_by(997) {
            seen.insert([px[0], px[1], px[2]]);
        }
        println!("captured {} x {}, {} distinct sampled colours", pixels.w, pixels.h, seen.len());
        assert!(pixels.w >= 640 && seen.len() > 1);
    }
}
