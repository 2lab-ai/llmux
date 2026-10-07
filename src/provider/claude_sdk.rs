//! Claude Agent SDK transport for the OpenAI frontend. The official SDK owns
//! Claude inference; client tool execution stays in Codex. See bridge/README.md.
use std::{path::PathBuf, process::Stdio, sync::OnceLock, time::Duration};

use bytes::Bytes;
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{mpsc, Mutex},
};
use tokio_stream::wrappers::ReceiverStream;

use crate::config::AccountCredential;

const SCRIPT: &str = include_str!("../../bridge/claude-agent.mjs");
const PACKAGE: &str = include_str!("../../bridge/package.json");
const LOCK: &str = include_str!("../../bridge/package-lock.json");
const SDK_VERSION: &str = "0.3.292";
static INSTALL: OnceLock<Mutex<()>> = OnceLock::new();

/// Kill the entire SDK process group when the HTTP body is abandoned, including
/// the SDK's native Claude child. Merely dropping Node leaves that child alive.
struct ProcessGuard {
    child: Child,
    directory: Option<PathBuf>,
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // This process created the isolated group with process_group(0).
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        if let Some(path) = &self.directory {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for key in [
        "PATH",
        "LANG",
        "LC_ALL",
        "SYSTEMROOT",
        "WINDIR",
        "TMPDIR",
        "TMP",
        "TEMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command
}

fn cache_path() -> Result<PathBuf, String> {
    dirs::cache_dir()
        .map(|p| {
            p.join("llmux")
                .join(format!("claude-agent-sdk-{SDK_VERSION}"))
        })
        .ok_or_else(|| "Cannot locate Claude Agent SDK cache directory".into())
}

/// Lazy, serialized provisioning on the daemon host. No account credentials are
/// passed to npm. A failed install never becomes the active SDK directory.
pub async fn ensure_installed() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("LLMUX_CLAUDE_SDK_DIR") {
        let path = PathBuf::from(path);
        let manifest =
            tokio::fs::read(path.join("node_modules/@anthropic-ai/claude-agent-sdk/package.json"))
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if path
            .join("node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs")
            .is_file()
            && manifest
                .as_ref()
                .and_then(|m| m.get("version"))
                .and_then(|v| v.as_str())
                == Some(SDK_VERSION)
        {
            return tokio::fs::canonicalize(path)
                .await
                .map_err(|_| "Cannot resolve LLMUX_CLAUDE_SDK_DIR".into());
        }
        return Err("LLMUX_CLAUDE_SDK_DIR must contain the pinned bridge node_modules; run npm ci using bridge/package-lock.json".into());
    }
    let _lock = INSTALL.get_or_init(|| Mutex::new(())).lock().await;
    let target = cache_path()?;
    if target.join(".llmux-ready").is_file() {
        return Ok(target);
    }
    let parent = target.parent().ok_or("Invalid SDK cache path")?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| "Cannot create Claude Agent SDK cache directory")?;
    let staging = parent.join(format!(".claude-sdk-install-{}", ulid::Ulid::new()));
    tokio::fs::create_dir(&staging)
        .await
        .map_err(|_| "Cannot create SDK installation directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Cannot secure SDK installation directory")?;
    }
    tokio::fs::write(staging.join("package.json"), PACKAGE)
        .await
        .map_err(|_| "Cannot stage SDK package manifest")?;
    tokio::fs::write(staging.join("package-lock.json"), LOCK)
        .await
        .map_err(|_| "Cannot stage SDK lockfile")?;
    let mut npm = command("npm");
    // npm's cache and user configuration are isolated from the user's scripts,
    // registry credentials, npmrc hooks, and global prefix.
    npm.current_dir(&staging)
        .env("HOME", &staging)
        .env("npm_config_cache", staging.join(".npm-cache"))
        .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = match npm.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err("Claude Agent SDK requires Node.js 18+ and npm on the llmux daemon host; install them and retry".into());
        }
    };
    let mut guard = ProcessGuard {
        child,
        directory: Some(staging.clone()),
    };
    let status = tokio::time::timeout(Duration::from_secs(300), guard.child.wait())
        .await
        .map_err(|_| {
            "Claude Agent SDK installation timed out; check npm registry access and retry"
        })?
        .map_err(|_| "Cannot wait for Claude Agent SDK installation")?;
    if !status.success() {
        return Err("Claude Agent SDK installation failed; check Node.js 18+, npm registry access and retry".into());
    }
    tokio::fs::write(staging.join(".llmux-ready"), SDK_VERSION)
        .await
        .map_err(|_| "Cannot finalize SDK installation")?;
    match tokio::fs::rename(&staging, &target).await {
        Ok(()) => guard.directory = None,
        Err(_) if target.join(".llmux-ready").is_file() => {}
        Err(_) => return Err("Cannot activate Claude Agent SDK installation".into()),
    }
    Ok(target)
}

