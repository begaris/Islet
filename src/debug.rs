//! Process statistics for the `--debug` HUD.

use std::time::Instant;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

pub struct Stats {
    pub working_set_mb: f32,
    pub private_mb: f32,
    /// Percent of one core since the previous sample.
    pub cpu: f32,
    /// Smoothed CPU-side time to build and submit a frame, in milliseconds.
    pub frame_ms: f32,
    pub frames: u32,
    last_cpu: u64,
    last_at: Instant,
}

impl Stats {
    pub fn new() -> Self {
        Self { working_set_mb: 0.0, private_mb: 0.0, cpu: 0.0, frame_ms: 0.0, frames: 0, last_cpu: cpu_time_100ns(), last_at: Instant::now() }
    }

    pub fn sample(&mut self) {
        let mut pmc = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        unsafe {
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, size).is_ok() {
                self.working_set_mb = pmc.WorkingSetSize as f32 / 1_048_576.0;
                self.private_mb = pmc.PrivateUsage as f32 / 1_048_576.0;
            }
        }
        let now = Instant::now();
        let cpu = cpu_time_100ns();
        let wall = now.duration_since(self.last_at).as_secs_f32();
        if wall > 0.05 {
            self.cpu = (cpu - self.last_cpu) as f32 / 1e7 / wall * 100.0;
            self.last_cpu = cpu;
            self.last_at = now;
        }
    }

    pub fn frame(&mut self, ms: f32) {
        self.frames = self.frames.wrapping_add(1);
        self.frame_ms = if self.frame_ms == 0.0 { ms } else { self.frame_ms * 0.9 + ms * 0.1 };
    }
}

fn cpu_time_100ns() -> u64 {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).is_err() {
            return 0;
        }
    }
    let wide = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    wide(k) + wide(u)
}
