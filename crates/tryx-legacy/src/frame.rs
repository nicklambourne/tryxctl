//! Byte-stuffed text frames of the legacy protocol.
//!
//! ```text
//! 0x5A  escape( len_be16 | text | crc8 )  0x5A
//! text = "<STATE> <command> <version>\r\n"
//!        "ContentType=json\r\nContentLength=<n>\r\nAckNumber=<seq>\r\n\r\n"
//!        <json>
//! ```
//!
//! `len_be16` counts the text plus five bytes of framing overhead; `crc8` is
//! the byte sum modulo 256 of the length and text. Inside a frame the marker
//! `0x5A` becomes `0x5B 0x01` and `0x5B` becomes `0x5B 0x02`, so a marker byte
//! never occurs between the delimiters. Ported from upstream
//! `src/core/protocol.cpp`; the checksum and length are verified here but,
//! as upstream, a mismatch does not reject the frame.

use crate::LegacyError;

pub const FRAME_MARKER: u8 = 0x5A;
pub const ESCAPE_MARKER: u8 = 0x5B;
const ESCAPED_FRAME_MARKER: u8 = 0x01;
const ESCAPED_ESCAPE_MARKER: u8 = 0x02;
/// Length prefix, checksum, and the two delimiters.
const FRAMING_OVERHEAD: usize = 5;

/// Byte sum modulo 256.
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

