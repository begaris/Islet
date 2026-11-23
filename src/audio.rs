//! Audio: WASAPI loopback capture feeding the equalizer, master volume and
//! output devices.
//!
//! The capture thread only exists while the equalizer is visible. It is
//! event-driven (it sleeps until the audio engine has data), runs a small FFT
//! about 30 times per second and publishes four band levels through atomics
//! plus one coalesced message to the UI thread.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::thread::JoinHandle;
use windows::core::{interface, IUnknown, IUnknown_Vtbl, Result, GUID, HRESULT, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_ONESHOT_RECONNECT, KSPROPERTY_TYPE_GET, KSPROPSETID_BtAudio};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the island window when new equalizer levels are available.
pub const WM_APP_EQ: u32 = WM_APP + 4;
pub const BANDS: usize = 4;

static LEVELS: [AtomicU32; BANDS] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
static PENDING: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);

/// Latest band levels (0..1). The UI thread calls this when handling `WM_APP_EQ`.
pub fn levels() -> [f32; BANDS] {
    PENDING.store(false, Relaxed);
    [0, 1, 2, 3].map(|i| f32::from_bits(LEVELS[i].load(Relaxed)))
}

fn publish(levels: &[f32; BANDS]) {
    for (slot, v) in LEVELS.iter().zip(levels) {
        slot.store(v.to_bits(), Relaxed);
    }
    if !PENDING.swap(true, Relaxed) {
        let hwnd = HWND(TARGET.load(Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_EQ, WPARAM(0), LPARAM(0));
        }
    }
}

// ------------------------------------------------------------------ capture --

pub struct Capture {
    stop: isize,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(target: HWND) -> Option<Self> {
        TARGET.store(target.0 as isize, Relaxed);
        let stop = unsafe { CreateEventW(None, true, false, None).ok()? };
        let stop_raw = stop.0 as isize;
        let thread = std::thread::Builder::new()
            .name("islet-audio".into())
            .stack_size(128 * 1024)
            .spawn(move || unsafe {
                let stop = HANDLE(stop_raw as *mut _);
                if CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() {
                    // Re-open the stream if the device goes away (unplugged, default changed).
                    loop {
                        let _ = capture_session(stop);
                        if WaitForMultipleObjects(&[stop], false, 700) != WAIT_TIMEOUT {
                            break;
                        }
                    }
                    CoUninitialize();
                }
                publish(&[0.0; BANDS]);
            })
            .ok();
        Some(Self { stop: stop_raw, thread })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let stop = HANDLE(self.stop as *mut _);
        unsafe {
            let _ = SetEvent(stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = CloseHandle(stop);
        }
    }
}

const FFT_SIZE: usize = 1024;

/// Captures what the default output device is playing until `stop` is set or
/// the stream fails.
unsafe fn capture_session(stop: HANDLE) -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format_ptr = client.GetMixFormat()?;
        let format = format_ptr.read_unaligned();
        let (channels, rate, bits) = (format.nChannels.max(1) as usize, format.nSamplesPerSec.max(8000) as f32, format.wBitsPerSample);
        let block = format.nBlockAlign as usize;
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 2_000_000, 0, format_ptr, None);
        CoTaskMemFree(Some(format_ptr as *const _));
        init?;
        let event = CreateEventW(None, false, false, None)?;
        let result = (|| -> Result<()> {
            client.SetEventHandle(event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;

            let mut ring = [0.0f32; FFT_SIZE];
            let mut write = 0usize;
            let mut since = 0usize;
            let hop = (rate / 30.0) as usize;
            let mut smooth = [0.0f32; BANDS];
            loop {
                let wait = WaitForMultipleObjects(&[stop, event], false, 150);
                if wait == WAIT_OBJECT_0 {
                    break;
                }
                if wait == WAIT_TIMEOUT {
                    // Nothing is playing: let the bars fall to rest, then stay quiet.
                    if smooth.iter().any(|v| *v > 0.001) {
                        for v in &mut smooth {
                            *v *= 0.4;
                            if *v < 0.01 {
                                *v = 0.0;
                            }
                        }
                        publish(&smooth);
                    }
                    continue;
                }
                loop {
                    if capture.GetNextPacketSize()? == 0 {
                        break;
                    }
                    let (mut data, mut frames, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                    for frame in 0..frames as usize {
                        let mut sample = 0.0f32;
                        if !silent {
                            let base = data.add(frame * block);
                            for ch in 0..channels {
                                sample += match bits {
                                    32 => (base.add(ch * 4) as *const f32).read_unaligned(),
                                    16 => (base.add(ch * 2) as *const i16).read_unaligned() as f32 / 32768.0,
                                    _ => 0.0,
                                };
                            }
                            sample /= channels as f32;
                        }
                        ring[write] = sample;
                        write = (write + 1) % FFT_SIZE;
                        since += 1;
                    }
                    capture.ReleaseBuffer(frames)?;
                }
                if since >= hop {
                    since = 0;
                    let fresh = analyse(&ring, write, rate);
                    for (s, f) in smooth.iter_mut().zip(fresh) {
                        // Fast attack, slower release.
                        *s += (f - *s) * if f > *s { 0.65 } else { 0.28 };
                    }
                    publish(&smooth);
                }
            }
            let _ = client.Stop();
            Ok(())
        })();
        let _ = CloseHandle(event);
        result
    }
}

