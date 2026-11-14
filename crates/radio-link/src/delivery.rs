use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
};

fn open_path(path: &Path) -> Result<OwnedFd, &'static str> {
    let mut bytes = path.as_os_str().as_bytes();
    // A trailing slash makes the final link an intermediate component for open.
    // Keep root intact, and leave ancestor components exactly as chosen.
    while bytes.len() > 1 && bytes.last() == Some(&b'/') {
        bytes = &bytes[..bytes.len() - 1];
    }
    let path = CString::new(bytes).map_err(|_| "the folder could not be opened")?;
    // SAFETY: path is NUL-terminated and valid for the call.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err("the folder could not be opened");
    }
    // SAFETY: open returned a new owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is live and stat points to writable storage of the required size.
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err("the folder could not be opened");
    }
    // SAFETY: fstat initialized stat on success.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err("the folder could not be opened");
    }
    // SAFETY: geteuid has no preconditions.
    if stat.st_uid != unsafe { libc::geteuid() } {
        return Err("the folder is not this user's");
    }
    if stat.st_mode & 0o022 != 0 {
        return Err("the folder is writable by others");
    }
    Ok(fd)
}

pub fn write(
    folder: &crate::fleet_db::DeliveryFolder,
    name: &str,
    body: impl FnMut(&mut File) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if !crate::wire::file_name(name) {
        return Err(crate::wire::WRITE);
    }
    let dir = prepare(folder, name)?;
    let (entries, bytes) = scan(&dir)?;
    fits(entries, bytes, 0, 0, 0, 0, available(&dir)?)?;
    write_open(&dir, name, body)
}

pub fn write_open(
    dir: &OwnedFd,
    name: &str,
    mut body: impl FnMut(&mut File) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if !crate::wire::file_name(name) {
        return Err(crate::wire::WRITE);
    }
    let name = CString::new(name).map_err(|_| crate::wire::WRITE)?;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| crate::wire::WRITE)?;
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary =
        CString::new(format!(".radio-partial-{hex}")).map_err(|_| crate::wire::WRITE)?;
    // SAFETY: dir is live, the name is NUL-terminated, and O_CREAT receives its mode.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(crate::wire::WRITE);
    }
    let _cleanup = Temporary {
        dir,
        name: &temporary,
    };
    // SAFETY: openat returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    body(&mut file)?;
    file.sync_all().map_err(|_| crate::wire::WRITE)?;
    // SAFETY: dir is live and both names are NUL-terminated.
    let result = unsafe {
        libc::linkat(
            dir.as_raw_fd(),
            temporary.as_ptr(),
            dir.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    };
    let error = std::io::Error::last_os_error();
    if result == 0 {
        Ok(())
    } else if error.raw_os_error() == Some(libc::EEXIST) {
        Err(crate::wire::EXISTS)
    } else {
        Err(crate::wire::WRITE)
    }
}

struct Temporary<'a> {
    dir: &'a OwnedFd,
    name: &'a CString,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        // SAFETY: the borrowed descriptor is live and name is a NUL-terminated relative name.
        unsafe { libc::unlinkat(self.dir.as_raw_fd(), self.name.as_ptr(), 0) };
    }
}

pub fn open_folder(folder: &crate::fleet_db::DeliveryFolder) -> Result<OwnedFd, &'static str> {
    let dir = open_path(&folder.path)?;
    let stat = folder_stat(&dir)?;
    if stat.st_dev as u64 != folder.dev || stat.st_ino != folder.ino {
        return Err("the folder could not be opened");
    }
    Ok(dir)
}

fn folder_stat(dir: &OwnedFd) -> Result<libc::stat, &'static str> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: the descriptor is live and stat points to writable storage.
    if unsafe { libc::fstat(dir.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(crate::wire::FOLDER);
    }
    // SAFETY: fstat initialized stat on success.
    Ok(unsafe { stat.assume_init() })
}

pub fn make_folder(path: &Path) -> Result<crate::fleet_db::DeliveryFolder, &'static str> {
    make_folder_in(path).map(|(folder, _, _)| folder)
}

