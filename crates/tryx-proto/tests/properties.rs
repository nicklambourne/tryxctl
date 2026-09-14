//! Properties of the TRYX frame codec over generated input.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;
use tryx_proto::frame;

proptest! {
    #[test]
    fn frames_reassemble_however_the_stream_is_cut(
        payloads in vec(vec(any::<u8>(), 0..3000), 1..5),
        cut in any::<Index>(),
    ) {
        let mut stream = Vec::new();
        for payload in &payloads {
            stream.extend(frame::encode(payload).unwrap());
        }
        let split = cut.index(stream.len() + 1);
        let mut buffer = stream[..split].to_vec();
        let mut taken = Vec::new();
        while let Ok(Some(payload)) = frame::take_frame(&mut buffer) {
            taken.push(payload);
        }
        buffer.extend_from_slice(&stream[split..]);
        while let Ok(Some(payload)) = frame::take_frame(&mut buffer) {
            taken.push(payload);
        }
        prop_assert!(buffer.is_empty());
        prop_assert_eq!(taken, payloads);
    }

    #[test]
    fn resynchronising_skips_garbage_to_the_next_frame(
        garbage in vec(any::<u8>(), 0..64),
        payload in vec(any::<u8>(), 0..256),
    ) {
        let encoded = frame::encode(&payload).unwrap();
        let mut buffer = garbage.clone();
        buffer.extend_from_slice(&encoded);
        // Garbage that spells the magic before the frame is another case: it
        // can pass for a header of its own.
        let magic_in_garbage = buffer[..garbage.len() + 3]
            .windows(4)
            .take(garbage.len())
            .any(|window| window == frame::MAGIC);
        prop_assume!(!magic_in_garbage);
        prop_assert_eq!(frame::complete_plausible_frame_index(&buffer), Some(garbage.len()));
        prop_assert_eq!(frame::discard_bytes_before_plausible_frame(&mut buffer), garbage.len());
        prop_assert_eq!(frame::take_frame(&mut buffer), Ok(Some(payload)));
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_codec(data in vec(any::<u8>(), 0..200)) {
        let mut buffer = data.clone();
        while let Ok(Some(_)) = frame::take_frame(&mut buffer) {}
        let mut buffer = data.clone();
        let dropped = frame::discard_bytes_before_frame_magic(&mut buffer);
        prop_assert_eq!(dropped + buffer.len(), data.len());
        let mut buffer = data.clone();
        let dropped = frame::discard_bytes_before_plausible_frame(&mut buffer);
        prop_assert_eq!(dropped + buffer.len(), data.len());
        let _ = frame::complete_plausible_frame_index(&data);
    }
}
