//! Screen recording to MP4 (H.264, 30 fps, no sound) of the monitor under the cursor.
//!
//! Windows.Graphics.Capture needs a hardware Direct3D device, and the graphics
//! driver keeps tens of MB in a process even after its device is released. So
//! Islet never records itself: a helper copy (`islet.exe --record <x>,<y> <file>`)
//! captures and encodes with Media Foundation, reports on stdout and stops when
//! a line arrives on stdin (or stdin closes, e.g. when Islet exits), finishing
//! the file. Islet keeps itself out of the picture (`WDA_EXCLUDEFROMCAPTURE`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{Interface, HSTRING};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_RECORD: u32 = WM_APP + 23;

const FPS: u64 = 30;
/// A recording stops by itself after two hours.
const LONGEST: Duration = Duration::from_secs(2 * 3600);

// ------------------------------------------------------------- Islet's side --

#[derive(Clone, PartialEq, Debug)]
pub enum Update {
    /// The first frame is in the file.
    Started,
    /// Finished: the file, and how long it is.
    Saved(PathBuf, u64),
    Failed(String),
}

static UPDATES: Mutex<Vec<Update>> = Mutex::new(Vec::new());
/// The helper's stdin: a line (or closing it) stops the recording.
static HELPER: Mutex<Option<std::process::ChildStdin>> = Mutex::new(None);

pub fn take() -> Vec<Update> {
    UPDATES.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

fn report(target: isize, update: Update) {
    if let Ok(mut list) = UPDATES.lock() {
        list.push(update);
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_RECORD, WPARAM(0), LPARAM(0));
    }
}

/// "Recording 2026-11-14 153012.mp4" in `dir`.
pub fn file_in(dir: &Path) -> PathBuf {
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    dir.join(format!("Recording {:04}-{:02}-{:02} {:02}{:02}{:02}.mp4", now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond))
}

/// Starts the helper recording the monitor under `at` into `path`.
pub fn start(target: HWND, at: POINT, path: PathBuf) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(exe) = std::env::current_exe() else { return false };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let child = std::process::Command::new(exe)
        .arg("--record")
        .arg(format!("{},{}", at.x, at.y))
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let Ok(mut child) = child else { return false };
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return false };
    if let Ok(mut helper) = HELPER.lock() {
        *helper = Some(stdin);
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-record".into()).stack_size(128 * 1024).spawn(move || {
        let mut outcome = None;
        for line in std::io::BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if line == "STARTED" {
                report(target, Update::Started);
            } else if let Some(secs) = line.strip_prefix("SAVED ") {
                outcome = Some(Update::Saved(path.clone(), secs.trim().parse().unwrap_or(0)));
            } else if let Some(why) = line.strip_prefix("ERROR ") {
                outcome = Some(Update::Failed(why.to_string()));
            }
        }
        let _ = child.wait();
        if let Ok(mut helper) = HELPER.lock() {
            *helper = None;
        }
        report(target, outcome.unwrap_or_else(|| Update::Failed("The recorder stopped unexpectedly".into())));
    });
    true
}

/// Asks the helper to finish the file.
pub fn stop() {
    if let Ok(mut helper) = HELPER.lock() {
        if let Some(mut stdin) = helper.take() {
            let _ = stdin.write_all(b"stop\n");
            // Dropping it closes the pipe, which also stops the helper.
        }
    }
}

// ------------------------------------------------------------ helper's side --

/// The helper process (`islet.exe --record <x>,<y> <file>`); returns the exit code.
pub fn helper(at: &str, path: &Path) -> i32 {
    let say = |text: String| {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    };
    let point = at.split_once(',').and_then(|(x, y)| Some(POINT { x: x.trim().parse().ok()?, y: y.trim().parse().ok()? })).unwrap_or_default();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    // Any line on stdin, or stdin closing, ends the recording.
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        flag.store(true, Relaxed);
    });
    match record(point, path, &stop, &|| say("STARTED".into())) {
        Ok(secs) => {
            say(format!("SAVED {secs}"));
            0
        }
        Err(why) => {
            let _ = std::fs::remove_file(path);
            say(format!("ERROR {why}"));
            1
        }
    }
}

fn message(context: &str, error: windows::core::Error) -> String {
    format!("{context} ({})", error.message().trim_end_matches(['.', '\r', '\n']))
}

