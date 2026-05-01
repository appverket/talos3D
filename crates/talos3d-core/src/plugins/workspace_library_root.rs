//! Workspace library root and storage helpers (PP-100 / PP-LIBPUB-1).
//!
//! Per `LIBRARY_PUBLICATION_BOUNDARY_AGREEMENT.md`, workspace
//! libraries live under
//!
//!     <workspace-root>/.talos3d/libraries/
//!
//! and the agreement is explicit that the application must not
//! "silently invent a workspace root in an unrelated directory".
//! This module provides the read-only pieces needed before MCP
//! tools (slice 3+) and Make Reusable targeting (slice 4+) layer
//! on top:
//!
//! - `workspace_library_subpath()` — the canonical relative path
//!   `.talos3d/libraries` that a workspace root joins onto.
//! - `workspace_library_root_for()` — given a known workspace
//!   root, returns the absolute libraries directory path. Pure
//!   computation, no filesystem touch.
//! - `discover_workspace_root()` — walks up from a starting
//!   directory looking for an existing `.talos3d/` marker. Returns
//!   the workspace root that contains it, or `None` when no marker
//!   is found between the start directory and the filesystem root.
//! - `ensure_workspace_library_root()` — explicitly creates the
//!   libraries directory, but only under an existing `.talos3d/`
//!   marker.
//! - `list_workspace_library_files()` — returns deterministic
//!   `*.json` library files under a libraries directory.
//!
//! Root discovery remains intentionally read-only. Directory creation
//! is separated into `ensure_workspace_library_root()` so MCP/UI
//! callers can surface explicit user intent before any filesystem
//! mutation. Permission probing and the MCP
//! `definition.library.workspace.create` tool live in subsequent
//! slices.
//!
//! The discovery walk stops at the filesystem root rather than at
//! `$HOME` so workspaces nested inside a user's home directory
//! resolve correctly. A separate "personal user library" root in
//! platform app data (per the agreement) is its own follow-up
//! discovery helper.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLibraryRootError {
    MissingWorkspaceMarker { workspace_root: PathBuf },
    WorkspaceMarkerIsNotDirectory { marker_path: PathBuf },
    CreateFailed { path: PathBuf, message: String },
}

impl fmt::Display for WorkspaceLibraryRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingWorkspaceMarker { workspace_root } => {
                write!(
                    f,
                    "workspace root '{}' does not contain an existing .talos3d directory",
                    workspace_root.display()
                )
            }
            Self::WorkspaceMarkerIsNotDirectory { marker_path } => {
                write!(
                    f,
                    "workspace marker '{}' exists but is not a directory",
                    marker_path.display()
                )
            }
            Self::CreateFailed { path, message } => {
                write!(
                    f,
                    "failed to create workspace library root '{}': {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for WorkspaceLibraryRootError {}

/// Canonical relative path from a workspace root to its libraries
/// directory. Keeping this as a single source of truth avoids
/// `.talos3d/libraries` literals leaking into MCP / UI / persistence
/// sites.
pub fn workspace_library_subpath() -> PathBuf {
    PathBuf::from(".talos3d").join("libraries")
}

/// Marker directory that identifies a workspace root: every
/// workspace places its Talos3D state under `.talos3d/`.
fn workspace_marker_subpath() -> &'static Path {
    Path::new(".talos3d")
}

/// Compute the absolute libraries directory for a known workspace
/// root. Pure path join; no filesystem access.
pub fn workspace_library_root_for(workspace_root: &Path) -> PathBuf {
    workspace_root.join(workspace_library_subpath())
}

/// Explicitly create `<workspace-root>/.talos3d/libraries/`.
///
/// This refuses to create the `.talos3d/` marker itself. Callers must
/// either discover an existing workspace root or ask the user to
/// initialize one before calling this helper.
pub fn ensure_workspace_library_root(
    workspace_root: &Path,
) -> Result<PathBuf, WorkspaceLibraryRootError> {
    let marker = workspace_root.join(workspace_marker_subpath());
    if !marker.exists() {
        return Err(WorkspaceLibraryRootError::MissingWorkspaceMarker {
            workspace_root: workspace_root.to_path_buf(),
        });
    }
    if !marker.is_dir() {
        return Err(WorkspaceLibraryRootError::WorkspaceMarkerIsNotDirectory {
            marker_path: marker,
        });
    }

    let library_root = workspace_library_root_for(workspace_root);
    fs::create_dir_all(&library_root).map_err(|error| WorkspaceLibraryRootError::CreateFailed {
        path: library_root.clone(),
        message: error.to_string(),
    })?;
    Ok(library_root)
}

/// Walk upward from `start_dir` (inclusive) looking for a directory
/// that contains a `.talos3d/` marker. Returns the first matching
/// ancestor as the workspace root, or `None` if no marker is found
/// before reaching the filesystem root.
///
/// This function only inspects directories; symlinks resolve under
/// `Path::join` semantics. It does not create any directories. A
/// caller that wants to *create* a workspace root must do so
/// explicitly per the agreement's no-silent-invention rule.
pub fn discover_workspace_root(start_dir: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(start_dir);
    while let Some(dir) = current {
        if dir.join(workspace_marker_subpath()).is_dir() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Return all regular JSON files below a workspace libraries root in
/// deterministic path order. A missing libraries directory is not an
/// error: no workspace libraries have been created yet.
pub fn list_workspace_library_files(library_root: &Path) -> io::Result<Vec<PathBuf>> {
    if !library_root.exists() {
        return Ok(Vec::new());
    }
    if !library_root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "workspace library root '{}' is not a directory",
                library_root.display()
            ),
        ));
    }

    let mut files = Vec::new();
    collect_workspace_library_files(library_root, &mut files)?;
    files.sort();
    Ok(files)
}

