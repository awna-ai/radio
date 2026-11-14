use crate::{PROTOCOL_MAJOR, RESPONSE_MAX_BYTES, Request, Response};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

/// The phrase for an unanswered or unreadable response.
pub const NO_ANSWER: &str = "Radio did not answer";
/// The phrase for a missing or stopped daemon.
pub const NOT_RUNNING: &str = "Radio is not running";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Sends one versioned request and reads its answer; transport failures return fixed error phrases.
pub fn request(socket: &Path, request: &Request) -> Result<Response, String> {
    request_line(socket, request).map(|(response, _)| response)
}

/// Sends one request and returns its parsed answer and original line without the newline.
pub fn request_line(socket: &Path, request: &Request) -> Result<(Response, String), String> {
    let mut value = serde_json::to_value(request).map_err(|_| NO_ANSWER)?;
    value
        .as_object_mut()
        .ok_or(NO_ANSWER)?
        .insert("v".into(), PROTOCOL_MAJOR.into());
    let stream = UnixStream::connect(socket).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            NOT_RUNNING.to_owned()
        }
        _ => NO_ANSWER.to_owned(),
    })?;
    exchange(stream, &value, answer_wait(request))
}

fn answer_wait(request: &Request) -> Duration {
    match request {
        // The daemon answers a send within 135 s.
        Request::Send { .. } => Duration::from_secs(150),
        Request::Join { .. } | Request::ComputerRemove { .. } | Request::Leave {} => {
            Duration::from_secs(120)
        }
        _ => TIMEOUT,
    }
}

fn exchange(
    mut stream: UnixStream,
    value: &serde_json::Value,
    wait: Duration,
) -> Result<(Response, String), String> {
    timeout_set(stream.set_read_timeout(Some(wait)))?;
    timeout_set(stream.set_write_timeout(Some(TIMEOUT)))?;
    // A refused caller may receive an answer before the peer reads anything.
    let sent = write_line(&mut stream, value).and_then(|()| stream.shutdown(Shutdown::Write));
    if sent.is_err_and(|error| {
        !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected
        )
    }) {
        return Err(NO_ANSWER.into());
    }
    let mut line = Vec::new();
    BufReader::new((&stream).take((RESPONSE_MAX_BYTES + 1) as u64))
        .read_until(b'\n', &mut line)
        .map_err(|_| NO_ANSWER)?;
    if line.len() > RESPONSE_MAX_BYTES || !line.ends_with(b"\n") {
        return Err(NO_ANSWER.into());
    }
    line.pop();
    let line = String::from_utf8(line).map_err(|_| NO_ANSWER)?;
    let response = serde_json::from_str(&line).map_err(|_| NO_ANSWER)?;
    Ok((response, line))
}

// macOS refuses socket options once both directions are shut (EINVAL): Radio has answered and closed, so
// the answer is already buffered and a read cannot block.
fn timeout_set(result: std::io::Result<()>) -> Result<(), &'static str> {
    match result {
        Err(error) if error.kind() != std::io::ErrorKind::InvalidInput => Err(NO_ANSWER),
        _ => Ok(()),
    }
}

fn write_line(stream: &mut UnixStream, value: &serde_json::Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *stream, value)?;
    stream.write_all(b"\n")
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
