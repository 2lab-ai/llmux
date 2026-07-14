//! Explicit, deterministic GUI snapshot launch configuration.

use std::{
    env,
    ffi::OsString,
    fmt, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const SNAPSHOT_NOW_MS: u64 = 1_700_000_000_000;
pub const SNAPSHOT_SURFACES: [&str; 3] = ["usage", "statistics", "menu"];

static ACTIVE_REQUEST: OnceLock<Option<SnapshotRequest>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRequest {
    output_directory: PathBuf,
    qml_output_directory: String,
}

impl SnapshotRequest {
    #[must_use]
    pub fn output_directory(&self) -> &Path {
        &self.output_directory
    }

    #[must_use]
    pub fn qml_output_directory(&self) -> &str {
        &self.qml_output_directory
    }

    /// Configure Qt before QApplication is created. Snapshot mode is an
    /// explicit headless render target, so inherited display settings must not
    /// make its output depend on the caller's desktop session.
    pub fn configure_headless_environment(&self) {
        env::set_var("QT_QPA_PLATFORM", "offscreen");
        env::set_var("QT_QUICK_BACKEND", "software");
        env::set_var("QSG_RHI_BACKEND", "software");
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotError(String);

impl SnapshotError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SnapshotError {}

/// Parse and prepare the optional snapshot request from process arguments.
/// Unknown arguments remain available to the normal Qt application path.
pub fn request_from_args(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Option<SnapshotRequest>, SnapshotError> {
    let mut arguments = arguments.into_iter();
    let mut output_directory = None;
    let mut smoke_test = false;

    while let Some(argument) = arguments.next() {
        if argument == "--smoke-test" {
            smoke_test = true;
            continue;
        }
        if argument != "--snapshot-dir" {
            continue;
        }
        if output_directory.is_some() {
            return Err(SnapshotError::new(
                "--snapshot-dir may only be provided once",
            ));
        }
        let Some(directory) = arguments.next() else {
            return Err(SnapshotError::new(
                "--snapshot-dir requires an output directory",
            ));
        };
        if directory.is_empty() {
            return Err(SnapshotError::new(
                "--snapshot-dir requires a non-empty output directory",
            ));
        }
        output_directory = Some(PathBuf::from(directory));
    }

    let Some(output_directory) = output_directory else {
        return Ok(None);
    };
    if smoke_test {
        return Err(SnapshotError::new(
            "--snapshot-dir and --smoke-test are mutually exclusive",
        ));
    }

    prepare_request(output_directory).map(Some)
}

fn prepare_request(output_directory: PathBuf) -> Result<SnapshotRequest, SnapshotError> {
    fs::create_dir_all(&output_directory).map_err(|error| {
        SnapshotError::new(format!(
            "could not create snapshot directory {}: {error}",
            output_directory.display()
        ))
    })?;
    let metadata = fs::metadata(&output_directory).map_err(|error| {
        SnapshotError::new(format!(
            "could not inspect snapshot directory {}: {error}",
            output_directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(SnapshotError::new(format!(
            "snapshot output is not a directory: {}",
            output_directory.display()
        )));
    }
    let output_directory = fs::canonicalize(&output_directory).map_err(|error| {
        SnapshotError::new(format!(
            "could not resolve snapshot directory {}: {error}",
            output_directory.display()
        ))
    })?;
    let qml_output_directory = output_directory
        .to_str()
        .ok_or_else(|| SnapshotError::new("snapshot directory must be valid UTF-8"))?
        .to_string();

    Ok(SnapshotRequest {
        output_directory,
        qml_output_directory,
    })
}

pub fn configure(request: Option<SnapshotRequest>) -> Result<(), SnapshotError> {
    ACTIVE_REQUEST
        .set(request)
        .map_err(|_| SnapshotError::new("snapshot launch configuration was already initialized"))
}

#[must_use]
pub fn active() -> Option<&'static SnapshotRequest> {
    ACTIVE_REQUEST.get().and_then(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::{request_from_args, SNAPSHOT_SURFACES};
    use std::{ffi::OsString, fs};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_explicit_snapshot_flag_preserves_the_live_launch_path() {
        assert!(request_from_args(args(&[]))
            .expect("empty arguments")
            .is_none());
        assert!(request_from_args(args(&["--unknown"]))
            .expect("unknown arguments remain available to Qt")
            .is_none());
    }

    #[test]
    fn explicit_snapshot_request_prepares_one_utf8_directory() {
        let temporary = tempfile_path("prepared");
        let request = request_from_args(vec![
            OsString::from("--snapshot-dir"),
            temporary.clone().into_os_string(),
        ])
        .expect("valid snapshot arguments")
        .expect("snapshot request");

        assert!(request.output_directory().is_dir());
        assert!(!request.qml_output_directory().is_empty());
        assert_eq!(SNAPSHOT_SURFACES, ["usage", "statistics", "menu"]);

        fs::remove_dir_all(request.output_directory()).expect("remove temporary directory");
    }

    #[test]
    fn ambiguous_or_incomplete_snapshot_modes_are_rejected() {
        assert!(request_from_args(args(&["--snapshot-dir"])).is_err());
        assert!(request_from_args(args(&[
            "--snapshot-dir",
            "/tmp/one",
            "--snapshot-dir",
            "/tmp/two"
        ]))
        .is_err());
        assert!(
            request_from_args(args(&["--smoke-test", "--snapshot-dir", "/tmp/snapshot"])).is_err()
        );
    }

    #[test]
    fn snapshot_output_must_be_a_creatable_directory() {
        let file = tempfile_path("not-a-directory");
        fs::write(&file, b"occupied").expect("create non-directory fixture");
        let result = request_from_args(vec![
            OsString::from("--snapshot-dir"),
            file.clone().into_os_string(),
        ]);
        assert!(result.is_err());
        fs::remove_file(file).expect("remove non-directory fixture");
    }

    fn tempfile_path(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "llmux-islands-snapshot-{label}-{}-{}-{nonce}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ))
    }
}
