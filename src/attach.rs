//! Chat attachments: pictures, PDF, Word, Excel, PowerPoint and plain text.
//!
//! Everything is read on this PC. Pictures are reduced to at most 1568 px and
//! re-encoded as JPEG; documents become text: Office files are unzipped and
//! their XML read (`miniz_oxide` inflates), PDF pages are rendered with the
//! built-in `Windows.Data.Pdf` and read with the Windows OCR engine. A worker
//! thread does the reading and reports with `WM_APP_ATTACH`.

use crate::chat::{Attachment, Content};
use crate::image::{self, Pixels};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_ATTACH: u32 = WM_APP + 20;

/// Longest side of a picture sent to the model.
const PICTURE_SIDE: u32 = 1568;
/// Text kept per document; longer documents are cut (and the chip says so).
pub const TEXT_LIMIT: usize = 150_000;
const FILE_LIMIT: u64 = 50 << 20;
/// PDF pages read (each is rendered and recognised, about half a second each).
const PDF_PAGES: u32 = 40;
/// Uncompressed size allowed for one entry of an Office file.
const ENTRY_LIMIT: usize = 64 << 20;

pub enum Update {
    Progress(u32, String),
    Done(u32, Result<Attachment, String>),
}

static UPDATES: Mutex<Vec<Update>> = Mutex::new(Vec::new());

pub fn take() -> Vec<Update> {
    UPDATES.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

fn report(target: isize, update: Update) {
    if let Ok(mut list) = UPDATES.lock() {
        list.push(update);
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_ATTACH, WPARAM(0), LPARAM(0));
    }
}

fn worker(target: HWND, id: u32, job: impl FnOnce(&dyn Fn(String)) -> Result<Attachment, String> + Send + 'static) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-attach".into()).stack_size(1024 * 1024).spawn(move || {
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        let progress = move |text: String| report(target, Update::Progress(id, text));
        let result = job(&progress);
        report(target, Update::Done(id, result));
        if com {
            unsafe { CoUninitialize() };
        }
    });
}

/// Reads a file into an attachment on a worker thread.
pub fn read_file(target: HWND, id: u32, path: PathBuf) {
    worker(target, id, move |progress| read(&path, progress));
}

/// Turns a picture (e.g. pasted from the clipboard) into an attachment on a worker thread.
pub fn read_pixels(target: HWND, id: u32, name: String, pixels: Pixels) {
    worker(target, id, move |_| picture(&name, &pixels));
}

// ------------------------------------------------------------------- reading --

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

fn read(path: &Path, progress: &dyn Fn(String)) -> Result<Attachment, String> {
    let name = file_name(path);
    let size = std::fs::metadata(path).map_err(|_| "The file could not be opened".to_string())?.len();
    if size > FILE_LIMIT {
        return Err("Files over 50 MB are not attached".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "The file could not be read".to_string())?;
    let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "ico" => {
            let pixels = image::decode_png(&bytes).ok_or("This picture could not be decoded")?;
            picture(&name, &pixels)
        }
        "pdf" => pdf_via_helper(path, progress).map(|(text, pages)| document(name, text, plural(pages, "page"))),
        "docx" | "docm" => docx(&bytes).map(|(text, pages)| {
            let detail = match pages {
                Some(pages) => plural(pages, "page"),
                None => plural(text.split_whitespace().count() as u32, "word"),
            };
            document(name, text, detail)
        }),
        "xlsx" | "xlsm" => xlsx(&bytes).map(|(text, sheets)| document(name, text, plural(sheets, "sheet"))),
        "pptx" => pptx(&bytes).map(|(text, slides)| document(name, text, plural(slides, "slide"))),
        "doc" | "xls" | "ppt" => Err("Old Office format: save it as .docx, .xlsx or .pptx first".into()),
        _ => match plain_text(&bytes) {
            Some(text) => {
                let detail = size_text(size);
                Ok(document(name, text, detail))
            }
            None => Err("This kind of file cannot be attached".into()),
        },
    }
}

