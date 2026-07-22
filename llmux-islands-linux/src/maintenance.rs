//! Package-owner-aware maintenance for the Linux Islands shell.
//!
//! Arch packages own `/usr` files. This adapter therefore never invokes
//! pacman, sudo, or an AUR helper: package-managed installations receive a
//! precise instruction which the UI records as a terminal no-change receipt.
//! Homebrew installations delegate to the existing `llmux` maintenance CLI.
//! Self-managed installs must supply a verified artifact and are constrained
//! to a caller-provided user-owned root.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

const MAX_TEMP_ATTEMPTS: usize = 32;
static UPDATE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOwner {
    Pacman { package: String },
    Homebrew,
    SelfManaged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallEvidence {
    pub executable: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub pacman_package: Option<String>,
    pub homebrew_prefixes: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceIntent {
    Update,
    ChangeChannel(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceDisposition {
    Completed,
    Instruction,
    VerifiedArtifactRequired,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub owner: InstallOwner,
    pub disposition: MaintenanceDisposition,
    pub message: String,
    /// A non-privileged command the caller may execute after confirmation.
    /// Package-manager instructions deliberately leave this as `None`.
    pub command: Option<Vec<OsString>>,
}

pub fn classify_install_owner(evidence: &InstallEvidence) -> InstallOwner {
    if let Some(package) = evidence
        .pacman_package
        .as_deref()
        .map(str::trim)
        .filter(|package| !package.is_empty())
    {
        return InstallOwner::Pacman {
            package: package.to_string(),
        };
    }

    if evidence
        .homebrew_prefixes
        .iter()
        .any(|prefix| evidence.executable.starts_with(prefix))
        || evidence
            .executable
            .components()
            .any(|component| matches!(component, Component::Normal(value) if value == "Cellar" || value == ".linuxbrew"))
    {
        return InstallOwner::Homebrew;
    }

    if evidence.home_dir.as_deref().is_some_and(|home| {
        evidence.executable.starts_with(home.join(".local/bin"))
            || evidence
                .executable
                .starts_with(home.join(".local/lib/llmux"))
    }) {
        return InstallOwner::SelfManaged;
    }

    InstallOwner::Unknown
}

pub fn plan_maintenance(owner: &InstallOwner, intent: MaintenanceIntent) -> MaintenanceReport {
    let channel = match &intent {
        MaintenanceIntent::ChangeChannel(channel)
            if !matches!(channel.as_str(), "stable" | "preview") =>
        {
            return MaintenanceReport {
                owner: owner.clone(),
                disposition: MaintenanceDisposition::Failed,
                message: "Unsupported release channel; no files were changed".to_string(),
                command: None,
            };
        }
        MaintenanceIntent::ChangeChannel(channel) => Some(channel.as_str()),
        MaintenanceIntent::Update => None,
    };

    match owner {
        InstallOwner::Pacman { package } => {
            let message = match channel {
                None => format!(
                    "Package-managed install. Run `sudo pacman -Syu {package}` (or update it with the AUR helper that installed it). No files were changed"
                ),
                Some("stable") => "Package-managed install. Install `llmux-islands` and the matching stable `llmux` package with pacman or your AUR helper. No files were changed".to_string(),
                Some("preview") => "Package-managed install. Install `llmux-islands-preview` and the matching preview `llmux` package with pacman or your AUR helper. No files were changed".to_string(),
                Some(_) => unreachable!("channel was validated above"),
            };
            MaintenanceReport {
                owner: owner.clone(),
                disposition: MaintenanceDisposition::Instruction,
                message,
                command: None,
            }
        }
        InstallOwner::Homebrew => {
            let mut command = Vec::new();
            match channel {
                None => command.push(OsString::from("update")),
                Some(channel) => {
                    command.push(OsString::from("channel"));
                    command.push(OsString::from(channel));
                }
            }
            MaintenanceReport {
                owner: owner.clone(),
                disposition: MaintenanceDisposition::Completed,
                message: "Ready to delegate maintenance to the installed llmux CLI".to_string(),
                command: Some(command),
            }
        }
        InstallOwner::SelfManaged => MaintenanceReport {
            owner: owner.clone(),
            disposition: MaintenanceDisposition::VerifiedArtifactRequired,
            message: "A signed release manifest and matching SHA-256 artifact are required before a self-managed install can be changed; no files were changed".to_string(),
            command: None,
        },
        InstallOwner::Unknown => MaintenanceReport {
            owner: owner.clone(),
            disposition: MaintenanceDisposition::Failed,
            message: "Could not determine the install owner; no files were changed".to_string(),
            command: None,
        },
    }
}

/// Inspect the running executable without changing machine state.
pub fn inspect_install_owner(executable: &Path) -> InstallOwner {
    let pacman_package = Command::new("pacman")
        .args(["-Qo", "--"])
        .arg(executable)
        .env("LC_ALL", "C")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_pacman_owner(&String::from_utf8_lossy(&output.stdout)));
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    let mut homebrew_prefixes = vec![
        PathBuf::from("/home/linuxbrew/.linuxbrew"),
        PathBuf::from("/opt/homebrew"),
        PathBuf::from("/usr/local/Homebrew"),
    ];
    if let Some(home) = &home_dir {
        homebrew_prefixes.push(home.join(".linuxbrew"));
    }
    classify_install_owner(&InstallEvidence {
        executable: executable.to_path_buf(),
        home_dir,
        pacman_package,
        homebrew_prefixes,
    })
}

/// Execute only a non-privileged Homebrew delegation. The CLI is resolved
/// through the absolute Homebrew installation prefix; `$PATH` is never used.
/// Pacman and self-managed
/// reports remain non-mutating instructions until the caller supplies the
/// separately verified artifact path.
pub fn execute_maintenance(executable: &Path, intent: MaintenanceIntent) -> MaintenanceReport {
    let owner = inspect_install_owner(executable);
    let mut report = plan_maintenance(&owner, intent);
    let Some(command) = report.command.take() else {
        return report;
    };
    if command.is_empty() {
        report.disposition = MaintenanceDisposition::Failed;
        report.message = "Invalid maintenance command; no files were changed".to_string();
        return report;
    }
    let Some(program) = resolve_homebrew_llmux(executable) else {
        report.disposition = MaintenanceDisposition::Failed;
        report.message =
            "Could not verify the installed Homebrew llmux command; no files were changed"
                .to_string();
        return report;
    };
    match Command::new(program).args(&command).output() {
        Ok(output) if output.status.success() => {
            report.disposition = MaintenanceDisposition::Completed;
            report.message = summarized_process_output(&output.stdout, "Maintenance completed");
        }
        Ok(output) => {
            report.disposition = MaintenanceDisposition::Failed;
            report.message = summarized_process_output(&output.stderr, "Maintenance failed");
        }
        Err(_) => {
            report.disposition = MaintenanceDisposition::Failed;
            report.message = "Could not launch the installed llmux maintenance command".to_string();
        }
    }
    report
}

fn resolve_homebrew_llmux(executable: &Path) -> Option<PathBuf> {
    let prefix = infer_homebrew_prefix(executable)?;
    let canonical_prefix = prefix.canonicalize().ok()?;
    let brew = canonical_prefix.join("bin/brew");
    if !brew.is_file() {
        return None;
    }
    let output = Command::new(&brew)
        .args(["--prefix", "llmux"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&output.stdout).ok()?;
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let formula_prefix = PathBuf::from(lines.next()?);
    if lines.next().is_some() || !formula_prefix.is_absolute() {
        return None;
    }
    let canonical_formula = formula_prefix.canonicalize().ok()?;
    if !canonical_formula.starts_with(&canonical_prefix) {
        return None;
    }
    let cli = canonical_formula.join("bin/llmux").canonicalize().ok()?;
    (cli.starts_with(&canonical_prefix) && cli.is_file()).then_some(cli)
}

fn infer_homebrew_prefix(executable: &Path) -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/home/linuxbrew/.linuxbrew"),
        PathBuf::from("/opt/homebrew"),
        PathBuf::from("/usr/local/Homebrew"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".linuxbrew"));
    }
    if let Some(candidate) = candidates
        .into_iter()
        .find(|candidate| executable.starts_with(candidate))
    {
        return Some(candidate);
    }
    executable
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "Cellar"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// Verify and atomically replace a self-managed executable below `user_root`.
/// The digest is checked before the destination or a temporary file is touched.
pub fn install_verified_artifact(
    bytes: &[u8],
    expected_sha256: &str,
    destination: &Path,
    user_root: &Path,
) -> Result<(), MaintenanceError> {
    let expected = normalize_digest(expected_sha256)?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(MaintenanceError::new("artifact checksum did not match"));
    }

    let canonical_root = canonical_user_owned_root(user_root)?;
    let relative = destination
        .strip_prefix(user_root)
        .map_err(|_| MaintenanceError::new("destination is outside the user-owned install root"))?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(MaintenanceError::new(
            "destination is outside the user-owned install root",
        ));
    }
    let canonical_destination = canonical_root.join(relative);
    let parent = canonical_destination
        .parent()
        .ok_or_else(|| MaintenanceError::new("destination has no parent directory"))?;
    reject_symlink_components(&canonical_root, parent)?;
    fs::create_dir_all(parent)
        .map_err(|_| MaintenanceError::new("could not create the install directory"))?;
    reject_symlink_components(&canonical_root, parent)?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|_| MaintenanceError::new("could not verify the install directory"))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(MaintenanceError::new(
            "destination is outside the user-owned install root",
        ));
    }
    require_current_user_owner(&canonical_parent)?;
    if fs::symlink_metadata(&canonical_destination)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(MaintenanceError::new("destination must not be a symlink"));
    }

    let file_name = canonical_destination
        .file_name()
        .ok_or_else(|| MaintenanceError::new("destination has no file name"))?
        .to_string_lossy();
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let id = UPDATE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temporary =
            canonical_parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), id));
        match write_and_replace(bytes, &temporary, &canonical_destination, &canonical_parent) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            result => {
                if result.is_err() {
                    let _ = fs::remove_file(&temporary);
                }
                return result.map_err(|error| error.into_maintenance());
            }
        }
    }
    Err(MaintenanceError::new(
        "could not create the atomic update file",
    ))
}

