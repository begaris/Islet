//! Voice to text: press the shortcut, speak, and the words are typed into the
//! app you are in.
//!
//! Two engines, both off until `voice = local | api` in the settings file:
//! - `local`: the Windows desktop speech recognizer (SAPI dictation), offline;
//!   this PC has English (US/UK) only.
//! - `api`: the microphone is recorded and sent to an OpenAI-compatible
//!   `/audio/transcriptions` (Whisper and similar; any language), or a local
//!   server on `http://localhost`.
//!
//! Either way the listening happens in a helper copy of Islet
//! (`islet.exe --listen ...`): the speech engine's memory leaves with it. The
//! helper prints `LISTENING`, `PARTIAL <words>`, `TRANSCRIBING`, then
//! `TEXT <words>` or `ERROR <reason>`, and stops on a line on stdin.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_VOICE: u32 = WM_APP + 24;

/// After words were heard, this much quiet ends the dictation.
const QUIET: Duration = Duration::from_millis(1800);
/// Nothing heard for this long: give up.
const NOTHING: Duration = Duration::from_secs(8);
const LONGEST: Duration = Duration::from_secs(120);

// ------------------------------------------------------------- Islet's side --

#[derive(Clone, PartialEq, Debug)]
pub enum Update {
    Listening,
    /// Words so far (local engine only).
    Partial(String),
    /// Recorded; waiting for the service.
    Transcribing,
    /// The result (may be empty: nothing was heard).
    Text(String),
    Failed(String),
}

static UPDATES: Mutex<Vec<Update>> = Mutex::new(Vec::new());
static HELPER: Mutex<Option<std::process::ChildStdin>> = Mutex::new(None);

pub fn take() -> Vec<Update> {
    UPDATES.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

fn report(target: isize, update: Update) {
    if let Ok(mut list) = UPDATES.lock() {
        list.push(update);
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_VOICE, WPARAM(0), LPARAM(0));
    }
}

/// How the helper should listen.
pub enum Engine {
    Local,
    /// Base address, model, language ("" = detect), API key ("" for a local server).
    Api { url: String, model: String, language: String, key: String },
}

/// Starts the helper. The key goes over stdin, never on the command line.
pub fn start(target: HWND, engine: Engine) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut command = std::process::Command::new(exe);
    command.arg("--listen");
    let key = match &engine {
        Engine::Local => {
            command.arg("local");
            None
        }
        Engine::Api { url, model, language, key } => {
            command.args(["api", url, model, if language.is_empty() { "-" } else { language }]);
            Some(key.clone())
        }
    };
    let child = command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).creation_flags(CREATE_NO_WINDOW).spawn();
    let Ok(mut child) = child else { return false };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return false };
    if let Some(key) = key {
        let _ = writeln!(stdin, "{key}");
    }
    if let Ok(mut helper) = HELPER.lock() {
        *helper = Some(stdin);
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-voice".into()).stack_size(128 * 1024).spawn(move || {
        let mut ended = false;
        for line in std::io::BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            let update = match line.split_once(' ').unwrap_or((line.as_str(), "")) {
                ("LISTENING", _) => Update::Listening,
                ("TRANSCRIBING", _) => Update::Transcribing,
                ("PARTIAL", words) => Update::Partial(words.to_string()),
                ("TEXT", words) => Update::Text(words.to_string()),
                ("ERROR", why) => Update::Failed(why.to_string()),
                _ => continue,
            };
            ended |= matches!(update, Update::Text(_) | Update::Failed(_));
            report(target, update);
        }
        let _ = child.wait();
        if let Ok(mut helper) = HELPER.lock() {
            *helper = None;
        }
        if !ended {
            report(target, Update::Failed("The listener stopped unexpectedly".into()));
        }
    });
    true
}

/// Ends the listening (what was said so far is still turned into text).
pub fn stop() {
    if let Ok(mut helper) = HELPER.lock() {
        if let Some(mut stdin) = helper.take() {
            let _ = stdin.write_all(b"stop\n");
        }
    }
}

/// Types `text` into whatever window has the keyboard (Unicode key events:
/// the clipboard is left alone).
pub fn type_text(text: &str) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let mut inputs = Vec::new();
    for unit in text.replace(['\r', '\n'], " ").encode_utf16() {
        for up in [false, true] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE }, time: 0, dwExtraInfo: 0 } },
            });
        }
    }
    for chunk in inputs.chunks(200) {
        unsafe { SendInput(chunk, size_of::<INPUT>() as i32) };
    }
}

