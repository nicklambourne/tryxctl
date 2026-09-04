//! TRYX frame codec and stream resynchronisation.
//!
//! Ported from `PrinterFrameCodec` and the resynchronisation helpers in
//! DXVSI/Tryx-Linux-GUI `src/printerprotocol.cpp`, so that both
//! implementations accept and reject exactly the same byte streams.

/// ASCII magic that opens every frame.
pub const MAGIC: &[u8; 4] = b"TRYX";
/// Magic plus the little-endian `u32` payload length.
pub const HEADER_LEN: usize = 8;
/// Largest payload either side will accept.
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MalformedFrame {
    #[error("TRYX response has invalid frame magic")]
    BadMagic,
    #[error("TRYX response payload is too large: {0} bytes")]
    PayloadTooLarge(u32),
}

/// Frames `payload`, or returns `None` when it exceeds [`MAX_PAYLOAD_LEN`].
pub fn encode(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return None;
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Some(frame)
}

/// Removes one complete frame from the front of `buffer` and returns its
/// payload. `Ok(None)` means more bytes are needed. A malformed header clears
/// the buffer, matching the upstream codec.
pub fn take_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, MalformedFrame> {
    if buffer.len() < MAGIC.len() {
        return Ok(None);
    }
    if !buffer.starts_with(MAGIC) {
        buffer.clear();
        return Err(MalformedFrame::BadMagic);
    }
    if buffer.len() < HEADER_LEN {
        return Ok(None);
    }
    let len = payload_len(buffer);
    if len as usize > MAX_PAYLOAD_LEN {
        buffer.clear();
        return Err(MalformedFrame::PayloadTooLarge(len));
    }
    let frame_len = HEADER_LEN + len as usize;
    if buffer.len() < frame_len {
        return Ok(None);
    }
    let payload = buffer[HEADER_LEN..frame_len].to_vec();
    buffer.drain(..frame_len);
    Ok(Some(payload))
}

/// Drops bytes until `buffer` starts with the magic. When no full magic is
/// present, the longest trailing prefix of the magic is kept so a header split
/// across two reads still assembles. Returns the number of bytes dropped.
pub fn discard_bytes_before_frame_magic(buffer: &mut Vec<u8>) -> usize {
    if buffer.is_empty() || buffer.starts_with(MAGIC) {
        return 0;
    }
    if let Some(index) = find(buffer, MAGIC) {
        buffer.drain(..index);
        return index;
    }
    let mut preserved = (MAGIC.len() - 1).min(buffer.len());
    while preserved > 0 && buffer[buffer.len() - preserved..] != MAGIC[..preserved] {
        preserved -= 1;
    }
    let discarded = buffer.len() - preserved;
    buffer.drain(..discarded);
    discarded
}

/// Index of the first magic that heads a complete, plausibly sized frame.
pub fn complete_plausible_frame_index(buffer: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(offset) = find(&buffer[start..], MAGIC) {
        let index = start + offset;
        let remaining = buffer.len() - index;
        if remaining >= HEADER_LEN {
            let len = payload_len(&buffer[index..]) as usize;
            if len <= MAX_PAYLOAD_LEN && remaining >= HEADER_LEN + len {
                return Some(index);
            }
        }
        start = index + 1;
    }
    None
}

/// Drops bytes until `buffer` starts with a header whose length is plausible.
///
/// Protobuf strings may legitimately contain the ASCII bytes `TRYX`. If a
/// damaged preceding frame leaves such a string at the front of the stream,
/// its following text must not be accepted as a frame length: one byte is
/// dropped and the magic search continues so a later real header can still be
/// recovered.
pub fn discard_bytes_before_plausible_frame(buffer: &mut Vec<u8>) -> usize {
    let mut discarded = 0;
    while !buffer.is_empty() {
        discarded += discard_bytes_before_frame_magic(buffer);
        if buffer.len() < HEADER_LEN {
            return discarded;
        }
        if payload_len(buffer) as usize <= MAX_PAYLOAD_LEN {
            return discarded;
        }
        buffer.remove(0);
        discarded += 1;
    }
    discarded
}

