//! Coding-agent integration: the named pipe that `islet-hook.exe` (Claude
//! Code hooks, Codex `notify`) and other tools talk to.
//!
//! Protocol: a client connects to `\\.\pipe\islet`, writes one JSON object on
//! one line, and reads the reply until the pipe closes. For most events the
//! reply is empty. For a permission request the reply is held until the user
//! clicks Allow or Deny in the island (or it times out), and is exactly what
//! the hook must print for Claude Code.
//!
//! Any tool can use it with a few lines of script:
//! `{"event":"done","source":"My tool","text":"build finished"}`
//! (`event`: working | waiting | done | notify | ask; `ask` is answered with
//! `{"decision":"allow"}`, `{"decision":"deny"}` or `{"decision":"none"}`).
//!
//! The pipe only accepts the current user.

use crate::json::{self, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::Storage::FileSystem::{CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the island window when [`take`] has requests.
pub const WM_APP_AGENT: u32 = WM_APP + 11;
const PIPE_NAME: PCWSTR = w!("\\\\.\\pipe\\islet");

pub struct Request {
    pub id: u64,
    pub text: String,
}

static PENDING: Mutex<Vec<Request>> = Mutex::new(Vec::new());
/// Open client connections waiting for their reply, by request id.
static CLIENTS: Mutex<Option<HashMap<u64, isize>>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static TARGET: AtomicIsize = AtomicIsize::new(0);

pub fn take() -> Vec<Request> {
    PENDING.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
}

/// Sends the reply for request `id` and closes its connection.
pub fn reply(id: u64, text: &str) {
    let handle = CLIENTS.lock().ok().and_then(|mut c| c.as_mut().and_then(|map| map.remove(&id)));
    let Some(raw) = handle else { return };
    let pipe = HANDLE(raw as *mut _);
    unsafe {
        if !text.is_empty() {
            let _ = WriteFile(pipe, Some(text.as_bytes()), None, None);
            let _ = FlushFileBuffers(pipe);
        }
        let _ = DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }
}

pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(target: HWND) -> Option<Self> {
        TARGET.store(target.0 as isize, Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("islet-pipe".into()).stack_size(128 * 1024).spawn(move || unsafe { serve(&flag) }).ok()?;
        Some(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        unsafe {
            // Connect once so the thread blocked in ConnectNamedPipe wakes up and sees the flag.
            if let Ok(wake) = CreateFileW(PIPE_NAME, (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, Default::default(), None) {
                let _ = CloseHandle(wake);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Nobody will answer the requests that are still open.
        let open: Vec<u64> = CLIENTS.lock().ok().and_then(|c| c.as_ref().map(|map| map.keys().copied().collect())).unwrap_or_default();
        for id in open {
            reply(id, "");
        }
    }
}

/// A security descriptor that grants access to the current user only.
unsafe fn current_user_only() -> Option<PSECURITY_DESCRIPTOR> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut buffer = [0u64; 32];
        let mut size = 0u32;
        let got = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr() as *mut _), size_of_val(&buffer) as u32, &mut size);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_text).ok()?;
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", String::from_utf16_lossy(sid_text.as_wide())).encode_utf16().chain([0]).collect();
        let _ = LocalFree(Some(HLOCAL(sid_text.0 as *mut _)));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut descriptor, None).ok()?;
        Some(descriptor)
    }
}

unsafe fn serve(stop: &AtomicBool) {
    unsafe {
        // Without the restricted descriptor the pipe is not opened at all.
        let Some(descriptor) = current_user_only() else { return };
        let security = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into() };
        while !stop.load(Relaxed) {
            let pipe = CreateNamedPipeW(PIPE_NAME, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 65536, 65536, 0, Some(&security));
            if pipe.is_invalid() {
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
            let connected = ConnectNamedPipe(pipe, None).is_ok() || GetLastError() == ERROR_PIPE_CONNECTED;
            if !connected || stop.load(Relaxed) {
                let _ = CloseHandle(pipe);
                continue;
            }
            let raw = pipe.0 as isize;
            // Each client gets its own short-lived reader so a slow one never blocks the others.
            let _ = std::thread::Builder::new().name("islet-pipe-client".into()).stack_size(128 * 1024).spawn(move || read_request(HANDLE(raw as *mut _)));
        }
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
}

unsafe fn read_request(pipe: HANDLE) {
    unsafe {
        let mut data = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let mut read = 0u32;
            if ReadFile(pipe, Some(&mut chunk), Some(&mut read), None).is_err() || read == 0 {
                break;
            }
            data.extend_from_slice(&chunk[..read as usize]);
            if data.contains(&b'\n') || data.len() > 1_000_000 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&data).trim().to_string();
        if text.is_empty() {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
            return;
        }
        let id = NEXT_ID.fetch_add(1, Relaxed);
        if let Ok(mut clients) = CLIENTS.lock() {
            clients.get_or_insert_with(HashMap::new).insert(id, pipe.0 as isize);
        }
        if let Ok(mut pending) = PENDING.lock() {
            pending.push(Request { id, text });
        }
        let _ = PostMessageW(Some(HWND(TARGET.load(Relaxed) as *mut _)), WM_APP_AGENT, WPARAM(0), LPARAM(0));
    }
}

