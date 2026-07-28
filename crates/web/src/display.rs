//! Optional presentation upgrades layered over the authoritative retail frame.
#![cfg_attr(
    test,
    allow(
        dead_code,
        reason = "browser-only DOM parsing is not compiled into host unit tests"
    )
)]

use crust_renderer::projection::Viewport;

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
}

impl OutputAspect {
    pub(crate) const fn from_value(value: &str) -> Self {
        match value.as_bytes() {
            b"16:9" => Self::Wide,
            b"21:9" => Self::Ultrawide,
            _ => Self::Retail,
        }
    }

    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Retail => "4:3",
            Self::Wide => "16:9",
            Self::Ultrawide => "21:9",
        }
    }

    pub(crate) const fn ratio(self) -> (u32, u32) {
        match self {
            Self::Retail => (4, 3),
            Self::Wide => (16, 9),
            Self::Ultrawide => (21, 9),
        }
    }

    /// Preserve the retail vertical field while revealing additional width.
    pub(crate) fn logical_viewport(self) -> Viewport {
        let (numerator, denominator) = self.ratio();
        let scaled_numerator = 512_u32.saturating_mul(numerator).saturating_mul(3);
        let scaled_denominator = denominator.saturating_mul(4);
        let rounded = scaled_numerator.saturating_add(scaled_denominator / 2) / scaled_denominator;
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

/// Browser drawing-buffer policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RenderResolution {
    /// Match CSS pixels times the device pixel ratio.
    #[default]
    Native,
    /// Fixed vertical resolution while preserving the selected aspect.
    Fixed(u32),
}

impl RenderResolution {
    pub(crate) fn from_value(value: &str) -> Self {
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
    pub resolution: RenderResolution,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            smooth_motion: false,
            extended_world: false,
            projection_percent: 100,
            aspect: OutputAspect::Retail,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_aspects_preserve_vertical_field_and_expand_only_width() {
        assert_eq!(OutputAspect::Retail.logical_viewport(), Viewport::PSX);
        assert_eq!(
            OutputAspect::Wide.logical_viewport(),
            Viewport {
                x: -342,
                y: -120,
                width: 684,
                height: 240,
            }
        );
        assert_eq!(
            OutputAspect::Ultrawide.logical_viewport(),
            Viewport {
                x: -448,
                y: -120,
                width: 896,
                height: 240,
            }
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
