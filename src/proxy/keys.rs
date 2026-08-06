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
use std::sync::{Arc, RwLock};

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
}

impl Tenant {
    pub fn local() -> Self {
        Self {
            id: "local".into(),
            name: "local".into(),
            // Keyless loopback is DATA-plane only: network position is not an
            // admin credential (an `ssh -L` peer looks loopback too).
            admin: false,
        }
    }

    pub fn legacy() -> Self {
        Self {
            id: "legacy".into(),
            name: "legacy".into(),
            // The shared proxy key retains its historical full capability.
            admin: true,
        }
    }

    fn from_key(key: &ClientKey) -> Self {
        Self {
            id: key.id.clone(),
            name: key.name.clone(),
            admin: matches!(key.kind, ClientKeyKind::Admin),
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
    /// The legacy shared proxy key digest, when configured.
    legacy_digest: Option<String>,
}

impl Snapshot {
    fn build(config: &Config) -> Self {
        let keys = config.client_keys.clone();
        let mut by_digest = HashMap::with_capacity(keys.len());
        for (i, key) in keys.iter().enumerate() {
            by_digest.insert(key.key_digest.clone(), i);
        }
        Self {
            keys,
            by_digest,
            legacy_digest: config
                .proxy
                .api_key
                .as_deref()
                .map(crate::config::client_key_digest),
        }
    }
}

/// Live registry: the one place the gate reads and mutations swap.
pub struct KeyRegistry {
    snapshot: RwLock<Arc<Snapshot>>,
}

impl KeyRegistry {
    pub fn from_config(config: &Config) -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(Snapshot::build(config))),
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
    /// - absent + loopback peer → `local` (data plane only).
    /// - anything else → denied. Keyless REMOTE is always denied, even when
    ///   no `proxy.api_key` is configured (keyless is loopback-only).
    pub fn resolve(&self, presented: Option<&str>, loopback: bool) -> Resolution {
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

    #[test]
    fn local_tenant_is_not_admin() {
        // Network position is identity, not privilege.
        assert!(!Tenant::local().admin);
    }
}
