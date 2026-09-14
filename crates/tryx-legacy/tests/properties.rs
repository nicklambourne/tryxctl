//! Properties of the frame codec and the adb parsers over generated input.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;
use tryx_legacy::adb;
use tryx_legacy::frame::{self, FRAME_MARKER};

proptest! {
    #[test]
    fn stuffing_round_trips_and_hides_the_marker(data in vec(any::<u8>(), 0..512)) {
        let escaped = frame::escape(&data);
        prop_assert!(!escaped.contains(&FRAME_MARKER));
        prop_assert_eq!(frame::unescape(&escaped), data);
    }

    #[test]
    fn requests_carry_their_command_and_content_exactly(
        command in "[A-Za-z]{1,24}",
        content in "\\PC{0,400}",
        ack in any::<u32>(),
    ) {
        let bytes = frame::build_frame("POST", &command, &content, "1", ack).unwrap();
        prop_assert_eq!(bytes.iter().filter(|b| **b == FRAME_MARKER).count(), 2);
        let parsed = frame::parse_response(&bytes).unwrap();
        prop_assert!(parsed.checksum_ok && parsed.length_ok);
        // For a request the first two tokens are the method and the command.
        prop_assert_eq!(parsed.version, "POST");
        prop_assert_eq!(parsed.status, command);
        prop_assert_eq!(parsed.body, content);
        let ack_header = format!("AckNumber={ack}\r\n");
        prop_assert!(parsed.raw.contains(&ack_header));
    }

    #[test]
    fn frames_reassemble_however_the_stream_is_cut(
        bodies in vec("\\PC{0,64}", 1..6),
        noise in vec(any::<u8>().prop_filter("not a marker", |b| *b != FRAME_MARKER), 0..16),
        cut in any::<Index>(),
    ) {
        let frames: Vec<Vec<u8>> = bodies
            .iter()
            .map(|body| frame::wrap(format!("1 200\r\n\r\n{body}").as_bytes()).unwrap())
            .collect();
        let mut stream = noise;
        for bytes in &frames {
            stream.extend_from_slice(bytes);
        }
        let split = cut.index(stream.len() + 1);
        let mut buffer = stream[..split].to_vec();
        let mut taken = Vec::new();
        while let Some(bytes) = frame::take_frame(&mut buffer) {
            taken.push(bytes);
        }
        buffer.extend_from_slice(&stream[split..]);
        while let Some(bytes) = frame::take_frame(&mut buffer) {
            taken.push(bytes);
        }
        prop_assert!(buffer.is_empty());
        prop_assert_eq!(&taken, &frames);
        for (bytes, body) in taken.iter().zip(&bodies) {
            prop_assert_eq!(&frame::parse_response(bytes).unwrap().body, body);
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_decoder(data in vec(any::<u8>(), 0..300)) {
        let _ = frame::parse_response(&data);
        let _ = frame::unescape(&data);
        let mut buffer = data;
        while frame::take_frame(&mut buffer).is_some() {}
    }

    #[test]
    fn adb_output_never_panics_the_parsers(text in "\\PC{0,400}") {
        let _ = adb::parse_devices(&text);
        let _ = adb::parse_stat_listing(&text);
        let _ = adb::parse_df(&text);
    }

    #[test]
    fn stat_listings_round_trip(files in vec(("[A-Za-z0-9_.-]{1,40}", any::<u32>()), 0..12)) {
        let text: String = files
            .iter()
            .map(|(name, size)| format!("{size} /sdcard/pcMedia/{name}\n"))
            .collect();
        let parsed = adb::parse_stat_listing(&text);
        prop_assert_eq!(parsed.len(), files.len());
        for (file, (name, size)) in parsed.iter().zip(&files) {
            prop_assert_eq!(&file.name, name);
            prop_assert_eq!(file.size, u64::from(*size));
        }
    }

    #[test]
    fn safe_media_names_need_no_quoting_in_a_shell(name in "\\PC{0,48}|[A-Za-z0-9_.-]{0,140}") {
        if adb::is_safe_media_name(&name) {
            prop_assert!(!name.is_empty() && name.len() <= 128);
            prop_assert!(!name.starts_with('.'));
            prop_assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)));
        }
    }
}
