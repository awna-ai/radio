use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    os::fd::AsFd,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub const LOG_MAX_BYTES: u64 = 1 << 20;
pub enum Field<'a> {
    Text(&'a str),
    Number(u64),
}

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

#[derive(Clone)]
pub struct Log {
    file: Option<Arc<Mutex<File>>>,
    cap: u64,
}

impl Log {
    pub fn none() -> Self {
        Self {
            file: None,
            cap: LOG_MAX_BYTES,
        }
    }

    pub fn stderr() -> Self {
        io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map(|fd| Self::to_file(File::from(fd), LOG_MAX_BYTES))
            .unwrap_or_else(|_| Self::none())
    }

    pub fn to_file(file: File, cap: u64) -> Self {
        Self {
            file: Some(Arc::new(Mutex::new(file))),
            cap,
        }
    }

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
