use bevy::{ecs::world::EntityRef, prelude::*};
use serde_json::Value;
use talos3d_capability_api::commands::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
use talos3d_core::plugins::{
    commands::{BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand, EndCommandGroup},
    identity::{ElementId, ElementIdAllocator},
    modeling::primitives::{ElevationMetadata, Polyline},
    selection::Selected,
    ui::StatusBarData,
};

use crate::{
    components::{
        ElevationCurve, ElevationCurveType, TerrainMeshCache, TerrainSurface,
        DEFAULT_TERRAIN_CONTOUR_INTERVAL, DEFAULT_TERRAIN_CONTOUR_JOIN_TOLERANCE,
        DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING, DEFAULT_TERRAIN_MAX_TRIANGLE_AREA,
        DEFAULT_TERRAIN_MINIMUM_ANGLE,
    },
    cut_fill::{cut_fill_against_datum, cut_fill_between_surfaces, CutFillOptions, CutFillResult},
    reconstruction::{
        estimate_terrain_boundary, planar_bounds_center, repair_elevation_curves,
        ContourRepairSettings,
    },
    review::TerrainGenerationReviewState,
    snapshots::{ElevationCurveSnapshot, TerrainSurfaceSnapshot},
};

pub struct TerrainCommandPlugin;

impl Plugin for TerrainCommandPlugin {
    fn build(&self, app: &mut App) {
        app.register_command(
            CommandDescriptor {
                id: "terrain.convert_to_elevation_curves".to_string(),
                label: "Convert To Elevation Curves".to_string(),
                description: "Convert selected imported contour polylines into terrain elevation curves.".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "delete_source": {"type": "boolean"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Convert selected imported contour polylines into terrain elevation curves".to_string()),
                requires_selection: true,
                show_in_menu: true,
            version: 1,
                activates_tool: None,
                    capability_id: Some("terrain".to_string()),
            },
            execute_convert_to_elevation_curves,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.generate_surface".to_string(),
                label: "Generate Terrain".to_string(),
                description: "Open terrain generation review for the selected elevation curves.".to_string(),
                category: CommandCategory::Create,
                parameters: None,
                default_shortcut: Some("T".to_string()),
                icon: Some("icon.create".to_string()),
                hint: Some("Select elevation curves, then review and generate a terrain surface".to_string()),
                requires_selection: true,
                show_in_menu: true,
            version: 1,
                activates_tool: None,
                    capability_id: Some("terrain".to_string()),
            },
            execute_generate_surface,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.prepare_site_surface".to_string(),
                label: "Prepare Site Surface".to_string(),
                description: "Repair selected survey contours, create elevation curves, and drape a terrain surface over them.".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "delete_source": {"type": "boolean"},
                        "center_at_origin": {"type": "boolean"},
                        "contour_layers": {
                            "type": "array",
                            "items": {"type": "string"}
                        },
                        "join_tolerance": {"type": "number", "minimum": 0.01},
                        "drape_sample_spacing": {"type": "number", "minimum": 0.1},
                        "max_triangle_area": {"type": "number", "minimum": 0.1},
                        "minimum_angle": {"type": "number", "minimum": 0.1, "maximum": 89.0},
                        "contour_interval": {"type": "number", "minimum": 0.1}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Select imported contours or elevation curves to build a draped site surface".to_string()),
                requires_selection: true,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_prepare_site_surface,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.cut_fill_analysis".to_string(),
                label: "Cut/Fill Analysis".to_string(),
                description: "Compute cut, fill, and net volumes between terrain surfaces or a datum.".to_string(),
                category: CommandCategory::Custom("Analysis".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "required": ["existing_surface_id"],
                    "properties": {
                        "existing_surface_id": {"type": "integer", "minimum": 0},
                        "proposed_surface_id": {"type": "integer", "minimum": 0},
                        "datum_y": {"type": "number"},
                        "sample_spacing": {"type": "number", "minimum": 0.01},
                        "boundary": {
                            "type": "array",
                            "items": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 2,
                                "items": {"type": "number"}
                            }
                        }
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.measure".to_string()),
                hint: Some("Compare an existing terrain surface to a proposed surface or datum".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_cut_fill_analysis,
        );
    }
}

#[derive(Debug, Clone)]
struct SelectedContourInput {
    source_id: ElementId,
    curve: ElevationCurve,
}

#[derive(Debug, Clone, Default)]
struct ContourSelectionSummary {
    accepted_inputs: Vec<SelectedContourInput>,
    accepted_layers: Vec<String>,
    rejected_layers: Vec<String>,
}

fn execute_convert_to_elevation_curves(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let delete_source = parameters
        .get("delete_source")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let selection = selected_elevation_source_polylines(world)?;

    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup {
            label: if delete_source {
                "Convert contours to elevation curves"
            } else {
                "Copy contours to elevation curves"
            },
        });

    let mut create_events = world.resource_mut::<Messages<CreateEntityCommand>>();
    for (_, snapshot) in &selection {
        create_events.write(CreateEntityCommand {
            snapshot: snapshot.clone().into(),
        });
    }
    let _ = create_events;

    if delete_source {
        world
            .resource_mut::<Messages<DeleteEntitiesCommand>>()
            .write(DeleteEntitiesCommand {
                element_ids: selection.iter().map(|(id, _)| *id).collect(),
            });
    }

    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(
            format!(
                "Queued {} elevation curve{}",
                selection.len(),
                if selection.len() == 1 { "" } else { "s" }
            ),
            2.0,
        );
    }
    Ok(CommandResult::empty())
}

