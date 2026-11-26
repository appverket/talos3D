//! One generic mechanism for portable kinds whose payload is serialisable
//! registry data (descriptors or graph records) rather than a bespoke runtime
//! object.
//!
//! A kind implements [`RegistryKind`] once; [`KindHandler::of`] erases it into a
//! const-table row, so payload schema, candidate check, install, withdrawal and
//! legacy inventory are all reached through [`handler`] with no per-kind code
//! in `knowledge_cloud`. Kinds that live in the shared `CapabilityRegistry`
//! implement the smaller [`CapabilityEntry`] trait and are lifted by the
//! blanket [`InCapabilityRegistry`] adapter.
//!
//! Install semantics: the payload is decoded and validated before anything is
//! touched; a previous revision of the same asset is then withdrawn, and the new
//! one upserted so that a corpus entry replaces a compiled one with the same id.
//! Withdrawal removes the entry and restores nothing compiled.

use std::marker::PhantomData;

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use super::{declarative_prior::GenerationPriorSpec, decode};
use crate::capability_registry::{
    AssemblyPatternDescriptor, AssemblyTypeDescriptor, CapabilityRegistry, ElementClassDescriptor,
    RelationTypeDescriptor,
};
use crate::semantics::{ConceptGraphPack, SemanticGraph};

/// A compiled registry entry offered for migration to the corpus.
pub(super) struct Legacy<P> {
    pub id: String,
    pub label: String,
    /// The exact payload, or why this entry cannot travel as data.
    pub payload: Result<P, &'static str>,
}

/// A portable kind backed by a runtime registry.
pub(super) trait RegistryKind: 'static {
    type Payload: DeserializeOwned + Serialize + JsonSchema + 'static;
    const KIND: &'static str;
    const CONSTRAINTS: &'static [&'static str];
    /// Identity that must equal the knowledge asset id.
    fn payload_id(payload: &Self::Payload) -> &str;
    /// Pure checks run before any registry is touched.
    fn validate(_payload: &Self::Payload) -> Result<(), String> {
        Ok(())
    }
    /// Upsert into the runtime registry (corpus wins over a compiled entry).
    fn install(world: &mut World, payload: Self::Payload);
    /// Remove exactly what `install` contributed for this payload.
    fn withdraw(world: &mut World, payload: &Self::Payload);
    /// Compiled entries not installed from the corpus are filtered by the caller.
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>>;
}

/// Registry entry stored in the shared [`CapabilityRegistry`], one descriptor per asset.
pub(super) trait CapabilityEntry: 'static {
    type Payload: DeserializeOwned + Serialize + JsonSchema + Clone + 'static;
    const KIND: &'static str;
    const CONSTRAINTS: &'static [&'static str];
    fn id(payload: &Self::Payload) -> &str;
    fn validate(_payload: &Self::Payload) -> Result<(), String> {
        Ok(())
    }
    fn upsert(registry: &mut CapabilityRegistry, payload: Self::Payload);
    fn remove(registry: &mut CapabilityRegistry, id: &str);
    fn compiled(registry: &CapabilityRegistry) -> Vec<Legacy<Self::Payload>>;
}

/// Lifts a [`CapabilityEntry`] to a [`RegistryKind`].
pub(super) struct InCapabilityRegistry<E>(PhantomData<E>);

impl<E: CapabilityEntry> RegistryKind for InCapabilityRegistry<E> {
    type Payload = E::Payload;
    const KIND: &'static str = E::KIND;
    const CONSTRAINTS: &'static [&'static str] = E::CONSTRAINTS;
    fn payload_id(payload: &Self::Payload) -> &str {
        E::id(payload)
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        if E::id(payload).trim().is_empty() {
            return Err("Payload identity is empty".into());
        }
        E::validate(payload)
    }
    fn install(world: &mut World, payload: Self::Payload) {
        world.init_resource::<CapabilityRegistry>();
        E::upsert(&mut world.resource_mut::<CapabilityRegistry>(), payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut registry) = world.get_resource_mut::<CapabilityRegistry>() {
            E::remove(&mut registry, E::id(payload));
        }
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        world
            .get_resource::<CapabilityRegistry>()
            .map(E::compiled)
            .unwrap_or_default()
    }
}

/// An inventory row with the payload withheld.
pub(super) struct LegacyItem {
    pub id: String,
    pub label: String,
    /// `None` when the entry is a portable migration candidate.
    pub gap: Option<&'static str>,
}

