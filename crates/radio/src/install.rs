//! The per-user install handshake, with launchd and socket probes supplied by the caller.
use crate::log::{Field, append};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, OsString},
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FILES: &str = "the install files could not be written";
const FRESH_FAILED: &str = "Radio did not start, so nothing is installed";
const QUIET: &str = "the installed Radio does not answer";
const OTHER_HOME: &str = "HOME is not this account's own, so no real launch agent is loaded from it; set RADIO_LAUNCHCTL to a stand-in";

/// The per-user launch agent label.
pub const LABEL: &str = "ai.awna.radio";

/// Install locations beneath one account home.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The account home from which every install path is derived.
    pub home: PathBuf,
    /// The private directory holding records, rooms and the socket.
    pub state: PathBuf,
    /// The exclusive install lock file.
    pub lock: PathBuf,
    /// The installed binary record.
    pub record: PathBuf,
    /// The directory of client registration records.
    pub clients: PathBuf,
    /// The daemon log, shared by stdout and stderr.
    pub log: PathBuf,
    /// The destination rooms store, retained on uninstall.
    pub rooms: PathBuf,
    /// The daemon socket probed during startup and status checks.
    pub socket: PathBuf,
    /// The parent of the versioned binary directories.
    pub binaries: PathBuf,
    /// The per-user launch agent plist.
    pub agent: PathBuf,
}

impl Paths {
    /// Derives install paths from an absolute, UTF-8 home path.
    pub fn new(home: &Path) -> Result<Self, String> {
        if !home.is_absolute() || home.to_str().is_none() {
            return Err("HOME is not a plain path".into());
        }
        let state = home.join(".local/state/radio");
        Ok(Self {
            home: home.into(),
            lock: state.join("install.lock"),
            record: state.join("install.json"),
            clients: state.join("clients"),
            log: state.join("radio.log"),
            rooms: state.join(radio_rooms::DB_FILE),
            socket: radio_client::socket_path(home),
            state,
            binaries: home.join(".local/libexec/radio"),
            agent: home.join("Library/LaunchAgents/ai.awna.radio.plist"),
        })
    }

    /// Returns the binary path for one version.
    pub fn binary(&self, version: &str) -> PathBuf {
        self.binaries.join(version).join("radio")
    }
}

/// Runs launchd commands without exposing process handling to the installer.
pub trait Launchd {
    /// Runs fixed launchctl arguments and reports whether the command succeeded.
    fn run(&self, args: &[&str]) -> Result<bool, String>;
}

/// Checks whether the daemon answers on its socket.
pub trait Probe {
    /// Returns the answering daemon version, or no answer.
    fn hello(&self, socket: &Path) -> Option<String>;
}

/// The real launchctl command or an explicitly supplied stand-in.
pub struct Launchctl {
    program: PathBuf,
}

impl Launchctl {
    /// Selects a stand-in, or real launchctl only for this account's own home.
    pub fn resolve(home: &Path, fake: Option<OsString>) -> Result<Self, String> {
        if let Some(program) = fake.filter(|value| !value.is_empty()) {
            return Ok(Self {
                program: program.into(),
            });
        }
        if account_home().as_deref() != Some(home) {
            return Err(OTHER_HOME.into());
        }
        Ok(Self {
            program: "/bin/launchctl".into(),
        })
    }
}

impl Launchd for Launchctl {
    fn run(&self, args: &[&str]) -> Result<bool, String> {
        Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .map_err(|_| "launchctl could not be run".into())
    }
}

/// Probes the daemon through the public hello request.
pub struct SocketProbe;

impl Probe for SocketProbe {
    fn hello(&self, socket: &Path) -> Option<String> {
        let response = radio_client::request(socket, &radio_client::Request::Hello {}).ok()?;
        response.ok.then_some(response.radio).flatten()
    }
}

/// Bounded waits and retries for install coordination and daemon startup.
#[derive(Clone, Copy)]
pub struct Timing {
    /// The maximum time to wait for another install to release its lock.
    pub lock_wait: Duration,
    /// The interval between lock attempts.
    pub lock_poll: Duration,
    /// The maximum time to wait for the expected daemon version.
    pub hello_wait: Duration,
    /// The interval between hello probes.
    pub hello_poll: Duration,
    /// The maximum number of checks that a service has stopped.
    pub bootout_checks: u32,
    /// The interval between stop checks.
    pub bootout_pause: Duration,
    /// The maximum number of bootstrap attempts.
    pub bootstrap_tries: u32,
    /// The interval between bootstrap attempts.
    pub bootstrap_pause: Duration,
}

