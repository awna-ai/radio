use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const KEY_FILE: &str = "node.key";
const FOLDER: &str = "the link folder is not usable";
const MAKE: &str = "the link key could not be made";
const READ: &str = "the link key could not be read";
const REGULAR: &str = "the link key is not a regular file";

pub fn load_or_create(dir: &Path) -> Result<iroh::SecretKey, &'static str> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|_| FOLDER)?;
        }
        Err(_) => return Err(FOLDER),
    }
    let metadata = fs::symlink_metadata(dir).map_err(|_| FOLDER)?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(FOLDER);
    }
    let folder = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|_| FOLDER)?;
    let metadata = folder.metadata().map_err(|_| FOLDER)?;
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(FOLDER);
    }
    // SAFETY: folder owns a live directory descriptor.
    if unsafe { libc::fchmod(folder.as_raw_fd(), 0o700) } != 0 {
        return Err(FOLDER);
    }
    remove_stale_temporary_keys(dir, uid);
    loop {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join(KEY_FILE))
        {
            Ok(mut file) => {
                let metadata = file.metadata().map_err(|_| READ)?;
                if !metadata.is_file() {
                    return Err(REGULAR);
                }
                if metadata.uid() != uid {
                    return Err("the link key belongs to another user");
                }
                if metadata.mode() & 0o077 != 0 {
                    return Err("the link key is open to others");
                }
                if metadata.len() != 32 {
                    return Err("the link key is damaged");
                }
                let mut bytes = [0; 32];
                file.read_exact(&mut bytes).map_err(|_| READ)?;
                exclude_from_backup(&file);
                return Ok(iroh::SecretKey::from_bytes(&bytes));
            }
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Err(REGULAR),
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                if let Some(key) = create(dir)? {
                    return Ok(key);
                }
            }
            Err(_) => return Err(READ),
        }
    }
}

fn remove_stale_temporary_keys(dir: &Path, uid: u32) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(b".node.key.")
        {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_file()
            && metadata.uid() == uid
            && metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(60))
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn create(dir: &Path) -> Result<Option<iroh::SecretKey>, &'static str> {
    let key = iroh::SecretKey::generate();
    let (temp, mut file) = loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| MAKE)?
            .as_nanos();
        let temp = dir.join(format!(".node.key.{}.{nanos}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(MAKE),
        }
    };
    exclude_from_backup(&file);
    let result = (|| {
        file.write_all(&key.to_bytes()).map_err(|_| MAKE)?;
        file.sync_all().map_err(|_| MAKE)?;
        match fs::hard_link(&temp, dir.join(KEY_FILE)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(_) => Err(MAKE),
        }
    })();
    let _ = fs::remove_file(&temp);
    if !result? {
        return Ok(None);
    }
    if let Ok(folder) = File::open(dir) {
        let _ = folder.sync_all();
    }
    Ok(Some(key))
}

pub fn endpoint_id(key: &iroh::SecretKey) -> String {
    key.public().to_string()
}

#[cfg(target_os = "macos")]
fn exclude_from_backup(file: &File) {
    const VALUE: &[u8] = b"bplist00\x5f\x10\x11com.apple.backupd\x08\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x1c";
    // SAFETY: the fd is live, the name is NUL-terminated, and VALUE spans the supplied length.
    let _ = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            c"com.apple.metadata:com_apple_backup_excludeItem".as_ptr(),
            VALUE.as_ptr().cast(),
            VALUE.len(),
            0,
            libc::XATTR_CREATE,
        )
    };
}

#[cfg(not(target_os = "macos"))]
fn exclude_from_backup(_file: &File) {}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
