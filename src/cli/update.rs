//! `llmux update` — self-update on the current release channel.
//!
//! Refreshes the tap, `brew upgrade`s the channel formula (and the Islands
//! cask when installed), then restarts a running daemon only if the binary
//! version actually changed and relaunches the Islands app only if it was
//! running and its cask changed. "already up to date" is a clean success.

use super::brew::{self, Host, RealHost};
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

    // Snapshot whether a daemon is up BEFORE the upgrade (the binary on disk
    // changes underneath it, but the process keeps running until we restart).
    let config = crate::config::load_or_init()?;
    let port = config.proxy.port;
    let daemon_running = matches!(
        daemon::probe_server(port, config.proxy.api_key.as_deref()).await?,
        daemon::ServerProbe::Running { .. }
    );

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

    // Restart the daemon only if it is running AND the binary changed.
    // Spawn the freshly upgraded formula's binary, not current_exe(): brew
    // removed the keg this process may be running from, and (both-installed
    // state) current_exe may even be the OTHER channel's binary.
    if brew::should_restart_daemon(daemon_running, report.formula_changed()) {
        println!("binary changed — restarting the daemon…");
        let exe = host.formula_bin_path(channel.formula()).ok_or_else(|| {
            CliError::Message(format!(
                "could not locate the {formula} binary after the upgrade \
                 (brew --prefix {formula})\n\
                 The daemon was not restarted — once resolved, run: llmux restart",
                formula = channel.formula()
            ))
        })?;
        daemon::restart(Some(exe)).await?;
    }

    // Relaunch Islands only if it was running AND the cask changed.
    if brew::should_relaunch_islands(report.islands_was_running, report.cask_changed()) {
        println!("Islands cask changed — relaunching the app…");
        host.relaunch_islands();
    }

    if !report.formula_changed() && !report.cask_changed() {
        println!("already up to date.");
    }
    Ok(())
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