/// Type-erased row of the kind table.
pub(super) struct KindHandler {
    pub kind: &'static str,
    pub constraints: &'static [&'static str],
    pub payload_schema: fn() -> Value,
    /// Decode + validate `payload` against `asset_id`, withdraw `previous` (the
    /// payload of the revision being replaced), then install. Nothing is
    /// touched when decoding or validation fails.
    pub install: fn(&mut World, &str, &Value, Option<&Value>) -> Result<(), String>,
    pub withdraw: fn(&mut World, &Value),
    pub legacy_list: fn(&World) -> Vec<LegacyItem>,
    pub legacy_get: fn(&World, &str) -> Result<Value, String>,
}

impl KindHandler {
    const fn of<K: RegistryKind>() -> Self {
        Self {
            kind: K::KIND,
            constraints: K::CONSTRAINTS,
            payload_schema: schema::<K>,
            install: install::<K>,
            withdraw: withdraw::<K>,
            legacy_list: legacy_list::<K>,
            legacy_get: legacy_get::<K>,
        }
    }
}

fn schema<K: RegistryKind>() -> Value {
    serde_json::to_value(schemars::schema_for!(K::Payload)).expect("schema serializes")
}

fn install<K: RegistryKind>(
    world: &mut World,
    asset_id: &str,
    payload: &Value,
    previous: Option<&Value>,
) -> Result<(), String> {
    let decoded: K::Payload = decode(payload.clone())?;
    if K::payload_id(&decoded) != asset_id {
        return Err(format!(
            "{} payload identity must match asset identity",
            K::KIND
        ));
    }
    K::validate(&decoded)?;
    if let Some(old) = previous.and_then(|v| serde_json::from_value::<K::Payload>(v.clone()).ok()) {
        K::withdraw(world, &old);
    }
    K::install(world, decoded);
    Ok(())
}

fn withdraw<K: RegistryKind>(world: &mut World, payload: &Value) {
    if let Ok(decoded) = serde_json::from_value::<K::Payload>(payload.clone()) {
        K::withdraw(world, &decoded);
    }
}

fn legacy_list<K: RegistryKind>(world: &World) -> Vec<LegacyItem> {
    K::legacy(world)
        .into_iter()
        .map(|l| LegacyItem {
            id: l.id,
            label: l.label,
            gap: l.payload.err(),
        })
        .collect()
}

fn legacy_get<K: RegistryKind>(world: &World, id: &str) -> Result<Value, String> {
    let entry = K::legacy(world)
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| format!("Missing {} entry", K::KIND))?;
    let payload = entry.payload.map_err(str::to_owned)?;
    serde_json::to_value(payload).map_err(|e| e.to_string())
}

/// Every registry-backed portable kind. Adding a kind is one row here.
pub(super) const HANDLERS: &[KindHandler] = &[
    KindHandler::of::<InCapabilityRegistry<ElementClass>>(),
    KindHandler::of::<InCapabilityRegistry<GenerationPrior>>(),
    KindHandler::of::<InCapabilityRegistry<AssemblyType>>(),
    KindHandler::of::<InCapabilityRegistry<RelationType>>(),
    KindHandler::of::<InCapabilityRegistry<AssemblyPattern>>(),
    KindHandler::of::<ConceptGraph>(),
];

/// The handler for a portable registry kind, if `kind` is one.
pub(super) fn handler(kind: &str) -> Option<&'static KindHandler> {
    HANDLERS.iter().find(|h| h.kind == kind)
}

/// Compiled-registry categories that cannot be delivered as data.
pub(super) const UNSUPPORTED_NATIVE: [&str; 3] = [
    "native_recipe_bodies",
    "native_generation_prior_closures",
    "native_constraints",
];

const CLOSURE_GAP: &str = "Compiled prior holds a native closure that cannot be serialised; author an equivalent declarative generation_prior.v1 decision table from grounded sources";

/// Rows for every compiled entry, from a descriptor accessor.
fn rows<D, P>(
    items: impl IntoIterator<Item = D>,
    id: impl Fn(&D) -> &str,
    label: impl Fn(&D) -> &str,
    payload: impl Fn(D) -> Result<P, &'static str>,
) -> Vec<Legacy<P>> {
    items
        .into_iter()
        .map(|d| Legacy {
            id: id(&d).to_owned(),
            label: label(&d).to_owned(),
            payload: payload(d),
        })
        .collect()
}

