//! Optional presentation upgrades layered over the authoritative retail frame.
#![cfg_attr(
    test,
    allow(
        dead_code,
        reason = "browser-only DOM parsing is not compiled into host unit tests"
    )
)]

use crust_renderer::projection::Viewport;

const MAX_CANVAS_DIMENSION: u32 = 12_288;

/// Reduced, positive output ratio used by both CSS layout and logical projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OutputRatio {
    numerator: u32,
    denominator: u32,
}

impl OutputRatio {
    pub(crate) const RETAIL: Self = Self {
        numerator: 4,
        denominator: 3,
    };
    pub(crate) const WIDE: Self = Self {
        numerator: 16,
        denominator: 9,
    };
    pub(crate) const ULTRAWIDE: Self = Self {
        numerator: 7,
        denominator: 3,
    };

    pub(crate) fn from_dimensions(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let divisor = greatest_common_divisor(width, height);
        Some(Self {
            numerator: width / divisor,
            denominator: height / divisor,
        })
    }

    pub(crate) const fn numerator(self) -> u32 {
        self.numerator
    }

    pub(crate) const fn denominator(self) -> u32 {
        self.denominator
    }

    pub(crate) fn width_for_height(self, height: u32) -> u32 {
        let width = u64::from(height)
            .saturating_mul(u64::from(self.numerator))
            .saturating_add(u64::from(self.denominator) / 2)
            / u64::from(self.denominator);
        u32::try_from(width)
            .unwrap_or(u32::MAX)
            .clamp(1, MAX_CANVAS_DIMENSION)
    }

    /// Preserve the retail vertical field while revealing or cropping width.
    pub(crate) fn logical_viewport(self) -> Viewport {
        let scaled_numerator = 512_u64
            .saturating_mul(u64::from(self.numerator))
            .saturating_mul(3);
        let scaled_denominator = u64::from(self.denominator).saturating_mul(4);
        let rounded =
            scaled_numerator.saturating_add(scaled_denominator / 2) / scaled_denominator.max(1);
        let rounded = u32::try_from(rounded)
            .unwrap_or(u32::MAX - 1)
            .clamp(2, i32::MAX.cast_unsigned() - 1);
        // An even width keeps logical x=0 exactly at the NDC center.
        let width = rounded.saturating_add(1) & !1;
        Viewport {
            x: -i32::try_from(width / 2).unwrap_or(i32::MAX),
            y: Viewport::PSX.y,
            width,
            height: Viewport::PSX.height,
        }
    }
}

const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

/// Output shape used by the browser canvas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OutputAspect {
    /// Original 4:3 television output.
    #[default]
    Retail,
    /// Uncropped 16:9 output with a wider logical viewport.
    Wide,
    /// Uncropped 21:9 output with a wider logical viewport.
    Ultrawide,
    /// Match the current browser viewport, including uncommon and portrait ratios.
    Screen,
}

impl OutputAspect {
    pub(crate) const fn from_value(value: &str) -> Self {
        match value.as_bytes() {
            b"16:9" => Self::Wide,
            b"21:9" => Self::Ultrawide,
            b"screen" => Self::Screen,
            _ => Self::Retail,
        }
    }

    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Retail => "4:3",
            Self::Wide => "16:9",
            Self::Ultrawide => "21:9",
            Self::Screen => "screen",
        }
    }

    pub(crate) const fn fixed_ratio(self) -> Option<OutputRatio> {
        match self {
            Self::Retail => Some(OutputRatio::RETAIL),
            Self::Wide => Some(OutputRatio::WIDE),
            Self::Ultrawide => Some(OutputRatio::ULTRAWIDE),
            Self::Screen => None,
        }
    }
}

/// Browser drawing-buffer policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RenderResolution {
    /// Match CSS pixels times the device pixel ratio.
    #[default]
    Native,
    /// Fixed vertical resolution while preserving the selected aspect.
    Fixed(u32),
    /// Exact drawing-buffer dimensions selected by the user.
    Custom { width: u32, height: u32 },
}

