//! Disk persistence for user-installed knowledge assets.
//!
//! On startup, loads all `*.json` files from the knowledge directory into the
//! corresponding in-memory registries.  On install (Change-2 / Change-7),
//! writes atomically (tempfile + rename) to the same directory.
//!
//! Default directory: `~/.talos3d/knowledge/`.
//! Override with the `TALOS3D_KNOWLEDGE_DIR` environment variable.
//!
//! Sub-directories:
//! - `recipes/`  — serialized [`RecipeArtifact`] JSON files
//! - `passages/` — serialized [`PersistedPassage`] JSON files

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use bevy::log::warn;

use crate::capability_registry::CorpusProvenance;
use crate::curation::{AssetId, RecipeArtifact, RecipeArtifactRegistry};
use crate::plugins::corpus_gap::CorpusPassageRegistry;

// -----------------------------------------------------------------------
// Home directory — avoid adding dirs_next as a dependency
// -----------------------------------------------------------------------

fn home_dir() -> PathBuf {
    // Prefer HOME (POSIX) then USERPROFILE (Windows). Fall back to `.`.
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

// -----------------------------------------------------------------------
// Knowledge directory resolution
// -----------------------------------------------------------------------

/// Return the root knowledge directory, honouring `TALOS3D_KNOWLEDGE_DIR`.
///
/// Does **not** create the directory — callers that write must ensure it
/// exists themselves.
pub fn knowledge_dir() -> PathBuf {
    if let Ok(val) = std::env::var("TALOS3D_KNOWLEDGE_DIR") {
        PathBuf::from(val)
    } else {
        home_dir().join(".talos3d").join("knowledge")
    }
}

/// Sub-path for recipe artifacts.
pub fn recipes_dir() -> PathBuf {
    knowledge_dir().join("recipes")
}

/// Sub-path for corpus passages.
pub fn passages_dir() -> PathBuf {
    knowledge_dir().join("passages")
}

/// Sub-path for geometric interference rules.
pub fn interference_rules_dir() -> PathBuf {
    knowledge_dir().join("interference_rules")
}

// -----------------------------------------------------------------------
// Atomic write helper
// -----------------------------------------------------------------------

/// Write `bytes` to `path` atomically via a sibling tempfile + rename.
///
/// Creates the parent directory if absent.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
    std::fs::create_dir_all(parent)?;

    // Build a tmp path alongside the target.
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let tmp_name = format!(".{}.tmp", file_name.to_string_lossy());
    let tmp_path = parent.join(&tmp_name);

    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// -----------------------------------------------------------------------
// Recipe persistence (Change-3)
// -----------------------------------------------------------------------

/// Load all `*.json` files from `recipes_dir()` into `registry`.
/// Silently skips files that fail to parse rather than aborting startup.
pub fn load_persisted_recipes(registry: &mut RecipeArtifactRegistry) {
    let dir = recipes_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<RecipeArtifact>(&bytes) {
            Ok(artifact) => {
                registry.insert(artifact);
            }
            Err(e) => {
                warn!(
                    "knowledge_persistence: skipping malformed recipe {:?}: {e}",
                    path
                );
            }
        }
    }
}

/// Persist a single `RecipeArtifact` to `recipes_dir()/<asset_id>.json`.
/// Returns the path written.
pub fn persist_recipe(artifact: &RecipeArtifact) -> Result<PathBuf, String> {
    let filename = sanitize_filename(artifact.meta.id.0.as_str());
    let path = recipes_dir().join(format!("{filename}.json"));
    let bytes = serde_json::to_vec_pretty(artifact)
        .map_err(|e| format!("failed to serialize recipe: {e}"))?;
    atomic_write(&path, &bytes).map_err(|e| format!("failed to write recipe: {e}"))?;
    Ok(path)
}

// -----------------------------------------------------------------------
// Passage persistence (Change-7)
// -----------------------------------------------------------------------

