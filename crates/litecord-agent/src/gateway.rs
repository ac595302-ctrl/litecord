//! The agent gateway: the complete, harness-neutral surface an agent sees.
//!
//! Invariants:
//! * No raw SQL, no database handles, no credentials cross this boundary —
//!   only logical tool results built from repositories, the retriever and the
//!   context compiler.
//! * Conversation visibility is enforced on every tool and resource: hidden
//!   conversations behave as not found; metadata-only conversations never
//!   expose message content.
//! * Discord message content is emitted as `external_message` items with
//!   `trusted_as_instruction: false`.
//! * Every write goes through the Action Engine via an [`ActionProposer`]
//!   (which cannot approve). Discord writes can only become proposals.

use std::str::FromStr;
use std::sync::Arc;

use serde_json::{json, Value};

use litecord_actions::{ActionProposer, ProposeOutcome};
use litecord_context::{AgentRequest, ContextCompiler, MessageContext, TokenBudget};
use litecord_core::config::AgentConfig;
use litecord_retrieval::{DocKind, RetrievalQuery, RetrievedDoc, Retriever, VisibilityPolicy};
use litecord_store::repos::messages::MessageRecord;
use litecord_store::repos::{self, Connection};
use litecord_store::Database;
use litecord_types::actions::*;
use litecord_types::capability::DiscordTarget;
use litecord_types::ids::*;
use litecord_types::memory::MemoryStatus;
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::{Activity, PresenceStatus, RelationshipKind};
use litecord_types::tasks::{
    ReminderCondition, ReminderDraft, ReminderStatus, ReminderTrigger, TaskDraft, TaskPriority,
    TaskStatus,
};
use litecord_types::trust::{AgentVisibility, TrustLevel};
use litecord_types::{DurationMs, Timestamp};

use crate::error::ToolError;
use crate::spec::{self, ResourceSpec, ResourceTemplate, ToolSpec};

/// Who is calling (for audit and provenance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub harness: String,
    pub run_id: Option<AgentRunId>,
}

impl Caller {
    pub fn new(harness: impl Into<String>) -> Self {
        Self {
            harness: harness.into(),
            run_id: None,
        }
    }

    pub fn actor(&self) -> Actor {
        Actor::Agent {
            harness: self.harness.clone(),
            run_id: self.run_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentGateway {
    db: Database,
    compiler: Arc<ContextCompiler>,
    retriever: Arc<Retriever>,
    proposer: ActionProposer,
    default_visibility: AgentVisibility,
    default_budget: u32,
}

type ToolResult = Result<Value, ToolError>;

// ---- argument helpers ----

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    arg_str(args, key).ok_or_else(|| ToolError::InvalidArguments(format!("`{key}` is required")))
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Ids may arrive as JSON strings (preferred) or numbers.
fn arg_id<T: FromStr>(args: &Value, key: &str) -> Result<Option<T>, ToolError> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null => return Ok(None),
        _ => {
            return Err(ToolError::InvalidArguments(format!(
                "`{key}` must be an id"
            )))
        }
    };
    s.parse()
        .map(Some)
        .map_err(|_| ToolError::InvalidArguments(format!("`{key}` is not a valid id")))
}

fn req_id<T: FromStr>(args: &Value, key: &str) -> Result<T, ToolError> {
    arg_id(args, key)?.ok_or_else(|| ToolError::InvalidArguments(format!("`{key}` is required")))
}

fn limit(args: &Value, default: u32, max: u32) -> u32 {
    arg_i64(args, "limit")
        .map(|l| l.clamp(1, i64::from(max)) as u32)
        .unwrap_or(default)
}

fn to_value<T: serde::Serialize>(v: &T) -> ToolResult {
    serde_json::to_value(v)
        .map_err(|e| ToolError::Core(litecord_core::Error::internal(e.to_string())))
}

impl AgentGateway {
    pub fn new(
        db: Database,
        compiler: Arc<ContextCompiler>,
        retriever: Arc<Retriever>,
        proposer: ActionProposer,
        cfg: &AgentConfig,
    ) -> Self {
        Self {
            db,
            compiler,
            retriever,
            proposer,
            default_visibility: cfg.default_visibility,
            default_budget: cfg.default_token_budget,
        }
    }

