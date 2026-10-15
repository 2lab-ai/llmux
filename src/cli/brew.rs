//! Shared machinery for `llmux channel` and `llmux update`: the release
//! channel model, the injectable host abstraction over `brew`/process calls,
//! and the PURE decision logic (channel detection, brew command plans,
//! restart/relaunch conditions).
//!
//! Distribution facts baked in here (single source of truth):
//! - the `llmux` binary ships as brew formula `llmux` (stable) /
//!   `llmux-preview` (preview) — each links a `llmux` executable, so at most
//!   one may be LINKED at a time. The outgoing one is uninstalled, except
//!   when another installed formula depends on it (somawork-cli → `llmux`):
//!   then it stays installed but unlinked, since brew refuses the uninstall;
//! - the menu-bar app ships as cask `llmux-islands` (stable) /
//!   `llmux-islands-preview`.
//!
//! The channel is DERIVED from what brew has installed (and, with both
//! formulae installed, which one's binary `bin/llmux` is) — there is no
//! config field. All side effects (running `brew`, reading versions, poking the
//! Islands process) sit behind the [`Host`] trait, mirroring the repo idiom
//! (`Prober` in `scheduler::idle_probe`, `UsageFetcher` in `scheduler::usage`)
//! so every decision is unit-tested against a scripted [`FakeHost`] with no
//! network, no brew, and no processes touched.

use super::CliError;

/// A brew-managed release channel. `dev` (a local build not installed via
/// brew) is deliberately NOT a variant — it is the absence of a channel and is
/// surfaced as an error by [`detect_channel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Preview,
}

impl Channel {
    /// The brew formula that installs the `llmux` binary on this channel.
    pub fn formula(self) -> &'static str {
        match self {
            Channel::Stable => "llmux",
            Channel::Preview => "llmux-preview",
        }
    }

    /// The brew cask that installs the menu-bar app on this channel.
    pub fn islands_cask(self) -> &'static str {
        match self {
            Channel::Stable => "llmux-islands",
            Channel::Preview => "llmux-islands-preview",
        }
    }

    /// Lowercase label for user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Preview => "preview",
        }
    }

    /// The other channel — the one to uninstall when switching to `self`.
    pub fn other(self) -> Channel {
        match self {
            Channel::Stable => Channel::Preview,
            Channel::Preview => Channel::Stable,
        }
    }
}

/// A single `brew` invocation (the args after `brew`), streamed to the user.
/// Kept as owned strings so the plans are plain data the tests assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewCmd(pub Vec<String>);

impl BrewCmd {
    fn new<const N: usize>(args: [&str; N]) -> Self {
        BrewCmd(args.iter().map(|s| (*s).to_string()).collect())
    }

    /// The args as `&str` slices, for `Host::run_brew`.
    pub fn args(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }
}

/// Where a detected channel came from — brew (authoritative) or the binary's
/// own build marker (fallback when brew is unavailable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectSource {
    Brew,
    BinaryFallback,
}

/// Outcome of channel detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub channel: Channel,
    pub source: DetectSource,
    /// Non-fatal advisory (e.g. both formulae installed and the link state
    /// can't tell which one is in use — preview wins).
    pub warning: Option<String>,
}

/// Injectable host: every side effect `channel`/`update` need. The production
/// [`RealHost`] shells out with `std::process::Command`; [`FakeHost`] (tests)
/// scripts the answers and records the mutating calls.
pub trait Host {
    /// Names from `brew list --formula`, or `None` when brew is unavailable
    /// (not installed, or the command errored).
    fn brew_formulae(&self) -> Option<Vec<String>>;
    /// Names from `brew list --cask`, or `None` when brew is unavailable.
    fn brew_casks(&self) -> Option<Vec<String>>;
    /// The running binary's channel from its build marker — the fallback when
    /// brew can't answer. `None` for a `dev` build.
    fn binary_channel(&self) -> Option<Channel>;
    /// Installed version string of a formula/cask (e.g. `"0.1.0"`), or `None`
    /// when it is not installed. For a formula with several kegs this is the
    /// keg in use (the one its opt link points to), not the first listed.
    fn installed_version(&self, name: &str, cask: bool) -> Option<String>;
    /// Raw `brew uses --installed <formula>` names (tap-prefixed, transitive,
    /// casks included), or `None` when brew can't answer. Empty = nothing
    /// installed depends on it.
    fn installed_dependents(&self, formula: &str) -> Option<Vec<String>>;
    /// The llmux formula brew has LINKED as `<brew --prefix>/bin/llmux`: its
    /// `var/homebrew/linked/<formula>` record resolves to a keg directory
    /// (Homebrew's `Keg#linked?`) AND `bin/llmux` resolves inside that keg
    /// (a surviving record with `bin/llmux` gone is #202's "Already linked"
    /// trap, not a link). `None` when brew can't answer or a record can't be
    /// read — only "not found" counts as absent. Decides the channel when
    /// both formulae are installed.
    fn linked_formulae(&self) -> Option<Vec<String>>;
    /// The llmux formulae brew has PINNED: those with a
    /// `<brew --prefix>/var/homebrew/pinned/<formula>` symlink (Homebrew's
    /// `FormulaPin#pinned?`). `None` when brew can't answer or a record
    /// can't be read — only "not found" counts as unpinned.
    fn pinned_formulae(&self) -> Option<Vec<String>>;
    /// Run `brew <args>`, streaming stdio to the user. Errs on non-zero exit.
    fn run_brew(&self, args: &[&str]) -> Result<(), CliError>;
    /// Absolute path to a formula's installed `llmux` binary (via
    /// `brew --prefix <formula>`), verified to exist on disk. `None` when the
    /// formula is not installed or brew can't answer. This is the binary a
    /// post-update/switch daemon restart must spawn — `current_exe()` may be
    /// the keg brew just deleted.
    fn formula_bin_path(&self, formula: &str) -> Option<std::path::PathBuf>;
    /// Full version line of a binary on disk (`<exe> --version`, trimmed) —
    /// byte-identical to what that build reports as `version` in
    /// `/llmux/status`, so the two are directly comparable. `None` when the
    /// binary cannot be run or prints nothing usable. Asking the ARTIFACT is
    /// the point: this process's own `build_info` describes whichever keg the
    /// user happened to invoke, which may be stale or the other channel's.
    fn binary_version(&self, exe: &std::path::Path) -> Option<String>;
    /// Is the Islands menu-bar app currently running?
    fn islands_running(&self) -> bool;
    /// Best-effort relaunch of the Islands app (quit + reopen).
    fn relaunch_islands(&self);
}

/// PURE: derive the current channel. Brew is authoritative — if it lists a
/// llmux formula that wins. Both installed (a switch keeps the outgoing
/// formula, unlinked, when other formulae depend on it) → the `linked` one
/// ([`Host::linked_formulae`]: its binary is `bin/llmux`) wins. Only when
/// the link state can't
/// settle it (both or neither linked, or unknown) does preview win, with a
/// warning. Only when brew lists neither formula (or is unavailable) do we
/// fall back to the binary's own build marker. A `dev` build with no brew
/// formula is an error.
pub fn detect_channel(
    formulae: Option<&[String]>,
    linked: Option<&[String]>,
    binary: Option<Channel>,
) -> Result<Detected, CliError> {
    if let Some(list) = formulae {
        let has_preview = list.iter().any(|f| f == Channel::Preview.formula());
        let has_stable = list.iter().any(|f| f == Channel::Stable.formula());
        match (has_stable, has_preview) {
            (true, true) => {
                if let Some(channel) = linked.and_then(sole_linked_channel) {
                    return Ok(Detected {
                        channel,
                        source: DetectSource::Brew,
                        warning: None,
                    });
                }
                return Ok(Detected {
                    channel: Channel::Preview,
                    source: DetectSource::Brew,
                    warning: Some(
                        "both llmux and llmux-preview are installed via brew; \
                         preview wins — `llmux channel preview` keeps preview \
                         (removes llmux, or unlinks it when other formulae need \
                         it), `llmux channel stable` moves to stable (removes \
                         llmux-preview, or unlinks it when other formulae need it)"
                            .into(),
                    ),
                });
            }
            (false, true) => {
                return Ok(Detected {
                    channel: Channel::Preview,
                    source: DetectSource::Brew,
                    warning: None,
                })
            }
            (true, false) => {
                return Ok(Detected {
                    channel: Channel::Stable,
                    source: DetectSource::Brew,
                    warning: None,
                })
            }
            // brew answered but neither formula is present — fall through to
            // the binary marker (e.g. a locally built / hot-deployed binary).
            (false, false) => {}
        }
    }
    match binary {
        Some(channel) => Ok(Detected {
            channel,
            source: DetectSource::BinaryFallback,
            warning: None,
        }),
        None => Err(CliError::Message(
            "cannot determine release channel: neither `llmux` nor \
             `llmux-preview` is installed via brew, and this is a local (dev) \
             build with no channel marker"
                .into(),
        )),
    }
}

/// PURE: the channel whose formula is the ONLY llmux formula in `linked`;
/// `None` when both or neither are linked.
fn sole_linked_channel(linked: &[String]) -> Option<Channel> {
    let is_linked = |channel: Channel| linked.iter().any(|f| f == channel.formula());
    match (is_linked(Channel::Stable), is_linked(Channel::Preview)) {
        (true, false) => Some(Channel::Stable),
        (false, true) => Some(Channel::Preview),
        _ => None,
    }
}

/// PURE: the installed dependents that keep a formula from being
/// uninstalled, as short names. `brew uses --installed` output is
/// tap-prefixed (`2lab-ai/tap/somawork-cli`), transitive and includes casks;
/// the two Islands casks are dropped because every plan uninstalls the
/// outgoing-channel cask BEFORE it touches that cask's formula.
pub fn blocking_dependents(dependents: &[String]) -> Vec<String> {
    let islands = [
        Channel::Stable.islands_cask(),
        Channel::Preview.islands_cask(),
    ];
    dependents
        .iter()
        .filter_map(|name| name.trim().rsplit('/').next())
        .filter(|name| !name.is_empty() && !islands.contains(name))
        .map(str::to_string)
        .collect()
}

/// PURE: how a plan retires the outgoing/stray formula. `uninstall --force`
/// removes EVERY keg (a plain uninstall drops only the current one, leaving
/// the rack in `brew list --formula`, so `detect_channel` keeps seeing both
/// formulae). With a blocking dependent brew refuses any uninstall, so the
/// formula is only unlinked: off `bin/llmux`, still installed for its users.
fn retire_formula(formula: &str, dependents: &[String]) -> BrewCmd {
    if blocking_dependents(dependents).is_empty() {
        BrewCmd::new(["uninstall", "--force", formula])
    } else {
        BrewCmd::new(["unlink", formula])
    }
}