fn execute_generate_surface(world: &mut World, _: &Value) -> Result<CommandResult, String> {
    let curves = selected_elevation_curve_summaries(world)?;
    let source_curve_ids = curves
        .iter()
        .map(|(element_id, _)| *element_id)
        .collect::<Vec<_>>();
    let boundary = estimate_terrain_boundary(
        &curves
            .iter()
            .map(|(_, curve)| curve.clone())
            .collect::<Vec<_>>(),
        DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING,
    );
    let snapshot = TerrainSurfaceSnapshot {
        element_id: world.resource::<ElementIdAllocator>().next_id(),
        surface: TerrainSurface {
            name: "Terrain Surface".to_string(),
            source_curve_ids,
            datum_elevation: curves
                .iter()
                .map(|(_, curve)| curve.elevation)
                .reduce(f32::min)
                .unwrap_or_default(),
            boundary,
            max_triangle_area: DEFAULT_TERRAIN_MAX_TRIANGLE_AREA,
            minimum_angle: DEFAULT_TERRAIN_MINIMUM_ANGLE,
            contour_interval: DEFAULT_TERRAIN_CONTOUR_INTERVAL,
            drape_sample_spacing: DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING,
            offset: Vec3::ZERO,
        },
    };

    let mut review = world.resource_mut::<TerrainGenerationReviewState>();
    review.curve_count = curves.len();
    review.preview_surface = Some(snapshot);
    review.preview_entity = None;
    review.frame_requested = true;

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(
            "Review terrain settings, then click Generate".to_string(),
            2.0,
        );
    }

    Ok(CommandResult::empty())
}

