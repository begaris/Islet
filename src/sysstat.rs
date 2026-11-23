//! System load for the System tab and the pill: CPU, memory, GPU and network.
//!
//! A worker thread samples once a second, but only while someone looks at the
//! numbers ([`want`]): the System tab is open, or the pill shows them. It stops
//! by itself otherwise, so nothing runs at rest.
//!
//! * CPU: `GetSystemTimes` deltas (all cores together).
//! * Memory: `GlobalMemoryStatusEx`.
//! * Network: `GetIfTable2`, byte counters of hardware adapters that are up
//!   (filter drivers and virtual adapters such as VPNs are left out, so traffic
//!   is not counted twice).
//! * GPU (NVIDIA only): NVML (`nvml.dll`, installed with the driver) for load,
//!   temperature and video memory, read by a helper process
//!   (`islet.exe --gpu-stats`) that lives only while sampling: in Islet's own
//!   process the driver parts NVML pulls in kept ~23 MB after unloading.
//!   Other graphics cards show no GPU numbers.
//!
//! CPU temperature needs a kernel driver or administrator rights: not shown.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::Duration;
use windows::core::{s, w};
use windows::Win32::Foundation::{FreeLibrary, FILETIME, HMODULE, HWND, LPARAM, WPARAM};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the island window when new numbers are ready ([`take`]).
pub const WM_APP_STATS: u32 = WM_APP + 29;
/// Seconds of history kept for the little charts.
pub const HISTORY: usize = 60;

/// One reading, with the last [`HISTORY`] seconds of each measure (oldest first).
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// 0..1 of all cores.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// GPU load 0..1, temperature in °C and video memory, when known.
    pub gpu: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub vram: Option<(u64, u64)>,
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
    pub cpu_hist: [f32; HISTORY],
    pub mem_hist: [f32; HISTORY],
    pub gpu_hist: [f32; HISTORY],
    /// Download + upload, bytes per second.
    pub net_hist: [f32; HISTORY],
    /// How many seconds of history there are (up to [`HISTORY`]).
    pub filled: usize,
}

impl Default for Stats {
    fn default() -> Self {
        Self { cpu: 0.0, mem_used: 0, mem_total: 0, gpu: None, gpu_temp: None, vram: None, down: 0.0, up: 0.0, cpu_hist: [0.0; HISTORY], mem_hist: [0.0; HISTORY], gpu_hist: [0.0; HISTORY], net_hist: [0.0; HISTORY], filled: 0 }
    }
}

impl Stats {
    /// Memory in use, 0..1.
    pub fn mem(&self) -> f32 {
        if self.mem_total == 0 { 0.0 } else { (self.mem_used as f64 / self.mem_total as f64) as f32 }
    }

    /// Adds a reading to the history (shifts the oldest out).
    fn push(&mut self) {
        let mem = self.mem();
        for (hist, value) in [(&mut self.cpu_hist, self.cpu), (&mut self.mem_hist, mem), (&mut self.gpu_hist, self.gpu.unwrap_or(0.0)), (&mut self.net_hist, (self.down + self.up) as f32)] {
            hist.rotate_left(1);
            hist[HISTORY - 1] = value;
        }
        self.filled = (self.filled + 1).min(HISTORY);
    }

    /// Made-up numbers with a minute of history (test hooks and snapshots).
    pub fn sample_data() -> Self {
        let mut s = Self { mem_total: 32 << 30, gpu_temp: Some(54.0), vram: Some((3_400 << 20, 12 << 30)), ..Default::default() };
        for i in 0..HISTORY {
            let t = i as f32;
            s.cpu = 0.18 + 0.12 * (t * 0.31).sin().abs() + if i % 17 == 3 { 0.35 } else { 0.0 };
            s.mem_used = ((0.46 + 0.02 * (t * 0.05).sin()) * (32u64 << 30) as f32) as u64;
            s.gpu = Some(0.35 + 0.3 * (t * 0.12).sin().max(0.0));
            s.down = 1.6e6 * (t * 0.2).sin().abs() as f64 + 40e3;
            s.up = 90e3;
            s.push();
        }
        s
    }
}

static LATEST: Mutex<Option<Stats>> = Mutex::new(None);
/// Someone is looking at the numbers.
static WANT: AtomicBool = AtomicBool::new(false);
/// The worker thread is alive.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The newest reading, if one arrived since the last call.
pub fn take() -> Option<Stats> {
    LATEST.lock().ok()?.take()
}

