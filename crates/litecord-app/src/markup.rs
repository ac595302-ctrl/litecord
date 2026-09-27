//! Display-only rendering of Discord message markup.
//!
//! Message content is stored exactly as Discord sent it, and editing or
//! replying keeps using that raw text. This module only produces what a
//! reader should see: `<@123>` becomes `@Name`, `<:wave:123>` becomes
//! `:wave:`, and so on. Unknown markup is left untouched.

use litecord_types::ids::UserId;

/// Replaces Discord's inline markup with readable text. `user_name` resolves
/// a mentioned user; `None` shows "@unknown user" rather than the raw ID.
pub fn display(content: &str, user_name: &mut dyn FnMut(UserId) -> Option<String>) -> String {
    if !content.contains('<') {
        return content.to_owned();
    }
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail.find('>').and_then(|end| {
            // Markup never spans lines or gets long.
            let inner = &tail[1..end];
            (end <= 100 && !inner.contains(['\n', '<']))
                .then(|| token(inner, user_name).map(|t| (t, end)))
                .flatten()
        }) {
            Some((text, end)) => {
                out.push_str(&text);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The users a message mentions, for resolving their names up front.
pub fn mentioned_users(content: &str) -> Vec<UserId> {
    let mut ids = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("<@") {
        let tail = &rest[start + 2..];
        let tail = tail.strip_prefix('!').unwrap_or(tail);
        if let Some(end) = tail.find('>') {
            if let Ok(id) = tail[..end].parse::<u64>() {
                let id = UserId(id);
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        rest = &rest[start + 2..];
    }
    ids
}

fn token(inner: &str, user_name: &mut dyn FnMut(UserId) -> Option<String>) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // <@123>, <@!123>: user mention.
    if let Some(id) = inner.strip_prefix('@') {
        if let Some(role) = id.strip_prefix('&') {
            return digits(role).then(|| "@role".to_owned());
        }
        let id = id.strip_prefix('!').unwrap_or(id);
        if !digits(id) {
            return None;
        }
        let name = id.parse().ok().map(UserId).and_then(&mut *user_name);
        return Some(format!("@{}", name.as_deref().unwrap_or("unknown user")));
    }
    // <#123>: channel mention.
    if let Some(id) = inner.strip_prefix('#') {
        return digits(id).then(|| "#channel".to_owned());
    }
    // <:name:123>, <a:name:123>: custom emoji.
    let emoji = inner.strip_prefix("a:").or_else(|| inner.strip_prefix(':'));
    if let Some(body) = emoji {
        let (name, id) = body.rsplit_once(':')?;
        let valid = !name.is_empty()
            && name.len() <= 32
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        return (valid && digits(id)).then(|| format!(":{name}:"));
    }
    // <t:1700000000> or <t:1700000000:R>: timestamp.
    if let Some(body) = inner.strip_prefix("t:") {
        let secs = body.split(':').next()?;
        let secs: i64 = secs.parse().ok()?;
        let dt = chrono::DateTime::from_timestamp(secs, 0)?;
        return Some(dt.format("%b %-d, %Y %H:%M UTC").to_string());
    }
    // </command:123>: slash command mention.
    if let Some(body) = inner.strip_prefix('/') {
        let (name, id) = body.rsplit_once(':')?;
        return (digits(id) && !name.is_empty()).then(|| format!("/{name}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(id: UserId) -> Option<String> {
        (id == UserId(42)).then(|| "Ada".to_owned())
    }

    #[test]
    fn mentions_and_custom_emoji_become_readable() {
        let d = |s: &str| display(s, &mut names);
        assert_eq!(d("hi <@42> and <@!42>"), "hi @Ada and @Ada");
        assert_eq!(d("who is <@7>?"), "who is @unknown user?");
        assert_eq!(
            d("nice <:pepe_hi:1234> <a:dance:99>"),
            "nice :pepe_hi: :dance:"
        );
        assert_eq!(d("in <#55> ping <@&66>"), "in #channel ping @role");
        assert_eq!(d("run </deploy:12>"), "run /deploy");
        assert_eq!(d("<t:0:R>"), "Jan 1, 1970 00:00 UTC");
    }

    #[test]
    fn plain_text_and_non_markup_angle_brackets_are_untouched() {
        let d = |s: &str| display(s, &mut names);
        assert_eq!(d("a < b > c"), "a < b > c");
        assert_eq!(d("<https://example.com>"), "<https://example.com>");
        assert_eq!(d("<:bad name:1>"), "<:bad name:1>");
        assert_eq!(d("<@abc>"), "<@abc>");
        assert_eq!(d("unclosed <@42"), "unclosed <@42");
        assert_eq!(d("❤️ 👍🏽 plain"), "❤️ 👍🏽 plain");
        assert_eq!(d("<<@42>>"), "<@Ada>");
    }

    #[test]
    fn mentioned_users_are_collected_once() {
        assert_eq!(
            mentioned_users("<@1> <@!2> <@1> <@&3> <#4>"),
            vec![UserId(1), UserId(2)]
        );
    }
}