fn execute_prepare_site_surface(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let requested_contour_layers = parameters
        .get("contour_layers")
        .and_then(Value::as_array)
        .map(|layers| {
            layers
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selection = selected_contour_inputs(world, &requested_contour_layers)?;
    let delete_source = parameters
        .get("delete_source")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let center_at_origin = parameters
        .get("center_at_origin")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let join_tolerance = parameters
        .get("join_tolerance")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_CONTOUR_JOIN_TOLERANCE);
    let drape_sample_spacing = parameters
        .get("drape_sample_spacing")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING);
    let max_triangle_area = parameters
        .get("max_triangle_area")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_MAX_TRIANGLE_AREA);
    let minimum_angle = parameters
        .get("minimum_angle")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_MINIMUM_ANGLE);
    let contour_interval = parameters
        .get("contour_interval")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_CONTOUR_INTERVAL);
    let surface_name = parameters
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Site Surface")
        .to_string();

    let repaired = repair_elevation_curves(
        &selection
            .accepted_inputs
            .iter()
            .map(|entry| entry.curve.clone())
            .collect::<Vec<_>>(),
        ContourRepairSettings {
            join_tolerance,
            ..Default::default()
        },
    );
    if repaired.is_empty() {
        return Err("Selected contours could not be repaired into terrain curves".to_string());
    }

    let mut repaired_curves = repaired
        .iter()
        .map(|curve| curve.curve.clone())
        .collect::<Vec<_>>();
    let mut boundary = estimate_terrain_boundary(&repaired_curves, drape_sample_spacing);
    if center_at_origin {
        if let Some(center) = planar_bounds_center(
            repaired_curves
                .iter()
                .flat_map(|curve| curve.points.iter().copied()),
        ) {
            let delta = Vec3::new(-center.x, 0.0, -center.y);
            for curve in &mut repaired_curves {
                for point in &mut curve.points {
                    *point += delta;
                }
            }
            for point in &mut boundary {
                *point -= center;
            }
        }
    }

    let (curve_snapshots, surface_element_id) = {
        let allocator = world.resource_mut::<ElementIdAllocator>();
        let curve_snapshots = repaired_curves
            .into_iter()
            .map(|repaired_curve| {
                let element_id = allocator.next_id();
                (
                    element_id,
                    ElevationCurveSnapshot {
                        element_id,
                        curve: repaired_curve,
                    },
                )
            })
            .collect::<Vec<_>>();
        let surface_element_id = allocator.next_id();
        (curve_snapshots, surface_element_id)
    };
    let surface_snapshot = TerrainSurfaceSnapshot {
        element_id: surface_element_id,
        surface: TerrainSurface {
            name: surface_name,
            source_curve_ids: curve_snapshots
                .iter()
                .map(|(element_id, _)| *element_id)
                .collect(),
            datum_elevation: curve_snapshots
                .iter()
                .map(|(_, snapshot)| snapshot.curve.elevation)
                .reduce(f32::min)
                .unwrap_or_default(),
            boundary,
            max_triangle_area,
            minimum_angle,
            contour_interval,
            drape_sample_spacing,
            offset: Vec3::ZERO,
        },
    };

    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup {
            label: "Prepare site surface from contours",
        });

    let mut create_events = world.resource_mut::<Messages<CreateEntityCommand>>();
    for (_, snapshot) in &curve_snapshots {
        create_events.write(CreateEntityCommand {
            snapshot: snapshot.clone().into(),
        });
    }
    create_events.write(CreateEntityCommand {
        snapshot: surface_snapshot.clone().into(),
    });
    let _ = create_events;

    if delete_source {
        world
            .resource_mut::<Messages<DeleteEntitiesCommand>>()
            .write(DeleteEntitiesCommand {
                element_ids: selection
                    .accepted_inputs
                    .iter()
                    .map(|entry| entry.source_id)
                    .collect(),
            });
    }

    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(
            format!(
                "Queued {} repaired curve{} and 1 terrain surface",
                curve_snapshots.len(),
                if curve_snapshots.len() == 1 { "" } else { "s" }
            ),
            2.5,
        );
    }

    Ok(CommandResult {
        created: curve_snapshots
            .iter()
            .map(|(element_id, _)| element_id.0)
            .chain(std::iter::once(surface_element_id.0))
            .collect(),
        modified: Vec::new(),
        deleted: if delete_source {
            selection
                .accepted_inputs
                .iter()
                .map(|entry| entry.source_id.0)
                .collect()
        } else {
            Vec::new()
        },
        output: Some(serde_json::json!({
            "surface_id": surface_element_id.0,
            "curve_ids": curve_snapshots.iter().map(|(element_id, _)| element_id.0).collect::<Vec<_>>(),
            "repaired_curve_count": curve_snapshots.len(),
            "source_fragment_count": selection.accepted_inputs.len(),
            "inserted_bridge_count": repaired.iter().map(|curve| curve.inserted_bridge_count).sum::<usize>(),
            "center_at_origin": center_at_origin,
            "used_layers": selection.accepted_layers,
            "skipped_layers": selection.rejected_layers,
        })),
    })
}

