//! AI chat: Anthropic's Messages API or any OpenAI-compatible chat-completions
//! endpoint with the user's own API key, or **Claude through the user's own
//! Claude login** (the Claude Code that the Claude desktop app installs, run
//! as a helper process in print mode with every tool switched off), streamed.
//!
//! Off unless `chat = on` in the settings file. A key is typed into the Chat
//! tab once (or taken from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) and kept
//! encrypted for this Windows user (DPAPI), as is the conversation. A worker
//! thread talks to the provider; text arrives through [`take`] after a
//! `WM_APP_CHAT`.

use crate::json::{self, quote, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_CHAT: u32 = WM_APP + 19;

/// Anthropic's current default model (used when `chat_model` is empty).
const ANTHROPIC_MODEL: &str = "claude-opus-5-5";
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Lets Anthropic route a declined request to another model by itself.
const ANTHROPIC_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub(crate) const SYSTEM: &str = "You are the assistant inside Islet, a small desktop overlay with a narrow chat panel. Answer briefly and directly. Use only light Markdown: bold and short bullet lists.";
/// Messages sent as context (the newest ones).
pub(crate) const CONTEXT: usize = 40;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Provider {
    Anthropic,
    OpenAi,
    /// Claude Code with the user's Claude login (no key).
    Claude,
    /// Codex with the user's ChatGPT login (no key).
    Codex,
}

impl Provider {
    /// Whether an API key has to be typed in first.
    pub fn needs_key(self) -> bool {
        !matches!(self, Provider::Claude | Provider::Codex)
    }

    /// The value written to `chat_provider`.
    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Claude => "claude",
            Provider::Codex => "chatgpt",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub provider: Provider,
    /// Empty: the provider's default (Anthropic only).
    pub model: String,
    /// Base address of the OpenAI-compatible endpoint.
    pub url: String,
}

impl Settings {
    pub fn new(provider: &str, model: &str, url: &str) -> Settings {
        let provider = if provider.starts_with("openai") {
            Provider::OpenAi
        } else if provider.starts_with("claude") {
            Provider::Claude
        } else if provider.starts_with("chatgpt") || provider.starts_with("codex") {
            Provider::Codex
        } else {
            Provider::Anthropic
        };
        Settings { provider, model: model.to_string(), url: url.to_string() }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Content {
    /// A JPEG of at most 1568 px.
    Image { jpeg: Vec<u8>, w: u32, h: u32 },
    /// Text read out of a document.
    Text(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Attachment {
    pub name: String,
    /// "12 pages", "3 sheets", "1568 × 784"...
    pub detail: String,
    pub content: Content,
}

/// An attachment being read (or ready) for the next message.
pub struct Pending {
    pub id: u32,
    pub name: String,
    /// "Reading…", "Page 3 of 12".
    pub status: String,
    pub ready: Option<Attachment>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Message {
    pub user: bool,
    pub text: String,
    /// A failure note shown in place of an answer: never sent or saved.
    pub error: bool,
    /// Only on the user's messages.
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn new(user: bool, text: String, error: bool) -> Message {
        Message { user, text, error, attachments: Vec::new() }
    }

    pub(crate) fn sendable(&self) -> bool {
        !self.error && (!self.text.is_empty() || !self.attachments.is_empty())
    }
}

/// A message's `content`: a plain string, or blocks when it has attachments.
/// Pictures come first, then the documents' text in `<document>` tags, then
/// what the user typed.
fn content(provider: Provider, message: &Message) -> String {
    if message.attachments.is_empty() {
        return quote(&message.text);
    }
    let mut blocks = Vec::new();
    let mut text = String::new();
    for attachment in &message.attachments {
        match &attachment.content {
            Content::Image { jpeg, .. } => {
                let data = crate::attach::base64(jpeg);
                blocks.push(match provider {
                    Provider::Anthropic | Provider::Claude => format!("{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/jpeg\",\"data\":\"{data}\"}}}}"),
                    Provider::OpenAi | Provider::Codex => format!("{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/jpeg;base64,{data}\"}}}}"),
                });
            }
            Content::Text(body) => text.push_str(&format!("<document name=\"{}\">\n{body}\n</document>\n\n", attachment.name.replace('"', "'"))),
        }
    }
    text.push_str(&message.text);
    if !text.trim().is_empty() {
        blocks.push(format!("{{\"type\":\"text\",\"text\":{}}}", quote(text.trim_end())));
    }
    format!("[{}]", blocks.join(","))
}

// ------------------------------------------------------------------ requests --

fn endpoint(settings: &Settings) -> String {
    match settings.provider {
        Provider::Anthropic | Provider::Claude => "https://api.anthropic.com/v1/messages".to_string(),
        Provider::OpenAi | Provider::Codex => format!("{}/chat/completions", settings.url.trim_end_matches('/')),
    }
}

/// The JSON body of a streaming request for the conversation so far.
fn body(settings: &Settings, history: &[Message]) -> String {
    let turns: Vec<&Message> = history.iter().filter(|m| m.sendable()).collect();
    // The context must open with a user turn.
    let mut recent = &turns[turns.len().saturating_sub(CONTEXT)..];
    while recent.first().is_some_and(|m| !m.user) {
        recent = &recent[1..];
    }
    let list = recent.iter().map(|m| format!("{{\"role\":{},\"content\":{}}}", quote(if m.user { "user" } else { "assistant" }), content(settings.provider, m))).collect::<Vec<_>>().join(",");
    match settings.provider {
        Provider::Anthropic | Provider::Claude => {
            // The server-side fallback is offered for the default model only: a
            // model the user picked may not support it.
            let (model, fallbacks) = if settings.model.is_empty() { (ANTHROPIC_MODEL, "\"fallbacks\":\"default\",") } else { (settings.model.as_str(), "") };
            format!("{{\"model\":{},\"max_tokens\":64000,\"stream\":true,{fallbacks}\"system\":{},\"messages\":[{list}]}}", quote(model), quote(SYSTEM))
        }
        Provider::OpenAi | Provider::Codex => format!("{{\"model\":{},\"stream\":true,\"messages\":[{{\"role\":\"system\",\"content\":{}}},{list}]}}", quote(&settings.model), quote(SYSTEM)),
    }
}

fn headers(settings: &Settings, key: &str) -> Vec<(&'static str, String)> {
    let mut list = vec![("Content-Type", "application/json".to_string())];
    match settings.provider {
        Provider::Anthropic | Provider::Claude => {
            list.push(("x-api-key", key.to_string()));
            list.push(("anthropic-version", ANTHROPIC_VERSION.to_string()));
            if settings.model.is_empty() {
                list.push(("anthropic-beta", ANTHROPIC_FALLBACK_BETA.to_string()));
            }
        }
        Provider::OpenAi | Provider::Codex => list.push(("Authorization", format!("Bearer {key}"))),
    }
    list
}

/// Splits a server-sent-event stream into the payloads of its `data:` lines.
#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
}

impl Sse {
    fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut payloads = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim_end().strip_prefix("data:") {
                payloads.push(data.trim_start().to_string());
            }
        }
        payloads
    }
}

#[derive(PartialEq, Debug)]
enum Event {
    Text(String),
    /// The answer ended early; the note says why.
    Note(&'static str),
    Error(String),
    Nothing,
}

/// What one stream payload means.
fn event(provider: Provider, data: &str) -> Event {
    let Some(root) = json::parse(data) else { return Event::Nothing };
    if let Some(error) = root.get("error") {
        let message = error.text("message");
        return Event::Error(if message.is_empty() { "The provider reported an error".to_string() } else { message.to_string() });
    }
    match provider {
        Provider::Anthropic | Provider::Claude => match root.text("type") {
            "content_block_delta" => match root.get("delta") {
                Some(delta) if delta.text("type") == "text_delta" => Event::Text(delta.text("text").to_string()),
                _ => Event::Nothing,
            },
            // Always look at the stop reason: a refusal is a normal, successful answer.
            "message_delta" => match root.get("delta").map(|d| d.text("stop_reason")) {
                Some("refusal") => Event::Note("The model declined to answer this."),
                Some("max_tokens") => Event::Note("(The answer was cut off at the length limit.)"),
                _ => Event::Nothing,
            },
            _ => Event::Nothing,
        },
        Provider::OpenAi | Provider::Codex => {
            let Some(Value::Arr(choices)) = root.get("choices") else { return Event::Nothing };
            match choices.first().and_then(|c| c.get("delta")).map(|d| d.text("content")) {
                Some(text) if !text.is_empty() => Event::Text(text.to_string()),
                _ => Event::Nothing,
            }
        }
    }
}

/// A readable reason from a provider's error body.
fn failure(status: u32, body: &str) -> String {
    let detail = json::parse(body).and_then(|root| root.get("error").map(|e| e.text("message").to_string())).filter(|m| !m.is_empty());
    match (status, detail) {
        (401 | 403, _) => "The API key was not accepted. Type /key to enter another one.".to_string(),
        (429, _) => "The provider is rate limiting or out of credit. Try again in a moment.".to_string(),
        (_, Some(detail)) => format!("{detail} ({status})"),
        (_, None) => format!("The provider answered {status}"),
    }
}

// -------------------------------------------------------------------- worker --

struct Stream {
    text: String,
    done: Option<Result<(), String>>,
    /// The Claude Code / Codex conversation the answer was added to.
    session: Option<crate::cli::Session>,
}

static STREAM: Mutex<Stream> = Mutex::new(Stream { text: String::new(), done: None, session: None });
/// One `WM_APP_CHAT` in flight at a time, however fast the text arrives.
static POSTED: AtomicBool = AtomicBool::new(false);
/// Bumped to abandon the answer that is being streamed.
static RUN: AtomicU32 = AtomicU32::new(0);

/// New text since the last call, how the answer ended once it has, and (with
/// a login provider) the conversation that now holds it.
pub fn take() -> (String, Option<Result<(), String>>, Option<crate::cli::Session>) {
    POSTED.store(false, Relaxed);
    match STREAM.lock() {
        Ok(mut stream) => (std::mem::take(&mut stream.text), stream.done.take(), stream.session.take()),
        Err(_) => (String::new(), None, None),
    }
}

/// Stops the answer in progress (its remaining text is dropped).
pub fn cancel() {
    RUN.fetch_add(1, Relaxed);
    if let Ok(mut stream) = STREAM.lock() {
        *stream = Stream { text: String::new(), done: None, session: None };
    }
}

fn push(target: isize, run: u32, text: &str, done: Option<Result<(), String>>) {
    if RUN.load(Relaxed) != run {
        return;
    }
    if let Ok(mut stream) = STREAM.lock() {
        stream.text.push_str(text);
        if done.is_some() {
            stream.done = done;
        }
    }
    if !POSTED.swap(true, Relaxed) {
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_CHAT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Asks the provider to continue `history` (which ends with the user's
/// message). With a login provider, `session` is the Claude Code / Codex
/// conversation it may continue.
pub fn send(target: HWND, settings: Settings, key: String, history: Vec<Message>, session: Option<crate::cli::Session>) {
    cancel();
    let run = RUN.load(Relaxed);
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-chat".into()).stack_size(512 * 1024).spawn(move || {
        if let Some(tool) = crate::cli::Tool::of(settings.provider) {
            let (done, kept) = crate::cli::ask(tool, &settings, &history, session, &mut |text| push(target, run, text, None), &|| RUN.load(Relaxed) != run);
            if RUN.load(Relaxed) == run {
                if let Ok(mut stream) = STREAM.lock() {
                    stream.session = kept;
                }
            }
            return push(target, run, "", Some(done));
        }
        if settings.provider == Provider::OpenAi && settings.model.is_empty() {
            return push(target, run, "", Some(Err("Set chat_model in the settings file for the openai provider.".to_string())));
        }
        let payload = body(&settings, &history);
        let list = headers(&settings, &key);
        let borrowed: Vec<(&str, &str)> = list.iter().map(|(name, value)| (*name, value.as_str())).collect();
        let (mut sse, mut refused, mut problem) = (Sse::default(), Vec::new(), None);
        let result = crate::net::request(&endpoint(&settings), &borrowed, Some(payload.as_bytes()), &mut |status, chunk| {
            if status != 200 {
                refused.extend_from_slice(chunk);
                return refused.len() < 64 * 1024;
            }
            for data in sse.feed(chunk) {
                match event(settings.provider, &data) {
                    Event::Text(text) => push(target, run, &text, None),
                    Event::Note(note) => push(target, run, &format!("\n\n{note}"), None),
                    Event::Error(why) => problem = Some(why),
                    Event::Nothing => {}
                }
            }
            problem.is_none() && RUN.load(Relaxed) == run
        });
        let done = match (result, problem) {
            (_, Some(why)) | (Err(why), None) => Err(why),
            (Ok(200), None) => Ok(()),
            (Ok(status), None) => Err(failure(status, &String::from_utf8_lossy(&refused))),
        };
        push(target, run, "", Some(done));
    });
}

// ---------------------------------------------------------------- model lists --

/// Posted when [`fetch_models`] has a list; read it with [`models`].
pub const WM_APP_MODELS: u32 = WM_APP + 27;
/// A provider's models as (value for `chat_model`, label).
pub type ModelList = Vec<(String, String)>;
static MODELS: Mutex<Option<(Provider, ModelList)>> = Mutex::new(None);

/// The models offered for a provider, as (value for `chat_model`, label).
/// An empty value means the provider's default.
pub fn preset_models(provider: Provider) -> Vec<(String, String)> {
    let list: &[(&str, &str)] = match provider {
        // Claude Code takes these aliases for the newest of each family.
        Provider::Claude => &[("", "Default"), ("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")],
        Provider::Anthropic => &[("", "Default (Opus 5.5)"), ("claude-opus-5-5", "Opus 5.5"), ("claude-sonnet-5-5", "Sonnet 5.5"), ("claude-haiku-5-5", "Haiku 5.5")],
        Provider::Codex => &[("", "Default")],
        Provider::OpenAi => &[],
    };
    list.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect()
}

/// Asks the provider which models there are, on a worker (Codex's own list,
/// or an OpenAI-compatible server's `/models`), then posts `WM_APP_MODELS`.
pub fn fetch_models(target: HWND, provider: Provider, url: String, key: String) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-models".into()).stack_size(512 * 1024).spawn(move || {
        let mut list = preset_models(provider);
        match provider {
            Provider::Codex => list.extend(crate::cli::codex_models()),
            Provider::OpenAi => {
                let mut body = Vec::new();
                let auth = format!("Bearer {key}");
                let headers: Vec<(&str, &str)> = if key.is_empty() { Vec::new() } else { vec![("Authorization", auth.as_str())] };
                let address = format!("{}/models", url.trim_end_matches('/'));
                if let Ok(200) = crate::net::request(&address, &headers, None, &mut |_, chunk| {
                    body.extend_from_slice(chunk);
                    body.len() < 4 << 20
                }) {
                    if let Some(Value::Arr(data)) = json::parse(&String::from_utf8_lossy(&body)).as_ref().and_then(|root| root.get("data")) {
                        let mut ids: Vec<String> = data.iter().map(|m| m.text("id").to_string()).filter(|id| !id.is_empty()).collect();
                        ids.sort();
                        list.extend(ids.into_iter().map(|id| (id.clone(), id)));
                    }
                }
            }
            _ => {}
        }
        if let Ok(mut models) = MODELS.lock() {
            *models = Some((provider, list));
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_MODELS, WPARAM(0), LPARAM(0));
        }
    });
}

/// The last list [`fetch_models`] found, and for which provider.
pub fn models() -> Option<(Provider, ModelList)> {
    MODELS.lock().ok().and_then(|m| m.clone())
}

// ------------------------------------------------------------------- storage --

fn file(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Islet").join(name))
}

pub fn load_key() -> Option<String> {
    let sealed = std::fs::read(file("chat.key")?).ok()?;
    String::from_utf8(crate::clipboard::unseal(&sealed)?).ok().filter(|key| !key.is_empty())
}

/// The key to use for `provider`: none needed for the Claude login; else the
/// saved one, else the usual environment variable (`ANTHROPIC_API_KEY` or
/// `OPENAI_API_KEY`), so a key that is already set up for other tools just works.
pub fn key_for(provider: Provider) -> Option<String> {
    if !provider.needs_key() {
        return Some(String::new());
    }
    let variable = if provider == Provider::OpenAi { "OPENAI_API_KEY" } else { "ANTHROPIC_API_KEY" };
    load_key().or_else(|| std::env::var(variable).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()))
}

