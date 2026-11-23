//! Islet: a liquid island overlay for Windows 11.
//!
//! Flags: `--demo` loops the nine reference scenes, `--debug` shows a HUD with
//! memory, CPU and frame time.

#![windows_subsystem = "windows"]

mod anim;
mod agent;
mod app;
mod attach;
mod audio;
mod chat;
mod cli;
mod clipboard;
mod command;
mod comp;
mod config;
mod control;
mod currency;
mod debug;
mod demo;
mod dock;
mod events;
mod gamemode;
mod hooks;
mod image;
mod input;
mod notes;
mod shelf;
mod snip;
mod json;
mod island;
mod markup;
mod mascot;
mod media;
mod net;
mod notify;
mod panels;
mod record;
mod lyrics;
mod settings;
mod sound;
mod render;
mod state;
mod sysstat;
mod theme;
mod timer;
mod tray;
mod update;
mod voice;
mod vpn;
mod watch;
mod weather;
mod ui;

use app::App;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Broadcast by Explorer when the taskbar is (re)created; the tray icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut guard| guard.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The tray menu runs a modal loop, so it must not hold the app borrow.
        tray::WM_APP_TRAY => {
            tray::on_message(hwnd, lparam);
            return LRESULT(0);
        }
        // Dragging files out of the shelf is a modal loop too.
        app::WM_APP_DRAG => {
            if let Some(paths) = with_app(|app| app.take_drag()).flatten() {
                let taken = shelf::drag(&paths);
                with_app(|app| app.drag_done(taken));
            }
            return LRESULT(0);
        }
        // So is the file picker for chat attachments.
        app::WM_APP_PICK => {
            let paths = attach::pick(hwnd);
            with_app(|app| app.picked(paths));
            return LRESULT(0);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ if msg == TASKBAR_CREATED.load(Relaxed) && msg != 0 => {
            tray::add(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    // Re-entrant messages (sent while the app is already borrowed) fall through to the default handler.
    match with_app(|app| app.on_message(msg, wparam, lparam)).flatten() {
        Some(result) => result,
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The settings window's messages go to the app (`App::on_settings_message`).
unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match with_app(|app| app.on_settings_message(msg, wparam, lparam)).flatten() {
        Some(result) => result,
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Sends a phrase to the Islet instance that is already running (`WM_COPYDATA`).
/// Returns `false` when there is none or it did not understand the phrase.
fn forward_command(phrase: &str) -> bool {
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    unsafe {
        let Ok(window) = FindWindowW(w!("IsletWindow"), w!("Islet")) else { return false };
        let data = COPYDATASTRUCT { dwData: 1, cbData: phrase.len() as u32, lpData: phrase.as_ptr() as *mut _ };
        SendMessageW(window, WM_COPYDATA, None, Some(LPARAM(&data as *const _ as isize))).0 != 0
    }
}

fn main() {
    let (mut demo, mut debug, mut test_hooks) = (None, false, false);
    let mut snapshot = None;
    let mut hidden = false;
    let mut downloads = None;
    let mut notes_dir = None;
    let mut cfg = config::load();
    let all: Vec<String> = std::env::args().skip(1).collect();
    // `--install-hooks` / `--uninstall-hooks` edit the agent config files and exit.
    // `--settings=<file>` and `--codex-config=<file>` point them at other files (tests).
    if let Some(install) = all.iter().find_map(|a| match a.as_str() {
        "--install-hooks" => Some(true),
        "--uninstall-hooks" => Some(false),
        _ => None,
    }) {
        let path = |prefix: &str| all.iter().find_map(|a| a.strip_prefix(prefix)).map(std::path::PathBuf::from);
        let result = hooks::apply(install, path("--settings="), path("--codex-config="));
        unsafe {
            // A GUI program has no console of its own: borrow the caller's to report.
            let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
        match &result {
            Ok(note) => println!("Islet hooks {}: {note}", if install { "installed" } else { "removed" }),
            Err(error) => println!("Islet hooks not changed: {error}"),
        }
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    // `--read-pdf <file>`: the helper that reads a PDF for a chat attachment and exits
    // (runs before the single-instance check: Islet itself is the one asking).
    // `--listen local|api ...`: the helper that turns speech into text (see `voice`).
    if let Some(at) = all.iter().position(|a| a == "--listen") {
        std::process::exit(voice::helper(&all[at + 1..]));
    }
    // `--record <x>,<y> <file>`: the helper that records the screen (see `record`).
    if let Some(at) = all.iter().position(|a| a == "--record") {
        let point = all.get(at + 1).cloned().unwrap_or_default();
        let path = all.get(at + 2).map(std::path::PathBuf::from).unwrap_or_default();
        std::process::exit(record::helper(&point, &path));
    }
    // `--gpu-stats`: the helper that reads the NVIDIA card's numbers (see `sysstat`).
    if all.iter().any(|a| a == "--gpu-stats") {
        std::process::exit(sysstat::gpu_helper());
    }
    if let Some(at) = all.iter().position(|a| a == "--read-pdf") {
        let path = all.get(at + 1).map(std::path::PathBuf::from).unwrap_or_default();
        std::process::exit(attach::pdf_helper(&path));
    }
    let mut args = all.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // `--cmd "timer 10 min"`: hand a phrase to the running Islet and exit.
            "--cmd" => {
                let phrase = args.next().unwrap_or_default();
                std::process::exit(if forward_command(&phrase) { 0 } else { 1 });
            }
            // `--downloads=<dir>` watches that folder instead of the user's Downloads (tests).
            other if other.starts_with("--downloads=") => downloads = Some(std::path::PathBuf::from(&other["--downloads=".len()..])),
            // `--notes=<dir>` keeps notes in that folder instead of the user's (tests).
            other if other.starts_with("--notes=") => notes_dir = Some(std::path::PathBuf::from(&other["--notes=".len()..])),
            "--demo" => demo = Some(false),
            // The demo without its caption and fake cursor, for measuring CPU.
            "--demo-bare" => demo = Some(true),
            "--debug" => debug = true,
            // Accept fake-cursor messages and publish state for automated tests.
            "--test-hooks" => test_hooks = true,
            // Override the `renderer` setting for one run (handy for comparing budgets).
            "--gpu" => cfg.gpu = true,
            "--software" => cfg.gpu = false,
            // `--scale=150` overrides the UI scale (percent) for one run.
            // `--snapshot=<dir>`: render with the window hidden and dump frames on request (tests).
            other if other.starts_with("--snapshot=") => {
                snapshot = Some(std::path::PathBuf::from(&other["--snapshot=".len()..]));
                test_hooks = true;
                hidden = true;
            }
            // Run with the window hidden and game-suspend ignored (measuring without drawing on screen).
            "--hidden" => {
                hidden = true;
                test_hooks = true;
            }
            other => {
                if let Some(percent) = other.strip_prefix("--scale=").and_then(|v| v.parse::<f32>().ok()) {
                    cfg.scale = (percent / 100.0).clamp(0.8, 2.0);
                }
            }
        }
    }

    unsafe {
        // Single instance.
        let _mutex = CreateMutexW(None, true, w!("Local\\Islet.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        // The UI thread talks to Core Audio (volume, devices) through COM; OLE
        // (a single-threaded apartment plus drag and drop) is needed for the shelf.
        let _ = windows::Win32::System::Ole::OleInitialize(None);

        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("IsletWindow"),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return;
        }
        // The settings window: an ordinary window with a title bar.
        let settings_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(settings_proc),
            hInstance: module.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(Some(module.into()), windows::core::PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: w!("IsletSettings"),
            ..Default::default()
        };
        let _ = RegisterClassExW(&settings_class);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);

        // Topmost, never activated, no taskbar button, no redirection surface
        // (DirectComposition draws it), and mouse-transparent until the cursor
        // is over the island's shape.
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT;
        let Ok(hwnd) = CreateWindowExW(ex, w!("IsletWindow"), w!("Islet"), WS_POPUP, 0, 0, 16, 16, None, None, Some(module.into()), None) else {
            return;
        };

        // A layered window is invisible to hit-testing until it has layered
        // attributes; without this, clicks pass through even while interactive.
        let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA);

        // The island draws its own shape: no Windows 11 rounding, border or shadow.
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
        let policy = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_NCRENDERING_POLICY, &policy as *const _ as *const _, 4);

        APP.with(|cell| *cell.borrow_mut() = Some(App::new(hwnd, cfg, demo, debug, test_hooks)));
        with_app(|app| {
            app.snapshot = snapshot;
            app.downloads = downloads;
            app.notes_dir = notes_dir;
            app.hidden = hidden;
            app.start()
        });

        let mut msg = MSG::default();
        'run: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                // Turns key presses into WM_CHAR for the command bar and search field.
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if with_app(|app| app.wants_frame()).unwrap_or(false) {
                let refreshes = with_app(|app| {
                    app.frame();
                    app.pace()
                });
                // Pace to the compositor; only reached while something animates.
                for _ in 0..refreshes.unwrap_or(1) {
                    let _ = DwmFlush();
                }
            } else {
                // Fully asleep until the next message: 0 % CPU.
                MsgWaitForMultipleObjectsEx(None, u32::MAX, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }

        with_app(|app| app.shutdown());
        APP.with(|cell| *cell.borrow_mut() = None);
    }
}