// ------------------------------------------------------------ interpretation --

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The agent is busy (key identifies the session).
    Working { key: String },
    /// The agent is idle and wants the user.
    Waiting { key: String, who: String, text: String },
    Done { key: String, who: String, text: String },
    /// The session went away.
    Ended { key: String },
    /// The user must allow or deny something (or answer it: see [`Ask`]).
    Permit { who: String, tool: String, detail: String, claude: bool, ask: Ask },
    Notice { title: String, text: String },
    Ignore,
}

/// What kind of answer a permission request wants.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// Allow or deny a tool call. `always`: Claude Code's `permission_suggestions`
    /// (compact JSON array) for "Always allow", when it sent any.
    Tool { always: Option<String> },
    /// Claude Code's AskUserQuestion: up to four questions with options. `raw` is
    /// the original `questions` array (compact JSON), echoed back with the answers.
    Questions { raw: String, list: Vec<Question> },
    /// ExitPlanMode: approve the plan. `input` is the tool input to echo back.
    Plan { input: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub text: String,
    pub header: String,
    /// (label, description)
    pub options: Vec<(String, String)>,
    pub multi: bool,
}

/// The AskUserQuestion questions in a tool input (empty if malformed).
fn questions(input: Option<&Value>) -> Vec<Question> {
    let Some(Value::Arr(list)) = input.and_then(|i| i.get("questions")) else { return Vec::new() };
    list.iter()
        .filter_map(|q| {
            let Some(Value::Arr(options)) = q.get("options") else { return None };
            let options: Vec<(String, String)> = options.iter().map(|o| (o.text("label").to_string(), o.text("description").to_string())).filter(|(l, _)| !l.is_empty()).collect();
            let text = q.text("question").to_string();
            (!text.is_empty() && !options.is_empty()).then(|| Question { text, header: q.text("header").to_string(), options, multi: q.get("multiSelect") == Some(&Value::Bool(true)) })
        })
        .take(4)
        .collect()
}

/// What Claude Code's PermissionRequest asks for.
fn claude_ask(v: &Value) -> Ask {
    let input = v.get("tool_input");
    match v.text("tool_name") {
        "AskUserQuestion" => {
            let list = questions(input);
            match input.and_then(|i| i.get("questions")) {
                Some(raw) if !list.is_empty() => Ask::Questions { raw: raw.compact(), list },
                _ => Ask::Tool { always: None },
            }
        }
        "ExitPlanMode" => Ask::Plan { input: input.map_or("{}".to_string(), Value::compact) },
        _ => Ask::Tool { always: match v.get("permission_suggestions") { Some(s @ Value::Arr(items)) if !items.is_empty() => Some(s.compact()), _ => None } },
    }
}

/// Last path component: the project folder an agent works in.
fn project(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("").to_string()
}