fn plural(count: u32, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

fn size_text(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// A text attachment, cut to [`TEXT_LIMIT`] characters.
fn document(name: String, text: String, detail: String) -> Attachment {
    let text = text.trim().to_string();
    match text.char_indices().nth(TEXT_LIMIT) {
        Some((cut, _)) => Attachment { name, detail: format!("{detail} \u{00B7} shortened"), content: Content::Text(text[..cut].to_string()) },
        None => Attachment { name, detail, content: Content::Text(text) },
    }
}

fn picture(name: &str, pixels: &Pixels) -> Result<Attachment, String> {
    let small = image::shrink(pixels, PICTURE_SIDE, PICTURE_SIDE);
    let jpeg = image::encode_jpeg(&small).ok_or("The picture could not be encoded")?;
    Ok(Attachment { name: name.to_string(), detail: format!("{} \u{00D7} {}", small.w, small.h), content: Content::Image { jpeg, w: small.w, h: small.h } })
}

/// UTF-8 or UTF-16 (with a byte-order mark) text; `None` for binary files.
fn plain_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    // Mostly undecodable: not text after all.
    let bad = text.chars().take(8192).filter(|&c| c == '\u{FFFD}').count();
    (bad * 20 < text.chars().take(8192).count().max(1)).then(|| text.into_owned())
}

// ----------------------------------------------------------------------- PDF --
//
// The Windows PDF engine keeps about 80 MB and some 45 threads alive in any
// process that has used it, for good. So Islet never loads it: a helper copy of
// itself (`islet.exe --read-pdf <file>`) reads the PDF, prints progress lines and
// the text on stdout, and exits, taking all of that with it.

/// Reads a PDF in the helper process. Its stdout: `PROGRESS <step>` lines, then
/// `OK <pages>` followed by the text, or `ERROR <reason>`.
fn pdf_via_helper(path: &Path, progress: &dyn Fn(String)) -> Result<(String, u32), String> {
    use std::io::{BufRead, Read};
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().map_err(|_| "The PDF reader could not be started".to_string())?;
    let null = std::process::Stdio::null;
    let mut child = std::process::Command::new(exe)
        .arg("--read-pdf")
        .arg(path)
        .stdin(null())
        .stdout(std::process::Stdio::piped())
        .stderr(null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "The PDF reader could not be started".to_string())?;
    let mut reader = std::io::BufReader::new(child.stdout.take().ok_or("The PDF reader could not be started")?);
    let mut result = Err("The PDF reader stopped unexpectedly".to_string());
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let text = line.trim_end();
        if let Some(step) = text.strip_prefix("PROGRESS ") {
            progress(step.to_string());
        } else if let Some(why) = text.strip_prefix("ERROR ") {
            result = Err(why.to_string());
            break;
        } else if let Some(pages) = text.strip_prefix("OK ") {
            let pages = pages.parse().unwrap_or(0);
            let mut body = String::new();
            let _ = reader.read_to_string(&mut body);
            result = Ok((body, pages));
            break;
        }
        line.clear();
    }
    let _ = child.wait();
    result
}

/// The helper process's side (`islet.exe --read-pdf <file>`); returns the exit code.
pub fn pdf_helper(path: &Path) -> i32 {
    use std::io::Write;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let say = |text: &str| {
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    };
    let result = std::fs::read(path).map_err(|_| "The file could not be read".to_string()).and_then(|bytes| pdf(&bytes, &|step| say(&format!("PROGRESS {step}\n"))));
    match result {
        Ok((text, pages)) => {
            say(&format!("OK {pages}\n{text}"));
            0
        }
        Err(why) => {
            say(&format!("ERROR {}\n", why.replace('\n', " ")));
            1
        }
    }
}

