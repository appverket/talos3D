use std::path::PathBuf;

use cadio_dwg::read_document;

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: dump_dwg_document <path-to-dwg>");
    let document = read_document(&path).expect("failed to read native DWG document");
    println!("layers: {}", document.layers.len());
    println!("blocks: {}", document.blocks.len());
    println!("entities: {}", document.entities.len());
    for layer in document.layers.iter().take(20) {
        println!("layer\t{}\t{}", layer.visible, layer.name);
    }
    for block in document.blocks.iter().take(20) {
        println!("block\t{}\t{}", block.name, block.entities.len());
    }
    for entity in document.entities.iter().take(20) {
        println!("entity\t{:?}", entity);
    }
}