/// Hann window + radix-2 FFT of the last `FFT_SIZE` samples, reduced to four
/// perceptually spaced band levels in 0..1.
fn analyse(ring: &[f32; FFT_SIZE], start: usize, rate: f32) -> [f32; BANDS] {
    let mut re = [0.0f32; FFT_SIZE];
    let mut im = [0.0f32; FFT_SIZE];
    for (i, slot) in re.iter_mut().enumerate() {
        let window = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos();
        *slot = ring[(start + i) % FFT_SIZE] * window;
    }
    fft(&mut re, &mut im);

    const EDGES: [f32; BANDS + 1] = [45.0, 220.0, 900.0, 3200.0, 11000.0];
    let hz_per_bin = rate / FFT_SIZE as f32;
    let mut out = [0.0f32; BANDS];
    for (band, level) in out.iter_mut().enumerate() {
        let lo = ((EDGES[band] / hz_per_bin) as usize).max(1);
        let hi = ((EDGES[band + 1] / hz_per_bin) as usize).clamp(lo + 1, FFT_SIZE / 2);
        let mut peak = 0.0f32;
        for bin in lo..hi {
            peak = peak.max(re[bin] * re[bin] + im[bin] * im[bin]);
        }
        // A full-scale sine peaks at FFT_SIZE / 4 with a Hann window.
        let magnitude = peak.sqrt() / (FFT_SIZE as f32 / 4.0);
        let db = 20.0 * (magnitude + 1e-7).log10() + band as f32 * 7.0;
        *level = ((db + 48.0) / 40.0).clamp(0.0, 1.0);
    }
    out
}

fn fft(re: &mut [f32; FFT_SIZE], im: &mut [f32; FFT_SIZE]) {
    let n = FFT_SIZE;
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f32::consts::TAU / len as f32;
        let (w_im, w_re) = angle.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut c, mut s) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = c * w_re - s * w_im;
                s = c * w_im + s * w_re;
                c = next;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_tone() {
        // 1 kHz at 48 kHz lands in the third band (900-3200 Hz).
        let mut ring = [0.0f32; FFT_SIZE];
        for (i, s) in ring.iter_mut().enumerate() {
            *s = 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin();
        }
        let levels = analyse(&ring, 0, 48000.0);
        assert!(levels[2] > 0.9, "{levels:?}");
        assert!(levels[0] < 0.3 && levels[3] < levels[2], "{levels:?}");
        assert_eq!(analyse(&[0.0; FFT_SIZE], 0, 48000.0), [0.0; BANDS]);
    }

    /// Live check of loopback capture, volume and devices (draws nothing):
    /// `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore = "uses the real audio devices"]
    fn live_audio() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        println!("volume = {:?}", volume());
        for o in outputs() {
            println!("output {:?} current={} id_len={}", String::from_utf16_lossy(&o.name), o.current, o.id.len());
        }
        let capture = Capture::start(HWND::default());
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            println!("levels = {:.2?}", levels());
        }
        drop(capture);
    }
}

// ------------------------------------------------------- volume and devices --

pub struct Output {
    /// Short display name, e.g. "Speakers".
    pub name: Vec<u16>,
    /// Endpoint id, NUL-terminated.
    pub id: Vec<u16>,
    pub current: bool,
    /// A paired Bluetooth device that is not connected; choosing it asks the
    /// driver to connect instead of switching the default output.
    pub offline: bool,
}

unsafe fn default_device() -> Result<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
    }
}

/// Master volume of the default output, 0..1.
pub fn volume() -> Option<f32> {
    unsafe {
        let endpoint: IAudioEndpointVolume = default_device().ok()?.Activate(CLSCTX_ALL, None).ok()?;
        endpoint.GetMasterVolumeLevelScalar().ok()
    }
}

