//! CLI black-box integration tests: spawn the BUILT binary
//! (`env!("CARGO_BIN_EXE_llmux")`) and assert its user-facing contract —
//! stdout/stderr strings and process exit codes.
//!
//! This is deliberately distinct from `tests/e2e.rs`, which drives the proxy
//! *in-process* (`llmux::proxy::server::serve`) and never spawns the binary.
//! Here every assertion is against the real CLI surface a user/script sees.
//!
//! Hermeticity (enforced on EVERY test):
//!   * `$LLMUX_CONFIG` points at a per-test tempdir file, so the binary never
//!     reads or writes the real `~/.config/llmux.json`.
//!   * Every seeded config uses a non-default, unique high port, and no test
//!     starts a real daemon or makes a network call (the one exception, the
//!     `run` test, talks only to an in-test loopback status mock it controls).
//!
//! Each test names the spec row IDs it covers in a comment.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Absolute path to the binary cargo built for this test target.
const BIN: &str = env!("CARGO_BIN_EXE_llmux");

/// A self-cleaning temp directory (no `tempfile` dev-dep in this crate, so a
/// tiny hand-rolled one keeps the test tree dependency-free). The directory is
/// created under the system temp dir with a process+counter-unique name and
/// removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "llmux-cli-test-{}-{}-{n}",
            std::process::id(),
            // Nanos give intra-process uniqueness even across counter resets.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create tempdir");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A hermetic test harness: a tempdir holding the config file `$LLMUX_CONFIG`
/// will point at. Seed the config (or let a `login`/`import` create it), then
/// run the binary through [`Harness::cmd`].
struct Harness {
    dir: TempDir,
    config_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new();
        let config_path = dir.path().join("llmux.json");
        Harness { dir, config_path }
    }

    /// Write `json` verbatim as the config file `$LLMUX_CONFIG` resolves to.
    fn seed_config(&self, json: &str) {
        std::fs::write(&self.config_path, json).expect("seed config");
    }

    /// Read the config file back as JSON (to assert a command's effect).
    fn read_config(&self) -> serde_json::Value {
        let raw = std::fs::read_to_string(&self.config_path).expect("read config");
        serde_json::from_str(&raw).expect("parse config")
    }

    /// Build a `Command` for the built binary with the hermetic env applied:
    /// `$LLMUX_CONFIG` → this harness's config file, `XDG_*` redirected into
    /// the tempdir so even commands that resolve a state dir never touch the
    /// real home, and demo mode explicitly off.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("LLMUX_CONFIG", &self.config_path)
            .env("XDG_CONFIG_HOME", self.dir.path())
            .env("XDG_STATE_HOME", self.dir.path())
            .env_remove("LLMUX_DEMO_MODE");
        cmd
    }
}