    pub fn tools(&self) -> Vec<ToolSpec> {
        spec::tools()
    }

    pub fn resources(&self) -> Vec<ResourceSpec> {
        spec::resources()
    }

    pub fn resource_templates(&self) -> Vec<ResourceTemplate> {
        spec::resource_templates()
    }

    /// Dispatch a tool call.
    pub async fn call_tool(&self, name: &str, args: Value, caller: &Caller) -> ToolResult {
        let span = tracing::info_span!("agent_tool", tool = name, harness = %caller.harness);
        let _enter = span.enter();
        let args = if args.is_null() { json!({}) } else { args };
        let sync_result = match name {
            "compile_context" => Some(self.compile_context(&args, caller)),
            "search_messages" => Some(self.search_messages(&args)),
            "search_memory" => Some(self.search_memory(&args)),
            "get_recent_activity" => Some(self.recent_activity(&args)),
            "get_user" => Some(self.get_user(&args)),
            "list_relationships" => Some(self.list_relationships(&args)),
            "list_conversations" => Some(self.list_conversations(&args)),
            "get_conversation" => Some(self.get_conversation(&args)),
            "list_guilds" => Some(self.list_guilds()),
            "get_guild" => Some(self.get_guild(&args)),
            "get_channel" => Some(self.get_channel(&args)),
            "list_tasks" => Some(self.list_tasks(&args)),
            "list_reminders" => Some(self.list_reminders()),
            "list_pending_actions" => Some(self.list_pending_actions()),
            "open_in_discord" => Some(self.open_in_discord(&args)),
            _ => None,
        };
        drop(_enter);
        if let Some(r) = sync_result {
            return r;
        }
        let identity = match arg_str(&args, "send_as") {
            None | Some("user") => DiscordIdentity::UserSocialSdk,
            Some("bot") if name == "propose_message" => DiscordIdentity::ApplicationBot,
            Some(other) => {
                return Err(ToolError::InvalidArguments(format!(
                    "`send_as` must be \"user\" or \"bot\" (got {other:?})"
                )))
            }
        };
        let (action, rationale, based_on) = match name {
            "create_reminder" => (self.reminder_action(&args)?, None, None),
            "create_task" => (task_action(&args)?, None, None),
            "complete_task" => (
                AgentAction::CompleteTask {
                    task_id: TaskId(arg_i64(&args, "task_id").ok_or_else(|| {
                        ToolError::InvalidArguments("`task_id` is required".into())
                    })?),
                },
                None,
                None,
            ),
            "add_note" => (
                AgentAction::AddNote {
                    user_id: req_id(&args, "user_id")?,
                    note: req_str(&args, "note")?.to_owned(),
                },
                None,
                None,
            ),
            "bookmark_message" => (
                AgentAction::BookmarkMessage {
                    message_id: req_id(&args, "message_id")?,
                    note: arg_str(&args, "note").map(str::to_owned),
                },
                None,
                None,
            ),
            "draft_message" => (
                AgentAction::DraftMessage {
                    conversation_id: req_id(&args, "conversation_id")?,
                    content: req_str(&args, "content")?.to_owned(),
                },
                None,
                None,
            ),
            "propose_message" => {
                let content = req_str(&args, "content")?.to_owned();
                let target = match (
                    arg_id::<ConversationId>(&args, "conversation_id")?,
                    arg_id::<UserId>(&args, "user_id")?,
                ) {
                    (Some(conversation_id), _) => MessageTarget::Conversation { conversation_id },
                    (None, Some(user_id)) => MessageTarget::User { user_id },
                    (None, None) => {
                        return Err(ToolError::InvalidArguments(
                            "`conversation_id` or `user_id` is required".into(),
                        ))
                    }
                };
                (
                    AgentAction::SendMessage {
                        target,
                        content,
                        reply_to: None,
                    },
                    arg_str(&args, "rationale").map(str::to_owned),
                    arg_i64(&args, "based_on_revision"),
                )
            }
            "propose_presence_change" => {
                let status = PresenceStatus::parse(req_str(&args, "status")?)
                    .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
                let activity = arg_str(&args, "activity").map(|name| Activity {
                    name: name.to_owned(),
                    details: None,
                    state: None,
                });
                (
                    AgentAction::ChangePresence {
                        presence: PresenceDraft { status, activity },
                    },
                    arg_str(&args, "rationale").map(str::to_owned),
                    None,
                )
            }
            "propose_relationship_change" => {
                let action = RelationshipAction::parse(req_str(&args, "action")?)
                    .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
                (
                    AgentAction::RelationshipChange {
                        user_id: req_id(&args, "user_id")?,
                        action,
                    },
                    arg_str(&args, "rationale").map(str::to_owned),
                    None,
                )
            }
            other => return Err(ToolError::UnknownTool(other.to_owned())),
        };
        let based_on = match based_on {
            Some(r) if r >= 0 => litecord_types::Revision(r as u64),
            _ => self.db.current_revision()?,
        };
        let outcome = self
            .proposer
            .propose_as(action, caller.actor(), identity, based_on, rationale)
            .await?;
        let note = match &outcome {
            ProposeOutcome::Executed { .. } => "done (recorded in the action audit log)",
            ProposeOutcome::PendingApproval { .. } => {
                "proposed; nothing happens until the user approves it in Litecord"
            }
        };
        Ok(json!({ "result": to_value(&outcome)?, "note": note }))
    }

