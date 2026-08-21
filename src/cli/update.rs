//! `llmux update` — self-update on the current release channel.
//!
//! Refreshes the tap, `brew upgrade`s the channel formula (and the Islands
//! cask when installed), then restarts a running daemon if the binary version
//! changed OR the daemon is still serving a different build than the one brew
//! has INSTALLED (never this process's own version — the CLI you invoked may
//! itself be a stale keg), and relaunches the Islands app only if it was
//! running and its cask changed. "already up to date" is a clean success.

use super::brew::{self, Host, RealHost};
use super::status::display_version;
use super::{daemon, CliError, UpdateArgs};

/// Entry point for `llmux update`.
pub async fn run(args: UpdateArgs) -> Result<(), CliError> {
    let UpdateArgs {} = args;
    let host = RealHost;

    let formulae = host.brew_formulae();
    let detected = brew::detect_channel(formulae.as_deref(), host.binary_channel())?;
    let channel = detected.channel;
    if let Some(warning) = &detected.warning {
        eprintln!("warning: {warning}");
    }
    println!("updating on the {} channel…", channel.label());

    // Snapshot the daemon BEFORE the upgrade (the binary on disk changes
    // underneath it, but the process keeps running until we restart). Keep the
    // version it reports, not just "is it up": a daemon that never got its
    // restart is stale even when brew has nothing left to upgrade.
    let config = crate::config::load_or_init()?;
    let port = config.proxy.port;
    let probe = daemon::probe_server(
        &super::proxy_base_url(port),
        config.proxy.api_key.as_deref(),
    )
    .await?;
    let daemon_running = matches!(probe, daemon::ServerProbe::Running { .. });
    // Same document `llmux status` reads; absent field = unknown, not a mismatch.
    let server_version = match &probe {
        daemon::ServerProbe::Running { status } => status["version"].as_str().map(str::to_owned),
        _ => None,
    };

    let report = brew::execute_update(&host, channel)?;

    // Concise before→after report.
    report_line(
        channel.formula(),
        report.formula_before.as_deref(),
        report.formula_after.as_deref(),
    );
    if report.islands_before.is_some() || report.islands_after.is_some() {
        report_line(
            channel.islands_cask(),
            report.islands_before.as_deref(),
            report.islands_after.as_deref(),
        );
    }

    // Restart the daemon if the binary changed, or if it is still serving a
    // different build than the one brew has installed. Everything here goes
    // through the freshly upgraded formula's binary, not current_exe(): brew
    // removed the keg this process may be running from, and (both-installed
    // state) current_exe may even be the OTHER channel's binary — which is
    // also why that artifact, asked for its own `--version`, is the comparand
    // instead of this process. Unresolvable ⇒ `None` ⇒ no mismatch restart;
    // the binary-changed path keeps its own actionable error below.
    let installed_exe = if daemon_running {
        brew::installed_binary(&host, channel).ok()
    } else {
        None
    };
    let installed_version = installed_exe
        .as_deref()
        .and_then(|exe| host.binary_version(exe));

    let restart = restart_reason(
        daemon_running,
        report.formula_changed(),
        server_version.as_deref(),
        installed_version.as_deref(),
    );
    match restart {
        Some(RestartReason::BinaryChanged) => {
            println!("binary changed — restarting the daemon…");
        }
        Some(RestartReason::VersionMismatch) => {
            println!(
                "server on {} ≠ installed {} — restarting the daemon…",
                display_version(server_version.as_deref().unwrap_or("unknown")),
                display_version(installed_version.as_deref().unwrap_or("unknown"))
            );
        }
        None => {}
    }
    if restart.is_some() {
        let exe = match installed_exe {
            Some(exe) => exe,
            // Only reachable on the binary-changed path (a mismatch needs a
            // resolved binary): re-run the lookup for its actionable error.
            None => brew::installed_binary(&host, channel)?,
        };
        daemon::restart(Some(exe)).await?;
    }

    // Relaunch Islands only if it was running AND the cask changed.
    if brew::should_relaunch_islands(report.islands_was_running, report.cask_changed()) {
        println!("Islands cask changed — relaunching the app…");
        host.relaunch_islands();
    }

    if !report.formula_changed() && !report.cask_changed() {
        // Never claim "already up to date" while the server was the stale part.
        if restart == Some(RestartReason::VersionMismatch) {
            println!(
                "binaries up to date; server restarted to apply {}.",
                display_version(installed_version.as_deref().unwrap_or("unknown"))
            );
        } else {
            println!("already up to date.");
        }
    }
    Ok(())
}

