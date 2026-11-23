//! Sounds: short interface cues and the Weather tab's ambience (rain, wind,
//! thunder, birds, crickets). Everything is synthesised here; there are no
//! sound files.
//!
//! A worker thread plays through WASAPI shared mode on the default output.
//! It exists only while something sounds: it starts with the first cue (or
//! ambience) and stops itself after `LINGER` seconds of silence, so at rest
//! nothing runs. Under `--hidden` nothing is ever played.

use std::f32::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;
use windows::core::Result;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// A sound to play once.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cue {
    /// A button.
    Tap,
    /// A tab or a view switcher.
    Tab,
    /// A switch (radio, theme, a setting).
    Toggle,
    /// The panel or command bar opening / closing.
    Open,
    Close,
    /// A toast: a soft two-note chime, or three rising notes for timers and questions.
    Chime,
    Alert,
    /// The character booped.
    Boop,
    /// A drop into water: the sky card clicked.
    Drop,
    /// Thunder, 0..1 (1 = close: a crack before the rumble).
    Thunder(f32),
    /// Droplets falling back into the pill (a notification's splash).
    Splash,
}

/// The sound settings (settings window, "Sounds").
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub ui: bool,
    pub weather: bool,
    pub ui_volume: f32,
    pub weather_volume: f32,
    /// 0 soft, 1 glassy (bells), 2 retro (square-wave blips).
    pub style: u8,
    /// Which interface events sound.
    pub clicks: bool,
    pub panel: bool,
    pub alerts: bool,
    pub character: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { ui: true, weather: true, ui_volume: 0.7, weather_volume: 0.7, style: 0, clicks: true, panel: true, alerts: true, character: true }
    }
}

impl Cue {
    /// Weather sounds follow the weather switch, the rest the interface switch.
    fn weather(self) -> bool {
        matches!(self, Cue::Drop | Cue::Thunder(_))
    }

    /// Whether the settings let this cue sound (volume aside).
    fn allowed(self, s: &Settings) -> bool {
        match self {
            Cue::Drop | Cue::Thunder(_) => s.weather,
            Cue::Tap | Cue::Tab | Cue::Toggle => s.ui && s.clicks,
            Cue::Open | Cue::Close => s.ui && s.panel,
            Cue::Chime | Cue::Alert | Cue::Splash => s.ui && s.alerts,
            Cue::Boop => s.ui && s.character,
        }
    }

    fn length(self) -> f32 {
        match self {
            Cue::Tap => 0.07,
            Cue::Tab => 0.1,
            Cue::Toggle => 0.2,
            Cue::Open | Cue::Close => 0.28,
            Cue::Chime => 1.3,
            Cue::Alert => 1.6,
            Cue::Boop => 0.22,
            Cue::Drop => 0.4,
            Cue::Thunder(_) => 6.0,
            Cue::Splash => 0.7,
        }
    }
}

/// How much of each ambient layer to play (0..1 each); all zero is silence.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Ambience {
    pub rain: f32,
    pub wind: f32,
    pub birds: f32,
    pub crickets: f32,
}

impl Ambience {
    pub const SILENT: Ambience = Ambience { rain: 0.0, wind: 0.0, birds: 0.0, crickets: 0.0 };

    fn quiet(&self) -> bool {
        self.rain + self.wind + self.birds + self.crickets < 0.001
    }
}

struct Shared {
    running: bool,
    cues: Vec<(Cue, f32)>,
    ambience: Ambience,
    ui_gain: f32,
    weather_gain: f32,
    style: u8,
    settings: Settings,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    running: false,
    cues: Vec::new(),
    ambience: Ambience::SILENT,
    ui_gain: 0.7,
    weather_gain: 0.7,
    style: 0,
    settings: Settings { ui: true, weather: true, ui_volume: 0.7, weather_volume: 0.7, style: 0, clicks: true, panel: true, alerts: true, character: true },
});
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Seconds of silence before the player thread lets the device go.
const LINGER: f32 = 12.0;

/// Allows playing at all (false under `--hidden`).
pub fn enable(on: bool) {
    ENABLED.store(on, Relaxed);
}

