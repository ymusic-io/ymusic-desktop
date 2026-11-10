use bytes::BytesMut;
use serde_json::Value;
use ym_quickjs::{PIPE_CAP, Pipe, PipeError};

const MAX_HEAD_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HeadError {
    #[error(transparent)]
    Pipe(#[from] PipeError),
    #[error("malformed head: {0}")]
    Malformed(String),
    #[error("head missing: the peer ended first")]
    Missing,
}

/// Reads one self-framing compact JSON head; returns it with whatever bytes followed it.
pub(crate) async fn read_json_head(pipe: &Pipe) -> Result<(Value, BytesMut), HeadError> {
    let mut buffer = BytesMut::new();
    loop {
        let Some(chunk) = pipe.receive(PIPE_CAP).await? else {
            return Err(if buffer.iter().all(u8::is_ascii_whitespace) {
                HeadError::Missing
            } else {
                HeadError::Malformed("truncated".into())
            });
        };
        buffer.extend_from_slice(&chunk);
        let mut values = serde_json::Deserializer::from_slice(&buffer).into_iter::<Value>();
        match values.next() {
            Some(Ok(head)) => {
                let end = values.byte_offset();
                return Ok((head, buffer.split_off(end)));
            }
            Some(Err(error)) if !error.is_eof() => return Err(HeadError::Malformed(error.to_string())),
            _ if buffer.len() > MAX_HEAD_BYTES => return Err(HeadError::Malformed("head too large".into())),
            _ => {}
        }
    }
}
