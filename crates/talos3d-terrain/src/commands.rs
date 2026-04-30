use bevy::{ecs::world::EntityRef, prelude::*};
use serde_json::Value;
use talos3d_capability_api::commands::{
    CommandCategory, CommandDescriptor, CommandRegistryAppExt, CommandResult,
};
use talos3d_core::plugins::{
    commands::{
        ApplyEntityChangesCommand, BeginCommandGroup, CreateEntityCommand, DeleteEntitiesCommand,
        EndCommandGroup,
    },
    identity::{ElementId, ElementIdAllocator},
    modeling::primitives::{ElevationMetadata, Polyline},
    selection::Selected,
    ui::StatusBarData,
};

use crate::{
    components::{
        ElevationCurve, ElevationCurveType, TerrainMeshCache, TerrainSurface, TerrainSurfaceRole,
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
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.create_proposed_surface".to_string(),
                label: "Create Proposed Surface".to_string(),
                description: "Duplicate an existing terrain surface and its source curves for proposed grading.".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source_surface_id": {"type": "integer", "minimum": 0},
                        "name": {"type": "string"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Create an independently editable proposed terrain surface from an existing surface".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_create_proposed_surface,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.add_elevation_curve".to_string(),
                label: "Add Elevation Curve".to_string(),
                description: "Add an editable elevation curve to an existing terrain surface.".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "required": ["points", "elevation"],
                    "properties": {
                        "surface_id": {"type": "integer", "minimum": 0},
                        "points": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 3,
                                "items": {"type": "number"}
                            }
                        },
                        "elevation": {"type": "number"},
                        "source_layer": {"type": "string"},
                        "curve_type": {"type": "string"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Append a new elevation curve to a terrain surface".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_add_elevation_curve,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.add_spot_elevation".to_string(),
                label: "Add Spot Elevation".to_string(),
                description: "Add a single-point elevation marker to an existing terrain surface.".to_string(),
                category: CommandCategory::Create,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "required": ["point", "elevation"],
                    "properties": {
                        "surface_id": {"type": "integer", "minimum": 0},
                        "point": {
                            "type": "array",
                            "minItems": 2,
                            "maxItems": 3,
                            "items": {"type": "number"}
                        },
                        "elevation": {"type": "number"},
                        "source_layer": {"type": "string"}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.create".to_string()),
                hint: Some("Append a spot elevation to a terrain surface".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_add_spot_elevation,
        )
        .register_command(
            CommandDescriptor {
                id: "terrain.delete_elevation_curve".to_string(),
                label: "Delete Elevation Curve".to_string(),
                description: "Delete an elevation curve and remove it from terrain surface source lists.".to_string(),
                category: CommandCategory::Edit,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "curve_id": {"type": "integer", "minimum": 0}
                    }
                })),
                default_shortcut: None,
                icon: Some("icon.delete".to_string()),
                hint: Some("Delete a terrain source curve and regenerate affected surfaces".to_string()),
                requires_selection: false,
                show_in_menu: true,
                version: 1,
                activates_tool: None,
                capability_id: Some("terrain".to_string()),
            },
            execute_delete_elevation_curve,
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
            role: TerrainSurfaceRole::Existing,
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
            role: TerrainSurfaceRole::Existing,
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

fn execute_create_proposed_surface(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let source_surface_id = optional_element_id(parameters, "source_surface_id")?
        .map_or_else(|| selected_terrain_surface_id(world), Ok)?;
    let source_surface = terrain_surface_by_id(world, source_surface_id)
        .ok_or_else(|| format!("Terrain surface {} was not found", source_surface_id.0))?;

    let mut curve_snapshots = Vec::with_capacity(source_surface.source_curve_ids.len());
    let mut curve_map = Vec::with_capacity(source_surface.source_curve_ids.len());
    {
        let allocator = world.resource::<ElementIdAllocator>();
        for source_curve_id in &source_surface.source_curve_ids {
            let curve = elevation_curve_by_id(world, *source_curve_id).ok_or_else(|| {
                format!(
                    "Source curve {} for terrain surface {} was not found",
                    source_curve_id.0, source_surface_id.0
                )
            })?;
            let proposed_curve_id = allocator.next_id();
            curve_map.push((*source_curve_id, proposed_curve_id));
            curve_snapshots.push(ElevationCurveSnapshot {
                element_id: proposed_curve_id,
                curve,
            });
        }
    }

    let proposed_surface_id = world.resource::<ElementIdAllocator>().next_id();
    let proposed_name = parameters
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Proposed {}", source_surface.name));
    let proposed_surface = TerrainSurfaceSnapshot {
        element_id: proposed_surface_id,
        surface: TerrainSurface {
            name: proposed_name,
            role: TerrainSurfaceRole::Proposed,
            source_curve_ids: curve_map
                .iter()
                .map(|(_, proposed_curve_id)| *proposed_curve_id)
                .collect(),
            ..source_surface
        },
    };

    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup {
            label: "Create proposed terrain surface",
        });

    let mut create_events = world.resource_mut::<Messages<CreateEntityCommand>>();
    for snapshot in &curve_snapshots {
        create_events.write(CreateEntityCommand {
            snapshot: snapshot.clone().into(),
        });
    }
    create_events.write(CreateEntityCommand {
        snapshot: proposed_surface.clone().into(),
    });
    let _ = create_events;

    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(
            format!("Queued proposed terrain surface {}", proposed_surface_id.0),
            2.5,
        );
    }

    Ok(CommandResult {
        created: curve_snapshots
            .iter()
            .map(|snapshot| snapshot.element_id.0)
            .chain(std::iter::once(proposed_surface_id.0))
            .collect(),
        output: Some(serde_json::json!({
            "source_surface_id": source_surface_id.0,
            "proposed_surface_id": proposed_surface_id.0,
            "role": TerrainSurfaceRole::Proposed.as_str(),
            "source_curve_count": curve_map.len(),
            "curve_map": curve_map
                .iter()
                .map(|(source, proposed)| serde_json::json!({
                    "source_curve_id": source.0,
                    "proposed_curve_id": proposed.0,
                }))
                .collect::<Vec<_>>(),
        })),
        ..CommandResult::empty()
    })
}