pub fn set_volume(level: f32) {
    unsafe {
        if let Ok(device) = default_device() {
            if let Ok(endpoint) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                let _ = endpoint.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
            }
        }
    }
}

unsafe fn wide(p: windows::core::PWSTR) -> Vec<u16> {
    unsafe {
        let v = p.as_wide().to_vec();
        CoTaskMemFree(Some(p.0 as *const _));
        v
    }
}

/// Active output devices, the default one flagged.
pub fn outputs() -> Vec<Output> {
    let mut list = Vec::new();
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return list };
        let current = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|d| d.GetId()).map(|p| wide(p)).unwrap_or_default();
        let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return list };
        for i in 0..devices.GetCount().unwrap_or(0) {
            let Ok(device) = devices.Item(i) else { continue };
            let Ok(id) = device.GetId().map(|p| wide(p)) else { continue };
            let mut name = friendly(&device);
            // "Speakers (Realtek(R) Audio)" -> "Speakers"
            if let Some(cut) = name.windows(2).position(|w| w[0] == b' ' as u16 && w[1] == b'(' as u16) {
                if cut > 0 {
                    name.truncate(cut);
                }
            }
            let is_current = id == current;
            let mut id = id;
            id.push(0);
            list.push(Output { name, id, current: is_current, offline: false });
        }
        // Paired Bluetooth devices that are switched off or out of range.
        for name in offline_bluetooth(&enumerator, eRender) {
            if list.len() < 8 && !list.iter().any(|o| o.name == name) {
                list.push(Output { name, id: Vec::new(), current: false, offline: true });
            }
        }
    }
    list
}

unsafe fn friendly(device: &IMMDevice) -> Vec<u16> {
    unsafe { device.OpenPropertyStore(STGM_READ).and_then(|store| store.GetValue(&PKEY_Device_FriendlyName)).and_then(|value| PropVariantToStringAlloc(&value)).map(|p| wide(p)).unwrap_or_default() }
}

// ---------------------------------------------------------------- Bluetooth --
//
// Windows has no documented "connect this paired headset" call. What the sound
// settings do is send KSPROPERTY_ONESHOT_RECONNECT to the Bluetooth audio
// driver's filter, reached through the endpoint's device topology.
// (Written without a Bluetooth radio to try it on.)

/// "Headphones (WH-1000XM4 Stereo)" -> "WH-1000XM4": the device's own name,
/// the same for its stereo and hands-free endpoints.
fn bluetooth_name(friendly: &[u16]) -> Vec<u16> {
    let text = String::from_utf16_lossy(friendly);
    let inner = match (text.find('('), text.rfind(')')) {
        (Some(open), Some(close)) if close > open + 1 => &text[open + 1..close],
        _ => text.as_str(),
    };
    let mut name = inner.trim();
    for suffix in [" Hands-Free AG Audio", " Hands-Free", " Stereo"] {
        name = name.strip_suffix(suffix).unwrap_or(name);
    }
    name.trim().encode_utf16().collect()
}

/// `true` for the id of a filter that belongs to the Bluetooth audio drivers
/// (`...\bthenum#...` for A2DP, `...\bthhfenum#...` for hands-free).
fn is_bluetooth(filter: &[u16]) -> bool {
    String::from_utf16_lossy(filter).to_ascii_lowercase().contains("bth")
}

/// The kernel-streaming filter behind an endpoint, when it is a Bluetooth one.
unsafe fn bluetooth_filter(enumerator: &IMMDeviceEnumerator, device: &IMMDevice) -> Option<IKsControl> {
    unsafe {
        let topology: IDeviceTopology = device.Activate(CLSCTX_ALL, None).ok()?;
        let mut filter = wide(topology.GetConnector(0).ok()?.GetDeviceIdConnectedTo().ok()?);
        if !is_bluetooth(&filter) {
            return None;
        }
        filter.push(0);
        enumerator.GetDevice(PCWSTR(filter.as_ptr())).ok()?.Activate(CLSCTX_ALL, None).ok()
    }
}

/// Unplugged Bluetooth endpoints of one direction, as (device name, filter).
unsafe fn unplugged_bluetooth(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Vec<(Vec<u16>, IKsControl)> {
    let mut list = Vec::new();
    unsafe {
        let Ok(devices) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_UNPLUGGED) else { return list };
        for i in 0..devices.GetCount().unwrap_or(0) {
            let Ok(device) = devices.Item(i) else { continue };
            if let Some(filter) = bluetooth_filter(enumerator, &device) {
                list.push((bluetooth_name(&friendly(&device)), filter));
            }
        }
    }
    list
}

