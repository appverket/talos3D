use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cadio_ir::{
    Arc, Block, CadDocument, CadFormat, CadVersion, Circle, Entity, EntityCommon, Face3D, Insert,
    Layer, Line, Point3, Polyline,
};

use crate::{
    read_classes, read_handle_map, read_section_data, DwgClassSummary, DwgObjectRecordSummary,
    DwgReadError, DwgRecordProbe, DwgSemanticProbe,
};

const PAPER_SPACE_NAME: &str = "*PAPER_SPACE";
const NO_OWNER_MODE: u8 = 0;
const PAPER_SPACE_MODE: u8 = 1;
const MODEL_SPACE_MODE: u8 = 2;
const BLOCK_HEADER_NEIGHBOR_WINDOW: u64 = 512;
const LAYER_NEIGHBOR_WINDOW: u64 = 32;
#[cfg(test)]
const INSERT_NEARBY_START_DELTA: isize = -32;
#[cfg(test)]
const INSERT_NEARBY_END_DELTA: isize = 24;
const MAX_OWNED_HANDLES: usize = 16_384;
const MAX_INSERT_HANDLES: usize = 16_384;
const MAX_INSERT_ARRAY_INSTANCES: usize = 1_024;
const MAX_PREVIEW_BYTES: usize = 1 << 20;
const MAX_POLYLINE_POINTS: usize = 100_000;
const MAX_REACTORS: usize = 8_192;
const MAX_TABLE_TEXT_STREAM_SEARCH_BITS: usize = 16_384;
const MAX_XDATA_SEGMENTS: usize = 8_192;
const MAX_REASONABLE_COORDINATE_ABS: f64 = 1.0e9;
const MAX_REASONABLE_HANDLE: u64 = 0x10_0000;
const MIN_REASONABLE_EXTENT: f64 = 1.0e-6;
const DEBUG_DIRECT_OFFSET_DECODE: bool = false;
const DEBUG_COMMON_TABLE_NAME_FAILURES: bool = false;

pub(crate) fn read_document(path: &Path) -> Result<CadDocument, DwgReadError> {
    let probe = crate::probe_file(path)?;
    let handle_map = read_handle_map(path)?;
    let object_index = crate::read_object_index(path).unwrap_or_default();
    let classes = read_classes(path).unwrap_or_default();
    let objects = read_section_data(path, "Objects")?;
    let type_hints = object_type_hints(&object_index);
    let decoded = decode_supported_objects(
        probe.version,
        &handle_map,
        &object_index,
        &objects,
        &classes,
        &type_hints,
    );
    let decoded_sequence = decoded.clone();

    let mut layers_by_handle = BTreeMap::<u64, Layer>::new();
    let mut blocks_by_handle = BTreeMap::<u64, DecodedBlockHeader>::new();
    let mut entities = Vec::<DecodedEntity>::new();
    let mut vertices = Vec::<DecodedVertex>::new();

    for object in decoded {
        match object {
            SupportedObject::Layer(layer) => {
                if !targeted_layer_is_reasonable(&layer) {
                    continue;
                }
                layers_by_handle.insert(
                    layer.handle,
                    Layer {
                        name: layer.name,
                        visible: layer.visible,
                    },
                );
            }
            SupportedObject::BlockHeader(block) => {
                if !block_header_has_structural_signal(&block, &type_hints) {
                    continue;
                }
                blocks_by_handle.insert(block.handle, block);
            }
            SupportedObject::Entity(entity) => entities.push(entity),
            SupportedObject::Vertex(vertex) => vertices.push(vertex),
            SupportedObject::SeqEnd => {}
            SupportedObject::Ignored => {}
        }
    }
    recover_referenced_layers(
        probe.version,
        &handle_map,
        &object_index,
        &objects,
        &classes,
        &type_hints,
        &entities,
        &mut layers_by_handle,
    );
    // recover_referenced_block_headers disabled: handle stream is broken for
    // R2007+, so entity block/owner references are garbage.  Using only the
    // type-hint path avoids false-positive blocks.
    recover_hinted_block_headers(
        probe.version,
        &handle_map,
        &object_index,
        &objects,
        &classes,
        &type_hints,
        &mut blocks_by_handle,
    );
    recover_owned_polyline_vertices(
        probe.version,
        &handle_map,
        &object_index,
        &objects,
        &decoded_sequence,
        &mut vertices,
    );

    let polyline_vertices = collect_polyline_vertices(&decoded_sequence, &vertices, &type_hints);
    let block_handle_lookup = build_block_handle_lookup(&blocks_by_handle);
    let mut block_entities = BTreeMap::<u64, Vec<Entity>>::new();
    let mut document_entities = Vec::new();

    for entity in entities {
        let owner_handle = entity.common.owner_handle;
        let entity_mode = entity.common.entity_mode;
        let Some(cad_entity) = entity.into_cad_entity(
            &layers_by_handle,
            &polyline_vertices,
            &blocks_by_handle,
            &block_handle_lookup,
        ) else {
            continue;
        };
        if !entity_is_reasonable(&cad_entity) {
            continue;
        }
        match owner_handle.and_then(|handle| block_handle_lookup.get(&handle).copied()) {
            Some(block_handle)
                if blocks_by_handle
                    .get(&block_handle)
                    .is_some_and(|block| block.is_paper_space) =>
            {
                if entity_mode != PAPER_SPACE_MODE {
                    document_entities.push(cad_entity);
                }
            }
            Some(block_handle)
                if entity_mode == NO_OWNER_MODE || entity_mode == PAPER_SPACE_MODE =>
            {
                block_entities
                    .entry(block_handle)
                    .or_default()
                    .push(cad_entity);
            }
            Some(_) | None => document_entities.push(cad_entity),
        }
    }

    // Synthesize polylines from orphaned vertex groups (vertices whose owner
    // polyline headers were not decoded, e.g. in zero-padded object regions).
    if std::env::var_os("DWG_PROFILE").is_some() {
        eprintln!(
            "dwg decode: polyline_vertices groups={} total_points={}",
            polyline_vertices.len(),
            polyline_vertices.values().map(|v| v.len()).sum::<usize>()
        );
    }
    let used_polyline_handles: std::collections::BTreeSet<u64> =
        entities_consumed_handles(&decoded_sequence);
    for (owner_handle, points) in &polyline_vertices {
        if used_polyline_handles.contains(owner_handle) || points.len() < 2 {
            continue;
        }
        let polyline = Entity::Polyline(Polyline {
            common: EntityCommon {
                handle: Some(format!("{:X}", owner_handle)),
                layer: None,
            },
            points: points.clone(),
            closed: false,
        });
        if entity_is_reasonable(&polyline) {
            document_entities.push(polyline);
        }
    }

    let mut blocks = Vec::new();
    for block in blocks_by_handle.values() {
        if block.is_paper_space || block.is_xref {
            continue;
        }
        blocks.push(Block {
            name: block.name.clone(),
            base_point: block.base_point,
            entities: block_entities.remove(&block.handle).unwrap_or_default(),
        });
    }

    let mut layers = layers_by_handle.into_values().collect::<Vec<_>>();
    layers.sort_by(|left, right| left.name.cmp(&right.name));
    blocks.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(CadDocument {
        format: CadFormat::Dwg,
        version: probe.version,
        units: None,
        extents: None,
        layers,
        blocks,
        entities: document_entities,
    })
}

pub(crate) fn probe_record(
    path: &Path,
    handle: u64,
) -> Result<Option<DwgRecordProbe>, DwgReadError> {
    let version = crate::probe_file(path)?.version;
    let classes = read_classes(path).unwrap_or_default();
    let handle_map = read_handle_map(path)?;
    let object_index = crate::read_object_index(path).unwrap_or_default();
    let objects = read_section_data(path, "Objects")?;
    let raw_offset_bytes = match handle_map
        .get(&handle)
        .and_then(|offset| u64::try_from(*offset).ok())
    {
        Some(offset) => offset,
        None => return Ok(None),
    };
    let raw_offset_bits = raw_offset_bytes.saturating_mul(8);
    let handles = sorted_handle_offsets(&handle_map, &object_index);
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(&handle_map, &object_index, handle)
    else {
        return Ok(None);
    };
    let Some(candidate) = direct_object_start_for_handle(
        version,
        &objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    ) else {
        return Ok(None);
    };

    let msb_declared_handle = declared_handle_after_object_type(
        &objects,
        candidate.object_stream_start_bits,
        ObjectBitOrder::Msb,
    );
    let lsb_declared_handle = declared_handle_after_object_type(
        &objects,
        candidate.object_stream_start_bits,
        ObjectBitOrder::Lsb,
    );
    let self_handle_match_end_bits = search_self_handle_lsb(
        &objects,
        candidate._record_start_bits,
        candidate
            .object_stream_start_bits
            .saturating_add(candidate.size_bytes.saturating_mul(8))
            .saturating_add(64),
        handle,
    )
    .and_then(|value| u64::try_from(value).ok());
    let self_handle_match_end_bits_msb = search_self_handle_msb(
        &objects,
        candidate._record_start_bits,
        candidate
            .object_stream_start_bits
            .saturating_add(candidate.size_bytes.saturating_mul(8))
            .saturating_add(64),
        handle,
    )
    .and_then(|value| u64::try_from(value).ok());
    let object_end_bits = candidate
        .object_stream_start_bits
        .saturating_add(candidate.size_bytes.saturating_mul(8));
    let mut object = LsbBitStream::new(&objects, candidate.object_stream_start_bits);
    let _object_type = object.read_object_type().unwrap_or(candidate.object_type);
    let _object_data_end_bits = if is_r2004_plus(version) {
        object
            .read_bit_long()
            .and_then(|bits| usize::try_from(bits).ok())
            .and_then(|bits| object.bit_index.checked_add(bits))
            .filter(|bits| *bits >= object.bit_index && *bits <= object_end_bits)
    } else {
        None
    };
    let legacy_handle_section_offset_bits =
        object_end_bits.saturating_sub(usize::try_from(candidate.handle_stream_bits).unwrap_or(0));
    let bounded_handle_section_offset_bits = candidate
        .object_data_end_bits
        .unwrap_or(object_end_bits)
        .saturating_sub(usize::try_from(candidate.handle_stream_bits).unwrap_or(0));
    let handle_section_offset_bits = if candidate.body_start_bits.is_some() {
        bounded_handle_section_offset_bits
    } else {
        legacy_handle_section_offset_bits
    };

    Ok(Some(DwgRecordProbe {
        handle,
        raw_offset_bits,
        candidate_offset_bits: u64::try_from(candidate._record_start_bits).unwrap_or(0),
        object_stream_start_bits: u64::try_from(candidate.object_stream_start_bits).unwrap_or(0),
        handle_section_offset_bits: u64::try_from(handle_section_offset_bits).unwrap_or(0),
        object_type_name: object_type_name(candidate.object_type, &classes),
        self_handle_match_end_bits,
        self_handle_match_end_bits_msb,
        msb_declared_handle,
        lsb_declared_handle,
        object_data_bits: candidate
            .object_data_end_bits
            .and_then(|end_bits| end_bits.checked_sub(candidate.object_stream_start_bits))
            .and_then(|bits| u32::try_from(bits).ok()),
        handle_stream_bits: Some(candidate.handle_stream_bits),
    }))
}