fn payload_len(header: &[u8]) -> u32 {
    u32::from_le_bytes([header[4], header[5], header[6], header[7]])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        encode(payload).expect("payload fits")
    }

    #[test]
    fn encode_prefixes_magic_and_little_endian_length() {
        assert_eq!(frame(b"abc"), b"TRYX\x03\x00\x00\x00abc");
        assert_eq!(frame(b""), b"TRYX\x00\x00\x00\x00");
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        assert!(encode(&vec![0u8; MAX_PAYLOAD_LEN]).is_some());
        assert!(encode(&vec![0u8; MAX_PAYLOAD_LEN + 1]).is_none());
    }

    #[test]
    fn take_frame_round_trips_consecutive_frames() {
        let mut buffer = frame(b"one");
        buffer.extend(frame(b"two"));
        assert_eq!(take_frame(&mut buffer), Ok(Some(b"one".to_vec())));
        assert_eq!(take_frame(&mut buffer), Ok(Some(b"two".to_vec())));
        assert_eq!(take_frame(&mut buffer), Ok(None));
        assert!(buffer.is_empty());
    }

    #[test]
    fn take_frame_waits_for_header_and_payload() {
        let full = frame(b"payload");
        for cut in [0, 3, 4, 7, 8, 10] {
            let mut partial = full[..cut].to_vec();
            assert_eq!(take_frame(&mut partial), Ok(None), "cut at {cut}");
            assert_eq!(partial, full[..cut], "buffer untouched at {cut}");
        }
    }

    #[test]
    fn take_frame_clears_buffer_on_bad_magic() {
        let mut buffer = b"TRYZ\x01\x00\x00\x00x".to_vec();
        assert_eq!(take_frame(&mut buffer), Err(MalformedFrame::BadMagic));
        assert!(buffer.is_empty());
    }

    #[test]
    fn take_frame_clears_buffer_on_oversized_length() {
        let mut buffer = b"TRYX\x01\x00\x10\x00".to_vec();
        assert_eq!(
            take_frame(&mut buffer),
            Err(MalformedFrame::PayloadTooLarge(MAX_PAYLOAD_LEN as u32 + 1))
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn discard_before_magic_drops_leading_garbage() {
        let mut buffer = b"junkTRYX\x00\x00\x00\x00".to_vec();
        assert_eq!(discard_bytes_before_frame_magic(&mut buffer), 4);
        assert_eq!(buffer, b"TRYX\x00\x00\x00\x00");
        assert_eq!(discard_bytes_before_frame_magic(&mut buffer), 0);
    }

    #[test]
    fn discard_before_magic_keeps_a_split_magic_prefix() {
        let mut buffer = b"noise..TR".to_vec();
        assert_eq!(discard_bytes_before_frame_magic(&mut buffer), 7);
        assert_eq!(buffer, b"TR");

        let mut buffer = b"noise".to_vec();
        assert_eq!(discard_bytes_before_frame_magic(&mut buffer), 5);
        assert!(buffer.is_empty());
    }

    #[test]
    fn plausible_frame_index_skips_magic_inside_text() {
        let mut stream = b"say TRYX\xff\xff\xff\xff now".to_vec();
        let real_at = stream.len();
        stream.extend(frame(b"real"));
        assert_eq!(complete_plausible_frame_index(&stream), Some(real_at));
        assert_eq!(
            complete_plausible_frame_index(b"TRYX\x01\x00\x00\x00"),
            None
        );
    }

    #[test]
    fn discard_before_plausible_frame_recovers_after_embedded_magic() {
        let mut buffer = b"TRYX\xff\xff\xff\xff".to_vec();
        buffer.extend(frame(b"ok"));
        let discarded = discard_bytes_before_plausible_frame(&mut buffer);
        assert_eq!(discarded, 8);
        assert_eq!(take_frame(&mut buffer), Ok(Some(b"ok".to_vec())));
    }
}