/// The H.264 writer for `w` x `h` frames (top-down), 30 fps. With a
/// device manager the writer takes Direct3D textures and converts and encodes
/// them on the graphics card: ARGB32 then, as the pooled textures must be
/// B8G8R8A8 like the capture to be copied into (B8G8R8X8, which RGB32 gives,
/// silently refuses the copy and records black). Also returns the input type.
unsafe fn writer(path: &Path, w: u32, h: u32, manager: Option<&IMFDXGIDeviceManager>) -> windows::core::Result<(IMFSinkWriter, u32, IMFMediaType)> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 3)?;
        let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
        // The graphics card's encoder when there is one.
        attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        if let Some(manager) = manager {
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, manager)?;
        }
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attributes)?;
        let size = (w as u64) << 32 | h as u64;
        let rate = FPS << 32 | 1;
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        // About 0.13 bit per pixel and frame: 8 Mbit/s for 1080p.
        output.SetUINT32(&MF_MT_AVG_BITRATE, ((w as u64 * h as u64 * FPS) / 8).clamp(2_000_000, 40_000_000) as u32)?;
        output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
        output.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        let stream = writer.AddStream(&output)?;
        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &if manager.is_some() { MFVideoFormat_ARGB32 } else { MFVideoFormat_RGB32 })?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
        input.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
        // A positive stride: rows top-down, as the capture delivers them.
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
        writer.SetInputMediaType(stream, &input, None)?;
        writer.BeginWriting()?;
        Ok((writer, stream, input))
    }
}

/// Where the frames handed to the encoder live.
enum Feed {
    /// On the graphics card: each frame is copied (GPU to GPU) into a texture
    /// from the allocator's pool, which the encoder hands back when done.
    Gpu { allocator: IMFVideoSampleAllocatorEx, current: ID3D11Texture2D },
    /// Through memory: copied out via a staging texture (the fallback when the
    /// encoder does not take textures).
    Cpu { staging: ID3D11Texture2D, pixels: Vec<u8> },
}

fn texture_desc(w: u32, h: u32, usage: D3D11_USAGE, bind: D3D11_BIND_FLAG, cpu: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: cpu,
        ..Default::default()
    }
}

/// The writer fed with textures, or `None` when the encoder will not take them.
unsafe fn gpu_writer(path: &Path, device: &ID3D11Device, w: u32, h: u32) -> Option<(IMFSinkWriter, u32, Feed)> {
    unsafe {
        // Media Foundation uses the device from its own threads.
        let _ = device.cast::<ID3D11Multithread>().ok()?.SetMultithreadProtected(true);
        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager).ok()?;
        let manager = manager?;
        manager.ResetDevice(device, token).ok()?;
        let (writer, stream, input) = writer(path, w, h, Some(&manager)).ok()?;
        let mut raw = std::ptr::null_mut();
        MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw).ok()?;
        let allocator = IMFVideoSampleAllocatorEx::from_raw(raw);
        allocator.SetDirectXManager(&manager).ok()?;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2).ok()?;
        let attributes = attributes?;
        attributes.SetUINT32(&MF_SA_D3D11_BINDFLAGS, (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32).ok()?;
        attributes.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32).ok()?;
        allocator.InitializeSampleAllocatorEx(4, 12, &attributes, &input).ok()?;
        let mut current = None;
        device.CreateTexture2D(&texture_desc(w, h, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0), None, Some(&mut current)).ok()?;
        Some((writer, stream, Feed::Gpu { allocator, current: current? }))
    }
}

/// The texture (and its subresource) inside a sample from the allocator.
unsafe fn sample_texture(sample: &IMFSample) -> Option<(ID3D11Texture2D, u32)> {
    unsafe {
        let media = sample.GetBufferByIndex(0).ok()?;
        // A pooled buffer may come back with no length, which the writer refuses.
        if let Ok(length) = media.cast::<IMF2DBuffer>().and_then(|b| b.GetContiguousLength()) {
            let _ = media.SetCurrentLength(length);
        }
        let buffer: IMFDXGIBuffer = media.cast().ok()?;
        let mut raw = std::ptr::null_mut();
        buffer.GetResource(&ID3D11Texture2D::IID, &mut raw).ok()?;
        Some((ID3D11Texture2D::from_raw(raw), buffer.GetSubresourceIndex().ok()?))
    }
}

/// Records until `stop` is set; returns the length in seconds.
fn record(at: POINT, path: &Path, stop: &AtomicBool, started: &dyn Fn()) -> Result<u64, String> {
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| message("Media Foundation is not available", e))?;
        let result = record_inner(at, path, stop, started);
        let _ = MFShutdown();
        result
    }
}

