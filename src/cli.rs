//! The user's own **Claude Code** and **Codex** as chat back-ends: Islet runs
//! the copy that the Claude / Codex desktop apps install, signed in with the
//! user's Claude or ChatGPT account, so the chat needs no API key and no
//! extra bill. Each answer is one helper process (the heavy engines stay out
//! of Islet's own memory, like the PDF reader and the recorder).
//!
//! Conversations are kept by the tool itself, in the folder `~\Islet Chat`:
//! a Claude Code session or a Codex thread per Islet conversation. The next
//! message continues it (`--resume` / `exec resume`), and the user can open
//! the very same conversation in Claude Code or Codex ([`open_session`]).
//!
//! Claude Code runs with every tool off; Codex in its read-only sandbox and
//! told to just answer. Neither reads the user's settings, hooks or MCP
//! servers.

use crate::chat::{Content, Message, Provider, Settings, CONTEXT, SYSTEM};
use crate::json::{self, Value};
use std::io::{BufRead, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

const NO_WINDOW: u32 = 0x0800_0000;
const NEW_CONSOLE: u32 = 0x0000_0010;

/// Told to Codex at the start of a conversation (it is a coding agent).
const CODEX_BRIEF: &str = "You are answering inside Islet, a small chat panel on the user's desktop. Just answer the message: do not run commands, edit files or look around the computer. Answer briefly; use only light Markdown (bold, short bullet lists).\n\n";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Claude,
    Codex,
}

impl Tool {
    pub fn of(provider: Provider) -> Option<Tool> {
        match provider {
            Provider::Claude => Some(Tool::Claude),
            Provider::Codex => Some(Tool::Codex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Whether the tool is there and signed in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Login {
    /// Not installed (no Claude / Codex app).
    Missing,
    SignedOut,
    SignedIn,
}

/// A conversation kept by Claude Code or Codex. `turns` is how many messages
/// (questions and answers) it already holds, so Islet knows whether its own
/// conversation still matches it and can simply continue it.
#[derive(Clone, PartialEq, Debug)]
pub struct Session {
    pub tool: Tool,
    pub id: String,
    pub turns: usize,
}

impl Session {
    /// One line for the small state file: "claude <id> <turns>".
    pub fn line(&self) -> String {
        let tool = if self.tool == Tool::Claude { "claude" } else { "codex" };
        format!("{tool} {} {}", self.id, self.turns)
    }

    pub fn parse(line: &str) -> Option<Session> {
        let mut parts = line.split_whitespace();
        let tool = match parts.next()? {
            "claude" => Tool::Claude,
            "codex" => Tool::Codex,
            _ => return None,
        };
        let id = parts.next()?.to_string();
        let turns = parts.next()?.parse().ok()?;
        Some(Session { tool, id, turns })
    }
}

// ------------------------------------------------------------- finding them --

/// The newest `name` inside `root` (looking `depth` folders down).
fn newest(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut folders = vec![(root.to_path_buf(), 0)];
    while let Some((dir, level)) = folders.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if level < depth {
                    folders.push((path, level + 1));
                }
            } else if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
                if let Ok(modified) = path.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                        best = Some((modified, path));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file()))
}

/// The program: the copy the desktop app keeps up to date, else a stand-alone
/// install. Claude: `%APPDATA%\Claude\claude-code\<ver>\<build>\claude.exe`,
/// `~\.local\bin\claude.exe`, PATH. Codex: `%LOCALAPPDATA%\OpenAI\Codex\bin\<build>\codex.exe`, PATH.
pub fn exe(tool: Tool) -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    match tool {
        Tool::Claude => env("APPDATA")
            .and_then(|a| newest(&a.join("Claude").join("claude-code"), "claude.exe", 2))
            .or_else(|| env("USERPROFILE").map(|h| h.join(".local").join("bin").join("claude.exe")).filter(|p| p.is_file()))
            .or_else(|| on_path("claude.exe")),
        Tool::Codex => env("LOCALAPPDATA").and_then(|a| newest(&a.join("OpenAI").join("Codex").join("bin"), "codex.exe", 1)).or_else(|| on_path("codex.exe")),
    }
}