pub(crate) fn probe_semantic_record(
    path: &Path,
    handle: u64,
) -> Result<Option<DwgSemanticProbe>, DwgReadError> {
    let probe = crate::probe_file(path)?;
    let classes = read_classes(path).unwrap_or_default();
    let handle_map = read_handle_map(path)?;
    let object_index = crate::read_object_index(path).unwrap_or_default();
    let type_hints = object_type_hints(&object_index);
    let objects = read_section_data(path, "Objects")?;
    let handles = sorted_handle_offsets(&handle_map, &object_index);
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(&handle_map, &object_index, handle)
    else {
        return Ok(None);
    };
    let Some(candidate) = direct_object_start_for_handle(
        probe.version,
        &objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    ) else {
        return Ok(None);
    };
    let mut ctx = ObjectDecodeContext::new(probe.version, &objects, handle, candidate);
    let object_type = ctx
        .object_type()
        .or_else(|| read_object_type_at(&objects, candidate.object_stream_start_bits));
    let object_type_name = preferred_object_type_name(handle, object_type, &classes, &type_hints);
    let mut failure_detail = None::<String>;
    let decoded = match object_type_name.as_deref() {
        Some("LAYER") => match read_common_table_data_detailed(&mut ctx) {
            Ok(common) => {
                let values = if is_r2004_plus(ctx.version) {
                    match ctx.object.read_bit_short() {
                        Some(values) => values,
                        None => {
                            failure_detail = Some("failed to read layer flags".to_string());
                            return Ok(Some(DwgSemanticProbe {
                                handle,
                                object_type_name,
                                decoded_kind: None,
                                detail: failure_detail,
                            }));
                        }
                    }
                } else {
                    let frozen = u16::from(ctx.object.read_bit().unwrap_or(false));
                    let off = u16::from(ctx.object.read_bit().unwrap_or(false));
                    let frozen_in_new = u16::from(ctx.object.read_bit().unwrap_or(false));
                    let locked = u16::from(ctx.object.read_bit().unwrap_or(false));
                    match i16::try_from(frozen | (off << 1) | (frozen_in_new << 2) | (locked << 3))
                        .ok()
                    {
                        Some(values) => values,
                        None => {
                            failure_detail =
                                Some("failed to derive legacy layer flags".to_string());
                            return Ok(Some(DwgSemanticProbe {
                                handle,
                                object_type_name,
                                decoded_kind: None,
                                detail: failure_detail,
                            }));
                        }
                    }
                };
                Some(SupportedObject::Layer(DecodedLayer {
                    handle: common.handle,
                    name: common.name,
                    visible: (values & 0b10) == 0,
                }))
            }
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("BLOCK_HEADER") => match read_common_table_data_detailed(&mut ctx) {
            Ok(common) => {
                let _anonymous = ctx.object.read_bit().unwrap_or(false);
                let _has_attributes = ctx.object.read_bit().unwrap_or(false);
                let blk_is_xref = ctx.object.read_bit().unwrap_or(false);
                let is_xref_overlay = ctx.object.read_bit().unwrap_or(false);
                if matches!(
                    ctx.version,
                    CadVersion::Acad2000
                        | CadVersion::Acad2004
                        | CadVersion::Acad2007
                        | CadVersion::Acad2010
                        | CadVersion::Acad2013
                        | CadVersion::Acad2018
                ) {
                    let _xref_loaded = ctx.object.read_bit().unwrap_or(false);
                }
                let owned_count = if is_r2004_plus(ctx.version) && !blk_is_xref && !is_xref_overlay
                {
                    ctx.object
                        .read_bit_long()
                        .and_then(|count| usize::try_from(count).ok())
                        .unwrap_or(0)
                        .min(MAX_OWNED_HANDLES)
                } else {
                    0
                };
                let Some(base_point) = ctx.object.read_3bit_double() else {
                    failure_detail = Some("failed to read block base point".to_string());
                    return Ok(Some(DwgSemanticProbe {
                        handle,
                        object_type_name,
                        decoded_kind: None,
                        detail: failure_detail,
                    }));
                };
                let _xref_path = ctx.text.read_variable_text(ctx.version).unwrap_or_default();
                let mut insert_count = 0usize;
                while ctx.object.read_byte().is_some_and(|value| value != 0) {
                    insert_count += 1;
                    if insert_count >= MAX_INSERT_HANDLES {
                        break;
                    }
                }
                let _comments = ctx.text.read_variable_text(ctx.version).unwrap_or_default();
                let preview_size = ctx
                    .object
                    .read_bit_long()
                    .and_then(|size| usize::try_from(size).ok())
                    .unwrap_or(0);
                let _ = ctx
                    .object
                    .advance_bytes(preview_size.min(MAX_PREVIEW_BYTES));
                let _units = ctx.object.read_bit_short().unwrap_or_default();
                let _explodable = ctx.object.read_bit().unwrap_or(false);
                let _can_scale = ctx.object.read_byte().unwrap_or_default();
                let begin_block_handle = ctx.handles.read_handle_reference(common.handle);
                if !is_r2004_plus(ctx.version) && !blk_is_xref && !is_xref_overlay {
                    let _first_entity = ctx.handles.read_handle_reference(0);
                    let _last_entity = ctx.handles.read_handle_reference(0);
                }
                for _ in 0..owned_count {
                    let _owned = ctx.handles.read_handle_reference(0);
                }
                let end_block_handle = ctx.handles.read_handle_reference(0);
                for _ in 0..insert_count {
                    let _insert = ctx.handles.read_handle_reference(0);
                }
                let _layout = ctx.handles.read_handle_reference(0);
                let normalized = common.name.to_ascii_uppercase();
                Some(SupportedObject::BlockHeader(DecodedBlockHeader {
                    handle: common.handle,
                    begin_block_handle,
                    end_block_handle,
                    name: common.name,
                    base_point,
                    // xref flags are unreliable due to object stream bit alignment;
                    // force false — real xrefs would fail other structural checks
                    is_xref: false,
                    is_paper_space: normalized == PAPER_SPACE_NAME,
                }))
            }
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("LINE") => decode_line(&mut ctx).map(SupportedObject::Entity),
        Some("ARC") => decode_arc(&mut ctx).map(SupportedObject::Entity),
        Some("CIRCLE") => decode_circle(&mut ctx).map(SupportedObject::Entity),
        Some("3DFACE") => decode_face3d(&mut ctx).map(SupportedObject::Entity),
        Some("INSERT") => match decode_insert_detailed(&mut ctx, false) {
            Ok(entity) => Some(SupportedObject::Entity(entity)),
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("MINSERT") => match decode_insert_detailed(&mut ctx, true) {
            Ok(entity) => Some(SupportedObject::Entity(entity)),
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("LWPOLYLINE") => decode_lwpolyline(&mut ctx).map(SupportedObject::Entity),
        Some("POLYLINE_2D") => decode_polyline2d(&mut ctx).map(SupportedObject::Entity),
        Some("POLYLINE_3D") => match decode_polyline3d_detailed(&mut ctx) {
            Ok(entity) => Some(SupportedObject::Entity(entity)),
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("VERTEX_2D") => match decode_vertex2d_detailed(&mut ctx) {
            Ok(vertex) => Some(SupportedObject::Vertex(vertex)),
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("VERTEX_3D")
        | Some("VERTEX_MESH")
        | Some("VERTEX_PFACE")
        | Some("VERTEX_PFACE_FACE") => match decode_vertex3d_detailed(&mut ctx) {
            Ok(vertex) => Some(SupportedObject::Vertex(vertex)),
            Err(stage) => {
                failure_detail = Some(stage.to_string());
                None
            }
        },
        Some("SEQEND") => Some(SupportedObject::SeqEnd),
        _ => Some(SupportedObject::Ignored),
    };
    let (decoded_kind, detail) = match decoded {
        Some(SupportedObject::Layer(layer)) => {
            (Some("Layer".to_string()), Some(format!("{layer:?}")))
        }
        Some(SupportedObject::BlockHeader(block)) => {
            (Some("BlockHeader".to_string()), Some(format!("{block:?}")))
        }
        Some(SupportedObject::Entity(entity)) => {
            (Some("Entity".to_string()), Some(format!("{entity:?}")))
        }
        Some(SupportedObject::Vertex(vertex)) => {
            (Some("Vertex".to_string()), Some(format!("{vertex:?}")))
        }
        Some(SupportedObject::SeqEnd) => (Some("SeqEnd".to_string()), None),
        Some(SupportedObject::Ignored) => (Some("Ignored".to_string()), None),
        None => (None, failure_detail),
    };
    let (decoded_kind, detail) = if decoded_kind.is_none()
        && object_type_name
            .as_deref()
            .is_none_or(is_geometry_object_type)
    {
        match attempt_geometry_decode(
            probe.version,
            &objects,
            handle,
            candidate,
            object_type_name.as_deref(),
            &type_hints,
        ) {
            Some(SupportedObject::Entity(entity)) => (
                Some("Entity".to_string()),
                Some(format!("fallback decode: {entity:?}")),
            ),
            Some(SupportedObject::Vertex(vertex)) => (
                Some("Vertex".to_string()),
                Some(format!("fallback decode: {vertex:?}")),
            ),
            Some(SupportedObject::SeqEnd) => (Some("SeqEnd".to_string()), detail),
            Some(SupportedObject::Ignored) => (Some("Ignored".to_string()), detail),
            _ => (decoded_kind, detail),
        }
    } else {
        (decoded_kind, detail)
    };
    Ok(Some(DwgSemanticProbe {
        handle,
        object_type_name,
        decoded_kind,
        detail,
    }))
}

#[derive(Debug, Clone)]
enum SupportedObject {
    Layer(DecodedLayer),
    BlockHeader(DecodedBlockHeader),
    Entity(DecodedEntity),
    Vertex(DecodedVertex),
    SeqEnd,
    Ignored,
}

#[derive(Debug, Clone)]
struct DecodedLayer {
    handle: u64,
    name: String,
    visible: bool,
}

#[derive(Debug, Clone)]
struct DecodedCommonTableData {
    handle: u64,
    name: String,
}

#[derive(Debug, Clone)]
struct DecodedBlockHeader {
    handle: u64,
    begin_block_handle: Option<u64>,
    end_block_handle: Option<u64>,
    name: String,
    base_point: Point3,
    is_xref: bool,
    is_paper_space: bool,
}

#[derive(Debug, Clone)]
struct DecodedEntityCommon {
    handle: u64,
    owner_handle: Option<u64>,
    layer_handle: Option<u64>,
    alternate_layer_handle: Option<u64>,
    entity_mode: u8,
}

#[derive(Clone, Copy)]
struct DecodedEntityPrefix {
    handle: u64,
    entity_mode: u8,
    reactor_count: usize,
    xdict_missing: bool,
    nolinks: bool,
    line_type_flags: u8,
    plotstyle_flags: u8,
    material_flags: u8,
    shadow_flags: u8,
    has_full_visual_style: bool,
    has_face_visual_style: bool,
    has_edge_visual_style: bool,
}

#[derive(Debug, Clone)]
struct DecodedEntity {
    common: DecodedEntityCommon,
    kind: DecodedEntityKind,
}

#[derive(Debug, Clone)]
enum DecodedEntityKind {
    Line(Line),
    Arc(Arc),
    Circle(Circle),
    Face3D(Face3D),
    Insert(DecodedInsert),
    LwPolyline(DecodedLwPolyline),
    Polyline2D(DecodedPolylineHeader),
    Polyline3D(DecodedPolylineHeader),
}

#[derive(Debug, Clone)]
struct DecodedInsert {
    block_handle: Option<u64>,
    alternate_block_handle: Option<u64>,
    block_name: Option<String>,
    insertion_point: Point3,
    scale: Point3,
    rotation_degrees: f64,
    column_count: u16,
    row_count: u16,
    column_spacing: f64,
    row_spacing: f64,
}

#[derive(Debug, Clone)]
struct DecodedLwPolyline {
    points: Vec<Point3>,
    closed: bool,
}

#[derive(Debug, Clone)]
struct DecodedPolylineHeader {
    closed: bool,
    owned_count: usize,
    owned_handles: Vec<u64>,
}

#[derive(Debug, Clone)]
struct DecodedVertex {
    handle: u64,
    owner_handle: Option<u64>,
    point: Point3,
}

#[derive(Clone, Copy)]
enum VertexPointLayout {
    BitDouble,
    RawDouble,
}

#[derive(Clone, Copy)]
enum InsertPointLayout {
    BitDouble,
    RawDouble,
}

#[derive(Clone, Copy)]
enum InsertBodyLayout {
    PointThenScale(InsertPointLayout),
    ScaleThenPoint(InsertPointLayout),
}

impl DecodedEntity {
    fn into_cad_entity(
        self,
        layers_by_handle: &BTreeMap<u64, Layer>,
        polyline_vertices: &BTreeMap<u64, Vec<Point3>>,
        blocks_by_handle: &BTreeMap<u64, DecodedBlockHeader>,
        block_handle_lookup: &BTreeMap<u64, u64>,
    ) -> Option<Entity> {
        let resolved_layer_handle = resolve_entity_layer_handle(
            self.common.layer_handle,
            self.common.alternate_layer_handle,
            layers_by_handle,
        );
        let common = EntityCommon {
            handle: Some(format!("{:X}", self.common.handle)),
            layer: resolved_layer_handle
                .and_then(|handle| layers_by_handle.get(&handle))
                .map(|layer| layer.name.clone()),
        };
        match self.kind {
            DecodedEntityKind::Line(mut line) => {
                line.common = common;
                Some(Entity::Line(line))
            }
            DecodedEntityKind::Arc(mut arc) => {
                arc.common = common;
                Some(Entity::Arc(arc))
            }
            DecodedEntityKind::Circle(mut circle) => {
                circle.common = common;
                Some(Entity::Circle(circle))
            }
            DecodedEntityKind::Face3D(mut face) => {
                face.common = common;
                Some(Entity::Face3D(face))
            }
            DecodedEntityKind::Insert(insert) => {
                let resolved_block_handle = resolve_insert_block_handle(
                    insert.block_handle,
                    insert.alternate_block_handle,
                    block_handle_lookup,
                );
                Some(Entity::Insert(Insert {
                    common,
                    block_name: insert
                        .block_name
                        .or_else(|| {
                            resolved_block_handle
                                .and_then(|handle| block_handle_lookup.get(&handle).copied())
                                .and_then(|handle| blocks_by_handle.get(&handle))
                                .map(|block| block.name.clone())
                        })
                        .unwrap_or_else(|| {
                            resolved_block_handle
                                .map(|handle| format!("UNKNOWN_BLOCK_{handle:X}"))
                                .unwrap_or_else(|| "UNKNOWN_BLOCK".to_string())
                        }),
                    insertion_point: insert.insertion_point,
                    scale: insert.scale,
                    rotation_degrees: insert.rotation_degrees,
                    column_count: insert.column_count,
                    row_count: insert.row_count,
                    column_spacing: insert.column_spacing,
                    row_spacing: insert.row_spacing,
                }))
            }
            DecodedEntityKind::LwPolyline(polyline) => Some(Entity::Polyline(Polyline {
                common,
                points: polyline.points,
                closed: polyline.closed,
            })),
            DecodedEntityKind::Polyline2D(header) | DecodedEntityKind::Polyline3D(header) => {
                let points = polyline_vertices.get(&self.common.handle)?.clone();
                Some(Entity::Polyline(Polyline {
                    common,
                    points,
                    closed: header.closed,
                }))
            }
        }
    }
}

fn entities_consumed_handles(objects: &[SupportedObject]) -> std::collections::BTreeSet<u64> {
    objects
        .iter()
        .filter_map(|obj| match obj {
            SupportedObject::Entity(e) => match &e.kind {
                DecodedEntityKind::Polyline2D(_) | DecodedEntityKind::Polyline3D(_) => {
                    Some(e.common.handle)
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn collect_polyline_vertices(
    objects: &[SupportedObject],
    vertices: &[DecodedVertex],
    type_hints: &BTreeMap<u64, String>,
) -> BTreeMap<u64, Vec<Point3>> {
    let mut grouped = BTreeMap::<u64, Vec<Point3>>::new();
    let vertices_by_handle = vertices
        .iter()
        .map(|vertex| (vertex.handle, vertex.point))
        .collect::<BTreeMap<_, _>>();
    // Collect the set of known polyline handles (decoded + hinted).
    let polyline_handles: BTreeSet<u64> = objects
        .iter()
        .filter_map(|object| match object {
            SupportedObject::Entity(DecodedEntity {
                common,
                kind: DecodedEntityKind::Polyline2D(_) | DecodedEntityKind::Polyline3D(_),
            }) => Some(common.handle),
            _ => None,
        })
        .chain(
            type_hints
                .iter()
                .filter(|(_, name)| {
                    name.as_str() == "POLYLINE_3D" || name.as_str() == "POLYLINE_2D"
                })
                .map(|(handle, _)| *handle),
        )
        .collect();
    // First pass: group vertices by owner_handle if it points to a known polyline.
    for vertex in vertices {
        if let Some(owner) = vertex.owner_handle.filter(|h| polyline_handles.contains(h)) {
            grouped.entry(owner).or_default().push(vertex.point);
        }
    }
    // Second pass: use owned_handles from decoded polyline headers.
    for object in objects {
        if let SupportedObject::Entity(DecodedEntity {
            common,
            kind: DecodedEntityKind::Polyline2D(header) | DecodedEntityKind::Polyline3D(header),
        }) = object
        {
            if !grouped.contains_key(&common.handle) && !header.owned_handles.is_empty() {
                let points = header
                    .owned_handles
                    .iter()
                    .filter_map(|handle| vertices_by_handle.get(handle).copied())
                    .collect::<Vec<_>>();
                if points.len() >= 2 {
                    grouped.insert(common.handle, points);
                }
            }
        }
    }
    // Third pass: sequential fallback from decode order.
    let mut current_polyline = None::<u64>;
    for object in objects {
        match object {
            SupportedObject::Entity(DecodedEntity {
                common,
                kind: DecodedEntityKind::Polyline2D(_) | DecodedEntityKind::Polyline3D(_),
            }) => {
                current_polyline = Some(common.handle);
            }
            SupportedObject::Entity(_)
            | SupportedObject::Layer(_)
            | SupportedObject::BlockHeader(_) => {
                current_polyline = None;
            }
            SupportedObject::Vertex(vertex) => {
                if !vertex
                    .owner_handle
                    .is_some_and(|h| polyline_handles.contains(&h))
                {
                    if let Some(polyline_handle) = current_polyline {
                        grouped
                            .entry(polyline_handle)
                            .or_default()
                            .push(vertex.point);
                    }
                }
            }
            SupportedObject::SeqEnd => {
                current_polyline = None;
            }
            SupportedObject::Ignored => {}
        }
    }
    // Fourth pass: handle-proximity grouping using type_hints.
    // For each polyline handle H, vertices at consecutive handles H+1, H+2, ... belong to it.
    for &polyline_handle in &polyline_handles {
        if grouped.contains_key(&polyline_handle) {
            continue;
        }
        let mut points = Vec::new();
        let mut h = polyline_handle + 1;
        while let Some(point) = vertices_by_handle.get(&h) {
            points.push(*point);
            h += 1;
        }
        if points.len() >= 2 {
            grouped.insert(polyline_handle, points);
        }
    }
    grouped
}

fn recover_owned_polyline_vertices(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    decoded_sequence: &[SupportedObject],
    vertices: &mut Vec<DecodedVertex>,
) {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let mut existing_handles = vertices
        .iter()
        .map(|vertex| vertex.handle)
        .collect::<BTreeSet<_>>();
    let referenced_handles = decoded_sequence
        .iter()
        .filter_map(|object| match object {
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Polyline2D(header) | DecodedEntityKind::Polyline3D(header),
                ..
            }) => Some(header.owned_handles.to_vec()),
            _ => None,
        })
        .flatten()
        .collect::<BTreeSet<_>>();

    for handle in referenced_handles {
        if existing_handles.contains(&handle) {
            continue;
        }
        let Some((_, _raw_offset_bits)) = handles
            .iter()
            .find(|(entry_handle, _)| *entry_handle == handle)
        else {
            continue;
        };
        let Some((preferred_offset_bits, alternate_offset_bits)) =
            exact_handle_offsets(handle_map, object_index, handle)
        else {
            continue;
        };
        let next_offset_bits = next_handle_offset_bits(&handles, handle);
        let Some(vertex) = best_exact_offset_vertex_decode(
            version,
            objects,
            handle,
            preferred_offset_bits,
            alternate_offset_bits,
            next_offset_bits,
        ) else {
            continue;
        };
        existing_handles.insert(handle);
        vertices.push(vertex);
    }
}

fn best_exact_offset_vertex_decode(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    preferred_offset_bits: usize,
    _alternate_offset_bits: Option<usize>,
    _next_offset_bits: Option<usize>,
) -> Option<DecodedVertex> {
    let start = parse_modern_object_start(objects, preferred_offset_bits)?;
    let vertex = attempt_vertex_decode_at_start(version, objects, handle, start)?;
    exact_offset_vertex_candidate_score(&vertex, start, preferred_offset_bits)?;
    Some(vertex)
}

#[cfg(test)]
fn exact_offset_start_candidates(
    objects: &[u8],
    _handle: u64,
    offset_bits: usize,
    next_offset_bits: Option<usize>,
) -> Vec<ModernObjectStart> {
    let mut starts = Vec::<ModernObjectStart>::new();
    let direct = parse_modern_object_start(objects, offset_bits)
        .map(|candidate| bounded_object_start(candidate, next_offset_bits));
    if let Some(start) = direct {
        starts.push(start);
    }
    let without_handle_stream =
        parse_modern_object_start_without_handle_stream_bits(objects, offset_bits)
            .map(|candidate| bounded_object_start(candidate, next_offset_bits));
    if let Some(start) = without_handle_stream {
        let is_new = starts.iter().all(|existing| {
            existing.object_stream_start_bits != start.object_stream_start_bits
                || existing.handle_stream_bits != start.handle_stream_bits
                || existing.object_type != start.object_type
        });
        if is_new {
            starts.push(start);
        }
    }
    if let (Some(direct), Some(without_handle_stream)) = (direct, without_handle_stream) {
        let hybrid = ModernObjectStart {
            handle_stream_bits: direct.handle_stream_bits,
            ..without_handle_stream
        };
        let is_new = starts.iter().all(|existing| {
            existing.object_stream_start_bits != hybrid.object_stream_start_bits
                || existing.handle_stream_bits != hybrid.handle_stream_bits
                || existing.object_type != hybrid.object_type
                || existing.body_start_bits != hybrid.body_start_bits
        });
        if is_new {
            starts.push(hybrid);
        }
    }
    starts
}

#[cfg(test)]
fn nearby_start_candidates(
    objects: &[u8],
    offset_bits: usize,
    start_delta: isize,
    end_delta: isize,
) -> Vec<ModernObjectStart> {
    let mut starts = Vec::<ModernObjectStart>::new();
    for delta in start_delta..=end_delta {
        let candidate_start = offset_bits as isize + delta;
        if candidate_start < 0 {
            continue;
        }
        let Some(candidate_start) = usize::try_from(candidate_start).ok() else {
            continue;
        };
        for start in [
            parse_modern_object_start(objects, candidate_start),
            parse_modern_object_start_without_handle_stream_bits(objects, candidate_start),
        ]
        .into_iter()
        .flatten()
        {
            let is_new = starts.iter().all(|existing| {
                existing._record_start_bits != start._record_start_bits
                    || existing.object_stream_start_bits != start.object_stream_start_bits
                    || existing.handle_stream_bits != start.handle_stream_bits
                    || existing.object_type != start.object_type
            });
            if is_new {
                starts.push(start);
            }
        }
        if starts.len() >= 32 {
            break;
        }
    }
    starts
}

fn exact_offset_vertex_candidate_score(
    vertex: &DecodedVertex,
    candidate: ModernObjectStart,
    raw_offset_bits: usize,
) -> Option<i64> {
    let proximity_bonus = 1_000_i64.saturating_sub(
        i64::try_from(raw_offset_bits.abs_diff(candidate._record_start_bits)).ok()?,
    );
    let owner_bonus = i64::from(vertex.owner_handle.is_some()) * 500;
    let coordinate_bonus =
        i64::from(point_has_plausible_horizontal_cad_scale(vertex.point)) * 1_000;
    Some(proximity_bonus + owner_bonus + coordinate_bonus)
}

fn build_block_handle_lookup(
    blocks_by_handle: &BTreeMap<u64, DecodedBlockHeader>,
) -> BTreeMap<u64, u64> {
    let mut lookup = BTreeMap::new();
    for (handle, block) in blocks_by_handle {
        lookup.insert(*handle, *handle);
        if let Some(begin_block_handle) = block.begin_block_handle {
            lookup.entry(begin_block_handle).or_insert(*handle);
        }
    }
    lookup
}

fn resolve_entity_layer_handle(
    primary: Option<u64>,
    alternate: Option<u64>,
    layers_by_handle: &BTreeMap<u64, Layer>,
) -> Option<u64> {
    primary
        .and_then(|handle| resolve_nearby_layer_handle(handle, layers_by_handle))
        .or_else(|| {
            alternate.and_then(|handle| resolve_nearby_layer_handle(handle, layers_by_handle))
        })
}

fn resolve_nearby_layer_handle(
    handle: u64,
    layers_by_handle: &BTreeMap<u64, Layer>,
) -> Option<u64> {
    nearby_handle_candidates(handle, LAYER_NEIGHBOR_WINDOW)
        .into_iter()
        .find(|candidate| layers_by_handle.contains_key(candidate))
}

fn resolve_insert_block_handle(
    primary: Option<u64>,
    alternate: Option<u64>,
    block_handle_lookup: &BTreeMap<u64, u64>,
) -> Option<u64> {
    primary
        .and_then(|handle| resolve_nearby_block_handle(handle, block_handle_lookup))
        .or_else(|| {
            alternate.and_then(|handle| resolve_nearby_block_handle(handle, block_handle_lookup))
        })
}

fn resolve_nearby_block_handle(
    handle: u64,
    block_handle_lookup: &BTreeMap<u64, u64>,
) -> Option<u64> {
    nearby_handle_candidates(handle, BLOCK_HEADER_NEIGHBOR_WINDOW)
        .into_iter()
        .find_map(|candidate| block_handle_lookup.get(&candidate).copied())
}

fn sorted_handle_offsets(
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
) -> Vec<(u64, usize)> {
    let mut handles = handle_map
        .iter()
        .filter_map(|(handle, raw_offset_bytes)| {
            usize::try_from(*raw_offset_bytes)
                .ok()
                .and_then(|offset_bytes| offset_bytes.checked_mul(8))
                .map(|offset_bits| (*handle, offset_bits))
        })
        .collect::<Vec<_>>();
    if handles.is_empty() {
        handles = object_index
            .iter()
            .filter_map(|record| {
                usize::try_from(record.offset_bits)
                    .ok()
                    .map(|offset_bits| (record.handle, offset_bits))
            })
            .collect::<Vec<_>>();
    }
    handles.sort_by_key(|(_, offset_bits)| *offset_bits);
    handles
}

fn exact_handle_offsets(
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    handle: u64,
) -> Option<(usize, Option<usize>)> {
    let index_record = object_index.iter().find(|record| record.handle == handle);
    let index_offset = index_record.and_then(preferred_index_offset_bits);
    let handle_map_offset = handle_map.get(&handle).and_then(|raw_offset_bytes| {
        usize::try_from(*raw_offset_bytes)
            .ok()
            .and_then(|offset_bytes| offset_bytes.checked_mul(8))
    });
    if let Some(preferred) = index_offset.or(handle_map_offset) {
        return Some((preferred, None));
    }
    let preferred = index_record.and_then(|record| usize::try_from(record.raw_offset_bits).ok())?;
    Some((preferred, None))
}

fn preferred_index_offset_bits(record: &DwgObjectRecordSummary) -> Option<usize> {
    let offset_bits = usize::try_from(record.offset_bits).ok()?;
    let delta_bits = record
        .handle_match_search_delta_bits
        .and_then(|delta| usize::try_from(delta).ok())
        .filter(|delta| *delta <= 32)
        .unwrap_or(0);
    offset_bits.checked_add(delta_bits)
}

fn next_handle_offset_bits(handles: &[(u64, usize)], handle: u64) -> Option<usize> {
    let index = handles
        .iter()
        .position(|(entry_handle, _)| *entry_handle == handle)?;
    handles.get(index + 1).map(|(_, offset_bits)| *offset_bits)
}

fn direct_object_start_for_handle(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    index_record: Option<&DwgObjectRecordSummary>,
    preferred_offset_bits: usize,
    alternate_offset_bits: Option<usize>,
    next_offset_bits: Option<usize>,
) -> Option<ModernObjectStart> {
    let mut starts = exact_runtime_object_start_candidates(
        version,
        objects,
        handle,
        preferred_offset_bits,
        alternate_offset_bits,
        next_offset_bits,
    );
    if let Some(record) = index_record {
        if let Some(object_type) = record.object_type {
            for start_candidate in &mut starts {
                start_candidate.object_type = object_type;
            }
        }
    }
    let start = starts.into_iter().max_by_key(|candidate| {
        let proximity = 1_000_i64.saturating_sub(
            i64::try_from(preferred_offset_bits.abs_diff(candidate._record_start_bits))
                .unwrap_or(1_000),
        );
        let bounded_bonus = i64::from(candidate.object_data_end_bits.is_some()) * 512;
        proximity + bounded_bonus
    });
    if start.is_none() && DEBUG_DIRECT_OFFSET_DECODE {
        eprintln!(
            "native dwg: failed to parse object preamble at handle={handle:X} offset_bits={preferred_offset_bits}"
        );
    }
    start
}

fn exact_runtime_object_start_candidates(
    version: CadVersion,
    objects: &[u8],
    _handle: u64,
    preferred_offset_bits: usize,
    alternate_offset_bits: Option<usize>,
    next_offset_bits: Option<usize>,
) -> Vec<ModernObjectStart> {
    let mut starts = Vec::<ModernObjectStart>::new();
    for offset_bits in [Some(preferred_offset_bits), alternate_offset_bits]
        .into_iter()
        .flatten()
    {
        let mut candidates: Vec<Option<ModernObjectStart>> =
            vec![parse_modern_object_start(objects, offset_bits)
                .map(|candidate| bounded_object_start(candidate, next_offset_bits))];
        if !is_r2004_plus(version) {
            candidates.push(
                parse_legacy_object_start(objects, offset_bits)
                    .map(|candidate| bounded_object_start(candidate, next_offset_bits)),
            );
        }
        for start in candidates.into_iter().flatten() {
            let is_new = starts.iter().all(|existing| {
                existing._record_start_bits != start._record_start_bits
                    || existing.object_stream_start_bits != start.object_stream_start_bits
                    || existing.handle_stream_bits != start.handle_stream_bits
                    || existing.object_type != start.object_type
                    || existing.body_start_bits != start.body_start_bits
            });
            if is_new {
                starts.push(start);
            }
        }
    }
    starts
}

fn bounded_object_start(
    mut start: ModernObjectStart,
    next_offset_bits: Option<usize>,
) -> ModernObjectStart {
    let Some(next_offset_bits) = next_offset_bits else {
        return start;
    };
    if next_offset_bits <= start.object_stream_start_bits {
        return start;
    }
    let available_bits = next_offset_bits.saturating_sub(start.object_stream_start_bits);
    let available_bytes = available_bits.div_ceil(8);
    if available_bytes == 0 {
        return start;
    }
    start.size_bytes = start.size_bytes.min(available_bytes);
    start.handle_stream_bits = start
        .handle_stream_bits
        .min(u64::try_from(available_bits).unwrap_or(u64::MAX));
    start
}

fn decode_supported_objects(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
) -> Vec<SupportedObject> {
    let profile = std::env::var_os("DWG_PROFILE").is_some();
    let handles = sorted_handle_offsets(handle_map, object_index);
    let mut failed_offsets = 0usize;
    let mut failed_starts = 0usize;
    let mut unsupported_types = BTreeMap::<String, usize>::new();
    let mut supported_types = BTreeMap::<String, usize>::new();
    let mut unknown_types = 0usize;
    let mut decode_results = BTreeMap::<String, usize>::new();
    let results = handles
        .iter()
        .map(|(handle, _raw_offset_bits)| {
            let Some((preferred_offset_bits, alternate_offset_bits)) =
                exact_handle_offsets(handle_map, object_index, *handle)
            else {
                failed_offsets += 1;
                return SupportedObject::Ignored;
            };
            let Some(candidate) = direct_object_start_for_handle(
                version,
                objects,
                *handle,
                object_index.iter().find(|record| record.handle == *handle),
                preferred_offset_bits,
                alternate_offset_bits,
                next_handle_offset_bits(&handles, *handle),
            ) else {
                failed_starts += 1;
                if profile && failed_starts <= 10 {
                    let byte_offset = preferred_offset_bits / 8;
                    let preview: Vec<u8> = (0..16)
                        .filter_map(|i| objects.get(byte_offset + i).copied())
                        .collect();
                    let ms_result = parse_modern_object_start(objects, preferred_offset_bits);
                    let mc_result = parse_legacy_object_start(objects, preferred_offset_bits);
                    eprintln!(
                        "dwg decode: FAILED START handle={:#x} offset_bits={} byte={} preview={:02x?} ms={:?} mc={:?}",
                        handle, preferred_offset_bits, byte_offset, preview,
                        ms_result.map(|s| (s.size_bytes, s.handle_stream_bits, s.object_type)),
                        mc_result.map(|s| (s.size_bytes, s.handle_stream_bits, s.object_type)),
                    );
                }
                return SupportedObject::Ignored;
            };
            if profile {
                let type_name = crate::object_type_name(candidate.object_type, classes);
                match &type_name {
                    Some(name) if is_supported_object_type(name) => {
                        let count = supported_types.entry(name.clone()).or_default();
                        *count += 1;
                        if name == "VERTEX_3D" && *count <= 2 {
                            let byte_offset = candidate._record_start_bits / 8;
                            let preview: Vec<u8> = (0..24)
                                .filter_map(|i| objects.get(byte_offset + i).copied())
                                .collect();
                            eprintln!(
                                "dwg decode: VERTEX_3D handle={:#x} rec_start={} obj_start={} size={} hsb={} type={:#x} preview={:02x?}",
                                handle, candidate._record_start_bits, candidate.object_stream_start_bits,
                                candidate.size_bytes, candidate.handle_stream_bits, candidate.object_type, preview
                            );
                        }
                    }
                    Some(name) => {
                        *unsupported_types.entry(name.clone()).or_default() += 1;
                    }
                    None => {
                        unknown_types += 1;
                        if candidate.object_type == 0 && unknown_types <= 5 {
                            let byte_offset = candidate._record_start_bits / 8;
                            let preview: Vec<u8> = (0..12)
                                .filter_map(|i| objects.get(byte_offset + i).copied())
                                .collect();
                            eprintln!(
                                "dwg decode: type-0 handle={:#x} offset_bits={} byte_offset={} size_bytes={} hsb={} obj_start={} preview={:02x?}",
                                handle, candidate._record_start_bits, byte_offset, candidate.size_bytes, candidate.handle_stream_bits, candidate.object_stream_start_bits, preview
                            );
                        }
                        *unsupported_types.entry(format!("?{:#06x}", candidate.object_type)).or_default() += 1;
                    }
                }
            }
            let result = attempt_supported_object_decode(
                version,
                objects,
                *handle,
                candidate,
                classes,
                type_hints,
            )
            .unwrap_or(SupportedObject::Ignored);
            if profile {
                let label = match &result {
                    SupportedObject::Layer(_) => "Layer",
                    SupportedObject::BlockHeader(_) => "BlockHeader",
                    SupportedObject::Entity(e) => match &e.kind {
                        DecodedEntityKind::Line(_) => "Line",
                        DecodedEntityKind::Arc(_) => "Arc",
                        DecodedEntityKind::Circle(_) => "Circle",
                        DecodedEntityKind::Face3D(_) => "Face3D",
                        DecodedEntityKind::Insert(_) => "Insert",
                        DecodedEntityKind::Polyline2D(_) | DecodedEntityKind::Polyline3D(_) => "Polyline",
                        DecodedEntityKind::LwPolyline(_) => "LwPolyline",
                    },
                    SupportedObject::Vertex(_) => "Vertex",
                    SupportedObject::SeqEnd => "SeqEnd",
                    SupportedObject::Ignored => "Ignored",
                };
                *decode_results.entry(label.to_string()).or_default() += 1;
            }
            result
        })
        .collect();
    if profile {
        eprintln!(
            "dwg decode: {} handles, {} failed offsets, {} failed starts",
            handles.len(),
            failed_offsets,
            failed_starts
        );
        let mut sup: Vec<_> = supported_types.into_iter().collect();
        sup.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("dwg decode: supported types: {:?}", sup);
        let mut unsup: Vec<_> = unsupported_types.into_iter().collect();
        unsup.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!(
            "dwg decode: unsupported types: {:?}, unknown: {}",
            unsup, unknown_types
        );
        let mut res: Vec<_> = decode_results.into_iter().collect();
        res.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("dwg decode: results: {:?}", res);
    }
    results
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn exact_supported_object_candidates(
    objects: &[u8],
    handle: u64,
    index_record: Option<&DwgObjectRecordSummary>,
    preferred_offset_bits: usize,
    alternate_offset_bits: Option<usize>,
    next_offset_bits: Option<usize>,
) -> Vec<ModernObjectStart> {
    let mut starts = Vec::new();
    for offset_bits in [Some(preferred_offset_bits), alternate_offset_bits]
        .into_iter()
        .flatten()
    {
        for mut candidate in
            exact_offset_start_candidates(objects, handle, offset_bits, next_offset_bits)
        {
            if let Some(record) = index_record {
                if let Some(object_type) = record.object_type {
                    candidate.object_type = object_type;
                }
            }
            push_unique_exact_supported_object_candidate(&mut starts, candidate);
        }
    }
    starts
}

#[cfg(test)]
fn push_unique_exact_supported_object_candidate(
    starts: &mut Vec<ModernObjectStart>,
    candidate: ModernObjectStart,
) {
    let is_new = starts.iter().all(|existing| {
        existing._record_start_bits != candidate._record_start_bits
            || existing.object_stream_start_bits != candidate.object_stream_start_bits
            || existing.handle_stream_bits != candidate.handle_stream_bits
            || existing.object_type != candidate.object_type
            || existing.body_start_bits != candidate.body_start_bits
    });
    if is_new {
        starts.push(candidate);
    }
}

#[allow(clippy::too_many_arguments)]
fn recover_hinted_block_headers(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    _classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
    blocks_by_handle: &mut BTreeMap<u64, DecodedBlockHeader>,
) {
    let handles = sorted_handle_offsets(handle_map, object_index);
    for (handle, type_name) in type_hints {
        if type_name != "BLOCK_HEADER" || blocks_by_handle.contains_key(handle) {
            continue;
        }
        // Try standard offset-based approach first
        if let Some((preferred_offset_bits, alternate_offset_bits)) =
            exact_handle_offsets(handle_map, object_index, *handle)
        {
            if let Some(start) = direct_object_start_for_handle(
                version,
                objects,
                *handle,
                object_index.iter().find(|record| record.handle == *handle),
                preferred_offset_bits,
                alternate_offset_bits,
                next_handle_offset_bits(&handles, *handle),
            ) {
                if start.object_type == 0x31 {
                    let mut ctx = ObjectDecodeContext::new_with_text_search_bits(
                        version,
                        objects,
                        *handle,
                        start,
                        Some(MAX_TABLE_TEXT_STREAM_SEARCH_BITS),
                    );
                    if let Some(mut block) = decode_block_header(&mut ctx) {
                        if text_name_is_reasonable(&block.name) {
                            // xref flags are unreliable due to object stream bit alignment issues
                            block.is_xref = false;
                            blocks_by_handle.insert(block.handle, block);
                            continue;
                        }
                    }
                }
            }
        }
        // Fallback: search Objects section for this handle's object header.
        // Use parse_object_header which verifies the declared handle.
        if let Some(start) = search_object_by_handle(version, objects, *handle, &handles, 0x31) {
            let mut ctx = ObjectDecodeContext::new_with_text_search_bits(
                version,
                objects,
                *handle,
                start,
                Some(MAX_TABLE_TEXT_STREAM_SEARCH_BITS),
            );
            if let Some(mut block) =
                decode_block_header(&mut ctx).filter(|b| text_name_is_reasonable(&b.name))
            {
                block.is_xref = false;
                blocks_by_handle.insert(block.handle, block);
            }
        }
    }
}

/// Search the Objects section for an object with a specific handle.
/// Uses parse_modern_object_start at byte-aligned positions then verifies the declared handle.
fn search_object_by_handle(
    _version: CadVersion,
    objects: &[u8],
    expected_handle: u64,
    sorted_handles: &[(u64, usize)],
    expected_type: u32,
) -> Option<ModernObjectStart> {
    // First try near known handle offsets (fast path)
    let nearby_offset = sorted_handles
        .iter()
        .filter(|(h, _)| h.abs_diff(expected_handle) <= 64)
        .map(|(_, offset)| *offset)
        .min();

    // Search ranges: near known offsets first, then full scan if needed
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    if let Some(offset) = nearby_offset {
        ranges.push((
            offset.saturating_sub(16384),
            (offset + 16384).min(objects.len() * 8),
        ));
    }
    // Full scan as fallback
    ranges.push((0, objects.len() * 8));

    for (search_start, search_end) in ranges {
        for bit_offset in (search_start..search_end).step_by(8) {
            let Some(start) = parse_modern_object_start(objects, bit_offset) else {
                continue;
            };
            if start.object_type != expected_type {
                continue;
            }
            // Verify the declared handle by reading type + handle from the object stream
            let handle = declared_handle_after_object_type(
                objects,
                start.object_stream_start_bits,
                ObjectBitOrder::Lsb,
            );
            if handle != Some(expected_handle) {
                continue;
            }
            return Some(start);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn recover_referenced_layers(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    _classes: &[DwgClassSummary],
    _type_hints: &BTreeMap<u64, String>,
    entities: &[DecodedEntity],
    layers_by_handle: &mut BTreeMap<u64, Layer>,
) {
    let handles = sorted_handle_offsets(handle_map, object_index);

    let referenced_handles = entities
        .iter()
        .flat_map(|entity| {
            [
                entity.common.layer_handle,
                entity.common.alternate_layer_handle,
            ]
        })
        .flatten()
        .collect::<BTreeSet<_>>();

    for handle in referenced_handles {
        if layers_by_handle.contains_key(&handle) {
            continue;
        }
        for candidate_handle in nearby_handle_candidates(handle, LAYER_NEIGHBOR_WINDOW) {
            if layers_by_handle.contains_key(&candidate_handle) {
                break;
            }
            let Some((preferred_offset_bits, alternate_offset_bits)) =
                exact_handle_offsets(handle_map, object_index, candidate_handle)
            else {
                continue;
            };
            let Some(start) = direct_object_start_for_handle(
                version,
                objects,
                candidate_handle,
                object_index
                    .iter()
                    .find(|record| record.handle == candidate_handle),
                preferred_offset_bits,
                alternate_offset_bits,
                next_handle_offset_bits(&handles, candidate_handle),
            ) else {
                continue;
            };
            let mut ctx = ObjectDecodeContext::new(version, objects, candidate_handle, start);
            let Some(layer) = decode_layer(&mut ctx).filter(targeted_layer_is_reasonable) else {
                continue;
            };
            layers_by_handle.insert(
                layer.handle,
                Layer {
                    name: layer.name,
                    visible: layer.visible,
                },
            );
            break;
        }
    }
}

fn nearby_handle_candidates(handle: u64, window: u64) -> Vec<u64> {
    let mut candidates = Vec::with_capacity((window as usize) * 2 + 1);
    candidates.push(handle);
    for delta in 1..=window {
        candidates.push(handle.saturating_sub(delta));
        candidates.push(handle.saturating_add(delta));
    }
    candidates
}

fn attempt_supported_object_decode(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
    classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
) -> Option<SupportedObject> {
    let mut starts = vec![candidate];
    if let Some(without_handle_stream) =
        parse_modern_object_start_without_handle_stream_bits(objects, candidate._record_start_bits)
    {
        let is_new = starts.iter().all(|existing| {
            existing.object_stream_start_bits != without_handle_stream.object_stream_start_bits
                || existing.handle_stream_bits != without_handle_stream.handle_stream_bits
                || existing.object_type != without_handle_stream.object_type
        });
        if is_new {
            starts.push(without_handle_stream);
        }
    }
    // Fallback: read through type + handle + EED + graphic_present using LSB (matching
    // the object data stream) to find the correct body_start_bits.
    if let Some(body_start) = compute_body_start_lsb(version, objects, handle, candidate) {
        let is_new = starts
            .iter()
            .all(|existing| existing.body_start_bits != body_start.body_start_bits);
        if is_new {
            starts.push(body_start);
        }
    }

    starts
        .into_iter()
        .filter_map(|start| {
            let object = attempt_supported_object_decode_at_start(
                version, objects, handle, start, classes, type_hints,
            )?;
            let score = supported_object_candidate_score(
                &object,
                start,
                candidate._record_start_bits,
                type_hints,
            )?;
            Some((score, object))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, object)| object)
}

fn attempt_supported_object_decode_at_start(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
    classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
) -> Option<SupportedObject> {
    let candidate_type_name = object_type_name(candidate.object_type, classes);
    if candidate_type_name
        .as_deref()
        .is_some_and(is_supported_object_type)
    {
        return attempt_supported_object_decode_as(
            version,
            objects,
            handle,
            candidate,
            candidate_type_name.as_deref(),
            type_hints,
        );
    }
    let msb_object_type = read_object_type_msb_at(objects, candidate.object_stream_start_bits);
    let msb_type_name =
        msb_object_type.and_then(|object_type| object_type_name(object_type, classes));
    if let (Some(candidate_type_name), Some(msb_type_name)) =
        (candidate_type_name.as_deref(), msb_type_name.as_deref())
    {
        if candidate_type_name != msb_type_name
            && (is_geometry_object_type(candidate_type_name)
                || is_geometry_object_type(msb_type_name))
        {
            return None;
        }
    }
    let ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
    let lsb_object_type = ctx.object_type();
    let lsb_type_name =
        lsb_object_type.and_then(|object_type| object_type_name(object_type, classes));
    let object_type_name =
        select_supported_object_type_name(lsb_type_name.as_deref(), msb_type_name.as_deref())
            .map(str::to_string)
            .or_else(|| {
                lsb_object_type.or(msb_object_type).and_then(|object_type| {
                    preferred_object_type_name(handle, Some(object_type), classes, type_hints)
                })
            });
    attempt_supported_object_decode_as(
        version,
        objects,
        handle,
        candidate,
        object_type_name.as_deref(),
        type_hints,
    )
}

fn attempt_supported_object_decode_as(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
    object_type_name: Option<&str>,
    type_hints: &BTreeMap<u64, String>,
) -> Option<SupportedObject> {
    match object_type_name {
        Some("LAYER") => {
            let mut ctx = ObjectDecodeContext::new_with_text_search_bits(
                version,
                objects,
                handle,
                candidate,
                Some(MAX_TABLE_TEXT_STREAM_SEARCH_BITS),
            );
            decode_layer(&mut ctx).map(SupportedObject::Layer)
        }
        Some("BLOCK_HEADER") => {
            let mut ctx = ObjectDecodeContext::new_with_text_search_bits(
                version,
                objects,
                handle,
                candidate,
                Some(MAX_TABLE_TEXT_STREAM_SEARCH_BITS),
            );
            decode_block_header(&mut ctx).map(SupportedObject::BlockHeader)
        }
        Some(name) if is_geometry_object_type(name) => {
            attempt_geometry_decode(version, objects, handle, candidate, Some(name), type_hints)
        }
        Some("SEQEND") => Some(SupportedObject::SeqEnd),
        None => Some(SupportedObject::Ignored),
        _ => {
            if type_hints.contains_key(&handle) {
                Some(SupportedObject::Ignored)
            } else {
                attempt_geometry_decode(version, objects, handle, candidate, None, type_hints)
                    .or(Some(SupportedObject::Ignored))
            }
        }
    }
}

fn attempt_geometry_decode(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
    preferred_name: Option<&str>,
    type_hints: &BTreeMap<u64, String>,
) -> Option<SupportedObject> {
    let try_entity = |decode: fn(&mut ObjectDecodeContext<'_>) -> Option<DecodedEntity>| {
        let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
        let entity = decode(&mut ctx)?;
        if !decoded_entity_is_reasonable(&entity, type_hints) {
            return None;
        }
        let object = SupportedObject::Entity(entity.clone());
        if preferred_name.is_some_and(|name| !supported_object_matches_geometry_hint(&object, name))
        {
            return None;
        }
        if matches!(
            entity.kind,
            DecodedEntityKind::Polyline2D(_) | DecodedEntityKind::Polyline3D(_)
        ) {
            return Some(object);
        }
        let cad = entity.clone().into_cad_entity(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )?;
        entity_is_reasonable(&cad).then_some(object)
    };
    let try_vertex = |decode: fn(&mut ObjectDecodeContext<'_>) -> Option<DecodedVertex>| {
        let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
        let vertex = decode(&mut ctx)?;
        let object = SupportedObject::Vertex(vertex.clone());
        if preferred_name.is_some_and(|name| !supported_object_matches_geometry_hint(&object, name))
        {
            return None;
        }
        vertex_is_reasonable(&vertex).then_some(object)
    };
    match preferred_name {
        Some("LINE") => try_entity(decode_line),
        Some("ARC") => try_entity(decode_arc),
        Some("CIRCLE") => try_entity(decode_circle),
        Some("3DFACE") => try_entity(decode_face3d),
        Some("INSERT") => try_entity(decode_insert).or_else(|| try_entity(decode_minsert)),
        Some("MINSERT") => try_entity(decode_minsert).or_else(|| try_entity(decode_insert)),
        Some("LWPOLYLINE") => try_entity(decode_lwpolyline)
            .or_else(|| try_entity(decode_polyline2d))
            .or_else(|| try_entity(decode_polyline3d)),
        Some("POLYLINE_2D") => try_entity(decode_polyline2d)
            .or_else(|| try_entity(decode_lwpolyline))
            .or_else(|| try_entity(decode_polyline3d))
            .or_else(|| try_vertex(decode_vertex2d))
            .or_else(|| try_vertex(decode_vertex3d)),
        Some("POLYLINE_3D") => try_entity(decode_polyline3d)
            .or_else(|| try_entity(decode_polyline2d))
            .or_else(|| try_entity(decode_lwpolyline))
            .or_else(|| try_vertex(decode_vertex3d))
            .or_else(|| try_vertex(decode_vertex2d)),
        Some("VERTEX_2D") => try_entity(decode_lwpolyline)
            .or_else(|| try_entity(decode_polyline2d))
            .or_else(|| try_vertex(decode_vertex2d))
            .or_else(|| try_vertex(decode_vertex3d)),
        Some("VERTEX_3D")
        | Some("VERTEX_MESH")
        | Some("VERTEX_PFACE")
        | Some("VERTEX_PFACE_FACE") => try_entity(decode_lwpolyline)
            .or_else(|| try_entity(decode_polyline3d))
            .or_else(|| try_entity(decode_polyline2d))
            .or_else(|| try_vertex(decode_vertex3d))
            .or_else(|| try_vertex(decode_vertex2d)),
        Some("SEQEND") => Some(SupportedObject::SeqEnd),
        _ => try_entity(decode_polyline3d)
            .or_else(|| try_entity(decode_polyline2d))
            .or_else(|| try_entity(decode_lwpolyline))
            .or_else(|| try_entity(decode_insert))
            .or_else(|| try_vertex(decode_vertex3d))
            .or_else(|| try_vertex(decode_vertex2d))
            .or_else(|| try_entity(decode_minsert))
            .or_else(|| try_entity(decode_line))
            .or_else(|| try_entity(decode_arc))
            .or_else(|| try_entity(decode_circle))
            .or_else(|| try_entity(decode_face3d)),
    }
}

fn insert_entity_quality_score(entity: &DecodedEntity) -> i64 {
    match &entity.kind {
        DecodedEntityKind::Insert(insert) => {
            let block_bonus =
                i64::from(insert.block_handle.is_some() || insert.alternate_block_handle.is_some())
                    * 4_096;
            let point_bonus = i64::from(point_has_plausible_horizontal_cad_scale(
                insert.insertion_point,
            )) * 2_048;
            let local_anchor_bonus = i64::from(
                point_is_local_insert_anchor(insert.insertion_point)
                    && insert_scale_is_plausible(insert.scale),
            ) * 512;
            let layer_bonus = i64::from(entity.common.layer_handle.is_some()) * 512;
            let array_bonus = i64::from(insert.column_count > 1 || insert.row_count > 1) * 64;
            block_bonus + point_bonus + local_anchor_bonus + layer_bonus + array_bonus
        }
        _ => i64::MIN,
    }
}

fn attempt_vertex_decode_at_start(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
) -> Option<DecodedVertex> {
    let try_vertex = |decode: fn(&mut ObjectDecodeContext<'_>) -> Option<DecodedVertex>| {
        let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
        let vertex = decode(&mut ctx)?;
        vertex_is_reasonable(&vertex).then_some(vertex)
    };
    try_vertex(decode_vertex3d).or_else(|| try_vertex(decode_vertex2d))
}

#[cfg(test)]
fn attempt_lwpolyline_decode_at_start(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
) -> Option<DecodedEntity> {
    let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
    let entity = decode_lwpolyline(&mut ctx)?;
    decoded_entity_is_reasonable(&entity, &BTreeMap::new()).then_some(entity)
}

#[cfg(test)]
fn horizontal_bounds(points: &[Point3]) -> Option<(Point3, Point3)> {
    let mut iter = points.iter().copied();
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for point in iter {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        min.z = min.z.min(point.z);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
        max.z = max.z.max(point.z);
    }
    Some((min, max))
}

#[cfg(test)]
fn forced_lwpolyline_candidate_score(
    polyline: &DecodedLwPolyline,
    candidate: ModernObjectStart,
    raw_offset_bits: usize,
) -> Option<i64> {
    let (min, max) = horizontal_bounds(&polyline.points)?;
    let horizontal_extent = (max.x - min.x).abs() + (max.z - min.z).abs();
    let plausible_points = polyline
        .points
        .iter()
        .filter(|point| point_has_plausible_horizontal_cad_scale(**point))
        .count();
    let distinct_points = polyline
        .points
        .windows(2)
        .filter(|window| {
            let a = window[0];
            let b = window[1];
            (a.x - b.x).abs() > MIN_REASONABLE_EXTENT
                || (a.y - b.y).abs() > MIN_REASONABLE_EXTENT
                || (a.z - b.z).abs() > MIN_REASONABLE_EXTENT
        })
        .count();
    let proximity_bonus = 1_000_i64.saturating_sub(
        i64::try_from(raw_offset_bits.abs_diff(candidate._record_start_bits)).ok()?,
    );
    let point_bonus = i64::try_from(polyline.points.len().min(128))
        .ok()?
        .saturating_mul(32);
    let plausible_bonus = i64::try_from(plausible_points.min(32))
        .ok()?
        .saturating_mul(256);
    let distinct_bonus = i64::try_from(distinct_points.min(32))
        .ok()?
        .saturating_mul(64);
    let extent_bonus = horizontal_extent
        .is_finite()
        .then_some(horizontal_extent.clamp(1.0, 100_000.0) as i64)?;
    Some(proximity_bonus + point_bonus + plausible_bonus + distinct_bonus + extent_bonus)
}

fn supported_object_matches_geometry_hint(object: &SupportedObject, preferred_name: &str) -> bool {
    matches!(
        (preferred_name, object),
        (
            "LINE",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Line(_),
                ..
            })
        ) | (
            "ARC",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Arc(_),
                ..
            })
        ) | (
            "CIRCLE",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Circle(_),
                ..
            })
        ) | (
            "3DFACE",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Face3D(_),
                ..
            })
        ) | (
            "INSERT" | "MINSERT",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Insert(_),
                ..
            })
        ) | (
            "LWPOLYLINE",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::LwPolyline(_),
                ..
            })
        ) | (
            "POLYLINE_2D",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Polyline2D(_),
                ..
            })
        ) | (
            "POLYLINE_3D",
            SupportedObject::Entity(DecodedEntity {
                kind: DecodedEntityKind::Polyline3D(_),
                ..
            })
        ) | (
            "VERTEX_2D" | "VERTEX_3D" | "VERTEX_MESH" | "VERTEX_PFACE" | "VERTEX_PFACE_FACE",
            SupportedObject::Vertex(_),
        ) | ("SEQEND", SupportedObject::SeqEnd)
    )
}