fn canonical_user_owned_root(user_root: &Path) -> Result<PathBuf, MaintenanceError> {
    let metadata = fs::symlink_metadata(user_root)
        .map_err(|_| MaintenanceError::new("user-owned install root is unavailable"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MaintenanceError::new(
            "user-owned install root must not be a symlink",
        ));
    }
    require_current_user_owner(user_root)?;
    let canonical = user_root
        .canonicalize()
        .map_err(|_| MaintenanceError::new("user-owned install root is unavailable"))?;
    let system_roots = [
        Path::new("/bin"),
        Path::new("/boot"),
        Path::new("/etc"),
        Path::new("/lib"),
        Path::new("/lib64"),
        Path::new("/opt"),
        Path::new("/sbin"),
        Path::new("/usr"),
    ];
    if canonical == Path::new("/") || system_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(MaintenanceError::new(
            "system paths are not a user-owned install root",
        ));
    }
    Ok(canonical)
}

fn require_current_user_owner(path: &Path) -> Result<(), MaintenanceError> {
    let metadata = fs::metadata(path)
        .map_err(|_| MaintenanceError::new("could not verify user-owned install path"))?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() == effective_uid {
        Ok(())
    } else {
        Err(MaintenanceError::new(
            "install path is not owned by the current user",
        ))
    }
}