fn execute_cut_fill_analysis(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let existing_id = required_element_id(parameters, "existing_surface_id")?;
    let existing = terrain_mesh_by_id(world, existing_id)
        .ok_or_else(|| format!("Terrain surface {} has no generated mesh", existing_id.0))?;
    let sample_spacing = parameters
        .get("sample_spacing")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .unwrap_or(DEFAULT_TERRAIN_DRAPE_SAMPLE_SPACING);
    let options = CutFillOptions::new(sample_spacing).with_boundary(parse_boundary(parameters)?);

    let (result, comparison) = if let Some(proposed_id) =
        optional_element_id(parameters, "proposed_surface_id")?
    {
        let proposed = terrain_mesh_by_id(world, proposed_id)
            .ok_or_else(|| format!("Terrain surface {} has no generated mesh", proposed_id.0))?;
        let result = cut_fill_between_surfaces(&existing, &proposed, &options)
            .ok_or_else(|| "Cut/fill analysis had no overlapping terrain samples".to_string())?;
        (
            result,
            serde_json::json!({ "proposed_surface_id": proposed_id.0 }),
        )
    } else if let Some(datum_y) = parameters.get("datum_y").and_then(Value::as_f64) {
        let datum_y = datum_y as f32;
        let result = cut_fill_against_datum(&existing, datum_y, &options)
            .ok_or_else(|| "Cut/fill analysis had no terrain samples".to_string())?;
        (result, serde_json::json!({ "datum_y": datum_y }))
    } else {
        return Err("Provide either proposed_surface_id or datum_y".to_string());
    };

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(format_cut_fill_feedback(&result), 3.0);
    }

    Ok(CommandResult {
        output: Some(serde_json::json!({
            "existing_surface_id": existing_id.0,
            "comparison": comparison,
            "cut_volume": result.cut_volume,
            "fill_volume": result.fill_volume,
            "net_volume": result.net_volume,
            "sample_count": result.sample_count,
            "sample_spacing": options.sample_spacing,
            "boundary_vertex_count": options.boundary.len(),
        })),
        ..CommandResult::empty()
    })
}

fn required_element_id(parameters: &Value, key: &str) -> Result<ElementId, String> {
    optional_element_id(parameters, key)?.ok_or_else(|| format!("Missing required parameter {key}"))
}

fn optional_element_id(parameters: &Value, key: &str) -> Result<Option<ElementId>, String> {
    parameters
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .map(ElementId)
                .ok_or_else(|| format!("{key} must be an unsigned integer element id"))
        })
        .transpose()
}

fn parse_boundary(parameters: &Value) -> Result<Vec<Vec2>, String> {
    let Some(boundary) = parameters.get("boundary") else {
        return Ok(Vec::new());
    };
    let Some(points) = boundary.as_array() else {
        return Err("boundary must be an array of [x, z] points".to_string());
    };
    points
        .iter()
        .map(|point| {
            let Some(coords) = point.as_array() else {
                return Err("boundary points must be [x, z] arrays".to_string());
            };
            if coords.len() != 2 {
                return Err("boundary points must contain exactly two numbers".to_string());
            }
            let x = coords[0]
                .as_f64()
                .ok_or_else(|| "boundary x coordinate must be a number".to_string())?
                as f32;
            let z = coords[1]
                .as_f64()
                .ok_or_else(|| "boundary z coordinate must be a number".to_string())?
                as f32;
            Ok(Vec2::new(x, z))
        })
        .collect()
}

fn terrain_mesh_by_id(
    world: &World,
    element_id: ElementId,
) -> Option<talos3d_core::plugins::modeling::primitives::TriangleMesh> {
    let mut q = world.try_query::<EntityRef>()?;
    q.iter(world).find_map(|entity_ref| {
        if entity_ref.get::<ElementId>() != Some(&element_id) {
            return None;
        }
        entity_ref
            .get::<TerrainMeshCache>()
            .map(|cache| cache.mesh.clone())
    })
}

fn format_cut_fill_feedback(result: &CutFillResult) -> String {
    format!(
        "Cut {:.2} m^3, fill {:.2} m^3, net {:.2} m^3",
        result.cut_volume, result.fill_volume, result.net_volume
    )
}

fn selected_elevation_source_polylines(
    world: &World,
) -> Result<Vec<(ElementId, ElevationCurveSnapshot)>, String> {
    let allocator = world.resource::<ElementIdAllocator>();
    let mut results = Vec::new();
    let mut __q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in __q.iter(world) {
        if !entity_ref.contains::<Selected>() {
            continue;
        }
        let (Some(source_id), Some(polyline), Some(metadata)) = (
            entity_ref.get::<ElementId>(),
            entity_ref.get::<Polyline>(),
            entity_ref.get::<ElevationMetadata>(),
        ) else {
            continue;
        };
        let Some(curve_type) = detect_contour_layer_type(&metadata.source_layer) else {
            continue;
        };
        results.push((
            *source_id,
            ElevationCurveSnapshot {
                element_id: allocator.next_id(),
                curve: ElevationCurve {
                    points: polyline.points.clone(),
                    elevation: metadata.elevation,
                    source_layer: metadata.source_layer.clone(),
                    curve_type,
                    survey_source_id: metadata.survey_source_id.clone(),
                },
            },
        ));
    }

    if results.is_empty() {
        Err("Select imported polylines with elevation metadata before converting".to_string())
    } else {
        Ok(results)
    }
}

