//! The command palette backend: a registry of [`Command`]s that can be
//! searched, checked for availability against a [`CommandContext`], and
//! executed to produce [`AppIntent`]s.
//!
//! Executing a command never mutates anything in this crate: a handler is a
//! pure function from context to a list of intents.

use std::borrow::{Borrow, Cow};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::Serialize;

use litecord_types::capability::{Capability, CapabilitySet};
use litecord_types::ids::{ConversationId, MessageId};

use crate::intent::AppIntent;

/// Stable identifier for a command, e.g. `"appearance.toggle_compact"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CommandId(pub Cow<'static, str>);

impl CommandId {
    pub fn new(id: impl Into<Cow<'static, str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for CommandId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&'static str> for CommandId {
    fn from(s: &'static str) -> Self {
        Self(Cow::Borrowed(s))
    }
}

impl From<String> for CommandId {
    fn from(s: String) -> Self {
        Self(Cow::Owned(s))
    }
}

/// A parsed keyboard shortcut. `ctrl_or_cmd` is platform-neutral: it means
/// "the primary modifier", which is Ctrl on Windows/Linux and Cmd on macOS.
/// The canonical textual/`Display` form always spells it `Ctrl`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keybind {
    pub ctrl_or_cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

/// Named, non-single-character keys with a canonical spelling.
const NAMED_KEYS: &[&str] = &[
    "Up",
    "Down",
    "Left",
    "Right",
    "Enter",
    "Escape",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Insert",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid keybind {input:?}: {reason}")]
pub struct KeybindParseError {
    pub input: String,
    pub reason: String,
}

impl Keybind {
    fn normalize_key(raw: &str) -> Result<String, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("missing key".into());
        }
        let mut chars = raw.chars();
        let Some(first) = chars.next() else {
            return Err("missing key".into());
        };
        if chars.next().is_none() {
            // Single character: letters are uppercased, everything else
            // (digits, punctuation) is kept as-is.
            return Ok(if first.is_ascii_alphabetic() {
                first.to_ascii_uppercase().to_string()
            } else {
                first.to_string()
            });
        }
        for named in NAMED_KEYS {
            if named.eq_ignore_ascii_case(raw) {
                return Ok((*named).to_string());
            }
        }
        Err(format!("unrecognized key {raw:?}"))
    }
}

impl FromStr for Keybind {
    type Err = KeybindParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason: String| KeybindParseError {
            input: s.to_string(),
            reason,
        };
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) || parts.len() < 2 {
            return Err(err(
                "expected at least one modifier and a key, e.g. \"Ctrl+K\"".into(),
            ));
        }
        let (modifiers, key_part) = parts.split_at(parts.len() - 1);
        let key_part = key_part[0];

        let mut ctrl_or_cmd = false;
        let mut shift = false;
        let mut alt = false;
        for m in modifiers {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => ctrl_or_cmd = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                other => return Err(err(format!("unknown modifier {other:?}"))),
            }
        }
        let key = Keybind::normalize_key(key_part).map_err(err)?;
        if !(ctrl_or_cmd || shift || alt) {
            return Err(err("at least one modifier is required".into()));
        }

        Ok(Keybind {
            ctrl_or_cmd,
            shift,
            alt,
            key,
        })
    }
}

impl fmt::Display for Keybind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl_or_cmd {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(self.key.clone());
        f.write_str(&parts.join("+"))
    }
}

/// A precondition a command needs met before it can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    Capability(Capability),
    Online,
    ActiveConversation,
    SelectedMessage,
}

/// Snapshot of application state a command's availability and handler can
/// read. Building one, like everything else in this crate, has no side
/// effects.
#[derive(Debug, Clone, Default)]
pub struct CommandContext {
    pub capabilities: CapabilitySet,
    pub online: bool,
    pub active_conversation: Option<ConversationId>,
    pub selected_message: Option<MessageId>,
    pub settings: BTreeMap<String, serde_json::Value>,
}

/// A command handler: a pure function from context to the intents it wants
/// carried out.
pub type CommandHandler = Arc<dyn Fn(&CommandContext) -> Vec<AppIntent> + Send + Sync>;