fn decode_layer(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedLayer> {
    let common = read_common_table_data(ctx)?;
    let values = if is_r2004_plus(ctx.version) {
        ctx.object.read_bit_short()?
    } else {
        let frozen = u16::from(ctx.object.read_bit().unwrap_or(false));
        let off = u16::from(ctx.object.read_bit().unwrap_or(false));
        let frozen_in_new = u16::from(ctx.object.read_bit().unwrap_or(false));
        let locked = u16::from(ctx.object.read_bit().unwrap_or(false));
        i16::try_from(frozen | (off << 1) | (frozen_in_new << 2) | (locked << 3)).ok()?
    };
    Some(DecodedLayer {
        handle: common.handle,
        name: common.name,
        visible: (values & 0b10) == 0,
    })
}

fn decode_block_header(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedBlockHeader> {
    let common = read_common_table_data(ctx)?;
    let _anonymous = ctx.object.read_bit().unwrap_or(false);
    let _has_attributes = ctx.object.read_bit().unwrap_or(false);
    let blk_is_xref = ctx.object.read_bit().unwrap_or(false);
    let is_xref_overlay = ctx.object.read_bit().unwrap_or(false);
    if matches!(
        ctx.version,
        CadVersion::Acad2000
            | CadVersion::Acad2004
            | CadVersion::Acad2007
            | CadVersion::Acad2010
            | CadVersion::Acad2013
            | CadVersion::Acad2018
    ) {
        let _xref_loaded = ctx.object.read_bit().unwrap_or(false);
    }
    let owned_count = if is_r2004_plus(ctx.version) && !blk_is_xref && !is_xref_overlay {
        ctx.object
            .read_bit_long()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
            .min(MAX_OWNED_HANDLES)
    } else {
        0
    };
    let base_point = ctx.object.read_3bit_double().unwrap_or(Point3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    let _xref_path = ctx.text.read_variable_text(ctx.version).unwrap_or_default();
    let mut insert_count = 0usize;
    while ctx.object.read_byte().is_some_and(|value| value != 0) {
        insert_count += 1;
        if insert_count >= MAX_INSERT_HANDLES {
            break;
        }
    }
    let _comments = ctx.text.read_variable_text(ctx.version).unwrap_or_default();
    let preview_size = ctx
        .object
        .read_bit_long()
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or(0);
    let _ = ctx
        .object
        .advance_bytes(preview_size.min(MAX_PREVIEW_BYTES));
    let _units = ctx.object.read_bit_short().unwrap_or_default();
    let _explodable = ctx.object.read_bit().unwrap_or(false);
    let _can_scale = ctx.object.read_byte().unwrap_or_default();
    let begin_block_handle = ctx.handles.read_handle_reference(common.handle);
    if !is_r2004_plus(ctx.version) && !blk_is_xref && !is_xref_overlay {
        let _first_entity = ctx.handles.read_handle_reference(0);
        let _last_entity = ctx.handles.read_handle_reference(0);
    }
    for _ in 0..owned_count {
        let _owned = ctx.handles.read_handle_reference(0);
    }
    let end_block_handle = ctx.handles.read_handle_reference(0);
    for _ in 0..insert_count {
        let _insert = ctx.handles.read_handle_reference(0);
    }
    let _layout = ctx.handles.read_handle_reference(0);
    let normalized = common.name.to_ascii_uppercase();
    Some(DecodedBlockHeader {
        handle: common.handle,
        begin_block_handle,
        end_block_handle,
        name: common.name,
        base_point,
        is_xref: false,
        is_paper_space: normalized == PAPER_SPACE_NAME,
    })
}

fn decode_line(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let common = read_common_entity_data(ctx)?;
    let z_zero = ctx.object.read_bit()?;
    let start_x = ctx.object.read_double()?;
    let end_x = ctx.object.read_bit_double_with_default(start_x)?;
    let start_y = ctx.object.read_double()?;
    let end_y = ctx.object.read_bit_double_with_default(start_y)?;
    let (start_z, end_z) = if z_zero {
        (0.0, 0.0)
    } else {
        let start_z = ctx.object.read_double()?;
        let end_z = ctx.object.read_bit_double_with_default(start_z)?;
        (start_z, end_z)
    };
    let _thickness = ctx.object.read_bit_thickness().unwrap_or(0.0);
    let _normal = ctx.object.read_bit_extrusion();
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::Line(Line {
            common: EntityCommon {
                handle: None,
                layer: None,
            },
            start: Point3 {
                x: start_x,
                y: start_z,
                z: start_y,
            },
            end: Point3 {
                x: end_x,
                y: end_z,
                z: end_y,
            },
        }),
    })
}

fn decode_arc(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let common = read_common_entity_data(ctx)?;
    let center = ctx.object.read_3bit_double()?;
    let radius = ctx.object.read_bit_double()?;
    let _thickness = ctx.object.read_bit_thickness().unwrap_or(0.0);
    let _normal = ctx.object.read_bit_extrusion();
    let start_angle = ctx.object.read_bit_double()?;
    let end_angle = ctx.object.read_bit_double()?;
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::Arc(Arc {
            common: EntityCommon {
                handle: None,
                layer: None,
            },
            center,
            radius,
            start_angle_degrees: start_angle.to_degrees(),
            end_angle_degrees: end_angle.to_degrees(),
        }),
    })
}

