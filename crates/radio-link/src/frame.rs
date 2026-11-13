use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CONTROL_CAP: usize = 64 * 1024;
pub const RECORD_CAP: usize = 1024 * 1024;
const INVALID: &str = "the frame is not valid";

pub async fn write<T: Serialize, W: AsyncWrite + Unpin>(
    w: &mut W,
    value: &T,
    cap: usize,
) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| INVALID)?;
    if bytes.is_empty() || bytes.len() > cap {
        return Err("the frame is too large");
    }
    let length = u32::try_from(bytes.len()).map_err(|_| "the frame is too large")?;
    w.write_all(&length.to_be_bytes())
        .await
        .map_err(|_| INVALID)?;
    w.write_all(&bytes).await.map_err(|_| INVALID)
}

pub async fn read<T: DeserializeOwned, R: AsyncRead + Unpin>(
    r: &mut R,
    cap: usize,
) -> Result<T, &'static str> {
    let mut prefix = [0; 4];
    r.read_exact(&mut prefix).await.map_err(|_| INVALID)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > cap {
        return Err(INVALID);
    }
    let mut bytes = vec![0; length];
    r.read_exact(&mut bytes).await.map_err(|_| INVALID)?;
    if r.read(&mut [0]).await.map_err(|_| INVALID)? != 0 {
        return Err(INVALID);
    }
    serde_json::from_slice(&bytes).map_err(|_| INVALID)
}

pub async fn read_head<T: DeserializeOwned, R: AsyncRead + Unpin>(
    r: &mut R,
    cap: usize,
) -> Result<T, &'static str> {
    let mut prefix = [0; 4];
    r.read_exact(&mut prefix).await.map_err(|_| INVALID)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > cap {
        return Err(INVALID);
    }
    let mut bytes = vec![0; length];
    r.read_exact(&mut bytes).await.map_err(|_| INVALID)?;
    serde_json::from_slice(&bytes).map_err(|_| INVALID)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