pub fn save_key(key: &str) -> bool {
    let (Some(path), Some(sealed)) = (file("chat.key"), crate::clipboard::seal(key.as_bytes())) else { return false };
    std::fs::write(path, sealed).is_ok()
}

pub fn forget_key() {
    if let Some(path) = file("chat.key") {
        let _ = std::fs::remove_file(path);
    }
}

fn encode(messages: &[Message]) -> String {
    let attachment = |a: &Attachment| {
        let body = match &a.content {
            Content::Image { jpeg, w, h } => format!("\"image\":\"{}\",\"w\":{w},\"h\":{h}", crate::attach::base64(jpeg)),
            Content::Text(text) => format!("\"body\":{}", quote(text)),
        };
        format!("{{\"name\":{},\"detail\":{},{body}}}", quote(&a.name), quote(&a.detail))
    };
    let items = messages
        .iter()
        .filter(|m| m.sendable())
        .map(|m| format!("{{\"user\":{},\"text\":{},\"attachments\":[{}]}}", m.user, quote(&m.text), m.attachments.iter().map(attachment).collect::<Vec<_>>().join(",")))
        .collect::<Vec<_>>();
    format!("[{}]", items.join(","))
}

fn decode(text: &str) -> Vec<Message> {
    let number = |v: Option<&Value>| match v {
        Some(Value::Num(n)) => n.parse().unwrap_or(0),
        _ => 0,
    };
    let attachment = |item: &Value| {
        let content = match item.get("image").and_then(Value::str) {
            Some(data) => Content::Image { jpeg: crate::attach::unbase64(data)?, w: number(item.get("w")), h: number(item.get("h")) },
            None => Content::Text(item.text("body").to_string()),
        };
        Some(Attachment { name: item.text("name").to_string(), detail: item.text("detail").to_string(), content })
    };
    match json::parse(text) {
        Some(Value::Arr(items)) => items
            .iter()
            .map(|item| Message {
                user: item.get("user") == Some(&Value::Bool(true)),
                text: item.text("text").to_string(),
                error: false,
                attachments: match item.get("attachments") {
                    Some(Value::Arr(list)) => list.iter().filter_map(attachment).collect(),
                    _ => Vec::new(),
                },
            })
            .filter(Message::sendable)
            .collect(),
        _ => Vec::new(),
    }
}

