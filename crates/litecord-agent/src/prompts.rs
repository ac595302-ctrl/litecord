//! Prompts shipped with Litecord (see `docs/AGENT_HARNESS.md`).
//!
//! Kept as Markdown under `prompts/` so they can be reviewed as text, and
//! compiled in so every harness gets the same wording. They are short on
//! purpose: tools describe themselves, and context comes from
//! `compile_context` on demand rather than being pasted up front.

/// Developer instructions for the in-app assistant (Omni), given to the
/// harness when a session starts.
pub const OMNI: &str = include_str!("../prompts/omni.md");

/// Instructions the MCP server returns from `initialize`; harness-neutral,
/// no persona, so it also fits a user's own Codex/OpenCode session.
pub const MCP_INSTRUCTIONS: &str = include_str!("../prompts/mcp_instructions.md");

const HEARTBEAT: &str = include_str!("../prompts/heartbeat.md");

/// Inputs for a scheduled check-in turn.
#[derive(Debug, Clone)]
pub struct HeartbeatPrompt<'a> {
    /// Local time of the check-in, as shown to the model.
    pub at: &'a str,
    pub from_revision: u64,
    pub to_revision: u64,
    /// Content-free summary of what changed (counts and ids).
    pub delta: &'a str,
    pub max_actions: u32,
    /// Whether the user allowed check-ins to create Discord proposals.
    pub allow_proposals: bool,
}

/// The exact reply that marks a check-in with nothing to report; the
/// runner drops it instead of posting to the inbox.
pub const HEARTBEAT_OK: &str = "HEARTBEAT_OK";

impl HeartbeatPrompt<'_> {
    pub fn render(&self) -> String {
        HEARTBEAT
            .replace("{{at}}", self.at)
            .replace("{{from}}", &self.from_revision.to_string())
            .replace("{{to}}", &self.to_revision.to_string())
            .replace("{{delta}}", self.delta.trim())
            .replace("{{max_actions}}", &self.max_actions.to_string())
            .replace(
                "{{proposals_note}}",
                if self.allow_proposals {
                    " unless one is clearly needed; the user still approves it"
                } else {
                    ""
                },
            )
    }
}

/// Whether a check-in reply means "nothing to report".
pub fn is_heartbeat_ok(reply: &str) -> bool {
    reply.trim() == HEARTBEAT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rough token estimate (≈4 chars/token), enough to keep prompts lean.
    fn tokens(s: &str) -> usize {
        s.len().div_ceil(4)
    }

    #[test]
    fn prompts_stay_small() {
        assert!(tokens(OMNI) < 900, "omni.md is {} tokens", tokens(OMNI));
        assert!(tokens(MCP_INSTRUCTIONS) < 200);
        assert!(tokens(HEARTBEAT) < 200);
    }

    #[test]
    fn omni_names_only_real_tools() {
        let names: Vec<&str> = crate::spec::tools().into_iter().map(|t| t.name).collect();
        for word in OMNI.split('`').skip(1).step_by(2) {
            let is_tool_like = word.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && word.contains('_')
                && !word.ends_with('*');
            if is_tool_like
                && !["as_of_revision", "trusted_as_instruction", "send_as"].contains(&word)
            {
                assert!(names.contains(&word), "omni.md names unknown tool `{word}`");
            }
        }
    }

    #[test]
    fn heartbeat_renders_every_placeholder() {
        let p = HeartbeatPrompt {
            at: "Sat 09:00",
            from_revision: 10,
            to_revision: 42,
            delta: "3 new DMs (2 conversations); 1 reminder due\n",
            max_actions: 3,
            allow_proposals: false,
        }
        .render();
        assert!(!p.contains("{{"), "{p}");
        assert!(p.contains("10 → 42") && p.contains("at most 3 local writes"));
        assert!(p.contains("Do not propose Discord actions from a check-in."));
        assert!(is_heartbeat_ok(" HEARTBEAT_OK\n"));
        assert!(!is_heartbeat_ok("HEARTBEAT_OK, but also…"));
    }
}
