use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::PathBuf,
};
struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("radio-delivery-{}", u64::from_ne_bytes(random)));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn delivery_checks_a_private_folder() {
    let folder = Folder::new();
    assert!(open_folder(&record(&folder.0)).is_ok());
}
#[test]
fn delivery_rejects_a_symlink() {
    let folder = Folder::new();
    let link = folder.0.join("link");
    std::os::unix::fs::symlink(&folder.0, &link).unwrap();
    assert_eq!(
        open_folder(&record(&link)).unwrap_err(),
        "the folder could not be opened"
    );
}

#[test]
fn delivery_rejects_a_fifo_without_waiting_for_a_writer() {
    let folder = Folder::new();
    let path = folder.0.join("pipe");
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and valid for the call.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        sent.send(open_folder(&record(&path)).unwrap_err()).unwrap();
    });
    assert_eq!(
        received.recv_timeout(std::time::Duration::from_secs(2)),
        Ok("the folder could not be opened")
    );
    worker.join().unwrap();
}

#[test]
fn delivery_rejects_a_file_as_folder() {
    let folder = Folder::new();
    let path = folder.0.join("file");
    fs::write(&path, []).unwrap();
    assert_eq!(
        open_folder(&record(&path)).unwrap_err(),
        "the folder could not be opened"
    );
}

#[test]
fn delivery_rejects_a_writable_folder() {
    let folder = Folder::new();
    fs::set_permissions(&folder.0, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        open_folder(&record(&folder.0)).unwrap_err(),
        "the folder is writable by others"
    );
}

#[test]
fn delivery_writes_private_complete_bytes() {
    use std::io::Write;
    let folder = Folder::new();
    write(&record(&folder.0), "document", |file| {
        file.write_all(b"contents").map_err(|_| "write")
    })
    .unwrap();
    assert_eq!(fs::read(folder.0.join("document")).unwrap(), b"contents");
    assert_eq!(
        fs::metadata(folder.0.join("document")).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(&folder.0).unwrap().count(), 1);
}

#[test]
fn delivery_cleans_up_a_failed_body() {
    use std::io::Write;
    let folder = Folder::new();
    let error = write(&record(&folder.0), "document", |file| {
        file.write_all(b"partial").unwrap();
        Err("the file body is not valid")
    });
    assert_eq!(error, Err("the file body is not valid"));
    assert_eq!(fs::read_dir(&folder.0).unwrap().count(), 0);
}

#[test]
fn delivery_never_replaces_an_existing_file() {
    let folder = Folder::new();
    fs::write(folder.0.join("document"), b"original").unwrap();
    assert_eq!(
        write(&record(&folder.0), "document", |_| Ok(())),
        Err(crate::wire::EXISTS)
    );
    assert_eq!(fs::read(folder.0.join("document")).unwrap(), b"original");
    assert_eq!(fs::read_dir(&folder.0).unwrap().count(), 1);
}