/// Why (if at all) this run owes the daemon a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartReason {
    /// `brew upgrade` replaced the binary under a running daemon.
    BinaryChanged,
    /// Nothing was upgraded here, but the daemon runs another build (an
    /// out-of-band `brew upgrade`, or a restart skipped on an earlier run).
    VersionMismatch,
}

/// PURE: pick the restart reason, binary change first — when both hold, the
/// upgrade is the honest explanation and the message says so. Note what is
/// NOT an input: this process's own version. The decision is
/// server-vs-installed only.
fn restart_reason(
    daemon_running: bool,
    formula_changed: bool,
    server_version: Option<&str>,
    installed_version: Option<&str>,
) -> Option<RestartReason> {
    if brew::should_restart_daemon(daemon_running, formula_changed) {
        Some(RestartReason::BinaryChanged)
    } else if brew::should_restart_for_version_mismatch(
        daemon_running,
        server_version,
        installed_version,
    ) {
        Some(RestartReason::VersionMismatch)
    } else {
        None
    }
}

/// "<name>: 0.1.0 → 0.2.0" (or "installed 0.2.0" / "up to date at 0.1.0").
fn report_line(name: &str, before: Option<&str>, after: Option<&str>) {
    match (before, after) {
        (Some(b), Some(a)) if b == a => println!("  {name}: up to date at {a}"),
        (Some(b), Some(a)) => println!("  {name}: {b} → {a}"),
        (None, Some(a)) => println!("  {name}: installed {a}"),
        (Some(b), None) => println!("  {name}: {b} → (removed?)"),
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Three independent builds. CLI_A is what the user typed — it appears
    // only as a server/"stale" value below, never as a comparand, because
    // this process's version is deliberately not a decision input.
    const CLI_A: &str = "llmux 0.2.19 (preview 2026-08-13-0413-aaaaaaa)";
    const INSTALLED_B: &str = "llmux 0.2.19 (preview 2026-08-21-0458-bbbbbbb)";
    const OTHER_C: &str = "llmux 0.2.18 (stable 2026-07-30-1200-ccccccc)";

    #[test]
    fn restart_reason_covers_change_and_mismatch() {
        // The reported bug, shape (a): formula already current, the daemon
        // AND the invoking CLI both on stale A, brew holding B. Comparing
        // server-vs-installed is what makes this fire.
        assert_eq!(
            restart_reason(true, false, Some(CLI_A), Some(INSTALLED_B)),
            Some(RestartReason::VersionMismatch)
        );
        // Shape (b): stale CLI A, but the server already runs the installed
        // B — a client-process comparand would restart here for nothing.
        assert_eq!(
            restart_reason(true, false, Some(INSTALLED_B), Some(INSTALLED_B)),
            None
        );
        // A third build on the server is still a mismatch against installed.
        assert_eq!(
            restart_reason(true, false, Some(OTHER_C), Some(INSTALLED_B)),
            Some(RestartReason::VersionMismatch)
        );
        // An upgrade explains itself even when the versions also differ.
        assert_eq!(
            restart_reason(true, true, Some(CLI_A), Some(INSTALLED_B)),
            Some(RestartReason::BinaryChanged)
        );
        // Missing data on either side never restarts…
        assert_eq!(restart_reason(true, false, None, Some(INSTALLED_B)), None);
        assert_eq!(restart_reason(true, false, Some(CLI_A), None), None);
        // …but an actual upgrade still restarts without a readable version.
        assert_eq!(
            restart_reason(true, true, Some(CLI_A), None),
            Some(RestartReason::BinaryChanged)
        );
        // Nothing running ⇒ nothing to restart, whatever the versions say.
        assert_eq!(
            restart_reason(false, false, Some(CLI_A), Some(INSTALLED_B)),
            None
        );
        assert_eq!(
            restart_reason(false, true, Some(CLI_A), Some(INSTALLED_B)),
            None
        );
    }
}
