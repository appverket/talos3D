use std::{collections::BTreeMap, path::PathBuf};

use cadio_dwg::read_object_index;
use dxf::{entities::EntityType, Drawing};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let dwg_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: correlate_dwg_dxf <path-to-dwg> <path-to-dxf>");
    let dxf_path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: correlate_dwg_dxf <path-to-dwg> <path-to-dxf>");

    let mut dxf_entities = BTreeMap::new();
    let drawing = Drawing::load_file(&dxf_path).expect("failed to load DXF");
    for entity in drawing.entities() {
        if entity.common.is_in_paper_space {
            continue;
        }
        let Some(kind) = entity_kind(&entity.specific) else {
            continue;
        };
        let handle = u64::from_str_radix(&entity.common.handle.as_string(), 16)
            .expect("DXF handle should be hex");
        dxf_entities.insert(handle, (kind, entity.common.layer.clone()));
    }

    let object_index = read_object_index(&dwg_path).expect("failed to read DWG object index");
    let mut by_type = BTreeMap::<String, usize>::new();
    let mut by_signature = BTreeMap::<String, usize>::new();
    let mut by_type_and_kind = BTreeMap::<String, usize>::new();
    let mut by_signature_and_kind = BTreeMap::<String, usize>::new();

    for record in object_index {
        let Some((kind, layer)) = dxf_entities.get(&record.handle) else {
            continue;
        };

        let type_key = format!("{:?}", record.object_type);
        let signature_key = record.header_signature.clone();
        *by_type.entry(type_key.clone()).or_default() += 1;
        *by_signature.entry(signature_key.clone()).or_default() += 1;
        *by_type_and_kind
            .entry(format!("{type_key}\t{kind}\t{layer}"))
            .or_default() += 1;
        *by_signature_and_kind
            .entry(format!("{signature_key}\t{kind}\t{layer}"))
            .or_default() += 1;
    }

    print_ranked("Object type correlations", &by_type_and_kind);
    print_ranked("Header signature correlations", &by_signature_and_kind);
    print_ranked("Object type totals", &by_type);
    print_ranked("Header signature totals", &by_signature);
}

fn entity_kind(entity: &EntityType) -> Option<&'static str> {
    match entity {
        EntityType::Line(_) => Some("LINE"),
        EntityType::Polyline(_) => Some("POLYLINE"),
        EntityType::LwPolyline(_) => Some("LWPOLYLINE"),
        EntityType::Arc(_) => Some("ARC"),
        EntityType::Circle(_) => Some("CIRCLE"),
        EntityType::Face3D(_) => Some("3DFACE"),
        EntityType::Insert(_) => Some("INSERT"),
        EntityType::Solid(_) => Some("SOLID"),
        EntityType::Trace(_) => Some("TRACE"),
        _ => None,
    }
}

fn print_ranked(title: &str, counts: &BTreeMap<String, usize>) {
    println!("# {title}");
    let mut rows = counts.iter().collect::<Vec<_>>();
    rows.sort_by(|(left_key, left_count), (right_key, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_key.cmp(right_key))
    });
    for (key, count) in rows.into_iter().take(40) {
        println!("{count}\t{key}");
    }
    println!();
}