fn execute_add_elevation_curve(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let elevation = required_scalar(parameters, "elevation")?;
    let points = parse_elevation_points(
        parameters
            .get("points")
            .ok_or_else(|| "Missing required parameter points".to_string())?,
        elevation,
        "points",
    )?;
    if points.is_empty() {
        return Err("points must contain at least one point".to_string());
    }
    let curve_type = parameters
        .get("curve_type")
        .and_then(Value::as_str)
        .map(parse_elevation_curve_type)
        .transpose()?
        .unwrap_or(ElevationCurveType::Supplementary);
    let source_layer = parameters
        .get("source_layer")
        .and_then(Value::as_str)
        .filter(|layer| !layer.trim().is_empty())
        .unwrap_or("Edited Terrain")
        .to_string();

    add_elevation_curve_to_surface(
        world,
        parameters,
        ElevationCurve {
            points,
            elevation,
            source_layer,
            curve_type,
            survey_source_id: None,
        },
        "Add elevation curve",
        "Queued elevation curve",
    )
}

fn execute_add_spot_elevation(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let elevation = required_scalar(parameters, "elevation")?;
    let mut points = parse_elevation_points(
        parameters
            .get("point")
            .ok_or_else(|| "Missing required parameter point".to_string())?,
        elevation,
        "point",
    )?;
    if points.len() != 1 {
        return Err("point must contain exactly one point".to_string());
    }
    let source_layer = parameters
        .get("source_layer")
        .and_then(Value::as_str)
        .filter(|layer| !layer.trim().is_empty())
        .unwrap_or("Spot Elevations")
        .to_string();

    add_elevation_curve_to_surface(
        world,
        parameters,
        ElevationCurve {
            points: points.drain(..).collect(),
            elevation,
            source_layer,
            curve_type: ElevationCurveType::Supplementary,
            survey_source_id: None,
        },
        "Add spot elevation",
        "Queued spot elevation",
    )
}