/// Where the conversations live: `~\Islet Chat` (an empty folder, so no
/// project files or settings are read; its name makes the sessions easy to
/// find in Claude Code and Codex).
pub fn chat_dir() -> PathBuf {
    let dir = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Islet Chat");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn quiet(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    command.creation_flags(NO_WINDOW).current_dir(chat_dir());
    command
}

// ------------------------------------------------------------------ signing in --

/// Whether the user is signed in (reads local files only; about a second).
pub fn login(tool: Tool) -> Login {
    let Some(exe) = exe(tool) else { return Login::Missing };
    let signed_in = match tool {
        Tool::Claude => quiet(&exe)
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .output()
            .ok()
            .and_then(|o| json::parse(&String::from_utf8_lossy(&o.stdout)))
            .is_some_and(|root| matches!(root.get("loggedIn"), Some(Value::Bool(true)))),
        // It says "Logged in using ChatGPT" (on stderr) when signed in.
        Tool::Codex => quiet(&exe).args(["login", "status"]).stdin(Stdio::null()).output().ok().is_some_and(|o| {
            let said = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            o.status.success() && said.contains("Logged in")
        }),
    };
    if signed_in {
        Login::SignedIn
    } else {
        Login::SignedOut
    }
}

/// Posted when [`check_login`] knows the answers; read them with [`login_state`].
pub const WM_APP_LOGIN: u32 = WM_APP + 26;
static LOGIN: [AtomicU8; 2] = [AtomicU8::new(0), AtomicU8::new(0)];

/// Finds out on a worker whether the user is signed in to Claude and to
/// ChatGPT, then posts `WM_APP_LOGIN`.
pub fn check_login(target: HWND) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-login".into()).stack_size(256 * 1024).spawn(move || {
        for tool in [Tool::Claude, Tool::Codex] {
            let state = match login(tool) {
                Login::Missing => 1,
                Login::SignedOut => 2,
                Login::SignedIn => 3,
            };
            LOGIN[tool.index()].store(state, Relaxed);
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_LOGIN, WPARAM(0), LPARAM(0));
        }
    });
}

/// The last answer of [`check_login`] (`None` before the first one).
pub fn login_state(tool: Tool) -> Option<Login> {
    match LOGIN[tool.index()].load(Relaxed) {
        1 => Some(Login::Missing),
        2 => Some(Login::SignedOut),
        3 => Some(Login::SignedIn),
        _ => None,
    }
}

/// Opens a console window that signs the user in (it opens the browser):
/// `claude auth login --claudeai` or `codex login`.
pub fn sign_in(tool: Tool) -> bool {
    let Some(exe) = exe(tool) else { return false };
    let args: &[&str] = match tool {
        Tool::Claude => &["auth", "login", "--claudeai"],
        Tool::Codex => &["login"],
    };
    Command::new(exe).args(args).creation_flags(NEW_CONSOLE).current_dir(chat_dir()).spawn().is_ok()
}

/// Opens the conversation in a Claude Code / Codex window, where the user can
/// read it and carry on.
pub fn open_session(session: &Session) -> bool {
    let Some(exe) = exe(session.tool) else { return false };
    let mut command = Command::new(exe);
    match session.tool {
        Tool::Claude => command.args(["--resume", &session.id]),
        Tool::Codex => command.args(["resume", &session.id]),
    };
    command.creation_flags(NEW_CONSOLE).current_dir(chat_dir()).spawn().is_ok()
}

// --------------------------------------------------------------------- models --