fn selected_elevation_curve_summaries(
    world: &World,
) -> Result<Vec<(ElementId, ElevationCurve)>, String> {
    let mut results = Vec::new();
    let mut __q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in __q.iter(world) {
        if !entity_ref.contains::<Selected>() {
            continue;
        }
        let (Some(element_id), Some(curve)) = (
            entity_ref.get::<ElementId>(),
            entity_ref.get::<ElevationCurve>(),
        ) else {
            continue;
        };
        results.push((*element_id, curve.clone()));
    }
    if results.is_empty() {
        Err("Select elevation curves before generating terrain".to_string())
    } else {
        Ok(results)
    }
}

fn selected_contour_inputs(
    world: &World,
    requested_contour_layers: &[String],
) -> Result<ContourSelectionSummary, String> {
    let requested_layers = normalize_requested_layers(requested_contour_layers);
    let mut results = Vec::new();
    let mut accepted_layers = std::collections::BTreeSet::new();
    let mut rejected_layers = std::collections::BTreeSet::new();
    let mut __q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in __q.iter(world) {
        if !entity_ref.contains::<Selected>() {
            continue;
        }
        let Some(source_id) = entity_ref.get::<ElementId>() else {
            continue;
        };

        if let (Some(polyline), Some(metadata)) = (
            entity_ref.get::<Polyline>(),
            entity_ref.get::<ElevationMetadata>(),
        ) {
            let Some(curve_type) =
                classify_selected_layer(&metadata.source_layer, &requested_layers)
            else {
                rejected_layers.insert(metadata.source_layer.clone());
                continue;
            };
            accepted_layers.insert(metadata.source_layer.clone());
            results.push(SelectedContourInput {
                source_id: *source_id,
                curve: ElevationCurve {
                    points: polyline.points.clone(),
                    elevation: metadata.elevation,
                    source_layer: metadata.source_layer.clone(),
                    curve_type,
                    survey_source_id: metadata.survey_source_id.clone(),
                },
            });
            continue;
        }

        if let Some(curve) = entity_ref.get::<ElevationCurve>() {
            let Some(curve_type) = classify_selected_layer(&curve.source_layer, &requested_layers)
            else {
                rejected_layers.insert(curve.source_layer.clone());
                continue;
            };
            accepted_layers.insert(curve.source_layer.clone());
            results.push(SelectedContourInput {
                source_id: *source_id,
                curve: ElevationCurve {
                    curve_type,
                    ..curve.clone()
                },
            });
        }
    }

    if results.is_empty() {
        if requested_layers.is_empty() {
            Err("Select imported contour polylines or elevation curves from contour layers before preparing a site surface".to_string())
        } else {
            Err(format!(
                "Selection did not contain any contour inputs on the requested layers: {}",
                requested_contour_layers.join(", ")
            ))
        }
    } else {
        Ok(ContourSelectionSummary {
            accepted_inputs: results,
            accepted_layers: accepted_layers.into_iter().collect(),
            rejected_layers: rejected_layers.into_iter().collect(),
        })
    }
}

fn normalize_requested_layers(layers: &[String]) -> std::collections::BTreeSet<String> {
    layers
        .iter()
        .map(|layer| canonicalize_layer_name(layer))
        .filter(|layer| !layer.is_empty())
        .collect()
}

fn classify_selected_layer(
    layer_name: &str,
    requested_layers: &std::collections::BTreeSet<String>,
) -> Option<ElevationCurveType> {
    if requested_layers.is_empty() {
        detect_contour_layer_type(layer_name)
    } else if requested_layers.contains(&canonicalize_layer_name(layer_name)) {
        Some(infer_curve_type(layer_name))
    } else {
        None
    }
}