impl Timing {
    /// Production wait and retry limits.
    pub const REAL: Self = Self {
        lock_wait: Duration::from_secs(120),
        lock_poll: Duration::from_millis(100),
        hello_wait: Duration::from_secs(10),
        hello_poll: Duration::from_millis(100),
        bootout_checks: 10,
        bootout_pause: Duration::from_millis(500),
        bootstrap_tries: 3,
        bootstrap_pause: Duration::from_secs(1),
    };
}

/// Coordinates a per-user install using supplied launchd and probe adapters.
pub struct Installer<'a> {
    /// The account's install locations.
    pub paths: Paths,
    /// The adapter used to start and stop the launch agent.
    pub launchd: &'a dyn Launchd,
    /// The adapter used to verify the daemon version.
    pub probe: &'a dyn Probe,
    /// The wait and retry limits for this operation.
    pub timing: Timing,
    /// The version of the binary being offered for installation.
    pub version: String,
    /// The canonical path of the binary being offered for installation.
    pub source: PathBuf,
    /// The account uid used for launchd targets and handler ownership checks.
    pub uid: u32,
}

/// The client registration and optional older store supplied by an app.
pub struct InstallArgs {
    /// The client name stored in its registration record.
    pub app: String,
    /// The client's own version, independent of the daemon version.
    pub app_version: String,
    /// The executable path supplied by the client, checked before any writes.
    pub handler: PathBuf,
    /// An optional older rooms store to import after installing.
    pub rooms: Option<PathBuf>,
}

/// How the offered binary affected the installed daemon.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// A new installation, including recovery from an invalid record or changed binary.
    Fresh { version: String },
    /// A newer daemon replaced the installed version.
    Upgraded { from: String, to: String },
    /// The existing daemon was at least as new as the offered version.
    Kept { installed: String },
}

/// The result of importing the optional older rooms store.
#[derive(Debug, PartialEq, Eq)]
pub enum RoomsOutcome {
    /// The store was imported, with counts of copied and skipped rows.
    Imported(radio_rooms::ImportReport),
    /// That source path was imported before.
    AlreadyImported,
    /// The install succeeded, but the import failed with this fixed phrase.
    Failed(String),
}

/// A successful installation and any recoveries or import outcome.
#[derive(Debug)]
pub struct InstallOutcome {
    /// Whether the daemon was installed, upgraded or kept.
    pub installed: Installed,
    /// The import result, when a source store was supplied.
    pub rooms: Option<RoomsOutcome>,
    /// An invalid install record required a fresh installation.
    pub record_unreadable: bool,
    /// A missing or changed installed binary required a fresh installation.
    pub binary_changed: bool,
}

/// A fixed user-facing failure phrase and the command exit code.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    /// The command exit code.
    pub code: i32,
    /// The fixed failure phrase, without a command prefix.
    pub message: String,
}

/// The result of removing one client registration.
#[derive(Debug, PartialEq, Eq)]
pub enum Uninstalled {
    /// No record existed for that client.
    NotAClient,
    /// The client was removed; last means the daemon was also stopped and removed.
    Removed { last: bool },
}

/// Validated installation and client records together with a live socket probe.
#[derive(Debug)]
pub struct StatusReport {
    /// The socket answers with the recorded installed version.
    pub ok: bool,
    /// The installed version, installing client and Unix timestamp, when valid.
    pub installation: Option<(String, String, u64)>,
    /// Client labels in filename order, including unreadable-record placeholders.
    pub clients: Vec<String>,
    /// The valid version reported by the socket, when it answers.
    pub radio: Option<String>,
}

/// Parses an exact three-part ASCII numeric version.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let mut part = || {
        let value = parts.next()?;
        (value.len() <= 20
            && !value.is_empty()
            && (value.len() == 1 || !value.starts_with('0'))
            && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
    };
    let result = (part()?, part()?, part()?);
    parts.next().is_none().then_some(result)
}

fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: sysconf and geteuid have no pointer preconditions.
    let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut size = if suggested > 0 {
        suggested as usize
    } else {
        16384
    };
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: entry, buffer and result are writable for their advertised sizes. The returned
        // strings remain in buffer and are copied before buffer is dropped.
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE {
            size = size.checked_mul(2)?;
            continue;
        }
        if code != 0 || result.is_null() {
            return None;
        }

        // SAFETY: a successful non-null result initialized entry; pw_dir is a NUL-terminated string.
        let bytes = unsafe { CStr::from_ptr(entry.assume_init().pw_dir) }.to_bytes();
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Renders the launch agent plist with escaped binary and log paths.
pub fn plist(binary: &Path, log: &Path) -> String {
    let binary = xml(&binary.to_string_lossy());
    let log = xml(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>ai.awna.radio</string>
	<key>ProgramArguments</key>
	<array>
		<string>{binary}</string>
		<string>serve</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Background</string>
	<key>Umask</key>
	<integer>63</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryRecord {
    version: String,
    sha256: String,
    path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRecord {
    version: String,
    sha256: String,
    path: String,
    installed_by: String,
    at: u64,
    previous: Option<BinaryRecord>,
}

impl InstallRecord {
    fn binary(&self) -> BinaryRecord {
        BinaryRecord {
            version: self.version.clone(),
            sha256: self.sha256.clone(),
            path: self.path.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRecord {
    app: String,
    app_version: String,
    majors: Vec<u32>,
    handler: String,
    at: u64,
}

fn app_name(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 64
        && (app.as_bytes()[0].is_ascii_lowercase() || app.as_bytes()[0].is_ascii_digit())
        && app.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn app_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

fn valid_binary(record: &BinaryRecord, paths: &Paths) -> bool {
    parse_version(&record.version).is_some()
        && Path::new(&record.path) == paths.binary(&record.version)
        && record.sha256.len() == 64
        && record
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_plain(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct ReadInstall {
    record: Option<InstallRecord>,
    record_unreadable: bool,
    binary_changed: bool,
}

fn read_install(paths: &Paths) -> ReadInstall {
    let mut result = ReadInstall {
        record: None,
        record_unreadable: false,
        binary_changed: false,
    };
    if fs::symlink_metadata(&paths.record).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return result;
    }
    let record = read_plain(&paths.record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InstallRecord>(&bytes).ok())
        .filter(|record| {
            valid_binary(&record.binary(), paths)
                && app_name(&record.installed_by)
                && record
                    .previous
                    .as_ref()
                    .is_none_or(|previous| valid_binary(previous, paths))
        });
    let Some(record) = record else {
        result.record_unreadable = true;
        return result;
    };
    if !read_plain(&paths.binary(&record.version)).is_ok_and(|bytes| hash(&bytes) == record.sha256)
    {
        result.binary_changed = true;
    }
    result.record = Some(record);
    result
}

impl Failure {
    fn new(code: i32, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { code: 1, message }
    }
}

// The file's lifetime is the lock's lifetime, including every early return.
fn lock(paths: &Paths, timing: Timing) -> Result<fs::File, Failure> {
    radio_rooms::private_dir(&paths.state, true).map_err(|_| Failure::new(1, FILES))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&paths.lock)
        .map_err(|_| Failure::new(1, FILES))?;
    if !file
        .metadata()
        .map_err(|_| Failure::new(1, FILES))?
        .is_file()
    {
        return Err(Failure::new(1, FILES));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| Failure::new(1, FILES))?;
    let start = Instant::now();
    loop {
        // SAFETY: file is live; flock has no pointer preconditions.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let code = std::io::Error::last_os_error().raw_os_error();
        if !matches!(code, Some(libc::EWOULDBLOCK) | Some(libc::EINTR)) {
            return Err(Failure::new(1, FILES));
        }
        if start.elapsed() >= timing.lock_wait {
            return Err(Failure::new(1, "another install did not finish in time"));
        }
        thread::sleep(
            timing
                .lock_poll
                .min(timing.lock_wait.saturating_sub(start.elapsed())),
        );
    }
}

fn public_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(FILES.into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(path)
            .map_err(|_| FILES.into()),
        Err(_) => Err(FILES.into()),
    }
}

// An absent directory is safe to create later; a link is never a directory here.
fn real_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(FILES.into()),
    }
}

fn numbered_temp(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn remove_temps(dir: &Path, matches: impl Fn(&str) -> bool) -> Result<(), String> {
    real_directory(dir)?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(FILES.into()),
    };
    for entry in entries {
        let entry = entry.map_err(|_| FILES)?;
        if entry.file_name().to_str().is_some_and(&matches)
            && entry.file_type().map_err(|_| FILES)?.is_file()
        {
            remove_file(&entry.path())?;
        }
    }
    Ok(())
}

// O_EXCL protects a stale temp file; never unlink a file we did not create.
fn atomic_write(path: &Path, temp: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(temp)
        .map_err(|_| FILES)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|_| FILES.into())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(FILES)?;
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    atomic_write(
        path,
        &temp,
        &serde_json::to_vec(value).map_err(|_| FILES)?,
        0o600,
    )
}

fn remove_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(FILES.into()),
    }
}

fn canonical_missing(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    let parent = canonical_missing(path.parent()?)?;
    Some(parent.join(path.file_name()?))
}

fn store_location(path: &Path) -> Option<PathBuf> {
    Some(canonical_missing(path.parent()?)?.join(path.file_name()?))
}

fn handler_folder_safe(owner: u32, group: u32, mode: u32, uid: u32) -> bool {
    (owner == uid || owner == 0)
        && (mode & 0o020 == 0 || matches!(group, 0 | 80))
        && (mode & 0o002 == 0 || mode & 0o1000 != 0)
}

fn validate(args: &InstallArgs, paths: &Paths, uid: u32) -> Result<String, Failure> {
    let refuse = |phrase| Failure::new(2, phrase);
    if !app_name(&args.app) {
        return Err(refuse("not an app name"));
    }
    if !app_version(&args.app_version) {
        return Err(refuse("not a version"));
    }
    if !args.handler.is_absolute() {
        return Err(refuse("the handler is not an absolute path"));
    }
    let canonical = args
        .handler
        .canonicalize()
        .map_err(|_| refuse("the handler is not an executable file"))?;
    let handler = canonical
        .to_str()
        .ok_or_else(|| refuse("the handler is not a plain path"))?
        .to_owned();
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&canonical)
        .map_err(|_| refuse("the handler is not an executable file"))?;
    let meta = file
        .metadata()
        .map_err(|_| refuse("the handler is not an executable file"))?;
    if !meta.is_file() || meta.mode() & 0o100 == 0 {
        return Err(refuse("the handler is not an executable file"));
    }
    if meta.mode() & 0o022 != 0 || (meta.uid() != uid && meta.uid() != 0) {
        return Err(refuse("the handler can be changed by another user"));
    }
    for folder in canonical.ancestors().skip(1) {
        let meta = fs::symlink_metadata(folder)
            .map_err(|_| refuse("the handler can be changed by another user"))?;
        if !meta.is_dir() || !handler_folder_safe(meta.uid(), meta.gid(), meta.mode(), uid) {
            return Err(refuse("the handler can be changed by another user"));
        }
    }
    if let Some(rooms) = &args.rooms {
        if !rooms.is_absolute() {
            return Err(refuse("the store to import is not an absolute path"));
        }
        if store_location(rooms)
            .zip(store_location(&paths.rooms))
            .is_some_and(|(a, b)| a == b)
            || fs::metadata(rooms)
                .ok()
                .zip(fs::metadata(&paths.rooms).ok())
                .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
        {
            return Err(refuse("the store to import is Radio's own"));
        }
    }
    Ok(handler)
}