pub fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    for &byte in data {
        match byte {
            FRAME_MARKER => out.extend_from_slice(&[ESCAPE_MARKER, ESCAPED_FRAME_MARKER]),
            ESCAPE_MARKER => out.extend_from_slice(&[ESCAPE_MARKER, ESCAPED_ESCAPE_MARKER]),
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape`]. An escape byte followed by anything else is kept as
/// is, matching upstream.
pub fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index] == ESCAPE_MARKER && index + 1 < data.len() {
            match data[index + 1] {
                ESCAPED_FRAME_MARKER => {
                    out.push(FRAME_MARKER);
                    index += 2;
                    continue;
                }
                ESCAPED_ESCAPE_MARKER => {
                    out.push(ESCAPE_MARKER);
                    index += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(data[index]);
        index += 1;
    }
    out
}

/// Builds the wire bytes of one request.
pub fn build_frame(
    state: &str,
    command: &str,
    content: &str,
    version: &str,
    ack_number: u32,
) -> Result<Vec<u8>, LegacyError> {
    let text = format!(
        "{state} {command} {version}\r\nContentType=json\r\nContentLength={}\r\nAckNumber={ack_number}\r\n\r\n{content}",
        content.len()
    );
    wrap(text.as_bytes())
}

/// Wraps message text in length prefix, checksum, byte stuffing, and
/// delimiters. Requests and device replies share this layout.
pub fn wrap(text: &[u8]) -> Result<Vec<u8>, LegacyError> {
    let wire_length = u16::try_from(text.len() + FRAMING_OVERHEAD)
        .map_err(|_| LegacyError::RequestTooLong(text.len()))?;
    let mut raw = Vec::with_capacity(text.len() + 3);
    raw.extend_from_slice(&wire_length.to_be_bytes());
    raw.extend_from_slice(text);
    raw.push(crc8(&raw));

    let mut frame = Vec::with_capacity(raw.len() + 2);
    frame.push(FRAME_MARKER);
    frame.extend(escape(&raw));
    frame.push(FRAME_MARKER);
    Ok(frame)
}

/// A decoded response frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The text between the length prefix and the checksum.
    pub raw: String,
    /// Everything after the first blank line.
    pub body: String,
    /// `body` parsed as JSON, when it is JSON.
    pub json: Option<serde_json::Value>,
    /// First token of the status line.
    pub version: String,
    /// Second token of the status line.
    pub status: String,
    /// Whether the trailing checksum matched.
    pub checksum_ok: bool,
    /// Whether the length prefix matched the frame.
    pub length_ok: bool,
}

/// Decodes one complete frame (both delimiters included).
pub fn parse_response(data: &[u8]) -> Option<Response> {
    if data.len() < 4 || data[0] != FRAME_MARKER || data[data.len() - 1] != FRAME_MARKER {
        return None;
    }
    let decoded = unescape(&data[1..data.len() - 1]);
    if decoded.len() < 3 {
        return None;
    }
    let (payload, checksum) = decoded.split_at(decoded.len() - 1);
    let declared_length = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let text = &payload[2..];
    let raw = String::from_utf8_lossy(text).into_owned();

    let mut response = Response {
        raw: raw.clone(),
        body: String::new(),
        json: None,
        version: String::new(),
        status: String::new(),
        checksum_ok: crc8(payload) == checksum[0],
        length_ok: declared_length == text.len() + FRAMING_OVERHEAD,
    };
    if let Some((headers, body)) = raw.split_once("\r\n\r\n") {
        response.body = body.to_string();
        if !body.is_empty() {
            response.json = serde_json::from_str(body).ok();
        }
        let status_line = headers.split("\r\n").next().unwrap_or(headers);
        let mut tokens = status_line.split_whitespace();
        response.version = tokens.next().unwrap_or_default().to_string();
        response.status = tokens.next().unwrap_or_default().to_string();
    }
    Some(response)
}

/// Removes the first complete frame from `buffer`, discarding any bytes that
/// precede its opening marker. Marker bytes never occur inside a frame, so
/// the next marker after the opener always closes it.
pub fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buffer.iter().position(|&byte| byte == FRAME_MARKER)?;
    let end = start
        + 1
        + buffer[start + 1..]
            .iter()
            .position(|&byte| byte == FRAME_MARKER)?;
    let frame = buffer[start..=end].to_vec();
    buffer.drain(..=end);
    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_is_the_byte_sum_modulo_256() {
        assert_eq!(crc8(&[]), 0);
        assert_eq!(crc8(&[0x01, 0x02]), 0x03);
        assert_eq!(crc8(&[0xff, 0x02]), 0x01);
    }

    #[test]
    fn escape_round_trips_marker_bytes() {
        let raw = [0x00, FRAME_MARKER, 0x41, ESCAPE_MARKER, 0xff];
        let escaped = escape(&raw);
        assert_eq!(escaped, [0x00, 0x5b, 0x01, 0x41, 0x5b, 0x02, 0xff]);
        assert_eq!(unescape(&escaped), raw);
    }

    #[test]
    fn unescape_keeps_unknown_escape_pairs() {
        assert_eq!(unescape(&[ESCAPE_MARKER, 0x07]), [ESCAPE_MARKER, 0x07]);
        assert_eq!(unescape(&[ESCAPE_MARKER]), [ESCAPE_MARKER]);
    }

    #[test]
    fn build_frame_matches_the_upstream_layout() {
        let frame = build_frame("POST", "conn", "", "1", 1).unwrap();
        let text = b"POST conn 1\r\nContentType=json\r\nContentLength=0\r\nAckNumber=1\r\n\r\n";
        assert_eq!(text.len(), 63);
        let mut expected = vec![FRAME_MARKER, 0x00, 63 + 5];
        expected.extend_from_slice(text);
        expected.push(crc8(&expected[1..]));
        expected.push(FRAME_MARKER);
        assert_eq!(frame, expected);
    }

    #[test]
    fn build_frame_escapes_json_brackets() {
        // '[' is 0x5B, the escape marker, so JSON arrays must be stuffed.
        let frame = build_frame("POST", "mediaDelete", r#"{"include":["a.mp4"]}"#, "1", 7).unwrap();
        let inner = &frame[1..frame.len() - 1];
        assert!(inner.iter().all(|&byte| byte != FRAME_MARKER));
        assert!(inner.windows(2).any(|pair| pair == [ESCAPE_MARKER, 0x02]));
        let decoded = unescape(inner);
        let text = &decoded[2..decoded.len() - 1];
        assert!(text.ends_with(br#"{"include":["a.mp4"]}"#));
    }

    #[test]
    fn build_frame_rejects_requests_over_u16() {
        let content = "x".repeat(70_000);
        assert!(matches!(
            build_frame("POST", "all", &content, "1", 1),
            Err(LegacyError::RequestTooLong(_))
        ));
    }

    fn device_reply(text: &[u8]) -> Vec<u8> {
        wrap(text).unwrap()
    }

    #[test]
    fn parse_response_splits_status_headers_and_json_body() {
        let frame = device_reply(
            b"1 OK\r\nContentType=json\r\n\r\n{\"productId\":\"cm01_se\",\"list\":[1]}",
        );
        let response = parse_response(&frame).unwrap();
        assert_eq!(response.version, "1");
        assert_eq!(response.status, "OK");
        assert_eq!(response.body, "{\"productId\":\"cm01_se\",\"list\":[1]}");
        assert_eq!(response.json.unwrap()["productId"], "cm01_se");
        assert!(response.checksum_ok);
        assert!(response.length_ok);
    }

    #[test]
    fn parse_response_flags_bad_checksum_without_rejecting() {
        let mut frame = device_reply(b"1 OK\r\n\r\n{}");
        let checksum_index = frame.len() - 2;
        frame[checksum_index] = frame[checksum_index].wrapping_add(1);
        let response = parse_response(&frame).unwrap();
        assert!(!response.checksum_ok);
        assert_eq!(response.body, "{}");
    }

    #[test]
    fn parse_response_rejects_short_or_unframed_data() {
        assert_eq!(parse_response(&[]), None);
        assert_eq!(parse_response(&[FRAME_MARKER, 0x00, FRAME_MARKER]), None);
        assert_eq!(parse_response(b"1 OK"), None);
        assert_eq!(
            parse_response(&[FRAME_MARKER, 0x00, 0x01, FRAME_MARKER]),
            None
        );
    }

    #[test]
    fn take_frame_skips_noise_and_splits_consecutive_frames() {
        let first = device_reply(b"1 OK\r\n\r\n{\"a\":1}");
        let second = device_reply(b"1 OK\r\n\r\n{\"b\":2}");
        let mut buffer = vec![0x00, 0x11];
        buffer.extend(&first);
        buffer.extend(&second[..3]);
        assert_eq!(take_frame(&mut buffer), Some(first));
        assert_eq!(take_frame(&mut buffer), None);
        buffer.extend(&second[3..]);
        assert_eq!(take_frame(&mut buffer), Some(second));
        assert!(buffer.is_empty());
    }
}