    /// Read a resource by URI.
    pub fn read_resource(&self, uri: &str) -> ToolResult {
        match uri {
            "discord://me" => self.db.read(|r| -> ToolResult {
                let me = current_user(r)?;
                match me {
                    Some(id) => user_json(r, id),
                    None => Err(ToolError::NotFound("current user".into())),
                }
            }),
            "discord://relationships" => self.list_relationships(&json!({})),
            "discord://conversations" => self.list_conversations(&json!({})),
            "discord://guilds" => self.list_guilds(),
            "discord://tasks" => self.list_tasks(&json!({})),
            "discord://reminders" => self.list_reminders(),
            "discord://actions/pending" => self.list_pending_actions(),
            "discord://memory/recent" => self.db.read(|r| -> ToolResult {
                let items = repos::memory::list(
                    r,
                    &repos::memory::MemoryFilter {
                        limit: 20,
                        ..Default::default()
                    },
                )?;
                let vis = VisibilityPolicy::load(r, self.default_visibility)
                    .map_err(litecord_core::Error::from)?;
                let items: Vec<Value> = items
                    .iter()
                    .filter(|m| memory_visible(&vis, &m.entities))
                    .map(memory_json)
                    .collect();
                Ok(json!({ "memories": items }))
            }),
            other => {
                if let Some(id) = other.strip_prefix("discord://conversations/") {
                    self.get_conversation(&json!({ "conversation_id": id }))
                } else if let Some(id) = other.strip_prefix("discord://guilds/") {
                    self.get_guild(&json!({ "guild_id": id }))
                } else {
                    Err(ToolError::NotFound(format!("resource {other}")))
                }
            }
        }
    }

    // ---- read tools ----

