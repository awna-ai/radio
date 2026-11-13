use radio_client::{Member, REQUEST_MAX_BYTES, Request};
use std::{fs::File, io::Read, path::PathBuf};

/// Where a post body or app section will be read after argument parsing.
#[derive(Debug, PartialEq)]
pub enum BodySource {
    Text(String),
    File(PathBuf),
}

/// One socket operation, with any body input still unresolved.
#[derive(Debug, PartialEq)]
pub struct Command {
    pub request: Request,
    pub body: Option<BodySource>,
}

fn member(value: &str) -> Result<Member, ()> {
    let (kind, name) = value.split_once(':').ok_or(())?;
    if !radio_rooms::is_kind(kind) || name.is_empty() {
        return Err(());
    }
    Ok(Member {
        kind: kind.into(),
        name: name.into(),
    })
}

fn digits<T: std::str::FromStr>(value: &str) -> Result<T, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse().map_err(|_| ())
}

/// Parses one operation without reading files, stdin or the environment.
#[allow(clippy::result_unit_err)]
pub fn parse(args: &[String]) -> Result<Command, ()> {
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    let (op, positional_count, rest, allowed): (&str, usize, &[&str], &[&str]) = match words
        .as_slice()
    {
        ["hello", rest @ ..] => ("hello", 0, rest, &[]),
        ["id", rest @ ..] => ("id", 0, rest, &[]),
        ["fleet", "init", rest @ ..] => ("fleet_init", 1, rest, &[]),
        ["code", "create", rest @ ..] => ("code_create", 0, rest, &["--name", "--ttl"]),
        ["code", "list", rest @ ..] => ("code_list", 0, rest, &[]),
        ["code", "revoke", rest @ ..] => ("code_revoke", 1, rest, &[]),
        ["resign", rest @ ..] => (
            "resign",
            0,
            rest,
            &["--app", "--section", "--section-file", "--roles"],
        ),
        ["computer", "records", rest @ ..] => ("computer_records", 0, rest, &["--app"]),
        ["computer", "list", rest @ ..] => ("computer_list", 0, rest, &[]),
        ["computer", "accept", rest @ ..] => ("computer_accept", 1, rest, &[]),
        ["computer", "block", rest @ ..] => ("computer_block", 1, rest, &[]),
        ["computer", "trust", rest @ ..] => ("computer_trust", 1, rest, &[]),
        ["register", rest @ ..] => ("register", 0, rest, &["--app", "--app-version", "--member"]),
        ["room", "create", rest @ ..] => ("create", 1, rest, &["--title"]),
        ["rooms", rest @ ..] => ("rooms", 0, rest, &["--after", "--limit"]),
        ["post", rest @ ..] => ("post", 1, rest, &["--as", "--body", "--body-file"]),
        ["read", rest @ ..] => ("read", 1, rest, &["--since", "--limit"]),
        ["line", "list", rest @ ..] => ("list", 1, rest, &[]),
        ["line", "set", rest @ ..] => ("set", 3, rest, &["--as", "--expires-at"]),
        ["line", "clear", rest @ ..] => ("clear", 2, rest, &["--as"]),
        _ => return Err(()),
    };
    if rest.len() < positional_count {
        return Err(());
    }
    let (positionals, flags) = rest.split_at(positional_count);
    if positionals.iter().any(|value| allowed.contains(value)) {
        return Err(());
    }
    let (pairs, remainder) = flags.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(());
    }
    let mut seen = std::collections::HashMap::new();
    let mut members = Vec::new();
    for [flag, value] in pairs {
        if !allowed.contains(flag) {
            return Err(());
        }
        if *flag == "--member" {
            members.push(member(value)?);
        } else if seen.insert(*flag, *value).is_some() {
            return Err(());
        }
    }
    let required = |flag| seen.get(flag).copied().ok_or(());
    let author = || member(required("--as")?);
    let limit = || seen.get("--limit").map(|value| digits(value)).transpose();
    let mut body = None;
    let request = match op {
        "hello" => Request::Hello {},
        "id" => Request::Id {},
        "fleet_init" => Request::FleetInit {
            name: positionals[0].into(),
        },
        "code_create" => Request::CodeCreate {
            name: seen.get("--name").map(|value| (*value).into()),
            ttl: seen.get("--ttl").map(|value| lifetime(value)).transpose()?,
        },
        "code_list" => Request::CodeList {},
        "code_revoke" => Request::CodeRevoke {
            id: positionals[0].into(),
        },
        "resign" => {
            body = match (seen.get("--section"), seen.get("--section-file")) {
                (Some(text), None) => Some(BodySource::Text((*text).into())),
                (None, Some(path)) => Some(BodySource::File((*path).into())),
                (None, None) => None,
                _ => return Err(()),
            };
            Request::Resign {
                app: required("--app")?.into(),
                section: None,
                roles: seen.get("--roles").map(|names| {
                    if names.is_empty() {
                        Vec::new()
                    } else {
                        names.split(',').map(String::from).collect()
                    }
                }),
            }
        }
        "computer_records" => Request::ComputerRecords {
            app: required("--app")?.into(),
        },
        "computer_list" => Request::ComputerList {},
        "computer_accept" => Request::ComputerAccept {
            computer: positionals[0].into(),
        },
        "computer_block" => Request::ComputerBlock {
            computer: positionals[0].into(),
        },
        "computer_trust" => Request::ComputerTrust {
            computer: positionals[0].into(),
        },
        "register" => Request::Register {
            app: required("--app")?.into(),
            app_version: required("--app-version")?.into(),
            members,
        },
        "create" => Request::CreateRoom {
            room: positionals[0].into(),
            title: required("--title")?.into(),
        },
        "rooms" => Request::ListRooms {
            after: seen.get("--after").map(|value| (*value).into()),
            limit: limit()?,
        },
        "post" => {
            body = Some(match (seen.get("--body"), seen.get("--body-file")) {
                (Some(text), None) => BodySource::Text((*text).into()),
                (None, Some(path)) => BodySource::File((*path).into()),
                _ => return Err(()),
            });
            Request::Post {
                room: positionals[0].into(),
                author: author()?,
                body: String::new(),
            }
        }
        "read" => Request::Read {
            room: positionals[0].into(),
            since: seen.get("--since").map(|value| digits(value)).transpose()?,
            limit: limit()?,
        },
        "list" => Request::Status {
            room: positionals[0].into(),
        },
        "set" => {
            let expires_at = seen
                .get("--expires-at")
                .map(|value| {
                    value.parse::<f64>().map_err(|_| ()).and_then(|value| {
                        if value.is_finite() {
                            Ok(value)
                        } else {
                            Err(())
                        }
                    })
                })
                .transpose()?;
            Request::StatusSet {
                room: positionals[0].into(),
                key: positionals[1].into(),
                line: positionals[2].into(),
                author: author()?,
                expires_at,
            }
        }
        "clear" => Request::StatusClear {
            room: positionals[0].into(),
            key: positionals[1].into(),
            author: author()?,
        },
        _ => unreachable!(),
    };
    Ok(Command { request, body })
}