/// A captured process result, with helpers for asserting on it.
struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Output {
    fn run(mut cmd: Command) -> Self {
        let out = cmd.output().expect("spawn llmux binary");
        Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Run with `input` fed to the child's stdin (closed after the write).
    fn run_with_stdin(mut cmd: Command, input: &str) -> Self {
        use std::process::Stdio;
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn llmux binary");
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait for llmux binary");
        Output {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// A minimal valid v1 config with the given port and account array, ready to
/// hand to [`Harness::seed_config`]. `accounts` is a raw JSON array literal.
fn config_json(port: u16, accounts: &str) -> String {
    format!(r#"{{"version":1,"proxy":{{"port":{port}}},"accounts":{accounts}}}"#)
}

// ---------------------------------------------------------------------------
// DIST-11 — `--version` shape.
// ---------------------------------------------------------------------------

#[test]
fn version_matches_dist11_shape() {
    // DIST-11: `llmux --version` → `llmux <semver> (<channel> <build>)`.
    let h = Harness::new();
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("--version");
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let line = out.stdout.trim_end();
    // ^llmux \d+\.\d+\.\d+ \(.+ .+\)$ — checked by structure (no regex dep).
    let rest = line
        .strip_prefix("llmux ")
        .unwrap_or_else(|| panic!("missing 'llmux ' prefix: {line:?}"));
    let (semver, build) = rest
        .split_once(' ')
        .unwrap_or_else(|| panic!("no space after semver: {line:?}"));
    let parts: Vec<&str> = semver.split('.').collect();
    assert_eq!(parts.len(), 3, "semver must be X.Y.Z: {semver:?}");
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "semver components must be digits: {semver:?}"
    );
    // `(.+ .+)` — a parenthesized "channel build" with both parts non-empty.
    let inner = build
        .strip_prefix('(')
        .and_then(|b| b.strip_suffix(')'))
        .unwrap_or_else(|| panic!("build must be parenthesized: {build:?}"));
    let (channel, id) = inner
        .split_once(' ')
        .unwrap_or_else(|| panic!("build needs 'channel id': {inner:?}"));
    assert!(
        !channel.is_empty() && !id.is_empty(),
        "build parts: {inner:?}"
    );
}

// ---------------------------------------------------------------------------
// CLI-12 — `env` prints the integration exports.
// ---------------------------------------------------------------------------

#[test]
fn env_prints_base_url_export() {
    // CLI-12: `llmux env` → `export ANTHROPIC_BASE_URL=http://localhost:<port>`.
    let h = Harness::new();
    h.seed_config(&config_json(39101, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("env");
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("export ANTHROPIC_BASE_URL=http://localhost:39101"),
        "stdout: {}",
        out.stdout
    );
    // Admin-0 self-heal (multi-tenant #22): a config with no proxy.api_key
    // gets one minted on load_or_init — the control plane must always keep an
    // admin credential — so `env` now exports the healed key like any other.
    assert!(
        out.stdout.contains("export ANTHROPIC_API_KEY=lm-"),
        "healed admin key exported: {}",
        out.stdout
    );
}

#[test]
fn env_prints_api_key_export_when_proxy_key_set() {
    // CLI-12: with proxy.api_key set, `env` also emits the ANTHROPIC_API_KEY
    // export (for off-host clients that must authenticate to the proxy).
    let h = Harness::new();
    h.seed_config(
        r#"{"version":1,"proxy":{"port":39102,"api_key":"lm-secret-key"},"accounts":[]}"#,
    );
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("env");
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("export ANTHROPIC_BASE_URL=http://localhost:39102"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("export ANTHROPIC_API_KEY=lm-secret-key"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn env_codex_exports_selected_remote_key_and_responses_url() {
    let h = Harness::new();
    h.seed_config(r#"{"version":1,"proxy":{"port":39103,"api_key":"lm-local-admin"},"remote":{"host":"gateway.example","port":4567,"api_key":"lmk-remote-client"},"accounts":[]}"#);
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["--remote", "override.example:5678", "env", "--codex"]);
        c
    });
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out
        .stdout
        .contains("export OPENAI_BASE_URL=http://override.example:5678/v1\n"));
    assert!(out
        .stdout
        .contains("export OPENAI_API_KEY=lmk-remote-client\n"));
    assert!(out
        .stdout
        .contains("model_providers.llmux_env.requires_openai_auth=false"));
    assert!(out
        .stdout
        .contains("model_providers.llmux_env.env_key=\"OPENAI_API_KEY\""));
    assert!(!out.stdout.contains("lm-local-admin"));
    assert!(!out.stdout.contains("ANTHROPIC_"));
}

#[test]
fn env_codex_initializes_local_key_and_refuses_missing_remote_key_without_exports() {
    let h = Harness::new();
    h.seed_config(&config_json(39104, "[]"));
    let local = Output::run({
        let mut c = h.cmd();
        c.args(["env", "--codex"]);
        c
    });
    assert_eq!(local.code, Some(0), "{}", local.stderr);
    assert!(local
        .stdout
        .contains("export OPENAI_BASE_URL=http://localhost:39104/v1\n"));
    assert!(local.stdout.contains("export OPENAI_API_KEY=lm-"));
    for key in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!("  "),
    ] {
        h.seed_config(
            &serde_json::json!({
                "version": 1,
                "proxy": {"api_key": "lm-local-admin"},
                "remote": {"host": "gateway.example", "api_key": key},
                "accounts": []
            })
            .to_string(),
        );
        let out = Output::run({
            let mut c = h.cmd();
            c.args(["env", "--codex"])
                .env("OPENAI_API_KEY", "unrelated-inherited-key");
            c
        });
        assert_ne!(out.code, Some(0));
        assert_eq!(out.stdout, "");
        assert!(out.stderr.contains("remote.api_key"));
        assert!(!out.stderr.contains("lm-local-admin"));
        assert!(!out.stderr.contains("unrelated-inherited-key"));
    }
}

#[cfg(unix)]
#[test]
fn env_exports_round_trip_shell_metacharacters_without_executing_them() {
    let h = Harness::new();
    let host = "gateway'$(touch should-not-exist)`touch also-not`;\\\"\n.example";
    let key = "lmk-quote'$(touch should-not-exist)`touch also-not`;\\\"\nsecret";
    h.seed_config(
        &serde_json::json!({
            "version": 1,
            "proxy": {"api_key": "lm-local-admin"},
            "remote": {"host": host, "port": 4567, "api_key": key},
            "accounts": []
        })
        .to_string(),
    );
    for codex in [false, true] {
        let out = Output::run({
            let mut c = h.cmd();
            c.arg("env");
            if codex {
                c.arg("--codex");
            }
            c
        });
        assert_eq!(out.code, Some(0), "{}", out.stderr);
        let prefix = if codex { "OPENAI" } else { "ANTHROPIC" };
        let script = format!(
            "{}\nprintf '%s\\0%s' \"${prefix}_BASE_URL\" \"${prefix}_API_KEY\"",
            out.stdout
        );
        let shell = Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(h.dir.path())
            .output()
            .expect("shell");
        assert!(shell.status.success(), "{:?}", shell.stderr);
        let suffix = if codex { "/v1" } else { "" };
        assert_eq!(
            shell.stdout,
            format!("http://{host}:4567{suffix}\0{key}").into_bytes()
        );
        assert!(!h.dir.path().join("should-not-exist").exists());
        assert!(!h.dir.path().join("also-not").exists());
        if codex {
            // Un-comment the suggested command and capture its argv instead of
            // launching a client. TOML then shell quoting must survive together.
            let command = out
                .stdout
                .lines()
                .find_map(|l| l.strip_prefix("# codex "))
                .expect("Codex hint");
            let capture = format!("set -- {command}; printf '%s\\0' \"$@\"");
            let argv = Command::new("sh")
                .args(["-c", &capture])
                .current_dir(h.dir.path())
                .output()
                .expect("argv capture");
            assert!(argv.status.success());
            let expected = format!(
                "model_providers.llmux_env.base_url={}\0",
                serde_json::json!(format!("http://{host}:4567/v1"))
            );
            assert!(String::from_utf8(argv.stdout)
                .expect("utf8")
                .contains(&expected));
            assert!(!h.dir.path().join("should-not-exist").exists());
        }
    }
}

// ---------------------------------------------------------------------------
// CLI-14 / CLI-15 — `status` with no server.
// ---------------------------------------------------------------------------

#[test]
fn status_no_server_reports_not_running_and_exits_1() {
    // CLI-14: human-readable `status` with nothing on the port → the
    // "not running" section and exit code 1.
    let h = Harness::new();
    h.seed_config(&config_json(39103, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("status");
        c
    });
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("status: not running"), "{}", out.stdout);
    assert!(out.stdout.contains("port: 39103"), "{}", out.stdout);
}

#[test]
fn status_json_no_server_emits_document_and_exits_1() {
    // CLI-15: `status --json` with no server → a JSON document
    // {"server":"not running","port":<port>} and exit code 1.
    let h = Harness::new();
    h.seed_config(&config_json(39104, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["status", "--json"]);
        c
    });
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    let doc: serde_json::Value = serde_json::from_str(out.stdout.trim())
        .unwrap_or_else(|e| panic!("not JSON: {e}\n{}", out.stdout));
    assert_eq!(doc["server"], "not running", "{doc}");
    assert_eq!(doc["port"], 39104, "{doc}");
}

// ---------------------------------------------------------------------------
// CLI-16 / CLI-17 — `accounts` listing.
// ---------------------------------------------------------------------------

#[test]
fn accounts_empty_prints_guidance_and_exits_0() {
    // CLI-16: `accounts` with no configured accounts → the "No accounts
    // configured." guidance and exit 0.
    let h = Harness::new();
    h.seed_config(&config_json(39105, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("accounts");
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("No accounts configured."),
        "{}",
        out.stdout
    );
}

#[test]
fn accounts_apikey_is_masked() {
    // CLI-16: a seeded apikey account lists as a masked line (prefix + "...",
    // never the full secret).
    let h = Harness::new();
    h.seed_config(&config_json(
        39106,
        r#"[{"name":"api-1","type":"apikey","api_key":"sk-ant-api03-SECRETSECRETSECRET"}]"#,
    ));
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("accounts");
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("api-1"), "{}", out.stdout);
    assert!(out.stdout.contains("(apikey)"), "{}", out.stdout);
    // Masked: first 15 chars + "..." present, full secret absent.
    assert!(out.stdout.contains("sk-ant-api03-SE..."), "{}", out.stdout);
    assert!(
        !out.stdout.contains("SECRETSECRETSECRET"),
        "full secret leaked: {}",
        out.stdout
    );
}

#[test]
fn accounts_verbose_shows_oauth_token_detail() {
    // CLI-17: `accounts -v` on an oauth account adds the verbose Uuid/Token
    // (expiry) detail lines that the non-verbose listing omits.
    let h = Harness::new();
    // Expiry ~2h in the future so the detail reads "expires in 1h ..".
    let expires_at_ms = (now_secs() + 2 * 3600) * 1000;
    h.seed_config(&config_json(
        39107,
        &format!(
            r#"[{{"name":"claude:me@x.com","type":"oauth","account_uuid":"uuid-abc",
                  "access_token":"sk-ant-oat01-AAA","refresh_token":"sk-ant-ort01-AAA",
                  "expires_at_ms":{expires_at_ms},"tier":"max"}}]"#
        ),
    ));

    // Non-verbose: no Uuid/Token detail.
    let plain = Output::run({
        let mut c = h.cmd();
        c.arg("accounts");
        c
    });
    assert_eq!(plain.code, Some(0), "stderr: {}", plain.stderr);
    assert!(plain.stdout.contains("(oauth, max)"), "{}", plain.stdout);
    assert!(!plain.stdout.contains("Token:"), "{}", plain.stdout);

    // Verbose: Uuid + Token expiry detail appear.
    let verbose = Output::run({
        let mut c = h.cmd();
        c.args(["accounts", "-v"]);
        c
    });
    assert_eq!(verbose.code, Some(0), "stderr: {}", verbose.stderr);
    assert!(verbose.stdout.contains("Uuid:"), "{}", verbose.stdout);
    assert!(verbose.stdout.contains("uuid-abc"), "{}", verbose.stdout);
    assert!(verbose.stdout.contains("Token:"), "{}", verbose.stdout);
    assert!(verbose.stdout.contains("expires in"), "{}", verbose.stdout);
}

