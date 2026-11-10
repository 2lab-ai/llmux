//! The 5-hour window is gone (owner, 2026-11-04: "5h 사용량 삭제해줘. 이제
//! 5시간 사용량 있는 프로바이더가 없어짐"; 2026-11-10: "llmux에서도 완전히
//! 삭제해줘"). No provider reports one, so llmux neither reads, stores, gates
//! on, nor shows a 5h window. These tests pin that from the outside:
//!
//! - a 5h reading on the wire (unified headers, a codex session window, a
//!   usage-poll `five_hour`) is not quota evidence and never parks an account;
//! - the config no longer carries `five_hour_max`, yet an old config file that
//!   still has it loads.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, HeaderValue};
use llmux::config::{self, AccountConfig, AccountCredential, Config, SchedulerConfig};
use llmux::scheduler::headers;
use llmux::scheduler::select::{self, Decision, SelectParams};
use llmux::scheduler::usage;
use llmux::scheduler::{AccountId, AccountPool};

fn epoch_in(secs: u64) -> String {
    (SystemTime::now() + Duration::from_secs(secs))
        .duration_since(UNIX_EPOCH)
        .expect("future")
        .as_secs()
        .to_string()
}

fn headers_of(pairs: &[(&str, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            http::header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
            HeaderValue::from_str(value).expect("value"),
        );
    }
    map
}

fn oauth(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        credential: AccountCredential::Oauth {
            account_uuid: format!("uuid-{name}"),
            access_token: format!("at-{name}"),
            refresh_token: format!("rt-{name}"),
            expires_at_ms: u64::MAX / 2,
            tier: None,
            last_refresh_ms: None,
        },
    }
}

fn params() -> SelectParams {
    SelectParams::from(&SchedulerConfig::default())
}

#[test]
fn unified_five_hour_headers_are_not_quota_evidence() {
    let parsed = headers::parse(&headers_of(&[
        ("anthropic-ratelimit-unified-5h-utilization", "1.0".into()),
        ("anthropic-ratelimit-unified-5h-reset", epoch_in(3_600)),
    ]));
    assert!(
        parsed.is_empty(),
        "a 5h-only response carries nothing llmux records: {parsed:?}"
    );
}

#[test]
fn codex_session_window_is_not_quota_evidence() {
    let parsed = headers::parse(&headers_of(&[
        ("x-codex-primary-used-percent", "100".into()),
        ("x-codex-primary-window-minutes", "300".into()),
        ("x-codex-primary-reset-at", epoch_in(3_600)),
    ]));
    assert!(
        parsed.is_empty(),
        "a codex 300-minute window is not filed anywhere: {parsed:?}"
    );
}

#[test]
fn an_account_whose_five_hour_headers_read_full_still_serves() {
    let pool = AccountPool::new(&[oauth("a")]);
    let now = SystemTime::now();
    let parsed = headers::parse(&headers_of(&[
        ("anthropic-ratelimit-unified-5h-utilization", "1.0".into()),
        ("anthropic-ratelimit-unified-5h-reset", epoch_in(3_600)),
        ("anthropic-ratelimit-unified-7d-utilization", "0.10".into()),
        ("anthropic-ratelimit-unified-7d-reset", epoch_in(86_400)),
    ]));
    pool.record_headers(&AccountId("a".into()), &parsed, now);
    let snapshot = pool.snapshot();
    assert_eq!(
        select::eligibility(&snapshot.accounts[0], &params(), now, false),
        None,
        "a full 5h reading never gates"
    );
    assert!(
        !matches!(
            pool.evaluate(None, &params(), now),
            Decision::Exhausted { .. }
        ),
        "the pool still serves"
    );
}

#[test]
fn a_usage_poll_five_hour_reading_never_gates() {
    let body = format!(
        r#"{{"five_hour":{{"utilization":100.0,"resets_at":{}}},"seven_day":{{"utilization":10.0,"resets_at":{}}}}}"#,
        epoch_in(3_600),
        epoch_in(86_400)
    );
    let usage = usage::parse_usage_body(body.as_bytes()).expect("a body with five_hour parses");
    let pool = AccountPool::new(&[oauth("a")]);
    let now = SystemTime::now();
    pool.record_usage(&AccountId("a".into()), &usage, now);
    let snapshot = pool.snapshot();
    assert_eq!(
        select::eligibility(&snapshot.accounts[0], &params(), now, false),
        None,
        "a usage poll that still reports five_hour at 100% never gates"
    );
}

#[test]
fn the_config_no_longer_carries_five_hour_max() {
    let json = serde_json::to_string(&Config::default()).expect("serialize");
    assert!(
        !json.contains("five_hour"),
        "no five_hour key is written: {json}"
    );
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn an_old_config_with_five_hour_max_still_loads() {
    let path = std::env::temp_dir().join(format!(
        "llmux-no-5h-{}-{}.json",
        std::process::id(),
        ulid::Ulid::new()
    ));
    let _guard = TempFile(path.clone());
    std::fs::write(
        &path,
        r#"{
  "scheduler": { "five_hour_max": 0.5, "seven_day_max": 0.8 },
  "account_limits": { "a": { "five_hour_max": 0.4, "seven_day_max": 0.7 } }
}"#,
    )
    .expect("write");
    let config = config::load_path(&path).expect("an old config with five_hour_max loads");
    assert!((config.scheduler.seven_day_max - 0.8).abs() < f64::EPSILON);
    assert_eq!(config.account_limits["a"].seven_day_max, Some(0.7));
    let rewritten = serde_json::to_string(&config).expect("serialize");
    assert!(
        !rewritten.contains("five_hour"),
        "the legacy key is dropped on the next write: {rewritten}"
    );
}