macro_rules! capability_entry {
    ($name:ident, $payload:ty, $kind:literal, $constraints:expr, id: |$p:ident| $id:expr,
     upsert: $upsert:ident, remove: $remove:ident, list: $list:ident, label: |$l:ident| $label:expr) => {
        pub(super) struct $name;
        impl CapabilityEntry for $name {
            type Payload = $payload;
            const KIND: &'static str = $kind;
            const CONSTRAINTS: &'static [&'static str] = $constraints;
            fn id($p: &Self::Payload) -> &str {
                $id
            }
            fn upsert(registry: &mut CapabilityRegistry, payload: Self::Payload) {
                registry.$upsert(payload);
            }
            fn remove(registry: &mut CapabilityRegistry, id: &str) {
                registry.$remove(id);
            }
            fn compiled(registry: &CapabilityRegistry) -> Vec<Legacy<Self::Payload>> {
                rows(
                    registry.$list().iter().cloned(),
                    |$p| Self::id($p),
                    |$l| $label,
                    Ok,
                )
            }
        }
    };
}

capability_entry!(
    ElementClass, ElementClassDescriptor, "element_class.v1",
    &[
        "payload.id must equal asset.id",
        "Installing replaces a compiled or earlier class with the same id; withdrawal removes it and restores nothing",
    ],
    id: |p| p.id.0.as_str(),
    upsert: upsert_element_class, remove: remove_element_class,
    list: element_class_descriptors, label: |p| p.label.as_str()
);
capability_entry!(
    AssemblyType, AssemblyTypeDescriptor, "assembly_type.v1",
    &[
        "payload.assembly_type must equal asset.id",
        "Installing replaces a compiled or earlier assembly type with the same id; withdrawal removes it and restores nothing",
    ],
    id: |p| p.assembly_type.as_str(),
    upsert: upsert_assembly_type, remove: remove_assembly_type,
    list: assembly_type_descriptors, label: |p| p.label.as_str()
);
capability_entry!(
    RelationType, RelationTypeDescriptor, "relation_type.v1",
    &[
        "payload.relation_type must equal asset.id",
        "Installing replaces a compiled or earlier relation type with the same id; withdrawal removes it and restores nothing",
    ],
    id: |p| p.relation_type.as_str(),
    upsert: upsert_relation_type, remove: remove_relation_type,
    list: relation_type_descriptors, label: |p| p.label.as_str()
);
capability_entry!(
    AssemblyPattern, AssemblyPatternDescriptor, "assembly_pattern.v1",
    &[
        "payload.pattern_id must equal asset.id",
        "Installing replaces a compiled or earlier pattern with the same id; withdrawal removes it and restores nothing",
        "Curated manifests are separate assets and are not installed by this kind",
    ],
    id: |p| p.pattern_id.as_str(),
    upsert: upsert_assembly_pattern, remove: remove_assembly_pattern,
    list: assembly_pattern_descriptors, label: |p| p.label.as_str()
);

/// Generation priors travel as a declarative decision table.
pub(super) struct GenerationPrior;
impl CapabilityEntry for GenerationPrior {
    type Payload = GenerationPriorSpec;
    const KIND: &'static str = "generation_prior.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.id must equal asset.id",
        "A prior is a first-match-wins decision table over jurisdiction, style_intent, terrain slope and exact context keys; every weight is within [0, 1]",
        "Compiled priors hold native closures and are listed in the legacy inventory as gaps, not exportable payloads",
    ];
    fn id(payload: &Self::Payload) -> &str {
        &payload.id.0
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        payload.validate()
    }
    fn upsert(registry: &mut CapabilityRegistry, payload: Self::Payload) {
        registry.upsert_generation_prior(payload.into_descriptor());
    }
    fn remove(registry: &mut CapabilityRegistry, id: &str) {
        registry.remove_generation_prior(id);
    }
    fn compiled(registry: &CapabilityRegistry) -> Vec<Legacy<Self::Payload>> {
        rows(
            registry.generation_prior_descriptors(None),
            |d| d.id.0.as_str(),
            |d| d.label.as_str(),
            |_| Err(CLOSURE_GAP),
        )
    }
}

