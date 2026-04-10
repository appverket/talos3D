use serde::{Deserialize, Serialize};

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
}
