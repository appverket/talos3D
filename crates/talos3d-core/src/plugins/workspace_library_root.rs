//! Workspace library root discovery (PP-100 / PP-LIBPUB-1 slice 2).
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
//!
//! Slice 2 is intentionally read-only: directory creation,
//! permission probing, library-file enumeration, and the MCP
//! `definition.library.workspace.create` tool live in subsequent
//! slices.
//!
//! The discovery walk stops at the filesystem root rather than at
//! `$HOME` so workspaces nested inside a user's home directory
//! resolve correctly. A separate "personal user library" root in
//! platform app data (per the agreement) is its own follow-up
//! discovery helper.

use std::path::{Path, PathBuf};

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
}
