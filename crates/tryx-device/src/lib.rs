//! Discovery and product profiles for TRYX cooler displays.

pub mod discovery;
pub mod product;

pub use discovery::{Discovery, DiscoveryError, discover};
pub use product::Product;
