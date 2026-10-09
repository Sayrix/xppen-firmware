use xppen_firmware_core::{
    Result,
    image::{self, Firmware},
};

/// Firmware choices reference embedded, checksum-pinned images.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Stock,
    Fast,
    FullPressure,
    NoSmoothing,
}

impl Choice {
    pub const ALL: [Self; 4] = [
        Self::Stock,
        Self::Fast,
        Self::FullPressure,
        Self::NoSmoothing,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Stock => "stock",
            Self::Fast => "fast",
            Self::FullPressure => "full-pressure",
            Self::NoSmoothing => "no-smoothing",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stock => "Stock firmware",
            Self::Fast => "~330 Hz · No smoothing",
            Self::FullPressure => "280 Hz · No smoothing",
            Self::NoSmoothing => "240 Hz · No smoothing",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Stock => "Original factory behavior",
            Self::Fast => "Slower pressure rate",
            Self::FullPressure => "Full pressure rate",
            Self::NoSmoothing => "Stock timing, smoothing bypassed",
        }
    }

    pub fn details(self) -> &'static [&'static str] {
        match self {
            Self::Stock => &[
                "Original Deco Fun XS firmware.",
                "Position: approximately 240 Hz. Pressure acquired every position cycle.",
                "37 excitation periods. Original smoothing, scan timing and pen buttons.",
            ],
            Self::Fast => &[
                "Position: approximately 330 Hz; 337 Hz observed on one tablet.",
                "Pressure acquired every third cycle: approximately 110–112 Hz at those position rates.",
                "Excitation: 31 periods (stock: 37). Reduced excitation can affect chatter and signal quality.",
                "Coordinate smoothing bypassed. Optimized GPIO resets, calculation and copy loops, and acquisition bookkeeping; reduced diagnostic overhead. Pen buttons supported.",
                "Measured scan rates vary with pen angle and contact; USB delivery rates may differ.",
            ],
            Self::FullPressure => &[
                "Position and pressure acquired on every cycle, with no pressure decimation.",
                "Position: approximately 280 Hz observed on one tablet. Pressure cadence follows the same full-cycle schedule.",
                "Measured scan rates vary with pen angle and contact.",
                "Optimized GPIO resets, calculation and copy loops, and acquisition bookkeeping; reduced diagnostic overhead. Pen buttons supported.",
                "Excitation: 31 periods (stock: 37). Coordinate smoothing bypassed.",
            ],
            Self::NoSmoothing => &[
                "Stock firmware with only the main coordinate smoothing bypassed.",
                "Approximately 240 Hz position, with pressure acquired every cycle.",
                "37 excitation periods. Original acquisition, pressure processing, timing and pen buttons.",
                "Stock scan timing, calculation routines and downstream movement gates.",
            ],
        }
    }

    pub fn firmware(self) -> Result<Firmware> {
        let (bytes, hash): (&[u8], &str) = match self {
            Self::Stock => (
                include_bytes!("../firmware/stock-deco-fun-xs/firmware.bin"),
                image::STOCK_HASH,
            ),
            Self::Fast => (
                include_bytes!(
                    "../firmware/deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31/firmware.bin"
                ),
                image::OPTIMIZED_HASH,
            ),
            Self::FullPressure => (
                include_bytes!(
                    "../firmware/deco-fun-xs-no-smoothing-full-pressure-gpio-reset-excitation-31/firmware.bin"
                ),
                "07e68dc994b66e64abc1f6c5a7084ec4938ea14dd359a0a8337c733ab6d0d40d",
            ),
            Self::NoSmoothing => (
                include_bytes!("../firmware/deco-fun-xs-no-smoothing/firmware.bin"),
                "cbc01e506d8ce863121390c8815b4c27ecfb1182c39df4c7bfd0c40d61832eec",
            ),
        };
        Firmware::pinned(self.label(), bytes, hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_public_choices_validate_and_have_distinct_images() {
        let images: Vec<_> = Choice::ALL
            .into_iter()
            .map(|choice| choice.firmware().unwrap())
            .collect();
        for (index, image) in images.iter().enumerate() {
            assert!(images[..index].iter().all(|other| image.hash != other.hash));
        }
    }

    #[test]
    fn distributed_hex_files_match_the_pinned_binary_choices() {
        // Validate distributed HEX files against embedded BIN images to catch
        // stale or mislabeled release payloads.
        let files = [
            (
                Choice::Stock,
                include_str!("../firmware/stock-deco-fun-xs/firmware.hex"),
            ),
            (
                Choice::NoSmoothing,
                include_str!("../firmware/deco-fun-xs-no-smoothing/firmware.hex"),
            ),
            (
                Choice::FullPressure,
                include_str!(
                    "../firmware/deco-fun-xs-no-smoothing-full-pressure-gpio-reset-excitation-31/firmware.hex"
                ),
            ),
            (
                Choice::Fast,
                include_str!(
                    "../firmware/deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31/firmware.hex"
                ),
            ),
        ];
        for (choice, hex) in files {
            assert_eq!(
                image::parse_hex(hex).unwrap(),
                choice.firmware().unwrap().code
            );
        }
    }
}
