use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Physical dimension carried by a numeric Definition parameter.
///
/// The dimension is explicit at the interchange boundary so a plausible value
/// cannot silently cross from (for example) inches to millimetres or from area
/// to length. `Scalar` is a pure number; `Ratio` and `Count` remain distinct
/// semantic dimensions even though all three are dimensionless in SI terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum QuantityDimension {
    Length,
    Angle,
    Area,
    Volume,
    Ratio,
    Count,
    Scalar,
}

/// Shared unit vocabulary for Definition metadata and relational quantities.
///
/// Existing relational spellings (`mm`, `deg`, `dimensionless`) remain stable
/// on the wire. Definition metadata wraps a `Unit` in [`ParameterUnit`] so its
/// dimension is also serialized and legacy unknown spellings can be retained
/// without weakening this closed evaluator vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub enum Unit {
    #[serde(rename = "mm")]
    Mm,
    #[serde(rename = "cm")]
    Cm,
    #[serde(rename = "m")]
    M,
    #[serde(rename = "ft")]
    Ft,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "deg")]
    Deg,
    #[serde(rename = "rad")]
    Rad,
    #[serde(rename = "mm2")]
    SquareMm,
    #[serde(rename = "cm2")]
    SquareCm,
    #[serde(rename = "m2")]
    SquareM,
    #[serde(rename = "ft2")]
    SquareFt,
    #[serde(rename = "in2")]
    SquareIn,
    #[serde(rename = "mm3")]
    CubicMm,
    #[serde(rename = "cm3")]
    CubicCm,
    #[serde(rename = "m3")]
    CubicM,
    #[serde(rename = "ft3")]
    CubicFt,
    #[serde(rename = "in3")]
    CubicIn,
    #[serde(rename = "percent")]
    Percent,
    #[serde(rename = "ratio")]
    Ratio,
    #[serde(rename = "count")]
    Count,
    #[serde(rename = "dimensionless")]
    Dimensionless,
}

impl Unit {
    pub const fn dimension(self) -> QuantityDimension {
        match self {
            Self::Mm | Self::Cm | Self::M | Self::Ft | Self::In => QuantityDimension::Length,
            Self::Deg | Self::Rad => QuantityDimension::Angle,
            Self::SquareMm | Self::SquareCm | Self::SquareM | Self::SquareFt | Self::SquareIn => {
                QuantityDimension::Area
            }
            Self::CubicMm | Self::CubicCm | Self::CubicM | Self::CubicFt | Self::CubicIn => {
                QuantityDimension::Volume
            }
            Self::Percent | Self::Ratio => QuantityDimension::Ratio,
            Self::Count => QuantityDimension::Count,
            Self::Dimensionless => QuantityDimension::Scalar,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Mm => "mm",
            Self::Cm => "cm",
            Self::M => "m",
            Self::Ft => "ft",
            Self::In => "in",
            Self::Deg => "deg",
            Self::Rad => "rad",
            Self::SquareMm => "mm2",
            Self::SquareCm => "cm2",
            Self::SquareM => "m2",
            Self::SquareFt => "ft2",
            Self::SquareIn => "in2",
            Self::CubicMm => "mm3",
            Self::CubicCm => "cm3",
            Self::CubicM => "m3",
            Self::CubicFt => "ft3",
            Self::CubicIn => "in3",
            Self::Percent => "%",
            Self::Ratio => "ratio",
            Self::Count => "count",
            Self::Dimensionless => "scalar",
        }
    }

