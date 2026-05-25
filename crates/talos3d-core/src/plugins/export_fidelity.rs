//! Export fidelity manifests for PP-186.
//!
//! These manifests describe what a target export surface preserves, degrades,
//! or omits. They are intentionally declarative: exporters can report the
//! contract without depending on foreign runtimes or changing file writers.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExportFidelityManifest {
    pub surface_id: String,
    pub label: String,
    pub artifact_kind: ExportArtifactKind,
    pub categories: Vec<ExportFidelityCategory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ExportFidelityWarning>,
    pub foreign_runtime_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportArtifactKind {
    VisualDocument,
    DraftingArtifact,
    NativeReusableContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExportFidelityCategory {
    pub category: ExportFidelityCategoryKind,
    pub disposition: ExportFidelityDisposition,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportFidelityCategoryKind {
    SemanticIdentity,
    Geometry,
    MaterialIntent,
    Quantities,
    Provenance,
    RefinementState,
    Editability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportFidelityDisposition {
    Preserved,
    Degraded,
    Omitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "model-api", derive(schemars::JsonSchema))]
pub struct ExportFidelityWarning {
    pub code: String,
    pub message: String,
}

pub fn all_export_fidelity_manifests() -> Vec<ExportFidelityManifest> {
    vec![
        drawing_manifest(
            "drawing.png",
            "Drawing PNG",
            ExportArtifactKind::VisualDocument,
        ),
        drawing_manifest(
            "drawing.pdf",
            "Drawing PDF",
            ExportArtifactKind::VisualDocument,
        ),
        drawing_manifest(
            "drawing.svg",
            "Drawing SVG",
            ExportArtifactKind::VisualDocument,
        ),
        drawing_manifest(
            "drawing.dxf",
            "Drawing DXF",
            ExportArtifactKind::DraftingArtifact,
        ),
        definition_library_json_manifest(),
    ]
}

pub fn export_fidelity_manifest_for_surface(surface: &str) -> Option<ExportFidelityManifest> {
    let normalized = normalize_surface(surface)?;
    all_export_fidelity_manifests()
        .into_iter()
        .find(|manifest| manifest.surface_id == normalized)
}

pub fn export_fidelity_manifest_for_path(path: impl AsRef<Path>) -> Option<ExportFidelityManifest> {
    let path = path.as_ref();
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => export_fidelity_manifest_for_surface("drawing.png"),
        "pdf" => export_fidelity_manifest_for_surface("drawing.pdf"),
        "svg" | "svd" => export_fidelity_manifest_for_surface("drawing.svg"),
        "dxf" => export_fidelity_manifest_for_surface("drawing.dxf"),
        "json" => export_fidelity_manifest_for_surface("definition_library.json"),
        _ => None,
    }
}

fn normalize_surface(surface: &str) -> Option<&'static str> {
    match surface.trim().to_ascii_lowercase().as_str() {
        "drawing.png" | "png" | "export_drawing.png" => Some("drawing.png"),
        "drawing.pdf" | "pdf" | "export_drawing.pdf" => Some("drawing.pdf"),
        "drawing.svg" | "svg" | "svd" | "export_drawing.svg" => Some("drawing.svg"),
        "drawing.dxf" | "dxf" | "export_drawing.dxf" | "drafting_sheet.dxf" => Some("drawing.dxf"),
        "definition_library.json"
        | "definition.library.json"
        | "definition_library_json"
        | "library.json"
        | "json" => Some("definition_library.json"),
        _ => None,
    }
}

fn drawing_manifest(
    surface_id: &str,
    label: &str,
    artifact_kind: ExportArtifactKind,
) -> ExportFidelityManifest {
    let geometry_note = match artifact_kind {
        ExportArtifactKind::DraftingArtifact => {
            "Preserves 2D drafting geometry in export units; not a native model graph.".to_string()
        }
        _ => "Preserves visual projection/rasterization, not authoritative editable geometry."
            .to_string(),
    };
    ExportFidelityManifest {
        surface_id: surface_id.to_string(),
        label: label.to_string(),
        artifact_kind,
        categories: vec![
            category(
                ExportFidelityCategoryKind::SemanticIdentity,
                ExportFidelityDisposition::Omitted,
                "Element ids and semantic component graph are not represented.",
            ),
            category(
                ExportFidelityCategoryKind::Geometry,
                ExportFidelityDisposition::Degraded,
                geometry_note,
            ),
            category(
                ExportFidelityCategoryKind::MaterialIntent,
                ExportFidelityDisposition::Degraded,
                "Rendered appearance or drafting styling may survive; material definitions do not.",
            ),
            category(
                ExportFidelityCategoryKind::Quantities,
                ExportFidelityDisposition::Omitted,
                "Quantity sets and measurement provenance are not exported as data.",
            ),
            category(
                ExportFidelityCategoryKind::Provenance,
                ExportFidelityDisposition::Omitted,
                "Authoring provenance and grounding records are not exported as data.",
            ),
            category(
                ExportFidelityCategoryKind::RefinementState,
                ExportFidelityDisposition::Omitted,
                "Refinement state and obligations are not represented.",
            ),
            category(
                ExportFidelityCategoryKind::Editability,
                ExportFidelityDisposition::Degraded,
                "Useful for viewing, documentation, or drafting; not a semantic edit store.",
            ),
        ],
        warnings: vec![ExportFidelityWarning {
            code: "not_native_truth".to_string(),
            message: "Use native .talos3d project files or definition library JSON for reusable semantic content movement.".to_string(),
        }],
        foreign_runtime_required: false,
    }
}