/// Wire format for persisted corpus passages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPassage {
    pub passage_ref: String,
    pub text: String,
    pub citation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    /// License tag string: `"cc0"`, `"public_record"`, `"boverket_public"`,
    /// `"icc_cite_only"`, `"standards_body_citation_only"`.
    /// Defaults to `"public_record"` if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Optional data-driven promotion of this passage into a proactive
    /// must-read guidance card (surfaced up front by the capability snapshot).
    /// Omitted by default; present only on passages that encode generative
    /// authoring "skills" an agent should read before building.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive_guidance: Option<crate::plugins::corpus_gap::ProactivePassageGuidance>,
}

/// Load all `*.json` files from `passages_dir()` into `registry`.
/// Silently skips files that fail to parse.
pub fn load_persisted_passages(registry: &mut CorpusPassageRegistry) {
    let dir = passages_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<PersistedPassage>(&bytes) {
            Ok(p) => {
                let provenance = build_provenance_for_passage(&p);
                registry.register_with_guidance(
                    crate::capability_registry::PassageRef(p.passage_ref),
                    p.text,
                    provenance,
                    p.proactive_guidance,
                );
            }
            Err(e) => {
                warn!(
                    "knowledge_persistence: skipping malformed passage {:?}: {e}",
                    path
                );
            }
        }
    }
}

// -----------------------------------------------------------------------
// Interference-rule persistence (data-driven geometric clash policy)
// -----------------------------------------------------------------------

/// Load all `*.json` files from `interference_rules_dir()` into `policy`.
///
/// Each file may contain either a single [`InterferenceRule`] object or a JSON
/// array of them. Malformed files are skipped with a warning rather than
/// aborting startup, mirroring the recipe/passage loaders.
pub fn load_persisted_interference_rules(
    policy: &mut crate::plugins::interference::InterferencePolicy,
) {
    use crate::plugins::interference::InterferenceRule;

    let dir = interference_rules_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // Accept either a bare rule or an array of rules.
        if let Ok(rule) = serde_json::from_slice::<InterferenceRule>(&bytes) {
            policy.rules.push(rule);
            continue;
        }
        match serde_json::from_slice::<Vec<InterferenceRule>>(&bytes) {
            Ok(rules) => policy.rules.extend(rules),
            Err(e) => {
                warn!(
                    "knowledge_persistence: skipping malformed interference rules {:?}: {e}",
                    path
                );
            }
        }
    }
}

/// Persist a single passage to `passages_dir()/<passage_ref>.json`.
pub fn persist_passage(passage: &PersistedPassage) -> Result<PathBuf, String> {
    let filename = sanitize_filename(&passage.passage_ref);
    let path = passages_dir().join(format!("{filename}.json"));
    let bytes = serde_json::to_vec_pretty(passage)
        .map_err(|e| format!("failed to serialize passage: {e}"))?;
    atomic_write(&path, &bytes).map_err(|e| format!("failed to write passage: {e}"))?;
    Ok(path)
}

// -----------------------------------------------------------------------
// Private helpers
// -----------------------------------------------------------------------

/// Build a `CorpusProvenance` from a `PersistedPassage` for registry insertion.
pub fn build_provenance_for_passage(p: &PersistedPassage) -> CorpusProvenance {
    use crate::capability_registry::LicenseTag;
    let license = match p.license.as_deref().unwrap_or("public_record") {
        "cc0" => LicenseTag::Cc0,
        "boverket_public" => LicenseTag::BoverketPublic,
        "icc_cite_only" => LicenseTag::IccCiteOnly,
        "standards_body_citation_only" => LicenseTag::StandardsBodyCitationOnly,
        _ => LicenseTag::PublicRecord,
    };
    CorpusProvenance {
        source: p.citation.clone(),
        source_version: "acquired".into(),
        jurisdiction: p.jurisdiction.clone(),
        ingested_at: 0,
        license,
        backlink: None,
        supersedes: Vec::new(),
    }
}

/// Replace characters that are unsafe in filenames with `_`.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c => c,
        })
        .collect()
}

// -----------------------------------------------------------------------
// Startup system
// -----------------------------------------------------------------------

