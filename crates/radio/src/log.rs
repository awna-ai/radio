use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{self, Seek, SeekFrom, Write},
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// The maximum regular log file size before the daemon truncates it.
pub const LOG_MAX_BYTES: u64 = 1 << 20;
/// A quoted text value or an unsigned number in a log line.
pub enum Field<'a> {
    Text(&'a str),
    Number(u64),
}

/// Formats whole seconds since the Unix epoch as a UTC timestamp.
pub fn utc(secs: u64) -> String {
    // Civil date from days since the epoch, in 400-year Gregorian eras.
    let days = (secs / 86400) as i64 + 719468;
    let era = days / 146097;
    let day_of_era = days % 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = month_from_march + if month_from_march < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = secs % 86400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

/// Formats one timestamped event with escaped fields and a trailing newline.
pub fn line(secs: u64, source: &str, event: &str, fields: &[(&str, Field)]) -> String {
    let mut text = format!("{} {source} {event}", utc(secs));
    for (key, value) in fields {
        match value {
            Field::Text(value) => {
                let _ = write!(text, " {key}={value:?}");
            }
            Field::Number(value) => {
                let _ = write!(text, " {key}={value}");
            }
        }
    }
    text.push('\n');
    text
}

/// Appends one event to an existing regular file, ignoring every error.
pub fn append(path: &Path, source: &str, event: &str, fields: &[(&str, Field)]) {
    let Ok(mut file) = File::options()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return;
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let _ = file.write_all(line(secs, source, event, fields).as_bytes());
}

/// A shared event log with a cap on regular file size.
#[derive(Clone)]
pub struct Log {
    file: Option<Arc<Mutex<File>>>,
    cap: u64,
}

impl Log {
    /// Returns a log that discards every event.
    pub fn none() -> Self {
        Self {
            file: None,
            cap: LOG_MAX_BYTES,
        }
    }

    /// Returns a log writing to standard error when it can be cloned.
    pub fn stderr(log: &Path) -> Self {
        io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map(|fd| Self::stderr_file(File::from(fd), log))
            .unwrap_or_else(|_| Self::none())
    }

    pub(crate) fn stderr_file(file: File, log: &Path) -> Self {
        let same_file = file
            .metadata()
            .ok()
            .zip(fs::metadata(log).ok())
            .is_some_and(|(fd, path)| {
                fd.is_file() && fd.dev() == path.dev() && fd.ino() == path.ino()
            });
        Self::to_file(file, if same_file { LOG_MAX_BYTES } else { u64::MAX })
    }

    /// Returns a log writing to the supplied file with the given size cap.
    pub fn to_file(file: File, cap: u64) -> Self {
        Self {
            file: Some(Arc::new(Mutex::new(file))),
            cap,
        }
    }

    /// Writes one event, truncating a regular file at its cap and ignoring errors.
    pub fn event(&self, source: &str, event: &str, fields: &[(&str, Field)]) {
        let Some(file) = &self.file else {
            return;
        };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs());
        let mut file = file.lock().unwrap_or_else(|poison| poison.into_inner());
        if file
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() >= self.cap)
            && file.set_len(0).is_ok()
        {
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(
                line(
                    secs,
                    "log",
                    "truncated",
                    &[("limit", Field::Number(self.cap))],
                )
                .as_bytes(),
            );
        }
        let _ = file.write_all(line(secs, source, event, fields).as_bytes());
    }
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