/// `true` while Ctrl, Alt, Shift or Win is held (typing then would trigger shortcuts).
pub fn modifiers_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].iter().any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } < 0)
}

// ------------------------------------------------------------ helper's side --

/// Turns a Windows error into "what went wrong (why)".
fn fail(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what} ({})", e.message().trim_end_matches(['.', '\r', '\n']))
}

fn say(text: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", text.replace(['\r', '\n'], " "));
    let _ = out.flush();
}

/// The helper process: `--listen local [--file <wav>]` or `--listen api <url> <model> <language|->`.
pub fn helper(args: &[String]) -> i32 {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let mode = args.first().map(String::as_str).unwrap_or("local");
    let key = if mode == "api" {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        line.trim().to_string()
    } else {
        String::new()
    };
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        flag.store(true, Relaxed);
    });
    let result = match mode {
        "api" => {
            let get = |i: usize| args.get(i).cloned().unwrap_or_default();
            let language = get(3);
            listen_api(&get(1), &get(2), if language == "-" { "" } else { &language }, &key, &stop)
        }
        _ => {
            let file = args.iter().position(|a| a == "--file").and_then(|i| args.get(i + 1)).map(Path::new);
            listen_local(file, &stop)
        }
    };
    match result {
        Ok(text) => {
            say(&format!("TEXT {}", text.trim()));
            0
        }
        Err(why) => {
            say(&format!("ERROR {why}"));
            1
        }
    }
}

/// Windows desktop speech recognition (SAPI) with the dictation grammar, from
/// the default microphone or a WAV file.
fn listen_local(file: Option<&Path>, stop: &AtomicBool) -> Result<String, String> {
    use windows::core::{Interface, IUnknown, HSTRING, PCWSTR};
    use windows::Win32::Media::Speech::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
    const WHOLE: u32 = u32::MAX;
    unsafe {
        let recognizer: ISpRecognizer = CoCreateInstance(&SpInprocRecognizer, None, CLSCTX_ALL).map_err(fail("Windows speech recognition is not available"))?;
        let input: IUnknown = match file {
            Some(path) => {
                let stream: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL).map_err(fail("Speech input failed"))?;
                stream.BindToFile(&HSTRING::from(path.as_os_str()), SPFM_OPEN_READONLY, None, None, 0).map_err(fail("The sound file could not be read"))?;
                stream.cast().map_err(fail("Speech input failed"))?
            }
            None => CoCreateInstance(&SpMMAudioIn, None, CLSCTX_ALL).map_err(fail("No microphone"))?,
        };
        recognizer.SetInput(&input, true).map_err(fail("The microphone could not be opened"))?;
        let context = recognizer.CreateRecoContext().map_err(fail("No speech recognizer for the language of this PC"))?;
        let interest = |events: &[SPEVENTENUM]| events.iter().fold((1u64 << SPEI_RESERVED1.0) | (1u64 << SPEI_RESERVED2.0), |mask, e| mask | 1u64 << e.0);
        let wanted = interest(&[SPEI_RECOGNITION, SPEI_HYPOTHESIS, SPEI_END_SR_STREAM]);
        context.SetInterest(wanted, wanted).map_err(fail("Speech recognition failed"))?;
        context.SetNotifyWin32Event().map_err(fail("Speech recognition failed"))?;
        let grammar = context.CreateGrammar(0).map_err(fail("Speech recognition failed"))?;
        grammar.LoadDictation(PCWSTR::null(), SPLO_STATIC).map_err(fail("Dictation is not available"))?;
        grammar.SetDictationState(SPRS_ACTIVE).map_err(fail("Dictation is not available"))?;
        let _ = recognizer.SetRecoState(SPRST_ACTIVE);
        say("LISTENING");

        let mut text = String::new();
        let (begin, mut last_words) = (Instant::now(), None::<Instant>);
        let mut ended = false;
        while !ended && !stop.load(Relaxed) && begin.elapsed() < LONGEST {
            let _ = context.WaitForNotifyEvent(100);
            loop {
                let mut event = SPEVENT::default();
                let mut fetched = 0u32;
                if context.GetEvents(1, &mut event, &mut fetched).is_err() || fetched == 0 {
                    break;
                }
                let (id, kind) = (event._bitfield & 0xffff, event._bitfield >> 16);
                // Each event owns what its lParam points to: take it over so it is freed.
                let result: Option<ISpRecoResult> = match SPEVENTLPARAMTYPE(kind) {
                    SPET_LPARAM_IS_OBJECT if event.lParam.0 != 0 => IUnknown::from_raw(event.lParam.0 as *mut _).cast().ok(),
                    SPET_LPARAM_IS_POINTER | SPET_LPARAM_IS_STRING if event.lParam.0 != 0 => {
                        CoTaskMemFree(Some(event.lParam.0 as *const _));
                        None
                    }
                    _ => None,
                };
                let words = || {
                    let mut out = windows::core::PWSTR::null();
                    result.as_ref()?.GetText(WHOLE, WHOLE, true, &mut out, None).ok()?;
                    let words = out.to_string().ok();
                    CoTaskMemFree(Some(out.0 as *const _));
                    words
                };
                if id == SPEI_RECOGNITION.0 {
                    if let Some(words) = words() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(words.trim());
                        last_words = Some(Instant::now());
                        say(&format!("PARTIAL {text}"));
                    }
                } else if id == SPEI_HYPOTHESIS.0 {
                    if let Some(words) = words() {
                        last_words = Some(Instant::now());
                        say(&format!("PARTIAL {}{}{}", text, if text.is_empty() { "" } else { " " }, words.trim()));
                    }
                } else if id == SPEI_END_SR_STREAM.0 {
                    ended = true;
                }
            }
            // From the microphone: stop after a pause that follows words, or when nothing comes.
            if file.is_none() {
                match last_words {
                    Some(at) if !text.is_empty() && at.elapsed() > QUIET => break,
                    None if begin.elapsed() > NOTHING => break,
                    _ => {}
                }
            }
        }
        let _ = recognizer.SetRecoState(SPRECOSTATE(0));
        Ok(text)
    }
}

