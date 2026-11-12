use super::*;

const SNAPSHOT_FAILED: &str = "the room store could not be snapshotted";

/// Writes a consistent copy of the store at `store` to `snapshot` (0600, created new), without migrating it.
pub fn snapshot_store(store: &Path, snapshot: &Path) -> Result<(), String> {
    match fs::symlink_metadata(snapshot) {
        Ok(_) => return Err("the snapshot already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(SNAPSHOT_FAILED.into()),
    }
    let store = store
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| store.file_name().map(|name| parent.join(name)))
        .ok_or("the room store could not be opened")?;
    let conn = Connection::open_with_flags(&store, STORE_OPEN_FLAGS)
        .map_err(|_| "the room store could not be opened")?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| SNAPSHOT_FAILED)?;
    let snapshot = snapshot
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| snapshot.file_name().map(|name| parent.join(name)))
        .ok_or(SNAPSHOT_FAILED)?;
    let path = snapshot.to_str().ok_or(SNAPSHOT_FAILED)?;
    conn.execute("VACUUM INTO ?1", [path])
        .map_err(|_| SNAPSHOT_FAILED)?;
    fs::set_permissions(snapshot, fs::Permissions::from_mode(0o600))
        .map_err(|_| SNAPSHOT_FAILED.into())
}

/// Replaces the store at `store` with `snapshot`, removing the store's `-wal` and `-shm` first.
pub fn restore_store(snapshot: &Path, store: &Path) -> Result<(), String> {
    const RESTORE_FAILED: &str = "the room store could not be restored";
    if !fs::symlink_metadata(snapshot)
        .map_err(|_| "the snapshot is not a file")?
        .is_file()
    {
        return Err("the snapshot is not a file".into());
    }
    for suffix in ["-wal", "-shm"] {
        let mut path = store.as_os_str().to_os_string();
        path.push(suffix);
        match fs::remove_file(PathBuf::from(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RESTORE_FAILED.into()),
        }
    }
    fs::rename(snapshot, store).map_err(|_| RESTORE_FAILED.into())
}