    /// Parse common authored and legacy spellings into the closed unit
    /// vocabulary. The returned spelling is always canonical on serialization.
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => Some(Self::Mm),
            "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => Some(Self::Cm),
            "m" | "metre" | "metres" | "meter" | "meters" => Some(Self::M),
            "ft" | "foot" | "feet" => Some(Self::Ft),
            "in" | "inch" | "inches" => Some(Self::In),
            "deg" | "degree" | "degrees" | "°" => Some(Self::Deg),
            "rad" | "radian" | "radians" => Some(Self::Rad),
            "mm2" | "mm^2" | "mm²" | "square_mm" | "square millimetres" | "square millimeters" => {
                Some(Self::SquareMm)
            }
            "cm2" | "cm^2" | "cm²" | "square_cm" | "square centimetres" | "square centimeters" => {
                Some(Self::SquareCm)
            }
            "m2" | "m^2" | "m²" | "square_m" | "square metres" | "square meters" => {
                Some(Self::SquareM)
            }
            "ft2" | "ft^2" | "ft²" | "square_ft" | "square feet" => Some(Self::SquareFt),
            "in2" | "in^2" | "in²" | "square_in" | "square inches" => Some(Self::SquareIn),
            "mm3" | "mm^3" | "mm³" | "cubic_mm" | "cubic millimetres" | "cubic millimeters" => {
                Some(Self::CubicMm)
            }
            "cm3" | "cm^3" | "cm³" | "cubic_cm" | "cubic centimetres" | "cubic centimeters" => {
                Some(Self::CubicCm)
            }
            "m3" | "m^3" | "m³" | "cubic_m" | "cubic metres" | "cubic meters" => {
                Some(Self::CubicM)
            }
            "ft3" | "ft^3" | "ft³" | "cubic_ft" | "cubic feet" => Some(Self::CubicFt),
            "in3" | "in^3" | "in³" | "cubic_in" | "cubic inches" => Some(Self::CubicIn),
            "%" | "percent" | "percentage" => Some(Self::Percent),
            "ratio" | "proportion" => Some(Self::Ratio),
            "count" | "integer_count" | "number_of_items" => Some(Self::Count),
            "scalar" | "number" | "unitless" | "dimensionless" | "1" => Some(Self::Dimensionless),
            _ => None,
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

/// Exchange-facing Definition parameter unit.
///
/// New content serializes as a typed object. Legacy string values are accepted
/// and normalized during deserialization. Unknown strings remain explicit and
/// round-trippable instead of being guessed or discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterUnit {
    Typed {
        dimension: QuantityDimension,
        unit: Unit,
    },
    UnknownLegacy {
        value: String,
    },
}

impl ParameterUnit {
    pub const fn typed(unit: Unit) -> Self {
        Self::Typed {
            dimension: unit.dimension(),
            unit,
        }
    }

    pub fn from_legacy(value: impl Into<String>) -> Self {
        let value = value.into();
        Unit::parse(&value)
            .map(Self::typed)
            .unwrap_or(Self::UnknownLegacy { value })
    }

    pub fn parse_known(value: &str) -> Result<Self, String> {
        Unit::parse(value).map(Self::typed).ok_or_else(|| {
            format!(
                "Unknown parameter unit '{}'; use a supported typed unit",
                value.trim()
            )
        })
    }

    pub const fn dimension(&self) -> Option<QuantityDimension> {
        match self {
            Self::Typed { dimension, .. } => Some(*dimension),
            Self::UnknownLegacy { .. } => None,
        }
    }

    pub const fn unit(&self) -> Option<Unit> {
        match self {
            Self::Typed { unit, .. } => Some(*unit),
            Self::UnknownLegacy { .. } => None,
        }
    }

    pub fn unknown_legacy_value(&self) -> Option<&str> {
        match self {
            Self::UnknownLegacy { value } => Some(value),
            Self::Typed { .. } => None,
        }
    }
}

impl From<Unit> for ParameterUnit {
    fn from(unit: Unit) -> Self {
        Self::typed(unit)
    }
}

impl fmt::Display for ParameterUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Typed { unit, .. } => unit.fmt(f),
            Self::UnknownLegacy { value } => value.fmt(f),
        }
    }
}