// ----------------------------------------------------------------- API mode --

/// 16 kHz mono 16-bit samples as a WAV file.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// A multipart/form-data body: text fields, then the WAV as `file`.
pub fn multipart(boundary: &str, fields: &[(&str, &str)], audio: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(audio.len() + 512);
    for (name, value) in fields.iter().filter(|(_, v)| !v.is_empty()) {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes());
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Turns captured frames (any rate and channel count) into 16 kHz mono and
/// keeps track of speech: `push` returns the loudness of what came in.
#[derive(Default)]
pub struct Recorder {
    pub samples: Vec<i16>,
    /// Source rate and the fractional position of the next output sample.
    rate: f32,
    phase: f32,
    sum: f32,
    count: f32,
}

impl Recorder {
    pub fn new(rate: u32) -> Recorder {
        Recorder { rate: rate.max(8000) as f32, ..Default::default() }
    }

    /// Mono samples in -1..1 at the source rate; returns their RMS.
    pub fn push(&mut self, mono: &[f32]) -> f32 {
        let step = 16_000.0 / self.rate;
        let mut energy = 0.0;
        for &s in mono {
            energy += s * s;
            // Averages the source samples that fall into each output sample.
            self.sum += s;
            self.count += 1.0;
            self.phase += step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.samples.push(((self.sum / self.count).clamp(-1.0, 1.0) * 32767.0) as i16);
                (self.sum, self.count) = (0.0, 0.0);
            }
        }
        (energy / mono.len().max(1) as f32).sqrt()
    }
}

