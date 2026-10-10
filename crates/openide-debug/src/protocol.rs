//! DAP framing: byte lengths, bounded ASCII headers, bounded UTF-8 JSON bodies.
use crate::{error, DebugError};
use serde_json::Value;
use std::io::{BufRead, Read};

pub(crate) const MAX_FRAME: usize = 2 * 1024 * 1024;
pub(crate) const MAX_OUTGOING: usize = 1024 * 1024;
const MAX_HEADER: usize = 8 * 1024;

pub(crate) fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>, DebugError> {
    let mut header = Vec::new();
    let mut length = None;
    loop {
        let start = header.len();
        let mut line = (&mut *reader).take((MAX_HEADER.saturating_sub(start) + 1) as u64);
        let count = line
            .read_until(b'\n', &mut header)
            .map_err(|e| error(format!("DAP header: {e}")))?;
        if count == 0 && start == 0 {
            return Ok(None);
        }
        if count == 0 || header.len() > MAX_HEADER || !header.ends_with(b"\r\n") {
            return Err(error("Truncated or oversized DAP header"));
        }
        let bytes = &header[start..header.len() - 2];
        if bytes.is_empty() {
            break;
        }
        if !bytes.is_ascii() {
            return Err(error("DAP headers must be ASCII"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| error("Invalid DAP header"))?;
        let (name, value) = text
            .split_once(':')
            .ok_or_else(|| error("Invalid DAP header field"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(error("Duplicate DAP Content-Length"));
            }
            let count = value
                .trim()
                .parse::<usize>()
                .map_err(|_| error("Invalid DAP Content-Length"))?;
            if count == 0 || count > MAX_FRAME {
                return Err(error("DAP message exceeds byte limit"));
            }
            length = Some(count);
        }
    }
    let length = length.ok_or_else(|| error("Missing DAP Content-Length"))?;
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| error(format!("DAP body: {e}")))?;
    let message: Value =
        serde_json::from_slice(&body).map_err(|e| error(format!("DAP JSON: {e}")))?;
    if !message.is_object() {
        return Err(error("DAP message must be an object"));
    }
    Ok(Some(message))
}

pub(crate) fn frame(message: &Value) -> Result<Vec<u8>, DebugError> {
    let body = serde_json::to_vec(message).map_err(|e| error(e.to_string()))?;
    if body.len() > MAX_OUTGOING {
        return Err(error("DAP outgoing message exceeds byte limit"));
    }
    let mut result = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    result.extend(body);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    #[test]
    fn fragmented_unicode_frames_are_byte_counted_and_consecutive() {
        let value =
            serde_json::json!({"type":"event","event":"output","body":{"output":"café 🦀"}});
        let one = frame(&value).unwrap();
        let mut both = one.clone();
        both.extend(one);
        let mut reader = BufReader::with_capacity(1, Cursor::new(both));
        assert_eq!(read_message(&mut reader).unwrap(), Some(value.clone()));
        assert_eq!(read_message(&mut reader).unwrap(), Some(value));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }
    #[test]
    fn malformed_oversized_or_truncated_frames_fail_before_allocating_unbounded_data() {
        for input in [
            b"Content-Length: 999999999\r\n\r\n".to_vec(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            b"Content-Length: 2\n\n{}".to_vec(),
            b"Content-Length: 5\r\n\r\n{}".to_vec(),
            b"Missing: 2\r\n\r\n{}".to_vec(),
            vec![b'a'; MAX_HEADER + 2],
        ] {
            assert!(read_message(&mut Cursor::new(input)).is_err());
        }
    }
}