fn decode_circle(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let common = match read_common_entity_data_detailed(ctx) {
        Ok(c) => c,
        Err(stage) => {
            if std::env::var_os("DWG_PROFILE").is_some() {
                eprintln!(
                    "dwg decode: circle handle={:#x} common_entity_data failed: {stage}",
                    ctx.expected_handle
                );
            }
            return None;
        }
    };
    let center = ctx.object.read_3bit_double();
    if center.is_none() {
        if std::env::var_os("DWG_PROFILE").is_some() {
            eprintln!(
                "dwg decode: circle handle={:#x} center read failed at bit {}",
                ctx.expected_handle, ctx.object.bit_index
            );
        }
        return None;
    }
    let center = center?;
    let radius = ctx.object.read_bit_double()?;
    let _thickness = ctx.object.read_bit_thickness().unwrap_or(0.0);
    let _normal = ctx.object.read_bit_extrusion();
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::Circle(Circle {
            common: EntityCommon {
                handle: None,
                layer: None,
            },
            center,
            radius,
        }),
    })
}

fn decode_face3d(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let common = read_common_entity_data(ctx)?;
    let _has_no_flags = ctx.object.read_bit().unwrap_or(false);
    let z_zero = ctx.object.read_bit().unwrap_or(false);
    let x = ctx.object.read_double()?;
    let y = ctx.object.read_double()?;
    let z = if z_zero {
        0.0
    } else {
        ctx.object.read_double()?
    };
    let first = Point3 { x, y, z };
    let second = ctx.object.read_3bit_double_with_default(first)?;
    let third = ctx.object.read_3bit_double_with_default(second)?;
    let fourth = ctx.object.read_3bit_double_with_default(third)?;
    if !ctx.object.read_bit().unwrap_or(true) {
        let _flags = ctx.object.read_bit_short();
    }
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::Face3D(Face3D {
            common: EntityCommon {
                handle: None,
                layer: None,
            },
            corners: [first, second, third, fourth],
        }),
    })
}

fn decode_insert(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    decode_insert_detailed(ctx, false).ok()
}

fn decode_minsert(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    decode_insert_detailed(ctx, true).ok()
}

fn decode_insert_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
    is_minsert: bool,
) -> Result<DecodedEntity, &'static str> {
    let common = read_common_entity_data_detailed(ctx)?;
    let primary = decode_insert_body(
        common.clone(),
        ctx.object.clone(),
        ctx.handles.clone(),
        is_minsert,
        InsertBodyLayout::PointThenScale(InsertPointLayout::BitDouble),
    );
    let fallback = decode_insert_body(
        common.clone(),
        ctx.object.clone(),
        ctx.handles.clone(),
        is_minsert,
        InsertBodyLayout::PointThenScale(InsertPointLayout::RawDouble),
    );
    let scale_first = decode_insert_body(
        common.clone(),
        ctx.object.clone(),
        ctx.handles.clone(),
        is_minsert,
        InsertBodyLayout::ScaleThenPoint(InsertPointLayout::BitDouble),
    );
    let scale_first_raw = decode_insert_body(
        common,
        ctx.object.clone(),
        ctx.handles.clone(),
        is_minsert,
        InsertBodyLayout::ScaleThenPoint(InsertPointLayout::RawDouble),
    );
    let mut best = None::<DecodedEntity>;
    for entity in [primary, fallback, scale_first, scale_first_raw]
        .into_iter()
        .flatten()
    {
        best = Some(match best {
            Some(current)
                if insert_entity_quality_score(&current)
                    >= insert_entity_quality_score(&entity) =>
            {
                current
            }
            _ => entity,
        });
    }
    best.ok_or("failed to decode insert body")
}

fn read_insert_point(
    object: &mut LsbBitStream<'_>,
    point_layout: InsertPointLayout,
) -> Result<Point3, &'static str> {
    match point_layout {
        InsertPointLayout::BitDouble => Ok(object
            .read_3bit_double()
            .ok_or("failed to read insertion point")?),
        InsertPointLayout::RawDouble => Ok(Point3 {
            x: object
                .read_double()
                .ok_or("failed to read raw insertion point x")?,
            y: object
                .read_double()
                .ok_or("failed to read raw insertion point y")?,
            z: object
                .read_double()
                .ok_or("failed to read raw insertion point z")?,
        }),
    }
}

fn read_insert_scale(object: &mut LsbBitStream<'_>) -> Result<Point3, &'static str> {
    match object.read_2bits().ok_or("failed to read scale flags")? {
        0 => {
            let x = object.read_double().ok_or("failed to read scale.x")?;
            Ok(Point3 {
                x,
                y: object
                    .read_bit_double_with_default(x)
                    .ok_or("failed to read scale.y")?,
                z: object
                    .read_bit_double_with_default(x)
                    .ok_or("failed to read scale.z")?,
            })
        }
        1 => Ok(Point3 {
            x: 1.0,
            y: object
                .read_bit_double_with_default(1.0)
                .ok_or("failed to read unit-scale y")?,
            z: object
                .read_bit_double_with_default(1.0)
                .ok_or("failed to read unit-scale z")?,
        }),
        2 => {
            let xyz = object.read_double().ok_or("failed to read uniform scale")?;
            Ok(Point3 {
                x: xyz,
                y: xyz,
                z: xyz,
            })
        }
        3 => Ok(Point3 {
            x: 1.0,
            y: 1.0,
            z: 1.0,
        }),
        _ => Err("invalid scale flags"),
    }
}

fn decode_insert_body(
    common: DecodedEntityCommon,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
    is_minsert: bool,
    body_layout: InsertBodyLayout,
) -> Result<DecodedEntity, &'static str> {
    let (insertion_point, scale) = match body_layout {
        InsertBodyLayout::PointThenScale(point_layout) => {
            let insertion_point = read_insert_point(&mut object, point_layout)?;
            let scale = read_insert_scale(&mut object)?;
            (insertion_point, scale)
        }
        InsertBodyLayout::ScaleThenPoint(point_layout) => {
            let scale = read_insert_scale(&mut object)?;
            let insertion_point = read_insert_point(&mut object, point_layout)?;
            (insertion_point, scale)
        }
    };
    let rotation_degrees = object.read_bit_double().unwrap_or(0.0).to_degrees();
    let _extrusion = object.read_3bit_double().unwrap_or(Point3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    });
    let has_attributes = object.read_bit().unwrap_or(false);
    let owned_count = if has_attributes {
        object
            .read_bit_long()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
            .min(MAX_OWNED_HANDLES)
    } else {
        0
    };
    let (column_count, row_count, column_spacing, row_spacing) = if is_minsert {
        let column_count = u16::try_from(object.read_bit_short().unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        let row_count = u16::try_from(object.read_bit_short().unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        let column_spacing = object.read_bit_double().unwrap_or(0.0);
        let row_spacing = object.read_bit_double().unwrap_or(0.0);
        if !minsert_array_is_reasonable(column_count, row_count, column_spacing, row_spacing) {
            return Err("unreasonable minsert array parameters");
        }
        (column_count, row_count, column_spacing, row_spacing)
    } else {
        (1, 1, 0.0, 0.0)
    };
    let mut absolute_handles = handles.clone();
    let absolute_block_handle = absolute_handles
        .read_handle_reference(0)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, common.handle));
    let mut relative_handles = handles.clone();
    let relative_block_handle = relative_handles
        .read_handle_reference(common.handle)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, common.handle));
    let block_handle = relative_block_handle.or(absolute_block_handle);
    let alternate_block_handle =
        absolute_block_handle.filter(|handle| Some(*handle) != block_handle);
    handles = relative_handles;
    for _ in 0..owned_count {
        let _owned = handles.read_handle_reference(0);
    }
    if has_attributes {
        let _seqend = handles.read_handle_reference(0);
    }
    Ok(DecodedEntity {
        common,
        kind: DecodedEntityKind::Insert(DecodedInsert {
            block_handle,
            alternate_block_handle,
            block_name: None,
            insertion_point,
            scale,
            rotation_degrees,
            column_count,
            row_count,
            column_spacing,
            row_spacing,
        }),
    })
}

fn decode_lwpolyline(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let common = read_common_entity_data(ctx)?;
    let flags = u16::try_from(ctx.object.read_bit_short()?).ok()?;
    if (flags & 0x4) != 0 {
        let _constant_width = ctx.object.read_bit_double();
    }
    let elevation = if (flags & 0x8) != 0 {
        ctx.object.read_bit_double().unwrap_or(0.0)
    } else {
        0.0
    };
    if (flags & 0x2) != 0 {
        let _thickness = ctx.object.read_bit_double();
    }
    if (flags & 0x1) != 0 {
        let _normal = ctx.object.read_3bit_double();
    }
    let vertex_count = usize::try_from(ctx.object.read_bit_long()?).ok()?;
    let vertex_count = vertex_count.min(MAX_POLYLINE_POINTS);
    let bulge_count = if (flags & 0x10) != 0 {
        usize::try_from(ctx.object.read_bit_long()?).ok()?
    } else {
        0
    }
    .min(MAX_POLYLINE_POINTS);
    let id_count = if (flags & 0x400) != 0 {
        usize::try_from(ctx.object.read_bit_long()?).ok()?
    } else {
        0
    }
    .min(MAX_POLYLINE_POINTS);
    let width_count = if (flags & 0x20) != 0 {
        usize::try_from(ctx.object.read_bit_long()?).ok()?
    } else {
        0
    }
    .min(MAX_POLYLINE_POINTS);
    let mut points = Vec::with_capacity(vertex_count);
    if vertex_count > 0 {
        let mut current = ctx.object.read_2raw_double()?;
        points.push(Point3 {
            x: current.0,
            y: current.1,
            z: elevation,
        });
        for _ in 1..vertex_count {
            current = ctx.object.read_2bit_double_with_default(current)?;
            points.push(Point3 {
                x: current.0,
                y: current.1,
                z: elevation,
            });
        }
    }
    for _ in 0..bulge_count {
        let _bulge = ctx.object.read_bit_double();
    }
    for _ in 0..id_count {
        let _id = ctx.object.read_bit_long();
    }
    for _ in 0..width_count {
        let _start_width = ctx.object.read_bit_double();
        let _end_width = ctx.object.read_bit_double();
    }
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::LwPolyline(DecodedLwPolyline {
            points,
            closed: (flags & 0x200) != 0,
        }),
    })
}

fn decode_polyline2d(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    let prefix = read_common_entity_prefix_detailed(ctx).ok()?;
    let _has_vertex = ctx.object.read_bit().unwrap_or(false);
    let owned_count = ctx
        .object
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
        .min(MAX_OWNED_HANDLES);
    let flags = u16::try_from(ctx.object.read_bit_short()?).ok()?;
    let _curve_type = ctx.object.read_bit_short().unwrap_or_default();
    let _start_width = ctx.object.read_bit_double().unwrap_or(0.0);
    let _end_width = ctx.object.read_bit_double().unwrap_or(0.0);
    let _thickness = ctx.object.read_bit_thickness().unwrap_or(0.0);
    let _elevation = ctx.object.read_bit_double().unwrap_or(0.0);
    let _normal = ctx.object.read_bit_extrusion();
    let common = finish_entity_data(ctx, prefix, false).ok()?;
    let _first_vertex = ctx.handles.read_handle_reference(0);
    let _last_vertex = ctx.handles.read_handle_reference(0);
    let mut owned_handles = Vec::with_capacity(owned_count);
    for _ in 0..owned_count {
        if let Some(owned) = read_owned_polyline_handle(&mut ctx.handles, common.handle) {
            owned_handles.push(owned);
        }
    }
    let _seqend = ctx.handles.read_handle_reference(0);
    Some(DecodedEntity {
        common,
        kind: DecodedEntityKind::Polyline2D(DecodedPolylineHeader {
            closed: (flags & 0x1) != 0,
            owned_count,
            owned_handles,
        }),
    })
}

fn decode_polyline3d(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntity> {
    decode_polyline3d_detailed(ctx).ok()
}

fn decode_vertex2d(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedVertex> {
    decode_vertex2d_detailed(ctx).ok()
}

fn decode_vertex3d(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedVertex> {
    decode_vertex3d_detailed(ctx).ok()
}

fn decode_polyline3d_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedEntity, &'static str> {
    let prefix = read_common_entity_prefix_detailed(ctx)?;
    let primary = decode_polyline3d_body_specish(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
    );
    let has_vertex_first = decode_polyline3d_body_has_vertex_first(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
    );
    let count_first = decode_polyline3d_body_count_first(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
    );
    let count_first_swapped = decode_polyline3d_body_count_first_swapped(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
    );
    let fallback =
        decode_polyline3d_body_legacy(ctx.version, prefix, ctx.object.clone(), ctx.handles.clone());
    let mut best = None::<DecodedEntity>;
    for entity in [
        primary,
        has_vertex_first,
        count_first,
        count_first_swapped,
        fallback,
    ]
    .into_iter()
    .flatten()
    {
        best = Some(match best {
            Some(current_best) => prefer_better_polyline_entity(current_best, entity),
            None => entity,
        });
    }
    best.ok_or("failed to decode polyline3d body")
}

fn prefer_better_polyline_entity(primary: DecodedEntity, fallback: DecodedEntity) -> DecodedEntity {
    let primary_score = polyline_entity_quality_score(&primary);
    let fallback_score = polyline_entity_quality_score(&fallback);
    if fallback_score > primary_score {
        fallback
    } else {
        primary
    }
}

fn polyline_entity_quality_score(entity: &DecodedEntity) -> i64 {
    let (owned_count, resolved_handles) = match &entity.kind {
        DecodedEntityKind::Polyline2D(header) | DecodedEntityKind::Polyline3D(header) => {
            (header.owned_count, header.owned_handles.len())
        }
        _ => return i64::MIN,
    };
    let sane_bonus = if polyline_owned_count_is_sane(owned_count) {
        20_000
    } else {
        0
    };
    let absurd_penalty = if owned_count >= 4_096 { 50_000 } else { 0 };
    let resolved_bonus = i64::try_from(resolved_handles.min(1024)).unwrap_or(0) * 16;
    let count_penalty = i64::try_from(owned_count.min(MAX_OWNED_HANDLES)).unwrap_or(i64::MAX / 4);
    sane_bonus + resolved_bonus - count_penalty - absurd_penalty
}

fn decode_polyline3d_body_specish(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
) -> Result<DecodedEntity, &'static str> {
    let _curve_type = object
        .read_byte()
        .ok_or("failed to read polyline3d curve type")?;
    let flags = object
        .read_byte()
        .ok_or("failed to read polyline3d flags")?;
    let owned_count = if is_r2004_plus(version) {
        object
            .read_bit_long()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
            .min(MAX_OWNED_HANDLES)
    } else {
        0
    };
    let common = finish_entity_data_from_handles(version, &mut handles, prefix, false)?;
    let _first_vertex = handles.read_handle_reference(0);
    let _last_vertex = handles.read_handle_reference(0);
    let mut owned_handles = Vec::with_capacity(owned_count);
    for _ in 0..owned_count {
        if let Some(owned) = read_owned_polyline_handle(&mut handles, common.handle) {
            owned_handles.push(owned);
        }
    }
    let _seqend = handles.read_handle_reference(0);
    Ok(DecodedEntity {
        common,
        kind: DecodedEntityKind::Polyline3D(DecodedPolylineHeader {
            closed: (flags & 0x1) != 0,
            owned_count,
            owned_handles,
        }),
    })
}

fn decode_polyline3d_body_legacy(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
) -> Result<DecodedEntity, &'static str> {
    let flags0 = object
        .read_byte()
        .ok_or("failed to read polyline3d legacy flags0")?;
    let flags1 = object
        .read_byte()
        .ok_or("failed to read polyline3d legacy flags1")?;
    let owned_count = object
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
        .min(MAX_OWNED_HANDLES);
    let common = finish_entity_data_from_handles(version, &mut handles, prefix, false)?;
    let mut owned_handles = Vec::with_capacity(owned_count);
    for _ in 0..owned_count {
        if let Some(owned) = read_owned_polyline_handle(&mut handles, common.handle) {
            owned_handles.push(owned);
        }
    }
    let _seqend = handles.read_handle_reference(0);
    Ok(DecodedEntity {
        common,
        kind: DecodedEntityKind::Polyline3D(DecodedPolylineHeader {
            closed: (flags1 & 0x1) != 0 || (flags0 & 0x8) != 0,
            owned_count,
            owned_handles,
        }),
    })
}

fn decode_polyline3d_body_has_vertex_first(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
) -> Result<DecodedEntity, &'static str> {
    let _has_vertex = object
        .read_bit()
        .ok_or("failed to read polyline3d has-vertex flag")?;
    let _curve_type = object
        .read_byte()
        .ok_or("failed to read polyline3d curve type")?;
    let flags = object
        .read_byte()
        .ok_or("failed to read polyline3d flags")?;
    let owned_count = if is_r2004_plus(version) {
        object
            .read_bit_long()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
            .min(MAX_OWNED_HANDLES)
    } else {
        0
    };
    decode_polyline3d_with_owned_handles(version, prefix, &mut handles, flags, owned_count)
}

fn decode_polyline3d_body_count_first(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
) -> Result<DecodedEntity, &'static str> {
    let owned_count = if is_r2004_plus(version) {
        object
            .read_bit_long()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
            .min(MAX_OWNED_HANDLES)
    } else {
        0
    };
    let first = object
        .read_byte()
        .ok_or("failed to read polyline3d count-first byte0")?;
    let second = object
        .read_byte()
        .ok_or("failed to read polyline3d count-first byte1")?;
    let (flags, _curve_type) = if second <= 7 {
        (first, second)
    } else {
        (second, first)
    };
    decode_polyline3d_with_owned_handles(version, prefix, &mut handles, flags, owned_count)
}

fn decode_polyline3d_body_count_first_swapped(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
) -> Result<DecodedEntity, &'static str> {
    let count_word = object
        .read_bit_short()
        .and_then(|count| u16::try_from(count).ok())
        .map(usize::from)
        .unwrap_or(0)
        .min(MAX_OWNED_HANDLES);
    let flags = object
        .read_byte()
        .ok_or("failed to read polyline3d swapped flags")?;
    let _curve_type = object
        .read_byte()
        .ok_or("failed to read polyline3d swapped curve type")?;
    decode_polyline3d_with_owned_handles(version, prefix, &mut handles, flags, count_word)
}

fn decode_polyline3d_with_owned_handles(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    handles: &mut LsbBitStream<'_>,
    flags: u8,
    owned_count: usize,
) -> Result<DecodedEntity, &'static str> {
    let common = finish_entity_data_from_handles(version, handles, prefix, false)?;
    let _first_vertex = handles.read_handle_reference(0);
    let _last_vertex = handles.read_handle_reference(0);
    let mut owned_handles = Vec::with_capacity(owned_count);
    for _ in 0..owned_count {
        if let Some(owned) = read_owned_polyline_handle(handles, common.handle) {
            owned_handles.push(owned);
        }
    }
    let _seqend = handles.read_handle_reference(0);
    Ok(DecodedEntity {
        common,
        kind: DecodedEntityKind::Polyline3D(DecodedPolylineHeader {
            closed: (flags & 0x1) != 0,
            owned_count,
            owned_handles,
        }),
    })
}

