//! Shared wire placement contract, independent of the engine.
use serde::{Deserialize, Serialize};

/// World-space placement for a parametric instance (metres + Euler degrees).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// World-space translation in model metres: [tx, ty, tz].
    #[serde(default)]
    pub translate: [f64; 3],
    /// Euler rotation in degrees (X, Y, Z application order): [rx_deg, ry_deg, rz_deg].
    #[serde(default)]
    pub rotate_euler_deg: [f64; 3],
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_placement_fields_do_not_silently_place_at_origin() {
        for value in [
            json!({"translation":[1.0,2.0,3.0]}),
            json!({"translate":[1.0,2.0,3.0],"rotation":[0.0,90.0,0.0]}),
            json!({"translate":[1.0,2.0]}),
        ] {
            assert!(serde_json::from_value::<Placement>(value).is_err());
        }
        let error = serde_json::from_value::<Placement>(json!({"translation":[1,2,3]}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `translation`"));
        assert!(error.contains("translate"));
    }

    #[test]
    fn omitted_and_valid_saved_placements_remain_compatible() {
        assert_eq!(
            serde_json::from_value::<Placement>(json!({})).unwrap(),
            Placement::default()
        );
        let original = Placement {
            translate: [1.0, 2.0, 3.0],
            rotate_euler_deg: [0.0, 15.0, 0.0],
        };
        assert_eq!(
            serde_json::from_value::<Placement>(serde_json::to_value(&original).unwrap()).unwrap(),
            original
        );
    }

    #[cfg(feature = "model-api")]
    #[test]
    fn advertised_schema_matches_strict_decoder() {
        let schema = serde_json::to_value(schemars::schema_for!(Placement)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"].get("translate").is_some());
        assert!(schema["properties"].get("translation").is_none());
    }
}
