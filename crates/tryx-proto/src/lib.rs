//! Wire protocol for TRYX printer-class displays (Panorama SE, Panorama,
//! Turris 620).
//!
//! A message on the bulk USB pipe is a [`frame`]: the ASCII magic `TRYX`, a
//! little-endian `u32` payload length, then a serialised
//! [`wire::v1::Request`] or [`wire::v1::Response`].

pub mod frame;

pub mod wire {
    // Generated code: the `Request`/`Response` body enums carry one large
    // variant (`UserConfiguration`), which clippy flags but prost owns.
    #[allow(clippy::large_enum_variant)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/panorama.wire.v1.rs"));
    }
}