/// Starts sampling (posting [`WM_APP_STATS`] to `target` once a second) or
/// lets the worker stop after its current second.
pub fn want(target: HWND, on: bool) {
    WANT.store(on, SeqCst);
    if !on || RUNNING.swap(true, SeqCst) {
        return;
    }
    let target = target.0 as isize;
    let started = std::thread::Builder::new().name("islet-stats".into()).stack_size(256 * 1024).spawn(move || {
        worker(target);
        RUNNING.store(false, SeqCst);
        // Asked again in the moment it was stopping: carry on in a new one.
        if WANT.load(SeqCst) {
            want(HWND(target as *mut _), true);
        }
    });
    if started.is_err() {
        RUNNING.store(false, SeqCst);
    }
}

/// The GPU numbers as the helper last reported them.
type GpuReading = (Option<f32>, Option<f32>, Option<(u64, u64)>);

/// Runs `islet.exe --gpu-stats` and keeps its latest line in `out`. NVML stays
/// out of Islet's own process: the NVIDIA driver parts it loads keep ~23 MB
/// there even after `FreeLibrary`. Killed when the worker stops; it also ends
/// by itself when its stdin closes (Islet gone).
fn gpu_helper_start(out: std::sync::Arc<Mutex<GpuReading>>) -> Option<std::process::Child> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = std::process::Command::new(std::env::current_exe().ok()?)
        .arg("--gpu-stats")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let _ = std::thread::Builder::new().name("islet-gpu".into()).stack_size(64 * 1024).spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if let Some(reading) = parse_gpu_line(&line) {
                if let Ok(mut slot) = out.lock() {
                    *slot = reading;
                }
            }
        }
    });
    Some(child)
}

/// "GPU <load 0..1> <°C> <used bytes> <total bytes>", -1 where unknown.
fn parse_gpu_line(line: &str) -> Option<GpuReading> {
    let mut parts = line.strip_prefix("GPU ")?.split_whitespace().map(|p| p.parse::<f64>().ok());
    let (load, temp, used, total) = (parts.next()??, parts.next()??, parts.next()??, parts.next()??);
    let known = |v: f64| (v >= 0.0).then_some(v);
    Some((known(load).map(|v| v as f32), known(temp).map(|v| v as f32), known(used).zip(known(total)).map(|(u, t)| (u as u64, t as u64))))
}

/// The `--gpu-stats` helper: prints one reading a second until stdin closes or
/// a line arrives; exits at once (code 2) without an NVIDIA card.
pub fn gpu_helper() -> i32 {
    use std::io::{BufRead, Write};
    let Some(nvml) = Nvml::load() else { return 2 };
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        flag.store(true, SeqCst);
    });
    let mut out = std::io::stdout();
    while !stop.load(SeqCst) {
        let (load, temp, vram) = nvml.read();
        let (used, total) = vram.map_or((-1.0, -1.0), |(u, t)| (u as f64, t as f64));
        if writeln!(out, "GPU {} {} {} {}", load.map_or(-1.0, |v| v as f64), temp.map_or(-1.0, |v| v as f64), used, total).and_then(|_| out.flush()).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    0
}

fn worker(target: isize) {
    let mut cpu = CpuMeter::default();
    let mut net = NetMeter::default();
    let gpu = std::sync::Arc::new(Mutex::new((None, None, None)));
    let mut helper = gpu_helper_start(gpu.clone());
    let mut stats = Stats::default();
    // A first reading to measure the next one against.
    cpu.sample();
    net.sample(0.0);
    let mut last = std::time::Instant::now();
    while WANT.load(SeqCst) {
        std::thread::sleep(Duration::from_secs(1));
        let secs = last.elapsed().as_secs_f64().max(0.01);
        last = std::time::Instant::now();
        stats.cpu = cpu.sample();
        (stats.mem_used, stats.mem_total) = memory();
        (stats.down, stats.up) = net.sample(secs);
        if let Ok(reading) = gpu.lock() {
            (stats.gpu, stats.gpu_temp, stats.vram) = *reading;
        }
        stats.push();
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some(stats.clone());
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_STATS, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(child) = helper.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn ticks(t: FILETIME) -> u64 {
    (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64
}

#[derive(Default)]
struct CpuMeter {
    last: Option<(u64, u64)>,
}

impl CpuMeter {
    /// Busy share of all cores since the last call (0 the first time).
    fn sample(&mut self) -> f32 {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
            return 0.0;
        }
        // Kernel time includes idle time.
        let (idle, total) = (ticks(idle), ticks(kernel) + ticks(user));
        let busy = match self.last {
            Some((idle0, total0)) if total > total0 => busy_share(idle - idle0.min(idle), total - total0),
            _ => 0.0,
        };
        self.last = Some((idle, total));
        busy
    }
}

/// Busy share from idle and total time spent over the same span.
fn busy_share(idle: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (1.0 - idle as f64 / total as f64).clamp(0.0, 1.0) as f32 }
}

fn memory() -> (u64, u64) {
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return (0, 0);
    }
    (status.ullTotalPhys.saturating_sub(status.ullAvailPhys), status.ullTotalPhys)
}

