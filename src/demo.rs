//! Demo mode (`LLMUX_DEMO_MODE`): replace real account identities (which are
//! emails) with deterministic, stable fake ones so a screen recording or
//! screenshot never leaks the operator's real accounts.
//!
//! The substitution happens once, at config load (`config::load*`), on the
//! account `name` — which is the display id used everywhere (table, detail,
//! current/next, activity, logs). Aliasing it at the source keeps every surface
//! consistent with zero per-render-site risk of a miss, while credentials
//! (looked up by token/uuid, never by name) keep working. Config writes are
//! suppressed in demo mode so the aliases never reach disk.
//!
//! "Stable" = the same real name always maps to the same fake one (FNV-1a hash
//! into a fixed pool), so the recording is internally coherent across frames.

/// Whether `LLMUX_DEMO_MODE` is set to an on-ish value (set + not empty / `0` /
/// `false`).
pub fn enabled() -> bool {
    match std::env::var_os("LLMUX_DEMO_MODE") {
        Some(v) => {
            let v = v.to_string_lossy();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        None => false,
    }
}

/// A fixed pool of obviously-fake-but-realistic emails. Replacing a real email
/// with one of these keeps the dashboard legible without exposing anything.
const POOL: [&str; 16] = [
    "ada.lovelace@example.com",
    "alan.turing@example.com",
    "grace.hopper@example.com",
    "katherine.j@example.com",
    "linus.t@example.com",
    "margaret.h@example.com",
    "dennis.r@example.com",
    "barbara.l@example.com",
    "john.mccarthy@example.com",
    "edsger.d@example.com",
    "claude.s@example.com",
    "donald.k@example.com",
    "rosalind.f@example.com",
    "tim.bl@example.com",
    "vint.cerf@example.com",
    "radia.p@example.com",
];

/// Deterministic display alias for `name` when demo mode is on; otherwise the
/// name unchanged. See [`alias_always`] for the mapping itself.
pub fn alias(name: &str) -> String {
    if enabled() {
        alias_always(name)
    } else {
        name.to_string()
    }
}

/// The pure mapping (no env read): a `provider:` tag (e.g. `codex:`) is kept and
/// only the email after it is replaced; a value with no `@` is returned as-is.
/// Public because the `email_anonymous` server setting reuses this exact
/// mapping at the TUI RENDER layer (demo mode keeps its load-time substitution
/// and therefore takes precedence — aliasing an already-aliased name still
/// lands in the fake pool).
pub fn alias_always(name: &str) -> String {
    let (prefix, email) = match name.split_once(':') {
        Some((tag, rest)) if rest.contains('@') => (format!("{tag}:"), rest),
        _ => (String::new(), name),
    };
    if !email.contains('@') {
        return name.to_string();
    }
    let idx = (fnv1a(email) % POOL.len() as u64) as usize;
    format!("{prefix}{}", POOL[idx])
}

/// Replace every email-looking token inside free-form `text` with its
/// deterministic alias — for TUI surfaces that carry emails EMBEDDED in a
/// sentence (activity notes like `switch a@x.com → b@y.com (manual)`, tracing
/// lines) where [`alias_always`]'s whole-name contract doesn't apply. A token
/// is a maximal run of email-charset bytes (`[A-Za-z0-9._%+-@]`); runs
/// containing `@` with a non-empty local part and a dot-bearing domain are
/// swapped via [`alias_always`], everything else passes through byte-for-byte.
pub fn mask_email_text(text: &str) -> String {
    fn email_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-' | b'@')
    }
    fn looks_like_email(token: &str) -> bool {
        match token.split_once('@') {
            Some((local, domain)) => {
                !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
            }
            None => false,
        }
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if email_byte(bytes[i]) {
            let start = i;
            while i < bytes.len() && email_byte(bytes[i]) {
                i += 1;
            }
            // The run is pure ASCII (email_byte admits only ASCII), so this
            // slice is always on a char boundary.
            let token = &text[start..i];
            if looks_like_email(token) {
                out.push_str(&alias_always(token));
            } else {
                out.push_str(token);
            }
        } else {
            let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

/// FNV-1a over the bytes — a small, dependency-free, deterministic hash. (Not
/// `DefaultHasher`: its output is not guaranteed stable across runs/versions,
/// and stability is the whole point here.)
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_is_stable_for_the_same_input() {
        assert_eq!(
            alias_always("info@insightquest.io"),
            alias_always("info@insightquest.io"),
            "same real name must always map to the same fake one"
        );
    }

    #[test]
    fn alias_lands_in_the_fake_pool() {
        let a = alias_always("someone@real-domain.com");
        assert!(POOL.contains(&a.as_str()), "got {a}");
        assert!(!a.contains("real-domain"), "real domain must not survive");
    }

    #[test]
    fn codex_provider_tag_is_preserved() {
        let a = alias_always("codex:chatgpt-user@gmail.com");
        assert!(a.starts_with("codex:"), "got {a}");
        let email = a.strip_prefix("codex:").unwrap();
        assert!(POOL.contains(&email), "got {a}");
    }

    #[test]
    fn non_email_names_are_left_alone() {
        assert_eq!(alias_always("my-api-key-account"), "my-api-key-account");
    }

    #[test]
    fn distinct_typical_accounts_get_distinct_aliases() {
        // The four demo accounts must read as four different people.
        let names = [
            "ai2@insightquest.io",
            "notify@insightquest.io",
            "codex:ai@insightquest.io",
            "codex:icedac@gmail.com",
        ];
        let aliased: Vec<String> = names.iter().map(|n| alias_always(n)).collect();
        let unique: std::collections::HashSet<&String> = aliased.iter().collect();
        assert_eq!(unique.len(), names.len(), "aliases collided: {aliased:?}");
    }

    #[test]
    fn mask_email_text_replaces_embedded_emails_only() {
        let masked = mask_email_text("switch a@real-x.com → b@real-y.io (manual), 3s ago");
        assert!(!masked.contains("real-x"), "got {masked}");
        assert!(!masked.contains("real-y"), "got {masked}");
        assert!(masked.starts_with("switch "), "prose kept: {masked}");
        assert!(masked.contains(" → "), "arrow kept: {masked}");
        assert!(
            masked.ends_with("(manual), 3s ago"),
            "suffix kept: {masked}"
        );
        // Both replacements come from the fake pool.
        for alias in masked
            .split_whitespace()
            .filter(|t| t.contains('@'))
            .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '@' && c != '.'))
        {
            assert!(POOL.contains(&alias), "not a pool alias: {alias}");
        }
    }

    #[test]
    fn mask_email_text_leaves_non_email_text_alone() {
        for text in [
            "config reloaded: 3 account(s)",
            "POST /v1/messages 200",
            "utf-8 안전 · no emails here",
            "half@ and @half stay", // no domain dot / no local part
        ] {
            assert_eq!(mask_email_text(text), text);
        }
    }

    #[test]
    fn enabled_parses_common_values() {
        // Pure check of the truthiness rule without mutating process env in a
        // way that races other tests: exercise the same predicate inline.
        let truthy = |v: &str| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false");
        assert!(truthy("1"));
        assert!(truthy("true"));
        assert!(!truthy("0"));
        assert!(!truthy("false"));
        assert!(!truthy(""));
    }
}