fn add_elevation_curve_to_surface(
    world: &mut World,
    parameters: &Value,
    curve: ElevationCurve,
    group_label: &'static str,
    feedback_label: &'static str,
) -> Result<CommandResult, String> {
    let surface_id = optional_element_id(parameters, "surface_id")?
        .map_or_else(|| selected_terrain_surface_id(world), Ok)?;
    let source_surface = terrain_surface_by_id(world, surface_id)
        .ok_or_else(|| format!("Terrain surface {} was not found", surface_id.0))?;
    let curve_id = world.resource::<ElementIdAllocator>().next_id();
    let mut updated_surface = source_surface.clone();
    updated_surface.source_curve_ids.push(curve_id);

    let before_surface = TerrainSurfaceSnapshot {
        element_id: surface_id,
        surface: source_surface,
    };
    let after_surface = TerrainSurfaceSnapshot {
        element_id: surface_id,
        surface: updated_surface,
    };
    let curve_snapshot = ElevationCurveSnapshot {
        element_id: curve_id,
        curve,
    };

    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup { label: group_label });

    world
        .resource_mut::<Messages<CreateEntityCommand>>()
        .write(CreateEntityCommand {
            snapshot: curve_snapshot.clone().into(),
        });
    world
        .resource_mut::<Messages<ApplyEntityChangesCommand>>()
        .write(ApplyEntityChangesCommand {
            label: "Update terrain surface sources",
            before: vec![before_surface.into()],
            after: vec![after_surface.into()],
        });
    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(format!("{feedback_label} {}", curve_id.0), 2.0);
    }

    Ok(CommandResult {
        created: vec![curve_id.0],
        output: Some(serde_json::json!({
            "surface_id": surface_id.0,
            "elevation_curve_id": curve_id.0,
            "point_count": curve_snapshot.curve.points.len(),
            "elevation": curve_snapshot.curve.elevation,
        })),
        ..CommandResult::empty()
    })
}

fn execute_delete_elevation_curve(
    world: &mut World,
    parameters: &Value,
) -> Result<CommandResult, String> {
    let curve_id = optional_element_id(parameters, "curve_id")?
        .map_or_else(|| selected_elevation_curve_id(world), Ok)?;
    if elevation_curve_by_id(world, curve_id).is_none() {
        return Err(format!("Elevation curve {} was not found", curve_id.0));
    }

    let mut before_surfaces = Vec::new();
    let mut after_surfaces = Vec::new();
    let mut affected_surface_ids = Vec::new();
    let mut q = world.try_query::<EntityRef>().unwrap();
    for entity_ref in q.iter(world) {
        let (Some(surface_id), Some(surface)) = (
            entity_ref.get::<ElementId>().copied(),
            entity_ref.get::<TerrainSurface>(),
        ) else {
            continue;
        };
        if !surface.source_curve_ids.contains(&curve_id) {
            continue;
        }
        let mut updated_surface = surface.clone();
        updated_surface
            .source_curve_ids
            .retain(|source_curve_id| *source_curve_id != curve_id);
        before_surfaces.push(
            TerrainSurfaceSnapshot {
                element_id: surface_id,
                surface: surface.clone(),
            }
            .into(),
        );
        after_surfaces.push(
            TerrainSurfaceSnapshot {
                element_id: surface_id,
                surface: updated_surface,
            }
            .into(),
        );
        affected_surface_ids.push(surface_id.0);
    }

    world
        .resource_mut::<Messages<BeginCommandGroup>>()
        .write(BeginCommandGroup {
            label: "Delete elevation curve",
        });
    if !before_surfaces.is_empty() {
        world
            .resource_mut::<Messages<ApplyEntityChangesCommand>>()
            .write(ApplyEntityChangesCommand {
                label: "Update terrain surface sources",
                before: before_surfaces,
                after: after_surfaces,
            });
    }
    world
        .resource_mut::<Messages<DeleteEntitiesCommand>>()
        .write(DeleteEntitiesCommand {
            element_ids: vec![curve_id],
        });
    world
        .resource_mut::<Messages<EndCommandGroup>>()
        .write(EndCommandGroup);

    if let Some(mut status_bar_data) = world.get_resource_mut::<StatusBarData>() {
        status_bar_data.set_feedback(format!("Queued elevation curve {}", curve_id.0), 2.0);
    }

    Ok(CommandResult {
        deleted: vec![curve_id.0],
        output: Some(serde_json::json!({
            "curve_id": curve_id.0,
            "removed_from_surface_ids": affected_surface_ids,
        })),
        ..CommandResult::empty()
    })
}

