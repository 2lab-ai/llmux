//! System clipboard + file export for the raw viewer's action buttons (UI-8).
//!
//! Dependency-free by design: `copy` shells out to the platform clipboard tool
//! (`pbcopy` on macOS; `wl-copy`/`xclip`/`xsel` on Linux, first available) and
//! falls back to the OSC 52 terminal escape when no tool works — that covers
//! SSH sessions where the clipboard lives on the terminal's side. `save`
//! writes under `~/Downloads` (or the temp dir when absent). Everything
//! returns a human label for the modal's flash line; failures return the
//! reason instead of panicking — a copy button must never take the TUI down.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// OSC 52 payload ceiling: terminals cap the escape's length (tmux ~74 KiB by
/// default, others vary). Past this, silently "succeeding" would lie — the
/// caller gets an error naming the real tools instead.
const OSC52_MAX_BYTES: usize = 512 * 1024;

/// Copy `text` to the system clipboard. Returns the destination label
/// (`pbcopy`, `wl-copy`, …, or `OSC52`) for the flash message.
pub(crate) fn copy(text: &str) -> Result<String, String> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["-ib"]),
        ]
    };
    for (tool, args) in candidates {
        match pipe_to(tool, args, text) {
            Ok(()) => return Ok((*tool).to_string()),
            Err(_) => continue, // tool missing/failed — try the next
        }
    }
    osc52(text)
}

/// Pipe `text` into `tool args…` via stdin and require exit 0. The child is
/// ALWAYS reaped — a write failure (e.g. BrokenPipe from a tool that exited
/// early) must not leak a zombie per retry (hotpath review).
fn pipe_to(tool: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{tool}: {err}"))?;
    let write = match child.stdin.as_mut() {
        Some(stdin) => stdin
            .write_all(text.as_bytes())
            .map_err(|err| format!("{tool}: {err}")),
        None => Ok(()),
    };
    // Reap unconditionally (wait() closes our stdin handle first, so the
    // tool sees EOF); only then propagate a write error.
    let status = child.wait().map_err(|err| format!("{tool}: {err}"));
    write?;
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{tool} exited {status}"))
    }
}

/// OSC 52 fallback: hand the payload to the terminal emulator itself
/// (base64, `\x1b]52;c;…\x07`), which owns the clipboard on the machine the
/// user is actually looking at. Written straight to the tty so it bypasses
/// ratatui's buffer (an escape sequence renders nothing).
fn osc52(text: &str) -> Result<String, String> {
    if text.len() > OSC52_MAX_BYTES {
        return Err(format!(
            "no clipboard tool found and {} bytes exceeds the OSC52 escape cap",
            text.len()
        ));
    }
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{payload}\x07");
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .and_then(|mut tty| tty.write_all(seq.as_bytes()))
        .map(|()| "OSC52".to_string())
        .map_err(|err| format!("OSC52 write failed: {err}"))
}

/// Write `contents` to `<downloads>/<stem>-<ts>.<ext>` (timestamped so
/// repeated saves never clobber) and return the written path for the flash
/// message. Falls back to the temp dir when no Downloads dir exists.
pub(crate) fn save(stem: &str, ext: &str, contents: &str) -> Result<String, String> {
    let dir = dirs::download_dir()
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{stem}-{ts}.{ext}"));
    std::fs::write(&path, contents).map_err(|err| format!("save failed: {err}"))?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_writes_a_timestamped_file_and_returns_its_path() {
        let path = save("llmux-clip-test", "txt", "payload").expect("writable temp/downloads");
        let read = std::fs::read_to_string(&path).expect("file exists");
        assert_eq!(read, "payload");
        assert!(path.contains("llmux-clip-test-"));
        assert!(path.ends_with(".txt"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn osc52_refuses_oversized_payloads_instead_of_silently_truncating() {
        let big = "x".repeat(OSC52_MAX_BYTES + 1);
        let err = osc52(&big).expect_err("over the escape cap");
        assert!(err.contains("exceeds"), "{err}");
    }
}
