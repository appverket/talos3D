//! Bounded translation of a curated relational representation into ordinary
//! draft Definitions. No relational instance store is required after translation.
//! This does not infer controls from frozen geometry or create a release digest.
use super::{
    definition::*,
    occurrence::{validate_occurrence_geometry, OccurrenceIdentity},
};
use crate::{
    plugins::units::{ParameterUnit, Unit},
    relational::{
        component::{DriverPolicy, ParamRole},
        param_expr::{Quantity, ScalarExpr},
        registry::ParametricTypeDef,
    },
};
use serde_json::json;
use std::collections::BTreeSet;

/// Dependency order, root last. Domain member annotations are retained verbatim
/// in child Definition domain_data; the domain owns their interpretation.
pub fn translate_representation(
    source: &ParametricTypeDef,
    root_id: DefinitionId,
) -> Result<Vec<Definition>, String> {
    let representation = source
        .representation
        .as_ref()
        .ok_or("No emitting representation; cannot create an executable Definition")?;
    if representation.members.is_empty() || representation.members.len() > 256 {
        return Err("Native representation translation requires 1..=256 members".into());
    }
    let mut parameters = Vec::new();
    for (name, role) in &source.params.roles {
        if let ParamRole::Driver { policy } = role {
            let unit = *source
                .driver_units
                .get(name)
                .ok_or_else(|| format!("Missing unit for driver '{name}'"))?;
            let default = *source
                .defaults
                .get(name)
                .ok_or_else(|| format!("Missing default for driver '{name}'"))?;
            if !default.is_finite() {
                return Err(format!("Non-finite driver default '{name}'"));
            }
            parameters.push(ParameterDef {
                name: name.clone(),
                param_type: ParamType::Numeric,
                default_value: json!(default),
                override_policy: match policy {
                    DriverPolicy::Editable => OverridePolicy::Overridable,
                    _ => OverridePolicy::Locked,
                },
                geometry_affecting: true,
                metadata: ParameterMetadata {
                    unit: Some(ParameterUnit::typed(unit)),
                    ..Default::default()
                },
            });
        } else if !source.derivations.contains_key(name) {
            return Err(format!("Missing expression for derived parameter '{name}'"));
        }
    }
    // ScalarExpr's declared dependencies, rather than map insertion order,
    // determine execution order. Unknown dependencies and cycles are refusals.
    let mut available: BTreeSet<String> = parameters.iter().map(|p| p.name.clone()).collect();
    let mut pending = source.derivations.clone();
    let mut derived = Vec::new();
    while !pending.is_empty() {
        let name = pending
            .iter()
            .find(|(_, expr)| expr.dependencies().iter().all(|d| available.contains(d)))
            .map(|(name, _)| name.clone())
            .ok_or("Unresolved or cyclic representation derivations")?;
        if available.contains(&name) {
            return Err(format!("Driver/derived name collision '{name}'"));
        }
        let expr = pending.remove(&name).expect("selected derivation");
        derived.push(DerivedParameterDef {
            name: name.clone(),
            param_type: ParamType::Numeric,
            dependencies: expr.dependencies().into_iter().collect(),
            expr: BodyExpr::Scalar { expr },
            metadata: Default::default(),
        });
        available.insert(name);
    }
    let interface = Interface {
        parameters: ParameterSchema(parameters),
        ..Default::default()
    };
    let mut definitions = Vec::new();
    let mut slots = Vec::new();
    for (index, member) in representation.members.iter().enumerate() {
        let slot_id = format!("member_{index:03}");
        let child_id = DefinitionId(format!("{}.{slot_id}", root_id.0));
        let profile = if member.profile_xz.is_empty() {
            // Profile2d::rectangle is centered in X/Z. Preserve that convention.
            let half = |axis: usize, sign: f64| ScalarExpr::Mul {
                lhs: Box::new(member.size[axis].clone()),
                rhs: Box::new(ScalarExpr::lit(Quantity::num(sign * 0.5))),
            };
            vec![
                [half(0, -1.0), half(2, -1.0)],
                [half(0, 1.0), half(2, -1.0)],
                [half(0, 1.0), half(2, 1.0)],
                [half(0, -1.0), half(2, 1.0)],
            ]
        } else {
            member.profile_xz.clone()
        };
        let child = Definition {
            id: child_id.clone(),
            base_definition_id: None,
            name: member.label.clone().unwrap_or_else(|| slot_id.clone()),
            definition_kind: DefinitionKind::Solid,
            definition_version: 1,
            visibility: DefinitionVisibility::InternalPart,
            interface: interface.clone(),
            body: DefinitionBody::new(
                vec![EvaluatorDecl::PolygonExtrusion(PolygonExtrusionEvaluator {
                    coordinate_unit: Unit::Mm,
                    profile_xz: profile
                        .into_iter()
                        .map(|p| p.map(|expr| BodyExpr::Scalar { expr }))
                        .collect(),
                    height: BodyExpr::Scalar {
                        expr: member.size[1].clone(),
                    },
                })],
                vec![],
                Some(CompoundDefinition {
                    derived_parameters: derived.clone(),
                    ..Default::default()
                }),
            ),
            material_assignment: None,
            domain_data: json!({"parametric_member_semantic": member.semantic, "source_member_index": index}),
        };
        slots.push(ChildSlotDef {
            slot_id: slot_id.clone(),
            role: member.label.clone().unwrap_or(slot_id),
            definition_id: child_id,
            parameter_bindings: interface
                .parameters
                .0
                .iter()
                .map(|p| ParameterBinding {
                    target_param: p.name.clone(),
                    expr: BodyExpr::Reference {
                        path: p.name.clone(),
                    },
                })
                .collect(),
            transform_binding: TransformBinding {
                translation_unit: Some(Unit::Mm),
                // Preserve the declared source unit; normalize only at evaluation.
                translation: Some(
                    member
                        .translate
                        .iter()
                        .map(|expr| BodyExpr::Scalar { expr: expr.clone() })
                        .collect(),
                ),
                rotation_euler_deg: Some(
                    member
                        .rotate_euler_deg
                        .iter()
                        .map(|expr| BodyExpr::Scalar { expr: expr.clone() })
                        .collect(),
                ),
            },
            suppression_expr: None,
            multiplicity: SlotMultiplicity::Single,
        });
        definitions.push(child);
    }
    definitions.push(Definition {
        id: root_id.clone(), base_definition_id: None, name: source.label.clone(),
        definition_kind: DefinitionKind::Solid, definition_version: 1, visibility: DefinitionVisibility::PublicRoot,
        interface, body: DefinitionBody::new(vec![], vec![], Some(CompoundDefinition { child_slots: slots, derived_parameters: derived, ..Default::default() })),
        material_assignment: None,
        domain_data: json!({"translated_representation": {"source_type_id": source.id, "source_driver_policies": source.params,
            "scope": "draft_native_geometry_and_controls", "source_member_units": "mm", "source_rotation_units": "deg"}}),
    });
    let mut registry = DefinitionRegistry::default();
    for definition in &definitions {
        registry.validate_definition(definition)?;
        registry.insert(definition.clone());
    }
    validate_occurrence_geometry(&registry, &OccurrenceIdentity::new(root_id, 1))?;
    Ok(definitions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authored_entity::AuthoredEntity,
        plugins::{
            identity::ElementId,
            modeling::{
                occurrence::{GeneratedOccurrencePart, OccurrenceSnapshot},
                primitives::ShapeRotation,
                profile::ProfileExtrusion,
            },
        },
        relational::{
            component::ComponentParams,
            registry::{ParametricMember, ParametricRepresentation},
            transform::TransformBindings,
        },
    };
    use bevy::prelude::*;
    use std::collections::BTreeMap;

    fn source() -> ParametricTypeDef {
        let width = ScalarExpr::Param {
            name: "width".into(),
        };
        let mm = |v| ScalarExpr::lit(Quantity::mm(v));
        let zero = ScalarExpr::lit(Quantity::num(0.0));
        ParametricTypeDef {
            id: "test.native_profile".into(),
            label: "Native profile".into(),
            public: true,
            params: ComponentParams::default()
                .driver("width", DriverPolicy::Editable)
                .driver("depth", DriverPolicy::ReadOnly),
            defaults: BTreeMap::from([("width".into(), 2000.0), ("depth".into(), 100.0)]),
            driver_units: BTreeMap::from([("width".into(), Unit::Mm), ("depth".into(), Unit::Mm)]),
            derivations: BTreeMap::new(),
            transform: TransformBindings::default(),
            representation: Some(ParametricRepresentation {
                members: vec![ParametricMember {
                    size: [
                        width.clone(),
                        ScalarExpr::Param {
                            name: "depth".into(),
                        },
                        mm(1000.0),
                    ],
                    translate: [mm(500.0), mm(200.0), mm(300.0)],
                    rotate_euler_deg: [zero.clone(), ScalarExpr::lit(Quantity::deg(30.0)), zero],
                    profile_xz: vec![[mm(0.0), mm(0.0)], [width, mm(0.0)], [mm(0.0), mm(1000.0)]],
                    label: Some("triangular part".into()),
                    semantic: None,
                }],
            }),
        }
    }

    #[test]
    fn translated_body_roundtrips_and_matches_curated_geometry_at_changed_controls() {
        let source = source();
        let defs = translate_representation(&source, DefinitionId("native.test".into())).unwrap();
        let encoded = serde_json::to_vec(&defs).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("ParametricStore"));
        let loaded: Vec<Definition> = serde_json::from_slice(&encoded).unwrap();
        let mut registry = DefinitionRegistry::default();
        for def in loaded {
            registry.insert(def);
        }
        let mut world = World::new();
        world.insert_resource(registry);
        let origin = Vec3::new(5.0, 2.0, -4.0);
        let rotation = Quat::from_rotation_y(0.4);
        for width in [2000.0, 3500.0] {
            let mut id = OccurrenceIdentity::new(DefinitionId("native.test".into()), 1);
            id.overrides.set("width", json!(width));
            let mut snapshot = OccurrenceSnapshot::new(ElementId(7), id, "Control test");
            snapshot.offset = origin;
            snapshot.rotation = rotation;
            snapshot.apply_to(&mut world);
            let expected = source
                .evaluate_representation(&BTreeMap::from([("width".into(), json!(width))]))
                .unwrap()
                .unwrap();
            let actual: Vec<_> = world
                .query::<(&GeneratedOccurrencePart, &ProfileExtrusion, &ShapeRotation)>()
                .iter(&world)
                .collect();
            assert_eq!(actual.len(), 1);
            let (part, geometry, shape_rotation) = actual[0];
            assert_eq!(part.owner, ElementId(7));
            assert_eq!(part.slot_path, "member_000");
            let e = &expected[0];
            assert!(geometry.centre.abs_diff_eq(
                origin + rotation * Vec3::from_array(e.translate.map(|v| v as f32 * 0.001)),
                1e-6
            ));
            assert!((geometry.height - e.size[1] as f32 * 0.001).abs() < 1e-6);
            assert!(shape_rotation.0.abs_diff_eq(
                rotation * Quat::from_rotation_y(30.0_f32.to_radians()),
                1e-6
            ));
            let points = geometry.profile.tessellate(1);
            for p in &e.profile_xz {
                assert!(points
                    .iter()
                    .any(|v| v
                        .abs_diff_eq(Vec2::new(p[0] as f32 * 0.001, p[1] as f32 * 0.001), 1e-6)));
            }
        }
        let mut locked = OccurrenceIdentity::new(DefinitionId("native.test".into()), 1);
        locked.overrides.set("depth", json!(500.0));
        assert!(
            validate_occurrence_geometry(world.resource::<DefinitionRegistry>(), &locked).is_err()
        );
    }

    #[test]
    fn late_child_failure_keeps_the_existing_body_intact() {
        let mut source = source();
        let mut second = source.representation.as_ref().unwrap().members[0].clone();
        second.size[1] = ScalarExpr::Sub {
            lhs: Box::new(ScalarExpr::lit(Quantity::mm(3000.0))),
            rhs: Box::new(ScalarExpr::Param {
                name: "width".into(),
            }),
        };
        source.representation.as_mut().unwrap().members.push(second);
        let definitions =
            translate_representation(&source, DefinitionId("atomic.profile".into())).unwrap();
        let mut registry = DefinitionRegistry::default();
        for definition in definitions {
            registry.insert(definition);
        }
        let mut world = World::new();
        world.insert_resource(registry.clone());
        let identity = OccurrenceIdentity::new(DefinitionId("atomic.profile".into()), 1);
        super::super::occurrence::render_occurrence(
            &mut world,
            &registry,
            ElementId(7),
            &identity,
            Transform::default(),
            None,
        )
        .unwrap();
        let before: Vec<_> = world
            .query::<(Entity, &GeneratedOccurrencePart, &ProfileExtrusion)>()
            .iter(&world)
            .map(|(entity, part, geometry)| (entity, part.slot_path.clone(), geometry.clone()))
            .collect();
        let mut invalid = identity.clone();
        invalid.overrides.set("width", json!(4000.0));
        assert!(super::super::occurrence::render_occurrence(
            &mut world,
            &registry,
            ElementId(7),
            &invalid,
            Transform::default(),
            None
        )
        .is_err());
        let after: Vec<_> = world
            .query::<(Entity, &GeneratedOccurrencePart, &ProfileExtrusion)>()
            .iter(&world)
            .map(|(entity, part, geometry)| (entity, part.slot_path.clone(), geometry.clone()))
            .collect();
        assert_eq!(
            after, before,
            "even render entities survive a late child refusal"
        );
        assert_eq!(
            serde_json::to_value(world.query::<&OccurrenceIdentity>().single(&world).unwrap())
                .unwrap(),
            serde_json::to_value(&identity).unwrap()
        );
    }

    #[test]
    fn translation_refuses_absent_units_and_unresolved_derivations() {
        let mut source = source();
        source.driver_units.remove("width");
        assert!(
            translate_representation(&source, DefinitionId("bad".into()))
                .unwrap_err()
                .contains("Missing unit")
        );
        source.driver_units.insert("width".into(), Unit::Mm);
        source.derivations.insert(
            "lost".into(),
            ScalarExpr::Param {
                name: "missing".into(),
            },
        );
        assert!(
            translate_representation(&source, DefinitionId("bad".into()))
                .unwrap_err()
                .contains("Unresolved")
        );
    }
}
