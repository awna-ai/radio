use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
};

/// Creates or secures a directory with mode 0700, refusing a link or a nondirectory.
pub fn private_dir(dir: &Path, recursive: bool) -> Result<(), &'static str> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() => return Err("not a directory"),
        Ok(_) => {}
        Err(_) => fs::DirBuilder::new()
            .recursive(recursive)
            .mode(0o700)
            .create(dir)
            .map_err(|_| "could not be created")?,
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| "could not be secured")
}