#[cfg(test)]
fn debug_polyline3d_handle_sequence(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut handles: LsbBitStream<'_>,
    owned_count: usize,
) -> String {
    let common = finish_entity_data_from_handles(version, &mut handles, prefix, false);
    let first_vertex = handles.read_handle_reference(0);
    let last_vertex = handles.read_handle_reference(0);
    let mut owned = Vec::new();
    for _ in 0..owned_count.min(32) {
        owned.push(handles.read_handle_reference(0));
    }
    let seqend = handles.read_handle_reference(0);
    format!(
        "common={common:?} first={first_vertex:?} last={last_vertex:?} owned={owned:?} seqend={seqend:?}"
    )
}

fn decode_vertex2d_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedVertex, &'static str> {
    let prefix = read_vertex_entity_prefix_detailed(ctx)?;
    let _flags = ctx
        .object
        .read_byte()
        .ok_or("failed to read vertex2d flags")?;
    let point = ctx
        .object
        .read_3bit_double()
        .ok_or("failed to read vertex2d point")?;
    let start_width = ctx.object.read_bit_double().unwrap_or(0.0);
    if start_width >= 0.0 {
        let _end_width = ctx.object.read_bit_double();
    }
    let _bulge = ctx.object.read_bit_double();
    let _id = ctx.object.read_bit_long();
    let _tangent = ctx.object.read_bit_double();
    let common = finish_entity_data(ctx, prefix, true)?;
    Ok(DecodedVertex {
        handle: common.handle,
        owner_handle: common.owner_handle,
        point,
    })
}

fn decode_vertex3d_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedVertex, &'static str> {
    let prefix = read_vertex_entity_prefix_detailed(ctx)?;
    let primary = decode_vertex3d_body(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
        VertexPointLayout::BitDouble,
    );
    let fallback = decode_vertex3d_body(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
        VertexPointLayout::RawDouble,
    );
    let point_first = decode_vertex3d_body_point_first(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
        VertexPointLayout::BitDouble,
    );
    let raw_point_first = decode_vertex3d_body_point_first(
        ctx.version,
        prefix,
        ctx.object.clone(),
        ctx.handles.clone(),
        VertexPointLayout::RawDouble,
    );
    static DBG_VTX: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if std::env::var_os("DWG_PROFILE").is_some()
        && DBG_VTX.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5
    {
        eprintln!(
            "dwg decode: vertex3d handle={:#x} owner={:?} point={:?}",
            ctx.expected_handle,
            primary.as_ref().map(|v| v.owner_handle),
            primary.as_ref().map(|v| v.point),
        );
        eprintln!(
            "dwg decode: vertex3d handle={:#x} obj_start_bits={} obj_end_bits={} type={:#x} primary={:?} fallback={:?} point_first={:?} raw_point_first={:?}",
            ctx.expected_handle,
            ctx.object.bit_index,
            ctx.object_end_bits,
            ctx.object_type,
            primary.as_ref().map(|v| v.point),
            fallback.as_ref().map(|v| v.point),
            point_first.as_ref().map(|v| v.point),
            raw_point_first.as_ref().map(|v| v.point),
        );
    }
    let mut best = None::<DecodedVertex>;
    for vertex in [primary, fallback, point_first, raw_point_first]
        .into_iter()
        .flatten()
    {
        best = Some(match best {
            Some(current_best) => prefer_better_vertex(current_best, vertex),
            None => vertex,
        });
    }
    best.ok_or("failed to decode vertex3d body")
}

fn decode_vertex3d_body(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
    layout: VertexPointLayout,
) -> Result<DecodedVertex, &'static str> {
    let _flags = object.read_byte().ok_or("failed to read vertex3d flags")?;
    let point = match layout {
        VertexPointLayout::BitDouble => object
            .read_3bit_double()
            .ok_or("failed to read vertex3d point")?,
        VertexPointLayout::RawDouble => Point3 {
            x: object
                .read_double()
                .ok_or("failed to read vertex3d raw x")?,
            y: object
                .read_double()
                .ok_or("failed to read vertex3d raw y")?,
            z: object
                .read_double()
                .ok_or("failed to read vertex3d raw z")?,
        },
    };
    let common = finish_entity_data_from_handles(version, &mut handles, prefix, true)?;
    Ok(DecodedVertex {
        handle: common.handle,
        owner_handle: common.owner_handle,
        point,
    })
}

fn decode_vertex3d_body_point_first(
    version: CadVersion,
    prefix: DecodedEntityPrefix,
    mut object: LsbBitStream<'_>,
    mut handles: LsbBitStream<'_>,
    layout: VertexPointLayout,
) -> Result<DecodedVertex, &'static str> {
    let point = match layout {
        VertexPointLayout::BitDouble => object
            .read_3bit_double()
            .ok_or("failed to read vertex3d point")?,
        VertexPointLayout::RawDouble => Point3 {
            x: object
                .read_double()
                .ok_or("failed to read vertex3d raw x")?,
            y: object
                .read_double()
                .ok_or("failed to read vertex3d raw y")?,
            z: object
                .read_double()
                .ok_or("failed to read vertex3d raw z")?,
        },
    };
    let _flags = object.read_byte().ok_or("failed to read vertex3d flags")?;
    let common = finish_entity_data_from_handles(version, &mut handles, prefix, true)?;
    Ok(DecodedVertex {
        handle: common.handle,
        owner_handle: common.owner_handle,
        point,
    })
}

fn prefer_better_vertex(primary: DecodedVertex, fallback: DecodedVertex) -> DecodedVertex {
    let primary_score = vertex_quality_score(&primary);
    let fallback_score = vertex_quality_score(&fallback);
    if fallback_score > primary_score {
        fallback
    } else {
        primary
    }
}

fn vertex_quality_score(vertex: &DecodedVertex) -> i64 {
    let owner_bonus = i64::from(vertex.owner_handle.is_some()) * 256;
    let plausible_bonus = i64::from(point_has_plausible_horizontal_cad_scale(vertex.point)) * 4_096;
    let elevation_bonus = i64::from(point_has_plausible_survey_elevation(vertex.point)) * 1_024;
    let point_bonus = i64::from(point_is_reasonable(vertex.point)) * 1_024;
    owner_bonus + plausible_bonus + elevation_bonus + point_bonus
}

fn read_owned_polyline_handle(handles: &mut LsbBitStream<'_>, current_handle: u64) -> Option<u64> {
    let mut absolute_handles = handles.clone();
    let absolute = absolute_handles
        .read_handle_reference(0)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, current_handle));
    let mut relative_handles = handles.clone();
    let relative = relative_handles
        .read_handle_reference(current_handle)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, current_handle));
    *handles = relative_handles;
    relative.or(absolute)
}

fn skip_common_non_entity_data_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<u64, &'static str> {
    let handle = read_common_data_detailed(ctx)?;
    let reactor_count = ctx
        .object
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
        .min(MAX_REACTORS);
    let xdict_missing = if is_r2004_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(true)
    } else {
        false
    };
    if is_r2013_plus(ctx.version) {
        let _has_ds_data = ctx.object.read_bit();
    }
    let _owner = ctx.handles.read_handle_reference(handle);
    for _ in 0..reactor_count {
        let _reactor = ctx.handles.read_handle_reference(0);
    }
    if !xdict_missing {
        let _xdict = ctx.handles.read_handle_reference(0);
    }
    Ok(handle)
}

fn read_common_table_data(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedCommonTableData> {
    read_common_table_data_detailed(ctx).ok()
}

fn read_common_table_data_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedCommonTableData, &'static str> {
    let handle = skip_common_non_entity_data_detailed(ctx)?;
    let text_name = ctx
        .text
        .read_variable_text(ctx.version)
        .filter(|name| text_stream_candidate_score(name).is_some());
    // R2007+ stores string data in a separate text stream; the object data stream
    // does NOT contain string fields. For pre-R2007, strings are inline in the object
    // stream and must be consumed.
    let object_name = if !is_r2007_plus(ctx.version) {
        let mut object_reader = ctx.object.clone();
        let name = object_reader
            .read_variable_text(ctx.version)
            .filter(|name| text_stream_candidate_score(name).is_some());
        if name.is_some() {
            ctx.object = object_reader;
        }
        name
    } else {
        None
    };
    let name = match (text_name, object_name) {
        (Some(text_name), Some(object_name))
            if is_generic_table_space_name(&text_name)
                && !is_generic_table_space_name(&object_name) =>
        {
            object_name
        }
        (Some(text_name), _) => text_name,
        (None, Some(object_name)) => object_name,
        (None, None) => {
            if DEBUG_COMMON_TABLE_NAME_FAILURES {
                // Try reading raw name without score filter to see what's in the object stream
                let mut raw_reader = ctx.object.clone();
                let raw_name = raw_reader.read_variable_text(ctx.version);
                eprintln!(
                    "native dwg: failed common table name handle={:X} text_bit={} object_bit={} handles_bit={} raw_obj_name={:?}",
                    ctx.expected_handle,
                    ctx.text.bit_index,
                    ctx.object.bit_index,
                    ctx.handles.bit_index,
                    raw_name,
                );
            }
            return Err("failed to read common table name");
        }
    };
    // xref fields: consume bits for alignment but discard values
    // (object stream bit position is unreliable for these fields)
    if is_r2004_plus(ctx.version) {
        let _xref_dep = ctx.object.read_bit();
        let _xref_index = ctx.object.read_bit_short();
        let _xref = ctx.handles.read_handle_reference(0);
    } else {
        let _is_xref_ref = ctx.object.read_bit();
        let _is_xref_resolved = ctx.object.read_bit_short();
        let _is_xref_dep = ctx.object.read_bit();
        let _xref = ctx.handles.read_handle_reference(0);
    }
    Ok(DecodedCommonTableData { handle, name })
}

fn read_common_entity_data(ctx: &mut ObjectDecodeContext<'_>) -> Option<DecodedEntityCommon> {
    read_common_entity_data_detailed(ctx).ok()
}

fn read_common_entity_data_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedEntityCommon, &'static str> {
    let prefix = read_common_entity_prefix_detailed(ctx)?;
    finish_entity_data(ctx, prefix, false)
}

fn skip_eed(object: &mut LsbBitStream<'_>, max_bits: usize) {
    // EED: repeated BS(size) + app_handle + data blocks until size==0
    let start = object.bit_index;
    loop {
        if object.bit_index.saturating_sub(start) > max_bits {
            break;
        }
        let size = match object.read_bit_short() {
            Some(s) if s > 0 && (s as usize) < 4096 => s as usize,
            _ => break,
        };
        // Skip application handle (H)
        let _ = object.read_handle_reference(0);
        // Skip EED data bytes
        for _ in 0..size {
            let _ = object.read_u8();
        }
    }
}

fn read_common_entity_prefix_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedEntityPrefix, &'static str> {
    let handle = ctx.expected_handle;
    let entity_mode = ctx.object.read_2bits().unwrap_or(MODEL_SPACE_MODE);
    let reactor_count = ctx
        .object
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
        .min(MAX_REACTORS);
    let mut line_type_flags = 0;
    let xdict_missing = if is_r2004_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(true)
    } else {
        false
    };
    let nolinks = if !is_r2004_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(false)
    } else {
        false
    };
    if is_r2013_plus(ctx.version) {
        let _has_ds_binary_data = ctx.object.read_bit();
    }
    let _is_book_color = skip_entity_color_detailed(ctx)?;
    // R2004+: color book handle is in the handle stream, not the object data stream.
    // Do not read a handle reference from ctx.object here.
    let _line_type_scale = ctx.object.read_bit_double().unwrap_or(1.0);
    if is_r2004_plus(ctx.version) {
        line_type_flags = ctx.object.read_2bits().unwrap_or_default();
    }
    let plotstyle_flags = if is_r2004_plus(ctx.version) {
        ctx.object.read_2bits().unwrap_or_default()
    } else {
        0
    };
    let material_flags = if is_r2007_plus(ctx.version) {
        ctx.object.read_2bits().unwrap_or_default()
    } else {
        0
    };
    let shadow_flags = if is_r2007_plus(ctx.version) {
        ctx.object.read_byte().unwrap_or_default()
    } else {
        0
    };
    let has_full_visual_style = if is_r2010_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(false)
    } else {
        false
    };
    let has_face_visual_style = if is_r2010_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(false)
    } else {
        false
    };
    let has_edge_visual_style = if is_r2010_plus(ctx.version) {
        ctx.object.read_bit().unwrap_or(false)
    } else {
        false
    };
    let _invisible = ctx.object.read_bit_short();
    let _lineweight = ctx.object.read_byte();
    Ok(DecodedEntityPrefix {
        handle,
        entity_mode,
        reactor_count,
        xdict_missing,
        nolinks,
        line_type_flags,
        plotstyle_flags,
        material_flags,
        shadow_flags,
        has_full_visual_style,
        has_face_visual_style,
        has_edge_visual_style,
    })
}

fn read_vertex_entity_prefix_detailed(
    ctx: &mut ObjectDecodeContext<'_>,
) -> Result<DecodedEntityPrefix, &'static str> {
    read_common_entity_prefix_detailed(ctx)
}

fn finish_entity_data(
    ctx: &mut ObjectDecodeContext<'_>,
    prefix: DecodedEntityPrefix,
    force_owner_handle: bool,
) -> Result<DecodedEntityCommon, &'static str> {
    finish_entity_data_from_handles(ctx.version, &mut ctx.handles, prefix, force_owner_handle)
}

fn finish_entity_data_from_handles(
    version: CadVersion,
    handles: &mut LsbBitStream<'_>,
    prefix: DecodedEntityPrefix,
    force_owner_handle: bool,
) -> Result<DecodedEntityCommon, &'static str> {
    let owner_handle = if force_owner_handle || prefix.entity_mode == NO_OWNER_MODE {
        let mut absolute_owner_handles = handles.clone();
        let raw_absolute = absolute_owner_handles.read_handle_reference(0);
        let absolute_owner_handle = raw_absolute
            .filter(|handle| decoded_related_handle_is_reasonable(*handle, prefix.handle));
        let mut relative_owner_handles = handles.clone();
        let raw_relative = relative_owner_handles.read_handle_reference(prefix.handle);
        let relative_owner_handle = raw_relative
            .filter(|handle| decoded_related_handle_is_reasonable(*handle, prefix.handle));
        *handles = relative_owner_handles;
        relative_owner_handle.or(absolute_owner_handle)
    } else {
        None
    };
    for _ in 0..prefix.reactor_count {
        let _reactor = handles.read_handle_reference(0);
    }
    if !prefix.xdict_missing {
        let _xdict = handles.read_handle_reference(0);
    }
    let mut absolute_handles = handles.clone();
    let absolute_layer_handle = absolute_handles
        .read_handle_reference(0)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, prefix.handle));
    let mut relative_handles = handles.clone();
    let relative_layer_handle = relative_handles
        .read_handle_reference(prefix.handle)
        .filter(|handle| decoded_related_handle_is_reasonable(*handle, prefix.handle));
    let layer_handle = relative_layer_handle.or(absolute_layer_handle);
    let alternate_layer_handle = absolute_layer_handle.filter(|value| Some(*value) != layer_handle);
    *handles = relative_handles;
    if prefix.line_type_flags == 3 {
        let _linetype = handles.read_handle_reference(0);
    }
    if !is_r2004_plus(version) && !prefix.nolinks {
        let _prev_entity = handles.read_handle_reference(0);
        let _next_entity = handles.read_handle_reference(0);
    }
    if is_r2007_plus(version) && prefix.material_flags == 3 {
        let _material = handles.read_handle_reference(0);
    }
    if is_r2007_plus(version) && prefix.shadow_flags == 3 {
        let _shadow = handles.read_handle_reference(0);
    }
    if prefix.plotstyle_flags == 3 {
        let _plotstyle = handles.read_handle_reference(0);
    }
    if is_r2010_plus(version) {
        if prefix.has_full_visual_style {
            let _full_visual_style = handles.read_handle_reference(0);
        }
        if prefix.has_face_visual_style {
            let _face_visual_style = handles.read_handle_reference(0);
        }
        if prefix.has_edge_visual_style {
            let _edge_visual_style = handles.read_handle_reference(0);
        }
    }
    Ok(DecodedEntityCommon {
        handle: prefix.handle,
        owner_handle,
        layer_handle,
        alternate_layer_handle,
        entity_mode: prefix.entity_mode,
    })
}

fn read_common_data_detailed(ctx: &mut ObjectDecodeContext<'_>) -> Result<u64, &'static str> {
    const COMMON_HANDLE_SEARCH_BITS: usize = 128;
    let search_end_bit = ctx.object_end_bits.min(
        ctx.object
            .bit_index
            .saturating_add(COMMON_HANDLE_SEARCH_BITS),
    );

    let mut handle_probe = ctx.object.clone();
    if handle_probe.read_handle_reference(0) == Some(ctx.expected_handle) {
        ctx.object.bit_index = handle_probe.bit_index;
    } else {
        let mut relative_handle_probe = ctx.object.clone();
        if relative_handle_probe.read_handle_reference(ctx.expected_handle)
            == Some(ctx.expected_handle)
        {
            ctx.object.bit_index = relative_handle_probe.bit_index;
        } else if let Some(handle_end_bit) = search_self_handle_lsb(
            ctx.object.bytes,
            ctx.object.bit_index,
            search_end_bit,
            ctx.expected_handle,
        ) {
            ctx.object.bit_index = handle_end_bit;
        } else {
            return Err("failed to read common stream handle");
        }
    }
    skip_extended_data_detailed(ctx)?;
    Ok(ctx.expected_handle)
}

fn skip_extended_data_detailed(ctx: &mut ObjectDecodeContext<'_>) -> Result<(), &'static str> {
    let start_bit = ctx.object.bit_index;
    let mut probe = ctx.object.clone();
    let Some(mut segment_size) = probe.read_bit_short() else {
        return Ok(());
    };
    let mut segment_count = 0usize;
    while segment_size != 0 && segment_count < MAX_XDATA_SEGMENTS {
        let Some(size_bytes) = usize::try_from(segment_size).ok() else {
            ctx.object.bit_index = start_bit;
            return Ok(());
        };
        if size_bytes > MAX_PREVIEW_BYTES
            || probe.read_handle_reference(0).is_none()
            || probe.advance_bytes(size_bytes).is_none()
            || probe.bit_index > ctx.object_end_bits
        {
            ctx.object.bit_index = start_bit;
            return Ok(());
        }
        let Some(next_size) = probe.read_bit_short() else {
            ctx.object.bit_index = start_bit;
            return Ok(());
        };
        if probe.bit_index > ctx.object_end_bits {
            ctx.object.bit_index = start_bit;
            return Ok(());
        }
        segment_size = next_size;
        segment_count += 1;
    }
    if segment_size != 0 || segment_count >= MAX_XDATA_SEGMENTS {
        ctx.object.bit_index = start_bit;
        return Ok(());
    }
    ctx.object = probe;
    Ok(())
}

fn skip_entity_color_detailed(ctx: &mut ObjectDecodeContext<'_>) -> Result<bool, &'static str> {
    let raw = ctx
        .object
        .read_bit_short()
        .map(|value| value as u16)
        .unwrap_or(0);
    if raw == 0 {
        return Ok(false);
    }
    let flags = raw >> 8;
    if (flags & 0x20) != 0 {
        let _transparency = ctx.object.read_bit_long();
    }
    if (flags & 0x0040) != 0 {
        // R2004+: color book handle is read from the handle stream, not the object data stream.
        // Do not read handle from ctx.object here.
    } else if (flags & 0x0080) != 0 {
        let _rgb = ctx.object.read_bit_long();
    }
    if (flags & 0x0041) == 0x0041 {
        let _color_name = ctx.text.read_variable_text(ctx.version);
    }
    if (flags & 0x0042) == 0x0042 {
        let _book_name = ctx.text.read_variable_text(ctx.version);
    }
    Ok((flags & 0x0040) != 0)
}

struct ObjectDecodeContext<'a> {
    version: CadVersion,
    expected_handle: u64,
    object_end_bits: usize,
    object: LsbBitStream<'a>,
    text: BitStream<'a>,
    handles: LsbBitStream<'a>,
    object_type: u32,
}

impl<'a> ObjectDecodeContext<'a> {
    fn new(
        version: CadVersion,
        objects: &'a [u8],
        expected_handle: u64,
        start: ModernObjectStart,
    ) -> Self {
        Self::new_with_text_search_bits(version, objects, expected_handle, start, None)
    }

    fn new_with_text_search_bits(
        version: CadVersion,
        objects: &'a [u8],
        expected_handle: u64,
        start: ModernObjectStart,
        forced_text_search_bits: Option<usize>,
    ) -> Self {
        // MS(size) counts bytes starting AFTER the MC field (object_stream_start_bits),
        // not from data_start_bits (which is right after MS, before MC).
        let object_end_bits = start
            .object_stream_start_bits
            .saturating_add(start.size_bytes.saturating_mul(8));
        let (object, object_type, object_data_end_bits) = if let Some(body_start_bits) =
            start.body_start_bits
        {
            (
                LsbBitStream::new(objects, body_start_bits),
                start.object_type,
                start
                    .object_data_end_bits
                    .filter(|bits| *bits <= object_end_bits),
            )
        } else {
            let mut object = LsbBitStream::new(objects, start.object_stream_start_bits);
            let object_type = object.read_object_type().unwrap_or(start.object_type);
            // After the type: self-handle (H), EED, graphic present, then entity data.
            let _self_handle = object.read_handle_reference(0);
            skip_eed(&mut object, 4096);
            // Only entities have the graphic_present field.
            // Non-entity objects (BLOCK_HEADER, LAYER, control objects, etc.) skip directly to data.
            // Only apply this check when we have reliable type info (handle_stream_bits > 0).
            // The without_handle_stream candidate misreads the MC byte as the type, so we
            // must fall back to always reading graphic_present for it (matching old behavior).
            let type_is_reliable = start.handle_stream_bits > 0 || start.body_start_bits.is_some();
            let skip_graphic = type_is_reliable && is_non_entity_object_type(object_type);
            if is_r2004_plus(version) && !skip_graphic {
                let graphic_present = object.read_bit().unwrap_or(false);
                if graphic_present {
                    if let Some(graphics_size) = object.read_bit_long() {
                        let skip_bytes = usize::try_from(graphics_size)
                            .unwrap_or(0)
                            .min(object_end_bits.saturating_sub(object.bit_index) / 8);
                        for _ in 0..skip_bytes {
                            let _ = object.read_u8();
                        }
                    }
                }
            }
            let object_data_end_bits: Option<usize> = None;
            (object, object_type, object_data_end_bits)
        };
        let mc_handle_section_offset =
            object_end_bits.saturating_sub(usize::try_from(start.handle_stream_bits).unwrap_or(0));
        let bounded_handle_section_offset = start
            .object_data_end_bits
            .unwrap_or(object_end_bits)
            .saturating_sub(usize::try_from(start.handle_stream_bits).unwrap_or(0));
        // For R2010+, prefer RL-derived handle position; fall back to MC-derived
        let handle_section_offset = if let Some(rl_end) = object_data_end_bits {
            rl_end
        } else if start.body_start_bits.is_some() {
            bounded_handle_section_offset
        } else {
            mc_handle_section_offset
        };
        let _object_data_end_bits =
            object_data_end_bits.filter(|bits| bits.abs_diff(handle_section_offset) <= 64);
        let handles = LsbBitStream::new(objects, handle_section_offset);
        let text_search_bits = forced_text_search_bits.unwrap_or(0);
        // Text flag is at handle_section_offset - 1 (the bit right before the handle stream).
        let text_flag_offset = handle_section_offset.saturating_sub(1);
        let text = text_stream_for_object(
            objects,
            start.data_start_bits,
            object_end_bits,
            handle_section_offset,
            text_flag_offset,
            version,
            text_search_bits,
        );
        Self {
            version,
            expected_handle,
            object_end_bits,
            object,
            text,
            handles,
            object_type,
        }
    }

    fn object_type(&self) -> Option<u32> {
        (self.object_type != u32::MAX).then_some(self.object_type)
    }
}

fn search_self_handle_lsb(
    bytes: &[u8],
    start_bit: usize,
    end_bit: usize,
    expected_handle: u64,
) -> Option<usize> {
    (start_bit..=end_bit).find_map(|bit_index| {
        let mut absolute_reader = LsbBitStream::new(bytes, bit_index);
        if absolute_reader.read_handle_reference(0) == Some(expected_handle) {
            return Some(absolute_reader.bit_index);
        }
        let mut relative_reader = LsbBitStream::new(bytes, bit_index);
        (relative_reader.read_handle_reference(expected_handle) == Some(expected_handle))
            .then_some(relative_reader.bit_index)
    })
}

fn search_self_handle_msb(
    bytes: &[u8],
    start_bit: usize,
    end_bit: usize,
    expected_handle: u64,
) -> Option<usize> {
    (start_bit..=end_bit).find_map(|bit_index| {
        let mut absolute_reader = BitStream::new(bytes, bit_index);
        if absolute_reader.read_handle_reference(0) == Some(expected_handle) {
            return Some(absolute_reader.bit_index);
        }
        let mut relative_reader = BitStream::new(bytes, bit_index);
        (relative_reader.read_handle_reference(expected_handle) == Some(expected_handle))
            .then_some(relative_reader.bit_index)
    })
}

#[derive(Clone, Copy)]
struct ModernObjectStart {
    _record_start_bits: usize,
    /// Bit position right after the MS field; size_bytes count from here.
    data_start_bits: usize,
    object_stream_start_bits: usize,
    size_bytes: usize,
    handle_stream_bits: u64,
    object_type: u32,
    body_start_bits: Option<usize>,
    object_data_end_bits: Option<usize>,
}

#[cfg(test)]
pub(crate) fn debug_object_start_candidates(
    _version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    classes: &[DwgClassSummary],
    handle: u64,
) -> Vec<(usize, usize, usize, u64, u32, Option<String>)> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let Some((_, _raw_offset_bits)) = handles
        .iter()
        .find(|(entry_handle, _)| *entry_handle == handle)
    else {
        return Vec::new();
    };
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(handle_map, object_index, handle)
    else {
        return Vec::new();
    };
    exact_supported_object_candidates(
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    )
    .into_iter()
    .map(|candidate| {
        (
            candidate._record_start_bits,
            candidate.object_stream_start_bits,
            candidate.size_bytes,
            candidate.handle_stream_bits,
            candidate.object_type,
            object_type_name(candidate.object_type, classes),
        )
    })
    .collect()
}

#[cfg(test)]
type DebugScoredCandidate = (usize, usize, u32, Option<String>, String, Option<i64>);

#[cfg(test)]
pub(crate) fn debug_scored_candidates(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
    handle: u64,
) -> Vec<DebugScoredCandidate> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let Some((_, raw_offset_bits)) = handles
        .iter()
        .find(|(entry_handle, _)| *entry_handle == handle)
    else {
        return Vec::new();
    };
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(handle_map, object_index, handle)
    else {
        return Vec::new();
    };
    direct_object_start_for_handle(
        version,
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    )
    .into_iter()
    .filter_map(|candidate| {
        let object = attempt_supported_object_decode(
            version, objects, handle, candidate, classes, type_hints,
        )?;
        let kind = match &object {
            SupportedObject::Layer(_) => "Layer",
            SupportedObject::BlockHeader(_) => "BlockHeader",
            SupportedObject::Entity(DecodedEntity { kind, .. }) => match kind {
                DecodedEntityKind::Line(_) => "Line",
                DecodedEntityKind::Arc(_) => "Arc",
                DecodedEntityKind::Circle(_) => "Circle",
                DecodedEntityKind::Face3D(_) => "Face3D",
                DecodedEntityKind::Insert(_) => "Insert",
                DecodedEntityKind::LwPolyline(_) => "LwPolyline",
                DecodedEntityKind::Polyline2D(_) => "Polyline2D",
                DecodedEntityKind::Polyline3D(_) => "Polyline3D",
            },
            SupportedObject::Vertex(_) => "Vertex",
            SupportedObject::SeqEnd => "SeqEnd",
            SupportedObject::Ignored => "Ignored",
        };
        let score =
            supported_object_candidate_score(&object, candidate, *raw_offset_bits, type_hints);
        Some((
            candidate._record_start_bits,
            candidate.object_stream_start_bits,
            candidate.object_type,
            object_type_name(candidate.object_type, classes),
            kind.to_string(),
            score,
        ))
    })
    .collect()
}