pub(crate) fn make_folder_in(
    path: &Path,
) -> Result<(crate::fleet_db::DeliveryFolder, OwnedFd, CString), &'static str> {
    if !path.is_absolute() {
        return Err("the folder must be an absolute path");
    }
    let bytes = path.as_os_str().as_bytes();
    let end = bytes.iter().rposition(|b| *b != b'/').map_or(0, |i| i + 1);
    let bytes = &bytes[..end];
    let split = bytes
        .iter()
        .rposition(|b| *b == b'/')
        .ok_or("not a folder name")?;
    let last = &bytes[split + 1..];
    if last.is_empty() || last == b"." || last == b".." {
        return Err("not a folder name");
    }
    let parent = CString::new(if split == 0 {
        &b"/"[..]
    } else {
        &bytes[..split]
    })
    .map_err(|_| "not a folder name")?;
    let last = CString::new(last).map_err(|_| "not a folder name")?;
    // SAFETY: parent is a NUL-terminated path.
    let raw = unsafe {
        libc::open(
            parent.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err("the folder could not be made");
    }
    // SAFETY: open returned an owned descriptor.
    let parent = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: parent is live and last is a NUL-terminated single component.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), last.as_ptr(), 0o700) } != 0 {
        return Err(
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                "the folder already exists; name a new one"
            } else {
                "the folder could not be made"
            },
        );
    }
    // SAFETY: parent is live and last is a NUL-terminated single component.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            last.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    };
    if raw < 0 {
        // SAFETY: parent is live and last names the directory just created.
        unsafe { libc::unlinkat(parent.as_raw_fd(), last.as_ptr(), libc::AT_REMOVEDIR) };
        return Err("the folder could not be made");
    }
    // SAFETY: openat returned an owned descriptor.
    let dir = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: dir is live and mode is valid.
    if unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } != 0 {
        // SAFETY: parent is live and last names the directory just created.
        unsafe { libc::unlinkat(parent.as_raw_fd(), last.as_ptr(), libc::AT_REMOVEDIR) };
        return Err("the folder could not be made");
    }
    let stat = match folder_stat(&dir) {
        Ok(stat) => stat,
        Err(_) => {
            // SAFETY: parent is live and last names the directory just created.
            unsafe { libc::unlinkat(parent.as_raw_fd(), last.as_ptr(), libc::AT_REMOVEDIR) };
            return Err("the folder could not be made");
        }
    };
    Ok((
        crate::fleet_db::DeliveryFolder {
            path: PathBuf::from(std::ffi::OsStr::from_bytes(bytes)),
            dev: stat.st_dev as u64,
            ino: stat.st_ino,
        },
        parent,
        last,
    ))
}

pub fn installed(clients: Option<&Path>, app: &str) -> bool {
    crate::wire::app_name(app)
        && clients.is_some_and(|clients| {
            std::fs::symlink_metadata(clients.join(format!("{app}.json")))
                .is_ok_and(|metadata| metadata.is_file())
        })
}

pub fn prepare(
    folder: &crate::fleet_db::DeliveryFolder,
    name: &str,
) -> Result<OwnedFd, &'static str> {
    if !crate::wire::file_name(name) {
        return Err(crate::wire::WRITE);
    }
    let dir = open_folder(folder).map_err(|_| crate::wire::FOLDER)?;
    let name = CString::new(name).map_err(|_| crate::wire::WRITE)?;
    if entry_stat(&dir, &name).is_some() {
        return Err(crate::wire::EXISTS);
    }
    Ok(dir)
}

fn entry_stat(dir: &OwnedFd, name: &std::ffi::CStr) -> Option<libc::stat> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: dir is live, name is NUL-terminated, and stat is writable storage.
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return None;
    }
    // SAFETY: fstatat initialized stat on success.
    Some(unsafe { stat.assume_init() })
}