/// Design Concept Graph records, delivered as a pack under one identity.
///
/// A pack, not one record per asset: publications, propositions and anchor kinds
/// reference one another and feed compiled indexes, so they are installed,
/// replaced and withdrawn atomically.
pub(super) struct ConceptGraph;
impl RegistryKind for ConceptGraph {
    type Payload = ConceptGraphPack;
    const KIND: &'static str = "concept_graph.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.id must equal asset.id",
        "Between 1 and 2000 records; no record identity repeated within the pack",
        "Installing replaces compiled or earlier records with the same identity; withdrawal removes exactly the records the pack names and restores nothing",
    ];
    fn payload_id(payload: &Self::Payload) -> &str {
        &payload.id
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        payload.validate()
    }
    fn install(world: &mut World, payload: Self::Payload) {
        world.init_resource::<SemanticGraph>();
        world
            .resource_mut::<SemanticGraph>()
            .0
            .upsert_pack(&payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut graph) = world.get_resource_mut::<SemanticGraph>() {
            graph.0.remove_pack(payload);
        }
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        world
            .get_resource::<SemanticGraph>()
            .map(|g| g.0.export_packs())
            .unwrap_or_default()
            .into_iter()
            .map(|p| Legacy {
                id: p.id.clone(),
                label: p.id.clone(),
                payload: Ok(p),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        check_candidate, installed_revision, legacy_get, legacy_list, payload_schema, reconcile,
        AdmittedAsset, Asset, CandidateCheck, Index, LegacyRequest, SCHEMA,
    };
    use super::*;
    use crate::capability_registry::{CapabilityRegistry, ElementClassId, PriorContext, PriorId};
    use crate::semantics::SemanticGraph;
    use serde_json::json;

    /// One synthetic, domain-free fixture per registry kind.
    struct Fixture {
        kind: &'static str,
        id: &'static str,
        /// Compiled form, as a plugin would register it.
        compiled: Value,
        /// Register `compiled` the way a compiled plugin does.
        register: fn(&mut World, Value),
        /// What the runtime registry says about `id` ("compiled" / "corpus").
        observe: fn(&World) -> Option<String>,
        /// Turn the compiled payload into a distinguishable corpus revision.
        edit: fn(&mut Value),
    }

    fn registry(world: &mut World) -> Mut<'_, CapabilityRegistry> {
        world.init_resource::<CapabilityRegistry>();
        world.resource_mut::<CapabilityRegistry>()
    }
    fn from<T: serde::de::DeserializeOwned>(v: Value) -> T {
        serde_json::from_value(v).expect("fixture decodes")
    }
    fn relabel(v: &mut Value) {
        v["label"] = json!("corpus");
    }

    fn fixtures() -> Vec<Fixture> {
        vec![
            Fixture {
                kind: "element_class.v1",
                id: "synthetic.class",
                compiled: json!({"id":"synthetic.class","label":"compiled","description":"d",
                    "semantic_roles":["role"],
                    "class_min_obligations":{"Schematic":[{"id":"o1","role":"role","required_by_state":"Schematic"}]},
                    "class_min_promotion_critical_paths":{"Schematic":["a/b"]},
                    "parameter_schema":{"type":"object"}}),
                register: |w, v| registry(w).register_element_class(from(v)),
                observe: |w| {
                    w.get_resource::<CapabilityRegistry>()?
                        .element_class_descriptor(&ElementClassId("synthetic.class".into()))
                        .map(|d| d.label.clone())
                },
                edit: relabel,
            },
            Fixture {
                kind: "assembly_type.v1",
                id: "synthetic.assembly",
                compiled: json!({"assembly_type":"synthetic.assembly","label":"compiled","description":"d",
                    "expected_member_types":[],"expected_member_roles":["r"],
                    "expected_relation_types":[],"parameter_schema":{"type":"object"}}),
                register: |w, v| registry(w).register_assembly_type(from(v)),
                observe: |w| {
                    w.get_resource::<CapabilityRegistry>()?
                        .assembly_type_descriptor("synthetic.assembly")
                        .map(|d| d.label.clone())
                },
                edit: relabel,
            },
            Fixture {
                kind: "relation_type.v1",
                id: "synthetic.relation",
                compiled: json!({"relation_type":"synthetic.relation","label":"compiled","description":"d",
                    "valid_source_types":[],"valid_target_types":[],"parameter_schema":{"type":"object"},
                    "participates_in_dependency_graph":false}),
                register: |w, v| registry(w).register_relation_type(from(v)),
                observe: |w| {
                    w.get_resource::<CapabilityRegistry>()?
                        .relation_type_descriptors()
                        .iter()
                        .find(|d| d.relation_type == "synthetic.relation")
                        .map(|d| d.label.clone())
                },
                edit: relabel,
            },
            Fixture {
                kind: "assembly_pattern.v1",
                id: "synthetic.pattern",
                compiled: json!({"pattern_id":"synthetic.pattern","label":"compiled","description":"d",
                    "target_types":["t"],"axis":"a","layers":[],"relation_rules":[],"root_layer_ids":[],
                    "requires_support_path":false,"tags":[],"parameter_schema":{"type":"object"}}),
                register: |w, v| registry(w).register_assembly_pattern(from(v)),
                observe: |w| {
                    w.get_resource::<CapabilityRegistry>()?
                        .assembly_pattern_descriptor("synthetic.pattern")
                        .map(|d| d.label.clone())
                },
                edit: relabel,
            },
            Fixture {
                kind: "concept_graph.v1",
                id: "synthetic.concept",
                compiled: json!({"id":"synthetic.concept","concepts":[{"id":"synthetic.concept",
                    "lexical":[{"locale":"en","preferred_label":"compiled","aliases":[],"deprecated_terms":[]}]}]}),
                register: |w, v| {
                    let pack: ConceptGraphPack = from(v);
                    w.init_resource::<SemanticGraph>();
                    w.resource_mut::<SemanticGraph>().0.upsert_pack(&pack);
                },
                observe: |w| {
                    w.get_resource::<SemanticGraph>()?
                        .concept(&"synthetic.concept".into())
                        .map(|c| c.lexical[0].preferred_label.clone())
                },
                edit: |v| v["concepts"][0]["lexical"][0]["preferred_label"] = json!("corpus"),
            },
        ]
    }

    fn asset(kind: &str, id: &str, payload: Value) -> Asset {
        Asset {
            schema: SCHEMA.into(),
            id: id.into(),
            kind: kind.into(),
            title: "Synthetic".into(),
            description: "Registry kind unit fixture".into(),
            element_classes: vec![],
            path_kind: talos3d_knowledge_protocol::portable_path_kind(kind)
                .expect("portable kind")
                .into(),
            required_tool_ids: vec![],
            payload,
            applicability: "Unit tests".into(),
            limitations: vec![],
            evidence: vec!["Synthetic".into()],
            redistribution_rights: "Original synthetic fixture".into(),
            contains_private_context: false,
        }
    }
    fn admitted(asset: Asset, revision: &str) -> AdmittedAsset {
        AdmittedAsset {
            asset,
            revision: revision.into(),
            scope_sha256: None,
        }
    }
    fn withdrawn(world: &mut World, id: &str) {
        let index = Index {
            schema: SCHEMA.into(),
            epoch: 9,
            assets: vec![],
            withdrawn_asset_ids: vec![id.into()],
            evaluation_scope: None,
            released: None,
        };
        reconcile(world, &index);
    }
    fn listed(world: &World, kind: &str, id: &str) -> Option<Value> {
        legacy_list(world)["assets"]
            .as_array()?
            .iter()
            .find(|a| a["kind"] == kind && a["id"] == id)
            .cloned()
    }

    #[test]
    fn every_registry_kind_is_served_checked_installed_overridden_and_withdrawn() {
        for f in fixtures() {
            let mut world = World::new();
            // Schema is served through the generic mechanism.
            let schema = payload_schema(f.kind).unwrap();
            assert_eq!(schema["kind"], f.kind);
            assert!(schema["payload_schema"].is_object(), "{}", f.kind);
            assert!(!schema["constraints"].as_array().unwrap().is_empty());

            // The compiled entry is a portable migration candidate.
            (f.register)(&mut world, f.compiled.clone());
            assert_eq!((f.observe)(&world).as_deref(), Some("compiled"));
            let item = listed(&world, f.kind, f.id)
                .unwrap_or_else(|| panic!("{} not in legacy inventory", f.kind));
            assert_eq!(item["portable"], true, "{}", f.kind);

            // The exact legacy payload passes the candidate check unchanged.
            let captured = legacy_get(
                &world,
                LegacyRequest {
                    kind: f.kind.into(),
                    id: f.id.into(),
                },
            )
            .unwrap_or_else(|e| panic!("{}: {e}", f.kind));
            let checked = check_candidate(CandidateCheck {
                asset: asset(f.kind, f.id, captured["payload"].clone()),
                driver_cases: vec![],
                catalog_queries: vec![],
            })
            .unwrap_or_else(|e| panic!("{}: {e}", f.kind));
            assert_eq!(checked["installed"], false);
            assert!((f.observe)(&world).as_deref() == Some("compiled"));

            // Install a distinguishable revision: corpus wins, provenance recorded.
            let mut payload = captured["payload"].clone();
            (f.edit)(&mut payload);
            install_asset(&mut world, f.kind, f.id, payload.clone(), "rev-1");
            assert_eq!((f.observe)(&world).as_deref(), Some("corpus"), "{}", f.kind);
            assert_eq!(
                installed_revision(&world, f.kind, f.id).map(|r| r.content_hash),
                Some("rev-1".to_string()),
                "{}",
                f.kind
            );
            assert!(listed(&world, f.kind, f.id).is_none(), "{}", f.kind);

            // An invalid replacement preserves the previous revision.
            let mut bad = asset(f.kind, f.id, payload.clone());
            bad.payload["unexpected_field"] = json!(1);
            assert!(super::super::install(&mut world, admitted(bad, "rev-bad")).is_err());
            assert_eq!((f.observe)(&world).as_deref(), Some("corpus"), "{}", f.kind);

            // Withdrawal removes the entry and restores nothing compiled.
            withdrawn(&mut world, f.id);
            assert_eq!((f.observe)(&world), None, "{}", f.kind);
            assert!(installed_revision(&world, f.kind, f.id).is_none());
        }
    }

    fn install_asset(world: &mut World, kind: &str, id: &str, payload: Value, rev: &str) {
        super::super::install(world, admitted(asset(kind, id, payload), rev))
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }

    #[test]
    fn payload_identity_must_equal_asset_identity() {
        for f in fixtures() {
            let mut world = World::new();
            let error = super::super::install(
                &mut world,
                admitted(asset(f.kind, "other.id", f.compiled.clone()), "r"),
            )
            .unwrap_err();
            assert!(error.contains("identity"), "{}: {error}", f.kind);
        }
    }

    #[test]
    fn replacing_a_pack_revision_removes_records_it_no_longer_names() {
        let mut world = World::new();
        let pack = |ids: &[&str]| json!({"id":"synthetic.pack","concepts":ids.iter().map(|i| json!({"id":i})).collect::<Vec<_>>()});
        install_asset(
            &mut world,
            "concept_graph.v1",
            "synthetic.pack",
            pack(&["a", "b"]),
            "r1",
        );
        install_asset(
            &mut world,
            "concept_graph.v1",
            "synthetic.pack",
            pack(&["b", "c"]),
            "r2",
        );
        let graph = world.resource::<SemanticGraph>();
        assert!(graph.concept(&"a".into()).is_none());
        assert!(graph.concept(&"b".into()).is_some() && graph.concept(&"c".into()).is_some());
    }

    #[test]
    fn concept_graph_partition_covers_every_record_and_round_trips() {
        let mut world = World::new();
        let source = json!({"id":"all",
            "concepts":[{"id":"c1"},{"id":"c2"}],
            "anchor_kinds":[{"id":"k1","label":"k","description":"d","geometry":"point","cardinality":"exactly_one"}],
            "publications":[{"publisher":"c1","anchor_kind":"k1","roles":[],"resolver":"r"},
                            {"publisher":"ghost","anchor_kind":"k1","roles":[],"resolver":"r"}],
            "propositions":[{"id":"p1","subject":"c2","predicate":"p","object":{"anchor_kind":"k1"},
                "cardinality":"exactly_one","refusal_reason":"x","repair_hint":"y"}]});
        let pack: ConceptGraphPack =
            serde_json::from_value(source).unwrap_or_else(|e| panic!("{e}"));
        world.init_resource::<SemanticGraph>();
        world.resource_mut::<SemanticGraph>().0.upsert_pack(&pack);
        let packs = world.resource::<SemanticGraph>().0.export_packs();
        assert_eq!(
            packs
                .iter()
                .map(ConceptGraphPack::record_count)
                .sum::<usize>(),
            6
        );
        let mut rebuilt = SemanticGraph::default();
        for p in &packs {
            rebuilt.0.upsert_pack(p);
        }
        assert_eq!(rebuilt.0.concepts().count(), 2);
        assert_eq!(rebuilt.0.publications().len(), 2);
        assert_eq!(rebuilt.0.consumers_of(&"k1".into()), &["c2".into()]);
        for p in &packs {
            rebuilt.0.remove_pack(p);
        }
        assert_eq!(
            rebuilt.0.concepts().count() + rebuilt.0.publications().len(),
            0
        );
    }

    #[test]
    fn declarative_prior_installs_evaluates_and_compiled_priors_are_explicit_gaps() {
        let mut world = World::new();
        let provenance = json!({"source":"synthetic","source_version":"v","jurisdiction":null,
            "ingested_at":0,"license":{"kind":"Cc0"},"backlink":null,"supersedes":[]});
        // A compiled closure prior cannot be exported as data.
        registry(&mut world).register_generation_prior(
            crate::capability_registry::GenerationPriorDescriptor {
                id: PriorId("synthetic.prior".into()),
                label: "compiled".into(),
                description: "d".into(),
                scope: from(
                    json!({"kind":"RecipeSelection","element_class":"c","recipe_family":null}),
                ),
                source_provenance: from(provenance.clone()),
                prior_fn: std::sync::Arc::new(|_| {
                    crate::capability_registry::PriorEvaluation::neutral("compiled")
                }),
            },
        );
        let item = listed(&world, "generation_prior.v1", "synthetic.prior").unwrap();
        assert_eq!(item["portable"], false);
        assert!(item["gap"].as_str().unwrap().contains("closure"));
        assert!(legacy_get(
            &world,
            LegacyRequest {
                kind: "generation_prior.v1".into(),
                id: "synthetic.prior".into()
            }
        )
        .is_err());
        assert!(payload_schema("generation_prior.v1").is_ok());

        let spec = json!({"id":"synthetic.prior","label":"corpus","description":"d",
            "scope":{"kind":"RecipeSelection","element_class":"c","recipe_family":null},
            "source_provenance":provenance,
            "cases":[{"when":{"jurisdiction":"se","terrain_slope_pct_min":10.0},
                      "then":{"weight":0.25,"suggestion":null,"rationale":"steep"}}],
            "otherwise":{"weight":1.0,"suggestion":null,"rationale":"flat"}});
        check_candidate(CandidateCheck {
            asset: asset("generation_prior.v1", "synthetic.prior", spec.clone()),
            driver_cases: vec![],
            catalog_queries: vec![],
        })
        .unwrap();
        install_asset(
            &mut world,
            "generation_prior.v1",
            "synthetic.prior",
            spec.clone(),
            "p1",
        );
        let eval = |world: &World, ctx: Value| {
            let r = world.resource::<CapabilityRegistry>();
            let d = r
                .generation_prior_descriptor(&PriorId("synthetic.prior".into()))
                .unwrap();
            assert_eq!(d.label, "corpus");
            (d.prior_fn)(&PriorContext::from_json(&ctx)).weight
        };
        assert_eq!(
            eval(
                &world,
                json!({"jurisdiction":"SE","terrain_slope_pct":12.0})
            ),
            0.25
        );
        assert_eq!(
            eval(&world, json!({"jurisdiction":"SE","terrain_slope_pct":3.0})),
            1.0
        );
        assert_eq!(eval(&world, json!({"terrain_slope_pct":12.0})), 1.0);

        let mut bad = spec;
        bad["otherwise"]["weight"] = json!(1.5);
        assert!(super::super::install(
            &mut world,
            admitted(asset("generation_prior.v1", "synthetic.prior", bad), "p2")
        )
        .is_err());
        withdrawn(&mut world, "synthetic.prior");
        assert!(world
            .resource::<CapabilityRegistry>()
            .generation_prior_descriptor(&PriorId("synthetic.prior".into()))
            .is_none());
    }

    #[test]
    fn legacy_coverage_lists_registry_kinds_and_keeps_native_gaps() {
        let coverage = legacy_list(&World::new())["coverage"].clone();
        let portable = coverage["portable_kinds"].as_array().unwrap();
        for f in fixtures() {
            assert!(portable.contains(&json!(f.kind)), "{}", f.kind);
        }
        assert!(portable.contains(&json!("generation_prior.v1")));
        assert_eq!(coverage["inspection_only_kinds"], json!([]));
        assert!(coverage["unsupported_dynamic_kinds"]
            .as_array()
            .unwrap()
            .contains(&json!("native_generation_prior_closures")));
    }
}
