//! `islet-hook.exe`: the tiny companion that Claude Code runs as a hook
//! command and Codex runs as its `notify` program.
//!
//! It forwards one JSON object to the island over `\\.\pipe\islet` and prints
//! whatever the island answers. If the island is not running it exits at once
//! with success and no output, so it never blocks or changes the agent.
//!
//! Usage:
//!   islet-hook                 JSON on stdin (Claude Code hooks)
//!   islet-hook --codex <json>  JSON as the last argument (Codex `notify`)
//!   islet-hook --send <json>   same, for scripts and other tools
//!
//! Standard library only, to stay small.

use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut payload = String::new();
    if args.first().is_some_and(|a| a == "--codex" || a == "--send") {
        payload = args.last().cloned().unwrap_or_default();
    } else if std::io::stdin().read_to_string(&mut payload).is_err() {
        return;
    }
    // The protocol is one object per line; raw newlines in JSON are only whitespace.
    // Some shells put a byte-order mark in front of piped text.
    let line: String = payload.trim_start_matches('\u{feff}').trim().chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    if line.is_empty() {
        return;
    }

    let mut pipe = None;
    for attempt in 0..4 {
        match std::fs::OpenOptions::new().read(true).write(true).open(r"\\.\pipe\islet") {
            Ok(file) => {
                pipe = Some(file);
                break;
            }
            // 231 = ERROR_PIPE_BUSY: the island is between two listening instances.
            Err(e) if e.raw_os_error() == Some(231) && attempt < 3 => std::thread::sleep(std::time::Duration::from_millis(25)),
            // Not running (or not reachable): succeed silently.
            Err(_) => return,
        }
    }
    let Some(mut pipe) = pipe else { return };
    if pipe.write_all(line.as_bytes()).is_err() || pipe.write_all(b"\n").is_err() {
        return;
    }
    let mut reply = Vec::new();
    let _ = pipe.read_to_end(&mut reply);
    let reply = String::from_utf8_lossy(&reply);
    let reply = reply.trim();
    if !reply.is_empty() {
        println!("{reply}");
    }
}
