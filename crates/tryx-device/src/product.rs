//! Product profiles for the KANALI printer-class displays, keyed by USB
//! product ID. Capability gates mirror DXVSI/Tryx-Linux-GUI
//! (`printerProductProfileForId`), so unsupported operations are rejected
//! before any USB traffic starts.

use serde::Serialize;

/// USB vendor ID shared by every KANALI printer-class TRYX display.
pub const VENDOR_ID: u16 = 0x391a;
/// Transitional Rockchip USB gadget identity (`391a:0006`) seen while a
/// display boots or updates. It is discovered but never opened.
pub const ROCKCHIP_GADGET_PRODUCT_ID: u16 = 0x0006;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Product {
    /// Panorama SE, `391a:1021`. Full feature set.
    PanoramaSe,
    /// Panorama, `391a:1011`. Everything except firmware flashing.
    Panorama,
    /// Turris 620, `391a:2011`. Acknowledged media upload only.
    Turris620,
}

/// Native resolution the device expects prepared media to have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MediaGeometry {
    pub width: u32,
    pub height: u32,
}

/// How the device is kept alive between operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdleMode {
    /// A periodic overlay layout and ping keep the display session open.
    OverlayLayout,
    /// No session: the transport is opened only for an explicit transfer.
    TransferOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub media_upload: bool,
    pub media_catalog: bool,
    pub display_configuration: bool,
    pub overlay_metrics: bool,
    pub firmware_flash: bool,
}

impl Product {
    pub const ALL: [Product; 3] = [Product::PanoramaSe, Product::Panorama, Product::Turris620];

    pub fn from_product_id(product_id: u16) -> Option<Product> {
        Product::ALL
            .into_iter()
            .find(|product| product.product_id() == product_id)
    }

    pub fn product_id(self) -> u16 {
        match self {
            Product::PanoramaSe => 0x1021,
            Product::Panorama => 0x1011,
            Product::Turris620 => 0x2011,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Product::PanoramaSe => "Panorama SE",
            Product::Panorama => "Panorama",
            Product::Turris620 => "Turris 620",
        }
    }

    pub fn media_geometry(self) -> MediaGeometry {
        match self {
            Product::PanoramaSe | Product::Panorama => MediaGeometry {
                width: 2240,
                height: 1080,
            },
            Product::Turris620 => MediaGeometry {
                width: 1280,
                height: 720,
            },
        }
    }

    /// Suffix the device requires on every uploaded media name.
    pub fn media_name_suffix(self) -> &'static str {
        match self {
            Product::PanoramaSe | Product::Panorama => ".h264_2240x1080",
            Product::Turris620 => ".h264_1280x720",
        }
    }

    pub fn idle_mode(self) -> IdleMode {
        match self {
            Product::PanoramaSe | Product::Panorama => IdleMode::OverlayLayout,
            Product::Turris620 => IdleMode::TransferOnly,
        }
    }

    pub fn capabilities(self) -> Capabilities {
        match self {
            Product::PanoramaSe => Capabilities {
                media_upload: true,
                media_catalog: true,
                display_configuration: true,
                overlay_metrics: true,
                firmware_flash: true,
            },
            Product::Panorama => Capabilities {
                media_upload: true,
                media_catalog: true,
                display_configuration: true,
                overlay_metrics: true,
                firmware_flash: false,
            },
            Product::Turris620 => Capabilities {
                media_upload: true,
                media_catalog: false,
                display_configuration: false,
                overlay_metrics: false,
                firmware_flash: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_ids_round_trip() {
        for product in Product::ALL {
            assert_eq!(
                Product::from_product_id(product.product_id()),
                Some(product)
            );
        }
        assert_eq!(Product::from_product_id(ROCKCHIP_GADGET_PRODUCT_ID), None);
        assert_eq!(Product::from_product_id(0xffff), None);
    }

    #[test]
    fn media_suffix_matches_geometry() {
        for product in Product::ALL {
            let geometry = product.media_geometry();
            assert_eq!(
                product.media_name_suffix(),
                format!(".h264_{}x{}", geometry.width, geometry.height)
            );
        }
    }

    #[test]
    fn turris_is_upload_only() {
        let capabilities = Product::Turris620.capabilities();
        assert!(capabilities.media_upload);
        assert!(!capabilities.media_catalog);
        assert!(!capabilities.display_configuration);
        assert!(!capabilities.overlay_metrics);
        assert!(!capabilities.firmware_flash);
        assert_eq!(Product::Turris620.idle_mode(), IdleMode::TransferOnly);
    }

    #[test]
    fn only_panorama_se_flashes_firmware() {
        assert!(Product::PanoramaSe.capabilities().firmware_flash);
        assert!(!Product::Panorama.capabilities().firmware_flash);
    }
}
