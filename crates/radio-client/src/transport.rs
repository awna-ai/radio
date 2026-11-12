use crate::{PROTOCOL_MAJOR, RESPONSE_MAX_BYTES, Request, Response};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

const NO_ANSWER: &str = "the Radio did not answer";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Sends one versioned request and reads its answer; transport failures return fixed error phrases.
pub fn request(socket: &Path, request: &Request) -> Result<Response, String> {
    let mut value = serde_json::to_value(request).map_err(|_| NO_ANSWER)?;
    value
        .as_object_mut()
        .ok_or(NO_ANSWER)?
        .insert("v".into(), PROTOCOL_MAJOR.into());
    let stream = UnixStream::connect(socket).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "the Radio is not running".to_owned()
        }
        _ => NO_ANSWER.to_owned(),
    })?;
    exchange(stream, &value)
}

fn exchange(mut stream: UnixStream, value: &serde_json::Value) -> Result<Response, String> {
    timeout_set(stream.set_read_timeout(Some(TIMEOUT)))?;
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
    serde_json::from_slice(&line).map_err(|_| NO_ANSWER.into())
}

// macOS refuses socket options once both directions are shut (EINVAL): the Radio has answered and closed, so
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
