use std::collections::BTreeMap;

use llmux::dashboard::{ActivityDoc, CompletedDoc};

use crate::contract::{ActivityReceipt, Provider, ReceiptCache, ReceiptKind, ReceiptTokens};
use crate::privacy::{
    display_account, display_receipt_target, sanitize_activity_note, sanitize_path, sanitize_text,
};

/// Project the daemon's bounded activity sample into privacy-safe semantic
/// receipts. In-flight order and completed order are preserved exactly.
pub fn from_activity(activity: &ActivityDoc, now_ms: u64) -> Vec<ActivityReceipt> {
    from_activity_with_account_privacy(activity, now_ms, true, &BTreeMap::new())
}

pub(crate) fn from_activity_with_account_privacy(
    activity: &ActivityDoc,
    now_ms: u64,
    anonymous: bool,
    account_handles: &BTreeMap<String, String>,
) -> Vec<ActivityReceipt> {
    let mut receipts = Vec::with_capacity(activity.in_flight.len() + activity.completed.len());
    let mut request_id_occurrences = BTreeMap::<String, usize>::new();
    let account_handles = activity_account_handles(activity, anonymous, account_handles);

    for row in &activity.in_flight {
        receipts.push(ActivityReceipt {
            receipt_id: format!("in_flight:{}", row.id),
            kind: ReceiptKind::InFlight,
            occurred_at_ms: row.started_at_ms,
            status: None,
            method: Some(sanitize_text(&row.method)),
            path: Some(sanitize_path(&row.path)),
            account_display: row
                .account
                .as_deref()
                .map(|account| receipt_account_display(account, anonymous, &account_handles)),
            provider: row.group.as_deref().map(Provider::from_group),
            model: row.model.as_deref().map(sanitize_text),
            effort: row.effort.as_deref().map(sanitize_text),
            fast: row.fast,
            tokens: None,
            cache: None,
            cost_usd: None,
            duration_ms: None,
            elapsed_ms: Some(now_ms.saturating_sub(row.started_at_ms)),
            message: None,
            error: false,
        });
    }

    for (index, row) in activity.completed.iter().enumerate() {
        match row {
            CompletedDoc::Request {
                at_ms,
                method,
                path,
                account,
                status,
                duration_ms,
                tokens,
                cost_usd,
                group,
                model,
                effort,
                fast,
            } => {
                let safe_path = sanitize_path(path);
                let base_receipt_id = format!(
                    "request:{at_ms}:{}:{safe_path}:{status}",
                    sanitize_text(method)
                );
                let occurrence = request_id_occurrences
                    .entry(base_receipt_id.clone())
                    .or_default();
                let receipt_id = if *occurrence == 0 {
                    base_receipt_id
                } else {
                    format!("{base_receipt_id}:{}", *occurrence)
                };
                *occurrence = occurrence.saturating_add(1);
                let cache = tokens.as_ref().and_then(|counts| {
                    (counts.cache_read.is_some() || counts.cache_creation.is_some()).then_some(
                        ReceiptCache {
                            read: counts.cache_read,
                            creation: counts.cache_creation,
                        },
                    )
                });
                receipts.push(ActivityReceipt {
                    receipt_id,
                    kind: ReceiptKind::Request,
                    occurred_at_ms: *at_ms,
                    status: Some(*status),
                    method: Some(sanitize_text(method)),
                    path: Some(safe_path),
                    account_display: account.as_deref().map(|account| {
                        receipt_account_display(account, anonymous, &account_handles)
                    }),
                    provider: group.as_deref().map(Provider::from_group),
                    model: model.as_deref().map(sanitize_text),
                    effort: effort.as_deref().map(sanitize_text),
                    fast: *fast,
                    tokens: tokens.as_ref().map(|counts| ReceiptTokens {
                        input: counts.input,
                        output: counts.output,
                    }),
                    cache,
                    cost_usd: cost_usd.is_finite().then_some(cost_usd.max(0.0)),
                    duration_ms: Some(*duration_ms),
                    elapsed_ms: None,
                    message: None,
                    error: *status >= 400,
                });
            }
            CompletedDoc::Note { at_ms, text, error } => receipts.push(ActivityReceipt {
                receipt_id: format!("note:{at_ms}:{index}"),
                kind: ReceiptKind::Note,
                occurred_at_ms: *at_ms,
                status: None,
                method: None,
                path: None,
                account_display: None,
                provider: None,
                model: None,
                effort: None,
                fast: false,
                tokens: None,
                cache: None,
                cost_usd: None,
                duration_ms: None,
                elapsed_ms: None,
                message: Some(sanitize_activity_note(text, anonymous, &account_handles)),
                error: *error,
            }),
        }
    }

    receipts
}

fn activity_account_handles(
    activity: &ActivityDoc,
    anonymous: bool,
    known_handles: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut handles = known_handles.clone();
    if !anonymous {
        return handles;
    }

    for account in activity
        .in_flight
        .iter()
        .filter_map(|row| row.account.as_deref())
        .chain(activity.completed.iter().filter_map(|row| match row {
            CompletedDoc::Request { account, .. } => account.as_deref(),
            CompletedDoc::Note { .. } => None,
        }))
    {
        handles
            .entry(account.to_string())
            .or_insert_with(|| display_account(account, true));
    }
    handles
}

fn receipt_account_display(
    account: &str,
    anonymous: bool,
    account_handles: &BTreeMap<String, String>,
) -> String {
    if anonymous {
        account_handles.get(account).map_or_else(
            || display_account(account, true),
            |handle| sanitize_text(handle),
        )
    } else {
        display_receipt_target(account)
    }
}
