//! Linux desktop adapters with deterministic, dependency-light behavior.

use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    net::{IpAddr, TcpStream, ToSocketAddrs},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

const AUTOSTART_FILE: &str = "io.twolab.LlmuxIslands.desktop";
const MAX_TEMP_ATTEMPTS: usize = 32;
static AUTOSTART_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Changed,
    Unchanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutostartState {
    Missing,
    Installed,
    Drifted,
}

/// Idempotent writer for the user's XDG autostart entry.
pub struct AutostartManager {
    entry_path: PathBuf,
}

impl AutostartManager {
    pub fn from_env() -> io::Result<Self> {
        let config_home = match env::var_os("XDG_CONFIG_HOME") {
            Some(path) => PathBuf::from(path),
            None => env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "HOME and XDG_CONFIG_HOME are unset",
                    )
                })?,
        };
        Ok(Self::new(config_home))
    }

    #[must_use]
    pub fn new(config_home: impl Into<PathBuf>) -> Self {
        Self {
            entry_path: config_home.into().join("autostart").join(AUTOSTART_FILE),
        }
    }

    #[must_use]
    pub fn entry_path(&self) -> &Path {
        &self.entry_path
    }

    pub fn install(&self, executable: &Path) -> io::Result<Change> {
        let desired = autostart_entry(executable);
        let parent = self.entry_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "autostart path has no parent")
        })?;
        ensure_nonsymlink_directory(parent)?;
        reject_unsafe_entry(&self.entry_path)?;
        if read_nonsymlink_text(&self.entry_path)?.as_deref() == Some(desired.as_str()) {
            return Ok(Change::Unchanged);
        }

        let (temporary, mut file) = create_autostart_temp(parent)?;
        let mut cleanup = AutostartTempCleanup(Some(temporary.clone()));
        file.write_all(desired.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, &self.entry_path)?;
        cleanup.0 = None;
        File::open(parent)?.sync_all()?;
        Ok(Change::Changed)
    }

    pub fn remove(&self) -> io::Result<Change> {
        if !nonsymlink_parent_exists(&self.entry_path)? {
            return Ok(Change::Unchanged);
        }
        reject_unsafe_entry(&self.entry_path)?;
        match fs::remove_file(&self.entry_path) {
            Ok(()) => Ok(Change::Changed),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Change::Unchanged),
            Err(error) => Err(error),
        }
    }

    pub fn readback(&self, executable: &Path) -> io::Result<AutostartState> {
        if !nonsymlink_parent_exists(&self.entry_path)? {
            return Ok(AutostartState::Missing);
        }
        match read_nonsymlink_text(&self.entry_path)? {
            Some(current) if current == autostart_entry(executable) => {
                Ok(AutostartState::Installed)
            }
            Some(_) => Ok(AutostartState::Drifted),
            None => Ok(AutostartState::Missing),
        }
    }
}

fn nonsymlink_parent_exists(path: &Path) -> io::Result<bool> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "autostart path has no parent")
    })?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(unsafe_autostart_path()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn unsafe_autostart_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "unsafe autostart path")
}

fn ensure_nonsymlink_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(unsafe_autostart_path());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(unsafe_autostart_path())
    }
}

fn reject_unsafe_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(unsafe_autostart_path()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_nonsymlink_text(path: &Path) -> io::Result<Option<String>> {
    reject_unsafe_entry(path)?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(unsafe_autostart_path());
        }
        Err(error) => return Err(error),
    };
    let mut content = String::new();
    std::io::Read::read_to_string(&mut file, &mut content)?;
    Ok(Some(content))
}

fn create_autostart_temp(parent: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let id = AUTOSTART_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{AUTOSTART_FILE}.{}.{}.tmp",
            std::process::id(),
            id
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create autostart temporary file",
    ))
}

struct AutostartTempCleanup(Option<PathBuf>);

impl Drop for AutostartTempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn autostart_entry(executable: &Path) -> String {
    let executable = executable
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "[Desktop Entry]\nType=Application\nName=llmux Islands\nExec=\"{executable}\"\nIcon=io.twolab.LlmuxIslands\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoopbackEndpoint {
    host: String,
    port: u16,
}