/// One entry in the command palette.
pub struct Command {
    pub id: CommandId,
    pub name: String,
    pub description: String,
    pub category: String,
    pub keywords: Vec<String>,
    pub shortcut: Option<Keybind>,
    pub requires: Vec<Requirement>,
    pub handler: CommandHandler,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("category", &self.category)
            .field("keywords", &self.keywords)
            .field("shortcut", &self.shortcut)
            .field("requires", &self.requires)
            .field("handler", &"<handler fn>")
            .finish()
    }
}

impl Command {
    pub fn new(
        id: impl Into<CommandId>,
        name: impl Into<String>,
        handler: impl Fn(&CommandContext) -> Vec<AppIntent> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            category: String::new(),
            keywords: Vec::new(),
            shortcut: None,
            requires: Vec::new(),
            handler: Arc::new(handler),
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    #[must_use]
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = category.into();
        self
    }

    #[must_use]
    pub fn keywords<I, S>(mut self, keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    /// Parses `shortcut` as a [`Keybind`]. An invalid shortcut is logged and
    /// leaves the command with no shortcut, rather than failing the whole
    /// registration.
    #[must_use]
    pub fn shortcut(mut self, shortcut: &str) -> Self {
        match shortcut.parse::<Keybind>() {
            Ok(kb) => self.shortcut = Some(kb),
            Err(e) => {
                tracing::warn!(command = %self.id, shortcut, error = %e, "invalid shortcut, ignoring");
            }
        }
        self
    }

    #[must_use]
    pub fn requires(mut self, requirement: Requirement) -> Self {
        self.requires.push(requirement);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("command \"{0}\" is already registered")]
    DuplicateId(CommandId),
    #[error("shortcut \"{shortcut}\" is already bound to \"{existing}\"")]
    DuplicateShortcut {
        shortcut: String,
        existing: CommandId,
    },
    #[error("command \"{0}\" not found")]
    NotFound(CommandId),
    #[error("command \"{id}\" is unavailable: {reason}")]
    Unavailable { id: CommandId, reason: String },
}

/// One search result: a command plus how well it matched and whether it can
/// currently run.
#[derive(Debug, Clone, Serialize)]
pub struct CommandMatch {
    pub id: CommandId,
    pub name: String,
    pub description: String,
    pub category: String,
    pub shortcut: Option<String>,
    pub score: i32,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

fn split_words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    'outer: for n in needle.chars() {
        for h in haystack.by_ref() {
            if h == n {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Score tiers, highest first. `None` means no match at all.
fn fuzzy_score(query_lower: &str, text: &str) -> Option<i32> {
    let text_lower = text.to_lowercase();
    if text_lower.starts_with(query_lower) {
        return Some(300);
    }
    if split_words(&text_lower).any(|w| w.starts_with(query_lower)) {
        return Some(200);
    }
    if is_subsequence(query_lower, &text_lower) {
        return Some(100);
    }
    None
}

/// Registry of installed commands, keyed by id, with a shortcut index.
#[derive(Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<CommandId, Command>,
    shortcuts: HashMap<Keybind, CommandId>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, command: Command) -> Result<(), CommandError> {
        if self.commands.contains_key(&command.id) {
            return Err(CommandError::DuplicateId(command.id));
        }
        if let Some(shortcut) = &command.shortcut {
            if let Some(existing) = self.shortcuts.get(shortcut) {
                return Err(CommandError::DuplicateShortcut {
                    shortcut: shortcut.to_string(),
                    existing: existing.clone(),
                });
            }
        }
        if let Some(shortcut) = command.shortcut.clone() {
            self.shortcuts.insert(shortcut, command.id.clone());
        }
        self.commands.insert(command.id.clone(), command);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.get(id)
    }

    pub fn by_shortcut(&self, keybind: &Keybind) -> Option<&Command> {
        let id = self.shortcuts.get(keybind)?;
        self.commands.get(id)
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    /// Checks `command`'s [`Requirement`]s against `ctx`, returning the
    /// reason for the first one that is not met.
    pub fn availability(&self, command: &Command, ctx: &CommandContext) -> Result<(), String> {
        for req in &command.requires {
            match req {
                Requirement::Capability(cap) => {
                    if !ctx.capabilities.is_usable(*cap) {
                        return Err(format!("requires {} support", capability_label(*cap)));
                    }
                }
                Requirement::Online => {
                    if !ctx.online {
                        return Err("offline".to_string());
                    }
                }
                Requirement::ActiveConversation => {
                    if ctx.active_conversation.is_none() {
                        return Err("no active conversation".to_string());
                    }
                }
                Requirement::SelectedMessage => {
                    if ctx.selected_message.is_none() {
                        return Err("no selected message".to_string());
                    }
                }
            }
        }
        Ok(())
    }

    /// Fuzzy-searches commands by name, keywords and id. An empty query
    /// lists every command, sorted by category then name.
    pub fn search(&self, query: &str, ctx: &CommandContext, limit: usize) -> Vec<CommandMatch> {
        let query = query.trim();
        let mut matches: Vec<CommandMatch> = if query.is_empty() {
            self.commands
                .values()
                .map(|c| self.to_match(c, ctx, 0))
                .collect()
        } else {
            let query_lower = query.to_lowercase();
            self.commands
                .values()
                .filter_map(|c| {
                    let mut best: Option<i32> = fuzzy_score(&query_lower, &c.name);
                    for kw in &c.keywords {
                        if let Some(s) = fuzzy_score(&query_lower, kw) {
                            best = Some(best.map_or(s, |b| b.max(s)));
                        }
                    }
                    if let Some(s) = fuzzy_score(&query_lower, c.id.as_str()) {
                        best = Some(best.map_or(s, |b| b.max(s)));
                    }
                    best.map(|score| self.to_match(c, ctx, score))
                })
                .collect()
        };

        if query.is_empty() {
            matches.sort_by(|a, b| {
                a.category
                    .cmp(&b.category)
                    .then_with(|| a.name.cmp(&b.name))
            });
        } else {
            matches.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| b.available.cmp(&a.available))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        matches.truncate(limit);
        matches
    }

    fn to_match(&self, command: &Command, ctx: &CommandContext, score: i32) -> CommandMatch {
        let availability = self.availability(command, ctx);
        CommandMatch {
            id: command.id.clone(),
            name: command.name.clone(),
            description: command.description.clone(),
            category: command.category.clone(),
            shortcut: command.shortcut.as_ref().map(ToString::to_string),
            score,
            available: availability.is_ok(),
            unavailable_reason: availability.err(),
        }
    }

    /// Runs `id`'s handler after checking availability.
    pub fn execute(&self, id: &str, ctx: &CommandContext) -> Result<Vec<AppIntent>, CommandError> {
        let command = self
            .get(id)
            .ok_or_else(|| CommandError::NotFound(CommandId::new(id.to_string())))?;
        if let Err(reason) = self.availability(command, ctx) {
            return Err(CommandError::Unavailable {
                id: command.id.clone(),
                reason,
            });
        }
        Ok((command.handler)(ctx))
    }
}

/// Turns a `CamelCase` capability variant into a lowercase, space-separated
/// label suitable for an "requires X support" message.
fn capability_label(cap: Capability) -> String {
    let name = format!("{cap:?}");
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push(' ');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CommandContext {
        CommandContext::default()
    }

    #[test]
    fn keybind_parse_and_display_roundtrip() {
        let cases = [
            ("Ctrl+Shift+M", "Ctrl+Shift+M"),
            ("Cmd+K", "Ctrl+K"),
            ("Alt+Up", "Alt+Up"),
            ("Ctrl+,", "Ctrl+,"),
            ("ctrl+shift+m", "Ctrl+Shift+M"),
        ];
        for (input, canonical) in cases {
            let kb: Keybind = input.parse().unwrap();
            assert_eq!(kb.to_string(), canonical, "input {input:?}");
        }
    }

    #[test]
    fn keybind_parse_rejects_invalid_input() {
        assert!("".parse::<Keybind>().is_err());
        assert!("K".parse::<Keybind>().is_err()); // no modifier
        assert!("Foo+K".parse::<Keybind>().is_err()); // unknown modifier
        assert!("Ctrl+".parse::<Keybind>().is_err()); // missing key
        assert!("Ctrl+Blorp".parse::<Keybind>().is_err()); // unknown named key
    }

    fn dummy_command(id: &'static str) -> Command {
        Command::new(id, "Test", |_| vec![])
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let mut reg = CommandRegistry::new();
        reg.register(dummy_command("a")).unwrap();
        let err = reg.register(dummy_command("a")).unwrap_err();
        assert!(matches!(err, CommandError::DuplicateId(id) if id.as_str() == "a"));
    }

    #[test]
    fn duplicate_shortcut_is_rejected() {
        let mut reg = CommandRegistry::new();
        reg.register(dummy_command("a").shortcut("Ctrl+K")).unwrap();
        let err = reg
            .register(dummy_command("b").shortcut("Ctrl+K"))
            .unwrap_err();
        assert!(matches!(err, CommandError::DuplicateShortcut { .. }));
    }

    #[test]
    fn search_prefix_beats_subsequence() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("nav.tasks", "Tasks", |_| vec![]))
            .unwrap();
        // "tasks" is a subsequence of "automatic disk sync" (t-a-s-k-s) but
        // is neither a prefix of the whole name nor of any of its words.
        reg.register(Command::new("misc.sync", "Automatic Disk Sync", |_| vec![]))
            .unwrap();
        let results = reg.search("tasks", &ctx(), 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.as_str(), "nav.tasks");
        assert_eq!(results[1].id.as_str(), "misc.sync");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn search_empty_query_lists_all_sorted_by_category_then_name() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("b", "Beta", |_| vec![]).category("Z"))
            .unwrap();
        reg.register(Command::new("a", "Alpha", |_| vec![]).category("A"))
            .unwrap();
        let results = reg.search("", &ctx(), 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id.as_str(), "a");
        assert_eq!(results[1].id.as_str(), "b");
    }