impl Installer<'_> {
    /// Installs or keeps the newest daemon, registers the client, then imports rooms.
    pub fn install(&self, args: &InstallArgs, now: u64) -> Result<InstallOutcome, Failure> {
        let result = self.install_inner(args, now);
        let app = if app_name(&args.app) {
            &args.app
        } else {
            "unknown"
        };
        match &result {
            Ok(outcome) => match &outcome.installed {
                Installed::Fresh { version } => append(
                    &self.paths.log,
                    "install",
                    "installed",
                    &[("version", Field::Text(version)), ("app", Field::Text(app))],
                ),
                Installed::Upgraded { from, to } => append(
                    &self.paths.log,
                    "install",
                    "upgraded",
                    &[
                        ("from", Field::Text(from)),
                        ("to", Field::Text(to)),
                        ("app", Field::Text(app)),
                    ],
                ),
                Installed::Kept { installed } => append(
                    &self.paths.log,
                    "install",
                    "kept",
                    &[
                        ("installed", Field::Text(installed)),
                        ("app", Field::Text(app)),
                    ],
                ),
            },
            Err(failure) => append(
                &self.paths.log,
                "install",
                "failed",
                &[
                    ("app", Field::Text(app)),
                    ("error", Field::Text(&failure.message)),
                    ("code", Field::Number(failure.code as u64)),
                ],
            ),
        }
        result
    }

    fn install_inner(&self, args: &InstallArgs, now: u64) -> Result<InstallOutcome, Failure> {
        let handler = validate(args, &self.paths, self.uid)?;
        let version = parse_version(&self.version)
            .ok_or_else(|| Failure::new(1, "this build has no valid version"))?;
        let domain = self.domain();
        if !self
            .launchd
            .run(&["print", &domain])
            .map_err(|_| Failure::new(1, "launchctl could not be run"))?
        {
            return Err(Failure::new(3, "no GUI session for this user"));
        }
        let _lock = lock(&self.paths, self.timing)?;
        self.binary_directories()?;
        self.clean_temps()?;
        let read = read_install(&self.paths);
        if read.binary_changed
            && read.record.as_ref().is_some_and(|record| {
                parse_version(&record.version).is_some_and(|old| old > version)
            })
        {
            return Err(Failure::new(
                1,
                "the installed Radio is newer than this copy and its binary is missing or changed",
            ));
        }
        let installed = match read.record.filter(|_| !read.binary_changed) {
            None => {
                match fs::read_dir(&self.paths.binaries) {
                    Ok(entries) => {
                        for entry in entries {
                            let entry = entry.map_err(|_| Failure::new(1, FILES))?;
                            if fs::symlink_metadata(entry.path())
                                .map_err(|_| Failure::new(1, FILES))?
                                .is_dir()
                                && entry
                                    .file_name()
                                    .to_str()
                                    .and_then(parse_version)
                                    .is_some_and(|old| old > version)
                            {
                                return Err(Failure::new(
                                    1,
                                    "a newer Radio is installed here and its record is missing or damaged",
                                ));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Failure::new(1, FILES)),
                }
                if radio_rooms::store_is_newer(&self.paths.rooms)
                    .map_err(|_| Failure::new(1, FILES))?
                {
                    return Err(Failure::new(1, "the room store is newer than this build"));
                }
                let created = match fs::symlink_metadata(
                    self.paths
                        .binary(&self.version)
                        .parent()
                        .ok_or(FILES.to_owned())?,
                ) {
                    Ok(_) => false,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(_) => return Err(Failure::new(1, FILES)),
                };
                let result = self
                    .place_binary(&self.source, &self.version)
                    .and_then(|sha| {
                        self.write_agent(&self.version)?;
                        self.create_log()?;
                        self.restart(&self.version)
                            .then_some(sha)
                            .ok_or(FRESH_FAILED.into())
                    });
                let sha256 = match result {
                    Ok(sha) => sha,
                    Err(_) => {
                        self.clean_fresh(created)?;
                        return Err(Failure::new(1, FRESH_FAILED));
                    }
                };
                let record = self.new_record(args, now, sha256, None);
                if write_json(&self.paths.record, &record).is_err() {
                    self.clean_fresh(created)?;
                    return Err(Failure::new(1, FRESH_FAILED));
                }
                self.clean_versions(&[&self.version], Some(version))?;
                Installed::Fresh {
                    version: self.version.clone(),
                }
            }
            Some(record) if parse_version(&record.version).is_some_and(|old| old >= version) => {
                if !self.wait_hello(&record.version) {
                    let target = self.target();
                    let started = match self.launchd.run(&["print", &target]) {
                        Ok(true) => {
                            let _ = self.launchd.run(&["kickstart", "-k", &target]);
                            true
                        }
                        Ok(false) => {
                            self.write_agent(&record.version)
                                .and_then(|()| self.create_log())
                                .is_ok()
                                && self.bootstrap()
                        }
                        Err(_) => false,
                    };
                    if !started || !self.wait_hello(&record.version) {
                        return Err(Failure::new(1, QUIET));
                    }
                }
                Installed::Kept {
                    installed: record.version,
                }
            }
            Some(record) => {
                let sha256 = self.place_binary(&self.source, &self.version)?;
                let snapshot = self.paths.state.join("rooms.db.before-upgrade");
                let mut had_store = None;
                let stopped = self.bootout();
                let started = stopped
                    && (|| {
                        remove_file(&snapshot)?;
                        let exists = match fs::symlink_metadata(&self.paths.rooms) {
                            Ok(_) => true,
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                            Err(_) => return Err(FILES.to_owned()),
                        };
                        if exists {
                            radio_rooms::snapshot_store(&self.paths.rooms, &snapshot)?;
                        }
                        had_store = Some(exists);
                        self.write_agent(&self.version)?;
                        self.create_log()?;
                        Ok(())
                    })()
                    .is_ok()
                    && self.bootstrap()
                    && self.wait_hello(&self.version);
                let new = self.new_record(args, now, sha256, Some(record.binary()));
                if !started || write_json(&self.paths.record, &new).is_err() {
                    let rollback_stopped = (stopped && had_store.is_none()) || self.bootout();
                    let restored = rollback_stopped
                        && match had_store {
                            Some(true) => {
                                radio_rooms::restore_store(&snapshot, &self.paths.rooms).is_ok()
                            }
                            Some(false) => self.remove_store().is_ok(),
                            None => true,
                        }
                        && self.write_agent(&record.version).is_ok()
                        && self.bootstrap()
                        && self.wait_hello(&record.version);
                    if rollback_stopped {
                        self.binary_directories()?;
                        let _ = fs::remove_dir_all(
                            self.paths
                                .binary(&self.version)
                                .parent()
                                .ok_or_else(|| Failure::new(1, FILES))?,
                        );
                    }
                    // A failed snapshot can leave a partial copy, never used for rollback.
                    if had_store.is_none() {
                        let _ = remove_file(&snapshot);
                    }
                    return Err(Failure::new(
                        1,
                        if restored {
                            "the upgrade failed; the version before it is running again"
                        } else {
                            "the upgrade failed, and the version before it did not start again"
                        },
                    ));
                }
                remove_file(&snapshot)?;
                self.clean_versions(&[&self.version, &record.version], None)?;
                Installed::Upgraded {
                    from: record.version,
                    to: self.version.clone(),
                }
            }
        };
        radio_rooms::private_dir(&self.paths.clients, false).map_err(|_| Failure::new(1, FILES))?;
        write_json(
            &self.paths.clients.join(format!("{}.json", args.app)),
            &ClientRecord {
                app: args.app.clone(),
                app_version: args.app_version.clone(),
                majors: vec![radio_client::PROTOCOL_MAJOR],
                handler,
                at: now,
            },
        )?;
        let rooms = args.rooms.as_ref().map(|source| {
            match radio_rooms::RoomStore::open(&self.paths.rooms)
                .and_then(|mut store| store.import_rooms(source, now as f64))
            {
                Ok(radio_rooms::Import::Imported(report)) => RoomsOutcome::Imported(report),
                Ok(radio_rooms::Import::AlreadyImported) => RoomsOutcome::AlreadyImported,
                Err(phrase) => RoomsOutcome::Failed(phrase),
            }
        });
        Ok(InstallOutcome {
            installed,
            rooms,
            record_unreadable: read.record_unreadable,
            binary_changed: read.binary_changed,
        })
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self) -> String {
        format!("{}/{LABEL}", self.domain())
    }

    fn new_record(
        &self,
        args: &InstallArgs,
        now: u64,
        sha256: String,
        previous: Option<BinaryRecord>,
    ) -> InstallRecord {
        InstallRecord {
            version: self.version.clone(),
            sha256,
            path: self
                .paths
                .binary(&self.version)
                .to_string_lossy()
                .into_owned(),
            installed_by: args.app.clone(),
            at: now,
            previous,
        }
    }

    fn place_binary(&self, source: &Path, version: &str) -> Result<String, String> {
        if parse_version(version).is_none() {
            return Err("this build has no valid version".into());
        }
        self.binary_directories()?;
        let bytes = fs::read(source).map_err(|_| FILES)?;
        public_dir(self.paths.binaries.parent().ok_or(FILES)?)?;
        radio_rooms::private_dir(&self.paths.binaries, false).map_err(|_| FILES)?;
        let binary = self.paths.binary(version);
        let dir = binary.parent().ok_or(FILES)?;
        radio_rooms::private_dir(dir, false).map_err(|_| FILES)?;
        let temp = dir.join(format!("radio.{}.tmp", std::process::id()));
        atomic_write(&binary, &temp, &bytes, 0o700)?;
        Ok(hash(&bytes))
    }

    fn write_agent(&self, version: &str) -> Result<(), String> {
        public_dir(self.paths.agent.parent().ok_or(FILES)?)?;
        let temp = self
            .paths
            .agent
            .with_file_name(format!(".{LABEL}.plist.{}.tmp", std::process::id()));
        atomic_write(
            &self.paths.agent,
            &temp,
            plist(&self.paths.binary(version), &self.paths.log).as_bytes(),
            0o644,
        )
    }

    fn create_log(&self) -> Result<(), String> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.paths.log)
            .map_err(|_| FILES)?;
        if !file.metadata().map_err(|_| FILES)?.is_file() {
            return Err(FILES.into());
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| FILES.into())
    }

    fn bootout(&self) -> bool {
        let target = self.target();
        let _ = self.launchd.run(&["bootout", &target]);
        for check in 0..self.timing.bootout_checks {
            if matches!(self.launchd.run(&["print", &target]), Ok(false)) {
                return true;
            }
            if check + 1 < self.timing.bootout_checks {
                thread::sleep(self.timing.bootout_pause);
            }
        }
        false
    }

    fn bootstrap(&self) -> bool {
        let domain = self.domain();
        let Some(path) = self.paths.agent.to_str() else {
            return false;
        };
        for attempt in 0..self.timing.bootstrap_tries {
            if matches!(self.launchd.run(&["bootstrap", &domain, path]), Ok(true)) {
                return true;
            }
            if attempt + 1 < self.timing.bootstrap_tries {
                thread::sleep(self.timing.bootstrap_pause);
            }
        }
        false
    }

    fn wait_hello(&self, version: &str) -> bool {
        let start = Instant::now();
        loop {
            if self.probe.hello(&self.paths.socket).as_deref() == Some(version) {
                return true;
            }
            if start.elapsed() >= self.timing.hello_wait {
                return false;
            }
            thread::sleep(
                self.timing
                    .hello_poll
                    .min(self.timing.hello_wait.saturating_sub(start.elapsed())),
            );
        }
    }

    fn restart(&self, version: &str) -> bool {
        self.bootout() && self.bootstrap() && self.wait_hello(version)
    }

    fn binary_directories(&self) -> Result<(), String> {
        for path in [
            self.paths.binaries.parent().ok_or(FILES)?,
            &self.paths.binaries,
        ] {
            real_directory(path)?;
        }
        Ok(())
    }

    fn clean_temps(&self) -> Result<(), String> {
        remove_temps(&self.paths.state, |name| {
            numbered_temp(name, ".install.json.")
        })?;
        remove_temps(&self.paths.clients, |name| {
            let Some((app, tail)) = name
                .strip_prefix('.')
                .and_then(|name| name.rsplit_once(".json."))
            else {
                return false;
            };
            app_name(app) && numbered_temp(tail, "")
        })?;
        remove_temps(self.paths.agent.parent().ok_or(FILES)?, |name| {
            numbered_temp(name, ".ai.awna.radio.plist.")
        })?;
        if !self.paths.binaries.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.paths.binaries).map_err(|_| FILES)? {
            let entry = entry.map_err(|_| FILES)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| parse_version(name).is_some())
                && entry.file_type().map_err(|_| FILES)?.is_dir()
            {
                remove_temps(&entry.path(), |name| numbered_temp(name, "radio."))?;
            }
        }
        Ok(())
    }

    fn remove_store(&self) -> Result<(), String> {
        for name in ["rooms.db", "rooms.db-wal", "rooms.db-shm"] {
            remove_file(&self.paths.state.join(name))?;
        }
        Ok(())
    }

    fn clean_fresh(&self, created: bool) -> Result<(), Failure> {
        self.binary_directories()?;
        if !self.bootout() {
            return Err(Failure::new(
                1,
                "Radio did not start and could not be stopped; nothing was removed",
            ));
        }
        remove_file(&self.paths.agent)?;
        if created && let Some(dir) = self.paths.binary(&self.version).parent() {
            fs::remove_dir_all(dir)
                .or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
                .map_err(|_| Failure::new(1, FILES))?;
        }
        Ok(())
    }

    fn clean_versions(
        &self,
        keep: &[&str],
        older_than: Option<(u64, u64, u64)>,
    ) -> Result<(), String> {
        self.binary_directories()?;
        let entries = match fs::read_dir(&self.paths.binaries) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(FILES.into()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| FILES)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if parse_version(name)
                .is_some_and(|version| older_than.is_none_or(|ours| version < ours))
                && !keep.contains(&name)
                && fs::symlink_metadata(entry.path())
                    .map_err(|_| FILES)?
                    .is_dir()
            {
                fs::remove_dir_all(entry.path()).map_err(|_| FILES)?;
            }
        }
        Ok(())
    }

    /// Removes a client and stops the daemon only for the last client, retaining data.
    pub fn uninstall(&self, app: &str) -> Result<Uninstalled, Failure> {
        let result = self.uninstall_inner(app);
        let app = if app_name(app) { app } else { "unknown" };
        match &result {
            Ok(Uninstalled::Removed { last }) => append(
                &self.paths.log,
                "uninstall",
                if *last { "removed" } else { "client-removed" },
                &[("app", Field::Text(app))],
            ),
            Ok(Uninstalled::NotAClient) => {}
            Err(failure) => append(
                &self.paths.log,
                "uninstall",
                "failed",
                &[
                    ("app", Field::Text(app)),
                    ("error", Field::Text(&failure.message)),
                    ("code", Field::Number(failure.code as u64)),
                ],
            ),
        }
        result
    }

    fn uninstall_inner(&self, app: &str) -> Result<Uninstalled, Failure> {
        if !app_name(app) {
            return Err(Failure::new(2, "not an app name"));
        }
        let _lock = lock(&self.paths, self.timing)?;
        self.binary_directories()?;
        self.clean_temps()?;
        let client = self.paths.clients.join(format!("{app}.json"));
        match fs::symlink_metadata(&client) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Uninstalled::NotAClient);
            }
            Err(_) => return Err(Failure::new(1, FILES)),
            Ok(_) => {}
        }
        let mut last = true;
        for entry in fs::read_dir(&self.paths.clients).map_err(|_| Failure::new(1, FILES))? {
            let entry = entry.map_err(|_| Failure::new(1, FILES))?;
            if entry.path() != client
                && entry.path().extension().is_some_and(|ext| ext == "json")
                && fs::symlink_metadata(entry.path())
                    .map_err(|_| Failure::new(1, FILES))?
                    .is_file()
            {
                last = false;
            }
        }
        if last && !self.bootout() {
            return Err(Failure::new(1, "Radio could not be stopped"));
        }
        remove_file(&client)?;
        if last {
            remove_file(&self.paths.agent)?;
            self.clean_versions(&[], None)?;
            remove_file(&self.paths.record)?;
            match fs::read_dir(&self.paths.binaries) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        fs::remove_dir(&self.paths.binaries).map_err(|_| Failure::new(1, FILES))?;
                    }
                }
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Failure::new(1, FILES));
                }
                _ => {}
            }
        }
        Ok(Uninstalled::Removed { last })
    }

    /// Reads status using this installer's paths and probe, without launchd calls.
    pub fn status(&self) -> StatusReport {
        status(&self.paths, self.probe)
    }
}