impl Serialize for ParameterUnit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Repr<'a> {
            Typed {
                dimension: QuantityDimension,
                unit: Unit,
            },
            UnknownLegacy {
                status: &'static str,
                legacy: &'a str,
            },
        }

        match self {
            Self::Typed { dimension, unit } => Repr::Typed {
                dimension: *dimension,
                unit: *unit,
            }
            .serialize(serializer),
            Self::UnknownLegacy { value } => Repr::UnknownLegacy {
                status: "unknown_legacy",
                legacy: value,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ParameterUnit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Legacy(String),
            Typed {
                dimension: QuantityDimension,
                unit: Unit,
            },
            UnknownLegacy {
                status: String,
                legacy: String,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Legacy(value) => Ok(Self::from_legacy(value)),
            Repr::Typed { dimension, unit } if dimension == unit.dimension() => {
                Ok(Self::Typed { dimension, unit })
            }
            Repr::Typed { dimension, unit } => Err(de::Error::custom(format!(
                "unit '{unit}' has dimension '{:?}', not '{dimension:?}'",
                unit.dimension()
            ))),
            Repr::UnknownLegacy { status, legacy } if status == "unknown_legacy" => {
                Ok(Self::UnknownLegacy { value: legacy })
            }
            Repr::UnknownLegacy { status, .. } => Err(de::Error::custom(format!(
                "unsupported parameter unit status '{status}'"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DisplayUnit {
    Millimetres,
    Centimetres,
    #[default]
    Metres,
    Feet,
    Inches,
}

impl DisplayUnit {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::Millimetres => "mm",
            Self::Centimetres => "cm",
            Self::Metres => "m",
            Self::Feet => "ft",
            Self::Inches => "in",
        }
    }

    pub fn from_metres(&self, metres: f32) -> f32 {
        metres * self.scale_factor()
    }

    pub fn to_metres(&self, value: f32) -> f32 {
        value / self.scale_factor()
    }

    pub fn abbreviation(&self) -> &'static str {
        self.identifier()
    }

    pub fn format_value(&self, metres: f32, precision: u8) -> String {
        let value = self.from_metres(metres);
        format!(
            "{:.prec$}{}",
            value,
            self.abbreviation(),
            prec = precision as usize
        )
    }

    fn scale_factor(&self) -> f32 {
        match self {
            Self::Millimetres => 1000.0,
            Self::Centimetres => 100.0,
            Self::Metres => 1.0,
            Self::Feet => 3.280_84,
            Self::Inches => 39.370_1,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => {
                Some(Self::Millimetres)
            }
            "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => {
                Some(Self::Centimetres)
            }
            "m" | "metre" | "metres" | "meter" | "meters" => Some(Self::Metres),
            "ft" | "foot" | "feet" => Some(Self::Feet),
            "in" | "inch" | "inches" => Some(Self::Inches),
            _ => None,
        }
    }
}

impl From<DisplayUnit> for Unit {
    fn from(value: DisplayUnit) -> Self {
        match value {
            DisplayUnit::Millimetres => Self::Mm,
            DisplayUnit::Centimetres => Self::Cm,
            DisplayUnit::Metres => Self::M,
            DisplayUnit::Feet => Self::Ft,
            DisplayUnit::Inches => Self::In,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metres_round_trip() {
        for unit in [
            DisplayUnit::Millimetres,
            DisplayUnit::Centimetres,
            DisplayUnit::Metres,
            DisplayUnit::Feet,
            DisplayUnit::Inches,
        ] {
            let metres = 2.5_f32;
            let display = unit.from_metres(metres);
            let back = unit.to_metres(display);
            assert!((back - metres).abs() < 1e-4, "{unit:?} round-trip failed");
        }
    }

    #[test]
    fn millimetre_conversion() {
        let mm = DisplayUnit::Millimetres.from_metres(1.0);
        assert!((mm - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn format_value_precision() {
        assert_eq!(DisplayUnit::Metres.format_value(1.234, 2), "1.23m");
        assert_eq!(DisplayUnit::Millimetres.format_value(0.001, 0), "1mm");
    }

    #[test]
    fn parse_common_unit_names() {
        assert_eq!(DisplayUnit::parse("mm"), Some(DisplayUnit::Millimetres));
        assert_eq!(
            DisplayUnit::parse("millimeters"),
            Some(DisplayUnit::Millimetres)
        );
        assert_eq!(DisplayUnit::parse("cm"), Some(DisplayUnit::Centimetres));
        assert_eq!(DisplayUnit::parse("m"), Some(DisplayUnit::Metres));
        assert_eq!(DisplayUnit::parse("feet"), Some(DisplayUnit::Feet));
        assert_eq!(DisplayUnit::parse("inches"), Some(DisplayUnit::Inches));
        assert_eq!(DisplayUnit::parse("yards"), None);
    }

    #[test]
    fn shared_unit_vocabulary_covers_exchange_dimensions() {
        for (spelling, expected, dimension) in [
            ("millimeters", Unit::Mm, QuantityDimension::Length),
            ("°", Unit::Deg, QuantityDimension::Angle),
            ("m²", Unit::SquareM, QuantityDimension::Area),
            ("ft3", Unit::CubicFt, QuantityDimension::Volume),
            ("%", Unit::Percent, QuantityDimension::Ratio),
            ("count", Unit::Count, QuantityDimension::Count),
            ("scalar", Unit::Dimensionless, QuantityDimension::Scalar),
        ] {
            let unit = Unit::parse(spelling).unwrap();
            assert_eq!(unit, expected);
            assert_eq!(unit.dimension(), dimension);
        }
    }

    #[test]
    fn legacy_parameter_unit_normalizes_to_typed_object() {
        let parsed: ParameterUnit = serde_json::from_str(r#""metres""#).unwrap();
        assert_eq!(parsed, ParameterUnit::typed(Unit::M));
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::json!({"dimension": "length", "unit": "m"})
        );
    }

    #[test]
    fn unknown_legacy_parameter_unit_is_explicit_and_round_trips() {
        let parsed: ParameterUnit = serde_json::from_str(r#""furlong""#).unwrap();
        assert_eq!(parsed.unknown_legacy_value(), Some("furlong"));
        let value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"status": "unknown_legacy", "legacy": "furlong"})
        );
        assert_eq!(
            serde_json::from_value::<ParameterUnit>(value).unwrap(),
            parsed
        );
    }

    #[test]
    fn typed_parameter_unit_rejects_dimension_mismatch() {
        let error = serde_json::from_value::<ParameterUnit>(serde_json::json!({
            "dimension": "area",
            "unit": "m"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("not 'Area'"));
    }
}