// ---------------------------------------------------------------------------
// .prd/16 — `accounts refresh|resets|reset` (codex usage controls).
// ---------------------------------------------------------------------------

#[test]
fn accounts_subcommands_are_additive_to_the_bare_listing() {
    // The usage controls are subcommands OF `accounts`: the bare listing (and
    // its flags) keep working, and `--help` advertises all three.
    let h = Harness::new();
    h.seed_config(&config_json(39121, "[]"));
    let help = Output::run({
        let mut c = h.cmd();
        c.args(["accounts", "--help"]);
        c
    });
    assert_eq!(help.code, Some(0), "stderr: {}", help.stderr);
    for sub in ["refresh", "resets", "reset"] {
        assert!(help.stdout.contains(sub), "{sub} missing:\n{}", help.stdout);
    }
    // The bare listing is unchanged (offline, exit 0, no daemon needed).
    let list = Output::run({
        let mut c = h.cmd();
        c.arg("accounts");
        c
    });
    assert_eq!(list.code, Some(0), "stderr: {}", list.stderr);
    assert!(
        list.stdout.contains("No accounts configured."),
        "{}",
        list.stdout
    );
}

#[test]
fn accounts_reset_without_yes_refuses_on_a_non_tty_before_any_network_call() {
    // A redemption is irreversible, so a non-TTY invocation without `--yes`
    // is refused BEFORE the daemon is contacted. The seeded port has no
    // daemon: if the refusal happened after the request, the error would be a
    // connection failure instead.
    let h = Harness::new();
    h.seed_config(&config_json(39122, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["accounts", "reset", "codex:me@x.com"]);
        c
    });
    assert_eq!(out.code, Some(1), "stdout: {}", out.stdout);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert!(out.stderr.contains("codex:me@x.com"), "{}", out.stderr);
    assert!(
        !out.stderr.contains("request failed"),
        "the refusal must precede any network call: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("request id"),
        "no idempotency key is minted for a refused redemption: {}",
        out.stdout
    );
}