/// Applies the sound settings.
pub fn configure(settings: Settings) {
    if let Ok(mut shared) = SHARED.lock() {
        shared.ui_gain = if settings.ui { settings.ui_volume } else { 0.0 };
        shared.weather_gain = if settings.weather { settings.weather_volume } else { 0.0 };
        shared.style = settings.style;
        shared.settings = settings;
        if !settings.weather {
            shared.ambience = Ambience::SILENT;
        }
    }
}

pub fn cue(cue: Cue) {
    cue_after(cue, 0.0);
}

/// Plays `cue` in `delay` seconds.
pub fn cue_after(cue: Cue, delay: f32) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    if let Ok(mut shared) = SHARED.lock() {
        let gain = if cue.weather() { shared.weather_gain } else { shared.ui_gain };
        if gain <= 0.0 || !cue.allowed(&shared.settings) || shared.cues.len() >= 16 {
            return;
        }
        shared.cues.push((cue, delay.max(0.0)));
        wake(&mut shared);
    }
}

/// What the weather sounds like now (cross-faded over about a second).
pub fn ambience(ambience: Ambience) {
    if let Ok(mut shared) = SHARED.lock() {
        let ambience = if shared.weather_gain > 0.0 && ENABLED.load(Relaxed) { ambience } else { Ambience::SILENT };
        if shared.ambience == ambience {
            return;
        }
        shared.ambience = ambience;
        if !ambience.quiet() {
            wake(&mut shared);
        }
    }
}

fn wake(shared: &mut Shared) {
    if shared.running {
        return;
    }
    shared.running = true;
    let spawned = std::thread::Builder::new().name("islet-sound".into()).stack_size(256 * 1024).spawn(worker);
    if spawned.is_err() {
        shared.running = false;
        shared.cues.clear();
    }
}

fn worker() {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let mut failures = 0;
        loop {
            match play() {
                Ok(()) => break,
                Err(_) => {
                    // No device, or it went away (unplugged, default changed): try again a
                    // couple of times, then drop what was waiting.
                    failures += 1;
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if failures >= 3 {
                        if let Ok(mut shared) = SHARED.lock() {
                            shared.cues.clear();
                            shared.running = false;
                        }
                        break;
                    }
                }
            }
        }
        if com {
            CoUninitialize();
        }
    }
}

/// One stream on the default output, until it has been silent for `LINGER` s.
unsafe fn play() -> Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format_ptr = client.GetMixFormat()?;
        let format = format_ptr.read_unaligned();
        let (channels, rate, bits) = (format.nChannels.max(1) as usize, format.nSamplesPerSec.max(8000) as f32, format.wBitsPerSample);
        // 30 ms of buffer: short enough that a click sounds with the click.
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 300_000, 0, format_ptr, None);
        CoTaskMemFree(Some(format_ptr as *const _));
        init?;
        let event = CreateEventW(None, false, false, None)?;
        let result = (|| -> Result<()> {
            client.SetEventHandle(event)?;
            let render: IAudioRenderClient = client.GetService()?;
            let size = client.GetBufferSize()?;
            render.GetBuffer(size)?;
            render.ReleaseBuffer(size, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
            client.Start()?;
            let mut engine = Engine::new(rate);
            let mut mono = vec![0.0f32; size as usize];
            loop {
                // Woken when the device wants more (or every 200 ms, to check whether to stop).
                let _ = WaitForSingleObject(event, 200);
                if let Ok(mut shared) = SHARED.lock() {
                    engine.take(&mut shared);
                    if engine.idle > LINGER && shared.cues.is_empty() && shared.ambience.quiet() {
                        shared.running = false;
                        break;
                    }
                }
                let free = size - client.GetCurrentPadding()?;
                if free == 0 {
                    continue;
                }
                let frames = free as usize;
                engine.render(&mut mono[..frames]);
                let data = render.GetBuffer(free)?;
                for (frame, &v) in mono[..frames].iter().enumerate() {
                    for ch in 0..channels {
                        match bits {
                            16 => (data.add((frame * channels + ch) * 2) as *mut i16).write_unaligned((v * 32767.0) as i16),
                            _ => (data.add((frame * channels + ch) * 4) as *mut f32).write_unaligned(v),
                        }
                    }
                }
                render.ReleaseBuffer(free, 0)?;
            }
            let _ = client.Stop();
            Ok(())
        })();
        let _ = CloseHandle(event);
        result
    }
}

