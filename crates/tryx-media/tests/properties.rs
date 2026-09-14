//! Properties of names, trims, transforms, and the Turris header over
//! generated input.

use proptest::prelude::*;
use tryx_media::check::{Trim, sanitize_stem};
use tryx_media::mxhd::{self, MediaKind};
use tryx_media::transform::{Mode, Transform};

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![
        Just(Mode::Fit),
        Just(Mode::Fill),
        Just(Mode::Crop),
        Just(Mode::Stretch)
    ]
}

proptest! {
    #[test]
    fn sanitised_stems_are_always_safe_display_names(stem in "\\PC{0,300}") {
        let safe = sanitize_stem(&stem);
        prop_assert!(!safe.is_empty() && safe.chars().count() <= 100);
        prop_assert!(!safe.starts_with('.') && !safe.starts_with('-'));
        prop_assert!(safe.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)), "{}", safe);
        // Already safe stems pass through unchanged.
        prop_assert_eq!(sanitize_stem(&safe), safe);
    }

    #[test]
    fn trims_parse_to_forward_ranges(text in "\\PC{0,24}|-?[0-9]{0,3}(\\.[0-9]{1,2})?-[0-9]{0,3}(\\.[0-9]{1,2})?") {
        if let Some(trim) = Trim::parse(&text) {
            prop_assert!(trim.start >= 0.0);
            if let Some(end) = trim.end {
                prop_assert!(end > trim.start);
                prop_assert!(trim.length(None).unwrap() > 0.0);
            }
            prop_assert!(trim.length(Some(3.0)).unwrap() >= 0.0 || trim.end.is_some());
        }
    }

    #[test]
    fn valid_transforms_always_build_filters(
        mode in mode(),
        turns in 0u32..6,
        zoom in 500u32..4500,
        focus_x in 0u32..11000,
        focus_y in 0u32..11000,
        background in 0u32..0x0200_0000,
        width in 16u32..4000,
        height in 16u32..4000,
    ) {
        let transform = Transform {
            mode,
            rotation_quarter_turns: turns,
            zoom_permille: zoom,
            focus_x,
            focus_y,
            background_rgb: background,
        };
        if transform.validate().is_ok() {
            let video = transform.video_filter(width, height, 30);
            let image = transform.image_filter(width, height);
            prop_assert!(!video.is_empty() && !image.is_empty());
            prop_assert!(video.contains(&format!("{width}")), "{}", video);
        }
    }

    #[test]
    fn turris_headers_read_back(frames in any::<u64>(), image in any::<bool>(), tail in proptest::collection::vec(any::<u8>(), 0..64)) {
        let kind = if image { MediaKind::Image } else { MediaKind::Video };
        let mut blob = mxhd::prefix(kind, frames);
        blob.extend_from_slice(&tail);
        let header = mxhd::read_header(&blob).unwrap();
        prop_assert_eq!(header.kind, kind);
        prop_assert_eq!(header.frames, frames);
        prop_assert_eq!((header.width, header.height, header.fps), (1280, 720, 30));
    }

    #[test]
    fn arbitrary_blobs_never_panic_the_header_reader(blob in proptest::collection::vec(any::<u8>(), 0..128)) {
        let _ = mxhd::read_header(&blob);
        let _ = mxhd::count_access_units(&blob);
    }
}
