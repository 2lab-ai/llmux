use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use llmux_islands_core::{ClientConfig, ClientErrorKind, ReleaseChannel, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3456";
pub const DEFAULT_SOUND_ID: &str = "message-new-instant";

const SETTINGS_DIRECTORY: &str = "llmux";
const SETTINGS_DOCUMENT: &str = "islands.json";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_DOCUMENT_MODE: u32 = 0o600;
const MAX_TEMP_ATTEMPTS: usize = 32;

static TEMP_DOCUMENT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(SecretString);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::new(value))
    }

    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl Serialize for ApiKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for ApiKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalSettings {
    pub selected_screen_id: String,
    pub sound_id: String,
    pub show_fable_weekly: bool,
    pub endpoint: String,
    pub api_key: Option<ApiKey>,
    pub release_channel: ReleaseChannel,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            selected_screen_id: String::new(),
            sound_id: DEFAULT_SOUND_ID.to_string(),
            show_fable_weekly: true,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: None,
            release_channel: ReleaseChannel::Stable,
        }
    }
}

impl fmt::Debug for LocalSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalSettings")
            .field(
                "selected_screen_id_configured",
                &!self.selected_screen_id.is_empty(),
            )
            .field("sound_id_configured", &!self.sound_id.is_empty())
            .field("show_fable_weekly", &self.show_fable_weekly)
            .field("endpoint_configured", &!self.endpoint.is_empty())
            .field("api_key_configured", &self.api_key.is_some())
            .field("release_channel", &self.release_channel)
            .finish()
    }
}

impl LocalSettings {
    /// Validate the stored endpoint independently of authentication. An
    /// explicitly cleared remote key is a valid, visibly unauthenticated
    /// configuration; request execution still fails closed until a key is set.
    pub fn validate(&self) -> Result<(), SettingsError> {
        ClientConfig::new(&self.endpoint)
            .map(|_| ())
            .map_err(SettingsError::from_client_kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Written,
    WrittenDurabilityUnknown,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsErrorKind {
    ConfigHomeUnavailable,
    InvalidEndpoint,
    InsecureEndpoint,
    MissingApiKey,
    InvalidSettings,
    CorruptDocument,
    UnsafeDocument,
    Io,
}

pub struct SettingsError {
    kind: SettingsErrorKind,
}

impl SettingsError {
    const fn new(kind: SettingsErrorKind) -> Self {
        Self { kind }
    }

    fn from_client_kind(error: llmux_islands_core::ClientError) -> Self {
        let kind = match error.kind() {
            ClientErrorKind::InvalidEndpoint => SettingsErrorKind::InvalidEndpoint,
            ClientErrorKind::InsecureEndpoint => SettingsErrorKind::InsecureEndpoint,
            ClientErrorKind::MissingApiKey => SettingsErrorKind::MissingApiKey,
            _ => SettingsErrorKind::InvalidSettings,
        };
        Self::new(kind)
    }

    fn from_io(_: io::Error) -> Self {
        Self::new(SettingsErrorKind::Io)
    }

    pub const fn kind(&self) -> SettingsErrorKind {
        self.kind
    }

    const fn message(&self) -> &'static str {
        match self.kind {
            SettingsErrorKind::ConfigHomeUnavailable => {
                "the XDG configuration directory is unavailable"
            }
            SettingsErrorKind::InvalidEndpoint => "the daemon endpoint is invalid",
            SettingsErrorKind::InsecureEndpoint => "a remote daemon endpoint requires HTTPS",
            SettingsErrorKind::MissingApiKey => "a remote daemon requires an API key",
            SettingsErrorKind::InvalidSettings => "the local settings are invalid",
            SettingsErrorKind::CorruptDocument => "the local settings document is invalid",
            SettingsErrorKind::UnsafeDocument => {
                "the local settings path or permissions are unsafe"
            }
            SettingsErrorKind::Io => "the local settings operation failed",
        }
    }
}

impl fmt::Debug for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsError")
            .field("kind", &self.kind)
            .field("message", &self.message())
            .finish()
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SettingsError {}

#[derive(Clone, PartialEq, Eq)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Resolve `$XDG_CONFIG_HOME/llmux/islands.json`, falling back to
    /// `$HOME/.config/llmux/islands.json` as required by the XDG base-directory spec.
    pub fn discover() -> Result<Self, SettingsError> {
        let config_home = config_home(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))?;
        Ok(Self::from_config_home(config_home))
    }

