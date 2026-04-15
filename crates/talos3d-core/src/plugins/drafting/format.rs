//! Professional number formatting for architectural and engineering dimensions.
//!
//! Implements the four canonical format families used in production drafting:
//!
//! - `FeetInchesFractional` — US architectural, e.g. `15'-0"`, `7'-8 1/2"`.
//!   Fractions are reduced to lowest terms. Inches are always shown (never `15'`).
//!   Zero-inch values always render as `0"`.
//!
//! - `FeetInchesDecimal` — rarely used but supported for completeness.
//!
//! - `Decimal` — mechanical/engineering format, e.g. `4572` mm at precision 0,
//!   `1.250` inch at precision 3. Leading zero included per ISO for metric, omitted
//!   per ASME Y14.5 for inches (controlled by `omit_leading_zero`).
//!
//! - `MetricArchitectural` — integer mm with optional thin-space thousands separator
//!   per ISO/DIN (`4572` or `4 572`).
//!
//! All functions take the measured length in **metres** — the canonical unit in
//! talos3D — and return a String ready to display. They are `#[must_use]` pure
//! functions with no I/O.

use serde::{Deserialize, Serialize};

use crate::plugins::units::DisplayUnit;

/// Number formatting contract. Variants are mutually exclusive and each carries
/// its own precision/style fields. This is intentionally enum-dispatch rather
/// than a single struct-with-modes: mixing feet-inches formatting with decimal
/// precision is a category error that should be unrepresentable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NumberFormat {
    /// `15'-0"`, `7'-8 1/2"`. `denominator` is the smallest fraction allowed
    /// (typically 2, 4, 8, or 16). Values round to the nearest 1/denominator.
    FeetInchesFractional { denominator: u8 },

    /// `15'-0.5"` — decimal inches rather than fractions.
    FeetInchesDecimal { precision: u8 },

    /// Plain decimal in `unit` with fixed precision. `omit_leading_zero=true`
    /// emits `.500` (ASME inch), false emits `0.500` (ISO metric).
    /// `strip_trailing_zeros=true` emits `1.5` instead of `1.500` when precision allows.
    Decimal {
        unit: DisplayUnit,
        precision: u8,
        omit_leading_zero: bool,
        strip_trailing_zeros: bool,
    },

    /// Integer mm, ISO/DIN architectural style. `thousands_separator=true` uses
    /// a thin space (U+2009) per ISO 80000-1 (`4 572`). False emits `4572`.
    MetricArchitectural { thousands_separator: bool },
}

impl NumberFormat {
    /// Format a length in metres as a display string. Never panics. Treats NaN
    /// and infinite values as `"—"` so corrupt state does not corrupt drawings.
    #[must_use]
    pub fn format_metres(&self, metres: f32) -> String {
        if !metres.is_finite() {
            return "—".to_string();
        }
        match self {
            Self::FeetInchesFractional { denominator } => {
                format_feet_inches_fractional(metres, *denominator)
            }
            Self::FeetInchesDecimal { precision } => {
                format_feet_inches_decimal(metres, *precision)
            }
            Self::Decimal {
                unit,
                precision,
                omit_leading_zero,
                strip_trailing_zeros,
            } => format_decimal(
                metres,
                *unit,
                *precision,
                *omit_leading_zero,
                *strip_trailing_zeros,
            ),
            Self::MetricArchitectural {
                thousands_separator,
            } => format_metric_architectural(metres, *thousands_separator),
        }
    }
}

// ─── Feet-inches (fractional) ────────────────────────────────────────────────

fn format_feet_inches_fractional(metres: f32, denominator: u8) -> String {
    // Convert to total inches using the exact 1 m = 39.370078... in factor.
    const INCHES_PER_METRE: f32 = 39.370_08;
    let denom = denominator.max(1) as i64;

    let total_inches = (metres * INCHES_PER_METRE) as f64;
    let sign = if total_inches < 0.0 { "-" } else { "" };
    let abs_inches = total_inches.abs();

    // Round to nearest 1/denom unit of an inch, then decompose.
    let total_units = (abs_inches * denom as f64).round() as i64;
    let feet = total_units / (denom * 12);
    let remainder_units = total_units % (denom * 12);
    let whole_inches = remainder_units / denom;
    let frac_units = remainder_units % denom;

    let mut out = format!("{sign}{feet}'-");
    if frac_units == 0 {
        out.push_str(&format!("{whole_inches}\""));
    } else {
        let (num, den) = reduce_fraction(frac_units as u32, denom as u32);
        if whole_inches == 0 {
            out.push_str(&format!("{num}/{den}\""));
        } else {
            out.push_str(&format!("{whole_inches} {num}/{den}\""));
        }
    }
    out
}