/// The models Codex offers (its own catalogue, read locally), as (slug, name).
pub fn codex_models() -> Vec<(String, String)> {
    let Some(exe) = exe(Tool::Codex) else { return Vec::new() };
    let Ok(output) = quiet(&exe).args(["debug", "models"]).stdin(Stdio::null()).output() else { return Vec::new() };
    let Some(root) = json::parse(&String::from_utf8_lossy(&output.stdout)) else { return Vec::new() };
    let Some(Value::Arr(models)) = root.get("models") else { return Vec::new() };
    models
        .iter()
        .filter(|m| m.text("visibility") == "list" && !m.text("slug").is_empty())
        .map(|m| {
            let name = if m.text("display_name").is_empty() { m.text("slug") } else { m.text("display_name") };
            (m.text("slug").to_string(), name.to_string())
        })
        .collect()
}

/// The `model = "..."` the user set for Codex (Islet does not load the rest
/// of that file: it holds hooks and servers meant for coding sessions).
pub fn codex_default_model() -> Option<String> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let text = std::fs::read_to_string(home.join(".codex").join("config.toml")).ok()?;
    config_model(&text)
}

fn config_model(toml: &str) -> Option<String> {
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            break; // only the top level counts
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "model" {
                let value = value.split('#').next().unwrap_or("").trim().trim_matches('"').trim_matches('\'');
                return (!value.is_empty()).then(|| value.to_string());
            }
        }
    }
    None
}

// ------------------------------------------------------------------- asking --

/// A message as text: its documents in `<document>` tags, then what was typed.
/// Pictures are named (`pictures` says whether they travel separately).
fn text_of(m: &Message, pictures_sent: bool) -> String {
    let mut text = String::new();
    for a in &m.attachments {
        match &a.content {
            Content::Text(body) => text.push_str(&format!("<document name=\"{}\">\n{body}\n</document>\n\n", a.name.replace('"', "'"))),
            Content::Image { .. } if pictures_sent => {}
            Content::Image { .. } => text.push_str(&format!("[A picture named \"{}\" was attached, but pictures cannot be sent with the Claude login. Use another provider for pictures.]\n\n", a.name)),
        }
    }
    text.push_str(&m.text);
    text
}

/// The conversation as one prompt (for a conversation the tool does not hold
/// yet): earlier turns as a transcript, then the newest message.
fn transcript(history: &[&Message], pictures_sent: bool) -> String {
    let recent = &history[history.len().saturating_sub(CONTEXT)..];
    let Some((last, earlier)) = recent.split_last() else { return String::new() };
    if earlier.is_empty() {
        return text_of(last, pictures_sent);
    }
    let mut prompt = String::from("The conversation so far:\n\n");
    for m in earlier {
        prompt.push_str(if m.user { "User: " } else { "Assistant: " });
        prompt.push_str(&text_of(m, pictures_sent));
        prompt.push_str("\n\n");
    }
    prompt.push_str("---\nThe user's new message (answer this):\n\n");
    prompt.push_str(&text_of(last, pictures_sent));
    prompt
}