fn required_element_id(parameters: &Value, key: &str) -> Result<ElementId, String> {
    optional_element_id(parameters, key)?.ok_or_else(|| format!("Missing required parameter {key}"))
}

fn required_scalar(parameters: &Value, key: &str) -> Result<f32, String> {
    parameters
        .get(key)
        .and_then(Value::as_f64)
        .map(|value| value as f32)
        .ok_or_else(|| format!("Missing required numeric parameter {key}"))
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

fn parse_elevation_points(value: &Value, elevation: f32, key: &str) -> Result<Vec<Vec3>, String> {
    let point_values = if let Some(points) = value.as_array() {
        if points.first().is_some_and(Value::is_number) {
            vec![value]
        } else {
            points.iter().collect::<Vec<_>>()
        }
    } else {
        return Err(format!("{key} must be a point array"));
    };

    point_values
        .into_iter()
        .map(|point| {
            let Some(coords) = point.as_array() else {
                return Err(format!("{key} entries must be [x, z] or [x, y, z] arrays"));
            };
            if !(coords.len() == 2 || coords.len() == 3) {
                return Err(format!(
                    "{key} entries must contain either two or three numbers"
                ));
            }
            let x = coords[0]
                .as_f64()
                .ok_or_else(|| format!("{key} x coordinate must be a number"))?
                as f32;
            let z_index = if coords.len() == 2 { 1 } else { 2 };
            let z = coords[z_index]
                .as_f64()
                .ok_or_else(|| format!("{key} z coordinate must be a number"))?
                as f32;
            Ok(Vec3::new(x, elevation, z))
        })
        .collect()
}

fn parse_elevation_curve_type(value: &str) -> Result<ElevationCurveType, String> {
    match value {
        "major" => Ok(ElevationCurveType::Major),
        "minor" => Ok(ElevationCurveType::Minor),
        "index" => Ok(ElevationCurveType::Index),
        "supplementary" => Ok(ElevationCurveType::Supplementary),
        _ => Err("curve_type must be one of: major, minor, index, supplementary".to_string()),
    }
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

fn selected_terrain_surface_id(world: &World) -> Result<ElementId, String> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .find_map(|entity_ref| {
            if !entity_ref.contains::<Selected>() || !entity_ref.contains::<TerrainSurface>() {
                return None;
            }
            entity_ref.get::<ElementId>().copied()
        })
        .ok_or_else(|| "Select a terrain surface or provide source_surface_id".to_string())
}

fn selected_elevation_curve_id(world: &World) -> Result<ElementId, String> {
    let mut q = world.try_query::<EntityRef>().unwrap();
    q.iter(world)
        .find_map(|entity_ref| {
            if !entity_ref.contains::<Selected>() || !entity_ref.contains::<ElevationCurve>() {
                return None;
            }
            entity_ref.get::<ElementId>().copied()
        })
        .ok_or_else(|| "Select an elevation curve or provide curve_id".to_string())
}

fn terrain_surface_by_id(world: &World, element_id: ElementId) -> Option<TerrainSurface> {
    let mut q = world.try_query::<EntityRef>()?;
    q.iter(world).find_map(|entity_ref| {
        if entity_ref.get::<ElementId>() != Some(&element_id) {
            return None;
        }
        entity_ref.get::<TerrainSurface>().cloned()
    })
}

fn elevation_curve_by_id(world: &World, element_id: ElementId) -> Option<ElevationCurve> {
    let mut q = world.try_query::<EntityRef>()?;
    q.iter(world).find_map(|entity_ref| {
        if entity_ref.get::<ElementId>() != Some(&element_id) {
            return None;
        }
        entity_ref.get::<ElevationCurve>().cloned()
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
        world.insert_resource(Messages::<ApplyEntityChangesCommand>::default());
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

    fn sample_curve(elevation: f32) -> ElevationCurve {
        ElevationCurve {
            points: vec![
                Vec3::new(0.0, elevation, 0.0),
                Vec3::new(1.0, elevation, 0.0),
            ],
            elevation,
            source_layer: "Contour".to_string(),
            curve_type: ElevationCurveType::Major,
            survey_source_id: None,
        }
    }

    #[test]
    fn proposed_surface_command_duplicates_surface_and_source_curves() {
        let mut world = init_command_test_world();
        world.resource_mut::<ElementIdAllocator>().set_next(1000);
        world.spawn((ElementId(10), sample_curve(1.0)));
        world.spawn((ElementId(11), sample_curve(2.0)));
        world.spawn((
            ElementId(20),
            TerrainSurface {
                name: "Existing Site".to_string(),
                source_curve_ids: vec![ElementId(10), ElementId(11)],
                role: TerrainSurfaceRole::Existing,
                datum_elevation: 1.0,
                boundary: vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(2.0, 0.0),
                    Vec2::new(2.0, 2.0),
                    Vec2::new(0.0, 2.0),
                ],
                max_triangle_area: 5.0,
                minimum_angle: 12.0,
                contour_interval: 0.5,
                drape_sample_spacing: 0.25,
                offset: Vec3::new(1.0, 0.0, 2.0),
            },
        ));

        let result = execute_create_proposed_surface(
            &mut world,
            &json!({
                "source_surface_id": 20,
                "name": "Proposed Site"
            }),
        )
        .expect("proposed surface command should succeed");

        assert_eq!(result.created, vec![1000, 1001, 1002]);
        assert_eq!(
            result.output.as_ref().unwrap()["source_curve_count"],
            json!(2)
        );
        assert_eq!(result.output.as_ref().unwrap()["role"], json!("proposed"));
        let created = world
            .resource_mut::<Messages<CreateEntityCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 3);
        assert_eq!(created[0].snapshot.type_name(), "elevation_curve");
        assert_eq!(created[1].snapshot.type_name(), "elevation_curve");
        assert_eq!(created[2].snapshot.type_name(), "terrain_surface");

        let surface_json = created[2].snapshot.to_json();
        assert_eq!(
            surface_json["TerrainSurface"]["surface"]["name"],
            json!("Proposed Site")
        );
        assert_eq!(
            surface_json["TerrainSurface"]["surface"]["source_curve_ids"],
            json!([1000, 1001])
        );
        assert_eq!(
            surface_json["TerrainSurface"]["surface"]["role"],
            json!("proposed")
        );
        assert_eq!(
            surface_json["TerrainSurface"]["surface"]["drape_sample_spacing"],
            json!(0.25)
        );
    }

    #[test]
    fn proposed_surface_command_uses_selected_surface_when_id_is_omitted() {
        let mut world = init_command_test_world();
        world.resource_mut::<ElementIdAllocator>().set_next(2000);
        world.spawn((ElementId(10), sample_curve(1.0)));
        world.spawn((
            ElementId(20),
            Selected,
            TerrainSurface::new("Existing Site".to_string(), vec![ElementId(10)]),
        ));

        let result = execute_create_proposed_surface(&mut world, &json!({}))
            .expect("selected terrain surface should be used");

        assert_eq!(result.created, vec![2000, 2001]);
        assert_eq!(
            result.output.as_ref().unwrap()["source_surface_id"],
            json!(20)
        );
    }

    #[test]
    fn add_elevation_curve_command_creates_curve_and_updates_surface_sources() {
        let mut world = init_command_test_world();
        world.resource_mut::<ElementIdAllocator>().set_next(3000);
        world.spawn((
            ElementId(20),
            TerrainSurface::new("Existing Site".to_string(), vec![ElementId(10)]),
        ));

        let result = execute_add_elevation_curve(
            &mut world,
            &json!({
                "surface_id": 20,
                "points": [[0.0, 0.0], [2.0, 2.0]],
                "elevation": 4.5,
                "source_layer": "Design Contours",
                "curve_type": "minor"
            }),
        )
        .expect("add elevation curve should succeed");

        assert_eq!(result.created, vec![3000]);
        assert_eq!(result.output.as_ref().unwrap()["point_count"], json!(2));
        let created = world
            .resource_mut::<Messages<CreateEntityCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1);
        let curve_json = created[0].snapshot.to_json();
        assert_eq!(curve_json["ElevationCurve"]["element_id"], json!(3000));
        assert_eq!(
            curve_json["ElevationCurve"]["curve"]["points"],
            json!([[0.0, 4.5, 0.0], [2.0, 4.5, 2.0]])
        );
        assert_eq!(
            curve_json["ElevationCurve"]["curve"]["curve_type"],
            json!("Minor")
        );

        let changes = world
            .resource_mut::<Messages<ApplyEntityChangesCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(changes.len(), 1);
        let after_json = changes[0].after[0].to_json();
        assert_eq!(
            after_json["TerrainSurface"]["surface"]["source_curve_ids"],
            json!([10, 3000])
        );
    }

    #[test]
    fn add_spot_elevation_command_appends_single_point_curve() {
        let mut world = init_command_test_world();
        world.resource_mut::<ElementIdAllocator>().set_next(4000);
        world.spawn((
            ElementId(20),
            Selected,
            TerrainSurface::new("Existing Site".to_string(), vec![]),
        ));

        let result = execute_add_spot_elevation(
            &mut world,
            &json!({
                "point": [3.0, 7.0],
                "elevation": 12.25
            }),
        )
        .expect("add spot elevation should succeed");

        assert_eq!(result.created, vec![4000]);
        let created = world
            .resource_mut::<Messages<CreateEntityCommand>>()
            .drain()
            .collect::<Vec<_>>();
        let curve_json = created[0].snapshot.to_json();
        assert_eq!(
            curve_json["ElevationCurve"]["curve"]["points"],
            json!([[3.0, 12.25, 7.0]])
        );
        assert_eq!(
            curve_json["ElevationCurve"]["curve"]["source_layer"],
            json!("Spot Elevations")
        );
    }

    #[test]
    fn delete_elevation_curve_command_prunes_surface_references_before_delete() {
        let mut world = init_command_test_world();
        world.spawn((ElementId(10), sample_curve(1.0)));
        world.spawn((ElementId(11), sample_curve(2.0)));
        world.spawn((
            ElementId(20),
            TerrainSurface::new(
                "Existing Site".to_string(),
                vec![ElementId(10), ElementId(11)],
            ),
        ));
        world.spawn((
            ElementId(21),
            TerrainSurface::new("Proposed Site".to_string(), vec![ElementId(10)]),
        ));

        let result = execute_delete_elevation_curve(&mut world, &json!({ "curve_id": 10 }))
            .expect("delete elevation curve should succeed");

        assert_eq!(result.deleted, vec![10]);
        assert_eq!(
            result.output.as_ref().unwrap()["removed_from_surface_ids"],
            json!([20, 21])
        );
        let changes = world
            .resource_mut::<Messages<ApplyEntityChangesCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before.len(), 2);
        let after_source_lists = changes[0]
            .after
            .iter()
            .map(|snapshot| {
                snapshot.to_json()["TerrainSurface"]["surface"]["source_curve_ids"].clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(after_source_lists, vec![json!([11]), json!([])]);

        let deletes = world
            .resource_mut::<Messages<DeleteEntitiesCommand>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].element_ids, vec![ElementId(10)]);
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
        let surface_json = created[1].snapshot.to_json();
        assert_eq!(
            surface_json["TerrainSurface"]["surface"]["role"],
            json!("existing")
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
