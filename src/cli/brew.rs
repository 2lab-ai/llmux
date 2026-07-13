//! Shared machinery for `llmux channel` and `llmux update`: the release
//! channel model, the injectable host abstraction over `brew`/process calls,
//! and the PURE decision logic (channel detection, brew command plans,
//! restart/relaunch conditions).
//!
//! Distribution facts baked in here (single source of truth):
//! - the `llmux` binary ships as brew formula `llmux` (stable) /
//!   `llmux-preview` (preview) — each links a `llmux` executable, so at most
//!   one may be installed at a time;
//! - the menu-bar app ships as cask `llmux-islands` (stable) /
//!   `llmux-islands-preview`.
//!
//! The channel is DERIVED from what brew has installed — there is no config
//! field. All side effects (running `brew`, reading versions, poking the
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
    /// Non-fatal advisory (e.g. both formulae installed — preview wins).
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
    /// when it is not installed.
    fn installed_version(&self, name: &str, cask: bool) -> Option<String>;
    /// Run `brew <args>`, streaming stdio to the user. Errs on non-zero exit.
    fn run_brew(&self, args: &[&str]) -> Result<(), CliError>;
    /// Absolute path to a formula's installed `llmux` binary (via
    /// `brew --prefix <formula>`), verified to exist on disk. `None` when the
    /// formula is not installed or brew can't answer. This is the binary a
    /// post-update/switch daemon restart must spawn — `current_exe()` may be
    /// the keg brew just deleted.
    fn formula_bin_path(&self, formula: &str) -> Option<std::path::PathBuf>;
    /// Is the Islands menu-bar app currently running?
    fn islands_running(&self) -> bool;
    /// Best-effort relaunch of the Islands app (quit + reopen).
    fn relaunch_islands(&self);
}

