# Islet

A liquid "island" for the top of your Windows 11 screen. It shows the time, what is
playing, timers, downloads and notifications, and opens into a small panel with music
controls, quick tools, a clipboard history, notes, weather, system load and more.

Written in Rust on plain Win32 / DirectComposition: no Electron, no browser engine, no
.NET. At rest it uses no CPU and a few MB of memory.

## Download and run

1. Open the [Releases](https://github.com/begaris/islet/releases) page and download the
   latest `Islet-<version>-windows-x64.zip`.
2. Unzip it anywhere you like (for example `Documents\Islet`). Keep `islet.exe` and
   `islet-hook.exe` together.
3. Double-click `islet.exe`. The island appears at the top of the screen and an icon
   appears in the tray.

There is nothing to install. Windows may show **"Windows protected your PC"** the first
time, because the app is not signed: click **More info → Run anyway**.

Requirements: Windows 11, 64-bit. GPU numbers on the System tab need an NVIDIA card.

## Using it

- **Hover** the island to open it; move away to close it.
- **Ctrl + drag** the island to move it sideways (let go near a screen edge and it melts
  into it). Ctrl + drag also reorders tabs, Home buttons, app icons, cards and sections.
- The **cog** in the open island (or the tray menu) opens the settings.
- **Alt + Space** opens the command bar (apps, calculator, timers like "timer 10 min",
  "100 usd to eur").
- To start it with Windows: Settings → General → Start with Windows.

## Your data stays on your PC

- Everything Islet keeps is in `%APPDATA%\Islet` on your own computer: settings
  (`config.txt`), clipboard history and chat history (both encrypted with your Windows
  account), notes, the shelf and the app bar. Screenshots go to `Pictures\Islet`,
  recordings to `Videos\Islet`.
- Nothing is sent anywhere unless you switch a network feature on in the settings. All of
  them are **off** at first:
  - Weather: the city name once to open-meteo.com, then only its coordinates.
  - Song lyrics: the song's title, artist and length to lrclib.net.
  - Currency conversion: daily rates from frankfurter.dev (nothing about you is sent).
  - Update check: asks GitHub for the latest release, once a day.
  - AI chat and online voice to text: your messages / recordings go to the provider you
    choose, with your own login or key.
- Claude Code / Codex notifications are optional: tray menu → *Install Claude Code hooks* adds
  Islet to your Claude Code settings (*Remove Claude Code hooks* takes it out again).

## Removing it

1. Tray icon → **Quit**.
2. If you turned on *Start with Windows*, turn it off first in the settings (or delete
   the `Islet` value in `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`).
3. If you added the Claude Code hooks: tray menu → *Remove Claude Code hooks* first.
4. Delete the folder you unzipped and, to remove all your data, `%APPDATA%\Islet`.

## Building it yourself

1. Install [Rust](https://rustup.rs) (the MSVC toolchain).
2. In the project folder: `cargo build --release`
3. The programs are in `target\release\` (`islet.exe`, `islet-hook.exe`).

`tools\release.ps1` builds a release zip the way the downloads are made (without the
builder's own folder names in the program) and checks it.

## Licence

MIT. See [LICENSE](LICENSE). All icons and characters are drawn by the program itself.