fn reduce_fraction(num: u32, den: u32) -> (u32, u32) {
    let g = gcd(num, den);
    if g == 0 {
        (num, den)
    } else {
        (num / g, den / g)
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

// ─── Feet-inches (decimal) ───────────────────────────────────────────────────

fn format_feet_inches_decimal(metres: f32, precision: u8) -> String {
    const INCHES_PER_METRE: f32 = 39.370_08;
    let total_inches = metres * INCHES_PER_METRE;
    let sign = if total_inches < 0.0 { "-" } else { "" };
    let abs_inches = total_inches.abs();

    // Decompose first, round after, to avoid `12.0"` creeping into `0'-12"`.
    let feet = abs_inches.floor() as i64 / 12;
    let inches = abs_inches - (feet as f32 * 12.0);

    // Handle the edge case where rounding inches pushes it to 12.
    let rounded = round_to_precision(inches, precision);
    if (rounded - 12.0).abs() < f32::EPSILON {
        return format!("{sign}{}'-0\"", feet + 1);
    }
    format!(
        "{sign}{feet}'-{:.*}\"",
        precision as usize,
        rounded
    )
}

// ─── Decimal (mechanical) ────────────────────────────────────────────────────

fn format_decimal(
    metres: f32,
    unit: DisplayUnit,
    precision: u8,
    omit_leading_zero: bool,
    strip_trailing_zeros: bool,
) -> String {
    let value = unit.from_metres(metres);
    let mut s = format!("{:.*}", precision as usize, value);

    if strip_trailing_zeros && s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }

    if omit_leading_zero {
        // `0.500` → `.500`, `-0.500` → `-.500`, but not `10.5` → `1.5`.
        if let Some(rest) = s.strip_prefix("0.") {
            s = format!(".{rest}");
        } else if let Some(rest) = s.strip_prefix("-0.") {
            s = format!("-.{rest}");
        }
    }
    s
}

// ─── Metric architectural ────────────────────────────────────────────────────

fn format_metric_architectural(metres: f32, thousands_separator: bool) -> String {
    let mm = (metres * 1000.0).round() as i64;
    if !thousands_separator {
        return mm.to_string();
    }
    // DIN/arch convention: always group at thousands with a thin space (U+2009).
    // 4-digit dimension values (common in building plans) read cleaner grouped.
    // If strict ISO 80000-1 behaviour is wanted later, add a variant.
    let abs_str = mm.abs().to_string();
    if abs_str.len() <= 3 {
        return mm.to_string();
    }
    let mut out = String::with_capacity(abs_str.len() + abs_str.len() / 3);
    if mm < 0 {
        out.push('-');
    }
    let first_group_len = abs_str.len() % 3;
    if first_group_len > 0 {
        out.push_str(&abs_str[..first_group_len]);
    }
    let mut idx = first_group_len;
    while idx < abs_str.len() {
        if !out.is_empty() && !out.ends_with('-') {
            out.push('\u{2009}');
        }
        out.push_str(&abs_str[idx..idx + 3]);
        idx += 3;
    }
    out
}

fn round_to_precision(value: f32, precision: u8) -> f32 {
    let factor = 10_f32.powi(precision as i32);
    (value * factor).round() / factor
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FT: f32 = 0.3048;
    const IN: f32 = 0.0254;

    #[test]
    fn feet_inches_fractional_exact_feet() {
        let fmt = NumberFormat::FeetInchesFractional { denominator: 2 };
        assert_eq!(fmt.format_metres(15.0 * FT), "15'-0\"");
        assert_eq!(fmt.format_metres(1.0 * FT), "1'-0\"");
        assert_eq!(fmt.format_metres(0.0), "0'-0\"");
    }

    #[test]
    fn feet_inches_fractional_half_inch() {
        let fmt = NumberFormat::FeetInchesFractional { denominator: 2 };
        assert_eq!(fmt.format_metres(7.0 * FT + 8.5 * IN), "7'-8 1/2\"");
    }

    #[test]
    fn feet_inches_fractional_reduction() {
        // 4/8 must reduce to 1/2, 8/16 to 1/2, 2/4 to 1/2.
        let fmt = NumberFormat::FeetInchesFractional { denominator: 16 };
        assert_eq!(fmt.format_metres(0.5 * IN), "0'-1/2\"");
        assert_eq!(fmt.format_metres(2.0 * IN + 0.25 * IN), "0'-2 1/4\"");
    }

    #[test]
    fn feet_inches_fractional_negative() {
        let fmt = NumberFormat::FeetInchesFractional { denominator: 2 };
        assert_eq!(fmt.format_metres(-(5.0 * FT + 6.0 * IN)), "-5'-6\"");
    }

    #[test]
    fn feet_inches_fractional_round_up_to_next_foot() {
        // 11' 11 31/32" at precision 1/2 rounds to 12' 0".
        let fmt = NumberFormat::FeetInchesFractional { denominator: 2 };
        let almost_12 = 11.0 * FT + 11.99 * IN;
        assert_eq!(fmt.format_metres(almost_12), "12'-0\"");
    }

    #[test]
    fn feet_inches_fractional_only_inches() {
        let fmt = NumberFormat::FeetInchesFractional { denominator: 4 };
        assert_eq!(fmt.format_metres(6.25 * IN), "0'-6 1/4\"");
    }

    #[test]
    fn feet_inches_fractional_nan_safe() {
        let fmt = NumberFormat::FeetInchesFractional { denominator: 2 };
        assert_eq!(fmt.format_metres(f32::NAN), "—");
        assert_eq!(fmt.format_metres(f32::INFINITY), "—");
    }

    #[test]
    fn feet_inches_decimal() {
        let fmt = NumberFormat::FeetInchesDecimal { precision: 2 };
        assert_eq!(fmt.format_metres(7.0 * FT + 8.5 * IN), "7'-8.50\"");
        assert_eq!(fmt.format_metres(15.0 * FT), "15'-0.00\"");
    }

    #[test]
    fn decimal_metric_precision_zero() {
        let fmt = NumberFormat::Decimal {
            unit: DisplayUnit::Millimetres,
            precision: 0,
            omit_leading_zero: false,
            strip_trailing_zeros: false,
        };
        assert_eq!(fmt.format_metres(4.572), "4572");
    }

    #[test]
    fn decimal_inch_asme_style() {
        let fmt = NumberFormat::Decimal {
            unit: DisplayUnit::Inches,
            precision: 3,
            omit_leading_zero: true,
            strip_trailing_zeros: false,
        };
        assert_eq!(fmt.format_metres(0.5 * IN), ".500");
        assert_eq!(fmt.format_metres(-0.5 * IN), "-.500");
        assert_eq!(fmt.format_metres(1.25 * IN), "1.250");
    }

    #[test]
    fn decimal_strip_trailing_zeros() {
        let fmt = NumberFormat::Decimal {
            unit: DisplayUnit::Metres,
            precision: 3,
            omit_leading_zero: false,
            strip_trailing_zeros: true,
        };
        assert_eq!(fmt.format_metres(1.5), "1.5");
        assert_eq!(fmt.format_metres(1.0), "1");
        assert_eq!(fmt.format_metres(1.234), "1.234");
    }

    #[test]
    fn metric_architectural_no_separator() {
        let fmt = NumberFormat::MetricArchitectural {
            thousands_separator: false,
        };
        assert_eq!(fmt.format_metres(4.572), "4572");
        assert_eq!(fmt.format_metres(0.0), "0");
        assert_eq!(fmt.format_metres(14.5), "14500");
    }

    #[test]
    fn metric_architectural_thin_space() {
        let fmt = NumberFormat::MetricArchitectural {
            thousands_separator: true,
        };
        // U+2009 thin space between thousands; DIN-style grouping from 4+
        // digits so wall-scale dimensions like 4572 mm read cleanly.
        assert_eq!(fmt.format_metres(4.572), "4\u{2009}572");
        assert_eq!(fmt.format_metres(14.5), "14\u{2009}500");
        assert_eq!(fmt.format_metres(1.0), "1\u{2009}000");
        assert_eq!(fmt.format_metres(0.5), "500"); // 3-digit stays ungrouped
        assert_eq!(fmt.format_metres(1234.567), "1\u{2009}234\u{2009}567");
    }

    #[test]
    fn metric_architectural_negative() {
        let fmt = NumberFormat::MetricArchitectural {
            thousands_separator: true,
        };
        assert_eq!(fmt.format_metres(-4.572), "-4\u{2009}572");
    }
}