// ------------------------------------------------------------------ synthesis --

/// xorshift: noise and the dice of birds and drops.
struct Rng(u32);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 16_777_216.0
    }

    fn noise(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// One-pole low-pass.
#[derive(Default)]
struct Low(f32);

impl Low {
    fn run(&mut self, x: f32, hz: f32, rate: f32) -> f32 {
        self.0 += (1.0 - (-TAU * hz / rate).exp()) * (x - self.0);
        self.0
    }
}

/// State-variable band-pass (Chamberlin), for whooshes and wind.
#[derive(Default)]
struct Band {
    low: f32,
    band: f32,
}

impl Band {
    fn run(&mut self, x: f32, hz: f32, damping: f32, rate: f32) -> f32 {
        let f = 2.0 * (PI * hz.min(rate / 7.0) / rate).sin();
        self.low += f * self.band;
        let high = x - self.low - damping * self.band;
        self.band += f * high;
        self.band
    }
}

/// A cue being played.
struct Voice {
    cue: Cue,
    /// Seconds since it started (negative while it waits).
    t: f32,
    rng: Rng,
    band: Band,
    lows: [Low; 3],
    /// Oscillator phases: three per note slot (partials), three slots.
    phase: [f32; 9],
    /// 0 soft, 1 glassy, 2 retro.
    style: u8,
}

impl Voice {
    fn new(cue: Cue, delay: f32, seed: u32, style: u8) -> Self {
        Self { cue, t: -delay, rng: Rng(seed | 1), band: Band::default(), lows: Default::default(), phase: [0.0; 9], style }
    }

    /// Advances an oscillator: sine at `hz`.
    fn osc(&mut self, k: usize, hz: f32, rate: f32) -> f32 {
        self.phase[k] = (self.phase[k] + hz / rate).fract();
        (self.phase[k] * TAU).sin()
    }

    /// Advances an oscillator: square wave at `hz` (the retro style).
    fn square(&mut self, k: usize, hz: f32, rate: f32) -> f32 {
        self.phase[k] = (self.phase[k] + hz / rate).fract();
        if self.phase[k] < 0.5 {
            1.0
        } else {
            -1.0
        }
    }

    /// A struck note in slot `k` (0..3) starting at `at`, fading over `decay` s:
    /// soft = sine with a faint octave, glassy = a ringing bell, retro = a
    /// square wave held and then cut.
    fn note(&mut self, k: usize, at: f32, hz: f32, decay: f32, rate: f32) -> f32 {
        let since = self.t - at;
        if since < 0.0 {
            return 0.0;
        }
        let attack = 1.0 - (-since / 0.004).exp();
        match self.style {
            1 => {
                let tone = self.osc(k * 3, hz, rate) + 0.45 * self.osc(k * 3 + 1, hz * 2.76, rate) * (-since / (decay * 0.6)).exp() + 0.2 * self.osc(k * 3 + 2, hz * 5.4, rate) * (-since / (decay * 0.3)).exp();
                tone * attack * (-since / (decay * 2.4)).exp() * 0.8
            }
            2 => {
                let gate = (-(since - decay * 1.6).max(0.0) / 0.006).exp();
                self.square(k * 3, hz, rate) * 0.38 * gate * (1.0 - (-since / 0.001).exp())
            }
            _ => {
                let tone = self.osc(k * 3, hz, rate) + 0.22 * self.osc(k * 3 + 1, hz * 2.01, rate);
                tone * attack * (-since / decay).exp()
            }
        }
    }

    /// A droplet: a short blip whose pitch jumps up as it rings out.
    fn plip(&mut self, k: usize, at: f32, base: f32, rate: f32) -> f32 {
        let s = self.t - at;
        if s < 0.0 {
            return 0.0;
        }
        let hz = base * (1.0 + 1.3 * (1.0 - (-s / 0.012).exp()));
        let wave = if self.style == 2 { self.square(k, hz, rate) * 0.45 } else { self.osc(k, hz, rate) };
        wave * (1.0 - (-s / 0.002).exp()) * (-s / 0.035).exp()
    }

    /// Notes in turn, `step` s apart, cycling through the three slots.
    fn run_of(&mut self, notes: &[f32], step: f32, decay: f32, rate: f32) -> f32 {
        let mut out = 0.0;
        for (i, &hz) in notes.iter().enumerate() {
            out += self.note(i % 3, i as f32 * step, hz, decay, rate);
        }
        out
    }

    fn sample(&mut self, rate: f32) -> f32 {
        let t = self.t;
        self.t += 1.0 / rate;
        if t < 0.0 {
            return 0.0;
        }
        let len = self.cue.length();
        let style = self.style;
        match self.cue {
            Cue::Tap => match style {
                1 => self.note(0, 0.0, 2350.0, 0.025, rate) * 0.12,
                2 => self.note(0, 0.0, 1320.0, 0.012, rate) * 0.1,
                _ => {
                    let hz = 1150.0 + 650.0 * (-t / 0.006).exp();
                    self.osc(0, hz, rate) * (-t / 0.012).exp() * 0.16 + self.rng.noise() * (-t / 0.0015).exp() * 0.05
                }
            },
            Cue::Tab => match style {
                1 => self.note(0, 0.0, 1760.0, 0.035, rate) * 0.11,
                2 => (self.note(0, 0.0, 880.0, 0.014, rate) + self.note(1, 0.03, 1320.0, 0.016, rate)) * 0.09,
                _ => {
                    let hz = 900.0 + 260.0 * (-t / 0.01).exp();
                    let body = self.osc(0, hz, rate) + 0.25 * self.osc(1, hz * 2.0, rate);
                    body * (-t / 0.024).exp() * 0.12
                }
            },
            Cue::Toggle => {
                let (pitch, gain) = match style {
                    1 => (2.0, 0.11),
                    2 => (1.0, 0.09),
                    _ => (1.0, 0.12),
                };
                (self.note(0, 0.0, 660.0 * pitch, 0.03, rate) + self.note(1, 0.07, 990.0 * pitch, 0.04, rate)) * gain
            }
            Cue::Open | Cue::Close => {
                let u = t / len;
                let open = self.cue == Cue::Open;
                if style == 2 {
                    // A chip sweep up (open) or down (close).
                    let hz = if open { 300.0 * 3.0f32.powf(u) } else { 900.0 / 3.0f32.powf(u) };
                    return self.square(0, hz, rate) * (PI * u).sin() * 0.06;
                }
                let (from, to): (f32, f32) = if open { (450.0, 1700.0) } else { (1500.0, 380.0) };
                let centre = from * (to / from).powf(u);
                let noise = self.rng.noise();
                let air = self.band.run(noise, centre, 0.6, rate);
                let swell = (PI * u).sin().powi(2);
                if style == 1 {
                    // Air with a little run of bells on top.
                    let notes: [f32; 3] = if open { [1046.5, 1318.5, 1568.0] } else { [1568.0, 1318.5, 1046.5] };
                    return air * swell * 0.06 + self.run_of(&notes, 0.055, 0.06, rate) * 0.06;
                }
                let glide = if open { 520.0 + 260.0 * u } else { 720.0 - 240.0 * u };
                air * swell * 0.11 + self.osc(0, glide, rate) * swell * (-t / 0.12).exp() * 0.035
            }
            Cue::Chime => match style {
                1 => (self.note(0, 0.0, 1568.0, 0.22, rate) + self.note(1, 0.09, 2349.0, 0.25, rate)) * 0.07,
                2 => self.run_of(&[1046.5, 1318.5, 1568.0, 2093.0], 0.06, 0.03, rate) * 0.07,
                _ => (self.note(0, 0.0, 1318.5, 0.26, rate) + self.note(1, 0.085, 1975.5, 0.3, rate)) * 0.075,
            },
            Cue::Alert => match style {
                1 => (self.note(0, 0.0, 1046.5, 0.3, rate) + self.note(1, 0.13, 1318.5, 0.3, rate) + self.note(2, 0.26, 1568.0, 0.4, rate)) * 0.075,
                2 => self.run_of(&[1568.0, 2093.0, 1568.0, 2093.0, 1568.0, 2093.0], 0.08, 0.035, rate) * 0.07,
                _ => (self.note(0, 0.0, 1046.5, 0.32, rate) + self.note(1, 0.12, 1318.5, 0.32, rate) + self.note(2, 0.24, 1568.0, 0.4, rate)) * 0.085,
            },
            Cue::Boop => match style {
                1 => self.note(0, 0.0, 1175.0, 0.05, rate) * 0.12 + self.note(1, 0.05, 1760.0, 0.05, rate) * 0.08,
                2 => {
                    let hz = 400.0 + 600.0 * (t / 0.08).min(1.0);
                    let gate = if t < 0.12 { 1.0 } else { (-(t - 0.12) / 0.006).exp() };
                    self.square(0, hz, rate) * gate * 0.07
                }
                _ => {
                    let hz = 520.0 + 380.0 * (t / 0.05).min(1.0) - 120.0 * ((t - 0.05).max(0.0) / 0.15);
                    self.osc(0, hz, rate) * (1.0 - (-t / 0.006).exp()) * (-t / 0.06).exp() * 0.15
                }
            },
            Cue::Splash => (self.plip(0, 0.0, 950.0, rate) + self.plip(1, 0.09, 1250.0, rate) + self.plip(2, 0.17, 1080.0, rate) + self.plip(3, 0.29, 1420.0, rate)) * 0.07,
            Cue::Drop => {
                // The pitch of a drop rises as its bubble rings out.
                let hz = 380.0 + 950.0 * (1.0 - (-t / 0.025).exp());
                let main = self.osc(0, hz, rate) * (1.0 - (-t / 0.002).exp()) * (-t / 0.07).exp() * 0.16;
                let echo = if t > 0.11 { self.osc(1, hz * 1.5, rate) * (-(t - 0.11) / 0.05).exp() * 0.04 } else { 0.0 };
                main + echo
            }
            Cue::Thunder(strength) => {
                let n = self.rng.noise();
                // The crack: bright noise, only when it is close.
                let bright = n - self.lows[0].run(n, 1200.0, rate);
                let crack = bright * (-t / 0.06).exp() * 0.35 * (strength - 0.5).max(0.0) * 2.0;
                // The rumble: deep noise rolling in and out.
                let low = self.lows[1].run(n, 110.0, rate);
                let deep = self.lows[2].run(low, 160.0, rate) * 9.0;
                let roll = 0.55 + 0.45 * (t * 2.3 + (t * 0.7).sin() * 2.0).sin();
                let envelope = (1.0 - (-t / 0.09).exp()) * (-t / (1.2 + strength)).exp();
                crack + deep * roll * envelope * (0.35 + 0.35 * strength)
            }
        }
    }

    fn done(&self) -> bool {
        // Bells ring on for longer.
        self.t > self.cue.length() * if self.style == 1 && !self.cue.weather() { 2.0 } else { 1.0 }
    }
}

/// A rain drop hitting something: a tiny ping.
#[derive(Clone, Copy, Default)]
struct Ping {
    t: f32,
    hz: f32,
    amp: f32,
    decay: f32,
    phase: f32,
}

/// The weather's continuous layers.
struct Ambient {
    level: Ambience,
    rng: Rng,
    hiss: [Low; 3],
    gust: Band,
    wind_low: Low,
    pings: [Ping; 10],
    t: f32,
    /// Birds: when the next phrase starts, and the phrase being sung
    /// (syllables left, start of this one, its pitch and sweep).
    next_song: f32,
    song: Option<(u32, f32, f32, f32)>,
    bird_phase: f32,
    cricket_phase: [f32; 2],
}

impl Ambient {
    fn new() -> Self {
        Self { level: Ambience::SILENT, rng: Rng(0x2545_F491), hiss: Default::default(), gust: Band::default(), wind_low: Low::default(), pings: [Ping::default(); 10], t: 0.0, next_song: 1.0, song: None, bird_phase: 0.0, cricket_phase: [0.0; 2] }
    }

    fn sample(&mut self, target: &Ambience, rate: f32) -> f32 {
        let dt = 1.0 / rate;
        self.t += dt;
        let t = self.t;
        // Each layer fades towards its target over about a second.
        let k = dt / 1.1;
        let l = &mut self.level;
        for (now, to) in [(&mut l.rain, target.rain), (&mut l.wind, target.wind), (&mut l.birds, target.birds), (&mut l.crickets, target.crickets)] {
            *now += (to - *now) * k;
        }
        let level = self.level;
        let mut out = 0.0;

        if level.rain > 0.001 {
            let n = self.rng.noise();
            let hiss = n - self.hiss[0].run(n, 350.0, rate);
            let hiss = self.hiss[1].run(hiss, 6000.0, rate);
            let roar = self.hiss[2].run(n, 700.0, rate);
            out += (hiss * 0.07 + roar * 0.12 * level.rain) * level.rain;
            // Drops landing close by.
            if self.rng.unit() < (25.0 + 160.0 * level.rain) * dt {
                if let Some(slot) = self.pings.iter_mut().find(|p| p.amp == 0.0) {
                    *slot = Ping { t: 0.0, hz: 1700.0 + self.rng.unit() * 3600.0, amp: (0.01 + self.rng.unit() * 0.03) * level.rain, decay: 0.003 + self.rng.unit() * 0.007, phase: 0.0 };
                }
            }
            for p in self.pings.iter_mut().filter(|p| p.amp > 0.0) {
                p.phase = (p.phase + p.hz * dt).fract();
                out += (p.phase * TAU).sin() * p.amp * (-p.t / p.decay).exp();
                p.t += dt;
                if p.t > p.decay * 8.0 {
                    p.amp = 0.0;
                }
            }
        }

        if level.wind > 0.001 {
            let n = self.rng.noise();
            let gust = 0.5 + 0.5 * ((TAU * 0.11 * t).sin() * 0.6 + (TAU * 0.29 * t + 1.0).sin() * 0.4);
            let centre = 230.0 + 260.0 * (0.5 + 0.5 * (TAU * 0.06 * t).sin()) + 220.0 * gust;
            let howl = self.gust.run(n, centre, 0.35, rate);
            let body = self.wind_low.run(n, 260.0, rate);
            out += (howl * 0.09 + body * 0.18) * level.wind * (0.3 + 0.7 * gust);
        }

        if level.birds > 0.001 {
            if self.song.is_none() && t >= self.next_song {
                let syllables = 2 + (self.rng.unit() * 4.0) as u32;
                let pitch = 2600.0 + self.rng.unit() * 1800.0;
                let sweep = (self.rng.unit() - 0.4) * 0.8;
                self.song = Some((syllables, t, pitch, sweep));
            }
            if let Some((left, start, pitch, sweep)) = self.song {
                const SYLLABLE: f32 = 0.075;
                let u = (t - start) / SYLLABLE;
                if u < 1.0 {
                    let hz = pitch * (1.0 + sweep * u) * (1.0 + 0.03 * (t * 90.0).sin());
                    self.bird_phase = (self.bird_phase + hz * dt).fract();
                    out += (self.bird_phase * TAU).sin() * (PI * u).sin().powi(2) * 0.03 * level.birds;
                } else if u > 1.6 {
                    // Next syllable after a short gap, a little different each time.
                    self.song = if left > 1 { Some((left - 1, t, pitch * (0.92 + self.rng.unit() * 0.16), sweep)) } else { None };
                    if left <= 1 {
                        self.next_song = t + 1.4 + self.rng.unit() * 4.5;
                    }
                }
            }
        }

        if level.crickets > 0.001 {
            // Two crickets, each three quick pulses at its own pace and pitch.
            for (i, (period, hz)) in [(0.82f32, 4400.0f32), (1.07, 4750.0)].into_iter().enumerate() {
                let local = (t + i as f32 * 0.37) % period;
                self.cricket_phase[i] = (self.cricket_phase[i] + hz * dt).fract();
                if local < 0.12 {
                    let pulse = (local / 0.04).fract();
                    let shape = if pulse < 0.65 { (PI * pulse / 0.65).sin() } else { 0.0 };
                    out += (self.cricket_phase[i] * TAU).sin() * shape * 0.016 * level.crickets;
                }
            }
        }
        out
    }
}

/// Mixes the cues and the ambience.
pub struct Engine {
    rate: f32,
    voices: Vec<(Voice, f32)>,
    ambient: Ambient,
    target: Ambience,
    weather_gain: f32,
    ui_gain: f32,
    seed: u32,
    /// Seconds the output has been silent.
    idle: f32,
}

impl Engine {
    pub fn new(rate: f32) -> Self {
        Self { rate, voices: Vec::with_capacity(16), ambient: Ambient::new(), target: Ambience::SILENT, weather_gain: 0.0, ui_gain: 0.0, seed: 0x9E37_79B9, idle: 0.0 }
    }

    fn take(&mut self, shared: &mut Shared) {
        self.target = shared.ambience;
        self.weather_gain = shared.weather_gain;
        self.ui_gain = shared.ui_gain;
        for (cue, delay) in shared.cues.drain(..) {
            self.seed = self.seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
            let gain = if cue.weather() { shared.weather_gain } else { shared.ui_gain };
            if self.voices.len() < 16 {
                self.voices.push((Voice::new(cue, delay, self.seed, shared.style), gain));
            }
        }
    }

    pub fn render(&mut self, out: &mut [f32]) {
        let rate = self.rate;
        let ambient_on = !self.target.quiet() || !self.ambient.level.quiet();
        for v in out.iter_mut() {
            let mut mix = 0.0;
            for (voice, gain) in &mut self.voices {
                mix += voice.sample(rate) * *gain;
            }
            if ambient_on {
                mix += self.ambient.sample(&self.target, rate) * self.weather_gain;
            }
            // Soft limit: transparent at these levels, no clipping when several overlap.
            *v = mix.tanh();
        }
        self.voices.retain(|(voice, _)| !voice.done());
        let silent = self.voices.is_empty() && self.target.quiet() && self.ambient.level.quiet();
        self.idle = if silent { self.idle + out.len() as f32 / rate } else { 0.0 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_styled(cues: &[Cue], ambience: Ambience, style: u8) -> Engine {
        let mut engine = Engine::new(48_000.0);
        let mut shared = Shared { running: true, cues: cues.iter().map(|&c| (c, 0.0)).collect(), ambience, ui_gain: 1.0, weather_gain: 1.0, style, settings: Settings::default() };
        engine.take(&mut shared);
        engine
    }

    fn engine_with(cues: &[Cue], ambience: Ambience) -> Engine {
        engine_styled(cues, ambience, 0)
    }

    /// Every cue, in every style, makes a sound, stays in range and ends.
    #[test]
    fn cues_sound_and_end() {
        for style in 0..3 {
            for cue in [Cue::Tap, Cue::Tab, Cue::Toggle, Cue::Open, Cue::Close, Cue::Chime, Cue::Alert, Cue::Boop, Cue::Splash, Cue::Drop, Cue::Thunder(1.0), Cue::Thunder(0.3)] {
                let mut engine = engine_styled(&[cue], Ambience::SILENT, style);
                let mut buf = vec![0.0f32; 48_000 * 7];
                engine.render(&mut buf);
                let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(buf.iter().all(|v| v.is_finite()), "{cue:?} style {style} not finite");
                assert!(peak > 0.01 && peak < 0.9, "{cue:?} style {style} peak {peak}");
                assert!(engine.voices.is_empty(), "{cue:?} style {style} did not end");
            }
        }
    }

    /// The event switches silence their cues, and only those.
    #[test]
    fn switches() {
        let s = Settings { panel: false, character: false, ..Settings::default() };
        assert!(!Cue::Open.allowed(&s) && !Cue::Boop.allowed(&s) && Cue::Tap.allowed(&s) && Cue::Splash.allowed(&s));
        let s = Settings { ui: false, ..Settings::default() };
        assert!(!Cue::Tap.allowed(&s) && !Cue::Alert.allowed(&s) && Cue::Thunder(1.0).allowed(&s));
        let s = Settings { weather: false, ..Settings::default() };
        assert!(!Cue::Drop.allowed(&s) && Cue::Tab.allowed(&s));
    }

    /// Each ambient layer is audible, bounded, and fades out to silence.
    #[test]
    fn ambience_layers() {
        for layer in 0..4 {
            let mut a = Ambience::SILENT;
            *[&mut a.rain, &mut a.wind, &mut a.birds, &mut a.crickets][layer] = 1.0;
            let mut engine = engine_with(&[], a);
            let mut buf = vec![0.0f32; 48_000 * 8];
            engine.render(&mut buf);
            let rms = (buf[48_000 * 3..].iter().map(|v| v * v).sum::<f32>() / (48_000.0 * 5.0)).sqrt();
            let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(rms > 0.0005 && peak < 0.8, "layer {layer}: rms {rms} peak {peak}");
            engine.target = Ambience::SILENT;
            let mut tail = vec![0.0f32; 48_000 * 12];
            engine.render(&mut tail);
            assert!(tail[tail.len() - 4800..].iter().all(|v| v.abs() < 0.002), "layer {layer} did not fade");
        }
    }

    /// Rendering a second of a storm with a thunder costs far less than a second.
    #[test]
    fn cheap_enough() {
        let mut engine = engine_with(&[Cue::Thunder(1.0)], Ambience { rain: 1.0, wind: 0.6, birds: 0.0, crickets: 0.0 });
        let mut buf = vec![0.0f32; 480];
        let start = std::time::Instant::now();
        for _ in 0..100 {
            engine.render(&mut buf);
        }
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        println!("1 s of storm rendered in {ms:.2} ms");
        assert!(ms < 40.0);
    }

    /// The player thread starts with a cue and lets the device go after
    /// `LINGER` s of silence. Played at 1/10000 of the volume: inaudible.
    #[test]
    #[ignore = "opens the audio device"]
    fn live_player_lifecycle() {
        let running = || SHARED.lock().unwrap().running;
        enable(true);
        configure(Settings { ui_volume: 0.0001, weather_volume: 0.0001, ..Settings::default() });
        cue(Cue::Tap);
        assert!(running());
        ambience(Ambience { rain: 1.0, wind: 0.5, birds: 0.0, crickets: 0.0 });
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(running());
        ambience(Ambience::SILENT);
        // The ambience fades out (~8 s), then the silence has to last LINGER.
        std::thread::sleep(std::time::Duration::from_secs_f32(LINGER + 11.0));
        assert!(!running(), "the player did not stop");
        enable(false);
        cue(Cue::Tap);
        assert!(!running(), "played while disabled");
    }

    /// Opens the real default output and plays half a second of silence (nothing
    /// audible): checks the device path.
    #[test]
    #[ignore = "opens the audio device"]
    fn live_device() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
            let format = client.GetMixFormat().unwrap();
            let f = format.read_unaligned();
            let (channels, rate, bits) = (f.nChannels, f.nSamplesPerSec, f.wBitsPerSample);
            println!("{channels} channels, {rate} Hz, {bits} bits");
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 300_000, 0, format, None).unwrap();
            CoTaskMemFree(Some(format as *const _));
            let render: IAudioRenderClient = client.GetService().unwrap();
            let size = client.GetBufferSize().unwrap();
            render.GetBuffer(size).unwrap();
            render.ReleaseBuffer(size, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32).unwrap();
            client.Start().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            client.Stop().unwrap();
        }
    }
}