/// A bridge response carries account rejection separately from its HTTP status.
/// Generic upstream 403/402 responses are not sufficient evidence to bench an
/// account; these codes come only from the isolated, pinned SDK transport.
pub struct SdkResponse {
    pub response: reqwest::Response,
    pub account_rejected: bool,
}

fn account_rejected(header: &serde_json::Value, status: http::StatusCode) -> bool {
    matches!(
        (
            header.get("error_code").and_then(|v| v.as_str()),
            status.as_u16()
        ),
        (
            Some("oauth_org_not_allowed" | "account_on_hold" | "verification_required"),
            403
        ) | (Some("billing_error"), 402)
    )
}

/// The response fits the shared Anthropic relay/taxonomy, including pre-stream
/// auth/rate-limit status. Credentials cross only the child's stdin boundary.
pub async fn send(
    body: &[u8],
    credential: &AccountCredential,
    upstream: &str,
) -> Result<SdkResponse, String> {
    let credential = match credential {
        AccountCredential::Oauth { access_token, .. } => {
            json!({"type":"oauth", "token":access_token})
        }
        AccountCredential::Apikey { api_key } => json!({"type":"apikey", "token":api_key}),
        _ => return Err("Claude Agent SDK requires a Claude OAuth or API-key account".into()),
    };
    let body: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "Invalid Claude SDK request JSON")?;
    let sdk = ensure_installed().await?;
    let directory = std::env::temp_dir().join(format!("llmux-agent-{}", ulid::Ulid::new()));
    tokio::fs::create_dir(&directory)
        .await
        .map_err(|_| "Cannot create isolated Claude SDK directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Cannot secure Claude SDK directory")?;
    }
    let mut node = command(std::env::var_os("LLMUX_NODE").unwrap_or_else(|| "node".into()));
    node.current_dir(&directory)
        .env("HOME", &directory)
        .args(["--input-type=module", "--eval"])
        .arg(format!("{SCRIPT}\nawait main();"))
        .arg(&sdk)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = match node.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = tokio::fs::remove_dir_all(&directory).await;
            return Err("Claude Agent SDK requires Node.js 18+ on the llmux daemon host; install Node or set LLMUX_NODE".into());
        }
    };
    let mut guard = ProcessGuard {
        child,
        directory: Some(directory.clone()),
    };
    let mut stdin = guard
        .child
        .stdin
        .take()
        .ok_or("Cannot open Claude SDK stdin")?;
    let payload = serde_json::to_vec(
        &json!({"body":body,"credential":credential,"upstream":upstream,"directory":directory}),
    )
    .map_err(|_| "Cannot encode Claude SDK request")?;
    stdin
        .write_all(&payload)
        .await
        .map_err(|_| "Cannot send request to Claude Agent SDK")?;
    drop(stdin);
    let stdout = guard
        .child
        .stdout
        .take()
        .ok_or("Cannot open Claude SDK stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut header = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(120),
        (&mut reader).take(8192).read_until(b'\n', &mut header),
    )
    .await
    .map_err(|_| "Claude Agent SDK timed out before starting its response")?
    .map_err(|_| "Cannot read Claude Agent SDK response")?;
    let header: serde_json::Value = serde_json::from_slice(&header).map_err(|_| {
        "Claude Agent SDK exited before a valid response; verify the pinned SDK installation"
    })?;
    let status = header
        .get("status")
        .and_then(|s| s.as_u64())
        .and_then(|s| u16::try_from(s).ok())
        .and_then(|s| http::StatusCode::from_u16(s).ok())
        .ok_or("Invalid Claude SDK response status")?;
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        loop {
            let mut chunk = vec![0; 16 * 1024];
            tokio::select! {
                _ = tx.closed() => break,
                result = reader.read(&mut chunk) => match result {
                    Ok(0) => break,
                    Ok(n) => { chunk.truncate(n); if tx.send(Ok(Bytes::from(chunk))).await.is_err() { break; } },
                    Err(_) => { let _ = tx.send(Err(std::io::Error::other("Claude Agent SDK stream failed"))).await; break; }
                }
            }
        }
        drop(guard);
    });
    let response = http::Response::builder()
        .status(status)
        .header(
            http::header::CONTENT_TYPE,
            if status.is_success() {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(reqwest::Body::wrap_stream(ReceiverStream::new(rx)))
        .map_err(|_| "Cannot construct Claude SDK response")?;
    Ok(SdkResponse {
        response: reqwest::Response::from(response),
        account_rejected: account_rejected(&header, status),
    })
}