/// Return true only for an unencrypted HTTP endpoint whose host is loopback.
#[must_use]
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    parse_loopback_endpoint(endpoint).is_some()
}

fn parse_loopback_endpoint(endpoint: &str) -> Option<LoopbackEndpoint> {
    let rest = endpoint.strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    if authority.contains('@') {
        return None;
    }

    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']')?;
        let port = suffix.strip_prefix(':')?.parse().ok()?;
        (host, port)
    } else {
        let (host, port) = authority.rsplit_once(':')?;
        (host, port.parse().ok()?)
    };

    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    loopback.then(|| LoopbackEndpoint {
        host: host.to_owned(),
        port,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonStartReceipt {
    RejectedNonLoopback,
    AlreadyRunning,
    Started,
    MissingSibling,
    SpawnedButUnreachable,
}

/// Probe a local daemon and start the sibling `llmux` binary only for loopback endpoints.
pub fn ensure_sibling_daemon(endpoint: &str) -> io::Result<DaemonStartReceipt> {
    let Some(endpoint) = parse_loopback_endpoint(endpoint) else {
        return Ok(DaemonStartReceipt::RejectedNonLoopback);
    };
    if probe_loopback(&endpoint, Duration::from_millis(150)) {
        return Ok(DaemonStartReceipt::AlreadyRunning);
    }

    let sibling = env::current_exe()?
        .parent()
        .map(|parent| parent.join("llmux"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "executable has no parent"))?;
    if !sibling.is_file() {
        return Ok(DaemonStartReceipt::MissingSibling);
    }

    sibling_daemon_command(&sibling, endpoint.port).spawn()?;

    for _ in 0..20 {
        if probe_loopback(&endpoint, Duration::from_millis(150)) {
            return Ok(DaemonStartReceipt::Started);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(DaemonStartReceipt::SpawnedButUnreachable)
}

fn sibling_daemon_command(sibling: &Path, port: u16) -> Command {
    let mut command = Command::new(sibling);
    // The endpoint port has already been parsed as a u16. Pass it as a direct
    // argv value so the sibling follows the real `llmux server --port` CLI
    // contract without evaluating any endpoint text through a shell.
    command
        .arg("server")
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-tui")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn probe_loopback(endpoint: &LoopbackEndpoint, timeout: Duration) -> bool {
    let addresses = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    addresses.into_iter().any(|address| {
        let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(timeout));
        let request = format!(
            "GET /llmux/status HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
            endpoint.host, endpoint.port
        );
        if stream.write_all(request.as_bytes()).is_err() {
            return false;
        }
        let mut status = String::new();
        BufReader::new(stream)
            .read_line(&mut status)
            .is_ok_and(|count| count > 0 && status.contains(" 200 "))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotificationReceipt {
    pub notification_sent: bool,
    pub sound_played: bool,
}

/// Command adapter for the freedesktop notification and sound-theme implementations.
pub struct FreedesktopNotifier {
    notify_send: PathBuf,
    sound_player: Option<PathBuf>,
}

impl FreedesktopNotifier {
    #[must_use]
    pub fn discover() -> Option<Self> {
        Some(Self {
            notify_send: command_in_path("notify-send")?,
            sound_player: command_in_path("canberra-gtk-play"),
        })
    }

    #[must_use]
    pub fn with_commands(notify_send: PathBuf, sound_player: Option<PathBuf>) -> Self {
        Self {
            notify_send,
            sound_player,
        }
    }

    pub fn notify(
        &self,
        summary: &str,
        body: &str,
        sound_event: Option<&str>,
    ) -> io::Result<NotificationReceipt> {
        let notification_sent = Command::new(&self.notify_send)
            .args([
                "--app-name=llmux Islands",
                "--icon=io.twolab.LlmuxIslands",
                summary,
                body,
            ])
            .status()?
            .success();

        let sound_played = match (sound_event, &self.sound_player) {
            (Some(event), Some(player)) => Command::new(player)
                .args(["--id", event, "--description", summary])
                .status()?
                .success(),
            _ => false,
        };

        Ok(NotificationReceipt {
            notification_sent,
            sound_played,
        })
    }
}

fn command_in_path(command: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(command))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{
        is_loopback_endpoint, parse_loopback_endpoint, probe_loopback, sibling_daemon_command,
        AutostartManager, AutostartState, Change, FreedesktopNotifier, NotificationReceipt,
    };
    use std::{
        env, fs,
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::symlink,
        path::PathBuf,
        thread,
        time::Duration,
    };

    fn temporary_root(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "llmux-islands-linux-{label}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn autostart_install_remove_and_readback_are_idempotent() {
        let root = temporary_root("autostart");
        let _ = fs::remove_dir_all(&root);
        let manager = AutostartManager::new(&root);
        let executable = PathBuf::from("/opt/llmux/bin/llmux-islands-linux");

        assert_eq!(manager.install(&executable).unwrap(), Change::Changed);
        assert_eq!(manager.install(&executable).unwrap(), Change::Unchanged);
        assert_eq!(
            manager.readback(&executable).unwrap(),
            AutostartState::Installed
        );
        assert_eq!(manager.remove().unwrap(), Change::Changed);
        assert_eq!(manager.remove().unwrap(), Change::Unchanged);
        assert_eq!(
            manager.readback(&executable).unwrap(),
            AutostartState::Missing
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn autostart_install_rejects_symlink_targets_and_parents() {
        let root = temporary_root("autostart-symlink");
        let _ = fs::remove_dir_all(&root);
        let manager = AutostartManager::new(&root);
        let executable = PathBuf::from("/opt/llmux/bin/llmux-islands-linux");
        let parent = manager.entry_path().parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let outside = root.join("outside.desktop");
        fs::write(&outside, "sentinel").unwrap();
        symlink(&outside, manager.entry_path()).unwrap();

        assert_eq!(
            manager.install(&executable).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "sentinel");

        fs::remove_file(manager.entry_path()).unwrap();
        fs::remove_dir_all(parent).unwrap();
        let outside_directory = root.join("outside-directory");
        fs::create_dir_all(&outside_directory).unwrap();
        symlink(&outside_directory, parent).unwrap();

        assert_eq!(
            manager.install(&executable).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            manager.readback(&executable).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            manager.remove().unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(fs::read_dir(&outside_directory).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_autostart_gate_accepts_only_loopback_http() {
        assert!(is_loopback_endpoint("http://127.0.0.1:3456"));
        assert!(is_loopback_endpoint("http://[::1]:3456"));
        assert!(is_loopback_endpoint("http://localhost:3456"));
        assert!(!is_loopback_endpoint("https://127.0.0.1:3456"));
        assert!(!is_loopback_endpoint("http://192.0.2.4:3456"));
        assert!(!is_loopback_endpoint("http://user@127.0.0.1:3456"));
    }

    #[test]
    fn daemon_spawn_uses_the_selected_port_as_a_typed_cli_argument() {
        let endpoint = parse_loopback_endpoint("http://127.0.0.1:45678").unwrap();
        let command = sibling_daemon_command(&PathBuf::from("/opt/llmux/bin/llmux"), endpoint.port);

        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(arguments, ["server", "--port", "45678", "--no-tui"]);
    }

    #[test]
    fn ipv6_loopback_probe_reads_the_status_receipt() {
        let Ok(listener) = TcpListener::bind("[::1]:0") else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        });
        let endpoint = parse_loopback_endpoint(&format!("http://[::1]:{port}")).unwrap();

        assert!(probe_loopback(&endpoint, Duration::from_secs(1)));
        server.join().unwrap();
    }

    #[test]
    fn notification_adapter_reports_both_process_results() {
        let notifier = FreedesktopNotifier::with_commands(
            PathBuf::from("/usr/bin/true"),
            Some(PathBuf::from("/usr/bin/true")),
        );
        assert_eq!(
            notifier
                .notify("llmux", "request complete", Some("complete"))
                .unwrap(),
            NotificationReceipt {
                notification_sent: true,
                sound_played: true
            }
        );
    }
}