#[cfg(test)]
pub(crate) fn debug_forced_vertex_probe(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    handle: u64,
) -> Option<String> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let (preferred_offset_bits, alternate_offset_bits) =
        exact_handle_offsets(handle_map, object_index, handle)?;
    let start = direct_object_start_for_handle(
        version,
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    )?;
    let vertex = attempt_vertex_decode_at_start(version, objects, handle, start);
    Some(format!(
        "start={:?} forced_vertex={vertex:?}",
        (
            start._record_start_bits,
            start.object_stream_start_bits,
            start.size_bytes,
            start.handle_stream_bits,
            start.object_type,
            start.body_start_bits,
            start.object_data_end_bits,
        )
    ))
}

#[cfg(test)]
pub(crate) fn debug_exact_offset_vertex_candidates(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    preferred_offset_bits: usize,
    alternate_offset_bits: Option<usize>,
    next_offset_bits: Option<usize>,
) -> Vec<String> {
    [Some(preferred_offset_bits), alternate_offset_bits]
        .into_iter()
        .flatten()
        .flat_map(|offset_bits| exact_offset_start_candidates(objects, handle, offset_bits, next_offset_bits))
        .map(|start| {
            let raw_vertex3d = {
                let mut ctx = ObjectDecodeContext::new(version, objects, handle, start);
                decode_vertex3d(&mut ctx)
            };
            let raw_vertex2d = {
                let mut ctx = ObjectDecodeContext::new(version, objects, handle, start);
                decode_vertex2d(&mut ctx)
            };
            let skip_flag_3bd = {
                let mut ctx = ObjectDecodeContext::new(version, objects, handle, start);
                read_vertex_entity_prefix_detailed(&mut ctx)
                    .ok()
                    .and_then(|_| ctx.object.read_3bit_double())
            };
            let skip_flag_raw = {
                let mut ctx = ObjectDecodeContext::new(version, objects, handle, start);
                read_vertex_entity_prefix_detailed(&mut ctx)
                    .ok()
                    .and_then(|_| {
                        Some(Point3 {
                            x: ctx.object.read_double()?,
                            y: ctx.object.read_double()?,
                            z: ctx.object.read_double()?,
                        })
                    })
            };
            let vertex = attempt_vertex_decode_at_start(version, objects, handle, start);
            format!(
                "start={:?} vertex={vertex:?} raw3d={raw_vertex3d:?} raw2d={raw_vertex2d:?} skip_flag_3bd={skip_flag_3bd:?} skip_flag_raw={skip_flag_raw:?}",
                (
                    start._record_start_bits,
                    start.object_stream_start_bits,
                    start.size_bytes,
                    start.handle_stream_bits,
                    start.object_type,
                    start.body_start_bits,
                    start.object_data_end_bits,
                )
            )
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn debug_forced_vertex_nearby_starts(
    version: CadVersion,
    objects: &[u8],
    raw_offset_bits: usize,
    handle: u64,
) -> Vec<String> {
    let mut results = Vec::new();
    for delta in -64isize..=24 {
        let candidate_start = raw_offset_bits as isize + delta;
        if candidate_start < 0 {
            continue;
        }
        let candidate_start = usize::try_from(candidate_start).ok();
        let Some(candidate_start) = candidate_start else {
            continue;
        };
        for candidate in [
            parse_modern_object_start(objects, candidate_start),
            parse_modern_object_start_without_handle_stream_bits(objects, candidate_start),
        ]
        .into_iter()
        .flatten()
        {
            let forced = attempt_vertex_decode_at_start(version, objects, handle, candidate);
            if forced.is_some() {
                results.push(format!(
                    "delta={delta} start={:?} forced={forced:?}",
                    (
                        candidate._record_start_bits,
                        candidate.object_stream_start_bits,
                        candidate.size_bytes,
                        candidate.handle_stream_bits,
                        candidate.object_type,
                        candidate.body_start_bits,
                        candidate.object_data_end_bits,
                    )
                ));
            }
        }
    }
    results
}

#[cfg(test)]
pub(crate) fn debug_direct_vertex_body_offsets(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    start_bits: std::ops::RangeInclusive<usize>,
) -> Vec<String> {
    let mut results = Vec::new();
    for start_bit in start_bits {
        let vertex = {
            let mut ctx = ObjectDecodeContext {
                version,
                expected_handle: handle,
                object_end_bits: objects.len().saturating_mul(8),
                object: LsbBitStream::new(objects, start_bit),
                text: BitStream::new(objects, objects.len().saturating_mul(8)),
                handles: LsbBitStream::new(objects, objects.len().saturating_mul(8)),
                object_type: u32::MAX,
            };
            decode_vertex3d(&mut ctx)
        };
        let raw3d = {
            let mut ctx = ObjectDecodeContext {
                version,
                expected_handle: handle,
                object_end_bits: objects.len().saturating_mul(8),
                object: LsbBitStream::new(objects, start_bit),
                text: BitStream::new(objects, objects.len().saturating_mul(8)),
                handles: LsbBitStream::new(objects, objects.len().saturating_mul(8)),
                object_type: u32::MAX,
            };
            read_vertex_entity_prefix_detailed(&mut ctx)
                .ok()
                .and_then(|prefix| {
                    decode_vertex3d_body(
                        version,
                        prefix,
                        ctx.object,
                        ctx.handles,
                        VertexPointLayout::BitDouble,
                    )
                    .ok()
                })
        };
        let raw_double = {
            let mut ctx = ObjectDecodeContext {
                version,
                expected_handle: handle,
                object_end_bits: objects.len().saturating_mul(8),
                object: LsbBitStream::new(objects, start_bit),
                text: BitStream::new(objects, objects.len().saturating_mul(8)),
                handles: LsbBitStream::new(objects, objects.len().saturating_mul(8)),
                object_type: u32::MAX,
            };
            read_vertex_entity_prefix_detailed(&mut ctx)
                .ok()
                .and_then(|prefix| {
                    decode_vertex3d_body(
                        version,
                        prefix,
                        ctx.object,
                        ctx.handles,
                        VertexPointLayout::RawDouble,
                    )
                    .ok()
                })
        };
        if vertex.is_some() || raw3d.is_some() || raw_double.is_some() {
            results.push(format!(
                "start_bit={start_bit} vertex={vertex:?} raw3d={raw3d:?} raw_double={raw_double:?}"
            ));
        }
    }
    results
}

#[cfg(test)]
pub(crate) fn debug_direct_vertex_body_offsets_msb(
    version: CadVersion,
    objects: &[u8],
    start_bits: std::ops::RangeInclusive<usize>,
) -> Vec<String> {
    let mut results = Vec::new();
    for start_bit in start_bits {
        let mut object = BitStream::new(objects, start_bit);
        let preview_exists = object.read_bit().unwrap_or(false);
        if preview_exists {
            let preview_size = if is_r2010_plus(version) {
                object
                    .read_bit_long_long()
                    .and_then(|size| usize::try_from(size).ok())
            } else {
                object
                    .read_bit_long()
                    .and_then(|size| usize::try_from(size).ok())
            };
            let Some(preview_size) = preview_size else {
                continue;
            };
            if preview_size > MAX_PREVIEW_BYTES || object.advance_bytes(preview_size).is_none() {
                continue;
            }
        }
        let _entity_mode = object.read_2bits().unwrap_or(MODEL_SPACE_MODE);
        let _reactor_count = object.read_bit_long();
        if is_r2004_plus(version) {
            let _xdict_missing = object.read_bit();
        }
        if !is_r2004_plus(version) {
            let _nolinks = object.read_bit();
        }
        if is_r2013_plus(version) {
            let _has_ds_binary_data = object.read_bit();
        }
        let color_raw = object
            .read_bit_short()
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0);
        if color_raw != 0 {
            let flags = color_raw >> 8;
            if (flags & 0x20) != 0 {
                let _alpha = object.read_bit_long();
            }
            if (flags & 0x40) != 0 {
                let _color_handle = object.read_handle_reference(0);
            } else if (flags & 0x80) != 0 {
                let _rgb = object.read_bit_long();
            }
            if (flags & 0x41) == 0x41 {
                let _color_name = object.read_variable_text(version);
            }
            if (flags & 0x42) == 0x42 {
                let _book_name = object.read_variable_text(version);
            }
        }
        let _line_type_scale = object.read_bit_double();
        if is_r2004_plus(version) {
            let _line_type_flags = object.read_2bits();
            let _plotstyle_flags = object.read_2bits();
            if is_r2007_plus(version) {
                let _material_flags = object.read_2bits();
                let _shadow_flags = object.read_byte();
            }
            if is_r2010_plus(version) {
                let _has_full_visual_style = object.read_bit();
                let _has_face_visual_style = object.read_bit();
                let _has_edge_visual_style = object.read_bit();
            }
            let _invisible = object.read_bit_short();
            let _lineweight = object.read_byte();
        }
        let flags = object.read_byte();
        let point_3bd = {
            let mut point_reader = object.clone();
            point_reader.read_3bit_double()
        };
        let point_raw = if flags.is_some() {
            let mut point_reader = object.clone();
            Some(Point3 {
                x: point_reader.read_double().unwrap_or(0.0),
                y: point_reader.read_double().unwrap_or(0.0),
                z: point_reader.read_double().unwrap_or(0.0),
            })
        } else {
            None
        };
        if point_3bd.is_some() || point_raw.is_some() {
            results.push(format!(
                "start_bit={start_bit} msb_flag={flags:?} msb_3bd={point_3bd:?} msb_raw={point_raw:?}"
            ));
        }
    }
    results
}

#[cfg(test)]
fn debug_polyline3d_layouts(
    version: CadVersion,
    objects: &[u8],
    handle: u64,
    candidate: ModernObjectStart,
) -> Vec<String> {
    let mut layouts = Vec::new();
    let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
    let Ok(_prefix) = read_common_entity_prefix_detailed(&mut ctx) else {
        return layouts;
    };

    let mut current = ctx.object.clone();
    let current_curve = current.read_byte();
    let current_flags = current.read_byte();
    let current_count = current
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok());
    layouts.push(format!(
        "current curve={current_curve:?} flags={current_flags:?} count={current_count:?}"
    ));

    let mut with_has_vertex = ctx.object.clone();
    let has_vertex = with_has_vertex.read_bit();
    let curve = with_has_vertex.read_byte();
    let flags = with_has_vertex.read_byte();
    let count = with_has_vertex
        .read_bit_long()
        .and_then(|value| usize::try_from(value).ok());
    layouts.push(format!(
        "has_vertex_first has_vertex={has_vertex:?} curve={curve:?} flags={flags:?} count={count:?}"
    ));

    let mut legacyish = ctx.object.clone();
    let legacy_flags0 = legacyish.read_byte();
    let legacy_flags1 = legacyish.read_byte();
    let legacy_count = legacyish
        .read_bit_long()
        .and_then(|value| usize::try_from(value).ok());
    layouts.push(format!(
        "legacy flags0={legacy_flags0:?} flags1={legacy_flags1:?} count={legacy_count:?}"
    ));

    let mut count_first = ctx.object.clone();
    let count_first_value = count_first
        .read_bit_long()
        .and_then(|value| usize::try_from(value).ok());
    let count_first_curve = count_first.read_byte();
    let count_first_flags = count_first.read_byte();
    layouts.push(format!(
        "count_first count={count_first_value:?} curve={count_first_curve:?} flags={count_first_flags:?}"
    ));

    layouts
}

#[cfg(test)]
pub(crate) fn debug_polyline3d_layouts_for_handle(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    handle: u64,
) -> Vec<String> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(handle_map, object_index, handle)
    else {
        return Vec::new();
    };
    let Some(candidate) = direct_object_start_for_handle(
        version,
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    ) else {
        return Vec::new();
    };
    debug_polyline3d_layouts(version, objects, handle, candidate)
}

#[cfg(test)]
pub(crate) fn debug_polyline3d_handle_sequence_for_handle(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    handle: u64,
) -> Option<String> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let (preferred_offset_bits, alternate_offset_bits) =
        exact_handle_offsets(handle_map, object_index, handle)?;
    let candidate = direct_object_start_for_handle(
        version,
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    )?;
    let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
    let prefix = read_common_entity_prefix_detailed(&mut ctx).ok()?;
    let owned_count = ctx
        .object
        .read_bit_long()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
        .min(MAX_OWNED_HANDLES);
    Some(debug_polyline3d_handle_sequence(
        version,
        prefix,
        ctx.handles,
        owned_count,
    ))
}

#[cfg(test)]
pub(crate) fn debug_forced_lwpolyline_nearby_starts(
    version: CadVersion,
    objects: &[u8],
    raw_offset_bits: usize,
    handle: u64,
) -> Vec<String> {
    let mut results = Vec::new();
    for delta in -64isize..=24 {
        let candidate_start = raw_offset_bits as isize + delta;
        if candidate_start < 0 {
            continue;
        }
        let Some(candidate_start) = usize::try_from(candidate_start).ok() else {
            continue;
        };
        for candidate in [
            parse_modern_object_start(objects, candidate_start),
            parse_modern_object_start_without_handle_stream_bits(objects, candidate_start),
        ]
        .into_iter()
        .flatten()
        {
            let polyline = attempt_lwpolyline_decode_at_start(version, objects, handle, candidate);
            if let Some(DecodedEntity {
                kind: DecodedEntityKind::LwPolyline(polyline),
                ..
            }) = polyline
            {
                let (min, max) = match horizontal_bounds(&polyline.points) {
                    Some(bounds) => bounds,
                    None => continue,
                };
                let score =
                    forced_lwpolyline_candidate_score(&polyline, candidate, raw_offset_bits)
                        .unwrap_or_default();
                results.push((
                    score,
                    format!(
                        "score={score} delta={delta} start={:?} points={} closed={} bounds=({:.3},{:.3},{:.3})..({:.3},{:.3},{:.3}) first={:?} last={:?}",
                        (
                            candidate._record_start_bits,
                            candidate.object_stream_start_bits,
                            candidate.size_bytes,
                            candidate.handle_stream_bits,
                            candidate.object_type,
                            candidate.body_start_bits,
                            candidate.object_data_end_bits,
                        ),
                        polyline.points.len(),
                        polyline.closed,
                        min.x,
                        min.y,
                        min.z,
                        max.x,
                        max.y,
                        max.z,
                        polyline.points.first(),
                        polyline.points.last(),
                    ),
                ));
            }
        }
    }
    results.sort_by(|left, right| right.0.cmp(&left.0));
    results.truncate(12);
    results.into_iter().map(|(_, line)| line).collect()
}

#[cfg(test)]
pub(crate) fn debug_forced_polyline3d_nearby_starts(
    version: CadVersion,
    objects: &[u8],
    raw_offset_bits: usize,
    handle: u64,
) -> Vec<String> {
    let mut results = Vec::new();
    for delta in -32isize..=24 {
        let candidate_start = raw_offset_bits as isize + delta;
        if candidate_start < 0 {
            continue;
        }
        let Some(candidate_start) = usize::try_from(candidate_start).ok() else {
            continue;
        };
        for candidate in [
            parse_modern_object_start(objects, candidate_start),
            parse_modern_object_start_without_handle_stream_bits(objects, candidate_start),
        ]
        .into_iter()
        .flatten()
        {
            let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
            let Some(entity) = decode_polyline3d(&mut ctx) else {
                continue;
            };
            let score = polyline_entity_quality_score(&entity);
            if let DecodedEntityKind::Polyline3D(header) = &entity.kind {
                results.push((
                    score,
                    format!(
                        "score={score} delta={delta} start={:?} owned_count={} owned_handles={} closed={} owner={:?}",
                        (
                            candidate._record_start_bits,
                            candidate.object_stream_start_bits,
                            candidate.size_bytes,
                            candidate.handle_stream_bits,
                            candidate.object_type,
                            candidate.body_start_bits,
                            candidate.object_data_end_bits,
                        ),
                        header.owned_count,
                        header.owned_handles.len(),
                        header.closed,
                        entity.common.owner_handle,
                    ),
                ));
            }
        }
    }
    results.sort_by(|left, right| right.0.cmp(&left.0));
    results.truncate(16);
    results.into_iter().map(|(_, line)| line).collect()
}

#[cfg(test)]
pub(crate) fn debug_forced_insert_nearby_starts(
    version: CadVersion,
    objects: &[u8],
    raw_offset_bits: usize,
    handle: u64,
    is_minsert: bool,
    type_hints: &BTreeMap<u64, String>,
) -> Vec<String> {
    let mut results = Vec::new();
    for candidate in nearby_start_candidates(
        objects,
        raw_offset_bits,
        INSERT_NEARBY_START_DELTA,
        INSERT_NEARBY_END_DELTA,
    ) {
        let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
        let entity = if is_minsert {
            decode_minsert(&mut ctx)
        } else {
            decode_insert(&mut ctx)
        };
        let Some(entity) = entity else {
            continue;
        };
        let score = insert_entity_quality_score(&entity);
        let allowed = decoded_entity_is_reasonable(&entity, type_hints);
        if let DecodedEntityKind::Insert(insert) = &entity.kind {
            results.push((
                score,
                format!(
                    "score={score} allowed={allowed} start={:?} block={:?}/{:?} point={:?} layer={:?}",
                    (
                        candidate._record_start_bits,
                        candidate.object_stream_start_bits,
                        candidate.size_bytes,
                        candidate.handle_stream_bits,
                        candidate.object_type,
                        candidate.body_start_bits,
                        candidate.object_data_end_bits,
                    ),
                    insert.block_handle,
                    insert.alternate_block_handle,
                    insert.insertion_point,
                    entity.common.layer_handle,
                ),
            ));
        }
    }
    results.sort_by(|left, right| right.0.cmp(&left.0));
    results.truncate(16);
    results.into_iter().map(|(_, line)| line).collect()
}

