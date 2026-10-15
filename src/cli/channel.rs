//! `llmux channel [stable|preview]` — report or switch the release channel.
//!
//! The channel is DERIVED from what brew has installed (see [`super::brew`]);
//! there is no config field. With no argument this prints the current channel;
//! with `stable`/`preview` it performs the switch (brew uninstall old — or only
//! unlink it when other formulae depend on it — then install, or upgrade and
//! relink, new; mirrored onto the Islands cask), then restarts a running
//! daemon and prints old→new versions.

use super::brew::{self, Channel, DetectSource, Host, RealHost};
use super::status::display_version;
use super::{daemon, ChannelArgs, ChannelName, CliError};

/// Entry point for `llmux channel [stable|preview]`.
pub async fn run(args: ChannelArgs) -> Result<(), CliError> {
    let host = RealHost;
    match args.channel {
        None => {
            print_current(&host)?;
            Ok(())
        }
        Some(target) => switch(&host, channel_of(target)).await,
    }
}

fn channel_of(name: ChannelName) -> Channel {
    match name {
        ChannelName::Stable => Channel::Stable,
        ChannelName::Preview => Channel::Preview,
    }
}

/// `llmux channel` — detect and print the current channel.
fn print_current(host: &dyn Host) -> Result<(), CliError> {
    let detected = detect(host)?;
    if let Some(warning) = &detected.warning {
        eprintln!("warning: {warning}");
    }
    let via = match detected.source {
        DetectSource::Brew => "brew",
        DetectSource::BinaryFallback => "build marker (brew unavailable)",
    };
    println!("channel: {} (via {via})", detected.channel.label());
    Ok(())
}

/// Shared detection: brew formulae are authoritative (the linked one when
/// both are installed), the build marker is the fallback.
fn detect(host: &dyn Host) -> Result<brew::Detected, CliError> {
    let formulae = host.brew_formulae();
    let linked = host.linked_formulae();
    brew::detect_channel(
        formulae.as_deref(),
        linked.as_deref(),
        host.binary_channel(),
    )
}

/// `llmux channel <target>` — switch NOW.
async fn switch(host: &dyn Host, target: Channel) -> Result<(), CliError> {
    let current = detect(host)?.channel;
    let islands = brew::installed_islands_channel(host.brew_casks().as_deref());
    if current == target {
        // Already on it, but the machine may not be converged: a stray
        // other-channel formula (consolidate it, mirroring a stray-channel
        // Islands cask too), or #202's aftermath — `bin/llmux` not the
        // target's binary (relink it). A stray kept for its dependents and
        // already unlinked is converged: nothing runs.
        let (action, cmds) = brew::plan_same_channel(host, target, islands)?;
        // A mutating action snapshots the daemon BEFORE brew runs (as
        // `llmux update` does), so a probe failure aborts with nothing changed.
        let snapshot = if action.mutates() {
            Some(snapshot_daemon().await?)
        } else {
            None
        };
        println!("{}", action.headline(target));
        brew::run_plan(host, &cmds)?;
        if let Some(outcome) = action.outcome(target) {
            println!("{outcome}");
        }
        if let Some(snapshot) = snapshot {
            reconcile_daemon(host, target, &action, &snapshot).await?;
        }
        return Ok(());
    }

    // Is a daemon up on the configured port? (Decides the post-switch
    // restart; a foreign/absent listener means no restart.)
    let daemon_running = snapshot_daemon().await?.running;

    println!(
        "switching channel: {} → {}…",
        current.label(),
        target.label()
    );
    let report = brew::execute_switch(host, current, target, islands, daemon_running)?;
    if let Some(kept) = &report.kept {
        println!("{}", kept.summary());
    }

    // Restart the daemon so the freshly installed binary is the one serving.
    // NEVER current_exe(): this process runs from the keg the switch just
    // uninstalled — resolve the target formula's binary via brew instead
    // (and fail here, daemon untouched, if it can't be found).
    if report.daemon_running {
        println!("restarting the daemon on the new binary…");
        let exe = brew::installed_binary(host, target)?;
        daemon::restart(Some(exe)).await?;
    }

    let old = report.old_version.as_deref().unwrap_or("(unknown)");
    let new = report.new_version.as_deref().unwrap_or("(unknown)");
    println!(
        "switched to {} channel: {} {old} → {} {new}",
        target.label(),
        current.formula(),
        target.formula()
    );
    Ok(())
}

/// The daemon on the configured port: is one of ours up, and which build
/// does it report? A foreign/absent listener = not running.
struct DaemonSnapshot {
    running: bool,
    version: Option<String>,
}

async fn snapshot_daemon() -> Result<DaemonSnapshot, CliError> {
    let config = crate::config::load_or_init()?;
    let probe = daemon::probe_server(
        &super::proxy_base_url(config.proxy.port),
        config.proxy.api_key.as_deref(),
    )
    .await?;
    Ok(match probe {
        daemon::ServerProbe::Running { status } => DaemonSnapshot {
            running: true,
            // Same document `llmux status` reads; absent = unknown.
            version: status["version"].as_str().map(str::to_owned),
        },
        _ => DaemonSnapshot {
            running: false,
            version: None,
        },
    })
}

/// After a MUTATING same-channel action: a running daemon serving another
/// build than the target's installed binary restarts onto it, through
/// `installed_binary` (never `current_exe()`) — `llmux update`'s
/// server-vs-installed rule. An unresolvable binary or version is missing
/// data: no restart.
async fn reconcile_daemon(
    host: &dyn Host,
    target: Channel,
    action: &brew::SameChannel,
    snapshot: &DaemonSnapshot,
) -> Result<(), CliError> {
    let installed_exe = if snapshot.running {
        brew::installed_binary(host, target).ok()
    } else {
        None
    };
    let installed_version = installed_exe
        .as_deref()
        .and_then(|exe| host.binary_version(exe));
    let restart = brew::should_reconcile_daemon(
        action,
        snapshot.running,
        snapshot.version.as_deref(),
        installed_version.as_deref(),
    );
    if let (true, Some(exe)) = (restart, installed_exe) {
        println!(
            "server on {} ≠ installed {} — restarting the daemon…",
            display_version(snapshot.version.as_deref().unwrap_or("unknown")),
            display_version(installed_version.as_deref().unwrap_or("unknown"))
        );
        daemon::restart(Some(exe)).await?;
    }
    Ok(())
}