/// PURE: restore an installed formula's `bin/llmux` symlink. A bare `brew
/// link` of a keg whose LinkedKegs record survived (the outgoing uninstall
/// removed the shared symlink, not the target's record) answers "Already
/// linked" with exit 0 and does nothing; `unlink` clears the record (and is
/// a harmless no-op on an unlinked keg), so the `link` that follows is real.
fn relink_formula(formula: &str) -> [BrewCmd; 2] {
    [
        BrewCmd::new(["unlink", formula]),
        BrewCmd::new(["link", "--overwrite", formula]),
    ]
}

/// PURE: the ordered `brew` commands to switch from `current` to `target` —
/// the target made ready FIRST, so a failure leaves the old channel whole:
///
/// 1. `update`;
/// 2. the target: `install` it, or `upgrade` it when ALREADY installed (the
///    both-installed state `detect_channel` warns about, where `brew
///    install` would no-op on a possibly stale keg; exit 0 when already
///    current). Nothing is unlinked first: a failure here — download/build,
///    before brew links anything — leaves the outgoing channel untouched;
/// 3. the outgoing cask — it DEPENDS on the outgoing formula, and brew
///    refuses to uninstall a formula something depends on (live-verified:
///    "required by llmux-islands");
/// 4. the outgoing formula, retired by [`retire_formula`]: uninstalled, or
///    only unlinked when `outgoing_dependents` (its `brew uses --installed`
///    answer, queried before anything ran) names a formula that needs it;
/// 5. `unlink` + `link --overwrite` the target ([`relink_formula`]): step 2
///    usually linked it over `bin/llmux` already (llmux is not keg-only, so
///    install AND upgrade link the new keg; both formulae declare
///    `link_overwrite "bin/llmux"`) — this makes the end state certain;
/// 6. the target cask LAST — its formula is in place, and the two casks
///    conflict, so it follows step 3.
///
/// The cask is mirrored only when one is installed and not already on the
/// target channel. Assumes `current != target` (the no-op case is handled
/// by the command, not the plan).
pub fn switch_plan(
    current: Channel,
    target: Channel,
    islands: Option<Channel>,
    target_installed: bool,
    outgoing_dependents: &[String],
) -> Vec<BrewCmd> {
    let mirror_islands = islands.is_some_and(|installed| installed != target);
    let mut cmds = vec![BrewCmd::new(["update"])];
    // Target first: a failure here leaves the outgoing channel whole.
    if target_installed {
        cmds.push(BrewCmd::new(["upgrade", target.formula()]));
    } else {
        cmds.push(BrewCmd::new(["install", target.formula()]));
    }
    // Outgoing cask before the outgoing formula it depends on.
    if mirror_islands {
        if let Some(installed) = islands {
            cmds.push(BrewCmd::new([
                "uninstall",
                "--cask",
                installed.islands_cask(),
            ]));
        }
    }
    cmds.push(retire_formula(current.formula(), outgoing_dependents));
    cmds.extend(relink_formula(target.formula()));
    // Target cask last — its formula dependency is in place now.
    if mirror_islands {
        cmds.push(BrewCmd::new(["install", "--cask", target.islands_cask()]));
    }
    cmds
}

/// PURE: the `brew` commands to consolidate the both-installed state when the
/// user re-selects the channel they are already on. `detect_channel` makes
/// preview win whenever both formulae are installed, so `llmux channel
/// preview` would otherwise no-op forever while the "both installed" warning
/// keeps firing on every command. Same dependency ordering as [`switch_plan`]:
/// a stray-channel Islands cask blocks `brew uninstall` of the stray formula,
/// so it is removed first and the target-channel cask installed last. Then
/// retire the stray formula — `uninstall --force`, or only `unlink` when
/// `stray_dependents` (its `brew uses --installed` answer) names a formula
/// that needs it — and relink the target via `unlink` + `link --overwrite`
/// (the stray may have owned the shared `bin/llmux` symlink).
pub fn consolidate_plan(
    target: Channel,
    islands: Option<Channel>,
    stray_dependents: &[String],
) -> Vec<BrewCmd> {
    let stray = target.other();
    let mirror_islands = islands == Some(stray);
    let mut cmds = Vec::new();
    if mirror_islands {
        cmds.push(BrewCmd::new(["uninstall", "--cask", stray.islands_cask()]));
    }
    cmds.push(retire_formula(stray.formula(), stray_dependents));
    cmds.extend(relink_formula(target.formula()));
    if mirror_islands {
        cmds.push(BrewCmd::new(["install", "--cask", target.islands_cask()]));
    }
    cmds
}

/// A formula a plan unlinked instead of uninstalling, because other
/// installed formulae depend on it — reported to the user once the plan ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptFormula {
    pub formula: String,
    /// Short names of the blocking dependents ([`blocking_dependents`]).
    pub required_by: Vec<String>,
}

impl KeptFormula {
    /// PURE: from the dependents answer the plan used — `Some` exactly when
    /// [`retire_formula`] chose `unlink`.
    fn from_dependents(formula: &str, dependents: &[String]) -> Option<KeptFormula> {
        let required_by = blocking_dependents(dependents);
        (!required_by.is_empty()).then(|| KeptFormula {
            formula: formula.to_string(),
            required_by,
        })
    }

    /// The one-line, past-tense report printed after the plan completed.
    pub fn summary(&self) -> String {
        format!(
            "kept {} installed (required by {}) — unlinked",
            self.formula,
            self.required_by.join(", ")
        )
    }
}

/// Refuse, BEFORE any mutating command runs, to `brew upgrade` a formula
/// the user pinned: an outdated pinned formula fails the upgrade (Homebrew
/// `cmd/upgrade.rb`), and in a switch that lands after the outgoing formula
/// is gone. Only upgrades care — `install`, `link`, `unlink` and
/// `uninstall --force` ignore pins — so callers ask only about a formula
/// their plan upgrades. Unreadable pin records abort the same way: unknown
/// is not "unpinned".
fn check_not_pinned(host: &dyn Host, formula: &str) -> Result<(), CliError> {
    let pinned = host.pinned_formulae().ok_or_else(|| {
        CliError::Message(format!(
            "could not read brew's pin records before upgrading {formula} \
             — nothing was changed"
        ))
    })?;
    if pinned.iter().any(|p| p == formula) {
        return Err(CliError::Message(format!(
            "{formula} is pinned in brew, so it can't be upgraded — run \
             `brew unpin {formula}`, then retry; nothing was changed"
        )));
    }
    Ok(())
}

/// Ask brew what depends on `formula` BEFORE any mutating command runs. An
/// unanswerable query aborts with the machine untouched: guessing "no
/// dependents" would reproduce #202's mid-plan uninstall refusal (after the
/// outgoing Islands cask is already gone) — a half-switched machine.
fn query_dependents(host: &dyn Host, formula: &str) -> Result<Vec<String>, CliError> {
    host.installed_dependents(formula).ok_or_else(|| {
        CliError::Message(format!(
            "could not list what depends on {formula} \
             (`brew uses --installed {formula}` failed) — nothing was changed"
        ))
    })
}

/// The installed binary a post-update/switch daemon restart must spawn,
/// resolved through brew (never `current_exe()` — that may be the keg brew
/// just deleted). Errs with recovery guidance; callers treat the error as
/// "daemon untouched".
pub fn installed_binary(host: &dyn Host, channel: Channel) -> Result<std::path::PathBuf, CliError> {
    host.formula_bin_path(channel.formula()).ok_or_else(|| {
        CliError::Message(format!(
            "could not locate the {formula} binary (brew --prefix {formula})\n\
             The daemon was not restarted — once resolved, run: llmux restart",
            formula = channel.formula()
        ))
    })
}

/// PURE: the `brew` commands for an in-channel self-update. Always refresh the
/// tap, upgrade the channel formula, and upgrade the Islands cask when one is
/// installed on this channel.
pub fn update_plan(channel: Channel, islands_installed: bool) -> Vec<BrewCmd> {
    let mut cmds = vec![
        BrewCmd::new(["update"]),
        BrewCmd::new(["upgrade", channel.formula()]),
    ];
    if islands_installed {
        cmds.push(BrewCmd::new(["upgrade", "--cask", channel.islands_cask()]));
    }
    cmds
}

/// PURE: restart the daemon only when it is running AND the binary version
/// actually changed (an `already up to date` upgrade must not churn it).
pub fn should_restart_daemon(daemon_running: bool, version_changed: bool) -> bool {
    daemon_running && version_changed
}

/// PURE: restart a running daemon that serves a DIFFERENT build than the
/// binary brew has INSTALLED, even when brew had nothing to upgrade — an
/// out-of-band `brew upgrade` (or a restart skipped on an earlier run) leaves
/// exactly that state, and `update` must converge it instead of reporting
/// "already up to date" over a stale server.
///
/// The comparand is deliberately the installed artifact, never the invoking
/// process's `build_info`: a stale CLI equal to a stale server would hide a
/// real mismatch, and a stale CLI against an already-current server would
/// invent one. Either version unknown = missing data, so no restart.
pub fn should_restart_for_version_mismatch(
    daemon_running: bool,
    server_version: Option<&str>,
    installed_version: Option<&str>,
) -> bool {
    match (server_version, installed_version) {
        (Some(server), Some(installed)) => daemon_running && server != installed,
        _ => false,
    }
}

/// PURE: relaunch the Islands app only when it was running AND its cask
/// version actually changed.
pub fn should_relaunch_islands(was_running: bool, cask_changed: bool) -> bool {
    was_running && cask_changed
}

/// Is an Islands cask (either channel) installed? Derives the channel of the
/// installed one, if any — used to mirror a channel switch onto the cask.
pub fn installed_islands_channel(casks: Option<&[String]>) -> Option<Channel> {
    let casks = casks?;
    // Preview first: `llmux-islands-preview` also ends with `llmux-islands`'s
    // stem, so match the more specific name before the plain one.
    if casks.iter().any(|c| c == Channel::Preview.islands_cask()) {
        Some(Channel::Preview)
    } else if casks.iter().any(|c| c == Channel::Stable.islands_cask()) {
        Some(Channel::Stable)
    } else {
        None
    }
}

/// The current binary's channel from its compile-time build marker. This is
/// exactly what `llmux --version` prints as its `(<channel> <id>)` marker, so
/// it is the same fallback the spec describes without shelling out to self.
pub fn binary_channel_from_build() -> Option<Channel> {
    match crate::build_info::BUILD_CHANNEL {
        "preview" => Some(Channel::Preview),
        "stable" => Some(Channel::Stable),
        _ => None,
    }
}

/// Report of a completed channel switch (for the old→new summary line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchReport {
    pub from: Channel,
    pub to: Channel,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub daemon_running: bool,
    /// The outgoing formula, when other formulae kept it installed.
    pub kept: Option<KeptFormula>,
}