#[test]
fn accounts_refresh_without_a_daemon_fails_cleanly() {
    // The controls act on the DAEMON; with nothing on the port the command
    // fails with a transport error and a nonzero exit — never a fake green.
    let h = Harness::new();
    h.seed_config(&config_json(39123, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["accounts", "refresh"]);
        c
    });
    assert_eq!(out.code, Some(1), "stdout: {}", out.stdout);
    assert!(out.stderr.contains("refresh"), "{}", out.stderr);
}

// ---------------------------------------------------------------------------
// CLI-07 — `login --api`.
// ---------------------------------------------------------------------------

#[test]
fn login_api_writes_apikey_account_from_stdin() {
    // CLI-07: `login --api` reads the key from stdin and writes an apikey
    // account; assert by reading the config back.
    let h = Harness::new();
    h.seed_config(&config_json(39108, "[]"));
    let out = Output::run_with_stdin(
        {
            let mut c = h.cmd();
            c.args(["login", "--api"]);
            c
        },
        "sk-ant-api03-FROM-STDIN-1234567890\n",
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(r#"Added API key account "api-1""#),
        "{}",
        out.stdout
    );

    let cfg = h.read_config();
    let accounts = cfg["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 1, "{cfg}");
    assert_eq!(accounts[0]["name"], "api-1", "{cfg}");
    assert_eq!(accounts[0]["type"], "apikey", "{cfg}");
    assert_eq!(
        accounts[0]["api_key"], "sk-ant-api03-FROM-STDIN-1234567890",
        "{cfg}"
    );
}

#[test]
fn login_api_empty_stdin_errors() {
    // CLI-07: empty stdin → "no API key provided" error, exit 1, config
    // unchanged (still no accounts).
    let h = Harness::new();
    h.seed_config(&config_json(39109, "[]"));
    let out = Output::run_with_stdin(
        {
            let mut c = h.cmd();
            c.args(["login", "--api"]);
            c
        },
        "\n",
    );
    assert_eq!(
        out.code,
        Some(1),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("no API key provided"),
        "stderr: {}",
        out.stderr
    );
    let cfg = h.read_config();
    assert!(cfg["accounts"].as_array().unwrap().is_empty(), "{cfg}");
}

// ---------------------------------------------------------------------------
// CLI-08 (partial) — `login` clap conflict.
// ---------------------------------------------------------------------------

#[test]
fn login_api_and_codex_conflict_exits_2() {
    // CLI-08 (partial): `--api` and `--codex` are mutually exclusive; clap
    // rejects the combination with usage error exit code 2.
    let h = Harness::new();
    h.seed_config(&config_json(39110, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["login", "--api", "--codex"]);
        c
    });
    assert_eq!(
        out.code,
        Some(2),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("cannot be used with"),
        "stderr: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// CLI-10 / CLI-11 — `import`.
// ---------------------------------------------------------------------------

#[test]
fn import_from_file_imports_account() {
    // CLI-10: `import --from <fixture>` reads a teamclaude-shaped credentials
    // file and imports its account (assert via config read-back).
    let h = Harness::new();
    h.seed_config(&config_json(39111, "[]"));
    let fixture = h.dir.path().join("teamclaude.json");
    std::fs::write(
        &fixture,
        r#"{"accounts":[{"name":"primary","type":"oauth","accountUuid":"uuid-p",
            "accessToken":"sk-ant-oat01-X","refreshToken":"sk-ant-ort01-X",
            "expiresAt":1750000000000}]}"#,
    )
    .expect("write fixture");

    let out = Output::run({
        let mut c = h.cmd();
        c.args(["import", "--from"]).arg(&fixture);
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(r#"Added account "primary""#),
        "{}",
        out.stdout
    );

    let cfg = h.read_config();
    let accounts = cfg["accounts"].as_array().expect("accounts");
    assert_eq!(accounts.len(), 1, "{cfg}");
    assert_eq!(accounts[0]["name"], "primary", "{cfg}");
    assert_eq!(accounts[0]["type"], "oauth", "{cfg}");
    assert_eq!(accounts[0]["account_uuid"], "uuid-p", "{cfg}");
}

#[test]
fn import_json_blob_imports_account() {
    // CLI-11: `import --json '<blob>'` imports a single inline account.
    let h = Harness::new();
    h.seed_config(&config_json(39112, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args([
            "import",
            "--json",
            r#"{"name":"inline-api","type":"apikey","apiKey":"sk-ant-api03-INLINE"}"#,
        ]);
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(r#"Added account "inline-api""#),
        "{}",
        out.stdout
    );

    let cfg = h.read_config();
    let accounts = cfg["accounts"].as_array().expect("accounts");
    assert_eq!(accounts.len(), 1, "{cfg}");
    assert_eq!(accounts[0]["name"], "inline-api", "{cfg}");
    assert_eq!(accounts[0]["type"], "apikey", "{cfg}");
    assert_eq!(accounts[0]["api_key"], "sk-ant-api03-INLINE", "{cfg}");
}

#[test]
fn import_from_and_json_conflict_exits_2() {
    // CLI-10/CLI-11: `--from` and `--json` are mutually exclusive; clap
    // rejects the combination with usage error exit code 2.
    let h = Harness::new();
    h.seed_config(&config_json(39113, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["import", "--from", "/nonexistent/x", "--json", "{}"]);
        c
    });
    assert_eq!(
        out.code,
        Some(2),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("cannot be used with"),
        "stderr: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// CLI-18 — `remove`.
// ---------------------------------------------------------------------------

#[test]
fn remove_without_yes_non_tty_errors() {
    // CLI-18: `remove <name>` with stdin not a TTY and no --yes refuses
    // (exit 1) and leaves the account in place.
    let h = Harness::new();
    h.seed_config(&config_json(
        39114,
        r#"[{"name":"api-1","type":"apikey","api_key":"sk-ant-api03-X"}]"#,
    ));
    // Closed stdin (/dev/null-equivalent) → not a TTY.
    let out = Output::run_with_stdin(
        {
            let mut c = h.cmd();
            c.args(["remove", "api-1"]);
            c
        },
        "",
    );
    assert_eq!(
        out.code,
        Some(1),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("without confirmation; pass --yes"),
        "stderr: {}",
        out.stderr
    );
    let cfg = h.read_config();
    assert_eq!(cfg["accounts"].as_array().unwrap().len(), 1, "{cfg}");
}

#[test]
fn remove_with_yes_removes_account() {
    // CLI-18: `remove <name> --yes` on a seeded account removes it (assert via
    // config read-back: the array is now empty).
    let h = Harness::new();
    h.seed_config(&config_json(
        39115,
        r#"[{"name":"api-1","type":"apikey","api_key":"sk-ant-api03-X"}]"#,
    ));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["remove", "api-1", "--yes"]);
        c
    });
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(r#"Removed account "api-1""#),
        "{}",
        out.stdout
    );
    let cfg = h.read_config();
    assert!(cfg["accounts"].as_array().unwrap().is_empty(), "{cfg}");
}

#[test]
fn remove_nonexistent_errors() {
    // CLI-18: `remove <missing> --yes` → a clean not-found error, exit 1.
    let h = Harness::new();
    h.seed_config(&config_json(
        39116,
        r#"[{"name":"api-1","type":"apikey","api_key":"sk-ant-api03-X"}]"#,
    ));
    let out = Output::run({
        let mut c = h.cmd();
        c.args(["remove", "ghost", "--yes"]);
        c
    });
    assert_eq!(
        out.code,
        Some(1),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(out.stderr.contains("not found"), "stderr: {}", out.stderr);
    // The real account survives the failed removal.
    let cfg = h.read_config();
    assert_eq!(cfg["accounts"].as_array().unwrap().len(), 1, "{cfg}");
}

// ---------------------------------------------------------------------------
// CLI-13 (refusal path) — `dashboard` with no daemon.
// ---------------------------------------------------------------------------

#[test]
fn dashboard_no_daemon_refuses_and_exits_1() {
    // CLI-13 (refusal path): `dashboard` with nothing on the port refuses
    // cleanly with a "no llmux daemon" error and exit 1 — no TUI, no hang.
    let h = Harness::new();
    h.seed_config(&config_json(39117, "[]"));
    let out = Output::run({
        let mut c = h.cmd();
        c.arg("dashboard");
        c
    });
    assert_eq!(
        out.code,
        Some(1),
        "stdout: {} stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("no llmux daemon at localhost:39117"),
        "stderr: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// CLI-02 — `run` injects ANTHROPIC_BASE_URL and propagates the child exit code.
// ---------------------------------------------------------------------------

/// CLI-02: `llmux run` must (a) inject `ANTHROPIC_BASE_URL=http://localhost:<port>`
/// into the spawned `claude` and (b) propagate `claude`'s exit code.
///
/// Hermetic wiring: we stand up our own loopback `/llmux/status` mock that
/// answers as a same-version llmux daemon, so `run` takes the
/// `AlreadyRunning` branch and never spawns a real daemon; and we put a stub
/// `claude` (which echoes `$ANTHROPIC_BASE_URL` and exits a chosen code) first
/// on the child's PATH. No network beyond loopback, no real daemon.
#[tokio::test]
async fn run_injects_base_url_and_propagates_exit_code() {
    use axum::routing::get;
    use axum::Router;

    // Mirror the binary's own version string verbatim so `should_restart`
    // sees a same-version daemon and reuses it (the restart path is never
    // taken — no real daemon is spawned).
    let version_out = std::process::Command::new(BIN)
        .arg("--version")
        .output()
        .expect("llmux --version");
    let version = String::from_utf8(version_out.stdout)
        .expect("utf8 version")
        .trim()
        .to_string();
    assert!(version.starts_with("llmux "), "version: {version}");

    // Bind a loopback status mock that classifies as a running llmux daemon.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind status mock");
    let port = listener.local_addr().expect("addr").port();
    let body = serde_json::json!({
        "version": version,
        "pid": 4321,
        "accounts": [],
    })
    .to_string();
    let app = Router::new().route("/llmux/status", get(move || async move { body }));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // Hermetic config: an account (so `run` doesn't refuse on empty) and the
    // mock's port.
    let h = Harness::new();
    h.seed_config(&config_json(
        port,
        r#"[{"name":"api-1","type":"apikey","api_key":"sk-ant-api03-X"}]"#,
    ));

    // Stub `claude`: echo the injected base URL, exit a recognizable code (7).
    let bindir = h.dir.path().join("bin");
    std::fs::create_dir_all(&bindir).expect("bindir");
    let stub = bindir.join("claude");
    std::fs::write(
        &stub,
        "#!/bin/sh\necho \"BASE_URL=$ANTHROPIC_BASE_URL\"\nexit 7\n",
    )
    .expect("write stub claude");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }

    let path_env = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let config_path = h.config_path.clone();
    let state_dir = h.dir.path().to_path_buf();

    // The child blocks (it waits on the stub), so run it off the async runtime
    // thread; the mock keeps serving on this runtime meanwhile.
    let out = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(BIN);
        cmd.arg("run")
            .env("LLMUX_CONFIG", &config_path)
            .env("XDG_CONFIG_HOME", &state_dir)
            .env("XDG_STATE_HOME", &state_dir)
            .env_remove("LLMUX_DEMO_MODE")
            .env("PATH", path_env);
        let o = cmd.output().expect("spawn llmux run");
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    })
    .await
    .expect("join run");

    server.abort();

    let (code, stdout, stderr) = out;
    // (a) base-url injection: the stub echoed exactly the proxy URL.
    assert!(
        stdout.contains(&format!("BASE_URL=http://localhost:{port}")),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    // (b) exit-code propagation: the stub's 7 reaches the caller.
    assert_eq!(code, Some(7), "stdout: {stdout}\nstderr: {stderr}");
}

// ---------------------------------------------------------------------------
// Shutdown drain deadline — the REAL binary, end to end (iq-64 zombie daemon,
// 2026-09-10..21): a client that never closes its connection must not keep a
// retired daemon alive past the drain deadline. Runs `llmux server --no-tui`
// with `LLMUX_SHUTDOWN_DRAIN_DEADLINE_SECS=1`, holds a half-sent request open,
// POSTs `/llmux/shutdown`, and asserts the PROCESS exits — not just that the
// port frees — with the deadline warning on stderr.
// ---------------------------------------------------------------------------

#[test]
fn server_exits_at_the_drain_deadline_with_a_connection_still_open() {
    use std::io::{BufRead as _, Read as _};
    use std::net::TcpStream;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let h = Harness::new();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        probe.local_addr().expect("probe addr").port()
    };
    // This test measures local connection drain only. The default immediate
    // idle sweep would send this fake key to the real upstream and can leave
    // blocking DNS work running during Tokio runtime teardown.
    h.seed_config(&format!(
        r#"{{"version":1,"proxy":{{"port":{port},"api_key":"testkey","idle_probe":{{"enabled":false}}}},"accounts":[{{"name":"fake","type":"apikey","api_key":"sk-ant-test"}}]}}"#
    ));

    let mut cmd = h.cmd();
    cmd.arg("server")
        .arg("--no-tui")
        .env("LLMUX_SHUTDOWN_DRAIN_DEADLINE_SECS", "1")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn llmux server");
    let stderr = child.stderr.take().expect("child stderr");
    // Drain stderr on a thread so the child can never block on a full pipe;
    // collect it for the final assertion.
    let stderr_lines = std::thread::spawn(move || {
        let mut out = Vec::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            out.push(line);
        }
        out
    });

    // Wait for the listener.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut hang = loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            break stream;
        }
        assert!(Instant::now() < deadline, "server never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    };
    // A request that never finishes: declares a 100-byte body, sends 7.
    hang.write_all(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\n\r\npartial")
        .expect("send partial request");

    // Ask for shutdown through the control plane, like `llmux stop` does.
    let mut ctl = TcpStream::connect(("127.0.0.1", port)).expect("control connect");
    // Bounded: a regression in the shutdown reply must fail this test, not
    // hang the suite on an unbounded read.
    ctl.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("control read timeout");
    ctl.write_all(
        b"POST /llmux/shutdown HTTP/1.1\r\nHost: x\r\nx-api-key: testkey\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .expect("send shutdown");
    let mut reply = String::new();
    let _ = ctl.read_to_string(&mut reply);
    if !reply.starts_with("HTTP/1.1 200") {
        let _ = child.kill();
        panic!("shutdown reply: {reply:?}");
    }
    let requested_at = Instant::now();

    // The PROCESS must exit while `hang` is still open — the deadline is 1s,
    // the settle tail is bounded, so 15s is generous.
    let exit_deadline = requested_at + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= exit_deadline {
            let _ = child.kill();
            let _ = child.wait();
            let joined = stderr_lines.join().expect("stderr thread").join("\n");
            panic!("server still alive 15s after shutdown with a hung connection open\n{joined}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let lived = requested_at.elapsed();
    drop(hang);
    let lines = stderr_lines.join().expect("stderr thread");
    let joined = lines.join("\n");
    assert!(status.success(), "exit status {status:?}\n{joined}");
    assert!(
        lived >= Duration::from_millis(900),
        "exited before the deadline could have fired: {lived:?}\n{joined}"
    );
    assert!(
        joined.contains("shutdown drain deadline reached; exiting with connections still open"),
        "{joined}"
    );
    assert!(
        joined.contains("background loops stopping; draining in-flight connections (deadline 1s)"),
        "{joined}"
    );
}

/// Wall-clock seconds since the epoch (for building future token expiries).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Codex subcommand configuration replaces clap's parent config list. All
/// provider settings must be inside exec, before user overrides.
#[test]
fn run_codex_scopes_provider_to_exec_and_propagates_exit() {
    let h = Harness::new();
    h.seed_config(
        r#"{"version":1,"remote":{"host":"127.0.0.1","port":45678,"api_key":"remote-key"},"accounts":[]}"#,
    );
    let bindir = h.dir.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let stub = bindir.join("codex");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'KEY=%s\\n' \"$LLMUX_CODEX_API_KEY\"\nexit 7\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = h
        .cmd()
        .args([
            "run",
            "--codex",
            "--no-model-picker",
            "--",
            "exec",
            "--ignore-user-config",
            "-c",
            "model_reasoning_effort=low",
            "hello",
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bindir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("OPENAI_API_KEY", "unrelated-key")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(
        out.starts_with("exec\n-c\nmodel_provider=\"llmux\""),
        "{out}"
    );
    assert!(
        out.contains("base_url=\"http://127.0.0.1:45678/v1\""),
        "{out}"
    );
    assert!(out.contains("KEY=remote-key"));
    assert!(!out.contains("unrelated-key"));
    assert!(out.contains("--ignore-user-config\n-c\nmodel_reasoning_effort=low\nhello"));
}

// Native first-run ordering: read-only handoff cannot create a config/key;
// starting the daemon initializes its admin key before the next handoff.
#[test]
fn islands_connection_cold_start_reads_only_then_authenticates_dashboard() {
    use std::io::Read as _;
    use std::net::TcpStream;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let h = Harness::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let handoff = || {
        h.cmd()
            .args(["islands-connection", "--port", &port.to_string()])
            .output()
            .unwrap()
    };
    assert!(!handoff().status.success());
    assert!(
        !h.config_path.exists(),
        "handoff must not initialize configuration"
    );
    h.seed_config(&format!(r#"{{"version":1,"proxy":{{"port":{port},"idle_probe":{{"enabled":false}}}},"accounts":[]}}"#));
    assert!(!handoff().status.success(), "missing key must fail closed");
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = ChildGuard(
        h.cmd()
            .args(["server", "--no-tui"])
            .env("HOME", h.dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "isolated cold-start daemon did not bind"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let before = std::fs::read(&h.config_path).unwrap();
    let output = handoff();
    assert!(output.status.success(), "handoff failed after bootstrap");
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt.as_object().unwrap().len(), 2);
    assert_eq!(receipt["endpoint"], format!("http://127.0.0.1:{port}"));
    let key = receipt["api_key"].as_str().expect("control key");
    assert!(!key.is_empty());
    assert_eq!(std::fs::read(&h.config_path).unwrap(), before);
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(socket, "GET /llmux/dashboard HTTP/1.1\r\nHost: localhost\r\nx-api-key: {key}\r\nConnection: close\r\n\r\n").unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    assert!(
        reply.starts_with("HTTP/1.1 200"),
        "authenticated empty dashboard must succeed"
    );
}