impl RenderResolution {
    pub(crate) fn from_values(value: &str, custom_width: &str, custom_height: &str) -> Self {
        if value == "custom" {
            let dimensions = custom_width
                .parse::<u32>()
                .ok()
                .zip(custom_height.parse::<u32>().ok())
                .filter(|(width, height)| {
                    (1..=MAX_CANVAS_DIMENSION).contains(width)
                        && (1..=MAX_CANVAS_DIMENSION).contains(height)
                });
            return dimensions.map_or(Self::Native, |(width, height)| Self::Custom {
                width,
                height,
            });
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|height| matches!(*height, 720 | 1080 | 1440 | 2160))
            .map_or(Self::Native, Self::Fixed)
    }
}

/// Complete opt-in display policy. None of these fields change simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DisplaySettings {
    pub smooth_motion: bool,
    pub extended_world: bool,
    /// Percentage of the authored projection distance. Smaller is farther out.
    pub projection_percent: u32,
    pub aspect: OutputAspect,
    /// Resolved ratio. For Screen/Auto this follows the live browser viewport.
    pub output_ratio: OutputRatio,
    pub resolution: RenderResolution,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            smooth_motion: false,
            extended_world: false,
            projection_percent: 100,
            aspect: OutputAspect::Retail,
            output_ratio: OutputRatio::RETAIL,
            resolution: RenderResolution::Native,
        }
    }
}

impl DisplaySettings {
    pub(crate) fn projection_distance(self, authored: u32) -> u32 {
        authored
            .saturating_mul(self.projection_percent.clamp(50, 100))
            .saturating_add(50)
            / 100
    }

    pub(crate) fn logical_viewport(self) -> Viewport {
        self.output_ratio.logical_viewport()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_aspects_preserve_vertical_field_and_expand_only_width() {
        assert_eq!(OutputRatio::RETAIL.logical_viewport(), Viewport::PSX);
        assert_eq!(
            OutputRatio::WIDE.logical_viewport(),
            Viewport {
                x: -342,
                y: -120,
                width: 684,
                height: 240,
            }
        );
        assert_eq!(
            OutputRatio::ULTRAWIDE.logical_viewport(),
            Viewport {
                x: -448,
                y: -120,
                width: 896,
                height: 240,
            }
        );
    }

    #[test]
    fn arbitrary_output_ratios_are_reduced_and_preserve_the_vertical_field() {
        let super_ultrawide = OutputRatio::from_dimensions(5_120, 1_440).unwrap();
        assert_eq!(super_ultrawide.numerator(), 32);
        assert_eq!(super_ultrawide.denominator(), 9);
        assert_eq!(
            super_ultrawide.logical_viewport(),
            Viewport {
                x: -683,
                y: -120,
                width: 1_366,
                height: 240,
            }
        );

        let three_two = OutputRatio::from_dimensions(3_000, 2_000).unwrap();
        assert_eq!(three_two.numerator(), 3);
        assert_eq!(three_two.denominator(), 2);
        assert_eq!(
            three_two.logical_viewport(),
            Viewport {
                x: -288,
                y: -120,
                width: 576,
                height: 240,
            }
        );
        assert!(OutputRatio::from_dimensions(0, 1_080).is_none());
    }

    #[test]
    fn fixed_and_custom_buffers_validate_and_preserve_exact_dimensions() {
        assert_eq!(
            RenderResolution::from_values("1440", "", ""),
            RenderResolution::Fixed(1_440)
        );
        assert_eq!(
            RenderResolution::from_values("custom", "3440", "1440"),
            RenderResolution::Custom {
                width: 3_440,
                height: 1_440
            }
        );
        assert_eq!(
            RenderResolution::from_values("custom", "0", "1440"),
            RenderResolution::Native
        );
        assert_eq!(
            RenderResolution::from_values("custom", "16384", "1440"),
            RenderResolution::Native
        );
        assert_eq!(OutputRatio::WIDE.width_for_height(1_080), 1_920);
        assert_eq!(
            OutputRatio::from_dimensions(32, 9)
                .unwrap()
                .width_for_height(1_440),
            5_120
        );
    }

    #[test]
    fn zoom_scales_projection_without_touching_retail_default() {
        assert_eq!(DisplaySettings::default().projection_distance(500), 500);
        assert_eq!(
            DisplaySettings {
                projection_percent: 85,
                ..DisplaySettings::default()
            }
            .projection_distance(500),
            425
        );
    }
}