/// What a tool call is about, in one line (the command, the file, ...).
fn describe_tool(input: Option<&Value>) -> String {
    let Some(input) = input else { return String::new() };
    for key in ["command", "file_path", "path", "url", "pattern", "query", "prompt", "description"] {
        let text = input.text(key);
        if !text.is_empty() {
            return text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    input.compact()
}

pub fn interpret(text: &str) -> Event {
    let Some(v) = json::parse(text.trim_start_matches('\u{feff}').trim()) else { return Event::Ignore };
    let key = |fallback: &str| {
        let id = v.text("session_id");
        if id.is_empty() { fallback.to_string() } else { id.to_string() }
    };

    // Claude Code hook payloads carry `hook_event_name`.
    let hook = v.text("hook_event_name");
    if !hook.is_empty() {
        let who = "Claude Code".to_string();
        let folder = project(v.text("cwd"));
        return match hook {
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStop" => Event::Working { key: key("claude") },
            "PermissionRequest" => {
                let ask = claude_ask(&v);
                let detail = match &ask {
                    Ask::Questions { list, .. } => list[0].text.clone(),
                    Ask::Plan { .. } => v.get("tool_input").map_or(String::new(), |i| i.text("plan").split_whitespace().collect::<Vec<_>>().join(" ")),
                    Ask::Tool { .. } => describe_tool(v.get("tool_input")),
                };
                Event::Permit { who, tool: v.text("tool_name").to_string(), detail, claude: true, ask }
            }
            "Notification" => match v.text("notification_type") {
                // The permission dialog itself is handled by PermissionRequest.
                "idle_prompt" | "agent_needs_input" | "elicitation_dialog" => Event::Waiting { key: key("claude"), who, text: v.text("message").to_string() },
                _ => Event::Ignore,
            },
            "Stop" => Event::Done { key: key("claude"), who, text: folder },
            "SessionEnd" => Event::Ended { key: key("claude") },
            _ => Event::Ignore,
        };
    }

    // Codex `notify` passes its own JSON.
    if v.text("type") == "agent-turn-complete" {
        return Event::Done { key: v.text("thread-id").to_string(), who: "Codex".into(), text: project(v.text("cwd")) };
    }

    // The generic protocol for other tools.
    let who = if v.text("source").is_empty() { "Agent".to_string() } else { v.text("source").to_string() };
    let (title, body) = (v.text("title").to_string(), v.text("text").to_string());
    match v.text("event") {
        "working" => Event::Working { key: who },
        "waiting" => Event::Waiting { key: who.clone(), who, text: body },
        "done" => Event::Done { key: who.clone(), who, text: body },
        "ended" => Event::Ended { key: who },
        "ask" => Event::Permit { who, tool: title, detail: body, claude: false, ask: Ask::Tool { always: None } },
        "notify" => Event::Notice { title: if title.is_empty() { who } else { title }, text: body },
        _ => Event::Ignore,
    }
}

/// What to send back for a permission request. `decision`: `Some(true)` allow,
/// `Some(false)` deny, `None` no decision (the tool falls back to its own prompt).
pub fn permit_reply(claude: bool, decision: Option<bool>) -> String {
    match (claude, decision) {
        (true, Some(true)) => r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#.into(),
        (true, Some(false)) => r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Islet"}}}"#.into(),
        // Printing nothing leaves Claude Code's normal permission prompt in charge.
        (true, None) => String::new(),
        (false, Some(true)) => r#"{"decision":"allow"}"#.into(),
        (false, Some(false)) => r#"{"decision":"deny"}"#.into(),
        (false, None) => r#"{"decision":"none"}"#.into(),
    }
}

fn allow_with(extra: &str) -> String {
    format!(r#"{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"allow",{extra}}}}}}}"#)
}

/// Allow, and add Claude Code's own suggested rules (`suggestions`, a JSON
/// array) so it stops asking for this.
pub fn always_reply(suggestions: &str) -> String {
    allow_with(&format!(r#""updatedPermissions":{suggestions}"#))
}

/// Answers AskUserQuestion: the original `questions` array (JSON) and, per
/// question text, the chosen labels (several are joined with ", ").
pub fn answers_reply(raw_questions: &str, answers: &[(String, Vec<String>)]) -> String {
    let pairs: Vec<String> = answers.iter().map(|(q, labels)| format!("{}:{}", json::quote(q), json::quote(&labels.join(", ")))).collect();
    allow_with(&format!(r#""updatedInput":{{"questions":{raw_questions},"answers":{{{}}}}}"#, pairs.join(",")))
}

/// Approves ExitPlanMode with its own input echoed back (Claude Code wants
/// `updatedInput` for it).
pub fn plan_reply(input: &str) -> String {
    allow_with(&format!(r#""updatedInput":{input}"#))
}

/// Denies with a reason Claude reads (e.g. "keep planning").
pub fn deny_reply(message: &str) -> String {
    format!(r#"{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"deny","message":{}}}}}}}"#, json::quote(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_events() {
        let permission = r#"{"session_id":"abc","cwd":"C:\\dev\\shop","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"git push\n  origin main","description":"Push"}}"#;
        assert_eq!(interpret(permission), Event::Permit { who: "Claude Code".into(), tool: "Bash".into(), detail: "git push origin main".into(), claude: true, ask: Ask::Tool { always: None } });
        assert_eq!(interpret(r#"{"session_id":"abc","hook_event_name":"PreToolUse","tool_name":"Read"}"#), Event::Working { key: "abc".into() });
        assert_eq!(interpret(r#"{"session_id":"abc","cwd":"C:\\dev\\shop\\","hook_event_name":"Stop"}"#), Event::Done { key: "abc".into(), who: "Claude Code".into(), text: "shop".into() });
        assert_eq!(interpret(r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"x"}"#), Event::Ignore);
        assert_eq!(
            interpret(r#"{"session_id":"s","hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#),
            Event::Waiting { key: "s".into(), who: "Claude Code".into(), text: "Claude is waiting for your input".into() }
        );
        assert_eq!(interpret(r#"{"session_id":"abc","hook_event_name":"SessionEnd"}"#), Event::Ended { key: "abc".into() });
    }

    #[test]
    fn codex_and_generic_events() {
        assert_eq!(interpret(r#"{"type":"agent-turn-complete","thread-id":"t1","cwd":"/home/me/api"}"#), Event::Done { key: "t1".into(), who: "Codex".into(), text: "api".into() });
        assert_eq!(interpret(r#"{"event":"ask","source":"Deploy","title":"kubectl","text":"apply -f prod.yaml"}"#), Event::Permit { who: "Deploy".into(), tool: "kubectl".into(), detail: "apply -f prod.yaml".into(), claude: false, ask: Ask::Tool { always: None } });
        assert_eq!(interpret(r#"{"event":"notify","title":"Build","text":"finished"}"#), Event::Notice { title: "Build".into(), text: "finished".into() });
        assert_eq!(interpret("not json"), Event::Ignore);
        // A byte-order mark in front (PowerShell pipes add one) must not matter.
        assert_eq!(interpret("\u{feff}{\"event\":\"working\",\"source\":\"x\"}\r\n"), Event::Working { key: "x".into() });
    }

    #[test]
    fn questions_suggestions_and_plans() {
        let ask = r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which framework?","header":"Framework","options":[{"label":"React","description":"Components"},{"label":"Vue","description":"Progressive"}],"multiSelect":false},{"question":"Extras?","header":"Extras","options":[{"label":"Tests"},{"label":"Docs"}],"multiSelect":true}]}}"#;
        let Event::Permit { detail, ask: Ask::Questions { raw, list }, .. } = interpret(ask) else { panic!("not questions") };
        assert_eq!(detail, "Which framework?");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].options[1], ("Vue".to_string(), "Progressive".to_string()));
        assert!(!list[0].multi && list[1].multi);
        // The answer echoes the questions and maps question text to labels.
        let reply = json::parse(&answers_reply(&raw, &[("Which framework?".into(), vec!["Vue".into()]), ("Extras?".into(), vec!["Tests".into(), "Docs".into()])])).unwrap();
        let decision = reply.get("hookSpecificOutput").unwrap().get("decision").unwrap();
        assert_eq!(decision.text("behavior"), "allow");
        let input = decision.get("updatedInput").unwrap();
        assert!(matches!(input.get("questions"), Some(Value::Arr(q)) if q.len() == 2));
        assert_eq!(input.get("answers").unwrap().text("Extras?"), "Tests, Docs");

        let bash = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"npm test"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"npm test"}],"behavior":"allow","destination":"localSettings"}]}"#;
        let Event::Permit { ask: Ask::Tool { always: Some(always) }, .. } = interpret(bash) else { panic!("no suggestion") };
        let reply = json::parse(&always_reply(&always)).unwrap();
        assert!(matches!(reply.get("hookSpecificOutput").unwrap().get("decision").unwrap().get("updatedPermissions"), Some(Value::Arr(p)) if p.len() == 1));

        let plan = r#"{"hook_event_name":"PermissionRequest","tool_name":"ExitPlanMode","tool_input":{"plan":"1. Do this\n2. Then that"}}"#;
        let Event::Permit { detail, ask: Ask::Plan { input }, .. } = interpret(plan) else { panic!("not a plan") };
        assert_eq!(detail, "1. Do this 2. Then that");
        assert_eq!(json::parse(&plan_reply(&input)).unwrap().get("hookSpecificOutput").unwrap().get("decision").unwrap().get("updatedInput").unwrap().text("plan"), "1. Do this\n2. Then that");
        assert!(json::parse(&deny_reply("Keep \"planning\"")).is_some());
        // A malformed question list falls back to a plain allow / deny.
        assert!(matches!(interpret(r#"{"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"questions":[]}}"#), Event::Permit { ask: Ask::Tool { .. }, .. }));
    }

    #[test]
    fn replies_are_valid_json() {
        for claude in [true, false] {
            for decision in [Some(true), Some(false), None] {
                let text = permit_reply(claude, decision);
                assert!(text.is_empty() || json::parse(&text).is_some(), "{text}");
            }
        }
        let allow = json::parse(&permit_reply(true, Some(true))).unwrap();
        assert_eq!(allow.get("hookSpecificOutput").unwrap().get("decision").unwrap().text("behavior"), "allow");
        assert_eq!(permit_reply(true, None), "");
    }
}
