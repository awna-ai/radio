use super::*;
use tokio::{
    io::{AsyncWriteExt, duplex},
    time::{Duration, timeout},
};

async fn decode(bytes: &[u8], cap: usize) -> Result<String, &'static str> {
    let (mut writer, mut reader) = duplex(bytes.len().max(1));
    writer.write_all(bytes).await.unwrap();
    writer.shutdown().await.unwrap();
    timeout(Duration::from_secs(1), read(&mut reader, cap))
        .await
        .unwrap()
}

#[tokio::test]
async fn both_caps_round_trip_at_the_exact_limit() {
    for cap in [CONTROL_CAP, RECORD_CAP] {
        let value = "a".repeat(cap - 2);
        let (mut writer, mut reader) = duplex(cap + 4);
        write(&mut writer, &value, cap).await.unwrap();
        writer.shutdown().await.unwrap();
        let actual: String = timeout(Duration::from_secs(1), read(&mut reader, cap))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, value);
    }
}

#[tokio::test]
async fn invalid_lengths_return_without_reading_a_body() {
    for cap in [CONTROL_CAP, RECORD_CAP] {
        for length in [0, cap + 1] {
            let (mut writer, mut reader) = duplex(4);
            writer
                .write_all(&(length as u32).to_be_bytes())
                .await
                .unwrap();
            let result = timeout(Duration::from_secs(1), read::<String, _>(&mut reader, cap))
                .await
                .unwrap();
            assert_eq!(result, Err("the frame is not valid"));
        }
    }
}

#[tokio::test]
async fn truncated_invalid_and_trailing_bytes_are_refused() {
    for bytes in [
        vec![],
        vec![0, 0],
        vec![0, 0, 0, 3, b'"', b'a'],
        vec![0, 0, 0, 1, b'x'],
        vec![0, 0, 0, 3, b'"', b'a', b'"', 0],
    ] {
        assert_eq!(
            decode(&bytes, CONTROL_CAP).await,
            Err("the frame is not valid")
        );
    }
}

#[tokio::test]
async fn writer_refuses_over_cap_without_writing() {
    for cap in [CONTROL_CAP, RECORD_CAP] {
        let (mut writer, mut reader) = duplex(4);
        assert_eq!(
            write(&mut writer, &"a".repeat(cap), cap).await,
            Err("the frame is too large")
        );
        writer.shutdown().await.unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes)
            .await
            .unwrap();
        assert!(bytes.is_empty());
    }
}
