//! What the single pill slot shows.
//!
//! Persistent sources (a running download, a running timer, music, the clock)
//! are ranked by [`persistent`]. Short-lived alerts are [`Toast`]s in a
//! [`Queue`]: the most important one is shown first, the rest wait their turn.
//! Important toasts (a finished timer, a reminder) survive game-suspend and
//! are shown after waking; trivial ones are dropped.

use crate::render::Rgba;
use crate::state::Pill;
use crate::watch::Finished;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Bell,
    Timer,
    Keyboard,
    Shield,
    Info,
    /// A coding agent.
    Spark,
}

/// A pending permission request from a coding agent.
#[derive(Clone, Copy, Debug)]
pub struct Permit {
    /// Request id to answer through `agent::reply`.
    pub id: u64,
    /// Answer in Claude Code's hook format (otherwise the generic one).
    pub claude: bool,
}

#[derive(Clone, Debug)]
pub struct Toast {
    /// Higher is shown first.
    pub priority: u8,
    /// Kept across game-suspend and shown until its time is up after waking.
    pub important: bool,
    pub seconds: f32,
    pub icon: Icon,
    pub title: String,
    pub text: String,
    pub accent: Rgba,
    /// A completed download: shown as the taller "Downloaded" card.
    pub done: Option<Finished>,
    /// A permission request: shown as the Allow / Deny card until answered.
    pub permit: Option<Permit>,
    /// Toasts with the same key replace each other (e.g. repeated Caps Lock presses).
    pub key: Option<&'static str>,
}

impl Toast {
    pub fn new(priority: u8, seconds: f32, icon: Icon, title: impl Into<String>, text: impl Into<String>, accent: Rgba) -> Self {
        Self { priority, important: false, seconds, icon, title: title.into(), text: text.into(), accent, done: None, permit: None, key: None }
    }

    pub fn important(mut self) -> Self {
        self.important = true;
        self
    }

    pub fn keyed(mut self, key: &'static str) -> Self {
        self.key = Some(key);
        self
    }
}

/// Key of Windows notifications that come from an AI agent (see [`from_ai_agent`]).
pub const AI_NOTE: &str = "ai-note";

/// A Windows notification from a coding agent or AI app (Claude, Codex, ChatGPT):
/// when the agent's hooks talk to Islet too, it is the same news twice.
pub fn from_ai_agent(app: &str, title: &str, body: &str) -> bool {
    let all = format!("{app} {title} {body}").to_lowercase();
    ["claude", "codex", "chatgpt", "anthropic", "openai"].iter().any(|word| all.contains(word))
}

pub const PERMIT: u8 = 100;
pub const TIMER_DONE: u8 = 90;
pub const AGENT_WAITING: u8 = 75;
pub const AGENT_DONE: u8 = 70;
pub const REMINDER: u8 = 80;
pub const DOWNLOAD_DONE: u8 = 60;
pub const NOTIFICATION: u8 = 50;
pub const VPN: u8 = 40;
pub const INFO: u8 = 30;
pub const KEYBOARD: u8 = 20;

#[derive(Default)]
pub struct Queue {
    items: Vec<Toast>,
}

impl Queue {
    pub fn push(&mut self, toast: Toast) {
        if let Some(key) = toast.key {
            self.items.retain(|t| t.key != Some(key));
        }
        self.items.push(toast);
    }

    /// Removes and returns the most important toast (oldest first among equals).
    pub fn pop(&mut self) -> Option<Toast> {
        let best = self.items.iter().map(|t| t.priority).max()?;
        let index = self.items.iter().position(|t| t.priority == best)?;
        Some(self.items.remove(index))
    }

    /// Going into deep sleep: only important toasts are worth showing later.
    pub fn keep_important(&mut self) {
        self.items.retain(|t| t.important);
    }

    /// Removes and returns every queued permission request.
    pub fn take_permits(&mut self) -> Vec<Permit> {
        let permits = self.items.iter().filter_map(|t| t.permit).collect();
        self.items.retain(|t| t.permit.is_none());
        permits
    }

    /// Drops every queued toast with this key.
    pub fn drop_key(&mut self, key: &'static str) {
        self.items.retain(|t| t.key != Some(key));
    }

    pub fn has_permit(&self) -> bool {
        self.items.iter().any(|t| t.permit.is_some())
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// The pill to show when no toast is up: voice input > screen recording >
/// download progress > running timer > music > clock.
pub fn persistent(listening: bool, recording: bool, download: bool, timer: bool, music: bool) -> Pill {
    if listening {
        Pill::Listen
    } else if recording {
        Pill::Record
    } else if download {
        Pill::Download
    } else if timer {
        Pill::Timer
    } else if music {
        Pill::Music
    } else {
        Pill::Clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_notifications_and_keys() {
        assert!(from_ai_agent("Claude", "Claude needs your permission", ""));
        assert!(from_ai_agent("Windows Terminal", "Claude Code", "Waiting for input"));
        assert!(from_ai_agent("Codex", "", "Task finished"));
        assert!(!from_ai_agent("Discord", "Anna", "see you at 5"));
        let mut q = Queue::default();
        q.push(Toast::new(NOTIFICATION, 4.0, Icon::Bell, "Claude", "done", Rgba::hex(0)).keyed(AI_NOTE));
        q.push(Toast::new(NOTIFICATION, 4.0, Icon::Bell, "Mail", "hi", Rgba::hex(0)));
        q.drop_key(AI_NOTE);
        assert_eq!(q.pop().map(|t| t.title), Some("Mail".to_string()));
        assert!(q.is_empty());
    }

    fn toast(priority: u8, title: &str) -> Toast {
        Toast::new(priority, 1.0, Icon::Info, title, "", Rgba::hex(0))
    }

    #[test]
    fn most_important_first_then_oldest() {
        let mut q = Queue::default();
        q.push(toast(KEYBOARD, "caps"));
        q.push(toast(REMINDER, "first reminder"));
        q.push(toast(TIMER_DONE, "timer"));
        q.push(toast(REMINDER, "second reminder"));
        let order: Vec<String> = std::iter::from_fn(|| q.pop()).map(|t| t.title).collect();
        assert_eq!(order, ["timer", "first reminder", "second reminder", "caps"]);
    }

    #[test]
    fn keyed_toasts_replace_and_suspend_keeps_important() {
        let mut q = Queue::default();
        q.push(toast(KEYBOARD, "Caps Lock on").keyed("caps"));
        q.push(toast(KEYBOARD, "Caps Lock off").keyed("caps"));
        q.push(toast(TIMER_DONE, "timer").important());
        q.keep_important();
        assert_eq!(q.pop().map(|t| t.title).as_deref(), Some("timer"));
        assert!(q.is_empty());
    }

    #[test]
    fn persistent_priority() {
        assert_eq!(persistent(true, true, true, true, true), Pill::Listen);
        assert_eq!(persistent(false, true, true, true, true), Pill::Record);
        assert_eq!(persistent(false, false, true, true, true), Pill::Download);
        assert_eq!(persistent(false, false, false, true, true), Pill::Timer);
        assert_eq!(persistent(false, false, false, false, true), Pill::Music);
        assert_eq!(persistent(false, false, false, false, false), Pill::Clock);
    }
}