    pub fn from_config_home(config_home: impl AsRef<Path>) -> Self {
        Self::from_path(
            config_home
                .as_ref()
                .join(SETTINGS_DIRECTORY)
                .join(SETTINGS_DOCUMENT),
        )
    }

    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LocalSettings, SettingsError> {
        validate_private_parent_if_present(&self.path)?;
        let document = match read_private_document(&self.path)? {
            Some(document) => document,
            None => return Ok(LocalSettings::default()),
        };
        let settings = serde_json::from_slice::<LocalSettings>(&document)
            .map_err(|_| SettingsError::new(SettingsErrorKind::CorruptDocument))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, settings: &LocalSettings) -> Result<SaveOutcome, SettingsError> {
        settings.validate()?;

        validate_replaceable_target(&self.path)?;

        if self.is_unchanged_private_document(settings) {
            return Ok(SaveOutcome::Unchanged);
        }

        let mut document = Zeroizing::new(
            serde_json::to_vec_pretty(settings)
                .map_err(|_| SettingsError::new(SettingsErrorKind::InvalidSettings))?,
        );
        document.push(b'\n');

        let parent = self
            .path
            .parent()
            .ok_or_else(|| SettingsError::new(SettingsErrorKind::Io))?;
        ensure_private_directory(parent)?;

        let (temp_path, mut temp_file) = create_private_temp_document(parent)?;
        let mut cleanup = TempCleanup::new(temp_path.clone());
        write_private_document(&mut temp_file, &document)?;
        drop(temp_file);
        verify_readback(&temp_path, settings)?;

        fs::rename(&temp_path, &self.path).map_err(SettingsError::from_io)?;
        cleanup.disarm();
        // The private mode was set and verified on the same-filesystem temp
        // file before rename, so no fallible permission mutation is needed
        // after the namespace change. A directory-fsync failure means the new
        // document is visible now but crash durability could not be confirmed.
        Ok(if sync_directory(parent).is_ok() {
            SaveOutcome::Written
        } else {
            SaveOutcome::WrittenDurabilityUnknown
        })
    }

    fn is_unchanged_private_document(&self, settings: &LocalSettings) -> bool {
        let Ok(current) = self.load() else {
            return false;
        };
        if current != *settings {
            return false;
        }
        let Some(parent) = self.path.parent() else {
            return false;
        };
        private_document(&self.path) && private_directory(parent)
    }
}

fn config_home(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, SettingsError> {
    if let Some(path) = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".config"))
        .ok_or_else(|| SettingsError::new(SettingsErrorKind::ConfigHomeUnavailable))
}

fn private_document(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.permissions().mode() & 0o777 == PRIVATE_DOCUMENT_MODE
    })
}

fn private_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_dir() && metadata.permissions().mode() & 0o777 == PRIVATE_DIRECTORY_MODE
    })
}

fn ensure_private_directory(path: &Path) -> Result<(), SettingsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(SettingsError::new(SettingsErrorKind::UnsafeDocument));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(SettingsError::from_io(error)),
    }
    fs::create_dir_all(path).map_err(SettingsError::from_io)?;
    let metadata = fs::symlink_metadata(path).map_err(SettingsError::from_io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(SettingsError::new(SettingsErrorKind::UnsafeDocument));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
        .map_err(SettingsError::from_io)?;
    if private_directory(path) {
        Ok(())
    } else {
        Err(SettingsError::new(SettingsErrorKind::UnsafeDocument))
    }
}

fn validate_private_parent_if_present(path: &Path) -> Result<(), SettingsError> {
    let parent = path
        .parent()
        .ok_or_else(|| SettingsError::new(SettingsErrorKind::UnsafeDocument))?;
    match fs::symlink_metadata(parent) {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o777 == PRIVATE_DIRECTORY_MODE =>
        {
            Ok(())
        }
        Ok(_) => Err(SettingsError::new(SettingsErrorKind::UnsafeDocument)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SettingsError::from_io(error)),
    }
}

fn validate_replaceable_target(path: &Path) -> Result<(), SettingsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(SettingsError::new(SettingsErrorKind::UnsafeDocument)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SettingsError::from_io(error)),
    }
}

fn read_private_document(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, SettingsError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SettingsError::from_io(error)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != PRIVATE_DOCUMENT_MODE
    {
        return Err(SettingsError::new(SettingsErrorKind::UnsafeDocument));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                SettingsError::new(SettingsErrorKind::UnsafeDocument)
            } else {
                SettingsError::from_io(error)
            }
        })?;
    let opened = file.metadata().map_err(SettingsError::from_io)?;
    if !opened.is_file() || opened.permissions().mode() & 0o777 != PRIVATE_DOCUMENT_MODE {
        return Err(SettingsError::new(SettingsErrorKind::UnsafeDocument));
    }
    let mut document = Zeroizing::new(Vec::new());
    file.read_to_end(&mut document)
        .map_err(SettingsError::from_io)?;
    Ok(Some(document))
}

fn create_private_temp_document(parent: &Path) -> Result<(PathBuf, File), SettingsError> {
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let id = TEMP_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{SETTINGS_DOCUMENT}.{}.{}.tmp",
            std::process::id(),
            id
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_DOCUMENT_MODE)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SettingsError::from_io(error)),
        }
    }
    Err(SettingsError::new(SettingsErrorKind::Io))
}

fn write_private_document(file: &mut File, document: &[u8]) -> Result<(), SettingsError> {
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_DOCUMENT_MODE))
        .map_err(SettingsError::from_io)?;
    file.write_all(document).map_err(SettingsError::from_io)?;
    file.sync_all().map_err(SettingsError::from_io)
}

fn verify_readback(path: &Path, expected: &LocalSettings) -> Result<(), SettingsError> {
    let document = read_private_document(path)?
        .ok_or_else(|| SettingsError::new(SettingsErrorKind::CorruptDocument))?;
    let actual = serde_json::from_slice::<LocalSettings>(&document)
        .map_err(|_| SettingsError::new(SettingsErrorKind::CorruptDocument))?;
    actual.validate()?;
    if actual == *expected {
        Ok(())
    } else {
        Err(SettingsError::new(SettingsErrorKind::CorruptDocument))
    }
}

fn sync_directory(path: &Path) -> Result<(), SettingsError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(SettingsError::from_io)
}

struct TempCleanup {
    path: PathBuf,
    armed: bool,
}

impl TempCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
