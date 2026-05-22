//! Transform-as-driver (PP-RPS-5, core mechanism).
//!
//! Per `RELATIONAL_PARAMETRIC_SUBSTRATE_AGREEMENT.md` ("Transform Semantics"):
//! a transform/resize gesture on a parametric component does **not** mesh-scale
//! geometry — it maps onto the component's declared **drivers**, after which the
//! parts re-derive through the dependency graph. Parameters not bound to the
//! gesture (frame profile, mullion width, chord section) are preserved because
//! they are simply not bound to the transform.
//!
//! If there is no declared driver for the requested axis, the transform
//! **refuses** and offers explicit choices (propose a driver, make a
//! freeform/unique copy, cancel). There is no silent mesh-scale path for a
//! parametric component — that is the invariant this module exists to protect.
//!
//! This module is generic core mechanism. The window-specific re-derivation math
//! lives in the architecture layer (a declared `ScalarExpr` graph); a behavioral
//! proof of that math is included here as a test so the invariant is verified.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The axis of a transform gesture in the component's local frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TransformAxis {
    X,
    Y,
    Z,
    Uniform,
}

/// The gesture itself.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransformGesture {
    /// Multiply the bound driver by `factor` (e.g. scale handle).
    Scale { factor: f64 },
    /// Set the bound driver to an absolute extent (e.g. drag an edge to 1500mm).
    SetExtent { value: f64 },
    /// Add a delta to the bound driver.
    Translate { delta: f64 },
}

/// Declares which driver each transform axis edits, for a component type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct TransformBindings {
    pub axis_to_driver: BTreeMap<TransformAxis, String>,
}

impl TransformBindings {
    pub fn bind(mut self, axis: TransformAxis, driver: impl Into<String>) -> Self {
        self.axis_to_driver.insert(axis, driver.into());
        self
    }
    pub fn driver_for(&self, axis: TransformAxis) -> Option<&String> {
        self.axis_to_driver.get(&axis)
    }
}

/// What the user can choose when a transform has no mapped driver. There is
/// deliberately **no** "mesh scale" option for parametric components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RefusalOption {
    /// Add/propose a new driver for this axis.
    ProposeDriver,
    /// Drop out of the parametric family into free-form geometry (ADR-025
    /// "make unique"); only then is mesh editing available.
    MakeUniqueFreeform,
    Cancel,
}

/// Outcome of mapping a transform gesture onto a parametric component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TransformOutcome {
    /// The gesture became a driver edit; parts re-derive from here.
    DriverEdit { driver: String, new_value: f64 },
    /// No driver for this axis — refuse, do not mesh-scale.
    Refused {
        axis: TransformAxis,
        options: Vec<RefusalOption>,
    },
}

