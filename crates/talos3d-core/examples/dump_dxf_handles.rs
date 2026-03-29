use std::path::PathBuf;

use dxf::{entities::EntityType, Drawing};

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: dump_dxf_handles <path-to-dxf>");
    let drawing = Drawing::load_file(&path).expect("failed to load DXF");

    for entity in drawing.entities() {
        if entity.common.is_in_paper_space {
            continue;
        }
        let kind = match &entity.specific {
            EntityType::Line(_) => "LINE",
            EntityType::Polyline(_) => "POLYLINE",
            EntityType::LwPolyline(_) => "LWPOLYLINE",
            EntityType::Arc(_) => "ARC",
            EntityType::Circle(_) => "CIRCLE",
            EntityType::Face3D(_) => "3DFACE",
            EntityType::Insert(_) => "INSERT",
            EntityType::Solid(_) => "SOLID",
            EntityType::Trace(_) => "TRACE",
            _ => continue,
        };
        println!(
            "{}\t{}\t{}",
            entity.common.handle.as_string(),
            kind,
            entity.common.layer
        );
    }
}