#[cfg(test)]
pub(crate) fn debug_forced_insert_exact_starts(
    version: CadVersion,
    handle_map: &BTreeMap<u64, i64>,
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
    handle: u64,
    is_minsert: bool,
    type_hints: &BTreeMap<u64, String>,
) -> Vec<String> {
    let handles = sorted_handle_offsets(handle_map, object_index);
    let Some((preferred_offset_bits, alternate_offset_bits)) =
        exact_handle_offsets(handle_map, object_index, handle)
    else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for candidate in exact_supported_object_candidates(
        objects,
        handle,
        object_index.iter().find(|record| record.handle == handle),
        preferred_offset_bits,
        alternate_offset_bits,
        next_handle_offset_bits(&handles, handle),
    ) {
        let mut ctx = ObjectDecodeContext::new(version, objects, handle, candidate);
        let entity = if is_minsert {
            decode_minsert(&mut ctx)
        } else {
            decode_insert(&mut ctx)
        };
        let Some(entity) = entity else {
            results.push(format!(
                "start={:?} decode=None",
                (
                    candidate._record_start_bits,
                    candidate.object_stream_start_bits,
                    candidate.size_bytes,
                    candidate.handle_stream_bits,
                    candidate.object_type,
                    candidate.body_start_bits,
                    candidate.object_data_end_bits,
                )
            ));
            continue;
        };
        let score = insert_entity_quality_score(&entity);
        let allowed = decoded_entity_is_reasonable(&entity, type_hints);
        if let DecodedEntityKind::Insert(insert) = &entity.kind {
            results.push(format!(
                "score={score} allowed={allowed} start={:?} block={:?}/{:?} point={:?} scale={:?} rot={} layer={:?}",
                (
                    candidate._record_start_bits,
                    candidate.object_stream_start_bits,
                    candidate.size_bytes,
                    candidate.handle_stream_bits,
                    candidate.object_type,
                    candidate.body_start_bits,
                    candidate.object_data_end_bits,
                ),
                insert.block_handle,
                insert.alternate_block_handle,
                insert.insertion_point,
                insert.scale,
                insert.rotation_degrees,
                entity.common.layer_handle,
            ));
        }
    }
    results
}

fn parse_modern_object_start(bytes: &[u8], bit_index: usize) -> Option<ModernObjectStart> {
    let mut preamble = BitStream::new(bytes, bit_index);
    let size_bytes = preamble.read_modular_short()?;
    if size_bytes == 0 {
        return None;
    }
    let data_start_bits = preamble.bit_index;
    let handle_stream_bits = preamble.read_modular_char()?;
    let object_stream_start_bits = preamble.bit_index;
    let object_type = read_object_type_at(bytes, object_stream_start_bits)?;
    Some(ModernObjectStart {
        _record_start_bits: bit_index,
        data_start_bits,
        object_stream_start_bits,
        size_bytes,
        handle_stream_bits,
        object_type,
        body_start_bits: None,
        object_data_end_bits: None,
    })
}

fn parse_legacy_object_start(bytes: &[u8], bit_index: usize) -> Option<ModernObjectStart> {
    let mut preamble = BitStream::new(bytes, bit_index);
    let size_bytes = usize::try_from(preamble.read_modular_char()?).ok()?;
    if size_bytes == 0 {
        return None;
    }
    let data_start_bits = preamble.bit_index;
    let handle_stream_bits = preamble.read_modular_char()?;
    let object_stream_start_bits = preamble.bit_index;
    let object_type = read_object_type_at(bytes, object_stream_start_bits)?;
    Some(ModernObjectStart {
        _record_start_bits: bit_index,
        data_start_bits,
        object_stream_start_bits,
        size_bytes,
        handle_stream_bits,
        object_type,
        body_start_bits: None,
        object_data_end_bits: None,
    })
}

fn parse_modern_object_start_without_handle_stream_bits(
    bytes: &[u8],
    bit_index: usize,
) -> Option<ModernObjectStart> {
    let mut preamble = BitStream::new(bytes, bit_index);
    let size_bytes = preamble.read_modular_short()?;
    if size_bytes == 0 {
        return None;
    }
    let data_start_bits = preamble.bit_index;
    let object_stream_start_bits = preamble.bit_index;
    let object_type = read_object_type_at(bytes, object_stream_start_bits)?;
    Some(ModernObjectStart {
        _record_start_bits: bit_index,
        data_start_bits,
        object_stream_start_bits,
        size_bytes,
        handle_stream_bits: 0,
        object_type,
        body_start_bits: None,
        object_data_end_bits: None,
    })
}

/// Build a fallback candidate from the normal candidate by reading through
/// the object preamble (type, self-handle, EED, graphic_present) using LSB
/// bit ordering to find the correct body_start_bits.
fn compute_body_start_lsb(
    version: CadVersion,
    objects: &[u8],
    _expected_handle: u64,
    candidate: ModernObjectStart,
) -> Option<ModernObjectStart> {
    let mut stream = LsbBitStream::new(objects, candidate.object_stream_start_bits);
    let object_type = stream.read_object_type()?;
    if is_non_entity_object_type(object_type) {
        // Non-entity objects (LAYER, BLOCK_HEADER, etc.): body starts right after
        // the type. The handle, EED, and reactor data will be consumed by
        // skip_common_non_entity_data_detailed / read_common_data_detailed.
        return Some(ModernObjectStart {
            body_start_bits: Some(stream.bit_index),
            object_type,
            ..candidate
        });
    }
    // Entity objects: consume handle, EED, and graphic_present.
    let _self_handle = stream.read_handle_reference(0);
    skip_eed(&mut stream, 4096);
    if is_r2004_plus(version) {
        let graphic_present = stream.read_bit().unwrap_or(false);
        if graphic_present {
            let object_end_bits = candidate
                .object_stream_start_bits
                .saturating_add(candidate.size_bytes.saturating_mul(8));
            if let Some(graphics_size) = stream.read_bit_long() {
                let skip_bytes = usize::try_from(graphics_size)
                    .unwrap_or(0)
                    .min(object_end_bits.saturating_sub(stream.bit_index) / 8);
                for _ in 0..skip_bytes {
                    let _ = stream.read_u8();
                }
            }
        }
    }
    Some(ModernObjectStart {
        body_start_bits: Some(stream.bit_index),
        object_type,
        ..candidate
    })
}

fn polyline_owned_count_is_sane(owned_count: usize) -> bool {
    owned_count > 0 && owned_count <= 4_096
}

fn read_object_type_at(bytes: &[u8], bit_index: usize) -> Option<u32> {
    let mut reader = LsbBitStream::new(bytes, bit_index);
    reader.read_object_type()
}

fn read_object_type_msb_at(bytes: &[u8], bit_index: usize) -> Option<u32> {
    let mut reader = BitStream::new(bytes, bit_index);
    reader.read_object_type()
}

#[derive(Clone, Copy)]
enum ObjectBitOrder {
    Msb,
    Lsb,
}

fn declared_handle_after_object_type(
    bytes: &[u8],
    bit_index: usize,
    order: ObjectBitOrder,
) -> Option<u64> {
    match order {
        ObjectBitOrder::Msb => {
            let mut reader = BitStream::new(bytes, bit_index);
            let _object_type = reader.read_object_type()?;
            reader.read_handle_reference(0)
        }
        ObjectBitOrder::Lsb => {
            let mut reader = LsbBitStream::new(bytes, bit_index);
            let _object_type = reader.read_object_type()?;
            reader.read_handle_reference(0)
        }
    }
}

fn locate_embedded_text_stream_start(bytes: &[u8], flag_pos: usize) -> Option<usize> {
    let mut flag_reader = BitStream::new(bytes, flag_pos);
    if !flag_reader.read_bit()? {
        return None;
    }
    let length_pos = flag_pos.checked_sub(16)?;
    let mut size_reader = BitStream::new(bytes, length_pos);
    let raw = usize::from(size_reader.read_u16_le()?);
    // The high bit of the 16-bit size indicates a 32-bit extended size.
    let (size_bits, anchor_pos) = if (raw & 0x8000) != 0 {
        let high_pos = length_pos.checked_sub(16)?;
        let mut hi_reader = BitStream::new(bytes, high_pos);
        let high = usize::from(hi_reader.read_u16_le()?);
        ((raw & 0x7fff) | (high << 15), high_pos)
    } else {
        (raw, length_pos)
    };
    anchor_pos.checked_sub(size_bits)
}

fn text_stream_for_object<'a>(
    bytes: &'a [u8],
    data_start_bits: usize,
    object_end_bits: usize,
    handle_section_offset: usize,
    text_flag_offset: usize,
    _version: CadVersion,
    search_bits: usize,
) -> BitStream<'a> {
    if search_bits == 0 {
        return BitStream::new(bytes, bytes.len().saturating_mul(8));
    }
    // R2007+: try multiple text flag positions.
    // Convention 1: MC includes 8 bits of text overhead → flag at hs_off + 7
    // Convention 2: MC is pure handle data → flag at hs_off - 1
    // Also try single-byte MC interpretation (for multi-byte MC misreads).
    let mc_bits = object_end_bits.saturating_sub(handle_section_offset);
    let alt_mc = mc_bits & 0x7F;
    let alt_hs_off = object_end_bits.saturating_sub(alt_mc);
    let mut candidates = vec![(handle_section_offset, text_flag_offset)];
    // Try flag at hs_off - 1 (MC = pure handle data, text metadata is separate)
    if handle_section_offset > 0 {
        candidates.push((
            handle_section_offset,
            handle_section_offset.saturating_sub(1),
        ));
    }
    // Try alt MC (single-byte interpretation)
    if alt_hs_off != handle_section_offset && alt_mc > 0 {
        candidates.push((alt_hs_off, alt_hs_off.saturating_add(7)));
        if alt_hs_off > 0 {
            candidates.push((alt_hs_off, alt_hs_off.saturating_sub(1)));
        }
    }
    for (hs_off, flag_off) in candidates {
        if flag_off < object_end_bits {
            if let Some(start) = locate_embedded_text_stream_start(bytes, flag_off) {
                if start >= data_start_bits && start < hs_off {
                    return BitStream::new(bytes, start);
                }
            }
        }
    }
    // Fallback: scan for 7-zeros+flag pattern in the object data region
    if search_bits > 0 {
        if let Some(start) = scan_for_text_stream(bytes, data_start_bits, handle_section_offset) {
            return BitStream::new(bytes, start);
        }
    }
    // No text stream found — return an exhausted stream
    BitStream::new(bytes, bytes.len().saturating_mul(8))
}

/// Scan backward from `end_bits` looking for the text stream signature:
/// 7 zero bits followed by a 1 bit (the text-present flag), then valid
/// text size metadata.
fn scan_for_text_stream(bytes: &[u8], start_bits: usize, end_bits: usize) -> Option<usize> {
    // The flag must be at least 8 bits from the start (for the 7 padding zeros)
    // and at least 16 bits after start (for the text size before it).
    let scan_begin = start_bits.saturating_add(24);
    // Scan from the end backward — the text overhead is near the end of the object
    for candidate_flag in (scan_begin..end_bits).rev() {
        // Check: bit at candidate_flag must be 1
        let byte = *bytes.get(candidate_flag / 8)?;
        let shift = 7 - (candidate_flag % 8);
        if ((byte >> shift) & 1) != 1 {
            continue;
        }
        // Check: 7 preceding bits must all be 0
        let padding_ok = (1..=7).all(|offset| {
            let pos = candidate_flag - offset;
            let b = bytes.get(pos / 8).copied().unwrap_or(0xFF);
            let s = 7 - (pos % 8);
            ((b >> s) & 1) == 0
        });
        if !padding_ok {
            continue;
        }
        // Try to read text stream metadata from this position
        if let Some(text_start) = locate_embedded_text_stream_start(bytes, candidate_flag) {
            if text_start >= start_bits && text_start < candidate_flag {
                return Some(text_start);
            }
        }
    }
    None
}

#[derive(Clone)]
struct BitStream<'a> {
    bytes: &'a [u8],
    bit_index: usize,
}

#[derive(Clone)]
struct LsbBitStream<'a> {
    bytes: &'a [u8],
    bit_index: usize,
}