/// A new random id (a GUID) for a Claude Code session.
fn new_id() -> String {
    match unsafe { windows::Win32::System::Com::CoCreateGuid() } {
        Ok(g) => format!(
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
        ),
        Err(_) => format!("00000000-0000-4000-8000-{:012x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64 & 0xffff_ffff_ffff).unwrap_or(1)),
    }
}

/// Answers the conversation with Claude Code or Codex. Continues `session`
/// when it holds exactly the conversation so far, else starts a new one with
/// a transcript. Calls `text` with the answer as it comes; `stop` ends it.
/// Returns how it went and the conversation that now holds the answer.
pub fn ask(tool: Tool, settings: &Settings, history: &[Message], session: Option<Session>, text: &mut dyn FnMut(&str), stop: &dyn Fn() -> bool) -> (Result<(), String>, Option<Session>) {
    let turns: Vec<&Message> = history.iter().filter(|m| m.sendable()).collect();
    let Some(last) = turns.last() else { return (Err("Nothing to answer.".to_string()), None) };
    let Some(exe) = exe(tool) else {
        let app = if tool == Tool::Claude { "the Claude desktop app" } else { "the Codex app" };
        return (Err(format!("{} was not found. Install {app}, or choose another chat provider in the settings.", tool.name())), None);
    };
    let resume = session.filter(|s| s.tool == tool && s.turns + 1 == turns.len()).map(|s| s.id);
    let (outcome, id) = match tool {
        Tool::Claude => {
            let prompt = if resume.is_some() { text_of(last, false) } else { transcript(&turns, false) };
            run_claude(&exe, settings, &prompt, resume, last, text, stop)
        }
        Tool::Codex => {
            let prompt = match resume {
                Some(_) => text_of(last, true),
                None => format!("{CODEX_BRIEF}{}", transcript(&turns, true)),
            };
            run_codex(&exe, settings, &prompt, resume, last, text, stop)
        }
    };
    let kept = match (&outcome, id) {
        (Ok(()), Some(id)) => Some(Session { tool, id, turns: turns.len() + 1 }),
        _ => None,
    };
    (outcome, kept)
}

/// Starts the helper with `prompt` on stdin (never on a command line).
fn start(mut command: Command, prompt: &str, what: &str) -> Result<std::process::Child, String> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("{what} could not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    Ok(child)
}

/// The conversation's title in Claude Code: the start of the first message.
fn title(first: &Message) -> String {
    let words: String = first.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = words.chars().take(48).collect();
    format!("Islet: {}", if short.is_empty() { "chat".to_string() } else { short })
}

fn run_claude(exe: &Path, settings: &Settings, prompt: &str, resume: Option<String>, last: &Message, text: &mut dyn FnMut(&str), stop: &dyn Fn() -> bool) -> (Result<(), String>, Option<String>) {
    let mut command = quiet(exe);
    command.args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--tools", "", "--strict-mcp-config", "--setting-sources", "project", "--system-prompt", SYSTEM]);
    let id = match &resume {
        Some(id) => {
            command.args(["--resume", id]);
            id.clone()
        }
        None => {
            let id = new_id();
            command.args(["--session-id", &id, "-n", &title(last)]);
            id
        }
    };
    if !settings.model.is_empty() {
        command.args(["--model", &settings.model]);
    }
    let mut child = match start(command, prompt, "Claude Code") {
        Ok(child) => child,
        Err(why) => return (Err(why), None),
    };
    let Some(stdout) = child.stdout.take() else { return (Err("Claude Code did not answer.".to_string()), None) };
    let (mut streamed, mut outcome) = (false, None);
    for line in std::io::BufReader::new(stdout).lines() {
        if stop() {
            let _ = child.kill();
            return (Ok(()), None);
        }
        let Ok(line) = line else { break };
        let Some(root) = json::parse(&line) else { continue };
        match root.text("type") {
            "stream_event" => {
                if let Some(delta) = root.get("event").filter(|e| e.text("type") == "content_block_delta").and_then(|e| e.get("delta")).filter(|d| d.text("type") == "text_delta") {
                    streamed = true;
                    text(delta.text("text"));
                }
            }
            "result" => {
                let said = root.text("result").to_string();
                outcome = Some(if matches!(root.get("is_error"), Some(Value::Bool(true))) {
                    Err(claude_failure(&said))
                } else {
                    if !streamed && !said.is_empty() {
                        text(&said);
                    }
                    Ok(())
                });
            }
            _ => {}
        }
    }
    let _ = child.wait();
    (outcome.unwrap_or_else(|| Err("Claude Code stopped without an answer.".to_string())), Some(id))
}

fn run_codex(exe: &Path, settings: &Settings, prompt: &str, resume: Option<String>, last: &Message, text: &mut dyn FnMut(&str), stop: &dyn Fn() -> bool) -> (Result<(), String>, Option<String>) {
    let mut command = quiet(exe);
    match &resume {
        Some(id) => command.args(["exec", "resume", id.as_str(), "--json", "--skip-git-repo-check", "--ignore-user-config", "--ignore-rules"]),
        None => command.args(["exec", "--json", "--skip-git-repo-check", "--ignore-user-config", "--ignore-rules", "--sandbox", "read-only"]),
    };
    let model = if settings.model.is_empty() { codex_default_model().unwrap_or_default() } else { settings.model.clone() };
    if !model.is_empty() {
        command.args(["-m", &model]);
    }
    // Pictures of the new message go along as files.
    let mut pictures = Vec::new();
    for (i, a) in last.attachments.iter().enumerate() {
        if let Content::Image { jpeg, .. } = &a.content {
            let path = std::env::temp_dir().join(format!("islet-codex-picture-{i}.jpg"));
            if std::fs::write(&path, jpeg).is_ok() {
                command.arg(format!("--image={}", path.display()));
                pictures.push(path);
            }
        }
    }
    command.arg("-");
    let mut child = match start(command, prompt, "Codex") {
        Ok(child) => child,
        Err(why) => return (Err(why), None),
    };
    let Some(stdout) = child.stdout.take() else { return (Err("Codex did not answer.".to_string()), None) };
    let (mut id, mut said_any, mut outcome) = (resume, false, None);
    for line in std::io::BufReader::new(stdout).lines() {
        if stop() {
            let _ = child.kill();
            outcome = Some(Ok(()));
            break;
        }
        let Ok(line) = line else { break };
        let Some(root) = json::parse(&line) else { continue };
        match root.text("type") {
            "thread.started" if !root.text("thread_id").is_empty() => id = Some(root.text("thread_id").to_string()),
            "item.completed" => {
                if let Some(item) = root.get("item").filter(|i| i.text("type") == "agent_message") {
                    if said_any {
                        text("\n\n");
                    }
                    said_any = true;
                    text(item.text("text"));
                }
            }
            "turn.completed" => outcome = Some(Ok(())),
            "turn.failed" => outcome = Some(Err(codex_failure(root.get("error").map(|e| e.text("message")).unwrap_or("")))),
            "error" => outcome = Some(Err(codex_failure(root.text("message")))),
            _ => {}
        }
    }
    let _ = child.wait();
    for path in pictures {
        let _ = std::fs::remove_file(path);
    }
    (outcome.unwrap_or_else(|| Err(codex_failure(""))), id)
}

/// A readable reason for a failed Claude Code run.
fn claude_failure(said: &str) -> String {
    let lower = said.to_ascii_lowercase();
    if lower.contains("not logged in") || lower.contains("/login") || lower.contains("authenticat") {
        "Not signed in to Claude yet. Open the settings (Network) and press \u{201C}Sign in\u{201D} next to Claude login.".to_string()
    } else if lower.contains("limit") {
        format!("Your Claude plan's usage limit was reached: {said}")
    } else if said.is_empty() {
        "Claude Code reported an error.".to_string()
    } else {
        said.to_string()
    }
}

/// A readable reason for a failed Codex run.
fn codex_failure(said: &str) -> String {
    let lower = said.to_ascii_lowercase();
    if lower.contains("login") || lower.contains("logged in") || lower.contains("401") || lower.contains("unauthorized") {
        "Not signed in to ChatGPT yet. Open the settings (Network) and press \u{201C}Sign in\u{201D} next to ChatGPT login.".to_string()
    } else if lower.contains("usage limit") || lower.contains("rate limit") {
        format!("Your ChatGPT plan's usage limit was reached: {said}")
    } else if said.is_empty() {
        "Codex stopped without an answer.".to_string()
    } else {
        said.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Attachment, Content};

    fn message(user: bool, text: &str) -> Message {
        Message::new(user, text.to_string(), false)
    }

    #[test]
    fn prompts_titles_and_sessions() {
        let mut first = message(true, "Summarise   this\nplease");
        first.attachments = vec![
            Attachment { name: "Q3.pdf".into(), detail: String::new(), content: Content::Text("Sales up".into()) },
            Attachment { name: "chart.jpg".into(), detail: String::new(), content: Content::Image { jpeg: vec![1, 2, 3], w: 1, h: 1 } },
        ];
        // Claude: the picture is named; Codex: it travels as a file, so it is not mentioned.
        let claude = transcript(&[&first], false);
        assert!(claude.contains("<document name=\"Q3.pdf\">\nSales up\n</document>") && claude.contains("picture named \"chart.jpg\""));
        assert!(!transcript(&[&first], true).contains("chart.jpg"));
        assert_eq!(title(&first), "Islet: Summarise this please");
        // A transcript leaves error notes out and ends with the new message.
        let (a, b, err, c) = (message(true, "Hi"), message(false, "Hello!"), Message::new(false, "boom".into(), true), message(true, "And now?"));
        let turns: Vec<&Message> = [&a, &b, &err, &c].into_iter().filter(|m| m.sendable()).collect();
        let prompt = transcript(&turns, false);
        assert!(prompt.starts_with("The conversation so far:\n\nUser: Hi\n\nAssistant: Hello!\n\n") && !prompt.contains("boom") && prompt.ends_with("answer this):\n\nAnd now?"));
        // The session line round-trips.
        let s = Session { tool: Tool::Codex, id: "01a1e0d9-a366".into(), turns: 4 };
        assert_eq!(Session::parse(&s.line()), Some(s));
        assert_eq!(new_id().len(), 36);
    }

    /// Live: a two-message conversation with each login (two tiny requests on
    /// the user's own accounts), checking the second one continues the first.
    /// The sessions it makes are deleted again.
    /// `cargo test --release live_logins -- --ignored --nocapture`
    #[test]
    #[ignore = "uses the user's Claude and ChatGPT accounts"]
    fn live_logins() {
        for tool in [Tool::Claude, Tool::Codex] {
            if login(tool) != Login::SignedIn {
                println!("{}: not signed in, skipped", tool.name());
                continue;
            }
            let settings = Settings::new(if tool == Tool::Claude { "claude" } else { "chatgpt" }, if tool == Tool::Claude { "haiku" } else { "" }, "");
            let mut history = vec![message(true, "Remember the word PELICAN. Reply with just: OK")];
            let mut said = String::new();
            let (done, session) = ask(tool, &settings, &history, None, &mut |t| said.push_str(t), &|| false);
            println!("{} first: {done:?} {said:?} {session:?}", tool.name());
            let session = session.expect("a kept conversation");
            assert!(done.is_ok() && session.turns == 2);
            history.push(message(false, &said));
            history.push(message(true, "Which word did I ask you to remember? One word."));
            let mut again = String::new();
            let (done, next) = ask(tool, &settings, &history, Some(session.clone()), &mut |t| again.push_str(t), &|| false);
            println!("{} second: {done:?} {again:?} {next:?}", tool.name());
            assert!(done.is_ok() && again.to_uppercase().contains("PELICAN"));
            assert_eq!(next.as_ref().map(|s| (&s.id, s.turns)), Some((&session.id, 4)), "continued the same conversation");
            // Clean up the test conversation.
            match tool {
                Tool::Claude => {
                    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
                    for project in std::fs::read_dir(home.join(".claude").join("projects")).into_iter().flatten().flatten() {
                        let _ = std::fs::remove_file(project.path().join(format!("{}.jsonl", session.id)));
                    }
                }
                Tool::Codex => {
                    let _ = quiet(&exe(Tool::Codex).unwrap()).args(["delete", &session.id]).stdin(Stdio::null()).output();
                }
            }
        }
    }

    #[test]
    fn failures_and_codex_config() {
        assert!(claude_failure("Not logged in · Please run /login").contains("Sign in"));
        assert!(claude_failure("Claude AI usage limit reached|1731600000").starts_with("Your Claude plan"));
        assert!(codex_failure("401 Unauthorized").contains("ChatGPT login"));
        assert_eq!(codex_failure("Something else"), "Something else");
        assert_eq!(config_model("model = \"gpt-6-luna\"\nnotify = [\"x\"]\n[profiles.fast]\nmodel = \"other\""), Some("gpt-6-luna".into()));
        assert_eq!(config_model("[mcp_servers.a]\nmodel = \"x\""), None);
    }
}