struct Entries(*mut libc::DIR);
impl Drop for Entries {
    fn drop(&mut self) {
        // SAFETY: fdopendir returned this live stream, which is closed exactly once.
        unsafe { libc::closedir(self.0) };
    }
}
fn entries(
    dir: &OwnedFd,
    strict: bool,
    mut visit: impl FnMut(&std::ffi::CStr, &libc::stat) -> bool,
) -> Result<(), &'static str> {
    // SAFETY: dir is live and the literal is a NUL-terminated relative path.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(crate::wire::FOLDER);
    }
    // SAFETY: raw is a new owned directory descriptor; fdopendir takes ownership on success.
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        // SAFETY: fdopendir failed without taking ownership of raw.
        unsafe { libc::close(raw) };
        return Err(crate::wire::FOLDER);
    }
    let stream = Entries(stream);
    loop {
        // SAFETY: errno returns the current thread's errno pointer on this platform.
        unsafe { *errno() = 0 };
        // SAFETY: stream is live; the entry remains valid until the next call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: __error returns the current thread's errno pointer.
            if strict && unsafe { *errno() } != 0 {
                return Err(crate::wire::FOLDER);
            }
            break;
        }
        // SAFETY: readdir returned a live entry with a NUL-terminated name.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        // The app may delete an entry between readdir and fstatat, even in a strict scan.
        if let Some(stat) = entry_stat(dir, name) {
            if !visit(name, &stat) {
                break;
            }
        } else if strict && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
            return Err(crate::wire::FOLDER);
        }
    }
    Ok(())
}

pub fn scan(dir: &OwnedFd) -> Result<(usize, u64), &'static str> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    entries(dir, true, |name, stat| {
        if name.to_bytes().starts_with(b".radio-partial-") {
            return true;
        }
        count += 1;
        if stat.st_mode & libc::S_IFMT == libc::S_IFREG {
            bytes = bytes.saturating_add(stat.st_size.max(0) as u64);
        }
        count <= crate::wire::FOLDER_FILES
    })?;
    Ok((count, bytes))
}

pub fn available(dir: &OwnedFd) -> Result<u64, &'static str> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: dir is live and stat is writable storage.
    if unsafe { libc::fstatvfs(dir.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(crate::wire::SPACE);
    }
    // SAFETY: fstatvfs initialized stat on success.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize))
}

pub fn fits(
    entries: usize,
    bytes: u64,
    reserved_files: usize,
    reserved_bytes: u64,
    reserved_all: u64,
    size: u64,
    available: u64,
) -> Result<(), &'static str> {
    if entries.saturating_add(reserved_files).saturating_add(1) > crate::wire::FOLDER_FILES
        || bytes.saturating_add(reserved_bytes).saturating_add(size) > crate::wire::FOLDER_BYTES
    {
        return Err(crate::wire::FULL);
    }
    if size.saturating_add(reserved_all) > available.saturating_sub(crate::wire::FREE_FLOOR) {
        return Err(crate::wire::SPACE);
    }
    Ok(())
}

pub fn sweep(db: &mut crate::fleet_db::FleetDb, clients: Option<&Path>) {
    let Ok(folders) = db.delivery_folders() else {
        return;
    };
    for (app, folder) in folders {
        if !installed(clients, &app) {
            let _ = db.clear_delivery_folder(&app);
            continue;
        }
        let Ok(dir) = open_folder(&folder) else {
            continue;
        };
        let _ = entries(&dir, false, |name, stat| {
            if name.to_bytes().starts_with(b".radio-partial-")
                && stat.st_mode & libc::S_IFMT == libc::S_IFREG
            {
                // SAFETY: dir is live and name is the NUL-terminated relative entry name.
                unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
            }
            true
        });
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;

fn errno() -> *mut libc::c_int {
    #[cfg(target_os = "macos")]
    // SAFETY: __error returns the current thread's errno pointer.
    unsafe {
        libc::__error()
    }
    #[cfg(target_os = "linux")]
    // SAFETY: __errno_location returns the current thread's errno pointer.
    unsafe {
        libc::__errno_location()
    }
}