/// The saved conversation (empty when there is none).
pub fn load() -> Vec<Message> {
    file("chat.bin").and_then(|path| std::fs::read(path).ok()).and_then(|sealed| crate::clipboard::unseal(&sealed)).map(|plain| decode(&String::from_utf8_lossy(&plain))).unwrap_or_default()
}

pub fn save(messages: &[Message]) {
    if let (Some(path), Some(sealed)) = (file("chat.bin"), crate::clipboard::seal(encode(messages).as_bytes())) {
        let _ = std::fs::write(path, sealed);
    }
}

// ------------------------------------------------------------------ markdown --

/// Light Markdown for display: `**bold**` and `# headings` become bold runs
/// (start, length in UTF-16 units of the returned text), `- ` / `* ` bullets
/// become dots. Everything else is shown as written.
pub fn render(text: &str) -> (Vec<u16>, Vec<(u32, u32)>) {
    let mut out: Vec<u16> = Vec::new();
    let mut bold = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push(b'\n' as u16);
        }
        let indent = line.len() - line.trim_start().len();
        let mut rest = line.trim_start();
        out.extend(line[..indent].encode_utf16());
        let start = out.len() as u32;
        let heading = rest.starts_with('#') && rest.trim_start_matches('#').starts_with(' ');
        if heading {
            rest = rest.trim_start_matches('#').trim_start();
        } else if let Some(item) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
            out.extend("\u{2022} ".encode_utf16());
            rest = item;
        }
        // Inline bold: text between pairs of "**".
        let mut open: Option<u32> = None;
        let mut pieces = rest.split("**").peekable();
        while let Some(piece) = pieces.next() {
            out.extend(piece.encode_utf16());
            if pieces.peek().is_some() {
                match open.take() {
                    Some(from) => bold.push((from, out.len() as u32 - from)),
                    None => open = Some(out.len() as u32),
                }
            }
        }
        if heading {
            bold.retain(|&(from, _)| from < start);
            bold.push((start, out.len() as u32 - start));
        }
    }
    bold.retain(|&(_, len)| len > 0);
    (out, bold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(user: bool, text: &str) -> Message {
        Message::new(user, text.to_string(), false)
    }

    fn with_files(text: &str) -> Message {
        let mut m = message(true, text);
        m.attachments = vec![
            Attachment { name: "Report \"Q3\".pdf".into(), detail: "2 pages".into(), content: Content::Text("Sales up".into()) },
            Attachment { name: "shot.png".into(), detail: "2 \u{00D7} 1".into(), content: Content::Image { jpeg: vec![0xff, 0xd8, 1], w: 2, h: 1 } },
        ];
        m
    }

    #[test]
    fn attachments_become_content_blocks() {
        let m = with_files("Summarise");
        let Some(Value::Arr(blocks)) = json::parse(&content(Provider::Anthropic, &m)) else { panic!() };
        assert_eq!(blocks.len(), 2);
        // The picture first, then documents and the question in one text block.
        assert_eq!(blocks[0].text("type"), "image");
        let source = blocks[0].get("source").unwrap();
        assert_eq!((source.text("media_type"), source.text("data")), ("image/jpeg", "/9gB"));
        assert_eq!(blocks[1].text("text"), "<document name=\"Report 'Q3'.pdf\">\nSales up\n</document>\n\nSummarise");
        let Some(Value::Arr(blocks)) = json::parse(&content(Provider::OpenAi, &m)) else { panic!() };
        assert_eq!(blocks[0].get("image_url").unwrap().text("url"), "data:image/jpeg;base64,/9gB");
        // A message without attachments stays a plain string; a picture alone has no text block.
        assert_eq!(content(Provider::Anthropic, &message(true, "hi")), "\"hi\"");
        let mut picture_only = with_files("");
        picture_only.attachments.remove(0);
        let Some(Value::Arr(blocks)) = json::parse(&content(Provider::Anthropic, &picture_only)) else { panic!() };
        assert_eq!(blocks.len(), 1);
        // Saved and read back with the attachments.
        let saved = vec![m, message(false, "Done")];
        assert_eq!(decode(&encode(&saved)), saved);
    }

    #[test]
    fn login_providers() {
        for p in [Provider::Claude, Provider::Codex, Provider::Anthropic, Provider::OpenAi] {
            assert_eq!(Settings::new(p.name(), "", "").provider, p);
        }
        assert!(!Provider::Claude.needs_key() && !Provider::Codex.needs_key() && Provider::Anthropic.needs_key());
        assert_eq!(key_for(Provider::Codex), Some(String::new()));
        // Every list starts with the provider's default where there is one.
        assert_eq!(preset_models(Provider::Claude)[0].0, "");
        assert!(preset_models(Provider::Anthropic).iter().any(|(v, _)| v == "claude-haiku-5-5"));
    }

    #[test]
    fn request_shapes() {
        let history = [message(false, "stray answer"), message(true, "Hi \"there\""), message(false, "Hello"), Message::new(false, "boom".into(), true), message(true, "Next")];
        let anthropic = Settings::new("anthropic", "", "");
        let parsed = json::parse(&body(&anthropic, &history)).unwrap();
        assert_eq!((parsed.text("model"), parsed.text("fallbacks")), ("claude-opus-5-5", "default"));
        assert_eq!(parsed.get("stream"), Some(&Value::Bool(true)));
        // Starts with a user turn; the error note is left out.
        let Some(Value::Arr(turns)) = parsed.get("messages") else { panic!() };
        assert_eq!(turns.iter().map(|t| t.text("role")).collect::<Vec<_>>(), ["user", "assistant", "user"]);
        assert_eq!(turns[0].text("content"), "Hi \"there\"");
        assert!(headers(&anthropic, "k").iter().any(|(name, value)| *name == "anthropic-beta" && value == ANTHROPIC_FALLBACK_BETA));
        // A model the user chose gets no fallback parameter or beta header.
        let chosen = Settings::new("anthropic", "claude-haiku-5-5", "");
        assert!(json::parse(&body(&chosen, &history)).unwrap().get("fallbacks").is_none());
        assert!(!headers(&chosen, "k").iter().any(|(name, _)| *name == "anthropic-beta"));

        let open = Settings::new("openai", "some-model", "http://localhost:11434/v1/");
        assert_eq!(endpoint(&open), "http://localhost:11434/v1/chat/completions");
        let parsed = json::parse(&body(&open, &history)).unwrap();
        let Some(Value::Arr(turns)) = parsed.get("messages") else { panic!() };
        assert_eq!((parsed.text("model"), turns[0].text("role"), turns.len()), ("some-model", "system", 4));
        assert!(headers(&open, "k").contains(&("Authorization", "Bearer k".to_string())));
    }

    #[test]
    fn stream_events() {
        let mut sse = Sse::default();
        assert!(sse.feed(b"event: content_block_delta\ndata: {\"type\":\"content_block_del").is_empty());
        let data = sse.feed(b"ta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\ndata: [DONE]\n");
        assert_eq!(data.len(), 2);
        assert_eq!(event(Provider::Anthropic, &data[0]), Event::Text("Hel".into()));
        assert_eq!(event(Provider::OpenAi, &data[1]), Event::Nothing);
        assert_eq!(event(Provider::Anthropic, r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#), Event::Note("The model declined to answer this."));
        assert_eq!(event(Provider::Anthropic, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#), Event::Nothing);
        assert_eq!(event(Provider::Anthropic, r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"x"}}"#), Event::Nothing);
        assert_eq!(event(Provider::Anthropic, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), Event::Error("Overloaded".into()));
        assert_eq!(event(Provider::OpenAi, r#"{"choices":[{"delta":{"content":"lo"}}]}"#), Event::Text("lo".into()));
        assert_eq!(event(Provider::OpenAi, r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#), Event::Nothing);
        assert!(failure(401, "{}").contains("/key"));
        assert_eq!(failure(400, r#"{"error":{"message":"bad model"}}"#), "bad model (400)");
    }

    #[test]
    fn history_round_trip_and_markdown() {
        let messages = vec![message(true, "Line\n\"two\""), message(false, "**Answer**"), Message::new(false, "failed".into(), true)];
        assert_eq!(decode(&encode(&messages)), messages[..2]);
        assert!(decode("garbage").is_empty());

        let text = |units: &[u16]| String::from_utf16_lossy(units);
        let (shown, bold) = render("# Title\nSome **bold** text\n- one\n  * two\n2 * 3 ** 4");
        assert_eq!(text(&shown), "Title\nSome bold text\n\u{2022} one\n  \u{2022} two\n2 * 3  4");
        assert_eq!(bold, [(0, 5), (11, 4)]);
        assert_eq!(text(&shown[11..15]), "bold");
        assert_eq!(render(""), (Vec::new(), Vec::new()));
    }
}
