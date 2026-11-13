use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

pub fn check_folder(path: &Path) -> Result<OwnedFd, &'static str> {
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
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
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
    folder: &Path,
    name: &str,
    mut body: impl FnMut(&mut File) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if !crate::wire::file_name(name) {
        return Err(crate::wire::WRITE);
    }
    let dir = check_folder(folder).map_err(|_| crate::wire::FOLDER)?;
    let name = CString::new(name).map_err(|_| crate::wire::WRITE)?;
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|_| crate::wire::WRITE)?;
    let temporary = CString::new(format!(
        ".radio-incoming-{:016x}",
        u64::from_ne_bytes(random)
    ))
    .map_err(|_| crate::wire::WRITE)?;
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
        dir: &dir,
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

#[cfg(test)]
mod tests {
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
        assert!(check_folder(&folder.0).is_ok());
    }
    #[test]
    fn delivery_rejects_a_symlink() {
        let folder = Folder::new();
        let link = folder.0.join("link");
        std::os::unix::fs::symlink(&folder.0, &link).unwrap();
        assert_eq!(
            check_folder(&link).unwrap_err(),
            "the folder could not be opened"
        );
    }

    #[test]
    fn delivery_rejects_a_file_as_folder() {
        let folder = Folder::new();
        let path = folder.0.join("file");
        fs::write(&path, []).unwrap();
        assert_eq!(
            check_folder(&path).unwrap_err(),
            "the folder could not be opened"
        );
    }

    #[test]
    fn delivery_rejects_a_writable_folder() {
        let folder = Folder::new();
        fs::set_permissions(&folder.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            check_folder(&folder.0).unwrap_err(),
            "the folder is writable by others"
        );
    }

    #[test]
    fn delivery_writes_private_complete_bytes() {
        use std::io::Write;
        let folder = Folder::new();
        write(&folder.0, "document", |file| {
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
        let error = write(&folder.0, "document", |file| {
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
            write(&folder.0, "document", |_| Ok(())),
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
                write(&folder.0, name, |_| {
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
            check_folder(&path).unwrap_err(),
            "the folder could not be opened"
        );
    }
}
