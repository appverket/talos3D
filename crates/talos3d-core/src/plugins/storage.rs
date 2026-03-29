use bevy::prelude::*;

pub trait StorageBackend: Send + Sync + 'static {
    fn save(&self, data: &[u8], path: &str) -> Result<(), String>;
    fn load(&self, path: &str) -> Result<Vec<u8>, String>;
    fn exists(&self, path: &str) -> Result<bool, String>;
    fn delete(&self, path: &str) -> Result<(), String>;
}

#[derive(Resource)]
pub struct Storage(pub Box<dyn StorageBackend>);

pub struct LocalFileBackend;

impl StorageBackend for LocalFileBackend {
    fn save(&self, data: &[u8], path: &str) -> Result<(), String> {
        std::fs::write(path, data).map_err(|e| e.to_string())
    }

    fn load(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| e.to_string())
    }

    fn exists(&self, path: &str) -> Result<bool, String> {
        Ok(std::path::Path::new(path).exists())
    }

    fn delete(&self, path: &str) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}
