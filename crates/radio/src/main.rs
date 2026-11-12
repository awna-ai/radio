use std::{env, path::PathBuf};

const USAGE: &str = "usage: radio serve [--name <name>]";

fn host_name(explicit: Option<&str>, detected: &str) -> Result<String, ()> {
    if let Some(name) = explicit {
        return if !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            Ok(name.into())
        } else {
            Err(())
        };
    }
    let name: String = detected
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(64)
        .collect();
    Ok(if name.is_empty() {
        "local".into()
    } else {
        name
    })
}

fn detected_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: buffer is writable for the provided length; a trailing NUL is checked below.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return String::new();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

fn run(args: &[String]) -> Result<(), (i32, String)> {
    let explicit = match args {
        [serve] if serve == "serve" => None,
        [serve, flag, name] if serve == "serve" && flag == "--name" => Some(name.as_str()),
        _ => return Err((2, USAGE.into())),
    };
    let host = host_name(explicit, &detected_name()).map_err(|()| (2, USAGE.into()))?;
    let home = env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| (1, "radio: HOME is not set".into()))?;
    let home = PathBuf::from(home);
    let socket = radio_client::socket_path(&home);
    let db = home.join(".local/state/radio/rooms.db");
    radio::server::Server::bind(&socket, &db, host)
        .map_err(|error| (1, format!("radio: {error}")))?
        .serve();
    Ok(())
}

fn main() {
    if let Err((code, message)) = run(&env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("{message}");
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_rules() {
        assert_eq!(
            host_name(None, "Garden-Shed.local").ok().as_deref(),
            Some("garden-shed")
        );
        assert_eq!(host_name(None, "..").ok().as_deref(), Some("local"));
        assert_eq!(host_name(None, "_A!7-é.local").ok().as_deref(), Some("a7-"));
        assert_eq!(host_name(None, &"A".repeat(80)).unwrap(), "a".repeat(64));
        for valid in ["Garden-Shed", "a._-0", &"a".repeat(64)] {
            assert_eq!(host_name(Some(valid), "ignored").unwrap(), valid);
        }
        for invalid in ["", ".bad/path", "a b", "é", &"a".repeat(65)] {
            assert!(host_name(Some(invalid), "ignored").is_err());
        }
    }
}