/// Records the default microphone until a pause after speech (or `stop`), then
/// asks the transcription service.
fn listen_api(url: &str, model: &str, language: &str, key: &str, stop: &AtomicBool) -> Result<String, String> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
    let recorder = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(fail("No audio"))?;
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications).map_err(fail("No microphone"))?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(fail("The microphone could not be opened"))?;
        let format_ptr = client.GetMixFormat().map_err(fail("The microphone could not be opened"))?;
        let format = format_ptr.read_unaligned();
        let (channels, bits, block) = (format.nChannels.max(1) as usize, format.wBitsPerSample, format.nBlockAlign as usize);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, format_ptr, None);
        CoTaskMemFree(Some(format_ptr as *const _));
        init.map_err(fail("The microphone could not be opened"))?;
        let capture: IAudioCaptureClient = client.GetService().map_err(fail("The microphone could not be opened"))?;
        client.Start().map_err(fail("The microphone could not be opened"))?;
        say("LISTENING");
        let mut recorder = Recorder::new(format.nSamplesPerSec);
        let begin = Instant::now();
        let (mut floor, mut heard, mut last_loud) = (0.0f32, false, Instant::now());
        let mut mono = Vec::new();
        while !stop.load(Relaxed) && begin.elapsed() < LONGEST {
            std::thread::sleep(Duration::from_millis(30));
            while capture.GetNextPacketSize().unwrap_or(0) > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                if capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                mono.clear();
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                for frame in 0..frames as usize {
                    let mut sample = 0.0;
                    if !silent {
                        let base = data.add(frame * block);
                        for ch in 0..channels {
                            sample += match bits {
                                32 => (base.add(ch * 4) as *const f32).read_unaligned(),
                                16 => (base.add(ch * 2) as *const i16).read_unaligned() as f32 / 32768.0,
                                _ => 0.0,
                            };
                        }
                    }
                    mono.push(sample / channels as f32);
                }
                let _ = capture.ReleaseBuffer(frames);
                let level = recorder.push(&mono);
                // The first half second sets the noise floor; speech is well above it.
                if begin.elapsed() < Duration::from_millis(500) {
                    floor = floor.max(level);
                } else if level > (floor * 3.0).max(0.015) {
                    heard = true;
                    last_loud = Instant::now();
                }
            }
            if (heard && last_loud.elapsed() > QUIET) || (!heard && begin.elapsed() > NOTHING) {
                break;
            }
        }
        let _ = client.Stop();
        if !heard {
            return Ok(String::new());
        }
        recorder
    };
    say("TRANSCRIBING");
    let boundary = "islet-voice-7d1f3c";
    let body = multipart(boundary, &[("model", model), ("language", language), ("response_format", "json")], &wav(&recorder.samples));
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let auth = format!("Bearer {key}");
    let mut headers = vec![("Content-Type", content_type.as_str())];
    if !key.is_empty() {
        headers.push(("Authorization", auth.as_str()));
    }
    let mut answer = Vec::new();
    let status = crate::net::request(&format!("{}/audio/transcriptions", url.trim_end_matches('/')), &headers, Some(&body), &mut |_, chunk| {
        answer.extend_from_slice(chunk);
        answer.len() < 1 << 20
    })?;
    let answer = String::from_utf8_lossy(&answer);
    let root = crate::json::parse(&answer);
    match (status, root) {
        (200, Some(root)) => Ok(root.text("text").to_string()),
        (401 | 403, _) => Err("The API key was not accepted".into()),
        (_, Some(root)) if root.get("error").is_some() => Err(format!("{} ({status})", root.get("error").map(|e| e.text("message")).unwrap_or(""))),
        _ => Err(format!("The service answered {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_is_resampled_to_16_khz_mono() {
        let mut r = Recorder::new(48_000);
        let level = r.push(&[0.5; 4800]);
        assert_eq!(r.samples.len(), 1600);
        assert!((level - 0.5).abs() < 1e-6);
        assert!(r.samples.iter().all(|&s| (s - 16383).abs() <= 1));
        let file = wav(&r.samples[..3]);
        assert_eq!((&file[..4], &file[8..16], file.len()), (&b"RIFF"[..], &b"WAVEfmt "[..], 44 + 6));
        assert_eq!(u32::from_le_bytes([file[24], file[25], file[26], file[27]]), 16_000);
    }

    #[test]
    fn form_data() {
        let body = String::from_utf8(multipart("B", &[("model", "whisper-1"), ("language", "")], b"RIFFdata")).unwrap();
        assert_eq!(body, "--B\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFFdata\r\n--B--\r\n");
    }

    /// Speaks a sentence with the Windows voice into a WAV file (PowerShell's
    /// System.Speech) and reads it back with the local recognizer.
    #[test]
    #[ignore = "needs the Windows English speech recognizer"]
    fn live_local_from_file() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let path = std::env::temp_dir().join("islet-voice-test.wav");
        let script = format!("Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.SetOutputToWaveFile('{}'); $s.Speak('Please send the report to the team tomorrow morning'); $s.Dispose()", path.display());
        assert!(std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).status().unwrap().success());
        let text = listen_local(Some(&path), &AtomicBool::new(false)).unwrap();
        println!("heard: {text:?}");
        let _ = std::fs::remove_file(&path);
        let lower = text.to_lowercase();
        assert!(lower.contains("report") && lower.contains("tomorrow"), "{text}");
    }
}