/// Reads validated install records and probes the socket without consulting launchd.
pub fn status(paths: &Paths, probe: &dyn Probe) -> StatusReport {
    let read = read_install(paths);
    let installation = read
        .record
        .filter(|_| !read.binary_changed)
        .map(|record| (record.version, record.installed_by, record.at));
    let mut clients = Vec::new();
    if let Ok(entries) = fs::read_dir(&paths.clients) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let record = read_plain(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ClientRecord>(&bytes).ok())
                .filter(|record| {
                    app_name(&record.app)
                        && app_version(&record.app_version)
                        && record.majors == [radio_client::PROTOCOL_MAJOR]
                        && Path::new(&record.handler).is_absolute()
                        && path
                            .file_stem()
                            .is_some_and(|name| name == record.app.as_str())
                });
            let label = record
                .map(|record| format!("{} {}", record.app, record.app_version))
                .unwrap_or_else(|| "(an unreadable record)".into());
            clients.push((entry.file_name(), label));
        }
    }
    clients.sort_by(|a, b| a.0.cmp(&b.0));
    let radio = probe
        .hello(&paths.socket)
        .filter(|version| parse_version(version).is_some());
    let ok = installation
        .as_ref()
        .is_some_and(|record| radio.as_deref() == Some(&record.0));
    StatusReport {
        ok,
        installation,
        clients: clients.into_iter().map(|entry| entry.1).collect(),
        radio,
    }
}

/// Renders the installation, clients and socket as three status lines.
pub fn render(status: &StatusReport) -> String {
    let installed = match &status.installation {
        Some((version, app, at)) => format!(
            "radio {version}, installed by {app} at {}",
            crate::log::utc(*at)
        ),
        None => "radio: not installed".into(),
    };
    let clients = if status.clients.is_empty() {
        "none".into()
    } else {
        status.clients.join(", ")
    };
    let socket = status
        .radio
        .as_ref()
        .map(|version| format!("answers, radio {version}"))
        .unwrap_or_else(|| "no answer".into());
    format!("{installed}\nclients: {clients}\nsocket: {socket}\n")
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
