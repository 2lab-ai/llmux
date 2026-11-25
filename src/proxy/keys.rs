//! Live client-key registry + tenant resolution (multi-tenant keys, #22).
//!
//! The auth gate must see key mutations (suspend/resume/rotate/revoke)
//! IMMEDIATELY — the daemon's `AppState.config` is a boot-time snapshot, so
//! persisting to disk alone never reaches a running gate. This module holds
//! the runtime truth: an immutable snapshot swapped atomically under a std
//! `RwLock` (state/runtime separation, same locking discipline as the
//! scheduler pool — sync, IO-free, never held across an await).
//!
//! Ordering contract: mutations persist to disk FIRST (`config::update_path`
//! read-merge-write), and only on success swap the in-memory snapshot from
//! the merged config. Mutation serialization is the caller's job
//! ([`crate::proxy::server::AppState`] takes the registry's write lock around
//! the whole persist-then-swap sequence).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::config::{ClientKey, ClientKeyKind, Config};

/// Attribution identity resolved by the auth gate, attached to the request as
/// an axum extension. `id` is the stable tenant bucket usage records carry:
/// `local` (keyless loopback), `legacy` (the shared `proxy.api_key`), or a
/// client-key id (`k-…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub id: String,
    /// Display name (key name; `local`/`legacy` for the builtin buckets).
    pub name: String,
    /// Admin scope: unlocks the control plane (`/llmux/*`). Identity and
    /// privilege are separate axes — loopback is NOT privilege.
    pub admin: bool,
    /// Resolved from a secret the key carried before a rotation (loopback
    /// only). Attribution survives; privilege does not (`admin` is false).
    pub retired_secret: bool,
}

impl Tenant {
    pub fn local() -> Self {
        Self {
            id: "local".into(),
            name: "local".into(),
            // Keyless loopback is DATA-plane only: network position is not an
            // admin credential (an `ssh -L` peer looks loopback too).
            admin: false,
            retired_secret: false,
        }
    }

    pub fn legacy() -> Self {
        Self {
            id: "legacy".into(),
            name: "legacy".into(),
            // The shared proxy key retains its historical full capability.
            admin: true,
            retired_secret: false,
        }
    }

    fn from_key(key: &ClientKey) -> Self {
        Self {
            id: key.id.clone(),
            name: key.name.clone(),
            admin: matches!(key.kind, ClientKeyKind::Admin),
            retired_secret: false,
        }
    }

    /// A pre-rotation secret presented from loopback: the key's identity for
    /// metering, never its admin scope.
    fn from_retired(key: &ClientKey) -> Self {
        Self {
            admin: false,
            retired_secret: true,
            ..Self::from_key(key)
        }
    }
}

/// Outcome of resolving a presented (or absent) credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Valid credential (or permitted keyless loopback) → tenant identity.
    Allowed(Tenant),
    /// A known key that is currently suspended.
    Suspended,
    /// A known key that has been revoked.
    Revoked,
    /// A secret the key carried before a rotation (still in its grace
    /// window), presented where it cannot be attributed: a remote peer, or the
    /// OpenAI / Gemini ingress, which refuse any unknown explicit key.
    Retired,
    /// No/unknown credential from a peer that required one.
    Denied,
}

/// Immutable snapshot of the issued-key set, indexed by digest for O(1)
/// lookup. Comparing SHA-256 digests via table lookup leaks nothing usable:
/// the digest of the presented key is not secret, and equality of digests is
/// the authentication predicate itself.
#[derive(Default)]
struct Snapshot {
    /// All keys (including revoked — attribution metadata lives here).
    keys: Vec<ClientKey>,
    /// digest → index into `keys`.
    by_digest: HashMap<String, usize>,
    /// pre-rotation digest → (index into `keys`, retired_at_ms)
    /// (`ClientKey::retired_digests`).
    retired_by_digest: HashMap<String, (usize, u64)>,
    /// The legacy shared proxy key digest, when configured.
    legacy_digest: Option<String>,
}

impl Snapshot {
    fn build(config: &Config) -> Self {
        let keys = config.client_keys.clone();
        let mut by_digest = HashMap::with_capacity(keys.len());
        let mut retired_by_digest = HashMap::new();
        for (i, key) in keys.iter().enumerate() {
            by_digest.insert(key.key_digest.clone(), i);
            for retired in &key.retired_digests {
                retired_by_digest.insert(retired.digest.clone(), (i, retired.retired_at_ms));
            }
        }
        Self {
            keys,
            by_digest,
            retired_by_digest,
            legacy_digest: config
                .proxy
                .api_key
                .as_deref()
                .map(crate::config::client_key_digest),
        }
    }
}

