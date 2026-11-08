//! Display orientation for a panel whose native resolution is portrait.
//!
//! The renderer draws a logical (unrotated) screen and rotates it into the panel's buffer;
//! touches arrive in panel coordinates and must be mapped back to logical ones.

/// How the user holds or mounts the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
    PortraitFlipped,
    LandscapeFlipped,
}

impl Orientation {
    pub const ALL: [Self; 4] = [
        Self::Portrait,
        Self::Landscape,
        Self::PortraitFlipped,
        Self::LandscapeFlipped,
    ];

    /// Stable name used for storage and in front end callbacks.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
            Self::PortraitFlipped => "portrait_flipped",
            Self::LandscapeFlipped => "landscape_flipped",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|orientation| orientation.key() == key)
    }

    #[must_use]
    pub const fn is_landscape(self) -> bool {
        matches!(self, Self::Landscape | Self::LandscapeFlipped)
    }

    #[must_use]
    pub const fn is_flipped(self) -> bool {
        matches!(self, Self::PortraitFlipped | Self::LandscapeFlipped)
    }

    /// The same flip with the given shape.
    #[must_use]
    pub const fn with_landscape(self, landscape: bool) -> Self {
        match (landscape, self.is_flipped()) {
            (false, false) => Self::Portrait,
            (false, true) => Self::PortraitFlipped,
            (true, false) => Self::Landscape,
            (true, true) => Self::LandscapeFlipped,
        }
    }

    /// The same shape turned upside down.
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            Self::Portrait => Self::PortraitFlipped,
            Self::PortraitFlipped => Self::Portrait,
            Self::Landscape => Self::LandscapeFlipped,
            Self::LandscapeFlipped => Self::Landscape,
        }
    }

    /// Clockwise rotation the renderer applies, in degrees.
    #[must_use]
    pub const fn rotation_degrees(self) -> u16 {
        match self {
            Self::Portrait => 0,
            Self::Landscape => 90,
            Self::PortraitFlipped => 180,
            Self::LandscapeFlipped => 270,
        }
    }

    /// Size of the logical screen for a panel of `panel_width` x `panel_height` pixels.
    #[must_use]
    pub const fn logical_size(self, panel_width: u32, panel_height: u32) -> (u32, u32) {
        if self.is_landscape() {
            (panel_height, panel_width)
        } else {
            (panel_width, panel_height)
        }
    }

    /// Maps a touch at panel pixel (`x`, `y`) to logical coordinates, the inverse of the
    /// renderer's rotation. Points outside the panel are clamped to its edge.
    #[must_use]
    pub const fn to_logical(
        self,
        x: u32,
        y: u32,
        panel_width: u32,
        panel_height: u32,
    ) -> (u32, u32) {
        let x = if x >= panel_width { panel_width - 1 } else { x };
        let y = if y >= panel_height {
            panel_height - 1
        } else {
            y
        };
        match self {
            Self::Portrait => (x, y),
            Self::Landscape => (y, panel_width - 1 - x),
            Self::PortraitFlipped => (panel_width - 1 - x, panel_height - 1 - y),
            Self::LandscapeFlipped => (panel_height - 1 - y, x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Orientation;

    const W: u32 = 800;
    const H: u32 = 1280;

    #[test]
    fn keys_round_trip() {
        for orientation in Orientation::ALL {
            assert_eq!(Orientation::from_key(orientation.key()), Some(orientation));
        }
        assert_eq!(Orientation::from_key("sideways"), None);
    }

    #[test]
    fn shape_and_flip_compose() {
        use Orientation::*;
        assert_eq!(Portrait.with_landscape(true), Landscape);
        assert_eq!(PortraitFlipped.with_landscape(true), LandscapeFlipped);
        assert_eq!(LandscapeFlipped.with_landscape(false), PortraitFlipped);
        for orientation in Orientation::ALL {
            assert_eq!(orientation.flipped().flipped(), orientation);
            assert_ne!(orientation.flipped(), orientation);
            assert_eq!(
                orientation.flipped().is_landscape(),
                orientation.is_landscape()
            );
        }
    }

    #[test]
    fn logical_size_swaps_for_landscape() {
        assert_eq!(Orientation::Portrait.logical_size(W, H), (800, 1280));
        assert_eq!(Orientation::PortraitFlipped.logical_size(W, H), (800, 1280));
        assert_eq!(Orientation::Landscape.logical_size(W, H), (1280, 800));
        assert_eq!(
            Orientation::LandscapeFlipped.logical_size(W, H),
            (1280, 800)
        );
    }

    #[test]
    fn corners_map_to_the_opposite_logical_corners() {
        // The panel's own corners, as logical coordinates, for every orientation.
        let corners = [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)];
        for orientation in Orientation::ALL {
            let (lw, lh) = orientation.logical_size(W, H);
            let mut seen: Vec<(u32, u32)> = corners
                .iter()
                .map(|(x, y)| orientation.to_logical(*x, *y, W, H))
                .collect();
            seen.sort_unstable();
            let mut expected = vec![(0, 0), (lw - 1, 0), (0, lh - 1), (lw - 1, lh - 1)];
            expected.sort_unstable();
            assert_eq!(seen, expected, "{orientation:?}");
        }
    }

    #[test]
    fn touches_are_clamped_to_the_panel() {
        assert_eq!(
            Orientation::Portrait.to_logical(5000, 5000, W, H),
            (W - 1, H - 1)
        );
    }
}