    fn compile_context(&self, args: &Value, caller: &Caller) -> ToolResult {
        let mut req = AgentRequest::new(req_str(args, "instruction")?);
        req.focus_conversation = arg_id(args, "conversation_id")?;
        req.include_history = arg_bool(args, "include_history");
        let budget = TokenBudget::new(
            arg_i64(args, "max_tokens")
                .map(|t| t.clamp(200, 32_000) as u32)
                .unwrap_or(self.default_budget),
        );
        let revision = self.db.current_revision()?;
        let run = self.db.write(|tx| {
            repos::agent_runs::start(tx, &caller.harness, &req.instruction, revision)
        })?;
        let result = self.compiler.compile(&req, budget);
        let (ok, items, tokens, err) = match &result {
            Ok(p) => (
                true,
                (p.messages.len() + p.memories.len() + p.tasks.len() + p.conversations.len())
                    as u32,
                p.stats.used_tokens,
                None,
            ),
            Err(e) => (false, 0, 0, Some(e.kind().to_string())),
        };
        self.db.write(|tx| {
            repos::agent_runs::finish(tx, run.value, ok, items, tokens, err.as_deref())
        })?;
        to_value(&result?)
    }

    fn search_messages(&self, args: &Value) -> ToolResult {
        let mut q = RetrievalQuery::new(req_str(args, "query")?);
        q.kinds = vec![DocKind::Message];
        q.limit_per_kind = limit(args, 20, 50);
        if let Some(c) = arg_id::<ConversationId>(args, "conversation_id")? {
            q.filters.conversation_ids = Some(vec![c]);
            q.focus_conversation = Some(c);
        }
        q.filters.author_id = arg_id(args, "author_id")?;
        q.filters.since = arg_i64(args, "since_ms").map(Timestamp::from_millis);
        let now = self.db.now();
        self.db.read(|r| -> ToolResult {
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let hits = self
                .retriever
                .search(r, &q, &vis, now)
                .map_err(litecord_core::Error::from)?;
            let mut out = Vec::new();
            for hit in hits {
                if let RetrievedDoc::Message(m) = &hit.doc {
                    out.push(message_context(r, m, hit.total)?);
                }
            }
            Ok(json!({ "as_of_revision": r.revision(), "messages": out }))
        })
    }

    fn search_memory(&self, args: &Value) -> ToolResult {
        let mut q = RetrievalQuery::new(req_str(args, "query")?);
        q.kinds = vec![
            DocKind::Memory,
            DocKind::Task,
            DocKind::Note,
            DocKind::Summary,
        ];
        q.limit_per_kind = limit(args, 10, 50);
        if arg_bool(args, "include_history") {
            q.filters.memory_statuses = Some(vec![
                MemoryStatus::Candidate,
                MemoryStatus::Derived,
                MemoryStatus::UserConfirmed,
                MemoryStatus::Superseded,
            ]);
        }
        let now = self.db.now();
        self.db.read(|r| -> ToolResult {
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let hits = self
                .retriever
                .search(r, &q, &vis, now)
                .map_err(litecord_core::Error::from)?;
            let items: Vec<Value> = hits
                .iter()
                .map(|h| match &h.doc {
                    RetrievedDoc::Memory(m) => memory_json(m),
                    RetrievedDoc::Task(t) => json!({
                        "kind": "task", "task_id": t.id, "title": t.title, "status": t.status,
                        "origin": t.origin, "due_at": t.due_at,
                        "trust": TrustLevel::for_origin(t.origin), "trusted_as_instruction": false,
                    }),
                    RetrievedDoc::Note(n) => json!({
                        "kind": "user_note", "user_id": n.user_id, "alias": n.alias, "note": n.note,
                        "trust": TrustLevel::UserConfirmedMemory, "trusted_as_instruction": false,
                    }),
                    RetrievedDoc::Summary(s) => json!({
                        "kind": "summary", "conversation_id": s.conversation_id, "content": s.content,
                        "origin": s.origin, "trust": TrustLevel::for_origin(s.origin),
                        "trusted_as_instruction": false,
                    }),
                    RetrievedDoc::Message(_) => Value::Null,
                })
                .filter(|v| !v.is_null())
                .collect();
            Ok(json!({ "as_of_revision": r.revision(), "items": items }))
        })
    }