/// Bevy system: load persisted recipes and passages into their registries.
///
/// Register this at `Startup` after domain plugins have populated the
/// registries with shipped content so installed assets layer on top.
pub fn load_knowledge_on_startup(world: &mut bevy::prelude::World) {
    {
        let mut registry = world.resource_mut::<RecipeArtifactRegistry>();
        load_persisted_recipes(&mut registry);
    }
    {
        let mut registry = world.resource_mut::<CorpusPassageRegistry>();
        load_persisted_passages(&mut registry);
    }
    if let Some(mut policy) =
        world.get_resource_mut::<crate::plugins::interference::InterferencePolicy>()
    {
        load_persisted_interference_rules(&mut policy);
        bevy::log::info!(
            "knowledge_persistence: interference policy now has {} rule(s) from {:?}",
            policy.rules.len(),
            interference_rules_dir()
        );
    } else {
        bevy::log::warn!(
            "knowledge_persistence: InterferencePolicy resource ABSENT at startup; \
             interference rules NOT loaded"
        );
    }
}

// -----------------------------------------------------------------------
// AssetId helper for installed recipes
// -----------------------------------------------------------------------

/// Build a deterministic [`AssetId`] for an installed recipe given its
/// `family_id`.  Prefixed `installed_recipe/` to avoid collisions with
/// shipped assets.
pub fn installed_recipe_asset_id(family_id: &str) -> AssetId {
    AssetId(format!("installed_recipe/{family_id}"))
}

#[cfg(test)]
mod diag_tests {
    use super::*;
    use crate::plugins::interference::InterferencePolicy;

    /// Diagnostic: load interference rules from a temp dir via the real loader.
    #[test]
    fn loader_parses_rule_array_from_disk() {
        let base = std::env::temp_dir().join(format!(
            "t3d_if_diag_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let rules_dir = base.join("interference_rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let json = r#"[
          {"id":"a","label":"A","subject":{"component_roles":["structural_framing"]},
           "barrier":{"component_roles":["weather_covering"]},
           "relation":"must_not_penetrate","tolerance_m":0.004,"severity":"error",
           "rationale":"r","backlink":"DOC"},
          {"id":"b","subject":{"component_roles":["structural_framing"]},
           "barrier":{"component_roles":["interior_lining"]},
           "relation":"must_not_penetrate","severity":"warning"},
          {"id":"c","subject":{"component_roles":["weather_covering"]},
           "barrier":{"component_roles":["interior_lining"]},
           "relation":"must_not_penetrate","severity":"warning"}
        ]"#;
        std::fs::write(rules_dir.join("r.json"), json).unwrap();

        // Point the loader at our temp dir.
        std::env::set_var("TALOS3D_KNOWLEDGE_DIR", &base);
        let mut policy = InterferencePolicy::default();
        load_persisted_interference_rules(&mut policy);
        std::env::remove_var("TALOS3D_KNOWLEDGE_DIR");

        assert_eq!(policy.rules.len(), 3, "expected 3 rules loaded");
        assert_eq!(policy.rules[0].id, "a");
        assert_eq!(policy.rules[0].tolerance_m, 0.004);
    }

    /// Diagnostic: parse the REAL installed interference rules dir (if present).
    #[test]
    fn real_installed_rules_parse() {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = std::path::PathBuf::from(&home)
            .join(".talos3d/knowledge/interference_rules");
        if !dir.exists() {
            eprintln!("no installed interference_rules dir; skipping");
            return;
        }
        std::env::set_var(
            "TALOS3D_KNOWLEDGE_DIR",
            std::path::PathBuf::from(&home).join(".talos3d/knowledge"),
        );
        let mut policy = InterferencePolicy::default();
        load_persisted_interference_rules(&mut policy);
        std::env::remove_var("TALOS3D_KNOWLEDGE_DIR");
        eprintln!("REAL_RULES_LOADED={}", policy.rules.len());
        for r in &policy.rules {
            eprintln!(
                "  rule id={} subj_roles={:?} barr_roles={:?} sev={:?}",
                r.id, r.subject.component_roles, r.barrier.component_roles, r.severity
            );
        }
        assert!(
            !policy.rules.is_empty(),
            "installed rules dir present but parsed ZERO rules"
        );
    }
}