fn definition_library_json_manifest() -> ExportFidelityManifest {
    ExportFidelityManifest {
        surface_id: "definition_library.json".to_string(),
        label: "Definition Library JSON".to_string(),
        artifact_kind: ExportArtifactKind::NativeReusableContent,
        categories: vec![
            category(
                ExportFidelityCategoryKind::SemanticIdentity,
                ExportFidelityDisposition::Preserved,
                "Definition and library ids are preserved for reusable content.",
            ),
            category(
                ExportFidelityCategoryKind::Geometry,
                ExportFidelityDisposition::Preserved,
                "Definition representations, evaluators, and compound slots are preserved.",
            ),
            category(
                ExportFidelityCategoryKind::MaterialIntent,
                ExportFidelityDisposition::Preserved,
                "Definition material assignments and material-facing parameters are preserved.",
            ),
            category(
                ExportFidelityCategoryKind::Quantities,
                ExportFidelityDisposition::Degraded,
                "Reusable definition parameters persist; project instance quantities are outside the library file.",
            ),
            category(
                ExportFidelityCategoryKind::Provenance,
                ExportFidelityDisposition::Degraded,
                "Definition domain data and embedded metadata persist; project/session provenance is outside the library file.",
            ),
            category(
                ExportFidelityCategoryKind::RefinementState,
                ExportFidelityDisposition::Degraded,
                "Reusable definition contracts persist where modeled; project entity refinement state is outside the library file.",
            ),
            category(
                ExportFidelityCategoryKind::Editability,
                ExportFidelityDisposition::Preserved,
                "Native reusable-content movement; imported libraries remain editable through library workflows.",
            ),
        ],
        warnings: Vec::new(),
        foreign_runtime_required: false,
    }
}

fn category(
    category: ExportFidelityCategoryKind,
    disposition: ExportFidelityDisposition,
    note: impl Into<String>,
) -> ExportFidelityCategory {
    ExportFidelityCategory {
        category,
        disposition,
        note: note.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_manifests_label_visual_exports_as_non_native_truth() {
        let manifest = export_fidelity_manifest_for_surface("drawing.svg").unwrap();
        assert_eq!(manifest.artifact_kind, ExportArtifactKind::VisualDocument);
        assert!(!manifest.foreign_runtime_required);
        assert!(manifest.categories.iter().any(|category| {
            category.category == ExportFidelityCategoryKind::SemanticIdentity
                && category.disposition == ExportFidelityDisposition::Omitted
        }));
        assert!(manifest.categories.iter().any(|category| {
            category.category == ExportFidelityCategoryKind::Editability
                && category.disposition == ExportFidelityDisposition::Degraded
        }));
    }

    #[test]
    fn definition_library_json_manifest_is_native_reusable_content() {
        let manifest = export_fidelity_manifest_for_surface("definition.library.json").unwrap();
        assert_eq!(
            manifest.artifact_kind,
            ExportArtifactKind::NativeReusableContent
        );
        assert!(manifest.warnings.is_empty());
        assert!(manifest.categories.iter().any(|category| {
            category.category == ExportFidelityCategoryKind::SemanticIdentity
                && category.disposition == ExportFidelityDisposition::Preserved
        }));
    }

    #[test]
    fn path_extension_infers_export_surface_manifest() {
        assert_eq!(
            export_fidelity_manifest_for_path("/tmp/sheet.dxf")
                .unwrap()
                .surface_id,
            "drawing.dxf"
        );
        assert_eq!(
            export_fidelity_manifest_for_path("/tmp/library.json")
                .unwrap()
                .surface_id,
            "definition_library.json"
        );
        assert!(export_fidelity_manifest_for_path("/tmp/model.ifc").is_none());
    }
}