/// Map a transform gesture onto a driver edit, or refuse. `current` is the
/// bound driver's present value (needed for Scale/Translate).
pub fn map_transform(
    bindings: &TransformBindings,
    axis: TransformAxis,
    gesture: TransformGesture,
    current: f64,
) -> TransformOutcome {
    match bindings.driver_for(axis) {
        Some(driver) => {
            let new_value = match gesture {
                TransformGesture::Scale { factor } => current * factor,
                TransformGesture::SetExtent { value } => value,
                TransformGesture::Translate { delta } => current + delta,
            };
            TransformOutcome::DriverEdit {
                driver: driver.clone(),
                new_value,
            }
        }
        None => TransformOutcome::Refused {
            axis,
            options: vec![
                RefusalOption::ProposeDriver,
                RefusalOption::MakeUniqueFreeform,
                RefusalOption::Cancel,
            ],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relational::component::{ComponentParams, DriverPolicy, OccurrenceDrivers};
    use serde_json::json;

    // A European-double-window-shaped parametric component, expressed with the
    // component model. Frame and mullion thickness are constant (read-only);
    // width/height are drivers; sash width and mullion centre are derived.
    fn window() -> (ComponentParams, TransformBindings) {
        let params = ComponentParams::default()
            .driver("width", DriverPolicy::Editable)
            .driver("height", DriverPolicy::Editable)
            .driver("frame_thickness", DriverPolicy::ReadOnly)
            .driver("mullion_width", DriverPolicy::ReadOnly)
            .derived("sash_width")
            .derived("mullion_centre");
        let bindings = TransformBindings::default()
            .bind(TransformAxis::X, "width")
            .bind(TransformAxis::Y, "height");
        (params, bindings)
    }

    // The window's declared derivation (a stand-in for the architecture-layer
    // ScalarExpr graph): sash = (width - 2*frame - mullion)/2 ; centre = width/2.
    fn derive_window(width: f64, frame: f64, mullion: f64) -> (f64, f64) {
        let sash = (width - 2.0 * frame - mullion) / 2.0;
        let centre = width / 2.0;
        (sash, centre)
    }

    #[test]
    fn width_resize_maps_to_driver_edit() {
        let (_p, b) = window();
        let out = map_transform(
            &b,
            TransformAxis::X,
            TransformGesture::SetExtent { value: 1500.0 },
            1200.0,
        );
        assert_eq!(
            out,
            TransformOutcome::DriverEdit {
                driver: "width".into(),
                new_value: 1500.0
            }
        );
    }

    #[test]
    fn window_smart_resize_keeps_frame_and_centres_mullion() {
        let frame = 58.0; // constant
        let mullion = 68.0; // constant
                            // before: width 1200
        let (sash0, centre0) = derive_window(1200.0, frame, mullion);
        // resize X -> 1500 becomes a width driver edit; re-derive
        let (_p, b) = window();
        let out = map_transform(
            &b,
            TransformAxis::X,
            TransformGesture::SetExtent { value: 1500.0 },
            1200.0,
        );
        let new_width = match out {
            TransformOutcome::DriverEdit { new_value, .. } => new_value,
            _ => panic!("expected driver edit"),
        };
        let (sash1, centre1) = derive_window(new_width, frame, mullion);
        // glazing/sashes grew
        assert!(sash1 > sash0, "sashes resize with width");
        assert_eq!(sash1, (1500.0 - 2.0 * frame - mullion) / 2.0);
        // mullion stays centered
        assert_eq!(centre1, 750.0);
        assert!(centre1 > centre0);
        // frame & mullion thickness are read-only drivers => never edited by the
        // resize: a direct attempt is refused by the component model.
        let mut occ = OccurrenceDrivers::default();
        assert!(occ
            .set_driver(&window().0, "frame_thickness", json!(80.0))
            .is_err());
        assert!(occ
            .set_driver(&window().0, "mullion_width", json!(90.0))
            .is_err());
    }

    #[test]
    fn unmapped_axis_refuses_with_options_no_mesh_scale() {
        let (_p, b) = window();
        // Z has no bound driver
        let out = map_transform(
            &b,
            TransformAxis::Z,
            TransformGesture::Scale { factor: 1.5 },
            100.0,
        );
        match out {
            TransformOutcome::Refused { axis, options } => {
                assert_eq!(axis, TransformAxis::Z);
                assert!(options.contains(&RefusalOption::ProposeDriver));
                assert!(options.contains(&RefusalOption::MakeUniqueFreeform));
                assert!(options.contains(&RefusalOption::Cancel));
                // There is no mesh-scale option — the invariant.
            }
            _ => panic!("expected refusal for unmapped axis"),
        }
    }

    #[test]
    fn scale_and_translate_gestures() {
        let b = TransformBindings::default().bind(TransformAxis::X, "span");
        assert_eq!(
            map_transform(
                &b,
                TransformAxis::X,
                TransformGesture::Scale { factor: 1.5 },
                6000.0
            ),
            TransformOutcome::DriverEdit {
                driver: "span".into(),
                new_value: 9000.0
            }
        );
        assert_eq!(
            map_transform(
                &b,
                TransformAxis::X,
                TransformGesture::Translate { delta: 600.0 },
                6000.0
            ),
            TransformOutcome::DriverEdit {
                driver: "span".into(),
                new_value: 6600.0
            }
        );
    }
}