fn detect_contour_layer_type(layer_name: &str) -> Option<ElevationCurveType> {
    let layer_name = canonicalize_layer_name(layer_name);
    if layer_name.is_empty() || layer_name.contains("text") {
        return None;
    }

    let is_contour_layer = layer_name.contains("hojdkurva")
        || layer_name.contains("höjdkurva")
        || layer_name.contains("contour")
        || layer_name.contains("levelcurve")
        || layer_name.contains("elev");
    if !is_contour_layer {
        return None;
    }

    if layer_name.contains("byggnad")
        || layer_name.contains("tak")
        || layer_name.contains("altan")
        || layer_name.contains("hydrografi")
        || layer_name.contains("markanordning")
        || layer_name.contains("mark_symbol")
        || layer_name.contains("vag")
        || layer_name.contains("väg")
        || layer_name.contains("grans")
        || layer_name.contains("gräns")
        || layer_name.contains("ram")
        || layer_name.contains("prickmark")
    {
        return None;
    }

    Some(infer_curve_type(layer_name.as_str()))
}

fn infer_curve_type(layer_name: &str) -> ElevationCurveType {
    let layer_name = canonicalize_layer_name(layer_name);
    if layer_name.contains("index") {
        ElevationCurveType::Index
    } else if layer_name.contains("major") || layer_name == "hojdkurva" {
        ElevationCurveType::Major
    } else if layer_name.contains("minor")
        || layer_name.contains("halv")
        || layer_name.contains("half")
    {
        ElevationCurveType::Minor
    } else if layer_name.contains("hojdkurva") || layer_name.contains("höjdkurva") {
        ElevationCurveType::Major
    } else {
        ElevationCurveType::Supplementary
    }
}

