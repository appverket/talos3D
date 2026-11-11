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
        atomic_write_file(std::path::Path::new(key), data)
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

/// Commit complete bytes beside the destination before replacing it. A failed
/// write/rename never deletes the previous project, including on recovery saves.
pub(crate) fn atomic_write_file(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .ok_or("Missing file name")?
        .to_string_lossy();
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".{name}.{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    };
    let result = (|| -> std::io::Result<()> {
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_replace_and_failed_replace_preserve_existing_data() {
        let root = std::env::temp_dir().join(format!("talos-atomic-save-{}", uuid::Uuid::new_v4()));
        let path = root.join("model.talos3d");
        atomic_write_file(&path, b"original").unwrap();
        atomic_write_file(&path, b"replacement").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        let directory = root.join("existing-directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("keep"), b"original").unwrap();
        assert!(atomic_write_file(&directory, b"must not replace").is_err());
        assert_eq!(std::fs::read(directory.join("keep")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }
}