#[derive(Default)]
struct NetMeter {
    last: Option<(u64, u64)>,
}

impl NetMeter {
    /// Received and sent bytes per second since the last call (`secs` ago).
    fn sample(&mut self, secs: f64) -> (f64, f64) {
        let Some((rx, tx)) = octets() else { return (0.0, 0.0) };
        let speed = match self.last {
            // Counters reset when an adapter comes back: count that second as 0.
            Some((rx0, tx0)) if rx >= rx0 && tx >= tx0 && secs > 0.0 => ((rx - rx0) as f64 / secs, (tx - tx0) as f64 / secs),
            _ => (0.0, 0.0),
        };
        self.last = Some((rx, tx));
        speed
    }
}

/// Bytes received and sent so far by hardware adapters that are up.
fn octets() -> Option<(u64, u64)> {
    const HARDWARE: u8 = 1;
    const FILTER: u8 = 2;
    const LOOPBACK: u32 = 24;
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            if flags & HARDWARE != 0 && flags & FILTER == 0 && row.Type != LOOPBACK && row.OperStatus == IfOperStatusUp {
                rx = rx.wrapping_add(row.InOctets);
                tx = tx.wrapping_add(row.OutOctets);
            }
        }
        FreeMibTable(table as *const _);
        Some((rx, tx))
    }
}

// ------------------------------------------------------------------- NVML --

#[repr(C)]
#[derive(Default)]
struct NvUtilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default)]
struct NvMemory {
    total: u64,
    free: u64,
    used: u64,
}

type Device = *mut core::ffi::c_void;
/// What `GetProcAddress` returns.
type Proc = unsafe extern "system" fn() -> isize;

/// NVIDIA's management library, loaded from System32 for as long as the worker runs.
struct Nvml {
    module: HMODULE,
    device: Device,
    shutdown: unsafe extern "C" fn() -> i32,
    temperature: unsafe extern "C" fn(Device, u32, *mut u32) -> i32,
    utilization: unsafe extern "C" fn(Device, *mut NvUtilization) -> i32,
    memory: unsafe extern "C" fn(Device, *mut NvMemory) -> i32,
}

impl Nvml {
    fn load() -> Option<Self> {
        unsafe {
            let module = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let found = (|| {
                let init: unsafe extern "C" fn() -> i32 = std::mem::transmute::<Proc, _>(GetProcAddress(module, s!("nvmlInit_v2"))?);
                let handle: unsafe extern "C" fn(u32, *mut Device) -> i32 = std::mem::transmute::<Proc, _>(GetProcAddress(module, s!("nvmlDeviceGetHandleByIndex_v2"))?);
                let shutdown: unsafe extern "C" fn() -> i32 = std::mem::transmute::<Proc, _>(GetProcAddress(module, s!("nvmlShutdown"))?);
                let temperature = std::mem::transmute::<Proc, unsafe extern "C" fn(Device, u32, *mut u32) -> i32>(GetProcAddress(module, s!("nvmlDeviceGetTemperature"))?);
                let utilization = std::mem::transmute::<Proc, unsafe extern "C" fn(Device, *mut NvUtilization) -> i32>(GetProcAddress(module, s!("nvmlDeviceGetUtilizationRates"))?);
                let memory = std::mem::transmute::<Proc, unsafe extern "C" fn(Device, *mut NvMemory) -> i32>(GetProcAddress(module, s!("nvmlDeviceGetMemoryInfo"))?);
                if init() != 0 {
                    return None;
                }
                let mut device: Device = std::ptr::null_mut();
                if handle(0, &mut device) != 0 {
                    shutdown();
                    return None;
                }
                Some(Self { module, device, shutdown, temperature, utilization, memory })
            })();
            if found.is_none() {
                let _ = FreeLibrary(module);
            }
            found
        }
    }