#[allow(dead_code)]
impl<'a> LsbBitStream<'a> {
    fn new(bytes: &'a [u8], bit_index: usize) -> Self {
        Self { bytes, bit_index }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let byte = *self.bytes.get(self.bit_index / 8)?;
        let shift = 7usize.checked_sub(self.bit_index % 8)?;
        self.bit_index += 1;
        Some(((byte >> shift) & 1) != 0)
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_u8(&mut self) -> Option<u8> {
        u8::try_from(self.read_bits(8)?).ok()
    }

    fn read_byte(&mut self) -> Option<u8> {
        self.read_u8()
    }

    fn read_u16_le(&mut self) -> Option<u16> {
        Some(u16::from(self.read_u8()?) | (u16::from(self.read_u8()?) << 8))
    }

    fn read_i16_le(&mut self) -> Option<i16> {
        Some(self.read_u16_le()? as i16)
    }

    fn read_i32_le(&mut self) -> Option<i32> {
        let b0 = self.read_u8()?;
        let b1 = self.read_u8()?;
        let b2 = self.read_u8()?;
        let b3 = self.read_u8()?;
        Some(i32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn read_f64_le(&mut self) -> Option<f64> {
        let mut bytes = [0u8; 8];
        for byte in &mut bytes {
            *byte = self.read_u8()?;
        }
        Some(f64::from_le_bytes(bytes))
    }

    fn read_2bits(&mut self) -> Option<u8> {
        u8::try_from(self.read_bits(2)?).ok()
    }

    fn read_object_type(&mut self) -> Option<u32> {
        match self.read_bits(2)? {
            0 => Some(u32::from(self.read_u8()?)),
            1 => Some(0x1f0 + u32::from(self.read_u8()?)),
            2 | 3 => Some(u32::from(self.read_u16_le()?)),
            _ => None,
        }
    }

    fn read_bit_short(&mut self) -> Option<i16> {
        match self.read_2bits()? {
            0 => self.read_i16_le(),
            1 => Some(i16::from(self.read_u8()?)),
            2 => Some(0),
            3 => Some(256),
            _ => None,
        }
    }

    fn read_bit_long(&mut self) -> Option<i32> {
        match self.read_2bits()? {
            0 => self.read_i32_le(),
            1 => Some(i32::from(self.read_u8()?)),
            2 => Some(0),
            _ => None,
        }
    }

    fn read_bit_long_long(&mut self) -> Option<u64> {
        let count = usize::try_from(self.read_bits(3)?).ok()?;
        let mut value = 0u64;
        for index in 0..count {
            value |= u64::from(self.read_u8()?) << (index * 8);
        }
        Some(value)
    }

    fn read_double(&mut self) -> Option<f64> {
        self.read_f64_le()
    }

    fn read_bit_double(&mut self) -> Option<f64> {
        match self.read_2bits()? {
            0 => self.read_f64_le(),
            1 => Some(1.0),
            2 => Some(0.0),
            _ => None,
        }
    }

    fn read_bit_double_with_default(&mut self, default: f64) -> Option<f64> {
        let mut bytes = default.to_le_bytes();
        match self.read_2bits()? {
            0 => Some(default),
            1 => {
                bytes[0] = self.read_u8()?;
                bytes[1] = self.read_u8()?;
                bytes[2] = self.read_u8()?;
                bytes[3] = self.read_u8()?;
                Some(f64::from_le_bytes(bytes))
            }
            2 => {
                bytes[4] = self.read_u8()?;
                bytes[5] = self.read_u8()?;
                bytes[0] = self.read_u8()?;
                bytes[1] = self.read_u8()?;
                bytes[2] = self.read_u8()?;
                bytes[3] = self.read_u8()?;
                Some(f64::from_le_bytes(bytes))
            }
            3 => self.read_f64_le(),
            _ => None,
        }
    }

    fn read_2raw_double(&mut self) -> Option<(f64, f64)> {
        Some((self.read_f64_le()?, self.read_f64_le()?))
    }

    fn read_2bit_double_with_default(&mut self, default: (f64, f64)) -> Option<(f64, f64)> {
        Some((
            self.read_bit_double_with_default(default.0)?,
            self.read_bit_double_with_default(default.1)?,
        ))
    }

    fn read_3bit_double(&mut self) -> Option<Point3> {
        Some(Point3 {
            x: self.read_bit_double()?,
            y: self.read_bit_double()?,
            z: self.read_bit_double()?,
        })
    }

    fn read_3bit_double_with_default(&mut self, default: Point3) -> Option<Point3> {
        Some(Point3 {
            x: self.read_bit_double_with_default(default.x)?,
            y: self.read_bit_double_with_default(default.y)?,
            z: self.read_bit_double_with_default(default.z)?,
        })
    }

    fn read_bit_extrusion(&mut self) -> Option<Point3> {
        if self.read_bit()? {
            Some(Point3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            })
        } else {
            self.read_3bit_double()
        }
    }

    fn read_bit_thickness(&mut self) -> Option<f64> {
        if self.read_bit()? {
            Some(0.0)
        } else {
            self.read_bit_double()
        }
    }

    fn read_modular_char(&mut self) -> Option<u64> {
        let mut shift = 0usize;
        let mut byte = self.read_u8()?;
        let mut value = u64::from(byte & 0x7f);
        while (byte & 0x80) != 0 {
            shift += 7;
            byte = self.read_u8()?;
            value |= u64::from(byte & 0x7f).checked_shl(u32::try_from(shift).ok()?)?;
        }
        Some(value)
    }

    fn read_variable_text(&mut self, version: CadVersion) -> Option<String> {
        let len = usize::try_from(self.read_bit_short()?).ok()?;
        if len == 0 {
            return Some(String::new());
        }
        if is_r2007_plus(version) {
            let mut data = Vec::with_capacity(len);
            for _ in 0..len {
                data.push(self.read_u16_le()?);
            }
            Some(String::from_utf16_lossy(&data).replace('\0', ""))
        } else {
            let mut bytes = vec![0u8; len];
            for byte in &mut bytes {
                *byte = self.read_u8()?;
            }
            Some(
                String::from_utf8_lossy(&bytes)
                    .replace('\0', "")
                    .to_string(),
            )
        }
    }

    fn advance_bytes(&mut self, count: usize) -> Option<()> {
        self.bit_index = self.bit_index.checked_add(count.checked_mul(8)?)?;
        Some(())
    }

    fn read_handle_reference(&mut self, reference_handle: u64) -> Option<u64> {
        let form = self.read_u8()?;
        let code = form >> 4;
        let length = usize::from(form & 0x0f);
        match code {
            0x0..=0x5 => self.read_raw_handle(length),
            0x6 => Some(reference_handle + 1),
            0x8 => Some(reference_handle.saturating_sub(1)),
            0xA => Some(reference_handle + self.read_raw_handle(length)?),
            0xC => Some(reference_handle.saturating_sub(self.read_raw_handle(length)?)),
            _ => None,
        }
    }

    fn read_raw_handle(&mut self, length: usize) -> Option<u64> {
        let mut value = 0u64;
        for _ in 0..length {
            value = (value << 8) | u64::from(self.read_u8()?);
        }
        Some(value)
    }
}

#[allow(dead_code)]
impl<'a> BitStream<'a> {
    fn new(bytes: &'a [u8], bit_index: usize) -> Self {
        Self { bytes, bit_index }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let byte = *self.bytes.get(self.bit_index / 8)?;
        let shift = 7usize.checked_sub(self.bit_index % 8)?;
        self.bit_index += 1;
        Some(((byte >> shift) & 1) != 0)
    }

    fn read_bits_msb(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_2bits(&mut self) -> Option<u8> {
        u8::try_from(self.read_bits_msb(2)?).ok()
    }

    fn read_byte(&mut self) -> Option<u8> {
        u8::try_from(self.read_bits_msb(8)?).ok()
    }

    fn read_u16_le(&mut self) -> Option<u16> {
        Some(u16::from(self.read_byte()?) | (u16::from(self.read_byte()?) << 8))
    }

    fn read_i16_le(&mut self) -> Option<i16> {
        Some(self.read_u16_le()? as i16)
    }

    fn read_i32_le(&mut self) -> Option<i32> {
        let b0 = self.read_byte()?;
        let b1 = self.read_byte()?;
        let b2 = self.read_byte()?;
        let b3 = self.read_byte()?;
        Some(i32::from_le_bytes([b0, b1, b2, b3]))
    }

    fn read_f64_le(&mut self) -> Option<f64> {
        let mut bytes = [0u8; 8];
        for byte in &mut bytes {
            *byte = self.read_byte()?;
        }
        Some(f64::from_le_bytes(bytes))
    }

    fn read_bit_short(&mut self) -> Option<i16> {
        match self.read_2bits()? {
            0 => self.read_i16_le(),
            1 => Some(i16::from(self.read_byte()?)),
            2 => Some(0),
            3 => Some(256),
            _ => None,
        }
    }

    fn read_bit_long(&mut self) -> Option<i32> {
        match self.read_2bits()? {
            0 => self.read_i32_le(),
            1 => Some(i32::from(self.read_byte()?)),
            2 => Some(0),
            _ => None,
        }
    }

    fn read_bit_long_long(&mut self) -> Option<u64> {
        let count = usize::from(self.read_bits_msb(3)? as u8);
        let mut value = 0u64;
        for index in 0..count {
            value |= u64::from(self.read_byte()?) << (index * 8);
        }
        Some(value)
    }

    fn read_double(&mut self) -> Option<f64> {
        self.read_f64_le()
    }

    fn read_bit_double(&mut self) -> Option<f64> {
        match self.read_2bits()? {
            0 => self.read_f64_le(),
            1 => Some(1.0),
            2 => Some(0.0),
            _ => None,
        }
    }

    fn read_bit_double_with_default(&mut self, default: f64) -> Option<f64> {
        let mut bytes = default.to_le_bytes();
        match self.read_2bits()? {
            0 => Some(default),
            1 => {
                bytes[0] = self.read_byte()?;
                bytes[1] = self.read_byte()?;
                bytes[2] = self.read_byte()?;
                bytes[3] = self.read_byte()?;
                Some(f64::from_le_bytes(bytes))
            }
            2 => {
                bytes[4] = self.read_byte()?;
                bytes[5] = self.read_byte()?;
                bytes[0] = self.read_byte()?;
                bytes[1] = self.read_byte()?;
                bytes[2] = self.read_byte()?;
                bytes[3] = self.read_byte()?;
                Some(f64::from_le_bytes(bytes))
            }
            3 => self.read_f64_le(),
            _ => None,
        }
    }

    fn read_2raw_double(&mut self) -> Option<(f64, f64)> {
        Some((self.read_f64_le()?, self.read_f64_le()?))
    }

    fn read_2bit_double_with_default(&mut self, default: (f64, f64)) -> Option<(f64, f64)> {
        Some((
            self.read_bit_double_with_default(default.0)?,
            self.read_bit_double_with_default(default.1)?,
        ))
    }

    fn read_3bit_double(&mut self) -> Option<Point3> {
        Some(Point3 {
            x: self.read_bit_double()?,
            y: self.read_bit_double()?,
            z: self.read_bit_double()?,
        })
    }

    fn read_3bit_double_with_default(&mut self, default: Point3) -> Option<Point3> {
        Some(Point3 {
            x: self.read_bit_double_with_default(default.x)?,
            y: self.read_bit_double_with_default(default.y)?,
            z: self.read_bit_double_with_default(default.z)?,
        })
    }

    fn read_bit_extrusion(&mut self) -> Option<Point3> {
        if self.read_bit()? {
            Some(Point3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            })
        } else {
            self.read_3bit_double()
        }
    }

    fn read_bit_thickness(&mut self) -> Option<f64> {
        if self.read_bit()? {
            Some(0.0)
        } else {
            self.read_bit_double()
        }
    }

    fn read_modular_char(&mut self) -> Option<u64> {
        let mut shift = 0usize;
        let mut byte = self.read_byte()?;
        let mut value = u64::from(byte & 0x7f);
        while (byte & 0x80) != 0 {
            shift += 7;
            byte = self.read_byte()?;
            value |= u64::from(byte & 0x7f).checked_shl(u32::try_from(shift).ok()?)?;
        }
        Some(value)
    }

    fn read_modular_short(&mut self) -> Option<usize> {
        let mut shift = 15usize;
        let mut b1 = self.read_byte()?;
        let mut b2 = self.read_byte()?;
        let mut value = usize::from(b1) | (usize::from(b2 & 0x7f) << 8);
        while (b2 & 0x80) != 0 {
            b1 = self.read_byte()?;
            b2 = self.read_byte()?;
            value |= usize::from(b1).checked_shl(u32::try_from(shift).ok()?)?;
            shift += 8;
            value |= usize::from(b2 & 0x7f).checked_shl(u32::try_from(shift).ok()?)?;
            shift += 7;
        }
        Some(value)
    }

    fn read_handle_reference(&mut self, reference_handle: u64) -> Option<u64> {
        let form = self.read_byte()?;
        let code = form >> 4;
        let length = usize::from(form & 0x0f);
        match code {
            0x0..=0x5 => self.read_raw_handle(length),
            0x6 => Some(reference_handle + 1),
            0x8 => Some(reference_handle.saturating_sub(1)),
            0xA => Some(reference_handle + self.read_raw_handle(length)?),
            0xC => Some(reference_handle.saturating_sub(self.read_raw_handle(length)?)),
            _ => None,
        }
    }

    fn read_raw_handle(&mut self, length: usize) -> Option<u64> {
        let mut value = 0u64;
        for _ in 0..length {
            value = (value << 8) | u64::from(self.read_byte()?);
        }
        Some(value)
    }

    fn read_object_type(&mut self) -> Option<u32> {
        match self.read_2bits()? {
            0 => Some(u32::from(self.read_byte()?)),
            1 => Some(0x1f0 + u32::from(self.read_byte()?)),
            2 | 3 => Some(u32::from(self.read_u16_le()?)),
            _ => None,
        }
    }

    fn read_variable_text(&mut self, version: CadVersion) -> Option<String> {
        let len = usize::try_from(self.read_bit_short()?).ok()?;
        if len == 0 {
            return Some(String::new());
        }
        if is_r2007_plus(version) {
            let mut data = Vec::with_capacity(len);
            for _ in 0..len {
                data.push(self.read_u16_le()?);
            }
            Some(String::from_utf16_lossy(&data).replace('\0', ""))
        } else {
            let mut bytes = vec![0u8; len];
            for byte in &mut bytes {
                *byte = self.read_byte()?;
            }
            Some(
                String::from_utf8_lossy(&bytes)
                    .replace('\0', "")
                    .to_string(),
            )
        }
    }

    fn advance_bytes(&mut self, count: usize) -> Option<()> {
        self.bit_index = self.bit_index.checked_add(count.checked_mul(8)?)?;
        Some(())
    }
}

fn is_r2004_plus(version: CadVersion) -> bool {
    matches!(
        version,
        CadVersion::Acad2004
            | CadVersion::Acad2007
            | CadVersion::Acad2010
            | CadVersion::Acad2013
            | CadVersion::Acad2018
    )
}

fn is_r2007_plus(version: CadVersion) -> bool {
    matches!(
        version,
        CadVersion::Acad2007 | CadVersion::Acad2010 | CadVersion::Acad2013 | CadVersion::Acad2018
    )
}

fn is_r2010_plus(version: CadVersion) -> bool {
    matches!(
        version,
        CadVersion::Acad2010 | CadVersion::Acad2013 | CadVersion::Acad2018
    )
}

fn is_r2013_plus(version: CadVersion) -> bool {
    matches!(version, CadVersion::Acad2013 | CadVersion::Acad2018)
}

/// Returns true if the object type is a known non-entity (table/control object).
/// Non-entities do NOT have the graphic_present/entity_mode fields.
/// Only uses types with KNOWN fixed mappings — class-defined types (which can
/// reuse numbers like 0x4D for LWPOLYLINE) are never treated as non-entities.
fn is_non_entity_object_type(object_type: u32) -> bool {
    // Only include types that have confirmed fixed names as non-entity objects.
    crate::fixed_object_type_name(object_type).is_some_and(|name| {
        matches!(
            name,
            "DICTIONARY"
                | "BLOCK_CONTROL_OBJ"
                | "BLOCK_HEADER"
                | "LAYER_CONTROL_OBJ"
                | "LAYER"
                | "STYLE_CONTROL_OBJ"
                | "STYLE"
                | "LTYPE_CONTROL_OBJ"
                | "LTYPE"
                | "VIEW_CONTROL_OBJ"
                | "VIEW"
                | "UCS_CONTROL_OBJ"
                | "UCS"
                | "VPORT_CONTROL_OBJ"
                | "VPORT"
                | "APPID_CONTROL_OBJ"
                | "APPID"
                | "DIMSTYLE_CONTROL_OBJ"
                | "DIMSTYLE"
                | "VPORT_ENTITY_CONTROL_OBJ"
                | "VPORT_ENTITY_HEADER"
        )
    })
}

fn object_type_name(object_type: u32, classes: &[DwgClassSummary]) -> Option<String> {
    crate::fixed_object_type_name(object_type)
        .map(str::to_string)
        .or_else(|| {
            classes
                .iter()
                .find(|class| u32::from(class.class_number) == object_type)
                .map(|class| {
                    if !class.dxf_name.is_empty() {
                        class.dxf_name.clone()
                    } else if !class.cpp_class_name.is_empty() {
                        class.cpp_class_name.clone()
                    } else {
                        format!("CLASS#{}", class.class_number)
                    }
                })
        })
        .or_else(|| crate::fixed_object_type_name(object_type & 0x01FF).map(str::to_string))
}

fn entity_is_reasonable(entity: &Entity) -> bool {
    match entity {
        Entity::Line(line) => {
            points_are_reasonable(&[line.start, line.end])
                && point_has_plausible_horizontal_cad_scale(line.start)
                && point_has_plausible_horizontal_cad_scale(line.end)
        }
        Entity::Arc(arc) => {
            scalar_is_reasonable(arc.radius)
                && arc.radius > MIN_REASONABLE_EXTENT
                && point_is_reasonable(arc.center)
                && point_has_plausible_horizontal_cad_scale(arc.center)
                && arc.start_angle_degrees.is_finite()
                && arc.end_angle_degrees.is_finite()
        }
        Entity::Circle(circle) => {
            scalar_is_reasonable(circle.radius)
                && circle.radius > MIN_REASONABLE_EXTENT
                && point_is_reasonable(circle.center)
                && point_has_plausible_horizontal_cad_scale(circle.center)
        }
        Entity::Polyline(polyline) => polyline_points_are_reasonable(&polyline.points),
        Entity::Face3D(face) => {
            points_are_reasonable(&face.corners)
                && face
                    .corners
                    .iter()
                    .filter(|point| point_has_plausible_horizontal_cad_scale(**point))
                    .take(3)
                    .count()
                    >= 3
        }
        Entity::Insert(insert) => {
            point_is_reasonable(insert.insertion_point)
                && point_is_reasonable(insert.scale)
                && point_has_plausible_horizontal_cad_scale(insert.insertion_point)
                && insert.scale.x.abs() > MIN_REASONABLE_EXTENT
                && insert.scale.y.abs() > MIN_REASONABLE_EXTENT
                && insert.scale.z.abs() > MIN_REASONABLE_EXTENT
                && insert.column_count > 0
                && insert.row_count > 0
                && scalar_is_reasonable(insert.column_spacing)
                && scalar_is_reasonable(insert.row_spacing)
        }
        Entity::Unknown(_) => false,
    }
}

fn points_are_reasonable(points: &[Point3]) -> bool {
    if points.len() < 2 {
        return false;
    }
    let first = points[0];
    let mut has_distinct_point = false;
    for point in points {
        if !point_is_reasonable(*point) {
            return false;
        }
        if !has_distinct_point
            && ((point.x - first.x).abs() > MIN_REASONABLE_EXTENT
                || (point.y - first.y).abs() > MIN_REASONABLE_EXTENT
                || (point.z - first.z).abs() > MIN_REASONABLE_EXTENT)
        {
            has_distinct_point = true;
        }
    }
    has_distinct_point
}

fn polyline_points_are_reasonable(points: &[Point3]) -> bool {
    points_are_reasonable(points)
        && points
            .iter()
            .filter(|point| point_has_plausible_horizontal_cad_scale(**point))
            .take(2)
            .count()
            >= 2
}

fn point_is_reasonable(point: Point3) -> bool {
    scalar_is_reasonable(point.x) && scalar_is_reasonable(point.y) && scalar_is_reasonable(point.z)
}

fn point_has_plausible_horizontal_cad_scale(point: Point3) -> bool {
    let x = point.x.abs();
    let y = point.y.abs();
    x > 10.0 && y > 10.0 && x <= MAX_REASONABLE_COORDINATE_ABS && y <= MAX_REASONABLE_COORDINATE_ABS
}

fn point_is_local_insert_anchor(point: Point3) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.z.is_finite()
        && point.x.abs() <= 5.0
        && point.y.abs() <= 5.0
        && point.z.abs() <= 1_000.0
}

fn point_has_plausible_survey_elevation(point: Point3) -> bool {
    point.z.is_finite() && point.z.abs() <= 1_000.0
}

fn insert_scale_is_plausible(scale: Point3) -> bool {
    [scale.x, scale.y, scale.z]
        .into_iter()
        .all(|value| value.is_finite() && value.abs() >= 0.01 && value.abs() <= 1_000.0)
}

fn scalar_is_reasonable(value: f64) -> bool {
    value.is_finite() && value.abs() <= MAX_REASONABLE_COORDINATE_ABS
}

fn vertex_is_reasonable(vertex: &DecodedVertex) -> bool {
    vertex.owner_handle.is_none_or(handle_is_reasonable)
        && point_is_reasonable(vertex.point)
        && point_has_plausible_horizontal_cad_scale(vertex.point)
}

fn targeted_layer_is_reasonable(layer: &DecodedLayer) -> bool {
    text_name_is_reasonable(&layer.name)
}

fn supported_object_candidate_score(
    object: &SupportedObject,
    candidate: ModernObjectStart,
    raw_offset_bits: usize,
    type_hints: &BTreeMap<u64, String>,
) -> Option<i64> {
    let proximity_bonus = 1_000_i64.saturating_sub(
        i64::try_from(raw_offset_bits.abs_diff(candidate._record_start_bits)).ok()?,
    );
    let score = match object {
        SupportedObject::Layer(layer) => {
            if layer.name.is_empty() {
                return None;
            }
            2_000 + i64::try_from(layer.name.len()).ok()?
        }
        SupportedObject::BlockHeader(block) => {
            i64::try_from(block_header_candidate_score(block)?).ok()? + 5_000
        }
        SupportedObject::Entity(entity) => match &entity.kind {
            DecodedEntityKind::Insert(insert) => {
                let mut score = 3_000_i64;
                let block_handle = preferred_insert_block_handle(insert, type_hints)?;
                if let Some(type_name) = type_hints.get(&block_handle) {
                    if type_name != "BLOCK_HEADER" {
                        return None;
                    }
                    score += 4_000;
                } else {
                    score += 500;
                }
                score += i64::from(insert.column_count) + i64::from(insert.row_count);
                if point_has_plausible_horizontal_cad_scale(insert.insertion_point) {
                    score += 1_500;
                } else if point_is_local_insert_anchor(insert.insertion_point)
                    && insert_scale_is_plausible(insert.scale)
                {
                    score += 250;
                }
                score
            }
            DecodedEntityKind::LwPolyline(_)
            | DecodedEntityKind::Polyline2D(_)
            | DecodedEntityKind::Polyline3D(_) => 2_500,
            DecodedEntityKind::Line(_)
            | DecodedEntityKind::Arc(_)
            | DecodedEntityKind::Circle(_) => 2_000,
            DecodedEntityKind::Face3D(_) => 1_800,
        },
        SupportedObject::Vertex(_) => 1_000,
        SupportedObject::SeqEnd => 500,
        SupportedObject::Ignored => return None,
    };
    Some(score.saturating_add(proximity_bonus))
}

fn decoded_entity_is_reasonable(
    entity: &DecodedEntity,
    type_hints: &BTreeMap<u64, String>,
) -> bool {
    if !handle_is_reasonable(entity.common.handle) {
        return false;
    }
    if entity
        .common
        .owner_handle
        .is_some_and(|handle| !handle_is_reasonable(handle))
    {
        return false;
    }
    if entity
        .common
        .layer_handle
        .is_some_and(|handle| !handle_is_reasonable(handle))
    {
        return false;
    }
    if entity
        .common
        .alternate_layer_handle
        .is_some_and(|handle| !handle_is_reasonable(handle))
    {
        return false;
    }
    match &entity.kind {
        DecodedEntityKind::LwPolyline(polyline) => polyline_points_are_reasonable(&polyline.points),
        DecodedEntityKind::Polyline2D(header) | DecodedEntityKind::Polyline3D(header) => {
            polyline_owned_count_is_sane(header.owned_count)
                && header.owned_handles.len() <= header.owned_count
                && header.owned_handles.iter().all(|handle| {
                    decoded_related_handle_is_reasonable(*handle, entity.common.handle)
                })
        }
        DecodedEntityKind::Insert(insert) => {
            if preferred_insert_block_handle(insert, type_hints).is_none() {
                return false;
            }
            if !(point_has_plausible_horizontal_cad_scale(insert.insertion_point)
                || (point_is_local_insert_anchor(insert.insertion_point)
                    && insert_scale_is_plausible(insert.scale)
                    && entity.common.layer_handle.is_some()))
            {
                return false;
            }
            let instance_count =
                usize::from(insert.column_count).saturating_mul(usize::from(insert.row_count));
            if instance_count > MAX_INSERT_ARRAY_INSTANCES {
                return false;
            }
            if insert.column_count > 1 && insert.column_spacing.abs() < MIN_REASONABLE_EXTENT {
                return false;
            }
            if insert.row_count > 1 && insert.row_spacing.abs() < MIN_REASONABLE_EXTENT {
                return false;
            }
            true
        }
        _ => true,
    }
}

fn preferred_insert_block_handle(
    insert: &DecodedInsert,
    type_hints: &BTreeMap<u64, String>,
) -> Option<u64> {
    insert
        .block_handle
        .and_then(|handle| nearby_block_header_hint(handle, type_hints))
        .or_else(|| {
            insert
                .alternate_block_handle
                .and_then(|handle| nearby_block_header_hint(handle, type_hints))
        })
        .or(insert
            .block_handle
            .filter(|handle| handle_is_reasonable(*handle)))
        .or(insert
            .alternate_block_handle
            .filter(|handle| handle_is_reasonable(*handle)))
}

fn nearby_block_header_hint(handle: u64, type_hints: &BTreeMap<u64, String>) -> Option<u64> {
    nearby_handle_candidates(handle, BLOCK_HEADER_NEIGHBOR_WINDOW)
        .into_iter()
        .find(|candidate| {
            type_hints
                .get(candidate)
                .is_some_and(|type_name| type_name == "BLOCK_HEADER")
        })
}

fn minsert_array_is_reasonable(
    column_count: u16,
    row_count: u16,
    column_spacing: f64,
    row_spacing: f64,
) -> bool {
    let instance_count = usize::from(column_count).saturating_mul(usize::from(row_count));
    if instance_count > MAX_INSERT_ARRAY_INSTANCES {
        return false;
    }
    if column_count > 1
        && (!scalar_is_reasonable(column_spacing) || column_spacing.abs() < MIN_REASONABLE_EXTENT)
    {
        return false;
    }
    if row_count > 1
        && (!scalar_is_reasonable(row_spacing) || row_spacing.abs() < MIN_REASONABLE_EXTENT)
    {
        return false;
    }
    true
}

fn block_header_has_structural_signal(
    block: &DecodedBlockHeader,
    type_hints: &BTreeMap<u64, String>,
) -> bool {
    let begin_is_block = block
        .begin_block_handle
        .and_then(|handle| type_hints.get(&handle))
        .is_some_and(|type_name| type_name == "BLOCK");
    let end_is_endblk = block
        .end_block_handle
        .and_then(|handle| type_hints.get(&handle))
        .is_some_and(|type_name| type_name == "ENDBLK");
    begin_is_block
        || end_is_endblk
        || (block.begin_block_handle.is_some() && block.end_block_handle.is_some())
}

fn block_header_candidate_score(block: &DecodedBlockHeader) -> Option<usize> {
    if !text_name_is_reasonable(&block.name) {
        return None;
    }

    let name_like = block
        .name
        .chars()
        .filter(|character| is_reasonable_name_char(*character))
        .count();
    let ascii_printable = block
        .name
        .chars()
        .filter(|character| character.is_ascii_graphic() || *character == ' ')
        .count();
    let mut score = name_like
        .saturating_mul(8)
        .saturating_add(ascii_printable.saturating_mul(4));
    if block.begin_block_handle.is_some_and(|handle| handle != 0) {
        score = score.saturating_add(32);
    }
    if block.end_block_handle.is_some_and(|handle| handle != 0) {
        score = score.saturating_add(32);
    }
    if !block.is_xref {
        score = score.saturating_add(16);
    }
    Some(score)
}

fn text_name_is_reasonable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|character| !character.is_control() && is_reasonable_name_char(character))
        && name
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
        && !name
            .chars()
            .collect::<Vec<_>>()
            .windows(8)
            .any(|window| window.iter().all(|character| !character.is_ascii()))
}

fn text_stream_candidate_score(name: &str) -> Option<i64> {
    if !text_name_is_reasonable(name) {
        return None;
    }
    let has_alpha = name
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    if !has_alpha && name != "0" {
        return None;
    }
    let mut score = i64::try_from(name.len()).ok()?;
    if has_alpha {
        score += 64;
    }
    if name.contains('_') {
        score += 48;
    }
    if name.contains('*') {
        score += 32;
    }
    if name.contains('-') {
        score += 8;
    }
    if name
        .chars()
        .all(|character| character.is_ascii_uppercase() || !character.is_ascii_alphabetic())
    {
        score += 16;
    }
    Some(score)
}

fn is_generic_table_space_name(name: &str) -> bool {
    matches!(name, "*Model_Space" | "*Paper_Space" | "*Paper_Space0")
}

fn is_reasonable_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '_' | '-'
                | '*'
                | ':'
                | ' '
                | '.'
                | ','
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '/'
                | '\\'
                | '+'
                | '&'
                | '#'
        )
        || is_latin_extended(character)
}

fn is_latin_extended(character: char) -> bool {
    matches!(character as u32, 0x00C0..=0x024F | 0x1E00..=0x1EFF)
}

fn handle_is_reasonable(handle: u64) -> bool {
    handle != 0 && handle <= MAX_REASONABLE_HANDLE
}

fn decoded_related_handle_is_reasonable(handle: u64, current_handle: u64) -> bool {
    handle_is_reasonable(handle) && handle != current_handle
}

pub(crate) fn object_type_hints(
    object_index: &[crate::DwgObjectRecordSummary],
) -> BTreeMap<u64, String> {
    object_index
        .iter()
        .filter_map(|record| {
            record
                .object_type_name
                .as_ref()
                .cloned()
                .or_else(|| {
                    record.object_type.and_then(|object_type| {
                        // Only use the exact type code, not a masked version.
                        // Masking with 0x01FF creates false positives (e.g., 0x231 → 0x31).
                        crate::fixed_object_type_name(object_type)
                            .filter(|name| is_supported_object_type(name))
                            .map(str::to_string)
                    })
                })
                .map(|name| (record.handle, name))
        })
        .collect()
}

fn preferred_object_type_name(
    handle: u64,
    object_type: Option<u32>,
    classes: &[DwgClassSummary],
    type_hints: &BTreeMap<u64, String>,
) -> Option<String> {
    let candidate_name = object_type.and_then(|object_type| object_type_name(object_type, classes));
    match (
        candidate_name.as_deref(),
        type_hints.get(&handle).map(String::as_str),
    ) {
        (Some(candidate), Some(hint))
            if is_geometry_object_type(hint)
                && !is_geometry_object_type(candidate)
                && candidate != "VIEWPORT"
                && candidate != "VP_ENT_HDR"
                && candidate != "DICTIONARY"
                && candidate != "LTYPE" =>
        {
            Some(hint.to_string())
        }
        (Some(name), _) if is_supported_object_type(name) => Some(name.to_string()),
        (Some(_), _) => candidate_name,
        (None, Some(hint)) if is_supported_object_type(hint) => Some(hint.to_string()),
        _ => None,
    }
}

fn select_supported_object_type_name<'a>(
    lsb_type_name: Option<&'a str>,
    msb_type_name: Option<&'a str>,
) -> Option<&'a str> {
    match (lsb_type_name, msb_type_name) {
        (Some(lsb), Some(msb))
            if lsb != msb
                && is_geometry_object_type(msb)
                && !is_geometry_object_type(lsb)
                && lsb != "VIEWPORT"
                && lsb != "VP_ENT_HDR"
                && lsb != "DICTIONARY"
                && lsb != "LTYPE" =>
        {
            Some(msb)
        }
        (Some(lsb), _) => Some(lsb),
        (None, Some(msb)) => Some(msb),
        _ => None,
    }
}

fn is_supported_object_type(name: &str) -> bool {
    matches!(name, "LAYER" | "BLOCK_HEADER") || is_geometry_object_type(name)
}

fn is_geometry_object_type(name: &str) -> bool {
    matches!(
        name,
        "LINE"
            | "ARC"
            | "CIRCLE"
            | "3DFACE"
            | "INSERT"
            | "MINSERT"
            | "LWPOLYLINE"
            | "POLYLINE_2D"
            | "POLYLINE_3D"
            | "VERTEX_2D"
            | "VERTEX_3D"
            | "VERTEX_MESH"
            | "VERTEX_PFACE"
            | "VERTEX_PFACE_FACE"
            | "SEQEND"
    )
}

#[allow(dead_code)]
fn referenced_block_handles(
    blocks: &BTreeMap<u64, DecodedBlockHeader>,
    entities: &[DecodedEntity],
) -> BTreeSet<u64> {
    let mut handles = BTreeSet::new();
    for entity in entities {
        if let DecodedEntityKind::Insert(insert) = &entity.kind {
            if let Some(block_handle) = insert.block_handle {
                if blocks.contains_key(&block_handle) {
                    handles.insert(block_handle);
                }
            }
        }
    }
    handles
}