/// Execute a channel switch through `host`: read-only preflight before ANY
/// mutating command — capture the outgoing version, refuse a pinned target
/// when the plan will upgrade it (target already installed), query the
/// outgoing formula's dependents (either failing errs with nothing changed)
/// — then run [`switch_plan`] (target first) in order and capture the
/// incoming version. The daemon restart itself
/// is async and lives in the command handler; this returns whether one is
/// warranted via `daemon_running`.
pub fn execute_switch(
    host: &dyn Host,
    current: Channel,
    target: Channel,
    islands: Option<Channel>,
    daemon_running: bool,
) -> Result<SwitchReport, CliError> {
    let old_version = host.installed_version(current.formula(), false);
    let target_installed = host.installed_version(target.formula(), false).is_some();
    if target_installed {
        // The plan upgrades an installed target.
        check_not_pinned(host, target.formula())?;
    }
    let dependents = query_dependents(host, current.formula())?;
    run_plan(
        host,
        &switch_plan(current, target, islands, target_installed, &dependents),
    )?;
    let new_version = host.installed_version(target.formula(), false);
    Ok(SwitchReport {
        from: current,
        to: target,
        old_version,
        new_version,
        daemon_running,
        kept: KeptFormula::from_dependents(current.formula(), &dependents),
    })
}

/// What `llmux channel <c>` does when `c` is already the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SameChannel {
    /// Only the target formula is installed and `bin/llmux` is its binary
    /// (or the link state is unknown — no reason to touch brew).
    NothingToDo,
    /// Only the target formula is installed but `bin/llmux` is not its
    /// binary — #202's aftermath: `unlink` + `link --overwrite` it.
    Relink,
    /// Both installed, the other formula kept for its dependents, already
    /// unlinked, and `bin/llmux` the target's binary: already converged,
    /// nothing to change.
    KeptUnlinked(KeptFormula),
    /// Both installed and not converged: run [`consolidate_plan`]; `kept`
    /// when the other formula's dependents keep it (then only unlinked).
    Consolidate { kept: Option<KeptFormula> },
}

impl SameChannel {
    /// Does this action change brew state? Only then may the daemon need
    /// reconciling ([`should_reconcile_daemon`]).
    pub fn mutates(&self) -> bool {
        matches!(self, SameChannel::Relink | SameChannel::Consolidate { .. })
    }

    /// The line printed before any command runs — the only line when none
    /// runs.
    pub fn headline(&self, target: Channel) -> String {
        let on = format!("already on the {} channel", target.label());
        match self {
            SameChannel::NothingToDo => format!("{on} — nothing to do"),
            SameChannel::Relink => format!(
                "{on} — relinking {} (`bin/llmux` is not its binary)…",
                target.formula()
            ),
            SameChannel::KeptUnlinked(kept) => format!(
                "{on} — kept {} installed (required by {}), unlinked",
                kept.formula,
                kept.required_by.join(", ")
            ),
            SameChannel::Consolidate { .. } => format!(
                "{on} — consolidating the stray {} formula…",
                target.other().formula()
            ),
        }
    }

    /// The line printed once the commands ran; `None` when the headline
    /// already said everything.
    pub fn outcome(&self, target: Channel) -> Option<String> {
        match self {
            SameChannel::NothingToDo | SameChannel::KeptUnlinked(_) => None,
            SameChannel::Relink => Some(format!(
                "relinked {} — `bin/llmux` is its binary again",
                target.formula()
            )),
            SameChannel::Consolidate { kept: Some(kept) } => Some(kept.summary()),
            SameChannel::Consolidate { kept: None } => {
                Some(format!("removed {}", target.other().formula()))
            }
        }
    }
}

/// PURE: after a same-channel action, restart a running daemon onto the
/// target's installed binary only when the action changed brew state
/// ([`SameChannel::mutates`]) AND the daemon serves a different build —
/// `llmux update`'s server-vs-installed rule
/// ([`should_restart_for_version_mismatch`]). The genuine no-op and the
/// converged no-churn case never touch the daemon.
pub fn should_reconcile_daemon(
    action: &SameChannel,
    daemon_running: bool,
    server_version: Option<&str>,
    installed_version: Option<&str>,
) -> bool {
    action.mutates()
        && should_restart_for_version_mismatch(daemon_running, server_version, installed_version)
}

/// PURE: decide [`SameChannel`] from the read-only preflight: whether the
/// target and the other formula are installed, the other's `brew uses
/// --installed` answer (empty when not queried), the linked formulae
/// (`None` = unknown) and the installed Islands cask's channel. No target
/// keg (channel from the build marker) = nothing to relink or consolidate
/// onto. Converged = the other formula is kept for its dependents, the
/// target is the sole linked formula, and no stray-channel Islands cask is
/// left (only [`consolidate_plan`] mirrors it).
pub fn same_channel_decision(
    target: Channel,
    islands: Option<Channel>,
    target_installed: bool,
    other_installed: bool,
    other_dependents: &[String],
    linked: Option<&[String]>,
) -> SameChannel {
    let other = target.other();
    if !target_installed {
        return SameChannel::NothingToDo;
    }
    if !other_installed {
        let target_linked = linked.map(|names| names.iter().any(|f| f == target.formula()));
        return match target_linked {
            Some(false) => SameChannel::Relink,
            _ => SameChannel::NothingToDo,
        };
    }
    let target_sole_linked = linked.and_then(sole_linked_channel) == Some(target);
    match KeptFormula::from_dependents(other.formula(), other_dependents) {
        Some(kept) if target_sole_linked && islands != Some(other) => {
            SameChannel::KeptUnlinked(kept)
        }
        kept => SameChannel::Consolidate { kept },
    }
}

/// Read-only preflight + [`same_channel_decision`] for `llmux channel <c>`
/// when `c` is already the channel: the other formula's install state, its
/// dependents (queried BEFORE deciding; an unanswerable query errs with
/// nothing changed) and the link state. Returns the decision and its `brew`
/// commands — none for [`SameChannel::NothingToDo`] and
/// [`SameChannel::KeptUnlinked`]. The caller announces, then [`run_plan`]s.
/// No pin check: nothing here upgrades.
pub fn plan_same_channel(
    host: &dyn Host,
    target: Channel,
    islands: Option<Channel>,
) -> Result<(SameChannel, Vec<BrewCmd>), CliError> {
    let other = target.other();
    let target_installed = host.installed_version(target.formula(), false).is_some();
    let other_installed = host.installed_version(other.formula(), false).is_some();
    let dependents = if target_installed && other_installed {
        query_dependents(host, other.formula())?
    } else {
        Vec::new()
    };
    let linked = host.linked_formulae();
    let action = same_channel_decision(
        target,
        islands,
        target_installed,
        other_installed,
        &dependents,
        linked.as_deref(),
    );
    let cmds = match &action {
        SameChannel::NothingToDo | SameChannel::KeptUnlinked(_) => Vec::new(),
        SameChannel::Relink => relink_formula(target.formula()).to_vec(),
        SameChannel::Consolidate { .. } => consolidate_plan(target, islands, &dependents),
    };
    Ok((action, cmds))
}

/// Run planned `brew` commands in order, stopping at the first failure.
pub fn run_plan(host: &dyn Host, cmds: &[BrewCmd]) -> Result<(), CliError> {
    for cmd in cmds {
        host.run_brew(&cmd.args())?;
    }
    Ok(())
}

/// Report of a completed in-channel update (before→after per artifact).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub channel: Channel,
    pub formula_before: Option<String>,
    pub formula_after: Option<String>,
    pub islands_before: Option<String>,
    pub islands_after: Option<String>,
    pub islands_was_running: bool,
}

impl UpdateReport {
    pub fn formula_changed(&self) -> bool {
        self.formula_before != self.formula_after
    }

    pub fn cask_changed(&self) -> bool {
        self.islands_before != self.islands_after
    }
}

/// Execute an in-channel update through `host`: snapshot versions + the
/// Islands run-state, run the planned `brew upgrade`s, snapshot again. The
/// caller decides restart/relaunch from the returned before/after via the
/// pure `should_*` predicates.
pub fn execute_update(host: &dyn Host, channel: Channel) -> Result<UpdateReport, CliError> {
    let islands_installed = host
        .installed_version(channel.islands_cask(), true)
        .is_some();
    let formula_before = host.installed_version(channel.formula(), false);
    let islands_before = host.installed_version(channel.islands_cask(), true);
    let islands_was_running = islands_installed && host.islands_running();

    for cmd in update_plan(channel, islands_installed) {
        host.run_brew(&cmd.args())?;
    }

    Ok(UpdateReport {
        channel,
        formula_before,
        formula_after: host.installed_version(channel.formula(), false),
        islands_before,
        islands_after: host.installed_version(channel.islands_cask(), true),
        islands_was_running,
    })
}

/// The llmux formula whose binary `bin_llmux` (`<prefix>/bin/llmux`) is:
/// its record in `dir` (`<prefix>/var/homebrew/linked`) must resolve to a
/// keg directory (Homebrew's `Keg#linked?`) AND `bin/llmux` must resolve
/// inside that keg. The record alone is not enough: #202 left a surviving
/// record with `bin/llmux` gone, which brew reports as "Already linked". A
/// missing `dir` or `bin/llmux` = nothing linked; any other read error =
/// `None` (unknown), never "not linked".
fn linked_records(dir: &std::path::Path, bin_llmux: &std::path::Path) -> Option<Vec<String>> {
    let binary = match probe(std::fs::canonicalize(bin_llmux)) {
        Probe::Present(binary) => binary,
        Probe::Absent => return Some(Vec::new()),
        Probe::Unknown => return None,
    };
    llmux_formulae_where(|formula| {
        let record = dir.join(formula);
        if !is_symlink_record(&record)? {
            return Some(false);
        }
        // A record whose keg was removed (dangling) is not a link.
        match probe(std::fs::canonicalize(&record)) {
            Probe::Present(keg) => match probe(std::fs::metadata(&keg)) {
                Probe::Present(meta) => Some(meta.is_dir() && binary.starts_with(&keg)),
                Probe::Absent => Some(false),
                Probe::Unknown => None,
            },
            Probe::Absent => Some(false),
            Probe::Unknown => None,
        }
    })
}

/// The llmux formulae with a pin record in `dir`
/// (`<prefix>/var/homebrew/pinned`). Homebrew's `FormulaPin#pinned?` is the
/// symlink's existence alone. A missing `dir` (brew removes it when the last
/// pin goes) = nothing pinned; any other read error = `None` (unknown).
fn pinned_records(dir: &std::path::Path) -> Option<Vec<String>> {
    llmux_formulae_where(|formula| is_symlink_record(&dir.join(formula)))
}

/// Is `path` a symlink? `Some(false)` when it is absent (not found), `None`
/// when it can't be read.
fn is_symlink_record(path: &std::path::Path) -> Option<bool> {
    match probe(std::fs::symlink_metadata(path)) {
        Probe::Present(meta) => Some(meta.file_type().is_symlink()),
        Probe::Absent => Some(false),
        Probe::Unknown => None,
    }
}

/// The llmux formulae `keep` accepts; `None` as soon as `keep` can't tell.
fn llmux_formulae_where(keep: impl Fn(&str) -> Option<bool>) -> Option<Vec<String>> {
    let mut kept = Vec::new();
    for formula in [Channel::Stable, Channel::Preview].map(Channel::formula) {
        if keep(formula)? {
            kept.push(formula.to_string());
        }
    }
    Some(kept)
}

