use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::model::{SpikeError, SpikeResult};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;

pub async fn read_limited_line<R>(reader: &mut R) -> SpikeResult<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    let mut too_large = false;
    let mut saw_input = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !saw_input {
                return Ok(None);
            }
            return if too_large {
                Err(SpikeError::FrameTooLarge)
            } else {
                Ok(Some(frame))
            };
        }
        saw_input = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let content = newline.map_or(available, |position| &available[..position]);
        if frame.len().saturating_add(content.len()) > MAX_FRAME_BYTES {
            too_large = true;
        } else if !too_large {
            frame.extend_from_slice(content);
        }
        reader.consume(consumed);
        if newline.is_some() {
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return if too_large {
                Err(SpikeError::FrameTooLarge)
            } else {
                Ok(Some(frame))
            };
        }
    }
}
