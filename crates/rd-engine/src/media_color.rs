//! Transport-level pixel and color contract negotiation.
//!
//! This module classifies only contracts that are already identical at both
//! ends. It does not inspect codec profiles, infer color from VUI labels, copy
//! pixels, convert formats, or select a fallback contract.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaSubsampling {
    Cs444,
    Cs422,
    Cs420,
    Monochrome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericRepresentation {
    UnsignedNormalized,
    SignedNormalized,
    UnsignedInteger,
    SignedInteger,
    FloatingPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelStorage {
    pub bit_depth: u8,
    pub subsampling: ChromaSubsampling,
    pub numeric_representation: NumericRepresentation,
}

impl PixelStorage {
    fn is_explicit(self) -> bool {
        (8..=16).contains(&self.bit_depth)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPrimaries {
    Unspecified,
    Bt709,
    Bt2020,
    DisplayP3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferCharacteristics {
    Unspecified,
    Srgb,
    Bt709,
    Bt1886,
    Gamma22,
    Pq,
    Hlg,
    /// Linear extended-range scRGB, normally FP16 RGB with BT.709 primaries.
    Scrgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatrixCoefficients {
    Unspecified,
    Identity,
    Bt709,
    Bt2020NonConstantLuminance,
    Bt2020ConstantLuminance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorRange {
    Unspecified,
    Limited,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colorimetry {
    pub primaries: ColorPrimaries,
    pub transfer: TransferCharacteristics,
    pub matrix: MatrixCoefficients,
    pub range: ColorRange,
}

impl Colorimetry {
    fn is_explicit(self) -> bool {
        self.primaries != ColorPrimaries::Unspecified
            && self.transfer != TransferCharacteristics::Unspecified
            && self.matrix != MatrixCoefficients::Unspecified
            && self.range != ColorRange::Unspecified
    }

    fn is_explicit_bt2020(self) -> bool {
        self.is_explicit()
            && self.primaries == ColorPrimaries::Bt2020
            && matches!(
                self.matrix,
                MatrixCoefficients::Bt2020NonConstantLuminance
                    | MatrixCoefficients::Bt2020ConstantLuminance
            )
    }
}

/// CIE 1931 chromaticity coordinates in units of 0.00002.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chromaticity {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasteringDisplay {
    pub red: Chromaticity,
    pub green: Chromaticity,
    pub blue: Chromaticity,
    pub white_point: Chromaticity,
    /// Maximum mastering luminance in units of 0.0001 cd/m².
    pub max_luminance: u32,
    /// Minimum mastering luminance in units of 0.0001 cd/m².
    pub min_luminance: u32,
}

impl MasteringDisplay {
    pub fn is_valid(self) -> bool {
        let valid_chromaticity = |point: Chromaticity| point.x <= 50_000 && point.y <= 50_000;
        valid_chromaticity(self.red)
            && valid_chromaticity(self.green)
            && valid_chromaticity(self.blue)
            && valid_chromaticity(self.white_point)
            && self.max_luminance > self.min_luminance
            && self.max_luminance != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLight {
    /// Maximum content light level in cd/m².
    pub max_cll: u16,
    /// Maximum frame-average light level in cd/m².
    pub max_fall: u16,
}

impl ContentLight {
    pub fn is_valid(self) -> bool {
        self.max_cll != 0 && self.max_fall != 0 && self.max_fall <= self.max_cll
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrStaticMetadata {
    pub mastering_display: MasteringDisplay,
    pub content_light: ContentLight,
}

impl HdrStaticMetadata {
    pub fn is_valid(self) -> bool {
        self.mastering_display.is_valid() && self.content_light.is_valid()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaColorContract {
    pub storage: PixelStorage,
    pub colorimetry: Colorimetry,
    pub hdr_static: Option<HdrStaticMetadata>,
    /// True only when the producer preserved source color meaning and metadata
    /// through any explicitly negotiated storage conversion. It must be false
    /// for relabeling, clipping or tone mapping; RGB-to-YCbCr/chroma conversion
    /// inside a hardware encoder may keep it true when colorimetry is preserved.
    pub source_preserved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiatedColorClass {
    Sdr,
    Sdr10,
    Hdr10Pq,
    Hlg10,
    ScrgbLinear,
}

/// Classifies one exact producer/consumer transport contract.
///
/// `None` means there is no exact usable agreement. In particular, codec
/// Main10 capability and bitstream/VUI labels are not inputs and cannot promote
/// an agreement to HDR. PQ requires complete, valid mastering-display and
/// MaxCLL/MaxFALL metadata.
pub fn classify_negotiated(
    producer: &MediaColorContract,
    consumer: &MediaColorContract,
) -> Option<NegotiatedColorClass> {
    if producer != consumer
        || !producer.storage.is_explicit()
        || !producer.colorimetry.is_explicit()
        || !producer.source_preserved
    {
        return None;
    }

    let contract = *producer;
    let ten_bit_420_unorm = contract.storage
        == (PixelStorage {
            bit_depth: 10,
            subsampling: ChromaSubsampling::Cs420,
            numeric_representation: NumericRepresentation::UnsignedNormalized,
        });
    let eight_bit_unorm = contract.storage.bit_depth == 8
        && contract.storage.numeric_representation == NumericRepresentation::UnsignedNormalized;
    let scrgb_fp16 = contract.storage
        == (PixelStorage {
            bit_depth: 16,
            subsampling: ChromaSubsampling::Cs444,
            numeric_representation: NumericRepresentation::FloatingPoint,
        })
        && contract.colorimetry.primaries == ColorPrimaries::Bt709
        && contract.colorimetry.matrix == MatrixCoefficients::Identity
        && contract.colorimetry.range == ColorRange::Full;
    let hdr_base = ten_bit_420_unorm && contract.colorimetry.is_explicit_bt2020();

    match contract.colorimetry.transfer {
        TransferCharacteristics::Pq
            if hdr_base && contract.hdr_static.is_some_and(HdrStaticMetadata::is_valid) =>
        {
            Some(NegotiatedColorClass::Hdr10Pq)
        }
        TransferCharacteristics::Hlg
            if hdr_base && contract.hdr_static.is_none_or(HdrStaticMetadata::is_valid) =>
        {
            Some(NegotiatedColorClass::Hlg10)
        }
        TransferCharacteristics::Pq | TransferCharacteristics::Hlg => None,
        TransferCharacteristics::Scrgb if scrgb_fp16 && contract.hdr_static.is_none() => {
            Some(NegotiatedColorClass::ScrgbLinear)
        }
        TransferCharacteristics::Scrgb => None,
        _ if contract.hdr_static.is_some() => None,
        _ if ten_bit_420_unorm => Some(NegotiatedColorClass::Sdr10),
        _ if eight_bit_unorm => Some(NegotiatedColorClass::Sdr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr10_contract() -> MediaColorContract {
        MediaColorContract {
            storage: PixelStorage {
                bit_depth: 10,
                subsampling: ChromaSubsampling::Cs420,
                numeric_representation: NumericRepresentation::UnsignedNormalized,
            },
            colorimetry: Colorimetry {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferCharacteristics::Pq,
                matrix: MatrixCoefficients::Bt2020NonConstantLuminance,
                range: ColorRange::Limited,
            },
            hdr_static: Some(HdrStaticMetadata {
                mastering_display: MasteringDisplay {
                    red: Chromaticity {
                        x: 35_400,
                        y: 14_600,
                    },
                    green: Chromaticity {
                        x: 8_500,
                        y: 39_850,
                    },
                    blue: Chromaticity { x: 6_550, y: 2_300 },
                    white_point: Chromaticity {
                        x: 15_635,
                        y: 16_450,
                    },
                    max_luminance: 10_000_000,
                    min_luminance: 1,
                },
                content_light: ContentLight {
                    max_cll: 1_000,
                    max_fall: 400,
                },
            }),
            source_preserved: true,
        }
    }

    #[test]
    fn exact_complete_hdr10_contract_is_hdr10_pq() {
        let contract = hdr10_contract();
        assert_eq!(
            classify_negotiated(&contract, &contract),
            Some(NegotiatedColorClass::Hdr10Pq)
        );
    }

    #[test]
    fn hdr_requires_preservation_metadata_and_exact_transport_match() {
        let producer = hdr10_contract();

        let mut missing_metadata = producer;
        missing_metadata.hdr_static = None;
        assert_eq!(
            classify_negotiated(&missing_metadata, &missing_metadata),
            None
        );

        let mut relabeled_only = producer;
        relabeled_only.source_preserved = false;
        assert_eq!(classify_negotiated(&relabeled_only, &relabeled_only), None);

        let mut mismatched_consumer = producer;
        mismatched_consumer.storage.subsampling = ChromaSubsampling::Cs422;
        assert_eq!(classify_negotiated(&producer, &mismatched_consumer), None);

        for storage in [
            PixelStorage {
                bit_depth: 12,
                ..producer.storage
            },
            PixelStorage {
                bit_depth: 16,
                ..producer.storage
            },
            PixelStorage {
                numeric_representation: NumericRepresentation::FloatingPoint,
                ..producer.storage
            },
            PixelStorage {
                subsampling: ChromaSubsampling::Cs444,
                ..producer.storage
            },
            PixelStorage {
                subsampling: ChromaSubsampling::Cs422,
                ..producer.storage
            },
        ] {
            let mut invalid_hdr10_name = producer;
            invalid_hdr10_name.storage = storage;
            assert_eq!(
                classify_negotiated(&invalid_hdr10_name, &invalid_hdr10_name),
                None
            );

            invalid_hdr10_name.colorimetry.transfer = TransferCharacteristics::Hlg;
            invalid_hdr10_name.hdr_static = None;
            assert_eq!(
                classify_negotiated(&invalid_hdr10_name, &invalid_hdr10_name),
                None
            );

            invalid_hdr10_name.colorimetry.transfer = TransferCharacteristics::Bt1886;
            assert_eq!(
                classify_negotiated(&invalid_hdr10_name, &invalid_hdr10_name),
                None
            );
        }

        let mut sdr8 = producer;
        sdr8.storage = PixelStorage {
            bit_depth: 8,
            subsampling: ChromaSubsampling::Cs444,
            numeric_representation: NumericRepresentation::UnsignedNormalized,
        };
        sdr8.colorimetry = Colorimetry {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristics::Srgb,
            matrix: MatrixCoefficients::Identity,
            range: ColorRange::Full,
        };
        sdr8.hdr_static = None;
        assert_eq!(
            classify_negotiated(&sdr8, &sdr8),
            Some(NegotiatedColorClass::Sdr)
        );

        let mut scrgb = sdr8;
        scrgb.storage = PixelStorage {
            bit_depth: 16,
            subsampling: ChromaSubsampling::Cs444,
            numeric_representation: NumericRepresentation::FloatingPoint,
        };
        scrgb.colorimetry.transfer = TransferCharacteristics::Scrgb;
        assert_eq!(
            classify_negotiated(&scrgb, &scrgb),
            Some(NegotiatedColorClass::ScrgbLinear)
        );

        scrgb.storage.bit_depth = 10;
        assert_eq!(classify_negotiated(&scrgb, &scrgb), None);
    }
}