fn reject_symlink_components(root: &Path, parent: &Path) -> Result<(), MaintenanceError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| MaintenanceError::new("destination is outside the user-owned install root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(MaintenanceError::new(
                "destination is outside the user-owned install root",
            ));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MaintenanceError::new(
                    "install path must not contain symlinks",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(MaintenanceError::new(
                    "install path must contain only directories",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(_) => {
                return Err(MaintenanceError::new(
                    "could not verify the install directory",
                ));
            }
        }
    }
    Ok(())
}

fn write_and_replace(
    bytes: &[u8],
    temporary: &Path,
    destination: &Path,
    parent: &Path,
) -> Result<(), AtomicReplaceError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o700)
        .open(temporary)
        .map_err(AtomicReplaceError::Create)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| AtomicReplaceError::Other("could not write the atomic update file"))?;
    fs::set_permissions(temporary, fs::Permissions::from_mode(0o755))
        .map_err(|_| AtomicReplaceError::Other("could not set executable permissions"))?;
    fs::rename(temporary, destination)
        .map_err(|_| AtomicReplaceError::Other("could not atomically install the update"))?;
    // The namespace change has already happened after a successful rename.
    // A directory-fsync failure can only make crash durability uncertain; it
    // must not be reported as if the previous executable were still active.
    let _ = sync_directory(parent);
    Ok(())
}

enum AtomicReplaceError {
    Create(io::Error),
    Other(&'static str),
}

impl AtomicReplaceError {
    fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Create(error) => error.kind(),
            Self::Other(_) => io::ErrorKind::Other,
        }
    }

    fn into_maintenance(self) -> MaintenanceError {
        match self {
            Self::Create(_) => MaintenanceError::new("could not create the atomic update file"),
            Self::Other(message) => MaintenanceError::new(message),
        }
    }
}

fn sync_directory(path: &Path) -> Result<(), MaintenanceError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| MaintenanceError::new("could not sync the install directory"))
}

fn normalize_digest(value: &str) -> Result<String, MaintenanceError> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MaintenanceError::new("invalid SHA-256 checksum"));
    }
    Ok(value)
}

fn parse_pacman_owner(output: &str) -> Option<String> {
    let owner = output.split_once(" is owned by ")?.1.trim();
    let mut words = owner.split_whitespace();
    let package = words.next()?.trim();
    (!package.is_empty()).then(|| package.to_string())
}

fn summarized_process_output(output: &[u8], fallback: &str) -> String {
    let text = String::from_utf8_lossy(output);
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fallback);
    let lowered = line.to_ascii_lowercase();
    if [
        "authorization",
        "bearer ",
        "api_key",
        "api-key",
        "x-api-key",
        "access_token",
        "refresh_token",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        "[REDACTED]".to_string()
    } else {
        line.chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect()
    }
}

#[derive(Debug)]
pub struct MaintenanceError {
    message: &'static str,
    source: Option<io::Error>,
}

impl MaintenanceError {
    fn new(message: &'static str) -> Self {
        Self {
            message,
            source: None,
        }
    }
}

impl fmt::Display for MaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for MaintenanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_pacman_owner;

    #[test]
    fn parses_the_package_name_from_pacman_qo() {
        assert_eq!(
            parse_pacman_owner(
                "/usr/bin/llmux-islands-linux is owned by llmux-islands-git 0.1.0-1\n"
            ),
            Some("llmux-islands-git".to_string())
        );
        assert_eq!(
            parse_pacman_owner("llmux-islands-git 0.1.0-1 owns /usr/bin/llmux-islands-linux\n"),
            None,
            "unexpected formats must fail safe"
        );
    }
}
