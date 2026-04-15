//! Dimension kinds — the six canonical dimension flavours in professional drafting.
//!
//! Kind is a pure data enum; it carries enough information to describe what the
//! dimension measures but no rendering state. Visual presentation (terminators,
//! text placement, number format) lives in [`DimensionStyle`](super::style::DimensionStyle).

use bevy::math::Vec3;
use serde::{Deserialize, Serialize};

/// What the dimension is measuring. Each variant holds the kind-specific
/// geometric data beyond the two primary reference points stored on the
/// annotation itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DimensionKind {
    /// Horizontal or vertical distance between two points, measured along
    /// `direction` (usually world X or Y). Use for wall runs, exterior strings.
    Linear { direction: Vec3 },

    /// Straight-line distance between the two points, along the line that
    /// joins them. Use for diagonal features.
    Aligned,

    /// Angle at `vertex` between the two reference points.
    Angular { vertex: Vec3 },

    /// Radius of a circle/arc whose centre is `center` and whose edge passes
    /// through the annotation's second reference point. Text shows `R<value>`.
    Radial { center: Vec3 },

    /// Diameter of a circle passing through the two reference points with
    /// centre at `center`. Text shows `Ø<value>`.
    Diameter { center: Vec3 },

    /// A note with an arrow — not a measurement. Text is user-supplied.
    Leader { text: String },
}

impl DimensionKind {
    /// Discriminant tag used for filtering and registry keys.
    #[must_use]
    pub fn tag(&self) -> DimensionKindTag {
        match self {
            Self::Linear { .. } => DimensionKindTag::Linear,
            Self::Aligned => DimensionKindTag::Aligned,
            Self::Angular { .. } => DimensionKindTag::Angular,
            Self::Radial { .. } => DimensionKindTag::Radial,
            Self::Diameter { .. } => DimensionKindTag::Diameter,
            Self::Leader { .. } => DimensionKindTag::Leader,
        }
    }
}

/// Flat discriminant for [`DimensionKind`]. Cheap to hash, serialize, and
/// include in visibility filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DimensionKindTag {
    Linear,
    Aligned,
    Angular,
    Radial,
    Diameter,
    Leader,
}

impl DimensionKindTag {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Aligned => "aligned",
            Self::Angular => "angular",
            Self::Radial => "radial",
            Self::Diameter => "diameter",
            Self::Leader => "leader",
        }
    }
}