    #[test]
    fn search_ranks_available_above_unavailable_at_equal_score() {
        let mut reg = CommandRegistry::new();
        reg.register(
            Command::new("voice.a", "Voice A", |_| vec![])
                .requires(Requirement::Capability(Capability::Voice)),
        )
        .unwrap();
        reg.register(Command::new("voice.b", "Voice B", |_| vec![]))
            .unwrap();
        let results = reg.search("voice", &ctx(), 10);
        assert_eq!(results.len(), 2);
        assert!(results[0].available);
        assert_eq!(results[0].id.as_str(), "voice.b");
        assert!(!results[1].available);
    }

    #[test]
    fn execute_unavailable_returns_reason() {
        let mut reg = CommandRegistry::new();
        reg.register(
            Command::new("voice.mute", "Toggle mute", |_| {
                vec![AppIntent::ShowNotice {
                    message: "muted".into(),
                }]
            })
            .requires(Requirement::Capability(Capability::Voice)),
        )
        .unwrap();
        let err = reg.execute("voice.mute", &ctx()).unwrap_err();
        match err {
            CommandError::Unavailable { id, reason } => {
                assert_eq!(id.as_str(), "voice.mute");
                assert_eq!(reason, "requires voice support");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn execute_returns_handler_intents_when_available() {
        let mut reg = CommandRegistry::new();
        reg.register(Command::new("notice", "Notice", |_| {
            vec![AppIntent::ShowNotice {
                message: "hi".into(),
            }]
        }))
        .unwrap();
        let intents = reg.execute("notice", &ctx()).unwrap();
        assert_eq!(
            intents,
            vec![AppIntent::ShowNotice {
                message: "hi".into()
            }]
        );
    }

    #[test]
    fn execute_missing_command_is_not_found() {
        let reg = CommandRegistry::new();
        assert!(matches!(
            reg.execute("nope", &ctx()),
            Err(CommandError::NotFound(_))
        ));
    }
}