/// Text of the first pages through the Windows PDF renderer and OCR engine.
fn pdf(bytes: &[u8], progress: &dyn Fn(String)) -> Result<(String, u32), String> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    let inner = || -> windows::core::Result<Result<(String, u32), String>> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(bytes)?;
        writer.StoreAsync()?.join()?;
        writer.DetachStream()?;
        stream.Seek(0)?;
        let Ok(document) = PdfDocument::LoadFromStreamAsync(&stream)?.join() else {
            return Ok(Err("The PDF is damaged or protected with a password".into()));
        };
        let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() else {
            return Ok(Err("Reading PDFs needs a Windows text-recognition language".into()));
        };
        let limit = OcrEngine::MaxImageDimension().unwrap_or(2600).clamp(800, 4000) as f32 - 1.0;
        let pages = document.PageCount()?;
        let mut text = String::new();
        for i in 0..pages.min(PDF_PAGES) {
            progress(format!("Page {} of {}", i + 1, pages.min(PDF_PAGES)));
            let page = document.GetPage(i)?;
            let size = page.Size()?;
            // About 200 dpi for an A4 page, within what the OCR engine accepts.
            let k = (1700.0 / size.Width.max(1.0)).min(limit / size.Height.max(1.0)).min(limit / size.Width.max(1.0));
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth((size.Width * k).max(1.0) as u32)?;
            let rendered = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&rendered, &options)?.join()?;
            let bitmap = BitmapDecoder::CreateAsync(&rendered)?.join()?.GetSoftwareBitmapAsync()?.join()?;
            let bitmap = SoftwareBitmap::Convert(&bitmap, BitmapPixelFormat::Bgra8)?;
            let result = engine.RecognizeAsync(&bitmap)?.join()?;
            text.push_str(&format!("\n--- Page {} ---\n", i + 1));
            for line in result.Lines()? {
                text.push_str(&line.Text()?.to_string_lossy());
                text.push('\n');
            }
        }
        if pages > PDF_PAGES {
            text.push_str(&format!("\n[Only the first {PDF_PAGES} of {pages} pages were read.]\n"));
        }
        Ok(Ok((text, pages)))
    };
    inner().unwrap_or_else(|e| Err(format!("The PDF could not be read ({})", e.message())))
}

// ----------------------------------------------------------------------- zip --

fn u16_at(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
}

fn u32_at(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// The entries of a zip archive (enough of the format for Office files).
struct Zip<'a> {
    data: &'a [u8],
    /// Name, method, compressed size, local header offset.
    entries: Vec<(String, usize, usize, usize)>,
}