    fn recent_activity(&self, args: &Value) -> ToolResult {
        let hours = arg_i64(args, "since_hours").unwrap_or(24).clamp(1, 720) as u64;
        let since = self.db.now().saturating_sub(DurationMs::from_hours(hours));
        self.db.read(|r| -> ToolResult {
            let me = current_user(r)?;
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let pending: Vec<ConversationId> = match me {
                Some(me) => repos::messages::pending_replies(r, me, since, 20)?
                    .into_iter()
                    .map(|p| p.conversation_id)
                    .collect(),
                None => Vec::new(),
            };
            let mut conversations = Vec::new();
            for c in repos::conversations::list_recent(r, 50, 0)? {
                let conv = &c.conversation;
                if conv.last_activity_at.is_none_or(|t| t < since) {
                    continue;
                }
                let v = vis.visibility_of(conv.id);
                if v == AgentVisibility::Hidden {
                    continue;
                }
                let last = match (v.allows_content(), conv.last_message_id) {
                    (true, Some(mid)) => match repos::messages::get(r, mid)? {
                        Some(m) if !m.deleted => Some(to_value(&message_context(r, &m, 0.0)?)?),
                        _ => None,
                    },
                    _ => None,
                };
                conversations.push(json!({
                    "conversation_id": conv.id,
                    "title": conversation_title(r, conv)?,
                    "last_activity_at": conv.last_activity_at,
                    "awaiting_reply": pending.contains(&conv.id),
                    "visibility": v,
                    "last_message": last,
                }));
            }
            Ok(json!({ "as_of_revision": r.revision(), "since": since, "conversations": conversations }))
        })
    }

    fn get_user(&self, args: &Value) -> ToolResult {
        let id: UserId = req_id(args, "user_id")?;
        self.db.read(|r| user_json(r, id))
    }

    fn list_relationships(&self, args: &Value) -> ToolResult {
        let kinds = match arg_str(args, "kind") {
            Some(k) => Some(vec![RelationshipKind::parse(k)
                .map_err(|e| ToolError::InvalidArguments(e.to_string()))?]),
            None => None,
        };
        self.db.read(|r| -> ToolResult {
            let list = repos::relationships::list(r, kinds.as_deref())?;
            let items: Vec<Value> = list
                .iter()
                .map(|rec| {
                    json!({
                        "user_id": rec.relationship.user_id,
                        "name": rec.user.as_ref().map(|u| u.user.display_name().to_owned()),
                        "discord": rec.relationship.discord,
                        "game": rec.relationship.game,
                        "presence": rec.user.as_ref().map(|u| u.presence.status),
                        "origin": rec.origin,
                    })
                })
                .collect();
            Ok(json!({ "relationships": items }))
        })
    }

    fn list_conversations(&self, args: &Value) -> ToolResult {
        let n = limit(args, 30, 100);
        self.db.read(|r| -> ToolResult {
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let mut out = Vec::new();
            for c in repos::conversations::list_recent(r, n, 0)? {
                let v = vis.visibility_of(c.conversation.id);
                if v == AgentVisibility::Hidden {
                    continue;
                }
                out.push(json!({
                    "conversation_id": c.conversation.id,
                    "kind": c.conversation.kind,
                    "title": conversation_title(r, &c.conversation)?,
                    "recipient_id": c.conversation.recipient_id,
                    "last_activity_at": c.conversation.last_activity_at,
                    "visibility": v,
                }));
            }
            Ok(json!({ "conversations": out }))
        })
    }

