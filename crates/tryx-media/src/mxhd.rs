//! The Turris media blob: a little-endian length, a hand-rolled protobuf
//! header, then the raw H.264 stream. Mirrors the vendor tool's layout.

use crate::MediaError;
use std::io::{Read, Write};
use std::path::Path;

pub const MAGIC: u32 = 0x4D58_4844;
pub const DESCRIPTION: &str = "Tryx media header v1, fps=30, size=1280x720";
pub const VERSION: u64 = 1;
pub const FPS: u64 = 30;
pub const WIDTH: u64 = 1280;
pub const HEIGHT: u64 = 720;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image = 2,
    Video = 4,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub kind: MediaKind,
    pub frames: u64,
    pub width: u64,
    pub height: u64,
    pub fps: u64,
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value & 0x7f) as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    varint(out, field << 3);
    varint(out, value);
}

fn bytes_field(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    varint(out, (field << 3) | 2);
    varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// The header bytes, length prefix included.
pub fn prefix(kind: MediaKind, frames: u64) -> Vec<u8> {
    let mut metadata = Vec::new();
    varint_field(&mut metadata, 1, u64::from(MAGIC));
    bytes_field(&mut metadata, 2, DESCRIPTION.as_bytes());
    varint_field(&mut metadata, 3, kind as u64);
    varint_field(&mut metadata, 4, VERSION);
    varint_field(&mut metadata, 5, FPS);
    varint_field(&mut metadata, 6, WIDTH);
    varint_field(&mut metadata, 7, HEIGHT);
    varint_field(&mut metadata, 8, frames);
    let mut out = (metadata.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&metadata);
    out
}

/// Counts access units in an Annex-B stream by its access unit delimiters
/// (NAL type 9), which the Turris encoder settings emit for every frame.
pub fn count_access_units(stream: &[u8]) -> u64 {
    let mut count = 0;
    let mut i = 0;
    while i + 3 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            if stream[i + 3] & 0x1f == 9 {
                count += 1;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    count
}

/// Wraps `raw` into `output`; returns the frame count written.
pub fn wrap(raw: &Path, output: &Path, kind: MediaKind) -> Result<u64, MediaError> {
    let fail = |message: String| MediaError::Verify {
        path: raw.to_path_buf(),
        message,
    };
    let mut stream = Vec::new();
    std::fs::File::open(raw)?.read_to_end(&mut stream)?;
    if stream.is_empty() {
        return Err(fail("the encoded stream is empty".to_string()));
    }
    let frames = count_access_units(&stream);
    if frames == 0 {
        return Err(fail(
            "the encoded stream has no access unit delimiters".to_string(),
        ));
    }
    if kind == MediaKind::Image && frames != 1 {
        return Err(fail(format!(
            "an image must encode to one frame, found {frames}"
        )));
    }
    let mut file = std::fs::File::create(output)?;
    file.write_all(&prefix(kind, frames))?;
    file.write_all(&stream)?;
    file.flush()?;
    Ok(frames)
}

fn read_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Parses the header at the start of a blob.
pub fn read_header(blob: &[u8]) -> Option<Header> {
    let length = u32::from_le_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    let metadata = blob.get(4..4 + length)?;
    let mut at = 0;
    let mut header = Header {
        kind: MediaKind::Video,
        frames: 0,
        width: 0,
        height: 0,
        fps: 0,
    };
    let mut magic = 0;
    while at < metadata.len() {
        let tag = read_varint(metadata, &mut at)?;
        match (tag >> 3, tag & 7) {
            (field, 0) => {
                let value = read_varint(metadata, &mut at)?;
                match field {
                    1 => magic = value,
                    3 => {
                        header.kind = match value {
                            2 => MediaKind::Image,
                            4 => MediaKind::Video,
                            _ => return None,
                        }
                    }
                    5 => header.fps = value,
                    6 => header.width = value,
                    7 => header.height = value,
                    8 => header.frames = value,
                    _ => {}
                }
            }
            (_, 2) => {
                let len = read_varint(metadata, &mut at)? as usize;
                at = at.checked_add(len)?;
            }
            _ => return None,
        }
    }
    (magic == u64::from(MAGIC)).then_some(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bytes_match_the_vendor_layout() {
        assert_eq!(
            hex(&prefix(MediaKind::Image, 1)),
            "4100000008c490e1ea04122b54727978206d65646961206865616465722076312c206670733d33302c2073697a653d313238307837323018022001281e30800a38d0054001"
        );
    }

    #[test]
    fn wrap_counts_frames_and_the_header_reads_back() {
        let dir = std::env::temp_dir().join(format!("tryx-mxhd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("clip.h264");
        let blob = dir.join("clip.mxhd");
        // Three access units: AUD, SPS-ish payload, slice; four-byte and
        // three-byte start codes mixed, as x264 emits them.
        let mut stream = Vec::new();
        for _ in 0..3 {
            stream.extend_from_slice(&[
                0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65, 0x88,
            ]);
        }
        std::fs::write(&raw, &stream).unwrap();
        assert_eq!(wrap(&raw, &blob, MediaKind::Video).unwrap(), 3);
        let bytes = std::fs::read(&blob).unwrap();
        let header = read_header(&bytes).unwrap();
        assert_eq!(
            header,
            Header {
                kind: MediaKind::Video,
                frames: 3,
                width: 1280,
                height: 720,
                fps: 30
            }
        );
        assert!(bytes.ends_with(&stream));
        assert!(
            wrap(&raw, &blob, MediaKind::Image).is_err(),
            "an image needs exactly one frame"
        );
        std::fs::write(&raw, [0u8, 0, 1, 0x65]).unwrap();
        assert!(
            wrap(&raw, &blob, MediaKind::Video).is_err(),
            "no delimiters"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
