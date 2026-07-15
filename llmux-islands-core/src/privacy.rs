use std::collections::BTreeMap;

use serde_json::Value;

const MAX_DISPLAY_TEXT: usize = 512;

/// Apply the user-selected account anonymity policy to a display label.
/// The original account id remains separate for action routing.
pub fn display_account(account: &str, anonymous: bool) -> String {
    if !anonymous {
        return sanitize_text(account);
    }
    let Some((local, domain)) = account.split_once('@') else {
        return "anonymous".to_string();
    };
    let first = local.chars().next().unwrap_or('*');
    format!("{first}***@{}", sanitize_text(domain))
}

/// Receipt targets are always privacy-safe, independent of the account-table
/// display preference, because receipts may persist longer than a view frame.
pub fn display_receipt_target(target: &str) -> String {
    if target.contains('@') {
        display_account(target, true)
    } else {
        sanitize_text(target)
    }
}

/// Paths are observability metadata, not an authorization channel. Query and
/// fragment data are dropped because OAuth states and API keys can appear there.
pub fn sanitize_path(path: &str) -> String {
    path.split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Render an endpoint without user-info, query parameters, or fragments.
pub fn sanitize_endpoint(endpoint: &str) -> String {
    let without_tail = endpoint.split(['?', '#']).next().unwrap_or_default().trim();
    let Some((scheme, remainder)) = without_tail.split_once("://") else {
        return sanitize_text(without_tail);
    };
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let (authority, path) = remainder.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

/// Sanitizes user-visible errors, notes, and external text. If a credential
/// marker or a token-shaped value is present, the whole message is redacted;
/// retaining partial provider error text is not worth credential exposure.
pub fn sanitize_text(input: &str) -> String {
    let normalized: String = input
        .chars()
        .filter(|ch| !ch.is_control() || *ch == ' ')
        .take(MAX_DISPLAY_TEXT)
        .collect();
    let lower = normalized.to_ascii_lowercase();
    let marked = [
        "authorization",
        "bearer ",
        "api_key",
        "api-key",
        "x-api-key",
        "access_token",
        "refresh_token",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let token_shaped = normalized.split_whitespace().any(looks_like_secret);
    if marked || token_shaped {
        "[REDACTED]".to_string()
    } else {
        normalized
    }
}

/// Sanitizes free text and, while account anonymity is enabled, replaces
/// daemon account identifiers with their opaque UI handles. Email addresses
/// that are not present in the current dashboard are masked as a final guard
/// for login and stale-activity messages.
pub(crate) fn sanitize_account_text(
    input: &str,
    anonymous: bool,
    account_handles: &BTreeMap<String, String>,
) -> String {
    let mut sanitized = sanitize_text(input);
    if !anonymous || sanitized == "[REDACTED]" {
        return sanitized;
    }

    let mut accounts: Vec<_> = account_handles.iter().collect();
    accounts.sort_by_key(|(raw, _)| std::cmp::Reverse(raw.len()));
    for (raw, handle) in accounts {
        if raw.is_empty() {
            continue;
        }
        let replacement = if raw == handle {
            display_account(raw, true)
        } else {
            sanitize_text(handle)
        };
        sanitized = sanitized.replace(raw, &replacement);
    }

    mask_email_addresses(&sanitized)
}

/// Sanitizes daemon activity notes without treating every word as a possible
/// account id. Historical aliases handle arbitrary text for accounts observed
/// by this core instance; the small grammar below covers persisted daemon
/// notes whose account disappeared before this instance saw a dashboard.
pub(crate) fn sanitize_activity_note(
    input: &str,
    anonymous: bool,
    account_handles: &BTreeMap<String, String>,
) -> String {
    let sanitized = sanitize_account_text(input, anonymous, account_handles);
    if !anonymous || sanitized == "[REDACTED]" {
        return sanitized;
    }

    if let Some(body) = sanitized.strip_prefix("switch ") {
        if let Some((from, destination)) = body.split_once(" → ") {
            let reason_at = destination
                .rfind(" (")
                .filter(|_| destination.ends_with(')'));
            let (to, reason) =
                reason_at.map_or((destination, ""), |index| destination.split_at(index));
            return format!(
                "switch {} → {}{reason}",
                private_note_account(from, account_handles),
                private_note_account(to, account_handles)
            );
        }
    }

    if let Some(body) = sanitized.strip_prefix("token refreshed: ") {
        let expiry_at = body.rfind(" (expires ").filter(|_| body.ends_with(')'));
        let (account, expiry) = expiry_at.map_or((body, ""), |index| body.split_at(index));
        return format!(
            "token refreshed: {}{expiry}",
            private_note_account(account, account_handles)
        );
    }

    for (prefix, suffix) in [
        ("refresh: ", ": refresh token dead;"),
        ("", ": refresh token dead;"),
    ] {
        if let Some(masked) = mask_note_account_between(&sanitized, prefix, suffix, account_handles)
        {
            return masked;
        }
    }

    for prefix in ["upstream: transient error on ", "transient error on "] {
        if let Some(body) = sanitized.strip_prefix(prefix) {
            let account = body.split_once(": ").map_or(body, |(account, _)| account);
            if !account.is_empty() {
                return format!("{prefix}{}", private_note_account(account, account_handles));
            }
        }
    }

    mask_status_from_note(&sanitized, account_handles).unwrap_or(sanitized)
}

pub(crate) fn sanitize_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = sanitize_text(text),
        Value::Array(items) => {
            for item in items {
                sanitize_json_strings(item);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                sanitize_json_strings(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn looks_like_secret(word: &str) -> bool {
    let trimmed =
        word.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_');
    (trimmed.starts_with("sk-") || trimmed.starts_with("ta-") || trimmed.starts_with("eyJ"))
        && trimmed.len() >= 12
}

fn private_note_account(account: &str, account_handles: &BTreeMap<String, String>) -> String {
    if account == "(none)" {
        return account.to_string();
    }
    if account_handles.values().any(|handle| handle == account) {
        sanitize_text(account)
    } else {
        account_handles
            .get(account)
            .cloned()
            .unwrap_or_else(|| display_account(account, true))
    }
}

fn mask_note_account_between(
    input: &str,
    prefix: &str,
    suffix: &str,
    account_handles: &BTreeMap<String, String>,
) -> Option<String> {
    let body = input.strip_prefix(prefix)?;
    let (account, remainder) = body.split_once(suffix)?;
    if account.is_empty() {
        return None;
    }
    Some(format!(
        "{prefix}{}{suffix}{remainder}",
        private_note_account(account, account_handles)
    ))
}

fn mask_status_from_note(
    input: &str,
    account_handles: &BTreeMap<String, String>,
) -> Option<String> {
    let (prefix, body) = input
        .strip_prefix("upstream: ")
        .map_or(("", input), |body| ("upstream: ", body));
    let (status, account_and_detail) = body.split_once(" from ")?;
    if status.parse::<u16>().is_err() {
        return None;
    }
    let (account, detail) = account_and_detail.split_once(": ")?;
    let retry_suffix = detail
        .rsplit_once(" · retry-after ")
        .and_then(|(_, retry)| retry.strip_suffix('s'))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map_or_else(String::new, |seconds| format!(" · retry-after {seconds}s"));
    Some(format!(
        "{prefix}{status} from {}{retry_suffix}",
        private_note_account(account, account_handles)
    ))
}

fn mask_email_addresses(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_at) = input[cursor..].find('@') {
        let at = cursor.saturating_add(relative_at);
        let mut start = at;
        while start > cursor && is_email_local_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at.saturating_add(1);
        while end < bytes.len() && is_email_domain_byte(bytes[end]) {
            end += 1;
        }

        if start == at || end == at.saturating_add(1) {
            output.push_str(&input[cursor..at.saturating_add(1)]);
            cursor = at.saturating_add(1);
            continue;
        }

        output.push_str(&input[cursor..start]);
        output.push(char::from(bytes[start]));
        output.push_str("***@");
        output.push_str(&input[at.saturating_add(1)..end]);
        cursor = end;
    }

    output.push_str(&input[cursor..]);
    output
}

fn is_email_local_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'.' | b'!'
                | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'/'
                | b'='
                | b'?'
                | b'^'
                | b'_'
                | b'`'
                | b'{'
                | b'|'
                | b'}'
                | b'~'
        )
}

fn is_email_domain_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
}
