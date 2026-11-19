//! Server `instructions` from the MCP `initialize` result, shown to the model.
//!
//! Each server's text is capped and rendered under its own heading, in a
//! section separate from the user's AGENTS.md instructions, so the model can
//! tell server guidance from user guidance.

/// Per-server cap, in characters. Matches the limit Claude Code applies, so a
/// server that fits there fits here.
pub const MAX_SERVER_INSTRUCTIONS_CHARS: usize = 4096;

const TRUNCATION_MARKER: &str = "\n[instructions truncated]";

/// Trim and cap one server's instructions. Returns `None` when nothing is left.
pub fn cap_server_instructions(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_SERVER_INSTRUCTIONS_CHARS {
        return Some(text.to_string());
    }
    let keep = MAX_SERVER_INSTRUCTIONS_CHARS - TRUNCATION_MARKER.chars().count();
    let mut capped: String = text.chars().take(keep).collect();
    capped.push_str(TRUNCATION_MARKER);
    Some(capped)
}

/// Render the system-prompt section for `(server name, instructions)` pairs.
///
/// Servers are listed by name so the section is identical however the
/// connections happened to finish. Returns `None` when no server sent any.
pub fn build_instructions_section(servers: &[(String, String)]) -> Option<String> {
    let mut servers: Vec<(&str, String)> = servers
        .iter()
        .filter_map(|(name, text)| Some((name.as_str(), cap_server_instructions(text)?)))
        .collect();
    if servers.is_empty() {
        return None;
    }
    servers.sort_by(|a, b| a.0.cmp(b.0));
    servers.dedup_by(|a, b| a.0 == b.0);
    let mut section = String::from(
        "# MCP Server Instructions\n\n\
         The following MCP servers provided instructions for how to use their \
         tools and resources. They come from the servers, not from the user.",
    );
    for (name, text) in servers {
        section.push_str("\n\n## ");
        section.push_str(name);
        section.push('\n');
        section.push_str(&text);
    }
    Some(section)
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
