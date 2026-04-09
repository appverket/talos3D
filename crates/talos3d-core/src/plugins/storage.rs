use bevy::prelude::*;

/// Opaque project/content storage backend.
///
/// Callers should treat the `key` as an implementation-defined locator. Native
/// desktop builds may use filesystem paths, while browser or backend-backed
/// builds can map the same API to object keys or document ids.
pub trait StorageBackend: Send + Sync + 'static {
    fn save(&self, data: &[u8], key: &str) -> Result<(), String>;
    fn load(&self, key: &str) -> Result<Vec<u8>, String>;
    fn exists(&self, key: &str) -> Result<bool, String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

#[derive(Resource)]
pub struct Storage(pub Box<dyn StorageBackend>);

pub struct LocalFileBackend;

impl StorageBackend for LocalFileBackend {
    fn save(&self, data: &[u8], key: &str) -> Result<(), String> {
        std::fs::write(key, data).map_err(|e| e.to_string())
    }

    fn load(&self, key: &str) -> Result<Vec<u8>, String> {
        std::fs::read(key).map_err(|e| e.to_string())
    }

    fn exists(&self, key: &str) -> Result<bool, String> {
        Ok(std::path::Path::new(key).exists())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        std::fs::remove_file(key).map_err(|e| e.to_string())
    }
}
