//! The Home tab's app dock: a few pinned apps, files or folders that open with
//! one click. Only paths are stored (`%APPDATA%\Islet\dock.txt`); icons come
//! from the shell.

use crate::image::Pixels;
use std::path::{Path, PathBuf};
use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY};

/// Slots in the dock row.
pub const LIMIT: usize = 10;
/// Icons are fetched at this size (the slot is 30 dp, so this is sharp up to 200 %).
const ICON: i32 = 64;

#[derive(Default)]
pub struct Dock {
    pub items: Vec<PathBuf>,
    /// `None`: memory only (tests).
    file: Option<PathBuf>,
}

impl Dock {
    pub fn load() -> Self {
        let file = std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("Islet").join("dock.txt"));
        let items = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).map(|text| text.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).take(LIMIT).collect()).unwrap_or_default();
        Self { items, file }
    }

    pub fn memory_only() -> Self {
        Self::default()
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self.items.iter().map(|p| format!("{}\n", p.display())).collect();
        let _ = std::fs::write(file, text);
    }

    /// Pins paths at the end, while there is room. Returns how many were added.
    pub fn add(&mut self, paths: Vec<PathBuf>) -> usize {
        let mut added = 0;
        for path in paths {
            if self.items.len() < LIMIT && !self.items.contains(&path) {
                self.items.push(path);
                added += 1;
            }
        }
        if added > 0 {
            self.save();
        }
        added
    }

    /// Moves the item at `from` to `to` (the others shift). Not saved until
    /// [`Dock::commit`], so a drag over several slots writes the file once.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to < self.items.len() && from != to {
            let item = self.items.remove(from);
            self.items.insert(to, item);
        }
    }

    pub fn commit(&self) {
        self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.save();
        }
    }
}

/// What to call a pinned item: its file name without ".lnk" / ".exe".
pub fn label(path: &Path) -> String {
    let shortcut = path.extension().is_some_and(|e| ["lnk", "exe", "url"].iter().any(|x| e.eq_ignore_ascii_case(x)));
    let name = if shortcut { path.file_stem() } else { path.file_name() };
    name.map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// A stable key for a path's icon (see `Gfx::set_image`); never 0.
pub fn key(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | 1 << 63
}

/// The shell's icon for a file, folder or shortcut as premultiplied BGRA.
/// Needs COM on the calling thread.
pub fn icon(path: &Path) -> Option<Pixels> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None::<&IBindCtx>).ok()?;
        let bitmap = factory.GetImage(SIZE { cx: ICON, cy: ICON }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        // Ask for the pixels top-down; the first call fills in the bitmap's real size.
        let mut info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, ..Default::default() }, ..Default::default() };
        let dc = CreateCompatibleDC(None);
        let mut pixels = None;
        if GetDIBits(dc, bitmap, 0, 0, None, &mut info, DIB_RGB_COLORS) != 0 {
            let (w, h) = (info.bmiHeader.biWidth, info.bmiHeader.biHeight.abs());
            if (1..=256).contains(&w) && (1..=256).contains(&h) {
                info.bmiHeader.biHeight = -h;
                info.bmiHeader.biBitCount = 32;
                info.bmiHeader.biCompression = 0;
                let mut bgra = vec![0u8; (w * h * 4) as usize];
                if GetDIBits(dc, bitmap, 0, h as u32, Some(bgra.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS) != 0 {
                    // Icons without an alpha channel come back with alpha 0 everywhere.
                    if bgra.as_chunks::<4>().0.iter().all(|px| px[3] == 0) {
                        for px in bgra.as_chunks_mut::<4>().0 {
                            px[3] = 255;
                        }
                    }
                    pixels = Some(Pixels { w: w as u32, h: h as u32, bgra });
                }
            }
        }
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bitmap.into());
        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_have_a_limit_and_labels() {
        let mut dock = Dock::memory_only();
        assert_eq!(dock.add((0..LIMIT + 3).map(|i| PathBuf::from(format!("C:\\apps\\app{i}.lnk"))).collect()), LIMIT);
        assert_eq!(dock.add(vec!["C:\\apps\\app1.lnk".into()]), 0);
        dock.remove(0);
        assert_eq!(dock.add(vec!["C:\\Work\\Designs".into()]), 1);
        assert_eq!(label(&dock.items[0]), "app1");
        assert_eq!(label(&dock.items[LIMIT - 1]), "Designs");
        assert_eq!(label(Path::new("C:\\a\\report.v2.pdf")), "report.v2.pdf");
        assert_ne!(key(Path::new("C:\\a")), key(Path::new("C:\\b")));
        assert_eq!(key(Path::new("C:\\A")), key(Path::new("c:\\a")));
    }

    /// The shell hands out an icon with visible pixels for a folder and a program.
    #[test]
    fn shell_icons() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        for path in [std::env::temp_dir(), PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("explorer.exe")] {
            let started = std::time::Instant::now();
            let icon = icon(&path).expect("icon");
            println!("{} -> {} x {} in {:?}", path.display(), icon.w, icon.h, started.elapsed());
            assert!(icon.w >= 32 && icon.bgra.len() == (icon.w * icon.h * 4) as usize);
            assert!(icon.bgra.as_chunks::<4>().0.iter().any(|px| px[3] > 200));
        }
    }
}