fn canonicalize_layer_name(layer_name: &str) -> String {
    layer_name
        .trim()
        .to_lowercase()
        .replace([' ', '-', '.'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use talos3d_core::plugins::{modeling::primitives::TriangleMesh, ui::StatusBarData};

    fn init_command_test_world() -> World {
        let mut world = World::new();
        world.insert_resource(Messages::<BeginCommandGroup>::default());
        world.insert_resource(Messages::<CreateEntityCommand>::default());
        world.insert_resource(Messages::<DeleteEntitiesCommand>::default());
        world.insert_resource(Messages::<EndCommandGroup>::default());
        world.insert_resource(ElementIdAllocator::default());
        world.insert_resource(StatusBarData::default());
        world
    }

    fn flat_square(elevation: f32) -> TerrainMeshCache {
        TerrainMeshCache {
            mesh: TriangleMesh {
                vertices: vec![
                    Vec3::new(0.0, elevation, 0.0),
                    Vec3::new(2.0, elevation, 0.0),
                    Vec3::new(2.0, elevation, 2.0),
                    Vec3::new(0.0, elevation, 2.0),
                ],
                faces: vec![[0, 1, 2], [0, 2, 3]],
                normals: None,
                name: None,
            },
            contour_segments: Vec::new(),
        }
    }

    #[test]
    fn cut_fill_command_outputs_surface_comparison() {
        let mut world = init_command_test_world();
        world.spawn((ElementId(10), flat_square(2.0)));
        world.spawn((ElementId(20), flat_square(1.5)));

        let result = execute_cut_fill_analysis(
            &mut world,
            &json!({
                "existing_surface_id": 10,
                "proposed_surface_id": 20,
                "sample_spacing": 1.0
            }),
        )
        .expect("cut/fill command should succeed");
        let output = result.output.expect("command returns output");

        assert_eq!(output["existing_surface_id"], json!(10));
        assert_eq!(output["comparison"]["proposed_surface_id"], json!(20));
        assert_eq!(output["sample_count"], json!(4));
        assert_eq!(output["cut_volume"], json!(2.0));
        assert_eq!(output["fill_volume"], json!(0.0));
        assert_eq!(output["net_volume"], json!(2.0));
    }

    #[test]
    fn cut_fill_command_supports_datum_and_boundary() {
        let mut world = init_command_test_world();
        world.spawn((ElementId(10), flat_square(2.0)));

        let result = execute_cut_fill_analysis(
            &mut world,
            &json!({
                "existing_surface_id": 10,
                "datum_y": 1.0,
                "sample_spacing": 1.0,
                "boundary": [[0.0, 0.0], [1.0, 0.0], [1.0, 2.0], [0.0, 2.0]]
            }),
        )
        .expect("datum cut/fill command should succeed");
        let output = result.output.expect("command returns output");

        assert_eq!(output["comparison"]["datum_y"], json!(1.0));
        assert_eq!(output["sample_count"], json!(2));
        assert_eq!(output["cut_volume"], json!(2.0));
        assert_eq!(output["net_volume"], json!(2.0));
        assert_eq!(output["boundary_vertex_count"], json!(4));
    }

    #[test]
    fn cut_fill_command_requires_comparison_target() {
        let mut world = init_command_test_world();
        world.spawn((ElementId(10), flat_square(2.0)));

        let error = execute_cut_fill_analysis(
            &mut world,
            &json!({
                "existing_surface_id": 10
            }),
        )
        .expect_err("missing comparison should fail");

        assert_eq!(error, "Provide either proposed_surface_id or datum_y");
    }

    #[test]
    fn prepare_site_surface_repairs_fragments_and_creates_surface() {
        let mut world = init_command_test_world();
        world.spawn((
            ElementId(100),
            Selected,
            Polyline {
                points: vec![Vec3::new(0.0, 10.0, 0.0), Vec3::new(5.0, 10.0, 0.0)],
            },
            ElevationMetadata {
                source_layer: "Contour_10".to_string(),
                elevation: 10.0,
                survey_source_id: Some("survey-1".to_string()),
            },
        ));
        world.spawn((
            ElementId(101),
            Selected,
            Polyline {
                points: vec![Vec3::new(6.0, 10.0, 0.0), Vec3::new(10.0, 10.0, 0.0)],
            },
            ElevationMetadata {
                source_layer: "Contour_10".to_string(),
                elevation: 10.0,
                survey_source_id: Some("survey-1".to_string()),
            },
        ));

        let result = execute_prepare_site_surface(
            &mut world,
            &json!({
                "delete_source": true,
                "join_tolerance": 1.5
            }),
        )
        .expect("site surface preparation should succeed");

        assert_eq!(result.created.len(), 2);
        assert_eq!(result.deleted, vec![100, 101]);
        assert_eq!(
            result
                .output
                .as_ref()
                .and_then(|v| v.get("inserted_bridge_count"))
                .and_then(Value::as_u64),
            Some(1)
        );

        let created = world
            .resource_mut::<Messages<CreateEntityCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 2);
        assert_eq!(created[0].snapshot.type_name(), "elevation_curve");
        assert_eq!(created[1].snapshot.type_name(), "terrain_surface");
        let curve_json = created[0].snapshot.to_json();
        assert_eq!(
            curve_json["ElevationCurve"]["curve"]["points"][0],
            json!([-5.0, 10.0, 0.0])
        );
    }

    #[test]
    fn contour_layer_detection_prefers_swedish_height_curve_layers() {
        assert_eq!(
            detect_contour_layer_type("HOJDKURVA"),
            Some(ElevationCurveType::Major)
        );
        assert_eq!(
            detect_contour_layer_type("HOJDKURVA_HALV"),
            Some(ElevationCurveType::Minor)
        );
        assert_eq!(detect_contour_layer_type("BYGGNAD_TAK"), None);
        assert_eq!(detect_contour_layer_type("HYDROGRAFI_STRAND"), None);
    }

    #[test]
    fn prepare_site_surface_skips_non_contour_layers() {
        let mut world = init_command_test_world();
        world.spawn((
            ElementId(100),
            Selected,
            Polyline {
                points: vec![Vec3::new(0.0, 10.0, 0.0), Vec3::new(5.0, 10.0, 0.0)],
            },
            ElevationMetadata {
                source_layer: "HOJDKURVA".to_string(),
                elevation: 10.0,
                survey_source_id: Some("survey-1".to_string()),
            },
        ));
        world.spawn((
            ElementId(101),
            Selected,
            Polyline {
                points: vec![Vec3::new(1.0, 11.0, 1.0), Vec3::new(4.0, 11.0, 1.0)],
            },
            ElevationMetadata {
                source_layer: "BYGGNAD_TAK".to_string(),
                elevation: 11.0,
                survey_source_id: Some("survey-1".to_string()),
            },
        ));

        let result = execute_prepare_site_surface(&mut world, &json!({}))
            .expect("site surface preparation should succeed");

        assert_eq!(
            result
                .output
                .as_ref()
                .and_then(|value| value.get("used_layers"))
                .and_then(Value::as_array)
                .cloned(),
            Some(vec![json!("HOJDKURVA")])
        );
        assert_eq!(
            result
                .output
                .as_ref()
                .and_then(|value| value.get("skipped_layers"))
                .and_then(Value::as_array)
                .cloned(),
            Some(vec![json!("BYGGNAD_TAK")])
        );
    }
}