fn lifetime(value: &str) -> Result<u64, ()> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3600)
    } else {
        return Err(());
    };
    digits::<u64>(number)?.checked_mul(multiplier).ok_or(())
}

fn read_body(reader: impl Read) -> Result<String, (i32, String)> {
    let mut bytes = Vec::new();
    reader
        .take((REQUEST_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| (2, "radio: the body file could not be read".into()))?;
    if bytes.len() > REQUEST_MAX_BYTES {
        return Err((1, format!("radio: {}", radio_rooms::MESSAGE_INVALID)));
    }
    String::from_utf8(bytes).map_err(|_| (2, "radio: the body is not UTF-8".into()))
}

impl Command {
    /// Resolves a post body or app section with a bounded read before the socket or HOME is consulted.
    pub fn resolve(mut self, stdin: impl Read) -> Result<Request, (i32, String)> {
        if let Some(source) = self.body {
            let section_file = matches!(self.request, Request::Resign { .. })
                && matches!(source, BodySource::File(_));
            let body = match source {
                BodySource::Text(text) => text,
                BodySource::File(path) if path.as_os_str() == "-" => read_body(stdin)?,
                BodySource::File(path) => read_body(
                    File::open(path)
                        .map_err(|_| (2, "radio: the body file could not be read".into()))?,
                )?,
            };
            if section_file && body.is_empty() {
                return Err((2, "radio: the section file is empty".into()));
            }
            if body.len() > REQUEST_MAX_BYTES {
                return Err((1, format!("radio: {}", radio_rooms::MESSAGE_INVALID)));
            }
            match &mut self.request {
                Request::Post {
                    body: destination, ..
                } => *destination = body,
                Request::Resign { section, .. } => *section = Some(body),
                _ => {}
            }
        }
        Ok(self.request)
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