    /// Load (0..1), temperature (°C) and video memory (used, total) of the first GPU.
    fn read(&self) -> (Option<f32>, Option<f32>, Option<(u64, u64)>) {
        unsafe {
            let mut util = NvUtilization::default();
            let load = ((self.utilization)(self.device, &mut util) == 0).then(|| util.gpu.min(100) as f32 / 100.0);
            let mut celsius = 0u32;
            let temp = ((self.temperature)(self.device, 0, &mut celsius) == 0).then_some(celsius as f32);
            let mut mem = NvMemory::default();
            let vram = ((self.memory)(self.device, &mut mem) == 0 && mem.total > 0).then_some((mem.used, mem.total));
            (load, temp, vram)
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.module);
        }
    }
}

/// "1.4 MB/s", "820 KB/s", "0 B/s".
pub fn speed_text(bytes: f64) -> String {
    match bytes {
        b if b >= 1e9 => format!("{:.1} GB/s", b / 1e9),
        b if b >= 1e6 => format!("{:.1} MB/s", b / 1e6),
        b if b >= 1e3 => format!("{:.0} KB/s", b / 1e3),
        b => format!("{:.0} B/s", b.max(0.0)),
    }
}

/// "12.3 GB" (binary units, as Task Manager shows them).
pub fn gb_text(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_share_and_texts() {
        assert_eq!(busy_share(0, 0), 0.0);
        assert!((busy_share(75, 100) - 0.25).abs() < 1e-6);
        assert_eq!(busy_share(200, 100), 0.0);
        assert_eq!(speed_text(0.0), "0 B/s");
        assert_eq!(speed_text(820_000.0), "820 KB/s");
        assert_eq!(speed_text(1_400_000.0), "1.4 MB/s");
        assert_eq!(gb_text(12 << 30), "12.0 GB");
    }

    #[test]
    fn gpu_lines() {
        assert_eq!(parse_gpu_line("GPU 0.03 46 2279116800 12884901888"), Some((Some(0.03), Some(46.0), Some((2279116800, 12884901888)))));
        assert_eq!(parse_gpu_line("GPU 0.5 -1 -1 -1"), Some((Some(0.5), None, None)));
        assert_eq!(parse_gpu_line("GPU 0.5 46"), None);
        assert_eq!(parse_gpu_line("hello"), None);
    }

    #[test]
    fn history_shifts_and_fills() {
        let s = Stats::sample_data();
        assert_eq!(s.filled, HISTORY);
        assert!(s.cpu_hist.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!((s.mem() - s.mem_hist[HISTORY - 1]).abs() < 1e-6);
        let mut t = Stats { cpu: 0.5, ..Default::default() };
        t.push();
        assert_eq!((t.filled, t.cpu_hist[HISTORY - 1], t.cpu_hist[0]), (1, 0.5, 0.0));
    }

    /// Reads the real counters for two seconds (prints numbers only).
    #[test]
    #[ignore = "reads this PC's live load"]
    fn live_sample() {
        let mut cpu = CpuMeter::default();
        let mut net = NetMeter::default();
        cpu.sample();
        net.sample(0.0);
        std::thread::sleep(Duration::from_secs(2));
        let busy = cpu.sample();
        let (down, up) = net.sample(2.0);
        let (used, total) = memory();
        println!("cpu {:.1} %, memory {} / {}, down {} up {}", busy * 100.0, gb_text(used), gb_text(total), speed_text(down), speed_text(up));
        assert!((0.0..=1.0).contains(&busy) && total > 0 && used <= total);
        match Nvml::load() {
            Some(nvml) => {
                let (load, temp, vram) = nvml.read();
                println!("gpu load {load:?}, temp {temp:?}, vram {vram:?}");
                assert!(load.is_some());
            }
            None => println!("no NVML (not an NVIDIA card)"),
        }
    }
}