/// PURE: derive the current channel. Brew is authoritative — if it lists a
/// llmux formula that wins; both installed → preview wins with a warning.
/// Only when brew lists neither (or is unavailable) do we fall back to the
/// binary's own build marker. A `dev` build with no brew formula is an error.
pub fn detect_channel(
    formulae: Option<&[String]>,
    binary: Option<Channel>,
) -> Result<Detected, CliError> {
    if let Some(list) = formulae {
        let has_preview = list.iter().any(|f| f == Channel::Preview.formula());
        let has_stable = list.iter().any(|f| f == Channel::Stable.formula());
        match (has_stable, has_preview) {
            (true, true) => {
                return Ok(Detected {
                    channel: Channel::Preview,
                    source: DetectSource::Brew,
                    warning: Some(
                        "both llmux and llmux-preview are installed via brew; \
                         preview wins — run `llmux channel stable` to consolidate"
                            .into(),
                    ),
                })
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

/// PURE: the ordered `brew` commands to switch from `current` to `target`.
/// The binary formulae both link `bin/llmux`, so the OLD one is uninstalled
/// BEFORE the new one is installed — that removes the symlink and sidesteps a
/// brew link conflict entirely (the running process keeps its open file).
/// When the target formula is ALREADY installed (the both-installed state
/// `detect_channel` warns about), `brew install` is a no-op that does NOT
/// restore the `bin/llmux` symlink the uninstall just removed — so the plan
/// relinks instead. The Islands cask is mirrored only when a cask is
/// installed and not already on the target channel. Assumes
/// `current != target` (the no-op case is handled by the command, not the
/// plan).
pub fn switch_plan(
    current: Channel,
    target: Channel,
    islands: Option<Channel>,
    target_installed: bool,
) -> Vec<BrewCmd> {
    let mut cmds = vec![BrewCmd::new(["update"])];
    // Formula: uninstall the outgoing channel, then install (or relink) the
    // target.
    cmds.push(BrewCmd::new(["uninstall", current.formula()]));
    if target_installed {
        cmds.push(BrewCmd::new(["link", "--overwrite", target.formula()]));
    } else {
        cmds.push(BrewCmd::new(["install", target.formula()]));
    }
    // Islands cask mirror — only if installed and on the wrong channel.
    if let Some(installed) = islands {
        if installed != target {
            cmds.push(BrewCmd::new([
                "uninstall",
                "--cask",
                installed.islands_cask(),
            ]));
            cmds.push(BrewCmd::new(["install", "--cask", target.islands_cask()]));
        }
    }
    cmds
}

/// PURE: the `brew` commands to consolidate the both-installed state when the
/// user re-selects the channel they are already on. `detect_channel` makes
/// preview win whenever both formulae are installed, so `llmux channel
/// preview` would otherwise no-op forever while the "both installed" warning
/// keeps firing on every command. Remove the stray formula, then relink the
/// target (the uninstall may have owned the shared `bin/llmux` symlink).
pub fn consolidate_plan(target: Channel) -> Vec<BrewCmd> {
    vec![
        BrewCmd::new(["uninstall", target.other().formula()]),
        BrewCmd::new(["link", "--overwrite", target.formula()]),
    ]
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
}

/// Execute a channel switch through `host`: capture the outgoing version, run
/// every planned `brew` command in order, capture the incoming version. The
/// daemon restart itself is async and lives in the command handler; this
/// returns whether one is warranted via `daemon_running`.
pub fn execute_switch(
    host: &dyn Host,
    current: Channel,
    target: Channel,
    islands: Option<Channel>,
    daemon_running: bool,
) -> Result<SwitchReport, CliError> {
    let old_version = host.installed_version(current.formula(), false);
    let target_installed = host.installed_version(target.formula(), false).is_some();
    for cmd in switch_plan(current, target, islands, target_installed) {
        host.run_brew(&cmd.args())?;
    }
    let new_version = host.installed_version(target.formula(), false);
    Ok(SwitchReport {
        from: current,
        to: target,
        old_version,
        new_version,
        daemon_running,
    })
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
        // `brew list --versions <name>` → "<name> <version> [<version>…]".
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .next()?
            .split_whitespace()
            .nth(1)
            .map(str::to_string)
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
        let output = std::process::Command::new("brew")
            .args(["--prefix", formula])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if prefix.is_empty() {
            return None;
        }
        let bin = std::path::PathBuf::from(prefix).join("bin").join("llmux");
        bin.exists().then_some(bin)
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
        /// Ordered log: `"brew <args>"` and `"relaunch-islands"`.
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
        fn run_brew(&self, args: &[&str]) -> Result<(), CliError> {
            self.calls
                .borrow_mut()
                .push(format!("brew {}", args.join(" ")));
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
        let d = detect_channel(Some(&f), None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, DetectSource::Brew);
        assert!(d.warning.is_some(), "both installed must warn");
    }

    #[test]
    fn detect_reads_single_installed_formula() {
        let stable = detect_channel(Some(&strs(&["llmux", "jq"])), None).unwrap();
        assert_eq!(stable.channel, Channel::Stable);
        assert!(stable.warning.is_none());

        let preview = detect_channel(Some(&strs(&["llmux-preview"])), None).unwrap();
        assert_eq!(preview.channel, Channel::Preview);
    }

    #[test]
    fn detect_does_not_confuse_preview_for_stable() {
        // Only preview installed: the exact-match must NOT count `llmux` as
        // present just because `llmux-preview` contains it.
        let d = detect_channel(Some(&strs(&["llmux-preview"])), None).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert!(d.warning.is_none(), "not both-installed");
    }

    #[test]
    fn detect_falls_back_to_binary_marker_when_brew_silent() {
        // brew unavailable → use the build marker.
        let d = detect_channel(None, Some(Channel::Preview)).unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, DetectSource::BinaryFallback);

        // brew present but neither formula installed → also fall back.
        let d = detect_channel(Some(&strs(&["jq"])), Some(Channel::Stable)).unwrap();
        assert_eq!(d.channel, Channel::Stable);
        assert_eq!(d.source, DetectSource::BinaryFallback);
    }

    #[test]
    fn detect_errors_on_dev_build_with_no_brew_formula() {
        let err = detect_channel(None, None).unwrap_err();
        assert!(err.to_string().contains("cannot determine release channel"));
    }

    #[test]
    fn switch_plan_uninstalls_old_before_installing_new() {
        let plan = switch_plan(Channel::Stable, Channel::Preview, None, false);
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["uninstall", "llmux"])),
                BrewCmd(strs(&["install", "llmux-preview"])),
            ]
        );
    }

    #[test]
    fn switch_plan_relinks_when_target_already_installed() {
        // Both-installed state: `brew install` would be a no-op warning and
        // leave the target UNLINKED after the uninstall removed the shared
        // `bin/llmux` symlink — the plan must relink instead.
        let plan = switch_plan(Channel::Preview, Channel::Stable, None, true);
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["uninstall", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
            ]
        );
    }

    #[test]
    fn consolidate_plan_removes_stray_and_relinks_target() {
        // Re-selecting the current channel in the both-installed state must
        // remove the OTHER formula and restore the target's symlink.
        assert_eq!(
            consolidate_plan(Channel::Preview),
            vec![
                BrewCmd(strs(&["uninstall", "llmux"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux-preview"])),
            ]
        );
        assert_eq!(
            consolidate_plan(Channel::Stable),
            vec![
                BrewCmd(strs(&["uninstall", "llmux-preview"])),
                BrewCmd(strs(&["link", "--overwrite", "llmux"])),
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
        );
        assert_eq!(
            plan,
            vec![
                BrewCmd(strs(&["update"])),
                BrewCmd(strs(&["uninstall", "llmux-preview"])),
                BrewCmd(strs(&["install", "llmux"])),
                BrewCmd(strs(&["uninstall", "--cask", "llmux-islands-preview"])),
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
                "brew update",
                "brew uninstall llmux",
                "brew install llmux-preview",
            ]
        );
    }

    #[test]
    fn execute_switch_relinks_instead_of_installing_when_target_present() {
        // The exact both-installed state from the field: preview + stable
        // both listed, switching preview → stable. `brew install llmux`
        // would no-op and leave `bin/llmux` unlinked after the uninstall.
        let host = FakeHost::new().with_versions(&[
            ("llmux", false, "0.2.16"),
            ("llmux-preview", false, "2026.07.10.0734"),
        ]);
        let report = execute_switch(&host, Channel::Preview, Channel::Stable, None, true).unwrap();
        assert_eq!(report.new_version.as_deref(), Some("0.2.16"));
        assert_eq!(
            *host.calls.borrow(),
            vec![
                "brew update",
                "brew uninstall llmux-preview",
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
}
