use super::*;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::PathBuf,
    sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "radio-key-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn key(&self) -> PathBuf {
        self.0.join(KEY_FILE)
    }
    fn write(&self, size: usize, mode: u32) {
        fs::create_dir_all(&self.0).unwrap();
        fs::write(self.key(), vec![7; size]).unwrap();
        fs::set_permissions(self.key(), fs::Permissions::from_mode(mode)).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

#[test]
fn creates_private_key_and_reloads_it() {
    let temp = Temp::new();
    let first = load_or_create(&temp.0).unwrap();
    assert_eq!(mode(&temp.0), 0o700);
    assert_eq!(mode(&temp.key()), 0o600);
    assert_eq!(fs::metadata(temp.key()).unwrap().len(), 32);
    assert_eq!(first.public(), load_or_create(&temp.0).unwrap().public());
}
#[test]
fn public_id_is_lowercase_hex() {
    let key = iroh::SecretKey::generate();
    let id = endpoint_id(&key);
    assert_eq!(id.len(), 64);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(id, key.public().to_string());
}
#[test]
fn creates_missing_parents_and_tightens_existing_folder() {
    let temp = Temp::new();
    let dir = temp.0.join("parent/child");
    load_or_create(&dir).unwrap();
    assert_eq!(mode(&temp.0), 0o700);
    assert_eq!(mode(&temp.0.join("parent")), 0o700);
    assert_eq!(mode(&dir), 0o700);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    load_or_create(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
}
#[test]
fn refuses_loose_and_damaged_files_unchanged() {
    for (size, permissions, phrase) in [
        (32, 0o644, "the link key is open to others"),
        (31, 0o600, "the link key is damaged"),
        (33, 0o600, "the link key is damaged"),
    ] {
        let temp = Temp::new();
        temp.write(size, permissions);
        let before = fs::read(temp.key()).unwrap();
        let owner = fs::metadata(temp.key()).unwrap().uid();
        assert_eq!(load_or_create(&temp.0).err(), Some(phrase));
        assert!(fs::read(temp.key()).unwrap() == before);
        assert_eq!(mode(&temp.key()), permissions);
        assert_eq!(fs::metadata(temp.key()).unwrap().uid(), owner);
    }
}
#[test]
fn refuses_key_symlink_directory_and_fifo_without_changing_them() {
    let target = Temp::new();
    target.write(32, 0o600);
    for kind in 0..3 {
        let temp = Temp::new();
        fs::create_dir_all(&temp.0).unwrap();
        match kind {
            0 => symlink(target.key(), temp.key()).unwrap(),
            1 => fs::create_dir(temp.key()).unwrap(),
            _ => {
                use std::os::unix::ffi::OsStrExt;
                let path = std::ffi::CString::new(temp.key().as_os_str().as_bytes()).unwrap();
                // SAFETY: path is a live NUL-terminated string.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
        }
        let before = fs::symlink_metadata(temp.key()).unwrap();
        let bytes = fs::read(target.key()).unwrap();
        assert_eq!(
            load_or_create(&temp.0).err(),
            Some("the link key is not a regular file")
        );
        let after = fs::symlink_metadata(temp.key()).unwrap();
        assert_eq!(
            (before.mode(), before.uid(), before.ino(), before.len()),
            (after.mode(), after.uid(), after.ino(), after.len())
        );
        assert!(fs::read(target.key()).unwrap() == bytes);
        assert_eq!(mode(&target.key()), 0o600);
        if kind == 0 {
            assert_eq!(fs::read_link(temp.key()).unwrap(), target.key());
        }
        if kind == 1 {
            assert_eq!(fs::read_dir(temp.key()).unwrap().count(), 0);
        }
    }
}
#[test]
fn refuses_folder_symlink() {
    let temp = Temp::new();
    let target = Temp::new();
    fs::create_dir_all(&target.0).unwrap();
    symlink(&target.0, &temp.0).unwrap();
    assert_eq!(
        load_or_create(&temp.0).err(),
        Some("the link folder is not usable")
    );
    assert_eq!(fs::read_dir(&target.0).unwrap().count(), 0);
    fs::remove_file(&temp.0).unwrap();
}
#[test]
fn concurrent_creation_keeps_one_key_and_no_temporary_files() {
    for _ in 0..20 {
        let temp = Temp::new();
        let barrier = Barrier::new(2);
        let keys = thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        load_or_create(&temp.0).unwrap().public()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(keys[0], keys[1]);
        let entries: Vec<_> = fs::read_dir(&temp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, [KEY_FILE]);
    }
}
#[cfg(target_os = "macos")]
#[test]
fn excludes_created_and_loaded_keys_from_backups() {
    use std::os::unix::ffi::OsStrExt;
    for existing in [false, true] {
        let temp = Temp::new();
        if existing {
            temp.write(32, 0o600);
        }
        load_or_create(&temp.0).unwrap();
        let path = std::ffi::CString::new(temp.key().as_os_str().as_bytes()).unwrap();
        let mut bytes = [0u8; 128];
        // SAFETY: both strings are NUL-terminated and bytes is writable for its advertised length.
        let length = unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"com.apple.metadata:com_apple_backup_excludeItem".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                0,
            )
        };
        assert!(length > 0);
        let value = &bytes[..length as usize];
        assert!(value.starts_with(b"bplist00"));
        assert!(
            value
                .windows(b"com.apple.backupd".len())
                .any(|w| w == b"com.apple.backupd")
        );
    }
}
#[test]
fn ignores_a_temporary_file_from_a_crash() {
    let temp = Temp::new();
    fs::create_dir_all(&temp.0).unwrap();
    let stale = temp.0.join(".node.key.1.1");
    fs::write(&stale, b"keep").unwrap();
    load_or_create(&temp.0).unwrap();
    assert_eq!(fs::metadata(temp.key()).unwrap().len(), 32);
    assert_eq!(fs::read(stale).unwrap(), b"keep");
}