unsafe fn offline_bluetooth(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Vec<Vec<u16>> {
    let mut names: Vec<Vec<u16>> = Vec::new();
    for (name, _) in unsafe { unplugged_bluetooth(enumerator, flow) } {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Asks the Bluetooth audio driver to connect the paired device called `name`
/// (as listed by [`outputs`]). Runs on a worker thread and does not wait for
/// the result: the endpoint simply becomes active when the headset answers.
pub fn connect_bluetooth(name: Vec<u16>) {
    let _ = std::thread::Builder::new().name("islet-bluetooth".into()).stack_size(256 * 1024).spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            // Both profiles: the stereo and the hands-free filter each connect their own link.
            for (found, filter) in unplugged_bluetooth(&enumerator, eAll) {
                if found == name {
                    let property = KSIDENTIFIER { Anonymous: KSIDENTIFIER_0 { Anonymous: KSIDENTIFIER_0_0 { Set: KSPROPSETID_BtAudio, Id: KSPROPERTY_ONESHOT_RECONNECT.0 as u32, Flags: KSPROPERTY_TYPE_GET } } };
                    let mut returned = 0u32;
                    let _ = filter.KsProperty(&property, size_of::<KSIDENTIFIER>() as u32, std::ptr::null_mut(), 0, &mut returned);
                }
            }
        }
        CoUninitialize();
    });
}

#[cfg(test)]
mod bluetooth_tests {
    use super::*;

    fn name(text: &str) -> String {
        String::from_utf16_lossy(&bluetooth_name(&text.encode_utf16().collect::<Vec<_>>()))
    }

    #[test]
    fn both_endpoints_of_a_headset_share_one_name() {
        assert_eq!(name("Headphones (WH-1000XM4 Stereo)"), "WH-1000XM4");
        assert_eq!(name("Headset (WH-1000XM4 Hands-Free AG Audio)"), "WH-1000XM4");
        assert_eq!(name("Headphones (2- AirPods Pro (2))"), "2- AirPods Pro (2)");
        assert_eq!(name("JBL Flip"), "JBL Flip");
    }

    #[test]
    fn bluetooth_filters_are_recognised_by_their_enumerator() {
        let id = |text: &str| text.encode_utf16().collect::<Vec<_>>();
        assert!(is_bluetooth(&id(r"{2}.\\?\bthenum#{0000110b-0000-1000-8000-00805f9b34fb}_ven#8&1&0#{6994ad04}\snk")));
        assert!(is_bluetooth(&id(r"{2}.\\?\BTHHFENUM#BthHFPAudio#8&2&0#{6994ad04}\global")));
        assert!(!is_bluetooth(&id(r"{2}.\\?\hdaudio#func_01&ven_10ec#5&1&0#{6994ad04}\espeakertopo")));
    }

    /// Lists the outputs, including paired Bluetooth devices that are not connected.
    #[test]
    #[ignore = "reads the real audio endpoints"]
    fn live_outputs() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        for output in outputs() {
            println!("{} current={} offline={}", String::from_utf16_lossy(&output.name), output.current, output.offline);
        }
    }
}

// Switching the default endpoint has no documented API. This is the interface
// the Windows sound settings use themselves; widely relied on, but undocumented.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    // Only the position in the vtable matters; the first ten slots are never
    // called and are declared without their real parameters.
    unsafe fn get_mix_format(&self) -> HRESULT;
    unsafe fn get_device_format(&self) -> HRESULT;
    unsafe fn reset_device_format(&self) -> HRESULT;
    unsafe fn set_device_format(&self) -> HRESULT;
    unsafe fn get_processing_period(&self) -> HRESULT;
    unsafe fn set_processing_period(&self) -> HRESULT;
    unsafe fn get_share_mode(&self) -> HRESULT;
    unsafe fn set_share_mode(&self) -> HRESULT;
    unsafe fn get_property_value(&self) -> HRESULT;
    unsafe fn set_property_value(&self) -> HRESULT;
    unsafe fn set_default_endpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
    unsafe fn set_endpoint_visibility(&self) -> HRESULT;
}

const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Makes the endpoint with this (NUL-terminated) id the default output.
pub fn set_default_output(id: &[u16]) -> bool {
    if id.last() != Some(&0) {
        return false;
    }
    unsafe {
        let Ok(policy) = CoCreateInstance::<_, IPolicyConfig>(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) else { return false };
        let mut ok = true;
        for role in [eConsole, eMultimedia, eCommunications] {
            ok &= policy.set_default_endpoint(PCWSTR(id.as_ptr()), role).is_ok();
        }
        ok
    }
}
