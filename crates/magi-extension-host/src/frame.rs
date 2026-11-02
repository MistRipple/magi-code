//! 帧：4 字节大端长度 + UTF-8 JSON 正文（见 `docs/extension-protocol-v1.md` 第 1 节）。

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 单帧上限（宿主策略，不由清单自抬）。
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("帧超过 {limit} 字节上限")]
    TooLarge { limit: usize },
    #[error("帧正文不是合法 JSON")]
    InvalidJson,
    #[error("传输错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 读取一帧。对端在帧边界处关闭返回 `Ok(None)`；帧中途断开是错误。
pub async fn read_frame<R>(reader: &mut R, limit: usize) -> Result<Option<Value>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        let read = reader.read(&mut header[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(FrameError::Io(std::io::ErrorKind::UnexpectedEof.into()))
            };
        }
        filled += read;
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > limit {
        return Err(FrameError::TooLarge { limit });
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| FrameError::InvalidJson)
}

pub async fn write_frame<W>(writer: &mut W, value: &Value, limit: usize) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value).map_err(|_| FrameError::InvalidJson)?;
    if body.len() > limit {
        return Err(FrameError::TooLarge { limit });
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn frames_round_trip_and_eof_at_a_boundary_is_clean() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &json!({"a": 1}), MAX_FRAME_BYTES)
            .await
            .unwrap();
        drop(a);
        assert_eq!(
            read_frame(&mut b, MAX_FRAME_BYTES).await.unwrap(),
            Some(json!({"a": 1}))
        );
        assert!(read_frame(&mut b, MAX_FRAME_BYTES).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_truncated_and_invalid_frames_are_errors() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(10u32).to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame(&mut b, 4).await,
            Err(FrameError::TooLarge { .. })
        ));

        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(10u32).to_be_bytes()).await.unwrap();
        a.write_all(b"abc").await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame(&mut b, 100).await,
            Err(FrameError::Io(_))
        ));

        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&(3u32).to_be_bytes()).await.unwrap();
        a.write_all(b"abc").await.unwrap();
        assert!(matches!(
            read_frame(&mut b, 100).await,
            Err(FrameError::InvalidJson)
        ));

        let (mut w, _r) = tokio::io::duplex(1024);
        assert!(matches!(
            write_frame(&mut w, &json!("x".repeat(100)), 10).await,
            Err(FrameError::TooLarge { .. })
        ));
    }
}