/// A filesystem read, tri-state: "not found" is the only `Absent`; any
/// other error (permission, I/O) is `Unknown`, never silently absent.
#[derive(Debug, PartialEq, Eq)]
enum Probe<T> {
    Present(T),
    Absent,
    Unknown,
}

/// PURE: classify an I/O result as a [`Probe`].
fn probe<T>(result: std::io::Result<T>) -> Probe<T> {
    match result {
        Ok(value) => Probe::Present(value),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Probe::Absent,
        Err(_) => Probe::Unknown,
    }
}

/// PURE: a formula's installed version by precedence — the keg its opt link
/// resolves to (`opt_keg`, already canonicalized) when that is this
/// formula's keg; otherwise the newest keg of `brew list --versions`
/// (`list_versions`, only run when needed); otherwise `None`.
fn formula_version(
    formula: &str,
    opt_keg: Option<&std::path::Path>,
    list_versions: impl FnOnce() -> Option<String>,
) -> Option<String> {
    opt_keg
        .and_then(|keg| keg_version(formula, keg))
        .or_else(|| list_versions().as_deref().and_then(highest_listed_version))
}

/// PURE: the version of the keg a formula's opt link resolves to — the
/// canonical `<Cellar>/<formula>/<version>` path yields `<version>`. `None`
/// unless the parent dir is this formula's rack: a link that resolves
/// anywhere else is not an answer.
fn keg_version(formula: &str, keg: &std::path::Path) -> Option<String> {
    let rack = keg.parent()?.file_name()?;
    if rack != std::ffi::OsStr::new(formula) {
        return None;
    }
    keg.file_name()?.to_str().map(str::to_string)
}

/// PURE: the newest version on a `brew list --versions <formula>` line
/// (`<name> <version>…`, kegs in no meaningful order — the first one listed
/// is NOT the newest).
fn highest_listed_version(listing: &str) -> Option<String> {
    listing
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .max_by(|a, b| compare_versions(a, b))
        .map(str::to_string)
}

/// PURE: version order for brew keg names. Segments split on `.`, `_` (brew
/// revision) and `-`; numeric segments compare as numbers (`0.2.25 > 0.2.9`,
/// `2026.10.08.0306 > 2026.10.07.2040`), anything else as text; on a common
/// prefix the longer version wins (`0.2.25_1 > 0.2.25`).
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let is_separator = |c: char| matches!(c, '.' | '_' | '-');
    let mut left = a.split(is_separator);
    let mut right = b.split(is_separator);
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// Production [`Host`]: thin `std::process::Command` wrappers. brew output is
/// inherited (streamed live) so long upgrades are never silent; queries
/// capture stdout. Never handles credentials.
pub struct RealHost;

impl RealHost {
    fn brew_list(&self, kind: &str) -> Option<Vec<String>> {
        let output = std::process::Command::new("brew")
            .args(["list", kind])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Some(text.split_whitespace().map(str::to_string).collect())
    }