/// Requests that presented a key's pre-rotation secret since the daemon
/// started: the sign that some client still holds the old secret.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetiredUse {
    pub requests: u64,
    pub last_ms: u64,
}

/// Live registry: the one place the gate reads and mutations swap.
pub struct KeyRegistry {
    snapshot: RwLock<Arc<Snapshot>>,
    /// key id → [`RetiredUse`]. In memory only, outside the snapshot so a
    /// key mutation does not reset it. Touched only on the rare retired-secret
    /// path, never on a normal request.
    retired_uses: Mutex<HashMap<String, RetiredUse>>,
}

impl KeyRegistry {
    pub fn from_config(config: &Config) -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(Snapshot::build(config))),
            retired_uses: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically replace the snapshot from a freshly merged config. Called
    /// AFTER the disk write succeeded (persist-then-swap ordering).
    pub fn reload(&self, config: &Config) {
        let next = Arc::new(Snapshot::build(config));
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    fn snap(&self) -> Arc<Snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Resolve a presented credential to a tenant identity (identity axis
    /// only — the scope axis is enforced per-route by the gate).
    ///
    /// - present + matches legacy `proxy.api_key` → `legacy` (admin).
    /// - present + matches an issued key digest → that key's tenant, unless
    ///   revoked/suspended.
    /// - present + matches a key's pre-rotation digest retired less than
    ///   [`crate::config::RETIRED_SECRET_GRACE_MS`] ago → from loopback, that
    ///   key's tenant with `retired_secret` set and no admin scope (unless
    ///   revoked/suspended); otherwise [`Resolution::Retired`]. Loopback may
    ///   send no key at all and still be served, so attributing the old secret
    ///   there grants no access — it keeps the request on its owner's bill
    ///   instead of `local`. Past the grace window the digest is unknown again.
    ///   (The OpenAI / Gemini ingress pass `loopback = false` for an explicit
    ///   key, so there it is refused even locally; the gate also refuses it on
    ///   the control plane.)
    /// - absent + loopback peer → `local` (data plane only).
    /// - anything else → denied. Keyless REMOTE is always denied, even when
    ///   no `proxy.api_key` is configured (keyless is loopback-only).
    pub fn resolve(&self, presented: Option<&str>, loopback: bool) -> Resolution {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.resolve_at(presented, loopback, now_ms)
    }

    /// [`Self::resolve`] at an explicit clock (the grace window's only input).
    pub fn resolve_at(&self, presented: Option<&str>, loopback: bool, now_ms: u64) -> Resolution {
        let snap = self.snap();
        if let Some(presented) = presented {
            let digest = crate::config::client_key_digest(presented);
            if snap.legacy_digest.as_deref() == Some(digest.as_str()) {
                return Resolution::Allowed(Tenant::legacy());
            }
            if let Some(&i) = snap.by_digest.get(&digest) {
                let key = &snap.keys[i];
                if key.revoked_at_ms.is_some() {
                    return Resolution::Revoked;
                }
                if key.suspended {
                    return Resolution::Suspended;
                }
                return Resolution::Allowed(Tenant::from_key(key));
            }
            if let Some(&(i, _)) = snap.retired_by_digest.get(&digest).filter(|(_, at)| {
                now_ms.saturating_sub(*at) < crate::config::RETIRED_SECRET_GRACE_MS
            }) {
                let key = &snap.keys[i];
                if key.revoked_at_ms.is_some() {
                    return Resolution::Revoked;
                }
                if key.suspended {
                    return Resolution::Suspended;
                }
                if !loopback {
                    return Resolution::Retired;
                }
                return Resolution::Allowed(Tenant::from_retired(key));
            }
            // An unknown presented key falls through to the keyless rules for
            // loopback (matches the historical loopback exemption: a local
            // client with a stale key keeps working, attributed as `local`).
            if loopback {
                return Resolution::Allowed(Tenant::local());
            }
            return Resolution::Denied;
        }
        if loopback {
            return Resolution::Allowed(Tenant::local());
        }
        Resolution::Denied
    }

    /// Metadata view for the list endpoint / dashboard (never secrets).
    pub fn list(&self) -> Vec<ClientKey> {
        self.snap().keys.clone()
    }

    /// Count one request that presented `key_id`'s pre-rotation secret.
    /// Returns the updated tally (`requests == 1` is the first since start).
    pub fn note_retired_use(&self, key_id: &str, now_ms: u64) -> RetiredUse {
        let mut uses = self.retired_uses.lock().unwrap_or_else(|e| e.into_inner());
        let entry = uses.entry(key_id.to_string()).or_default();
        entry.requests = entry.requests.saturating_add(1);
        entry.last_ms = now_ms;
        *entry
    }

    /// Requests that presented `key_id`'s pre-rotation secret since start.
    pub fn retired_use(&self, key_id: &str) -> Option<RetiredUse> {
        self.retired_uses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key_id)
            .copied()
    }

    /// Resolve an attribution id to display metadata (name/email), for
    /// rendering usage rows. `local`/`legacy` resolve to themselves; unknown
    /// ids (pre-tenant history) resolve to `None`.
    pub fn display_name(&self, tenant_id: &str) -> Option<String> {
        match tenant_id {
            "local" | "legacy" => Some(tenant_id.to_string()),
            _ => {
                let snap = self.snap();
                snap.keys
                    .iter()
                    .find(|k| k.id == tenant_id)
                    .map(|k| k.name.clone())
            }
        }
    }

    /// Count of ACTIVE admin credentials in the registry (non-revoked,
    /// non-suspended admin keys). The legacy `proxy.api_key` is counted by the
    /// caller (it lives outside `client_keys`).
    pub fn active_admin_keys(&self) -> usize {
        self.snap()
            .keys
            .iter()
            .filter(|k| {
                matches!(k.kind, ClientKeyKind::Admin) && !k.suspended && k.revoked_at_ms.is_none()
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{generate_client_key, generate_client_key_id};

    fn key(kind: ClientKeyKind, suspended: bool) -> (ClientKey, String) {
        let issued = generate_client_key();
        (
            ClientKey {
                id: generate_client_key_id(),
                name: "pc-a".into(),
                email: Some("a@example.com".into()),
                kind,
                key_prefix: issued.prefix.clone(),
                key_digest: issued.digest.clone(),
                retired_digests: Vec::new(),
                suspended,
                created_at_ms: 1,
                revoked_at_ms: None,
            },
            issued.secret,
        )
    }

    fn config_with(keys: Vec<ClientKey>, legacy: Option<&str>) -> Config {
        let mut config = Config {
            client_keys: keys,
            ..Default::default()
        };
        config.proxy.api_key = legacy.map(Into::into);
        config
    }

    #[test]
    fn resolves_issued_key_to_its_tenant() {
        let (k, secret) = key(ClientKeyKind::Default, false);
        let id = k.id.clone();
        let reg = KeyRegistry::from_config(&config_with(vec![k], Some("lm-legacy")));
        match reg.resolve(Some(&secret), false) {
            Resolution::Allowed(t) => {
                assert_eq!(t.id, id);
                assert_eq!(t.name, "pc-a");
                assert!(!t.admin);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn legacy_key_resolves_admin() {
        let reg = KeyRegistry::from_config(&config_with(Vec::new(), Some("lm-legacy")));
        match reg.resolve(Some("lm-legacy"), false) {
            Resolution::Allowed(t) => {
                assert_eq!(t.id, "legacy");
                assert!(t.admin);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn suspended_and_revoked_are_distinct_denials() {
        let (mut k1, s1) = key(ClientKeyKind::Default, true);
        k1.id = "k-1".into();
        let (mut k2, s2) = key(ClientKeyKind::Default, false);
        k2.id = "k-2".into();
        k2.revoked_at_ms = Some(2);
        let reg = KeyRegistry::from_config(&config_with(vec![k1, k2], None));
        assert_eq!(reg.resolve(Some(&s1), false), Resolution::Suspended);
        assert_eq!(reg.resolve(Some(&s2), false), Resolution::Revoked);
    }

    #[test]
    fn keyless_loopback_is_local_and_keyless_remote_is_denied() {
        // Keyless remote is denied EVEN with no proxy key configured —
        // keyless is loopback-only (issue #22 P0-B).
        let reg = KeyRegistry::from_config(&config_with(Vec::new(), None));
        assert_eq!(
            reg.resolve(None, true),
            Resolution::Allowed(Tenant::local())
        );
        assert_eq!(reg.resolve(None, false), Resolution::Denied);
        assert_eq!(reg.resolve(Some("lmk-unknown"), false), Resolution::Denied);
    }

    #[test]
    fn reload_swaps_snapshot_without_restart() {
        let (k, secret) = key(ClientKeyKind::Default, false);
        let mut config = config_with(vec![k], None);
        let reg = KeyRegistry::from_config(&config);
        assert!(matches!(
            reg.resolve(Some(&secret), false),
            Resolution::Allowed(_)
        ));
        config.client_keys[0].suspended = true;
        reg.reload(&config);
        assert_eq!(reg.resolve(Some(&secret), false), Resolution::Suspended);
    }

    /// Rotation time in these tests (epoch ms).
    const ROTATED_AT: u64 = 1_000_000;

    /// `key` rotated once at [`ROTATED_AT`]: the old secret is retired.
    fn rotated(kind: ClientKeyKind) -> (ClientKey, String, String) {
        let (mut k, old) = key(kind, false);
        let issued = generate_client_key();
        k.retire_digest(issued.digest, ROTATED_AT);
        (k, old, issued.secret)
    }

    #[test]
    fn retired_secret_is_attributed_only_inside_the_grace_window() {
        let (k, old, _) = rotated(ClientKeyKind::Default);
        let id = k.id.clone();
        let reg = KeyRegistry::from_config(&config_with(vec![k], None));
        let grace = crate::config::RETIRED_SECRET_GRACE_MS;
        assert!(matches!(
            reg.resolve_at(Some(&old), true, ROTATED_AT + grace - 1),
            Resolution::Allowed(t) if t.id == id && t.retired_secret
        ));
        assert_eq!(
            reg.resolve_at(Some(&old), true, ROTATED_AT + grace),
            Resolution::Allowed(Tenant::local()),
            "past the window the old secret is unknown again"
        );
        assert_eq!(
            reg.resolve_at(Some(&old), false, ROTATED_AT + grace),
            Resolution::Denied
        );
    }

    #[test]
    fn retired_secret_from_loopback_meters_to_its_key_without_admin() {
        let (k, old, new) = rotated(ClientKeyKind::Admin);
        let id = k.id.clone();
        let reg = KeyRegistry::from_config(&config_with(vec![k], None));
        match reg.resolve_at(Some(&old), true, ROTATED_AT) {
            Resolution::Allowed(t) => {
                assert_eq!(t.id, id);
                assert!(t.retired_secret);
                assert!(!t.admin, "a retired secret never carries admin scope");
            }
            other => panic!("expected Allowed, got {other:?}"),
        }
        match reg.resolve_at(Some(&new), true, ROTATED_AT) {
            Resolution::Allowed(t) => assert!(t.admin && !t.retired_secret),
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn retired_secret_from_a_remote_peer_is_refused() {
        let (k, old, _) = rotated(ClientKeyKind::Default);
        let reg = KeyRegistry::from_config(&config_with(vec![k], None));
        assert_eq!(
            reg.resolve_at(Some(&old), false, ROTATED_AT),
            Resolution::Retired
        );
    }

    #[test]
    fn retired_secret_of_a_suspended_or_revoked_key_is_refused_even_on_loopback() {
        let (mut suspended, s_old, _) = rotated(ClientKeyKind::Default);
        suspended.id = "k-1".into();
        suspended.suspended = true;
        let (mut revoked, r_old, _) = rotated(ClientKeyKind::Default);
        revoked.id = "k-2".into();
        revoked.revoked_at_ms = Some(2);
        let reg = KeyRegistry::from_config(&config_with(vec![suspended, revoked], None));
        assert_eq!(
            reg.resolve_at(Some(&s_old), true, ROTATED_AT),
            Resolution::Suspended
        );
        assert_eq!(
            reg.resolve_at(Some(&r_old), true, ROTATED_AT),
            Resolution::Revoked
        );
    }

    #[test]
    fn retired_use_is_counted_per_key_and_survives_reload() {
        let (k, _, _) = rotated(ClientKeyKind::Default);
        let id = k.id.clone();
        let config = config_with(vec![k], None);
        let reg = KeyRegistry::from_config(&config);
        assert_eq!(reg.retired_use(&id), None);
        assert_eq!(reg.note_retired_use(&id, 10).requests, 1);
        reg.reload(&config);
        let used = reg.note_retired_use(&id, 20);
        assert_eq!(
            used,
            RetiredUse {
                requests: 2,
                last_ms: 20
            }
        );
        assert_eq!(reg.retired_use(&id), Some(used));
        assert_eq!(reg.retired_use("k-other"), None);
    }

    #[test]
    fn local_tenant_is_not_admin() {
        // Network position is identity, not privilege.
        assert!(!Tenant::local().admin);
    }
}