/// Discover the workspace root from `start_dir` and enumerate its
/// workspace library JSON files. Returns `Ok(None)` when no workspace
/// root marker exists.
pub fn discover_workspace_library_files(start_dir: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let Some(workspace_root) = discover_workspace_root(start_dir) else {
        return Ok(None);
    };
    let library_root = workspace_library_root_for(&workspace_root);
    list_workspace_library_files(&library_root).map(Some)
}

fn collect_workspace_library_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, io::Error>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_workspace_library_files(&path, files)?;
        } else if file_type.is_file() && is_json_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_json_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn workspace_library_subpath_is_dot_talos3d_slash_libraries() {
        let sub = workspace_library_subpath();
        assert_eq!(sub, Path::new(".talos3d").join("libraries"));
    }

    #[test]
    fn workspace_library_root_for_joins_the_canonical_subpath() {
        let root = Path::new("/tmp/example/workspace");
        let lib_root = workspace_library_root_for(root);
        assert_eq!(lib_root, root.join(".talos3d").join("libraries"));
    }

    #[test]
    fn ensure_workspace_library_root_creates_libraries_under_existing_marker() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        fs::create_dir_all(tmp.path().join(".talos3d")).expect("marker dir creatable");

        let library_root =
            ensure_workspace_library_root(tmp.path()).expect("library root should be created");

        assert_eq!(library_root, tmp.path().join(".talos3d").join("libraries"));
        assert!(library_root.is_dir());
    }

    #[test]
    fn ensure_workspace_library_root_refuses_to_create_workspace_marker() {
        let tmp = TempDir::new().expect("tempdir should be creatable");

        let error = ensure_workspace_library_root(tmp.path()).expect_err("marker is missing");

        assert_eq!(
            error,
            WorkspaceLibraryRootError::MissingWorkspaceMarker {
                workspace_root: tmp.path().to_path_buf(),
            }
        );
        assert!(!tmp.path().join(".talos3d").exists());
    }

    #[test]
    fn ensure_workspace_library_root_refuses_marker_file() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let marker = tmp.path().join(".talos3d");
        fs::write(&marker, "not a directory").expect("marker file write");

        let error = ensure_workspace_library_root(tmp.path()).expect_err("marker file is invalid");

        assert_eq!(
            error,
            WorkspaceLibraryRootError::WorkspaceMarkerIsNotDirectory {
                marker_path: marker,
            }
        );
    }

    #[test]
    fn discover_returns_none_when_no_marker_exists() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let nested = tmp.path().join("a").join("b");
        fs::create_dir_all(&nested).expect("nested dir should be creatable");
        let found = discover_workspace_root(&nested);
        // Walking up from `nested` past `a`, past `tmp.path()`, then
        // beyond may eventually hit a `.talos3d/` directory in some
        // ancestor (e.g. an actual workspace on the developer's
        // machine). We only assert that no marker was synthesised
        // *inside* the temp tree, by checking the returned path
        // (when Some) is not under `tmp.path()`.
        if let Some(found) = found {
            assert!(
                !found.starts_with(tmp.path()),
                "discover should not fabricate a workspace inside an unrelated temp dir, got {found:?}",
            );
        }
    }

    #[test]
    fn discover_finds_marker_at_start_directory() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        fs::create_dir_all(tmp.path().join(".talos3d")).expect("marker dir creatable");
        let found = discover_workspace_root(tmp.path()).expect("marker should be found");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn discover_finds_marker_at_ancestor_directory() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        fs::create_dir_all(tmp.path().join(".talos3d")).expect("marker dir creatable");
        let nested = tmp.path().join("project").join("src").join("modules");
        fs::create_dir_all(&nested).expect("nested dir creatable");
        let found = discover_workspace_root(&nested).expect("marker should be found");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn discover_picks_innermost_workspace_when_marker_is_nested() {
        // If two ancestors carry `.talos3d/` markers, the innermost
        // one wins — this matches the "deepest enclosing workspace"
        // semantics most users expect.
        let tmp = TempDir::new().expect("tempdir should be creatable");
        fs::create_dir_all(tmp.path().join(".talos3d")).expect("outer marker creatable");
        let inner_root = tmp.path().join("inner");
        fs::create_dir_all(inner_root.join(".talos3d")).expect("inner marker creatable");
        let nested = inner_root.join("project").join("src");
        fs::create_dir_all(&nested).expect("nested dir creatable");
        let found = discover_workspace_root(&nested).expect("inner marker should be found");
        assert_eq!(found, inner_root);
    }

    #[test]
    fn discover_ignores_a_marker_file_that_is_not_a_directory() {
        // The agreement requires the marker to be an actual
        // directory: a stray `.talos3d` file (e.g. a misplaced
        // backup or download) must not be treated as a workspace
        // root.
        let tmp = TempDir::new().expect("tempdir should be creatable");
        fs::write(tmp.path().join(".talos3d"), "not a directory").expect("file write");
        let found = discover_workspace_root(tmp.path());
        if let Some(found) = found {
            assert!(
                !found.starts_with(tmp.path()),
                "stray `.talos3d` file should not be treated as a workspace root, got {found:?}",
            );
        }
    }

    #[test]
    fn list_workspace_library_files_returns_empty_when_library_root_is_missing() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let files = list_workspace_library_files(&tmp.path().join(".talos3d").join("libraries"))
            .expect("missing library root should be a clean empty listing");

        assert!(files.is_empty());
    }

    #[test]
    fn list_workspace_library_files_returns_sorted_json_files_recursively() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let root = tmp.path().join(".talos3d").join("libraries");
        fs::create_dir_all(root.join("nested")).expect("nested libraries dir creatable");
        fs::write(root.join("zeta.JSON"), "{}").expect("zeta write");
        fs::write(root.join("alpha.json"), "{}").expect("alpha write");
        fs::write(root.join("notes.txt"), "not a library").expect("notes write");
        fs::write(root.join("nested").join("beta.json"), "{}").expect("beta write");

        let files = list_workspace_library_files(&root).expect("library files should list");

        assert_eq!(
            files,
            vec![
                root.join("alpha.json"),
                root.join("nested").join("beta.json"),
                root.join("zeta.JSON"),
            ]
        );
    }

    #[test]
    fn list_workspace_library_files_rejects_file_root() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let root = tmp.path().join("libraries");
        fs::write(&root, "not a directory").expect("root file write");

        let error = list_workspace_library_files(&root).expect_err("file root should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn discover_workspace_library_files_lists_files_from_discovered_workspace() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let nested = tmp.path().join("project").join("src");
        let library_root = tmp.path().join(".talos3d").join("libraries");
        fs::create_dir_all(&nested).expect("nested dir creatable");
        fs::create_dir_all(&library_root).expect("library root creatable");
        fs::write(library_root.join("team.json"), "{}").expect("team library write");

        let files = discover_workspace_library_files(&nested)
            .expect("discovery listing should not fail")
            .expect("workspace root should be discovered");

        assert_eq!(files, vec![library_root.join("team.json")]);
    }

    #[test]
    fn discover_workspace_library_files_returns_none_without_workspace_marker() {
        let tmp = TempDir::new().expect("tempdir should be creatable");
        let nested = tmp.path().join("project").join("src");
        fs::create_dir_all(&nested).expect("nested dir creatable");

        let files =
            discover_workspace_library_files(&nested).expect("discovery listing should not fail");

        if let Some(files) = files {
            assert!(
                files.iter().all(|file| !file.starts_with(tmp.path())),
                "listing should not fabricate workspace files inside the temp tree: {files:?}",
            );
        }
    }
}