#[test]
fn delivery_rejects_unsafe_names_before_writing() {
    let folder = Folder::new();
    for name in [
        ".hidden",
        "../escape",
        "a/b",
        "",
        "a\nb",
        "a\\b",
        "a\0b",
        &"x".repeat(256),
    ] {
        let mut called = false;
        assert_eq!(
            write(&record(&folder.0), name, |_| {
                called = true;
                Ok(())
            }),
            Err(crate::wire::WRITE)
        );
        assert!(!called);
    }
    assert_eq!(fs::read_dir(&folder.0).unwrap().count(), 0);
}
#[test]
fn delivery_rejects_a_symlink_with_trailing_slashes() {
    use std::os::unix::ffi::OsStringExt;
    let folder = Folder::new();
    let link = folder.0.join("link");
    std::os::unix::fs::symlink(&folder.0, &link).unwrap();
    let mut bytes = link.as_os_str().as_bytes().to_vec();
    bytes.extend_from_slice(b"///");
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    assert_eq!(
        open_folder(&record(&path)).unwrap_err(),
        "the folder could not be opened"
    );
}
fn record(path: &Path) -> crate::fleet_db::DeliveryFolder {
    let metadata = fs::metadata(path).unwrap();
    crate::fleet_db::DeliveryFolder {
        path: path.to_path_buf(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}
#[test]
fn delivery_refuses_replaced_inode() {
    let folder = Folder::new();
    let original = record(&folder.0);
    let moved = folder.0.with_extension("moved");
    fs::rename(&folder.0, &moved).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&folder.0).unwrap();
    assert!(open_folder(&original).is_err());
    fs::remove_dir(moved).unwrap();
}
#[test]
fn delivery_exists_before_body() {
    let folder = Folder::new();
    fs::write(folder.0.join("report.txt"), b"old").unwrap();
    let mut called = false;
    assert_eq!(
        write(&record(&folder.0), "report.txt", |_| {
            called = true;
            Ok(())
        }),
        Err(crate::wire::EXISTS)
    );
    assert!(!called);
}
#[test]
fn delivery_partial_has_random_hex_name() {
    let folder = Folder::new();
    write(&record(&folder.0), "report.txt", |_| {
        let name = fs::read_dir(&folder.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        let name = name.to_str().unwrap();
        let suffix = name.strip_prefix(".radio-partial-").unwrap();
        assert_eq!(suffix.len(), 32);
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        Ok(())
    })
    .unwrap();
}
#[test]
fn delivery_caps_count_entries_bytes_and_reservations() {
    assert_eq!(fits(64, 0, 0, 0, 0, u64::MAX), Err(crate::wire::FULL));
    assert_eq!(fits(0, 1 << 30, 0, 0, 1, u64::MAX), Err(crate::wire::FULL));
    assert_eq!(fits(63, 0, 1, 0, 1, u64::MAX), Err(crate::wire::FULL));
    assert_eq!(
        fits(0, 0, 1, (1 << 30) - 1, 2, u64::MAX),
        Err(crate::wire::FULL)
    );
    assert_eq!(fits(0, 0, 1, 5, 6, 10), Err(crate::wire::SPACE));
    assert_eq!(fits(0, 0, 1, 5, 5, 10), Ok(()));
    let folder = Folder::new();
    for i in 0..64 {
        fs::write(folder.0.join(format!("entry-{i}")), []).unwrap();
    }
    assert_eq!(
        write(&record(&folder.0), "report.txt", |_| panic!("body read")),
        Err(crate::wire::FULL)
    );
}
#[test]
fn delivery_scan_counts_sparse_bytes() {
    let folder = Folder::new();
    File::create(folder.0.join("sparse"))
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    let dir = open_folder(&record(&folder.0)).unwrap();
    let (count, bytes) = scan(&dir).unwrap();
    assert_eq!((count, bytes), (1, 1 << 30));
    assert_eq!(
        fits(count, bytes, 0, 0, 1, u64::MAX),
        Err(crate::wire::FULL)
    );
}
#[test]
fn delivery_sweep_removes_only_partial_regular_files_and_missing_apps() {
    let folder = Folder::new();
    let mut db = crate::fleet_db::FleetDb::open(&folder.0.join("fleet.db")).unwrap();
    let path = folder.0.join("arrivals");
    db.set_delivery_folder("notes", &path).unwrap();
    db.set_delivery_folder("garden", &folder.0.join("missing"))
        .unwrap();
    let clients = folder.0.join("clients");
    fs::create_dir(&clients).unwrap();
    fs::write(clients.join("notes.json"), b"{}").unwrap();
    fs::write(path.join(".radio-partial-old"), []).unwrap();
    fs::write(path.join("report.txt"), []).unwrap();
    fs::create_dir(path.join(".radio-partial-folder")).unwrap();
    std::os::unix::fs::symlink(path.join("report.txt"), path.join(".radio-partial-x")).unwrap();
    sweep(&mut db, Some(&clients));
    assert!(!path.join(".radio-partial-old").exists());
    assert!(path.join("report.txt").exists());
    assert!(path.join(".radio-partial-folder").is_dir());
    assert!(
        fs::symlink_metadata(path.join(".radio-partial-x"))
            .unwrap()
            .is_symlink()
    );
    assert!(db.delivery_folder("garden").unwrap().is_none());
    let moved = folder.0.join("moved");
    fs::rename(&path, &moved).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join(".radio-partial-keep"), []).unwrap();
    sweep(&mut db, Some(&clients));
    assert!(path.join(".radio-partial-keep").exists());
}

#[test]
fn delivery_installed_requires_valid_app_and_regular_record() {
    let folder = Folder::new();
    assert!(!installed(None, "notes"));
    assert!(!installed(Some(&folder.0), "../notes"));
    fs::write(folder.0.join("notes.json"), b"{}").unwrap();
    assert!(installed(Some(&folder.0), "notes"));
    std::os::unix::fs::symlink(folder.0.join("notes.json"), folder.0.join("garden.json")).unwrap();
    assert!(!installed(Some(&folder.0), "garden"));
}
#[test]
fn delivery_scan_counts_folders_and_links_but_no_partial_names() {
    let folder = Folder::new();
    fs::create_dir(folder.0.join("directory")).unwrap();
    std::os::unix::fs::symlink("missing", folder.0.join("link")).unwrap();
    fs::write(folder.0.join(".radio-partial-ignored"), b"ignored").unwrap();
    let dir = open_folder(&record(&folder.0)).unwrap();
    assert_eq!(scan(&dir), Ok((2, 0)));
}