    fn get_conversation(&self, args: &Value) -> ToolResult {
        let id: ConversationId = req_id(args, "conversation_id")?;
        let n = limit(args, 30, 100);
        self.db.read(|r| -> ToolResult {
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let v = vis.visibility_of(id);
            let rec = repos::conversations::get(r, id)?
                .filter(|_| v != AgentVisibility::Hidden)
                .ok_or_else(|| ToolError::NotFound(format!("conversation {id}")))?;
            let messages = if v.allows_content() {
                let mut recent = repos::messages::recent(r, id, n, None)?;
                recent.reverse();
                recent
                    .iter()
                    .map(|m| message_context(r, m, 0.0))
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            Ok(json!({
                "as_of_revision": r.revision(),
                "conversation_id": id,
                "kind": rec.conversation.kind,
                "title": conversation_title(r, &rec.conversation)?,
                "visibility": v,
                "content_available": v.allows_content(),
                "messages": messages,
            }))
        })
    }

    fn list_guilds(&self) -> ToolResult {
        self.db.read(|r| -> ToolResult {
            let guilds: Vec<Value> = repos::guilds::list(r, false)?
                .iter()
                .map(|g| json!({"guild_id": g.guild.id, "name": g.guild.name, "origin": g.origin}))
                .collect();
            Ok(json!({ "guilds": guilds }))
        })
    }

    fn get_guild(&self, args: &Value) -> ToolResult {
        let id: GuildId = req_id(args, "guild_id")?;
        self.db.read(|r| -> ToolResult {
            let g = repos::guilds::get(r, id)?
                .ok_or_else(|| ToolError::NotFound(format!("guild {id}")))?;
            let channels: Vec<Value> = repos::channels::list_for_guild(r, id, false)?
                .iter()
                .map(|c| channel_json(&c.channel))
                .collect();
            Ok(json!({"guild_id": id, "name": g.guild.name, "departed": g.departed, "channels": channels}))
        })
    }

    fn get_channel(&self, args: &Value) -> ToolResult {
        let id: ChannelId = req_id(args, "channel_id")?;
        self.db.read(|r| -> ToolResult {
            let c = repos::channels::get(r, id)?
                .ok_or_else(|| ToolError::NotFound(format!("channel {id}")))?;
            Ok(channel_json(&c.channel))
        })
    }

    fn list_tasks(&self, args: &Value) -> ToolResult {
        let statuses = match arg_str(args, "status") {
            Some(s) => {
                vec![TaskStatus::parse(s).map_err(|e| ToolError::InvalidArguments(e.to_string()))?]
            }
            None => vec![TaskStatus::Open, TaskStatus::Candidate],
        };
        self.db.read(|r| -> ToolResult {
            let tasks = repos::tasks::list(
                r,
                &repos::tasks::TaskFilter {
                    statuses: Some(statuses),
                    limit: 50,
                    ..Default::default()
                },
            )?;
            // Tasks extracted from a conversation carry its text; they
            // follow that conversation's agent visibility.
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let tasks: Vec<_> = tasks
                .into_iter()
                .filter(|t| t.conversation_id.is_none_or(|c| vis.allows_content(c)))
                .collect();
            Ok(json!({ "tasks": to_value(&tasks)? }))
        })
    }

    fn list_reminders(&self) -> ToolResult {
        self.db.read(|r| -> ToolResult {
            let reminders = repos::reminders::list(
                r,
                &repos::reminders::ReminderFilter {
                    statuses: Some(vec![ReminderStatus::Pending]),
                    limit: 50,
                    ..Default::default()
                },
            )?;
            let vis = VisibilityPolicy::load(r, self.default_visibility)
                .map_err(litecord_core::Error::from)?;
            let reminders: Vec<_> = reminders
                .into_iter()
                .filter(|m| m.conversation_id.is_none_or(|c| vis.allows_content(c)))
                .collect();
            Ok(json!({ "reminders": to_value(&reminders)? }))
        })
    }

    fn list_pending_actions(&self) -> ToolResult {
        let pending = self.proposer.pending(50)?;
        let items: Vec<Value> = pending
            .iter()
            .map(|p| {
                json!({
                    "action_id": p.id, "kind": p.action.kind(), "status": p.status,
                    "class": p.class, "created_at": p.created_at, "rationale": p.rationale,
                })
            })
            .collect();
        Ok(json!({ "pending_actions": items }))
    }

    fn open_in_discord(&self, args: &Value) -> ToolResult {
        let conversation_id: ConversationId = req_id(args, "conversation_id")?;
        let target = match arg_id::<MessageId>(args, "message_id")? {
            Some(message_id) => DiscordTarget::Message {
                guild_id: None,
                channel_id: ChannelId(conversation_id.get()),
                message_id,
            },
            None => DiscordTarget::Conversation { conversation_id },
        };
        Ok(json!({ "url": target.web_url(), "note": "Opening links is the user's decision." }))
    }

    // ---- write helpers ----

    fn reminder_action(&self, args: &Value) -> Result<AgentAction, ToolError> {
        let now = self.db.now();
        let due = match (arg_i64(args, "due_at_ms"), arg_i64(args, "in_minutes")) {
            (Some(ms), _) => Timestamp::from_millis(ms),
            (None, Some(min)) if min > 0 => now.saturating_add(DurationMs::from_mins(min as u64)),
            _ => {
                return Err(ToolError::InvalidArguments(
                    "`due_at_ms` or `in_minutes` is required".into(),
                ))
            }
        };
        let conversation_id: Option<ConversationId> = arg_id(args, "conversation_id")?;
        let trigger = match (
            arg_id::<UserId>(args, "unless_reply_from")?,
            conversation_id,
        ) {
            (Some(user_id), Some(conversation_id)) => ReminderTrigger::Conditional {
                condition: ReminderCondition::NoReplyFrom {
                    user_id,
                    conversation_id,
                    since: now,
                },
                check_at: due,
            },
            (Some(_), None) => {
                return Err(ToolError::InvalidArguments(
                    "`unless_reply_from` requires `conversation_id`".into(),
                ))
            }
            (None, _) => ReminderTrigger::At { at: due },
        };
        Ok(AgentAction::CreateReminder {
            reminder: ReminderDraft {
                title: req_str(args, "title")?.to_owned(),
                note: arg_str(args, "note").map(str::to_owned),
                trigger,
                conversation_id,
                task_id: None,
                source: None,
            },
        })
    }
}

fn task_action(args: &Value) -> Result<AgentAction, ToolError> {
    let related_users = match args.get("related_user_ids").and_then(Value::as_array) {
        Some(ids) => ids
            .iter()
            .map(|v| {
                v.as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| v.as_u64().map(UserId))
                    .ok_or_else(|| ToolError::InvalidArguments("invalid user id".into()))
            })
            .collect::<Result<Vec<UserId>, _>>()?,
        None => Vec::new(),
    };
    let priority = match arg_str(args, "priority") {
        Some(s) => {
            TaskPriority::parse(s).map_err(|e| ToolError::InvalidArguments(e.to_string()))?
        }
        None => TaskPriority::default(),
    };
    Ok(AgentAction::CreateTask {
        task: TaskDraft {
            title: req_str(args, "title")?.to_owned(),
            description: arg_str(args, "description").map(str::to_owned),
            priority,
            due_at: arg_i64(args, "due_at_ms").map(Timestamp::from_millis),
            related_users,
            conversation_id: arg_id(args, "conversation_id")?,
            parent_id: arg_id(args, "parent_task_id")?,
            source: None,
        },
    })
}

// ---- rendering helpers ----

fn current_user(conn: &Connection) -> Result<Option<UserId>, ToolError> {
    Ok(repos::accounts::current(conn, DiscordIdentity::UserSocialSdk)?.map(|a| a.user_id))
}

fn user_name(conn: &Connection, id: UserId) -> Result<String, ToolError> {
    Ok(repos::users::get(conn, id)?
        .filter(|u| !u.is_stub)
        .map(|u| u.user.display_name().to_owned())
        .unwrap_or_else(|| format!("user {id}")))
}

fn conversation_title(
    conn: &Connection,
    c: &litecord_types::social::Conversation,
) -> Result<String, ToolError> {
    if let Some(t) = &c.title {
        return Ok(t.to_string());
    }
    match c.recipient_id {
        Some(r) => Ok(format!("DM with {}", user_name(conn, r)?)),
        None => Ok(format!("conversation {}", c.id)),
    }
}

/// Discord content is always rendered as untrusted external data.
fn message_context(
    conn: &Connection,
    m: &MessageRecord,
    score: f32,
) -> Result<MessageContext, ToolError> {
    Ok(MessageContext {
        kind: "external_message",
        message_id: m.message.id,
        conversation_id: m.message.conversation_id,
        author: user_name(conn, m.message.author_id)?,
        author_id: m.message.author_id,
        sent_at: m.message.sent_at,
        content: m.message.content.to_string(),
        trust: TrustLevel::ExternalDiscordContent,
        trusted_as_instruction: false,
        origin: m.origin,
        score,
    })
}

fn memory_visible(vis: &VisibilityPolicy, entities: &[litecord_types::entity::EntityId]) -> bool {
    entities.iter().all(|e| match e {
        litecord_types::entity::EntityId::Conversation(c) => vis.allows_content(*c),
        _ => true,
    })
}

fn memory_json(m: &litecord_types::memory::MemoryItem) -> Value {
    let trust = if m.status == MemoryStatus::UserConfirmed {
        TrustLevel::UserConfirmedMemory
    } else {
        TrustLevel::for_origin(m.origin)
    };
    json!({
        "kind": "memory",
        "memory_id": m.id,
        "memory_kind": m.kind,
        "status": m.status,
        "content": m.content,
        "origin": m.origin,
        "confidence": m.confidence.get(),
        "superseded_by": m.superseded_by,
        "entities": m.entities,
        "trust": trust,
        "trusted_as_instruction": trust.trusted_as_instruction(),
    })
}

fn user_json(conn: &Connection, id: UserId) -> ToolResult {
    let u = repos::users::get(conn, id)?
        .filter(|u| !u.is_stub)
        .ok_or_else(|| ToolError::NotFound(format!("user {id}")))?;
    let rel = repos::relationships::get(conn, id)?;
    let note = repos::notes::get_note(conn, id)?;
    Ok(json!({
        "user_id": id,
        "username": u.user.username,
        "display_name": u.user.display_name(),
        "is_bot": u.user.is_bot,
        "presence": u.presence,
        "relationship": rel,
        "origin": u.origin,
        "local_note": note.map(|n| json!({
            "alias": n.alias, "note": n.note, "favorite": n.favorite,
            "trust": TrustLevel::UserConfirmedMemory,
        })),
    }))
}

fn channel_json(c: &litecord_types::social::Channel) -> Value {
    use litecord_types::social::ChannelCapabilities as Caps;
    json!({
        "channel_id": c.id,
        "guild_id": c.guild_id,
        "name": c.name,
        "kind": c.kind,
        "access": c.access,
        "readable": c.capabilities.contains(Caps::READABLE),
        "writable": c.capabilities.contains(Caps::WRITABLE),
        "open_external_url": DiscordTarget::Channel { guild_id: c.guild_id, channel_id: c.id }.web_url(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn create_task_defaults_to_normal_priority() {
        let action = task_action(&json!({ "title": "write report" })).unwrap();
        let AgentAction::CreateTask { task } = action else {
            panic!("expected CreateTask");
        };
        assert_eq!(task.priority, TaskPriority::Normal);
        assert_eq!(task.parent_id, None);
    }

    #[test]
    fn create_task_accepts_priority_and_parent() {
        let action = task_action(&json!({
            "title": "sub-step",
            "priority": "urgent",
            "parent_task_id": 42,
        }))
        .unwrap();
        let AgentAction::CreateTask { task } = action else {
            panic!("expected CreateTask");
        };
        assert_eq!(task.priority, TaskPriority::Urgent);
        assert_eq!(task.parent_id, Some(TaskId(42)));
    }

    #[test]
    fn create_task_rejects_invalid_priority() {
        let err = task_action(&json!({ "title": "x", "priority": "whenever" })).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