    /// `brew --prefix [<formula>]`: Homebrew's prefix, or a formula's opt
    /// path. The opt path is printed even for a formula that is not
    /// installed — callers must check the disk.
    fn brew_prefix(&self, formula: Option<&str>) -> Option<std::path::PathBuf> {
        let mut command = std::process::Command::new("brew");
        command.arg("--prefix");
        if let Some(formula) = formula {
            command.arg(formula);
        }
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!prefix.is_empty()).then(|| std::path::PathBuf::from(prefix))
    }

    /// `brew list --versions [--cask] <name>` stdout (`<name> <version>…`),
    /// or `None` when it is not installed or brew can't answer.
    fn list_versions(&self, name: &str, cask: bool) -> Option<String> {
        let mut command = std::process::Command::new("brew");
        command.args(["list", "--versions"]);
        if cask {
            command.arg("--cask");
        }
        command.arg(name);
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// `<prefix>/var/homebrew/<kind>`: where brew keeps its per-formula `linked`
/// / `pinned` records (Homebrew `startup/config.rb`).
fn records_dir(prefix: &std::path::Path, kind: &str) -> std::path::PathBuf {
    prefix.join("var").join("homebrew").join(kind)
}

impl Host for RealHost {
    fn brew_formulae(&self) -> Option<Vec<String>> {
        self.brew_list("--formula")
    }

    fn brew_casks(&self) -> Option<Vec<String>> {
        self.brew_list("--cask")
    }

    fn binary_channel(&self) -> Option<Channel> {
        binary_channel_from_build()
    }

    fn installed_version(&self, name: &str, cask: bool) -> Option<String> {
        if cask {
            // `brew list --versions --cask <name>` → "<name> <version>".
            return self
                .list_versions(name, true)?
                .lines()
                .next()?
                .split_whitespace()
                .nth(1)
                .map(str::to_string);
        }
        // A formula's opt link points at the keg actually in use — the
        // authoritative answer when several kegs are installed.
        let opt_keg = self
            .brew_prefix(Some(name))
            .and_then(|opt| std::fs::canonicalize(opt).ok());
        formula_version(name, opt_keg.as_deref(), || self.list_versions(name, false))
    }

    fn installed_dependents(&self, formula: &str) -> Option<Vec<String>> {
        let output = std::process::Command::new("brew")
            .args(["uses", "--installed", formula])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Some(text.split_whitespace().map(str::to_string).collect())
    }

    fn linked_formulae(&self) -> Option<Vec<String>> {
        let prefix = self.brew_prefix(None)?;
        linked_records(
            &records_dir(&prefix, "linked"),
            &prefix.join("bin").join("llmux"),
        )
    }

    fn pinned_formulae(&self) -> Option<Vec<String>> {
        let prefix = self.brew_prefix(None)?;
        pinned_records(&records_dir(&prefix, "pinned"))
    }

    fn run_brew(&self, args: &[&str]) -> Result<(), CliError> {
        // Inherit stdio: the user watches the live brew progress.
        let status = std::process::Command::new("brew")
            .args(args)
            .status()
            .map_err(|err| {
                CliError::Message(format!(
                    "failed to run `brew {}`: {err} (is Homebrew installed?)",
                    args.join(" ")
                ))
            })?;
        if !status.success() {
            return Err(CliError::Message(format!(
                "`brew {}` failed with {status}",
                args.join(" ")
            )));
        }
        Ok(())
    }

    fn formula_bin_path(&self, formula: &str) -> Option<std::path::PathBuf> {
        // `brew --prefix <formula>` prints the opt path even for a formula
        // that is not installed — the exists() check below is what makes the
        // answer authoritative.
        let bin = self.brew_prefix(Some(formula))?.join("bin").join("llmux");
        bin.exists().then_some(bin)
    }

    fn binary_version(&self, exe: &std::path::Path) -> Option<String> {
        // clap prints "<bin> <version> (<channel> <id>)" on stdout.
        let output = std::process::Command::new(exe)
            .arg("--version")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!line.is_empty()).then_some(line)
    }

    fn islands_running(&self) -> bool {
        // `pgrep -x` matches the exact process name (the app's PRODUCT_NAME,
        // `LlmuxIslands`, from llmux-islands/project.yml).
        std::process::Command::new("pgrep")
            .args(["-x", ISLANDS_PROCESS])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn relaunch_islands(&self) {
        // Best-effort: quit the old build, reopen the new one. Failures are
        // swallowed — a stale app is a minor annoyance, not a command failure.
        let _ = std::process::Command::new("killall")
            .arg(ISLANDS_PROCESS)
            .status();
        let _ = std::process::Command::new("open")
            .args(["-a", ISLANDS_PROCESS])
            .status();
    }
}

/// Exact process/app name of the menu-bar app (PRODUCT_NAME in
/// `llmux-islands/project.yml`), used by `pgrep -x` / `killall` / `open -a`.
const ISLANDS_PROCESS: &str = "LlmuxIslands";

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::RefCell;

    /// `(name, is_cask)` → installed version, the store `installed_version`
    /// answers from.
    type VersionStore = RefCell<Vec<((String, bool), String)>>;
    /// Hook run after each `brew` command, to model an upgrade changing an
    /// installed version.
    type RunHook = Box<dyn Fn(&[&str], &VersionStore)>;

    /// Scripted [`Host`] for unit tests: canned query answers + a recorded
    /// ordered log of every mutating call (`brew` runs, relaunches).
    pub struct FakeHost {
        pub formulae: Option<Vec<String>>,
        pub casks: Option<Vec<String>>,
        pub binary: Option<Channel>,
        /// `(name, cask)` → version answered by `installed_version`. Mutated
        /// by `on_run` to model an upgrade changing the installed version.
        pub versions: VersionStore,
        pub islands_running: bool,
        /// `formula` → binary path answered by `formula_bin_path`.
        pub bin_paths: Vec<(String, std::path::PathBuf)>,
        /// `path` → version answered by `binary_version` (no subprocess).
        pub binary_versions: Vec<(std::path::PathBuf, String)>,
        /// `formula` → `installed_dependents` answer (`None` = brew can't
        /// answer). A formula with no entry has no dependents.
        pub dependents: Vec<(String, Option<Vec<String>>)>,
        /// `linked_formulae` answer (default `None`: link state unknown).
        pub linked: Option<Vec<String>>,
        /// `pinned_formulae` answer (default: nothing pinned).
        pub pinned: Option<Vec<String>>,
        /// `run_brew` fails for exactly this `"brew <args>"` (still logged),
        /// modelling a download/build failure mid-plan.
        pub fail_on: Option<String>,
        /// Ordered log: `"brew <args>"`, `"uses <formula>"` (the dependents
        /// query, logged so tests can pin it before any brew command) and
        /// `"relaunch-islands"`.
        pub calls: RefCell<Vec<String>>,
        /// Optional hook: given the brew args just run, mutate `versions`
        /// (e.g. bump the formula version so `formula_changed()` is true).
        pub on_run: Option<RunHook>,
    }

    impl FakeHost {
        pub fn new() -> Self {
            FakeHost {
                formulae: None,
                casks: None,
                binary: None,
                versions: RefCell::new(Vec::new()),
                islands_running: false,
                bin_paths: Vec::new(),
                binary_versions: Vec::new(),
                dependents: Vec::new(),
                linked: None,
                pinned: Some(Vec::new()),
                fail_on: None,
                calls: RefCell::new(Vec::new()),
                on_run: None,
            }
        }

        pub fn with_versions(mut self, entries: &[(&str, bool, &str)]) -> Self {
            self.versions = RefCell::new(
                entries
                    .iter()
                    .map(|(n, c, v)| (((*n).to_string(), *c), (*v).to_string()))
                    .collect(),
            );
            self
        }
    }

    impl Host for FakeHost {
        fn brew_formulae(&self) -> Option<Vec<String>> {
            self.formulae.clone()
        }
        fn brew_casks(&self) -> Option<Vec<String>> {
            self.casks.clone()
        }
        fn binary_channel(&self) -> Option<Channel> {
            self.binary
        }
        fn installed_version(&self, name: &str, cask: bool) -> Option<String> {
            self.versions
                .borrow()
                .iter()
                .find(|((n, c), _)| n == name && *c == cask)
                .map(|(_, v)| v.clone())
        }
        fn installed_dependents(&self, formula: &str) -> Option<Vec<String>> {
            self.calls.borrow_mut().push(format!("uses {formula}"));
            match self.dependents.iter().find(|(name, _)| name == formula) {
                Some((_, answer)) => answer.clone(),
                None => Some(Vec::new()),
            }
        }
        fn linked_formulae(&self) -> Option<Vec<String>> {
            self.linked.clone()
        }
        fn pinned_formulae(&self) -> Option<Vec<String>> {
            self.pinned.clone()
        }
        fn run_brew(&self, args: &[&str]) -> Result<(), CliError> {
            let call = format!("brew {}", args.join(" "));
            self.calls.borrow_mut().push(call.clone());
            if self.fail_on.as_deref() == Some(call.as_str()) {
                return Err(CliError::Message(format!("`{call}` failed (injected)")));
            }
            if let Some(hook) = &self.on_run {
                hook(args, &self.versions);
            }
            Ok(())
        }
        fn formula_bin_path(&self, formula: &str) -> Option<std::path::PathBuf> {
            self.bin_paths
                .iter()
                .find(|(name, _)| name == formula)
                .map(|(_, path)| path.clone())
        }
        fn binary_version(&self, exe: &std::path::Path) -> Option<String> {
            self.binary_versions
                .iter()
                .find(|(path, _)| path == exe)
                .map(|(_, version)| version.clone())
        }
        fn islands_running(&self) -> bool {
            self.islands_running
        }
        fn relaunch_islands(&self) {
            self.calls.borrow_mut().push("relaunch-islands".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeHost;
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn detect_prefers_preview_when_both_formulae_present() {
        let f = strs(&["llmux", "llmux-preview", "jq"]);
        let d = detect_channel(Some(&f), None, None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, DetectSource::Brew);
        assert!(d.warning.is_some(), "both installed must warn");
    }

    #[test]
    fn detect_warning_names_both_channel_choices() {
        // Both installed ⇒ preview wins, so advice naming only `llmux channel
        // stable` would move the user AWAY from the channel they run. The
        // warning must offer both choices and say what each one does — true
        // also when another formula needs the loser (it is only unlinked).
        let d = detect_channel(Some(&strs(&["llmux", "llmux-preview"])), None, None).unwrap();
        let warning = d.warning.unwrap();
        assert!(
            warning.contains(
                "`llmux channel preview` keeps preview \
                 (removes llmux, or unlinks it when other formulae need it)"
            ),
            "{warning}"
        );
        assert!(
            warning.contains(
                "`llmux channel stable` moves to stable \
                 (removes llmux-preview, or unlinks it when other formulae need it)"
            ),
            "{warning}"
        );
        assert!(!warning.contains("(removes llmux)"), "{warning}");
    }

    #[test]
    fn detect_reads_single_installed_formula() {
        let stable = detect_channel(Some(&strs(&["llmux", "jq"])), None, None).unwrap();
        assert_eq!(stable.channel, Channel::Stable);
        assert!(stable.warning.is_none());

        let preview = detect_channel(Some(&strs(&["llmux-preview"])), None, None).unwrap();
        assert_eq!(preview.channel, Channel::Preview);
    }

    #[test]
    fn detect_does_not_confuse_preview_for_stable() {
        // Only preview installed: the exact-match must NOT count `llmux` as
        // present just because `llmux-preview` contains it.
        let d = detect_channel(Some(&strs(&["llmux-preview"])), None, None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert!(d.warning.is_none(), "not both-installed");
    }

    #[test]
    fn detect_falls_back_to_binary_marker_when_brew_silent() {
        // brew unavailable → use the build marker.
        let d = detect_channel(None, None, Some(Channel::Preview)).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, DetectSource::BinaryFallback);

        // brew present but neither formula installed → also fall back.
        let d = detect_channel(Some(&strs(&["jq"])), None, Some(Channel::Stable)).unwrap();
        assert_eq!(d.channel, Channel::Stable);
        assert_eq!(d.source, DetectSource::BinaryFallback);
    }

    #[test]
    fn detect_errors_on_dev_build_with_no_brew_formula() {
        let err = detect_channel(None, None, None).unwrap_err();
        assert!(err.to_string().contains("cannot determine release channel"));
    }

    #[test]
    fn switch_plan_installs_the_target_before_retiring_the_outgoing_formula() {
        // Target first: a failed download/build stops the plan while the old
        // channel is still whole.
        let plan = switch_plan(Channel::Stable, Channel::Preview, None, false, &[]);
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["install", "llmux-preview"])),
                // `--force`: a plain uninstall removes only the current keg,
                // leaving the rack listed by `brew list --formula` — and so
                // `detect_channel` still seeing both formulae.
                BrewCmd(strs(&["uninstall", "--force", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
            ]
        );
    }

    #[test]
    fn switch_plan_upgrades_and_relinks_when_target_already_installed() {
        // Both-installed state (issue #202): `brew install` would no-op, and
        // a bare `brew link` answers "Already linked" (exit 0, nothing done)
        // while the keg's LinkedKegs record survives. So: upgrade the stale
        // target keg BEFORE touching the outgoing channel, then link it for
        // real (unlink clears the record) once the outgoing one is retired.
        let plan = switch_plan(Channel::Preview, Channel::Stable, None, true, &[]);
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["upgrade", "llmux"])),
                BrewCmd(strs(&["uninstall", "--force", "llmux-preview"])),
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
            ]
        );
    }

    #[test]
    fn consolidate_plan_removes_stray_and_relinks_target() {
        // Re-selecting the current channel in the both-installed state must
        // remove the OTHER formula (every keg: `--force`) and restore the
        // target's symlink — `unlink` first, because a bare `link` of a keg
        // whose link record survived answers "Already linked" and does nothing.
        assert_eq!(
            consolidate_plan(Channel::Preview, None, &[]),
            vec![
                BrewCmd(strs(&["uninstall", "--force", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
            ]
        );
        assert_eq!(
            consolidate_plan(Channel::Stable, None, &[]),
            vec![
                BrewCmd(strs(&["uninstall", "--force", "llmux-preview"])),
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
            ]
        );
        // A stray-channel Islands cask blocks `brew uninstall <stray formula>`
        // (live-verified on iq-64: "required by llmux-islands") — it must be
        // removed first and the target-channel cask installed last.
        assert_eq!(
            consolidate_plan(Channel::Preview, Some(Channel::Stable), &[]),
            vec![
                BrewCmd(strs(&["uninstall", "--cask", "llmux-islands"])),
                BrewCmd(strs(&["uninstall", "--force", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
                BrewCmd(strs(&["install", "--cask", "llmux-islands-preview"])),
            ]
        );
        // Islands already on the target channel → formula-only consolidation.
        assert_eq!(
            consolidate_plan(Channel::Preview, Some(Channel::Preview), &[]),
            vec![
                BrewCmd(strs(&["uninstall", "--force", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
            ]
        );
    }

    #[test]
    fn installed_binary_resolves_or_errors_with_guidance() {
        let mut host = FakeHost::new();
        host.bin_paths = vec![(
            "llmux".into(),
            std::path::PathBuf::from("/opt/homebrew/opt/llmux/bin/llmux"),
        )];
        assert_eq!(
            installed_binary(&host, Channel::Stable).unwrap(),
            std::path::PathBuf::from("/opt/homebrew/opt/llmux/bin/llmux")
        );
        let err = installed_binary(&host, Channel::Preview).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("llmux-preview"), "names the formula: {msg}");
        assert!(
            msg.contains("was not restarted"),
            "promises the daemon was untouched: {msg}"
        );
    }

    #[test]
    fn switch_plan_mirrors_islands_cask_when_installed_off_target() {
        let plan = switch_plan(
            Channel::Preview,
            Channel::Stable,
            Some(Channel::Preview),
            false,
            &[],
        );
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["install", "llmux"])),
                // Outgoing cask before the outgoing formula: it depends on it,
                // so brew would refuse the formula uninstall while it exists.
                BrewCmd(strs(&["uninstall", "--cask", "llmux-islands-preview"])),
                BrewCmd(strs(&["uninstall", "--force", "llmux-preview"])),
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
                BrewCmd(strs(&["install", "--cask", "llmux-islands"])),
            ]
        );
    }

    #[test]
    fn switch_plan_skips_cask_already_on_target_channel() {
        // Islands somehow already on the target channel → no cask churn.
        let plan = switch_plan(
            Channel::Stable,
            Channel::Preview,
            Some(Channel::Preview),
            false,
            &[],
        );
        assert!(
            !plan.iter().any(|c| c.0.contains(&"--cask".to_string())),
            "no cask commands: {plan:?}"
        );
    }

    #[test]
    fn update_plan_upgrades_formula_and_optional_cask() {
        assert_eq!(
            update_plan(Channel::Preview, false),
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["upgrade", "llmux-preview"])),
            ]
        );
        assert_eq!(
            update_plan(Channel::Stable, true),
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["upgrade", "llmux"])),
                BrewCmd(strs(&["upgrade", "--cask", "llmux-islands"])),
            ]
        );
    }

    #[test]
    fn restart_and_relaunch_conditions() {
        assert!(should_restart_daemon(true, true));
        assert!(
            !should_restart_daemon(true, false),
            "unchanged must not churn"
        );
        assert!(!should_restart_daemon(false, true), "not running");

        assert!(should_relaunch_islands(true, true));
        assert!(!should_relaunch_islands(false, true));
        assert!(!should_relaunch_islands(true, false));
    }

    // Three INDEPENDENT builds, so no case can pass by accident: what the
    // user invoked (A), what the daemon serves, and what brew installed (B).
    const STALE_A: &str = "llmux 0.2.19 (preview 2026-08-13-0413-aaaaaaa)";
    const FRESH_B: &str = "llmux 0.2.19 (preview 2026-08-21-0458-bbbbbbb)";
    const OTHER_C: &str = "llmux 0.2.18 (stable 2026-07-30-1200-ccccccc)";

    #[test]
    fn version_mismatch_restart_conditions() {
        // Shape (a): the invoking CLI is itself stale A and so is the server;
        // brew has B installed. Comparing against the INSTALLED binary is what
        // makes this fire — a client-process comparand would see A == A.
        assert!(
            should_restart_for_version_mismatch(true, Some(STALE_A), Some(FRESH_B)),
            "a daemon left on another build must converge on the installed one"
        );
        // Shape (b): the CLI is stale A but the server already runs the
        // installed B — nothing to apply, restarting would be pointless churn.
        assert!(
            !should_restart_for_version_mismatch(true, Some(FRESH_B), Some(FRESH_B)),
            "server already on the installed build must not churn"
        );
        // A third build in play changes nothing: server ≠ installed ⇒ restart.
        assert!(should_restart_for_version_mismatch(
            true,
            Some(OTHER_C),
            Some(FRESH_B)
        ));
        assert!(
            !should_restart_for_version_mismatch(true, None, Some(FRESH_B)),
            "unknown server version is missing data, not a mismatch"
        );
        assert!(
            !should_restart_for_version_mismatch(true, Some(STALE_A), None),
            "unresolvable installed version is missing data, not a mismatch"
        );
        assert!(
            !should_restart_for_version_mismatch(false, Some(STALE_A), Some(FRESH_B)),
            "not running"
        );
    }

    #[test]
    fn binary_version_comes_from_the_artifact_on_disk() {
        let exe = std::path::PathBuf::from("/opt/homebrew/opt/llmux-preview/bin/llmux");
        let mut host = FakeHost::new();
        host.binary_versions = vec![(exe.clone(), FRESH_B.to_string())];
        assert_eq!(host.binary_version(&exe).as_deref(), Some(FRESH_B));
        assert_eq!(host.binary_version(std::path::Path::new("/nope")), None);
    }

    #[test]
    fn installed_islands_channel_prefers_preview() {
        assert_eq!(
            installed_islands_channel(Some(&strs(&["llmux-islands", "llmux-islands-preview"]))),
            Some(Channel::Preview)
        );
        assert_eq!(
            installed_islands_channel(Some(&strs(&["llmux-islands"]))),
            Some(Channel::Stable)
        );
        assert_eq!(installed_islands_channel(Some(&strs(&["jq"]))), None);
        assert_eq!(installed_islands_channel(None), None);
    }

    #[test]
    fn execute_switch_runs_plan_in_order_and_reports_versions() {
        // Only the OUTGOING formula is installed up front; the target's
        // version appears when the plan's `install` runs (`on_run` hook) —
        // pre-populating it would model the both-installed state, which
        // plans a relink instead of an install.
        let host = FakeHost {
            on_run: Some(Box::new(|args, versions| {
                if args == ["install", "llmux-preview"] {
                    versions
                        .borrow_mut()
                        .push((("llmux-preview".into(), false), "0.2.0".into()));
                }
            })),
            ..FakeHost::new()
        }
        .with_versions(&[("llmux", false, "0.1.0")]);
        let report = execute_switch(&host, Channel::Stable, Channel::Preview, None, true).unwrap();
        assert_eq!(report.old_version.as_deref(), Some("0.1.0"));
        assert_eq!(report.new_version.as_deref(), Some("0.2.0"));
        assert!(report.daemon_running);
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux",
                "brew update",
                "brew install llmux-preview",
                "brew uninstall --force llmux",
                "brew unlink llmux-preview",
                "brew link --overwrite llmux-preview",
            ]
        );
    }

    #[test]
    fn execute_switch_upgrades_and_relinks_a_stale_installed_target() {
        // Issue #202 verbatim: preview + stable both installed, the stable
        // keg stale (0.2.24) while the tap carries 0.2.25. The switch must
        // upgrade it (else the daemon restarts on the stale keg) and relink
        // it via unlink + link (else `bin/llmux` stays missing).
        let host = FakeHost {
            on_run: Some(Box::new(|args, versions| {
                if args == ["upgrade", "llmux"] {
                    for ((name, cask), version) in versions.borrow_mut().iter_mut() {
                        if name == "llmux" && !*cask {
                            *version = "0.2.25".into();
                        }
                    }
                }
            })),
            ..FakeHost::new()
        }
        .with_versions(&[
            ("llmux", false, "0.2.24"),
            ("llmux-preview", false, "2026.10.08.0306"),
        ]);
        let report = execute_switch(&host, Channel::Preview, Channel::Stable, None, true).unwrap();
        assert_eq!(report.old_version.as_deref(), Some("2026.10.08.0306"));
        assert_eq!(report.new_version.as_deref(), Some("0.2.25"));
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux-preview",
                "brew update",
                "brew upgrade llmux",
                "brew uninstall --force llmux-preview",
                "brew unlink llmux",
                "brew link --overwrite llmux",
            ]
        );
    }

    #[test]
    fn execute_update_detects_changed_formula_and_streams_islands_state() {
        // Islands installed + running; the upgrade bumps the formula version.
        let host = FakeHost {
            islands_running: true,
            on_run: Some(Box::new(|args, versions| {
                // Model the upgrade bumping the installed formula version.
                if args == ["upgrade", "llmux-preview"] {
                    for ((name, cask), version) in versions.borrow_mut().iter_mut() {
                        if name == "llmux-preview" && !*cask {
                            *version = "0.3.0".into();
                        }
                    }
                }
            })),
            ..FakeHost::new()
        }
        .with_versions(&[
            ("llmux-preview", false, "0.2.0"),
            ("llmux-islands-preview", true, "0.2.3"),
        ]);

        let report = execute_update(&host, Channel::Preview).unwrap();
        assert!(report.islands_was_running);
        assert!(
            report.formula_changed(),
            "formula version moved 0.2.0→0.3.0"
        );
        assert!(!report.cask_changed(), "cask untouched by the hook");
        // The cask WAS installed, so its upgrade must be in the plan.
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "brew update",
                "brew upgrade llmux-preview",
                "brew upgrade --cask llmux-islands-preview",
            ]
        );
        // Decisions the handler will act on.
        assert!(should_restart_daemon(true, report.formula_changed()));
        assert!(!should_relaunch_islands(
            report.islands_was_running,
            report.cask_changed()
        ));
    }

    #[test]
    fn execute_update_skips_cask_when_islands_absent() {
        let host = FakeHost::new().with_versions(&[("llmux", false, "0.1.0")]);
        let report = execute_update(&host, Channel::Stable).unwrap();
        assert!(!report.islands_was_running);
        assert!(!report.formula_changed(), "already up to date");
        assert_eq!(
            *host.calls.borrow(),
            vec!["brew update", "brew upgrade llmux"]
        );
    }

    // `brew uses --installed` as observed on the reporting machine (#202).
    const LLMUX_USERS: [&str; 2] = ["2lab-ai/tap/somawork-cli", "2lab-ai/tap/somawork-preview"];

    #[test]
    fn blocking_dependents_strips_taps_and_ignores_islands_casks() {
        // Real output is tap-prefixed, transitive, and includes casks.
        assert_eq!(
            blocking_dependents(&strs(&LLMUX_USERS)),
            strs(&["somawork-cli", "somawork-preview"])
        );
        // The Islands casks never block: every plan removes the outgoing
        // cask before it touches the formula that cask depends on.
        assert!(blocking_dependents(&strs(&["2lab-ai/tap/llmux-islands-preview"])).is_empty());
        assert!(
            blocking_dependents(&strs(&["llmux-islands", "2lab-ai/tap/llmux-islands"])).is_empty()
        );
        assert!(blocking_dependents(&[]).is_empty());
    }

    #[test]
    fn switch_plan_unlinks_an_outgoing_formula_other_formulae_need() {
        // `brew uninstall [--force] llmux` refuses while somawork-cli needs
        // it — and would do so mid-plan, after the outgoing Islands cask is
        // already gone. Keep it installed, just get it off `bin/llmux`.
        let mut users = strs(&LLMUX_USERS);
        users.push("2lab-ai/tap/llmux-islands".into());
        let plan = switch_plan(
            Channel::Stable,
            Channel::Preview,
            Some(Channel::Stable),
            false,
            &users,
        );
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["install", "llmux-preview"])),
                BrewCmd(strs(&["uninstall", "--cask", "llmux-islands"])),
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
                BrewCmd(strs(&["install", "--cask", "llmux-islands-preview"])),
            ]
        );
    }

    #[test]
    fn switch_plan_still_uninstalls_when_only_islands_depend() {
        // `brew uses --installed llmux-preview` = the Islands cask alone,
        // which the plan removes first — so the formula really goes.
        let plan = switch_plan(
            Channel::Preview,
            Channel::Stable,
            Some(Channel::Preview),
            true,
            &strs(&["2lab-ai/tap/llmux-islands-preview"]),
        );
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["upgrade", "llmux"])),
                BrewCmd(strs(&["uninstall", "--cask", "llmux-islands-preview"])),
                BrewCmd(strs(&["uninstall", "--force", "llmux-preview"])),
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
                BrewCmd(strs(&["install", "--cask", "llmux-islands"])),
            ]
        );
    }

    #[test]
    fn consolidate_plan_unlinks_a_stray_formula_other_formulae_need() {
        assert_eq!(
            consolidate_plan(Channel::Preview, None, &strs(&["2lab-ai/tap/somawork-cli"])),
            vec![
                BrewCmd(strs(&["unlink", "llmux"])),
                BrewCmd(strs(&["unlink", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
            ]
        );
    }

    #[test]
    fn kept_formula_summary_names_the_dependents() {
        let kept = KeptFormula {
            formula: "llmux".into(),
            required_by: strs(&["somawork-cli", "somawork-preview"]),
        };
        assert_eq!(
            kept.summary(),
            "kept llmux installed (required by somawork-cli, somawork-preview) — unlinked"
        );
    }

    #[test]
    fn execute_switch_queries_dependents_before_any_brew_command() {
        let mut host = FakeHost::new().with_versions(&[("llmux", false, "0.2.25")]);
        host.dependents = vec![("llmux".into(), Some(strs(&LLMUX_USERS)))];
        let report = execute_switch(&host, Channel::Stable, Channel::Preview, None, false).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux",
                "brew update",
                "brew install llmux-preview",
                "brew unlink llmux",
                "brew unlink llmux-preview",
                "brew link --overwrite llmux-preview",
            ]
        );
        assert_eq!(
            report.kept,
            Some(KeptFormula {
                formula: "llmux".into(),
                required_by: strs(&["somawork-cli", "somawork-preview"]),
            })
        );
    }

    #[test]
    fn execute_switch_leaves_the_machine_untouched_when_dependents_are_unknown() {
        // An unanswerable `brew uses` must not fall back to "no dependents":
        // that is exactly the mid-plan refusal that half-switched #202.
        let mut host = FakeHost::new().with_versions(&[("llmux", false, "0.2.25")]);
        host.dependents = vec![("llmux".into(), None)];
        let err = execute_switch(
            &host,
            Channel::Stable,
            Channel::Preview,
            Some(Channel::Stable),
            true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("brew uses --installed llmux"), "{msg}");
        assert!(msg.contains("nothing was changed"), "{msg}");
        assert_eq!(
            *host.calls.borrow(),
            vec!["uses llmux"],
            "no brew command ran"
        );
    }

    #[test]
    fn keg_version_reads_the_cellar_dir_the_opt_link_resolves_to() {
        use std::path::Path;
        assert_eq!(
            keg_version("llmux", Path::new("/opt/homebrew/Cellar/llmux/0.2.25")).as_deref(),
            Some("0.2.25")
        );
        assert_eq!(
            keg_version(
                "llmux-preview",
                Path::new("/opt/homebrew/Cellar/llmux-preview/2026.10.08.0724")
            )
            .as_deref(),
            Some("2026.10.08.0724")
        );
        // Resolved to something that is not this formula's keg → no answer
        // (the caller falls back to `brew list --versions`).
        assert_eq!(
            keg_version(
                "llmux",
                Path::new("/opt/homebrew/Cellar/llmux-preview/2026.10.08.0724")
            ),
            None
        );
        assert_eq!(
            keg_version("llmux", Path::new("/opt/homebrew/opt/llmux")),
            None
        );
        assert_eq!(keg_version("llmux", Path::new("/")), None);
    }

    #[test]
    fn highest_listed_version_picks_the_newest_keg_not_the_first() {
        // #202's multi-keg listing: the first entry (what `.nth(1)` printed)
        // is not the newest one.
        assert_eq!(
            highest_listed_version(
                "llmux-preview 2026.10.07.1540 2026.10.07.2040 2026.10.07.1404 2026.10.08.0306\n"
            )
            .as_deref(),
            Some("2026.10.08.0306")
        );
        assert_eq!(
            highest_listed_version("llmux 0.2.9 0.2.25").as_deref(),
            Some("0.2.25"),
            "numeric, not lexical"
        );
        assert_eq!(
            highest_listed_version("llmux 0.2.25_1 0.2.25").as_deref(),
            Some("0.2.25_1"),
            "a brew revision is newer"
        );
        assert_eq!(
            highest_listed_version("llmux 0.2.23").as_deref(),
            Some("0.2.23")
        );
        assert_eq!(highest_listed_version("llmux"), None);
        assert_eq!(highest_listed_version(""), None);
    }

    #[test]
    fn compare_versions_orders_segments_numerically() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.2.25", "0.2.9"), Ordering::Greater);
        assert_eq!(
            compare_versions("2026.10.08.0306", "2026.10.07.2040"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("1.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.25", "0.2.25_1"), Ordering::Less);
        assert_eq!(compare_versions("0.2.25", "0.2.25"), Ordering::Equal);
    }

    // Both formulae installed: what a switch leaves when another formula
    // (somawork-cli) keeps the outgoing one installed but unlinked.
    fn both_installed() -> Vec<String> {
        strs(&["llmux", "llmux-preview", "somawork-cli"])
    }

    #[test]
    fn detect_both_installed_only_stable_linked_is_stable() {
        let d = detect_channel(Some(&both_installed()), Some(&strs(&["llmux"])), None).unwrap();
        assert_eq!(
            d.channel,
            Channel::Stable,
            "`bin/llmux` is the stable binary"
        );
        assert_eq!(d.source, DetectSource::Brew);
        assert_eq!(d.warning, None);
    }

    #[test]
    fn detect_both_installed_only_preview_linked_is_preview() {
        // This machine: `llmux` kept (unlinked) for somawork-cli.
        let d = detect_channel(
            Some(&both_installed()),
            Some(&strs(&["llmux-preview"])),
            None,
        )
        .unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, DetectSource::Brew);
        assert_eq!(d.warning, None, "a kept, unlinked stray is not ambiguous");
    }

    #[test]
    fn detect_both_installed_both_linked_warns_and_preview_wins() {
        // Two link records (a `link --overwrite` leaves the loser's record):
        // brew's state is ambiguous, so the advisory stays.
        let d = detect_channel(
            Some(&both_installed()),
            Some(&strs(&["llmux", "llmux-preview"])),
            None,
        )
        .unwrap();
        assert_eq!(d.channel, Channel::Preview);
        let warning = d.warning.unwrap();
        assert!(
            warning.contains("`llmux channel stable` moves to stable"),
            "{warning}"
        );
    }

    #[test]
    fn detect_both_installed_neither_linked_warns_and_preview_wins() {
        let d = detect_channel(Some(&both_installed()), Some(&[]), None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert!(d.warning.is_some(), "neither linked must warn");
        // Unreadable link state is no evidence either way: same answer.
        let d = detect_channel(Some(&both_installed()), None, None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert!(d.warning.is_some(), "unknown link state must warn");
    }

    /// Self-cleaning unique temp dir (no tempfile dev-dependency) — the
    /// `config` tests' idiom.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "llmux-brew-test-{}-{}",
                std::process::id(),
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn execute_switch_refuses_only_a_pinned_formula_it_would_upgrade() {
        let both = [
            ("llmux", false, "0.2.24"),
            ("llmux-preview", false, "2026.10.08.0306"),
        ];
        // Target installed AND pinned: the plan would `brew upgrade llmux`,
        // which refuses mid-switch (after the outgoing formula is gone).
        let mut host = FakeHost::new().with_versions(&both);
        host.pinned = Some(strs(&["llmux"]));
        let err = execute_switch(
            &host,
            Channel::Preview,
            Channel::Stable,
            Some(Channel::Preview),
            true,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`brew unpin llmux`"), "{msg}");
        assert!(msg.contains("nothing was changed"), "{msg}");
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls.borrow());

        // Unreadable pin records where the check applies: unknown is not
        // "unpinned".
        let mut host = FakeHost::new().with_versions(&both);
        host.pinned = None;
        let err = execute_switch(&host, Channel::Preview, Channel::Stable, None, true).unwrap_err();
        assert!(err.to_string().contains("nothing was changed"), "{err}");
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls.borrow());

        // Outgoing pinned: `uninstall --force` / `unlink` ignore pins, so the
        // switch proceeds.
        let mut host = FakeHost::new().with_versions(&both);
        host.pinned = Some(strs(&["llmux-preview"]));
        execute_switch(&host, Channel::Preview, Channel::Stable, None, true).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux-preview",
                "brew update",
                "brew upgrade llmux",
                "brew uninstall --force llmux-preview",
                "brew unlink llmux",
                "brew link --overwrite llmux",
            ]
        );

        // Target NOT installed: the plan installs, never upgrades — a stale
        // pin record and an unreadable pin state are both irrelevant.
        for pinned in [Some(strs(&["llmux"])), None] {
            let mut host =
                FakeHost::new().with_versions(&[("llmux-preview", false, "2026.10.08.0306")]);
            host.pinned = pinned;
            execute_switch(&host, Channel::Preview, Channel::Stable, None, true).unwrap();
            assert_eq!(
                *host.calls.borrow(),
                vec![
                    "uses llmux-preview",
                    "brew update",
                    "brew install llmux",
                    "brew uninstall --force llmux-preview",
                    "brew unlink llmux",
                    "brew link --overwrite llmux",
                ]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn brew_records_follow_keg_linked_and_formula_pin_semantics() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new();
        let preview_keg = tmp.0.join("Cellar/llmux-preview/2026.10.08.0724");
        std::fs::create_dir_all(preview_keg.join("bin")).unwrap();
        std::fs::write(preview_keg.join("bin/llmux"), b"").unwrap();
        let stable_keg = tmp.0.join("Cellar/llmux/0.2.24");
        std::fs::create_dir_all(&stable_keg).unwrap();
        let bin = tmp.0.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bin_llmux = bin.join("llmux");
        symlink(preview_keg.join("bin/llmux"), &bin_llmux).unwrap();

        // Linked = the record resolves to a keg dir (`Keg#linked?`) AND
        // `bin/llmux` resolves inside that keg. `llmux` still has a record
        // pointing at a real keg, but `bin/llmux` is preview's binary.
        let linked = tmp.0.join("var/homebrew/linked");
        std::fs::create_dir_all(&linked).unwrap();
        symlink(&preview_keg, linked.join("llmux-preview")).unwrap();
        symlink(&stable_keg, linked.join("llmux")).unwrap();
        assert_eq!(
            linked_records(&linked, &bin_llmux),
            Some(strs(&["llmux-preview"]))
        );

        // #202's aftermath: the record survives, `bin/llmux` is gone —
        // brew calls that "Already linked"; it is not.
        std::fs::remove_file(&bin_llmux).unwrap();
        assert_eq!(linked_records(&linked, &bin_llmux), Some(vec![]));

        // A record whose keg was removed does not count either.
        symlink(stable_keg.join("bin/llmux"), &bin_llmux).unwrap();
        std::fs::remove_dir_all(&stable_keg).unwrap();
        assert_eq!(linked_records(&linked, &bin_llmux), Some(vec![]));

        // `FormulaPin#pinned?`: any symlink record, dangling or not; a plain
        // file is not a pin.
        let pinned = tmp.0.join("var/homebrew/pinned");
        std::fs::create_dir_all(&pinned).unwrap();
        symlink(&stable_keg, pinned.join("llmux")).unwrap();
        std::fs::write(pinned.join("llmux-preview"), b"").unwrap();
        assert_eq!(pinned_records(&pinned), Some(strs(&["llmux"])));

        // No record dir at all (this machine has no `pinned/`) = none.
        let absent = tmp.0.join("var/homebrew/absent");
        assert_eq!(linked_records(&absent, &bin_llmux), Some(vec![]));
        assert_eq!(pinned_records(&absent), Some(vec![]));
    }

    #[test]
    fn formula_version_prefers_the_opt_link_then_the_newest_listed_keg() {
        use std::path::Path;
        // The opt link resolves to this formula's keg: that version, and
        // `brew list --versions` is never run.
        assert_eq!(
            formula_version(
                "llmux",
                Some(Path::new("/opt/homebrew/Cellar/llmux/0.2.25")),
                || panic!("the listing must not run when the opt link answers"),
            )
            .as_deref(),
            Some("0.2.25")
        );
        // No opt link: the newest listed keg, not the first.
        assert_eq!(
            formula_version("llmux-preview", None, || Some(
                "llmux-preview 2026.10.07.1540 2026.10.08.0306 2026.10.07.2040\n".into()
            ))
            .as_deref(),
            Some("2026.10.08.0306")
        );
        // An opt link resolving into another rack is no answer: fall back.
        assert_eq!(
            formula_version(
                "llmux",
                Some(Path::new(
                    "/opt/homebrew/Cellar/llmux-preview/2026.10.08.0724"
                )),
                || Some("llmux 0.2.24 0.2.25".into()),
            )
            .as_deref(),
            Some("0.2.25")
        );
        // Neither answers.
        assert_eq!(formula_version("llmux", None, || None), None);
    }

    fn kept_llmux() -> KeptFormula {
        KeptFormula {
            formula: "llmux".into(),
            required_by: strs(&["somawork-cli", "somawork-preview"]),
        }
    }

    #[test]
    fn same_channel_decision_covers_each_state() {
        let users = strs(&LLMUX_USERS);
        let islands_only = strs(&["2lab-ai/tap/llmux-islands"]);
        let only = |f: &str| Some(strs(&[f]));
        let decide = |installed, deps: &[String], linked: Option<Vec<String>>, islands| {
            same_channel_decision(
                Channel::Preview,
                islands,
                true,
                installed,
                deps,
                linked.as_deref(),
            )
        };

        // The target formula itself not installed (channel from the build
        // marker): there is no keg to relink or consolidate onto.
        for (other_installed, linked) in
            [(false, Some(vec![])), (true, Some(vec![])), (false, None)]
        {
            assert_eq!(
                same_channel_decision(
                    Channel::Preview,
                    None,
                    false,
                    other_installed,
                    &[],
                    linked.as_deref(),
                ),
                SameChannel::NothingToDo
            );
        }

        // Only the target installed.
        assert_eq!(
            decide(false, &[], only("llmux-preview"), None),
            SameChannel::NothingToDo
        );
        assert_eq!(
            decide(false, &[], None, None),
            SameChannel::NothingToDo,
            "unknown link state is no reason to touch brew"
        );
        // #202's aftermath: installed, but `bin/llmux` is not its binary.
        assert_eq!(decide(false, &[], Some(vec![]), None), SameChannel::Relink);
        assert_eq!(decide(false, &[], only("llmux"), None), SameChannel::Relink);

        // Both installed, the stray kept for its dependents, already
        // unlinked, the target the sole linked one: nothing to change.
        assert_eq!(
            decide(true, &users, only("llmux-preview"), None),
            SameChannel::KeptUnlinked(kept_llmux())
        );
        // …unless Islands is on the stray's channel: only consolidation
        // mirrors the cask.
        assert_eq!(
            decide(true, &users, only("llmux-preview"), Some(Channel::Stable)),
            SameChannel::Consolidate {
                kept: Some(kept_llmux())
            }
        );
        // Link state unknown, neither linked, or the stray linked: relink.
        for linked in [None, Some(vec![]), only("llmux")] {
            assert_eq!(
                decide(true, &users, linked, None),
                SameChannel::Consolidate {
                    kept: Some(kept_llmux())
                }
            );
        }
        // Nothing but Islands needs the stray: remove it.
        assert_eq!(
            decide(true, &islands_only, only("llmux-preview"), None),
            SameChannel::Consolidate { kept: None }
        );
    }

    #[test]
    fn same_channel_changes_nothing_when_the_kept_stray_is_already_unlinked() {
        // This machine: `llmux` kept (unlinked) for somawork-cli, preview the
        // sole linked formula. Dependents are queried, then no brew runs.
        let mut host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.25"),
            ("llmux-preview", false, "2026.10.08.0724"),
        ]);
        host.dependents = vec![("llmux".into(), Some(strs(&LLMUX_USERS)))];
        host.linked = Some(strs(&["llmux-preview"]));
        let (action, cmds) = plan_same_channel(&host, Channel::Preview, None).unwrap();
        assert_eq!(action, SameChannel::KeptUnlinked(kept_llmux()));
        assert!(cmds.is_empty(), "{cmds:?}");
        run_plan(&host, &cmds).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec!["uses llmux"],
            "no brew command ran"
        );
        assert_eq!(
            action.headline(Channel::Preview),
            "already on the preview channel — kept llmux installed \
             (required by somawork-cli, somawork-preview), unlinked"
        );
        assert_eq!(action.outcome(Channel::Preview), None);
    }

    #[test]
    fn same_channel_relinks_a_sole_formula_that_lost_bin_llmux() {
        // #202's aftermath on a one-formula machine. A pin is irrelevant:
        // relinking never upgrades.
        let mut host = FakeHost::new().with_versions(&[("llmux", false, "0.2.25")]);
        host.linked = Some(vec![]);
        host.pinned = Some(strs(&["llmux"]));
        let (action, cmds) = plan_same_channel(&host, Channel::Stable, None).unwrap();
        assert_eq!(action, SameChannel::Relink);
        run_plan(&host, &cmds).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec!["brew unlink llmux", "brew link --overwrite llmux"]
        );
        assert!(
            action.headline(Channel::Stable).contains("relinking llmux"),
            "{}",
            action.headline(Channel::Stable)
        );
        assert!(
            action
                .outcome(Channel::Stable)
                .is_some_and(|line| line.starts_with("relinked llmux")),
            "{:?}",
            action.outcome(Channel::Stable)
        );

        // Already its binary: nothing to do, nothing runs.
        let mut host = FakeHost::new().with_versions(&[("llmux", false, "0.2.25")]);
        host.linked = Some(strs(&["llmux"]));
        let (action, cmds) = plan_same_channel(&host, Channel::Stable, None).unwrap();
        assert_eq!(action, SameChannel::NothingToDo);
        run_plan(&host, &cmds).unwrap();
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls.borrow());
        assert_eq!(
            action.headline(Channel::Stable),
            "already on the stable channel — nothing to do"
        );
    }

    #[test]
    fn same_channel_consolidates_after_querying_dependents() {
        // The stray `llmux` is needed by somawork-cli but still linked:
        // keep it, unlink it, relink preview.
        let mut host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.25"),
            ("llmux-preview", false, "2026.10.08.0724"),
        ]);
        host.dependents = vec![("llmux".into(), Some(strs(&LLMUX_USERS)))];
        host.linked = Some(strs(&["llmux"]));
        let (action, cmds) = plan_same_channel(&host, Channel::Preview, None).unwrap();
        run_plan(&host, &cmds).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux",
                "brew unlink llmux",
                "brew unlink llmux-preview",
                "brew link --overwrite llmux-preview",
            ]
        );
        assert_eq!(
            action.outcome(Channel::Preview),
            Some(kept_llmux().summary())
        );

        // Nothing but the stray-channel Islands cask depends on it → gone,
        // and a pinned target does not matter (nothing is upgraded).
        let mut host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.25"),
            ("llmux-preview", false, "2026.10.08.0724"),
        ]);
        host.dependents = vec![(
            "llmux-preview".into(),
            Some(strs(&["2lab-ai/tap/llmux-islands-preview"])),
        )];
        host.pinned = Some(strs(&["llmux"]));
        let (action, cmds) =
            plan_same_channel(&host, Channel::Stable, Some(Channel::Preview)).unwrap();
        assert_eq!(action, SameChannel::Consolidate { kept: None });
        run_plan(&host, &cmds).unwrap();
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "uses llmux-preview",
                "brew uninstall --cask llmux-islands-preview",
                "brew uninstall --force llmux-preview",
                "brew unlink llmux",
                "brew link --overwrite llmux",
                "brew install --cask llmux-islands",
            ]
        );
        assert_eq!(
            action.outcome(Channel::Stable).as_deref(),
            Some("removed llmux-preview")
        );
    }

    #[test]
    fn same_channel_leaves_the_machine_untouched_when_dependents_are_unknown() {
        let mut host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.25"),
            ("llmux-preview", false, "2026.10.08.0724"),
        ]);
        host.dependents = vec![("llmux".into(), None)];
        let err = plan_same_channel(&host, Channel::Preview, Some(Channel::Stable)).unwrap_err();
        assert!(err.to_string().contains("nothing was changed"), "{err}");
        assert_eq!(
            *host.calls.borrow(),
            vec!["uses llmux"],
            "no brew command ran"
        );
    }

    #[test]
    fn same_channel_without_the_target_installed_runs_nothing() {
        // Channel from the build marker (brew lists neither formula): there
        // is no keg to relink — `brew unlink llmux` would just fail.
        let mut host = FakeHost::new();
        host.linked = Some(vec![]);
        let (action, cmds) = plan_same_channel(&host, Channel::Stable, None).unwrap();
        assert_eq!(action, SameChannel::NothingToDo);
        assert!(cmds.is_empty(), "{cmds:?}");
        run_plan(&host, &cmds).unwrap();
        assert!(host.calls.borrow().is_empty(), "{:?}", host.calls.borrow());
    }

    #[test]
    fn execute_switch_failing_target_step_leaves_the_old_channel_intact() {
        // `brew install` fails (download/build — before brew links
        // anything): neither the outgoing cask nor the outgoing formula has
        // been touched.
        let mut host =
            FakeHost::new().with_versions(&[("llmux-preview", false, "2026.10.08.0724")]);
        host.fail_on = Some("brew install llmux".into());
        let err = execute_switch(
            &host,
            Channel::Preview,
            Channel::Stable,
            Some(Channel::Preview),
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("brew install llmux"), "{err}");
        assert_eq!(
            *host.calls.borrow(),
            vec!["uses llmux-preview", "brew update", "brew install llmux"]
        );

        // Upgrading an installed target fails: same — the old channel is
        // whole, and nothing was unlinked before the upgrade.
        let mut host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.24"),
            ("llmux-preview", false, "2026.10.08.0724"),
        ]);
        host.fail_on = Some("brew upgrade llmux".into());
        execute_switch(
            &host,
            Channel::Preview,
            Channel::Stable,
            Some(Channel::Preview),
            true,
        )
        .unwrap_err();
        assert_eq!(
            *host.calls.borrow(),
            vec!["uses llmux-preview", "brew update", "brew upgrade llmux",]
        );
    }

    #[test]
    fn reconcile_daemon_only_after_a_mutating_same_channel_action() {
        let mutating = [
            SameChannel::Relink,
            SameChannel::Consolidate { kept: None },
            SameChannel::Consolidate {
                kept: Some(kept_llmux()),
            },
        ];
        for action in &mutating {
            assert!(action.mutates(), "{action:?}");
            assert!(
                should_reconcile_daemon(action, true, Some(STALE_A), Some(FRESH_B)),
                "{action:?}: a daemon on another build converges"
            );
            assert!(!should_reconcile_daemon(
                action,
                true,
                Some(FRESH_B),
                Some(FRESH_B)
            ));
            assert!(!should_reconcile_daemon(
                action,
                false,
                Some(STALE_A),
                Some(FRESH_B)
            ));
            assert!(!should_reconcile_daemon(action, true, None, Some(FRESH_B)));
            assert!(!should_reconcile_daemon(action, true, Some(STALE_A), None));
        }
        // The genuine no-op and the converged no-churn case never touch the
        // daemon, whatever it serves.
        for action in [
            SameChannel::NothingToDo,
            SameChannel::KeptUnlinked(kept_llmux()),
        ] {
            assert!(!action.mutates(), "{action:?}");
            assert!(!should_reconcile_daemon(
                &action,
                true,
                Some(STALE_A),
                Some(FRESH_B)
            ));
        }
    }

    #[test]
    fn record_probe_is_absent_only_on_not_found() {
        use std::io::{Error, ErrorKind};
        assert_eq!(probe(Ok(7)), Probe::Present(7));
        assert_eq!(
            probe::<()>(Err(Error::from(ErrorKind::NotFound))),
            Probe::Absent
        );
        assert_eq!(
            probe::<()>(Err(Error::from(ErrorKind::PermissionDenied))),
            Probe::Unknown,
            "an unreadable record is not an absent one"
        );
        assert_eq!(probe::<()>(Err(Error::other("io"))), Probe::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_record_dirs_read_as_unknown_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new();
        let records = tmp.0.join("var/homebrew/records");
        std::fs::create_dir_all(&records).unwrap();
        let bin_llmux = tmp.0.join("bin/llmux");
        std::fs::create_dir_all(tmp.0.join("bin")).unwrap();
        std::fs::write(&bin_llmux, b"").unwrap();

        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::symlink_metadata(records.join("llmux"))
            .is_err_and(|err| err.kind() == std::io::ErrorKind::PermissionDenied);
        let pinned = pinned_records(&records);
        let linked = linked_records(&records, &bin_llmux);
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Root ignores directory permissions — the state can't be built.
        if denied {
            assert_eq!(pinned, None, "permission denied is not 'not pinned'");
            assert_eq!(linked, None, "permission denied is not 'not linked'");
        }
    }
}