impl<'a> Zip<'a> {
    fn open(data: &'a [u8]) -> Option<Zip<'a>> {
        let from = data.len().saturating_sub(65_557);
        let end = (from..data.len().saturating_sub(21)).rev().find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
        let (count, mut at) = (u16_at(data, end + 10)?, u32_at(data, end + 16)?);
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if data.get(at..at + 4)? != [0x50, 0x4b, 0x01, 0x02] {
                return None;
            }
            let (method, packed) = (u16_at(data, at + 10)?, u32_at(data, at + 20)?);
            let (name_len, extra, comment) = (u16_at(data, at + 28)?, u16_at(data, at + 30)?, u16_at(data, at + 32)?);
            let local = u32_at(data, at + 42)?;
            let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + name_len)?).into_owned();
            entries.push((name, method, packed, local));
            at += 46 + name_len + extra + comment;
        }
        Some(Zip { data, entries })
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, ..)| name.as_str())
    }

    fn read(&self, wanted: &str) -> Option<Vec<u8>> {
        let (_, method, packed, local) = self.entries.iter().find(|(name, ..)| name.eq_ignore_ascii_case(wanted))?;
        let data = self.data;
        if data.get(*local..*local + 4)? != [0x50, 0x4b, 0x03, 0x04] {
            return None;
        }
        let start = local + 30 + u16_at(data, local + 26)? + u16_at(data, local + 28)?;
        let raw = data.get(start..start + packed)?;
        match method {
            0 => Some(raw.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, ENTRY_LIMIT).ok(),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<String> {
        self.read(name).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

// ----------------------------------------------------------------------- XML --

/// Splits XML into (tag, text after it) pairs: `<a x="1">hi</a>` gives
/// ("a x=\"1\"", "hi") and ("/a", "").
fn tags(xml: &str) -> impl Iterator<Item = (&str, &str)> {
    xml.split('<').skip(1).filter_map(|piece| piece.split_once('>'))
}

/// The element name of a tag: "w:t" for `w:t xml:space="preserve"`, "/c" for a closing `/c`.
fn tag_name(tag: &str) -> &str {
    let body = tag.strip_prefix('/').unwrap_or(tag);
    let end = body.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(body.len());
    &tag[..end + (tag.len() - body.len())]
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let rest = &tag[at..];
    Some(&rest[..rest.find('"')?])
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&end| end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")).map(|hex| u32::from_str_radix(hex, 16)).or_else(|| entity.strip_prefix('#').map(|dec| dec.parse::<u32>())).and_then(|n| n.ok()).and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text of WordprocessingML / DrawingML: runs `text_tag`, paragraphs end at `paragraph`.
fn runs(xml: &str, text_tag: &str, paragraph: &str) -> String {
    let mut out = String::new();
    for (tag, after) in tags(xml) {
        match tag_name(tag) {
            // The text right after an opening run tag is the run's text.
            name if name == text_tag && !tag.ends_with('/') => out.push_str(&unescape(after)),
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" | "a:br" => out.push('\n'),
            _ if tag.strip_prefix('/') == Some(paragraph) => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn office(bytes: &[u8]) -> Result<Zip<'_>, String> {
    Zip::open(bytes).ok_or_else(|| "The file is damaged or not an Office document".to_string())
}

fn docx(bytes: &[u8]) -> Result<(String, Option<u32>), String> {
    let zip = office(bytes)?;
    let body = zip.text("word/document.xml").ok_or("No document text inside")?;
    let pages = zip.text("docProps/app.xml").and_then(|app| tags(&app).find(|(tag, _)| *tag == "Pages").and_then(|(_, n)| n.trim().parse().ok()));
    Ok((runs(&body, "w:t", "w:p"), pages))
}

/// Numbers in a part name ("ppt/slides/slide12.xml" -> 12) for ordering.
fn number_in(name: &str) -> u32 {
    name.rsplit('/').next().unwrap_or("").chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

fn pptx(bytes: &[u8]) -> Result<(String, u32), String> {
    let zip = office(bytes)?;
    let mut slides: Vec<&str> = zip.names().filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml")).collect();
    slides.sort_by_key(|n| number_in(n));
    let mut text = String::new();
    for (i, slide) in slides.iter().enumerate() {
        text.push_str(&format!("\n--- Slide {} ---\n", i + 1));
        text.push_str(&runs(&zip.text(slide).unwrap_or_default(), "a:t", "a:p"));
    }
    Ok((text, slides.len() as u32))
}

/// "AB12" -> 27 (zero-based column).
fn column(reference: &str) -> Option<usize> {
    let letters: Vec<u8> = reference.bytes().take_while(u8::is_ascii_alphabetic).collect();
    (!letters.is_empty()).then(|| letters.iter().fold(0usize, |n, &c| n * 26 + (c.to_ascii_uppercase() - b'A') as usize + 1) - 1)
}

fn xlsx(bytes: &[u8]) -> Result<(String, u32), String> {
    let zip = office(bytes)?;
    // Shared strings: each <si> is one string, possibly in several <t> runs.
    let mut shared = Vec::new();
    if let Some(xml) = zip.text("xl/sharedStrings.xml") {
        let (mut current, mut phonetic) = (String::new(), false);
        for (tag, after) in tags(&xml) {
            match tag_name(tag) {
                "si" if !tag.ends_with('/') => current.clear(),
                "/si" => shared.push(std::mem::take(&mut current)),
                "rPh" => phonetic = !tag.ends_with('/'),
                "/rPh" => phonetic = false,
                "t" if !tag.ends_with('/') && !phonetic => current.push_str(&unescape(after)),
                _ => {}
            }
        }
    }
    // Sheet order and names from the workbook, file names from its relations.
    let workbook = zip.text("xl/workbook.xml").ok_or("No workbook inside")?;
    let relations = zip.text("xl/_rels/workbook.xml.rels").unwrap_or_default();
    let target = |id: &str| {
        tags(&relations).map(|(tag, _)| tag).find(|tag| attr(tag, "Id") == Some(id)).and_then(|tag| attr(tag, "Target")).map(|t| match t.strip_prefix('/') {
            Some(absolute) => absolute.to_string(),
            None => format!("xl/{t}"),
        })
    };
    let mut text = String::new();
    let mut count = 0;
    for (tag, _) in tags(&workbook).filter(|(tag, _)| tag_name(tag) == "sheet") {
        let name = unescape(attr(tag, "name").unwrap_or("Sheet"));
        let Some(xml) = attr(tag, "r:id").and_then(target).and_then(|part| zip.text(&part)) else { continue };
        count += 1;
        text.push_str(&format!("\n--- Sheet: {name} ---\n"));
        let (mut row, mut col, mut kind, mut value) = (Vec::<String>::new(), 0usize, String::new(), None::<String>);
        for (tag, after) in tags(&xml) {
            match tag_name(tag) {
                "row" if !tag.ends_with('/') => row.clear(),
                "/row" => {
                    while row.last().is_some_and(String::is_empty) {
                        row.pop();
                    }
                    if !row.is_empty() {
                        text.push_str(&row.join("\t"));
                        text.push('\n');
                    }
                    if text.len() > TEXT_LIMIT * 4 {
                        break;
                    }
                }
                "c" => {
                    col = attr(tag, "r").and_then(column).unwrap_or(row.len());
                    kind = attr(tag, "t").unwrap_or("").to_string();
                    value = None;
                }
                "v" | "t" if !tag.ends_with('/') => value = Some(value.unwrap_or_default() + &unescape(after)),
                "/c" => {
                    let shown = match (kind.as_str(), value.take()) {
                        ("s", Some(v)) => v.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                        ("b", Some(v)) => if v.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
                        (_, Some(v)) => v,
                        (_, None) => String::new(),
                    };
                    if col < 16_384 {
                        if row.len() <= col {
                            row.resize(col + 1, String::new());
                        }
                        row[col] = shown;
                    }
                }
                _ => {}
            }
        }
    }
    Ok((text, count))
}

// ------------------------------------------------------------------- base64 --

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - i * 6)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace() && *b != b'=') {
        let value = ALPHABET.iter().position(|&c| c == byte)? as u32;
        buffer = buffer << 6 | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------------- picker --

/// The Windows "Open" dialog for attachments (modal: call outside the app's borrow).
pub fn pick(owner: HWND) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    unsafe {
        let Ok(dialog) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else { return paths };
        let types = [
            COMDLG_FILTERSPEC { pszName: w!("Pictures, documents and text"), pszSpec: w!("*.png;*.jpg;*.jpeg;*.gif;*.bmp;*.webp;*.tif;*.tiff;*.heic;*.pdf;*.docx;*.xlsx;*.xlsm;*.pptx;*.txt;*.md;*.csv;*.json;*.log;*.xml;*.html;*.rs;*.py;*.js;*.ts") },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&types);
        let _ = dialog.SetTitle(w!("Attach to the chat"));
        if let Ok(options) = dialog.GetOptions() {
            let _ = dialog.SetOptions(options | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        }
        if dialog.Show(Some(owner)).is_err() {
            return paths;
        }
        let Ok(items) = dialog.GetResults() else { return paths };
        for i in 0..items.GetCount().unwrap_or(0) {
            if let Ok(path) = items.GetItemAt(i).and_then(|item| item.GetDisplayName(SIGDN_FILESYSPATH)) {
                paths.push(PathBuf::from(String::from_utf16_lossy(path.as_wide())));
                CoTaskMemFree(Some(path.0 as *const _));
            }
        }
    }
    paths
}

/// A one-page PDF saying `line` in Helvetica (tests and the hidden test hook).
pub fn sample_pdf(line: &str) -> Vec<u8> {
    let stream = format!("BT /F1 28 Tf 72 700 Td ({line}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    pdf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip archive: (name, content, deflate?) entries.
    fn zip(files: &[(&str, &str, bool)]) -> Vec<u8> {
        let (mut out, mut directory) = (Vec::new(), Vec::new());
        for (name, content, deflate) in files {
            let packed = if *deflate { miniz_oxide::deflate::compress_to_vec(content.as_bytes(), 6) } else { content.as_bytes().to_vec() };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            let header = |sig: &[u8; 4], central: bool| {
                let mut h = sig.to_vec();
                if central {
                    h.extend_from_slice(&20u16.to_le_bytes());
                }
                h.extend_from_slice(&20u16.to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes());
                h.extend_from_slice(&method.to_le_bytes());
                h.extend_from_slice(&[0; 8]);
                h.extend_from_slice(&(packed.len() as u32).to_le_bytes());
                h.extend_from_slice(&(content.len() as u32).to_le_bytes());
                h.extend_from_slice(&(name.len() as u16).to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes());
                if central {
                    // Comment length, disk, internal and external attributes.
                    h.extend_from_slice(&[0; 10]);
                    h.extend_from_slice(&offset.to_le_bytes());
                }
                h.extend_from_slice(name.as_bytes());
                h
            };
            out.extend_from_slice(&header(b"PK\x03\x04", false));
            out.extend_from_slice(&packed);
            directory.extend_from_slice(&header(b"PK\x01\x02", true));
        }
        let start = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn word_document() {
        let body = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Report Q3</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Sales &amp; costs </w:t><w:tab/><w:t>up 5%</w:t></w:r></w:p><w:p><w:r><w:t/></w:r><w:r><w:t>Caf&#233; &#x1F600;</w:t></w:r></w:p></w:body></w:document>"#;
        let file = zip(&[("word/document.xml", body, true), ("docProps/app.xml", "<Properties><Pages>3</Pages></Properties>", false)]);
        let (text, pages) = docx(&file).unwrap();
        assert_eq!(text, "Report Q3\nSales & costs \tup 5%\nCaf\u{e9} \u{1F600}\n");
        assert_eq!(pages, Some(3));
        assert!(docx(b"not a zip").is_err());
    }

    #[test]
    fn excel_workbook() {
        let workbook = r#"<workbook><sheets><sheet name="Budget &amp; plan" sheetId="1" r:id="rId1"/><sheet name="Empty" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
        let relations = r#"<Relationships><Relationship Id="rId1" Type="x/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="x/worksheet" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#;
        let strings = r#"<sst><si><t>Item</t></si><si><r><t>Co</t></r><r><t>st</t></r><rPh><t>x</t></rPh></si></sst>"#;
        let sheet = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Rent</t></is></c><c r="B2" s="1"/><c r="C2"><v>1200.5</v></c><c r="D2" t="b"><v>1</v></c></row></sheetData></worksheet>"#;
        let file = zip(&[("xl/workbook.xml", workbook, true), ("xl/_rels/workbook.xml.rels", relations, false), ("xl/sharedStrings.xml", strings, true), ("xl/worksheets/sheet1.xml", sheet, true), ("xl/worksheets/sheet2.xml", "<worksheet/>", false)]);
        let (text, sheets) = xlsx(&file).unwrap();
        assert_eq!(sheets, 2);
        assert_eq!(text, "\n--- Sheet: Budget & plan ---\nItem\t\tCost\nRent\t\t1200.5\tTRUE\n\n--- Sheet: Empty ---\n");
        assert_eq!((column("A1"), column("AB12"), column("12")), (Some(0), Some(27), None));
    }

    #[test]
    fn slides_text_and_helpers() {
        let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
        let file = zip(&[("ppt/slides/slide10.xml", &slide("Ten"), true), ("ppt/slides/slide2.xml", &slide("Two"), true), ("ppt/slides/_rels/slide2.xml.rels", "<x/>", false)]);
        let (text, slides) = pptx(&file).unwrap();
        assert_eq!((text.as_str(), slides), ("\n--- Slide 1 ---\nTwo\n\n--- Slide 2 ---\nTen\n", 2));

        assert_eq!(plain_text("h\u{e9}llo".as_bytes()).as_deref(), Some("h\u{e9}llo"));
        assert_eq!(plain_text(&[0xff, 0xfe, b'h', 0, b'i', 0]).as_deref(), Some("hi"));
        assert_eq!(plain_text(&[0, 1, 2, 3]), None);
        assert_eq!(unescape("a &lt;b&gt; &unknown; & &#65;"), "a <b> &unknown; & A");
        let long = document("a.txt".into(), "x".repeat(TEXT_LIMIT + 5), "1 KB".into());
        assert!(long.detail.ends_with("shortened"));
        assert!(matches!(long.content, Content::Text(ref t) if t.len() == TEXT_LIMIT));
        for sample in [&b""[..], b"f", b"fo", b"foo", b"foob", b"\xff\x00\x10"] {
            assert_eq!(unbase64(&base64(sample)).unwrap(), sample);
        }
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!((size_text(900), size_text(4096), plural(1, "page"), plural(3, "sheet")), ("900 B".into(), "4 KB".into(), "1 page".into(), "3 sheets".into()));
    }

    #[test]
    fn pictures_become_small_jpegs() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let big = Pixels { w: 3200, h: 1600, bgra: vec![128; 3200 * 1600 * 4] };
        let attachment = picture("shot.png", &big).unwrap();
        let Content::Image { jpeg, w, h } = attachment.content else { panic!() };
        assert_eq!((w, h, &jpeg[..2]), (1568, 784, &[0xff, 0xd8][..]));
        assert_eq!(attachment.detail, "1568 \u{00D7} 784");
    }

    /// Renders a one-page PDF with the Windows PDF engine and reads it with OCR.
    #[test]
    #[ignore = "needs an installed OCR language"]
    fn live_pdf() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (text, pages) = pdf(&sample_pdf("Quarterly report 2026"), &|step| println!("{step}")).unwrap();
        println!("{text:?}");
        assert_eq!(pages, 1);
        assert!(text.contains("Quarterly") && text.contains("2026"));
        assert!(pdf(b"%PDF-1.4 broken", &|_| {}).is_err());
    }
}