unsafe fn record_inner(at: POINT, path: &Path, stop: &AtomicBool, started: &dyn Fn()) -> Result<u64, String> {
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context)).map_err(|e| message("No graphics device for capturing", e))?;
        let (Some(device), Some(context)) = (device, context) else { return Err("No graphics device for capturing".into()) };
        let dxgi: IDXGIDevice = device.cast().map_err(|e| message("No graphics device", e))?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).and_then(|d| d.cast()).map_err(|e| message("No graphics device", e))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| message("Screen capture is not available", e))?;
        let item: GraphicsCaptureItem = interop.CreateForMonitor(MonitorFromPoint(at, MONITOR_DEFAULTTOPRIMARY)).map_err(|e| message("The monitor cannot be captured", e))?;
        let size = item.Size().map_err(|e| message("The monitor cannot be captured", e))?;
        // H.264 wants even sizes.
        let (w, h) = ((size.Width.max(2) & !1) as u32, (size.Height.max(2) & !1) as u32);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size).map_err(|e| message("Screen capture failed", e))?;
        let session = pool.CreateCaptureSession(&item).map_err(|e| message("Screen capture failed", e))?;
        // Without the yellow frame where Windows allows it.
        if GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless).and_then(|op| op.join()).is_ok() {
            let _ = session.SetIsBorderRequired(false);
        }
        let _ = session.SetIsCursorCaptureEnabled(true);
        // `ISLET_RECORD_CPU` forces the memory path (for comparing the two).
        let gpu = if std::env::var_os("ISLET_RECORD_CPU").is_some() { None } else { gpu_writer(path, &device, w, h) };
        let (writer, stream, mut feed) = match gpu {
            Some(gpu) => gpu,
            None => {
                let _ = std::fs::remove_file(path);
                let mut staging = None;
                device.CreateTexture2D(&texture_desc(w, h, D3D11_USAGE_STAGING, D3D11_BIND_FLAG(0), D3D11_CPU_ACCESS_READ.0 as u32), None, Some(&mut staging)).map_err(|e| message("Screen capture failed", e))?;
                let staging = staging.ok_or("Screen capture failed")?;
                let (writer, stream, _) = writer(path, w, h, None).map_err(|e| message("No H.264 encoder", e))?;
                (writer, stream, Feed::Cpu { staging, pixels: vec![0u8; (w * h * 4) as usize] })
            }
        };
        session.StartCapture().map_err(|e| message("Screen capture failed", e))?;

        let frame_bytes = (w * h * 4) as usize;
        let mut have_frame = false;
        let period = Duration::from_nanos(1_000_000_000 / FPS);
        let begin = Instant::now();
        let mut index: u64 = 0;
        let mut announced = false;
        while !stop.load(Relaxed) && begin.elapsed() < LONGEST {
            // The newest frame since the last tick, if the screen changed.
            let mut newest = None;
            while let Ok(frame) = pool.TryGetNextFrame() {
                newest = Some(frame);
            }
            if let Some(frame) = newest {
                let texture: Option<ID3D11Texture2D> = frame.Surface().ok().and_then(|s| s.cast::<IDirect3DDxgiInterfaceAccess>().ok()).and_then(|access| access.GetInterface().ok());
                if let Some(texture) = texture {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    texture.GetDesc(&mut desc);
                    let region = D3D11_BOX { left: 0, top: 0, front: 0, right: desc.Width.min(w), bottom: desc.Height.min(h), back: 1 };
                    match &mut feed {
                        Feed::Gpu { current, .. } => {
                            context.CopySubresourceRegion(&*current, 0, 0, 0, 0, &texture, 0, Some(&region));
                            have_frame = true;
                        }
                        Feed::Cpu { staging, pixels } => {
                            context.CopySubresourceRegion(&*staging, 0, 0, 0, 0, &texture, 0, Some(&region));
                            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                            if context.Map(&*staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).is_ok() {
                                let row = (w * 4) as usize;
                                for y in 0..h as usize {
                                    let from = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), row);
                                    pixels[y * row..(y + 1) * row].copy_from_slice(from);
                                }
                                context.Unmap(&*staging, 0);
                                have_frame = true;
                            }
                        }
                    }
                }
            }
            // Every tick writes a frame (the last one again when nothing changed).
            let sample = if !have_frame {
                None
            } else {
                match &feed {
                    Feed::Gpu { allocator, current } => match allocator.AllocateSample() {
                        Ok(sample) => {
                            let (target, sub) = sample_texture(&sample).ok_or("Encoding failed (no texture in the sample)")?;
                            context.CopySubresourceRegion(&target, sub, 0, 0, 0, current, 0, None);
                            Some(sample)
                        }
                        // Every pooled frame is still with the encoder: try again shortly.
                        Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => return Err(message("Encoding failed (allocate)", e)),
                    },
                    Feed::Cpu { pixels, .. } => {
                        let buffer = MFCreateMemoryBuffer(frame_bytes as u32).map_err(|e| message("Out of memory", e))?;
                        let mut data = std::ptr::null_mut();
                        buffer.Lock(&mut data, None, None).map_err(|e| message("Out of memory", e))?;
                        std::ptr::copy_nonoverlapping(pixels.as_ptr(), data, frame_bytes);
                        let _ = buffer.Unlock();
                        buffer.SetCurrentLength(frame_bytes as u32).map_err(|e| message("Encoding failed", e))?;
                        let sample = MFCreateSample().map_err(|e| message("Encoding failed", e))?;
                        sample.AddBuffer(&buffer).map_err(|e| message("Encoding failed", e))?;
                        Some(sample)
                    }
                }
            };
            if let Some(sample) = sample {
                sample.SetSampleTime((index * 10_000_000 / FPS) as i64).map_err(|e| message("Encoding failed", e))?;
                sample.SetSampleDuration((10_000_000 / FPS) as i64).map_err(|e| message("Encoding failed", e))?;
                writer.WriteSample(stream, &sample).map_err(|e| message("Encoding failed (write)", e))?;
                index += 1;
                if !announced {
                    announced = true;
                    started();
                }
            }
            // Keep to the clock; a slow tick is not made up with a burst.
            let next = begin + period * (index.max(1) as u32);
            match next.checked_duration_since(Instant::now()) {
                Some(wait) => std::thread::sleep(wait),
                None if have_frame => {}
                None => std::thread::sleep(period),
            }
        }
        let _ = session.Close();
        let _ = pool.Close();
        if index == 0 {
            return Err("No picture arrived from the screen".into());
        }
        writer.Finalize().map_err(|e| message("The file could not be finished", e))?;
        Ok(index / FPS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How varied a decoded RGB32 frame is: the spread of grey values over a
    /// grid of points (0 = one flat colour, e.g. a black frame).
    unsafe fn spread(sample: &IMFSample, w: usize, h: usize) -> f64 {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut length)).unwrap();
            let pixels = std::slice::from_raw_parts(data, length as usize);
            let mut greys = Vec::new();
            for gy in 0..32 {
                for gx in 0..32 {
                    let at = ((gy * h / 32) * w + gx * w / 32) * 4;
                    if at + 2 < pixels.len() {
                        greys.push((pixels[at] as f64 + pixels[at + 1] as f64 + pixels[at + 2] as f64) / 3.0);
                    }
                }
            }
            let _ = buffer.Unlock();
            let mean = greys.iter().sum::<f64>() / greys.len().max(1) as f64;
            (greys.iter().map(|g| (g - mean) * (g - mean)).sum::<f64>() / greys.len().max(1) as f64).sqrt()
        }
    }

    /// Records two seconds of the real screen into a temporary file, reads it
    /// back with Media Foundation (size, frame count, and that the last frame
    /// is a picture rather than one flat colour), and deletes it. Set
    /// `ISLET_RECORD_CPU=1` to test the memory path instead of the GPU one.
    #[test]
    #[ignore = "records the real screen for two seconds"]
    fn live_recording() {
        let path = std::env::temp_dir().join("islet-recording-test.mp4");
        let stop = AtomicBool::new(false);
        let begin = Instant::now();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_secs(2));
                stop.store(true, Relaxed);
            });
            record(POINT { x: 10, y: 10 }, &path, &stop, &|| println!("first frame after {:?}", begin.elapsed()))
        });
        println!("result {result:?}, file {} bytes", std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).unwrap();
            let attributes = attributes.unwrap();
            // Decoded straight to RGB32 so the picture can be checked.
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).unwrap();
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes).unwrap();
            let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let size = reader.GetCurrentMediaType(video).unwrap().GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
            let rgb = MFCreateMediaType().unwrap();
            rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
            rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).unwrap();
            reader.SetCurrentMediaType(video, None, &rgb).unwrap();
            let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
            let (mut frames, mut last, mut variety) = (0, 0i64, 0.0);
            loop {
                let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
                reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample)).unwrap();
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
                if let Some(sample) = sample {
                    frames += 1;
                    last = time;
                    variety = spread(&sample, w, h);
                }
            }
            drop(reader);
            let _ = MFShutdown();
            println!("{w} x {h}, {frames} frames, last at {:.2} s, grey spread of the last frame {variety:.1}", last as f64 / 1e7);
            // A real desktop is never one flat colour; a black (empty) frame is.
            assert!(variety > 2.0, "the last frame is flat (spread {variety:.1})");
            // Starting the device and encoder takes most of a second of the two.
            assert!(frames >= 25 && (frames as f64) / (last as f64 / 1e7 + 1.0 / 30.0) > 25.0, "{frames} frames");
        }
        let _ = std::fs::remove_file(&path);
        assert!(result.is_ok());
    }
}
